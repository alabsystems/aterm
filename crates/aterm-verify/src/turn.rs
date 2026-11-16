// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE JUDGMENT TURN, VERIFY'S SIDE (`docs/DESIGN-landing-economy.md` §2.3): a dev-set run
//! predicted HEAVY takes the turn in this user's capacity ledger ([`aterm_capacity::Heavy`])
//! before it claims its disk, and holds it for its whole life, so heavy judgments on a Mac run
//! one at a time, shortest predicted first and aged, and each at the full width of the machine
//! (its own claims of cores are granted all the room, and its test dispatch's live share is what
//! it asked: `crate::test_share`). A run that is not heavy asks nothing and runs beside it.
//!
//! THE PREDICTION ([`predict`]) is the scope's test binaries' latest recorded CPU in the test
//! history, over the machine's cores: what the run's tests are predicted to take of this
//! machine. Each binary counts as the dispatch runs it now: a long one split in two
//! (`crate::test_shards`) by the halves of its newest split, not by its parent's older row or an
//! older split's halves, which the history keeps. A crate of the scope none of whose binaries is
//! recorded counts as the median recorded crate: such a crate has no test target or is new, so
//! a typical crate is the honest guess, where the largest would make every cone that holds one
//! heavy. The empty cone predicts nothing and asks no turn. The compile's CPU is not in it: the
//! stage book keeps no CPU (a follow-up).
//!
//! THE WAIT ENDS behind a holder that makes no progress for [`crate::STUCK_ENDS`] (a stopped job,
//! a hung binary): the turn orders work and guards nothing, so a run that cannot take it, the
//! ledger unread or its holder stuck, goes on without it, as runs did before the turn.
//!
//! THE EVENTS. A run whose first look finds the turn taken says `judgment-queued` (what it
//! waits behind); every heavy run says `judgment-turn` once its wait ends (how long it waited,
//! and whether it holds the turn or goes on without it) and, holding it, `judgment-cpu` at its
//! end (its test binaries' CPU against the prediction, and its whole tree's), which calibrates
//! [`HEAVY`] and the prediction. `verify land` reads the first two (`crate::land`'s report): a
//! cut's seconds leave out the wait.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::Scope;
use crate::event_log::{self, EventKind};
use crate::test_history::{History, Row};

/// A run predicted to take this much of the machine or more is HEAVY and takes the turn. Three
/// machine-minutes is 54 CPU-minutes on 18 cores: §1.1 measured a passing judgment's test run at
/// a p50 floor of 7.8 minutes (its longest binary, or its CPU over the cores), so every judgment
/// whose tests alone hold the machine for minutes is ordered, while the median cone (5
/// packages), whose tests take a fraction of that, runs beside the turn's holder and is never
/// queued behind it.
pub(crate) const HEAVY: Duration = Duration::from_secs(3 * 60);

/// What a run's tests are predicted to take ([`predict`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Prediction {
    /// The scope's test binaries' CPU, ms: each recorded binary's latest reading, and for each
    /// crate none of whose binaries is recorded, the median crate's.
    pub(crate) cpu_ms: u64,
    /// That over the machine's cores: what it takes of this machine, ms.
    pub(crate) machine_ms: u64,
    /// The scope's crates none of whose binaries is recorded.
    pub(crate) unknown: usize,
}

impl Prediction {
    /// Whether it is heavy ([`HEAVY`]).
    pub(crate) fn heavy(self) -> bool {
        Duration::from_millis(self.machine_ms) >= HEAVY
    }
}

/// Predict a run over `scope` (every recorded crate for the whole workspace, none for the empty
/// cone) from `history`, on a machine of `cores` cores ([`Prediction`]).
pub(crate) fn predict(history: &History, scope: &Scope, cores: u32) -> Prediction {
    let by_crate = crate_cpu(history);
    let (cpu_ms, unknown) = match scope {
        Scope::Workspace => (by_crate.values().copied().fold(0, u64::saturating_add), 0),
        Scope::Packages(_) | Scope::Changed(_) => {
            let mut known: Vec<u64> = by_crate.values().copied().collect();
            known.sort_unstable();
            let median = known.get(known.len() / 2).copied().unwrap_or(0);
            scope
                .crates()
                .iter()
                .fold((0u64, 0usize), |(sum, unknown), name| {
                    match by_crate.get(name.as_str()) {
                        Some(cpu) => (sum.saturating_add(*cpu), unknown),
                        None => (sum.saturating_add(median), unknown + 1),
                    }
                })
        }
    };
    Prediction {
        cpu_ms,
        machine_ms: cpu_ms / u64::from(cores.max(1)),
        unknown,
    }
}

