// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl await task`: a manager's ONE wait over its workers, wherever on
//! this machine they run (`docs/DESIGN-sessions-as-subagents-2026-10-15.md`
//! §2.2).
//!
//! ```text
//! aterm ctl await task @a,@b,@c [any|all] [since=<rev>] [timeout=<ms>]
//! aterm ctl @a await task [since=<rev>] [timeout=<ms>]
//! ```
//!
//! The server answers for the sessions of its own instance; this client:
//!
//! 1. GROUPS the targets by the instance that hosts them — a global sid by its
//!    graph entry (`<dir>/graph/<sid>`, the rendezvous `ls` walks), a local id
//!    or `@.` on the instance the call resolves to, everything on the one
//!    instance `--sock`/`--pid` names (so a sibling's sid there is that
//!    instance's `ERR no such session`: the list form is never relayed);
//! 2. reads each target once (a `timeout=0` leg): the server answers a row per
//!    target in request order, so row `i` names target `i`'s global sid, which
//!    every later leg uses, and its rev, the BASELINE every later leg waits
//!    past (`@<sid>:<rev>`) — the wait is one edge however many legs it takes:
//!    news reached during a re-arm is still news, and news a target already
//!    had at the start is not (a `since=` the caller gives is the baseline
//!    instead: `since=0` takes any news);
//! 3. asks every instance AT ONCE, one thread each, and combines them: `any`
//!    answers with the first instance that has news, `all` once every one has;
//! 4. RE-ARMS each instance past the server's 600 s bound until its own
//!    deadline — the verb's `timeout=<ms>`, else `--timeout`, else
//!    [`DEFAULT_TOTAL`] — and FOLLOWS A SELF-UPDATE: a leg cut before its reply
//!    by a server that went away is asked again of its successor
//!    ([`redial::Redial::host`]), with the same baselines;
//! 5. KEEPS A WORKER THAT IS GONE: a row that says `state=closed`, or a leg
//!    refused `ERR no such session` for a target already read (its session
//!    closed between two legs, or the update's successor did not carry it),
//!    is that target's news — its row stands, as `closed`, and it leaves every
//!    later leg — so a person closing one finished worker's tab never ends a
//!    manager's wait on the others.
//!
//! It prints every target's row (`@<sid> rev= state= agent= attention=
//! result_bytes=`), instance by instance, and exits 0 when the wait was
//! answered, 124 at the deadline (`OK timeout` on stderr), 75 when an
//! instance went away with no successor.

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

/// The whole wait when neither the verb's `timeout=` nor `--timeout` names one:
/// a worker's task is minutes to an hour, and the manager runs this as a
/// background command whose exit wakes it.
pub(crate) const DEFAULT_TOTAL: Duration = Duration::from_secs(3600);

/// The longest one leg asks for: the server's clamp on every `await`
/// (`aterm_control::wire::WAIT_MAX_MS`). A server with a lower clamp answers
/// its own timeout sooner, and the leg is simply re-armed.
pub(crate) const LEG_MAX: Duration = Duration::from_secs(600);

/// What a leg's socket may take beyond the wait it asked for: the reply's own
/// write, and a loaded machine.
pub(crate) const LEG_SLACK: Duration = Duration::from_secs(30);

/// The client's usage line for this form.
const USAGE: &str = "ERR usage: aterm ctl [@<sel>] await task [@<sel>[:<rev>][,@<sel>...]] \
                     [any|all] [since=<rev>] [timeout=<ms>]";

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
    fn word(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::All => "all",
        }
    }
}

/// One target as the caller wrote it: the selector body, and its own since.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    selector: String,
    since: Option<u64>,
}

/// The request, parsed client-side (the server's grammar: the same words).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Request {
    targets: Vec<Target>,
    combine: Combine,
    since: Option<u64>,
    timeout: Option<Duration>,
}

fn usage() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, USAGE)
}

