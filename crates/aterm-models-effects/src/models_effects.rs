// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sparkle, ignition, cursor-cat, rainbow-kitty, and error-class effect models.

use aterm_spec::derive::*;

/// Rate-limited pointer stroke detector model with contact, travel, and cooldown obligations.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn pet_stroke_detector_model() -> Model {
    aterm_spec::ty_model! {
        PetStrokeDetector {
            const Buggy = 0;
            var contact = 0;
            var progress = 0;
            var cooling = 0;
            var cooldown_due = 0;
            var emitted = 0;
            var event = 0;

            action Seed when (contact == 0) {
                contact = 1; progress = 0; emitted = 0; event = 1;
            }
            action Partial when (contact == 1 && cooling == 0) {
                progress = 1; emitted = if Buggy == 1 { 1 } else { 0 }; event = 2;
            }
            action Still when (contact == 1) {
                emitted = if Buggy == 1 { 1 } else { 0 }; event = 3;
            }
            action Earn when (contact == 1 && progress == 1 && cooling == 0) {
                progress = 0;
                cooling = if Buggy == 1 { 0 } else { 1 };
                cooldown_due = 1; emitted = 1; event = 4;
            }
            action CoolingMove when (contact == 1 && cooling == 1) {
                progress = 0; emitted = if Buggy == 1 { 1 } else { 0 }; event = 5;
            }
            action Reset when (contact == 1) {
                contact = if Buggy == 1 { contact } else { 0 };
                progress = if Buggy == 1 { progress } else { 0 };
                cooling = if Buggy == 1 { 0 } else { cooling };
                emitted = 0; event = 6;
            }
            action Reject when (contact == 1) {
                progress = 0; emitted = 0; event = 7;
            }
            action ExpireCooldown when (cooldown_due == 1) {
                cooling = 0; cooldown_due = 0; emitted = 0; event = 8;
            }

            invariant OnlyCompleteStrokesEarn:
                if event == 4 { emitted <= 1 } else { emitted == 0 };
            invariant CooldownCannotBeBypassed:
                if cooldown_due == 1 { cooling == 1 } else { cooling <= 1 };
            invariant ResetForgetsContact:
                if event == 6 { contact == 0 && progress == 0 } else { contact <= 1 };
            bound StrokeStateBounded:
                contact <= 1 && progress <= 1 && cooling <= 1 && cooldown_due <= 1
                    && emitted <= 1 && event <= 8;
        }
    }
}

/// Sparkle-word episode lifecycle model over the grace-TTL persistence map.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn sparkle_identity_model() -> Model {
    aterm_spec::ty_model! {
        SparkleIdentity {
            const Buggy = 0;
            const GraceMax = 3;
            const MaxBirths = 3;
            const MaxRekeys = 2;
            const MaxContextMoves = 2;
            var state = 0;
            var births = 0;
            var rolls = 0;
            var age = 0;
            var nova = 0;
            var done = 0;
            var fires = 0;
            var rekeys = 0;
            var context = 0;
            var context_moves = 0;
            var recognized_moves = 0;
            var false_births = 0;
            action Appear when (state == 0 && births <= MaxBirths - 1) {
                state = 1; births = births + 1; rolls = rolls + 1; nova = 0;
            }
            action Vanish when (state == 1) { state = 2; age = 0; }
            action Rehit when (state == 2) {
                state = 1; age = 0;
                rolls = if Buggy == 1 { rolls + 1 } else { rolls };
            }
            action Rekey when (state == 1 && rekeys <= MaxRekeys - 1) {
                rekeys = rekeys + 1;
                rolls = if Buggy == 1 { rolls + 1 } else { rolls };
                nova = if Buggy == 1 { 0 } else { nova };
                done = if Buggy == 1 { 0 } else { done };
            }
            action ContextMove when (state == 1
                && context_moves <= MaxContextMoves - 1
                && rekeys <= MaxRekeys - 1
                && births <= MaxBirths - 1)
            {
                context = if context == 0 { 1 } else { 0 };
                context_moves = context_moves + 1;
                recognized_moves = if Buggy == 1 {
                    recognized_moves
                } else {
                    recognized_moves + 1
                };
                false_births = if Buggy == 1 { false_births + 1 } else { false_births };
                rekeys = if Buggy == 1 { rekeys } else { rekeys + 1 };
                births = if Buggy == 1 { births + 1 } else { births };
                rolls = if Buggy == 1 { rolls + 1 } else { rolls };
                nova = if Buggy == 1 { 0 } else { nova };
                done = if Buggy == 1 { 0 } else { done };
            }
            action Tick when (state == 2 && age <= GraceMax - 1) { age = age + 1; }
            action Expire when (state == 2 && age == GraceMax) { state = 0; }
            action Ignite when (state == 1
                && nova + (if Buggy == 1 { 0 } else { done }) <= if Buggy == 1 { 1 } else { 0 })
            {
                nova = nova + 1; fires = fires + 1; done = 1;
            }
            invariant GenomeFrozen: rolls == births;
            invariant OneNovaPerEpisode: nova <= 1;
            bound AgeBounded: age <= GraceMax;
            bound RekeysBounded: rekeys <= MaxRekeys;
            bound ContextBounded: context <= 1;
            bound ContextMovesBounded: context_moves <= MaxContextMoves;
            invariant RecognitionComplete: recognized_moves == context_moves;
            invariant NoFalseBirths: false_births == 0;
            invariant PlayedOnce: fires <= 1;
        }
    }
}

/// Bounded repeated-surface matching model complementing [`sparkle_identity_model`].
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn sparkle_reflow_cardinality_model() -> Model {
    aterm_spec::ty_model! {
        SparkleReflowCardinality {
            const Buggy = 0;
            const OldCount = 2;
            const WeakMaxScans = 2;
            var state = 0;
            var global_redraw = 0;
            var stationary_anchor = 0;
            var typed_retype = 0;
            var recent_typed_retype = 0;
            var blank_grace = 0;
            var recent = 0;
            var seq_gap = 0;
            var stale_same_seed = 0;
            var exact_context = 0;
            var continuity_tainted = 0;
            var new_count = 0;
            var logical_new = 0;
            var expected_fresh = 0;
            var expected_recognized = 0;
            var transferred = 0;
            var recognized = 0;
            var fresh = 0;
            var armed = 0;
            var false_births = 0;
            var false_transfers = 0;
            action MovePair when (state == 0) {
                state = 1;
                global_redraw = 1;
                stationary_anchor = if Buggy == 5 { 1 } else { 0 };
                recent = 1;
                seq_gap = 1;
                new_count = 2;
                logical_new = 0;
                expected_fresh = 0;
                expected_recognized = OldCount;
                transferred = if Buggy == 1 { 0 } else { if Buggy == 2 { 1 } else { OldCount } };
                recognized = if Buggy == 1 { 0 } else { if Buggy == 2 { 1 } else { OldCount } };
                fresh = if Buggy == 1 { 2 } else { 0 };
                armed = if Buggy == 1 { 2 } else { 0 };
                false_births = if Buggy == 1 { 2 } else { 0 };
            }
            action GrowOne when (state == 0) {
                state = 1;
                global_redraw = 1;
                recent = 1;
                seq_gap = 1;
                new_count = 3;
                logical_new = 1;
                expected_fresh = 1;
                expected_recognized = OldCount;
                transferred = if Buggy == 1 {
                    0
                } else {
                    if Buggy == 3 { OldCount + 1 } else { OldCount }
                };
                recognized = if Buggy == 1 { 0 } else { OldCount };
                fresh = if Buggy == 1 { 3 } else { if Buggy == 3 { 0 } else { 1 } };
                armed = if Buggy == 1 {
                    3
                } else {
                    if Buggy == 3 { 0 } else { if Buggy == 4 { 2 } else { 1 } }
                };
                false_births = if Buggy == 1 { 2 } else { 0 };
                false_transfers = if Buggy == 3 { 1 } else { 0 };
            }
            action RotatePair when (state == 0) {
                state = 1;
                stationary_anchor = 1;
                recent = 1;
                seq_gap = 1;
                new_count = 2;
                logical_new = 1;
                expected_fresh = 1;
                expected_recognized = 1;
                transferred = if Buggy == 1 { 2 } else { 1 };
                recognized = 1;
                fresh = if Buggy == 1 { 0 } else { 1 };
                armed = if Buggy == 1 { 0 } else { 1 };
                false_transfers = if Buggy == 1 { 1 } else { 0 };
            }
            action BlankGrace when (state == 0) {
                state = 1;
                blank_grace = 1;
                typed_retype = if Buggy == 10 { 1 } else { 0 };
                recent = if Buggy == 8 { 1 } else { 0 };
                seq_gap = 3;
                stale_same_seed = 1;
                exact_context = 1;
                continuity_tainted = if Buggy == 9 { 1 } else { 0 };
                new_count = 1;
                logical_new = 0;
                expected_fresh = 0;
                expected_recognized = 1;
                transferred = if Buggy == 1 { 0 } else { 1 };
                recognized = if Buggy == 1 { 0 } else { 1 };
                fresh = if Buggy == 1 { 1 } else { 0 };
                armed = if Buggy == 1 { 1 } else { 0 };
                false_births = if Buggy == 1 { 1 } else { 0 };
            }
            action TypedRetype when (state == 0) {
                state = 1;
                typed_retype = 1;
                blank_grace = if Buggy == 11 { 1 } else { 0 };
                recent = 0;
                seq_gap = 3;
                stale_same_seed = 1;
                exact_context = 1;
                continuity_tainted = 1;
                new_count = 1;
                logical_new = 1;
                expected_fresh = 1;
                expected_recognized = 0;
                transferred = if Buggy == 1 { 1 } else { 0 };
                recognized = 0;
                fresh = if Buggy == 1 { 0 } else { 1 };
                armed = if Buggy == 1 { 0 } else { 1 };
                false_transfers = if Buggy == 1 { 1 } else { 0 };
            }
            action RecentTypedRetype when (state == 0) {
                state = 1;
                typed_retype = if Buggy == 6 { 0 } else { 1 };
                recent_typed_retype = 1;
                recent = if Buggy == 7 { 0 } else { 1 };
                seq_gap = 2;
                stale_same_seed = 0;
                exact_context = 1;
                continuity_tainted = 1;
                new_count = 1;
                logical_new = 1;
                expected_fresh = 1;
                expected_recognized = 0;
                transferred = if Buggy == 1 { 1 } else { 0 };
                recognized = 0;
                fresh = if Buggy == 1 { 0 } else { 1 };
                armed = if Buggy == 1 { 0 } else { 1 };
                false_transfers = if Buggy == 1 { 1 } else { 0 };
            }
            invariant CandidateAccounting: transferred + fresh == new_count;
            invariant SurvivorsBounded: transferred <= OldCount;
            invariant ScenarioSelected:
                global_redraw + stationary_anchor + blank_grace + typed_retype == state;
            invariant GlobalRedrawHasNoAnchor: global_redraw + stationary_anchor <= 1;
            invariant LicensedEdgesAreRecent:
                global_redraw + stationary_anchor <= recent;
            invariant RecentTypedRetypeIsRecent: recent_typed_retype <= recent;
            invariant RecentTypedRetypeIsTyped:
                recent_typed_retype <= typed_retype;
            invariant BlankGraceOutsideWindow: blank_grace + recent <= 1;
            invariant StaleSameSeedCases:
                stale_same_seed + recent_typed_retype == blank_grace + typed_retype;
            invariant ExactContextCases:
                exact_context == blank_grace + typed_retype;
            invariant TaintSelectsTypedRetype: continuity_tainted == typed_retype;
            invariant BlankGraceUntainted: blank_grace + continuity_tainted <= 1;
            invariant StaleSameSeedOutsideWindow: stale_same_seed + recent <= 1;
            invariant TypedRetypeIsLogicalNew: typed_retype <= logical_new;
            invariant BlankGraceIsContinuation: blank_grace + logical_new <= 1;
            invariant RecentMatchesScanWindow:
                if state == 0 {
                    seq_gap == 0
                } else {
                    if recent == 1 {
                        seq_gap <= WeakMaxScans
                    } else {
                        seq_gap > WeakMaxScans
                    }
                };
            bound SeqGapBounded: seq_gap <= WeakMaxScans + 1;
            invariant FreshMatchesExpected: fresh == expected_fresh;
            invariant ArmedMatchesExpected: armed == expected_fresh;
            invariant ArmedMatchesFresh: armed == fresh;
            invariant FreshAtMostNetGrowth: fresh <= expected_fresh;
            invariant ArmedAtMostNetGrowth: armed <= expected_fresh;
            invariant RecognitionComplete: recognized == expected_recognized;
            invariant NoFalseBirths: false_births == 0;
            invariant NoFalseTransfers: false_transfers == 0;
            bound StateBounded: state <= 1;
            bound NewCountBounded: new_count <= 3;
        }
    }
}

