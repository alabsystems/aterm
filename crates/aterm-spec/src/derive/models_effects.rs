// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Sparkle / ignition / cursor-cat / rainbow-kitty effect models, plus the
//! generalized error-class trio — spec-model data constructors moved verbatim
//! out of the one-file catalog in `derive.rs`
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

/// CURSOR-CAT host gate and collectible discovery lifecycle. Ordinary rainbow kitty
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
/// ordinary rainbow kitty cat behind a disabled trail master. `HiddenTick` also consumes
/// the wall clock while `presented` stays
/// fixed. Enough hidden samples therefore walk Discovery through Fade to
/// Hidden without delivering the promised hold. The same switch makes
/// `LongPresentableGap` strand an expired hello in a visible Fade. `ty` proves
/// the full contract at `Buggy = 0` and catches both defect witnesses at
/// `Buggy = 1`.
///
/// Tier-0: `derived_cursor_cat_proves_and_catches_hidden_expiry`
/// (aterm-spec/tests/derived_ring_ty.rs). Tier-1 drives the real
/// `aterm_effects::kitty_cursor::CursorCat` clock and projects its public frame
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
            var trail_master = 0;   // host's ordinary rainbow-kitty owner gate
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

/// SING-ALONG held-key detector. A run becomes armed only on the sixteenth
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
/// `aterm_effects::kitty_sing::KittySing` press, release, and settle methods in
/// `aterm-effects/tests/kitty_activation_conformance.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn kitty_sing_detector_model() -> Model {
    crate::ty_model! {
        KittySingDetector {
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

/// Cursor-cat SING-ALONG bypass floor. Pinning the canonical momentum for a
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

/// Cursor classifier and typed-credit bookkeeping after causal movement
/// admission. Movement birth itself belongs exclusively to
/// [`cursor_move_candidate_model`]: typed/Return/reflow/navigation/Tab/paste
/// timestamps here are morphology inputs only. `AdmitCandidate` represents an
/// already-proven exact/synthetic decision before `WitnessedMove`; a cold move
/// and fresh/stale gesture classes stay dark.
/// `ArmGesture` also supersedes any swallowed typed/strong classifier so paste
/// cannot inherit re-anchor, ZOOM, or navigation suppression semantics. An
/// input with no possible cursor movement (empty/sanitized/zero-width paste or
/// text, or a stationary kill) takes `IgnoreNoMoveInput`: bytes/protocol
/// framing may egress, but no movement licence is armed for a later program
/// delta. `RevokeQueued` withdraws a newly armed class when its key bytes joined
/// an already-pending FIFO or its inline write fails, and retires the complete
/// typed-credit high-water owned by that now-unwitnessed latest hint. `RevokeAsyncPaste`
/// withdraws every paste's arrival-time class: even the first FIFO job is
/// written on another thread, so enqueue is not delivery. `DeclineHidden` consumes every class whenever a
/// hidden→visible completion reaches no `spawn` decision (a far relocation or
/// a same-cell return). Historical
/// warmth is amplitude only and remains `UnwitnessedMove`. `ArmTypedCredit` ->
/// `ObserveTypedEcho` is the bounded committed-cell ledger: every observed
/// non-coalesced boundary retires every earlier outstanding credit (the
/// one-slot hint cannot correlate a partial remote echo to one of several
/// in-flight presses), so already-rendered text cannot subsidize a later
/// coalesced program move. `recent_typing_activity` is a separate bounded
/// witness: spending admission cells does not erase the user's earned
/// navigation burst/debris reward, and that activity bit is never summed into
/// coalesce admission. `ArmNextTypedCredit` -> `ColdTwoCellMove` crosses into a
/// second cohort and proves its one new credit cannot pool with any retired
/// pre-boundary history. `SupersedeTypedCohort` is the Enter/Tab/navigation
/// boundary: dropping a swallowed typed hint retires the same credit high-water
/// even though no typed echo was observed. `ExpireTypedCohort` is the same
/// high-water fence when a later key arrives after the prior one-shot expired;
/// credits may outlive one hint only inside the still-correlatable batch window.
///
/// `Buggy=1` preserves the audited credit-retention mutants and restores the
/// forbidden timestamp-only birth. Tier-1 drives the genuine credit ring;
/// movement admission is bound separately by `cursor_move_candidate_model`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn rainbow_move_admission_model() -> Model {
    crate::ty_model! {
        RainbowMoveAdmission {
            const Buggy = 0;
            const CreditCap = 3;
            var witnessed = 0;
            var admitted = 0;
            var candidate_admitted = 0;
            var gesture_hint = 0;       // 0 absent, 1 fresh, 2 stale
            var gesture_arms = 0;
            var gesture_admissions = 0;
            var typed_class = 0;
            var strong_class = 0;       // Return/reflow/navigation
            var no_move_ignored = 0;
            var queued_revoked = 0;
            var async_paste_revoked = 0;
            var hidden_declined = 0;
            var credit_arms = 0;        // high-water before one observed boundary
            var pending_credits = 0;
            var spent_credits = 0;
            var observed_credit_boundary = 0;
            var recent_typing_activity = 0;
            var next_generation_credit = 0;
            var cold_two_cell_admitted = 0;
            var credit_superseded = 0;
            var credit_expired = 0;

            action UnwitnessedMove when (
                gesture_hint == 0 && queued_revoked == 0 && async_paste_revoked == 0
                    && hidden_declined == 0
            ) {
                witnessed = 0;
                admitted = if Buggy == 1 { 1 } else { 0 };
            }
            action AdmitCandidate when (
                candidate_admitted == 0 && queued_revoked == 0
                    && async_paste_revoked == 0 && hidden_declined == 0
            ) {
                candidate_admitted = 1;
            }
            action WitnessedMove when (
                candidate_admitted == 1 && queued_revoked == 0
                    && async_paste_revoked == 0 && hidden_declined == 0
            ) {
                witnessed = 1;
                admitted = 1;
                candidate_admitted = 0;
                gesture_hint = 0;
            }
            action ArmTyped when (
                queued_revoked == 0 && async_paste_revoked == 0 && hidden_declined == 0
            ) {
                typed_class = 1;
                strong_class = 0;
                gesture_hint = 0;
            }
            action ArmStrong when (
                queued_revoked == 0 && async_paste_revoked == 0 && hidden_declined == 0
            ) {
                strong_class = 1;
                typed_class = 0;
                gesture_hint = 0;
            }
            action IgnoreNoMoveInput when (
                gesture_hint == 0 && gesture_arms == 0 && no_move_ignored == 0
                    && queued_revoked == 0 && async_paste_revoked == 0
                    && hidden_declined == 0
            ) {
                no_move_ignored = 1;
                gesture_hint = if Buggy == 1 { 1 } else { 0 };
                gesture_arms = if Buggy == 1 { 1 } else { 0 };
            }
            action ArmGesture when (
                gesture_hint == 0 && gesture_arms == 0 && no_move_ignored == 0
                    && queued_revoked == 0 && async_paste_revoked == 0
                    && hidden_declined == 0
            ) {
                gesture_hint = 1;
                gesture_arms = 1;
                typed_class = if Buggy == 1 { typed_class } else { 0 };
                strong_class = if Buggy == 1 { strong_class } else { 0 };
            }
            action AgeGesture when (
                gesture_hint == 1 && queued_revoked == 0 && async_paste_revoked == 0
                    && hidden_declined == 0
            ) {
                gesture_hint = 2;
            }
            action GestureMove when (
                gesture_hint == 1 && queued_revoked == 0 && async_paste_revoked == 0
                    && hidden_declined == 0
            ) {
                witnessed = 0;
                admitted = if Buggy == 1 { 1 } else { 0 };
                gesture_hint = if Buggy == 1 { 1 } else { 0 };
                gesture_admissions = gesture_admissions + 1;
            }
            action StaleGestureMove when (
                gesture_hint == 2 && queued_revoked == 0 && async_paste_revoked == 0
                    && hidden_declined == 0
            ) {
                witnessed = 0;
                admitted = if Buggy == 1 { 1 } else { 0 };
                gesture_hint = 0;
                gesture_admissions = if Buggy == 1 {
                    gesture_admissions + 1
                } else {
                    gesture_admissions
                };
            }
            action RevokeQueued when (
                queued_revoked == 0 && async_paste_revoked == 0 && hidden_declined == 0
                    && (gesture_hint > 0 || typed_class > 0 || strong_class > 0)
            ) {
                gesture_hint = if Buggy == 1 { gesture_hint } else { 0 };
                typed_class = if Buggy == 1 { typed_class } else { 0 };
                strong_class = if Buggy == 1 { strong_class } else { 0 };
                pending_credits = if Buggy == 1 { pending_credits } else { 0 };
                spent_credits = if pending_credits > 0 {
                    if Buggy == 1 { spent_credits } else { credit_arms }
                } else {
                    spent_credits
                };
                observed_credit_boundary = if pending_credits > 0 {
                    1
                } else {
                    observed_credit_boundary
                };
                witnessed = 0;
                admitted = 0;
                queued_revoked = 1;
            }
            action RevokeAsyncPaste when (
                async_paste_revoked == 0 && queued_revoked == 0 && hidden_declined == 0
                    && (gesture_hint > 0 || typed_class > 0 || strong_class > 0)
            ) {
                gesture_hint = if Buggy == 1 { gesture_hint } else { 0 };
                typed_class = if Buggy == 1 { typed_class } else { 0 };
                strong_class = if Buggy == 1 { strong_class } else { 0 };
                witnessed = 0;
                admitted = 0;
                async_paste_revoked = 1;
            }
            action DeclineHidden when (
                hidden_declined == 0 && queued_revoked == 0 && async_paste_revoked == 0
                    && (gesture_hint > 0 || typed_class > 0 || strong_class > 0)
            ) {
                gesture_hint = if Buggy == 1 { gesture_hint } else { 0 };
                typed_class = if Buggy == 1 { typed_class } else { 0 };
                strong_class = if Buggy == 1 { strong_class } else { 0 };
                witnessed = 0;
                admitted = 0;
                hidden_declined = 1;
            }
            action ArmTypedCredit when (
                credit_arms <= CreditCap - 1 && observed_credit_boundary == 0
                    && queued_revoked == 0 && async_paste_revoked == 0
            ) {
                credit_arms = credit_arms + 1;
                pending_credits = pending_credits + 1;
                recent_typing_activity = 1;
            }
            action ObserveTypedEcho when (
                pending_credits > 0 && observed_credit_boundary == 0
            ) {
                pending_credits = if Buggy == 1 {
                    pending_credits
                } else {
                    0
                };
                spent_credits = credit_arms;
                observed_credit_boundary = 1;
                recent_typing_activity = if Buggy == 1 { 0 } else { 1 };
            }
            action SupersedeTypedCohort when (
                pending_credits > 0 && observed_credit_boundary == 0
                    && credit_superseded == 0
            ) {
                pending_credits = if Buggy == 1 { pending_credits } else { 0 };
                spent_credits = credit_arms;
                observed_credit_boundary = 1;
                credit_superseded = 1;
                typed_class = 0;
            }
            action ExpireTypedCohort when (
                pending_credits > 0 && observed_credit_boundary == 0
                    && credit_expired == 0
            ) {
                pending_credits = if Buggy == 1 { pending_credits } else { 0 };
                spent_credits = credit_arms;
                observed_credit_boundary = 1;
                credit_expired = 1;
                typed_class = 0;
            }
            action ArmNextTypedCredit when (
                observed_credit_boundary == 1 && next_generation_credit == 0
                    && cold_two_cell_admitted == 0
            ) {
                next_generation_credit = 1;
                recent_typing_activity = 1;
            }
            action ColdTwoCellMove when (
                observed_credit_boundary == 1 && next_generation_credit == 1
                    && cold_two_cell_admitted == 0
            ) {
                cold_two_cell_admitted = if pending_credits + next_generation_credit > 1 {
                    1
                } else {
                    0
                };
                next_generation_credit = 0;
            }

            invariant WitnessRequired:
                if witnessed == 0 { admitted == 0 } else { admitted == 1 };
            invariant CandidateRequired:
                if admitted == 1 { witnessed == 1 } else { admitted == 0 };
            invariant CandidateFlagBounded: candidate_admitted <= 1;
            invariant GestureAtMostOnce:
                gesture_admissions <= gesture_arms && gesture_admissions <= 1;
            invariant GestureHintBounded: gesture_hint <= 2;
            invariant GestureArmsBounded: gesture_arms <= 1;
            invariant NoMoveInputNeverArms:
                if no_move_ignored == 1 {
                    gesture_hint == 0 && gesture_arms == 0
                } else {
                    no_move_ignored == 0
                };
            invariant GestureClassUnambiguous:
                if gesture_hint > 0 {
                    typed_class == 0 && strong_class == 0
                } else {
                    typed_class <= 1 && strong_class <= 1
                };
            invariant QueuedDispatchRevokes:
                if queued_revoked == 1 {
                    gesture_hint == 0 && typed_class == 0
                        && strong_class == 0 && admitted == 0
                } else {
                    queued_revoked == 0
                };
            invariant QueuedDispatchRetiresTypedCreditCohort:
                if queued_revoked == 1 && credit_arms > 0 {
                    pending_credits == 0 && spent_credits == credit_arms
                        && observed_credit_boundary == 1
                } else {
                    queued_revoked <= 1
                };
            invariant AsyncPasteDispatchRevokes:
                if async_paste_revoked == 1 {
                    gesture_hint == 0 && typed_class == 0
                        && strong_class == 0 && admitted == 0
                } else {
                    async_paste_revoked == 0
                };
            invariant DeclinedHiddenConsumes:
                if hidden_declined == 1 {
                    gesture_hint == 0 && typed_class == 0
                        && strong_class == 0 && admitted == 0
                } else {
                    hidden_declined == 0
                };
            invariant TypedCreditConservation:
                pending_credits + spent_credits == credit_arms;
            invariant TypedCreditBounds:
                credit_arms <= CreditCap && pending_credits <= CreditCap
                    && spent_credits <= CreditCap
                    && observed_credit_boundary <= 1;
            invariant ObservedTypedBoundaryClearsCreditHighWater:
                if observed_credit_boundary == 1 {
                    pending_credits == 0 && spent_credits == credit_arms
                } else {
                    observed_credit_boundary == 0
                };
            invariant SpendingAdmissionKeepsRecentTypingActivity:
                if spent_credits > 0 {
                    recent_typing_activity == 1
                } else {
                    recent_typing_activity <= 1
                };
            invariant PostBoundaryCreditCannotPoolWithHistory:
                cold_two_cell_admitted == 0;
            invariant NextGenerationCreditBounded:
                next_generation_credit <= 1;
            invariant SupersessionRetiresTypedCreditCohort:
                if credit_superseded == 1 {
                    pending_credits == 0 && spent_credits == credit_arms
                } else {
                    credit_superseded == 0
                };
            invariant ExpiryRetiresTypedCreditCohort:
                if credit_expired == 1 {
                    pending_credits == 0 && spent_credits == credit_arms
                } else {
                    credit_expired == 0
                };
        }
    }
}

/// One-shot causal-evidence gate shared by every cursor-light style and the
/// classic comet. A user-input timestamp is only a classifier hint: typed and
/// Backspace movement becomes drawable after an input-time baseline and an
/// exact owned-cell diff prove the predicted source and target. Synthetic
/// previews carry an explicit trusted candidate; unsupported input, a second
/// unobserved event, scroll, hidden/same-cell completion, expiry, or any source /
/// target mismatch consumes the candidate dark.
/// Resident light is independently charged/projected: a coherent next frame
/// may retain it, final-extraction drift suppresses it, and any unowned parser
/// generation retires it even when the cursor did not move. An exact authored
/// generation also retires every prior resident pool before its admitted move
/// forges fresh geometry; its row proof cannot certify older light elsewhere.
///
/// `Buggy=1` is the historical timestamp-alone mutant: a fresh, geometrically
/// matching typed candidate admits without content evidence. `EvidenceRequired`
/// gives that exact defect a direct counterexample (`ArmTyped` -> `ObserveMove`),
/// while the endpoint/freshness/one-shot invariants cover the rest of the real
/// engine decision.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_move_candidate_model() -> Model {
    crate::ty_model! {
        CursorMoveCandidate {
            const Buggy = 0;
            var phase = 0;          // 0 idle, 1 captured, 2 armed, 3 confirmed, 4 consumed
            var kind = 0;           // 0 none, 1 typed, 2 Backspace, 3 synthetic
            var fresh = 0;
            var delivery_stable = 0;
            var next_generation = 0;
            var evidence_exact = 0;
            var origin_match = 0;
            var target_match = 0;
            var admitted = 0;
            var birth = 0;
            var observations = 0;
            var unsupported = 0;
            var final_checked = 0;
            var final_generation_match = 0;
            var projection = 0;
            var resident_charged = 0;
            var resident_projection = 0;
            var unowned_rewrite = 0;
            var candidate_rewrite = 0;
            var hidden_boundary = 0;
            var style = 0;          // ten built-ins/custom selectors, 0..9
            var engine = 0;         // 0 Glow, 1 classic trail
            var host = 0;           // 0 native, 1 embedded pipeline
            var birth_style = 0;
            var birth_engine = 0;
            var birth_host = 0;

            action NextStyle when (phase == 0 && style <= 8) { style = style + 1; }
            action NextEngine when (phase == 0 && engine == 0) { engine = 1; }
            action NextHost when (phase == 0 && host == 0) { host = 1; }
            action ChargeResident when (
                phase == 0 && resident_charged == 0
                    && unowned_rewrite == 0 && evidence_exact == 0
            ) {
                resident_charged = 1;
            }
            action NextResidentFrame when (
                final_checked == 1 && resident_charged == 1
            ) {
                final_checked = 0; final_generation_match = 0;
                projection = 0; resident_projection = 0;
            }
            action UnownedContentRewrite when (
                resident_charged == 1 || (phase > 0 && phase <= 3)
            ) {
                unowned_rewrite = 1;
                candidate_rewrite = if phase > 0 && phase <= 3 { 1 } else { 0 };
                phase = if Buggy == 1 {
                    phase
                } else {
                    if phase > 0 && phase <= 3 { 4 } else { phase }
                };
                admitted = 0; birth = 0;
                projection = if Buggy == 1 { projection } else { 0 };
                resident_charged = if Buggy == 1 { 1 } else { 0 };
                resident_projection = if Buggy == 1 { resident_projection } else { 0 };
            }
            action BeginResidentEpoch when (
                unowned_rewrite == 1 && resident_charged == 0
                    && resident_projection == 0
            ) {
                // The retirement marker describes the generation just
                // crossed. Clear it explicitly before a later, independently
                // admitted segment may become resident; never let
                // ChargeResident silently overwrite a live retirement proof.
                unowned_rewrite = 0; candidate_rewrite = 0;
            }

            action BeginCandidateEpoch when (
                phase == 4 && admitted == 0 && birth == 0 && projection == 0
                    && resident_projection == 0
            ) {
                phase = 0; kind = 0; fresh = 0; delivery_stable = 0;
                next_generation = 0; evidence_exact = 0;
                origin_match = 0; target_match = 0; observations = 0;
                unsupported = 0; final_checked = 0;
                final_generation_match = 0; unowned_rewrite = 0;
                candidate_rewrite = 0; hidden_boundary = 0; birth_style = 0;
                birth_engine = 0; birth_host = 0;
            }
            action ArmTyped when (
                phase == 0 && unowned_rewrite == 0 && hidden_boundary == 0
            ) {
                phase = 1; kind = 1; fresh = 1;
                origin_match = 1; target_match = 1;
            }
            action ArmBackspace when (
                phase == 0 && unowned_rewrite == 0 && hidden_boundary == 0
            ) {
                phase = 1; kind = 2; fresh = 1;
                origin_match = 1; target_match = 1;
            }
            action ArmSynthetic when (
                phase == 0 && unowned_rewrite == 0 && hidden_boundary == 0
            ) {
                phase = 2; kind = 3; fresh = 1; delivery_stable = 1;
                origin_match = 1; target_match = 1;
            }
            action DeliverStable when (phase == 1) {
                phase = 2; delivery_stable = 1;
            }
            action DeliverRaced when (phase == 1) {
                phase = if Buggy == 1 { 2 } else { 4 };
                delivery_stable = 0;
            }
            action DeliverQueued when (phase == 1) {
                phase = 4; delivery_stable = 0;
            }
            action ConfirmTypedNext when (phase == 2 && kind == 1) {
                phase = 3; next_generation = 1; evidence_exact = 1;
                resident_charged = if Buggy == 1 { resident_charged } else { 0 };
                resident_projection = if Buggy == 1 { resident_projection } else { 0 };
            }
            action ConfirmBackspaceNext when (phase == 2 && kind == 2) {
                phase = 3; next_generation = 1; evidence_exact = 1;
                resident_charged = if Buggy == 1 { resident_charged } else { 0 };
                resident_projection = if Buggy == 1 { resident_projection } else { 0 };
            }
            action NextGenerationMismatch when (phase == 2 && kind <= 2) {
                phase = 4; next_generation = 1;
            }
            action SkippedGeneration when (phase == 2 && kind <= 2) {
                phase = 4; next_generation = 0;
            }
            action OriginMismatch when (phase > 0 && phase <= 3) {
                phase = 4; origin_match = 0;
            }
            action TargetMismatch when (phase > 0 && phase <= 3) {
                phase = 4; target_match = 0;
            }
            action Age when (phase > 0 && phase <= 3) {
                phase = 4; fresh = 0;
            }
            action ObserveMove when (phase == 2 || phase == 3) {
                admitted = if kind == 3 {
                    if fresh == 1 && origin_match == 1 { 1 } else { 0 }
                } else {
                    if Buggy == 1 {
                        if fresh == 1 && origin_match == 1 && target_match == 1
                        { 1 } else { 0 }
                    } else {
                        if phase == 3 && fresh == 1 && delivery_stable == 1
                            && next_generation == 1 && evidence_exact == 1
                            && origin_match == 1 && target_match == 1
                        { 1 } else { 0 }
                    }
                };
                birth = if kind == 3 {
                    if fresh == 1 && origin_match == 1 { 1 } else { 0 }
                } else {
                    if Buggy == 1 {
                        if fresh == 1 && origin_match == 1 && target_match == 1
                        { 1 } else { 0 }
                    } else {
                        if phase == 3 && fresh == 1 && delivery_stable == 1
                            && next_generation == 1 && evidence_exact == 1
                            && origin_match == 1 && target_match == 1
                        { 1 } else { 0 }
                    }
                };
                birth_style = if Buggy == 1 { 0 } else { style };
                birth_engine = if Buggy == 1 { 0 } else { engine };
                birth_host = if Buggy == 1 { 0 } else { host };
                phase = 4; observations = observations + 1;
            }
            action ReobserveConsumed when (phase == 4 && observations == 1) {
                observations = if Buggy == 1 { 2 } else { 1 };
            }
            action ForgeUnadmittedBirth when (
                phase == 4 && admitted == 0 && birth == 0
            ) {
                birth = if Buggy == 1 { 1 } else { 0 };
            }
            action FinalExtractSame when (
                ((phase == 4 && admitted == 1) || resident_charged == 1)
                    && final_checked == 0
            ) {
                final_checked = 1; final_generation_match = 1;
                projection = admitted; resident_projection = resident_charged;
            }
            action FinalExtractDrift when (
                ((phase == 4 && admitted == 1) || resident_charged == 1)
                    && final_checked == 0
            ) {
                final_checked = 1; final_generation_match = 0;
                projection = if Buggy == 1 { admitted } else { 0 };
                resident_projection = if Buggy == 1 { resident_charged } else { 0 };
            }
            action PromoteProjectedBirth when (
                phase == 4 && birth == 1 && projection == 1
                    && final_checked == 1 && final_generation_match == 1
                    && resident_charged == 0
            ) {
                // The freshly projected candidate geometry becomes the
                // resident light a later terminal generation must fence. Its
                // consumed evidence is no longer a licence for any new birth.
                resident_charged = 1; resident_projection = 1;
                admitted = 0; projection = 0; evidence_exact = 0; birth = 0;
            }
            action CompleteNoMove when (phase > 0 && phase <= 3) {
                phase = 4; admitted = 0; birth = 0;
            }
            action Supersede when (phase > 0 && phase <= 3) {
                phase = 4; admitted = 0; birth = 0;
            }
            action UnsupportedInput when (phase > 0 && phase <= 3) {
                phase = 4; admitted = 0; birth = 0; unsupported = 1;
            }
            action ScrollBoundary when (phase > 0 && phase <= 3) {
                phase = 4; admitted = 0; birth = 0;
            }
            action HiddenBoundary when (
                (phase > 0 && phase <= 3) || resident_charged == 1
                    || resident_projection == 1
            ) {
                hidden_boundary = 1;
                phase = if Buggy == 1 { phase } else { 4 };
                admitted = 0; birth = 0;
                projection = if Buggy == 1 { projection } else { 0 };
                resident_charged = if Buggy == 1 { resident_charged } else { 0 };
                resident_projection = if Buggy == 1 { resident_projection } else { 0 };
            }

            invariant EvidenceRequired:
                if admitted == 1 && kind <= 2 { evidence_exact == 1 } else { admitted <= 1 };
            invariant StableDeliveryRequired:
                if admitted == 1 && kind <= 2 { delivery_stable == 1 } else { admitted <= 1 };
            invariant NextGenerationRequired:
                if admitted == 1 && kind <= 2 { next_generation == 1 } else { admitted <= 1 };
            invariant ExactEndpointRequired:
                if admitted == 1 {
                    origin_match == 1 && (kind == 3 || target_match == 1)
                } else { admitted == 0 };
            invariant FreshRequired:
                if admitted == 1 { fresh == 1 } else { admitted == 0 };
            invariant BirthRequiresAdmission: birth <= admitted;
            invariant ProjectionRequiresFinalGeneration:
                if projection == 1 {
                    admitted == 1 && final_checked == 1 && final_generation_match == 1
                } else { projection == 0 };
            invariant ResidentProjectionRequiresFinalGeneration:
                if resident_projection == 1 {
                    resident_charged == 1 && final_checked == 1
                        && final_generation_match == 1
                } else { resident_projection == 0 };
            invariant UnownedContentRewriteRetiresResident:
                if unowned_rewrite == 1 {
                    resident_charged == 0 && resident_projection == 0
                } else { unowned_rewrite == 0 };
            invariant UnownedContentRewriteConsumesCandidate:
                if candidate_rewrite == 1 {
                    phase == 4 && admitted == 0 && birth == 0
                } else { candidate_rewrite == 0 };
            invariant HiddenBoundaryDrainsCandidateAndResident:
                if hidden_boundary == 1 {
                    phase == 4 && admitted == 0 && birth == 0
                        && resident_charged == 0 && resident_projection == 0
                } else { hidden_boundary == 0 };
            invariant ExactContentChangeRetiresPriorResident:
                if evidence_exact == 1 {
                    resident_charged == 0 && resident_projection == 0
                } else { evidence_exact == 0 };
            invariant UnsupportedStaysDark:
                if unsupported == 1 { admitted == 0 && birth == 0 } else { unsupported == 0 };
            invariant CandidateConsumedOnce: observations <= 1;
            invariant UniversalSelectorsBounded: style <= 9 && engine <= 1 && host <= 1;
            invariant BirthBoundToSelectedStyleEngineHost:
                if birth == 1 {
                    birth_style == style && birth_engine == engine && birth_host == host
                } else { birth == 0 };
            invariant UniversalBirthNeedsCandidateEvidence:
                if birth == 1 {
                    kind == 3 || (delivery_stable == 1 && next_generation == 1
                        && evidence_exact == 1 && origin_match == 1 && target_match == 1)
                } else { birth == 0 };
            invariant CandidateBounds:
                phase <= 4 && kind <= 3 && fresh <= 1 && delivery_stable <= 1
                    && next_generation <= 1 && evidence_exact <= 1
                    && origin_match <= 1 && target_match <= 1
                    && final_checked <= 1 && final_generation_match <= 1
                    && candidate_rewrite <= 1
                    && projection <= 1 && resident_charged <= 1
                    && resident_projection <= 1 && unowned_rewrite <= 1
                    && hidden_boundary <= 1 && birth_style <= 9
                    && birth_engine <= 1 && birth_host <= 1;
        }
    }
}

