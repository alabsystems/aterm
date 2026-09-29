// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cast drift` on the client: the ONE spelling, whoever computes it.
//!
//! `aterm ctl [@<sid>] cast drift [max_runs=<k>] [rows=<n>] [seed=…]` asks the
//! server first. A server that knows the verb answers `OK <n> verdict=…` and
//! the rows, printed as they come. A server that PREDATES it answers the bare
//! `cast` (its dispatch reads only `cast frames` as a sub-form), so the reply is
//! `OK <nbytes>` and an asciicast body — told apart by the header having no
//! `verdict=`. Then, when the binary running this client links the engine (the
//! one `aterm` binary hands the analyzer in through [`LocalVerbs`]), the client
//! computes the SAME report itself from what that server can serve: `status`
//! (for the cut), `text`, `cursor`, `modes` and `cast`, read-only verbs all, so
//! the audit is safe to aim at a tab a person is using. Its header says
//! `computed=client`.
//!
//! THROUGH A RELAY (2026-09-28): `aterm ctl dial <name> [@<sid>] cast drift …`
//! asks the REMOTE the same way, every request sent as `dial <name> …` and
//! every reply framed as the remote's answer to the verb after the head
//! ([`Route`]). A remote that predates the verb gets the same client-side
//! report, its material read through the relay too; the refusal stays only
//! for a relay that cannot carry those reads (the error names why).
//!
//! The OFFLINE form reads saved files and needs no socket:
//! `aterm ctl cast drift --file <cast> [--screen <text>] [--until <t>] [k=v…]`
//! (with no screen it compares the faithful replay with the stable one:
//! `fidelity=-`).
//!
//! WHY INJECTED, NOT LINKED: this crate is the dependency-free client (it links
//! `aterm-types` and `aterm-uds` only), and the analysis folds recordings
//! through the terminal engine. The one binary already links the engine, so it
//! passes a plain `fn` in ([`main_entry_with`](crate::main_entry_with)): no
//! global registration, no new dependency here, and the standalone dev
//! `aterm-ctl` says by name that it cannot compute the report.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use super::{
    StdoutSink, TargetOrigin, byte_count, converse, converse_served, drain_refusal_tail,
    malformed_header_error, read_bounded_line, read_status_line, read_token_for, send_request,
    send_served, stderr_line, stdout_handle, stream_count,
};

/// Verbs this client answers IN-PROCESS when the binary embedding it links the
/// engine. The one `aterm` binary passes them to
/// [`main_entry_with`](crate::main_entry_with); the standalone `aterm-ctl`
/// passes [`LocalVerbs::NONE`].
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalVerbs {
    /// The `cast drift` analyzer (`aterm_control::cast_drift` behind an adapter).
    pub cast_drift: Option<CastDriftFn>,
}

impl LocalVerbs {
    /// No local verbs: every request goes to a server.
    pub const NONE: Self = Self { cast_drift: None };
}

/// Computes a `cast drift` report from fetched (or saved) material: the whole
/// reply text — `OK <n> verdict=… computed=client` then `n` lines — or the
/// refusal to print (a usage error, an unreadable recording).
pub type CastDriftFn = fn(&CastDriftJob<'_>) -> Result<String, String>;

/// How the screen and the recording were read against each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriftCut {
    /// The screen's `seq=`/`hash=` stamp did not move across the reads.
    Quiescent,
    /// It moved on every attempt: a difference is `unknown`.
    Racy,
    /// Saved files: nothing live to cut.
    File,
}

/// Which aterm made the recording, for the one engine behavior the replays
/// must match: the RESIZE UNDO (2026-09-28), with which a flap with nothing
/// output between its halves moves nothing. An aterm without it shifted its
/// screen on every such flap, and a replay with the undo cannot reproduce that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriftPeer {
    /// An aterm older than `cast drift` (the live fallback, direct or through
    /// `dial`), and so older than the undo, which landed after the verb: the
    /// replays drop it (`undo=off`).
    PreUndo,
    /// Not known (saved files): the replays keep the undo, and drop it only
    /// when that alone reproduces the given screen.
    Unknown,
}

/// One `cast drift` to compute client-side.
#[derive(Clone, Copy, Debug)]
pub struct CastDriftJob<'a> {
    /// The asciicast v2 text (`cast`'s body, or a saved file).
    pub cast: &'a str,
    /// The screen's rows (`text`'s body, or a saved dump); `None` offline
    /// without `--screen`.
    pub screen: Option<&'a [String]>,
    /// The live cursor (row, col), when read.
    pub cursor: Option<(u16, u16)>,
    /// Whether the live screen is on the alternate buffer, when read.
    pub alt: Option<bool>,
    /// The request tail (`max_runs=… rows=… seed=…`), for the analyzer to parse.
    pub args: &'a str,
    /// How the reads were cut.
    pub cut: DriftCut,
    /// Offline: cut the recording at this many seconds.
    pub until: Option<f64>,
    /// Which aterm made the recording.
    pub peer: DriftPeer,
}

/// How long the client lets the server's recorder catch up before it reads the
/// screen — the server verb's own settle (`CAST_DRIFT_SETTLE`, 60 ms).
const SETTLE: Duration = Duration::from_millis(60);

/// Consistent-cut attempts before the client proceeds with `cut=racy`.
const CUT_ATTEMPTS: usize = 3;

