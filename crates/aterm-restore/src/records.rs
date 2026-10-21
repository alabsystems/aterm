// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The region's records and their strict decoders (the crash-restore plan §2.4).
//!
//! # Tail records
//!
//! Raw engine input, in the order the engine took it. No CRC, for two reasons: a
//! record below the tail's commit word was written in full by a process whose stores
//! all stay visible, and anything above the word is never read; and the VT parser
//! accepts any bytes. The reader still bounds-checks every length and kind and stops
//! at the first invalid record, counting a fault. Each record is 8-byte aligned and
//! never straddles the ring's end (a PAD record, or fewer than 16 bytes the reader
//! skips, fills the gap):
//!
//! ```text
//! u8  kind     0 PAD · 1 BYTES · 2 RESIZE · 3 RESIZES_LOST · 4 ENGINE_REPLACED
//!              · 5 SUPERSEDED
//! u8  flags    BYTES: bit0 synthesized (foreground handback / manual reset);
//!              RESIZE: the ResizePolicy (0 Native, 1 ConPty)
//! u16 reserved (0)
//! u32 payload_len
//! u64 stream_end   PTY bytes processed after this record (synthesized bytes add none)
//! payload  BYTES: the bytes · RESIZE: rows u16, cols u16, pad u32, lifetime u64,
//!          ordinal u64 · RESIZES_LOST: n u32
//! pad to 8
//! ```
//!
//! # Keyframes (KFR v1) and history chunks (HCR v1)
//!
//! Both are astream step records (`astream_wire::steps`): a CRC frame around
//! `STEP_VERSION | step u64 | len u32 | body`. A keyframe's step is its sequence
//! number, which must equal its slot's commit word; a history chunk's steps are
//! dense across the ring's wraps. Inside a region bounded by a commit word, a torn
//! tail cannot appear, so any end the step scan reports other than `Clean` is a
//! fault (astream's "refuse to build on a mid-log fault", applied read-only), save
//! one: the zeroed gap the writer leaves at the ring's physical end when the next
//! chunk would straddle it. The ring walk (`recover.rs`) reads a non-`Clean` end there
//! as that gap when every byte after the last record is zero and the run goes on at
//! the ring's start, so it relies on the scan accepting every record before the zeros
//! and stopping where they start.
//!
//! A keyframe's body is `aterm_lz4::compress_prepend_size` of:
//!
//! ```text
//! u8 version=1 · u8 flags (bit0 parser_ground, bit1 alt_active, bit2 bridge_on_inactive,
//!                         bit3 integration_on)
//! u16 rows · u16 cols · u16 0
//! u64 seq · u64 tail_from · u64 stream_at · u64 resize_lifetime · u64 resize_ordinal
//! HistoryFence at capture: u64 renumber_epoch · u64 clear_gen · u64 reveal_gen · u16 cols
//!                          · u64 oldest · u64 end · u8 detached
//! u32 bridge_lines · u64 config_fp
//! u32 meta_len, the CheckpointMeta JSON (no nonce; parser and output_state kept)
//! u32 grid_len, the main grid (serialize_lines, scrollback-then-visible)
//! u32 alt_len, the inactive grid (0 = none)
//! ```
//!
//! A history chunk's body is a 40-byte head and one handoff-history sidecar frame,
//! LZ4-compressed with its size prepended:
//!
//! ```text
//! u8 version=1 · u8 0 · u16 cols · u32 n_lines · u64 first_row · u64 renumber_epoch
//! · u64 clear_gen · u64 reveal_gen
//! lz4( u32 frame_len | serialize_lines(≤ 1024 lines) )
//! ```
//!
//! # The control record (CTR v1)
//!
//! The control slot holds one astream step record whose step is the slot's commit word,
//! around `u8 version=1 · aterm_lz4::compress_prepend_size(carry)`. The carry is the
//! window's (`aterm-gui`'s crash carry of the session's turn ledger), opaque here.
//!
//! Every decoder here is panic-free on any bytes: lengths are checked before they
//! index, and every LZ4 size prefix is bounded before anything is allocated.

use astream_wire::steps::{StepEnd, encode_step, scan_steps_from};
use aterm_core::grid::{HistoryFence, MAX_GRID_COLS, MAX_GRID_ROWS, ResizePolicy};
use aterm_core::scrollback::{
    Line, deserialize_lines_strict, max_linked_record_bytes, serialize_lines_bounded,
};
use aterm_core::terminal::{CHECKPOINT_META_REQUIRED_KEYS, CheckpointMeta, TerminalCheckpoint};

// ---------------------------------------------------------------------------
// Tail records
// ---------------------------------------------------------------------------

/// A tail record's header length.
pub const TAIL_RECORD_HEADER: usize = 16;
/// PAD: fills the ring from a record's end to the ring's end.
pub const KIND_PAD: u8 = 0;
/// BYTES: engine input.
pub const KIND_BYTES: u8 = 1;
/// RESIZE: a resize the engine applied, in engine order.
pub const KIND_RESIZE: u8 = 2;
/// RESIZES_LOST: the engine's resize journal overran; that many resizes are not here.
pub const KIND_RESIZES_LOST: u8 = 3;
/// ENGINE_REPLACED: the session's engine was replaced; what follows is another
/// lifetime's.
pub const KIND_ENGINE_REPLACED: u8 = 4;
/// SUPERSEDED: the region's last record. The session's journal goes on in another region
/// (a memory-pressure shed replaced this one with a history-less one, `aterm-gui`'s
/// `crash_restore`), so everything the session read after this point is somewhere else.
/// A restore that crosses it is never exact: the region speaks for the session only up to
/// here, and the keeper may still hold it for a moment (until the new region's COMPANION
/// reaches it), so a window that died in that moment is restored from it. A reader that
/// predates the kind stops at it as at any unknown kind, a counted fault, so it is never
/// read as exact by any build.
pub const KIND_SUPERSEDED: u8 = 5;
/// A RESIZE record's payload length.
pub const RESIZE_PAYLOAD: usize = 24;
/// A RESIZES_LOST record's payload length.
pub const RESIZES_LOST_PAYLOAD: usize = 4;
/// The most bytes one BYTES record carries; a longer slice is split, so one record
/// never needs more than a small part of the ring.
pub const MAX_BYTES_PAYLOAD: usize = 32 * 1024;

