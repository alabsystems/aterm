// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reconnecting `subscribe` streams and blocking reads across aterm self-updates.
//!
//! On unexpected hang-up or [`aterm_types::control_verbs::host_gone_line`], checks whether the
//! server still answers ([`serving_pid_within`], [`ListenerId`]) or redials the successor within
//! [`SUCCESSOR_BOUND`] via [`Follow`] ([`Follow::Host`], [`after_refusal`]), exiting with
//! [`EXIT_REPLACED`] if none answers.

use std::io::{self, BufReader, Read as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aterm_uds::CtlStream;
use aterm_uds::exitwatch::ExitMoment;
use aterm_uds::process::BootClock;

/// The exit code of an exchange cut by its server going away with no successor answering.
pub const EXIT_REPLACED: u8 = 75;

/// How long a successor may take to answer.
pub(crate) const SUCCESSOR_BOUND: Duration = Duration::from_secs(30);

/// Between two looks for a successor.
const SUCCESSOR_POLL: Duration = Duration::from_millis(100);

/// Where a successor of the server this call reached may answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Follow {
    /// A socket path the call pinned (`--sock`).
    Path(String),
    /// Nothing pinned: with a session to follow.
    Flagless {
        /// The rendezvous dir the call resolved.
        dir: PathBuf,
        /// The session whose host the call reached.
        sid: Option<String>,
    },
    /// Behind a forward: the server this call dialled only relayed the request to the aterm.
    Host {
        /// The socket the gone host was reached.
        sock: String,
        /// The session the request names (`@s-…`).
        sid: Option<String>,
    },
    /// No successor follows, for the reason given.
    Nothing(Unfollowed),
}

/// Why a call follows no successor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unfollowed {
    /// `--pid <n>` names one process.
    PidPin(u32),
    /// The redialled leg of a blocking read.
    AskedOnce,
}

/// The redial policy of one call.
#[derive(Clone, Debug)]
pub(crate) struct Redial {
    follow: Follow,
    /// The start record of the process a `--pid` pin names ([`process_started`]).
    pinned: Option<Stamp>,
    bound: Duration,
    /// When a process started.
    started: fn(u32) -> Option<Stamp>,
    /// The clock a hang-up is read.
    clock: fn() -> Stamp,
    /// Told the pid of each server whose exit this relay watches.
    #[cfg(test)]
    on_watch: Option<fn(u32)>,
}

/// What a hang-up turned out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Hangup {
    /// The server we dialled still answers.
    StillServing,
    /// A live server answers where this call follows, and it is not the one the stream.
    Successor { path: String, pid: u32 },
    /// Nothing answered within `waited`.
    Gone { waited: Duration, by_deadline: bool },
    /// This platform cannot read a peer pid, so a replacement cannot be told from a deliberate end.
    Unknown,
}

impl Redial {
    /// The policy for a call that follows `follow`.
    pub(crate) fn new(follow: Follow) -> Self {
        let pinned = match &follow {
            Follow::Nothing(Unfollowed::PidPin(pid)) => process_started(*pid),
            _ => None,
        };
        Self {
            follow,
            pinned,
            bound: SUCCESSOR_BOUND,
            started: process_started,
            clock: Stamp::now,
            #[cfg(test)]
            on_watch: None,
        }
    }

    /// A policy that never follows.
    pub(crate) fn none() -> Self {
        Self::new(Follow::Nothing(Unfollowed::AskedOnce))
    }

    /// Why this policy follows no successor, when it follows none.
    pub(crate) fn unfollowed(&self) -> Option<&Unfollowed> {
        match &self.follow {
            Follow::Nothing(why) => Some(why),
            _ => None,
        }
    }

    /// The policy for what follows a forward's host that went away at `sock` ([`Follow::Host`]).
    pub(crate) fn host(&self, sock: &str, sid: Option<&str>) -> Self {
        match &self.follow {
            Follow::Nothing(Unfollowed::PidPin(_) | Unfollowed::AskedOnce) => self.clone(),
            _ => Self {
                follow: Follow::Host {
                    sock: sock.to_string(),
                    sid: sid.map(str::to_string),
                },
                pinned: None,
                ..self.clone()
            },
        }
    }

    /// The same policy with a different successor bound (tests).
    #[cfg(all(test, unix))]
    pub(crate) fn with_bound(mut self, bound: Duration) -> Self {
        self.bound = bound;
        self
    }

    /// The same policy reading a successor's start from `started`.
    #[cfg(all(test, unix))]
    pub(crate) fn with_started(mut self, started: fn(u32) -> Option<Stamp>) -> Self {
        self.started = started;
        self
    }

    /// The same policy reading every moment it stamps from `clock`.
    #[cfg(all(test, unix))]
    pub(crate) fn with_clock(mut self, clock: fn() -> Stamp) -> Self {
        self.clock = clock;
        self
    }

    /// The same policy telling `note` the pid of each server whose exit it watches.
    #[cfg(all(test, unix))]
    pub(crate) fn with_watch_note(mut self, note: fn(u32)) -> Self {
        self.on_watch = Some(note);
        self
    }

    /// Stamp the exit of the live server `pid` when the kernel reports it
    fn watch_exit(&self, pid: Option<u32>) -> Option<ExitMoment<Stamp>> {
        let pid = pid?;
        let watch = ExitMoment::watch(pid, self.clock)?;
        #[cfg(test)]
        if let Some(note) = self.on_watch {
            note(pid);
        }
        Some(watch)
    }

    /// The [`Arrival`] of the successor `pid` answering at `path`.
    pub(crate) fn arrival(&self, pid: u32, path: &str, hung_up: Stamp) -> Arrival {
        match &self.follow {
            Follow::Path(fixed) if fixed == path && only_a_relaunch_binds(path) => {
                arrival_of((self.started)(pid), hung_up)
            }
            _ => Arrival::Shared,
        }
    }

    /// Every path a successor of the server reached at `dialed` may answer on,
    fn candidates(&self, dialed: &str) -> Vec<String> {
        match &self.follow {
            Follow::Path(path) => vec![dialed.to_string(), path.clone()],
            Follow::Flagless {
                dir,
                sid: Some(sid),
            } => super::self_instance_sock_in(dir, Some(sid))
                .into_iter()
                .collect(),
            Follow::Flagless { dir, sid: None } => vec![
                dialed.to_string(),
                dir.join(super::SOCK_FILE).to_string_lossy().into_owned(),
            ],
            Follow::Host { sock, sid } => {
                let republished = std::path::Path::new(sock)
                    .parent()
                    .and_then(|dir| super::self_instance_sock_in(dir, sid.as_deref()));
                std::iter::once(sock.clone())
                    .chain(republished.filter(|entry| entry != sock))
                    .collect()
            }
            Follow::Nothing(Unfollowed::PidPin(_)) => vec![dialed.to_string()],
            Follow::Nothing(_) => Vec::new(),
        }
    }

