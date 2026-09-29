// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE UPDATE CHECKER'S WATCHDOG, on the window's event loop (the 2026-09-22/23
//! update audit, plan P2-1).
//!
//! The background check loop (`aterm_update`'s `aterm-update` thread) once went
//! silent for 4.6 days in a live window: v0.74.0 and v0.75.0 were published inside
//! that silence and never staged, and nothing in the process noticed — the one
//! trace was `stale_check=` on `aterm ctl update status`, four hours in, for
//! whoever thought to ask. The loop now stamps a heartbeat
//! (`aterm_update::checker_watch`), and this module is the reader that acts on it:
//! when the stamp has gone stale for its phase — and is still the same stale stamp
//! one [`checker_watch::STALL_CONFIRM`] later — the window
//!
//! * logs it (WARN, with the generation, the phase and how long),
//! * raises the update-health warning through main's messages system
//!   ([`App::note_update_health`], under its own title,
//!   [`crate::update_words::CHECKER_STALLED_TITLE`], with the OS banner an
//!   announcement owes) — and heals it the first time a check completes after it,
//! * starts a replacement checker under a new generation
//!   (`aterm_update::respawn_stalled_checker`), which retires the stalled thread
//!   the moment it wakes, and
//! * leaves `checker_stalled=<secs>:<phase>` on `aterm ctl update status`
//!   ([`checker_status_tokens`]) for as long as the stall stands — including while
//!   the retired generation still holds the check lane and its replacement cannot
//!   check ([`checker_watch::standing_stall`]).
//!
//! NO NEW THREAD AND NO POLLING. The look runs on the way into every park
//! (`about_to_wait`), where it costs a few atomic loads, and the event loop folds
//! ONE wake ([`App::update_checker_deadline`], `DeadlineOwner::UpdateChecker`) at
//! the next instant the look has something to decide: when the stamp would go
//! stale, when a suspected stall is due its second look, or at once when a stale
//! stamp has not been looked at yet — so an idle window still looks exactly when
//! a stall becomes one, and a healthy loop (which re-stamps long before that
//! instant) costs a wake about once an hour.

use aterm_update::checker_watch::{self, CheckerBeat, CheckerPhase, CheckerVerdict};

use crate::App;

/// What the watchdog remembers between looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CheckerWatchState {
    /// The generation whose stall was already acted on — a stall is acted on once,
    /// and the next look at the same stalled generation (a replacement that could
    /// not be started leaves it live) is quiet.
    acted_on: Option<u64>,
    /// A stale stamp seen by one look and not acted on yet: acted on only if it is
    /// still the same stamp at its second look ([`checker_watch::STALL_CONFIRM`]).
    suspect: Option<Suspect>,
    /// The loop's completed-check count when this watchdog's warning was posted:
    /// the first check completed after it heals the warning. `None` while no
    /// warning of this watchdog's is up.
    announced_at_checks: Option<u64>,
    /// The warning that a sibling holds `checker.lock` without making progress
    /// is up ([`lock_held_look`]): healed when the deferral streak ends, never
    /// by a check — the loop checks without the lock while it stands.
    lock_held: Option<LockHeldSaid>,
}

/// The streak the lock-held warning stands for ([`lock_held_look`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LockHeldSaid {
    /// The loop generation whose deferrals the warning follows.
    generation: u64,
    /// The loop's completed-check count the last time that generation was seen:
    /// a replacement generation proves the streak over only by a check completed
    /// past it.
    checks: u64,
}

/// What one look at the deferral streak asks the window to do
/// ([`lock_held_look`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LockHeld {
    /// The streak reached `CHECKER_UNGATED_AFTER`: say it, with the count.
    Announce(u64),
    /// The streak the warning said is over (a cycle got the lock): heal it.
    Heal,
}

