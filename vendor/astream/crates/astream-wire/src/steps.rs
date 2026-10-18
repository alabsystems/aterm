//! Step records: astream's generic opaque record, and the pure classifier that
//! tells a torn tail from a fault.
//!
//! A step record is one [`Frame`] whose payload is
//!
//! ```text
//! | STEP_VERSION (1) | step: u64 LE (8) | body_len: u32 LE (4) | body ... |
//! ```
//!
//! so a record of an `n`-byte body takes `HEADER_SIZE + STEP_HEADER + n` bytes.
//! The body is opaque: the record names no type, and the reader decides what a
//! body means (a workflow step's output, a keyframe, a chunk of history). The
//! frame's CRC covers the whole payload, so the step number and the length are
//! checked as well as the body.
//!
//! Steps are **dense**. A run that starts at step `first` is accepted only while
//! each record's step is exactly the next one — `first`, `first + 1`, … — so a
//! gap, a repeat or a reordering is seen, never skipped over. A run may start at
//! any step ([`scan_steps_from`]), because a ring that has overwritten its oldest
//! records, or the second half of a run split at a ring's wrap, continues the
//! count rather than restarting it. The count never wraps: `u64::MAX` is the last
//! step a run can hold, and an intact record after it is a fault
//! ([`StepFault::PastLastStep`]), never a step 0 accepted as the next.
//!
//! ## A torn tail or a fault
//!
//! [`scan_steps_from`] keeps the longest intact, in-order prefix and says what
//! ended it, as a [`StepEnd`]:
//!
//! * [`StepEnd::Clean`]: the bytes end exactly at a record boundary.
//! * [`StepEnd::Torn`]: an incomplete or corrupt last record with nothing
//!   decodable after it. This is what a writer that dies in the middle of an
//!   append to an append-only file leaves, so a file's reader may drop it: it
//!   held nothing anyone was told was committed.
//! * [`StepEnd::Fault`]: anything else — a record this build cannot read, a step
//!   out of order, or damage with an intact record after it. None of these is a
//!   crash's signature, so a reader must not treat the bytes from there on as
//!   merely torn. Truncating at a fault in a file destroys records that were
//!   acknowledged.
//!
//! The rule that separates the two, [`frame_decodes_after`], is the one
//! `astream-engine`'s log store applies to its own envelope records: a crash
//! leaves nothing decodable behind its partial frame. Every function here is
//! pure and panic-free, so the classification is a function of the bytes alone.
//!
//! ## Inside a region bounded by a commit word
//!
//! An embedder may keep step records in memory it publishes with a commit word:
//! the writer copies a record and then stores the word one past it (a release
//! store); a reader loads the word (an acquire load) and scans only the bytes
//! below it. A torn tail cannot appear below such a word. Every byte there was
//! written in full before the word admitted it, a process that dies loses no
//! store it already made to its memory, and a writer that dies in the middle of
//! a copy leaves its partial record above the word, where no reader looks.
//!
//! So inside a region bounded by a commit word, **any end that is not
//! [`StepEnd::Clean`] is a fault** — a bug, a version skew or damaged memory —
//! whichever variant names it: a `Torn` end there means bytes the word vouched
//! for do not decode. The reader keeps the intact prefix the scan returns and
//! counts the rest as lost; it does not truncate and carry on as a file reader
//! would.
//!
//! `astream-engine`'s workflow journal is the file-backed user of this module: it
//! truncates a `Torn` tail on open and refuses to open past a `Fault`.

use crate::frame::{Frame, FrameError, HEADER_SIZE, MAX_PAYLOAD_LEN};
use std::ops::Range;

// The Clean island, out of line: only a Trust compiler lexes it, so it mounts under
// `cfg(clean_islands)` (build.rs) and a stock compiler never opens the file.
#[cfg(clean_islands)]
#[path = "steps_island.rs"]
mod steps_island;
#[cfg(not(clean_islands))]
pub use crate::twins::step_prefix_advance;
#[cfg(clean_islands)]
pub use steps_island::step_prefix_advance;

contract_fn! {
    /// Kernel-certified step-cursor bound check (`pos <= limit`), backed by the
    /// Clean `step_within_bounds_isl` specification (`clean-kernel-defeq`) and the
    /// Clean-origin [`step_prefix_advance`] (`max(valid_len, next_pos)`).
    #[must_use]
    pub fn step_within_bounds(pos: u64, limit: u64) -> bool
    clauses [ensures result == (pos <= limit)]
    {
        pos <= limit
    }
}

