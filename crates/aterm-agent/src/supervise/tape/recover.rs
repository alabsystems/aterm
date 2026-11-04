// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tape recovery and prefix salvage ([`recover`], [`salvage`]).
//!
//! A tape is its [`MAGIC`], then astream step records from step 0, the header
//! ([`super::frame`]). astream's scan ([`scan_steps_from`], claim
//! `wire.steps.torn-vs-fault`) keeps the longest intact, in-order prefix of them
//! and says what ended it: the bytes' end ([`Tail::Clean`]); a record a crash
//! cut short or damaged with nothing decodable after it ([`Tail::Torn`]); or
//! damage with a record after it, a record out of its place, or one this build
//! cannot read ([`Tail::Fault`], [`Why::Steps`]). Its bounded search after a
//! break is astream's too: where the search runs out of budget the answer is a
//! fault, never a torn tail. This module decodes the bodies of the records the
//! scan kept ([`super::delta`], [`super::codec`]), within [`HELD_MAX`].
//!
//! astream's rule reads bytes, not intent, so it brings one exception with it
//! (`astream_wire::steps`, "A body that holds a frame of its own"): a frame's
//! bytes are opaque, and a payload stored raw whose items hold a whole encoded
//! astream frame (an `Emit`, `Exchange` or `File` of such bytes) keeps that
//! frame intact. A cut in the last frame past the nested frame's last byte is
//! then a fault at that frame ([`Why::Steps`], `LengthPastEnd`), and damage to
//! it `CorruptFrame`, never [`Tail::Torn`], because a rotted length looks the
//! same. The error runs one way, a torn tail read as a fault, never the
//! reverse: [`recover`] refuses, and [`salvage`] keeps every frame before the
//! last, the prefix a torn read keeps.
//!
//! A magic one byte off this build's, before a record the scan keeps, is a
//! damaged magic ([`Why::DamagedMagic`]): never a tape of the format a rotted
//! format byte names, nor no tape at all.

use astream_effects::EffectRecord;
use astream_wire::frame::HEADER_SIZE;
use astream_wire::steps::{STEP_HEADER, StepEnd, StepFault, StepScan, scan_steps_from};

use super::codec::{CodecError, Decoder, End, Input, Item, Mark};
use super::frame::{FRAME_FORMAT, FRAME_RAW_MAX, Kind, MAGIC};

/// The most a read holds in memory.
pub const HELD_MAX: usize = 1 << 30;

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
    /// A frame cut short (or damaged) with nothing whole after it, at byte `at`.
    Torn { at: usize },
    /// Damage inside the tape, or a frame that is whole and wrong, at byte `at`.
    Fault { at: usize, why: Why },
}

/// Why a tape is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// The file does not start with a tape's magic.
    NotATape,
    /// A tape of a frame format this build does not read: 1 and 2, the frames
    /// before astream step records, or a later one.
    Format(u8),
    /// A magic one byte off this build's, at `byte`, before a step record the
    /// scan keeps from step 0: this build's tape, its magic damaged.
    DamagedMagic { byte: usize },
    /// What astream's scan found wrong with the records ([`StepFault`]): damage
    /// with a whole record after it, a record this build cannot read, or a
    /// record whose step is not its place (frames repeated, dropped or moved).
    Steps(StepFault),
    /// The first frame is not the header.
    NoHeader,
    /// A header frame after the first.
    SecondHeader,
    /// A whole frame whose bytes are not what it claims.
    Unreadable(String),
    /// Items that would hold more than `limit` bytes in memory ([`HELD_MAX`]).
    TooLarge { limit: usize },
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotATape => write!(f, "not a harness tape"),
            Self::Format(n) => write!(
                f,
                "a tape of frame format {n}: this build reads only format {FRAME_FORMAT}, astream \
                 step records"
            ),
            Self::DamagedMagic { byte } => write!(
                f,
                "a tape magic damaged at byte {byte}: frame format {FRAME_FORMAT}'s astream step \
                 records follow it"
            ),
            Self::Steps(fault) => write!(f, "{fault}"),
            Self::NoHeader => write!(f, "the first frame is not the header"),
            Self::SecondHeader => write!(f, "a header frame after the first"),
            Self::Unreadable(why) => write!(f, "a whole frame this build cannot read: {why}"),
            Self::TooLarge { limit } => write!(
                f,
                "items that would hold more than {limit} bytes, more than this reader holds"
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
    read(bytes, Keep::Refuse, HELD_MAX, steps)
}

/// [`recover`], keeping the prefix before a fault.
#[must_use]
pub fn salvage(bytes: &[u8]) -> Recovered {
    read(bytes, Keep::Prefix, HELD_MAX, steps)
}

/// astream's scan of a tape's records, from step 0.
pub(crate) fn steps(records: &[u8]) -> StepScan {
    scan_steps_from(records, 0)
}

/// What a read keeps of a faulted tape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keep {
    Refuse,
    Prefix,
}

/// Where a record starts, counted back from its body: the astream frame's head
/// and the step record's.
pub(crate) const RECORD_HEAD: usize = HEADER_SIZE + STEP_HEADER;

