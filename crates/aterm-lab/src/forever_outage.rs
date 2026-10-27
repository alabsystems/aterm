// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AN OUTAGE IS A SILENT WAIT (v6 §14.17.5.2, slice 12b): what `lab forever` wants of an API
//! overload that stands, then lifts, under a real Claude Code.
//!
//! * The stub stands the overload in the custom mode [`MODE`] ([`spec`]): every request answered
//!   `529 overloaded_error` with `x-should-retry: false`, so Claude Code shows its final error at
//!   once rather than its own retries, and no `times`, so it stands until the lab lifts it.
//!   - `--mode outage-land` (`land-outage`) raises it before the chain's first prompt: the prompt
//!     meets it, and Land's instruction was never given.
//!   - `--mode outage-cruise` (`cruise-outage`) raises it at the chain's first point, as
//!     `wall-cruise` raises its wall: Cruise's own act meets it.
//! * The lab lifts it, the stub's scripted mode again, once [`TRIES`] acts met it and the loop
//!   decided a wait after the last; [`hold_at_most_ms`] past its meeting, a hang detector lifts it
//!   from under a loop that never tried ([`outage_move`]).
//! * Both modes run the loop's ladders at three eighths ([`SCALE`], [`env`]), so the outage's
//!   rungs ([`rung_ms`]) are 45 s, 90 s, 180 s and 360 s.
//! * [`judge`] wants:
//!   - every act typed while the outage stood a try ([`RULE_OUTAGE_RESUME`]), each its one line
//!     ([`line`]) or, as Land's first, the instruction never given before it (12a's rule), each on
//!     its rung counted from its own wall: the wait on the outage journaled where the outage met
//!     the agent again names the rung as its due, and the try comes at it ([`try_faults`]);
//!   - nothing else typed into it, nothing escalated, needed or landed while it stood, and
//!     nothing held while it stood or on the resume's answer;
//!   - `aterm ctl status` saying `waiting=overload` while it stood, and no longer once the agent
//!     worked;
//!   - after the lift exactly one resume, the blind line on its rung (nothing measures an
//!     overload's end), which the stub reads, and the agent's turn moving on: Land lands on its
//!     answer, Cruise types its own act. A one-off cut's `api-cutoff@v1` between is neither a
//!     second resume nor the mode's act.
//! * [`state_line`], a window want, wants the tab's Settings ▸ Harness row to lead its state line
//!   with `waiting: API overload · next try HH:MM` while the outage stood, with no need on it and
//!   no chip title naming the outage.
//!
//! Main's rule fails it: its `api-retry@v1` at the first minute is an act typed into the outage
//! that is no try, and a second resume after the lift is one resume too many.

use aterm_agent::harness::acts::FACT_SEPARATOR;
use aterm_agent::supervise::Outage;
use aterm_agent::supervise::ladder::doubling;
use aterm_agent::supervise::policy::turn_end::{self, Cleared, Clearing, RULE_OUTAGE_RESUME};

use super::surfaces::held::stall_of;
use super::surfaces::{Ctx, observed, page_field};
use super::walls::{named_backoff, unledgered};
use super::{
    Chain, DUE_SLACK_MS, LAND_KEY, PERSISTENCE_GRACE_MS, STEP_SLACK_MS, Seen, Tab, Verdict,
    act_rule, at, dumps, first_point_unanswered, ladder_step_ms, landed_verdict, landing_quiet,
    point_read, take_window_ms, word,
};
use crate::Fail;
use crate::pyjson::J;
use crate::run::Run;
use crate::util::one_line;

/// The custom stub mode the outage stands in.
pub const MODE: &str = "outage";

/// The outage the stub stands: Claude Code reads its 529 as an overload.
pub const WALL: Outage = Outage::Overload;

/// The factor the outage modes scale the loop's ladders by (`turn_end::LADDER_SCALE_SEAM`): three
/// eighths, so the first rung (45 s) outlasts the take window (30 s, unscaled) a Cruise act that
/// meets the outage is waited on for, as the shipped 2 min does, and the chain fits its
/// [`LIMIT_MS`].
pub const SCALE: f64 = 0.375;

/// The instance environment the outage modes run their aterm under: the ladders' seam at
/// [`SCALE`], which only a development build reads.
#[must_use]
pub fn env() -> String {
    format!("{}={SCALE}", turn_end::LADDER_SCALE_SEAM)
}

/// How many acts meet the outage before the lab lifts it: its first three rungs, so the resume is
/// the fourth try.
pub const TRIES: usize = 3;

/// Each outage chain's hang detector.
pub const LIMIT_MS: i64 = 20 * 60 * 1000;

/// How long `cruise-outage` is watched past Cruise's own act after the resume, so a second resume
/// typed with it shows.
pub const WENT_ON_WATCH_MS: i64 = 30_000;

