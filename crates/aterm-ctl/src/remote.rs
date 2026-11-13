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
    EXIT_TIMEOUT, LocalVerbs, MAX_BODY_BYTES, SELF_SID_ENV, any_host, await_task, board,
    bytes_payload, collect_reply_lines, malformed_header_error, print_frame_answer, print_payload,
    print_stdout_line, read_bounded_line, read_frame_answer, read_status_line, read_task_payload,
    redial, reply_is_timeout, request_line, spawn_wire_line, split_selector, stderr_line,
    stderr_prefixed, stream_count, streams_payload, survey, usage_error, validate_request_line_len,
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
    /// The node id the peer's handshake proved, where the dial knows it: what a board records a
    /// worker's Mac by ([`crate::board`]).
    pub node: Option<String>,
}

/// THE FLEET CLIENT'S DRIVE DIAL the `aterm` binary links: its [`DriveLink`] has already sent
/// the drive's requester line ([`aterm_types::control_verbs::Requester::line`]), so the stream
/// takes requests at once.
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

/// The dev seam that sets, in milliseconds, [`drive_deadline`] and what an `await task` leg over
/// a drive waits past the wait it asks for ([`crate::await_task`]) for a test of the binary: a
/// test world whose every peer is an aterm on the same machine, which serves and so must answer
/// however loaded the machine is, gives each drive its hang detector.
const DRIVE_WITHIN_SEAM: &str = "ATERM_DEBUG_FLEET_DRIVE_WITHIN_MS";

/// What [`DRIVE_WITHIN_SEAM`] names, where a test of the binary sets it.
pub(crate) fn drive_seam() -> Option<Duration> {
    aterm_types::dev_seam!(DRIVE_WITHIN_SEAM)
        .and_then(|ms| crate::parse_uint::<u64>(ms.to_str()?.trim()))
        .filter(|&ms| ms > 0)
        .map(|ms| Duration::from_millis(ms.min(24 * 3_600_000)))
}

/// A drive connection's own deadline: [`DRIVE_WITHIN`], or what [`DRIVE_WITHIN_SEAM`] names.
fn drive_deadline() -> Duration {
    drive_seam().unwrap_or(DRIVE_WITHIN)
}

/// `within` as a cut names it: in whole seconds where it is whole (`28 s`), else to the
/// millisecond (`0.1 s`, `9.5 s`), as a test world's seam, an `await task` leg's `timeout=` and a
/// `--timeout` can make it.
fn secs(within: Duration) -> String {
    let ms = within.as_millis();
    let part = format!("{:03}", ms % 1000);
    let part = part.trim_end_matches('0');
    if part.is_empty() {
        format!("{} s", ms / 1000)
    } else {
        format!("{}.{part} s", ms / 1000)
    }
}

/// A drive connection's deadline, its handshake's and its reply's, and whose it is, which a
/// reply cut at it names ([`closed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Within {
    /// The drive's own: [`drive_deadline`], or an `await task` leg's wait and its slack.
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

