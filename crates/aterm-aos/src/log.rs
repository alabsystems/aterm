// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ordered input log of one hosted task, from one checkpoint onward.
//!
//! # The law
//!
//! A log is the complete, ordered list of the inputs a task was delivered after its
//! ancestor checkpoint: input `k` sits at cursor `first_cursor + k`. Only inputs whose step
//! succeeded are in it (a refused input left the state untouched, so it is as if it never
//! arrived). A log that cannot be complete — an input that could not be recorded, a host
//! that broke the admit/advance protocol — is marked *broken*, and a broken log answers no
//! range ([`RangeFault::Broken`]), so no repair can replay across the gap. The log is bounded
//! (`max_events`): a full log refuses the push ([`LogFull`]) and its owner re-anchors at a
//! fresh checkpoint rather than dropping anything.
//!
//! # The encoding
//!
//! A log is a run of `astream_wire::steps` records, each a CRC-32 frame around
//! `STEP_VERSION | step u64 | len u32 | body`; the steps are dense from 0. Every body is
//! `RECORD_VERSION: u8 | kind: u8 | rest`:
//!
//! * step 0 is the header (kind 0): [`LOG_MAGIC`] (whose last byte is the format), the task
//!   identity, the log epoch, the first cursor, the ancestor checkpoint's digest, and the
//!   broken flag;
//! * steps `1..=n` are events (kind 1): one canonically encoded input each;
//! * step `n + 1` is the seal (kind 2): the event count `n` and the digest of the whole log
//!   (the trace digest of `[first_cursor, end_cursor)`, taken whatever the broken flag says).
//!
//! [`EventLog::decode`] keeps the longest intact prefix (astream's classifier):
//!
//! * a short or damaged **last** record with nothing whole after it is a torn tail: it is
//!   dropped and **reported** ([`Tail::Torn`]), never silently;
//! * bytes that end exactly at a record boundary with no seal as the last record are a cut
//!   log: **reported** ([`Tail::Unsealed`]), never read as whole. Only the bytes of a whole
//!   log, ending in a seal that names its event count and digest, decode [`Tail::Clean`];
//! * a seal anywhere but last ([`LogFault::Kind`]), or one that names another count
//!   ([`LogFault::SealCount`]) or digest ([`LogFault::SealDigest`]), is a fault;
//! * damage followed by a whole record, a gap, a repeat or a reordered step is a fault, and
//!   the log is refused ([`LogFault::Step`]), never truncated;
//! * a record whose frame holds bytes past its body is refused ([`WireFault::Trailing`]), so
//!   every log has exactly one encoding.
//!
//! The header's identity is checked against the identity the reader expects, and a mismatch
//! names which artifact differs ([`IdentityMismatch`]); its ancestor against the digest of the
//! checkpoint the reader holds beside it ([`LogFault::Ancestor`]), so a log never replays from
//! a checkpoint it does not extend. The event count is checked against `max_events` before
//! anything is allocated for the inputs. Decoding is a pure, panic-free function of the
//! bytes.

use core::fmt;

use astream_wire::frame::{HEADER_SIZE, MAGIC as FRAME_MAGIC, MAX_PAYLOAD_LEN};
use astream_wire::steps::{STEP_HEADER, StepEnd, encode_step, scan_steps_from};

use crate::ident::{Digest, DigestBuilder, TaskIdentity, len_u64};
use crate::task::Schema;
use crate::wire::{Dec, Enc, WireFault};

/// The header record's magic; its last byte is the format (2: the header names the ancestor
/// checkpoint and the log ends in a seal).
pub const LOG_MAGIC: [u8; 8] = *b"AOSLOG\0\x02";

/// Every record body starts with this version byte.
pub const RECORD_VERSION: u8 = 1;

/// The domain under which [`EventLog::trace_digest`] digests a range of inputs.
pub const TRACE_DOMAIN: &[u8] = b"aterm-aos-trace-v2\0";

