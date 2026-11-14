// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl await task`: a manager's wait over its workers.

use std::io::{self, BufReader};
use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use super::{
    EXIT_TIMEOUT, StdoutSink, TargetOrigin, connect_served, instant_after, read_bounded_line,
    read_status_line, redial, same_socket_path, self_instance_sock_in, send_served, socket_dir,
    stderr_line, stdout_handle, stream_count,
};

/// The whole wait when neither the verb's `timeout=` nor `--timeout` names one.
pub(crate) const DEFAULT_TOTAL: Duration = Duration::from_secs(3600);

/// The longest one leg asks for: the server's clamp on every `await`.
pub(crate) const LEG_MAX: Duration = Duration::from_secs(600);

/// What a leg's socket may take beyond the wait it asked for: the reply's own write.
pub(crate) const LEG_SLACK: Duration = Duration::from_secs(30);

const USAGE: &str = "ERR usage: aterm ctl await task [@<sel>[:<rev>[:<fact_rev>]][,...]] \
                     [any|all] [since=<rev>] [facts=1 [fact_since=<fact_rev>]] [timeout=<ms>] | \
                     aterm ctl @<sel> await task [any|all] [since=<rev>] [facts=1 \
                     [fact_since=<fact_rev>]] [timeout=<ms>]";

/// Whether `parts` (the request, `@self` expanded) is `[@<sel>] await task …`.
pub(crate) fn is_await_task(parts: &[String]) -> bool {
    let rest = match parts.first() {
        Some(sel) if sel.starts_with('@') => &parts[1..],
        _ => parts,
    };
    rest.first().map(String::as_str) == Some("await")
        && rest.get(1).map(String::as_str) == Some("task")
}

/// How the instances' answers combine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Combine {
    Any,
    All,
}

impl Combine {
    const fn word(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::All => "all",
        }
    }
}

/// One target as the caller wrote it: the selector body, its own since, and its own fact edge
/// (`@<sel>:<rev>:<fact_rev>`, read under `facts=1` alone).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    selector: String,
    since: Option<u64>,
    fact_since: Option<u64>,
}

/// The request, parsed client-side (the server's grammar: the same words).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Request {
    targets: Vec<Target>,
    combine: Combine,
    since: Option<u64>,
    /// `facts=1`: a new fact word on a worker's task record answers too.
    facts: bool,
    fact_since: Option<u64>,
    timeout: Option<Duration>,
}

fn usage() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, USAGE)
}

fn parse_u64(v: &str) -> Option<u64> {
    (!v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .then(|| v.parse().ok())
        .flatten()
}

fn parse(parts: &[String]) -> io::Result<Request> {
    let (leading, rest) = match parts.first() {
        Some(sel) if sel.starts_with('@') => (Some(sel.as_str()), &parts[1..]),
        _ => (None, parts),
    };
    let words = rest.get(2..).unwrap_or(&[]);
    let mut request = Request {
        targets: Vec::new(),
        combine: Combine::Any,
        since: None,
        facts: false,
        fact_since: None,
        timeout: None,
    };
    let (mut listed, mut combined) = (false, false);
    let mut words = words.iter().map(String::as_str);
    while let Some(word) = words.next() {
        if word.starts_with('@') {
            if leading.is_some() || listed {
                return Err(usage());
            }
            listed = true;
            for element in word.split(',') {
                request.targets.push(target(element)?);
            }
        } else if matches!(word, "any" | "all") {
            if combined {
                return Err(usage());
            }
            combined = true;
            request.combine = if word == "all" {
                Combine::All
            } else {
                Combine::Any
            };
        } else if let Some(rev) = word.strip_prefix("since=") {
            if request.since.is_some() {
                return Err(usage());
            }
            request.since = Some(parse_u64(rev).ok_or_else(usage)?);
        } else if word == "facts=1" {
            if request.facts {
                return Err(usage());
            }
            request.facts = true;
        } else if let Some(rev) = word.strip_prefix("fact_since=") {
            if request.fact_since.is_some() {
                return Err(usage());
            }
            request.fact_since = Some(parse_u64(rev).ok_or_else(usage)?);
        } else {
            let value = match word.strip_prefix("timeout=") {
                Some(value) => value,
                None if word == "timeout" => words.next().ok_or_else(usage)?,
                None => return Err(usage()),
            };
            if request.timeout.is_some() {
                return Err(usage());
            }
            request.timeout = Some(Duration::from_millis(parse_u64(value).ok_or_else(usage)?));
        }
    }
    if let Some(sel) = leading {
        request.targets.push(target(sel)?);
    }
    let fact_edge =
        request.fact_since.is_some() || request.targets.iter().any(|t| t.fact_since.is_some());
    if fact_edge && !request.facts {
        return Err(usage());
    }
    if request.targets.is_empty() {
        request.targets.push(Target {
            selector: ".".to_string(),
            since: None,
            fact_since: None,
        });
    }
    let mut seen: Vec<String> = Vec::new();
    request.targets.retain(|t| {
        let fresh = !seen.contains(&t.selector);
        seen.push(t.selector.clone());
        fresh
    });
    Ok(request)
}

/// One list element, `@<sel>`, `@<sel>:<rev>` or `@<sel>:<rev>:<fact_rev>`
/// ([`aterm_types::control_verbs::EMPTY_SELECTOR`]).
fn target(element: &str) -> io::Result<Target> {
    let body = element.strip_prefix('@').ok_or_else(usage)?;
    let (selector, since, fact_since) = match body.split_once(':') {
        Some((selector, revs)) => {
            let (rev, fact) = match revs.split_once(':') {
                Some((rev, fact)) => (rev, Some(parse_u64(fact).ok_or_else(usage)?)),
                None => (revs, None),
            };
            (selector, Some(parse_u64(rev).ok_or_else(usage)?), fact)
        }
        None => (body, None, None),
    };
    if !aterm_types::control_verbs::selector_body_names_something(selector) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            aterm_types::control_verbs::EMPTY_SELECTOR,
        ));
    }
    Ok(Target {
        selector: selector.to_string(),
        since,
        fact_since,
    })
}

fn global_sid(selector: &str) -> Option<&str> {
    selector
        .strip_prefix("s-")
        .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then_some(selector)
}

/// The targets of one instance, where it answers, and each target's caller-chosen index.
#[derive(Clone, Debug)]
struct Group {
    sock: String,
    targets: Vec<Target>,
    order: Vec<usize>,
    /// For targets read off a board ([`run_board_wait`]), each one's row as the board last
    /// knew it: a worker gone from its Mac before the first answer closes, and a Mac that does
    /// not answer before it keeps these rows, instead of either ending the wait.
    recorded: Option<Vec<String>>,
}

/// Group `targets` by the instance that answers for each: a global sid's host, found in `dir`'s
/// graph, unless the socket is `pinned`; every other target, and a sid no live instance hosts, at
/// `default`. A worker whose tab the caller's own instance closed once its task concluded is
/// hosted nowhere, but that instance keeps its end and answers for it; a sid nothing kept is that
/// instance's `ERR no such session` to say.
fn group(targets: Vec<Target>, pinned: bool, default: &str, dir: Option<&Path>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (pos, target) in targets.into_iter().enumerate() {
        let sock = match global_sid(&target.selector) {
            Some(sid) if !pinned => dir
                .and_then(|dir| self_instance_sock_in(dir, Some(sid)))
                .filter(|sock| aterm_uds::CtlStream::connect(sock).is_ok())
                .unwrap_or_else(|| default.to_string()),
            _ => default.to_string(),
        };
        match groups.iter_mut().find(|g| same_socket_path(&g.sock, &sock)) {
            Some(group) => {
                group.targets.push(target);
                group.order.push(pos);
            }
            None => groups.push(Group {
                sock,
                targets: vec![target],
                order: vec![pos],
                recorded: None,
            }),
        }
    }
    groups
}

/// Whether any of `groups` is served by an aterm other than the one at `own`: a worker another
/// aterm on this Mac hosts ([`group`]), or every target where the socket was pinned elsewhere.
fn served_elsewhere(groups: &[Group], own: &str) -> bool {
    groups.iter().any(|g| !same_socket_path(&g.sock, own))
}