/// Where `cast drift`'s requests go: a `dial <name>` relay head when the
/// session is on a remote, then the session's `@<selector>`, if any.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Route<'a> {
    /// The saved connection the local server relays to (`dial <name>`).
    pub(crate) dial: Option<&'a str>,
    /// The session, `@<sid>` / `@.`.
    pub(crate) selector: Option<&'a str>,
}

impl Route<'_> {
    /// The request as the REMOTE (or the one server) reads it:
    /// `[@<selector> ]<verb>` — what its reply is framed by.
    fn framed(&self, verb: &str) -> String {
        let mut r = String::new();
        if let Some(sel) = self.selector {
            r.push_str(sel);
            r.push(' ');
        }
        r.push_str(verb);
        r
    }

    /// The line sent: [`framed`](Self::framed) behind the relay head.
    fn line(&self, verb: &str) -> String {
        let framed = self.framed(verb);
        match self.dial {
            Some(name) => {
                let mut r = String::from("dial ");
                r.push_str(name);
                r.push(' ');
                r.push_str(&framed);
                r
            }
            None => framed,
        }
    }
}

/// `dial <name> [@<selector>] <verb…>` split into the connection, the
/// selector and the request from its verb on; `None` for anything else (a
/// bare `dial <name>` included).
pub(crate) fn split_dial(parts: &[String]) -> Option<(&str, Option<&str>, &[String])> {
    if parts.first().map(String::as_str) != Some("dial") {
        return None;
    }
    let name = parts.get(1)?.as_str();
    let rest = parts.get(2..)?;
    match rest.first() {
        Some(sel) if sel.starts_with('@') => Some((name, Some(sel.as_str()), &rest[1..])),
        Some(_) => Some((name, None, rest)),
        None => None,
    }
}

/// Whether `rest` (the request with any `@<selector>` split off) is `cast drift`.
pub(crate) fn is_cast_drift(rest: &[String]) -> bool {
    rest.first().map(String::as_str) == Some("cast")
        && rest.get(1).map(String::as_str) == Some("drift")
}

/// Whether the reply to `request` (by its `verb`) is a `cast drift` report,
/// whose HEADER carries the verdict and so leads the rows on stdout.
pub(crate) fn is_drift_request(verb: &str, request: &str) -> bool {
    let tail = request
        .strip_prefix('@')
        .and_then(|r| r.split_once(' ').map(|x| x.1))
        .unwrap_or(request);
    verb == "cast" && tail.split_whitespace().nth(1) == Some("drift")
}

fn usage(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

/// The offline form's parts.
#[derive(Debug, PartialEq)]
struct Offline {
    file: String,
    screen: Option<String>,
    until: Option<f64>,
    args: Vec<String>,
}

/// Split `cast drift`'s tail into the offline flags and the `k=v` arguments.
/// `Ok(None)`: no flag at all (the live form). A flag without `--file`, an
/// unknown flag, or a flag missing its value is a usage error.
fn parse_offline(tail: &[String]) -> io::Result<Option<Offline>> {
    let mut file = None;
    let mut screen = None;
    let mut until = None;
    let mut args = Vec::new();
    let mut flagged = false;
    let mut it = tail.iter();
    while let Some(tok) = it.next() {
        let Some(flag) = tok.strip_prefix("--") else {
            args.push(tok.clone());
            continue;
        };
        flagged = true;
        let (name, inline) = match flag.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (flag, None),
        };
        let value = match inline {
            Some(v) => v,
            None => it
                .next()
                .cloned()
                .ok_or_else(|| usage("cast drift: --file, --screen and --until take a value"))?,
        };
        match name {
            "file" => file = Some(value),
            "screen" => screen = Some(value),
            "until" => {
                until = Some(value.parse::<f64>().map_err(|_| {
                    usage("cast drift: --until takes seconds on the cast's timeline")
                })?);
            }
            _ => {
                return Err(usage(
                    "cast drift: the offline flags are --file <cast> [--screen <text>] [--until <t>]",
                ));
            }
        }
    }
    if !flagged {
        return Ok(None);
    }
    let file = file.ok_or_else(|| {
        usage("cast drift: --screen and --until read saved files; give the recording with --file <cast>")
    })?;
    Ok(Some(Offline {
        file,
        screen,
        until,
        args,
    }))
}

fn no_analyzer_error(what: &str) -> io::Error {
    let mut msg = String::from(what);
    msg.push_str(
        "; this client does not link the engine the replay needs — run it as `aterm ctl …` \
         (the one aterm binary computes it client-side)",
    );
    io::Error::other(msg)
}

/// The rows of a `text` dump: one per line, a final newline not making an
/// extra row, `\r` endings tolerated.
fn screen_rows(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect()
}

