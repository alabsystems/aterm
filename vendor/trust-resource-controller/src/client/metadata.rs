// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! Direct authenticated metadata members. This holds actual original borrowed
//! descriptions; it never creates a pool, GNU credit, memory grant or Child.

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use trust_resource_provenance::{Host, process, program};

use super::Client;
pub use crate::MetadataResult;
use crate::{
    Cancel, Channel, GnuTransport, METADATA_EVENT_PREFIX_MAX_BYTES, METADATA_MAX_OUTPUT,
    MetadataCompletion, MetadataEvent, MetadataFailureObservation, MetadataPrepared,
    SealedMetadataRequest, Started, Status,
};

/// Actual held CPU/GNU descriptions, whose source labels must exactly match
/// the request. The caller retains the original logical dispatch owner until
/// positive retirement. Duplicating a descriptor does not acquire a grant.
pub struct MetadataIo {
    pub descriptions: Vec<OwnedFd>,
}

pub enum MetadataError {
    BeforeRequest(io::Error),
    /// The host positively declined before descriptor attachment or launch.
    Refused,
    /// Delivery may have happened. Retain/cancel this member, fail its original
    /// execution unit, and never replay the request or mint replacement credit.
    Unknown {
        member: Box<MetadataMember>,
        error: io::Error,
    },
}
impl std::fmt::Debug for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BeforeRequest(error) => f.debug_tuple("BeforeRequest").field(error).finish(),
            Self::Refused => f.write_str("Refused"),
            Self::Unknown { error, .. } => {
                f.debug_struct("Unknown").field("error", error).finish_non_exhaustive()
            }
        }
    }
}
impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "native metadata member {self:?}")
    }
}
impl std::error::Error for MetadataError {}

// Closed internal sum: both purposes retain exactly the same transport and
// lifecycle owner, while each validates its own mandatory request envelope.
#[derive(Clone)]
enum NativeQueryRequest {
    Metadata(SealedMetadataRequest),
    Resolver(crate::resolver::SealedResolverQueryRequest),
}
trait NativeQuery {
    fn member(&self) -> &crate::Request;
    fn validate_request(&self) -> io::Result<()>;
    fn payload(&self) -> io::Result<Vec<u8>>;
    fn purpose_hello(&self) -> io::Result<Vec<u8>>;
    fn validate_prepared(&self, prepared: &MetadataPrepared) -> io::Result<()>;
}
impl NativeQuery for SealedMetadataRequest {
    fn member(&self) -> &crate::Request {
        &self.member
    }
    fn validate_request(&self) -> io::Result<()> {
        self.validate()
    }
    fn payload(&self) -> io::Result<Vec<u8>> {
        self.encode()
    }
    fn purpose_hello(&self) -> io::Result<Vec<u8>> {
        self.hello()
    }
    fn validate_prepared(&self, p: &MetadataPrepared) -> io::Result<()> {
        p.validate_for(self)
    }
}
impl NativeQuery for crate::resolver::SealedResolverQueryRequest {
    fn member(&self) -> &crate::Request {
        &self.member
    }
    fn validate_request(&self) -> io::Result<()> {
        self.validate()
    }
    fn payload(&self) -> io::Result<Vec<u8>> {
        self.encode()
    }
    fn purpose_hello(&self) -> io::Result<Vec<u8>> {
        self.hello()
    }
    fn validate_prepared(&self, p: &MetadataPrepared) -> io::Result<()> {
        p.validate_for_resolver(self)
    }
}
impl NativeQuery for NativeQueryRequest {
    fn member(&self) -> &crate::Request {
        match self {
            Self::Metadata(r) => r.member(),
            Self::Resolver(r) => r.member(),
        }
    }
    fn validate_request(&self) -> io::Result<()> {
        match self {
            Self::Metadata(r) => r.validate_request(),
            Self::Resolver(r) => r.validate_request(),
        }
    }
    fn payload(&self) -> io::Result<Vec<u8>> {
        match self {
            Self::Metadata(r) => r.payload(),
            Self::Resolver(r) => r.payload(),
        }
    }
    fn purpose_hello(&self) -> io::Result<Vec<u8>> {
        match self {
            Self::Metadata(r) => r.purpose_hello(),
            Self::Resolver(r) => r.purpose_hello(),
        }
    }
    fn validate_prepared(&self, p: &MetadataPrepared) -> io::Result<()> {
        match self {
            Self::Metadata(r) => r.validate_prepared(p),
            Self::Resolver(r) => r.validate_prepared(p),
        }
    }
}

