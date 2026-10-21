// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A TAPE'S FRAMES, AND THE WRITER THAT CLOSES THEM (the replay plan's §6.4).
//!
//! ```text
//! file  := "ATAPE\0" format:u8 "\n" frame*        format 2 written; 1 still read
//! frame := kind:u8  raw_len:u32le  comp_len:u32le  sum:u64le  payload
//!          sum = xxh64, seeded with kind, of raw_len, comp_len and payload,
//!                XOR place(n), n the frame's place (the header's is 0)
//!                (format 1: no place, the xxh64 alone)
//!          kind 0 = the header (its text), 1 = records (codec items)
//!          comp_len 0 = the payload is raw; else an aterm_lz4 block of it
//! ```
//!
//! The magic's seventh byte is the frame format's number; the header's own
//! text names its grammar's (`atape <n>`). The first frame is the header
//! ([`super::header`]), every later one records. A frame's payload is LZ4
//! only when that saves an eighth of it or more.
//!
//! EACH FRAME IS TIED TO ITS PLACE (format 2). Format 1 seeded a frame's sum
//! with its kind alone, so nothing tied a frame to where it stood: a whole
//! frame repeated, dropped or moved read back `Clean` (T3's review measured it
//! on a four-frame tape: `[1,2,2,3,4]`, `[1,3,4]` and `[1,3,2,4]` all clean).
//! And what such a read returns is neither what was written nor the moved
//! frames' own items: the codec's state crosses frames ([`super::codec`]), so
//! a reply stored against the previous one under its key decodes against the
//! wrong base, and the read hands back replies no run ever saw. Format 2 folds
//! the frame's place into its sum, at no byte: `place(n)` is MurmurHash3's
//! 64-bit finalizer of `n` XOR the format-2 magic's own word, a bijection
//! whose inverse is as cheap ([`place_named`]). So a reader takes the place a
//! whole frame names off its own bytes — `xxh64 XOR sum`, unmixed — and
//! judges it:
//!
//! * its own place: the frame is whole and where it was written;
//! * another place a writer's tape can hold (below [`PLACES`]): a whole frame
//!   out of place, which no crash writes, so a fault wherever it stands —
//!   astream-wire's rule for its dense step records
//!   (`astream_wire::steps::StepFault::OutOfOrder`), the frame repeated,
//!   dropped or moved named by the place it was written at;
//! * anything else: a damaged frame, as before. A damaged bit anywhere in the
//!   frame scatters the place it names over all 2^64 words, so damage reads
//!   as a real place about once in 2^42 (`PLACES` < 2^22).
//!
//! WHY THE PLACE, NOT A SEED OR A CHAIN. The search after a short or damaged
//! frame ([`super::recover`]) must know a whole frame when it meets one
//! without knowing its place: the damage hides how many frames stood before
//! it. A place folded in by a bijection is read off the frame alone; a seed of
//! the place, or a chain through the previous frame's sum, can only be checked
//! against a place or a sum the damage may have taken, and a reader that
//! cannot check would call a fault a torn tail and drop whole frames.
//!
//! WHAT THE PLACE CANNOT TELL. A tape whose last frames are missing is a
//! prefix of whole frames in place: what a writer that dies between two
//! frames leaves, so no frame says it is not the last. The run's end item
//! ([`super::codec::End`]) says it: a tape whose items stop without one is a
//! run that stopped there, which a replay reports (ENDED, `no end on the
//! tape`). And a frame of ANOTHER tape at its own place is in place: the
//! place is tied, the tape is not — a tie to the tape would be checked
//! against its header, and damage to the header would then hide every whole
//! frame after it from the search. Neither a crash nor a disk splices two
//! files' frames at the same place.
//!
//! WHY NOT ASTREAM'S STEP RECORDS. aterm builds on astream, and
//! `astream_wire::steps` is this very law — dense numbered records, a gap, a
//! repeat or a reordering a fault, a torn tail told from damage by the
//! store's rule — and aterm-restore keeps its region in them. The tape keeps
//! its own frame, concretely:
//!
//! * format 1 tapes are read exactly as before, and the place rides in the
//!   sum, so both formats share one grammar and one recovery, where step
//!   records would be a second grammar beside the first;
//! * an astream frame carries at most 16 MiB, under this frame's
//!   [`FRAME_RAW_MAX`].
//!
//! Nor is the search for a whole frame after a broken one: astream's
//! (`steps::frame_decodes_after`) once checked the CRC of every candidate with
//! no budget, the quadratic this reader also had until T3's review measured it
//! (632 ms a MiB), and both are now bounded the same way, a budget per byte
//! after the break with a floor, its exhaustion a refusal or a fault and never
//! a torn tail (this reader's in [`super::recover`]; astream's since R12,
//! astream 31eb80e and d12dc08, `docs/DESIGN-crash-restore-2026-10-20.md` §3).
//!
//! Size is not a reason: a step record's 25 bytes of head and the tape's own
//! kind and lengths beside them (34 bytes, where this head is [`HEAD`]'s 17)
//! would add 17 bytes a frame, under 2% of the kilobyte frames `tape stats`
//! measured ([`super`]: 816 bytes on its 48 frames' 48,088, 1.7%).
//!
//! THE CHECKSUM COVERS THE FRAME AS STORED: an xxh64 seeded with the kind, of
//! the two lengths and the payload, so every byte of a frame but the checksum
//! itself is under it, and a damaged checksum fails it too (it names no
//! place). The plan wrote `xxh64(raw)`, the bytes before compression; what
//! changed, and why: a damaged byte of an LZ4 payload can decode to the very
//! same raw bytes (a match offset moved onto an identical earlier run — a
//! screen's rows repeat one another), and a checksum of the raw bytes then
//! passes damage the disk did. Measured on a three-frame test tape: seven of
//! its single-byte flips did exactly that. The raw bytes are still held to
//! account — they must come back to `raw_len`, and every item in them must
//! decode — and a damaged frame is now refused before anything is
//! decompressed.
//!
//! THE WRITER ([`TapeWriter`]) appends items to an open frame and closes it —
//! one `write(2)` of the whole frame, so a crash leaves at most one frame
//! short — at each act ([`Hint::Act`]), before each wait ([`Hint::BeforeWait`]:
//! an idle loop's tape is on disk while it waits), at [`FRAME_RAW`] raw bytes,
//! and at each [`Mark`] and the [`End`]. It flushes only after the end, and only
//! to keep the tape ahead of what follows on the device (`aterm_durable::ordered`,
//! not a full flush): a tape is a diagnostic record, with the journal's posture,
//! and a crash's torn tail is what recovery is for ([`super::recover`]). The
//! first write is the magic and the header frame together, so a tape that
//! exists at all names its format.
//!
//! The writer never fails its caller. Recording must not change the run it
//! records, so every error — a full disk, a revoked directory — is said once,
//! to the writer's warning sink, as the journal says its own, and the writer
//! stops: [`TapeWriter::failed`]. The tape on disk is then a prefix of whole
//! frames (a write that failed partway is the torn tail of the next read).
//! A run that reaches [`RUN_CAP`] bytes writes [`Mark::Truncated`], naming
//! the first effect it did not keep, and stops: the prefix still replays.
//! Every frame the writer hands its sink takes the next place, the header's 0.

