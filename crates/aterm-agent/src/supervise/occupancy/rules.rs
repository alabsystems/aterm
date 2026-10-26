// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Built-in occupancy rules (`wait-nudge@v1`, `hold-room@v1` and `wake@v1`) and their pure
//! decider [`decide`], which a break's pick ranks ([`rank`]).

use super::reading::OccupancyReading;
use super::templates::{self, FactLine};
use super::wakes::UNHEARD_MS;
use super::wakes_seen::{WAKE_RULE, WakeSeen, WakeState, WakesSeen, head_key};
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
    /// A wake the agent declared in its session's book whose condition held and whose need
    /// holds (design §4.1): once a wake ([`wake_opens`]).
    Wake,
}

impl RuleId {
    /// How many rules there are: the length of every per-rule array ([`Self::index`]).
    pub const COUNT: usize = 3;

    /// Every rule, in [`Self::index`]'s order: the order a break decides them in. A recording
    /// without slice 2's doors picks the first due ([`WakeSight::ranked`]); else [`rank`] picks.
    pub const ALL: [RuleId; Self::COUNT] = [RuleId::WaitNudge, RuleId::HoldRoom, RuleId::Wake];

    /// Its name and revision, as rows and journal lines spell it. Stable: a successor loop books
    /// by it ([`would_rows_in`]).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            RuleId::WaitNudge => "wait-nudge@v1",
            RuleId::HoldRoom => "hold-room@v1",
            RuleId::Wake => WAKE_RULE,
        }
    }

    /// The rule a name spells, or `None`.
    #[must_use]
    pub fn of_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|rule| rule.name() == name)
    }

    /// The kind of fact its line tells: the hold rules the hold's, a wake's its own.
    #[must_use]
    pub const fn kind(self) -> crate::supervise::facts::FactKind {
        match self {
            RuleId::WaitNudge | RuleId::HoldRoom => crate::supervise::facts::FactKind::Hold,
            RuleId::Wake => crate::supervise::facts::FactKind::Wake,
        }
    }

    /// Whether slice 1's builds had it: a told word replayed from such a build's tape carries
    /// these alone ([`super::said::told_rules`]).
    #[must_use]
    pub const fn slice1(self) -> bool {
        match self {
            RuleId::WaitNudge | RuleId::HoldRoom => true,
            RuleId::Wake => false,
        }
    }

    /// Its slot in a per-rule array.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            RuleId::WaitNudge => 0,
            RuleId::HoldRoom => 1,
            RuleId::Wake => 2,
        }
    }

    /// Whether its line needs a free build slot and types the board's figures: its pace
    /// reserves the slot, and the host re-reads the board for the write
    /// ([`super::PaceGrant::Room`]). A wake's line reserves one by its own need, through its
    /// claim (the slice-2 plan's 2.8), never through this pace.
    #[must_use]
    pub const fn slot(self) -> bool {
        match self {
            RuleId::WaitNudge | RuleId::Wake => false,
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
    /// A manager waits on the session: the fact is the manager's to carry, by the fact word on
    /// the session's task record under `prompt` (the occupancy loop's task door), never a line.
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

    /// The first guard but `except` that holds a line of `rule` (a brake first, as
    /// [`Self::held`] asks), or `None`: whether `except` alone holds it, as the task door asks
    /// of [`Guard::Managed`] (a line typed but for the manager).
    #[must_use]
    pub fn held_except(&self, except: Guard, rule: RuleId) -> Option<Guard> {
        if let Some(brake) = self.brake.filter(|brake| *brake != except) {
            return Some(brake);
        }
        self.each(rule)
            .into_iter()
            .find_map(|(guard, holds)| (holds && guard != except).then_some(guard))
    }

    /// The first of the guards that hold a fact a host types at an idle point of its own
    /// (`HostTurn::Fact`, the ask to free disk space: `IdlePoint::held`), in [`Self::each_for`]'s
    /// order: a person within their grace or another driver's hand ([`Guard::Person`]), the
    /// person's own exchange ([`Guard::Exchange`]), a manager's wait ([`Guard::Managed`]). The
    /// host's fenced write judges the halt and a draft itself, and its own seat rule the task and
    /// the autopilot, so those are not asked here.
    #[must_use]
    pub fn held_host_fact(&self) -> Option<Guard> {
        [
            (Guard::Person, self.person),
            (Guard::Exchange, self.exchange),
            (Guard::Managed, self.managed),
        ]
        .into_iter()
        .find_map(|(guard, holds)| holds.then_some(guard))
    }

    /// Of [`Self::held_host_fact`]'s guards, the first of those that hold the session for someone
    /// else for as long as they keep it, not for a grace: the person's own exchange
    /// ([`Guard::Exchange`], until they hand it back) and a manager's wait ([`Guard::Managed`], for
    /// the loop's life while its task record stands). A host's own ask of such a session
    /// (`IdlePoint::theirs`) is given up, not kept owed for a later point.
    #[must_use]
    pub fn theirs(&self) -> Option<Guard> {
        [
            (Guard::Exchange, self.exchange),
            (Guard::Managed, self.managed),
        ]
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

/// The autopilots a rule's lines are for (§3.2: the hold rules are Cruise's; a wake the agent
/// declared is told under Land too).
#[must_use]
pub const fn modes(rule: RuleId) -> &'static [Autopilot] {
    match rule {
        RuleId::WaitNudge | RuleId::HoldRoom => &[Autopilot::Cruise],
        RuleId::Wake => &[Autopilot::Cruise, Autopilot::Land],
    }
}

/// The most wake episodes [`Books`] keeps booked: the book holds at most
/// [`super::wakes::MAX_WAKES`] live, and a loop prunes the rest to the live ones
/// ([`Books::keep_wakes`]).
pub const WAKES_BOOKED: usize = 64;

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
    /// Each wake episode whose line went (or would have), oldest first, at most
    /// [`WAKES_BOOKED`].
    wakes: Vec<u64>,
    /// The last line of any rule.
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
            // A wake's line is booked by its episode ([`Self::woke`]).
            RuleId::Wake => {}
        }
        self.last_ms = Some(self.last_ms.map_or(at_ms, |last| last.max(at_ms)));
    }

    /// Wake episode `episode`'s line went (or would have) at `at_ms`: told once, and counted
    /// toward the session's pace.
    pub fn woke(&mut self, episode: u64, at_ms: i64) {
        if !self.wakes.contains(&episode) {
            self.wakes.push(episode);
            let over = self.wakes.len().saturating_sub(WAKES_BOOKED);
            self.wakes.drain(..over);
        }
        self.last_ms = Some(self.last_ms.map_or(at_ms, |last| last.max(at_ms)));
    }

    /// Whether wake episode `episode`'s line went (or would have).
    #[must_use]
    pub fn has_woke(&self, episode: u64) -> bool {
        self.wakes.contains(&episode)
    }

    /// The booked wake episodes the book no longer holds live (`live`, as the loop last read it)
    /// are forgotten: a wake told, taken or expired leaves the book, and so its episode.
    pub fn keep_wakes(&mut self, live: &WakesSeen) {
        self.wakes
            .retain(|episode| live.wakes.iter().any(|w| w.episode() == *episode));
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
            // A wake's episode is its own, of no hold.
            RuleId::Wake => 0,
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
    /// Due, and a line [`rank`] puts before it goes at this break: it stays due.
    Outranked,
    /// Due: this line would go.
    Due(FactLine),
}

