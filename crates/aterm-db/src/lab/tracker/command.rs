// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm goals` (docs/DESIGN-lab-tracker-2026-11-14.md §1, §2; package TK-12): the four
//! commands an agent types, the same on every machine that holds Commons. Each record they
//! make says who made it (the session and program aterm read from the process tree), why
//! (`--reason`) and on what evidence (`--evidence`, repeated).
//!
//! - `claim <package>` takes a ready package and `done <package>` ends a running one. Each is
//!   one write of the package's row through the outbox of `lab/packages`
//!   ([`crate::file::PACKAGES`]), exactly as `aterm file set` makes it: read against this
//!   machine's snapshot, which the command first has a materializer pass bring up to what the
//!   machine holds, queued as one intent, and offered to the publisher for its bound. The
//!   answer is `published <operation>`; `queued (not claimed yet) <id>: <why>` (a `done`:
//!   `queued (not done yet)`), where the intent waits in this machine's outbox for a later
//!   pass and the package is not claimed (or done) until one publishes it; or `held <id>:
//!   <why>` (exit 4), where the row changed between the read and the write (another agent
//!   claimed it first) and nothing was overwritten.
//! - `note <id> <text>` and `measure <metric> <value>` append one entry to the tracker's `note`
//!   and `measure` logs ([`super::NOTE`], [`super::MEASURE`]). A log entry is this machine's own
//!   statement at its own key, appended by its own program under its writer lock
//!   (`att::Handle::append_log`) and never through an outbox (`crate::file::Type::edited`), so
//!   these two answer `published <key>` or say why nothing was written; neither is ever queued.
//!
//! - `claim --next [--lane <lane>]` takes the package [`super::lease`] gives this agent: the one
//!   it already holds, else one of the first ready packages nobody holds, by a hash of the
//!   agent; where its claim of a package stands contested and is not the one that keeps it, it
//!   first releases that package to the claim that does. `release <package>` gives this agent's
//!   claim back: the package is ready again, or held by the other claim where two stood.
//!
//! A claim reads the package's settled state ([`super::state`]) and its lease: it takes a ready
//! package nobody holds, or a running one whose every claim lapsed, and names this machine in
//! the claim (`by.machine`), whose renewals keep it ([`super::lease::renew`]).
//!
//! A claim records the digest of the exit it claimed ([`frozen::digest`]); what holds that exit
//! after it is the frozen rule's (TK-15).
//!
//! What this module leaves to the packages that follow it: the exit test `done` runs (TK-22).
//! Until then `done` refuses a package it does not read as running, and cites evidence the
//! writer names.
//!
//! No build registers `lab/tracker` before go-live (TK-32), and an append declares the table it
//! writes, so the front door ([`main_entry`]) refuses every command and writes nothing until
//! this build registers the family ([`live`]).

use super::{
    APPLICATION, MEASURE, NOTE, ROLE, frozen,
    lease::{self, Leases, same_agent},
    mapped,
    state::States,
    tables,
};
use crate::{
    alab::journal::Spool,
    att::{self, Handle},
    file::{
        self, materialize, publish,
        write::{self, Refusal, Said as Wrote},
    },
    generation,
    said::Said,
    typed::{
        keys,
        values::{Evidence, Principal},
    },
};
use aterm_file::exit;
use aterm_json::{Map, Value, json};
use aterm_secrets::client::Paths;
use std::{
    ffi::OsString,
    io::Write as _,
    path::Path,
    process::ExitCode,
    time::{Duration, Instant},
};

const VERB: &str = "aterm goals";

/// What `aterm goals --help` prints.
pub const USAGE: &str = "\
aterm goals claim <package>
aterm goals claim --next [--lane <lane>]
aterm goals release <package>
aterm goals note <id> <text> [--on <table>]
aterm goals done <package> --evidence <piece>...
aterm goals measure <metric> <value> [--source <command>] [--commit <hash>] [--class <word>]

Every command takes --reason <line>, --evidence <piece> (repeated), --by <word> and --json.
A piece of evidence is commit:<repo>@<commit>, path:<repo>:<path>, measure:<metric>=<value>,
note:<text> or an https:// URL.

