// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Authenticated observer for the private headless signed-Codex proof.
//! It can withhold one settlement reply. It cannot admit source execution.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use aterm_agent::harness::native_act::Process;
use aterm_uds::{CtlStream, connect, fdpass, hangup, private_file};
use serde::{Deserialize, Serialize};

use super::settle_ack_fault::{self, Attempt, Expectation, Fault, Observation, Witness};
use crate::control::control_lanes::{LaneReader, Outbox};
use crate::control::{
    CONNECTION_CLAIMED, Request, Selector, ServeDisposition, Store, serving_connection,
    serving_peer_pid,
};

const GUARDIAN: &str = "ATERM_LAB_CODEX_SETTLE_ACK_GUARDIAN";
const DRIVER_PID: &str = "ATERM_LAB_CODEX_SETTLE_ACK_DRIVER_PID";
const DRIVER_BIRTH: &str = "ATERM_LAB_CODEX_SETTLE_ACK_DRIVER_BIRTH";
const CONNECT: Duration = Duration::from_secs(10);
const FRAME_TIME: Duration = Duration::from_secs(3);
const SDK_EOF: Duration = Duration::from_secs(15);
const FRAME_LIMIT: usize = 8192;

struct Offer {
    socket: PathBuf,
    driver: Process,
}

impl Offer {
    fn from_environment(socket: PathBuf, control_dir: &Path) -> io::Result<Self> {
        validate_world(
            &socket,
            control_dir,
            env_path("HOME")?.as_path(),
            env_path("XDG_CONFIG_HOME")?.as_path(),
            env_path("XDG_RUNTIME_DIR")?.as_path(),
            env_path(aterm_types::world::MARKER)?.as_path(),
        )?;
        let pid = env_positive::<u32>(DRIVER_PID)?;
        let birth = env_positive::<u64>(DRIVER_BIRTH)?;
        let driver = Process::observe(pid)
            .filter(|process| process.birth == birth && pid > 1)
            .ok_or_else(|| invalid("original settlement proof driver unavailable"))?;
        if !parent_matches(&driver) {
            return Err(invalid("settlement proof driver is not the kernel parent"));
        }
        Ok(Self { socket, driver })
    }
}

fn env_path(name: &str) -> io::Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("private settlement world is incomplete"))
}

fn env_positive<T: std::str::FromStr>(name: &str) -> io::Result<T> {
    let value =
        std::env::var(name).map_err(|_| invalid("settlement driver identity unavailable"))?;
    if value
        .as_bytes()
        .first()
        .is_none_or(|byte| !matches!(*byte, b'1'..=b'9'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid("invalid settlement driver identity"));
    }
    value
        .parse()
        .map_err(|_| invalid("invalid settlement driver identity"))
}

fn validate_world(
    socket: &Path,
    control_dir: &Path,
    home: &Path,
    config: &Path,
    runtime: &Path,
    marker: &Path,
) -> io::Result<()> {
    let root = socket
        .parent()
        .ok_or_else(|| invalid("settlement world root unavailable"))?;
    if !root.is_absolute()
        || std::fs::canonicalize(root)? != root
        || socket.file_name() != Some(OsStr::new("sa.sock"))
        || home != root.join("home")
        || config != root.join("cfg")
        || runtime != root.join("run")
        || control_dir != runtime
        || aterm_types::world::state_home(home).as_deref() != Some(marker)
    {
        return Err(invalid(
            "settlement guardian is outside the private instance world",
        ));
    }
    for dir in [root, home, config, runtime] {
        private_file::check_private_dir(dir)?;
    }
    let world = private_file::read_private(root, "lab-world", 64).map_err(io::Error::other)?;
    if world.as_slice() != b"aterm-lab-world/1\n" {
        return Err(invalid(
            "settlement guardian lacks the private lab world marker",
        ));
    }
    let endpoint = std::fs::symlink_metadata(socket)?;
    if !endpoint.file_type().is_socket()
        || endpoint.mode() & 0o777 != 0o600
        || endpoint.uid() != std::fs::metadata(root)?.uid()
    {
        return Err(invalid(
            "settlement guardian is not an owner-private socket",
        ));
    }
    Ok(())
}

fn parent_matches(driver: &Process) -> bool {
    Process::observe(std::process::id()).is_some_and(|host| host.parent == driver.pid)
        && Process::observe(driver.pid).as_ref() == Some(driver)
}

fn authenticate(stream: &CtlStream, driver: &Process) -> io::Result<()> {
    if fdpass::peer_pid(stream)? == driver.pid && parent_matches(driver) {
        Ok(())
    } else {
        Err(invalid("settlement guardian peer changed"))
    }
}