/// Step-record format version: the first payload byte of every step record.
///
/// It versions this one layout, so a build refuses a step record written under
/// a layout it does not know ([`StepFault::BadStep`]). It is **not** a tag that
/// tells one kind of stream from another, and must not be read as one: the
/// cognition `COG_VERSION` is also 1, and a cognition record handed to
/// [`scan_steps_from`] can read as a well-formed step record (its tag and
/// offset bytes land in the step field, its timestamp in the body length). A
/// reader knows what kind of records it holds from where it found them — the
/// journal file it opened, the region it mapped — before it scans a byte. The
/// value cannot move to make the kinds disjoint: every workflow journal on disk
/// holds records stamped 1.
pub const STEP_VERSION: u8 = 1;

/// Bytes of a step record's own header inside the frame payload: version (1) +
/// step (8) + body length (4). A record of an `n`-byte body takes
/// `HEADER_SIZE + STEP_HEADER + n` bytes in all.
pub const STEP_HEADER: usize = 13;

/// The largest body a step record can carry: the frame's payload cap less the
/// step header.
const MAX_STEP_BODY: usize = MAX_PAYLOAD_LEN - STEP_HEADER;

/// Where a record's body starts, counted from the start of its frame.
const BODY_OFFSET: usize = HEADER_SIZE + STEP_HEADER;

/// Encode one step record: `STEP_VERSION | step u64 LE | len u32 LE | body`,
/// inside a CRC [`Frame`].
///
/// A body too large for the frame's payload cap is [`FrameError::TooLarge`], as
/// it would be from [`Frame::encode`]; it is refused before anything is copied.
pub fn encode_step(step: u64, body: &[u8]) -> Result<Vec<u8>, FrameError> {
    if body.len() > MAX_STEP_BODY {
        return Err(FrameError::TooLarge);
    }
    // `MAX_STEP_BODY < MAX_PAYLOAD_LEN` (16 MiB), which always fits in `u32`.
    let len = body.len() as u32;
    let mut payload = Vec::with_capacity(STEP_HEADER + 256);
    payload.push(STEP_VERSION);
    payload.extend_from_slice(&step.to_le_bytes());
    payload.extend_from_slice(&len.to_le_bytes());
    payload.extend_from_slice(body);
    Frame::new(payload).encode()
}

/// Decode one step record's payload (the [`Frame`] payload, not the framed
/// bytes) into `(step, body)`.
///
/// `None` when the payload is shorter than the step header, carries another
/// [`STEP_VERSION`], or names a body longer than the bytes after the header.
/// Bytes after the body are ignored, as `astream-engine`'s journal always has
/// ignored them: the frame's CRC covers them, so they are not damage, and a
/// reader that refused them would refuse journals that open today.
pub fn decode_step(payload: &[u8]) -> Option<(u64, &[u8])> {
    if payload.len() < STEP_HEADER {
        return None;
    }
    let b0 = payload[0];
    let b1 = payload[1];
    let b2 = payload[2];
    let b3 = payload[3];
    let b4 = payload[4];
    let b5 = payload[5];
    let b6 = payload[6];
    let b7 = payload[7];
    let b8 = payload[8];
    let b9 = payload[9];
    let b10 = payload[10];
    let b11 = payload[11];
    let b12 = payload[12];
    if b0 != STEP_VERSION {
        return None;
    }
    let step = u64::from_le_bytes([b1, b2, b3, b4, b5, b6, b7, b8]);
    let len = u32::from_le_bytes([b9, b10, b11, b12]) as usize;
    let end = STEP_HEADER.checked_add(len)?;
    let body = payload.get(STEP_HEADER..end)?;
    Some((step, body))
}

/// What a scan of step records found: the longest intact, in-order prefix and
/// what ended it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepScan {
    /// Each accepted record, in order: its step, and the byte range of its body
    /// within the scanned bytes (`&bytes[range]` is the body).
    pub steps: Vec<(u64, Range<usize>)>,
    /// Byte length of the intact prefix: where the last accepted record ends,
    /// and where a file holding a torn tail would be truncated to.
    pub valid_len: usize,
    /// What ended the prefix.
    pub end: StepEnd,
}

