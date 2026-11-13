// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The draft journal's image, format 2: [`MAGIC`], then astream step records from step 0
//! (`astream_wire::steps`, claim `wire.steps.torn-vs-fault`): the header, then each record in
//! one frame or more. An append adds a record's frames in place ([`frame_record`]); a read
//! ([`decode_image`]) keeps the longest prefix of whole records, drops a torn tail and names a
//! fault beside the whole records before it. A torn tail is what astream's scan calls
//! [`StepEnd::Torn`]: what a crash in the middle of an append leaves, and equally a last record
//! damaged with nothing decodable after it, which a read cannot tell from a crash's cut (a
//! crash can leave the file's new length in place before the frame's bytes). It is dropped
//! with no notice and no copy kept, as a crash's is. Damage with a whole record after it is a
//! fault, and so is a cut inside the last record past a whole astream frame its text holds
//! (`astream_wire::steps`, "A body that holds a frame of its own"): a record's text is any
//! UTF-8, so a crash's cut can read that way. A fault's image is kept, never cut, and the whole
//! records before it are still the draft's durable prefix: [`decode_image`] returns them with
//! the fault, and a host opens them and keeps the image aside. Whole records after damage are
//! not in that prefix; they survive only in the image kept aside.
//!
//! ```text
//! a frame's body  | tag (1) | prefix (8, LE) | content … |
//! the header      | 'H' | prefix | document key (8, LE) |
//! a record        | 'P' | prefix | chunk | … | 'R' | prefix | last chunk |
//! its chunks join | kind (1) | base_seq (8, LE) | seq (8, LE) | payload … |
//! ```
//!
//! A frame's `prefix` is the [`ContentFingerprint`] of every byte of the image before the
//! frame, so each frame names the image it was written after. Under the journal lock, an
//! appender compares its expected [`JournalTail`] with the file's length and last frame's
//! head ([`FRAME_HEAD`] bytes), without rescanning earlier payloads. This relies on writers
//! preserving the framed image protocol; it does not detect arbitrary earlier-byte changes.
//! A full read checks every frame and prefix, refusing a mismatch rather than a record.
//!
//! A record longer than one step record carries ([`astream_wire::frame::MAX_PAYLOAD_LEN`] less
//! the heads) is split into `'P'` frames and a last `'R'`. Its frames go in one write, so `'P'`
//! frames with no `'R'` after them are a torn append, dropped with the tail.
//!
//! The record kinds and payloads are format 1's (`ATDJ` records with their own checksum), which
//! a read still takes ([`super::decode_journal`]). Every image published is format 2 but
//! one: an open that read a format-1 journal republishes format 1
//! ([`super::encode_journal_image_v1`]), and the first write after it publishes format 2.

use std::fmt;

use astream_wire::frame::{HEADER_SIZE, MAX_PAYLOAD_LEN};
use astream_wire::steps::{STEP_HEADER, StepEnd, StepFault, encode_step, scan_steps_from};
use aterm_buffer::Seq;

use super::{
    ContentFingerprint, JournalError, JournalPayload, JournalRecord, MAX_JOURNAL_BYTES,
    MAX_JOURNAL_RECORDS, MAX_RECORD_BYTES, decode_edits, encode_payload, put_u64,
    validate_record_shape,
};

/// A format-2 image's first eight bytes: `ATDJ`, the bytes a format-1 reader takes for a
/// record version it does not know (`0x0200`, so an older aterm refuses the image and keeps
/// it), the format's letter and a newline.
pub const MAGIC: [u8; 8] = [b'A', b'T', b'D', b'J', 0, 2, b'S', b'\n'];

const TAG_HEADER: u8 = b'H';
const TAG_PART: u8 = b'P';
const TAG_LAST: u8 = b'R';

/// A frame body's tag and prefix fingerprint.
const BODY_HEAD: usize = 1 + 8;

/// Where a frame's body starts, from the start of the frame.
const BODY_OFFSET: usize = HEADER_SIZE + STEP_HEADER;

/// The bytes of a frame an appender checks before it appends: the astream frame's head (its
/// CRC among them), the step's, the tag and the prefix fingerprint.
pub const FRAME_HEAD: usize = BODY_OFFSET + BODY_HEAD;

/// The most of a record one frame carries: the largest body a step record holds, less the
/// body's head.
const CHUNK_MAX: usize = MAX_PAYLOAD_LEN - STEP_HEADER - BODY_HEAD;

/// A record's kind and its two sequence numbers, ahead of its payload.
const RECORD_HEAD: usize = 1 + 8 + 8;

/// A snapshot record's kind byte, format 1's (`encode_payload`).
const KIND_SNAPSHOT: u8 = 0;
/// A delta record's kind byte, format 1's.
const KIND_DELTA: u8 = 1;

