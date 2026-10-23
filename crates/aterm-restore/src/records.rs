// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Crash-restore region record encoders and strict decoders.

use astream_wire::steps::{StepEnd, encode_step, scan_steps_from};
use aterm_core::grid::{HistoryFence, MAX_GRID_COLS, MAX_GRID_ROWS, ResizePolicy};
use aterm_core::scrollback::{
    Line, deserialize_lines_strict, max_linked_record_bytes, serialize_lines_bounded,
};
use aterm_core::terminal::{CHECKPOINT_META_REQUIRED_KEYS, CheckpointMeta, TerminalCheckpoint};

/// Tail record header byte length.
pub const TAIL_RECORD_HEADER: usize = 16;
/// `PAD` record kind filling the remainder of the ring.
pub const KIND_PAD: u8 = 0;
/// `BYTES` record kind for engine input.
pub const KIND_BYTES: u8 = 1;
/// `RESIZE` record kind for an applied engine resize.
pub const KIND_RESIZE: u8 = 2;
/// `RESIZES_LOST` record kind when the engine resize journal overran.
pub const KIND_RESIZES_LOST: u8 = 3;
/// `ENGINE_REPLACED` record kind when the session engine was replaced.
pub const KIND_ENGINE_REPLACED: u8 = 4;
/// `SUPERSEDED` record kind marking a region replaced by another region.
pub const KIND_SUPERSEDED: u8 = 5;
/// `RESIZE` record payload byte length.
pub const RESIZE_PAYLOAD: usize = 24;
/// `RESIZES_LOST` record payload byte length.
pub const RESIZES_LOST_PAYLOAD: usize = 4;
/// Maximum payload byte length for a single `BYTES` record.
pub const MAX_BYTES_PAYLOAD: usize = 32 * 1024;

/// Round `n` up to a multiple of 8.
#[must_use]
pub const fn pad8(n: usize) -> usize {
    n.div_ceil(8).saturating_mul(8)
}

/// Decoded tail record payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailRecord<'a> {
    /// Engine input bytes.
    Bytes {
        /// Whether the bytes were synthesized rather than read from the PTY.
        synthesized: bool,
        /// Engine input slice.
        bytes: &'a [u8],
    },
    /// Applied engine resize.
    Resize {
        /// Rows after the resize.
        rows: u16,
        /// Columns after the resize.
        cols: u16,
        /// Host resize policy.
        policy: ResizePolicy,
        /// Engine resize lifetime.
        lifetime: u64,
        /// Resize ordinal within `lifetime`.
        ordinal: u64,
    },
    /// Count of resizes lost when the engine resize journal overran.
    ResizesLost {
        /// Number of lost resizes.
        n: u32,
    },
    /// Marker that the session's engine was replaced.
    EngineReplaced,
    /// Marker that the session's journal continued in another region ([`KIND_SUPERSEDED`]).
    Superseded,
}

/// A policy's byte. `ResizePolicy` is non-exhaustive: a policy this build does not know is
/// journaled as a byte no reader accepts, so a replay across it stops and is counted
/// rather than reflow the grid under the wrong policy.
const fn policy_byte(policy: ResizePolicy) -> u8 {
    match policy {
        ResizePolicy::Native => 0,
        ResizePolicy::ConPty => 1,
        _ => u8::MAX,
    }
}

const fn policy_of(byte: u8) -> Option<ResizePolicy> {
    match byte {
        0 => Some(ResizePolicy::Native),
        1 => Some(ResizePolicy::ConPty),
        _ => None,
    }
}

impl TailRecord<'_> {
    const fn kind(&self) -> u8 {
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

    /// Unpadded payload byte length.
    #[must_use]
    pub const fn payload_len(&self) -> usize {
        match self {
            Self::Bytes { bytes, .. } => bytes.len(),
            Self::Resize { .. } => RESIZE_PAYLOAD,
            Self::ResizesLost { .. } => RESIZES_LOST_PAYLOAD,
            Self::EngineReplaced | Self::Superseded => 0,
        }
    }

    /// Total 8-byte-aligned record byte length in the ring.
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        TAIL_RECORD_HEADER.saturating_add(pad8(self.payload_len()))
    }

    /// Build the 16-byte record header with `stream_end`.
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

    /// Append the encoded record with `stream_end` to `out`.
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

/// Build a `PAD` record header filling `gap` bytes to the end of the ring.
#[must_use]
pub fn pad_header(gap: usize, stream_end: u64) -> [u8; TAIL_RECORD_HEADER] {
    let mut out = [0u8; TAIL_RECORD_HEADER];
    let len = u32::try_from(gap.saturating_sub(TAIL_RECORD_HEADER)).unwrap_or(u32::MAX);
    out[0] = KIND_PAD;
    out[4..8].copy_from_slice(&len.to_le_bytes());
    out[8..16].copy_from_slice(&stream_end.to_le_bytes());
    out
}

