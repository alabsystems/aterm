// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One-use native descriptor handoff before an app-band job's GO.

use std::fs::File;
use std::io;
use std::net::Shutdown;
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::window_guards::{MemoryGuard, WindowGuards};
use aterm_uds::{CtlListener, CtlStream, fdpass, ready};

const SOCKET: &str = "band.guard.sock";
const OFFER: &str = "aterm-lab-window-guards-v2:";
const ACK: &str = "aterm-lab-window-guards-ack-v2:";
const MAX_MESSAGE: usize = 256;

pub(super) struct Request {
    parent: u32,
    nonce: String,
    memory: Option<String>,
}

impl Request {
    pub(super) fn parse(parent: &str, nonce: &str, memory: Option<&str>) -> io::Result<Self> {
        let parent = decimal_pid(parent)?;
        if nonce.len() != 32
            || !nonce
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid("invalid app-band guard nonce"));
        }
        if let Some(memory) = memory {
            WindowGuards::arguments(&[3, 4], Some(memory)).map_err(|why| invalid(&why))?;
        }
        Ok(Self {
            parent,
            nonce: nonce.into(),
            memory: memory.map(str::to_owned),
        })
    }

    pub(super) fn receive(&self, dir: &Path, deadline: Instant) -> io::Result<WindowGuards> {
        aterm_uds::private_file::check_private_dir(dir)?;
        let stream = aterm_uds::connect::until(&dir.join(SOCKET), deadline)?;
        stream.set_nonblocking(true)?;
        check_peer(&stream, self.parent)?;
        let nonce = self.bound_nonce();
        let count = if self.memory.is_some() { 2 } else { 1 };
        let fds = receive_exact(&stream, OFFER, &nonce, count, deadline)?;
        let guards =
            WindowGuards::from_received(fds, self.memory.clone()).map_err(|why| invalid(&why))?;
        validate_guards(&guards)?;
        // This ACK proves receipt and continued custody, not a new admission. The original
        // parent keeps the original close-only lock throughout. Keeper performs inherited
        // lock validation after exec; no fresh-path lock is acquired here.
        send(&stream, &message(ACK, &nonce), &[], deadline)?;
        stream.shutdown(Shutdown::Write)?;
        Ok(guards)
    }

    fn bound_nonce(&self) -> String {
        format!("{}\n{}", self.nonce, self.memory.as_deref().unwrap_or("-"))
    }
}

pub(super) struct Offer {
    listener: CtlListener,
    directory: File,
    path: PathBuf,
    identity: (u64, u64),
    request: Request,
    guards: WindowGuards,
}

