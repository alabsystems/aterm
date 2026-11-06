// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CRUISE NEVER PARKS: what `lab forever` wants of `same-cruise` (HC), whose agent answers every
//! act with one defense and no work. The continuous-forward design of 2026-11-01
//! (`docs/DESIGN-continuous-forward-2026-11-01.md`) replaced ruling F's same-reply hold under
//! Cruise: "Repeated no-work replies permanently stopping Cruise | Use the existing pacing
//! interval, then request a different approach". So HC no longer wants a hold:
//!
//! * the first act is Cruise's done check ([`turn_end::RULE_DONE_CHECK`]) at the done claim;
//! * EACH GAP AT LEAST THE LADDER'S: act `n` (from 1) comes no sooner than the short back-off
//!   ladder's step `n` ([`ladder_step_ms`]) after the reply it answers, less [`STEP_SLACK_MS`] —
//!   every reply of the chain is a short turn, so act `n` closes `n` of them in a row — and
//!   within the step the loop decided at that reply ([`ladder_steps`]): no sooner than its due,
//!   no later than [`PERSISTENCE_GRACE_MS`] past it;
//! * A DIFFERENT APPROACH: the act after the second same answer is the request for one
//!   ([`turn_end::FORWARD_RECOVERY`] under [`turn_end::RULE_CONTINUE`], [`is_recovery`]), after
//!   the loop's wait for it ([`recovery_wait`]);
//! * NEVER A PERMANENT STOP: no hold (`EXCEPTION … kind=stalled`), no step whose due passed by
//!   [`PERSISTENCE_GRACE_MS`] with nothing typed after it ([`lost_after`]), and THE REQUEST'S
//!   ANSWER DECIDED AS THE LADDER'S NEXT STEP ([`next_after_request`]): a wait for that answer's
//!   streak (a back-off or another request's wait) due the ladder's step past it, never a `LEFT`,
//!   a `DONE`, a hold, a wait in other words or a due far past the step. The act at that step's
//!   due is `--mode ladder`'s want, which reads every request's wait as a step and wants each
//!   step's act at its due;
//! * BOUNDED: never more acts in any hour than the ladder allows ([`per_hour`]). A live watch
//!   holds three acts in about a quarter hour, so live it is the gaps that bound a build; the
//!   hour judges a longer journal (the constant-rate control's).
//!
//! A build that types at a constant rate fails the gap and the hour; one that holds fails the
//! stop, and one that parks after its request fails the next step. The explicit holds keep their
//! own wants: Land's same reply (HL), a person's hold and a claim's custody.

use aterm_agent::supervise::policy::turn_end;

use super::surfaces::held::{decided_after, is_stall, stall_of};
use super::{
    PERSISTENCE_GRACE_MS, STEP_SLACK_MS, Seen, Verdict, act_rule, at, backoff_due, backoff_named,
    idle_words, ladder_step_ms, ladder_steps, lost_after, recovery_due, recovery_wait,
};
use crate::util::one_line;

/// The hang detector of `same-cruise`'s watch: the shipped ladder's first three steps (2, 4 and
/// 8 min), the grace past the last, and slack for a loaded Mac's replies.
pub const LIMIT_MS: i64 = 20 * 60 * 1000;

/// How long the watch runs past the decision on the request's answer.
pub const PAST_ANSWER_MS: i64 = 10_000;

/// An hour, the window [`per_hour`] bounds.
pub const HOUR_MS: i64 = 60 * 60 * 1000;

/// The scale `same-cruise` runs the ladder at: the measured pass's instance sets no seam, so its
/// ladder is the shipped one.
pub const SHIPPED: f64 = 1.0;

/// The words a request for a different approach carries past the harness mark: the head of
/// [`turn_end::FORWARD_RECOVERY`], short enough that the journal's line holds it whole.
#[must_use]
pub fn recovery_head() -> &'static str {
    let rest = turn_end::FORWARD_RECOVERY
        .split_once(']')
        .map_or(turn_end::FORWARD_RECOVERY, |(_, r)| r)
        .trim_start();
    let end = rest.char_indices().nth(48).map_or(rest.len(), |(i, _)| i);
    &rest[..end]
}

