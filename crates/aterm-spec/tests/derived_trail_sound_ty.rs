// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-0 for the typing-sound machines: the key seam (`TrailSoundSeam`: a motion or
//! performance policy must not mute typing) and the audio host's reopen ladder
//! (`TrailAudioReopenLadder`: one device fault is not forever, and the budget is really spent).
//!
//! Beyond prove-and-catch, both machines are checked for non-vacuity: every invariant is caught
//! alone, every action fires, and the guard-conjunct audit (which invariants each conjunct
//! carries, and whether dropping it changes a non-stutter transition) is pinned against a
//! declared `Role` per conjunct. A guard conjunct can make a racing state unreachable and its
//! invariant vacuously true, so a new guard, or one that starts carrying an invariant, fails
//! until declared.

use std::collections::BTreeSet;

use aterm_spec::derive::{Expr, Model, trail_audio_reopen_ladder_model, trail_sound_seam_model};
use aterm_spec::interp::{self, State};
use aterm_spec::verify;

use crate::conjunct_audit::audit_guard_conjuncts;

fn fire(m: &Model, action: &str, s: &State) -> State {
    let next = m.successors(action, s);
    assert_eq!(
        next.len(),
        1,
        "{}: `{action}` must be enabled and deterministic at {s:?}",
        m.name
    );
    next.into_iter().next().expect("one successor")
}

fn run(m: &Model, actions: &[&str]) -> State {
    actions
        .iter()
        .fold(m.init_state(), |s, action| fire(m, action, &s))
}

fn assert_no_ghosts_and_no_dead_actions(m: &Model) {
    assert!(
        verify::uncaught_invariants(m).is_empty(),
        "{}: invariants no Buggy=1 member falsifies alone: {:?}",
        m.name,
        verify::uncaught_invariants(m)
    );
    let fired = interp::fired_actions(&interp::with_buggy(m, 0));
    let all: BTreeSet<&str> = m.actions.iter().map(|a| a.name).collect();
    assert_eq!(fired, all, "{}: an action never fires at Buggy=0", m.name);
}

/// The declared role of one guard conjunct.
#[derive(Clone, Copy, Debug)]
enum Role {
    /// Carries no invariant and changes behaviour.
    Decision,
    /// Carries no invariant and suppresses only self-loops; bound at Tier-1
    /// as an enabling condition like every other guard.
    Stutter,
    /// An environment fact that the named invariants rest on; bound at
    /// Tier-1 by `action_enabled` against the real predicate.
    Environment(&'static [&'static str]),
}

/// Assert the audit against the declared roles, exactly.
fn assert_audit(m: &Model, declared: &[(&str, usize, Role)]) {
    let report = audit_guard_conjuncts(m, None);
    assert_eq!(
        report.len(),
        declared.len(),
        "{}: the audit found {} conjuncts, {} declared: {report:#?}",
        m.name,
        report.len(),
        declared.len()
    );
    for (r, &(action, index, role)) in report.iter().zip(declared) {
        assert_eq!((r.action, r.index), (action, index), "{}: {r:?}", m.name);
        let (carries, changes): (&[&str], bool) = match role {
            Role::Decision => (&[], true),
            Role::Stutter => (&[], false),
            Role::Environment(carries) => (carries, true),
        };
        assert_eq!(
            (r.carries.as_slice(), r.changes_behaviour),
            (carries, changes),
            "{}: conjunct #{index} of `{action}` declared {role:?}, measured {r:?}",
            m.name
        );
    }
}

#[test]
fn trail_sound_seam_proves_the_rulings_and_catches_d1() {
    let m = trail_sound_seam_model();
    let covered = verify::prove_and_catch_scalar(&m, m.name);
    eprintln!("TrailSoundSeam prove-and-catch covered by: {covered:?}");
    assert_no_ghosts_and_no_dead_actions(&m);
    // No guards at all, asserted so a guard added later is audited.
    assert!(m.actions.iter().all(|a| a.guard.is_none()));
    assert!(audit_guard_conjuncts(&m, None).is_empty());
}

