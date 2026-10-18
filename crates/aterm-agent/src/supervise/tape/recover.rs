// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! READING A TAPE BACK: the longest prefix of whole frames, and what ended it
//! (the replay plan's §6.4), as a pure function of the bytes.
//!
//! A tape's writer closes each frame with one `write(2)` and fsyncs only after
//! the run's end, so what a reader meets is a run of whole frames, then
//! possibly one that is not: half-written when the process died inside its
//! write, or never flushed past a kernel crash. And a disk can damage a byte
//! of a frame written long ago. The classification is astream-engine's store's
//! (`vendor/astream/crates/astream-engine/src/store.rs`):
//!
//! * a frame that is short, or fails its checksum, with NO whole frame
//!   anywhere after it, is the TORN tail a crash leaves: dropped, and said —
//!   [`Tail::Torn`], at the offset where the whole prefix ends;
//! * one with a whole frame after it is a FAULT ([`Tail::Fault`]): no crash
//!   leaves a whole frame behind a broken one, so the damage is inside the
//!   tape, and dropping from there would throw away frames that were written
//!   whole. A plain read ([`recover`]) refuses the tape, naming the offset;
//!   [`salvage`] keeps the prefix before it, the explicit repair `tape cat
//!   --salvage` asks for;
//! * a frame that is whole but is not what its place holds — a first frame
//!   that is not the header, a header after the first, items this build
//!   cannot read — is a fault wherever it is: it is readable and wrong, which
//!   no crash makes.
//!
//! "Whole after it" is looked for byte by byte, as the store does: a damaged
//! head has no trustworthy length to skip by. A damaged LAST frame therefore
//! reads as the torn tail — no reader can tell it from one a crash cut short
//! (the store says the same of a rotted length on its last frame), and it is
//! reported torn, never passed over in silence (`SuperviseTapeRecovery`'s
//! `RotLast`).
//!
//! A tape ends cleanly only after its header: the writer's first write is the
//! magic and the header together, so bytes that stop at or inside it are a
//! first write that tore. An empty file is a tape whose first write never
//! happened: clean, and empty. Whole bytes that do not start with the magic
//! are refused whatever follows them, even a whole header: a damaged magic
//! cannot be told from a file that is no tape (`SuperviseTapeRecovery`'s
//! `RotMagic`).
//!
//! THE READER'S OWN BOUNDS. A checksum that passes says a frame holds the
//! bytes it was closed with, not that a tape's writer closed it: xxh64 is
//! unkeyed, so a tape made by hand passes it, and two shapes of such a tape
//! cost a plain read far more than their size. Neither is a shape the writer
//! leaves, and both are refused by name ([`Tail::Fault`]); `salvage` keeps
//! the whole frames before the one that met the bound, as for any fault.
//! * WHAT A READ HOLDS. A `Same` body is one byte that stands for a copy of a
//!   base of up to 64 KiB ([`super::delta`]), and a name spelled once is
//!   copied into every item that names it, so the decoded items can outgrow
//!   the file without limit: a forged 401-byte tape decoded to 268 MB
//!   (measured by the review of this reader), and one frame's 64 MiB of raw
//!   items could ask for a terabyte. A read counts what it holds — every item
//!   with what it owns, the interned names, the header — after each item, and
//!   the item that takes it past [`HELD_MAX`] refuses the tape
//!   ([`Why::TooLarge`]), so a read holds at most that and one item more (an
//!   item is bounded by its frame). A real run is well below it: the plan's
//!   §8 puts a busy hour at about 0.9 MB of items and 200 KB on disk, so a
//!   tape at its 64 MiB cap ([`super::frame::RUN_CAP`]), about 330 busy
//!   hours, holds about 300 MB.
//! * HOW FAR THE SEARCH GOES. "Whole after it" hashes each frame-like head it
//!   meets, payload and all, so bytes crowded with such heads made the search
//!   quadratic in the file (632 ms for 1 MiB, measured by the same review;
//!   tens of minutes at a run's 64 MiB). A crash's tail is one frame, and
//!   real data rarely holds a byte that reads as a records frame's head whose
//!   lengths fit, so its search stays short. The search hashes at most
//!   [`SEARCH_PER_BYTE`] times the bytes after the frame, and never less than
//!   [`SEARCH_FLOOR`]: linear in the file. Bytes that exhaust it are refused
//!   ([`Why::Unsearched`]) rather than guessed torn: a guess of torn could
//!   drop whole frames behind the look-alikes, which is the loss a fault
//!   exists to prevent.
//!
//! Both bounds are outside `SuperviseTapeRecovery`, which reads tapes a writer
//! left: no tape its Tier-1 bind reads comes near either.

