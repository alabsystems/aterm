// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl await task`: a manager's wait over its workers.

use std::io::{self, BufReader};
use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use super::{
    EXIT_TIMEOUT, StdoutSink, TargetOrigin, connect_served, read_bounded_line, read_status_line,
    redial, same_socket_path, self_instance_sock_in, send_served, socket_dir, stderr_line,
    stdout_handle, stream_count,
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
    /// `facts=1`: a new fact word on a worker's task record answers too (the occupancy design's
    /// manager door).
    facts: bool,
    fact_since: Option<u64>,
    timeout: Option<Duration>,
}

fn usage() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, USAGE)
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
            request.since = Some(rev.parse().map_err(|_| usage())?);
        } else if word == "facts=1" {
            if request.facts {
                return Err(usage());
            }
            request.facts = true;
        } else if let Some(rev) = word.strip_prefix("fact_since=") {
            if request.fact_since.is_some() {
                return Err(usage());
            }
            request.fact_since = Some(rev.parse().map_err(|_| usage())?);
        } else {
            let value = match word.strip_prefix("timeout=") {
                Some(value) => value,
                None if word == "timeout" => words.next().ok_or_else(usage)?,
                None => return Err(usage()),
            };
            if request.timeout.is_some() {
                return Err(usage());
            }
            request.timeout = Some(Duration::from_millis(value.parse().map_err(|_| usage())?));
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
                Some((rev, fact)) => (rev, Some(fact.parse().map_err(|_| usage())?)),
                None => (revs, None),
            };
            (selector, Some(rev.parse().map_err(|_| usage())?), fact)
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

/// The targets of one instance, and where it answers.
#[derive(Clone, Debug)]
struct Group {
    sock: String,
    targets: Vec<Target>,
}

fn group(
    targets: Vec<Target>,
    pinned: bool,
    default: &str,
    dir: Option<&Path>,
) -> io::Result<Vec<Group>> {
    let mut groups: Vec<Group> = Vec::new();
    for target in targets {
        let sock = match global_sid(&target.selector) {
            Some(sid) if !pinned => dir
                .and_then(|dir| self_instance_sock_in(dir, Some(sid)))
                .filter(|sock| aterm_uds::CtlStream::connect(sock).is_ok())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "ERR no such session @{sid}: no live aterm instance on this machine hosts it"
                        ),
                    )
                })?,
            _ => default.to_string(),
        };
        match groups.iter_mut().find(|g| same_socket_path(&g.sock, &sock)) {
            Some(group) => group.targets.push(target),
            None => groups.push(Group {
                sock,
                targets: vec![target],
            }),
        }
    }
    Ok(groups)
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
    /// A leg over a drive the peer did not serve, which the same leg a
    /// moment later may be: its share for this machine's address full,
    /// nothing listening at its port (an aterm restarting, its listener
    /// rebinding), or the connection cut before the rows.
    Again(io::Error),
    /// A connect phase that already said why on stderr.
    Code(ExitCode),
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
    let stream = match connect_served(sock, TargetOrigin::Pinned, Some(wait + LEG_SLACK)) {
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
    read_leg(&mut BufReader::new(&stream), super::MAX_BODY_BYTES, || {
        Leg::HungUp { server, listener }
    })
}

/// The longest one leg over a drive asks for (`aterm ctl --host`, [`crate::remote`]).
const DRIVEN_LEG_MAX: Duration = Duration::from_secs(20);

const DRIVEN_SLACK: Duration = Duration::from_secs(8);

/// Backoff before retrying an unserved leg ([`Leg::Again`], [`Legs::again`]).
const AGAIN_AFTER: Duration = Duration::from_secs(1);

/// The longest a group asks again ([`Leg::Again`], [`Legs::again`]).
const AGAIN_FOR: Duration = Duration::from_secs(60);