/// The stub's answer while the outage stands ([`MODE`]).
#[must_use]
pub fn spec() -> J {
    J::obj([
        ("status", J::Int(529)),
        ("headers", J::obj([("x-should-retry", J::s("false"))])),
        (
            "json",
            J::obj([
                ("type", J::s("error")),
                (
                    "error",
                    J::obj([
                        ("type", J::s("overloaded_error")),
                        ("message", J::s("Overloaded")),
                    ]),
                ),
            ]),
        ),
    ])
}

/// Rung `n` (from 1) of the outage's back-off, in ms: the policy's own ladder, `outage_first`
/// doubled to `outage_cap` ([`turn_end::TurnEndTiming`]), scaled by `scale`.
#[must_use]
pub fn rung_ms(n: u32, scale: f64) -> i64 {
    let t = turn_end::TurnEndTiming::default().ladder_scaled(scale);
    let rung = doubling(t.outage_first, n.max(1) - 1, t.outage_cap);
    i64::try_from(rung.as_millis()).unwrap_or(i64::MAX)
}

/// How long past its meeting the lab holds the outage at the most: each of its [`TRIES`] rungs and
/// a grace past each.
#[must_use]
pub fn hold_at_most_ms(scale: f64) -> i64 {
    (1..=TRIES)
        .map(|n| rung_ms(u32::try_from(n).unwrap_or(u32::MAX), scale) + PERSISTENCE_GRACE_MS)
        .sum()
}

/// The words of a try at outage `o`, where nothing measures its end, as the policy types them
/// under [`RULE_OUTAGE_RESUME`] ([`turn_end::cleared_text`]).
#[must_use]
pub fn line(o: Outage) -> String {
    turn_end::cleared_text(Cleared::Outage(o), Clearing::Blind).to_string()
}

/// Whether journal `line` is the loop's wait on an outage: `WAITING seq=<n> until=<when> waiting:
/// <words>`.
#[must_use]
pub fn is_wait(line: &str) -> bool {
    line.starts_with("WAITING ")
        && line
            .split_once(" until=")
            .and_then(|(_, rest)| rest.split_once(' '))
            .is_some_and(|(_, why)| why.starts_with("waiting: "))
}

/// What the lab does to the stub's outage ([`step`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutageMove {
    /// Nothing yet.
    Hold,
    /// Stand the outage ([`MODE`]).
    Raise,
    /// Give the stub its scripted mode back.
    Lift,
}

/// What the lab does to chain `chain`'s outage at `now` (`t0` its first prompt, `scale` the
/// ladder seam's factor): raise it before the first prompt where it stands from there
/// ([`Chain::outage_at_prompt`]), else at the first point while nothing was typed
/// ([`first_point_unanswered`]); lift it once [`TRIES`] acts met it and a wait on it was journaled
/// after the last, or [`hold_at_most_ms`] after its meeting. Every act counts, so a loop that
/// types its own retries into the outage is lifted from too. Nothing once lifted.
#[must_use]
pub fn outage_move(chain: Chain, seen: &Seen, t0: i64, scale: f64, now: i64) -> OutageMove {
    let cover = &seen.cover;
    if !chain.outage() || cover.outage_lifted_ms.is_some() {
        return OutageMove::Hold;
    }
    if cover.outage_ms.is_none() {
        let due = if chain.outage_at_prompt() {
            !point_read(seen, t0) && seen.all_acts().is_empty()
        } else {
            first_point_unanswered(seen, t0)
        };
        return if due {
            OutageMove::Raise
        } else {
            OutageMove::Hold
        };
    }
    let Some(met) = met(chain, seen) else {
        return OutageMove::Hold;
    };
    let acts: Vec<i64> = seen
        .all_acts()
        .into_iter()
        .map(|(t, _)| *t)
        .filter(|t| *t > met)
        .collect();
    let waited = acts
        .last()
        .is_some_and(|last| wait_after(seen, *last, i64::MAX).is_some());
    if (acts.len() >= TRIES && waited) || now >= met + hold_at_most_ms(scale) {
        OutageMove::Lift
    } else {
        OutageMove::Hold
    }
}

/// When chain `chain`'s outage was met: at its raise where it stands from the first prompt, else
/// at the first act after the raise.
#[must_use]
pub fn met(chain: Chain, seen: &Seen) -> Option<i64> {
    let raised = seen.cover.outage_ms?;
    if chain.outage_at_prompt() {
        return Some(raised);
    }
    seen.all_acts()
        .into_iter()
        .map(|(t, _)| *t)
        .find(|t| *t > raised)
}