    /// Classify a hang-up from the server `old` reached at `dialed` through `listener`.
    pub(crate) fn after_hangup(
        &self,
        dialed: &str,
        old: Option<u32>,
        listener: Option<ListenerId>,
        until: Option<Instant>,
    ) -> Hangup {
        let Some(old) = old else {
            return Hangup::Unknown;
        };
        let wait = Wait::from_now(self.bound, until);
        if wait
            .probe(dialed)
            .is_some_and(|pid| is_the_same_server(pid, dialed, old, listener))
        {
            return Hangup::StillServing;
        }
        self.successor(dialed, old, listener, &wait, true)
    }

    /// Classify a hang-up a forward said.
    pub(crate) fn after_host_gone(&self, sock: &str, old: u32, until: Option<Instant>) -> Hangup {
        self.successor(sock, old, None, &Wait::from_now(self.bound, until), false)
    }

    /// The successor wait of [`Redial::after_hangup`] and
    fn successor(
        &self,
        dialed: &str,
        old: u32,
        listener: Option<ListenerId>,
        wait: &Wait,
        exclude_old: bool,
    ) -> Hangup {
        let pin = match (&self.follow, self.pinned) {
            (Follow::Nothing(Unfollowed::PidPin(pin)), Some(born)) if listener.is_some() => {
                Some((*pin, born))
            }
            (Follow::Nothing(_), _) => {
                return Hangup::Gone {
                    waited: Duration::ZERO,
                    by_deadline: false,
                };
            }
            _ => None,
        };
        let still_pinned = |pid: u32, born: Stamp| process_started(pid) == Some(born);
        loop {
            // The pinned process has ended: it went away, and no wait ran out.
            if let Some((pin, born)) = pin
                && !still_pinned(pin, born)
            {
                return Hangup::Gone {
                    waited: Duration::ZERO,
                    by_deadline: false,
                };
            }
            for path in self.candidates(dialed) {
                if let Some(pid) = wait.probe(&path)
                    && !(exclude_old && is_the_same_server(pid, &path, old, listener))
                    && pin.is_none_or(|(pin, born)| pid == pin && still_pinned(pid, born))
                {
                    return Hangup::Successor { path, pid };
                }
            }
            let now = Instant::now();
            if now >= wait.deadline {
                return Hangup::Gone {
                    waited: now.duration_since(wait.started),
                    by_deadline: wait.by_deadline,
                };
            }
            std::thread::sleep(SUCCESSOR_POLL.min(wait.deadline - now));
        }
    }
}

/// One wait for a successor: from when it began, to the bound or the caller's
struct Wait {
    started: Instant,
    deadline: Instant,
    /// The caller's deadline, not the bound, ends it.
    by_deadline: bool,
}

impl Wait {
    fn from_now(bound: Duration, until: Option<Instant>) -> Self {
        let started = Instant::now();
        let bound = started + bound;
        Self {
            started,
            deadline: until.map_or(bound, |until| until.min(bound)),
            by_deadline: until.is_some_and(|until| until < bound),
        }
    }

    /// The pid answering at `path`, waiting no longer than the wait has left
    fn probe(&self, path: &str) -> Option<u32> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        serving_pid_within(path, left.clamp(PROBE_FLOOR, PROBE_TIMEOUT))
    }
}

/// How long a probe waits for `OK` to its `version` — less when the caller's
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The shortest probe: a live aterm answers `version` in well under this.
const PROBE_FLOOR: Duration = Duration::from_millis(100);

/// [`serving_pid_within`] with the full probe wait (tests).
#[cfg(all(test, unix))]
pub(crate) fn serving_pid(path: &str) -> Option<u32> {
    serving_pid_within(path, PROBE_TIMEOUT)
}

/// The pid of the process answering at `path` right now (it accepted a
fn serving_pid_within(path: &str, wait: Duration) -> Option<u32> {
    let (pid, line) = version_at(path, wait)?;
    super::is_ok_reply(&line).then_some(pid?)
}

/// What answered `version` at `path` within `wait`: the peer's pid, where it
fn version_at(path: &str, wait: Duration) -> Option<(Option<u32>, String)> {
    let (stream, token) = super::connect_then_read(path, super::read_token_for).ok()?;
    let pid = server_pid(&stream);
    let _ = stream.set_read_timeout(Some(wait));
    let _ = stream.set_write_timeout(Some(wait));
    let mut reader = BufReader::new(&stream);
    match ask_once(&stream, &mut reader, token.as_deref(), "version\n") {
        Answer::Line(line) if !line.is_empty() => Some((pid, line)),
        Answer::Line(_) | Answer::Gone => None,
    }
}

/// Whether the server `pid` still answers at `path` through `listener`, with
fn answers_at(path: &str, pid: u32, listener: Option<ListenerId>) -> bool {
    let Some((answered, line)) = version_at(path, PROBE_TIMEOUT) else {
        return false;
    };
    (super::is_ok_reply(&line) || is_busy(&line))
        && match answered {
            Some(answered) => is_the_same_server(answered, path, pid, listener),
            None => listener.is_some_and(|then| listener_at(path) == Some(then)),
        }
}

/// The pid of the server at the other end of `stream`.
pub(crate) fn server_pid(stream: &CtlStream) -> Option<u32> {
    aterm_uds::fdpass::peer_pid(stream).ok()
}

/// Which listening socket a path names, since a pid alone does not name a server.
pub(crate) use aterm_uds::listener::{ListenerId, listener_at};

/// Whether `pid`, answering at `path`, is the server `old` a stream reached
fn is_the_same_server(pid: u32, path: &str, old: u32, listener: Option<ListenerId>) -> bool {
    pid == old && aterm_uds::listener::still_names(path, listener)
}

/// The one session the request `line` names by id, read by the wire's one grammar.
#[must_use]
pub fn target_sid(line: &str) -> Option<aterm_types::control_request::Sid<'_>> {
    use aterm_types::control_request::{self, Selector};
    let head = control_request::head(line.trim_end_matches(['\r', '\n']));
    match head.selector {
        Some(Selector::Sid(sid)) => Some(sid),
        None if head.verb == "subscribe" => {
            control_request::subscribe_target(head.rest).map(|(sid, _)| sid)
        }
        _ => None,
    }
}