/// Model for repeated explicit feline retyping without session done-mark poisoning.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn sparkle_retype_rearm_model() -> Model {
    aterm_spec::ty_model! {
        SparkleRetypeRearm {
            const Buggy = 0;
            const MaxRetypes = 2;
            var retypes = 0;
            var armed = 0;
            var poisoned = 0;
            action TypeAgain when (retypes <= MaxRetypes - 1) {
                retypes = retypes + 1;
                armed = armed + (if Buggy == 1 && poisoned == 1 { 0 } else { 1 });
                poisoned = if Buggy == 1 { 1 } else { 0 };
            }
            invariant EveryRetypeArmed: armed == retypes;
            bound RetypesBounded: retypes <= MaxRetypes;
            bound ArmedBounded: armed <= MaxRetypes;
        }
    }
}

/// Capacity-boundary alignment transaction model for the sparkle identity persistence map.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn sparkle_persist_capacity_model() -> Model {
    aterm_spec::ty_model! {
        SparklePersistCapacity {
            const Buggy = 0;
            const Cap = 3;
            var resident = 3;
            var pulled = 0;
            var admitted = 0;
            var departed = 0;
            var phase = 0;
            action Pull when (phase == 0 && resident > 0) {
                resident = resident - 1;
                pulled = 1;
                phase = 1;
            }
            action Fresh when (phase == 1 && resident <= Cap - 1) {
                resident = resident + 1;
                admitted = admitted + 1;
                phase = 2;
            }
            action Reinsert when (phase == 2 && pulled == 1) {
                resident = resident + (if Buggy == 1 { 1 } else { 0 });
                pulled = 0;
                departed = departed + (if Buggy == 1 || Buggy == 2 { 0 } else { 1 });
                phase = 3;
            }
            invariant ResidentBounded: resident <= Cap;
            invariant Conservation:
                resident + pulled + departed == Cap + admitted;
            bound PulledBounded: pulled <= 1;
            bound PhaseBounded: phase <= 3;
        }
    }
}

/// Sparkle-word supernova phase machine model (`Armed` through `Settled`) with bounded re-arming.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn nova_phase_model() -> Model {
    aterm_spec::ty_model! {
        NovaPhase {
            const Buggy = 0;
            const MaxSteps = 18;
            const MaxArms = 2;
            var phase = 0;
            var steps = 0;
            var flashes = 0;
            var arms = 0;
            action Step when (phase <= 5 && steps <= MaxSteps - 1) {
                phase = phase + 1;
                steps = steps + 1;
                flashes = flashes + (if phase == 1 { 1 } else { 0 });
            }
            action Rearm when (
                phase == 6 && (arms <= MaxArms - 1 || (Buggy == 2 && arms <= MaxArms))
            ) {
                arms = arms + 1;
                phase = if Buggy == 1 { 2 } else { 0 };
                flashes = if Buggy == 1 { flashes + 1 } else { 0 };
            }
            bound Monotone: phase <= 6;
            invariant OneFlashPerArm: flashes <= 1;
            invariant CanSettle: steps + 6 <= MaxSteps + phase;
        }
    }
}

/// Rolling-second sparkle-word flash limiter model (at most 2 ignitions/s, 1 under overlap).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn flash_limiter_model() -> Model {
    aterm_spec::ty_model! {
        FlashLimiter {
            const Buggy = 0;
            const Overlap = 0;
            scenario Overlap = 1;
            var s0 = 0;
            var s1 = 0;
            var s2 = 0;
            var s3 = 0;
            action Shift { s3 = s2; s2 = s1; s1 = s0; s0 = 0; }
            action Ignite when (s0 + s1 + s2 + s3
                <= if Overlap == 1 { if Buggy == 1 { 1 } else { 0 } } else { 1 })
            { s0 = s0 + 1; }
            invariant IgnitionBound:
                s0 + s1 + s2 + s3 <= if Overlap == 1 { 1 } else { 2 };
            invariant RegionFlashPairs:
                (if Overlap == 1 { (s0 + s1 + s2 + s3) + (s0 + s1 + s2 + s3) }
                 else { 2 }) <= 3;
        }
    }
}

/// Window-wide flash limiter model parameterizing live enforcer count (`Instances`) and locality (`Local`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn flash_limiter_window_model() -> Model {
    aterm_spec::ty_model! {
        FlashLimiterWindow {
            const Buggy = 0;
            const Overlap = 0;
            const Local = 0;
            const Instances = 2;
            scenario Overlap = 1;
            var a0 = 0;
            var a1 = 0;
            var a2 = 0;
            var a3 = 0;
            var b0 = 0;
            var b1 = 0;
            var b2 = 0;
            var b3 = 0;
            var live2 = 0;

            action Shift {
                a3 = a2; a2 = a1; a1 = a0; a0 = 0;
                b3 = b2; b2 = b1; b1 = b0; b0 = 0;
            }
            action Spawn when (Instances > 1 && live2 == 0) { live2 = 1; }
            action IgniteA when ((if Local == 1 { a0 + a1 + a2 + a3 }
                                  else { a0 + a1 + a2 + a3 + b0 + b1 + b2 + b3 })
                <= if Overlap == 1 { if Buggy == 1 { 1 } else { 0 } } else { 1 })
            { a0 = a0 + 1; }
            action IgniteB when (live2 == 1
                && (if Local == 1 { b0 + b1 + b2 + b3 }
                    else { a0 + a1 + a2 + a3 + b0 + b1 + b2 + b3 })
                <= if Overlap == 1 { if Buggy == 1 { 1 } else { 0 } } else { 1 })
            { b0 = b0 + 1; }

            invariant WindowIgnitionBound:
                a0 + a1 + a2 + a3 + b0 + b1 + b2 + b3
                    <= if Overlap == 1 { 1 } else { 2 };
        }
    }
}

/// Two-way global burst mutex model over the shared `nova_add` decoration channel.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn supernova_burst_mutex_model() -> Model {
    aterm_spec::ty_model! {
        SupernovaBurstMutex {
            const Buggy = 0;
            const MaxNovaQuads = 1536;
            const SuperQuads = 900;
            const ClassicQuads = 392;
            const MaxActiveNovas = 3;
            var supers = 0;
            var classics = 0;
            var funded = 0;

            action IgniteSuper when (
                if Buggy == 1 { supers <= 1 } else { supers + classics <= 0 }
            ) {
                supers = supers + 1;
                funded = funded + SuperQuads;
            }
            action IgniteClassic when (
                classics <= MaxActiveNovas - 1
                    && (if Buggy == 1 { classics <= MaxActiveNovas } else { supers <= 0 })
            ) {
                classics = classics + 1;
                funded = funded + ClassicQuads;
            }
            action RetireSuper when (supers > 0) {
                supers = supers - 1;
                funded = funded - SuperQuads;
            }
            action RetireClassic when (classics > 0) {
                classics = classics - 1;
                funded = funded - ClassicQuads;
            }

            invariant AtMostOneSupernova: supers <= 1;
            invariant BurstsExclude:
                if supers > 0 { classics <= 0 } else { classics <= MaxActiveNovas };
            invariant DecorationShareFunded: funded <= MaxNovaQuads;
        }
    }
}

/// Ownership and cardinality model for delayed and recently fired sparkle-word ignition slots.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ignition_reservation_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        IgnitionReservationLifecycle {
            const Buggy = 0;
            const Cap = 3;
            const RecentCap = 2;
            var live = 0;
            var pending = 0;
            var recent = 0;

            action ReserveNow when (live <= Cap - 1 && recent <= RecentCap - 1) {
                live = live + 1;
                recent = recent + 1;
            }
            action ReserveFuture when (
                (live <= Cap - 1 || (Buggy == 3 && live <= Cap)) && pending <= Cap
            ) {
                live = live + 1;
                pending = pending + 1;
            }
            action CancelFuture when (live > 0 && pending > 0) {
                live = live - 1;
                pending = if Buggy == 1 { pending } else { pending - 1 };
            }
            action FirePending when (
                pending > 0 && (recent <= RecentCap - 1 || (Buggy == 2 && recent <= RecentCap))
            ) {
                pending = pending - 1;
                recent = recent + 1;
            }
            action DepartFired when (live > pending) {
                live = live - 1;
            }
            action ExpireRecent when (recent > 0) {
                recent = recent - 1;
            }

            invariant FutureOwned: pending <= live;
            invariant PendingBound: pending <= Cap;
            invariant RecentBound: recent <= RecentCap;
            invariant ReservationBound: pending + recent <= Cap + RecentCap;
            invariant LiveBound: live <= Cap;
        }
    }
}

