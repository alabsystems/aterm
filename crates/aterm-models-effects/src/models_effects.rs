// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Sparkle, ignition, cursor-cat and rainbow-kitty effect models, plus the error-class trio,
//! re-exported from this crate's root.

use aterm_spec::derive::*;

/// Pointer strokes are input-driven and rate limited. This projection distinguishes absent
/// contact, partial travel and a completed slow stroke; geometry, speed and time thresholds are
/// left to the detector's regression tests. `cooldown_due` is the obligation the latest earned
/// stroke creates, so resetting contact cannot erase it. `Buggy = 1` admits stationary, partial
/// and cooling samples as strokes, drops the cooldown on reset, and keeps abandoned contact.
/// Tier-1 in `aterm-effects/src/pet_stroke.rs` drives the real detector, reads its contact,
/// travel and deadline fields, and binds each transition.
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
            // Input classes: seed, slow partial, still, completed stroke,
            // cooling movement, contact reset, rejected movement, expiry.
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

/// Sparkle-words identity and persistence episodes (docs/sparkle-words-v2-design.md §3.6/§9): one
/// word identity's episode lifecycle over the GUI's grace-TTL persist map.
///
/// * `Appear`: a persist miss births an episode; the genome rolls once (`rolls`) and the
///   per-episode nova re-arms. `MaxBirths` guard-bounds the re-enterable
///   Absent→Live→Grace→Absent cycle so explicit-state BFS terminates (§9).
/// * `Vanish`/`Rehit`: occlusion enters grace; a re-hit within grace continues the episode. At
///   `Buggy = 1` the re-hit re-rolls the genome.
/// * `Rekey`: a terminal redraw moves the same logical occurrence, changing its position key but
///   not its genome, spent-nova bit, done mark or fire count. `Buggy = 1` treats the move as a
///   fresh identity, re-rolling and resetting the spent guards.
/// * `ContextMove`: the same occurrence moves while its row-local neighbor/status context
///   changes. Healthy recognition still transfers the episode; `Buggy = 1` rejects the changed
///   fingerprint, births falsely and re-arms the spent occurrence.
/// * `Tick`/`Expire`: grace ages out; only true expiry re-rolls.
/// * `Ignite`: one nova per episode; the `Buggy` threshold admits a re-ignition (the
///   print-erase-print strobe the grace map exists to stop).
///
/// Invariants: `GenomeFrozen: rolls = births`, `OneNovaPerEpisode: nova <= 1`,
/// `PlayedOnce: fires <= 1`, plus `RecognitionComplete` and `NoFalseBirths` for context-changing
/// moves.
///
/// Tier-1: `sparkle_identity_conformance_real_persist_map` in
/// `aterm-effects/src/word_decorations.rs` drives the real `WordDecorations` persist map with a
/// fake clock and validates the projected `(state, rolls, nova)` trace, with a grace-less
/// negative control; the public-API redraw regression is
/// `aterm-effects/tests/word_reflow_identity.rs`.
#[must_use]
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
pub fn sparkle_identity_model() -> Model {
    aterm_spec::ty_model! {
        SparkleIdentity {
            const Buggy = 0;
            const GraceMax = 3;
            const MaxBirths = 3;  // bounds the re-enterable episode cycle (finiteness guard)
            const MaxRekeys = 2;  // bounds same-episode position changes
            const MaxContextMoves = 2;
            var state = 0;    // 0 Absent, 1 Live, 2 Grace
            var births = 0;   // actual fresh episode allocations
            var rolls = 0;    // genome rolls
            var age = 0;      // grace age (rescans since last_seen)
            var nova = 0;     // novas fired this episode
            var done = 0;     // the identity's done mark; survives Expire
            var fires = 0;    // ignitions across every episode generation
            var rekeys = 0;   // logical occurrence position changes
            var context = 0;  // bounded row-local context version
            var context_moves = 0;      // logical moves whose local context changed
            var recognized_moves = 0;   // those moves transferred as the same episode
            var false_births = 0;       // context moves misclassified as fresh
            action Appear when (state == 0 && births <= MaxBirths - 1) {
                // An Appear from the done set is born done (Ignite stays disabled). The genome
                // still rolls (born-done words show settled ink), so GenomeFrozen holds.
                state = 1; births = births + 1; rolls = rolls + 1; nova = 0;
            }
            action Vanish when (state == 1) { state = 2; age = 0; }
            action Rehit when (state == 2) {
                state = 1; age = 0;
                rolls = if Buggy == 1 { rolls + 1 } else { rolls };  // buggy: re-roll on re-hit
            }
            action Rekey when (state == 1 && rekeys <= MaxRekeys - 1) {
                rekeys = rekeys + 1;
                // Healthy: only the map key changes. Buggy: a fresh-identity replacement
                // re-rolls and forgets both spent guards, so PlayedOnce catches a second Ignite.
                rolls = if Buggy == 1 { rolls + 1 } else { rolls };
                nova = if Buggy == 1 { 0 } else { nova };
                done = if Buggy == 1 { 0 } else { done };
            }
            action ContextMove when (state == 1
                && context_moves <= MaxContextMoves - 1
                && rekeys <= MaxRekeys - 1
                && births <= MaxBirths - 1)
            {
                // The occurrence is unchanged but its row-local neighbors changed while a
                // same-width redraw moved it. The healthy recognizer transfers the episode; the
                // buggy one takes the real fresh-allocation path, so births and rolls stay
                // consistent and only the recognition obligations catch it before the second
                // Ignite.
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
            // v3 §1.3: the done flag survives Expire, so a re-born episode never re-fires.
            invariant PlayedOnce: fires <= 1;
        }
    }
}

/// Bounded repeated-surface policy, the group complement to [`sparkle_identity_model`]'s
/// single-occurrence `ContextMove`. Not a general same-form oracle: each action names one
/// licensed matcher premise for spent episodes in one collision-free exact-`FormId` group.
///
/// * `MovePair`: an immediate same-width global redraw moves both old occurrences within the
///   bounded linear window with no stationary same-seed anchor; 2 → 2 is two transfers and no
///   birth. The buggy exact-context-only classifier misses both.
/// * `GrowOne`: the same premises, but 2 → 3 holds one logically new occurrence: two transfer
///   and exactly one is fresh/armed. The buggy context gate births all three.
/// * `RotatePair`: a recent log-tail rotation with a stationary same-seed survivor, which
///   anchors the group: one old episode transfers and the new bottom occurrence is fresh. The
///   buggy cardinality-only classifier also transfers the departed twin.
/// * `BlankGrace`: after more than two blank occlusion scans the exact seed and context return
///   untainted; still the same episode, so it transfers despite exceeding the weak redraw
///   window. The buggy blanket recency gate births it.
/// * `TypedRetype` / `RecentTypedRetype`: for the feline class only, after nonblank incremental
///   replacement the same form returns at the same seed and context but with continuity
///   tainted. Both the >2-scan case and a coalesced one-partial case inside the weak window are
///   logically new and must be one fresh/armed birth; the buggy exact-evidence path steals the
///   spent episode, so the kitty does not activate after more typing. Profanity does not use
///   these actions: it transfers an exact surface for the full grace lifetime, so unrelated
///   `fix` composer churn cannot relight it.
///
/// `expected_fresh`/`logical_new` encode the scenario truth independently of the classifier.
/// `RecognitionComplete` catches missed licensed transfers and `NoFalseTransfers` identity theft
/// outside those premises; neither is inferred from raw 2 → 2 cardinality.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn sparkle_reflow_cardinality_model() -> Model {
    aterm_spec::ty_model! {
        SparkleReflowCardinality {
            const Buggy = 0;
            const OldCount = 2;
            const WeakMaxScans = 2;
            var state = 0;             // 0 ready, 1 classified
            var global_redraw = 0;     // same-width/recent/local, no stationary anchor
            var stationary_anchor = 0; // same-seed survivor licenses rotation semantics
            var typed_retype = 0;      // same form after nonblank replacement typing
            var recent_typed_retype = 0; // coalesced retype inside the weak window
            var blank_grace = 0;       // >2 blank scans preserve untainted exact continuity
            var recent = 0;            // within the two-scan weak-continuity window
            var seq_gap = 0;           // rescans since the candidate old episode was seen
            var stale_same_seed = 0;   // exact position seed beyond the weak recency window
            var exact_context = 0;     // candidate context equals the spent episode context
            var continuity_tainted = 0; // intervening nonblank replacement invalidated continuity
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
            // Every candidate is transferred or born (dial 2 loses a survivor).
            invariant CandidateAccounting: transferred + fresh == new_count;
            // No more transfers than old episodes (dial 3 claims one twice).
            invariant SurvivorsBounded: transferred <= OldCount;
            // The matcher's premises are one consistent scenario: dial 5 sees a stationary
            // anchor in a global redraw, dial 6 leaves a recent retype untyped, dials 7 and 8
            // misplace the weak window, dial 9 taints a blank grace, dials 10 and 11 swap a
            // blank grace and a typed retype.
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
            // No law restates the scenario truth's own partitions (`expected_recognized +
            // logical_new == new_count`, `expected_fresh == logical_new`) or the definition
            // `recognized + false_transfers == transferred`: the truth is fixed per scenario
            // and a transfer is licensed or false by definition, so no defect reaches them.
            // RecognitionComplete and NoFalseTransfers hold `transferred` from both sides.
            invariant FreshMatchesExpected: fresh == expected_fresh;
            invariant ArmedMatchesExpected: armed == expected_fresh;
            // Arming follows birth (dial 4 arms beyond its births).
            invariant ArmedMatchesFresh: armed == fresh;
            // Directional forms for the Tier-1 fresh-birth fault injection; the equality above
            // also catches under-birth from a false transfer in RotatePair/TypedRetype.
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

/// Repeated explicit feline retyping versus session done-mark poisoning. Each completed,
/// continuity-tainted `kitty` token is a new intentional episode and arms once; closing the
/// replaced episode installs no redraw done mark. `Buggy = 1`: the outgoing episode writes a
/// mark, so the second retype is born-done and inert.
///
/// Tier-1: `word_decorations::tests::two_consecutive_recent_kitty_retypes_both_arm` drives two
/// real partial→complete cycles at the same identity key and checks the birth/armed counters.
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

/// Capacity-boundary transaction for the sparkle identity persist map. The alignment pass pulls
/// one unmatched old episode from a full map, admits a freshly observed replacement, then offers
/// the old episode back for grace; the offer is an LRU union that departs one episode, so
/// cardinality stays at `Cap`. `Buggy = 1` is a raw grace `insert`, reaching `Cap + 1`.
///
/// Tier-1: `word_decorations::tests::persist_cap_drops_unmatched_grace_after_fresh_move` drives
/// this transaction through the real scanner and persist map, checks the fresh claimant stayed
/// resident, and that the departed one-shot's done mark was written.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn sparkle_persist_capacity_model() -> Model {
    aterm_spec::ty_model! {
        SparklePersistCapacity {
            const Buggy = 0;
            const Cap = 3;
            var resident = 3; // full map before the alignment transaction
            var pulled = 0;   // unmatched old episode held in alignment scratch
            var admitted = 0; // freshly visible replacement episodes admitted
            var departed = 0; // episodes whose grace/done lifecycle was closed
            var phase = 0;    // 0 Full, 1 Pulled, 2 FreshAdmitted, 3 Committed
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
            // Every episode is resident, pulled or departed (dial 2 drops the pulled one
            // without closing its lifecycle).
            invariant Conservation:
                resident + pulled + departed == Cap + admitted;
            bound PulledBounded: pulled <= 1;
            bound PhaseBounded: phase <= 3;
        }
    }
}

/// Sparkle-words supernova phase machine (docs/sparkle-words-v2-design.md §6.1/§9):
/// `{0 Armed, 1 Dip, 2 Flash, 3 Ring, 4 Debris, 5 Ember, 6 Settled}` with one monotone `Step`
/// (guard-bounded by `MaxSteps`) that counts a flash on entering Flash (pre-state phase 1), and a
/// `Rearm` (bounded by `MaxArms`) that resets both `phase` and `flashes`. Both resets are
/// load-bearing: resetting only `phase` breaks `OneFlashPerArm` on the second cycle. The
/// per-episode one-nova property is [`sparkle_identity_model`]'s `nova`.
///
/// `Buggy = 1`: `Rearm` re-enters Flash directly, refuted on `OneFlashPerArm`. The fuel invariant
/// `CanSettle: steps + 6 <= MaxSteps + phase` means the finiteness guard strands no reachable arm.
/// `MaxSteps = 18` is 6 steps × (1 + `MaxArms`) walks; 12 would strand the third arm.
///
/// Tier-1, two binds. `word_decorations::nova_host_conformance` drives the real
/// `WordDecorations` engine over a real terminal through three arms and what lies between (the
/// flash limiter's grant, the spent mark, a grace re-hit and an identity rebirth that must not
/// re-ignite, a suspend frozen mid-crown and thawed, two `hard_reset` re-arms), projecting `phase`
/// from the host's grant and `flashes` from the light its nova stream carries; every window
/// boundary validates as `Step`, every re-arm as `Rearm`, and the `Buggy = 1` re-flash built from
/// the real settled state is rejected. `tests/nova_phase_conformance.rs` walks the shipping
/// `nova::phase` and crown emitter at 1 ms for every genome duration against `Step`.
#[must_use]
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
pub fn nova_phase_model() -> Model {
    aterm_spec::ty_model! {
        NovaPhase {
            const Buggy = 0;
            const MaxSteps = 18;  // 6-step walk × (1 + MaxArms) arms: every arm can settle
            const MaxArms = 2;
            var phase = 0;   // 0 Armed .. 6 Settled
            var steps = 0;   // guard-bounds the monotone walk (finiteness)
            var flashes = 0; // Flash entries THIS arm
            var arms = 0;    // guard-bounds re-entry (finiteness)
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
            // §9 self-termination as a fuel invariant: MaxSteps - steps covers the 6 - phase
            // steps left to Settled. Dial 2 re-arms once past the cap the budget was sized for.
            invariant CanSettle: steps + 6 <= MaxSteps + phase;
        }
    }
}

/// Sparkle-words flash limiter (docs/sparkle-words-v2-design.md §6.4/§9): WCAG 2.3.1 as a
/// machine-checked property. Time is 250 ms ticks; the rolling second is a 4-slot ring of
/// per-tick ignition counts (`Shift`'s simultaneous updates are a correct rotation without
/// modulo). `Overlap` is a scenario constant (two candidates' regions overlap); `2·x` is `x + x`.
///
/// `IgnitionBound`: at most 2 ignitions per rolling second, 1 under overlap. `RegionFlashPairs`:
/// the region-scoped flash-pair count (2 pairs per ignition, co-charged to a region only when
/// regions overlap) stays at most 3. It is not window-global: two disjoint ignitions expose their
/// pairs to different regions, and a window-global "pairs ≤ 3" contradicts "ignitions ≤ 2" at 2
/// pairs each (WCAG counts flashes per region).
///
/// `Buggy = 1` removes the overlap tightening, so the counterexample needs both `Buggy = 1` and
/// `Overlap = 1` (the Tier-0 test drives `to_cfg_with`); `Buggy = 1, Overlap = 0` stays green,
/// since disjoint novas at 2/s are legal.
///
/// Tier-1 (the limiter is the WCAG argument, so no per-frame flash audit ships):
/// `flash_limiter_conformance_real_limiter_projects_onto_model` drives the real `grant_ignition`
/// queue through disjoint and overlapping ignition storms and projects every decision onto this
/// model (grants ↔ `Ignite` admitted, delays ↔ `Ignite` disabled until the matching `Shift`),
/// with `flash_limiter_negative_control_overlap_blind_limiter_is_buggy_trace` as the non-vacuity
/// twin; `limiter_delays_third_of_three_ignitions_deterministically` and
/// `limiter_tightens_to_one_per_second_on_overlap` drive the limiter through the full host tick.
#[must_use]
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
pub fn flash_limiter_model() -> Model {
    aterm_spec::ty_model! {
        FlashLimiter {
            const Buggy = 0;
            const Overlap = 0;   // scenario: two candidates' regions overlap (§6.4)
            // The overlap the code also runs under, where the mutant bites.
            scenario Overlap = 1;
            var s0 = 0; // ignitions per 250 ms slot, newest…
            var s1 = 0;
            var s2 = 0;
            var s3 = 0; // …oldest
            action Shift { s3 = s2; s2 = s1; s1 = s0; s0 = 0; } // simultaneous ⇒ true rotation
            action Ignite when (s0 + s1 + s2 + s3
                <= if Overlap == 1 { if Buggy == 1 { 1 } else { 0 } } else { 1 })
            { s0 = s0 + 1; }
            // window-wide ignition bound: <= 2, tightening to <= 1 under overlap
            invariant IgnitionBound:
                s0 + s1 + s2 + s3 <= if Overlap == 1 { 1 } else { 2 };
            // Region-scoped flash pairs (2 per ignition on a shared region) stay <= 3.
            invariant RegionFlashPairs:
                (if Overlap == 1 { (s0 + s1 + s2 + s3) + (s0 + s1 + s2 + s3) }
                 else { 2 }) <= 3;
        }
    }
}

/// Sparkle-words flash limiter, window-wide: the WCAG 2.3.1 argument of [`flash_limiter_model`]
/// with the enforcer count as a dial. [`flash_limiter_model`] is true of one limiter however many
/// exist, so it cannot see per-pane limiters: give each split pane its own `WordDecorations` and
/// each limiter satisfies `IgnitionBound` while a viewer, with one retina, sees 2N flashes a
/// second.
///
/// Two enforcers (`a0..a3`, `b0..b3`, each the same 4-slot rolling second) share one `Shift`
/// clock, with two dials:
///
/// * `Instances` — live enforcers; `Spawn` admits the second only when `Instances > 1`, so
///   `Instances = 1` collapses onto the single-limiter model.
/// * `Local` — what each enforcer consults. `Local = 0` (committed) is the shipping shape, one
///   limiter whose guard sees every reservation; `Local = 1` is per-pane, each enforcer blind to
///   the other's slots.
///
/// `WindowIgnitionBound` is charged against the sum. At `Local = 1, Instances = 2` two
/// individually correct limiters overshoot it at every scenario corner, unlike the `Buggy = 1`
/// overlap defect, which also needs `Overlap = 1`: multiplication alone is the defect.
///
/// No state machine can know how many engines the host builds, so this does not catch the
/// refactor; it machine-checks the sentence the scope-cardinality census (`aterm-census`
/// OB-13..OB-16, claim `flash-limiter`) would otherwise assert in prose. OB-16 fails if this
/// model is deleted, and `prove_catch_and_multiply_scalar` fails if it goes toothless.
#[must_use]
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
pub fn flash_limiter_window_model() -> Model {
    aterm_spec::ty_model! {
        FlashLimiterWindow {
            const Buggy = 0;
            const Overlap = 0;    // scenario: two candidates' regions overlap (§6.4)
            const Local = 0;      // 1 = each enforcer only sees its OWN slots
            const Instances = 2;  // live enforcers (2 = one per pane, the refactor)
            // The overlap the code also runs under, where the mutant bites.
            scenario Overlap = 1;
            var a0 = 0; // enforcer A's ignitions per 250 ms slot, newest…
            var a1 = 0;
            var a2 = 0;
            var a3 = 0; // …oldest
            var b0 = 0; // enforcer B, same rolling second
            var b1 = 0;
            var b2 = 0;
            var b3 = 0;
            var live2 = 0; // is the second enforcer live?

            // One wall clock, one retina: the rotation is simultaneous for both.
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

            // The bound the retina sees: every live enforcer's slots at once.
            invariant WindowIgnitionBound:
                a0 + a1 + a2 + a3 + b0 + b1 + b2 + b3
                    <= if Overlap == 1 { 1 } else { 2 };
        }
    }
}

/// The §3.2 two-way global burst mutex over the shared `nova_add` channel (sparkle words,
/// `aterm-effects` `supernova.rs` / `word_decorations.rs`).
///
/// A supernova's per-frame decoration quads are bounded by `S_MAX_BOUND` (900) and a classic
/// nova's by 392; the channel funds at most `MAX_NOVA_QUADS` (1536). `MAX_ACTIVE_SUPERNOVAE = 1`
/// and the two-way mutex (a supernova grant defers behind any live classic window, classic grants
/// behind a live supernova) keep the funded share inside the budget without the funding clamp
/// truncating a live effect. The const-asserts in `supernova.rs` cannot see a second live
/// instance; this machine states the instance count. The mutex is window-wide over every pane's
/// episode shard (the scope census's `supernova-burst-mutex` claim, whose `machine` this is).
///
/// `Buggy = 1` admits a second supernova beside a live one (1800) and a classic beside a live
/// supernova (1684); each falsifies a named invariant.
///
/// Tier-1: `word_decorations`'s `real_burst_grants_conform_to_the_supernova_mutex_model` drives
/// the real `super_prepass`/`nova_prepass` grants over a bound pane and a parked one and checks
/// every grant against `action_enabled`, with a negative control. The bind is test-only (the
/// grant sites carry no `#[refines]` anchor), so spec-link closure does not see it; the
/// non-vacuity sweep covers its invariants.
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
            // Live supernova windows, window-wide (every pane's shard).
            var supers = 0;
            // Live classic nova windows.
            var classics = 0;
            // The decoration share of `nova_add` those windows fund per frame.
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

/// Ownership and cardinality of delayed sparkle-word ignition slots. A future reservation is
/// one-to-one with a live persist episode; when its owner departs it is cancelled, since no
/// flash occurred. A slot that already fired moves into the rolling safety history and survives
/// its owner until its one-second window expires, so at most `Cap` owner-backed future slots
/// plus `RecentCap` fired slots are resident. `Buggy = 1` sweeps only on expiry, so a cancelled
/// owner leaves its future slot behind and churn exceeds the pending bound.
///
/// Tier-1: `ignition_reservation_lifecycle_real_queue_conforms` drives the real
/// `grant_ignition`/`prune_ignitions` queue through immediate and delayed grants, future- and
/// fired-owner departure, and history expiry, with an expiry-only negative control.
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
            // Dial 2 fires a pending reservation into a full recent window.
            invariant RecentBound: recent <= RecentCap;
            invariant ReservationBound: pending + recent <= Cap + RecentCap;
            // Dial 3 reserves one past the cap.
            invariant LiveBound: live <= Cap;
        }
    }
}

