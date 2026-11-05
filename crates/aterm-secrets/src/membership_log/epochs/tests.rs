// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The epoch verdicts give what the scans they replaced gave ([`oracle`]; on 200 signed logs,
//! `entries`'s `two_hundred_signed_logs_read_as_the_replaced_readings_did`); a log keeps its cache
//! across records that change nothing it read and lets it go on the rest; a verdict once taken
//! reads nothing again; and what a 12,288-epoch audience costs is pinned by count, where the
//! pair test it replaced runs past the pin.

use super::super::reach::tests::indexed;
use super::super::reach::{WORK, Work};
use super::super::tests::signed::{Mac, founded, generated, probe, three_macs};
use super::oracle::{self, Exhausted};
use super::{MUTANT, Mutant, Seat, knew};
use crate::SecretBytes;
use crate::membership_log::{Body, Entrant, EpochAudience, Evidence, Kind, Log, Record, RecordId};
use crate::text::{Audience, Entry};
use std::sync::Arc;
use zeroize::Zeroizing;

/// `run`'s result, and the work it counted.
fn counted<T>(run: impl FnOnce() -> T) -> (T, Work) {
    WORK.set(Work::NONE);
    let out = run();
    let work = WORK.get();
    WORK.set(Work::NONE);
    (out, work)
}

/// `run` with this thread's cache mutant `mutant`, counting the oracle's disagreements rather
/// than failing on them; the checks it ran and how many disagreed.
fn disagreeing(mutant: Mutant, run: impl FnOnce()) -> (usize, usize) {
    oracle::CHECKED.set((0, 0));
    oracle::COUNT_ONLY.set(true);
    MUTANT.set(Some(mutant));
    run();
    MUTANT.set(None);
    oracle::COUNT_ONLY.set(false);
    let checked = oracle::CHECKED.get();
    oracle::CHECKED.set((0, 0));
    checked
}

const K: usize = 12_288;

/// The pin: `O(K^2 / 64)` words, three rows per epoch in all.
const PIN: usize = 3 * K * K / 64;