enum Leg {
    /// `OK <n>`: the instance has the news the combine asked for.
    Answered(Vec<String>),
    /// `OK <n> status=timeout`: the leg's deadline, with every row.
    TimedOut(Vec<String>),
    /// The server went away before it answered.
    HungUp {
        server: Option<u32>,
        listener: Option<redial::ListenerId>,
    },
    /// An `ERR` line, or a reply this client cannot read: the run's end, except an
    /// `ERR no such session @<sel>` after the baseline, which closes that target.
    Failed(io::Error),
    /// A leg over a drive the peer did not serve but may serve a moment later: its share for this
    /// machine's address full, nothing listening at its port (an aterm restarting, its listener
    /// rebinding), or the connection cut before the rows.
    Again(io::Error),
    /// A leg over a drive nothing answered within its deadline, or no route reached (a Mac asleep
    /// or off its network, an aterm frozen): asked again as [`Leg::Again`] past the baseline,
    /// and before it the run's end.
    Silent(io::Error),
    /// A connect phase that already said why on stderr.
    Code(ExitCode),
    /// A leg over a drive that ctl's own `--timeout` ran out within, unserved: the wait's end, with
    /// the rows read, or before the baseline this error.
    Cut(io::Error),
    /// ctl's own `--timeout` ran out before a leg over a drive was asked: the wait's end as the
    /// legs before it left it.
    Spent(io::Error),
    /// A leg over a drive the peer refused this Mac at its admission, no grant there admitting
    /// it: its fleet level below drive (`lowered`), or its
    /// [`aterm_types::control_verbs::DRIVE_UNGRANTED`]. Past the baseline, which it served, the
    /// wait's end with the rows read; before it, the run's end.
    Ungranted { why: io::Error, lowered: bool },
}

type LegFn = dyn Fn(&str, &str, Duration) -> Leg + Send + Sync;

/// How a group's legs reach their instance ([`socket_leg`], [`driven_leg`], [`Leg::Again`]).
#[derive(Clone)]
struct Legs {
    max: Duration,
    send: Arc<LegFn>,
    again: (Duration, Duration),
}

fn socket_leg(sock: &str, request: &str, wait: Duration) -> Leg {
    let stream = match connect_served(
        sock,
        TargetOrigin::Pinned,
        Some(wait.saturating_add(LEG_SLACK)),
    ) {
        Ok(Ok(stream)) => stream,
        Ok(Err(code)) => return Leg::Code(code),
        Err(e) => return Leg::Failed(e),
    };
    let server = redial::server_pid(&stream);
    let listener = redial::listener_at(sock);
    match send_served(&stream, request.as_bytes()) {
        Ok(None) => {}
        Ok(Some(code)) => return Leg::Code(code),
        Err(e) if redial::is_hangup(&e) => return Leg::HungUp { server, listener },
        Err(e) => return Leg::Failed(e),
    }
    read_leg(&mut super::reply_on(&stream), super::MAX_BODY_BYTES, || {
        Leg::HungUp { server, listener }
    })
}

/// The longest one leg over a drive asks for (`aterm ctl --host`, [`crate::remote`]).
const DRIVEN_LEG_MAX: Duration = Duration::from_secs(20);

const DRIVEN_SLACK: Duration = Duration::from_secs(8);

/// What a leg over a drive waits past the wait it asks the peer for, room for its dial, handshake
/// and reply: [`DRIVEN_SLACK`], or what the drive's dev seam names for a test of the binary
/// ([`crate::remote::drive_seam`]).
fn driven_slack() -> Duration {
    crate::remote::drive_seam().unwrap_or(DRIVEN_SLACK)
}

/// What a wait over a drive keeps of ctl's own `--timeout` past the wait its last leg asks the
/// peer for, room for that leg's dial, handshake and reply: a second, at most a quarter of it.
fn cut_reserve(flag: Duration) -> Duration {
    Duration::from_secs(1).min(flag / 4)
}

/// Backoff before retrying an unserved leg ([`Leg::Again`], [`Legs::again`]).
const AGAIN_AFTER: Duration = Duration::from_secs(1);

/// The longest a group asks again before its baseline ([`Leg::Again`]); after it, its timeout,
/// or this where it has none.
const AGAIN_FOR: Duration = Duration::from_secs(60);

/// One leg over a drive, within its wait and [`driven_slack`], or what is left of ctl's own
/// `--timeout` (`cut`: when it runs out, and the `--timeout`) where that is shorter, which a cut
/// says a longer one waits out (`longer`: the wait's, where the `--timeout` ends it, else the
/// leg's own); a peer that took the connection and sent no handshake within it keeps that cause
/// (exit 2 before the baseline, and past it asked again).
fn driven_leg(
    asking: &Asking,
    (host, request): (&str, &str),
    wait: Duration,
    (cut, longer): (Option<(Instant, Duration)>, Option<crate::remote::Longer>),
) -> Leg {
    use crate::remote::{Longer, Within};
    use io::ErrorKind::{TimedOut, WouldBlock};
    let own = wait.saturating_add(driven_slack());
    let left = cut.map(|(at, flag)| (at.saturating_duration_since(Instant::now()), flag));
    let (within, said) = match left {
        Some((left, flag)) if left < own => (
            left,
            Within::Timeout(flag, longer.unwrap_or(Longer::Drive(own))),
        ),
        _ => (own, Within::Drive(own)),
    };
    if within.is_zero() {
        return Leg::Spent(crate::remote::closed(host, request, said, TimedOut.into()));
    }
    let ends = instant_after(Instant::now(), within);
    match driven_exchange(asking, (host, request), within, said) {
        Leg::Failed(e) | Leg::Again(e) | Leg::Silent(e)
            if matches!(said, Within::Timeout(..))
                && !crate::is_unserved(&e)
                && (Instant::now() >= ends || matches!(e.kind(), WouldBlock | TimedOut)) =>
        {
            Leg::Cut(e)
        }
        leg => leg,
    }
}

/// HOW ONE WAIT DIALS THE MACS IT ASKS. A wait over a drive is cut into legs of at most
/// [`DRIVEN_LEG_MAX`], each a drive of its own; they are aterm's re-asks of one request, so
/// the wait is one read of each Mac in both Macs' fleet activity records. Until a Mac answers
/// a leg, the legs name the session that waits (`drive`, its requester line); the first leg it
/// answers is noted here, and the legs after it dial naming no session
/// ([`crate::DriveLink::unnamed`]), which the driven Mac counts as no request.
struct Asking {
    drive: crate::FleetDriveFn,
    /// Each Mac that answered a leg of this wait, with the dial that names no session where
    /// its link gave one.
    answered: std::sync::Mutex<Vec<(String, Option<crate::FleetDriveFn>)>>,
}

impl Asking {
    fn by(drive: crate::FleetDriveFn) -> Self {
        Self {
            drive,
            answered: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The dial of the next leg to `host`, and whether `host` already answered a leg of this
    /// wait: that leg was the wait's read of it.
    fn dial(&self, host: &str) -> (crate::FleetDriveFn, bool) {
        let answered = self.answered.lock().unwrap_or_else(|p| p.into_inner());
        answered
            .iter()
            .find(|(held, _)| held == host)
            .map_or((self.drive, false), |(_, unnamed)| {
                (unnamed.unwrap_or(self.drive), true)
            })
    }

    /// `host` answered a leg, over a link whose later dial is `unnamed`.
    fn answered(&self, host: &str, unnamed: Option<crate::FleetDriveFn>) {
        let mut answered = self.answered.lock().unwrap_or_else(|p| p.into_inner());
        if !answered.iter().any(|(held, _)| held == host) {
            answered.push((host.to_owned(), unnamed));
        }
    }
}

fn driven_exchange(
    asking: &Asking,
    (host, request): (&str, &str),
    within: Duration,
    cut: crate::remote::Within,
) -> Leg {
    let (drive, counted) = asking.dial(host);
    let mut link = match drive(host, within) {
        Ok(link) => link,
        Err(refused) if refused.ungranted => {
            return Leg::Ungranted {
                why: io::Error::other(refused),
                lowered: true,
            };
        }
        Err(refused) if refused.again => return Leg::Again(io::Error::other(refused)),
        Err(refused) if refused.silent => return Leg::Silent(io::Error::other(refused)),
        Err(refused) => return Leg::Failed(io::Error::other(refused)),
    };
    let unnamed = link.unnamed;
    let asked = crate::remote::Asked::of(&link);
    let label = link.label;
    if let Err(e) = io::Write::write_all(&mut link.stream, request.as_bytes())
        .and_then(|()| io::Write::flush(&mut link.stream))
    {
        return if redial::is_hangup(&e) {
            Leg::Again(e)
        } else {
            unanswered_or_failed(crate::remote::closed(&label, request, cut, e))
        };
    }
    let leg = read_leg(
        &mut BufReader::new(link.stream),
        super::MAX_BODY_BYTES,
        || {
            Leg::Again(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{label} closed the drive connection before it answered `await task`"),
            ))
        },
    );
    // The first leg the peer answers is the wait's one read of it, for the session that waits;
    // a wait the board's keeper makes names no session and is not counted.
    if matches!(leg, Leg::Answered(_) | Leg::TimedOut(_)) && !counted {
        asked.note(Some(aterm_types::fleet_access::DriveKind::Read), "OK");
        asking.answered(host, unnamed);
    }
    match leg {
        Leg::Failed(e)
            if e.to_string()
                .starts_with(aterm_types::control_verbs::DRIVE_UNGRANTED) =>
        {
            Leg::Ungranted {
                why: e,
                lowered: false,
            }
        }
        Leg::Failed(e) => unanswered_or_failed(crate::remote::closed(&label, request, cut, e)),
        leg => leg,
    }
}

/// A drive connection's failure: [`Leg::Silent`] where its deadline came first, else
/// [`Leg::Failed`].
fn unanswered_or_failed(e: io::Error) -> Leg {
    if matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ) {
        Leg::Silent(e)
    } else {
        Leg::Failed(e)
    }
}