/// Raises or lifts the stub's outage for `tab` as [`outage_move`] says at `now`.
///
/// # Errors
/// The stub did not take the mode.
pub(super) fn step(run: &Run, tab: &mut Tab, now: i64) -> Result<(), Fail> {
    match outage_move(tab.chain, &tab.seen, tab.t0, SCALE, now) {
        OutageMove::Hold => {}
        OutageMove::Raise => {
            let answer = crate::verbs::set_custom_mode(run, MODE, &spec())?;
            tab.seen.cover.outage_ms = Some(now);
            println!(
                "  [{}] +{} stub    the overload raised: mode {MODE}, 529 overloaded_error with x-should-retry: false, no times ({})",
                tab.chain.name(),
                at(tab.t0, now),
                one_line(&answer, 120)
            );
        }
        OutageMove::Lift => {
            let answer = crate::verbs::set_mode(run, super::MODE)?;
            tab.seen.cover.outage_lifted_ms = Some(now);
            let into = met(tab.chain, &tab.seen).map_or(0, |m| {
                tab.seen.all_acts().iter().filter(|(t, _)| *t > m).count()
            });
            println!(
                "  [{}] +{} stub    the overload lifted after {into} act(s) met it: mode {} again ({})",
                tab.chain.name(),
                at(tab.t0, now),
                super::MODE,
                one_line(&answer, 120)
            );
        }
    }
    Ok(())
}

/// When Cruise went on past the outage: its first act after the lift other than a try or a
/// one-off cut's answer ([`cut_answer`]), which ends `cruise-outage`'s watch [`WENT_ON_WATCH_MS`]
/// on.
#[must_use]
pub fn went_on(seen: &Seen) -> Option<i64> {
    let lifted = seen.cover.outage_lifted_ms?;
    seen.all_acts()
        .into_iter()
        .find(|(t, l)| {
            *t > lifted && act_rule(l).is_some_and(|r| r != RULE_OUTAGE_RESUME && !cut_answer(r))
        })
        .map(|(t, _)| *t)
}

/// The requests of conversation `token` in `dumps` (`(modified ms, body)`, [`super::dumps_in`])
/// read after `since` whose last user text holds the outage's line: the resume reaching the stub.
#[must_use]
pub fn resumed_requests(dumps: &[(i64, Vec<u8>)], token: &str, since: i64) -> usize {
    let words = line(WALL);
    dumps
        .iter()
        .filter(|(t, body)| {
            *t > since
                && String::from_utf8_lossy(body).contains(token)
                && crate::stub::message_users(body)
                    .last()
                    .is_some_and(|u| u.contains(&words))
        })
        .count()
}

/// [`Cover::outage_resumed`](super::Cover::outage_resumed) of an outage chain's tab once its watch
/// is over: the stub's dumps from the lift on.
pub(super) fn resumed_witness(run: &Run, tab: &Tab) -> Option<usize> {
    if !tab.chain.outage() {
        return None;
    }
    let lifted = tab.seen.cover.outage_lifted_ms?;
    Some(resumed_requests(&dumps(run), &tab.chain.token(), lifted))
}

/// One act the loop typed: when, under which rule, and its text (the ledger's, else the journal
/// line's).
#[derive(Clone, Copy, Debug)]
struct Act<'a> {
    at: i64,
    rule: &'a str,
    text: &'a str,
}

fn acts_after(seen: &Seen, t: i64) -> Vec<Act<'_>> {
    seen.all_acts()
        .into_iter()
        .filter(|(at, _)| *at > t)
        .map(|a| {
            let rule = act_rule(&a.1).unwrap_or("?");
            Act {
                at: a.0,
                rule,
                text: seen.act_text(rule, a),
            }
        })
        .collect()
}

/// The first wait on an outage journaled after `from` and before `to`: when, and the due it names
/// ([`wait_due`]).
fn wait_after(seen: &Seen, from: i64, to: i64) -> Option<(i64, Option<i64>)> {
    seen.journal
        .iter()
        .find(|(t, l)| *t > from && *t < to && is_wait(l))
        .map(|(t, l)| (*t, wait_due(l)))
}

/// A try's wall ([`try_faults`]): the wait decided at `waited` where its `due` is on `rung` from
/// it, else the first point read after `prev` where the due is on its rung from that, else the
/// wait.
fn wall_of(seen: &Seen, (prev, waited): (i64, i64), due: Option<i64>, rung: i64) -> i64 {
    let on_rung = |wall: i64| due.is_some_and(|d| (d - wall - rung).abs() <= STEP_SLACK_MS);
    if on_rung(waited) {
        return waited;
    }
    first_read(seen, prev, waited)
        .filter(|r| on_rung(*r))
        .unwrap_or(waited)
}

/// When the first point after `from` and by `to` was read (its `EVENT` journal line).
fn first_read(seen: &Seen, from: i64, to: i64) -> Option<i64> {
    seen.journal
        .iter()
        .find(|(t, l)| *t > from && *t <= to && l.starts_with("EVENT "))
        .map(|(t, _)| *t)
}