pub struct MetadataMember {
    host: Host,
    pin: std::sync::Arc<trust_resource_provenance::PinnedHelper>,
    control: Channel,
    streams: MetadataStreams,
    request: NativeQueryRequest,
    _descriptions: Vec<OwnedFd>,
    poisoned: bool,
    result_delivered: bool,
    failure_observed: bool,
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn ordered(request: &impl NativeQuery, descriptions: Vec<OwnedFd>) -> io::Result<Vec<OwnedFd>> {
    request.validate_request()?;
    let mut descriptors = std::collections::BTreeMap::new();
    for fd in descriptions {
        let source = u32::try_from(fd.as_raw_fd()).map_err(|_| invalid("negative descriptor"))?;
        // SAFETY: observe only this owned live descriptor's flags.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        if flags < 0 || flags & libc::FD_CLOEXEC == 0 || descriptors.insert(source, fd).is_some() {
            return Err(invalid("metadata descriptions must be distinct owned CLOEXEC handles"));
        }
    }
    let mut result = Vec::new();
    if let Some(cpu) = request.member().delegations.cpu {
        result
            .push(descriptors.remove(&cpu.source).ok_or_else(|| invalid("missing original CPU"))?);
    }
    let (read, write) = match &request.member().delegations.gnu {
        GnuTransport::NumericPipe { read, write } | GnuTransport::NamedFifo { read, write, .. } => {
            (read, write)
        }
        GnuTransport::None => return Err(invalid("missing original GNU transport")),
    };
    for descriptor in [read, write] {
        result.push(
            descriptors
                .remove(&descriptor.source)
                .ok_or_else(|| invalid("missing original GNU handle"))?,
        );
    }
    if !descriptors.is_empty() {
        return Err(invalid("unrequested metadata descriptions"));
    }
    Ok(result)
}

impl Client {
    /// One direct request on a freshly authenticated connection to the SAME
    /// pinned native ancestor. Only that host can admit CPU/memory and launch.
    /// After request delivery, errors retain the original connection/handles.
    pub fn metadata(
        &self,
        request: &SealedMetadataRequest,
        io: MetadataIo,
        deadline: Instant,
    ) -> Result<MetadataMember, MetadataError> {
        self.native_query(&NativeQueryRequest::Metadata(request.clone()), io, deadline)
    }

