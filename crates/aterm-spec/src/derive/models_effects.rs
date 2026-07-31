// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Sparkle / ignition / cursor-cat / nyan effect models, plus the generalized error-class trio — spec-model
//! data constructors moved verbatim out of the one-file catalog in `derive.rs`
//! (pure code motion; every constructor keeps its `crate::derive` path via the
//! `pub use` re-exports there).

use super::*;

/// SPARKLE-WORDS v2 identity/persistence episodes (docs/sparkle-words-v2-design.md
/// §3.6/§9), authored via [`ty_model!`] exactly as the design prints it. One word
/// identity's episode lifecycle over the GUI's grace-TTL persist map:
///
///   * `Appear` — a persist MISS births an episode: the genome rolls exactly
///     once (`rolls`), the per-episode nova re-arms (`nova = 0`). Guarded by
///     `MaxBirths` — the §9 finiteness rule (vars carry no declared bounds, so
///     the re-enterable Absent→Live→Grace→Absent cycle must be guard-bounded
///     for explicit-state BFS to terminate; the design's §15.14e correction).
///   * `Vanish`/`Rehit` — occlusion enters grace; a re-hit within grace
///     CONTINUES the episode. At `Buggy = 1` the re-hit re-rolls the genome —
///     exactly v1's one-epoch `prev_appeared` amnesia (flaw B-3).
///   * `Rekey` — a same logical occurrence moved by a terminal redraw changes
///     its position key without changing its genome, spent-nova bit, done mark,
///     or fire count. `Buggy = 1` models treating the move as a fresh identity:
///     it re-rolls and resets the spent guards, admitting a second ignition.
///   * `ContextMove` — the stronger redraw-recognition obligation: the same
///     logical occurrence moves while its row-local neighbor/status context
///     changes. Healthy recognition still transfers the episode; `Buggy = 1`
///     models rejecting the changed fingerprint, allocating a false fresh
///     birth, and re-arming the spent occurrence.
///   * `Tick`/`Expire` — grace ages out; only true expiry re-rolls.
///   * `Ignite` — one nova per episode; the `Buggy` threshold form admits a
///     re-ignition (the print-erase-print strobe the grace map exists to stop).
///
/// Invariants, proven at `Buggy = 0` (exhaustive) and caught at `Buggy = 1`
/// (violates `GenomeFrozen` on re-hit/rekey, `RecognitionComplete` /
/// `NoFalseBirths` on a context-changing move, and admits a second fire):
/// `GenomeFrozen: rolls = births`, `OneNovaPerEpisode: nova <= 1`,
/// `PlayedOnce: fires <= 1`, `AgeBounded: age <= GraceMax`.
///
/// Tier-0: `derived_sparkle_identity_proves_and_catches_amnesia_and_rekey`
/// (aterm-spec/tests/derived_ring_ty.rs). Tier-1 lives beside the shipping map
/// in `aterm-effects/src/word_decorations.rs`:
/// `sparkle_identity_conformance_real_persist_map` drives the REAL
/// `WordDecorations` persist map with a fake clock and validates the projected
/// `(state, rolls, nova)` trace, with grace-less and missing-rekey negative
/// controls. The public-API redraw regression is
/// `aterm-effects/tests/word_reflow_identity.rs`.
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn sparkle_identity_model() -> Model {
    crate::ty_model! {
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
            var done = 0;     // v3: the identity's done mark — SURVIVES Expire
            var fires = 0;    // v3: total ignitions across every episode generation
            var rekeys = 0;   // logical occurrence position changes
            var context = 0;  // bounded row-local context version
            var context_moves = 0;      // logical moves whose local context changed
            var recognized_moves = 0;   // those moves transferred as the same episode
            var false_births = 0;       // context moves misclassified as fresh
            action Appear when (state == 0 && births <= MaxBirths - 1) {
                // v3: an Appear from the done set enters BORN-DONE (done = 1
                // holds; the Ignite guard below stays disabled forever). The
                // genome still rolls (born-done words show settled ink, which
                // needs the genome), so GenomeFrozen is unchanged.
                state = 1; births = births + 1; rolls = rolls + 1; nova = 0;
            }
            action Vanish when (state == 1) { state = 2; age = 0; }
            action Rehit when (state == 2) {
                state = 1; age = 0;
                rolls = if Buggy == 1 { rolls + 1 } else { rolls };  // buggy: re-roll on re-hit
            }
            action Rekey when (state == 1 && rekeys <= MaxRekeys - 1) {
                rekeys = rekeys + 1;
                // Healthy: only the map key changes (all modeled lifecycle
                // fields are UNCHANGED). Buggy: a fresh-identity replacement
                // re-rolls and forgets both spent guards, so Ignite can fire
                // again and PlayedOnce catches the second event.
                rolls = if Buggy == 1 { rolls + 1 } else { rolls };
                nova = if Buggy == 1 { 0 } else { nova };
                done = if Buggy == 1 { 0 } else { done };
            }
            action ContextMove when (state == 1
                && context_moves <= MaxContextMoves - 1
                && rekeys <= MaxRekeys - 1
                && births <= MaxBirths - 1)
            {
                // The surface/logical occurrence is unchanged, but row-local
                // neighbors changed while a same-width redraw moved it. The
                // healthy recognizer transfers the old episode despite that
                // context toggle. The buggy recognizer takes the real fresh
                // allocation path: births and rolls remain internally
                // consistent, so only the explicit recognition obligations
                // catch the classification error before the second Ignite.
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
            invariant GenomeFrozen: rolls == births;     // v1's one-epoch amnesia is the Buggy trace
            invariant OneNovaPerEpisode: nova <= 1;
            invariant AgeBounded: age <= GraceMax;
            invariant RekeysBounded: rekeys <= MaxRekeys;
            invariant ContextBounded: context <= 1;
            invariant ContextMovesBounded: context_moves <= MaxContextMoves;
            invariant RecognitionComplete: recognized_moves == context_moves;
            invariant NoFalseBirths: false_births == 0;
            // v3 §1.3 PlayedOnce: no second Ignite/entrance per done-marked
            // identity — the done flag survives Expire, so a re-born episode
            // can never re-fire (Buggy = 1 re-admits and violates this too).
            invariant PlayedOnce: fires <= 1;
        }
    }
}

