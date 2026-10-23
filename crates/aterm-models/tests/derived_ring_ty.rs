// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Tier-0 for `ty_model!` models (`aterm_spec::derive`'s module docs).

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
use aterm_models_session::{
    forward_handshake_model, session_id_claim_model, tls_buffered_relay_model,
};
use aterm_spec::derive::Model;
use aterm_spec::verify;

/// NO WEDGE: deadlock-free at `Buggy = 0` and a DEADLOCK (not an invariant
#[test]
fn designs_that_must_not_wedge_are_deadlock_free_and_catch_the_wedge() {
    type Final = fn(&aterm_spec::interp::State) -> bool;
    let mut release_publish_once = release_publish_once_model();
    release_publish_once.invariants.clear();
    let rows: [(Model, Final); 3] = [
        (forward_handshake_model(), |s| {
            s.get("client_waiting") == Some(&0)
        }),
        (tls_buffered_relay_model(), |s| {
            s.get("service_waiting") == Some(&0)
        }),
        (release_publish_once, |s| s["phase"] == 5 || s["phase"] == 6),
    ];
    for (m, is_final) in rows {
        verify::deadlock_free_and_catches_tiered(&m, is_final, m.name);
    }
}

/// Sparkle-words v2 flash limiter (design §6.4/§9): WCAG 2.3.1, model-checked.
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
#[test]
fn scope_aggregate_models_prove_catch_and_multiply() {
    type Corner = &'static [(&'static str, i64)];
    let rows: [(Model, &[Corner], Corner); 3] = [
        (
            flash_limiter_window_model(),
            &[&[], &[("Overlap", 1)]],
            &[("Overlap", 1)],
        ),
        (alt_archive_pool_model(), &[&[]], &[]),
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
fn assert_proves_and_catches(m: &Model) {
    verify::prove_and_catch_scalar(m, m.name);
}

/// Every design invariant must be the ONLY law some `Buggy = 1` step breaks — a
fn assert_every_invariant_breaks_first(m: &Model) {
    let mut alone: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for dial in m.mutant_dials() {
        let buggy = aterm_spec::interp::with_buggy(m, dial);
        let r = aterm_spec::interp::Resolved::new(&buggy).expect("the model resolves");
        let laws = 0..buggy.invariants.len();
        let mut seen = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        let init = r.init();
        seen.insert(init.clone());
        queue.push_back(init);
        while let Some(st) = queue.pop_front() {
            assert!(
                seen.len() < 100_000,
                "{}: Buggy={dial} good space unbounded",
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
        "{}: {shared:?} never break alone — every mutant step from an all-good state \
         that breaks them breaks another law too (or they break only after another law \
         already broke), so their catch is some other law's",
        m.name
    );
}

/// The committed-dead actions of `m` are exactly `expected`, pinned where the
fn assert_committed_dead_actions(m: &Model, expected: &[&str]) {
    let healthy_fired = aterm_spec::interp::fired_actions(&aterm_spec::interp::with_buggy(m, 0));
    let dead: Vec<_> = m
        .actions
        .iter()
        .map(|action| action.name)
        .filter(|name| !healthy_fired.contains(name))
        .collect();
    assert_eq!(dead, expected, "{}: committed-dead actions", m.name);
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
#[test]
fn derived_session_log_ring_proves_and_catches_each_law() {
    let model = aterm_models_session::session_log_ring_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_actions(
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
    let mut c = wide.init_state();
    for action in ["OpenRun", "OpenTurn", "OpenStep", "CloseRunCascade"] {
        assert!(wide.fire(action, &mut c), "{action}: {c:?}");
        settle(&wide, &mut c);
    }
    assert_eq!((c["seq"], c["tclose"], c["tn"], c["fturns"]), (6, 1, 1, 1));

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
#[test]
fn derived_log_mirror_proves_and_catches_each_law() {
    let model = aterm_models_session::log_mirror_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_actions(
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
#[test]
fn derived_session_log_cursor_proves_and_catches_each_law() {
    let model = aterm_models_session::session_log_cursor_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_actions(
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

/// The journal reader's view: each of its five laws
#[test]
fn derived_session_log_journal_proves_and_catches_each_law() {
    let model = aterm_models_session::session_log_journal_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_actions(
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
    let mut b = model.init_state();
    fire(
        &mut b,
        &[
            "Append", "Write", "Append", "Append", "Snap", "Crash", "Read",
        ],
    );
    assert_eq!((b["sb"], b["ca"], b["vs"]), (3, 1, 0), "after= the cursor");
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

/// The mark quota (§7.4): each of its four laws is the
#[test]
fn derived_session_log_mark_quota_proves_and_catches_each_law() {
    let model = aterm_models_session::session_log_mark_quota_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_actions(
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

/// The program share: each of its four laws
#[test]
fn derived_session_log_program_share_proves_and_catches_each_law() {
    let model = aterm_models_session::session_log_program_share_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_actions(
        &model,
        &[
            "AppendAUnshared",
            "ProgramShareEvictsAny",
            "ShareTakesOtherNode",
            "ShareUncounted",
            "ShareTakesFenced",
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
        &["AppendOther", "AppendA", "AppendB", "AppendA", "AppendA"],
    );
    assert_eq!(
        (s["uo"], s["ua"], s["ub"], s["lost"]),
        (1, 2, 1, 1),
        "the run's oldest program mark went, and nothing beside it"
    );
    fire(&mut s, &["Fence", "AppendA", "AppendA"]);
    assert_eq!(
        (s["fa"], s["ua"], s["lost"]),
        (2, 1, 2),
        "the fence keeps its marks; only the newest stands beside them"
    );
    fire(&mut s, &["Lift"]);
    assert_eq!(
        (s["ua"], s["ub"], s["uo"], s["lost"]),
        (2, 1, 1, 3),
        "lifted, the share trims the run alone"
    );
}

/// The rows: each of its three laws is the
#[test]
fn derived_session_log_rows_proves_and_catches_each_law() {
    let model = aterm_models_session::session_log_rows_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_actions(
        &model,
        &[
            "EvictRowUnderFloor",
            "HandoffRowsRestart",
            "CrashKeepsEpoch",
            "CarryShedUncounted",
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
            "AppendRow",
            "AppendRow",
            "AppendOther",
            "AppendOther",
            "AppendOther",
        ],
    );
    assert_eq!((s["rf"], s["ro"], s["u"]), (2, 0, 3));
    assert!(
        !model.fire("EvictFloor", &mut s.clone()),
        "a floor row waits while another record may go"
    );
    fire(&mut s, &["EvictOther", "EvictOther"]);
    assert!(
        model.fire("EvictFloor", &mut s.clone()),
        "only the newest is left beside the floor: its rows may go"
    );
    fire(&mut s, &["AppendRow"]);
    assert_eq!((s["rf"], s["ro"]), (2, 1), "the oldest row left the floor");
    assert!(!model.fire("EvictFloor", &mut s.clone()));
    fire(&mut s, &["EvictOutside", "AppendRow"]);
    assert_eq!(
        (s["rid"], s["rfirst"], s["rapp"]),
        (4, 1, 4),
        "each row takes the next id"
    );
    let mut crashed = s.clone();
    fire(&mut s, &["HandoffCarried"]);
    assert_eq!(
        (s["rid"], s["rfirst"], s["rprev"], s["rf"], s["u"]),
        (6, 6, 5, 1, 4),
        "the successor's ids continue past the ceiling; the carried records are no rows"
    );
    fire(&mut crashed, &["Crash"]);
    assert_eq!(
        (
            crashed["rid"],
            crashed["rfirst"],
            crashed["rprev"],
            crashed["seq"]
        ),
        (1, 1, 0, 1),
        "a crash adoption's ids start at 1 in its own epoch"
    );
}

/// The claims (§3.7, §7.3):
#[test]
fn derived_session_log_claims_proves_and_catches_each_law() {
    let model = aterm_models_session::session_log_claims_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_actions(
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
#[test]
fn derived_transcript_join_proves_and_catches_each_law() {
    let model = aterm_models_session::transcript_join_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_actions(
        &model,
        &[
            "PromptOpensBeside",
            "LateStepOrphan",
            "RestLeavesSteps",
            "ScreenConfirms",
            "ReplayAfterLapse",
            "ReplayFromProcess",
            "AttachEarly",
            "OldPromptAttaches",
            "LateNamesOpen",
            "MergeClosesBox",
            "TxCloseTakesWall",
            "RefuteLeavesSettle",
            "PartialCloses",
        ],
    );

    let mut s = model.init_state();
    assert!(model.fire("ScreenBusy", &mut s));
    for row in ["PromptAttach", "ToolUse", "TxEndCloses", "Replay"] {
        assert!(!model.action_enabled(row, &s), "{row} before a bind");
    }
    assert!(model.fire("Bind", &mut s));

    assert!(model.fire("PromptAttach", &mut s));
    assert!(model.fire("ToolUse", &mut s));
    assert_eq!((s["lt"], s["vm"], s["ts"]), (2, 1, 1));
    assert!(model.fire("TxEndCloses", &mut s));
    assert_eq!((s["lt"], s["vm"], s["ve"], s["ts"]), (0, 2, 1, 0));

    let mut aged = model.init_state();
    for a in ["Bind", "ScreenBusy", "Age"] {
        assert!(model.fire(a, &mut aged));
    }
    assert!(
        !model.action_enabled("PromptAttach", &aged),
        "outside the window"
    );
    let mut boxed = aged.clone();
    assert!(model.fire("ScreenBox", &mut boxed));
    assert!(!model.action_enabled("PromptSupersede", &boxed));
    assert!(model.fire("PromptShare", &mut boxed));
    assert_eq!((boxed["lt"], boxed["ss"], boxed["vm"]), (1, 1, 1));
    assert!(
        !model.action_enabled("TxEndCloses", &boxed),
        "a box under the turn outlives its vendor turn's end"
    );
    assert!(model.fire("PromptSupersede", &mut aged));
    assert_eq!((aged["lt"], aged["old"], aged["vm"]), (2, 0, 1));

    let mut late = model.init_state();
    for a in ["Bind", "ScreenBusy", "ScreenRest"] {
        assert!(model.fire(a, &mut late));
    }
    assert!(!model.action_enabled("PromptOpens", &late));
    assert!(model.fire("PromptLate", &mut late));
    assert_eq!((late["lt"], late["vm"], late["lc"]), (0, 2, 0));
    assert!(
        !model.action_enabled("PromptLate", &late),
        "a closed turn takes one"
    );

    let mut partial = model.init_state();
    for a in ["Bind", "SetPartial", "ScreenBusy", "Age", "PromptShare"] {
        assert!(model.fire(a, &mut partial));
    }
    assert!(!model.action_enabled("TxEndCloses", &partial));
    assert!(model.fire("TxEndHeld", &mut partial));
    assert_eq!(partial["lt"], 1);

    let mut wall = model.init_state();
    for a in ["Bind", "ScreenBusy", "PromptAttach", "ScreenWall"] {
        assert!(model.fire(a, &mut wall));
    }
    assert!(
        !model.action_enabled("TxEndCloses", &wall),
        "the wall holds it"
    );
    assert!(model.fire("Refute", &mut wall));
    assert_eq!((wall["ss"], wall["pend"], wall["rf"]), (0, 0, 1));
    assert!(model.fire("TxEndCloses", &mut wall));

    let fired = |path: &[&str]| {
        let mut s = model.init_state();
        for a in path {
            assert!(model.fire(a, &mut s), "{a} along {path:?}");
        }
        s
    };
    let ran = ["Bind", "SetPartial", "ScreenBusy", "PromptAttach"];
    let esc = fired(&[&ran[..], &["Interrupt", "ScreenRest", "ScreenBusy"]].concat());
    assert_eq!(
        (esc["lt"], esc["vm"], esc["ve"]),
        (1, 2, 1),
        "no continuation"
    );
    let running = fired(&[&ran[..], &["ScreenRest", "ScreenBusy"]].concat());
    assert_eq!((running["lt"], running["vm"]), (1, 1), "continued");

    let first = fired(&["Bind", "SetPartial", "PromptUnmapped", "ScreenBusy"]);
    assert_eq!((first["lt"], first["vm"]), (2, 1));
    let stale = fired(&["Bind", "SetPartial", "PromptUnmapped", "Age", "ScreenBusy"]);
    assert_eq!((stale["lt"], stale["vm"], stale["vo"]), (1, 3, 1));

    let old = fired(&["Bind", "ScreenBusy", "PromptOld"]);
    assert_eq!((old["lt"], old["vm"], old["vo"]), (1, 3, 1));
    let next = fired(&[
        "Bind",
        "ScreenBusy",
        "PromptOld",
        "ScreenRest",
        "ScreenBusy",
    ]);
    assert_eq!((next["lt"], next["vm"]), (1, 3), "no screen turn takes it");
    let near = fired(&[
        "Bind",
        "ScreenBusy",
        "ScreenRest",
        "ScreenBusy",
        "PromptLateNear",
    ]);
    assert_eq!(
        (near["lt"], near["vm"], near["lv"], near["lc"]),
        (1, 2, 0, 0)
    );
    let held = fired(&[
        "Bind",
        "ScreenBusy",
        "PromptAttach",
        "ScreenRest",
        "ScreenBusy",
    ]);
    assert!(!model.action_enabled("PromptLateNear", &held));
}

#[test]
fn derived_turn_tracker_proves_and_catches_each_law() {
    let model = aterm_models_session::turn_tracker_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_actions(
        &model,
        &[
            "SecondTurn",
            "UnknownCloses",
            "EarlyRest",
            "DialogCloses",
            "StepAfterClose",
            "StallCloses",
            "TxCloseTakesWall",
            "RefuteKeepsSettle",
            "ActBindsNextBox",
            "AttributeStaleInput",
            "ActOverPerson",
            "ActLeadOverPerson",
            "ActTwice",
            "ActorSwap",
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

    let mut w = model.init_state();
    for action in ["SeeBusy", "SeeWall"] {
        assert!(model.fire(action, &mut w));
    }
    assert!(
        !model.action_enabled("TxClose", &w),
        "a wall under the turn outlives the transcript's end"
    );
    assert!(model.fire("TxRefute", &mut w));
    assert_eq!((w["turn"], w["step"], w["pend"], w["rf"]), (1, 0, 0, 1));
    assert!(model.fire("TxClose", &mut w));
    assert_eq!((w["turn"], w["cw"]), (0, 12));
    let mut settling = model.init_state();
    for action in ["SeeBusy", "SeeIdle", "Tick"] {
        assert!(model.fire(action, &mut settling));
    }
    assert!(model.fire("TxClose", &mut settling));
    assert_eq!(
        (settling["turn"], settling["cw"], settling["pend"]),
        (0, 12, 0),
        "the transcript's end closes a settling turn and disarms its settle"
    );

    let mut b = model.init_state();
    for action in ["SeeBusy", "SeeBox", "AdmitHarness", "SeeNextBox"] {
        assert!(model.fire(action, &mut b), "{action}");
    }
    assert_eq!(b["pd"], 4, "box 1 closed before the acceptance");
    assert!(model.fire("Accept", &mut b));
    assert_eq!(
        (b["bk"], b["pd"]),
        (0, 0),
        "a late acceptance binds nothing"
    );
    let mut lead = model.init_state();
    for action in ["SeeBusy", "AdmitCtl", "Accept"] {
        assert!(model.fire(action, &mut lead), "{action}");
    }
    assert_eq!(lead["pd"], 3, "accepted on the lead");
    assert!(model.fire("SeeBox", &mut lead));
    assert_eq!((lead["bk"], lead["pd"]), (1, 0), "the lead's first box");
    assert_eq!(
        (lead["ba"], lead["bs"], lead["pa"]),
        (1, 1, 0),
        "a ctl act on the lead binds as ctl's"
    );
    assert!(model.fire("Person", &mut lead));
    assert_eq!(
        (lead["bk"], lead["pp"]),
        (2, 1),
        "the person's input is latest"
    );
    assert_eq!(
        (lead["ba"], lead["bs"]),
        (0, 0),
        "the person's binds no actor"
    );
    assert!(model.fire("SeeBusy", &mut lead));
    assert_eq!((lead["bk"], lead["pp"]), (0, 0), "the close consumed it");
    let mut first = model.init_state();
    for action in ["SeeBusy", "AdmitHarness", "SeeBox", "Accept"] {
        assert!(model.fire(action, &mut first), "{action}");
    }
    assert_eq!(
        (first["bk"], first["pd"]),
        (1, 0),
        "admitted on the lead, bound by its first box"
    );
    assert_eq!(
        (first["ba"], first["bs"], first["pa"]),
        (2, 2, 0),
        "a harness act binds as the harness's, never ctl's"
    );
    let mut stale = model.init_state();
    for action in ["SeeBusy", "SeeBox", "AdmitCtl", "Accept", "Age"] {
        assert!(model.fire(action, &mut stale), "{action}");
    }
    assert_eq!((stale["bk"], stale["ba"], stale["bs"]), (3, 0, 0));
    assert!(model.fire("SeeBusy", &mut stale));
    assert_eq!(
        (stale["bk"], stale["ga"]),
        (0, 0),
        "a stale input attributes nothing"
    );
}

/// The session log fold (phase 3B, plan §4.2 as amended by A31 and S13):
#[test]
fn derived_session_log_fold_proves_and_catches_each_law() {
    let model = aterm_models_session::session_log_fold_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_actions(
        &model,
        &[
            "ReplayApplies",
            "StrongAmendRefines",
            "LowerAmendWins",
            "ConfirmNoUpgrade",
            "RebucketWithoutDecrement",
            "RetractKeepsNeed",
            "TickRaisesNeed",
            "CloseRunLeavesTurn",
            "ExpiredLabelShown",
            "LabelHidesNeed",
        ],
    );
    let fire = |s: &mut aterm_spec::interp::State, path: &[&str]| {
        for action in path {
            assert!(model.fire(action, s), "{action}: {s:?}");
        }
    };
    let mut s = model.init_state();
    fire(&mut s, &["OpenRun", "OpenTurn", "CloseDone"]);
    assert_eq!((s["ow"], s["oc"], s["nd"], s["nw"]), (1, 3, 1, 0));
    fire(&mut s, &["AmendOutExact"]);
    assert_eq!(
        (s["ow"], s["oc"], s["nd"], s["nw"]),
        (2, 4, 0, 1),
        "an exact amend refines the strong close and re-buckets it"
    );
    let refined = s.clone();
    fire(&mut s, &["AmendOutStrong", "Replay", "Tick"]);
    assert_eq!(
        s, refined,
        "a strong amend, a replay and silence change nothing"
    );

    let mut early = model.init_state();
    fire(
        &mut early,
        &["OpenRun", "OpenTurn", "AmendOutExact", "CloseDone"],
    );
    assert_eq!(
        (early["ow"], early["oc"], early["nw"]),
        (2, 4, 1),
        "an exact amend while open outranks the later strong close"
    );

    let mut need = model.init_state();
    fire(&mut need, &["OpenRun", "OpenTurn", "OpenStep", "Label"]);
    assert_eq!(
        (need["top"], need["cl"]),
        (1, 1),
        "the need, never the label"
    );
    fire(&mut need, &["ConfirmStep"]);
    assert_eq!(need["nc"], 4, "the confirmation raised the need");
    fire(&mut need, &["RetractStep"]);
    assert_eq!(
        (need["s"], need["nc"], need["top"], need["cl"]),
        (2, 0, 0, 2),
        "the need left with its step; the label is the clause"
    );
    fire(&mut need, &["Expire"]);
    assert_eq!(need["cl"], 3, "an expired label gives the clause back");
    fire(&mut need, &["CloseRun"]);
    assert_eq!(
        (need["t"], need["ow"], need["ng"], need["cl"]),
        (2, 3, 1, 0),
        "the run's close cascades its turn gone"
    );
}

/// The heuristic mail → turn link (phase 5, plan 5D-5; T10, T39): each of
#[test]
fn derived_session_log_mail_link_proves_and_catches_each_law() {
    let model = aterm_models_session::session_log_mail_link_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model);
    assert_committed_dead_actions(
        &model,
        &[
            "LinkAtTurnConf",
            "LinkOverExplicit",
            "LinkAcrossHuman",
            "LinkPastWindow",
            "MailLinksTwice",
            "LinkNote",
            "LinkAmongMany",
        ],
    );
    let after = |path: &[&str]| {
        let mut s = model.init_state();
        for action in path {
            assert!(model.fire(action, &mut s), "{action}: {s:?}");
        }
        s
    };
    let linked = after(&["TaskMail", "OpenTurn"]);
    assert_eq!(
        (linked["lk"], linked["lc"], linked["lm"], linked["cands"]),
        (1, 1, 1, 0),
        "one task mail links the next turn, heuristically"
    );
    let s = after(&["TaskMail", "OpenTurn", "CloseTurn", "OpenTurn"]);
    assert_eq!((s["lk"], s["lm"]), (0, 1), "the mail links one turn");
    let s = after(&["NoteMail", "OpenTurn"]);
    assert_eq!((s["lk"], s["notes"]), (0, 1), "a note links none");
    let s = after(&["TaskMail", "Human", "OpenTurn"]);
    assert_eq!((s["lk"], s["hm"]), (0, 1), "a person between takes it");
    let s = after(&["TaskMail", "HumanNoReach", "OpenTurn"]);
    assert_eq!(
        (s["lk"], s["hm"]),
        (1, 0),
        "a gesture that reached no program takes nothing"
    );
    let s = after(&["TaskMail", "Age"]);
    assert_eq!((s["cands"], s["old"]), (0, 1));
    let s = after(&["TaskMail", "Age", "OpenTurn"]);
    assert_eq!((s["lk"], s["old"]), (0, 0), "past the window, dropped");
    let s = after(&["TaskMail", "OpenCausedTurn"]);
    assert_eq!(
        (s["lk"], s["ex"], s["lm"], s["cands"]),
        (0, 1, 1, 0),
        "an explicit cause links none heuristically and takes its mail"
    );
    let s = after(&["TaskMail", "TaskMail", "OpenTurn"]);
    assert_eq!(
        (s["lk"], s["amb"], s["cands"]),
        (0, 1, 0),
        "two link none and count the turn ambiguous"
    );
    let s = after(&["TaskMail", "OpenPersonTurn"]);
    assert_eq!((s["lk"], s["th"], s["hm"]), (0, 1, 1));
}
