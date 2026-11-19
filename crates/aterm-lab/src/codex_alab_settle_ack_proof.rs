// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private guardian for the signed original's lost-settlement-ACK proof.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::harness::native_act::{Image, Process};
use aterm_uds::{CtlListener, CtlStream, deadline, fdpass, process};
use serde::{Deserialize, Serialize};

const GUARDIAN: &str = "ATERM_LAB_CODEX_SETTLE_ACK_GUARDIAN";
const DRIVER_PID: &str = "ATERM_LAB_CODEX_SETTLE_ACK_DRIVER_PID";
const DRIVER_BIRTH: &str = "ATERM_LAB_CODEX_SETTLE_ACK_DRIVER_BIRTH";
const HANDSHAKE: Duration = Duration::from_secs(15);
const EXCHANGE: Duration = Duration::from_secs(3);
const FRAME_LIMIT: usize = 8192;

pub(super) struct Listener {
    listener: CtlListener,
    path: PathBuf,
    driver_pid: u32,
    driver_birth: u64,
}

impl Listener {
    pub(super) fn bind(world: &Path) -> io::Result<Self> {
        aterm_uds::private_file::check_private_dir(world)?;
        let driver_pid = std::process::id();
        let driver_birth = birth(driver_pid).ok_or_else(|| invalid("driver birth unavailable"))?;
        let path = world.join("sa.sock");
        let listener = CtlListener::bind(&path)?;
        let owner = Self {
            listener,
            path,
            driver_pid,
            driver_birth,
        };
        std::fs::set_permissions(&owner.path, std::fs::Permissions::from_mode(0o600))?;
        owner.listener.set_nonblocking(true)?;
        Ok(owner)
    }

    pub(super) fn append_environment(&self, env: &mut Vec<(String, String)>) -> io::Result<()> {
        let path = self
            .path
            .to_str()
            .ok_or_else(|| invalid("guardian path is not UTF-8"))?;
        env.extend([
            (GUARDIAN.into(), path.into()),
            (DRIVER_PID.into(), self.driver_pid.to_string()),
            (DRIVER_BIRTH.into(), self.driver_birth.to_string()),
        ]);
        Ok(())
    }