/// A drive connection's deadline: [`drive_deadline`], or a shorter `--timeout` (`total`, none for
/// `--timeout 0`).
fn drive_within(total: Option<Option<Duration>>) -> Within {
    let own = drive_deadline();
    match total {
        Some(Some(total)) if total < own => Within::Timeout(total, Longer::Drive(own)),
        _ => Within::Drive(own),
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
    let board_read = board::is_board_verb(parts);
    let board_wait = await_task::is_board_wait(parts);
    if (board_read || board_wait) && is_any_host(host) {
        return Err(refused(
            "a board belongs to the Mac that keeps it: name that Mac with `--host <h>` (`aterm \
             ctl board` reads this Mac's)",
        ));
    }
    let Some(drive) = local.fleet_drive else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "--host drives another Mac's aterm over the fleet, which this aterm-ctl does not \
             link: run `aterm ctl --host …`",
        ));
    };
    if board_read {
        return board::run_read(parts, Some((host, total, drive)));
    }
    if board_wait {
        // A worker of that board on this Mac is waited on here, never over a drive to itself.
        let here = crate::resolve_sock_for(None).ok();
        return await_task::run_board_wait(
            parts,
            total,
            &await_task::BoardSite {
                at: await_task::BoardAt::Host(host),
                here: here.as_deref(),
                own: local.fleet_node.and_then(|node| node()),
            },
            Some(drive),
            &redial::Redial::none(),
        );
    }
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
                "a session spawned over the fleet opens where that Mac's aterm puts it: name no \
                 @<sel>",
            ));
        }
        // `need=` is this client's to judge, by the survey `--host any` reads: no Mac is sent it.
        let (needs, args): (Vec<String>, Vec<String>) = args
            .iter()
            .cloned()
            .partition(|arg| arg.starts_with(any_host::NEED));
        let args = args.as_slice();
        if !needs.is_empty() && !is_any_host(host) {
            return Err(refused(
                "need= picks the Mac under --host any: with one Mac named, its survey says what \
                 it holds (`aterm ctl survey --host <h>`)",
            ));
        }
        if !args.iter().any(|arg| arg.starts_with("agent=")) {
            if is_any_host(host) {
                return Err(refused(
                    "a plain shell needs one explicit --host, never any",
                ));
            }
            let head = shell_spawn_head(args)?;
            return exchange_with_retry(&format!("{head}\n"), "spawn", within, |remaining| {
                drive(host, remaining).map_err(io::Error::other)
            });
        }
        let (args, label) = board::take_label(args)?;
        let starter = worker_starter(env::var(SELF_SID_ENV).ok());
        let head = spawn_head(&args, starter.as_deref())?;
        let (chosen_host, placed) = if is_any_host(host) {
            any_host_node(&args, &needs, total, local)?
        } else {
            (host.to_owned(), board::Placed::default())
        };
        let task = read_task_payload()?;
        let worker = Worker {
            host: chosen_host,
            placed,
            args: &args,
            label,
            starter: starter.as_deref(),
            head: &head,
            task: &task,
        };
        return spawn_worker(worker, within, drive);
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

/// Whether a fleet spawn's `cwd=` value (`dir`, unquoted) names a folder of the driven Mac: an
/// absolute path, or one in its home (`~`, `~/<path>`), which that Mac resolves.
/// # Errors
/// `~user`, or a home-relative path with a `..`: why.
fn fleet_cwd(dir: &str) -> Result<bool, &'static str> {
    match aterm_types::control_verbs::home_cwd(dir) {
        Some(Ok(_)) => Ok(true),
        Some(Err(why)) => Err(why),
        None => Ok(dir.starts_with('/')),
    }
}