pub(in crate::control) struct Guardian {
    fault: OnceLock<Arc<Fault>>,
    teardown: Mutex<Option<(u64, Attempt)>>,
    stopping: AtomicBool,
}

impl Guardian {
    pub(in crate::control) fn start(
        headless: bool,
        control_dir: &Path,
        store: &Store,
    ) -> Option<Arc<Self>> {
        let socket = PathBuf::from(std::env::var_os(GUARDIAN)?);
        if !headless {
            return None;
        }
        let offer = match Offer::from_environment(socket, control_dir) {
            Ok(offer) => offer,
            Err(error) => {
                aterm_log::warn!("private settlement guardian refused: {error}");
                return None;
            }
        };
        let guardian = Arc::new(Self {
            fault: OnceLock::new(),
            teardown: Mutex::new(None),
            stopping: AtomicBool::new(false),
        });
        let worker = Arc::clone(&guardian);
        let store = store.clone();
        if crate::qos::spawn_lane_thread(
            "signed-settlement-guardian",
            crate::qos::Role::Responsive,
            move || {
                let result = worker.serve(offer, &store);
                worker.stopping.store(true, Ordering::Release);
                if let Err(error) = result {
                    aterm_log::warn!("private settlement guardian stopped: {error}");
                }
            },
        )
        .is_err()
        {
            return None;
        }
        Some(guardian)
    }

    fn serve(&self, offer: Offer, store: &Store) -> io::Result<()> {
        let mut stream = connect::until(&offer.socket, Instant::now() + CONNECT)?;
        stream.set_write_timeout(Some(FRAME_TIME))?;
        let mut sequence = Sequence::default();
        loop {
            authenticate(&stream, &offer.driver)?;
            let mut first = [0];
            match hangup::read_until(&stream, &mut first, Instant::now() + FRAME_TIME) {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::TimedOut => continue,
                Err(error) => return Err(error),
            }
            let frame = read_frame(&stream, first[0], Instant::now() + FRAME_TIME)?;
            authenticate(&stream, &offer.driver)?;
            let command: Command = aterm_json::from_slice(&frame)
                .map_err(|_| invalid("invalid settlement guardian command"))?;
            sequence.accept(&command)?;
            let observation = match command.action {
                Action::Arm => {
                    let expected = command
                        .expect
                        .ok_or_else(|| invalid("settlement arm missing identity"))?;
                    self.arm(expected, store)?;
                    None
                }
                Action::Observe if command.expect.is_none() => self.snapshot()?,
                Action::Observe => return Err(invalid("settlement observation supplied arm data")),
            };
            let mut reply = aterm_json::to_vec(&Reply {
                schema: 1,
                request: &command.request,
                action: command.action,
                outcome: "ok",
                observation,
            })
            .map_err(|_| invalid("cannot encode settlement guardian observation"))?;
            if reply.len() >= FRAME_LIMIT {
                return Err(invalid("settlement guardian observation too large"));
            }
            reply.push(b'\n');
            stream.write_all(&reply)?;
        }
    }

    fn arm(&self, expected: Expectation, store: &Store) -> io::Result<()> {
        let (sid, pid, birth) = expected.original_peer();
        let selector = Selector::parse(sid);
        if store
            .read()
            .unwrap_or_else(|failure| failure.into_inner())
            .by_selector(&selector)
            .is_none_or(|session| session.ctx.self_id.as_str() != sid)
            || Process::observe(pid).is_none_or(|process| process.birth != birth)
        {
            return Err(invalid("settlement arm does not name a live local source"));
        }
        let fault = Arc::new(Fault::new(sid.to_owned()));
        fault.arm(expected).map_err(invalid)?;
        self.fault
            .set(fault)
            .map_err(|_| invalid("settlement guardian already armed"))
    }

    fn snapshot(&self) -> io::Result<Option<Observation>> {
        // A positive native readback can race the final witness publication.
        // The actual release below and this observation share the same guard,
        // so an observer cannot see completion before the cleanup receipt.
        let _teardown = self
            .teardown
            .lock()
            .unwrap_or_else(|failure| failure.into_inner());
        self.fault
            .get()
            .map(|fault| fault.observation())
            .ok_or_else(|| invalid("settlement observation precedes arm"))
    }