    pub(super) fn connect_host(self, pid: i32, executable: &Path) -> io::Result<Guardian> {
        let host_pid = u32::try_from(pid).map_err(io::Error::other)?;
        let host_birth = birth(host_pid).ok_or_else(|| invalid("host birth unavailable"))?;
        let host_image = Image::at(executable).ok_or_else(|| invalid("host image unavailable"))?;
        let until = Instant::now() + HANDSHAKE;
        loop {
            if birth(host_pid) != Some(host_birth) {
                return Err(invalid("original host changed before guardian connection"));
            }
            poll(self.listener.as_raw_fd(), libc::POLLIN, until)?;
            let stream = match aterm_uds::accept(
                &self.listener,
                aterm_uds::AcceptWait::Until(until),
                aterm_uds::AcceptedMode::Nonblocking,
            ) {
                Ok((stream, _)) => stream,
                Err(error) if retryable(&error) => continue,
                Err(error) => return Err(error),
            };
            if fdpass::peer_pid(&stream)? != host_pid
                || !matches_host(host_pid, host_birth, &host_image)
            {
                return Err(invalid("guardian peer is not the launched host"));
            }
            let nonce = aterm_uds::rand::hex_token::<8>()?;
            return Ok(Guardian {
                stream,
                host_pid,
                host_birth,
                host_image,
                nonce,
                counter: 0,
                expected: None,
                attempt: None,
                readback_connection: None,
                witnessed: None,
            });
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Debug, Serialize)]
pub(super) struct Expected {
    pub(super) sid: String,
    pub(super) pid: u32,
    pub(super) birth: u64,
    #[serde(rename = "sourceBuild")]
    pub(super) source_build: u64,
    pub(super) build: u64,
    pub(super) primary: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Action {
    ArmSettleAckLoss,
    Observe,
}

#[derive(Serialize)]
struct Command<'a> {
    schema: u8,
    request: &'a str,
    action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    expect: Option<&'a Expected>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Outcome {
    Ok,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    schema: u8,
    request: String,
    action: Action,
    #[serde(rename = "outcome")]
    _outcome: Outcome,
    #[serde(rename = "settleAckLoss", deserialize_with = "required_nullable")]
    settle_ack_loss: Option<Observation>,
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    attempt: Attempt,
    #[serde(deserialize_with = "required_nullable")]
    cleanup: Option<Cleanup>,
    #[serde(deserialize_with = "required_nullable")]
    readback: Option<Readback>,
    #[serde(rename = "settleRequests")]
    settle_requests: u64,
    commits: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    sid: String,
    plan: Plan,
    connection: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    operation: String,
    primary: String,
    build: u64,
    pid: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Cleanup {
    #[serde(deserialize_with = "required_nullable")]
    retained_claim: Option<Claim>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    revision: u64,
    expires_us: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Readback {
    operation: String,
    primary: String,
    build: u64,
    pid: u32,
    generation: u64,
    connection: u64,
    state: State,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum State {
    Waiting,
    Settled,
}

impl Observation {
    fn complete(&self, expected: &Expected) -> io::Result<bool> {
        let plan = &self.attempt.plan;
        if self.attempt.sid != expected.sid
            || plan.pid != expected.pid
            || plan.build != expected.build
            || plan.primary != expected.primary
            || !aterm_codec::hex::is_lower(&plan.operation, 32)
            || self.attempt.connection == 0
            || self.settle_requests > 1
            || self.commits > 1
            || self.commits > self.settle_requests
        {
            return Err(invalid(
                "settlement attempt differs from the proven original",
            ));
        }
        if let Some(cleanup) = &self.cleanup
            && !cleanup
                .retained_claim
                .as_ref()
                .is_some_and(|claim| claim.revision > 0 && claim.expires_us > 0)
        {
            return Err(invalid("original EOF did not retain its native claim"));
        }
        let Some(readback) = &self.readback else {
            return Ok(false);
        };
        if readback.pid != plan.pid
            || readback.build != plan.build
            || readback.primary != plan.primary
            || readback.operation != plan.operation
            || readback.generation == 0
            || readback.connection == 0
            || readback.connection == self.attempt.connection
        {
            return Err(invalid("native settlement readback changed identity"));
        }
        if readback.state == State::Waiting {
            return Ok(false);
        }
        if self.cleanup.is_none() || self.settle_requests != 1 || self.commits != 1 {
            return Err(invalid(
                "settled readback lacks one original commit and EOF",
            ));
        }
        Ok(true)
    }
}

pub(super) struct Guardian {
    stream: CtlStream,
    host_pid: u32,
    host_birth: u64,
    host_image: Image,
    nonce: String,
    counter: u64,
    expected: Option<Expected>,
    attempt: Option<Attempt>,
    readback_connection: Option<u64>,
    witnessed: Option<Observation>,
}

#[derive(Serialize)]
pub(super) struct Evidence<'a> {
    schema: u8,
    #[serde(rename = "hostPid")]
    host_pid: u32,
    #[serde(rename = "hostBirth")]
    host_birth: u64,
    original: &'a Expected,
    #[serde(rename = "settleAckLoss")]
    settle_ack_loss: Observation,
}

impl Guardian {
    pub(super) fn arm(&mut self, expected: Expected) -> io::Result<()> {
        if self.expected.is_some() || birth(expected.pid) != Some(expected.birth) {
            return Err(invalid("signed original cannot arm the settlement fault"));
        }
        let reply = self.exchange(
            Action::ArmSettleAckLoss,
            Some(&expected),
            Instant::now() + EXCHANGE,
        )?;
        if reply.settle_ack_loss.is_some() {
            return Err(invalid(
                "armed guardian unexpectedly supplied prior fault evidence",
            ));
        }
        self.expected = Some(expected);
        Ok(())
    }

    pub(super) fn await_recovery(&mut self) -> io::Result<()> {
        let until = Instant::now() + HANDSHAKE;
        loop {
            if Instant::now() >= until {
                return Err(invalid("native settlement readback did not complete"));
            }
            if let Some(observed) = self.observe(until)? {
                self.witnessed = Some(observed);
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Re-observe after the real continuation to exclude a later duplicate settle.
    pub(super) fn final_observation(&mut self) -> io::Result<Evidence<'_>> {
        let final_state = self.observe(Instant::now() + EXCHANGE)?;
        let Some(observed) = final_state.filter(|current| self.witnessed.as_ref() == Some(current))
        else {
            return Err(invalid(
                "settlement witness changed after the original continuation",
            ));
        };
        let original = self
            .expected
            .as_ref()
            .ok_or_else(|| invalid("settlement fault was not armed"))?;
        Ok(Evidence {
            schema: 1,
            host_pid: self.host_pid,
            host_birth: self.host_birth,
            original,
            settle_ack_loss: observed,
        })
    }

    fn observe(&mut self, until: Instant) -> io::Result<Option<Observation>> {
        let reply = self.exchange(Action::Observe, None, until)?;
        let expected = self
            .expected
            .as_ref()
            .ok_or_else(|| invalid("settlement fault was not armed"))?;
        if birth(expected.pid) != Some(expected.birth) {
            return Err(invalid("original Codex process changed before observation"));
        }
        let Some(observed) = reply.settle_ack_loss else {
            if self.attempt.is_some() {
                return Err(invalid("original settlement witness disappeared"));
            }
            return Ok(None);
        };
        let complete = observed.complete(expected)?;
        if self
            .attempt
            .as_ref()
            .is_some_and(|old| old != &observed.attempt)
        {
            return Err(invalid("original settlement attempt changed"));
        }
        if let Some(readback) = &observed.readback {
            if self
                .readback_connection
                .is_some_and(|old| old != readback.connection)
            {
                return Err(invalid("replacement settlement connection changed"));
            }
            self.readback_connection = Some(readback.connection);
        } else if self.readback_connection.is_some() {
            return Err(invalid("replacement settlement readback disappeared"));
        }
        self.attempt = Some(observed.attempt.clone());
        Ok(complete.then_some(observed))
    }

    fn exchange(
        &mut self,
        action: Action,
        expect: Option<&Expected>,
        deadline: Instant,
    ) -> io::Result<Reply> {
        self.check_host()?;
        self.counter = self
            .counter
            .checked_add(1)
            .ok_or_else(|| invalid("guardian request counter exhausted"))?;
        let request = format!("{}{:016x}", self.nonce, self.counter);
        let mut frame = aterm_json::to_vec(&Command {
            schema: 1,
            request: &request,
            action,
            expect,
        })
        .map_err(io::Error::other)?;
        frame.push(b'\n');
        let until = deadline.min(Instant::now() + EXCHANGE);
        write_frame(&mut self.stream, &frame, until)?;
        let frame = read_frame(&mut self.stream, until)?;
        self.check_host()?;
        let reply: Reply = aterm_json::from_slice(&frame).map_err(io::Error::other)?;
        if reply.schema != 1 || reply.request != request || reply.action != action {
            return Err(invalid(
                "guardian reply does not match the original request",
            ));
        }
        Ok(reply)
    }

    fn check_host(&self) -> io::Result<()> {
        if fdpass::peer_pid(&self.stream)? == self.host_pid
            && matches_host(self.host_pid, self.host_birth, &self.host_image)
        {
            Ok(())
        } else {
            Err(invalid("settlement guardian host changed"))
        }
    }
}

fn matches_host(pid: u32, birth: u64, image: &Image) -> bool {
    Process::observe(pid).is_some_and(|process| process.birth == birth && process.image == *image)
}

fn birth(pid: u32) -> Option<u64> {
    process::read_birth(i32::try_from(pid).ok()?).map(|birth| birth.0)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn retryable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

fn poll(fd: i32, events: i16, until: Instant) -> io::Result<()> {
    let mut descriptor = [libc::pollfd {
        fd,
        events,
        revents: 0,
    }];
    loop {
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "settlement guardian deadline",
            ));
        }
        match crate::sys::poll(&mut descriptor, deadline::poll_timeout_ms(remaining)) {
            Ok(0) => {}
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

fn write_frame(stream: &mut CtlStream, mut bytes: &[u8], until: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        poll(stream.as_raw_fd(), libc::POLLOUT, until)?;
        match stream.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => bytes = &bytes[written..],
            Err(error) if retryable(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_frame(stream: &mut CtlStream, until: Instant) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        poll(stream.as_raw_fd(), libc::POLLIN, until)?;
        let n = match stream.read(&mut buffer) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => n,
            Err(error) if retryable(&error) => continue,
            Err(error) => return Err(error),
        };
        if bytes.len() + n > FRAME_LIMIT {
            return Err(invalid("settlement guardian reply exceeds its frame limit"));
        }
        if let Some(end) = buffer[..n].iter().position(|byte| *byte == b'\n') {
            if end + 1 != n {
                return Err(invalid("unexpected bytes after settlement guardian reply"));
            }
            bytes.extend_from_slice(&buffer[..end]);
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
}

#[cfg(test)]
#[path = "codex_alab_settle_ack_proof_tests.rs"]
mod tests;