/// Bounded repeated-surface policy, the group complement to
/// [`sparkle_identity_model`]'s single-occurrence `ContextMove`. This is NOT a
/// general same-form oracle: each action names one licensed matcher premise for
/// spent episodes in one collision-free exact-`FormId` group:
///
/// * `MovePair`: an immediate, same-width GLOBAL redraw moves both old
///   occurrences within the bounded linear window and leaves no stationary
///   same-seed anchor. Both are recognizable; 2 → 2 means two transfers and no
///   birth/arm. The buggy exact-context-only classifier misses both.
/// * `GrowOne`: the same global-redraw premises, but 2 → 3 contains one
///   logically new occurrence. Two transfer and exactly one is fresh/armed;
///   the buggy context gate births all three.
/// * `RotatePair`: a recent log-tail rotation has a stationary same-seed
///   survivor. That survivor anchors the group: one old episode transfers and
///   the new bottom occurrence is genuinely fresh/armed. The buggy
///   cardinality-only classifier falsely transfers the departed twin too.
/// * `BlankGrace`: after MORE than two BLANK occlusion scans, the exact seed
///   and exact context return untainted. Blank grace is still the same logical
///   episode and must transfer despite exceeding the weak redraw window. The
///   buggy blanket recency gate creates a false fresh/armed birth.
/// * `TypedRetype` / `RecentTypedRetype`: for the feline class only, after
///   NONBLANK incremental replacement, the same form returns at the SAME
///   position-bearing seed and exact context, but continuity is tainted. Both
///   the >2-scan case and a coalesced one-partial case inside the weak window
///   are logically new and must be one fresh/armed birth. The buggy
///   exact-evidence path ignores taint and steals the spent episode (the
///   “kitty does not activate after more typing” witness). Profanity does not
///   use these actions: it conservatively transfers an exact surface for the
///   full grace lifetime so unrelated `fix` composer churn cannot relight it.
///
/// `expected_fresh`/`logical_new` encode the scenario truth independently of
/// the classifier outputs. `RecognitionComplete` catches missed licensed
/// transfers, while `NoFalseTransfers` catches identity theft outside those
/// premises; neither direction is inferred from raw 2 → 2 cardinality alone.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn sparkle_reflow_cardinality_model() -> Model {
    crate::ty_model! {
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
                recent = 1;
                seq_gap = 1;
                new_count = 2;
                logical_new = 0;
                expected_fresh = 0;
                expected_recognized = OldCount;
                transferred = if Buggy == 1 { 0 } else { OldCount };
                recognized = if Buggy == 1 { 0 } else { OldCount };
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
                transferred = if Buggy == 1 { 0 } else { OldCount };
                recognized = if Buggy == 1 { 0 } else { OldCount };
                fresh = if Buggy == 1 { 3 } else { 1 };
                armed = if Buggy == 1 { 3 } else { 1 };
                false_births = if Buggy == 1 { 2 } else { 0 };
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
                recent = 0;
                seq_gap = 3;
                stale_same_seed = 1;
                exact_context = 1;
                continuity_tainted = 0;
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
                typed_retype = 1;
                recent_typed_retype = 1;
                recent = 1;
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
            invariant SeqGapBounded: seq_gap <= WeakMaxScans + 1;
            invariant ExpectedPartition:
                expected_recognized + logical_new == new_count;
            invariant ExpectedFreshIsLogicalNew: expected_fresh == logical_new;
            invariant TransferAccounting:
                recognized + false_transfers == transferred;
            invariant FreshMatchesExpected: fresh == expected_fresh;
            invariant ArmedMatchesExpected: armed == expected_fresh;
            invariant ArmedMatchesFresh: armed == fresh;
            // Directional aliases retained for the Tier-1 fresh-birth fault
            // injection: the equality above additionally catches UNDER-birth
            // caused by a false transfer in RotatePair/TypedRetype.
            invariant FreshAtMostNetGrowth: fresh <= expected_fresh;
            invariant ArmedAtMostNetGrowth: armed <= expected_fresh;
            invariant RecognitionComplete: recognized == expected_recognized;
            invariant NoFalseBirths: false_births == 0;
            invariant NoFalseTransfers: false_transfers == 0;
            invariant StateBounded: state <= 1;
            invariant NewCountBounded: new_count <= 3;
        }
    }
}

/// Repeated explicit feline retyping versus session done-mark poisoning.
///
/// Each completed, continuity-tainted `kitty` token is a new intentional
/// episode and must arm once. The healthy transition closes the replaced
/// episode without installing/inheriting a redraw done mark. `Buggy = 1`
/// reproduces the former ordering: the first retype arms but its outgoing old
/// episode writes a mark, so the second retype is born-done and inert.
///
/// Tier-1 binding:
/// `word_decorations::tests::two_consecutive_recent_kitty_retypes_both_arm`
/// drives two real partial→complete cycles at the same identity key and checks
/// the real birth/armed counters after each one.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn sparkle_retype_rearm_model() -> Model {
    crate::ty_model! {
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
            invariant RetypesBounded: retypes <= MaxRetypes;
            invariant ArmedBounded: armed <= MaxRetypes;
        }
    }
}