/// Raw 16-byte tail record header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailHeader {
    /// Record kind tag.
    pub kind: u8,
    /// Kind-specific flags.
    pub flags: u8,
    /// Reserved field (must be zero).
    pub reserved: u16,
    /// Unpadded payload length in bytes.
    pub payload_len: u32,
    /// Cumulative PTY bytes processed after this record.
    pub stream_end: u64,
}

impl TailHeader {
    /// Decode a 16-byte header for validation by [`decode_tail_payload`].
    #[must_use]
    pub const fn read(bytes: &[u8; TAIL_RECORD_HEADER]) -> Self {
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

    /// Total 8-byte-aligned record length in the ring, saturating on overflow.
    #[must_use]
    pub fn record_len(&self) -> usize {
        let payload = usize::try_from(self.payload_len).unwrap_or(usize::MAX);
        TAIL_RECORD_HEADER.saturating_add(pad8(payload))
    }
}

/// Fault that terminated tail replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailFault {
    /// Unrecognized record kind.
    UnknownKind(u8),
    /// Undefined flags or non-zero reserved bits.
    BadFlags,
    /// Invalid payload length for the record kind or ring bounds.
    BadLength,
    /// `PAD` record does not end at the ring boundary.
    BadPad,
    /// Record extends past the commit word or commit bounds are inconsistent.
    PastCommit,
    /// Record `stream_end` regressed relative to the previous record.
    StreamBackwards,
    /// Keyframe tail offset does not land on a record boundary.
    Misplaced,
    /// Region read failed.
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

/// Decode and validate a tail record payload against `header`.
///
/// # Errors
/// Returns the [`TailFault`] that stops tail replay.
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

/// Fault making a step record unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepFault {
    /// Step scan did not end cleanly at the buffer boundary.
    Frame(StepEnd),
    /// Step scan did not yield exactly one record.
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

/// Error returned when encoding a region record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// Metadata serialization failed.
    Meta(String),
    /// Part or total record length exceeds format limits.
    TooLarge,
    /// Checkpoint contains a shell-integration nonce, which is never journaled.
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

/// Control record body format version.
pub const CTR_VERSION: u8 = 1;
/// Maximum decompressed byte length of a control carry.
pub const MAX_CONTROL_CARRY: usize = 4 * 1024 * 1024;

/// Fault returned when decoding a control record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlFault {
    /// Step frame or sequence mismatch.
    Step(StepFault),
    /// Unsupported control record body version.
    Version(u8),
    /// Malformed LZ4 payload or uncompressed size above [`MAX_CONTROL_CARRY`].
    Compressed,
}

/// Encode a control carry into step record `seq`.
///
/// # Errors
/// Returns [`EncodeError::TooLarge`] if `carry` exceeds [`MAX_CONTROL_CARRY`] or step framing limits.
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

/// Strictly decode control step record `record` at sequence `seq`.
///
/// # Errors
/// Returns the first [`ControlFault`] encountered.
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

/// Keyframe record body format version.
pub const KFR_VERSION: u8 = 1;
/// Maximum decompressed byte length of a keyframe body.
pub const MAX_KEYFRAME_BODY: usize = 64 * 1024 * 1024;

const FLAG_PARSER_GROUND: u8 = 0b001;
const FLAG_ALT_ACTIVE: u8 = 0b010;
const FLAG_BRIDGE_ON_INACTIVE: u8 = 0b100;
const FLAG_INTEGRATION_ON: u8 = 0b1000;

/// Keyframe header metadata alongside its [`TerminalCheckpoint`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyframeHead {
    /// Keyframe sequence number and slot commit word.
    pub seq: u64,
    /// Tail commit offset where this keyframe was captured.
    pub tail_from: u64,
    /// Cumulative PTY bytes processed at capture.
    pub stream_at: u64,
    /// Engine resize lifetime at capture.
    pub resize_lifetime: u64,
    /// Engine resize ordinal at capture.
    pub resize_ordinal: u64,
    /// Engine history fence at capture.
    pub fence: HistoryFence,
    /// Maximum history bridge lines permitted by the decoder.
    pub bridge_lines: u32,
    /// Engine-relevant config fingerprint at capture.
    pub config_fp: u64,
    /// Whether shell-integration nonce posture was active at capture.
    pub integration_on: bool,
}

/// Decoded keyframe record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyframe {
    /// Keyframe header metadata.
    pub head: KeyframeHead,
    /// Captured terminal checkpoint.
    pub checkpoint: TerminalCheckpoint,
}

