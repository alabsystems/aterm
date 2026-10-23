// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DOOR'S WRITE UNDER `prompt` (`docs/DESIGN-harness-occupancy-2026-10-20.md` §2.1; the
//! red-team's F1, F10 and F11): the pure parts of typing a due line at a drawn break, once the
//! owner's grant is in hand.

use super::rules::RuleId;
use crate::supervise::Guard;
use crate::supervise::facts::{EpisodeKey, FactKind, SeedOutcome};
use crate::supervise::screen::HumanInput;

/// The ledger rule the loop's intent to type a fact line is booked under, before the write: an
/// intent with no outcome after it is an episode the loop may never have told.
pub const INTENT_RULE: &str = "fact-intent@v1";

/// THE `status` READ JUST BEFORE THE WRITE, as the door judges it: whose hands are on the session
/// and whether input stands unread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusSeen {
    /// The server's person stamp (`human_ms=`; [`HumanInput::of_status`]).
    pub human: HumanInput,
    /// Another driver's hand is on the session (`hand=lease:…` not this harness's, or a turn's).
    pub driver: bool,
    /// The session is halted (`hold=1`).
    pub halted: bool,
    /// Bytes written to the worker stand unread or undrawn (`input=pending|typeahead`): a line
    /// typed now would land behind them, as a draft would.
    pub input_unread: bool,
}

impl StatusSeen {
    /// The `status` reply `stdout`, judged.
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

    /// Whether a person's hand may be on the session within `grace_s` (`[harness] human_grace_s`):
    /// a driver's, or their keys — and a server that sends no stamp counts as someone, the safe way
    /// to be wrong where the loop would type.
    #[must_use]
    pub fn person(&self, grace_s: u32) -> bool {
        self.driver || self.human.within(grace_s) != Some(false)
    }
}

/// What the write itself finds, after the grant and the decider's guards ([`write_held`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AtWrite {
    /// This break's one line went already: the host's (§2.1: the upgrade's notice outranks the
    /// nudges).
    pub host_line: bool,
    /// The host withholds the session's continuations or owns its turn ends.
    pub withheld: bool,
    /// The upgrade's notice is due (`IdleHost::upgrade_due`).
    pub upgrade_due: bool,
}

/// THE WRITE'S OWN HOLDS on a due line of `rule`, in order: this break's line gone
/// ([`Guard::HostLine`]), the host's withhold ([`Guard::HostWithheld`]), the upgrade's notice due
/// ([`Guard::UpgradeDue`]; both built-in rules are nudges, which wait for it: `OccupancyDoor`'s
/// `Type` asks `due == 2 || upg == 0`).
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

/// Why a host refuses the door its line (the red-team's F10): closed words, each the host's own
/// state.
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

/// THE HOST'S ANSWER FOR A LINE'S PACE (`IdleHost::occupancy_pace`, design §4.2): the machine-wide
/// pace and, for a slot rule, one free slot reserved for the line's session, taken together — or
/// what holds the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaceGrant {
    /// The host keeps no machine pace (the default host): the session's own pace, which the decider
    /// judged, is the line's.
    Local,
    /// Taken: the machine's pace, and the reservation where the rule needs a slot.
    Granted,
    /// The machine's pace holds the line: the lines of the last hour across the machine.
    Machine,
    /// No free slot is left to reserve: others hold them, or an older hold waits for one.
    Reserved,
    /// The host refuses the line ([`PaceRefusal`]).
    Refused(PaceRefusal),
}

impl PaceGrant {
    /// Whether the line may go.
    #[must_use]
    pub const fn lets(self) -> bool {
        matches!(self, PaceGrant::Local | PaceGrant::Granted)
    }

    /// Its word (the journal's, the tape's).
    #[must_use]
    pub fn word(self) -> String {
        match self {
            PaceGrant::Local => "local".to_string(),
            PaceGrant::Granted => "granted".to_string(),
            PaceGrant::Machine => "machine".to_string(),
            PaceGrant::Reserved => "reserved".to_string(),
            PaceGrant::Refused(why) => format!("refused:{}", why.word()),
        }
    }

    /// The answer `word` names ([`Self::word`]).
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Some(match word {
            "local" => PaceGrant::Local,
            "granted" => PaceGrant::Granted,
            "machine" => PaceGrant::Machine,
            "reserved" => PaceGrant::Reserved,
            _ => PaceGrant::Refused(PaceRefusal::of_word(word.strip_prefix("refused:")?)?),
        })
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

/// The episode's words every fact row of the door carries: `rule=<name> episode=<hex>
/// held_ms=<n>`, with ` declared` for a declared hold — the shape `fact-would@v1`'s rows carry
/// ([`super::rules::would_reason`]), so the books seed from either.
fn episode_words(rule: RuleId, episode: u64, held_ms: i64, declared: bool) -> String {
    super::rules::would_reason(rule, episode, held_ms, declared)
}

/// The reason a `fact-intent@v1` row carries ([`INTENT_RULE`]): the episode's words.
#[must_use]
pub fn intent_reason(rule: RuleId, episode: u64, held_ms: i64, declared: bool) -> String {
    episode_words(rule, episode, held_ms, declared)
}

/// The reason the outcome's `fact-hold@v1` row carries (`typed` or `skipped`): the episode's words,
/// and for a skip `why=<word>`.
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

/// THE DOOR'S FACT ROWS OF SESSION `sid` in the ledger text `body`, oldest first, for
/// [`FactsState::seed`](crate::supervise::facts::FactsState::seed): each `fact-intent@v1` row an
/// intent, each `fact-hold@v1` row `typed` or `skipped` that carries an episode its outcome.
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