/// `n` rounded up to a multiple of 8.
#[must_use]
pub const fn pad8(n: usize) -> usize {
    n.div_ceil(8).saturating_mul(8)
}

/// One tail record's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailRecord<'a> {
    /// Engine input. `synthesized` bytes (a foreground handback, a manual reset)
    /// reached the engine without coming from the PTY, so they add nothing to the
    /// stream count.
    Bytes {
        /// Not PTY output.
        synthesized: bool,
        /// The bytes, as the engine processed them.
        bytes: &'a [u8],
    },
    /// A resize the engine applied.
    Resize {
        /// Rows after the resize.
        rows: u16,
        /// Columns after the resize.
        cols: u16,
        /// The host's resize policy.
        policy: ResizePolicy,
        /// The engine's resize lifetime.
        lifetime: u64,
        /// The resize's ordinal in that lifetime.
        ordinal: u64,
    },
    /// This many resizes the engine's journal no longer held when the tail caught up.
    ResizesLost {
        /// How many.
        n: u32,
    },
    /// The session's engine was replaced.
    EngineReplaced,
    /// The session's journal went on in another region ([`KIND_SUPERSEDED`]).
    Superseded,
}

/// A policy's byte. `ResizePolicy` is non-exhaustive: a policy this build does not know is
/// journaled as a byte no reader accepts, so a replay across it stops and is counted
/// rather than reflow the grid under the wrong policy.
fn policy_byte(policy: ResizePolicy) -> u8 {
    match policy {
        ResizePolicy::Native => 0,
        ResizePolicy::ConPty => 1,
        _ => u8::MAX,
    }
}

fn policy_of(byte: u8) -> Option<ResizePolicy> {
    match byte {
        0 => Some(ResizePolicy::Native),
        1 => Some(ResizePolicy::ConPty),
        _ => None,
    }
}

impl TailRecord<'_> {
    fn kind(&self) -> u8 {
        match self {
            Self::Bytes { .. } => KIND_BYTES,
            Self::Resize { .. } => KIND_RESIZE,
            Self::ResizesLost { .. } => KIND_RESIZES_LOST,
            Self::EngineReplaced => KIND_ENGINE_REPLACED,
            Self::Superseded => KIND_SUPERSEDED,
        }
    }

    fn flags(&self) -> u8 {
        match self {
            Self::Bytes { synthesized, .. } => u8::from(*synthesized),
            Self::Resize { policy, .. } => policy_byte(*policy),
            Self::ResizesLost { .. } | Self::EngineReplaced | Self::Superseded => 0,
        }
    }

    /// The payload's length.
    #[must_use]
    pub fn payload_len(&self) -> usize {
        match self {
            Self::Bytes { bytes, .. } => bytes.len(),
            Self::Resize { .. } => RESIZE_PAYLOAD,
            Self::ResizesLost { .. } => RESIZES_LOST_PAYLOAD,
            Self::EngineReplaced | Self::Superseded => 0,
        }
    }

    /// The record's length in the ring: header, payload, padding to 8.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        TAIL_RECORD_HEADER.saturating_add(pad8(self.payload_len()))
    }

    /// The record's 16-byte header, `stream_end` in it: what the writer copies before a
    /// BYTES payload it copies straight from the slice.
    #[must_use]
    pub fn header(&self, stream_end: u64) -> [u8; TAIL_RECORD_HEADER] {
        let mut out = [0u8; TAIL_RECORD_HEADER];
        out[0] = self.kind();
        out[1] = self.flags();
        let len = u32::try_from(self.payload_len()).unwrap_or(u32::MAX);
        out[4..8].copy_from_slice(&len.to_le_bytes());
        out[8..16].copy_from_slice(&stream_end.to_le_bytes());
        out
    }

    /// Append the record, `stream_end` in its header, to `out`. A BYTES payload over
    /// `u32::MAX` is the caller's to split (the writer splits at
    /// [`MAX_BYTES_PAYLOAD`]); its length field saturates.
    pub fn encode(&self, stream_end: u64, out: &mut Vec<u8>) {
        let start = out.len();
        out.extend_from_slice(&self.header(stream_end));
        match self {
            Self::Bytes { bytes, .. } => out.extend_from_slice(bytes),
            Self::Resize {
                rows,
                cols,
                lifetime,
                ordinal,
                ..
            } => {
                out.extend_from_slice(&rows.to_le_bytes());
                out.extend_from_slice(&cols.to_le_bytes());
                out.extend_from_slice(&0u32.to_le_bytes());
                out.extend_from_slice(&lifetime.to_le_bytes());
                out.extend_from_slice(&ordinal.to_le_bytes());
            }
            Self::ResizesLost { n } => out.extend_from_slice(&n.to_le_bytes()),
            Self::EngineReplaced | Self::Superseded => {}
        }
        let end = start.saturating_add(self.encoded_len());
        out.resize(end, 0);
    }
}

/// A PAD record's header, filling `gap` bytes (at least [`TAIL_RECORD_HEADER`]) to the
/// ring's end. Its payload bytes are left as they are.
#[must_use]
pub fn pad_header(gap: usize, stream_end: u64) -> [u8; TAIL_RECORD_HEADER] {
    let mut out = [0u8; TAIL_RECORD_HEADER];
    let len = u32::try_from(gap.saturating_sub(TAIL_RECORD_HEADER)).unwrap_or(u32::MAX);
    out[0] = KIND_PAD;
    out[4..8].copy_from_slice(&len.to_le_bytes());
    out[8..16].copy_from_slice(&stream_end.to_le_bytes());
    out
}

/// A tail record's header, as read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailHeader {
    /// The record kind.
    pub kind: u8,
    /// The kind's flags.
    pub flags: u8,
    /// Must be zero.
    pub reserved: u16,
    /// The payload's length (before padding).
    pub payload_len: u32,
    /// PTY bytes processed after the record.
    pub stream_end: u64,
}

