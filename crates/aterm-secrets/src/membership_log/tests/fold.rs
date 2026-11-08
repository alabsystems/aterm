// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fold's key and the epoch a table writer seals under (one-mesh design §4.2 M6, §11 B11):
//! the network key is HKDF-SHA-256 of the `[everyone]` key, a writer takes the lowest newest
//! epoch it holds that is not due a rotation (P1's gate, `Log::head_due`), and an epoch that is
//! due is the view a writer waits on.

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
        assert!(!view.due);
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
        held.due,
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
        assert!(!view.due);
        assert_eq!(*view.key, *fold_network_key(log.network(), &second, &key));
    }
}

/// M6: among concurrent newest epochs a writer takes the lowest id that is not due, and only
/// when none qualifies the lowest that is (and waits).
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
    assert!(waiting.due);
    // A head that wraps the removed Mac yields to a concurrent one that follows the `Remove` and
    // does not, whatever their ids; the two are minted on separate branches, so both stay
    // newest.
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
    remove(&mut right, &macs2, 2);
    let b = mint(&mut right, &macs2[1], &[&macs2[0], &macs2[1]], 2, 3_100).0;
    left.merge(&right).unwrap();
    assert_eq!(left.newest_epochs(&EpochAudience::Everyone).len(), 2);
    let view = left.fold_epoch_view(&macs2[0].identity).unwrap().unwrap();
    assert_eq!(
        view.basis, b,
        "the head that wraps no removed Mac is the one sealed under"
    );
    assert!(!view.due);
}

