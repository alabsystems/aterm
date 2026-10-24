// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE DOOR SAYS TO ITS HOST (`docs/DESIGN-harness-occupancy-2026-10-20.md` §6).

use super::door::PaceGrant;
use super::reading::OccupancyReading;
use super::rules::{self, Guards, RuleId};
use crate::harness::disk::Level;
use crate::supervise::Guard;
use crate::supervise::config::Occupancy;
use crate::supervise::memory::Pressure;

/// The open hold as the host is told it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaidHold {
    /// When it began, unix ms.
    pub since_ms: i64,
    /// The agent declared it (`WAITING:`).
    pub declared: bool,
}

/// What a rule last said at a break: the `OCCUPANCY` line's verdict, as a closed word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaidVerdict {
    /// Not due (or nothing said yet this hold).
    Quiet,
    /// Under `observe`: its line would go (a `fact-would@v1` row).
    Would,
    /// A guard held it.
    Held(Guard),
    /// The session's pace held it.
    Paced,
    /// Its cap is spent.
    Spent,
    /// Under `prompt`: typed.
    Typed,
    /// Under `prompt`: its write wrote nothing, and why.
    Skipped(&'static str),
    /// Under `prompt`: the episode was told already.
    Told,
    /// Under `prompt`: the host's pace held or refused it.
    HostPaced(PaceGrant),
}

impl SaidVerdict {
    /// Its words.
    #[must_use]
    pub fn words(self) -> String {
        match self {
            SaidVerdict::Quiet => "quiet".to_string(),
            SaidVerdict::Would => "would".to_string(),
            SaidVerdict::Held(guard) => format!("held:{}", guard.word()),
            SaidVerdict::Paced => "paced".to_string(),
            SaidVerdict::Spent => "spent".to_string(),
            SaidVerdict::Typed => "typed".to_string(),
            SaidVerdict::Skipped(why) => format!("skipped why={why}"),
            SaidVerdict::Told => "told".to_string(),
            SaidVerdict::HostPaced(PaceGrant::Refused(why)) => {
                format!("refused why={}", why.word())
            }
            SaidVerdict::HostPaced(pace) => format!("paced why={}", pace.word()),
        }
    }
}

/// One rule's last word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaidRule {
    /// The rule.
    pub rule: RuleId,
    /// What it last said.
    pub verdict: SaidVerdict,
    /// When it said it, unix ms; 0 for a rule quiet all this hold.
    pub at_ms: i64,
}

/// A DUE LINE PAST K LOOKS UNTOLD, as the host is told it (the slice-2 plan's D2): what the tab's
/// `ready-idle` Entry names, closed words and numbers only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaidReadyIdle {
    /// The rule whose line stood due.
    pub rule: RuleId,
    /// The guard holding it at the last look; `None` where none did (its pace, or a write that
    /// wrote nothing).
    pub guard: Option<Guard>,
    /// When it was first seen due, unix ms.
    pub since_ms: i64,
}

/// THE DOOR'S STATE AS THE HOST IS TOLD IT, once a change ([module docs](self)).
#[derive(Clone, PartialEq, Eq)]
pub struct OccupancySaid {
    /// The mode the break ran under (`[harness] occupancy`).
    pub mode: Occupancy,
    /// The open hold; `None` once it closed.
    pub hold: Option<SaidHold>,
    /// THE READY PROXY (§6).
    pub ready: bool,
    /// The guards the last break judged (§2.1's set, [`rules::Guards`]).
    pub guards: Option<Guards>,
    /// Each rule's last word, in [`RuleId::ALL`]'s order.
    pub rules: Vec<SaidRule>,
    /// When this was told, unix ms. Not part of the change.
    pub at_ms: i64,
    /// The reading the break's decider judged (the empty reading where none was asked).
    pub reading: OccupancyReading,
    /// A due line of the open hold past K looks untold ([`SaidReadyIdle`]); always `None` in a
    /// replay of a recording without slice 2's doors (`Fx::slice2_recorded`), so its told words
    /// compare as recorded.
    pub ready_idle: Option<SaidReadyIdle>,
}