impl TailHeader {
    /// Read a header. Total: any 16 bytes are a header; [`decode_tail_payload`] judges.
    #[must_use]
    pub fn read(bytes: &[u8; TAIL_RECORD_HEADER]) -> Self {
        let [
            kind,
            flags,
            r0,
            r1,
            l0,
            l1,
            l2,
            l3,
            s0,
            s1,
            s2,
            s3,
            s4,
            s5,
            s6,
            s7,
        ] = *bytes;
        Self {
            kind,
            flags,
            reserved: u16::from_le_bytes([r0, r1]),
            payload_len: u32::from_le_bytes([l0, l1, l2, l3]),
            stream_end: u64::from_le_bytes([s0, s1, s2, s3, s4, s5, s6, s7]),
        }
    }

    /// The record's whole length in the ring (header, payload, padding), saturating.
    #[must_use]
    pub fn record_len(&self) -> usize {
        let payload = usize::try_from(self.payload_len).unwrap_or(usize::MAX);
        TAIL_RECORD_HEADER.saturating_add(pad8(payload))
    }
}

/// Why the tail stopped being read: the first invalid record ends the replay, and is
/// counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailFault {
    /// A kind this build does not know.
    UnknownKind(u8),
    /// Flags or the reserved field hold bits the kind does not define.
    BadFlags,
    /// A payload length the kind does not allow, or one past the ring's end.
    BadLength,
    /// A PAD that does not end exactly at the ring's end.
    BadPad,
    /// A record that runs past the commit word, or a commit word inconsistent with the
    /// floor and the ring.
    PastCommit,
    /// A record whose stream position is behind the one before it.
    StreamBackwards,
    /// The keyframe's tail position is not a record boundary of the tail.
    Misplaced,
    /// The region refused a read.
    Unreadable,
}

impl std::fmt::Display for TailFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownKind(kind) => write!(f, "a tail record of unknown kind {kind}"),
            Self::BadFlags => f.write_str("a tail record with undefined flags"),
            Self::BadLength => f.write_str("a tail record with an impossible length"),
            Self::BadPad => f.write_str("a PAD that does not end at the ring's end"),
            Self::PastCommit => f.write_str("a tail record past the commit word"),
            Self::StreamBackwards => f.write_str("a tail record behind the one before it"),
            Self::Misplaced => f.write_str("a keyframe position that is no record boundary"),
            Self::Unreadable => f.write_str("a tail the region would not read"),
        }
    }
}

/// Judge a record's payload against its header. The PAD kind is the reader's to
/// handle (it carries no content), so it is refused here.
///
/// # Errors
///
/// The [`TailFault`] that ends the replay.
pub fn decode_tail_payload<'a>(
    header: &TailHeader,
    payload: &'a [u8],
) -> Result<TailRecord<'a>, TailFault> {
    if header.reserved != 0 {
        return Err(TailFault::BadFlags);
    }
    if usize::try_from(header.payload_len).ok() != Some(payload.len()) {
        return Err(TailFault::BadLength);
    }
    match header.kind {
        KIND_BYTES => match header.flags {
            0 | 1 => Ok(TailRecord::Bytes {
                synthesized: header.flags == 1,
                bytes: payload,
            }),
            _ => Err(TailFault::BadFlags),
        },
        KIND_RESIZE => {
            let policy = policy_of(header.flags).ok_or(TailFault::BadFlags)?;
            let Some(
                &[
                    r0,
                    r1,
                    c0,
                    c1,
                    p0,
                    p1,
                    p2,
                    p3,
                    l0,
                    l1,
                    l2,
                    l3,
                    l4,
                    l5,
                    l6,
                    l7,
                    o0,
                    o1,
                    o2,
                    o3,
                    o4,
                    o5,
                    o6,
                    o7,
                ],
            ) = payload
                .first_chunk::<RESIZE_PAYLOAD>()
                .filter(|_| payload.len() == RESIZE_PAYLOAD)
            else {
                return Err(TailFault::BadLength);
            };
            if [p0, p1, p2, p3] != [0; 4] {
                return Err(TailFault::BadFlags);
            }
            Ok(TailRecord::Resize {
                rows: u16::from_le_bytes([r0, r1]),
                cols: u16::from_le_bytes([c0, c1]),
                policy,
                lifetime: u64::from_le_bytes([l0, l1, l2, l3, l4, l5, l6, l7]),
                ordinal: u64::from_le_bytes([o0, o1, o2, o3, o4, o5, o6, o7]),
            })
        }
        KIND_RESIZES_LOST => {
            if header.flags != 0 {
                return Err(TailFault::BadFlags);
            }
            let n = payload
                .first_chunk::<RESIZES_LOST_PAYLOAD>()
                .filter(|_| payload.len() == RESIZES_LOST_PAYLOAD)
                .ok_or(TailFault::BadLength)?;
            Ok(TailRecord::ResizesLost {
                n: u32::from_le_bytes(*n),
            })
        }
        KIND_ENGINE_REPLACED => {
            if header.flags != 0 {
                return Err(TailFault::BadFlags);
            }
            if !payload.is_empty() {
                return Err(TailFault::BadLength);
            }
            Ok(TailRecord::EngineReplaced)
        }
        KIND_SUPERSEDED => {
            if header.flags != 0 {
                return Err(TailFault::BadFlags);
            }
            if !payload.is_empty() {
                return Err(TailFault::BadLength);
            }
            Ok(TailRecord::Superseded)
        }
        other => Err(TailFault::UnknownKind(other)),
    }
}

// ---------------------------------------------------------------------------
// Shared step-record reading
// ---------------------------------------------------------------------------

/// What made a step record unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepFault {
    /// The scan did not accept exactly one record ending at the bytes' end (inside a
    /// commit word any end but `Clean` is a fault).
    Frame(StepEnd),
    /// The scan accepted no record, or more than one.
    Count(usize),
}