/// A moment as this machine's clocks read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    /// The kernel's boot clock ([`BootClock`]).
    pub(crate) boot: Option<BootClock>,
    /// The wall clock: the basis only where one side has no boot reading.
    pub(crate) wall: Option<SystemTime>,
}

impl Stamp {
    /// Now, on both clocks.
    pub(crate) fn now() -> Self {
        Self {
            boot: aterm_uds::process::boot_clock_now(),
            wall: Some(SystemTime::now()),
        }
    }
}

/// Whether `a` is strictly after `b`.
pub(crate) fn later(a: Stamp, b: Stamp) -> Option<bool> {
    match (a.boot, b.boot) {
        (Some(a), Some(b)) => Some(a > b),
        _ => Some(a.wall? > b.wall?),
    }
}

/// When the process `pid` started.
pub(crate) fn process_started(pid: u32) -> Option<Stamp> {
    let pid = i32::try_from(pid).ok()?;
    let wall = |birth: aterm_uds::process::ProcessBirth| {
        UNIX_EPOCH
            .checked_add(Duration::from_secs(birth.seconds))?
            .checked_add(Duration::from_micros(birth.microseconds))
    };
    if let Some(start) = aterm_uds::process::read_process_start(pid) {
        return Some(Stamp {
            boot: Some(start.boot),
            wall: start.wall.and_then(wall),
        });
    }
    let birth = aterm_uds::process::read_process_birth(pid)?;
    Some(Stamp {
        boot: None,
        wall: Some(wall(birth)?),
    })
}

/// Whether the socket at `path` is one only a relaunch of the instance
fn only_a_relaunch_binds(path: &str) -> bool {
    let path = std::path::Path::new(path);
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name != super::SOCK_FILE
        && aterm_types::control_socket::instance_pid(name).is_none()
        && !std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
        && aterm_uds::latest::target_name(path).is_none()
}

/// Whether an I/O error means the peer went away (as opposed to a deadline or a local fault).
pub(crate) fn is_hangup(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
    )
}

/// Whether a request is retried once on a successor after a hang-up before its reply.
pub(crate) fn retries_across_replacement(request_parts: &[String]) -> bool {
    let rest = match request_parts.first() {
        Some(sel) if is_local_selector(sel) => return false,
        Some(sel) if sel.starts_with('@') => &request_parts[1..],
        _ => request_parts,
    };
    let args: Vec<&str> = rest.iter().skip(1).map(String::as_str).collect();
    match rest.first().map(String::as_str) {
        Some("text" | "wait" | "ready") => true,
        Some("inbox") => inbox_plain_read(&args),
        Some("await") => match args.first() {
            Some(&("seq" | "block" | "consent")) => false,
            Some(&("inbox" | "agent")) => !args.iter().any(|a| a.starts_with("since=")),
            Some(&"edit") => !args.iter().any(|a| a.starts_with("ticket=")),
            Some(&"log") => log_wait_resumes(&args),
            Some(_) => true,
            None => false,
        },
        _ => false,
    }
}

/// Whether an `await log` (its arguments, `log` first) names where it reads
fn log_wait_resumes(args: &[&str]) -> bool {
    args.iter()
        .skip(1)
        .any(|a| a.starts_with("since=") || a.split(',').all(|w| w == "need"))
}

/// Whether an `inbox` request is a plain read of what is queued now: a
fn inbox_plain_read(args: &[&str]) -> bool {
    args.iter()
        .all(|a| matches!(*a, "--peek" | "--meta") || a.parse::<u64>().is_ok())
}

