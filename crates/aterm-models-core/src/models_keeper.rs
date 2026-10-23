// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pty keeper (`docs/DESIGN-pty-keeper-2026-09-26.md` §6.1, §6.2, §6.5, §6.6).

use aterm_spec::derive::Model;

/// Custody of two masters, `a` and `b`, across four processes.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn pty_keeper_custody_model() -> Model {
    aterm_spec::ty_model! {
        PtyKeeperCustody {
            const Buggy = 0;
            const MaxRelaunch = 3;
            var w_live = 1;
            var w_holds_a = 1;
            var w_reads_a = 1;
            var w_holds_b = 1;
            var w_reads_b = 1;
            var s_live = 0;
            var s_holds_a = 0;
            var s_reads_a = 0;
            var s_holds_b = 0;
            var s_reads_b = 0;
            var r_live = 0;
            var r_holds_a = 0;
            var r_reads_a = 0;
            var r_holds_b = 0;
            var r_reads_b = 0;
            var k_live = 1;
            var k_holds_a = 0;
            var k_holds_b = 0;
            var orphan_a = 0;
            var orphan_b = 0;
            var pending = 0;
            var committed = 0;
            var bye = 0;
            var closed_a = 0;
            var closed_b = 0;
            var ever_registered_a = 0;
            var ever_registered_b = 0;
            var lost_a = 0;
            var lost_b = 0;
            var relaunches = 0;
            var k_faults = 0;
            var o_draining = 0;
            var o_claims_a = 0;
            var r_relinked = 0;

            action RegisterA when (
                k_live == 1 && k_holds_a == 0 && closed_a == 0 &&
                ((w_live == 1 && w_holds_a == 1) ||
                 (s_live == 1 && committed == 1 && s_holds_a == 1) ||
                 (r_live == 1 && r_reads_a == 1))
            ) {
                k_holds_a = 1;
                ever_registered_a = 1;
                orphan_a = 0;
            }
            action RegisterB when (
                k_live == 1 && k_holds_b == 0 && closed_b == 0 &&
                ((w_live == 1 && w_holds_b == 1) ||
                 (s_live == 1 && committed == 1 && s_holds_b == 1) ||
                 (r_live == 1 && r_reads_b == 1))
            ) {
                k_holds_b = 1;
                ever_registered_b = 1;
                orphan_b = 0;
            }
            action CloseTabA when (
                closed_a == 0 &&
                ((w_live == 1 && w_reads_a == 1 && pending == 0) || (r_live == 1 && r_reads_a == 1))
            ) {
                closed_a = 1;
                w_holds_a = 0;
                w_reads_a = 0;
                r_holds_a = 0;
                r_reads_a = 0;
                k_holds_a = if Buggy == 1 { k_holds_a } else { 0 };
                orphan_a = if Buggy == 1 { orphan_a } else { 0 };
            }
            action CloseTabB when (
                closed_b == 0 &&
                ((w_live == 1 && w_reads_b == 1 && pending == 0) || (r_live == 1 && r_reads_b == 1))
            ) {
                closed_b = 1;
                w_holds_b = 0;
                w_reads_b = 0;
                r_holds_b = 0;
                r_reads_b = 0;
                k_holds_b = if Buggy == 1 { k_holds_b } else { 0 };
                orphan_b = if Buggy == 1 { orphan_b } else { 0 };
            }
            action Quit when (w_live == 1 && pending == 0) {
                w_live = 0;
                bye = 1;
                w_holds_a = 0;
                w_reads_a = 0;
                w_holds_b = 0;
                w_reads_b = 0;
            }
            action KeeperHonoursBye when (
                bye == 1 && k_live == 1 && w_live == 0 && s_live == 0 && r_live == 0 &&
                (k_holds_a == 1 || k_holds_b == 1) && orphan_a == 0 && orphan_b == 0
            ) {
                k_holds_a = if Buggy == 1 { k_holds_a } else { 0 };
                k_holds_b = if Buggy == 1 { k_holds_b } else { 0 };
                orphan_a = if Buggy == 1 { k_holds_a } else { 0 };
                orphan_b = if Buggy == 1 { k_holds_b } else { 0 };
            }
            action WindowCrashes when (w_live == 1) {
                w_live = 0;
                w_holds_a = 0;
                w_reads_a = 0;
                w_holds_b = 0;
                w_reads_b = 0;
                orphan_a = if pending == 0 { k_holds_a } else { orphan_a };
                orphan_b = if pending == 0 { k_holds_b } else { orphan_b };
                lost_a = if w_holds_a == 1 && k_holds_a == 0 && s_holds_a == 0 && closed_a == 0 { 1 } else { lost_a };
                lost_b = if w_holds_b == 1 && k_holds_b == 0 && s_holds_b == 0 && closed_b == 0 { 1 } else { lost_b };
            }
            action LinkDrops when (w_live == 1 && orphan_a == 0 && orphan_b == 0) {
                orphan_a = if Buggy == 1 { k_holds_a } else { orphan_a };
                orphan_b = if Buggy == 1 { k_holds_b } else { orphan_b };
            }
            action StartSuccessor when (
                w_live == 1 && s_live == 0 && pending == 0 && committed == 0 && bye == 0
            ) {
                s_live = 1;
                pending = 1;
                s_holds_a = w_holds_a;
                s_holds_b = w_holds_b;
                w_reads_a = 0;
                w_reads_b = 0;
            }
            action Commit when (w_live == 1 && s_live == 1 && pending == 1) {
                w_live = 0;
                w_holds_a = 0;
                w_holds_b = 0;
                s_reads_a = s_holds_a;
                s_reads_b = s_holds_b;
                committed = 1;
                pending = 0;
                o_draining = 1;
                o_claims_a = k_holds_a;
                orphan_a = if Buggy == 1 { k_holds_a } else { orphan_a };
                orphan_b = if Buggy == 1 { k_holds_b } else { orphan_b };
            }
            action OutgoingReleasesA when (o_draining == 1 && o_claims_a == 1 && k_holds_a == 1) {
                o_claims_a = 0;
                k_holds_a = if Buggy == 1 || closed_a == 1 { 0 } else { k_holds_a };
            }
            action SuccessorClosesTabA when (
                o_draining == 1 && s_live == 1 && s_reads_a == 1 && closed_a == 0
            ) {
                closed_a = 1;
                s_holds_a = 0;
                s_reads_a = 0;
                k_holds_a = if o_draining == 1 && o_claims_a == 1 { k_holds_a } else { 0 };
            }
            action OutgoingDrained when (o_draining == 1) {
                o_draining = 0;
                o_claims_a = 0;
                k_holds_a = if closed_a == 1 { 0 } else { k_holds_a };
            }
            action Rollback when (w_live == 1 && s_live == 1 && pending == 1) {
                s_live = 0;
                s_holds_a = 0;
                s_holds_b = 0;
                pending = 0;
                w_reads_a = w_holds_a;
                w_reads_b = w_holds_b;
            }
            action SuccessorFailStops when (w_live == 0 && s_live == 1 && committed == 0) {
                s_live = 0;
                pending = 0;
                s_holds_a = 0;
                s_holds_b = 0;
                orphan_a = k_holds_a;
                orphan_b = k_holds_b;
                lost_a = if s_holds_a == 1 && k_holds_a == 0 && closed_a == 0 { 1 } else { lost_a };
                lost_b = if s_holds_b == 1 && k_holds_b == 0 && closed_b == 0 { 1 } else { lost_b };
            }
            action SuccessorCrashes when (s_live == 1 && committed == 1 && o_draining == 0) {
                s_live = 0;
                s_holds_a = 0;
                s_reads_a = 0;
                s_holds_b = 0;
                s_reads_b = 0;
                orphan_a = k_holds_a;
                orphan_b = k_holds_b;
                lost_a = if s_holds_a == 1 && k_holds_a == 0 && closed_a == 0 { 1 } else { lost_a };
                lost_b = if s_holds_b == 1 && k_holds_b == 0 && closed_b == 0 { 1 } else { lost_b };
            }
            action SuccessorCrashesFirst when (o_draining == 1 && s_live == 1 && committed == 1) {
                o_draining = 0;
                o_claims_a = 0;
                s_live = 0;
                s_holds_a = 0;
                s_reads_a = 0;
                s_holds_b = 0;
                s_reads_b = 0;
                k_holds_a = if closed_a == 1 || Buggy == 1 { 0 } else { k_holds_a };
                k_holds_b = if Buggy == 1 { 0 } else { k_holds_b };
                orphan_a = if closed_a == 1 || Buggy == 1 { 0 } else { k_holds_a };
                orphan_b = if Buggy == 1 { 0 } else { k_holds_b };
                lost_a = if s_holds_a == 1 && closed_a == 0 && (k_holds_a == 0 || Buggy == 1) { 1 } else { lost_a };
                lost_b = if s_holds_b == 1 && closed_b == 0 && (k_holds_b == 0 || Buggy == 1) { 1 } else { lost_b };
            }
            action SuccessorQuits when (s_live == 1 && committed == 1 && o_draining == 0) {
                s_live = 0;
                s_holds_a = 0;
                s_reads_a = 0;
                s_holds_b = 0;
                s_reads_b = 0;
                bye = 1;
                k_holds_a = if Buggy == 1 { k_holds_a } else { 0 };
                k_holds_b = if Buggy == 1 { k_holds_b } else { 0 };
                orphan_a = if Buggy == 1 { k_holds_a } else { 0 };
                orphan_b = if Buggy == 1 { k_holds_b } else { 0 };
            }
            action Relaunch when (
                k_live == 1 && (w_live == 0 || Buggy == 1) && s_live == 0 && r_live == 0 &&
                (bye == 0 || Buggy == 1) && (orphan_a == 1 || orphan_b == 1) &&
                relaunches <= MaxRelaunch - 1
            ) {
                r_live = 1;
                relaunches = relaunches + 1;
            }
            action OfferA when (
                k_live == 1 && r_live == 1 && orphan_a == 1 && k_holds_a == 1 && r_holds_a == 0 &&
                (w_holds_a + s_holds_a == 0 || Buggy == 1) && (closed_a == 0 || Buggy == 1)
            ) {
                r_holds_a = 1;
                orphan_a = 0;
                k_holds_a = if Buggy == 1 { 0 } else { 1 };
            }
            action OfferB when (
                k_live == 1 && r_live == 1 && orphan_b == 1 && k_holds_b == 1 && r_holds_b == 0 &&
                (w_holds_b + s_holds_b == 0 || Buggy == 1) && (closed_b == 0 || Buggy == 1)
            ) {
                r_holds_b = 1;
                orphan_b = 0;
                k_holds_b = if Buggy == 1 { 0 } else { 1 };
            }
            action AdoptA when (r_live == 1 && r_holds_a == 1 && r_reads_a == 0) {
                r_reads_a = 1;
                orphan_a = 0;
            }
            action AdoptB when (r_live == 1 && r_holds_b == 1 && r_reads_b == 0) {
                r_reads_b = 1;
                orphan_b = 0;
            }
            action RelaunchedLinkDrops when (
                k_live == 1 && r_live == 1 &&
                (orphan_a == 0 || r_holds_a == 1) && (orphan_b == 0 || r_holds_b == 1) &&
                ((r_holds_a == 1 && r_reads_a == 0 && k_holds_a == 1 && orphan_a == 0) ||
                 (r_holds_b == 1 && r_reads_b == 0 && k_holds_b == 1 && orphan_b == 0))
            ) {
                orphan_a = if Buggy == 0 && r_holds_a == 1 && r_reads_a == 0 { k_holds_a } else { orphan_a };
                orphan_b = if Buggy == 0 && r_holds_b == 1 && r_reads_b == 0 { k_holds_b } else { orphan_b };
                r_relinked = if Buggy == 0 { 1 } else { r_relinked };
            }
            action RelaunchedWindowQuits when (r_live == 1 && r_relinked == 1) {
                r_live = 0;
                r_holds_a = 0;
                r_reads_a = 0;
                r_holds_b = 0;
                r_reads_b = 0;
                k_holds_a = if r_reads_a == 1 { 0 } else { k_holds_a };
                k_holds_b = if r_reads_b == 1 { 0 } else { k_holds_b };
                orphan_a = if r_holds_a == 1 && r_reads_a == 0 { k_holds_a } else { orphan_a };
                orphan_b = if r_holds_b == 1 && r_reads_b == 0 { k_holds_b } else { orphan_b };
                bye = if Buggy == 1 && r_relinked == 1 { 0 } else { 1 };
                r_relinked = 0;
            }
            action RelaunchedWindowCrashes when (r_live == 1) {
                r_live = 0;
                r_relinked = 0;
                r_holds_a = 0;
                r_reads_a = 0;
                r_holds_b = 0;
                r_reads_b = 0;
                orphan_a = k_holds_a;
                orphan_b = k_holds_b;
                lost_a = if r_holds_a == 1 && k_holds_a == 0 && closed_a == 0 { 1 } else { lost_a };
                lost_b = if r_holds_b == 1 && k_holds_b == 0 && closed_b == 0 { 1 } else { lost_b };
            }
            action KeeperCrashes when (k_live == 1 && k_faults == 0) {
                k_live = 0;
                k_faults = 1;
                k_holds_a = 0;
                k_holds_b = 0;
                orphan_a = 0;
                orphan_b = 0;
                o_draining = 0;
                o_claims_a = 0;
                r_relinked = 0;
                lost_a = if k_holds_a == 1 && w_holds_a + s_holds_a + r_holds_a == 0 && closed_a == 0 { 1 } else { lost_a };
                lost_b = if k_holds_b == 1 && w_holds_b + s_holds_b + r_holds_b == 0 && closed_b == 0 { 1 } else { lost_b };
            }
            action KeeperRestarts when (k_live == 0) {
                k_live = 1;
                relaunches = 0;
            }

            invariant NeverTwoReadersA: w_reads_a + s_reads_a + r_reads_a <= 1;
            invariant NeverTwoReadersB: w_reads_b + s_reads_b + r_reads_b <= 1;
            invariant OfferOnlyWithNoLiveHolderA: r_holds_a == 0 || (w_live == 0 && s_live == 0);
            invariant OfferOnlyWithNoLiveHolderB: r_holds_b == 0 || (w_live == 0 && s_live == 0);
            invariant ClosedNeverReachesAWindowA: closed_a == 0 || s_holds_a + r_holds_a == 0;
            invariant ClosedNeverReachesAWindowB: closed_b == 0 || s_holds_b + r_holds_b == 0;
            invariant NoRelaunchAfterQuit: bye == 0 || r_live == 0;
            invariant LossNeedsKeeperFaultA: lost_a == 0 || k_faults > 0 || ever_registered_a == 0;
            invariant LossNeedsKeeperFaultB: lost_b == 0 || k_faults > 0 || ever_registered_b == 0;
            bound RelaunchSpace: relaunches <= MaxRelaunch;
        }
    }
}

