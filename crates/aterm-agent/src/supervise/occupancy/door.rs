// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure decision helpers for typing a due fact line at a drawn break under `prompt`, and for the
//! hung-call stuck fact under `[harness] stuck_break_note` ([`BreakLine::Stuck`],
//! [`stuck_at_break`]).

use super::reading::OccupancyReading;
use super::rules::{self, Books, Guards, HoldSeen, RuleId};
use super::templates::FactLine;
use crate::supervise::Guard;
use crate::supervise::config::{Autopilot, StuckBreakGrant};
use crate::supervise::facts::{EpisodeKey, FactKind, Left, SeedOutcome, Settled};
use crate::supervise::screen::HumanInput;

/// Ledger rule for the loop's intent to type a fact line before the write.
pub const INTENT_RULE: &str = "fact-intent@v1";

/// Session `status` snapshot read just before writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusSeen {
    /// Server's person stamp (`human_ms=`; [`HumanInput::of_status`]).
    pub human: HumanInput,
    /// Another driver's hand is on the session (`hand=lease:…` not this harness's, or a turn's).
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

    /// Whether a person's hand may be on the session within `grace_s` (a missing stamp counts as
    /// present).
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

/// A line at a break (§2.1's order): a built-in rule's (rank 5), or the hung-call stuck fact
/// (rank 4: after the upgrade's notice, before the nudges).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakLine {
    /// A built-in rule's line under `prompt`.
    Rule(RuleId),
    /// The stuck fact under `[harness] stuck_break_note`.
    Stuck,
}

impl From<RuleId> for BreakLine {
    fn from(rule: RuleId) -> Self {
        BreakLine::Rule(rule)
    }
}

impl BreakLine {
    /// The autopilots its lines are for: a rule's ([`rules::modes`]); for the stuck fact, every
    /// one but Off (system awareness §14.4).
    #[must_use]
    pub const fn modes(self) -> &'static [Autopilot] {
        match self {
            BreakLine::Rule(rule) => rules::modes(rule),
            BreakLine::Stuck => &[Autopilot::Cruise, Autopilot::Land],
        }
    }
}

/// Returns the first write-time hold on a due `line`, if any: the break's line gone, the host's
/// withhold, then the upgrade's notice due (rank 3, which every nudge and the stuck fact wait
/// for; a wake's line goes over it, the model's `due == 2 || upg == 0`).
#[must_use]
pub fn write_held(line: impl Into<BreakLine>, w: AtWrite) -> Option<Guard> {
    use super::fault::{Fault, armed};
    let waits = match line.into() {
        BreakLine::Rule(RuleId::WaitNudge | RuleId::HoldRoom) => !armed(Fault::NudgeOverNotice),
        BreakLine::Rule(RuleId::Wake) => false,
        BreakLine::Stuck => !armed(Fault::StuckOverNotice),
    };
    [
        (
            w.host_line && !armed(Fault::StackOnHostLine),
            Guard::HostLine,
        ),
        (w.withheld, Guard::HostWithheld),
        (waits && w.upgrade_due, Guard::UpgradeDue),
    ]
    .into_iter()
    .find_map(|(holds, guard)| holds.then_some(guard))
}

/// WHETHER THE HOST'S OWN STEP MAY GO AT A BREAK (`IdleHost::at_background`: the upgrade's notice,
/// §2.1's rank 3), the break's due line being `due`'s (the decider's pick, under `prompt`): not
/// before a wake's line, which ranks first (the model's `2 > due || due > 2`). The wake's write
/// goes first (`Session::wake_first`), and a line it types holds the host's step to the next break.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "OccupancyDoor",
        action = "UpgradeTypes",
        project = "conformance_occupancy::project"
    )
)]
pub fn host_step_may_go(due: Option<RuleId>) -> bool {
    use super::fault::{Fault, armed};
    due != Some(RuleId::Wake) || armed(Fault::HostBeforeWake)
}

/// What the stuck fact's line comes to at a break ([`stuck_at_break`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StuckVerdict {
    /// The owner's key is off (`[harness] stuck_break_note`, or `facts = false`): nothing.
    Ungranted,
    /// This guard holds it.
    Held(Guard),
    /// The session's pace holds it until `next_ms` (the clock's).
    Paced {
        /// When the pace lets a line go.
        next_ms: i64,
    },
    /// It goes: its intent row, then the fenced write.
    Due,
}

