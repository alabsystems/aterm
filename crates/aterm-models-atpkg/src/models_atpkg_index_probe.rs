// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The store-scoped atpkg index hint's lock, stamp, and cooldown contract.

use aterm_spec::derive::*;

/// Paces the next local index-hint worker from the previous probe's shared-stamp completion rather than its start.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_index_probe_completion_cadence_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgIndexProbeCompletionCadence {
            const Buggy = 0;
            const Interval = 6;
            const MaxAge = 7;
            // 0 idle, 1 network in flight, 2 stamped and waiting, 3 next probe.
            var phase = 0;
            var start_age = 0;
            var stamp_age = 0;
            var checked_early = 0;

            action Start when (phase == 0) {
                phase = 1;
            }
            action NetworkTick when (phase == 1 && start_age == 0) {
                start_age = 1;
            }
            action Complete when (phase == 1 && start_age == 1) {
                phase = 2;
                stamp_age = 0;
            }
            action Tick when (phase == 2 && stamp_age <= MaxAge - 1) {
                start_age = if start_age <= MaxAge - 1 { start_age + 1 } else { start_age };
                stamp_age = stamp_age + 1;
            }
            action LocalCheck when (
                phase == 2 && start_age > Interval - 1 && checked_early == 0
            ) {
                phase = if Buggy == 1 || stamp_age > Interval - 1 { 3 } else { 2 };
                checked_early = 1;
            }
            action DueCheck when (phase == 2 && stamp_age > Interval - 1) {
                phase = 3;
            }

            invariant NoProbeBeforeSharedStampExpires:
                phase <= 2 || stamp_age > Interval - 1;
        }
    }
}

/// Schedules the next local index probe at an active shared stamp's remaining expiry instead of starting a fresh local interval.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_index_shared_handoff_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgIndexSharedHandoff {
            const Buggy = 0;
            const Interval = 6;
            var stamp_age = 0;
            // 0 no stamp, 1 shared stamp live, 2 second host waiting, 3 expired and checked.
            var phase = 0;
            var local_wait = 0;
            var requests = 0;
            var missed = 0;

            action Stamp when (phase == 0) {
                phase = 1;
            }
            action TickBeforeHandoff when (phase == 1 && stamp_age <= Interval - 2) {
                stamp_age = stamp_age + 1;
            }
            action Handoff when (phase == 1 && stamp_age > 0 && stamp_age <= Interval - 1) {
                phase = 2;
                local_wait = if Buggy == 1 { Interval } else { Interval - stamp_age };
            }
            action TickAfterHandoff when (phase == 2 && stamp_age <= Interval - 1) {
                stamp_age = stamp_age + 1;
                local_wait = if local_wait > 0 { local_wait - 1 } else { 0 };
            }
            action AtExpiry when (phase == 2 && stamp_age == Interval) {
                phase = 3;
                requests = if local_wait == 0 { 1 } else { 0 };
                missed = if local_wait == 0 { 0 } else { 1 };
            }

            invariant NoSecondLocalCooldown: missed == 0;
        }
    }
}