/// The relaunch brake as a writer/reader pair (the shape of `NativeUpdateFailedMarkSuppression`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn keeper_relaunch_brake_model() -> Model {
    aterm_spec::ty_model! {
        KeeperRelaunchBrake {
            const Buggy = 0;
            const MaxStreak = 3;
            var kind = 0;
            var phase = 0;
            var clean = 0;
            var streak = 0;
            var held = 0;
            var grants = 0;
            var after_quit = 0;

            action PickRepeatedDeaths when (kind == 0) {
                kind = 1;
            }
            action PickBootTrial when (kind == 0) {
                kind = 2;
            }
            action Death when (kind > 0 && phase == 0) {
                phase = 1;
                clean = 0;
            }
            action CleanQuit when (kind > 0 && phase == 0) {
                phase = 1;
                clean = 1;
            }
            action Decide when (phase == 1) {
                phase = if clean == 1 && Buggy == 0 { 0 } else {
                    if streak <= MaxStreak - 1 || (Buggy == 1 && kind == 1 && streak <= MaxStreak) { 2 } else { 3 }
                };
                grants = if clean == 1 && Buggy == 0 { grants } else {
                    if streak <= MaxStreak - 1 || (Buggy == 1 && kind == 1 && streak <= MaxStreak) { grants + 1 } else { grants }
                };
                streak = if clean == 1 && Buggy == 0 { streak } else {
                    if streak <= MaxStreak - 1 || (Buggy == 1 && kind == 1 && streak <= MaxStreak) { streak + 1 } else { streak }
                };
                held = if clean == 1 && Buggy == 0 { held } else {
                    if streak <= MaxStreak - 1 || (Buggy == 1 && kind == 1 && streak <= MaxStreak) { 0 } else { 1 }
                };
                after_quit = if clean == 1 && Buggy == 1 { 1 } else { after_quit };
            }
            action RelaunchFails when (phase == 2) {
                phase = 1;
                clean = 0;
                streak = if Buggy == 1 && kind == 2 && streak == 1 && grants == 1 { streak + 1 } else { streak };
            }
            action Healthy when (phase == 2) {
                phase = 0;
                streak = 0;
                grants = 0;
            }
            action ManualLaunch when (phase == 3) {
                phase = 0;
                streak = 0;
                grants = 0;
                held = 0;
            }

            invariant CleanQuitNeverRelaunches: after_quit == 0;
            invariant StreakBounded: streak <= MaxStreak;
            invariant HoldOnlyWhenSpent: held == 0 || grants > MaxStreak - 1;
        }
    }
}

