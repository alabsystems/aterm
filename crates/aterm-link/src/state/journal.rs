// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE POST JOURNAL: one append-only file per state dir, [`FILE`], in place of a file per post
//! reservation (`sent/<sid>.<id>`) and per own-lane record in flight (`inflight/<seq>`).
//!
//! A post cost the state dir two replaces (a temp written and renamed over the target) and two
//! unlinks; it costs this journal four appends to one held descriptor (B19, after B10 ranked the
//! state dir's writes at 36% of an ordered request/reply round trip and 46% of a relaxed one).
//!
//! Each fact is one [`astream_wire::steps`] record: a CRC frame whose steps run densely from 0
//! in each file, so a cut append reads as a torn tail and damage before an intact record reads
//! as a fault ([`read`]). The bodies are ASCII text (a record's body in hex, as `inflight/` held
//! it), so no body can hold an astream frame of its own (the frame magic starts `0xA5`), and a
//! cut is never mistaken for a fault. The state dir keeps an [`Index`] of what the journal holds
//! and rewrites the journal from it ([`Index::encode`]) at a size bound.
//!
//! A build before the journal reads none of it, so the journal also names the incarnation each
//! attach of this build took ([`Frame::Attached`]): an incarnation above it is one a bridge of
//! an older build took, which went past the journal ([`super::StateDir::judge_posts`]).
//!
//! A RECORD'S TAIL: the bytes of a step record after its body, which astream's step codec
//! keeps under the frame's CRC and every reader skips ([`astream_wire::steps::decode_step`]: a
//! reader that refused them would refuse journals that open today). A reservation and a record
//! in flight carry their heads there ([`PublishPin::at`], [`InFlight::at`]: `at <n>`, and
//! `pin-at <n>` for an entry's pin), the one fact this build adds to the B19 record, so a
//! journal this build wrote reads the same, record for record, to a build of the journal before
//! it, whose parse of a body is strict (aterm-link B05 review). A tail this build cannot read
//! is no head: the record reads without one, and is not read back before it is published again.

use std::collections::BTreeMap;
use std::io;

use astream_wire::frame::{validate_frame_at, HEADER_SIZE, MAGIC};
use astream_wire::steps::{
    decode_step_header, encode_step_header, scan_steps_from, StepEnd, STEP_HEADER,
};

use super::{InFlight, PublishPin};

/// The journal's directory in the state dir.
pub(crate) const DIR: &str = "journal";
/// The journal's name in [`DIR`].
pub(crate) const NAME: &str = "posts";
/// The journal, relative to the state dir.
pub(crate) const FILE: &str = "journal/posts";
/// What a stop's hand-off kept of the journal beside the files it wrote, relative to the state
/// dir: a journal of the same records ([`Index::encode`]) whose tails hold their heads, which
/// the files cannot ([`super::StateDir::hand_posts_to_files`]). No build reads it but this one's
/// adoption of those files, which takes each file's heads from it while no attach went past it
/// ([`super::StateDir::adopt_legacy_posts`]); a build of the journal before the heads reads only
/// [`FILE`] in this directory, and one before the journal none of it.
pub(crate) const HANDED: &str = "journal/handed";
/// The size from which an append first rewrites the journal from its index; after a rewrite the
/// next one waits for twice the rewritten size, so a rewrite's cost is spread over at least as
/// many appended bytes as it wrote.
pub(crate) const COMPACT_AT: u64 = 256 * 1024;

/// One fact the journal records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    /// A post's sequence reservation ([`super::StateDir::set_post_pin`]); a later one for the same
    /// post replaces it.
    Reserve {
        sid: String,
        id: u64,
        pin: PublishPin,
    },
    /// A post's reservation forgotten, once aterm retired the post
    /// ([`super::StateDir::clear_post_seq`]).
    Retire { sid: String, id: u64 },
    /// A record in flight onto this node's own lanes ([`super::StateDir::set_in_flight`]); a later
    /// one at the same sequence replaces it.
    InFlight(InFlight),
    /// The record in flight at this sequence landed ([`super::StateDir::clear_in_flight`]).
    Landed(u64),
    /// The record in flight at this sequence is dropped for good, and the journal flushed after
    /// it ([`super::StateDir::drop_in_flight`]).
    Dropped(u64),
    /// An attach of this build took this incarnation: written before the incarnation lease
    /// (`inc-lease`) says so ([`super::StateDir::take_incarnation`]).
    Attached(u64),
}

/// Whether `sid` can name a session in a frame: what `sent/<sid>.<id>` could hold.
fn is_sid(sid: &str) -> bool {
    !sid.is_empty()
        && !sid.starts_with('.')
        && sid
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'/' && b != b'.')
}