/// A 12,288-epoch audience costs `O(K^2 / 64)` words to find its newest and take a verdict: all
/// of them forked from one root, half with a wrap to the recipient after them (no epoch's past
/// holds one, so every unwrapped one is read, and none leaves the recipient out); half of them
/// forked, wrapped, joined by one merge tree, and the other half forked on it, all but the last
/// wrapped (it leaves the recipient out, the earlier wraps in its past); and a chain, under
/// whose last the recipient's seat entry lies. Each stays within three rows per epoch,
/// 7,077,888 words and steps, while the pair test it replaced runs past that count on each.
#[test]
fn a_12288_epoch_audience_costs_a_few_rows_per_epoch() {
    // All forked from one root; a wrap after each odd one.
    let mut fork: Vec<Vec<usize>> = vec![Vec::new()];
    let fork_epochs: Vec<usize> = (1..=K).collect();
    fork.extend(fork_epochs.iter().map(|_| vec![0]));
    let mut fork_wraps = Vec::new();
    for &e in fork_epochs.iter().filter(|&&e| e % 2 == 1) {
        fork_wraps.push(fork.len());
        fork.push(vec![e]);
    }

    // Half forked and wrapped, one merge tree, half forked on it.
    let mut fan: Vec<Vec<usize>> = vec![Vec::new()];
    let lower: Vec<usize> = (1..=K / 2).collect();
    fan.extend(lower.iter().map(|_| vec![0]));
    let mut level = Vec::new();
    for &e in &lower {
        level.push(fan.len());
        fan.push(vec![e]);
    }
    let fan_wraps = level.clone();
    while level.len() > 1 {
        let mut next = Vec::new();
        for chunk in level.chunks(64) {
            next.push(fan.len());
            fan.push(chunk.to_vec());
        }
        level = next;
    }
    let upper: Vec<usize> = (fan.len()..fan.len() + K / 2).collect();
    fan.extend(upper.iter().map(|_| vec![level[0]]));
    let fan_epochs: Vec<usize> = lower.iter().chain(&upper).copied().collect();
    let last = *upper.last().expect("upper epochs");

    // A chain.
    let chain: Vec<Vec<usize>> = (0..=K)
        .map(|i| i.checked_sub(1).into_iter().collect())
        .collect();
    let chain_epochs: Vec<usize> = (1..=K).collect();

    type Shape<'a> = (
        &'a str,
        &'a [Vec<usize>],
        &'a [usize],
        &'a [usize],
        Box<dyn Fn(usize) -> bool>,
        Option<Seat>,
        bool,
        &'a [usize],
    );
    let shapes: [Shape<'_>; 3] = [
        (
            "forked",
            &fork,
            &fork_epochs,
            &fork_wraps,
            Box::new(|at| at % 2 == 1),
            None,
            false,
            &fork_epochs,
        ),
        (
            "fanned",
            &fan,
            &fan_epochs,
            &fan_wraps,
            Box::new(move |at| at != last),
            None,
            true,
            &upper,
        ),
        (
            "chained",
            &chain,
            &chain_epochs,
            &[],
            Box::new(|_| false),
            Some(Seat {
                at: 0,
                listed: false,
            }),
            true,
            &chain_epochs[K - 1..],
        ),
    ];
    for (name, parents, epochs, prior, wrapped, seat, verdict, want) in shapes {
        assert_eq!(epochs.len(), K, "{name}");
        let reach = indexed(parents);
        let ((newest, left_out), work) = counted(|| {
            let newest = reach.maximal(epochs);
            let left_out = knew(&reach, &newest, &*wrapped, prior, seat, true);
            (newest, left_out)
        });
        eprintln!("epoch audience {name}: {work:?}, pin {PIN}");
        assert_eq!(newest, want, "{name}: the newest");
        assert_eq!(left_out, verdict, "{name}: the verdict");
        assert!(work.total() <= PIN, "{name}: {work:?} past {PIN}");
        assert_eq!(
            oracle::pairwise_newest(epochs, &|a, b| reach.before(a, b), PIN),
            Err(Exhausted),
            "{name}: the replaced pair test stays within the pin"
        );
    }
}