fn read_leg<R: io::BufRead>(reader: &mut R, max_bytes: usize, hung_up: impl Fn() -> Leg) -> Leg {
    let mut status = String::new();
    match read_status_line(reader, &mut status) {
        Ok(0) => return hung_up(),
        Ok(_) => {}
        Err(e) if redial::is_hangup(&e) => return hung_up(),
        Err(e) => return Leg::Failed(e),
    }
    let status = status.trim_end_matches(['\r', '\n']);
    let Some(tail) = status.strip_prefix("OK ") else {
        return Leg::Failed(io::Error::other(status.to_string()));
    };
    let Some(count) = stream_count(tail) else {
        return Leg::Failed(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("malformed await task reply: {status}"),
        ));
    };
    let mut rows = match super::collect_reply_lines(count, max_bytes, |line| {
        read_bounded_line(reader, line)
    }) {
        Ok(rows) => rows,
        Err(e) if redial::is_hangup(&e) => return hung_up(),
        Err(e) => return Leg::Failed(e),
    };
    for row in &mut rows {
        row.truncate(row.trim_end_matches(['\r', '\n']).len());
    }
    if tail.split_whitespace().nth(1) == Some("status=timeout") {
        Leg::TimedOut(rows)
    } else {
        Leg::Answered(rows)
    }
}

pub(crate) fn row_field<'a>(row: &'a str, key: &str) -> Option<&'a str> {
    row.split_whitespace()
        .find_map(|t| t.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
}

fn row_rev(row: &str) -> Option<u64> {
    row_field(row, "rev").and_then(parse_u64)
}

/// A row's `fact_rev=`, 0 where it has none (a record whose fact word was never edited, or any
/// row from an older server).
fn row_fact_rev(row: &str) -> u64 {
    row_field(row, "fact_rev").and_then(parse_u64).unwrap_or(0)
}

fn row_sid(row: &str) -> Option<&str> {
    row.split_whitespace()
        .next()
        .and_then(|head| head.strip_prefix('@'))
        .and_then(global_sid)
}

fn row_closed(row: &str) -> bool {
    row_field(row, "state") == Some("closed")
}

/// Whether `row` is news for `target`. A server's `task_news=` is authoritative (it latches from
/// the qualifying request, covering the baseline leg's edges the client has not read yet); a row
/// without one is judged by the target's `since` and `fact_since` edges.
fn row_news(row: &str, target: &Target, facts: bool) -> bool {
    match row_field(row, "task_news") {
        Some("1") => return true,
        Some(_) => return false,
        None => {}
    }
    let terminal = matches!(
        row_field(row, "state"),
        Some("done" | "exited" | "stalled" | "failed")
    ) && row_rev(row)
        .zip(target.since)
        .is_some_and(|(rev, edge)| rev > edge);
    let fact = facts
        && target
            .fact_since
            .is_some_and(|edge| row_fact_rev(row) > edge)
        && row_field(row, "fact").is_some_and(|word| !word.is_empty() && word != "-");
    terminal || fact
}

/// The row a wait makes for `target` once its Mac no longer hosts it. From `last`, the last row
/// that Mac answered for it, it is that row with `state=closed`, the state that row named kept as
/// its record's own (`task_state=`, as a Mac's own close says it), and its `task_news=` as `1`:
/// its rev is the Mac's, which a board takes as one its worker reported
/// ([`crate::board::Row::merge`]; a row from a Mac too old to write `task_news=` stays without
/// one, and is taken as below). With none read, it is a row of every column a server's has, at
/// the asker's edge and with no `task_news=`, which a board reads as no rev its worker reported.
fn closed_row(last: Option<&str>, target: &Target) -> String {
    match last {
        Some(row) => {
            let state = row_field(row, "state")
                .filter(|s| !matches!(*s, "closed" | "-" | ""))
                .filter(|_| row_field(row, "task_state").is_none())
                .map(|s| format!("task_state={s}"));
            let mut words: Vec<String> = Vec::new();
            for t in row.split_whitespace() {
                if t.starts_with("state=") {
                    words.push("state=closed".to_owned());
                } else if t.starts_with("task_news=") {
                    words.push("task_news=1".to_owned());
                } else {
                    words.push(t.to_owned());
                }
            }
            // The record's own state goes where a Mac's own closed row says it: after
            // `result_bytes=`, before its conclusion (or at the end of a row with none).
            if let Some(state) = state {
                let at = words
                    .iter()
                    .position(|w| w.starts_with("result_bytes="))
                    .map_or(words.len(), |i| i + 1);
                words.insert(at, state);
            }
            words.join(" ")
        }
        None => format!(
            "@{} rev={} state=closed agent=- program_state=- attention=- result_bytes=0",
            target.selector,
            target.since.unwrap_or(0)
        ),
    }
}

/// Whether `e` is a Mac's answer that no window is open on it: its node job's, which hosts no
/// session.
fn is_windowless(e: &io::Error) -> bool {
    e.to_string().starts_with("ERR no window open on this Mac")
}

fn gone_target(e: &io::Error, targets: &[Target], live: &[usize]) -> Option<usize> {
    let refusal = e.to_string();
    let selector = refusal.strip_prefix("ERR no such session @")?;
    live.iter()
        .copied()
        .find(|&i| targets[i].selector == selector)
}

