// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_cli::macos_doctor::{InitialSetupAdmissionFault, InitialSetupAdmissionStopTest};

struct ScratchStartup<'a, 'root> {
    setup: &'a mut InitialSetupAdmissionStopTest<'root>,
    fault: InitialSetupAdmissionFault,
    sent: StartupSent,
    stop_after_persist: bool,
    retry: bool,
    stop_on_error: bool,
    calls: usize,
    waits: usize,
    failures: Vec<String>,
    receipt: Option<aterm_gui::InitialSetupApplyReceipt>,
}

impl SetupStartup for ScratchStartup<'_, '_> {
    type Claim = StartupClaim;

    fn bind_authorization(&mut self) -> Result<u64, String> {
        panic!("the scratch ticket was bound before its answer")
    }
    fn revision(&mut self) -> Result<u64, String> {
        panic!("an accepted answer does not reload its baseline")
    }
    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        StartupClaim::claim()
    }
    fn finished(&mut self) -> Result<bool, String> {
        panic!("a terminal admission error does not turn into a fresh review")
    }
    fn admission(&mut self) -> Result<Option<SetupAdmission>, String> {
        panic!("an accepted answer retains its original ticket")
    }
    fn initial(&mut self) -> Result<Plan, String> {
        panic!("the answer is already selected")
    }
    fn offer(&mut self, _: Plan) -> aterm_gui::WorkstationSetupDecision {
        panic!("a failed answer is not offered again")
    }
    fn authorize(&mut self, _: &Plan) -> Result<Option<u64>, String> {
        self.calls += 1;
        assert!(self.calls <= 1 + usize::from(self.retry));
        let result = self.setup.persist(if self.calls == 1 {
            self.fault
        } else {
            InitialSetupAdmissionFault::None
        });
        if self.stop_after_persist && self.calls == 1 {
            let _ = stop_initial_setup();
            assert!(
                !settle_setup_stop(Duration::ZERO),
                "the delivered answer is still under way before its worker returns"
            );
        }
        self.sent.record_publication(self.setup.stop_target());
        result
    }
    fn authorization_pending(&self) -> bool {
        self.setup.pending()
    }
    fn stopped(&self) -> bool {
        self.stop_on_error && stop_covers(self.sent.sent)
    }
    fn wait_holding_claim(&mut self) -> bool {
        assert!(
            self.retry,
            "the lost binding refuses further admission writes"
        );
        self.waits += 1;
        assert_saved_stop(self.setup);
        false
    }
    fn backoff(&mut self) {
        panic!("a terminal answer is not re-admitted after a wait")
    }
    fn report_failure(&mut self, why: &str) {
        self.failures.push(why.to_owned());
    }
}

fn answer(
    setup: &mut InitialSetupAdmissionStopTest<'_>,
    fault: InitialSetupAdmissionFault,
    stop_after_persist: bool,
) -> (PreparedSetup<StartupClaim>, StartupSent) {
    answer_with_retry(setup, fault, stop_after_persist, false)
}

fn answer_with_retry(
    setup: &mut InitialSetupAdmissionStopTest<'_>,
    fault: InitialSetupAdmissionFault,
    stop_after_persist: bool,
    retry: bool,
) -> (PreparedSetup<StartupClaim>, StartupSent) {
    answer_with_sent(
        setup,
        fault,
        stop_after_persist,
        retry,
        StartupSent::default(),
    )
}

pub(super) fn answer_with_sent(
    setup: &mut InitialSetupAdmissionStopTest<'_>,
    fault: InitialSetupAdmissionFault,
    stop_after_persist: bool,
    retry: bool,
    sent: StartupSent,
) -> (PreparedSetup<StartupClaim>, StartupSent) {
    let plan = setup.plan();
    let mut host = ScratchStartup {
        setup,
        fault,
        sent,
        stop_after_persist,
        retry,
        stop_on_error: false,
        calls: 0,
        waits: 0,
        failures: Vec::new(),
        receipt: None,
    };
    host.sent.send();
    // This is the same shared guard installed synchronously by a delivered Begin.
    host.sent.answered();
    let claim = host.claim().unwrap().unwrap();
    let prepared = finish_setup_choice(&mut host, claim, plan);
    assert_eq!(host.calls, 1 + usize::from(retry));
    assert_eq!(host.waits, usize::from(retry));
    (prepared, std::mem::take(&mut host.sent))
}