use astream_effects::EffectRecord;

use super::codec::{CodecError, Decoder, End, Input, Item, Mark};
use super::frame::{FRAME_FORMAT, FRAME_RAW_MAX, HEAD, Kind, MAGIC, checksum};

/// The most a read holds in memory: every item and what it owns, the names it
/// interned and the header ([module docs](self)). A tape past it is refused.
pub const HELD_MAX: usize = 1 << 30;

/// The search for a whole frame after a short or bad one hashes at most this
/// many times the bytes after it ([module docs](self)).
const SEARCH_PER_BYTE: usize = 64;

/// The least the search may hash, however few bytes follow the frame: as much
/// as one frame can hold. A crash's tail is part of one frame, so each
/// look-alike head inside it costs at most that to check.
const SEARCH_FLOOR: usize = FRAME_RAW_MAX;

/// What a tape's bytes hold ([module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    /// The header's text ([`super::header::Header::decode`] reads it); `None`
    /// when no header frame was whole.
    pub header: Option<String>,
    /// Every item of the whole records frames, in order.
    pub items: Vec<Item>,
    /// The whole frames taken, the header's included.
    pub frames: usize,
    /// What ended the prefix.
    pub tail: Tail,
}

/// What ended a tape's whole prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tail {
    /// The bytes end exactly after a whole frame (or are empty).
    Clean,
    /// A frame cut short (or damaged) with nothing whole after it; the whole
    /// prefix ends at byte `at`.
    Torn { at: usize },
    /// Damage inside the tape, or a frame that is whole and wrong, at byte
    /// `at`. A plain read refuses the tape.
    Fault { at: usize, why: Why },
}

/// Why a tape is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// The file does not start with a tape's magic.
    NotATape,
    /// A tape of a frame format this build does not read: a newer build's.
    Format(u8),
    /// A frame that fails its checksum or its shape, with a whole frame after
    /// it.
    Damaged,
    /// A frame whose length runs past the end, with a whole frame after it: a
    /// damaged length, not a torn write.
    LengthPastEnd,
    /// The first frame is not the header.
    NoHeader,
    /// A header frame after the first.
    SecondHeader,
    /// A whole frame whose bytes are not what it claims: the header's text
    /// not UTF-8, or items this build does not read.
    Unreadable(String),
    /// Items that would hold more than `limit` bytes in memory ([`HELD_MAX`]):
    /// no tape a writer leaves comes near it.
    TooLarge { limit: usize },
    /// A frame that is short or fails its checksum, followed by more
    /// frame-like bytes than the reader hashes: whether a whole frame is
    /// among them is not decided, so the tape is refused, never guessed torn.
    Unsearched,
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotATape => write!(f, "not a harness tape"),
            Self::Format(n) => write!(
                f,
                "a tape of frame format {n}: this build reads format {FRAME_FORMAT}"
            ),
            Self::Damaged => write!(f, "a damaged frame with whole frames after it"),
            Self::LengthPastEnd => write!(
                f,
                "a frame whose length runs past the end with whole frames after it"
            ),
            Self::NoHeader => write!(f, "the first frame is not the header"),
            Self::SecondHeader => write!(f, "a header frame after the first"),
            Self::Unreadable(why) => write!(f, "a whole frame this build cannot read: {why}"),
            Self::TooLarge { limit } => write!(
                f,
                "items that would hold more than {limit} bytes, more than this reader holds"
            ),
            Self::Unsearched => write!(
                f,
                "a short or damaged frame followed by more frame-like bytes than this reader \
                 checks"
            ),
        }
    }
}