/// The derived form's bytes, with `ready_idle` printed only where it says something: a word with
/// none prints as slice 1's did, so a tape that compares a told word's `Debug` replays unchanged
/// (the slice-2 plan's D7).
impl std::fmt::Debug for OccupancySaid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("OccupancySaid");
        s.field("mode", &self.mode)
            .field("hold", &self.hold)
            .field("ready", &self.ready)
            .field("guards", &self.guards)
            .field("rules", &self.rules)
            .field("at_ms", &self.at_ms)
            .field("reading", &self.reading);
        if let Some(ready_idle) = &self.ready_idle {
            s.field("ready_idle", ready_idle);
        }
        s.finish()
    }
}

impl OccupancySaid {
    /// Whether `other` says what this says.
    #[must_use]
    pub fn same_change(&self, other: &Self) -> bool {
        self.mode == other.mode
            && self.hold == other.hold
            && self.ready == other.ready
            && self.guards == other.guards
            && self.rules == other.rules
            && self.ready_idle == other.ready_idle
    }

    /// The due line past K looks the tab's `ready-idle` Entry names: under `prompt` alone, with
    /// a hold open (the loop's half of the plan's W4; the GUI re-checks its own copy of the
    /// owner's mode).
    #[must_use]
    pub fn ready_idle_under_prompt(&self) -> Option<SaidReadyIdle> {
        if self.mode != Occupancy::Prompt || self.hold.is_none() {
            return None;
        }
        self.ready_idle
    }

    /// The first guard the last break judged holding a line.
    #[must_use]
    pub fn guard(&self) -> Option<Guard> {
        self.guards
            .and_then(|guards| guards.held(RuleId::WaitNudge))
    }

    /// The hold's word for the status line: `declared`, `undeclared` or `none`.
    #[must_use]
    pub const fn hold_word(&self) -> &'static str {
        match self.hold {
            Some(SaidHold { declared: true, .. }) => "declared",
            Some(SaidHold {
                declared: false, ..
            }) => "undeclared",
            None => "none",
        }
    }

    /// The rules whose cap is spent while a hold is open under `prompt`.
    #[must_use]
    pub fn spent_under_prompt(&self) -> Vec<RuleId> {
        if self.mode != Occupancy::Prompt || self.hold.is_none() {
            return Vec::new();
        }
        self.rules
            .iter()
            .filter(|r| r.verdict == SaidVerdict::Spent)
            .map(|r| r.rule)
            .collect()
    }
}

/// THE RULES A TOLD WORD CARRIES ([`OccupancySaid::rules`]), in [`RuleId::ALL`]'s order: every
/// rule, for a recording whose host had slice 2's doors (`slice2`, `Fx::slice2_recorded`), and of
/// an older recording only the rules its build had ([`RuleId::slice1`]), so its told words replay
/// byte for byte. Every rule is slice 1's so far: both lists are [`RuleId::ALL`].
#[must_use]
pub fn told_rules(slice2: bool) -> Vec<RuleId> {
    RuleId::ALL
        .into_iter()
        .filter(|rule| slice2 || rule.slice1())
        .collect()
}

/// A rule's cap, as an Entry says it: `3 nudges today` or `4 hold-room asks this hold`.
#[must_use]
pub fn cap_words(rule: RuleId) -> String {
    match rule {
        RuleId::WaitNudge => format!("{} nudges today", rules::NUDGES_PER_DAY),
        RuleId::HoldRoom => format!("{} hold-room asks this hold", rules::HOLD_ROOM_PER_HOLD),
    }
}

/// `ms` as the `why` lines spell a span: `5h25m`, `20m`, `45s`.
#[must_use]
pub fn span_words(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    match (s / 3600, (s % 3600) / 60) {
        (0, 0) => format!("{s}s"),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h{m:02}m"),
    }
}

/// `at_ms` (unix) as a clock at `offset_s` east of UTC: `HH:MM`.
#[must_use]
pub fn clock_words(at_ms: i64, offset_s: i64) -> String {
    let day_s = 86_400;
    let local_s = (at_ms.div_euclid(1000) + offset_s).rem_euclid(day_s);
    format!("{:02}:{:02}", local_s / 3600, (local_s % 3600) / 60)
}