/// Each recorded crate's test binaries' CPU, ms, by crate: each binary's latest reading, as the
/// dispatch runs the binary now ([`binary_rows`]).
fn crate_cpu(history: &History) -> BTreeMap<&str, u64> {
    let mut binaries: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for row in history.rows() {
        let parent = row
            .test
            .split_once(" -- ")
            .map_or(row.test.as_str(), |(p, _)| p);
        binaries.entry(parent).or_default().push(row);
    }
    let mut by_crate: BTreeMap<&str, u64> = BTreeMap::new();
    for (parent, rows) in binaries {
        let Some(name) = parent
            .strip_prefix("-p ")
            .and_then(|rest| rest.split(' ').next())
        else {
            continue;
        };
        let cpu = binary_rows(parent, &rows)
            .into_iter()
            .filter_map(|row| row.samples.iter().rev().find_map(|s| s.cpu_ms))
            .reduce(u64::saturating_add);
        if let Some(cpu) = cpu {
            let sum = by_crate.entry(name).or_default();
            *sum = sum.saturating_add(cpu);
        }
    }
    by_crate
}

/// The rows of one binary, `parent` its spec, that its next run is predicted by: its newest
/// run's. Where that ran the whole binary, the parent's row alone; where it ran one half of a
/// split (`<parent> -- <prefix>` or `<parent> -- --skip <prefix>`, `crate::test_shards`), the two
/// halves of that prefix, so neither the parent's row from before the split nor an older split's
/// halves count twice.
fn binary_rows<'r>(parent: &str, rows: &[&'r Row]) -> Vec<&'r Row> {
    let newest = rows
        .iter()
        .copied()
        .max_by_key(|row| row.samples.last().map_or(0, |s| s.at_ms));
    let Some(split) = newest
        .and_then(|row| row.test.strip_prefix(parent))
        .and_then(|rest| rest.strip_prefix(" -- "))
    else {
        return rows
            .iter()
            .copied()
            .filter(|row| row.test == parent)
            .collect();
    };
    let prefix = split.strip_prefix("--skip ").unwrap_or(split);
    let halves = [
        format!("{parent} -- {prefix}"),
        format!("{parent} -- --skip {prefix}"),
    ];
    rows.iter()
        .copied()
        .filter(|row| halves.contains(&row.test))
        .collect()
}

/// Whether a run of `ctx`'s asks for the turn when it is heavy: the dev set, every check of it,
/// as a development run (not a release's or a compile-only one).
pub(crate) fn asks(ctx: &crate::Ctx) -> bool {
    !ctx.release && !ctx.compile_only && crate::checks::DEV.iter().all(|c| ctx.checks.contains(c))
}

/// The turn a heavy run holds ([`take`]) until it ends ([`Held::end`]).
pub(crate) struct Held {
    _claim: aterm_capacity::Claim,
    prediction: Prediction,
    cores: u32,
    began: Instant,
    cpu_before: Option<u64>,
}

/// What the turn's events say of the prediction: the fields every one of them carries.
fn predicted(prediction: Prediction, heavy: aterm_capacity::Heavy) -> aterm_json::Value {
    aterm_json::json!({
        "predicted_ms": prediction.machine_ms,
        "predicted_cpu_ms": prediction.cpu_ms,
        "unknown_crates": prediction.unknown,
        "since_ms": heavy.since_ms,
        "key": heavy.key(),
    })
}

/// `judgment-queued`: the first look found the turn taken, `behind` saying by whom.
pub(crate) fn queued_event(
    prediction: Prediction,
    heavy: aterm_capacity::Heavy,
    behind: &str,
) -> String {
    event(
        EventKind::JudgmentQueued,
        with(
            predicted(prediction, heavy),
            "behind",
            aterm_json::Value::from(crate::report::clip_json(behind, 1024)),
        ),
    )
}

/// `judgment-turn`: the run's wait for the turn ended after `waited`, with the turn where
/// `held`, else with the run going on without it (the ledger unread, or its holder stuck), so it
/// is no longer queued either way.
pub(crate) fn turn_event(
    prediction: Prediction,
    heavy: aterm_capacity::Heavy,
    waited: Duration,
    held: bool,
) -> String {
    let value = with(
        predicted(prediction, heavy),
        "waited_ms",
        aterm_json::Value::from(millis(waited)),
    );
    event(
        EventKind::JudgmentTurn,
        with(value, "held", aterm_json::Value::from(held)),
    )
}

/// What a run that held the turn used ([`cpu_event`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Used {
    /// Its test binaries' CPU, ms: what [`Prediction::cpu_ms`] predicts, so the two compare
    /// ([`crate::Ctx::tests_cpu_ms`]).
    pub(crate) tests_cpu_ms: u64,
    /// Its whole process tree's, compile and lint included, ms; `None` where unread.
    pub(crate) run_cpu_ms: Option<u64>,
    /// Its wall from taking the turn to its end.
    pub(crate) wall: Duration,
}