claim, release and done answer `published <operation>`, `queued (not claimed yet) <id>: <why>`
(release: `queued (not released yet)`, done: `queued (not done yet)`; the write waits in this
machine's outbox) or `held <id>: <why>` (exit 4: the package changed since it was read, and
nothing was overwritten). claim --next takes the package this agent holds, else one of the
first eight ready packages nobody holds, by a hash of the agent, and first releases a package
whose claim it lost to another made at once; it answers `holds <package>` where it holds one
already. note and measure
append this machine's own log entry and answer `published <key>`. `--` ends the flags: each
word after it is the command's own, as a text that starts with `--` is.
";

/// What one command asks, read from its words alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    pub act: Act,
    /// What the writer says of itself (`--by`): the front door's word where aterm reads no
    /// program for this process ([`crate::said::principal_of`]).
    pub by: Option<String>,
    /// Why (`--reason`), one line.
    pub reason: Option<String>,
    /// On what evidence (`--evidence`, repeated).
    pub evidence: Vec<Evidence>,
    /// Answer as one JSON object (`--json`).
    pub json: bool,
}

/// The command and its own words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// `claim <package>`.
    Claim { package: String },
    /// `claim --next [--lane <lane>]`.
    Next { lane: Option<String> },
    /// `release <package>`.
    Release { package: String },
    /// `done <package>`.
    Done { package: String },
    /// `note <id> <text> [--on <table>]`: `on` is one of [`tables::NOTED`].
    Note {
        on: &'static str,
        about: String,
        text: String,
    },
    /// `measure <metric> <value> [--source <command>] [--commit <hash>] [--class <word>]`.
    Measure {
        metric: String,
        value: String,
        source: Option<String>,
        commit: Option<String>,
        class: Option<String>,
    },
}

fn usage(why: impl Into<String>) -> Refusal {
    Refusal::Usage(why.into())
}

fn problem(why: impl ToString) -> Refusal {
    Refusal::Problem(why.to_string())
}

impl Ask {
    /// Read `words`, the command's after `aterm goals`. Nothing is opened or read: a refusal
    /// here is the same at a terminal and through a pipe.
    ///
    /// # Errors
    /// The words are not a command's: its usage line, for exit [`exit::USAGE`].
    pub fn parse(words: &[String]) -> Result<Self, Refusal> {
        const CLAIM: &str = "claim <package> | claim --next [--lane <lane>]";
        const RELEASE: &str = "release <package>";
        const DONE: &str = "done <package> --evidence <piece>...";
        const NOTE: &str = "note <id> <text> [--on <table>]";
        const MEASURE: &str = "measure <metric> <value> [--source <command>] [--commit <hash>] \
                               [--class <word>]";
        let Some((command, rest)) = words.split_first() else {
            return Err(usage("claim|release|note|done|measure ..."));
        };
        let what = match command.as_str() {
            "claim" => CLAIM,
            "release" => RELEASE,
            "done" => DONE,
            "note" => NOTE,
            "measure" => MEASURE,
            _ => return Err(usage("claim|release|note|done|measure ...")),
        };
        let (mut by, mut reason, mut evidence, mut json) = (None, None, Vec::new(), false);
        let (mut on, mut source, mut commit, mut class) = (None, None, None, None);
        let (mut next, mut lane) = (false, None);
        let mut plain = Vec::new();
        let mut iter = rest.iter();
        let mut flags = true;
        while let Some(word) = iter.next() {
            if !flags {
                plain.push(word.clone());
                continue;
            }
            let mut value = |takes: &str| {
                iter.next()
                    .cloned()
                    .ok_or_else(|| usage(format!("{word} takes {takes}")))
            };
            match (word.as_str(), command.as_str()) {
                ("--by", _) => by = Some(value("one word")?),
                ("--reason", _) => reason = Some(value("one line")?),
                ("--evidence", _) => evidence.push(
                    Evidence::parse(&value("one piece")?)
                        .map_err(|why| usage(format!("--evidence: {why}")))?,
                ),
                ("--json", _) => json = true,
                ("--", _) => flags = false,
                ("--on", "note") => on = Some(value("a table's name")?),
                ("--next", "claim") => next = true,
                ("--lane", "claim") => lane = Some(value("a lane")?),
                ("--source", "measure") => source = Some(value("one command")?),
                ("--commit", "measure") => commit = Some(value("one commit hash")?),
                ("--class", "measure") => class = Some(value("one word")?),
                (flag, _) if flag.starts_with("--") => return Err(usage(what)),
                _ => plain.push(word.clone()),
            }
        }
        if lane.is_some() && !next {
            return Err(usage(what));
        }
        let act = match (command.as_str(), plain.as_slice()) {
            ("claim", []) if next => Act::Next { lane },
            ("claim", [package]) if !next => Act::Claim {
                package: package.clone(),
            },
            ("release", [package]) => Act::Release {
                package: package.clone(),
            },
            ("done", [package]) => {
                if evidence.is_empty() {
                    return Err(usage(format!(
                        "{DONE}: done cites what shows it (a commit, a path, a measurement, a \
                         URL or a note)"
                    )));
                }
                Act::Done {
                    package: package.clone(),
                }
            }
            ("note", [about, text]) => {
                let on = on.as_deref().unwrap_or(tables::PACKAGE.name);
                let Some(on) = tables::NOTED.iter().find(|table| **table == on) else {
                    return Err(usage(format!(
                        "--on takes one of {}",
                        tables::NOTED.join(", ")
                    )));
                };
                Act::Note {
                    on,
                    about: about.clone(),
                    text: text.clone(),
                }
            }
            ("measure", [metric, value]) => Act::Measure {
                metric: metric.clone(),
                value: value.clone(),
                source,
                commit,
                class,
            },
            _ => return Err(usage(what)),
        };
        Ok(Self {
            act,
            by,
            reason,
            evidence,
            json,
        })
    }
}

