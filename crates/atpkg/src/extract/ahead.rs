// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! DECODE AHEAD: an archive's decompression on a thread of its own, a few chunks ahead of the
//! tar walk that vets, hashes and writes what it yields.

use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};

/// The most bytes in one chunk the decoder thread hands the walk: past a zstd block's most
/// (128 KiB), so a chunk is usually one block.
const CHUNK: usize = 256 << 10;

/// Chunks decoded and not yet walked, at most: 4 MiB of decoded bytes.
const DEPTH: usize = 16;

/// One message from the decoder thread: a buffer and how many of its bytes it filled (none at
/// the end of the stream), or the error the stream ended with.
type Chunk = io::Result<(Vec<u8>, usize)>;

/// Run `walk` over the bytes `decoder` yields, `decoder` read on a thread of its own up to
/// [`DEPTH`] chunks ahead of it, so decompression overlaps the walk's hashing and writes. The
/// walk reads exactly the bytes, and then the error, that reading `decoder` itself would give
/// it. `decoder` comes back once `walk` has returned and the thread has stopped: at the end of
/// its stream, at its first error, or as soon as the walk stops reading. Where no thread can be
/// spawned, `walk` reads `decoder` itself.
///
/// # Panics
/// Re-raises a panic of the decoder thread.
pub(crate) fn decoded_ahead<D: Read + Send, T>(
    mut decoder: D,
    walk: impl FnOnce(&mut dyn Read) -> T,
) -> (T, D) {
    let (give_tx, give_rx) = mpsc::sync_channel::<D>(1);
    let (full_tx, full_rx) = mpsc::sync_channel::<Chunk>(DEPTH);
    let (spare_tx, spare_rx) = mpsc::channel::<Vec<u8>>();
    std::thread::scope(|scope| {
        let spawned = std::thread::Builder::new()
            .name("atpkg-decode".into())
            .spawn_scoped(scope, move || {
                let mut decoder = give_rx.recv().ok()?;
                pump(&mut decoder, &full_tx, &spare_rx);
                Some(decoder)
            });
        let Ok(decoding) = spawned else {
            return (walk(&mut decoder), decoder);
        };
        if let Err(mpsc::SendError(back)) = give_tx.send(decoder) {
            decoder = back;
            return (walk(&mut decoder), decoder);
        }
        let mut ahead = Ahead {
            full: full_rx,
            spare: spare_tx,
            chunk: Vec::new(),
            filled: 0,
            pos: 0,
            ended: false,
        };
        let walked = walk(&mut ahead);
        drop(ahead);
        match decoding.join() {
            Ok(Some(decoder)) => (walked, decoder),
            Ok(None) => unreachable!("the decoder thread received the decoder it was sent"),
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}

/// The decoder thread: read chunk after chunk from `decoder` and hand each to the walk, until
/// the stream ends or fails or the walk is gone.
fn pump(decoder: &mut impl Read, full: &SyncSender<Chunk>, spare: &Receiver<Vec<u8>>) {
    loop {
        let mut buf = spare.try_recv().unwrap_or_else(|_| vec![0u8; CHUNK]);
        let (filled, failed) = read_once(decoder, &mut buf);
        if filled > 0 && full.send(Ok((buf, filled))).is_err() {
            return;
        }
        if let Some(e) = failed {
            let _ = full.send(Err(e));
            return;
        }
        if filled == 0 {
            let _ = full.send(Ok((Vec::new(), 0)));
            return;
        }
    }
}

/// One read of `decoder` into `buf` (none at the end of the stream), or the failure. What one
/// read yields goes to the walk at once, never held for a next read that may wait on bytes
/// still arriving ([`crate::flow::arriving`]).
fn read_once(decoder: &mut impl Read, buf: &mut [u8]) -> (usize, Option<io::Error>) {
    loop {
        match decoder.read(buf) {
            Ok(n) => return (n.min(buf.len()), None),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return (0, Some(e)),
        }
    }
}

/// The walk's side of [`decoded_ahead`]: the chunks the decoder thread filled, in order.
struct Ahead {
    full: Receiver<Chunk>,
    /// Where a walked chunk's buffer goes back for the decoder thread to fill again.
    spare: Sender<Vec<u8>>,
    chunk: Vec<u8>,
    filled: usize,
    pos: usize,
    /// The decoder thread said the stream ended.
    ended: bool,
}

impl Read for Ahead {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            if let Some(pending) = self.chunk.get(self.pos..self.filled)
                && !pending.is_empty()
            {
                let n = pending.len().min(out.len());
                if let (Some(dst), Some(src)) = (out.get_mut(..n), pending.get(..n)) {
                    dst.copy_from_slice(src);
                }
                self.pos = self.pos.saturating_add(n);
                return Ok(n);
            }
            if self.ended {
                return Ok(0);
            }
            match self.full.recv() {
                Ok(Ok((buf, filled))) => {
                    let walked = std::mem::replace(&mut self.chunk, buf);
                    if walked.capacity() > 0 {
                        let _ = self.spare.send(walked);
                    }
                    self.filled = filled;
                    self.pos = 0;
                    self.ended = filled == 0;
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    return Err(io::Error::other(
                        "the archive's decoder stopped before the end of its stream",
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "ahead_tests.rs"]
mod tests;
