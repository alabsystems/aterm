// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUILT-IN RULES' LINES (`docs/DESIGN-harness-occupancy-2026-10-20.md` §3.2): the harness
//! mark, one fact sentence (at most [`FACT_CAP`] characters), the fixed advice sentence, then
//! `(rule <name>)`. Closed templates: what is filled in is numbers, durations and closed words,
//! never a label, a path, a note or anything the agent or a person wrote, so a line can carry
//! nothing planted.
//!
//! A [`FactLine`] is made here and nowhere else (its constructor is private), to judge and book
//! what `prompt` would type (`fact-would@v1`).
//!
//! `wait-nudge` leaves out §3.2's `(2 workflows, 3 shells)`: no reader counts a
//! session's workflows yet, and a count the harness cannot measure is not typed.

use super::super::facts::FACT_CAP;
use super::rules::RuleId;
use crate::harness::harness_mark;

/// The longest a whole line may be, in characters: the facts seam's budget for a typed line, so
/// it stays typed text a composer holds and the fence can read back, never a pasted-text
/// placeholder.
pub const LINE_CAP: usize = 620;

/// What every line opens with: the harness's mark and whose fact it is.
const HEAD: &str = concat!(harness_mark!(), " aterm: ");

/// The fixed advice sentence (§3.2): F1's re-plan, and F2's base re-read.
const ADVICE: &str = "If a step of your plan has its inputs ready, start it now rather than at the \
                      end of the whole workflow; re-read the base of any cut, mint or publish \
                      before you start it.";

/// `wait-nudge`'s ask for the declaration, a constant like the advice.
const DECLARE: &str = "Put one line starting WAITING: that names what you wait for and the step \
                       it unblocks.";

/// One line a built-in rule would type ([module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactLine {
    rule: RuleId,
    fact: String,
    text: String,
}

impl FactLine {
    /// The line from its rule, its fact sentence and the rest of its constant words.
    fn new(rule: RuleId, fact: String, tail: &[&str]) -> Self {
        let mut text = String::from(HEAD);
        text.push_str(&fact);
        for part in tail {
            text.push(' ');
            text.push_str(part);
        }
        text.push_str(" (rule ");
        text.push_str(rule.name());
        text.push(')');
        debug_assert!(
            fact.chars().count() <= FACT_CAP,
            "fact sentence over FACT_CAP"
        );
        debug_assert!(text.chars().count() < LINE_CAP, "line over LINE_CAP");
        Self { rule, fact, text }
    }

    /// The rule that made it.
    #[must_use]
    pub const fn rule(&self) -> RuleId {
        self.rule
    }

    /// Its fact sentence alone (no mark, no advice).
    #[must_use]
    pub fn fact(&self) -> &str {
        &self.fact
    }

    /// The whole line.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// A span as the lines say it: whole minutes under an hour (`30 min`), hours and two-digit
/// minutes past it (`5h06m`). Rounded down.
#[must_use]
pub fn span(ms: i64) -> String {
    let min = ms.max(0) / 60_000;
    if min < 60 {
        format!("{min} min")
    } else {
        format!("{}h{:02}m", min / 60, min % 60)
    }
}

/// THE STUCK FACT'S LINE AT A BREAK (system awareness §14.4, "The background break"): the mark,
/// then the fact's own sentence (`BackgroundCall::told`, a closed template) and a full stop,
/// `[aterm harness] aterm: shell task bplle5dxj (pid 27098) has made no progress for 31 min.`
/// No advice and no rule name: it is a measurement, said once per episode.
#[must_use]
pub fn stuck_line(fact: &str) -> String {
    debug_assert!(
        fact.chars().count() <= FACT_CAP,
        "fact sentence over FACT_CAP"
    );
    format!("{HEAD}{fact}.")
}

/// `wait-nudge@v1`: the hold has stood `held_ms` and the agent has not declared it.
#[must_use]
pub fn wait_nudge(held_ms: i64) -> FactLine {
    let fact = format!(
        "this tab has waited {} on its own background work.",
        span(held_ms)
    );
    FactLine::new(RuleId::WaitNudge, fact, &[DECLARE, ADVICE])
}

/// `hold-room@v1`: the hold has stood `held_ms`; `usable` of `slots` build slots have been free
/// for `free_ms`, and `avail_mib` of memory is available.
#[must_use]
pub fn hold_room(held_ms: i64, usable: u16, slots: u16, free_ms: u64, avail_mib: u64) -> FactLine {
    let verb = if usable == 1 { "has" } else { "have" };
    let fact = format!(
        "this tab has waited {} on its own background work; {usable} of {slots} build slots {verb} \
         been free for {} and {} GiB is available.",
        span(held_ms),
        span(i64::try_from(free_ms).unwrap_or(i64::MAX)),
        avail_mib / 1024,
    );
    FactLine::new(RuleId::HoldRoom, fact, &[ADVICE])
}

#[cfg(test)]
#[path = "templates_tests.rs"]
mod tests;