/// Capacity-boundary transaction for the sparkle identity persist map.
///
/// The shipping alignment pass temporarily pulls one unmatched old episode
/// from a full map, admits a freshly observed replacement, then offers the old
/// episode back for grace. The healthy implementation treats the final offer
/// as an LRU union and departs one episode, so cardinality remains at `Cap`.
/// `Buggy = 1` reproduces the former raw grace `insert`: the old episode is
/// reinserted after the fresh slot refills, reaching `Cap + 1`.
///
/// Tier-1 binding:
/// `word_decorations::tests::persist_cap_drops_unmatched_grace_after_fresh_move`
/// drives this exact transaction through the real scanner/persist map, proves
/// the fresh claimant stayed resident, and checks that the departed one-shot's
/// done mark was written.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn sparkle_persist_capacity_model() -> Model {
    crate::ty_model! {
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
                departed = departed + (if Buggy == 1 { 0 } else { 1 });
                phase = 3;
            }
            invariant ResidentBounded: resident <= Cap;
            invariant Conservation:
                resident + pulled + departed == Cap + admitted;
            invariant PulledBounded: pulled <= 1;
            invariant PhaseBounded: phase <= 3;
        }
    }
}

/// SPARKLE-WORDS v2 supernova phase machine (docs/sparkle-words-v2-design.md
/// §6.1/§9), authored via [`ty_model!`] per the design's NovaPhase sketch:
/// `{0 Armed, 1 Dip, 2 Flash, 3 Ring, 4 Debris, 5 Ember, 6 Settled}` with a
/// single monotone `Step` (guard-bounded by `MaxSteps` — the §9 finiteness
/// rule) that counts a flash on ENTERING Flash (pre-state phase == 1; updates
/// are simultaneous), and a `Rearm` (bounded by `MaxArms`) that resets BOTH
/// `phase = 0` and `flashes = 0`. Both resets are load-bearing (§9): a Rearm
/// that reset only `phase` would break `OneFlashPerArm` at `Buggy = 0` on the
/// second cycle — `flashes` is per-arm by construction; the per-EPISODE
/// one-nova property lives in [`sparkle_identity_model`]'s `nova`.
///
/// At `Buggy = 1`, `Rearm` re-enters **Flash directly** (the §9 negative
/// control: a re-arm that skips the machine and re-flashes) — `ty` catches it
/// on `OneFlashPerArm`. Invariants: `Monotone: phase <= 6`,
/// `OneFlashPerArm: flashes <= 1`, and the §9 self-termination property as a
/// FUEL invariant — `CanSettle: steps + 6 <= MaxSteps + phase`, i.e. the
/// remaining step budget always covers the `6 − phase` steps still needed to
/// reach Settled, so no reachable arm is ever stranded mid-walk by the
/// finiteness guard (which is why `MaxSteps = 18 = 6 steps × (1 + MaxArms)
/// walks`: a 12-step budget would strand the third arm at `steps = 12,
/// phase = 0` and fail this very invariant at `Buggy = 0`).
///
/// Tier-1 binding: aterm-gui's nova battery drives the real host tick through
/// a full window (`nova_one_flash_per_episode_across_occlusion` — Dip emits
/// nothing, Flash crowns, Ring quads, Settled emits nothing and re-arms only
/// on true episode death) against `nova::phase`, the pure phase function.
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn nova_phase_model() -> Model {
    crate::ty_model! {
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
            action Rearm when (phase == 6 && arms <= MaxArms - 1) {
                arms = arms + 1;
                phase = if Buggy == 1 { 2 } else { 0 };
                flashes = if Buggy == 1 { flashes + 1 } else { 0 };
            }
            invariant Monotone: phase <= 6;
            invariant OneFlashPerArm: flashes <= 1;
            // §9 self-termination, as a safety (fuel) invariant: the unspent
            // step budget MaxSteps - steps always covers the 6 - phase steps
            // left to Settled, so the guard bound never strands a walk.
            invariant CanSettle: steps + 6 <= MaxSteps + phase;
        }
    }
}

