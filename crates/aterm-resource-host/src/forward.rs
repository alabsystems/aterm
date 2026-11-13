// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GUI delegation over its privately inherited custodian socket. Public
//! listeners must never accept this protocol or let same-UID callers assert
//! another process's authenticated identity.
use aterm_ctl::resource_admission_transport::{self as wire, AuthorizedSubmission, Peer};
use aterm_types::resource_admission::Submission;
use aterm_uds::CtlStream;
use std::{
    io,
    os::fd::{AsFd as _, OwnedFd},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const DEADLINE: Duration = Duration::from_secs(60);
const CAPABILITIES: u8 = 1;
const AUTHORIZE: u8 = 2;
const INSPECT: u8 = 3;
const SUBMIT: u8 = 4;
const SUBMIT_ACCOUNTED: u8 = 5;
const EXPECT_STORE: u8 = 6;
const SUBMIT_ACCOUNTED_EXPECT_STORE: u8 = 7;

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "custodian delegation unavailable",
    )
}
fn current(peer: Peer) -> bool {
    i32::try_from(peer.pid)
        .ok()
        .and_then(aterm_uds::process::read_birth)
        == Some(aterm_uds::process::Birth(peer.birth))
}

/// Construct only from the private endpoint retained by the actual launcher
/// after validating the spawned immutable custodian image and native birth.
pub(crate) struct Forwarder {
    stream: Mutex<CtlStream>,
    custodian: Peer,
    broken: AtomicBool,
}
impl Forwarder {
    pub(crate) fn is_broken(&self) -> bool {
        self.broken.load(Ordering::Acquire)
    }
    pub(crate) fn new(stream: CtlStream, custodian: Peer) -> io::Result<Self> {
        if !current(custodian) {
            return Err(refused());
        }
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream: Mutex::new(stream),
            custodian,
            broken: AtomicBool::new(false),
        })
    }
    fn with<T>(&self, f: impl FnOnce(&CtlStream, Instant) -> io::Result<T>) -> io::Result<T> {
        self.with_at(Instant::now(), f)
    }
    fn with_at<T>(
        &self,
        started: Instant,
        f: impl FnOnce(&CtlStream, Instant) -> io::Result<T>,
    ) -> io::Result<T> {
        // UI control threads never queue behind an unbounded mutex wait.
        let stream = self.stream.try_lock().map_err(|_| refused())?;
        if self.broken.load(Ordering::Acquire) {
            return Err(refused());
        }
        let result = if current(self.custodian) {
            f(&stream, started + DEADLINE).and_then(|value| {
                if current(self.custodian) {
                    Ok(value)
                } else {
                    Err(refused())
                }
            })
        } else {
            Err(refused())
        };
        if result.is_err() {
            // A partial frame or lost ACK cannot be resynchronized or retried
            // as a fresh request. Permanently retire this private lane.
            self.broken.store(true, Ordering::Release);
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        result
    }
    pub(crate) fn capabilities(&self) -> io::Result<aterm_json::Value> {
        self.with(|stream, deadline| {
            wire::write_frame(stream, &[CAPABILITIES], deadline)?;
            json_reply(stream, deadline)
        })
    }
    pub(crate) fn authorize(&self, request: &Submission) -> io::Result<bool> {
        self.with(|stream, deadline| {
            let mut frame = vec![AUTHORIZE];
            frame.extend(request.encode()?);
            wire::write_frame(stream, &frame, deadline)?;
            match wire::read_frame(stream, deadline)?.as_slice() {
                [0] => Ok(false),
                [1] => Ok(true),
                _ => Err(refused()),
            }
        })
    }
    pub(crate) fn inspect(&self, peer: Peer, session: &str) -> io::Result<aterm_json::Value> {
        self.with(|stream, deadline| {
            let mut frame = vec![INSPECT];
            frame.extend(peer.pid.to_be_bytes());
            frame.extend(peer.birth.to_be_bytes());
            frame.extend(session.as_bytes());
            wire::write_frame(stream, &frame, deadline)?;
            json_reply(stream, deadline)
        })
    }
    pub(crate) fn submit(&self, submission: AuthorizedSubmission) -> io::Result<()> {
        self.submit_mode(submission, false)
    }
    pub(crate) fn submit_accounted(&self, submission: AuthorizedSubmission) -> io::Result<()> {
        self.submit_mode(submission, true)
    }
    pub(crate) fn matches_original_store(&self, store: &OwnedFd) -> io::Result<bool> {
        self.with(|stream, deadline| {
            wire::write_frame(stream, &[EXPECT_STORE], deadline)?;
            wire::store_expectation::send(stream, store.as_fd(), deadline)?;
            match wire::read_frame(stream, deadline)?.as_slice() {
                [0] => Ok(false),
                [1] => Ok(true),
                _ => Err(refused()),
            }
        })
    }
    pub(crate) fn submit_accounted_expected(
        &self,
        submission: AuthorizedSubmission,
        store: OwnedFd,
    ) -> io::Result<()> {
        self.with(|stream, deadline| {
            if !(wire::store_expectation::eligible(&submission.request)
                || wire::native_store::eligible(&submission.request))
                || submission.request.peer_pid != submission.peer.pid
                || submission.request.peer_birth != submission.peer.birth
            {
                return Err(refused());
            }
            let mut frame = vec![SUBMIT_ACCOUNTED_EXPECT_STORE];
            frame.extend(submission.request.encode()?);
            wire::write_frame(stream, &frame, deadline)?;
            wire::store_expectation::send(stream, store.as_fd(), deadline)?;
            if wire::read_frame(stream, deadline)?.as_slice() != [1] {
                return Err(refused());
            }
            wire::send_attachments(stream, &submission.fds, deadline)?;
            if wire::read_frame(stream, deadline)?.as_slice() != [1] {
                return Err(refused());
            }
            Ok(())
        })
    }
    fn submit_mode(&self, submission: AuthorizedSubmission, accounted: bool) -> io::Result<()> {
        self.with(|stream, deadline| {
            if submission.request.peer_pid != submission.peer.pid
                || submission.request.peer_birth != submission.peer.birth
            {
                return Err(refused());
            }
            let mut frame = vec![if accounted { SUBMIT_ACCOUNTED } else { SUBMIT }];
            frame.extend(submission.request.encode()?);
            wire::write_frame(stream, &frame, deadline)?;
            wire::send_attachments(stream, &submission.fds, deadline)?;
            // Failure after rights delivery is ambiguous; the original
            // client's private lifecycle channel remains the outcome source.
            if wire::read_frame(stream, deadline)?.as_slice() != [1] {
                return Err(refused());
            }
            Ok(())
        })
    }
}

