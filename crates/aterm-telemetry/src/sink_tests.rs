// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};

use super::{Counts, Msg, QUEUE, run};
use crate::log::{self, Limits, Log};
use crate::row::{Kind, Row};
use crate::tests::{facts, row};

/// Run the writer over `rows`, queued before it starts so they are one batch, until the
/// queue ends.
fn batch(log: &Log, rows: &[Row], counts: &Counts) {
    let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
    for row in rows {
        assert!(tx.send(Msg::Row(Box::new(row.clone()))).is_ok(), "queued");
    }
    drop(tx);
    run(log, &rx, counts);
}

/// AN UNCUT BATCH'S ROWS THE LOG HOLDS ARE NOT SAID LOST (TM-1 review, reproduced by the
/// sixth review): a batch of three byte-identical rows whose write fails 10 bytes into the
/// third and cannot be cut back leaves two whole and one torn; the log counts the two from its
/// length after the write, so the writer counts them written and only the third lost, and the
/// next batch's gap line says one row. NEGATIVE CONTROLS (by hand): every row of a failed
/// batch counted lost, as before: three said, two of them in the log; the log asked which
/// lines it holds (the first version of this fix): an identical whole row answers for the
/// torn one, none said; the split inverted: two said.
#[test]
fn an_uncut_batchs_rows_the_log_holds_are_not_said_lost() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("telemetry");
    let left = Arc::new(AtomicU64::new(u64::MAX));
    let log = Log::new(dir.clone(), Limits::DEFAULT, facts())
        .cut_after(Arc::clone(&left))
        .never_cut_back();
    let counts = Counts::default();
    batch(&log, &[row(1, Kind::Spawn, 1)], &counts);
    let two = row(2, Kind::Spawn, 2);
    left.store(2 * two.line().len() as u64 + 10, SeqCst);
    batch(&log, &[two.clone(), two.clone(), two], &counts);
    assert_eq!(
        (counts.written.load(SeqCst), counts.dropped.load(SeqCst)),
        (3, 1),
        "the two whole rows written, the torn one lost"
    );
    left.store(u64::MAX, SeqCst);
    batch(&log, &[row(4, Kind::Spawn, 4)], &counts);
    let read = log::read(&dir, 4, None).expect("read");
    assert_eq!(
        read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
        vec![1, 2, 2, 4]
    );
    assert_eq!(
        (read.dropped, read.torn),
        (1, 1),
        "the torn row said lost once, the whole ones not"
    );
}

/// AN UNCUT BATCH THE LOG CANNOT MEASURE IS COUNTED NEITHER WAY (TM-1 review): where the
/// file's length cannot be read after a write that could not be cut back, the log cannot say
/// which rows it left, so the writer counts none written and none lost: the row it left whole
/// is never said lost, and the torn one is missing at worst. NEGATIVE CONTROL (by hand: such
/// a batch's rows counted lost): two said, one of them in the log.
#[test]
fn an_uncut_batch_the_log_cannot_measure_is_said_neither_written_nor_lost() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("telemetry");
    let left = Arc::new(AtomicU64::new(u64::MAX));
    let log = Log::new(dir.clone(), Limits::DEFAULT, facts())
        .cut_after(Arc::clone(&left))
        .never_cut_back()
        .never_measured();
    let counts = Counts::default();
    batch(&log, &[row(1, Kind::Spawn, 1)], &counts);
    let (two, three) = (row(2, Kind::Spawn, 2), row(3, Kind::Spawn, 3));
    left.store(two.line().len() as u64 + 10, SeqCst);
    batch(&log, &[two, three], &counts);
    assert_eq!(
        (counts.written.load(SeqCst), counts.dropped.load(SeqCst)),
        (1, 0),
        "neither written nor lost"
    );
    left.store(u64::MAX, SeqCst);
    batch(&log, &[row(4, Kind::Spawn, 4)], &counts);
    let read = log::read(&dir, 4, None).expect("read");
    assert_eq!(
        read.rows.iter().map(|r| r.at_ms).collect::<Vec<_>>(),
        vec![1, 2, 4]
    );
    assert_eq!(
        (read.dropped, read.torn),
        (0, 1),
        "the whole row never said lost"
    );
}
