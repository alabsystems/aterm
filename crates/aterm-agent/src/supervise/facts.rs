// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FACTS SEAM'S STATE (`docs/DESIGN-system-awareness-2026-10-16.md` §14.4;
//! `docs/DESIGN-harness-occupancy-2026-10-20.md` §8): which episodes of a
//! measured condition a session has been told, kept apart from the measuring and the typing.
//! Pure and clockless: every instant is the caller's milliseconds.
//!
//! An episode is told once, across every door that tells it. A successor loop (a live upgrade,
//! a seamless update) seeds the state from the ledger's `fact-*@v1` rows ([`FactsState::seed`]):
//! an episode whose last row is an intent with no outcome was never told (the loop may have died
//! between), and a kind whose episodes a restart cannot name again exactly ([`FactKind::Hold`],
//! keyed by a hold start the successor places anew) counts as told within [`SEED_HOLD_MS`] until
//! its condition has been seen clear once ([`FactsState::observe_closed`]), so a restart never
//! tells it twice.
//!
//! Five kinds, in priority order among what a reading names (a wake's line is ranked at a break
//! by the occupancy door, `occupancy::rules::rank`, not by this order):
//!
//! * [`FactKind::NoProgress`]: a live background call of this session's agent that the host's
//!   jobs reading says has made no progress (the hung-call design's R2 and R3;
//!   [`crate::supervise::BackgroundCall`]), read through [`FactsReading`]
//!   (`IdleHost::facts`). Its episode is the call's run of no progress, keyed by its process
//!   group and its leader's birth ([`EpisodeKey::no_progress`]), a stable fold a successor reads
//!   back exactly. It rides a prompt the turn-end policy types anyway
//!   (`policy::turn_end::FACT_RIDES`) and, with `[harness] stuck_break_note` on, is typed at a
//!   break of the agent's own background work (the occupancy design's §2.1 rank 4). It is
//!   forgotten only on a present, fresh reading that no longer names it ([`FactsState::observe`]),
//!   so an absent or stale reading never makes a told fact new again.
//! * [`FactKind::ProgramCheck`]: programs macOS's program check holds before their first
//!   instruction, in this session's agent's calls and the orphans they left
//!   (`docs/DESIGN-leftover-processes-2026-10-16.md` §6.3), read through [`FactsReading::held`]
//!   ([`HeldReading`]). Its episode is the session's run of held programs: it opens at the first
//!   fresh reading that names one and closes at a fresh reading that names none, whichever
//!   programs come and go between, so a queue that holds one program after another is told once.
//!   Its id is the opening reading's time, which a successor cannot place again, so a seed holds
//!   the kind ([`SEED_HOLD_MS`]). It rides the same prompts and the same break as the call at no
//!   progress, after it.
//! * [`FactKind::Hold`]: the session's wait on its own background work, whose rows the hold
//!   clock writes (`occupancy::hold`), and the occupancy door's outcome rows under `prompt`
//!   (`occupancy::door`).
//! * [`FactKind::Wake`]: a wake the agent declared in its session's book whose condition held
//!   (`occupancy::wakes`), told once an episode. Its episode is the wake's
//!   (`occupancy::wakes_seen::wake_episode`: its declaration's stamp and ordinal), which a
//!   successor names again exactly, so a seed holds it told per episode, never kind-wide.
//! * [`FactKind::Leftover`]: processes this session's agent started that still run
//!   (`docs/DESIGN-leftovers-by-default-2026-10-30.md` §4; the agent's left rows,
//!   `aterm_vitals::left::agents_left`), read through [`FactsReading::left`]
//!   ([`LeftoverReading`]). Its episode is shaped as the held programs' is: it opens at the first
//!   fresh reading that names a row and closes at a fresh reading that names none, its id the
//!   opening reading's time, so a seed holds the kind. It rides only the prompts the turn-end
//!   policy types anyway (`policy::turn_end::FACT_RIDES`), never a break's line
//!   ([`FactKind::rides_breaks`]).
//! * [`FactKind::Overlap`]: other sessions of this aterm that change a file this session's branch
//!   changes, read from the sessions' work records through its own door (`IdleHost::overlap`,
//!   [`OverlapReading`], as old as the oldest record it rests on), told as a count and those
//!   sessions' sids. Its episode is one set of
//!   peers ([`EpisodeKey::overlap`], their sids folded), which a successor names again exactly:
//!   the same peers with more files are not told again, a new peer is a new episode, and a
//!   present, fresh reading that names no peer, or other ones, closes it
//!   ([`FactsState::observe_overlap`]). It rides a prompt the turn-end policy types anyway, after
//!   every other kind, and is never typed at a break: the stuck line reads [`FactsReading`] alone.
//!
//! A line any door wrote into the composer and did not submit (`Written::Left`) is neither told
//! nor untold: it is LEFT ([`Left`]), its episode told by no door again and a wake's claim kept,
//! until the session's next look settles it ([`FactsState::look`]): sent, told; cleared unsent,
//! released; the session gone, released. One account for every door (the slice-2 plan's 2.9b).
//!
//! [`FactKind`] shares its name with `crate::harness::footer::FactKind` (the footer's fact
//! lines): name this one by its path, never glob-import both.

use std::collections::BTreeMap;