/// The body of the one step record `bytes` must be, whose step must be `seq`.
fn one_step(bytes: &[u8], seq: u64) -> Result<&[u8], StepFault> {
    let scan = scan_steps_from(bytes, seq);
    if scan.end != StepEnd::Clean {
        return Err(StepFault::Frame(scan.end));
    }
    match scan.steps.as_slice() {
        [(_, range)] if scan.valid_len == bytes.len() => {
            bytes.get(range.clone()).ok_or(StepFault::Count(1))
        }
        steps => Err(StepFault::Count(steps.len())),
    }
}

/// Decompress a size-prepended LZ4 block whose size is at most `max`, allocating only
/// after the bound holds and only fallibly.
fn bounded_lz4(input: &[u8], max: usize) -> Option<Vec<u8>> {
    let (size, rest) = aterm_lz4::uncompressed_size(input).ok()?;
    if size > max {
        return None;
    }
    let mut out = Vec::new();
    out.try_reserve_exact(size).ok()?;
    out.resize(size, 0);
    let written = aterm_lz4::decompress_into(rest, &mut out).ok()?;
    (written == size).then_some(out)
}

/// A little-endian field reader over a body that refuses to read past its end.
struct Fields<'a> {
    bytes: &'a [u8],
}

impl<'a> Fields<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.bytes.split_at_checked(n)?;
        self.bytes = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(*self.take(2)?.first_chunk::<2>()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(*self.take(4)?.first_chunk::<4>()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(*self.take(8)?.first_chunk::<8>()?))
    }

    /// A `u32`-length-prefixed blob.
    fn blob(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u32()?).ok()?;
        self.take(len)
    }
}

fn put_blob(out: &mut Vec<u8>, blob: &[u8]) -> Result<(), EncodeError> {
    let len = u32::try_from(blob.len()).map_err(|_| EncodeError::TooLarge)?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(blob);
    Ok(())
}

/// Why a record could not be encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// The meta did not serialize.
    Meta(String),
    /// A part, or the record, is past what its length field or the frame can hold.
    TooLarge,
    /// The checkpoint carries a shell-integration nonce, which never enters a region.
    Nonce,
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Meta(why) => write!(f, "the keyframe meta did not serialize: {why}"),
            Self::TooLarge => f.write_str("a record too large for its frame"),
            Self::Nonce => f.write_str("a checkpoint carrying a nonce is never journaled"),
        }
    }
}

// ---------------------------------------------------------------------------
// The control record (CTR v1, increment 7)
// ---------------------------------------------------------------------------

/// The control record's body version.
pub const CTR_VERSION: u8 = 1;
/// The largest carry a reader decompresses from a control record: what the update
/// handoff's own control sidecar may hold (`aterm-gui`'s `handoff_carry::MAX_SIDECAR_BYTES`,
/// which a whole 512-record ledger stays well under), so a crash carry is held to the
/// bound an update's is, and a size prefix past it is refused before anything is
/// allocated.
pub const MAX_CONTROL_CARRY: usize = 4 * 1024 * 1024;

/// Why a control record was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlFault {
    /// Not one step record whose step is the slot's commit word, or its CRC fails.
    Step(StepFault),
    /// A body version this build does not read.
    Version(u8),
    /// The LZ4 block is malformed or its size prefix is past [`MAX_CONTROL_CARRY`].
    Compressed,
}

/// Encode a control record (the slot's sequence number `seq`, nonzero): the window's
/// CONTROL CARRY (`carry`, opaque here: `aterm-gui`'s crash carry of the session's turn
/// ledger) as an astream step record whose step is `seq`, around
/// `u8 CTR_VERSION · aterm_lz4::compress_prepend_size(carry)`. The carry is compressed
/// because the slot is small and a ledger's JSON compresses well: more of the newest turns
/// fit.
///
/// # Errors
///
/// [`EncodeError::TooLarge`] for a carry past [`MAX_CONTROL_CARRY`] or a record past what
/// a frame holds.
pub fn encode_control(seq: u64, carry: &[u8]) -> Result<Vec<u8>, EncodeError> {
    if carry.len() > MAX_CONTROL_CARRY {
        return Err(EncodeError::TooLarge);
    }
    let compressed = aterm_lz4::compress_prepend_size(carry);
    let mut body = Vec::with_capacity(1 + compressed.len());
    body.push(CTR_VERSION);
    body.extend_from_slice(&compressed);
    encode_step(seq, &body).map_err(|_| EncodeError::TooLarge)
}

/// Decode the control step record `record`, whose slot's commit word is `seq`: the carry.
///
/// Strict: one step record whose step is `seq` and whose CRC holds, a body version this
/// build reads, and an LZ4 size bounded before anything is allocated. Panic-free on any
/// bytes.
///
/// # Errors
///
/// The first [`ControlFault`].
pub fn decode_control(record: &[u8], seq: u64) -> Result<Vec<u8>, ControlFault> {
    let body = one_step(record, seq).map_err(ControlFault::Step)?;
    let (&version, compressed) = body
        .split_first()
        .ok_or(ControlFault::Step(StepFault::Count(0)))?;
    if version != CTR_VERSION {
        return Err(ControlFault::Version(version));
    }
    bounded_lz4(compressed, MAX_CONTROL_CARRY).ok_or(ControlFault::Compressed)
}

// ---------------------------------------------------------------------------
// Keyframes (KFR v1)
// ---------------------------------------------------------------------------

/// The keyframe record's body version.
pub const KFR_VERSION: u8 = 1;
/// The largest keyframe body a reader decompresses: a 384 KiB slot cannot honestly
/// hold more than this much screen, and a size prefix past it is refused before any
/// allocation.
pub const MAX_KEYFRAME_BODY: usize = 64 * 1024 * 1024;

const FLAG_PARSER_GROUND: u8 = 0b001;
const FLAG_ALT_ACTIVE: u8 = 0b010;
const FLAG_BRIDGE_ON_INACTIVE: u8 = 0b100;
const FLAG_INTEGRATION_ON: u8 = 0b1000;

