// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! When one more start of a replaced Linux executable rolls it back, and which kind of
//! start may confirm it.
//!
//! A replaced executable (`crate::linux`) gets [`crate::LINUX_TRIAL_LAUNCHES`] starts to
//! prove itself healthy; the start after that restores the previous one and refuses the
//! new build for good. Terminal sessions count and confirm as well as windows, and
//! sessions start in bursts (six restored tabs start six copies inside a second, all
//! counted before the first prompt), so counting alone would roll a healthy build back.
//!
//! - A start over the budget rolls back only once the start before it is no longer young:
//!   [`START_SETTLES_SECS`] old, long enough for a healthy start to have confirmed itself.
//! - A session still alive and silent at [`crate::LINUX_SESSION_SILENT_DEFER`] moves the
//!   trial's stamp once (`linux::defer_session_verdict`), buying a slow shell one more
//!   window; the move confirms nothing.
//! - The wait is bounded ([`start_stamp`]): a start past the budget does not restamp, so
//!   a crash loop, however fast, is rolled back at the first start a settle window after
//!   the budget's last one.
//! - Youth is judged on the monotonic clock where both stamps read the same one
//!   ([`judged_now`]), so a wall-clock step inside a burst is no age.
//! - Each lane proves its own ([`Lanes`]): a session's prompt never confirms a window.

/// How long after a start it may still confirm itself: 20 s. A session confirms at its
/// shell's first output (milliseconds), a window after its first frame; the rest is room
/// for a confirmation that waits on the update lock or on hashing the executable, and for
/// a slow rc file.
pub(crate) const START_SETTLES_SECS: i64 = 20;

/// Whether the start stamped at `previous_start_unix` may still confirm itself at
/// `now_unix`. `0` is "no start stamped" (an older build's record), never a start to wait
/// for. A stamp in the future (the clock stepped back) is not young either, so a wrong
/// clock delays no verdict. The shipping caller passes `now_unix` on the stamp's own clock
/// ([`judged_now`]).
pub(crate) fn start_is_young(previous_start_unix: i64, now_unix: i64) -> bool {
    previous_start_unix > 0
        && (0..START_SETTLES_SECS).contains(&now_unix.saturating_sub(previous_start_unix))
}

/// Whether a silent session's deferral moves the trial's stamp, still the one its start
/// left at `own_unix`, to `now_unix` (on that stamp's own clock, [`judged_now`]): only
/// later, and only while the stamp is still young ([`start_is_young`]). A settled stamp is
/// a verdict due, which a deferral that landed late must not make young again, so the move
/// defers the verdict by less than one window, once per stamp, and never confirms.
pub(crate) fn defer_moves(own_unix: i64, now_unix: i64) -> bool {
    now_unix > own_unix && start_is_young(own_unix, now_unix)
}

/// Whether the start just counted — `starts` includes it — rolls the trial back.
pub(crate) fn start_rolls_back(starts: u32, previous_start_unix: i64, now_unix: i64) -> bool {
    starts > crate::LINUX_TRIAL_LAUNCHES && !start_is_young(previous_start_unix, now_unix)
}

/// The stamp the start just counted (`starts` includes it) leaves for the next start to be
/// judged against ([`start_rolls_back`]): its own time while the trial is within its
/// budget, the stamp as it stood once past it. A restamp past the budget would open a fresh
/// settle window, so starts less than [`START_SETTLES_SECS`] apart would defer the verdict
/// for as long as they kept coming.
pub(crate) fn start_stamp<T>(starts: u32, previous_start: T, now: T) -> T {
    if starts <= crate::LINUX_TRIAL_LAUNCHES {
        now
    } else {
        previous_start
    }
}

/// When a start was counted, as the record keeps it and the next start reads it
/// ([`judged_now`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StartClock {
    /// Wall-clock Unix seconds; `0` for no start stamped. Comparable across boots, and
    /// the stamp an older build keeps, but it steps.
    pub(crate) unix: i64,
    /// The same moment on a clock no step moves, where the system has one to read.
    pub(crate) steady: Option<Steady>,
}

impl From<i64> for StartClock {
    /// A wall-clock reading alone: what an older build's record holds, and what a
    /// system with no steady clock to read gives.
    fn from(unix: i64) -> Self {
        Self { unix, steady: None }
    }
}

