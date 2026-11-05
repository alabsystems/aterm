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
    drive: crate::FleetDriveFn,
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
    match driven_exchange(drive, (host, request), within, said) {
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

fn driven_exchange(
    drive: crate::FleetDriveFn,
    (host, request): (&str, &str),
    within: Duration,
    cut: crate::remote::Within,
) -> Leg {
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
/// that Mac answered for it, it is that row with `state=closed` and its `task_news=` as `1`: its
/// rev is the Mac's, which a board takes as one its worker reported ([`crate::board::Row::merge`];
/// a row from a Mac too old to write `task_news=` stays without one, and is taken as below). With
/// none read, it is a row of every column a server's has, at the asker's edge and with no
/// `task_news=`, which a board reads as no rev its worker reported.
fn closed_row(last: Option<&str>, target: &Target) -> String {
    match last {
        Some(row) => row
            .split_whitespace()
            .map(|t| {
                if t.starts_with("state=") {
                    "state=closed"
                } else if t.starts_with("task_news=") {
                    "task_news=1"
                } else {
                    t
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
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
    let legs = Legs {
        max: DRIVEN_LEG_MAX,
        send: Arc::new(move |peer_host: &str, req: &str, wait: Duration| {
            driven_leg(drive, (peer_host, req), wait, (cut, longer))
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
    let legs = Legs {
        max: DRIVEN_LEG_MAX,
        send: Arc::new(move |peer_host: &str, request: &str, wait: Duration| {
            driven_leg(drive, (peer_host, request), wait, (cut, longer))
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

/// One leg to `reach`: over its socket, over a drive (`drive`, cut by `cut`), or nowhere.
fn reach_leg(
    reach: &Reach,
    drive: Option<crate::FleetDriveFn>,
    (request, wait): (&str, Duration),
    (cut, longer): (Option<(Instant, Duration)>, Option<crate::remote::Longer>),
) -> Leg {
    match (reach, drive) {
        (Reach::Socket(sock), _) => socket_leg(sock, request, wait),
        (Reach::Drive(host), Some(drive)) => {
            driven_leg(drive, (host, request), wait, (cut, longer))
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
    let legs = Legs {
        max: DRIVEN_LEG_MAX,
        // A key no reach names is a local instance's successor (`Redial`), at its socket.
        send: Arc::new(move |key: &str, req: &str, wait: Duration| {
            match reaches.iter().find(|(k, _)| k == key) {
                Some((_, reach)) => reach_leg(reach, drive, (req, wait), (cut, longer)),
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
    let mut seen = Vec::new();
    let unanswered = || match reach {
        Reach::Drive(host) => vec![host.clone()],
        _ => Vec::new(),
    };
    while !targets.is_empty() {
        let request = leg_request(targets.iter(), Combine::Any, false, wait);
        match reach_leg(reach, drive, (&request, wait), (None, None)) {
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
mod tests {
    use super::*;

    fn parts(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn the_client_reads_every_form_and_refuses_the_rest() {
        let list = parse(&parts(
            "await task @s-a1,@2:5,@s-a1 all since=3 timeout=1500",
        ))
        .unwrap();
        assert_eq!(
            list,
            Request {
                targets: vec![
                    Target {
                        selector: "s-a1".into(),
                        since: None,
                        fact_since: None,
                    },
                    Target {
                        selector: "2".into(),
                        since: Some(5),
                        fact_since: None,
                    },
                ],
                combine: Combine::All,
                since: Some(3),
                facts: false,
                fact_since: None,
                timeout: Some(Duration::from_millis(1500)),
            },
            "a target named twice is waited on once"
        );
        let single = parse(&parts("@s-b2 await task since=4 timeout 90000")).unwrap();
        assert_eq!(
            single.targets,
            vec![Target {
                selector: "s-b2".into(),
                since: None,
                fact_since: None,
            }]
        );
        assert_eq!(
            (single.since, single.timeout),
            (Some(4), Some(Duration::from_secs(90)))
        );
        let bare = parse(&parts("await task")).unwrap();
        assert_eq!(
            bare.targets,
            vec![Target {
                selector: ".".into(),
                since: None,
                fact_since: None,
            }]
        );
        assert_eq!(bare.combine, Combine::Any);
        assert!(!bare.facts);
        let facts = parse(&parts("await task @s-a:4:2,@s-b facts=1 fact_since=7")).unwrap();
        assert_eq!(
            (facts.facts, facts.fact_since),
            (true, Some(7)),
            "the manager door's opt-in"
        );
        assert_eq!(
            facts.targets,
            vec![
                Target {
                    selector: "s-a".into(),
                    since: Some(4),
                    fact_since: Some(2),
                },
                Target {
                    selector: "s-b".into(),
                    since: None,
                    fact_since: None,
                },
            ]
        );
        for bad in [
            "@s-a await task @s-b",
            "await task @s-a @s-b",
            "await task @s-a:x",
            "await task s-a",
            "await task any all",
            "await task since=1 since=2",
            "await task timeout=soon",
            "await task timeout",
            "await task later",
            "await task facts=1 facts=1",
            "await task facts=2",
            "await task facts=1 fact_since=1 fact_since=2",
            "await task facts=1 fact_since=x",
            "await task fact_since=1",
            "await task @s-a:4:2",
            "await task @s-a:4:x facts=1",
            "await task @s-a::2 facts=1",
            "await task @s-a:+4",
            "await task @s-a:4:+2 facts=1",
            "await task since=+1",
            "await task facts=1 fact_since=+1",
            "await task timeout=+1500",
            "await task timeout +1500",
        ] {
            assert!(parse(&parts(bad)).is_err(), "{bad:?}");
        }
        for empty in [
            "await task @s-a,@,@s-c",
            "await task @",
            "await task @:3",
            "await task @s-a,@:2",
        ] {
            let e = parse(&parts(empty)).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{empty:?}");
            assert_eq!(
                e.to_string(),
                aterm_types::control_verbs::EMPTY_SELECTOR,
                "{empty:?}"
            );
        }
        let explicit = parse(&parts("await task @s-a,@.,@s-c")).unwrap();
        assert_eq!(
            explicit
                .targets
                .iter()
                .map(|t| t.selector.as_str())
                .collect::<Vec<_>>(),
            ["s-a", ".", "s-c"]
        );
        assert!(is_await_task(&parts("@s-a await task")));
        assert!(is_await_task(&parts("await task @s-a")));
        assert!(!is_await_task(&parts("await agent idle")));
        assert!(!is_await_task(&parts("@s-a await")));
        assert!(
            run_await_task(
                &parts("await task timeout=18446744073709551615"),
                true,
                "/no/such/aterm-ctl-test.sock",
                &redial::Redial::none(),
                Some(Some(Duration::from_secs(u64::MAX))),
            )
            .is_err()
        );
    }

    /// `failed` WAKES A WAIT like the other terminal words, past the edge the waiter last read.
    /// NEGATIVE CONTROL: at or before its edge it is old news, and `working` is none.
    #[test]
    fn a_failed_row_is_news_past_its_edge() {
        let target = |since| Target {
            selector: "s-a".into(),
            since,
            fact_since: None,
        };
        let row = "@s-a rev=4 state=failed agent=claude program_state=- attention=- result_bytes=0 \
                   conclusion=failed why=could-not-finish";
        assert!(row_news(row, &target(Some(3)), false));
        assert!(!row_news(row, &target(Some(4)), false));
        let working = row.replace("state=failed", "state=working");
        assert!(!row_news(&working, &target(Some(3)), false));
    }

    #[test]
    fn a_leg_names_each_targets_baseline() {
        let targets = vec![
            Target {
                selector: "s-a".into(),
                since: Some(3),
                fact_since: None,
            },
            Target {
                selector: "1".into(),
                since: None,
                fact_since: None,
            },
        ];
        assert_eq!(
            leg_request(
                targets.iter(),
                Combine::All,
                false,
                Duration::from_secs(600)
            ),
            "await task @s-a:3,@1 all timeout=600000\n"
        );
        let mut facts = targets.clone();
        assert_eq!(
            leg_request(facts.iter(), Combine::Any, true, Duration::ZERO),
            "await task @s-a:3,@1 any facts=1 timeout=0\n"
        );
        facts[0].fact_since = Some(2);
        facts[1].fact_since = Some(5);
        assert_eq!(
            leg_request(facts.iter(), Combine::Any, true, Duration::ZERO),
            "await task @s-a:3:2,@1 any facts=1 fact_since=5 timeout=0\n",
            "a target with no rev yet takes the request's fact edge"
        );
        facts[1].since = Some(9);
        assert_eq!(
            leg_request(facts.iter(), Combine::Any, true, Duration::ZERO),
            "await task @s-a:3:2,@1:9:5 any facts=1 timeout=0\n"
        );
        assert_eq!(row_fact_rev("@s-a rev=7 fact=- fact_rev=3"), 3);
        assert_eq!(row_fact_rev("@s-a rev=7 state=done"), 0, "never edited");
        let row = "@s-a rev=7 state=done agent=- program_state=idle attention=- result_bytes=1";
        assert_eq!(row_rev(row), Some(7));
        assert_eq!(row_rev("garbage"), None);
        assert_eq!(row_sid(row), Some("s-a"));
        assert_eq!(row_sid("@3 rev=1"), None, "a row names a global sid");
        assert!(!row_closed(row));
        assert_eq!(
            closed_row(Some(row), &targets[0]),
            "@s-a rev=7 state=closed agent=- program_state=idle attention=- result_bytes=1",
            "the server's last row stands, its program's report with it"
        );
        assert_eq!(
            closed_row(None, &targets[0]),
            "@s-a rev=3 state=closed agent=- program_state=- attention=- result_bytes=0",
            "a row the client writes has every column a server row has"
        );
        assert!(row_closed(&closed_row(None, &targets[0])));
        assert!(
            row_field(&closed_row(None, &targets[0]), "task_news").is_none(),
            "a row the client writes says so by having no `task_news=`, which the board reads \
             as a rev no worker reported"
        );
        let refusal = io::Error::other("ERR no such session @s-a");
        assert_eq!(gone_target(&refusal, &targets, &[0, 1]), Some(0));
        assert_eq!(gone_target(&refusal, &targets, &[1]), None, "not live");
        let other = io::Error::other("ERR denied");
        assert_eq!(gone_target(&other, &targets, &[0, 1]), None);
    }

    /// A close a wait builds from the last row the worker's Mac answered keeps that row's rev and
    /// `task_news=`, so a board takes the rev as one its worker reported: a live row read before
    /// the close, at a lower rev, does not reopen the row and drop the conclusion and receipt the
    /// close kept (the loss 6a8935aa23 stopped for a close the Mac wrote, which dropping
    /// `task_news=` here would bring back on this path), and the worker's own row at the close's
    /// rev still reopens it. NEGATIVE CONTROL: the close a wait builds from no row carries the
    /// asker's edge and no `task_news=`, which leaves the board's rev, so the worker's own answer
    /// below that edge still reopens the row.
    #[test]
    fn a_close_from_the_macs_last_row_keeps_its_rev_and_one_from_none_does_not() {
        let target = Target {
            selector: "s-0a1".into(),
            since: Some(9),
            fact_since: None,
        };
        let open = |rev: u64| crate::board::Row {
            rev,
            ..crate::board::Row::started("s-0a1", 1)
        };
        let last = "@s-0a1 rev=5 state=done agent=- program_state=- attention=- result_bytes=4 \
                    conclusion=finished checked=0123456789ab@origin/main task_news=0";
        let closed = closed_row(Some(last), &target);
        assert_eq!(
            (row_rev(&closed), row_field(&closed, "task_news")),
            (Some(5), Some("1")),
            "the Mac's rev, marked as the Mac's: {closed}"
        );
        let mut row = open(3);
        assert!(row.merge(&closed, 10));
        assert_eq!(
            (row.state.as_str(), row.rev, row.landed.as_deref()),
            ("closed", 5, Some("0123456789ab"))
        );
        assert!(
            !row.merge(
                "@s-0a1 rev=4 state=working agent=- program_state=- attention=- result_bytes=0 \
                 task_news=0",
                20
            ),
            "a read from before the close changes nothing"
        );
        assert_eq!(
            (
                row.state.as_str(),
                row.conclusion.as_deref(),
                row.checked.as_deref()
            ),
            ("closed", Some("finished"), Some("0123456789ab@origin/main"))
        );
        assert!(
            row.merge(last, 30),
            "the worker's own row at the close's rev reopens it: its tab came back"
        );
        assert_eq!((row.state.as_str(), row.rev), ("done", 5));

        let gone = closed_row(None, &target);
        assert_eq!(
            (row_rev(&gone), row_field(&gone, "task_news")),
            (Some(9), None),
            "the asker's edge, unmarked: {gone}"
        );
        let mut row = open(3);
        assert!(row.merge(&gone, 10));
        assert_eq!(
            (row.state.as_str(), row.rev),
            ("closed", 3),
            "an edge no worker reported is not the board's rev"
        );
        assert!(
            row.merge(
                "@s-0a1 rev=3 state=working agent=- program_state=- attention=- result_bytes=0 \
                 task_news=0",
                20
            ),
            "the worker's own answer at its own rev reopens it"
        );
        assert_eq!(row.state, "working");
    }

    #[test]
    fn a_legs_rows_share_one_byte_budget() {
        use std::io::Cursor;
        let cut = || Leg::Again(io::Error::from(io::ErrorKind::UnexpectedEof));
        let body = "one\r\r\nsecond\n";
        let wire = format!("OK 2 status=timeout\n{body}");
        let Leg::TimedOut(rows) = read_leg(&mut Cursor::new(wire.as_bytes()), body.len(), cut)
        else {
            panic!("the exact byte boundary must fit");
        };
        assert_eq!(rows, ["one", "second"]);
        let Leg::Failed(error) = read_leg(&mut Cursor::new(wire.as_bytes()), body.len() - 1, cut)
        else {
            panic!("the next row must be refused before it is retained");
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let Leg::Answered(rows) = read_leg(&mut Cursor::new(b"OK 0\n"), 0, cut) else {
            panic!("a zero-row answer fits no budget");
        };
        assert!(rows.is_empty());
        for partial in ["", "OK 2\none\n"] {
            assert!(matches!(
                read_leg(&mut Cursor::new(partial.as_bytes()), body.len(), cut),
                Leg::Again(_)
            ));
        }
        for malformed in ["OK nope\n", "OK 200001\n"] {
            let Leg::Failed(error) =
                read_leg(&mut Cursor::new(malformed.as_bytes()), body.len(), cut)
            else {
                panic!("a malformed count is a refusal, not a cut");
            };
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
        let Leg::Failed(error) = read_leg(&mut Cursor::new(b"ERR denied\n"), body.len(), cut)
        else {
            panic!("the peer's refusal must remain a refusal");
        };
        assert_eq!(error.to_string(), "ERR denied");
    }

    type Reply = Box<dyn FnOnce() -> Leg + Send>;
    type Step = (String, Reply);

    fn scripted(script: Vec<(&'static str, &'static str, Reply)>) -> Legs {
        let mut by_sock: std::collections::HashMap<String, std::collections::VecDeque<Step>> =
            std::collections::HashMap::new();
        for (sock, request, step) in script {
            by_sock
                .entry(sock.to_string())
                .or_default()
                .push_back((request.to_string(), step));
        }
        let by_sock = std::sync::Mutex::new(by_sock);
        Legs {
            max: Duration::from_millis(600),
            again: (Duration::from_millis(5), AGAIN_FOR),
            send: Arc::new(move |sock: &str, request: &str, _wait: Duration| {
                let step = by_sock
                    .lock()
                    .unwrap()
                    .get_mut(sock)
                    .and_then(std::collections::VecDeque::pop_front);
                match step {
                    Some((want, step)) if want == request => step(),
                    Some((want, _)) => Leg::Failed(io::Error::other(format!(
                        "{sock}: sent {request:?}, the script wants {want:?}"
                    ))),
                    None => Leg::Failed(io::Error::other(format!(
                        "{sock}: nothing scripted for {request:?}"
                    ))),
                }
            }),
        }
    }

    fn rows(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|r| (*r).to_string()).collect()
    }

    fn timed_out(r: &[&str]) -> Reply {
        let r = rows(r);
        Box::new(move || Leg::TimedOut(r))
    }

    fn answered(r: &[&str]) -> Reply {
        let r = rows(r);
        Box::new(move || Leg::Answered(r))
    }

    fn refused(line: &'static str) -> Reply {
        Box::new(move || Leg::Failed(io::Error::other(line)))
    }

    fn step(f: impl FnOnce() -> Leg + Send + 'static) -> Reply {
        Box::new(f)
    }

    fn one_group(sock: &str, selectors: &[&str]) -> Vec<Group> {
        vec![Group {
            sock: sock.to_string(),
            targets: selectors
                .iter()
                .map(|s| Target {
                    selector: (*s).to_string(),
                    since: None,
                    fact_since: None,
                })
                .collect(),
            order: Vec::new(),
            recorded: None,
        }]
    }

    fn waited(
        groups: Vec<Group>,
        combine: Combine,
        until: Option<Instant>,
        legs: &Legs,
    ) -> (Vec<String>, bool) {
        match wait_task_groups(groups, combine, false, until, &redial::Redial::none(), legs) {
            Ok(Waited::Rows { rows, answered }) => (rows, answered),
            Ok(Waited::Unanswered { why, .. }) => panic!("unanswered: {why}"),
            Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
            Ok(Waited::Ungranted { why, .. }) => panic!("refused: {why}"),
            Err(e) => panic!("{e}"),
        }
    }

    /// The re-arm carries each target's fact edge as its baseline row read it (0 with no
    /// `fact_rev=`); without `facts=1` no leg asks for facts and no element carries a fact edge.
    #[test]
    fn under_facts_1_the_rearm_carries_each_targets_fact_edge() {
        let x = "/x.sock";
        let legs = scripted(vec![
            (
                x,
                "await task @s-a,@s-b any facts=1 timeout=0\n",
                timed_out(&[
                    "@s-a rev=2 state=working agent=- attention=- result_bytes=0",
                    "@s-b rev=1 state=working agent=- attention=- result_bytes=0 fact=- fact_rev=4",
                ]),
            ),
            (
                x,
                "await task @s-a:2:0,@s-b:1:4 any facts=1 timeout=600\n",
                answered(&[
                    "@s-a rev=2 state=working agent=- attention=- result_bytes=0 \
                     fact=hold-room@v1#1 fact_rev=1",
                    "@s-b rev=1 state=working agent=- attention=- result_bytes=0 fact=- fact_rev=4",
                ]),
            ),
        ]);
        let groups = one_group(x, &["s-a", "s-b"]);
        let Ok(Waited::Rows {
            rows,
            answered: woke,
        }) = wait_task_groups(
            groups,
            Combine::Any,
            true,
            None,
            &redial::Redial::none(),
            &legs,
        )
        else {
            panic!("the scripted legs answer");
        };
        assert!(woke);
        assert!(
            rows[0].ends_with(" fact=hold-room@v1#1 fact_rev=1"),
            "{rows:?}"
        );
        let legs = scripted(vec![
            (
                x,
                "await task @s-b any timeout=0\n",
                timed_out(&[
                    "@s-b rev=1 state=working agent=- attention=- result_bytes=0 fact=- fact_rev=4",
                ]),
            ),
            (
                x,
                "await task @s-b:1 any timeout=600\n",
                timed_out(&[
                    "@s-b rev=1 state=working agent=- attention=- result_bytes=0 fact=hold-room@v1#1 fact_rev=5",
                ]),
            ),
            (
                x,
                "await task @s-b:1 any timeout=600\n",
                answered(&[
                    "@s-b rev=2 state=done agent=- attention=- result_bytes=3 fact=- fact_rev=4",
                ]),
            ),
        ]);
        let (rows, woke) = waited(one_group(x, &["s-b"]), Combine::Any, None, &legs);
        assert!(woke);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn all_waits_do_not_latch_baseline_or_cleared_facts_or_display_attention() {
        let x = "/x.sock";
        let a = "@s-a rev=1 state=working agent=- attention=- result_bytes=0 fact=hold-room@v1#1 fact_rev=3";
        let b = "@s-b rev=1 state=working agent=- attention=- result_bytes=0";
        let final_a = "@s-a rev=3 state=done agent=- attention=- result_bytes=9";
        let final_b = "@s-b rev=2 state=done agent=- attention=- result_bytes=3";
        let legs = scripted(vec![
            (
                x,
                "await task @s-a,@s-b all facts=1 timeout=0\n",
                timed_out(&[a, b]),
            ),
            (
                x,
                "await task @s-a:1:3,@s-b:1:0 all facts=1 timeout=600\n",
                timed_out(&[
                    "@s-a rev=2 state=working agent=- attention=an%20unrelated%20owner result_bytes=0 fact=hold-room@v1#2 fact_rev=4 task_news=0",
                    b,
                ]),
            ),
            (
                x,
                "await task @s-a:1:3,@s-b:1:0 all facts=1 timeout=600\n",
                timed_out(&[
                    "@s-a rev=2 state=done agent=- attention=- result_bytes=0 fact=- fact_rev=4 task_news=unknown",
                    b,
                ]),
            ),
            (
                x,
                "await task @s-a:1:3,@s-b:1:0 all facts=1 timeout=600\n",
                answered(&[final_a, final_b]),
            ),
        ]);
        let result = wait_task_groups(
            one_group(x, &["s-a", "s-b"]),
            Combine::All,
            true,
            None,
            &redial::Redial::none(),
            &legs,
        );
        assert!(
            matches!(result, Ok(Waited::Rows { rows, answered: true }) if rows == [final_a, final_b])
        );
    }

    /// An all-wait keeps news it saw, even when a fact clears or a terminal task reopens before
    /// the other target answers.
    #[test]
    fn all_waits_keep_terminal_news_that_came_and_went_between_legs() {
        all_waits_keep_news_between_legs(false, false);
    }

    #[test]
    fn all_waits_keep_fact_news_that_came_and_went_between_legs() {
        all_waits_keep_news_between_legs(true, false);
    }

    #[test]
    fn all_waits_keep_authoritative_attention_news_between_legs() {
        all_waits_keep_news_between_legs(false, true);
    }

    #[test]
    fn all_waits_keep_news_the_initial_baseline_leg_already_latched() {
        let x = "/x.sock";
        let first_news = "@s-a rev=2 state=working agent=- attention=- result_bytes=0 task_news=1";
        let final_news = "@s-b rev=2 state=done agent=- attention=- result_bytes=3 task_news=1";
        let legs = scripted(vec![
            (
                x,
                "await task @s-a,@s-b all timeout=0\n",
                timed_out(&[
                    first_news,
                    "@s-b rev=1 state=working agent=- attention=- result_bytes=0 task_news=0",
                ]),
            ),
            (
                x,
                "await task @s-b:1 all timeout=600\n",
                answered(&[final_news]),
            ),
        ]);
        let result = wait_task_groups(
            one_group(x, &["s-a", "s-b"]),
            Combine::All,
            false,
            None,
            &redial::Redial::none(),
            &legs,
        );
        assert!(
            matches!(result, Ok(Waited::Rows { rows, answered: true }) if rows == [first_news, final_news])
        );
    }

    fn all_waits_keep_news_between_legs(facts: bool, attention: bool) {
        let x = "/x.sock";
        let baseline = "@s-a rev=1 state=working agent=- attention=- result_bytes=0";
        let other = "@s-b rev=1 state=working agent=- attention=- result_bytes=0";
        let news = if attention {
            "@s-a rev=2 state=working agent=- attention=- result_bytes=0 task_news=1"
        } else if facts {
            "@s-a rev=1 state=working agent=- attention=- result_bytes=0 fact=hold-room@v1#1 fact_rev=1"
        } else {
            "@s-a rev=2 state=done agent=- attention=- result_bytes=9"
        };
        let first = if facts {
            "await task @s-a,@s-b all facts=1 timeout=0\n"
        } else {
            "await task @s-a,@s-b all timeout=0\n"
        };
        let second = if facts {
            "await task @s-a:1:0,@s-b:1:0 all facts=1 timeout=600\n"
        } else {
            "await task @s-a:1,@s-b:1 all timeout=600\n"
        };
        let third = if facts {
            "await task @s-b:1:0 all facts=1 timeout=600\n"
        } else {
            "await task @s-b:1 all timeout=600\n"
        };
        let final_news = "@s-b rev=2 state=done agent=- attention=- result_bytes=3";
        let legs = scripted(vec![
            (x, first, timed_out(&[baseline, other])),
            (x, second, timed_out(&[news, other])),
            // s-a's news is held, so this leg does not ask for it again.
            (x, third, answered(&[final_news])),
        ]);
        let result = wait_task_groups(
            one_group(x, &["s-a", "s-b"]),
            Combine::All,
            facts,
            None,
            &redial::Redial::none(),
            &legs,
        );
        assert!(
            matches!(result, Ok(Waited::Rows { rows, answered: true }) if rows == [news, final_news]),
            "the all-wait must retain each observed row (facts={facts}, attention={attention})"
        );
    }

    #[test]
    fn a_wait_rearms_past_its_baseline_and_keeps_a_worker_that_closed() {
        let x = "/x.sock";
        let legs = scripted(vec![
            (
                x,
                "await task @1,@s-b,@. all timeout=0\n",
                timed_out(&[
                    "@s-a rev=1 state=working agent=- attention=- result_bytes=0",
                    "@s-b rev=1 state=working agent=- attention=- result_bytes=0",
                    "@s-c rev=1 state=working agent=- attention=- result_bytes=0",
                ]),
            ),
            (
                x,
                "await task @s-a:1,@s-b:1,@s-c:1 all timeout=600\n",
                timed_out(&[
                    "@s-a rev=2 state=done agent=- attention=- result_bytes=9",
                    "@s-b rev=1 state=working agent=busy attention=- result_bytes=0",
                    "@s-c rev=1 state=working agent=- attention=- result_bytes=0",
                ]),
            ),
            (
                x,
                "await task @s-b:1,@s-c:1 all timeout=600\n",
                refused("ERR no such session @s-b"),
            ),
            (
                x,
                "await task @s-c:1 all timeout=600\n",
                answered(&["@s-c rev=3 state=exited agent=- attention=- result_bytes=0"]),
            ),
        ]);
        let (rows, answered) = waited(one_group(x, &["1", "s-b", "."]), Combine::All, None, &legs);
        assert!(answered);
        assert_eq!(
            rows,
            [
                "@s-a rev=2 state=done agent=- attention=- result_bytes=9",
                "@s-b rev=1 state=closed agent=busy attention=- result_bytes=0",
                "@s-c rev=3 state=exited agent=- attention=- result_bytes=0",
            ]
        );

        let legs = scripted(vec![
            (
                x,
                "await task @s-a,@s-b all timeout=0\n",
                timed_out(&[
                    "@s-a rev=1 state=working agent=- attention=- result_bytes=0",
                    "@s-b rev=4 state=closed agent=- attention=- result_bytes=0",
                ]),
            ),
            (
                x,
                "await task @s-a:1 all timeout=600\n",
                refused("ERR no such session @s-a"),
            ),
        ]);
        let (rows, answered) = waited(one_group(x, &["s-a", "s-b"]), Combine::All, None, &legs);
        assert!(answered);
        assert_eq!(
            rows,
            [
                "@s-a rev=1 state=closed agent=- attention=- result_bytes=0",
                "@s-b rev=4 state=closed agent=- attention=- result_bytes=0",
            ]
        );

        let legs = scripted(vec![(
            x,
            "await task @s-d all timeout=0\n",
            refused("ERR no such session @s-d"),
        )]);
        let refusal = wait_task_groups(
            one_group(x, &["s-d"]),
            Combine::All,
            false,
            None,
            &redial::Redial::none(),
            &legs,
        );
        assert!(
            matches!(&refusal, Err(e) if e.to_string() == "ERR no such session @s-d"),
            "the refusal ends the run"
        );
    }

    #[test]
    fn a_leg_the_peer_did_not_serve_is_asked_again() {
        let x = "lab-mini";
        let busy = || step(|| Leg::Again(io::Error::other("lab-mini is busy")));
        let working = "@s-a rev=3 state=working agent=busy attention=- result_bytes=0";
        let done = "@s-a rev=4 state=done agent=idle attention=- result_bytes=9";
        let legs = scripted(vec![
            (x, "await task @s-a any timeout=0\n", busy()),
            (x, "await task @s-a any timeout=0\n", timed_out(&[working])),
            (x, "await task @s-a:3 any timeout=600\n", busy()),
            (x, "await task @s-a:3 any timeout=600\n", answered(&[done])),
        ]);
        let (rows, answered) = waited(one_group(x, &["s-a"]), Combine::Any, None, &legs);
        assert!(answered);
        assert_eq!(rows, [done]);

        let legs = scripted(vec![(
            x,
            "await task @s-a any timeout=0\n",
            refused("ERR denied: node ab12cd34 is not one the dialled Mac lets drive it"),
        )]);
        let refusal = wait_task_groups(
            one_group(x, &["s-a"]),
            Combine::Any,
            false,
            None,
            &redial::Redial::none(),
            &legs,
        );
        assert!(
            matches!(&refusal, Err(e) if e.to_string().starts_with("ERR denied")),
            "{:?}",
            refusal.err()
        );

        let legs = Legs {
            again: (Duration::ZERO, Duration::ZERO),
            ..scripted(vec![(x, "await task @s-a any timeout=0\n", busy())])
        };
        let gave_up = wait_task_groups(
            one_group(x, &["s-a"]),
            Combine::Any,
            false,
            None,
            &redial::Redial::none(),
            &legs,
        );
        assert!(
            matches!(&gave_up, Err(e) if e.to_string() == "lab-mini is busy"),
            "{:?}",
            gave_up.err()
        );
    }

    #[test]
    fn instances_combine_any_and_all() {
        let (x, y) = ("/x.sock", "/y.sock");
        let two = || {
            let mut groups = one_group(x, &["s-a"]);
            groups.extend(one_group(y, &["s-b"]));
            groups
        };
        let legs = scripted(vec![
            (
                x,
                "await task @s-a all timeout=0\n",
                timed_out(&["@s-a rev=1 state=working agent=- attention=- result_bytes=0"]),
            ),
            (
                x,
                "await task @s-a:1 all timeout=600\n",
                answered(&["@s-a rev=2 state=done agent=- attention=- result_bytes=3"]),
            ),
            (
                y,
                "await task @s-b all timeout=0\n",
                timed_out(&["@s-b rev=4 state=working agent=- attention=- result_bytes=0"]),
            ),
            (
                y,
                "await task @s-b:4 all timeout=600\n",
                timed_out(&["@s-b rev=4 state=working agent=- attention=- result_bytes=0"]),
            ),
            (
                y,
                "await task @s-b:4 all timeout=600\n",
                answered(&["@s-b rev=5 state=stalled agent=- attention=- result_bytes=0"]),
            ),
        ]);
        assert_eq!(
            waited(two(), Combine::All, None, &legs),
            (
                rows(&[
                    "@s-a rev=2 state=done agent=- attention=- result_bytes=3",
                    "@s-b rev=5 state=stalled agent=- attention=- result_bytes=0",
                ]),
                true
            )
        );

        let (armed_tx, armed_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let legs = scripted(vec![
            (
                x,
                "await task @s-a any timeout=0\n",
                timed_out(&["@s-a rev=1 state=working agent=- attention=- result_bytes=0"]),
            ),
            (
                x,
                "await task @s-a:1 any timeout=600\n",
                step(move || {
                    armed_rx
                        .recv_timeout(Duration::from_secs(60))
                        .expect("y parks");
                    Leg::Answered(rows(&[
                        "@s-a rev=2 state=done agent=- attention=- result_bytes=3",
                    ]))
                }),
            ),
            (
                y,
                "await task @s-b any timeout=0\n",
                timed_out(&["@s-b rev=4 state=working agent=- attention=- result_bytes=0"]),
            ),
            (
                y,
                "await task @s-b:4 any timeout=600\n",
                step(move || {
                    let _ = armed_tx.send(());
                    let _ = release_rx.recv();
                    Leg::Failed(io::Error::other("released"))
                }),
            ),
        ]);
        assert_eq!(
            waited(two(), Combine::Any, None, &legs),
            (
                rows(&[
                    "@s-a rev=2 state=done agent=- attention=- result_bytes=3",
                    "@s-b rev=4 state=working agent=- attention=- result_bytes=0",
                ]),
                true
            )
        );
        drop(release_tx);

        let legs = scripted(vec![
            (
                x,
                "await task @s-a all timeout=0\n",
                timed_out(&["@s-a rev=1 state=working agent=- attention=- result_bytes=0"]),
            ),
            (
                y,
                "await task @s-b all timeout=0\n",
                timed_out(&["@s-b rev=4 state=working agent=- attention=- result_bytes=0"]),
            ),
        ]);
        let (rows, answered) = waited(two(), Combine::All, Some(Instant::now()), &legs);
        assert!(!answered);
        assert_eq!(rows.len(), 2, "{rows:?}");
    }

    #[test]
    fn targets_group_by_the_instance_that_hosts_them() {
        let scratch = crate::test_scratch_dir("await-task");
        let dir = scratch.path();
        std::fs::create_dir_all(dir.join("graph")).expect("graph dir");
        let targets = vec![
            Target {
                selector: "s-abc".into(),
                since: None,
                fact_since: None,
            },
            Target {
                selector: "3".into(),
                since: None,
                fact_since: None,
            },
        ];
        let pinned = group(targets.clone(), true, "/tmp/pinned.sock", Some(dir));
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned[0].targets.len(), 2);
        // A sid no live instance hosts goes to the caller's own, which may keep the end of a
        // worker whose tab it closed, and otherwise refuses it.
        let unhosted = group(targets, false, "/tmp/default.sock", Some(dir));
        assert_eq!(unhosted.len(), 1);
        assert_eq!(unhosted[0].sock, "/tmp/default.sock");
        assert_eq!(unhosted[0].order, [0, 1]);
        let local = group(
            vec![Target {
                selector: ".".into(),
                since: None,
                fact_since: None,
            }],
            false,
            "/tmp/default.sock",
            Some(dir),
        );
        assert_eq!(local[0].sock, "/tmp/default.sock");
    }

    const BUSY: &str = "@s-a1 rev=3 state=working agent=busy attention=- result_bytes=0";
    const NEEDS_YOU: &str = "@s-a1 rev=3 state=working agent=idle attention=needs-you \
                             result_bytes=0";

    const HANDSHAKE: Duration = Duration::from_millis(30);

    struct Peer {
        sent: Vec<u8>,
        deadline: Instant,
        reply: Option<io::Cursor<Vec<u8>>>,
    }

    // A driven Mac's `await task`: its timeout= runs from the request's arrival, a handshake
    // after the dial, and the connection gives up at the dial's deadline.
    impl io::Read for Peer {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.reply.is_none() {
                let sent = String::from_utf8_lossy(&self.sent).into_owned();
                let ms = sent
                    .split_whitespace()
                    .find_map(|word| word.strip_prefix("timeout="))
                    .and_then(|ms| ms.parse().ok())
                    .unwrap_or(0);
                let answers = Instant::now() + HANDSHAKE + Duration::from_millis(ms);
                if answers > self.deadline {
                    std::thread::sleep(self.deadline.saturating_duration_since(Instant::now()));
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                std::thread::sleep(answers.saturating_duration_since(Instant::now()));
                let row = if ms == 0 { BUSY } else { NEEDS_YOU };
                self.reply = Some(io::Cursor::new(
                    format!("OK 1 status=timeout\n{row}\n").into_bytes(),
                ));
            }
            self.reply.as_mut().map_or(Ok(0), |reply| reply.read(buf))
        }
    }

    impl io::Write for Peer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.sent.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn peer(_: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
        Ok(crate::DriveLink {
            stream: Box::new(Peer {
                sent: Vec::new(),
                deadline: Instant::now() + within,
                reply: None,
            }),
            label: "lab-mini".to_owned(),
            node: None,
        })
    }

    struct Answers(io::Cursor<Vec<u8>>);

    impl io::Read for Answers {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl io::Write for Answers {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn answers(line: &str) -> crate::DriveLink {
        crate::DriveLink {
            stream: Box::new(Answers(io::Cursor::new(format!("{line}\n").into_bytes()))),
            label: "lab-mini".to_owned(),
            node: None,
        }
    }

    #[test]
    fn a_wait_its_own_timeout_ends_reads_the_peers_rows_of_its_last_leg() {
        let second = Some(Some(Duration::from_secs(1)));
        match driven_wait(&parts("await task @s-a1"), second, "lab-mini", peer) {
            Ok(Waited::Rows { rows, answered }) => {
                assert_eq!((rows, answered), (vec![NEEDS_YOU.to_owned()], false));
            }
            Ok(Waited::Unanswered { why, .. }) => panic!("unanswered: {why}"),
            Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
            Ok(Waited::Ungranted { why, .. }) => panic!("refused: {why}"),
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn a_handshake_a_later_leg_never_gets_keeps_its_cause_under_ctls_timeout() {
        static DIALS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn frozen_later(
            host: &str,
            within: Duration,
        ) -> Result<crate::DriveLink, crate::DriveRefused> {
            if DIALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                return peer(host, within);
            }
            std::thread::sleep(within);
            Err(crate::DriveRefused {
                unserved: true,
                ..crate::DriveRefused::once(format!(
                    "127.0.0.1:7100: connected, but no fleet handshake within {:.1}s",
                    within.as_secs_f64()
                ))
            })
        }
        let second = Some(Some(Duration::from_secs(1)));
        let refused = run_driven(&parts("await task @s-a1"), second, "lab-mini", frozen_later)
            .expect_err("unserved");
        assert_eq!(crate::exit_for(&refused), ExitCode::from(2), "{refused}");
        assert!(
            refused.to_string().contains("no fleet handshake"),
            "{refused}"
        );
    }

    const GONE: &str =
        "nothing listens on 127.0.0.1:7100: no aterm running there, or no [fleet] listen";

    fn gone_after_baseline(baseline: &'static str, most: Duration) -> Legs {
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Legs {
            max: Duration::from_millis(600),
            again: (Duration::from_millis(5), most),
            send: Arc::new(move |_: &str, _: &str, _: Duration| {
                if asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                    Leg::TimedOut(rows(&[baseline]))
                } else {
                    Leg::Again(io::Error::other(GONE))
                }
            }),
        }
    }

    #[test]
    fn a_peer_that_serves_no_leg_past_its_baseline_is_asked_until_the_timeout_and_named() {
        let working = "@s-a rev=3 state=working agent=busy attention=- result_bytes=0";
        for most in [Duration::from_millis(20), AGAIN_FOR] {
            let until = Instant::now() + Duration::from_millis(300);
            let ended = wait_task_groups(
                one_group("lab-mini", &["s-a"]),
                Combine::Any,
                false,
                Some(until),
                &redial::Redial::none(),
                &gone_after_baseline(working, most),
            );
            assert!(Instant::now() >= until, "never before its timeout");
            match ended {
                Ok(Waited::Unanswered { rows, why, .. }) => {
                    assert_eq!(
                        (rows, why.to_string()),
                        (vec![working.to_owned()], GONE.into())
                    );
                }
                Ok(Waited::Rows { rows, answered }) => {
                    panic!("{rows:?} answered={answered}: the cause is dropped")
                }
                Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
                Ok(Waited::Ungranted { why, .. }) => panic!("refused: {why}"),
                Err(e) => panic!("ended before its timeout: {e}"),
            }
        }
    }

    #[test]
    fn a_wait_with_no_timeout_whose_peer_serves_no_leg_ends_a_minute_on_with_its_rows() {
        let working = "@s-a rev=3 state=working agent=busy attention=- result_bytes=0";
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let legs = Legs {
            max: Duration::from_millis(600),
            again: (Duration::from_millis(1), Duration::from_millis(20)),
            send: Arc::new(move |_: &str, _: &str, _: Duration| {
                match asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed) {
                    0 => Leg::TimedOut(rows(&[working])),
                    // A hang detector: the bound on asking again is 20 asks or so.
                    10_000.. => Leg::Code(ExitCode::from(99)),
                    _ => Leg::Again(io::Error::other(GONE)),
                }
            }),
        };
        let ended = wait_task_groups(
            one_group("lab-mini", &["s-a"]),
            Combine::Any,
            false,
            None,
            &redial::Redial::none(),
            &legs,
        );
        match ended {
            Ok(Waited::Unanswered { rows, why, .. }) => {
                assert_eq!(
                    (rows, why.to_string()),
                    (vec![working.to_owned()], GONE.into())
                );
            }
            Ok(Waited::Rows { rows, answered }) => panic!("{rows:?} answered={answered}"),
            Ok(Waited::Code(code)) => panic!("asked again without bound: {code:?}"),
            Ok(Waited::Ungranted { why, .. }) => panic!("refused: {why}"),
            Err(e) => panic!("{e}"),
        }
    }

    const ASLEEP: &str = "lab-mini did not answer within 9.5s";

    #[test]
    fn a_driven_wait_whose_peer_went_away_or_fell_silent_ends_at_its_timeout_with_its_rows() {
        use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
        fn later(
            dials: &AtomicUsize,
            (host, within): (&str, Duration),
            then: fn() -> Result<crate::DriveLink, crate::DriveRefused>,
        ) -> Result<crate::DriveLink, crate::DriveRefused> {
            if dials.fetch_add(1, Relaxed) == 0 {
                peer(host, within)
            } else {
                then()
            }
        }
        fn gone(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
            static DIALS: AtomicUsize = AtomicUsize::new(0);
            later(&DIALS, (host, within), || {
                Err(crate::DriveRefused {
                    again: true,
                    ..crate::DriveRefused::once(GONE.to_owned())
                })
            })
        }
        fn asleep(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
            static DIALS: AtomicUsize = AtomicUsize::new(0);
            later(&DIALS, (host, within), || {
                Err(crate::DriveRefused {
                    silent: true,
                    ..crate::DriveRefused::once(ASLEEP.to_owned())
                })
            })
        }
        fn frozen(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
            static DIALS: AtomicUsize = AtomicUsize::new(0);
            later(&DIALS, (host, within), || {
                Ok(crate::DriveLink {
                    stream: Box::new(Peer {
                        sent: Vec::new(),
                        deadline: Instant::now() + HANDSHAKE,
                        reply: None,
                    }),
                    label: "lab-mini".to_owned(),
                    node: None,
                })
            })
        }
        for (drive, cause) in [
            (gone as crate::FleetDriveFn, GONE),
            (asleep, ASLEEP),
            (frozen, "lab-mini did not answer within "),
        ] {
            let began = Instant::now();
            let waited = driven_wait(
                &parts("await task @s-a1 timeout=1500"),
                None,
                "lab-mini",
                drive,
            );
            assert!(
                began.elapsed() >= Duration::from_millis(1500),
                "{cause}: never before its timeout"
            );
            match waited {
                Ok(Waited::Unanswered { rows, why, .. }) => {
                    assert_eq!(rows, [BUSY], "{cause}: the row it read");
                    assert!(why.to_string().starts_with(cause), "{why}");
                }
                Ok(Waited::Rows { rows, answered }) => {
                    panic!("{cause}: {rows:?} answered={answered}, the cause dropped")
                }
                Ok(Waited::Code(code)) => panic!("{cause}: an exit code: {code:?}"),
                Ok(Waited::Ungranted { why, .. }) => panic!("refused: {why}"),
                Err(e) => panic!("{cause}: ended before its timeout: {e}"),
            }
        }
        let unanswered = Waited::Unanswered {
            at: "lab-mini".to_owned(),
            rows: Vec::new(),
            since: Instant::now(),
            why: io::Error::other(GONE),
            served: true,
        };
        assert_eq!(
            print_waited(unanswered, "lab-mini", crate::board::Writes::Nothing).unwrap(),
            ExitCode::from(redial::EXIT_REPLACED)
        );
    }
    #[test]
    fn a_driven_wait_whose_peer_took_its_grant_back_ends_with_its_rows_and_says_so() {
        use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
        const LOWERED: &str = "127.0.0.1:7100 lets no one drive it: its person runs `aterm fleet \
                               allow drive --from <this Mac>` there";
        const DENIED: &str = "node ab12cd34 is not one the dialled Mac lets drive it";
        fn lowered_now() -> Result<crate::DriveLink, crate::DriveRefused> {
            Err(crate::DriveRefused {
                ungranted: true,
                ..crate::DriveRefused::once(LOWERED.to_owned())
            })
        }
        fn lowered(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
            static DIALS: AtomicUsize = AtomicUsize::new(0);
            if DIALS.fetch_add(1, Relaxed) == 0 {
                return peer(host, within);
            }
            lowered_now()
        }
        fn denied(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
            static DIALS: AtomicUsize = AtomicUsize::new(0);
            if DIALS.fetch_add(1, Relaxed) == 0 {
                return peer(host, within);
            }
            Ok(answers(&format!(
                "{}{DENIED}",
                aterm_types::control_verbs::DRIVE_UNGRANTED
            )))
        }
        for (drive, was_lowered, says) in [
            (
                lowered as crate::FleetDriveFn,
                true,
                "its [fleet] allow is below drive now",
            ),
            (denied, false, DENIED),
        ] {
            let waited = driven_wait(
                &parts("await task @s-a1 timeout=15000"),
                None,
                "lab-mini",
                drive,
            );
            match waited {
                Ok(Waited::Ungranted {
                    at,
                    rows,
                    why,
                    lowered,
                    served,
                }) => {
                    assert_eq!(at, "lab-mini");
                    assert_eq!((rows, lowered), (vec![BUSY.to_owned()], was_lowered));
                    assert!(served, "the baseline was served");
                    let said = ungranted(&at, &why, lowered, served);
                    assert!(
                        said.starts_with(
                            "lab-mini served this wait's baseline and refuses this Mac's drive \
                             since (its person took back this Mac's grant"
                        ) && said.contains(says)
                            && !said.contains("--from <this Mac>"),
                        "{said}"
                    );
                }
                Ok(Waited::Unanswered { why, .. }) => panic!("asked again: {why}"),
                Ok(Waited::Rows { rows, answered }) => panic!("{rows:?} answered={answered}"),
                Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
                Err(e) => panic!("the rows it read are dropped: {e}"),
            }
        }
        fn lowered_first(_: &str, _: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
            lowered_now()
        }
        match driven_wait(&parts("await task @s-a1"), None, "lab-mini", lowered_first) {
            Err(e) => assert_eq!(e.to_string(), LOWERED, "before the baseline, its own words"),
            Ok(_) => panic!("a wait no leg of which was served"),
        }
        let refused = Waited::Ungranted {
            at: "lab-mini".to_owned(),
            rows: Vec::new(),
            why: io::Error::other(LOWERED),
            lowered: true,
            served: true,
        };
        assert_eq!(
            print_waited(refused, "lab-mini", crate::board::Writes::Nothing).unwrap(),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn a_driven_wait_past_its_baseline_closes_a_gone_worker_and_ends_any_other_denial_as_said() {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
        const UNCHECKED: &str = "ERR denied: node ab12cd34 is bound on the dialled Mac only by \
                                 its copy of its issuer's book, which is over 60 s old, and it \
                                 could not read that book again to check it (timed out): it \
                                 drives again once the dialled Mac reaches its issuer";
        const GATE: &str = "ERR denied: a drive names a session the driven Mac hosts by its sid \
                            (`survey --host` lists them)";
        static DIALS: AtomicUsize = AtomicUsize::new(0);
        static LATER: Mutex<&str> = Mutex::new("");
        fn then(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
            if DIALS.fetch_add(1, Relaxed) == 0 {
                return peer(host, within);
            }
            Ok(answers(&LATER.lock().unwrap()))
        }
        let wait = |later: &'static str| {
            DIALS.store(0, Relaxed);
            *LATER.lock().unwrap() = later;
            driven_wait(
                &parts("await task @s-a1 timeout=15000"),
                None,
                "lab-mini",
                then,
            )
        };
        match wait("ERR no such session @s-a1") {
            Ok(Waited::Rows { rows, answered }) => assert_eq!(
                (rows, answered),
                (vec![BUSY.replace("state=working", "state=closed")], true)
            ),
            Ok(Waited::Ungranted { why, .. }) => panic!("a gone worker read as a grant: {why}"),
            Ok(Waited::Unanswered { why, .. }) => panic!("unanswered: {why}"),
            Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
            Err(e) => panic!("{e}"),
        }
        for denial in [UNCHECKED, GATE] {
            match wait(denial) {
                Err(e) => assert_eq!(e.to_string(), denial),
                Ok(Waited::Ungranted { why, .. }) => panic!("read as a grant taken back: {why}"),
                Ok(Waited::Rows { rows, answered }) => panic!("{rows:?} answered={answered}"),
                Ok(Waited::Unanswered { why, .. }) => panic!("unanswered: {why}"),
                Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
            }
        }
    }

    #[test]
    fn a_driven_wait_ctls_timeout_cuts_ends_as_unanswered_where_its_peer_froze_past_its_baseline() {
        use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
        struct Frozen(Instant);
        impl io::Read for Frozen {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                std::thread::sleep(self.0.saturating_duration_since(Instant::now()));
                Err(io::ErrorKind::WouldBlock.into())
            }
        }
        impl io::Write for Frozen {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        fn freezes(
            dials: &AtomicUsize,
            (host, within): (&str, Duration),
        ) -> Result<crate::DriveLink, crate::DriveRefused> {
            if dials.fetch_add(1, Relaxed) == 0 {
                return peer(host, within);
            }
            Ok(crate::DriveLink {
                stream: Box::new(Frozen(Instant::now() + within)),
                label: "lab-mini".to_owned(),
                node: None,
            })
        }
        fn frozen(host: &str, within: Duration) -> Result<crate::DriveLink, crate::DriveRefused> {
            static DIALS: AtomicUsize = AtomicUsize::new(0);
            freezes(&DIALS, (host, within))
        }
        fn frozen_untimed(
            host: &str,
            within: Duration,
        ) -> Result<crate::DriveLink, crate::DriveRefused> {
            static DIALS: AtomicUsize = AtomicUsize::new(0);
            freezes(&DIALS, (host, within))
        }
        fn frozen_short(
            host: &str,
            within: Duration,
        ) -> Result<crate::DriveLink, crate::DriveRefused> {
            static DIALS: AtomicUsize = AtomicUsize::new(0);
            freezes(&DIALS, (host, within))
        }
        for (wait, drive, longer) in [
            (
                "await task @s-a1 timeout=60000",
                frozen as crate::FleetDriveFn,
                "a longer --timeout waits as long as it names, up to the wait's timeout= of 60000 \
                 ms, and 0 that whole timeout=",
            ),
            (
                "await task @s-a1",
                frozen_untimed,
                "a longer --timeout waits as long as it names, and 0 until the news comes, where a \
                 peer that answers no leg is asked again for a minute",
            ),
            // Its timeout= ran out inside the --timeout: a longer one waits out the leg alone, what
            // was left of its timeout= and 8 s.
            (
                "await task @s-a1 timeout=1000",
                frozen_short,
                "a longer --timeout, or 0, waits up to the drive's own 8",
            ),
        ] {
            let began = Instant::now();
            let flag = Some(Some(Duration::from_millis(1500)));
            let waited = driven_wait(&parts(wait), flag, "lab-mini", drive);
            assert!(
                began.elapsed() >= Duration::from_millis(1500),
                "never before its --timeout"
            );
            match waited {
                Ok(Waited::Unanswered { rows, why, .. }) => {
                    assert_eq!(rows, [BUSY], "the row it read");
                    assert!(
                        why.to_string().starts_with(&format!(
                            "no answer came from lab-mini within this call's --timeout of 1.5 s \
                             ({longer}"
                        )),
                        "{why}"
                    );
                }
                Ok(Waited::Rows { rows, answered }) => {
                    panic!("{rows:?} answered={answered}: a frozen peer is said as one that served")
                }
                Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
                Ok(Waited::Ungranted { why, .. }) => panic!("refused: {why}"),
                Err(e) => panic!("ended before its timeout: {e}"),
            }
        }
    }

    /// NEGATIVE CONTROL: multi-instance `any` waits must still read every other group's baseline
    /// row (and surface any baseline refusal), keep interleaved targets in caller order, and set
    /// `task_news=1` when synthesizing a `state=closed` row from a baseline row that carried
    /// `task_news=0`.
    #[test]
    fn multi_group_any_wait_keeps_interleaved_order_baseline_rows_and_closed_task_news() {
        let target = Target {
            selector: "s-a".into(),
            since: Some(1),
            fact_since: None,
        };
        assert_eq!(
            closed_row(
                Some(
                    "@s-a rev=1 state=working agent=busy program_state=- attention=- result_bytes=0 task_news=0"
                ),
                &target,
            ),
            "@s-a rev=1 state=closed agent=busy program_state=- attention=- result_bytes=0 task_news=1"
        );

        let (x, y) = ("/x.sock", "/y.sock");
        let interleaved = || {
            vec![
                Group {
                    sock: x.to_string(),
                    targets: vec![
                        Target {
                            selector: "s-a".into(),
                            since: Some(0),
                            fact_since: None,
                        },
                        Target {
                            selector: "s-c".into(),
                            since: Some(0),
                            fact_since: None,
                        },
                    ],
                    order: vec![0, 2],
                    recorded: None,
                },
                Group {
                    sock: y.to_string(),
                    targets: vec![Target {
                        selector: "s-b".into(),
                        since: Some(0),
                        fact_since: None,
                    }],
                    order: vec![1],
                    recorded: None,
                },
            ]
        };
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let legs = scripted(vec![
            (
                x,
                "await task @s-a:0,@s-c:0 any timeout=0\n",
                step(move || {
                    let _ = done_tx.send(());
                    Leg::Answered(rows(&[
                        "@s-a rev=2 state=done agent=- attention=- result_bytes=3 task_news=1",
                        "@s-c rev=1 state=working agent=- attention=- result_bytes=0 task_news=0",
                    ]))
                }),
            ),
            (
                y,
                "await task @s-b:0 any timeout=0\n",
                step(move || {
                    done_rx
                        .recv_timeout(Duration::from_secs(60))
                        .expect("x answered first");
                    std::thread::sleep(Duration::from_millis(10));
                    Leg::TimedOut(rows(&[
                        "@s-b rev=4 state=working agent=- attention=- result_bytes=0 task_news=0",
                    ]))
                }),
            ),
        ]);
        let (got, woke) = waited(interleaved(), Combine::Any, None, &legs);
        assert!(woke);
        assert_eq!(
            got,
            rows(&[
                "@s-a rev=2 state=done agent=- attention=- result_bytes=3 task_news=1",
                "@s-b rev=4 state=working agent=- attention=- result_bytes=0 task_news=0",
                "@s-c rev=1 state=working agent=- attention=- result_bytes=0 task_news=0",
            ])
        );

        let (done_tx, done_rx) = mpsc::channel::<()>();
        let legs = scripted(vec![
            (
                x,
                "await task @s-a:0,@s-c:0 any timeout=0\n",
                step(move || {
                    let _ = done_tx.send(());
                    Leg::Answered(rows(&[
                        "@s-a rev=2 state=done agent=- attention=- result_bytes=3 task_news=1",
                        "@s-c rev=1 state=working agent=- attention=- result_bytes=0 task_news=0",
                    ]))
                }),
            ),
            (
                y,
                "await task @s-b:0 any timeout=0\n",
                step(move || {
                    done_rx
                        .recv_timeout(Duration::from_secs(60))
                        .expect("x answered first");
                    std::thread::sleep(Duration::from_millis(10));
                    Leg::Failed(io::Error::other("ERR no such session @s-b"))
                }),
            ),
        ]);
        let err = match wait_task_groups(
            interleaved(),
            Combine::Any,
            false,
            None,
            &redial::Redial::none(),
            &legs,
        ) {
            Err(err) => err,
            Ok(_) => panic!("baseline refusal on another group must not be dropped"),
        };
        assert_eq!(err.to_string(), "ERR no such session @s-b");
    }

    fn board_group(key: &str, pos: usize, sid: &str, rev: u64) -> Group {
        Group {
            sock: key.to_owned(),
            targets: vec![Target {
                selector: sid.to_owned(),
                since: Some(rev),
                fact_since: None,
            }],
            order: vec![pos],
            recorded: Some(vec![format!(
                "@{sid} rev={rev} state=working agent=- program_state=- attention=- \
                 result_bytes=0 read=board"
            )]),
        }
    }

    /// A worker asked by its sid is answered only by a row naming that sid, in every wait: the
    /// regression a driven Mac answering for another worker was, whose state then landed on that
    /// worker's board row.
    #[test]
    fn a_row_naming_another_sid_is_no_answer_for_the_one_asked() {
        let legs = scripted(vec![(
            "drive:e",
            "await task @s-a1 any timeout=0\n",
            answered(&[
                "@s-b2 rev=9 state=closed agent=- attention=- result_bytes=0 \
                        conclusion=finished",
            ]),
        )]);
        match wait_task_groups(
            one_group("drive:e", &["s-a1"]),
            Combine::Any,
            false,
            None,
            &redial::Redial::none(),
            &legs,
        ) {
            Err(e) => assert!(e.to_string().contains("@s-b2"), "{e}"),
            Ok(_) => panic!("a row of another sid read as the answer for s-a1"),
        }
    }

    /// A Mac that refuses a board's wait before it answered at all says its rows are the
    /// board's, never that it served a baseline it did not.
    #[test]
    fn a_board_wait_a_mac_refused_from_the_start_says_its_rows_are_the_boards() {
        let legs = scripted(vec![(
            "drive:m2",
            "await task @s-b2:1 any timeout=0\n",
            step(|| Leg::Ungranted {
                why: io::Error::other(format!(
                    "{}node ab12cd34 is not one the dialled Mac lets drive it",
                    aterm_types::control_verbs::DRIVE_UNGRANTED
                )),
                lowered: false,
            }),
        )]);
        let waited = wait_task_groups(
            vec![board_group("drive:m2", 0, "s-b2", 1)],
            Combine::Any,
            false,
            None,
            &redial::Redial::none(),
            &legs,
        );
        let (rows, why, lowered, served) = match waited {
            Ok(Waited::Ungranted {
                rows,
                why,
                lowered,
                served,
                ..
            }) => (rows, why, lowered, served),
            Ok(_) => panic!("a refusal before any answer read as another end"),
            Err(e) => panic!("a refusal before any answer ended the board's wait: {e}"),
        };
        assert!(!served);
        assert!(
            rows.iter().all(|row| row.ends_with(" read=board")),
            "{rows:?}"
        );
        let said = ungranted("m2", &why, lowered, served);
        assert!(
            said.contains("board's record") && !said.contains("baseline"),
            "{said}"
        );
        let said = unanswered("m2", Instant::now(), &io::Error::other("x"), false);
        assert!(
            said.contains("board's record") && !said.contains("last it read"),
            "{said}"
        );
    }

    /// A board's wait keeps each Mac's failure to its own rows: one Mac answering news ends an
    /// `any` wait with the rows the board knows of a Mac that refused, and under `all` the wait
    /// ends with every row, the refusing Mac's said, never an error for the whole wait.
    #[test]
    fn a_board_wait_keeps_one_macs_failure_to_its_rows() {
        let script = |m1: &'static str, m2: &'static str| {
            scripted(vec![
                (
                    "drive:m1",
                    m1,
                    answered(
                        &["@s-a1 rev=4 state=done agent=- attention=- result_bytes=0 \
                               conclusion=finished"],
                    ),
                ),
                (
                    "drive:m2",
                    m2,
                    refused(
                        "ERR busy: an aterm window is open on this Mac and took no fleet \
                             connection (x); ask again",
                    ),
                ),
            ])
        };
        let groups = || {
            vec![
                board_group("drive:m1", 0, "s-a1", 3),
                board_group("drive:m2", 1, "s-b2", 1),
            ]
        };
        let any = script(
            "await task @s-a1:3 any timeout=0\n",
            "await task @s-b2:1 any timeout=0\n",
        );
        let (rows, answered) = waited(groups(), Combine::Any, None, &any);
        assert!(answered);
        assert!(rows[0].contains("state=done"), "{rows:?}");
        assert!(rows[1].ends_with("read=board"), "{rows:?}");

        let all = wait_task_groups(
            groups(),
            Combine::All,
            false,
            None,
            &redial::Redial::none(),
            &script(
                "await task @s-a1:3 all timeout=0\n",
                "await task @s-b2:1 all timeout=0\n",
            ),
        )
        .expect("no error for the whole wait");
        let Waited::Unanswered { at, rows, .. } = all else {
            panic!("the refusing Mac is said");
        };
        assert_eq!(at, "drive:m2");
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(
            rows[0].contains("state=done"),
            "the Mac that answered: {rows:?}"
        );
        assert!(
            rows[1].ends_with("read=board"),
            "the Mac that refused: {rows:?}"
        );
    }

    /// A Mac with no window open hosts no worker: a board's rows on it close, and a row naming a
    /// sid the leg did not ask for is no answer for the one it asked for.
    #[test]
    fn a_board_wait_closes_a_windowless_macs_workers_and_refuses_a_foreign_row() {
        let legs = scripted(vec![(
            "drive:m3",
            "await task @s-c3:2 any timeout=0\n",
            refused("ERR no window open on this Mac"),
        )]);
        let (rows, closed) = waited(
            vec![board_group("drive:m3", 0, "s-c3", 2)],
            Combine::Any,
            None,
            &legs,
        );
        assert!(closed, "a closed worker answers the wait");
        assert!(
            rows[0].starts_with("@s-c3 ") && rows[0].contains("state=closed"),
            "{rows:?}"
        );

        let legs = scripted(vec![(
            "drive:m4",
            "await task @s-d4:1 any timeout=0\n",
            answered(&[
                "@s-0ther rev=9 state=done agent=- attention=- result_bytes=0 \
                       conclusion=finished checked=0123456789ab@origin/main",
            ]),
        )]);
        let ended = wait_task_groups(
            vec![board_group("drive:m4", 0, "s-d4", 1)],
            Combine::Any,
            false,
            None,
            &redial::Redial::none(),
            &legs,
        )
        .expect("no error for the whole wait");
        let Waited::Unanswered { rows, why, .. } = ended else {
            panic!("a foreign row is no answer");
        };
        assert!(why.to_string().contains("malformed"), "{why}");
        assert_eq!(
            rows,
            [
                "@s-d4 rev=1 state=working agent=- program_state=- attention=- result_bytes=0 \
              read=board"
            ],
            "the board's own row, not the foreign one"
        );
    }

    /// A board's row is waited on where its worker runs: this Mac's own (`node=-` on its own
    /// board, or its own node) at its instance, never over a drive to itself; another Mac's
    /// over a drive to its node; `node=-` on another Mac's board over a drive to that Mac.
    #[test]
    fn a_board_row_is_waited_on_where_its_worker_runs() {
        let own = "0a".repeat(16);
        let other = "0b".repeat(16);
        let row = |node: Option<&str>| crate::board::Row {
            node: node.map(str::to_owned),
            ..crate::board::Row::started("s-0a1", 1)
        };
        let here = (Some("/here.sock"), Some(own.as_str()));
        let sock = Reach::Socket("/here.sock".to_owned());
        assert_eq!(reach_of(&row(None), BoardAt::Here, None, here), sock);
        assert_eq!(reach_of(&row(Some(&own)), BoardAt::Here, None, here), sock);
        assert_eq!(
            reach_of(&row(Some(&own)), BoardAt::Host("m14"), None, here),
            sock
        );
        assert_eq!(
            reach_of(&row(Some(&other)), BoardAt::Here, None, here),
            Reach::Drive(other.clone())
        );
        assert_eq!(
            reach_of(&row(None), BoardAt::Host("m14"), None, here),
            Reach::Drive("m14".to_owned())
        );
        assert!(matches!(
            reach_of(
                &row(Some(&own)),
                BoardAt::Here,
                None,
                (None, Some(own.as_str()))
            ),
            Reach::Nowhere(_)
        ));
    }

    /// With no `since=`, each worker's edge is its board row's revision; more than 64 workers on
    /// one Mac go in several legs.
    #[test]
    fn a_board_wait_starts_each_worker_from_its_row_in_legs_of_64() {
        let rows: Vec<crate::board::Row> = (0..70u64)
            .map(|n| crate::board::Row {
                node: Some("0b".repeat(16)),
                rev: n,
                ..crate::board::Row::started(&format!("s-{:x}", 0x100 + n), n)
            })
            .collect();
        let (groups, reaches) = board_groups(&rows, BoardAt::Here, (None, None), (None, None));
        assert_eq!(reaches.len(), 1);
        assert_eq!(
            groups.iter().map(|g| g.targets.len()).collect::<Vec<_>>(),
            [64, 6]
        );
        assert_eq!(
            groups[1].targets[5].since,
            Some(69),
            "the row's own revision"
        );
        assert_eq!(groups[1].order[5], 69);
        let (groups, _) = board_groups(&rows, BoardAt::Here, (None, None), (Some(0), None));
        assert!(
            groups
                .iter()
                .flat_map(|g| &g.targets)
                .all(|t| t.since == Some(0)),
            "since= names every worker's edge"
        );
    }
}