use super::background::BackgroundCall;
use super::occupancy::door::LeftRow;
use super::overlap::{OverlapReading, PeerSid};

/// What a fact is about, in priority order: where two open episodes are untold, the first kind
/// is told first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FactKind {
    /// A live background call of the agent's that has made no progress (the hung-call design's
    /// R2; `aterm_vitals::jobs::JobRow::no_progress`).
    NoProgress,
    /// Programs of the agent's calls (or their orphans) that macOS's program check holds before
    /// their first instruction ([`HeldReading`]): a call that hangs is the agent's to fix first.
    ProgramCheck,
    /// The session's wait on its own background work: how long it has held, and whether the
    /// agent declared it (`WAITING:`; the occupancy design's §3.1 `hold`).
    Hold,
    /// A wake of the agent's book whose condition held (`wake@v1`; the occupancy design's §4.1).
    Wake,
    /// Processes the agent started that still run ([`LeftoverReading`]): told after the
    /// others a reading names, a record of what it may end before it is done.
    Leftover,
    /// Other sessions of this aterm that change a file this session's branch changes
    /// ([`OverlapReading`]).
    Overlap,
}

impl FactKind {
    /// Every kind, in priority order.
    pub const ALL: [FactKind; 6] = [
        FactKind::NoProgress,
        FactKind::ProgramCheck,
        FactKind::Hold,
        FactKind::Wake,
        FactKind::Leftover,
        FactKind::Overlap,
    ];

    /// The ledger rule a row of this kind is booked under, `fact-<kind>@v1`. Stable: a successor
    /// loop seeds by it ([`Self::of_rule`]).
    #[must_use]
    pub const fn rule(self) -> &'static str {
        match self {
            FactKind::NoProgress => "fact-no-progress@v1",
            FactKind::ProgramCheck => "fact-program-check@v1",
            FactKind::Hold => "fact-hold@v1",
            FactKind::Wake => "fact-wake@v1",
            FactKind::Leftover => "fact-leftover@v1",
            FactKind::Overlap => "fact-overlap@v1",
        }
    }

    /// Its word, as the Harness page counts it and an intent row names it (`kind=<word>`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            FactKind::NoProgress => "no-progress",
            FactKind::ProgramCheck => "program-check",
            FactKind::Hold => "hold",
            FactKind::Wake => "wake",
            FactKind::Leftover => "leftover",
            FactKind::Overlap => "overlap",
        }
    }

    /// The kind a ledger rule names, or `None` for a rule that is no fact's.
    #[must_use]
    pub fn of_rule(rule: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.rule() == rule)
    }

    /// The kind [`Self::word`] names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.word() == word)
    }

    /// Whether a seed holds the whole kind told ([`FactsState::seed`]): a kind whose episode ids
    /// a successor cannot name again exactly. A no-progress episode's id folds the call's group
    /// and its leader's birth, a wake's its declaration's stamp and ordinal, and an overlap's its
    /// peers' sids, which survive a restart, so its own key carries it; a held episode's, the
    /// hold's and the leftover episode's are the times they opened, which a successor places anew.
    fn seeds_whole(self) -> bool {
        use crate::supervise::occupancy::fault::{Fault, armed};
        match self {
            FactKind::NoProgress | FactKind::Overlap => false,
            FactKind::ProgramCheck | FactKind::Hold | FactKind::Leftover => true,
            FactKind::Wake => armed(Fault::KindWideWakeSeed),
        }
    }

    /// Whether the stuck line may tell it at a break of the agent's own background work
    /// (`[harness] stuck_break_note`; [`FactsState::pending_at_break`]): a call at no progress and
    /// a held program, which the background work waits on, and neither a leftover nor an overlap,
    /// which ride only a prompt typed anyway.
    #[must_use]
    pub const fn rides_breaks(self) -> bool {
        !matches!(self, FactKind::Leftover | FactKind::Overlap)
    }
}

/// One episode of a kind's condition: the kind and an id (a job's `(pgid, leader birth)` folded,
/// [`Self::no_progress`], which survives a restart; for the held programs and the leftovers, the
/// opening reading's time, and for the hold, the episode's start, each in Unix milliseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EpisodeKey {
    /// What the episode is of.
    pub kind: FactKind,
    /// Which episode of it.
    pub id: u64,
}

impl EpisodeKey {
    /// The no-progress episode of the call whose process group is `pgid` and whose leader was born
    /// at `birth`: FNV-1a over the two, little endian, never std's `DefaultHasher`, whose output
    /// may change between builds, since a key is read back from the ledger by a later one.
    #[must_use]
    pub fn no_progress(pgid: u32, birth: u64) -> Self {
        let mut bytes = [0u8; 12];
        bytes[..4].copy_from_slice(&pgid.to_le_bytes());
        bytes[4..].copy_from_slice(&birth.to_le_bytes());
        Self {
            kind: FactKind::NoProgress,
            id: aterm_hash::fnv1a_64(&bytes),
        }
    }

    /// The episode of `call`; `None` where its leader's birth was not read, which nothing could
    /// tell from a later process in a reused group.
    #[must_use]
    pub fn of_call(call: &BackgroundCall) -> Option<Self> {
        call.birth.map(|birth| Self::no_progress(call.pgid, birth))
    }

