// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT A COMMONS TABLE OPERATION COSTS THE DEVICE. `astream_sys::flush_tally` is process-wide,
//! so a table's flushes are read in a binary of their own: here nothing else flushes while a
//! reading is taken, but for the neighbour the measurement runs on purpose, whose own flushes
//! it counts apart. The binary takes the full flushes aterm ships (`F_FULLFSYNC` on Apple),
//! whatever test mark it inherits, so a timing here is never a test's plain `fsync(2)`.

#![cfg(unix)]

use aterm_db::table::{Session, fixture::Lab, store::Store};
use aterm_secrets::commons::roster::Role;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

aterm_durable::shipped_durable_flushes_in_tests!();

/// The flushes `f` moved the tally by: `(full, barrier)`.
fn flushes<T>(f: impl FnOnce() -> T) -> (u64, u64) {
    let before = astream_sys::flush_tally();
    drop(f());
    let after = astream_sys::flush_tally();
    (after.full - before.full, after.barrier - before.barrier)
}

/// ONE FULL FLUSH ACKNOWLEDGES A COMMONS TABLE OPERATION, local or ingested, where astream
/// serves the table's journal Ordered: the record and the checkpoint's file and directory each
/// take a barrier, the journal's commit one full flush, and after it the witness's file and
/// directory a barrier each (`aterm_journal::Log::append_then`). A journal served Strict (off
/// the boot volume) flushes each of those five in full, and its commit finds nothing to cover.
/// NEGATIVE CONTROL: the append with its checkpoint and witness flushed in full, as before its
/// commit (`Mutant::Durable`), reads five full flushes an operation, and the Ordered assertion
/// fails on it.
#[test]
fn a_table_operation_takes_one_full_flush_on_an_ordered_journal() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let root = aterm_tempfile::tempdir().unwrap();
    let a = lab.bootstrap(0);
    let ours = Session::open(&a).unwrap();
    let mut store = Store::create(&ours, &root.path().join("a")).unwrap();
    let scope = store.scope();
    let b = lab.bootstrap(1);
    let theirs = Session::open(&b).unwrap();
    let mut peer = Store::join(&theirs, &root.path().join("b"), scope).unwrap();
    let wires: Vec<Vec<u8>> = (0..3)
        .map(|n| {
            let receipt = peer
                .commit_catalog(&theirs, format!("peer {n}").as_bytes())
                .unwrap();
            peer.wire(&theirs, &receipt.id).unwrap().unwrap().to_vec()
        })
        .collect();
    drop(peer);
    let probe = aterm_journal::Log::open(
        &root.path().join("probe"),
        "probe",
        b"pinned",
        false,
        aterm_journal::Limits {
            records: 1,
            bytes: 1024,
            record_bytes: 64,
        },
    )
    .unwrap();
    let served = probe.served().served;
    drop(probe);
    let local = flushes(|| store.commit_catalog(&ours, b"local").unwrap());
    let ingest = flushes(|| store.ingest(&ours, &wires[0]).unwrap());
    drop(store);
    let mut strict = {
        let _strict = aterm_journal::seam::serve_strict();
        Store::open(&ours, &root.path().join("a"), scope).unwrap()
    };
    let strict_local = flushes(|| strict.commit_catalog(&ours, b"strict").unwrap());
    let strict_ingest = flushes(|| strict.ingest(&ours, &wires[1]).unwrap());
    drop(strict);
    let mut control = Store::open(&ours, &root.path().join("a"), scope).unwrap();
    let control_ingest = {
        let _durable = aterm_journal::seam::mutate(aterm_journal::seam::Mutant::Durable);
        flushes(|| control.ingest(&ours, &wires[2]).unwrap())
    };
    println!(
        "served={served:?}: local {local:?}, ingest {ingest:?}, strict local {strict_local:?}, \
         strict ingest {strict_ingest:?}, durable (control) ingest {control_ingest:?}"
    );
    assert_eq!(
        served == astream_broker::Tier::Ordered,
        cfg!(target_vendor = "apple"),
        "astream serves an Apple boot volume Ordered, and Strict off Apple"
    );
    if served != astream_broker::Tier::Ordered {
        return;
    }
    let one_commit = |costs: (u64, u64)| costs == (1, 5);
    assert!(
        one_commit(local) && one_commit(ingest),
        "{local:?} {ingest:?}"
    );
    assert_eq!((strict_local, strict_ingest), ((5, 0), (5, 0)));
    assert_eq!(control_ingest, (5, 1));
    assert!(!one_commit(control_ingest), "the control is caught");
}

/// The `p`th percentile of `sorted`, nearest rank.
fn percentile(sorted: &[Duration], p: usize) -> Duration {
    sorted[(sorted.len() * p).div_ceil(100).saturating_sub(1)]
}