/// SPARKLE-WORDS v2 flash limiter (docs/sparkle-words-v2-design.md §6.4/§9):
/// WCAG 2.3.1 as a machine-checked property, authored via [`ty_model!`]
/// exactly as the design prints it. Time is discretized to 250 ms ticks; the
/// rolling second is a 4-slot ring of per-tick ignition counts (no modulo —
/// the grammar has none; `Shift`'s simultaneous updates ARE a correct
/// rotation). `Overlap` is a scenario constant (two candidates' regions
/// overlap, §6.4 item 2); `2·x` is written `x + x` (no `*`).
///
/// Invariants: `IgnitionBound` — ≤ 2 ignitions per rolling second, tightening
/// to ≤ 1 under overlap; `RegionFlashPairs` — the REGION-scoped flash-pair
/// count (2 pairs/ignition co-charged to a region only when regions overlap)
/// stays ≤ 3. Deliberately NOT window-global: two disjoint ignitions expose
/// their 2 pairs to DIFFERENT regions — a window-global "pairs ≤ 3" is
/// arithmetically inconsistent with "ignitions ≤ 2" at 2 pairs/ignition
/// (4 > 3) and fails `ty` at `Buggy = 0` (the §9/§15.14 binding-spec erratum,
/// corrected in the open: WCAG counts flashes per region).
///
/// `Buggy = 1` removes the overlap tightening — two overlapping ignitions in
/// one rolling second put 4 transition pairs on the shared region — so the
/// counterexample needs BOTH `Buggy = 1` and `Overlap = 1` (the Tier-0 test
/// drives `to_cfg_with` accordingly; a plain `Buggy = 1, Overlap = 0` run
/// stays green, which is exactly the point: disjoint novas at 2/s are legal).
///
/// Tier-1 binding (the §7.5 ledger's "FlashLimiter" row — the limiter IS the
/// WCAG argument, so no per-frame flash audit ships): aterm-gui's
/// `flash_limiter_conformance_real_limiter_projects_onto_model` drives the
/// REAL `grant_ignition` queue through scripted disjoint + overlapping
/// ignition storms and projects every decision onto THIS model (grants ↔
/// `Ignite` admitted, delays ↔ `Ignite` disabled until the matching `Shift`
/// tick), with `…negative_control_overlap_blind_limiter_is_buggy_trace` as
/// the non-vacuity twin (an overlap-blind limiter reproduces the
/// `Buggy = 1, Overlap = 1` counterexample); `limiter_delays_third_of_three_
/// ignitions…` and `limiter_tightens_to_one_per_second_on_overlap` drive the
/// same limiter end-to-end through the full host tick.
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn flash_limiter_model() -> Model {
    crate::ty_model! {
        FlashLimiter {
            const Buggy = 0;
            const Overlap = 0;   // scenario: two candidates' regions overlap (§6.4)
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
            // REGION-scoped flash pairs (2 per ignition on a shared region) stay <= 3.
            invariant RegionFlashPairs:
                (if Overlap == 1 { (s0 + s1 + s2 + s3) + (s0 + s1 + s2 + s3) }
                 else { 2 }) <= 3;
        }
    }
}

/// SPARKLE-WORDS v2 flash limiter, WINDOW-WIDE: the same WCAG 2.3.1 argument
/// as [`flash_limiter_model`], but with the ENFORCER COUNT as a dial.
///
/// [`flash_limiter_model`] is the correct theorem about ONE limiter, and it
/// stays true no matter how many limiters exist — which is exactly why it
/// cannot see the defect that prompted this model. Give every split pane its
/// own `WordDecorations`, hence its own reservation vec, hence its own
/// limiter, and each limiter independently satisfies `IgnitionBound` while a
/// photosensitive viewer — who has ONE retina, not one per pane — sees 2N
/// flashes per second.
///
/// So this model carries TWO structural enforcers (`a0..a3`, `b0..b3`: the
/// same 4-slot rolling second each), one SHARED `Shift` clock (they are on one
/// wall clock and one retina), and two dials:
///
/// * `Instances` — how many enforcers are live. `Spawn` admits the second one
///   only when `Instances > 1`, so `Instances = 1` collapses this model onto
///   the single-limiter one.
/// * `Local` — WHAT each enforcer consults. `Local = 0` (committed) is the
///   shipping shape: one limiter whose guard sees every reservation.
///   `Local = 1` is the per-pane refactor: each enforcer admits against ITS
///   OWN slots and is blind to the other's.
///
/// `WindowIgnitionBound` is charged against the SUM, because the retina is.
/// At `Local = 1, Instances = 2` two individually-correct limiters overshoot
/// it at EVERY scenario corner — no geometric coincidence needed, unlike the
/// `Buggy = 1` overlap defect, which needs `Overlap = 1` as well. That
/// asymmetry is the point: multiplication alone is the defect.
///
/// This model does NOT catch the refactor — nothing in a state machine can
/// know how many engines the host builds. It machine-checks the SENTENCE the
/// scope-cardinality census (`aterm-census` OB-13..OB-18, claim
/// `flash-limiter`) would otherwise assert in prose, and the two interlock:
/// OB-16 fails the build if this model is deleted, and
/// `prove_catch_and_multiply_scalar` fails the test if it goes toothless.
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor — same class as
// `flash_limiter_model`; the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn flash_limiter_window_model() -> Model {
    crate::ty_model! {
        FlashLimiterWindow {
            const Buggy = 0;
            const Overlap = 0;    // scenario: two candidates' regions overlap (§6.4)
            const Local = 0;      // 1 = each enforcer only sees its OWN slots
            const Instances = 2;  // live enforcers (2 = one per pane, the refactor)
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

            // The bound the RETINA experiences: charged against every live
            // enforcer's slots at once, not against each enforcer's own.
            invariant WindowIgnitionBound:
                a0 + a1 + a2 + a3 + b0 + b1 + b2 + b3
                    <= if Overlap == 1 { 1 } else { 2 };
        }
    }
}

