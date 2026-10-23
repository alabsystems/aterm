// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BUILT-IN RULES AND THEIR DECIDER (`docs/DESIGN-harness-occupancy-2026-10-20.md` §3.2,
//! §5; the red-team's F1, F9 and F15): whether `wait-nudge@v1` or `hold-room@v1` is due at a
//! break, and if so what holds it. Pure and clockless: every instant is the caller's milliseconds
//! (the loop's hold clock), and every input is a value the caller already holds — the hold, the
//! board's [`OccupancyReading`], the [`Books`] of what was told, and the [`Guards`] it judged
//! from its own state. [`decide`] reads nothing and types nothing.
//!
//! Each rule's verdict, checked in this order:
//!
//! 1. [`Verdict::Quiet`]: its condition does not hold, or its own cadence has it wait (once a
//!    hold for the nudge; [`HOLD_ROOM_APART_MS`] between `hold-room`'s).
//! 2. [`Verdict::Spent`]: its cap is spent ([`NUDGES_PER_DAY`] nudges a rolling day;
//!    [`HOLD_ROOM_PER_HOLD`] a hold), judged before the machine's terms, which a spent rule
//!    never needs read. Past it §3.2 raises an Entry.
//! 3. [`Verdict::Quiet`] again where the machine's terms do not hold on the board.
//! 4. [`Verdict::Held`]: a guard holds it — a brake of the break's, a person's hand or grace,
//!    the halt, a draft in the composer, a session with no task yet, the person's own exchange,
//!    the memory wall, the API's route down, then the session's autopilot (both rules are
//!    Cruise's).
//! 5. [`Verdict::Paced`]: the session's own pace, one line per [`SESSION_PACE_MS`].
//! 6. [`Verdict::Due`], with the line it would type. At most one rule is due at a break: a
//!    second is paced behind the first.

use super::reading::OccupancyReading;
use super::templates::{self, FactLine};
use crate::harness::disk::Level;
use crate::supervise::config::Autopilot;
use crate::supervise::run::Guard;

/// `hold-room`'s slot term, as the board latches it over its looks (`AwaitSys::parse` reads it
/// whole, its leading `sys` included).
pub const HOLD_ROOM_SLOTS: &str = "sys slots>=1 for=20m";

/// `hold-room`'s machine terms, judged at each look. `disk=ok` is the volume's and judged apart
/// (the grammar refuses a volume term beside a machine term: `MIXED`).
pub const HOLD_ROOM_MACHINE: &str = "sys avail>=8GiB and mem<critical";

/// `wait-nudge` holds from a hold this old.
pub const WAIT_NUDGE_AFTER_MS: i64 = 30 * 60_000;

/// `hold-room` holds from a hold this old.
pub const HOLD_ROOM_AFTER_MS: i64 = 2 * 60 * 60_000;

/// `hold-room`'s lines stand at least this far apart.
pub const HOLD_ROOM_APART_MS: i64 = 2 * 60 * 60_000;

/// The most `hold-room` lines a hold.
pub const HOLD_ROOM_PER_HOLD: usize = 4;

/// The most `wait-nudge` lines a session in a rolling [`DAY_MS`].
pub const NUDGES_PER_DAY: usize = 3;

/// The rolling day [`NUDGES_PER_DAY`] counts in.
pub const DAY_MS: i64 = 24 * 60 * 60_000;

/// One line per session per this long, whichever rule.
pub const SESSION_PACE_MS: i64 = 20 * 60_000;

/// A built-in rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuleId {
    /// `hold>=30m and hold=undeclared`: once a hold.
    WaitNudge,
    /// `hold>=2h and slots>=1 for=20m and avail>=8GiB and disk=ok and mem<critical`, in a
    /// session whose jobs held a build slot during this hold.
    HoldRoom,
}

impl RuleId {
    /// Every rule, in the order a break decides them (the first due is the one due).
    pub const ALL: [RuleId; 2] = [RuleId::WaitNudge, RuleId::HoldRoom];

