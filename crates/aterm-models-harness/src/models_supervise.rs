// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal state-machine models (`ty_model!`) for harness session supervision, Autopilot, and wall handling.

use aterm_spec::derive::{Liveness, Model};

/// Formal model for single-owner session claim fencing (`supervise/claim.rs`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_claim_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorClaim {
            const Buggy = 0;
            const Renew = 1;
            const Ttl = 3;
            var started = 0;
            var fenced = 1;
            var held = 0;
            var view = 0;
            var age = 0;
            var pressed_under_other = 0;

            action Hosted when (started == 0) {
                started = 1;
            }
            action External when (started == 0) {
                started = 1;
                fenced = 0;
            }
            action Tick when (started == 1 && age <= Ttl - 1) {
                age = age + 1;
            }
            action Lapse when (started == 1 && held == 1 && age == Ttl) {
                held = 0;
            }
            action OtherClaims when (started == 1 && held == 0) {
                held = 2;
            }
            action ProvenReplaces when (started == 1 && held == 1 && fenced == 1) {
                held = 2;
            }
            action OtherReleases when (started == 1 && held == 2) {
                held = 0;
            }
            action Renewal when (started == 1 && (Renew <= age || view == 3)) {
                view = if held == 2 { 2 } else { 1 };
                held = if held == 2 { 2 } else { 1 };
                age = 0;
            }
            action Unserved when (started == 1 && (Renew <= age || view == 3)) {
                view = 3;
                age = 0;
            }
            action Press when (
                started == 1 && view == 1 && (age <= Renew - 1 || Buggy == 1)
            ) {
                pressed_under_other = if held == 2 && (fenced == 0 || Buggy == 1) {
                    1
                } else {
                    pressed_under_other
                };
            }

            invariant NoPressUnderAnothersClaim: pressed_under_other == 0;
        }
    }
}

/// Formal model for focus-then-Enter selection on unnumbered dialogs (`supervise/press.rs`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_focus_choice_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorFocusChoice {
            const Buggy = 0;
            const Tries = 4;
            var focus = 0;
            var moved = 0;
            var confirmed = 0;
            var fresh = 0;
            var entered = 0;
            var entered_on_other = 0;
            var tries = 0;

            action MoveLands when (
                entered == 0 &&
                (moved == 0 || (fresh == 1 && confirmed == 0 && focus == 0 && tries <= Tries - 1))
            ) {
                tries = if moved == 1 { tries + 1 } else { tries };
                moved = 1;
                focus = 1;
                fresh = 0;
            }
            action MoveLost when (
                entered == 0 &&
                (moved == 0 || (fresh == 1 && confirmed == 0 && focus == 0 && tries <= Tries - 1))
            ) {
                tries = if moved == 1 { tries + 1 } else { tries };
                moved = 1;
                fresh = 0;
            }
            action Reread when (moved == 1 && entered == 0) {
                confirmed = focus;
                fresh = 1;
            }
            action ScreenMoves when (entered == 0) {
                focus = 0;
                fresh = 0;
            }
            action Enter when (
                entered == 0 && moved == 1 &&
                ((confirmed == 1 && fresh == 1) || Buggy == 1)
            ) {
                entered = 1;
                entered_on_other = if focus == 1 { 0 } else { 1 };
            }

            invariant NeverEnterOffTheChosenOption: entered_on_other == 0;
        }
    }
}

/// Formal model for the turn-end decision policy across Autopilot modes (`supervise/policy/turn_end.rs`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_turn_end_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorTurnEnd {
            const Buggy = 0;
            const Budget = 0;
            const Take = 2;
            const Streak = 3;
            scenario Budget = 2;
            var box_up = 0;
            var wall = 0;
            var person = 0;
            var used = 0;
            var short = 0;
            var backoff = 0;
            var pending = 0;
            var waited = 0;
            var draft = 0;
            var typed_blocked = 0;
            var typed_over = 0;
            var typed_early = 0;
            var escalated = 0;
            var latched = 0;
            var asked = 0;
            var tasked = 0;
            var typed_taskless = 0;
            var harness = 0;
            var earned = 0;
            var long = 0;
            var claim = 0;
            var other = 0;
            var need = 0;
            var rested = 0;
            var held = 0;

            action BoxAppears when (box_up == 0 && pending == 0 && other == 0) {
                box_up = 1;
            }
            action BoxLeaves when (box_up == 1) {
                box_up = 0;
            }
            action WallAppears when (
                wall == 0 && box_up == 0 && pending == 0 && backoff == 0 && other == 0
            ) {
                wall = 1;
                claim = 0;
            }
            action WallDue when (wall == 1) {
                wall = 2;
            }
            action PersonTypes when (person == 0 && pending <= 2) {
                person = 1;
            }
            action DraftLeft when (draft == 0 && pending == 0 && box_up == 0 && other == 0) {
                draft = 1;
                person = 1;
            }
            action GraceEnds when (person == 1) {
                person = 0;
            }
            action HourPasses when (used > 0 && wall == 0 && backoff == 0 && pending <= 2) {
                used = 0;
            }
            action BackoffDue when (backoff == 1 && pending == 0) {
                backoff = 0;
                earned = 0;
            }
            action HumanWork when (pending == 0 && box_up == 0 && wall == 0) {
                short = 0;
                backoff = 0;
                earned = 0;
                long = 1;
                draft = 0;
                asked = 1;
                tasked = 1;
                claim = 0;
                other = 0;
                need = 0;
                rested = 0;
                held = 0;
            }
            action OtherTurnRuns when (
                pending == 0 && box_up == 0 && wall == 0 && draft == 0 && other == 0
            ) {
                other = 1;
            }
            action HarnessTurn when (
                harness <= 1 && pending == 0 && box_up == 0 && wall == 0 && draft == 0 &&
                other == 0
            ) {
                harness = harness + 1;
                short = if Buggy == 1 && short <= Streak - 1 { short + 1 } else { short };
                backoff = if Buggy == 1 { 1 } else { backoff };
                tasked = if Buggy == 1 { 1 } else { tasked };
            }
            action HarnessTurnLong when (
                harness <= 1 && pending == 0 && box_up == 0 && wall == 0 && draft == 0 &&
                other == 0
            ) {
                harness = harness + 1;
                short = if Buggy == 1 { short } else { 0 };
                backoff = if Buggy == 1 { backoff } else { 0 };
                earned = if Buggy == 1 { earned } else { 0 };
                long = 1;
                tasked = if Buggy == 1 { 1 } else { tasked };
                claim = 0;
            }
            action Continue when (
                pending == 0 && wall == 0 && other == 0 && rested == 0 && held == 0 &&
                (tasked == 1 || Buggy == 1) &&
                ((box_up == 0 && person == 0 && draft == 0 && backoff == 0) || Buggy == 1) &&
                (Budget == 0 || used <= Budget - 1 || (Buggy == 2 && used <= Budget))
            ) {
                typed_taskless = if asked == 0 { 1 } else { typed_taskless };
                typed_blocked = if box_up == 1 || person == 1 { 1 } else { typed_blocked };
                typed_over = if draft == 1 { 1 } else { typed_over };
                typed_early = if backoff == 1 { 1 } else { typed_early };
                used = if Budget == 0 { used } else { used + 1 };
                pending = 1;
            }
            action WorkedShort when (pending == 1) {
                pending = 0;
                short = if short <= Streak - 1 { short + 1 } else { short };
                backoff = 1;
                earned = 1;
                long = 0;
                claim = 0;
            }
            action WorkedLong when (pending == 1) {
                pending = 0;
                short = 0;
                backoff = 0;
                earned = 0;
                long = 1;
                claim = 0;
            }
            action WorkedClaimsDone when (pending == 1) {
                pending = 0;
                short = if short <= Streak - 1 { short + 1 } else { short };
                backoff = 1;
                earned = 1;
                long = 0;
                claim = 1;
                rested = if Buggy == 1 { 1 } else { rested };
            }
            action WorkedNeeds when (pending == 1) {
                pending = 0;
                short = 0;
                backoff = 0;
                earned = 0;
                long = 1;
                claim = 0;
                need = 1;
                held = if Buggy == 1 { 1 } else { held };
            }
            action ReplyUnseen when (pending == 1) {
                pending = 3;
                waited = 0;
            }
            action TimePassesUnseen when (pending == 3 && waited <= Take - 1) {
                waited = waited + 1;
            }
            action DeadlineUnseen when (pending == 3 && waited == Take) {
                latched = if Buggy == 1 { 1 } else { latched };
                pending = if Buggy == 1 { 3 } else { 0 };
                short = if Buggy == 1 || short == Streak { short } else { short + 1 };
                backoff = if Buggy == 1 { backoff } else { 1 };
                earned = if Buggy == 1 { earned } else { 1 };
                long = if Buggy == 1 { long } else { 0 };
                waited = 0;
            }
            action Retry when (
                pending == 0 && wall == 2 && other == 0 && rested == 0 &&
                (tasked == 1 || Buggy == 1) &&
                ((box_up == 0 && person == 0 && draft == 0) || Buggy == 1) &&
                (Budget == 0 || used <= Budget - 1)
            ) {
                typed_taskless = if asked == 0 { 1 } else { typed_taskless };
                typed_blocked = if box_up == 1 || person == 1 { 1 } else { typed_blocked };
                typed_over = if draft == 1 { 1 } else { typed_over };
                used = if Budget == 0 { used } else { used + 1 };
                pending = 2;
            }
            action SubmitDraft when (
                pending == 0 && draft == 1 && (wall == 0 || wall == 2) && tasked == 1 &&
                other == 0 && rested == 0 && held == 0 &&
                box_up == 0 && person == 0 && backoff == 0 &&
                (Budget == 0 || used <= Budget - 1)
            ) {
                draft = 0;
                used = if Budget == 0 { used } else { used + 1 };
                pending = if wall == 2 { 2 } else { 1 };
            }
            action RetryTaken when (pending == 2) {
                pending = 0;
                wall = 0;
            }
            action RetryHitsTheWall when (pending == 2) {
                pending = 0;
                wall = 1;
            }
            action Escalate when (
                pending == 0 && box_up == 0 && person == 0 && tasked == 1 &&
                other == 0 && rested == 0 && (
                    (Budget > 0 && used == Budget && backoff == 0 && (wall == 0 || wall == 2)) ||
                    (Buggy == 1 && short > 1)
                )
            ) {
                escalated = if Budget == 0 { 1 } else { escalated };
            }

            invariant NeverTypeUnderABoxOrAPerson: typed_blocked == 0;
            invariant NeverTypeOverADraft: typed_over == 0;
            invariant NeverTypeBeforeItsBackoff: typed_early == 0;
            invariant NeverEscalateUnderFullPower: escalated == 0;
            invariant WithinTheCap: Budget == 0 || used <= Budget;
            invariant NoSilentLatch: latched == 0;
            invariant NeverTypeIntoATasklessSession: typed_taskless == 0;
            invariant NeverBackOffForTheHarness: backoff <= earned;
            invariant NeverBackOffAfterRealWork: backoff == 0 || long == 0;
            invariant CruiseNeverRests: rested == 0;
            invariant ANeedNeverBlocks: held == 0;
        }
    }
}