/// A keyframe's facts beside its checkpoint: where in the tail and the stream it was
/// cut, what the engine's resize journal and history fence said then, how much history
/// its bridge may carry, and the config it ran under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyframeHead {
    /// Its sequence number: its step, and its slot's commit word.
    pub seq: u64,
    /// The tail position (`tail_commit`) it was captured at: the tail it needs starts
    /// here.
    pub tail_from: u64,
    /// PTY bytes processed when it was captured.
    pub stream_at: u64,
    /// The engine's resize lifetime then.
    pub resize_lifetime: u64,
    /// The engine's resize ordinal then.
    pub resize_ordinal: u64,
    /// The engine's history fence then.
    pub fence: HistoryFence,
    /// The most history lines its bridge may carry (the decoder's bound).
    pub bridge_lines: u32,
    /// The engine-relevant config's fingerprint then.
    pub config_fp: u64,
    /// The engine required a shell-integration nonce and had one in use then
    /// (`ShellIntegrationPosture::On`). The checkpoint carries no nonce (a restored shell
    /// re-keys), yet the replay depends on the posture: an OSC 7 a background job prints
    /// while an authenticated shell sits at its prompt waits for the shell's next prompt
    /// in such an engine, and lands at once in one with no nonce in use. So the recovery
    /// authorizes a nonce of its own in the scratch engine when this is set
    /// (`RecoverPolicy::scratch_nonce`).
    pub integration_on: bool,
}

/// A decoded keyframe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyframe {
    /// Its facts.
    pub head: KeyframeHead,
    /// The engine as it was.
    pub checkpoint: TerminalCheckpoint,
}

/// Why a keyframe slot holds no keyframe this build restores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyframeFault {
    /// The step record is damaged or not the one the commit word names.
    Step(StepFault),
    /// The LZ4 body is damaged, or its size prefix is past [`MAX_KEYFRAME_BODY`].
    Compressed,
    /// A body version this build does not know.
    Version(u8),
    /// The body ends early, or holds bytes past its last field.
    Truncated,
    /// The body's own sequence number is not its step's.
    Seq,
    /// Flags or reserved bits this build does not define, or flags that disagree with
    /// the meta.
    Flags,
    /// The meta is not JSON, lacks a required key, or does not deserialize.
    Meta,
    /// A geometry no grid has, or one the meta and the head disagree on.
    Geometry,
    /// A grid blob fails the strict line decode or holds the wrong number of records.
    Grid,
    /// The meta carries a shell-integration nonce, in its own field or in an OSC 133/633
    /// mark its parser carry holds: no journal keyframe may carry either form.
    Nonce,
}

impl std::fmt::Display for KeyframeFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Step(fault) => write!(f, "a damaged keyframe record ({fault:?})"),
            Self::Compressed => f.write_str("a keyframe body that does not decompress"),
            Self::Version(v) => write!(f, "a keyframe body of version {v}"),
            Self::Truncated => f.write_str("a keyframe body of the wrong length"),
            Self::Seq => f.write_str("a keyframe whose sequence number is not its step's"),
            Self::Flags => f.write_str("a keyframe with undefined or inconsistent flags"),
            Self::Meta => f.write_str("a keyframe meta this build does not read"),
            Self::Geometry => f.write_str("a keyframe geometry no grid has"),
            Self::Grid => f.write_str("a keyframe grid that fails the strict decode"),
            Self::Nonce => f.write_str("a keyframe that carries a nonce"),
        }
    }
}

/// The strict per-line bounds a grid record is decoded under at `cols`: at most
/// `cols * 256` content bytes (one 256-byte grapheme unit a cell), and a record of at
/// most the handoff's cap (`16 KiB + cols * 512`) plus one OSC 8 link span a column at
/// the ingestion ceilings (`max_linked_record_bytes`). A line the engine holds is never
/// over that; the journal was written by a build of this program in the same session,
/// so nothing a link-heavy row legitimately holds is refused.
fn line_caps(cols: u16) -> (usize, usize, usize) {
    let cells = usize::from(cols);
    let content_cap = cells.saturating_mul(256);
    let record_cap = 16usize
        .saturating_mul(1024)
        .saturating_add(cells.saturating_mul(512));
    (
        cells,
        content_cap,
        max_linked_record_bytes(record_cap, cells),
    )
}

/// Decode a grid blob strictly: at most `max_records` records at `cols`, each under
/// [`line_caps`], no trailing bytes. Returns the record count.
fn strict_records(blob: &[u8], max_records: usize, cols: u16) -> Option<usize> {
    let (cells, content_cap, record_cap) = line_caps(cols);
    deserialize_lines_strict(blob, max_records, cells, content_cap, record_cap).map(|l| l.len())
}

/// Encode one keyframe as the step record a slot holds.
///
/// # Errors
///
/// [`EncodeError::Nonce`] for a checkpoint that carries a nonce, in its own field (only
/// the seamless carry projects one) or inside an OSC 133/633 mark its parser carry holds
/// (`TerminalCheckpoint::parser_holds_shell_mark`); the journal takes
/// `Terminal::checkpoint_journal`, which never does either. [`EncodeError::Meta`] or
/// [`EncodeError::TooLarge`] otherwise.
pub fn encode_keyframe(
    head: &KeyframeHead,
    checkpoint: &TerminalCheckpoint,
) -> Result<Vec<u8>, EncodeError> {
    if checkpoint.shell_integration_nonce.is_some() || checkpoint.parser_holds_shell_mark() {
        return Err(EncodeError::Nonce);
    }
    let meta = CheckpointMeta::from_checkpoint(checkpoint);
    let meta = aterm_json::to_string(&meta).map_err(|e| EncodeError::Meta(e.to_string()))?;
    let alt_active = checkpoint.modes.alternate_screen;
    let mut flags = 0;
    if checkpoint.parser_ground {
        flags |= FLAG_PARSER_GROUND;
    }
    if alt_active {
        flags |= FLAG_ALT_ACTIVE | FLAG_BRIDGE_ON_INACTIVE;
    }
    if head.integration_on {
        flags |= FLAG_INTEGRATION_ON;
    }
    let mut body = Vec::with_capacity(
        128 + meta.len() + checkpoint.grid.len() + checkpoint.alt_grid.as_ref().map_or(0, Vec::len),
    );
    body.push(KFR_VERSION);
    body.push(flags);
    body.extend_from_slice(&checkpoint.rows.to_le_bytes());
    body.extend_from_slice(&checkpoint.cols.to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes());
    for word in [
        head.seq,
        head.tail_from,
        head.stream_at,
        head.resize_lifetime,
        head.resize_ordinal,
        head.fence.renumber_epoch,
        head.fence.clear_gen,
        head.fence.reveal_gen,
    ] {
        body.extend_from_slice(&word.to_le_bytes());
    }
    body.extend_from_slice(&head.fence.cols.to_le_bytes());
    body.extend_from_slice(&head.fence.oldest.to_le_bytes());
    body.extend_from_slice(&head.fence.end.to_le_bytes());
    body.push(u8::from(head.fence.detached));
    body.extend_from_slice(&head.bridge_lines.to_le_bytes());
    body.extend_from_slice(&head.config_fp.to_le_bytes());
    put_blob(&mut body, meta.as_bytes())?;
    put_blob(&mut body, &checkpoint.grid)?;
    put_blob(&mut body, checkpoint.alt_grid.as_deref().unwrap_or(&[]))?;
    if body.len() > MAX_KEYFRAME_BODY {
        return Err(EncodeError::TooLarge);
    }
    let compressed = aterm_lz4::compress_prepend_size(&body);
    encode_step(head.seq, &compressed).map_err(|_| EncodeError::TooLarge)
}