/// The due an outage wait's journal line names: its `until=`, unix ms (to the second).
fn wait_due(line: &str) -> Option<i64> {
    let (_, rest) = line.split_once(" until=")?;
    crate::workers::iso_ms(rest.split_whitespace().next()?)
}

/// Whether `rule` is one of main's typed answers to an API wall that the outage wait retires as
/// acts (`api-retry@v1`, `api-back@v1`).
fn typed_retry(rule: &str) -> bool {
    matches!(rule, turn_end::RULE_API_RETRY | turn_end::RULE_API_BACK)
}

/// Whether `rule` answers a one-off cut (`api-cutoff@v1`), an act 12b keeps: past the outage it is
/// neither a second resume nor the mode's own act.
fn cut_answer(rule: &str) -> bool {
    rule == turn_end::RULE_API_CUTOFF
}

fn main_note(rule: &str) -> &'static str {
    if typed_retry(rule) {
        " (main's typed retry)"
    } else {
        ""
    }
}

/// Whether Land's instruction was typed by `t`.
fn instructed(seen: &Seen, t: i64) -> bool {
    seen.all_acts()
        .into_iter()
        .filter(|(at, _)| *at <= t)
        .any(|a| {
            seen.act_text(act_rule(&a.1).unwrap_or("?"), a)
                .contains(LAND_KEY)
        })
}

/// Why `text` is not what a try must say, if it is not: the outage's line alone, or, where it is
/// Land's first try and Land's instruction was never given (`owed`), the instruction alone (12a's
/// rule). Nothing measures an overload's end, so no try says it *has cleared*, the resume after
/// the lift included.
fn wrong_words(text: &str, owed: bool) -> Result<(), String> {
    let line = line(WALL);
    let said = one_line(text, 100);
    match (owed, text.contains(LAND_KEY), text.contains(&line)) {
        (true, true, false) | (false, false, true) => Ok(()),
        (true, ..) => Err(format!(
            "not Land's instruction alone, which was never given before it: \"{said}\""
        )),
        (false, ..) => Err(format!(
            "not the outage's line alone (\"{line}\"): \"{said}\""
        )),
    }
}

/// How late past its rung a try may come: half the rung, so a try a rung late is never its own,
/// and a minute at the most.
fn late_ms(rung: i64) -> i64 {
    (rung / 2).min(PERSISTENCE_GRACE_MS)
}

/// What is wrong with try `a`, the `n`th, the try before it at `prev` (the outage's meeting, for
/// the first): its words ([`wrong_words`], `owed` as it takes it); no wait on the outage journaled
/// between, at the try's own wall; that wait's due off its rung from the wall by more than the
/// ladder's slack; the try before its rung from the wall, or [`late_ms`] past it; no ledger row.
/// The wall is where the loop read the outage meeting the agent again: the first point read after
/// `prev`, or the wait it decided once the act's take window was over, whichever the due counts
/// from (the loop starts an outage met by the mode's own act at its first reading, and moves a
/// try's wall where it decides past the try's take window).
fn try_faults(
    seen: &Seen,
    a: &Act<'_>,
    (n, prev): (u32, i64),
    owed: bool,
    (scale, t0): (f64, i64),
) -> Vec<String> {
    let mut bad = Vec::new();
    let rung = rung_ms(n, scale);
    if let Err(why) = wrong_words(a.text, owed) {
        bad.push(format!("try {n} at +{}: {why}", at(t0, a.at)));
    }
    match wait_after(seen, prev, a.at) {
        None => bad.push(format!(
            "try {n} at +{} came with no wait on the outage journaled before it",
            at(t0, a.at)
        )),
        Some((waited, due)) => {
            let wall = wall_of(seen, (prev, waited), due, rung);
            match due {
                Some(due) if (due - wall - rung).abs() <= STEP_SLACK_MS => {}
                Some(due) => bad.push(format!(
                    "try {n}: its wall's wait at +{} OFF ITS RUNG: due {} on, its rung {}",
                    at(t0, wall),
                    at(0, due - wall),
                    at(0, rung)
                )),
                None => bad.push(format!(
                    "try {n}: its wall's wait at +{} names no due",
                    at(t0, wall)
                )),
            }
            let past = a.at - wall;
            let when = if past < rung - DUE_SLACK_MS {
                Some("BEFORE ITS RUNG")
            } else if past > rung + late_ms(rung) {
                Some("LATE")
            } else {
                None
            };
            if let Some(when) = when {
                bad.push(format!(
                    "try {n} at +{} {when}: {} after its wall's wait at +{}, its rung {}",
                    at(t0, a.at),
                    at(0, past),
                    at(t0, wall),
                    at(0, rung)
                ));
            }
        }
    }
    bad.extend(unledgered(seen, RULE_OUTAGE_RESUME, a.at, t0));
    bad
}