const KIND_HEADER: u8 = 0;
const KIND_EVENT: u8 = 1;
const KIND_SEAL: u8 = 2;

/// Where a record's body starts, counted from the start of its frame.
const BODY_OFFSET: usize = HEADER_SIZE + STEP_HEADER;

/// The largest body a step record can carry.
const MAX_BODY: usize = MAX_PAYLOAD_LEN - STEP_HEADER;

/// The identity and place of one log epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogHeader {
    /// The task identity the inputs were delivered under.
    pub identity: TaskIdentity,
    /// The log epoch (a new one after every re-anchor or commit).
    pub log_epoch: u64,
    /// The cursor of the first input (the ancestor checkpoint's input cursor).
    pub first_cursor: u64,
    /// The ancestor checkpoint's digest (`Checkpoint::digest`): the state the first input is
    /// applied to. A reader holds the log beside that checkpoint or refuses it.
    pub ancestor: Digest,
}

/// Which artifact of two identities differs (the first, in the order task, code, contract,
/// schema, profile).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IdentityMismatch {
    /// The task differs.
    Task,
    /// The code differs.
    Code,
    /// The contract differs.
    Contract,
    /// The schema differs.
    Schema,
    /// The execution profile differs.
    Profile,
}

impl IdentityMismatch {
    /// The first artifact `found` and `expect` disagree on, or `None` if they are equal.
    #[inline]
    #[must_use]
    pub fn first(found: &TaskIdentity, expect: &TaskIdentity) -> Option<IdentityMismatch> {
        if found.task != expect.task {
            Some(IdentityMismatch::Task)
        } else if found.code != expect.code {
            Some(IdentityMismatch::Code)
        } else if found.contract != expect.contract {
            Some(IdentityMismatch::Contract)
        } else if found.schema != expect.schema {
            Some(IdentityMismatch::Schema)
        } else if found.profile != expect.profile {
            Some(IdentityMismatch::Profile)
        } else {
            None
        }
    }

    #[inline]
    const fn word(self) -> &'static str {
        match self {
            IdentityMismatch::Task => "task",
            IdentityMismatch::Code => "code",
            IdentityMismatch::Contract => "contract",
            IdentityMismatch::Schema => "schema",
            IdentityMismatch::Profile => "profile",
        }
    }
}

impl fmt::Display for IdentityMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the {} artifact differs from the one expected",
            self.word()
        )
    }
}

impl std::error::Error for IdentityMismatch {}

/// A push the log refused because it holds `limit` inputs (or its cursors would leave the
/// `u64` range). The log is untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogFull {
    /// The most inputs the log holds.
    pub limit: usize,
}

impl fmt::Display for LogFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the log already holds the {} inputs it may hold",
            self.limit
        )
    }
}

impl std::error::Error for LogFull {}

/// How a decoded log ended. Only [`Tail::Clean`] is a whole log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tail {
    /// The bytes ended exactly after a seal naming every event before it: the whole log.
    Clean,
    /// A short or damaged last record, with nothing whole after it, was dropped. The seal may
    /// have been in it: the events returned may be all of the log's, or a prefix.
    Torn {
        /// The byte offset where the dropped bytes start.
        at: usize,
        /// How many bytes were dropped.
        dropped: usize,
    },
    /// The bytes ended exactly at a record boundary, but the last record is not a seal: the
    /// log was cut after its `events`-th event, and how many followed is unknown.
    Unsealed {
        /// The events the cut kept.
        events: usize,
    },
}

