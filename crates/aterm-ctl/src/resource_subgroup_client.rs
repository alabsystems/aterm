// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Job-scoped native subgroup transport and remote lifecycle observation. Never owns an OS child.

use crate::resource_admission_transport as transport;
use aterm_types::{resource_admission as admission, resource_subgroup as wire};
use aterm_uds::{
    CtlStream,
    process::{self, Birth},
    ready::{self, Interest},
};
use std::{
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsFd, OwnedFd},
    path::PathBuf,
    time::Instant,
};

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid resource subgroup transport or lifecycle",
    )
}
fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "resource subgroup authentication refused",
    )
}
fn time(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}

/// Endpoint supplied by independent canonical discovery, with expected native host identity.
/// A capability inside a Request authenticates only its original live job.
pub struct Endpoint {
    pub path: PathBuf,
    pub host_pid: u32,
    pub host_birth: Birth,
    pub host_uid: u32,
}
fn pin(stream: &CtlStream, endpoint: &Endpoint) -> io::Result<()> {
    if endpoint.host_uid != aterm_uds::peer::our_uid()
        || aterm_uds::peer::peer_uid(stream) != Some(endpoint.host_uid)
        || transport::local_peer(stream)?
            != (transport::Peer {
                pid: endpoint.host_pid,
                birth: endpoint.host_birth.0,
            })
    {
        return Err(denied());
    }
    Ok(())
}
fn caller(request: &wire::Request) -> io::Result<()> {
    let identity = process::read_identity(std::process::id() as i32).ok_or_else(denied)?;
    if request.member.peer_pid != std::process::id()
        || request.member.peer_birth != identity.birth.0
    {
        return Err(denied());
    }
    Ok(())
}

/// Original delegated open descriptions. No pathname is reopened by this transport.
pub struct Delegations {
    pub cpu: Option<OwnedFd>,
    pub gnu: Vec<OwnedFd>,
    pub fences: Vec<OwnedFd>,
}
/// Standard I/O behavior is explicit; supplied descriptors preserve their original open descriptions.
#[derive(Default)]
pub enum Stdio {
    #[default]
    Inherit,
    Null,
    Piped,
    Fd(OwnedFd),
}
/// Defaults match a native Command's inherited standard streams.
#[derive(Default)]
pub struct StdioConfig {
    pub stdin: Stdio,
    pub stdout: Stdio,
    pub stderr: Stdio,
}

/// Socket-backed child input avoids process-fatal SIGPIPE under hardened native defaults.
pub struct ChildInput(CtlStream);
impl Write for ChildInput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        aterm_uds::fdpass::send_vectored(&self.0, &[io::IoSlice::new(bytes)])
    }
    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        aterm_uds::fdpass::send_vectored(&self.0, bufs)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn inherited(index: u8) -> io::Result<OwnedFd> {
    aterm_uds::kept_out(|| match index {
        0 => io::stdin().as_fd().try_clone_to_owned(),
        1 => io::stdout().as_fd().try_clone_to_owned(),
        _ => io::stderr().as_fd().try_clone_to_owned(),
    })
}
fn null(input: bool) -> io::Result<OwnedFd> {
    aterm_uds::kept_out(|| File::options().read(input).write(!input).open("/dev/null"))
        .map(Into::into)
}
fn prepare_input(mode: Stdio) -> io::Result<(OwnedFd, Option<ChildInput>)> {
    match mode {
        Stdio::Inherit => Ok((inherited(0)?, None)),
        Stdio::Null => Ok((null(true)?, None)),
        Stdio::Fd(fd) => Ok((fd, None)),
        Stdio::Piped => {
            let (client, host) = aterm_uds::kept_out(CtlStream::pair)?;
            Ok((host.into(), Some(ChildInput(client))))
        }
    }
}
fn prepare_output(mode: Stdio, index: u8) -> io::Result<(OwnedFd, Option<File>)> {
    match mode {
        Stdio::Inherit => Ok((inherited(index)?, None)),
        Stdio::Null => Ok((null(false)?, None)),
        Stdio::Fd(fd) => Ok((fd, None)),
        Stdio::Piped => {
            let (reader, writer) = aterm_uds::kept_out(io::pipe)?;
            Ok((writer.into(), Some(File::from(OwnedFd::from(reader)))))
        }
    }
}
impl Delegations {
    fn validate(&self, request: &wire::Request) -> io::Result<()> {
        if self.cpu.is_some() != matches!(request.cpu, wire::CpuDelegation::SharedBorrow { .. })
            || self.gnu.len() != request.gnu.fd_count()
            || self.fences.len() != request.fence_targets.len()
        {
            return Err(invalid());
        }
        Ok(())
    }
}
/// Exact descriptor order; the host alone interprets and retains delegated resource custody.
pub struct Rights {
    pub stdin: OwnedFd,
    pub stdout: OwnedFd,
    pub stderr: OwnedFd,
    pub events: OwnedFd,
    pub delegated: Delegations,
}
impl Rights {
    fn borrowed(&self) -> Vec<std::os::fd::BorrowedFd<'_>> {
        let mut fds = vec![
            self.stdin.as_fd(),
            self.stdout.as_fd(),
            self.stderr.as_fd(),
            self.events.as_fd(),
        ];
        fds.extend(self.delegated.cpu.iter().map(AsFd::as_fd));
        fds.extend(self.delegated.gnu.iter().map(AsFd::as_fd));
        fds.extend(self.delegated.fences.iter().map(AsFd::as_fd));
        fds
    }
}
impl aterm_uds::spawn_lock::Descriptors for Rights {
    fn mark(&self) -> io::Result<()> {
        for fd in self.borrowed() {
            aterm_uds::keep_out_of_children(&fd)?;
        }
        Ok(())
    }
}

