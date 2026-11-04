// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One-shot observation over privately delegated local socket capabilities.
//! The server owns no execution, cancellation or native leadership capability.
//! Pump it on the host's independent I/O context, never from native job policy.

use super::Observer;
use aterm_capacity::host_protocol::{Generation, MAX_FRAME_BYTES, Snapshot};
use aterm_uds::{
    CtlStream,
    ready::{self, Interest},
};
use std::{
    io::{self, Read as _, Write as _},
    net::Shutdown,
    os::fd::{AsFd as _, BorrowedFd},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const REQUEST: &[u8; 8] = b"ATMON01\n";
const RESPONSE: &[u8; 8] = b"ATMOR01\n";
const REQUEST_LEN: usize = 24;
const HEADER_LEN: usize = 20;
const IO_TURN: usize = 16;
const WRITE_TURN: usize = 16 * 1024;
const MAX_EXCHANGE: Duration = Duration::from_secs(60);
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
/// 16-connection limit with every delegation from that owner. Streams authenticate through trusted FD delivery,
/// not a discovered path, generation string or same-user identity alone.
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
    /// Create one private one-shot connection. Trusted launch control may pass
    /// only its client descriptor to the intended reader; no token is persisted.
    /// The server's absolute deadline is capped at one minute from creation.
    /// # Errors
    /// Foreign issuer, expired deadline, 16 outstanding connections, socket or peer failure.
    pub fn pair(&self, deadline: Instant) -> io::Result<(Client, Exchange)> {
        if self.process != std::process::id() {
            return Err(refused());
        }
        let now = Instant::now();
        if deadline <= now {
            return Err(expired());
        }
        let permit = Permit::take(&self.active)?;
        let (client, server) = aterm_uds::kept_out(CtlStream::pair)?;
        client.set_nonblocking(true)?;
        server.set_nonblocking(true)?;
        same_user(&server)?;
        Ok((
            Client {
                stream: client,
                generation: self.generation,
            },
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
                permit: Some(permit),
            },
        ))
    }
}

/// A consumed-on-read client capability. Any partial I/O, framing or deadline
/// failure closes it; a later request cannot consume an earlier partial reply.
pub struct Client {
    stream: CtlStream,
    generation: Generation,
}

impl Client {
    /// Adopt an actual connected descriptor delivered by trusted launch control,
    /// with its expected incarnation. The caller establishes issuer provenance;
    /// kernel same-user checks alone cannot authenticate arbitrary discovery.
    /// # Errors
    /// Peer identity or nonblocking mode failure.
    pub fn from_trusted_stream(stream: CtlStream, generation: Generation) -> io::Result<Self> {
        same_user(&stream)?;
        stream.set_nonblocking(true)?;
        Ok(Self { stream, generation })
    }
    /// Transfer this read-only capability for explicit inherited-FD delivery.
    /// The recipient also receives the expected generation through trusted control.
    #[must_use]
    pub fn into_stream(self) -> CtlStream {
        self.stream
    }

    /// Read one bounded snapshot. This blocks only the calling I/O context and
    /// consumes the connection on every result. There is no execution opcode.
    /// # Errors
    /// Expiry, peer/I/O, framing, oversized payload, malformed snapshot or stale generation.
    pub fn read_until(mut self, deadline: Instant) -> io::Result<Observation> {
        let started = Instant::now();
        let deadline = deadline.min(started + MAX_EXCHANGE);
        same_user(&self.stream)?;
        let mut request = [0; REQUEST_LEN];
        request[..8].copy_from_slice(REQUEST);
        request[8..].copy_from_slice(&self.generation.0);
        write_all(&mut self.stream, &request, deadline)?;
        self.stream.shutdown(Shutdown::Write)?;
        let mut header = [0; HEADER_LEN];
        read_exact(&mut self.stream, &mut header, deadline)?;
        if &header[..8] != RESPONSE {
            return Err(refused());
        }
        let len =
            u32::from_be_bytes(header[8..12].try_into().expect("fixed length width")) as usize;
        if len == 0 || len > MAX_FRAME_BYTES {
            return Err(refused());
        }
        let age = Duration::from_nanos(u64::from_be_bytes(
            header[12..].try_into().expect("fixed age width"),
        ));
        let mut bytes = vec![0; len];
        read_exact(&mut self.stream, &mut bytes, deadline)?;
        read_end(&mut self.stream, deadline)?;
        let received = Instant::now();
        let upper = age
            .checked_add(received.duration_since(started))
            .ok_or_else(refused)?;
        let snapshot = Snapshot::decode(&bytes)?;
        if snapshot.generation != self.generation {
            return Err(refused());
        }
        Ok(Observation {
            snapshot,
            received,
            age: AgeBounds { lower: age, upper },
        })
    }
}

/// Conservative observation age without synchronized clocks. Transport delay
/// lies between zero and this exchange's measured round trip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgeBounds {
    pub lower: Duration,
    pub upper: Duration,
}

