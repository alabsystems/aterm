// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Persistent bounded observations over privately delegated local capabilities.
//! Monitoring owns no execution, cancellation or native leadership capability.
//! Pump and wait on an independent I/O context, never from native job policy.

use super::{Cursor, Event, Gap, Observer, Record};
use aterm_capacity::host_protocol::{Generation, MAX_FRAME_BYTES, Snapshot};
use aterm_uds::{
    CtlStream,
    ready::{self, Interest},
};
use std::{
    io::{self, Read as _, Write as _},
    net::Shutdown,
    os::fd::AsFd as _,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const REQUEST: &[u8; 8] = b"ATMON02\n";
const RESPONSE: &[u8; 8] = b"ATMOR02\n";
const REQUEST_LEN: usize = 48;
const HEADER_LEN: usize = 28;
const META_LEN: usize = 40;
const RECORD_LEN: usize = 18;
const IO_TURN: usize = 16;
const WRITE_TURN: usize = 16 * 1024;
const MAX_EXCHANGE: Duration = Duration::from_secs(60);
const HEARTBEAT: Duration = Duration::from_millis(500);
const MAX_CONNECTIONS: usize = 16;

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "native monitor exchange refused",
    )
}
fn expired() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "native monitor exchange deadline")
}
fn same_user(stream: &CtlStream) -> io::Result<()> {
    if aterm_uds::peer::peer_uid(stream) != Some(aterm_uds::peer::our_uid()) {
        return Err(refused());
    }
    Ok(())
}

/// Explicit read delegation minted by a fenced execution owner. Clones share a
/// 16-connection limit with all delegations from that owner. Trusted FD delivery
/// establishes issuer provenance; a path, generation or same UID cannot do so.
#[derive(Clone)]
pub struct Source {
    observer: Observer,
    generation: Generation,
    process: u32,
    active: Arc<AtomicUsize>,
}
struct Permit(Arc<AtomicUsize>);
impl Permit {
    fn take(active: &Arc<AtomicUsize>) -> io::Result<Self> {
        for _ in 0..MAX_CONNECTIONS {
            let used = active.load(Ordering::Acquire);
            if used >= MAX_CONNECTIONS {
                break;
            }
            if active
                .compare_exchange(used, used + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(Self(Arc::clone(active)));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "native monitor connection budget busy",
        ))
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Source {
    pub(super) fn new(
        observer: Observer,
        generation: Generation,
        active: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            observer,
            generation,
            process: std::process::id(),
            active,
        }
    }
    /// Create one persistent private connection. Trusted control may transfer
    /// only its fresh client descriptor to the intended reader. The supplied
    /// deadline bounds initial admission, not the connection's total lifetime.
    /// Complete exchanges renew bounded idle admission; partial bytes never do.
    /// # Errors
    /// Foreign issuer, expired deadline, connection budget or socket/peer failure.
    pub fn pair(&self, deadline: Instant) -> io::Result<(Client, Exchange)> {
        if self.process != std::process::id() {
            return Err(refused());
        }
        let now = Instant::now();
        ensure_time(deadline)?;
        let permit = Permit::take(&self.active)?;
        let (client, server) = aterm_uds::kept_out(CtlStream::pair)?;
        client.set_nonblocking(true)?;
        server.set_nonblocking(true)?;
        same_user(&server)?;
        Ok((
            Client::from_trusted_stream(client, self.generation)?,
            Exchange {
                stream: Some(server),
                observer: self.observer.clone(),
                generation: self.generation,
                deadline: deadline.min(now + MAX_EXCHANGE),
                process: self.process,
                phase: Phase::Request {
                    bytes: [0; REQUEST_LEN],
                    used: 0,
                },
                sequence: 0,
                cursor: None,
                permit: Some(permit),
            },
        ))
    }
}

/// Closed event projection, carrying no private prose or control authority.
/// Finished records summarize owner results, not retained-cohort drain proofs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Submitted,
    IntentDelivered,
    Rejected,
    AdmissionRefused,
    NativeObservation,
    Finished { succeeded: bool, cancelled: bool },
    Failed,
    StateChanged,
    OwnerClosed,
}
impl EventKind {
    fn of(event: &Event) -> Self {
        match event {
            Event::Submitted => Self::Submitted,
            Event::IntentDelivered(_) => Self::IntentDelivered,
            Event::Rejected(_) => Self::Rejected,
            Event::AdmissionRefused(_) => Self::AdmissionRefused,
            Event::Native(_) => Self::NativeObservation,
            Event::Finished {
                succeeded,
                cancelled,
                ..
            } => Self::Finished {
                succeeded: *succeeded,
                cancelled: *cancelled,
            },
            Event::Failed(_) => Self::Failed,
            Event::StateChanged => Self::StateChanged,
            Event::OwnerClosed => Self::OwnerClosed,
        }
    }
    fn encode(self) -> (u8, u8) {
        match self {
            Self::Submitted => (1, 0),
            Self::IntentDelivered => (2, 0),
            Self::Rejected => (3, 0),
            Self::AdmissionRefused => (4, 0),
            Self::NativeObservation => (5, 0),
            Self::Finished {
                succeeded,
                cancelled,
            } => (6, u8::from(succeeded) | (u8::from(cancelled) << 1)),
            Self::Failed => (7, 0),
            Self::StateChanged => (8, 0),
            Self::OwnerClosed => (9, 0),
        }
    }
    fn decode(code: u8, flags: u8) -> io::Result<Self> {
        Ok(match (code, flags) {
            (1, 0) => Self::Submitted,
            (2, 0) => Self::IntentDelivered,
            (3, 0) => Self::Rejected,
            (4, 0) => Self::AdmissionRefused,
            (5, 0) => Self::NativeObservation,
            (6, 0..=3) => Self::Finished {
                succeeded: flags & 1 != 0,
                cancelled: flags & 2 != 0,
            },
            (7, 0) => Self::Failed,
            (8, 0) => Self::StateChanged,
            (9, 0) => Self::OwnerClosed,
            _ => return Err(refused()),
        })
    }
}