/// Atomic ownership-transfer model for delayed ignitions across episode rekeying.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ignition_reservation_rekey_model() -> Model {
    aterm_spec::ty_model! {
        IgnitionReservationRekey {
            const Buggy = 0;
            var phase = 0;
            var reservation = 0;
            var owner_match = 0;
            var rekey_untracked = 0;
            var live_slot_pruned = 0;
            var original_flash = 0;
            var competing_flash = 0;

            action GrantDelayed when (phase == 0) {
                phase = 1;
                reservation = 1;
                owner_match = 1;
            }
            action Rekey when (phase == 1) {
                phase = 2;
                owner_match = if Buggy == 1 { 0 } else { 1 };
                rekey_untracked = if Buggy == 1 { 1 } else { 0 };
            }
            action Prune when (phase == 2) {
                phase = 3;
                reservation = if owner_match == 1 { 1 } else { 0 };
                live_slot_pruned = if owner_match == 1 { 0 } else { 1 };
            }
            action CompetingGrant when (phase == 3) {
                phase = 4;
                original_flash = 1;
                competing_flash = if reservation == 0 { 1 } else { 0 };
            }

            invariant RekeyOwnsReservation: rekey_untracked == 0;
            invariant DelayedSlotSurvivesPrune: live_slot_pruned == 0;
            invariant NoOverlappingFlash: original_flash + competing_flash <= 1;
            bound PhaseBounded: phase <= 4;
        }
    }
}

/// Constant-selection intrusive LRU model for completed sparkle-word episodes.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn done_mark_lru_model() -> Model {
    aterm_spec::ty_model! {
        DoneMarkLruBound {
            const Buggy = 0;
            const Cap = 3;
            var resident = 0;
            var selections = 0;
            var evictions = 0;
            var touches = 0;

            action Insert when (resident <= Cap - 1 || (Buggy == 2 && resident <= Cap)) {
                resident = resident + 1;
                selections = 0;
            }
            action Touch when (resident > 0 && touches == 0) {
                touches = touches + 1;
                selections = 0;
            }
            action ReplaceOldest when (resident == Cap && evictions == 0) {
                resident = resident;
                evictions = evictions + 1;
                selections = if Buggy == 1 { resident } else { 1 };
            }

            invariant ResidentBounded: resident <= Cap;
            invariant ConstantSelection: selections <= 1;
            bound EvictionsBounded: evictions <= 1;
            bound TouchesBounded: touches <= 1;
        }
    }
}

/// One-shot peek phase lifecycle model (`Idle -> Rise -> Dwell -> Descend -> Done`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn one_shot_peek_model() -> Model {
    aterm_spec::ty_model! {
        OneShotPeek {
            const Buggy = 0;
            const MaxSteps = 8;
            var phase = 0;
            var steps = 0;
            var rises = 0;
            action Start when (phase == 0 && steps <= MaxSteps - 1) {
                phase = 1; rises = rises + 1; steps = steps + 1;
            }
            action Step when (phase > 0 && phase <= 2 && steps <= MaxSteps - 1) {
                phase = phase + 1; steps = steps + 1;
            }
            action Finish when (phase == 3 && steps <= MaxSteps - 1) {
                phase = 4; steps = steps + 1;
            }
            action Repeek when (phase == 4 && steps <= MaxSteps - 1) {
                phase = if Buggy == 1 { 1 } else { if Buggy == 2 { 0 } else { phase } };
                rises = if Buggy == 1 { rises + 1 } else { rises };
                steps = steps + 1;
            }
            invariant NoRepeek: rises <= 1;
            bound PhaseBounded: phase <= 4;
            invariant CanFinish: steps + 4 <= MaxSteps + phase;
        }
    }
}

/// Cursor-cat trail-master gate and collectible discovery-hello lifecycle model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_cat_model() -> Model {
    aterm_spec::ty_model! {
        CursorCat {
            const Buggy = 0;
            const HoldTicks = 3;
            const TotalTicks = 5;
            const MaxHidden = 5;
            var phase = 0;
            var elapsed = 0;
            var presented = 0;
            var hidden = 0;
            var presentable = 0;
            var collections = 0;
            var visible = 0;
            var forced = 0;
            var presented_once = 0;
            var wall_expired = 0;
            var trail_master = 0;
            var ordinary_armed = 0;
            var ordinary_visible = 0;
            action EnableTrail when (trail_master == 0) {
                trail_master = 1;
            }
            action DisableTrail when (trail_master == 1) {
                trail_master = 0;
                visible = if Buggy == 4 { 0 } else { visible };
                ordinary_armed = 0;
                ordinary_visible = 0;
            }
            action TypeOrdinary when (trail_master == 1) {
                ordinary_armed = 1;
                ordinary_visible = 1;
            }
            action TypeWhileTrailOff when (trail_master == 0) {
                ordinary_armed = if Buggy == 1 { 1 } else { ordinary_armed };
                ordinary_visible = if Buggy == 1 { 1 } else { ordinary_visible };
            }
            action SettleOrdinary when (ordinary_armed == 1) {
                ordinary_armed = 0;
                ordinary_visible = 0;
            }
            action Collect when (phase == 0 && collections == 0) {
                phase = 1; elapsed = 0; presented = 0; hidden = 0;
                presentable = 1; collections = 1; visible = 1; forced = 1;
                presented_once = if Buggy == 3 { 0 } else { 1 }; wall_expired = 0;
            }
            action Tick when (collections == 1 && elapsed <= TotalTicks - 1) {
                elapsed = elapsed + 1;
                presented = presented + 1;
                presentable = 1;
                phase = if elapsed + 1 == TotalTicks { 0 }
                    else { if elapsed + 1 <= HoldTicks - 1 { 1 } else { 2 } };
                visible = if elapsed + 1 == TotalTicks { 0 } else { 1 };
                forced = if elapsed + 1 == TotalTicks { 0 }
                    else { if elapsed + 1 <= HoldTicks - 1 { 1 } else { 0 } };
            }
            action HiddenTick when (
                collections == 1 && hidden <= MaxHidden - 1 && elapsed <= TotalTicks - 1
            ) {
                hidden = hidden + 1;
                presentable = 0;
                visible = if Buggy == 2 { visible } else { 0 };
                elapsed = if Buggy == 1 { elapsed + 1 } else { elapsed };
                phase = if Buggy == 1 {
                    if elapsed + 1 == TotalTicks { 0 }
                    else { if elapsed + 1 <= HoldTicks - 1 { 1 } else { 2 } }
                } else { phase };
                forced = if Buggy == 1 {
                    if elapsed + 1 <= HoldTicks - 1 { 1 } else { 0 }
                } else { forced };
            }
            action LongPresentableGap when (
                collections == 1 && presented_once == 1 && presentable == 1 &&
                visible == 1 && wall_expired == 0 && elapsed <= TotalTicks - 1
            ) {
                wall_expired = 1;
                elapsed = TotalTicks;
                presented = presented + 1;
                presentable = 1;
                phase = if Buggy == 1 { 2 } else { 0 };
                visible = if Buggy == 1 { 1 } else { 0 };
                forced = 0;
            }
            invariant ForcedUntilPresented:
                collections <= forced +
                    (if presented > HoldTicks - 1 { 1 } else { 0 }) + wall_expired &&
                collections <= (if phase == 1 { 1 } else { 0 }) +
                    (if presented > HoldTicks - 1 { 1 } else { 0 }) + wall_expired;
            invariant VisibilityMatchesContext:
                if presentable == 0 { visible == 0 }
                else { if phase == 0 { visible == 0 } else { visible == 1 } };
            invariant HiddenAtDeadline:
                if elapsed == TotalTicks {
                    phase == 0 && visible == 0 && forced == 0
                } else { elapsed <= TotalTicks - 1 };
            invariant LongGapSettlesHidden:
                if wall_expired == 1 {
                    presented_once == 1 && presentable == 1 &&
                    elapsed == TotalTicks && phase == 0 &&
                    visible == 0 && forced == 0
                } else { wall_expired == 0 };
            invariant VisibleFrameWasPresented: visible <= presented_once;
            invariant TrailMasterOwnsOrdinary:
                if trail_master == 0 {
                    ordinary_armed == 0 && ordinary_visible == 0
                } else { ordinary_visible == ordinary_armed };
            invariant HelloIndependentOfTrailMaster:
                if trail_master == 0 && phase > 0 && presentable == 1 {
                    visible == 1
                } else { visible <= 1 };
            bound PresentedOnceBounded: presented_once <= 1;
            bound WallExpiredBounded: wall_expired <= 1;
            bound TimeBounded: elapsed <= TotalTicks;
            bound PresentedBounded: presented <= TotalTicks;
            bound HiddenBounded: hidden <= MaxHidden;
            bound PhaseBounded: phase <= 2;
            bound OrdinaryGateBounded:
                trail_master <= 1 && ordinary_armed <= 1 && ordinary_visible <= 1;
        }
    }
}