/// Parse `[@<sel>] await task <words…>`. A leading selector is the one target;
/// otherwise the list names them; neither is the session the call resolves to
/// (`@.`).
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
    if request.targets.is_empty() {
        request.targets.push(Target {
            selector: ".".to_string(),
            since: None,
        });
    }
    // A target named twice is waited on once.
    let mut seen: Vec<String> = Vec::new();
    request.targets.retain(|t| {
        let fresh = !seen.contains(&t.selector);
        seen.push(t.selector.clone());
        fresh
    });
    Ok(request)
}

/// One list element, `@<sel>` or `@<sel>:<rev>`. A selector that names
/// nothing (`@`, `@:3`, an empty `@$W` in `@$W1,@$W2`) is refused
/// ([`aterm_types::control_verbs::EMPTY_SELECTOR`]), never waited on as the
/// session the call resolves to.
fn target(element: &str) -> io::Result<Target> {
    let body = element.strip_prefix('@').ok_or_else(usage)?;
    let (selector, since) = match body.split_once(':') {
        Some((selector, rev)) => (selector, Some(rev.parse().map_err(|_| usage())?)),
        None => (body, None),
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
    })
}

/// The global sid a selector body names, when it names one (`s-<hex>`).
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

/// Group `targets` by the instance hosting each: `pinned` (`--sock`/`--pid`)
/// takes them all; else a global sid goes where its graph entry points, and
/// anything else to `default`, the instance the call resolves to. A sid no
/// live instance hosts is the caller's error, said before anything is sent.
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

/// How one leg ended.
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
    /// An `ERR` line, or a reply this client cannot read: the run's end.
    Failed(io::Error),
    /// A leg over a drive the peer did not serve, which the same leg a
    /// moment later may be: its share for this machine's address full,
    /// nothing listening at its port (an aterm restarting, its listener
    /// rebinding), or the connection cut before the rows. Asked again within
    /// the wait's deadline ([`Legs::again`]).
    Again(io::Error),
    /// A connect phase that already said why on stderr.
    Code(ExitCode),
}

/// One leg's round trip: `(sock, request line, the wait it asks for)`.
type LegFn = dyn Fn(&str, &str, Duration) -> Leg + Send + Sync;

/// How a group's legs reach their instance: the longest wait one leg asks
/// for, the round trip — the socket's ([`socket_leg`]), a drive's
/// ([`driven_leg`]), or a test's script — and how a leg its peer did not
/// serve is asked again ([`Leg::Again`]): after `again.0`, for at most
/// `again.1` of such legs in a row.
#[derive(Clone)]
struct Legs {
    max: Duration,
    send: Arc<LegFn>,
    again: (Duration, Duration),
}

/// Send one `await task` leg to `sock` and read its rows.
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

/// The longest one leg over a drive asks for (`aterm ctl --host`,
/// [`crate::remote`]): the peer cuts a drive connection at 30 s, so each leg
/// is a fresh connection well inside it.
const DRIVEN_LEG_MAX: Duration = Duration::from_secs(20);

/// What a leg's drive connection may take beyond the wait it asks for: the
/// dial, the proof and the reply, inside the peer's 30 s cut.
const DRIVEN_SLACK: Duration = Duration::from_secs(8);

/// How long a group waits before it asks again a leg the peer did not serve
/// ([`Leg::Again`], [`Legs::again`]).
const AGAIN_AFTER: Duration = Duration::from_secs(1);

/// The longest a group asks again, in a row, legs the peer does not serve
/// ([`Leg::Again`], [`Legs::again`]): a peer that has served none for a
/// minute is not busy or restarting, and the wait ends with its refusal.
const AGAIN_FOR: Duration = Duration::from_secs(60);

/// Send one `await task` leg over a fresh drive connection to `host` and read
/// its rows. A leg the peer did not serve — a dial it refused for its share,
/// one nothing answered, a connection cut before the rows — is
/// [`Leg::Again`]: on another Mac there is no successor to follow, and the
/// worker it waits on lives on through its aterm's restart or update.
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

