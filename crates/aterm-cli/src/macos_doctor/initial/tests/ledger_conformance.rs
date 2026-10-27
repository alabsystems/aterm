// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance of the setup run's ledger and the kept-by-person record to `SetupLedger`
//! (`aterm_models_setup::setup_ledger_model`, design §6.9).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_models_setup::setup_ledger_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::xref::StepEvidence;

use super::*;
use ledger::{Admit, Ledger};

const PROJECTED: [&str; 16] = [
    "alive",
    "done_a",
    "run_a",
    "att_a",
    "park_a",
    "ever_a",
    "quit_a",
    "done_b",
    "run_b",
    "att_b",
    "park_b",
    "ever_b",
    "quit_b",
    "applied",
    "kept",
    "wrote_kept",
];

const ANCHORED: [&str; 16] = [
    "ApplyKey",
    "BeginA",
    "BeginB",
    "CheckAgain",
    "EndA",
    "EndB",
    "FailA",
    "FailB",
    "GroupOn",
    "HangA",
    "HangB",
    "PersonChanges",
    "Quit",
    "Relaunch",
    "Restore",
    "UnattendedPass",
];

const MUTANTS: [&str; 4] = [
    "RerunDone",
    "RelaunchForgetsDone",
    "RelaunchNeverParks",
    "ApplyKept",
];

/// The model's two stages: one of the Spotlight lane and one of the vault lane, which lanes run
/// at once.
const A: Stage = Stage::Spotlight;
const B: Stage = Stage::Repositories;

/// The one key the model follows: the Dock's.
const KEY: &str = "init.dock@conformance";

const BOOT: u128 = 7;
const NOW: u64 = 1_800_000_000;

/// The real ledger and record, and what the model reads of the environment: whether the
/// process lives, what the run's progress has done, and whether a pass wrote the kept key.
#[derive(Default)]
struct Rig {
    ledger: Ledger,
    kept: kept::Kept,
    dead: bool,
    done: u16,
    ever: u16,
    wrote_kept: bool,
}

impl Rig {
    fn new() -> Self {
        Self {
            ledger: Ledger::of(1),
            ..Self::default()
        }
    }

    /// What a launch reads back of the ledger and the record a process ended with.
    fn read_back(&mut self) {
        self.ledger = Ledger::parse(&self.ledger.record()).expect("the ledger reads back");
        self.kept = kept::Kept::parse(&self.kept.record()).expect("the record reads back");
    }
}

fn dock() -> Selection {
    let mut dock = Selection::none();
    dock.set(Setting::Dock, true);
    dock
}

fn bit(bits: u16, stage: Stage) -> i64 {
    i64::from(bits & stage.bit() != 0)
}

fn project(rig: &Rig, model_state: &State) -> State {
    let mut state = model_state.clone();
    let ledger = &rig.ledger;
    state.insert("alive", i64::from(!rig.dead));
    for (stage, done, run, att, park, ever, quit) in [
        (A, "done_a", "run_a", "att_a", "park_a", "ever_a", "quit_a"),
        (B, "done_b", "run_b", "att_b", "park_b", "ever_b", "quit_b"),
    ] {
        state.insert(done, bit(rig.done, stage));
        state.insert(run, bit(ledger.running, stage));
        state.insert(att, i64::from(ledger.attempt(stage).began));
        state.insert(park, bit(ledger.crashed | ledger.hung, stage));
        state.insert(ever, bit(rig.ever, stage));
        state.insert(quit, bit(ledger.quit, stage));
    }
    state.insert("applied", i64::from(rig.kept.applied(KEY)));
    state.insert("kept", i64::from(rig.kept.keeps(KEY)));
    state.insert("wrote_kept", i64::from(rig.wrote_kept));
    state
}