/// Formal model for Autopilot Cruise continuation and verification prompts.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_cruise_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorCruise {
            const Buggy = 0;
            var claim = 0;
            var pending = 0;
            var unchecked = 0;
            var walled = 0;
            var asked = 0;
            var managed = 0;
            var short = 0;
            var backoff = 0;
            var waited = 0;
            var rushed = 0;
            var bg = 0;
            var open = 0;
            var typed_busy = 0;
            var rested = 0;

            action Tasked when (managed == 0) {
                managed = 1;
            }
            action SeedOldDone when (Buggy == 1 && pending == 0 && bg == 0 && rested == 0) {
                rested = 1;
            }
            action OtherTurnWork when (pending == 0 && bg == 0) {
                claim = 0;
                open = 0;
                walled = 0;
                asked = 0;
                short = 0;
                backoff = 0;
            }
            action OtherTurnClaim when (pending == 0 && bg == 0) {
                claim = 1;
                open = 0;
                walled = 0;
                asked = 0;
                short = 0;
                backoff = 0;
            }
            action OtherTurnOpenClaim when (pending == 0 && bg == 0) {
                claim = 1;
                open = 1;
                walled = 0;
                asked = 0;
                short = 0;
                backoff = 0;
            }
            action OtherTurnDirtyClaim when (pending == 0 && bg == 0) {
                claim = 1;
                open = 2;
                walled = 0;
                asked = 0;
                short = 0;
                backoff = 0;
            }
            action OtherTurnWall when (pending == 0 && bg == 0) {
                claim = 0;
                open = 0;
                walled = 1;
                asked = 0;
                backoff = if short > 0 { 1 } else { 0 };
            }
            action OtherTurnClaimBg when (pending == 0 && bg == 0) {
                claim = 1;
                open = 0;
                walled = 0;
                asked = 0;
                short = 0;
                backoff = 0;
                bg = 1;
            }
            action BackgroundEnds when (bg == 1) {
                bg = 0;
            }
            action FinishOverBackground when (Buggy == 1 && bg == 1 && rested == 0) {
                rested = 1;
            }
            action WallAct when (pending == 0 && bg == 0 && walled == 1 && rested == 0) {
                walled = 0;
                open = 0;
                pending = 3;
            }
            action Continue when (
                pending == 0 && bg == 0 && walled == 0 && asked == 0 && rested == 0 &&
                (backoff == 0 || Buggy == 1) && (claim == 0 || Buggy == 1)
            ) {
                unchecked = if claim == 1 { 1 } else { unchecked };
                open = 0;
                pending = 1;
            }
            action Check when (
                pending == 0 && (bg == 0 || Buggy == 1) && walled == 0 && claim == 1 &&
                rested == 0 &&
                (backoff == 0 || (managed == 1 && short <= 3 && Buggy == 0))
            ) {
                rushed = if backoff == 1 && short > 3 { 1 } else { rushed };
                typed_busy = if bg == 1 { 1 } else { typed_busy };
                open = 0;
                pending = 2;
            }
            action Answer when (
                pending == 0 && bg == 0 && walled == 0 && asked == 1 && claim == 0 &&
                rested == 0 &&
                (backoff == 0 || (managed == 1 && (short <= 3 || Buggy == 1)))
            ) {
                rushed = if backoff == 1 && short > 3 { 1 } else { rushed };
                asked = 0;
                pending = 4;
            }
            action BackoffDue when (
                pending == 0 && bg == 0 && walled == 0 && backoff == 1 && (
                    (claim == 0 && asked == 0) ||
                    (claim == 1 && (managed == 0 || short > 3 || Buggy == 1)) ||
                    (asked == 1 && (managed == 0 || (short > 3 && Buggy == 0)))
                )
            ) {
                waited = if managed == 1 && short <= 3 && (claim == 1 || asked == 1) {
                    1
                } else {
                    waited
                };
                backoff = 0;
            }
            action YieldWork when (pending > 0 && bg == 0) {
                pending = 0;
                claim = 0;
                open = 0;
                short = if pending == 3 { short } else { 0 };
                backoff = if pending == 3 && short > 0 { 1 } else { 0 };
            }
            action YieldClaim when (pending > 0 && bg == 0) {
                rested = if Buggy == 1 && pending == 2 { 1 } else { rested };
                claim = 1;
                open = 0;
                short = if pending == 3 { short } else if short > 3 { 4 } else { short + 1 };
                backoff = if pending == 3 && short == 0 { 0 } else { 1 };
                pending = 0;
            }
            action YieldOpenClaim when (pending > 0 && bg == 0) {
                claim = 1;
                open = 1;
                short = if pending == 3 { short } else if short > 3 { 4 } else { short + 1 };
                backoff = if pending == 3 && short == 0 { 0 } else { 1 };
                pending = 0;
            }
            action YieldDirtyClaim when (pending > 0 && bg == 0) {
                claim = 1;
                open = 2;
                short = if pending == 3 { short } else if short > 3 { 4 } else { short + 1 };
                backoff = if pending == 3 && short == 0 { 0 } else { 1 };
                pending = 0;
            }
            action YieldAsk when (pending > 0 && bg == 0) {
                claim = 0;
                asked = 1;
                open = 0;
                short = if pending == 3 { short } else if short > 3 { 4 } else { short + 1 };
                backoff = if pending == 3 && short == 0 { 0 } else { 1 };
                pending = 0;
            }
            action YieldWall when (pending > 0 && bg == 0) {
                pending = 0;
                walled = 1;
                asked = 0;
                open = 0;
                short = if pending == 3 { short } else { 0 };
                backoff = if pending == 3 && short > 0 { 1 } else { 0 };
            }
            action YieldClaimBg when (pending > 0 && bg == 0) {
                claim = 1;
                open = 0;
                asked = 0;
                short = if pending == 3 { short } else if short > 3 { 4 } else { short + 1 };
                backoff = if pending == 3 && short == 0 { 0 } else { 1 };
                pending = 0;
                bg = 1;
            }

            invariant CruiseNeverRests: rested == 0;
            invariant AClaimIsDefended: unchecked == 0;
            invariant AManagerIsNeverKeptWaiting: waited == 0;
            invariant AWorkerThatKeepsAskingWaitsTheBackOff: rushed == 0;
            invariant TheBusyScreenGetsNoAct: typed_busy == 0;
        }
    }
}

