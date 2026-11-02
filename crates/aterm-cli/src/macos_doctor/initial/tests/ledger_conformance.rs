// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance of the setup run's ledger and the kept-by-person record to `SetupLedger`
//! (`aterm_models_setup::setup_ledger_model`, design §6.9).
//!
//! The walk replays every reachable step of the model on the real ledger and record, and at
//! every reachable state drives each begin the model refuses, which the ledger must refuse
//! too. The negative control plants each defect in the code under test
//! ([`ledger::Defect`]), never in the rig, and shows the walk or an invariant catches it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_models_setup::setup_ledger_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::xref::StepEvidence;

use super::*;
use ledger::{Admit, Defect, Ledger, Stamp};

const PROJECTED: [&str; 17] = [
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
    "noting",
    "applied",
    "kept",
    "wrote_kept",
];

const ANCHORED: [&str; 17] = [
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
    "NoteQuit",
    "PersonChanges",
    "RefuseQuit",
    "Relaunch",
    "Restore",
    "UnattendedPass",
];

const MUTANTS: [&str; 6] = [
    "RerunDone",
    "RelaunchForgetsDone",
    "RelaunchNeverParks",
    "ApplyKept",
    "RefuseKeepsNote",
    "BeginWhileQuitting",
];

/// The model's two stages: one of the Spotlight lane and one of the vault lane, which lanes run
/// at once.
const A: Stage = Stage::Spotlight;
const B: Stage = Stage::Repositories;

/// The one key the model follows: the Dock's.
const KEY: &str = "init.dock@conformance";

/// Every begin's stamp: one boot, one hold, one moment.
const STAMP: Stamp = Stamp {
    boot: Some(7),
    at: 1_800_000_000,
    awake: Some(600),
    run: Some(9),
};

/// The real ledger and record, and what the model reads of the environment: whether the
/// process lives, what the run's progress has done, and whether a pass wrote the kept key.
/// Whether a quit or a handoff has noted its stages and not yet gone or been refused is the
/// process's own latch for the rig's profile root ([`ledger::quitting`]), which the rig sets
/// and clears as a quit does and `begin` reads itself: the model's `noting` is bound to it.
struct Rig {
    ledger: Ledger,
    kept: kept::Kept,
    /// The profile root the latch is keyed by: the rig's own, so no other test's quit
    /// latches it.
    root: PathBuf,
    dead: bool,
    done: u16,
    ever: u16,
    wrote_kept: bool,
}

impl Rig {
    fn new() -> Self {
        static RIGS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let rig = RIGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            ledger: Ledger::of(1),
            kept: kept::Kept::default(),
            root: PathBuf::from(format!("/nowhere/aterm-ledger-conformance/{rig}")),
            dead: false,
            done: 0,
            ever: 0,
            wrote_kept: false,
        }
    }

    /// Whether a quit decides now: the process's latch for the rig's root.
    fn noting(&self) -> bool {
        ledger::quitting(&self.root)
    }

    /// The process ends (a crash, or the quit or Commit that went): its latch goes with it.
    fn ends(&mut self) {
        self.dead = true;
        ledger::set_quitting(&self.root, false);
    }

    /// What a launch reads back of the ledger and the record a process ended with.
    fn read_back(&mut self) {
        self.ledger = Ledger::parse(&self.ledger.record()).expect("the ledger reads back");
        self.kept = kept::Kept::parse(&self.kept.record()).expect("the record reads back");
    }

    /// An unattended pass over the kept key: it writes the key unless the record holds it.
    fn unattended_pass(&mut self) {
        let mut holds = Vec::new();
        self.kept.hold(&mut holds, &|_| SETTLED);
        let held = holds
            .iter()
            .any(|(item, hold)| item == KEY && matches!(hold, Hold::AsIs(_)));
        if !held && self.kept.keeps(KEY) {
            self.wrote_kept = true;
        }
    }
}

impl Drop for Rig {
    /// The rig's process is gone: no latch of its outlives it.
    fn drop(&mut self) {
        ledger::set_quitting(&self.root, false);
    }
}

