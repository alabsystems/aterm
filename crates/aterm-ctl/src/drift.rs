// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cast drift` on the client.
//!
//! `aterm ctl [@<sid>] cast drift [max_runs=<k>] [rows=<n>] [seed=…]` asks the
//! server first. A server that knows the verb answers `OK <n> verdict=…` and
//! the rows. One that predates it answers the bare `cast` (`OK <nbytes>` and an
//! asciicast body, no `verdict=`); then, when the binary links the engine
//! ([`LocalVerbs`]), the client computes the same report from read-only verbs
//! (`status`, `text`, `cursor`, `modes`, `cast`), so it is safe against a tab
//! in use. Its header says `computed=client`.
//!
//! The offline form reads saved files and needs no socket:
//! `aterm ctl cast drift --file <cast> [--screen <text>] [--until <t>] [k=v…]`; with no
//! screen it compares the faithful replay with the stable one (`fidelity=-`).
//!
//! The analyzer is injected, not linked: this crate is the dependency-free
//! client (workspace-only dependencies), and the `aterm` binary, which
//! links the engine, passes a plain `fn` in
//! ([`main_entry_with`](crate::main_entry_with)). The standalone dev
//! `aterm-ctl` says by name that it cannot compute the report.

use std::io::{self, BufReader, Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use super::{
    StdoutSink, TargetOrigin, byte_count, converse, converse_served, drain_refusal_tail,
    malformed_header_error, read_reply_lines, read_status_line, send_request, send_served,
    stderr_line, stdout_handle, stream_count,
};

/// Verbs this client answers in-process when its binary links the engine. The
/// `aterm` binary passes them to [`main_entry_with`](crate::main_entry_with);
/// the standalone `aterm-ctl` passes [`LocalVerbs::NONE`].
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalVerbs {
    /// The `cast drift` analyzer (`aterm_control::cast_drift` behind an adapter).
    pub cast_drift: Option<CastDriftFn>,
    /// The fleet client `survey --fleet | --host` asks the fleet's peers
    /// through (`aterm_net`'s TLS and the fleet's proof, which this
    /// dependency-free client does not link).
    pub fleet_survey: Option<crate::FleetSurveyFn>,
    /// The fleet client `--host` drives another Mac's aterm through (the
    /// step-in: the fleet's `drive` op, [`crate::FleetDriveFn`]).
    pub fleet_drive: Option<crate::FleetDriveFn>,
}

impl LocalVerbs {
    /// No local verbs: every request goes to a server.
    pub const NONE: Self = Self {
        cast_drift: None,
        fleet_survey: None,
        fleet_drive: None,
    };
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

/// Which aterm made the recording, for the one engine behaviour the replays
/// must match: the resize undo, under which a flap with no output between its
/// halves moves nothing. An aterm without it shifts its screen on every flap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriftPeer {
    /// An aterm older than `cast drift` (the live fallback), and so older than
    /// the resize undo: the replays drop it (`undo=off`).
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
/// screen. The server's own verb waits on its recording's watermark instead
/// (`CastRecorder::snapshot_at`); over the wire there is none to wait on, so the
/// client keeps the 60 ms settle that verb used before.
const SETTLE: Duration = Duration::from_millis(60);

/// Consistent-cut attempts before the client proceeds with `cut=racy`.
const CUT_ATTEMPTS: usize = 3;

/// The request line `[@<selector> ]<verb>` — what its reply is framed by.
/// `selector` is the session `cast drift`'s requests go to (`@<sid>` / `@.`),
/// if any.
fn request_line(selector: Option<&str>, verb: &str) -> String {
    let mut r = String::new();
    if let Some(sel) = selector {
        r.push_str(sel);
        r.push(' ');
    }
    r.push_str(verb);
    r
}

/// Whether `rest` (the request with any `@<selector>` split off) is `cast drift`.
pub(crate) fn is_cast_drift(rest: &[String]) -> bool {
    rest.first().map(String::as_str) == Some("cast")
        && rest.get(1).map(String::as_str) == Some("drift")
}

/// Whether the reply to `request` (by its `verb`) is a `cast drift` report,
/// whose header carries the verdict and so leads the rows on stdout.
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
    msg.push_str("; this client does not link the engine — run it as `aterm ctl …`");
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
/// it (the caller goes on to the server). A `@<selector>` with the offline flags is a
/// usage error: the files are the session.
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
            let mut out = StdoutSink::new(stdout.lock());
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

/// A request a server answered `ERR …`: the request and its status line.
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
/// framing. An `ERR` reply is an error naming the request ([`Refused`]).
fn fetch(
    path: &str,
    origin: TargetOrigin,
    selector: Option<&str>,
    verb: &str,
    deadline: Option<Duration>,
) -> io::Result<Reply> {
    let framed = request_line(selector, verb);
    let request = framed.as_str();
    converse(path, origin, deadline, |stream, token| {
        stream.set_read_timeout(deadline)?;
        stream.set_write_timeout(deadline)?;
        let mut line = String::from(request);
        line.push('\n');
        send_request(&stream, token, &line)?;
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
                Ok(Reply::Lines(read_reply_lines(&mut reader, n)?))
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

/// The screen stamp a `status` line carries: its `seq=` and `hash=` values
/// (parse by key). `None` when it carries neither, or either is `-` (no whole
/// frame to stamp) — a cut against it cannot be shown quiescent.
fn stamp(status: &str) -> Option<(String, String)> {
    let key = |k: &str| {
        status
            .split_whitespace()
            .find_map(|t| t.strip_prefix(k))
            .map(str::to_string)
    };
    match (key("seq="), key("hash=")) {
        (None, None) => None,
        (seq, hash) if seq.as_deref() == Some("-") || hash.as_deref() == Some("-") => None,
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
    selector: Option<&str>,
    deadline: Option<Duration>,
    settle: &dyn Fn(Duration),
) -> io::Result<Material> {
    let get = |verb: &str| fetch(path, origin, selector, verb, deadline);
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
/// (`OK <n> verdict=…`) is printed header first; an `ERR` goes to stderr as in `exchange`;
/// a lone byte count is an older server's bare cast, read and dropped.
fn probe(
    path: &str,
    origin: TargetOrigin,
    request: &str,
    deadline: Option<Duration>,
) -> io::Result<Probe> {
    // `exchange`'s connect phase ([`converse_served`]): an unaccepted connection is reported
    // within seconds, and a busy listener's refusal as itself. The fallback's reads
    // ([`fetch`]) run only once this connection proved the instance is serving.
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
            let rows = read_reply_lines(&mut reader, n)?;
            let stdout = stdout_handle();
            let mut out = StdoutSink::new(io::BufWriter::new(stdout.lock()));
            out.write_all(status.as_bytes())?;
            out.write_all(b"\n")?;
            for row in &rows {
                out.write_all(row.as_bytes())?;
                out.write_all(b"\n")?;
            }
            out.flush()?;
            return Ok(Probe::Answered(ExitCode::SUCCESS));
        }
        // A lone byte count is the bare cast an older server answers for any
        // `cast <word>` but `frames`: consume it and let the caller compute the
        // report.
        let n = match (tail.split_whitespace().count(), byte_count(tail)) {
            (1, Some(n)) => n,
            _ => return Err(malformed_header_error(status)),
        };
        io::copy(&mut (&mut reader).take(n as u64), &mut io::sink())?;
        Ok(Probe::Predates)
    })?;
    Ok(served.unwrap_or_else(Probe::Answered))
}

/// The live form: ask the server; compute client-side when it predates the
/// verb.
pub(crate) fn live(
    path: &str,
    origin: TargetOrigin,
    selector: Option<&str>,
    tail: &[String],
    deadline: Option<Duration>,
    local: &LocalVerbs,
) -> io::Result<ExitCode> {
    live_with(path, origin, selector, tail, deadline, local, &|d| {
        std::thread::sleep(d);
    })
}

/// [`live`] with the settle step injected, so a test drives the fallback
/// against a mock server without sleeping.
fn live_with(
    path: &str,
    origin: TargetOrigin,
    selector: Option<&str>,
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
    let mut request = request_line(selector, &verb);
    request.push('\n');
    if let Probe::Answered(code) = probe(path, origin, &request, deadline)? {
        return Ok(code);
    }
    // This instance took the request: a read that finds it gone is the call's
    // failure, never a stale entry to move past.
    let origin = origin.settled();
    let Some(analyze) = local.cast_drift else {
        return Err(no_analyzer_error(
            "this aterm predates `cast drift` (it answered the bare cast)",
        ));
    };
    let m = read_material(path, origin, selector, deadline, settle)?;
    stderr_line("this aterm predates `cast drift`; computed client-side (computed=client)")?;
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
        for unstamped in [
            "OK schema=1 gen=4.9 seq=- hash=-",
            "OK schema=1 gen=4.9 seq=9 hash=-",
            "OK schema=1 gen=4.9 seq=- hash=ab",
        ] {
            assert_eq!(stamp(unstamped), None, "{unstamped}");
        }
        assert_eq!(parse_cursor("OK 58 2 1 blinking_block"), Some((58, 2)));
        assert_eq!(screen_rows("one\r\ntwo\n"), strings(&["one", "two"]));
        assert_eq!(
            parse_alt(&strings(&["cursor_visible=true", "alt_screen=true"])),
            Some(true)
        );
    }

    /// [`echo`], refusing a job that does not name the peer the live fallback
    /// computes against (an aterm older than the verb and the resize undo).
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

    /// A fake analyzer that echoes the material it was handed.
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

    /// Against a mock older server that answers `cast drift` with the bare cast, the client
    /// reads the material itself (unmoved stamp: quiescent) and hands it to the analyzer;
    /// without an analyzer it refuses by name.
    #[cfg(unix)]
    #[test]
    fn an_older_server_gets_the_report_computed_client_side() {
        use std::io::BufRead as _;
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
            fleet_survey: None,
            fleet_drive: None,
        };
        let code = live_with(
            &path,
            TargetOrigin::Pinned,
            Some("@s-x"),
            &strings(&["rows=3"]),
            Some(Duration::from_secs(60)),
            &local,
            &|_| {},
        )
        .expect("computed client-side");
        assert_eq!(code, ExitCode::SUCCESS);
        live_with(
            &path,
            TargetOrigin::Pinned,
            Some("@s-x"),
            &[],
            Some(Duration::from_secs(60)),
            &LocalVerbs::NONE,
            &|_| {},
        )
        .expect_err("no analyzer: refused");
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

    /// The offline form with no analyzer refuses by name, and with a selector is
    /// a usage error; a live-form request is not taken.
    #[test]
    fn the_offline_entry_refuses_what_it_cannot_do() {
        let parts = strings(&["cast", "drift", "--file", "/nonexistent.cast"]);
        offline_entry(&parts, &LocalVerbs::NONE).expect_err("no analyzer");
        let with_sel = strings(&["@s-a", "cast", "drift", "--file", "x"]);
        let local = LocalVerbs {
            cast_drift: Some(echo),
            fleet_survey: None,
            fleet_drive: None,
        };
        assert!(offline_entry(&with_sel, &local).is_err());
        let live_form = strings(&["@s-a", "cast", "drift", "rows=2"]);
        assert!(offline_entry(&live_form, &local).unwrap().is_none());
        let missing = offline_entry(&parts, &local).expect_err("unreadable file");
        assert!(
            missing.to_string().contains("/nonexistent.cast"),
            "{missing}"
        );
        // A readable file: the peer is unknown (a live fallback's peer is refused here).
        let dir = std::env::temp_dir().join(format!("aterm-ctl-offline-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("saved.cast");
        std::fs::write(&file, "{\"version\": 2, \"width\": 20, \"height\": 8}\n").expect("write");
        let saved = strings(&["cast", "drift", "--file", file.to_str().expect("utf8")]);
        let unknown = LocalVerbs {
            cast_drift: Some(echo_unknown),
            fleet_survey: None,
            fleet_drive: None,
        };
        assert_eq!(
            offline_entry(&saved, &unknown).expect("computed offline"),
            Some(ExitCode::SUCCESS)
        );
        let pre_undo = LocalVerbs {
            cast_drift: Some(echo_pre_undo),
            fleet_survey: None,
            fleet_drive: None,
        };
        assert_eq!(
            offline_entry(&saved, &pre_undo).expect("refused by the analyzer"),
            Some(ExitCode::FAILURE)
        );
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&dir);
    }
}
