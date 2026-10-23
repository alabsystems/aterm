// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE DOOR SAYS TO ITS HOST (`docs/DESIGN-harness-occupancy-2026-10-20.md` §6): the
//! loop's one notification a change, [`OccupancySaid`] (`IdleHost::occupancy_said`, taped), and
//! its `occupancy why` form ([`why_lines`]). The loop composes it from what it holds already —
//! the mode it ran the break under, the hold's clock, what each rule last said, the guards the
//! break judged and the reading the decider judged — and reads nothing for it, so under
//! `observe` it changes no decision and types nothing. Plain numbers and closed words, as the
//! reading is.
//!
//! The host keeps the last one a session's loop told: its status fields
//! (`occupancy= occupancy_hold=`), the `why` it answers, and the Entries a spent cap raises
//! under `prompt`.

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
    /// Its words, as the `OCCUPANCY` line spells them: `quiet`, `would`, `held:<guard>`,
    /// `paced`, `spent`, `typed`, `skipped why=<word>`, `told`, `paced why=<word>` or
    /// `refused why=<word>`.
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

/// THE DOOR'S STATE AS THE HOST IS TOLD IT, once a change ([module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OccupancySaid {
    /// The mode the break ran under (`[harness] occupancy`).
    pub mode: Occupancy,
    /// The open hold; `None` once it closed.
    pub hold: Option<SaidHold>,
    /// THE READY PROXY (§6): the hold past `hold-room`'s two hours with its machine terms met
    /// on the reading the break judged ([`rules::ready`]); false where none was asked.
    pub ready: bool,
    /// The guards the last break judged (§2.1's set, [`rules::Guards`]), whether or not a rule
    /// was due: why nothing fired. `None` before a break was judged.
    pub guards: Option<Guards>,
    /// Each rule's last word, in [`RuleId::ALL`]'s order.
    pub rules: [SaidRule; 2],
    /// When this was told, unix ms. Not part of the change.
    pub at_ms: i64,
    /// The reading the break's decider judged (the empty reading where none was asked). Not
    /// part of the change: a new reading alone tells nothing.
    pub reading: OccupancyReading,
}

impl OccupancySaid {
    /// Whether `other` says what this says: the mode, the hold, the ready proxy, each guard
    /// and each rule's word. The stamp and the reading are not the change.
    #[must_use]
    pub fn same_change(&self, other: &Self) -> bool {
        self.mode == other.mode
            && self.hold == other.hold
            && self.ready == other.ready
            && self.guards == other.guards
            && self.rules == other.rules
    }

    /// The first guard the last break judged holding a line ([`rules::Guards::held`] of
    /// `wait-nudge`; both built-ins ask the one set): what the ring's row names.
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

    /// The rules whose cap is spent while a hold is open under `prompt`: what an
    /// `occupancy.hold` Entry names. Under `observe` or `off` none (nothing was typed, so
    /// nothing was spent of what the owner sees).
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

/// A term's mark: `✓` where it holds, `✗` where it does not.
fn mark(holds: bool) -> &'static str {
    if holds { "✓" } else { "✗" }
}

/// THE `occupancy why` FORM of `said` at `now_ms` (unix), clocks at `offset_s` (§6): the mode
/// and the hold, then a line a rule — its verdict, when, and each of its terms with its mark —
/// and the guards' line, each guard with its mark. Plain words and numbers ([module docs](self)).
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
                mark(held >= rules::WAIT_NUDGE_AFTER_MS),
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

/// `why`'s guards line (§6): each guard the last break judged with its mark, `✗` where it
/// holds a line — the break's brake first, then [`Guards::each`] of `wait-nudge` (the
/// autopilot's with its mode) — or `none judged` before a break was.
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