/// A bus stand-in beside the table: an Ordered log with no horizon, appended to as fast as it
/// acknowledges, each append its barrier, until `stop`.
struct Neighbour {
    stop: Arc<AtomicBool>,
    /// The flushes its log has made, `(full, barrier)`, stored after each append, so a reading
    /// of the tally counts them apart to within the one append in flight.
    flushed: Arc<(AtomicU64, AtomicU64)>,
    /// Each append's wall time.
    thread: std::thread::JoinHandle<Vec<Duration>>,
}

impl Neighbour {
    fn start(dir: &std::path::Path) -> Self {
        let path = dir.join("bus.log");
        let stop = Arc::new(AtomicBool::new(false));
        let flushed = Arc::new((AtomicU64::new(0), AtomicU64::new(0)));
        let (halt, counts) = (Arc::clone(&stop), Arc::clone(&flushed));
        let thread = std::thread::spawn(move || {
            let mut log = astream_broker::BrokerLog::open_opts(
                &path,
                astream_broker::OpenOptions::new(astream_broker::Durability::Ordered {
                    horizon: None,
                }),
                || Ok(astream_broker::Prepared::Nothing),
            )
            .unwrap();
            let mut each = Vec::new();
            let mut seq = 0;
            while !halt.load(Ordering::Relaxed) {
                seq += 1;
                let began = Instant::now();
                log.publish_checked(1, seq, "ev/bus".into(), vec![7; 64])
                    .unwrap();
                each.push(began.elapsed());
                let made = log.flush_counts();
                counts.0.store(made.full, Ordering::Relaxed);
                counts.1.store(made.barrier, Ordering::Relaxed);
            }
            each
        });
        Self {
            stop,
            flushed,
            thread,
        }
    }

    /// The tally, less the neighbour's own flushes so far.
    fn tally(&self) -> (u64, u64) {
        let tally = astream_sys::flush_tally();
        (
            tally
                .full
                .saturating_sub(self.flushed.0.load(Ordering::Relaxed)),
            tally
                .barrier
                .saturating_sub(self.flushed.1.load(Ordering::Relaxed)),
        )
    }

    fn finish(self) -> Vec<Duration> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.join().unwrap()
    }
}

/// One reading of `INGESTS` records one Mac committed, ingested by another: the ingest's wall
/// time and each one's, the flushes per ingest, and what a bus stand-in beside it took per
/// append meanwhile. Run it alone, in an optimized build: `--profile opt-test -- --ignored
/// --exact a_table_ingest_reports_its_flushes_and_a_neighbours_appends --nocapture`. Timing is
/// never a pass condition.
#[test]
#[ignore = "isolated flush measurement; run it alone with --ignored --nocapture"]
fn a_table_ingest_reports_its_flushes_and_a_neighbours_appends() {
    const INGESTS: usize = 700;
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let root = aterm_tempfile::tempdir().unwrap();
    let a = lab.bootstrap(0);
    let ours = Session::open(&a).unwrap();
    let mut store = Store::create(&ours, &root.path().join("a")).unwrap();
    let scope = store.scope();
    let b = lab.bootstrap(1);
    let theirs = Session::open(&b).unwrap();
    let mut peer = Store::join(&theirs, &root.path().join("b"), scope).unwrap();
    let wires: Vec<Vec<u8>> = (0..INGESTS)
        .map(|n| {
            let receipt = peer
                .commit_catalog(&theirs, format!("record {n}").as_bytes())
                .unwrap();
            peer.wire(&theirs, &receipt.id).unwrap().unwrap().to_vec()
        })
        .collect();
    drop(peer);
    drop(theirs);
    let bus = Neighbour::start(root.path());
    let mut each = Vec::with_capacity(INGESTS);
    let before = bus.tally();
    let start = Instant::now();
    for wire in &wires {
        let began = Instant::now();
        store.ingest(&ours, wire).unwrap();
        each.push(began.elapsed());
    }
    let wall = start.elapsed();
    let after = bus.tally();
    let mut appends = bus.finish();
    let (full, barrier) = (after.0 - before.0, after.1 - before.1);
    each.sort();
    appends.sort();
    let ingests = INGESTS as f64;
    println!(
        "table-ingest n={INGESTS} wall_ms={:.1} p50_us={} p95_us={} full_per_ingest={:.2} \
         barrier_per_ingest={:.2} bus_appends={} bus_p50_us={} bus_p95_us={} bus_p99_us={}{}",
        wall.as_secs_f64() * 1e3,
        percentile(&each, 50).as_micros(),
        percentile(&each, 95).as_micros(),
        full as f64 / ingests,
        barrier as f64 / ingests,
        appends.len(),
        percentile(&appends, 50).as_micros(),
        percentile(&appends, 95).as_micros(),
        percentile(&appends, 99).as_micros(),
        aterm_durable::durable_flushes_note(),
    );
}