fn driven_leg(drive: crate::FleetDriveFn, host: &str, request: &str, wait: Duration) -> Leg {
    let mut link = match drive(host, wait + DRIVEN_SLACK) {
        Ok(link) => link,
        Err(refused) if refused.again => return Leg::Again(io::Error::other(refused)),
        Err(refused) => return Leg::Failed(io::Error::other(refused)),
    };
    if let Err(e) = io::Write::write_all(&mut link.stream, request.as_bytes())
        .and_then(|()| io::Write::flush(&mut link.stream))
    {
        return if redial::is_hangup(&e) {
            Leg::Again(e)
        } else {
            Leg::Failed(e)
        };
    }
    let label = link.label;
    read_leg(
        &mut BufReader::new(link.stream),
        super::MAX_BODY_BYTES,
        || {
            Leg::Again(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{label} closed the drive connection before it answered `await task`"),
            ))
        },
    )
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

fn row_field<'a>(row: &'a str, key: &str) -> Option<&'a str> {
    row.split_whitespace()
        .find_map(|t| t.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
}

fn row_rev(row: &str) -> Option<u64> {
    row_field(row, "rev").and_then(|rev| rev.parse().ok())
}

/// A row's `fact_rev=`: 0 where it has none, as a record whose fact word was never edited
/// answers (and an older server's every row).
fn row_fact_rev(row: &str) -> u64 {
    row_field(row, "fact_rev")
        .and_then(|rev| rev.parse().ok())
        .unwrap_or(0)
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

fn closed_row(last: Option<&str>, target: &Target) -> String {
    match last {
        Some(row) => row
            .split_whitespace()
            .map(|t| {
                if t.starts_with("state=") {
                    "state=closed"
                } else {
                    t
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        None => format!(
            "@{} rev={} state=closed agent=- attention=- result_bytes=0",
            target.selector,
            target.since.unwrap_or(0)
        ),
    }
}

fn gone_target(e: &io::Error, targets: &[Target], live: &[usize]) -> Option<usize> {
    let refusal = e.to_string();
    let selector = refusal.strip_prefix("ERR no such session @")?;
    live.iter()
        .copied()
        .find(|&i| targets[i].selector == selector)
}

/// One leg's request: each target with its edges, and under `facts=1` the request's own
/// `fact_since=` for the targets whose element cannot carry one before the baseline named their
/// `rev` (every such target's edge is the request's, [`run_await_task`]).
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
    Failed(io::Error),
    Code(ExitCode),
}

fn malformed(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("malformed await task reply: {what}"),
    )
}

fn run_group(
    index: usize,
    mut group: Group,
    (combine, facts): (Combine, bool),
    until: Option<Instant>,
    follow: redial::Redial,
    legs: &Legs,
    tx: &mpsc::Sender<Report>,
) -> End {
    let n = group.targets.len();
    let mut rows: Vec<Option<String>> = vec![None; n];
    let mut gone = vec![false; n];
    let mut baseline_read = false;
    let mut unserved_since: Option<Instant> = None;
    loop {
        let live: Vec<usize> = (0..n).filter(|&i| !gone[i]).collect();
        if live.is_empty() {
            return End::Answered(rows.into_iter().flatten().collect());
        }
        let wait = if !baseline_read || (combine == Combine::Any && gone.contains(&true)) {
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
        let (answered, latest) = match (legs.send)(&group.sock, &request, wait) {
            Leg::Answered(latest) => (true, latest),
            Leg::TimedOut(latest) => (false, latest),
            Leg::Failed(e) => {
                let Some(i) = baseline_read
                    .then(|| gone_target(&e, &group.targets, &live))
                    .flatten()
                else {
                    return End::Failed(e);
                };
                gone[i] = true;
                rows[i] = Some(closed_row(rows[i].as_deref(), &group.targets[i]));
                continue;
            }
            Leg::Code(code) => return End::Code(code),
            Leg::Again(e) => {
                let now = Instant::now();
                let since = *unserved_since.get_or_insert(now);
                if until.is_some_and(|until| now >= until) {
                    return if baseline_read {
                        End::TimedOut(rows.into_iter().flatten().collect())
                    } else {
                        End::Failed(e)
                    };
                }
                let (after, most) = legs.again;
                if now.saturating_duration_since(since) >= most {
                    return End::Failed(e);
                }
                std::thread::sleep(until.map_or(after, |until| {
                    until.saturating_duration_since(now).min(after)
                }));
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
                        let _ = stderr_line(&redial::gone_note(
                            server,
                            waited,
                            by_deadline,
                            policy.unfollowed(),
                        ));
                        return End::Code(ExitCode::from(redial::EXIT_REPLACED));
                    }
                    redial::Hangup::StillServing | redial::Hangup::Unknown => {
                        return End::Failed(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "server closed the connection without responding",
                        ));
                    }
                }
            }
        };
        unserved_since = None;
        if latest.len() != live.len() {
            return End::Failed(malformed(&format!(
                "{} rows for {} targets",
                latest.len(),
                live.len()
            )));
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
            gone[i] = row_closed(&row);
            rows[i] = Some(row);
        }
        baseline_read = true;
        let latest: Vec<String> = rows.iter().flatten().cloned().collect();
        if answered || (combine == Combine::Any && gone.contains(&true)) || !gone.contains(&false) {
            return End::Answered(latest);
        }
        if until.is_some_and(|until| Instant::now() >= until) {
            return End::TimedOut(latest);
        }
        let _ = tx.send(Report::Rows(index, latest));
    }
}