/// Cursor-cat forward/reverse wrap placement model requiring off-glass side changes.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_cat_fold_model() -> Model {
    aterm_spec::ty_model! {
        CursorCatFold {
            const Buggy = 0;
            const MaxFolds = 2;
            var phase = 0;
            var direction = 0;
            var origin_side = 1;
            var side = 1;
            var off_glass = 0;
            var off_samples = 0;
            var folds = 0;

            action StartForward when (phase == 0 && folds <= MaxFolds - 1) {
                phase = 1; direction = 1; origin_side = 1; side = 1;
                off_glass = 0; off_samples = 0; folds = folds + 1;
            }
            action StartReverse when (phase == 0 && folds <= MaxFolds - 1) {
                phase = 1; direction = 2; origin_side = 0; side = 0;
                off_glass = 0; off_samples = 0; folds = folds + 1;
            }
            action LeaveOff when (phase == 1) {
                phase = if Buggy == 1 { 4 } else { 2 };
                side = if Buggy == 1 { 1 - side } else { side };
                off_glass = if Buggy == 1 { 0 } else { 1 };
                off_samples = if Buggy == 1 { 0 } else { off_samples + 1 };
            }
            action CrossSide when (phase == 2) {
                phase = 3; side = 1 - side; off_glass = 1;
            }
            action EnterGlass when (phase == 3) {
                phase = 4; off_glass = 0;
            }
            action Finish when (phase == 4) {
                phase = 0; direction = 0; origin_side = side;
                off_glass = 0; off_samples = 0;
            }

            invariant OffGlassBeforeSideChange:
                phase == 0 || side == origin_side || off_samples > 0;
            bound StateBounded:
                phase <= 4 && direction <= 2 && origin_side <= 1 && side <= 1 &&
                off_glass <= 1 && off_samples <= 1 && folds <= MaxFolds;
        }
    }
}

/// Cursor-cat wince reaction model for complete profanity cues on visible companions.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_cat_curse_wince_model() -> Model {
    aterm_spec::ty_model! {
        CursorCatCurseWince {
            const Buggy = 0;
            const MaxChain = 4;
            var active = 1;
            var prefixes = 0;
            var completions = 0;
            var winces = 0;
            var chain = 0;
            var reaction = 0;

            action TypeFuc when (active == 1 && prefixes == 0) {
                prefixes = 1;
                winces = if Buggy == 1 { 1 } else { 0 };
                chain = if Buggy == 1 { 1 } else { 0 };
                reaction = if Buggy == 1 { 1 } else { 0 };
            }
            action Complete when (active == 1 && completions <= MaxChain - 1) {
                completions = completions + 1;
                winces = winces + 1;
                chain = if chain + 1 > MaxChain { MaxChain } else { chain + 1 };
                reaction = 1;
            }
            action Decay when (reaction == 1) {
                reaction = 0;
            }
            action Hide when (active == 1 && completions == 0 && prefixes == 0) {
                active = 0;
            }
            action HiddenComplete when (active == 0 && completions == 0) {
                completions = 1;
                winces = if Buggy == 1 { winces + 1 } else { winces };
                chain = if Buggy == 1 { chain + 1 } else { chain };
                reaction = if Buggy == 1 { 1 } else { reaction };
            }
            action Done when (completions == MaxChain || active == 0) {
                active = active;
            }

            invariant PrefixNeverWinces:
                if prefixes == 1 && completions == 0 {
                    winces == 0 && chain == 0 && reaction == 0
                } else { prefixes <= 1 };
            invariant WinceRequiresComplete:
                if winces > 0 { completions > 0 } else { winces == 0 };
            invariant DynamicChainTracksAcceptedWinces: chain == winces;
            invariant HiddenCueNeverSummons:
                if active == 0 { reaction == 0 && winces == 0 } else { active == 1 };
            invariant Bounds:
                active <= 1 && prefixes <= 1 && completions <= MaxChain &&
                winces <= MaxChain && chain <= MaxChain && reaction <= 1;
        }
    }
}

/// Held-key sing-along detector model requiring sixteen presses at full drive.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kitty_sing_detector_model() -> Model {
    aterm_spec::ty_model! {
        KittySingDetector {
            const Buggy = 0;
            const ArmRepeats = 16;
            const OldArmRepeats = 8;
            var phase = 0;
            var count = 0;
            var drive_live = 0;
            var ramp_fault = 0;

            action PickRampedArm when (ramp_fault == 0 && phase == 0 && count == 0) {
                ramp_fault = Buggy;
            }
            action Repeat when (phase == 0 && count <= ArmRepeats - 1) {
                count = count + 1;
                phase = if Buggy == 1 && ramp_fault == 0 {
                    if count + 1 > OldArmRepeats - 1 { 1 } else { 0 }
                } else {
                    if count + 1 > ArmRepeats - 1 { 1 } else { 0 }
                };
                drive_live = if ramp_fault == 1 {
                    0
                } else {
                    if Buggy == 1 {
                        if count + 1 > OldArmRepeats - 1 { 1 } else { 0 }
                    } else {
                        if count + 1 > ArmRepeats - 1 { 1 } else { 0 }
                    }
                };
            }
            action Break when (phase == 0 && count > 0) {
                count = 0;
            }
            action Release when (phase == 1) {
                phase = 2;
                count = 0;
                drive_live = 1;
            }
            action Finish when (phase == 2) {
                phase = 0;
                drive_live = 0;
            }

            invariant ArmedRequiresCurrentThreshold:
                if phase == 1 { count == ArmRepeats } else { count <= ArmRepeats - 1 };
            invariant ArmedRunIsAtFullDrive:
                if phase == 1 { drive_live == 1 } else { 1 == 1 };
            bound CountBounded: count <= ArmRepeats;
            bound PhaseBounded: phase <= 2;
        }
    }
}

/// Cursor-cat sing-along bypass floor model requiring sixteen forward cursor events.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_cat_earn_floor_model() -> Model {
    aterm_spec::ty_model! {
        CursorCatEarnFloor {
            const Buggy = 0;
            const MinRun = 16;
            const OldMinRun = 10;
            var singing = 0;
            var run = 0;
            var active = 0;

            action BeginSing when (singing == 0 && run == 0 && active == 0) {
                singing = 1;
            }
            action Qualify when (
                singing == 1 && active == 0 && run <= MinRun - 1
            ) {
                run = run + 1;
                active = if Buggy == 1 {
                    if run + 1 > OldMinRun - 1 { 1 } else { 0 }
                } else {
                    if run + 1 > MinRun - 1 { 1 } else { 0 }
                };
            }

            invariant NoCatBeforeSixteen:
                if active == 1 { run == MinRun } else { run <= MinRun - 1 };
            bound RunBounded: run <= MinRun;
            bound FlagsBounded: singing <= 1 && active <= 1;
        }
    }
}

/// Resident companion custody model across sampled song and drain phases.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn reduced_motion_companion_handoff_model() -> Model {
    aterm_spec::ty_model! {
        ReducedMotionCompanionHandoff {
            const Buggy = 0;
            var phase = 0;
            var started = 0;
            var song_tail_live = 0;
            var singer_visible = 0;
            var pet_ready = 1;
            var pet_visible = 1;

            action StartReducedSong when (phase == 0 && started == 0) {
                phase = 1;
                started = 1;
                song_tail_live = 1;
                singer_visible = if Buggy == 1 { 1 } else { 0 };
                pet_ready = 1;
                pet_visible = if Buggy == 1 { 0 } else { 1 };
            }
            action SampleAtHalfCutoff when (phase == 1) {
                phase = 2;
                singer_visible = 0;
                pet_ready = 1;
                pet_visible = if Buggy == 1 { 0 } else { 1 };
            }
            action SampleCadencedBelowHalf when (phase == 2) {
                phase = 3;
                singer_visible = 0;
                pet_ready = 1;
                pet_visible = if Buggy == 1 { 0 } else { 1 };
            }
            action SampleLateBelowHalf when (phase == 1) {
                phase = 3;
                singer_visible = 0;
                pet_ready = 1;
                pet_visible = if Buggy == 1 { 0 } else { 1 };
            }
            action SampleLateBelowFaceSwap when (phase == 1) {
                phase = 4;
                singer_visible = 0;
                pet_visible = if Buggy == 1 { 0 } else { 1 };
            }
            action SampleLateDrained when (phase == 1) {
                phase = 5;
                song_tail_live = 0;
                singer_visible = 0;
                pet_visible = if Buggy == 1 { 0 } else { 1 };
            }
            action SampleBelowFaceSwap when (phase == 3) {
                phase = 4;
                singer_visible = 0;
                pet_visible = 1;
            }
            action DrainSongTail when (phase == 4) {
                phase = 5;
                song_tail_live = 0;
                singer_visible = 0;
                pet_visible = 1;
            }

            invariant LiveTailKeepsCompanionVisible:
                if song_tail_live == 1 {
                    singer_visible + pet_visible > 0
                } else { song_tail_live == 0 };
            invariant ResidentAlwaysOwnsPetMode:
                singer_visible == 0 && pet_ready == 1 && pet_visible == 1;
            invariant GlassCustodyIsExclusive:
                singer_visible + pet_visible == 1;
            invariant ResidentOwnsBelowSwap:
                if phase > 3 {
                    singer_visible == 0 && pet_ready == 1 && pet_visible == 1
                } else { phase <= 3 };
            bound StateBounded:
                phase <= 5 && started <= 1 && song_tail_live <= 1
                    && singer_visible <= 1 && pet_ready <= 1 && pet_visible <= 1;
        }
    }
}

/// One-shot routing model for the classifier-minted cursor-cat motion pulse.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_cat_motion_pulse_routing_model() -> Model {
    aterm_spec::ty_model! {
        CursorCatMotionPulseRouting {
            const Buggy = 0;
            var licensed = 0;
            var classified = 0;
            var pending = 0;
            var route = 0;
            var attempted_route = 0;
            var consumes = 0;
            var deliveries = 0;
            var switched = 0;
            var stranded = 0;
            var stale_replay = 0;
            var cold_pulse = 0;

            action LicenseMove when (licensed == 0) {
                licensed = 1;
            }
            action ClassifyPulse when (
                (licensed == 1 || Buggy == 1) && classified == 0
            ) {
                classified = 1;
                pending = 1;
                cold_pulse = if licensed == 0 { 1 } else { cold_pulse };
            }
            action SelectOrdinaryRoute when (classified == 1 && route == 0) {
                route = 1;
            }
            action SelectComposedExtractedRoute when (
                classified == 1 && route == 0
            ) {
                route = 2;
            }
            action RenderOrdinaryRoute when (pending == 1 && route == 1) {
                pending = 0;
                attempted_route = if attempted_route == 0 { 1 } else { attempted_route };
                consumes = consumes + 1;
                deliveries = deliveries + 1;
                stale_replay = if attempted_route > 0 { 1 } else { stale_replay };
            }
            action RenderComposedExtractedRoute when (
                pending == 1 && route == 2 && attempted_route == 0
            ) {
                attempted_route = 2;
                pending = if Buggy == 1 { 1 } else { 0 };
                consumes = if Buggy == 1 { consumes } else { consumes + 1 };
                deliveries = if Buggy == 1 { deliveries } else { deliveries + 1 };
                stranded = if Buggy == 1 { 1 } else { 0 };
            }
            action SwitchToOrdinaryRoute when (
                route == 2 && attempted_route == 2 && switched == 0
            ) {
                route = 1;
                switched = 1;
            }
            action SwitchToComposedExtractedRoute when (
                route == 1 && attempted_route == 1 && switched == 0
            ) {
                route = 2;
                switched = 1;
            }

            invariant AttemptConsumesAndDeliversExactlyOnce:
                if attempted_route > 0 {
                    pending == 0 && consumes == 1 && deliveries == 1
                } else {
                    pending == classified && consumes == 0 && deliveries == 0
                };
            invariant NoComposedRouteStrand: stranded == 0;
            invariant RouteSwitchCannotReplay: stale_replay == 0;
            invariant NoPulseFromAnUnlicensedMove: cold_pulse == 0;
            bound StateBounded:
                licensed <= 1 && classified <= 1 && pending <= 1 && route <= 2
                    && attempted_route <= 2 && consumes <= 1 && deliveries <= 1
                    && switched <= 1 && stranded <= 1 && stale_replay <= 1
                    && cold_pulse <= 1;
        }
    }
}

