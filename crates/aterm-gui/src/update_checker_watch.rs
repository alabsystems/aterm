// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Event-loop watchdog for the background update checker ([`checker_watch::STALL_CONFIRM`],
//! [`App::note_update_health`], [`crate::update_words::CHECKER_STALLED_TITLE`],
//! [`checker_status_tokens`], [`checker_watch::standing_stall`], [`App::update_checker_deadline`]).

use aterm_update::checker_watch::{self, CheckerBeat, CheckerPhase, CheckerVerdict};

use crate::App;

/// What the watchdog remembers between looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CheckerWatchState {
    /// The generation and returned-slot epoch whose stall was already acted on.
    acted_on: Option<ActedOn>,
    /// A stale stamp seen by one look and not yet acted on.
    suspect: Option<Suspect>,
    /// The loop's completed-check count when this watchdog's warning was posted.
    announced_at_checks: Option<u64>,
    /// The warning that a sibling holds `checker.lock` without making progress.
    lock_held: Option<LockHeldSaid>,
}

impl CheckerWatchState {
    /// A stall warning the parent carried across an update is this watchdog's to heal.
    pub(crate) fn adopt_carried_stall(&mut self) {
        self.announced_at_checks.get_or_insert(0);
    }

    /// Whether this watchdog owes a stall warning's heal (test-only).
    #[cfg(all(test, unix))]
    pub(crate) fn owes_stall_heal(&self) -> bool {
        self.announced_at_checks.is_some()
    }

    /// A lock-held warning the parent carried across an update is this watchdog's to heal.
    pub(crate) const fn adopt_carried_lock_held(&mut self) {
        if self.lock_held.is_none() {
            self.lock_held = Some(LockHeldSaid {
                generation: CARRIED_GENERATION,
                checks: 0,
            });
        }
    }
}

/// The generation a streak carried from the parent process is followed under
/// ([`CheckerWatchState::adopt_carried_lock_held`]): none of this process's, since generation 0
/// means no checker registered and the first registers as 1.
const CARRIED_GENERATION: u64 = 0;

/// The streak the lock-held warning stands for ([`lock_held_look`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LockHeldSaid {
    /// The loop generation whose deferrals the warning follows.
    generation: u64,
    /// The loop's completed-check count the last time that generation was seen.
    checks: u64,
}

/// What one look at the deferral streak asks the window to do ([`lock_held_look`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockHeld {
    /// The streak reached `CHECKER_UNGATED_AFTER`.
    Announce(u64),
    /// The streak the warning said is over (a cycle got the lock).
    Heal,
}

/// A stopped sibling holding `checker.lock`, judged at one look (pure).
pub(crate) fn lock_held_look(
    state: &mut CheckerWatchState,
    beat: Option<CheckerBeat>,
) -> Option<LockHeld> {
    let deferrals = beat.map_or(0, |beat| beat.deferrals);
    let said = state.lock_held;
    if let (Some(said), Some(beat)) = (said, beat)
        && (beat.generation == said.generation || deferrals > 0)
    {
        state.lock_held = Some(LockHeldSaid {
            generation: beat.generation,
            checks: beat.checks,
        });
    }
    if deferrals >= checker_watch::CHECKER_UNGATED_AFTER {
        if said.is_some() {
            return None;
        }
        state.lock_held = beat.map(|beat| LockHeldSaid {
            generation: beat.generation,
            checks: beat.checks,
        });
        return Some(LockHeld::Announce(deferrals));
    }
    let said = said?;
    let ended = deferrals == 0
        && beat.map_or(said.generation != CARRIED_GENERATION, |beat| {
            beat.generation == said.generation || beat.checks > said.checks
        });
    if ended {
        state.lock_held = None;
        return Some(LockHeld::Heal);
    }
    None
}

/// How soon the watchdog looks again while a deferral streak stands, in running seconds.
pub(crate) const DEFERRAL_LOOK_SECS: u64 = 60;

/// One stale stamp awaiting its second look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Suspect {
    generation: u64,
    phase: CheckerPhase,
    at_secs: u64,
    /// When the second look is due, in [`checker_watch::now_secs`] seconds.
    confirm_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActedOn {
    generation: u64,
    phase: CheckerPhase,
    at_secs: u64,
    worker_epoch: u64,
}

impl ActedOn {
    fn matches(self, beat: CheckerBeat) -> bool {
        self.generation == beat.generation
            && self.phase == beat.phase
            && self.at_secs == beat.at_secs
    }

    fn suppresses(self, beat: CheckerBeat) -> bool {
        self.matches(beat) && self.worker_epoch >= beat.worker_epoch
    }
}

