// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl --host <h> …`.

use std::env;
use std::fmt;
use std::io::{self, BufReader, Write};
use std::process::ExitCode;
use std::time::Duration;

use super::{
    EXIT_TIMEOUT, LocalVerbs, SELF_SID_ENV, await_task, bytes_payload, malformed_header_error,
    print_frame_answer, print_payload, print_stdout_line, read_frame_answer, read_status_line,
    read_task_payload, reply_is_timeout, request_line, spawn_wire_line, split_selector,
    stderr_line, stream_count, streams_payload, survey, usage_error, validate_request_line_len,
    validate_request_parts, worker_starter,
};

/// A drive connection's stream, as the fleet client hands it over.
pub trait DriveStream: io::Read + Write {}

impl<T: io::Read + Write> DriveStream for T {}

/// ONE DRIVE CONNECTION to another Mac's aterm ([`FleetDriveFn`]).
pub struct DriveLink {
    /// The proved stream.
    pub stream: Box<dyn DriveStream>,
    /// How the peer is named in what this client says: its host as this machine knows it.
    pub label: String,
}

/// THE FLEET CLIENT'S DRIVE DIAL the `aterm` binary links.
pub type FleetDriveFn = fn(&str, Duration) -> Result<DriveLink, DriveRefused>;

/// Why a drive dial reached no peer to drive ([`FleetDriveFn`]).
#[derive(Debug)]
pub struct DriveRefused {
    /// What to say of it.
    pub why: String,
    /// Whether the same dial a moment later may be served.
    pub again: bool,
}

impl DriveRefused {
    /// A refusal a dial again would meet the same way.
    #[must_use]
    pub const fn once(why: String) -> Self {
        Self { why, again: false }
    }
}

impl fmt::Display for DriveRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.why)
    }
}

impl std::error::Error for DriveRefused {}

impl From<String> for DriveRefused {
    fn from(why: String) -> Self {
        Self::once(why)
    }
}

impl From<&str> for DriveRefused {
    fn from(why: &str) -> Self {
        Self::once(why.to_owned())
    }
}

/// The deadline of one connection that waits for nothing (a read, a key, a
/// spawn, a hold): under the peer's 30 s cut.
const DRIVE_WITHIN: Duration = Duration::from_secs(28);

/// The `timeout=` a turn over a drive waits when it names none.
const TURN_WAIT_MS: u64 = 20_000;

/// The longest `timeout=` a turn over a drive takes: its connection is cut at
/// 30 s, the dial and the reply inside it.
const TURN_MAX_MS: u64 = aterm_types::control_verbs::DRIVE_TURN_MAX_MS;

/// The options a `turn` reads before its text (`help turn`).
const TURN_OPTIONS: &[&str] = &[
    "idle",
    "timeout",
    "submit",
    "settle",
    "submit_window",
    "presses",
    "submit_verify",
    "trim",
    "typed",
    "cadence",
    "cap",
    "yield",
    "if-gen",
    "if-rows",
    "cause",
];

fn refused(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why.into())
}

/// `aterm ctl --host <host> <parts…>` ([the module](self)).
pub(crate) fn run(
    host: &str,
    parts: &[String],
    total: Option<Option<Duration>>,
    local: &LocalVerbs,
) -> io::Result<ExitCode> {
    validate_request_parts(parts)?;
    if survey::is_survey(parts) {
        let mut parts = parts.to_vec();
        if !parts
            .iter()
            .any(|p| p == "--host" || p == "--fleet" || p.starts_with("--host="))
        {
            parts.extend(["--host".to_owned(), host.to_owned()]);
        }
        return survey::run_survey(&parts, None, None, local);
    }
    let Some(drive) = local.fleet_drive else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "--host drives another Mac's aterm over the fleet, which this aterm-ctl does not \
             link: run `aterm ctl --host …`",
        ));
    };
    refuse_local_selector(parts)?;
    let (selector, rest) = split_selector(parts);
    let verb = rest.first().map(String::as_str).ok_or_else(usage_error)?;
    let args = &rest[1..];
    if await_task::is_await_task(parts) {
        return await_task::run_driven(parts, total, host, drive);
    }
    if verb == "spawn" {
        if selector.is_some() {
            return Err(refused(
                "a worker spawned over the fleet opens where that Mac's aterm puts it: name no \
                 @<sel>",
            ));
        }
        let head = spawn_head(args, worker_starter(env::var(SELF_SID_ENV).ok()).as_deref())?;
        let task = read_task_payload()?;
        return spawn_exchange(
            drive(host, DRIVE_WITHIN).map_err(io::Error::other)?,
            &format!("{head} len={}", task.len()),
            &task,
        );
    }
    if matches!(verb, "feed-bin" | "paste")
        || (verb == "send" && matches!(args, [one] if one == "--stdin" || one == "-"))
    {
        return Err(refused(
            "a drive types inline text alone (`@<w> send <text>`, `@<w> turn <text>`): no \
             stdin frame but a worker's task crosses the fleet",
        ));
    }
    let mut parts = parts.to_vec();
    if verb == "turn" {
        turn_wait(&mut parts)?;
    }
    let mut request = request_line(&parts)?;
    validate_request_line_len(&request, Some(verb))?;
    request.push('\n');
    exchange(
        drive(host, DRIVE_WITHIN).map_err(io::Error::other)?,
        &request,
        verb,
    )
}

