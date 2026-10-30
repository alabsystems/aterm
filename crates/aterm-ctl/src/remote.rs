// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl --host <h> …`.

use std::env;
use std::fmt;
use std::io::{self, BufReader, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use aterm_types::control_verbs::turn_option_words;

use super::{
    EXIT_TIMEOUT, LocalVerbs, SELF_SID_ENV, any_host, await_task, bytes_payload,
    malformed_header_error, print_frame_answer, print_payload, print_stdout_line,
    read_frame_answer, read_status_line, read_task_payload, reply_is_timeout, request_line,
    spawn_wire_line, split_selector, stderr_line, stderr_prefixed, stream_count, streams_payload,
    survey, usage_error, validate_request_line_len, validate_request_parts, worker_starter,
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
    /// word names: exit 2, as for an instance that accepted and did not serve.
    pub unserved: bool,
    /// Nothing answered the dial within its deadline, or no route reached the peer (a Mac asleep
    /// or off its network, an aterm frozen): an `await task` past its baseline asks again.
    pub silent: bool,
    /// The peer serves no drive, its fleet level below drive: an `await task` past its baseline,
    /// which it served, says its person took this Mac's grant back.
    pub ungranted: bool,
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
            silent: false,
            ungranted: false,
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
    /// The caller's `--timeout` (the first), shorter than what the call waits with a longer one,
    /// or 0 (the second).
    Timeout(Duration, Longer),
}

/// What a drive cut by a shorter `--timeout` waits with a longer one, or 0 ([`Within::Timeout`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Longer {
    /// Up to the drive's own deadline, an `await task` leg's where its `timeout=` ends the wait.
    Drive(Duration),
    /// An `await task` the `--timeout` ends: as long as a longer one names, and with 0 its whole
    /// `timeout=`, or with none until the news comes (a minute of asking a silent peer again).
    Wait(Option<Duration>),
}

impl Within {
    const fn get(self) -> Duration {
        match self {
            Self::Drive(within) | Self::Timeout(within, _) => within,
        }
    }
}

/// A drive connection's deadline: [`DRIVE_WITHIN`], or a shorter `--timeout` (`total`, none for
/// `--timeout 0`).
fn drive_within(total: Option<Option<Duration>>) -> Within {
    match total {
        Some(Some(total)) if total < DRIVE_WITHIN => {
            Within::Timeout(total, Longer::Drive(DRIVE_WITHIN))
        }
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
        if is_any_host(host) {
            return Err(refused(
                "`--host any` picks one Mac to drive a worker on: run `aterm ctl survey --fleet` \
                 to read every fleet peer",
            ));
        }
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
    if await_task::is_await_task(parts) {
        if is_any_host(host) {
            return await_task::run_driven_any(parts, total, drive, local.fleet_survey);
        }
        return await_task::run_driven(parts, total, host, drive);
    }
    refuse_local_selector(parts)?;
    let (selector, rest) = split_selector(parts);
    let verb = rest.first().map(String::as_str).ok_or_else(usage_error)?;
    let args = &rest[1..];
    let within = drive_within(total);
    if verb == "spawn" {
        if selector.is_some() {
            return Err(refused(
                "a worker spawned over the fleet opens where that Mac's aterm puts it: name no \
                 @<sel>",
            ));
        }
        let head = spawn_head(args, worker_starter(env::var(SELF_SID_ENV).ok()).as_deref())?;
        let chosen_host = if is_any_host(host) {
            any_host_node(args, total, local)?
        } else {
            host.to_owned()
        };
        let task = read_task_payload()?;
        let (code, spawned_sid) = spawn_exchange(
            drive(&chosen_host, within.get()).map_err(io::Error::other)?,
            &format!("{head} len={}", task.len()),
            &task,
            within,
        )?;
        if let Some(sid) = spawned_sid {
            record_any_sid_host(&sid, &chosen_host);
        }
        return Ok(code);
    }
    if matches!(verb, "feed-bin" | "paste")
        || (verb == "send" && matches!(args, [one] if one == "--stdin" || one == "-"))
    {
        return Err(refused(
            "a drive types inline text alone (`@<w> send <text>`, `@<w> turn <text>`): no \
             stdin frame but a worker's task crosses the fleet",
        ));
    }
    let resolved_host = if is_any_host(host) {
        let sid = any_target_sid(selector, verb, args)?;
        resolve_any_sid_hosts(&[sid], total, local.fleet_survey)?
            .into_iter()
            .next()
            .expect("one sid yields one host")
    } else {
        host.to_owned()
    };
    let host = resolved_host.as_str();
    let mut parts = parts.to_vec();
    if verb == "turn" {
        let words = turn_wait(args, selector, host)?;
        parts.truncate(parts.len() - args.len());
        parts.push(words);
    }
    let mut request = request_line(&parts)?;
    validate_request_line_len(&request, Some(verb))?;
    request.push('\n');
    exchange_with_retry(&request, verb, within, |remaining| {
        drive(host, remaining).map_err(|refused| match (&refused.left, args) {
            (Some(left), [sid, off, ..]) if verb == "hold" && off == "off" => {
                io::Error::other(left_holding(host, sid, left, &refused.why))
            }
            _ => io::Error::other(refused),
        })
    })
}

/// A complete busy reply promises the request was not dispatched. Only that response may
/// redial; sends, EOFs and partial responses remain ambiguous and are never repeated here.
fn exchange_with_retry(
    request: &str,
    verb: &str,
    within: Within,
    mut dial: impl FnMut(Duration) -> io::Result<DriveLink>,
) -> io::Result<ExitCode> {
    let started = Instant::now();
    let mut remaining = within.get();
    loop {
        let link = dial(remaining)?;
        let label = link.label.clone();
        match exchange(link, request, verb, within)? {
            Exchange::Completed(code) => return Ok(code),
            Exchange::Busy => {
                remaining = within.get().saturating_sub(started.elapsed());
                if !remaining.is_zero() {
                    std::thread::sleep(remaining.min(Duration::from_millis(50)));
                    remaining = within.get().saturating_sub(started.elapsed());
                }
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "{label} remained busy until this call's deadline; the request was not dispatched"
                        ),
                    ));
                }
            }
        }
    }
}