/// Who runs a command, when, and how long its own publication attempt may take.
#[derive(Clone, Copy)]
pub struct Env<'a> {
    /// Who makes the write, as aterm read it from this process's lineage
    /// ([`crate::said::principal_of`]): each record states it, and a claim names it as holder.
    pub principal: &'a Principal,
    /// The word of the outbox act: `person`, or a program's word.
    pub by: &'a str,
    /// The aterm session it ran in.
    pub session: Option<&'a str>,
    /// This machine's clock, Unix milliseconds.
    pub now_ms: u64,
    /// The bound of `claim` and `done`'s own materializer and publisher passes
    /// ([`publish::INLINE`] at the front door); zero runs neither, so the write is read against
    /// the snapshot as it stands and queued for a later pass.
    pub attempt: Duration,
    /// The journal spool the publisher pass notes what it signed in.
    pub journal: Option<&'a Spool>,
}

/// Whether this build registers `lab/tracker` (`att::FAMILIES`; TK-32): until it does, the
/// front door writes nothing.
#[must_use]
pub fn live() -> bool {
    att::generations()
        .any(|compiled| compiled.table.application == APPLICATION && compiled.table.role == ROLE)
}

/// Run `ask` over `paths`, whose configuration is `config`: what to print, and the exit code.
///
/// # Errors
/// Nothing was written: the words break a rule ([`Refusal::Usage`]), or the write cannot be
/// made here now ([`Refusal::Problem`]).
pub fn run(
    paths: &Paths,
    config: &Path,
    ask: &Ask,
    env: &Env<'_>,
) -> Result<(String, u8), Refusal> {
    let said = Said {
        principal: env.principal.clone(),
        reason: ask.reason.clone(),
        evidence: ask.evidence.clone(),
    };
    said.check().map_err(usage)?;
    match &ask.act {
        Act::Claim { package } => {
            let machine = here(paths)?;
            let (row, rev) = package_row(paths, config, &mapped::key(package), env)?;
            let (_, states, leases) = tracker(paths, env.now_ms)?;
            let state = settled(&states, &row);
            if !leases.free(&row.pk, Some(&state)) {
                let holder = leases
                    .of_package(&row.pk)
                    .and_then(lease::Lease::holder)
                    .map(|claim| format!(", held by {}", Said::by(claim.by.clone()).who()))
                    .unwrap_or_default();
                return Err(problem(format!(
                    "{} is {state}{holder}: a claim takes a ready package",
                    keys::safe_key(package)
                )));
            }
            take(
                paths,
                config,
                (package, &row.pk, rev),
                &said,
                &machine,
                ask,
                env,
            )
        }
        Act::Next { lane } => next(paths, config, lane.as_deref(), &said, ask, env),
        Act::Release { package } => {
            let me = on_machine(&said.principal, &here(paths)?);
            let (row, rev) = package_row(paths, config, &mapped::key(package), env)?;
            let (_, _, leases) = tracker(paths, env.now_ms)?;
            let lease = leases.of_package(&row.pk);
            let claims = || {
                lease
                    .into_iter()
                    .flat_map(|lease| lease.held.iter().chain(&lease.lapsed))
            };
            if !claims().any(|claim| same_agent(&claim.by, &me)) {
                return Err(problem(format!(
                    "{} is not held by {}: a release gives back the releaser's own claim",
                    keys::safe_key(package),
                    said.who()
                )));
            }
            release(
                paths,
                config,
                (package, &row.pk, rev),
                (lease, &me),
                &said,
                ask,
                env,
            )
        }
        Act::Done { package } => {
            let (row, rev) = package_row(paths, config, &mapped::key(package), env)?;
            let state = state_of(&row);
            if state != "running" {
                return Err(problem(format!(
                    "{} is {state}: done ends a running package",
                    keys::safe_key(package)
                )));
            }
            let cells = ["state=done".to_owned()];
            let says = Says {
                command: "done",
                now: "done".to_owned(),
                not_yet: "not done yet",
            };
            set(
                paths,
                config,
                (package, &row.pk, rev),
                &cells,
                &said,
                (ask, &says),
                env,
            )
        }
        Act::Note { on, about, text } => {
            let mut entry = Map::new();
            entry.insert("on".to_owned(), json!(on));
            entry.insert("about".to_owned(), json!(mapped::key(about)));
            entry.insert("text".to_owned(), json!(text));
            if !said.evidence.is_empty() {
                entry.insert("evidence".to_owned(), Value::Array(said.evidence_values()));
            }
            entry.insert("at".to_owned(), json!(env.now_ms));
            let key = append(paths, &NOTE, (on, about), entry, said)?;
            Ok(appended(
                ask,
                &key,
                format!("a note on {on} {}", keys::safe_key(about)),
            ))
        }
        Act::Measure {
            metric,
            value,
            source,
            commit,
            class,
        } => {
            let mut entry = Map::new();
            entry.insert("metric".to_owned(), json!(mapped::key(metric)));
            entry.insert("value".to_owned(), json!(value));
            entry.insert("at".to_owned(), json!(env.now_ms));
            for (column, stated) in [("source", source), ("commit", commit), ("class", class)] {
                if let Some(stated) = stated {
                    entry.insert(column.to_owned(), json!(stated));
                }
            }
            // The sample was taken here, so its machine is this one: the link a comparison of
            // one machine's samples follows (§4.6).
            entry.insert("machine".to_owned(), json!(here(paths)?));
            let key = append(paths, &MEASURE, (tables::METRIC.name, metric), entry, said)?;
            Ok(appended(
                ask,
                &key,
                format!("a sample of {}", keys::safe_key(metric)),
            ))
        }
    }
}

