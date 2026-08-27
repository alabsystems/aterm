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

/// The classic flying cursor kitty's forward/reverse wrap placement. A fold
/// begins on the old edge, leaves that edge wholly off glass, changes sides
/// while still wholly off glass, and only then re-enters on the new edge.
/// `off_samples` is the history abstraction bound by Tier-1 to actual signed
/// sprite rectangles; it cannot be minted by an on-glass sample.
///
/// `Buggy = 1` reproduces the retired direct placement law: the first fold
/// sample jumps straight from the old on-glass edge to the new on-glass edge,
/// with no wholly-off-glass witness. `OffGlassBeforeSideChange` catches that
/// transition in either direction.
///
/// Tier-0: `derived_cursor_cat_fold_proves_and_catches_direct_edge_teleport`.
/// Tier-1: `cursor_cat_fold_conformance_real_placement_and_teleport_mutant`
/// drives `WordDecorations::resolve_kitty_cursor_placement` and projects the
/// emitted signed sprite rectangle after every transition.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_cat_fold_model() -> Model {
    crate::ty_model! {
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
            invariant StateBounded:
                phase <= 4 && direction <= 2 && origin_side <= 1 && side <= 1 &&
                off_glass <= 1 && off_samples <= 1 && folds <= MaxFolds;
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

/// Reduced-motion custody handoff from the static singing cursor cat back to
/// the resident pet. The full song owns the glass first. Its first wind-down
/// sample keeps the already-ready resident suppressed behind the still-opaque
/// singer; the singer remains visible through the inclusive 0.50 down to 0.33
/// static band, and the resident takes over below the 0.33 face-swap threshold.
///
/// `SampleCadencedBelowHalf` is the ordinary sequence through an observed 0.50
/// sample. `SampleLateBelowHalf` is the equally valid direct 1.0 -> 0.49
/// observation after an occluded/delayed callback. `SampleLateBelowFaceSwap`
/// and `SampleLateDrained` cover stronger direct 1.0 -> 0.30 / 0.0 callbacks
/// with no intermediate tick. Every route must preserve visible custody.
/// `Buggy=1` restores the historical handoff blackout: the resident is ready
/// but its draw gate has not opened, while the singer is already cut.
/// `LiveTailKeepsCompanionVisible` catches the live-tail gap and exclusive
/// custody catches the fully drained all-transparent sample.
///
/// Tier-0 lives in `derived_ring_ty.rs`. Tier-1 binds these sample actions to
/// the real `flying_kitty_admitted` / `pet_companion_admitted` custody gates.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn reduced_motion_companion_handoff_model() -> Model {
    crate::ty_model! {
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
                singer_visible = 1;
                pet_ready = 1;
                pet_visible = 0;
            }
            action SampleAtHalfCutoff when (phase == 1) {
                phase = 2;
                singer_visible = if Buggy == 1 { 0 } else { 1 };
                pet_ready = 1;
                pet_visible = 0;
            }
            action SampleCadencedBelowHalf when (phase == 2) {
                phase = 3;
                singer_visible = if Buggy == 1 { 0 } else { 1 };
                pet_ready = 1;
                pet_visible = 0;
            }
            action SampleLateBelowHalf when (phase == 1) {
                phase = 3;
                singer_visible = if Buggy == 1 { 0 } else { 1 };
                pet_ready = 1;
                pet_visible = 0;
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
            invariant GlassCustodyIsExclusive:
                singer_visible + pet_visible == 1;
            invariant ResidentOwnsBelowSwap:
                if phase > 3 {
                    singer_visible == 0 && pet_ready == 1 && pet_visible == 1
                } else { phase <= 3 };
            invariant StateBounded:
                phase <= 5 && started <= 1 && song_tail_live <= 1
                    && singer_visible <= 1 && pet_ready <= 1 && pet_visible <= 1;
        }
    }
}