/// Why bytes are not a log this reader accepts. Nothing is returned from them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogFault {
    /// The bytes do not start with an Aos log header.
    NotALog,
    /// An Aos log of another format (the magic's last byte).
    Format(u8),
    /// A record of another [`RECORD_VERSION`].
    RecordVersion(u8),
    /// Damage followed by a whole record, a gap, a repeat or a reorder (astream's
    /// `StepFault`, rendered).
    Step(String),
    /// No whole header record.
    MissingHeader,
    /// The header names another identity than the one expected.
    Identity(IdentityMismatch),
    /// More events than the reader accepts.
    TooManyEvents {
        /// The most the reader accepts.
        limit: usize,
    },
    /// A record body that is not the canonical encoding of its value.
    Wire(WireFault),
    /// The header names another ancestor checkpoint than the one the reader holds.
    Ancestor {
        /// The digest of the checkpoint the reader holds.
        expected: Digest,
        /// The ancestor the header names.
        found: Digest,
    },
    /// The seal names another event count than the events before it.
    SealCount {
        /// The count the seal names.
        sealed: u64,
        /// The events before it.
        found: u64,
    },
    /// The seal names another digest than the digest of the events before it.
    SealDigest {
        /// The digest the seal names.
        sealed: Digest,
        /// The digest of the decoded log.
        found: Digest,
    },
    /// A record of a kind that cannot stand at step `at` (a header past step 0, a seal that
    /// is not the last record, an unknown kind).
    Kind {
        /// The step.
        at: u64,
        /// The kind byte found.
        kind: u8,
    },
    /// Encoding only: a record at step `at` whose body of `len` bytes exceeds the `limit` a
    /// step record can carry. Nothing was written.
    TooLarge {
        /// The step.
        at: u64,
        /// The body length.
        len: usize,
        /// The most a record body may hold.
        limit: usize,
    },
}

impl fmt::Display for LogFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogFault::NotALog => f.write_str("the bytes do not start with an Aos log header"),
            LogFault::Format(b) => write!(
                f,
                "the log is of format {b}, which this build does not read"
            ),
            LogFault::RecordVersion(v) => {
                write!(
                    f,
                    "a log record is of version {v}, which this build does not read"
                )
            }
            LogFault::Step(s) => write!(f, "the log's records are damaged or out of order: {s}"),
            LogFault::MissingHeader => f.write_str("the log holds no whole header record"),
            LogFault::Identity(m) => write!(f, "the log was recorded under another identity: {m}"),
            LogFault::TooManyEvents { limit } => {
                write!(
                    f,
                    "the log holds more than the {limit} events this reader accepts"
                )
            }
            LogFault::Wire(w) => write!(f, "a log record is not canonical: {w}"),
            LogFault::Ancestor { expected, found } => write!(
                f,
                "the log extends checkpoint {}, not the checkpoint {} beside it",
                found.short(),
                expected.short()
            ),
            LogFault::SealCount { sealed, found } => write!(
                f,
                "the log's seal names {sealed} events and {found} precede it"
            ),
            LogFault::SealDigest { sealed, found } => write!(
                f,
                "the log's seal names digest {} and its events digest to {}",
                sealed.short(),
                found.short()
            ),
            LogFault::Kind { at, kind } => {
                write!(
                    f,
                    "the record at step {at} is of kind {kind}, which cannot stand there"
                )
            }
            LogFault::TooLarge { at, len, limit } => write!(
                f,
                "the record at step {at} has a {len}-byte body, more than the {limit} a record carries"
            ),
        }
    }
}

impl std::error::Error for LogFault {}

impl From<WireFault> for LogFault {
    #[inline]
    fn from(w: WireFault) -> Self {
        LogFault::Wire(w)
    }
}

/// Why a range of cursors cannot be read from a log. The log is untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeFault {
    /// `from` is past `to`.
    Inverted {
        /// The start asked for.
        from: u64,
        /// The end asked for.
        to: u64,
    },
    /// `from` precedes the log's first cursor.
    BeforeStart {
        /// The start asked for.
        from: u64,
        /// The log's first cursor.
        first: u64,
    },
    /// `to` is past the log's end cursor.
    PastEnd {
        /// The end asked for.
        to: u64,
        /// The log's end cursor.
        end: u64,
    },
    /// The log is broken: some delivered input is missing from it.
    Broken(&'static str),
}