/// One event in a bounded retained-history page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordSummary {
    pub revision: u64,
    pub attempt: Option<u64>,
    pub kind: EventKind,
}
impl From<&Record> for RecordSummary {
    fn from(record: &Record) -> Self {
        Self {
            revision: record.revision,
            attempt: record.attempt,
            kind: EventKind::of(&record.event),
        }
    }
}

/// Persistent reader. One mutable borrow permits one in-flight exchange. Any
/// failure permanently shuts down the socket, including cancellation duplicates.
pub struct Client {
    stream: Option<CtlStream>,
    generation: Generation,
    sequence: u64,
    cursor: Option<Cursor>,
    last: Option<Snapshot>,
}
impl Client {
    /// Adopt a fresh connected descriptor delivered by trusted launch control.
    /// The caller establishes issuer provenance; same UID alone is insufficient.
    /// # Errors
    /// Peer identity or nonblocking mode failure.
    pub fn from_trusted_stream(stream: CtlStream, generation: Generation) -> io::Result<Self> {
        same_user(&stream)?;
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream: Some(stream),
            generation,
            sequence: 0,
            cursor: None,
            last: None,
        })
    }
    /// Transfer a fresh capability and its separately trusted generation.
    /// An active or poisoned protocol state is never transferable as fresh.
    /// # Panics
    /// A read was attempted or the capability was already poisoned.
    #[must_use]
    pub fn into_stream(mut self) -> CtlStream {
        assert_eq!(
            self.sequence, 0,
            "only a fresh monitor capability can transfer"
        );
        assert!(
            self.cursor.is_none(),
            "only a fresh monitor capability can transfer"
        );
        self.stream
            .take()
            .expect("poisoned monitor capability cannot transfer")
    }
    fn poison(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
    /// Read one snapshot/history page, then close this capability. The same v2
    /// protocol serves persistent and one-read callers.
    /// # Errors
    /// The same expiry, framing, identity, history and socket errors as read_next.
    pub fn read_until(mut self, deadline: Instant) -> io::Result<Observation> {
        self.read_next(deadline)
    }
    /// Read the next bounded page. Unchanged long-poll heartbeats keep the
    /// source's original observation age and cursor. A cursor follows history,
    /// never the newer current snapshot revision.
    /// # Errors
    /// Poisoned capability, expiry, peer/I/O, framing, oversized payload, stale
    /// generation, sequence mismatch, rollback or malformed history. Every error
    /// permanently shuts down this capability; late bytes cannot satisfy a retry.
    pub fn read_next(&mut self, deadline: Instant) -> io::Result<Observation> {
        let result = self.read_inner(deadline);
        if result.is_err() {
            self.poison();
        }
        result
    }
    fn read_inner(&mut self, deadline: Instant) -> io::Result<Observation> {
        let started = Instant::now();
        let deadline = deadline.min(started + MAX_EXCHANGE);
        let sequence = self.sequence.checked_add(1).ok_or_else(refused)?;
        let stream = self.stream.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "native monitor capability poisoned",
            )
        })?;
        same_user(stream)?;
        let request = encode_request(self.generation, sequence, self.cursor);
        // Attempted use cannot be converted into a supposedly fresh capability.
        self.sequence = sequence;
        write_all(stream, &request, deadline)?;
        let mut header = [0; HEADER_LEN];
        read_exact(stream, &mut header, deadline)?;
        if &header[..8] != RESPONSE || number(&header[8..16]) != sequence {
            return Err(refused());
        }
        let len = u32::from_be_bytes(header[16..20].try_into().expect("length width")) as usize;
        if !(META_LEN..=MAX_FRAME_BYTES - HEADER_LEN).contains(&len) {
            return Err(refused());
        }
        let age = Duration::from_nanos(number(&header[20..28]));
        let mut body = vec![0; len];
        read_exact(stream, &mut body, deadline)?;
        // With one in-flight request the peer owes exactly this frame. Buffered
        // suffix bytes are not a future response and must poison the stream.
        if matches!(probe(stream)?, Quiet::Extra) {
            return Err(refused());
        }
        let received = Instant::now();
        let upper = age
            .checked_add(received.duration_since(started))
            .ok_or_else(refused)?;
        let mut observation = decode_body(&body, self.generation, sequence, self.cursor)?;
        if let Some(last) = &self.last
            && (observation.snapshot.revision < last.revision
                || (observation.snapshot.revision == last.revision
                    && observation.snapshot != *last)
                || (last.closed && !observation.snapshot.closed))
        {
            return Err(refused());
        }
        observation.received = received;
        observation.age = AgeBounds { lower: age, upper };
        self.cursor = Some(observation.next);
        self.last = Some(observation.snapshot.clone());
        Ok(observation)
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.poison();
    }
}