use std::io::Write;

use super::codec::{Encoder, Item, Mark, len64};

/// The file's first eight bytes: `ATAPE`, a zero, the frame format this build
/// writes ([`FRAME_FORMAT`]), a newline.
pub const MAGIC: [u8; 8] = [b'A', b'T', b'A', b'P', b'E', 0, FRAME_FORMAT, b'\n'];

/// The frame format this build writes (the magic's seventh byte). It reads
/// this one and format 1, whose sums name no place.
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

/// No frame's raw bytes are longer: an item that would make one longer is the
/// cap ([`Mark::Truncated`]). The reader refuses a longer one unread, so a
/// damaged length cannot make it allocate more.
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

/// A frame's checksum: seeded with its kind, over its head's two lengths
/// (`lengths`, as stored) and its payload as stored ([module docs](self)).
pub(crate) fn checksum(kind: Kind, lengths: &[u8], payload: &[u8]) -> u64 {
    let mut sum = aterm_zstd::xxh64::Xxh64::new(kind as u64);
    sum.update(lengths);
    sum.update(payload);
    sum.digest()
}

/// The most frames a writer's tape holds: each is at least [`HEAD`] bytes,
/// and a run's tape at most [`RUN_CAP`]. A format-2 sum that names a place at
/// or past it names none: its frame is damaged ([module docs](self)).
pub(crate) const PLACES: u64 = RUN_CAP / HEAD as u64;

/// What every place is mixed with before it is folded into a sum: the
/// format-2 magic's own word, so no place's word is 0 (the word a format-1
/// sum would carry). Format 2's, not [`MAGIC`] (the format this build
/// writes): a later format leaves how a format-2 tape names its places as it
/// is, and `a_format_2_tape_reads_as_it_always_did` holds it to bytes the
/// format-2 writer wrote.
const PLACE_SALT: u64 = u64::from_le_bytes(Format::Two.magic());