/// WHAT A BREAK READS OF THE SESSION'S BOOK OF WAKES ([`decide`]'s `wake@v1`), and how it picks
/// among its due lines.
#[derive(Clone, Copy)]
pub struct WakeSight<'a> {
    /// The book as the loop read it at this break: the empty one on a recording whose host had
    /// no `host.wakes`, which never read it.
    pub seen: &'a WakesSeen,
    /// The break's wall clock, unix ms: the book's clocks are the host's.
    pub wall_ms: i64,
    /// Whether a wake episode was told already: typed, or answered by the agent's own
    /// `await wake` and heard (the loop's `FactsState`). What would have gone, [`Books`] keeps.
    pub told: &'a dyn Fn(u64) -> bool,
    /// The break ranks its due lines ([`rank`], the slice-2 plan's D6): one goes, and every other
    /// is [`Verdict::Outranked`]. Else slice 1's pick: the first due in [`RuleId::ALL`]'s order
    /// goes, and the session's pace holds the rest.
    pub ranked: bool,
}

fn never_told(_: u64) -> bool {
    false
}

static NO_WAKES: WakesSeen = WakesSeen { wakes: Vec::new() };

impl WakeSight<'static> {
    /// Slice 1's break: no book, slice 1's pick. A recording without slice 2's doors replays
    /// under it, and so does the ring, which holds no wakes.
    #[must_use]
    pub fn slice1() -> Self {
        Self {
            seen: &NO_WAKES,
            wall_ms: 0,
            told: &never_told,
            ranked: false,
        }
    }
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
        RuleId::Wake => false,
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

