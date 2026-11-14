// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_capacity::{Fixed, Ledger, Need};

/// A 12-core machine at a 5-minute load of `load5`.
fn machine(load5: f64) -> Reading {
    Reading {
        load1: Some(load5),
        load5: Some(load5),
        ..Reading::of_gib(48, 40, 12)
    }
}

/// This process asking after every claim `ledger` lists, with no ancestors.
fn asker_after(ledger: &Ledger) -> Asker {
    Asker {
        pid: std::process::id(),
        ancestors: Vec::new(),
        rank: Rank::Ordinary,
        since_ms: ledger
            .list()
            .expect("lists")
            .iter()
            .map(|r| r.start_ms)
            .max()
            .unwrap_or(0)
            + 1,
    }
}

/// THE SHARE OF A RUN WITH NO BINARY RUNNING IS WHAT THE LEDGER GRANTS ONE CLAIM OF ITS ASK.
/// On each machine and ledger below, [`fair`] over [`Others::of`] answers what the ledger
/// itself then grants a new claim of the same cores: the rule is the ledger's, kept beside it
/// by this comparison.
#[test]
fn the_share_of_a_run_with_nothing_running_is_the_ledgers_grant_to_one_claim() {
    /// One machine and ledger, and what a claim of `asked` cores is granted on them.
    struct Case {
        load5: f64,
        /// The cores the other held claims asked.
        held: &'static [u32],
        /// A waiter ahead, and whether it waits for cores alone.
        waiter: Option<bool>,
        asked: u32,
        granted: u32,
    }
    let case = |load5, held, waiter, asked, granted| Case {
        load5,
        held,
        waiter,
        asked,
        granted,
    };
    let cases = [
        case(0.0, &[], None, 12, 12),
        case(24.0, &[], None, 12, 4),
        case(6.0, &[], None, 12, 8),
        case(24.0, &[], None, 2, 2),
        case(60.0, &[], None, 12, 2),
        case(200.0, &[], None, 12, 1),
        case(0.0, &[4], None, 12, 8),
        case(12.0, &[4], None, 12, 4),
        case(0.0, &[], Some(false), 12, 6),
        case(0.0, &[], Some(true), 12, 6),
        case(0.0, &[8], Some(true), 12, 1),
    ];
    for (
        at,
        Case {
            load5,
            held,
            waiter,
            asked,
            granted,
        },
    ) in cases.into_iter().enumerate()
    {
        let tmp = aterm_tempfile::tempdir().expect("scratch");
        let ledger = Ledger::at(tmp.path().join("ledger"));
        let reading = machine(load5);
        let fixed = Fixed::new(reading.clone());
        let _held: Vec<_> = held
            .iter()
            .map(|&cores| {
                aterm_capacity::admit::try_admit_with(
                    &ledger,
                    &fixed,
                    Need::new("other work").cores(cores),
                )
                .expect("the ledger answers")
                .expect("admitted")
            })
            .collect();
        let _waiter = waiter.map(|cores_alone| {
            let need = Need::new("a waiter ahead").cores(3);
            aterm_capacity::admit::Waiter::enter(
                &ledger,
                &fixed,
                if cores_alone { need } else { need.mem(1 << 20) },
            )
            .expect("entered")
        });
        let asker = asker_after(&ledger);
        let others = Others::of(&ledger.list().expect("lists"), &asker, unix_ms());
        let ours = fair(asked, &reading, &others, 0.0);
        let theirs = aterm_capacity::admit::try_admit_with(
            &ledger,
            &fixed,
            Need::new("one claim for the run").cores(asked),
        )
        .expect("the ledger answers")
        .expect("admitted")
        .cores();
        assert_eq!((ours, theirs), (granted, granted), "case {at}: {others:?}");
    }
}

/// A RUN'S OWN BINARIES ARE THE CLAIM ITS SHARE IS ASKED FOR: their claims are neither cores
/// held against it nor waiters ahead of it, as a claim of no cores and a claim one of its
/// ancestors holds are not. NEGATIVE CONTROL: read by another process, the same records are
/// three held cores; read by a process under this one, they are its kin and none.
#[test]
fn a_runs_own_binaries_are_no_party_to_its_share() {
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    let ledger = Ledger::at(tmp.path().join("ledger"));
    let fixed = Fixed::new(machine(0.0));
    let admit = |need: Need| {
        aterm_capacity::admit::try_admit_with(&ledger, &fixed, need)
            .expect("the ledger answers")
            .expect("admitted")
    };
    let _binaries: Vec<_> = (0..3)
        .map(|_| admit(Need::new(BINARY_CLAIM).cores(1)))
        .collect();
    let _memory = admit(Need::new("a compile").mem(1 << 20));
    let records = ledger.list().expect("lists");
    let mut asker = asker_after(&ledger);
    assert_eq!(Others::of(&records, &asker, unix_ms()), Others::default());
    assert_eq!(
        fair(
            12,
            &machine(0.0),
            &Others::of(&records, &asker, unix_ms()),
            0.0
        ),
        12,
        "three of the run's binaries running, its share is still the 12 it asked"
    );
    asker.pid += 1;
    assert_eq!(Others::of(&records, &asker, unix_ms()).held, 3);
    asker.ancestors = vec![std::process::id()];
    assert_eq!(Others::of(&records, &asker, unix_ms()), Others::default());
}