fn number(word: &str) -> Option<u64> {
    (!word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()))
        .then(|| word.parse().ok())
        .flatten()
}

impl Frame {
    /// The file a build before the journal wrote this fact into, where it was a write
    /// (a reservation, a record in flight) and not an unlink: a test's refusal of that file's
    /// write refuses this record (`StateDir::refuse_writes_of`).
    #[cfg(test)]
    pub(crate) fn written_file(&self) -> Option<String> {
        match self {
            Self::Reserve { sid, id, .. } => Some(format!("sent/{sid}.{id}")),
            Self::InFlight(entry) => Some(format!("inflight/{}", entry.pin.seq)),
            Self::Retire { .. } | Self::Landed(_) | Self::Dropped(_) | Self::Attached(_) => None,
        }
    }

    /// Whether the open takes this fact as true once it is asked for, its record written or not:
    /// a reservation retired or a record in flight dropped (each the unlink of an older build's
    /// file, which a disk too full to write does not refuse), an attach named. Its index
    /// then holds what the file may not, and the file is rewritten from the index before its
    /// next record ([`super::StateDir::append_post_frame`]). A reservation or a record in flight
    /// is not (a write that fails reserves nothing), and nor is a landing: the bridge delivers a
    /// record of its own lane only once its landing is written (`Bridge::settle_landed`), so a
    /// landing the disk refuses leaves the record in flight, to be cleared again.
    pub(crate) const fn holds_unwritten(&self) -> bool {
        matches!(
            self,
            Self::Retire { .. } | Self::Dropped(_) | Self::Attached(_)
        )
    }

    /// The body this frame is recorded as.
    fn body(&self) -> String {
        match self {
            Self::Reserve { sid, id, pin } => format!("reserve {sid} {id} {}", pin.render()),
            Self::Retire { sid, id } => format!("retire {sid} {id}"),
            Self::InFlight(entry) => format!("inflight\n{}\n", entry.render()),
            Self::Landed(seq) => format!("landed {seq}"),
            Self::Dropped(seq) => format!("dropped {seq}"),
            Self::Attached(inc) => format!("attached {inc}"),
        }
    }

    /// The tail this frame is recorded with: its heads, as words (the module's A RECORD'S TAIL).
    fn tail(&self) -> String {
        let (at, pin_at) = match self {
            Self::Reserve { pin, .. } => (pin.at, None),
            Self::InFlight(entry) => (entry.at, entry.pin.at),
            Self::Retire { .. } | Self::Landed(_) | Self::Dropped(_) | Self::Attached(_) => {
                (None, None)
            }
        };
        let mut words = Vec::new();
        if let Some(at) = at {
            words.push(format!("at {at}"));
        }
        if let Some(at) = pin_at {
            words.push(format!("pin-at {at}"));
        }
        words.join(" ")
    }

    /// This frame with the heads `tail` names (see [`Frame::tail`]); a tail this build cannot
    /// read names none.
    fn with_tail(mut self, tail: &[u8]) -> Self {
        let mut at = None;
        let mut pin_at = None;
        let words: Option<Vec<&str>> = std::str::from_utf8(tail)
            .ok()
            .map(|text| text.split_ascii_whitespace().collect());
        for pair in words.as_deref().unwrap_or_default().chunks(2) {
            let (slot, value) = match pair {
                ["at", n] if at.is_none() => (&mut at, n),
                ["pin-at", n] if pin_at.is_none() => (&mut pin_at, n),
                _ => return self,
            };
            let Some(value) = number(value) else {
                return self;
            };
            *slot = Some(value);
        }
        match &mut self {
            Self::Reserve { pin, .. } if pin_at.is_none() => pin.at = at,
            Self::InFlight(entry) => {
                entry.at = at;
                entry.pin.at = pin_at;
            }
            _ => {}
        }
        self
    }

    /// The frame a record holds: its body ([`Frame::parse`]) with the heads its tail names.
    fn of_record(body: &[u8], tail: &[u8]) -> Option<Self> {
        Some(Self::parse(body)?.with_tail(tail))
    }