/// Refuse a leading selector that names a session of THIS Mac, or none by
/// its sid: over the fleet a session is `@s-<hex>`, as `survey --host`
/// shows it.
fn refuse_local_selector(parts: &[String]) -> io::Result<()> {
    let Some(selector) = parts.first().filter(|p| p.starts_with('@')) else {
        return Ok(());
    };
    let sid = &selector[1..];
    if is_sid(sid) {
        return Ok(());
    }
    Err(refused(format!(
        "over the fleet a session is named by its sid (@s-…, as `aterm ctl survey --host <h>` \
         shows it): `{}` names a session of this Mac, or several",
        aterm_codec::tty::one_line(selector, 40)
    )))
}

/// Whether `word` is a global session id (`s-<hex>`).
fn is_sid(word: &str) -> bool {
    word.strip_prefix("s-")
        .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The options the server's dispatch takes off a `turn` before the turn's
/// own (`aterm-gui`'s `take_leading_options`): the exactly-once `id=`, the
/// `if-supervisor=` fence and a leading `cause=`.
const TURN_LEADING: &[&str] = &["id=", "if-supervisor=", "cause="];

/// A turn's wait over a drive: its `timeout=`, at most [`TURN_MAX_MS`], or
/// [`TURN_WAIT_MS`] added where it names none.
fn turn_wait(parts: &mut Vec<String>) -> io::Result<()> {
    let at = parts
        .iter()
        .position(|p| p == "turn")
        .ok_or_else(usage_error)?;
    let mut options = at + 1;
    while let Some(word) = parts.get(options) {
        if word == "--" {
            options += 1;
            break;
        }
        if !TURN_LEADING.iter().any(|lead| word.starts_with(lead)) {
            break;
        }
        options += 1;
    }
    let named = parts[options..]
        .iter()
        .map_while(|word| {
            let (key, value) = word.split_once('=')?;
            TURN_OPTIONS.contains(&key).then_some((key, value))
        })
        .find(|(key, _)| *key == "timeout")
        .map(|(_, value)| value.to_owned());
    match named {
        None => {
            parts.insert(options, format!("timeout={TURN_WAIT_MS}"));
            Ok(())
        }
        Some(ms) if ms.parse::<u64>().is_ok_and(|ms| ms <= TURN_MAX_MS) => Ok(()),
        Some(_) => Err(refused(format!(
            "a turn over the fleet waits at most timeout={TURN_MAX_MS} (its connection lasts \
             30 s): say settle=submitted and wait with `await task @<w>:<task_rev>`"
        ))),
    }
}

/// The request line of a worker's spawn over a drive, without its `len=`:
/// `spawn <args>`, where `args` end with `-` (the task is on stdin), name no
/// `len=`, a `cwd=` only absolute — a folder of THIS Mac names nothing there,
/// so none is added: that Mac's aterm starts it in its own default — and
/// gain `from=<starter>` ([`worker_starter`]) when they name none.
fn spawn_head(args: &[String], starter: Option<&str>) -> io::Result<String> {
    let usage = |why: &str| {
        refused(format!(
            "spawn agent= over the fleet: {why} (aterm ctl --host <h> spawn agent=<word> \
             [model=<m>] [cwd=<absolute folder there>] - < task)"
        ))
    };
    if !args.iter().any(|a| a.starts_with("agent=")) {
        return Err(usage("a drive spawns a worker alone"));
    }
    let Some(("-", args)) = args.split_last().map(|(last, args)| (last.as_str(), args)) else {
        return Err(usage("the task is read from stdin: end the request with -"));
    };
    if args.iter().any(|a| a.starts_with("len=")) {
        return Err(usage("len= is the client's to write"));
    }
    for arg in args {
        let Some(dir) = arg.strip_prefix("cwd=") else {
            continue;
        };
        let dir = dir
            .strip_prefix('"')
            .and_then(|d| d.strip_suffix('"'))
            .unwrap_or(dir);
        if !dir.starts_with('/') {
            return Err(usage(
                "cwd= names a folder of that Mac, by its absolute path",
            ));
        }
    }
    let mut parts = vec!["spawn".to_owned()];
    parts.extend(args.iter().cloned());
    if let Some(sid) = starter
        && !args.iter().any(|a| a.starts_with("from="))
    {
        parts.push(format!("from={sid}"));
    }
    let head = spawn_wire_line(&parts)?;
    validate_request_line_len(&head, Some("spawn"))?;
    Ok(head)
}

/// A worker's spawn over `link`: the line `head`; then, once it is answered
/// `GO <n>`, the `task`, and the answer printed as on one Mac (a second `GO`
/// read past).
fn spawn_exchange(link: DriveLink, head: &str, task: &[u8]) -> io::Result<ExitCode> {
    let DriveLink { mut stream, label } = link;
    let go = aterm_types::control_verbs::task_frame_go(task.len());
    let go = go.trim_end_matches('\n');
    stream.write_all(head.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut answer = read_frame_answer(&mut reader).map_err(|e| closed(&label, head, e))?;
    if answer != go {
        return print_frame_answer(&answer);
    }
    reader.get_mut().write_all(task)?;
    reader.get_mut().flush()?;
    while answer == go {
        answer = read_frame_answer(&mut reader).map_err(|e| closed(&label, head, e))?;
    }
    print_frame_answer(&answer)
}

/// A connection the peer closed or cut before it answered `request`, said as the peer's: what
/// cuts one, and how to learn whether what it sent landed.
fn closed(label: &str, request: &str, e: io::Error) -> io::Error {
    use io::ErrorKind::{ConnectionAborted, ConnectionReset, UnexpectedEof};
    if !matches!(
        e.kind(),
        UnexpectedEof | ConnectionReset | ConnectionAborted
    ) {
        return e;
    }
    let mut words = request.split_whitespace();
    let (verb, session) = match (words.next(), words.next()) {
        (Some(sel), Some(verb)) if sel.starts_with('@') => (verb, Some(sel.to_owned())),
        (Some("hold"), sid) => ("hold", sid.map(|sid| format!("@{sid}"))),
        (Some(verb), _) => (verb, None),
        (None, _) => ("request", None),
    };
    let landed = match (verb, session) {
        ("turn" | "key" | "send" | "hold" | "close", Some(session)) => format!(
            ", so what this `{verb}` sent may have landed: `aterm ctl --host {label} {session} \
             status` shows whether it did, or why it is refused"
        ),
        ("spawn", _) => format!(
            ", so the worker may have started: `aterm ctl --host {label} survey` lists it, or \
             says why it is refused"
        ),
        (_, session) => {
            let at = session.map_or_else(String::new, |session| format!("{session} "));
            format!(": `aterm ctl --host {label} {at}{verb} …` again says which")
        }
    };
    io::Error::new(
        e.kind(),
        format!(
            "{label} cut the drive connection before it answered (its person took back this \
             Mac's grant, or its aterm stopped or restarted){landed}"
        ),
    )
}

/// One request over `link` and its answer, framed as on one Mac: an `ERR`
/// on stderr (exit 1); a line-framed reply's rows on stdout, a turn's
/// verdict and a result's header on stderr; a status line on stdout; a
/// reported timeout exits 124.
fn exchange(link: DriveLink, request: &str, verb: &str) -> io::Result<ExitCode> {
    let DriveLink { mut stream, label } = link;
    stream.write_all(request.as_bytes())?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    let read =
        read_status_line(&mut reader, &mut status).map_err(|e| closed(&label, request, e))?;
    if read == 0 {
        return Err(closed(
            &label,
            request,
            io::Error::from(io::ErrorKind::UnexpectedEof),
        ));
    }
    let status = status.trim_end_matches(['\r', '\n']);
    let (head, tail) = status.split_once(' ').unwrap_or((status, ""));
    if head != "OK" {
        stderr_line(status)?;
        return Ok(ExitCode::FAILURE);
    }
    if streams_payload(verb, request) {
        let count = stream_count(tail).ok_or_else(|| malformed_header_error(status))?;
        if tail.split_whitespace().nth(1) == Some("turn") {
            let verdict: Vec<&str> = tail.split_whitespace().skip(1).collect();
            stderr_line(&verdict.join(" "))?;
        } else if verb == "result" {
            stderr_line(status)?;
        } else if count == 0 {
            stderr_line(&format!("{status} ({verb}: no results)"))?;
        }
        print_payload(&mut reader, count).map_err(|e| closed(&label, request, e))?;
    } else if bytes_payload(verb, request) {
        return Err(io::Error::other(format!(
            "{label} answered `{verb}` with a body no drive carries"
        )));
    } else {
        print_stdout_line(status)?;
    }
    Ok(if reply_is_timeout(status) {
        ExitCode::from(EXIT_TIMEOUT)
    } else {
        ExitCode::SUCCESS
    })
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
