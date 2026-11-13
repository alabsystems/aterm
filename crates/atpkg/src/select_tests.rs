// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::sig::testkit;

/// A minimal but complete, valid index naming one program, at `build`, attributed to
/// the test roster's machine.
fn index_body(build: u64) -> String {
    format!(
        "schema = 2\nindex_build = {build}\nvalid_until = \"2030-07-05T12:00:00Z\"\n\
         machine_id = \"{id}\"\nroster_seq = {seq}\n\
         [programs.ay]\nrepo = \"ay\"\n",
        id = testkit::MACHINE_ID,
        seq = testkit::SEQ
    )
}

/// A candidate whose index is genuinely signed by the rostered machine, published
/// beside the genuinely master-signed roster.
fn signed(label: &str, build: u64) -> Candidate {
    let raw = index_body(build).into_bytes();
    let sig = testkit::sign(&testkit::MACHINE_SEED, &raw);
    let (roster_bytes, roster_sig) = testkit::published_roster();
    Candidate {
        label: label.into(),
        index_bytes: raw,
        sig,
        roster_bytes,
        roster_sig,
    }
}

/// A candidate carrying a (high) build but an INVALID index signature.
fn unsigned(label: &str, build: u64) -> Candidate {
    let mut c = signed(label, build);
    c.sig = vec![0u8; 64];
    c
}

fn anchor(roster_floor: u64) -> Anchor {
    Anchor::of(
        vec![testkit::pubkey_b64(&testkit::MASTER_SEED)],
        roster_floor,
    )
}

/// A build floor of `build` under the fixture's own generation, so it binds.
fn floor(build: u64) -> BuildFloor {
    BuildFloor {
        index_build: build,
        roster_seq: testkit::SEQ,
    }
}

/// Nothing recorded — first contact.
fn no_floor() -> BuildFloor {
    BuildFloor::none()
}

#[test]
fn an_unarmed_anchor_selects_nothing() {
    let cands = vec![signed("v60", 60)];
    let out = select_index(
        &Anchor::of(vec![], 0),
        cands.clone(),
        no_floor(),
        None,
        testkit::NOW,
    );
    assert!(
        out.selected.is_none(),
        "an unpinned build must install nothing, not everything"
    );
    assert_eq!(
        out.observed_roster_seq, 0,
        "an unpinned build observes nothing either — it must not ratchet a floor it \
         cannot have verified"
    );
    assert!(
        select_index(&anchor(0), cands, no_floor(), None, testkit::NOW)
            .selected
            .is_some()
    );
}

#[test]
fn picks_highest_signed_skips_unsigned() {
    let cands = vec![
        signed("v50", 50),
        unsigned("v999-attack", 999),
        signed("v60", 60),
        signed("v55", 55),
        signed("v60-again", 60),
    ];
    let out = select_index(&anchor(0), cands, floor(40), None, testkit::NOW);
    let sel = out.selected.expect("a signed index wins");
    assert_eq!((sel.label.as_str(), sel.candidate), ("v60", 2));
    assert_eq!(sel.index.index_build, 60);
    assert_eq!(sel.index.attribution().machine_id, testkit::MACHINE_ID);
    assert_eq!(
        out.observed_roster_seq,
        testkit::SEQ,
        "the pass reports the generation it admitted, for the durable ratchet"
    );
}

#[test]
fn floor_filters_below_high_water() {
    let cands = vec![signed("v30", 30), signed("v45", 45), signed("v42", 42)];
    for at in [44, 45] {
        let sel = select_index(&anchor(0), cands.clone(), floor(at), None, testkit::NOW)
            .selected
            .expect("v45 qualifies");
        assert_eq!(sel.index.index_build, 45, "floor {at}");
    }
    assert!(
        select_index(&anchor(0), cands, floor(100), None, testkit::NOW)
            .selected
            .is_none()
    );
}

#[test]
fn a_canonical_release_label_must_name_the_signed_index_build() {
    let out = select_index(
        &anchor(0),
        vec![
            signed("atpkg-index-61", 60),
            signed("atpkg-index-060", 60),
            signed("atpkg-index-59", 59),
        ],
        no_floor(),
        None,
        testkit::NOW,
    );
    let selected = out.selected.expect("the accurately labelled release wins");
    assert_eq!(selected.label, "atpkg-index-59");
    assert_eq!(selected.index.index_build, 59);
}

#[test]
fn a_different_master_selects_nothing() {
    let other = Anchor::of(vec![testkit::pubkey_b64(&testkit::OUTSIDER_SEED)], 0);
    let out = select_index(
        &other,
        vec![signed("v60", 60)],
        no_floor(),
        None,
        testkit::NOW,
    );
    assert!(out.selected.is_none());
    assert_eq!(
        out.observed_roster_seq, 0,
        "a roster that did not verify was not observed, and must not ratchet anything"
    );
}

