// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure decision helpers for typing a due fact line at a drawn break under `prompt`.

use super::reading::OccupancyReading;
use super::rules::{self, HoldSeen, RuleId};
use super::templates::FactLine;
use crate::supervise::Guard;
use crate::supervise::facts::{EpisodeKey, FactKind, SeedOutcome};
use crate::supervise::screen::HumanInput;

/// Ledger rule for the loop's intent to type a fact line before the write.
pub const INTENT_RULE: &str = "fact-intent@v1";

/// Session `status` snapshot read just before writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusSeen {
    /// Server's person stamp (`human_ms=`; [`HumanInput::of_status`]).
    pub human: HumanInput,
    /// Another driver's hand is on the session (`hand=lease:…`).
    pub driver: bool,
    /// The session is halted (`hold=1`).
    pub halted: bool,
    /// Bytes written to the worker stand unread or undrawn (`input=pending|typeahead`).
    pub input_unread: bool,
}

impl StatusSeen {
    /// Parses `StatusSeen` from `status` reply `stdout`.
    #[must_use]
    pub fn of(stdout: &str) -> Option<Self> {
        stdout.lines().find(|l| crate::is_ok_reply(l))?;
        let field = |f: &str| crate::supervise::screen::status_field(stdout, f);
        Some(Self {
            human: HumanInput::of_status(stdout),
            driver: crate::harness::relaunch::driver_hand(stdout),
            halted: field("hold") == Some("1"),
            input_unread: matches!(field("input"), Some("pending" | "typeahead")),
        })
    }

    /// Whether a person's hand may be on the session within `grace_s` (treating missing stamps as present).
    #[must_use]
    pub fn person(&self, grace_s: u32) -> bool {
        self.driver || self.human.within(grace_s) != Some(false)
    }
}

/// Host state observed at the write after the grant and the decider's guards ([`write_held`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AtWrite {
    /// This break's one line went already.
    pub host_line: bool,
    /// The host withholds the session's continuations or owns its turn ends.
    pub withheld: bool,
    /// The upgrade's notice is due (`IdleHost::upgrade_due`).
    pub upgrade_due: bool,
}

/// Returns the first write-time hold on a due line of `rule`, if any.
#[must_use]
pub fn write_held(rule: RuleId, w: AtWrite) -> Option<Guard> {
    let nudge = match rule {
        RuleId::WaitNudge | RuleId::HoldRoom => true,
    };
    use super::fault::{Fault, armed};
    [
        (
            w.host_line && !armed(Fault::StackOnHostLine),
            Guard::HostLine,
        ),
        (w.withheld, Guard::HostWithheld),
        (
            nudge && w.upgrade_due && !armed(Fault::NudgeOverNotice),
            Guard::UpgradeDue,
        ),
    ]
    .into_iter()
    .find_map(|(holds, guard)| holds.then_some(guard))
}

/// Why a host refuses the door its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaceRefusal {
    /// The harness is off for the session at the host.
    Off,
    /// The worker watches beside a self-supervising agent: it takes no step.
    Beside,
    /// The live upgrade's wind-down owns the session's turn ends.
    Owns,
    /// A relaunch's carry-on is owed the session.
    CarryOwed,
}

impl PaceRefusal {
    /// Every refusal.
    pub const ALL: [PaceRefusal; 4] = [
        PaceRefusal::Off,
        PaceRefusal::Beside,
        PaceRefusal::Owns,
        PaceRefusal::CarryOwed,
    ];

    /// Its word (`refused why=<word>`, the tape's).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            PaceRefusal::Off => "off",
            PaceRefusal::Beside => "beside",
            PaceRefusal::Owns => "owns",
            PaceRefusal::CarryOwed => "carry-owed",
        }
    }

    /// The refusal `word` names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.word() == word)
    }
}

/// Host's answer for a line's machine-wide pace and slot reservation (`IdleHost::occupancy_pace`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaceGrant {
    /// Host keeps no machine pace or board; slot-rule lines wait ([`Self::slot_answer`]).
    Local,
    /// Machine pace granted without a board re-read.
    Granted,
    /// Machine pace and slot reservation granted on the re-read board.
    Room(OccupancyReading),
    /// Held by the machine-wide hourly pace.
    Machine,
    /// Held because no free slot remains.
    Reserved,
    /// Held because a slot rule's figures could not be re-read fresh.
    Stale,
    /// Held because a slot rule's terms no longer hold on the re-read board.
    Unmet,
    /// Refused by the host ([`PaceRefusal`]).
    Refused(PaceRefusal),
}