/// A plain remote shell carries no task or command, names its one destination folder, and says in
/// `why=` what it is for: the one line the driven Mac keeps for a person reviewing what was asked
/// of it (ATERM-KI-1109-3), held here to the rule that Mac holds it to. A `req=<id>` (an earlier
/// spawn's reply named it) joins the new shell to that request there.
fn shell_spawn_head(args: &[String]) -> io::Result<String> {
    const FORM: &str = "a plain fleet shell is spawn cwd=/absolute|'cwd=~/<path>' 'why=<one line, at \
                        most 200 characters, saying what it is for>' [req=<id>] [raise=false]; \
                        no task, command or other options";
    let not = |arg: &str| {
        refused(format!(
            "{FORM}; not `{}`",
            aterm_codec::tty::one_line(arg, 32)
        ))
    };
    let (mut cwd, mut why, mut req) = (None, None, None);
    let mut raised = false;
    for arg in args {
        if let Some(id) = arg.strip_prefix("req=") {
            let id = aterm_types::control_verbs::request_id(id).map_err(|bad| {
                refused(format!(
                    "{FORM}; its req= {bad}: `{}`",
                    aterm_codec::tty::one_line(arg, 32)
                ))
            })?;
            if req.replace(id).is_some() {
                return Err(not(arg));
            }
            continue;
        }
        if let Some(said) = arg.strip_prefix("why=") {
            let said = aterm_types::control_verbs::request_why(said).map_err(|bad| {
                refused(format!(
                    "{FORM}; its why= {bad}: `{}`",
                    aterm_codec::tty::one_line(arg, 32)
                ))
            })?;
            if why.replace(said).is_some() {
                return Err(not(arg));
            }
        } else if let Some(dir) = arg.strip_prefix("cwd=") {
            if let Err(why) = fleet_cwd(dir) {
                return Err(refused(format!(
                    "{FORM}; `{}` {why}",
                    aterm_codec::tty::one_line(arg, 32)
                )));
            }
            if !fleet_cwd(dir).unwrap_or(false) || cwd.replace(dir).is_some() {
                return Err(not(arg));
            }
        } else if arg == "raise=false" && !raised {
            raised = true;
        } else {
            return Err(not(arg));
        }
    }
    if cwd.is_none() {
        return Err(refused(format!("{FORM}: it names no cwd=")));
    }
    if why.is_none() {
        return Err(refused(format!("{FORM}: it names no why=")));
    }
    let mut parts = vec!["spawn".to_owned()];
    parts.extend(args.iter().cloned());
    let head = spawn_wire_line(&parts)?;
    validate_request_line_len(&head, Some("spawn"))?;
    Ok(head)
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
                         [autopilot=<land|cruise>] [need=repo:<name>,free:<n>g] \
                         [cwd=<absolute folder there>|'cwd=~/<path>'] [label=<word>] - < task";

/// The node a `--host any` spawn (its words after `spawn`, `args`, and its `need=` words,
/// `need_words`) goes to ([`any_host`]): one survey of every member within the call's `--timeout`
/// (`total`) where that is shorter than a survey's own, the pick by its needs (and, for a
/// `cwd=~/…`, a home the member names) and its accounts, and a line on stderr naming it; with
/// where the board records the worker (that node, its host, and the account row it was chosen
/// by, masked). The spawn is sent there as `--host <node>` sends it, and the worker's later verbs
/// follow it there by its board row ([`board`]).
/// # Errors
/// `args` spawn no Claude Code or Codex worker, `need_words` name no need this knows, this client
/// links no fleet, the survey was refused, or no member can take the worker.
fn any_host_node(
    args: &[String],
    need_words: &[String],
    total: Option<Option<Duration>>,
    local: &LocalVerbs,
) -> io::Result<(String, board::Placed)> {
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
    let mut needs = any_host::needs_of(need_words).map_err(refused)?;
    let home_relative = args.iter().any(|arg| {
        arg.strip_prefix("cwd=")
            .map(|dir| dir.trim_matches('"'))
            .and_then(aterm_types::control_verbs::home_cwd)
            .is_some()
    });
    if home_relative {
        needs.push(any_host::Need::Home);
    }
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
    let chosen = any_host::pick(program, &needs, &members).map_err(|counts| {
        refused(any_host::none_able(
            program,
            &needs,
            &counts,
            read.alone.as_deref(),
        ))
    })?;
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
            any_host::why(program, &needs, &chosen)
        ),
    )?;
    let placed = board::Placed {
        node: Some(node.clone()),
        host: Some(member.host().to_owned()),
        account: (!chosen.row.label.is_empty())
            .then(|| aterm_codec::tty::one_line(&chosen.row.label, 32)),
    };
    Ok((node, placed))
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
    word.len() <= 64
        && word
            .strip_prefix("s-")
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
        .map(|(_, ms)| {
            (!ms.is_empty() && ms.bytes().all(|b| b.is_ascii_digit()))
                .then(|| ms.parse::<u64>().ok())
                .flatten()
                .ok_or(())
        })
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
/// `len=`, a `cwd=` only absolute or in that Mac's home (`~/<path>`, which it
/// resolves) — a folder of THIS Mac names nothing there, so none is added:
/// that Mac's aterm starts it in its own default — and gain `from=<starter>`
/// ([`worker_starter`]) when they name none.
fn spawn_head(args: &[String], starter: Option<&str>) -> io::Result<String> {
    let usage = |why: &str| {
        refused(format!(
            "spawn agent= over the fleet: {why} (aterm ctl --host <h> spawn agent=<word> \
             [model=<m>] [autopilot=<land|cruise>] [cwd=<absolute folder there>|'cwd=~/<path>'] \
             [label=<word>] - < task)"
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
        match fleet_cwd(dir) {
            Ok(true) => {}
            Ok(false) => {
                return Err(usage(
                    "cwd= names a folder of that Mac, by its absolute path or ~/<path> in its home",
                ));
            }
            Err(why) => return Err(usage(&format!("cwd={dir} {why}"))),
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

/// A worker's fleet spawn, its words judged and its task read ([`spawn_worker`]).
struct Worker<'a> {
    /// The Mac it is sent to: the one named, or the node `--host any` chose.
    host: String,
    /// Where the board records it runs, as far as `--host any`'s choice proved it.
    placed: board::Placed,
    /// Its words after `spawn`, its `need=` and `label=` taken off.
    args: &'a [String],
    /// Its `label=`.
    label: Option<String>,
    /// The session that starts it ([`worker_starter`]).
    starter: Option<&'a str>,
    /// Its request line, without its `len=` ([`spawn_head`]).
    head: &'a str,
    /// Its task.
    task: &'a [u8],
}

/// A worker's spawn over the drive `drive` dials, `within` its deadline: the exchange
/// ([`spawn_exchange`]); then, from what it answered, the sender's `kind=spawn` row in this Mac's
/// log ([`crate::fleet_telemetry::here_log`], [`crate::fleet_telemetry::spawned`]: `refused` where
/// the dial failed and nothing was sent) and the board's row of the worker the answer named; and only then the answer printed as
/// on one Mac ([`print_frame_answer`]). A print that fails (stdout closed:
/// `… spawn … - | head -0`) is the call's own error and sets its exit status alone: the worker
/// started all the same, and the row and the board say so.
fn spawn_worker(worker: Worker<'_>, within: Within, drive: FleetDriveFn) -> io::Result<ExitCode> {
    let Worker {
        host,
        mut placed,
        args,
        label,
        starter,
        head,
        task,
    } = worker;
    let began = Instant::now();
    let dialled = drive(&host, within.get()).map_err(io::Error::other);
    if let Ok(link) = &dialled {
        placed.node = placed.node.or_else(|| link.node.clone());
        placed.host = placed.host.or_else(|| Some(link.label.clone()));
    }
    placed.node = placed.node.or(Some(host));
    let exchanged = dialled
        .map(|link| spawn_exchange(link, &format!("{head} len={}", task.len()), task, within));
    let log = crate::fleet_telemetry::here_log();
    crate::fleet_telemetry::spawned(log, args, began, &exchanged, placed.node.as_deref());
    let (answer, sid) = exchanged??;
    if let Some(sid) = sid {
        board::record_spawned(&board::spawned(
            &sid,
            placed,
            label,
            (args, task),
            starter,
            board::now_ms(),
        ));
    }
    print_frame_answer(&answer)
}

/// A worker's spawn over `link`: the line `head`; then, once it is answered
/// `GO <n>`, the `task`, all `within` the connection's deadline. What it answered (a second `GO`
/// read past), for its caller to print as on one Mac once it has recorded the worker, and the sid
/// of that worker, where the answer to the task named one ([`spawned_sid_from_answer`]). Nothing
/// is printed here: a print that fails is no lost exchange.
fn spawn_exchange(
    link: DriveLink,
    head: &str,
    task: &[u8],
    within: Within,
) -> io::Result<(String, Option<String>)> {
    let DriveLink {
        mut stream, label, ..
    } = link;
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
        return Ok((answer, None));
    }
    let sent = reader.get_mut();
    sent.write_all(task)
        .and_then(|()| sent.flush())
        .map_err(cut)?;
    while answer == go {
        answer = read_frame_answer(&mut reader).map_err(cut)?;
    }
    let spawned_sid = spawned_sid_from_answer(&answer);
    Ok((answer, spawned_sid))
}

fn is_any_host(host: &str) -> bool {
    host.eq_ignore_ascii_case(any_host::ANY)
}

/// The sid a spawn's answer `answer` (`OK <sid> …`) names: `None` where it refused, or named none.
#[must_use]
pub fn spawned_sid_from_answer(answer: &str) -> Option<String> {
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

const MAX_ANY_HOST_LEN: usize = 255;

fn valid_any_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= MAX_ANY_HOST_LEN
        && host.trim() == host
        && !is_any_host(host)
        && !host.chars().any(char::is_control)
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

/// The Mac each of `sids` runs on, for `--host any @<sid> …` and `--host any await task @…`:
/// the node this Mac's board recorded when it started that worker ([`board`]), and for a sid the
/// board does not hold (another dispatcher's worker), the one member a fleet survey finds hosting
/// it. A survey's finding is not written down: the board holds only workers this Mac started.
/// # Errors
/// A sid the board places on this Mac (`--host any` drives other Macs), a survey this client
/// cannot run or was refused, or a sid no member, or more than one, hosts.
pub(crate) fn resolve_any_sid_hosts(
    sids: &[&str],
    total: Option<Option<Duration>>,
    survey_fn: Option<crate::FleetSurveyFn>,
) -> io::Result<Vec<String>> {
    let board = board::Board::here();
    let mut placed: std::collections::BTreeMap<String, Option<String>> =
        std::collections::BTreeMap::new();
    for &sid in sids {
        if let Some(row) = board.as_ref().and_then(|b| b.row(sid)) {
            placed.insert(sid.to_owned(), row.node.filter(|n| valid_any_host(n)));
        }
    }
    if let Some((sid, _)) = placed.iter().find(|(_, node)| node.is_none()) {
        return Err(refused(format!(
            "@{sid} runs on this Mac, by its board: drop `--host any` to reach it"
        )));
    }
    let mut discovered: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    if sids.iter().any(|sid| !placed.contains_key(*sid)) {
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
            let Some(node) = peer.node.as_deref().filter(|n| valid_any_host(n)) else {
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
                    if is_sid(clean) && !placed.contains_key(clean) {
                        let nodes = discovered.entry(clean.to_owned()).or_default();
                        if !nodes.iter().any(|existing| existing == node) {
                            nodes.push(node.to_owned());
                        }
                    }
                }
            }
        }
    }

    let mut out = Vec::with_capacity(sids.len());
    for &sid in sids {
        if let Some(Some(node)) = placed.get(sid) {
            out.push(node.clone());
            continue;
        }
        match discovered.get(sid).map(Vec::as_slice) {
            Some([node]) => out.push(node.clone()),
            Some(nodes) if nodes.len() > 1 => {
                return Err(io::Error::other(format!(
                    "--host any found @{sid} on multiple fleet peers ({})",
                    nodes.join(", ")
                )));
            }
            _ => {
                return Err(io::Error::other(format!(
                    "--host any found no live fleet peer hosting @{sid}"
                )));
            }
        }
    }
    Ok(out)
}

/// `<host>`'s board, read over a drive within `total` (the call's `--timeout`, else a drive's
/// own wait): `board` sent as `aterm ctl --host <host> board` sends it, a busy answer asked
/// again until the deadline, and the `OK <n>` reply parsed ([`board::parse_reply`]).
/// # Errors
/// The drive was refused, the host answered `ERR …` (an older aterm that serves no board, or a
/// member that does not let this Mac drive it), or the reply was cut or malformed.
pub(crate) fn read_board(
    host: &str,
    total: Option<Option<Duration>>,
    drive: FleetDriveFn,
) -> io::Result<board::Read> {
    let within = drive_within(total);
    let started = Instant::now();
    loop {
        let remaining = within.get().saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{host} remained busy until this call's deadline; its board was not read"),
            ));
        }
        let link = drive(host, remaining).map_err(io::Error::other)?;
        match board_exchange(link, within)? {
            Some(lines) => {
                return board::parse_reply(&lines).map_err(|why| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("{host}'s board: {why}"))
                });
            }
            None => std::thread::sleep(Duration::from_millis(50).min(remaining)),
        }
    }
}