/// One leg's answer off `reader`: `hung_up` is a connection that closed
/// before the whole of it.
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
    // Keep this reader's existing normalization of trailing CRs as well as
    // the collector's one wire CRLF; every row is charged before it is kept.
    for row in &mut rows {
        row.truncate(row.trim_end_matches(['\r', '\n']).len());
    }
    if tail.split_whitespace().nth(1) == Some("status=timeout") {
        Leg::TimedOut(rows)
    } else {
        Leg::Answered(rows)
    }
}

/// The value of a row's `key=`.
fn row_field<'a>(row: &'a str, key: &str) -> Option<&'a str> {
    row.split_whitespace()
        .find_map(|t| t.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
}

/// The `rev=` of a row.
fn row_rev(row: &str) -> Option<u64> {
    row_field(row, "rev").and_then(|rev| rev.parse().ok())
}

/// The global sid a row names (`@<sid> …`).
fn row_sid(row: &str) -> Option<&str> {
    row.split_whitespace()
        .next()
        .and_then(|head| head.strip_prefix('@'))
        .and_then(global_sid)
}

/// Whether a row says its session closed.
fn row_closed(row: &str) -> bool {
    row_field(row, "state") == Some("closed")
}

/// The row a target that is gone stands as: its last row with `state=closed`,
/// or, never having had one, its selector and its baseline.
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

/// Which of `live` a leg's refusal says is gone: `ERR no such session
/// @<sid>` for a target this wait already read.
fn gone_target(e: &io::Error, targets: &[Target], live: &[usize]) -> Option<usize> {
    let refusal = e.to_string();
    let selector = refusal.strip_prefix("ERR no such session @")?;
    live.iter()
        .copied()
        .find(|&i| targets[i].selector == selector)
}

/// The request line for `targets` (each with its since) on one leg.
fn leg_request<'a>(
    targets: impl Iterator<Item = &'a Target>,
    combine: Combine,
    wait: Duration,
) -> String {
    let list = targets
        .map(|t| match t.since {
            Some(rev) => format!("@{}:{rev}", t.selector),
            None => format!("@{}", t.selector),
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "await task {list} {} timeout={}\n",
        combine.word(),
        wait.as_millis()
    )
}

/// What a group's thread tells the run.
enum Report {
    /// The rows as last read (the baseline, or a re-arm's timeout).
    Rows(usize, Vec<String>),
    /// The group's wait ended.
    End(usize, End),
}

/// How a group's wait ended.
enum End {
    Answered(Vec<String>),
    TimedOut(Vec<String>),
    Failed(io::Error),
    Code(ExitCode),
}

/// A reply this client cannot read.
fn malformed(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("malformed await task reply: {what}"),
    )
}