/// Atomic ownership transfer for a delayed ignition across episode alignment. Alignment may
/// replace an occurrence identity while keeping the episode's frozen `nova_start`; the future
/// limiter slot moves in the same operation, or pruning drops it as ownerless and a competing
/// overlapping request claims the same rolling window although the rekeyed episode will still
/// flash. `Buggy = 1` skips the owner rewrite.
///
/// Tier-1: `ignition_reservation_rekey_real_queue_conforms` drives the real rekey helper and
/// limiter through GrantDelayed -> Rekey -> Prune -> CompetingGrant, then without the owner
/// transfer as a negative control.
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

/// Bounded constant-selection LRU for completed sparkle-word episodes. The oldest slot is
/// addressed through an intrusive head link, so replacement at capacity makes one oldest-slot
/// selection whatever `resident` is. `Buggy = 1` probes every resident (a `min_by_key` scan),
/// breaking `ConstantSelection` once the map holds more than one entry; cardinality stays bounded
/// in both, so the counterexample is about work, not overflow.
///
/// Tier-1: `done_mark_lru_real_order_and_constant_work_conform` drives the shipping LRU at a
/// small cap, checks touch/eviction order and fixed link-write work, and runs a full scan
/// against the buggy model.
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

            // Dial 2 inserts into a full map without evicting.
            invariant ResidentBounded: resident <= Cap;
            invariant ConstantSelection: selections <= 1;
            bound EvictionsBounded: evictions <= 1;
            bound TouchesBounded: touches <= 1;
        }
    }
}

