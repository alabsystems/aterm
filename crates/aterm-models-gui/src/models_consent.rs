// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal models for macOS debugger-access and consent gestures.

use aterm_spec::derive::*;

/// Formal state-machine model for the macOS debugger-access change workflow.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn debugger_access_change_model() -> Model {
    aterm_spec::ty_model! {
        DebuggerAccessChange {
            const Buggy = 0;
            const Cap = 2;
            var phase = 0;
            var world = 0;
            var obs = 3;
            var claim = 0;
            var polling = 0;
            var lock = 0;
            var fresh = 0;
            var launches = 0;
            var unconsented = 0;
            var inflight = 0;
            var armed = 0;
            var unarmed = 0;
            var clobbered = 0;
            var stale = 0;
            var misadopted = 0;
            var asked = 0;
            var reasked = 0;
            var overlapped = 0;

            action Probe when (phase == 0 || phase == 6) {
                obs = world;
                claim = if (world == 1) { 1 } else { 0 };
                phase = 0;
                polling = 0;
            }
            action ProbeFails when (phase == 0 || phase == 6) {
                obs = 3;
                claim = 0;
                phase = 0;
                polling = 0;
            }
            action Press when (phase == 0 && obs == 0) {
                phase = 1;
            }
            action PressWhileInFlight when (Buggy == 1 && phase == 4) {
                phase = 1;
            }
            action Offer when (phase == 0 && obs == 0 && lock == 0 && stale == 0 && asked == 0) {
                asked = 1;
                phase = 1;
            }
            action OfferAgain when (Buggy == 2 && phase == 0 && obs == 0 && lock == 0 && stale == 0 && asked == 1) {
                reasked = 1;
                phase = 1;
            }
            action OfferWhileInFlight when (Buggy == 2 && phase == 4 && asked == 0) {
                asked = 1;
                overlapped = 1;
            }
            action OfferRunsUnasked when (Buggy == 2 && phase == 0 && obs == 0 && lock == 0 && stale == 0 && asked == 0 && launches <= Cap - 1) {
                asked = 1;
                polling = 1;
                phase = 2;
            }
            action Forgo when (asked == 0) {
                asked = 1;
            }
            action Decline when (phase == 1) {
                phase = if (polling == 1) { 4 } else { 0 };
            }
            action Consent when (phase == 1 && launches <= Cap - 1) {
                fresh = 1;
                polling = 1;
                phase = 2;
            }
            action SpendBanked when (Buggy == 1 && phase == 0 && obs == 0 && launches <= Cap - 1) {
                polling = 1;
                phase = 2;
            }
            action Recheck when (phase == 2) {
                obs = if (lock == 0) { world } else { obs };
                armed = if (lock == 0 && world == 0) { 1 } else { 0 };
                fresh = if (lock == 0 && world == 0) { fresh } else { 0 };
                lock = if (lock == 0 && world == 0) { 1 } else { lock };
                polling = if (lock == 0 && world == 0) { 1 } else { 0 };
                phase = if (lock == 0 && world == 0) { 3 } else { 0 };
            }
            action SkipRecheck when (Buggy == 1 && phase == 2 && lock == 0) {
                lock = 1;
                phase = 3;
            }
            action IgnorePeerLock when (Buggy == 1 && phase == 2 && lock == 2 && world == 0) {
                obs = world;
                armed = 1;
                phase = 3;
            }
            action Launch when (phase == 3) {
                stale = 0;
                launches = launches + 1;
                inflight = inflight + 1;
                unarmed = if (armed == 1) { unarmed } else { unarmed + 1 };
                unconsented = if (fresh == 1) { unconsented } else { unconsented + 1 };
                armed = 0;
                fresh = 0;
                phase = 4;
            }
            action ChildApplied when (phase == 4 && lock <= 1 && inflight > 0 && world == 0) {
                world = 1;
                inflight = inflight - 1;
                lock = if (lock == 1) { 0 } else { lock };
                phase = 5;
            }
            action ChildClobbers when (phase == 4 && lock <= 1 && inflight > 0 && world == 2) {
                world = 1;
                clobbered = 1;
                inflight = inflight - 1;
                lock = if (lock == 1) { 0 } else { lock };
                phase = 5;
            }
            action ChildUnchanged when (phase == 4 && lock <= 1 && inflight > 0) {
                inflight = inflight - 1;
                lock = if (lock == 1) { 0 } else { lock };
                phase = 5;
            }
            action Notice when (phase == 5 && polling == 1) {
                phase = 6;
            }
            action ClaimFromExitCode when (Buggy == 1 && phase == 5) {
                claim = 1;
            }
            action RetryAfterCancel when (Buggy == 1 && phase == 6 && lock == 0 && world == 0 && launches <= Cap - 1) {
                lock = 1;
                armed = 1;
                phase = 3;
            }
            action ResultWakeLost when (Buggy == 1 && phase == 5 && polling == 1) {
                polling = 0;
            }
            action Adopt when (phase == 0 && stale == 1) {
                stale = 0;
                polling = 1;
                phase = 6;
            }
            action AdoptLive when (Buggy == 1 && phase == 0 && lock == 2) {
                misadopted = 1;
                polling = 1;
                phase = 6;
            }
            action Follow when (phase == 0 && lock == 2) {
                polling = 1;
                phase = 4;
            }
            action Discard when (phase > 0 && stale == 1) {
                stale = 0;
            }
            action PeerLaunch when (lock == 0 && inflight <= Cap - 1) {
                lock = 2;
                inflight = inflight + 1;
                stale = 0;
            }
            action PeerExit when (lock == 2 && inflight > 0) {
                lock = 0;
                inflight = inflight - 1;
                stale = if (phase == 4) { 0 } else { 1 };
                phase = if (phase == 4) { 5 } else { phase };
            }
            action PeerSettle when (stale == 1) {
                stale = 0;
            }
            action SetOutside when (world == 0) {
                world = 1;
            }
            action Drift when (world == 1) {
                world = 0;
            }
            action Customize when (world <= 1) {
                world = 2;
            }
            action Normalize when (world == 2) {
                world = 0;
            }

            invariant NoActWithoutConsent: unconsented == 0;
            invariant OneChild: inflight <= 1;
            invariant ClaimIsObserved: claim == 0 || obs == 1;
            invariant EveryLaunchFollowsAFreshRead: unarmed == 0;
            invariant UnderWayIsPolled: (phase <= 1 && (lock == 0 || lock == 2)) || polling == 1;
            invariant ReReadsOnlyAnEndedChange: misadopted == 0;
            invariant AtMostOneOffer: reasked == 0;
            invariant NoOfferOverAChange: overlapped == 0;
        }
    }
}

/// Liveness specification (`[]<>(phase <= 4)`) for [`debugger_access_change_model`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(debugger_access_change_model)]
pub fn debugger_access_change_liveness() -> Liveness {
    Liveness {
        name: "AnExitedChangeIsRead",
        goal: le(var("phase"), int(4)),
        weak: vec!["Notice", "Probe"],
        strong: vec![],
        mutants: vec!["ResultWakeLost"],
    }
}
