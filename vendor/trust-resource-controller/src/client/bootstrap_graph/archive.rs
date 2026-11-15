// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! One original archive-copy observation; native ownership remains with the Host.
use std::io;
use std::os::fd::{AsRawFd as _, BorrowedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256};
use trust_resource_provenance::{Host, PinnedHelper};

use super::{OutputBoundBootstrapGraph, bounded, finish, time};
use crate::bootstrap_graph::{archive as wire, transport};
use crate::client::{Client, Delivery};
use crate::{Channel, MetadataCompletion, MetadataPrepared, Started};

/// Captured DATA with absent accounting preserved. Neither these fields nor
/// stdout JSON create file ownership, native retirement or returned resources.
#[derive(Debug)]
pub struct ArchiveObservation {
    pub prepared: MetadataPrepared,
    pub started: Started,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub completion: MetadataCompletion,
    pub accounting: crate::accounting::Observation,
    pub event_prefix: Vec<u8>,
    pub event_bytes_seen: u64,
    pub event_frames_seen: u64,
}

struct Transcript {
    prepared: Option<MetadataPrepared>,
    started: Option<Started>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    eof: [bool; 2],
    failed: bool,
    raw: Vec<u8>,
    bytes_seen: u64,
    frames_seen: u64,
    terminal: Option<wire::Terminal>,
}
impl Transcript {
    fn new() -> Self {
        Self {
            prepared: None,
            started: None,
            stdout: vec![],
            stderr: vec![],
            eof: [false; 2],
            failed: false,
            raw: vec![],
            bytes_seen: 0,
            frames_seen: 0,
            terminal: None,
        }
    }
    fn observe(&mut self, bytes: &[u8]) {
        match self.bytes_seen.checked_add(bytes.len() as u64) {
            Some(count) => self.bytes_seen = count,
            None => self.failed = true,
        }
        let capacity = crate::METADATA_EVENT_PREFIX_MAX_BYTES.saturating_sub(self.raw.len());
        self.raw.extend_from_slice(&bytes[..bytes.len().min(capacity)]);
    }
    fn receive_frame(&mut self, channel: &mut Channel, deadline: Instant) -> io::Result<Vec<u8>> {
        if self.frames_seen >= 2 * wire::MAX_OUTPUT + 5 {
            return Err(crate::wire::invalid());
        }
        let bytes = channel.receive_observed(deadline, |bytes| self.observe(bytes))?;
        self.frames_seen = self.frames_seen.checked_add(1).ok_or_else(crate::wire::invalid)?;
        Ok(bytes)
    }
    fn event(&mut self, event: wire::Event) -> io::Result<()> {
        if self.failed {
            return Err(crate::wire::invalid());
        }
        let result = (|| {
            match event {
                wire::Event::Prepared(prepared)
                    if self.prepared.is_none() && self.started.is_none() =>
                {
                    self.prepared = Some(*prepared)
                }
                wire::Event::Started(started)
                    if self.prepared.is_some() && self.started.is_none() =>
                {
                    self.started = Some(started)
                }
                wire::Event::Output { stream, offset, bytes } if self.started.is_some() => {
                    let index = match stream {
                        wire::Stream::Stdout => 0,
                        wire::Stream::Stderr => 1,
                    };
                    let output = if index == 0 { &mut self.stdout } else { &mut self.stderr };
                    if bytes.is_empty()
                        || bytes.len() > wire::MAX_CHUNK
                        || self.eof[index]
                        || offset != output.len() as u64
                        || output
                            .len()
                            .checked_add(bytes.len())
                            .is_none_or(|len| len as u64 > wire::MAX_OUTPUT)
                    {
                        return Err(crate::wire::invalid());
                    }
                    output.extend(bytes);
                }
                wire::Event::Eof { stream, bytes } if self.started.is_some() => {
                    let index = match stream {
                        wire::Stream::Stdout => 0,
                        wire::Stream::Stderr => 1,
                    };
                    let observed = if index == 0 { self.stdout.len() } else { self.stderr.len() };
                    if self.eof[index] || bytes != observed as u64 {
                        return Err(crate::wire::invalid());
                    }
                    self.eof[index] = true;
                }
                _ => return Err(crate::wire::invalid()),
            }
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn complete(&self) -> io::Result<()> {
        if self.failed || self.prepared.is_none() || self.started.is_none() || self.eof != [true; 2]
        {
            return Err(crate::wire::invalid());
        }
        Ok(())
    }
    fn observation(
        self,
        completion: MetadataCompletion,
        accounting: crate::accounting::Observation,
    ) -> ArchiveObservation {
        ArchiveObservation {
            prepared: self.prepared.expect("checked original preparation"),
            started: self.started.expect("checked actual start"),
            stdout: self.stdout,
            stderr: self.stderr,
            completion,
            accounting,
            event_prefix: self.raw,
            event_bytes_seen: self.bytes_seen,
            event_frames_seen: self.frames_seen,
        }
    }
}

struct ExpectedPreparation<'a> {
    request: &'a wire::Request,
    digest: [u8; 32],
    host: &'a str,
    boot: &'a str,
}

fn receive_operation(
    channel: &mut Channel,
    expected: ExpectedPreparation<'_>,
    deadline: Instant,
    transcript: &mut Transcript,
) -> io::Result<(MetadataCompletion, crate::accounting::Observation)> {
    let ExpectedPreparation { request, digest, host, boot } = expected;
    let result = (|| {
        loop {
            let bytes = transcript.receive_frame(channel, deadline)?;
            if crate::wire::header(&bytes, 121).is_ok() {
                let event = wire::Event::decode(&bytes, request, digest)?;
                if let wire::Event::Prepared(prepared) = &event
                    && (prepared.host != host || prepared.boot != boot)
                {
                    return Err(crate::wire::invalid());
                }
                transcript.event(event)?;
            } else {
                let terminal = wire::Terminal::decode(&bytes, request, digest)?;
                transcript.terminal = Some(terminal.clone());
                return match terminal {
                    wire::Terminal::Completed { completion, observation } => {
                        transcript.complete()?;
                        Ok((*completion, observation))
                    }
                    wire::Terminal::Refused { reason } => {
                        Err(io::Error::new(io::ErrorKind::PermissionDenied, reason))
                    }
                    wire::Terminal::Unknown { reason } => Err(io::Error::other(reason)),
                };
            }
        }
    })();
    if result.is_err() {
        transcript.failed = true;
        channel.poison();
    }
    result
}
fn require_final_eof(
    channel: &mut Channel,
    deadline: Instant,
    transcript: &mut Transcript,
) -> io::Result<()> {
    let result = (|| {
        loop {
            time(deadline)?;
            if channel.at_eof()? {
                return time(deadline);
            }
            if channel.ready_to_receive()? {
                // Retain actually read trailing/partial bytes before refusal.
                let _ = transcript.receive_frame(channel, deadline)?;
                return Err(crate::wire::invalid());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    if result.is_err() {
        transcript.failed = true;
        channel.poison();
    }
    result
}

fn acknowledge(
    channel: &mut Channel,
    request: &wire::Request,
    digest: [u8; 32],
    terminal: &wire::Terminal,
    deadline: Instant,
    transcript: &mut Transcript,
) -> io::Result<()> {
    let result = (|| {
        require_final_eof(channel, deadline, transcript)?;
        channel.send(&wire::Ack::encode(request, digest, terminal)?, deadline)?;
        // SAFETY: this Channel retains the original live socket. Shutdown
        // affects only its write direction, after the complete bound ACK;
        // it neither closes the descriptor nor creates native retirement.
        if unsafe { libc::shutdown(channel.as_fd().as_raw_fd(), libc::SHUT_WR) } < 0 {
            return Err(io::Error::last_os_error());
        }
        require_final_eof(channel, deadline, transcript)
    })();
    if result.is_err() {
        transcript.failed = true;
        channel.poison();
    }
    result
}

struct OriginalArchive {
    host: Option<Host>,
    channel: Option<Channel>,
    pin: Arc<PinnedHelper>,
    request: wire::Request,
    deadline: Instant,
    transcript: Transcript,
}
impl Drop for OriginalArchive {
    fn drop(&mut self) {
        // Unknown abandonment cannot release the accepted operation boundary.
        // Actual Host independently retains its original Job/borrower until
        // native retirement. This is no cancellation/closure or replay path.
        if let Some(original) = self.host.take() {
            std::mem::forget(original);
        }
        if let Some(original) = self.channel.take() {
            std::mem::forget(original);
        }
    }
}

pub struct ArchiveFailure {
    delivery: Delivery,
    error: io::Error,
    original: Option<Box<OriginalArchive>>,
    completed: Option<Box<CompletedArchive>>,
}
impl ArchiveFailure {
    pub fn delivery(&self) -> Delivery {
        self.delivery
    }
    pub fn error(&self) -> &io::Error {
        &self.error
    }
    /// Exact received prefixes survive malformed/trailing frames and late
    /// native failures; neither prefix length nor status supplies completion.
    pub fn prefixes(&self) -> (&[u8], &[u8]) {
        if let Some(original) = &self.original {
            return (&original.transcript.stdout, &original.transcript.stderr);
        }
        if let Some(completed) = &self.completed {
            return (&completed.observation.stdout, &completed.observation.stderr);
        }
        (&[], &[])
    }
    pub fn event_prefix(&self) -> (&[u8], u64) {
        if let Some(original) = &self.original {
            return (&original.transcript.raw, original.transcript.bytes_seen);
        }
        if let Some(completed) = &self.completed {
            return (&completed.observation.event_prefix, completed.observation.event_bytes_seen);
        }
        (&[], 0)
    }
    pub fn terminal(&self) -> Option<&wire::Terminal> {
        self.original
            .as_ref()
            .and_then(|original| original.transcript.terminal.as_ref())
            .or_else(|| self.completed.as_ref().map(|completed| &completed.terminal))
    }
    pub fn event_frames_seen(&self) -> u64 {
        self.original
            .as_ref()
            .map(|original| original.transcript.frames_seen)
            .or_else(|| {
                self.completed.as_ref().map(|completed| completed.observation.event_frames_seen)
            })
            .unwrap_or(0)
    }
    fn before(error: io::Error) -> Self {
        Self { delivery: Delivery::BeforeRequest, error, original: None, completed: None }
    }
    fn unknown(mut original: OriginalArchive, error: io::Error) -> Self {
        if let Some(channel) = &mut original.channel {
            channel.poison();
        }
        Self {
            delivery: Delivery::Unknown,
            error,
            original: Some(Box::new(original)),
            completed: None,
        }
    }
}
impl Drop for ArchiveFailure {
    fn drop(&mut self) {
        // Preserve the actual opaque boundary and raw observations even if a
        // caller abandons the error, including a failure after compaction.
        if let Some(original) = self.original.take() {
            std::mem::forget(original);
        }
        if let Some(completed) = self.completed.take() {
            std::mem::forget(completed);
        }
    }
}
impl std::fmt::Debug for ArchiveFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootstrapArchiveFailure")
            .field("delivery", &self.delivery)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for ArchiveFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "original Bootstrap archive copy {:?}: {}", self.delivery, self.error)
    }
}
impl std::error::Error for ArchiveFailure {}

/// A same-native-operation observer, created only after the complete bound
/// exchange and final actual Host/root/pin/GNU checks. No public constructor
/// exists from a terminal or saved report. It supplies no producing owner, GNU
/// return, Seed/SDK adoption or closure of the enclosing Bootstrap graph.
pub struct CompletedArchive {
    accepted: PinnedHelper,
    pin: Arc<PinnedHelper>,
    request: wire::Request,
    deadline: Instant,
    observation: ArchiveObservation,
    terminal: wire::Terminal,
}
impl CompletedArchive {
    pub fn request(&self) -> &wire::Request {
        &self.request
    }
    pub fn observation(&self) -> &ArchiveObservation {
        &self.observation
    }
    pub fn revalidate(&self, deadline: Instant) -> io::Result<()> {
        bounded(self.deadline, deadline)?;
        finish(deadline, || {
            self.accepted.validate(deadline)?;
            self.pin.validate(deadline)
        })
    }
}

impl OutputBoundBootstrapGraph {
    /// Request the distinct native archive-copy operation under this SAME
    /// original graph. Actual input/SDK/GNU/resource owners remain with their
    /// original coordinator and Host through all outcomes and native closure.
    /// Borrowed GNU descriptions are restrictive witnesses, never a loan.
    /// The returned observer does not adopt copied files, a Seed or an SDK.
    /// The peer's EOF after ACK is the same earlier Host write-half closure;
    /// it is not an ACK receipt. The Host independently consumes ACK and peer
    /// EOF under its actual native operation before closing its own work gate.
    pub fn copy_local_archives(
        &mut self,
        client: &Client,
        request: wire::Request,
        gnu: &[BorrowedFd<'_>; 2],
        deadline: Instant,
    ) -> Result<CompletedArchive, ArchiveFailure> {
        let started = Instant::now();
        let accepted_contents = self.materialization_contents();
        let preparation_key = self.materialization().map(|request| request.key.clone());
        let root = self.original.as_mut().expect("same original output-bound graph");
        let preflight = (|| {
            bounded(root.deadline, deadline)?;
            request.validate()?;
            root.queries.check(request.key.sequence)?;
            let materialization = self.materialization.as_ref().ok_or_else(crate::wire::invalid)?;
            materialization.check(self.source.source_directory, Some(&self.output))?;
            if request.key.graph != root.request.key
                || request.source_root != root.request.source
                || request.source_directory != self.source.source_directory
                || Some(request.contents_directory) != accepted_contents
                || preparation_key.as_ref() != Some(&root.request.key)
                || request.gnu != root.request.gnu
                || request.host != root.context.context.host
                || request.boot != root.context.context.boot
                || root
                    .context
                    .context
                    .sdk
                    .as_ref()
                    .is_none_or(|sdk| sdk.execution != request.sdk_root)
                || !Arc::ptr_eq(&root.pin, &client.host)
            {
                return Err(crate::wire::invalid());
            }
            transport::check_gnu_identity(&request.gnu, gnu)?;
            root.check(deadline)
        })();
        preflight.map_err(ArchiveFailure::before)?;
        let payload = request.encode().map_err(ArchiveFailure::before)?;
        let hello = request.hello().map_err(ArchiveFailure::before)?;
        let digest =
            Sha256::digest(request.binding_preimage().map_err(ArchiveFailure::before)?).into();
        let work_end = started
            .checked_add(Duration::from_nanos(request.timeout_ns))
            .ok_or_else(|| ArchiveFailure::before(io::ErrorKind::TimedOut.into()))?;
        let deadline = deadline.min(work_end);
        time(deadline).map_err(ArchiveFailure::before)?;
        root.queries.begin().map_err(ArchiveFailure::before)?;
        // The restriction precedes connect. Capture an accepted Host before
        // any fallible stream cloning; errors/unwind never clear the veto.
        let mut original = OriginalArchive {
            host: None,
            channel: None,
            pin: Arc::clone(&client.host),
            request,
            deadline,
            transcript: Transcript::new(),
        };
        original.host = Some(client.host.connect(deadline).map_err(ArchiveFailure::before)?);
        let channel = original
            .host
            .as_ref()
            .expect("same accepted Host")
            .stream()
            .try_clone()
            .and_then(Channel::new);
        original.channel = Some(match channel {
            Ok(channel) => channel,
            Err(error) => return Err(ArchiveFailure::unknown(original, error)),
        });
        let result = finish(deadline, || {
            let host = original.host.as_ref().expect("same accepted archive Host");
            host.revalidate(deadline)?;
            original.channel.as_mut().unwrap().send(&hello, deadline)?;
            original.channel.as_mut().unwrap().send(&payload, deadline)?;
            let (completion, accounting) = receive_operation(
                original.channel.as_mut().unwrap(),
                ExpectedPreparation {
                    request: &original.request,
                    digest,
                    host: &root.context.context.host,
                    boot: &root.context.context.boot,
                },
                deadline,
                &mut original.transcript,
            )?;
            require_final_eof(
                original.channel.as_mut().unwrap(),
                deadline,
                &mut original.transcript,
            )?;
            host.revalidate(deadline)?;
            original.pin.validate(deadline)?;
            root.check(deadline)?;
            transport::check_gnu_identity(&original.request.gnu, gnu)?;
            let terminal = original.transcript.terminal.clone().expect("same decoded terminal");
            // ACK binds the FULL terminal and request. EOF is emitted only
            // after that complete ACK, never as a substitute for native return.
            acknowledge(
                original.channel.as_mut().unwrap(),
                &original.request,
                digest,
                &terminal,
                deadline,
                &mut original.transcript,
            )?;
            host.revalidate(deadline)?;
            original.pin.validate(deadline)?;
            root.check(deadline)?;
            transport::check_gnu_identity(&original.request.gnu, gnu)?;
            require_success(&completion)?;
            Ok((completion, accounting))
        });
        let (completion, accounting) = match result {
            Ok(value) => value,
            Err(error) => return Err(ArchiveFailure::unknown(original, error)),
        };
        let accepted = match original
            .host
            .take()
            .expect("same accepted Host")
            .into_helper_preserving(deadline)
        {
            Ok(value) => value,
            Err(failure) => {
                let (host, error) = *failure;
                original.host = Some(host);
                return Err(ArchiveFailure::unknown(original, error));
            }
        };
        drop(original.channel.take());
        let transcript = std::mem::replace(&mut original.transcript, Transcript::new());
        let terminal = transcript.terminal.clone().expect("same exact terminal");
        let completed = CompletedArchive {
            accepted,
            pin: Arc::clone(&original.pin),
            request: original.request.clone(),
            deadline: original.deadline,
            observation: transcript.observation(completion, accounting),
            terminal,
        };
        if let Err(error) = finish(deadline, || {
            completed.revalidate(deadline)?;
            root.check(deadline)?;
            transport::check_gnu_identity(&completed.request.gnu, gnu)
        })
        .and_then(|()| root.queries.completed_archive(&completed))
        .and_then(|()| {
            time(deadline).inspect_err(|_| {
                root.queries.active = Some(completed.request.key.sequence);
            })
        }) {
            return Err(ArchiveFailure {
                delivery: Delivery::Unknown,
                error,
                original: None,
                completed: Some(Box::new(completed)),
            });
        }
        Ok(completed)
    }
}
fn require_success(completion: &MetadataCompletion) -> io::Result<()> {
    if completion.inputs_unchanged.is_err()
        || !matches!(
            completion.finished.status,
            crate::Status::Finished {
                outcome: crate::Outcome::Exit(0),
                cause: crate::CompletionCause::Exited,
                ..
            }
        )
    {
        return Err(io::Error::other(
            "original archive copy did not finish successfully with unchanged inputs",
        ));
    }
    Ok(())
}
#[cfg(test)]
mod tests;