/// A held claim's cores are in the 5-minute load by how long it has been held, and a waiter
/// that arrived after the dispatch began is behind it.
#[test]
fn a_held_claim_accounts_for_load_by_its_age_and_a_later_waiter_is_behind() {
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    let ledger = Ledger::at(tmp.path().join("ledger"));
    let fixed = Fixed::new(machine(0.0));
    let _held =
        aterm_capacity::admit::try_admit_with(&ledger, &fixed, Need::new("other work").cores(4))
            .expect("the ledger answers")
            .expect("admitted");
    let mut asker = asker_after(&ledger);
    asker.pid += 1;
    let records = ledger.list().expect("lists");
    let admitted = records[0].held.expect("held").admitted_ms;
    let tau = u64::try_from(aterm_capacity::admit::LOAD5_TIME_CONSTANT.as_millis()).unwrap();
    let aged = Others::of(&records, &asker, admitted + tau);
    assert_eq!(aged.held, 4);
    assert!(
        (aged.in_load - 4.0 * (1.0 - (-1.0_f64).exp())).abs() < 1e-9,
        "{aged:?}"
    );
    // A 5-minute load of 6 on 12 cores: 2.53 of it is that claim's, and the rest nobody's.
    assert_eq!(fair(12, &machine(6.0), &aged, 0.0), 6);
    assert_eq!(
        fair(
            12,
            &machine(6.0),
            &Others {
                in_load: 0.0,
                ..aged
            },
            0.0
        ),
        5,
        "unaccounted, the claim's own load reads as more work nobody claimed"
    );
    let _waiter =
        aterm_capacity::admit::Waiter::enter(&ledger, &fixed, Need::new("a later waiter").cores(3))
            .expect("entered");
    let records = ledger.list().expect("lists");
    asker.since_ms = 0;
    assert_eq!(Others::of(&records, &asker, admitted).waiting, 0);
    asker.since_ms = u64::MAX;
    let behind = Others::of(&records, &asker, admitted);
    assert_eq!((behind.waiting, behind.reserved), (1, 3));
}

/// THE LOAD A RUN'S OWN BINARIES PUT IN IS NOT WORK NOBODY CLAIMED. Alone on a machine whose
/// 5-minute load of 12 is its own binaries', a run has the 12 cores it asked. NEGATIVE CONTROL:
/// with that load unread the share is 6, and a run would hold itself to half the machine.
#[test]
fn a_runs_own_load_is_taken_out_of_the_unclaimed_work() {
    let none = Others::default();
    assert_eq!(fair(12, &machine(12.0), &none, 12.0), 12);
    assert_eq!(fair(12, &machine(12.0), &none, 6.0), 8);
    assert_eq!(fair(12, &machine(12.0), &none, 0.0), 6);
    assert_eq!(
        fair(12, &machine(12.0), &none, 40.0),
        12,
        "more than the load is none of it"
    );
    let unread = Reading {
        load5: None,
        ..machine(0.0)
    };
    assert_eq!(
        fair(12, &unread, &none, 0.0),
        12,
        "the load unread, as the ledger has it"
    );
    let held = Others {
        held: 12,
        ..Others::default()
    };
    assert_eq!(
        fair(12, &machine(0.0), &held, 0.0),
        1,
        "never none: a binary alone asks the ledger, which holds it until a core is free"
    );
    assert_eq!(fair(0, &machine(0.0), &none, 0.0), 1);
}

/// [`OwnLoad`] averages the binaries' CPU rate as the 5-minute load does: nothing at its first
/// note, `1 - 1/e` of a steady rate after one time constant, and decaying by `1/e` over the
/// next with nothing running.
#[test]
fn own_load_averages_the_rate_as_the_five_minute_load_does() {
    let tau = aterm_capacity::admit::LOAD5_TIME_CONSTANT;
    let start = Instant::now();
    let mut own = OwnLoad::default();
    assert_eq!(own.note(8.0, start), 0.0);
    let risen = own.note(8.0, start + tau);
    assert!(
        (risen - 8.0 * (1.0 - (-1.0_f64).exp())).abs() < 1e-9,
        "{risen}"
    );
    let fallen = own.note(0.0, start + tau + tau);
    assert!((fallen - risen * (-1.0_f64).exp()).abs() < 1e-9, "{fallen}");
    assert_eq!(own.note(f64::NAN, start + tau + tau), fallen);
}