/// The part of a blocking read that names something in the old process.
pub(crate) fn old_process_anchor(request_parts: &[String]) -> Option<String> {
    let rest = match request_parts.first() {
        Some(sel) if is_local_selector(sel) => return Some(sel.clone()),
        Some(sel) if sel.starts_with('@') => &request_parts[1..],
        _ => request_parts,
    };
    let args: Vec<&str> = rest.iter().skip(1).map(String::as_str).collect();
    match rest.first().map(String::as_str) {
        Some("inbox") if !inbox_plain_read(&args) => Some(rest.join(" ")),
        Some("await") => match args.first() {
            Some(&"seq") if args.len() >= 2 && !args[1].contains('=') => {
                Some(format!("seq {}", args[1]))
            }
            Some(&("inbox" | "agent")) => args
                .iter()
                .find(|a| a.starts_with("since="))
                .map(|a| (*a).to_string()),
            Some(&"edit") => args
                .iter()
                .find(|a| a.starts_with("ticket="))
                .map(|a| (*a).to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// The first `subscribe` target named by local number (`@<n>`) that the
fn untied_local_target(request: &str, sids: &[(String, String)]) -> Option<String> {
    request
        .split_whitespace()
        .filter(|token| token.starts_with('@'))
        .flat_map(|token| token.split(','))
        .find(|piece| {
            is_local_selector(piece) && !sids.iter().any(|(local, _)| *local == piece[1..])
        })
        .map(str::to_string)
}

/// The verb and its arguments, past a leading `@<selector>`.
fn verb_and_args<S: AsRef<str>>(request_parts: &[S]) -> (Option<&str>, Vec<&str>) {
    let rest = match request_parts.first() {
        Some(sel) if sel.as_ref().starts_with('@') => &request_parts[1..],
        _ => request_parts,
    };
    (
        rest.first().map(AsRef::as_ref),
        rest.iter().skip(1).map(AsRef::as_ref).collect(),
    )
}

/// The wait a request is when it waits for a change after the moment it was
fn armed_wait<S: AsRef<str>>(request_parts: &[S]) -> Option<&'static str> {
    let (Some("await"), args) = verb_and_args(request_parts) else {
        return None;
    };
    match args.first().copied() {
        Some("block") => Some("await block"),
        Some("consent") => Some("await consent"),
        Some("seq") if args.get(1).is_none_or(|a| a.contains('=')) => Some("await seq"),
        Some("log") if !log_wait_resumes(&args) => Some("await log"),
        _ => None,
    }
}

/// What a request cut by a replacement and not asked again ([`retries_across_replacement`]) tells.
pub(crate) fn not_asked_again_note(request_parts: &[String]) -> String {
    match (verb_and_args(request_parts).0, armed_wait(request_parts)) {
        (Some("turn"), _) => {
            "a `turn` TYPES: its text may already have been typed, and submitted, in the \
             session — read the screen (`text`) before sending it again; never retry it blind"
                .to_owned()
        }
        (Some("post"), _) => {
            "the `post` may already be queued or sent (an update hands a queued post to the \
             successor; a crash loses it, and a session the PTY keeper recovers retires it \
             `reason=not-carried`) — never post it again blind: re-post it with the same \
             `key=`, or look for it first (the `post`/`post-landed` rows of `timeline`)"
                .to_owned()
        }
        (_, Some("await seq")) => {
            "a bare `await seq` waits for the next change after it was armed, and the update \
             may have made it — read the session (`status`, `text`) before waiting again"
                .to_owned()
        }
        (_, Some("await log")) => {
            "an `await log` with no `since=` waits from the moment it was armed, and the record \
             it waits for may have been written during the update — read `status` (its \
             `log_seq=` and `log_ep=`) or `log`, then wait again with `since=<log_seq> \
             ep=<log_ep>`"
                .to_owned()
        }
        (_, Some(_)) => {
            "`await block` and `await consent` wait for a change after they were armed, and it \
             may have happened during the update — read the session (`status`, `text`) before \
             waiting again"
                .to_owned()
        }
        _ if old_process_anchor(request_parts).is_some_and(|a| a.starts_with("ticket=")) => {
            "an edit's `ticket=` names a ticket of the old process — wait again by the file: \
             `await edit uri=<file-uri>`"
                .to_owned()
        }
        _ => match old_process_anchor(request_parts) {
            Some(anchor) => {
                format!("`{anchor}` names a position in the old process — re-issue it")
            }
            None => "re-issue the request".to_owned(),
        },
    }
}

/// [`gone_note`] for the request it cut, and for a `turn` or a `post`.
pub(crate) fn gone_request_note(gone: String, request_parts: &[String]) -> String {
    if !matches!(verb_and_args(request_parts).0, Some("turn" | "post")) {
        return gone;
    }
    let exit = format!(" (exit {EXIT_REPLACED})");
    match gone.strip_suffix(&exit) {
        Some(head) => format!("{head}; {}{exit}", not_asked_again_note(request_parts)),
        None => gone,
    }
}

/// Whether a hang-up before the reply is reported as a replacement.
pub(crate) fn reports_replacement(request_parts: &[String]) -> bool {
    is_blocking_read(request_parts)
        || matches!(verb_and_args(request_parts).0, Some("turn" | "post"))
}

/// Whether the request is a blocking read at all.
pub(crate) fn is_blocking_read(request_parts: &[String]) -> bool {
    let rest = match request_parts.first() {
        Some(sel) if sel.starts_with('@') => &request_parts[1..],
        _ => request_parts,
    };
    matches!(
        rest.first().map(String::as_str),
        Some("text" | "wait" | "ready" | "await" | "inbox")
    )
}

/// Whether `sel` names a session by the process-local number the server gave it.
pub(crate) fn is_local_selector(sel: &str) -> bool {
    sel.strip_prefix('@')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The request a redialled `subscribe` sends.
pub(crate) fn resume_request(
    request: &str,
    sids: &[(String, String)],
    seen: &EventAnchors,
) -> Option<String> {
    resume_request_with(request, sids, seen, true)
}

/// The same request with its self target as the client wrote it and no anchors.
pub(crate) fn unpinned_resume_request(
    request: &str,
    sids: &[(String, String)],
    seen: &EventAnchors,
) -> Option<String> {
    let pinned = resume_request_with(request, sids, seen, true)?;
    let unpinned = resume_request_with(request, sids, seen, false)?;
    (unpinned != pinned).then_some(unpinned)
}

/// What one resume leg may ask ([`relay_subscription`]).
pub(crate) fn resume_lines(
    original: &str,
    request: &str,
    sids: &[(String, String)],
    seen: &EventAnchors,
) -> Option<(String, Option<String>)> {
    let resumed = resume_request(request, sids, seen)?;
    let fallback = unpinned_resume_request(original, sids, seen).filter(|own| *own != resumed);
    Some((resumed, fallback))
}

fn resume_request_with(
    request: &str,
    sids: &[(String, String)],
    seen: &EventAnchors,
    pin: bool,
) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    let mut block = seen.block;
    let mut turn = seen.turn;
    let (mut log_seq, mut log_ep) = (None, None);
    let anchor = |v: &str| v.parse::<u64>().ok();
    for token in request.split_whitespace() {
        if token.starts_with("since=") {
            continue;
        }
        if let Some(v) = token.strip_prefix("since-block=") {
            block = block.max(anchor(v));
            continue;
        }
        if let Some(v) = token.strip_prefix("since-turn=") {
            turn = turn.max(anchor(v));
            continue;
        }
        if let Some(v) = token.strip_prefix("since-log=") {
            log_seq = anchor(v);
            continue;
        }
        if let Some(v) = token.strip_prefix("log-ep=") {
            log_ep = v.parse::<u32>().ok();
            continue;
        }
        if !token.starts_with('@') {
            out.push(token.to_string());
            continue;
        }
        let mut pieces = Vec::new();
        for piece in token.split(',') {
            if is_local_selector(piece) {
                let (_, sid) = sids.iter().find(|(local, _)| *local == piece[1..])?;
                pieces.push(format!("@{sid}"));
            } else {
                pieces.push(piece.to_string());
            }
        }
        out.push(pieces.join(","));
    }
    let selectors: Vec<&String> = out.iter().filter(|t| t.starts_with('@')).collect();
    let single = match selectors.as_slice() {
        [] => true,
        [one] => !one.contains(',') && one.as_str() != "@*",
        _ => false,
    };
    let self_target = selectors.first().is_none_or(|s| is_self_selector(s));
    let pinned = single && (!self_target || (pin && pin_self(&mut out, sids)));
    if pinned {
        if let Some(id) = block {
            out.push(format!("since-block={id}"));
        }
        if let Some(id) = turn {
            out.push(format!("since-turn={id}"));
        }
        if let (Some(seq), Some(ep)) = (seen.log_seq.or(log_seq), seen.log_ep.or(log_ep)) {
            out.push(format!("since-log={seq} log-ep={ep}"));
        }
    }
    let mut line = out.join(" ");
    line.push('\n');
    Some(line)
}

/// Rewrite a self target in `out` to `@<sid>` of the session the stream was
fn pin_self(out: &mut Vec<String>, sids: &[(String, String)]) -> bool {
    let Some(sid) = self_sid(sids) else {
        return false;
    };
    let at = format!("@{sid}");
    if let Some(slot) = out.iter_mut().find(|t| is_self_selector(t)) {
        *slot = at;
        return true;
    }
    match out.iter().position(|t| t == "subscribe") {
        Some(verb) => {
            out.insert(verb + 1, at);
            true
        }
        None => false,
    }
}

/// Whether a selector token names the connection's own session (`@.`), which
fn is_self_selector(token: &str) -> bool {
    token == "@."
}

/// The one session a single-target stream's `sub` lines tied it to, or `None`
fn self_sid(sids: &[(String, String)]) -> Option<&str> {
    let (_, first) = sids.first()?;
    sids.iter()
        .all(|(_, sid)| sid == first)
        .then_some(first.as_str())
}

/// The newest `block-complete` and `turn` ids a relayed `events` stream has shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EventAnchors {
    pub(crate) block: Option<u64>,
    pub(crate) turn: Option<u64>,
    /// The `log` stream's cursor.
    pub(crate) log_seq: Option<u64>,
    /// … and the epoch that seq is read in (the `sub` line's `log_ep=`, a `GAP`'s `ep=`).
    pub(crate) log_ep: Option<u32>,
}

/// Whether the subscription's target set is the live `@*` roster, which never
fn targets_everything(request: &str) -> bool {
    request.split_whitespace().any(|t| t == "@*")
}

/// Whether `request` names every session it follows by id, as a pinned resume
fn names_its_sessions(request: &str) -> bool {
    let mut pieces = request
        .split_whitespace()
        .filter(|token| token.starts_with('@'))
        .flat_map(|token| token.split(','))
        .peekable();
    pieces.peek().is_some() && pieces.all(|piece| piece != "@*" && !is_self_selector(piece))
}

/// Follows the push stream's framing just far enough to know when every subscribed session.
#[derive(Debug, Default)]
pub(crate) struct FrameTracker {
    line: Vec<u8>,
    skip_lines: u64,
    skip_bytes: u64,
    /// `(local, sid, exited)` per `sub` line seen on the current leg.
    targets: Vec<(String, String, bool)>,
    everything: bool,
    lost: bool,
    /// The newest block and turn ids relayed, kept across legs.
    anchors: EventAnchors,
    /// The host a forward said went away under this leg.
    gone: Option<(u32, String)>,
    /// The instance said `EVENT * bye quit` on this leg.
    quit: bool,
}

/// The longest header line the tracker keeps; a longer one is not a frame
const MAX_HEADER: usize = 4096;

impl FrameTracker {
    /// A tracker for the stream `request` opened.
    pub(crate) fn new(request: &str) -> Self {
        Self {
            everything: targets_everything(request),
            ..Self::default()
        }
    }

    /// A redialled leg starts a new stream.
    pub(crate) fn start_leg(&mut self, session: LegSession) {
        let everything = self.everything;
        let anchors = match session {
            LegSession::Same => self.anchors,
            LegSession::Other => EventAnchors::default(),
        };
        *self = Self {
            everything,
            anchors,
            ..Self::default()
        };
    }

    /// The newest `block-complete` and `turn` ids relayed so far, every leg.
    pub(crate) fn anchors(&self) -> EventAnchors {
        self.anchors
    }

    /// Whether the instance said on this leg that it is quitting.
    pub(crate) fn said_quit(&self) -> bool {
        self.quit && !self.lost
    }

    /// Whether the stream said every one of its sessions exited.
    pub(crate) fn session_over(&self) -> bool {
        !self.lost
            && !self.everything
            && !self.targets.is_empty()
            && self.targets.iter().all(|(_, _, exited)| *exited)
    }

    /// `(local, sid)` per `sub` line of the current leg.
    pub(crate) fn sids(&self) -> Vec<(String, String)> {
        self.targets
            .iter()
            .map(|(local, sid, _)| (local.clone(), sid.clone()))
            .collect()
    }

    /// Account for `bytes` of the stream, relaying nothing (tests).
    #[cfg(test)]
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        self.relay(bytes, &mut io::sink())
            .expect("a sink takes every write");
    }

    /// Account for `bytes` of the stream and write them on to `out`.
    pub(crate) fn relay(&mut self, mut bytes: &[u8], out: &mut impl io::Write) -> io::Result<()> {
        while !bytes.is_empty() {
            if self.lost {
                return out.write_all(bytes);
            }
            if self.skip_bytes > 0 {
                let n = usize::try_from(self.skip_bytes)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                out.write_all(&bytes[..n])?;
                self.skip_bytes -= n as u64;
                bytes = &bytes[n..];
                continue;
            }
            let Some(nl) = bytes.iter().position(|&b| b == b'\n') else {
                self.line.extend_from_slice(bytes);
                if self.line.len() > MAX_HEADER {
                    self.lost = true;
                    out.write_all(&std::mem::take(&mut self.line))?;
                }
                return Ok(());
            };
            self.line.extend_from_slice(&bytes[..=nl]);
            bytes = &bytes[nl + 1..];
            let line = std::mem::take(&mut self.line);
            if self.skip_lines > 0 {
                self.skip_lines -= 1;
                out.write_all(&line)?;
                continue;
            }
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            if let Some((pid, sock)) = aterm_types::control_verbs::host_gone(&text) {
                self.gone = Some((pid, sock.to_string()));
                continue;
            }
            out.write_all(&line)?;
            self.header(&text);
        }
        Ok(())
    }

    /// At a leg's end: write on the part of a line the stream ended inside, as it came.
    pub(crate) fn finish(&mut self, out: &mut impl io::Write) -> io::Result<()> {
        out.write_all(&std::mem::take(&mut self.line))
    }

    /// The host a forward said went away under this leg.
    pub(crate) fn take_host_gone(&mut self) -> Option<(u32, String)> {
        self.gone.take()
    }

    fn header(&mut self, line: &str) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let value = |key: &str| {
            tokens
                .iter()
                .find_map(|t| t.strip_prefix(key)?.strip_prefix('='))
        };
        match tokens.as_slice() {
            ["sub", local, sid, ..] => {
                self.targets
                    .push(((*local).to_string(), (*sid).to_string(), false));
                if let (Some(seq), Some(ep)) = (
                    value("log_seq").and_then(|v| v.parse().ok()),
                    value("log_ep").and_then(|v| v.parse().ok()),
                ) {
                    self.anchors.log_seq = Some(seq);
                    self.anchors.log_ep = Some(ep);
                }
            }
            ["LOG", _, seq, ..] => {
                if let Ok(seq) = seq.parse::<u64>() {
                    self.anchors.log_seq = Some(seq);
                }
            }
            ["GAP", _, lost, ..] if lost.starts_with("log-lost=") => {
                if let (Some(seq), Some(ep)) = (
                    value("resync_to").and_then(|v| v.parse().ok()),
                    value("ep").and_then(|v| v.parse().ok()),
                ) {
                    self.anchors.log_seq = Some(seq);
                    self.anchors.log_ep = Some(ep);
                }
            }
            ["EVENT", "*", "bye", "quit"] => self.quit = true,
            ["EVENT", local, "exited", ..] => {
                for (known, _, exited) in &mut self.targets {
                    if known == local {
                        *exited = true;
                    }
                }
            }
            ["EVENT", _, "block-complete", id, ..] => {
                if let Ok(id) = id.parse::<u64>() {
                    self.anchors.block = self.anchors.block.max(Some(id));
                }
            }
            ["EVENT", _, "turn", id, ..] => {
                if let Ok(id) = id.parse::<u64>() {
                    self.anchors.turn = self.anchors.turn.max(Some(id));
                }
            }
            ["DELTA", _, _, "screen", rows, ..] => match rows.parse::<u64>() {
                Ok(rows) => self.skip_lines = rows,
                Err(_) => self.lost = true,
            },
            ["DELTA", _, _, "cells", len, ..] | ["BYTES", _, len, ..] => match len.parse::<u64>() {
                Ok(len) => self.skip_bytes = len.saturating_add(1),
                Err(_) => self.lost = true,
            },
            _ => {}
        }
    }
}