/// Decode the keyframe step record `record`, whose slot's commit word is `seq`.
///
/// Strict: the step must be `seq`, the frame's CRC must hold, the LZ4 size is bounded
/// before anything is allocated, the meta must name every key of
/// `CHECKPOINT_META_REQUIRED_KEYS` and carry no nonce (in its own field, or in an OSC
/// 133/633 mark its parser carry holds), and each grid must pass the
/// strict line decode with the record count its role allows: the main grid exactly
/// `rows + history_lines` (the bridge on the main screen, `history_lines ≤
/// bridge_lines`), the inactive grid `rows` — or, while the alternate screen is up and
/// the bridge rides the saved primary, between `rows` and `rows + bridge_lines`.
///
/// # Errors
///
/// The first [`KeyframeFault`].
pub fn decode_keyframe(record: &[u8], seq: u64) -> Result<Keyframe, KeyframeFault> {
    let compressed = one_step(record, seq).map_err(KeyframeFault::Step)?;
    let body = bounded_lz4(compressed, MAX_KEYFRAME_BODY).ok_or(KeyframeFault::Compressed)?;
    let mut f = Fields { bytes: &body };
    let version = f.u8().ok_or(KeyframeFault::Truncated)?;
    if version != KFR_VERSION {
        return Err(KeyframeFault::Version(version));
    }
    let flags = f.u8().ok_or(KeyframeFault::Truncated)?;
    let rows = f.u16().ok_or(KeyframeFault::Truncated)?;
    let cols = f.u16().ok_or(KeyframeFault::Truncated)?;
    let reserved = f.u16().ok_or(KeyframeFault::Truncated)?;
    if reserved != 0
        || flags
            & !(FLAG_PARSER_GROUND
                | FLAG_ALT_ACTIVE
                | FLAG_BRIDGE_ON_INACTIVE
                | FLAG_INTEGRATION_ON)
            != 0
    {
        return Err(KeyframeFault::Flags);
    }
    let mut word = || f.u64().ok_or(KeyframeFault::Truncated);
    let body_seq = word()?;
    let tail_from = word()?;
    let stream_at = word()?;
    let resize_lifetime = word()?;
    let resize_ordinal = word()?;
    let renumber_epoch = word()?;
    let clear_gen = word()?;
    let reveal_gen = word()?;
    let fence_cols = f.u16().ok_or(KeyframeFault::Truncated)?;
    let oldest = f.u64().ok_or(KeyframeFault::Truncated)?;
    let end = f.u64().ok_or(KeyframeFault::Truncated)?;
    let detached = match f.u8().ok_or(KeyframeFault::Truncated)? {
        0 => false,
        1 => true,
        _ => return Err(KeyframeFault::Flags),
    };
    let bridge_lines = f.u32().ok_or(KeyframeFault::Truncated)?;
    let config_fp = f.u64().ok_or(KeyframeFault::Truncated)?;
    let meta = f.blob().ok_or(KeyframeFault::Truncated)?;
    let grid = f.blob().ok_or(KeyframeFault::Truncated)?;
    let alt = f.blob().ok_or(KeyframeFault::Truncated)?;
    if !f.bytes.is_empty() {
        return Err(KeyframeFault::Truncated);
    }
    if body_seq != seq {
        return Err(KeyframeFault::Seq);
    }
    if rows == 0 || cols == 0 || rows > MAX_GRID_ROWS || cols > MAX_GRID_COLS {
        return Err(KeyframeFault::Geometry);
    }

    let meta = parse_meta(meta)?;
    if meta.rows != rows || meta.cols != cols {
        return Err(KeyframeFault::Geometry);
    }
    if meta.shell_integration_nonce.is_some() {
        return Err(KeyframeFault::Nonce);
    }
    let alt_active = flags & FLAG_ALT_ACTIVE != 0;
    let on_inactive = flags & FLAG_BRIDGE_ON_INACTIVE != 0;
    if alt_active != meta.modes.alternate_screen || on_inactive != alt_active {
        return Err(KeyframeFault::Flags);
    }
    let bridge = usize::try_from(bridge_lines).map_err(|_| KeyframeFault::Grid)?;
    let visible = usize::from(rows);

    // The main grid: exactly `rows + history_lines`, the history bounded by the bridge
    // (none at all while the bridge rides the saved primary).
    let main_bridge = if on_inactive { 0 } else { bridge };
    let history = usize::try_from(meta.history_lines).map_err(|_| KeyframeFault::Grid)?;
    if history > main_bridge {
        return Err(KeyframeFault::Grid);
    }
    let main_records = strict_records(grid, visible.saturating_add(main_bridge), cols)
        .ok_or(KeyframeFault::Grid)?;
    if main_records != visible.saturating_add(history) {
        return Err(KeyframeFault::Grid);
    }

    // The inactive grid: present exactly when the meta has its cursor.
    let alt_grid = match (alt.is_empty(), meta.alt_cursor.is_some()) {
        (true, false) => None,
        (false, true) => {
            let alt_bridge = if on_inactive { bridge } else { 0 };
            let records = strict_records(alt, visible.saturating_add(alt_bridge), cols)
                .ok_or(KeyframeFault::Grid)?;
            if records < visible {
                return Err(KeyframeFault::Grid);
            }
            Some(alt.to_vec())
        }
        _ => return Err(KeyframeFault::Grid),
    };

    let checkpoint = meta.into_checkpoint(grid.to_vec(), alt_grid);
    if checkpoint.parser_holds_shell_mark() {
        return Err(KeyframeFault::Nonce);
    }
    if checkpoint.parser_ground != (flags & FLAG_PARSER_GROUND != 0) {
        return Err(KeyframeFault::Flags);
    }
    Ok(Keyframe {
        head: KeyframeHead {
            seq,
            tail_from,
            stream_at,
            resize_lifetime,
            resize_ordinal,
            fence: HistoryFence {
                renumber_epoch,
                clear_gen,
                reveal_gen,
                cols: fence_cols,
                oldest,
                end,
                detached,
            },
            bridge_lines,
            config_fp,
            integration_on: flags & FLAG_INTEGRATION_ON != 0,
        },
        checkpoint,
    })
}