/// Whether a wake of the book stands ready with its need met at the book's last look: the
/// session is ready while build slots stand idle (F14), as [`ready`] says of a hold.
#[must_use]
pub fn wake_met(seen: &WakesSeen) -> bool {
    seen.wakes
        .iter()
        .any(|w| w.state == WakeState::Ready && w.need_met)
}

/// THE WAKE A BREAK WOULD TELL (`OccupancyDoor`'s `OpenWake`): of the book's wakes still to be
/// told by a line (live, not time-only, and untold: by `told`, and not booked in `books`), the
/// head ([`head_key`]: a critical one first, then the lowest ordinal), where its condition held —
/// ready, or answered by the agent's own `await wake` and unheard [`UNHEARD_MS`] at the break's
/// wall clock (the plan's S2: only a hosted session's book holds an answer unheard) — and its need
/// held at the book's last look; else `None`. An `unless=` atom that held expired the wake, which
/// left the book. A head not ready, answered but not yet unheard, or with its need unmet opens
/// nothing: the wakes are told in the head's order.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "OccupancyDoor",
        action = "OpenWake",
        project = "conformance_occupancy::project"
    )
)]
pub fn wake_opens<'a>(sight: &WakeSight<'a>, books: &Books) -> Option<&'a WakeSeen> {
    let head = sight
        .seen
        .wakes
        .iter()
        .filter(|w| w.state.live() && !w.await_only)
        .filter(|w| !matches!(w.state, WakeState::Pending))
        .filter(|w| {
            let episode = w.episode();
            !books.has_woke(episode) && !(sight.told)(episode)
        })
        .min_by_key(|w| head_key(w.prio, w.ord))?;
    let held = match head.state {
        WakeState::Ready => true,
        WakeState::Answered { at_ms } => sight.wall_ms.saturating_sub(at_ms) >= UNHEARD_MS,
        _ => false,
    };
    (held && head.need_met).then_some(head)
}

/// A RULE'S RANK AT A BREAK (the slice-2 plan's D6, design §2.1): a wake's line first (the
/// book's head, [`wake_opens`]: a critical wake before a ready one), then `wait-nudge`, then
/// `hold-room`. The lower goes first.
#[must_use]
pub const fn rank_of(rule: RuleId) -> u8 {
    match rule {
        RuleId::Wake => 0,
        RuleId::WaitNudge => 1,
        RuleId::HoldRoom => 2,
    }
}

/// THE BREAK'S PICK (D6): of the due lines in `verdicts`, the one [`rank_of`] puts first keeps its
/// verdict, and every other due line is [`Verdict::Outranked`]: it stays due, unbooked.
pub fn rank(verdicts: &mut [(RuleId, Verdict)]) {
    let first = verdicts
        .iter()
        .filter(|(_, verdict)| matches!(verdict, Verdict::Due(_)))
        .map(|(rule, _)| *rule)
        .min_by_key(|rule| rank_of(*rule));
    for (rule, verdict) in verdicts.iter_mut() {
        if matches!(verdict, Verdict::Due(_)) && Some(*rule) != first {
            *verdict = Verdict::Outranked;
        }
    }
}