#[test]
fn trail_sound_seam_witnesses_every_ruling_and_the_d1_replay() {
    let m = trail_sound_seam_model();

    // The rulings, on the committed machine.
    for (tick, key, tone) in [
        ("TickLit", 1, 1),
        // Dark for a motion or performance reason: still heard, and the verb says so.
        ("TickDarkMotion", 1, 1),
        ("TickDarkUnfocused", 2, 2),
        ("TickDarkUserOff", 2, 2),
        ("TickMasterOff", 2, 2),
    ] {
        let s = run(&m, &[tick, "Key", "Tone"]);
        assert_eq!((s["key"], s["tone"]), (key, tone), "{tick}");
        for inv in &m.invariants {
            assert!(m.check_invariant(inv.name, &s), "{tick}: {}", inv.name);
        }
    }
    // Before any tick nothing is heard: a host that never ticks is silent.
    let s = run(&m, &["Key"]);
    assert_eq!(s["key"], 2);
    // A tick clears the readers, so an answer is always about its tick.
    let s = run(&m, &["TickDarkUnfocused", "Key", "TickLit"]);
    assert_eq!(s["key"], 0);

    let buggy = interp::with_buggy(&m, 1);
    // The muted-typing replay: the zero-amplitude return closes the seam unconditionally, so a
    // shed frame or Reduce Motion mutes the key and the verb reports `closed:engine-silent`.
    let d1 = run(&buggy, &["TickDarkMotion", "Key", "Tone"]);
    assert_eq!((d1["key"], d1["tone"]), (2, 3));
    assert!(!buggy.check_invariant("FocusedKeyIsHeard", &d1));
    assert!(!buggy.check_invariant("EngineSilentIsUnreachable", &d1));
    // The same trace on the committed machine is the positive control.
    let fixed = run(&m, &["TickDarkMotion", "Key", "Tone"]);
    assert!(m.check_invariant("FocusedKeyIsHeard", &fixed));

    // The over-correction: opening every dark return sounds a key in an unfocused window while
    // the verb says `closed:unfocused`.
    let over = run(&buggy, &["TickDarkUnfocused", "Key", "Tone"]);
    assert_eq!((over["key"], over["tone"]), (1, 2));
    assert!(!buggy.check_invariant("ForeignKeyIsSilent", &over));
    assert!(!buggy.check_invariant("ToneAgreesWithTheKey", &over));

    // Buggy=1, `trail_on = enabled`: a user-dimmed aurora is silent, correctly, but named
    // `engine-silent`, a false regression alarm.
    let dimmed = run(&buggy, &["TickDarkUserOff", "Key", "Tone"]);
    assert_eq!((dimmed["key"], dimmed["tone"]), (2, 3));
    assert!(!buggy.check_invariant("EngineSilentIsUnreachable", &dimmed));
    assert!(buggy.check_invariant("ToneAgreesWithTheKey", &dimmed));
}

#[test]
fn trail_audio_reopen_ladder_proves_bounded_recovery_and_catches_every_replay() {
    let m = trail_audio_reopen_ladder_model();
    let covered = verify::prove_and_catch_scalar(&m, m.name);
    eprintln!("TrailAudioReopenLadder prove-and-catch covered by: {covered:?}");
    assert_no_ghosts_and_no_dead_actions(&m);
    // Only the healthy window's "it has started" conjunct carries invariants: an environment fact
    // Tier-1 checks against the real `played_since`. It carries all three because of the
    // `evidence` record, which refutes a window that finishes without a delivery.
    assert_audit(
        &m,
        &[
            ("Cue", 0, Role::Stutter),
            ("DeviceFaults", 0, Role::Stutter),
            ("DeviceFaults", 1, Role::Decision),
            ("BackoffElapses", 0, Role::Stutter),
            ("StaleElapses", 0, Role::Stutter),
            (
                "HealthyWindowElapses",
                0,
                Role::Environment(&[
                    "RecoveryIsBounded",
                    "ReopensLeftIsTruthful",
                    "ResetOnlyOnPlayedWindow",
                ]),
            ),
        ],
    );

    // It also proves and catches at the shipping budget (`REOPEN_BUDGET = 6`, what Tier-1 drives).
    let shipping = interp::with_consts(&m, &[("Budget", 6)]);
    let covered = verify::prove_and_catch_scalar(&shipping, "TrailAudioReopenLadder@Budget=6");
    eprintln!("TrailAudioReopenLadder@Budget=6 covered by: {covered:?}");
    assert!(verify::uncaught_invariants(&shipping).is_empty());
}