fn drive(rig: &mut Rig, action: &'static str, ev: &mut StepEvidence) {
    let stage = |action: &str| if action.ends_with('A') { A } else { B };
    match action {
        "BeginA" | "BeginB" => {
            let stage = stage(action);
            let done = rig.done;
            let admitted = ev.step(action, || rig.ledger.begin(stage, done, Some(BOOT), NOW));
            assert_eq!(admitted, Admit::Run, "{action}");
        }
        "EndA" | "EndB" => {
            let stage = stage(action);
            // As the pass saves its progress with the stage done, then ends it in the ledger.
            rig.done |= stage.bit();
            rig.ever |= stage.bit();
            ev.step(action, || rig.ledger.end(stage));
        }
        "FailA" | "FailB" => {
            let stage = stage(action);
            ev.step(action, || rig.ledger.end(stage));
        }
        "HangA" | "HangB" => {
            let stage = stage(action);
            let at = rig.ledger.attempt(stage).at;
            assert!(
                ev.step(action, || rig.ledger.mark_hung(stage, at)),
                "{action}"
            );
        }
        "CheckAgain" => {
            ev.step(action, || rig.ledger.retry());
        }
        "Crash" => rig.dead = true,
        "Quit" => {
            ev.step(action, || rig.ledger.interrupt());
            rig.dead = true;
        }
        "Relaunch" => {
            rig.read_back();
            let done = rig.done;
            ev.step(action, || rig.ledger.recover(done, None));
            rig.dead = false;
        }
        "ApplyKey" => {
            ev.step(action, || {
                rig.kept.note(
                    &[KEY.to_owned()],
                    &[(KEY.to_owned(), "as aterm sets it".to_owned())],
                    &[],
                    &|_| Some(Setting::Dock),
                )
            });
        }
        "PersonChanges" => {
            ev.step(action, || {
                rig.kept.note(
                    &[KEY.to_owned()],
                    &[(KEY.to_owned(), "as the person moved it".to_owned())],
                    &[KEY.to_owned()],
                    &|_| Some(Setting::Dock),
                )
            });
        }
        "UnattendedPass" => {
            let mut holds = Vec::new();
            ev.step(action, || rig.kept.hold(&mut holds, &|_| SETTLED));
            // The pass writes the key unless it is held: a write of a kept key is the defect.
            let held = holds
                .iter()
                .any(|(item, hold)| item == KEY && matches!(hold, Hold::AsIs(_)));
            if !held && rig.kept.keeps(KEY) {
                rig.wrote_kept = true;
            }
        }
        "Restore" => {
            ev.step(action, || rig.kept.forget_all());
        }
        "GroupOn" => {
            ev.step(action, || rig.kept.forget(dock()));
        }
        other => panic!("no real step for `{other}`"),
    }
}

fn replay(
    model: &Model,
    schedule: &[&'static str],
    ev: &mut StepEvidence,
    stand_in: &dyn Fn(&mut Rig, &str) -> bool,
) -> State {
    let mut rig = Rig::new();
    let mut state = model.init_state();
    let mut real = project(&rig, &state);
    assert_eq!(real, state, "a fresh run is the model's start");
    for &action in schedule {
        assert!(
            model.fire(action, &mut state),
            "{action} is enabled after {schedule:?}: {state:?}"
        );
        if !stand_in(&mut rig, action) {
            drive(&mut rig, action, ev);
        }
        real = project(&rig, &state);
        for var in PROJECTED {
            assert_eq!(
                real[var], state[var],
                "`{var}` after {action} in {schedule:?}: real {real:?}, model {state:?}"
            );
        }
    }
    real
}

fn transitions(model: &Model) -> Vec<Vec<&'static str>> {
    let mut path: BTreeMap<State, Vec<&'static str>> = BTreeMap::new();
    let mut queue = VecDeque::new();
    path.insert(model.init_state(), Vec::new());
    queue.push_back(model.init_state());
    let mut out = Vec::new();
    while let Some(state) = queue.pop_front() {
        for action in model.actions.iter().map(|a| a.name) {
            for next in model.successors(action, &state) {
                let mut schedule = path[&state].clone();
                schedule.push(action);
                out.push(schedule.clone());
                if !path.contains_key(&next) {
                    path.insert(next.clone(), schedule);
                    queue.push_back(next);
                }
            }
        }
    }
    out
}

