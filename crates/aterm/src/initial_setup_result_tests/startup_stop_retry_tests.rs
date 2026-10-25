// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_cli::macos_doctor::{
    AutomaticSetupClaim, InitialSetupAdmissionFault, InitialSetupAdmissionStopTest,
    initial_setup_scope_ready_at, set_aside_unreadable_setup_at_observing,
};
use std::sync::atomic::Ordering;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadAt {
    Scope,
    SetAside,
    Completion,
}

struct Startup<'a> {
    home: &'a Path,
    root: &'a Path,
    setup: InitialSetupAdmissionStopTest<'a>,
    sent: StartupSent,
    read_at: ReadAt,
    stop: bool,
    hidden: Option<std::path::PathBuf>,
    peer: Option<AutomaticSetupClaim>,
    awaiting: Option<AwaitingClaim>,
    receipt: Option<aterm_gui::InitialSetupApplyReceipt>,
    reads: usize,
    writes: usize,
    waits: usize,
    backoffs: usize,
    failures: Vec<String>,
    rejections: Vec<String>,
}

impl Startup<'_> {
    fn hide_decision(&mut self) {
        let decision = self.root.join("first-run.json");
        let hidden = self.root.join("decision-held");
        std::fs::rename(&decision, &hidden).unwrap();
        std::fs::create_dir(decision).unwrap();
        self.hidden = Some(hidden);
    }

    fn restore_read(&mut self) {
        let hidden = self.hidden.take().unwrap();
        if self.read_at == ReadAt::Scope {
            std::fs::rename(hidden, self.home).unwrap();
        } else {
            let decision = self.root.join("first-run.json");
            std::fs::remove_dir(&decision).unwrap();
            std::fs::rename(hidden, decision).unwrap();
        }
    }

    fn read_started(&mut self) {
        assert!(
            !self.stopped(),
            "a covered startup must retire before another read attempt"
        );
        self.reads += 1;
    }
}

impl SetupStartup for Startup<'_> {
    type Claim = StartupClaim;

    fn send(&mut self) {
        self.sent.send();
    }

    fn scope_ready(&mut self) -> Result<bool, String> {
        if self.read_at != ReadAt::Scope {
            return Ok(true);
        }
        self.read_started();
        initial_setup_scope_ready_at(self.home, self.home)
    }

    fn set_aside(&mut self) -> Result<(), String> {
        if self.read_at != ReadAt::SetAside {
            return Ok(());
        }
        self.read_started();
        self.recover_after_read_error().map(|_| ())
    }

    fn recover_after_read_error(&mut self) -> Result<bool, String> {
        let root = self.root;
        self.sent
            .observe_repair_with(|published| {
                set_aside_unreadable_setup_at_observing(root, false, published)
            })
            .map(|set| set.is_some())
    }

    fn stopped(&self) -> bool {
        stop_covers(self.sent.sent)
    }

    fn bind_authorization(&mut self) -> Result<u64, String> {
        panic!("a stopped pre-offer retry must not bind a review")
    }

    fn revision(&mut self) -> Result<u64, String> {
        panic!("the delivered answer keeps its original ticket")
    }

    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        StartupClaim::claim()
    }

    fn finished(&mut self) -> Result<bool, String> {
        self.read_started();
        self.setup
            .snapshot()
            .map(|snapshot| snapshot.finished)
            .map_err(|why| why.to_string())
    }

    fn admission(&mut self) -> Result<Option<SetupAdmission>, String> {
        panic!("the delivered answer is not a fresh resume")
    }

    fn initial(&mut self) -> Result<Plan, String> {
        panic!("no old review may be offered again")
    }

    fn offer(&mut self, _: Plan) -> aterm_gui::WorkstationSetupDecision {
        panic!("no old review may be offered again")
    }

    fn authorize(&mut self, _: &Plan) -> Result<Option<u64>, String> {
        self.writes += 1;
        assert!(self.writes <= 2);
        let result = self.setup.persist(if self.writes == 1 {
            InitialSetupAdmissionFault::BeforeFinal
        } else {
            InitialSetupAdmissionFault::None
        });
        self.sent.record_publication(self.setup.stop_target());
        if result.as_ref().is_ok_and(Option::is_some) {
            self.sent.admitted();
            self.receipt.take().unwrap().accept();
        }
        result
    }

    fn authorization_pending(&self) -> bool {
        self.setup.pending()
    }

    fn wait_holding_claim(&mut self) -> bool {
        self.waits += 1;
        assert_eq!(self.waits, 1);
        self.awaiting = Some(AwaitingClaim::new());
        assert!(choice_awaits_claim());
        true
    }

    fn backoff(&mut self) {
        self.backoffs += 1;
        assert_eq!(STARTUP_CLAIMS.load(Ordering::Acquire), 0);
        if self.read_at == ReadAt::Completion && self.backoffs == 1 {
            self.awaiting.take();
            self.peer = (setup_io().claim_rerun)().unwrap();
            assert!(
                self.peer.is_some(),
                "the accepted answer released its claim"
            );
            self.hide_decision();
            return;
        }
        if self.hidden.is_some() {
            self.restore_read();
            if self.stop {
                assert!(stop_initial_setup());
                assert!(self.stopped());
                wait_until("the exact Stop is written", stop_worker_idle);
            } else {
                self.peer.take();
            }
            return;
        }
        assert!(!self.stop, "Stop must not enter another retry wait");
        assert_eq!(self.backoffs, 3);
    }

    fn report_failure(&mut self, why: &str) {
        assert!(!self.stopped(), "Stop itself is not a startup failure");
        self.failures.push(why.to_owned());
    }

    fn report_rejection(&mut self, why: &str) {
        self.rejections.push(why.to_owned());
    }
}