/// Where an image's whole records end, as the one appending to it holds it: its length, its
/// step and record counts, the FNV-1a state of its bytes (so its [`ContentFingerprint`] and
/// the next frame's prefix need no read), its last frame's place and head, and where its last
/// snapshot record ends.
///
/// The tail of a format-1 image an open republished ([`super::encode_journal_image_v1`])
/// names its bytes (its length and fingerprint) and has no frame: nothing appends to it in
/// place ([`Self::appends_in_place`]).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct JournalTail {
    len: u64,
    steps: u64,
    records: u64,
    fnv: u64,
    last_at: u64,
    last_head: [u8; FRAME_HEAD],
    snapshot_end: u64,
    format_1: bool,
}

impl fmt::Debug for JournalTail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JournalTail")
            .field("len", &self.len)
            .field("steps", &self.steps)
            .field("records", &self.records)
            .field("fingerprint", &self.fingerprint())
            .field("format_1", &self.format_1)
            .finish_non_exhaustive()
    }
}

impl JournalTail {
    /// The image of the magic alone.
    const fn magic() -> Self {
        Self {
            len: MAGIC.len() as u64,
            steps: 0,
            records: 0,
            fnv: aterm_hash::fnv1a_64(&MAGIC),
            last_at: 0,
            last_head: [0; FRAME_HEAD],
            snapshot_end: MAGIC.len() as u64,
            format_1: false,
        }
    }

    /// The tail of the format-1 image `bytes`, of `records` records: its length and
    /// fingerprint, and no frame to append after.
    pub(crate) const fn format_1(bytes: &[u8], records: u64) -> Self {
        Self {
            len: bytes.len() as u64,
            steps: 0,
            records,
            fnv: aterm_hash::fnv1a_64(bytes),
            last_at: 0,
            last_head: [0; FRAME_HEAD],
            snapshot_end: bytes.len() as u64,
            format_1: true,
        }
    }

    /// Whether an append extends the image in place ([`frame_record`]): a format-2 image. A
    /// format-1 one ([`Self::format_1`]) is compacted into format 2 by its first append.
    #[inline]
    #[must_use]
    pub const fn appends_in_place(&self) -> bool {
        !self.format_1
    }

    /// The image's length in bytes.
    #[inline]
    #[must_use]
    pub const fn byte_len(&self) -> u64 {
        self.len
    }

    /// The whole records it holds.
    #[inline]
    #[must_use]
    pub const fn records(&self) -> u64 {
        self.records
    }

    /// [`ContentFingerprint::of`] the image's bytes.
    #[inline]
    #[must_use]
    pub const fn fingerprint(&self) -> ContentFingerprint {
        ContentFingerprint(aterm_hash::fnv_mix_u64(self.fnv, self.len))
    }

    /// The bytes of the deltas after its last snapshot record: what a compaction would fold.
    /// An image whose snapshots are large holds them whole, so its length alone says nothing
    /// of what appends have added to it.
    #[inline]
    #[must_use]
    pub const fn appended_since_snapshot(&self) -> u64 {
        self.len.saturating_sub(self.snapshot_end)
    }

    /// Where its last frame starts, and that frame's first [`FRAME_HEAD`] bytes.
    #[inline]
    #[must_use]
    pub const fn last_frame(&self) -> (u64, &[u8; FRAME_HEAD]) {
        (self.last_at, &self.last_head)
    }

    /// Take `frame`, the next whole frame, into the image. A frame is never shorter than
    /// [`FRAME_HEAD`]: [`frame`] writes the body's head, and [`decode_image`] refuses a body
    /// shorter than it.
    fn push(&mut self, frame: &[u8]) {
        if let Some(head) = frame.first_chunk::<FRAME_HEAD>() {
            self.last_head = *head;
        }
        self.last_at = self.len;
        self.fnv = aterm_hash::fnv1a_64_extend(self.fnv, frame);
        self.len = self.len.saturating_add(frame.len() as u64);
        self.steps = self.steps.saturating_add(1);
    }
}

/// Frame `content` under `tag` as the step after `tail`, onto `out`.
fn frame(
    tail: &mut JournalTail,
    tag: u8,
    content: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), JournalError> {
    let mut body = Vec::with_capacity(BODY_HEAD + content.len());
    body.push(tag);
    put_u64(&mut body, tail.fingerprint().0);
    body.extend_from_slice(content);
    let framed = encode_step(tail.steps, &body).map_err(|_| JournalError::TooLarge)?;
    tail.push(&framed);
    out.extend_from_slice(&framed);
    Ok(())
}