/// The offline form, answered with no socket: `Ok(None)` when `parts` is not
/// it (the caller goes on to dial). A `@<selector>` with the offline flags is a
/// usage error: the files ARE the session.
pub(crate) fn offline_entry(parts: &[String], local: &LocalVerbs) -> io::Result<Option<ExitCode>> {
    let (selector, rest) = match parts.first() {
        Some(s) if s.starts_with('@') => (Some(s), &parts[1..]),
        _ => (None, parts),
    };
    if !is_cast_drift(rest) {
        return Ok(None);
    }
    let Some(off) = parse_offline(&rest[2..])? else {
        return Ok(None);
    };
    if selector.is_some() {
        return Err(usage(
            "cast drift --file reads saved files; it takes no @<selector>",
        ));
    }
    let Some(analyze) = local.cast_drift else {
        return Err(no_analyzer_error(
            "offline `cast drift --file` replays the recording",
        ));
    };
    let cast = std::fs::read_to_string(&off.file).map_err(|e| {
        let mut msg = String::from("cast drift: read ");
        msg.push_str(&off.file);
        msg.push_str(": ");
        msg.push_str(&e.to_string());
        io::Error::new(e.kind(), msg)
    })?;
    let screen = match &off.screen {
        Some(path) => Some(screen_rows(&std::fs::read_to_string(path).map_err(
            |e| {
                let mut msg = String::from("cast drift: read ");
                msg.push_str(path);
                msg.push_str(": ");
                msg.push_str(&e.to_string());
                io::Error::new(e.kind(), msg)
            },
        )?)),
        None => None,
    };
    let args = off.args.join(" ");
    let job = CastDriftJob {
        cast: &cast,
        screen: screen.as_deref(),
        cursor: None,
        alt: None,
        args: &args,
        cut: DriftCut::File,
        until: off.until,
        peer: DriftPeer::Unknown,
    };
    print_report(analyze(&job)).map(Some)
}

/// Print a computed report (or its refusal).
fn print_report(report: Result<String, String>) -> io::Result<ExitCode> {
    match report {
        Ok(text) => {
            let stdout = stdout_handle();
            let mut out = StdoutSink(stdout.lock());
            out.write_all(text.as_bytes())?;
            out.flush()?;
            Ok(ExitCode::SUCCESS)
        }
        Err(msg) => {
            stderr_line(&msg)?;
            Ok(ExitCode::FAILURE)
        }
    }
}

/// A request a server answered `ERR …`: the request and its status line,
/// kept apart so a caller can tell WHO refused (the local relay's `ERR dial …`,
/// or the remote's own answer carried through it).
#[derive(Debug)]
struct Refused {
    request: String,
    status: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.request, self.status)
    }
}

impl std::error::Error for Refused {}

/// Whether `e`, from a read sent through `dial`, is the RELAY failing — the
/// transport (a connection that closed, reset, refused or timed out) or the
/// local server's own `ERR dial …` — rather than the remote answering. A
/// remote's refusal (`ERR no such session`, the tab closed between the probe
/// and the reads), a reply of the wrong shape or a cast that is not UTF-8
/// came through a relay that worked, and is reported as itself.
fn relay_failed(e: &io::Error) -> bool {
    if let Some(refused) = e
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<Refused>())
    {
        return refused.status.starts_with("ERR dial");
    }
    matches!(
        e.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
    )
}

/// One reply, read whole.
enum Reply {
    /// A status line.
    Status(String),
    /// `OK <n>` and its `n` rows.
    Lines(Vec<String>),
    /// `OK <nbytes>` and its body.
    Bytes(Vec<u8>),
}

/// One request on its own connection, its reply read whole by the verb's
/// framing (the remote's answer to `verb`, for a relayed route). An `ERR`
/// reply is an error naming the request ([`Refused`]).
fn fetch(
    path: &str,
    origin: TargetOrigin,
    route: Route<'_>,
    verb: &str,
    deadline: Option<Duration>,
) -> io::Result<Reply> {
    let request = route.line(verb);
    let framed = route.framed(verb);
    let request = request.as_str();
    converse(path, origin, deadline, |stream| {
        stream.set_read_timeout(deadline)?;
        stream.set_write_timeout(deadline)?;
        let mut line = String::from(request);
        line.push('\n');
        send_request(&stream, read_token_for(path).as_deref(), &line)?;
        let mut reader = BufReader::new(&stream);
        let mut status = String::new();
        if read_status_line(&mut reader, &mut status)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed the connection without responding",
            ));
        }
        let status = status.trim_end_matches(['\r', '\n']).to_string();
        let tail = match status.strip_prefix("OK") {
            Some(t) if t.is_empty() || t.starts_with(' ') => t.trim_start(),
            _ => {
                return Err(io::Error::other(Refused {
                    request: request.to_string(),
                    status,
                }));
            }
        };
        let without_selector = framed
            .strip_prefix('@')
            .and_then(|r| r.split_once(' ').map(|x| x.1))
            .unwrap_or(&framed);
        let verb = without_selector.split_whitespace().next().unwrap_or("");
        use aterm_types::control_verbs::{Framing, framing_of};
        match framing_of(verb, &framed) {
            Framing::Lines => {
                let n = stream_count(tail).ok_or_else(|| malformed_header_error(&status))?;
                Ok(Reply::Lines(read_lines(&mut reader, n)?))
            }
            Framing::Bytes => {
                let n = byte_count(tail).ok_or_else(|| malformed_header_error(&status))?;
                let mut body = Vec::with_capacity(n.min(8 << 20));
                (&mut reader).take(n as u64).read_to_end(&mut body)?;
                if body.len() < n {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "server hung up mid-body",
                    ));
                }
                Ok(Reply::Bytes(body))
            }
            _ => Ok(Reply::Status(status)),
        }
    })
}