/// How `--host any` is used ([`any_host_node`]).
const ANY_USAGE: &str = "aterm ctl --host any spawn agent=<claude|codex> [model=<m>] \
                         [cwd=<absolute folder there>] - < task";

/// The node a `--host any` spawn (its words after `spawn`, `args`) goes to ([`any_host`]): one
/// survey of every member within the call's `--timeout` (`total`) where that is shorter than a
/// survey's own, the pick, and a line on stderr naming it. The spawn is sent there as `--host
/// <node>` sends it, and the worker's later verbs follow it there ([`record_any_sid_host`]).
/// # Errors
/// `args` spawn no Claude Code or Codex worker, this client links no fleet, the survey was
/// refused, or no member can take the worker.
fn any_host_node(
    args: &[String],
    total: Option<Option<Duration>>,
    local: &LocalVerbs,
) -> io::Result<String> {
    let program = args
        .iter()
        .find_map(|a| a.strip_prefix("agent="))
        .filter(|word| any_host::PROGRAMS.contains(word))
        .ok_or_else(|| {
            refused(format!(
                "--host any picks a Mac by the accounts a Claude Code or Codex worker runs on: \
                 `{ANY_USAGE}`"
            ))
        })?;
    let (Some(fleet_survey), Some(_)) = (local.fleet_survey, local.fleet_drive) else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "--host any reads and drives the fleet, which this aterm-ctl does not link: run \
             `aterm ctl --host any …`",
        ));
    };
    let within = survey_within_for_any(total);
    let read =
        fleet_survey(&survey::FleetAsk { host: None, within }).map_err(fleet_survey_error)?;
    let members = any_host::members_of(&read.peers);
    let chosen = any_host::pick(program, &members)
        .map_err(|counts| refused(any_host::none_able(program, &counts, read.alone.as_deref())))?;
    let member = chosen.member;
    let node = member
        .node
        .clone()
        .ok_or_else(|| refused("--host any chose a Mac with no node"))?;
    stderr_prefixed(
        "",
        &format!(
            "--host any: chose {} ({}): {}",
            aterm_codec::tty::one_line(member.host(), 80),
            member.addr.as_deref().unwrap_or("address unknown"),
            any_host::why(program, &chosen)
        ),
    )?;
    Ok(node)
}

