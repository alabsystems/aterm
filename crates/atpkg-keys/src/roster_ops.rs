// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Roster editing — the three operations the owner actually performs, expressed as PURE
//! functions over a [`Roster`] value so each one is testable without a filesystem, a
//! terminal, or the paper master.
//!
//! The master is needed to SIGN the result, never to compute it. Keeping that split means
//! the interesting logic (which id is added, which is denied, how the sequence advances)
//! is exercised by ordinary tests, and the only code that ever touches a secret is the
//! thin CLI layer that calls these and then signs.

use aterm_update_core::roster::{MAX_MACHINES, Machine, Roster, SUPPORTED_SCHEMA};

/// The `valid_until` every mint and revocation stamps: KEYS LAST FOREVER.
///
/// # The dial existed, and the owner turned it off — decided, not defaulted
///
/// `valid_until` was a 180-day window: the only protection a brand-new install (no
/// `roster_seq` floor yet) had against being served a replayed pre-revocation roster.
/// The owner weighed that and removed it, on the argument that months of a valid stolen
/// key already force a full re-key, so the window's residual protection did not pay for
/// its cost — a mandatory return to the paper twice a year, and a fleet-wide fail-closed
/// outage if the date ever lapsed unattended.
///
/// What remains is the honest shape of that decision: REVOCATION IS THE ONLY DEFENSE
/// against a stolen machine key, and it requires the owner to notice the theft. The
/// roster format keeps its date (fielded verifiers check one), so "forever" is a
/// far-future date the strict parser accepts, not a format change.
pub const VALID_UNTIL_FOREVER: &str = "9999-12-31T00:00:00Z";

/// The roster a brand-new channel starts from: schema 1, sequence 0, nobody listed.
///
/// Sequence 0 and an EMPTY machine list is a deliberate starting point rather than a
/// special case — it authorizes nothing, so a roster that was created and never populated
/// fails closed like everything else here.
#[must_use]
pub fn empty(_now_unix: u64) -> Roster {
    Roster {
        schema: SUPPORTED_SCHEMA,
        roster_seq: 0,
        valid_until: VALID_UNTIL_FOREVER.to_string(),
        machines: Vec::new(),
        revoked: Vec::new(),
    }
}

/// Add a machine, advancing the sequence and refreshing the window.
///
/// # Why an id collision is a hard refusal and not an overwrite
///
/// The roster maps id → key, so re-minting under an existing id silently REPLACES that
/// id's public key. Clients still holding the old roster would honour the old key while
/// clients on the new one honour the new key — two live authorities under one name — and
/// the deny-list, which names ids, could not name one without naming the other. That is
/// invisible when it goes wrong, so the tool refuses rather than leaving the rule to the
/// documentation. Reformatting a machine means a NEW id (`m3-2026b`) plus a revocation of
/// the old one, which is [`revoke`] followed by this.
///
/// A REVOKED id is refused for the same reason with more force: an id never returns from
/// the dead, because a client that still holds the revoking roster would keep denying it
/// while a client on the new roster allowed it.
pub fn add(
    mut r: Roster,
    id: &str,
    pubkey_b64: &str,
    now_unix: u64,
) -> Result<Roster, String> {
    if id.is_empty() {
        return Err(
            "a machine id is required (it is how a release is attributed, and \
                    how a revocation names its target)"
                .to_string(),
        );
    }
    if r.machines.iter().any(|m| m.id == id) {
        let mut s = String::from("machine id '");
        s.push_str(id);
        s.push_str(
            "' is already on the roster. Reusing an id REPLACES its key and leaves two live \
             authorities under one name — mint under a new id (e.g. '",
        );
        s.push_str(id);
        s.push_str("-2'), then revoke the old one.");
        return Err(s);
    }
    if r.is_revoked(id) {
        let mut s = String::from("machine id '");
        s.push_str(id);
        s.push_str("' has been revoked; an id never returns. Mint under a new one.");
        return Err(s);
    }
    // THE OTHER DIRECTION OF THE SAME RULE, and the one that breaks revocation outright.
    // Authority is decided by KEY and denial is expressed by ID, so one key under two ids
    // means `machine-revoke <either>` withdraws nothing: the twin entry survives and the
    // same key keeps signing under the surviving name. The owner would see the revocation
    // succeed and still be publishable by the machine they cut off. `Roster::validate`
    // refuses such a document on the client too; this is where an operator gets told why.
    if let Some(twin) = r.machines.iter().find(|m| m.pubkey == pubkey_b64) {
        let mut s = String::from("that public key is already on the roster as '");
        s.push_str(&twin.id);
        s.push_str(
            "'. One key under two ids cannot be revoked: authority is decided by key and \
             revocation names an id, so revoking either leaves the other signing with the \
             same key. Mint this machine its own key, or revoke '",
        );
        s.push_str(&twin.id);
        s.push_str("' if this IS that machine.");
        return Err(s);
    }
    if r.machines.len() >= MAX_MACHINES {
        return Err(
            "the roster is full. Every machine on it can sign any release for \
                    every user, so the ceiling is deliberate — revoke one before adding \
                    another."
                .to_string(),
        );
    }
    r.machines.push(Machine {
        id: id.to_string(),
        pubkey: pubkey_b64.to_string(),
        added_at: aterm_types::rfc3339::format_rfc3339(now_unix),
        not_after: None,
    });
    Ok(bump(r, now_unix))
}

