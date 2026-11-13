// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Host-owned opaque runtime data, never SDK or execution admission. Receiving
//! data does not consume it: retries and failed replacement startup retain the
//! original host copy until the same kernel process explicitly releases it.
//! Descriptors are immutable data, with a shared file offset: readers must use
//! positional reads. Neither image identity nor SDK permission comes from them.
use super::*;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

const MAX_BYTES: u64 = 256 * 1024 * 1024;
const TRANSFER_TIME: Duration = Duration::from_secs(30);
const REFUSED: &str = "ERR native runtime data unavailable\n";

#[derive(Debug)]
pub(super) struct Slot {
    operation: String,
    owner: Process,
    state: Mutex<State>,
}
#[derive(Debug)]
struct State {
    outcome: Outcome,
    sending: bool,
    pins: usize,
}
#[derive(Debug)]
enum Outcome {
    Reserved,
    Receiving,
    Ready(Arc<Snapshot>),
    Failed,
    Released,
}
#[derive(Debug)]
struct Snapshot {
    file: File,
    size: u64,
    digest: String,
}
impl Snapshot {
    fn copy_from(source: File) -> io::Result<Self> {
        // A source may retain another description. Copy its bytes into the
        // host's own file, rather than treating source read-only flags as a seal.
        let metadata = source.metadata()?;
        // SAFETY: fcntl inspects the borrowed owned descriptor, with no pointers.
        let flags = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if flags & libc::O_ACCMODE != libc::O_RDONLY
            || !metadata.is_file()
            || metadata.nlink() != 0
            || metadata.len() == 0
            || metadata.len() > MAX_BYTES
        {
            return Err(io::Error::other("invalid runtime data descriptor"));
        }
        let directory = aterm_tempfile::Builder::new()
            .prefix("aterm-runtime-data-")
            .suffix(".noindex")
            .tempdir()?;
        let path = directory.path().join("state");
        let mut temporary = File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let mut digest = aterm_digest::Sha256::new();
        let mut offset = 0;
        let mut buffer = [0; 8192];
        while offset < metadata.len() {
            let count = usize::try_from((metadata.len() - offset).min(buffer.len() as u64))
                .map_err(io::Error::other)?;
            let read = source.read_at(&mut buffer[..count], offset)?;
            if read == 0 {
                return Err(io::Error::other("runtime data ended during capture"));
            }
            temporary.write_all(&buffer[..read])?;
            digest.update(&buffer[..read]);
            offset += read as u64;
        }
        let after = source.metadata()?;
        if (metadata.len(), metadata.mtime(), metadata.mtime_nsec())
            != (after.len(), after.mtime(), after.mtime_nsec())
        {
            return Err(io::Error::other("runtime data changed during capture"));
        }
        let file = File::open(&path)?;
        std::fs::remove_file(path)?;
        drop(temporary);
        Ok(Self {
            file,
            size: offset,
            digest: aterm_codec::hex::encode_lower(&digest.finalize()),
        })
    }
}