fn assert_saved_stop(setup: &InitialSetupAdmissionStopTest<'_>) {
    assert!(
        setup.pending(),
        "the original admission still owns its retry"
    );
    assert!(
        settle_setup_stop(HANG),
        "Stop must be saved before the admission enters its retry wait"
    );
    let snapshot = setup.snapshot().unwrap();
    assert_eq!(script().stops, [snapshot.revision]);
    assert!(snapshot.finished && !snapshot.permissions_allowed);
}

#[test]
fn admission_stop_handoff_startup_saves_stop_before_waiting_on_an_ordinary_final_error() {
    let _globals = globals();
    for fault in [
        InitialSetupAdmissionFault::BeforeFinal,
        InitialSetupAdmissionFault::AfterFinal,
    ] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
        change(|script| script.stop_root = Some(root.path().to_owned()));
        let (prepared, sent) = answer_with_retry(&mut setup, fault, true, true);
        assert!(matches!(prepared, PreparedSetup::Unavailable));
        run_prepared_setup(prepared, None, sent);
        assert!(settle_setup_stop(Duration::ZERO));
        assert!(setup.recover().is_err());
        assert!(script().runs.is_empty());
    }
}

#[test]
fn admission_stop_handoff_a_covered_startup_failure_retires_its_receipt_without_retrying() {
    let _globals = globals();
    for fault in [
        InitialSetupAdmissionFault::BeforeFinal,
        InitialSetupAdmissionFault::AfterFinal,
    ] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
        change(|script| script.stop_root = Some(root.path().to_owned()));
        let plan = setup.plan();
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(false);
        let mut host = ScratchStartup {
            setup: &mut setup,
            fault,
            sent: StartupSent::default(),
            stop_after_persist: true,
            retry: false,
            stop_on_error: true,
            calls: 0,
            waits: 0,
            failures: Vec::new(),
            receipt: Some(receipt),
        };
        host.sent.send();
        host.sent.answered();
        let claim = host.claim().unwrap().unwrap();
        let prepared = finish_setup_choice(&mut host, claim, plan);
        assert!(matches!(prepared, PreparedSetup::Unavailable));
        assert_eq!((host.calls, host.waits), (1, 0));
        assert!(host.receipt.is_some());
        drop(host);
        assert!(!answer.recv_timeout(HANG).unwrap());
        assert!(settle_setup_stop(HANG));
        assert!(setup.snapshot().unwrap().finished);
        assert!(setup.recover().is_err());
        assert!(script().runs.is_empty());
    }
}

#[test]
fn admission_stop_handoff_a_live_manual_failure_releases_its_receipt_and_busy_slot() {
    let _globals = globals();
    for fault in [
        InitialSetupAdmissionFault::BeforeFinal,
        InitialSetupAdmissionFault::AfterFinal,
    ] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = InitialSetupAdmissionStopTest::restart(root.path()).unwrap();
        assert!(setup.persist(fault).is_err());
        assert!(setup.pending());
        let plan = setup.plan();
        let revision = setup.revision().unwrap();
        let restart = setup.into_restart();
        change(|script| {
            script.stop_root = Some(root.path().to_owned());
            script.persist_hold = true;
        });
        let sent = send_admission();
        let under_way = UnderWay::new(sent);
        let busy = take_initial_setup_slot().unwrap();
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        let worker = std::thread::spawn(move || {
            let mut host = LiveSetupRestart {
                restart,
                vault_typed: false,
                observation: None,
                generation: SETUP_APPLY_GENERATION.load(std::sync::atomic::Ordering::Acquire),
                sent,
                awaiting: None,
                under_way: Some(under_way),
                startup_held: false,
                retry_seen: SETUP_WAKES.count(),
            };
            let returned = run_setup_restart(&mut host, &plan, None, receipt, busy)
                .expect("the stopped failure does not dispatch a run");
            drop(host);
            drop(returned);
        });
        wait_until("the live manual retry enters persistence", || {
            script().persists == 1
        });
        let _ = stop_initial_setup();
        assert!(!settle_setup_stop(Duration::ZERO));
        change(|script| script.persist_hold = false);
        assert!(
            !answer.recv_timeout(HANG).unwrap(),
            "Stop retires the failed choice rather than leaving its receipt in backoff"
        );
        worker.join().unwrap();
        assert_eq!(script().persists, 1);
        assert!(!INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
        assert!(settle_setup_stop(HANG));
        assert_eq!(script().stops, [revision]);
        assert!(
            aterm_cli::macos_doctor::ProvisionalSetupWatchTest::at(root.path())
                .snapshot()
                .unwrap()
                .finished
        );
        assert!(script().runs.is_empty());
    }
}

