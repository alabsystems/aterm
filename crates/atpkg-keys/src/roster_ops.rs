// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Roster edits as pure functions over a [`Roster`] value, testable without a filesystem, a
//! terminal or the paper master. The master signs the result and never computes it, so only
//! the thin layer that calls these and then signs touches a secret.

use aterm_update_core::roster::{MAX_MACHINES, Machine, Roster, SUPPORTED_SCHEMA};

/// The `valid_until` every mint and revocation stamps: keys last forever.
///
/// There is deliberately no expiry window. Its only protection, a floor-less fresh install
/// refusing a replayed pre-revocation roster, does not pay for a twice-yearly return to the
/// paper and a fleet-wide fail-closed outage if the date lapses; months of a stolen key force a
/// full re-key anyway. Revocation is the only defense against a stolen machine key, and it
/// needs the owner to notice the theft. The format keeps its date (fielded verifiers check
/// one), so "forever" is a far-future date the strict parser accepts.
pub const VALID_UNTIL_FOREVER: &str = "9999-12-31T00:00:00Z";

/// The roster a new channel starts from: schema 1, sequence 0, nobody listed, so it
/// authorizes nothing.
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

/// Add a machine, advancing the sequence and restamping `valid_until` (see [`bump`]).
///
/// An existing id is refused, not overwritten: clients on the old roster would honour the
/// old key and clients on the new one the new key, two live authorities under one name that
/// the id-based deny-list cannot separate. Reformatting a machine means a new id plus a
/// [`revoke`] of the old one. A revoked id is refused too: a client still holding the
/// revoking roster would keep denying it while one on the new roster allowed it.
pub fn add(mut r: Roster, id: &str, pubkey_b64: &str, now_unix: u64) -> Result<Roster, String> {
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
    // The same rule from the other side: authority is decided by key and denial by id, so
    // one key under two ids survives `machine-revoke` of either. `Roster::validate` refuses
    // such a roster on the client too; this is where the operator is told why.
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
    Ok(bump(r))
}

/// Withdraw a machine: remove it from `[[machine]]` and name it in `revoked`, so a client
/// reading this roster learns an explicit denial, not merely an absence.
pub fn revoke(mut r: Roster, id: &str, _now_unix: u64) -> Result<Roster, String> {
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
    Ok(bump(r))
}