/// What ended the intact prefix of a run of step records.
///
/// Inside a region bounded by a commit word, anything but [`StepEnd::Clean`] is
/// a fault (see the module doc); the variants below are named for an
/// append-only file, where a crash can leave a torn tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepEnd {
    /// The bytes end exactly at a record boundary.
    Clean,
    /// Bytes past the prefix that hold no complete readable record: an
    /// incomplete or corrupt last record with nothing decodable after it, which
    /// is what a crash in the middle of an append leaves. (A rotted length field
    /// on the very last record looks the same.)
    Torn,
    /// Not a crash's signature: the record at byte `at` is readable but wrong, or
    /// damaged with an intact record after it.
    Fault {
        /// Byte offset of the offending record in the scanned bytes.
        at: usize,
        /// Why it is a fault.
        kind: StepFault,
    },
}

/// Why a record ends a run as a [`StepEnd::Fault`] rather than a torn tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepFault {
    /// The frame is corrupt (bad magic, version, flags or CRC), yet a complete
    /// frame decodes after it: damage inside the run, not at its end.
    CorruptFrame(FrameError),
    /// The frame's length runs past the end of the bytes, yet a complete frame
    /// decodes after it: a damaged length field, not a torn write.
    LengthPastEnd,
    /// The frame is intact, but its payload is not a step record this build
    /// reads (another [`STEP_VERSION`], or a body length past the payload).
    BadStep,
    /// The record is intact, but its step is not the next one the run expects.
    OutOfOrder {
        /// The step the scan expected.
        expected: u64,
        /// The step stored in the record.
        found: u64,
    },
    /// The record is intact, but it follows the record at step `u64::MAX`, the
    /// last a run can hold: no step can come next, so whatever step it names
    /// is out of order. It is its own variant because [`StepFault::OutOfOrder`]
    /// has no honest `expected` to report.
    PastLastStep {
        /// The step stored in the record.
        found: u64,
    },
}

impl std::fmt::Display for StepFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepFault::CorruptFrame(e) => {
                write!(f, "corrupt frame ({e}) followed by a decodable frame")
            }
            StepFault::LengthPastEnd => write!(
                f,
                "frame length runs past the end of the bytes but a decodable frame follows it"
            ),
            StepFault::BadStep => write!(f, "unreadable step record"),
            StepFault::OutOfOrder { expected, found } => {
                write!(
                    f,
                    "out-of-order step record (step {found}, expected {expected})"
                )
            }
            StepFault::PastLastStep { found } => write!(
                f,
                "step record after the last step a run can hold (step {found} follows step {})",
                u64::MAX
            ),
        }
    }
}

/// Scan `bytes` as a run of step records whose first step is `first`, keeping the
/// longest intact, in-order prefix and classifying what ended it.
///
/// A record is accepted only when its frame decodes (magic, version, flags,
/// length and CRC all valid), its payload is a step record this build reads, and
/// its step equals `first + steps.len()`. The first record that fails any check
/// ends the prefix, as a torn tail or a fault (see [`StepEnd`]).
///
/// A record whose step is `u64::MAX` is the last the run can hold, because no
/// step can follow it, so the prefix cannot grow past it. The scan still looks at
/// what comes after, by the same rules as anywhere else: nothing is
/// [`StepEnd::Clean`], an incomplete or corrupt record with nothing decodable
/// after it is [`StepEnd::Torn`], damage with an intact record after it is a
/// fault, and an intact record is a fault too — [`StepFault::PastLastStep`], or
/// [`StepFault::BadStep`] if it is no step record. An intact record there is
/// never `Torn`: a file's reader may drop a torn tail, and that record could
/// have been acknowledged. (A counter that starts at 0 needs 2^64 records to
/// get here; only a caller's `first` near the top of the range can.)
///
/// Pure and panic-free: it reads `bytes` only through checked slicing. It keeps
/// ranges, not bodies, so a caller copies only what it keeps; [`Frame::decode`]
/// still copies each payload while it checks the CRC, a transient copy freed
/// before the next record.
pub fn scan_steps_from(bytes: &[u8], first: u64) -> StepScan {
    let mut steps: Vec<(u64, Range<usize>)> = Vec::new();
    let mut pos = 0usize;
    // The step the next record must carry; `None` once the run has accepted step
    // `u64::MAX`, after which no record can be in order.
    let mut expected = Some(first);
    let end = loop {
        // `pos` only ever advances by a frame that decoded from `bytes[pos..]`,
        // so it never passes the end. Were it ever past, the end rule names it
        // Clean, as it names the end of the bytes.
        let Some(rest) = bytes.get(pos..) else {
            break end_without_frame(bytes, pos);
        };
        match Frame::decode(rest) {
            // No complete frame at `pos`: a clean end, a torn tail, or a rotted
            // length field.
            Ok(None) => break end_without_frame(bytes, pos),
            // A complete-but-corrupt frame (or an unreadable header).
            Err(e) => break end_at_corrupt_frame(bytes, pos, e),
            Ok(Some(decoded)) => match (decode_step(&decoded.frame.payload), expected) {
                (None, _) => {
                    break StepEnd::Fault {
                        at: pos,
                        kind: StepFault::BadStep,
                    }
                }
                (Some((found, _)), None) => {
                    break StepEnd::Fault {
                        at: pos,
                        kind: StepFault::PastLastStep { found },
                    }
                }
                (Some((found, _)), Some(expected)) if found != expected => {
                    break StepEnd::Fault {
                        at: pos,
                        kind: StepFault::OutOfOrder { expected, found },
                    }
                }
                (Some((step, body)), Some(_)) => {
                    // The body sits after the frame header and the step header.
                    // Every sum here stays inside the frame just decoded, which
                    // lies inside `bytes`, so none can reach `usize::MAX`; they
                    // saturate rather than add because the verified lane cannot
                    // see `Frame::decode`'s bound on `consumed` (an unchecked `+`
                    // there is a refuted overflow obligation, not a real one).
                    let start = pos.saturating_add(BODY_OFFSET);
                    steps.push((step, start..start.saturating_add(body.len())));
                    pos = pos.saturating_add(decoded.consumed);
                    // Past `u64::MAX` there is no next step. The loop goes on
                    // regardless, so what follows the last step is classified by
                    // the arms above — a complete frame there is a fault, never
                    // handed to `end_without_frame`, whose rule assumes no
                    // complete frame starts at `pos` and would call an intact
                    // last record Torn.
                    expected = step.checked_add(1);
                }
            },
        }
    };
    StepScan {
        steps,
        valid_len: pos,
        end,
    }
}

