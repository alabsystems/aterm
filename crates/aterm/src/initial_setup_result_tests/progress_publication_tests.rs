// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_cli::macos_doctor::{BroughtForward, InitialSetupAdmissionStopTest};
use std::sync::atomic::Ordering;

struct Startup<'a> {
    root: &'a Path,
    setup: InitialSetupAdmissionStopTest<'a>,
    sent: StartupSent,
    stop: bool,
    brought: usize,
    bound: usize,
}

impl SetupStartup for Startup<'_> {
    type Claim = StartupClaim;

    fn send(&mut self) {
        self.sent.send();
    }

    fn bring_forward(&mut self) -> bool {
        self.brought += 1;
        assert_eq!(
            self.brought, 1,
            "this fixture ends bring-forward after one failed write"
        );
        let result = self.sent.observe_repair_with(|published| {
            self.setup.bring_forward_progress_error(true, published)
        });
        assert!(result.is_err());
        if self.stop {
            stop_initial_setup();
        }
        true
    }

    fn bind_authorization(&mut self) -> Result<u64, String> {
        self.bound += 1;
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
        panic!("the still-Restored decision finishes startup before an admission is read")
    }

    fn initial(&mut self) -> Result<Plan, String> {
        panic!("the interrupted bring-forward has not published Pending yet")
    }

    fn offer(&mut self, _: Plan) -> aterm_gui::WorkstationSetupDecision {
        panic!("a Restore's remaining decision offers nothing")
    }

    fn authorize(&mut self, _: &Plan) -> Result<Option<u64>, String> {
        panic!("no Begin was delivered")
    }

    fn authorization_pending(&self) -> bool {
        self.setup.pending()
    }

    fn stopped(&self) -> bool {
        stop_covers(self.sent.sent)
    }

    fn wait_holding_claim(&mut self) -> bool {
        panic!("there is no accepted answer to persist")
    }

    fn backoff(&mut self) {
        panic!("a readable Restored decision completes the coordinator")
    }

    fn report_failure(&mut self, _: &str) {}
}

#[test]
fn startup_progress_a_renamed_checkpoint_remains_stoppable_when_its_flush_returns_an_error() {
    let _globals = globals();
    for stop in [false, true] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        *CLAIM_ROOT.lock().unwrap() = Some(root.path().to_owned());
        change(|script| script.stop_root = Some(root.path().to_owned()));
        let mut host = Startup {
            root: root.path(),
            setup: InitialSetupAdmissionStopTest::restored_before_run(root.path()).unwrap(),
            sent: StartupSent::default(),
            stop,
            brought: 0,
            bound: 0,
        };
        let prepared = prepare_setup(&mut host).unwrap();
        if stop {
            assert!(matches!(prepared, PreparedSetup::Unavailable));
            assert_eq!(host.bound, 0);
        } else {
            assert!(matches!(prepared, PreparedSetup::Finished));
            assert_eq!(host.bound, 1);
        }
        let staged = host.setup.snapshot().unwrap();
        assert_ne!(staged.revision, 0, "the actual Progress rename succeeded");
        assert!(staged.finished && !staged.permissions_allowed);
        assert!(host.setup.admission().unwrap().is_none());
        run_prepared_setup(prepared, None, std::mem::take(&mut host.sent));
        assert_eq!(STARTUP_CLAIMS.load(Ordering::Acquire), 0);
        assert!(!INITIAL_SETUP_BUSY.load(Ordering::Acquire));
        wait_until("the Stop saver finishes its queued IO", || {
            let saves = stop_saves();
            saves.wanted.is_none() && saves.saving.is_none()
        });
        assert!(
            settle_setup_stop(Duration::ZERO),
            "a readable owned checkpoint must resolve its publication debt after the coordinator retires"
        );

        // The retry is exactly what another launch reads. Its matching Progress
        // is already there, so it performs no second checkpoint write.
        let next = host
            .setup
            .bring_forward_progress_error(false, |_| {})
            .unwrap();
        if stop {
            assert_eq!(script().stops, [staged.revision]);
            assert_eq!(next, BroughtForward::Nothing);
            assert!(host.setup.recover().is_err());
            let held = host.setup.snapshot().unwrap();
            assert!(held.finished && !held.permissions_allowed);
            assert_eq!(held.plan, staged.plan);
        } else {
            assert_eq!(next, BroughtForward::Run(staged.revision));
            assert!(script().stops.is_empty());
            let revision = host.setup.recover().unwrap();
            assert!(revision > staged.revision);
            let admitted = host.setup.snapshot().unwrap();
            assert!(!admitted.finished && admitted.permissions_allowed);
        }
        assert!(
            script().runs.is_empty(),
            "this drives record writers, never an OS setup stage"
        );
    }
}
