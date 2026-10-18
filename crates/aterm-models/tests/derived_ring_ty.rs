// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-0 for `ty_model!` models (`aterm_spec::derive`'s module docs): each
//! model's generated TLA+ (`Model::to_tla()` / `.to_cfg()`) is exhaustively
//! checked over its bounded state space, so a model change re-checks the spec.
//!
//! Two tiers ([`aterm_spec::verify`]): the in-process interpreter always
//! discharges every obligation, and the external `ty` re-checks it wherever it
//! is installed. The tiers must agree; disagreement panics.

#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]
use aterm_models_core::alt_archive_pool_model;
use aterm_models_effects::{
    erased_key_echo_order_model, flash_limiter_model, flash_limiter_window_model,
    same_caret_typed_echo_model,
};
use aterm_models_release::release_publish_once_model;
use aterm_models_session::{forward_handshake_model, session_id_claim_model};
use aterm_spec::derive::Model;
use aterm_spec::verify;

// --- What the registry sweeps do not cover ---
//
// A REGISTERED model needs no test here, and every model written with
// `ty_model!` or `#[aterm_spec::model]` registers itself: `tests/non_vacuity.rs`
// holds each one, in every configuration it declares, to `verify::sweep_faults`
// (it holds with no mutant on, and each of its claims is broken by a `Buggy`
// mutant), and `spec-check models` walks the same space with `ty`. What is left
// is what that sweep does not ask: deadlock freedom, a scenario matrix whose
// corners must hold or break, the multiplication defect of a scope-aggregate
// model, and a law that must break alone, pinned by name with the paths it is
// about.

/// NO WEDGE: deadlock-free at `Buggy = 0` and a DEADLOCK (not an invariant
/// violation) at `Buggy = 1`, on both tiers — the blocking-call class (the
/// `drain_buffered` `fill_buf` hang) no reachable-bad-STATE invariant can see.
/// `is_final` marks the legitimate work-complete terminal.
#[test]
fn designs_that_must_not_wedge_are_deadlock_free_and_catch_the_wedge() {
    type Final = fn(&aterm_spec::interp::State) -> bool;
    let mut release_publish_once = release_publish_once_model();
    release_publish_once.invariants.clear();
    let rows: [(Model, Final); 2] = [
        (forward_handshake_model(), |s| {
            s.get("client_waiting") == Some(&0)
        }),
        (release_publish_once, |s| s["phase"] == 5 || s["phase"] == 6),
    ];
    for (m, is_final) in rows {
        verify::deadlock_free_and_catches_tiered(&m, is_final, m.name);
    }
}

/// Sparkle-words v2 flash limiter (design §6.4/§9): WCAG 2.3.1, model-checked.
/// The interpreter PROVES `IgnitionBound` (≤ 2 ignitions per rolling second, ≤ 1 under
/// overlap) and the REGION-scoped `RegionFlashPairs ≤ 3` at `Buggy = 0` for
/// BOTH `Overlap ∈ {0, 1}`, and CATCHES the overlap-blind limiter at
/// `Buggy = 1, Overlap = 1` (two overlapping ignitions in one second ⇒ 4
/// transition pairs on the shared region). `Buggy = 1, Overlap = 0` stays
/// green by design — disjoint novas at 2/s are legal, which is why the pair
/// bound is per-region, not window-global (the §9 binding-spec erratum).
#[test]
fn derived_flash_limiter_proves_and_catches_overlap_blindness() {
    for (buggy, overlap, holds) in [(0, 0, true), (0, 1, true), (1, 0, true), (1, 1, false)] {
        let m = aterm_spec::interp::with_consts(
            &flash_limiter_model(),
            &[("Buggy", buggy), ("Overlap", overlap)],
        );
        assert_eq!(
            aterm_spec::interp::bmc(&m).is_ok(),
            holds,
            "FlashLimiter (Buggy={buggy}, Overlap={overlap})"
        );
    }
}

/// Scope-aggregate models: a budget charged against EVERY live instance at
/// once — the window's WCAG flash budget (`FlashLimiterWindow`, the
/// machine-checked form of the scope-cardinality census's `flash-limiter`
/// claim), the process's alt-screen archive pool (`AltArchivePool`), one live
/// holder per session id (`SessionIdClaim`). A per-instance theorem stays true
/// however many instances exist, so each model also carries `Local = 1` (every
/// instance deciding alone), which must break it at every corner: the
/// multiplication defect (`verify::prove_catch_and_multiply_scalar`'s five
/// gates).
#[test]
fn scope_aggregate_models_prove_catch_and_multiply() {
    type Corner = &'static [(&'static str, i64)];
    let rows: [(Model, &[Corner], Corner); 3] = [
        // An overlap-blind limiter misbehaves only where regions coincide, but
        // multiplying enforcers is wrong at both `Overlap` corners.
        (
            flash_limiter_window_model(),
            &[&[], &[("Overlap", 1)]],
            &[("Overlap", 1)],
        ),
        (alt_archive_pool_model(), &[&[]], &[]),
        // The lock goes free across a seamless handoff while the successor's
        // own entry holds the id, so the entry gate's mutant shows only there.
        (
            session_id_claim_model(),
            &[&[], &[("Handoff", 1)]],
            &[("Handoff", 1)],
        ),
    ];
    for (m, scenarios, catch_at) in rows {
        let _ = verify::prove_catch_and_multiply_scalar(&m, scenarios, catch_at, m.name);
    }
}

