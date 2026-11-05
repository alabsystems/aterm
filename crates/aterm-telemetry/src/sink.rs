// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WRITER: rows go to a thread of their own through a bounded queue, so no session waits
//! on the disk. [`Sink::emit`] never blocks: a row that finds the queue full is counted, and the
//! writer says how many it lost in a `kind=gap` line before its next batch.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::log::Log;
use crate::row::{Row, gap_line};

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
    /// Rows written.
    pub written: AtomicU64,
    /// Rows lost: the queue was full, or their append failed.
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
    while let Ok(first) = rx.recv() {
        let mut batch = Vec::new();
        let mut take = |msg: Msg, batch: &mut Vec<Row>| match msg {
            Msg::Row(row) => batch.push(*row),
            Msg::Flush(done) => flushes.push(done),
        };
        take(first, &mut batch);
        while batch.len() < BATCH {
            match rx.recv_timeout(Duration::ZERO) {
                Ok(msg) => take(msg, &mut batch),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
        let mut lines = Vec::with_capacity(batch.len() + 1);
        let lost = counts.dropped.load(Ordering::Relaxed);
        if lost > told {
            lines.push(gap_line(crate::now_ms(), lost - told));
        }
        lines.extend(batch.iter().map(Row::line));
        if !lines.is_empty() {
            match log.append(&lines) {
                Ok(_) => {
                    told = lost;
                    counts
                        .written
                        .fetch_add(batch.len() as u64, Ordering::Relaxed);
                }
                Err(_) => {
                    counts
                        .dropped
                        .fetch_add(batch.len() as u64, Ordering::Relaxed);
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