    /// Its name and revision, as rows and journal lines spell it. Stable: a successor loop books
    /// by it ([`would_rows_in`]).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            RuleId::WaitNudge => "wait-nudge@v1",
            RuleId::HoldRoom => "hold-room@v1",
        }
    }

    /// The rule a name spells, or `None`.
    #[must_use]
    pub fn of_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|rule| rule.name() == name)
    }

    /// The kind of fact its line tells: both built-ins tell the hold's.
    #[must_use]
    pub const fn kind(self) -> crate::supervise::facts::FactKind {
        match self {
            RuleId::WaitNudge | RuleId::HoldRoom => crate::supervise::facts::FactKind::Hold,
        }
    }

    /// Its slot in a per-rule array.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            RuleId::WaitNudge => 0,
            RuleId::HoldRoom => 1,
        }
    }
}

/// The open hold as the decider reads it (`occupancy::hold`'s clock).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldSeen {
    /// When it began.
    pub since_ms: i64,
    /// The agent declared it (`WAITING:`).
    pub declared: bool,
}

/// What the caller judged from the state it holds (the red-team's F1: no read is made for
/// them). Each is a reason no line goes now, in the order [`Self::held`] asks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guards {
    /// A brake of the break's own (a held stall, a limit episode, an act untaken, the wake's
    /// settle, a credits hold), the first that holds.
    pub brake: Option<Guard>,
    /// A person typed within their grace, or another driver's hand is on the session.
    pub person: bool,
    /// The session is halted.
    pub halted: bool,
    /// A draft stands in the composer: a line typed now would land in it.
    pub draft: bool,
    /// Nobody has asked the session anything yet: a person's own session, whatever runs under
    /// it.
    pub taskless: bool,
    /// The task is done and the person's own exchange holds it.
    pub exchange: bool,
    /// A manager waits on the session: the fact is the manager's to carry, by a note no slice
    /// writes yet.
    pub managed: bool,
    /// The memory wall acts.
    pub memory: bool,
    /// The agent's route to its API is down.
    pub reach_down: bool,
    /// The session's autopilot.
    pub mode: Autopilot,
}

impl Guards {
    /// Nothing holds, under `mode`.
    #[must_use]
    pub const fn clear(mode: Autopilot) -> Self {
        Self {
            brake: None,
            person: false,
            halted: false,
            draft: false,
            taskless: false,
            exchange: false,
            managed: false,
            memory: false,
            reach_down: false,
            mode,
        }
    }

    /// The first guard that holds a line of `rule`, or `None`.
    #[must_use]
    pub fn held(&self, rule: RuleId) -> Option<Guard> {
        if let Some(brake) = self.brake {
            return Some(brake);
        }
        self.each(rule)
            .into_iter()
            .find_map(|(guard, holds)| holds.then_some(guard))
    }

    /// Each guard past the brake, in [`Self::held`]'s order, with whether it holds a line of
    /// `rule`: what `occupancy why`'s guards line marks (design §6).
    #[must_use]
    pub fn each(&self, rule: RuleId) -> [(Guard, bool); 9] {
        [
            (Guard::Person, self.person),
            (Guard::Halted, self.halted),
            (Guard::Draft, self.draft),
            (Guard::Taskless, self.taskless),
            (Guard::Exchange, self.exchange),
            (Guard::Managed, self.managed),
            (Guard::Memory, self.memory),
            (Guard::Reach, self.reach_down),
            (Guard::Autopilot, !modes(rule).contains(&self.mode)),
        ]
    }
}

/// The autopilots a rule's lines are for (§3.2: both built-ins are Cruise's).
#[must_use]
pub const fn modes(rule: RuleId) -> &'static [Autopilot] {
    match rule {
        RuleId::WaitNudge | RuleId::HoldRoom => &[Autopilot::Cruise],
    }
}