/// Advance the replay counter, which every edit must do, and restamp `valid_until` to
/// [`VALID_UNTIL_FOREVER`]. One helper because a forgotten bump is silent and serious: a
/// roster published without one can be swapped for its predecessor by anyone with a copy.
fn bump(mut r: Roster) -> Roster {
    r.roster_seq += 1;
    r.valid_until = VALID_UNTIL_FOREVER.to_string();
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-08-04T00:00:00Z, a fixture.
    const NOW: u64 = 1_785_801_600;
    const M3_KEY: &str = "PGwCmwCNJnpwjPqXKzcgKDBtGRoIvxWkuIXpsyC5cJc=";
    const M11_KEY: &str = "Fo0aiPXNC/1JVAOAuMFTB6XtBg5o4bXsPI5rUcC1YZo=";

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
        // A second machine joins alongside the first.
        let r = add(r, "m11", M11_KEY, NOW).unwrap();
        assert_eq!(r.roster_seq, 2);
        assert_eq!(r.machines.len(), 2);
        assert!(r.validate().is_ok(), "every edit must leave a valid roster");
    }

    #[test]
    fn reusing_a_machine_id_is_refused_rather_than_overwriting_the_key() {
        let r = add(empty(NOW), "m3", M3_KEY, NOW).unwrap();
        let err = add(r.clone(), "m3", M11_KEY, NOW).unwrap_err();
        assert!(err.contains("already on the roster"), "{err}");
        assert!(err.contains("mint under a new id"), "{err}");
        // The original key is untouched: no partial edit happened.
        assert_eq!(r.machines[0].pubkey, M3_KEY);
        // An empty id is refused too — attribution with no name is not attribution —
        // under a key no other rule refuses.
        assert!(add(r, "", M11_KEY, NOW).is_err());
    }

    /// The stronger of the two rules: a reused id makes revocation ambiguous, a reused key
    /// makes it a no-op. With only the id check, the first assertion fails.
    #[test]
    fn reusing_a_key_under_a_second_id_is_refused_because_revocation_names_ids() {
        let r = add(empty(NOW), "m3", M3_KEY, NOW).unwrap();
        let err = add(r.clone(), "m3-2", M3_KEY, NOW).unwrap_err();
        assert!(err.contains("already on the roster as 'm3'"), "{err}");
        assert!(err.contains("cannot be revoked"), "{err}");
        // No partial edit: the roster is exactly as it was.
        assert_eq!(r.machines.len(), 1);
        assert!(r.validate().is_ok());
        // Negative control: a different key under a different id is the ordinary case.
        assert!(add(r, "m11", M11_KEY, NOW).is_ok());
    }

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
        // m3 is untouched: revocation is targeted.
        assert!(r.machines.iter().any(|m| m.id == "m3"));
        assert!(r.validate().is_ok());

        // Revoking it AGAIN (a re-run after an interrupted publish) converges: accepted,
        // and the deny-list still names it once.
        let r = revoke(r, "m11", NOW).expect("an id already revoked is still known");
        assert_eq!(r.revoked, ["m11"]);
    }

    #[test]
    fn a_revoked_id_can_never_be_re_added() {
        let r = add(empty(NOW), "m11", M11_KEY, NOW).unwrap();
        let r = revoke(r, "m11", NOW).unwrap();
        let err = add(r.clone(), "m11", M3_KEY, NOW).unwrap_err();
        assert!(err.contains("never returns"), "{err}");
        // The reformat recipe works: a new id beside the revoked one.
        let r = add(r, "m11-2026b", M3_KEY, NOW).unwrap();
        assert!(r.is_revoked("m11"));
        assert!(r.machines.iter().any(|m| m.id == "m11-2026b"));
    }

    /// A typo'd id would otherwise sign a roster that revokes nobody while the owner believes
    /// the machine cut off.
    #[test]
    fn revoking_an_unknown_id_is_an_error_not_a_silent_no_op() {
        let r = add(empty(NOW), "m3", M3_KEY, NOW).unwrap();
        let err = revoke(r, "m4", NOW).unwrap_err();
        assert!(err.contains("not on the roster"), "{err}");
    }

    #[test]
    fn the_roster_has_a_hard_ceiling() {
        // A distinct key per machine: one repeated key would trip the key-collision rule
        // first and pass for the wrong reason.
        let key = |i: usize| format!("{}{i:02}=", &M3_KEY[..M3_KEY.len() - 3]);
        let mut r = empty(NOW);
        for i in 0..MAX_MACHINES {
            r = add(r, &i.to_string(), &key(i), NOW).unwrap();
        }
        assert_eq!(r.machines.len(), MAX_MACHINES, "the roster really is full");
        let err = add(r, "one-too-many", &key(MAX_MACHINES), NOW).unwrap_err();
        assert!(err.contains("full"), "{err}");
    }

    /// Even a roster carrying an earlier date leaves an edit stamped forever.
    #[test]
    fn every_edit_stamps_the_forever_window() {
        let stale = Roster {
            valid_until: "2020-01-01T00:00:00Z".into(),
            ..empty(NOW)
        };
        let fresh = add(stale, "m3", M3_KEY, NOW).unwrap();
        assert_eq!(fresh.valid_until, VALID_UNTIL_FOREVER);
        // It parses under the client's strict gate: an unparseable forever would read as
        // lapsed.
        assert_eq!(
            fresh.admit(0, NOW as i64),
            Ok(()),
            "9999-12-31 parses under the client's own gate and sits in the future"
        );
        // The decided residual: a client with no floor (a fresh install) admits this
        // generation decades on, so a replayed pre-revocation roster never lapses against it.
        let years_later = NOW as i64 + 20 * 365 * 86_400;
        assert_eq!(
            fresh.admit(0, years_later),
            Ok(()),
            "keys last forever: the replay window against floor-less clients never lapses"
        );
    }
}
