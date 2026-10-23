// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure functions for editing [`Roster`] values before master signing.

use aterm_update_core::roster::{MAX_MACHINES, Machine, Roster, SUPPORTED_SCHEMA};

/// The `valid_until` every mint and revocation stamps: keys do not expire.
pub const VALID_UNTIL_FOREVER: &str = "9999-12-31T00:00:00Z";

/// Initial empty roster at sequence 0.
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

/// Add a machine, advancing the sequence and restamping `valid_until`.
/// Refuses duplicate or revoked machine IDs and duplicate public keys.
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

/// Remove a machine from `[[machine]]`, record it in `revoked`, and advance `roster_seq`.
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

fn bump(mut r: Roster) -> Roster {
    r.roster_seq += 1;
    r.valid_until = VALID_UNTIL_FOREVER.to_string();
    r
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(r.machines[0].pubkey, M3_KEY);
        assert!(add(r, "", M11_KEY, NOW).is_err());
    }

    #[test]
    fn reusing_a_key_under_a_second_id_is_refused_because_revocation_names_ids() {
        let r = add(empty(NOW), "m3", M3_KEY, NOW).unwrap();
        let err = add(r.clone(), "m3-2", M3_KEY, NOW).unwrap_err();
        assert!(err.contains("already on the roster as 'm3'"), "{err}");
        assert!(err.contains("cannot be revoked"), "{err}");
        assert_eq!(r.machines.len(), 1);
        assert!(r.validate().is_ok());
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
        assert!(r.machines.iter().any(|m| m.id == "m3"));
        assert!(r.validate().is_ok());

        let r = revoke(r, "m11", NOW).expect("an id already revoked is still known");
        assert_eq!(r.revoked, ["m11"]);
    }

    #[test]
    fn a_revoked_id_can_never_be_re_added() {
        let r = add(empty(NOW), "m11", M11_KEY, NOW).unwrap();
        let r = revoke(r, "m11", NOW).unwrap();
        let err = add(r.clone(), "m11", M3_KEY, NOW).unwrap_err();
        assert!(err.contains("never returns"), "{err}");
        let r = add(r, "m11-2026b", M3_KEY, NOW).unwrap();
        assert!(r.is_revoked("m11"));
        assert!(r.machines.iter().any(|m| m.id == "m11-2026b"));
    }

    #[test]
    fn revoking_an_unknown_id_is_an_error_not_a_silent_no_op() {
        let r = add(empty(NOW), "m3", M3_KEY, NOW).unwrap();
        let err = revoke(r, "m4", NOW).unwrap_err();
        assert!(err.contains("not on the roster"), "{err}");
    }

    #[test]
    fn the_roster_has_a_hard_ceiling() {
        let key = |i: usize| format!("{}{i:02}=", &M3_KEY[..M3_KEY.len() - 3]);
        let mut r = empty(NOW);
        for i in 0..MAX_MACHINES {
            r = add(r, &i.to_string(), &key(i), NOW).unwrap();
        }
        assert_eq!(r.machines.len(), MAX_MACHINES, "the roster really is full");
        let err = add(r, "one-too-many", &key(MAX_MACHINES), NOW).unwrap_err();
        assert!(err.contains("full"), "{err}");
    }

    #[test]
    fn every_edit_stamps_the_forever_window() {
        let stale = Roster {
            valid_until: "2020-01-01T00:00:00Z".into(),
            ..empty(NOW)
        };
        let fresh = add(stale, "m3", M3_KEY, NOW).unwrap();
        assert_eq!(fresh.valid_until, VALID_UNTIL_FOREVER);
        assert_eq!(
            fresh.admit(0, NOW as i64),
            Ok(()),
            "9999-12-31 parses under the client's own gate and sits in the future"
        );
        let years_later = NOW as i64 + 20 * 365 * 86_400;
        assert_eq!(
            fresh.admit(0, years_later),
            Ok(()),
            "keys last forever: the replay window against floor-less clients never lapses"
        );
    }
}