/// One leg's request: each target with its edges, and under `facts=1` the request's own
/// `fact_since=` for the targets with no `rev` yet, whose element cannot carry a fact edge (each
/// such target's edge is the request's, [`run_await_task`]).
fn leg_request<'a>(
    targets: impl Iterator<Item = &'a Target>,
    combine: Combine,
    facts: bool,
    wait: Duration,
) -> String {
    let mut loose = None;
    let list = targets
        .map(|t| match (t.since, t.fact_since) {
            (Some(rev), Some(fact)) => format!("@{}:{rev}:{fact}", t.selector),
            (Some(rev), None) => format!("@{}:{rev}", t.selector),
            (None, fact) => {
                loose = loose.or(fact);
                format!("@{}", t.selector)
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let facts = match (facts, loose) {
        (true, Some(fact)) => format!(" facts=1 fact_since={fact}"),
        (true, None) => " facts=1".to_string(),
        (false, _) => String::new(),
    };
    format!(
        "await task {list} {}{facts} timeout={}\n",
        combine.word(),
        wait.as_millis()
    )
}

enum Report {
    /// The rows as last read (the baseline, or a re-arm's timeout).
    Rows(usize, Vec<String>),
    /// The group's wait ended.
    End(usize, End),
}

enum End {
    Answered(Vec<String>),
    TimedOut(Vec<String>),
    /// Past the baseline (or a board's group before it), its peer served no leg from `since`
    /// to the timeout (`why`: the last's); `served` whether it served the baseline.
    Unanswered {
        rows: Vec<String>,
        since: Instant,
        why: io::Error,
        served: bool,
    },
    Failed(io::Error),
    /// Past the baseline (or a board's group before it), its peer refused this Mac's drive
    /// ([`Leg::Ungranted`]); `served` whether it served the baseline.
    Ungranted {
        rows: Vec<String>,
        why: io::Error,
        lowered: bool,
        served: bool,
    },
    Code(ExitCode),
}

fn malformed(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("malformed await task reply: {what}"),
    )
}

fn run_group(
    (index, mut group): (usize, Group),
    (combine, facts): (Combine, bool),
    until: Option<Instant>,
    follow: redial::Redial,
    legs: &Legs,
    stop: &std::sync::atomic::AtomicBool,
    tx: &mpsc::Sender<Report>,
) -> End {
    use std::sync::atomic::Ordering::Relaxed;
    let n = group.targets.len();
    // A board's targets start from the rows it knows, which a Mac that never answers leaves: no
    // failure of its Mac ends the other Macs' wait.
    let tolerant = group.recorded.is_some();
    let failed = |rows: Vec<Option<String>>, (asked, served): (Instant, bool), e: io::Error| {
        if tolerant {
            ran_out((served, true), rows, (Some(asked), e))
        } else {
            End::Failed(e)
        }
    };
    let mut rows: Vec<Option<String>> = match group.recorded.clone() {
        Some(known) if known.len() == n => known.into_iter().map(Some).collect(),
        _ => vec![None; n],
    };
    let mut ready = vec![false; n];
    let mut baseline_read = false;
    let mut unserved_since: Option<Instant> = None;
    loop {
        if baseline_read && stop.load(Relaxed) {
            return End::TimedOut(rows.into_iter().flatten().collect());
        }
        let live: Vec<usize> = (0..n).filter(|&i| !ready[i]).collect();
        if live.is_empty() {
            return End::Answered(rows.into_iter().flatten().collect());
        }
        let wait = if !baseline_read || (combine == Combine::Any && ready.contains(&true)) {
            Duration::ZERO
        } else {
            until.map_or(legs.max, |until| {
                until
                    .saturating_duration_since(Instant::now())
                    .min(legs.max)
            })
        };
        let request = leg_request(
            live.iter().map(|&i| &group.targets[i]),
            combine,
            facts,
            wait,
        );
        let asked = Instant::now();
        let (answered, latest) = match (legs.send)(&group.sock, &request, wait) {
            Leg::Answered(latest) => (true, latest),
            Leg::TimedOut(latest) => (false, latest),
            Leg::Failed(e) if tolerant && is_windowless(&e) => {
                // A Mac with no window open hosts no worker: each is gone, as a closed tab.
                for &i in &live {
                    ready[i] = true;
                    let last = rows[i].as_deref().filter(|_| baseline_read);
                    rows[i] = Some(closed_row(last, &group.targets[i]));
                }
                continue;
            }
            Leg::Failed(e) => {
                let Some(i) = (baseline_read || tolerant)
                    .then(|| gone_target(&e, &group.targets, &live))
                    .flatten()
                else {
                    return failed(rows, (asked, baseline_read), e);
                };
                ready[i] = true;
                let last = rows[i].as_deref().filter(|_| baseline_read);
                rows[i] = Some(closed_row(last, &group.targets[i]));
                continue;
            }
            Leg::Code(_) if tolerant => {
                let why =
                    io::Error::other(format!("the aterm at {} did not take the wait", group.sock));
                return ran_out((baseline_read, true), rows, (Some(asked), why));
            }
            Leg::Code(code) => return End::Code(code),
            Leg::Ungranted { why, lowered } if baseline_read || tolerant => {
                return End::Ungranted {
                    rows: rows.into_iter().flatten().collect(),
                    why,
                    lowered,
                    served: baseline_read,
                };
            }
            Leg::Ungranted { why, .. } => return End::Failed(why),
            Leg::Cut(e) => {
                let since = *unserved_since.get_or_insert(asked);
                return ran_out((baseline_read, tolerant), rows, (Some(since), e));
            }
            Leg::Spent(e) => {
                let since = unserved_since.or(tolerant.then_some(asked));
                return ran_out((baseline_read, tolerant), rows, (since, e));
            }
            Leg::Silent(e) if !baseline_read && tolerant => {
                return ran_out((false, true), rows, (Some(asked), e));
            }
            Leg::Silent(e) if !baseline_read => return End::Failed(e),
            Leg::Again(e) | Leg::Silent(e) => {
                if baseline_read && stop.load(Relaxed) {
                    return End::TimedOut(rows.into_iter().flatten().collect());
                }
                let now = Instant::now();
                let since = *unserved_since.get_or_insert(asked);
                if until.is_some_and(|until| now >= until) {
                    return ran_out((baseline_read, tolerant), rows, (Some(since), e));
                }
                // Past the baseline, a wait with a timeout asks until it; one with none, as long
                // as one before it would.
                let (after, most) = legs.again;
                if (!baseline_read || until.is_none())
                    && now.saturating_duration_since(since) >= most
                {
                    return ran_out((baseline_read, tolerant), rows, (Some(since), e));
                }
                std::thread::sleep(until.map_or(after, |until| {
                    until.saturating_duration_since(now).min(after)
                }));
                if baseline_read && stop.load(Relaxed) {
                    return End::TimedOut(rows.into_iter().flatten().collect());
                }
                if until.is_some_and(|until| Instant::now() >= until) {
                    return ran_out((baseline_read, tolerant), rows, (Some(since), e));
                }
                continue;
            }
            Leg::HungUp { server, listener } => {
                let policy = match live
                    .iter()
                    .find_map(|&i| global_sid(&group.targets[i].selector))
                {
                    Some(sid) => follow.host(&group.sock, Some(sid)),
                    None => follow.clone(),
                };
                match policy.after_hangup(&group.sock, server, listener, until) {
                    redial::Hangup::Successor { path, pid } => {
                        let _ = stderr_line(&format!(
                            "{} during the wait; still waiting",
                            redial::replaced_words(server.unwrap_or(0), pid, "")
                        ));
                        group.sock = path;
                        continue;
                    }
                    redial::Hangup::Gone {
                        waited,
                        by_deadline,
                    } => {
                        let note =
                            redial::gone_note(server, waited, by_deadline, policy.unfollowed());
                        if tolerant {
                            return ran_out(
                                (baseline_read, true),
                                rows,
                                (Some(asked), io::Error::other(note)),
                            );
                        }
                        let _ = stderr_line(&note);
                        return End::Code(ExitCode::from(redial::EXIT_REPLACED));
                    }
                    redial::Hangup::StillServing | redial::Hangup::Unknown => {
                        return failed(
                            rows,
                            (asked, baseline_read),
                            io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "server closed the connection without responding",
                            ),
                        );
                    }
                }
            }
        };
        unserved_since = None;
        if latest.len() != live.len() {
            let e = malformed(&format!("{} rows for {} targets", latest.len(), live.len()));
            return failed(rows, (asked, baseline_read), e);
        }
        // A worker asked by its global sid is answered by a row naming it: a row naming another
        // is no answer for it, and would make one Mac's answer another worker's state.
        if let Some(row) = live
            .iter()
            .zip(&latest)
            .find(|&(&i, row)| {
                global_sid(&group.targets[i].selector).is_some_and(|sid| row_sid(row) != Some(sid))
            })
            .map(|(_, row)| row.clone())
        {
            return failed(rows, (asked, baseline_read), malformed(&row));
        }
        for (&i, row) in live.iter().zip(latest) {
            if !baseline_read {
                let Some(sid) = row_sid(&row) else {
                    return End::Failed(malformed(&row));
                };
                let target = &mut group.targets[i];
                target.selector = sid.to_string();
                target.since = target.since.or_else(|| row_rev(&row));
                if facts {
                    target.fact_since = target.fact_since.or(Some(row_fact_rev(&row)));
                }
            }
            ready[i] = row_closed(&row) || row_news(&row, &group.targets[i], facts);
            rows[i] = Some(row);
        }
        baseline_read = true;
        let latest: Vec<String> = rows.iter().flatten().cloned().collect();
        if answered || (combine == Combine::Any && ready.contains(&true)) || !ready.contains(&false)
        {
            return End::Answered(latest);
        }
        if until.is_some_and(|until| Instant::now() >= until) || stop.load(Relaxed) {
            return End::TimedOut(latest);
        }
        let _ = tx.send(Report::Rows(index, latest));
    }
}

/// A wait whose deadline ran out at a leg the peer did not answer (`e`): with the rows read, and
/// since when none was served (`unserved`), or before the baseline that leg's cause.
fn ran_out(
    (served, tolerant): (bool, bool),
    rows: Vec<Option<String>>,
    (unserved, e): (Option<Instant>, io::Error),
) -> End {
    let rows = rows.into_iter().flatten().collect();
    match unserved {
        _ if !served && !tolerant => End::Failed(e),
        Some(since) => End::Unanswered {
            rows,
            since,
            why: e,
            served,
        },
        None => End::TimedOut(rows),
    }
}

enum Waited {
    Rows {
        rows: Vec<String>,
        answered: bool,
    },
    /// A wait over a drive that ended as [`End::Unanswered`].
    Unanswered {
        at: String,
        rows: Vec<String>,
        since: Instant,
        why: io::Error,
        served: bool,
    },
    /// A wait over a drive that ended as [`End::Ungranted`].
    Ungranted {
        at: String,
        rows: Vec<String>,
        why: io::Error,
        lowered: bool,
        served: bool,
    },
    Code(ExitCode),
}

fn wait_task_groups(
    groups: Vec<Group>,
    combine: Combine,
    facts: bool,
    until: Option<Instant>,
    follow: &redial::Redial,
    legs: &Legs,
) -> io::Result<Waited> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (tx, rx) = mpsc::channel();
    let count = groups.len();
    let group_socks: Vec<String> = groups.iter().map(|g| g.sock.clone()).collect();
    let mut next_pos = 0usize;
    let group_orders: Vec<Vec<usize>> = groups
        .iter()
        .map(|g| {
            if g.order.len() == g.targets.len() {
                next_pos = next_pos.max(g.order.iter().copied().max().map_or(0, |m| m + 1));
                g.order.clone()
            } else {
                let start = next_pos;
                next_pos += g.targets.len();
                (start..next_pos).collect()
            }
        })
        .collect();
    let stop = Arc::new(AtomicBool::new(false));
    for (index, group) in groups.into_iter().enumerate() {
        let (tx, follow, legs, stop) =
            (tx.clone(), follow.clone(), legs.clone(), Arc::clone(&stop));
        std::thread::Builder::new()
            .name("aterm-ctl-await-task".into())
            .spawn(move || {
                let end = run_group(
                    (index, group),
                    (combine, facts),
                    until,
                    follow,
                    &legs,
                    &stop,
                    &tx,
                );
                let _ = tx.send(Report::End(index, end));
            })?;
    }
    drop(tx);
    let mut rows: Vec<Vec<String>> = vec![Vec::new(); count];
    let mut baseline = vec![false; count];
    let mut ended = vec![false; count];
    let mut answered = vec![false; count];
    let mut unanswered = None;
    let mut ungranted = None;
    let done = |answered: &[bool]| match combine {
        Combine::Any => answered.iter().any(|a| *a),
        Combine::All => answered.iter().all(|a| *a),
    };
    while let Ok(report) = rx.recv() {
        match report {
            Report::Rows(index, latest) => {
                rows[index] = latest;
                baseline[index] = true;
            }
            Report::End(index, end) => {
                ended[index] = true;
                baseline[index] = true;
                match end {
                    End::Answered(latest) => {
                        rows[index] = latest;
                        answered[index] = true;
                        if done(&answered) {
                            stop.store(true, Ordering::Relaxed);
                        }
                    }
                    End::TimedOut(latest) => rows[index] = latest,
                    End::Unanswered {
                        rows: latest,
                        since,
                        why,
                        served,
                    } => {
                        rows[index] = latest;
                        unanswered = Some((group_socks[index].clone(), since, why, served));
                    }
                    End::Ungranted {
                        rows: latest,
                        why,
                        lowered,
                        served,
                    } => {
                        rows[index] = latest;
                        ungranted = Some((group_socks[index].clone(), why, lowered, served));
                    }
                    End::Failed(e) => {
                        stop.store(true, Ordering::Relaxed);
                        return Err(e);
                    }
                    End::Code(code) => {
                        stop.store(true, Ordering::Relaxed);
                        return Ok(Waited::Code(code));
                    }
                }
            }
        }
        if (done(&answered) && baseline.iter().all(|b| *b)) || ended.iter().all(|e| *e) {
            stop.store(true, Ordering::Relaxed);
            break;
        }
    }
    let mut ordered: Vec<(usize, String)> = rows
        .into_iter()
        .zip(group_orders.iter())
        .flat_map(|(group_rows, order)| {
            group_rows
                .into_iter()
                .enumerate()
                .map(move |(k, row)| (order.get(k).copied().unwrap_or(usize::MAX), row))
        })
        .collect();
    ordered.sort_by_key(|(pos, _)| *pos);
    let rows: Vec<String> = ordered.into_iter().map(|(_, row)| row).collect();
    Ok(match (ungranted, unanswered) {
        _ if done(&answered) => Waited::Rows {
            rows,
            answered: true,
        },
        (Some((at, why, lowered, served)), _) => Waited::Ungranted {
            at,
            rows,
            why,
            lowered,
            served,
        },
        (None, Some((at, since, why, served))) => Waited::Unanswered {
            at,
            rows,
            since,
            why,
            served,
        },
        (None, None) => Waited::Rows {
            rows,
            answered: false,
        },
    })
}