/// Judges the stuck fact at a break (§2.1's rank 4, for an untold episode on a fresh reading):
/// ungranted without the key, else held by `g` for its autopilots ([`BreakLine::modes`]), then
/// by `w` ([`write_held`]), then by the session's pace ([`rules::pace_next`]); else due. Asked at
/// the decider and again at the write.
#[must_use]
pub fn stuck_at_break(
    grant: Option<&StuckBreakGrant>,
    g: &Guards,
    w: AtWrite,
    books: &Books,
    now_ms: i64,
) -> StuckVerdict {
    use super::fault::{Fault, armed};
    if grant.is_none() && !armed(Fault::StuckWithoutKey) {
        return StuckVerdict::Ungranted;
    }
    if let Some(guard) = g
        .held_for(BreakLine::Stuck)
        .or_else(|| write_held(BreakLine::Stuck, w))
    {
        return StuckVerdict::Held(guard);
    }
    match rules::pace_next(books, now_ms) {
        Some(next_ms) => StuckVerdict::Paced { next_ms },
        None => StuckVerdict::Due,
    }
}

/// The door a fact of a kind other than the hold was told at, as its rows name it (`door=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactDoor {
    /// A line of its own at a break (`[harness] stuck_break_note`).
    Break,
    /// Riding a prompt typed at a turn end (`policy::turn_end::FACT_RIDES`).
    TurnEnd,
}

impl FactDoor {
    /// Its word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            FactDoor::Break => "break",
            FactDoor::TurnEnd => "turn-end",
        }
    }
}

/// The `fact-intent@v1` reason for an episode of a kind other than the hold:
/// `kind=<word> episode=<hex> door=<word>`, which [`seed_rows_in`] reads back.
#[must_use]
pub fn fact_intent_reason(key: EpisodeKey, door: FactDoor) -> String {
    format!(
        "kind={} episode={:016x} door={}",
        key.kind.word(),
        key.id,
        door.word()
    )
}

/// The outcome row's reason (`fact-<kind>@v1`): the intent's words, and for a skip `why=<word>`.
#[must_use]
pub fn fact_outcome_reason(key: EpisodeKey, door: FactDoor, told: Told) -> String {
    let words = fact_intent_reason(key, door);
    match told {
        Told::Typed => words,
        Told::Skipped(why) => format!("{words} why={why}"),
    }
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
    /// Held because no free slot is left for this line: others hold them, or an older hold waits
    /// for one.
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

/// The reason of a `fact-wake@v1` row `waiting` for wake `ord` of `episode` told by the agent's
/// own `await wake`, the loop having seen a turn run after its answer (the slice-2 plan's S2,
/// "heard"): `rule=wake@v1 episode=<hex> ord=<n> told door=await`. Nothing was typed;
/// [`seed_rows_in`] reads it as [`SeedOutcome::Answered`].
#[must_use]
pub fn wake_heard_reason(ord: u16, episode: u64) -> String {
    format!(
        "rule={} episode={episode:016x} ord={ord} told door={}",
        super::wakes_seen::WAKE_RULE,
        super::wakes_seen::WakeDoor::Await.word()
    )
}

/// The reason of a wake's `fact-intent@v1` row at `door`, a break's or an idle point's (`waiting`,
/// before the claim and the write): `kind=wake episode=<hex> ord=<n> held_ms=<n> door=<word>`,
/// which [`seed_rows_in`] reads as the wake's intent.
#[must_use]
pub fn wake_intent_reason(
    ord: u16,
    episode: u64,
    held_ms: i64,
    door: super::wakes_seen::WakeDoor,
) -> String {
    format!(
        "kind={} episode={episode:016x} ord={ord} held_ms={held_ms} door={}",
        FactKind::Wake.word(),
        door.word()
    )
}

/// The words a wake's line booked at `door` carries: `rule=wake@v1 episode=<hex> held_ms=<n>
/// ord=<n> door=<word>`, the reason of its `fact-wake@v1` row typed there
/// ([`wake_outcome_reason`]) and, under `observe`, of the `fact-would@v1` row an idle point's act
/// would have carried it on (the slice-2 plan's 2.9), which [`super::rules::would_rows_in`] books
/// alike.
#[must_use]
pub fn wake_line_words(
    ord: u16,
    episode: u64,
    held_ms: i64,
    door: super::wakes_seen::WakeDoor,
) -> String {
    format!(
        "{} ord={ord} door={}",
        super::rules::would_reason(RuleId::Wake, episode, held_ms, false),
        door.word()
    )
}

/// The reason of a wake's `fact-wake@v1` outcome row at `door`, `typed` or `skipped`: its line's
/// words ([`wake_line_words`]), and for a skip `why=<word>`. [`seed_rows_in`] reads its episode's
/// outcome, [`super::rules::would_rows_in`] a typed one's place in the session's pace and
/// [`break_lines_in`] a break's line (a point's rides an act: no line of its own).
#[must_use]
pub fn wake_outcome_reason(
    ord: u16,
    episode: u64,
    held_ms: i64,
    door: super::wakes_seen::WakeDoor,
    told: Told,
) -> String {
    let words = wake_line_words(ord, episode, held_ms, door);
    match told {
        Told::Typed => words,
        Told::Skipped(why) => format!("{words} why={why}"),
    }
}

/// THE DOOR THAT LEFT A LINE in the composer ([`Left`]; the slice-2 plan's 2.9b), with the words
/// its rows carry ([`left_reason`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftRow {
    /// A built-in rule's line at a break (`fact-hold@v1`, slice 1's door under `prompt`).
    Hold {
        rule: RuleId,
        /// How long the hold had held at the write.
        held_ms: i64,
        declared: bool,
        /// The hold's start on the loop's clock, where this loop wrote it (`None`: a seed's).
        since_ms: Option<i64>,
    },
    /// A wake's line (`fact-wake@v1`) under `prompt`, at a break or riding a point's act, its
    /// claim held through `door`.
    Wake {
        ord: u16,
        /// How long the hold had held at the claim (`0`: none stood).
        held_ms: i64,
        door: super::wakes_seen::WakeDoor,
    },
    /// A hung call's fact (`fact-no-progress@v1`): the stuck line at a break, under the owner's
    /// `[harness] stuck_break_note` grant, or riding a turn end's act under `[harness] facts`.
    Fact { door: FactDoor },
}