/// Withdraw a machine: remove it from `[[machine]]` AND name it in `revoked`.
///
/// Both halves are required, and the second is the one that does the work. Merely
/// deleting the entry would leave a client that fetched the OLD roster with no reason to
/// stop trusting the machine; naming it explicitly means any client that reads this roster
/// learns the denial in the same document that grants everyone else's authority.
pub fn revoke(mut r: Roster, id: &str, now_unix: u64) -> Result<Roster, String> {
    if !r.machines.iter().any(|m| m.id == id) && !r.is_revoked(id) {
        let mut s = String::from("machine id '");
        s.push_str(id);
        s.push_str("' is not on the roster — nothing to revoke. Check the id.");
        return Err(s);
    }
    r.machines.retain(|m| m.id != id);
    if !r.is_revoked(id) {
        r.revoked.push(id.to_string());
    }
    Ok(bump(r, now_unix))
}

/// Advance the replay counter and refresh the freshness window — the two things EVERY
/// edit must do.
///
/// They are here, in one private helper, rather than at each call site because forgetting
/// either one is silent and serious: a roster published without a sequence bump can be
/// swapped for its predecessor by anyone who kept a copy, and one published without a
/// refreshed window starts life closer to bricking the channel.
fn bump(mut r: Roster, _now_unix: u64) -> Roster {
    r.roster_seq += 1;
    r.valid_until = VALID_UNTIL_FOREVER.to_string();
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-08-04T00:00:00Z. Obviously a fixture; no real key or date depends on it.
    const NOW: u64 = 1_785_801_600;
    const M3_KEY: &str = "PGwCmwCNJnpwjPqXKzcgKDBtGRoIvxWkuIXpsyC5cJc=";
    const M11_KEY: &str = "Fo0aiPXNC/1JVAOAuMFTB6XtBg5o4bXsPI5rUcC1YZo=";

    /// A machine joins: it is listed, the sequence advances, and the stamp is forever.
    #[test]
    fn adding_a_machine_lists_it_and_advances_the_sequence() {
        let r = empty(NOW);
        assert_eq!(r.roster_seq, 0);
        let r = add(r, "m3", M3_KEY, NOW).unwrap();
        assert_eq!(r.roster_seq, 1);
        assert_eq!(r.machines.len(), 1);
        assert_eq!(r.machines[0].id, "m3");
        assert_eq!(r.machines[0].pubkey, M3_KEY);
        assert_eq!(r.machines[0].added_at, "2026-08-04T00:00:00Z");
        assert_eq!(r.valid_until, VALID_UNTIL_FOREVER);
        // A second machine joins alongside the first — this is the owner's whole point.
        let r = add(r, "m11", M11_KEY, NOW).unwrap();
        assert_eq!(r.roster_seq, 2);
        assert_eq!(r.machines.len(), 2);
        assert!(r.validate().is_ok(), "every edit must leave a valid roster");
    }

    /// REUSING AN ID IS REFUSED. The failure it prevents is invisible when it happens, so
    /// the tool refuses rather than documenting the rule.
    #[test]
    fn reusing_a_machine_id_is_refused_rather_than_overwriting_the_key() {
        let r = add(empty(NOW), "m3", M3_KEY, NOW).unwrap();
        let err = add(r.clone(), "m3", M11_KEY, NOW).unwrap_err();
        assert!(err.contains("already on the roster"), "{err}");
        assert!(err.contains("mint under a new id"), "{err}");
        // The original key is untouched: no partial edit happened.
        assert_eq!(r.machines[0].pubkey, M3_KEY);
        // An empty id is refused too — attribution with no name is not attribution.
        assert!(add(r, "", M3_KEY, NOW).is_err());
    }

    /// REUSING A KEY UNDER A SECOND ID IS REFUSED, and this is the stronger of the two
    /// rules — reusing an id makes revocation ambiguous, reusing a key makes it a no-op.
    ///
    /// `revoke` names an id; `Roster::authorize_appcast` decides by key. So a key listed
    /// twice cannot be withdrawn at all: revoke either name and the twin keeps signing
    /// with the same key. The tool refuses because the failure is silent — the operator
    /// sees the revocation succeed.
    ///
    /// Kills the mutation "check the id collision only": the first assertion below turns
    /// into an `unwrap` of a roster that `Roster::validate` would then refuse anyway, so
    /// the two halves of the rule are pinned on both sides.
    #[test]
    fn reusing_a_key_under_a_second_id_is_refused_because_revocation_names_ids() {
        let r = add(empty(NOW), "m3", M3_KEY, NOW).unwrap();
        let err = add(r.clone(), "m3-2", M3_KEY, NOW).unwrap_err();
        assert!(err.contains("already on the roster as 'm3'"), "{err}");
        assert!(err.contains("cannot be revoked"), "{err}");
        // No partial edit: the roster is exactly as it was.
        assert_eq!(r.machines.len(), 1);
        assert!(r.validate().is_ok());
        // The negative control — a DIFFERENT key under a different id is the ordinary
        // case and must stay easy, since making it easy is the point of the tier.
        assert!(add(r, "m11", M11_KEY, NOW).is_ok());
    }

    /// REVOCATION removes AND denies. The deny is the half that reaches a client which
    /// already saw the machine listed.
    #[test]
    fn revoking_removes_the_machine_and_names_it_in_the_deny_list() {
        let r = add(empty(NOW), "m3", M3_KEY, NOW).unwrap();
        let r = add(r, "m11", M11_KEY, NOW).unwrap();
        let seq_before = r.roster_seq;

        let r = revoke(r, "m11", NOW).unwrap();
        assert!(!r.machines.iter().any(|m| m.id == "m11"), "removed");
        assert!(r.is_revoked("m11"), "and explicitly denied");
        assert_eq!(
            r.roster_seq,
            seq_before + 1,
            "a revocation that did not bump the sequence could be swapped for its \
             predecessor by anyone holding a copy"
        );
        // m3 is untouched: revocation is targeted, not a channel-wide brick.
        assert!(r.machines.iter().any(|m| m.id == "m3"));
        assert!(r.validate().is_ok());
    }

    /// A revoked id NEVER returns, so a re-mint under it cannot resurrect the machine.
    #[test]
    fn a_revoked_id_can_never_be_re_added() {
        let r = add(empty(NOW), "m11", M11_KEY, NOW).unwrap();
        let r = revoke(r, "m11", NOW).unwrap();
        let err = add(r.clone(), "m11", M3_KEY, NOW).unwrap_err();
        assert!(err.contains("never returns"), "{err}");
        // ...and the reformat recipe DOES work: a new id beside the revoked one.
        let r = add(r, "m11-2026b", M3_KEY, NOW).unwrap();
        assert!(r.is_revoked("m11"));
        assert!(r.machines.iter().any(|m| m.id == "m11-2026b"));
    }

    /// Revoking something that was never there is an error, not a silent no-op: a typo'd
    /// id would otherwise produce a freshly signed roster that revokes nobody, and the
    /// owner would believe the stolen machine had been cut off.
    #[test]
    fn revoking_an_unknown_id_is_an_error_not_a_silent_no_op() {
        let r = add(empty(NOW), "m3", M3_KEY, NOW).unwrap();
        let err = revoke(r, "m4", NOW).unwrap_err();
        assert!(err.contains("not on the roster"), "{err}");
    }

    /// The ceiling holds. The roster is the set of machines that can publish to every
    /// user, so it is bounded on purpose.
    #[test]
    fn the_roster_has_a_hard_ceiling() {
        // A DISTINCT key per machine, so this test measures the ceiling and nothing else.
        // Filling it with one repeated key would now trip the key-collision rule at the
        // second `add` and the assertion below would pass for the wrong reason.
        let key = |i: usize| format!("{}{i:02}=", &M3_KEY[..M3_KEY.len() - 3]);
        let mut r = empty(NOW);
        for i in 0..MAX_MACHINES {
            r = add(r, &i.to_string(), &key(i), NOW).unwrap();
        }
        assert_eq!(r.machines.len(), MAX_MACHINES, "the roster really is full");
        let err = add(r, "one-too-many", &key(MAX_MACHINES), NOW).unwrap_err();
        assert!(err.contains("full"), "{err}");
    }

    /// Every edit stamps FOREVER — even a roster that somehow carries an earlier date
    /// leaves an edit with the decided stamp, so no stale window can survive a mint.
    #[test]
    fn every_edit_stamps_the_forever_window() {
        let stale = Roster {
            valid_until: "2020-01-01T00:00:00Z".into(),
            ..empty(NOW)
        };
        let fresh = add(stale, "m3", M3_KEY, NOW).unwrap();
        assert_eq!(fresh.valid_until, VALID_UNTIL_FOREVER);
        // The stamp must PARSE under the same strict parser the client gates with —
        // an unparseable forever would read as LAPSED and brick the tier at mint time.
        assert!(
            fresh.valid_until_unix().expect("forever parses") > NOW as i64,
            "9999-12-31 parses and sits in the future"
        );
    }
}