    /// The overlap episode with the sessions `peers`: FNV-1a over their sids, sorted and joined
    /// by new lines, so one set of peers is one episode however it is listed.
    #[must_use]
    pub fn overlap(peers: &[PeerSid]) -> Self {
        let mut sids: Vec<&str> = peers.iter().map(PeerSid::as_str).collect();
        sids.sort_unstable();
        Self {
            kind: FactKind::Overlap,
            id: aterm_hash::fnv1a_64(sids.join("\n").as_bytes()),
        }
    }
}

/// The longest fact sentence, in characters: one sentence, filled from a closed template.
pub const FACT_CAP: usize = 200;

/// How old the reading a fact comes from may be when it is typed.
pub const FACT_FRESH_MS: i64 = 5_000;

/// How old a no-progress reading may be and still forget a told episode it no longer names: the
/// jobs board's own bound (`aterm-gui`'s `vitals_jobs::JOBS_FRESH`). A fact is told only from a
/// reading [`FACT_FRESH_MS`] old ([`FactsReading::tells_at`]), counted from when the probe read
/// it, which the board meets: its view of a call at no progress is read about every 2 s, a window
/// in view or none (`vitals_jobs::told_after`, `windowless_after`), and the host re-proves at the
/// ask that every leader it names still lives with the same birth.
pub const NO_PROGRESS_FRESH_MS: i64 = 60_000;

/// After a restart, a kind told this recently waits for its condition to clear once before any
/// episode of it is told again.
pub const SEED_HOLD_MS: i64 = 30 * 60_000;

/// What a ledger row says of an episode, for [`FactsState::seed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedOutcome {
    /// The loop meant to tell it: written before the act, so an intent with no outcome after it
    /// is an episode the loop may never have told.
    Intent,
    /// It was told.
    Typed,
    /// The loop meant to and did not (a guard held it at the last moment): untold.
    Skipped,
    /// A wake the agent's own `await wake` answered, and the loop saw a turn run after it (a
    /// `fact-wake@v1` row `waiting … told door=await`): told, with nothing typed.
    Answered,
    /// Written into the composer and not submitted (a `fact-<kind>@v1` row `waiting … left`):
    /// neither told nor untold, its look owed ([`FactsState::seed_left`]).
    Left,
}

/// A LINE LEFT IN THE COMPOSER (`Written::Left`): written at a door and not submitted, its tell
/// or its release owed at the session's next look ([`FactsState::look`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Left {
    /// The door that wrote it, with the words its rows carry.
    pub row: LeftRow,
    /// The line as its rows carry it (their `command`).
    pub text: String,
    /// When it was left, unix ms.
    pub at_ms: i64,
    /// The session's submit count where it was left (`text --json`'s `submits`, as the read
    /// before the write's Enter saw it); `None` on a host that serves none.
    pub submits: Option<u64>,
    /// A turn was seen running since, at a look whose composer was unread.
    pub ran: bool,
    /// Read back from a loop's row before this one (`occupancy::door::left_rows_in`), not
    /// written by this loop: its line is the ledger's, never this loop's own template, so a
    /// tell of it is raised to no one (`Session::left_look`).
    pub seeded: bool,
    /// Its count, and the turns this loop saw since, are this loop's to weigh against the host it
    /// reads now. Each host process counts its own submits from 0 (a seamless update or a
    /// relaunch starts the count again), so a line read back from a loop's row before this one,
    /// or left before the host came back from an outage ([`FactsState::recount`]), keeps a count
    /// no look may weigh ([`FactsState::look`]).
    pub counted: bool,
}

/// What the session's next look saw of the lines left in its composer ([`FactsState::look`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftLook {
    /// A read of the session.
    Read {
        /// Its composer holds a draft (`Some(true)`), holds none (`Some(false)`), or is unread
        /// (`None`: a dialog over it, no composer drawn).
        draft: Option<bool>,
        /// Its submit count, where the host serves one.
        submits: Option<u64>,
        /// A turn runs.
        live: bool,
    },
    /// The policy's draft step sent the draft standing (its fenced Enter taken).
    Submitted,
    /// The session is gone.
    Gone,
}

/// What a look made of a left line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// It went, and how: [`SUBMITTED`] (the session's submit count rose, or the policy's draft
    /// step sent it), [`TURN`] (a turn ran with the composer clear, on a host that counts no
    /// submits), or [`UNCOUNTED`] (the composer clear and its count none this loop may weigh:
    /// told, since a line that went must never go again).
    Told(&'static str),
    /// It did not, and why: [`CLEARED`] (the composer holds no draft and nothing was sent) or
    /// [`GONE`] (the session ended).
    Released(&'static str),
}

/// A left line went by an Enter: the session's count rose, or the policy's draft step sent it.
pub const SUBMITTED: &str = "submitted";
/// A left line went with a turn seen running with the composer clear (no count served).
pub const TURN: &str = "turn";
/// A left line's composer is clear and its count is none this loop may weigh ([`Left::counted`],
/// or a count lower than it was left at): whether the person sent it or cleared it no look can
/// say, so it is booked told, one tell lost at most and never a second.
pub const UNCOUNTED: &str = "uncounted";
/// A left line's composer holds no draft and nothing was sent.
pub const CLEARED: &str = "cleared";
/// A left line's session ended.
pub const GONE: &str = "gone";

/// A fact to tell: its episode, its one sentence, and when the reading it came from was taken
/// (unix ms).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// Its episode.
    pub key: EpisodeKey,
    /// One sentence from a closed template, no imperative, at most [`FACT_CAP`] characters.
    pub text: String,
    /// When the reading it came from was taken, unix ms.
    pub as_of_ms: i64,
}