/// LIVENESS twin of the [`cursor_move_candidate_model`] confirmation seam —
/// the rainbow-trail blackout's missing half. 201449c2 shipped the movement
/// admission gate WITH a formal model that proved SAFETY (cold program output
/// never paints trails) — and nobody stated that a real keystroke with a real
/// echo ever ADMITS. The model's echo environment was idealized; real shells
/// produce shapes it never contained, so a gate that provably never lied still
/// provably never spoke. This model closes that class: an ENVIRONMENT
/// ADVERSARY whose actions produce every audited echo shape, composed with the
/// shipped confirmation decision (`CursorGlow::confirm_content_candidate`),
/// under BOTH obligation families at once.
///
/// The adversary's shapes (`shape`), each an audited incident witness:
///
///   1 `KeyPlainEcho`         — the textbook echo: the typed glyph
///       materializes at the caret in the single next processed generation.
///   2 `KeyGhostSuggest` (E1) — zsh-autosuggestions POSTDISPLAY: the SAME
///       echo batch also paints ghost text at/after the caret. The caret is
///       the exactness frontier; post-caret cells are the shell's
///       presentation zone and carry no veto.
///   3 `KeySpaceOnBlanks` (E3) — a typed SPACE onto tail-filled implicit
///       blanks: content-invisible under the implicit-blank lens; the
///       materialization witness is STORAGE GROWTH of the stored row over the
///       owned span.
///   4 `KeyOvertypeSuggestion` (E4) — retyping under a VISIBLE suggestion:
///       the typed glyph is already painted at the caret, so the echo is a
///       NULL DIFF; the witness is the expected span already present plus the
///       exact predicted landing under the one attributable generation.
///   5 `KeySplitEcho` (E2) — the echo crosses TWO PTY read batches (baseline
///       + 2, not + 1). REGISTERED STANDING GAP: the strict next-generation
///       law retires it today (`StandingGapSplitEchoRetiresE2`).
///   6 `KeyBurst` (E5) — several keys between rendered frames: the proof
///       anchor is stale (the row probe predates the keystroke), and proof
///       capture declines. REGISTERED STANDING GAP
///       (`StandingGapBurstRetiresE5`).
///   7 `ColdSpinner`          — cold program output, no keystroke at all.
///   8 `KeyEchoSwallowed`     — the echo never arrives; an unrelated batch
///       crosses the input boundary instead.
///   9 `KeyDeviatingEcho`     — the shell echoes something other than the
///       typed glyph at the caret.
///
/// Obligation families:
///
///   SAFETY (kept, never traded away for liveness): cold, swallowed, and
///   deviating shapes never confirm; every confirmation carries an armed
///   keystroke, its delivered echo, the single attributable generation, a
///   fresh anchor, an intact pre-caret prefix, and a materialization witness
///   (`ConfirmIsWitnessed` and the per-shape dark invariants).
///
///   LIVENESS (the new family): every settled run over a shape the shipped
///   code claims to handle — plain, E1, E3, E4 — ends CONFIRMED
///   (`LivePlainEchoConfirms`, `LiveGhostTextConfirmsE1`,
///   `LiveBlankSpaceConfirmsE3`, `LiveOvertypeConfirmsE4`), and every run
///   reaches a decision at all (Tier-0 runs `find_deadlock` with
///   `settled = 1` as the final predicate — the bounded eventuality).
///
///   STANDING GAPS (honest limits, stated as checked facts and reprinted by
///   the Tier-0 driver — never silently waived): E2 and E5 settle RETIRED by
///   design today. The strict generation law (split-batch echoes) and the
///   multi-key stale anchor remain registered follow-ups to the blackout
///   repair, alongside split-pane arming.
///
/// `Buggy = 1` is the 201449c2 confirmation law verbatim: whole-row exactness
/// (any post-caret change vetoes) and a newly-materialize-only witness (null
/// diffs and content-invisible blanks cannot testify). That mutant is still
/// SAFE — every safety invariant above holds at `Buggy = 1` — and that is
/// precisely the audited defect class: only the LIVENESS family catches it
/// (E1/E3/E4 settle retired), so a checker that stated safety alone would
/// have called the mute gate green. Tier-1 binds the real
/// `CursorGlow::confirm_content_candidate` decision to `Decide` per shape in
/// `aterm-effects/src/cursor_glow.rs`
/// (`real_confirm_content_candidate_refines_the_typed_echo_liveness_model`).
#[must_use]
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn typed_echo_liveness_model() -> Model {
    crate::ty_model! {
        TypedEchoLiveness {
            const Buggy = 0;
            var shape = 0;              // 0 unpicked, then the roster above
            var armed = 0;              // a real typed keystroke armed a candidate
            var echoed = 0;             // the environment delivered its batch
            var gen_delta = 0;          // parser generations crossed since baseline
            var stale_anchor = 0;       // proof capture predates the keystroke (E5)
            var caret_material = 0;     // expected span present at the caret in the probe
            var pre_caret_intact = 0;   // every cell BEFORE the caret content-identical
            var post_caret_changed = 0; // presentation zone painted in the same batch (E1)
            var content_changed = 0;    // row content differs under the implicit-blank lens
            var storage_growth = 0;     // stored row grew to cover the owned span (E3)
            var prepainted = 0;         // expected glyphs already at the caret at input (E4)
            var confirmed = 0;
            var retired = 0;
            var settled = 0;            // the decision is final

            // -- the environment adversary picks one echo shape --------------
            action KeyPlainEcho when (shape == 0) { shape = 1; armed = 1; }
            action KeyGhostSuggest when (shape == 0) { shape = 2; armed = 1; }
            action KeySpaceOnBlanks when (shape == 0) { shape = 3; armed = 1; }
            action KeyOvertypeSuggestion when (shape == 0) {
                shape = 4; armed = 1; prepainted = 1;
            }
            action KeySplitEcho when (shape == 0) { shape = 5; armed = 1; }
            action KeyBurst when (shape == 0) { shape = 6; armed = 1; stale_anchor = 1; }
            action ColdSpinner when (shape == 0) { shape = 7; }
            action KeyEchoSwallowed when (shape == 0) { shape = 8; armed = 1; }
            action KeyDeviatingEcho when (shape == 0) { shape = 9; armed = 1; }

            // -- the environment delivers the batch its shape promised -------
            action EchoPlain when (shape == 1 && echoed == 0) {
                echoed = 1; gen_delta = 1; caret_material = 1;
                pre_caret_intact = 1; content_changed = 1;
            }
            action EchoWithGhostText when (shape == 2 && echoed == 0) {
                echoed = 1; gen_delta = 1; caret_material = 1;
                pre_caret_intact = 1; content_changed = 1; post_caret_changed = 1;
            }
            action EchoStorageGrowth when (shape == 3 && echoed == 0) {
                echoed = 1; gen_delta = 1; caret_material = 1;
                pre_caret_intact = 1; storage_growth = 1;
            }
            action EchoNullDiffOvertype when (shape == 4 && echoed == 0) {
                echoed = 1; gen_delta = 1; caret_material = 1;
                pre_caret_intact = 1;
            }
            action EchoSplitBatches when (shape == 5 && echoed == 0) {
                echoed = 1; gen_delta = 2; caret_material = 1;
                pre_caret_intact = 1; content_changed = 1;
            }
            action EchoAfterBurst when (shape == 6 && echoed == 0) {
                echoed = 1; gen_delta = 1; caret_material = 1;
                pre_caret_intact = 1; content_changed = 1;
            }
            action ColdPaint when (shape == 7 && echoed == 0) {
                echoed = 1; gen_delta = 1; content_changed = 1;
                post_caret_changed = 1;
            }
            action UnrelatedBatch when (shape == 8 && echoed == 0) {
                echoed = 1; gen_delta = 1; pre_caret_intact = 1;
            }
            action EchoDeviates when (shape == 9 && echoed == 0) {
                echoed = 1; gen_delta = 1; content_changed = 1;
                pre_caret_intact = 1;
            }

            // -- the shipped confirmation decision ---------------------------
            action Decide when (echoed == 1 && settled == 0) {
                confirmed = if Buggy == 1 {
                    // 201449c2 verbatim: whole-row exactness (any post-caret
                    // change vetoes) + newly-materialize-only witness.
                    if armed == 1 && gen_delta == 1 && stale_anchor == 0
                        && pre_caret_intact == 1 && caret_material == 1
                        && content_changed == 1 && post_caret_changed == 0
                        && prepainted == 0
                    { 1 } else { 0 }
                } else {
                    // The shipped fix: caret-frontier exactness + any of the
                    // three materialization witnesses (content diff, storage
                    // growth, overtype-null-diff).
                    if armed == 1 && gen_delta == 1 && stale_anchor == 0
                        && pre_caret_intact == 1 && caret_material == 1
                        && (content_changed == 1 || storage_growth == 1
                            || prepainted == 1)
                    { 1 } else { 0 }
                };
                retired = if Buggy == 1 {
                    if armed == 1 && gen_delta == 1 && stale_anchor == 0
                        && pre_caret_intact == 1 && caret_material == 1
                        && content_changed == 1 && post_caret_changed == 0
                        && prepainted == 0
                    { 0 } else { armed }
                } else {
                    if armed == 1 && gen_delta == 1 && stale_anchor == 0
                        && pre_caret_intact == 1 && caret_material == 1
                        && (content_changed == 1 || storage_growth == 1
                            || prepainted == 1)
                    { 0 } else { armed }
                };
                settled = 1;
            }

            // SAFETY — the 201449c2 protections, kept word for word.
            invariant ColdSpinnerNeverConfirms:
                if shape == 7 { confirmed == 0 } else { shape <= 9 };
            invariant SwallowedEchoNeverConfirms:
                if shape == 8 { confirmed == 0 } else { shape <= 9 };
            invariant DeviatingEchoNeverConfirms:
                if shape == 9 { confirmed == 0 } else { shape <= 9 };
            invariant ConfirmIsWitnessed:
                if confirmed == 1 {
                    armed == 1 && echoed == 1 && gen_delta == 1
                        && stale_anchor == 0 && pre_caret_intact == 1
                        && caret_material == 1
                } else { confirmed == 0 };
            invariant SettledIsDecided:
                if settled == 1 {
                    confirmed + retired == armed
                } else { confirmed == 0 && retired == 0 };

            // LIVENESS — every handled shape's real echo eventually confirms.
            invariant LivePlainEchoConfirms:
                if settled == 1 && shape == 1 { confirmed == 1 } else { settled <= 1 };
            invariant LiveGhostTextConfirmsE1:
                if settled == 1 && shape == 2 { confirmed == 1 } else { settled <= 1 };
            invariant LiveBlankSpaceConfirmsE3:
                if settled == 1 && shape == 3 { confirmed == 1 } else { settled <= 1 };
            invariant LiveOvertypeConfirmsE4:
                if settled == 1 && shape == 4 { confirmed == 1 } else { settled <= 1 };

            // REGISTERED STANDING GAPS — checked facts, not aspirations: the
            // strict generation law (E2) and the multi-key stale anchor (E5)
            // retire real echoes today. Their Tier-0 driver reprints these as
            // standing findings on every run; deleting either invariant (or
            // one starting to fail because the gap was FIXED) must be a loud,
            // deliberate model edit, not a silent drift.
            invariant StandingGapSplitEchoRetiresE2:
                if settled == 1 && shape == 5 {
                    confirmed == 0 && retired == 1
                } else { settled <= 1 };
            invariant StandingGapBurstRetiresE5:
                if settled == 1 && shape == 6 {
                    confirmed == 0 && retired == 1
                } else { settled <= 1 };

            invariant EchoLivenessBounds:
                shape <= 9 && armed <= 1 && echoed <= 1 && gen_delta <= 2
                    && stale_anchor <= 1 && caret_material <= 1
                    && pre_caret_intact <= 1 && post_caret_changed <= 1
                    && content_changed <= 1 && storage_growth <= 1
                    && prepainted <= 1 && confirmed <= 1 && retired <= 1
                    && settled <= 1;
        }
    }
}