impl fmt::Display for RangeFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RangeFault::Inverted { from, to } => {
                write!(f, "the range [{from}, {to}) starts after it ends")
            }
            RangeFault::BeforeStart { from, first } => {
                write!(f, "cursor {from} precedes the log's first cursor {first}")
            }
            RangeFault::PastEnd { to, end } => {
                write!(f, "cursor {to} is past the log's end cursor {end}")
            }
            RangeFault::Broken(why) => write!(f, "the log is broken: {why}"),
        }
    }
}

impl std::error::Error for RangeFault {}

/// The ordered input log of one epoch: a header, the inputs, and the break flag.
pub struct EventLog<S: Schema> {
    header: LogHeader,
    inputs: Vec<S::Input>,
    max_events: usize,
    broken: Option<&'static str>,
}

impl<S: Schema> Clone for EventLog<S> {
    fn clone(&self) -> Self {
        EventLog {
            header: self.header.clone(),
            inputs: self.inputs.clone(),
            max_events: self.max_events,
            broken: self.broken,
        }
    }
}

impl<S: Schema> fmt::Debug for EventLog<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventLog")
            .field("header", &self.header)
            .field("len", &self.inputs.len())
            .field("max_events", &self.max_events)
            .field("broken", &self.broken)
            .finish()
    }
}

/// What a decoded log says when its header carried the break flag.
const DECODED_BREAK: &str = "the log was written with a recording break";

impl<S: Schema> EventLog<S> {
    /// An empty log holding at most `max_events` inputs.
    #[inline]
    #[must_use]
    pub const fn new(header: LogHeader, max_events: usize) -> Self {
        EventLog {
            header,
            inputs: Vec::new(),
            max_events,
            broken: None,
        }
    }

    /// Appends an input and returns its cursor; [`LogFull`] (log untouched) when the log
    /// holds `max_events` inputs or the cursor after it would leave the `u64` range.
    pub fn push(&mut self, i: S::Input) -> Result<u64, LogFull> {
        let full = LogFull {
            limit: self.max_events,
        };
        if self.inputs.len() >= self.max_events {
            return Err(full);
        }
        let cursor = self.end_cursor();
        // The end cursor after this input must still be representable.
        if cursor.checked_add(1).is_none() {
            return Err(full);
        }
        self.inputs.push(i);
        Ok(cursor)
    }

    /// Takes back the last input (its step was refused after it was recorded).
    #[inline]
    pub(crate) fn retract_last(&mut self) -> Option<S::Input> {
        self.inputs.pop()
    }

    /// Marks a recording break: some delivered input is missing. The first reason sticks.
    #[inline]
    pub fn mark_broken(&mut self, why: &'static str) {
        if self.broken.is_none() {
            self.broken = Some(why);
        }
    }