/// How long after its wall ([`wall_of`]) the try at `t` on `rung`, the try before it at `prev`,
/// came: `<d> past its wall`, or `no wall`.
fn past_wall(seen: &Seen, prev: i64, t: i64, rung: i64) -> String {
    wait_after(seen, prev, t).map_or_else(
        || "no wall".to_string(),
        |(waited, due)| {
            let wall = wall_of(seen, (prev, waited), due, rung);
            format!("{} past its wall", at(0, t - wall))
        },
    )
}

/// What may not happen while the outage stood (`met` to `stood_to`), or at all: an escalation in
/// it, a need, a hold in it, a landing in it under Land or any under Cruise.
fn stood_faults(seen: &Seen, (met, stood_to): (i64, i64), land: bool, t0: i64) -> Vec<String> {
    let mut bad = Vec::new();
    if let Some(r) = seen
        .ledger
        .iter()
        .find(|r| r.ts > met && r.ts < stood_to && r.decision == "escalated")
    {
        bad.push(format!(
            "ESCALATED the outage to a person at +{}: {}",
            at(t0, r.ts),
            one_line(&r.reason, 140)
        ));
    }
    if let Some((t, l)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t >= t0 && l.starts_with("NEEDS "))
    {
        bad.push(format!(
            "a need was raised at +{}, though no reply named one: {}",
            at(t0, *t),
            one_line(l, 100)
        ));
    }
    if let Some((t, l)) = stall_of(seen, t0).filter(|(t, _)| *t < stood_to) {
        bad.push(format!(
            "HELD at +{}: aterm stopped typing into it ({})",
            at(t0, *t),
            one_line(l, 120)
        ));
    }
    match seen.first_after("DONE ", t0) {
        Some((d, l)) if !land => bad.push(format!(
            "DONE under Cruise at +{}: {}",
            at(t0, *d),
            one_line(l, 100)
        )),
        Some((d, _)) if *d < stood_to => {
            bad.push(format!("LANDED at +{} while the outage stood", at(t0, *d)));
        }
        _ => {}
    }
    bad
}

/// Whether status words `s` say the loop waits on the outage: `waiting=<its word>`.
fn waits_on_it(s: &str) -> bool {
    word(s, "waiting").as_deref() == Some(WALL.word())
}

/// How long past the resume Land must have landed on its answer: the take window and a grace.
fn lands_within_ms() -> i64 {
    take_window_ms() + PERSISTENCE_GRACE_MS
}

/// How long past the resume Cruise's own act may come where no back-off was named: the scaled
/// ladder's fourth step and a grace.
fn cruise_within_ms(scale: f64) -> i64 {
    ladder_step_ms(4, scale) + PERSISTENCE_GRACE_MS
}