/// Fault returned when decoding a keyframe slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyframeFault {
    /// Step frame or sequence mismatch.
    Step(StepFault),
    /// Malformed LZ4 payload or uncompressed size above [`MAX_KEYFRAME_BODY`].
    Compressed,
    /// Unsupported keyframe body version.
    Version(u8),
    /// Truncated body or trailing bytes after the final field.
    Truncated,
    /// Body sequence number does not match the step sequence.
    Seq,
    /// Undefined flags/reserved bits or flag mismatch with checkpoint metadata.
    Flags,
    /// Invalid or incomplete checkpoint metadata JSON.
    Meta,
    /// Invalid grid dimensions or mismatch between header and metadata.
    Geometry,
    /// Grid blob failed strict line decoding or line-count validation.
    Grid,
    /// Checkpoint metadata or parser carry contains a shell-integration nonce.
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
/// the ingestion ceilings (`max_linked_record_bytes`).
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
/// [`line_caps`], no trailing bytes.
fn strict_records(blob: &[u8], max_records: usize, cols: u16) -> Option<usize> {
    let (cells, content_cap, record_cap) = line_caps(cols);
    deserialize_lines_strict(blob, max_records, cells, content_cap, record_cap).map(|l| l.len())
}

/// Encode a keyframe (`head` and `checkpoint`) as step record `head.seq`.
///
/// # Errors
/// Returns [`EncodeError::Nonce`] if `checkpoint` holds a nonce, or [`EncodeError::Meta`] / [`EncodeError::TooLarge`].
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

/// Strictly decode keyframe step record `record` at sequence `seq`.
///
/// # Errors
/// Returns the first [`KeyframeFault`] encountered.
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

/// History chunk body format version.
pub const HCR_VERSION: u8 = 1;
/// History chunk header byte length.
pub const HCR_HEAD_LEN: usize = 40;
/// Maximum lines per history chunk.
pub const CHUNK_LINES: usize = 1024;
/// Maximum decompressed byte budget for a history frame.
pub const MAX_HISTORY_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// History identity fields shared by compatible [`HistoryFence`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HistoryIdentity {
    /// Epoch bumped by width reflow or unscroll.
    pub renumber_epoch: u64,
    /// Generation bumped by ED3 or full reset.
    pub clear_gen: u64,
    /// Generation bumped when a rows-grow resize reveals history lines.
    pub reveal_gen: u64,
    /// Column width at which history lines are wrapped.
    pub cols: u16,
}

impl HistoryIdentity {
    /// Extract the [`HistoryIdentity`] from `fence`.
    #[must_use]
    pub const fn of(fence: &HistoryFence) -> Self {
        Self {
            renumber_epoch: fence.renumber_epoch,
            clear_gen: fence.clear_gen,
            reveal_gen: fence.reveal_gen,
            cols: fence.cols,
        }
    }
}

/// Encoded history chunk header and compressed frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryChunk {
    /// Ring step sequence number.
    pub seq: u64,
    /// Absolute row index of the chunk's first line.
    pub first_row: u64,
    /// Number of lines in the chunk.
    pub n_lines: u32,
    /// History identity under which the chunk was exported.
    pub identity: HistoryIdentity,
    /// Size-prepended LZ4-compressed history frame.
    pub frame: Vec<u8>,
}

/// Fault returned when decoding a history chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkFault {
    /// Step frame or sequence mismatch.
    Step(StepFault),
    /// Truncated, wrong-version, or malformed chunk header.
    Head,
    /// Frame failed bounded LZ4 decompression or strict line decoding.
    Frame,
}

/// Encode `lines` (at most [`CHUNK_LINES`]) as history ring step record `seq`.
///
/// # Errors
/// Returns [`EncodeError::TooLarge`] if `lines` exceeds [`CHUNK_LINES`] or frame limits.
pub fn encode_history_chunk(
    seq: u64,
    first_row: u64,
    identity: HistoryIdentity,
    lines: &[Line],
) -> Result<Vec<u8>, EncodeError> {
    encode_history_chunk_with_budget(seq, first_row, identity, lines, MAX_HISTORY_FRAME_BYTES)
}

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

/// Decode a [`HistoryChunk`] header from step payload `body` at sequence `seq`.
///
/// # Errors
/// Returns [`ChunkFault::Head`] if the header is invalid.
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
    /// Strictly decompress and decode the chunk's [`Line`]s.
    ///
    /// # Errors
    /// Returns [`ChunkFault::Frame`] if decompression or strict line decoding fails.
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

    /// Return the decompressed frame byte length from the LZ4 size prefix after checking bounds.
    ///
    /// # Errors
    /// Returns [`ChunkFault::Frame`] if the prefix is missing or exceeds [`Self::frame`] bounds.
    pub fn frame_size(&self) -> Result<usize, ChunkFault> {
        let max = self.frame_bound(MAX_HISTORY_FRAME_BYTES)?;
        let (size, _) = aterm_lz4::uncompressed_size(&self.frame).map_err(|_| ChunkFault::Frame)?;
        (size <= max).then_some(size).ok_or(ChunkFault::Frame)
    }

    /// Decompress and validate the raw handoff-history sidecar frame (`u32 frame_len | serialize_lines(lines)`).
    ///
    /// # Errors
    /// Returns [`ChunkFault::Frame`] if decompression or block validation fails.
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
