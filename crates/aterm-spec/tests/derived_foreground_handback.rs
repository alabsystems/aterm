// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 for `ForegroundHandback` (the 2026-09-25 "crashed" tab): the
//! in-stream handback proves over the whole bounded space, and the designs it
//! replaced are caught — no handback with the restore left to the UI-thread
//! status sweep (`Buggy = 1`, on every invariant; without a sweep it is the
//! incident itself) and batch-end-only sampling (`Split = 0`), and the two
//! designs the 2026-09-25 review caught: a handback at EVERY foreground
//! change, which strips a stopped (or job-controlling) program's modes while
//! it still runs (`Alive = 0`), and a restarted reader seeded with a fresh
//! probe, which never sees a job that died while the readers were parked
//! (`Carry = 0`). And the rule the second 2026-09-25 review caught: display
//! modes alone counted as orphaned, so a one-shot `tput smcup` or `tput civis`
//! was reverted the moment `tput` exited (`Display = 1`). Tier-1 — the real
//! PTY reader with a scripted foreground probe — is
//! `aterm-gui/src/foreground_handback_conformance.rs`.

use aterm_spec::derive::{Model, foreground_handback_model, foreground_handback_ownership_model};
use aterm_spec::{interp, verify};

/// The incident: zle turns 2004 off, the job runs and writes, the reader
/// delivers and parses it, the job dies, the shell reclaims and draws its
/// prompt (`2004h` included), and the reader delivers and parses that.
const INCIDENT: [&str; 13] = [
    "Launch",
    "JobWrite",
    "ReadJob",
    "Park",
    "Deliver",
    "Parse",
    "Die",
    "Reclaim",
    "ShellWrite",
    "ReadShell",
    "Park",
    "Deliver",
    "Parse",
];

/// The adopted session: the reader attaches while the job already holds the
/// terminal with its modes in force (the incident tab was exactly this).
const ADOPTED: [&str; 8] = [
    "Adopt",
    "Die",
    "Reclaim",
    "ShellWrite",
    "ReadShell",
    "Park",
    "Deliver",
    "Parse",
];

/// Ctrl-Z of a job that armed its modes, the shell's `zsh: suspended` prompt,
/// `fg`, the job's repaint, and only then its death and the shell's reclaim.
/// The stop edge keeps the modes (the job lives); the death edge hands back.
const STOP_FG: [&str; 25] = [
    "Launch",
    "JobWrite",
    "ReadJob",
    "Park",
    "Deliver",
    "Parse",
    "Stop",
    "Reclaim",
    "ShellWrite",
    "ReadShell",
    "Park",
    "Deliver",
    "Parse",
    "Resume",
    "JobWrite",
    "ReadJob",
    "Park",
    "Deliver",
    "Parse",
    "Die",
    "Reclaim",
    "ShellWrite",
    "ReadShell",
    "Park",
    "Deliver",
];

/// The incident with the readers parked across the death: the job dies and
/// the shell reclaims and writes its prompt while no reader runs; a new reader
/// is attached and reads the prompt.
const PARKED: [&str; 14] = [
    "Launch",
    "JobWrite",
    "ReadJob",
    "Park",
    "Deliver",
    "Parse",
    "Die",
    "Reclaim",
    "ShellWrite",
    "Restart",
    "ReadShell",
    "Park",
    "Deliver",
    "Parse",
];

/// A one-shot that sets a DISPLAY mode on purpose (`tput smcup`, `tput
/// civis`) and exits cleanly, then the shell's prompt: the incident's
/// schedule with `OneShot` in place of `Launch`.
const ONE_SHOT: [&str; 13] = [
    "OneShot",
    "JobWrite",
    "ReadJob",
    "Park",
    "Deliver",
    "Parse",
    "Die",
    "Reclaim",
    "ShellWrite",
    "ReadShell",
    "Park",
    "Deliver",
    "Parse",
];

fn only(m: &Model, invariant: &str) -> Model {
    let mut m = m.clone();
    m.invariants.retain(|i| i.name == invariant);
    m
}

#[test]
fn foreground_handback_proves_catches_and_has_no_dead_action() {
    let model = foreground_handback_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the foreground handback must stay enrolled in the spec-link registry"
    );
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires at the committed configuration: {fired:?}"
    );
    assert_eq!(verify::audit_dead_negative_controls(&model, &[]), Ok(0));
    verify::prove_and_catch_scalar(
        &model,
        "foreground handback: the prompt is never hijacked and keeps its own modes",
    );
}