/// `[@<sel>] await task …`.
pub(crate) fn run_await_task(
    parts: &[String],
    pinned: bool,
    default: &str,
    follow: &redial::Redial,
    total: Option<Option<Duration>>,
) -> io::Result<ExitCode> {
    let request = parse(parts)?;
    let total = match (request.timeout, total) {
        (Some(verb), _) => Some(verb),
        (None, Some(flag)) => flag,
        (None, None) => Some(DEFAULT_TOTAL),
    };
    let until = total.map(|t| instant_after(Instant::now(), t));
    let dir = socket_dir();
    let targets: Vec<_> = request
        .targets
        .iter()
        .map(|t| Target {
            selector: t.selector.clone(),
            since: t.since.or(request.since),
            fact_since: t.fact_since.or(request.fact_since),
        })
        .collect();
    let groups = group(targets, pinned, default, dir.as_deref());
    let own = crate::own_instance_sock();
    let _vouch = own
        .as_deref()
        .filter(|own| served_elsewhere(&groups, own))
        .and_then(crate::vouch::hold);
    let legs = Legs {
        max: LEG_MAX,
        send: Arc::new(socket_leg),
        again: (AGAIN_AFTER, AGAIN_FOR),
    };
    print_waited(
        wait_task_groups(groups, request.combine, request.facts, until, follow, &legs)?,
        default,
        crate::board::Writes::Here,
    )
}

/// `await task …` over the fleet ([`crate::remote`], [`DRIVEN_LEG_MAX`], [`DEFAULT_TOTAL`]).
pub(crate) fn run_driven(
    parts: &[String],
    total: Option<Option<Duration>>,
    host: &str,
    drive: crate::FleetDriveFn,
) -> io::Result<ExitCode> {
    // Another Mac serves this wait: this Mac's aterm vouches for it while it runs.
    let _vouch = crate::own_instance_sock().and_then(|own| crate::vouch::hold(&own));
    // A Mac a person named answers for whatever sids it is asked: the board's keeper, which
    // asks each worker at its own Mac, keeps their rows.
    print_waited(
        driven_wait(parts, total, host, drive)?,
        host,
        crate::board::Writes::Nothing,
    )
}

/// `await task …` under `--host any`, resolving each `@s-<hex>` target to the peer hosting it.
pub(crate) fn run_driven_any(
    parts: &[String],
    total: Option<Option<Duration>>,
    drive: crate::FleetDriveFn,
    survey: Option<crate::FleetSurveyFn>,
) -> io::Result<ExitCode> {
    let _vouch = crate::own_instance_sock().and_then(|own| crate::vouch::hold(&own));
    let (waited, at) = driven_wait_any(parts, total, drive, survey)?;
    // Each worker the board holds was asked at the Mac its row names.
    print_waited(waited, &at, crate::board::Writes::Rows)
}