impl Offer {
    pub(super) fn new(dir: &Path, guards: WindowGuards) -> io::Result<Self> {
        aterm_uds::private_file::check_private_dir(dir)?;
        validate_guards(&guards)?;
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir)?;
        let path = dir.join(SOCKET);
        // bind is exclusive: never unlink or adopt an existing rendezvous.
        let listener = aterm_uds::kept_out(|| CtlListener::bind(&path))?;
        aterm_uds::atomic_file::set_mode_nofollow(&path, 0o600)?;
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_socket() {
            return Err(invalid("app-band guard rendezvous is not a socket"));
        }
        let offer = Self {
            listener,
            directory,
            path,
            identity: (metadata.dev(), metadata.ino()),
            request: Request {
                parent: std::process::id(),
                nonce: aterm_uds::rand::hex_token::<16>()?,
                memory: guards.claim().map(str::to_owned),
            },
            guards,
        };
        offer.validate()?;
        Ok(offer)
    }

    pub(super) fn arguments(&self) -> Vec<String> {
        let mut args = vec![
            "--guard-parent".into(),
            self.request.parent.to_string(),
            "--guard-nonce".into(),
            self.request.nonce.clone(),
        ];
        if let Some(memory) = &self.request.memory {
            args.extend(["--guard-memory-leaf".to_owned(), memory.clone()]);
        }
        args
    }

    fn validate(&self) -> io::Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| invalid("guard has no parent"))?;
        aterm_uds::private_file::check_private_dir(parent)?;
        let named = std::fs::symlink_metadata(parent)?;
        let held = self.directory.metadata()?;
        let socket = std::fs::symlink_metadata(&self.path)?;
        if (named.dev(), named.ino()) != (held.dev(), held.ino())
            || !socket.file_type().is_socket()
            || (socket.dev(), socket.ino()) != self.identity
            || socket.mode() & 0o777 != 0o600
            || socket.uid() != crate::sys::getuid()
        {
            return Err(invalid("original app-band guard rendezvous changed"));
        }
        Ok(())
    }

    pub(super) fn transfer(&self, pid: u32, label: &str, deadline: Instant) -> io::Result<()> {
        self.validate()?;
        let (stream, _) = aterm_uds::accept(
            &self.listener,
            aterm_uds::AcceptWait::Until(deadline),
            aterm_uds::AcceptedMode::Nonblocking,
        )?;
        check_peer(&stream, pid)?;
        // The PID file is a readiness rendezvous, never descriptor authority. Require the
        // actual launchd job's current PID as well as the connected kernel credential.
        if launchd_pid(label, deadline)? != pid {
            return Err(invalid(
                "app-band guard peer is not the current launchd job",
            ));
        }
        self.validate()?;
        validate_guards(&self.guards)?;
        let nonce = self.request.bound_nonce();
        send(
            &stream,
            &message(OFFER, &nonce),
            &self.guards.borrowed(),
            deadline,
        )?;
        stream.shutdown(Shutdown::Write)?;
        receive_exact(&stream, ACK, &nonce, 0, deadline)?;
        self.validate()?;
        Ok(())
    }
}

impl Drop for Offer {
    fn drop(&mut self) {
        if self.validate().is_ok() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn validate_guard(guard: &OwnedFd) -> io::Result<()> {
    let ledger =
        aterm_capacity::Ledger::user().ok_or_else(|| invalid("no account capacity ledger"))?;
    aterm_uds::private_file::check_private_dir(ledger.dir())?;
    let path = ledger.dir().join(aterm_types::lab_window_guard::LOCK_NAME);
    let named = std::fs::symlink_metadata(path)?;
    let held = aterm_uds::spawnfd::descriptor_metadata(guard.as_raw_fd())?;
    if guard.as_raw_fd() <= 2
        || !held.is_file()
        || !named.is_file()
        || held.uid() != crate::sys::getuid()
        || held.mode() & 0o777 != 0o600
        || held.nlink() != 1
        || named.nlink() != 1
        || (held.dev(), held.ino()) != (named.dev(), named.ino())
    {
        return Err(invalid(
            "app-band guard is not the original account window lane",
        ));
    }
    Ok(())
}

fn validate_guards(guards: &WindowGuards) -> io::Result<()> {
    validate_guard(&guards.lane)?;
    if let Some(MemoryGuard { descriptor, claim }) = &guards.memory {
        WindowGuards::arguments(&[3, 4], Some(claim)).map_err(|why| invalid(&why))?;
        let ledger =
            aterm_capacity::Ledger::user().ok_or_else(|| invalid("no original memory ledger"))?;
        aterm_uds::private_file::check_private_dir(ledger.dir())?;
        let named = std::fs::symlink_metadata(ledger.dir().join(claim))?;
        let held = aterm_uds::spawnfd::descriptor_metadata(descriptor.as_raw_fd())?;
        let lane = aterm_uds::spawnfd::descriptor_metadata(guards.lane.as_raw_fd())?;
        if descriptor.as_raw_fd() <= 2
            || !held.is_file()
            || !named.is_file()
            || held.uid() != crate::sys::getuid()
            || held.mode() & 0o777 != 0o600
            || held.nlink() != 1
            || named.nlink() != 1
            || (held.dev(), held.ino()) != (named.dev(), named.ino())
            || (held.dev(), held.ino()) == (lane.dev(), lane.ino())
        {
            return Err(invalid(
                "memory role is not the original separate account claim",
            ));
        }
        // ACK proves exact descriptor receipt, not new memory authority. Keeper/core
        // validate the persisted Detached record before any GUI can start.
    }
    Ok(())
}

fn check_peer(stream: &CtlStream, expected: u32) -> io::Result<()> {
    if expected <= 1 || fdpass::peer_pid(stream)? != expected {
        return Err(invalid("wrong app-band guard peer"));
    }
    Ok(())
}

pub(super) fn launchd_pid(label: &str, deadline: Instant) -> io::Result<u32> {
    let mut command = aterm_uds::spawnfd::command(super::LAUNCHCTL);
    command.arg("print").arg(super::target(label));
    let output = super::cleanup::output("job identity", &mut command, deadline)
        .map_err(|error| io::Error::other(format!("cannot inspect app-band job: {error}")))?;
    if !output.status.success() {
        return Err(invalid(
            "launchd cannot identify the app-band guard receiver",
        ));
    }
    parse_launchd_pid(
        std::str::from_utf8(&output.stdout).map_err(|_| invalid("non-UTF8 job state"))?,
    )
}

fn parse_launchd_pid(text: &str) -> io::Result<u32> {
    let mut fields = text
        .lines()
        .filter_map(|line| line.strip_prefix("\tpid = "));
    let pid = decimal_pid(
        fields
            .next()
            .ok_or_else(|| invalid("launchd job has no PID"))?
            .trim_end(),
    )?;
    if fields.next().is_some() {
        return Err(invalid("launchd job has duplicate PID fields"));
    }
    Ok(pid)
}

fn decimal_pid(text: &str) -> io::Result<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid("invalid app-band guard PID"));
    }
    text.parse::<u32>()
        .ok()
        .filter(|pid| *pid > 1)
        .ok_or_else(|| invalid("invalid app-band guard PID"))
}