/// What ends a run at `pos` when no complete frame starts there (the frame
/// codec needs more bytes than `bytes` holds).
///
/// [`StepEnd::Clean`] at the end of the bytes. Otherwise usually the incomplete
/// last frame a crash in the middle of an append leaves, [`StepEnd::Torn`]; but
/// if a complete frame decodes after `pos`, it is the frame's length field that
/// is wrong, and the end is a [`StepFault::LengthPastEnd`] fault.
pub fn end_without_frame(bytes: &[u8], pos: usize) -> StepEnd {
    if pos >= bytes.len() {
        StepEnd::Clean
    } else if frame_decodes_after(bytes, pos) {
        StepEnd::Fault {
            at: pos,
            kind: StepFault::LengthPastEnd,
        }
    } else {
        StepEnd::Torn
    }
}

/// What ends a run at `pos` when the frame there failed to decode with `e`.
///
/// If any complete frame decodes after it, the damage is inside the run, a
/// [`StepFault::CorruptFrame`] fault. If nothing after it decodes, it is the
/// tail a crash left, [`StepEnd::Torn`].
pub fn end_at_corrupt_frame(bytes: &[u8], pos: usize, e: FrameError) -> StepEnd {
    if frame_decodes_after(bytes, pos) {
        StepEnd::Fault {
            at: pos,
            kind: StepFault::CorruptFrame(e),
        }
    } else {
        StepEnd::Torn
    }
}

