// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance of the setup run's ledger to `SetupLedger`
//! (`aterm_models_setup::setup_ledger_model`, design §6.9), and of the record of the keys that
//! changed after aterm set them to `SetupKeptKey` (`aterm_models_setup::setup_kept_key_model`).
//!
//! The ledger's walk replays every reachable step of its model on the real ledger (and the
//! login job's definition following the progress record, `resume::follow_with`), and at
//! every reachable state drives each begin the model refuses, which the ledger must refuse
//! too. The record's walk replays every reachable step of its model on the real record and
//! reads the record back after each, as a launch after a process end anywhere would. Each
//! negative control plants each defect in the code under test ([`ledger::Defect`]), never in
//! the rig, and shows the walk or an invariant catches it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_models_setup::{setup_kept_key_model, setup_ledger_model};
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::xref::StepEvidence;

use super::*;
use ledger::{Admit, Defect, Ledger, Software, SoftwareEpoch, Stamp};

const PROJECTED: [&str; 22] = [
    "alive",
    "software_known_a",
    "software_known_b",
    "done_a",
    "run_a",
    "att_a",
    "park_a",
    "due_a",
    "ever_a",
    "quit_a",
    "done_b",
    "run_b",
    "att_b",
    "park_b",
    "due_b",
    "ever_b",
    "quit_b",
    "noting",
    "rebooted",
    "trigger",
    "strike_a",
    "strike_b",
];

const ANCHORED: [&str; 21] = [
    "AdoptSoftwareA",
    "AdoptSoftwareB",
    "BeginA",
    "BeginB",
    "BeginDueA",
    "BeginDueB",
    "CheckAgain",
    "EndA",
    "EndB",
    "FailA",
    "FailB",
    "HangA",
    "HangB",
    "Login",
    "NoteQuit",
    "NewSoftwareA",
    "NewSoftwareB",
    "RefuseQuit",
    "Relaunch",
    "RetainSoftwareA",
    "RetainSoftwareB",
];

const MUTANTS: [&str; 8] = [
    "RerunDone",
    "RelaunchForgetsDone",
    "RelaunchNeverParks",
    "RelaunchParksForGood",
    "RelaunchCountsRestart",
    "RefuseKeepsNote",
    "BeginWhileQuitting",
    "DisarmWhileOwed",
];

/// The model's two stages: one of the Spotlight lane and one of the vault lane, which lanes run
/// at once.
const A: Stage = Stage::Spotlight;
const B: Stage = Stage::Repositories;

/// Every begin's stamp: one hold, one moment, which is also when every park the rig sets begins
/// its pause, so a begin at it finds every park still ahead, in the rig's first boot (a begin
/// after a restart is stamped with the boot it runs in, [`Rig::stamp`]).
const STAMP: Stamp = Stamp {
    boot: Some(7),
    at: 1_800_000_000,
    awake: Some(600),
    run: Some(9),
    software: None,
};

const SOFTWARE: SoftwareEpoch = SoftwareEpoch {
    build: 100,
    commit: [0x11; 20],
};

/// A begin two days on, past any park's longest pause ([`ledger::PARK_PAUSE`]): every park has
/// come due.
const LATER: Stamp = Stamp {
    at: STAMP.at + 2 * 24 * 60 * 60,
    ..STAMP
};

/// The real ledger, and what the model reads of the environment: whether the process lives
/// and what the run's progress has done. Whether a quit or a handoff has noted its stages and not yet gone or been refused is the
/// process's own latch for the rig's profile root ([`ledger::quitting`]), which the rig sets
/// and clears as a quit does and `begin` reads itself: the model's `noting` is bound to it.
struct Rig {
    ledger: Ledger,
    /// The profile root the latch is keyed by: the rig's own, so no other test's quit
    /// latches it.
    root: PathBuf,
    dead: bool,
    /// The Mac's boot now: a restart starts another ([`Rig::stamp`]).
    boot: u128,
    /// The Mac restarted since the process last launched: the model's `rebooted`.
    rebooted: bool,
    done: u16,
    ever: u16,
    /// How many times aterm stopped in each stage (a crash while it ran, its hang) since its
    /// last end, fail, press or fresh budget, a due begin counting as one, at most two: the
    /// model's `strike_a` and `strike_b`.
    strike: [i64; 2],
    /// The run's login job ([`resume::Trigger`]): its definition, as the progress record's saves
    /// leave it.
    trigger: Armed,
}

