// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `[everyone]` key a keyed digest is taken under (`commons::everyone_key`, one-mesh design
//! §11 A7): the lowest id among the causally newest `[everyone]` epochs, when this Mac holds its
//! key, and nothing older in its place.

use super::signed::{founded, three_macs};
use crate::client::newest_everyone;
use crate::membership_log::EpochAudience;
use crate::text::Audience;

#[test]
fn the_founded_epoch_and_its_key_are_every_member_s() {
    let (macs, entrants) = three_macs();
    let (log, epoch, key) = founded(&macs, &entrants);
    for mac in &macs {
        let (held, held_key) = newest_everyone(&log, &mac.identity)
            .unwrap()
            .expect("a founded member holds the [everyone] key");
        assert_eq!(held, epoch);
        assert_eq!(*held_key, *key);
    }
}

#[test]
fn a_member_left_out_of_the_newest_epoch_gets_none_not_an_older_key() {
    let (macs, entrants) = three_macs();
    let (mut log, first, _) = founded(&macs, &entrants);
    let (draft, key) = log
        .draft_epoch(&macs[0].member, 2_000, Audience::Everyone, 2)
        .unwrap();
    let second = macs[0].sign(&mut log, draft);
    for to in &macs[..2] {
        let wrap = log
            .draft_wrap(
                &macs[0].member,
                2_001,
                second,
                &key,
                to.identity.recipient(),
            )
            .unwrap();
        macs[0].sign(&mut log, wrap);
    }
    assert_ne!(first, second);
    for mac in &macs[..2] {
        let (held, held_key) = newest_everyone(&log, &mac.identity).unwrap().unwrap();
        assert_eq!(held, second);
        assert_eq!(*held_key, *key);
    }
    // The negative control: the third Mac still holds the first epoch's key, and that key does
    // not stand in for the newest one.
    assert!(
        log.unwrap_epochs(&macs[2].identity)
            .unwrap()
            .contains_key(&first)
    );
    assert!(newest_everyone(&log, &macs[2].identity).unwrap().is_none());
}

#[test]
fn concurrent_newest_epochs_resolve_to_the_lowest_id() {
    let (macs, entrants) = three_macs();
    let (base, _, _) = founded(&macs, &entrants);
    let mut minted = Vec::new();
    let mut copies = Vec::new();
    for (m, ms) in [(0usize, 2_000u64), (1, 2_000)] {
        let mut copy = base.clone();
        let (draft, key) = copy
            .draft_epoch(&macs[m].member, ms, Audience::Everyone, 2)
            .unwrap();
        let epoch = macs[m].sign(&mut copy, draft);
        for to in &macs {
            let wrap = copy
                .draft_wrap(
                    &macs[m].member,
                    ms + 1,
                    epoch,
                    &key,
                    to.identity.recipient(),
                )
                .unwrap();
            macs[m].sign(&mut copy, wrap);
        }
        minted.push((epoch, key));
        copies.push(copy);
    }
    let mut merged = copies.swap_remove(0);
    merged.merge(&copies[0]).unwrap();
    assert_eq!(
        merged.newest_epochs(&EpochAudience::Everyone).len(),
        2,
        "two concurrent epochs are both causally newest"
    );
    let (lowest, lowest_key) = minted.iter().min_by_key(|(id, _)| *id).unwrap();
    for mac in &macs {
        let (held, held_key) = newest_everyone(&merged, &mac.identity).unwrap().unwrap();
        assert_eq!(held, *lowest);
        assert_eq!(*held_key, **lowest_key);
    }
}

#[test]
fn a_newer_epoch_of_another_audience_is_never_the_everyone_key() {
    let (macs, entrants) = three_macs();
    let (mut log, everyone, _) = founded(&macs, &entrants);
    for (ms, audience) in [
        (2_000u64, EpochAudience::Commons),
        (
            2_100,
            EpochAudience::Hosts(vec!["mac-1".to_owned(), "mac-2".to_owned()]),
        ),
    ] {
        let (draft, key) = log.draft_epoch(&macs[0].member, ms, audience, 1).unwrap();
        let epoch = macs[0].sign(&mut log, draft);
        // `mac-1` and `mac-2` are in both audiences.
        for to in &macs[..2] {
            let wrap = log
                .draft_wrap(
                    &macs[0].member,
                    ms + 1,
                    epoch,
                    &key,
                    to.identity.recipient(),
                )
                .unwrap();
            macs[0].sign(&mut log, wrap);
        }
    }
    for mac in &macs {
        let (held, _) = newest_everyone(&log, &mac.identity).unwrap().unwrap();
        assert_eq!(held, everyone);
    }
}

#[test]
fn the_key_s_debug_names_its_epoch_and_no_key_byte() {
    let (macs, entrants) = three_macs();
    let (log, epoch, key) = founded(&macs, &entrants);
    let (epoch_held, key_held) = newest_everyone(&log, &macs[0].identity).unwrap().unwrap();
    let shown = format!(
        "{:?}",
        crate::commons::everyone_key::EveryoneKey {
            epoch: epoch_held,
            key: key_held,
        }
    );
    assert!(shown.contains(&aterm_codec::hex::encode_lower(&epoch)));
    let key_hex = aterm_codec::hex::encode_lower(&*key);
    assert!(!shown.contains(&key_hex));
    assert!(!shown.contains(&format!("{:?}", &key[..4])));
}