/// Snapshot-only reply. It does not advance a history cursor or supply completion
/// evidence for disappeared rows. An empty table never certifies cohort drain.
#[derive(Debug)]
pub struct Observation {
    pub snapshot: Snapshot,
    received: Instant,
    age: AgeBounds,
}
impl Observation {
    /// Original age plus elapsed time after receipt, never a fresh receipt stamp.
    /// None is arithmetic exhaustion, not zero age.
    #[must_use]
    pub fn age_bounds(&self) -> Option<AgeBounds> {
        let elapsed = self.received.elapsed();
        Some(AgeBounds {
            lower: self.age.lower.checked_add(elapsed)?,
            upper: self.age.upper.checked_add(elapsed)?,
        })
    }
}

enum Phase {
    Request {
        bytes: [u8; REQUEST_LEN],
        used: usize,
    },
    End,
    Reply {
        bytes: Vec<u8>,
        sent: usize,
    },
    Closed,
}

/// One caller-driven nonblocking server exchange. It owns only immutable
/// publication access. Failures close this connection, never native jobs.
pub struct Exchange {
    stream: Option<CtlStream>,
    observer: Observer,
    generation: Generation,
    deadline: Instant,
    process: u32,
    phase: Phase,
    permit: Option<Permit>,
}

/// Whether this one-shot exchange needs further independent I/O service.
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
    /// Pump at most 16 native I/O attempts and 16 KiB of response bytes. No socket
    /// readiness wait, thread creation or client callback runs here. Snapshot
    /// capture briefly locks publication; encoding follows mutex release.
    /// # Errors
    /// Foreign process, expiry, protocol/publication or socket failure. The
    /// exchange is closed permanently on any failure.
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
        for _ in 0..IO_TURN {
            if Instant::now() >= self.deadline {
                return Err(expired());
            }
            let stream = self.stream.as_mut().expect("open exchange");
            let result = match &mut self.phase {
                Phase::Request { bytes, used } => match stream.read(&mut bytes[*used..]) {
                    Ok(0) => return Err(refused()),
                    Ok(n) => {
                        *used += n;
                        if *used == REQUEST_LEN {
                            if &bytes[..8] != REQUEST || bytes[8..] != self.generation.0 {
                                return Err(refused());
                            }
                            self.phase = Phase::End;
                        }
                        Ok(())
                    }
                    Err(e) => Err(e),
                },
                Phase::End => match stream.read(&mut [0]) {
                    Ok(0) => {
                        let (snapshot, at) = self.observer.latest()?;
                        if snapshot.generation != self.generation {
                            return Err(refused());
                        }
                        let body = snapshot.encode()?;
                        let age = Instant::now()
                            .checked_duration_since(at)
                            .ok_or_else(refused)?;
                        let age = u64::try_from(age.as_nanos()).map_err(|_| refused())?;
                        let mut bytes = Vec::with_capacity(HEADER_LEN + body.len());
                        bytes.extend_from_slice(RESPONSE);
                        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
                        bytes.extend_from_slice(&age.to_be_bytes());
                        bytes.extend_from_slice(&body);
                        self.phase = Phase::Reply { bytes, sent: 0 };
                        Ok(())
                    }
                    Ok(_) => return Err(refused()),
                    Err(e) => Err(e),
                },
                Phase::Reply { bytes, sent } => {
                    if wrote == WRITE_TURN {
                        return Ok(Progress::Pending);
                    }
                    let end = bytes.len().min(*sent + WRITE_TURN - wrote);
                    match stream.write(&bytes[*sent..end]) {
                        Ok(0) => return Err(refused()),
                        Ok(n) => {
                            *sent += n;
                            wrote += n;
                            if *sent == bytes.len() {
                                self.close();
                                return Ok(Progress::Complete);
                            }
                            Ok(())
                        }
                        Err(e) => Err(e),
                    }
                }
                Phase::Closed => return Ok(Progress::Complete),
            };
            match result {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(Progress::Pending),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Progress::Pending)
    }
    /// Readiness needed by the host's I/O multiplexer; no job-state inference.
    #[must_use]
    pub fn readiness(&self) -> Option<(BorrowedFd<'_>, Interest)> {
        Some((
            self.stream.as_ref()?.as_fd(),
            if matches!(self.phase, Phase::Reply { .. }) {
                Interest::Write
            } else {
                Interest::Read
            },
        ))
    }
    /// The original absolute exchange deadline; repeated steps do not extend it.
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
fn read_end(stream: &mut CtlStream, deadline: Instant) -> io::Result<()> {
    let mut byte = [0];
    loop {
        ensure_time(deadline)?;
        match stream.read(&mut byte) {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(refused()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Read, deadline)?
            }
            Err(e) => return Err(e),
        }
    }
}
fn write_all(stream: &mut CtlStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        ensure_time(deadline)?;
        match stream.write(bytes) {
            Ok(0) => return Err(refused()),
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