/// The meta, read as the screen carry reads one: JSON, an object naming every required
/// key, then the typed form.
fn parse_meta(bytes: &[u8]) -> Result<CheckpointMeta, KeyframeFault> {
    let text = std::str::from_utf8(bytes).map_err(|_| KeyframeFault::Meta)?;
    let value: aterm_json::Value = aterm_json::from_str(text).map_err(|_| KeyframeFault::Meta)?;
    let object = value.as_object().ok_or(KeyframeFault::Meta)?;
    if !CHECKPOINT_META_REQUIRED_KEYS
        .iter()
        .all(|key| object.contains_key(*key))
    {
        return Err(KeyframeFault::Meta);
    }
    aterm_json::from_value(value).map_err(|_| KeyframeFault::Meta)
}

// ---------------------------------------------------------------------------
// History chunks (HCR v1)
// ---------------------------------------------------------------------------

/// The history chunk body version.
pub const HCR_VERSION: u8 = 1;
/// A history chunk head's length.
pub const HCR_HEAD_LEN: usize = 40;
/// The most lines one chunk holds: one handoff-history sidecar frame
/// (`aterm-gui`'s `handoff_history::CHUNK_LINES`).
pub const CHUNK_LINES: usize = 1024;
/// Absolute decompressed history-frame budget, including the length-prefixed block.
/// This matches the scrollback codec's 64 MiB decompressed-page ceiling.
pub const MAX_HISTORY_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// What a history export is fenced on, less its extent: two fences with the same
/// identity number the same history the same way (`HistoryFence::holds_at`, the
/// `end` half aside).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HistoryIdentity {
    /// Advanced by any width reflow or unscroll.
    pub renumber_epoch: u64,
    /// Advanced by ED3 and a full reset.
    pub clear_gen: u64,
    /// Advanced by a rows-grow that handed history lines back to the screen.
    pub reveal_gen: u64,
    /// The width every history line is wrapped at.
    pub cols: u16,
}

impl HistoryIdentity {
    /// The identity half of `fence`.
    #[must_use]
    pub fn of(fence: &HistoryFence) -> Self {
        Self {
            renumber_epoch: fence.renumber_epoch,
            clear_gen: fence.clear_gen,
            reveal_gen: fence.reveal_gen,
            cols: fence.cols,
        }
    }
}

/// A history chunk as the ring holds it: its head, and its compressed frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryChunk {
    /// The ring step it was read at.
    pub seq: u64,
    /// The absolute row of its first line.
    pub first_row: u64,
    /// How many lines it holds.
    pub n_lines: u32,
    /// The history identity it was exported under.
    pub identity: HistoryIdentity,
    /// `compress_prepend_size(u32 frame_len | serialize_lines(lines))`.
    pub frame: Vec<u8>,
}

/// Why a history chunk is unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkFault {
    /// The step record is damaged, or not the next step.
    Step(StepFault),
    /// The head is short, of another version, or holds undefined bits.
    Head,
    /// The frame does not decompress within its bound, or does not decode strictly.
    Frame,
}

/// Encode `lines` (at most [`CHUNK_LINES`], wrapped at `identity.cols`, the first at
/// absolute row `first_row`) as the step record `seq` of the history ring.
///
/// # Errors
///
/// [`EncodeError::TooLarge`] past [`CHUNK_LINES`] lines or the frame's cap.
pub fn encode_history_chunk(
    seq: u64,
    first_row: u64,
    identity: HistoryIdentity,
    lines: &[Line],
) -> Result<Vec<u8>, EncodeError> {
    encode_history_chunk_with_budget(seq, first_row, identity, lines, MAX_HISTORY_FRAME_BYTES)
}

// Supplied-budget seam shared by the public entry and its small allocation controls.
fn encode_history_chunk_with_budget(
    seq: u64,
    first_row: u64,
    identity: HistoryIdentity,
    lines: &[Line],
    frame_budget: usize,
) -> Result<Vec<u8>, EncodeError> {
    if lines.len() > CHUNK_LINES {
        return Err(EncodeError::TooLarge);
    }
    let n_lines = u32::try_from(lines.len()).map_err(|_| EncodeError::TooLarge)?;
    let block_budget = frame_budget
        .min(MAX_HISTORY_FRAME_BYTES)
        .checked_sub(4)
        .ok_or(EncodeError::TooLarge)?;
    let serialized = serialize_lines_bounded(lines, block_budget).ok_or(EncodeError::TooLarge)?;
    let frame_len = 4usize
        .checked_add(serialized.len())
        .ok_or(EncodeError::TooLarge)?;
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(frame_len)
        .map_err(|_| EncodeError::TooLarge)?;
    put_blob(&mut frame, &serialized)?;
    let compressed = aterm_lz4::compress_prepend_size(&frame);
    let mut body = Vec::with_capacity(HCR_HEAD_LEN + compressed.len());
    body.push(HCR_VERSION);
    body.push(0);
    body.extend_from_slice(&identity.cols.to_le_bytes());
    body.extend_from_slice(&n_lines.to_le_bytes());
    for word in [
        first_row,
        identity.renumber_epoch,
        identity.clear_gen,
        identity.reveal_gen,
    ] {
        body.extend_from_slice(&word.to_le_bytes());
    }
    body.extend_from_slice(&compressed);
    encode_step(seq, &body).map_err(|_| EncodeError::TooLarge)
}

