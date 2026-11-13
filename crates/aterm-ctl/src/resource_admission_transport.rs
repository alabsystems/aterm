// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Authenticated private local resource submission. Attachment is never launch.
use aterm_types::resource_admission as wire;
use aterm_uds::{
    CtlStream,
    ready::{self, Interest},
};
use std::{
    io::{self, IoSlice, Read},
    os::fd::{AsFd, OwnedFd},
    path::Path,
    time::Instant,
};

#[path = "resource_admission_native_store.rs"]
pub mod native_store;
#[path = "resource_admission_store_expectation.rs"]
pub mod store_expectation;

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid resource admission transport",
    )
}
fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "resource admission authentication refused",
    )
}
fn time(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::ErrorKind::TimedOut.into())
    } else {
        Ok(())
    }
}
/// Write without blocking past the caller's absolute deadline.
pub fn write_all(stream: &CtlStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    while !bytes.is_empty() {
        time(deadline)?;
        match aterm_uds::fdpass::send_vectored(stream, &[IoSlice::new(bytes)]) {
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
fn read_exact(mut stream: &CtlStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    while !bytes.is_empty() {
        time(deadline)?;
        match stream.read(bytes) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
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
/// Read a bounded control response without consuming any following frame.
pub fn read_line(stream: &CtlStream, deadline: Instant) -> io::Result<String> {
    let mut line = Vec::new();
    loop {
        let mut b = [0];
        read_exact(stream, &mut b, deadline)?;
        if b[0] == b'\n' {
            return String::from_utf8(line).map_err(|_| invalid());
        }
        if line.len() == 4096 {
            return Err(invalid());
        }
        line.push(b[0])
    }
}
/// Length framing shares one absolute deadline. Never logs frame contents.
pub fn write_frame(stream: &CtlStream, bytes: &[u8], deadline: Instant) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > wire::MAX_PAYLOAD {
        return Err(invalid());
    }
    write_all(stream, &(bytes.len() as u32).to_be_bytes(), deadline)?;
    write_all(stream, bytes, deadline)
}
/// Read one bounded length-prefixed frame using an absolute deadline.
pub fn read_frame(stream: &CtlStream, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut len = [0; 4];
    read_exact(stream, &mut len, deadline)?;
    let n = u32::from_be_bytes(len) as usize;
    if n == 0 || n > wire::MAX_PAYLOAD {
        return Err(invalid());
    }
    let mut bytes = vec![0; n];
    read_exact(stream, &mut bytes, deadline)?;
    Ok(bytes)
}

/// Four privately delegated descriptors, always consumed even on refusal.
pub struct Attachments {
    /// Child's input.
    pub stdin: OwnedFd,
    /// Child's output.
    pub stdout: OwnedFd,
    /// Child's diagnostic output.
    pub stderr: OwnedFd,
    /// Host endpoint of the private bidirectional event/cancel channel.
    pub events: OwnedFd,
}
/// Submit to one pinned absolute local owner endpoint. No retry or remote fallback.
/// Success means attachment accepted only; observe Started on the private channel.
/// An error after descriptor delivery can also mean accepted work whose final
/// acknowledgement was lost. It is never proof of refusal or of no launch.
/// Retain the other private socket endpoint to observe actual lifecycle facts;
/// do not synthesize Refused or resubmit under a new attempt after such an error.
pub fn attach(
    path: &str,
    request: &wire::Submission,
    fds: Attachments,
    deadline: Instant,
) -> io::Result<()> {
    attach_mode(path, request, fds, deadline, false)
}
/// Require an explicitly accounted root; unsupported peers fail before rights.
/// Lost attachment remains ambiguous and must not be retried.
pub fn attach_accounted(
    path: &str,
    request: &wire::Submission,
    fds: Attachments,
    deadline: Instant,
) -> io::Result<()> {
    attach_mode(path, request, fds, deadline, true)
}
fn attach_mode(
    path: &str,
    request: &wire::Submission,
    fds: Attachments,
    deadline: Instant,
    accounted: bool,
) -> io::Result<()> {
    time(deadline)?;
    let bytes = request.encode()?;
    let stream = connect_owner(path, deadline)?;
    write_all(
        &stream,
        if accounted {
            wire::ACCOUNTED_REQUEST
        } else {
            wire::REQUEST
        }
        .as_bytes(),
        deadline,
    )?;
    if read_line(&stream, deadline)?
        != if accounted {
            wire::ACCOUNTED_PREPARE
        } else {
            wire::PREPARE
        }
    {
        return Err(denied());
    }
    write_frame(&stream, &bytes, deadline)?;
    if read_line(&stream, deadline)?
        != if accounted {
            wire::ACCOUNTED_ADMISSION
        } else {
            wire::ADMISSION
        }
    {
        return Err(denied());
    }
    send_attachments(&stream, &fds, deadline)?;
    if read_line(&stream, deadline)?
        != if accounted {
            wire::ACCOUNTED_ATTACHED
        } else {
            wire::ATTACHED
        }
    {
        return Err(invalid());
    }
    Ok(())
}
fn connect_owner(path: &str, deadline: Instant) -> io::Result<CtlStream> {
    if !Path::new(path).is_absolute() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let pinned = crate::pin_alias(path);
    let stream = aterm_uds::connect::until(Path::new(&pinned), deadline)?;
    stream.set_nonblocking(true)?;
    if aterm_uds::peer::peer_uid(&stream) != Some(aterm_uds::peer::our_uid()) {
        return Err(denied());
    }
    let (_, token) = crate::read_token_file_at(&pinned).map_err(|_| denied())?;
    let token = aterm_uds::secret::Secret::from_vec(token.into_bytes());
    let mut hello = b"AUTH ".to_vec();
    hello.extend(token.expose());
    hello.extend(b"\nversion\n");
    let hello = aterm_uds::secret::Secret::from_vec(hello);
    write_all(&stream, hello.expose(), deadline)?;
    if !read_line(&stream, deadline)?.starts_with("OK ") {
        return Err(denied());
    }
    Ok(stream)
}
/// Transfer the exact four rights on an already authenticated private lane.
/// Completion proves descriptor delivery only, never refusal or execution.
pub fn send_attachments(
    stream: &CtlStream,
    fds: &Attachments,
    deadline: Instant,
) -> io::Result<()> {
    loop {
        time(deadline)?;
        match aterm_uds::fdpass::send_with_fds(
            stream,
            &[wire::FD_MARKER],
            &[
                fds.stdin.as_fd(),
                fds.stdout.as_fd(),
                fds.stderr.as_fd(),
                fds.events.as_fd(),
            ],
        ) {
            Ok(1) => return Ok(()),
            Ok(_) => return Err(io::ErrorKind::WriteZero.into()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Write, deadline)?
            }
            Err(e) => return Err(e),
        }
    }
}
/// Kernel-bound peer identity, checked independently from client claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer {
    /// Kernel-authenticated process ID.
    pub pid: u32,
    /// Native process birth scalar.
    pub birth: u64,
}
/// Resolve same-user kernel peer PID and native birth, failing closed.
pub fn local_peer(stream: &CtlStream) -> io::Result<Peer> {
    if aterm_uds::peer::peer_uid(stream) != Some(aterm_uds::peer::our_uid()) {
        return Err(denied());
    }
    let pid = aterm_uds::fdpass::peer_pid(stream)?;
    let birth = aterm_uds::process::read_birth(i32::try_from(pid).map_err(|_| denied())?)
        .ok_or_else(denied)?
        .0;
    Ok(Peer { pid, birth })
}
/// Only returned after owner AUTH, local identity and session gates accepted.
/// Deliberately has no Debug implementation: the request holds private values.
pub struct AuthorizedSubmission {
    /// Validated private request.
    pub request: wire::Submission,
    /// Independently authenticated kernel peer.
    pub peer: Peer,
    /// Exactly four delegated rights.
    pub fds: Attachments,
}
/// Called only by the owner-authenticated control dispatcher. `authorize` must
/// establish the exact session belongs to this live peer, and reject duplicate
/// attempts. This gate runs before admission and before accepting any rights.
/// `owner_authenticated` must mean the exact Instance principal, never a read
/// capability, remote principal, or tokenless compatibility principal.
pub fn receive(
    stream: &CtlStream,
    owner_authenticated: bool,
    deadline: Instant,
    authorize: impl FnOnce(&wire::Submission, Peer) -> bool,
) -> io::Result<AuthorizedSubmission> {
    receive_mode(stream, owner_authenticated, deadline, authorize, false)
}
/// Accounted attachment ingress after exact owner/session authentication.
pub fn receive_accounted(
    stream: &CtlStream,
    owner_authenticated: bool,
    deadline: Instant,
    authorize: impl FnOnce(&wire::Submission, Peer) -> bool,
) -> io::Result<AuthorizedSubmission> {
    receive_mode(stream, owner_authenticated, deadline, authorize, true)
}
fn receive_mode(
    stream: &CtlStream,
    owner_authenticated: bool,
    deadline: Instant,
    authorize: impl FnOnce(&wire::Submission, Peer) -> bool,
    accounted: bool,
) -> io::Result<AuthorizedSubmission> {
    stream.set_nonblocking(true)?;
    if !owner_authenticated {
        return Err(denied());
    }
    let peer = local_peer(stream)?;
    write_all(
        stream,
        format!(
            "{}\n",
            if accounted {
                wire::ACCOUNTED_PREPARE
            } else {
                wire::PREPARE
            }
        )
        .as_bytes(),
        deadline,
    )?;
    let request = wire::Submission::decode(&read_frame(stream, deadline)?)?;
    if request.peer_pid != peer.pid
        || request.peer_birth != peer.birth
        || !authorize(&request, peer)
    {
        return Err(denied());
    }
    if local_peer(stream)? != peer {
        return Err(denied());
    }
    write_all(
        stream,
        format!(
            "{}\n",
            if accounted {
                wire::ACCOUNTED_ADMISSION
            } else {
                wire::ADMISSION
            }
        )
        .as_bytes(),
        deadline,
    )?;
    let fds = receive_attachments(stream, peer, deadline)?;
    if local_peer(stream)? != peer {
        return Err(denied());
    }
    Ok(AuthorizedSubmission { request, peer, fds })
}
/// Receive exactly four rights over an already authenticated delegation lane.
/// `peer` must come from prior native authentication, never untrusted JSON.
/// Independently rechecks the private event socket's actual original peer,
/// including when a trusted GUI forwards the rights to its custodian.
pub fn receive_attachments(
    stream: &CtlStream,
    peer: Peer,
    deadline: Instant,
) -> io::Result<Attachments> {
    let ((stdin, stdout), (stderr, events)) = loop {
        time(deadline)?;
        let mut marker = [0; 2];
        match aterm_uds::kept_out(|| {
            let r = aterm_uds::fdpass::recv_with_fds(stream, &mut marker, 4)?;
            if r.bytes != 1 || marker[0] != wire::FD_MARKER || r.fds.len() != 4 {
                return Err(invalid());
            }
            let [stdin, stdout, stderr, events]: [OwnedFd; 4] =
                r.fds.try_into().map_err(|_| invalid())?;
            Ok(((stdin, stdout), (stderr, events)))
        }) {
            Ok(fds) => break fds,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Read, deadline)?
            }
            Err(e) => return Err(e),
        }
    };
    // Validate that the fourth right really is a connected local same-user
    // socket. It carries event/cancel frames, never pid-based process control.
    let channel = CtlStream::from(events);
    if local_peer(&channel)? != peer {
        return Err(denied());
    }
    channel.set_nonblocking(true)?;
    Ok(Attachments {
        stdin,
        stdout,
        stderr,
        events: channel.into(),
    })
}
/// Acknowledge only after the callback took ownership of the authorized request.
pub fn acknowledge(stream: &CtlStream, deadline: Instant) -> io::Result<()> {
    acknowledge_mode(stream, deadline, false)
}
/// Acknowledge an explicitly accounted attachment, never a launch.
pub fn acknowledge_accounted(stream: &CtlStream, deadline: Instant) -> io::Result<()> {
    acknowledge_mode(stream, deadline, true)
}
fn acknowledge_mode(stream: &CtlStream, deadline: Instant, accounted: bool) -> io::Result<()> {
    write_all(
        stream,
        format!(
            "{}\n",
            if accounted {
                wire::ACCOUNTED_ATTACHED
            } else {
                wire::ATTACHED
            }
        )
        .as_bytes(),
        deadline,
    )
}