#[test]
fn foreground_handback_the_ui_sweep_design_is_caught_on_every_invariant() {
    let m = interp::with_buggy(&foreground_handback_model(), 1);
    // The sweep runs after the prompt drew: it switches the shell's 2004 off.
    let (a, _) = interp::bmc(&only(&m, "ShellKeepsItsOwnModes")).expect_err("paste clobbered");
    assert_eq!((a["prompt"], a["paste"], a["swept"]), (1, 0, 1), "{a:?}");
    // The sweep has not run yet when the prompt is parsed: the window is open.
    let (b, _) = interp::bmc(&only(&m, "PromptNotHijacked")).expect_err("prompt hijacked");
    assert_eq!((b["prompt"], b["leak"], b["tail"]), (1, 1, 0), "{b:?}");
    // The sweep cannot tell a stopped job from a dead one: it strips the
    // modes of a job that still lives.
    let (c, _) = interp::bmc(&only(&m, "JobKeepsItsModesWhileAlive")).expect_err("stripped");
    assert_eq!((c["life"], c["armed"], c["leak"]), (5, 1, 0), "{c:?}");
}

#[test]
fn foreground_handback_batch_end_only_sampling_is_caught() {
    let m = interp::with_consts(&foreground_handback_model(), &[("Split", 0)]);
    let (state, invariant) = interp::bmc(&m).expect_err("a mixed batch has no cut");
    assert_eq!(
        (invariant, state["tail"]),
        ("PromptNotHijacked", 0),
        "{state:?}"
    );
}

#[test]
fn foreground_handback_incident_and_adopted_schedules_committed_vs_buggy() {
    let m = foreground_handback_model();
    let b = interp::with_buggy(&m, 1);
    for schedule in [&INCIDENT[..], &ADOPTED[..]] {
        let mut fixed = m.init_state();
        let mut broken = b.init_state();
        for action in schedule {
            assert!(m.fire(action, &mut fixed), "{action}: {fixed:?}");
            assert!(b.fire(action, &mut broken), "{action}: {broken:?}");
        }
        assert_eq!(
            (
                fixed["prompt"],
                fixed["leak"],
                fixed["paste"],
                fixed["tail"]
            ),
            (1, 0, 1, 0),
            "{schedule:?}"
        );
        // The 2026-09-25 terminal: the prompt under the dead program's modes.
        assert_eq!(
            (broken["prompt"], broken["leak"], broken["tail"]),
            (1, 1, 0),
            "{schedule:?}"
        );
        assert!(!m.check_invariant("PromptNotHijacked", &broken));
    }
}

#[test]
fn foreground_handback_a_stopped_job_keeps_its_modes_until_it_dies() {
    let m = foreground_handback_model();
    let mut s = m.init_state();
    for (i, action) in STOP_FG.iter().enumerate() {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
        if i == 12 {
            // The stop edge was parsed: the job lives, so its modes stay —
            // under the shell's `zsh: suspended`, as before the handback.
            assert_eq!((s["life"], s["leak"], s["prompt"]), (5, 1, 1), "{s:?}");
        }
        if i == 18 {
            // `fg`: the job runs with the modes it armed.
            assert_eq!((s["life"], s["leak"]), (1, 1), "{s:?}");
        }
    }
    assert!(m.fire("Parse", &mut s));
    // The death edge: handed back, and the shell's 2004h kept.
    assert_eq!(
        (s["life"], s["prompt"], s["leak"], s["paste"], s["tail"]),
        (3, 1, 0, 1, 0),
        "{s:?}"
    );
}

#[test]
fn foreground_handback_at_every_foreground_change_is_caught() {
    let m = interp::with_consts(&foreground_handback_model(), &[("Alive", 0)]);
    let (state, invariant) = interp::bmc(&m).expect_err("a live job's modes stripped");
    assert_eq!(invariant, "JobKeepsItsModesWhileAlive", "{state:?}");
    assert_eq!((state["armed"], state["leak"]), (1, 0), "{state:?}");
    assert!(
        [1, 4, 5].contains(&state["life"]),
        "the job is alive: {state:?}"
    );

    // The review's probe, replayed: Ctrl-Z strips the modes, `fg` resumes the
    // job without them.
    let mut s = m.init_state();
    for action in &STOP_FG[..14] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["life"], s["leak"], s["armed"]), (1, 0, 1), "{s:?}");
    assert!(!m.check_invariant("JobKeepsItsModesWhileAlive", &s));
}