    /// Parse a body back, `None` for anything this build did not write.
    fn parse(body: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(body).ok()?;
        if let Some(entry) = text.strip_prefix("inflight\n") {
            let seq = number(entry.split_whitespace().next()?)?;
            return InFlight::parse(seq, entry).map(Self::InFlight);
        }
        let (head, rest) = text.split_once(' ')?;
        let mut words = rest.splitn(3, ' ');
        match head {
            "reserve" => {
                let sid = words.next().filter(|s| is_sid(s))?;
                let id = number(words.next()?)?;
                let pin = PublishPin::parse(words.next()?).ok()?;
                Some(Self::Reserve {
                    sid: sid.to_string(),
                    id,
                    pin,
                })
            }
            "retire" => {
                let sid = words.next().filter(|s| is_sid(s))?;
                let id = number(words.next()?)?;
                words.next().is_none().then(|| Self::Retire {
                    sid: sid.to_string(),
                    id,
                })
            }
            "landed" => number(rest).map(Self::Landed),
            "dropped" => number(rest).map(Self::Dropped),
            "attached" => number(rest).map(Self::Attached),
            _ => None,
        }
    }
}

/// `frame` as the record at `step`: its body, then its tail (the module's A RECORD'S TAIL). A
/// frame whose record would not read back as itself (a session id the body cannot carry, a
/// reservation its reader refuses) is refused here, so the journal never holds a record its
/// next read takes for damage.
///
/// # Errors
/// That frame (`InvalidInput`), or a record past a step record's payload cap (`InvalidData`).
pub(crate) fn encode(step: u64, frame: &Frame) -> io::Result<Vec<u8>> {
    let (body, tail) = (frame.body(), frame.tail());
    if Frame::of_record(body.as_bytes(), tail.as_bytes()).as_ref() != Some(frame) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "a post journal record would not read back: {}",
                body.lines().next().unwrap_or_default()
            ),
        ));
    }
    let framed = encode_step_header(step, body.len()).and_then(|header| {
        let mut payload = Vec::with_capacity(STEP_HEADER + body.len() + tail.len());
        payload.extend_from_slice(&header);
        payload.extend_from_slice(body.as_bytes());
        payload.extend_from_slice(tail.as_bytes());
        astream_wire::frame::Frame::new(payload).encode()
    });
    framed.map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a post journal record does not frame: {e:?}"),
        )
    })
}

/// The frame of the intact record whose body is `body` in `bytes` (a range a step scan of
/// `bytes` gave): its body read with its tail, the bytes from the body's end to its frame's.
fn record(bytes: &[u8], body: &std::ops::Range<usize>) -> Option<Frame> {
    use astream_wire::frame::decode_frame_header_bytes;
    let start = body.start.checked_sub(HEADER_SIZE + STEP_HEADER)?;
    let header = bytes.get(start..start.checked_add(HEADER_SIZE)?)?;
    let (len, _) = decode_frame_header_bytes(&<[u8; HEADER_SIZE]>::try_from(header).ok()?).ok()?;
    let end = start.checked_add(HEADER_SIZE)?.checked_add(len)?;
    Frame::of_record(bytes.get(body.clone())?, bytes.get(body.end..end)?)
}

/// What the journal holds: every post reserved and not retired, every record in flight that
/// neither landed nor was dropped, and the newest incarnation an attach of this build named.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Index {
    /// By session, then post id.
    pub(crate) reserved: BTreeMap<String, BTreeMap<u64, PublishPin>>,
    /// By sequence.
    pub(crate) in_flight: BTreeMap<u64, InFlight>,
    /// The newest incarnation an attach of this build took ([`Frame::Attached`]); `None` in a
    /// journal no attach named.
    pub(crate) attached: Option<u64>,
}

impl Index {
    /// Nothing reserved, nothing in flight, no attach named.
    pub(crate) const EMPTY: Self = Self {
        reserved: BTreeMap::new(),
        in_flight: BTreeMap::new(),
        attached: None,
    };

    /// Whether it holds no reservation and no record in flight.
    pub(crate) fn holds_nothing(&self) -> bool {
        self.reserved.is_empty() && self.in_flight.is_empty()
    }

    /// The reservation of post `id` of `sid`.
    pub(crate) fn pin(&self, sid: &str, id: u64) -> Option<&PublishPin> {
        self.reserved.get(sid)?.get(&id)
    }

    /// Every reservation, by session then post id.
    pub(crate) fn pins(&self) -> impl Iterator<Item = (&str, u64, &PublishPin)> {
        self.reserved
            .iter()
            .flat_map(|(sid, posts)| posts.iter().map(move |(id, pin)| (sid.as_str(), *id, pin)))
    }

    /// The index after `frame`.
    pub(crate) fn apply(&mut self, frame: Frame) {
        match frame {
            Frame::Reserve { sid, id, pin } => {
                self.reserved.entry(sid).or_default().insert(id, pin);
            }
            Frame::Retire { sid, id } => {
                if let Some(posts) = self.reserved.get_mut(&sid) {
                    posts.remove(&id);
                    if posts.is_empty() {
                        self.reserved.remove(&sid);
                    }
                }
            }
            Frame::InFlight(entry) => {
                self.in_flight.insert(entry.pin.seq, entry);
            }
            Frame::Landed(seq) | Frame::Dropped(seq) => {
                self.in_flight.remove(&seq);
            }
            Frame::Attached(inc) => {
                self.attached = Some(self.attached.map_or(inc, |was| was.max(inc)));
            }
        }
    }