/// Run one group to its end: the baseline leg, then legs until news, the
/// deadline, or a failure; a hang-up follows the successor, and a target that
/// is gone stands as its `closed` row ([the module](self), 5).
fn run_group(
    index: usize,
    mut group: Group,
    combine: Combine,
    until: Option<Instant>,
    follow: redial::Redial,
    legs: &Legs,
    tx: &mpsc::Sender<Report>,
) -> End {
    let n = group.targets.len();
    // Each target's newest row, and whether it is GONE: a `closed` row stands
    // for the rest of the wait, and the target leaves every later leg.
    let mut rows: Vec<Option<String>> = vec![None; n];
    let mut gone = vec![false; n];
    let mut baseline_read = false;
    // When the legs the peer has not served began, while none has been since.
    let mut unserved_since: Option<Instant> = None;
    loop {
        let live: Vec<usize> = (0..n).filter(|&i| !gone[i]).collect();
        if live.is_empty() {
            // Every target closed: each is news, so `all` has its answer too.
            return End::Answered(rows.into_iter().flatten().collect());
        }
        // A gone target is `any`'s answer: one zero leg reads the others'
        // rows as they stand, and the group answers with them.
        let wait = if !baseline_read || (combine == Combine::Any && gone.contains(&true)) {
            Duration::ZERO
        } else {
            until.map_or(legs.max, |until| {
                until
                    .saturating_duration_since(Instant::now())
                    .min(legs.max)
            })
        };
        let request = leg_request(live.iter().map(|&i| &group.targets[i]), combine, wait);
        let (answered, latest) = match (legs.send)(&group.sock, &request, wait) {
            Leg::Answered(latest) => (true, latest),
            Leg::TimedOut(latest) => (false, latest),
            Leg::Failed(e) => {
                // A target this wait already read that its instance no longer
                // hosts — its session closed between two legs, or the
                // successor of a self-update did not carry it — is its news,
                // as a close inside a leg is. Before the baseline no target
                // was read yet, and the refusal is the run's.
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
                // A group of global sids follows the instance that
                // republishes one's graph entry (every target is one past the
                // baseline); before it, local ids follow the call's own policy
                // (its session's entry, or the alias).
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
                            "{} during the wait; waiting on its successor",
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
        // One row per target sent, in its order (the server's contract).
        if latest.len() != live.len() {
            return End::Failed(malformed(&format!(
                "{} rows for {} targets",
                latest.len(),
                live.len()
            )));
        }
        for (&i, row) in live.iter().zip(latest) {
            if !baseline_read {
                // THE BASELINE: the sid every later leg names (`@3`, `@.` and
                // a sid's own spelling alike), and the rev it waits past.
                let Some(sid) = row_sid(&row) else {
                    return End::Failed(malformed(&row));
                };
                let target = &mut group.targets[i];
                target.selector = sid.to_string();
                target.since = target.since.or_else(|| row_rev(&row));
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

/// How the whole wait ended: every target's rows, instance by instance, and
/// whether the combine was answered; or an exit a connect phase or a gone
/// instance already explained.
enum Waited {
    Rows { rows: Vec<String>, answered: bool },
    Code(ExitCode),
}

/// Ask every group at once, one thread each, and combine their ends.
fn wait_task_groups(
    groups: Vec<Group>,
    combine: Combine,
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
                let end = run_group(index, group, combine, until, follow, &legs, &tx);
                let _ = tx.send(Report::End(index, end));
            })?;
    }
    drop(tx);
    // Every group's latest rows, and which have answered; the run ends at the
    // combine's answer, or once every group has ended (its deadline). A group
    // still parked when `any` answers is left to the process's exit.
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

/// `[@<sel>] await task …`: the whole client wait ([the module](self)).
/// `pinned` is a `--sock`/`--pid` call, `default` the socket the call
/// resolves to, `total` the explicit `--timeout` when one was given (`Some(None)`:
/// `--timeout 0`, no deadline).
pub(crate) fn run_await_task(
    parts: &[String],
    pinned: bool,
    default: &str,
    follow: &redial::Redial,
    total: Option<Option<Duration>>,
) -> io::Result<ExitCode> {
    let request = parse(parts)?;
    // The verb's `timeout=` is the whole wait; then `--timeout`; then the default.
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
        until,
        follow,
        &legs,
    )?)
}

/// `await task …` over the fleet (`aterm ctl --host <host>`,
/// [`crate::remote`]): every target a worker of that Mac named by its sid, the
/// one group asked in legs of at most [`DRIVEN_LEG_MAX`], each a fresh drive
/// connection `drive` dials, up to the same deadline as on one Mac (the
/// verb's `timeout=`, else `--timeout`, else [`DEFAULT_TOTAL`]), printed as
/// on one Mac.
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
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "await task over the fleet names each worker by its sid (@s-…, as `aterm ctl \
                 survey --host <h>` shows it): `@{}` names a session of this Mac",
                local.selector
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
        until,
        &redial::Redial::none(),
        &legs,
    )?)
}