/// The one refusal a pinned self stream is asked again after: the successor
const NO_SUCH_SESSION: &str = "ERR no such session";

/// Which session a redialled leg relays, against the one the stream followed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LegSession {
    /// The same one: the successor carried it (an update), so the event anchors go.
    Same,
    /// A fresh successor's own session ([`after_refusal`]).
    Other,
}

/// One request put to a successor, and the session its leg relays when the successor accepts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ask {
    pub(crate) request: String,
    pub(crate) session: LegSession,
}

/// What a successor lacking the stream's session is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arrival {
    /// On the fixed socket the call named, started after the hang-up.
    Fresh,
    /// On the fixed socket and already running when the old server went away ([`HangupAt::moment`])
    Predates,
    /// On the fixed socket.
    Unknown,
    /// Not on a fixed `--sock` socket ([`only_a_relaunch_binds`]).
    Shared,
}

/// The [`Arrival`] of a successor on the fixed socket that started at `started`.
pub(crate) fn arrival_of(started: Option<Stamp>, hung_up: Stamp) -> Arrival {
    match started.and_then(|at| later(at, hung_up)) {
        Some(true) => Arrival::Fresh,
        Some(false) => Arrival::Predates,
        None => Arrival::Unknown,
    }
}

/// When the server a leg was on went away, against which a successor's start is ordered.
pub(crate) struct HangupAt {
    /// When this relay read the hang-up (an EOF, a reset, a failed send).
    pub(crate) seen: Stamp,
    /// The server's exit, stamped as the kernel reported it.
    pub(crate) exit: Option<ExitMoment<Stamp>>,
}