    /// NEW resolver-query purpose; never reinterprets a role3 metadata request.
    pub fn resolver_query(
        &self,
        request: &crate::resolver::SealedResolverQueryRequest,
        io: MetadataIo,
        deadline: Instant,
    ) -> Result<MetadataMember, MetadataError> {
        self.native_query(&NativeQueryRequest::Resolver(request.clone()), io, deadline)
    }
    fn native_query(
        &self,
        request: &NativeQueryRequest,
        io: MetadataIo,
        deadline: Instant,
    ) -> Result<MetadataMember, MetadataError> {
        let prepare = || -> io::Result<_> {
            let payload = request.payload()?;
            let hello = request.purpose_hello()?;
            let descriptions = ordered(request, io.descriptions)?;
            let host = self.host.connect(deadline)?;
            let mut control = Channel::new(host.stream().try_clone()?)?;
            let (events, target_events) = UnixStream::pair()?;
            let (output, stdout, stderr) = Capture::new()?;
            let stdin = File::open("/dev/null")?;
            control.send(&hello, deadline)?;
            if control.receive(deadline)? != [0x50] {
                return Err(invalid("metadata host did not prepare the negotiated purpose"));
            }
            host.revalidate(deadline)?;
            Ok((
                payload,
                MetadataMember {
                    host,
                    pin: self.host.clone(),
                    control,
                    streams: MetadataStreams {
                        events: Channel::new(events)?,
                        output,
                        state: Lifecycle::default(),
                        observed: ObservedEvents::default(),
                        cancel_sent: false,
                    },
                    request: request.clone(),
                    _descriptions: descriptions,
                    poisoned: false,
                    result_delivered: false,
                    failure_observed: false,
                },
                stdin,
                stdout,
                stderr,
                target_events,
            ))
        };
        let (payload, mut member, stdin, stdout, stderr, target_events) =
            prepare().map_err(MetadataError::BeforeRequest)?;
        let attach = (|| -> io::Result<bool> {
            member.control.send(&payload, deadline)?;
            let ready = member.control.receive(deadline)?;
            match ready.as_slice() {
                [0] => {
                    member.pin.validate(deadline)?;
                    return Ok(false);
                }
                [0x41] => {}
                _ => return Err(invalid("metadata host readiness was malformed")),
            }
            member.host.revalidate(deadline)?;
            let mut rights =
                vec![stdin.as_fd(), stdout.as_fd(), stderr.as_fd(), target_events.as_fd()];
            rights.extend(member._descriptions.iter().map(AsFd::as_fd));
            jobserver_utils::descriptor_transfer::send(
                member.host.stream(),
                &rights,
                0x46,
                deadline,
            )?;
            if member.control.receive(deadline)? != [1] {
                return Err(invalid("metadata host did not acknowledge descriptor attachment"));
            }
            member.pin.validate(deadline)?;
            Ok(true)
        })();
        // Only the host retains target-side output/events endpoints from here.
        drop((stdin, stdout, stderr, target_events));
        match attach {
            Ok(true) => Ok(member),
            Ok(false) => Err(MetadataError::Refused),
            Err(error) => Err(MetadataError::Unknown { member: Box::new(member), error }),
        }
    }
}

impl MetadataMember {
    pub fn started(&self) -> Option<Started> {
        self.streams.state.started
    }

    /// Cancellation is best effort and never a release receipt. Continue
    /// polling the SAME member until Completed/Refused or conservative failure.
    pub fn cancel(&mut self, deadline: Instant) -> io::Result<()> {
        self.streams.cancel(&self.request, deadline)
    }

    /// Abandon this client unit and retain its available original bounded
    /// stream bytes once. This never reports Completed, exit or retirement;
    /// the caller must still cancel/fail and retire the original native scope.
    pub fn take_failure_observation(&mut self) -> Option<MetadataFailureObservation> {
        if self.result_delivered || self.failure_observed {
            return None;
        }
        self.failure_observed = true;
        self.poisoned = true;
        Some(self.streams.take_failure_observation(&self.request))
    }