/// A login job's definition held in memory: armed or not.
#[derive(Default)]
struct Armed(std::cell::Cell<bool>);

impl resume::Trigger for Armed {
    fn arm(&self) -> Result<bool, String> {
        Ok(!self.0.replace(true))
    }
    fn disarm(&self) -> Result<bool, String> {
        Ok(self.0.replace(false))
    }
}

impl Rig {
    fn new() -> Self {
        static RIGS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let rig = RIGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let rig = Self {
            ledger: Ledger::of(1),
            root: PathBuf::from(format!("/nowhere/aterm-ledger-conformance/{rig}")),
            dead: false,
            boot: STAMP.boot.expect("the first boot"),
            rebooted: false,
            done: 0,
            ever: 0,
            strike: [0; 2],
            trigger: Armed::default(),
        };
        // The admission's own save: the run owes a pass from here.
        rig.save(Progress {
            admitted: true,
            ..Progress::default()
        });
        rig
    }

    /// Publish `progress` as `save_progress` does, with the login job following it: no reader
    /// ever finds a record that owes a pass without its job.
    fn save(&self, progress: Progress) {
        resume::follow_with(&self.trigger, &progress, || {
            assert!(
                !resume::owes(&progress) || self.trigger.0.get(),
                "a record that owes a pass is published only with its login job"
            );
            Ok(())
        })
        .expect("the record saves");
    }