fn driven_wait_any(
    parts: &[String],
    total: Option<Option<Duration>>,
    drive: crate::FleetDriveFn,
    survey: Option<crate::FleetSurveyFn>,
) -> io::Result<(Waited, String)> {
    let request = parse(parts)?;
    if let Some(local) = request
        .targets
        .iter()
        .find(|t| global_sid(&t.selector).is_none())
    {
        let named = if local.selector == "." {
            "no target, or `@.`, names this Mac's active tab instead".to_owned()
        } else {
            format!("`@{}` is not one", local.selector)
        };
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "await task over the fleet names each worker by its sid (@s-…, as `aterm ctl \
                 survey --host <h>` shows it); {named}"
            ),
        ));
    }
    let sids: Vec<&str> = request
        .targets
        .iter()
        .map(|t| global_sid(&t.selector).expect("checked global_sid"))
        .collect();
    let start = Instant::now();
    let hosts = crate::remote::resolve_any_sid_hosts(&sids, total, survey)?;
    let cut = total
        .flatten()
        .map(|flag| (instant_after(start, flag), flag));
    let total = match (request.timeout, total) {
        (Some(verb), _) => Some(verb),
        (None, Some(flag)) => flag,
        (None, None) => Some(DEFAULT_TOTAL),
    };
    let inside = cut.map(|(_, flag)| flag.saturating_sub(cut_reserve(flag)));
    let until = total.map(|t| instant_after(start, inside.map_or(t, |inside| t.min(inside))));
    let longer = inside
        .filter(|&inside| total.is_some_and(|t| inside < t))
        .map(|_| crate::remote::Longer::Wait(request.timeout));

    let mut groups: Vec<Group> = Vec::new();
    for (pos, (t, host)) in request.targets.iter().zip(hosts.iter()).enumerate() {
        let target = Target {
            selector: t.selector.clone(),
            since: t.since.or(request.since),
            fact_since: t.fact_since.or(request.fact_since),
        };
        if let Some(existing) = groups.iter_mut().find(|g| g.sock == *host) {
            existing.targets.push(target);
            existing.order.push(pos);
        } else {
            groups.push(Group {
                sock: host.clone(),
                targets: vec![target],
                order: vec![pos],
                recorded: None,
            });
        }
    }
    let at = if groups.len() == 1 {
        groups[0].sock.clone()
    } else {
        "any".to_owned()
    };
    let asking = Asking::by(drive);
    let legs = Legs {
        max: DRIVEN_LEG_MAX,
        send: Arc::new(move |peer_host: &str, req: &str, wait: Duration| {
            driven_leg(&asking, (peer_host, req), wait, (cut, longer))
        }),
        again: (AGAIN_AFTER, AGAIN_FOR),
    };
    let waited = wait_task_groups(
        groups,
        request.combine,
        request.facts,
        until,
        &redial::Redial::none(),
        &legs,
    )?;
    Ok((waited, at))
}

fn driven_wait(
    parts: &[String],
    total: Option<Option<Duration>>,
    host: &str,
    drive: crate::FleetDriveFn,
) -> io::Result<Waited> {
    let request = parse(parts)?;
    if let Some(local) = request
        .targets
        .iter()
        .find(|t| global_sid(&t.selector).is_none())
    {
        let named = if local.selector == "." {
            "no target, or `@.`, names this Mac's active tab instead".to_owned()
        } else {
            format!("`@{}` is not one", local.selector)
        };
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "await task over the fleet names each worker by its sid (@s-…, as `aterm ctl \
                 survey --host <h>` shows it); {named}"
            ),
        ));
    }
    let start = Instant::now();
    // ctl's own `--timeout` is one deadline over every leg: their dials, handshakes and replies.
    let cut = total
        .flatten()
        .map(|flag| (instant_after(start, flag), flag));
    let total = match (request.timeout, total) {
        (Some(verb), _) => Some(verb),
        (None, Some(flag)) => flag,
        (None, None) => Some(DEFAULT_TOTAL),
    };
    // Its last leg asks for a wait that ends inside it, so the rows it reads are the peer's.
    let inside = cut.map(|(_, flag)| flag.saturating_sub(cut_reserve(flag)));
    let until = total.map(|t| instant_after(start, inside.map_or(t, |inside| t.min(inside))));
    // A longer `--timeout` waits longer than a leg's own deadline only where it ends the wait.
    let longer = inside
        .filter(|&inside| total.is_some_and(|t| inside < t))
        .map(|_| crate::remote::Longer::Wait(request.timeout));
    let targets: Vec<_> = request
        .targets
        .iter()
        .map(|t| Target {
            selector: t.selector.clone(),
            since: t.since.or(request.since),
            fact_since: t.fact_since.or(request.fact_since),
        })
        .collect();
    let host = host.to_owned();
    let asking = Asking::by(drive);
    let legs = Legs {
        max: DRIVEN_LEG_MAX,
        send: Arc::new(move |peer_host: &str, request: &str, wait: Duration| {
            driven_leg(&asking, (peer_host, request), wait, (cut, longer))
        }),
        again: (AGAIN_AFTER, AGAIN_FOR),
    };
    let order = (0..targets.len()).collect();
    let groups = vec![Group {
        sock: host,
        targets,
        order,
        recorded: None,
    }];
    wait_task_groups(
        groups,
        request.combine,
        request.facts,
        until,
        &redial::Redial::none(),
        &legs,
    )
}

/// Where the board a wait reads is kept ([`run_board_wait`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoardAt<'a> {
    /// This Mac's.
    Here,
    /// The Mac `--host <h>` names, read over a drive.
    Host(&'a str),
}

/// Whether `parts` is `await task board …` ([`run_board_wait`]).
pub(crate) fn is_board_wait(parts: &[String]) -> bool {
    is_await_task(parts)
        && parts.first().is_some_and(|word| word == "await")
        && parts.get(2).is_some_and(|word| word == "board")
}

/// The most targets one `await task` leg names: the server's limit (`MAX_AWAIT_TARGETS`).
const LEG_TARGETS_MAX: usize = 64;

/// Where one group of a board's workers is waited on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Reach {
    /// This Mac's instance at a socket.
    Socket(String),
    /// Another Mac, over a drive, by the word `--host` takes (its node id, or a host).
    Drive(String),
    /// Nowhere this client can reach, and why.
    Nowhere(String),
}

impl Reach {
    /// The group key a wait's legs are sent by.
    fn key(&self) -> String {
        match self {
            Self::Socket(sock) => sock.clone(),
            Self::Drive(host) => format!("drive:{host}"),
            Self::Nowhere(why) => format!("nowhere:{why}"),
        }
    }
}

/// Where the worker of `row` is waited on: a sid an instance of this Mac hosts (the socket
/// folder `dir` names it) over that instance's socket, or nowhere while it does not answer
/// (an instance restarting, whose workers are not gone); a row of this Mac (`node=-` on its own
/// board, or this Mac's node `own`) over its instance at `here`, which answers for a worker no
/// instance hosts any longer that it is gone, never over a drive to itself; `node=-` on another
/// Mac's board (`at`) over a drive to that Mac; any other node over a drive.
fn reach_of(
    row: &crate::board::Row,
    at: BoardAt<'_>,
    dir: Option<&Path>,
    (here, own): (Option<&str>, Option<&str>),
) -> Reach {
    if let Some(sock) = dir.and_then(|dir| self_instance_sock_in(dir, Some(&row.sid))) {
        if aterm_uds::CtlStream::connect(&sock).is_ok() {
            return Reach::Socket(sock);
        }
        return Reach::Nowhere(format!(
            "the aterm instance of this Mac hosting @{} does not answer",
            row.sid
        ));
    }
    let this_mac = match (&row.node, at) {
        (Some(node), _) => own == Some(node.as_str()),
        (None, BoardAt::Here) => true,
        (None, BoardAt::Host(_)) => false,
    };
    if this_mac {
        return here.map_or_else(
            || Reach::Nowhere("no aterm instance on this Mac answers".to_owned()),
            |sock| Reach::Socket(sock.to_owned()),
        );
    }
    match (&row.node, at) {
        (Some(node), _) => Reach::Drive(node.clone()),
        (None, BoardAt::Host(host)) => Reach::Drive(host.to_owned()),
        (None, BoardAt::Here) => unreachable!("a row of this Mac is waited on here"),
    }
}

/// How a person names `reach` in a note: the Mac's host as a row names it, else its word.
fn reach_name(reach: &Reach, rows: &[crate::board::Row]) -> String {
    match reach {
        Reach::Socket(_) => "this Mac".to_owned(),
        Reach::Drive(word) => rows
            .iter()
            .find(|row| row.node.as_deref() == Some(word.as_str()))
            .and_then(|row| row.host.clone())
            .unwrap_or_else(|| word.clone()),
        Reach::Nowhere(why) => why.clone(),
    }
}

/// One leg to `reach`: over its socket, over a drive (`asking`'s, cut by `cut`), or nowhere.
fn reach_leg(
    reach: &Reach,
    asking: Option<&Asking>,
    (request, wait): (&str, Duration),
    (cut, longer): (Option<(Instant, Duration)>, Option<crate::remote::Longer>),
) -> Leg {
    match (reach, asking) {
        (Reach::Socket(sock), _) => socket_leg(sock, request, wait),
        (Reach::Drive(host), Some(asking)) => {
            driven_leg(asking, (host, request), wait, (cut, longer))
        }
        (Reach::Drive(_), None) => Leg::Silent(io::Error::new(
            io::ErrorKind::Unsupported,
            "this aterm-ctl links no fleet, so it reaches no other Mac: run `aterm ctl`",
        )),
        (Reach::Nowhere(why), _) => Leg::Silent(io::Error::other(why.clone())),
    }
}

