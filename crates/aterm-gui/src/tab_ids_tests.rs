// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_minted_id_is_known_and_a_fleet_id_is_never_minted() {
    let control = aterm_tempfile::tempdir().expect("scratch control dir");
    let _rendezvous = crate::identity_claim::rendezvous_override(control.path());
    let sid = mint();
    let minted = sid.tab().expect("a tab id");
    assert!(known(&minted), "a minted ID is known from then on");

    let fleet = TabId::from_bits(0x2_dead_beef);
    // NEGATIVE CONTROL: an ID nobody named is not known.
    assert!(!known(&fleet));
    note_fleet([fleet]);
    assert!(known(&fleet));
    // A later fleet read replaces the earlier one whole.
    note_fleet([]);
    assert!(!known(&fleet));
}

#[test]
fn a_claimed_or_reserved_id_is_known_and_never_kept_twice() {
    let control = aterm_tempfile::tempdir().expect("scratch control dir");
    let _rendezvous = crate::identity_claim::rendezvous_override(control.path());
    let claimed = TabId::from_bits(0x3_0000_c1a1);
    // NEGATIVE CONTROL: before any claim file names it, nothing knows it.
    assert!(!known(&claimed));
    std::fs::create_dir_all(control.path().join("claims")).expect("claims dir");
    std::fs::write(control.path().join("claims").join(claimed.as_str()), "")
        .expect("a claim file another session left");
    assert!(known(&claimed), "a claim this Mac ever made is known");

    let reserved = TabId::from_bits(0x3_0000_5e5e);
    assert!(!known(&reserved));
    reserve([reserved]);
    assert!(
        known(&reserved),
        "a restore layout's ID is held for its leaf"
    );
    let kept = keep(reserved.as_str()).expect("the leaf keeps its own ID");
    assert_eq!(kept.as_str(), reserved.as_str());
    assert!(known(&reserved), "a kept ID stays known");
    assert_eq!(
        keep(reserved.as_str()),
        None,
        "NEGATIVE CONTROL: an ID a tab of this process holds is kept for no second leaf"
    );
    assert_eq!(
        keep("human"),
        None,
        "a word that is no tab ID is never kept"
    );
}