/// WHAT THE SESSION WAS TOLD — or, under `observe`, would have been (`fact-would@v1`): the caps'
/// and the pace's book, on the caller's milliseconds. A successor loop seeds it from its rows
/// ([`would_rows_in`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Books {
    /// The hold the per-hold counts are of, by its start.
    hold_since_ms: Option<i64>,
    /// `wait-nudge` went in this hold.
    nudged: bool,
    /// Each `hold-room` line of this hold.
    rooms: Vec<i64>,
    /// Each `wait-nudge` line within the last [`DAY_MS`], oldest first.
    nudges: Vec<i64>,
    /// The last line of either rule.
    last_ms: Option<i64>,
}

impl Books {
    /// The hold begun at `since_ms` is the one the per-hold counts are of: another hold's are
    /// forgotten.
    pub fn hold(&mut self, since_ms: i64) {
        if self.hold_since_ms != Some(since_ms) {
            self.hold_since_ms = Some(since_ms);
            if super::fault::armed(super::fault::Fault::DedupeByRule) {
                return;
            }
            self.nudged = false;
            self.rooms.clear();
        }
    }

    /// The hold the per-hold counts are of.
    #[must_use]
    pub const fn hold_since_ms(&self) -> Option<i64> {
        self.hold_since_ms
    }

    /// `rule` went (or would have) at `at_ms`, in the hold the books are of. `this_hold` false
    /// for a line a seed counts only toward the day and the pace (one of an earlier hold's).
    pub fn fired(&mut self, rule: RuleId, at_ms: i64, this_hold: bool) {
        match rule {
            RuleId::WaitNudge => {
                self.nudges.retain(|at| at_ms.saturating_sub(*at) < DAY_MS);
                self.nudges.push(at_ms);
                self.nudges.sort_unstable();
                self.nudged |= this_hold;
            }
            RuleId::HoldRoom => {
                if this_hold {
                    self.rooms.push(at_ms);
                    self.rooms.sort_unstable();
                }
            }
        }
        self.last_ms = Some(self.last_ms.map_or(at_ms, |last| last.max(at_ms)));
    }

    /// How many lines of `rule` this hold has had: the next one's ordinal.
    #[must_use]
    pub fn this_hold(&self, rule: RuleId) -> usize {
        match rule {
            RuleId::WaitNudge => usize::from(self.nudged),
            RuleId::HoldRoom => self.rooms.len(),
        }
    }

    /// The nudges within the day before `now_ms`.
    fn nudges_today(&self, now_ms: i64) -> usize {
        self.nudges
            .iter()
            .filter(|at| now_ms.saturating_sub(**at) < DAY_MS)
            .count()
    }
}

/// A rule's verdict at a break ([module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Not due.
    Quiet,
    /// Due, and its cap is spent.
    Spent,
    /// Due, and this guard holds it.
    Held(Guard),
    /// Due, and the session's pace holds it until `next_ms`.
    Paced {
        /// When the pace lets a line go.
        next_ms: i64,
    },
    /// Due: this line would go.
    Due(FactLine),
}

/// Whether a break at `now_ms` needs the board's reading: `hold-room` could be due by everything
/// but the machine (the hold old enough, its spacing and its cap not in the way). Only then is
/// the host asked (the red-team's F9: the reading is not asked at every break read).
#[must_use]
pub fn wants_reading(hold: Option<HoldSeen>, books: &Books, now_ms: i64) -> bool {
    hold.is_some_and(|hold| {
        holds_but_machine(RuleId::HoldRoom, hold, books, now_ms)
            && !spent(RuleId::HoldRoom, books, now_ms)
    })
}

/// THE READY PROXY (§6, idle-while-ready): `hold-room`'s terms but its cadence, its cap and
/// the guards hold at `now_ms` on `r` — the hold past [`HOLD_ROOM_AFTER_MS`], the board fresh,
/// its slot term, its machine terms and `disk=ok` met, in a session whose jobs held a build
/// slot during the hold. A session the host counts as ready while build slots stand idle.
#[must_use]
pub fn ready(r: &OccupancyReading, hold: Option<HoldSeen>, now_ms: i64) -> bool {
    hold.is_some_and(|hold| {
        now_ms.saturating_sub(hold.since_ms) >= HOLD_ROOM_AFTER_MS
            && line(RuleId::HoldRoom, r, hold, now_ms).is_some()
    })
}