/// Cursor-owned pixels/cells live in the active viewport coordinate space.
/// Entering retained history must immediately suppress the DEC cursor, both
/// trail engines, every cursor body/companion overlay, and a later Retain
/// projection must keep them dark. The resident pet brain still receives a
/// hidden-caret tick and can settle its scheduler; presentation is suppressed,
/// not lifecycle progress.
///
/// `Buggy=1` reproduces the former `cur=None` implementation: resident trail
/// geometry and cursor companions remain projected over unrelated history,
/// while the hidden pet lifecycle receives no progress and can strand its
/// frame cadence.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_viewport_lifecycle_model() -> Model {
    crate::ty_model! {
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
                pet_brain_ticked = if pet_brain_pending == 1 { 1 } else { 0 };
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
                scheduler_stuck = if Buggy == 1 { 1 } else { 0 };
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
            invariant CursorViewportValuesBounded:
                live_viewport <= 1 && glow_visible <= 1 && trail_visible <= 1
                    && cursor_body_visible <= 1 && pet_visible <= 1
                    && base_cursor_visible <= 1 && pet_brain_pending <= 1
                    && pet_brain_ticked <= 1 && retained_checked <= 1;
        }
    }
}

/// Family-wide lifecycle for PTY scroll translation. `survivor_y` abstracts
/// every position-bearing member whose translated anchor remains visible;
/// `off_top_alive` abstracts every member that crosses the top boundary. The
/// real Tier-1 fixtures seed every concrete CursorGlow pool (including tail,
/// wake, future glide landing, and an outgoing fade) and every CursorTrail
/// position field, assert their exact deltas, then project one survivor and one
/// retired member from each engine onto this transition.
///
/// `stale_proof_alive` covers an input proof whose row identity is invalidated
/// by the same scroll fence (the plain-Backspace `(row, fill)` baseline and its
/// one-shot poof hint). `Buggy=1` is the historical per-family omission: live
/// geometry and stale provenance all remain in their pre-scroll state. The
/// invariants separately catch a detached segment, an off-top segment, and a
/// proof later reused against unrelated content at the same numeric row.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_effect_scroll_model() -> Model {
    crate::ty_model! {
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

/// Host-side scroll observation at a retained-history cap. The terminal owns
/// two cumulative, non-consuming clocks: `uniform_rows` for composable
/// full-screen upward motion and `epoch` for region/alt/reset/restore mutations whose
/// coordinates cannot be transformed as one plane. `retained_history` stays
/// zero in every state, modeling both a zero-history terminal and a saturated
/// ring whose public count no longer changes.
///
/// `UniformAtCap` must still reach one exact translation and retire row-bound
/// proof. Region, alternate-screen, reset and in-place restore events must instead choose the
/// invalidation decision and drop all geometry/proof. `Buggy=1` is the former
/// GUI policy: infer motion from the unchanged retained count and preserve the
/// stranded effect state.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_scroll_signal_model() -> Model {
    crate::ty_model! {
        CursorScrollSignal {
            const StartY = 3;
            const Delta = 2;
            const Buggy = 0;
            var event = 0;              // 0 initial, 1 uniform, 2 region, 3 alt, 4 reset, 5 restore
            var retained_history = 0;   // capped: deliberately unchanged
            var uniform_rows = 0;       // cumulative composable scroll clock
            var epoch = 0;              // cumulative invalidation clock
            var decision = 0;           // 0 none, 1 translate, 2 invalidate
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
            action AltInvalidation when (event == 0) {
                event = 3;
                epoch = 1;
                decision = if Buggy == 1 { 0 } else { 2 };
                geometry_alive = if Buggy == 1 { 1 } else { 0 };
                proof_alive = if Buggy == 1 { 1 } else { 0 };
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

            invariant RetainedCountIsNotAuthority:
                if event == 1 {
                    retained_history == 0 && uniform_rows == Delta && decision == 1
                } else {
                    retained_history == 0
                };
            invariant UniformTranslationExact:
                if event == 1 {
                    survivor_y == StartY - Delta && geometry_alive == 1
                        && proof_alive == 0
                } else {
                    survivor_y == StartY
                };
            invariant AmbiguousMotionInvalidates:
                if event > 1 {
                    epoch == 1 && decision == 2
                        && geometry_alive == 0 && proof_alive == 0
                } else {
                    epoch == 0
                };
        }
    }
}

/// Bounded FIFO/lifecycle for the rainbow kitty fast-jump landing starbursts. Every
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
pub fn rainbow_jump_burst_lifecycle_model() -> Model {
    crate::ty_model! {
        RainbowJumpBurstLifecycle {
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

/// Bounded admission/gating model for the rainbow terminus twinkle pool. Jump
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
pub fn rainbow_terminus_admission_model() -> Model {
    crate::ty_model! {
        RainbowTerminusAdmission {
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