fn mark(holds: bool) -> &'static str {
    if holds { "✓" } else { "✗" }
}

/// THE `occupancy why` FORM of `said` at `now_ms` (unix).
#[must_use]
pub fn why_lines(said: &OccupancySaid, now_ms: i64, offset_s: i64) -> Vec<String> {
    let mut lines = Vec::new();
    let held_ms = said
        .hold
        .map(|hold| now_ms.saturating_sub(hold.since_ms).max(0));
    lines.push(match (said.hold, held_ms) {
        (Some(hold), Some(held_ms)) => format!(
            "occupancy={} hold {} since {} held {} ready={}",
            said.mode.word(),
            said.hold_word(),
            clock_words(hold.since_ms, offset_s),
            span_words(held_ms),
            u8::from(said.ready)
        ),
        _ => format!("occupancy={} hold none", said.mode.word()),
    });
    let held = held_ms.unwrap_or(0);
    let r = &said.reading;
    for said_rule in &said.rules {
        let when = if said_rule.at_ms > 0 {
            format!(" {}", clock_words(said_rule.at_ms, offset_s))
        } else {
            String::new()
        };
        let head = format!(
            "{} rev=builtin {}{when}:",
            said_rule.rule.name(),
            said_rule.verdict.words()
        );
        let terms = match said_rule.rule {
            RuleId::WaitNudge => format!(
                "hold {} {}  undeclared {}",
                span_words(held),
                mark(held >= rules::wait_nudge_after_ms()),
                mark(said.hold.is_some_and(|h| !h.declared))
            ),
            RuleId::HoldRoom => {
                let slots = r
                    .slots
                    .map_or_else(|| "-".to_string(), |s| format!("{}/{}", s.usable, s.n));
                let free = r.slots_free_ms.map_or_else(
                    || "-".to_string(),
                    |ms| span_words(i64::try_from(ms).unwrap_or(i64::MAX)),
                );
                let avail = r
                    .avail_mib
                    .map_or_else(|| "-".to_string(), |mib| format!("{}GiB", mib >> 10));
                let disk = r.disk.map_or("unknown", Level::as_str);
                let mem = match r.pressure {
                    Some(Pressure::Normal) => "normal",
                    Some(Pressure::Warn) => "warn",
                    Some(Pressure::Critical) => "critical",
                    None => "unknown",
                };
                format!(
                    "hold {} {}  {} {slots} {free} {}  avail {avail} {}  disk {disk} {}  mem {mem} {}  \
                     slot held this hold {}  board {}",
                    span_words(held),
                    mark(held >= rules::HOLD_ROOM_AFTER_MS),
                    rules::HOLD_ROOM_SLOTS.trim_start_matches("sys "),
                    mark(r.slots_free_ms.is_some() && r.slots.is_some_and(|s| s.usable >= 1)),
                    mark(r.avail_mib.is_some_and(|mib| mib >= 8 << 10)),
                    mark(r.disk == Some(Level::Ok)),
                    mark(r.pressure.is_some_and(|p| p != Pressure::Critical)),
                    mark(r.held_slot_within(held)),
                    if r.fresh() { "fresh ✓" } else { "stale ✗" }
                )
            }
        };
        lines.push(format!("{head} {terms}"));
    }
    lines.push(guards_line(said.guards));
    lines
}

fn guards_line(guards: Option<Guards>) -> String {
    let Some(guards) = guards else {
        return "  guards: none judged".to_string();
    };
    let brake = guards.brake.map_or_else(
        || format!("brakes none {}", mark(true)),
        |brake| format!("brakes {} {}", brake.word(), mark(false)),
    );
    let each: Vec<String> = guards
        .each(RuleId::WaitNudge)
        .iter()
        .map(|(guard, holds)| match guard {
            Guard::Autopilot => format!("autopilot {} {}", guards.mode.word(), mark(!holds)),
            _ => format!("{} {}", guard.word(), mark(!holds)),
        })
        .collect();
    format!("  guards: {brake}  {}", each.join("  "))
}

#[cfg(test)]
#[path = "said_tests.rs"]
mod tests;