/// A plain read of a tape's bytes: the whole prefix when the tail is clean
/// or torn; a fault refused — no header and no items, only its [`Tail`].
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "SuperviseTapeRecovery",
        action = "Recover",
        project = "aterm_agent::supervise::tape::conformance_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SuperviseTapeRecovery",
        action = "Tear",
        reason = "The writer dying inside a frame's one write(2) runs no tape code: what it leaves is a prefix of that frame on disk. The Tier-1 bind reads every byte-offset truncation of a real tape."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SuperviseTapeRecovery",
        action = "Crash",
        reason = "The writer dying, or its run ending, between two frames runs no tape code. The Tier-1 bind reads each truncation at a frame boundary."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SuperviseTapeRecovery",
        action = "Rot",
        reason = "A byte damaged at rest runs no tape code. The Tier-1 bind flips every byte of every frame that has a whole frame after it, under three masks (0x01, 0x80, 0xff)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SuperviseTapeRecovery",
        action = "RotLast",
        reason = "A byte of the last whole frame damaged at rest runs no tape code. The Tier-1 bind flips every byte of the last frame, under three masks (0x01, 0x80, 0xff)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SuperviseTapeRecovery",
        action = "RotMagic",
        reason = "A byte of the magic damaged at rest runs no tape code. The Tier-1 bind flips every byte of the magic of tapes of one, two and three frames, under three masks (0x01, 0x80, 0xff)."
    )
)]
#[must_use]
pub fn recover(bytes: &[u8]) -> Recovered {
    read(bytes, Keep::Refuse, Check::Xxh64, Limits::TAPE)
}

/// [`recover`], keeping the prefix before a fault: the explicit repair of a
/// damaged tape (`tape cat --salvage`). The tail still says the fault.
#[must_use]
pub fn salvage(bytes: &[u8]) -> Recovered {
    read(bytes, Keep::Prefix, Check::Xxh64, Limits::TAPE)
}

/// What a read keeps of a faulted tape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keep {
    Refuse,
    Prefix,
}

/// Whether a frame's checksum is checked: always, but in the negative control
/// of the Tier-1 bind, a reader that takes a damaged frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Check {
    Xxh64,
    #[cfg(test)]
    Nothing,
}

/// The reader's own bounds ([module docs](self)): what a read holds, and the
/// least its search after a short or bad frame may hash. A test passes small
/// ones to reach them with small bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub(crate) held: usize,
    pub(crate) search_floor: usize,
}

impl Limits {
    /// The bounds [`recover`] and [`salvage`] read with.
    pub(crate) const TAPE: Self = Self {
        held: HELD_MAX,
        search_floor: SEARCH_FLOOR,
    };
}

/// One frame as it stands at an offset.
enum Frame {
    /// Whole: its kind, raw bytes and where the next frame starts.
    Whole {
        kind: Kind,
        raw: Vec<u8>,
        end: usize,
    },
    /// Its head or payload runs past the end of the bytes.
    Short,
    /// It fails its checksum or its shape.
    Bad,
}

/// A frame's head whose lengths are within the writer's bounds and whose
/// payload is inside the bytes: what can be known of a frame without hashing
/// it.
struct Head<'a> {
    kind: Kind,
    raw_len: usize,
    comp_len: usize,
    sum: u64,
    /// The head's two lengths, as stored (the checksum covers them).
    lengths: &'a [u8],
    /// The payload, as stored.
    payload: &'a [u8],
    /// Where the next frame starts.
    end: usize,
}

/// The head of the frame at `at`, or the frame it already is ([`Frame::Short`],
/// [`Frame::Bad`]).
fn head_at(bytes: &[u8], at: usize) -> Result<Head<'_>, Frame> {
    let Some(head) = bytes.get(at..).and_then(|rest| rest.get(..HEAD)) else {
        return Err(Frame::Short);
    };
    let word = |from: usize| {
        let mut w = [0u8; 4];
        w.copy_from_slice(&head[from..from + 4]);
        u32::from_le_bytes(w) as usize
    };
    let raw_len = word(1);
    let comp_len = word(5);
    let mut sum = [0u8; 8];
    sum.copy_from_slice(&head[9..HEAD]);
    let sum = u64::from_le_bytes(sum);
    let Some(kind) = Kind::of(head[0]) else {
        return Err(Frame::Bad);
    };
    // The writer's own bounds: no frame longer than the cap, and LZ4 only
    // where it made the payload shorter — which also bounds what a damaged
    // length could make the decompression allocate.
    if raw_len > FRAME_RAW_MAX || (comp_len != 0 && comp_len >= raw_len) {
        return Err(Frame::Bad);
    }
    let stored = if comp_len == 0 { raw_len } else { comp_len };
    let start = at + HEAD;
    let end = start.checked_add(stored).ok_or(Frame::Short)?;
    let payload = bytes.get(start..end).ok_or(Frame::Short)?;
    Ok(Head {
        kind,
        raw_len,
        comp_len,
        sum,
        lengths: &head[1..9],
        payload,
        end,
    })
}

