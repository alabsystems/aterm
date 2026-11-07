// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private native proxy. Target identity is observational; all execution,
//! cancellation and settlement remain with the authenticated custodian.
use aterm_ctl::resource_subgroup_client::{
    Delegations, RemoteChild, Stdio, StdioConfig, SubmitError, Terminal,
};
use aterm_types::resource_admission as admission;
use std::{
    ffi::OsString,
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{fs::FileTypeExt, net::UnixStream},
    },
    process::ExitCode,
    time::{Duration, Instant},
};
use trust_resource_controller as wire;

const EXCHANGE: Duration = Duration::from_secs(8);
const POLL: Duration = Duration::from_millis(100);

/// The owner authenticates the actual producer, host and resource descriptions.
/// Return Unknown with the original RemoteChild after any ambiguous delivery.
/// Submission must be bounded; it must not reconstruct the complete command
/// from ambient environment or retain redundant standard-stream descriptions.
pub(crate) trait Backend {
    fn submit(
        &mut self,
        request: wire::Request,
        producer: OwnedFd,
        stdio: StdioConfig,
        delegated: Delegations,
    ) -> Result<RemoteChild, SubmitError>;
}

/// Recognize only the exact private entry; helper success is transport health,
/// never the target's exit code. No diagnostics use the target's standard I/O.
pub(crate) fn entry(args: &[OsString], backend: &mut dyn Backend) -> Option<ExitCode> {
    let fds = match wire::HelperFds::parse(args) {
        Ok(None) => return None,
        Ok(Some(fds)) => fds,
        Err(_) => return Some(ExitCode::from(75)),
    };
    Some(if run(fds, backend).is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(75)
    })
}

fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}

