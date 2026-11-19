// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A whole-client capture whose connection owns pixel custody through a native photograph.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use super::{CtlStream, TargetOrigin};

const HANG: Duration = Duration::from_secs(60);
const TICK: Duration = Duration::from_millis(500);
const PNG_MAX: usize = aterm_uds::deadline::MAX_COMPLETE_LISTING_BYTES;

fn invalid(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

fn remaining(deadline: Option<Instant>) -> io::Result<Option<Duration>> {
    remaining_at(deadline, Instant::now())
}

fn remaining_at(deadline: Option<Instant>, now: Instant) -> io::Result<Option<Duration>> {
    match deadline {
        None => Ok(None),
        Some(until) => until
            .checked_duration_since(now)
            .filter(|left| !left.is_zero())
            .map(Some)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "presentation capture's operation deadline expired",
                )
            }),
    }
}

/// Keep one absolute budget on an unbuffered control stream and its no-op flush.
/// The original native error wins; a successful call cannot outlive the budget.
struct DeadlineWriter<'a, W> {
    writer: W,
    deadline: Option<Instant>,
    now: &'a mut dyn FnMut() -> Instant,
    arm: &'a mut dyn FnMut(Option<Duration>) -> io::Result<()>,
}

impl<W: Write> DeadlineWriter<'_, W> {
    fn arm(&mut self) -> io::Result<()> {
        let left = remaining_at(self.deadline, (self.now)())?;
        (self.arm)(left)?;
        remaining_at(self.deadline, (self.now)())?;
        Ok(())
    }
}

impl<W: Write> Write for DeadlineWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.arm()?;
        let count = self.writer.write(bytes)?;
        remaining_at(self.deadline, (self.now)())?;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        // CtlStream is unbuffered. Its peer may already have answered and closed
        // after the final write, so this no-op must not rearm a detached socket.
        remaining_at(self.deadline, (self.now)())?;
        self.writer.flush()?;
        remaining_at(self.deadline, (self.now)())?;
        Ok(())
    }
}

fn write_unbuffered_until<W: Write>(
    writer: W,
    bytes: &[u8],
    deadline: Option<Instant>,
    now: &mut dyn FnMut() -> Instant,
    arm: &mut dyn FnMut(Option<Duration>) -> io::Result<()>,
) -> io::Result<()> {
    let mut writer = DeadlineWriter {
        writer,
        deadline,
        now,
        arm,
    };
    writer.write_all(bytes)?;
    writer.flush()
}

