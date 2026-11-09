// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;

struct Driver<'a> {
    fixture: &'a Fixture,
    terminal: upgrade_drive::St,
    candidate: renewal::Candidate,
    scenario: i64,
    calls: RefCell<Vec<&'static str>>,
}

impl Driver<'_> {
    fn record(&self, call: &'static str) {
        aterm_uds::file_lock::with_lock_wait_observer(
            |_| panic!("native effect entered under the account writer"),
            || {
                renewal::with_receipt_writer(&self.fixture.env, |_| Ok(()))
                    .unwrap()
                    .unwrap()
            },
        );
        self.calls.borrow_mut().push(call);
    }

    fn complete(&self, noexec: bool) {
        let f = self.fixture;
        if !noexec {
            renewal::with_receipt_writer(&f.env, |_| {
                let mut root = renewal::read_receipt(&f.env, &f.root.id).unwrap();
                assert!(
                    root.launch_attempted,
                    "write-ahead marker precedes attach/drive"
                );
                root.candidate = Some(self.candidate.clone());
                renewal::write_receipt(&f.env, &root)
            })
            .unwrap()
            .unwrap();
        }
        let mut native = upgrade_drive::load_record(&f.opts, &f.key())
            .unwrap()
            .unwrap();
        native.native_prepare.clear();
        native.native = None;
        if noexec {
            native.native_noexec_prepare = Some(Box::new(f.prepare.clone()));
            native.native_completed_prepare = None;
            native.native_observation = None;
        } else {
            native.native_completed_prepare = self.terminal.native_completed_prepare.clone();
            native.native_observation = self.terminal.native_observation.clone();
            native.phase = self.terminal.phase.clone();
        }
        upgrade_drive::save_checked(&f.opts, &f.key(), &native).unwrap();
    }
}

impl RootDriver for Driver<'_> {
    fn now_us(&self) -> Option<u64> {
        Some(
            if matches!(self.scenario, 2 | 4)
                || (self.scenario == 13 && !self.calls.borrow().is_empty())
            {
                self.fixture.prepare.deadline_us
            } else {
                1
            },
        )
    }

    fn prepare(&self, _: &upgrade_drive::Opts, _: &str, _: &Prepare) -> Result<(), &'static str> {
        self.record("prepare");
        match self.scenario {
            6 => {
                let f = self.fixture;
                renewal::with_receipt_writer(&f.env, |_| {
                    let mut changed = renewal::read_receipt(&f.env, &f.root.id).unwrap();
                    changed.generation += 1;
                    renewal::write_receipt(&f.env, &changed)
                })
                .unwrap()
                .unwrap();
            }
            9 => {
                self.complete(true);
                return Err("prepared-noexec");
            }
            10 => self.complete(false),
            _ => {}
        }
        Ok(())
    }

    fn before_signal(&self) -> bool {
        self.record("before");
        self.scenario != 7
    }

    fn drive(&self, _: &upgrade_drive::Opts, _: &str, _: &Prepare) {
        self.record("drive");
        self.complete(false);
    }
}

fn run_case(scenario: i64) -> (bool, bool, bool) {
    let mut f = Fixture::new(false, true, scenario == 5);
    let terminal = f.state.clone();
    let candidate = f.root.candidate.clone().unwrap();
    if !matches!(scenario, 1 | 2 | 5) {
        f.state.native_completed_prepare = None;
        f.state.native_observation = None;
        f.state.native_prepare = Request::prepare(f.prepare.clone()).encode().unwrap();
        f.state.phase = crate::harness::upgrade::Phase::Exiting { at_s: 1 };
        if scenario == 3 {
            f.state.native_completed_prepare = Some(Box::new(f.prepare.clone()));
            f.state.native_prepare.clear();
        }
        f.save_native();
        if !matches!(scenario, 10 | 11) {
            f.root.launch_attempted = false;
        }
        if scenario != 11 {
            f.root.candidate = None;
        }
        f.write_root();
    }
    let mut expected = f.root.clone();
    if scenario == 8 {
        expected.generation += 1;
    }
    if scenario == 12 {
        renewal::with_receipt_writer(&f.env, |writer| {
            writer.cancel(&f.root, renewal::CancellationKind::OwnerStop, 8, 9)
        })
        .unwrap()
        .unwrap();
    }
    let driver = Driver {
        fixture: &f,
        terminal,
        candidate,
        scenario,
        calls: RefCell::new(Vec::new()),
    };
    let outcome = advance_root(&f.env, &expected, &f.prepare, &f.opts, &driver);
    if scenario == 13 {
        assert!(
            !renewal::read_receipt(&f.env, &f.root.id)
                .unwrap()
                .launch_attempted
        );
    }
    let calls = driver.calls.borrow();
    let prepared = calls.contains(&"prepare");
    let driven = calls.contains(&"drive");
    if driven {
        assert_eq!(*calls, ["prepare", "before", "drive"]);
    }
    if scenario == 7 {
        assert_eq!(*calls, ["prepare", "before"]);
    }
    (
        prepared,
        driven,
        matches!(outcome, Ok(CompletionProgress::Published(_))),
    )
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "NextCase" => {
            next.insert("scenario", state["scenario"] + 1);
        }
        "Step" => {
            let (prepared, driven, published) = run_case(state["scenario"]);
            next.insert("done", 1);
            next.insert("prepared", i64::from(prepared));
            next.insert("driven", i64::from(driven));
            next.insert("published", i64::from(published));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn root_recovery_refines_actual_driver_calls_and_durable_publication() {
    let model = aterm_models_harness::renewal_root_recovery_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} {next:?}"
                );
            }
        }
    }
    for (scenario, field, value, law) in [
        (3, "prepared", 1, "PrepareOnlyLive"),
        (7, "driven", 1, "DriveOnlyAuthorized"),
        (2, "published", 0, "RetainedCompletionProgress"),
        (3, "published", 1, "NoInventedCompletion"),
    ] {
        let mut state = model.init_state();
        state.insert("scenario", scenario);
        let mut wrong = transition(&state, "Step");
        wrong.insert(field, value);
        assert!(!model.check_invariant(law, &wrong));
        assert!(!model.successors("Step", &state).contains(&wrong));
    }
}

#[test]
fn root_launch_marker_repairs_uncertain_rename_without_changing_candidate() {
    let mut f = Fixture::new(false, false, false);
    let candidate = f.root.candidate.clone();
    f.root.launch_attempted = false;
    f.write_root();
    renewal::with_receipt_writer(&f.env, |writer| {
        assert_eq!(
            writer.record_launch_attempted(&f.root, &f.prepare),
            Err("launch-unmarked-candidate")
        );
        Ok(())
    })
    .unwrap()
    .unwrap();
    f.root.candidate = None;
    f.write_root();
    renewal::with_receipt_writer(&f.env, |writer| {
        let failure = aterm_durable::seam::FailingDirs::after(1, aterm_durable::seam::EIO);
        assert!(writer.record_launch_attempted(&f.root, &f.prepare).is_err());
        drop(failure);
        let mut marked = renewal::read_receipt(&f.env, &f.root.id).unwrap();
        assert!(marked.launch_attempted);
        marked.candidate = candidate.clone();
        renewal::write_receipt(&f.env, &marked)?;
        let before = f.receipt_bytes();
        assert_eq!(writer.record_launch_attempted(&marked, &f.prepare)?, marked);
        assert_eq!(f.receipt_bytes(), before);
        Ok(())
    })
    .unwrap()
    .unwrap();
}