#[test]
fn trail_audio_reopen_ladder_witnesses_recovery_exhaustion_and_the_replays() {
    let m = trail_audio_reopen_ladder_model();
    let budget = m.consts.iter().find(|c| c.0 == "Budget").expect("Budget").1;
    assert_eq!(budget, 2);

    // A transient fault: the first reopen is immediate, and once the device has played through a
    // healthy window the budget is back.
    let s = run(&m, &["Cue", "CueSounds", "DeviceFaults"]);
    assert_eq!((s["attempts"], s["waiting"], s["failed"]), (1, 0, 0));
    let s = fire(&m, "Cue", &s);
    assert_eq!(s["device"], 1, "the immediate reopen opens on the next cue");
    let playing = fire(&m, "CueSounds", &s);
    assert_eq!((playing["attempts"], playing["played"]), (1, 1));
    let again = fire(&m, "CueSounds", &playing);
    assert_eq!(
        again["attempts"], 1,
        "deliveries inside the window are not health"
    );
    let aged = fire(&m, "HealthyWindowElapses", &playing);
    let healed = fire(&m, "CueSounds", &aged);
    assert_eq!(
        (healed["attempts"], healed["spent"], healed["played"]),
        (0, 0, 0)
    );

    // Time since the fault is not play time: a stale fault then one delivery hands nothing back.
    let s = run(
        &m,
        &["Cue", "DeviceFaults", "StaleElapses", "Cue", "CueSounds"],
    );
    assert_eq!((s["stale"], s["played"], s["attempts"]), (1, 1, 1));

    // A device that never plays: every later reopen waits its window, and
    // exhaustion — at exactly the budget — is the one terminal state.
    let s = run(&m, &["Cue", "DeviceFaults", "Cue", "DeviceFaults"]);
    assert_eq!((s["attempts"], s["waiting"]), (2, 1));
    let blocked = fire(&m, "Cue", &s);
    assert_eq!(
        blocked["device"], 0,
        "a cue inside the window opens nothing"
    );
    let waited = fire(&m, "BackoffElapses", &s);
    assert_eq!(waited["stale"], 1, "the last wait is the whole window");
    let s = fire(&m, "DeviceFaults", &waited);
    assert_eq!((s["failed"], s["attempts"]), (1, budget));
    assert!(m.check_invariant("SilenceIsPermanentOnlyWhenTheBudgetIsSpent", &s));
    // Terminal: nothing after exhaustion opens a device, resets the ladder or spends a reopen.
    for action in ["Cue", "CueSounds"] {
        assert_eq!(fire(&m, action, &s), s, "{action} after exhaustion");
    }
    assert!(!m.action_enabled("DeviceFaults", &s));

    let buggy = interp::with_buggy(&m, 1);
    // Buggy=1, one failed open is terminal with the whole budget unspent.
    let terminal = run(&buggy, &["DeviceFaults"]);
    assert_eq!((terminal["failed"], terminal["attempts"]), (1, 0));
    assert!(!buggy.check_invariant("SilenceIsPermanentOnlyWhenTheBudgetIsSpent", &terminal));

    // Buggy=1, a successful open resets the ladder: a device that constructs and never plays
    // reopens forever while the wire reports a full budget.
    let mut s = fire(&buggy, "Cue", &buggy.init_state());
    for _ in 0..=budget {
        s = fire(&buggy, "DeviceFaults", &s);
        s = fire(&buggy, "Cue", &s);
    }
    assert_eq!(
        (s["failed"], s["attempts"]),
        (0, 0),
        "never exhausted, budget shown full"
    );
    assert_eq!(s["spent"], budget + 1);
    assert!(!buggy.check_invariant("RecoveryIsBounded", &s));
    assert!(!buggy.check_invariant("ReopensLeftIsTruthful", &s));

    // Buggy=1, window measured from the fault: a device that plays one block and stalls gets the
    // budget back on every delivery after a stale fault and cycles the ladder forever.
    let mut s = fire(&buggy, "Cue", &buggy.init_state());
    for round in 1..=budget + 1 {
        s = fire(&buggy, "DeviceFaults", &s);
        s = fire(&buggy, "StaleElapses", &s);
        s = fire(&buggy, "Cue", &s);
        s = fire(&buggy, "CueSounds", &s);
        assert_eq!((s["attempts"], s["spent"]), (0, round), "round {round}");
    }
    assert_eq!(s["failed"], 0);
    assert!(!buggy.check_invariant("RecoveryIsBounded", &s));
    assert!(!buggy.check_invariant("ReopensLeftIsTruthful", &s));
}