/// The groups a wait over `rows` (a board's open rows) sends its legs to, at most
/// [`LEG_TARGETS_MAX`] targets each, every target with `since` (else its row's revision) and
/// `fact_since` and its row as the board knows it ([`Group::recorded`]), with each group's
/// [`Reach`] by its key; `here` is this Mac's instance socket and fleet node ([`reach_of`]).
fn board_groups(
    rows: &[crate::board::Row],
    at: BoardAt<'_>,
    here: (Option<&str>, Option<&str>),
    (since, fact_since): (Option<u64>, Option<u64>),
) -> (Vec<Group>, Vec<(String, Reach)>) {
    let dir = socket_dir();
    let mut groups: Vec<Group> = Vec::new();
    let mut reaches: Vec<(String, Reach)> = Vec::new();
    for (pos, row) in rows.iter().enumerate() {
        let reach = reach_of(row, at, dir.as_deref(), here);
        let key = reach.key();
        if !reaches.iter().any(|(k, _)| *k == key) {
            reaches.push((key.clone(), reach));
        }
        // With no `since=`, a worker's edge is its row's: news the board lacks.
        let target = Target {
            selector: row.sid.clone(),
            since: since.or(Some(row.rev)),
            fact_since,
        };
        match groups
            .iter_mut()
            .find(|g| g.sock == key && g.targets.len() < LEG_TARGETS_MAX)
        {
            Some(group) => {
                group.targets.push(target);
                group.order.push(pos);
                if let Some(known) = group.recorded.as_mut() {
                    known.push(row.await_row());
                }
            }
            None => groups.push(Group {
                sock: key,
                targets: vec![target],
                order: vec![pos],
                recorded: Some(vec![row.await_row()]),
            }),
        }
    }
    (groups, reaches)
}

/// How long a board's wait asks again a Mac that answered `Again` before its first answer:
/// briefly, so an `any` is not held by a Mac that is down.
const BOARD_AGAIN_FOR: Duration = Duration::from_secs(5);

/// Where a board's wait runs: the board (`at`), this Mac's instance socket (`here`) and its
/// fleet node (`own`), where it has them.
pub(crate) struct BoardSite<'a> {
    pub(crate) at: BoardAt<'a>,
    pub(crate) here: Option<&'a str>,
    pub(crate) own: Option<String>,
}

/// `aterm ctl [--host <h>] await task board [any|all] [since=<rev>] [facts=1 …] [timeout=<ms>]
/// [from=<sid>] [label=<word>|<prefix>*]`: `await task` over every open row of a board (this
/// Mac's, or `<h>`'s read over a drive) that the filter takes, each worker where it runs
/// ([`reach_of`]), so a session waits on everything a dispatcher started without knowing a sid.
/// With no `since=`, each worker's edge is its row's revision: news the board lacks. A Mac that
/// fails, refuses or does not answer keeps the board's rows for its workers (`read=board`); it
/// never ends the others' wait. `follow` is how a local leg follows its instance's successor.
/// # Errors
/// The words are no `await task board …`, the board cannot be read, or the wait fails.
pub(crate) fn run_board_wait(
    parts: &[String],
    total: Option<Option<Duration>>,
    site: &BoardSite<'_>,
    drive: Option<crate::FleetDriveFn>,
    follow: &redial::Redial,
) -> io::Result<ExitCode> {
    // A board's workers are asked where each is hosted, another aterm or another Mac among them:
    // this process's own aterm vouches for the wait while it runs.
    let _vouch = crate::own_instance_sock().and_then(|own| crate::vouch::hold(&own));
    let at = site.at;
    let mut words = parts.to_vec();
    words.remove(2);
    let (words, filter) = crate::board::Filter::take(&words)?;
    if words.iter().any(|word| word.starts_with('@')) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ERR usage: aterm ctl [--host <h>] await task board [any|all] [since=<rev>] \
             [timeout=<ms>] [from=<sid>] [label=<word>]: the board names the workers, so it \
             takes no @<sid>",
        ));
    }
    let request = parse(&words)?;
    let start = Instant::now();
    let read = match at {
        BoardAt::Here => {
            let board = crate::board::Board::here().ok_or_else(|| {
                io::Error::new(io::ErrorKind::Unsupported, "this Mac keeps no board")
            })?;
            crate::board::Read {
                header: crate::board::host_name()
                    .map(|host| vec![("host".to_owned(), host)])
                    .unwrap_or_default(),
                rows: board.scan().rows,
            }
        }
        BoardAt::Host(host) => {
            let drive = drive.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "--host reads another Mac's board over the fleet, which this aterm-ctl does \
                     not link: run `aterm ctl --host …`",
                )
            })?;
            let read = crate::remote::read_board(host, total, drive)?;
            let shown_open = read.rows.iter().filter(|row| row.is_open()).count();
            let open = read
                .header
                .iter()
                .find(|(key, _)| key == "open")
                .and_then(|(_, n)| parse_u64(n));
            if let Some(open) = open.filter(|&open| open > shown_open as u64) {
                stderr_line(&format!(
                    "note: {host}'s board holds {open} open rows and showed {shown_open}: the \
                     wait is on those"
                ))?;
            }
            read
        }
    };
    let board_host = match at {
        BoardAt::Here => read.host().map(str::to_owned),
        BoardAt::Host(host) => Some(read.host().unwrap_or(host).to_owned()),
    };
    let open: Vec<crate::board::Row> = read
        .rows
        .into_iter()
        .filter(|row| row.is_open() && row.matches(&filter))
        .collect();
    if open.is_empty() {
        stderr_line("OK board: no open worker")?;
        return Ok(ExitCode::SUCCESS);
    }
    let cut = total
        .flatten()
        .map(|flag| (instant_after(start, flag), flag));
    let whole = match (request.timeout, total) {
        (Some(verb), _) => Some(verb),
        (None, Some(flag)) => flag,
        (None, None) => Some(DEFAULT_TOTAL),
    };
    let inside = cut.map(|(_, flag)| flag.saturating_sub(cut_reserve(flag)));
    let until = whole.map(|t| instant_after(start, inside.map_or(t, |inside| t.min(inside))));
    let longer = inside
        .filter(|&inside| whole.is_some_and(|t| inside < t))
        .map(|_| crate::remote::Longer::Wait(request.timeout));
    let (groups, reaches) = board_groups(
        &open,
        at,
        (site.here, site.own.as_deref()),
        (request.since, request.fact_since),
    );
    let names: Vec<(String, String)> = reaches
        .iter()
        .map(|(key, reach)| (key.clone(), reach_name(reach, &open)))
        .collect();
    let asking = drive.map(Asking::by);
    let legs = Legs {
        max: DRIVEN_LEG_MAX,
        // A key no reach names is a local instance's successor (`Redial`), at its socket.
        send: Arc::new(move |key: &str, req: &str, wait: Duration| {
            match reaches.iter().find(|(k, _)| k == key) {
                Some((_, reach)) => reach_leg(reach, asking.as_ref(), (req, wait), (cut, longer)),
                None => socket_leg(key, req, wait),
            }
        }),
        again: (AGAIN_AFTER, BOARD_AGAIN_FOR),
    };
    let waited = wait_task_groups(groups, request.combine, request.facts, until, follow, &legs)?;
    let tails: Vec<(String, String)> = open
        .iter()
        .map(|row| (row.sid.clone(), row.wait_tail(board_host.as_deref())))
        .collect();
    let at_word = match at {
        BoardAt::Here => "board".to_owned(),
        BoardAt::Host(host) => host.to_owned(),
    };
    let waited = with_tails(waited, &tails);
    // Another Mac's board names where its workers run, not this one's.
    let writes = match at {
        BoardAt::Here => crate::board::Writes::Rows,
        BoardAt::Host(_) => crate::board::Writes::Nothing,
    };
    print_waited(named_at(waited, &names), &at_word, writes)
}

/// `waited`'s note names the Mac that failed it as a person names it, not by its group's key.
fn named_at(waited: Waited, names: &[(String, String)]) -> Waited {
    let name = |at: String| {
        names
            .iter()
            .find(|(key, _)| *key == at)
            .map_or(at, |(_, name)| name.clone())
    };
    match waited {
        Waited::Unanswered {
            at,
            rows,
            since,
            why,
            served,
        } => Waited::Unanswered {
            at: name(at),
            rows,
            since,
            why,
            served,
        },
        Waited::Ungranted {
            at,
            rows,
            why,
            lowered,
            served,
        } => Waited::Ungranted {
            at: name(at),
            rows,
            why,
            lowered,
            served,
        },
        waited => waited,
    }
}