/// A model using the `Buggy` convention: the invariant must be PROVEN at the
/// committed `Buggy=0`, and a COUNTEREXAMPLE found at `Buggy=1` — so the
/// invariant is non-trivial AND genuinely catches the bug
/// ([`verify::prove_and_catch_scalar`]; a function-valued model is
/// `spec-check models`'s, under `ty`).
fn assert_proves_and_catches(m: &Model) {
    verify::prove_and_catch_scalar(m, m.name);
}

/// Every design invariant must be the ONLY law some `Buggy = 1` step breaks — a
/// transition out of a reachable state where every invariant still holds, into
/// one where that law, and no other, fails.
///
/// Isolation alone accepts an invariant that is only ever reached through a
/// state another law already refused: an inline `Buggy` arm that makes one
/// slip unavoidable before the next can fire, or a mutant whose harm is a
/// consequence of an earlier one. Being among the laws a first step breaks is
/// not enough either: a law that repeats clauses of others breaks on exactly
/// their steps, never alone, and its catch is theirs. Such a law carries no
/// catch of its own, and the per-invariant ratchet cannot see that. This walks
/// the `Buggy = 1` space from every all-good state and demands that each design
/// law be the sole law some single step breaks.
fn assert_every_invariant_breaks_first(m: &Model) {
    let buggy = aterm_spec::interp::with_buggy(m, 1);
    // Walked resolved (`aterm_spec::interp::Resolved`): the same states and
    // verdicts as `successors` / `check_invariant` by name, without the
    // string-keyed map each of those builds per call.
    let r = aterm_spec::interp::Resolved::new(&buggy).expect("the model resolves");
    let laws = 0..buggy.invariants.len();
    let mut alone: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut seen = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    let init = r.init();
    seen.insert(init.clone());
    queue.push_back(init);
    while let Some(st) = queue.pop_front() {
        assert!(
            seen.len() < 100_000,
            "{}: Buggy=1 good space unbounded",
            m.name
        );
        for a in 0..buggy.actions.len() {
            for next in r.successors(a, &st) {
                if laws.clone().all(|i| r.holds(i, &next)) {
                    if !seen.contains(&next) {
                        seen.insert(next.clone());
                        queue.push_back(next);
                    }
                } else {
                    let broken: Vec<&str> = laws
                        .clone()
                        .filter(|&i| !r.holds(i, &next))
                        .map(|i| buggy.invariants[i].name)
                        .collect();
                    if let [only] = broken.as_slice() {
                        alone.insert(only);
                    }
                }
            }
        }
    }
    let shared: Vec<&str> = m
        .invariants
        .iter()
        .filter(|inv| inv.kind == aterm_spec::derive::InvariantKind::Claim)
        .map(|inv| inv.name)
        .filter(|name| !alone.contains(name))
        .collect();
    assert!(
        shared.is_empty(),
        "{}: {shared:?} never break alone — every Buggy=1 step from an all-good state \
         that breaks them breaks another law too (or they break only after another law \
         already broke), so their catch is some other law's",
        m.name
    );
}

/// The committed-dead actions of `m` are exactly `expected`, and each is an
/// independently caught mutant (`verify::audit_dead_negative_controls`: the
/// all-live `Buggy=1` baseline is safe, and each dead action, added back ALONE,
/// fires and is caught). This is the interpreter half of the `aterm-gui` gate's
/// `ty --strict-vacuity` audit, pinned where the mutants are written, so a mutant
/// folded back into a live action — which masks its neighbours and removes a
/// healthy path from the `Buggy=1` world — fails here by name.
fn assert_committed_dead_are_caught_mutants(m: &Model, expected: &[&str]) {
    let healthy_fired = aterm_spec::interp::fired_actions(&aterm_spec::interp::with_buggy(m, 0));
    let dead: Vec<_> = m
        .actions
        .iter()
        .map(|action| action.name)
        .filter(|name| !healthy_fired.contains(name))
        .collect();
    assert_eq!(dead, expected, "{}: committed-dead actions", m.name);
    assert_eq!(
        verify::audit_dead_negative_controls(std::slice::from_ref(m)),
        Ok(expected.len()),
        "{}: every committed-dead action must remain an independently caught mutant",
        m.name
    );
}