/// Judges O1 and O2 (`--mode outage-land`, `--mode outage-cruise`) over chain `chain`'s records,
/// the outage's rungs scaled by `scale` ([`rung_ms`]).
#[must_use]
pub fn judge(chain: Chain, seen: &Seen, scale: f64, t0: i64) -> Verdict {
    let Some(raised) = seen.cover.outage_ms else {
        return Verdict::Unobserved(if chain.outage_at_prompt() {
            "the stub's outage was never raised".into()
        } else {
            "the stub's outage was never raised (no first point read)".into()
        });
    };
    let Some(met) = met(chain, seen) else {
        return Verdict::Fail(format!(
            "the overload raised at +{}, but no act of Cruise's met it within {}",
            at(t0, raised),
            at(t0, seen.end_ms)
        ));
    };
    let land = chain.autopilot() == "land";
    let mut said = if chain.outage_at_prompt() {
        format!(
            "the overload raised at +{}, before the first prompt",
            at(t0, raised)
        )
    } else {
        let rule = seen
            .all_acts()
            .into_iter()
            .find(|(t, _)| *t == met)
            .and_then(|(_, l)| act_rule(l))
            .unwrap_or("?");
        format!(
            "the overload raised at +{}; Cruise's {rule} typed at +{} met it",
            at(t0, raised),
            at(t0, met)
        )
    };
    let lifted = seen.cover.outage_lifted_ms;
    let stood_to = lifted.unwrap_or(seen.end_ms + 1);
    let (inside, after): (Vec<Act<'_>>, Vec<Act<'_>>) = acts_after(seen, met)
        .into_iter()
        .partition(|a| a.at < stood_to);
    let mut bad: Vec<String> = inside
        .iter()
        .filter(|a| a.rule != RULE_OUTAGE_RESUME)
        .map(|a| {
            format!(
                "TYPED INTO THE OUTAGE: {} at +{}{}",
                a.rule,
                at(t0, a.at),
                main_note(a.rule)
            )
        })
        .collect();
    bad.extend(stood_faults(seen, (met, stood_to), land, t0));
    match wait_after(seen, met, i64::MAX) {
        Some((tw, _)) => said.push_str(&format!("; waited from +{}", at(t0, tw))),
        None => bad.push(format!(
            "NOT WAITED: no `waiting: {}` journaled after the outage was met",
            WALL.words()
        )),
    }
    let tries: Vec<Act<'_>> = inside
        .iter()
        .copied()
        .filter(|a| a.rule == RULE_OUTAGE_RESUME)
        .collect();
    let owed = land && !instructed(seen, met);
    let mut prev = met;
    let mut ladder = Vec::new();
    for (i, a) in tries.iter().enumerate() {
        let n = u32::try_from(i + 1).unwrap_or(u32::MAX);
        bad.extend(try_faults(seen, a, (n, prev), owed && i == 0, (scale, t0)));
        ladder.push(format!(
            "{n} at +{} ({})",
            at(t0, a.at),
            past_wall(seen, prev, a.at, rung_ms(n, scale))
        ));
        prev = a.at;
    }
    if !ladder.is_empty() {
        said.push_str(&format!(
            "; {} {}: [{}]",
            tries.len(),
            RULE_OUTAGE_RESUME,
            ladder.join(", ")
        ));
    }
    let status_read = seen
        .status_reads
        .iter()
        .chain(seen.status.iter().map(|(t, _)| t))
        .any(|t| *t > met && *t <= stood_to);
    let waiting = seen
        .status
        .iter()
        .find(|(t, s)| *t > met && *t <= stood_to && waits_on_it(s))
        .map(|(t, _)| *t);
    match waiting {
        Some(t) => said.push_str(&format!(
            "; status waiting={} at +{}",
            WALL.word(),
            at(t0, t)
        )),
        None if status_read => bad.push(format!(
            "NOT WAITING IN STATUS: `aterm ctl status` never said waiting={} while the outage stood",
            WALL.word()
        )),
        None => {}
    }
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    let next = u32::try_from(tries.len() + 1).unwrap_or(u32::MAX);
    let next_due =
        wait_after(seen, prev, i64::MAX).map_or(prev, |(wall, _)| wall) + rung_ms(next, scale);
    let Some(lifted) = lifted else {
        if seen.end_ms >= next_due + PERSISTENCE_GRACE_MS {
            return Verdict::Fail(format!(
                "{said}; PERSISTENCE LOST: try {next}, due +{}, not typed by +{}",
                at(t0, next_due),
                at(t0, seen.end_ms)
            ));
        }
        return Verdict::Unobserved(format!(
            "{said}; watched to +{}, the outage still standing",
            at(t0, seen.end_ms)
        ));
    };
    if tries.len() < TRIES {
        return Verdict::Fail(format!(
            "{said}; PERSISTENCE LOST: {} of {TRIES} tries before the lab's hang detector lifted the outage at +{}",
            tries.len(),
            at(t0, lifted)
        ));
    }
    said.push_str(&format!("; lifted at +{}", at(t0, lifted)));
    let Some(r) = after.first().copied() else {
        if seen.end_ms >= next_due + PERSISTENCE_GRACE_MS {
            return Verdict::Fail(format!(
                "{said}; PERSISTENCE LOST: no resume by +{}, its try {next} due +{}",
                at(t0, seen.end_ms),
                at(t0, next_due)
            ));
        }
        return Verdict::Unobserved(format!(
            "{said}; watched to +{}, before the resume (due +{})",
            at(t0, seen.end_ms),
            at(t0, next_due)
        ));
    };
    if r.rule != RULE_OUTAGE_RESUME {
        return Verdict::Fail(format!(
            "{said}; NO RESUME: {} typed at +{} after the lift{}, where the outage's try {next} was due +{}",
            r.rule,
            at(t0, r.at),
            main_note(r.rule),
            at(t0, next_due)
        ));
    }
    let mut bad = try_faults(seen, &r, (next, prev), false, (scale, t0));
    let (cuts, later): (Vec<Act<'_>>, Vec<Act<'_>>) =
        after[1..].iter().copied().partition(|a| cut_answer(a.rule));
    if let Some(again) = later
        .iter()
        .find(|a| a.rule == RULE_OUTAGE_RESUME || typed_retry(a.rule))
    {
        bad.push(format!(
            "A SECOND RESUME: {} at +{}{}, after the resume at +{}",
            again.rule,
            at(t0, again.at),
            main_note(again.rule),
            at(t0, r.at)
        ));
    }
    let read_past = seen.status_reads.iter().any(|t| *t > r.at)
        || seen.status.last().is_some_and(|(t, _)| *t > r.at);
    if let Some((t, _)) = seen
        .status
        .last()
        .filter(|(_, s)| read_past && waits_on_it(s))
    {
        bad.push(format!(
            "STILL WAITING IN STATUS: `aterm ctl status` said waiting={} from +{} to the end of the watch, past the resume",
            WALL.word(),
            at(t0, *t)
        ));
    }
    let answered = after.get(1).map_or(i64::MAX, |a| a.at);
    if let Some((t, l)) = stall_of(seen, stood_to).filter(|(t, _)| *t < answered) {
        bad.push(format!(
            "HELD at +{} on the resume's answer: the outage's tries counted ({})",
            at(t0, *t),
            one_line(l, 120)
        ));
    }
    if seen.cover.outage_resumed == Some(0) {
        bad.push("the stub read NO request carrying the resume after the lift".into());
    }
    said.push_str(&format!(
        "; the resume at +{} ({})",
        at(t0, r.at),
        past_wall(seen, prev, r.at, rung_ms(next, scale))
    ));
    if let Some(c) = cuts.first() {
        said.push_str(&format!(
            "; a one-off cut answered at +{} ({})",
            at(t0, c.at),
            c.rule
        ));
    }
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    let unread = match (seen.cover.outage_resumed, status_read) {
        (None, _) => Some("the stub's dumps after the lift were not read"),
        (_, false) => Some("no status read while the outage stood"),
        (Some(_), true) => None,
    };
    if land {
        judge_landed(seen, &r, &later, &said, unread, t0)
    } else {
        judge_went_on(seen, &r, &later, &said, unread, (scale, t0))
    }
}

