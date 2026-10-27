// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WRITE BEHIND: a large file's bytes written at their offsets by a few threads of their own,
//! while the tar walk hashes them and goes on to the next entry. The walk still opens every
//! file, makes every directory and link, and folds every digest in archive order; only the
//! copy of a file's bytes into it moves off the walk's thread. A failed write is answered at
//! the walk's next hand-over, or at [`WriteBehind::flush`], which the walk calls before it
//! reports the tree laid and before it opens a path it laid once already.

use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

/// The bytes of one hand-over: a file's bytes go behind in pieces this large, and a file no
/// larger is written by the walk itself.
pub(crate) const CHUNK: usize = 1 << 20;

/// Bytes handed over and not yet written, at most (one piece more when a single piece is
/// larger than what is free).
const IN_FLIGHT: usize = 32 << 20;

/// Threads that write, at most; none off Unix, where the walk writes every file itself.
const WRITERS: usize = if cfg!(unix) { 4 } else { 0 };

/// Write all of `bytes` into `file` at `offset`, leaving its cursor alone.
#[cfg(unix)]
fn write_at(file: &File, bytes: &[u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt as _;
    file.write_all_at(bytes, offset)
}

#[cfg(not(unix))]
fn write_at(_file: &File, _bytes: &[u8], _offset: u64) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// One piece of a file to write at `offset`.
struct Piece {
    file: Arc<File>,
    path: Arc<PathBuf>,
    offset: u64,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Piece>,
    /// Bytes handed over and not yet written.
    in_flight: usize,
    /// The first write that failed, and the file it was for.
    failed: Option<(io::Error, PathBuf)>,
    /// Buffers written and free to fill again.
    spare: Vec<Vec<u8>>,
    /// The walk is done: the writers stop.
    closed: bool,
    /// Tests: the writers write nothing until the walk flushes ([`HOLD_UNTIL_FLUSH`]).
    held: bool,
}

#[cfg(test)]
thread_local! {
    /// Tests: the writers of the next [`run`] on this thread hold every piece until the walk
    /// first flushes, so a test can tell a flush the walk makes from one it skips. Only for
    /// walks that hand over less than [`IN_FLIGHT`] bytes before that flush.
    pub(crate) static HOLD_UNTIL_FLUSH: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'g>(&self, guard: MutexGuard<'g, State>) -> MutexGuard<'g, State> {
        self.changed
            .wait(guard)
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// One writer: write each piece handed over until the walk is done.
    fn write(&self) {
        let mut state = self.lock();
        loop {
            if state.closed {
                return;
            }
            let next = if state.held {
                None
            } else {
                state.queue.pop_front()
            };
            let Some(piece) = next else {
                state = self.wait(state);
                continue;
            };
            drop(state);
            let wrote = write_at(&piece.file, &piece.bytes, piece.offset);
            let Piece {
                path, mut bytes, ..
            } = piece;
            state = self.lock();
            state.in_flight = state.in_flight.saturating_sub(bytes.len());
            if let Err(e) = wrote
                && state.failed.is_none()
            {
                state.failed = Some((e, PathBuf::clone(&path)));
            }
            bytes.clear();
            state.spare.push(bytes);
            self.changed.notify_all();
        }
    }
}

/// The writers of one extraction ([`run`]).
pub(crate) struct WriteBehind {
    shared: Arc<Shared>,
}

impl WriteBehind {
    /// A buffer to fill with the next piece: empty, with room for [`CHUNK`] bytes.
    pub(crate) fn buffer(&self) -> Vec<u8> {
        self.shared
            .lock()
            .spare
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(CHUNK))
    }

    /// Hand `bytes`, the piece of `file` (at `path`) at `offset`, to the writers; wait first
    /// while [`IN_FLIGHT`] bytes are already handed over.
    ///
    /// # Errors
    /// A write handed over earlier failed: its error and the file it was for.
    pub(crate) fn hand(
        &self,
        file: &Arc<File>,
        path: &Arc<PathBuf>,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<(), (io::Error, PathBuf)> {
        let mut state = self.shared.lock();
        loop {
            if let Some(failed) = state.failed.take() {
                return Err(failed);
            }
            if state.in_flight == 0 || state.in_flight.saturating_add(bytes.len()) <= IN_FLIGHT {
                break;
            }
            state = self.shared.wait(state);
        }
        state.in_flight = state.in_flight.saturating_add(bytes.len());
        state.queue.push_back(Piece {
            file: Arc::clone(file),
            path: Arc::clone(path),
            offset,
            bytes,
        });
        self.shared.changed.notify_all();
        Ok(())
    }

    /// Wait until every piece handed over is written.
    ///
    /// # Errors
    /// A write failed: its error and the file it was for.
    pub(crate) fn flush(&self) -> Result<(), (io::Error, PathBuf)> {
        let mut state = self.shared.lock();
        state.held = false;
        self.shared.changed.notify_all();
        loop {
            if let Some(failed) = state.failed.take() {
                return Err(failed);
            }
            if state.in_flight == 0 {
                return Ok(());
            }
            state = self.shared.wait(state);
        }
    }
}