/// Write a versioned lifecycle fact on the private channel with an absolute deadline.
pub fn send_event(
    stream: &CtlStream,
    event: &wire::EventFrame,
    deadline: Instant,
) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    write_frame(stream, &event.encode()?, deadline)
}

/// Read an event for exactly this session and attempt. No other binding is accepted.
pub fn read_event(
    stream: &CtlStream,
    binding: &wire::Binding,
    deadline: Instant,
) -> io::Result<wire::EventFrame> {
    stream.set_nonblocking(true)?;
    wire::EventFrame::decode(&read_frame(stream, deadline)?, binding)
}

/// Send a cancellation bound to this session and attempt, never a raw process ID.
pub fn send_cancel(stream: &CtlStream, cancel: wire::Cancel, deadline: Instant) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    write_frame(stream, &cancel.encode()?, deadline)
}

/// Receive a cancellation only for the host's exact expected session and attempt.
pub fn read_cancel(
    stream: &CtlStream,
    binding: &wire::Binding,
    deadline: Instant,
) -> io::Result<wire::Cancel> {
    stream.set_nonblocking(true)?;
    wire::Cancel::decode(&read_frame(stream, deadline)?, binding)
}

#[cfg(test)]
#[path = "resource_admission_transport_tests.rs"]
mod tests;