/// Whole seconds on the system's monotonic clock (Linux `CLOCK_MONOTONIC`: one count
/// for every process of a boot, never stepped, stopped while the machine sleeps, so an
/// age on it is time a start could have spent confirming itself), with the clock they
/// count on: the boot, and the time namespace, which may offset it. Two readings are
/// compared only when that names the same clock.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Steady {
    /// The boot (`/proc/sys/kernel/random/boot_id`) and time namespace the count is on.
    pub(crate) clock: String,
    /// Seconds on it.
    pub(crate) secs: i64,
}

/// `now` as the wall clock that stamped `previous` reads it: the stamp's wall time plus the
/// steady clock's age since it, when both were read on the same steady clock, and `now`'s
/// wall time otherwise (no start stamped, another boot, or no steady reading). So two
/// starts of one boot are judged by how long really passed between them, however the wall
/// clock steps (a board with no battery-backed clock set by NTP, a dual-boot machine's
/// local-time clock).
pub(crate) fn judged_now(previous: &StartClock, now: &StartClock) -> i64 {
    match (&previous.steady, &now.steady) {
        (Some(then), Some(steady)) if previous.unix > 0 && then.clock == steady.clock => previous
            .unix
            .saturating_add(steady.secs.saturating_sub(then.secs)),
        _ => now.unix,
    }
}

/// Whether the start just counted (`starts` includes it) finds the trial past its budget
/// with its stamp lost: no start stamped, although the budget's starts each stamped one.
/// Only an older build's save does that (it rewrites the whole record without
/// `last_start_unix`), and it can land inside a burst of session starts, where "no stamp"
/// read as the plain budget would roll a healthy build back before its first prompt.
///
/// Such a start, one of the first [`LOST_STAMP_SPARES`] past the budget, stamps its own
/// time and does not roll back. A stamp lost before every start costs a crash loop at most
/// [`LOST_STAMP_SPARES`] more starts.
pub(crate) fn stamp_lost(starts: u32, previous_start_unix: i64) -> bool {
    previous_start_unix == 0
        && (crate::LINUX_TRIAL_LAUNCHES + 1
            ..=crate::LINUX_TRIAL_LAUNCHES.saturating_add(LOST_STAMP_SPARES))
            .contains(&starts)
}

/// How many starts past the budget may each find the stamp lost and be spared
/// ([`stamp_lost`]): the longest burst a lost stamp is forgiven in, and the most starts a
/// crash loop an older build keeps saving over costs before it is rolled back. Eight past
/// the budget covers a launch that restores eleven tabs at once.
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

/// Where a trial's two lanes stand: the trial record's `healthy` and `window_proven`.
///
/// A session confirms the session lane only: window starts go on counting, and roll the
/// install back past the budget, until a window confirms, or `aterm` typed into a terminal
/// would confirm a build whose window crashes at start. A session is never refused, and its
/// confirmation restarts the count at zero, so the window lane's budget is spent by window
/// starts alone.
///
/// An older build saves the whole record without these fields, so each one's missing value
/// is the careful one: a dropped `window_proven` counts window starts again until a window
/// confirms again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lanes {
    /// A start of either lane confirmed the trial: update checks and applies go
    /// on.
    pub(crate) healthy: bool,
    /// A window start of the trial confirmed it: its window starts stop counting.
    pub(crate) window_proven: bool,
}

/// Whether a start from `lane` counts against the trial: a session while nothing
/// has confirmed it, a window until a window has.
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