/// MurmurHash3's 64-bit finalizer's two multipliers, and their inverses
/// modulo 2^64.
const FMIX_1: u64 = 0xff51_afd7_ed55_8ccd;
const FMIX_2: u64 = 0xc4ce_b9fe_1a85_ec53;
const FMIX_1_INV: u64 = inverse(FMIX_1);
const FMIX_2_INV: u64 = inverse(FMIX_2);

/// The inverse of an odd `k` modulo 2^64, by Newton's iteration: each step
/// doubles the low bits that are right, from the three `k` itself gets right
/// (an odd square is 1 modulo 8), so five steps make 96 of them.
const fn inverse(k: u64) -> u64 {
    let mut x = k;
    let mut step = 0;
    while step < 5 {
        x = x.wrapping_mul(2u64.wrapping_sub(k.wrapping_mul(x)));
        step += 1;
    }
    x
}

/// The word a format-2 frame's sum carries for its place `n`: MurmurHash3's
/// 64-bit finalizer of `n` XOR [`PLACE_SALT`], a bijection of `u64` whose every
/// output bit hangs on every input bit, and [`place_named`] undoes it.
pub(crate) fn place_word(n: u64) -> u64 {
    let mut z = n ^ PLACE_SALT;
    z = (z ^ (z >> 33)).wrapping_mul(FMIX_1);
    z = (z ^ (z >> 33)).wrapping_mul(FMIX_2);
    z ^ (z >> 33)
}

/// The place a format-2 frame names: its stored `sum` with the [`checksum`]
/// its bytes hash to taken out, and [`place_word`] undone (a shift by 33 of 64
/// bits XORed in is its own inverse). `None` when that is no place a writer's
/// tape holds ([`PLACES`]): the frame is damaged.
pub(crate) fn place_named(sum: u64, hashed: u64) -> Option<u64> {
    let mut z = sum ^ hashed;
    z = (z ^ (z >> 33)).wrapping_mul(FMIX_2_INV);
    z = (z ^ (z >> 33)).wrapping_mul(FMIX_1_INV);
    let n = (z ^ (z >> 33)) ^ PLACE_SALT;
    (n < PLACES).then_some(n)
}

/// One frame's bytes, head and payload, at `place` on a tape this build
/// writes: LZ4 when that saves an eighth.
pub(crate) fn frame_bytes(kind: Kind, place: u64, raw: &[u8]) -> Vec<u8> {
    framed(Format::Two, kind, place, raw)
}

/// One frame's bytes in `format`: [`frame_bytes`], or (a test's) the frame a
/// format-1 writer wrote, whose sum names no place.
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
/// [`FRAME_RAW_MAX`], which a `u32` holds.
fn u32_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Where a tape's frames go: a file, or (a test's) memory.
pub trait FrameSink: Send {
    /// Write one whole frame: one `write(2)` for a file.
    ///
    /// # Errors
    ///
    /// The write's.
    fn write_frame(&mut self, bytes: &[u8]) -> std::io::Result<()>;

    /// Order everything written ahead of every later write on the device
    /// (`aterm_durable::ordered` for a file). Called once, after the end.
    ///
    /// # Errors
    ///
    /// The sync's.
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
    /// The loop acted (a key, a send, a host step): what it acted on is on
    /// disk before anything that follows.
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
    /// A tape named `name` (said in its one warning) on `sink`, its header
    /// `header` written at once with the magic, in one write. A failure is
    /// said to `warn` and the writer stops.
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

    /// The same writer with the run's cap at `cap` bytes, for a test that
    /// reaches it.
    #[cfg(test)]
    pub(crate) fn capped_at(mut self, cap: u64) -> Self {
        self.cap = cap;
        self
    }

    /// Append `item`. A mark or the end closes its frame (the end then orders
    /// the tape ahead of what follows); so does reaching [`FRAME_RAW`]. Nothing is written
    /// once the writer stopped.
    pub fn append(&mut self, item: &Item) {
        let end = matches!(item, Item::End(_));
        if self.stopped.is_none() {
            self.take(item);
        }
        // The end orders whatever the tape holds ahead of what follows — a
        // capped one's prefix and its mark included.
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
    /// truncation mark in its place.
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
    /// kept, alone in its frame, and stop. The mark refers to no name and no
    /// body, so the reader decodes it whatever the dropped items taught the
    /// encoder.
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