/// `waited` with each row of a board's worker ending in its label and Mac ([`Row::wait_tail`]).
///
/// [`Row::wait_tail`]: crate::board::Row::wait_tail
fn with_tails(waited: Waited, tails: &[(String, String)]) -> Waited {
    let tail = |rows: Vec<String>| -> Vec<String> {
        rows.into_iter()
            .map(|row| {
                let sid = row_sid(&row).map(str::to_owned);
                match sid.and_then(|sid| tails.iter().find(|(s, _)| *s == sid)) {
                    Some((_, tail)) => format!("{row}{tail}"),
                    None => row,
                }
            })
            .collect()
    };
    match waited {
        Waited::Rows { rows, answered } => Waited::Rows {
            rows: tail(rows),
            answered,
        },
        Waited::Unanswered {
            at,
            rows,
            since,
            why,
            served,
        } => Waited::Unanswered {
            at,
            rows: tail(rows),
            since,
            why,
            served,
        },
        Waited::Ungranted {
            at,
            rows,
            why,
            lowered,
            served,
        } => Waited::Ungranted {
            at,
            rows: tail(rows),
            why,
            lowered,
            served,
        },
        Waited::Code(code) => Waited::Code(code),
    }
}

/// What one round of a board's keeper read ([`keep_round`]).
#[cfg(unix)]
#[derive(Debug, Default)]
pub(crate) struct Round {
    /// Every row a Mac answered, as `await task` gives it, and a closed row for each worker its
    /// Mac no longer hosts.
    pub(crate) seen: Vec<String>,
    /// The drive words (node ids, hosts) of the Macs that did not answer.
    pub(crate) unanswered: Vec<String>,
}

/// One round of a board's keeper over `rows`: for each Mac, one `await task @<sid>:<rev>,… any
/// timeout=<wait>` leg, each sid's rev the row's, so a Mac answers at once with news the board
/// lacks and otherwise at the leg's end with every row. A worker its Mac no longer hosts (`ERR no
/// such session`, or no window open there) comes back closed; `here` is this Mac's own instance
/// socket and fleet node ([`reach_of`]).
#[cfg(unix)]
pub(crate) fn keep_round(
    rows: &[crate::board::Row],
    drive: Option<crate::FleetDriveFn>,
    here: (Option<&str>, Option<&str>),
    wait: Duration,
) -> Round {
    let dir = socket_dir();
    let mut groups: Vec<(Reach, Vec<Target>)> = Vec::new();
    for row in rows {
        let reach = reach_of(row, BoardAt::Here, dir.as_deref(), here);
        if matches!(reach, Reach::Nowhere(_))
            || (matches!(reach, Reach::Drive(_)) && drive.is_none())
        {
            continue;
        }
        let target = Target {
            selector: row.sid.clone(),
            since: Some(row.rev),
            fact_since: None,
        };
        match groups
            .iter_mut()
            .find(|(r, targets)| *r == reach && targets.len() < LEG_TARGETS_MAX)
        {
            Some((_, targets)) => targets.push(target),
            None => groups.push((reach, vec![target])),
        }
    }
    let handles: Vec<_> = groups
        .into_iter()
        .filter_map(|(reach, targets)| {
            std::thread::Builder::new()
                .name("aterm-board-keeper-leg".into())
                .spawn(move || keep_group(&reach, targets, drive, wait))
                .ok()
        })
        .collect();
    let mut round = Round::default();
    for handle in handles {
        if let Ok((seen, unanswered)) = handle.join() {
            round.seen.extend(seen);
            round.unanswered.extend(unanswered);
        }
    }
    round
}

/// One keeper leg to `reach` over `targets`, asked again past each worker its Mac no longer
/// hosts: the rows read, and the reach's drive word where it did not answer.
#[cfg(unix)]
fn keep_group(
    reach: &Reach,
    mut targets: Vec<Target>,
    drive: Option<crate::FleetDriveFn>,
    wait: Duration,
) -> (Vec<String>, Vec<String>) {
    let asking = drive.map(Asking::by);
    let mut seen = Vec::new();
    let unanswered = || match reach {
        Reach::Drive(host) => vec![host.clone()],
        _ => Vec::new(),
    };
    while !targets.is_empty() {
        let request = leg_request(targets.iter(), Combine::Any, false, wait);
        match reach_leg(reach, asking.as_ref(), (&request, wait), (None, None)) {
            Leg::Answered(rows) | Leg::TimedOut(rows)
                if rows.len() == targets.len()
                    && rows
                        .iter()
                        .zip(&targets)
                        .all(|(row, target)| row_sid(row) == Some(target.selector.as_str())) =>
            {
                seen.extend(rows);
                return (seen, Vec::new());
            }
            Leg::Failed(e) if is_windowless(&e) => {
                seen.extend(targets.iter().map(|gone| closed_row(None, gone)));
                return (seen, Vec::new());
            }
            Leg::Failed(e) => {
                let live: Vec<usize> = (0..targets.len()).collect();
                let Some(i) = gone_target(&e, &targets, &live) else {
                    return (seen, unanswered());
                };
                let gone = targets.remove(i);
                seen.push(closed_row(None, &gone));
            }
            _ => return (seen, unanswered()),
        }
    }
    (seen, Vec::new())
}

/// The rows on stdout, then `OK timeout` (124), [`unanswered`] at `at` (75, as a blocking read
/// whose instance went away), or [`ungranted`] at `at` (1, as a denial).
fn print_waited(waited: Waited, at: &str, writes: crate::board::Writes) -> io::Result<ExitCode> {
    let (rows, end) = match waited {
        Waited::Rows {
            rows,
            answered: true,
        } => (rows, None),
        Waited::Rows { rows, .. } => (rows, Some(("OK timeout".to_owned(), EXIT_TIMEOUT))),
        Waited::Unanswered {
            at: group_at,
            rows,
            since,
            why,
            served,
        } => {
            let target = if group_at.is_empty() { at } else { &group_at };
            (
                rows,
                Some((
                    unanswered(target, since, &why, served),
                    redial::EXIT_REPLACED,
                )),
            )
        }
        Waited::Ungranted {
            at: group_at,
            rows,
            why,
            lowered,
            served,
        } => {
            let target = if group_at.is_empty() { at } else { &group_at };
            (rows, Some((ungranted(target, &why, lowered, served), 1)))
        }
        Waited::Code(code) => return Ok(code),
    };
    crate::board::apply_seen(&rows, writes);
    let stdout = stdout_handle();
    let mut out = StdoutSink::new(stdout.lock());
    for row in &rows {
        io::Write::write_all(&mut out, row.as_bytes())?;
        io::Write::write_all(&mut out, b"\n")?;
    }
    drop(out);
    match end {
        None => Ok(ExitCode::SUCCESS),
        Some((said, code)) => {
            stderr_line(&said)?;
            Ok(ExitCode::from(code))
        }
    }
}

/// Why a wait over a drive ended with the rows it read before: `at` refused this Mac's drive
/// (`why`), its fleet level below drive now where `lowered`, after serving the wait's baseline
/// where `served`, or before it, on a board's wait, which keeps the board's rows.
fn ungranted(at: &str, why: &io::Error, lowered: bool, served: bool) -> String {
    let cause = if lowered {
        "its person took back this Mac's grant: its [fleet] allow is below drive now (`aterm fleet \
         allow content` or `facts` there)"
            .to_owned()
    } else {
        format!(
            "its person took back this Mac's grant, or its books bind this Mac's node no more: {}",
            why.to_string()
                .trim_start_matches(aterm_types::control_verbs::DRIVE_UNGRANTED)
        )
    };
    if served {
        format!(
            "{at} served this wait's baseline and refuses this Mac's drive since ({cause}), so \
             the rows are the last it read there"
        )
    } else {
        format!(
            "{at} refuses this Mac's drive ({}), so its workers' rows are the board's record \
             (`read=board`)",
            why.to_string()
                .trim_start_matches(aterm_types::control_verbs::DRIVE_UNGRANTED)
        )
    }
}

/// Why a wait over a drive ended with the rows it read before: nothing answered at `at` since
/// `since`, after it served the wait's baseline where `served`, or before it, on a board's wait,
/// which keeps the board's rows.
fn unanswered(at: &str, since: Instant, why: &io::Error, served: bool) -> String {
    let rows = if served {
        "the rows are the last it read there"
    } else {
        "its workers' rows are the board's record (`read=board`), or closed where it said they \
         are gone"
    };
    format!(
        "nothing answered at {at} for the last {:.1} s of this wait ({why}), so {rows}; `aterm \
         ctl survey --host {at}` says whether that Mac answers again",
        since.elapsed().as_secs_f64()
    )
}

#[cfg(test)]
#[path = "await_task_tests.rs"]
mod tests;