/// Key-hint and delivered-insert license model for `CursorGlow::spawn` and `CursorTrail::spawn`:
///
/// ```text
/// licensed = fresh(type_hint) || fresh(quench_hint) || fresh(nav_hint)
///         || fresh(return_hint) || fresh(newline_hint)
///         || fresh(user_gesture_hint) || synthetic_note_pending
/// ```
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_hint_license_model() -> Model {
    aterm_spec::ty_model! {
        CursorHintLicense {
            const Buggy = 0;
            const ArmCap = 2;
            const CoalesceCells = 2;
            const MoveCap = 2;

            var hint = 0;
            var morphology = 0;
            var swallowed = 0;
            var arms = 0;
            var consumed = 0;
            var expired = 0;
            var cleared = 0;
            var superseded = 0;
            var admissions = 0;
            var credit_arms = 0;
            var spent = 0;
            var forfeited = 0;
            var just_forgot = 0;
            var phantom_admitted = 0;
            var coalesce_births = 0;
            var births = 0;
            var resident = 0;
            var spawns = 0;
            var licensed_tally = 0;
            var declined_tally = 0;
            var cold_admitted = 0;
            var stale_admitted = 0;
            var morphology_admitted = 0;
            var swallow_admitted = 0;
            var wiped = 0;
            var credit_refunded = 0;
            var write_pending = 0;
            var insert_hint = 0;
            var insert_undelivered = 0;
            var undelivered_admitted = 0;
            var later_key_credit = 0;
            var later_key_stolen = 0;

            action PressArmsLicense when (arms <= ArmCap - 1) {
                superseded = if hint == 1 { superseded + 1 } else { superseded };
                hint = 1;
                arms = arms + 1;
                credit_arms = credit_arms + 1;
                swallowed = 0;
                just_forgot = 0;
            }
            action ByteSilentPointerPreservesLicense when (hint == 1) {
                hint = if Buggy == 1 { 0 } else { hint };
            }
            action PasteEnqueues when (write_pending == 0 && arms <= ArmCap - 1) {
                write_pending = 1;
                superseded = if Buggy == 1 && insert_hint == 1 {
                    superseded + 1
                } else {
                    superseded
                };
                insert_hint = if Buggy == 1 { 1 } else { insert_hint };
                insert_undelivered = if Buggy == 1 { 1 } else { insert_undelivered };
                arms = if Buggy == 1 { arms + 1 } else { arms };
            }
            action WriteCompletesArmsInsertLicence when (
                write_pending == 1 && arms <= ArmCap - 1
            ) {
                write_pending = 0;
                superseded = if insert_hint == 1 { superseded + 1 } else { superseded };
                insert_hint = 1;
                insert_undelivered = 0;
                arms = arms + 1;
            }
            action PressBehindQueuedInsert when (
                write_pending == 1 && later_key_credit == 0 && arms <= ArmCap - 1
            ) {
                superseded = if hint == 1 { superseded + 1 } else { superseded };
                hint = 1;
                arms = arms + 1;
                credit_arms = credit_arms + 1;
                later_key_credit = 1;
                swallowed = 0;
                just_forgot = 0;
            }
            action DelayedInsertEchoPrecedesLaterKey when (
                insert_hint == 1 && later_key_credit == 1
                    && hint == 1 && spawns <= MoveCap - 1
            ) {
                insert_hint = if Buggy == 1 { 1 } else { 0 };
                hint = if Buggy == 1 { 0 } else { 1 };
                later_key_credit = if Buggy == 1 { 0 } else { 1 };
                later_key_stolen = if Buggy == 1 { 1 } else { later_key_stolen };
                spent = if Buggy == 1 { spent + 1 } else { spent };
                consumed = consumed + 1;
                admissions = admissions + 1;
                spawns = spawns + 1;
                births = births + 1;
                resident = 1;
                licensed_tally = licensed_tally + 1;
            }
            action InsertLicenceExpires when (insert_hint == 1) {
                insert_hint = 2;
                expired = expired + 1;
            }
            action RetireStaleInsertLicence when (insert_hint == 2) {
                insert_hint = 0;
            }
            action LicensedInsertEchoMintsLight when (
                insert_hint == 1 && spawns <= MoveCap - 1
            ) {
                spawns = spawns + 1;
                insert_hint = if Buggy == 1 { 1 } else { 0 };
                consumed = consumed + 1;
                admissions = admissions + 1;
                births = births + 1;
                resident = 1;
                licensed_tally = licensed_tally + 1;
                undelivered_admitted = if insert_undelivered == 1 {
                    1
                } else {
                    undelivered_admitted
                };
            }
            action StaleInsertStampDeclines when (
                insert_hint == 2 && hint == 0 && spawns <= MoveCap - 1
            ) {
                spawns = spawns + 1;
                stale_admitted = if Buggy == 1 { 1 } else { stale_admitted };
                births = if Buggy == 1 { births + 1 } else { births };
                resident = if Buggy == 1 { 1 } else { resident };
                licensed_tally = if Buggy == 1 {
                    licensed_tally + 1
                } else {
                    licensed_tally
                };
                declined_tally = if Buggy == 1 {
                    declined_tally
                } else {
                    declined_tally + 1
                };
            }
            action MorphologyStampArrives when (morphology == 0) {
                morphology = 1;
            }
            action LicenseExpires when (hint == 1) {
                hint = 2;
                expired = expired + 1;
            }
            action RetireStaleLicense when (hint == 2) {
                hint = 0;
                spent = if Buggy == 1 && spent > 0 { spent - 1 } else { spent };
                credit_refunded = if Buggy == 1 && spent > 0 {
                    1
                } else {
                    credit_refunded
                };
            }
            action SwallowedKeyClearsLicense when (hint == 1 && swallowed == 0) {
                hint = if Buggy == 1 { 1 } else { 0 };
                cleared = if Buggy == 1 { cleared } else { cleared + 1 };
                swallowed = 1;
            }
            action LicensedTypedMoveMintsLight when (
                hint == 1 && spawns <= MoveCap - 1
            ) {
                spawns = spawns + 1;
                hint = if Buggy == 1 { 1 } else { 0 };
                consumed = consumed + 1;
                admissions = admissions + 1;
                spent = if credit_arms - spent - forfeited > 0 { spent + 1 } else { spent };
                births = births + 1;
                resident = 1;
                licensed_tally = licensed_tally + 1;
                swallow_admitted = if swallowed == 1 { 1 } else { swallow_admitted };
            }
            action AdmitCoalesceSpendsCredits when (
                hint == 1 && coalesce_births == 0 && spawns <= MoveCap - 1
                    && credit_arms - spent - forfeited > CoalesceCells - 1
            ) {
                spawns = spawns + 1;
                hint = 0;
                consumed = consumed + 1;
                admissions = admissions + 1;
                spent = if Buggy == 1 { spent } else { spent + CoalesceCells };
                coalesce_births = coalesce_births + 1;
                births = births + 1;
                resident = 1;
                licensed_tally = licensed_tally + 1;
                swallow_admitted = if swallowed == 1 { 1 } else { swallow_admitted };
            }
            action StarvedCoalesceDeclines when (
                hint == 1 && spawns <= MoveCap - 1
                    && credit_arms - spent - forfeited <= CoalesceCells - 1
            ) {
                spawns = spawns + 1;
                hint = 0;
                consumed = consumed + 1;
                admissions = admissions + 1;
                spent = if Buggy == 1 { spent + CoalesceCells } else { spent };
                forfeited = if Buggy == 1 { forfeited } else { credit_arms - spent };
                just_forgot = 1;
                licensed_tally = if Buggy == 1 {
                    licensed_tally + 1
                } else {
                    licensed_tally
                };
                declined_tally = if Buggy == 1 {
                    declined_tally
                } else {
                    declined_tally + 1
                };
            }
            action InFlightBatchEchoMintsLight when (
                (hint == 0 || hint == 2) && coalesce_births == 0 && spawns <= MoveCap - 1
                    && credit_arms - spent - forfeited > CoalesceCells - 1
            ) {
                spawns = spawns + 1;
                spent = spent + CoalesceCells;
                coalesce_births = coalesce_births + 1;
                admissions = admissions + 1;
                births = births + 1;
                resident = 1;
                licensed_tally = licensed_tally + 1;
                phantom_admitted = if just_forgot == 1 { 1 } else { phantom_admitted };
            }
            action InFlightPressEchoMintsLight when (
                (hint == 0 || hint == 2) && spawns <= MoveCap - 1
                    && credit_arms - spent - forfeited == 1
            ) {
                spawns = spawns + 1;
                spent = if Buggy == 1 { spent } else { spent + 1 };
                admissions = admissions + 1;
                births = births + 1;
                resident = 1;
                licensed_tally = licensed_tally + 1;
                phantom_admitted = if just_forgot == 1 { 1 } else { phantom_admitted };
            }
            action UnexplainedHopForgetsCredits when (
                (hint == 0 || hint == 2) && spawns <= MoveCap - 1
                    && credit_arms - spent - forfeited > 0
            ) {
                spawns = spawns + 1;
                declined_tally = declined_tally + 1;
                forfeited = if Buggy == 1 { forfeited } else { credit_arms - spent };
                just_forgot = 1;
            }
            action PatienceElapses when (credit_arms - spent - forfeited > 0) {
                forfeited = credit_arms - spent;
            }
            action ColdMoveDeclines when (
                hint == 0 && spawns <= MoveCap - 1
                    && credit_arms - spent - forfeited <= CoalesceCells - 1
            ) {
                spawns = spawns + 1;
                cold_admitted = if Buggy == 1 { 1 } else { cold_admitted };
                births = if Buggy == 1 { births + 1 } else { births };
                resident = if Buggy == 1 { 1 } else { resident };
                licensed_tally = if Buggy == 1 {
                    licensed_tally + 1
                } else {
                    licensed_tally
                };
                declined_tally = if Buggy == 1 {
                    declined_tally
                } else {
                    declined_tally + 1
                };
            }
            action ColdMoveOverEarnedLight when (
                hint == 0 && resident == 1 && spawns <= MoveCap - 1
                    && credit_arms - spent - forfeited <= CoalesceCells - 1
            ) {
                spawns = spawns + 1;
                resident = if Buggy == 1 { 0 } else { 1 };
                wiped = if Buggy == 1 { 1 } else { wiped };
                declined_tally = if Buggy == 1 {
                    declined_tally
                } else {
                    declined_tally + 1
                };
            }
            action MorphologyOnlyMoveDeclines when (
                hint == 0 && morphology == 1 && spawns <= MoveCap - 1
            ) {
                spawns = spawns + 1;
                morphology = 0;
                morphology_admitted = if Buggy == 1 {
                    1
                } else {
                    morphology_admitted
                };
                births = if Buggy == 1 { births + 1 } else { births };
                resident = if Buggy == 1 { 1 } else { resident };
                licensed_tally = if Buggy == 1 {
                    licensed_tally + 1
                } else {
                    licensed_tally
                };
                declined_tally = if Buggy == 1 {
                    declined_tally
                } else {
                    declined_tally + 1
                };
            }
            action StaleStampMoveDeclines when (
                hint == 2 && spawns <= MoveCap - 1
                    && credit_arms - spent - forfeited <= CoalesceCells - 1
            ) {
                spawns = spawns + 1;
                stale_admitted = if Buggy == 1 { 1 } else { stale_admitted };
                births = if Buggy == 1 { births + 1 } else { births };
                resident = if Buggy == 1 { 1 } else { resident };
                licensed_tally = if Buggy == 1 {
                    licensed_tally + 1
                } else {
                    licensed_tally
                };
                declined_tally = if Buggy == 1 {
                    declined_tally
                } else {
                    declined_tally + 1
                };
            }

            invariant NoLightWithoutAFreshLicence: cold_admitted == 0;
            invariant AStaleStampIsNotALicence: stale_admitted == 0;
            invariant MorphologyStampsNeverLicence: morphology_admitted == 0;
            invariant ASwallowedKeyNeverLicences: swallow_admitted == 0;
            invariant DeclinedMovesNeverDestroyEarnedLight: wiped == 0;
            invariant SpentCreditsNeverComeBack: credit_refunded == 0;
            invariant AForgottenPressNeverLicences: phantom_admitted == 0;
            invariant ForgottenCreditsNeverReturn:
                just_forgot == 0 || credit_arms - spent - forfeited == 0;
            invariant AnInsertLicenceIsMintedOnlyByACompletedWrite:
                undelivered_admitted == 0;
            invariant LaterKeysCannotOwnAnEarlierInsertEcho: later_key_stolen == 0;
            invariant EveryArmReachesExactlyOneDisposition:
                arms == consumed + expired + cleared + superseded
                    + (if hint == 1 { 1 } else { 0 })
                    + (if insert_hint == 1 { 1 } else { 0 });
            invariant PairedAdmissionsNeverExceedArms: admissions <= arms;
            invariant SpentCreditsNeverExceedArmed: spent <= credit_arms;
            invariant ACoalesceRibbonIsPaidFor:
                if coalesce_births == 1 {
                    spent > CoalesceCells - 1
                } else {
                    coalesce_births == 0
                };
            invariant TheRingCannotClaimUnmintedLight: licensed_tally == births;
            invariant TheRingScoresEverySpawnOnce:
                licensed_tally + declined_tally == spawns;
            bound StateBounded:
                hint <= 2 && morphology <= 1 && swallowed <= 1 && arms <= ArmCap
                    && write_pending <= 1 && insert_hint <= 2 && insert_undelivered <= 1
                    && undelivered_admitted <= 1
                    && consumed <= ArmCap && expired <= ArmCap
                    && cleared <= ArmCap && superseded <= ArmCap
                    && admissions <= MoveCap && credit_arms <= ArmCap
                    && spent <= ArmCap + CoalesceCells && coalesce_births <= 1
                    && forfeited <= ArmCap && just_forgot <= 1 && phantom_admitted <= 1
                    && births <= MoveCap && resident <= 1 && spawns <= MoveCap
                    && licensed_tally <= MoveCap && declined_tally <= MoveCap
                    && cold_admitted <= 1 && stale_admitted <= 1
                    && morphology_admitted <= 1 && swallow_admitted <= 1
                    && wiped <= 1 && credit_refunded <= 1;
        }
    }
}