/// A STOPPED SIBLING HOLDING `checker.lock` (round four of the 2026-09 update
/// robustness work, plan item 14), judged at one look (pure). The check loop
/// publishes its consecutive deferrals to a holder showing NO progress in the
/// heartbeat (a holder still checking rewrites the lock as it works, a slow
/// download included, and is never counted — the round-four review) and, past
/// [`checker_watch::CHECKER_UNGATED_AFTER`] of them, checks without the lock:
/// that much the loop handles by itself, and this says it, once per streak — the
/// holder is another aterm that is stopped or hung, and before round four every
/// other aterm on the machine waited on it for good with no notice. The warning
/// heals when the streak ends; a check never heals it (the loop checks without
/// the lock while the holder stands).
///
/// A ZERO FROM A REPLACEMENT IS NOT THE STREAK'S END (round six, finding 9). The
/// deferrals are published per generation, and the watchdog's replacement of a
/// stalled generation starts them at zero — the sibling may hold the lock still,
/// and the stuck generation the lane. Only the generation that deferred can end
/// its streak with a zero (it got the lock, or saw its holder working); a later
/// generation proves it by a check completed without deferring — the loop's
/// check count moving past where the streak's generation left it — and one that
/// defers in turn takes the streak over.
pub(crate) fn lock_held_look(
    state: &mut CheckerWatchState,
    beat: Option<CheckerBeat>,
) -> Option<LockHeld> {
    let deferrals = beat.map_or(0, |beat| beat.deferrals);
    let said = state.lock_held;
    if let (Some(said), Some(beat)) = (said, beat)
        && (beat.generation == said.generation || deferrals > 0)
    {
        // The streak's own generation (or one that took it over by deferring
        // in turn): follow where its check count stands.
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
        && beat.is_none_or(|beat| beat.generation == said.generation || beat.checks > said.checks);
    if ended {
        state.lock_held = None;
        return Some(LockHeld::Heal);
    }
    None
}

/// How soon the watchdog looks again while a deferral streak stands, in running
/// seconds. The streak moves once per check cycle, on the checker thread, with no
/// wake of the window's own; this one short look is the only way an idle window
/// sees it cross the bound, or end. Only while a streak stands: a healthy loop
/// still costs about one wake an hour.
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
    /// Whether it holds the check lane — it stalled in its lock wait or inside
    /// its check — so no replacement can check until it lets go.
    pub(crate) holds_lane: bool,
    /// The loop's completed-check count at the stall.
    pub(crate) checks: u64,
}

/// Judge one look (pure: the event loop and the tests ask the same question).
/// `Some` exactly once per stalled generation, and only for a stale stamp that
/// was ALREADY stale, unchanged, one [`checker_watch::STALL_CONFIRM`] earlier;
/// `None` for a fresh beat, for no checker at all (headless, automatic checks off,
/// an uninstalled copy), for a stale stamp seen for the first time (its second
/// look is armed), and for a stall this watchdog has already acted on.
///
/// WHY TWO LOOKS: the clock leaves the machine's sleep out but not a stop of this
/// process (`SIGSTOP`, a debugger pause), and the first look after the process is
/// continued sees every stamp as old as the stop — while the checker, stopped with
/// it, has had no chance to stamp. It stamps within the confirmation window; a
/// stuck one does not.
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
    if state.acted_on == Some(beat.generation) {
        return None;
    }
    match state.suspect {
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
    state.acted_on = Some(beat.generation);
    Some(CheckerStall {
        generation: beat.generation,
        for_secs,
        phase,
        holds_lane: beat.holds_lane(),
        checks: beat.checks,
    })
}

/// Running seconds from `now_secs` until the watchdog's next look has something to
/// decide (pure): the stamp going stale, a suspected stall's second look, or — `0`
/// — a stale stamp no look has seen yet. `None` with no checker, and while a stall
/// that was acted on stands (a past deadline would only spin the loop).
///
/// Derived from the SAME state [`judge`] keeps, never from the beat alone: the
/// look and this fold run hundreds of lines apart in one `about_to_wait`, and a
/// stamp that crossed its threshold between them used to fold no wake at all (the
/// beat was stale, so "the watchdog has acted on it") — an idle window then parked
/// with the stall unacted on until something else woke it.
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
        CheckerVerdict::Stalled { .. } if state.acted_on == Some(beat.generation) => None,
        CheckerVerdict::Stalled { .. } => Some(
            state
                .suspect
                .filter(|suspect| suspect.is(&beat))
                .map_or(0, |suspect| suspect.confirm_at.saturating_sub(now_secs)),
        ),
    }
}