/// The frame `head` heads: whole when it passes its checksum and its payload
/// comes back to its raw length.
fn whole(head: &Head<'_>, check: Check) -> Frame {
    // The checksum first: a damaged frame is refused before anything in it
    // is decompressed.
    if check == Check::Xxh64 && checksum(head.kind, head.lengths, head.payload) != head.sum {
        return Frame::Bad;
    }
    let raw = if head.comp_len == 0 {
        head.payload.to_vec()
    } else {
        match aterm_lz4::decompress(head.payload, head.raw_len) {
            Ok(raw) if raw.len() == head.raw_len => raw,
            Ok(_) | Err(_) => return Frame::Bad,
        }
    };
    Frame::Whole {
        kind: head.kind,
        raw,
        end: head.end,
    }
}

/// The frame at `at` ([`Frame`]).
fn frame_at(bytes: &[u8], at: usize, check: Check) -> Frame {
    match head_at(bytes, at) {
        Ok(head) => whole(&head, check),
        Err(frame) => frame,
    }
}

/// What the search after a short or bad frame found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    /// A whole records frame starts after it.
    Whole,
    /// None does.
    Nothing,
    /// The search met its bound before it could say ([module docs](self)).
    Unsearched,
}

/// Whether a whole records frame starts anywhere after `at` (never at `at`),
/// hashing at most [`SEARCH_PER_BYTE`] times the bytes after `at`, and at
/// least `floor`. Each would-be frame is charged its payload before it is
/// hashed, so the bound holds whatever the bytes are.
fn whole_frame_after(bytes: &[u8], at: usize, check: Check, floor: usize) -> After {
    let rest = bytes.len().saturating_sub(at);
    let mut budget = rest.saturating_mul(SEARCH_PER_BYTE).max(floor);
    for q in at.saturating_add(1)..bytes.len() {
        if bytes[q] != Kind::Records as u8 {
            continue;
        }
        let Ok(head) = head_at(bytes, q) else {
            continue;
        };
        let Some(left) = budget.checked_sub(head.payload.len()) else {
            return After::Unsearched;
        };
        budget = left;
        if matches!(
            whole(&head, check),
            Frame::Whole {
                kind: Kind::Records,
                ..
            }
        ) {
            return After::Whole;
        }
    }
    After::Nothing
}

/// The read behind [`recover`] and [`salvage`].
pub(crate) fn read(bytes: &[u8], keep: Keep, check: Check, limits: Limits) -> Recovered {
    let mut out = Recovered {
        header: None,
        items: Vec::new(),
        frames: 0,
        tail: Tail::Clean,
    };
    if bytes.is_empty() {
        return out;
    }
    if bytes.len() < MAGIC.len() || bytes[..MAGIC.len()] != MAGIC {
        out.tail = magic_tail(bytes);
        return out;
    }
    let mut reader = Reader {
        decoder: Decoder::default(),
        held: 0,
        limit: limits.held,
    };
    let mut at = MAGIC.len();
    let tail = loop {
        if at == bytes.len() {
            break if out.frames == 0 {
                Tail::Torn { at }
            } else {
                Tail::Clean
            };
        }
        let floor = limits.search_floor;
        match frame_at(bytes, at, check) {
            Frame::Whole { kind, raw, end } => {
                if let Err(why) = reader.take(&mut out, kind, &raw) {
                    break Tail::Fault { at, why };
                }
                at = end;
            }
            Frame::Short => break broken(bytes, at, check, floor, Why::LengthPastEnd),
            Frame::Bad => break broken(bytes, at, check, floor, Why::Damaged),
        }
    };
    if matches!(tail, Tail::Fault { .. }) && keep == Keep::Refuse {
        out.header = None;
        out.items.clear();
        out.frames = 0;
    }
    out.tail = tail;
    out
}

