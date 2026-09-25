// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The store-scoped atpkg index hint's lock, stamp, and cooldown contract.

use super::*;

/// The GUI package lane owns at most one asynchronous index probe across park
/// slices. A positive HEAD may be offered while the probe's other HEAD is still
/// out; its final answer cannot offer that same build twice. A
/// vendor completion may be chosen while the probe is still running; an index
/// answer already offered at the decision point wins over the vendor.
/// A vendor answer harvested in that same slice survives the index, bump, or
/// full-pass priority choice until the next park can deliver or recheck it.
/// A bump and an expired full-pass deadline win before either hint. `NextPark`
/// keeps an unfinished worker owned after a targeted pass. A full seed/update
/// pass invalidates the in-flight final answer without releasing its worker slot.
/// A higher near HEAD arriving after that pass may wake another signed pass.
/// Thread pressure cannot turn a failed vendor helper spawn into a blocking GET,
/// whether or not the index answer is outstanding. Tier-1 drives the real
/// worker handle, package park, and vendor spawn-failure path.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_index_pending_park_model() -> Model {
    crate::ty_model! {
        AtpkgIndexPendingPark {
            const Buggy = 0;
            // 0 idle, 1 running, 2 finished but owned, 3 harvested,
            // 4 an illegal second worker.
            var phase = 0;
            // An actionable published answer is available after either a near
            // positive or the final harvest.
            var published = 0;
            var near_offered = 0;
            // A higher near HEAD arrived after the pass began; unlike the
            // worker's final answer, this is a fresh hint.
            var late_near = 0;
            // 0 no answer, 1 published, 2 missing. A finished worker stays owned.
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
            // A second start call while one worker is owned is a real, rejected
            // transition in the GUI. Record that attempt at the committed dial;
            // the Buggy dial instead admits an illegal second owner.
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

/// One `Probe*` action is one locked attempt — the pair of HEADs, `floor + 1` and
/// `floor + 2` — before its single stamp is written. `age` advances in 30-second ticks
/// (`atpkg::index_probe::INTERVAL`); a missing pair and a published hint cool for one
/// tick, an error (the download host's 429 included) for ten. A changed durable floor
/// bypasses an old stamp immediately. Tier-1 drives `atpkg::index_probe::probe_next` and
/// its real lock/stamp file.
///
/// Until 2026-09-23 a second locked range (`floor + 3`, `floor + 4`, every five minutes)
/// also fetched an anonymous Releases listing when its HEADs missed, and a rate-limited
/// listing stamped an hour (`ProbeRateLimited`, marker 4). Every cooldown held, so nothing
/// here was violated, yet in the steady state that listing ran every five minutes on every
/// machine (audit PK-2): a request budget, not a cooldown. It is gone with its action and
/// its range (owner ruling R3; the publisher's contiguous index builds left the far range
/// nothing to find). That the probe spends no metered request is a fact of the code, whose
/// only transport is the HEAD, not a property of this model; `atpkg::index_probe`'s tests
/// pin it with a recording transport over a hundred steady-state ticks.
///
/// `Buggy=1` admits a second lock owner and a second attempt inside its cooldown,
/// lengthens the published cooldown to five minutes, and shortens the error cooldown to
/// one tick. Each invariant has its own counterexample: mutual exclusion alone is
/// insufficient to prevent duplicate work after the first owner releases the lock.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_index_probe_cooldown_model() -> Model {
    crate::ty_model! {
        AtpkgIndexProbeCooldown {
            const Buggy = 0;
            // Saturates past the longest cooldown (an error's ten ticks).
            const MaxAge = 11;
            // 0 unlocked, 1 lock held before stamp decision, 2 decided/written.
            var phase = 0;
            var floor = 1;
            var stamp_floor = 0;
            // marker: 0 absent, 1 missing, 2 error, 3 published.
            var marker = 0;
            var ttl = 0;
            var age = 11;
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
            // An owner dying before it writes a stamp releases the OS lock;
            // the next process may retry because no decision was published.
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
                ttl = if Buggy == 1 { 1 } else { 10 };
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
                (marker == 2 && ttl == 10) ||
                (marker == 3 && ttl == 1);
        }
    }
}

/// The probe's two HEADs — `floor + 1` (Low) and `floor + 2` (High) — are aggregated
/// after both have had their chance to run. A published higher build wins over an
/// earlier lower build and over uncertainty; `Missing` is valid only when both HEADs
/// really answered missing. `Buggy=1` replays the old first-hit and false-missing
/// behavior. Tier-1 drives `atpkg::index_probe::successor_with` over real HEAD answers.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_index_successor_selection_model() -> Model {
    crate::ty_model! {
        AtpkgIndexSuccessorSelection {
            const Buggy = 0;
            var seen = 0;
            var first = 0;
            var highest = 0;
            var uncertain = 0;
            // 0 undecided, 1 missing, 2 deferred, 3/5 published ranks (floor+1, floor+2).
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

/// The GUI keeps the highest index a full pass did not land. A lower hint from
/// an unstamped near range cannot replay an older pass while the farther range
/// is stamped. The model describes the decisions, with the caller's minute
/// cadence projected by Tier-1 rather than duplicated here. `Buggy=1` restores
/// equality-only suppression and lowers the mark on a late failed pass.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn atpkg_index_wake_highwater_model() -> Model {
    crate::ty_model! {
        AtpkgIndexWakeHighwater {
            const Buggy = 0;
            // Ranks 1, 2, 3 map to test builds 45, 49, 50.
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