    /// The recording break, if any.
    #[inline]
    #[must_use]
    pub const fn broken(&self) -> Option<&'static str> {
        self.broken
    }

    /// The header.
    #[inline]
    #[must_use]
    pub const fn header(&self) -> &LogHeader {
        &self.header
    }

    /// The most inputs this log holds.
    #[inline]
    #[must_use]
    pub const fn max_events(&self) -> usize {
        self.max_events
    }

    /// How many inputs it holds.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.inputs.len()
    }

    /// Whether it holds no input.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }

    /// Every input, in order (regardless of the break flag).
    #[inline]
    #[must_use]
    pub fn inputs(&self) -> &[S::Input] {
        &self.inputs
    }

    /// `first_cursor + len`: the cursor the next input gets.
    #[inline]
    #[must_use]
    pub fn end_cursor(&self) -> u64 {
        self.header
            .first_cursor
            .saturating_add(len_u64(self.inputs.len()))
    }

    /// The inputs at cursors `[from, to)`. Refused when the log is broken, the range is
    /// inverted, or it reaches outside `[first_cursor, end_cursor]`.
    pub fn range(&self, from: u64, to: u64) -> Result<&[S::Input], RangeFault> {
        if let Some(why) = self.broken {
            return Err(RangeFault::Broken(why));
        }
        if from > to {
            return Err(RangeFault::Inverted { from, to });
        }
        let first = self.header.first_cursor;
        if from < first {
            return Err(RangeFault::BeforeStart { from, first });
        }
        let end = self.end_cursor();
        if to > end {
            return Err(RangeFault::PastEnd { to, end });
        }
        // Both offsets are at most `len`, which is a `usize`.
        let lo = usize::try_from(from - first).map_err(|_| RangeFault::PastEnd { to, end })?;
        let hi = usize::try_from(to - first).map_err(|_| RangeFault::PastEnd { to, end })?;
        self.inputs
            .get(lo..hi)
            .ok_or(RangeFault::PastEnd { to, end })
    }

    /// The digest of the inputs at `[from, to)`: `Digest::of(b"aterm-aos-trace-v2\0",
    /// [schema digest, identity digest, ancestor, log epoch, from, to, input…])`, each input
    /// as its canonical encoding. Refused as [`EventLog::range`] refuses.
    pub fn trace_digest(&self, schema: &S, from: u64, to: u64) -> Result<Digest, RangeFault> {
        let inputs = self.range(from, to)?;
        Ok(self.digest_inputs(schema, from, to, inputs))
    }

    /// What the seal names: the trace digest of the whole log, whatever the broken flag says
    /// (the flag is in the header record, under its own CRC).
    fn seal_digest(&self, schema: &S) -> Digest {
        let (from, to) = (self.header.first_cursor, self.end_cursor());
        self.digest_inputs(schema, from, to, &self.inputs)
    }

    fn digest_inputs(&self, schema: &S, from: u64, to: u64, inputs: &[S::Input]) -> Digest {
        let mut b = DigestBuilder::new(TRACE_DOMAIN);
        b.part(&schema.id().digest.0);
        b.part(&self.header.identity.digest().0);
        b.part(&self.header.ancestor.0);
        b.part(&self.header.log_epoch.to_le_bytes());
        b.part(&from.to_le_bytes());
        b.part(&to.to_le_bytes());
        let mut e = Enc::new();
        for i in inputs {
            schema.encode_input(i, &mut e);
            b.part(e.as_bytes());
            e = Enc::with_capacity(e.len());
        }
        b.finish()
    }

    /// The log's bytes, ending in its seal; [`LogFault::TooLarge`] if one record would exceed
    /// what a step record carries (nothing is returned then).
    pub fn encode(&self, schema: &S) -> Result<Vec<u8>, LogFault> {
        let mut out = Vec::new();
        let mut body = Enc::new();
        body.u8(RECORD_VERSION);
        body.u8(KIND_HEADER);
        body.raw(&LOG_MAGIC);
        body.identity(&self.header.identity);
        body.u64(self.header.log_epoch);
        body.u64(self.header.first_cursor);
        body.digest(&self.header.ancestor);
        body.bool(self.broken.is_some());
        put_record(&mut out, 0, body.as_bytes())?;
        for (k, i) in self.inputs.iter().enumerate() {
            let step = len_u64(k).saturating_add(1);
            let mut body = Enc::new();
            body.u8(RECORD_VERSION);
            body.u8(KIND_EVENT);
            schema.encode_input(i, &mut body);
            put_record(&mut out, step, body.as_bytes())?;
        }
        let n = len_u64(self.inputs.len());
        let mut body = Enc::new();
        body.u8(RECORD_VERSION);
        body.u8(KIND_SEAL);
        body.u64(n);
        body.digest(&self.seal_digest(schema));
        put_record(&mut out, n.saturating_add(1), body.as_bytes())?;
        Ok(out)
    }

    /// Reads a log recorded under `expect` that extends the checkpoint whose digest is
    /// `ancestor`, accepting at most `max_events` events. Returns the log and how its bytes
    /// ended: only [`Tail::Clean`] is the whole log; a torn tail is dropped and reported, and
    /// a cut at a record boundary is reported [`Tail::Unsealed`].
    pub fn decode(
        schema: &S,
        bytes: &[u8],
        expect: &TaskIdentity,
        ancestor: &Digest,
        max_events: usize,
    ) -> Result<(EventLog<S>, Tail), LogFault> {
        if !bytes.starts_with(&FRAME_MAGIC) {
            return Err(LogFault::NotALog);
        }
        let scan = scan_steps_from(bytes, 0);
        let tail = match &scan.end {
            StepEnd::Clean => Tail::Clean,
            StepEnd::Torn => Tail::Torn {
                at: scan.valid_len,
                dropped: bytes.len().saturating_sub(scan.valid_len),
            },
            StepEnd::Fault { at, kind } => {
                return Err(LogFault::Step(format!("at byte {at}: {kind}")));
            }
        };
        let Some((_, head_range)) = scan.steps.first() else {
            return Err(LogFault::MissingHeader);
        };
        check_layout(&scan.steps, scan.valid_len)?;
        let head_body = bytes
            .get(head_range.clone())
            .ok_or(LogFault::MissingHeader)?;
        let (header, broken) = read_header(head_body)?;
        if let Some(m) = IdentityMismatch::first(&header.identity, expect) {
            return Err(LogFault::Identity(m));
        }
        if schema.id() != &expect.schema {
            return Err(LogFault::Identity(IdentityMismatch::Schema));
        }
        if header.ancestor != *ancestor {
            return Err(LogFault::Ancestor {
                expected: *ancestor,
                found: header.ancestor,
            });
        }
        // The seal can stand only last; a seal anywhere before it is refused as a record
        // of the wrong kind when the events are read.
        let records = scan.steps.get(1..).unwrap_or(&[]);
        let (events, seal) = match records.split_last() {
            Some(((step, range), init))
                if bytes.get(range.clone()).and_then(|b| b.get(..2))
                    == Some(&[RECORD_VERSION, KIND_SEAL][..]) =>
            {
                (init, Some((*step, range.clone())))
            }
            _ => (records, None),
        };
        if events.len() > max_events {
            return Err(LogFault::TooManyEvents { limit: max_events });
        }
        let n = len_u64(events.len());
        if header.first_cursor.checked_add(n).is_none() {
            return Err(LogFault::Wire(WireFault::TooLong {
                field: "log.events",
                len: n,
                limit: u64::MAX - header.first_cursor,
            }));
        }
        let mut inputs = Vec::with_capacity(events.len());
        for (step, range) in events {
            let body = bytes.get(range.clone()).ok_or(LogFault::MissingHeader)?;
            inputs.push(read_event(schema, *step, body)?);
        }
        let mut log = EventLog {
            header,
            inputs,
            max_events,
            broken: None,
        };
        if broken {
            log.mark_broken(DECODED_BREAK);
        }
        let Some((step, range)) = seal else {
            // No seal: a cut at a record boundary (or a torn seal, reported as torn).
            let tail = match tail {
                Tail::Clean => Tail::Unsealed {
                    events: log.inputs.len(),
                },
                torn => torn,
            };
            return Ok((log, tail));
        };
        let body = bytes.get(range).ok_or(LogFault::MissingHeader)?;
        let (sealed, digest) = read_seal(step, body)?;
        if sealed != n {
            return Err(LogFault::SealCount { sealed, found: n });
        }
        let found = log.seal_digest(schema);
        if digest != found {
            return Err(LogFault::SealDigest {
                sealed: digest,
                found,
            });
        }
        Ok((log, tail))
    }
}

