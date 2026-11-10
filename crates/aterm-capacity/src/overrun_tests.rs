// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! OVERRUN on fixed process readings and records: what a holder's tree holds past its held
//! claims, as a waiter's line names it. No test reads the live machine or a ledger.

use std::collections::{HashMap, HashSet};

use crate::admit::{decide, others};
use crate::floor::GIB;
use crate::{Held, Need, Overrun, Proc, Procs, Reading, Record, TreeUse, overruns};

/// The Trust lane's run, the holder.
const RUN: u32 = 18_032;

/// Its trustc, the run's child.
const TRUSTC: u32 = 18_100;

/// A claim by `pid` of `need`, held when `held`.
fn record(pid: u32, start_ms: u64, need: Need, held: bool) -> Record {
    Record {
        accounting: crate::Accounting::Holder,
        timing: crate::TimingEvidence::default(),
        pid,
        start_ms,
        seq: 0,
        checkout: "/ck".to_owned(),
        need,
        held: held.then_some(Held {
            admitted_ms: start_ms,
            cores: 1,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

/// The run's tree as the ledger reads it ([`Proc::of_reading`]): the run itself (100 MiB, all of
/// it resident) and its trustc, `footprint` GiB of footprint in `resident` GiB of resident pages.
fn tree(footprint: u64, resident: u64) -> HashMap<u32, TreeUse> {
    let read = |pid, ppid, footprint, resident| {
        Proc::of_reading(&aterm_sysprobe::ProcessReading {
            pid,
            ppid,
            footprint: Some(footprint),
            resident: Some(resident),
            ..aterm_sysprobe::ProcessReading::default()
        })
    };
    Procs::of(vec![
        read(RUN, 1, 100 << 20, 100 << 20),
        read(TRUSTC, RUN, footprint * GIB, resident * GIB),
    ])
    .trees(&[RUN])
}

/// The line a waiter of 12 GiB on 14 GiB available names the held claims `records` by, its
/// holders' trees `trees`.
fn waiters_line(records: &[Record], trees: &HashMap<u32, TreeUse>) -> String {
    let r = Reading::of_gib(48, 14, 14);
    let records: Vec<&Record> = records.iter().collect();
    let taken: HashSet<u32> = records.iter().map(|r| r.pid).collect();
    let others = others(&records, taken, trees);
    let need = Need::new("trust check aterm-render").mem_gib(12);
    decide(&need, (9, 1, 0), &r, &others, 60_000)
        .expect_err("waits")
        .to_string()
}

/// THE LIVE STALL OF 2026-10-30: a Trust lane run held a 16 GiB claim while its trustc's
/// footprint was 33 GB, most of it compressed. A tree of 31.1 GiB in 9.1 GiB of resident pages
/// (trustc's 31 in 9, the run's own 100 MiB) reserves 6.9 GiB, as before, and a waiter's line
/// now says its tree holds 31.1 GiB, 22 GiB of it compressed or swapped and 9.1 GiB in memory:
/// 15.1 GiB past its 16 GiB claim. A tree within its claim through compressed pages (trustc's
/// 14 GiB in 9) is not named past it, its line the old one. NEGATIVE CONTROL: the old line, which
/// named only the 6.9 GiB its holder still reserved, says neither the 31.1 GiB nor the overrun,
/// and the first assertion fails on it.
#[test]
fn a_waiters_line_names_a_holder_over_its_claim_through_compressed_pages() {
    let run = [record(
        RUN,
        0,
        Need::new("trust check aterm-gui").mem_gib(16),
        true,
    )];
    let over = tree(31, 9);
    assert_eq!(over[&RUN].charged, 9 * GIB + (100 << 20));
    assert_eq!(over[&RUN].footprint, 31 * GIB + (100 << 20));
    let line = waiters_line(&run, &over);
    assert!(
        line.contains(
            "pid 18032 holds 6.9 GiB of memory for 1m00s: \"trust check aterm-gui\" in /ck, its \
             tree holds 31.1 GiB, 22 GiB of it compressed or swapped and 9.1 GiB in memory: \
             15.1 GiB past its 16 GiB claim"
        ),
        "{line}"
    );
    let within = waiters_line(&run, &tree(14, 9));
    assert!(
        within.ends_with(
            "pid 18032 holds 6.9 GiB of memory for 1m00s: \"trust check aterm-gui\" in /ck"
        ),
        "{within}"
    );
    assert!(!within.contains("past"), "{within}");
}

/// A holder of several claims is reckoned against them all, its tree counted once: two held
/// 8 GiB claims whose tree holds 20 GiB are 4 GiB past the 16 GiB of its 2 claims, on each one's
/// line; with 20 GiB claimed it is within them and named past none. A waiting claim of its own,
/// or a held one of no memory (a disk claim), never carries it. NEGATIVE CONTROL: reckoned claim
/// by claim, each 8 GiB claim would read 12 GiB past it.
#[test]
fn a_holder_of_several_claims_is_reckoned_against_them_all() {
    let trees = HashMap::from([(
        RUN,
        TreeUse {
            charged: 12 * GIB,
            footprint: 20 * GIB,
            written: 0,
        },
    )]);
    let claims = |second: u64| {
        [
            record(RUN, 0, Need::new("trust check a").mem_gib(8), true),
            record(RUN, 1, Need::new("trust check b").mem_gib(second), true),
            record(RUN, 2, Need::new("trust check c").mem_gib(4), false),
            record(RUN, 3, Need::new("disk").disk(GIB, "/vol"), true),
        ]
    };
    let past = overruns(&claims(8), Some(&trees));
    assert_eq!(
        past.get(&RUN),
        Some(&Overrun {
            holds: 20 * GIB,
            counted: 12 * GIB,
            claimed: 16 * GIB,
            claims: 2,
        })
    );
    assert_eq!(
        past[&RUN].to_string(),
        "20 GiB, 8 GiB of it compressed or swapped and 12 GiB in memory: 4 GiB past the 16 GiB \
         of its 2 claims"
    );
    let line = waiters_line(&claims(8), &trees);
    assert_eq!(
        line.matches("4 GiB past the 16 GiB of its 2 claims")
            .count(),
        2,
        "{line}"
    );
    assert!(!line.contains("12 GiB past"), "{line}");
    assert!(overruns(&claims(12), Some(&trees)).is_empty());
    assert!(
        overruns(&claims(8), None).is_empty(),
        "no sweep, none known"
    );
}

/// A tree past its claim with nothing compressed names its whole alone: 20 GiB resident under a
/// 16 GiB claim is `20 GiB: 4 GiB past its 16 GiB claim`. One a twentieth of a GiB past it is
/// not named, so no line says `0 GiB past`.
#[test]
fn an_overrun_names_what_is_compressed_only_where_there_is_some() {
    let at = |whole, counted| {
        let trees = HashMap::from([(
            RUN,
            TreeUse {
                charged: counted,
                footprint: whole,
                written: 0,
            },
        )]);
        overruns(
            &[record(RUN, 0, Need::new("lint").mem_gib(16), true)],
            Some(&trees),
        )
        .remove(&RUN)
    };
    assert_eq!(
        at(20 * GIB, 20 * GIB).map(|o| o.to_string()).as_deref(),
        Some("20 GiB: 4 GiB past its 16 GiB claim")
    );
    assert_eq!(at(16 * GIB + GIB / 20, 16 * GIB + GIB / 20), None);
}

/// A claim of whole or detached accounting attributes no current process to its record's pid
/// (which may since have been reused) and reserves its whole need, so it is neither reckoned nor
/// named: beside a held 16 GiB holder claim of the same pid whose tree holds 31.1 GiB, a held
/// 16 GiB claim of either kind leaves the holder 15.1 GiB past its one 16 GiB claim, named on
/// the holder claim's line alone. NEGATIVE CONTROL: reckoned with the other claim, the holder
/// would be within the 32 GiB of its 2 claims and named past nothing.
#[test]
fn a_claim_of_other_accounting_is_neither_reckoned_nor_named() {
    for accounting in [crate::Accounting::Whole, crate::Accounting::Detached] {
        let mut cohort = record(RUN, 1, Need::new("compiler cohort").mem_gib(16), true);
        cohort.accounting = accounting;
        let claims = [
            record(RUN, 0, Need::new("trust check aterm-gui").mem_gib(16), true),
            cohort,
        ];
        let over = tree(31, 9);
        let past = overruns(&claims, Some(&over));
        assert_eq!(
            past.get(&RUN).map(|o| (o.claimed, o.claims)),
            Some((16 * GIB, 1)),
            "{accounting:?}"
        );
        let line = waiters_line(&claims, &over);
        assert_eq!(
            line.matches("15.1 GiB past its 16 GiB claim").count(),
            1,
            "{accounting:?}: {line}"
        );
        let named = line
            .split("; ")
            .find(|part| part.contains("\"compiler cohort\""))
            .unwrap_or_default();
        assert!(
            named.contains("holds 16 GiB of memory") && !named.contains("past"),
            "{accounting:?}: {line}"
        );
    }
}
