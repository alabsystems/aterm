// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fold's key and the epoch a table writer seals under (one-mesh design §4.2 M6, §11 B11):
//! the network key is HKDF-SHA-256 of the `[everyone]` key, a writer takes the lowest newest
//! epoch it holds that wraps no removed recipient, and an epoch that still wraps a removed Mac is
//! the view a writer waits on.

use super::signed::{Mac, founded, three_macs};
use crate::membership_log::{Body, EpochAudience, Log, RecordId, fold_network_key};
use crate::text::Audience;
use zeroize::Zeroizing;

/// The derivation is RFC 5869 with an empty salt, the info naming the network and the epoch:
/// a known answer computed outside this crate (Python's `hmac`), so a change of label, order or
/// salt is caught.
#[test]
fn the_network_key_is_hkdf_of_the_everyone_key_under_its_label() {
    let key = fold_network_key(&[1; 32], &[2; 32], &[7; 32]);
    assert_eq!(
        aterm_codec::hex::encode_lower(&*key),
        "49063c9f9d9b23c59722e850b9f1ff622919efdd82ce42ec3268308090cba319"
    );
    assert_ne!(*key, *fold_network_key(&[1; 32], &[3; 32], &[7; 32]));
    assert_ne!(*key, *fold_network_key(&[4; 32], &[2; 32], &[7; 32]));
    assert_ne!(*key, [7; 32], "the derived key is never the [everyone] key");
}

/// `macs[0]` removes `target` with a plain `Remove` cut at its seat.
fn remove(log: &mut Log, macs: &[Mac], target: usize) {
    let entry = log.membership().get(&macs[target].member).unwrap().entry;
    let removal = log.draft(
        &macs[0].member,
        3_000,
        Body::Remove {
            member: macs[target].member,
            cutoff: entry,
        },
    );
    macs[0].sign(log, removal);
}

/// `by` mints the `[everyone]` epoch `generation` wrapped to `to`; its id and key.
fn mint(
    log: &mut Log,
    by: &Mac,
    to: &[&Mac],
    generation: u64,
    ms: u64,
) -> (RecordId, Zeroizing<[u8; 32]>) {
    let (draft, key) = log
        .draft_epoch(&by.member, ms, Audience::Everyone, generation)
        .unwrap();
    let epoch = by.sign(log, draft);
    for mac in to {
        let wrap = log
            .draft_wrap(&by.member, ms + 1, epoch, &key, mac.identity.recipient())
            .unwrap();
        by.sign(log, wrap);
    }
    (epoch, key)
}

#[test]
fn a_member_seals_under_its_newest_everyone_epoch_through_the_derived_key() {
    let (macs, entrants) = three_macs();
    let (log, epoch, key) = founded(&macs, &entrants);
    for mac in &macs {
        let view = log.fold_epoch_view(&mac.identity).unwrap().unwrap();
        assert_eq!(view.basis, epoch);
        assert_eq!(view.generation, 1);
        assert!(!view.wraps_removed);
        assert_eq!(*view.key, *fold_network_key(log.network(), &epoch, &key));
        assert_eq!(
            *log.unwrap_fold_epoch(&epoch, &mac.identity)
                .unwrap()
                .unwrap(),
            *view.key
        );
    }
    assert!(log.is_fold_epoch(&epoch));
    assert!(!log.is_fold_epoch(log.network()));
    assert_eq!(log.fold_epoch_ids(), vec![epoch]);
}

/// M4: after a `Remove`, the newest epoch still wraps the removed Mac, so the view says so and a
/// writer waits; once the rotation lands, the view moves to it and seals. The removed Mac opens
/// no fold key at all.
#[test]
fn a_held_remove_makes_the_view_wait_until_the_rotation_arrives() {
    let (macs, entrants) = three_macs();
    let (mut log, first, _) = founded(&macs, &entrants);
    remove(&mut log, &macs, 2);
    let held = log.fold_epoch_view(&macs[1].identity).unwrap().unwrap();
    assert_eq!(held.basis, first);
    assert!(
        held.wraps_removed,
        "the newest epoch still wraps the removed Mac: a writer waits"
    );
    assert!(log.fold_epoch_view(&macs[2].identity).unwrap().is_none());
    assert!(
        log.unwrap_fold_epoch(&first, &macs[2].identity)
            .unwrap()
            .is_none()
    );
    let (second, key) = mint(&mut log, &macs[0], &[&macs[0], &macs[1]], 2, 3_100);
    for mac in &macs[..2] {
        let view = log.fold_epoch_view(&mac.identity).unwrap().unwrap();
        assert_eq!(view.basis, second);
        assert!(!view.wraps_removed);
        assert_eq!(*view.key, *fold_network_key(log.network(), &second, &key));
    }
}