/// P1's gate, not the held `Wrap`s, decides the table writer's wait (one-mesh proofs P1b). The
/// second Mac mints an `[everyone]` epoch wrapped to all three; the first holds the epoch and its
/// `Wrap`s to the first two, not yet the one to the third, and removes the third beside it. No
/// held `Wrap` of the epoch goes to a removed Mac, but the `Remove` is not in its causal past, so
/// the third Mac opens it once its `Wrap` arrives: the epoch is due, and the writer waits.
///
/// NEGATIVE CONTROL: `ATERM_TEST_NEGATIVE_CONTROL=table-held-wrap` reads the held `Wrap`s alone,
/// as before P1b, and the assert that the writer waits fails.
#[test]
fn a_newest_epoch_whose_remove_is_outside_its_past_is_due_whatever_wraps_are_held() {
    let (macs, entrants) = three_macs();
    let (base, _, _) = founded(&macs, &entrants);
    let mut minter = base.clone();
    let (epoch, _) = mint(
        &mut minter,
        &macs[1],
        &[&macs[0], &macs[1], &macs[2]],
        2,
        2_000,
    );
    let mut writer = base;
    remove(&mut writer, &macs, 2);
    let third = macs[2].identity.recipient();
    for record in minter.records() {
        let wrap_to_third =
            matches!(record.body(), Body::Wrap { recipient, .. } if *recipient == third);
        if writer.get(record.id()).is_none() && !wrap_to_third {
            writer.insert(record.clone()).unwrap();
        }
    }
    assert_eq!(writer.newest_epochs(&EpochAudience::Everyone), vec![epoch]);
    assert!(writer.recipient_is_removed(&third));
    let view = writer.fold_epoch_view(&macs[0].identity).unwrap().unwrap();
    assert_eq!(view.basis, epoch);
    assert!(
        view.due,
        "the epoch's past lacks the Remove: a table writer waits"
    );
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

/// Under one history lease every epoch's writers are the ones the log names for that epoch
/// alone, `[everyone]` and Commons epochs alike, though the lease derives what they read of the
/// log once for all of them (`eval::HistoricalWriters`).
/// NEGATIVE CONTROL: a lease that keeps the first epoch's writers for every epoch answers the
/// epoch after a removal with a removed Mac's binding.
#[cfg(unix)]
#[test]
fn a_lease_names_each_epochs_writers_as_the_log_does_for_it_alone() {
    use crate::membership_log::history::{HistoryView, RecordState};
    let (macs, entrants) = three_macs();
    let (mut log, _, _) = founded(&macs, &entrants);
    let commons = |log: &mut Log, by: &Mac, to: &[&Mac], generation: u64, ms: u64| {
        let (draft, key) = log
            .draft_epoch(&by.member, ms, EpochAudience::Commons, generation)
            .unwrap();
        let epoch = by.sign(log, draft);
        for mac in to {
            let wrap = log
                .draft_wrap(&by.member, ms + 1, epoch, &key, mac.identity.recipient())
                .unwrap();
            by.sign(log, wrap);
        }
    };
    commons(
        &mut log,
        &macs[0],
        &[&macs[0], &macs[1], &macs[2]],
        1,
        2_000,
    );
    remove(&mut log, &macs, 2);
    mint(&mut log, &macs[0], &[&macs[0], &macs[1]], 2, 3_100);
    commons(&mut log, &macs[0], &[&macs[0], &macs[1]], 2, 3_200);
    let view = HistoryView::new(&log);
    let lease = view.lease_complete().unwrap();
    let mut answered = Vec::new();
    for record in log.records() {
        let RecordState::Complete(held) = lease.record(record.id()).unwrap() else {
            panic!("a held record is complete")
        };
        let alone = log
            .historical_commons_writers(record.id())
            .or_else(|| log.historical_fold_writers(record.id()));
        assert_eq!(lease.historical_commons_writers(&held).unwrap(), alone);
        answered.extend(alone.map(|(_, writers)| writers));
    }
    assert!(
        answered.len() >= 4,
        "two epochs of each audience name writers"
    );
    assert!(
        answered.iter().any(|writers| writers != &answered[0]),
        "the epochs after the removal name other writers"
    );
}

/// A Commons epoch's historical writers read only the wraps and the newer Commons epochs that
/// count: its counting `Wrap`s restrict its writers to their recipients, a removed Mac's `Wrap`
/// of an epoch nothing else wraps restricts none of them, a removed Mac's newer epoch supersedes
/// no Mac let in after it, and an epoch that does not count names no writers.
/// NEGATIVE CONTROLS: taking every `Wrap`'s recipient leaves the unwrapped epoch the removed Mac
/// alone for its writer; taking every Commons epoch as a newer one leaves the Mac let in after
/// the removed Mac's epoch out of the epoch before it; keying the wraps by the `Wrap`'s own id
/// lets the Mac the wrapped epoch leaves out write under it; naming the writers of an epoch that
/// does not count names the removed Mac's epoch's.
#[test]
fn a_historical_epochs_writers_read_only_the_wraps_and_epochs_that_count() {
    use crate::membership_log::Evidence;
    let (macs, entrants) = three_macs();
    let (fourth, entrant) = Mac::new(4);
    let (mut log, _, _) = founded(&macs, &entrants);
    let (draft, key) = log
        .draft_epoch(&macs[0].member, 1_900, EpochAudience::Commons, 1)
        .unwrap();
    let wrapped = macs[0].sign(&mut log, draft);
    for to in &macs[..2] {
        let wrap = log
            .draft_wrap(
                &macs[0].member,
                1_901,
                wrapped,
                &key,
                to.identity.recipient(),
            )
            .unwrap();
        macs[0].sign(&mut log, wrap);
    }
    let (draft, key) = log
        .draft_epoch(&macs[0].member, 2_000, EpochAudience::Commons, 2)
        .unwrap();
    let unwrapped = macs[0].sign(&mut log, draft);
    let wrap = log
        .draft_wrap(
            &macs[2].member,
            2_001,
            unwrapped,
            &key,
            macs[2].identity.recipient(),
        )
        .unwrap();
    macs[2].sign(&mut log, wrap);
    let (draft, _) = log
        .draft_epoch(&macs[2].member, 2_002, EpochAudience::Commons, 3)
        .unwrap();
    let cut = macs[2].sign(&mut log, draft);
    let admit = log.draft(
        &macs[0].member,
        2_003,
        Body::Admit {
            entrant,
            evidence: Evidence::Phrase,
        },
    );
    macs[0].sign(&mut log, admit);
    remove(&mut log, &macs, 2);
    let writes_under = |epoch: &RecordId, mac: &Mac| {
        let (_, writers) = log.historical_commons_writers(epoch).unwrap();
        writers
            .iter()
            .any(|binding| binding.recipient() == mac.identity.recipient())
    };
    assert!(
        writes_under(&wrapped, &macs[0]) && writes_under(&wrapped, &macs[1]),
        "the epoch's recipients write under it"
    );
    assert!(
        !writes_under(&wrapped, &macs[2]),
        "a Mac the epoch's counting wraps leave out writes under it nowhere"
    );
    assert!(
        log.historical_commons_writers(&cut).is_none(),
        "an epoch that does not count names no writers"
    );
    let writes = |mac: &Mac| writes_under(&unwrapped, mac);
    assert!(
        writes(&macs[0]) && writes(&macs[1]),
        "the removed Mac's wrap restricts no one"
    );
    assert!(
        writes(&fourth),
        "the removed Mac's epoch supersedes no one let in after it"
    );
}