/// Whether `act` (a journal act of `seen`) is the request for a different approach: typed under
/// [`turn_end::RULE_CONTINUE`], its text (the ledger's command, else the journal's line) holding
/// [`recovery_head`].
#[must_use]
pub fn is_recovery(seen: &Seen, act: &(i64, String)) -> bool {
    act_rule(&act.1) == Some(turn_end::RULE_CONTINUE)
        && seen
            .act_text(turn_end::RULE_CONTINUE, act)
            .contains(recovery_head())
}

/// The most acts the ladder at `scale` lets into any hour: its densest hour opens at the first
/// act, and act `n + 1` follows act `n` by step `n + 1` at the least, less [`STEP_SLACK_MS`].
#[must_use]
pub fn per_hour(scale: f64) -> usize {
    let mut allowed = 1usize;
    let mut span = 0i64;
    for k in 2u32.. {
        let step = (ladder_step_ms(k, scale) - STEP_SLACK_MS).max(1);
        if span + step >= HOUR_MS {
            break;
        }
        span += step;
        allowed += 1;
    }
    allowed
}

/// The acts `seen` journaled from `t0` on.
fn acts_from(seen: &Seen, t0: i64) -> Vec<&(i64, String)> {
    seen.all_acts()
        .into_iter()
        .filter(|(t, _)| *t >= t0)
        .collect()
}

/// A point's words, where it said any.
fn worded(line: &str) -> Option<&str> {
    idle_words(line).filter(|w| !w.is_empty() && *w != "-")
}

/// The last reply read after `after` and before `t`: the one the act at `t` answers.
fn reply_before(seen: &Seen, after: i64, t: i64) -> Option<&(i64, String)> {
    seen.journal
        .iter()
        .rfind(|(at, l)| *at > after && *at < t && worded(l).is_some())
}

/// The first reply read after `t`: the answer to the act at `t`, and when it was read.
fn answer_read_after(seen: &Seen, t: i64) -> Option<(i64, &str)> {
    seen.journal
        .iter()
        .find(|(at, l)| *at > t && worded(l).is_some())
        .and_then(|(at, l)| worded(l).map(|w| (*at, w)))
}

/// The first reply read after `t`: the answer to the act at `t`.
fn answer_after(seen: &Seen, t: i64) -> Option<&str> {
    answer_read_after(seen, t).map(|(_, w)| w)
}

/// THE STEP AFTER THE REQUEST: what the loop decided at the answer to the request for a different
/// approach at `rc` (act `n`, from 1), the ladder at `scale`. `None` where it is the ladder's next
/// step ([`ladder_step_ms`] for `n + 1` short turns, from the decision, within [`STEP_SLACK_MS`]:
/// the loop dates its `until=` from when it decides): a back-off ([`backoff_named`]) or another request's wait
/// ([`recovery_wait`]). Else a fault: a `LEFT`, a `DONE`, a hold, a wait in other words or one
/// due off the step. No answer or no decision yet is no fault here: the watch then runs to its
/// limit, where [`lost_after`] judges the silence.
#[must_use]
pub fn next_after_request(seen: &Seen, rc: i64, n: u32, scale: f64, t0: i64) -> Option<String> {
    let (ta, _) = answer_read_after(seen, rc)?;
    let (td, line) = decided_after(seen, rc)?;
    let said = one_line(line, 120);
    let parked = |what: &str| {
        Some(format!(
            "STOPPED AFTER THE REQUEST: its answer (+{}) was decided at +{} as {what} ({said}), not \
             the ladder's next step",
            at(t0, ta),
            at(t0, *td)
        ))
    };
    if is_stall(line) {
        return parked("a hold");
    }
    if line.starts_with("LEFT ") {
        return parked("left to the person");
    }
    if line.starts_with("DONE ") {
        return parked("an end");
    }
    if !backoff_named(line) && !recovery_wait(line) {
        return parked("a wait in other words");
    }
    let due = backoff_due(line).or_else(|| recovery_due(line))?;
    let want = ladder_step_ms(n.saturating_add(1), scale);
    let waits = due - td;
    ((waits - want).abs() > STEP_SLACK_MS).then(|| {
        format!(
            "the request's answer (+{}), decided at +{}, waits {} to its next act ({said}), not the \
             ladder's step {} of {}",
            at(t0, ta),
            at(t0, *td),
            at(0, waits),
            n.saturating_add(1),
            at(0, want)
        )
    })
}