/// Formal model for Autopilot Land convergence to a clean pushed `origin/main`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_land_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorLand {
            const Buggy = 0;
            const StaleHold = 0;
            var typed = 0;
            var pending = 0;
            var said = 0;
            var need = 0;
            var dirty = 0;
            var walled = 0;
            var cut = 0;
            var asked = 0;
            var short = 0;
            var backoff = 0;
            var landed = 0;
            var left = 0;
            var other = 0;
            var told = 0;
            var nudges = 0;
            var quiet = 0;
            var held = 0;
            var typed_landed = 0;
            var judged = 0;
            var relanded = 0;
            var hold = 0;
            var hlegs = 0;

            action Instruction when (
                pending == 0 && walled == 0 && asked == 0 && typed == 0 && backoff == 0 &&
                landed == 0 && left == 0 && other == 0 && quiet == 0 && held == 0
            ) {
                typed = 1;
                pending = 1;
                relanded = 0;
                hold = 1;
                hlegs = 0;
            }
            action Nudge when (
                pending == 0 && walled == 0 && asked == 0 && typed == 1 && backoff == 0 &&
                (landed == 0 || Buggy == 1) && left == 0 && other == 0 && quiet == 0 &&
                held == 0
            ) {
                typed_landed = if landed == 1 { 1 } else { typed_landed };
                nudges = if Buggy == 1 && nudges <= 1 { nudges + 1 } else { nudges };
                pending = 1;
                relanded = 0;
                hold = if landed == 1 { 0 } else if judged == 1 { 2 } else { 1 };
                hlegs = if landed == 1 { 0 } else { hlegs };
            }
            action Answer when (
                pending == 0 && walled == 0 && asked == 1 && backoff == 0 && landed == 0 &&
                left == 0 && other == 0 && quiet == 0 && held == 0
            ) {
                asked = 0;
                pending = if typed == 1 { 1 } else { 2 };
                relanded = 0;
                hold = if typed == 0 { 0 } else if judged == 1 { 2 } else { 1 };
                hlegs = if typed == 0 { 0 } else { hlegs };
            }
            action WallAct when (pending == 0 && walled == 1 && landed == 0 && left == 0) {
                walled = 0;
                pending = 3;
            }
            action BackoffDue when (
                pending == 0 && walled == 0 && backoff == 1 && landed == 0 && left == 0 &&
                other == 0
            ) {
                backoff = 0;
            }
            action YieldWork when (pending > 0) {
                said = 0;
                dirty = 0;
                asked = 0;
                landed = 0;
                told = 0;
                cut = 0;
                short = if pending == 3 { short } else { 0 };
                backoff = if pending == 3 { short } else { 0 };
                pending = 0;
                judged = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { judged };
                hold = if StaleHold == 1 || Buggy == 2 {
                    hold
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    2
                } else {
                    hold
                };
                hlegs = if StaleHold == 1 || Buggy == 2 {
                    hlegs
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    8
                } else {
                    hlegs
                };
            }
            action YieldAsk when (pending > 0) {
                said = 0;
                dirty = 0;
                asked = 1;
                landed = 0;
                told = 0;
                cut = 0;
                short = if pending == 3 { short } else { 1 };
                backoff = if pending == 3 { short } else { 1 };
                pending = 0;
                judged = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { judged };
                hold = if StaleHold == 1 || Buggy == 2 {
                    hold
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    2
                } else {
                    hold
                };
                hlegs = if StaleHold == 1 || Buggy == 2 {
                    hlegs
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    8
                } else {
                    hlegs
                };
            }
            action YieldWall when (pending > 0) {
                walled = 1;
                cut = if pending == 1 { 1 } else if pending == 3 { cut } else { 0 };
                dirty = 0;
                asked = 0;
                landed = 0;
                told = 0;
                short = if pending == 3 { short } else { 0 };
                backoff = if pending == 3 { short } else { 0 };
                pending = 0;
            }
            action YieldClaim when (pending > 0) {
                said = 1;
                dirty = 0;
                asked = 0;
                landed = if Buggy == 1 && (pending == 1 || (pending == 3 && cut == 1)) {
                    1
                } else {
                    0
                };
                told = if Buggy == 1 && (pending == 1 || (pending == 3 && cut == 1)) {
                    1
                } else {
                    0
                };
                cut = 0;
                short = if pending == 3 { short } else { 1 };
                backoff = if pending == 3 { short } else { 1 };
                pending = 0;
                judged = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { judged };
                hold = if StaleHold == 1 || Buggy == 2 {
                    hold
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    2
                } else {
                    hold
                };
                hlegs = if StaleHold == 1 || Buggy == 2 {
                    hlegs
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    8
                } else {
                    hlegs
                };
            }
            action YieldDefense when (pending > 0) {
                said = 2;
                need = 0;
                dirty = 0;
                asked = 0;
                landed = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { 0 };
                told = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { 0 };
                cut = 0;
                short = if pending == 3 { short } else { 1 };
                backoff = if pending == 3 { short } else { 1 };
                pending = 0;
                judged = if pending == 1 || (pending == 3 && cut == 1) { 0 } else { judged };
                relanded = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { relanded };
                hold = if StaleHold == 1 || Buggy == 2 {
                    hold
                } else if typed == 0 || relanded == 1 || pending == 1 || (pending == 3 && cut == 1) {
                    0
                } else {
                    hold
                };
                hlegs = if StaleHold == 1 || Buggy == 2 {
                    hlegs
                } else if typed == 0 || relanded == 1 || pending == 1 || (pending == 3 && cut == 1) {
                    0
                } else {
                    hlegs
                };
            }
            action YieldDefenseDirty when (pending > 0) {
                said = 2;
                need = 0;
                dirty = 1;
                asked = 0;
                landed = if Buggy == 1 && (pending == 1 || (pending == 3 && cut == 1)) {
                    1
                } else {
                    0
                };
                told = if Buggy == 1 && (pending == 1 || (pending == 3 && cut == 1)) {
                    1
                } else {
                    0
                };
                cut = 0;
                short = if pending == 3 { short } else { 1 };
                backoff = if pending == 3 { short } else { 1 };
                pending = 0;
                judged = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { judged };
                hold = if StaleHold == 1 || Buggy == 2 {
                    hold
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    2
                } else {
                    hold
                };
                hlegs = if StaleHold == 1 || Buggy == 2 {
                    hlegs
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    2
                } else {
                    hlegs
                };
            }
            action YieldDefenseNeeds when (pending > 0) {
                said = 2;
                need = 1;
                dirty = 0;
                asked = 0;
                landed = if Buggy == 1 && (pending == 1 || (pending == 3 && cut == 1)) {
                    1
                } else {
                    0
                };
                told = if Buggy == 1 && (pending == 1 || (pending == 3 && cut == 1)) {
                    1
                } else {
                    0
                };
                cut = 0;
                short = if pending == 3 { short } else { 1 };
                backoff = if pending == 3 { short } else { 1 };
                pending = 0;
                judged = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { judged };
                hold = if StaleHold == 1 || Buggy == 2 {
                    hold
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    2
                } else {
                    hold
                };
                hlegs = if StaleHold == 1 || Buggy == 2 {
                    hlegs
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    1
                } else {
                    hlegs
                };
            }
            action YieldOpenDefense when (pending > 0) {
                said = 3;
                need = 0;
                dirty = 0;
                asked = 0;
                landed = 0;
                told = 0;
                cut = 0;
                short = if pending == 3 { short } else { 1 };
                backoff = if pending == 3 { short } else { 1 };
                pending = 0;
                judged = if pending == 1 || (pending == 3 && cut == 1) { 1 } else { judged };
                hold = if StaleHold == 1 || Buggy == 2 {
                    hold
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    2
                } else {
                    hold
                };
                hlegs = if StaleHold == 1 || Buggy == 2 {
                    hlegs
                } else if typed == 0 || relanded == 1 {
                    0
                } else if pending == 1 || (pending == 3 && cut == 1) {
                    4
                } else {
                    hlegs
                };
            }
            action OtherTurnRuns when (pending == 0 && walled == 0 && other == 0) {
                other = 1;
                told = if Buggy == 1 { told } else { 0 };
                judged = 0;
                hold = if typed == 1 && relanded == 0 { 1 } else { 0 };
                hlegs = 0;
            }
            action HumanWork when (pending == 0 && walled == 0) {
                said = 0;
                need = 0;
                dirty = 0;
                asked = 0;
                cut = 0;
                short = 0;
                backoff = 0;
                landed = 0;
                left = 0;
                other = 0;
                told = 0;
                judged = 0;
                hold = if typed == 1 && relanded == 0 { 1 } else { 0 };
                hlegs = 0;
            }
            action OtherTurnDefense when (pending == 0 && walled == 0) {
                said = 2;
                need = 0;
                dirty = 0;
                asked = 0;
                cut = 0;
                short = 0;
                backoff = 0;
                landed = 0;
                left = 0;
                other = 0;
                told = 0;
                judged = 0;
                hold = if typed == 1 && relanded == 0 { 1 } else { 0 };
                hlegs = 0;
            }
            action PersonExchange when (
                pending == 0 && walled == 0 && left == 0 && (said > 0 || landed == 1)
            ) {
                said = 0;
                need = 0;
                dirty = 0;
                asked = 0;
                cut = 0;
                backoff = 0;
                landed = 0;
                left = 1;
                other = 0;
                told = 2;
                judged = 0;
                hold = 0;
                hlegs = 0;
            }
            action OtherTurnWall when (pending == 0 && walled == 0) {
                said = 0;
                dirty = 0;
                asked = 0;
                walled = 1;
                cut = 0;
                backoff = short;
                landed = 0;
                left = 0;
                other = 0;
                told = 0;
                judged = 0;
                hold = if typed == 1 && relanded == 0 { 1 } else { 0 };
                hlegs = 0;
            }
            action SeedLanded when (
                pending == 0 && (walled == 0 || Buggy == 1) && need == 0 && left == 0 &&
                other == 0
            ) {
                landed = 1;
                said = 2;
                dirty = 0;
                told = 1;
                judged = 0;
                relanded = 1;
                hold = 0;
                hlegs = 0;
            }
            action SpentAfterTwo when (
                Buggy == 1 && nudges == 2 && pending == 0 && walled == 0 && landed == 0 &&
                left == 0 && other == 0 && quiet == 0
            ) {
                quiet = 1;
            }
            action AskHolds when (
                Buggy == 1 && need == 1 && pending == 0 && walled == 0 && landed == 0 &&
                left == 0 && other == 0 && held == 0
            ) {
                held = 1;
            }

            invariant LandNeverGoesQuiet: quiet == 0;
            invariant LandedOnlyByCleanDefense:
                landed == 0 || (said == 2 && need == 0 && dirty == 0);
            invariant ANeedNeverBlocks: held == 0;
            invariant NeverTypeIntoALandedTask: typed_landed == 0;
            invariant AWallIsNeverLeftOnALandedTask: walled == 0 || landed == 0;
            invariant TheHostIsToldTheTasksFace:
                (other == 1 && told == 0) ||
                (other == 0 && left == 1 && told == 2) ||
                (other == 0 && left == 0 && landed == 1 && told == 1) ||
                (other == 0 && left == 0 && landed == 0 && told == 0);
            invariant TheHostIsToldWhatHoldsTheLanding:
                (hold == 0 && hlegs == 0 &&
                    (typed == 0 || relanded == 1 || (other == 0 && (landed == 1 || left == 1)))) ||
                (typed == 1 && relanded == 0 && (other == 1 || (landed == 0 && left == 0)) &&
                    ((hold == 1 && judged == 0 && hlegs == 0) ||
                        (hold == 2 && judged == 1 &&
                            (cut == 1 ||
                                hlegs == (if said <= 1 { 8 } else { 0 }) +
                                    (if said == 3 { 4 } else { 0 }) +
                                    (if said == 2 && need == 1 { 1 } else { 0 }) +
                                    (if dirty == 1 { 2 } else { 0 })))));
        }
    }
}

/// Liveness specification asserting Autopilot Land never rests short of landing.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(supervisor_land_model)]
pub fn supervisor_land_liveness() -> Liveness {
    use aterm_spec::derive::{eq, int, or_, var};
    Liveness {
        name: "LandNeverRestsShortOfItsLanding",
        goal: or_(
            or_(
                or_(eq(var("pending"), int(1)), eq(var("pending"), int(2))),
                eq(var("pending"), int(3)),
            ),
            or_(
                or_(eq(var("landed"), int(1)), eq(var("left"), int(1))),
                eq(var("other"), int(1)),
            ),
        ),
        weak: vec!["Instruction", "Nudge", "Answer", "WallAct", "BackoffDue"],
        strong: vec![],
        mutants: vec!["SpentAfterTwo", "AskHolds"],
    }
}

/// Formal model for the supervisor stall-remedy escalation ladder (`supervise/stall.rs`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_stall_remedy_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorStallRemedy {
            const Buggy = 0;
            var stalled = 0;
            var aged = 0;
            var off = 0;
            var hand = 0;
            var host = 1;
            var termed = 0;
            var terms = 0;
            var wrong = 0;
            var killed = 0;
            var beside = 0;
            var rest = 0;
            var withheld = 0;

            action Freeze when (stalled == 0) {
                stalled = 1;
            }
            action Stop when (stalled == 0) {
                stalled = 2;
            }
            action Age when (stalled > 0 && aged == 0) {
                aged = 1;
            }
            action Thaw when (stalled > 0) {
                stalled = 0;
                aged = 0;
                termed = 0;
                terms = 0;
            }
            action HandOn when (hand == 0) {
                hand = 1;
            }
            action HandOff when (hand == 1) {
                hand = 0;
            }
            action HostFlips when (termed == 0) {
                host = if host == 1 { 0 } else { 1 };
            }
            action BoundFlips when (termed == 0) {
                off = if off == 1 { 0 } else { 1 };
            }
            action BesideFlips when (termed == 0) {
                beside = if beside == 1 { 0 } else { 1 };
            }
            action RestFlips when (termed == 0) {
                rest = if rest == 1 { 0 } else { 1 };
            }
            action Term when (stalled == 1 && aged == 1 && off == 0 && host == 1
                && hand == 0 && termed == 0) {
                terms = terms + 1;
                termed = 1;
            }
            action WaitsForTheRest when (Buggy == 1 && stalled == 1 && aged == 1 && off == 0
                && host == 1 && hand == 0 && termed == 0 && beside == 1 && rest == 0) {
                withheld = 1;
            }
            action TermBlind when (Buggy == 1 && stalled == 1 && aged == 1 && off == 0
                && host == 1 && hand == 1 && termed == 0) {
                wrong = 1;
                terms = terms + 1;
                termed = 1;
            }
            action TermAgain when (Buggy == 1 && stalled == 1 && termed == 1 && terms <= 1) {
                terms = terms + 1;
            }
            action Kill when (Buggy == 1 && termed == 1 && stalled == 1) {
                killed = 1;
            }

            invariant NeverUnderAHand: wrong == 0;
            invariant OneTermAnEpisode: terms <= 1;
            invariant NeverAKill: killed == 0;
            invariant TheStallAloneDecides: withheld == 0;
        }
    }
}