fn write_on(stream: &CtlStream, bytes: &[u8], deadline: Option<Instant>) -> io::Result<()> {
    write_unbuffered_until(stream, bytes, deadline, &mut Instant::now, &mut |left| {
        stream.set_write_timeout(left)
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Plan {
    argv: Vec<String>,
    producers: Vec<u32>,
}

impl Plan {
    pub(super) fn take(parts: &mut Vec<String>) -> io::Result<Option<Self>> {
        let (_, request) = super::split_selector(parts);
        if request.first().map(String::as_str) != Some("image")
            || !request
                .iter()
                .take_while(|word| word.as_str() != "--")
                .any(|word| word == "--presentation-lease")
        {
            return Ok(None);
        }
        if parts.first().map(String::as_str) != Some("image") {
            return Err(invalid("presentation capture requires image"));
        }
        let split = parts
            .iter()
            .position(|word| word == "--")
            .ok_or_else(|| invalid("presentation capture requires -- and a photograph command"))?;
        let mut name = None;
        let mut producers = Vec::new();
        let mut leased = false;
        let mut words = parts[1..split].iter();
        while let Some(word) = words.next() {
            match word.as_str() {
                "--presentation-lease" if !leased => leased = true,
                "--producer-pid" => {
                    let pid = words
                        .next()
                        .and_then(|word| word.parse::<u32>().ok())
                        .filter(|pid| *pid > 0 && i32::try_from(*pid).is_ok())
                        .ok_or_else(|| invalid("presentation capture has no valid producer pid"))?;
                    if !producers.contains(&pid) {
                        producers.push(pid);
                    }
                }
                word if !word.starts_with('-')
                    && !word.contains('/')
                    && word.bytes().all(|byte| byte.is_ascii_graphic())
                    && name.is_none() =>
                {
                    name = Some(word.to_string());
                }
                _ => {
                    return Err(invalid(
                        "presentation capture requires one filename and a literal photograph command",
                    ));
                }
            }
        }
        let name = name
            .filter(|name| !matches!(name.as_str(), "." | ".." | "auto"))
            .ok_or_else(|| invalid("presentation capture requires a filename"))?;
        let argv = parts[split + 1..].to_vec();
        if argv.first().is_none_or(String::is_empty) {
            return Err(invalid("presentation capture has no photograph command"));
        }
        *parts = vec!["image".into(), "--presentation-lease".into(), name];
        Ok(Some(Self { argv, producers }))
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Receipt {
    identity: BTreeMap<String, u64>,
    path: Option<PathBuf>,
}

impl Receipt {
    fn parse(line: &str, prefix: &str, path: bool) -> io::Result<Self> {
        if line.starts_with("ERR ") {
            return Err(invalid(line));
        }
        let tail = line
            .strip_prefix(prefix)
            .ok_or_else(|| invalid("presentation capture returned no matching receipt"))?;
        let mut identity = BTreeMap::new();
        let mut destination = None;
        for word in tail.split_whitespace() {
            let (key, value) = word
                .split_once('=')
                .ok_or_else(|| invalid("malformed presentation receipt"))?;
            if key == "path" && path && destination.is_none() {
                let decoded = aterm_codec::pct::decode(value);
                if aterm_codec::pct::encode(&decoded) != value {
                    return Err(invalid(
                        "presentation image path is not canonically escaped",
                    ));
                }
                let target = PathBuf::from(decoded);
                if !target.is_absolute() {
                    return Err(invalid("presentation image path is not absolute"));
                }
                destination = Some(target);
            } else if matches!(key, "window" | "serial" | "surface" | "width" | "height") {
                let number = value
                    .parse::<u64>()
                    .ok()
                    .filter(|number| {
                        (key == "window" || *number > 0) && number.to_string() == value
                    })
                    .ok_or_else(|| invalid("invalid presentation identity"))?;
                if matches!(key, "width" | "height") && u32::try_from(number).is_err() {
                    return Err(invalid("presentation dimensions exceed the native size"));
                }
                if identity.insert(key.to_string(), number).is_some() {
                    return Err(invalid("repeated presentation identity"));
                }
            } else {
                return Err(invalid("unknown presentation receipt field"));
            }
        }
        if identity.len() != 5 || path != destination.is_some() {
            return Err(invalid("incomplete presentation receipt"));
        }
        Ok(Self {
            identity,
            path: destination,
        })
    }

    fn fields(&self) -> String {
        ["window", "serial", "surface", "width", "height"]
            .into_iter()
            .map(|key| format!("{key}={}", self.identity[key]))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

struct Progress {
    roots: Vec<(u32, aterm_uds::deadline::ProcessProgressProbe, Option<u64>)>,
    last: Instant,
    read: Option<Instant>,
}

impl Progress {
    fn new(roots: &[u32]) -> Self {
        Self {
            roots: roots
                .iter()
                .map(|pid| (*pid, Default::default(), None))
                .collect(),
            last: Instant::now(),
            read: None,
        }
    }

    fn check(&mut self) -> io::Result<()> {
        let now = Instant::now();
        if self.read.is_some_and(|at| now.duration_since(at) < TICK) {
            return Ok(());
        }
        self.read = Some(now);
        let mut changed = false;
        for (pid, probe, seen) in &mut self.roots {
            use aterm_uds::process::{ProcessPresence, process_presence};
            if matches!(
                process_presence(i32::try_from(*pid).unwrap_or(0)),
                ProcessPresence::Absent | ProcessPresence::Exited
            ) {
                return Err(invalid("presentation capture's native producer ended"));
            }
            let work = probe.read(*pid);
            changed |= work.is_none() || work != *seen;
            *seen = work;
        }
        if changed {
            self.last = now;
        }
        if now.duration_since(self.last) >= HANG {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "presentation capture's native producers made no progress for a minute",
            ));
        }
        Ok(())
    }
}

/// One exclusively owned capture stream. Readiness enforces each polling interval
/// without setting socket options after Darwin has detached an already answered peer.
struct CaptureRead<'a> {
    stream: &'a CtlStream,
    deadline: Option<Instant>,
}

impl Read for CaptureRead<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        remaining(self.deadline)?;
        let poll_until = super::instant_after(Instant::now(), TICK);
        let until = self
            .deadline
            .map_or(poll_until, |until| until.min(poll_until));
        let count = aterm_uds::hangup::read_until(self.stream, bytes, until)?;
        remaining(self.deadline)?;
        Ok(count)
    }
}

fn line(
    reader: &mut impl BufRead,
    progress: &mut Progress,
    deadline: Option<Instant>,
) -> io::Result<String> {
    let mut result = Vec::new();
    loop {
        remaining(deadline)?;
        match reader.fill_buf() {
            Ok([]) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(bytes) => {
                // A complete authenticated frame remains evidence after its peer
                // exits. Process liveness bounds a wait, never bytes already here.
                progress.last = Instant::now();
                let n = bytes
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |at| at + 1);
                result.extend_from_slice(&bytes[..n]);
                reader.consume(n);
                if result.len() > 64 * 1024 {
                    return Err(invalid("presentation receipt is too large"));
                }
                if result.last() == Some(&b'\n') {
                    remaining(deadline)?;
                    return String::from_utf8(result)
                        .map(|line| line.trim_end_matches(['\r', '\n']).to_string())
                        .map_err(|_| invalid("presentation receipt is not UTF-8"));
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                remaining(deadline)?;
                progress.check()?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn photograph(plan: &Plan, deadline: Option<Instant>) -> io::Result<()> {
    remaining(deadline)?;
    use aterm_uds::spawnfd::Tether as _;
    let mut command = aterm_uds::spawnfd::command(&plan.argv[0]);
    command
        .tether()
        .args(&plan.argv[1..])
        .stdin(aterm_uds::spawnfd::Stdio::null());
    let progress = std::cell::RefCell::new(Progress::new(&plan.producers));
    let child = std::cell::Cell::new(None);
    let mut child_probe = aterm_uds::deadline::ProcessProgressProbe::default();
    let mut child_seen = None;
    let mut failed = None;
    let mut diagnostics = Vec::new();
    let status = aterm_uds::deadline::stream_until_stopped_observing_exit(
        &mut command,
        deadline,
        |pid| child.set(Some(pid)),
        |_, bytes| {
            progress.borrow_mut().last = Instant::now();
            let room = aterm_uds::deadline::OUTPUT_KEEP.saturating_sub(diagnostics.len());
            diagnostics.extend_from_slice(&bytes[..bytes.len().min(room)]);
            Ok(())
        },
        || {
            if let Err(error) = remaining(deadline) {
                failed = Some(error);
                return true;
            }
            let mut progress = progress.borrow_mut();
            if let Some(pid) = child.get() {
                if progress.read.is_none_or(|at| at.elapsed() >= TICK) {
                    let work = child_probe.read(pid);
                    if work.is_none() || work != child_seen {
                        progress.last = Instant::now();
                    }
                    child_seen = work;
                }
            }
            match progress.check() {
                Ok(()) => false,
                Err(error) => {
                    failed = Some(error);
                    true
                }
            }
        },
        |_| {
            child.set(None);
            let mut progress = progress.borrow_mut();
            progress.roots.clear();
            progress.last = Instant::now();
        },
    )?;
    if let Some(error) = failed {
        return Err(error);
    }
    remaining(deadline)?;
    match status {
        Some(status) if status.success() => Ok(()),
        _ => Err(invalid(format!(
            "native photograph did not complete: {}",
            String::from_utf8_lossy(&diagnostics)
        ))),
    }
}

fn capture_on(
    stream: CtlStream,
    plan: &Plan,
    deadline: Option<Instant>,
) -> io::Result<(Receipt, Receipt, String)> {
    remaining(deadline)?;
    let server = super::redial::server_pid(&stream)
        .ok_or_else(|| invalid("presentation capture has no native server identity"))?;
    let mut progress = Progress::new(&[server]);
    let mut reader = std::io::BufReader::new(CaptureRead {
        stream: &stream,
        deadline,
    });
    let receipt = Receipt::parse(
        &line(&mut reader, &mut progress, deadline)?,
        "OK presentation-image ",
        true,
    )?;
    let challenge = line(&mut reader, &mut progress, deadline)?;
    let nonce = challenge
        .strip_prefix(aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX)
        .filter(|nonce| aterm_types::control_verbs::valid_artifact_ack_nonce(nonce))
        .ok_or_else(|| invalid("presentation capture has no acknowledgement challenge"))?;
    let file = receipt.path.as_ref().expect("parsed image has a path");
    let parent = file
        .parent()
        .ok_or_else(|| invalid("presentation image has no parent"))?;
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("presentation image has no filename"))?;
    remaining(deadline)?;
    let png = aterm_uds::private_file::read_private(parent, name, PNG_MAX)
        .map_err(|error| invalid(format!("presentation image could not be retained: {error}")))?;
    remaining(deadline)?;
    let digest = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&png));
    photograph(plan, deadline)?;
    let acknowledgement = format!(
        "{}{nonce}\n",
        aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX
    );
    write_on(&stream, acknowledgement.as_bytes(), deadline)?;
    let released = Receipt::parse(
        &line(&mut reader, &mut Progress::new(&[server]), deadline)?,
        "OK presentation-released ",
        false,
    )?;
    if released.identity != receipt.identity {
        return Err(invalid(
            "presentation identity changed during the photograph",
        ));
    }
    remaining(deadline)?;
    Ok((receipt, released, digest))
}

pub(super) fn run(
    path: &str,
    parts: &[String],
    plan: &Plan,
    budget: Option<Duration>,
) -> io::Result<ExitCode> {
    let request = format!("{}\n", super::request_line(parts)?);
    super::validate_request_line_len(request.trim_end_matches('\n'), Some("image"))?;
    let answered = super::converse_served(path, TargetOrigin::Pinned, budget, |stream| {
        // As ordinary exchange_on, the operation budget begins once this exact
        // connection has been accepted. Progress never replenishes that budget.
        let deadline = budget.map(|wait| super::instant_after(Instant::now(), wait));
        if let Some(code) =
            super::served_write_result(&stream, write_on(&stream, request.as_bytes(), deadline))?
        {
            return Ok(code);
        }
        let (receipt, released, digest) = capture_on(stream, plan, deadline)?;
        let file = receipt.path.as_ref().expect("parsed image has a path");
        super::print_stdout_line(&format!(
            "OK presentation-image {} png_sha256={digest} path={}",
            receipt.fields(),
            aterm_codec::pct::encode(&file.to_string_lossy())
        ))?;
        super::print_stdout_line(&format!("OK presentation-released {}", released.fields()))?;
        Ok(ExitCode::SUCCESS)
    })?;
    Ok(answered.unwrap_or_else(std::convert::identity))
}

#[cfg(test)]
#[path = "presentation_capture_tests.rs"]
mod tests;
