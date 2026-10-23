// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Built-in occupancy rules (`wait-nudge@v1` and `hold-room@v1`) and their pure decider [`decide`].

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

/// THE DEVELOPMENT SEAM that shortens [`WAIT_NUDGE_AFTER_MS`] to its whole seconds (the lab's
/// `occupancy-break-claude` sets 60), read through [`aterm_types::dev_seam!`]: a shipped binary
/// never reads it and nudges at 30 min.
pub const NUDGE_SEAM: &str = "ATERM_OCCUPANCY_NUDGE_S";

/// How old a hold `wait-nudge` holds from in this process: [`WAIT_NUDGE_AFTER_MS`], or
/// [`NUDGE_SEAM`]'s seconds in a build that reads seams, read once. A session's setup takes it
/// when the session is made; [`decide`] takes the setup's.
#[must_use]
pub fn wait_nudge_after_ms() -> i64 {
    static AFTER: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *AFTER.get_or_init(|| nudge_after_ms_of(aterm_types::dev_seam!(NUDGE_SEAM).as_deref()))
}

fn nudge_after_ms_of(seam: Option<&std::ffi::OsStr>) -> i64 {
    seam.and_then(|v| v.to_str()?.trim().parse::<i64>().ok())
        .filter(|s| (1..=WAIT_NUDGE_AFTER_MS / 1000).contains(s))
        .map_or(WAIT_NUDGE_AFTER_MS, |s| s * 1000)
}

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
    /// How many rules there are: the length of every per-rule array ([`Self::index`]).
    pub const COUNT: usize = 2;

    /// Every rule, in the order a break decides them (the first due is the one due).
    pub const ALL: [RuleId; Self::COUNT] = [RuleId::WaitNudge, RuleId::HoldRoom];

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

    /// Whether slice 1's builds had it: a told word replayed from such a build's tape carries
    /// these alone ([`super::said::told_rules`]).
    #[must_use]
    pub const fn slice1(self) -> bool {
        match self {
            RuleId::WaitNudge | RuleId::HoldRoom => true,
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

    /// Whether its line needs a free build slot and types the board's figures: its pace
    /// reserves the slot, and the host re-reads the board for the write
    /// ([`super::PaceGrant::Room`]).
    #[must_use]
    pub const fn slot(self) -> bool {
        match self {
            RuleId::WaitNudge => false,
            RuleId::HoldRoom => true,
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
        self.held_for(rule.into())
    }

    /// The first guard that holds `line` ([`super::door::BreakLine`]: a rule's, or the stuck
    /// fact's), or `None`.
    #[must_use]
    pub fn held_for(&self, line: super::door::BreakLine) -> Option<Guard> {
        if let Some(brake) = self.brake {
            return Some(brake);
        }
        self.each_for(line)
            .into_iter()
            .find_map(|(guard, holds)| holds.then_some(guard))
    }

    /// Each guard past the brake, in [`Self::held`]'s order, with whether it holds a line of
    /// `rule`: what `occupancy why`'s guards line marks (design §6).
    #[must_use]
    pub fn each(&self, rule: RuleId) -> [(Guard, bool); 9] {
        self.each_for(rule.into())
    }

    /// [`Self::each`] for any line at a break.
    #[must_use]
    pub fn each_for(&self, line: super::door::BreakLine) -> [(Guard, bool); 9] {
        [
            (Guard::Person, self.person),
            (Guard::Halted, self.halted),
            (Guard::Draft, self.draft),
            (Guard::Taskless, self.taskless),
            (Guard::Exchange, self.exchange),
            (Guard::Managed, self.managed),
            (Guard::Memory, self.memory),
            (Guard::Reach, self.reach_down),
            (Guard::Autopilot, !line.modes().contains(&self.mode)),
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

    /// A line that is no rule's went at `at_ms` (the stuck fact's, §2.1's rank 4): it counts
    /// toward the session's pace alone.
    pub fn lined(&mut self, at_ms: i64) {
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

    fn nudges_today(&self, now_ms: i64) -> usize {
        self.nudges
            .iter()
            .filter(|at| now_ms.saturating_sub(**at) < DAY_MS)
            .count()
    }
}

/// A rule's verdict at a break.
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
        room_holds(hold, books, now_ms) && !spent(RuleId::HoldRoom, books, now_ms)
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

fn holds_but_machine(
    rule: RuleId,
    hold: HoldSeen,
    books: &Books,
    nudge_after_ms: i64,
    now_ms: i64,
) -> bool {
    match rule {
        RuleId::WaitNudge => {
            now_ms.saturating_sub(hold.since_ms) >= nudge_after_ms
                && !hold.declared
                && !books.nudged
        }
        RuleId::HoldRoom => room_holds(hold, books, now_ms),
    }
}

fn room_holds(hold: HoldSeen, books: &Books, now_ms: i64) -> bool {
    now_ms >= room_due_ms(hold.since_ms, books.rooms.last().copied())
}

/// WHEN `hold-room`'S CADENCE NEXT LETS ITS LINE GO, on the clock `since_ms` is on: on a hold
/// begun at `since_ms` whose last line went at `last_ms` (none yet: `None`), the hold
/// [`HOLD_ROOM_AFTER_MS`] old and that line [`HOLD_ROOM_APART_MS`] back: the term [`decide`]
/// judges the rule's cadence by, which a host that samples its board for the rule's slot term
/// from that term's `for=` before then reads too.
#[must_use]
pub fn room_due_ms(since_ms: i64, last_ms: Option<i64>) -> i64 {
    let after = since_ms.saturating_add(HOLD_ROOM_AFTER_MS);
    last_ms.map_or(after, |last| {
        after.max(last.saturating_add(HOLD_ROOM_APART_MS))
    })
}

/// Decide each rule's verdict at a break at `now_ms`.
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
    nudge_after_ms: i64,
    now_ms: i64,
) -> [(RuleId, Verdict); RuleId::COUNT] {
    let mut due_seen = false;
    RuleId::ALL.map(|rule| {
        let verdict = match hold
            .filter(|hold| holds_but_machine(rule, *hold, books, nudge_after_ms, now_ms))
        {
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
/// ([`holds_but_machine`]): [`decide`] on the board's look, and the door's write again on the
/// board its host re-read for it, whose figures the line then types.
#[must_use]
pub fn line(rule: RuleId, r: &OccupancyReading, hold: HoldSeen, now_ms: i64) -> Option<FactLine> {
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

fn spent(rule: RuleId, books: &Books, now_ms: i64) -> bool {
    match rule {
        RuleId::WaitNudge => books.nudges_today(now_ms) >= NUDGES_PER_DAY,
        RuleId::HoldRoom => books.rooms.len() >= HOLD_ROOM_PER_HOLD,
    }
}

/// When the session's pace next lets a line go, if after `now_ms`; every door's lines share it ([`super::door::stuck_at_break`]).
#[must_use]
pub fn pace_next(books: &Books, now_ms: i64) -> Option<i64> {
    paced(books, now_ms, false)
}

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

/// The ledger rule a held verdict is booked under (design §6): `waiting`, on a transition only,
/// once an episode a guard ([`held_reason`]).
pub const HELD_RULE: &str = "fact-held@v1";

/// The reason a `fact-held@v1` row carries: the episode's words ([`would_reason`]), its
/// ordinal in the hold (`ordinal=<n>`, [`Books::this_hold`]) and `guard=<word>`, the guard that
/// held it.
#[must_use]
pub fn held_reason(
    rule: RuleId,
    episode: u64,
    ordinal: usize,
    held_ms: i64,
    declared: bool,
    guard: Guard,
) -> String {
    format!(
        "{} ordinal={ordinal} guard={}",
        would_reason(rule, episode, held_ms, declared),
        guard.word()
    )
}

/// A `fact-held@v1` row read back ([`held_rows_in`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldRow {
    /// The rule held.
    pub rule: RuleId,
    /// The episode's ordinal in its hold: which of the rule's lines in the hold was held.
    pub ordinal: usize,
    /// The guard that held it.
    pub guard: Guard,
    /// The row's stamp, unix ms.
    pub row_ms: i64,
}

/// THE HELD VERDICTS A LOOP BEFORE THIS ONE BOOKED for session `sid` in the ledger text `body`,
/// oldest first: each `fact-held@v1` row's rule, ordinal, guard and stamp, so a successor that
/// carries the hold books none of its rows again. A row this build cannot read is passed over.
#[must_use]
pub fn held_rows_in(body: &str, sid: Option<&str>) -> Vec<HeldRow> {
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut rows = Vec::new();
    for line in body.lines() {
        if !line.contains(HELD_RULE) || want.as_deref().is_some_and(|sid| !line.contains(sid)) {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want || text("rule_id") != Some(HELD_RULE) {
            continue;
        }
        let (Some(row_ms), Some(reason)) = (
            v.get("ts").and_then(aterm_json::Value::as_i64),
            text("reason"),
        ) else {
            continue;
        };
        let mut rule = None;
        let mut ordinal = None;
        let mut guard = None;
        for word in reason.split(' ') {
            if let Some(name) = word.strip_prefix("rule=") {
                rule = RuleId::of_name(name);
            } else if let Some(n) = word.strip_prefix("ordinal=") {
                ordinal = n.parse::<usize>().ok();
            } else if let Some(word) = word.strip_prefix("guard=") {
                guard = Guard::from_word(word);
            }
        }
        if let (Some(rule), Some(ordinal), Some(guard)) = (rule, ordinal, guard) {
            rows.push(HeldRow {
                rule,
                ordinal,
                guard,
                row_ms,
            });
        }
    }
    rows
}

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