    /// The records a journal holding exactly this index opens with, from `step`: the attach it
    /// names, then every reservation, then every record in flight. Returns the bytes and
    /// the step after the last.
    ///
    /// # Errors
    /// A record past a step record's payload cap ([`encode`]).
    pub(crate) fn encode_from(&self, mut step: u64) -> io::Result<(Vec<u8>, u64)> {
        let mut bytes = Vec::new();
        let attached = self.attached.map(Frame::Attached);
        let reserved = self.pins().map(|(sid, id, pin)| Frame::Reserve {
            sid: sid.to_string(),
            id,
            pin: pin.clone(),
        });
        let in_flight = self.in_flight.values().cloned().map(Frame::InFlight);
        for frame in attached.into_iter().chain(reserved).chain(in_flight) {
            bytes.extend_from_slice(&encode(step, &frame)?);
            step += 1;
        }
        Ok((bytes, step))
    }

    /// The journal that holds exactly this index, its steps from 0 ([`Index::encode_from`]).
    ///
    /// # Errors
    /// A record past a step record's payload cap ([`encode`]).
    pub(crate) fn encode(&self) -> io::Result<(Vec<u8>, u64)> {
        self.encode_from(0)
    }
}

/// How a journal's bytes end ([`read`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum End {
    /// At a record boundary.
    Clean,
    /// In the record the next step holds, cut short: bytes that begin it as it would be written
    /// and stop before its end. What an append that failed part-way leaves, and what a power cut
    /// leaves of an append; the records before it are the journal, and nothing was told of it.
    Cut,
    /// In a broken record with nothing whole after it (astream's torn tail) that is not that
    /// record cut short: a tear inside bytes the device was given, which a power cut can leave and
    /// nothing within a boot does, or damage.
    Torn,
    /// Damage: a record this build did not write, a step out of order, or a broken record with a
    /// whole one after it. Nothing a crash leaves; the bytes from `at` on are not to be dropped
    /// as torn.
    Fault {
        /// Where, in bytes.
        at: u64,
        /// What, for the person who repairs it.
        why: String,
    },
}

/// What a read of a journal's bytes found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Read {
    /// What the intact records say.
    pub(crate) index: Index,
    /// Where the intact records end.
    pub(crate) valid_len: u64,
    /// The step the next record carries.
    pub(crate) next_step: u64,
    /// What ended them.
    pub(crate) end: End,
}

/// Read a journal's bytes: the longest intact run of records from step 0
/// ([`astream_wire::steps::scan_steps_from`]), applied in order, and what ended it. An intact
/// record whose body this build cannot read ends the run as a fault at that record.
#[must_use]
pub(crate) fn read(bytes: &[u8]) -> Read {
    let scan = scan_steps_from(bytes, 0);
    let mut index = Index::default();
    let mut next_step = 0u64;
    for (step, body) in &scan.steps {
        let Some(frame) = record(bytes, body) else {
            let at = body.start.saturating_sub(HEADER_SIZE + STEP_HEADER) as u64;
            return Read {
                index,
                valid_len: at,
                next_step,
                end: End::Fault {
                    at,
                    why: format!("record {step} is not a post journal record this build reads"),
                },
            };
        };
        index.apply(frame);
        next_step = step.saturating_add(1);
    }
    let valid_len = scan.valid_len as u64;
    let end = match scan.end {
        StepEnd::Clean => End::Clean,
        StepEnd::Torn if cut_short(&bytes[scan.valid_len..], next_step) => End::Cut,
        StepEnd::Torn => End::Torn,
        StepEnd::Fault { at, kind } => End::Fault {
            at: at as u64,
            why: kind.to_string(),
        },
    };
    Read {
        index,
        valid_len,
        next_step,
        end,
    }
}