impl LeftRow {
    /// Whether the grant that typed it is still in force, where `prompt` says whether the
    /// owner's `prompt` is and `stuck` whether the `stuck_break_note` grant is: a rule's or a
    /// wake's line was typed under `prompt`, the stuck line at a break under `stuck_break_note`,
    /// and a fact riding a turn end's act rode the policy's own line, which needs neither.
    #[must_use]
    pub const fn granted(self, prompt: bool, stuck: bool) -> bool {
        match self {
            LeftRow::Hold { .. } | LeftRow::Wake { .. } => prompt,
            LeftRow::Fact {
                door: FactDoor::Break,
            } => stuck,
            LeftRow::Fact {
                door: FactDoor::TurnEnd,
            } => true,
        }
    }

    /// Whether it rode an act the policy typed (a point's or a turn end's), so its line is the
    /// fact's sentence alone, not a line of its own.
    #[must_use]
    pub const fn rides(self) -> bool {
        matches!(
            self,
            LeftRow::Wake {
                door: super::wakes_seen::WakeDoor::Point,
                ..
            } | LeftRow::Fact {
                door: FactDoor::TurnEnd
            }
        )
    }
}

/// What a left line's row says ([`left_reason`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftSaid {
    /// Left in the composer at the session's submit count `submits` (`None`: unserved): a
    /// `waiting` row, `left [submits=<n>]`.
    Left { submits: Option<u64> },
    /// Settled at a look: told (a `typed` row, `settled=<how>`) or released (a `skipped` row,
    /// `why=<why>`).
    Settled(Settled),
}