/// Formal model for re-deciding a turn end after a host hold is released.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_host_turn_end_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorHostTurnEnd {
            const Buggy = 0;
            var point = 0;
            var owns = 0;
            var stranded = 0;
            var stale = 0;

            action TurnEnds when (point == 0) {
                point = if owns == 1 { 2 } else { 1 };
            }
            action HostKeeps when (point == 1 || point == 2) {
                owns = 1;
                point = 2;
            }
            action HostLetsGo when (point == 1 || point == 2) {
                owns = 0;
                point = 2;
            }
            action HostMoves when (point == 1 || point == 2) {
                point = 3;
            }
            action Lapse when (owns == 1) {
                owns = 0;
            }
            action Redecide when (owns == 0 && (point == 2 || (point == 3 && Buggy == 1))) {
                stale = if point == 3 { 1 } else { stale };
                point = 1;
            }
            action LoopWaits when (point > 0 && (point == 1 || point == 3 || owns == 1 || Buggy == 1)) {
                stranded = if point == 2 && owns == 0 { 1 } else { stranded };
            }
            action TurnRuns when (point > 0) {
                point = 0;
            }

            invariant NoTurnEndLeftToNobody: stranded == 0;
            invariant NeverDecideAPointAStepMoved: stale == 0;
        }
    }
}

/// Formal model ensuring human-grace expiry over a READY reply triggers upgrade restart rather than continuation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn upgrade_attended_turn_end_model() -> Model {
    aterm_spec::ty_model! {
        UpgradeAttendedTurnEnd {
            const Buggy = 0;
            const Every = 0;
            const Stretch = 0;
            const Bound = 3;
            const Looks = 7;
            var point = 0;
            var person = 1;
            var owns = 0;
            var looked = 0;
            var count = 0;
            var waits = 0;
            var here = 0;
            var over = 0;
            var steps = 0;

            action HostWaitsBefore when (point == 0 && steps <= Looks) {
                steps = steps + 1;
                waits = waits + 1;
                count = 0;
            }
            action HostHoldsBefore when (point == 0 && person == 1 && steps <= Looks) {
                steps = steps + 1;
                waits = waits + 1;
                count = count + 1;
            }
            action ReadyArrives when (point == 0) {
                point = 1;
                count = if Stretch == 1 { count } else { 0 };
            }
            action HostLooks when (point == 1 && person == 1 && steps <= Looks) {
                steps = steps + 1;
                looked = 1;
                owns = if Buggy == 1 {
                    0
                } else {
                    if Every == 1 {
                        if waits <= Bound { 1 } else { 0 }
                    } else {
                        if count <= Bound { 1 } else { 0 }
                    }
                };
                count = count + 1;
                waits = waits + 1;
                here = here + 1;
            }
            action HostActs when (point == 1 && person == 0) {
                looked = 1;
                owns = 0;
                point = 2;
            }
            action PersonTouches when (point <= 1 && person == 0 && steps <= Looks) {
                steps = steps + 1;
                person = 1;
            }
            action GraceLapses when (person == 1) {
                person = 0;
            }
            action LoopContinues when (point == 1 && looked == 1 && person == 0 && owns == 0) {
                over = if here <= Bound + 1 { 1 } else { over };
                point = 2;
            }

            invariant NeverContinueOverAReadyHeldByAPerson: over == 0;
        }
    }
}

/// Formal model ensuring a completed supervisor action never blocks a host break offer.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_break_offer_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorBreakOffer {
            const Buggy = 0;
            const Judged = 0;
            const Blind = 0;
            const Acts = 2;
            var screen = 0;
            var act = 0;
            var acts = 0;
            var stacked = 0;
            var withheld = 0;

            action LoopActs when (screen == 0 && act == 0 && acts <= Acts - 1) {
                act = 1;
                acts = acts + 1;
            }
            action LiveTurn when (screen == 0 || screen == 2) {
                screen = 1;
                act = if act == 1 { 2 } else { act };
            }
            action BreakSettles when (screen <= 1) {
                screen = 2;
            }
            action PointComes when (screen > 0) {
                screen = 0;
                act = 0;
            }
            action Offer when (
                screen == 2 && (act == 0 || act == 2 || Blind == 1 || Buggy == 1) && (act <= 1 || Judged == 0)
            ) {
                stacked = if act == 1 { 1 } else { stacked };
            }
            action Withhold when (
                screen == 2 && (act == 1 || (act == 2 && (Judged == 1 || Buggy == 1)))
            ) {
                withheld = if act == 2 { 1 } else { withheld };
            }

            invariant NeverStackOnAnUntakenAct: stacked == 0;
            invariant NoBreakWithheldAfterTheActWasTaken: withheld == 0;
        }
    }
}

/// Formal model for upgrade point ownership across the quiet settle window.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn upgrade_settle_turn_end_model() -> Model {
    aterm_spec::ty_model! {
        UpgradeSettleTurnEnd {
            const Buggy = 0;
            const Carry = 0;
            const Every = 0;
            const Bound = 2;
            const Steps = 5;
            var point = 0;
            var noticed = 0;
            var waits = 0;
            var looks = 0;
            var owns = 0;
            var over = 0;
            var steps = 0;

            action SettleBefore when (point == 0 && noticed == 0 && steps <= Steps - 1) {
                steps = steps + 1;
                waits = waits + 1;
            }
            action OtherWait when (point == 0 && steps <= Steps - 1) {
                steps = steps + 1;
                waits = if (Every == 1 || Buggy == 1) { waits + 1 } else { waits };
            }
            action NoticeAtIdle when (point == 0 && steps <= Steps - 1) {
                steps = steps + 1;
                noticed = 1;
                waits = 0;
            }
            action NoticeAtBreak when (point == 0 && steps <= Steps - 1) {
                steps = steps + 1;
                noticed = 1;
                waits = if (Carry == 1 || Buggy == 1) { waits } else { 0 };
            }
            action ReadyArrives when (point == 0 && noticed == 1) {
                point = 1;
            }
            action HostSettles when (point == 1 && looks <= Bound) {
                owns = if waits <= Bound - 1 { 1 } else { 0 };
                waits = waits + 1;
                looks = looks + 1;
            }
            action HostActs when (point == 1 && looks > 0) {
                owns = 0;
                point = 2;
            }
            action LoopContinues when (point == 1 && looks > 0 && owns == 0) {
                over = if looks <= Bound { 1 } else { over };
                point = 2;
            }

            invariant NeverContinueOverASettlingReady: over == 0;
        }
    }
}

/// Formal model for bounded point ownership when an upgrade notice is refused by its fence.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn upgrade_refused_turn_end_model() -> Model {
    aterm_spec::ty_model! {
        UpgradeRefusedTurnEnd {
            const Buggy = 0;
            const Renew = 0;
            const Shared = 0;
            const Stale = 0;
            const Bound = 3;
            const Steps = 8;
            var point = 1;
            var due = 1;
            var count = 0;
            var refusals = 0;
            var owns = 0;
            var looked = 0;
            var over = 0;
            var steps = 0;

            action SettleBefore when (point == 1 && due == 1 && steps <= Steps - 1) {
                steps = steps + 1;
                looked = 0;
                owns = 0;
                count = if (Shared == 1 || Buggy == 1) { count + 1 } else { count };
            }
            action Refused when (point == 1 && due == 1 && steps <= Steps - 1) {
                steps = steps + 1;
                looked = 1;
                owns = if (Buggy == 0 && count <= Bound - 1) { 1 } else { 0 };
                count = count + 1;
                refusals = refusals + 1;
            }
            action Types when (point == 1 && due == 1) {
                looked = 1;
                due = 0;
                owns = 1;
                count = 0;
                refusals = 0;
            }
            action OwnerWord when (due == 1 && steps <= Steps - 1) {
                steps = steps + 1;
                looked = 0;
                owns = 0;
                count = if (Stale == 1 || Buggy == 1) { count } else { 0 };
                refusals = 0;
            }
            action LoopContinues when (point == 1 && looked == 1 && owns == 0) {
                over = if (due == 1 && refusals <= Bound) { 1 } else { over };
                point = 0;
            }
            action PointComes when (point == 0 && steps <= Steps - 1) {
                steps = steps + 1;
                point = 1;
                looked = 0;
                owns = 0;
                count = if Renew == 1 { 0 } else { count };
            }

            invariant NeverContinueOverARefusedNoticeInItsLooks: over == 0;
        }
    }
}

