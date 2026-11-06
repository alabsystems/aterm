// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT AN ACKNOWLEDGED APPEND COSTS THE DEVICE. `astream_sys::flush_tally` is process-wide, so
//! the journal's flushes are read in a binary of their own: here nothing else flushes while a
//! reading is taken. The binary takes the full flushes aterm ships (`F_FULLFSYNC` on Apple),
//! whatever test mark it inherits, so a timing here is never a test's plain `fsync(2)`.

#![cfg(unix)]

use aterm_journal::{Limits, Log};
use std::time::{Duration, Instant};

aterm_durable::shipped_durable_flushes_in_tests!();

/// What `astream_sys::flush_tally` moved by between two readings: `(full, barrier)`.
fn moved(before: astream_sys::FlushTally, after: astream_sys::FlushTally) -> (u64, u64) {
    (after.full - before.full, after.barrier - before.barrier)
}

fn limits() -> Limits {
    Limits {
        records: 1024,
        bytes: 1 << 20,
        record_bytes: 1024,
    }
}

/// The flushes `f` moved the tally by: `(full, barrier)`.
fn flushes<T>(f: impl FnOnce() -> T) -> ((u64, u64), T) {
    let before = astream_sys::flush_tally();
    let made = f();
    (moved(before, astream_sys::flush_tally()), made)
}

/// What one append, one append with a dependent write, and one reopen flush.
#[derive(Debug, PartialEq, Eq)]
struct Costs {
    append: (u64, u64),
    then: (u64, u64),
    reopen: (u64, u64),
}

/// [`Costs`] of a fresh journal in `dir`, its dependent write a private file in `beside`.
fn costs(dir: &std::path::Path, beside: &std::path::Path) -> (Costs, astream_broker::Tier) {
    let mut log = Log::open(dir, "count", b"pinned", false, limits()).unwrap();
    log.finish_open().unwrap();
    let served = log.served().served;
    let (append, _) = flushes(|| log.append("app/count", vec![1; 64]).unwrap());
    let (then, _) = flushes(|| {
        log.append_then("app/count", vec![2; 64], beside, |_, flush| {
            aterm_uds::private_file::write_private_flushed(beside, "witness", b"2", flush)
        })
        .unwrap()
    });
    drop(log);
    let (reopen, log) = flushes(|| Log::open(dir, "count", b"pinned", true, limits()).unwrap());
    assert_eq!(
        log.read_range(astream_wire::Offset::ZERO, 8).unwrap().len(),
        2
    );
    (
        Costs {
            append,
            then,
            reopen,
        },
        served,
    )
}