impl PaceGrant {
    /// Whether the line may go.
    #[must_use]
    pub const fn lets(self) -> bool {
        matches!(
            self,
            PaceGrant::Local | PaceGrant::Granted | PaceGrant::Room(_)
        )
    }

    /// Journal and tape word (`room` for [`Self::Room`]).
    #[must_use]
    pub fn word(self) -> String {
        match self {
            PaceGrant::Local => "local".to_string(),
            PaceGrant::Granted => "granted".to_string(),
            PaceGrant::Room(_) => "room".to_string(),
            PaceGrant::Machine => "machine".to_string(),
            PaceGrant::Reserved => "reserved".to_string(),
            PaceGrant::Stale => "stale".to_string(),
            PaceGrant::Unmet => "unmet".to_string(),
            PaceGrant::Refused(why) => format!("refused:{}", why.word()),
        }
    }

    /// The answer `word` names ([`Self::word`]); `room`, which needs its reading, is none.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Some(match word {
            "local" => PaceGrant::Local,
            "granted" => PaceGrant::Granted,
            "machine" => PaceGrant::Machine,
            "reserved" => PaceGrant::Reserved,
            "stale" => PaceGrant::Stale,
            "unmet" => PaceGrant::Unmet,
            _ => PaceGrant::Refused(PaceRefusal::of_word(word.strip_prefix("refused:")?)?),
        })
    }

    /// Evaluates a slot rule's `line` against `self` at `now_ms` and break time `at_ms`.
    #[must_use]
    pub fn slot_answer(
        self,
        line: &FactLine,
        hold: HoldSeen,
        now_ms: i64,
        at_ms: Option<i64>,
    ) -> (Self, FactLine) {
        match self {
            PaceGrant::Room(r) if !r.typeable_at(at_ms) => (PaceGrant::Stale, line.clone()),
            PaceGrant::Room(r) => match rules::line(line.rule(), &r, hold, now_ms) {
                Some(reread) => (self, reread),
                None => (PaceGrant::Unmet, line.clone()),
            },
            PaceGrant::Local => (PaceGrant::Stale, line.clone()),
            _ => (self, line.clone()),
        }
    }
}

/// What the write's outcome row says ([`outcome_reason`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Told {
    /// The line was typed and submitted.
    Typed,
    /// It was not, and why: a closed word.
    Skipped(&'static str),
}

fn episode_words(rule: RuleId, episode: u64, held_ms: i64, declared: bool) -> String {
    super::rules::would_reason(rule, episode, held_ms, declared)
}

/// Formats the reason for a `fact-intent@v1` row ([`INTENT_RULE`]).
#[must_use]
pub fn intent_reason(rule: RuleId, episode: u64, held_ms: i64, declared: bool) -> String {
    episode_words(rule, episode, held_ms, declared)
}

/// Formats the reason for a `fact-hold@v1` outcome row (`typed` or `skipped`).
#[must_use]
pub fn outcome_reason(
    rule: RuleId,
    episode: u64,
    held_ms: i64,
    declared: bool,
    told: Told,
) -> String {
    let words = episode_words(rule, episode, held_ms, declared);
    match told {
        Told::Typed => words,
        Told::Skipped(why) => format!("{words} why={why}"),
    }
}

/// Extracts the door's fact rows for `sid` from ledger `body`, oldest first, for [`FactsState::seed`](crate::supervise::facts::FactsState::seed).
#[must_use]
pub fn seed_rows_in(body: &str, sid: Option<&str>) -> Vec<(EpisodeKey, i64, SeedOutcome)> {
    let hold = FactKind::Hold.rule();
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut rows = Vec::new();
    for line in body.lines() {
        if !(line.contains(INTENT_RULE) || line.contains(hold))
            || want.as_deref().is_some_and(|sid| !line.contains(sid))
        {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        let outcome = match (text("rule_id"), text("decision")) {
            (Some(INTENT_RULE), Some("waiting")) => SeedOutcome::Intent,
            (Some(rule), Some("typed")) if rule == hold => SeedOutcome::Typed,
            (Some(rule), Some("skipped")) if rule == hold => SeedOutcome::Skipped,
            _ => continue,
        };
        let (Some(at_ms), Some(reason)) = (
            v.get("ts").and_then(aterm_json::Value::as_i64),
            text("reason"),
        ) else {
            continue;
        };
        let id = reason
            .split(' ')
            .find_map(|w| w.strip_prefix("episode="))
            .and_then(|hex| u64::from_str_radix(hex, 16).ok());
        if let Some(id) = id {
            let key = EpisodeKey {
                kind: FactKind::Hold,
                id,
            };
            rows.push((key, at_ms, outcome));
        }
    }
    rows
}

#[cfg(test)]
#[path = "door_tests.rs"]
mod tests;