fn exercise(read_at: ReadAt, stop: bool) {
    let _globals = globals();
    let _io = TestIo::install();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let root = home.join("doctor");
    *CLAIM_ROOT.lock().unwrap() = Some(root.clone());
    change(|script| script.stop_root = Some(root.clone()));
    let mut setup = InitialSetupAdmissionStopTest::startup(&root).unwrap();
    let predecessor = if read_at == ReadAt::Completion {
        None
    } else {
        let revision = setup
            .persist(InitialSetupAdmissionFault::None)
            .unwrap()
            .unwrap();
        register_admitted_run(revision, send_admission(), false, None);
        Some(revision)
    };
    let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(false);
    let mut host = Startup {
        home: &home,
        root: &root,
        setup,
        sent: StartupSent::default(),
        read_at,
        stop,
        hidden: None,
        peer: None,
        awaiting: None,
        receipt: Some(receipt),
        reads: 0,
        writes: 0,
        waits: 0,
        backoffs: 0,
        failures: Vec::new(),
        rejections: Vec::new(),
    };
    let prepared = if read_at == ReadAt::Completion {
        host.send();
        host.sent.answered();
        let plan = host.setup.plan();
        let claim = host.claim().unwrap().unwrap();
        finish_setup_choice(&mut host, claim, plan)
    } else {
        if read_at == ReadAt::Scope {
            let hidden = scratch.path().join("home-held");
            std::fs::rename(&home, &hidden).unwrap();
            host.hidden = Some(hidden);
        } else {
            host.hide_decision();
        }
        prepare_setup(&mut host).unwrap()
    };
    assert_eq!(matches!(prepared, PreparedSetup::Unavailable), stop);
    assert_eq!(host.reads, 1 + usize::from(!stop));
    assert_eq!(
        host.writes,
        if read_at == ReadAt::Completion {
            1 + usize::from(!stop)
        } else {
            0
        }
    );
    assert_eq!(
        host.backoffs,
        if read_at == ReadAt::Completion {
            2 + usize::from(!stop)
        } else {
            1
        }
    );
    assert_eq!(
        host.failures.len(),
        if read_at == ReadAt::Completion { 2 } else { 1 }
    );
    assert!(host.rejections.is_empty(), "Stop retires quietly");
    let revision = host.setup.revision().unwrap();
    let uncertain = read_at != ReadAt::Scope;
    if stop {
        assert_eq!(script().stops, [predecessor.unwrap_or(revision)]);
        assert_eq!(
            stop_saves()
                .under_way
                .iter()
                .any(|debt| debt.sent == host.sent.sent && debt.unconfirmed),
            uncertain,
            "retirement must not pretend an uncertain preparatory write was unpublished"
        );
    }
    run_prepared_setup(prepared, None, std::mem::take(&mut host.sent));
    drop(host);
    assert_eq!(answer.recv_timeout(HANG).unwrap(), !stop);
    assert_eq!(STARTUP_CLAIMS.load(Ordering::Acquire), 0);
    assert!(!INITIAL_SETUP_BUSY.load(Ordering::Acquire));
    assert_eq!(script().runs, if stop { vec![] } else { vec![revision] });
    if stop {
        assert_eq!(settle_setup_stop(Duration::ZERO), !uncertain);
        let old = InitialSetupAdmissionStopTest::startup(&root).unwrap();
        assert!(old.snapshot().unwrap().finished);
        assert!(old.recover().is_err());
        let sent = send_admission();
        let choice = InitialSetupAdmissionStopTest::restart(&root).unwrap();
        let current = choice.choose().unwrap();
        retire_unconfirmed_admissions(sent);
        register_admitted_run(current, sent, false, None);
        assert!(current > revision);
        assert!(!setup_stop_requested(current));
        assert!(settle_setup_stop(Duration::ZERO));
        assert!(!choice.snapshot().unwrap().finished);
        clear_setup_run(current);
    } else {
        clear_setup_run(revision);
    }
}

#[test]
fn startup_stop_retry_a_delivered_answer_retires_its_inner_completion_read() {
    exercise(ReadAt::Completion, true);
}

#[test]
fn startup_stop_retry_a_scope_wait_retires_without_another_query() {
    exercise(ReadAt::Scope, true);
}

#[test]
fn startup_stop_retry_a_set_aside_wait_preserves_uncertainty_without_retrying() {
    exercise(ReadAt::SetAside, true);
}

#[test]
fn startup_stop_retry_an_unstopped_answer_retains_and_finishes_its_original_ticket() {
    exercise(ReadAt::Completion, false);
}
