// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Original Store directory is an expectation on an independently selected
//! enrolled backend. It never selects a store, pool, SDK or admission authority.
use super::*;
use std::os::fd::BorrowedFd;

/// Advertised only by an actual backend retaining its original Store directory.
pub const CAPABILITY: &str = "native-job-original-store-fd-v1";
/// Distinct accounted ingress; old attachment bytes remain unchanged.
pub const COMMAND: &str = "attach-accounted-native-job-original-store-v1";
const REQUEST: &str = "resource-admission-v3 attach-accounted-native-job-original-store-v1\n";
const PREPARE: &str = "OK resource-admission-v3 prepare-accounted-native-job-original-store-v1";
const EXPECT: &str = "OK resource-admission-v3 expect-native-job-original-store-v1";
const ADMISSION: &str = "OK resource-admission-v3 admitted-accounted-native-job-original-store-v1";
const ATTACHED: &str = "OK resource-admission-v3 attached-accounted-native-job-original-store-v1";
const MARKER: u8 = 0x66;

/// Native jobs have a distinct ingress; Workload40 and other roots refuse.
pub fn eligible(request: &wire::Submission) -> bool {
    matches!(request.argv.as_slice(), [_, entry] if entry == b"--resource-native-job-v1")
        && request.cpu_units > 0
        && request.memory_bytes > 0
        && request.timeout_ms.is_some_and(|value| value > 0)
}

/// Transfer one held expectation, without interpreting its path or contents.
pub fn send(stream: &CtlStream, store: BorrowedFd<'_>, deadline: Instant) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    loop {
        time(deadline)?;
        match aterm_uds::fdpass::send_with_fds(stream, &[MARKER], &[store]) {
            Ok(1) => return Ok(()),
            Ok(_) => return Err(io::ErrorKind::WriteZero.into()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Write, deadline)?;
            }
            Err(error) => return Err(error),
        }
    }
}
/// Consume exactly one received descriptor. Eligibility and original backend
/// identity belong to the actual custodian, not to this transport codec.
pub fn receive(stream: &CtlStream, deadline: Instant) -> io::Result<OwnedFd> {
    stream.set_nonblocking(true)?;
    loop {
        time(deadline)?;
        let received = aterm_uds::kept_out(|| {
            let mut marker = [0; 2];
            let received = aterm_uds::fdpass::recv_with_fds(stream, &mut marker, 64)?;
            if received.bytes != 1 || marker[0] != MARKER || received.fds.len() != 1 {
                return Err(invalid());
            }
            received.fds.into_iter().next().ok_or_else(invalid)
        });
        match received {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Read, deadline)?;
            }
            Err(error) => return Err(error),
        }
    }
}

/// No retry: rights delivery may precede a lost final acknowledgement.
pub fn attach(
    path: &str,
    request: &wire::Submission,
    fds: Attachments,
    store: OwnedFd,
    deadline: Instant,
) -> io::Result<()> {
    request.validate()?;
    if !eligible(request) {
        return Err(denied());
    }
    let stream = connect_owner(path, deadline)?;
    write_all(&stream, REQUEST.as_bytes(), deadline)?;
    if read_line(&stream, deadline)? != PREPARE {
        return Err(denied());
    }
    write_frame(&stream, &request.encode()?, deadline)?;
    if read_line(&stream, deadline)? != EXPECT {
        return Err(denied());
    }
    send(&stream, store.as_fd(), deadline)?;
    if read_line(&stream, deadline)? != ADMISSION {
        return Err(denied());
    }
    send_attachments(&stream, &fds, deadline)?;
    if read_line(&stream, deadline)? != ATTACHED {
        return Err(invalid());
    }
    Ok(())
}

/// Exact local-owner/session authorization precedes expectation transfer; the
/// independently enrolled backend must match before any root rights/admission.
pub fn receive_submission(
    stream: &CtlStream,
    owner_authenticated: bool,
    deadline: Instant,
    authorize: impl FnOnce(&wire::Submission, Peer) -> bool,
    matches_backend: impl FnOnce(&OwnedFd) -> io::Result<bool>,
) -> io::Result<(AuthorizedSubmission, OwnedFd)> {
    stream.set_nonblocking(true)?;
    if !owner_authenticated {
        return Err(denied());
    }
    let peer = local_peer(stream)?;
    write_all(stream, format!("{PREPARE}\n").as_bytes(), deadline)?;
    let request = wire::Submission::decode(&read_frame(stream, deadline)?)?;
    if !eligible(&request)
        || request.peer_pid != peer.pid
        || request.peer_birth != peer.birth
        || !authorize(&request, peer)
        || local_peer(stream)? != peer
    {
        return Err(denied());
    }
    write_all(stream, format!("{EXPECT}\n").as_bytes(), deadline)?;
    let store = receive(stream, deadline)?;
    if !matches_backend(&store)? || local_peer(stream)? != peer {
        return Err(denied());
    }
    write_all(stream, format!("{ADMISSION}\n").as_bytes(), deadline)?;
    let fds = receive_attachments(stream, peer, deadline)?;
    if local_peer(stream)? != peer {
        return Err(denied());
    }
    Ok((AuthorizedSubmission { request, peer, fds }, store))
}
/// Attachment only; an actual owning Started/terminal remains authoritative.
pub fn acknowledge(stream: &CtlStream, deadline: Instant) -> io::Result<()> {
    write_all(stream, format!("{ATTACHED}\n").as_bytes(), deadline)
}

#[cfg(test)]
#[path = "resource_admission_native_store_tests.rs"]
mod tests;
