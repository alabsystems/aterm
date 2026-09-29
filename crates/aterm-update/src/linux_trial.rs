// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! When one more start of a replaced Linux executable rolls it back, and which
//! kind of start may confirm it.
//!
//! A replaced executable (`crate::linux`) gets [`crate::LINUX_TRIAL_LAUNCHES`]
//! starts to prove itself healthy; the start after that restores the previous one
//! and refuses the new build for good. Until 2026-09-28 only a WINDOW counted or
//! confirmed a start, so a copy used only from the terminal installed one update,
//! never confirmed it, and stopped updating for good. Terminal sessions now count
//! and confirm too, and sessions start in BURSTS: a terminal that restores six
//! tabs, each running `aterm`, starts six copies inside a second, and every one of
//! them is counted before the first has printed a prompt and confirmed. Counting
//! alone would roll a healthy build back at the fourth, and a rolled-back build
//! never installs again.
//!
//! So a start over the budget rolls back only when the start before it is no
//! longer YOUNG: [`START_SETTLES_SECS`] old or more, long enough for any start to
//! have confirmed itself (a session confirms at its shell's first output, or after
//! [`crate::LINUX_SESSION_HEALTHY_AFTER`] at the latest; a window after its first
//! frame).
//!
//! AND THE WAIT IS BOUNDED ([`start_stamp`]). "The start before it" is the last
//! start WITHIN the budget: a start past the budget leaves the stamp where it was.
//! When every start restamped, a restarter faster than [`START_SETTLES_SECS`] — a
//! systemd unit with `Restart=always` and `RestartSec=5`, a shell loop around
//! `aterm --headless` — found every start young, so a build that crashed at every
//! start was never rolled back; and while its trial stood unconfirmed, update
//! checks and applies waited, so a fixed release could not install either. Now a
//! build that crashes at every start is rolled back at the first start at least
//! [`START_SETTLES_SECS`] after the budget's last one, however fast the restarts
//! come: the rule moves the verdict by at most that much, and never drops it.
//!
//! AND EACH LANE PROVES ITS OWN ([`Lanes`]). A terminal session's prompt confirms
//! the trial for update checks and applies, never for a window: a trial only a
//! session confirmed still counts its window starts until a window confirms it.
//! Otherwise `aterm` typed into a terminal to look into a window that crashes at
//! start would keep that build for good (the round-four review). And the window
//! lane's budget is spent by window starts alone: a session's confirmation starts
//! the count afresh, and is never refused (round six).
//!
//! Compiled on every target so these rules, their derived models and the binds
//! between them run on the macOS gate too; the transaction code in `crate::linux`
//! is the only shipping caller.

/// How long after a start it may still confirm itself: twice the longest a
/// session waits before confirming ([`crate::LINUX_SESSION_HEALTHY_AFTER`]), so a
/// start whose confirmation waits on the update lock or on hashing the executable
/// is still inside it.
pub(crate) const START_SETTLES_SECS: i64 = 2 * crate::LINUX_SESSION_HEALTHY_AFTER.as_secs() as i64;

/// Whether the start stamped at `previous_start_unix` may still confirm itself at
/// `now_unix`. `0` is "no start stamped": a record an older build wrote carries no
/// stamp, and that reads as today's rule, never as a start to wait for (the one
/// shipping caller first restamps a trial past its budget whose stamp an older
/// build's save dropped, [`stamp_lost`]). A stamp in
/// the future (the clock stepped back) is not young either, so a wrong clock can
/// delay no verdict.
pub(crate) fn start_is_young(previous_start_unix: i64, now_unix: i64) -> bool {
    previous_start_unix > 0
        && (0..START_SETTLES_SECS).contains(&now_unix.saturating_sub(previous_start_unix))
}

/// Whether the start just counted — `starts` includes it — rolls the trial back.
pub(crate) fn start_rolls_back(starts: u32, previous_start_unix: i64, now_unix: i64) -> bool {
    starts > crate::LINUX_TRIAL_LAUNCHES && !start_is_young(previous_start_unix, now_unix)
}

/// The stamp the start just counted — `starts` includes it — leaves for the next
/// start to be judged against ([`start_rolls_back`]): its own time while the trial
/// is within its budget, the stamp as it stood once the trial is past it.
///
/// Past the budget a start must not restamp: each restamp opened a fresh settle
/// window, so starts less than [`START_SETTLES_SECS`] apart deferred the verdict
/// for as long as they kept coming (see the module docs). The budget's starts each
/// keep their full window, which is what a burst of healthy session starts needs;
/// the starts after them share the last one's.
pub(crate) fn start_stamp(starts: u32, previous_start_unix: i64, now_unix: i64) -> i64 {
    if starts <= crate::LINUX_TRIAL_LAUNCHES {
        now_unix
    } else {
        previous_start_unix
    }
}

