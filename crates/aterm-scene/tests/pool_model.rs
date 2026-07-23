// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Formal bounded-state model for a Scene **entity pool** (AGENTS.md "bounded state
//! machine" obligation): a pool that `Spawn`s up to a hard `Cap` and `Despawn`s down to
//! zero must never exceed `Cap`. The model is authored once in the `ty_model!`
//! light-annotation surface; the same single source yields the `ty`-checkable TLA+ spec
//! (Tier-0, run under the Trust toolchain) *and* an executable interpreter, which we drive
//! here so the bound is enforced even without the toolchain present (the runnable half).
//!
//! The `Buggy` convention demonstrates catchability: at `Buggy = 0` the guard holds the
//! invariant; flipping the guard (the `pool_buggy` model) lets the pool overflow, which
//! the interpreter detects — proving the model would catch a real off-by-one in the
//! spawn guard. This bound is self-contained — it stands on its own without any concrete
//! scene present, and is the live Tier-0 `Pool` invariant that ANY future scene's
//! spawn/despawn pool must conform to. When a concrete scene lands (the framework contract
//! is `docs/living-panels/SCENE_CONTRACT.md`), it re-attaches a Tier-1 conformance test
//! that drives the real pool under adversarial drives and projects its length onto this
//! bound (per AGENTS.md's bounded-state-machine rule and DESIGN.md §5a).

use aterm_spec::derive::Model;
use aterm_spec::ty_model;

/// The correct pool: `Spawn` is guarded by `count <= Cap - 1`, so `count` never exceeds
/// `Cap`; `Despawn` is guarded by `count > 0`.
fn pool_model() -> Model {
    ty_model! {
        Pool {
            const Cap = 6;
            var count = 0;
            action Spawn when (count <= Cap - 1) { count = count + 1; }
            action Despawn when (count > 0) { count = count - 1; }
            invariant Bounded: count <= Cap;
        }
    }
}

/// The deliberately buggy pool: the spawn guard is off by one (`count <= Cap`), so the
/// pool can reach `Cap + 1` — the defect the model is meant to catch.
fn pool_buggy() -> Model {
    ty_model! {
        Pool {
            const Cap = 6;
            var count = 0;
            action Spawn when (count <= Cap) { count = count + 1; }
            action Despawn when (count > 0) { count = count - 1; }
            invariant Bounded: count <= Cap;
        }
    }
}

#[test]
fn correct_pool_never_overflows_via_interpreter() {
    // Exhaustively reachable state is tiny (count ∈ 0..=Cap); a long mixed drive visits it
    // all. The guard must keep `Bounded` true after every fired action, and must REFUSE a
    // Spawn at the cap.
    let m = pool_model();
    let mut st = m.init_state();
    // Fill to the cap.
    for _ in 0..6 {
        assert!(m.fire("Spawn", &mut st), "spawn allowed below cap");
        assert!(m.check_invariant("Bounded", &st), "bounded while filling");
    }
    assert_eq!(st[&"count"], 6, "filled to cap");
    assert!(
        !m.fire("Spawn", &mut st),
        "spawn at cap must be REFUSED by the guard"
    );
    assert_eq!(st[&"count"], 6, "refused spawn left the count unchanged");

    // A long triangle-wave drive sweeps the full range (cap → 0 → cap → …); the invariant
    // must hold after every step, and the guards must keep `count` inside `0..=Cap`.
    let mut up = false;
    let (mut seen_zero, mut seen_cap) = (false, false);
    for i in 0..1000 {
        if up {
            m.fire("Spawn", &mut st);
        } else {
            m.fire("Despawn", &mut st);
        }
        let c = st[&"count"];
        assert!(
            m.check_invariant("Bounded", &st),
            "Bounded holds at step {i}, count={c}"
        );
        assert!(c <= 6, "guard keeps count in range: {c}");
        if c == 0 {
            seen_zero = true;
            up = true;
        }
        if c == 6 {
            seen_cap = true;
            up = false;
        }
    }
    assert!(seen_zero && seen_cap, "drive swept the full range");
}

#[test]
fn buggy_pool_is_caught_by_the_model() {
    // The off-by-one guard lets the pool overflow; the interpreter reaches count = Cap+1,
    // where `Bounded` is FALSE — i.e. the model catches the defect (the proves-and-catches
    // discipline, executable half).
    let m = pool_buggy();
    let mut st = m.init_state();
    for _ in 0..7 {
        m.fire("Spawn", &mut st);
    }
    assert_eq!(st[&"count"], 7, "buggy guard allowed an overflow spawn");
    assert!(
        !m.check_invariant("Bounded", &st),
        "the model must CATCH the overflow (Bounded violated at count=7)"
    );
}