impl Suspect {
    /// Whether `beat` is still exactly the stamp this suspicion was raised over —
    /// a loop that stamped in between, however briefly, was not stuck.
    fn is(&self, beat: &CheckerBeat) -> bool {
        self.generation == beat.generation
            && self.phase == beat.phase
            && self.at_secs == beat.at_secs
    }
}

/// One stall the watchdog must act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CheckerStall {
    /// The stalled generation.
    pub(crate) generation: u64,
    /// Running seconds since its last stamp.
    pub(crate) for_secs: u64,
    /// Where it was stamped last.
    pub(crate) phase: CheckerPhase,
    /// Whether it holds the check lane.
    pub(crate) holds_lane: bool,
    /// The loop's completed-check count at the stall.
    pub(crate) checks: u64,
}

/// Judge one look. Late heartbeats require confirmation; observed thread-start
/// failures and observed worker exits require only their retry backoff. The window and node share this.
pub(crate) fn judge(
    state: &mut CheckerWatchState,
    beat: Option<CheckerBeat>,
    now_secs: u64,
) -> Option<CheckerStall> {
    let beat = beat?;
    let CheckerVerdict::Stalled { for_secs, phase } = checker_watch::classify(&beat, now_secs)
    else {
        state.suspect = None;
        return None;
    };
    if state.acted_on.is_some_and(|acted| acted.suppresses(beat)) {
        return None;
    }
    // A failed thread creation or observed exit is absence, not a heartbeat that
    // may merely be late. Its retry backoff already elapsed in `classify`. Returned
    // capacity rearms a previously confirmed stamp, never a different heartbeat.
    match state.suspect {
        _ if phase.worker_absent() || state.acted_on.is_some_and(|acted| acted.matches(beat)) => {}
        Some(suspect) if suspect.is(&beat) && now_secs >= suspect.confirm_at => {}
        Some(suspect) if suspect.is(&beat) => return None,
        _ => {
            state.suspect = Some(Suspect {
                generation: beat.generation,
                phase: beat.phase,
                at_secs: beat.at_secs,
                confirm_at: now_secs.saturating_add(checker_watch::STALL_CONFIRM.as_secs()),
            });
            return None;
        }
    }
    state.suspect = None;
    state.acted_on = Some(ActedOn {
        generation: beat.generation,
        phase: beat.phase,
        at_secs: beat.at_secs,
        worker_epoch: beat.worker_epoch,
    });
    Some(CheckerStall {
        generation: beat.generation,
        for_secs,
        phase,
        holds_lane: beat.holds_lane(),
        checks: beat.checks,
    })
}