/// Models single-worker ownership, priority ordering (bump > full-pass > index > vendor), generation invalidation, and non-blocking vendor fallback across GUI package park slices.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_index_pending_park_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgIndexPendingPark {
            const Buggy = 0;
            // 0 idle, 1 running, 2 finished, 3 harvested, 4 illegal second worker.
            var phase = 0;
            var published = 0;
            var near_offered = 0;
            var late_near = 0;
            // 0 no answer, 1 published, 2 missing.
            var answer = 0;
            var pass_generation = 0;
            var probe_generation = 0;
            var unlanded = 0;
            var vendor = 0;
            var vendor_retry = 0;
            var inline_vendor_get = 0;
            var bump = 0;
            var full_due = 0;
            // 0 undecided, 1 bump, 2 full-pass deadline, 3 index, 4 vendor.
            var chosen = 0;
            var delivered = 0;
            var skipped_ready_index = 0;
            var extra_spawn_attempts = 0;
            var lost_vendor = 0;
            var stale_index_chosen = 0;

            action Start when (phase == 0 && chosen == 0) {
                phase = 1;
                probe_generation = pass_generation;
            }
            action NearPublished when (
                phase == 1 && near_offered == 0 && chosen == 0 &&
                bump == 0 && full_due == 0 && pass_generation == probe_generation
            ) {
                near_offered = 1;
                published = 1;
                unlanded = 0;
            }
            action NearAfterPass when (
                phase > 0 && phase <= 2 && near_offered == 1 && late_near == 0 &&
                chosen == 0 && pass_generation > probe_generation
            ) {
                near_offered = 2;
                late_near = 1;
                published = 1;
                delivered = 0;
                unlanded = 0;
            }
            action FinishPublished when (phase == 1) {
                phase = 2;
                answer = 1;
            }
            action FinishMissing when (phase == 1 && near_offered == 0) {
                phase = 2;
                answer = 2;
            }
            action FullPass when (phase > 0 && phase <= 2 && pass_generation == 0) {
                pass_generation = pass_generation + 1;
                unlanded = 1;
            }
            action Harvest when (phase == 2) {
                phase = 3;
                published = if late_near == 1 && delivered == 0 {
                    1
                } else if answer == 1 && near_offered == 0 &&
                    (pass_generation == probe_generation || Buggy == 1) {
                    1
                } else {
                    0
                };
                unlanded = if (answer == 1 || answer == 2) &&
                    (pass_generation == probe_generation || Buggy == 1) {
                    0
                } else {
                    unlanded
                };
            }
            action VendorReady when (chosen == 0 && vendor == 0) {
                vendor = 1;
            }
            action VendorSpawnFailed when (
                phase <= 2 && vendor_retry == 0
            ) {
                vendor_retry = 1;
                inline_vendor_get = if Buggy == 1 { 1 } else { 0 };
            }
            action BumpReady when (chosen == 0 && bump == 0) {
                bump = 1;
            }
            action FullDue when (chosen == 0 && full_due == 0) {
                full_due = 1;
            }
            action ChooseBump when (chosen == 0 && bump == 1) {
                chosen = 1;
            }
            action ChooseFull when (chosen == 0 && bump == 0 && full_due == 1) {
                chosen = 2;
            }
            action ChooseIndex when (
                chosen == 0 && bump == 0 && full_due == 0 &&
                phase > 0 && phase <= 3 && published == 1 && delivered == 0 &&
                (pass_generation == probe_generation || late_near == 1 || Buggy == 1)
            ) {
                chosen = 3;
                delivered = 1;
                stale_index_chosen = if pass_generation == probe_generation || late_near == 1 {
                    0
                } else {
                    1
                };
            }
            action ChooseVendor when (
                chosen == 0 && bump == 0 && full_due == 0 && vendor == 1 &&
                (published == 0 || delivered == 1 || Buggy == 1)
            ) {
                skipped_ready_index = if published == 1 && delivered == 0 {
                    1
                } else {
                    0
                };
                chosen = 4;
            }
            action NextPark when (chosen > 0) {
                lost_vendor = if vendor == 1 && chosen <= 3 && Buggy == 1 {
                    1
                } else {
                    lost_vendor
                };
                vendor = if chosen == 4 || Buggy == 1 { 0 } else { vendor };
                chosen = 0;
                bump = 0;
                full_due = 0;
            }
            action Rearm when (phase == 3 && (published == 0 || delivered == 1)) {
                phase = 0;
                published = 0;
                near_offered = 0;
                late_near = 0;
                answer = 0;
                delivered = 0;
            }
            action SpawnExtra when (phase == 1 && extra_spawn_attempts == 0) {
                extra_spawn_attempts = 1;
                phase = if Buggy == 1 { 4 } else { phase };
            }

            invariant OneOwnedIndexWorker: phase <= 3;
            invariant ReadyIndexBeforeVendor: skipped_ready_index == 0;
            invariant NoStaleAnswerApplied:
                phase <= 2 || pass_generation == probe_generation ||
                (published == 0 && unlanded == 1) || late_near == 1;
            invariant NoStaleNearChosen:
                stale_index_chosen == 0;
            invariant NoInlineVendorGetOnHelperFailure: inline_vendor_get == 0;
            invariant ReadyVendorSurvivesPreemption: lost_vendor == 0;
        }
    }
}

/// Models single-owner locking, per-outcome cooldown TTLs (1 tick for missing/published, 2 for error), and floor-advance bypass for `atpkg::index_probe::probe_next`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_index_probe_cooldown_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgIndexProbeCooldown {
            const Buggy = 0;
            const MaxAge = 3;
            // 0 unlocked, 1 lock held, 2 decided/written, 3 second owner.
            var phase = 0;
            var floor = 1;
            var stamp_floor = 0;
            // 0 absent, 1 missing, 2 error, 3 published.
            var marker = 0;
            var ttl = 0;
            var age = 3;
            var duplicate = 0;

            action Acquire when (
                phase == 0 || (Buggy == 1 && phase == 1)
            ) {
                phase = if phase == 1 { 3 } else { 1 };
            }
            action SkipFresh when (
                phase == 1 && stamp_floor == floor && age <= ttl - 1
            ) {
                phase = 2;
            }
            action Abort when (phase == 1) {
                phase = 0;
            }
            action Release when (phase == 2) {
                phase = 0;
            }
            action AdvanceTick when (phase == 0 && age <= MaxAge - 1) {
                age = age + 1;
            }
            action AdvanceFloor when (phase == 0 && floor == 1) {
                floor = 2;
            }
            action ProbeMissing when (
                phase == 1 &&
                (stamp_floor <= floor - 1 || age > ttl - 1 || Buggy == 1)
            ) {
                duplicate = if stamp_floor == floor && age <= ttl - 1 {
                    1
                } else {
                    duplicate
                };
                stamp_floor = floor;
                marker = 1;
                ttl = 1;
                age = 0;
                phase = 2;
            }
            action ProbeError when (
                phase == 1 &&
                (stamp_floor <= floor - 1 || age > ttl - 1 || Buggy == 1)
            ) {
                duplicate = if stamp_floor == floor && age <= ttl - 1 {
                    1
                } else {
                    duplicate
                };
                stamp_floor = floor;
                marker = 2;
                ttl = if Buggy == 1 { 1 } else { 2 };
                age = 0;
                phase = 2;
            }
            action ProbePublished when (
                phase == 1 &&
                (stamp_floor <= floor - 1 || age > ttl - 1 || Buggy == 1)
            ) {
                duplicate = if stamp_floor == floor && age <= ttl - 1 {
                    1
                } else {
                    duplicate
                };
                stamp_floor = floor;
                marker = 3;
                ttl = if Buggy == 1 { 10 } else { 1 };
                age = 0;
                phase = 2;
            }
            invariant OneOwner: phase <= 2;
            invariant NoDuplicateRangeInsideCooldown: duplicate == 0;
            invariant StampMatchesOutcome:
                (marker == 0 && ttl == 0) ||
                (marker == 1 && ttl == 1) ||
                (marker == 2 && ttl == 2) ||
                (marker == 3 && ttl == 1);
        }
    }
}

