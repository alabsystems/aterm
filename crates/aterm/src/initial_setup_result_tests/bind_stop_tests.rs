// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_cli::macos_doctor::{InitialSetupAdmissionFault, InitialSetupAdmissionStopTest};
use std::sync::atomic::Ordering;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ManualAt {
    DuringBind,
    DuringBindAfterStop,
    AfterCapture,
    ExistingAfterStop,
}

struct Startup<'a> {
    root: &'a Path,
    setup: InitialSetupAdmissionStopTest<'a>,
    sent: StartupSent,
    manual_at: ManualAt,
    current_revision: u64,
    manual_sent: Option<u64>,
    bound_revision: u64,
    binds: usize,
    claims: usize,
}

impl Startup<'_> {
    fn stop_current(&self) {
        assert!(stop_initial_setup());
        wait_until("the predecessor's Stop is durable", stop_worker_idle);
        assert!(settle_setup_stop(Duration::ZERO));
        assert!(stop_covers(self.sent.sent));
    }

    fn choose_later(&mut self) {
        assert_eq!(STARTUP_CLAIMS.load(Ordering::Acquire), 0);
        if self.manual_at == ManualAt::DuringBindAfterStop {
            self.stop_current();
        }
        let busy = take_initial_setup_slot().unwrap();
        let claim = (setup_io().claim_rerun)().unwrap().unwrap();
        let generation = SETUP_APPLY_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
        let choice = begin_setup_choice(&self.setup.plan(), true, generation);
        let under_way = UnderWay::new(choice.sent);
        let manual = InitialSetupAdmissionStopTest::restart(self.root).unwrap();
        let revision = manual.choose().unwrap();
        assert!(!register_admitted_run(revision, choice.sent, false, None));
        self.current_revision = revision;
        self.manual_sent = Some(choice.sent);
        assert!(choice.sent > self.sent.sent);
        assert!(!setup_stop_requested(revision));
        // The new run is admitted and parked: its real locks are released,
        // while the inert fixture never executes an operating-system stage.
        drop(under_way);
        drop(claim);
        drop(busy);
    }
}

impl SetupStartup for Startup<'_> {
    type Claim = StartupClaim;

    fn send(&mut self) {
        self.sent.send();
    }

    fn stopped(&self) -> bool {
        stop_covers(self.sent.sent)
    }

    fn bind_authorization(&mut self) -> Result<u64, String> {
        self.binds += 1;
        assert_eq!(self.binds, 1);
        if self.manual_at == ManualAt::ExistingAfterStop {
            self.stop_current();
        } else if self.manual_at != ManualAt::AfterCapture {
            // Actual Stop and manual admission happen after prepare_setup's
            // final pre-bind check, before the fresh ticket captures records.
            self.choose_later();
        }
        self.setup = InitialSetupAdmissionStopTest::startup(self.root)?;
        let revision = self.setup.revision()?;
        self.bound_revision = revision;
        if self.manual_at == ManualAt::ExistingAfterStop {
            self.current_revision = revision;
        }
        if self.manual_at == ManualAt::AfterCapture {
            self.choose_later();
        }
        Ok(revision)
    }

    fn revision(&mut self) -> Result<u64, String> {
        self.setup.revision()
    }

    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        self.claims += 1;
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
            plan: admitted.plan,
            revision: admitted.revision,
            completed: admitted.completed,
        }))
    }

    fn initial(&mut self) -> Result<Plan, String> {
        panic!("the current run is already admitted")
    }

    fn offer(&mut self, _: Plan) -> aterm_gui::WorkstationSetupDecision {
        panic!("the newer manual admission needs no automatic offer")
    }

    fn authorize(&mut self, _: &Plan) -> Result<Option<u64>, String> {
        panic!("no automatic Begin was delivered")
    }

    fn authorization_pending(&self) -> bool {
        self.setup.pending()
    }

    fn wait_holding_claim(&mut self) -> bool {
        panic!("the coordinator must not start an admission")
    }

    fn backoff(&mut self) {
        panic!("all scratch locks and records are readable")
    }

    fn report_failure(&mut self, why: &str) {
        panic!("unexpected startup failure: {why}")
    }
}