/// Why a `hold <sid> off` cannot be sent to `host` by this machine, out of the fleet now or
/// holding a book that names no such Mac ([`Left`]): the dial's own refusal `why`, then, as a
/// condition this machine cannot check, what lifts a hold it took there under any grant
/// ([`lifts_there`]) and when the same `hold off` from here works.
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

/// What lifts, on another Mac, a hold this machine (`node`) took there, under any grant there:
/// the person's Take Over/Hand Back or a lower drive level. Peer removal also drops named
/// grants on that Mac, but another member can restore signed membership under an all-member grant.
#[must_use]
pub fn lifts_there(node: &str) -> String {
    format!(
        "that Mac lifts the hold when its person takes the tab (Take Over then Hand Back) or \
         runs `aterm fleet allow content` there; `aterm fleet peers rm {node}` there also drops \
         grants naming this node, but an all-member grant can admit a valid signed entry merged \
         back from another member's book"
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
) -> io::Result<(ExitCode, Option<String>)> {
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
        return Ok((print_frame_answer(&answer)?, None));
    }
    let sent = reader.get_mut();
    sent.write_all(task)
        .and_then(|()| sent.flush())
        .map_err(cut)?;
    while answer == go {
        answer = read_frame_answer(&mut reader).map_err(cut)?;
    }
    let spawned_sid = spawned_sid_from_answer(&answer);
    Ok((print_frame_answer(&answer)?, spawned_sid))
}

const ANY_HOSTS_FILE: &str = "driven-any-hosts.tsv";
const ANY_HOSTS_MAX: usize = 512;

#[cfg(test)]
std::thread_local! {
    static ANY_HOSTS_OVERRIDE: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

fn any_hosts_path() -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(custom) = ANY_HOSTS_OVERRIDE.with(|cell| cell.borrow().clone()) {
        return Some(custom);
    }
    super::socket_dir().map(|dir| dir.join(ANY_HOSTS_FILE))
}

fn is_any_host(host: &str) -> bool {
    host.eq_ignore_ascii_case(any_host::ANY)
}

fn spawned_sid_from_answer(answer: &str) -> Option<String> {
    let line = answer.trim_end_matches(['\r', '\n']);
    let rest = line.strip_prefix("OK ")?;
    rest.split_whitespace()
        .map(|w| w.trim_start_matches('@'))
        .find(|w| is_sid(w))
        .map(str::to_owned)
}

fn any_target_sid<'a>(
    selector: Option<&'a str>,
    verb: &str,
    args: &'a [String],
) -> io::Result<&'a str> {
    if let Some(sel) = selector {
        let sid = sel.trim_start_matches('@');
        if is_sid(sid) {
            return Ok(sid);
        }
    }
    if verb == "hold"
        && let Some(first) = args.first()
    {
        let sid = first.trim_start_matches('@');
        if is_sid(sid) {
            return Ok(sid);
        }
    }
    Err(refused(
        "`--host any` follows a worker by its `@s-<hex>` sid (or `hold s-<hex> on|off`)",
    ))
}

fn read_any_sid_hosts() -> Vec<(String, String)> {
    let Some(path) = any_hosts_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let (sid, host) = line.split_once('\t')?;
            let (sid, host) = (sid.trim(), host.trim());
            (is_sid(sid) && !host.is_empty()).then(|| (sid.to_owned(), host.to_owned()))
        })
        .collect()
}

pub(crate) fn record_any_sid_host(sid: &str, host: &str) {
    if !is_sid(sid) || host.is_empty() {
        return;
    }
    let Some(path) = any_hosts_path() else {
        return;
    };
    let mut rows = read_any_sid_hosts();
    rows.retain(|(existing_sid, _)| existing_sid != sid);
    rows.push((sid.to_owned(), host.to_owned()));
    if rows.len() > ANY_HOSTS_MAX {
        let drop_n = rows.len() - ANY_HOSTS_MAX;
        rows.drain(..drop_n);
    }
    let mut body = String::new();
    for (s, h) in rows {
        body.push_str(&s);
        body.push('\t');
        body.push_str(&h);
        body.push('\n');
    }
    #[cfg(unix)]
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
        && aterm_uds::private_file::ensure_private_dir(parent).is_ok()
    {
        let _ = aterm_uds::private_file::write_private(parent, name, body.as_bytes());
    }
    #[cfg(not(unix))]
    {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, body.as_bytes());
    }
}