#[test]
fn derived_same_caret_typed_echo_requires_exact_oldest_key_and_fresh_probe() {
    let model = same_caret_typed_echo_model();
    assert_proves_and_catches(&model);

    let mut exact = model.init_state();
    for action in ["BankA", "BankN", "ExactA", "Resolve"] {
        assert!(model.fire(action, &mut exact));
    }
    assert_eq!(
        exact["admitted"], 1,
        "later typeahead `n` replaced oldest `a`"
    );
    assert_eq!(exact["spent"], 1);
    let mut delayed = model.init_state();
    for action in ["BankA", "DelayedExactA", "Resolve"] {
        assert!(model.fire(action, &mut delayed));
    }
    assert_eq!(delayed["admitted"], 1, "a live delayed exact echo was dark");
    assert_eq!(delayed["spent"], 1);
    let mut expired = model.init_state();
    for action in ["BankA", "ExpiredExactA", "Resolve"] {
        assert!(model.fire(action, &mut expired));
    }
    assert_eq!(expired["admitted"], 0, "an expired key claimed a print");
    for refusal in [
        "AmbientOtherGlyph",
        "DelayedAmbientOtherGlyph",
        "EarlierMatchingPaint",
        "NoRowProbe",
        "NoHandOwner",
    ] {
        let mut state = model.init_state();
        for action in ["BankA", refusal, "Resolve"] {
            assert!(model.fire(action, &mut state));
        }
        assert_eq!(state["admitted"], 0, "{refusal} claimed a key");
    }
    let mut batch = model.init_state();
    for action in ["BankA", "BankN", "ExactBatch", "Resolve"] {
        assert!(model.fire(action, &mut batch));
    }
    assert_eq!(
        batch["admitted"], 1,
        "the exact in-flight batch was refused"
    );
    for refusal in ["BatchOtherGlyph", "BatchNoPriorProbe", "BatchStaleTail"] {
        let mut state = model.init_state();
        for action in ["BankA", "BankN", refusal, "Resolve"] {
            assert!(model.fire(action, &mut state));
        }
        assert_eq!(state["admitted"], 0, "{refusal} claimed a key");
    }
    let mut keyless = model.init_state();
    for action in ["KeylessPaint", "Resolve"] {
        assert!(model.fire(action, &mut keyless));
    }
    assert_eq!(keyless["admitted"], 0);
    let mut batch_keyless = model.init_state();
    for action in ["BatchKeyless", "Resolve"] {
        assert!(model.fire(action, &mut batch_keyless));
    }
    assert_eq!(batch_keyless["admitted"], 0);
    for retype in ["RetypedInPlace", "RetypedPrinted"] {
        let mut state = model.init_state();
        for action in ["BankA", retype, "Resolve"] {
            assert!(model.fire(action, &mut state));
        }
        assert_eq!(
            state["admitted"], 1,
            "{retype}: a key retyped in place was dark"
        );
        assert_eq!(state["spent"], 1);
    }
    let mut late = model.init_state();
    for action in ["BankA", "RetypedOverLateEcho", "Resolve"] {
        assert!(model.fire(action, &mut late));
    }
    assert_eq!(
        late["admitted"], 1,
        "a key retyped over the erased key's late echo was dark"
    );
    let owned_only = aterm_spec::interp::with_buggy(&model, 6);
    let mut late = owned_only.init_state();
    for action in ["BankA", "RetypedOverLateEcho", "Resolve"] {
        assert!(owned_only.fire(action, &mut late));
    }
    assert_eq!(
        late["admitted"], 0,
        "the hand-cell law lights the late echo"
    );
    let mut replaced = model.init_state();
    for action in ["BankA", "ReplacedNoErase", "Resolve"] {
        assert!(model.fire(action, &mut replaced));
    }
    assert_eq!(
        replaced["admitted"], 0,
        "a glyph replaced with no erase claimed a key"
    );
}

#[test]
fn derived_erased_key_echo_order_keeps_the_next_key_lit() {
    let model = erased_key_echo_order_model();
    assert_proves_and_catches(&model);
    // The owner's take: `s` erased before its echo, the Space typed, the late
    // `s` echo, then the retreat and the Space apart or as one repaint.
    let apart = [
        "EraseUnechoed",
        "NextKey",
        "LateEcho",
        "Retreat",
        "NextEcho",
    ];
    let merged = ["EraseUnechoed", "NextKey", "LateEcho", "MergedCorrection"];
    for path in [&apart[..], &merged[..]] {
        let mut state = model.init_state();
        for action in path {
            assert!(model.fire(action, &mut state), "{path:?}: {action}");
        }
        assert_eq!(state["lit"], 1, "{path:?}: the next key was dark");
        assert_eq!(state["late_paid"], 0);
        for (buggy, law) in [(1, "NoPaidLateEcho"), (2, "NextKeyLit")] {
            let bug = aterm_spec::interp::with_buggy(&model, buggy);
            let mut state = bug.init_state();
            for action in path {
                assert!(bug.fire(action, &mut state));
            }
            assert!(
                !bug.check_invariant(law, &state),
                "{path:?}: Buggy = {buggy} keeps {law}"
            );
        }
    }
}