/// Running seconds from `now_secs` until the watchdog's next look has something to decide (pure).
pub(crate) fn next_look_in(
    state: &CheckerWatchState,
    beat: Option<CheckerBeat>,
    now_secs: u64,
) -> Option<u64> {
    let beat = beat?;
    let streak = (beat.deferrals > 0 || state.lock_held.is_some()).then_some(DEFERRAL_LOOK_SECS);
    let heartbeat = heartbeat_look_in(state, beat, now_secs);
    match (heartbeat, streak) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// [`next_look_in`]'s heartbeat half: the stamp's own schedule.
fn heartbeat_look_in(state: &CheckerWatchState, beat: CheckerBeat, now_secs: u64) -> Option<u64> {
    match checker_watch::classify(&beat, now_secs) {
        CheckerVerdict::Fresh { stale_in_secs } => Some(stale_in_secs),
        CheckerVerdict::Stalled { .. }
            if state.acted_on.is_some_and(|acted| acted.suppresses(beat)) =>
        {
            None
        }
        CheckerVerdict::Stalled { .. } => Some(
            state
                .suspect
                .filter(|suspect| suspect.is(&beat))
                .map_or(0, |suspect| suspect.confirm_at.saturating_sub(now_secs)),
        ),
    }
}

/// The checker's tokens on `aterm ctl update status`, each present only in the state it names.
pub(crate) fn checker_status_tokens(beat: Option<CheckerBeat>, now_secs: u64) -> String {
    let Some(beat) = beat else {
        return String::new();
    };
    let mut out = String::new();
    if let Some((for_secs, phase)) = checker_watch::standing_stall(&beat, now_secs) {
        out.push_str(&format!(" checker_stalled={for_secs}:{}", phase.as_str()));
    }
    if beat.respawns > 0 {
        out.push_str(&format!(" checker_respawns={}", beat.respawns));
    }
    if beat.deferrals > 0 {
        out.push_str(&format!(" checker_deferred={}", beat.deferrals));
    }
    out
}

impl App {
    /// The watchdog's look, on the way into every park; it also runs an update lane's start that
    /// got no thread to wait on its launch setting, once that setting answers.
    pub(crate) fn watch_update_checker(&mut self) {
        aterm_update::start_owed_launch_waits();
        self.look_at_update_checker(checker_watch::checker_snapshot(), checker_watch::now_secs());
    }

    /// One look at `beat`: heal this watchdog's warning once a check has completed since it.
    pub(crate) fn look_at_update_checker(&mut self, beat: Option<CheckerBeat>, now_secs: u64) {
        if let Some(at) = self.update_checker_watch.announced_at_checks
            && beat.is_some_and(|beat| beat.checks > at)
        {
            self.update_checker_watch.announced_at_checks = None;
            self.heal_update_checker_stall();
        }
        match lock_held_look(&mut self.update_checker_watch, beat) {
            Some(LockHeld::Announce(cycles)) => self.announce_checker_lock_held(cycles),
            Some(LockHeld::Heal) => self.heal_checker_lock_held(),
            None => {}
        }
        let Some(stall) = judge(&mut self.update_checker_watch, beat, now_secs) else {
            return;
        };
        let replacement = aterm_update::respawn_stalled_checker(stall.generation);
        self.announce_update_checker_stall(stall, replacement);
    }

    /// Say a stall: the log line.
    pub(crate) fn announce_update_checker_stall(
        &mut self,
        stall: CheckerStall,
        replacement: Option<u64>,
    ) {
        match replacement {
            Some(next) if stall.holds_lane => aterm_log::warn!(
                "update checker stalled: generation {} has not stamped its heartbeat for {} s \
                 (last in {}) and still holds the check lane; started generation {next}, which \
                 cannot check until the stuck one lets go",
                stall.generation,
                stall.for_secs,
                stall.phase.as_str()
            ),
            Some(next) => aterm_log::warn!(
                "update checker stalled: generation {} has not stamped its heartbeat for {} s \
                 (last in {}); started generation {next} in its place",
                stall.generation,
                stall.for_secs,
                stall.phase.as_str()
            ),
            None => {
                let workers = checker_watch::checker_snapshot().map_or(0, |beat| beat.workers);
                let budget = if workers as u64 > checker_watch::MAX_RESPAWNS {
                    format!(" (this process has {workers} outstanding checker workers, the limit)",)
                } else {
                    String::new()
                };
                aterm_log::warn!(
                    "update checker stalled: generation {} has not stamped its heartbeat for {} \
                     s (last in {}); no replacement was started{budget}",
                    stall.generation,
                    stall.for_secs,
                    stall.phase.as_str()
                )
            }
        }
        let title = crate::update_words::CHECKER_STALLED_TITLE;
        let body = crate::update_words::checker_stalled_body(
            stall.for_secs,
            stall.phase.as_str(),
            replacement.is_some(),
            stall.holds_lane,
        );
        let announced =
            self.note_update_health_as(crate::update_words::HealthKind::Stalled, title, &body);
        if announced {
            self.update_checker_watch.announced_at_checks = Some(stall.checks);
        }
    }

    /// Say that a sibling aterm holds the machine's update check without making progress.
    pub(crate) fn announce_checker_lock_held(&mut self, cycles: u64) {
        let title = crate::update_words::CHECKER_LOCK_HELD_TITLE;
        let body = crate::update_words::checker_lock_held_body(cycles);
        self.note_update_health_as(crate::update_words::HealthKind::LockHeld, title, &body);
    }

    /// Heal the lock-held warning once the deferral streak ends.
    pub(crate) fn heal_checker_lock_held(&mut self) {
        let held = crate::update_words::HealthKind::LockHeld;
        aterm_log::info!(
            "update checker: the checker-lock streak ended; the lock-held warning is healed"
        );
        self.heal_update_health_where(|kind| kind == held);
    }

    /// The stall is over: a check completed after this watchdog's warning was posted.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "native_update_carried_health",
            action = "StallHeals",
            project = "aterm_gui::messages_host::carried_health_conformance::project"
        )
    )]
    pub(crate) fn heal_update_checker_stall(&mut self) {
        let stalled = crate::update_words::HealthKind::Stalled;
        aterm_log::info!(
            "update checker: a check completed after the stall; the warning is healed"
        );
        self.heal_update_health_where(|kind| kind == stalled);
    }

    /// The one wake the watchdog owes the event loop: its next look, or sooner the next try of
    /// an update lane's start that got no thread to wait on its launch setting
    /// ([`aterm_update::owed_launch_starts_due`]).
    pub(crate) fn update_checker_deadline(&self) -> Option<std::time::Instant> {
        let now = checker_watch::now_secs();
        next_look_in(
            &self.update_checker_watch,
            checker_watch::checker_snapshot(),
            now,
        )
        .map(|secs| checker_watch::instant_at(now.saturating_add(secs)))
        .into_iter()
        .chain(aterm_update::owed_launch_starts_due())
        .min()
    }
}

#[cfg(test)]
#[path = "update_checker_watch_tests.rs"]
mod tests;