#[test]
fn a_candidate_with_no_roster_is_skipped() {
    let mut c = signed("v60", 60);
    c.roster_bytes = vec![];
    c.roster_sig = vec![];
    assert!(
        select_index(&anchor(0), vec![c], no_floor(), None, testkit::NOW)
            .selected
            .is_none()
    );
    let mut broken = signed("v70", 70);
    broken.roster_sig = vec![0u8; 64];
    let sel = select_index(
        &anchor(0),
        vec![broken, signed("v60", 60)],
        no_floor(),
        None,
        testkit::NOW,
    )
    .selected
    .expect("the intact candidate still qualifies");
    assert_eq!(
        sel.index.index_build, 60,
        "the higher build had no authority"
    );
    let mut forged = signed("v70", 70);
    forged.roster_bytes.push(b'\n');
    let sel = select_index(
        &anchor(0),
        vec![signed("v60", 60), forged],
        no_floor(),
        None,
        testkit::NOW,
    )
    .selected
    .expect("the intact candidate still qualifies");
    assert_eq!(sel.index.index_build, 60);
}

#[test]
fn a_rolled_back_roster_generation_selects_nothing() {
    let out = select_index(
        &anchor(testkit::SEQ + 1),
        vec![signed("v60", 60)],
        no_floor(),
        None,
        testkit::NOW,
    );
    assert!(
        out.selected.is_none(),
        "a client that has durably seen a newer generation refuses this one forever"
    );
    assert_eq!(
        out.observed_roster_seq, 0,
        "a generation the ratchet refused was never admitted, so it is not an observation"
    );
    assert!(
        select_index(
            &anchor(testkit::SEQ),
            vec![signed("v60", 60)],
            no_floor(),
            None,
            testkit::NOW
        )
        .selected
        .is_some()
    );
}

#[test]
fn a_stale_roster_selects_nothing() {
    let far_future = 4_200_000_000_i64;
    assert!(
        select_index(
            &anchor(0),
            vec![signed("v60", 60)],
            no_floor(),
            None,
            far_future
        )
        .selected
        .is_none()
    );
}

/// A candidate on a generation older than the newest admitted is discarded however high
/// its signed `index_build`.
#[test]
fn an_older_generation_loses_however_high_its_build() {
    use aterm_update_core::roster::Roster;

    let mut newer: Roster = testkit::roster();
    newer.roster_seq = testkit::SEQ + 1;
    let newer_bytes = newer.to_toml().expect("a valid roster emits").into_bytes();
    let newer_sig = testkit::sign(&testkit::MASTER_SEED, &newer_bytes);

    let body = format!(
        "schema = 2\nindex_build = 5\nvalid_until = \"2030-07-05T12:00:00Z\"\n\
         machine_id = \"{id}\"\nroster_seq = {seq}\n[programs.ay]\nrepo = \"ay\"\n",
        id = testkit::MACHINE_ID,
        seq = testkit::SEQ + 1
    )
    .into_bytes();
    let on_newer = Candidate {
        label: "newest-generation".into(),
        sig: testkit::sign(&testkit::MACHINE_SEED, &body),
        index_bytes: body,
        roster_bytes: newer_bytes,
        roster_sig: newer_sig,
    };
    let on_older = signed("old-generation-big-build", 9_000);

    let out = select_index(
        &anchor(0),
        vec![on_older, on_newer],
        no_floor(),
        None,
        testkit::NOW,
    );
    let sel = out
        .selected
        .expect("the newest generation has a valid index");
    assert_eq!(
        sel.label, "newest-generation",
        "index_build must not outrank the generation that authorizes the signer"
    );
    assert_eq!(sel.index.index_build, 5);
    assert_eq!(out.observed_roster_seq, testkit::SEQ + 1);

    let alone = select_index(
        &anchor(0),
        vec![signed("old-generation-big-build", 9_000)],
        no_floor(),
        None,
        testkit::NOW,
    )
    .selected
    .expect("it is perfectly valid on its own");
    assert_eq!(alone.index.index_build, 9_000);
}

/// A build floor recorded under an older generation does not bind, so the paper master
/// can rescue a store whose floor a machine drove out of reach.
#[test]
fn a_floor_from_an_older_generation_is_rebased_not_obeyed() {
    let poisoned = BuildFloor {
        index_build: u64::MAX,
        roster_seq: testkit::SEQ - 1,
    };
    let sel = select_index(
        &anchor(0),
        vec![signed("rescue", 41)],
        poisoned,
        None,
        testkit::NOW,
    )
    .selected
    .expect("a newer generation re-bases the floor");
    assert_eq!(sel.index.index_build, 41);

    let same_generation = BuildFloor {
        index_build: u64::MAX,
        roster_seq: testkit::SEQ,
    };
    assert!(
        select_index(
            &anchor(0),
            vec![signed("rescue", 41)],
            same_generation,
            None,
            testkit::NOW
        )
        .selected
        .is_none(),
        "within one generation the ratchet is exactly as monotonic as it was"
    );
}