/// The reason of a left line's row of `key`, the words its door's outcome row carries ([`outcome_reason`],
/// [`wake_line_words`], [`fact_intent_reason`]) and what it says: `left [submits=<n>]`,
/// `settled=submitted|turn|uncounted` or `why=cleared|gone`. [`seed_rows_in`] reads a `left` row as
/// [`SeedOutcome::Left`] and [`left_rows_in`] reads it back whole; every reader of typed rows
/// takes a `settled=` row as the line typed then, and of skips a `why=` row as a skip.
#[must_use]
pub fn left_reason(row: LeftRow, key: EpisodeKey, said: LeftSaid) -> String {
    let words = match row {
        LeftRow::Hold {
            rule,
            held_ms,
            declared,
            ..
        } => episode_words(rule, key.id, held_ms, declared),
        LeftRow::Wake { ord, held_ms, door } => wake_line_words(ord, key.id, held_ms, door),
        LeftRow::Fact { door } => fact_intent_reason(key, door),
    };
    match said {
        LeftSaid::Left { submits: Some(n) } => format!("{words} left submits={n}"),
        LeftSaid::Left { submits: None } => format!("{words} left"),
        LeftSaid::Settled(Settled::Told(how)) => format!("{words} settled={how}"),
        LeftSaid::Settled(Settled::Released(why)) => format!("{words} why={why}"),
    }
}