/// Return only after the host's job/capability/live-claim authorization, before actual launch.
pub struct AuthorizedSubgroup {
    pub request: wire::Request,
    pub peer: transport::Peer,
    pub rights: Rights,
}

/// After common runtime role dispatch and PREPARE, receive a job-scoped request,
/// authorizing native caller and capability before any rights.
/// The callback owns all live-claim, replay, ancestry and subbudget policy.
/// # Errors
/// Wrong job/caller, denied authorization, expired deadline or malformed/extra/truncated rights.
pub fn receive(
    stream: &CtlStream,
    expected_job: &admission::Binding,
    deadline: Instant,
    authorize: impl FnOnce(&wire::Request, transport::Peer) -> bool,
) -> io::Result<AuthorizedSubgroup> {
    stream.set_nonblocking(true)?;
    let peer = transport::local_peer(stream)?;
    let request = wire::Request::decode(&transport::read_frame(stream, deadline)?, expected_job)?;
    if request.member.peer_pid != peer.pid
        || request.member.peer_birth != peer.birth
        || request.fd_count()? > aterm_uds::fdpass::MAX_FDS
        || !authorize(&request, peer)
        || transport::local_peer(stream)? != peer
    {
        transport::write_frame(stream, &[0], deadline)?;
        return Err(denied());
    }
    transport::write_frame(stream, wire::READY, deadline)?;
    let rights = receive_rights(stream, &request, deadline)?;
    if transport::local_peer(stream)? != peer {
        return Err(denied());
    }
    // Socketpair credentials name its creator/caller, not the host receiving this right.
    let channel = CtlStream::from(rights.events);
    if transport::local_peer(&channel)? != peer {
        return Err(denied());
    }
    channel.set_nonblocking(true)?;
    Ok(AuthorizedSubgroup {
        request,
        peer,
        rights: Rights {
            events: channel.into(),
            ..rights
        },
    })
}
fn receive_rights(
    stream: &CtlStream,
    request: &wire::Request,
    deadline: Instant,
) -> io::Result<Rights> {
    let count = request.fd_count()?;
    loop {
        time(deadline)?;
        let result = aterm_uds::kept_out(|| {
            let mut marker = [0; 2];
            let got = aterm_uds::fdpass::recv_with_fds(stream, &mut marker, count)?;
            if got.bytes != 1 || marker[0] != wire::FD_MARKER || got.fds.len() != count {
                return Err(invalid());
            }
            let mut fds = got.fds.into_iter();
            let stdin = fds.next().ok_or_else(invalid)?;
            let stdout = fds.next().ok_or_else(invalid)?;
            let stderr = fds.next().ok_or_else(invalid)?;
            let events = fds.next().ok_or_else(invalid)?;
            let cpu = if matches!(request.cpu, wire::CpuDelegation::SharedBorrow { .. }) {
                Some(fds.next().ok_or_else(invalid)?)
            } else {
                None
            };
            let gnu = fds.by_ref().take(request.gnu.fd_count()).collect();
            let fences = fds.collect();
            let delegated = Delegations { cpu, gnu, fences };
            delegated.validate(request)?;
            Ok(Rights {
                stdin,
                stdout,
                stderr,
                events,
                delegated,
            })
        });
        match result {
            Ok(rights) => return Ok(rights),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Read, deadline)?
            }
            Err(e) => return Err(e),
        }
    }
}
/// Acknowledge attachment only after the authorized callback took ownership. Never a launch fact.
pub fn acknowledge(stream: &CtlStream, deadline: Instant) -> io::Result<()> {
    transport::write_frame(stream, wire::ATTACHED, deadline)
}