fn exercise(manual_at: ManualAt) {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    *CLAIM_ROOT.lock().unwrap() = Some(root.path().to_owned());
    change(|script| script.stop_root = Some(root.path().to_owned()));
    let mut setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
    let predecessor = setup
        .persist(InitialSetupAdmissionFault::None)
        .unwrap()
        .unwrap();
    register_admitted_run(predecessor, send_admission(), false, None);
    let mut host = Startup {
        root: root.path(),
        setup,
        sent: StartupSent::default(),
        manual_at,
        current_revision: 0,
        manual_sent: None,
        bound_revision: 0,
        binds: 0,
        claims: 0,
    };
    let prepared = prepare_setup(&mut host).unwrap();
    let resumed = match &prepared {
        PreparedSetup::Authorized { revision, .. } => Some(*revision),
        _ => None,
    };
    let unavailable = matches!(&prepared, PreparedSetup::Unavailable);
    let revision = host.current_revision;
    if let Some(sent) = host.manual_sent {
        assert!(revision > predecessor);
        assert!(!stop_covers(sent));
    } else {
        assert_eq!(revision, predecessor);
    }
    run_prepared_setup(prepared, None, std::mem::take(&mut host.sent));
    wait_until("the Stop saver finishes its queued IO", stop_worker_idle);
    assert!(settle_setup_stop(Duration::ZERO));
    if host.manual_sent.is_some() {
        assert!(
            !setup_stop_requested(revision),
            "an older covered startup must never register and Stop the newer manual run"
        );
    }
    assert_eq!(setup_run(), Some(revision));
    let snapshot = host.setup.snapshot().unwrap();
    assert_eq!(snapshot.revision, revision);
    if host.manual_sent.is_some() {
        assert!(!snapshot.finished && snapshot.permissions_allowed);
        assert_eq!(host.setup.recover().unwrap(), revision);
    } else {
        assert!(snapshot.finished && !snapshot.permissions_allowed);
        assert!(host.setup.recover().is_err());
    }
    assert_eq!(host.binds, 1);
    match manual_at {
        ManualAt::DuringBind => {
            assert_eq!(host.bound_revision, revision);
            assert_eq!(resumed, Some(revision));
            assert_eq!(host.claims, 1);
            assert_eq!(script().runs, [revision]);
            assert!(script().stops.is_empty());
        }
        ManualAt::DuringBindAfterStop => {
            assert_eq!(host.bound_revision, revision);
            assert!(unavailable && resumed.is_none());
            assert_eq!(
                host.claims, 0,
                "the covered coordinator retires before claiming"
            );
            assert!(script().runs.is_empty());
            assert_eq!(script().stops, [predecessor]);
        }
        ManualAt::AfterCapture => {
            assert_eq!(host.bound_revision, predecessor);
            assert!(unavailable && resumed.is_none());
            assert_eq!(
                host.claims, 1,
                "the existing revision fence runs under the claim"
            );
            assert!(script().runs.is_empty());
            assert!(script().stops.is_empty());
        }
        ManualAt::ExistingAfterStop => {
            assert_eq!(host.bound_revision, predecessor);
            assert!(resumed.is_none(), "the stopped run is never adopted");
            assert!(setup_stop_requested(predecessor));
            assert!(script().runs.is_empty());
            assert_eq!(script().stops, [predecessor]);
        }
    }
    assert_eq!(STARTUP_CLAIMS.load(Ordering::Acquire), 0);
    assert!(!INITIAL_SETUP_BUSY.load(Ordering::Acquire));
    clear_setup_run(revision);
}

#[test]
fn startup_bind_stop_a_newer_manual_admission_survives_the_older_coordinator() {
    exercise(ManualAt::DuringBindAfterStop);
}

#[test]
fn startup_bind_stop_an_unstopped_capture_can_resume_the_current_admission() {
    exercise(ManualAt::DuringBind);
}

#[test]
fn startup_bind_stop_an_older_bound_revision_cannot_resume_a_later_choice() {
    exercise(ManualAt::AfterCapture);
}

#[test]
fn startup_bind_stop_a_known_registered_target_stays_stopped_without_adoption() {
    exercise(ManualAt::ExistingAfterStop);
}