fn survey_within_for_any(total: Option<Option<Duration>>) -> Duration {
    match total.flatten() {
        Some(flag) if !flag.is_zero() && flag < survey::FLEET_WITHIN => flag,
        _ => survey::FLEET_WITHIN,
    }
}

fn fleet_survey_error(refused: survey::FleetRefused) -> io::Error {
    io::Error::other(DriveRefused {
        unserved: refused.unserved,
        ..DriveRefused::once(refused.why)
    })
}

pub(crate) fn resolve_any_sid_hosts(
    sids: &[&str],
    total: Option<Option<Duration>>,
    survey_fn: Option<crate::FleetSurveyFn>,
) -> io::Result<Vec<String>> {
    let mut table = read_any_sid_hosts();
    let lookup = |rows: &[(String, String)], target: &str| -> Option<String> {
        rows.iter()
            .rev()
            .find(|(s, _)| s == target)
            .map(|(_, h)| h.clone())
    };
    if sids.iter().any(|sid| lookup(&table, sid).is_none()) {
        let Some(survey_fn) = survey_fn else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "--host any queries the fleet before driving, which this aterm-ctl does not link: \
                 run `aterm ctl --host any …`",
            ));
        };
        let read = survey_fn(&survey::FleetAsk {
            host: None,
            within: survey_within_for_any(total),
        })
        .map_err(fleet_survey_error)?;
        for peer in &read.peers {
            if peer.unproven {
                continue;
            }
            let Some(node) = peer.node.as_deref() else {
                continue;
            };
            let Ok(lines) = &peer.answer else {
                continue;
            };
            let Ok(survey) = survey::parse_reply(lines) else {
                continue;
            };
            for session in &survey.sessions {
                if let Some(found_sid) = session.get("sid") {
                    let clean = found_sid.trim_start_matches('@');
                    if is_sid(clean) {
                        record_any_sid_host(clean, node);
                        table.push((clean.to_owned(), node.to_owned()));
                    }
                }
            }
        }
    }

    let mut out = Vec::with_capacity(sids.len());
    for &sid in sids {
        let Some(host) = lookup(&table, sid) else {
            return Err(io::Error::other(format!(
                "--host any found no live fleet peer hosting @{sid}"
            )));
        };
        out.push(host);
    }
    Ok(out)
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
                 this Mac's grant, took this Mac out of its books or lowered its fleet level, its \
                 fleet key or listen place changed, or its aterm stopped or restarted)"
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
                Within::Timeout(within, longer) => {
                    let turn = if verb == "turn" {
                        format!(
                            "a `turn` there answers once it settles or its timeout= runs out, \
                             {TURN_WAIT_MS} ms where it names none; "
                        )
                    } else {
                        String::new()
                    };
                    let longer = match longer {
                        Longer::Drive(own) => format!(
                            "a longer --timeout, or 0, waits up to the drive's own {} s",
                            own.as_secs()
                        ),
                        Longer::Wait(Some(whole)) => format!(
                            "a longer --timeout waits as long as it names, up to the wait's \
                             timeout= of {} ms, and 0 that whole timeout=",
                            whole.as_millis()
                        ),
                        Longer::Wait(None) => {
                            "a longer --timeout waits as long as it names, and 0 until the news \
                             comes, where a peer that answers no leg is asked again for a minute"
                                .to_owned()
                        }
                    };
                    format!(
                        "no answer came from {label} within this call's --timeout of {} s \
                         ({turn}{longer})",
                        within.as_secs()
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
#[derive(Debug)]
enum Exchange {
    Completed(ExitCode),
    Busy,
}

fn exchange(link: DriveLink, request: &str, verb: &str, within: Within) -> io::Result<Exchange> {
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
    let complete = status.ends_with('\n');
    let status = status.trim_end_matches(['\r', '\n']);
    if complete
        && status == aterm_types::control_verbs::CONTROL_BUSY_REPLY
        && reader.buffer().is_empty()
    {
        return Ok(Exchange::Busy);
    }
    let (head, tail) = status.split_once(' ').unwrap_or((status, ""));
    if head != "OK" {
        stderr_line(status)?;
        return Ok(Exchange::Completed(ExitCode::FAILURE));
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
    Ok(Exchange::Completed(if reply_is_timeout(status) {
        ExitCode::from(EXIT_TIMEOUT)
    } else {
        ExitCode::SUCCESS
    }))
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