/// WHAT THE HOST READ FOR THIS SESSION'S AGENT (`IdleHost::facts`): the live background calls of
/// THIS session's agent that made no progress, each regardless of the others, worst (quiet
/// longest) first, as of `as_of_ms` (unix ms), and the programs of its calls macOS's program
/// check holds ([`Self::held`], as of its own time), and the processes it started that still run
/// ([`Self::left`], as of its own time). Plain values only: this crate takes no dependency on the
/// engine's types.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactsReading {
    /// When the reading was taken, unix ms.
    pub as_of_ms: i64,
    /// Each call at no progress.
    pub no_progress: Vec<BackgroundCall>,
    /// The held programs ([`FactKind::ProgramCheck`]): `None` where they were not read, which
    /// neither opens nor closes an episode; `Some` with no program where none is held, which
    /// closes one.
    pub held: Option<HeldReading>,
    /// The agent's left rows ([`FactKind::Leftover`]): `None` where they were not read (no
    /// leftover view, a partial one, or a tape recorded before they were read), which neither
    /// opens nor closes an episode; `Some` with a count of 0 where none is left, which closes one.
    pub left: Option<LeftoverReading>,
}

/// THE AGENT'S LEFT ROWS THE HOST READ FOR THIS SESSION ([`FactsReading::left`]): how many
/// processes its agent started still run (`aterm_vitals::left::agents_left`: offered rows its
/// session's environment ties to its tab, born after its agent started, and the offered members
/// of a dead verify run that rode in it) and the oldest one's age, as of the leftover view that
/// named them (`as_of_ms`, unix ms). Numbers only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeftoverReading {
    /// When the leftover view was read, unix ms.
    pub as_of_ms: i64,
    /// How many rows it names.
    pub count: u32,
    /// The oldest one's fork age at that reading, ms; `None` with no row, or none whose age read.
    pub oldest_ms: Option<u64>,
}

impl LeftoverReading {
    /// No older than [`NO_PROGRESS_FRESH_MS`] at `now_ms`: a part that may open or close an
    /// episode.
    #[must_use]
    pub fn fresh_at(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.as_of_ms) <= NO_PROGRESS_FRESH_MS
    }

    /// No older than [`FACT_FRESH_MS`] at `now_ms`: a part a fact may be told from.
    #[must_use]
    pub fn tells_at(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.as_of_ms) <= FACT_FRESH_MS
    }

    /// THE LEFTOVER SENTENCE, from a closed template of numbers only (system awareness §14.4: no
    /// exe word, path, argv or imperative): how many processes, and the oldest's age in the words
    /// the held sentence uses (`aterm_uds::process::held_age`), left out where no age was read.
    /// Empty with none.
    #[must_use]
    pub fn told(&self) -> String {
        let age = self.oldest_ms.map(aterm_uds::process::held_age);
        match (self.count, age) {
            (0, _) => String::new(),
            (1, age) => format!(
                "1 process this session started still runs{}; `aterm ctl @self sys left` lists it",
                age.map_or_else(String::new, |age| format!(", for {age}"))
            ),
            (n, age) => format!(
                "{n} processes this session started still run{}; `aterm ctl @self sys left` lists \
                 them",
                age.map_or_else(String::new, |age| format!(", the oldest for {age}"))
            ),
        }
    }
}

/// One program of the agent's calls (or an orphan of one) that macOS's program check holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldCall {
    /// Its pid.
    pub pid: u32,
    /// The actual exec version at the held reading, when available. The live host must match it
    /// when re-proving the wait; an older tape may omit it without changing its recorded answer.
    pub exec_version: Option<u64>,
    /// How long it has been held, ms: since the first reading that saw it held, to the reading's
    /// time.
    pub held_ms: u64,
}

/// THE HELD PROGRAMS THE HOST READ FOR THIS SESSION'S AGENT ([`FactsReading::held`]): each program
/// of its calls and their orphans that macOS's program check holds, oldest (held longest) first,
/// as of `as_of_ms` (unix ms), when the host last proved the claim: its own re-proof at the ask
/// where it proved one held, else the jobs view's. The part keeps its own time because the view
/// that names a held program is read every 30 s, and a fact is told only from a reading
/// [`FACT_FRESH_MS`] old.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeldReading {
    /// When the claim was last proved, unix ms.
    pub as_of_ms: i64,
    /// Each held program, oldest first.
    pub programs: Vec<HeldCall>,
}

/// How many pids the held sentence names before `…`.
const HELD_PIDS_SHOWN: usize = 4;