/// Whether a complete, CRC-valid frame starts anywhere after `pos` (never at
/// `pos` itself). A crash leaves nothing decodable behind its partial frame.
/// Scanning byte by byte is the honest test: a damaged header carries no
/// trustworthy length to skip by.
pub fn frame_decodes_after(bytes: &[u8], pos: usize) -> bool {
    let mut q = pos.saturating_add(1);
    while q < bytes.len() {
        if let Some(rest) = bytes.get(q..) {
            if matches!(Frame::decode(rest), Ok(Some(_))) {
                return true;
            }
        }
        q = q.saturating_add(1);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A step payload written by hand from the documented layout.
    fn payload(version: u8, step: u64, len: u32, body: &[u8]) -> Vec<u8> {
        let mut p = vec![version];
        p.extend_from_slice(&step.to_le_bytes());
        p.extend_from_slice(&len.to_le_bytes());
        p.extend_from_slice(body);
        p
    }

    #[test]
    fn a_step_record_round_trips_through_its_frame() {
        let bytes = encode_step(3, b"hello").unwrap();
        assert_eq!(bytes.len(), HEADER_SIZE + STEP_HEADER + 5);
        let decoded = Frame::decode(&bytes).unwrap().unwrap();
        assert_eq!(decoded.consumed, bytes.len());
        assert_eq!(
            decode_step(&decoded.frame.payload),
            Some((3, &b"hello"[..]))
        );
        let empty = encode_step(u64::MAX, b"").unwrap();
        let decoded = Frame::decode(&empty).unwrap().unwrap();
        assert_eq!(
            decode_step(&decoded.frame.payload),
            Some((u64::MAX, &b""[..]))
        );
    }

    #[test]
    fn decode_step_refuses_what_it_cannot_read_without_panicking() {
        // Every prefix shorter than the header is refused, the empty one too.
        let full = payload(STEP_VERSION, 7, 2, b"ok");
        for cut in 0..STEP_HEADER {
            assert_eq!(decode_step(&full[..cut]), None, "a {cut}-byte payload");
        }
        // Another version.
        assert_eq!(decode_step(&payload(STEP_VERSION + 1, 7, 2, b"ok")), None);
        // A body length past the payload, up to the largest the field can name.
        assert_eq!(decode_step(&payload(STEP_VERSION, 7, 3, b"ok")), None);
        assert_eq!(
            decode_step(&payload(STEP_VERSION, 7, u32::MAX, b"ok")),
            None
        );
        // The documented tolerance: bytes after the body are ignored.
        assert_eq!(
            decode_step(&payload(STEP_VERSION, 7, 1, b"ok")),
            Some((7, &b"o"[..]))
        );
    }

    #[test]
    fn an_oversized_body_is_refused_before_it_is_copied() {
        // One byte past what the frame can carry is the frame's own TooLarge.
        let body = vec![0u8; MAX_STEP_BODY + 1];
        assert_eq!(encode_step(0, &body), Err(FrameError::TooLarge));
        // Exactly the largest body still frames, and decodes back.
        let body = vec![0x5Au8; MAX_STEP_BODY];
        let bytes = encode_step(1, &body).unwrap();
        let scan = scan_steps_from(&bytes, 1);
        assert_eq!(scan.end, StepEnd::Clean);
        assert_eq!(
            scan.steps,
            vec![(1, HEADER_SIZE + STEP_HEADER..bytes.len())]
        );
    }

    #[test]
    fn frame_decodes_after_looks_strictly_past_pos() {
        let one = encode_step(0, b"a").unwrap();
        // The frame AT pos is not "after" it.
        assert!(!frame_decodes_after(&one, 0));
        let mut two = vec![0xFFu8];
        two.extend_from_slice(&one);
        assert!(frame_decodes_after(&two, 0));
        assert!(!frame_decodes_after(&two, 1));
        // Past the end, and at usize::MAX, it is simply false.
        assert!(!frame_decodes_after(&two, two.len()));
        assert!(!frame_decodes_after(&two, usize::MAX));
    }

    #[test]
    fn a_run_that_reaches_the_last_step_ends_there() {
        let mut bytes = encode_step(u64::MAX - 1, b"a").unwrap();
        bytes.extend_from_slice(&encode_step(u64::MAX, b"b").unwrap());
        let scan = scan_steps_from(&bytes, u64::MAX - 1);
        assert_eq!(scan.end, StepEnd::Clean);
        assert_eq!(
            scan.steps.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![u64::MAX - 1, u64::MAX]
        );
        assert_eq!(scan.valid_len, bytes.len());
    }

    #[test]
    fn fault_display_strings_name_their_cause() {
        let corrupt = StepFault::CorruptFrame(FrameError::ChecksumMismatch).to_string();
        assert!(corrupt.contains("CRC"), "{corrupt}");
        assert!(StepFault::LengthPastEnd.to_string().contains("length"));
        assert!(StepFault::BadStep.to_string().contains("step record"));
        let order = StepFault::OutOfOrder {
            expected: 5,
            found: 0,
        }
        .to_string();
        assert!(
            order.contains("step 0") && order.contains("expected 5"),
            "{order}"
        );
        let past = StepFault::PastLastStep { found: 0 }.to_string();
        assert!(
            past.contains("step 0") && past.contains(&u64::MAX.to_string()),
            "{past}"
        );
    }

    #[test]
    fn clean_origin_step_prefix_advance_and_within_bounds_match_spec() {
        assert!(step_within_bounds(0, 0));
        assert!(step_within_bounds(25, 100));
        assert!(!step_within_bounds(101, 100));
        assert_eq!(step_prefix_advance(0, 25), 25);
        assert_eq!(step_prefix_advance(25, 10), 25);
        assert_eq!(step_prefix_advance(25, 50), 50);
    }
}