/// The session log's ring (`docs/DESIGN-session-log-2026-10-15.md` §5.1,
/// §9; phase 2A): each law is the only one some single
/// `Buggy = 1` step breaks, every mutant is a dead action at the committed
/// config that is caught on its own, and the paths the laws are about are
/// reachable: a batch takes a closed turn's detail before its open and keeps
/// its close, the open path alone may overfill the ring by the one record just
/// appended, an orphan step outlives a turn beside it, and a run closed
/// mid-turn cascades over its turn and step. Tier-1: aterm-slog's
/// `slog_ring_conforms_to_derived_model`.
#[test]
fn derived_session_log_ring_proves_and_catches_each_law() {
    let model = aterm_models_session::session_log_ring_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_are_caught_mutants(
        &model,
        &[
            "AppendNoEvict",
            "EvictOpenRecord",
            "EvictCloseBeforeDetail",
            "EvictUncounted",
            "CloseParentLeaveChild",
            "StepUnderClosedTurn",
            "CloseTwice",
            "HandoffReuse",
            "HandoffForgetsOpen",
            "CarryShedUncounted",
            "CrashSameEpoch",
            "RefoldFromRing",
        ],
    );
    // Fire `action`, then the batch it armed, one eviction at a time.
    let settle = |m: &aterm_spec::derive::Model, s: &mut aterm_spec::interp::State| {
        for _ in 0..16 {
            if s["ev"] == 0 {
                return;
            }
            let fired = [
                "EvictOldDetail",
                "EvictTurnDetail",
                "EvictClosedOpen",
                "EvictClose",
                "EvictTurnClose",
                "EvictCurrent",
                "EvictCurrentOut",
                "EvictDone",
            ]
            .into_iter()
            .find(|a| m.fire(a, s));
            assert!(fired.is_some(), "a batch with no step: {s:?}");
        }
        panic!("a batch that never ends: {s:?}");
    };

    // A closed turn's detail goes first, then its open; its close stays.
    let mut s = model.init_state();
    for action in ["OpenRun", "OpenTurn", "Mark", "CloseTurn"] {
        assert!(model.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["ev"], s["tdet"], s["ocl"], s["cls"]), (1, 1, 1, 1));
    assert!(!model.action_enabled("EvictClosedOpen", &s), "detail first");
    settle(&model, &mut s);
    assert_eq!(
        (
            s["tdet"], s["ocl"], s["cls"], s["tcl"], s["prun"], s["lost"]
        ),
        (0, 0, 1, 1, 1, 2),
        "the close outlives its detail and its open"
    );

    // The open path alone may overfill the ring by the record just appended.
    let mut p = model.init_state();
    for action in ["OpenRun", "OpenTurn", "OpenStep", "Mark"] {
        assert!(model.fire(action, &mut p), "{action}: {p:?}");
        settle(&model, &mut p);
    }
    assert_eq!(
        (p["prun"], p["pturn"], p["pstep"], p["dturn"], p["lost"]),
        (1, 1, 1, 1, 0),
        "nothing to take: the open path and the newest stay"
    );

    // An orphan step on the run outlives a turn opened and closed beside it;
    // the run's close then cascades over it (a wider `MaxSeq` leaves room).
    let wide = aterm_spec::interp::with_consts(&model, &[("MaxSeq", 9)]);
    let mut o = wide.init_state();
    for action in [
        "OpenRun",
        "OpenOrphanStep",
        "OpenTurn",
        "CloseTurn",
        "CloseRunCascade",
    ] {
        assert!(wide.fire(action, &mut o), "{action}: {o:?}");
        settle(&wide, &mut o);
    }
    assert_eq!(
        (o["run"], o["turn"], o["step"], o["sorph"], o["tclose"]),
        (0, 0, 0, 0, 1)
    );
    assert_eq!(o["seq"], 6);
    // A run closed over its open turn and step writes three records.
    let mut c = wide.init_state();
    for action in ["OpenRun", "OpenTurn", "OpenStep", "CloseRunCascade"] {
        assert!(wide.fire(action, &mut c), "{action}: {c:?}");
        settle(&wide, &mut c);
    }
    assert_eq!((c["seq"], c["tclose"], c["tn"], c["fturns"]), (6, 1, 1, 1));

    // An update that carries no records: the manifest draws the count, the
    // outgoing process writes on within the reserve, and the successor's
    // seqs start above everything it wrote. Past the reserve the Commit
    // stands down.
    let mut u = model.init_state();
    for action in ["OpenRun", "Mark", "Draw", "Mark"] {
        assert!(model.fire(action, &mut u), "{action}: {u:?}");
        settle(&model, &mut u);
    }
    assert!(
        model.action_enabled("HandoffUncarried", &u),
        "at the reserve"
    );
    let mut over = u.clone();
    assert!(model.fire("Mark", &mut over));
    settle(&model, &mut over);
    assert!(!model.action_enabled("HandoffUncarried", &over), "past it");
    assert!(model.fire("StandDown", &mut over));
    assert!(model.fire("HandoffUncarried", &mut u));
    assert_eq!(
        (u["hi"], u["seq"], u["first"], u["ep"], u["run"]),
        (3, 4, 4, 1, 0),
        "the successor's gap at the drawn count plus one"
    );

    // A carry cannot shed its mandatory path to fit a hard record bound.
    // The uncarried handoff remains available at that same manifest.
    let mut too_wide = model.init_state();
    for action in ["OpenRun", "OpenTurn", "OpenStep", "Draw"] {
        assert!(model.fire(action, &mut too_wide), "{action}: {too_wide:?}");
        settle(&model, &mut too_wide);
    }
    assert!(!model.action_enabled("HandoffCarried", &too_wide));
    assert!(model.action_enabled("HandoffUncarried", &too_wide));
    assert!(
        !aterm_spec::interp::with_buggy(&model, 1).action_enabled("HandoffForgetsOpen", &too_wide)
    );

    // A carried update: the open run stays open, the carry sheds in the
    // eviction order down to its bound (one of the two marks, counted), and
    // the successor's handoff mark lands above the reserve, in the same seq
    // space.
    let mut c = model.init_state();
    for action in ["OpenRun", "Mark", "Mark", "Draw"] {
        assert!(model.fire(action, &mut c), "{action}: {c:?}");
        settle(&model, &mut c);
    }
    assert!(model.fire("HandoffCarried", &mut c));
    assert_eq!((c["ev"], c["cv"]), (1, 1), "the carry's shed");
    for _ in 0..8 {
        if c["cv"] == 0 {
            break;
        }
        let fired = [
            "EvictOldDetail",
            "EvictTurnDetail",
            "EvictClosedOpen",
            "EvictClose",
            "EvictTurnClose",
            "EvictCurrent",
            "EvictCurrentOut",
            "CarryDone",
        ]
        .into_iter()
        .find(|a| model.fire(a, &mut c));
        assert!(fired.is_some(), "a carry with no step: {c:?}");
    }
    assert_eq!(
        (c["run"], c["prun"], c["ep"], c["lost"]),
        (1, 1, 1, 1),
        "the run crosses open; one mark shed, counted"
    );
    assert_eq!(
        (c["seq"], c["holes"], c["first"]),
        (5, 1, 1),
        "the mark above the reserve; the seq space goes on"
    );

    // A crash adoption: a new seq space from 1, at an epoch above the last.
    let mut k = model.init_state();
    for action in ["OpenRun", "Mark"] {
        assert!(model.fire(action, &mut k), "{action}: {k:?}");
    }
    assert!(model.fire("Crash", &mut k));
    assert_eq!(
        (
            k["seq"],
            k["first"],
            k["hi"],
            k["ep"],
            k["eplast"],
            k["run"]
        ),
        (1, 1, 0, 1, 0, 0),
        "the gap why=crash at seq 1, the epoch raised"
    );
}