/// A record's bytes as a plan carries them: its kind, its sequences and its payload, framed
/// only where it lands ([`frame_record`]).
pub(crate) fn encode_record(record: &JournalRecord) -> Result<Vec<u8>, JournalError> {
    validate_record_shape(record)?;
    let (kind, payload) = encode_payload(&record.payload)?;
    let len = RECORD_HEAD
        .checked_add(payload.len())
        .ok_or(JournalError::TooLarge)?;
    if len > MAX_RECORD_BYTES {
        return Err(JournalError::TooLarge);
    }
    let mut encoded = Vec::with_capacity(len);
    encoded.push(kind);
    put_u64(&mut encoded, record.base_seq.0);
    put_u64(&mut encoded, record.seq.0);
    encoded.extend_from_slice(&payload);
    Ok(encoded)
}

/// The frames that append `record` (bytes [`encode_record`] made) to the image `tail` names,
/// and the tail after them.
///
/// # Errors
/// `record` is shorter than a record's head or longer than a record may be, or `tail` is a
/// format-1 image's, which has no frame to follow ([`JournalTail::appends_in_place`]).
pub fn frame_record(
    tail: &JournalTail,
    record: &[u8],
) -> Result<(Vec<u8>, JournalTail), JournalError> {
    if !tail.appends_in_place() {
        return Err(JournalError::NoHeader);
    }
    if record.len() < RECORD_HEAD {
        return Err(JournalError::Truncated);
    }
    if record.len() > MAX_RECORD_BYTES {
        return Err(JournalError::TooLarge);
    }
    let mut after = *tail;
    let mut out = Vec::with_capacity(record.len() + FRAME_HEAD * (record.len() / CHUNK_MAX + 1));
    let mut chunks = record.chunks(CHUNK_MAX).peekable();
    while let Some(chunk) = chunks.next() {
        let tag = if chunks.peek().is_some() {
            TAG_PART
        } else {
            TAG_LAST
        };
        frame(&mut after, tag, chunk, &mut out)?;
    }
    after.records = after.records.saturating_add(1);
    if record.first() == Some(&KIND_SNAPSHOT) {
        after.snapshot_end = after.len;
    }
    Ok((out, after))
}

/// A whole image of `records`, every one of one document, and its tail.
pub(crate) fn encode_image(
    records: &[JournalRecord],
) -> Result<(Vec<u8>, JournalTail), JournalError> {
    let first = records.first().ok_or(JournalError::Empty)?;
    if records.len() > MAX_JOURNAL_RECORDS {
        return Err(JournalError::TooManyRecords);
    }
    let mut tail = JournalTail::magic();
    let mut image = MAGIC.to_vec();
    frame(
        &mut tail,
        TAG_HEADER,
        &first.document_raw.to_le_bytes(),
        &mut image,
    )?;
    for record in records {
        if record.document_raw != first.document_raw {
            return Err(JournalError::WrongDocument);
        }
        let (frames, after) = frame_record(&tail, &encode_record(record)?)?;
        if image.len().saturating_add(frames.len()) > MAX_JOURNAL_BYTES {
            return Err(JournalError::TooLarge);
        }
        image.extend_from_slice(&frames);
        tail = after;
    }
    Ok((image, tail))
}

/// What a format-2 image holds.
#[derive(Debug)]
pub(crate) struct Image {
    /// Its whole records, in order.
    pub(crate) records: Vec<JournalRecord>,
    /// Where they end: bytes past it are a torn append, dropped, or with a [`Self::fault`]
    /// the bytes a crash does not leave, kept.
    pub(crate) tail: JournalTail,
    /// What astream's scan found after them that is not a crash's signature, if anything.
    pub(crate) fault: Option<StepFault>,
}