/// Formal model for answering Claude Code question dialogs with free-text notes (`answer-recommended@v1`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_question_text_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorQuestionText {
            const Buggy = 0;
            const Tabs = 2;
            const PersonFence = 1;
            const TakenBeforeStill = 1;
            const Review = 1;
            const MaxRetry = 2;
            const Text = 1;
            const Multi = 0;
            const OwnTextRemembered = 1;
            const EnterWhereTyped = 0;
            var tab = 0;
            var focus = 0;
            var row = 0;
            var typed = 0;
            var q1 = 0;
            var q2 = 0;
            var quiet = 1;
            var fresh = 0;
            var seen_tab = 0;
            var seen_focus = 0;
            var seen_row = 0;
            var seen_typed = 0;
            var seen_quiet = 1;
            var touched = 0;
            var mine = 0;
            var stilled = 0;
            var retried = 0;
            var in_field = 0;
            var stale = 0;
            var cancelled = 0;
            var composer = 0;
            var opened = 1;
            var own = 0;
            var seen_own = 0;
            var astray = 0;
            var fallback = 0;
            var seen_fallback = 0;
            var wrote = 0;
            var twice = 0;
            var exact = 0;
            var seen_exact = 0;

            action PersonKey when (tab <= Tabs && q2 == 0) {
                q1 = if q1 == 0 { 1 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 1 };
                quiet = 0;
                touched = 1;
            }
            action QuietReturns when (
                quiet == 0 && (q1 == 0 || 2 <= q1) && (q2 == 0 || 2 <= q2)
            ) {
                quiet = 1;
            }

            action PersonMovesOff when (q1 == 1 && tab <= Tabs) {
                focus = 1;
                row = 1 - row;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action PersonMovesOnto when (q1 == 1 && tab <= Tabs) {
                focus = 0;
                row = 1 - row;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action PersonTypes when (
                q1 == 1 && tab <= Tabs - 1 && (focus == 1 || (Text == 1 && fallback == 0))
            ) {
                typed = 1;
                own = 0;
                exact = 0;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action PersonTypesPolicyText when (
                Text == 1 && q1 == 1 && tab <= Tabs - 1 && (focus == 1 || fallback == 0)
            ) {
                typed = 1 - wrote;
                own = wrote;
                exact = 1 - wrote;
                focus = if wrote == 1 && Multi == 1 { 1 } else { focus };
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action PersonAnswers when (q1 == 1 && tab <= Tabs) {
                tab = if tab == Tabs - 1 && Review == 0 { Tabs + 1 } else { tab + 1 };
                focus = 0;
                row = 0;
                typed = 0;
                own = 0;
                exact = 0;
                fallback = 0;
                wrote = 0;
                q1 = q2;
                q2 = 0;
                fresh = 0;
                opened = 1;
            }
            action PersonKeyIgnored when (q1 == 1) {
                q1 = q2;
                q2 = 0;
            }
            action MoveLands when (q1 == 2 && tab <= Tabs) {
                focus = 1 - focus;
                row = 1 - row;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action MoveShort when (q1 == 2 && tab <= Tabs && focus == 1) {
                row = 1 - row;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action EnterTaken when (3 <= q1 && q1 <= 3 + Tabs && tab <= Tabs) {
                stale = if q1 == 3 + tab { stale } else { 1 };
                in_field = if tab <= Tabs - 1 && (
                    focus == 1 || (Text == 1 && fallback == 0 && (own == 0 || typed == 1))
                ) { 1 } else { in_field };
                cancelled = if tab == Tabs && focus == 1 { 1 } else { cancelled };
                tab = if tab == Tabs - 1 && Review == 0 { Tabs + 1 } else { tab + 1 };
                focus = 0;
                row = 0;
                typed = 0;
                own = 0;
                exact = 0;
                fallback = 0;
                wrote = 0;
                q1 = q2;
                q2 = 0;
                fresh = 0;
                opened = 1;
            }
            action TextLands when (4 + Tabs <= q1 && tab <= Tabs) {
                stale = if q1 == 4 + Tabs + tab { stale } else { 1 };
                astray = if q1 == 4 + Tabs + tab && tab <= Tabs - 1 && focus == 0
                    && typed == 0 && own == 0 { astray } else { 1 };
                own = if q1 == 4 + Tabs + tab && tab <= Tabs - 1 && focus == 0
                    && typed == 0 && own == 0 { 1 } else { 0 };
                typed = if q1 == 4 + Tabs + tab && tab <= Tabs - 1 && focus == 0
                    && typed == 0 && own == 0 { typed } else { 1 };
                focus = if q1 == 4 + Tabs + tab && tab <= Tabs - 1 && focus == 0
                    && typed == 0 && own == 0 && Multi == 1 { 1 } else { focus };
                exact = 0;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action TextPartly when (
                q1 == 4 + Tabs + tab && tab <= Tabs - 1 && focus == 0 &&
                typed == 0 && own == 0
            ) {
                typed = 1;
                own = 0;
                exact = 0;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action FitLostOnIt when (
                Text == 1 && fallback == 0 && tab <= Tabs - 1 && wrote == 0 &&
                own == 0 && typed == 0 && q1 == 0 && q2 == 0
            ) {
                fallback = 1;
                focus = 0;
                fresh = 0;
            }
            action FitLostOffIt when (
                Text == 1 && fallback == 0 && tab <= Tabs - 1 && wrote == 0 &&
                own == 0 && typed == 0 && q1 == 0 && q2 == 0
            ) {
                fallback = 1;
                focus = 1;
                fresh = 0;
            }
            action OpensOffChoice when (opened == 1 && tab <= Tabs - 1) {
                focus = 1;
                row = 1 - row;
                fresh = 0;
            }
            action KeyRefused when (2 <= q1) {
                composer = if Tabs + 1 <= tab { 1 } else { composer };
                q1 = q2;
                q2 = 0;
            }

            action Read when (tab <= Tabs) {
                fresh = 1;
                opened = 0;
                seen_tab = tab;
                seen_focus = focus;
                seen_row = row;
                seen_typed = typed;
                seen_own = own;
                seen_exact = exact;
                seen_fallback = fallback;
                seen_quiet = quiet;
                touched = 0;
                mine = if mine == 1 && tab == seen_tab && focus == seen_focus
                    && row == seen_row && typed == seen_typed
                    && own == seen_own { 1 } else { 0 };
                stilled = if mine == 1 && tab == seen_tab && focus == seen_focus
                    && row == seen_row && typed == seen_typed
                    && own == seen_own { stilled } else { 0 };
                retried = if mine == 1 && tab == seen_tab && focus == seen_focus
                    && row == seen_row && typed == seen_typed
                    && own == seen_own { retried } else { 0 };
            }
            action StillScreen when (
                mine == 1 && stilled == 0 && fresh == 1 &&
                (TakenBeforeStill == 0 || (q1 <= 1 && q2 <= 1))
            ) {
                stilled = 1;
            }
            action HarnessEnter when (
                fresh == 1 && q2 == 0 && seen_tab <= Tabs &&
                (seen_typed == 0 || (OwnTextRemembered == 0 && seen_exact == 1)) &&
                (seen_focus == 0 || (EnterWhereTyped == 1 && seen_own == 1)) &&
                (Text == 0 || seen_fallback == 1 || seen_tab == Tabs || seen_own == 1 ||
                    (OwnTextRemembered == 0 && seen_exact == 1)) &&
                (Buggy == 1 || (
                    seen_quiet == 1 && (PersonFence == 0 || touched == 0) &&
                    (mine == 0 || (stilled == 1 && retried <= MaxRetry - 1))
                ))
            ) {
                q1 = if q1 == 0 { 3 + seen_tab } else { q1 };
                q2 = if q1 == 0 { 0 } else { 3 + seen_tab };
                retried = if mine == 0 { 0 }
                    else { if retried <= MaxRetry - 1 { retried + 1 } else { retried } };
                stilled = 0;
                mine = 1;
                fresh = 0;
            }
            action HarnessMove when (
                fresh == 1 && q2 == 0 && seen_tab <= Tabs &&
                (seen_typed == 0 || (OwnTextRemembered == 0 && seen_exact == 1)) &&
                seen_focus == 1 &&
                (Buggy == 1 || (
                    seen_quiet == 1 && (PersonFence == 0 || touched == 0) &&
                    (mine == 0 || (stilled == 1 && retried <= MaxRetry - 1))
                ))
            ) {
                q1 = if q1 == 0 { 2 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 2 };
                retried = if mine == 0 { 0 }
                    else { if retried <= MaxRetry - 1 { retried + 1 } else { retried } };
                stilled = 0;
                mine = 1;
                fresh = 0;
            }
            action HarnessType when (
                Text == 1 && seen_fallback == 0 && fresh == 1 && q2 == 0 &&
                seen_tab <= Tabs - 1 &&
                seen_focus == 0 && seen_typed == 0 && seen_own == 0 &&
                (Buggy == 1 || (
                    seen_quiet == 1 && (PersonFence == 0 || touched == 0) &&
                    mine == 0 && (OwnTextRemembered == 0 || wrote == 0)
                ))
            ) {
                q1 = if q1 == 0 { 4 + Tabs + seen_tab } else { q1 };
                q2 = if q1 == 0 { 0 } else { 4 + Tabs + seen_tab };
                twice = if wrote == 1 { 1 } else { twice };
                wrote = 1;
                retried = MaxRetry;
                stilled = 0;
                mine = 1;
                fresh = 0;
            }

            invariant NoDigitInTheField: in_field == 0;
            invariant NeverAnswersANextTabWithAStaleKey: stale == 0;
            invariant NeverCancelsTheReview: cancelled == 0;
            invariant NoKeyIntoTheComposer: composer == 0;
            invariant OwnTextOnlyIntoItsEmptyField: astray == 0;
            invariant TextNeverWrittenTwice: twice == 0;
        }
    }
}

const QUESTION_TEXT_LAWS: [&str; 2] = ["OwnTextOnlyIntoItsEmptyField", "TextNeverWrittenTwice"];

const QUESTION_TEXT_ACTIONS: [&str; 6] = [
    "HarnessType",
    "TextLands",
    "TextPartly",
    "FitLostOnIt",
    "FitLostOffIt",
    "PersonTypesPolicyText",
];

/// Specialization of [`supervisor_question_text_model`] with `Text = 0` for the recommended-option path.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn supervisor_question_answer_model() -> Model {
    let mut m = supervisor_question_text_model();
    m.name = "SupervisorQuestionAnswer";
    for (name, value) in &mut m.consts {
        if *name == "Text" {
            *value = 0;
        }
    }
    m.invariants
        .retain(|law| !QUESTION_TEXT_LAWS.contains(&law.name));
    m.actions
        .retain(|action| !QUESTION_TEXT_ACTIONS.contains(&action.name));
    m
}

/// Formal model for keystroke sequences when declining a tool-approval dialog (`supervise/press.rs`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_decline_keys_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorDeclineKeys {
            const Buggy = 0;
            const PersonFence = 1;
            var focus = 0;
            var open = 0;
            var text = 0;
            var gone = 0;
            var approved = 0;
            var amended_yes = 0;
            var bare = 0;
            var foreign = 0;
            var composer = 0;
            var q1 = 0;
            var q2 = 0;
            var quiet = 1;
            var fresh = 0;
            var seen_focus = 0;
            var seen_open = 0;
            var seen_text = 0;
            var seen_quiet = 1;
            var touched = 0;
            var mine = 0;

            action PersonKey when (gone == 0 && q2 == 0) {
                q1 = if q1 == 0 { 5 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 5 };
                quiet = 0;
                touched = 1;
            }
            action QuietReturns when (quiet == 0 && q1 <= 4 && q2 <= 4) {
                quiet = 1;
            }
            action PersonMoves when (q1 == 5 && gone == 0) {
                focus = 1 - focus;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action PersonTabs when (q1 == 5 && gone == 0 && focus == 1) {
                open = 1 - open;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action PersonTypes when (q1 == 5 && gone == 0 && open == 1 && focus == 1) {
                text = 2;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action PersonClears when (q1 == 5 && gone == 0 && open == 1 && focus == 1) {
                text = 0;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action PersonAnswers when (q1 == 5 && gone == 0) {
                gone = 2;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action PersonKeyIgnored when (q1 == 5) {
                q1 = q2;
                q2 = 0;
            }

            action DownTaken when (q1 == 1 && gone == 0) {
                focus = 1 - focus;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action TabTaken when (q1 == 2 && gone == 0) {
                open = if focus == 1 { 1 - open } else { open };
                amended_yes = if focus == 0 { 1 } else { amended_yes };
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action TextIntoTheInput when (q1 == 3 && gone == 0 && open == 1 && focus == 1) {
                text = if text == 0 { 1 } else { 2 };
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action TextOnTheSelect when (q1 == 3 && gone == 0 && (open == 0 || focus == 0)) {
                approved = 1;
                gone = 2;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action EnterSubmitsTheReason when (
                q1 == 4 && gone == 0 && open == 1 && focus == 1 && text == 1
            ) {
                gone = 1;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action EnterSubmitsAnotherText when (
                q1 == 4 && gone == 0 && open == 1 && focus == 1 && 2 <= text
            ) {
                foreign = 1;
                gone = 2;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action EnterOnTheEmptyInput when (
                q1 == 4 && gone == 0 && open == 1 && focus == 1 && text == 0
            ) {
                bare = 1;
                gone = 2;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action EnterOnTheSelect when (q1 == 4 && gone == 0 && (open == 0 || focus == 0)) {
                approved = if focus == 0 { 1 } else { approved };
                bare = if focus == 1 { 1 } else { bare };
                gone = 2;
                q1 = q2;
                q2 = 0;
                fresh = 0;
            }
            action KeyRefused when (1 <= q1 && q1 <= 4 && gone == 0) {
                q1 = q2;
                q2 = 0;
            }
            action KeyIntoTheComposer when (1 <= q1 && q1 <= 4 && 1 <= gone) {
                composer = 1;
                q1 = q2;
                q2 = 0;
            }

            action Read when (gone == 0) {
                fresh = 1;
                seen_focus = focus;
                seen_open = open;
                seen_text = text;
                seen_quiet = quiet;
                touched = 0;
                mine = if mine == 1 && focus == seen_focus && open == seen_open
                    && text == seen_text { 1 } else { 0 };
            }
            action HarnessDown when (
                fresh == 1 && q2 == 0 && seen_focus == 0 && seen_open == 0 &&
                (Buggy == 1 || (mine == 0 &&
                    seen_quiet == 1 && (PersonFence == 0 || touched == 0)))
            ) {
                q1 = if q1 == 0 { 1 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 1 };
                mine = 1;
                fresh = 0;
            }
            action HarnessTab when (
                fresh == 1 && q2 == 0 && seen_focus == 1 && seen_open == 0 &&
                (Buggy == 1 || (mine == 0 &&
                    seen_quiet == 1 && (PersonFence == 0 || touched == 0)))
            ) {
                q1 = if q1 == 0 { 2 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 2 };
                mine = 1;
                fresh = 0;
            }
            action HarnessText when (
                fresh == 1 && q2 == 0 && seen_focus == 1 && seen_open == 1 && seen_text == 0 &&
                (Buggy == 1 || (mine == 0 &&
                    seen_quiet == 1 && (PersonFence == 0 || touched == 0)))
            ) {
                q1 = if q1 == 0 { 3 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 3 };
                mine = 1;
                fresh = 0;
            }
            action HarnessEnter when (
                fresh == 1 && q2 == 0 && seen_focus == 1 && seen_open == 1 && seen_text == 1 &&
                (Buggy == 1 || (mine == 0 &&
                    seen_quiet == 1 && (PersonFence == 0 || touched == 0)))
            ) {
                q1 = if q1 == 0 { 4 } else { q1 };
                q2 = if q1 == 0 { 0 } else { 4 };
                mine = 1;
                fresh = 0;
            }

            invariant NeverChoosesTheAllow: approved == 0;
            invariant NeverAmendsTheAllow: amended_yes == 0;
            invariant NeverTheBareNo: bare == 0;
            invariant NeverSubmitsAnotherText: foreign == 0;
            invariant NoKeyIntoTheComposer: composer == 0;
        }
    }
}

/// Formal model for bounded retries on transient network/API walls (`supervise/policy/turn_end.rs`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_network_wall_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorNetworkWall {
            const Buggy = 0;
            const Carry = 0;
            const Rung1 = 1;
            const Rung2 = 2;
            const Hold = 3;
            const AtOnce = 2;
            var net = 1;
            var verdict = 0;
            var wall = 0;
            var cause = 0;
            var waited = 0;
            var tries = 0;
            var ups = 0;
            var quick = 0;
            var dacts = 0;
            var dticks = 0;

            action NetFails when (net == 1) {
                net = 0;
            }
            action NetReturns when (net == 0) {
                net = 1;
            }

            action MeasureDown when (net == 0 && (verdict == 0 || verdict == 2)) {
                verdict = 1;
            }
            action MeasureUp when (net == 1 && (verdict == 0 || verdict == 1)) {
                verdict = 2;
            }
            action MeasureLost when (verdict == 1 || verdict == 2) {
                verdict = 0;
            }
            action MeasureLies when (net == 0 && (verdict == 0 || verdict == 1)) {
                verdict = 2;
            }
            action MeasureWrongDown when (net == 1 && (verdict == 0 || verdict == 2)) {
                verdict = 1;
            }

            action Unreachable when (wall == 0 && net == 0) {
                wall = 1;
                cause = 0;
                waited = 0;
            }
            action CutOff when (wall == 0) {
                wall = 1;
                cause = 1;
                waited = 0;
            }

            action Tick when (wall == 1 && waited <= Hold - 1) {
                waited = waited + 1;
                dticks = if dticks <= Hold + Hold - 1 { dticks + 1 } else { dticks };
            }

            action Act when (
                wall == 1 && (
                    Buggy == 1 ||
                    (cause == 0 && verdict == 2 &&
                        (ups == 0 || (ups == 1 && Rung1 <= waited) || (ups == 2 && Rung2 <= waited))) ||
                    (verdict == 1 && Hold <= waited) ||
                    (cause == 0 && verdict == 0 &&
                        ((tries == 0 && Rung1 <= waited) || (1 <= tries && Rung2 <= waited))) ||
                    (cause == 1 && (verdict == 0 || verdict == 2) &&
                        (tries == 0 || (tries == 1 && Rung1 <= waited) ||
                            (tries == 2 && Rung2 <= waited)))
                )
            ) {
                dacts = if verdict == 1 && dacts <= 1 { dacts + 1 } else { dacts };
                quick = if waited == 0 && quick <= AtOnce { quick + 1 } else { quick };
                ups = if cause == 0 && verdict == 2 && ups <= 1 { ups + 1 } else { ups };
                tries = if tries <= 1 { tries + 1 } else { tries };
                wall = 2;
                waited = if Carry == 1 { waited } else { 0 };
            }

            action MetUnreachable when (wall == 2 && net == 0) {
                wall = 1;
                cause = 0;
            }
            action MetCutOff when (wall == 2) {
                wall = 1;
                cause = 1;
            }
            action WorkedThenUnreachable when (wall == 2 && net == 0) {
                wall = 1;
                cause = 0;
                tries = 0;
                ups = 0;
                quick = 0;
                dacts = 0;
                dticks = 0;
            }
            action WorkedThenCutOff when (wall == 2) {
                wall = 1;
                cause = 1;
                tries = 0;
                ups = 0;
                quick = 0;
                dacts = 0;
                dticks = 0;
            }
            action Taken when (wall == 2 && net == 1) {
                wall = 0;
                cause = 0;
                tries = 0;
                ups = 0;
                quick = 0;
                dacts = 0;
                dticks = 0;
            }

            invariant AMeasuredOutageIsTypedIntoAtMostOnceAHold:
                dacts == 0 || (dacts == 1 && Hold <= dticks) || Hold + Hold <= dticks;
            invariant AtMostTwoActsAtOncePerEpisode: quick <= AtOnce;
        }
    }
}

/// Formal model for dismissing Claude Code's feedback survey card with `0`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_survey_press_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorSurveyPress {
            const Buggy = 0;
            const Tries = 2;
            var card = 0;
            var hand = 0;
            var draft = 0;
            var boxed = 0;
            var zero = 0;
            var takes = 0;
            var owed = 0;
            var tries = 0;
            var young_zero = 0;
            var hand_zero = 0;
            var draft_zero = 0;
            var stacked_zero = 0;
            var pending_backspace = 0;

            action CardOpens when (card == 0) {
                card = 1;
                tries = 0;
            }
            action SettleElapses when (card == 1) {
                card = 2;
            }
            action CardLeaves when (card > 0) {
                card = 0;
                takes = 0;
            }
            action PersonTypes when (hand == 0) {
                hand = 1;
            }
            action GraceEnds when (hand == 1) {
                hand = 0;
            }
            action DraftTyped when (draft == 0 && zero == 0) {
                draft = 1;
            }
            action DraftGone when (draft == 1) {
                draft = 0;
            }
            action BoxUp when (boxed == 0) {
                boxed = 1;
            }
            action BoxGone when (boxed == 1) {
                boxed = 0;
            }
            action Press when (
                card > 0 && tries <= Tries - 1 &&
                ((card == 2 && hand == 0 && draft == 0 && boxed == 0 && zero == 0) || Buggy == 1)
            ) {
                tries = tries + 1;
                young_zero = if card == 1 { 1 } else { young_zero };
                hand_zero = if hand == 1 { 1 } else { hand_zero };
                draft_zero = if draft == 1 || boxed == 1 { 1 } else { draft_zero };
                stacked_zero = if zero > 0 { 1 } else { stacked_zero };
                takes = if card == 2 { 1 } else { 0 };
                zero = 1;
                owed = 1;
            }
            action DebounceRuns when (zero == 1) {
                card = if takes == 1 { 0 } else { card };
                owed = if takes == 1 { 0 } else { owed };
                zero = if takes == 1 { 0 } else { 2 };
                takes = 0;
            }
            action Backspace when (
                draft == 0 && hand == 0 && (zero == 2 || (Buggy == 1 && zero == 1))
            ) {
                pending_backspace = if zero == 1 && takes == 1 { 1 } else { pending_backspace };
                zero = 0;
                takes = 0;
                owed = 0;
            }
            action BackspaceRefused when (
                draft == 0 && hand == 0 && (zero == 2 || (Buggy == 1 && zero == 1))
            ) {
                owed = if Buggy == 1 { 0 } else { 1 };
            }

            invariant NoZeroInsideTheSettle: young_zero == 0;
            invariant NoZeroUnderAPersonsHand: hand_zero == 0;
            invariant NoZeroOverADraftOrABox: draft_zero == 0;
            invariant NoZeroOverAZero: stacked_zero == 0;
            invariant NoBackspaceOfAPendingZero: pending_backspace == 0;
            invariant NoForgottenZero: zero <= 1 || owed == 1;
        }
    }
}

/// Formal model for waiting out or resetting a standing usage-limit wall, the usage tap timing a cut notice's reset included (`Tapped`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_usage_wall_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorUsageWall {
            const Buggy = 0;
            const ResetAt = 2;
            const Grace = 1;
            const Rung1 = 1;
            const Rung2 = 2;
            const Skew = 8;
            const Cap = 12;
            const Hidden = 1;
            const Leaves = 1;
            const OwnAct = 1;
            const Unread = 1;
            const Credits = 1;
            const Tapped = 1;
            const CutAt = 4;
            var wall = 0;
            var bg = 0;
            var due = 2;
            var clock = 0;
            var view = 0;
            var tries = 0;
            var waited = 0;
            var blind = 0;
            var mem = 1;
            var sitting = 0;
            var ran = 0;
            var other = 0;
            var held = 0;
            var unread = 0;
            var credits = 0;
            var cut = 0;
            var tap = 0;

            action BackgroundStarts when (bg == 0 && wall <= 5) {
                bg = 1;
            }
            action BackgroundEnds when (bg == 1) {
                bg = 0;
            }
            action WallAppears when (wall == 0 && credits == 0) {
                wall = 1;
                sitting = 0;
                ran = 0;
                blind = 0;
                due = ResetAt;
                clock = 0;
                view = 0;
                tries = 0;
                waited = 0;
            }
            action WallAppearsLate when (wall == 0 && credits == 0) {
                wall = 1;
                sitting = 0;
                ran = 0;
                blind = 0;
                due = ResetAt + Skew;
                clock = 0;
                view = 0;
                tries = 0;
                waited = 0;
            }
            action WallFoundSitting when (wall == 0 && held == 0 && credits == 0) {
                wall = 1;
                sitting = 1;
                ran = 0;
                blind = 0;
                due = ResetAt;
                clock = 0;
                view = 1;
                tries = 0;
                waited = 0;
            }
            action Tick when (1 <= wall && clock <= Cap - 1 && (cut == 0 || 1 <= tap)) {
                clock = clock + 1;
                waited = if waited <= Rung2 - 1 { waited + 1 } else { waited };
                held = 0;
                unread = 0;
            }

            action WallHides when (Hidden == 1 && wall == 1 && bg == 1) {
                wall = 5;
            }
            action WallShows when (wall == 5) {
                wall = 1;
            }
            action NoticeLeaves when (Leaves == 1 && wall == 1 && bg == 0 && cut == 0) {
                wall = 6;
            }
            action PersonTurnServed when (wall == 6 && due <= clock && other == 0) {
                wall = 0;
                held = 0;
                unread = 0;
                tries = 0;
                clock = 0;
                waited = 0;
                view = 0;
                sitting = 0;
                blind = 0;
                cut = 0;
                tap = 0;
            }
            action LimitLiftedEarly when (wall == 6 && clock <= due - 1) {
                due = clock;
            }
            action ConversationResumed when (Leaves == 1 && wall == 6 && other == 0) {
                other = 1;
            }
            action ArmedConversationResumed when (
                Leaves == 1 && wall == 1 && bg == 0 && other == 0 && cut == 0
            ) {
                wall = 6;
                other = 2;
            }
            action WallAppearsLeft when (
                Unread == 1 && Leaves == 1 && held == 1 && wall == 0 && bg == 0 && credits == 0
            ) {
                wall = 6;
                unread = 1;
                sitting = 0;
                ran = 0;
                blind = 0;
                due = ResetAt;
                clock = 0;
                view = 0;
                tries = 0;
                waited = 0;
            }

            action PersonWallLeft when (
                Unread == 1 && Leaves == 1 && held == 0 && wall == 0 && bg == 0 && credits == 0
            ) {
                wall = 6;
                unread = 1;
                sitting = 0;
                ran = 0;
                blind = 0;
                due = ResetAt;
                clock = 0;
                view = 0;
                tries = 0;
                waited = 0;
            }
            action OlderLeftResumed when (
                Unread == 1 && Leaves == 1 && held == 1 && wall == 0 && bg == 0 && credits == 0
            ) {
                wall = 6;
                unread = 1;
                sitting = 1;
                ran = 0;
                blind = 0;
                due = 0;
                clock = 0;
                view = 1;
                tries = 0;
                waited = 0;
            }

            // A wall whose notice's reset is cut (the `Tapped` dial): its reset `CutAt` out, past the back-off.
            action WallAppearsCut when (Tapped == 1 && wall == 0 && held == 0 && credits == 0) {
                wall = 1;
                sitting = 0;
                ran = 0;
                blind = 0;
                due = CutAt;
                clock = 0;
                view = 0;
                tries = 0;
                waited = 0;
                cut = 1;
                tap = 0;
            }
            // The tap's run after the wall was first read names its reset, before a tick.
            action TapReads when (cut == 1 && tap == 0 && (wall == 1 || wall == 5)) {
                tap = 1;
            }
            // The tap's run at the reset: the window rolled over.
            action TapRollsOver when (cut == 1 && tap == 1 && due <= clock && (wall == 1 || wall == 5)) {
                tap = 2;
            }

            action CreditsServes when (
                Credits == 1 && wall == 0 && held == 0 && credits == 0 && bg == 0
            ) {
                credits = 1;
            }
            action CreditsTurnEnds when (credits == 1) {
                credits = 0;
            }

            action OwnActTyped when (OwnAct == 1 && wall == 0 && held == 0 && credits == 0) {
                held = 1;
            }
            action Act when (
                held == 0 &&
                (wall == 1 || ((wall == 5 || wall == 6) && mem == 1)) &&
                (
                    (cut == 0 && ((view == 0 && ResetAt + Grace <= clock) || view == 1)) ||
                    (cut == 1 && 1 <= tap && CutAt + Grace <= clock)
                ) && (
                    tries == 0 ||
                    (tries == 1 && Rung1 <= waited) ||
                    (2 <= tries && Rung2 <= waited)
                )
            ) {
                blind = if clock <= ResetAt - 1 { if blind <= 1 { blind + 1 } else { blind } } else { blind };
                tries = if tries <= 2 { tries + 1 } else { tries };
                wall = 2;
                waited = 0;
                other = 0;
                held = OwnAct;
            }
            action Taken when (wall == 2 && due <= clock) {
                wall = 0;
                tries = 0;
                clock = 0;
                waited = 0;
                view = 0;
                sitting = 0;
                blind = 0;
                ran = 0;
                held = 0;
                cut = 0;
                tap = 0;
            }
            action Met when (wall == 2 && clock <= due - 1) {
                wall = 1;
                waited = 0;
                view = 0;
                held = 0;
            }
            action MetInWindow when (wall == 2 && held == 1 && clock <= due - 1) {
                wall = 1;
                waited = 0;
                view = 0;
            }
            action MetLeftUnread when (
                Unread == 1 && Leaves == 1 && wall == 2 && held == 1 && clock <= due - 1 && bg == 0
            ) {
                wall = 6;
                unread = 1;
                waited = 0;
                view = 0;
            }

            action WallMasked when (Buggy == 1 && wall == 0 && bg == 1 && credits == 0) {
                wall = 3;
                sitting = 0;
                blind = 0;
                due = ResetAt;
                clock = 0;
                view = 0;
                tries = 0;
                waited = 0;
            }
            action MemoryLost when (Buggy == 1 && (wall == 5 || wall == 6) && mem == 1) {
                mem = 0;
            }
            action LeftReadAsTurnEnd when (Buggy == 1 && wall == 6 && clock <= ResetAt - 1) {
                blind = if blind <= 1 { blind + 1 } else { blind };
                tries = if tries <= 2 { tries + 1 } else { tries };
                wall = 2;
                waited = 0;
                other = 0;
                unread = 0;
            }
            action PersonTurnUnseen when (Buggy == 1 && wall == 6 && due <= clock && other == 0 && ran == 0) {
                ran = 1;
            }
            action OtherLimitsTurnRead when (Buggy == 1 && wall == 6 && other == 1 && clock <= due - 1) {
                blind = if blind <= 1 { blind + 1 } else { blind };
                tries = if tries <= 2 { tries + 1 } else { tries };
                wall = 2;
                waited = 0;
                other = 0;
                unread = 0;
            }
            action OtherLimitsContinuationRead when (
                Buggy == 1 && wall == 6 && other == 2 && clock <= due - 1
            ) {
                blind = if blind <= 1 { blind + 1 } else { blind };
                tries = if tries <= 2 { tries + 1 } else { tries };
                wall = 2;
                waited = 0;
                other = 0;
                unread = 0;
            }
            action TakeWindowDropsWall when (Buggy == 1 && wall == 6 && held == 1 && clock <= ResetAt - 1) {
                blind = if blind <= 1 { blind + 1 } else { blind };
                tries = if tries <= 2 { tries + 1 } else { tries };
                wall = 2;
                waited = 0;
                other = 0;
                held = 0;
                unread = 0;
            }
            action RetryReadAsTaken when (Buggy == 1 && wall == 6 && held == 1 && 1 <= tries && mem == 1) {
                mem = 0;
            }
            action UnreadLeftDropped when (
                Buggy == 1 && wall == 6 && unread == 1 && tries == 0 && clock <= ResetAt - 1
            ) {
                blind = if blind <= 1 { blind + 1 } else { blind };
                tries = if tries <= 2 { tries + 1 } else { tries };
                wall = 2;
                waited = 0;
                other = 0;
                held = 0;
                unread = 0;
            }
            action OlderLeftPlacedTomorrow when (
                Buggy == 1 && Unread == 1 && Leaves == 1 && held == 1 && wall == 0 && bg == 0
                    && credits == 0
            ) {
                wall = 6;
                unread = 1;
                sitting = 1;
                ran = 0;
                blind = 0;
                due = 0;
                clock = 0;
                view = 2;
                tries = 0;
                waited = 0;
            }
            action UnreadAnswerReadAsTaken when (
                Buggy == 1 && wall == 6 && unread == 1 && 1 <= tries && mem == 1
            ) {
                mem = 0;
            }
            action GivesUp when (Buggy == 1 && wall == 1 && 3 <= tries) {
                wall = 4;
            }
            action MetKeepsTheGuess when (Buggy == 1 && wall == 2 && clock <= due - 1) {
                wall = 1;
                waited = 0;
            }
            action WallMisplaced when (Buggy == 1 && wall == 0 && credits == 0) {
                wall = 1;
                sitting = 0;
                blind = 0;
                due = ResetAt;
                clock = 0;
                view = 1;
                tries = 0;
                waited = 0;
            }

            // The tap's reset unread at a cut notice: the act goes on the back-off, before the reset.
            action TapResetIgnored when (
                Buggy == 1 && wall == 1 && cut == 1 && tap == 1 && held == 0 && tries == 0
                    && Rung2 <= clock && clock <= CutAt - 1
            ) {
                blind = if blind <= 1 { blind + 1 } else { blind };
                tries = 1;
                wall = 2;
                waited = 0;
                other = 0;
                held = OwnAct;
            }

            action CreditsReadAsWall when (Buggy == 1 && credits == 1 && wall == 0) {
                wall = 1;
                sitting = 0;
                ran = 0;
                blind = 0;
                due = ResetAt;
                clock = 0;
                view = 0;
                tries = 0;
                waited = 0;
            }

            invariant NoBlindAct: blind <= sitting;
            invariant NoWallOnCredits: credits == 0 || wall == 0;
            invariant NoStuckWall: wall <= 2 || 5 <= wall;
            invariant WallRemembered: mem == 1;
            invariant NoWaitOverAServedTurn: ran == 0;
            invariant NoDayWait: view <= 1;
        }
    }
}

/// Liveness specification asserting a usage-limit wall eventually resumes work when tokens return.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(supervisor_usage_wall_model)]
pub fn supervisor_usage_wall_liveness() -> Liveness {
    use aterm_spec::derive::{eq, int, le, or_, sub, var};
    Liveness {
        name: "TheUsageWallEnds",
        goal: or_(
            eq(var("wall"), int(0)),
            le(var("clock"), sub(var("due"), int(1))),
        ),
        weak: vec!["Tick", "Act", "Taken"],
        strong: vec![],
        mutants: vec![
            "WallMasked",
            "GivesUp",
            "MemoryLost",
            "OlderLeftPlacedTomorrow",
        ],
    }
}

/// Formal model for holding a session on usage credits when another account is available.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_credits_hold_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorCreditsHold {
            const Buggy = 0;
            const UsageCredits = 0;
            const ResetAt = 2;
            const Bound = 3;
            const Cap = 3;
            var point = 0;
            var carry = 0;
            var notice = 0;
            var known = 0;
            var named = 0;
            var clock = 0;
            var told = 0;
            var billed = 0;
            var lift = 1;

            action CreditsStart when (point == 0 && carry == 0 && notice == 0 && known == 0) {
                carry = 1;
                notice = 1;
                known = 1;
                named = 1;
                clock = 0;
                told = 0;
                lift = 1;
            }
            action CreditsStartUnread when (point == 0 && carry == 0 && notice == 0 && known == 0) {
                carry = 1;
                notice = 1;
                known = 1;
                named = 0;
                clock = 0;
                told = 0;
                lift = 1;
            }
            action NoticeGone when (notice == 1 && named == 0) {
                notice = 0;
                known = if (named == 1 && ResetAt <= clock)
                    || (named == 0 && Bound <= clock) { 0 } else { known };
            }
            action NoticeReturns when (known == 1 && notice == 0) {
                notice = 1;
            }
            action Tick when (known == 1 && clock <= Cap - 1) {
                clock = clock + 1;
                carry = if ResetAt <= clock + 1 { 0 } else { carry };
            }
            action TurnEnds when (point == 0) {
                point = 1;
                notice = if carry == 0 { 0 } else { notice };
                known = if carry == 0
                    && ((named == 1 && ResetAt <= clock)
                        || (named == 0 && Bound <= clock)) { 0 } else { known };
            }
            action AnotherWindow when (point == 0 && notice == 1 && carry == 0) {
                point = 1;
                carry = 1;
                told = if named == 1 { 0 } else { told };
                named = 0;
                clock = if named == 1 || Bound <= clock { 0 } else { clock };
            }
            action PersonTurn when (point == 1) {
                point = 0;
            }

            action OwnAct when (
                point == 1
                    && (UsageCredits == 1
                        || known == 0
                        || (lift == 1
                            && ((named == 1 && ResetAt <= clock)
                                || (named == 0 && Bound <= clock))))
            ) {
                point = 0;
                billed = if carry == 1 && UsageCredits == 0 { 1 } else { billed };
            }
            action Tell when (
                point == 1 && UsageCredits == 0 && known == 1 && named == 0 && told == 0
                    && clock <= Bound - 1
            ) {
                told = 1;
            }

            action ActsOnCredits when (Buggy == 1 && point == 1 && carry == 1) {
                point = 0;
                billed = if UsageCredits == 0 { 1 } else { billed };
            }
            action TellsAgain when (
                Buggy == 1 && point == 1 && UsageCredits == 0 && known == 1 && named == 0
                    && told == 1
            ) {
                told = 2;
            }
            action StaleNoticeHolds when (
                Buggy == 1 && point == 1 && UsageCredits == 0 && notice == 1 && carry == 0
                    && named == 1 && lift == 1
            ) {
                lift = 0;
            }
            action UnboundedHold when (
                Buggy == 1 && point == 1 && UsageCredits == 0 && known == 1 && named == 0
                    && lift == 1
            ) {
                lift = 0;
            }

            invariant NoOwnActOnCredits: UsageCredits == 1 || billed == 0;
            invariant ToldOnce: told <= 1;
            invariant NoStaleHold: point == 0
                || named == 0
                || clock <= ResetAt - 1
                || UsageCredits == 1
                || known == 0
                || (lift == 1
                    && ((named == 1 && ResetAt <= clock) || (named == 0 && Bound <= clock)));
            invariant NoEndlessHold: point == 0
                || named == 1
                || clock <= Bound - 1
                || UsageCredits == 1
                || known == 0
                || (lift == 1
                    && ((named == 1 && ResetAt <= clock) || (named == 0 && Bound <= clock)));
        }
    }
}

/// Liveness specification asserting a credits hold eventually releases or hands off.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(supervisor_credits_hold_model)]
pub fn supervisor_credits_hold_liveness() -> Liveness {
    use aterm_spec::derive::{and_, eq, int, le, or_, var};
    let own_act = or_(
        eq(var("UsageCredits"), int(1)),
        and_(
            eq(var("lift"), int(1)),
            or_(
                and_(eq(var("named"), int(1)), le(var("ResetAt"), var("clock"))),
                and_(eq(var("named"), int(0)), le(var("Bound"), var("clock"))),
            ),
        ),
    );
    Liveness {
        name: "TheCreditsHoldEnds",
        goal: or_(
            or_(eq(var("point"), int(0)), eq(var("carry"), int(1))),
            or_(eq(var("known"), int(0)), own_act),
        ),
        weak: vec!["Tick"],
        strong: vec![],
        mutants: vec!["StaleNoticeHolds", "UnboundedHold"],
    }
}

/// Formal model for switching models when the current model bucket hits a usage limit.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_model_switch_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorModelSwitch {
            const Buggy = 0;
            const ResetAt = 2;
            const Cap = 3;
            const Guess = 1;
            const GiveUpAfter = 2;
            var bucket = 0;
            var named = 0;
            var on = 0;
            var back = 0;
            var jobs = 0;
            var clock = 0;
            var probed = 0;
            var tries = 0;
            var flips = 0;
            var gaveup = 0;
            var held = 0;
            var badret = 0;

            action JobsStart when (jobs == 0) {
                jobs = 1;
            }
            action JobsEnd when (jobs == 1) {
                jobs = 0;
            }
            action Tick when (bucket == 1 && clock <= Cap - 1) {
                clock = clock + 1;
            }
            action Works when (bucket == 0 && on == 0 && 1 <= flips) {
                flips = 0;
            }
            action BucketHitsNamed when (bucket == 0 && on == 0 && flips == 0) {
                bucket = 1;
                named = 1;
                back = 0;
                clock = 0;
                probed = 0;
                tries = 0;
                gaveup = 0;
            }
            action BucketHitsUnnamed when (bucket == 0 && on == 0 && flips == 0) {
                bucket = 1;
                named = 0;
                back = 0;
                clock = 0;
                probed = 0;
                tries = 0;
                gaveup = 0;
            }
            action BucketLifts when (bucket == 1 && named == 1 && ResetAt <= clock) {
                bucket = 0;
            }

            action Continue when (bucket == 1 && on == 0 && named == 1 && ResetAt <= clock && probed == 0) {
                probed = 1;
            }
            action AskFallback when (
                bucket == 1 && on == 0 && gaveup == 0 &&
                (named == 0 || probed == 1 || clock <= ResetAt - 1)
            ) {
                tries = if tries <= GiveUpAfter { tries + 1 } else { tries };
                on = if jobs == 0 { 1 } else { on };
                probed = if jobs == 0 { 1 } else { probed };
                back = if jobs == 0 { if named == 1 && clock <= ResetAt - 1 { 1 } else { 0 } } else { back };
                flips = if jobs == 0 { if flips <= 3 { flips + 1 } else { flips } } else { flips };
            }
            action Return when (on == 1 && back == 1 && ResetAt <= clock && jobs == 0) {
                on = 0;
                back = 0;
                flips = if flips <= 3 { flips + 1 } else { flips };
            }

            action GuessedReturn when (Buggy == 1 && on == 1 && bucket == 1 && back == 0 && Guess <= clock) {
                on = 0;
                flips = if flips <= 3 { flips + 1 } else { flips };
                badret = 1;
            }
            action GivesUp when (Buggy == 1 && on == 0 && bucket == 1 && GiveUpAfter <= tries) {
                gaveup = 1;
            }
            action ReturnBlocks when (Buggy == 1 && on == 1 && back == 1 && ResetAt <= clock && jobs == 1) {
                held = 1;
            }

            invariant NoReturnWithoutAReset: badret == 0;
            invariant NoPingPong: flips <= 3;
            invariant PointsGoOn: held == 0;
            invariant AsksNeverStop: gaveup == 0;
        }
    }
}