/// An edit to the reset law (the healthy window finishing without a delivery, or the budget
/// handed back on any delivery) is refuted at the committed and shipping budgets, because the
/// law reads its own `evidence` record.
#[test]
fn trail_audio_reopen_ladder_refutes_edits_to_its_own_reset_law() {
    use aterm_spec::derive::Update;
    let committed = interp::with_buggy(&trail_audio_reopen_ladder_model(), 0);
    let var = |n: &'static str| Box::new(Expr::Var(n));
    let int = |v: i64| Box::new(Expr::Int(v));
    // `if device == 1 { 0 } else { <var> }`.
    let on_any_delivery =
        |v: &'static str| Expr::If(Box::new(Expr::Eq(var("device"), int(1))), int(0), var(v));
    let set = |m: &mut Model, action: &str, v: &'static str, expr: Expr| {
        let a = m
            .actions
            .iter_mut()
            .find(|a| a.name == action)
            .expect("action");
        let u: &mut Update = a.updates.iter_mut().find(|u| u.var == v).expect("update");
        u.expr = expr;
    };

    let mut unstarted_window = committed.clone();
    unstarted_window
        .actions
        .iter_mut()
        .find(|a| a.name == "HealthyWindowElapses")
        .expect("action")
        .guard = None;

    let mut eager_code = committed.clone();
    set(
        &mut eager_code,
        "CueSounds",
        "attempts",
        on_any_delivery("attempts"),
    );

    let mut eager_both = eager_code.clone();
    set(
        &mut eager_both,
        "CueSounds",
        "spent",
        on_any_delivery("spent"),
    );

    for (label, edited) in [
        ("the window finishes without a delivery", unstarted_window),
        ("the code resets on any delivery", eager_code),
        ("both counters reset on any delivery", eager_both),
    ] {
        for budget in [2, 6] {
            // Checked alone, so the refutation is this invariant's own.
            let mut at = interp::with_consts(&edited, &[("Budget", budget)]);
            at.invariants
                .retain(|i| i.name == "ResetOnlyOnPlayedWindow");
            let (state, inv) = interp::bmc(&at).expect_err(label);
            eprintln!("{label} @Budget={budget}: refuted by {inv} at {state:?}");
        }
    }
    // The committed machine proves at both budgets.
    for budget in [2, 6] {
        interp::bmc(&interp::with_consts(&committed, &[("Budget", budget)]))
            .unwrap_or_else(|e| panic!("committed @Budget={budget}: {e:?}"));
    }
}
