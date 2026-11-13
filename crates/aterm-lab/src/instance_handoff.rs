// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Follow only peers holding the continuously retained original window description.

use std::fs::File;
use std::io;
use std::os::fd::AsFd as _;
use std::os::unix::fs::{FileExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_toolchain::record_lock::CloseOnlyLock;
use aterm_uds::process::{self, Identity, ProcessPresence};

const HANG: Duration = Duration::from_secs(60);
const PROBE: Duration = Duration::from_secs(1);

struct Token {
    path: PathBuf,
    file: File,
    identity: (u64, u64),
    value: aterm_uds::secret::Secret,
}

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

fn token_bytes(file: &File) -> io::Result<Vec<u8>> {
    let len = usize::try_from(file.metadata()?.len()).map_err(|_| invalid("token size"))?;
    if len == 0 || len > 256 {
        return Err(invalid("token size"));
    }
    let mut bytes = vec![0; len];
    file.read_exact_at(&mut bytes, 0)?;
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes.pop();
    }
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return Err(invalid("invalid native control token"));
    }
    Ok(bytes)
}

impl Token {
    fn capture(socket: &Path) -> io::Result<Self> {
        let name = socket
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| invalid("socket name"))?;
        let path = socket.with_file_name(aterm_types::control_socket::token_name_for_sock(name));
        let (file, metadata) = aterm_uds::regular_file::open(&path)?;
        let identity = (metadata.dev(), metadata.ino());
        let value = aterm_uds::secret::Secret::from_vec(token_bytes(&file)?);
        let token = Self {
            path,
            file,
            identity,
            value,
        };
        if !token.current()? {
            return Err(invalid("control token disappeared"));
        }
        Ok(token)
    }

    fn current(&self) -> io::Result<bool> {
        let metadata = match std::fs::symlink_metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        if !metadata.is_file()
            || (metadata.dev(), metadata.ino()) != self.identity
            || token_bytes(&self.file)? != self.value.expose()
        {
            return Err(invalid("original control token changed"));
        }
        Ok(true)
    }
}

pub(super) struct Tracking {
    original: CloseOnlyLock,
    token: Token,
    last: Option<Identity>,
    owned: Vec<Identity>,
    pending: Option<Instant>,
    refusal: Option<String>,
}

impl Tracking {
    pub fn new(
        original: CloseOnlyLock,
        socket: &Path,
        seed: Option<Identity>,
    ) -> Result<Self, String> {
        Ok(Self {
            original,
            token: Token::capture(socket).map_err(|e| e.to_string())?,
            last: seed,
            owned: seed.into_iter().collect(),
            pending: None,
            refusal: None,
        })
    }

    pub fn pid(&self) -> Option<i32> {
        let last = self.last?;
        (process::read_identity(last.pid) == Some(last)).then_some(last.pid)
    }

    pub fn identity(&self) -> Option<Identity> {
        self.last
    }

    fn remember(&mut self, identity: Identity) {
        self.owned.retain(|known| !retired(*known));
        if let Some(known) = self
            .owned
            .iter_mut()
            .find(|known| known.pid == identity.pid && known.birth == identity.birth)
        {
            *known = identity;
        } else {
            self.owned.push(identity);
        }
        self.last = Some(identity);
    }

    pub fn running(&mut self, socket: &Path) -> bool {
        if self.pid().is_some() {
            return true;
        }
        match self.observe(socket, Instant::now() + PROBE) {
            Ok(Some(identity)) => {
                self.remember(identity);
                self.pending = None;
                self.refusal = None;
                true
            }
            Ok(None) => self.waiting(),
            Err(e) if unavailable(&e) => self.waiting(),
            Err(e) => {
                self.refusal = Some(e.to_string());
                false
            }
        }
    }

    fn waiting(&mut self) -> bool {
        let until = *self.pending.get_or_insert_with(|| Instant::now() + HANG);
        match self.token.current() {
            Ok(true) if Instant::now() < until => true,
            Ok(true) => {
                self.refusal =
                    Some("one-minute hang detector: successor custody unavailable".into());
                false
            }
            Ok(false) => false,
            Err(e) => {
                self.refusal = Some(e.to_string());
                Instant::now() < until
            }
        }
    }