/// A Mac that came back is not left out by the epochs before its return. The third Mac, let in
/// by a listed `Admit` and wrapped the first epoch, is removed, a reseal leaves it out, and a
/// listed `Admit` lets it back in after the reseal: the reseal's minter knew of its earlier wrap,
/// yet the Mac's entry follows the reseal. The second Mac, removed, is let back in by its phrase
/// while, concurrently, a reseal that leaves it out follows its earlier wrap: that wrap lies
/// before the Mac's return, so the reseal's minter knew only the seat the Mac left.
/// NEGATIVE CONTROLS: letting a newest epoch the entry follows learn from a prior wrap, or
/// counting a prior wrap the returned Mac's entry follows, reads each as left out.
#[test]
fn a_mac_that_came_back_is_not_left_out_by_the_epochs_before_its_return() {
    let (macs, entrants) = three_macs();
    let (base, _, _) = founded(&macs, &entrants);
    let [first, second, third] = &macs[..] else {
        unreachable!("three Macs");
    };
    let remove = |log: &mut Log, who: &Mac| {
        let entry = log.membership().get(&who.member).expect("a member").entry;
        let removal = log.draft(
            &first.member,
            2_000,
            Body::Remove {
                member: who.member,
                cutoff: entry,
            },
        );
        first.sign(log, removal);
    };
    let reseal = |log: &mut Log, by: &Mac, generation: u64, to: &[&Mac]| {
        let (draft, key) = log
            .draft_epoch(&by.member, 2_001, Audience::Everyone, generation)
            .unwrap();
        let epoch = by.sign(log, draft);
        for mac in to {
            let wrap = log
                .draft_wrap(&by.member, 2_002, epoch, &key, mac.identity.recipient())
                .unwrap();
            by.sign(log, wrap);
        }
    };
    let back = |log: &mut Log, index: usize, evidence: Evidence| {
        let admit = log.draft(
            &first.member,
            2_003,
            Body::Admit {
                entrant: entrants[index].clone(),
                evidence,
            },
        );
        first.sign(log, admit);
    };

    // The third: removed, left out by a reseal, let back in by a listed Admit after it.
    let mut listed = base.clone();
    remove(&mut listed, third);
    reseal(&mut listed, first, 2, &[first, second]);
    back(&mut listed, 2, Evidence::Listed);
    // The second: removed, let back in by its phrase beside a reseal that leaves it out.
    let mut returned = base;
    remove(&mut returned, second);
    let mut beside = returned.clone();
    back(&mut returned, 1, Evidence::Phrase);
    reseal(&mut beside, third, 2, &[first, third]);
    returned.merge(&beside).unwrap();

    for (log, mac, mutant) in [
        (&listed, third, Mutant::IgnoreEntryAfter),
        (&returned, second, Mutant::IgnoreReturn),
    ] {
        let recipient = mac.identity.recipient();
        assert!(
            log.membership().contains(&mac.member),
            "{mutant:?}: it is back"
        );
        assert!(
            !log.audience_left_out_recipient(&EpochAudience::Everyone, &recipient),
            "{mutant:?}: the epoch before its return leaves it out"
        );
        let (checked, wrong) = disagreeing(mutant, || {
            let mut log = log.clone();
            log.epochs = Arc::default();
            let _ = log.audience_left_out_recipient(&EpochAudience::Everyone, &recipient);
        });
        assert!(checked > 0 && wrong > 0, "{mutant:?} goes unnoticed");
    }
}

/// A log of `macs` seated, with one `[everyone]` epoch wrapped to each; the epoch and its key.
fn seated(macs: &[Mac], entrants: Vec<Entrant>) -> (Log, RecordId, Zeroizing<[u8; 32]>) {
    let mut entrants = entrants.into_iter();
    let first = entrants.next().expect("a founder");
    let mut log = Log::new(Record::found(&[7; 32], vec![first], 1_000).unwrap()).unwrap();
    for entrant in entrants {
        let admit = log.draft(
            &macs[0].member,
            1_001,
            Body::Admit {
                entrant,
                evidence: Evidence::Phrase,
            },
        );
        macs[0].sign(&mut log, admit);
    }
    let (draft, key) = log
        .draft_epoch(&macs[0].member, 1_002, Audience::Everyone, 1)
        .unwrap();
    let epoch = macs[0].sign(&mut log, draft);
    for to in macs {
        let wrap = log
            .draft_wrap(&macs[0].member, 1_003, epoch, &key, to.identity.recipient())
            .unwrap();
        macs[0].sign(&mut log, wrap);
    }
    (log, epoch, key)
}