/// THE LINES A LOOP BEFORE THIS ONE LEFT in session `sid`'s composer, from ledger `body`, oldest
/// first: each `fact-<kind>@v1` row `waiting … left` ([`left_reason`]) read back whole, its row,
/// its line (the row's `command`), its stamp and the submit count it was left at, each read back
/// seeded and uncounted (`Left::seeded`, `Left::counted`: the count is another loop's, perhaps
/// another host process's). A successor keeps those still left
/// ([`crate::supervise::facts::FactsState::seed_left`]).
#[must_use]
pub fn left_rows_in(body: &str, sid: Option<&str>) -> Vec<(EpisodeKey, Left)> {
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut rows = Vec::new();
    for line in body.lines() {
        if !line.contains(" left") || !FactKind::ALL.iter().any(|k| line.contains(k.rule())) {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        let (Some(kind), Some("waiting"), Some(reason), Some(at_ms)) = (
            text("rule_id").and_then(FactKind::of_rule),
            text("decision"),
            text("reason"),
            v.get("ts").and_then(aterm_json::Value::as_i64),
        ) else {
            continue;
        };
        if text("sid").map(str::to_string) != want
            || is_task_row(reason)
            || !reason.split(' ').any(|w| w == "left")
        {
            continue;
        }
        let word = |key: &str| reason.split(' ').find_map(|w| w.strip_prefix(key));
        let Some(id) = word("episode=").and_then(|hex| u64::from_str_radix(hex, 16).ok()) else {
            continue;
        };
        let held_ms = word("held_ms=").and_then(|n| n.parse::<i64>().ok());
        let row = match kind {
            FactKind::Hold => {
                let (Some(rule), Some(held_ms)) =
                    (word("rule=").and_then(RuleId::of_name), held_ms)
                else {
                    continue;
                };
                LeftRow::Hold {
                    rule,
                    held_ms,
                    declared: reason.split(' ').any(|w| w == "declared"),
                    since_ms: None,
                }
            }
            FactKind::Wake => {
                let door = super::wakes_seen::WakeDoor::ALL
                    .into_iter()
                    .find(|d| word("door=") == Some(d.word()));
                let (Some(ord), Some(held_ms), Some(door)) = (
                    word("ord=").and_then(|n| n.parse::<u16>().ok()),
                    held_ms,
                    door,
                ) else {
                    continue;
                };
                LeftRow::Wake { ord, held_ms, door }
            }
            FactKind::NoProgress => {
                let door = [FactDoor::Break, FactDoor::TurnEnd]
                    .into_iter()
                    .find(|d| word("door=") == Some(d.word()));
                let Some(door) = door else {
                    continue;
                };
                LeftRow::Fact { door }
            }
        };
        let left = Left {
            row,
            text: text("command").unwrap_or_default().to_string(),
            at_ms,
            submits: word("submits=").and_then(|n| n.parse::<u64>().ok()),
            ran: false,
            seeded: true,
            counted: false,
        };
        rows.push((EpisodeKey { kind, id }, left));
    }
    rows
}

/// The task door's mark: every row the occupancy loop books for a managed session's fact word
/// carries it (intent, outcome, clear and, under `observe`, the would-be note), and no reader of
/// the typed door's rows ([`seed_rows_in`], [`break_lines_in`], `rules::would_rows_in`, the
/// Harness page's counts) takes such a row for a line.
pub const TASK_DOOR: &str = "door=task";

/// Whether a row's `reason` is the task door's ([`TASK_DOOR`]).
#[must_use]
pub fn is_task_row(reason: &str) -> bool {
    reason.split(' ').any(|w| w == TASK_DOOR)
}

/// The last acknowledged task note, and whether its hold subsequently closed.
/// This is custody only: it gives no typed-line credit, pace or rule-cap booking.
#[derive(Debug, Default)]
pub(crate) struct TaskCustody {
    word: Option<super::FactWord>,
    closed: bool,
}

impl TaskCustody {
    /// Reads this session's acknowledged notes and clears beside its hold boundaries. An intent,
    /// would-be note or refusal never replaces an acknowledged predecessor.
    pub(crate) fn from_ledger(body: &str, sid: Option<&str>) -> Self {
        let want = sid.map(|sid| sid.trim_start_matches('@'));
        let hold = FactKind::Hold.rule();
        let mut custody = Self::default();
        for line in body.lines().filter(|line| line.contains(hold)) {
            let Ok(row) = aterm_json::from_str::<aterm_json::Value>(line) else {
                continue;
            };
            let text = |key: &str| row.get(key).and_then(aterm_json::Value::as_str);
            if text("sid") != want
                || text("rule_id") != Some(hold)
                || text("decision") != Some("waiting")
            {
                continue;
            }
            let Some(reason) = text("reason") else {
                continue;
            };
            if matches!(
                super::hold::row_said(reason),
                Some(super::hold::RowSaid::Closed { .. })
            ) {
                custody.close();
            } else if is_task_row(reason)
                && let Some(word) = text("command").and_then(super::FactWord::parse)
            {
                if reason == task_clear_reason(word, super::NoteOutcome::Cleared) {
                    custody = Self::default();
                } else if reason.split(' ').next_back() == Some("noted")
                    && reason
                        .split(' ')
                        .any(|part| part.strip_prefix("rule=") == Some(word.rule.name()))
                {
                    custody.word = Some(word);
                    custody.closed = false;
                }
            }
        }
        custody
    }

    /// Marks the hold closed: a stale or relaunch close can come after the ledger was read but
    /// before task seeding.
    pub(crate) fn close(&mut self) {
        self.closed = true;
    }

    /// Whether this exact standing word is known to belong to a hold that ended.
    pub(crate) fn closed(&self, word: super::FactWord) -> bool {
        self.closed && self.word == Some(word)
    }
}

/// The task door's intent row's reason (`fact-intent@v1`, `waiting`), and under `observe` its
/// would-be note's (`fact-would@v1`): the episode's words and [`TASK_DOOR`].
#[must_use]
pub fn task_intent_reason(rule: RuleId, episode: u64, held_ms: i64, declared: bool) -> String {
    format!(
        "{} {TASK_DOOR}",
        episode_words(rule, episode, held_ms, declared)
    )
}

/// The task door's outcome row's reason (`fact-hold@v1`): the intent's words, then `noted` for a
/// word the host took (a `waiting` row), `cleared` for a clear (`waiting`, the hold closed), or
/// `why=<word>` for one it did not (`skipped`: `no-task`, `closed` or `fenced`).
#[must_use]
pub fn task_outcome_reason(
    rule: RuleId,
    episode: u64,
    held_ms: i64,
    declared: bool,
    outcome: super::NoteOutcome,
) -> String {
    let words = task_intent_reason(rule, episode, held_ms, declared);
    match outcome {
        super::NoteOutcome::Noted | super::NoteOutcome::Cleared => {
            format!("{words} {}", outcome.word())
        }
        refused => format!("{words} why={}", refused.word()),
    }
}

/// The task door's clear row's reason (`fact-hold@v1`): `rule=<name> fact=<word>`, [`TASK_DOOR`],
/// then `cleared` (a `waiting` row) or `why=<word>` (`skipped`).
#[must_use]
pub fn task_clear_reason(word: super::FactWord, outcome: super::NoteOutcome) -> String {
    let words = format!("rule={} fact={} {TASK_DOOR}", word.rule.name(), word.word());
    match outcome {
        super::NoteOutcome::Cleared => format!("{words} cleared"),
        refused => format!("{words} why={}", refused.word()),
    }
}

/// Extracts the fact rows for `sid` from ledger `body`, oldest first, for
/// [`FactsState::seed`](crate::supervise::facts::FactsState::seed): an intent of the kind its
/// `kind=` word names (the hold where none), each kind's `typed` or `skipped` row, a wake's
/// `waiting` row told by its await ([`wake_heard_reason`]) and each kind's `waiting` row of a
/// line left ([`left_reason`]), but none of the task door's rows ([`is_task_row`]).
#[must_use]
pub fn seed_rows_in(body: &str, sid: Option<&str>) -> Vec<(EpisodeKey, i64, SeedOutcome)> {
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut rows = Vec::new();
    for line in body.lines() {
        let ours =
            line.contains(INTENT_RULE) || FactKind::ALL.iter().any(|k| line.contains(k.rule()));
        if !ours || want.as_deref().is_some_and(|sid| !line.contains(sid)) {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        let (Some(at_ms), Some(reason)) = (
            v.get("ts").and_then(aterm_json::Value::as_i64),
            text("reason"),
        ) else {
            continue;
        };
        if is_task_row(reason) {
            continue;
        }
        let word = |key: &str| reason.split(' ').find_map(|w| w.strip_prefix(key));
        let (kind, outcome) = match (text("rule_id"), text("decision")) {
            (Some(INTENT_RULE), Some("waiting")) => match word("kind=") {
                None => (FactKind::Hold, SeedOutcome::Intent),
                Some(kind) => match FactKind::of_word(kind) {
                    Some(kind) => (kind, SeedOutcome::Intent),
                    None => continue,
                },
            },
            (Some(rule), Some(decision)) => match (FactKind::of_rule(rule), decision) {
                (Some(kind), "typed") => (kind, SeedOutcome::Typed),
                (Some(kind), "skipped") => (kind, SeedOutcome::Skipped),
                (Some(FactKind::Wake), "waiting")
                    if reason.split(' ').any(|w| w == "told")
                        && word("door=") == Some(super::wakes_seen::WakeDoor::Await.word()) =>
                {
                    (FactKind::Wake, SeedOutcome::Answered)
                }
                (Some(kind), "waiting") if reason.split(' ').any(|w| w == "left") => {
                    (kind, SeedOutcome::Left)
                }
                _ => continue,
            },
            _ => continue,
        };
        if let Some(id) = word("episode=").and_then(|hex| u64::from_str_radix(hex, 16).ok()) {
            rows.push((EpisodeKey { kind, id }, at_ms, outcome));
        }
    }
    rows
}

/// When (unix ms, oldest first) each line an earlier loop typed at a break went, from ledger
/// `body`, with its kind: session `sid`'s `fact-no-progress@v1` and `fact-wake@v1` rows `typed`
/// at `door=break` (a rider's `door=turn-end` or `door=point` row is no line of its own) and its
/// `fact-hold@v1` rows `typed`. A successor paces the stuck line by them, as
/// [`super::rules::would_rows_in`] seeds the door's own books.
#[must_use]
pub fn break_lines_in(body: &str, sid: Option<&str>) -> Vec<(i64, FactKind)> {
    let (stuck, hold, wake) = (
        FactKind::NoProgress.rule(),
        FactKind::Hold.rule(),
        FactKind::Wake.rule(),
    );
    let want = sid.map(|s| s.trim_start_matches('@'));
    body.lines()
        .filter(|line| {
            (line.contains(stuck) || line.contains(hold) || line.contains(wake))
                && want.is_none_or(|sid| line.contains(sid))
        })
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .filter_map(|v| {
            let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
            let at_break = || {
                text("reason").is_some_and(|r| {
                    r.split(' ')
                        .any(|w| w.strip_prefix("door=") == Some(FactDoor::Break.word()))
                })
            };
            let kind = match text("rule_id") {
                Some(rule) if rule == hold => Some(FactKind::Hold),
                Some(rule) if rule == stuck => at_break().then_some(FactKind::NoProgress),
                Some(rule) if rule == wake => at_break().then_some(FactKind::Wake),
                _ => None,
            };
            if text("sid") != want || text("decision") != Some("typed") {
                return None;
            }
            Some((v.get("ts").and_then(aterm_json::Value::as_i64)?, kind?))
        })
        .collect()
}

#[cfg(test)]
#[path = "door_tests.rs"]
mod tests;