/// What ends the prefix at a short or bad frame at `at`: the torn tail when
/// nothing whole follows it, else the fault `why`, or [`Why::Unsearched`]
/// when the search met its bound first.
fn broken(bytes: &[u8], at: usize, check: Check, floor: usize, why: Why) -> Tail {
    match whole_frame_after(bytes, at, check, floor) {
        After::Nothing => Tail::Torn { at },
        After::Whole => Tail::Fault { at, why },
        After::Unsearched => Tail::Fault {
            at,
            why: Why::Unsearched,
        },
    }
}

/// The tail of bytes that do not start with the magic: the start of one, cut
/// short, is a first write that tore; a tape of another frame format is
/// refused by its number; anything else is no tape.
fn magic_tail(bytes: &[u8]) -> Tail {
    if bytes.len() < MAGIC.len() && MAGIC.starts_with(bytes) {
        return Tail::Torn { at: 0 };
    }
    let why = match bytes.get(..MAGIC.len()) {
        Some(m) if m[..6] == MAGIC[..6] && m[7] == MAGIC[7] => Why::Format(m[6]),
        _ => Why::NotATape,
    };
    Tail::Fault { at: 0, why }
}

/// What a read carries from one whole frame to the next: the items' decoder,
/// and the bytes held so far against the limit ([module docs](self)).
struct Reader {
    decoder: Decoder,
    held: usize,
    limit: usize,
}

impl Reader {
    /// `bytes` more held, or the read is past its limit.
    fn hold(&mut self, bytes: usize) -> Result<(), Why> {
        self.held = self.held.saturating_add(bytes);
        if self.held > self.limit {
            return Err(Why::TooLarge { limit: self.limit });
        }
        Ok(())
    }

    /// A whole frame, taken into `out`: the first must be the header, every
    /// later one records. A frame is taken whole or not at all.
    fn take(&mut self, out: &mut Recovered, kind: Kind, raw: &[u8]) -> Result<(), Why> {
        match (out.frames, kind) {
            (0, Kind::Header) => {
                let text = String::from_utf8(raw.to_vec())
                    .map_err(|_| Why::Unreadable("a header that is not UTF-8".to_string()))?;
                self.hold(text.len())?;
                out.header = Some(text);
            }
            (0, Kind::Records) => return Err(Why::NoHeader),
            (_, Kind::Header) => return Err(Why::SecondHeader),
            (_, Kind::Records) => {
                let mut input = Input::new(raw);
                let mut items = Vec::new();
                while !input.is_empty() {
                    let spelled = self.decoder.spelled();
                    let item = self
                        .decoder
                        .item(&mut input)
                        .map_err(|e: CodecError| Why::Unreadable(e.to_string()))?;
                    // A name the item spelled joins the decoder's table, and
                    // the item holds its own copy besides.
                    self.hold(held_by(&item) + (self.decoder.spelled() - spelled))?;
                    items.push(item);
                }
                out.items.extend(items);
            }
        }
        out.frames += 1;
        Ok(())
    }
}

/// The bytes `item` holds in memory: itself and everything it owns.
fn held_by(item: &Item) -> usize {
    let owned = match item {
        Item::Effect { record, .. } => match record {
            EffectRecord::Clock(_) | EffectRecord::Rand(_) | EffectRecord::Mono(_) => 0,
            EffectRecord::File { path, bytes } => path.len() + bytes.len(),
            EffectRecord::FileErr { path, .. } => path.len(),
            EffectRecord::Exchange {
                door,
                request,
                reply,
            } => door.len() + request.len() + reply.len(),
            EffectRecord::Emit { door, bytes } => door.len() + bytes.len(),
            EffectRecord::Raised { door, .. } => door.len(),
        },
        Item::Note { kind, text } => kind.len() + text.len(),
        Item::Mark(Mark::Nested { inner, outer }) => inner.len() + outer.len(),
        Item::Mark(Mark::Truncated { .. }) | Item::End(End::Suspended) => 0,
        Item::End(End::Returned { how }) => how.len(),
        Item::End(End::Unwound { message }) => message.len(),
    };
    std::mem::size_of::<Item>() + owned
}

#[cfg(test)]
#[path = "recover_tests.rs"]
mod tests;