/// The exit of the package keyed `pk` as this machine's table holds it, with the frozen rule
/// applied (`state::read`, `States::over`); none where it has none.
fn claimed_exit(paths: &Paths, pk: &str) -> Result<Option<Value>, Refusal> {
    let handle = crate::att::Handle::at(paths.clone(), &crate::att::TRACKER);
    let (view, states) = super::state::read(&handle).map_err(problem)?;
    let mut packages = view.project(&tables::PACKAGE);
    states.over(&mut packages);
    Ok(packages
        .rows
        .get(pk)
        .and_then(|row| row.get("exit"))
        .filter(|exit| !exit.is_null())
        .cloned())
}

/// The state a package's row states, as this machine's snapshot reads it: a conflict's
/// provisional word where two writers stated one at once.
fn state_of(row: &aterm_file::Row) -> &str {
    row.values
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unread")
}

/// The live row of the package keyed `key` in this machine's snapshot of `lab/packages`, and the
/// snapshot's revision ([`package_rows`]).
fn package_row(
    paths: &Paths,
    config: &Path,
    key: &str,
    env: &Env<'_>,
) -> Result<(aterm_file::Row, u64), Refusal> {
    let (rows, rev) = package_rows(paths, config, env)?;
    rows.into_iter()
        .find(|row| row.pk == key)
        .map(|row| (row, rev))
        .ok_or_else(|| problem(format!("no package {}", keys::safe_key(key))))
}