enum Waited {
    Rows { rows: Vec<String>, answered: bool },
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
    let (tx, rx) = mpsc::channel();
    let count = groups.len();
    for (index, group) in groups.into_iter().enumerate() {
        let (tx, follow, legs) = (tx.clone(), follow.clone(), legs.clone());
        std::thread::Builder::new()
            .name("aterm-ctl-await-task".into())
            .spawn(move || {
                let end = run_group(index, group, (combine, facts), until, follow, &legs, &tx);
                let _ = tx.send(Report::End(index, end));
            })?;
    }
    drop(tx);
    let mut rows: Vec<Vec<String>> = vec![Vec::new(); count];
    let mut ended = vec![false; count];
    let mut answered = vec![false; count];
    let done = |answered: &[bool]| match combine {
        Combine::Any => answered.iter().any(|a| *a),
        Combine::All => answered.iter().all(|a| *a),
    };
    while let Ok(report) = rx.recv() {
        match report {
            Report::Rows(index, latest) => rows[index] = latest,
            Report::End(index, end) => {
                ended[index] = true;
                match end {
                    End::Answered(latest) => {
                        rows[index] = latest;
                        answered[index] = true;
                    }
                    End::TimedOut(latest) => rows[index] = latest,
                    End::Failed(e) => return Err(e),
                    End::Code(code) => return Ok(Waited::Code(code)),
                }
                if done(&answered) || ended.iter().all(|e| *e) {
                    break;
                }
            }
        }
    }
    Ok(Waited::Rows {
        rows: rows.into_iter().flatten().collect(),
        answered: done(&answered),
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
    let until = total.map(|t| Instant::now() + t);
    let dir = socket_dir();
    let targets = request
        .targets
        .iter()
        .map(|t| Target {
            selector: t.selector.clone(),
            since: t.since.or(request.since),
            fact_since: t.fact_since.or(request.fact_since),
        })
        .collect();
    let groups = group(targets, pinned, default, dir.as_deref())?;
    let legs = Legs {
        max: LEG_MAX,
        send: Arc::new(socket_leg),
        again: (AGAIN_AFTER, AGAIN_FOR),
    };
    print_waited(wait_task_groups(
        groups,
        request.combine,
        request.facts,
        until,
        follow,
        &legs,
    )?)
}

/// `await task …` over the fleet ([`crate::remote`], [`DRIVEN_LEG_MAX`], [`DEFAULT_TOTAL`]).
pub(crate) fn run_driven(
    parts: &[String],
    total: Option<Option<Duration>>,
    host: &str,
    drive: crate::FleetDriveFn,
) -> io::Result<ExitCode> {
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
    let total = match (request.timeout, total) {
        (Some(verb), _) => Some(verb),
        (None, Some(flag)) => flag,
        (None, None) => Some(DEFAULT_TOTAL),
    };
    let until = total.map(|t| Instant::now() + t);
    let targets = request
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
        send: Arc::new(move |_: &str, request: &str, wait: Duration| {
            driven_leg(drive, &host, request, wait)
        }),
        again: (AGAIN_AFTER, AGAIN_FOR),
    };
    let groups = vec![Group {
        sock: String::new(),
        targets,
    }];
    print_waited(wait_task_groups(
        groups,
        request.combine,
        request.facts,
        until,
        &redial::Redial::none(),
        &legs,
    )?)
}

fn print_waited(waited: Waited) -> io::Result<ExitCode> {
    let (rows, answered) = match waited {
        Waited::Rows { rows, answered } => (rows, answered),
        Waited::Code(code) => return Ok(code),
    };
    let stdout = stdout_handle();
    let mut out = StdoutSink::new(stdout.lock());
    for row in &rows {
        io::Write::write_all(&mut out, row.as_bytes())?;
        io::Write::write_all(&mut out, b"\n")?;
    }
    drop(out);
    if answered {
        Ok(ExitCode::SUCCESS)
    } else {
        stderr_line("OK timeout")?;
        Ok(ExitCode::from(EXIT_TIMEOUT))
    }
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
        let row = "@s-a rev=7 state=done agent=- attention=- result_bytes=1";
        assert_eq!(row_rev(row), Some(7));
        assert_eq!(row_rev("garbage"), None);
        assert_eq!(row_sid(row), Some("s-a"));
        assert_eq!(row_sid("@3 rev=1"), None, "a row names a global sid");
        assert!(!row_closed(row));
        assert_eq!(
            closed_row(Some(row), &targets[0]),
            "@s-a rev=7 state=closed agent=- attention=- result_bytes=1"
        );
        assert!(row_closed(&closed_row(None, &targets[0])));
        let refusal = io::Error::other("ERR no such session @s-a");
        assert_eq!(gone_target(&refusal, &targets, &[0, 1]), Some(0));
        assert_eq!(gone_target(&refusal, &targets, &[1]), None, "not live");
        let other = io::Error::other("ERR denied");
        assert_eq!(gone_target(&other, &targets, &[0, 1]), None);
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
            Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
            Err(e) => panic!("{e}"),
        }
    }

    /// `facts=1` rides every leg, and the re-arm carries each target's fact edge as its baseline
    /// row read it (`fact_rev=`, 0 for a record whose word was never edited), so a word noted
    /// between legs answers the next one. NEGATIVE CONTROL: without `facts=1` no leg asks for it
    /// and no element carries a fact edge.
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
                "await task @s-a:1,@s-b:1,@s-c:1 all timeout=600\n",
                refused("ERR no such session @s-b"),
            ),
            (
                x,
                "await task @s-a:1,@s-c:1 all timeout=600\n",
                answered(&[
                    "@s-a rev=2 state=done agent=- attention=- result_bytes=9",
                    "@s-c rev=3 state=exited agent=- attention=- result_bytes=0",
                ]),
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
            refused("ERR denied: node ab12cd34 is not one this Mac lets drive it"),
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
        let dir = std::env::temp_dir().join(format!("aterm-await-task-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("graph"));
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
        let pinned = group(targets.clone(), true, "/tmp/pinned.sock", Some(&dir)).unwrap();
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned[0].targets.len(), 2);
        let err = group(targets, false, "/tmp/default.sock", Some(&dir)).unwrap_err();
        assert!(
            err.to_string().contains("ERR no such session @s-abc"),
            "{err}"
        );
        let local = group(
            vec![Target {
                selector: ".".into(),
                since: None,
                fact_since: None,
            }],
            false,
            "/tmp/default.sock",
            Some(&dir),
        )
        .unwrap();
        assert_eq!(local[0].sock, "/tmp/default.sock");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