    /// The progress the run's last save published: its done stages, completed once both are.
    fn progress(&self) -> Progress {
        Progress {
            admitted: true,
            done: self.done,
            completed: self.done & (A.bit() | B.bit()) == A.bit() | B.bit(),
            ..Progress::default()
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

    /// `stamp` as a begin in the Mac's boot now makes it.
    const fn stamp(&self, stamp: Stamp) -> Stamp {
        Stamp {
            boot: Some(self.boot),
            ..stamp
        }
    }

    /// The process launches again: whatever restart came before is behind it.
    fn relaunched(&mut self) {
        self.dead = false;
        self.rebooted = false;
    }

    /// What a launch reads back of the ledger a process ended with.
    fn read_back(&mut self) {
        self.ledger = Ledger::parse(&self.ledger.record()).expect("the ledger reads back");
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

fn bit(bits: u16, stage: Stage) -> i64 {
    i64::from(bits & stage.bit() != 0)
}

/// The model's stage `stage` is, as the rig's per-stage counts index it.
fn slot(stage: Stage) -> usize {
    usize::from(stage == B)
}

impl Rig {
    /// aterm stopped in `stage` once more.
    fn struck(&mut self, stage: Stage) {
        let strike = &mut self.strike[slot(stage)];
        *strike = (*strike + 1).min(2);
    }
}

fn project(rig: &Rig, model_state: &State) -> State {
    let mut state = model_state.clone();
    let ledger = &rig.ledger;
    state.insert("alive", i64::from(!rig.dead));
    for (stage, name) in [(A, "software_known_a"), (B, "software_known_b")] {
        state.insert(
            name,
            i64::from(matches!(
                ledger.software[stage as usize],
                Software::Known(_)
            )),
        );
    }
    for (stage, done, run, att, park, due, ever, quit) in [
        (
            A, "done_a", "run_a", "att_a", "park_a", "due_a", "ever_a", "quit_a",
        ),
        (
            B, "done_b", "run_b", "att_b", "park_b", "due_b", "ever_b", "quit_b",
        ),
    ] {
        state.insert(done, bit(rig.done, stage));
        state.insert(run, bit(ledger.running, stage));
        state.insert(att, i64::from(ledger.attempt(stage).began));
        state.insert(park, bit(ledger.crashed | ledger.hung, stage));
        state.insert(due, i64::from(ledger.park_due(stage, STAMP.at).is_some()));
        state.insert(ever, bit(rig.ever, stage));
        state.insert(quit, bit(ledger.quit, stage));
    }
    state.insert("noting", i64::from(rig.noting()));
    state.insert("rebooted", i64::from(rig.rebooted));
    state.insert("trigger", i64::from(rig.trigger.0.get()));
    state.insert("strike_a", rig.strike[slot(A)]);
    state.insert("strike_b", rig.strike[slot(B)]);
    state
}

fn drive(rig: &mut Rig, action: &'static str, ev: &mut StepEvidence) {
    let stage = |action: &str| if action.ends_with('A') { A } else { B };
    match action {
        "BeginA" | "BeginB" => {
            let stage = stage(action);
            let done = rig.done;
            let stamp = rig.stamp(STAMP);
            let admitted = ev.step(action, || rig.ledger.begin(stage, done, stamp, &rig.root));
            assert_eq!(admitted, Admit::Run, "{action}");
        }
        // The park's pause is over: the begin lifts it, with no press.
        "BeginDueA" | "BeginDueB" => {
            let stage = stage(action);
            let done = rig.done;
            let stamp = rig.stamp(LATER);
            let admitted = ev.step(action, || rig.ledger.begin(stage, done, stamp, &rig.root));
            assert_eq!(admitted, Admit::Run, "{action}");
            // The stage's last begin before another stop parks it: as one stop.
            rig.strike[slot(stage)] = 1;
        }
        "EndA" | "EndB" => {
            let stage = stage(action);
            rig.strike[slot(stage)] = 0;
            // As the pass saves its progress with the stage done (and, the run's last, its
            // completion), then ends it in the ledger.
            rig.done |= stage.bit();
            rig.ever |= stage.bit();
            rig.save(rig.progress());
            ev.step(action, || rig.ledger.end(stage));
        }
        "FailA" | "FailB" => {
            let stage = stage(action);
            rig.strike[slot(stage)] = 0;
            ev.step(action, || rig.ledger.end(stage));
        }
        "HangA" | "HangB" => {
            let stage = stage(action);
            let at = rig.ledger.attempt(stage).at;
            assert!(
                ev.step(action, || rig.ledger.mark_hung(stage, at, STAMP.at)),
                "{action}"
            );
            rig.struck(stage);
        }
        "CheckAgain" => {
            ev.step(action, || rig.ledger.retry());
            rig.strike = [0; 2];
        }
        // The process ends in the stages it runs, each a stop aterm counts, but one its hang
        // parked, whose park counted that stop.
        "Crash" => {
            for stage in [A, B] {
                if bit(rig.ledger.running & !rig.ledger.hung, stage) == 1 {
                    rig.struck(stage);
                }
            }
            rig.ends();
        }
        "AdoptSoftwareA" | "AdoptSoftwareB" => {
            let stage = stage(action);
            assert!(matches!(
                rig.ledger.software[stage as usize],
                Software::Unattributed
            ));
            assert!(ev.step(action, || rig.ledger.software_for(
                stage,
                rig.done,
                Some(SOFTWARE),
                &rig.root
            )));
            rig.strike[slot(stage)] = 0;
        }
        "NewSoftwareA" | "NewSoftwareB" => {
            let stage = stage(action);
            let Software::Known(old) = rig.ledger.software[stage as usize] else {
                panic!("{action} requires a recorded software identity");
            };
            let mut commit = old.commit;
            commit[0] ^= 1;
            let newer = SoftwareEpoch {
                build: old.build.checked_add(100).expect("finite replay"),
                commit,
            };
            assert!(ev.step(action, || rig.ledger.software_for(
                stage,
                rig.done,
                Some(newer),
                &rig.root
            )));
            rig.strike[slot(stage)] = 0;
        }
        "RetainSoftwareA" | "RetainSoftwareB" => {
            let stage = stage(action);
            let (inputs, count) = if let Software::Known(old) = rig.ledger.software[stage as usize]
            {
                let mut commit = old.commit;
                commit[0] ^= 1;
                (
                    [
                        None,
                        Some(old),
                        Some(SoftwareEpoch {
                            build: old.build - 1,
                            commit,
                        }),
                        Some(SoftwareEpoch { commit, ..old }),
                        Some(SoftwareEpoch {
                            build: old.build + 1,
                            ..old
                        }),
                    ],
                    5,
                )
            } else {
                ([None; 5], 1)
            };
            for current in inputs.into_iter().take(count) {
                let before = rig.ledger;
                assert!(!ev.step(action, || {
                    rig.ledger.software_for(stage, rig.done, current, &rig.root)
                }));
                assert_eq!(rig.ledger, before, "{action}: {current:?}");
            }
        }
        // The Mac restarts: the process, if it runs, ends with it, and the next launch is in
        // another boot.
        "Reboot" => {
            rig.ends();
            rig.boot += 1;
            rig.rebooted = true;
        }
        // As `ledger::interrupt_at`: the latch first, then the note.
        "NoteQuit" => {
            ledger::set_quitting(&rig.root, true);
            ev.step(action, || {
                rig.ledger
                    .interrupt(ledger::hold_process(STAMP.run.unwrap_or(0)))
            });
        }
        // The quit or the Commit goes: the process ends with its notes.
        "CommitQuit" => rig.ends(),
        // As `ledger::refused_at`: the notes taken back, then the latch released.
        "RefuseQuit" => {
            ev.step(action, || {
                rig.ledger
                    .refused(ledger::hold_process(STAMP.run.unwrap_or(0)))
            });
            ledger::set_quitting(&rig.root, false);
        }
        // launchd starts the login job, or a person opens aterm: the launch resumes the run.
        "Login" | "Relaunch" => {
            rig.read_back();
            let done = rig.done;
            let boot = Some(rig.boot);
            ev.step(action, || rig.ledger.recover(done, None, boot, STAMP.at));
            rig.relaunched();
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
        let admitted = rig
            .ledger
            .begin(stage, rig.done, rig.stamp(STAMP), &rig.root);
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
fn the_ledger_conforms_to_its_model() {
    the_walk_conforms();

    // These native refusal inputs share the model's RetainSoftware stutter. In particular,
    // unread persisted identity is not the legacy fact an AdoptSoftware action requires.
    let mut rig = Rig::new();
    let newer = SoftwareEpoch {
        build: 200,
        commit: [0x22; 20],
    };
    let foreign = SoftwareEpoch {
        build: SOFTWARE.build,
        commit: newer.commit,
    };
    rig.ledger.attempts[A as usize].began = 2;
    rig.ledger.crashed = A.bit();
    assert!(rig.ledger.software_for(A, 0, Some(SOFTWARE), &rig.root));
    assert_eq!(rig.ledger.software[A as usize], Software::Known(SOFTWARE));
    assert_eq!(rig.ledger.attempt(A).began, 0);
    assert_eq!(rig.ledger.parked(A), None);

    // Adoption was spent: exhausting this code's budget again cannot adopt it again.
    rig.ledger.attempts[A as usize].began = 2;
    rig.ledger.crashed = A.bit();
    for current in [
        None,
        Some(SOFTWARE),
        Some(SoftwareEpoch {
            build: 99,
            ..foreign
        }),
        Some(foreign),
        Some(SoftwareEpoch {
            build: 200,
            ..SOFTWARE
        }),
        Some(SoftwareEpoch { build: 0, ..newer }),
        Some(SoftwareEpoch {
            commit: [0; 20],
            ..newer
        }),
    ] {
        let before = rig.ledger;
        assert!(!rig.ledger.software_for(A, 0, current, &rig.root));
        assert_eq!(rig.ledger, before, "{current:?} leaves the spent budget");
    }

    rig.ledger.software[A as usize] = Software::Unread;
    rig.read_back();
    let unread = rig.ledger;
    assert!(!rig.ledger.software_for(A, 0, Some(newer), &rig.root));
    assert_eq!(rig.ledger, unread, "unread identity survives a round trip");
    rig.ledger.software[A as usize] = Software::Known(SOFTWARE);

    for (done, running, quit, noting) in [
        (A.bit(), 0, 0, false),
        (0, A.bit(), 0, false),
        (0, 0, A.bit(), false),
        (0, 0, 0, true),
    ] {
        rig.ledger.running = running;
        rig.ledger.quit = quit;
        ledger::set_quitting(&rig.root, noting);
        let before = rig.ledger;
        assert!(!rig.ledger.software_for(A, done, Some(newer), &rig.root));
        assert_eq!(
            rig.ledger, before,
            "software never clears an admission hold"
        );
    }
    ledger::set_quitting(&rig.root, false);
    rig.ledger.running = B.bit();
    rig.ledger.quit = 0;
    rig.ledger.software[B as usize] = Software::Known(SOFTWARE);
    rig.ledger.attempts[B as usize].began = 2;
    rig.ledger.hung = B.bit();
    let before_b = rig.ledger;
    assert!(rig.ledger.software_for(A, 0, Some(newer), &rig.root));
    assert_eq!(rig.ledger.software[A as usize], Software::Known(newer));
    assert_eq!(rig.ledger.attempt(A).began, 0);
    assert_eq!(rig.ledger.parked(A), None);
    assert_eq!(
        rig.ledger.software[B as usize],
        before_b.software[B as usize]
    );
    assert_eq!(rig.ledger.attempt(B), before_b.attempt(B));
    assert_eq!(rig.ledger.parked(B), before_b.parked(B));
    assert_eq!(rig.ledger.running, before_b.running);
    // Once B's own dead work has been recovered, A has not consumed its software change.
    rig.ledger.recover(0, None, Some(rig.boot), STAMP.at);
    assert!(rig.ledger.software_for(B, 0, Some(newer), &rig.root));
    assert_eq!(rig.ledger.software[B as usize], Software::Known(newer));
    assert_eq!(rig.ledger.attempt(B).began, 0);
    assert_eq!(rig.ledger.parked(B), None);
}

/// Each Buggy=1 mutant's step, played on the real code with its defect planted there: only the
/// progress record losing a done stage is the rig's, since the ledger holds no done stage.
fn mutant_step(rig: &mut Rig, action: &str) -> bool {
    match action {
        // A begin that never asks the progress.
        "RerunDone" => {
            let done = rig.done;
            let stamp = rig.stamp(STAMP);
            let admitted = planted(Defect::BeginIgnoresDone, || {
                rig.ledger.begin(A, done, stamp, &rig.root)
            });
            assert_eq!(admitted, Admit::Run);
            true
        }
        // A begin that never asks whether aterm quits.
        "BeginWhileQuitting" => {
            let done = rig.done;
            assert!(rig.noting(), "a quit decides");
            let stamp = rig.stamp(STAMP);
            let admitted = planted(Defect::BeginIgnoresQuit, || {
                rig.ledger.begin(A, done, stamp, &rig.root)
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
            rig.relaunched();
            true
        }
        "RelaunchNeverParks" => {
            rig.read_back();
            let done = rig.done;
            let boot = Some(rig.boot);
            planted(Defect::RecoverNeverParks, || {
                rig.ledger.recover(done, None, boot, STAMP.at)
            });
            rig.relaunched();
            true
        }
        "RelaunchParksForGood" => {
            rig.read_back();
            let done = rig.done;
            let boot = Some(rig.boot);
            planted(Defect::ParkWithoutDue, || {
                rig.ledger.recover(done, None, boot, STAMP.at)
            });
            rig.relaunched();
            true
        }
        "RelaunchCountsRestart" => {
            rig.read_back();
            let done = rig.done;
            let boot = Some(rig.boot);
            planted(Defect::RestartCountsAsCrash, || {
                rig.ledger.recover(done, None, boot, STAMP.at)
            });
            rig.relaunched();
            true
        }
        "RefuseKeepsNote" => {
            planted(Defect::RefusalKeepsNotes, || {
                rig.ledger
                    .refused(ledger::hold_process(STAMP.run.unwrap_or(0)))
            });
            ledger::set_quitting(&rig.root, false);
            true
        }
        // A save that reads an unfinished run as owing nothing: its login job goes with it.
        "DisarmWhileOwed" => {
            let progress = rig.progress();
            planted(Defect::NothingOwed, || {
                resume::follow_with(&rig.trigger, &progress, || Ok(())).expect("the record saves");
            });
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
    let cases: [(&[&'static str], &str, &[&'static str]); 9] = [
        (
            &["BeginA", "EndA", "RerunDone"],
            "NotDoneAndRunning",
            &["BeginA", "EndA", "Crash", "Login"],
        ),
        (
            &["BeginA", "EndA", "Crash", "RelaunchForgetsDone"],
            "CrashKeepsDone",
            &["BeginA", "EndA", "Crash", "Login"],
        ),
        (
            &[
                "BeginA",
                "Crash",
                "Login",
                "BeginA",
                "Crash",
                "RelaunchNeverParks",
            ],
            "TwoBeginsPark",
            &["BeginA", "Crash", "Login", "BeginA", "Crash", "Login"],
        ),
        (
            &[
                "BeginA",
                "Crash",
                "Login",
                "BeginA",
                "Crash",
                "RelaunchParksForGood",
            ],
            "EveryParkComesDue",
            &["BeginA", "Crash", "Login", "BeginA", "Crash", "Login"],
        ),
        (
            &[
                "BeginA",
                "Reboot",
                "RelaunchCountsRestart",
                "BeginA",
                "Reboot",
                "RelaunchCountsRestart",
            ],
            "ParksNeedTwoStops",
            &[
                "BeginA", "Reboot", "Relaunch", "BeginA", "Reboot", "Relaunch",
            ],
        ),
        // A crash, then a restart counted as a second: one stop behind the park, which a law
        // that held every park to one stop let pass.
        (
            &[
                "BeginA",
                "Crash",
                "Relaunch",
                "BeginA",
                "Reboot",
                "RelaunchCountsRestart",
            ],
            "ParksNeedTwoStops",
            &[
                "BeginA", "Crash", "Relaunch", "BeginA", "Reboot", "Relaunch",
            ],
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
        (
            &["BeginA", "EndA", "DisarmWhileOwed"],
            "UnfinishedRunArmed",
            &["BeginA", "EndA", "Crash", "Login"],
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

/// A restart counted as aterm stopping in the stage it cut off: with it planted in the code under
/// test, the walk fails at the login after a restart, whose begin the model gives back, as a
/// quit's. Before the ledger read the boot, two restarts during one long stage (the Command
/// Line Tools installing) parked it as crash-looping.
#[test]
fn the_walk_catches_a_restart_counted_as_a_crash() {
    let caught = planted(Defect::RestartCountsAsCrash, || {
        std::panic::catch_unwind(the_walk_conforms).expect_err("the walk catches it")
    });
    let said = caught
        .downcast_ref::<String>()
        .map_or("", String::as_str)
        .to_owned();
    assert!(
        said.contains("after Login") && said.contains("Reboot"),
        "a restart counted as a crash fails the walk at a relaunch after one: {said}"
    );
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

const KEY_PROJECTED: [&str; 5] = ["applied", "again", "differs", "kept", "wrote_kept"];

const KEY_ANCHORED: [&str; 6] = [
    "ApplyAgain",
    "ApplyKey",
    "GroupOn",
    "KeepChanged",
    "Restore",
    "UnattendedPass",
];

const KEY_MUTANTS: [&str; 2] = ["ApplyKept", "KeepAnyDrift"];

/// The one key `SetupKeptKey` follows: the Dock's.
const KEY: &str = "init.dock@conformance";

/// What the key reads as aterm sets it, and as something else left it.
const WANT: &str = "as aterm sets it";
const OTHER: &str = "as something else left it";

/// The real record of the keys that changed after aterm set them, what the key reads on the
/// Mac, and whether an unattended pass wrote a kept key.
struct KeyRig {
    kept: kept::Kept,
    wrote_kept: bool,
    /// What the key reads on the Mac: [`WANT`] or [`OTHER`].
    value: &'static str,
}

impl KeyRig {
    fn new() -> Self {
        Self {
            kept: kept::Kept::default(),
            wrote_kept: false,
            value: OTHER,
        }
    }

    /// What a launch reads back of the record: the walk reads it back after every step, as a
    /// process that ends anywhere leaves it to the next launch.
    fn read_back(&mut self) {
        self.kept = kept::Kept::parse(&self.kept.record()).expect("the record reads back");
    }

    /// An unattended pass over the key: it holds what the record says, sets the key where the
    /// hold lets it (one held as read back is set unless aterm set it again already, as
    /// `Workstation` resolves it), and notes what it found.
    fn unattended_pass(&mut self) {
        let mut holds = Vec::new();
        self.kept.hold(&mut holds, &|_| None);
        let left = match holds.iter().find(|(item, _)| item == KEY) {
            None => false,
            Some((_, Hold::ReadBack { again, .. })) => *again,
            Some((_, Hold::AsIs(_) | Hold::Later(_))) => true,
        };
        let key = [KEY.to_owned()];
        let (readings, drifted, changed) = if self.value == WANT {
            (
                vec![(KEY.to_owned(), WANT.to_owned())],
                Vec::new(),
                Vec::new(),
            )
        } else if left {
            (
                vec![(KEY.to_owned(), OTHER.to_owned())],
                key.to_vec(),
                Vec::new(),
            )
        } else {
            if self.kept.keeps(KEY) {
                self.wrote_kept = true;
            }
            self.value = WANT;
            (
                vec![(KEY.to_owned(), WANT.to_owned())],
                Vec::new(),
                vec![(KEY.to_owned(), OTHER.to_owned())],
            )
        };
        self.kept.note(
            &kept::Found {
                settled: &key,
                readings: &readings,
                drifted: &drifted,
                changed: &changed,
                new_targets: &[],
                owed: &|_| false,
            },
            &|_| Some(Setting::Dock),
        );
    }
}

fn dock() -> Selection {
    let mut dock = Selection::none();
    dock.set(Setting::Dock, true);
    dock
}

fn project_key(rig: &KeyRig, model_state: &State) -> State {
    let mut state = model_state.clone();
    let tracked = rig.kept.applied(KEY) || rig.kept.keeps(KEY);
    state.insert("applied", i64::from(rig.kept.applied(KEY)));
    state.insert("again", i64::from(rig.kept.set_again(KEY)));
    state.insert("differs", i64::from(tracked && rig.value != WANT));
    state.insert("kept", i64::from(rig.kept.keeps(KEY)));
    state.insert("wrote_kept", i64::from(rig.wrote_kept));
    state
}

/// Each step of `SetupKeptKey` on the real record, each `Buggy=1` mutant with its defect
/// planted in the code under test.
fn drive_key(rig: &mut KeyRig, action: &'static str, ev: &mut StepEvidence) {
    match action {
        // The first pass that reaches the key sets it and reads it back.
        "ApplyKey" => {
            let before = rig.value;
            rig.value = WANT;
            ev.step(action, || {
                rig.kept.note(
                    &kept::Found {
                        settled: &[KEY.to_owned()],
                        readings: &[(KEY.to_owned(), WANT.to_owned())],
                        drifted: &[],
                        changed: &[(KEY.to_owned(), before.to_owned())],
                        new_targets: &[],
                        owed: &|_| false,
                    },
                    &|_| Some(Setting::Dock),
                )
            });
        }
        // Something other than aterm changes the key: no code step runs.
        "OtherWriterChanges" => rig.value = OTHER,
        // An unattended pass: what the model tells apart by the record and the key is what the
        // pass does with them. A write of a kept key is the defect.
        "ApplyAgain" | "KeepChanged" | "UnattendedPass" => {
            ev.step(action, || rig.unattended_pass());
        }
        "Restore" => {
            ev.step(action, || rig.kept.forget_all());
        }
        "GroupOn" => {
            ev.step(action, || rig.kept.forget(dock()));
        }
        "ApplyKept" => planted(Defect::HoldForgetsKept, || rig.unattended_pass()),
        "KeepAnyDrift" => planted(Defect::KeepsFirstChange, || rig.unattended_pass()),
        other => panic!("no real step for `{other}`"),
    }
}

/// `schedule` replayed on a fresh record, read back after each step, with every projected
/// variable checked against the model's: the real state it ends in.
fn replay_key(model: &Model, schedule: &[&'static str], ev: &mut StepEvidence) -> State {
    let mut rig = KeyRig::new();
    let mut state = model.init_state();
    assert_eq!(
        project_key(&rig, &state),
        state,
        "an empty record is the model's start"
    );
    for &action in schedule {
        assert!(
            model.fire(action, &mut state),
            "{action} is enabled after {schedule:?}: {state:?}"
        );
        drive_key(&mut rig, action, ev);
        rig.read_back();
        let real = project_key(&rig, &state);
        for var in KEY_PROJECTED {
            assert_eq!(
                real[var], state[var],
                "`{var}` after {action} in {schedule:?}, read back: real {real:?}, model {state:?}"
            );
        }
    }
    project_key(&rig, &state)
}

#[test]
fn the_kept_record_conforms_to_its_model() {
    ledger::setup_ledger_environment();
    let model = interp::with_buggy(&setup_kept_key_model(), 0);
    let mut ev = StepEvidence::new("SetupKeptKey");
    let (_, transitions) = walk(&model);
    let mut fired: BTreeSet<&str> = BTreeSet::new();
    for schedule in &transitions {
        let real = replay_key(&model, schedule, &mut ev);
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
        .filter(|name| !KEY_MUTANTS.contains(name))
        .collect();
    assert_eq!(fired, committed, "every committed action is driven");

    let audit = ev.audit();
    let actions: BTreeSet<&str> = audit.actions.iter().copied().collect();
    assert_eq!(actions, KEY_ANCHORED.into_iter().collect::<BTreeSet<_>>());
    assert!(audit.uninstrumented.is_empty(), "{audit:?}");
    assert!(audit.ambiguous.is_empty(), "{audit:?}");
    assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
    assert!(audit.undriven.is_empty(), "{audit:?}");
    assert!(audit.unwitnessed.is_empty(), "{audit:?}");
    assert!(audit.blind_steps.is_empty(), "{audit:?}");
    assert!(audit.stray.is_empty(), "{audit:?}");
}

#[test]
fn negative_control_each_key_mutant_breaks_its_invariant_on_the_real_record() {
    let buggy = interp::with_buggy(&setup_kept_key_model(), 1);
    let committed = interp::with_buggy(&setup_kept_key_model(), 0);
    let mut ev = StepEvidence::new("SetupKeptKey");
    let cases: [(&[&'static str], &str, &[&'static str]); 2] = [
        (
            &[
                "ApplyKey",
                "OtherWriterChanges",
                "ApplyAgain",
                "OtherWriterChanges",
                "KeepChanged",
                "ApplyKept",
            ],
            "KeptNeverWritten",
            &[
                "ApplyKey",
                "OtherWriterChanges",
                "ApplyAgain",
                "OtherWriterChanges",
                "KeepChanged",
                "UnattendedPass",
            ],
        ),
        (
            &["ApplyKey", "OtherWriterChanges", "KeepAnyDrift"],
            "KeptOnlyAfterSetAgain",
            &["ApplyKey", "OtherWriterChanges", "ApplyAgain"],
        ),
    ];
    for (mutant, invariant, shipping) in cases {
        let real = replay_key(&buggy, mutant, &mut ev);
        assert!(
            !buggy.check_invariant(invariant, &real),
            "{mutant:?} breaks {invariant} on the real record: {real:?}"
        );
        let real = replay_key(&committed, shipping, &mut ev);
        assert!(
            committed.check_invariant(invariant, &real),
            "{shipping:?} keeps {invariant}: {real:?}"
        );
    }
}