/// Whether the start just counted — `starts` includes it — finds the trial past its
/// budget with its stamp LOST: no start stamped, although the budget's starts each
/// stamped one ([`start_stamp`]). Only an older build's save does that — a running
/// 0.98 rewrites the whole record in its own shape, without `last_start_unix` — and
/// it can land in the middle of a burst of session starts, where the plain reading of
/// "no stamp" ([`start_is_young`]: the budget alone) rolled back, and refused for good,
/// a healthy build before its first prompt (round six).
///
/// Such a start — one of the first [`LOST_STAMP_SPARES`] past the budget — stamps its
/// own time and does not roll back; the next start past the budget is judged against
/// that stamp as usual. So one lost stamp costs a crash loop one start and at most one
/// settle window more; a stamp lost before EVERY start (a running 0.98 saving over each
/// one, below) costs it at most [`LOST_STAMP_SPARES`] starts, and the next rolls back.
/// The verdict is never dropped: the start that restamps is a start of a trial past its
/// budget, whose stamp then stays put ([`start_stamp`]). A record an older build wrote with no stamp WITHIN the
/// budget needs no such care: those starts stamp as they always do.
///
/// AND ONLY [`LOST_STAMP_SPARES`] STARTS PAST THE BUDGET ARE SPARED. The 0.98 that
/// installed the update keeps checking, and with a trial pending it saves the whole
/// record every half hour, dropping the stamp each time; `starts` survives those
/// saves. When every start past the budget that found no stamp restamped and was
/// spared, a build that crashed at every start, tried less often than 0.98 checks (a
/// window opened once an hour), found the stamp gone at each start and was never
/// rolled back, and while its trial stood unconfirmed no fixed release could install
/// either. Sparing only the FIRST start past the budget was no answer: sessions start
/// in bursts (six restored tabs in about a second), and a save landing between the
/// burst's fifth and sixth start left the sixth, past the first, reading "no stamp"
/// as the plain budget and rolling back a healthy build before its first prompt. So
/// a lost stamp is spared at any start from the first past the budget through
/// [`LOST_STAMP_SPARES`] past it — wherever in a burst of that many the save lands —
/// and a start after them that finds none reads "no stamp" as [`start_is_young`]
/// does, the plain budget, and rolls back: a crash loop 0.98 keeps saving over is
/// rolled back at the start [`LOST_STAMP_SPARES`] + 1 past the budget, at the latest.
pub(crate) fn stamp_lost(starts: u32, previous_start_unix: i64) -> bool {
    previous_start_unix == 0
        && (crate::LINUX_TRIAL_LAUNCHES + 1
            ..=crate::LINUX_TRIAL_LAUNCHES.saturating_add(LOST_STAMP_SPARES))
            .contains(&starts)
}

/// How many starts past the budget may each find the stamp lost and be spared
/// ([`stamp_lost`]): the longest burst of starts past the budget a lost stamp is
/// forgiven in, and so the most starts a crash loop an older build keeps saving over
/// costs before it is rolled back. A burst is the session starts one launch restores
/// (a window's restored tabs); eight past the budget covers eleven at once.
pub(crate) const LOST_STAMP_SPARES: u32 = 8;

/// Which kind of start of a replaced executable counts or confirms its trial. A
/// window and a terminal session run different code on the same file, so each
/// proves its own lane ([`Lanes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lane {
    /// The GUI entry with a window: `crate::linux::boot`, then
    /// `crate::linux::confirm` after its first frame.
    Window,
    /// An interactive terminal session (`crate::linux::session_started`, then
    /// `crate::linux::confirm_session` at its shell's first output), or a
    /// `--headless` instance, which opens no window and so proves what a session
    /// proves: that the file runs, not that its window opens.
    Session,
}

/// Where a trial's two lanes stand: the trial record's `healthy` and
/// `window_proven`.
///
/// WHY A SESSION DOES NOT VOUCH FOR A WINDOW (the round-four review). Once a
/// trial was confirmed its window starts stopped counting, so when a session
/// could confirm the whole file, `aterm` typed into a terminal to look into a
/// window that crashes at start (a GPU or Wayland regression) confirmed that
/// build for good: no rollback, and every window launch kept crashing until a
/// newer release shipped. So a session confirms the session lane only: window
/// starts go on counting, and roll the install back past the budget, until a
/// WINDOW confirms ([`Self::window_proven`]).
///
/// AND A SESSION IS NEVER REFUSED (round six). Round four also made a window
/// start that had not confirmed refuse every session's confirmation, while those
/// sessions' starts still counted against the one shared budget. One window start
/// that never reached a first frame — `aterm` run with no terminal on a box with
/// no display, a window killed at logout — then let three healthy terminal
/// sessions roll the build back and refuse it for good. A session's confirmation
/// now always confirms the session lane, and the count starts again at zero, so
/// the window lane's budget is spent by window starts alone.
///
/// AND AN OLDER BUILD MAY REWRITE THE RECORD. A running 0.98 saves the whole
/// state record with its own fields and none of these, so each is spelled so
/// that its missing value is the careful one: a trial whose `window_proven` was
/// dropped counts its window starts again, and a window that works confirms it
/// again ([`confirm`] starts the count afresh, so that recount never nears the
/// budget). Round four's `window_owed` read "the window lane owes nothing" when
/// absent, so one such save kept, for good, a build whose window crashes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lanes {
    /// A start of either lane confirmed the trial: update checks and applies go
    /// on.
    pub(crate) healthy: bool,
    /// A WINDOW start of the trial confirmed it: its window starts stop counting.
    pub(crate) window_proven: bool,
}

/// Whether a start from `lane` counts against the trial: a session while nothing
/// has confirmed it, a window until a WINDOW has.
pub(crate) fn start_counts(lane: Lane, lanes: Lanes) -> bool {
    match lane {
        Lane::Window => !lanes.window_proven,
        Lane::Session => !lanes.healthy,
    }
}