/// Print a wait's rows, and exit 0 when it was answered, 124 (`OK timeout`
/// on stderr) at its deadline.
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
                        since: None
                    },
                    Target {
                        selector: "2".into(),
                        since: Some(5)
                    },
                ],
                combine: Combine::All,
                since: Some(3),
                timeout: Some(Duration::from_millis(1500)),
            },
            "a target named twice is waited on once"
        );
        let single = parse(&parts("@s-b2 await task since=4 timeout 90000")).unwrap();
        assert_eq!(
            single.targets,
            vec![Target {
                selector: "s-b2".into(),
                since: None
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
                since: None
            }]
        );
        assert_eq!(bare.combine, Combine::Any);
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
        ] {
            assert!(parse(&parts(bad)).is_err(), "{bad:?}");
        }
        // An element that names nothing (`@$W` with `$W` empty) is the empty
        // selector refusal, never the session the call resolves to; the same
        // list with `@.` written out (the negative control) is three targets.
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
            },
            Target {
                selector: "1".into(),
                since: None,
            },
        ];
        assert_eq!(
            leg_request(targets.iter(), Combine::All, Duration::from_secs(600)),
            "await task @s-a:3,@1 all timeout=600000\n"
        );
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

    /// A leg retains no more than its aggregate wire budget, on the exact
    /// boundary too. A truncated reply is a retryable cut, while malformed or
    /// over-budget data is a failed answer, never a retry.
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

    /// What one scripted leg answers, run when the leg is sent.
    type Reply = Box<dyn FnOnce() -> Leg + Send>;
    /// A scripted leg: the request it must be, and its answer.
    type Step = (String, Reply);

    /// One scripted instance per socket: each leg must be the request the
    /// script names next, and answers what its step returns. The script's
    /// lock is released before a step runs, so a step that blocks holds up
    /// only its own group.
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
        match wait_task_groups(groups, combine, until, &redial::Redial::none(), legs) {
            Ok(Waited::Rows { rows, answered }) => (rows, answered),
            Ok(Waited::Code(code)) => panic!("an exit code: {code:?}"),
            Err(e) => panic!("{e}"),
        }
    }

    /// THE RE-ARM, AND A WORKER GONE BETWEEN TWO LEGS (`all`): the baseline
    /// leg names every target's sid and rev; a server timeout re-arms past
    /// those same revs (not the newer rev a target has since shown); a target
    /// the instance no longer hosts stands as its `closed` row and leaves the
    /// next leg; and the wait answers once the rest have news.
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

        // A closed row inside a leg is the same news: it leaves the next leg,
        // and with every target gone the group has its answer.
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

        // NEGATIVE CONTROL: before the baseline nothing was read, so a target
        // the instance does not host is the run's refusal, not a closed row.
        let legs = scripted(vec![(
            x,
            "await task @s-d all timeout=0\n",
            refused("ERR no such session @s-d"),
        )]);
        let refusal = wait_task_groups(
            one_group(x, &["s-d"]),
            Combine::All,
            None,
            &redial::Redial::none(),
            &legs,
        );
        assert!(
            matches!(&refusal, Err(e) if e.to_string() == "ERR no such session @s-d"),
            "the refusal ends the run"
        );
    }

    /// A LEG THE PEER DID NOT SERVE IS ASKED AGAIN (a drive's `Leg::Again`:
    /// its share for this address full, its listener restarting, a cut), the
    /// baseline and a re-arm alike, and the wait goes on to the worker's
    /// news. NEGATIVE CONTROLS: a refusal the peer said (`ERR`) ends the run,
    /// as on one Mac; and legs unserved past their bound in a row end it with
    /// the last one's words.
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

    /// TWO INSTANCES: `all` answers once each has news, whichever leg each
    /// took; `any` answers with the first instance's news and the other's rows
    /// as last read, while that one is still parked; and a deadline that
    /// passed answers every row unanswered.
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

        // `any`: y parks after its baseline (it says so first), x answers.
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
                    // A hang detector, never a budget: y always arms.
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

        // A deadline already behind: the baseline's rows, unanswered.
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

    /// `--sock`/`--pid` take every target; a local id goes where the call
    /// resolves; a global sid no instance hosts is refused before a byte is
    /// sent.
    #[test]
    fn targets_group_by_the_instance_that_hosts_them() {
        let dir = std::env::temp_dir().join(format!("aterm-await-task-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("graph"));
        let targets = vec![
            Target {
                selector: "s-abc".into(),
                since: None,
            },
            Target {
                selector: "3".into(),
                since: None,
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