impl HangupAt {
    /// The earlier of [`HangupAt::seen`] and the stamped exit.
    pub(crate) fn moment(&self) -> Stamp {
        earliest(self.seen, self.exit.as_ref().and_then(ExitMoment::get))
    }
}

/// [`HangupAt::moment`], of its two readings.
pub(crate) fn earliest(seen: Stamp, exited: Option<Stamp>) -> Stamp {
    match exited {
        Some(exited) if later(seen, exited) == Some(true) => exited,
        _ => seen,
    }
}

/// Whether a status line is the control socket's busy refusal
fn is_busy(status: &str) -> bool {
    status.starts_with("ERR control server busy")
}

/// Why a successor's refusal ends the relay with [`EXIT_REPLACED`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unresumed {
    /// Nothing else to ask: a refusal other than [`NO_SUCH_SESSION`].
    Refused,
    /// The successor answered busy.
    Busy,
    /// The successor lacks the self stream's session and did not start after the old server's end.
    Predates,
    /// The successor lacks the self stream's session.
    Unknown,
    /// The successor lacks the self stream's session and answers where any aterm.
    Shared,
}

/// What a relay does when a successor refused `asked` with `status`.
pub(crate) fn after_refusal(
    asked: &Ask,
    status: &str,
    fallback: Option<String>,
    arrival: Arrival,
) -> Result<Ask, Unresumed> {
    if is_busy(status) {
        return Err(Unresumed::Busy);
    }
    if status != NO_SUCH_SESSION || asked.session == LegSession::Other {
        return Err(Unresumed::Refused);
    }
    let Some(request) = fallback.filter(|f| *f != asked.request) else {
        return Err(Unresumed::Refused);
    };
    match arrival {
        Arrival::Fresh => Ok(Ask {
            request,
            session: LegSession::Other,
        }),
        Arrival::Predates => Err(Unresumed::Predates),
        Arrival::Unknown => Err(Unresumed::Unknown),
        Arrival::Shared => Err(Unresumed::Shared),
    }
}

/// The self target of `request` as the caller wrote it, resolved on a fresh
fn self_resolved(request: &str) -> &'static str {
    match request.split_whitespace().find(|t| t.starts_with('@')) {
        Some("@.") => "`@.` resolved there",
        _ => "the request names no session, and it resolved there",
    }
}

/// What one ask of a successor came to ([`ask_once`]).
pub(crate) enum Answer {
    /// The status line: `OK subscribe …` or a refusal.
    Line(String),
    /// The request could not be written and the successor left no word.
    Gone,
}

/// Put `request` to the successor `stream` is connected to and read its status line.
pub(crate) fn ask_once(
    stream: &CtlStream,
    reader: &mut BufReader<&CtlStream>,
    token: Option<&str>,
    request: &str,
) -> Answer {
    if super::send_request(stream, token, request).is_err() {
        return match super::pending_status_line(stream) {
            Some(line) => Answer::Line(line),
            None => Answer::Gone,
        };
    }
    let mut status = String::new();
    match super::read_bounded_line(reader, &mut status) {
        Ok(n) if n > 0 => Answer::Line(status.trim_end_matches(['\r', '\n']).to_string()),
        _ => Answer::Line(String::new()),
    }
}

