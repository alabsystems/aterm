// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tape recovery and prefix salvage ([`recover`], [`salvage`], [`Tail::Torn`], [`Tail::Fault`],
//! [`Why::OutOfPlace`], [`Why::MagicFormat`], [`Why::TooLarge`], [`Why::Unsearched`],
//! [`super::frame`], [`super::delta`], [`HELD_MAX`], [`super::frame::RUN_CAP`],
//! [`SEARCH_PER_BYTE`], [`SEARCH_FLOOR`]).

use astream_effects::EffectRecord;

use super::codec::{CodecError, Decoder, End, Input, Item, Mark, len64};
use super::frame::{FRAME_RAW_MAX, Format, HEAD, Kind, MAGIC, checksum, place_named};

/// The most a read holds in memory.
pub const HELD_MAX: usize = 1 << 30;

/// The search for a whole frame after a short or bad one hashes at most this
/// many times the bytes after it.
const SEARCH_PER_BYTE: usize = 64;

/// The least the search may hash, however few bytes follow the frame: as much
/// as one frame can hold.
const SEARCH_FLOOR: usize = FRAME_RAW_MAX;

/// What a tape's bytes hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    /// The header's text ([`super::header::Header::decode`] reads it).
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
    /// A frame cut short (or damaged) with nothing whole after it.
    Torn { at: usize },
    /// Damage inside the tape, or a frame that is whole and wrong, at byte `at`.
    Fault { at: usize, why: Why },
}

/// Why a tape is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// The file does not start with a tape's magic.
    NotATape,
    /// A tape of a frame format this build does not read.
    Format(u8),
    /// A magic naming frame format `named` before a header valid only under `header`.
    MagicFormat { named: u8, frames: u8 },
    /// A frame that fails its checksum or its shape, with a whole frame after it.
    Damaged,
    /// A whole frame at place `expected` whose checksum names place `found`.
    OutOfPlace { expected: u64, found: u64 },
    /// A frame whose length runs past the end, with a whole frame after it.
    LengthPastEnd,
    /// The first frame is not the header.
    NoHeader,
    /// A header frame after the first.
    SecondHeader,
    /// A whole frame whose bytes are not what it claims.
    Unreadable(String),
    /// Items that would hold more than `limit` bytes in memory ([`HELD_MAX`]).
    TooLarge { limit: usize },
    /// A frame that is short or fails its checksum.
    Unsearched,
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotATape => write!(f, "not a harness tape"),
            Self::Format(n) => write!(
                f,
                "a tape of frame format {n}: this build reads formats {} and {}",
                Format::One as u8,
                Format::Two as u8
            ),
            Self::MagicFormat { named, frames } => write!(
                f,
                "a magic naming frame format {named} before a header of format {frames}: a \
                 damaged magic"
            ),
            Self::Damaged => write!(f, "a damaged frame with whole frames after it"),
            Self::OutOfPlace { expected, found } => write!(
                f,
                "frame {found} where frame {expected} belongs: whole frames repeated, dropped or \
                 moved"
            ),
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

/// A plain read of a tape's bytes.
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
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "SuperviseTapeRecovery",
        action = "Misplace",
        reason = "Whole frames repeated, dropped or moved at rest run no tape code. The Tier-1 bind reads every arrangement of the whole frames of tapes of one, two and three frames, at most one frame longer than the tape, that is not the tape or a prefix of it."
    )
)]
#[must_use]
pub fn recover(bytes: &[u8]) -> Recovered {
    read(bytes, Keep::Refuse, Check::Xxh64, Limits::TAPE)
}

/// [`recover`], keeping the prefix before a fault.
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

/// Whether a frame's checksum is checked, and its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Check {
    Xxh64,
    #[cfg(test)]
    Nothing,
    #[cfg(test)]
    Unplaced,
}

/// The reader's own bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub(crate) held: usize,
    pub(crate) search_floor: usize,
}

impl Limits {
    /// The bounds [`recover`] and [`salvage`] enforce.
    pub(crate) const TAPE: Self = Self {
        held: HELD_MAX,
        search_floor: SEARCH_FLOOR,
    };
}

/// One frame as it stands at an offset.
enum Frame {
    /// Whole: its kind, raw bytes, where the next frame starts, and the place its sum names.
    Whole {
        kind: Kind,
        raw: Vec<u8>,
        end: usize,
        place: Option<u64>,
    },
    /// Its head or payload runs past the end of the bytes.
    Short,
    /// It fails its checksum or its shape.
    Bad,
}

/// How a read checks a frame's sum: by the tape's frame format, and (but in a
/// negative control) in full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sums {
    format: Format,
    check: Check,
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