/// Rainbow-kitty `EchoLedger` bridge model partitioning banked presses around the licensing key.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn echo_ledger_bridge_model() -> Model {
    aterm_spec::ty_model! {
        EchoLedgerBridge {
            const Buggy = 0;
            const Depth = 3;
            const BankCap = 4;
            const Batch = 2;

            var older = 0;
            var younger = 0;
            var stale = 0;
            var hole = 0;
            var banked = 0;
            var spent = 0;
            var forfeited = 0;
            var expired = 0;
            var stale_gone = 0;
            var bridged = 0;
            var laid_hole = 0;
            var last_older = 0;
            var bridged_delta = 0;
            var just_refused = 0;

            action KeyPressed when (
                banked <= BankCap - 1 && older + younger + stale <= Depth - 1
            ) {
                older = older + younger;
                younger = 1;
                banked = banked + 1;
                just_refused = 0;
                bridged = if Buggy == 1 { bridged + 1 } else { bridged };
            }
            action PressInFlight when (
                banked <= BankCap - 1 && older + younger + stale <= Depth - 1
            ) {
                younger = younger + 1;
                banked = banked + 1;
                just_refused = 0;
            }
            action UnlicensedAdvance when (hole <= Depth - 1) {
                hole = hole + 1;
            }
            action TimePasses when (1 <= older + younger) {
                stale = stale + older + younger;
                stale_gone = stale_gone + older + younger;
                older = 0;
                younger = 0;
            }
            action OnePressGoesStale when (1 <= older + younger) {
                older = if 1 <= older { older - 1 } else { older };
                younger = if older <= 0 { younger - 1 } else { younger };
                stale = stale + 1;
                stale_gone = stale_gone + 1;
            }
            action SweptMovePays when (
                (Buggy == 0 && older == hole && 1 <= younger)
                    || (Buggy == 1 && hole + 1 <= older + younger + stale)
            ) {
                expired = if Buggy == 1 { expired } else { expired + stale };
                stale = if Buggy == 1 && hole + 1 <= stale - 1 {
                    stale - hole - 1
                } else {
                    0
                };
                older = if Buggy == 0 {
                    0
                } else if hole + 1 <= stale {
                    older
                } else if hole + 1 - stale <= older {
                    older - (hole + 1 - stale)
                } else {
                    0
                };
                younger = if Buggy == 1 {
                    if hole + 1 <= stale + older {
                        younger
                    } else {
                        younger - (hole + 1 - stale - older)
                    }
                } else {
                    younger - 1
                };
                spent = spent + hole + 1;
                bridged = bridged + hole;
                bridged_delta = hole;
                laid_hole = hole;
                last_older = older;
                hole = 0;
            }
            action SweptMoveRefuses when (
                older <= hole - 1 || hole <= older - 1 || younger <= 0
            ) {
                expired = expired + stale;
                stale = 0;
                forfeited = if Buggy == 1 { forfeited } else { forfeited + older + younger };
                older = if Buggy == 1 { older } else { 0 };
                younger = if Buggy == 1 { younger } else { 0 };
                bridged = if Buggy == 1 && just_refused == 0 && 1 <= older + younger {
                    bridged + 1
                } else {
                    bridged
                };
                bridged_delta = if Buggy == 1 && just_refused == 0 && 1 <= older + younger {
                    1
                } else {
                    0
                };
                laid_hole = 0;
                last_older = older;
                hole = 0;
                just_refused = 1;
            }
            action UnsweptMovePays when (
                hole == 0
                    && ((Buggy == 0 && Batch <= older + younger)
                        || (Buggy == 1 && 1 <= older + younger))
            ) {
                expired = expired + stale;
                stale = 0;
                older = if Buggy == 1 {
                    0
                } else if Batch <= older {
                    older - Batch
                } else {
                    0
                };
                younger = if Buggy == 1 {
                    0
                } else if Batch <= older {
                    younger
                } else {
                    younger - (Batch - older)
                };
                spent = spent + Batch;
                bridged = bridged + Batch;
                bridged_delta = Batch;
                laid_hole = 0;
                last_older = older;
            }
            action UnsweptMoveRefuses when (1 <= hole || older + younger <= Batch - 1) {
                expired = expired + stale;
                stale = 0;
                forfeited = forfeited + older + younger;
                older = 0;
                younger = 0;
                bridged_delta = 0;
                laid_hole = 0;
                last_older = older;
                hole = 0;
                just_refused = 1;
            }
            action LedgerForgotten when (1 <= older + younger + stale + hole) {
                expired = expired + stale;
                stale = 0;
                forfeited = forfeited + older + younger;
                older = 0;
                younger = 0;
                hole = 0;
                bridged_delta = 0;
                laid_hole = 0;
                last_older = older;
            }
            action LedgerClearedInPlace when (1 <= older + younger + stale) {
                expired = expired + stale;
                stale = 0;
                forfeited = forfeited + older + younger;
                older = 0;
                younger = 0;
                bridged_delta = 0;
                laid_hole = 0;
            }
            action RetreatKeepsPresses when (1 <= hole || 1 <= stale) {
                expired = expired + stale;
                stale = 0;
                hole = 0;
                bridged_delta = 0;
                laid_hole = 0;
                last_older = older;
            }

            invariant BridgedNeverExceedsOlderPresses: laid_hole <= last_older;
            invariant AProgramGapIsNeverBridged:
                laid_hole == 0 || laid_hole == last_older;
            invariant ForfeitedCreditsNeverReturn:
                just_refused == 0 || older + younger == 0;
            invariant NoBridgeOutsideALicensedMove: bridged <= spent;
            invariant NoBridgeOnARefusedMove: just_refused == 0 || bridged_delta == 0;
            invariant OnePressOneCell:
                older + younger + stale + spent + forfeited + expired == banked;
            invariant ExpiredPressesBuyNothing: stale_gone == expired + stale;
            bound StateBounded:
                older <= Depth && younger <= Depth && stale <= Depth
                    && hole <= Depth && banked <= BankCap
                    && spent <= BankCap + BankCap && forfeited <= BankCap
                    && expired <= BankCap && stale_gone <= BankCap
                    && bridged <= BankCap + BankCap + BankCap + BankCap
                    && laid_hole <= Depth && last_older <= Depth
                    && bridged_delta <= Depth && just_refused <= 1;
        }
    }
}