/// Whether a newer release installed over this trial keeps the trial's own rollback target
/// instead of taking the installed build: only a session has confirmed the installed build,
/// and a window start of it has been counted since without confirming (after a session
/// confirms, `starts` counts window starts alone). Otherwise a build whose window crashed
/// would become the rollback target of a successor carrying the same regression. A build no
/// window has tried stays its successor's target, so a terminal-only copy does not roll
/// back further every release.
pub(crate) fn installed_window_unproven(lanes: Lanes, starts: u32) -> bool {
    lanes.healthy && !lanes.window_proven && starts > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The start rule as a machine. `young`: the stamped start (the latest within the
    /// budget) may still confirm; `Settle`: its window passes unconfirmed; `lost`: a
    /// rollback taken over a start that could still have confirmed. `Buggy = 1` drops the
    /// young test from `RollBack`. Untimed: [`deferral_model`] covers how long it waits.
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

    /// The machine proves no rollback is taken over a start that could still confirm, and
    /// catches the count-only rule (`Buggy = 1`); then the shipping [`start_rolls_back`]
    /// is bound to the model's guards over every count and every age that matters.
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
        // Negative control: the count-only rule rolls the fourth start of a burst back
        // with the third still able to confirm; the model refuses that step.
        let burst = projection(crate::LINUX_TRIAL_LAUNCHES, true);
        assert!(!model.action_enabled("RollBack", &burst));
        assert!(!start_rolls_back(crate::LINUX_TRIAL_LAUNCHES + 1, now, now));
    }

    /// A wall-clock step between two starts of one boot moves no verdict: for every count,
    /// real age and step (an hour either way, a second back, none), the shipping decision
    /// on [`judged_now`] is the model's `RollBack` guard at the real age. Negative control:
    /// the raw wall clock rolls back the fourth start of a burst an hour's step landed in.
    #[test]
    fn a_wall_clock_step_between_two_starts_of_one_boot_moves_no_verdict() {
        let model = start_model();
        let steady = |clock: &str, secs: i64| {
            Some(Steady {
                clock: clock.to_string(),
                secs,
            })
        };
        let wall = 1_000_000;
        let previous = StartClock {
            unix: wall,
            steady: steady("boot-a time:[1]", 400),
        };
        let mut both = [false, false];
        for starts_before in 0..=crate::LINUX_TRIAL_LAUNCHES + 2 {
            for age in [0, START_SETTLES_SECS - 1, START_SETTLES_SECS, 3_600] {
                for step in [-3_600, -1, 0, 3_600] {
                    let now = StartClock {
                        unix: wall + age + step,
                        steady: steady("boot-a time:[1]", 400 + age),
                    };
                    let judged = judged_now(&previous, &now);
                    assert_eq!(judged, wall + age, "age {age}, step {step}");
                    let shipping = start_rolls_back(starts_before + 1, previous.unix, judged);
                    let state = projection(starts_before, age < START_SETTLES_SECS);
                    assert_eq!(
                        model.action_enabled("RollBack", &state),
                        shipping,
                        "RollBack: {starts_before} before, {age} s on, wall stepped {step} s"
                    );
                    assert_eq!(
                        model.action_enabled("Count", &state),
                        !shipping,
                        "Count: {starts_before} before, {age} s on, wall stepped {step} s"
                    );
                    both[usize::from(shipping)] = true;
                }
            }
        }
        assert_eq!(both, [true, true], "the grid reaches both verdicts");
        let budget = crate::LINUX_TRIAL_LAUNCHES;
        assert!(!model.action_enabled("RollBack", &projection(budget, true)));
        assert!(
            start_rolls_back(budget + 1, wall, wall + 3_600),
            "the raw wall clock read the stepped burst as settled"
        );
        // Where no steady clock spans both starts the wall clock judges: another boot,
        // another time namespace, a side that read none, no stamp.
        let later = wall + 3_600;
        for (then, now) in [
            (previous.steady.clone(), steady("boot-b time:[1]", 401)),
            (previous.steady.clone(), steady("boot-a time:[2]", 401)),
            (previous.steady.clone(), None),
            (None, steady("boot-a time:[1]", 401)),
        ] {
            let then = StartClock {
                unix: wall,
                steady: then,
            };
            let now = StartClock {
                unix: later,
                steady: now,
            };
            assert_eq!(judged_now(&then, &now), later, "{then:?} -> {now:?}");
        }
        let unstamped = StartClock {
            unix: 0,
            steady: steady("boot-a time:[1]", 400),
        };
        let now = StartClock {
            unix: later,
            steady: steady("boot-a time:[1]", 401),
        };
        assert_eq!(judged_now(&unstamped, &now), later, "no start stamped");
        assert!(!start_is_young(
            unstamped.unix,
            judged_now(&unstamped, &now)
        ));
    }

    /// Six starts inside one second keep a build whose first start then confirms; a build
    /// that crashes at every start is still rolled back at the first start after a quiet gap.
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
        // One start a minute: the fourth rolls back.
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
        // No stamp (an older build's record) rolls back on the plain budget.
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

    /// How long the verdict waits, as a machine with a clock. `age` is ticks since the
    /// stamp (capped at the window `W`), `wait` ticks since the first start kept past the
    /// budget (capped just past `W` + 1), and `late` records a start kept past the budget
    /// more than `W` ticks after the first: the verdict deferred past its bound.
    /// `Buggy = 1` restamps at every start and is caught keeping one late.
    ///
    /// `Defer` is a silent session's one move of the stamp to now ([`defer_moves`]): only
    /// while the stamp is young (`age` one tick: half a window after the start) and
    /// `owned` (still the stamp a budget start left; `Count` sets it, the move clears it).
    /// `Once = 0` drops that latch and is caught keeping a start late.
    fn deferral_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            LinuxTrialDeferralBounded {
                const Buggy = 0;
                const Once = 1;
                const Max = 3;
                const W = 2;
                var starts = 0;
                var age = 0;
                var over = 0;
                var wait = 0;
                var rolled = 0;
                var late = 0;
                var owned = 0;
                action Tick when (
                    rolled == 0 && (age <= W - 1 || (over == 1 && wait <= W + 1))
                ) {
                    age = if age <= W - 1 { age + 1 } else { age };
                    wait = if (over == 1 && wait <= W + 1) { wait + 1 } else { wait };
                }
                action Count when (
                    rolled == 0 && starts <= Max + 2 && (starts <= Max - 1 || age <= W - 1)
                ) {
                    starts = starts + 1;
                    age = if (starts <= Max - 1 || Buggy == 1) { 0 } else { age };
                    over = if starts > Max - 1 { 1 } else { over };
                    late = if (starts > Max - 1 && wait > W) { 1 } else { late };
                    owned = if starts <= Max - 1 { 1 } else { owned };
                }
                action Defer when (
                    rolled == 0 && (owned == 1 || Once == 0) && age > 0 && age <= W - 1
                ) {
                    age = 0;
                    owned = 0;
                }
                action RollBack when (rolled == 0 && starts > Max - 1 && age > W - 1) {
                    starts = starts + 1;
                    rolled = 1;
                }
                invariant DeferralBounded: late == 0;
            }
        }
    }

    /// Past the budget the verdict waits at most one settle window, plus less than one more
    /// for a silent session, however fast the starts come. The model is proved (catching
    /// `Buggy = 1`) and bound to [`start_rolls_back`], [`start_stamp`] and [`defer_moves`].
    #[test]
    fn the_verdict_waits_at_most_one_window_past_the_budget() {
        let model = deferral_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        let now = 2_000_000;
        let budget = crate::LINUX_TRIAL_LAUNCHES;
        let mut both = [false, false];
        for starts_before in 0..=budget + 2 {
            for age in 0..=2_i64 {
                // A stamp of `0`, no stamp (an older build's record), reads in the
                // model as a window long passed.
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
                        ("owned", 0),
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
        // The silent session's move is the model's `Defer`, at every age the model
        // holds, the stamp still its own or not; and the move leaves the stamp as
        // old as `Defer` leaves `age`.
        let mut moved = [false, false];
        for age in 0..=2_i64 {
            for owned in [false, true] {
                let state = BTreeMap::from([
                    ("starts", i64::from(budget)),
                    ("age", age),
                    ("over", 0),
                    ("wait", 0),
                    ("rolled", 0),
                    ("late", 0),
                    ("owned", i64::from(owned)),
                ]);
                let own = now - age * TICK_SECS;
                let shipping = owned && defer_moves(own, now);
                assert_eq!(
                    model.action_enabled("Defer", &state),
                    shipping,
                    "Defer: stamp {age} tick(s) old, owned {owned}"
                );
                if shipping {
                    let mut next = state.clone();
                    assert!(model.fire("Defer", &mut next));
                    assert_eq!(next["age"], 0, "the move stamps now");
                    assert_eq!(next["owned"], 0, "once: the stamp is no longer its own");
                }
                moved[usize::from(shipping)] = true;
            }
        }
        assert_eq!(moved, [true, true], "the grid reaches both answers");
        // Negative controls: a move that is not later, or of a settled stamp.
        assert!(!defer_moves(now, now), "no move to the same moment");
        assert!(
            !defer_moves(now - START_SETTLES_SECS, now),
            "nor of a settled stamp"
        );
        // The `owned` latch is load-bearing: without it the verdict is kept late.
        assert!(
            aterm_spec::interp::bmc(&aterm_spec::interp::with_consts(&model, &[("Once", 0)]))
                .is_err(),
            "a session that moves the stamp as often as it stays silent defers for good"
        );
        // And the session waits half a window before it moves it.
        assert_eq!(
            i64::try_from(crate::LINUX_SESSION_SILENT_DEFER.as_secs()).unwrap(),
            TICK_SECS,
            "the silent session moves its stamp one tick (half a window) after its start"
        );
    }

    /// A crash loop every five seconds (`Restart=always`, `RestartSec=5`) is rolled back
    /// within a window of the budget's last start; six tabs in one second keep the build.
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

    /// With the stamp dropped after the budget's last start, a burst keeps its build and a
    /// crash loop is rolled back within one settle window of the start that restamped.
    #[test]
    fn a_lost_stamp_delays_no_verdict_for_good_and_rolls_no_burst_back() {
        let run = |spacing: i64, starts: u32| -> Option<u32> {
            let (mut previous, t0) = (0, 9_000);
            for start in 1..=starts {
                let now = t0 + i64::from(start) * spacing;
                if start == crate::LINUX_TRIAL_LAUNCHES + 1 {
                    previous = 0; // an older build's whole-record save
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

    /// A stamp lost at every start (an older build saving the record while the trial
    /// stands) still rolls a crash loop back by [`LOST_STAMP_SPARES`] + 1 starts past the
    /// budget; a burst one save lands in keeps its build.
    #[test]
    fn a_stamp_lost_before_every_start_still_rolls_a_crash_loop_back() {
        let run = |spacing: i64, starts: u32, lose: &dyn Fn(u32) -> bool| -> Option<u32> {
            let (mut previous, t0) = (0, 11_000);
            for start in 1..=starts {
                let now = t0 + i64::from(start) * spacing;
                if lose(start) {
                    previous = 0; // an older build's whole-record save
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

    /// A stamp lost anywhere in a burst of up to [`LOST_STAMP_SPARES`] starts past the
    /// budget rolls no healthy build back.
    #[test]
    fn a_stamp_lost_late_in_a_burst_rolls_no_healthy_build_back() {
        let run = |starts: u32, lost_before: u32| -> Option<u32> {
            let (mut previous, now) = (0, 13_000);
            for start in 1..=starts {
                if start == lost_before {
                    previous = 0; // an older build's whole-record save
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

    /// The two lanes as a machine: `healthy` is the trial confirmed (checks and applies go
    /// on), `proven` a window confirmed it, `crashed` a window start counted with no
    /// window's confirmation since. A session's start moves no lane, so it is no action.
    /// `WindowVerdictKept`: an unconfirmed window start is never left behind a trial whose
    /// window starts no longer count. `Buggy = 1` lets a session's prompt confirm the file.
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

    /// A session never vouches for a window: the model is proved (catching `Buggy = 1`),
    /// and in all eight states [`start_counts`] and [`confirm`] match its guards and actions.
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
        /// Negative control: every lane counts only while nothing has confirmed, and any
        /// lane's confirmation settles the file.
        BeforeTheReview,
        /// Negative control: a window start that has not confirmed refuses every session's
        /// confirmation, while those sessions' starts still count against one budget.
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
        /// [`Rule::RoundFour`]'s `window_pending`: a counted window start not confirmed.
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

    /// A window that crashes at start is rolled back even when a session confirmed the
    /// trial, before or after the crashes. Negative control: [`Rule::BeforeTheReview`].
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

    /// One window start that never reached a first frame does not let healthy sessions roll
    /// the build back. Negative control: [`Rule::RoundFour`].
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

    /// A newer release keeps its predecessor's rollback target only over a build a window
    /// has tried and not confirmed.
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

    /// `crate::linux` decides every lane question through this module's rule, the one the
    /// models above are bound to (a scrape of its source).
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