/// Ownership/cardinality lifecycle for delayed sparkle-word ignition slots.
///
/// A future reservation is one-to-one with a live persist episode. Once its
/// owner departs, the reservation is cancelled because no flash occurred. A
/// slot that already fired moves into the rolling safety history and survives
/// owner departure until its one-second window expires. Thus at most `Cap`
/// owner-backed future slots plus `RecentCap` fired slots are resident.
/// `Buggy = 1` reproduces the former expiry-only sweep: cancelling an owner
/// leaves its future slot behind, so repeated churn creates ownerless pending
/// work and eventually exceeds the pending bound.
///
/// Tier-1 binding: `ignition_reservation_lifecycle_real_queue_conforms` drives
/// the real `grant_ignition`/`prune_ignitions` queue through immediate grants,
/// delayed grants, future-owner departure, fired-owner departure, and history
/// expiry, with an expiry-only negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ignition_reservation_lifecycle_model() -> Model {
    crate::ty_model! {
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
            action ReserveFuture when (live <= Cap - 1 && pending <= Cap) {
                live = live + 1;
                pending = pending + 1;
            }
            action CancelFuture when (live > 0 && pending > 0) {
                live = live - 1;
                pending = if Buggy == 1 { pending } else { pending - 1 };
            }
            action FirePending when (pending > 0 && recent <= RecentCap - 1) {
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

/// Atomic ownership transfer for a delayed ignition across episode alignment.
///
/// Alignment is allowed to replace an occurrence identity while preserving the
/// episode's frozen `nova_start`. The corresponding future limiter slot must
/// move in the same operation: otherwise pruning sees an ownerless reservation,
/// drops it, and a competing overlapping request can claim the same rolling
/// window even though the rekeyed episode will still flash. `Buggy = 1` models
/// precisely that missing owner rewrite.
///
/// Tier-1 binding: `ignition_reservation_rekey_real_queue_conforms` drives the
/// real rekey helper and limiter through GrantDelayed -> Rekey -> Prune ->
/// CompetingGrant, then repeats the trace without the owner transfer as a
/// negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn ignition_reservation_rekey_model() -> Model {
    crate::ty_model! {
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
            invariant PhaseBounded: phase <= 4;
        }
    }
}

/// Bounded constant-selection LRU for completed sparkle-word episodes.
///
/// The healthy implementation addresses the oldest slot directly through an
/// intrusive head link, so replacement at capacity performs one oldest-slot
/// selection independent of `resident`. `Buggy = 1` models the retired
/// `HashMap::iter().min_by_key(...)` path: selection probes every resident,
/// violating `ConstantSelection` as soon as the bounded map has more than one
/// entry. Cardinality remains bounded in both variants, making the performance
/// counterexample non-vacuous rather than conflating it with overflow.
///
/// Tier-1 binding: `done_mark_lru_real_order_and_constant_work_conform` drives
/// the shipping LRU at a small exact cap, checks deterministic touch/eviction
/// order and fixed link-write work, then demonstrates the legacy full scan
/// against the buggy model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn done_mark_lru_model() -> Model {
    crate::ty_model! {
        DoneMarkLruBound {
            const Buggy = 0;
            const Cap = 3;
            var resident = 0;
            var selections = 0;
            var evictions = 0;
            var touches = 0;

            action Insert when (resident <= Cap - 1) {
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
            invariant EvictionsBounded: evictions <= 1;
            invariant TouchesBounded: touches <= 1;
        }
    }
}

/// SPARKLE-WORDS v3 ONE-SHOT PEEK (docs/sparkle-words-v3-design.md §1.2/§1.3),
/// replacing the v2.2 `PeekCycle` bob model wholesale: a graphic plays exactly
/// once per word appearance — `Idle → Rise → Dwell → Descend → Done` — and
/// Done is ABSORBING per episode (zero quads forever, zero wakes: the duty
/// pin). The idle-event scheduler (bobs, blink deadlines) is retired; in-dwell
/// life is pure time inside the Dwell phase and never re-enters Rise.
///
/// `Buggy = 1` models exactly the replay class v3 §1.1 exists to kill: a
/// `Repeek` re-enters Rise after Done (ordinal churn / grace expiry / reset
/// re-birthing a finished episode) — `ty` PROVES `NoRepeek` (rises ≤ 1) at
/// `Buggy = 0` and CATCHES the counterexample at `Buggy = 1`. `CanFinish` is
/// the fuel/termination invariant (the step budget always covers the walk to
/// Done — no reachable state strands mid-cycle).
///
/// Tier-1 binding: aterm-effects' `one_shot_peek_conformance_real_engine`
/// drives the REAL engine across rescans, occlusion shorter and longer than
/// GRACE_TTL, twin growth/shrink/rotation, freeze/thaw mid-rise, and an
/// unfocused birth, projecting the emitted-quad phases onto this model.
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn one_shot_peek_model() -> Model {
    crate::ty_model! {
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
                // Buggy: a re-Rise after Done — the §1.1 replay classes
                // (ordinal churn, grace recount, reset) reborn as an entrance.
                phase = if Buggy == 1 { 1 } else { phase };
                rises = if Buggy == 1 { rises + 1 } else { rises };
                steps = steps + 1;
            }
            invariant NoRepeek: rises <= 1;   // Done is absorbing per episode
            invariant PhaseBounded: phase <= 4;
            // Fuel/termination: the unspent budget always covers the 4 - phase
            // steps left to Done, so the finiteness guard strands no walk.
            invariant CanFinish: steps + 4 <= MaxSteps + phase;
        }
    }
}