fn put_record(out: &mut Vec<u8>, step: u64, body: &[u8]) -> Result<(), LogFault> {
    let rec = encode_step(step, body).map_err(|_| LogFault::TooLarge {
        at: step,
        len: body.len(),
        limit: MAX_BODY,
    })?;
    out.extend_from_slice(&rec);
    Ok(())
}

/// Every accepted record's frame must end exactly where its body ends: astream accepts
/// payload bytes past a step's body (its CRC covers them), but an Aos log has one encoding.
fn check_layout(
    steps: &[(u64, core::ops::Range<usize>)],
    valid_len: usize,
) -> Result<(), LogFault> {
    for (k, (_, range)) in steps.iter().enumerate() {
        let frame_end = match steps.get(k.saturating_add(1)) {
            Some((_, next)) => next.start.checked_sub(BODY_OFFSET),
            None => Some(valid_len),
        };
        let extra = frame_end.and_then(|end| end.checked_sub(range.end));
        match extra {
            Some(0) => {}
            Some(extra) => return Err(LogFault::Wire(WireFault::Trailing { extra })),
            None => return Err(LogFault::Wire(WireFault::NonCanonical { field: "record" })),
        }
    }
    Ok(())
}

fn read_header(body: &[u8]) -> Result<(LogHeader, bool), LogFault> {
    let mut d = Dec::new(body);
    let version = d.u8("record.version")?;
    if version != RECORD_VERSION {
        return Err(LogFault::RecordVersion(version));
    }
    match d.u8("record.kind")? {
        KIND_HEADER => {}
        KIND_EVENT => return Err(LogFault::MissingHeader),
        kind => return Err(LogFault::Kind { at: 0, kind }),
    }
    let magic = d.raw("log.magic", LOG_MAGIC.len())?;
    let (name, format) = magic.split_at(LOG_MAGIC.len() - 1);
    if name != &LOG_MAGIC[..LOG_MAGIC.len() - 1] {
        return Err(LogFault::NotALog);
    }
    let format = format.first().copied().unwrap_or(0);
    if format != LOG_MAGIC[LOG_MAGIC.len() - 1] {
        return Err(LogFault::Format(format));
    }
    let identity = d.identity()?;
    let log_epoch = d.u64("log.epoch")?;
    let first_cursor = d.u64("log.first_cursor")?;
    let ancestor = d.digest("log.ancestor")?;
    let broken = d.bool("log.broken")?;
    d.finish()?;
    Ok((
        LogHeader {
            identity,
            log_epoch,
            first_cursor,
            ancestor,
        },
        broken,
    ))
}