/// `rule`'s terms but the machine's, with its own cadence: for `wait-nudge` the hold
/// [`WAIT_NUDGE_AFTER_MS`] old, undeclared and not nudged yet; for `hold-room` the hold
/// [`HOLD_ROOM_AFTER_MS`] old and its last line [`HOLD_ROOM_APART_MS`] back.
fn holds_but_machine(rule: RuleId, hold: HoldSeen, books: &Books, now_ms: i64) -> bool {
    let held_ms = now_ms.saturating_sub(hold.since_ms);
    match rule {
        RuleId::WaitNudge => held_ms >= WAIT_NUDGE_AFTER_MS && !hold.declared && !books.nudged,
        RuleId::HoldRoom => {
            held_ms >= HOLD_ROOM_AFTER_MS
                && books
                    .rooms
                    .last()
                    .is_none_or(|last| now_ms.saturating_sub(*last) >= HOLD_ROOM_APART_MS)
        }
    }
}

/// THE DECISION AT A BREAK at `now_ms` ([module docs](self)): each rule's verdict, in
/// [`RuleId::ALL`]'s order. `books` must be of `hold` ([`Books::hold`]). A cap is judged before
/// the machine: a spent `hold-room` needs no reading to be said spent. `OccupancyDoor`'s
/// `Intend` is enabled where this says [`Verdict::Due`] (the anchor binds that verdict; the
/// intent row itself is the typing path's), and its `Lapse` is a due rule whose condition no
/// longer holds ([`Verdict::Quiet`]: dropped, never told late).
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "OccupancyDoor",
        action = "Intend",
        project = "conformance_occupancy::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "OccupancyDoor",
        action = "Lapse",
        project = "conformance_occupancy::project"
    )
)]
pub fn decide(
    r: &OccupancyReading,
    hold: Option<HoldSeen>,
    books: &Books,
    g: &Guards,
    now_ms: i64,
) -> [(RuleId, Verdict); 2] {
    let mut due_seen = false;
    RuleId::ALL.map(|rule| {
        let verdict = match hold.filter(|hold| holds_but_machine(rule, *hold, books, now_ms)) {
            None => Verdict::Quiet,
            Some(_) if spent(rule, books, now_ms) => Verdict::Spent,
            Some(hold) => match line(rule, r, hold, now_ms) {
                None => Verdict::Quiet,
                Some(line) => match g.held(rule) {
                    Some(guard) => Verdict::Held(guard),
                    None => match paced(books, now_ms, due_seen) {
                        Some(next_ms) => Verdict::Paced { next_ms },
                        None => Verdict::Due(line),
                    },
                },
            },
        };
        due_seen |= matches!(verdict, Verdict::Due(_));
        (rule, verdict)
    })
}

/// `rule`'s line on `hold` at `now_ms`, where the machine lets it go (`hold-room`'s terms on the
/// board, [`HOLD_ROOM_SLOTS`], [`HOLD_ROOM_MACHINE`] and `disk=ok`, in a session whose jobs held
/// a build slot during the hold), else `None`. The caller judged the rest
/// ([`holds_but_machine`]).
fn line(rule: RuleId, r: &OccupancyReading, hold: HoldSeen, now_ms: i64) -> Option<FactLine> {
    let held_ms = now_ms.saturating_sub(hold.since_ms);
    match rule {
        RuleId::WaitNudge => Some(templates::wait_nudge(held_ms)),
        RuleId::HoldRoom => {
            if !r.fresh()
                || r.room != Some(true)
                || r.disk != Some(Level::Ok)
                || !r.held_slot_within(held_ms)
            {
                return None;
            }
            let (free_ms, slots, avail_mib) = (r.slots_free_ms?, r.slots?, r.avail_mib?);
            (slots.usable >= 1)
                .then(|| templates::hold_room(held_ms, slots.usable, slots.n, free_ms, avail_mib))
        }
    }
}

