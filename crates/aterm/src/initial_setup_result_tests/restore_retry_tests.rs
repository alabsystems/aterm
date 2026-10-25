// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_cli::macos_doctor::{InitialSetupAdmissionFault, InitialSetupAdmissionStopTest};
use std::sync::atomic::Ordering;

#[derive(Clone, Copy, PartialEq, Eq)]
enum StopAt {
    Never,
    Error,
    Backoff,
    Success,
}

struct Startup<'a> {
    root: &'a Path,
    setup: InitialSetupAdmissionStopTest<'a>,
    sent: StartupSent,
    stop: StopAt,
    brought: usize,
    backoffs: usize,
    binds: usize,
    offers: usize,
    saves: usize,
    failures: Vec<String>,
}

impl SetupStartup for Startup<'_> {
    type Claim = StartupClaim;

    fn send(&mut self) {
        self.sent.send();
    }

    fn answered(&mut self) {
        self.sent.answered();
    }

    fn stopped(&self) -> bool {
        stop_covers(self.sent.sent)
    }

    fn bring_forward(&mut self) -> Result<bool, String> {
        self.brought += 1;
        assert!(
            self.brought <= 2,
            "one retry completes the existing checkpoint"
        );
        let result = self.sent.observe_repair_with(|published| {
            self.setup
                .bring_forward_progress_error(self.brought == 1, published)
        });
        if self.brought == 1 {
            assert!(
                result.is_err(),
                "the actual Progress namespace flush failed"
            );
            assert!(
                self.setup.snapshot().unwrap().finished,
                "Pending has not landed"
            );
            if self.stop == StopAt::Error {
                stop_initial_setup();
            }
        } else {
            assert!(
                result.is_ok(),
                "the existing checkpoint needs no second Progress write"
            );
            if self.stop == StopAt::Success {
                stop_initial_setup();
            }
        }
        result.map(|outcome| !matches!(outcome, aterm_cli::macos_doctor::BroughtForward::Busy))
    }

    fn backoff(&mut self) {
        self.backoffs += 1;
        assert_eq!(self.backoffs, 1);
        assert_eq!((self.binds, self.offers, self.saves), (0, 0, 0));
        if self.stop == StopAt::Backoff {
            stop_initial_setup();
        }
    }

    fn bind_authorization(&mut self) -> Result<u64, String> {
        self.binds += 1;
        assert_eq!(self.binds, 1);
        assert!(!self.stopped(), "Stop retires this pre-offer coordinator");
        assert!(
            !self.setup.snapshot().unwrap().finished,
            "the launch must finish its failed bring-forward before binding Pending"
        );
        assert_eq!((self.brought, self.backoffs), (2, 1));
        self.setup = InitialSetupAdmissionStopTest::startup(self.root)?;
        self.setup.revision()
    }

    fn revision(&mut self) -> Result<u64, String> {
        self.setup.revision()
    }

    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        StartupClaim::claim()
    }

    fn finished(&mut self) -> Result<bool, String> {
        self.setup
            .snapshot()
            .map(|snapshot| snapshot.finished)
            .map_err(|why| why.to_string())
    }

    fn admission(&mut self) -> Result<Option<SetupAdmission>, String> {
        Ok(self.setup.admission()?.map(|admitted| SetupAdmission {
            revision: admitted.revision,
            completed: admitted.completed,
        }))
    }

    fn initial(&mut self) -> Result<Plan, String> {
        self.setup
            .snapshot()
            .map(|snapshot| snapshot.plan)
            .map_err(|why| why.to_string())
    }

    fn offer(&mut self, plan: Plan) -> aterm_gui::WorkstationSetupDecision {
        self.offers += 1;
        assert_eq!(self.offers, 1);
        assert_eq!(plan, self.setup.plan());
        aterm_gui::WorkstationSetupDecision::Begin(plan)
    }

    fn authorize(&mut self, plan: &Plan) -> Result<Option<u64>, String> {
        self.saves += 1;
        assert_eq!(self.saves, 1);
        assert_eq!(plan, &self.setup.plan());
        let result = self.setup.persist(InitialSetupAdmissionFault::None);
        self.sent.record_publication(self.setup.stop_target());
        if result.as_ref().is_ok_and(Option::is_some) {
            self.sent.admitted();
        }
        result
    }

    fn authorization_pending(&self) -> bool {
        self.setup.pending()
    }

    fn wait_holding_claim(&mut self) -> bool {
        panic!("the admission itself succeeds on its first save")
    }

    fn report_failure(&mut self, why: &str) {
        self.failures.push(why.to_owned());
    }
}