impl Slot {
    /// Data reservation only. The serving native producer independently proves
    /// its kernel owner before installing this cell in a session.
    pub(super) fn reserved(operation: String, owner: Process) -> Arc<Self> {
        Arc::new(Self {
            operation,
            owner,
            state: Mutex::new(State {
                outcome: Outcome::Reserved,
                sending: false,
                pins: 0,
            }),
        })
    }
    pub(super) fn reusable_by(&self, operation: &str, peer: &Process) -> bool {
        if self.operation == operation {
            return false;
        }
        let gone = i32::try_from(self.owner.pid).ok().is_some_and(|pid| {
            aterm_uds::process::read_birth(pid).is_some_and(|birth| birth.0 != self.owner.birth)
                || matches!(
                    aterm_uds::process::process_presence(pid),
                    aterm_uds::process::ProcessPresence::Absent
                        | aterm_uds::process::ProcessPresence::Exited
                )
        });
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.pins == 0
            && !state.sending
            && ((self.owned_by(peer) && matches!(state.outcome, Outcome::Released))
                || (gone
                    && matches!(
                        state.outcome,
                        Outcome::Reserved | Outcome::Failed | Outcome::Released
                    )))
    }
    pub(super) fn pin(self: &Arc<Self>, operation: &str, peer: &Process) -> Option<Pin> {
        if self.operation != operation || !self.owned_by(peer) {
            return None;
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if matches!(state.outcome, Outcome::Released | Outcome::Failed) {
            return None;
        }
        state.pins = state.pins.checked_add(1)?;
        Some(Pin(self.clone()))
    }
    fn owned_by(&self, peer: &Process) -> bool {
        // Image changes are the purpose of this data channel. PID and birth
        // select the original kernel process, not a new reader or SDK role.
        self.owner.pid == peer.pid && self.owner.birth == peer.birth
    }
    fn status(&self) -> String {
        match &self.state.lock().unwrap_or_else(|p| p.into_inner()).outcome {
            Outcome::Reserved => "OK native runtime-data state=reserved\n".into(),
            Outcome::Receiving => "OK native runtime-data state=receiving\n".into(),
            Outcome::Failed => "ERR native runtime-data state=failed\n".into(),
            Outcome::Released => "OK native runtime-data state=released\n".into(),
            Outcome::Ready(snapshot) => format!(
                "OK native runtime-data state=retained operation={} bytes={} digest={}\n",
                self.operation, snapshot.size, snapshot.digest
            ),
        }
    }
}

/// The native operation owns this custody, independently of request/ACK lifetime.
/// It grants neither execution nor SDK access. A reservation may precede bytes.
#[derive(Debug)]
pub(super) struct Pin(Arc<Slot>);
/// Disposal owner returned to the caller, so file close happens outside native
/// operation, metadata and data-state locks.
pub(super) struct Released {
    _snapshot: Arc<Snapshot>,
}
impl Pin {
    /// Only the owning native handover calls this after live settlement checks.
    /// Concurrent readers/pins or changed DATA retain custody for a later retry.
    pub(super) fn release_retained(&self, bytes: u64, digest: &str) -> Option<Released> {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.pins != 1
            || state.sending
            || !matches!(
                &state.outcome, Outcome::Ready(snapshot) if snapshot.size == bytes && snapshot.digest == digest
            )
        {
            return None;
        }
        let Outcome::Ready(snapshot) = std::mem::replace(&mut state.outcome, Outcome::Released)
        else {
            unreachable!("retained snapshot checked under the same lock");
        };
        Some(Released {
            _snapshot: snapshot,
        })
    }
    pub(super) fn retained(&self) -> Option<(u64, String)> {
        let state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        match &state.outcome {
            Outcome::Ready(snapshot) => Some((snapshot.size, snapshot.digest.clone())),
            _ => None,
        }
    }
}
impl Drop for Pin {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        state.pins -= 1;
    }
}

fn fields(line: &str) -> Option<(&str, BTreeMap<&str, &str>)> {
    let mut words = line.split_ascii_whitespace();
    let action = words.next()?;
    let mut fields = BTreeMap::new();
    for word in words {
        let (name, value) = word.split_once('=')?;
        if value.is_empty() || fields.insert(name, value).is_some() {
            return None;
        }
    }
    Some((action, fields))
}

fn connect(path: &Path, owner: &Process, deadline: Instant) -> io::Result<aterm_uds::CtlStream> {
    let stream = aterm_uds::connect::until(path, deadline)?;
    if aterm_uds::fdpass::peer_pid(&stream)? != owner.pid {
        return Err(io::Error::other("runtime data socket has another owner"));
    }
    let current = Process::observe(owner.pid)
        .ok_or_else(|| io::Error::other("runtime data owner is unreadable"))?;
    if current.birth != owner.birth {
        return Err(io::Error::other("runtime data owner changed"));
    }
    stream.set_nonblocking(true)?;
    Ok(stream)
}

fn receive(path: &Path, owner: &Process) -> io::Result<Snapshot> {
    let deadline = Instant::now() + TRANSFER_TIME;
    let stream = connect(path, owner, deadline)?;
    let mut marker = [0];
    let mut received = loop {
        aterm_uds::ready::until(stream.as_fd(), aterm_uds::ready::Interest::Read, deadline)?;
        match aterm_uds::fdpass::recv_with_fds(&stream, &mut marker, 1) {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            result => break result?,
        }
    };
    if received.bytes != 1 || marker != *b"S" || received.fds.len() != 1 {
        return Err(io::Error::other("invalid runtime data frame"));
    }
    Snapshot::copy_from(File::from(received.fds.remove(0)))
}

fn send(path: &Path, owner: &Process, snapshot: &Snapshot) -> io::Result<()> {
    let deadline = Instant::now() + TRANSFER_TIME;
    let stream = connect(path, owner, deadline)?;
    let sent = loop {
        aterm_uds::ready::until(stream.as_fd(), aterm_uds::ready::Interest::Write, deadline)?;
        match aterm_uds::fdpass::send_with_fds(&stream, b"S", &[snapshot.file.as_fd()]) {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            result => break result?,
        }
    };
    if sent != 1 {
        return Err(io::Error::other("runtime data descriptor was not sent"));
    }
    Ok(())
}