fn json_reply(stream: &CtlStream, deadline: Instant) -> io::Result<aterm_json::Value> {
    let frame = wire::read_frame(stream, deadline)?;
    if frame.len() > 8192 {
        return Err(refused());
    }
    aterm_json::from_slice(&frame).map_err(|_| refused())
}

#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
#[path = "forward_store_tests.rs"]
mod store_tests;
fn json_write(stream: &CtlStream, value: &aterm_json::Value, deadline: Instant) -> io::Result<()> {
    let frame = aterm_json::to_vec(value).map_err(|_| refused())?;
    if frame.len() > 8192 {
        return Err(refused());
    }
    wire::write_frame(stream, &frame, deadline)
}

/// Process-private serving loop. `authenticate` must validate the original
/// launcher against the immutable maintained executable and original native
/// lifetime before every request. This function does not authenticate a public
/// same-user connection and must never be installed on a public listener.
pub(crate) fn serve(
    stream: &CtlStream,
    mut authenticate: impl FnMut(Peer) -> io::Result<()>,
) -> io::Result<()> {
    let original = wire::local_peer(stream)?;
    authenticate(original)?;
    loop {
        // Idle time is not a transaction timeout. An incomplete transaction
        // gets one absolute bound once readable; EOF ends this GUI lane only.
        match aterm_uds::ready::until(
            std::os::fd::AsFd::as_fd(stream),
            aterm_uds::ready::Interest::Read,
            Instant::now() + Duration::from_secs(1),
        ) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::TimedOut => continue,
            Err(error) => return Err(error),
        }
        let deadline = Instant::now() + DEADLINE;
        if wire::local_peer(stream)? != original {
            return Err(refused());
        }
        authenticate(original)?;
        let frame = wire::read_frame(stream, deadline)?;
        let (op, body) = frame.split_first().ok_or_else(refused)?;
        match *op {
            EXPECT_STORE if body.is_empty() => {
                let expected = wire::store_expectation::receive(stream, deadline)?;
                authenticate(original)?;
                let matches =
                    crate::runtime::matches_original_store(expected.as_fd()).unwrap_or(false);
                wire::write_frame(stream, &[u8::from(matches)], deadline)?;
            }
            CAPABILITIES if body.is_empty() => {
                json_write(stream, &crate::actor::capabilities(), deadline)?
            }
            AUTHORIZE | SUBMIT | SUBMIT_ACCOUNTED | SUBMIT_ACCOUNTED_EXPECT_STORE => {
                let request = Submission::decode(body)?;
                let peer = Peer {
                    pid: request.peer_pid,
                    birth: request.peer_birth,
                };
                if !current(peer) {
                    return Err(refused());
                }
                if *op == AUTHORIZE {
                    let accepted = crate::actor::authorize(&request, peer);
                    wire::write_frame(stream, &[u8::from(accepted)], deadline)?;
                } else {
                    let expectation = if *op == SUBMIT_ACCOUNTED_EXPECT_STORE {
                        let expected = wire::store_expectation::receive(stream, deadline)?;
                        let matches = (wire::store_expectation::eligible(&request)
                            || wire::native_store::eligible(&request))
                            && crate::runtime::matches_original_store(expected.as_fd())
                                .unwrap_or(false);
                        wire::write_frame(stream, &[u8::from(matches)], deadline)?;
                        if !matches {
                            continue;
                        }
                        Some(expected)
                    } else {
                        None
                    };
                    let fds = wire::receive_attachments(stream, peer, deadline)?;
                    authenticate(original)?;
                    if wire::local_peer(stream)? != original {
                        return Err(refused());
                    }
                    let submission = AuthorizedSubmission { request, peer, fds };
                    if let Some(expected) = expectation {
                        if !crate::runtime::matches_original_store(expected.as_fd())? {
                            return Err(refused());
                        }
                        crate::actor::submit_accounted(submission)?;
                    } else if *op == SUBMIT_ACCOUNTED {
                        crate::actor::submit_accounted(submission)?;
                    } else {
                        crate::actor::submit(submission)?;
                    }
                    wire::write_frame(stream, &[1], deadline)?;
                }
            }
            INSPECT if (14..=76).contains(&body.len()) => {
                let pid = u32::from_be_bytes(body[..4].try_into().map_err(|_| refused())?);
                let birth = u64::from_be_bytes(body[4..12].try_into().map_err(|_| refused())?);
                let session = std::str::from_utf8(&body[12..]).map_err(|_| refused())?;
                let value = crate::actor::inspect(Peer { pid, birth }, session)?.json()?;
                json_write(stream, &value, deadline)?;
            }
            _ => return Err(refused()),
        }
    }
}

#[cfg(test)]
#[path = "forward_tests.rs"]
mod tests;