#[test]
fn foreground_handback_a_restarted_reader_without_the_carried_holder_is_caught() {
    let committed = foreground_handback_model();
    let mut s = committed.init_state();
    for action in PARKED {
        assert!(committed.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!(
        (s["prompt"], s["leak"], s["paste"], s["tail"]),
        (1, 0, 1, 0),
        "the carried holder makes the death an edge at the new reader's first sample"
    );

    let m = interp::with_consts(&committed, &[("Carry", 0)]);
    let (state, invariant) = interp::bmc(&m).expect_err("the death is never an edge");
    assert_eq!(
        (invariant, state["tail"]),
        ("PromptNotHijacked", 0),
        "{state:?}"
    );
    let mut s = m.init_state();
    for action in PARKED {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    // The 2026-09-25 terminal again: no edge, no handback.
    assert_eq!((s["prompt"], s["leak"], s["tail"]), (1, 1, 0), "{s:?}");
    assert!(!committed.check_invariant("PromptNotHijacked", &s));
}

#[test]
fn foreground_handback_a_one_shot_keeps_its_display_modes() {
    let committed = foreground_handback_model();
    let mut s = committed.init_state();
    for action in ONE_SHOT {
        assert!(committed.fire(action, &mut s), "{action}: {s:?}");
    }
    // `tput smcup` exited; its alt screen is kept under the shell (the
    // wrapper's `cmd` draws there), and the shell's own 2004h is in force.
    assert_eq!(
        (s["hij"], s["prompt"], s["leak"], s["paste"], s["life"]),
        (0, 1, 1, 1, 3),
        "{s:?}"
    );
    for invariant in [
        "PromptNotHijacked",
        "ShellKeepsItsOwnModes",
        "JobKeepsItsModesWhileAlive",
        "OneShotKeepsItsModes",
    ] {
        assert!(committed.check_invariant(invariant, &s), "{invariant}");
    }

    // The reviewed rule: display modes alone are orphaned, and the one-shot's
    // mode is reverted at the cut.
    let m = interp::with_consts(&committed, &[("Display", 1)]);
    let (state, invariant) = interp::bmc(&m).expect_err("a one-shot's mode reverted");
    assert_eq!(invariant, "OneShotKeepsItsModes", "{state:?}");
    assert_eq!(
        (state["hij"], state["armed"], state["leak"]),
        (0, 1, 0),
        "{state:?}"
    );
    let mut s = m.init_state();
    for action in ONE_SHOT {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["prompt"], s["leak"], s["paste"]), (1, 0, 1), "{s:?}");
    assert!(!committed.check_invariant("OneShotKeepsItsModes", &s));

    // The display-only exemption never reaches the incident: an
    // input-hijacking job is still handed back at `Display = 0`.
    let mut s = committed.init_state();
    for action in INCIDENT {
        assert!(committed.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["hij"], s["leak"]), (1, 0), "{s:?}");
}

/// The 2026-09-27 lane at load 59-65: a one-shot the reader never saw (its
/// `?1000h` parsed as the shell's), then a job that arms mouse tracking in
/// its own bytes and dies.
const MISSED_THEN_JOB: [&str; 9] = [
    "Launch", "Arm", "Die", "Reclaim", "Launch", "Sample", "Arm", "Die", "Reclaim",
];

/// zsh's builtin `printf '\e[?1000h'` (the shell's own bytes), then the job.
const SHELL_ARM_THEN_JOB: [&str; 6] = ["ShellArm", "Launch", "Sample", "Arm", "Die", "Reclaim"];

#[test]
fn foreground_handback_ownership_proves_catches_and_has_no_dead_action() {
    let model = foreground_handback_ownership_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the ownership model must stay enrolled in the spec-link registry"
    );
    let fired = interp::fired_actions(&model);
    assert_eq!(
        fired.len(),
        model.actions.len(),
        "every action fires at the committed configuration: {fired:?}"
    );
    assert_eq!(verify::audit_dead_negative_controls(&model, &[]), Ok(0));
    verify::prove_and_catch_scalar(
        &model,
        "foreground handback ownership: a missed job loses only its own handback",
    );
}

#[test]
fn foreground_handback_ownership_a_missed_job_loses_only_its_own_handback() {
    let committed = foreground_handback_ownership_model();
    let buggy = interp::with_buggy(&committed, 1);
    for (schedule, name) in [
        (&MISSED_THEN_JOB[..], "missed one-shot"),
        (&SHELL_ARM_THEN_JOB[..], "the shell's own printf"),
    ] {
        let mut s = committed.init_state();
        for action in schedule {
            assert!(committed.fire(action, &mut s), "{name}: {action}: {s:?}");
        }
        // The job that armed in its own bytes is handed back: the session
        // recovered.
        assert_eq!(
            (s["life"], s["done"], s["bit"], s["owner"], s["backs"]),
            (0, 1, 0, 0, 1),
            "{name}: {s:?}"
        );

        // The replaced rule: the job's re-arm leaves the shell the owner, and
        // its death hands nothing back.
        let mut b = buggy.init_state();
        for action in schedule {
            assert!(buggy.fire(action, &mut b), "{name}: {action}: {b:?}");
        }
        assert_eq!(
            (b["life"], b["done"], b["bit"], b["owner"], b["backs"]),
            (0, 1, 1, 1, 0),
            "{name}: {b:?}"
        );
        assert!(!committed.check_invariant("ObservedArmIsHandedBack", &b));
    }

    // The missed one-shot itself is the residual: its own handback is lost.
    let mut s = committed.init_state();
    for action in &MISSED_THEN_JOB[..4] {
        assert!(committed.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!(
        (s["done"], s["bit"], s["owner"], s["backs"]),
        (0, 1, 1, 0),
        "{s:?}"
    );

    // bmc finds the replaced rule's counterexample on its own.
    let (state, invariant) = interp::bmc(&buggy).expect_err("the stuck session");
    assert_eq!(invariant, "ObservedArmIsHandedBack", "{state:?}");
    assert_eq!((state["done"], state["bit"]), (1, 1), "{state:?}");
}
