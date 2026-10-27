// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `VzLifecycle` (`aterm_models_misc::vz_lifecycle_model`) on the interpreter: the model
//! holds, each law is broken by its own mutants alone and by no other, and §5.1's and §12's
//! orders read as the design writes them.

use std::collections::BTreeSet;

use aterm_models_misc::vz_lifecycle_model;
use aterm_spec::derive::{InvariantKind, Model};
use aterm_spec::{interp, verify};

type State = std::collections::BTreeMap<&'static str, i64>;

/// Each law and the mutants that break it, alone on the `Buggy = 1` baseline.
const LAWS: [(&str, &[&str]); 10] = [
    ("LiveAttachedOnce", &["SecondBoots"]),
    ("SealedNeverAttached", &["AttachBaked"]),
    ("ClonedOnlyWhenSealed", &["ClonePartial"]),
    ("SealedOnlyWhenFinished", &["SealHalfMade"]),
    (
        "NothingPreparedUnderAVm",
        &["PrepareUnderVm", "UpWithoutLock"],
    ),
    ("EveryExecEndsOnce", &["DropExit", "ExecBooting"]),
    ("ReleaseRule", &["KeepRunning"]),
    ("HardStopOnlyWhenAsked", &["HardUnasked"]),
    ("SignalOnlyTheRecordedHelper", &["SignalReused"]),
    ("ReadyOnlyForThePlansAgent", &["ReadyUnchecked"]),
];

fn fire(m: &Model, s: &mut State, actions: &[&str]) {
    for action in actions {
        assert!(m.fire(action, s), "{action} is enabled at {s:?}");
    }
}

#[test]
fn the_lifecycle_holds_and_every_action_fires() {
    let m = vz_lifecycle_model();
    assert!(interp::bmc(&m).is_ok(), "VzLifecycle holds");
    let fired = interp::fired_actions(&m);
    let healthy: BTreeSet<&str> = m
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|a| !LAWS.iter().any(|(_, mutants)| mutants.contains(a)))
        .collect();
    assert_eq!(fired, healthy, "every healthy action fires, and no mutant");
    assert!(verify::sweep_faults(&[m]).is_empty(), "the registry sweep");
}

#[test]
fn each_law_is_broken_by_its_own_mutants_alone() {
    let m = vz_lifecycle_model();
    for (law, mutants) in LAWS {
        for mutant in mutants {
            let alone = verify::buggy_baseline_with(&m, mutant);
            let broken = interp::violated_invariants(&alone);
            assert_eq!(
                broken.into_iter().collect::<Vec<_>>(),
                [law],
                "`{mutant}` alone breaks `{law}` and nothing else"
            );
        }
    }
    let all: BTreeSet<&str> = LAWS.iter().map(|(law, _)| *law).collect();
    let named: BTreeSet<&str> = m
        .invariants
        .iter()
        .filter(|i| i.kind == InvariantKind::Claim)
        .map(|i| i.name)
        .collect();
    assert_eq!(all, named, "every law has a mutant");
}

#[test]
fn a_bake_seals_before_its_clone_and_the_live_disk_is_kept() {
    let m = vz_lifecycle_model();
    let mut s = m.init_state();
    fire(&m, &mut s, &["UpLocks", "Start", "Attach", "Prepare"]);
    assert_eq!(
        (s["bake"], s["live"], s["disk"]),
        (1, 0, 1),
        "a bake begins"
    );
    // Provisioned, then the guest's own power-off (§4's step 4).
    fire(&m, &mut s, &["Boot", "Hello", "Ready", "Stopped"]);
    assert!(
        !m.action_enabled("Prepare", &s),
        "no prepare with no helper"
    );
    fire(&m, &mut s, &["Seal"]);
    assert_eq!(
        (s["bake"], s["disk"]),
        (2, 0),
        "sealed under its final name"
    );
    fire(
        &m,
        &mut s,
        &["UpUnlocks", "UpLocks", "Start", "Attach", "Prepare"],
    );
    assert_eq!(
        (s["live"], s["disk"]),
        (1, 2),
        "live cloned from the sealed disk"
    );
    fire(&m, &mut s, &["Boot", "Hello", "Ready"]);
    assert!(
        !m.action_enabled("Prepare", &s),
        "nothing is prepared under a VM"
    );
    fire(&m, &mut s, &["Detach"]);
    assert_eq!(
        (s["phase"], s["asked"]),
        (3, 3),
        "the release rule stops it"
    );
    fire(
        &m,
        &mut s,
        &["GraceOut", "Stopped", "UpUnlocks", "UpLocks", "Start"],
    );
    fire(&m, &mut s, &["AttachKeep", "Prepare"]);
    assert_eq!(
        (s["bake"], s["live"], s["disk"]),
        (2, 1, 2),
        "the live disk is kept"
    );
}