/// The seal record at `step`: the event count it names and the digest.
fn read_seal(step: u64, body: &[u8]) -> Result<(u64, Digest), LogFault> {
    let mut d = Dec::new(body);
    let version = d.u8("record.version")?;
    if version != RECORD_VERSION {
        return Err(LogFault::RecordVersion(version));
    }
    let kind = d.u8("record.kind")?;
    if kind != KIND_SEAL {
        return Err(LogFault::Kind { at: step, kind });
    }
    let count = d.u64("log.seal.count")?;
    let digest = d.digest("log.seal.digest")?;
    d.finish()?;
    Ok((count, digest))
}

fn read_event<S: Schema>(schema: &S, step: u64, body: &[u8]) -> Result<S::Input, LogFault> {
    let mut d = Dec::new(body);
    let version = d.u8("record.version")?;
    if version != RECORD_VERSION {
        return Err(LogFault::RecordVersion(version));
    }
    let kind = d.u8("record.kind")?;
    if kind != KIND_EVENT {
        return Err(LogFault::Kind { at: step, kind });
    }
    let rest = body.get(d.position()..).unwrap_or(&[]);
    let input = schema.decode_input(&mut d)?;
    d.finish()?;
    // The schema's decoder must be canonical: the input re-encodes to the bytes it came
    // from, so the trace digest of a decoded log is the digest of its bytes.
    let mut again = Enc::with_capacity(rest.len());
    schema.encode_input(&input, &mut again);
    if again.as_bytes() != rest {
        return Err(LogFault::Wire(WireFault::NonCanonical { field: "event" }));
    }
    Ok(input)
}