impl HeldReading {
    /// No older than [`NO_PROGRESS_FRESH_MS`] at `now_ms` (the jobs board's bound, as for the
    /// calls at no progress): a part that may open or close an episode.
    #[must_use]
    pub fn fresh_at(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.as_of_ms) <= NO_PROGRESS_FRESH_MS
    }

    /// No older than [`FACT_FRESH_MS`] at `now_ms`: a part a fact may be told from.
    #[must_use]
    pub fn tells_at(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.as_of_ms) <= FACT_FRESH_MS
    }

    /// The longest any program has been held, ms; 0 with none.
    #[must_use]
    pub fn oldest_ms(&self) -> u64 {
        self.programs.iter().map(|p| p.held_ms).max().unwrap_or(0)
    }

    /// THE HELD SENTENCE, from a closed template of numbers only (system awareness §14.4: no exe
    /// word, path, argv or imperative): how many programs, at most [`HELD_PIDS_SHOWN`] pids
    /// (oldest first) then `…`, and the oldest's age in the words every surface that counts the
    /// wait shares (`aterm_uds::process::held_age`). Empty with no program.
    #[must_use]
    pub fn told(&self) -> String {
        let n = self.programs.len();
        if n == 0 {
            return String::new();
        }
        let mut pids: Vec<String> = self
            .programs
            .iter()
            .take(HELD_PIDS_SHOWN)
            .map(|p| format!("pid {}", p.pid))
            .collect();
        if n > HELD_PIDS_SHOWN {
            pids.push("…".to_string());
        }
        let age = aterm_uds::process::held_age(self.oldest_ms());
        let (programs, oldest) = if n == 1 {
            ("1 program".to_string(), "for")
        } else {
            (format!("{n} programs"), "the oldest for")
        };
        format!(
            "macOS's program check is holding {programs} this tab started ({}), {oldest} {age}; a \
             retry waits in the same line",
            pids.join(", ")
        )
    }
}

impl FactsReading {
    /// No older than [`NO_PROGRESS_FRESH_MS`] at `now_ms` (a reading from the future, a clock
    /// stepped back, is no older).
    #[must_use]
    pub fn fresh_at(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.as_of_ms) <= NO_PROGRESS_FRESH_MS
    }

    /// No older than [`FACT_FRESH_MS`] at `now_ms`: a reading a fact may be told from (system
    /// awareness §14.4's gate), as [`Self::fresh_at`] is one that may forget.
    #[must_use]
    pub fn tells_at(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.as_of_ms) <= FACT_FRESH_MS
    }

    /// Every no-progress episode the reading names, with its sentence and its quiet, worst first.
    /// A call with no birth names none.
    fn open(&self) -> Vec<(EpisodeKey, String, u64)> {
        let mut open: Vec<(EpisodeKey, String, u64)> = self
            .no_progress
            .iter()
            .filter_map(|c| Some((EpisodeKey::of_call(c)?, c.told(), c.quiet_ms)))
            .collect();
        open.sort_by_key(|a| std::cmp::Reverse(a.2));
        open
    }
}

/// Which episodes this session has been told ([`FactsState::told`]), and which kinds a seed holds
/// told until their condition clears.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactsState {
    /// Each episode told, with when.
    told: BTreeMap<EpisodeKey, i64>,
    /// Each kind a seed holds told (with the last time it was told), until its condition has
    /// been seen clear once.
    seeded: Vec<(FactKind, i64)>,
    /// Each episode the occupancy loop's task door booked (`door=task`), with when: a fact word
    /// noted on a managed session's task record, a host's answer that took none, or under
    /// `observe` the note that would have been. A mark apart from [`Self::told`]: a note tells
    /// the session's manager, never the session, so a line the managed guard no longer holds is
    /// still the session's to be told.
    noted: BTreeMap<EpisodeKey, i64>,
    /// The held episode open now ([`FactKind::ProgramCheck`]): opened by a fresh reading that
    /// names a held program, closed by a fresh one that names none ([`Self::observe`]).
    held_open: Option<EpisodeKey>,
    /// The leftover episode open now ([`FactKind::Leftover`]): opened by a fresh reading that
    /// names a row, closed by a fresh one that names none ([`Self::observe`]).
    left_open: Option<EpisodeKey>,
    /// Each episode whose line stands left in the composer ([`Self::leave`]), until a look
    /// settles it ([`Self::look`]).
    left: BTreeMap<EpisodeKey, Left>,
}

impl FactsState {
    /// The episodes of `kind` open now, from one reading of it: a told episode of that kind not
    /// among them closed, so it is forgotten (the condition's next episode is told again), and a
    /// seed's hold on the kind, with no open episode of it, has been seen clear. The other kinds
    /// are not this reading's to judge, and keep what they hold.
    pub fn observe_closed(&mut self, kind: FactKind, open: &[EpisodeKey]) {
        self.told
            .retain(|key, _| key.kind != kind || open.contains(key));
        self.noted
            .retain(|key, _| key.kind != kind || open.contains(key));
        self.seeded
            .retain(|(seeded, _)| *seeded != kind || open.iter().any(|key| key.kind == kind));
    }