fn message(domain: &str, nonce: &str) -> Vec<u8> {
    format!("{domain}{nonce}").into_bytes()
}

fn send(
    stream: &CtlStream,
    payload: &[u8],
    fds: &[std::os::fd::BorrowedFd<'_>],
    deadline: Instant,
) -> io::Result<()> {
    let mut sent = 0;
    while sent < payload.len() {
        ready::until(stream.as_fd(), ready::Interest::Write, deadline)?;
        match fdpass::send_with_fds(stream, &payload[sent..], if sent == 0 { fds } else { &[] }) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => sent += count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn receive(
    stream: &CtlStream,
    max_fds: usize,
    deadline: Instant,
) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut payload = Vec::new();
    let mut fds = Vec::new();
    loop {
        ready::until(stream.as_fd(), ready::Interest::Read, deadline)?;
        let mut buffer = [0; MAX_MESSAGE + 1];
        match fdpass::recv_with_fds(stream, &mut buffer, max_fds.saturating_sub(fds.len())) {
            Ok(received) => {
                fds.extend(received.fds);
                if received.bytes == 0 {
                    return Ok((payload, fds));
                }
                if payload.len().saturating_add(received.bytes) > MAX_MESSAGE {
                    return Err(invalid("oversized app-band guard message"));
                }
                payload.extend_from_slice(&buffer[..received.bytes]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error),
        }
    }
}

fn receive_exact(
    stream: &CtlStream,
    domain: &str,
    nonce: &str,
    expected_fds: usize,
    deadline: Instant,
) -> io::Result<Vec<OwnedFd>> {
    let (bytes, fds) = receive(stream, expected_fds, deadline)?;
    if bytes != message(domain, nonce) || fds.len() != expected_fds {
        return Err(invalid(
            "invalid app-band guard message or descriptor count",
        ));
    }
    Ok(fds)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
#[path = "band_guard_tests.rs"]
mod tests;