pub(super) fn serve(ctx: &Arc<crate::SessionCtx>, master: i32, line: &str) -> String {
    if line == "info" {
        return format!(
            "OK native runtime-data protocol=1 max-bytes={MAX_BYTES} authority=data-only\n"
        );
    }
    if line == "discover" {
        let Some(peer) = crate::control::serving_peer_pid().and_then(Process::observe) else {
            return REFUSED.into();
        };
        let slot = ctx
            .fenced_writes
            .native
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .runtime_data
            .clone();
        return match slot.filter(|slot| slot.owned_by(&peer)) {
            Some(slot) => slot.status(),
            None => "OK native runtime-data state=absent\n".into(),
        };
    }
    let Some((action, mut fields)) = fields(line) else {
        return REFUSED.into();
    };
    let Some(operation) = fields.remove("operation").filter(|word| {
        word.len() == 32
            && word
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) else {
        return REFUSED.into();
    };
    let socket = fields
        .remove("socket")
        .map(aterm_codec::pct::decode)
        .map(PathBuf::from);
    if !fields.is_empty()
        || !matches!(
            action,
            "reserve" | "retain" | "status" | "fetch" | "release"
        )
        || (matches!(action, "retain" | "fetch") != socket.is_some())
        || socket.as_ref().is_some_and(|path| !path.is_absolute())
    {
        return REFUSED.into();
    }
    let Some(peer) = crate::control::serving_peer_pid().and_then(Process::observe) else {
        return REFUSED.into();
    };
    let previous = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .runtime_data
        .clone();
    let existing = match &previous {
        Some(slot) => {
            if matches!(action, "reserve" | "retain") && slot.reusable_by(operation, &peer) {
                None
            } else if slot.owned_by(&peer) && slot.operation == operation {
                Some(slot.clone())
            } else {
                return REFUSED.into();
            }
        }
        None => None,
    };
    let slot = match existing {
        Some(slot) => slot,
        None => {
            if !matches!(action, "reserve" | "retain")
                || crate::control::prove_self_claim_for(
                    ctx,
                    &format!("codex-alab@{}", peer.pid),
                    master,
                )
                .is_none()
            {
                return REFUSED.into();
            }
            let mut table = ctx
                .fenced_writes
                .native
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if !match (&table.runtime_data, &previous) {
                (None, None) => true,
                (Some(current), Some(previous)) => {
                    Arc::ptr_eq(current, previous) && current.reusable_by(operation, &peer)
                }
                _ => false,
            } {
                return REFUSED.into();
            }
            let slot = Slot::reserved(operation.into(), peer.clone());
            table.runtime_data = Some(slot.clone());
            drop(table);
            slot
        }
    };
    match action {
        "reserve" | "status" => slot.status(),
        "retain" => {
            let mut state = slot.state.lock().unwrap_or_else(|p| p.into_inner());
            if !matches!(state.outcome, Outcome::Reserved | Outcome::Failed) {
                drop(state);
                return slot.status();
            }
            state.outcome = Outcome::Receiving;
            drop(state);
            let worker = slot.clone();
            let path = socket.expect("retain requires socket");
            if std::thread::Builder::new()
                .name("codex-runtime-retain".into())
                .spawn(move || {
                    // Caller cancellation/exit does not cancel an accepted copy.
                    let result = receive(&path, &worker.owner);
                    worker
                        .state
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .outcome = match result {
                        Ok(snapshot) => Outcome::Ready(Arc::new(snapshot)),
                        Err(_) => Outcome::Failed,
                    };
                })
                .is_err()
            {
                slot.state.lock().unwrap_or_else(|p| p.into_inner()).outcome = Outcome::Failed;
            }
            slot.status()
        }
        "release" => {
            let old = {
                let mut state = slot.state.lock().unwrap_or_else(|p| p.into_inner());
                if state.sending || state.pins != 0 || matches!(state.outcome, Outcome::Receiving) {
                    return "OK native runtime-data state=busy\n".into();
                }
                std::mem::replace(&mut state.outcome, Outcome::Released)
            };
            // File disposal holds neither the operation table nor state lock.
            drop(old);
            slot.status()
        }
        "fetch" => {
            let snapshot = {
                let mut state = slot.state.lock().unwrap_or_else(|p| p.into_inner());
                match &state.outcome {
                    Outcome::Ready(snapshot) if !state.sending => {
                        let snapshot = snapshot.clone();
                        state.sending = true;
                        Some(snapshot)
                    }
                    _ => None,
                }
            };
            let Some(snapshot) = snapshot else {
                return slot.status();
            };
            let sending = Sending(slot.clone());
            let path = socket.expect("fetch requires socket");
            let owner = peer;
            if std::thread::Builder::new()
                .name("codex-runtime-fetch".into())
                .spawn(move || {
                    let _sending = sending;
                    let _ = send(&path, &owner, &snapshot);
                })
                .is_err()
            {
                return REFUSED.into();
            }
            "OK native runtime-data state=sending\n".into()
        }
        _ => REFUSED.into(),
    }
}

struct Sending(Arc<Slot>);
impl Drop for Sending {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .sending = false;
    }
}

#[cfg(test)]
#[path = "native_runtime_data_tests.rs"]
mod tests;
