// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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

#[test]
fn sequence_ceiling_refuses_overflow() {
    let max_seq = Roster {
        roster_seq: u64::MAX,
        ..empty(NOW)
    };
    let err = add(max_seq, "m3", M3_KEY, NOW).unwrap_err();
    assert!(err.contains("u64::MAX"), "{err}");

    let mut known = add(empty(NOW), "m3", M3_KEY, NOW).unwrap();
    known.roster_seq = u64::MAX;
    let err = revoke(known, "m3", NOW).unwrap_err();
    assert!(err.contains("u64::MAX"), "{err}");
}