/// The tracker as this machine holds it, every package's state settled and its lease read at
/// `now_ms` ([`lease::read`]).
fn tracker(paths: &Paths, now_ms: u64) -> Result<(att::View, States, Leases), Refusal> {
    let handle = Handle::at(paths.clone(), att::current(paths, &att::TRACKER));
    lease::read(&handle, now_ms).map_err(problem)
}

/// The state of the package `row` as the state rule settles it, else as its row states it.
fn settled(states: &States, row: &aterm_file::Row) -> String {
    states
        .state(&row.pk)
        .map_or_else(|| state_of(row).to_owned(), str::to_owned)
}

/// Claim the package `id`, whose row is `pk` as revision `rev` of this machine's snapshot showed
/// it, for the writer `said` names, on `machine`: one write of its state and its claim.
fn take(
    paths: &Paths,
    config: &Path,
    seen: (&str, &str, u64),
    said: &Said,
    machine: &str,
    ask: &Ask,
    env: &Env<'_>,
) -> Result<(String, u8), Refusal> {
    // The claim names the machine whose renewals keep it: this one, where the holder's session
    // runs, unless the principal already names one.
    let by = on_machine(&said.principal, machine);
    let mut claim = json!({
        "by": by.to_value(),
        "on": crate::alab::register::day_of(env.now_ms),
        "at": env.now_ms,
    });
    // The test it claims (§4.3): the digest of the package's exit as the table holds it and the
    // frozen rule reads it, where it has one. Not the snapshot's: a face withholds a value its
    // guard refuses to show, and a digest of that names no test.
    if let (Some(exit), Some(object)) = (claimed_exit(paths, seen.1)?, claim.as_object_mut()) {
        object.insert("exit".to_owned(), json!(frozen::digest(&exit)));
    }
    let cells = [
        "state=running".to_owned(),
        format!("claim={}", aterm_json::to_string(&claim).map_err(problem)?),
    ];
    let says = Says {
        command: "claim",
        now: format!("running, held by {}", said.who()),
        not_yet: "not claimed yet",
    };
    set(paths, config, seen, &cells, said, (ask, &says), env)
}

/// Give back this agent's claim of the package `id` (row `pk` at revision `rev`), whose lease is
/// `lease`: where another claim stands with it, that one becomes the cell's one value and the
/// package stays running; else the package is ready again, held by no one.
fn release(
    paths: &Paths,
    config: &Path,
    seen: (&str, &str, u64),
    (lease, me): (Option<&lease::Lease>, &Principal),
    said: &Said,
    ask: &Ask,
    env: &Env<'_>,
) -> Result<(String, u8), Refusal> {
    let keeper = lease.and_then(|lease| lease.held.iter().find(|claim| !same_agent(&claim.by, me)));
    let (cells, now) = match keeper {
        Some(keeper) => (
            vec![format!(
                "claim={}",
                aterm_json::to_string(&keeper.value).map_err(problem)?
            )],
            format!("running, held by {}", Said::by(keeper.by.clone()).who()),
        ),
        None => (
            vec!["state=ready".to_owned(), "claim=".to_owned()],
            "ready".to_owned(),
        ),
    };
    let says = Says {
        command: "release",
        now,
        not_yet: "not released yet",
    };
    set(paths, config, seen, &cells, said, (ask, &says), env)
}

/// `principal` as a claim names it: with the machine its session runs on, `machine`, unless it
/// names one already. A caller is compared with the claims as this, so two writers outside any
/// session on two machines are two agents.
fn on_machine(principal: &Principal, machine: &str) -> Principal {
    let mut by = principal.clone();
    by.machine.get_or_insert_with(|| machine.to_owned());
    by
}