    /// FOLD IN WHAT THE HOST READ at `now_ms`, each part by its own freshness. The held part
    /// ([`FactKind::ProgramCheck`]), present and fresh ([`HeldReading::fresh_at`]): naming a
    /// program, it opens the held episode where none is open (keyed by its own time) and keeps
    /// the open one whichever programs it names; naming none, it closes the episode and forgets
    /// it. The leftover part ([`FactKind::Leftover`]), present and fresh
    /// ([`LeftoverReading::fresh_at`]), opens and closes its own episode the same way. The
    /// no-progress part ([`FactKind::NoProgress`]), present and fresh: a told episode it no longer
    /// names is over (the call made progress or ended) and forgotten. An absent or stale part
    /// opens, closes and forgets nothing.
    pub fn observe(&mut self, reading: Option<&FactsReading>, now_ms: i64) {
        let Some(r) = reading else {
            return;
        };
        if let Some(left) = r.left.as_ref().filter(|l| l.fresh_at(now_ms)) {
            if left.count == 0 {
                self.left_open = None;
                self.observe_closed(FactKind::Leftover, &[]);
            } else {
                let key = *self.left_open.get_or_insert(EpisodeKey {
                    kind: FactKind::Leftover,
                    id: left.as_of_ms.unsigned_abs(),
                });
                self.observe_closed(FactKind::Leftover, &[key]);
            }
        }
        if let Some(held) = r.held.as_ref().filter(|h| h.fresh_at(now_ms)) {
            if held.programs.is_empty() {
                self.held_open = None;
                self.observe_closed(FactKind::ProgramCheck, &[]);
            } else {
                let key = *self.held_open.get_or_insert(EpisodeKey {
                    kind: FactKind::ProgramCheck,
                    id: held.as_of_ms.unsigned_abs(),
                });
                self.observe_closed(FactKind::ProgramCheck, &[key]);
            }
        }
        if r.fresh_at(now_ms) {
            let open: Vec<EpisodeKey> = r.open().into_iter().map(|(key, ..)| key).collect();
            self.observe_closed(FactKind::NoProgress, &open);
        }
    }

    /// THE FACT TO TELL NOW: the highest-priority open episode that was not told and stands left
    /// in no composer, its sentence within [`FACT_CAP`]; else `None`. A call at no progress comes
    /// from the reading's own part, told only while it is at most [`FACT_FRESH_MS`] old
    /// ([`FactsReading::tells_at`]); the held programs from the episode [`Self::observe`] opened,
    /// told only while the held part names one and is that young by its own time
    /// ([`HeldReading::tells_at`]); the leftovers likewise, by their own part's time
    /// ([`LeftoverReading::tells_at`]). The others wait, and one whose episode closes meanwhile is
    /// never told late. The fact a prompt typed at a turn end carries; a break's is
    /// [`Self::pending_at_break`].
    #[must_use]
    pub fn pending(&self, reading: Option<&FactsReading>, now_ms: i64) -> Option<Fact> {
        self.pending_of(reading, now_ms, |_| true)
    }

    /// [`Self::pending`] among the kinds a break's stuck line tells ([`FactKind::rides_breaks`]).
    #[must_use]
    pub fn pending_at_break(&self, reading: Option<&FactsReading>, now_ms: i64) -> Option<Fact> {
        self.pending_of(reading, now_ms, FactKind::rides_breaks)
    }

    fn pending_of(
        &self,
        reading: Option<&FactsReading>,
        now_ms: i64,
        admits: impl Fn(FactKind) -> bool,
    ) -> Option<Fact> {
        let r = reading?;
        let mut open: Vec<(EpisodeKey, String, i64)> = Vec::new();
        if let (Some(key), Some(held)) = (
            self.held_open,
            r.held
                .as_ref()
                .filter(|h| h.tells_at(now_ms) && !h.programs.is_empty()),
        ) {
            open.push((key, held.told(), held.as_of_ms));
        }
        if let (Some(key), Some(left)) = (
            self.left_open,
            r.left
                .as_ref()
                .filter(|l| l.tells_at(now_ms) && l.count > 0),
        ) {
            open.push((key, left.told(), left.as_of_ms));
        }
        if r.tells_at(now_ms) {
            open.extend(
                r.open()
                    .into_iter()
                    .map(|(key, text, _)| (key, text, r.as_of_ms)),
            );
        }
        open.sort_by_key(|(key, ..)| key.kind);
        open.into_iter()
            .find(|(key, text, _)| {
                admits(key.kind)
                    && !self.is_spoken_for(*key)
                    && !text.is_empty()
                    && text.chars().count() <= FACT_CAP
            })
            .map(|(key, text, as_of_ms)| Fact {
                key,
                text,
                as_of_ms,
            })
    }

    /// FOLD IN THE OVERLAP THE HOST READ at `now_ms` ([`FactKind::Overlap`]): a present reading
    /// young enough to tell ([`OverlapReading::tells_at`]) names the one overlap episode open now
    /// (none where it names no peer), so a told one it no longer names is over and forgotten. An
    /// absent or older reading forgets nothing.
    pub fn observe_overlap(&mut self, reading: Option<&OverlapReading>, now_ms: i64) {
        let Some(r) = reading.filter(|r| r.tells_at(now_ms)) else {
            return;
        };
        let open: Vec<EpisodeKey> = r.episode().into_iter().collect();
        self.observe_closed(FactKind::Overlap, &open);
    }