/// Whether `rule`'s cap is spent at `now_ms`.
fn spent(rule: RuleId, books: &Books, now_ms: i64) -> bool {
    match rule {
        RuleId::WaitNudge => books.nudges_today(now_ms) >= NUDGES_PER_DAY,
        RuleId::HoldRoom => books.rooms.len() >= HOLD_ROOM_PER_HOLD,
    }
}

/// When the session's pace lets the next line go, while it is ahead of `now_ms`; a rule due
/// behind another at the same break (`due_seen`) waits a whole pace.
fn paced(books: &Books, now_ms: i64, due_seen: bool) -> Option<i64> {
    if due_seen {
        return Some(now_ms.saturating_add(SESSION_PACE_MS));
    }
    books
        .last_ms
        .map(|last| last.saturating_add(SESSION_PACE_MS))
        .filter(|next| *next > now_ms)
}

/// The reason a `fact-would@v1` row carries: `rule=<name> episode=<hex> held_ms=<n>`, with
/// ` declared` for a declared hold.
#[must_use]
pub fn would_reason(rule: RuleId, episode: u64, held_ms: i64, declared: bool) -> String {
    let declared = if declared { " declared" } else { "" };
    format!(
        "rule={} episode={episode:016x} held_ms={held_ms}{declared}",
        rule.name()
    )
}

/// The ledger rule a would-be line is booked under.
pub const WOULD_RULE: &str = "fact-would@v1";

/// A `fact-would@v1` row read back ([`would_rows_in`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WouldRow {
    /// The rule that would have gone.
    pub rule: RuleId,
    /// The row's stamp, unix ms.
    pub row_ms: i64,
    /// How long the hold had held at it.
    pub held_ms: i64,
}

/// THE LINES A LOOP BEFORE THIS ONE BOOKED for session `sid` in the ledger text `body`, oldest
/// first, which [`Books`] seeds from: each `fact-would@v1` row (what `observe` would have typed),
/// and each `fact-hold@v1` row `typed` that names its rule (what `prompt` typed: the door's
/// outcome rows). A row this build cannot read is passed over.
#[must_use]
pub fn would_rows_in(body: &str, sid: Option<&str>) -> Vec<WouldRow> {
    let hold = crate::supervise::facts::FactKind::Hold.rule();
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut rows = Vec::new();
    for line in body.lines() {
        if !(line.contains(WOULD_RULE) || line.contains(hold))
            || want.as_deref().is_some_and(|sid| !line.contains(sid))
        {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        let booked = match text("rule_id") {
            Some(WOULD_RULE) => true,
            Some(rule) => rule == hold && text("decision") == Some("typed"),
            None => false,
        };
        if text("sid").map(str::to_string) != want || !booked {
            continue;
        }
        let (Some(row_ms), Some(reason)) = (
            v.get("ts").and_then(aterm_json::Value::as_i64),
            text("reason"),
        ) else {
            continue;
        };
        let mut rule = None;
        let mut held_ms = None;
        for word in reason.split(' ') {
            if let Some(name) = word.strip_prefix("rule=") {
                rule = RuleId::of_name(name);
            } else if let Some(n) = word.strip_prefix("held_ms=") {
                held_ms = n.parse::<i64>().ok().filter(|n| *n >= 0);
            }
        }
        if let (Some(rule), Some(held_ms)) = (rule, held_ms) {
            rows.push(WouldRow {
                rule,
                row_ms,
                held_ms,
            });
        }
    }
    rows
}

/// An episode's id: the rule, its revision (`builtin` for these), the hold it is of (by its
/// start in unix ms) and its ordinal in that hold, hashed. Every successor that carries a hold
/// keys it by the start its predecessor's row puts it at (the row's stamp less how long the
/// hold had stood), so they name the same episodes; the loop that opened the hold keyed it by
/// its own wall read at its start and its monotonic clock since, which agree with that row to
/// the millisecond only as far as the two clocks do.
#[must_use]
pub fn episode(rule: RuleId, hold_key: i64, ordinal: usize) -> u64 {
    aterm_hash::fnv1a_64(format!("{} builtin {hold_key} {ordinal}", rule.name()).as_bytes())
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