/// `claim --next`: the package this agent holds, else, once each package whose contested claim
/// it lost is released to the claim that keeps it, the package [`Leases::next`] gives it. Each
/// decision is read from the tracker after the snapshot revision its write is compared at, so a
/// record that arrives between them holds the write, never lets it overwrite.
fn next(
    paths: &Paths,
    config: &Path,
    lane: Option<&str>,
    said: &Said,
    ask: &Ask,
    env: &Env<'_>,
) -> Result<(String, u8), Refusal> {
    let machine = here(paths)?;
    let me = on_machine(&said.principal, &machine);
    // A claim of this agent's still waiting in this machine's outbox is its answer: a second
    // pick would claim a second package once both are published.
    if let Some((id, pk)) = pending(paths, &me) {
        let text = if ask.json {
            format!(
                "{}\n",
                json!({"command": "claim", "package": pk, "state": "queued", "id": id})
            )
        } else {
            format!(
                "queued (not claimed yet) {id}: this agent's claim of {pk} waits in this machine's outbox\n"
            )
        };
        return Ok((text, exit::OK));
    }
    let mut text = String::new();
    let mut released: Vec<String> = Vec::new();
    loop {
        let (rows, rev) = package_rows(paths, config, env)?;
        let (view, states, leases) = tracker(paths, env.now_ms)?;
        let mine = leases.held_by(&me);
        if let Some((pk, _)) = mine.iter().find(|(_, keeps)| *keeps) {
            let id = shown_id(&rows, pk);
            if ask.json {
                text.push_str(&format!(
                    "{}\n",
                    json!({"command": "claim", "package": id, "state": "holds"})
                ));
            } else {
                text.push_str(&format!(
                    "holds {id}: it is running, held by {}\n",
                    said.who()
                ));
            }
            return Ok((text, exit::OK));
        }
        // A contested claim this agent lost goes back to the claim that keeps the package,
        // one a pass, each read again after the last was written.
        if let Some((pk, _)) = mine
            .iter()
            .find(|(pk, _)| !released.iter().any(|done| done == pk))
        {
            let id = shown_id(&rows, pk);
            let (wrote, code) = release(
                paths,
                config,
                (&id, pk, rev),
                (leases.of_package(pk), &me),
                said,
                ask,
                env,
            )?;
            text.push_str(&wrote);
            if code != exit::OK {
                return Ok((text, code));
            }
            released.push((*pk).to_owned());
            continue;
        }
        let packages = view.project(&tables::PACKAGE);
        let Some(pk) = leases.next(&states, &packages, &me, lane) else {
            let within = lane.map(|lane| format!(" in lane {}", keys::safe_key(lane)));
            let none = format!(
                "no ready package{} that nobody holds",
                within.unwrap_or_default()
            );
            // Where a release was written, the answer says so: it is no refusal.
            if text.is_empty() {
                return Err(problem(none));
            }
            if ask.json {
                text.push_str(&format!(
                    "{}\n",
                    json!({"command": "claim", "state": "none", "why": none})
                ));
            } else {
                text.push_str(&none);
                text.push('\n');
            }
            return Ok((text, exit::PROBLEMS));
        };
        let id = shown_id(&rows, &pk);
        let (claimed, code) = take(paths, config, (&id, &pk, rev), said, &machine, ask, env)?;
        text.push_str(&claimed);
        return Ok((text, code));
    }
}

/// This agent's claim waiting in this machine's outbox of `lab/packages`, where one does: the
/// intent's id and the package's key. A release that writes another's claim back is none.
fn pending(paths: &Paths, me: &Principal) -> Option<(String, String)> {
    let name = file::PACKAGES.file();
    let outbox = aterm_file::File::at(&file::root(&generation::original(paths)), &name).outbox();
    outbox.queued.iter().find_map(|intent| {
        intent.edits.iter().find_map(|edit| {
            let by = edit.value.get("by")?;
            (edit.column == "claim"
                && Principal::from_value(by).is_ok_and(|by| same_agent(&by, me)))
            .then(|| (intent.id.clone(), edit.key.clone()))
        })
    })
}

/// Every live row of this machine's snapshot of `lab/packages`, and the snapshot's revision,
/// once a materializer pass has brought the snapshot up to what the machine holds (none where
/// [`Env::attempt`] is zero: the snapshot is read as it stands).
fn package_rows(
    paths: &Paths,
    config: &Path,
    env: &Env<'_>,
) -> Result<(Vec<aterm_file::Row>, u64), Refusal> {
    let name = file::PACKAGES.file();
    if !env.attempt.is_zero() {
        // What the pass could not read it says in the file's status; a write is still
        // compared with the table when it is published, so a snapshot left behind costs a
        // hold, never an overwrite.
        let _ = materialize::pass(
            paths,
            config,
            Some(&name),
            &materialize::Pass {
                read_by: Instant::now() + env.attempt,
                now_ms: env.now_ms,
                ..materialize::Pass::now()
            },
        );
    }
    let read = aterm_file::File::at(&file::root(&generation::original(paths)), &name).read();
    if read.snapshot.is_none() {
        return Err(problem(format!(
            "{name}: nothing is materialized here yet; `aterm file refresh {name}` says why"
        )));
    }
    let rev = read.status.rev();
    Ok((
        read.rows().iter().filter(|row| row.live).cloned().collect(),
        rev,
    ))
}

