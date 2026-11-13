// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WRITER: rows go to a thread of their own through a bounded queue, so no session waits
//! on the disk. [`Sink::emit`] never blocks: a row that finds the queue full is counted, and the
//! writer says how many it lost in a `kind=gap` line before its next batch, a write of its own
//! ([`Log::append_saying`]), so a batch that fails after it never has the line's rows said again.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::log::Log;
use crate::row::Row;

/// How many rows the queue holds before [`Sink::emit`] drops one.
pub const QUEUE: usize = 1024;

/// The most rows one append writes.
const BATCH: usize = 256;

enum Msg {
    Row(Box<Row>),
    Flush(SyncSender<()>),
}

/// A running writer.
#[derive(Debug)]
pub struct Sink {
    tx: SyncSender<Msg>,
    counts: Arc<Counts>,
}

/// What a writer did, for `aterm ctl telemetry`'s first line.
#[derive(Debug, Default)]
pub struct Counts {
    /// Rows handed to the writer.
    pub emitted: AtomicU64,
    /// Rows written, a failed append's rows the log holds whole among them.
    pub written: AtomicU64,
    /// Rows lost: the queue was full, or their append failed and the log does not hold them. A
    /// failed append the log could not judge counts its rows in neither.
    pub dropped: AtomicU64,
}

impl Sink {
    /// Start a writer appending to `log`.
    ///
    /// # Errors
    /// The thread could not be started.
    pub fn start(log: Log) -> std::io::Result<Self> {
        let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
        let counts = Arc::new(Counts::default());
        let theirs = Arc::clone(&counts);
        std::thread::Builder::new()
            .name("aterm-telemetry".to_owned())
            .spawn(move || run(&log, &rx, &theirs))?;
        Ok(Self { tx, counts })
    }

    /// Hand `row` to the writer; never blocks.
    pub fn emit(&self, row: Row) {
        self.counts.emitted.fetch_add(1, Ordering::Relaxed);
        match self.tx.try_send(Msg::Row(Box::new(row))) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.counts.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Wait up to `within` for every row emitted so far to be appended (or dropped): `false`
    /// when the writer did not answer in time.
    #[must_use]
    pub fn flush(&self, within: Duration) -> bool {
        let (done, wait) = std::sync::mpsc::sync_channel(1);
        if self.tx.send(Msg::Flush(done)).is_err() {
            return false;
        }
        wait.recv_timeout(within).is_ok()
    }

    /// Its counts.
    #[must_use]
    pub fn counts(&self) -> &Counts {
        &self.counts
    }
}

fn run(log: &Log, rx: &Receiver<Msg>, counts: &Counts) {
    let mut told = 0u64;
    let mut flushes = Vec::new();
    let mut lines = Vec::with_capacity(BATCH);
    while let Ok(first) = rx.recv() {
        lines.clear();
        let mut rows = 0usize;
        let lost = counts.dropped.load(Ordering::Relaxed);
        let mut take = |msg: Msg, lines: &mut Vec<String>, rows: &mut usize| match msg {
            Msg::Row(row) => {
                lines.push(row.line());
                *rows += 1;
            }
            Msg::Flush(done) => flushes.push(done),
        };
        take(first, &mut lines, &mut rows);
        while rows < BATCH {
            match rx.recv_timeout(Duration::ZERO) {
                Ok(msg) => take(msg, &mut lines, &mut rows),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
        let said = lost.saturating_sub(told);
        if !lines.is_empty() || said > 0 {
            match log.append_saying(said, &lines) {
                Ok(_) => {
                    told = lost;
                    counts.written.fetch_add(rows as u64, Ordering::Relaxed);
                }
                Err((e, gap_written)) => {
                    if gap_written {
                        told = lost;
                    }
                    // A write whose cut back failed may have left rows of the batch whole: the
                    // log counts them from its length after the write, those are written and
                    // only the rest lost; where it cannot say, neither (missing at worst, never
                    // said lost while the log holds it).
                    let (whole, lost_rows) = if crate::log::left_lines(&e) {
                        crate::log::whole_lines(&e)
                            .map_or((0, 0), |whole| (whole, rows.saturating_sub(whole)))
                    } else {
                        (0, rows)
                    };
                    counts.written.fetch_add(whole as u64, Ordering::Relaxed);
                    counts
                        .dropped
                        .fetch_add(lost_rows as u64, Ordering::Relaxed);
                }
            }
        }
        for done in flushes.drain(..) {
            let _ = done.send(());
        }
    }
}

static GLOBAL: OnceLock<Sink> = OnceLock::new();

/// Make `sink` this process's writer ([`emit`]); the first call wins, and a later one's sink is
/// handed back.
///
/// # Errors
/// A writer is already installed: `sink` is returned unused.
pub fn install(sink: Sink) -> Result<(), Sink> {
    GLOBAL.set(sink)
}

/// This process's writer, once [`install`]ed.
#[must_use]
pub fn global() -> Option<&'static Sink> {
    GLOBAL.get()
}

/// Hand `row` to this process's writer; nothing where none is installed.
pub fn emit(row: Row) {
    if let Some(sink) = global() {
        sink.emit(row);
    }
}

#[cfg(test)]
#[path = "sink_tests.rs"]
mod tests;