/// THE BUS MIRROR (design §5.5, phase 4): each of the reader's-view laws is
/// the only one some single `Buggy = 1` step breaks, every mutant is dead at
/// the committed config and caught on its own, and the paths are reachable:
/// a hole said by one cover; a slot pinned under the update's fence that no
/// landing passes, landed by the successor only after it adopted the session
/// (ruling Q1), its carried cursor moved past the pin; another writer's record
/// at the pinned seq re-pinned, never landed (ruling Q3). Tier-1: aterm-link's
/// `log_mirror_conforms_to_derived_model`.
#[test]
fn derived_log_mirror_proves_and_catches_each_law() {
    let model = aterm_models_session::log_mirror_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_are_caught_mutants(
        &model,
        &[
            "DropUnadopted",
            "TailPastFence",
            "SkipGap",
            "AckBeforePublish",
            "TrustDeduped",
            "LandUnderFence",
            "CloseDropsTail",
        ],
    );
    // A session that closes under the update's fence, its close the record
    // past the fence: the Commit stands down (it needs every session it
    // drew), and once the fence is lifted the closed session's tail lands
    // whole.
    let mut c = model.init_state();
    for action in [
        "Append",
        "Append",
        "TakeRecord",
        "Publish",
        "Landed",
        "Unpin",
        "Export",
        "Close",
    ] {
        assert!(model.fire(action, &mut c), "{action}: {c:?}");
    }
    assert!(
        !model.action_enabled("Commit", &c),
        "a closed session stands the Commit down"
    );
    assert!(
        !model.action_enabled("Append", &c),
        "closed: nothing more is appended"
    );
    for action in [
        "Rollback",
        "TakeRecord",
        "Publish",
        "Landed",
        "Unpin",
        "TakeRecord",
        "Publish",
        "Landed",
        "Unpin",
    ] {
        assert!(model.fire(action, &mut c), "{action}: {c:?}");
    }
    assert_eq!(
        (c["landed"], c["rec1"], c["rec2"], c["rec3"]),
        (3, 1, 1, 1),
        "the closed session's tail, all of it on the bus"
    );
    // A hole between two live records: one cover, then the record after it.
    let mut g = model.init_state();
    for action in [
        "Append",
        "Append",
        "Append",
        "TakeRecord",
        "Publish",
        "Landed",
        "Unpin",
        "Evict2",
        "TakeGap",
    ] {
        assert!(model.fire(action, &mut g), "{action}: {g:?}");
    }
    assert_eq!((g["plo"], g["phi"], g["pgap"]), (2, 2, 1), "the hole alone");
    for action in [
        "Publish",
        "Landed",
        "Unpin",
        "TakeRecord",
        "Publish",
        "Landed",
        "Unpin",
    ] {
        assert!(model.fire(action, &mut g), "{action}: {g:?}");
    }
    assert_eq!(
        (g["rec1"], g["cov2"], g["rec2"], g["rec3"], g["landed"]),
        (1, 1, 0, 1, 3),
        "said once each, dense"
    );
    // The update: a slot pinned and published under the fence is refused its
    // landing; the successor lands it only once it has adopted the session.
    let mut u = model.init_state();
    for action in [
        "Append",
        "Append",
        "TakeRecord",
        "Publish",
        "Landed",
        "Unpin",
        "Export",
        "TakeRecord",
        "Publish",
    ] {
        assert!(model.fire(action, &mut u), "{action}: {u:?}");
    }
    assert!(!model.action_enabled("Landed", &u), "the fence refuses it");
    assert!(model.fire("Commit", &mut u));
    assert_eq!(
        (u["landed"], u["pin"], u["adopted"]),
        (1, 1, 0),
        "the drawn cursor, the pin kept"
    );
    assert!(
        !model.action_enabled("Landed", &u),
        "busy adopting: the pin stays"
    );
    assert!(!model.action_enabled("TakeRecord", &u) && !model.action_enabled("TakeGap", &u));
    for action in ["Evict2", "AdoptLate", "Landed", "Unpin"] {
        assert!(model.fire(action, &mut u), "{action}: {u:?}");
    }
    assert_eq!(
        (u["landed"], u["rec2"], u["cov2"]),
        (2, 1, 0),
        "landed past the shed record, never covered"
    );
    // Another writer's record at the pinned seq: re-pinned, then published.
    let mut f = model.init_state();
    for action in ["Append", "TakeRecord", "Foreign"] {
        assert!(model.fire(action, &mut f), "{action}: {f:?}");
    }
    assert!(!model.action_enabled("Publish", &f) && !model.action_enabled("Landed", &f));
    for action in ["Repin", "Publish", "Landed", "Unpin"] {
        assert!(model.fire(action, &mut f), "{action}: {f:?}");
    }
    assert_eq!((f["rec1"], f["landed"]), (1, 1));
}