/// A [`Live`] share reads the machine again once its last read is
/// [`aterm_capacity::admit::RESOURCE_PROBE_MAX_AGE`] old, says what a look found only where
/// the share moved, and is the recording pass's grant where the ledger cannot be read.
#[test]
fn a_live_share_is_read_again_as_it_ages_and_is_the_grant_where_unread() {
    let looks = std::sync::Arc::new(AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&looks);
    let live = Live::new(
        12,
        4,
        Box::new(move || {
            // The load falls from 24 to none at the third look.
            let load = if counted.fetch_add(1, Ordering::SeqCst) < 2 {
                24.0
            } else {
                0.0
            };
            Some(Look {
                machine: machine(load),
                others: Others::default(),
            })
        }),
        None,
    );
    assert_eq!(live.range(), None);
    let start = Instant::now();
    let age = aterm_capacity::admit::RESOURCE_PROBE_MAX_AGE;
    let (share, moved) = live.read(0.0, start);
    assert_eq!((share, moved.map(|m| m.share)), (4, Some(4)));
    assert_eq!(live.read(0.0, start + age / 2), (4, None));
    assert_eq!(
        looks.load(Ordering::SeqCst),
        1,
        "a fresh read is not taken again"
    );
    assert_eq!(
        live.read(0.0, start + age),
        (4, None),
        "the same share is no news"
    );
    assert_eq!(looks.load(Ordering::SeqCst), 2);
    let (share, moved) = live.read(0.0, start + age * 2);
    let moved = moved.expect("the share moved");
    assert_eq!((share, moved.share), (12, 12));
    assert_eq!(moved.look.expect("a look").machine.load5, Some(0.0));
    assert_eq!(live.range(), Some((4, 12)));

    let unread = Live::new(12, 4, Box::new(|| None), None);
    let (share, moved) = unread.read(0.0, start);
    assert_eq!(share, 4, "the ledger's own last word");
    assert_eq!(moved.expect("the first read").look, None);
}

/// [`Live::refuses`] holds a binary back beside others while its charge does not fit in what
/// they leave of the share, or their measured rate with its charge is past the share and one
/// core, and never holds back a binary with none beside it.
#[test]
fn a_live_share_refuses_what_does_not_fit_beside_the_running_binaries() {
    let live = Live::new(
        12,
        4,
        Box::new(|| {
            Some(Look {
                machine: machine(24.0),
                others: Others::default(),
            })
        }),
        None,
    );
    let now = Instant::now();
    let refused = Some(Refused {
        share: 4,
        asked: 12,
    });
    assert_eq!(
        live.refuses(2, 2, 0.0, now).0,
        None,
        "two beside two fill a share of 4"
    );
    assert_eq!(live.refuses(2, 3, 0.0, now).0, refused);
    assert_eq!(
        live.refuses(1, 1, 4.5, now).0,
        refused,
        "the running binaries use more than they were charged"
    );
    assert_eq!(live.refuses(1, 1, 4.0, now).0, None);
    assert_eq!(
        live.refuses(12, 0, 9.0, now).0,
        None,
        "alone, it asks the ledger whatever its charge"
    );
    assert_eq!(live.asked(), 12);
}

/// The members of `kind` in the pool at `dir` a test could take now, each held.
fn takeable(dir: &Path, kind: &str, total: usize) -> Vec<LockFile> {
    (0..total)
        .filter_map(|index| {
            LockFile::take_existing(
                &dir.join(format!("{kind}-{index:04}.lock")),
                Instant::now(),
                "busy",
            )
            .ok()
        })
        .collect()
}

/// THE SUBPROCESS SLOTS FOLLOW THE SHARE. A pool made for 12 cores has 6 stack slots and 3 boot
/// permits. On a share of 4 the run holds 4 slots and 2 permits back, and a test finds 2 and 1,
/// as a run granted 4 cores always had; on the 12 asked every one is open. A slot a test holds
/// stays the test's: the run takes it once it is free.
#[test]
fn the_subprocess_slots_follow_the_share() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let total = crate::testrun::subprocess_slots_for_cores(12);
    assert_eq!(total, 6);
    let pool = crate::testrun::subprocess_pool(scratch.path(), total).expect("a pool");
    let slots = Slots::of(pool.path(), total);
    assert_eq!(slots.follow(4), [2, 1]);
    assert_eq!(takeable(pool.path(), "slot", 6).len(), 2);
    assert_eq!(takeable(pool.path(), "boot", 3).len(), 1);
    assert_eq!(slots.follow(12), [6, 3]);
    let tests = takeable(pool.path(), "slot", 6);
    assert_eq!(tests.len(), 6, "every slot is open to the tests");
    assert_eq!(
        slots.follow(1),
        [6, 1],
        "the tests hold every slot, so none can be held back yet"
    );
    drop(tests);
    assert_eq!(slots.follow(1), [1, 1]);
    assert_eq!(takeable(pool.path(), "slot", 6).len(), 1);
    drop(slots);
    assert_eq!(
        takeable(pool.path(), "slot", 6).len(),
        6,
        "the run's hold ends with it"
    );
}
