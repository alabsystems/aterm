// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl --host <h> …`.

use std::env;
use std::fmt;
use std::io::{self, BufReader, Write};
use std::process::ExitCode;
use std::time::Duration;

use aterm_types::control_verbs::turn_option_words;

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
    /// Why the dial reached no Mac this machine may hold a tab on, with this machine's node.
    pub left: Option<Left>,
    /// The request was not sent because an aterm on its way took the connection and sent no fleet
    /// handshake within the deadline: the peer, or this machine's issuer, read for which Mac the
    /// word names (a refusal that says `try again once it answers`): exit 2, as for an instance
    /// that accepted and did not serve.
    pub unserved: bool,
}

/// Why a drive's dial reaches no Mac a hold was taken on ([`DriveRefused::left`]), with this
/// machine's node, which that Mac's `peers rm` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Left {
    /// No fleet's key here: a leave kept the node.
    Fleet(String),
    /// A fleet's key, and no Mac of its book the word names: a typo, or a Mac it records no
    /// more (a join of another fleet since, or a `peers rm`), which the book keeps no trace of.
    Book(String),
}

impl DriveRefused {
    /// A refusal a dial again would meet the same way.
    #[must_use]
    pub const fn once(why: String) -> Self {
        Self {
            why,
            again: false,
            left: None,
            unserved: false,
        }
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

/// A drive connection's deadline, its handshake's and its reply's, and whose it is, which a
/// reply cut at it names ([`closed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Within {
    /// The drive's own: [`DRIVE_WITHIN`], or an `await task` leg's wait and its slack.
    Drive(Duration),
    /// The caller's `--timeout`, shorter than [`DRIVE_WITHIN`].
    Timeout(Duration),
}

impl Within {
    const fn get(self) -> Duration {
        match self {
            Self::Drive(within) | Self::Timeout(within) => within,
        }
    }
}

/// A drive connection's deadline: [`DRIVE_WITHIN`], or a shorter `--timeout` (`total`, none for
/// `--timeout 0`).
fn drive_within(total: Option<Option<Duration>>) -> Within {
    match total {
        Some(Some(total)) if total < DRIVE_WITHIN => Within::Timeout(total),
        _ => Within::Drive(DRIVE_WITHIN),
    }
}

/// The `timeout=` a turn over a drive waits when it names none.
const TURN_WAIT_MS: u64 = 20_000;

/// The longest `timeout=` a turn over a drive takes: its connection is cut at
/// 30 s, the dial and the reply inside it.
const TURN_MAX_MS: u64 = aterm_types::control_verbs::DRIVE_TURN_MAX_MS;

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
        if let Some(at) = parts
            .iter()
            .position(|p| p == "--host" || p == "--fleet" || p.starts_with("--host="))
        {
            let named = match (parts[at].as_str(), parts.get(at + 1)) {
                ("--host", Some(other)) => format!("--host {other}"),
                (word, _) => word.to_owned(),
            };
            return Err(refused(format!(
                "--host {} and survey's own `{}` each name the machines a survey reads: give one \
                 (`aterm ctl survey --host <h>` reads one peer, `aterm ctl survey --fleet` every \
                 one)",
                aterm_codec::tty::one_line(host, 80),
                aterm_codec::tty::one_line(&named, 80)
            )));
        }
        let mut parts = parts.to_vec();
        parts.extend(["--host".to_owned(), host.to_owned()]);
        return survey::run_survey(&parts, None, None, total, local);
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
    let within = drive_within(total);
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
            drive(host, within.get()).map_err(io::Error::other)?,
            &format!("{head} len={}", task.len()),
            &task,
            within,
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
        let words = turn_wait(args, selector, host)?;
        parts.truncate(parts.len() - args.len());
        parts.push(words);
    }
    let mut request = request_line(&parts)?;
    validate_request_line_len(&request, Some(verb))?;
    request.push('\n');
    let link = drive(host, within.get()).map_err(|refused| match (&refused.left, args) {
        (Some(left), [sid, off, ..]) if verb == "hold" && off == "off" => {
            io::Error::other(left_holding(host, sid, left, &refused.why))
        }
        _ => io::Error::other(refused),
    })?;
    exchange(link, &request, verb, within)
}