/// `judgment-cpu`: what a run that held the turn `used`, on `cores` cores, against its
/// `prediction`: `tests_cpu_ms` against `predicted_cpu_ms` (and `tests_machine_ms` against
/// `predicted_ms`) calibrates the prediction; `run_cpu_ms` is the whole tree's, which the
/// prediction does not yet cover.
pub(crate) fn cpu_event(prediction: Prediction, used: Used, cores: u32) -> String {
    let cores_ms = u64::from(cores.max(1));
    event(
        EventKind::JudgmentCpu,
        aterm_json::json!({
            "predicted_ms": prediction.machine_ms,
            "predicted_cpu_ms": prediction.cpu_ms,
            "unknown_crates": prediction.unknown,
            "tests_cpu_ms": used.tests_cpu_ms,
            "tests_machine_ms": used.tests_cpu_ms / cores_ms,
            "run_cpu_ms": used.run_cpu_ms,
            "wall_ms": millis(used.wall),
            "cores": cores,
        }),
    )
}

/// `value`, an object, with `key` set to `field`.
fn with(mut value: aterm_json::Value, key: &str, field: aterm_json::Value) -> aterm_json::Value {
    if let Some(fields) = value.as_object_mut() {
        fields.insert(key.into(), field);
    }
    value
}

fn event(kind: EventKind, value: aterm_json::Value) -> String {
    format!("verify-event: {}", event_log::event_value(kind, value))
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Now, unix ms.
fn now_ms() -> u64 {
    aterm_time::unix_millis().unwrap_or(0)
}

/// Take the turn for a run of `ctx`'s that [`asks`] for it and is predicted heavy, first queued
/// at `since_ms` (`--queued-since`: a landing's own start, so its re-judges keep its place), else
/// now, admitted as `mode` says. `Ok(None)` where it asks none, or where the ledger could not
/// be read or every holder it waits behind made no progress for [`crate::STUCK_ENDS`] (the turn
/// orders work and guards nothing, so the run goes on without it, saying so). Its events go to
/// the run's events, bound to the run first.
///
/// # Errors
/// The wait ended on an interrupt or the run's end, which ends the run.
pub(crate) fn take(
    ctx: &mut crate::Ctx,
    since_ms: Option<u64>,
    mode: aterm_capacity::Admit,
) -> Result<Option<Held>, String> {
    if !asks(ctx) {
        return Ok(None);
    }
    let cores = aterm_capacity::cores();
    let history = ctx
        .test_history
        .as_deref()
        .map(crate::test_history::load)
        .unwrap_or_default();
    let prediction = predict(&history, &ctx.scope, cores);
    if !prediction.heavy() {
        return Ok(None);
    }
    let heavy = aterm_capacity::Heavy {
        since_ms: since_ms.unwrap_or_else(now_ms),
        predicted_ms: prediction.machine_ms,
    };
    // The turn's events come before the run's log opens; bind them to the run as that would.
    let context = event_log::Context::captured(ctx);
    ctx.events
        .get_or_insert_with(event_log::EventLog::unrecorded)
        .bind(context);
    let events = ctx.events.as_ref();
    let need = aterm_capacity::Need::new(format!("verify judgment turn: {}", ctx.scope.label()))
        .heavy(heavy.since_ms, heavy.predicted_ms)
        .ends_when_stuck(crate::STUCK_ENDS);
    let asked = Instant::now();
    let mut queued = false;
    let admitted = crate::admit_with_say(need, mode, &mut |line| {
        if !queued {
            queued = true;
            event_log::emit(events, &queued_event(prediction, heavy, line));
        }
        eprintln!("{line}");
    });
    match admitted {
        Ok(claim) => {
            event_log::emit(events, &turn_event(prediction, heavy, claim.waited(), true));
            Ok(Some(Held {
                _claim: claim,
                prediction,
                cores,
                began: Instant::now(),
                cpu_before: cpu_used_ms(),
            }))
        }
        Err(e @ (aterm_capacity::Error::Ledger(_) | aterm_capacity::Error::Stuck { .. })) => {
            event_log::emit(
                events,
                &turn_event(prediction, heavy, asked.elapsed(), false),
            );
            eprintln!("verify: the judgment turn was not taken ({e}); the run goes on without it");
            Ok(None)
        }
        Err(e) => Err(format!("the judgment turn: {e}")),
    }
}

impl Held {
    /// The run ended, its test binaries having used `tests_cpu_ms`: say what it used beside its
    /// prediction, and let the turn go.
    pub(crate) fn end(self, events: Option<&event_log::EventLog>, tests_cpu_ms: u64) {
        let used = Used {
            tests_cpu_ms,
            run_cpu_ms: cpu_used_ms()
                .zip(self.cpu_before)
                .map(|(now, before)| now.saturating_sub(before)),
            wall: self.began.elapsed(),
        };
        event_log::emit(events, &cpu_event(self.prediction, used, self.cores));
    }
}

/// The CPU this process's tree has used, ms: each member's own and what it reaped
/// ([`crate::exec::TreeMember::cpu_ns`]), so a stage's children that ended are in it. `None`
/// where the process table does not read.
fn cpu_used_ms() -> Option<u64> {
    use crate::exec::Host as _;
    let members = crate::exec::LiveHost.tree_usage(std::process::id())?;
    let ns = members
        .iter()
        .map(|member| member.cpu_ns)
        .fold(0u64, u64::saturating_add);
    Some(ns / 1_000_000)
}

#[cfg(test)]
#[path = "turn_tests.rs"]
mod tests;