/// `board` over `link` and its rows; `None` where the peer answered busy (nothing was read).
/// # Errors
/// The peer answered `ERR …`, cut the reply, or sent no `OK <n>` header.
fn board_exchange(link: DriveLink, within: Within) -> io::Result<Option<Vec<String>>> {
    const REQUEST: &str = "board\n";
    let DriveLink {
        mut stream, label, ..
    } = link;
    let cut = |e| closed(&label, REQUEST, within, e);
    stream
        .write_all(REQUEST.as_bytes())
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
        return Ok(None);
    }
    let Some(tail) = status.strip_prefix("OK ") else {
        return Err(io::Error::other(format!("{label}: {status}")));
    };
    let count = stream_count(tail).ok_or_else(|| malformed_header_error(status))?;
    let mut rows = collect_reply_lines(count, MAX_BODY_BYTES, |line| {
        read_bounded_line(&mut reader, line)
    })
    .map_err(cut)?;
    for row in &mut rows {
        row.truncate(row.trim_end_matches(['\r', '\n']).len());
    }
    Ok(Some(rows))
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
                    "{label} did not answer within {} (its aterm is stopped, frozen or too \
                     busy to answer, or the network between is down)",
                    secs(within)
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
                            "a longer --timeout, or 0, waits up to the drive's own {}",
                            secs(own)
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
                        "no answer came from {label} within this call's --timeout of {} \
                         ({turn}{longer})",
                        secs(within)
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
    let DriveLink {
        mut stream, label, ..
    } = link;
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