/// CURSOR-CAT host gate and collectible discovery lifecycle. Ordinary Nyan
/// momentum is owned by the cursor-trail master: input while that master is
/// off cannot arm or draw an ordinary flight, and turning it off retracts an
/// existing ordinary host arm. A collection hello is a separate bounded
/// promise and remains drawable with that master off.
///
/// A newly collected look gets a
/// guaranteed, bounded hello even when ordinary cursor momentum is cold:
/// `Hidden -> Discovery -> Fade -> Hidden`. `visible` is deliberately separate
/// from `forced`: the former is the draw obligation, while the latter is the
/// stronger promise that cursor style and momentum cannot dismiss the hello.
///
/// Time is abstracted into five PRESENTABLE host-frame samples: three
/// discovery samples followed by two fade samples. `HiddenTick` represents an
/// unfocused or host-suppressed sample. It may hide the current draw, but it
/// cannot consume `elapsed`, `presented`, or the forced hold. Once samples are
/// presentable again, `Tick` resumes the same bounded lifecycle and
/// `HiddenAtDeadline` pins completion to a fully quiescent state.
/// `LongPresentableGap` is the other licensed presentable-clock transition:
/// after the host has drawn at least one visible discovery frame, one delayed
/// sample beyond the complete hold-plus-fade deadline settles directly to
/// Hidden. It must not manufacture a late, fully opaque Fade frame and another
/// animation tail merely because no intermediate frame callbacks arrived.
///
/// At `Buggy = 1`, `TypeWhileTrailOff` reproduces the host leak by arming an
/// ordinary Nyan cat behind a disabled trail master. `HiddenTick` also consumes
/// the wall clock while `presented` stays
/// fixed. Enough hidden samples therefore walk Discovery through Fade to
/// Hidden without delivering the promised hold. The same switch makes
/// `LongPresentableGap` strand an expired hello in a visible Fade. `ty` proves
/// the full contract at `Buggy = 0` and catches both defect witnesses at
/// `Buggy = 1`.
///
/// Tier-0: `derived_cursor_cat_proves_and_catches_hidden_expiry`
/// (aterm-spec/tests/derived_ring_ty.rs). Tier-1 drives the real
/// `aterm_effects::nyan_cursor::CursorCat` clock and projects its public frame
/// state plus host presentability at the same lifecycle boundaries.
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_cat_model() -> Model {
    crate::ty_model! {
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
            var trail_master = 0;   // host's ordinary Nyan owner gate
            var ordinary_armed = 0; // ordinary momentum owns host presentation
            var ordinary_visible = 0; // ordinary branch may be drawn
            action EnableTrail when (trail_master == 0) {
                trail_master = 1;
            }
            action DisableTrail when (trail_master == 1) {
                trail_master = 0;
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
                presented_once = 1; wall_expired = 0;
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
                visible = 0;
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
                // This is one presentable wall-clock observation beyond the
                // entire hold + fade, not a burst of synthetic frame ticks.
                // Healthy production consumes that elapsed tail atomically.
                wall_expired = 1;
                elapsed = TotalTicks;
                presented = presented + 1;
                presentable = 1;
                phase = if Buggy == 1 { 2 } else { 0 };
                visible = if Buggy == 1 { 1 } else { 0 };
                forced = 0;
            }
            // A collected hello remains in its protected Discovery phase until
            // the host has actually had HoldTicks presentable opportunities,
            // unless a later presentable observation is already beyond the
            // entire bounded lifecycle. HiddenTick cannot set wall_expired.
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
            invariant PresentedOnceBounded: presented_once <= 1;
            invariant WallExpiredBounded: wall_expired <= 1;
            invariant TimeBounded: elapsed <= TotalTicks;
            invariant PresentedBounded: presented <= TotalTicks;
            invariant HiddenBounded: hidden <= MaxHidden;
            invariant PhaseBounded: phase <= 2;
            invariant OrdinaryGateBounded:
                trail_master <= 1 && ordinary_armed <= 1 && ordinary_visible <= 1;
        }
    }
}

/// Cursor-cat reaction to complete profanity cues. Incomplete prefixes never
/// reach the visual reaction; each accepted complete token re-kicks the wince
/// and increases the bounded phrase chain through four distinct beats. A
/// hidden companion ignores the cue rather than being summoned by it.
///
/// `Buggy=1` reproduces the predictive-prefix defect by treating `fuc` as a
/// complete curse. Tier-1 drives the real `CursorCat::on_curse` seam and
/// projects its public reaction plus the accepted/rejected decision.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_cat_curse_wince_model() -> Model {
    crate::ty_model! {
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

/// FULL-NYAN held-key detector. A run becomes armed only on the sixteenth
/// distinct-in-time same-character press. Breaking an unarmed run clears its
/// partial count; releasing an armed run enters one bounded wind-down phase,
/// which then settles to the byte-identical idle state.
///
/// `Buggy=1` restores the original eight-press arm threshold. The
/// `ArmedRequiresCurrentThreshold` invariant therefore supplies a concrete
/// counterexample at the eighth press while the committed model proves that
/// every armed state has accumulated all sixteen presses.
///
/// Tier-0 lives in `derived_ring_ty.rs`; Tier-1 drives the genuine
/// `aterm_effects::nyan_sing::NyanSing` press, release, and settle methods in
/// `aterm-effects/tests/nyan_activation_conformance.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn nyan_sing_detector_model() -> Model {
    crate::ty_model! {
        NyanSingDetector {
            const Buggy = 0;
            const ArmRepeats = 16;
            const OldArmRepeats = 8;
            var phase = 0;      // 0 idle/accumulating, 1 armed, 2 winding down
            var count = 0;      // same-character presses in the current run
            var drive_live = 0; // full drive or its bounded release tail exists

            action Repeat when (phase == 0 && count <= ArmRepeats - 1) {
                count = count + 1;
                phase = if Buggy == 1 {
                    if count + 1 > OldArmRepeats - 1 { 1 } else { 0 }
                } else {
                    if count + 1 > ArmRepeats - 1 { 1 } else { 0 }
                };
                drive_live = if Buggy == 1 {
                    if count + 1 > OldArmRepeats - 1 { 1 } else { 0 }
                } else {
                    if count + 1 > ArmRepeats - 1 { 1 } else { 0 }
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
            invariant DriveMatchesLifecycle:
                if phase == 0 { drive_live == 0 } else { drive_live == 1 };
            invariant CountBounded: count <= ArmRepeats;
            invariant PhaseBounded: phase <= 2;
        }
    }
}

/// Cursor-cat FULL-NYAN bypass floor. Pinning the canonical momentum for a
/// held-key celebration starts a fresh qualifying run but may not summon the
/// companion until sixteen correlated forward cursor events have arrived.
/// This model deliberately isolates that travel guard from the stricter normal
/// band+dwell law: the bypass is the path on which the independent count is
/// load-bearing.
///
/// `Buggy=1` restores v0.56's ten-event floor. `NoCatBeforeSixteen` is therefore
/// violated by the exact historical witness (active at event ten) while the
/// committed model proves the cat remains hidden through event fifteen.
/// Tier-1 drives real `CursorCat::set_singing` + `CursorCat::on_key` calls and
/// includes the ten-event mutant as a negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_cat_earn_floor_model() -> Model {
    crate::ty_model! {
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
            invariant ActiveRequiresSinging:
                if active == 1 { singing == 1 } else { active == 0 };
            invariant RunBounded: run <= MinRun;
            invariant FlagsBounded: singing <= 1 && active <= 1;
        }
    }
}