/// Whether `same-cruise`'s watch is over: a minute past a hold (a build that parks, which HC
/// fails), [`PAST_ANSWER_MS`] past the decision on the request's answer, or once a step's due
/// passed with nothing typed after it (a stop, which HC fails too).
#[must_use]
pub fn watched(seen: &Seen, now: i64, t0: i64) -> bool {
    if let Some((ts, _)) = stall_of(seen, t0) {
        return now - ts >= super::PAST_NEXT_STEP_MS;
    }
    let acts = acts_from(seen, t0);
    if let Some(rec) = acts.iter().find(|a| is_recovery(seen, a)) {
        return decided_after(seen, rec.0).is_some_and(|(td, _)| now - td >= PAST_ANSWER_MS);
    }
    lost_after(seen, acts.last().map_or(t0, |(t, _)| *t), t0).is_some()
}

/// HC under continuous forward (the module's wants), the ladder at `scale`.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn judge(seen: &Seen, scale: f64, t0: i64) -> Verdict {
    let acts = acts_from(seen, t0);
    let Some(first) = acts.first() else {
        return match lost_after(seen, t0, t0) {
            Some(lost) => Verdict::Fail(format!("no act typed: {lost}")),
            None => Verdict::Fail(format!("no act typed within {}", at(t0, seen.end_ms))),
        };
    };
    if act_rule(&first.1) != Some(turn_end::RULE_DONE_CHECK) {
        return Verdict::Fail(format!(
            "its first act at +{} was {}, not {}",
            at(t0, first.0),
            act_rule(&first.1).unwrap_or("?"),
            turn_end::RULE_DONE_CHECK
        ));
    }
    if let Some((d, l)) = seen.first_after("DONE ", t0) {
        return Verdict::Fail(format!("ENDED at +{}: {}", at(t0, *d), one_line(l, 120)));
    }
    if seen.status_said("task", "landed", t0) {
        return Verdict::Fail("status said task=landed".into());
    }
    if let Some(n) = seen.needs_seen() {
        return Verdict::Fail(format!("a need was raised, though no reply named one: {n}"));
    }
    if let Some((ts, sl)) = stall_of(seen, t0) {
        return Verdict::Fail(format!(
            "STOPPED FOR GOOD at +{} ({}): Cruise never parks a repeated report (the \
             continuous-forward design, 2026-11-01); it waits the ladder's step and asks for a \
             different approach",
            at(t0, *ts),
            one_line(sl, 140)
        ));
    }
    let steps = ladder_steps(seen, t0);
    let mut said = Vec::new();
    let mut bad = Vec::new();
    let mut prev = i64::MIN;
    for (i, act) in acts.iter().enumerate() {
        let n = u32::try_from(i + 1).unwrap_or(u32::MAX);
        let (ta, la) = (act.0, act.1.as_str());
        let rule = act_rule(la).unwrap_or("?");
        let Some((tr, _)) = reply_before(seen, prev, ta) else {
            bad.push(format!(
                "act {n} ({rule}) at +{} typed with no reply read since the act before",
                at(t0, ta)
            ));
            prev = ta;
            continue;
        };
        let want = ladder_step_ms(n, scale);
        let gap = ta - tr;
        if gap < want - STEP_SLACK_MS {
            bad.push(format!(
                "SPAM: act {n} ({rule}) at +{}, {} after the reply it answers (+{}), under the \
                 ladder's step {n} of {}",
                at(t0, ta),
                at(0, gap),
                at(t0, *tr),
                at(0, want)
            ));
        }
        match steps.iter().rfind(|st| st.at >= *tr && st.at < ta) {
            Some(st) if ta < st.due - STEP_SLACK_MS => bad.push(format!(
                "act {n} ({rule}) at +{}, before the due its back-off named at +{} ({})",
                at(t0, ta),
                at(t0, st.due),
                one_line(&st.line, 100)
            )),
            Some(st) if ta > st.due + PERSISTENCE_GRACE_MS => bad.push(format!(
                "act {n} ({rule}) at +{}, {} past the due its back-off named at +{}",
                at(t0, ta),
                at(0, ta - st.due),
                at(t0, st.due)
            )),
            Some(_) => {}
            None => bad.push(format!(
                "act {n} ({rule}) at +{} typed with no back-off decided at the reply it answers \
                 (+{})",
                at(t0, ta),
                at(t0, *tr)
            )),
        }
        said.push(format!(
            "+{} {rule} {} after its reply",
            at(t0, ta),
            at(0, gap)
        ));
        prev = ta;
    }
    let answers: Vec<&str> = acts
        .iter()
        .filter_map(|(t, _)| answer_after(seen, *t))
        .collect();
    if let Some(other) = answers.iter().find(|a| a.trim() != answers[0].trim()) {
        bad.push(format!(
            "its answers differ (\"{}\", then \"{}\"), so it is no repeated report",
            one_line(answers[0], 70),
            one_line(other, 70)
        ));
    } else if let Some(a) = answers.first() {
        said.push(format!("each answered \"{}\"", one_line(a, 70)));
    }
    let recovery = acts.get(2).copied();
    let mut undecided = None;
    if let Some(rc) = recovery {
        let waited = steps
            .iter()
            .rfind(|st| st.at < rc.0)
            .is_some_and(|st| recovery_wait(&st.line));
        if !is_recovery(seen, rc) {
            bad.push(format!(
                "the act after the second same answer at +{} was {} \"{}\", not the request for a \
                 different approach (\"{}…\")",
                at(t0, rc.0),
                act_rule(&rc.1).unwrap_or("?"),
                one_line(seen.act_text(act_rule(&rc.1).unwrap_or(""), rc), 100),
                recovery_head()
            ));
        } else if !waited {
            bad.push(format!(
                "the request for a different approach at +{} followed no wait for it (`WAITING … \
                 {}`)",
                at(t0, rc.0),
                turn_end::REPEAT_RECOVERY_WAIT
            ));
        } else {
            said.push(format!("a different approach asked at +{}", at(t0, rc.0)));
            if let Some(fault) = next_after_request(seen, rc.0, 3, scale, t0) {
                bad.push(fault);
            } else if let Some((td, _)) = decided_after(seen, rc.0) {
                said.push(format!(
                    "its answer's next step decided at +{}",
                    at(t0, *td)
                ));
            } else {
                undecided = Some(rc.0);
            }
        }
    }
    let last = acts.last().map_or(t0, |(t, _)| *t);
    if let Some(lost) = lost_after(seen, last, t0) {
        bad.push(lost);
    }
    let allowed = per_hour(scale);
    if let Some((i, n)) = acts
        .iter()
        .enumerate()
        .map(|(i, (t, _))| {
            (
                i,
                acts.iter()
                    .filter(|(u, _)| *u >= *t && *u < t + HOUR_MS)
                    .count(),
            )
        })
        .find(|(_, n)| *n > allowed)
    {
        bad.push(format!(
            "SPAM: {n} acts within an hour from +{}, past the {allowed} the ladder allows",
            at(t0, acts[i].0)
        ));
    }
    let said = said.join("; ");
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    if let Some(rc) = undecided {
        return Verdict::Unobserved(format!(
            "{said}: the request's answer (+{}) not decided within {}",
            at(t0, rc),
            at(t0, seen.end_ms)
        ));
    }
    if recovery.is_none() {
        let due = steps
            .iter()
            .rfind(|st| st.at > last)
            .map_or_else(String::new, |st| {
                format!(", the next due at +{}", at(t0, st.due))
            });
        return Verdict::Unobserved(format!(
            "{said}: the request for a different approach not typed within {}{due}",
            at(t0, seen.end_ms)
        ));
    }
    Verdict::Pass(format!(
        "{said}; never stopped, at most {allowed} act(s) an hour on this ladder"
    ))
}

#[cfg(test)]
#[path = "forever_paced_tests.rs"]
mod tests;