/// Cursor viewport lifecycle model suppressing presentation in retained history while ticking the pet brain.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_viewport_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        CursorViewportLifecycle {
            const Buggy = 0;
            var live_viewport = 1;
            var glow_visible = 0;
            var trail_visible = 0;
            var cursor_body_visible = 0;
            var pet_visible = 0;
            var base_cursor_visible = 0;
            var pet_brain_pending = 0;
            var pet_brain_ticked = 0;
            var scheduler_stuck = 0;
            var retained_checked = 0;

            action ChargeLive when (live_viewport == 1 && glow_visible == 0) {
                glow_visible = 1;
                trail_visible = 1;
                cursor_body_visible = 1;
                pet_visible = 1;
                base_cursor_visible = 1;
                pet_brain_pending = 1;
            }
            action EnterHistory when (live_viewport == 1) {
                live_viewport = 0;
                glow_visible = if Buggy == 1 { glow_visible } else { 0 };
                trail_visible = if Buggy == 1 { trail_visible } else { 0 };
                cursor_body_visible = if Buggy == 1 { cursor_body_visible } else { 0 };
                pet_visible = if Buggy == 1 { pet_visible } else { 0 };
                base_cursor_visible = if Buggy == 1 { base_cursor_visible } else { 0 };
                pet_brain_ticked = if Buggy == 1 {
                    0
                } else {
                    if pet_brain_pending == 1 { 1 } else { 0 }
                };
                scheduler_stuck = if Buggy == 1 { pet_brain_pending } else { 0 };
            }
            action RetainHistory when (
                live_viewport == 0 && retained_checked == 0
            ) {
                retained_checked = 1;
                glow_visible = if Buggy == 1 { 1 } else { glow_visible };
                trail_visible = if Buggy == 1 { 1 } else { trail_visible };
                cursor_body_visible = if Buggy == 1 { 1 } else { cursor_body_visible };
                pet_visible = if Buggy == 1 { 1 } else { pet_visible };
                base_cursor_visible = if Buggy == 1 { 1 } else { base_cursor_visible };
            }
            action SettleHistoryBrain when (
                live_viewport == 0 && pet_brain_pending == 1
                    && pet_brain_ticked == 1
            ) {
                pet_brain_pending = 0;
                scheduler_stuck = 0;
            }
            action LeaveHistory when (
                live_viewport == 0 && pet_brain_pending == 0
            ) {
                live_viewport = 1;
                glow_visible = 0;
                trail_visible = 0;
                cursor_body_visible = 0;
                pet_visible = 0;
                base_cursor_visible = 0;
                pet_brain_ticked = 0;
                retained_checked = 0;
            }

            invariant HistorySuppressesCursorOwnedPixels:
                if live_viewport == 0 {
                    glow_visible == 0 && trail_visible == 0
                        && cursor_body_visible == 0 && pet_visible == 0
                        && base_cursor_visible == 0
                } else {
                    live_viewport == 1
                };
            invariant HiddenPetLifecycleProgresses:
                if live_viewport == 0 && pet_brain_pending == 1 {
                    pet_brain_ticked == 1
                } else {
                    pet_brain_ticked <= 1
                };
            invariant HiddenSchedulerNeverSticks: scheduler_stuck == 0;
            bound CursorViewportValuesBounded:
                live_viewport <= 1 && glow_visible <= 1 && trail_visible <= 1
                    && cursor_body_visible <= 1 && pet_visible <= 1
                    && base_cursor_visible <= 1 && pet_brain_pending <= 1
                    && pet_brain_ticked <= 1 && retained_checked <= 1;
        }
    }
}

/// Resident cursor-companion coordinate retirement and durable-identity retention model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_companion_owner_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        CursorCompanionOwnerLifecycle {
            const Buggy = 0;
            var phase = 0;
            var durable_identity = 1;
            var pet_visible = 0;
            var hit_target = 0;

            action Materialize when (phase == 0) {
                phase = 1;
                pet_visible = 1;
                hit_target = 1;
            }
            action PaneOwnerSwitch when (phase == 1) {
                phase = 2;
                pet_visible = if Buggy == 1 { pet_visible } else { 0 };
                hit_target = if Buggy == 1 { hit_target } else { 0 };
            }
            action TabOwnerSwitch when (phase == 1) {
                phase = 3;
                pet_visible = if Buggy == 1 { pet_visible } else { 0 };
                hit_target = if Buggy == 1 { hit_target } else { 0 };
            }
            action UnpresentableFocusLoss when (phase == 1) {
                phase = 4;
                pet_visible = if Buggy == 1 { pet_visible } else { 0 };
                hit_target = if Buggy == 1 { hit_target } else { 0 };
            }
            action TypedWakeFocusLoss when (phase == 1) {
                phase = 5;
                pet_visible = if Buggy == 1 { 0 } else { pet_visible };
                hit_target = if Buggy == 1 { 0 } else { hit_target };
                durable_identity = if Buggy == 1 { 0 } else { durable_identity };
            }
            action RecordingFocusLoss when (phase == 1) {
                phase = 6;
                pet_visible = if Buggy == 1 { 0 } else { pet_visible };
                hit_target = if Buggy == 1 { 0 } else { hit_target };
                durable_identity = if Buggy == 1 { 0 } else { durable_identity };
            }
            action ScreenBufferSwitch when (phase == 1) {
                phase = 7;
                pet_visible = if Buggy == 1 { pet_visible } else { 0 };
                hit_target = if Buggy == 1 { hit_target } else { 0 };
            }

            invariant RetiringBoundariesAreDark:
                if phase == 2 || phase == 3 || phase == 4 || phase == 7 {
                    pet_visible == 0 && hit_target == 0
                } else {
                    phase <= 7
                };
            invariant PresentationPinsPreserveTheSighting:
                if phase == 5 || phase == 6 {
                    pet_visible == 1 && hit_target == 1
                } else {
                    phase <= 7
                };
            invariant DurableIdentitySurvives: durable_identity == 1;
            bound CompanionOwnerValuesBounded:
                phase <= 7 && pet_visible <= 1 && hit_target <= 1
                    && durable_identity <= 1;
        }
    }
}

/// Composed-frame DEC-2026 hold model requiring every visible pane to release before presenting.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn composed_sync_hold_model() -> Model {
    aterm_spec::ty_model! {
        ComposedSyncHold {
            const Buggy = 0;
            var phase = 0;
            var a_hold = 0;
            var b_hold = 0;
            var presented = 0;

            action ArmBoth when (phase == 0) {
                phase = 1;
                a_hold = 1;
                b_hold = 1;
            }
            action CloseOnlyA when (phase == 1) {
                phase = 2;
                a_hold = 0;
                presented = if Buggy == 1 { 1 } else { 0 };
            }
            action CloseOnlyB when (phase == 1) {
                phase = 4;
                b_hold = 0;
                presented = if Buggy == 1 { 1 } else { 0 };
            }
            action CloseRemainingB when (phase == 2) {
                phase = 3;
                b_hold = 0;
                presented = 1;
            }
            action CloseRemainingA when (phase == 4) {
                phase = 3;
                a_hold = 0;
                presented = 1;
            }
            action CloseBoth when (phase == 1) {
                phase = 5;
                a_hold = 0;
                b_hold = 0;
                presented = 1;
            }

            invariant NoPartialCompositePresent:
                if a_hold == 1 || b_hold == 1 {
                    presented == 0
                } else {
                    presented <= 1
                };
            bound ComposedSyncValuesBounded:
                phase <= 5 && a_hold <= 1 && b_hold <= 1 && presented <= 1;
        }
    }
}

/// DEC-2026 reopen visibility model holding dirty reopened episodes while admitting clean boundaries.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn sync_reopen_visibility_model() -> Model {
    aterm_spec::ty_model! {
        SyncReopenVisibility {
            const Buggy = 0;
            var phase = 0;
            var sync_active = 1;
            var open_dirty = 1;
            var hold = 1;
            var completed_generation = 0;
            var presented_generation = 0;
            var partial_visible = 0;
            var level_fault = 0;

            action PickLevelHold when (phase == 0 && level_fault == 0) {
                level_fault = Buggy;
            }
            action CloseFirstEpisode when (phase == 0) {
                phase = 1;
                sync_active = 0;
                open_dirty = 0;
                hold = 0;
                completed_generation = 1;
            }
            action ReopenClean when (phase == 1) {
                phase = 2;
                sync_active = 1;
                hold = if level_fault == 1 { 1 } else { hold };
                presented_generation = if level_fault == 1 { presented_generation } else { 1 };
            }
            action DirtyReopenedEpisode when (phase == 2) {
                phase = 3;
                open_dirty = 1;
                hold = if Buggy == 1 && level_fault == 0 { 0 } else { 1 };
                partial_visible = if Buggy == 1 && level_fault == 0 { 1 } else { 0 };
            }
            action CloseReopenedEpisode when (phase == 3) {
                phase = 4;
                sync_active = 0;
                open_dirty = 0;
                hold = 0;
                completed_generation = 2;
                presented_generation = 2;
                partial_visible = 0;
            }

            invariant CleanReopenMayPresentCompletedBoundary:
                if phase == 2 {
                    sync_active == 1 && open_dirty == 0 && hold == 0
                        && completed_generation == 1
                        && presented_generation == 1 && partial_visible == 0
                } else {
                    phase <= 4
                };
            invariant DirtyReopenHoldsUntilClose:
                if phase == 3 {
                    sync_active == 1 && open_dirty == 1 && hold == 1
                        && completed_generation == 1
                        && presented_generation == 1 && partial_visible == 0
                } else {
                    phase <= 4
                };
            bound SyncReopenValuesBounded:
                phase <= 4 && sync_active <= 1 && open_dirty <= 1 && hold <= 1
                    && completed_generation <= 2 && presented_generation <= 2
                    && partial_visible <= 1;
        }
    }
}