/// The stderr note for a leg a successor accepted.
pub(crate) fn resumed_note(
    old: u32,
    pid: u32,
    path: &str,
    original: &str,
    asked: &Ask,
    arrival: Arrival,
) -> String {
    match asked.session {
        LegSession::Same => {
            let (cause, events) = match arrival {
                Arrival::Predates => (", an update", events_after(&asked.request)),
                Arrival::Fresh if !names_its_sessions(&asked.request) => (
                    ", a relaunch",
                    "its block and turn events start from the relaunch's newest",
                ),
                Arrival::Fresh => (
                    ", a relaunch that recovered the session",
                    "its block and turn events start over with the relaunch's own count, so \
                     some between the two may not have been delivered",
                ),
                Arrival::Unknown | Arrival::Shared => ("", events_after(&asked.request)),
            };
            format!(
                "{}; the stream resumed on {path}, and output, mail and records between the \
                 two were not delivered (`GAP <local> replaced` on each target); {events}",
                replaced_words(old, pid, cause)
            )
        }
        LegSession::Other => format!(
            "aterm was replaced (pid {old} -> {pid}, a relaunch without this stream's session); \
             the stream resumed on {path} with that instance's own session ({}), and output, \
             mail and records between the two were not delivered (`GAP <local> replaced` on \
             each target); its block and turn events start from that session's newest",
            self_resolved(original)
        ),
    }
}

/// What a resumed leg's block and turn events go on from: after the last ones
fn events_after(request: &str) -> &'static str {
    let carries = |key: &str| request.split_whitespace().any(|t| t.starts_with(key));
    match (carries("since-block="), carries("since-turn=")) {
        (true, true) => "its block and turn events go on after the last ones relayed",
        (true, false) => {
            "its block events go on after the last one relayed, and its turn events start \
             from the successor's newest"
        }
        (false, true) => {
            "its turn events go on after the last one relayed, and its block events start \
             from the successor's newest"
        }
        (false, false) => "its block and turn events start from the successor's newest",
    }
}

/// How a note names the server `old` giving way to `pid`, with `cause` said of a different process.
pub(crate) fn replaced_words(old: u32, pid: u32, cause: &str) -> String {
    if pid == old {
        format!("aterm (pid {pid}) reopened its control socket")
    } else {
        format!("aterm was replaced (pid {old} -> {pid}{cause})")
    }
}

/// The stderr note for a successor that did not take the stream.
pub(crate) fn unresumed_note(old: u32, pid: u32, path: &str, why: Unresumed) -> String {
    match why {
        Unresumed::Refused => format!(
            "{}, and {} did not resume this subscription; re-issue it",
            replaced_words(old, pid, ""),
            if pid == old { "it" } else { "its successor" }
        ),
        Unresumed::Busy => format!(
            "{}, and {} stayed too busy to resume this subscription; re-issue it",
            replaced_words(old, pid, ""),
            if pid == old { "it" } else { "its successor" }
        ),
        Unresumed::Predates | Unresumed::Unknown | Unresumed::Shared if pid == old => format!(
            "aterm (pid {pid}) reopened its control socket without the session this stream \
             followed; re-issue it"
        ),
        Unresumed::Predates | Unresumed::Unknown | Unresumed::Shared => format!(
            "aterm (pid {old}) went away; {path} now answers as pid {pid}, which does not hold \
             the session this stream followed; re-issue it"
        ),
    }
}

/// How one leg of a relayed stream ended.
enum Leg {
    /// The server hung up (EOF, reset), read at this moment ([`HangupAt::seen`]).
    HungUp(Stamp),
    /// The explicit `--timeout` wall clock ran out.
    Timeout,
}

/// Relay one connection's frames to `out` until the server hangs up or the
fn pump(
    stream: &CtlStream,
    reader: &mut BufReader<&CtlStream>,
    out: &mut impl io::Write,
    tracker: &mut FrameTracker,
    watch: Option<(Instant, Duration)>,
    clock: fn() -> Stamp,
) -> io::Result<Leg> {
    let poll = Duration::from_millis(250);
    let _ = stream.set_read_timeout(Some(poll));
    let mut buf = [0u8; 8192];
    loop {
        if let Some((start, limit)) = watch
            && start.elapsed() >= limit
        {
            tracker.finish(out)?;
            out.flush()?;
            return Ok(Leg::Timeout);
        }
        match reader.read(&mut buf) {
            Ok(0) => {
                let at = clock();
                tracker.finish(out)?;
                out.flush()?;
                return Ok(Leg::HungUp(at));
            }
            Ok(n) => {
                tracker.relay(&buf[..n], out)?;
                out.flush()?;
            }
            Err(ref e) if super::is_timeout_error(e) => {}
            Err(ref e) if is_hangup(e) => {
                let at = clock();
                tracker.finish(out)?;
                out.flush()?;
                return Ok(Leg::HungUp(at));
            }
            Err(e) => return Err(e),
        }
    }
}

/// The first leg of a `subscribe` relay, already past its `OK subscribe` ack.
pub(crate) struct FirstLeg<'a, 'b> {
    pub(crate) stream: &'b CtlStream,
    pub(crate) reader: &'a mut BufReader<&'b CtlStream>,
    /// The path it was dialled at, resolved.
    pub(crate) dialed: &'a str,
    /// The server's pid, when the platform can say.
    pub(crate) server: Option<u32>,
    /// The listening socket it was accepted through ([`listener_at`], read once connected).
    pub(crate) listener: Option<ListenerId>,
}