/// Read a format-2 image: its whole records, a torn tail dropped. A fault astream's scan finds
/// is named in [`Image::fault`], beside the whole records before it: the scan's kept prefix is
/// fixed before its end is classified, so they are exactly what a clean read of those bytes
/// gives.
///
/// # Errors
/// [`JournalError::Fault`] for a fault before the header; a frame whose prefix is not the bytes
/// before it ([`JournalError::PrefixMismatch`]); no header, or a second; and every record error
/// format 1 has.
pub(crate) fn decode_image(bytes: &[u8]) -> Result<Image, JournalError> {
    if bytes.len() > MAX_JOURNAL_BYTES {
        return Err(JournalError::TooLarge);
    }
    let steps = bytes.strip_prefix(&MAGIC).ok_or(JournalError::BadMagic)?;
    let scan = scan_steps_from(steps, 0);
    let fault = match scan.end {
        StepEnd::Fault { kind, .. } => Some(kind),
        StepEnd::Clean | StepEnd::Torn => None,
    };
    let mut tail = JournalTail::magic();
    let mut whole = None;
    let mut key = None;
    let mut records = Vec::new();
    let mut pending = Vec::new();
    for (index, (_, body)) in scan.steps.iter().enumerate() {
        let start = body
            .start
            .checked_sub(BODY_OFFSET)
            .ok_or(JournalError::Truncated)?;
        let end = scan
            .steps
            .get(index + 1)
            .map_or(Some(scan.valid_len), |(_, next)| {
                next.start.checked_sub(BODY_OFFSET)
            })
            .ok_or(JournalError::Truncated)?;
        let frame_bytes = steps.get(start..end).ok_or(JournalError::Truncated)?;
        let body = steps.get(body.clone()).ok_or(JournalError::Truncated)?;
        let (&tag, rest) = body.split_first().ok_or(JournalError::Truncated)?;
        let (prefix, content) = rest
            .split_first_chunk::<8>()
            .ok_or(JournalError::Truncated)?;
        if u64::from_le_bytes(*prefix) != tail.fingerprint().0 {
            return Err(JournalError::PrefixMismatch);
        }
        match (tag, key) {
            (TAG_HEADER, None) => {
                let raw: [u8; 8] = content
                    .try_into()
                    .map_err(|_| JournalError::LengthMismatch)?;
                key = Some(u64::from_le_bytes(raw));
            }
            (TAG_HEADER, Some(_)) | (TAG_PART | TAG_LAST, None) => {
                return Err(JournalError::NoHeader);
            }
            (TAG_PART | TAG_LAST, Some(document_raw)) => {
                if pending.len().saturating_add(content.len()) > MAX_RECORD_BYTES {
                    return Err(JournalError::TooLarge);
                }
                pending.extend_from_slice(content);
                if tag == TAG_LAST {
                    if records.len() == MAX_JOURNAL_RECORDS {
                        return Err(JournalError::TooManyRecords);
                    }
                    records.push(decode_record(document_raw, &pending)?);
                }
            }
            _ => return Err(JournalError::UnknownRecordKind),
        }
        tail.push(frame_bytes);
        if tag == TAG_LAST {
            if pending.first() == Some(&KIND_SNAPSHOT) {
                tail.snapshot_end = tail.len;
            }
            pending.clear();
        }
        if tag != TAG_PART {
            tail.records = records.len() as u64;
            whole = Some(tail);
        }
    }
    let Some(tail) = whole else {
        return Err(fault.map_or(JournalError::NoHeader, JournalError::Fault));
    };
    Ok(Image {
        records,
        tail,
        fault,
    })
}

/// Read a format-2 image whole: [`decode_image`], with its fault an error.
///
/// # Errors
/// [`decode_image`]'s, and [`JournalError::Fault`] for any fault.
pub(crate) fn decode_whole_image(bytes: &[u8]) -> Result<Image, JournalError> {
    let image = decode_image(bytes)?;
    match image.fault {
        Some(fault) => Err(JournalError::Fault(fault)),
        None => Ok(image),
    }
}

/// The tail of the format-2 image `bytes`: where its whole records end. An image with a fault
/// has none an appender may follow.
///
/// # Errors
/// [`decode_whole_image`]'s.
pub fn journal_tail(bytes: &[u8]) -> Result<JournalTail, JournalError> {
    decode_whole_image(bytes).map(|image| image.tail)
}

/// One record from its joined chunks, of the document whose raw key is `document_raw`.
pub(crate) fn decode_record(
    document_raw: u64,
    bytes: &[u8],
) -> Result<JournalRecord, JournalError> {
    let (head, payload) = bytes
        .split_first_chunk::<RECORD_HEAD>()
        .ok_or(JournalError::Truncated)?;
    let [
        kind,
        b0,
        b1,
        b2,
        b3,
        b4,
        b5,
        b6,
        b7,
        s0,
        s1,
        s2,
        s3,
        s4,
        s5,
        s6,
        s7,
    ] = *head;
    let payload = match kind {
        KIND_SNAPSHOT => JournalPayload::Snapshot(
            std::str::from_utf8(payload)
                .map_err(|_| JournalError::InvalidUtf8)?
                .to_string(),
        ),
        KIND_DELTA => JournalPayload::Delta(decode_edits(payload)?),
        _ => return Err(JournalError::UnknownRecordKind),
    };
    let record = JournalRecord {
        document_raw,
        base_seq: Seq(u64::from_le_bytes([b0, b1, b2, b3, b4, b5, b6, b7])),
        seq: Seq(u64::from_le_bytes([s0, s1, s2, s3, s4, s5, s6, s7])),
        payload,
    };
    validate_record_shape(&record)?;
    Ok(record)
}

#[cfg(test)]
#[path = "journal_steps_tests.rs"]
mod tests;