    /// THE OVERLAP TO TELL NOW: the episode a reading young enough to tell names
    /// ([`OverlapReading::tells_at`]), where it was not told and stands left in no composer, its
    /// sentence within [`FACT_CAP`]; else `None`.
    #[must_use]
    pub fn pending_overlap(&self, reading: Option<&OverlapReading>, now_ms: i64) -> Option<Fact> {
        let r = reading.filter(|r| r.tells_at(now_ms))?;
        let key = r.episode()?;
        let text = r.told();
        (!self.is_spoken_for(key) && !text.is_empty() && text.chars().count() <= FACT_CAP).then(
            || Fact {
                key,
                text,
                as_of_ms: r.as_of_ms(),
            },
        )
    }

    /// `key` was told at `at_ms`: where its act was taken, never where it was decided.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LeftWrite",
            action = "Types",
            project = "conformance_left::project"
        )
    )]
    pub fn told(&mut self, key: EpisodeKey, at_ms: i64) {
        self.told.insert(key, at_ms);
    }

    /// The task door booked `key` at `at_ms` ([`Self::noted`]).
    pub fn note(&mut self, key: EpisodeKey, at_ms: i64) {
        self.noted.insert(key, at_ms);
    }

    /// Whether the task door booked `key` ([`Self::note`]): forgotten with the episode
    /// ([`Self::observe_closed`]). A successor seeds none: the host's edit of a word standing
    /// already moves nothing.
    #[must_use]
    pub fn is_noted(&self, key: EpisodeKey) -> bool {
        self.noted.contains_key(&key)
    }

    /// Whether `key` has been told: the episode itself, or any episode of a kind a seed holds.
    #[must_use]
    pub fn is_told(&self, key: EpisodeKey) -> bool {
        self.told.contains_key(&key) || self.seeded.iter().any(|(kind, _)| *kind == key.kind)
    }

    /// `key`'s LINE IS LEFT in the composer (`Written::Left`): neither told nor untold until a
    /// look settles it ([`Self::look`]), told by no door meanwhile ([`Self::is_left`]). False
    /// under the planted `ReleaseOnLeft` alone, which keeps nothing: the door books it skipped,
    /// as every door did before.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LeftWrite",
            action = "Leaves",
            project = "conformance_left::project"
        )
    )]
    pub fn leave(&mut self, key: EpisodeKey, left: Left) -> bool {
        use crate::supervise::occupancy::fault::{Fault, armed};
        if armed(Fault::ReleaseOnLeft) {
            return false;
        }
        self.left.insert(key, left);
        true
    }

    /// Whether `key`'s line stands left in the composer ([`Self::leave`]).
    #[must_use]
    pub fn is_left(&self, key: EpisodeKey) -> bool {
        self.left.contains_key(&key)
    }

    /// Whether no door may tell `key` now: told ([`Self::is_told`]), or its line left in the
    /// composer ([`Self::is_left`]) until a look settles it. The one test every door's pick makes
    /// ([`Self::pending`], the break's and the point's wake sight, the point's claim): the look
    /// before each pick settles what its read can, so a line still left there is one its read
    /// could not settle, and no door types it again.
    #[must_use]
    pub fn is_spoken_for(&self, key: EpisodeKey) -> bool {
        self.is_told(key) || self.is_left(key)
    }

    /// Each line left in the composer, by its episode.
    pub fn left_lines(&self) -> impl Iterator<Item = (&EpisodeKey, &Left)> {
        self.left.iter()
    }

    /// WHETHER THE POLICY'S DRAFT STEP MAY SEND the draft standing (`TurnEndAction::Submit`),
    /// where `prompt` says whether the owner's `prompt` is in force and `stuck` whether the
    /// `stuck_break_note` grant is: only where every line left stands under a grant still in
    /// force ([`LeftRow::granted`]), so nothing `prompt` typed is sent once `prompt` is no longer
    /// in force: the person sends or clears it, and the next look settles it.
    #[must_use]
    pub fn draft_may_go(&self, prompt: bool, stuck: bool) -> bool {
        use crate::supervise::occupancy::fault::{Fault, armed};
        armed(Fault::SubmitWithoutPrompt)
            || self.left.values().all(|l| l.row.granted(prompt, stuck))
    }

    /// THE NEXT LOOK at `now_ms`, settling each left line ([`Left`]) it can:
    ///
    /// * the session gone: released ([`GONE`]);
    /// * the policy's draft step sent the draft: told ([`SUBMITTED`]);
    /// * a draft stands: nothing settles, and the count it was left at stays (an Enter can send
    ///   nothing past a line standing in the composer but the line, so a count that rose while
    ///   it stood says it went, never that something else did);
    /// * the composer unread: nothing settles; a turn running is kept ([`Left::ran`]);
    /// * the composer clear: told where the session's count rose since ([`SUBMITTED`]), released
    ///   where it did not ([`CLEARED`]); on a host that serves no count, told where a turn runs
    ///   or ran ([`TURN`]), else released;
    /// * the composer clear and the count none this loop may weigh ([`Left::counted`] false, or
    ///   the count now lower than it was left at, a host process's restarted count): told
    ///   ([`UNCOUNTED`]), never released, since a line the person sent would then be typed a
    ///   second time; one cleared unsent is a tell lost.
    ///
    /// Each settled line leaves the account, a told one told at `now_ms`; the caller books its
    /// row and tells or releases a wake's claim.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LeftWrite",
            action = "LookTold",
            project = "conformance_left::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LeftWrite",
            action = "LookReleased",
            project = "conformance_left::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LeftWrite",
            action = "GoneReleased",
            project = "conformance_left::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LeftWrite",
            action = "DraftSends",
            project = "conformance_left::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LeftWrite",
            action = "LookUncounted",
            project = "conformance_left::project"
        )
    )]
    pub fn look(&mut self, look: LeftLook, now_ms: i64) -> Vec<(EpisodeKey, Left, Settled)> {
        use crate::supervise::occupancy::fault::{Fault, armed};
        let mut settled = Vec::new();
        self.left.retain(|key, left| {
            let end = match look {
                LeftLook::Gone => Some(Settled::Released(GONE)),
                LeftLook::Submitted => Some(Settled::Told(SUBMITTED)),
                LeftLook::Read {
                    draft: Some(true), ..
                } => None,
                LeftLook::Read {
                    draft: None, live, ..
                } => {
                    left.ran |= live;
                    None
                }
                LeftLook::Read {
                    draft: Some(false),
                    submits,
                    live,
                } => {
                    let went_back =
                        matches!((left.submits, submits), (Some(at), Some(now)) if now < at);
                    let weighs = (left.counted && !went_back) || armed(Fault::ReleaseUncounted);
                    Some(match (left.submits, submits) {
                        _ if !weighs => Settled::Told(UNCOUNTED),
                        (Some(at), Some(now)) if now > at => Settled::Told(SUBMITTED),
                        (Some(_), Some(_)) => Settled::Released(CLEARED),
                        _ if live || left.ran => Settled::Told(TURN),
                        _ => Settled::Released(CLEARED),
                    })
                }
            };
            let end = match end {
                Some(Settled::Released(CLEARED)) if armed(Fault::LeftNeverSettles) => None,
                Some(Settled::Released(CLEARED)) if armed(Fault::ToldOnClear) => {
                    Some(Settled::Told(SUBMITTED))
                }
                end => end,
            };
            let Some(end) = end else {
                return true;
            };
            settled.push((*key, left.clone(), end));
            false
        });
        for (key, _, end) in &settled {
            if matches!(end, Settled::Told(_)) {
                self.told.insert(*key, now_ms);
            }
        }
        settled
    }

    /// THE LOOP'S HOST CAME BACK FROM AN OUTAGE (`Session::ride_out`): it may be another host
    /// process (a seamless update, a relaunch), whose submit count starts from 0 and whose turns
    /// this loop did not see, so no line left before keeps a count a look may weigh
    /// ([`Left::counted`]).
    pub fn recount(&mut self) {
        for left in self.left.values_mut() {
            left.counted = false;
        }
    }

    /// A successor loop's left lines: each of `lefts` (the ledger's `left` rows,
    /// `occupancy::door::left_rows_in`, read back [`Left::seeded`] with a count no look weighs)
    /// whose episode's LAST fact row in `rows` (oldest first, [`Self::seed`]'s) is that left row,
    /// owed a look as its predecessor's was.
    pub fn seed_left(
        &mut self,
        rows: &[(EpisodeKey, i64, SeedOutcome)],
        lefts: Vec<(EpisodeKey, Left)>,
    ) {
        let mut last: BTreeMap<EpisodeKey, SeedOutcome> = BTreeMap::new();
        for &(key, _, outcome) in rows {
            last.insert(key, outcome);
        }
        for (key, left) in lefts {
            if last.get(&key) == Some(&SeedOutcome::Left) {
                let _kept = self.leave(key, left);
            }
        }
    }

    /// A successor loop's state from the ledger's fact rows, oldest first, at `now_ms`: each
    /// episode whose LAST row says it was typed (or, a wake's, answered and heard) is told, and a
    /// kind whose ids a restart does not carry exactly ([`FactKind::Hold`]) is held told while
    /// that row is younger than [`SEED_HOLD_MS`]. An intent with no outcome after it, or a skip, leaves the episode untold
    /// (`OccupancyDoor`'s `Crash`), and a line left is left ([`Self::seed_left`]).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "OccupancyDoor",
            action = "Crash",
            project = "conformance_occupancy::project"
        )
    )]
    pub fn seed(&mut self, rows: &[(EpisodeKey, i64, SeedOutcome)], now_ms: i64) {
        use crate::supervise::occupancy::fault::{Fault, armed};
        let mut last: BTreeMap<EpisodeKey, (i64, SeedOutcome)> = BTreeMap::new();
        for &(key, at_ms, outcome) in rows {
            last.insert(key, (at_ms, outcome));
        }
        for (key, (at_ms, outcome)) in last {
            let told = matches!(outcome, SeedOutcome::Typed | SeedOutcome::Answered)
                || (outcome == SeedOutcome::Intent && armed(Fault::SeedIntentAsTold))
                || (outcome == SeedOutcome::Skipped && armed(Fault::SeedSkipAsTold));
            if !told {
                continue;
            }
            self.told.insert(key, at_ms);
            if key.kind.seeds_whole() && now_ms.saturating_sub(at_ms) < SEED_HOLD_MS {
                match self.seeded.iter_mut().find(|(kind, _)| *kind == key.kind) {
                    Some((_, when)) => *when = (*when).max(at_ms),
                    None => self.seeded.push((key.kind, at_ms)),
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "facts_tests.rs"]
mod tests;