/// WHAT EVERY WHOLE RECORD OF A DAMAGED JOURNAL SAYS, for the sweep after an unclean stop in an
/// earlier boot ([`super::StateDir::set_aside_torn`]): the intact run from step 0 ([`read`]),
/// then, past each damaged or unreadable record, the intact run from the next offset where a
/// record of a later step decodes, each record applied in order. A fault is never a crash's
/// tail, so the whole records after it are kept, not dropped as torn (astream's
/// [`astream_wire::steps`]: truncating at a fault destroys records that were acknowledged).
/// What the damaged bytes held is unknown: a reservation or a record in flight they held is
/// missing, and a retirement, landing or drop they held leaves its record in place.
#[must_use]
pub(crate) fn salvage(bytes: &[u8]) -> Index {
    use astream_wire::steps::{RESYNC_CRC_FLOOR, RESYNC_CRC_PER_BYTE};
    let mut index = Index::default();
    let mut at = 0usize;
    let mut first = 0u64;
    let mut budget = RESYNC_CRC_PER_BYTE
        .saturating_mul(bytes.len())
        .max(RESYNC_CRC_FLOOR);
    while let Some(rest) = bytes.get(at..) {
        let scan = scan_steps_from(rest, first);
        let mut resume_from = at.saturating_add(scan.valid_len);
        let mut stopped_early = false;
        for (step, body) in &scan.steps {
            let Some(frame) = record(rest, body) else {
                let start = body.start.saturating_sub(HEADER_SIZE + STEP_HEADER);
                resume_from = at.saturating_add(start).saturating_add(1);
                stopped_early = true;
                break;
            };
            index.apply(frame);
            first = step.saturating_add(1);
        }
        if !stopped_early && scan.end == StepEnd::Clean {
            break;
        }
        let Some((next, step)) = next_record(bytes, resume_from, first, &mut budget) else {
            break;
        };
        at = next;
        first = step;
    }
    index
}

/// The first offset at or after `from` where a whole step record of step `first` or later
/// starts, and its step. The bytes past damage are whatever the damage left, which may hold what
/// reads as a frame header at many offsets, each naming a length near the whole: each such
/// candidate is charged its length before its CRC runs, against `budget` (astream's
/// [`astream_wire::steps::RESYNC_CRC_PER_BYTE`] a byte of the journal, at least
/// [`astream_wire::steps::RESYNC_CRC_FLOOR`], as its own search past a break is bounded), and a
/// search past it finds nothing, so the salvage keeps what it found before.
fn next_record(bytes: &[u8], from: usize, first: u64, budget: &mut usize) -> Option<(usize, u64)> {
    use astream_wire::frame::decode_frame_header_bytes;
    for p in from..bytes.len() {
        if bytes.get(p) != Some(&MAGIC[0]) {
            continue;
        }
        let header = bytes.get(p..p.checked_add(HEADER_SIZE)?)?;
        let Ok((len, _)) = decode_frame_header_bytes(&<[u8; HEADER_SIZE]>::try_from(header).ok()?)
        else {
            continue;
        };
        if p.saturating_add(HEADER_SIZE).saturating_add(len) > bytes.len() {
            continue;
        }
        *budget = budget.checked_sub(len)?;
        let Ok(Some((len, after))) = validate_frame_at(bytes.get(p..)?) else {
            continue;
        };
        if let Some((step, _)) = decode_step_header(after, len) {
            if step >= first {
                return Some((p, step));
            }
        }
    }
    None
}

/// How many of a journal's intact records name an attach and how many record anything else: a
/// test's count of the writes a post cost, where the first record after an attach carries the
/// attach's name in the same write ([`super::StateDir::append_post_frame`]).
#[cfg(test)]
pub(crate) fn records_and_attaches(bytes: &[u8]) -> (u64, u64) {
    let scan = scan_steps_from(bytes, 0);
    let attaches = scan
        .steps
        .iter()
        .filter(|(_, body)| matches!(record(bytes, body), Some(Frame::Attached(_))))
        .count() as u64;
    (scan.steps.len() as u64 - attaches, attaches)
}