/// The id a package keyed `pk` is named by: its row's `id` among `rows`, else the key.
fn shown_id(rows: &[aterm_file::Row], pk: &str) -> String {
    rows.iter()
        .find(|row| row.pk == pk)
        .and_then(|row| row.values.get("id").and_then(Value::as_str))
        .unwrap_or(pk)
        .to_owned()
}

/// What a write of a package's row says it made: its command's word, what the package is once
/// the write is published, and the words of a write still queued.
struct Says<'a> {
    command: &'a str,
    now: String,
    not_yet: &'a str,
}

/// Write `cells` (`<column>=<value>` each) of the package `id`, whose row is `pk` as revision
/// `rev` of this machine's snapshot showed it, as one intent through the outbox of
/// `lab/packages`, and say what became of it.
fn set(
    paths: &Paths,
    config: &Path,
    (id, pk, rev): (&str, &str, u64),
    cells: &[String],
    said: &Said,
    (ask, says): (&Ask, &Says<'_>),
    env: &Env<'_>,
) -> Result<(String, u8), Refusal> {
    let name = file::PACKAGES.name;
    let mut words = vec!["set".to_owned(), name.to_owned(), pk.to_owned()];
    words.extend_from_slice(cells);
    let env = write::Env {
        by: env.by,
        session: env.session,
        now_ms: env.now_ms,
        attempt: env.attempt,
        journal: env.journal,
        json: false,
        stdin: None,
        said: Some(said),
        editor: &|_, _, _| Err("this verb opens no editor".to_owned()),
        say: &|_| {},
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the explicit `aterm goals` verbs claim and done: each is one write of a \
                  package's row, an owner's act named on the command line"
    )]
    let wrote = write::set_seen(paths, config, &words, rev, &env)?;
    let id = keys::safe_key(id);
    let (became, unflushed) = (wrote.said().cloned(), wrote.unflushed().map(str::to_owned));
    let code = wrote.printed().1;
    let (state, intent, op, why) = match &became {
        Some(Wrote::Published { id, op }) => ("published", Some(id), op.as_ref(), None),
        Some(Wrote::Queued { id, why }) => ("queued", Some(id), None, Some(why)),
        Some(Wrote::Held { id, why }) => ("held", Some(id), None, Some(why)),
        Some(Wrote::Unchanged | Wrote::Dropped { .. }) | None => ("unchanged", None, None, None),
    };
    if ask.json {
        let mut object = Map::new();
        object.insert("command".to_owned(), json!(says.command));
        object.insert("package".to_owned(), json!(id));
        object.insert("state".to_owned(), json!(state));
        object.insert("id".to_owned(), json!(intent));
        object.insert("op".to_owned(), json!(op));
        object.insert("why".to_owned(), json!(why));
        object.insert("unflushed".to_owned(), json!(unflushed));
        return Ok((format!("{}\n", Value::Object(object)), code));
    }
    let (now, not_yet) = (&says.now, says.not_yet);
    let intent = intent.map(String::as_str).unwrap_or_default();
    let mut text = match (state, op, why) {
        ("published", Some(op), _) => format!("published {op}: {id} is {now}\n"),
        ("published", None, _) => format!("published: the table already says {id} is {now}\n"),
        ("queued", _, Some(why)) => format!("queued ({not_yet}) {intent}: {why}\n"),
        ("held", _, Some(why)) => format!(
            "held {intent}: {why}\n{id} changed since this machine read it, so nothing was \
             overwritten: `aterm file resolve {name} {intent} theirs` drops this write\n"
        ),
        _ => format!("unchanged: {name} already says that of {id}\n"),
    };
    if let Some(why) = unflushed {
        text.push_str(&format!(
            "note: {intent} is in the outbox, but its folder's flush failed ({why}); a power \
             loss before it is published may lose it\n"
        ));
    }
    Ok((text, code))
}