#[test]
fn keep_holds_the_vm_and_stop_ends_it_and_the_helper() {
    let m = vz_lifecycle_model();
    let mut s = m.init_state();
    fire(
        &m,
        &mut s,
        &["UpLocks", "Start", "AttachKeep", "Attach", "Prepare"],
    );
    fire(&m, &mut s, &["Boot", "Hello", "Ready", "Detach", "Detach"]);
    assert_eq!(
        (s["helper"], s["phase"], s["sessions"]),
        (1, 2, 0),
        "keep holds it"
    );
    fire(
        &m,
        &mut s,
        &["UpRefused", "Attach", "KeepClear", "ExecBegin"],
    );
    fire(&m, &mut s, &["Stop"]);
    assert_eq!((s["phase"], s["keep"], s["asked"]), (3, 0, 2));
    fire(&m, &mut s, &["ExecExit", "Stopped"]);
    assert_eq!((s["helper"], s["phase"], s["outcome"]), (0, 4, 1));
    let mut s = m.init_state();
    fire(
        &m,
        &mut s,
        &["UpLocks", "Start", "Attach", "Prepare", "Boot", "Hello"],
    );
    fire(&m, &mut s, &["Ready", "ExecBegin", "Detect"]);
    assert_eq!(
        (s["phase"], s["hard"], s["asked"]),
        (3, 1, 1),
        "a detector stops it hard"
    );
    assert!(!m.action_enabled("Detect", &s), "once");
    fire(&m, &mut s, &["StoppedWithError"]);
    assert_eq!(
        (s["phase"], s["exec"], s["outcome"]),
        (5, 0, 2),
        "the command is Lost"
    );
}

#[test]
fn the_wrong_agent_never_reaches_ready_and_a_failed_boot_ends_with_its_sessions() {
    let m = vz_lifecycle_model();
    let mut s = m.init_state();
    fire(
        &m,
        &mut s,
        &[
            "UpLocks",
            "Start",
            "Attach",
            "Prepare",
            "Boot",
            "HelloWrong",
        ],
    );
    assert_eq!(
        (s["phase"], s["asked"], s["hard"]),
        (3, 1, 0),
        "stopped, never Ready"
    );
    fire(&m, &mut s, &["AttachRefused", "GraceOut", "Stopped"]);
    let mut s = m.init_state();
    fire(
        &m,
        &mut s,
        &["UpLocks", "Start", "Attach", "Prepare", "BootRefused"],
    );
    assert_eq!((s["helper"], s["phase"]), (1, 5));
    fire(&m, &mut s, &["AttachRefused", "Detach"]);
    assert_eq!(s["helper"], 0, "the last session's close ends the helper");
    let mut s = m.init_state();
    fire(&m, &mut s, &["Start"]);
    fire(&m, &mut s, &["SecondRefused", "Quiet"]);
    assert_eq!(s["helper"], 0, "no session within a minute");
}

#[test]
fn a_hung_helper_is_escalated_and_a_dead_ones_leftovers_go_unsignalled() {
    let m = vz_lifecycle_model();
    let mut s = m.init_state();
    fire(
        &m,
        &mut s,
        &["UpLocks", "Start", "Attach", "Prepare", "Boot", "Hello"],
    );
    fire(
        &m,
        &mut s,
        &["Ready", "ExecBegin", "Hang", "SecondRefused", "Escalate"],
    );
    assert_eq!((s["helper"], s["phase"], s["outcome"]), (0, 4, 2));
    let mut s = m.init_state();
    fire(&m, &mut s, &["Start", "Die", "PidReused"]);
    assert!(
        !m.action_enabled("Escalate", &s),
        "a reused pid is never signalled"
    );
    fire(&m, &mut s, &["ClearDead", "Start"]);
    assert_eq!(s["helper"], 1);
}

