// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tape frames and [`TapeWriter`]: after the tape's magic, each frame is one
//! `astream_wire::steps` record whose step is its place, the header's step 0
//! (astream claim `wire.steps.torn-vs-fault`; [`super::recover`] reads them back
//! with astream's scan).

use std::io::Write;

use astream_wire::frame::{FrameError, HEADER_SIZE, MAX_PAYLOAD_LEN};
use astream_wire::steps::{STEP_HEADER, encode_step};

use super::codec::{Encoder, Item, Mark, len64};

/// Eight-byte tape file magic (`ATAPE\0`, [`FRAME_FORMAT`], `'\n'`).
pub const MAGIC: [u8; 8] = [b'A', b'T', b'A', b'P', b'E', 0, FRAME_FORMAT, b'\n'];

/// The one frame format this build writes and reads: 3, astream step records.
/// Formats 1 and 2 framed each frame in aterm's own head and xxh64; a tape of
/// either is refused by its number ([`super::recover::Why::Format`]).
pub const FRAME_FORMAT: u8 = 3;

/// A frame's body before its payload: its kind, and its raw length.
pub(crate) const BODY_HEAD: usize = 1 + 4;

/// The bytes a frame takes beyond its stored payload: the astream frame's head,
/// the step record's head, and the body's.
pub(crate) const OVERHEAD: usize = HEADER_SIZE + STEP_HEADER + BODY_HEAD;

/// A frame closes once its raw items reach this.
pub const FRAME_RAW: usize = 64 * 1024;

/// A run's tape is at most this many bytes, its header's included.
pub const RUN_CAP: u64 = 64 * 1024 * 1024;

/// Maximum raw byte length of a single frame: the largest body an astream step
/// record carries ([`MAX_PAYLOAD_LEN`] less the step's head) less the body's
/// head. A frame stores its raw bytes compressed only when that is shorter, so
/// its stored payload is never longer than its raw bytes and every frame of at
/// most this many raw bytes is a step record astream encodes. An item longer
/// than this ends the tape at a truncation mark ([`TapeWriter::append`]).
pub(crate) const FRAME_RAW_MAX: usize = MAX_PAYLOAD_LEN - STEP_HEADER - BODY_HEAD;

/// Room kept below the cap for the truncation mark's own frame: a frame's
/// overhead, and the mark's two bytes and its door, a varint of at most ten
/// (stored raw: twelve bytes compress by none).
const MARK_ROOM: u64 = (OVERHEAD + 2 + 10) as u64;

/// What a frame holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The tape's header: its text.
    Header = 0,
    /// Codec items.
    Records = 1,
}

impl Kind {
    pub(crate) const fn of(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Header),
            1 => Some(Self::Records),
            _ => None,
        }
    }
}

/// One frame of `kind` holding `raw`, as the step record at `step`: the body is
/// the kind, the raw length and the payload, LZ4-compressed when that saves at
/// least an eighth.
///
/// # Errors
/// `raw` is longer than [`FRAME_RAW_MAX`], so its body is past what a step
/// record carries ([`FrameError::TooLarge`]).
pub(crate) fn frame_bytes(kind: Kind, step: u64, raw: &[u8]) -> Result<Vec<u8>, FrameError> {
    let raw_len = u32::try_from(raw.len())
        .ok()
        .filter(|_| raw.len() <= FRAME_RAW_MAX)
        .ok_or(FrameError::TooLarge)?;
    let packed = aterm_lz4::compress(raw);
    let saves = packed.len() <= raw.len() - raw.len() / 8 && packed.len() < raw.len();
    let payload: &[u8] = if saves { &packed } else { raw };
    let mut body = Vec::with_capacity(BODY_HEAD + payload.len());
    body.push(kind as u8);
    body.extend_from_slice(&raw_len.to_le_bytes());
    body.extend_from_slice(payload);
    encode_step(step, &body)
}