/// The place `head`'s frame names: `Ok(None)` when its tape's sums name none
/// (format 1, or a negative control), `Err` when it fails its checksum.
fn place_of(head: &Head<'_>, sums: Sums) -> Result<Option<u64>, ()> {
    let hashed = || checksum(head.kind, head.lengths, head.payload);
    match (sums.check, sums.format) {
        (Check::Xxh64, Format::One) => (hashed() == head.sum).then_some(None).ok_or(()),
        (Check::Xxh64, Format::Two) => place_named(head.sum, hashed()).map(Some).ok_or(()),
        #[cfg(test)]
        (Check::Nothing, _) => Ok(None),
        #[cfg(test)]
        (Check::Unplaced, Format::One) => (hashed() == head.sum).then_some(None).ok_or(()),
        #[cfg(test)]
        (Check::Unplaced, Format::Two) => place_named(head.sum, hashed()).map(|_| None).ok_or(()),
    }
}

/// The frame `head` heads: whole when it passes its checksum — its sum names a
/// place, in a format-2 tape — and its payload comes back to its raw length.
fn whole(head: &Head<'_>, sums: Sums) -> Frame {
    let Ok(place) = place_of(head, sums) else {
        return Frame::Bad;
    };
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
        place,
    }
}

/// The frame at `at` ([`Frame`]).
fn frame_at(bytes: &[u8], at: usize, sums: Sums) -> Frame {
    match head_at(bytes, at) {
        Ok(head) => whole(&head, sums),
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
    /// The search met its bound before it could say.
    Unsearched,
}

/// Whether a whole records frame starts anywhere after `at` (never at `at`),
/// hashing at most [`SEARCH_PER_BYTE`] times the bytes after `at`, and at
/// least `floor`.
fn whole_frame_after(bytes: &[u8], at: usize, sums: Sums, floor: usize) -> After {
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
            whole(&head, sums),
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
    let Some(format) = Format::of_magic(bytes) else {
        out.tail = magic_tail(bytes);
        return out;
    };
    let sums = Sums { format, check };
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
        match frame_at(bytes, at, sums) {
            Frame::Whole {
                kind,
                raw,
                end,
                place,
            } => {
                let expected = len64(out.frames);
                if let Some(found) = place
                    && found != expected
                {
                    break Tail::Fault {
                        at,
                        why: Why::OutOfPlace { expected, found },
                    };
                }
                if let Err(why) = reader.take(&mut out, kind, &raw) {
                    break Tail::Fault { at, why };
                }
                at = end;
            }
            Frame::Short => break broken(bytes, at, sums, floor, Why::LengthPastEnd),
            Frame::Bad => {
                if out.frames == 0
                    && let Some(frames) = header_reads_as_other(bytes, at, sums)
                {
                    break Tail::Fault {
                        at: 0,
                        why: Why::MagicFormat {
                            named: format as u8,
                            frames: frames as u8,
                        },
                    };
                }
                break broken(bytes, at, sums, floor, Why::Damaged);
            }
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

/// The other format, when the first frame at `at`, bad as the magic's format
/// reads it, is whole as the other's: then the magic's format byte is what
/// was damaged.
fn header_reads_as_other(bytes: &[u8], at: usize, sums: Sums) -> Option<Format> {
    let other = match sums.format {
        Format::One => Format::Two,
        Format::Two => Format::One,
    };
    let as_other = Sums {
        format: other,
        ..sums
    };
    matches!(frame_at(bytes, at, as_other), Frame::Whole { .. }).then_some(other)
}

/// What ends the prefix at a short or bad frame at `at`: the torn tail when
/// nothing whole follows it, else the fault `why`, or [`Why::Unsearched`]
/// when the search met its bound first.
fn broken(bytes: &[u8], at: usize, sums: Sums, floor: usize, why: Why) -> Tail {
    match whole_frame_after(bytes, at, sums, floor) {
        After::Nothing => Tail::Torn { at },
        After::Whole => Tail::Fault { at, why },
        After::Unsearched => Tail::Fault {
            at,
            why: Why::Unsearched,
        },
    }
}

/// The tail of bytes that do not start with a magic this build reads: the
/// start of one, cut short, is a first write that tore; a tape of another
/// frame format is refused by its number; anything else is no tape.
fn magic_tail(bytes: &[u8]) -> Tail {
    let magics = [Format::Two.magic(), Format::One.magic()];
    if bytes.len() < MAGIC.len() && magics.iter().any(|m| m.starts_with(bytes)) {
        return Tail::Torn { at: 0 };
    }
    let why = match bytes.get(..MAGIC.len()) {
        Some(m) if m[..6] == MAGIC[..6] && m[7] == MAGIC[7] => Why::Format(m[6]),
        _ => Why::NotATape,
    };
    Tail::Fault { at: 0, why }
}

/// What a read carries from one whole frame to the next: the items' decoder,
/// and the bytes held so far against the limit.
struct Reader {
    decoder: Decoder,
    held: usize,
    limit: usize,
}

impl Reader {
    /// `bytes` more held, or the read is past its limit.
    const fn hold(&mut self, bytes: usize) -> Result<(), Why> {
        self.held = self.held.saturating_add(bytes);
        if self.held > self.limit {
            return Err(Why::TooLarge { limit: self.limit });
        }
        Ok(())
    }

    /// A whole frame, taken into `out`: the first must be the header, every
    /// later one records.
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
const fn held_by(item: &Item) -> usize {
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