    pub(in crate::control) fn dispatch(
        &self,
        request: Request,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
        out: &mut Outbox,
        serve: impl FnOnce(Request, &mut LaneReader<'_>, &mut Outbox) -> Option<ServeDisposition>,
    ) -> Option<ServeDisposition> {
        let Some(fault) = self
            .fault
            .get()
            .filter(|_| !self.stopping.load(Ordering::Acquire))
        else {
            return serve(request, reader, out);
        };
        let connection = serving_connection();
        let attempt = serving_peer_pid()
            .zip(connection)
            .and_then(|(peer, connection)| fault.select(request.line(), peer, connection));
        let dispatch = || settle_ack_fault::observe_dispatch(fault, || serve(request, reader, out));
        let Some(attempt) = attempt else {
            return dispatch();
        };
        let (disposition, witness) = settle_ack_fault::witness(&attempt, dispatch);
        let Some(Witness::Commit(generation)) = witness else {
            return disposition;
        };
        if disposition.is_some()
            || !CONNECTION_CLAIMED.with(std::cell::Cell::get)
            || !out
                .fixture_unsent_status()
                .is_some_and(|status| fault.hold(&attempt, status, generation))
        {
            return disposition;
        }
        drop(std::mem::take(out));
        if settle_ack_fault::await_peer_eof(
            stream,
            !reader.buffer().is_empty(),
            &self.stopping,
            Instant::now() + SDK_EOF,
        )
        .is_ok()
            && let Some(connection) = connection
        {
            *self
                .teardown
                .lock()
                .unwrap_or_else(|failure| failure.into_inner()) = Some((connection, attempt));
        }
        Some(ServeDisposition::Close)
    }

    pub(in crate::control) fn release(&self, connection: u64, release: impl FnOnce()) {
        let held = self
            .teardown
            .lock()
            .unwrap_or_else(|failure| failure.into_inner())
            .take_if(|(id, _)| *id == connection);
        let Some((_, attempt)) = held else {
            return release();
        };
        let (_, witness) = settle_ack_fault::witness(&attempt, release);
        let claim = match witness {
            Some(Witness::Eof(claim)) => Some(claim),
            _ => None,
        };
        if let Some(fault) = self.fault.get() {
            fault.record_cleanup(&attempt, claim);
        }
    }
}

fn invalid(description: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, description)
}

#[derive(Clone, Copy, Deserialize, Serialize)]
enum Action {
    #[serde(rename = "arm-settle-ack-loss")]
    Arm,
    #[serde(rename = "observe")]
    Observe,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    schema: u8,
    request: String,
    action: Action,
    #[serde(default, deserialize_with = "expectation")]
    expect: Option<Expectation>,
}

fn expectation<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Expectation>, D::Error> {
    Expectation::deserialize(d).map(Some)
}

#[derive(Serialize)]
struct Reply<'a> {
    schema: u8,
    request: &'a str,
    action: Action,
    outcome: &'static str,
    #[serde(rename = "settleAckLoss")]
    observation: Option<Observation>,
}

#[derive(Default)]
struct Sequence {
    prefix: Option<String>,
    last: u64,
}

impl Sequence {
    fn accept(&mut self, command: &Command) -> io::Result<()> {
        if command.schema != 1 || !aterm_codec::hex::is_lower(&command.request, 32) {
            return Err(invalid("invalid settlement guardian sequence"));
        }
        let (prefix, suffix) = command.request.split_at(16);
        let expected = self
            .last
            .checked_add(1)
            .ok_or_else(|| invalid("guardian sequence exhausted"))?;
        if self.prefix.as_deref().is_some_and(|old| old != prefix)
            || u64::from_str_radix(suffix, 16) != Ok(expected)
        {
            return Err(invalid("settlement guardian sequence changed"));
        }
        self.prefix.get_or_insert_with(|| prefix.to_owned());
        self.last = expected;
        Ok(())
    }
}

fn read_frame(stream: &CtlStream, first: u8, until: Instant) -> io::Result<Vec<u8>> {
    if first == b'\n' {
        return Err(invalid("empty settlement guardian frame"));
    }
    let mut frame = vec![first];
    let mut buffer = [0; 512];
    loop {
        let n = hangup::read_until(stream, &mut buffer, until)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if frame.len() + n > FRAME_LIMIT {
            return Err(invalid("settlement guardian command too large"));
        }
        if let Some(end) = buffer[..n].iter().position(|byte| *byte == b'\n') {
            if end + 1 != n {
                return Err(invalid("settlement guardian command was pipelined"));
            }
            frame.extend_from_slice(&buffer[..end]);
            return Ok(frame);
        }
        frame.extend_from_slice(&buffer[..n]);
    }
}

#[cfg(test)]
#[path = "signed_source_settle_ack_guardian_tests.rs"]
mod tests;