/// Where a tape's frames go: a file, or (a test's) memory.
pub trait FrameSink: Send {
    /// Write one whole frame.
    ///
    /// # Errors
    /// The underlying write failed.
    fn write_frame(&mut self, bytes: &[u8]) -> std::io::Result<()>;

    /// Order everything written ahead of later writes on the device.
    ///
    /// # Errors
    /// The underlying sync failed.
    fn sync(&mut self) -> std::io::Result<()>;
}

impl FrameSink for std::fs::File {
    fn write_frame(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.write_all(bytes)
    }

    fn sync(&mut self) -> std::io::Result<()> {
        // Ordered: a diagnostic record, read back as a prefix of whole frames with a
        // torn tail reported (`tape::recover`); the barrier keeps it ahead of the
        // run's later journal and ledger writes, and nothing off this device needs it.
        aterm_durable::ordered(self)
    }
}

/// Why the caller closes the open frame early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    /// Loop acted (key, send, or host step).
    Act,
    /// The loop is about to wait: an idle loop's tape is on disk.
    BeforeWait,
}

/// Why the writer stopped writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// A write or the sync failed; it was said.
    Failed,
    /// The run reached [`RUN_CAP`]; [`Mark::Truncated`] says where.
    Capped,
    /// The end was written.
    Ended,
}

/// A tape being written.
pub struct TapeWriter {
    sink: Box<dyn FrameSink>,
    /// The tape's name in the one warning (its path).
    name: String,
    warn: Box<dyn Write + Send>,
    encoder: Encoder,
    /// The open frame's raw items.
    open: Vec<u8>,
    /// The ordinal of the first effect in the open frame.
    open_from: u64,
    /// Effects appended so far.
    effects: u64,
    /// Bytes handed to the sink so far.
    written: u64,
    /// Frames handed to the sink so far: the next frame's step.
    steps: u64,
    cap: u64,
    stopped: Option<Stopped>,
}

impl std::fmt::Debug for TapeWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TapeWriter")
            .field("name", &self.name)
            .field("open", &self.open.len())
            .field("effects", &self.effects)
            .field("written", &self.written)
            .field("steps", &self.steps)
            .field("stopped", &self.stopped)
            .finish_non_exhaustive()
    }
}

impl TapeWriter {
    /// Start a tape named `name` on `sink`, writing its magic and `header` frame.
    pub fn start(
        sink: Box<dyn FrameSink>,
        name: impl Into<String>,
        header: &str,
        warn: Box<dyn Write + Send>,
    ) -> Self {
        let mut w = Self {
            sink,
            name: name.into(),
            warn,
            encoder: Encoder::default(),
            open: Vec::new(),
            open_from: 0,
            effects: 0,
            written: 0,
            steps: 0,
            cap: RUN_CAP,
            stopped: None,
        };
        if let Some(frame) = w.frame(Kind::Header, header.as_bytes()) {
            w.put(&[MAGIC.as_slice(), &frame].concat());
        }
        w
    }

    /// Override the run's byte cap for testing.
    #[cfg(test)]
    pub(crate) fn capped_at(mut self, cap: u64) -> Self {
        self.cap = cap;
        self
    }

    /// Append `item`, closing the open frame on a mark, end, or [`FRAME_RAW`].
    pub fn append(&mut self, item: &Item) {
        let end = matches!(item, Item::End(_));
        if self.stopped.is_none() {
            self.take(item);
        }
        if end && matches!(self.stopped, None | Some(Stopped::Capped)) {
            match self.sink.sync() {
                Err(e) => self.fail(&e, "cannot flush it"),
                Ok(()) if self.stopped.is_none() => self.stopped = Some(Stopped::Ended),
                Ok(()) => {}
            }
        }
    }