/// Conservative source age without synchronized clocks. Round-trip delay
/// bounds transport age rather than turning receipt into a fresh observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgeBounds {
    pub lower: Duration,
    pub upper: Duration,
}

/// One snapshot and bounded history page. The snapshot may be newer than next;
/// only next continues history. An empty table never proves cohort drain.
#[derive(Debug)]
pub struct Observation {
    pub snapshot: Snapshot,
    pub request_sequence: u64,
    pub next: Cursor,
    pub gap: Option<Gap>,
    pub history: Vec<RecordSummary>,
    received: Instant,
    age: AgeBounds,
}
impl Observation {
    /// Original age plus elapsed receipt time. Arithmetic exhaustion is unknown.
    #[must_use]
    pub fn age_bounds(&self) -> Option<AgeBounds> {
        let elapsed = self.received.elapsed();
        Some(AgeBounds {
            lower: self.age.lower.checked_add(elapsed)?,
            upper: self.age.upper.checked_add(elapsed)?,
        })
    }
}

fn number(bytes: &[u8]) -> u64 {
    u64::from_be_bytes(bytes.try_into().expect("fixed number width"))
}
fn encode_request(
    generation: Generation,
    sequence: u64,
    cursor: Option<Cursor>,
) -> [u8; REQUEST_LEN] {
    let mut request = [0; REQUEST_LEN];
    request[..8].copy_from_slice(REQUEST);
    request[8..24].copy_from_slice(&generation.0);
    request[24..32].copy_from_slice(&sequence.to_be_bytes());
    if let Some(cursor) = cursor {
        request[32] = 1;
        request[33..41].copy_from_slice(&cursor.revision.to_be_bytes());
    }
    request
}
fn decode_request(
    bytes: &[u8; REQUEST_LEN],
    generation: Generation,
    sequence: u64,
    cursor: Option<Cursor>,
) -> io::Result<()> {
    if *bytes != encode_request(generation, sequence, cursor) {
        return Err(refused());
    }
    Ok(())
}
fn encode_body(read: &super::Read, requested: Option<Cursor>) -> io::Result<Vec<u8>> {
    let snapshot = read.snapshot.encode()?;
    let mut bytes = vec![0; META_LEN];
    if let Some(cursor) = requested {
        bytes[0] = 1;
        bytes[1..9].copy_from_slice(&cursor.revision.to_be_bytes());
    }
    bytes[9..17].copy_from_slice(&read.next.revision.to_be_bytes());
    if let Some(gap) = read.gap {
        bytes[17] = 1;
        bytes[18..26].copy_from_slice(&gap.first.to_be_bytes());
        bytes[26..34].copy_from_slice(&gap.last.to_be_bytes());
    }
    bytes[34..36].copy_from_slice(&(read.events.len() as u16).to_be_bytes());
    bytes[36..40].copy_from_slice(&(snapshot.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&snapshot);
    for record in &read.events {
        let record = RecordSummary::from(record);
        bytes.extend_from_slice(&record.revision.to_be_bytes());
        bytes.extend_from_slice(&record.attempt.unwrap_or(0).to_be_bytes());
        let (kind, flags) = record.kind.encode();
        bytes.extend_from_slice(&[kind, flags]);
    }
    if bytes.len() > MAX_FRAME_BYTES - HEADER_LEN {
        return Err(refused());
    }
    Ok(bytes)
}
fn decode_body(
    bytes: &[u8],
    generation: Generation,
    sequence: u64,
    requested: Option<Cursor>,
) -> io::Result<Observation> {
    if bytes.len() < META_LEN {
        return Err(refused());
    }
    let echoed = match bytes[0] {
        0 if number(&bytes[1..9]) == 0 => None,
        1 => Some(Cursor {
            generation,
            revision: number(&bytes[1..9]),
        }),
        _ => return Err(refused()),
    };
    if echoed != requested {
        return Err(refused());
    }
    let next = Cursor {
        generation,
        revision: number(&bytes[9..17]),
    };
    let gap = match bytes[17] {
        0 if number(&bytes[18..26]) == 0 && number(&bytes[26..34]) == 0 => None,
        1 => Some(Gap {
            first: number(&bytes[18..26]),
            last: number(&bytes[26..34]),
        }),
        _ => return Err(refused()),
    };
    let count = u16::from_be_bytes(bytes[34..36].try_into().expect("count width")) as usize;
    let snapshot_len =
        u32::from_be_bytes(bytes[36..40].try_into().expect("snapshot width")) as usize;
    if count > super::READ_PAGE
        || snapshot_len == 0
        || META_LEN
            .checked_add(snapshot_len)
            .and_then(|n| n.checked_add(count * RECORD_LEN))
            != Some(bytes.len())
    {
        return Err(refused());
    }
    let snapshot = Snapshot::decode(&bytes[META_LEN..META_LEN + snapshot_len])?;
    if snapshot.generation != generation || next.revision > snapshot.revision {
        return Err(refused());
    }
    let mut history = Vec::with_capacity(count);
    for record in bytes[META_LEN + snapshot_len..].as_chunks::<RECORD_LEN>().0 {
        let attempt = number(&record[8..16]);
        history.push(RecordSummary {
            revision: number(&record[..8]),
            attempt: (attempt != 0).then_some(attempt),
            kind: EventKind::decode(record[16], record[17])?,
        });
    }
    let after = requested.map(|c| c.revision);
    if after.is_some_and(|after| next.revision < after) {
        return Err(refused());
    }
    let base = match (after, gap) {
        (Some(after), Some(gap))
            if after.checked_add(1) == Some(gap.first) && gap.first <= gap.last =>
        {
            gap.last
        }
        (Some(after), None) => after,
        (None, None) => history
            .first()
            .map_or(next.revision, |r| r.revision.saturating_sub(1)),
        _ => return Err(refused()),
    };
    let mut revision = base;
    for record in &history {
        if revision.checked_add(1) != Some(record.revision) || record.revision > snapshot.revision {
            return Err(refused());
        }
        revision = record.revision;
    }
    if (count == 0 && snapshot.revision > after.unwrap_or(0))
        || gap.is_some_and(|gap| gap.last >= snapshot.revision)
        || revision != next.revision
        || (count < super::READ_PAGE && next.revision != snapshot.revision)
    {
        return Err(refused());
    }
    Ok(Observation {
        snapshot,
        request_sequence: sequence,
        next,
        gap,
        history,
        received: Instant::now(),
        age: AgeBounds {
            lower: Duration::ZERO,
            upper: Duration::ZERO,
        },
    })
}

enum Phase {
    Request {
        bytes: [u8; REQUEST_LEN],
        used: usize,
    },
    Waiting {
        revision: u64,
    },
    Reply {
        bytes: Vec<u8>,
        sent: usize,
    },
    Closed,
}

/// Caller-driven nonblocking server. No native lifetime/control capability.
pub struct Exchange {
    stream: Option<CtlStream>,
    observer: Observer,
    generation: Generation,
    deadline: Instant,
    process: u32,
    phase: Phase,
    sequence: u64,
    cursor: Option<Cursor>,
    permit: Option<Permit>,
}
/// Whether a persistent connection needs further independent I/O service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    Pending,
    Complete,
}