/// The read behind [`recover`] and [`salvage`]: the magic, then the records
/// `scan` keeps after it, decoded holding at most `held` bytes.
pub(crate) fn read(
    bytes: &[u8],
    keep: Keep,
    held: usize,
    scan: fn(&[u8]) -> StepScan,
) -> Recovered {
    let mut out = Recovered {
        header: None,
        items: Vec::new(),
        frames: 0,
        tail: Tail::Clean,
    };
    if bytes.is_empty() {
        return out;
    }
    let Some(records) = bytes.strip_prefix(MAGIC.as_slice()) else {
        out.tail = magic_tail(bytes, scan);
        return out;
    };
    let scanned = scan(records);
    let mut reader = Reader {
        decoder: Decoder::default(),
        held: 0,
        limit: held,
    };
    let taken = scanned.steps.iter().try_for_each(|(_, body)| {
        let at = MAGIC.len() + body.start.saturating_sub(RECORD_HEAD);
        records
            .get(body.clone())
            .ok_or_else(|| Why::Unreadable("a body outside the bytes".to_string()))
            .and_then(|body| reader.take(&mut out, body))
            .map_err(|why| Tail::Fault { at, why })
    });
    let tail = match (taken, scanned.end) {
        (Err(fault), _) => fault,
        (Ok(()), StepEnd::Clean) if out.frames == 0 => Tail::Torn { at: MAGIC.len() },
        (Ok(()), StepEnd::Clean) => Tail::Clean,
        (Ok(()), StepEnd::Torn) => Tail::Torn {
            at: MAGIC.len() + scanned.valid_len,
        },
        (Ok(()), StepEnd::Fault { at, kind }) => Tail::Fault {
            at: MAGIC.len() + at,
            why: Why::Steps(kind),
        },
    };
    if matches!(tail, Tail::Fault { .. }) && keep == Keep::Refuse {
        out.header = None;
        out.items.clear();
        out.frames = 0;
    }
    out.tail = tail;
    out
}

/// The tail of bytes that do not start with this build's magic: the start of
/// one, cut short, is a first write that tore; a magic one byte off before a
/// record `scan` keeps is damaged; a tape of another frame format is refused
/// by its number; anything else is no tape.
fn magic_tail(bytes: &[u8], scan: fn(&[u8]) -> StepScan) -> Tail {
    if bytes.len() < MAGIC.len() && MAGIC.starts_with(bytes) {
        return Tail::Torn { at: 0 };
    }
    let Some((magic, records)) = bytes.split_at_checked(MAGIC.len()) else {
        return Tail::Fault {
            at: 0,
            why: Why::NotATape,
        };
    };
    let mut off = (0..MAGIC.len()).filter(|&i| magic[i] != MAGIC[i]);
    let why = match (off.next(), off.next()) {
        (Some(byte), None) if !scan(records).steps.is_empty() => Why::DamagedMagic { byte },
        _ if magic[..6] == MAGIC[..6] && magic[7] == MAGIC[7] => Why::Format(magic[6]),
        _ => Why::NotATape,
    };
    Tail::Fault { at: 0, why }
}

/// A record's body as a frame: its kind and its raw bytes. The scan vouched
/// for its bytes, so a body that is not what it claims is a writer's fault,
/// never a crash's.
fn unframed(body: &[u8]) -> Result<(Kind, Vec<u8>), Why> {
    let unreadable = |what: &str| Why::Unreadable(what.to_string());
    let [kind, l0, l1, l2, l3, payload @ ..] = body else {
        return Err(unreadable("a frame shorter than its head"));
    };
    let kind = Kind::of(*kind).ok_or_else(|| unreadable("a frame of a kind this build lacks"))?;
    let raw_len = u32::from_le_bytes([*l0, *l1, *l2, *l3]) as usize;
    if raw_len > FRAME_RAW_MAX {
        return Err(unreadable("a frame longer than a frame may be"));
    }
    let raw = match payload.len().cmp(&raw_len) {
        std::cmp::Ordering::Equal => payload.to_vec(),
        std::cmp::Ordering::Less => aterm_lz4::decompress(payload, raw_len)
            .ok()
            .filter(|raw| raw.len() == raw_len)
            .ok_or_else(|| {
                unreadable("a compressed frame that does not come back to its length")
            })?,
        std::cmp::Ordering::Greater => {
            return Err(unreadable(
                "a frame whose payload is longer than its raw bytes",
            ));
        }
    };
    Ok((kind, raw))
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

    /// A whole record's body, taken into `out`: the first must be the header,
    /// every later one records.
    fn take(&mut self, out: &mut Recovered, body: &[u8]) -> Result<(), Why> {
        let (kind, raw) = unframed(body)?;
        match (out.frames, kind) {
            (0, Kind::Header) => {
                let text = String::from_utf8(raw)
                    .map_err(|_| Why::Unreadable("a header that is not UTF-8".to_string()))?;
                self.hold(text.len())?;
                out.header = Some(text);
            }
            (0, Kind::Records) => return Err(Why::NoHeader),
            (_, Kind::Header) => return Err(Why::SecondHeader),
            (_, Kind::Records) => {
                let mut input = Input::new(&raw);
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