#[test]
fn a_bake_is_sealed_only_once_it_finished() {
    let m = vz_lifecycle_model();
    let bake = ["UpLocks", "Start", "Attach", "Prepare", "Boot"];
    // A detector before the hello, after it, or after Ready; a stop before Ready; after
    // Ready, `down`'s `Stop` or the release rule (soft or hard after its grace) rather than
    // the guest's own power-off; a hard stop that never finished: each leaves the partial
    // file for the next Prepare.
    for end in [
        &["Detect", "Stopped"][..],
        &["Hello", "Detect", "Stopped"],
        &["Hello", "Ready", "Detect", "Stopped"],
        &["Hello", "Ready", "ExecBegin", "Detect", "StoppedWithError"],
        &["Hello", "Stop", "Stopped"],
        &["Stop", "GraceOut", "Stopped"],
        &["Hello", "Ready", "Stop", "Stopped"],
        &["Hello", "Ready", "Stop", "GraceOut", "Stopped"],
        &["Hello", "Ready", "Detach", "Stopped"],
        &["Hello", "Ready", "Detach", "GraceOut", "Stopped"],
        &["Hello", "Ready", "Stop", "GraceOut", "HardTimeout"],
        &["HelloWrong", "Stopped"],
        &["Hello", "Ready", "HelloWrong", "Stopped"],
    ] {
        let mut s = m.init_state();
        fire(&m, &mut s, &bake);
        fire(&m, &mut s, end);
        assert_eq!(s["bake"], 1, "{end:?}");
        assert!(!m.action_enabled("Seal", &s), "{end:?} is never sealed");
        fire(
            &m,
            &mut s,
            &["UpUnlocks", "UpLocks", "Start", "Attach", "Prepare"],
        );
        assert_eq!(
            (s["bake"], s["live"], s["disk"]),
            (1, 0, 1),
            "{end:?}: the next bring-up bakes again, cloning nothing"
        );
    }
    // Ready, then the lab's power-off through the agent: the guest's own, with no stop asked
    // (the helper's `POWERED_OFF`), and with a session left or none (`keep` set).
    for end in [&["Stopped"][..], &["KeepSet", "Detach", "Stopped"]] {
        let mut s = m.init_state();
        fire(&m, &mut s, &bake);
        fire(&m, &mut s, &["Hello", "Ready"]);
        fire(&m, &mut s, end);
        assert_eq!(s["ran"], 2, "{end:?}");
        fire(&m, &mut s, &["Seal"]);
        assert_eq!(s["bake"], 2, "{end:?}");
    }
    // The mutant seals what a detector stopped, and what `down`'s `Stop` stopped after Ready.
    let buggy = interp::with_buggy(&m, 1);
    for end in [
        &["Detect", "Stopped"][..],
        &["Hello", "Ready", "Stop", "Stopped"],
    ] {
        let mut s = buggy.init_state();
        fire(&buggy, &mut s, &bake);
        fire(&buggy, &mut s, end);
        fire(&buggy, &mut s, &["SealHalfMade"]);
        assert!(
            !buggy.check_invariant("SealedOnlyWhenFinished", &s),
            "{end:?}"
        );
    }
}

#[test]
fn a_command_runs_only_once_ready_and_an_exit_is_never_lost() {
    let m = vz_lifecycle_model();
    let mut s = m.init_state();
    fire(
        &m,
        &mut s,
        &["UpLocks", "Start", "Attach", "Prepare", "Boot", "Hello"],
    );
    assert!(
        !m.action_enabled("ExecBegin", &s),
        "no command while booting"
    );
    let buggy = interp::with_buggy(&m, 1);
    let mut b = s.clone();
    fire(&buggy, &mut b, &["ExecBooting"]);
    assert!(!buggy.check_invariant("EveryExecEndsOnce", &b));
    // A command in flight on a stopping VM: its Exit still ends it, and the mutant that
    // drops it breaks the law there too, not only on a Ready VM.
    fire(&m, &mut s, &["Ready", "ExecBegin", "Stop"]);
    assert_eq!((s["phase"], s["exec"]), (3, 1));
    let mut b = s.clone();
    fire(&buggy, &mut b, &["DropExit"]);
    assert_eq!((b["phase"], b["outcome"]), (3, 3));
    assert!(!buggy.check_invariant("EveryExecEndsOnce", &b));
    fire(&m, &mut s, &["ExecExit"]);
    assert_eq!(s["outcome"], 1);
}

#[test]
fn a_hard_stop_that_never_finishes_ends_the_helper_and_its_commands() {
    let m = vz_lifecycle_model();
    let mut s = m.init_state();
    fire(
        &m,
        &mut s,
        &["UpLocks", "Start", "Attach", "Prepare", "Boot", "Hello"],
    );
    fire(&m, &mut s, &["Ready", "ExecBegin", "Detect", "HardTimeout"]);
    assert_eq!(
        (
            s["helper"],
            s["phase"],
            s["sessions"],
            s["exec"],
            s["outcome"]
        ),
        (0, 4, 0, 0, 2)
    );
    assert!(!m.action_enabled("HardTimeout", &s));
    fire(&m, &mut s, &["Start"]);
    let mut s = m.init_state();
    fire(
        &m,
        &mut s,
        &["UpLocks", "Start", "Attach", "Prepare", "Boot", "Hello"],
    );
    fire(&m, &mut s, &["Ready", "Stop"]);
    assert!(
        !m.action_enabled("HardTimeout", &s),
        "only once stopped hard"
    );
}

#[test]
fn the_wrong_agents_hello_after_ready_stops_the_vm() {
    let m = vz_lifecycle_model();
    let mut s = m.init_state();
    fire(
        &m,
        &mut s,
        &["UpLocks", "Start", "Attach", "Prepare", "Boot", "Hello"],
    );
    fire(&m, &mut s, &["Ready", "HelloWrong"]);
    assert_eq!(
        (s["phase"], s["asked"], s["hard"], s["hello"]),
        (3, 1, 0, 2)
    );
    assert!(
        !m.action_enabled("HelloWrong", &s),
        "a stopping VM reads no frame"
    );
}