/// The session log's reader (§5.1, phase 2A): each law is the
/// only one some single `Buggy = 1` step breaks, every mutant is dead at the
/// committed config and caught on its own, and the paths are reachable: a
/// read across a hole is told so and counts it as no loss, and a read cut at
/// a live record past an eviction in the middle counts the eviction once.
/// Tier-1: aterm-slog's `slog_cursor_conforms_to_derived_model`.
#[test]
fn derived_session_log_cursor_proves_and_catches_each_law() {
    let model = aterm_models_session::session_log_cursor_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_are_caught_mutants(
        &model,
        &[
            "ReadSkipsEvicted",
            "ReadCountsHole",
            "ReadIgnoresHole",
            "ReadPastFence",
        ],
    );
    let mut s = model.init_state();
    for action in ["Append", "Hole", "Append", "ReadAll"] {
        assert!(model.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!(
        (s["cur"], s["got"], s["rlost"], s["told"]),
        (3, 2, 0, 1),
        "a hole is said, never counted lost"
    );
    let mut m = model.init_state();
    for action in ["Append", "Append", "Append", "Evict2", "ReadTo3"] {
        assert!(model.fire(action, &mut m), "{action}: {m:?}");
    }
    assert_eq!((m["cur"], m["got"], m["rlost"]), (3, 2, 1));
    assert!(model.fire("Append", &mut m));
    assert!(model.fire("ReadAll", &mut m));
    assert_eq!((m["cur"], m["got"], m["rlost"]), (4, 3, 1), "told once");

    // The overlap: a reader fenced at the carry reads nothing the outgoing
    // process wrote after it; once committed those seqs are holes, said.
    let mut f = model.init_state();
    for action in ["Append", "Append", "Export", "OverlapAppend", "ReadAll"] {
        assert!(model.fire(action, &mut f), "{action}: {f:?}");
    }
    assert_eq!((f["cur"], f["got"], f["ov"]), (2, 2, 0), "the fence holds");
    assert!(!model.action_enabled("ReadTo3", &f));
    for action in ["Commit", "Append", "ReadAll"] {
        assert!(model.fire(action, &mut f), "{action}: {f:?}");
    }
    assert_eq!(
        (f["cur"], f["got"], f["rlost"], f["told"], f["ov"]),
        (4, 3, 0, 1, 0),
        "the overlap's seq is a hole, said and not counted"
    );
}

/// The journal reader's view (phase 2B, ruling R12): each of its five laws
/// is the only one some single `Buggy = 1` step breaks, every mutant is dead
/// at the committed config and caught on its own, and the paths: a line
/// journaled twice reads once, a dead chain reads its crash closes innermost
/// first (turn, run, session) after a crash gap at the journal's head, a lost
/// run's turn closes before the session, a loss is said, a crash with a
/// backlog and a snap written behind says `after=` the writer's cursor, a
/// `#handed` chain reads open and a rolled-back one closed. Tier-1:
/// aterm-slog's `slog_journal_view_conforms_to_derived_model`.
#[test]
fn derived_session_log_journal_proves_and_catches_each_law() {
    let model = aterm_models_session::session_log_journal_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_are_caught_mutants(
        &model,
        &[
            "ReadNoDedupe",
            "ReadNoCrashClose",
            "ReadHandedSticky",
            "ReadParentFirst",
            "ReadHidesLoss",
            "ReadAfterRestated",
        ],
    );
    let fire = |s: &mut aterm_spec::interp::State, path: &[&str]| {
        for action in path {
            assert!(model.fire(action, s), "{action}: {s:?}");
        }
    };
    let mut s = model.init_state();
    fire(
        &mut s,
        &[
            "Append", "Write", "Append", "Write", "Again2", "Append", "Write", "Read",
        ],
    );
    assert_eq!((s["v1"], s["v2"], s["v3"]), (1, 1, 1), "each seq once");
    assert_eq!(
        (s["vs"], s["vr"], s["vt"]),
        (1, 1, 1),
        "a live chain reads open"
    );
    fire(&mut s, &["Crash", "Read"]);
    assert_eq!(
        (s["ct"], s["cr"], s["cs"], s["vs"], s["ca"]),
        (1, 2, 3, 0, 3),
        "a dead chain closes innermost first, after its head"
    );
    let mut l = model.init_state();
    fire(
        &mut l,
        &[
            "Append", "Write", "Append", "Lose", "Append", "Write", "Crash", "Read",
        ],
    );
    assert_eq!(
        (l["vlost"], l["ct"], l["cr"], l["cs"]),
        (1, 1, 0, 2),
        "a lost run"
    );
    // Behind: the log at 3, the writer at 1, a snap written there; the crash
    // gap says the writer's cursor.
    let mut b = model.init_state();
    fire(
        &mut b,
        &[
            "Append", "Write", "Append", "Append", "Snap", "Crash", "Read",
        ],
    );
    assert_eq!((b["sb"], b["ca"], b["vs"]), (3, 1, 0), "after= the cursor");
    // Handed, then the update stood down (a snap voids it), then a crash.
    let mut h = model.init_state();
    fire(&mut h, &["Append", "Write", "Hand", "Crash", "Read"]);
    assert_eq!((h["vs"], h["ca"]), (1, 0), "handed on: open, no crash");
    let mut r = model.init_state();
    fire(
        &mut r,
        &["Append", "Write", "Hand", "Snap", "Crash", "Read"],
    );
    assert_eq!((r["vs"], r["cs"], r["ca"]), (0, 1, 1), "stood down: closed");
}

/// The mark quota (phase 3A, §7.4; ruling R4): each of its four laws is the
/// only one some single `Buggy = 1` step breaks, every mutant is dead at the
/// committed config and caught on its own, and the paths: a third mark takes
/// the first, a fence keeps its marks past the quota (only the newest beside
/// them), lifting it trims them, and a lowered budget lowers the share.
/// Tier-1: aterm-slog's `slog_mark_quota_conforms_to_derived_model`.
#[test]
fn derived_session_log_mark_quota_proves_and_catches_each_law() {
    let model = aterm_models_session::session_log_mark_quota_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_are_caught_mutants(
        &model,
        &[
            "AppendMarkUnquotaed",
            "QuotaTakesObs",
            "QuotaUncounted",
            "QuotaTakesFenced",
        ],
    );
    let fire = |s: &mut aterm_spec::interp::State, path: &[&str]| {
        for action in path {
            assert!(model.fire(action, s), "{action}: {s:?}");
        }
    };
    let mut s = model.init_state();
    fire(
        &mut s,
        &["AppendMark", "AppendObs", "AppendMark", "AppendMark"],
    );
    assert_eq!(
        (s["um"], s["uo"], s["lost"]),
        (2, 1, 1),
        "the oldest mark went"
    );
    fire(&mut s, &["Fence", "AppendMark", "AppendMark"]);
    assert_eq!(
        (s["fm"], s["um"], s["lost"]),
        (2, 1, 2),
        "the fence keeps its marks; only the newest stands beside them"
    );
    fire(&mut s, &["Lift"]);
    assert_eq!((s["um"], s["lost"]), (2, 3), "lifted, the quota trims");
    fire(&mut s, &["LowerBudget"]);
    assert_eq!((s["um"], s["mq"], s["lost"]), (1, 1, 4), "a lowered share");
}

/// The claims (phase 3A, §3.7, §7.3; ruling R13 and the fixer's review):
/// each of its seven laws is the only one some single `Buggy = 1` step
/// breaks, every mutant is dead at the committed config and caught on its
/// own, and the paths: a claim's clear leaves an observed need standing, a
/// claim ranks below it, another author's label is stored but never shown,
/// another author's verdict leaves the trusted one held, a `ctl` verdict and
/// label leave the harness's held and shown, a `ctl` clear and an edge's
/// raise of the same word leave the harness's need standing, and the owner's
/// non-claim clear lifts every need but the harness's. Tier-1: aterm-slog's
/// `fold_claims_conform_to_derived_model`.
#[test]
fn derived_session_log_claims_proves_and_catches_each_law() {
    let model = aterm_models_session::session_log_claims_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_are_caught_mutants(
        &model,
        &[
            "ClaimClearLiftsObserved",
            "ClaimRankedByArrival",
            "OtherLabelShown",
            "OtherVerdictDisplaces",
            "CtlDisplacesHarness",
            "CtlLabelShownOverHarness",
            "ReraiseTakesOver",
            "CtlClearLiftsHarness",
        ],
    );
    let fire = |s: &mut aterm_spec::interp::State, path: &[&str]| {
        for action in path {
            assert!(model.fire(action, s), "{action}: {s:?}");
        }
    };
    let mut s = model.init_state();
    fire(&mut s, &["RaiseObserved", "CtlNeed", "CtlClear"]);
    assert_eq!(
        (s["obs"], s["cn"], s["top"]),
        (1, 0, 1),
        "the failure stands"
    );
    fire(
        &mut s,
        &[
            "HarnessLabel",
            "OtherLabel",
            "HarnessVerdict",
            "OtherVerdict",
        ],
    );
    assert_eq!(
        (s["lab"], s["tl"], s["vany"], s["tv"]),
        (2, 1, 2, 1),
        "stored as made, the trusted ones shown and held"
    );
    fire(&mut s, &["CtlVerdict", "CtlLabel"]);
    assert_eq!(
        (s["vby"], s["hv"], s["lby"], s["sh"]),
        (2, 1, 2, 1),
        "a ctl claim is the trusted one; the harness's is held and shown"
    );
    fire(&mut s, &["HarnessNeed", "EdgeNeed", "CtlClear"]);
    assert_eq!(
        (s["hn"], s["en"], s["hlive"]),
        (1, 0, 1),
        "ctl lifts the edge's need, never the harness's"
    );
    fire(&mut s, &["OwnerClear"]);
    assert_eq!(
        (s["obs"], s["live"], s["hn"], s["top"]),
        (0, 0, 1, 2),
        "the owner's flag lifts all but the harness's"
    );
    fire(&mut s, &["HarnessClear"]);
    assert_eq!((s["hn"], s["hlive"], s["top"]), (0, 0, 0));
}

/// The turn tracker (§2.4): each of its six laws is the only one some
/// single `Buggy = 1` step breaks, and every mutant is dead at the
/// committed config and caught on its own. The paths: a Codex mid-answer
/// idle that busy interrupts within the settle keeps its turn open, a
/// question dialog stays in its turn, a stall never closes it, a wall held
/// at rest closes the turn and stays open on the run, and a `ctl turn`
/// while resting closes the turn as a ctl close. Tier-1: aterm-slog's
/// `turn_tracker_conforms_to_derived_model`.
#[test]
fn derived_turn_tracker_proves_and_catches_each_law() {
    let model = aterm_models_session::turn_tracker_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_are_caught_mutants(
        &model,
        &[
            "SecondTurn",
            "UnknownCloses",
            "EarlyRest",
            "DialogCloses",
            "StepAfterClose",
            "StallCloses",
        ],
    );

    let mut s = model.init_state();
    assert!(model.fire("SeeBusy", &mut s));
    assert!(model.fire("SeeIdle", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(
        !model.action_enabled("Rest", &s),
        "half a settle closes nothing"
    );
    assert!(model.fire("SeeBusy", &mut s));
    assert_eq!((s["turn"], s["pend"]), (1, 0), "the mid-answer idle passed");

    assert!(model.fire("SeeDialog", &mut s));
    assert_eq!((s["turn"], s["step"], s["sturn"]), (1, 2, 1));
    assert!(model.fire("SeeStall", &mut s));
    assert_eq!(s["turn"], 1, "a stall never closes the turn");

    assert!(model.fire("SeeWall", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("Tick", &mut s));
    assert!(model.fire("Rest", &mut s));
    assert_eq!(
        (s["turn"], s["cw"], s["step"], s["sturn"]),
        (0, 5, 3, 0),
        "a wall at rest closes the turn and stays open on the run"
    );

    // A `ctl turn` while the agent rests closes the resting turn at once,
    // as a ctl close (never an early idle close); once the settle is due,
    // only the settle closes it.
    let mut c = model.init_state();
    for action in ["SeeBusy", "SeeIdle", "Tick"] {
        assert!(model.fire(action, &mut c));
    }
    assert!(model.fire("CtlTurn", &mut c));
    assert_eq!((c["turn"], c["cw"], c["cticks"]), (0, 11, 1));
    let mut due = model.init_state();
    for action in ["SeeBusy", "SeeIdle", "Tick", "Tick"] {
        assert!(model.fire(action, &mut due));
    }
    assert!(
        !model.action_enabled("CtlTurn", &due),
        "a due settle is the settle's"
    );
}