    /// Bounded nonblocking work except a ready partial event frame, whose
    /// original absolute deadline is capped at250ms. A lost/malformed lifecycle
    /// poisons this execution unit; it never produces a fabricated exit.
    pub fn poll(&mut self, deadline: Instant) -> io::Result<Option<MetadataResult>> {
        if self.poisoned {
            return Err(invalid("metadata lifecycle is unusable; never replay"));
        }
        let result = self.poll_inner(deadline);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn poll_inner(&mut self, deadline: Instant) -> io::Result<Option<MetadataResult>> {
        match self.streams.poll(&self.request, deadline)? {
            StreamReady::Pending => Ok(None),
            StreamReady::Refused => {
                self.pin.validate(deadline)?;
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "metadata member refused before execution: {}",
                        self.streams.state.refused.as_deref().unwrap_or("unknown")
                    ),
                ))
            }
            StreamReady::Completed => {
                self.pin.validate(deadline)?;
                let state = &mut self.streams.state;
                let prepared =
                    state.prepared.take().ok_or_else(|| invalid("missing preparation"))?;
                let started =
                    state.started.take().ok_or_else(|| invalid("missing actual start"))?;
                let completion =
                    state.completed.take().ok_or_else(|| invalid("missing completion"))?;
                self.poisoned = true;
                self.result_delivered = true;
                Ok(Some(MetadataResult {
                    prepared,
                    started,
                    completion,
                    stdout: std::mem::take(&mut self.streams.output.bytes[0]),
                    stderr: std::mem::take(&mut self.streams.output.bytes[1]),
                    output_limit_exceeded: self.streams.output.exceeded,
                }))
            }
        }
    }
}

#[derive(Default)]
struct ObservedEvents {
    prefix: Vec<u8>,
    seen: u64,
}
impl ObservedEvents {
    fn record(&mut self, bytes: &[u8]) {
        self.seen = self.seen.saturating_add(bytes.len() as u64);
        let room = METADATA_EVENT_PREFIX_MAX_BYTES.saturating_sub(self.prefix.len());
        self.prefix.extend_from_slice(&bytes[..bytes.len().min(room)]);
    }
}
enum StreamReady {
    Pending,
    Refused,
    Completed,
}
/// Stream observation has no host pin or capacity authority. Only the outer
/// MetadataMember can accept a result after revalidating its original pin.
struct MetadataStreams {
    events: Channel,
    output: Capture,
    state: Lifecycle,
    observed: ObservedEvents,
    cancel_sent: bool,
}
impl MetadataStreams {
    fn cancel(&mut self, request: &impl NativeQuery, deadline: Instant) -> io::Result<()> {
        if self.cancel_sent || self.state.completed.is_some() || self.state.refused.is_some() {
            return Ok(());
        }
        self.cancel_sent = true;
        // The handshake connection closes after attachment. Cancellation uses
        // the SAME bidirectional event endpoint retained by the actual host.
        self.events.send(
            &Cancel {
                root_attempt: request.member().binding.root_attempt,
                member_attempt: request.member().member_attempt,
            }
            .encode()?,
            deadline,
        )
    }

    fn poll(&mut self, request: &impl NativeQuery, deadline: Instant) -> io::Result<StreamReady> {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.output.drain()?;
        if self.output.exceeded {
            self.cancel(request, deadline)?;
        }
        // Bound one call even if a peer continuously emits complete frames.
        for _ in 0..8 {
            if !self.events.ready_to_receive()? {
                break;
            }
            if self.events.at_eof()? {
                if self.state.completed.is_none() && self.state.refused.is_none() {
                    return Err(invalid("metadata event stream closed before retirement"));
                }
                break;
            }
            let frame_deadline = deadline.min(Instant::now() + Duration::from_millis(250));
            let observed = &mut self.observed;
            let bytes =
                self.events.receive_observed(frame_deadline, |bytes| observed.record(bytes))?;
            let event = MetadataEvent::decode(
                &bytes,
                request.member().binding.root_attempt,
                request.member().member_attempt,
            )?;
            self.state.accept(event, request)?;
        }
        if self.state.refused.is_some() {
            self.output.drain()?;
            if !self.output.eof.iter().all(|eof| *eof) || !self.events.at_eof()? {
                return Ok(StreamReady::Pending);
            }
            return Ok(StreamReady::Refused);
        }
        if self.state.completed.is_some() {
            self.output.drain()?;
            if !self.output.eof.iter().all(|eof| *eof) || !self.events.at_eof()? {
                return Ok(StreamReady::Pending);
            }
            return Ok(StreamReady::Completed);
        }
        Ok(StreamReady::Pending)
    }
    fn take_failure_observation(
        &mut self,
        request: &impl NativeQuery,
    ) -> MetadataFailureObservation {
        let capture_error = self.output.drain().err().map(|e| e.to_string());
        MetadataFailureObservation {
            root_attempt: request.member().binding.root_attempt,
            member_attempt: request.member().member_attempt,
            stdout: std::mem::take(&mut self.output.bytes[0]),
            stderr: std::mem::take(&mut self.output.bytes[1]),
            stdout_eof: self.output.eof[0],
            stderr_eof: self.output.eof[1],
            output_limit_exceeded: self.output.exceeded,
            event_prefix: std::mem::take(&mut self.observed.prefix),
            event_bytes_seen: self.observed.seen,
            capture_error,
        }
    }
}