struct ScratchRestartRetry<'a, 'root> {
    setup: &'a mut InitialSetupAdmissionStopTest<'root>,
    fault: InitialSetupAdmissionFault,
    under_way: Option<UnderWay>,
    calls: usize,
    waits: usize,
}

impl SetupRestartHost for ScratchRestartRetry<'_, '_> {
    type Claim = aterm_cli::macos_doctor::AutomaticSetupClaim;
    type Busy = InitialSetupBusy;
    type Password = ();
    type Receipt = ();

    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        (setup_io().claim_rerun)()
    }
    fn startup_holds_claim(&self) -> bool {
        false
    }
    fn await_claim(&mut self) {
        panic!("the scratch restart has no competing startup")
    }
    fn persist(&mut self, _: &Plan) -> Result<Option<u64>, String> {
        self.calls += 1;
        assert!(self.calls <= 2);
        let result = self.setup.persist(if self.calls == 1 {
            self.fault
        } else {
            InitialSetupAdmissionFault::None
        });
        if self.calls == 1 {
            let _ = stop_initial_setup();
            assert!(!settle_setup_stop(Duration::ZERO));
        }
        self.under_way
            .as_ref()
            .unwrap()
            .record_publication(self.setup.stop_target());
        result
    }
    fn pending(&self) -> bool {
        self.setup.pending()
    }
    fn backoff(&mut self) {
        self.waits += 1;
        assert_saved_stop(self.setup);
    }
    fn admitted(
        &mut self,
        _: u64,
        _: Self::Claim,
        _: Self::Busy,
        (): Self::Password,
        (): Self::Receipt,
    ) {
        panic!("Stop must retire this exact admission before any run dispatch")
    }
    fn failure(&mut self, _: &str) {}
    fn held_elsewhere(&self, _: &str) -> bool {
        false
    }
    fn queue(&mut self) -> bool {
        // No other setup holds this scratch Mac's records: the choice is let go, as before.
        false
    }
}

#[test]
fn admission_stop_handoff_manual_saves_stop_before_waiting_on_an_ordinary_final_error() {
    let _globals = globals();
    for fault in [
        InitialSetupAdmissionFault::BeforeFinal,
        InitialSetupAdmissionFault::AfterFinal,
    ] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = InitialSetupAdmissionStopTest::restart(root.path()).unwrap();
        change(|script| script.stop_root = Some(root.path().to_owned()));
        let plan = setup.plan();
        let mut host = ScratchRestartRetry {
            setup: &mut setup,
            fault,
            under_way: Some(UnderWay::new(send_admission())),
            calls: 0,
            waits: 0,
        };
        let busy = take_initial_setup_slot().unwrap();
        let busy = run_setup_restart(&mut host, &plan, (), (), busy)
            .expect("the stopped restart does not transfer its slot to a worker");
        assert_eq!(host.calls, 2);
        assert_eq!(host.waits, 1);
        drop(host);
        drop(busy);
        assert!(settle_setup_stop(Duration::ZERO));
        assert!(setup.recover().is_err());
        assert!(script().runs.is_empty());
    }
}

#[test]
fn admission_stop_handoff_stops_its_published_target_after_the_answer_is_refused() {
    let _globals = globals();
    for fault in [
        InitialSetupAdmissionFault::BeforeFinalPublicationUnknown,
        InitialSetupAdmissionFault::FinalPublicationUnknown,
    ] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
        change(|script| script.stop_root = Some(root.path().to_owned()));
        let (prepared, sent) = answer(&mut setup, fault, true);
        assert!(matches!(prepared, PreparedSetup::Unavailable));
        assert!(!setup.pending(), "write authority is terminally refused");
        // The Stop's worker may be saving its request now, which a snapshot sees changing.
        let revision = setup.revision().unwrap();
        run_prepared_setup(prepared, None, sent);
        assert!(settle_setup_stop(HANG));
        assert_eq!(script().stops, [revision]);
        let stopped = setup.snapshot().unwrap();
        assert!(stopped.finished && !stopped.permissions_allowed);
        assert!(
            setup.recover().is_err(),
            "relaunch cannot mint a new run from the stopped staged admission"
        );
        assert!(script().runs.is_empty());
    }
}