/// Sparkle-words one-shot peek (docs/sparkle-words-v3-design.md §1.2/§1.3): a graphic plays
/// exactly once per word appearance, `Idle → Rise → Dwell → Descend → Done`, and Done is
/// absorbing per episode (zero quads, zero wakes). In-dwell life is pure time inside Dwell and
/// never re-enters Rise. `Buggy = 1` lets a `Repeek` re-enter Rise after Done (ordinal churn,
/// grace expiry or a reset re-birthing a finished episode), refuted by `NoRepeek`. `CanFinish`
/// is the fuel invariant.
///
/// Tier-1: aterm-effects' `one_shot_peek_conformance_real_engine` drives the real engine across
/// rescans, occlusion shorter and longer than `GRACE_TTL`, twin growth/shrink/rotation,
/// freeze/thaw mid-rise and an unfocused birth, projecting the emitted-quad phases.
#[must_use]
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
pub fn one_shot_peek_model() -> Model {
    aterm_spec::ty_model! {
        OneShotPeek {
            const Buggy = 0;
            const MaxSteps = 8;  // fuel: one full walk + repeek attempts (finiteness)
            var phase = 0;       // 0 Idle, 1 Rise, 2 Dwell, 3 Descend, 4 Done
            var steps = 0;
            var rises = 0;       // entrances started for this episode
            action Start when (phase == 0 && steps <= MaxSteps - 1) {
                phase = 1; rises = rises + 1; steps = steps + 1;
            }
            action Step when (phase > 0 && phase <= 2 && steps <= MaxSteps - 1) {
                phase = phase + 1; steps = steps + 1;
            }
            action Finish when (phase == 3 && steps <= MaxSteps - 1) {
                phase = 4; steps = steps + 1;  // Done: absorbing (see Repeek)
            }
            action Repeek when (phase == 4 && steps <= MaxSteps - 1) {
                // Buggy 1: a re-Rise after Done (ordinal churn, grace recount, reset); Buggy 2: a
                // finished peek reset to Idle.
                phase = if Buggy == 1 { 1 } else { if Buggy == 2 { 0 } else { phase } };
                rises = if Buggy == 1 { rises + 1 } else { rises };
                steps = steps + 1;
            }
            invariant NoRepeek: rises <= 1;   // Done is absorbing per episode
            bound PhaseBounded: phase <= 4;
            // Fuel: the unspent budget covers the 4 - phase steps left to Done. Dial 2 resets a
            // finished peek to Idle, spending the fuel of a walk it never takes.
            invariant CanFinish: steps + 4 <= MaxSteps + phase;
        }
    }
}