/// How the keeper judges one window's end (§5.4's classifier, with row 3's crash-marker cross-check), and the prune of an orphan whose shell died.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn keeper_death_judgement_model() -> Model {
    aterm_spec::ty_model! {
        KeeperDeathJudgement {
            const Buggy = 0;
            var sends_bye = 1;
            var names_marker = 1;
            var ending = 0;
            var quit = 0;
            var bye = 0;
            var status = 0;
            var marker = 0;
            var judged = 0;
            var judged_quit = 0;
            var orphaned = 0;
            var ever_orphaned = 0;
            var shell_live = 1;
            var watch_pending = 0;

            action PickP3Window when (ending == 0 && names_marker == 1) {
                names_marker = 0;
            }
            action PickPreByeWindow when (ending == 0 && sends_bye == 1) {
                sends_bye = 0;
                names_marker = 0;
            }
            action QuitWithBye when (ending == 0) {
                ending = 1;
                quit = 1;
                bye = sends_bye;
                status = 1;
                marker = if names_marker == 1 { 1 } else { 0 };
            }
            action QuitByeLost when (ending == 0 && sends_bye == 1) {
                ending = 2;
                quit = 1;
                bye = 0;
                status = 1;
                marker = if names_marker == 1 { 1 } else { 0 };
            }
            action Killed when (ending == 0) {
                ending = 3;
                status = 3;
                marker = if names_marker == 1 { 2 } else { 0 };
            }
            action PanicExit when (ending == 0) {
                ending = 4;
                status = 2;
                marker = if names_marker == 1 { 1 } else { 0 };
            }
            action StatusLost when (ending > 0 && judged == 0 && status > 0) {
                status = 0;
            }
            action MarkerSwept when (judged == 0 && marker == 2) {
                marker = 1;
            }
            action MarkerInherited when (judged == 0 && marker == 2) {
                marker = 3;
            }
            action Judge when (ending > 0 && judged == 0) {
                judged = 1;
                judged_quit = if bye == 1 || (Buggy == 0 && status == 1 && (marker == 1 || sends_bye == 0)) || (status == 1 && sends_bye == 0) || (Buggy == 1 && quit == 0 && marker == 1) { 1 } else { 0 };
                orphaned = if bye == 1 || (Buggy == 0 && status == 1 && (marker == 1 || sends_bye == 0)) || (status == 1 && sends_bye == 0) || (Buggy == 1 && quit == 0 && marker == 1) { 0 } else { shell_live };
                ever_orphaned = if bye == 1 || (Buggy == 0 && status == 1 && (marker == 1 || sends_bye == 0)) || (status == 1 && sends_bye == 0) || (Buggy == 1 && quit == 0 && marker == 1) { 0 } else { shell_live };
            }
            action ShellDies when (ending > 0 && shell_live == 1) {
                shell_live = 0;
                watch_pending = orphaned;
            }
            action KeeperSeesShellExit when (watch_pending == 1) {
                watch_pending = 0;
                orphaned = if Buggy == 1 { orphaned } else { 0 };
            }

            invariant CleanQuitNeverResurrected: ever_orphaned == 0 || quit == 0 || status == 0 || names_marker == 0;
            invariant CrashNeverClosed: judged_quit == 0 || quit == 1;
            invariant NeverListsTheDead: orphaned == 0 || shell_live == 1 || watch_pending == 1;
        }
    }
}