/// Run `step` with `defect` planted in the code under test on this thread.
fn planted<T>(defect: Defect, step: impl FnOnce() -> T) -> T {
    ledger::PLANTED.with(|planted| planted.set(Some(defect)));
    let out = step();
    ledger::PLANTED.with(|planted| planted.set(None));
    out
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
    state.insert("noting", i64::from(rig.noting()));
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
            let admitted = ev.step(action, || rig.ledger.begin(stage, done, STAMP, &rig.root));
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
        "Crash" => rig.ends(),
        // As `ledger::interrupt_at`: the latch first, then the note.
        "NoteQuit" => {
            ledger::set_quitting(&rig.root, true);
            ev.step(action, || rig.ledger.interrupt());
        }
        // The quit or the Commit goes: the process ends with its notes.
        "CommitQuit" => rig.ends(),
        // As `ledger::refused_at`: the notes taken back, then the latch released.
        "RefuseQuit" => {
            ev.step(action, || rig.ledger.refused());
            ledger::set_quitting(&rig.root, false);
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
    mutant: &dyn Fn(&mut Rig, &str) -> bool,
) -> (Rig, State) {
    let mut rig = Rig::new();
    let mut state = model.init_state();
    let mut real = project(&rig, &state);
    assert_eq!(real, state, "a fresh run is the model's start");
    for &action in schedule {
        assert!(
            model.fire(action, &mut state),
            "{action} is enabled after {schedule:?}: {state:?}"
        );
        if !mutant(&mut rig, action) {
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
    (rig, real)
}

/// Every reachable state's shortest path, and every transition's: the schedules the walk
/// replays.
fn walk(model: &Model) -> (Vec<Vec<&'static str>>, Vec<Vec<&'static str>>) {
    let mut path: BTreeMap<State, Vec<&'static str>> = BTreeMap::new();
    let mut queue = VecDeque::new();
    path.insert(model.init_state(), Vec::new());
    queue.push_back(model.init_state());
    let mut transitions = Vec::new();
    while let Some(state) = queue.pop_front() {
        for action in model.actions.iter().map(|a| a.name) {
            for next in model.successors(action, &state) {
                let mut schedule = path[&state].clone();
                schedule.push(action);
                transitions.push(schedule.clone());
                if !path.contains_key(&next) {
                    path.insert(next.clone(), schedule);
                    queue.push_back(next);
                }
            }
        }
    }
    (path.into_values().collect(), transitions)
}

/// At the state `schedule` reaches, each begin the model refuses while the process lives and
/// the stage is not running (it is done or parked, or a quit decides) is refused by the ledger,
/// which it leaves as it was.
fn refusals_conform(model: &Model, schedule: &[&'static str], ev: &mut StepEvidence) {
    let (mut rig, state) = replay(model, schedule, ev, &|_, _| false);
    for (action, stage, run) in [("BeginA", A, "run_a"), ("BeginB", B, "run_b")] {
        if state["alive"] == 0 || state[run] == 1 || !model.successors(action, &state).is_empty() {
            continue;
        }
        let before = rig.ledger;
        let admitted = rig.ledger.begin(stage, rig.done, STAMP, &rig.root);
        assert_ne!(
            admitted,
            Admit::Run,
            "{action} after {schedule:?}: the model refuses it at {state:?}"
        );
        assert_eq!(rig.ledger, before, "a refused {action} after {schedule:?}");
    }
}

/// The walk: every transition replayed with the invariants checked on the real state, every
/// refused begin driven, and the anchors' evidence audited.
fn the_walk_conforms() {
    ledger::setup_ledger_environment();
    let model = interp::with_buggy(&setup_ledger_model(), 0);
    let mut ev = StepEvidence::new("SetupLedger");
    let (states, transitions) = walk(&model);
    assert!(
        transitions.len() > 100,
        "the walk covers the machine: {} transitions",
        transitions.len()
    );
    let mut fired: BTreeSet<&str> = BTreeSet::new();
    for schedule in &transitions {
        let (_, real) = replay(&model, schedule, &mut ev, &|_, _| false);
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &real),
                "{} after {schedule:?}",
                invariant.name
            );
        }
        fired.extend(schedule.iter().copied());
    }
    let mut refused = 0;
    for schedule in &states {
        refusals_conform(&model, schedule, &mut ev);
        refused += 1;
    }
    assert_eq!(refused, states.len());
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

#[test]
fn the_ledger_and_the_kept_record_conform_to_the_model() {
    the_walk_conforms();
}

/// Each Buggy=1 mutant's step, played on the real code with its defect planted there: only the
/// progress record losing a done stage is the rig's, since the ledger holds no done stage.
fn mutant_step(rig: &mut Rig, action: &str) -> bool {
    match action {
        // A begin that never asks the progress.
        "RerunDone" => {
            let done = rig.done;
            let admitted = planted(Defect::BeginIgnoresDone, || {
                rig.ledger.begin(A, done, STAMP, &rig.root)
            });
            assert_eq!(admitted, Admit::Run);
            true
        }
        // A begin that never asks whether aterm quits.
        "BeginWhileQuitting" => {
            let done = rig.done;
            assert!(rig.noting(), "a quit decides");
            let admitted = planted(Defect::BeginIgnoresQuit, || {
                rig.ledger.begin(A, done, STAMP, &rig.root)
            });
            assert_eq!(admitted, Admit::Run);
            true
        }
        // A relaunch that reads the stage as never begun: the progress record lost it.
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
            let done = rig.done;
            planted(Defect::RecoverNeverParks, || rig.ledger.recover(done, None));
            rig.dead = false;
            true
        }
        "ApplyKept" => {
            planted(Defect::HoldForgetsKept, || rig.unattended_pass());
            true
        }
        "RefuseKeepsNote" => {
            planted(Defect::RefusalKeepsNotes, || rig.ledger.refused());
            ledger::set_quitting(&rig.root, false);
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
    let cases: [(&[&'static str], &str, &[&'static str]); 6] = [
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
        (
            &["BeginA", "NoteQuit", "RefuseKeepsNote"],
            "QuitMarksOnlyInAQuit",
            &["BeginA", "NoteQuit", "RefuseQuit"],
        ),
        (
            &["NoteQuit", "BeginWhileQuitting"],
            "QuitNotesEveryRunningStage",
            &["BeginA", "NoteQuit"],
        ),
    ];
    for (mutant, invariant, shipping) in cases {
        let (_, real) = replay(&buggy, mutant, &mut ev, &mutant_step);
        assert!(
            !buggy.check_invariant(invariant, &real),
            "{mutant:?} breaks {invariant} on the real ledger: {real:?}"
        );
        let (_, real) = replay(&committed, shipping, &mut ev, &|_, _| false);
        assert!(
            committed.check_invariant(invariant, &real),
            "{shipping:?} keeps {invariant}: {real:?}"
        );
    }
}

/// The begin defects rounds 21 and 22's reviews found, each a guard `begin` would lose: with
/// any one planted in the code under test, the walk fails, at the first begin the model
/// refuses. Before the walk drove refused begins, the first two passed it; before `begin` asked
/// whether aterm quits, the model let a stage begin while a quit decided.
#[test]
fn the_walk_catches_a_begin_that_ignores_done_a_park_or_a_quit() {
    for defect in [
        Defect::BeginIgnoresDone,
        Defect::BeginIgnoresPark,
        Defect::BeginIgnoresQuit,
    ] {
        let caught = planted(defect, || {
            std::panic::catch_unwind(the_walk_conforms).expect_err("the walk catches it")
        });
        let said = caught
            .downcast_ref::<String>()
            .map_or("", String::as_str)
            .to_owned();
        assert!(
            said.contains("the model refuses it"),
            "{defect:?} fails the walk at a refused begin: {said}"
        );
    }
}