/// This machine's Commons machine uuid, as its own roster binds it.
fn here(paths: &Paths) -> Result<String, Refusal> {
    let bootstrap = aterm_secrets::commons::bootstrap::Bootstrap::cached(paths)
        .map_err(problem)?
        .ok_or_else(|| problem("this machine holds no Commons"))?;
    let session = crate::table::Session::observe(&bootstrap).map_err(problem)?;
    let actor = session.actor();
    session
        .members()
        .map_err(problem)?
        .iter()
        .find(|binding| binding.actor() == actor)
        .map(|binding| keys::uuid32(binding.machine()))
        .ok_or_else(|| problem("this machine is not in its own Commons roster"))
}

/// Append `entry` to `log`, once the tracker here holds the live row it is about (the row named
/// `id` of the table `table`), and return the entry's key. The append states `said`.
fn append(
    paths: &Paths,
    log: &'static crate::typed::Collection,
    (table, id): (&str, &str),
    entry: Map,
    said: Said,
) -> Result<String, Refusal> {
    let compiled = att::current(paths, &att::TRACKER);
    let handle = Handle::at(paths.clone(), compiled).saying(said);
    let about = compiled.collection(table).map_err(problem)?;
    let view = handle.view().map_err(problem)?;
    if !view
        .project(about)
        .rows
        .get(&mapped::key(id))
        .is_some_and(crate::typed::cell::Row::live)
    {
        return Err(problem(format!("no {table} {}", keys::safe_key(id))));
    }
    handle
        .append_log(log, &[entry])
        .map_err(problem)?
        .pop()
        .ok_or_else(|| problem("the append returned no key"))
}

/// What `note` and `measure` print for the entry appended at `key`: `what` it is.
fn appended(ask: &Ask, key: &str, what: String) -> (String, u8) {
    let text = if ask.json {
        let mut object = Map::new();
        object.insert(
            "command".to_owned(),
            json!(if matches!(ask.act, Act::Note { .. }) {
                "note"
            } else {
                "measure"
            }),
        );
        object.insert("state".to_owned(), json!("published"));
        object.insert("key".to_owned(), json!(key));
        format!("{}\n", Value::Object(object))
    } else {
        format!("published {key}: {what}\n")
    };
    (text, exit::OK)
}

fn refuse(why: &str, code: u8) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "{VERB}: {why}");
    ExitCode::from(code)
}

/// `aterm goals …` from the command line.
pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    let Ok(words) = args
        .into_iter()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        return refuse("arguments must be UTF-8", exit::USAGE);
    };
    if words.is_empty() || words.iter().any(|word| word == "--help" || word == "-h") {
        return aterm_uds::stdout::answer_exit(VERB, USAGE, exit::OK);
    }
    let ask = match Ask::parse(&words) {
        Ok(ask) => ask,
        Err(Refusal::Usage(why) | Refusal::Problem(why)) => return refuse(&why, exit::USAGE),
    };
    if !live() {
        return refuse(
            "Product Planning is not live in this build: it registers no lab/tracker yet, so \
             nothing is written",
            exit::PROBLEMS,
        );
    }
    let Ok(paths) = Paths::live() else {
        return refuse("no home directory", exit::USAGE);
    };
    let Some(config) = aterm_types::dirs::aterm_config_path() else {
        return refuse("no configuration folder", exit::USAGE);
    };
    // Who says they ran it: `--by <word>`, else a person at a terminal, else a program.
    let by = ask.by.clone().unwrap_or_else(|| {
        use std::io::IsTerminal as _;
        if std::io::stdin().is_terminal() {
            "person".to_owned()
        } else {
            "program".to_owned()
        }
    });
    // Who makes the write, read now from this process's lineage, never its environment.
    let principal = crate::said::principal_of(crate::alab::command::session::lineage(), &by);
    let session = std::env::var("ATERM_SESSION_ID").ok();
    let journal = crate::alab::upkeep::journal_spool();
    let env = Env {
        principal: &principal,
        by: &by,
        session: session.as_deref().filter(|session| !session.is_empty()),
        now_ms: aterm_time::unix_millis().unwrap_or(0),
        attempt: publish::INLINE,
        journal: journal.as_ref(),
    };
    match run(&paths, &config, &ask, &env) {
        Ok((text, code)) => aterm_uds::stdout::answer_exit(VERB, &text, code),
        Err(Refusal::Usage(why)) => refuse(&why, exit::USAGE),
        Err(Refusal::Problem(why)) => refuse(&why, exit::PROBLEMS),
    }
}