/// Whether `tail` is the record at `step` cut short: non-empty, as much of a frame header as it
/// holds is the one this journal writes (the magic, the version, no flags), a whole header names
/// a length past the bytes there, and as much of the step header as it holds names `step`.
fn cut_short(tail: &[u8], step: u64) -> bool {
    use astream_wire::frame::{decode_frame_header_bytes, MAGIC, VERSION};
    let head = [MAGIC[0], MAGIC[1], VERSION, 0];
    if tail.is_empty() || !head.starts_with(&tail[..tail.len().min(head.len())]) {
        return false;
    }
    let Some(header) = tail.get(..HEADER_SIZE) else {
        return true;
    };
    let Ok(header) = <[u8; HEADER_SIZE]>::try_from(header) else {
        return false;
    };
    let Ok((len, _)) = decode_frame_header_bytes(&header) else {
        return false;
    };
    if HEADER_SIZE.saturating_add(len) <= tail.len() {
        return false;
    }
    let mut expected = vec![astream_wire::steps::STEP_VERSION];
    expected.extend_from_slice(&step.to_le_bytes());
    let present = &tail[HEADER_SIZE..];
    expected.starts_with(&present[..present.len().min(expected.len())])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PublishIdentity;
    use astream_wire::steps::encode_step;

    fn pin(seq: u64) -> PublishPin {
        PublishPin {
            seq,
            via_key: false,
            identity: Some(PublishIdentity {
                subject: "/f/sim/in/n-1/s-b/n-1/s-a/note".to_string(),
                digest: "a".repeat(64),
            }),
            original: None,
            original_off: None,
            owner: Some(4),
            at: Some(40 + seq),
        }
    }

    fn entry(seq: u64) -> InFlight {
        InFlight {
            pin: pin(seq),
            subject: "/f/sim/in/n-1/s-b/n-1/s-a/note".to_string(),
            body: b"v=1 t=5 text=hello".to_vec(),
            log: astream_broker::LogIdentity::parse(&format!("{}.3", "5a".repeat(16))),
            at: Some(50 + seq),
        }
    }

    fn journal(frames: &[Frame]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (step, frame) in frames.iter().enumerate() {
            bytes.extend(encode(step as u64, frame).expect("frames"));
        }
        bytes
    }

    fn every_kind() -> Vec<Frame> {
        vec![
            Frame::Attached(11),
            Frame::Attached(13),
            Frame::Attached(12),
            Frame::Reserve {
                sid: "s-a".to_string(),
                id: 1,
                pin: pin(7),
            },
            Frame::InFlight(entry(7)),
            Frame::Reserve {
                sid: "s-a".to_string(),
                id: 2,
                pin: pin(8),
            },
            Frame::InFlight(entry(8)),
            Frame::Landed(7),
            Frame::Retire {
                sid: "s-a".to_string(),
                id: 1,
            },
            Frame::InFlight(InFlight {
                log: None,
                ..entry(9)
            }),
            Frame::Dropped(9),
        ]
    }

    /// Every kind of record reads back as written, the index is what they say in order, and a
    /// rewrite from the index reads back to the same index.
    #[test]
    fn every_record_round_trips_and_a_rewrite_holds_the_same_index() {
        let frames = every_kind();
        for frame in &frames {
            assert_eq!(
                Frame::of_record(frame.body().as_bytes(), frame.tail().as_bytes()).as_ref(),
                Some(frame)
            );
        }
        let bytes = journal(&frames);
        let read = read(&bytes);
        assert_eq!(read.end, End::Clean);
        assert_eq!(read.valid_len, bytes.len() as u64);
        assert_eq!(read.next_step, frames.len() as u64);
        let mut expected = Index::default();
        expected.apply(Frame::Reserve {
            sid: "s-a".to_string(),
            id: 2,
            pin: pin(8),
        });
        expected.in_flight.insert(8, entry(8));
        expected.attached = Some(13);
        assert_eq!(read.index, expected);
        let (rewritten, steps) = read.index.encode().expect("rewrite");
        assert_eq!(
            steps, 3,
            "the newest attach, a reservation and a record in flight"
        );
        let again = super::read(&rewritten);
        assert_eq!((again.index, again.end), (expected, End::Clean));
        assert!(
            rewritten.len() < bytes.len(),
            "the rewrite holds only what is live"
        );
    }

    /// The read of a build of the journal before the heads (B19's, on aterm main at 19f6a3e034):
    /// each record's body by astream's step scan, parsed strictly, its tail never looked at.
    fn read_as_before_the_heads(bytes: &[u8]) -> Option<Index> {
        let scan = scan_steps_from(bytes, 0);
        let mut index = Index::default();
        for (_, body) in &scan.steps {
            index.apply(Frame::parse(bytes.get(body.clone())?)?);
        }
        (scan.end == StepEnd::Clean).then_some(index)
    }

    /// **A JOURNAL THIS BUILD WROTE READS, RECORD FOR RECORD, TO A BUILD BEFORE THE HEADS**
    /// (aterm-link B05 review): the heads ride in each record's tail, which astream's step scan
    /// leaves out of the body, so the strict parse of an earlier build reads every record as the
    /// one written, without its heads, and a rewrite from the index too; the text form never
    /// names a head. CONTROL: this build reads the heads back from the same bytes.
    #[test]
    fn a_journal_with_heads_reads_record_for_record_to_a_build_before_them() {
        let frames = every_kind();
        let headless = |frame: &Frame| match frame {
            Frame::Reserve { sid, id, pin } => Frame::Reserve {
                sid: sid.clone(),
                id: *id,
                pin: pin.without_head(),
            },
            Frame::InFlight(entry) => Frame::InFlight(entry.without_head()),
            other => other.clone(),
        };
        assert!(frames.iter().any(|frame| *frame != headless(frame)));
        for frame in &frames {
            assert_eq!(
                Frame::parse(frame.body().as_bytes()),
                Some(headless(frame)),
                "an earlier build's parse of the body alone"
            );
            assert!(!frame.body().contains(" at ") && !frame.body().contains("\nat="));
        }
        assert_eq!(
            pin(7).render(),
            format!(
                "7 identity /f/sim/in/n-1/s-b/n-1/s-a/note {} owner 4",
                "a".repeat(64)
            ),
            "the reservation's text form is B19's"
        );
        let bytes = journal(&frames);
        let mut headless_index = Index::default();
        for frame in &frames {
            headless_index.apply(headless(frame));
        }
        assert_eq!(
            read_as_before_the_heads(&bytes),
            Some(headless_index.clone())
        );
        let (rewritten, _) = read(&bytes).index.encode().expect("rewrite");
        assert_eq!(
            read_as_before_the_heads(&rewritten),
            Some(headless_index),
            "a rewrite from the index, without its heads"
        );
        let mut with_heads = Index::default();
        for frame in &frames {
            with_heads.apply(frame.clone());
        }
        assert_eq!(
            read(&bytes).index,
            with_heads,
            "CONTROL: this build reads the heads"
        );
        assert_eq!(read(&rewritten).index, with_heads);
    }

    /// A tail this build cannot read names no head: the record reads as its body alone.
    #[test]
    fn a_tail_this_build_cannot_read_names_no_head() {
        let entry = Frame::InFlight(entry(7));
        for tail in ["at", "at x", "at 1 at 2", "pin-at 3 later 4", "\u{ff}"] {
            assert_eq!(
                Frame::of_record(entry.body().as_bytes(), tail.as_bytes()),
                Frame::parse(entry.body().as_bytes()),
                "{tail:?}"
            );
        }
        assert_eq!(
            Frame::of_record(entry.body().as_bytes(), b"at 57 pin-at 47"),
            Some(entry),
            "CONTROL"
        );
    }

    /// A cut anywhere in the last record keeps every record before it and reads as that record
    /// cut short; a broken last record that is not one cut short is torn; damage before a whole
    /// record is a fault. CONTROL: an intact journal is clean at every record boundary.
    #[test]
    fn a_cut_last_record_is_cut_a_broken_one_torn_and_damage_before_a_whole_one_a_fault() {
        let frames = every_kind();
        let bytes = journal(&frames);
        let last_start = journal(&frames[..frames.len() - 1]).len();
        let before = super::read(&bytes[..last_start]);
        assert_eq!(before.end, End::Clean, "CONTROL: a boundary is clean");
        for cut in last_start + 1..bytes.len() {
            let read = super::read(&bytes[..cut]);
            assert_eq!(read.end, End::Cut, "cut at {cut}");
            assert_eq!(read.index, before.index, "cut at {cut}");
            assert_eq!(read.valid_len, last_start as u64);
        }
        for at in [last_start, last_start + 2, last_start + 13, bytes.len() - 1] {
            let mut broken = bytes.clone();
            broken[at] ^= 0x01;
            let read = super::read(&broken);
            assert_eq!(read.end, End::Torn, "the last record broken at {at}");
            assert_eq!(read.index, before.index);
        }
        let mut wrong_step = journal(&frames[..frames.len() - 1]);
        let next = encode(frames.len() as u64, frames.last().expect("frames")).expect("frames");
        wrong_step.extend_from_slice(&next[..next.len() - 1]);
        assert_eq!(
            super::read(&wrong_step).end,
            End::Torn,
            "a cut record that names another step is not this one cut short"
        );
        assert_eq!(super::read(b"not a journal entry\n").end, End::Torn);
        let mut damaged = bytes.clone();
        damaged[20] ^= 0x40;
        assert!(
            matches!(super::read(&damaged).end, End::Fault { at: 0, .. }),
            "a flipped byte in the first record, whole records after it"
        );
        let mut foreign = journal(&frames[..2]);
        let at = foreign.len() as u64;
        foreign.extend(encode_step(2, b"unknown 5").expect("frames"));
        let read = super::read(&foreign);
        assert_eq!(read.valid_len, at);
        assert!(
            matches!(read.end, End::Fault { at: a, .. } if a == at),
            "a record this build did not write: {:?}",
            read.end
        );
        let mut out_of_order = journal(&frames[..2]);
        out_of_order.extend(encode(5, &frames[2]).expect("frames"));
        assert!(matches!(super::read(&out_of_order).end, End::Fault { .. }));
    }

    /// A record's body is text, so a session's text that holds the frame magic cannot make a cut
    /// read as a fault: the body is hex.
    #[test]
    fn a_body_holding_a_whole_frame_still_reads_cut_when_cut() {
        let nested = encode(0, &Frame::Landed(3)).expect("frames");
        let frame = Frame::InFlight(InFlight {
            body: nested,
            ..entry(11)
        });
        let bytes = journal(std::slice::from_ref(&frame));
        for cut in 1..bytes.len() {
            assert_eq!(super::read(&bytes[..cut]).end, End::Cut, "cut at {cut}");
        }
        assert!(!frame.body().bytes().any(|b| b >= 0x80));
    }

    /// THE SWEEP'S SALVAGE: damage before whole records keeps every whole record after it, in
    /// order, and loses only what the damaged record said (here a landing, so its record in
    /// flight stays); an intact record whose body this build does not read is passed over the
    /// same way. CONTROL: an intact journal, and one whose last record is cut, salvage to what
    /// their read keeps.
    #[test]
    fn a_salvage_keeps_every_whole_record_after_damage() {
        let frames = every_kind();
        let bytes = journal(&frames);
        let whole = super::read(&bytes).index;
        assert_eq!(salvage(&bytes), whole, "CONTROL: an intact journal");
        let cut = &bytes[..bytes.len() - 3];
        assert_eq!(
            salvage(cut),
            super::read(cut).index,
            "CONTROL: a cut last record"
        );
        let landed = frames
            .iter()
            .position(|f| *f == Frame::Landed(7))
            .expect("a landing");
        let start = journal(&frames[..landed]).len();
        let mut damaged = bytes.clone();
        damaged[start + HEADER_SIZE + STEP_HEADER + 2] ^= 0x01;
        let read = super::read(&damaged);
        assert!(matches!(read.end, End::Fault { .. }), "{:?}", read.end);
        assert!(
            read.index.pin("s-a", 1).is_some(),
            "the read stops at the damage, before the retirement after it"
        );
        let mut expected = whole.clone();
        expected.in_flight.insert(7, entry(7));
        assert_eq!(salvage(&damaged), expected);
        let mut first = bytes.clone();
        first[20] ^= 0x40;
        assert!(matches!(super::read(&first).end, End::Fault { at: 0, .. }));
        assert_eq!(
            salvage(&first),
            whole,
            "the first record's attach is below a later one"
        );
        let mut foreign = journal(&frames[..2]);
        foreign.extend(encode_step(2, b"unknown 5").expect("frames"));
        for (step, frame) in frames[2..].iter().enumerate() {
            foreign.extend(encode(step as u64 + 3, frame).expect("frames"));
        }
        assert!(matches!(super::read(&foreign).end, End::Fault { .. }));
        assert_eq!(salvage(&foreign), whole);
    }

    /// THE SALVAGE'S SEARCH PAST DAMAGE IS BOUNDED: damage that reads as a frame header every
    /// twelve bytes, each naming a length to near the end, spends the search's CRC budget, and
    /// the salvage keeps what it found before the damage rather than hash every candidate's
    /// length (a search that would cost about the square of the bytes). CONTROL: the same whole
    /// record after a few such headers is found.
    #[test]
    fn a_salvage_search_past_damage_is_bounded() {
        use astream_wire::frame::{MAGIC, VERSION};
        let frames = every_kind();
        let before = journal(&frames[..4]);
        let after = encode(9, &frames[4]).expect("a record");
        let lookalikes = |n: usize, total: usize| {
            let mut bytes = Vec::new();
            for k in 0..n {
                let len = (total - 12 * (k + 1)) as u32;
                bytes.extend_from_slice(&[MAGIC[0], MAGIC[1], VERSION, 0]);
                bytes.extend_from_slice(&len.to_le_bytes());
                bytes.extend_from_slice(&[0, 0, 0, 0]);
            }
            bytes
        };
        let salvaged = |n: usize, total: usize| {
            let mut bytes = before.clone();
            bytes.extend(lookalikes(n, total));
            bytes.resize(before.len() + total, b' ');
            bytes.extend_from_slice(&after);
            salvage(&bytes)
        };
        let prefix = super::read(&before).index;
        let mut found = prefix.clone();
        found.apply(frames[4].clone());
        assert_eq!(
            salvaged(3, 4096),
            found,
            "CONTROL: a few look-alikes passed over"
        );
        let total = 2 * 1024 * 1024;
        assert_eq!(
            salvaged(total / 12 - 1, total),
            prefix,
            "the budget spent, what came before the damage kept"
        );
    }
}
