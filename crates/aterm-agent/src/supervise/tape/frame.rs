// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tape frame encoding, position-bound checksums, and [`TapeWriter`].

use std::io::Write;

use super::codec::{Encoder, Item, Mark, len64};

/// Eight-byte tape file magic (`ATAPE\0`, [`FRAME_FORMAT`], `'\n'`).
pub const MAGIC: [u8; 8] = [b'A', b'T', b'A', b'P', b'E', 0, FRAME_FORMAT, b'\n'];

/// Frame format byte written by this build.
pub const FRAME_FORMAT: u8 = Format::Two as u8;

/// A frame format this build reads: the magic's seventh byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    /// T3's: each frame's sum is its xxh64 alone, so it names no place.
    One = 1,
    /// Each frame's sum also names its place ([module docs](self)).
    Two = 2,
}

impl Format {
    /// The format whose magic `bytes` start with.
    pub(crate) fn of_magic(bytes: &[u8]) -> Option<Self> {
        [Self::Two, Self::One]
            .into_iter()
            .find(|f| bytes.starts_with(&f.magic()))
    }

    /// The magic of a tape of this format.
    pub(crate) const fn magic(self) -> [u8; 8] {
        let mut magic = MAGIC;
        magic[6] = self as u8;
        magic
    }
}

/// A frame's head: kind, raw length, payload length, checksum.
pub(crate) const HEAD: usize = 1 + 4 + 4 + 8;

/// A frame closes once its raw items reach this.
pub const FRAME_RAW: usize = 64 * 1024;

/// A run's tape is at most this many bytes, its header's included.
pub const RUN_CAP: u64 = 64 * 1024 * 1024;

/// Maximum raw byte length of a single frame.
pub(crate) const FRAME_RAW_MAX: usize = 64 * 1024 * 1024;

/// Room kept below the cap for the truncation mark's own frame.
const MARK_ROOM: u64 = 64;

/// What a frame holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The tape's header: its text.
    Header = 0,
    /// Codec items.
    Records = 1,
}

impl Kind {
    pub(crate) fn of(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Header),
            1 => Some(Self::Records),
            _ => None,
        }
    }
}

/// Compute a frame's xxh64 checksum over its kind, lengths, and stored payload.
pub(crate) fn checksum(kind: Kind, lengths: &[u8], payload: &[u8]) -> u64 {
    let mut sum = aterm_zstd::xxh64::Xxh64::new(kind as u64);
    sum.update(lengths);
    sum.update(payload);
    sum.digest()
}

/// Upper bound on frame count in a writer's tape (`RUN_CAP / HEAD`).
pub(crate) const PLACES: u64 = RUN_CAP / HEAD as u64;

/// What every place is mixed with before it is folded into a sum: the
const PLACE_SALT: u64 = u64::from_le_bytes(Format::Two.magic());

/// MurmurHash3's 64-bit finalizer's two multipliers, and their inverses
const FMIX_1: u64 = 0xff51_afd7_ed55_8ccd;
const FMIX_2: u64 = 0xc4ce_b9fe_1a85_ec53;
const FMIX_1_INV: u64 = inverse(FMIX_1);
const FMIX_2_INV: u64 = inverse(FMIX_2);

/// The inverse of an odd `k` modulo 2^64, by Newton's iteration: each step
const fn inverse(k: u64) -> u64 {
    let mut x = k;
    let mut step = 0;
    while step < 5 {
        x = x.wrapping_mul(2u64.wrapping_sub(k.wrapping_mul(x)));
        step += 1;
    }
    x
}

/// MurmurHash3-mixed word encoding frame position `n` in format-2 checksums.
pub(crate) fn place_word(n: u64) -> u64 {
    let mut z = n ^ PLACE_SALT;
    z = (z ^ (z >> 33)).wrapping_mul(FMIX_1);
    z = (z ^ (z >> 33)).wrapping_mul(FMIX_2);
    z ^ (z >> 33)
}

/// Recover the frame position encoded in format-2 `sum` given payload hash `hashed`.
pub(crate) fn place_named(sum: u64, hashed: u64) -> Option<u64> {
    let mut z = sum ^ hashed;
    z = (z ^ (z >> 33)).wrapping_mul(FMIX_2_INV);
    z = (z ^ (z >> 33)).wrapping_mul(FMIX_1_INV);
    let n = (z ^ (z >> 33)) ^ PLACE_SALT;
    (n < PLACES).then_some(n)
}

/// Encode one format-2 frame at `place` (LZ4-compressed when that saves at least 1/8).
pub(crate) fn frame_bytes(kind: Kind, place: u64, raw: &[u8]) -> Vec<u8> {
    framed(Format::Two, kind, place, raw)
}

/// Encode one frame in `format` at `place`.
pub(crate) fn framed(format: Format, kind: Kind, place: u64, raw: &[u8]) -> Vec<u8> {
    let packed = aterm_lz4::compress(raw);
    let saves = packed.len() <= raw.len() - raw.len() / 8 && packed.len() < raw.len();
    let payload: &[u8] = if saves { &packed } else { raw };
    let comp_len = if saves { payload.len() } else { 0 };
    let mut lengths = [0u8; 8];
    lengths[..4].copy_from_slice(&u32_of(raw.len()).to_le_bytes());
    lengths[4..].copy_from_slice(&u32_of(comp_len).to_le_bytes());
    let sum = checksum(kind, &lengths, payload);
    let sum = match format {
        Format::One => sum,
        Format::Two => sum ^ place_word(place),
    };
    let mut out = Vec::with_capacity(HEAD + payload.len());
    out.push(kind as u8);
    out.extend_from_slice(&lengths);
    out.extend_from_slice(&sum.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// A length as a frame's head holds it. Every frame's raw is at most
fn u32_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
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

/// A tape being written ([module docs](self)).
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
    /// Frames handed to the sink so far: the next frame's place.
    places: u64,
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
            .field("places", &self.places)
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
            places: 0,
            cap: RUN_CAP,
            stopped: None,
        };
        let mut first = MAGIC.to_vec();
        first.extend(frame_bytes(Kind::Header, 0, header.as_bytes()));
        w.put(&first);
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
    pub fn stopped(&self) -> Option<Stopped> {
        self.stopped
    }

    /// Bytes handed to the sink so far.
    #[must_use]
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Write the open frame, if it holds anything — or, past the cap, the
    fn close(&mut self) {
        if self.open.is_empty() || self.stopped.is_some() {
            return;
        }
        let raw = std::mem::take(&mut self.open);
        let from = std::mem::replace(&mut self.open_from, self.effects);
        let bytes = frame_bytes(Kind::Records, self.places, &raw);
        if self.written + len64(bytes.len()) + MARK_ROOM > self.cap {
            self.truncate(from);
            return;
        }
        self.put(&bytes);
    }

    /// The cap: write [`Mark::Truncated`] at `door`, the first effect not
    fn truncate(&mut self, door: u64) {
        self.open.clear();
        let mut raw = Vec::new();
        Encoder::default().item(&Item::Mark(Mark::Truncated { door }), &mut raw);
        self.put(&frame_bytes(Kind::Records, self.places, &raw));
        if self.stopped.is_none() {
            self.stopped = Some(Stopped::Capped);
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
                self.places += 1;
            }
            Err(e) => self.fail(&e, "cannot write to it"),
        }
    }

    /// A write that failed: said once, and the writer stops. A disk out of
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
    fn drop(&mut self) {
        self.close();
    }
}
