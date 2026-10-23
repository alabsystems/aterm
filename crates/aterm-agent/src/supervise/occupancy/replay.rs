// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Replay of occupancy rules over recorded session ring and hold rows. The nudge's wait is
//! this process's ([`rules::wait_nudge_after_ms`]).

use super::hold::RowSaid;
use super::ring::RingRow;
use super::rules::{self, Books, Guards, HoldSeen, RuleId, Verdict};
use super::said::{SaidVerdict, clock_words};
use crate::supervise::Guard;
use crate::supervise::config::Autopilot;

/// One hold as the ledger's rows tell it, unix ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldSpan {
    /// When it began.
    pub since_ms: i64,
    /// The first row that said it declared; `None` for undeclared.
    pub declared_ms: Option<i64>,
    /// When it closed; `None` while open.
    pub end_ms: Option<i64>,
}

impl HoldSpan {
    /// Whether the hold stood at `at_ms`.
    #[must_use]
    pub fn stands_at(&self, at_ms: i64) -> bool {
        self.since_ms <= at_ms && self.end_ms.is_none_or(|end| at_ms < end)
    }
}

const SAME_HOLD_MS: i64 = 60_000;

/// Rebuild [`HoldSpan`] intervals from ordered ledger hold `rows`.
#[must_use]
pub fn spans_of(rows: &[(i64, RowSaid)]) -> Vec<HoldSpan> {
    let mut spans: Vec<HoldSpan> = Vec::new();
    let mut open: Option<usize> = None;
    for (at_ms, said) in rows {
        match *said {
            RowSaid::Open { held_ms, declared } => {
                let since_ms = at_ms.saturating_sub(held_ms);
                let same = open.is_some_and(|i| {
                    spans.get(i).is_some_and(|s| {
                        s.since_ms.abs_diff(since_ms) <= SAME_HOLD_MS.unsigned_abs()
                    })
                });
                if !same {
                    if let Some(span) = open.and_then(|i| spans.get_mut(i)) {
                        span.end_ms = Some(since_ms.max(span.since_ms));
                    }
                    spans.push(HoldSpan {
                        since_ms,
                        declared_ms: None,
                        end_ms: None,
                    });
                    open = Some(spans.len() - 1);
                }
                if declared && let Some(span) = open.and_then(|i| spans.get_mut(i)) {
                    span.declared_ms.get_or_insert(*at_ms);
                }
            }
            RowSaid::Declared { .. } => {
                if let Some(span) = open.and_then(|i| spans.get_mut(i)) {
                    span.declared_ms.get_or_insert(*at_ms);
                }
            }
            RowSaid::Closed { held_ms, .. } => match open.take().and_then(|i| spans.get_mut(i)) {
                Some(span) => span.end_ms = Some(*at_ms),
                None => spans.push(HoldSpan {
                    since_ms: at_ms.saturating_sub(held_ms),
                    declared_ms: None,
                    end_ms: Some(*at_ms),
                }),
            },
        }
    }
    spans
}

/// One change of a rule's verdict in the replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Replayed {
    /// The look it changed at, unix ms.
    pub at_ms: i64,
    /// The rule.
    pub rule: RuleId,
    /// What it said: [`SaidVerdict::Would`], `Held`, `Paced` or `Spent`.
    pub verdict: SaidVerdict,
    /// When the hold it was said in began, unix ms.
    pub hold_since_ms: i64,
}

fn guard_of(word: &str) -> Option<Guard> {
    Guard::ALL.into_iter().find(|g| g.word() == word)
}

/// Replay occupancy verdicts for session `sid` over `rows` from `from_ms` onward.
#[must_use]
pub fn replay(rows: &[RingRow], spans: &[HoldSpan], sid: &str, from_ms: i64) -> Vec<Replayed> {
    let mut books = Books::default();
    let mut said: [Option<SaidVerdict>; RuleId::COUNT] = [None; RuleId::COUNT];
    let mut out = Vec::new();
    for row in rows.iter().filter(|row| row.at >= from_ms) {
        let at = row.at;
        let Some(span) = spans.iter().find(|span| span.stands_at(at)) else {
            said = [None; RuleId::COUNT];
            continue;
        };
        if books.hold_since_ms() != Some(span.since_ms) {
            said = [None; RuleId::COUNT];
        }
        books.hold(span.since_ms);
        let hold = HoldSeen {
            since_ms: span.since_ms,
            declared: span.declared_ms.is_some_and(|d| d <= at),
        };
        let mut guards = Guards::clear(Autopilot::Cruise);
        guards.brake = row
            .session(sid)
            .and_then(|s| s.guard.as_deref())
            .and_then(guard_of);
        let reading = row.reading(sid);
        let verdicts = rules::decide(
            &reading,
            Some(hold),
            &books,
            &guards,
            rules::wait_nudge_after_ms(),
            at,
        );
        for (rule, verdict) in verdicts {
            let now = match verdict {
                Verdict::Quiet => None,
                Verdict::Spent => Some(SaidVerdict::Spent),
                Verdict::Held(guard) => Some(SaidVerdict::Held(guard)),
                Verdict::Paced { .. } => Some(SaidVerdict::Paced),
                Verdict::Due(_) => Some(SaidVerdict::Would),
            };
            if now == Some(SaidVerdict::Would) {
                books.fired(rule, at, true);
            }
            let last = &mut said[rule.index()];
            if *last == now {
                continue;
            }
            *last = now;
            if let Some(verdict) = now {
                out.push(Replayed {
                    at_ms: at,
                    rule,
                    verdict,
                    hold_since_ms: span.since_ms,
                });
            }
        }
    }
    out
}

/// Format replay output lines with timestamps at `offset_s` east of UTC.
#[must_use]
pub fn replay_lines(
    replayed: &[Replayed],
    looks: usize,
    holds: usize,
    (from_ms, to_ms): (i64, i64),
    offset_s: i64,
) -> Vec<String> {
    let mut lines = vec![format!(
        "replay from {} to {} looks={looks} holds={holds}",
        clock_words(from_ms, offset_s),
        clock_words(to_ms, offset_s)
    )];
    for r in replayed {
        lines.push(format!(
            "{} {} {}",
            clock_words(r.at_ms, offset_s),
            r.rule.name(),
            r.verdict.words()
        ));
    }
    for rule in RuleId::ALL {
        let first = replayed
            .iter()
            .find(|r| r.rule == rule && r.verdict == SaidVerdict::Would)
            .map_or_else(|| "never".to_string(), |r| clock_words(r.at_ms, offset_s));
        lines.push(format!("{} first would {first}", rule.name()));
    }
    lines
}

/// Parse a `check --replay` duration (`<n>s`, `<n>m`, `<n>h`, or `<n>d`) into milliseconds.
#[must_use]
pub fn window_ms(word: &str) -> Option<i64> {
    let word = word.trim();
    let unit = word.chars().last()?;
    let n: i64 = word.get(..word.len() - unit.len_utf8())?.parse().ok()?;
    let ms = n.checked_mul(match unit {
        's' => 1000,
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => return None,
    })?;
    (ms > 0 && ms <= rules::DAY_MS + rules::DAY_MS / 2).then_some(ms)
}

#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;
