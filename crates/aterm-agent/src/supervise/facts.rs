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
//! Three kinds, in priority order among what a reading names (a wake's line is ranked at a break
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
//! * [`FactKind::Hold`]: the session's wait on its own background work, whose rows the hold
//!   clock writes (`occupancy::hold`), and the occupancy door's outcome rows under `prompt`
//!   (`occupancy::door`).
//! * [`FactKind::Wake`]: a wake the agent declared in its session's book whose condition held
//!   (`occupancy::wakes`), told once an episode. Its episode is the wake's
//!   (`occupancy::wakes_seen::wake_episode`: its declaration's stamp and ordinal), which a
//!   successor names again exactly, so a seed holds it told per episode, never kind-wide.
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

/// What a fact is about, in priority order: where two open episodes are untold, the first kind
/// is told first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FactKind {
    /// A live background call of the agent's that has made no progress (the hung-call design's
    /// R2; `aterm_vitals::jobs::JobRow::no_progress`).
    NoProgress,
    /// The session's wait on its own background work: how long it has held, and whether the
    /// agent declared it (`WAITING:`; the occupancy design's §3.1 `hold`).
    Hold,
    /// A wake of the agent's book whose condition held (`wake@v1`; the occupancy design's §4.1).
    Wake,
}

impl FactKind {
    /// Every kind, in priority order.
    pub const ALL: [FactKind; 3] = [FactKind::NoProgress, FactKind::Hold, FactKind::Wake];

    /// The ledger rule a row of this kind is booked under, `fact-<kind>@v1`. Stable: a successor
    /// loop seeds by it ([`Self::of_rule`]).
    #[must_use]
    pub const fn rule(self) -> &'static str {
        match self {
            FactKind::NoProgress => "fact-no-progress@v1",
            FactKind::Hold => "fact-hold@v1",
            FactKind::Wake => "fact-wake@v1",
        }
    }

    /// Its word, as the Harness page counts it and an intent row names it (`kind=<word>`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            FactKind::NoProgress => "no-progress",
            FactKind::Hold => "hold",
            FactKind::Wake => "wake",
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
    /// and its leader's birth, and a wake's its declaration's stamp and ordinal, which survive a
    /// restart, so its own key carries it (the slice-2 plan's S3).
    fn seeds_whole(self) -> bool {
        use crate::supervise::occupancy::fault::{Fault, armed};
        match self {
            FactKind::NoProgress => false,
            FactKind::Hold => true,
            FactKind::Wake => armed(Fault::KindWideWakeSeed),
        }
    }
}

/// One episode of a kind's condition: the kind and an id that survives a restart (a job's
/// `(pgid, leader birth)` folded, [`Self::no_progress`]; for the hold, the episode's start in
/// Unix milliseconds).
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
/// longest) first, as of `as_of_ms` (unix ms). Plain values only: this crate takes no dependency
/// on the engine's types.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactsReading {
    /// When the reading was taken, unix ms.
    pub as_of_ms: i64,
    /// Each call at no progress.
    pub no_progress: Vec<BackgroundCall>,
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

    /// Every open episode the reading names, with its sentence and its quiet: in priority order,
    /// each kind's worst first. A call with no birth names none.
    fn open(&self) -> Vec<(EpisodeKey, String, u64)> {
        let mut open: Vec<(EpisodeKey, String, u64)> = self
            .no_progress
            .iter()
            .filter_map(|c| Some((EpisodeKey::of_call(c)?, c.told(), c.quiet_ms)))
            .collect();
        open.sort_by(|a, b| a.0.kind.cmp(&b.0.kind).then(b.2.cmp(&a.2)));
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

    /// FOLD IN WHAT THE HOST READ at `now_ms` ([`FactKind::NoProgress`]): on a reading that is
    /// present and fresh, a told episode it no longer names is over (the call made progress or
    /// ended) and forgotten. An absent or stale reading forgets nothing.
    pub fn observe(&mut self, reading: Option<&FactsReading>, now_ms: i64) {
        let Some(r) = reading.filter(|r| r.fresh_at(now_ms)) else {
            return;
        };
        let open: Vec<EpisodeKey> = r.open().into_iter().map(|(key, ..)| key).collect();
        self.observe_closed(FactKind::NoProgress, &open);
    }

    /// THE FACT TO TELL NOW: the highest-priority open episode of a present, fresh reading that
    /// was not told and stands left in no composer, its sentence within [`FACT_CAP`]; else
    /// `None`. The others wait, and one whose
    /// episode closes meanwhile is never told late. Told only from a reading at most
    /// [`FACT_FRESH_MS`] old ([`FactsReading::tells_at`]).
    #[must_use]
    pub fn pending(&self, reading: Option<&FactsReading>, now_ms: i64) -> Option<Fact> {
        let r = reading.filter(|r| r.tells_at(now_ms))?;
        r.open()
            .into_iter()
            .find(|(key, text, _)| {
                !self.is_spoken_for(*key) && !text.is_empty() && text.chars().count() <= FACT_CAP
            })
            .map(|(key, text, _)| Fact {
                key,
                text,
                as_of_ms: r.as_of_ms,
            })
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