/// Validate before taking ownership. Duplicates never replace live host slots.
fn duplicate(raw: u32) -> io::Result<OwnedFd> {
    let raw = i32::try_from(raw).map_err(|_| invalid())?;
    // SAFETY: F_GETFD observes only the numeric descriptor; it dereferences no pointer.
    if unsafe { libc::fcntl(raw, libc::F_GETFD) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_DUPFD_CLOEXEC creates a fresh owned descriptor at an unused slot >=5.
    let copied = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 5) };
    if copied < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful F_DUPFD_CLOEXEC transferred exactly this fresh descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(copied) })
}
fn close_original(raw: u32) {
    // SAFETY: called only in this exact fresh helper role after successfully
    // duplicating this validated, uniquely listed inherited descriptor.
    drop(unsafe { OwnedFd::from_raw_fd(raw as i32) });
}
fn open(raw: u32) -> io::Result<()> {
    let raw = i32::try_from(raw).map_err(|_| invalid())?;
    // SAFETY: F_GETFD checks existence without dereferencing a pointer or allocating an FD.
    if unsafe { libc::fcntl(raw, libc::F_GETFD) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn socket(raw: u32) -> io::Result<OwnedFd> {
    open(raw)?;
    // SAFETY: zero is valid initial storage for this native sockaddr output.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let mut address_length = std::mem::size_of_val(&address) as libc::socklen_t;
    // SAFETY: live descriptor and correctly sized native sockaddr output storage.
    if unsafe {
        libc::getsockname(
            raw as i32,
            (&mut address as *mut libc::sockaddr_un).cast(),
            &mut address_length,
        )
    } != 0
        || address_length < 2
        || address_length as usize > std::mem::size_of_val(&address)
        || address.sun_family as libc::c_int != libc::AF_UNIX
    {
        return Err(invalid());
    }
    let mut kind: libc::c_int = 0;
    let mut length = std::mem::size_of_val(&kind) as libc::socklen_t;
    // SAFETY: validated inherited slot, live correctly-sized option output buffers.
    if unsafe {
        libc::getsockopt(
            raw as i32,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&mut kind as *mut libc::c_int).cast(),
            &mut length,
        )
    } != 0
        || length as usize != std::mem::size_of_val(&kind)
        || kind != libc::SOCK_STREAM
    {
        return Err(invalid());
    }
    // SAFETY: the exact helper owns this validated socket. ManuallyDrop borrows
    // it for the connected-peer check without closing it on an error path.
    let stream = std::mem::ManuallyDrop::new(unsafe { UnixStream::from_raw_fd(raw as i32) });
    stream.peer_addr()?;
    // SAFETY: exact private helper ownership; no duplicate owner has been created.
    let owned = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    // SAFETY: the exact owned socket remains live; preserve every existing FD flag.
    let flags = unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_GETFD) };
    // SAFETY: F_SETFD sets close-on-exec only on this helper's owned descriptor.
    if flags < 0
        || unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    aterm_uds::keep_out_of_children(&owned)?;
    Ok(owned)
}
fn description(source: u32, fifo: bool) -> io::Result<OwnedFd> {
    if source < 5 {
        return Err(invalid());
    }
    let fd = duplicate(source)?;
    let file = File::from(fd);
    let kind = file.metadata()?.file_type();
    if if fifo {
        !kind.is_fifo()
    } else {
        !kind.is_file()
    } {
        return Err(invalid());
    }
    Ok(file.into())
}
fn intake(fds: wire::HelperFds, request: &wire::Request) -> io::Result<(StdioConfig, Delegations)> {
    request.validate()?;
    fds.validate_delegations(&request.delegations)?;
    let mut sources: Vec<_> = request
        .delegations
        .fences
        .iter()
        .map(|d| d.source)
        .collect();
    if let Some(cpu) = request.delegations.cpu {
        sources.push(cpu.source);
    }
    match &request.delegations.gnu {
        wire::GnuTransport::None => {}
        wire::GnuTransport::NumericPipe { read, write }
        | wire::GnuTransport::NamedFifo { read, write, .. } => {
            sources.extend([read.source, write.source])
        }
    }
    // Check every original before allocating duplicates: otherwise a new FD
    // could accidentally occupy a missing source named by an invalid request.
    for raw in sources.iter().copied().chain(0..3) {
        open(raw)?;
    }
    let cpu = request
        .delegations
        .cpu
        .map(|d| description(d.source, false))
        .transpose()?;
    let gnu = match &request.delegations.gnu {
        wire::GnuTransport::None => Vec::new(),
        wire::GnuTransport::NumericPipe { read, write }
        | wire::GnuTransport::NamedFifo { read, write, .. } => vec![
            description(read.source, true)?,
            description(write.source, true)?,
        ],
    };
    let fences = request
        .delegations
        .fences
        .iter()
        .map(|d| description(d.source, false))
        .collect::<io::Result<Vec<_>>>()?;
    let mut streams = Vec::new();
    for raw in 0..3 {
        streams.push(duplicate(raw)?);
    }
    // The helper itself must not keep extra pipe ends after possible delivery.
    for raw in sources.into_iter().chain(0..3) {
        close_original(raw);
    }
    let stderr = streams.pop().ok_or_else(invalid)?;
    let stdout = streams.pop().ok_or_else(invalid)?;
    let stdin = streams.pop().ok_or_else(invalid)?;
    Ok((
        StdioConfig {
            stdin: Stdio::Fd(stdin),
            stdout: Stdio::Fd(stdout),
            stderr: Stdio::Fd(stderr),
        },
        Delegations { cpu, gnu, fences },
    ))
}
fn run(fds: wire::HelperFds, backend: &mut dyn Backend) -> io::Result<()> {
    let control = socket(fds.control)?;
    let producer = socket(fds.producer)?;
    let mut channel = wire::Channel::new(UnixStream::from(control))?;
    let request = wire::Request::decode(&channel.receive(Instant::now() + EXCHANGE)?)?;
    let context = Context {
        root: request.binding.root_attempt,
        member: request.member_attempt,
    };
    let (stdio, delegated) = match intake(fds, &request) {
        Ok(rights) => rights,
        Err(_) => {
            return context.send(
                &mut channel,
                wire::Status::Refused {
                    reason: "helper descriptions refused before submission".into(),
                },
            );
        }
    };
    let (mut child, attached) = match backend.submit(request, producer, stdio, delegated) {
        Ok(child) => (child, true),
        Err(SubmitError::Unknown { child, .. }) => (*child, false),
        Err(SubmitError::NotSubmitted(_)) => {
            return context.send(
                &mut channel,
                wire::Status::Refused {
                    reason: "member was not submitted".into(),
                },
            );
        }
    };
    let result = observe(&context, &mut channel, &mut child, attached);
    if result.is_err() {
        let _ = child.cancel(Instant::now() + EXCHANGE);
    }
    result
}
struct Context {
    root: [u8; 16],
    member: [u8; 16],
}
impl Context {
    fn send(&self, channel: &mut wire::Channel, status: wire::Status) -> io::Result<()> {
        channel.send(
            &wire::StatusFrame {
                root_attempt: self.root,
                member_attempt: self.member,
                status,
            }
            .encode()?,
            Instant::now() + EXCHANGE,
        )
    }
}
trait ObservedChild {
    fn started(&self) -> Option<wire::Started>;
    fn terminal(&mut self) -> io::Result<Option<Terminal>>;
    fn cancel(&mut self) -> io::Result<()>;
}
impl ObservedChild for RemoteChild {
    fn started(&self) -> Option<wire::Started> {
        self.identity().map(|value| wire::Started {
            identity: wire::Identity {
                pid: value.pid,
                birth: value.birth.0,
                #[cfg(target_os = "macos")]
                birth_kind: wire::BirthKind::MacosWallMicroseconds,
                #[cfg(not(target_os = "macos"))]
                birth_kind: wire::BirthKind::LinuxStartTicks,
            },
            monotonic_ns: value.monotonic_ns,
            unix_ns: value.unix_ns,
        })
    }
    fn terminal(&mut self) -> io::Result<Option<Terminal>> {
        self.try_wait()
    }
    fn cancel(&mut self) -> io::Result<()> {
        RemoteChild::cancel(self, Instant::now() + EXCHANGE)
    }
}
fn completed(done: Terminal) -> wire::Status {
    match done {
        Terminal::Refused { reason } => wire::Status::Refused { reason },
        Terminal::Finished(value) => wire::Status::Finished {
            outcome: match value.outcome {
                admission::Outcome::Exit(code) => wire::Outcome::Exit(code),
                admission::Outcome::Signal(signal) => wire::Outcome::Signal(signal),
            },
            cause: match value.cause {
                admission::CompletionCause::Exited => wire::CompletionCause::Exited,
                admission::CompletionCause::Timeout => wire::CompletionCause::Timeout,
                admission::CompletionCause::Cancelled => wire::CompletionCause::Cancelled,
                admission::CompletionCause::ResourceLimit => wire::CompletionCause::ResourceLimit,
            },
            runtime_ns: value.runtime_ns,
        },
    }
}
fn observe(
    context: &Context,
    channel: &mut wire::Channel,
    child: &mut impl ObservedChild,
    attached: bool,
) -> io::Result<()> {
    if attached {
        context.send(channel, wire::Status::Attached)?;
    }
    let mut started = None;
    let mut cancelled = false;
    loop {
        let terminal = child.terminal();
        if started.is_none()
            && let Some(actual) = child.started()
        {
            context.send(channel, wire::Status::Started(actual))?;
            started = Some(actual);
        }
        match terminal {
            Ok(Some(done)) => return context.send(channel, completed(done)),
            Err(_) => {
                let _ = context.send(
                    channel,
                    wire::Status::Unknown {
                        reason: "member lifecycle unavailable".into(),
                        started,
                    },
                );
                return Err(invalid());
            }
            Ok(None) => {}
        }
        if channel.ready_to_receive()? {
            let frame = channel.receive(Instant::now() + EXCHANGE)?;
            wire::Cancel::decode(
                &frame,
                wire::Cancel {
                    root_attempt: context.root,
                    member_attempt: context.member,
                },
            )?;
            if !cancelled {
                child.cancel()?;
                cancelled = true;
            }
        } else {
            match aterm_uds::ready::until(
                channel.as_fd(),
                aterm_uds::ready::Interest::Read,
                Instant::now() + POLL,
            ) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) => return Err(error),
            }
        }
    }
}
#[cfg(test)]
#[path = "resource_proxy/tests.rs"]
mod tests;