#[test]
fn the_ledger_and_the_kept_record_conform_to_the_model() {
    ledger::setup_ledger_environment();
    let model = interp::with_buggy(&setup_ledger_model(), 0);
    let mut ev = StepEvidence::new("SetupLedger");
    let schedules = transitions(&model);
    assert!(
        schedules.len() > 100,
        "the walk covers the machine: {} transitions",
        schedules.len()
    );
    let mut fired: BTreeSet<&str> = BTreeSet::new();
    for schedule in &schedules {
        let real = replay(&model, schedule, &mut ev, &|_, _| false);
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &real),
                "{} after {schedule:?}",
                invariant.name
            );
        }
        fired.extend(schedule.iter().copied());
    }
    let committed: BTreeSet<&str> = model
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|name| !MUTANTS.contains(name))
        .collect();
    assert_eq!(fired, committed, "every committed action is driven");

    let audit = ev.audit();
    let actions: BTreeSet<&str> = audit.actions.iter().copied().collect();
    assert_eq!(actions, ANCHORED.into_iter().collect::<BTreeSet<_>>());
    assert!(audit.uninstrumented.is_empty(), "{audit:?}");
    assert!(audit.ambiguous.is_empty(), "{audit:?}");
    assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
    assert!(audit.undriven.is_empty(), "{audit:?}");
    assert!(audit.unwitnessed.is_empty(), "{audit:?}");
    assert!(audit.blind_steps.is_empty(), "{audit:?}");
    assert!(audit.stray.is_empty(), "{audit:?}");
}

fn mutant_stand_in(rig: &mut Rig, action: &str) -> bool {
    match action {
        // A begin that never asks the progress.
        "RerunDone" => {
            rig.ledger.running |= A.bit();
            rig.ledger.attempts[A as usize].began += 1;
            true
        }
        // A relaunch that reads the stage as never begun.
        "RelaunchForgetsDone" => {
            rig.read_back();
            rig.done &= !A.bit();
            rig.ledger.running &= !A.bit();
            rig.ledger.crashed &= !A.bit();
            rig.ledger.hung &= !A.bit();
            rig.ledger.quit &= !A.bit();
            rig.ledger.attempts[A as usize] = ledger::Attempt::default();
            rig.dead = false;
            true
        }
        "RelaunchNeverParks" => {
            rig.read_back();
            rig.ledger.running &= !A.bit();
            rig.dead = false;
            true
        }
        "ApplyKept" => {
            rig.wrote_kept = true;
            true
        }
        _ => false,
    }
}

#[test]
fn negative_control_each_mutant_breaks_its_invariant_on_the_real_ledger() {
    let buggy = interp::with_buggy(&setup_ledger_model(), 1);
    let committed = interp::with_buggy(&setup_ledger_model(), 0);
    let mut ev = StepEvidence::new("SetupLedger");
    let cases: [(&[&'static str], &str, &[&'static str]); 4] = [
        (
            &["BeginA", "EndA", "RerunDone"],
            "NotDoneAndRunning",
            &["BeginA", "EndA", "Crash", "Relaunch"],
        ),
        (
            &["BeginA", "EndA", "Crash", "RelaunchForgetsDone"],
            "CrashKeepsDone",
            &["BeginA", "EndA", "Crash", "Relaunch"],
        ),
        (
            &[
                "BeginA",
                "Crash",
                "Relaunch",
                "BeginA",
                "Crash",
                "RelaunchNeverParks",
            ],
            "TwoBeginsPark",
            &["BeginA", "Crash", "Relaunch", "BeginA", "Crash", "Relaunch"],
        ),
        (
            &["ApplyKey", "PersonChanges", "ApplyKept"],
            "KeptNeverWritten",
            &["ApplyKey", "PersonChanges", "UnattendedPass"],
        ),
    ];
    for (mutant, invariant, shipping) in cases {
        let real = replay(&buggy, mutant, &mut ev, &mutant_stand_in);
        assert!(
            !buggy.check_invariant(invariant, &real),
            "{mutant:?} breaks {invariant} on the real ledger: {real:?}"
        );
        let real = replay(&committed, shipping, &mut ev, &|_, _| false);
        assert!(
            committed.check_invariant(invariant, &real),
            "{shipping:?} keeps {invariant}: {real:?}"
        );
    }
}