fn startup<'a>(root: &'a Path, stop: StopAt) -> Startup<'a> {
    Startup {
        root,
        setup: InitialSetupAdmissionStopTest::restored_before_run(root).unwrap(),
        sent: StartupSent::default(),
        stop,
        brought: 0,
        backoffs: 0,
        binds: 0,
        offers: 0,
        saves: 0,
        failures: Vec::new(),
    }
}

#[test]
fn startup_restore_retry_finishes_pending_before_binding_and_dispatches_once() {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    *CLAIM_ROOT.lock().unwrap() = Some(root.path().to_owned());
    let mut host = startup(root.path(), StopAt::Never);
    let prepared = prepare_setup(&mut host).unwrap();
    let PreparedSetup::Authorized { revision, .. } = &prepared else {
        panic!("a repaired bring-forward must dispatch its admitted permissions");
    };
    let revision = *revision;
    assert_eq!(
        (
            host.brought,
            host.backoffs,
            host.binds,
            host.offers,
            host.saves
        ),
        (2, 1, 1, 1, 1)
    );
    assert_eq!(host.failures.len(), 1);
    let admitted = host.setup.admission().unwrap().unwrap();
    assert_eq!(admitted.revision, revision);
    assert!(!admitted.completed);
    assert_eq!(admitted.plan, host.setup.plan());
    assert!(host.setup.snapshot().unwrap().permissions_allowed);
    run_prepared_setup(prepared, None, std::mem::take(&mut host.sent));
    assert_eq!(script().runs, [revision]);
    assert_eq!(STARTUP_CLAIMS.load(Ordering::Acquire), 0);
    assert!(!INITIAL_SETUP_BUSY.load(Ordering::Acquire));
    clear_setup_run(revision);
}

#[test]
fn startup_restore_retry_stop_retires_the_retry_and_leaves_a_later_manual_choice_independent() {
    let _globals = globals();
    for stop in [StopAt::Error, StopAt::Backoff, StopAt::Success] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        *CLAIM_ROOT.lock().unwrap() = Some(root.path().to_owned());
        change(|script| script.stop_root = Some(root.path().to_owned()));
        let mut host = startup(root.path(), stop);
        let prepared = prepare_setup(&mut host).unwrap();
        assert!(matches!(prepared, PreparedSetup::Unavailable));
        assert_eq!(
            (host.brought, host.binds, host.offers, host.saves),
            (1 + usize::from(stop == StopAt::Success), 0, 0, 0)
        );
        assert_eq!(host.backoffs, usize::from(stop != StopAt::Error));
        assert_eq!(host.failures.len(), usize::from(stop != StopAt::Error));
        let held = host.setup.snapshot().unwrap();
        run_prepared_setup(prepared, None, std::mem::take(&mut host.sent));
        wait_until("the Stop saver finishes its queued IO", || {
            let saves = stop_saves();
            saves.wanted.is_none() && saves.saving.is_none()
        });
        assert!(settle_setup_stop(Duration::ZERO));
        assert_eq!(script().stops, [held.revision]);
        assert_eq!(STARTUP_CLAIMS.load(Ordering::Acquire), 0);
        assert!(script().runs.is_empty());
        assert!(host.setup.recover().is_err());

        let chosen = InitialSetupAdmissionStopTest::restart(root.path()).unwrap();
        let revision = chosen.choose().unwrap();
        register_admitted_run(revision, send_admission(), false, None);
        assert!(revision > held.revision);
        assert!(!setup_stop_requested(revision));
        let resumed = chosen.snapshot().unwrap();
        assert_eq!(resumed.plan, held.plan);
        assert!(!resumed.finished && resumed.permissions_allowed);
        clear_setup_run(revision);
    }
}