/// M6: among concurrent newest epochs a writer takes the lowest id that wraps no removed
/// recipient, and only when none qualifies the lowest that does (and waits).
#[test]
fn concurrent_heads_resolve_to_the_lowest_qualifying_id() {
    let (macs, entrants) = three_macs();
    let (base, _, _) = founded(&macs, &entrants);
    let mut copies = Vec::new();
    let mut minted = Vec::new();
    for by in [0usize, 1] {
        let mut copy = base.clone();
        let to: Vec<&Mac> = macs.iter().collect();
        minted.push(mint(&mut copy, &macs[by], &to, 2, 2_000).0);
        copies.push(copy);
    }
    let mut merged = copies.swap_remove(0);
    merged.merge(&copies[0]).unwrap();
    assert_eq!(merged.newest_epochs(&EpochAudience::Everyone).len(), 2);
    let lowest = *minted.iter().min().unwrap();
    assert_eq!(
        merged
            .fold_epoch_view(&macs[0].identity)
            .unwrap()
            .unwrap()
            .basis,
        lowest
    );
    // Both heads wrap the third Mac; removing it makes neither qualify, so the view is the
    // lowest one, waiting.
    let mut removed = merged.clone();
    remove(&mut removed, &macs, 2);
    let waiting = removed.fold_epoch_view(&macs[0].identity).unwrap().unwrap();
    assert_eq!(waiting.basis, lowest);
    assert!(waiting.wraps_removed);
    // A head that wraps the removed Mac yields to a concurrent one that does not, whatever
    // their ids; the two are minted on separate branches, so both stay newest.
    let (macs2, entrants2) = three_macs();
    let (base2, _, _) = founded(&macs2, &entrants2);
    let mut left = base2.clone();
    let mut right = base2;
    mint(
        &mut left,
        &macs2[0],
        &[&macs2[0], &macs2[1], &macs2[2]],
        2,
        2_000,
    );
    let b = mint(&mut right, &macs2[1], &[&macs2[0], &macs2[1]], 2, 2_000).0;
    left.merge(&right).unwrap();
    remove(&mut left, &macs2, 2);
    assert_eq!(left.newest_epochs(&EpochAudience::Everyone).len(), 2);
    let view = left.fold_epoch_view(&macs2[0].identity).unwrap().unwrap();
    assert_eq!(
        view.basis, b,
        "the head that wraps no removed Mac is the one sealed under"
    );
    assert!(!view.wraps_removed);
}

/// A table writer's historical grant under an `[everyone]` epoch: the seats it was wrapped to,
/// read from the log, as for a Commons epoch.
#[test]
fn historical_fold_writers_are_the_epoch_s_seats() {
    let (macs, entrants) = three_macs();
    let (mut log, first, _) = founded(&macs, &entrants);
    let (generation, writers) = log.historical_fold_writers(&first).unwrap();
    assert_eq!(generation, 1);
    for mac in &macs {
        assert!(
            writers
                .iter()
                .any(|b| b.recipient() == mac.identity.recipient()),
            "a seat the epoch wraps writes under it"
        );
    }
    remove(&mut log, &macs, 2);
    let (second, _) = mint(&mut log, &macs[0], &[&macs[0], &macs[1]], 2, 3_100);
    let (_, later) = log.historical_fold_writers(&second).unwrap();
    assert!(
        !later
            .iter()
            .any(|b| b.recipient() == macs[2].identity.recipient()),
        "a Mac removed before the epoch never writes under it"
    );
    assert!(log.historical_fold_writers(log.network()).is_none());
}