/// `n` reply lines, each bounded, with the line ending stripped.
pub(crate) fn read_lines<R: BufRead>(reader: &mut R, n: usize) -> io::Result<Vec<String>> {
    let mut rows = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let mut line = String::new();
        if read_bounded_line(reader, &mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server hung up before the complete line-framed response",
            ));
        }
        let line = line.strip_suffix('\n').unwrap_or(&line);
        rows.push(line.strip_suffix('\r').unwrap_or(line).to_string());
    }
    Ok(rows)
}

/// The screen stamp a `status` line carries: its `seq=` and `hash=` values
/// (parse by key). `None` when it carries neither — a cut against it cannot be
/// shown quiescent.
fn stamp(status: &str) -> Option<(String, String)> {
    let key = |k: &str| {
        status
            .split_whitespace()
            .find_map(|t| t.strip_prefix(k))
            .map(str::to_string)
    };
    match (key("seq="), key("hash=")) {
        (None, None) => None,
        (seq, hash) => Some((seq.unwrap_or_default(), hash.unwrap_or_default())),
    }
}

/// `cursor`'s `OK <row> <col> <visible> <style>`.
fn parse_cursor(status: &str) -> Option<(u16, u16)> {
    let mut it = status.split_whitespace().skip(1);
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// `modes`' `alt_screen=<bool>` row.
fn parse_alt(rows: &[String]) -> Option<bool> {
    rows.iter()
        .find_map(|r| r.strip_prefix("alt_screen="))
        .and_then(|v| v.parse().ok())
}

fn want_status(reply: Reply) -> io::Result<String> {
    match reply {
        Reply::Status(s) => Ok(s),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected a status line",
        )),
    }
}

fn want_lines(reply: Reply) -> io::Result<Vec<String>> {
    match reply {
        Reply::Lines(rows) => Ok(rows),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected line-framed rows",
        )),
    }
}

/// What the client-side audit reads from a server, cut as the server verb cuts.
struct Material {
    cast: String,
    rows: Vec<String>,
    cursor: Option<(u16, u16)>,
    alt: Option<bool>,
    cut: DriftCut,
}

/// Read the session the way the server verb does, over the wire: stamp, settle,
/// read the screen (`text`, `cursor`, `modes`) and the recording (`cast`),
/// stamp again. An unmoved stamp is a quiescent cut; up to [`CUT_ATTEMPTS`]
/// tries, then racy.
fn read_material(
    path: &str,
    origin: TargetOrigin,
    route: Route<'_>,
    deadline: Option<Duration>,
    settle: &dyn Fn(Duration),
) -> io::Result<Material> {
    let get = |verb: &str| fetch(path, origin, route, verb, deadline);
    let mut attempt = 0;
    loop {
        attempt += 1;
        let s0 = stamp(&want_status(get("status")?)?);
        settle(SETTLE);
        let rows = want_lines(get("text")?)?;
        let cursor = parse_cursor(&want_status(get("cursor")?)?);
        let alt = parse_alt(&want_lines(get("modes")?)?);
        let cast = match get("cast")? {
            Reply::Bytes(body) => String::from_utf8(body).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "the cast body is not UTF-8")
            })?,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "expected the cast body",
                ));
            }
        };
        let s1 = stamp(&want_status(get("status")?)?);
        let quiescent = s0.is_some() && s0 == s1;
        if quiescent || attempt >= CUT_ATTEMPTS {
            return Ok(Material {
                cast,
                rows,
                cursor,
                alt,
                cut: if quiescent {
                    DriftCut::Quiescent
                } else {
                    DriftCut::Racy
                },
            });
        }
    }
}

/// What asking the server for `cast drift` came to.
enum Probe {
    /// The server answered — its report printed, or its refusal reported.
    Answered(ExitCode),
    /// The server predates the verb: it answered the bare cast (consumed).
    Predates,
}

/// Send `request` (`[@<sid> ]cast drift …\n`) and read the answer. A report
/// (`OK <n> verdict=…`) is printed header first; an `ERR` goes to stderr as
/// `exchange` reports one; a lone byte count is an older server's bare cast,
/// read off the socket and dropped.
fn probe(
    path: &str,
    origin: TargetOrigin,
    request: &str,
    deadline: Option<Duration>,
) -> io::Result<Probe> {
    // `exchange`'s connect phase ([`converse_served`]): a connection no instance
    // accepted is reported as that in seconds, not after `deadline`, and a busy
    // listener's refusal as itself. The client-side fallback's own reads
    // ([`fetch`]) follow only once this connection has proved the instance is
    // serving.
    let served = converse_served(path, origin, deadline, |stream| {
        if let Some(code) = send_served(&stream, request.as_bytes())? {
            return Ok(Probe::Answered(code));
        }
        let mut reader = BufReader::new(&stream);
        let mut status = String::new();
        if read_status_line(&mut reader, &mut status)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed the connection without responding",
            ));
        }
        let status = status.trim_end_matches(['\r', '\n']);
        let Some(tail) = status.strip_prefix("OK ") else {
            stderr_line(status)?;
            drain_refusal_tail(&mut reader)?;
            return Ok(Probe::Answered(ExitCode::FAILURE));
        };
        if tail.split_whitespace().any(|t| t.starts_with("verdict=")) {
            let n = stream_count(tail).ok_or_else(|| malformed_header_error(status))?;
            let rows = read_lines(&mut reader, n)?;
            let stdout = stdout_handle();
            let mut out = StdoutSink(io::BufWriter::new(stdout.lock()));
            out.write_all(status.as_bytes())?;
            out.write_all(b"\n")?;
            for row in &rows {
                out.write_all(row.as_bytes())?;
                out.write_all(b"\n")?;
            }
            out.flush()?;
            return Ok(Probe::Answered(ExitCode::SUCCESS));
        }
        // A lone byte count is the BARE cast an older server answers for any `cast
        // <word>` but `frames`: consume it (the connection stays orderly), and let
        // the caller compute the report.
        let n = match (tail.split_whitespace().count(), byte_count(tail)) {
            (1, Some(n)) => n,
            _ => return Err(malformed_header_error(status)),
        };
        io::copy(&mut (&mut reader).take(n as u64), &mut io::sink())?;
        Ok(Probe::Predates)
    })?;
    Ok(served.unwrap_or_else(Probe::Answered))
}