/// The checker's tokens on `aterm ctl update status`, each present only in the
/// state it names, so a healthy line stays byte-identical (the `stale_check=` /
/// `installable=` precedent):
///
/// * `checker_stalled=<secs>:<phase>` — the loop's heartbeat is stale right now,
///   or a retired generation that stalled holding the check lane still holds it
///   (its replacement cannot check): running seconds since the stuck generation's
///   last stamp, and the phase it stamped ([`checker_watch::standing_stall`]);
/// * `checker_respawns=<n>` — replacement checkers this process has started;
/// * `checker_deferred=<n>` — consecutive cycles that found `checker.lock` held
///   past its bound by another aterm showing no progress, and deferred to it (a
///   holder still checking rewrites the lock as it works and is not counted).
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
    /// The watchdog's look, on the way into every park: this process's checker
    /// heartbeat, judged ([`Self::look_at_update_checker`]).
    pub(crate) fn watch_update_checker(&mut self) {
        self.look_at_update_checker(checker_watch::checker_snapshot(), checker_watch::now_secs());
    }

    /// One look at `beat`: heal this watchdog's warning once a check has completed
    /// since it was posted, and act on a confirmed stall once.
    pub(crate) fn look_at_update_checker(&mut self, beat: Option<CheckerBeat>, now_secs: u64) {
        if let Some(at) = self.update_checker_watch.announced_at_checks
            && beat.is_some_and(|beat| beat.checks > at)
        {
            self.update_checker_watch.announced_at_checks = None;
            self.heal_update_checker_stall();
        }
        match lock_held_look(&mut self.update_checker_watch, beat) {
            Some(LockHeld::Announce(cycles)) => self.announce_checker_lock_held(cycles),
            // Only this warning's kind: a heartbeat stall's warning is healed by
            // its own completed check.
            Some(LockHeld::Heal) => self.heal_checker_lock_held(),
            None => {}
        }
        let Some(stall) = judge(&mut self.update_checker_watch, beat, now_secs) else {
            return;
        };
        let replacement = aterm_update::respawn_stalled_checker(stall.generation);
        self.announce_update_checker_stall(stall, replacement);
    }

    /// Say a stall: the log line, and the update-health warning through the one
    /// door every producer shares ([`App::note_update_health`], which latches a
    /// title once per launch) under this watchdog's own title, with the OS banner
    /// an announcement owes.
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
            None => aterm_log::warn!(
                "update checker stalled: generation {} has not stamped its heartbeat for {} s \
                 (last in {}); no replacement was started (this process has started {} of at \
                 most {})",
                stall.generation,
                stall.for_secs,
                stall.phase.as_str(),
                checker_watch::checker_snapshot().map_or(0, |beat| beat.respawns),
                checker_watch::MAX_RESPAWNS
            ),
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
        // The OS notification a health announcement owes, through the one door
        // `Wake::UpdateHealth` and the automatic lane's convergence share (which
        // holds `desktop_alerts`, and never posts from a unit test).
        if announced {
            self.post_update_health_banner(title, &body);
        }
    }

    /// Say that a sibling aterm holds the machine's update check without making
    /// progress ([`lock_held_look`]): the update-health warning under this
    /// watchdog's title, through the one door every producer shares, with the OS
    /// banner an announcement owes. The log already named the holder's pid (the
    /// check loop's line).
    ///
    /// Under its own kind ([`crate::update_words::HealthKind::LockHeld`], round
    /// six): a heartbeat stall's warning standing beside it neither swallows it
    /// nor heals it.
    pub(crate) fn announce_checker_lock_held(&mut self, cycles: u64) {
        let title = crate::update_words::CHECKER_STALLED_TITLE;
        let body = crate::update_words::checker_lock_held_body(cycles);
        let announced =
            self.note_update_health_as(crate::update_words::HealthKind::LockHeld, title, &body);
        if announced {
            self.post_update_health_banner(title, &body);
        }
    }

    /// THE SIBLING LET GO: the deferral streak the lock-held warning stood for
    /// is over ([`lock_held_look`]). Its latch, its row and the record of its
    /// healing go; a heartbeat stall's warning stays for its own proof.
    pub(crate) fn heal_checker_lock_held(&mut self) {
        let held = crate::update_words::HealthKind::LockHeld;
        aterm_log::info!(
            "update checker: the checker-lock streak ended; the lock-held warning is healed"
        );
        self.update_health_latched.retain(|kind| *kind != held);
        for id in self.live_update_health(|kind| kind == held) {
            self.resolve_message(id, aterm_messages::Outcome::Warn);
        }
        self.record_update_health_healed(|kind| kind == held);
    }

    /// THE STALL IS OVER: a check completed after this watchdog's warning was
    /// posted. The warning — its latch, its row if it is still up, and the record
    /// of its healing — goes, and nothing else does: a ledger warning under
    /// another title is the ledger's to heal.
    pub(crate) fn heal_update_checker_stall(&mut self) {
        let stalled = crate::update_words::HealthKind::Stalled;
        aterm_log::info!(
            "update checker: a check completed after the stall; the warning is healed"
        );
        self.update_health_latched.retain(|kind| *kind != stalled);
        for id in self.live_update_health(|kind| kind == stalled) {
            self.resolve_message(id, aterm_messages::Outcome::Warn);
        }
        self.record_update_health_healed(|kind| kind == stalled);
    }

    /// The one wake the watchdog owes the event loop ([`next_look_in`], over the
    /// state the look just left): `None` with no checker, and while a stall that
    /// was acted on stands.
    pub(crate) fn update_checker_deadline(&self) -> Option<std::time::Instant> {
        let now = checker_watch::now_secs();
        next_look_in(
            &self.update_checker_watch,
            checker_watch::checker_snapshot(),
            now,
        )
        .map(|secs| checker_watch::instant_at(now.saturating_add(secs)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update_words::LiveHealth as _;
    use aterm_update::checker_watch::{
        CHECK_PHASE_BUDGET, CHECKER_LOCK_WAIT, LaneHolder, STALL_CONFIRM, STALL_SLACK,
        stall_threshold_secs,
    };

    fn beat(generation: u64, phase: CheckerPhase, at: u64) -> CheckerBeat {
        CheckerBeat {
            generation,
            phase,
            at_secs: at,
            max_interval_secs: 1_100,
            respawns: 0,
            deferrals: 0,
            settings_misses: 0,
            lane: None,
            checks: 0,
        }
    }

    const CONFIRM: u64 = STALL_CONFIRM.as_secs();

    /// Judge `beat` twice, one confirmation apart: the second look's verdict.
    fn confirmed(
        state: &mut CheckerWatchState,
        beat: CheckerBeat,
        now: u64,
    ) -> Option<CheckerStall> {
        assert_eq!(
            judge(state, Some(beat), now),
            None,
            "the first look only suspects"
        );
        judge(state, Some(beat), now + CONFIRM)
    }

    /// A STALL IS ACTED ON ONCE PER GENERATION, AND ONLY A STALL. The code before
    /// this change had no look at all: the stall below (a checker parked in the
    /// lock wait for 4.6 days) produced nothing until someone read `update
    /// status`, four hours in.
    #[test]
    fn a_stall_is_acted_on_once_per_generation_and_a_fresh_beat_never() {
        let mut state = CheckerWatchState::default();
        assert_eq!(
            judge(&mut state, None, 1_000_000),
            None,
            "no checker, no look"
        );
        let fresh = beat(1, CheckerPhase::Waiting, 1_000);
        assert_eq!(judge(&mut state, Some(fresh), 1_000 + 60), None);
        let parked = beat(1, CheckerPhase::LockWait, 1_000);
        let days = 1_000 + 4 * 86_400;
        assert_eq!(
            confirmed(&mut state, parked, days),
            Some(CheckerStall {
                generation: 1,
                for_secs: 4 * 86_400 + CONFIRM,
                phase: CheckerPhase::LockWait,
                holds_lane: false,
                checks: 0,
            })
        );
        assert_eq!(
            judge(&mut state, Some(parked), days + CONFIRM + 60),
            None,
            "the same stall is acted on once"
        );
        // Its replacement stalling too is a new stall.
        let replacement = beat(2, CheckerPhase::Checking, days);
        let later = days + stall_threshold_secs(CheckerPhase::Checking, 1_100) + 1;
        assert!(confirmed(&mut state, replacement, later).is_some());
    }

    /// A PROCESS THAT WAS STOPPED IS NOT A STALLED CHECKER. The clock runs on while
    /// the process is `SIGSTOP`ped or paused in a debugger, so the first look after
    /// it is continued sees a stale stamp, and the checker, stopped too, has not
    /// had a chance to stamp. Before this change that first look acted — a WARN,
    /// the health warning with an OS banner, and one of the three lifetime
    /// replacements spent on a healthy loop (the first assertion fails against
    /// it). Now it only suspects; the checker stamps; nothing happens.
    #[test]
    fn a_stamp_that_moves_before_the_second_look_is_no_stall() {
        let mut state = CheckerWatchState::default();
        let threshold = stall_threshold_secs(CheckerPhase::Waiting, 1_100);
        let before_the_stop = beat(1, CheckerPhase::Waiting, 1_000);
        let continued = 1_000 + threshold + 3_600;
        assert_eq!(
            judge(&mut state, Some(before_the_stop), continued),
            None,
            "the first look after the process was continued only suspects"
        );
        assert_eq!(
            next_look_in(&state, Some(before_the_stop), continued),
            Some(CONFIRM),
            "and folds its second look"
        );
        assert_eq!(
            judge(&mut state, Some(before_the_stop), continued + CONFIRM - 1),
            None,
            "not before it is due"
        );
        // The continued checker stamps (settle, then its next cycle).
        let restamped = beat(1, CheckerPhase::Settings, continued + 25);
        assert_eq!(
            judge(&mut state, Some(restamped), continued + CONFIRM),
            None
        );
        assert_eq!(state.suspect, None, "the suspicion is dropped");
        assert_eq!(state.acted_on, None);
        // A stale stamp that did NOT move by its second look is acted on.
        let stuck = beat(1, CheckerPhase::Checking, 0);
        let stale = stall_threshold_secs(CheckerPhase::Checking, 1_100) + 1;
        assert!(confirmed(&mut state, stuck, stale).is_some());
    }

    /// THE WAKE IS FOLDED FROM THE LOOK'S OWN STATE. The look and the deadline
    /// fold run far apart in one `about_to_wait`; a stamp that went stale between
    /// them used to fold NO wake (the old deadline read "stale" as "already acted
    /// on"), and an idle window then parked with the stall unseen. A stale stamp no
    /// look has seen now folds an immediate wake, a suspicion its second look, and
    /// only a stall that was acted on folds nothing.
    #[test]
    fn a_stamp_gone_stale_between_the_look_and_the_fold_still_folds_a_wake() {
        let mut state = CheckerWatchState::default();
        let threshold = stall_threshold_secs(CheckerPhase::Waiting, 1_100);
        let b = beat(1, CheckerPhase::Waiting, 0);
        // The look, one second before the threshold: fresh, nothing to do.
        assert_eq!(judge(&mut state, Some(b), threshold), None);
        // The fold, a moment later, after the stamp crossed it.
        assert_eq!(
            next_look_in(&state, Some(b), threshold + 1),
            Some(0),
            "a look is owed now"
        );
        assert_eq!(next_look_in(&state, None, 10), None, "no checker, no wake");
        assert_eq!(
            next_look_in(&state, Some(b), 10),
            Some(threshold - 10 + 1),
            "a fresh stamp folds the instant it would go stale"
        );
        let stall = confirmed(&mut state, b, threshold + 1);
        assert!(stall.is_some());
        assert_eq!(
            next_look_in(&state, Some(b), threshold + 1 + CONFIRM),
            None,
            "a stall acted on folds nothing"
        );
    }

    /// The stall threshold is the phase's: a lock wait is called out in minutes,
    /// a check only after twice its whole budget.
    #[test]
    fn a_lock_wait_stalls_in_minutes_and_a_check_only_past_its_budget() {
        let mut state = CheckerWatchState::default();
        let lock = 2 * CHECKER_LOCK_WAIT.as_secs() + STALL_SLACK.as_secs();
        assert!(judge(&mut state, Some(beat(1, CheckerPhase::LockWait, 0)), lock).is_none());
        assert!(state.suspect.is_none(), "not stale yet");
        assert!(confirmed(&mut state, beat(1, CheckerPhase::LockWait, 0), lock + 1).is_some());
        let mut state = CheckerWatchState::default();
        assert!(
            judge(
                &mut state,
                Some(beat(1, CheckerPhase::Checking, 0)),
                2 * CHECK_PHASE_BUDGET.as_secs()
            )
            .is_none()
                && state.suspect.is_none(),
            "a long check is not a stall"
        );
    }

    /// The status tokens appear only in the state they name — and a stall inside
    /// the lane stays named while a replacement is blocked behind it.
    #[test]
    fn the_status_tokens_are_absent_on_a_healthy_checker() {
        assert_eq!(checker_status_tokens(None, 10), "");
        assert_eq!(
            checker_status_tokens(Some(beat(1, CheckerPhase::Waiting, 100)), 160),
            "",
            "a healthy line stays byte-identical"
        );
        let mut stalled = beat(2, CheckerPhase::LockWait, 0);
        stalled.respawns = 1;
        stalled.deferrals = 7;
        assert_eq!(
            checker_status_tokens(Some(stalled), 3_600),
            " checker_stalled=3600:lock-wait checker_respawns=1 checker_deferred=7"
        );
        // Generation 2 is fresh, but generation 1 stalled in its check and still
        // holds the lane: before this change the line read healthy but for the
        // respawn count.
        let mut blocked = beat(2, CheckerPhase::Waiting, 5_000);
        blocked.respawns = 1;
        blocked.lane = Some(LaneHolder {
            generation: 1,
            phase: CheckerPhase::Checking,
            at_secs: 1_000,
        });
        assert_eq!(
            checker_status_tokens(Some(blocked), 5_060),
            " checker_stalled=4060:checking checker_respawns=1"
        );
    }

    /// THE WARNING, THROUGH MAIN'S ONE DOOR, UNDER ITS OWN TITLE. A stall posts
    /// the stall warning once per launch with the stall's own sentence — and the
    /// ledger's "can't check for updates" is NOT latched by it: before this change
    /// the stall borrowed that title, so a real network or manifest failure later
    /// in the launch was "already said" (the ledger assertion below fails against
    /// it).
    #[test]
    fn a_stall_raises_its_own_warning_once_and_never_silences_the_ledgers() {
        let mut app = App::headless_for_test();
        let stall = CheckerStall {
            generation: 1,
            for_secs: 3_000,
            phase: CheckerPhase::LockWait,
            holds_lane: false,
            checks: 0,
        };
        app.announce_update_checker_stall(stall, Some(2));
        let live = app
            .messages
            .live_health()
            .expect("the health warning is up");
        assert_eq!(live.msg.title, crate::update_words::CHECKER_STALLED_TITLE);
        let words = live.msg.detail.join(" ");
        assert!(
            words.contains("waiting for another aterm's check to finish")
                && words.contains("started a fresh one in its place"),
            "{words}"
        );
        let before = app.messages.live_health().map(|l| l.id);
        app.announce_update_checker_stall(
            CheckerStall {
                generation: 2,
                ..stall
            },
            None,
        );
        assert_eq!(
            app.messages.live_health().map(|l| l.id),
            before,
            "announced once per launch"
        );
        assert!(
            app.note_update_health(
                aterm_update::health_failing_title("network"),
                "update checks have failed 5 times in a row"
            ),
            "the ledger's own warning is still owed its row"
        );
    }

    /// `desktop_alerts = false` (the default since 2026-09-28) withholds the
    /// update-health DESKTOP BANNER and nothing else: the stall's ⚠ row is on
    /// the band either way. NEGATIVE CONTROL: with `desktop_alerts = true` the
    /// same announcement records exactly one banner, in the banner's words —
    /// so the silence is the setting's, not a missing announcement.
    #[test]
    fn desktop_alerts_off_withholds_only_the_update_health_banner() {
        use crate::messages_host::UPDATE_HEALTH_BANNERS;
        let take = || UPDATE_HEALTH_BANNERS.with(|b| std::mem::take(&mut *b.borrow_mut()));
        let stall = CheckerStall {
            generation: 1,
            for_secs: 3_000,
            phase: CheckerPhase::LockWait,
            holds_lane: false,
            checks: 0,
        };
        for (alerts, banners) in [(None, 0), (Some(false), 0), (Some(true), 1)] {
            let mut app = App::headless_for_test();
            app.config.desktop_alerts = alerts;
            let _ = take();
            app.announce_update_checker_stall(stall, Some(2));
            assert!(
                app.messages.live_health().is_some(),
                "{alerts:?}: the band row is up either way"
            );
            let posted = take();
            assert_eq!(posted.len(), banners, "{alerts:?}: {posted:?}");
            if let Some((title, _)) = posted.first() {
                assert_eq!(title, crate::update_words::CHECKER_STALLED_TITLE);
            }
            // The checker-lock warning shares the door under its own kind: the
            // same setting decides its banner, and once said it is latched.
            app.announce_checker_lock_held(9);
            assert_eq!(take().len(), banners, "{alerts:?}: its own announcement");
            app.announce_checker_lock_held(10);
            assert!(take().is_empty(), "{alerts:?}: latched, no second banner");
        }
    }

    /// A STALL INSIDE THE LANE IS SAID AS ONE: the fresh checker cannot check
    /// until the stuck one lets go, and the sentence must not claim the remedy has
    /// already happened.
    #[test]
    fn a_stall_holding_the_lane_does_not_claim_its_replacement_fixed_it() {
        let mut app = App::headless_for_test();
        app.announce_update_checker_stall(
            CheckerStall {
                generation: 1,
                for_secs: 4_000,
                phase: CheckerPhase::Checking,
                holds_lane: true,
                checks: 3,
            },
            Some(2),
        );
        let words = app
            .messages
            .live_health()
            .expect("the warning is up")
            .msg
            .detail
            .join(" ");
        assert!(
            words.contains("cannot check until the stuck one finishes")
                && !words.contains("in its place"),
            "{words}"
        );
    }

    /// THE WARNING HEALS ON THE FIRST CHECK COMPLETED AFTER IT. Before this change
    /// nothing a replacement's ordinary checks produced healed it: the latch and
    /// the row stood for the rest of the launch.
    #[test]
    fn the_stall_warning_heals_when_a_check_completes_after_it() {
        let mut app = App::headless_for_test();
        let mut stuck = beat(1, CheckerPhase::Settings, 0);
        stuck.checks = 4;
        let stale = stall_threshold_secs(CheckerPhase::Settings, 1_100) + 1;
        app.look_at_update_checker(Some(stuck), stale);
        assert!(
            app.messages.live_health().is_none(),
            "one stale look only suspects"
        );
        app.look_at_update_checker(Some(stuck), stale + CONFIRM);
        assert!(
            app.messages
                .live_health()
                .is_some_and(|live| live.msg.title == crate::update_words::CHECKER_STALLED_TITLE),
            "the confirmed stall is announced"
        );
        // Its replacement stamps and waits, but has completed no check yet.
        let mut replacement = beat(2, CheckerPhase::Waiting, stale + CONFIRM + 10);
        replacement.checks = 4;
        replacement.respawns = 1;
        app.look_at_update_checker(Some(replacement), stale + CONFIRM + 20);
        assert!(
            app.messages.live_health().is_some(),
            "no check has completed since: the warning stands"
        );
        replacement.checks = 5;
        app.look_at_update_checker(Some(replacement), stale + CONFIRM + 700);
        assert!(
            app.messages.live_health().is_none(),
            "a completed check healed it"
        );
        assert!(
            !app.update_health_latched
                .contains(&crate::update_words::HealthKind::Stalled),
            "and its latch: a later stall is announced again"
        );
    }

    /// ROUND FOUR, PLAN ITEM 14: A SIBLING HOLDING `checker.lock` WITHOUT MAKING
    /// PROGRESS IS SAID, once per streak, when the loop's published deferrals
    /// reach `CHECKER_UNGATED_AFTER` — where the loop starts checking without
    /// the lock — and the warning heals when the streak ends. A check completing
    /// meanwhile (the loop's own, without the lock) does NOT heal it: the holder
    /// is still stuck. An idle window looks again within a minute while a streak
    /// stands. NEGATIVE CONTROLS: a streak below the bound says nothing, and a
    /// healthy beat folds no minute-look.
    ///
    /// RED before the change: nothing read `deferrals` but the status token, so
    /// the machine's update checks stopped behind a stopped sibling with no row
    /// — the look below left the messages empty at every count.
    #[test]
    fn a_sibling_holding_the_checker_lock_is_said_once_and_healed_when_it_lets_go() {
        use aterm_update::checker_watch::CHECKER_UNGATED_AFTER;
        let mut app = App::headless_for_test();
        let mut waiting = beat(1, CheckerPhase::Waiting, 100);
        let fresh = CheckerWatchState::default();
        assert!(
            next_look_in(&fresh, Some(waiting), 100).is_some_and(|secs| secs > DEFERRAL_LOOK_SECS),
            "a healthy beat folds no minute-look"
        );
        for deferrals in 1..CHECKER_UNGATED_AFTER {
            waiting.deferrals = deferrals;
            app.look_at_update_checker(Some(waiting), 110);
            assert!(
                app.messages.live_health().is_none(),
                "below the bound the sibling may just be checking: {deferrals}"
            );
            assert_eq!(
                next_look_in(&app.update_checker_watch, Some(waiting), 110),
                Some(DEFERRAL_LOOK_SECS),
                "a standing streak is looked at within a minute"
            );
        }
        waiting.deferrals = CHECKER_UNGATED_AFTER;
        app.look_at_update_checker(Some(waiting), 120);
        let live = app
            .messages
            .live_health()
            .expect("the stuck sibling is said");
        assert_eq!(live.msg.title, crate::update_words::CHECKER_STALLED_TITLE);
        assert_eq!(
            live.msg.detail.first().map(String::as_str),
            Some(crate::update_words::checker_lock_held_body(CHECKER_UNGATED_AFTER).as_str())
        );
        // The loop checks without the lock: its checks complete, the streak
        // stands, and the warning with it.
        waiting.deferrals += 1;
        waiting.checks += 2;
        app.look_at_update_checker(Some(waiting), 700);
        assert!(
            app.messages.live_health().is_some(),
            "a check without the lock does not heal a holder that is still stuck"
        );
        assert_eq!(
            app.messages
                .live_rows()
                .filter(|l| crate::update_words::HealthKind::of_row(&l.msg).is_some())
                .count(),
            1,
            "said once per streak"
        );
        // The holder lets go: a cycle got the lock, the streak is over.
        waiting.deferrals = 0;
        app.look_at_update_checker(Some(waiting), 1_300);
        assert!(app.messages.live_health().is_none(), "healed");
        assert!(
            !app.update_health_latched
                .contains(&crate::update_words::HealthKind::LockHeld),
            "and its latch: the next streak is said again"
        );
        waiting.deferrals = CHECKER_UNGATED_AFTER;
        app.look_at_update_checker(Some(waiting), 2_000);
        assert!(app.messages.live_health().is_some(), "a new streak is said");
    }

    /// The live rows standing for the lock-held warning, by its words.
    fn lock_held_rows(app: &App) -> usize {
        app.messages
            .live_rows()
            .filter(|l| crate::update_words::HealthKind::of_row(&l.msg).is_some())
            .filter(|l| {
                l.msg
                    .detail
                    .first()
                    .is_some_and(|d| d.contains("another aterm has held the update check's lock"))
            })
            .count()
    }

    /// Records of "aterm updates work again".
    fn healed_records(app: &App) -> usize {
        app.messages
            .log()
            .records()
            .filter(|r| r.title == "aterm updates work again")
            .count()
    }

    /// ROUND SIX, FINDING 9: A REPLACEMENT CHECKER STARTS AT ZERO DEFERRALS, AND
    /// THAT IS NO PROOF THE SIBLING LET GO. The lock-held warning was up; the loop's
    /// lock-free check then hung and the watchdog retired it, and the supersede
    /// zeroes the published deferrals. The next look read that zero as "the streak
    /// ended", healed the warning and put "aterm updates work again" on record —
    /// while the sibling still held the lock and the stuck generation still held the
    /// lane. NEGATIVE CONTROL: once the replacement completes a check without
    /// deferring, the streak really is over and it heals.
    #[test]
    fn a_replacement_at_zero_deferrals_is_no_proof_the_sibling_let_go() {
        use aterm_update::checker_watch::CHECKER_UNGATED_AFTER;
        let mut app = App::headless_for_test();
        let mut waiting = beat(1, CheckerPhase::Waiting, 100);
        waiting.deferrals = CHECKER_UNGATED_AFTER;
        app.look_at_update_checker(Some(waiting), 110);
        assert_eq!(lock_held_rows(&app), 1, "the stuck sibling is said");
        // The lock-free check hangs; the watchdog retires generation 1.
        app.announce_update_checker_stall(
            CheckerStall {
                generation: 1,
                for_secs: 3_000,
                phase: CheckerPhase::Checking,
                holds_lane: true,
                checks: 0,
            },
            Some(2),
        );
        let mut replacement = beat(2, CheckerPhase::Starting, 200);
        app.look_at_update_checker(Some(replacement), 260);
        assert_eq!(
            lock_held_rows(&app),
            1,
            "the sibling still holds the lock: the warning stands"
        );
        assert_eq!(
            healed_records(&app),
            0,
            "and nothing says updates work again"
        );
        // The replacement got the lock and completed a check: the streak is over.
        replacement.phase = CheckerPhase::Waiting;
        replacement.checks = 1;
        app.look_at_update_checker(Some(replacement), 320);
        assert_eq!(lock_held_rows(&app), 0, "healed once a check proves it");
    }

    /// ROUND SIX, FINDING 36: THE LOCK-HELD WARNING IS NEVER SWALLOWED BY A STALL
    /// WARNING. Both shared one latch: announced while a heartbeat stall's warning
    /// stood, the lock-held one was marked said, posted nothing, and was cleared
    /// with the stall's heal — then never said again while the sibling stayed stuck.
    #[test]
    fn a_stall_warning_up_first_does_not_swallow_the_lock_held_warning() {
        use aterm_update::checker_watch::CHECKER_UNGATED_AFTER;
        let mut app = App::headless_for_test();
        app.announce_update_checker_stall(
            CheckerStall {
                generation: 1,
                for_secs: 3_000,
                phase: CheckerPhase::Settings,
                holds_lane: false,
                checks: 4,
            },
            Some(2),
        );
        let mut b = beat(2, CheckerPhase::Waiting, 100);
        b.checks = 4;
        b.deferrals = CHECKER_UNGATED_AFTER;
        app.look_at_update_checker(Some(b), 110);
        b.checks = 5;
        b.deferrals += 1;
        app.look_at_update_checker(Some(b), 700);
        assert_eq!(
            lock_held_rows(&app),
            1,
            "the stuck sibling is still holding the lock and must be said"
        );
    }

    /// ROUND SIX, FINDING 53: A DOWNLOAD IS NO PROOF THE SIBLING LET GO. The
    /// lock-free check found a release and its bytes started arriving; the progress
    /// report healed every warning a check answers — the lock-held one included —
    /// and recorded "aterm updates work again" while the sibling was still stuck.
    #[test]
    fn a_download_does_not_heal_the_lock_held_warning() {
        use aterm_update::checker_watch::CHECKER_UNGATED_AFTER;
        let mut app = App::headless_for_test();
        let mut waiting = beat(1, CheckerPhase::Waiting, 100);
        waiting.deferrals = CHECKER_UNGATED_AFTER;
        app.look_at_update_checker(Some(waiting), 120);
        assert_eq!(lock_held_rows(&app), 1);
        app.note_update_progress(&aterm_update::Progress::Downloading {
            version: "9.9.9".into(),
            bytes_done: 1,
            bytes_total: 10,
        });
        assert_eq!(lock_held_rows(&app), 1, "the sibling is still stuck");
        assert_eq!(healed_records(&app), 0, "{:?}", app.messages.live_health());
    }

    /// The body says what is stuck and that updates go on, and asks nothing
    /// (grep_guard B12).
    #[test]
    fn the_lock_held_words_say_both_halves() {
        let words = crate::update_words::checker_lock_held_body(3);
        assert!(words.contains("another aterm has held the update check's lock"));
        assert!(words.contains("through the last 3 checks"));
        assert!(words.contains("checks for updates without waiting for it"));
        assert!(!words.to_lowercase().contains("restart"), "{words}");
    }

    /// A window with no checker (every headless App, every test binary) looks
    /// and folds nothing.
    #[test]
    fn no_checker_means_no_look_and_no_wake() {
        let mut app = App::headless_for_test();
        app.watch_update_checker();
        assert!(app.update_checker_deadline().is_none());
        assert!(app.messages.live_health().is_none());
    }
}