/// A log keeps its cache, shared with the copy it was cloned from, across a `Put`, a `Delete`
/// and a `Name`, and lets it go on an `Epoch`, a `Wrap` and an `Admit`; a verdict taken before
/// a reseal that leaves a Mac out says so after it.
/// NEGATIVE CONTROLS: kept across the reseal's `Epoch`, across a `Wrap` to the Mac it left out,
/// or across a returning Mac's `Admit`, a verdict is stale and disagrees with the replaced scan.
#[test]
fn a_log_keeps_its_epochs_until_an_epoch_a_wrap_or_a_weighed_record_lands() {
    let (macs, entrants) = three_macs();
    let (mut log, epoch, key) = seated(&macs, entrants);
    let third = macs[2].identity.recipient();
    assert!(!log.audience_left_out_recipient(&EpochAudience::Everyone, &third));
    let shared = Arc::clone(&log.epochs);
    let entry = Entry {
        name: "a".to_owned(),
        value: SecretBytes::from(&b"v"[..]),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    let put = log
        .draft_put(&macs[0].member, 2_000, epoch, &key, &entry)
        .unwrap();
    macs[0].sign(&mut log, put);
    let delete = log
        .draft_delete(&macs[1].member, 2_001, epoch, &key, "a")
        .unwrap();
    macs[1].sign(&mut log, delete);
    let name = log.draft(
        &macs[2].member,
        2_002,
        Body::Name {
            name: "renamed".to_owned(),
        },
    );
    macs[2].sign(&mut log, name);
    assert!(
        Arc::ptr_eq(&shared, &log.epochs),
        "a Put, a Delete and a Name keep it"
    );
    assert!(log.epochs.get().is_some(), "and what it read");

    let before = log.clone();
    let (draft, reseal_key) = log
        .draft_epoch(&macs[0].member, 2_003, Audience::Everyone, 2)
        .unwrap();
    let reseal = macs[0].sign(&mut log, draft);
    assert!(log.epochs.get().is_none(), "an Epoch lets it go");
    assert!(
        log.audience_left_out_recipient(&EpochAudience::Everyone, &third),
        "the reseal wraps to nobody, and the third Mac's entry lies in its past"
    );
    for to in &macs[..2] {
        let wrap = log
            .draft_wrap(
                &macs[0].member,
                2_004,
                reseal,
                &reseal_key,
                to.identity.recipient(),
            )
            .unwrap();
        macs[0].sign(&mut log, wrap);
        assert!(log.epochs.get().is_none(), "a Wrap lets it go");
        let _ = log.audience_left_out_recipient(&EpochAudience::Everyone, &third);
    }
    assert!(log.audience_left_out_recipient(&EpochAudience::Everyone, &third));
    assert!(
        !before.audience_left_out_recipient(&EpochAudience::Everyone, &third),
        "the copy from before the reseal keeps its own"
    );
    let (newcomer, entrant) = Mac::new(4);
    assert!(log.audience_left_out(&EpochAudience::Everyone, &newcomer.member));
    let admit = log.draft(
        &macs[0].member,
        2_005,
        Body::Admit {
            entrant,
            evidence: Evidence::Phrase,
        },
    );
    macs[0].sign(&mut log, admit);
    assert!(log.epochs.get().is_none(), "an Admit lets it go");
    assert!(
        !log.audience_left_out(&EpochAudience::Everyone, &newcomer.member),
        "the newcomer's entry follows the reseal"
    );

    // Stale: the reseal's Epoch, a Wrap to the Mac it left out, a returning Mac's Admit.
    let (macs, entrants) = three_macs();
    let (fresh, _, _) = seated(&macs, entrants);
    let (_, wrong) = disagreeing(Mutant::CarryAcross(&[Kind::Epoch]), || {
        let mut log = fresh.clone();
        assert!(!log.audience_left_out_recipient(&EpochAudience::Everyone, &third));
        let (draft, _) = log
            .draft_epoch(&macs[0].member, 2_003, Audience::Everyone, 2)
            .unwrap();
        macs[0].sign(&mut log, draft);
        let _ = log.audience_left_out_recipient(&EpochAudience::Everyone, &third);
    });
    assert!(wrong > 0, "kept across an Epoch, the verdict goes stale");
    let mut resealed = fresh.clone();
    let (draft, reseal_key) = resealed
        .draft_epoch(&macs[0].member, 2_003, Audience::Everyone, 2)
        .unwrap();
    let reseal = macs[0].sign(&mut resealed, draft);
    let (_, wrong) = disagreeing(Mutant::CarryAcross(&[Kind::Wrap]), || {
        let mut log = resealed.clone();
        log.epochs = Arc::default();
        assert!(log.audience_left_out_recipient(&EpochAudience::Everyone, &third));
        let wrap = log
            .draft_wrap(&macs[0].member, 2_004, reseal, &reseal_key, third)
            .unwrap();
        macs[0].sign(&mut log, wrap);
        let _ = log.audience_left_out_recipient(&EpochAudience::Everyone, &third);
    });
    assert!(wrong > 0, "kept across a Wrap, the verdict goes stale");
    let (_, wrong) = disagreeing(Mutant::CarryAcross(&[Kind::Admit]), || {
        let mut log = resealed.clone();
        let (_, mut entrant) = Mac::new(4);
        entrant.request = crate::commons::binding::Request::create(
            [0x31; 16],
            [0x42; 32],
            [0x24; 16],
            [4; 16],
            &macs[2].identity,
            &[4; 32],
        )
        .expect("the returning recipient under a new actor");
        assert!(log.audience_left_out_recipient(&EpochAudience::Everyone, &third));
        let admit = log.draft(
            &macs[0].member,
            2_005,
            Body::Admit {
                entrant,
                evidence: Evidence::Phrase,
            },
        );
        macs[0].sign(&mut log, admit);
        let _ = log.audience_left_out_recipient(&EpochAudience::Everyone, &third);
    });
    assert!(wrong > 0, "kept across an Admit, the verdict goes stale");
}

/// Queries outside the held history retain no memo keys. Known pairs still reuse their
/// verdicts; without the query-domain checks the foreign inputs grow both maps.
#[test]
fn foreign_query_keys_do_not_grow_epoch_memos() {
    use aterm_types::MutexExt as _;

    let (macs, entrants) = three_macs();
    let (log, _, _) = seated(&macs, entrants);
    let epochs = log.epochs();
    for n in 0_u64..512 {
        let mut key = [0; 32];
        key[..8].copy_from_slice(&n.to_le_bytes());
        let foreign = crate::age::Recipient(key);
        let mut foreign_member = macs[0].member;
        foreign_member[..8].copy_from_slice(&n.to_le_bytes());
        assert!(!epochs.known_recipients.contains(&foreign));
        assert!(!log.membership().contains(&foreign_member));
        assert!(!log.audience_left_out_recipient(&EpochAudience::Everyone, &foreign));
        assert!(log.audience_left_out(&EpochAudience::Everyone, &foreign_member));
        let audience = EpochAudience::Hosts(vec![format!("unknown-{n}")]);
        assert!(!log.audience_left_out_recipient(&audience, &macs[0].identity.recipient()));
        assert!(!log.audience_left_out(&audience, &macs[0].member));
    }
    assert!(epochs.audiences.values().all(|sealed| {
        sealed.recipients.lock_or_recover().is_empty()
            && sealed.members.lock_or_recover().is_empty()
    }));
    assert!(!log.audience_left_out(&EpochAudience::Everyone, &macs[0].member));
    let sealed = &epochs.audiences[&EpochAudience::Everyone];
    assert_eq!(sealed.recipients.lock_or_recover().len(), 1);
    assert_eq!(sealed.members.lock_or_recover().len(), 1);
}

/// A verdict once taken reads nothing again: asking every one of a generated log a second time
/// counts no word, bit or step, where the first asking read the index.
/// NEGATIVE CONTROL: with the verdicts forgotten, the second asking reads the index again.
#[test]
fn a_verdict_once_taken_reads_nothing_again() {
    let (macs, _) = three_macs();
    for mutant in [None, Some(Mutant::Forget)] {
        let mut log = generated(7);
        log.epochs = Arc::default();
        MUTANT.set(mutant);
        let ((), first) = counted(|| probe(&log, &macs));
        let ((), again) = counted(|| probe(&log, &macs));
        MUTANT.set(None);
        assert!(first.total() > 0, "the first asking reads the index");
        match mutant {
            None => assert_eq!(again, Work::NONE, "a verdict once taken is kept"),
            Some(_) => assert!(again.total() > 0, "forgetting goes unnoticed"),
        }
    }
}