/// What one start's confirmation, from `lane`, does to the trial ([`confirm`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaneConfirm {
    /// It confirms: `lanes` is where the trial now stands, and the start count
    /// and its stamp begin again at zero — the starts before a lane proved itself
    /// are no verdict on the lane still owing one. `settles` says the trial was
    /// unconfirmed until now, so its update is finished, which is news; a window
    /// proving a trial a session already settled is not.
    Confirms { lanes: Lanes, settles: bool },
    /// Nothing is waiting on this lane: the trial is already confirmed for it.
    NotPending,
}

/// A start from `lane` of the file on trial proved healthy: what that does
/// ([`LaneConfirm`]). A window's confirmation settles every lane; a session's
/// settles the trial for checks and applies and leaves the window lane to prove
/// itself.
pub(crate) fn confirm(lane: Lane, lanes: Lanes) -> LaneConfirm {
    if !start_counts(lane, lanes) {
        return LaneConfirm::NotPending;
    }
    LaneConfirm::Confirms {
        lanes: Lanes {
            healthy: true,
            window_proven: lanes.window_proven || lane == Lane::Window,
        },
        settles: !lanes.healthy,
    }
}

/// Whether a newer release installed over this trial keeps the trial's own
/// rollback target instead of taking the installed build as its own: `true` when
/// only a session has confirmed the installed build and a window start of it has
/// been counted since without confirming (once a session confirmed, `starts`
/// counts window starts alone, [`confirm`]).
///
/// Each install starts a fresh trial whose rollback restores whatever was
/// installed before it. Taking the installed build there dropped its window
/// verdict: a build whose window had crashed became the build a failing successor
/// rolled back TO, so a window regression that carried into the next release left
/// a window that still crashed and the successor refused for good (round six).
/// A build no window has tried — a copy used only from the terminal — is still its
/// successor's rollback target: nothing says its window is broken, and a copy that
/// never opens a window must not roll back further with every release.
pub(crate) fn installed_window_unproven(lanes: Lanes, starts: u32) -> bool {
    lanes.healthy && !lanes.window_proven && starts > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The start rule as a machine. `young` is "the stamped start — the latest
    /// within the budget — may still confirm" ([`start_stamp`]: a start past the
    /// budget is admitted only while it is, and leaves it as it was); `Settle` is
    /// its window passing with no confirmation (that start crashed, or hung);
    /// `lost` records a rollback taken over a start that could still have
    /// confirmed — the false rollback of a healthy build this rule exists to
    /// prevent. `Buggy = 1` drops the young test from `RollBack`, which is exactly
    /// the rule before 2026-09-28 (`starts > budget`, nothing else).
    ///
    /// Untimed, so it cannot see how LONG the verdict waits; that is
    /// [`deferral_model`]'s job.
    fn start_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            LinuxTrialStartSettles {
                const Buggy = 0;
                const Max = 3;
                var starts = 0;
                var young = 0;
                var healthy = 0;
                var rolled = 0;
                var lost = 0;
                action Count when (
                    healthy == 0 && rolled == 0 && starts <= Max + 2
                        && (starts <= Max - 1 || young == 1)
                ) {
                    starts = starts + 1;
                    young = 1;
                }
                action RollBack when (
                    healthy == 0 && rolled == 0 && starts > Max - 1 && (young == 0 || Buggy == 1)
                ) {
                    starts = starts + 1;
                    rolled = 1;
                    lost = young;
                }
                action Confirm when (young == 1 && healthy == 0 && rolled == 0) {
                    healthy = 1;
                    young = 0;
                }
                action Settle when (young == 1) {
                    young = 0;
                }
                invariant NoConfirmableStartLost: lost == 0;
            }
        }
    }

    fn projection(starts_before: u32, young: bool) -> BTreeMap<&'static str, i64> {
        BTreeMap::from([
            ("starts", i64::from(starts_before)),
            ("young", i64::from(young)),
            ("healthy", 0),
            ("rolled", 0),
            ("lost", 0),
        ])
    }

    /// The machine proves no rollback is ever taken over a start that could still
    /// confirm, and the pre-2026-09-28 rule (`Buggy = 1`) is caught taking one.
    /// Then the Tier-1 bind: for every count up to past the model's space and every
    /// age of the previous start that matters (none, this second, the last young
    /// second, the first settled one, long ago, and a stamp from the future), the
    /// shipping [`start_rolls_back`] decides exactly what the model's `RollBack`
    /// guard does, and a start it keeps is one the model's `Count` admits.
    #[test]
    fn a_start_rolls_back_only_once_the_start_before_it_has_settled() {
        let model = start_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        let now = 1_000_000;
        let ages = [
            None,
            Some(0),
            Some(START_SETTLES_SECS - 1),
            Some(START_SETTLES_SECS),
            Some(3_600),
            Some(-5),
        ];
        let mut both = [false, false];
        for starts_before in 0..=crate::LINUX_TRIAL_LAUNCHES + 2 {
            for age in ages {
                let previous = age.map_or(0, |age| now - age);
                let young = start_is_young(previous, now);
                let shipping = start_rolls_back(starts_before + 1, previous, now);
                let state = projection(starts_before, young);
                assert_eq!(
                    model.action_enabled("RollBack", &state),
                    shipping,
                    "RollBack: {starts_before} starts before, previous {age:?} s ago"
                );
                assert_eq!(
                    model.action_enabled("Count", &state),
                    !shipping,
                    "Count: {starts_before} starts before, previous {age:?} s ago"
                );
                both[usize::from(shipping)] = true;
            }
        }
        assert_eq!(both, [true, true], "the grid reaches both verdicts");
        // Negative control: the pre-2026-09-28 rule rolls the fourth start of a
        // burst back, with the third still able to confirm; the model refuses that
        // step.
        let burst = projection(crate::LINUX_TRIAL_LAUNCHES, true);
        assert!(!model.action_enabled("RollBack", &burst));
        assert!(!start_rolls_back(crate::LINUX_TRIAL_LAUNCHES + 1, now, now));
    }

    /// Six starts inside one second — a terminal restoring six tabs — keep a build
    /// whose first start then confirms; a build that crashes at every start is still
    /// rolled back, at the first start after a quiet gap, whether its starts came
    /// in a burst or one at a time.
    #[test]
    fn a_burst_defers_the_verdict_and_a_quiet_gap_delivers_it() {
        let t0 = 5_000;
        let mut previous = 0;
        for start in 1..=6u32 {
            assert!(
                start <= crate::LINUX_TRIAL_LAUNCHES || !start_rolls_back(start, previous, t0),
                "start {start} of a burst rolled back"
            );
            previous = t0;
        }
        // Past the budget, a quiet gap delivers the verdict.
        assert!(start_rolls_back(7, previous, t0 + START_SETTLES_SECS));
        // One start a minute: the fourth rolls back, as it always did.
        let mut previous = 0;
        for start in 1..=crate::LINUX_TRIAL_LAUNCHES + 1 {
            let now = t0 + i64::from(start) * 60;
            assert_eq!(
                start_rolls_back(start, previous, now),
                start > crate::LINUX_TRIAL_LAUNCHES,
                "start {start}"
            );
            previous = now;
        }
        // No stamp (a record an older build wrote) is today's rule.
        assert!(start_rolls_back(crate::LINUX_TRIAL_LAUNCHES + 1, 0, t0));
        // A stamp from the future does not delay the verdict.
        assert!(start_rolls_back(
            crate::LINUX_TRIAL_LAUNCHES + 1,
            t0 + 3_600,
            t0
        ));
    }

    /// Seconds per tick of [`deferral_model`]: its settle window `W` is two ticks.
    const TICK_SECS: i64 = START_SETTLES_SECS / 2;

    /// How long the verdict waits, as a machine with a clock. `age` is the ticks
    /// since the stamp (capped at the window `W`), `wait` the ticks since the
    /// first start kept past the budget (capped just past `W`), and `late`
    /// records a start kept past the budget `W` or more ticks after the first
    /// one — the verdict deferred past its bound. `Buggy = 1` restamps at every
    /// start, which is exactly the rule before this change: a restarter faster
    /// than the window then keeps every start young, and the machine is caught
    /// keeping one late.
    fn deferral_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            LinuxTrialDeferralBounded {
                const Buggy = 0;
                const Max = 3;
                const W = 2;
                var starts = 0;
                var age = 0;
                var over = 0;
                var wait = 0;
                var rolled = 0;
                var late = 0;
                action Tick when (rolled == 0 && (age <= W - 1 || (over == 1 && wait <= W))) {
                    age = if age <= W - 1 { age + 1 } else { age };
                    wait = if (over == 1 && wait <= W) { wait + 1 } else { wait };
                }
                action Count when (
                    rolled == 0 && starts <= Max + 2 && (starts <= Max - 1 || age <= W - 1)
                ) {
                    starts = starts + 1;
                    age = if (starts <= Max - 1 || Buggy == 1) { 0 } else { age };
                    over = if starts > Max - 1 { 1 } else { over };
                    late = if (starts > Max - 1 && wait > W - 1) { 1 } else { late };
                }
                action RollBack when (rolled == 0 && starts > Max - 1 && age > W - 1) {
                    starts = starts + 1;
                    rolled = 1;
                }
                invariant DeferralBounded: late == 0;
            }
        }
    }

    /// THE VERDICT WAITS AT MOST ONE SETTLE WINDOW PAST THE BUDGET, however fast
    /// the starts come. The machine proves it, and the rule before this change
    /// (`Buggy = 1`: every start restamps) is caught deferring past it. Then the
    /// Tier-1 bind: for every count up to past the model's space and every stamp
    /// age the model can hold (this tick, one tick, the window, no stamp at all),
    /// the shipping [`start_rolls_back`] decides what the model's `RollBack` guard
    /// does, and the shipping [`start_stamp`] leaves the stamp exactly as old as
    /// the model's `Count` leaves `age`.
    #[test]
    fn the_verdict_waits_at_most_one_window_past_the_budget() {
        let model = deferral_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        let now = 2_000_000;
        let budget = crate::LINUX_TRIAL_LAUNCHES;
        let mut both = [false, false];
        for starts_before in 0..=budget + 2 {
            for age in 0..=2_i64 {
                // `None`: no stamp — a record an older build wrote — which the
                // model reads as a window long passed.
                for previous in [Some(now - age * TICK_SECS), (age == 2).then_some(0)]
                    .into_iter()
                    .flatten()
                {
                    let state = BTreeMap::from([
                        ("starts", i64::from(starts_before)),
                        ("age", age),
                        ("over", 0),
                        ("wait", 0),
                        ("rolled", 0),
                        ("late", 0),
                    ]);
                    let rolls = start_rolls_back(starts_before + 1, previous, now);
                    assert_eq!(
                        model.action_enabled("RollBack", &state),
                        rolls,
                        "RollBack: {starts_before} before, stamp {previous}"
                    );
                    assert_eq!(
                        model.action_enabled("Count", &state),
                        !rolls,
                        "Count: {starts_before} before, stamp {previous}"
                    );
                    both[usize::from(rolls)] = true;
                    if !rolls {
                        let mut next = state.clone();
                        assert!(model.fire("Count", &mut next));
                        let stamp = start_stamp(starts_before + 1, previous, now);
                        assert_eq!(
                            (now - stamp) / TICK_SECS,
                            next["age"],
                            "the stamp a kept start leaves: {starts_before} before"
                        );
                    }
                }
            }
        }
        assert_eq!(both, [true, true], "the grid reaches both verdicts");
    }

    /// A RESTARTER FASTER THAN THE SETTLE WINDOW STILL GETS ITS VERDICT: a build
    /// that crashes at every start, restarted every five seconds forever (a
    /// systemd unit with `Restart=always`, `RestartSec=5`), is rolled back at the
    /// first start a whole window after the budget's last one. Healthy bursts
    /// still wait: six tabs in one second keep the build.
    ///
    /// FAILS WITHOUT THE FIX: with every start restamping, each start found the
    /// one five seconds before it young, and no start of the loop ever rolled
    /// back (checked below through a thousand of them).
    #[test]
    fn a_crash_loop_faster_than_the_window_is_rolled_back() {
        let run = |spacing: i64, starts: u32| -> Option<u32> {
            let (mut previous, t0) = (0, 7_000);
            for start in 1..=starts {
                let now = t0 + i64::from(start) * spacing;
                if start_rolls_back(start, previous, now) {
                    return Some(start);
                }
                previous = start_stamp(start, previous, now);
            }
            None
        };
        let budget = crate::LINUX_TRIAL_LAUNCHES;
        let rolled = run(5, 1_000).expect("a five-second crash loop is rolled back");
        let bound = budget + u32::try_from(START_SETTLES_SECS / 5).unwrap_or(u32::MAX);
        assert!(
            rolled <= bound,
            "rolled back at start {rolled}, past the bound {bound}"
        );
        // The budget's own starts, a minute apart, still roll back at the next.
        assert_eq!(run(60, 10), Some(budget + 1));
        // A burst inside one second is never rolled back.
        assert_eq!(run(0, 6), None);
    }

    /// A LOST STAMP DELAYS NO VERDICT FOR GOOD, AND ROLLS NO BURST BACK (round six).
    /// The start count as `crate::linux` runs it — [`stamp_lost`] first, then
    /// [`start_rolls_back`] and [`start_stamp`] — with an older build's save
    /// dropping the stamp after the budget's last start. A burst the save landed in
    /// keeps its build (the next start stamps and waits), and a crash loop is rolled
    /// back within one settle window of the start that restamped.
    ///
    /// FAILS WITHOUT THE FIX: the burst's fourth start, finding no stamp, rolled
    /// back a build none of whose starts had had the time to confirm.
    #[test]
    fn a_lost_stamp_delays_no_verdict_for_good_and_rolls_no_burst_back() {
        let run = |spacing: i64, starts: u32| -> Option<u32> {
            let (mut previous, t0) = (0, 9_000);
            for start in 1..=starts {
                let now = t0 + i64::from(start) * spacing;
                if start == crate::LINUX_TRIAL_LAUNCHES + 1 {
                    previous = 0; // a running 0.98's whole-record save
                }
                if stamp_lost(start, previous) {
                    previous = now;
                    continue;
                }
                if start_rolls_back(start, previous, now) {
                    return Some(start);
                }
                previous = start_stamp(start, previous, now);
            }
            None
        };
        let budget = crate::LINUX_TRIAL_LAUNCHES;
        assert_eq!(run(0, 8), None, "a burst is never rolled back");
        let rolled = run(5, 1_000).expect("a five-second crash loop is rolled back");
        let bound = budget + 1 + u32::try_from(START_SETTLES_SECS / 5).unwrap_or(u32::MAX);
        assert!(
            rolled <= bound,
            "rolled back at start {rolled}, bound {bound}"
        );
        assert_eq!(run(60, 10), Some(budget + 2), "one start later than before");
        assert!(
            !stamp_lost(budget, 0),
            "within the budget every start stamps"
        );
        assert!(!stamp_lost(budget + 1, 1), "a stamp on record is judged");
    }

    /// A STAMP LOST AT EVERY START STILL ROLLS A CRASH LOOP BACK. The 0.98 that
    /// installed the update saves the whole record every half hour while the trial
    /// stands, so a build that crashes at every start, tried once an hour, finds
    /// the stamp gone at every start past the budget. The count as `crate::linux`
    /// runs it rolls back at the start [`LOST_STAMP_SPARES`] + 1 past the budget,
    /// never later, and the burst a single save lands in still keeps its build.
    ///
    /// FAILS WITHOUT THE FIX: every start past the budget that found no stamp
    /// restamped and was spared, so the loop was never rolled back (checked below
    /// through a hundred hourly starts) and its trial stood for good.
    #[test]
    fn a_stamp_lost_before_every_start_still_rolls_a_crash_loop_back() {
        let run = |spacing: i64, starts: u32, lose: &dyn Fn(u32) -> bool| -> Option<u32> {
            let (mut previous, t0) = (0, 11_000);
            for start in 1..=starts {
                let now = t0 + i64::from(start) * spacing;
                if lose(start) {
                    previous = 0; // a running 0.98's whole-record save
                }
                if stamp_lost(start, previous) {
                    previous = now;
                    continue;
                }
                if start_rolls_back(start, previous, now) {
                    return Some(start);
                }
                previous = start_stamp(start, previous, now);
            }
            None
        };
        let budget = crate::LINUX_TRIAL_LAUNCHES;
        let every_start_past_the_budget = |start: u32| start > budget;
        let bound = Some(budget + LOST_STAMP_SPARES + 1);
        assert_eq!(
            run(3_600, 100, &every_start_past_the_budget),
            bound,
            "an hourly crash loop 0.98 keeps saving over is rolled back"
        );
        assert_eq!(
            run(5, 1_000, &every_start_past_the_budget),
            bound,
            "so is a fast one"
        );
        assert_eq!(
            run(0, 8, &|start| start == budget + 1),
            None,
            "a burst one save lands in keeps its build"
        );
        assert!(
            !stamp_lost(budget + LOST_STAMP_SPARES + 1, 0),
            "the spares are bounded"
        );
    }

    /// A STAMP LOST ANYWHERE IN A BURST ROLLS NO HEALTHY BUILD BACK. Sessions start
    /// in bursts — six restored tabs in about a second — and the budget's starts
    /// stamp; the burst's later starts are judged young against the last of them.
    /// A 0.98 save landing after the SECOND or THIRD start past the budget, not only
    /// the first, left the next start with no stamp: it must be spared as the first
    /// one is, wherever in a burst of up to [`LOST_STAMP_SPARES`] starts past the
    /// budget the save lands.
    ///
    /// FAILS WITHOUT THE FIX: only the first start past the budget was spared, so a
    /// save between the burst's fifth and sixth start rolled back a build none of
    /// whose starts had had the time to print a prompt.
    #[test]
    fn a_stamp_lost_late_in_a_burst_rolls_no_healthy_build_back() {
        let run = |starts: u32, lost_before: u32| -> Option<u32> {
            let (mut previous, now) = (0, 13_000);
            for start in 1..=starts {
                if start == lost_before {
                    previous = 0; // a running 0.98's whole-record save
                }
                if stamp_lost(start, previous) {
                    previous = now;
                    continue;
                }
                if start_rolls_back(start, previous, now) {
                    return Some(start);
                }
                previous = start_stamp(start, previous, now);
            }
            None
        };
        let budget = crate::LINUX_TRIAL_LAUNCHES;
        for past in 1..=LOST_STAMP_SPARES {
            assert_eq!(
                run(budget + LOST_STAMP_SPARES, budget + past),
                None,
                "a save before the burst's start {past} past the budget"
            );
        }
        assert_eq!(
            run(6, budget + 2),
            None,
            "six tabs, the save before the fifth"
        );
        assert_eq!(
            run(6, budget + 3),
            None,
            "six tabs, the save before the sixth"
        );
    }

    /// The two lanes as a machine: `healthy` is the trial confirmed (checks and
    /// applies go on), `proven` a window confirmed it, `crashed` a window start
    /// counted with no window's confirmation since. A session's start moves no
    /// lane (it counts while nothing has confirmed), so it is not an action here.
    /// `WindowVerdictKept` is the round-four review's finding as an invariant: a
    /// window start that never confirmed is never left behind a trial whose window
    /// starts no longer count. `Buggy = 1` is the rule before that review: a
    /// session's prompt confirmed the whole file, window included.
    fn lane_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            LinuxTrialLanes {
                const Buggy = 0;
                var healthy = 0;
                var proven = 0;
                var crashed = 0;
                action WindowStart when (proven == 0) {
                    crashed = 1;
                }
                action WindowConfirm when (proven == 0) {
                    healthy = 1;
                    proven = 1;
                    crashed = 0;
                }
                action SessionConfirm when (healthy == 0) {
                    healthy = 1;
                    proven = if Buggy == 1 { 1 } else { proven };
                }
                invariant WindowVerdictKept: crashed == 0 || proven == 0;
            }
        }
    }

    fn lanes_projection(lanes: Lanes, crashed: bool) -> BTreeMap<&'static str, i64> {
        BTreeMap::from([
            ("healthy", i64::from(lanes.healthy)),
            ("proven", i64::from(lanes.window_proven)),
            ("crashed", i64::from(crashed)),
        ])
    }

    /// A SESSION NEVER VOUCHES FOR A WINDOW. The machine proves a window start
    /// that has not confirmed is never stranded behind a trial whose window
    /// starts stopped counting, and the rule before the round-four review
    /// (`Buggy = 1`) is caught stranding one. Then the Tier-1 bind: in every one
    /// of the eight states, the shipping [`start_counts`] and [`confirm`] decide
    /// exactly what the model's guards do and leave exactly the lanes its actions
    /// leave; `settles` is exactly "nothing had confirmed".
    #[test]
    fn a_session_never_vouches_for_a_window() {
        let model = lane_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        let mut reached = [false; 2];
        for bits in 0..8u8 {
            let lanes = Lanes {
                healthy: bits & 1 != 0,
                window_proven: bits & 2 != 0,
            };
            let crashed = bits & 4 != 0;
            let state = lanes_projection(lanes, crashed);
            assert_eq!(
                model.action_enabled("WindowStart", &state),
                start_counts(Lane::Window, lanes),
                "WindowStart: {lanes:?}"
            );
            // A session's start counts while nothing has confirmed, and moves no
            // lane: the model has no action for it.
            assert_eq!(start_counts(Lane::Session, lanes), !lanes.healthy);
            for (action, lane) in [
                ("WindowConfirm", Lane::Window),
                ("SessionConfirm", Lane::Session),
            ] {
                let enabled = model.action_enabled(action, &state);
                match confirm(lane, lanes) {
                    LaneConfirm::Confirms {
                        lanes: after,
                        settles,
                    } => {
                        assert!(enabled, "{action}: {lanes:?}");
                        let mut next = state.clone();
                        assert!(model.fire(action, &mut next));
                        assert_eq!(next["healthy"], i64::from(after.healthy), "{action}");
                        assert_eq!(next["proven"], i64::from(after.window_proven), "{action}");
                        assert_eq!(settles, !lanes.healthy, "{action}: {lanes:?}");
                        reached[0] = true;
                    }
                    LaneConfirm::NotPending => {
                        assert!(!enabled, "{action}: {lanes:?}");
                        reached[1] = true;
                    }
                }
            }
        }
        assert_eq!(reached, [true; 2], "the grid reaches every answer");
    }

    /// The lane rule a [`Sim`] trial is driven by.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Rule {
        /// This module's [`start_counts`] and [`confirm`].
        Shipping,
        /// Before the round-four review: every lane counted only while nothing had
        /// confirmed, and any lane's confirmation settled the file.
        BeforeTheReview,
        /// Round four: a window start that had not confirmed refused every
        /// session's confirmation, while those sessions' starts still counted
        /// against the one budget.
        RoundFour,
    }

    /// One trial as `crate::linux` keeps it, driven start by start through a lane
    /// [`Rule`], with the shipping budget and stamp ([`start_rolls_back`],
    /// [`start_stamp`]).
    struct Sim {
        rule: Rule,
        starts: u32,
        stamp: i64,
        lanes: Lanes,
        /// Round four's `window_pending`: a counted window start not confirmed.
        pending: bool,
        rolled_back: bool,
    }

    impl Sim {
        fn new(rule: Rule) -> Self {
            Self {
                rule,
                starts: 0,
                stamp: 0,
                lanes: Lanes::default(),
                pending: false,
                rolled_back: false,
            }
        }

        fn counts(&self, lane: Lane) -> bool {
            match self.rule {
                Rule::Shipping | Rule::RoundFour => start_counts(lane, self.lanes),
                Rule::BeforeTheReview => !self.lanes.healthy,
            }
        }

        /// `crate::linux::count_start_locked`'s steps: count a start of `lane` at
        /// `now`, and roll back when the rule says so.
        fn start(&mut self, lane: Lane, now: i64) {
            if self.rolled_back || !self.counts(lane) {
                return;
            }
            let previous = self.stamp;
            self.starts += 1;
            self.stamp = start_stamp(self.starts, previous, now);
            self.pending |= lane == Lane::Window;
            self.rolled_back = start_rolls_back(self.starts, previous, now);
        }

        /// `crate::linux::confirm_lane_locked`'s steps: `true` when it confirmed.
        fn confirm(&mut self, lane: Lane) -> bool {
            if self.rolled_back {
                return false;
            }
            let lanes = match self.rule {
                Rule::RoundFour if lane == Lane::Session && self.pending => return false,
                Rule::BeforeTheReview if self.lanes.healthy => return false,
                Rule::BeforeTheReview => Lanes {
                    healthy: true,
                    window_proven: true,
                },
                Rule::Shipping | Rule::RoundFour => match confirm(lane, self.lanes) {
                    LaneConfirm::Confirms { lanes, .. } => lanes,
                    LaneConfirm::NotPending => return false,
                },
            };
            self.lanes = lanes;
            self.pending = false;
            if self.rule != Rule::BeforeTheReview {
                self.starts = 0;
                self.stamp = 0;
            }
            true
        }
    }

    fn minute(n: i64) -> i64 {
        1_700_000_000 + n * 60
    }

    /// A WINDOW THAT CRASHES AT START IS ROLLED BACK, EVEN WHEN A TERMINAL IS
    /// USED TO LOOK INTO IT (the round-four review). Two windows of the new build
    /// crash before their first frame; the person types `aterm` in a terminal to
    /// see why, and its shell prompts. That session confirms the session lane and
    /// the window lane goes on counting, from zero: its fourth crashing start
    /// after the session rolls the build back. A session that confirms FIRST does
    /// the same, and a window's own confirmation settles every lane.
    ///
    /// FAILS WITHOUT THE RULE, replayed below as the negative control: under the
    /// rule before the review the session's prompt confirmed the file, the window
    /// starts after it counted nothing, and no rollback ever came.
    #[test]
    fn a_crashing_window_is_rolled_back_whatever_a_session_says() {
        let window_crashes_then_a_session = |mut trial: Sim| {
            trial.start(Lane::Window, minute(0));
            trial.start(Lane::Window, minute(1));
            trial.start(Lane::Session, minute(2));
            assert!(trial.confirm(Lane::Session), "a session is never refused");
            let mut rolled_at = None;
            for n in 3..30 {
                trial.start(Lane::Window, minute(n));
                if trial.rolled_back {
                    rolled_at = Some(n - 2);
                    break;
                }
            }
            rolled_at
        };
        assert_eq!(
            window_crashes_then_a_session(Sim::new(Rule::Shipping)),
            Some(i64::from(crate::LINUX_TRIAL_LAUNCHES) + 1),
            "the window lane's own budget decides"
        );
        assert_eq!(
            window_crashes_then_a_session(Sim::new(Rule::BeforeTheReview)),
            None,
            "the historical bug: the build stayed for good"
        );

        let session_first_then_windows_crash = |mut trial: Sim| {
            for _ in 0..6 {
                trial.start(Lane::Session, minute(0));
            }
            assert!(trial.confirm(Lane::Session));
            assert!(trial.lanes.healthy, "checks and applies go on");
            let mut rolled_at = None;
            for n in 1..30 {
                trial.start(Lane::Window, minute(n));
                if trial.rolled_back {
                    rolled_at = Some(n);
                    break;
                }
            }
            rolled_at
        };
        assert_eq!(
            session_first_then_windows_crash(Sim::new(Rule::Shipping)),
            Some(i64::from(crate::LINUX_TRIAL_LAUNCHES) + 1),
            "the window lane counts from zero, and its own budget decides"
        );
        assert_eq!(
            session_first_then_windows_crash(Sim::new(Rule::BeforeTheReview)),
            None,
            "the historical bug: window starts after a session's prompt counted nothing"
        );

        // A healthy window settles every lane: nothing counts after it.
        let mut trial = Sim::new(Rule::Shipping);
        trial.start(Lane::Session, minute(0));
        assert!(trial.confirm(Lane::Session));
        trial.start(Lane::Window, minute(1));
        assert!(trial.confirm(Lane::Window));
        assert_eq!(
            trial.lanes,
            Lanes {
                healthy: true,
                window_proven: true,
            }
        );
        trial.start(Lane::Window, minute(2));
        trial.start(Lane::Session, minute(3));
        assert_eq!(trial.starts, 0, "a settled trial counts nothing");
        assert!(!trial.confirm(Lane::Session));
        assert!(
            !trial.confirm(Lane::Window),
            "nothing is waiting on a window"
        );
    }

    /// ONE WINDOW THAT NEVER REACHED A FIRST FRAME DOES NOT LET HEALTHY SESSIONS
    /// ROLL THE BUILD BACK (round six). `aterm` run with no terminal on a box with
    /// no display, or a window killed at logout before it drew, is one window
    /// start that never confirms. Then the person works in the terminal, as the
    /// update's own words tell them to: each session prompts. The first session
    /// confirms the session lane and the count starts again, so the sessions after
    /// it count nothing and the build stays.
    ///
    /// FAILS UNDER ROUND FOUR, replayed as the negative control: every session was
    /// refused while that window start stood, yet each was counted, and the third
    /// session, the fourth start, rolled back and refused a build no second window
    /// ever tried.
    #[test]
    fn one_window_that_never_opened_does_not_let_sessions_roll_the_build_back() {
        let one_window_then_sessions = |mut trial: Sim| {
            trial.start(Lane::Window, minute(0));
            let mut confirmed = 0;
            for n in 1..=5 {
                trial.start(Lane::Session, minute(n));
                confirmed += u32::from(trial.confirm(Lane::Session));
            }
            (confirmed, trial.rolled_back)
        };
        assert_eq!(
            one_window_then_sessions(Sim::new(Rule::Shipping)),
            (1, false),
            "the first session confirms and the build stays"
        );
        assert_eq!(
            one_window_then_sessions(Sim::new(Rule::RoundFour)),
            (0, true),
            "round four: every session refused, and the sessions rolled it back"
        );
    }

    /// A newer release keeps its predecessor's rollback target only over a build a
    /// window has tried and not confirmed: never over a build nothing confirmed
    /// (no newer release installs over one), one a window proved, or one only
    /// sessions have used.
    #[test]
    fn only_a_window_start_that_never_confirmed_holds_the_rollback_target_back() {
        let session_only = Lanes {
            healthy: true,
            window_proven: false,
        };
        assert!(installed_window_unproven(session_only, 1));
        assert!(
            !installed_window_unproven(session_only, 0),
            "a copy used only from the terminal moves its rollback target on"
        );
        assert!(!installed_window_unproven(
            Lanes {
                healthy: true,
                window_proven: true,
            },
            2
        ));
        assert!(!installed_window_unproven(Lanes::default(), 2));
        // Driven: a session confirms, one window crashes, and that is the state.
        let mut trial = Sim::new(Rule::Shipping);
        trial.start(Lane::Session, minute(0));
        assert!(trial.confirm(Lane::Session));
        assert!(!installed_window_unproven(trial.lanes, trial.starts));
        trial.start(Lane::Window, minute(1));
        assert!(installed_window_unproven(trial.lanes, trial.starts));
    }

    /// `crate::linux` decides every lane question through this module's rule —
    /// the one the model above is bound to. A scrape, because that module is
    /// Linux-only and the macOS gate cannot compile it; its own tests, on Linux,
    /// drive the transactions end to end.
    #[test]
    fn the_transaction_code_decides_its_lanes_through_this_rule() {
        let linux = include_str!("linux.rs");
        for call in [
            "crate::linux_trial::start_counts(lane, trial.lanes())",
            "crate::linux_trial::confirm(lane, trial.lanes())",
            "crate::linux_trial::installed_window_unproven(trial.lanes(), trial.starts)",
            "crate::linux_trial::stamp_lost(trial.starts, previous_start)",
        ] {
            assert!(linux.contains(call), "linux.rs no longer calls {call}");
        }
        assert!(!linux.contains("enum Lane"), "one lane type: this module's");
    }
}