/// Selects the highest published build from the two-HEAD probe (`floor + 1`, `floor + 2`), preferring uncertainty (`Deferred`) over `Missing`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_index_successor_selection_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgIndexSuccessorSelection {
            const Buggy = 0;
            var seen = 0;
            var first = 0;
            var highest = 0;
            var uncertain = 0;
            // 0 undecided, 1 missing, 2 deferred, 3/5 published (floor+1, floor+2).
            var chosen = 0;

            action ObserveMissing when (seen <= 1) {
                seen = seen + 1;
            }
            action ObserveDeferred when (seen <= 1) {
                seen = seen + 1;
                uncertain = 1;
            }
            action ObserveLow when (seen <= 1) {
                seen = seen + 1;
                first = if first == 0 { 3 } else { first };
                highest = if highest <= 2 { 3 } else { highest };
            }
            action ObserveHigh when (seen <= 1) {
                seen = seen + 1;
                first = if first == 0 { 5 } else { first };
                highest = 5;
            }
            action Choose when (seen == 2 && chosen == 0) {
                chosen = if Buggy == 1 && first > 0 {
                    first
                } else if Buggy == 1 {
                    1
                } else if highest > 0 {
                    highest
                } else if uncertain == 1 {
                    2
                } else {
                    1
                };
            }
            invariant BestAvailableHint:
                chosen == 0 ||
                (highest > 0 && chosen == highest) ||
                (highest == 0 && uncertain == 1 && chosen == 2) ||
                (highest == 0 && uncertain == 0 && chosen == 1);
        }
    }
}

/// Tracks the highest unlanded index high-water mark so lower hints cannot replay an older failed pass or downgrade the mark.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_index_wake_highwater_model() -> Model {
    aterm_spec::ty_model! {
        AtpkgIndexWakeHighwater {
            const Buggy = 0;
            var unlanded = 0;
            var wake = 0;
            var replay = 0;
            var downgraded = 0;

            action FailLow {
                downgraded = if Buggy == 1 && unlanded > 1 { 1 } else { downgraded };
                unlanded = if Buggy == 1 || unlanded == 0 { 1 } else { unlanded };
                wake = 0;
            }
            action FailMiddle {
                downgraded = if Buggy == 1 && unlanded > 2 { 1 } else { downgraded };
                unlanded = if Buggy == 1 || unlanded <= 1 { 2 } else { unlanded };
                wake = 0;
            }
            action FailHigh {
                unlanded = 3;
                wake = 0;
            }
            action HintLow {
                replay = if unlanded > 1 && Buggy == 1 {
                    1
                } else {
                    replay
                };
                wake = if unlanded > 0 && (Buggy == 0 || unlanded == 1) { 0 } else { 1 };
                unlanded = if unlanded > 0 && (Buggy == 0 || unlanded == 1) {
                    unlanded
                } else { 0 };
            }
            action HintMiddle {
                replay = if unlanded > 2 && Buggy == 1 {
                    1
                } else {
                    replay
                };
                wake = if unlanded > 1 && (Buggy == 0 || unlanded == 2) { 0 } else { 2 };
                unlanded = if unlanded > 1 && (Buggy == 0 || unlanded == 2) {
                    unlanded
                } else { 0 };
            }
            action HintHigh {
                wake = if unlanded == 3 { 0 } else { 3 };
                unlanded = if unlanded == 3 { unlanded } else { 0 };
            }
            action Missing {
                unlanded = 0;
                wake = 0;
            }
            action Deferred {
                wake = 0;
            }
            invariant NoReplayOfOlderHint: replay == 0;
            invariant FailureMarkNeverDowngrades: downgraded == 0;
        }
    }
}