#[derive(Default)]
struct Lifecycle {
    attached: bool,
    prepared: Option<MetadataPrepared>,
    started: Option<Started>,
    completed: Option<MetadataCompletion>,
    refused: Option<String>,
}
impl Lifecycle {
    fn accept(&mut self, event: MetadataEvent, request: &impl NativeQuery) -> io::Result<()> {
        if self.completed.is_some() || self.refused.is_some() {
            return Err(invalid("event after metadata terminal"));
        }
        match event {
            MetadataEvent::Prepared(prepared) => {
                request.validate_prepared(&prepared)?;
                let (host, boot) = process::host_boot()?;
                if !self.attached
                    || self.prepared.is_some()
                    || self.started.is_some()
                    || prepared.policy != program::RUNTIME_POLICY
                    || prepared.scope != program::INPUT_SCOPE
                    || prepared.host != host
                    || prepared.boot != boot
                {
                    return Err(invalid("metadata preparation ordering or input policy mismatch"));
                }
                self.prepared = Some(prepared);
            }
            MetadataEvent::Status(frame) => match frame.status {
                Status::Attached if !self.attached && self.prepared.is_none() => {
                    self.attached = true
                }
                Status::Started(started)
                    if self.attached && self.prepared.is_some() && self.started.is_none() =>
                {
                    self.started = Some(started)
                }
                Status::Refused { reason } if self.started.is_none() => self.refused = Some(reason),
                _ => {
                    return Err(invalid(
                        "metadata lifecycle missing, duplicate, unknown or contradictory",
                    ));
                }
            },
            MetadataEvent::Completed(completion) => {
                if self.started.is_none() {
                    return Err(invalid("metadata completion without actual start"));
                }
                self.completed = Some(completion);
            }
        }
        Ok(())
    }
}

struct Capture {
    pipes: [std::io::PipeReader; 2],
    bytes: [Vec<u8>; 2],
    eof: [bool; 2],
    exceeded: bool,
}
impl Capture {
    fn new() -> io::Result<(Self, std::io::PipeWriter, std::io::PipeWriter)> {
        let (stdout, target_stdout) = io::pipe()?;
        let (stderr, target_stderr) = io::pipe()?;
        for pipe in [&stdout, &stderr] {
            // SAFETY: exclusively owned new read endpoint; preserve all flags.
            let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
            if flags < 0
                || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                    < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok((
            Self {
                pipes: [stdout, stderr],
                bytes: Default::default(),
                eof: [false; 2],
                exceeded: false,
            },
            target_stdout,
            target_stderr,
        ))
    }
    fn drain(&mut self) -> io::Result<()> {
        let mut buffer = [0; 4096];
        for index in 0..2 {
            for _ in 0..16 {
                match self.pipes[index].read(&mut buffer) {
                    Ok(0) => {
                        self.eof[index] = true;
                        break;
                    }
                    Ok(count) => {
                        let available = METADATA_MAX_OUTPUT
                            .saturating_sub(self.bytes[0].len() + self.bytes[1].len());
                        self.bytes[index].extend_from_slice(&buffer[..count.min(available)]);
                        self.exceeded |= count > available;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