/// Decide each rule's verdict at a break at `now_ms`, the book of wakes as `wakes` reads it.
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
    wakes: &WakeSight<'_>,
) -> [(RuleId, Verdict); RuleId::COUNT] {
    let mut due_seen = false;
    let mut verdicts = RuleId::ALL.map(|rule| {
        let opened = match rule {
            RuleId::Wake => wake_opens(wakes, books).map(wake_line),
            _ => match hold
                .filter(|hold| holds_but_machine(rule, *hold, books, nudge_after_ms, now_ms))
            {
                None => None,
                Some(_) if spent(rule, books, now_ms) => {
                    return (rule, Verdict::Spent);
                }
                Some(hold) => line(rule, r, hold, now_ms),
            },
        };
        let verdict = match opened {
            None => Verdict::Quiet,
            Some(line) => match g.held(rule) {
                Some(guard) => Verdict::Held(guard),
                None => match paced(books, now_ms, due_seen && !wakes.ranked) {
                    Some(next_ms) => Verdict::Paced { next_ms },
                    None => Verdict::Due(line),
                },
            },
        };
        due_seen |= matches!(verdict, Verdict::Due(_));
        (rule, verdict)
    });
    if wakes.ranked {
        rank(&mut verdicts);
    }
    verdicts
}

/// A wake's line as the decider books it: its ordinal, class and kinds; the clock and the slots
/// are filled in by the write (`Session::wake_line`), from the figures the host's claim judged.
fn wake_line(wake: &WakeSeen) -> FactLine {
    templates::wake(wake.ord, wake.class, &wake.kinds, None, None)
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
        // A wake's line is no hold's ([`wake_opens`]).
        RuleId::Wake => None,
    }
}

fn spent(rule: RuleId, books: &Books, now_ms: i64) -> bool {
    match rule {
        RuleId::WaitNudge => books.nudges_today(now_ms) >= NUDGES_PER_DAY,
        RuleId::HoldRoom => books.rooms.len() >= HOLD_ROOM_PER_HOLD,
        // Once a wake: a told one opens nothing ([`wake_opens`]).
        RuleId::Wake => false,
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
    /// Its episode (`episode=<hex>`), where the row names one: a wake's books by it.
    pub episode: Option<u64>,
}

/// THE LINES A LOOP BEFORE THIS ONE BOOKED for session `sid` in the ledger text `body`, oldest
/// first, which [`Books`] seeds from: each `fact-would@v1` row (what `observe` would have typed),
/// and each `fact-hold@v1` or `fact-wake@v1` row `typed` that names its rule (what `prompt`
/// typed: the door's outcome rows). A row this build cannot read is passed over, and so is the
/// task door's (`door=task`, [`super::door::is_task_row`]): a note is no line.
#[must_use]
pub fn would_rows_in(body: &str, sid: Option<&str>) -> Vec<WouldRow> {
    use crate::supervise::facts::FactKind;
    let (hold, wake) = (FactKind::Hold.rule(), FactKind::Wake.rule());
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut rows = Vec::new();
    for line in body.lines() {
        if !(line.contains(WOULD_RULE) || line.contains(hold) || line.contains(wake))
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
            Some(rule) => (rule == hold || rule == wake) && text("decision") == Some("typed"),
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
        if super::door::is_task_row(reason) {
            continue;
        }
        let mut rule = None;
        let mut held_ms = None;
        let mut episode = None;
        for word in reason.split(' ') {
            if let Some(name) = word.strip_prefix("rule=") {
                rule = RuleId::of_name(name);
            } else if let Some(n) = word.strip_prefix("held_ms=") {
                held_ms = n.parse::<i64>().ok().filter(|n| *n >= 0);
            } else if let Some(hex) = word.strip_prefix("episode=") {
                episode = u64::from_str_radix(hex, 16).ok();
            }
        }
        if let (Some(rule), Some(held_ms)) = (rule, held_ms) {
            rows.push(WouldRow {
                rule,
                row_ms,
                held_ms,
                episode,
            });
        }
    }
    rows
}

/// An episode's id: the rule, its revision (`builtin` for these), the hold it is of (by its
/// start in unix ms) and its ordinal in that hold, hashed; a wake's, its declaration's stamp and
/// its ordinal in the book ([`super::wakes_seen::wake_episode`] is `episode(RuleId::Wake,
/// declared_ms, ord)`). Every successor that carries a hold
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