/// ONE FULL FLUSH ACKNOWLEDGES AN APPEND on a log astream serves Ordered: the record, the
/// checkpoint's file and its directory each take a barrier, and the commit one full flush. A
/// dependent write follows the commit: beside the log it takes two barriers more and no full
/// flush, and on another device it is flushed in full. A reopen takes one full flush of its own
/// and what astream's open takes. A log served Strict (off the boot volume) flushes the record and
/// the checkpoint in full, the commit then finding nothing to cover. NEGATIVE CONTROL: the
/// append with its checkpoint and dependent write flushed in full, as before its commit
/// (`Mutant::Durable`), reads three full flushes an append and five an append with its
/// dependent write, and the Ordered assertion fails on it.
#[test]
fn an_append_takes_one_full_flush_on_an_ordered_log() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let beside = scratch.path().join("beside");
    aterm_uds::private_file::create_private_dir(&beside).unwrap();
    let (ordered, served) = costs(&scratch.path().join("ordered"), &beside);
    let (strict, latched) = {
        let _strict = aterm_journal::seam::serve_strict();
        costs(&scratch.path().join("strict"), &beside)
    };
    assert_eq!(latched, astream_broker::Tier::Strict);
    let (apart, _) = {
        let _apart = aterm_journal::seam::apart();
        costs(&scratch.path().join("apart"), &beside)
    };
    let (durable, _) = {
        let _durable = aterm_journal::seam::mutate(aterm_journal::seam::Mutant::Durable);
        costs(&scratch.path().join("durable"), &beside)
    };
    println!(
        "served={served:?}: ordered {ordered:?}, strict {strict:?}, apart {apart:?}, \
         durable (control) {durable:?}"
    );
    // Off Apple a barrier is a full flush, counted `barrier`; nothing there serves Ordered.
    assert_eq!(
        served == astream_broker::Tier::Ordered,
        cfg!(target_vendor = "apple"),
        "astream serves an Apple boot volume Ordered, and Strict off Apple"
    );
    if served != astream_broker::Tier::Ordered {
        return;
    }
    // astream's own reopen of a log like the journal's, two records long.
    let bare = scratch.path().join("bare.astream");
    let options = || {
        let mut options =
            astream_broker::OpenOptions::new(astream_broker::Durability::Ordered { horizon: None });
        options.acks = astream_broker::Acks::FullFlush;
        options
    };
    let mut log = astream_broker::BrokerLog::open_opts(&bare, options(), || {
        Ok(astream_broker::Prepared::Nothing)
    })
    .unwrap();
    for seq in 1..=2 {
        log.publish_checked(1, seq, "app/count".into(), vec![1; 64])
            .unwrap();
    }
    log.sync_full().unwrap();
    drop(log);
    let (astream_reopen, _) = flushes(|| {
        astream_broker::BrokerLog::open_opts(&bare, options(), || {
            Ok(astream_broker::Prepared::Nothing)
        })
        .unwrap()
    });
    println!("astream's own reopen {astream_reopen:?}");
    let reopen = ordered.reopen;
    assert_eq!(
        reopen.0,
        astream_reopen.0 + 1,
        "a reopen takes one full flush of its own"
    );
    let one_commit = |costs: &Costs| costs.append.0 == 1 && costs.then.0 == 1;
    assert!(one_commit(&ordered), "{ordered:?}");
    assert_eq!((ordered.append.1, ordered.then.1), (3, 5));
    // A Strict log's open takes no barrier of astream's: only its full flushes compare.
    assert_eq!(
        (strict.append, strict.then, strict.reopen.0),
        ((3, 0), (5, 0), reopen.0)
    );
    assert_eq!(
        apart,
        Costs {
            append: (1, 3),
            then: (3, 3),
            reopen,
        }
    );
    assert_eq!(
        durable,
        Costs {
            append: (3, 1),
            then: (5, 1),
            reopen,
        }
    );
    assert!(!one_commit(&durable), "the control is caught");
}

/// The `p`th percentile of `sorted`, nearest rank.
fn percentile(sorted: &[Duration], p: usize) -> Duration {
    sorted[(sorted.len() * p).div_ceil(100).saturating_sub(1)]
}

/// One reading of `APPENDS` acknowledged appends of 256 bytes to a fresh journal, and of
/// the journal's reopen: each append's wall time, and the flushes per append and per open.
/// Run it alone, in an optimized build: `--profile opt-test -- --ignored --exact
/// a_journal_append_reports_its_flushes_and_latency --nocapture`. Timing is never a pass
/// condition.
#[test]
#[ignore = "isolated flush measurement; run it alone with --ignored --nocapture"]
fn a_journal_append_reports_its_flushes_and_latency() {
    const APPENDS: usize = 700;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("journal");
    let before = astream_sys::flush_tally();
    let mut log = Log::open(&path, "measure", b"pinned", false, limits()).unwrap();
    log.finish_open().unwrap();
    let created = moved(before, astream_sys::flush_tally());
    let mut each = Vec::with_capacity(APPENDS);
    let before = astream_sys::flush_tally();
    let start = Instant::now();
    for n in 0..APPENDS {
        let began = Instant::now();
        log.append("app/measure", vec![u8::try_from(n % 251).unwrap(); 256])
            .unwrap();
        each.push(began.elapsed());
    }
    let wall = start.elapsed();
    let (full, barrier) = moved(before, astream_sys::flush_tally());
    drop(log);
    let before = astream_sys::flush_tally();
    let reopened = Log::open(&path, "measure", b"pinned", true, limits()).unwrap();
    let opened = moved(before, astream_sys::flush_tally());
    drop(reopened);
    each.sort();
    let appends = APPENDS as f64;
    println!(
        "journal-append n={APPENDS} wall_ms={:.1} p50_us={} p95_us={} p99_us={} \
         full_per_append={:.2} barrier_per_append={:.2} create_full={} create_barrier={} \
         reopen_full={} reopen_barrier={}{}",
        wall.as_secs_f64() * 1e3,
        percentile(&each, 50).as_micros(),
        percentile(&each, 95).as_micros(),
        percentile(&each, 99).as_micros(),
        full as f64 / appends,
        barrier as f64 / appends,
        created.0,
        created.1,
        opened.0,
        opened.1,
        aterm_durable::durable_flushes_note(),
    );
}