/// Relay a `subscribe` push stream to `out` across server replacements (the module rule).
pub(crate) fn relay_subscription(
    first: FirstLeg<'_, '_>,
    request: &str,
    watch: Option<Duration>,
    redial: &Redial,
    out: &mut impl io::Write,
) -> io::Result<ExitCode> {
    let watch = watch.map(|limit| (Instant::now(), limit));
    let until = watch.map(|(start, limit)| start + limit);
    let mut redial = redial.clone();
    let mut tracker = FrameTracker::new(request);
    let exit = redial.watch_exit(first.server);
    let mut hung_up = match pump(
        first.stream,
        first.reader,
        out,
        &mut tracker,
        watch,
        redial.clock,
    )? {
        Leg::Timeout => return Ok(ExitCode::from(super::EXIT_TIMEOUT)),
        Leg::HungUp(seen) => HangupAt { seen, exit },
    };
    let mut ask_until = Wait::from_now(redial.bound, until).deadline;
    let original = request;
    let mut request = request.to_string();
    let mut dialed = first.dialed.to_string();
    let mut server = first.server;
    let mut listener = first.listener;
    let mut never_took: Option<(u32, u32, String)> = None;
    'leg: loop {
        if tracker.session_over() {
            return Ok(ExitCode::SUCCESS);
        }
        if tracker.said_quit() {
            super::stderr_line(&quit_note(server))?;
            return Ok(ExitCode::from(EXIT_REPLACED));
        }
        let hangup = match tracker.take_host_gone() {
            Some((pid, sock)) => {
                redial = redial.host(&sock, target_sid(&request).map(|sid| sid.as_str()));
                server = Some(pid);
                listener = None;
                dialed = sock;
                redial.after_host_gone(&dialed, pid, until)
            }
            None => redial.after_hangup(&dialed, server, listener, until),
        };
        if hangup == Hangup::StillServing
            && let Some((old, pid, path)) = never_took.take()
        {
            super::stderr_line(&unresumed_note(old, pid, &path, Unresumed::Refused))?;
            return Ok(ExitCode::from(EXIT_REPLACED));
        }
        let (path, pid) = match hangup {
            Hangup::StillServing | Hangup::Unknown => return Ok(ExitCode::SUCCESS),
            Hangup::Gone {
                waited,
                by_deadline,
            } => {
                super::stderr_line(&gone_note(server, waited, by_deadline, redial.unfollowed()))?;
                return Ok(ExitCode::from(EXIT_REPLACED));
            }
            Hangup::Successor { path, pid } => (path, pid),
        };
        let old = server.unwrap_or(0);
        let Some((resumed, mut fallback)) =
            resume_lines(original, &request, &tracker.sids(), &tracker.anchors())
        else {
            let target =
                untied_local_target(&request, &tracker.sids()).unwrap_or_else(|| "@<n>".to_owned());
            super::stderr_line(&format!(
                "{}; `{target}` cannot follow it — re-issue the subscription by sid (exit \
                 {EXIT_REPLACED})",
                replaced_words(old, pid, "")
            ))?;
            return Ok(ExitCode::from(EXIT_REPLACED));
        };
        let mut ask = Ask {
            request: resumed,
            session: LegSession::Same,
        };
        loop {
            let Ok((stream, token)) = super::connect_then_read(&path, super::read_token_for) else {
                dialed = path;
                continue 'leg;
            };
            if server_pid(&stream) != Some(pid) {
                continue 'leg;
            }
            let leg_listener = listener_at(&path);
            let exit = redial.watch_exit(Some(pid));
            let handshake = until.map_or(SUCCESSOR_BOUND, |until| {
                until
                    .saturating_duration_since(Instant::now())
                    .clamp(PROBE_FLOOR, SUCCESSOR_BOUND)
            });
            let _ = stream.set_write_timeout(Some(handshake));
            let _ = stream.set_read_timeout(Some(handshake));
            let mut reader = BufReader::new(&stream);
            let answer = ask_once(&stream, &mut reader, token.as_deref(), &ask.request);
            let busy = matches!(&answer, Answer::Line(line) if is_busy(line));
            let silent = matches!(&answer, Answer::Gone)
                || matches!(&answer, Answer::Line(line) if line.is_empty());
            let live = busy || (silent && answers_at(&path, pid, leg_listener));
            if live && Instant::now() < ask_until {
                std::thread::sleep(SUCCESSOR_POLL);
                continue;
            }
            if silent && live {
                super::stderr_line(&unresumed_note(old, pid, &path, Unresumed::Busy))?;
                return Ok(ExitCode::from(EXIT_REPLACED));
            }
            let status = match answer {
                Answer::Line(line) if !silent => line,
                Answer::Line(_) | Answer::Gone => {
                    never_took = Some((old, pid, path.clone()));
                    dialed = path;
                    server = Some(pid);
                    listener = leg_listener;
                    hung_up = HangupAt {
                        seen: (redial.clock)(),
                        exit,
                    };
                    continue 'leg;
                }
            };
            let status = status.as_str();
            if !super::is_ok_reply(status) {
                let arrival = redial.arrival(pid, &path, hung_up.moment());
                let why = match after_refusal(&ask, status, fallback.take(), arrival) {
                    Ok(next) => {
                        ask = next;
                        continue;
                    }
                    Err(why) => why,
                };
                if !status.is_empty() && why != Unresumed::Busy {
                    super::stderr_line(status)?;
                }
                super::stderr_line(&unresumed_note(old, pid, &path, why))?;
                return Ok(ExitCode::from(EXIT_REPLACED));
            }
            super::stderr_line(status)?;
            let arrival = redial.arrival(pid, &path, hung_up.moment());
            super::stderr_line(&resumed_note(old, pid, &path, original, &ask, arrival))?;
            for (local, _) in tracker.sids() {
                out.write_all(format!("GAP {local} replaced\n").as_bytes())?;
            }
            out.flush()?;
            never_took = None;
            tracker.start_leg(ask.session);
            request = ask.request;
            hung_up = match pump(&stream, &mut reader, out, &mut tracker, watch, redial.clock)? {
                Leg::Timeout => return Ok(ExitCode::from(super::EXIT_TIMEOUT)),
                Leg::HungUp(seen) => HangupAt { seen, exit },
            };
            ask_until = Wait::from_now(redial.bound, until).deadline;
            dialed = path;
            server = Some(pid);
            listener = leg_listener;
            continue 'leg;
        }
    }
}

/// The note for a hang-up nothing replaced.
pub(crate) fn gone_note(
    server: Option<u32>,
    waited: Duration,
    by_deadline: bool,
    unfollowed: Option<&Unfollowed>,
) -> String {
    let pinned = match unfollowed {
        Some(Unfollowed::PidPin(pin)) => Some(*pin),
        _ => None,
    };
    let pid = server.or(pinned).unwrap_or(0);
    let within = if by_deadline {
        format!("the {:.1} s its --timeout left", waited.as_secs_f64())
    } else {
        format!("{:.1} s", waited.as_secs_f64())
    };
    let why = match unfollowed {
        Some(Unfollowed::PidPin(_)) if !waited.is_zero() => {
            format!(
                " and did not answer again within {within}; a --pid call follows no other instance"
            )
        }
        Some(Unfollowed::PidPin(_)) => "; a --pid call follows no other instance".to_owned(),
        Some(Unfollowed::AskedOnce) => "; this was already the one retry".to_owned(),
        None => format!(" and nothing replaced it within {within}"),
    };
    format!(
        "aterm (pid {pid}) went away{why}; the exchange cannot be resumed (exit {EXIT_REPLACED})"
    )
}

/// The note for a stream whose instance said `bye quit` and hung up
fn quit_note(server: Option<u32>) -> String {
    let pid = server.map_or_else(|| "?".to_owned(), |pid| pid.to_string());
    format!(
        "aterm (pid {pid}) quit (`EVENT * bye quit`); nothing replaces a quit, so the stream \
         ends (exit {EXIT_REPLACED})"
    )
}

#[cfg(test)]
#[path = "redial_tests.rs"]
mod tests;