#[test]
fn admission_stop_handoff_keeps_unidentified_publications_owed_after_owner_drop() {
    let _globals = globals();
    for fault in [
        InitialSetupAdmissionFault::PlanPublicationUnknown,
        InitialSetupAdmissionFault::ProgressPublicationUnknown,
    ] {
        for late_stop in [false, true] {
            let _io = TestIo::install();
            let root = aterm_tempfile::tempdir().unwrap();
            let mut setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
            let (prepared, sent) = answer(&mut setup, fault, !late_stop);
            assert!(matches!(prepared, PreparedSetup::Unavailable));
            run_prepared_setup(prepared, None, sent);
            if late_stop {
                let _ = stop_initial_setup();
            }
            assert!(
                !settle_setup_stop(Duration::ZERO),
                "a lost publication identity cannot certify that Stop has nothing to save"
            );
            assert!(script().stop_attempts.is_empty(), "no target is guessed");
            let before = setup.snapshot().unwrap().observation;
            assert_eq!(
                setup.persist(InitialSetupAdmissionFault::None).unwrap(),
                None
            );
            assert_eq!(setup.snapshot().unwrap().observation, before);
            assert!(script().runs.is_empty());
        }
    }
}

struct ScratchChoice<'a, 'root> {
    setup: &'a InitialSetupAdmissionStopTest<'root>,
    sent: u64,
    under_way: Option<UnderWay>,
}

impl SetupRestartHost for ScratchChoice<'_, '_> {
    type Claim = aterm_cli::macos_doctor::AutomaticSetupClaim;
    type Busy = InitialSetupBusy;
    type Password = ();
    type Receipt = ();

    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        (setup_io().claim_rerun)()
    }
    fn startup_holds_claim(&self) -> bool {
        false
    }
    fn await_claim(&mut self) {
        panic!("the terminal startup already released its claim")
    }
    fn persist(&mut self, _: &Plan) -> Result<Option<u64>, String> {
        self.setup.choose().map(Some)
    }
    fn pending(&self) -> bool {
        false
    }
    fn backoff(&mut self) {
        panic!("the replacement admission must succeed")
    }
    fn admitted(
        &mut self,
        revision: u64,
        claim: Self::Claim,
        busy: Self::Busy,
        (): Self::Password,
        (): Self::Receipt,
    ) {
        retire_unconfirmed_admissions(self.sent);
        register_admitted_run(revision, self.sent, false, None);
        drop(self.under_way.take());
        (setup_io().run)(Some(claim), revision, None, busy);
    }
    fn failure(&mut self, why: &str) {
        panic!("the fresh explicit choice was refused: {why}")
    }
    fn held_elsewhere(&self, _: &str) -> bool {
        false
    }
    fn queue(&mut self) -> bool {
        panic!("no other setup holds this scratch Mac's records")
    }
}

#[test]
fn admission_stop_handoff_a_fresh_explicit_choice_supersedes_only_abandoned_debt() {
    let _globals = globals();
    for drop_before_choice in [false, true] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
        change(|script| script.stop_root = Some(root.path().to_owned()));
        let (prepared, sent) = answer(
            &mut setup,
            InitialSetupAdmissionFault::PlanPublicationUnknown,
            true,
        );
        assert!(matches!(prepared, PreparedSetup::Unavailable));
        let mut previous = Some((prepared, sent));
        if drop_before_choice {
            let (prepared, sent) = previous.take().unwrap();
            run_prepared_setup(prepared, None, sent);
        }
        let sent = send_admission();
        let mut choice = ScratchChoice {
            setup: &setup,
            sent,
            under_way: Some(UnderWay::new(sent)),
        };
        let busy = take_initial_setup_slot().unwrap();
        assert!(run_setup_restart(&mut choice, &setup.plan(), (), (), busy).is_none());
        drop(choice);
        let revision = setup.snapshot().unwrap().revision;
        assert_eq!(script().runs, [revision]);
        assert!(!setup_stop_requested(revision));
        assert!(!setup.snapshot().unwrap().finished);
        assert_eq!(settle_setup_stop(Duration::ZERO), drop_before_choice);
        if let Some((prepared, sent)) = previous {
            run_prepared_setup(prepared, None, sent);
        }
        assert!(settle_setup_stop(Duration::ZERO));
        assert!(script().stop_attempts.is_empty());
    }
}