/// Cursor-cat host gate and collectible discovery lifecycle. Ordinary rainbow kitty momentum is
/// owned by the cursor-trail master: input while it is off cannot arm or draw an ordinary
/// flight, and turning it off retracts an existing arm. A collection hello is a separate bounded
/// promise and stays drawable with the master off.
///
/// A newly collected look gets a guaranteed, bounded hello even when cursor momentum is cold:
/// `Hidden -> Discovery -> Fade -> Hidden`. `visible` is the draw obligation; `forced` the
/// stronger promise that cursor style and momentum cannot dismiss the hello.
///
/// Time is five presentable host-frame samples: three discovery, two fade. `HiddenTick` is an
/// unfocused or host-suppressed sample: it may hide the draw but cannot consume `elapsed`,
/// `presented` or the forced hold; `Tick` resumes the same lifecycle and `HiddenAtDeadline` pins
/// completion to a quiescent state. `LongPresentableGap`: after at least one visible discovery
/// frame, one delayed sample past the whole hold-plus-fade deadline settles straight to Hidden,
/// without a late opaque Fade frame and another animation tail.
///
/// `Buggy = 1`: `TypeWhileTrailOff` arms an ordinary cat behind a disabled trail master;
/// `HiddenTick` consumes the wall clock while `presented` stays fixed, so hidden samples walk
/// Discovery to Hidden without the promised hold; `LongPresentableGap` strands an expired hello
/// in a visible Fade.
///
/// Tier-1 drives the real `aterm_effects::kitty_cursor::CursorCat` clock and projects its frame
/// state and host presentability at the same boundaries.
#[must_use]
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_cat_model() -> Model {
    aterm_spec::ty_model! {
        CursorCat {
            const Buggy = 0;
            const HoldTicks = 3;
            const TotalTicks = 5;
            const MaxHidden = 5;
            var phase = 0;       // 0 Hidden, 1 Discovery, 2 Fade
            var elapsed = 0;     // presentable lifecycle samples since Collect
            var presented = 0;   // samples actually eligible for presentation
            var hidden = 0;      // bounded suppressed/unfocused host samples
            var presentable = 0; // whether the latest sample could be drawn
            var collections = 0; // one-shot bound keeps the state space finite
            var visible = 0;     // the host must draw a non-zero-alpha cat
            var forced = 0;      // discovery bypasses style/momentum dismissal
            var presented_once = 0; // at least one real visible frame was delivered
            var wall_expired = 0;   // presentable clock is beyond hold + fade
            var trail_master = 0;   // host's ordinary rainbow-kitty owner gate
            var ordinary_armed = 0; // ordinary momentum owns host presentation
            var ordinary_visible = 0; // ordinary branch may be drawn
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
                // One presentable observation past the whole hold + fade, not a burst of
                // synthetic ticks; production consumes the elapsed tail atomically.
                wall_expired = 1;
                elapsed = TotalTicks;
                presented = presented + 1;
                presentable = 1;
                phase = if Buggy == 1 { 2 } else { 0 };
                visible = if Buggy == 1 { 1 } else { 0 };
                forced = 0;
            }
            // A collected hello stays in its protected Discovery phase until the host has had
            // HoldTicks presentable opportunities, unless a presentable observation is already
            // past the whole lifecycle. HiddenTick cannot set wall_expired.
            invariant ForcedUntilPresented:
                collections <= forced +
                    (if presented > HoldTicks - 1 { 1 } else { 0 }) + wall_expired &&
                collections <= (if phase == 1 { 1 } else { 0 }) +
                    (if presented > HoldTicks - 1 { 1 } else { 0 }) + wall_expired;
            // Dial 2 keeps drawing through a hidden sample.
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
            // Dial 3 shows the hello without recording its first real frame.
            invariant VisibleFrameWasPresented: visible <= presented_once;
            invariant TrailMasterOwnsOrdinary:
                if trail_master == 0 {
                    ordinary_armed == 0 && ordinary_visible == 0
                } else { ordinary_visible == ordinary_armed };
            // Dial 4 hides the hello with the trail master.
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

/// The classic flying cursor kitty's forward/reverse wrap placement. A fold begins on the old
/// edge, leaves it wholly off glass, changes sides while wholly off glass, and only then
/// re-enters on the new edge. `off_samples` is the history abstraction Tier-1 binds to the real
/// signed sprite rectangles; an on-glass sample cannot mint it. `Buggy = 1` jumps straight from
/// the old on-glass edge to the new one (`OffGlassBeforeSideChange`).
///
/// Tier-1: `cursor_cat_fold_conformance_real_placement_and_teleport_mutant` drives
/// `WordDecorations::resolve_kitty_cursor_placement` and projects the emitted rectangle after
/// every transition.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_cat_fold_model() -> Model {
    aterm_spec::ty_model! {
        CursorCatFold {
            const Buggy = 0;
            const MaxFolds = 2;
            var phase = 0;       // 0 settled, 1 leave, 2 old-off, 3 new-off, 4 enter
            var direction = 0;   // 0 none, 1 forward (right->left), 2 reverse
            var origin_side = 1; // 0 left, 1 right
            var side = 1;        // edge occupied by the sampled body
            var off_glass = 0;   // the complete body rectangle misses the viewport
            var off_samples = 0; // wholly-off samples observed in this fold
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

/// Cursor-cat reaction to complete profanity cues. Incomplete prefixes never reach the visual
/// reaction; each accepted complete token re-kicks the wince and lengthens the bounded phrase
/// chain through four beats. A hidden companion ignores the cue rather than being summoned.
///
/// `Buggy=1` treats `fuc` as a complete curse and drops `on_curse`'s `!self.is_active()`
/// refusal, so a hidden cat accepts a curse: `on_curse` answers true (a spurious follow-up draw)
/// and the wince and its `Wince` reaction hold leak into a companion not on glass. The next
/// hidden frame's `reset_spine` clears the wince clock but not the reaction hold. Tier-1 drives
/// `CursorCat::on_curse` and projects its reaction and the accepted/rejected decision.
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

/// Sing-along held-key detector. A run arms only on the sixteenth distinct-in-time
/// same-character press. Breaking an unarmed run clears its count; releasing an armed run enters
/// one bounded wind-down, which settles to the byte-identical idle state.
///
/// `Buggy=1` picks one of two wrong arms per run (`PickRampedArm` sets `ramp_fault`, pinned to
/// zero at `Buggy=0`): an eight-press threshold at full drive, refuted by
/// `ArmedRequiresCurrentThreshold` at the eighth press; or a sixteen-press arm whose drive ramps
/// in from zero (easing 0 → 1 from `armed_at`), refuted by `ArmedRunIsAtFullDrive`, since the
/// arming instant would read armed with no drive and the host's 0.33 face gate would keep the
/// ordinary cursor kitty where the singer belongs. The shipping `drive` is 1.0 whenever the run
/// is armed and unreleased.
///
/// No law says an idle run has no drive or a released one some: every writer of phase 0 writes
/// `drive_live = 0`, `Release` writes 1, and the Tier-1 projection derives phases 0 and 2 from
/// the drive, so such laws would restate the writes. Tier-1 pins the release crossfade
/// (`drive > 0` at release).
///
/// Tier-0 is in `derived_ring_ty.rs`; Tier-1 drives `aterm_effects::kitty_sing::KittySing`'s
/// press, release and settle methods in `aterm-effects/tests/kitty_activation_conformance.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kitty_sing_detector_model() -> Model {
    aterm_spec::ty_model! {
        KittySingDetector {
            const Buggy = 0;
            const ArmRepeats = 16;
            const OldArmRepeats = 8;
            var phase = 0;      // 0 idle/accumulating, 1 armed, 2 winding down
            var count = 0;      // same-character presses in the current run
            var drive_live = 0; // full drive or its bounded release tail exists
            // Which arm a `Buggy=1` run carries: 0 the eight-press one, 1 the sixteen-press one
            // that ramps its drive in. Always 0 at `Buggy=0`.
            var ramp_fault = 0;

            // A stutter at `Buggy=0`, so no action is dead at the committed config.
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

/// Cursor-cat sing-along bypass floor. Pinning the canonical momentum for a held-key celebration
/// starts a fresh qualifying run but does not summon the companion until sixteen correlated
/// forward cursor events have arrived. The model isolates that travel guard from the stricter
/// band+dwell law: the bypass is where the independent count is load-bearing.
///
/// `Buggy=1` uses a ten-event floor (`NoCatBeforeSixteen` fails at event ten). Tier-1 drives
/// real `CursorCat::set_singing` and `CursorCat::on_key` calls, with the ten-event mutant as a
/// negative control.
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

            // No separate "active requires singing" law: `Qualify` is the only summons and its
            // guard reads `singing`; outside the bypass the cat earns by band+dwell.
            invariant NoCatBeforeSixteen:
                if active == 1 { run == MinRun } else { run <= MinRun - 1 };
            bound RunBounded: run <= MinRun;
            bound FlagsBounded: singing <= 1 && active <= 1;
        }
    }
}

/// The resident pet keeps pixel and caret custody through every sampled song phase, including
/// direct jumps from the held song into its tail or fully drained state. The model's name is
/// kept for registry compatibility; no replacement head takes over.
///
/// `Buggy=1` hands the glass to a singing head and then blacks out late frames. Tier-1 binds
/// the real host/capture admission helpers and resident alpha to each transition.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn reduced_motion_companion_handoff_model() -> Model {
    aterm_spec::ty_model! {
        ReducedMotionCompanionHandoff {
            const Buggy = 0;
            var phase = 0; // 0 resident, 1 full song, 2 at 0.50, 3 0.49..0.33,
                           // 4 below 0.33, 5 drained
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

/// One-shot routing for the classifier-minted cursor-cat motion pulse shared by ordinary
/// rendering and the extracted/composed render path. A route that observes the pulse takes it
/// from the glow producer and delivers it to the cat exactly once; later route changes see no
/// pulse, so a style or layout transition cannot replay old typing into a new owner.
///
/// The pulse (`docs/design/EFFECTS-LICENSE-REDESIGN.md`) is minted by a spawn that cleared the
/// license seam and was classified a typed advance or fold (`CursorGlow::momentum_pulse`, beside
/// `shape_wrap`/`re_anchor` in `CursorGlow::spawn`). Nothing is pending between key hint and
/// verdict, so `LicenseMove` -> `ClassifyPulse` is the whole provenance chain.
///
/// `Buggy=1` adds two defects: the composed route records an attempt but neither takes nor
/// delivers the pulse, so a later switch to the ordinary route consumes it stale
/// (`stale_replay`); and a pulse minted with no license (`cold_pulse`). Tier-1 binds the route
/// actions to `take_cursor_cat_motion_pulse` plus `forward_kitty_cursor_motion` at the ordinary
/// and composed/extracted seams.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_cat_motion_pulse_routing_model() -> Model {
    aterm_spec::ty_model! {
        CursorCatMotionPulseRouting {
            const Buggy = 0;
            var licensed = 0;
            var classified = 0;
            var pending = 0;
            var route = 0;          // 0 unselected, 1 ordinary, 2 composed/extracted
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
            // The producer mints the pulse inside a licensed spawn, from the classifier.
            // `Buggy=1` lets it mint with no license behind it.
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
            // No separate at-most-once law: the first pins `consumes == deliveries`, and
            // `consumes <= classified` holds by the guards (consuming needs `pending`, which
            // only `ClassifyPulse` sets).
            bound StateBounded:
                licensed <= 1 && classified <= 1 && pending <= 1 && route <= 2
                    && attempted_route <= 2 && consumes <= 1 && deliveries <= 1
                    && switched <= 1 && stranded <= 1 && stale_replay <= 1
                    && cold_pulse <= 1;
        }
    }
}

/// The license, modelled at the seam that asks it: did a human touch the keyboard just now?
/// (`docs/design/EFFECTS-LICENSE-REDESIGN.md`).
///
/// The predicate, evaluated at the top of `CursorGlow::spawn` / `CursorTrail::spawn` before
/// `classify_move` and before any state moves:
///
/// ```text
/// licensed = fresh(type_hint) || fresh(quench_hint) || fresh(nav_hint)
///         || fresh(return_hint) || fresh(newline_hint)
///         || fresh(user_gesture_hint) || synthetic_note_pending
/// ```
///
/// Six key-hint stamps under their own freshness constants (the 0.25 s class), plus the
/// scripted-preview note, which needs no field because `note_synthetic_move` /
/// `note_synthetic_typed` stamp `user_gesture_hint` / `type_hint` at the same instant. The
/// disjunction is class-blind (which choreography fires is `classify_move`'s job), so the model
/// carries one three-valued licence term (`hint`: absent / fresh / stale).
///
/// Claims, derived from the predicate and each with its own `Buggy=1` counterexample:
///
/// * A move with no stamp mints nothing (`cold_admitted`): a token streamer, `cat`, a spinner
///   two rows away.
/// * A move with a stale stamp mints nothing (`stale_admitted`).
/// * `reflow_hint` / `blink_hint` are not licence terms (`morphology_admitted`): a resize and a
///   TUI's repaint blink are morphology inputs, not keypresses, and adding either to the
///   disjunction is the mutant.
/// * A key this window swallowed never licenses (`swallow_admitted`): the `clear_move_license`
///   call sites (mouse, wheel, focus, paste, scroll, tab switch, IME, a11y, `signal`).
/// * A declined move destroys nothing (`wiped`): a denial never wipes the ribbon earlier typing
///   earned; retention is decay, `note_scroll` translation and reset, nothing else.
/// * Every armed press reaches exactly one disposition: consumed by its paired echo, expired,
///   cleared at a swallow boundary, or superseded by a newer press over the one-slot stamp;
///   `admissions <= arms` is the same law counted from the other side.
/// * The press credit budget spends monotonically: spent cells never exceed the cells presses
///   banked, an admitted multi-cell coalesce pays for its ribbon, a starved one pays nothing and
///   stays dark, and retiring a stale stamp never refunds cells (`credit_refunded`).
/// * A press is in flight until its row echoes or an observed edge forgets it (the stall). With
///   no stamp fresh, two or more unpaid presses license the row's late echo as one batch
///   (`InFlightBatchEchoMintsLight`) and one unpaid press licenses its own one-cell echo
///   (`InFlightPressEchoMintsLight`), spent on admission. A keyless move the echo shape refuses
///   forgets them (`UnexplainedHopForgetsCredits`), so a swallowed press never licenses a later
///   batch (`phantom_admitted`). The decline guards read a pool of at most one press: with two
///   or more in flight a keyless move is a batch's echo or an unexplained hop, never merely cold
///   or stale. With one, the model has no hop width, so it admits both the press's own +1 and
///   the engine's refusal of a wider hop, refused and kept. The mutants are patience that never
///   forgets and a one-press echo that does not spend. The model is clockless:
///   `PatienceElapses` is a forget like any other, and its number is pinned by the engine's
///   tests.
/// * A delivered insert's echo cannot spend a key dispatched behind it on the FIFO: a delayed
///   output frame may see both receipts, but the paste bytes precede the key bytes. The mutant
///   takes that first frame as typed, stealing the later key's credit.
/// * The diagnosis ring cannot lie to `ctl trail`: every spawn scores exactly one of
///   `licensed`/`declined`, and a `licensed` row means light was minted.
///
/// Tier-1 (`cursor_glow/tests/licence.rs`, `app_input.rs`) drives the real `CursorGlow` through
/// press, echo, cold echo, expiry and delayed paste/key frames.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_hint_license_model() -> Model {
    aterm_spec::ty_model! {
        CursorHintLicense {
            const Buggy = 0;
            const ArmCap = 2;
            const CoalesceCells = 2;
            const MoveCap = 2;

            // The class-blind licence term: 0 absent, 1 fresh, 2 present but stale; one slot,
            // like every `*_hint: Option<Instant>`.
            var hint = 0;
            // A live `reflow_hint`/`blink_hint`: morphology, never a licence.
            var morphology = 0;
            // The live arm's key was swallowed at the host boundary.
            var swallowed = 0;
            // Presses that armed a licence, and the four dispositions an arm can reach.
            var arms = 0;
            var consumed = 0;
            var expired = 0;
            var cleared = 0;
            var superseded = 0;
            // Moves that passed the licence seam (one per consumed arm).
            var admissions = 0;
            // The press credit budget (`CursorGlow::type_press_ring`). The unpaid pool is
            // `credit_arms - spent - forfeited`: presses not yet laid and not forgotten. A press
            // is in flight until its row echoes (spent) or a move it cannot explain forgets it
            // (forfeited); `just_forgot` says the last spawn was such an edge with no press
            // since, and `phantom_admitted` a batch licensed out of a forgotten pool.
            var credit_arms = 0;
            var spent = 0;
            var forfeited = 0;
            var just_forgot = 0;
            var phantom_admitted = 0;
            var coalesce_births = 0;
            // Light: minted this run, and earned light already on glass.
            var births = 0;
            var resident = 0;
            // The slimmed admission ring (`ctl trail`).
            var spawns = 0;
            var licensed_tally = 0;
            var declined_tally = 0;
            // Witnesses, all 0 at the committed config.
            var cold_admitted = 0;
            var stale_admitted = 0;
            var morphology_admitted = 0;
            var swallow_admitted = 0;
            var wiped = 0;
            var credit_refunded = 0;
            // The delivered insert: a paste's write is queued on the session FIFO and lands
            // later on the writer thread. `write_pending` is that write; `insert_hint` the
            // insert's own licence slot (0 absent, 1 fresh, 2 stale); `insert_undelivered` a
            // live insert arm minted at enqueue, before the bytes landed.
            var write_pending = 0;
            var insert_hint = 0;
            var insert_undelivered = 0;
            // Witness: an insert echo admitted on an arm minted before delivery.
            var undelivered_admitted = 0;
            // A typed press dispatched after the queued insert; its credit cannot own the
            // earlier paste echo even when both receipts reach the renderer before output.
            var later_key_credit = 0;
            var later_key_stolen = 0;

            // A press arms the licence and banks one cell credit. The stamp is one slot: a press
            // over a live stamp supersedes it (its credit survives in the ring), hence four
            // dispositions.
            action PressArmsLicense when (arms <= ArmCap - 1) {
                superseded = if hint == 1 { superseded + 1 } else { superseded };
                hint = 1;
                arms = arms + 1;
                credit_arms = credit_arms + 1;
                swallowed = 0;
                just_forgot = 0;
            }
            // Pointer hover that writes no terminal bytes and owns no local gesture does not
            // dispose of an in-flight key; it stutters the licence state. `Buggy=1` loses the
            // stamp with none of the four legitimate dispositions.
            action ByteSilentPointerPreservesLicense when (hint == 1) {
                hint = if Buggy == 1 { 0 } else { hint };
            }
            // The paste is queued: enqueue is not delivery, so nothing is armed yet (the host's
            // arrival-time gesture stamp is revoked in the same turn). `Buggy=1` arms at
            // enqueue, a licence for bytes not yet landed that program output could spend.
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
            // The writer thread's completed write puts the bytes on the wire and arms the insert
            // licence (one slot, the supersede shape). Its width is its own credit, carried on
            // the stamp, never a press credit in the typed ring.
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
            // The FIFO's paste bytes echo first: the insert arm is spent and the later key's
            // credit kept for the next frame. `Buggy=1` counts every banked key credit, so a
            // same-width paste frame spends the later key's credit as typed.
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
            // The insert window elapses: set, no longer fresh.
            action InsertLicenceExpires when (insert_hint == 1) {
                insert_hint = 2;
                expired = expired + 1;
            }
            action RetireStaleInsertLicence when (insert_hint == 2) {
                insert_hint = 0;
            }
            // The insert's echo: one same-row forward hop, laid as one sweep, spending the stamp
            // and its width. `Buggy=1` leaves the stamp live, so one delivery funds every hop
            // that follows (caught by the disposition conservation).
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
            // A stale insert stamp is not a licence either.
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
            // A resize settle or a TUI repaint blink. Live, and never a licence.
            action MorphologyStampArrives when (morphology == 0) {
                morphology = 1;
            }
            // The freshness window elapses: the stamp is still set, no longer fresh.
            action LicenseExpires when (hint == 1) {
                hint = 2;
                expired = expired + 1;
            }
            // The classifier's `take_if` drops the stale stamp. `Buggy=1` refunds its cell to the
            // press budget, funding a second stray multi-cell sweep.
            action RetireStaleLicense when (hint == 2) {
                hint = 0;
                spent = if Buggy == 1 && spent > 0 { spent - 1 } else { spent };
                credit_refunded = if Buggy == 1 && spent > 0 {
                    1
                } else {
                    credit_refunded
                };
            }
            // `clear_move_license`: a swallowed key's stamp does not outlive the boundary.
            // `Buggy=1` lets it survive.
            action SwallowedKeyClearsLicense when (hint == 1 && swallowed == 0) {
                hint = if Buggy == 1 { 1 } else { 0 };
                cleared = if Buggy == 1 { cleared } else { cleared + 1 };
                swallowed = 1;
            }
            // The licensed typed echo: one hint, one echo, spending the credit its cell lays.
            // `Buggy=1` leaves the stamp live, so one press funds every later echo (caught by
            // the disposition conservation and by `admissions <= arms`).
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
            // A batched multi-cell echo backed by enough cell credits spends them, so the pool
            // never pays for a second stray sweep. `Buggy=1` paints the ribbon for free.
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
            // Licensed and classified, refused by the credit budget alone: vim's `w` is one
            // press echoing as a multi-cell hop. It stays dark, pays nothing, and the ring names
            // `no-credits`; it is an unexplained hop too, so the pool is forgotten. `Buggy=1`
            // bills the budget and tells `ctl trail` it painted.
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
            // The in-flight batch (the stall): no stamp fresh (absent, or stale in place), and
            // at least two unpaid presses license the echo, which spends them. Stale stamps are
            // not consumed (the bank pops only fresh ones). At `Buggy=1` a pool an unexplained
            // hop should have forgotten licenses the batch (`phantom_admitted`).
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
            // The one-press echo (the stalled last key): no stamp fresh and exactly one press
            // unpaid, so its own +1 echo is licensed and the press spent. `Buggy=1` does not
            // spend, so the same press funds a second +1 (`PairedAdmissionsNeverExceedArms`).
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
            // An unexplained hop forgets the credits: a keyless move the echo shape refuses
            // (backward, cross-row, an unlicensed row change, a forward hop the share rule
            // refused). The presses did not describe it, so a swallowed press (a pager's `q`, a
            // password, a modal) never rolls forward as a phantom credit. `Buggy=1` keeps the
            // pool across the hop, and the next batch shape licenses it.
            action UnexplainedHopForgetsCredits when (
                (hint == 0 || hint == 2) && spawns <= MoveCap - 1
                    && credit_arms - spent - forfeited > 0
            ) {
                spawns = spawns + 1;
                declined_tally = declined_tally + 1;
                forfeited = if Buggy == 1 { forfeited } else { credit_arms - spent };
                just_forgot = 1;
            }
            // The in-flight patience elapses with the row silent and the clock forgets the
            // presses. The model has no clock, so it says only that the bound is a forget; the
            // engine's unit tests pin the number.
            action PatienceElapses when (credit_arms - spent - forfeited > 0) {
                forfeited = credit_arms - spent;
            }
            // The cold move: program output nobody's fingers asked for. No stamp, and at most
            // one press in flight (with two or more a keyless move is a batch's echo or an
            // unexplained hop). With one the model has no hop width: a +1 is the press's own
            // echo, and a wider hop is refused with the press kept
            // (`a_single_stalled_press_wider_than_one_cell_is_still_refused_and_kept`), so both
            // are enabled and neither forfeits. `Buggy=1` spawns on every cursor delta.
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
            // The same cold move over light the user's typing earned. `Buggy=1` refuses to
            // mint and wipes the ribbon, invisibly to the ring.
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
            // A move under a live reflow/blink stamp alone: consumed as morphology, never a
            // licence.
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
            // A move whose stamp is set but stale, with at most one press in flight. The press,
            // not the stamp, is the licence: over an empty pool a stale stamp licenses nothing;
            // over one press a +1 is `InFlightPressEchoMintsLight`'s and a wider hop (vim's
            // `w`) is this refusal, the press kept for the next key's bridge.
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
            // A press an unexplained hop forgot never licenses a later batch, and the pool
            // such a hop leaves is empty until the next press; each is falsified alone by
            // `UnexplainedHopForgetsCredits` at `Buggy=1`.
            invariant AForgottenPressNeverLicences: phantom_admitted == 0;
            invariant ForgottenCreditsNeverReturn:
                just_forgot == 0 || credit_arms - spent - forfeited == 0;
            // Only the completed write mints an insert licence; enqueue arms nothing.
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

/// The echo ledger's law (`docs/design/RAINBOW-KITTY-V2.md`, "The late echo's one-press case"):
/// the rainbow kitty engine banks every typed press and lets a licensed typed move pay a hole the
/// seam refused (the cells between the engine's mirror and the move's `from`) out of the presses
/// that could have produced it, one press one cell (`EchoLedger`, `Engine::echo_bridge` in
/// `aterm-effects`).
///
/// The ledger is two buckets and a stale lane, partitioned by the licensing key (the press whose
/// clock the host's sweep carries): `older` (banked before the key; their glyphs lie in the
/// hole), `younger` (the key and the presses in flight behind it, at and past `from`), `stale`
/// (past `ECHO_PATIENCE_S`, an alias of `IN_FLIGHT_PATIENCE_S`, which the next move drops as
/// swallowed). `hole` is the caret's unlicensed advance since the mirror: a refused late echo or
/// program output, which only the ledger can tell apart.
///
/// Claims, each with its own `Buggy = 1` counterexample:
///
/// * `BridgedNeverExceedsOlderPresses`: a move never lays more hole cells than the presses
///   banked before its key. The mutant lets any banked press pay any cell, so a program nudge
///   followed by a fast burst lights the nudge's cell with the burst's in-flight presses.
/// * `AProgramGapIsNeverBridged`: a hole is laid only when the older presses explain it exactly
///   (older 1 with hole 2, or older 2 with hole 1, lays nothing). Same mutant.
/// * `ForfeitedCreditsNeverReturn`: a refused bridge empties the ledger, so its presses never
///   fund a later cell as phantom credits. The mutant keeps them.
/// * `NoBridgeOutsideALicensedMove`: every bridged cell is a press a licensed move spent; a
///   keydown never pre-draws its cell (the mutant does).
/// * `OnePressOneCell`: every banked press is exactly one of waiting, stale, spent, forfeited or
///   expired. The mutant bills a two-cell unswept echo against one press.
/// * `ExpiredPressesBuyNothing`: a press past the patience line is only dropped, never spent.
///   The mutant counts stale presses as payment.
/// * `NoBridgeOnARefusedMove`: a refused bridge lays nothing. The mutant lays a cell for it.
///
/// Abstractions: a press is one cell (the engine banks a wide glyph as two); a swept move is one
/// cell of advance under the host's sweep; an unswept move is a `Batch`-cell echo the host
/// licensed but did not sweep (the host sweeps every typed same-row forward echo except a
/// non-coalesced re-anchor). `Depth` conflates the ledger's entry cap (`ECHO_LEDGER_DEPTH`
/// presses) with the paid move's cell cap, exact only for one-cell presses. Not modelled: the
/// overflow (the engine drops the oldest press past `ECHO_LEDGER_DEPTH`) and the clockless clears
/// `reset` (which also zeroes `bridged`) and `translate_scroll`. Patience elapses per press,
/// oldest first (`OnePressGoesStale`), or all at once (`TimePasses`). A backward typed move
/// re-anchors the mirror, drops stale presses and keeps fresh ones. Move-shaped clears are
/// `LedgerForgotten` (the mirror re-anchors, so the hole is zero); in-place clears are
/// `LedgerClearedInPlace` (the mirror and hole stay).
///
/// Tier-1 (`rainbow_kitty::tests::the_real_engine_conforms_to_the_echo_ledger_model`) drives the
/// real `Engine` through press, refused echo, licensed echo on the press's clock, the
/// nudge-then-burst break, the swallowed-press phantom, the navigation clear, the vim-`w`
/// re-anchor, the unswept batch, fresh and stale retreats, patience expiry whole and per press,
/// and the three in-place clears, projecting the ledger's buckets and tallies off the engine.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn echo_ledger_bridge_model() -> Model {
    aterm_spec::ty_model! {
        EchoLedgerBridge {
            const Buggy = 0;
            // `ECHO_LEDGER_DEPTH`, scaled down: the ledger's live capacity and the largest hole.
            const Depth = 3;
            // Presses one bounded run banks.
            const BankCap = 4;
            // Cells of an unswept echo the host licensed but did not sweep.
            const Batch = 2;

            // The ledger, partitioned by the licensing key.
            var older = 0;
            var younger = 0;
            var stale = 0;
            // The caret's unlicensed advance past the mirror.
            var hole = 0;
            // Tallies: every banked press is spent, forfeited, expired, waiting or stale.
            // `stale_gone` counts the presses that crossed the patience line.
            var banked = 0;
            var spent = 0;
            var forfeited = 0;
            var expired = 0;
            var stale_gone = 0;
            // `Status::bridged` — cells the ledger laid.
            var bridged = 0;
            // The last licensed move as the bridge saw it: the hole cells it laid, the older
            // presses it found, and every cell it bridged (the hole, or the unswept batch).
            var laid_hole = 0;
            var last_older = 0;
            var bridged_delta = 0;
            // The last move was a refused bridge and no press was banked since.
            var just_refused = 0;

            // A press the host will clock its next sweep at: everything banked before it is
            // older than the key. `Buggy=1` pre-draws the key's cell on the keydown.
            action KeyPressed when (
                banked <= BankCap - 1 && older + younger + stale <= Depth - 1
            ) {
                older = older + younger;
                younger = 1;
                banked = banked + 1;
                just_refused = 0;
                bridged = if Buggy == 1 { bridged + 1 } else { bridged };
            }
            // A press behind the key, still in flight: its glyph lies right of the key's cell.
            action PressInFlight when (
                banked <= BankCap - 1 && older + younger + stale <= Depth - 1
            ) {
                younger = younger + 1;
                banked = banked + 1;
                just_refused = 0;
            }
            // The caret advanced a cell with no licensed move: a refused late echo or program
            // output. The engine sees neither; it reads the hole off the next move's `from`.
            action UnlicensedAdvance when (hole <= Depth - 1) {
                hole = hole + 1;
            }
            // The patience elapses on every waiting press (swallowed, not late); they stay on
            // the ledger until a move drops them.
            action TimePasses when (1 <= older + younger) {
                stale = stale + older + younger;
                stale_gone = stale_gone + older + younger;
                older = 0;
                younger = 0;
            }
            // The patience elapses on one press, the oldest waiting: presses are banked in clock
            // order, so the stale prefix is oldest-first, older before younger.
            action OnePressGoesStale when (1 <= older + younger) {
                older = if 1 <= older { older - 1 } else { older };
                younger = if older <= 0 { younger - 1 } else { younger };
                stale = stale + 1;
                stale_gone = stale_gone + 1;
            }
            // The paid bridge: a licensed forward typed move on the mirror's row, one cell of
            // advance under the host's sweep on the key's clock. Stale presses drop first; the
            // hole is exactly the older presses', the advance the key's; spent oldest-first and
            // counted for `trail status`. `Buggy=1` lets any press on the ledger pay any cell.
            action SweptMovePays when (
                (Buggy == 0 && older == hole && 1 <= younger)
                    || (Buggy == 1 && hole + 1 <= older + younger + stale)
            ) {
                expired = if Buggy == 1 { expired } else { expired + stale };
                // `Buggy=1` spends oldest-first across every bucket, stale included.
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
            // The refused bridge: the same move when the older presses do not explain the hole
            // exactly, or the key is not on the ledger. Nothing is laid (the host's sweep lit
            // its own cell) and the ledger is forgotten. `Buggy=1` keeps the presses and lays a
            // cell for the refused hop, once per press so the space stays bounded.
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
            // The unswept echo: a licensed `Batch`-cell typed move the host did not sweep (a
            // non-coalesced re-anchor, or a batch its press credits could not pay for). With no
            // key clock there is no partition: only a move starting at the mirror is
            // attributed, to the oldest presses, and every cell is laid. `Buggy=1` bills the
            // batch against a single press.
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
            // The unswept echo refused: a hole before it (no clock to partition by), or too few
            // presses. Forgotten, like every refusal.
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
            // Move-shaped clears (a non-typed licence, a row change, a hop that landed behind
            // the mirror): the presses no longer describe the row, and the move re-anchors the
            // mirror, so the hole goes with them.
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
            // In-place clears (an erase, a kill, a focus loss): the presses are dropped (stale
            // ones as expired), but nothing moved the mirror, so the hole stays.
            action LedgerClearedInPlace when (1 <= older + younger + stale) {
                expired = expired + stale;
                stale = 0;
                forfeited = forfeited + older + younger;
                older = 0;
                younger = 0;
                bridged_delta = 0;
                laid_hole = 0;
            }
            // A backward typed move (a retreat, a re-anchor, a rewrite landing left of its
            // launch) is not an echo's shape: the mirror moves, fresh presses keep waiting,
            // stale ones are dropped (the engine expires before it looks at the shape).
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
            // The space, one variable per bound; the tallies' relations to `banked` are the
            // conservation laws above.
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

/// Cursor-owned pixels and cells live in the active viewport's coordinates. Entering retained
/// history suppresses the DEC cursor, both trail engines and every cursor body and companion
/// overlay, and a later Retain projection keeps them dark. The hidden pet brain still gets a
/// tick and can settle its scheduler: presentation is suppressed, not lifecycle progress.
///
/// `Buggy=1` keeps trail geometry and companions projected over unrelated history and withholds
/// the hidden brain's tick (`HiddenPetLifecycleProgresses`); an unticked brain never settles, so
/// `SettleHistoryBrain` cannot fire and the stuck scheduler is recorded where the tick was
/// withheld (`HiddenSchedulerNeverSticks`).
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

/// Resident cursor-pet coordinates are owned by one front terminal surface. A pane or tab owner
/// change, a main/alternate-screen change, or a focus loss that leaves cursor effects truly
/// unpresentable retires both the visible body and its host-owned hit target, while the pet's
/// durable identity survives for the next sighting. Raw focus loss is not a retirement proof: a
/// live typed-wake or recording pin keeps the surface admitted. What the next frame draws is
/// `PetSurfaceSeam`'s law (an owner switch crossfades from a non-interactive ghost of the old
/// body, at the pixels it occupied).
///
/// Only the resident `PetBrain` body and the GUI's paired hit target are projected. `Buggy = 1`
/// keeps the old body and hit target across retiring boundaries (so they jump into a
/// replacement pane or stay interactable), and also over-repairs the pinned blurs: typed-wake
/// and recording focus losses rebuild the brain (`PetBrain::default()`) where
/// `PetBrain::retire_coordinate_space` would carry the identity. That hides a body the pin still
/// presents (`PresentationPinsPreserveTheSighting`) and drops the identity
/// (`DurableIdentitySurvives`): `prepare_resident_pet_tick` re-applies the species from config,
/// but nothing restores the worn look, contentment, disposition and lifetime clocks. The Tier-1
/// projection reads the species at the blur itself, and the negative control replays the
/// rebuild on the real window.
///
/// The ordinary flying cursor cat has its own earned/promise lifecycle; its owner-switch
/// retirement is a separate host rule in the Tier-1 GUI regression.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_companion_owner_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        CursorCompanionOwnerLifecycle {
            const Buggy = 0;
            var phase = 0;
            // A configured durable identity, no surface-relative sighting yet.
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

/// A composed frame owns one DEC-2026 hold machine per visible terminal. One pane completing its
/// bracket cannot release a sibling whose bracket is still open; only a frame where every member
/// releases may present. `Buggy = 1` is an aggregate counter: the first pane close presents the
/// composite while the other pane is held.
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

/// A DEC-2026 close edge licenses the last completed episode for presentation. If the terminal
/// at once opens a new episode, that completed boundary may stay visible while the new episode
/// is clean; the first completed parser action in the reopened episode sets `open_dirty`, and
/// from then the host holds the partial episode until its matching close.
///
/// `Buggy = 1` picks one of two wrong hold rules per trace (`PickLevelHold` sets `level_fault`,
/// pinned to zero at `Buggy = 0`): the close sequence alone as a presentation license, which keeps
/// presenting after the reopened episode turns dirty (`DirtyReopenHoldsUntilClose`), or a
/// level-sampled hold, which reads a close+reopen as one endless episode, never presents the
/// completed boundary and pins a flooding TUI's presents to the sync timeout
/// (`CleanReopenMayPresentCompletedBoundary`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn sync_reopen_visibility_model() -> Model {
    aterm_spec::ty_model! {
        SyncReopenVisibility {
            const Buggy = 0;
            // Begin inside a dirty first episode, just before the close/reopen edge.
            var phase = 0;
            var sync_active = 1;
            var open_dirty = 1;
            var hold = 1;
            var completed_generation = 0;
            var presented_generation = 0;
            var partial_visible = 0;
            // Which hold rule a `Buggy = 1` trace carries: 0 the close-sequence
            // license, 1 the level-sampled hold. Always 0 at `Buggy = 0`.
            var level_fault = 0;

            // A stutter at `Buggy = 0`, so no action is dead at the committed config.
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
                // The new episode has made no presented mutation, so the completed close
                // boundary is a lawful frame; the level hold holds and presents nothing.
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

/// Family-wide lifecycle for PTY scroll translation. `survivor_y` abstracts every
/// position-bearing member whose translated anchor stays visible; `off_top_alive` every member
/// that crosses the top. Tier-1 seeds every CursorGlow pool (tail, wake, glide landing, an
/// outgoing fade) and every CursorTrail position field, asserts their exact deltas, and projects
/// one survivor and one retired member from each engine onto this transition.
///
/// `stale_proof_alive` is an input proof whose row identity the same scroll fence invalidates
/// (the plain-Backspace `(row, fill)` baseline and its one-shot poof hint). `Buggy=1` leaves live
/// geometry and stale provenance in their pre-scroll state; the invariants separately catch a
/// detached segment, an off-top segment, and a proof reused against other content at the same
/// row.
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

/// Host-side scroll observation at a retained-history cap. The terminal owns two cumulative,
/// non-consuming clocks: `uniform_rows` for composable full-screen upward motion on either
/// screen and `epoch` for region, reset and restore mutations whose coordinates cannot be
/// transformed as one plane. `retained_history` stays zero throughout, modelling both a
/// zero-history terminal and a saturated ring whose public count no longer changes.
///
/// `UniformAtCap` and `AltUniform` reach one exact translation and retire row-bound proof
/// (changing active screen invalidates on its own, but an in-screen full-grid shift is not
/// ambiguous). Region, reset and in-place restore events invalidate and drop all geometry and
/// proof. `Buggy=1` infers motion from the unchanged retained count and keeps the stranded state.
///
/// `RegionBandMove` (event 6) is the third answer, for the inline viewports Codex and others
/// stream through: the epoch still steps (`band_batches` counts it as a subset, so a consumer
/// that cannot translate bands still discards), but the batch's whole row motion is one exact
/// `RowBandMove`, so the host translates the band's light down by `Delta` (a region scroll
/// down, the reverse index a streamed transcript line writes) and keeps its geometry. Row-bound
/// proof still dies, since the band's rows hold different content. `Buggy=1` invalidates
/// (`decision = 2`) and destroys every mark, so the rainbow disappears on each streamed line.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_scroll_signal_model() -> Model {
    aterm_spec::ty_model! {
        CursorScrollSignal {
            const StartY = 3;
            const Delta = 2;
            const Buggy = 0;
            var event = 0;              // 0 initial, 1 primary uniform, 2 region, 3 alt uniform, 4 reset, 5 restore, 6 explained band
            var retained_history = 0;   // capped: deliberately unchanged
            var uniform_rows = 0;       // cumulative composable scroll clock
            var epoch = 0;              // cumulative invalidation clock
            var band_batches = 0;       // epoch steps whose whole row motion is an exact band
            var decision = 0;           // 0 none, 1 translate, 2 invalidate, 3 translate the band
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

/// The landing pool: the rainbow kitty's bounded FIFO of landings
/// (`rainbow_kitty::meteor::Meteors::landings`, `LANDING_POOL` = 3).
///
/// Every arrival is admitted, from either site: a meteor's arrival edge in `Meteors::emit` and
/// a ringing bar of the sing-along party in `Meteors::party`. At capacity the oldest landing is
/// evicted, so the pool holds exactly the latest arrivals in order and the jump the user just
/// made always lands. Every landing lives the same 600 ms (`Landing::end`; the spark life and the
/// ring's ceiling are both `FLIGHT_OFF_GLASS_MS`), so expiry takes the oldest first;
/// `Meteors::reset` empties the pool. `Buggy=1` drops the arrival when saturated
/// (`if len < LANDING_POOL { push }`), so the newest jump lands with no impact.
///
/// Tier-1: `rainbow_kitty::meteor::tests::the_landing_pool_conforms_to_the_model` walks the whole
/// reachable space on a real `Meteors` (flights through `emit`, ring bars through `party`, expiry
/// through `emit`'s retain, real `reset`), projecting the pool's length and its first and last
/// arrival ordinals, and rejects the drop-the-arrival mutant at each admission site.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rainbow_landing_pool_model() -> Model {
    aterm_spec::ty_model! {
        RainbowLandingPool {
            const Cap = 3;
            const MaxIssued = 5;
            const Buggy = 0;
            // Landings in the pool.
            var resident = 0;
            // Arrival ordinals (1-based) of the newest and the oldest resident,
            // 0 while the pool is empty.
            var newest = 0;
            var oldest = 0;
            // Arrivals so far.
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

            // The pool holds exactly the latest `resident` arrivals.
            invariant HoldsTheLatestArrivals:
                if resident > 0 {
                    newest == issued && oldest + resident == issued + 1
                } else {
                    newest == 0 && oldest == 0
                };
            // `newest`/`oldest` need no bound: `HoldsTheLatestArrivals` pins both to `issued`.
            bound StateBounds: resident <= Cap && issued <= MaxIssued;
        }
    }
}

// Error-class models: information flow, ordering and reply fidelity, each a property class a
// future regression of the same shape fails.

/// Capability secrecy (information flow): a bearer-token secret in an inheritable env sink
/// reaches a sandboxed same-uid peer (one that cannot read 0600 files); one routed through a
/// 0600 file does not. `published` is the channel (0 none, 1 file, 2 env); `peer_has` whether the
/// sandboxed peer obtained the secret. `Buggy = 1` chooses the env channel.
/// `NoSecretToSandboxedPeer`: the sandboxed peer never holds the token.
///
/// Tier-1: aterm-gui's `spawn::capability_secrecy_conformance` drives
/// `spawn::provision_child_proxy` with a private socket dir and with none, reads `published` off
/// the env it produced against the secrets minted for the child, and validates the real
/// `Provision` (the file channel exactly when there is a private dir; with none, or a file it
/// cannot write, provisioning fails closed). `SandboxedRead` follows from the channel by
/// definition, so the channel is what is bound.
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::model]
pub fn capability_secrecy_model() -> Model {
    props::two_stage_leak(props::TwoStage {
        name: "CapabilitySecrecy",
        // Provision over the 0600-file channel (1) or, when Buggy, the inheritable
        // env channel (2).
        stage: "published",
        stage_act: "Provision",
        stage_rhs: if_(eq(cst("Buggy"), int(1)), int(2), int(1)),
        // A sandboxed peer obtains the secret only from the env channel (2).
        leak: "peer_has",
        leak_act: "SandboxedRead",
        leak_guard: gt(var("published"), int(0)),
        leak_rhs: if_(eq(var("published"), int(2)), int(1), int(0)),
        inv: "NoSecretToSandboxedPeer",
        inv_expr: eq(var("peer_has"), int(0)),
    })
}

/// Publish ordering: a discovery entry is published only after its socket is bound, so a
/// concurrent stale sweep never sees an entry naming an unbound socket and deletes it.
/// `Buggy = 1` publishes before binding. `PublishImpliesBound`: `published ⟹ bound`.
///
/// The shipping order is structural: aterm-gui's `control::publish_discovery` consumes the
/// `BoundControl` only the bind produces, names the socket it was bound at, and hands back the
/// listener `control::spawn` serves on, so publishing first, naming another socket, or serving
/// without publishing do not compile. Tier-1: `publish_ordering_conformance` projects each real
/// entry (on disk? does its socket answer a dial?) through the real bind, publication,
/// register-seam publish and stale sweep; the pre-bind write is the step it rejects.
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
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

/// Reply fidelity: once a forwarded verb has been delivered to the host, a later relay-stage
/// failure does not report `ERR` to the client (a false "didn't happen" for an op that did). A
/// failure before delivery (the connect: nothing listening, or no token beside the host's socket
/// once the connection exists, answered `ERR no such session`; the connect probe; the handshake
/// write, `ERR forward`) is reported as `DialFail`. `Buggy = 1` reports a relay failure after
/// delivery. `NoErrorAfterDelivery`: never both delivered and error-reported.
///
/// Tier-1: aterm-gui's `reply_fidelity_conformance` drives `proxy::connect_hop` and
/// `proxy::deliver_then_relay` against a real host socket (the real relay, a relay that fails
/// after delivery, a dial with no listener, a host that never accepts, a host with no token),
/// projecting `delivered` from what the host read and `reported_err` from the results the caller
/// turns into an error reply.
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
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

/// A flickering program never takes the cursor: the anti-flap law of the cursor companion's
/// tenure gate, which keeps the cat from changing identity every time a command block opens
/// and closes.
///
/// `KittyTenure::observe` is a dwell gate: a claim that differs from the one on glass becomes a
/// candidate and lands only once it has held for `dwell_for` (`TENURE` to arrive, `RELEASE` to
/// fall back). The load-bearing arm is `_`: an observation that does not match the standing
/// candidate restarts it at `now`, so an alternating claim never accumulates dwell.
///
/// This is the flicker scenario only: `last` forces the two observations to alternate, and the
/// invariant is that the worn identity never changes. `Buggy = 1` lands the raw claim at once.
/// The steady case (a program that holds the pane lands after `Tenure`, changing the identity
/// once) is left out: covering both would need a rate invariant, which asserts less than either
/// count. `aterm_effects::companion_look`'s own tests drive the real gate across `TENURE`.
#[must_use]
// Skip (T2 vcgen-budget lane): a model data constructor; `ty` checks the model it returns.
#[cfg_attr(trust_verify, trust::skip)]
pub fn companion_tenure_flicker_model() -> Model {
    aterm_spec::ty_model! {
        CompanionTenureFlicker {
            const Buggy = 0;    // 1 = land the raw claim at once, no dwell
            const Tenure = 2;   // observations a candidate must hold to land
            var worn = 0;       // 0 = the base cat, 1 = the program cat
            var cand = 2;       // 2 = no candidate, 0 = base pending, 1 = program pending
            var dwell = 0;      // observations the standing candidate has held
            var changes = 0;    // times the identity on glass moved
            var last = 0;       // 0 = the last observation was `none`, 1 = a program
            var steps = 0;      // run bound

            // The pane reports a program executing; `last` forces alternation, so this is
            // the flicker of a command block opening, never a steady claim.
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

            // And the block closes again. Same gate, the other claim.
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

            // Across a whole flicker storm, the cat does not move.
            invariant FlickerNeverLands: changes == 0;
        }
    }
}