/// Actual launcher timestamps and native identity, available only after Started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Started {
    pub pid: u32,
    pub birth: Birth,
    pub monotonic_ns: u64,
    pub unix_ns: u64,
}
/// Actual completion, independent from whether a caller requested cancellation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completed {
    pub outcome: admission::Outcome,
    pub cause: admission::CompletionCause,
    pub runtime_ns: u64,
}
/// A prelaunch refusal remains distinct from actual child termination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminal {
    Finished(Completed),
    Refused { reason: String },
}

/// Failure before any descriptor delivery attempt, or ambiguous custody with an observable child.
pub enum SubmitError {
    NotSubmitted(io::Error),
    Unknown {
        error: io::Error,
        child: Box<RemoteChild>,
    },
}
impl std::fmt::Debug for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSubmitted(error) => f.debug_tuple("NotSubmitted").field(error).finish(),
            Self::Unknown { error, .. } => f
                .debug_struct("Unknown")
                .field("error", error)
                .finish_non_exhaustive(),
        }
    }
}
impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSubmitted(e) => write!(f, "subgroup not submitted: {e}"),
            Self::Unknown { error, .. } => write!(f, "subgroup custody unknown: {error}"),
        }
    }
}
impl std::error::Error for SubmitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotSubmitted(e) | Self::Unknown { error: e, .. } => Some(e),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Initial,
    Queued,
    Started,
    Terminal,
}

