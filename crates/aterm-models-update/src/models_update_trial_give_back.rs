// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A launch of the boot trial is given back by the launch it names, never by the
//! shared count's movement.

use aterm_spec::derive::Model;

/// The boot trial's count under overlapping launches. The sentinel counts the unconfirmed
/// launches of a trialed build; a launch that did not fail the build — a candidate its parent
/// stood down or killed — is given back, by the candidate on its way out (`ExitsByRule`) and by
/// its parent settling the same death (`Settled`). One launch the trial kept (a crash) is
/// already on the count, and a sibling of the same trial boots and ends beside this candidate:
/// several windows each hand off to the one installed build, and a cold or headless launch of
/// it counts like any other. A handoff target booting beside a live window launch of the trial
/// is not counted (`count_trial_launch`'s overlap exemption), nor is a headless launch of a
/// trial a window has launched (it proves nothing about the window path); any other launch is.
/// An uncounted launch names nothing on the count, so it gives nothing back.
///
/// `mine` and `theirs` say whether this candidate's launch and the sibling's are on the count.
/// A give-back takes exactly the launch it names, once (`Sentinel::forgive_launch_of`), so the
/// count is always the kept launch plus the launches still on it, a settled candidate's launch
/// is off it, and a crash the trial keeps stays on it.
///
/// A candidate LaunchServices reports terminated before it checked in carries no pid; its
/// parent settles it by `SettledUnnamed`, exactly `Settled`, by the name the candidate stamped
/// beside its rendezvous before its boot apply could count anything (aterm-update
/// `stamp_trial_launch`). A candidate that counted nothing, or gave its own launch back,
/// stamped a name not on the line, and nothing is given back. The stamp assumes no timing
/// between the sibling and the candidate's end.
///
/// A give-back waits a bounded while for the apply lock, and another aterm can hold it longer
/// (a ditto, a codesign, a supersede's Gatekeeper read). The give-back that times out writes
/// its launch's name beside the sentinel (`SettledOwed`) and the next lock holder pays it
/// (`PaysOwed`); a sibling's boot pays what is owed before it counts, so the count a revert
/// reads never stands on a launch its killer gave back (`AnOwedLaunchIsPaidBeforeTheNextCount`).
/// Unpaid, two such settles in one trial make the third candidate's boot revert and quarantine
/// a build that never failed. `SettledOwed` stands for either settle (by pid or stamped name)
/// and for the candidate's own give-back on its way out: each meets the same lock.
///
/// Mutants: `Buggy=1` gives one launch back whenever the count moved since the asker's snapshot
/// — the parent's, taken before the launch (`before`), or the candidate's own at boot
/// (`at_boot`) — and stands above zero, and never pays a debt. A sibling's give-back after the
/// snapshot then makes an uncounted candidate's ask erase the kept launch
/// (`TheCountIsTheLaunchesOnIt`), and one before the candidate's boot counted it leaves the
/// settled launch counted (`ASettledLaunchIsOffTheCount`). `Buggy=2` keeps that rule for the
/// unnamed settle alone, taking a sibling's launch counted after the snapshot for the uncounted
/// candidate's. `Buggy=3` takes the newest launch named since the parent's snapshot (`sib_at >
/// 0` for the sibling's) whose process is gone: beside an uncounted candidate, or one that gave
/// its own back, that is a sibling's, possibly a kept crash (`AKeptCrashStaysCounted`); beside
/// a counted one, a sibling that counted after it and was gone by the settle, which waits up to
/// ten seconds for the apply lock, is taken for it (`ASettledLaunchIsOffTheCount`).
///
/// Tier-1: aterm-update `install_boot_tests.rs`'s
/// `overlapping_launches_of_one_trial_are_given_back_by_name` drives these interleavings through
/// the real `count_trial_launch`, the stamp and the give-backs against a scratch sentinel, step
/// by step beside this machine, with `Buggy=1`, `2` and `3` as its negative controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_trial_give_back_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateTrialGiveBack {
            const Buggy = 0;
            // The sentinel's count, which starts at the one launch the trial kept.
            var count = 1;
            // The candidate: 0 not launched, 1 launched (the parent's snapshot
            // taken), 2 booted and live, 3 gone, 4 settled by its parent.
            var phase = 0;
            var before = 0;
            var at_boot = 0;
            var mine = 0;
            // The sibling: 0 not booted; 1 a live handoff successor; 2 gone, its
            // launch given back or never counted; 3 gone with its launch still
            // counted, for its parent to settle; 4 a live cold launch, which
            // nothing settles; 5 gone with a crash the trial keeps.
            var sibling = 0;
            var theirs = 0;
            // The candidate's phase when the sibling booted: above 0, its launch
            // is named since the parent's snapshot.
            var sib_at = 0;
            // The sibling crashed with its launch on the count: a second crash
            // the trial keeps.
            var kept = 0;
            // The candidate's settle met the apply lock held past its wait: its
            // launch is owed back, written beside the sentinel, until the next
            // holder of the lock pays it.
            var owed = 0;

            // A counted boot pays what is owed before it counts
            // (`count_trial_launch`); `Buggy=1` never pays.
            action SiblingBoots when (sibling == 0) {
                sibling = 1;
                sib_at = phase;
                theirs = if phase == 2 { 0 } else { 1 };
                count = if phase == 2 {
                    count
                } else {
                    if Buggy == 1 { count + 1 } else { count + 1 - owed }
                };
                mine = if Buggy == 1 { mine } else { mine - owed };
                owed = if Buggy == 1 { owed } else { 0 };
            }
            action SiblingLaunchedCold when (sibling == 0) {
                sibling = 4;
                sib_at = phase;
                theirs = 1;
                count = if Buggy == 1 { count + 1 } else { count + 1 - owed };
                mine = if Buggy == 1 { mine } else { mine - owed };
                owed = if Buggy == 1 { owed } else { 0 };
            }
            // Stood down, it gives its own launch back on its way out.
            action SiblingGivesBack when (sibling == 1) {
                sibling = 2;
                theirs = 0;
                count = count - theirs;
            }
            // A death its parent may give back by name later.
            action SiblingDies when (sibling == 1) {
                sibling = 3;
            }
            action SiblingSettled when (sibling == 3) {
                sibling = 2;
                theirs = 0;
                count = count - theirs;
            }
            // A crash the trial keeps: nothing gives it back.
            action SiblingCrashes when (sibling == 1 || sibling == 4) {
                sibling = 5;
                kept = theirs;
            }
            action Launch when (phase == 0) {
                phase = 1;
                before = count;
            }
            action Boots when (phase == 1) {
                phase = 2;
                at_boot = count;
                mine = if sibling == 1 || sibling == 4 { 0 } else { 1 };
                count = if sibling == 1 || sibling == 4 { count } else { count + 1 };
            }
            action ExitsByRule when (phase == 2) {
                phase = 3;
                count = if Buggy == 1 {
                    if count > 0 && (count > at_boot || at_boot > count) { count - 1 } else { count }
                } else {
                    count - mine
                };
                mine = if Buggy == 1 {
                    if count > 0 && (count > at_boot || at_boot > count) { 0 } else { mine }
                } else {
                    0
                };
            }
            action Killed when (phase == 2) {
                phase = 3;
            }
            action Settled when (phase == 3) {
                phase = 4;
                count = if Buggy == 1 {
                    if count > 0 && (count > before || before > count) { count - 1 } else { count }
                } else {
                    count - mine
                };
                mine = if Buggy == 1 {
                    if count > 0 && (count > before || before > count) { 0 } else { mine }
                } else {
                    0
                };
            }
            // LaunchServices reported the candidate terminated before it checked
            // in, with no pid: settled by the name it stamped, exactly `Settled`.
            action SettledUnnamed when (phase == 3) {
                phase = 4;
                count = if Buggy == 1 || Buggy == 2 {
                    if count > 0 && (count > before || before > count) { count - 1 } else { count }
                } else {
                    if Buggy == 3 {
                        if mine == 1 || (theirs == 1 && (sibling == 3 || sibling == 5) && sib_at > 0) {
                            count - 1
                        } else {
                            count
                        }
                    } else {
                        count - mine
                    }
                };
                // The newest gone name: a gone sibling's that counted after the
                // candidate's is taken first.
                mine = if Buggy == 1 || Buggy == 2 {
                    if count > 0 && (count > before || before > count) { 0 } else { mine }
                } else {
                    if Buggy == 3 && theirs == 1 && (sibling == 3 || sibling == 5) && sib_at > 1 {
                        mine
                    } else {
                        0
                    }
                };
                theirs = if Buggy == 3
                    && theirs == 1
                    && (sibling == 3 || sibling == 5)
                    && sib_at > 0
                    && (sib_at > 1 || mine == 0)
                {
                    0
                } else {
                    theirs
                };
            }

            // The settle met the apply lock held past its wait: the launch is
            // owed, and stays on the count until a holder of the lock pays it.
            action SettledOwed when (phase == 3) {
                phase = 4;
                owed = mine;
            }
            // The next holder of the apply lock that is no counted boot (another
            // give-back that took it) pays what is owed.
            action PaysOwed when (owed == 1 && (Buggy == 0 || Buggy > 1)) {
                count = count - 1;
                mine = 0;
                owed = 0;
            }

            invariant TheCountIsTheLaunchesOnIt: count == 1 + mine + theirs;
            invariant ASettledLaunchIsOffTheCount: phase <= 3 || mine == owed;
            invariant AKeptCrashStaysCounted: 1 + kept + mine <= count;
            invariant AnOwedLaunchIsPaidBeforeTheNextCount: sib_at <= 3 || owed == 0;
        }
    }
}