/// Bounded FIFO/lifecycle for the Nyan fast-jump landing starbursts. Every
/// admitted fast jump retains the newest issued identity while evicting the
/// oldest at capacity. A style switch moves (never copies or loses) the active
/// ring into an outgoing fade owner; staggered expiry, fade completion, and
/// reset maintain the exact brisk-frame wake predicate.
///
/// `Buggy=1` drops the newest item at saturation and loses the payload during
/// fade transfer. Either defect violates a committed invariant. Tier-1 drives
/// the genuine landing helper, distinct landing identities, frame pruning,
/// style-fade ownership, overlap, fade completion, cadence, and master-off.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn nyan_jump_burst_lifecycle_model() -> Model {
    crate::ty_model! {
        NyanJumpBurstLifecycle {
            const BurstCap = 3;
            const TotalCap = 6;
            const MaxIssued = 6;
            const Buggy = 0;
            var resident = 0;
            var newest = 0;
            var ghost = 0;
            var ghost_newest = 0;
            var issued = 0;
            var wake = 0;
            var lost = 0;

            action FastJump when (issued <= MaxIssued - 1) {
                resident = if resident + 1 > BurstCap { BurstCap } else { resident + 1 };
                newest = if Buggy == 1 && resident + 1 > BurstCap {
                    newest
                } else {
                    issued + 1
                };
                issued = issued + 1;
                wake = 1;
            }
            action SlowJump { resident = resident; }
            action ExpireOne when (resident > 0) {
                resident = resident - 1;
                newest = if resident > 1 { newest } else { 0 };
                wake = if resident - 1 + ghost > 0 { 1 } else { 0 };
            }
            action BeginFade when (resident > 0) {
                ghost = if Buggy == 1 { 0 } else { resident };
                ghost_newest = if Buggy == 1 { 0 } else { newest };
                resident = 0;
                newest = 0;
                wake = if Buggy == 1 { 0 } else { 1 };
                lost = if Buggy == 1 { 1 } else { 0 };
            }
            action FinishFade when (ghost > 0) {
                ghost = 0;
                ghost_newest = 0;
                wake = if resident > 0 { 1 } else { 0 };
            }
            action Reset when (resident + ghost > 0) {
                resident = 0;
                newest = 0;
                ghost = 0;
                ghost_newest = 0;
                wake = 0;
            }

            invariant ResidentBounded: resident <= BurstCap;
            invariant GhostBounded: ghost <= BurstCap;
            invariant TotalBounded: resident + ghost <= TotalCap;
            invariant NewestRetained:
                if resident > 0 { newest == issued } else { newest == 0 };
            invariant GhostIdentityBounded: ghost_newest <= issued;
            invariant NoLostFadePayload: lost == 0;
            invariant WakeMatchesResidents:
                wake == if resident + ghost > 0 { 1 } else { 0 };
            invariant IssuedBounded: issued <= MaxIssued;
            invariant WakeBounded: wake <= 1;
        }
    }
}