/// Pipes and authenticated lifecycle facts only. Contains no Child, pidfd or numeric process control.
pub struct RemoteChild {
    pub stdin: Option<ChildInput>,
    pub stdout: Option<File>,
    pub stderr: Option<File>,
    events: CtlStream,
    binding: admission::Binding,
    reader: FrameReader,
    phase: Phase,
    started: Option<Started>,
    terminal: Option<Terminal>,
    closed: bool,
    failure: Option<(io::ErrorKind, String)>,
    cancel: Vec<u8>,
    cancel_written: usize,
}
impl RemoteChild {
    fn new(
        events: CtlStream,
        binding: admission::Binding,
        stdin: Option<ChildInput>,
        stdout: Option<File>,
        stderr: Option<File>,
    ) -> io::Result<Self> {
        events.set_nonblocking(true)?;
        Ok(Self {
            stdin,
            stdout,
            stderr,
            events,
            binding,
            reader: FrameReader::default(),
            phase: Phase::Initial,
            started: None,
            terminal: None,
            closed: false,
            failure: None,
            cancel: Vec::new(),
            cancel_written: 0,
        })
    }
    /// Observe an independently authenticated transport's private lifecycle channel.
    /// This constructor authenticates no endpoint and establishes no admission,
    /// process-control, resource or settlement authority. The submitting transport
    /// must retain this observer after any possibly delivered rights transfer.
    /// # Errors
    /// Invalid binding, descriptor inheritance protection or socket configuration.
    pub fn from_private_events(binding: admission::Binding, events: OwnedFd) -> io::Result<Self> {
        admission::Cancel {
            binding: binding.clone(),
        }
        .encode()?;
        aterm_uds::keep_out_of_children(&events)?;
        Self::new(CtlStream::from(events), binding, None, None, None)
    }
    /// Connect anew to a pinned native host and submit this member once. Never retries admission.
    /// # Errors
    /// NotSubmitted precedes any rights attempt. Unknown retains the child after possible delivery;
    /// the caller must observe its private channel, never synthesize Refused or create a new attempt.
    pub fn submit(
        endpoint: &Endpoint,
        request: &wire::Request,
        delegated: Delegations,
        stdio: StdioConfig,
        deadline: Instant,
    ) -> Result<Self, SubmitError> {
        Self::submit_inner(endpoint, request, delegated, stdio, deadline)
    }
    fn submit_inner(
        endpoint: &Endpoint,
        request: &wire::Request,
        delegated: Delegations,
        stdio: StdioConfig,
        deadline: Instant,
    ) -> Result<Self, SubmitError> {
        let prepare = || -> io::Result<_> {
            time(deadline)?;
            if !endpoint.path.is_absolute() || endpoint.host_pid <= 1 || endpoint.host_birth.0 == 0
            {
                return Err(denied());
            }
            let bytes = request.encode()?;
            if request.fd_count()? > aterm_uds::fdpass::MAX_FDS {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "native descriptor transport cannot carry subgroup bundle",
                ));
            }
            delegated.validate(request)?;
            caller(request)?;
            let stream = aterm_uds::connect::until(&endpoint.path, deadline)?;
            stream.set_nonblocking(true)?;
            pin(&stream, endpoint)?;
            transport::write_frame(&stream, &wire::hello(&request.job)?, deadline)?;
            if transport::read_frame(&stream, deadline)? != wire::PREPARE {
                return Err(denied());
            }
            transport::write_frame(&stream, &bytes, deadline)?;
            if transport::read_frame(&stream, deadline)? != wire::READY {
                return Err(denied());
            }
            pin(&stream, endpoint)?;
            caller(request)?;
            let (input, stdin) = prepare_input(stdio.stdin)?;
            let (output, stdout) = prepare_output(stdio.stdout, 1)?;
            let (diagnostic, stderr) = prepare_output(stdio.stderr, 2)?;
            let (events, host_events) = aterm_uds::kept_out(CtlStream::pair)?;
            let child = Self::new(
                events,
                request.member.binding.clone(),
                stdin,
                stdout,
                stderr,
            )?;
            Ok((
                stream,
                child,
                Rights {
                    stdin: input,
                    stdout: output,
                    stderr: diagnostic,
                    events: host_events.into(),
                    delegated,
                },
            ))
        };
        let (stream, child, rights) = prepare().map_err(SubmitError::NotSubmitted)?;
        let result = (|| {
            loop {
                time(deadline)?;
                match aterm_uds::fdpass::send_with_fds(
                    &stream,
                    &[wire::FD_MARKER],
                    &rights.borrowed(),
                ) {
                    Ok(1) => break,
                    Ok(_) => return Err(io::ErrorKind::WriteZero.into()),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        ready::until(stream.as_fd(), Interest::Write, deadline)?
                    }
                    Err(e) => return Err(e),
                }
            }
            if transport::read_frame(&stream, deadline)? != wire::ATTACHED {
                return Err(invalid());
            }
            // The connected peer was pinned before payload and rights. A valid ACK
            // remains its fact after close; XNU clears peer-query state on close.
            Ok(())
        })();
        // Close our copies of every host endpoint on both success and ambiguous failure.
        drop(rights);
        match result {
            Ok(()) => Ok(child),
            Err(error) => Err(SubmitError::Unknown {
                error,
                child: Box::new(child),
            }),
        }
    }
    /// Actual identity after Started; attachment acknowledgement does not populate it.
    pub fn identity(&self) -> Option<&Started> {
        self.started.as_ref()
    }
    fn fail<T>(&mut self, error: io::Error) -> io::Result<T> {
        self.failure = Some((error.kind(), error.to_string()));
        Err(error)
    }
    fn event(&mut self, event: admission::Event) -> io::Result<()> {
        match (self.phase, event) {
            (Phase::Initial, admission::Event::Queued) => self.phase = Phase::Queued,
            (
                Phase::Initial | Phase::Queued,
                admission::Event::Started {
                    pid,
                    birth,
                    monotonic_ns,
                    unix_ns,
                },
            ) if pid > 1 && birth > 0 && monotonic_ns > 0 && unix_ns > 0 => {
                self.started = Some(Started {
                    pid,
                    birth: Birth(birth),
                    monotonic_ns,
                    unix_ns,
                });
                self.phase = Phase::Started;
            }
            (
                Phase::Started,
                admission::Event::Finished {
                    outcome,
                    cause,
                    runtime_ns,
                },
            ) => {
                self.terminal = Some(Terminal::Finished(Completed {
                    outcome,
                    cause,
                    runtime_ns,
                }));
                self.phase = Phase::Terminal;
            }
            (Phase::Initial | Phase::Queued, admission::Event::Refused { reason }) => {
                self.terminal = Some(Terminal::Refused { reason });
                self.phase = Phase::Terminal;
            }
            _ => return Err(invalid()),
        }
        Ok(())
    }
    /// Nonblocking, bounded parsing retains partial prefixes and bodies across calls.
    /// # Errors
    /// Wrong binding, duplicate/out-of-order facts, malformed frames or channel loss before a terminal fact.
    pub fn try_wait(&mut self) -> io::Result<Option<Terminal>> {
        if let Some((kind, message)) = &self.failure {
            return Err(io::Error::new(*kind, message.clone()));
        }
        if self.closed {
            return Ok(self.terminal.clone());
        }
        let mut budget = 64;
        loop {
            match self.reader.poll(&self.events, &mut budget) {
                Ok(Some(bytes)) => {
                    let event = match admission::EventFrame::decode(&bytes, &self.binding) {
                        Ok(frame) => frame.event,
                        Err(e) => return self.fail(e),
                    };
                    if let Err(e) = self.event(event) {
                        return self.fail(e);
                    }
                    // After a terminal fact, even one additional byte is forbidden. Keep
                    // one nonblocking read available when this call exhausted its budget.
                    if self.terminal.is_some() {
                        budget = budget.max(1);
                    }
                }
                Ok(None) => {
                    if self.terminal.is_some() && self.reader.pending() {
                        return self.fail(invalid());
                    }
                    return Ok(self.terminal.clone());
                }
                Err(e)
                    if e.kind() == io::ErrorKind::UnexpectedEof
                        && self.terminal.is_some()
                        && !self.reader.pending() =>
                {
                    self.closed = true;
                    return Ok(self.terminal.clone());
                }
                Err(e) => return self.fail(e),
            }
        }
    }
    /// Wait for an actual terminal fact; queue time never starts a client solver timer.
    pub fn wait(&mut self) -> io::Result<Terminal> {
        loop {
            if let Some(done) = self.try_wait()? {
                return Ok(done);
            }
            ready::one(self.events.as_fd())?;
        }
    }
    /// Bound this I/O wait by an absolute deadline, independently of the host-owned GO timeout.
    pub fn wait_until(&mut self, deadline: Instant) -> io::Result<Terminal> {
        loop {
            time(deadline)?;
            if let Some(done) = self.try_wait()? {
                return Ok(done);
            }
            ready::until(self.events.as_fd(), Interest::Read, deadline)?;
        }
    }
    /// Cancel only this member Binding. A timed-out partial write resumes on the next call.
    /// # Errors
    /// Deadline or socket error; neither proves cancellation nor prelaunch refusal.
    pub fn cancel(&mut self, deadline: Instant) -> io::Result<()> {
        if let Some((kind, message)) = &self.failure {
            return Err(io::Error::new(*kind, message.clone()));
        }
        if self.terminal.is_some() {
            return Ok(());
        }
        if self.cancel.is_empty() {
            let bytes = admission::Cancel {
                binding: self.binding.clone(),
            }
            .encode()?;
            self.cancel.extend((bytes.len() as u32).to_be_bytes());
            self.cancel.extend(bytes);
        }
        let mut channel = &self.events;
        while self.cancel_written < self.cancel.len() {
            time(deadline)?;
            match channel.write(&self.cancel[self.cancel_written..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => self.cancel_written += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    ready::until(channel.as_fd(), Interest::Write, deadline)?
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct FrameReader {
    prefix: [u8; 4],
    prefix_used: usize,
    body: Vec<u8>,
    body_used: usize,
}
impl FrameReader {
    fn pending(&self) -> bool {
        self.prefix_used != 0 || !self.body.is_empty()
    }
    fn poll(&mut self, mut stream: &CtlStream, budget: &mut usize) -> io::Result<Option<Vec<u8>>> {
        loop {
            if self.prefix_used == 4 && self.body_used == self.body.len() {
                self.prefix_used = 0;
                self.body_used = 0;
                return Ok(Some(std::mem::take(&mut self.body)));
            }
            if *budget == 0 {
                return Ok(None);
            }
            *budget -= 1;
            let into = if self.prefix_used < 4 {
                &mut self.prefix[self.prefix_used..]
            } else {
                &mut self.body[self.body_used..]
            };
            match stream.read(into) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    if self.prefix_used < 4 {
                        self.prefix_used += n;
                        if self.prefix_used == 4 {
                            let len = u32::from_be_bytes(self.prefix) as usize;
                            if len == 0 || len > admission::MAX_PAYLOAD {
                                return Err(invalid());
                            }
                            self.body = vec![0; len];
                        }
                    } else {
                        self.body_used += n;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
#[path = "resource_subgroup_client/tests.rs"]
mod tests;