/// O1 past its resume `r`, `later` the acts after it but a cut's answer: landed on the resume's
/// answer ([`landing_quiet`]) with nothing typed between.
fn judge_landed(
    seen: &Seen,
    r: &Act<'_>,
    later: &[Act<'_>],
    said: &str,
    unread: Option<&str>,
    t0: i64,
) -> Verdict {
    let within = lands_within_ms();
    let Some(done) = seen.first_after("DONE ", r.at) else {
        if seen.end_ms >= r.at + within {
            return Verdict::Fail(format!(
                "{said}; NOT LANDED: no DONE in the {} watched after the resume",
                at(0, seen.end_ms - r.at)
            ));
        }
        return Verdict::Unobserved(format!(
            "{said}; watched to +{}, not landed yet",
            at(t0, seen.end_ms)
        ));
    };
    if let Some(a) = later.iter().find(|a| a.at < done.0) {
        return Verdict::Fail(format!(
            "{said}; NOT LANDED ON THE RESUME'S ANSWER: {} typed at +{} before the landing at +{}",
            a.rule,
            at(t0, a.at),
            at(t0, done.0)
        ));
    }
    if done.0 > r.at + within {
        return Verdict::Fail(format!(
            "{said}; landed only {} after the resume, past {}",
            at(0, done.0 - r.at),
            at(0, within)
        ));
    }
    let (bad, quiet, window) = landing_quiet(seen, done, false, t0);
    let said = format!("{said}; landed on its answer: {quiet}");
    match unread {
        Some(why) if bad.is_empty() => Verdict::Unobserved(format!("{said}: {why}")),
        _ => landed_verdict(bad, said, seen, done.0, window),
    }
}

/// O2 past its resume `r`, `later` the acts after it but a cut's answer: Cruise's own act by its
/// back-off's due and a grace, ledgered.
fn judge_went_on(
    seen: &Seen,
    r: &Act<'_>,
    later: &[Act<'_>],
    said: &str,
    unread: Option<&str>,
    (scale, t0): (f64, i64),
) -> Verdict {
    let backoff = named_backoff(seen, r.at);
    let deadline = backoff.map_or(r.at + cruise_within_ms(scale), |(due, _)| {
        due + PERSISTENCE_GRACE_MS
    });
    let named = backoff.map_or_else(
        || "no back-off named".to_string(),
        |(due, l)| {
            format!(
                "the back-off it named due +{} ({})",
                at(t0, due),
                l.split_once(" until=")
                    .and_then(|(_, rest)| rest.split_once(' '))
                    .map_or("?", |(_, w)| w)
            )
        },
    );
    let Some(own) = later.iter().find(|a| a.rule != RULE_OUTAGE_RESUME) else {
        if seen.end_ms >= deadline {
            return Verdict::Fail(format!(
                "{said}; PERSISTENCE LOST: Cruise typed nothing of its own in the {} after the resume ({named})",
                at(0, seen.end_ms - r.at)
            ));
        }
        return Verdict::Unobserved(format!(
            "{said}; watched to +{}, before Cruise's own act",
            at(t0, seen.end_ms)
        ));
    };
    if own.at > deadline {
        return Verdict::Fail(format!(
            "{said}; PERSISTENCE LOST: Cruise's {} only at +{}, {} after the resume ({named})",
            own.rule,
            at(t0, own.at),
            at(0, own.at - r.at)
        ));
    }
    if let Some(why) = unledgered(seen, own.rule, own.at, t0) {
        return Verdict::Fail(format!("{said}; {why}"));
    }
    let said = format!(
        "{said}; Cruise went on: {} at +{} ({} after; {named}), ledgered; never DONE",
        own.rule,
        at(t0, own.at),
        at(0, own.at - r.at)
    );
    match unread {
        Some(why) => Verdict::Unobserved(format!("{said}: {why}")),
        None => Verdict::Pass(said),
    }
}

/// [`state_line`]'s name among the window's wants.
pub const STATE_LINE_WANT: &str = "while the outage stood, its state line led with `waiting: API overload · next try HH:MM` (a due its waits named), no need on it, no chip title naming it";

/// O1's and O2's window want ([`STATE_LINE_WANT`]): of the tab's Settings ▸ Harness rows read while
/// the outage stood (from its first wait to the lift), one leads its state line with `waiting:
/// <words> · next try HH:MM`, `HH:MM` the local clock of a due the loop's waits on it named (after
/// the due's weekday, `Wed 00:05`, where it falls on another day, as the window draws it); none
/// says a need, and no chip title read then names the outage (v6 §14.17.5.2: an outage raises no
/// alert, no need and no chip title).
#[must_use]
pub fn state_line(seen: &Seen, cx: &Ctx<'_>) -> Verdict {
    let Some(met) = met(cx.chain, seen) else {
        return Verdict::Unobserved("the outage was never met".into());
    };
    let Some((first, _)) = wait_after(seen, met, i64::MAX) else {
        return Verdict::Unobserved("no wait on the outage was journaled".into());
    };
    let stood_to = seen.cover.outage_lifted_ms.unwrap_or(seen.end_ms + 1);
    let w = &seen.window;
    let rows = observed(&w.page, &w.page_reads, first, stood_to);
    if rows.is_empty() {
        return Verdict::Unobserved("the Harness page was not read while the outage stood".into());
    }
    let lines: Vec<(i64, &str)> = rows
        .iter()
        .filter(|(_, p)| *p != "-")
        .filter_map(|(t, p)| Some((*t, page_field(p, "state")?)))
        .collect();
    let head = format!("waiting: {}{FACT_SEPARATOR}next try ", WALL.words());
    let mut bad = Vec::new();
    if let Some((t, l)) = lines.iter().find(|(_, l)| l.contains("needs you:")) {
        bad.push(format!(
            "+{} a need on the state line: \"{}\"",
            at(cx.t0, *t),
            one_line(l, 160)
        ));
    }
    if let Some((t, title)) = observed(&w.title, &[], first, stood_to)
        .into_iter()
        .find(|(_, title)| title.contains(WALL.words()) || title.contains("waiting:"))
    {
        bad.push(format!(
            "+{} the chip's title names the outage: \"{}\"",
            at(cx.t0, t),
            one_line(title, 160)
        ));
    }
    let said = match lines.iter().find(|(_, l)| l.starts_with(&head)) {
        Some((t, l)) => {
            let rest = &l[head.len()..];
            let rest = rest
                .split_once(' ')
                .filter(|(day, _)| day.len() == 3 && day.bytes().all(|b| b.is_ascii_alphabetic()))
                .map_or(rest, |(_, clock)| clock);
            let clock = rest.get(..5).unwrap_or("");
            let dues = due_clocks(seen, met);
            if !dues.iter().any(|c| c == clock) {
                bad.push(format!(
                    "+{} its next try {clock:?} is no due its waits named ({})",
                    at(cx.t0, *t),
                    dues.join(", ")
                ));
            }
            format!(
                "+{} \"{}\" ({} read(s) while the outage stood)",
                at(cx.t0, *t),
                one_line(l, 160),
                rows.len()
            )
        }
        None => {
            bad.push(format!(
                "NEVER SAID WAITING: none of its {} state line(s) leads with \"{head}HH:MM\"{}",
                lines.len(),
                lines.last().map_or_else(String::new, |(t, l)| format!(
                    ", the last +{} \"{}\"",
                    at(cx.t0, *t),
                    one_line(l, 160)
                ))
            ));
            format!("{} read(s) while the outage stood", rows.len())
        }
    };
    if bad.is_empty() {
        Verdict::Pass(said)
    } else {
        Verdict::Fail(format!("{said}: {}", bad.join("; ")))
    }
}

/// The local `HH:MM` of every due the loop's waits on the outage named after `met`, each read at
/// its second and the next (the journal's `until=` drops the due's milliseconds).
fn due_clocks(seen: &Seen, met: i64) -> Vec<String> {
    let mut out: Vec<String> = seen
        .journal
        .iter()
        .filter(|(t, l)| *t > met && is_wait(l))
        .filter_map(|(_, l)| wait_due(l))
        .flat_map(|due| [due, due + 1_000])
        .map(|ms| {
            let (.., h, m, _) = crate::sys::local_time(ms.div_euclid(1000));
            format!("{h:02}:{m:02}")
        })
        .collect();
    out.dedup();
    out
}