/// Run `walk` with writers behind it ([`WriteBehind`]), or with none (`None`) where no
/// writer thread can be spawned. The writers stop once `walk` returns: a piece still queued
/// then is dropped unwritten, so a walk that succeeds flushes first.
pub(crate) fn run<T>(walk: impl FnOnce(Option<&WriteBehind>) -> T) -> T {
    let shared = Arc::new(Shared::default());
    #[cfg(test)]
    {
        shared.lock().held = HOLD_UNTIL_FLUSH.with(std::cell::Cell::take);
    }
    let writers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(WRITERS);
    std::thread::scope(|scope| {
        let mut spawned = 0usize;
        for _ in 0..writers {
            let mine = Arc::clone(&shared);
            if std::thread::Builder::new()
                .name("atpkg-write-behind".into())
                .spawn_scoped(scope, move || mine.write())
                .is_ok()
            {
                spawned = spawned.saturating_add(1);
            }
        }
        let behind = WriteBehind {
            shared: Arc::clone(&shared),
        };
        /// Stops the writers however the walk ends.
        struct Close<'s>(&'s Shared);
        impl Drop for Close<'_> {
            fn drop(&mut self) {
                self.0.lock().closed = true;
                self.0.changed.notify_all();
            }
        }
        let _close = Close(&shared);
        walk((spawned > 0).then_some(&behind))
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    #[test]
    fn pieces_handed_in_any_order_land_at_their_offsets_once_flushed() {
        let dir = crate::testdir::dir("atpkg-behind-offsets-");
        let path = Arc::new(dir.join("big"));
        let body = pattern(5 * CHUNK + 123, 7);
        let file = Arc::new(File::create(&*path).unwrap());
        run(|behind| {
            let behind = behind.expect("writers start");
            // Last piece first: each lands at its own offset whatever the order.
            let pieces: Vec<(usize, &[u8])> = body.chunks(CHUNK).enumerate().collect();
            for (i, piece) in pieces.iter().rev() {
                let mut buf = behind.buffer();
                buf.extend_from_slice(piece);
                behind.hand(&file, &path, (i * CHUNK) as u64, buf).unwrap();
            }
            behind.flush().unwrap();
        });
        drop(file);
        assert_eq!(std::fs::read(&*path).unwrap(), body);
    }

    #[test]
    fn a_failed_write_is_answered_at_the_next_hand_over_or_flush() {
        let dir = crate::testdir::dir("atpkg-behind-failed-");
        let path = Arc::new(dir.join("read-only"));
        std::fs::write(&*path, b"x").unwrap();
        // Opened for reading only: every positioned write to it fails.
        let file = Arc::new(File::open(&*path).unwrap());
        let (first, second) = run(|behind| {
            let behind = behind.expect("writers start");
            let mut buf = behind.buffer();
            buf.extend_from_slice(b"bytes");
            behind.hand(&file, &path, 0, buf).unwrap();
            let flushed = behind.flush().map_err(|(_, at)| at);
            // The failure is answered once; what follows it is judged on its own.
            (flushed, behind.flush().is_ok())
        });
        assert_eq!(
            first,
            Err(PathBuf::clone(&path)),
            "the flush names the file"
        );
        assert!(second, "a failure is reported once");
        assert_eq!(std::fs::read(&*path).unwrap(), b"x", "nothing was written");
    }

    #[test]
    fn a_walk_that_returns_early_stops_the_writers() {
        // The scope joins every writer: a writer that did not stop would hang this test.
        let got = run(|behind| behind.is_some());
        assert!(got);
    }

    #[test]
    fn hand_over_waits_while_the_writers_are_behind_by_the_bound() {
        let dir = crate::testdir::dir("atpkg-behind-bound-");
        let path = Arc::new(dir.join("bounded"));
        let file = Arc::new(File::create(&*path).unwrap());
        let body = pattern(IN_FLIGHT * 2 + CHUNK, 3);
        run(|behind| {
            let behind = behind.expect("writers start");
            for (i, piece) in body.chunks(CHUNK).enumerate() {
                let mut buf = behind.buffer();
                buf.extend_from_slice(piece);
                behind.hand(&file, &path, (i * CHUNK) as u64, buf).unwrap();
                assert!(
                    behind.shared.lock().in_flight <= IN_FLIGHT,
                    "never more than the bound in flight"
                );
            }
            behind.flush().unwrap();
        });
        drop(file);
        assert_eq!(std::fs::read(&*path).unwrap(), body);
    }
}