/// One-shot routing for the CLASSIFIER-MINTED cursor-cat motion pulse shared by
/// ordinary rendering and the extracted/composed render path. A render route
/// that observes the pulse must take it from the glow producer and deliver it
/// to the cat exactly once. Route changes after that frame see no pulse, so a
/// style/layout transition cannot replay old typing into a new owner.
///
/// WHERE THE PULSE COMES FROM (`docs/design/EFFECTS-LICENSE-REDESIGN.md`): a
/// spawn that cleared the LICENSE seam and was then positively classified as a
/// typed advance or fold — `RainbowState::momentum_pulse`, minted beside
/// `shape_wrap`/`re_anchor` inside `CursorGlow::spawn`. It is not "authenticated"
/// by anything downstream: under the license law nothing is pending between the
/// key hint and the verdict, so `LicenseMove` -> `ClassifyPulse` is the whole
/// provenance chain and an unlicensed program move never reaches the classifier
/// that mints one.
///
/// `Buggy=1` reproduces two independent defects: the composed-route omission —
/// that route records an attempt but neither takes nor delivers the pulse,
/// stranding it in the producer, after which switching to the ordinary route
/// consumes the stale pulse (`stale_replay`) — and a pulse minted with no
/// license behind it (`cold_pulse`), the shape a re-introduced cold spawn seam
/// would have. Tier-0 proves the healthy conservation/one-shot/provenance laws
/// and requires all three witnesses. Tier-1 binds the route actions to
/// `take_cursor_cat_motion_pulse` plus `forward_kitty_cursor_motion` at the
/// ordinary and composed/extracted seams.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_cat_motion_pulse_routing_model() -> Model {
    crate::ty_model! {
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
            // The producer mints the pulse INSIDE a licensed spawn, from the
            // classifier. `Buggy=1` lets it mint with no license behind it.
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
            invariant DeliveryIsClassifiedAndAtMostOnce:
                consumes <= classified && deliveries <= classified
                    && deliveries == consumes;
            invariant StateBounded:
                licensed <= 1 && classified <= 1 && pending <= 1 && route <= 2
                    && attempted_route <= 2 && consumes <= 1 && deliveries <= 1
                    && switched <= 1 && stranded <= 1 && stale_replay <= 1
                    && cold_pulse <= 1;
        }
    }
}