#[test]
fn admission_stop_handoff_no_stop_and_untouched_refusals_owe_no_save() {
    let _globals = globals();
    for superseded in [false, true] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
        if superseded {
            setup.choose().unwrap();
        }
        let before = setup.snapshot().unwrap().observation;
        let (prepared, sent) = answer(
            &mut setup,
            if superseded {
                InitialSetupAdmissionFault::None
            } else {
                InitialSetupAdmissionFault::FinalPublicationUnknown
            },
            superseded,
        );
        assert!(matches!(prepared, PreparedSetup::Unavailable));
        run_prepared_setup(prepared, None, sent);
        assert!(settle_setup_stop(Duration::ZERO));
        assert!(script().stop_attempts.is_empty());
        if superseded {
            assert_eq!(setup.snapshot().unwrap().observation, before);
        } else {
            assert!(setup.recover().is_ok(), "without Stop, relaunch can resume");
        }
        assert!(script().runs.is_empty());
    }
}

/// The first-launch offer answered with a typed Join while this Mac's vault configuration
/// cannot be read (a root-owned `source.toml`): the real startup review admits the answer at
/// once, as the person's choice of groups without the Join, so aterm's own permissions (Full
/// Disk Access, the debugger, Local Network) get their run with no other press. Nothing is
/// tried again, nothing is reported failed, and the window learns the vault was not joined.
#[test]
fn a_startup_join_over_an_unreadable_vault_is_admitted_at_once_without_the_join() {
    struct JoinHost<'a, 'root> {
        setup: &'a mut InitialSetupAdmissionStopTest<'root>,
        tries: usize,
        not_joined: bool,
        failures: Vec<String>,
    }
    impl SetupStartup for JoinHost<'_, '_> {
        type Claim = ();
        fn bind_authorization(&mut self) -> Result<u64, String> {
            panic!("the scratch ticket was bound before its answer")
        }
        fn revision(&mut self) -> Result<u64, String> {
            panic!("an answered offer does not reload its baseline")
        }
        fn claim(&mut self) -> Result<Option<()>, String> {
            Ok(Some(()))
        }
        fn finished(&mut self) -> Result<bool, String> {
            Ok(false)
        }
        fn admission(&mut self) -> Result<Option<SetupAdmission>, String> {
            panic!("an answered offer keeps its own ticket")
        }
        fn initial(&mut self) -> Result<Plan, String> {
            panic!("the answer is already given")
        }
        fn offer(&mut self, _: Plan) -> aterm_gui::WorkstationSetupDecision {
            panic!("an answered offer is not offered again")
        }
        fn authorize(&mut self, _: &Plan) -> Result<Option<u64>, String> {
            self.tries += 1;
            let (result, not_joined) = self
                .setup
                .persist_join_over_unread_vault("https://typed.example/v1/typed");
            self.not_joined = not_joined;
            result
        }
        fn authorization_pending(&self) -> bool {
            self.setup.pending()
        }
        /// Bounds the old retry loop, so the test ends to say it looped.
        fn stopped(&self) -> bool {
            self.tries >= 3
        }
        fn wait_holding_claim(&mut self) -> bool {
            false
        }
        fn backoff(&mut self) {}
        fn report_failure(&mut self, why: &str) {
            self.failures.push(why.to_owned());
        }
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let mut setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
    let plan = setup.plan();
    let mut host = JoinHost {
        setup: &mut setup,
        tries: 0,
        not_joined: false,
        failures: Vec::new(),
    };
    let prepared = finish_setup_choice(&mut host, (), plan);
    assert!(
        matches!(prepared, PreparedSetup::Authorized { .. }),
        "tries {}, failures {:?}: aterm's permissions wait",
        host.tries,
        host.failures
    );
    assert_eq!((host.tries, host.failures.len()), (1, 0));
    assert!(host.not_joined, "the page says the vault was not joined");
    let snapshot = setup.snapshot().unwrap();
    assert!(snapshot.permissions_allowed && !snapshot.finished);
}
