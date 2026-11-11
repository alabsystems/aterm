// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE POST JOURNAL: one append-only file per state dir, [`FILE`], in place of a file per post
//! reservation (`sent/<sid>.<id>`) and per own-lane record in flight (`inflight/<seq>`).
//!
//! A post cost the state dir two replaces (a temp written and renamed over the target) and two
//! unlinks; it costs this journal four records appended to one held descriptor (B19, after B10
//! ranked the state dir's writes at 36% of an ordered request/reply round trip and 46% of a
//! relaxed one), its landing and its retirement in one write with every other post its ack pass
//! landed (critic item E07, [`super::StateDir::settle_posts`]).
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
//!
//! ONE CODEC, MORE THAN ONE JOURNAL: what a fact's record is ([`Record`]) and what a run of them
//! says ([`Replay`]) are the only things a journal of its own supplies; the framing, the read
//! and its ends, and the salvage past damage are this module's ([`encode`], [`read_as`],
//! [`salvage_as`]). The asks journal ([`super::asks`]) is the other one: a file of its own,
//! because a build of this journal reads [`FILE`] strictly and ends at a record it does not
//! know as a fault ([`read`]).

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

/// One fact a journal records, as the record it is written as: a body of ASCII text, so no
/// body can hold an astream frame of its own, and a tail every reader of the body skips (the
/// module's A RECORD'S TAIL).
pub(crate) trait Record: Sized + PartialEq {
    /// The journal's name in a refusal or a fault: "post journal".
    const JOURNAL: &'static str;

    /// The body this fact is recorded as.
    fn body(&self) -> String;

    /// The tail it is recorded with.
    fn tail(&self) -> String;

    /// The fact a record holds, `None` for a body this build did not write. A tail this build
    /// cannot read adds nothing to the fact.
    fn of_record(body: &[u8], tail: &[u8]) -> Option<Self>;
}

/// What a journal's records say, applied in the order they were written.
pub(crate) trait Replay: Default {
    /// The fact each record holds.
    type Record: Record;

    /// What it says after `record`.
    fn apply(&mut self, record: Self::Record);
}

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
    /// ([`super::StateDir::clear_post_seq`], [`super::StateDir::settle_posts`]), or once its
    /// session ended in an earlier boot ([`super::StateDir::sweep_sessions_of_an_earlier_boot`]).
    Retire { sid: String, id: u64 },
    /// A record in flight onto this node's own lanes ([`super::StateDir::set_in_flight`]); a later
    /// one at the same sequence replaces it.
    InFlight(InFlight),
    /// The record in flight at this sequence landed ([`super::StateDir::clear_in_flight`]); a
    /// post's, once aterm answered its `outbox sent` ([`super::StateDir::settle_posts`]).
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

/// A word of ASCII digits as a number, `None` for anything else (a sign, a space, an overflow).
pub(super) fn number(word: &str) -> Option<u64> {
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

    /// This frame with the heads `tail` names (see [`Frame`]'s [`Record::tail`]); a tail this
    /// build cannot read names none.
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

impl Record for Frame {
    const JOURNAL: &'static str = "post journal";

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

    /// Its heads, as words (the module's A RECORD'S TAIL).
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

    /// Its body ([`Frame::parse`]) with the heads its tail names.
    fn of_record(body: &[u8], tail: &[u8]) -> Option<Self> {
        Some(Self::parse(body)?.with_tail(tail))
    }
}

/// `frame` as the record at `step`: its body, then its tail (the module's A RECORD'S TAIL). A
/// frame whose record would not read back as itself (a session id the body cannot carry, a
/// reservation its reader refuses) is refused here, so the journal never holds a record its
/// next read takes for damage.
///
/// # Errors
/// That frame (`InvalidInput`), or a record past a step record's payload cap (`InvalidData`).
pub(crate) fn encode<R: Record>(step: u64, frame: &R) -> io::Result<Vec<u8>> {
    let (body, tail) = (frame.body(), frame.tail());
    if R::of_record(body.as_bytes(), tail.as_bytes()).as_ref() != Some(frame) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "a {} record would not read back: {}",
                R::JOURNAL,
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
            format!("a {} record does not frame: {e:?}", R::JOURNAL),
        )
    })
}

/// The fact of the intact record whose body is `body` in `bytes` (a range a step scan of
/// `bytes` gave): its body read with its tail, the bytes from the body's end to its frame's.
fn record<R: Record>(bytes: &[u8], body: &std::ops::Range<usize>) -> Option<R> {
    use astream_wire::frame::decode_frame_header_bytes;
    let start = body.start.checked_sub(HEADER_SIZE + STEP_HEADER)?;
    let header = bytes.get(start..start.checked_add(HEADER_SIZE)?)?;
    let (len, _) = decode_frame_header_bytes(&<[u8; HEADER_SIZE]>::try_from(header).ok()?).ok()?;
    let end = start.checked_add(HEADER_SIZE)?.checked_add(len)?;
    R::of_record(bytes.get(body.clone())?, bytes.get(body.end..end)?)
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

impl Replay for Index {
    type Record = Frame;

    fn apply(&mut self, record: Frame) {
        Self::apply(self, record);
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
pub(crate) struct Read<I = Index> {
    /// What the intact records say.
    pub(crate) index: I,
    /// Where the intact records end.
    pub(crate) valid_len: u64,
    /// The step the next record carries.
    pub(crate) next_step: u64,
    /// What ended them.
    pub(crate) end: End,
}

/// Read the post journal's bytes ([`read_as`]).
#[must_use]
pub(crate) fn read(bytes: &[u8]) -> Read {
    read_as(bytes)
}

/// Read a journal's bytes: the longest intact run of records from step 0
/// ([`astream_wire::steps::scan_steps_from`]), applied in order, and what ended it. An intact
/// record whose body this build cannot read ends the run as a fault at that record.
#[must_use]
pub(crate) fn read_as<I: Replay>(bytes: &[u8]) -> Read<I> {
    let scan = scan_steps_from(bytes, 0);
    let mut index = I::default();
    let mut next_step = 0u64;
    for (step, body) in &scan.steps {
        let Some(frame) = record::<I::Record>(bytes, body) else {
            let at = body.start.saturating_sub(HEADER_SIZE + STEP_HEADER) as u64;
            return Read {
                index,
                valid_len: at,
                next_step,
                end: End::Fault {
                    at,
                    why: format!(
                        "record {step} is not a {} record this build reads",
                        <I::Record as Record>::JOURNAL
                    ),
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
    salvage_as(bytes)
}

/// What every whole record of a damaged journal says ([`salvage`]), for any journal.
#[must_use]
pub(crate) fn salvage_as<I: Replay>(bytes: &[u8]) -> I {
    use astream_wire::steps::{RESYNC_CRC_FLOOR, RESYNC_CRC_PER_BYTE};
    let mut index = I::default();
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
            let Some(frame) = record::<I::Record>(rest, body) else {
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
#[path = "journal_tests.rs"]
mod tests;