/// THE LICENSE, modelled at the seam that asks it: *did a human touch the
/// keyboard just now?* (`docs/design/EFFECTS-LICENSE-REDESIGN.md`).
///
/// The predicate, enumerated term by term at the top of `CursorGlow::spawn` /
/// `CursorTrail::spawn`, before `classify_move` and before one byte of state
/// moves:
///
/// ```text
/// licensed = fresh(type_hint) || fresh(quench_hint) || fresh(nav_hint)
///         || fresh(return_hint) || fresh(newline_hint)
///         || fresh(user_gesture_hint) || synthetic_note_pending
/// ```
///
/// Six key-hint stamps under their own freshness constants (the 0.25 s class),
/// plus the scripted-preview note, which needs no field of its own because
/// `note_synthetic_move` / `note_synthetic_typed` stamp `user_gesture_hint` /
/// `type_hint` at the same instant. The disjunction is CLASS-BLIND on purpose —
/// which choreography fires stays `classify_move`'s job — so the model carries
/// ONE three-valued licence term (`hint`: absent / fresh / stale) rather than
/// six symmetric copies of the same disjunct.
///
/// WHAT THE MODEL CLAIMS, derived from that predicate rather than from the
/// implementation (the enumerate-the-predicate discipline that ended the
/// erase-poof lane), and each claim carrying its own `Buggy=1` counterexample:
///
/// * A move with NO stamp mints nothing (`cold_admitted`) — the cold token
///   streamer, `cat`, a spinner two rows away. This is the one invariant the
///   design names, and it is STRICTLY STRONGER than v0.43.0, whose tick spawned
///   on every presented cursor delta and let a cold one-cell advance earn heat.
/// * A move with a STALE stamp mints nothing (`stale_admitted`). Freshness is
///   half the predicate; a model that only knew about absence would prove
///   nothing about the window.
/// * `reflow_hint` / `blink_hint` are NOT licence terms (`morphology_admitted`).
///   A resize and a TUI's repaint blink are not keypresses; they stay morphology
///   inputs, and the mutant is the maintainer who "helpfully" adds one to the
///   disjunction.
/// * A key this window SWALLOWED never licenses (`swallow_admitted`) — the ~15
///   `clear_move_license` call sites (mouse, wheel, focus, paste, scroll, tab
///   switch, IME, a11y, `signal`). A stamp that outlives its own boundary is
///   not an answer to the licence question.
/// * A DECLINED move destroys nothing (`wiped`). This is the second half of the
///   law and the actual reported darkness: the proof era's denial paths refused
///   to mint (fine) and then wiped the ribbon the user's own typing had earned.
///   Retention is decay + `note_scroll` translation + reset, and nothing else.
/// * Every armed press reaches EXACTLY ONE disposition — consumed by its one
///   paired echo, expired by freshness, cleared at a swallow boundary, or
///   superseded by a newer press over the one-slot stamp. Never two, never none;
///   `admissions <= arms` is the same law counted from the other side (one hint,
///   one echo).
/// * The press CREDIT budget spends monotonically: spent cells never exceed the
///   cells real presses banked, an admitted multi-cell coalesce PAYS for its
///   ribbon, a starved one pays nothing and stays dark, and retiring a stale
///   stamp never refunds cells to fund a second stray sweep (`credit_refunded`).
/// * The slimmed diagnosis ring cannot lie to `ctl trail`: every spawn scores
///   exactly one of `licensed`/`declined`, and a `licensed` row means light was
///   actually minted.
///
/// Tier-1 (`cursor_glow.rs`) drives the REAL `CursorGlow` through press, echo,
/// cold echo and expiry against these transitions.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_hint_license_model() -> Model {
    crate::ty_model! {
        CursorHintLicense {
            const Buggy = 0;
            const ArmCap = 2;
            const CoalesceCells = 2;
            const MoveCap = 2;

            // The class-blind licence term: 0 absent, 1 fresh, 2 present but
            // stale. One slot, exactly like every `*_hint: Option<Instant>`.
            var hint = 0;
            // A live `reflow_hint`/`blink_hint`: morphology, never a licence.
            var morphology = 0;
            // The live arm's key was swallowed at the host boundary.
            var swallowed = 0;
            // Presses that armed a licence, and the four dispositions an arm can
            // reach. `EveryArmReachesExactlyOneDisposition` is the conservation.
            var arms = 0;
            var consumed = 0;
            var expired = 0;
            var cleared = 0;
            var superseded = 0;
            // Moves that passed the licence seam (one per consumed arm).
            var admissions = 0;
            // The press CREDIT budget (`RainbowState::type_press_ring`).
            var credit_arms = 0;
            var spent = 0;
            var coalesce_births = 0;
            // Light: minted this run, and earned light already on glass.
            var births = 0;
            var resident = 0;
            // The slimmed admission ring (`ctl trail`).
            var spawns = 0;
            var licensed_tally = 0;
            var declined_tally = 0;
            // Witnesses. Every one of these is a shape the proof era or a bare
            // v0.43.0 revert really produces; all are 0 at the committed config.
            var cold_admitted = 0;
            var stale_admitted = 0;
            var morphology_admitted = 0;
            var swallow_admitted = 0;
            var wiped = 0;
            var credit_refunded = 0;

            // A press arms the licence and banks one cell credit. The stamp is
            // ONE SLOT: a press over a live stamp supersedes it (the older arm
            // is disposed of, its credit survives in the ring), which is why the
            // conservation has four dispositions and not two.
            action PressArmsLicense when (arms <= ArmCap - 1) {
                superseded = if hint == 1 { superseded + 1 } else { superseded };
                hint = 1;
                arms = arms + 1;
                credit_arms = credit_arms + 1;
                swallowed = 0;
            }
            // A resize settle or a TUI repaint blink. Live, and never a licence.
            action MorphologyStampArrives when (morphology == 0) {
                morphology = 1;
            }
            // The freshness window elapses: the stamp is still SET, and no
            // longer fresh. That distinction is the whole point of the window.
            action LicenseExpires when (hint == 1) {
                hint = 2;
                expired = expired + 1;
            }
            // The classifier's `take_if` drops the stale stamp. `Buggy=1`
            // refunds its cell to the press budget — the historical shape where
            // a retired hint's cells funded a second stray multi-cell sweep.
            action RetireStaleLicense when (hint == 2) {
                hint = 0;
                spent = if Buggy == 1 && spent > 0 { spent - 1 } else { spent };
                credit_refunded = if Buggy == 1 && spent > 0 {
                    1
                } else {
                    credit_refunded
                };
            }
            // `clear_move_license`: a key this window swallowed is not an answer
            // to the licence question, so its stamp must not outlive the
            // boundary. `Buggy=1` lets it survive.
            action SwallowedKeyClearsLicense when (hint == 1 && swallowed == 0) {
                hint = if Buggy == 1 { 1 } else { 0 };
                cleared = if Buggy == 1 { cleared } else { cleared + 1 };
                swallowed = 1;
            }
            // The licensed typed echo: one hint, one echo. `Buggy=1` leaves the
            // stamp live after consuming it, so one press funds every echo that
            // follows — caught twice over, by the disposition conservation and
            // by `admissions <= arms`.
            action LicensedTypedMoveMintsLight when (
                hint == 1 && spawns <= MoveCap - 1
            ) {
                spawns = spawns + 1;
                hint = if Buggy == 1 { 1 } else { 0 };
                consumed = consumed + 1;
                admissions = admissions + 1;
                births = births + 1;
                resident = 1;
                licensed_tally = licensed_tally + 1;
                swallow_admitted = if swallowed == 1 { 1 } else { swallow_admitted };
            }
            // A batched multi-cell echo, backed by enough recent CELL credits to
            // pay for every swept cell, SPENDS them: the same pool can never pay
            // for a second stray sweep. `Buggy=1` paints the ribbon for free.
            action AdmitCoalesceSpendsCredits when (
                hint == 1 && coalesce_births == 0 && spawns <= MoveCap - 1
                    && credit_arms - spent > CoalesceCells - 1
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
            // Licensed and classified, and the CREDIT budget alone refused it —
            // vim's `w` is one press echoing as a multi-cell hop. It stays dark
            // and pays nothing; the ring names `no-credits`. `Buggy=1` bills the
            // budget it could not afford AND tells `ctl trail` it painted.
            action StarvedCoalesceDeclines when (
                hint == 1 && spawns <= MoveCap - 1
                    && credit_arms - spent <= CoalesceCells - 1
            ) {
                spawns = spawns + 1;
                hint = 0;
                consumed = consumed + 1;
                admissions = admissions + 1;
                spent = if Buggy == 1 { spent + CoalesceCells } else { spent };
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
            // THE COLD MOVE: program output nobody's fingers asked for. No
            // stamp at all. `Buggy=1` is the pre-licence seam — v0.43.0's tick,
            // which spawned on every presented cursor delta.
            action ColdMoveDeclines when (hint == 0 && spawns <= MoveCap - 1) {
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
            // The same cold move, over light the user's own typing earned.
            // `Buggy=1` is `clear_denied_move_visuals`: it refuses to mint AND
            // wipes the ribbon, invisibly to the ring. That wipe is the darkness.
            action ColdMoveOverEarnedLight when (
                hint == 0 && resident == 1 && spawns <= MoveCap - 1
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
            // A move under a live reflow/blink stamp and nothing else. The
            // stamp is consumed as morphology; it never licenses.
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
            // A move whose stamp is SET but stale. The window, not the field.
            action StaleStampMoveDeclines when (
                hint == 2 && spawns <= MoveCap - 1
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
            invariant EveryArmReachesExactlyOneDisposition:
                arms == consumed + expired + cleared + superseded
                    + (if hint == 1 { 1 } else { 0 });
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
            invariant StateBounded:
                hint <= 2 && morphology <= 1 && swallowed <= 1 && arms <= ArmCap
                    && consumed <= ArmCap && expired <= ArmCap
                    && cleared <= ArmCap && superseded <= ArmCap
                    && admissions <= MoveCap && credit_arms <= ArmCap
                    && spent <= ArmCap + CoalesceCells && coalesce_births <= 1
                    && births <= MoveCap && resident <= 1 && spawns <= MoveCap
                    && licensed_tally <= MoveCap && declined_tally <= MoveCap
                    && cold_admitted <= 1 && stale_admitted <= 1
                    && morphology_admitted <= 1 && swallow_admitted <= 1
                    && wiped <= 1 && credit_refunded <= 1;
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

/// Resident cursor-pet coordinates are owned by one front terminal surface.
/// A pane/tab owner change, a same-terminal main/alternate-screen change, or a
/// focus loss that leaves cursor effects truly unpresentable retires both the
/// visible body and its host-owned hit target.
/// The pet's durable identity survives so the next lawful sighting uses the
/// same companion. Raw focus loss is not itself a retirement proof: a live
/// typed-wake or recording pin keeps the same presentation surface admitted.
///
/// `Buggy = 1` reproduces the retained-coordinate defect: retiring boundaries
/// keep the old body and hit target, allowing them to jump into a replacement
/// pane/tab or remain interactable after presentation has stopped. The pinned
/// focus-loss actions are independent negative controls and remain visible in
/// both laws, so the repair cannot be "retire on every raw blur".
///
/// This model projects only the resident `PetBrain` body and the GUI's paired
/// hit target. The ordinary flying cursor cat has a distinct earned/promise
/// lifecycle; its owner-switch retirement is a separately asserted host rule
/// in the Tier-1 GUI regression, not one of these variables.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn cursor_companion_owner_lifecycle_model() -> Model {
    crate::ty_model! {
        CursorCompanionOwnerLifecycle {
            const Buggy = 0;
            var phase = 0;
            // The model begins with a configured durable identity but no
            // surface-relative sighting. 1 means that identity is retained.
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
            }
            action RecordingFocusLoss when (phase == 1) {
                phase = 6;
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
            invariant CompanionOwnerValuesBounded:
                phase <= 7 && pet_visible <= 1 && hit_target <= 1
                    && durable_identity <= 1;
        }
    }
}

/// A composed frame owns one DEC-2026 hold machine per visible terminal. One
/// pane completing its bracket cannot release a sibling pane whose bracket is
/// still incomplete; only a frame where every member releases may present.
///
/// `Buggy = 1` is the retired aggregate-counter defect: the first pane close
/// presents the composite even though the other pane remains held.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn composed_sync_hold_model() -> Model {
    crate::ty_model! {
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
            invariant ComposedSyncValuesBounded:
                phase <= 5 && a_hold <= 1 && b_hold <= 1 && presented <= 1;
        }
    }
}

/// A DEC-2026 close edge licenses the last completed episode for presentation.
/// If the terminal immediately opens a new episode, that completed boundary may
/// remain visible while the new episode is still clean. The first completed
/// parser action in the reopened episode sets `open_dirty`; from that point the
/// host must hold the partial episode until its matching close.
///
/// `Buggy = 1` is the close/reopen race that treats the close sequence alone as
/// a presentation license. It keeps presenting after the reopened episode has
/// become dirty, exposing cells that belong to an incomplete synchronized
/// update.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn sync_reopen_visibility_model() -> Model {
    crate::ty_model! {
        SyncReopenVisibility {
            const Buggy = 0;
            // Begin inside a dirty first episode. The modeled close/reopen edge
            // is the exact boundary at which the stale close-sequence-only
            // license used to lose track of the new episode's writes.
            var phase = 0;
            var sync_active = 1;
            var open_dirty = 1;
            var hold = 1;
            var completed_generation = 0;
            var presented_generation = 0;
            var partial_visible = 0;

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
                // The new episode has performed no presented mutation, so the
                // completed close boundary remains a lawful frame.
                presented_generation = 1;
            }
            action DirtyReopenedEpisode when (phase == 2) {
                phase = 3;
                open_dirty = 1;
                hold = if Buggy == 1 { 0 } else { 1 };
                partial_visible = if Buggy == 1 { 1 } else { 0 };
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
            invariant SyncReopenValuesBounded:
                phase <= 4 && sync_active <= 1 && open_dirty <= 1 && hold <= 1
                    && completed_generation <= 2 && presented_generation <= 2
                    && partial_visible <= 1;
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
/// full-screen upward motion on either active screen and `epoch` for
/// region/reset/restore mutations whose
/// coordinates cannot be transformed as one plane. `retained_history` stays
/// zero in every state, modeling both a zero-history terminal and a saturated
/// ring whose public count no longer changes.
///
/// `UniformAtCap` must still reach one exact translation and retire row-bound
/// proof. `AltUniform` must make the same exact translation: changing active
/// screen invalidates independently, but an in-screen full-grid shift is not
/// ambiguous. Region, reset and in-place restore events must instead choose
/// invalidation and drop all geometry/proof. `Buggy=1` is the former
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
            var event = 0;              // 0 initial, 1 primary uniform, 2 region, 3 alt uniform, 4 reset, 5 restore
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
                } else {
                    survivor_y == StartY
                };
            invariant AmbiguousMotionInvalidates:
                if event == 2 || event == 4 || event == 5 {
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