/// Read a chunk's head from its step body, keeping its frame.
///
/// # Errors
///
/// [`ChunkFault::Head`].
pub fn decode_history_chunk(seq: u64, body: &[u8]) -> Result<HistoryChunk, ChunkFault> {
    let mut f = Fields { bytes: body };
    let version = f.u8().ok_or(ChunkFault::Head)?;
    let zero = f.u8().ok_or(ChunkFault::Head)?;
    if version != HCR_VERSION || zero != 0 {
        return Err(ChunkFault::Head);
    }
    let cols = f.u16().ok_or(ChunkFault::Head)?;
    let n_lines = f.u32().ok_or(ChunkFault::Head)?;
    let first_row = f.u64().ok_or(ChunkFault::Head)?;
    let renumber_epoch = f.u64().ok_or(ChunkFault::Head)?;
    let clear_gen = f.u64().ok_or(ChunkFault::Head)?;
    let reveal_gen = f.u64().ok_or(ChunkFault::Head)?;
    if cols == 0
        || cols > MAX_GRID_COLS
        || usize::try_from(n_lines).map_or(true, |n| n > CHUNK_LINES)
    {
        return Err(ChunkFault::Head);
    }
    Ok(HistoryChunk {
        seq,
        first_row,
        n_lines,
        identity: HistoryIdentity {
            renumber_epoch,
            clear_gen,
            reveal_gen,
            cols,
        },
        frame: f.bytes.to_vec(),
    })
}

impl HistoryChunk {
    /// The chunk's lines, decoded strictly at its width: the frame decompresses within
    /// both the absolute frame budget and the bound its line count and width allow,
    /// holds exactly one length-prefixed
    /// block, and that block holds exactly `n_lines` lines.
    ///
    /// # Errors
    ///
    /// [`ChunkFault::Frame`].
    pub fn lines(&self) -> Result<Vec<Line>, ChunkFault> {
        self.lines_with_budget(MAX_HISTORY_FRAME_BYTES)
    }

    /// The decompressed frame's bound: what its line count and width allow, under the
    /// absolute frame budget.
    fn frame_bound(&self, frame_budget: usize) -> Result<usize, ChunkFault> {
        let (_, _, record_cap) = line_caps(self.identity.cols);
        let n = usize::try_from(self.n_lines).map_err(|_| ChunkFault::Frame)?;
        let max = n
            .checked_mul(record_cap)
            .and_then(|b| b.checked_add(8))
            .ok_or(ChunkFault::Frame)?;
        Ok(max.min(frame_budget).min(MAX_HISTORY_FRAME_BYTES))
    }

    /// The size the frame's LZ4 prefix says it decompresses to, when that is within the
    /// bound [`Self::frame`] decompresses under: what a reader of the frame stream counts
    /// before it decompresses anything.
    ///
    /// # Errors
    ///
    /// [`ChunkFault::Frame`] for a prefix past the bound, or none.
    pub fn frame_size(&self) -> Result<usize, ChunkFault> {
        let max = self.frame_bound(MAX_HISTORY_FRAME_BYTES)?;
        let (size, _) = aterm_lz4::uncompressed_size(&self.frame).map_err(|_| ChunkFault::Frame)?;
        (size <= max).then_some(size).ok_or(ChunkFault::Frame)
    }

    /// The decompressed frame, `u32 frame_len | serialize_lines(lines)`: exactly one
    /// handoff-history sidecar frame, which the window's import reads with the sidecar's
    /// own strict decoder (its link leniency included). Decompressed within the bound its
    /// line count and width allow, to exactly the size its prefix names. The frame holds
    /// exactly one block whose encoded line count equals `n_lines`; the importer still
    /// strictly decodes that block, including its existing hyperlink leniency.
    ///
    /// # Errors
    ///
    /// [`ChunkFault::Frame`].
    pub fn frame(&self) -> Result<Vec<u8>, ChunkFault> {
        let max = self.frame_bound(MAX_HISTORY_FRAME_BYTES)?;
        let frame = bounded_lz4(&self.frame, max).ok_or(ChunkFault::Frame)?;
        let mut outer = Fields { bytes: &frame };
        let block = outer.blob().ok_or(ChunkFault::Frame)?;
        if !outer.bytes.is_empty() {
            return Err(ChunkFault::Frame);
        }
        let mut block = Fields { bytes: block };
        if block.u32() != Some(self.n_lines) {
            return Err(ChunkFault::Frame);
        }
        Ok(frame)
    }

    fn lines_with_budget(&self, frame_budget: usize) -> Result<Vec<Line>, ChunkFault> {
        let (cells, content_cap, record_cap) = line_caps(self.identity.cols);
        let n = usize::try_from(self.n_lines).map_err(|_| ChunkFault::Frame)?;
        let max = self.frame_bound(frame_budget)?;
        let frame = bounded_lz4(&self.frame, max).ok_or(ChunkFault::Frame)?;
        let mut f = Fields { bytes: &frame };
        let block = f.blob().ok_or(ChunkFault::Frame)?;
        if !f.bytes.is_empty() {
            return Err(ChunkFault::Frame);
        }
        let lines = deserialize_lines_strict(block, n, cells, content_cap, record_cap)
            .ok_or(ChunkFault::Frame)?;
        (lines.len() == n).then_some(lines).ok_or(ChunkFault::Frame)
    }
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