/// Liveness specification asserting a model-bucket limit eventually switches to the fallback model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(supervisor_model_switch_model)]
pub fn supervisor_model_switch_liveness() -> Liveness {
    use aterm_spec::derive::{eq, int, or_, var};
    Liveness {
        name: "TheFallbackIsMade",
        goal: or_(
            or_(eq(var("on"), int(1)), eq(var("bucket"), int(0))),
            eq(var("jobs"), int(1)),
        ),
        weak: vec!["AskFallback", "Continue"],
        strong: vec![],
        mutants: vec!["GivesUp"],
    }
}

/// Formal model ensuring redraws of the same modal box are treated as a single decision point.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supervisor_box_ask_model() -> Model {
    aterm_spec::ty_model! {
        SupervisorBoxAsk {
            const Buggy = 0;
            const Drawing = 0;
            const Coarse = 0;
            const WritingWorks = 0;
            var req = 0;
            var seen = 0;
            var form = 0;
            var key = 0;
            var handed = 0;
            var live = 0;
            var asks = 0;
            var looked = 0;

            action NextBox when (seen <= 2 && (seen <= 1 || live == 1)) {
                seen = seen + 1;
                req = seen + 1;
                form = 0;
                asks = 0;
                looked = 0;
                key = if Coarse == 1 || Buggy == 1 {
                    1
                } else {
                    if seen == 2 { 2 } else { seen + 1 }
                };
            }
            action Redraw when (req > 0) {
                form = 1 - form;
                key = if Drawing == 1 || Buggy == 1 {
                    if form == 1 { key - 10 } else { key + 10 }
                } else {
                    key
                };
            }
            action Writing when (req > 0) {
                live = if WritingWorks == 1 || Buggy == 1 { 1 } else { live };
            }
            action Answer when (req > 0) {
                req = 0;
            }
            action Work when (req == 0) {
                live = 1;
            }
            action Look when (req > 0) {
                asks = if handed == key && live == 0 {
                    asks
                } else {
                    if asks <= 1 { asks + 1 } else { asks }
                };
                handed = key;
                live = 0;
                looked = 1;
            }

            invariant OneAskPerRequest: asks <= 1;
            invariant EveryLookedRequestIsAsked: req == 0 || looked == 0 || asks > 0;
        }
    }
}