impl Exchange {
    fn close(&mut self) {
        self.phase = Phase::Closed;
        self.stream.take();
        self.permit.take();
    }
    /// Pump at most 16 socket attempts and 16KiB output. Publication is captured
    /// briefly under its mutex, then encoded after release. No readiness wait,
    /// thread creation or subscriber callback runs here.
    /// # Errors
    /// Foreign process, deadline, framing, publication or socket failure. Any
    /// failure permanently closes this connection, never the native jobs.
    pub fn step(&mut self) -> io::Result<Progress> {
        let result = self.step_inner();
        if result.is_err() {
            self.close();
        }
        result
    }
    fn step_inner(&mut self) -> io::Result<Progress> {
        if matches!(self.phase, Phase::Closed) {
            return Ok(Progress::Complete);
        }
        if self.process != std::process::id() {
            return Err(refused());
        }
        let mut wrote = 0;
        // A completed Request iteration can perform both its read and one
        // suffix probe. There is at most one such transition per step (a
        // completed reply returns), so reserve its extra native I/O attempt.
        for _ in 0..IO_TURN - 1 {
            if !matches!(self.phase, Phase::Waiting { .. }) {
                ensure_time(self.deadline)?;
            }
            match &mut self.phase {
                Phase::Request { bytes, used } => {
                    match self
                        .stream
                        .as_mut()
                        .expect("open exchange")
                        .read(&mut bytes[*used..])
                    {
                        Ok(0) if *used == 0 => {
                            self.close();
                            return Ok(Progress::Complete);
                        }
                        Ok(0) => return Err(refused()),
                        Ok(n) => {
                            *used += n;
                            if *used == REQUEST_LEN {
                                let sequence = self.sequence.checked_add(1).ok_or_else(refused)?;
                                decode_request(bytes, self.generation, sequence, self.cursor)?;
                                if !matches!(
                                    probe(self.stream.as_mut().expect("open exchange"))?,
                                    Quiet::Open
                                ) {
                                    return Err(refused());
                                }
                                self.sequence = sequence;
                                let read =
                                    self.observer.read(self.cursor).map_err(|_| refused())?;
                                let unchanged = self.cursor.is_some_and(|cursor| {
                                    cursor.revision == read.snapshot.revision
                                }) && !read.snapshot.closed;
                                if unchanged {
                                    self.phase = Phase::Waiting {
                                        revision: read.snapshot.revision,
                                    };
                                    self.deadline = Instant::now() + HEARTBEAT;
                                    return Ok(Progress::Pending);
                                }
                                self.reply(read)?;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            return Ok(Progress::Pending);
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(e),
                    }
                }
                Phase::Waiting { revision } => {
                    match probe(self.stream.as_mut().expect("open exchange"))? {
                        Quiet::End => {
                            self.close();
                            return Ok(Progress::Complete);
                        }
                        Quiet::Extra => return Err(refused()),
                        Quiet::Open => {}
                    }
                    let read = self.observer.read(self.cursor).map_err(|_| refused())?;
                    if read.snapshot.revision == *revision
                        && !read.snapshot.closed
                        && Instant::now() < self.deadline
                    {
                        return Ok(Progress::Pending);
                    }
                    self.reply(read)?;
                }
                Phase::Reply { bytes, sent } => {
                    if wrote == WRITE_TURN {
                        return Ok(Progress::Pending);
                    }
                    let end = bytes.len().min(*sent + WRITE_TURN - wrote);
                    match self
                        .stream
                        .as_mut()
                        .expect("open exchange")
                        .write(&bytes[*sent..end])
                    {
                        Ok(0) => return Err(refused()),
                        Ok(n) => {
                            *sent += n;
                            wrote += n;
                            if *sent == bytes.len() {
                                self.phase = Phase::Request {
                                    bytes: [0; REQUEST_LEN],
                                    used: 0,
                                };
                                self.deadline = Instant::now() + MAX_EXCHANGE;
                                return Ok(Progress::Pending);
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            return Ok(Progress::Pending);
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(e),
                    }
                }
                Phase::Closed => return Ok(Progress::Complete),
            }
        }
        Ok(Progress::Pending)
    }
    fn reply(&mut self, read: super::Read) -> io::Result<()> {
        if read.snapshot.generation != self.generation {
            return Err(refused());
        }
        let body = encode_body(&read, self.cursor)?;
        let age = Instant::now()
            .checked_duration_since(read.observed_at)
            .ok_or_else(refused)?;
        let age = u64::try_from(age.as_nanos()).map_err(|_| refused())?;
        let mut bytes = Vec::with_capacity(HEADER_LEN + body.len());
        bytes.extend_from_slice(RESPONSE);
        bytes.extend_from_slice(&self.sequence.to_be_bytes());
        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&age.to_be_bytes());
        bytes.extend_from_slice(&body);
        self.cursor = Some(read.next);
        self.phase = Phase::Reply { bytes, sent: 0 };
        self.deadline = Instant::now() + MAX_EXCHANGE;
        Ok(())
    }
    /// Wait on this socket or its publication predicate. Publication capture and
    /// park share the owner's mutex, preventing lost wakes. Heartbeats bound
    /// cancellation/disconnect detection. Caller limits never renew budgets.
    /// # Errors
    /// Foreign process, expired socket wait or native readiness failure.
    pub fn wait(&self, until: Instant) -> io::Result<()> {
        if self.process != std::process::id() {
            return Err(refused());
        }
        match self.phase {
            Phase::Closed => Ok(()),
            Phase::Waiting { revision } => {
                self.observer
                    .wait_revision(revision, until.min(self.deadline));
                Ok(())
            }
            _ => {
                let Some(stream) = &self.stream else {
                    return Ok(());
                };
                ready::until(
                    stream.as_fd(),
                    if matches!(self.phase, Phase::Reply { .. }) {
                        Interest::Write
                    } else {
                        Interest::Read
                    },
                    until.min(self.deadline),
                )
            }
        }
    }
    /// Nearest fixed idle, request, heartbeat or response deadline. Partial
    /// socket progress never extends one; completed exchanges renew idle only.
    #[must_use]
    pub const fn deadline(&self) -> Instant {
        self.deadline
    }
}

fn ensure_time(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(expired())
    } else {
        Ok(())
    }
}
enum Quiet {
    Open,
    End,
    Extra,
}
fn probe(stream: &mut CtlStream) -> io::Result<Quiet> {
    match stream.read(&mut [0]) {
        Ok(0) => Ok(Quiet::End),
        Ok(_) => Ok(Quiet::Extra),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(Quiet::Open)
        }
        Err(e) => Err(e),
    }
}
fn read_exact(stream: &mut CtlStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        ensure_time(deadline)?;
        match stream.read(bytes) {
            Ok(0) => return Err(refused()),
            Ok(n) => bytes = &mut bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Read, deadline)?
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
fn write_all(stream: &mut CtlStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        ensure_time(deadline)?;
        match stream.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Write, deadline)?
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "build_admit_monitor_tests.rs"]
mod tests;