    fn observe(&mut self, socket: &Path, limit: Instant) -> io::Result<Option<Identity>> {
        let candidate_token = match Token::capture(socket) {
            Ok(token) => token,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let deadline = limit;
        let stream = match aterm_uds::connect::until(socket, deadline) {
            Ok(s) => s,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        stream.set_nonblocking(true)?;
        if aterm_uds::peer::peer_uid(&stream) != Some(aterm_uds::peer::our_uid()) {
            return Err(invalid("custody peer uid changed"));
        }
        let pid = i32::try_from(aterm_uds::fdpass::peer_pid(&stream)?)
            .map_err(|_| invalid("custody peer pid"))?;
        let before = match process::read_identity(pid) {
            Some(identity) => identity,
            None if matches!(
                process::process_presence(pid),
                ProcessPresence::Absent | ProcessPresence::Exited
            ) =>
            {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            None => return Err(invalid("custody peer identity unreadable")),
        };
        // Captured before sending an unpredictable challenge: a queued proof from a
        // dead socket peer cannot authenticate a different process that reused its PID.
        let challenge = aterm_uds::rand::hex_token::<32>()?;
        let mut hello = b"AUTH ".to_vec();
        hello.extend(candidate_token.value.expose());
        hello.extend(b"\n");
        hello.extend(aterm_types::lab_window_guard::CUSTODY_VERB.as_bytes());
        hello.extend(b" nonce=");
        hello.extend(challenge.as_bytes());
        hello.push(b'\n');
        let hello = aterm_uds::secret::Secret::from_vec(hello);
        aterm_ctl::resource_admission_transport::write_all(&stream, hello.expose(), deadline)?;
        receive_proof(&stream, &self.original, challenge.as_bytes(), deadline)?;
        if !matches!(candidate_token.current(), Ok(true))
            || process::read_identity(pid) != Some(before)
        {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        // Only the actual-description proof authorizes a credential generation.
        self.token = candidate_token;
        Ok(Some(before))
    }

    pub fn stop(&mut self, socket: &Path, deadline: Instant) -> Result<(), String> {
        let mut signalled = Vec::new();
        loop {
            // This identity came from an earlier actual-description proof (or the owned
            // original child), not the current pathname. Unavailable ingress cannot revoke
            // our ability to end that still-identical process or authorize another one.
            self.owned.retain(|known| !retired(*known));
            for identity in self.owned.iter().copied() {
                if !signalled.contains(&identity)
                    && process::read_identity(identity.pid) == Some(identity)
                    && process::process_presence(identity.pid) == ProcessPresence::Present
                {
                    if Instant::now() >= deadline {
                        return Err("successor stop deadline elapsed".into());
                    }
                    let grace = Duration::from_secs(5)
                        .min(deadline.saturating_duration_since(Instant::now()));
                    process::end_proven(identity, grace, &mut process::EndControlLive, |id| {
                        process::read_identity(id.pid) == Some(*id)
                    })
                    .map_err(|e| format!("proven successor stop refused: {e:?}"))?;
                    signalled.push(identity);
                }
            }
            match self.observe(socket, (Instant::now() + PROBE).min(deadline)) {
                Ok(Some(identity)) => self.remember(identity),
                Err(e) if !unavailable(&e) => return Err(e.to_string()),
                _ => {}
            }
            self.owned.retain(|known| !retired(*known));
            if self.owned.is_empty() {
                match self.token.current() {
                    Ok(false) => return Ok(()),
                    Err(e) => self.refusal = Some(e.to_string()),
                    Ok(true) => {}
                }
            }
            if Instant::now() >= deadline {
                return Err(self.refusal.clone().unwrap_or_else(||
                    "one-minute hang detector: successor native absence or endpoint retirement unproven".into()));
            }
            // A normal stop waits through its first signal; a second is reported by the caller.
            crate::stop::sleep(Duration::from_millis(20))
                .map_err(|e| format!("successor stop interrupted: {e:?}"))?;
        }
    }
}

fn retired(identity: Identity) -> bool {
    retired_by(
        identity,
        process::read_identity(identity.pid),
        process::process_presence(identity.pid),
    )
}

fn retired_by(identity: Identity, current: Option<Identity>, presence: ProcessPresence) -> bool {
    // An exec changes the image version, not the process lifetime. Its current
    // image still needs a fresh descriptor proof before it can be signalled.
    presence == ProcessPresence::Absent || current.is_some_and(|now| now.birth != identity.birth)
}

fn unavailable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::UnexpectedEof
    )
}

fn receive_proof(
    stream: &aterm_uds::CtlStream,
    original: &CloseOnlyLock,
    challenge: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    let mut expected = aterm_types::lab_window_guard::CUSTODY_PREFIX.to_vec();
    expected.extend_from_slice(challenge);
    expected.push(b'\n');
    let mut bytes = vec![0; expected.len()];
    let mut offset = 0;
    let mut descriptors = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let mut extra = [0];
        let target = if offset < bytes.len() {
            &mut bytes[offset..]
        } else {
            &mut extra[..]
        };
        match aterm_uds::fdpass::recv_with_fds(stream, target, 1) {
            Ok(received) => {
                descriptors.extend(received.fds);
                if descriptors.len() > 1 {
                    return Err(invalid("extra custody descriptors"));
                }
                if received.bytes == 0 {
                    if offset == 0 && descriptors.is_empty() {
                        return Err(io::ErrorKind::WouldBlock.into());
                    }
                    if offset != bytes.len() || descriptors.len() != 1 {
                        return Err(invalid("incomplete custody proof"));
                    }
                    return original.verify_copy(descriptors[0].as_fd());
                }
                if offset == bytes.len() {
                    return Err(invalid("trailing custody bytes"));
                }
                offset += received.bytes;
                if bytes[..offset] != expected[..offset] {
                    return Err(if descriptors.is_empty() {
                        io::ErrorKind::WouldBlock.into()
                    } else {
                        invalid("invalid custody response")
                    });
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                aterm_uds::ready::until(
                    stream.as_fd(),
                    aterm_uds::ready::Interest::Read,
                    deadline,
                )?;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
#[path = "instance_handoff_tests.rs"]
mod tests;