/// Why a `hold <sid> off` cannot be sent to `host` by this machine, out of the fleet now or
/// holding a book that names no such Mac ([`Left`]): the dial's own refusal `why`, then, as a
/// condition this machine cannot check, what lifts a hold it took there ([`lifts_there`]) and
/// when the same `hold off` from here works.
fn left_holding(host: &str, sid: &str, left: &Left, why: &str) -> String {
    let (host, sid) = (
        aterm_codec::tty::one_line(host, 80),
        aterm_codec::tty::one_line(sid.trim_start_matches('@'), 40),
    );
    let (node, again) = match left {
        Left::Fleet(node) => (
            node,
            " is in that fleet again (a fleet it issued went with its leave)",
        ),
        Left::Book(node) => (node, "'s book records that Mac again"),
    };
    format!(
        "{why}; if this machine took {sid} on {host}, {}, and `aterm ctl --host {host} hold {sid} \
         off` from here works once this machine{again}",
        lifts_there(node)
    )
}

/// What lifts, on another Mac, a hold this machine (`node`) took there: each ends the grant the
/// hold stands on, or takes the tab.
#[must_use]
pub fn lifts_there(node: &str) -> String {
    format!(
        "that Mac lifts the hold when its person takes the tab (Take Over), runs `aterm fleet \
         peers rm {}` there, or lowers `aterm fleet allow` there (to content or facts)",
        node.get(..8).unwrap_or(node)
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

/// A turn's words after its verb (`args`) over a drive to `host`, read as the driven Mac reads
/// them (past the [`TURN_LEADING`] words, [`turn_option_words`]): its last `timeout=`, at most
/// [`TURN_MAX_MS`], or [`TURN_WAIT_MS`] added where it names none; a longer one is refused,
/// naming the waits that work for a worker's turn and for any other session's. A `timeout=` that
/// is no number goes as it is, for the driven Mac's own usage error.
fn turn_wait(args: &[String], session: Option<&str>, host: &str) -> io::Result<String> {
    let line = args.join(" ");
    let mut options = line.trim_start();
    loop {
        let (word, tail) = options
            .split_once(char::is_whitespace)
            .map_or((options, ""), |(word, tail)| (word, tail.trim_start()));
        if word == "--" {
            options = tail;
            break;
        }
        if !TURN_LEADING.iter().any(|lead| word.starts_with(lead)) {
            break;
        }
        options = tail;
    }
    let waits: Result<Vec<u64>, _> = turn_option_words(options)
        .0
        .into_iter()
        .filter(|(key, _)| *key == "timeout")
        .map(|(_, ms)| ms.parse::<u64>())
        .collect();
    match waits.as_deref() {
        Ok([]) => {
            let leading = line[..line.len() - options.len()].trim_end();
            let wait = format!("timeout={TURN_WAIT_MS}");
            let words = [leading, &wait, options];
            Ok(words
                .into_iter()
                .filter(|words| !words.is_empty())
                .collect::<Vec<_>>()
                .join(" "))
        }
        Ok([.., ms]) if *ms > TURN_MAX_MS => {
            let host = aterm_codec::tty::one_line(host, 80);
            let session = session.unwrap_or("@<sid>");
            Err(refused(format!(
                "a turn over the fleet waits at most timeout={TURN_MAX_MS} (its connection lasts \
                 30 s): give it a shorter timeout=, or say settle=submitted and then wait for a \
                 worker with `aterm ctl --host {host} await task {session}:<task_rev>` (the rev \
                 its turn prints), or read any other session with `aterm ctl --host {host} \
                 {session} status` (or `text`)"
            )))
        }
        _ => Ok(line),
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
/// read past), all `within` the connection's deadline.
fn spawn_exchange(
    link: DriveLink,
    head: &str,
    task: &[u8],
    within: Within,
) -> io::Result<ExitCode> {
    let DriveLink { mut stream, label } = link;
    let go = aterm_types::control_verbs::task_frame_go(task.len());
    let go = go.trim_end_matches('\n');
    let cut = |e| closed(&label, head, within, e);
    stream
        .write_all(format!("{head}\n").as_bytes())
        .and_then(|()| stream.flush())
        .map_err(cut)?;
    let mut reader = BufReader::new(stream);
    let mut answer = read_frame_answer(&mut reader).map_err(cut)?;
    if answer != go {
        return print_frame_answer(&answer);
    }
    let sent = reader.get_mut();
    sent.write_all(task)
        .and_then(|()| sent.flush())
        .map_err(cut)?;
    while answer == go {
        answer = read_frame_answer(&mut reader).map_err(cut)?;
    }
    print_frame_answer(&answer)
}

/// A connection the peer closed, cut or left unanswered past `within` before it answered
/// `request`, said as the peer's, or as the caller's `--timeout` where that cut it: what does
/// that, and how to learn whether what it sent landed.
pub(crate) fn closed(label: &str, request: &str, within: Within, e: io::Error) -> io::Error {
    use io::ErrorKind::{
        BrokenPipe, ConnectionAborted, ConnectionReset, TimedOut, UnexpectedEof, WouldBlock,
    };
    if super::is_stdout_closed(&e) {
        return e;
    }
    let mut words = request.split_whitespace();
    let (verb, session) = match (words.next(), words.next()) {
        (Some(sel), Some(verb)) if sel.starts_with('@') => (verb, Some(sel.to_owned())),
        (Some("hold"), sid) => ("hold", sid.map(|sid| format!("@{sid}"))),
        (Some(verb), _) => (verb, None),
        (None, _) => ("request", None),
    };
    let (cause, read_again) = match e.kind() {
        UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe => (
            format!(
                "{label} cut the drive connection before it answered (its person took back \
                 this Mac's grant or lowered its fleet level, its fleet key or listen place \
                 changed, or its aterm stopped or restarted)"
            ),
            "again says which",
        ),
        WouldBlock | TimedOut => (
            match within {
                Within::Drive(within) => format!(
                    "{label} did not answer within {} s (its aterm is stopped, frozen or too \
                     busy to answer, or the network between is down)",
                    within.as_secs()
                ),
                Within::Timeout(within) => {
                    let turn = if verb == "turn" {
                        format!(
                            "a `turn` there answers once it settles or its timeout= runs out, \
                             {TURN_WAIT_MS} ms where it names none; "
                        )
                    } else {
                        String::new()
                    };
                    format!(
                        "no answer came from {label} within this call's --timeout of {} s \
                         ({turn}a longer --timeout, or 0, waits up to the drive's own {} s)",
                        within.as_secs(),
                        DRIVE_WITHIN.as_secs()
                    )
                }
            },
            "again is safe: it changes nothing there",
        ),
        _ => return e,
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
            format!(": `aterm ctl --host {label} {at}{verb} …` {read_again}")
        }
    };
    io::Error::new(e.kind(), format!("{cause}{landed}"))
}

/// One request over `link` and its answer, framed as on one Mac: an `ERR`
/// on stderr (exit 1); a line-framed reply's rows on stdout, a turn's
/// verdict and a result's header on stderr; a status line on stdout; a
/// reported timeout exits 124, as does an answer not come `within` the deadline.
fn exchange(link: DriveLink, request: &str, verb: &str, within: Within) -> io::Result<ExitCode> {
    let DriveLink { mut stream, label } = link;
    let cut = |e| closed(&label, request, within, e);
    stream
        .write_all(request.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(cut)?;
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    if read_status_line(&mut reader, &mut status).map_err(cut)? == 0 {
        return Err(cut(io::Error::from(io::ErrorKind::UnexpectedEof)));
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
        print_payload(&mut reader, count).map_err(cut)?;
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