/// The live form: ask the server (through `route`'s relay, if any); compute
/// client-side when it predates the verb.
pub(crate) fn live(
    path: &str,
    origin: TargetOrigin,
    route: Route<'_>,
    tail: &[String],
    deadline: Option<Duration>,
    local: &LocalVerbs,
) -> io::Result<ExitCode> {
    live_with(path, origin, route, tail, deadline, local, &|d| {
        std::thread::sleep(d);
    })
}

/// Why a relayed remote that predates the verb gets no report: said by name,
/// with the two ways that still get one.
fn relay_refusal(why: &str) -> io::Error {
    let mut msg = String::from(
        "the aterm this `dial` reached predates `cast drift` (it answered the bare cast, not a \
         report), and ",
    );
    msg.push_str(why);
    msg.push_str(
        "; ask it with a newer `aterm ctl` on its own machine, or save its `cast` and `text` \
         and run `aterm ctl cast drift --file <cast> --screen <text>`",
    );
    io::Error::other(msg)
}

/// [`live`] with the settle step injected, so a test drives the fallback
/// against a mock server without sleeping.
fn live_with(
    path: &str,
    origin: TargetOrigin,
    route: Route<'_>,
    tail: &[String],
    deadline: Option<Duration>,
    local: &LocalVerbs,
    settle: &dyn Fn(Duration),
) -> io::Result<ExitCode> {
    let args = tail.join(" ");
    let mut verb = String::from("cast drift");
    if !args.is_empty() {
        verb.push(' ');
        verb.push_str(&args);
    }
    let mut request = route.line(&verb);
    request.push('\n');
    if let Probe::Answered(code) = probe(path, origin, &request, deadline)? {
        return Ok(code);
    }
    let Some(analyze) = local.cast_drift else {
        return Err(no_analyzer_error(if route.dial.is_some() {
            "the aterm this `dial` reached predates `cast drift` (it answered the bare cast)"
        } else {
            "this aterm predates `cast drift` (it answered the bare cast)"
        }));
    };
    let m = match read_material(path, origin, route, deadline, settle) {
        Ok(m) => m,
        // Only the RELAY's failure is the refusal: through it the remote is
        // asked the same read-only verbs a local fallback asks. Anything the
        // remote itself answered (a tab that closed meanwhile, a reply of the
        // wrong shape) is reported as it is, as on the direct route.
        Err(e) if route.dial.is_some() && relay_failed(&e) => {
            let mut why =
                String::from("the relay could not carry the reads a client-side report needs (");
            why.push_str(&e.to_string());
            why.push(')');
            return Err(relay_refusal(&why));
        }
        Err(e) => return Err(e),
    };
    stderr_line(if route.dial.is_some() {
        "the aterm this `dial` reached predates `cast drift`; computed client-side through the \
         relay from its status, text, cursor, modes and cast (computed=client)"
    } else {
        "this aterm predates `cast drift`; computed client-side from its status, text, cursor, \
         modes and cast (computed=client)"
    })?;
    let job = CastDriftJob {
        cast: &m.cast,
        screen: Some(&m.rows),
        cursor: m.cursor,
        alt: m.alt,
        args: &args,
        cut: m.cut,
        until: None,
        peer: DriftPeer::PreUndo,
    };
    print_report(analyze(&job))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn the_offline_flags_parse_and_refuse() {
        assert_eq!(parse_offline(&strings(&["max_runs=3"])).unwrap(), None);
        assert_eq!(
            parse_offline(&strings(&[
                "--file",
                "a.cast",
                "rows=4",
                "--screen=s.txt",
                "--until",
                "2650.5",
            ]))
            .unwrap(),
            Some(Offline {
                file: "a.cast".to_string(),
                screen: Some("s.txt".to_string()),
                until: Some(2650.5),
                args: strings(&["rows=4"]),
            })
        );
        for bad in [
            &["--screen", "s.txt"][..],
            &["--file"][..],
            &["--file", "a", "--until", "soon"][..],
            &["--file", "a", "--json"][..],
        ] {
            assert!(parse_offline(&strings(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_drift_request_shapes_are_recognized() {
        assert!(is_cast_drift(&strings(&["cast", "drift"])));
        assert!(!is_cast_drift(&strings(&["cast", "frames"])));
        assert!(!is_cast_drift(&strings(&["cast"])));
        assert!(is_drift_request("cast", "@s-a cast drift rows=2\n"));
        assert!(is_drift_request("cast", "cast drift"));
        assert!(!is_drift_request("cast", "cast frames"));
        assert_eq!(
            stamp("OK schema=1 agent_gen=4.5 gen=4.9 seq=9 hash=ab"),
            Some(("9".to_string(), "ab".to_string()))
        );
        assert_eq!(stamp("OK nothing"), None);
        assert_eq!(parse_cursor("OK 58 2 1 blinking_block"), Some((58, 2)));
        assert_eq!(
            parse_alt(&strings(&["cursor_visible=true", "alt_screen=true"])),
            Some(true)
        );
    }

    /// [`echo`], refusing a job that does not name the peer the live fallback
    /// computes against: an aterm older than the verb, so older than the
    /// resize undo.
    fn echo_pre_undo(job: &CastDriftJob<'_>) -> Result<String, String> {
        if job.peer == DriftPeer::PreUndo {
            echo(job)
        } else {
            Err(format!("the live fallback said peer={:?}", job.peer))
        }
    }

    /// [`echo`], refusing a job that does not leave the peer unknown, as a
    /// saved recording's is.
    fn echo_unknown(job: &CastDriftJob<'_>) -> Result<String, String> {
        if job.peer == DriftPeer::Unknown {
            echo(job)
        } else {
            Err(format!("the offline form said peer={:?}", job.peer))
        }
    }

    /// A fake analyzer that echoes what it was handed, so the test sees exactly
    /// the material the client read.
    fn echo(job: &CastDriftJob<'_>) -> Result<String, String> {
        let screen = job.screen.map_or(0, <[String]>::len);
        Ok(format!(
            "OK 1 verdict=echo computed=client\ncast_bytes={} screen={screen} cursor={:?} alt={:?} args={} cut={:?} peer={:?}\n",
            job.cast.len(),
            job.cursor,
            job.alt,
            job.args,
            job.cut,
            job.peer
        ))
    }

    /// The fallback end to end against a mock OLDER server: it answers `cast
    /// drift` with the bare cast, so the client reads status/text/cursor/modes/
    /// cast itself (a stamp that does not move: quiescent) and hands the
    /// analyzer the material — and the standalone client without one refuses
    /// by name instead of misreading the byte body as rows.
    #[cfg(unix)]
    #[test]
    fn an_older_server_gets_the_report_computed_client_side() {
        use std::os::unix::net::UnixListener;
        const CAST: &str = "{\"version\": 2, \"width\": 20, \"height\": 8}\n[0.1, \"o\", \"hi\"]\n";
        let dir = std::env::temp_dir().join(format!("aterm-ctl-drift-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("old.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).expect("bind");
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            // 1 probe + 6 reads for the first (quiescent) attempt, then the
            // no-analyzer probe.
            for _ in 0..8 {
                let (conn, _) = listener.accept().expect("accept");
                let mut r = BufReader::new(conn.try_clone().expect("clone"));
                let mut line = String::new();
                r.read_line(&mut line).expect("request");
                let req = line.trim_end().to_string();
                let verb = req.split_whitespace().nth(1).unwrap_or("").to_string();
                let reply = match verb.as_str() {
                    "cast" => format!("OK {}\n{CAST}", CAST.len()),
                    "status" => "OK schema=1 gen=1.7 seq=7 hash=abc\n".to_string(),
                    "text" => "OK 2\nrow zero\nrow one\n".to_string(),
                    "cursor" => "OK 1 3 1 blinking_block\n".to_string(),
                    "modes" => "OK 2\nalt_screen=true\ncursor_visible=true\n".to_string(),
                    _ => "ERR unknown verb (try: help)\n".to_string(),
                };
                let mut w = conn;
                w.write_all(reply.as_bytes()).expect("reply");
                w.flush().expect("flush");
                seen.push(req);
            }
            seen
        });
        let path = sock.to_str().expect("utf8").to_string();
        let local = LocalVerbs {
            cast_drift: Some(echo_pre_undo),
        };
        let code = live_with(
            &path,
            TargetOrigin::Pinned,
            Route {
                dial: None,
                selector: Some("@s-x"),
            },
            &strings(&["rows=3"]),
            Some(Duration::from_secs(10)),
            &local,
            &|_| {},
        )
        .expect("computed client-side");
        assert_eq!(code, ExitCode::SUCCESS);
        let refused = live_with(
            &path,
            TargetOrigin::Pinned,
            Route {
                dial: None,
                selector: Some("@s-x"),
            },
            &[],
            Some(Duration::from_secs(10)),
            &LocalVerbs::NONE,
            &|_| {},
        )
        .expect_err("no analyzer: refused by name");
        assert!(
            refused.to_string().contains("predates `cast drift`"),
            "{refused}"
        );
        let seen = server.join().expect("server");
        assert_eq!(
            seen,
            [
                "@s-x cast drift rows=3",
                "@s-x status",
                "@s-x text",
                "@s-x cursor",
                "@s-x modes",
                "@s-x cast",
                "@s-x status",
                "@s-x cast drift",
            ]
        );
        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir(&dir);
    }

    /// A mock control server on a fresh socket: each connection reads one
    /// request line and answers `reply(verb)`, where the verb is the first
    /// token after any `dial <name>` head and `@<selector>`. Returns the
    /// socket path and the thread that hands back the request lines seen.
    #[cfg(unix)]
    fn mock_server(
        tag: &str,
        connections: usize,
        reply: fn(&str) -> String,
    ) -> (std::path::PathBuf, std::thread::JoinHandle<Vec<String>>) {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("aterm-ctl-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("m.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).expect("bind");
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..connections {
                let (conn, _) = listener.accept().expect("accept");
                let mut r = BufReader::new(conn.try_clone().expect("clone"));
                let mut line = String::new();
                r.read_line(&mut line).expect("request");
                let req = line.trim_end().to_string();
                let mut toks = req.split_whitespace().peekable();
                if toks.peek() == Some(&"dial") {
                    toks.next();
                    toks.next();
                }
                if toks.peek().is_some_and(|t| t.starts_with('@')) {
                    toks.next();
                }
                let verb = toks.next().unwrap_or("").to_string();
                let mut w = conn;
                w.write_all(reply(&verb).as_bytes()).expect("reply");
                w.flush().expect("flush");
                seen.push(req);
            }
            seen
        });
        (sock, server)
    }

    const OLD_CAST: &str = "{\"version\": 2, \"width\": 20, \"height\": 8}\n[0.1, \"o\", \"hi\"]\n";

    /// An older aterm behind the relay: every verb the fallback reads.
    fn older_remote(verb: &str) -> String {
        match verb {
            "cast" => format!("OK {}\n{OLD_CAST}", OLD_CAST.len()),
            "status" => "OK schema=1 gen=1.7 seq=7 hash=abc\n".to_string(),
            "text" => "OK 2\nrow zero\nrow one\n".to_string(),
            "cursor" => "OK 1 3 1 blinking_block\n".to_string(),
            "modes" => "OK 2\nalt_screen=true\ncursor_visible=true\n".to_string(),
            _ => "ERR unknown verb (try: help)\n".to_string(),
        }
    }

    /// `dial <name> [@<sid>] cast drift` against a remote that PREDATES the
    /// verb: the report is computed here from the remote's own status, text,
    /// cursor, modes and cast, every read sent through the relay head and
    /// framed as the remote's answer (the cast's byte body, the text's rows).
    #[cfg(unix)]
    #[test]
    fn a_relayed_older_remote_gets_the_report_computed_client_side() {
        let (sock, server) = mock_server("dial-drift", 7, older_remote);
        let path = sock.to_str().expect("utf8").to_string();
        let local = LocalVerbs {
            cast_drift: Some(echo_pre_undo),
        };
        let route = Route {
            dial: Some("box"),
            selector: Some("@s-r"),
        };
        let code = live_with(
            &path,
            TargetOrigin::Pinned,
            route,
            &strings(&["rows=3"]),
            Some(Duration::from_secs(10)),
            &local,
            &|_| {},
        )
        .expect("computed client-side through the relay");
        assert_eq!(code, ExitCode::SUCCESS);
        let seen = server.join().expect("server");
        assert_eq!(
            seen,
            [
                "dial box @s-r cast drift rows=3",
                "dial box @s-r status",
                "dial box @s-r text",
                "dial box @s-r cursor",
                "dial box @s-r modes",
                "dial box @s-r cast",
                "dial box @s-r status",
            ]
        );
        let _ = std::fs::remove_file(&sock);
        let _ = sock.parent().map(std::fs::remove_dir);
    }

    /// The refusal stays for a relay that cannot carry the reads: the remote
    /// answered the bare cast, but the relay refuses the next request. The
    /// error names the relay and why, and still says how to get a report.
    #[cfg(unix)]
    #[test]
    fn a_relay_that_cannot_carry_the_reads_is_refused_by_name() {
        fn only_the_probe(verb: &str) -> String {
            match verb {
                "cast" => older_remote(verb),
                _ => "ERR dial box: connection reset\n".to_string(),
            }
        }
        let (sock, server) = mock_server("dial-refuse", 2, only_the_probe);
        let path = sock.to_str().expect("utf8").to_string();
        let local = LocalVerbs {
            cast_drift: Some(echo),
        };
        let route = Route {
            dial: Some("box"),
            selector: None,
        };
        let refused = live_with(
            &path,
            TargetOrigin::Pinned,
            route,
            &[],
            Some(Duration::from_secs(10)),
            &local,
            &|_| {},
        )
        .expect_err("the relay dropped the reads");
        let msg = refused.to_string();
        assert!(
            msg.contains("the aterm this `dial` reached predates `cast drift`")
                && msg.contains("the relay could not carry")
                && msg.contains("connection reset")
                && msg.contains("--file <cast> --screen <text>"),
            "{msg}"
        );
        assert_eq!(
            server.join().expect("server"),
            ["dial box cast drift", "dial box status"]
        );
        let _ = std::fs::remove_file(&sock);
        let _ = sock.parent().map(std::fs::remove_dir);
    }

    /// A read the REMOTE refused came through a relay that worked: the tab
    /// closed between the probe and the reads, so the remote answers `ERR no
    /// such session`. That is reported as itself, as on the direct route —
    /// never framed as the relay failing, with advice to upgrade the remote.
    #[cfg(unix)]
    #[test]
    fn a_remote_refusal_through_a_working_relay_is_not_blamed_on_the_relay() {
        fn tab_closed(verb: &str) -> String {
            match verb {
                "cast" => older_remote(verb),
                _ => "ERR no such session\n".to_string(),
            }
        }
        let (sock, server) = mock_server("dial-gone", 2, tab_closed);
        let path = sock.to_str().expect("utf8").to_string();
        let local = LocalVerbs {
            cast_drift: Some(echo),
        };
        let route = Route {
            dial: Some("box"),
            selector: Some("@s-gone"),
        };
        let err = live_with(
            &path,
            TargetOrigin::Pinned,
            route,
            &[],
            Some(Duration::from_secs(10)),
            &local,
            &|_| {},
        )
        .expect_err("the remote refused the read");
        let msg = err.to_string();
        assert_eq!(msg, "dial box @s-gone status: ERR no such session");
        assert!(!relay_failed(&err));
        assert_eq!(
            server.join().expect("server"),
            ["dial box @s-gone cast drift", "dial box @s-gone status"]
        );
        let _ = std::fs::remove_file(&sock);
        let _ = sock.parent().map(std::fs::remove_dir);

        // What IS the relay: its own `ERR dial …`, and a transport that broke.
        let refused = |status: &str| {
            io::Error::other(Refused {
                request: "dial box status".to_string(),
                status: status.to_string(),
            })
        };
        assert!(relay_failed(&refused("ERR dial box: connection reset")));
        assert!(relay_failed(&io::Error::from(io::ErrorKind::UnexpectedEof)));
        assert!(!relay_failed(&io::Error::new(
            io::ErrorKind::InvalidData,
            "the cast body is not UTF-8"
        )));
    }

    /// `dial <name> [@<sid>] <verb…>` splits into its parts; a bare dial and
    /// any other request do not.
    #[test]
    fn the_dial_head_splits_off_the_request() {
        let parts = strings(&["dial", "box", "@s-a", "cast", "drift", "rows=2"]);
        let (name, sel, rest) = split_dial(&parts).expect("dial");
        assert_eq!((name, sel), ("box", Some("@s-a")));
        assert!(is_cast_drift(rest));
        let parts = strings(&["dial", "box", "cast", "drift"]);
        assert_eq!(
            split_dial(&parts).map(|(n, s, r)| (n, s, r.len())),
            Some(("box", None, 2))
        );
        assert!(split_dial(&strings(&["dial", "box"])).is_none());
        assert!(split_dial(&strings(&["cast", "drift"])).is_none());
        let route = Route {
            dial: Some("box"),
            selector: Some("@s-a"),
        };
        assert_eq!(route.line("text"), "dial box @s-a text");
        assert_eq!(route.framed("text"), "@s-a text");
        assert_eq!(Route::default().line("cast"), "cast");
    }

    /// The material itself, through the same mock: what the analyzer is handed.
    #[test]
    fn the_echo_analyzer_sees_the_material() {
        let rows = strings(&["a", "b"]);
        let job = CastDriftJob {
            cast: "xyz",
            screen: Some(&rows),
            cursor: Some((1, 3)),
            alt: Some(true),
            args: "rows=3",
            cut: DriftCut::Quiescent,
            until: None,
            peer: DriftPeer::PreUndo,
        };
        assert_eq!(
            echo(&job).unwrap(),
            "OK 1 verdict=echo computed=client\ncast_bytes=3 screen=2 cursor=Some((1, 3)) \
             alt=Some(true) args=rows=3 cut=Quiescent peer=PreUndo\n"
        );
        assert_eq!(screen_rows("one\r\ntwo\n"), strings(&["one", "two"]));
    }

    /// The offline form with no analyzer refuses by name, and with a selector is
    /// a usage error; a live-form request is not taken.
    #[test]
    fn the_offline_entry_refuses_what_it_cannot_do() {
        let parts = strings(&["cast", "drift", "--file", "/nonexistent.cast"]);
        let err = offline_entry(&parts, &LocalVerbs::NONE).expect_err("no analyzer");
        assert!(
            err.to_string().contains("does not link the engine"),
            "{err}"
        );
        let with_sel = strings(&["@s-a", "cast", "drift", "--file", "x"]);
        let local = LocalVerbs {
            cast_drift: Some(echo),
        };
        assert!(offline_entry(&with_sel, &local).is_err());
        let live_form = strings(&["@s-a", "cast", "drift", "rows=2"]);
        assert!(offline_entry(&live_form, &local).unwrap().is_none());
        let missing = offline_entry(&parts, &local).expect_err("unreadable file");
        assert!(
            missing.to_string().contains("/nonexistent.cast"),
            "{missing}"
        );
        // A readable file: the analyzer is told nothing about which aterm made
        // it (a live fallback's peer would be refused here).
        let dir = std::env::temp_dir().join(format!("aterm-ctl-offline-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("saved.cast");
        std::fs::write(&file, "{\"version\": 2, \"width\": 20, \"height\": 8}\n").expect("write");
        let saved = strings(&["cast", "drift", "--file", file.to_str().expect("utf8")]);
        let unknown = LocalVerbs {
            cast_drift: Some(echo_unknown),
        };
        assert_eq!(
            offline_entry(&saved, &unknown).expect("computed offline"),
            Some(ExitCode::SUCCESS)
        );
        let pre_undo = LocalVerbs {
            cast_drift: Some(echo_pre_undo),
        };
        assert_eq!(
            offline_entry(&saved, &pre_undo).expect("refused by the analyzer"),
            Some(ExitCode::FAILURE)
        );
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&dir);
    }
}