/// Bounded admission/gating model for the Nyan terminus twinkle pool. Jump
/// scatter requires both a live ribbon and full motion; right-margin scatter
/// requires full motion but is already evidence of a live typing ribbon.
/// Both paths saturate at the shared particle cap, expire to idle, and reset
/// without leaving the animation scheduler armed.
///
/// `Buggy=1` bypasses the cold/reduced-motion guard and the cap. The
/// `NoFalseScatter` and `ParticlesBounded` invariants make both defect classes
/// catchable; Tier-1 binds the exact landing and scatter helpers plus cadence,
/// expiry, margin, and master-off behavior.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn nyan_terminus_admission_model() -> Model {
    crate::ty_model! {
        NyanTerminusAdmission {
            const ParticleCap = 6;
            const ScatterBurst = 2;
            const Buggy = 0;
            var particles = 0;
            var warm = 0;
            var reduced = 0;
            var wake = 0;
            var false_scatter = 0;

            action Warm when (warm == 0) { warm = 1; }
            action Cool when (warm == 1) { warm = 0; }
            action Reduce when (reduced == 0) { reduced = 1; }
            action Restore when (reduced == 1) { reduced = 0; }
            action JumpTerminus {
                particles = if warm == 1 && reduced == 0 {
                    if particles + ScatterBurst > ParticleCap {
                        if Buggy == 1 { particles + ScatterBurst } else { ParticleCap }
                    } else {
                        particles + ScatterBurst
                    }
                } else {
                    if Buggy == 1 { particles + ScatterBurst } else { particles }
                };
                wake = if warm == 1 && reduced == 0 {
                    1
                } else {
                    if Buggy == 1 { 1 } else { wake }
                };
                false_scatter = if warm == 1 && reduced == 0 {
                    false_scatter
                } else {
                    if Buggy == 1 { 1 } else { false_scatter }
                };
            }
            action MarginTerminus {
                particles = if reduced == 0 {
                    if particles + ScatterBurst > ParticleCap {
                        if Buggy == 1 { particles + ScatterBurst } else { ParticleCap }
                    } else {
                        particles + ScatterBurst
                    }
                } else {
                    if Buggy == 1 { particles + ScatterBurst } else { particles }
                };
                wake = if reduced == 0 {
                    1
                } else {
                    if Buggy == 1 { 1 } else { wake }
                };
                false_scatter = if reduced == 0 {
                    false_scatter
                } else {
                    if Buggy == 1 { 1 } else { false_scatter }
                };
            }
            action Expire when (particles > 0) {
                particles = 0;
                wake = 0;
            }
            action Reset when (particles > 0) {
                particles = 0;
                wake = 0;
            }

            invariant ParticlesBounded: particles <= ParticleCap;
            invariant NoFalseScatter: false_scatter == 0;
            invariant WakeMatchesParticles:
                wake == if particles > 0 { 1 } else { 0 };
            invariant FlagsBounded: warm <= 1 && reduced <= 1 && wake <= 1;
        }
    }
}

// ===========================================================================
// Generalized error-CLASS models (audit findings F1, ordering, reply-fidelity).
// These teach Trust to catch the *classes* the second bug-hunt surfaced — not
// just the specific bugs — so a future regression of the same shape fails the
// exhaustive `ty` check. Same Buggy convention as the safety models above.
// ===========================================================================

/// CAPABILITY SECRECY / information-flow (audit finding F1). A bearer-token SECRET
/// must reach a SANDBOXED same-uid peer (one that cannot read 0600 files) only if
/// it is placed in an INHERITABLE env sink; routed through a 0600 file it must not.
/// `published` is the channel (0 none, 1 file, 2 env); `peer_has` is whether the
/// sandboxed peer obtained the secret. `Buggy = 1` chooses the env channel (the
/// original design); `Buggy = 0` the 0600-file channel (the F1 fix).
///
/// Invariant `NoSecretToSandboxedPeer`: the sandboxed peer never holds the token.
/// `ty` proves it for the file channel and catches the env-channel disclosure —
/// a genuinely new property CLASS: explicit information flow of a secret to an
/// untrusted sink.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn capability_secrecy_model() -> Model {
    props::two_stage_leak(props::TwoStage {
        name: "CapabilitySecrecy",
        // Provision over the 0600-file channel (1) or, when Buggy, the inheritable
        // env channel (2).
        stage: "published",
        stage_act: "Provision",
        stage_rhs: if_(eq(cst("Buggy"), int(1)), int(2), int(1)),
        // A sandboxed peer obtains the secret ONLY from the env channel (2).
        leak: "peer_has",
        leak_act: "SandboxedRead",
        leak_guard: gt(var("published"), int(0)),
        leak_rhs: if_(eq(var("published"), int(2)), int(1), int(0)),
        inv: "NoSecretToSandboxedPeer",
        inv_expr: eq(var("peer_has"), int(0)),
    })
}

/// PUBLISH ORDERING (the graph-entry-before-bind race). A discovery entry must be
/// published only AFTER the socket is bound, so a concurrent stale-sweep can never
/// see an entry pointing at a not-yet-bound socket and delete it. `Buggy = 1`
/// publishes before binding (the original main-thread write); `Buggy = 0` requires
/// `bound` first (publish from inside `spawn` after `bind`).
///
/// Invariant `PublishImpliesBound`: `published ⟹ bound`. `ty` proves the ordered
/// discipline and catches the pre-bind publish.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
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

/// REPLY FIDELITY (the ERR-after-delivery defect). Once a forwarded verb has been
/// DELIVERED to the child, a later relay-stage failure must NOT report `ERR` to
/// the client (a false "didn't happen" for an op that did). `delivered` and
/// `reported_err` are booleans; `Buggy = 1` reports the error after delivery (the
/// original `connect_and_relay` returning the relay error), `Buggy = 0` swallows it
/// (return Ok once delivered — the fix).
///
/// Invariant `NoErrorAfterDelivery`: never both delivered AND error-reported.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn reply_fidelity_model() -> Model {
    props::two_stage_leak(props::TwoStage {
        name: "ReplyFidelity",
        stage: "delivered",
        stage_act: "Deliver",
        stage_rhs: int(1),
        leak: "reported_err",
        leak_act: "RelayFail",
        leak_guard: and_(
            eq(var("delivered"), int(1)),
            eq(var("reported_err"), int(0)),
        ),
        leak_rhs: if_(eq(cst("Buggy"), int(1)), int(1), int(0)),
        inv: "NoErrorAfterDelivery",
        inv_expr: or_(
            eq(var("delivered"), int(0)),
            eq(var("reported_err"), int(0)),
        ),
    })
}