    /// [`Self::append`]'s item into the open frame, closing it when it must.
    fn take(&mut self, item: &Item) {
        let mut bytes = Vec::new();
        self.encoder.item(item, &mut bytes);
        if self.open.len() + bytes.len() > FRAME_RAW_MAX {
            self.close();
            if self.stopped.is_none() && bytes.len() > FRAME_RAW_MAX {
                self.truncate(self.effects);
            }
        }
        if self.stopped.is_some() {
            return;
        }
        self.open.extend_from_slice(&bytes);
        if item.record().is_some() {
            self.effects += 1;
        }
        if item.closes_frame() || self.open.len() >= FRAME_RAW {
            self.close();
        }
    }

    /// Close the open frame now, for `why` ([`Hint`]).
    pub fn flush_hint(&mut self, why: Hint) {
        match why {
            Hint::Act | Hint::BeforeWait => self.close(),
        }
    }

    /// Whether a write failed (it was said, once).
    #[must_use]
    pub fn failed(&self) -> bool {
        self.stopped == Some(Stopped::Failed)
    }

    /// Why the writer stopped, if it did.
    #[must_use]
    pub const fn stopped(&self) -> Option<Stopped> {
        self.stopped
    }

    /// Bytes handed to the sink so far.
    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }

    /// Write the open frame, if it holds anything — or, past the cap, the
    /// truncation mark in its place.
    fn close(&mut self) {
        if self.open.is_empty() || self.stopped.is_some() {
            return;
        }
        let raw = std::mem::take(&mut self.open);
        let from = std::mem::replace(&mut self.open_from, self.effects);
        let Some(bytes) = self.frame(Kind::Records, &raw) else {
            return;
        };
        if self.written + len64(bytes.len()) + MARK_ROOM > self.cap {
            self.truncate(from);
            return;
        }
        self.put(&bytes);
    }

    /// The cap: write [`Mark::Truncated`] at `door`, the first effect not
    /// kept, alone in its frame, and stop.
    fn truncate(&mut self, door: u64) {
        self.open.clear();
        let mut raw = Vec::new();
        Encoder::default().item(&Item::Mark(Mark::Truncated { door }), &mut raw);
        if let Some(bytes) = self.frame(Kind::Records, &raw) {
            self.put(&bytes);
        }
        if self.stopped.is_none() {
            self.stopped = Some(Stopped::Capped);
        }
    }

    /// The frame of `kind` holding `raw` at the next step ([`frame_bytes`]); one
    /// astream refuses is a failure, said once.
    fn frame(&mut self, kind: Kind, raw: &[u8]) -> Option<Vec<u8>> {
        match frame_bytes(kind, self.steps, raw) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                let e = std::io::Error::new(std::io::ErrorKind::InvalidInput, e);
                self.fail(&e, "cannot frame it");
                None
            }
        }
    }

    /// Hand one whole frame to the sink.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseTapeRecovery",
            action = "Write",
            project = "aterm_agent::supervise::tape::conformance_tests::project"
        )
    )]
    fn put(&mut self, bytes: &[u8]) {
        match self.sink.write_frame(bytes) {
            Ok(()) => {
                self.written += len64(bytes.len());
                self.steps += 1;
            }
            Err(e) => self.fail(&e, "cannot write to it"),
        }
    }

    /// A write that failed: said once, and the writer stops. A disk out of
    /// space or quota is also a sighting the window's disk watch is asked to
    /// act on ([`crate::harness::disk::poke`]), as the journal's is.
    fn fail(&mut self, e: &std::io::Error, what: &str) {
        self.stopped = Some(Stopped::Failed);
        if crate::harness::disk::poke::is_out_of_space(e) {
            crate::harness::disk::poke::poke(crate::harness::disk::poke::Cause::Write(format!(
                "tape {}: {e}",
                self.name
            )));
        }
        let _ = writeln!(
            self.warn,
            "aterm drive: tape {}: {what}: {e} — the loop goes on without it",
            self.name
        );
        let _ = self.warn.flush();
    }
}

impl Drop for TapeWriter {
    /// What was appended reaches the file even when its owner never wrote
    /// the end: the tape then ends without an `End`, which a replay reads as
    /// a run that stopped there.
    fn drop(&mut self) {
        self.close();
    }
}