/// PTY scroll translation and row-bound proof invalidation model for cursor effects.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_effect_scroll_model() -> Model {
    aterm_spec::ty_model! {
        CursorEffectScroll {
            const StartY = 3;
            const Delta = 2;
            const Buggy = 0;
            var survivor_y = 3;
            var off_top_alive = 1;
            var stale_proof_alive = 1;
            var scrolled = 0;

            action Scroll when (scrolled == 0) {
                survivor_y = if Buggy == 1 { StartY } else { StartY - Delta };
                off_top_alive = if Buggy == 1 { 1 } else { 0 };
                stale_proof_alive = if Buggy == 1 { 1 } else { 0 };
                scrolled = 1;
            }

            invariant SurvivorFollowsScroll:
                if scrolled == 0 {
                    survivor_y == StartY
                } else {
                    survivor_y == StartY - Delta
                };
            invariant OffTopIsRetired:
                if scrolled == 0 { off_top_alive == 1 } else { off_top_alive == 0 };
            invariant OldProofIsRetired:
                if scrolled == 0 { stale_proof_alive == 1 } else { stale_proof_alive == 0 };
        }
    }
}

/// Host scroll-observation model across uniform shifts, region band moves, and invalidating resets.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_scroll_signal_model() -> Model {
    aterm_spec::ty_model! {
        CursorScrollSignal {
            const StartY = 3;
            const Delta = 2;
            const Buggy = 0;
            var event = 0;
            var retained_history = 0;
            var uniform_rows = 0;
            var epoch = 0;
            var band_batches = 0;
            var decision = 0;
            var survivor_y = 3;
            var geometry_alive = 1;
            var proof_alive = 1;

            action UniformAtCap when (event == 0) {
                event = 1;
                uniform_rows = Delta;
                decision = if Buggy == 1 { 0 } else { 1 };
                survivor_y = if Buggy == 1 { StartY } else { StartY - Delta };
                proof_alive = if Buggy == 1 { 1 } else { 0 };
            }
            action RegionInvalidation when (event == 0) {
                event = 2;
                epoch = 1;
                decision = if Buggy == 1 { 0 } else { 2 };
                geometry_alive = if Buggy == 1 { 1 } else { 0 };
                proof_alive = if Buggy == 1 { 1 } else { 0 };
            }
            action AltUniform when (event == 0) {
                event = 3;
                uniform_rows = Delta;
                decision = if Buggy == 1 { 2 } else { 1 };
                survivor_y = if Buggy == 1 { StartY } else { StartY - Delta };
                geometry_alive = if Buggy == 1 { 0 } else { 1 };
                proof_alive = 0;
            }
            action ResetInvalidation when (event == 0) {
                event = 4;
                epoch = 1;
                decision = if Buggy == 1 { 0 } else { 2 };
                geometry_alive = if Buggy == 1 { 1 } else { 0 };
                proof_alive = if Buggy == 1 { 1 } else { 0 };
            }
            action RestoreInvalidation when (event == 0) {
                event = 5;
                epoch = 1;
                decision = if Buggy == 1 { 0 } else { 2 };
                geometry_alive = if Buggy == 1 { 1 } else { 0 };
                proof_alive = if Buggy == 1 { 1 } else { 0 };
            }
            action RegionBandMove when (event == 0) {
                event = 6;
                epoch = 1;
                band_batches = 1;
                decision = if Buggy == 1 { 2 } else { 3 };
                survivor_y = if Buggy == 1 { StartY } else { StartY + Delta };
                geometry_alive = if Buggy == 1 { 0 } else { 1 };
                proof_alive = 0;
            }

            invariant RetainedCountIsNotAuthority:
                if event == 1 || event == 3 {
                    retained_history == 0 && uniform_rows == Delta && decision == 1
                } else {
                    retained_history == 0
                };
            invariant UniformTranslationExact:
                if event == 1 || event == 3 {
                    survivor_y == StartY - Delta && geometry_alive == 1
                        && proof_alive == 0
                } else if event == 6 {
                    survivor_y == StartY + Delta && geometry_alive == 1
                        && proof_alive == 0
                } else {
                    survivor_y == StartY
                };
            invariant AmbiguousMotionInvalidates:
                if event == 2 || event == 4 || event == 5 {
                    epoch == 1 && decision == 2
                        && geometry_alive == 0 && proof_alive == 0
                } else if event == 6 {
                    epoch == 1
                } else {
                    epoch == 0
                };
            invariant ExplainedMotionTranslatesWithinItsBand:
                if event == 6 {
                    epoch == band_batches && decision == 3
                } else {
                    band_batches == 0
                };
        }
    }
}

/// Bounded FIFO landing-pool model (`LANDING_POOL = 3`) evicting the oldest landing on arrival.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rainbow_landing_pool_model() -> Model {
    aterm_spec::ty_model! {
        RainbowLandingPool {
            const Cap = 3;
            const MaxIssued = 5;
            const Buggy = 0;
            var resident = 0;
            var newest = 0;
            var oldest = 0;
            var issued = 0;

            action Land when (issued <= MaxIssued - 1) {
                resident = if resident + 1 > Cap { Cap } else { resident + 1 };
                newest = if Buggy == 1 && resident + 1 > Cap { newest } else { issued + 1 };
                oldest = if resident == 0 {
                    issued + 1
                } else {
                    if resident + 1 > Cap && Buggy == 0 { oldest + 1 } else { oldest }
                };
                issued = issued + 1;
            }
            action ExpireOne when (resident > 0) {
                resident = resident - 1;
                newest = if resident > 1 { newest } else { 0 };
                oldest = if resident > 1 { oldest + 1 } else { 0 };
            }
            action Reset when (resident > 0) {
                resident = 0;
                newest = 0;
                oldest = 0;
            }

            invariant HoldsTheLatestArrivals:
                if resident > 0 {
                    newest == issued && oldest + resident == issued + 1
                } else {
                    newest == 0 && oldest == 0
                };
            bound StateBounds: resident <= Cap && issued <= MaxIssued;
        }
    }
}

/// Capability secrecy information-flow model comparing 0600-file provisioning against inheritable env.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn capability_secrecy_model() -> Model {
    props::two_stage_leak(props::TwoStage {
        name: "CapabilitySecrecy",
        stage: "published",
        stage_act: "Provision",
        stage_rhs: if_(eq(cst("Buggy"), int(1)), int(2), int(1)),
        leak: "peer_has",
        leak_act: "SandboxedRead",
        leak_guard: gt(var("published"), int(0)),
        leak_rhs: if_(eq(var("published"), int(2)), int(1), int(0)),
        inv: "NoSecretToSandboxedPeer",
        inv_expr: eq(var("peer_has"), int(0)),
    })
}

/// Discovery publish-ordering model requiring socket bind before directory publication.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn publish_ordering_model() -> Model {
    props::happens_before(props::Ordering {
        name: "PublishOrdering",
        a: "bound",
        a_act: "Bind",
        b: "published",
        b_act: "Publish",
        inv: "PublishImpliesBound",
    })
}

/// Control-proxy reply-fidelity model forbidding error reports after host verb delivery.
#[cfg_attr(trust_verify, trust::skip)]
pub fn reply_fidelity_model() -> Model {
    aterm_spec::ty_model! {
        ReplyFidelity {
            const Buggy = 0;
            var delivered = 0;
            var reported_err = 0;
            action Deliver when (delivered == 0 && reported_err == 0) { delivered = 1; }
            action DialFail when (delivered == 0 && reported_err == 0) { reported_err = 1; }
            action RelayFail when (delivered == 1 && reported_err == 0) {
                reported_err = if Buggy == 1 { 1 } else { 0 };
            }
            invariant NoErrorAfterDelivery: delivered == 0 || reported_err == 0;
        }
    }
}

/// Cursor-companion tenure dwell-gate model preventing identity changes under alternating flicker.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn companion_tenure_flicker_model() -> Model {
    aterm_spec::ty_model! {
        CompanionTenureFlicker {
            const Buggy = 0;
            const Tenure = 2;
            var worn = 0;
            var cand = 2;
            var dwell = 0;
            var changes = 0;
            var last = 0;
            var steps = 0;

            action SeeProgram when (steps <= 7 && last == 0) {
                steps = steps + 1;
                last = 1;
                worn = if Buggy == 1 { 1 }
                       else { if worn == 1 { 1 }
                              else { if cand == 1 && Tenure <= dwell { 1 } else { worn } } };
                changes = if Buggy == 1 { if worn == 1 { changes } else { changes + 1 } }
                          else { if worn == 1 { changes }
                                 else { if cand == 1 && Tenure <= dwell
                                        { changes + 1 } else { changes } } };
                cand = if worn == 1 { 2 }
                       else { if cand == 1 { (if Tenure <= dwell { 2 } else { 1 }) }
                              else { 1 } };
                dwell = if worn == 1 { 0 }
                        else { if cand == 1 { (if Tenure <= dwell { 0 } else { dwell + 1 }) }
                               else { 0 } };
            }

            action SeeNone when (steps <= 7 && last == 1) {
                steps = steps + 1;
                last = 0;
                worn = if Buggy == 1 { 0 }
                       else { if worn == 0 { 0 }
                              else { if cand == 0 && Tenure <= dwell { 0 } else { worn } } };
                changes = if Buggy == 1 { if worn == 0 { changes } else { changes + 1 } }
                          else { if worn == 0 { changes }
                                 else { if cand == 0 && Tenure <= dwell
                                        { changes + 1 } else { changes } } };
                cand = if worn == 0 { 2 }
                       else { if cand == 0 { (if Tenure <= dwell { 2 } else { 0 }) }
                              else { 0 } };
                dwell = if worn == 0 { 0 }
                        else { if cand == 0 { (if Tenure <= dwell { 0 } else { dwell + 1 }) }
                               else { 0 } };
            }

            invariant FlickerNeverLands: changes == 0;
        }
    }
}