/// One record's crash-restore COMPANION (§6.6) across its reader, an update's two claimants, and the recovering windows offered it after the reader dies.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn keeper_companion_custody_model() -> Model {
    aterm_spec::ty_model! {
        KeeperCompanionCustody {
            const Buggy = 0;
            const MaxRecipients = 4;
            var reader = 1;
            var live = 1;
            var state = 0;
            var comp = 1;
            var done = 1;
            var attempts = 0;
            var jcap = 1;
            var next = 2;
            var dead_reader = 0;
            var outgoing = 0;
            var ocomp = 0;
            var bad_exact = 0;
            var bad_old = 0;
            var bad_poison = 0;

            action Release when (state == 0 && ((live == 1 && outgoing == 0) || (live == 0 && outgoing > 0))) {
                state = 3;
                comp = if Buggy == 3 { comp } else { 0 };
            }
            action ReaderDies when (state == 0 && live == 1) {
                live = 0;
                state = if outgoing > 0 { 0 } else { 1 };
                dead_reader = reader;
                attempts = if Buggy == 5 && outgoing > 0 && ocomp > 0 { 0 } else { if done == 1 { 0 } else { if Buggy == 1 { attempts } else { attempts + 1 } } };
                comp = if Buggy == 5 && outgoing > 0 && ocomp > 0 { ocomp } else { if Buggy == 4 { comp } else { if done == 0 && attempts > 0 { 0 } else { comp } } };
                ocomp = 0;
            }
            action SuccessorCommits when (state == 0 && live == 1 && outgoing == 0 && MaxRecipients + 1 > next) {
                outgoing = reader;
                ocomp = if done == 1 { comp } else { 0 };
                reader = next;
                next = next + 1;
                done = 0;
                jcap = 1;
            }
            action OutgoingEnds when (state == 0 && outgoing > 0) {
                outgoing = 0;
                ocomp = 0;
                state = if live == 1 { 0 } else { 1 };
            }
            action OfferNew when (state == 1 && MaxRecipients + 1 > next) {
                state = 2;
                jcap = 1;
                bad_exact = if comp > 0 && attempts == 0 && (comp > dead_reader || dead_reader > comp) { 1 } else { bad_exact };
                bad_poison = if comp > 0 && attempts > 1 { 1 } else { bad_poison };
            }
            action OfferOld when (state == 1 && MaxRecipients + 1 > next) {
                state = 2;
                jcap = 0;
                bad_old = if Buggy == 2 && comp > 0 { 1 } else { bad_old };
            }
            action RecipientDrops when (state == 2) {
                state = 1;
                next = next + 1;
                attempts = if jcap == 1 && Buggy == 1 { attempts } else { if jcap == 1 { attempts + 1 } else { attempts } };
                comp = if Buggy == 4 { comp } else { if jcap == 1 && attempts > 0 { 0 } else { comp } };
            }
            action Claim when (state == 2) {
                state = 0;
                live = 1;
                reader = next;
                next = next + 1;
                done = 0;
                comp = if jcap == 0 { 0 } else { comp };
            }
            action ReaderCompanion when (state == 0 && live == 1 && done == 0 && jcap == 1) {
                done = 1;
                attempts = 0;
                comp = reader;
            }
            action ReaderReleases when (state == 0 && live == 1 && done == 0 && jcap == 1) {
                done = 1;
                attempts = 0;
                comp = 0;
            }

            invariant ExactOnlyFromTheReader: bad_exact == 0;
            invariant NeverToAnOldWindow: bad_old == 0;
            invariant ReleasedCompanionClosed: state <= 2 || comp == 0;
            invariant PoisonBounded: bad_poison == 0;
        }
    }
}
