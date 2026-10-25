// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::Ordering;

struct Healing;

impl Healing {
    fn idle() -> Self {
        assert_eq!(STARTUP_RUNS.load(Ordering::Acquire), 0);
        HEAL_STARTING.store(false, Ordering::Release);
        Self
    }
}

impl Drop for Healing {
    fn drop(&mut self) {
        HEAL_STARTING.store(false, Ordering::Release);
    }
}

#[test]
fn startup_recovery_retries_a_refused_worker_instead_of_latching_it_as_pending() {
    let _globals = globals();
    let _io = TestIo::install();
    let _healing = Healing::idle();
    change(|script| script.startup_spawns = [false, true].into());

    heal_unread_setup();
    assert_eq!(script().startups, 1, "the first spawn was refused");
    assert_eq!(STARTUP_RUNS.load(Ordering::Acquire), 0);
    heal_unread_setup();
    assert_eq!(
        script().startups,
        2,
        "no worker owns the refused spawn; another unread status must recover"
    );
    heal_unread_setup();
    assert_eq!(script().startups, 2, "the accepted worker is pending");
    assert!(script().runs.is_empty(), "spawning grants no run authority");
}

#[test]
fn startup_recovery_coalesces_accepted_workers_and_preserves_scope_refusal() {
    let _globals = globals();
    let _io = TestIo::install();
    let _healing = Healing::idle();

    SETUP_IO.write().unwrap().scope_refused = || true;
    heal_unread_setup();
    assert_eq!(script().startups, 0);
    assert!(!HEAL_STARTING.load(Ordering::Acquire));
    SETUP_IO.write().unwrap().scope_refused = || false;

    heal_unread_setup();
    heal_unread_setup();
    assert_eq!(script().startups, 1, "one accepted worker owns the handoff");
    {
        let _running = StartupRunning::enter();
        assert!(!HEAL_STARTING.load(Ordering::Acquire));
        heal_unread_setup();
        assert_eq!(
            script().startups,
            1,
            "a running startup already reads again"
        );
    }
    heal_unread_setup();
    assert_eq!(
        script().startups,
        2,
        "a later unread state starts a fresh worker"
    );
    assert!(script().runs.is_empty());
}

#[test]
fn startup_recovery_a_stop_between_repaired_attempts_prevents_the_next_offer() {
    let _globals = globals();
    for stop in [false, true] {
        let _io = TestIo::install();
        change(|script| script.authorizations = [Ok(Some(2))].into());
        let mut attempts = 0;
        let mut retired = 0;
        run_repaired_startups(|reserved| {
            attempts += 1;
            if attempts == 1 {
                assert!(reserved.is_none());
                retired = send_admission();
                return Some(StartupSent {
                    sent: retired,
                    ..StartupSent::default()
                });
            }
            assert_eq!(attempts, 2);
            // The loop reserved this continuation. Stop is delivered before
            // its fresh coordinator starts reading the setup records.
            if stop {
                stop_initial_setup();
                assert!(stop_covers(retired));
            }
            let reserved = reserved.expect("the continuation reserved its admission before entry");
            let number = reserved.sent;
            let mut host = Startup {
                awaits_answer: false,
                answer: Some(Plan::default()),
                sent: reserved,
            };
            let prepared = prepare_setup(&mut host).unwrap();
            assert_eq!(
                host.sent.sent, number,
                "neither send boundary remints recovery"
            );
            assert_eq!(script().offers, usize::from(!stop));
            assert_eq!(script().authorizing, usize::from(!stop));
            assert_eq!(matches!(prepared, PreparedSetup::Unavailable), stop);
            run_prepared_setup(prepared, None, host.sent);
            None
        });
        assert_eq!(attempts, 2);
        assert_eq!(script().runs, if stop { vec![] } else { vec![2] });
    }
}

#[test]
fn startup_recovery_stop_covers_a_continuation_reserved_before_its_lock_and_leaves_a_new_choice() {
    let _globals = globals();
    let _io = TestIo::install();
    let retired = send_admission();
    let mut reserved = None;
    stop_initial_setup_with(|| {
        reserved = StartupSent::reserve_recovery(retired);
    });
    let mut reserved = reserved.expect("the continuation linearized before Stop took RUN");
    let number = reserved.sent;
    assert!(stop_covers(retired) && stop_covers(number));
    reserved.send();
    assert_eq!(
        reserved.sent, number,
        "the initial send cannot outrun this Stop"
    );
    let mut host = Startup {
        awaits_answer: false,
        answer: Some(Plan::default()),
        sent: reserved,
    };
    let prepared = prepare_setup(&mut host).unwrap();
    assert!(matches!(prepared, PreparedSetup::Unavailable));
    {
        let script = script();
        assert_eq!((script.offers, script.authorizing), (0, 0));
    }
    run_prepared_setup(prepared, None, host.sent);
    assert!(StartupSent::reserve_recovery(retired).is_none());

    // A genuine later choice has its own admission and is still allowed.
    change(|script| script.admits.push_back(Some(9)));
    let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    assert!(restart_initial_setup(left_as_is(), false, None, receipt));
    assert_eq!(answer.recv_timeout(HANG), Ok(true));
    wait_until("the later choice runs", || script().runs == [9]);
    assert!(!setup_stop_requested(9));
}

#[test]
fn startup_recovery_a_stop_after_delivered_begin_saves_its_exact_reserved_target() {
    use aterm_cli::macos_doctor::{InitialSetupAdmissionFault, InitialSetupAdmissionStopTest};
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    let mut setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
    change(|script| script.stop_root = Some(root.path().to_owned()));
    let reserved = StartupSent::reserve_recovery(send_admission()).unwrap();
    let number = reserved.sent;
    let (prepared, sent) = super::admission_stop_handoff_tests::answer_with_sent(
        &mut setup,
        InitialSetupAdmissionFault::None,
        true,
        false,
        reserved,
    );
    assert!(matches!(prepared, PreparedSetup::Authorized { .. }));
    assert_eq!(sent.sent, number);
    assert!(stop_covers(number));
    run_prepared_setup(prepared, None, sent);
    assert!(settle_setup_stop(HANG));
    let snapshot = setup.snapshot().unwrap();
    assert_eq!(script().stops, [snapshot.revision]);
    assert!(snapshot.finished && !snapshot.permissions_allowed);
    assert!(
        setup.recover().is_err(),
        "relaunch cannot resume the stopped target"
    );
}

struct CorruptAnswer<'a> {
    root: &'a std::path::Path,
    setup: aterm_cli::macos_doctor::InitialSetupAdmissionStopTest<'a>,
    sent: StartupSent,
    receipt: Option<aterm_gui::InitialSetupApplyReceipt>,
    calls: usize,
    repaired: bool,
    result_was_error: Option<bool>,
}

impl SetupStartup for CorruptAnswer<'_> {
    type Claim = StartupClaim;

    fn bind_authorization(&mut self) -> Result<u64, String> {
        panic!("the delivered answer already owns its bound ticket")
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
        panic!("an accepted answer never adopts a later admission")
    }
    fn initial(&mut self) -> Result<Plan, String> {
        panic!("an accepted answer keeps its exact plan")
    }
    fn offer(&mut self, _: Plan) -> aterm_gui::WorkstationSetupDecision {
        panic!("the delivered answer is never offered twice")
    }
    fn authorize(&mut self, _: &Plan) -> Result<Option<u64>, String> {
        self.calls += 1;
        assert_eq!(
            self.calls, 1,
            "the corrupt bound review writes no second time"
        );
        std::fs::write(self.root.join("initial-setup.json"), b"{").unwrap();
        let result = self
            .setup
            .persist(aterm_cli::macos_doctor::InitialSetupAdmissionFault::None);
        self.result_was_error = Some(result.is_err());
        self.sent.record_publication(self.setup.stop_target());
        result
    }
    fn authorization_pending(&self) -> bool {
        self.setup.pending()
    }
    fn authorization_read_failed(&self) -> bool {
        self.setup.snapshot().is_err()
    }
    fn recover_after_read_error(&mut self) -> Result<bool, String> {
        assert_eq!(STARTUP_CLAIMS.load(Ordering::Acquire), 0);
        let probe = aterm_cli::macos_doctor::claim_initial_setup_rerun_at(self.root)?
            .expect("the accepted review releases its actual claim before repair");
        drop(probe);
        self.sent.begin_repair();
        let set = aterm_cli::macos_doctor::set_aside_unreadable_setup_at(self.root, false)?;
        if let Some(set) = set {
            self.sent.record_repair(set.stop_target);
            self.repaired = true;
        } else {
            self.sent.no_repair();
        }
        Ok(self.repaired)
    }
    fn wait_holding_claim(&mut self) -> bool {
        panic!("a corrupt accepted review retires instead of waiting forever")
    }
    fn backoff(&mut self) {
        panic!("a completed repair starts a fresh review without a timer")
    }
    fn report_failure(&mut self, _: &str) {}
}

#[test]
fn startup_recovery_retires_a_corrupt_accepted_answer_then_restarts_unless_stopped() {
    let _globals = globals();
    for (stop, in_place) in [(false, false), (true, false), (false, true), (true, true)] {
        corrupt_accepted_answer_recovery(stop, in_place, false);
    }
}

#[test]
fn startup_recovery_a_stopped_repair_does_not_relaunch_after_saving_its_prior_target() {
    let _globals = globals();
    for stop in [false, true] {
        corrupt_accepted_answer_recovery(stop, true, true);
    }
}

fn corrupt_accepted_answer_recovery(stop: bool, in_place: bool, prior_target: bool) {
    use aterm_cli::macos_doctor::{
        InitialSetupAdmissionFault, InitialSetupAdmissionStopTest, ProvisionalSetupWatchTest,
    };
    let _io = TestIo::install();
    let _healing = Healing::idle();
    let _running = StartupRunning::enter();
    let root = aterm_tempfile::tempdir().unwrap();
    *CLAIM_ROOT.lock().unwrap() = Some(root.path().to_owned());
    change(|script| script.stop_root = Some(root.path().to_owned()));
    let registered = prior_target.then(|| {
        let mut setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
        let revision = setup
            .persist(InitialSetupAdmissionFault::None)
            .unwrap()
            .unwrap();
        register_admitted_run(revision, send_admission(), false, None);
        clear_setup_run(revision);
        revision
    });
    let mut passes = 0;
    let mut original_sent = 0;
    let mut admitted = None;
    run_repaired_startups(|reserved| {
        passes += 1;
        assert_eq!(STARTUP_CLAIMS.load(Ordering::Acquire), 0);
        heal_unread_setup();
        assert_eq!(
            script().startups,
            0,
            "the same worker keeps recovery coalesced"
        );
        if passes == 1 {
            if in_place {
                // Preserve the existing Progress inode when it becomes corrupt.
                // Raw publication pins still match, so this is a real read Err.
                let mut staged = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
                assert!(
                    staged
                        .persist(InitialSetupAdmissionFault::BeforeFinal)
                        .is_err()
                );
            }
            let setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
            let plan = setup.plan();
            let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(false);
            let mut host = CorruptAnswer {
                root: root.path(),
                setup,
                sent: StartupSent::default(),
                receipt: Some(receipt),
                calls: 0,
                repaired: false,
                result_was_error: None,
            };
            host.sent.send();
            host.sent.answered();
            let claim = host.claim().unwrap().unwrap();
            let prepared = finish_setup_choice(&mut host, claim, plan);
            assert!(matches!(prepared, PreparedSetup::Unavailable));
            assert!(
                host.repaired,
                "both unread refusal and read Err require repair"
            );
            assert_eq!(host.result_was_error, Some(in_place));
            assert_eq!(host.setup.pending(), in_place);
            let repaired = host.setup.snapshot().unwrap().observation;
            let bytes = || {
                [
                    "automatic-plan.json",
                    "initial-setup.json",
                    "first-run.json",
                ]
                .map(|name| std::fs::read(root.path().join(name)).ok())
            };
            let unchanged = bytes();
            assert_eq!(
                host.setup
                    .persist(InitialSetupAdmissionFault::None)
                    .unwrap(),
                None
            );
            assert!(
                !host.setup.pending(),
                "no old ticket adopts the repaired baseline"
            );
            assert_eq!(host.setup.snapshot().unwrap().observation, repaired);
            assert_eq!(bytes(), unchanged);
            assert!(host.receipt.is_some());
            assert_eq!(host.calls, 1);
            original_sent = host.sent.sent;
            let mut sent = std::mem::take(&mut host.sent);
            let recovery = sent.take_recovery();
            drop(host);
            assert!(
                !answer.recv_timeout(HANG).unwrap(),
                "the old receipt is refused"
            );
            run_prepared_setup(prepared, None, sent);
            if stop {
                stop_runs_sent_through(original_sent);
            }
            Some(recovery)
        } else {
            assert_eq!(passes, 2);
            let reserved = reserved.expect("the fresh review retains its repair checkpoint");
            assert!(reserved.sent > original_sent);
            let checkpoint = ProvisionalSetupWatchTest::at(root.path())
                .snapshot()
                .unwrap()
                .revision;
            let saves = stop_saves();
            let debt = saves
                .under_way
                .iter()
                .find(|debt| debt.sent == original_sent)
                .expect("the old review retained only its exact repair evidence");
            assert!(debt.owned && !debt.unconfirmed);
            assert_eq!(debt.target, Some(checkpoint));
            drop(saves);
            admitted = Some(
                ProvisionalSetupWatchTest::at(root.path())
                    .recover()
                    .unwrap(),
            );
            None
        }
    });
    assert_eq!(passes, if stop { 1 } else { 2 });
    assert_eq!(admitted.is_some(), !stop);
    let snapshot = ProvisionalSetupWatchTest::at(root.path())
        .snapshot()
        .unwrap();
    assert!(snapshot.recovered);
    for setting in aterm_types::macos_setup::Setting::ALL {
        assert_eq!(
            snapshot.plan.selection.contains(setting),
            setting == aterm_types::macos_setup::Setting::Permissions
        );
    }
    assert!(
        script().runs.is_empty(),
        "this recovery admits but executes no OS stage"
    );
    if stop {
        assert!(settle_setup_stop(HANG));
        assert_eq!(script().stops.last(), Some(&snapshot.revision));
        if let Some(registered) = registered {
            assert!(registered < snapshot.revision);
        }
        let before = [
            "automatic-plan.json",
            "initial-setup.json",
            "first-run.json",
        ]
        .map(|name| std::fs::read(root.path().join(name)).ok());
        let relaunch = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
        let admitted = relaunch.recover();
        assert!(
            admitted.is_err(),
            "automatic relaunch must not admit a repaired plan stopped before its next Begin: {admitted:?}"
        );
        let held = relaunch.snapshot().unwrap();
        assert!(held.finished && !held.permissions_allowed);
        assert_eq!(held.plan, snapshot.plan, "Stop retains every chosen group");
        assert_eq!(
            [
                "automatic-plan.json",
                "initial-setup.json",
                "first-run.json"
            ]
            .map(|name| std::fs::read(root.path().join(name)).ok()),
            before,
            "an automatic relaunch makes no new publication"
        );

        // The cancellation belongs to that repair. A genuine later person choice
        // still admits the retained plan with its own revision and access approval.
        let chosen = InitialSetupAdmissionStopTest::restart(root.path()).unwrap();
        let revision = chosen.choose().unwrap();
        register_admitted_run(revision, send_admission(), false, None);
        let resumed = chosen.snapshot().unwrap();
        assert_eq!(resumed.plan, snapshot.plan);
        assert!(!resumed.finished && resumed.permissions_allowed);
        assert!(!setup_stop_requested(revision));
        clear_setup_run(revision);
        assert!(script().runs.is_empty());
    }
}

fn repaired_review(root: &std::path::Path) -> (StartupSent, u64) {
    let mut sent = StartupSent::default();
    sent.send();
    sent.begin_repair();
    std::fs::write(root.join("first-run.json"), b"{").unwrap();
    let set = aterm_cli::macos_doctor::set_aside_unreadable_setup_at(root, false)
        .unwrap()
        .unwrap();
    sent.record_repair(set.stop_target);
    (sent, set.stop_target)
}

#[test]
fn startup_recovery_a_begin_still_owes_its_publication_after_the_checkpoint_stop_is_saved() {
    use aterm_cli::macos_doctor::{InitialSetupAdmissionFault, InitialSetupAdmissionStopTest};
    let _globals = globals();
    for continuation in [false, true] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        change(|script| script.stop_root = Some(root.path().to_owned()));
        let (mut sent, checkpoint) = repaired_review(root.path());
        let original_sent = sent.sent;
        if continuation {
            sent = StartupSent::continue_recovery(sent).unwrap();
        } else {
            sent.send();
        }
        assert!(sent.sent > original_sent);
        let mut setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
        sent.answered();
        sent.begin_repair();
        sent.repair_failed();
        sent.no_repair();
        {
            let saves = stop_saves();
            assert_eq!(saves.under_way.len(), 2);
            let begin = saves
                .under_way
                .iter()
                .find(|debt| debt.sent == sent.sent)
                .unwrap();
            assert!(begin.owned && begin.target.is_none());
        }

        // The actual first save published a new staged run, but its worker has
        // not handed its result back to the admission ledger yet.
        assert!(
            setup
                .persist(InitialSetupAdmissionFault::BeforeFinal)
                .is_err()
        );
        let revision = setup.stop_target().unwrap().unwrap();
        assert!(revision > checkpoint);
        stop_initial_setup();
        wait_until("the repair checkpoint Stop is saved", || {
            script().stops.contains(&checkpoint)
        });
        assert!(
            !settle_setup_stop(Duration::ZERO),
            "saved repair evidence cannot settle the delivered Begin's first persistence"
        );
        sent.record_publication(setup.stop_target());
        assert!(
            sent.preparing.is_none(),
            "the exact admission resolves earlier preparatory uncertainty"
        );
        assert!(settle_setup_stop(HANG));
        assert_eq!(script().stops, [checkpoint, revision]);
        let held = setup.snapshot().unwrap();
        assert_eq!(held.revision, revision);
        assert!(held.finished && !held.permissions_allowed);
        sent.begin_repair();
        assert!(
            !settle_setup_stop(Duration::ZERO),
            "a later repair must become unresolved again"
        );
        assert!(
            aterm_cli::macos_doctor::set_aside_unreadable_setup_at(root.path(), false)
                .unwrap()
                .is_none()
        );
        sent.no_repair();
        assert!(settle_setup_stop(Duration::ZERO));
        drop(sent);
        assert!(setup.recover().is_err());
        assert!(script().runs.is_empty());
    }
}

#[test]
fn startup_recovery_a_retired_checkpoint_still_receives_its_queued_stop() {
    use aterm_cli::macos_doctor::InitialSetupAdmissionStopTest;
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    change(|script| script.stop_root = Some(root.path().to_owned()));
    let (sent, checkpoint) = repaired_review(root.path());
    drop(sent);
    assert!(stop_saves().under_way.is_empty());
    assert!(
        settle_setup_stop(Duration::ZERO),
        "evidence alone owes no quit debt"
    );
    assert!(stop_initial_setup());
    assert!(settle_setup_stop(HANG));
    assert_eq!(script().stops, [checkpoint]);
    let setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
    assert!(setup.recover().is_err());
    assert!(setup.snapshot().unwrap().finished);
    assert!(script().runs.is_empty());
}

#[test]
fn startup_recovery_preparatory_reads_restore_only_confirmed_no_work() {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    change(|script| script.stop_root = Some(root.path().to_owned()));
    let (mut sent, checkpoint) = repaired_review(root.path());
    stop_initial_setup();
    assert!(settle_setup_stop(HANG));

    sent.begin_repair();
    assert!(
        !settle_setup_stop(Duration::ZERO),
        "a saved older target cannot settle a new mutation"
    );
    assert!(
        aterm_cli::macos_doctor::set_aside_unreadable_setup_at(root.path(), false)
            .unwrap()
            .is_none()
    );
    sent.no_repair();
    assert!(
        settle_setup_stop(Duration::ZERO),
        "a confirmed no-op restores the settled checkpoint"
    );

    sent.begin_repair();
    sent.repair_failed();
    sent.no_repair();
    assert!(
        !settle_setup_stop(Duration::ZERO),
        "a later no-op cannot identify an earlier failed publication"
    );
    assert_eq!(script().stops, [checkpoint]);
    assert!(script().runs.is_empty());
}

#[test]
fn startup_recovery_a_published_checkpoint_is_stoppable_after_its_done_marker_returns_an_error() {
    use aterm_cli::macos_doctor::{
        InitialSetupAdmissionStopTest, set_aside_unreadable_setup_at_observing_done_error,
    };
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    change(|script| script.stop_root = Some(root.path().to_owned()));
    let mut sent = StartupSent::default();
    sent.send();
    sent.begin_repair();
    std::fs::write(root.path().join("first-run.json"), b"{").unwrap();
    let mut published = None;
    let result =
        set_aside_unreadable_setup_at_observing_done_error(root.path(), false, |revision| {
            published = Some(revision)
        });
    assert!(result.is_err());
    let checkpoint = published.expect("the actual staged checkpoint succeeded before Done failed");
    sent.record_repair(checkpoint);
    sent.repair_failed();
    assert!(
        aterm_cli::macos_doctor::set_aside_unreadable_setup_at(root.path(), false)
            .unwrap()
            .is_none()
    );
    stop_initial_setup();
    drop(sent);
    assert!(settle_setup_stop(HANG));
    assert_eq!(script().stops, [checkpoint]);
    let setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
    assert!(setup.recover().is_err());
    assert!(setup.snapshot().unwrap().finished);
    assert!(script().runs.is_empty());
}

fn bring_forward_readable_permissions() -> Result<aterm_cli::macos_doctor::BroughtForward, String> {
    use aterm_cli::macos_doctor::{InitialSetupAdmissionFault, InitialSetupAdmissionStopTest};
    let root = CLAIM_ROOT.lock().unwrap().clone().unwrap();
    let mut setup = InitialSetupAdmissionStopTest::startup(&root)?;
    assert!(
        setup
            .persist(InitialSetupAdmissionFault::BeforeFinal)
            .is_err()
    );
    let snapshot = setup.snapshot().map_err(|why| why.to_string())?;
    assert!(!snapshot.finished);
    assert!(
        setup.admission()?.is_none(),
        "startup must still admit its readable offer"
    );
    Ok(aterm_cli::macos_doctor::BroughtForward::Run(
        snapshot.revision,
    ))
}

fn continue_readable_permissions_inline() {
    use aterm_cli::macos_doctor::{ProvisionalSetupWatchTest, claim_initial_setup_rerun_at};
    assert!(!INITIAL_SETUP_BUSY.load(Ordering::Acquire));
    assert_eq!(STARTUP_CLAIMS.load(Ordering::Acquire), 0);
    let activity = SETUP_ACTIVITY.lock().unwrap();
    assert!(activity.settings.is_empty() && activity.subjects.is_empty());
    drop(activity);
    let root = CLAIM_ROOT.lock().unwrap().clone().unwrap();
    let claim = claim_initial_setup_rerun_at(&root).unwrap().unwrap();
    drop(claim);
    assert!(
        !ProvisionalSetupWatchTest::at(&root)
            .snapshot()
            .unwrap()
            .finished
    );
    ProvisionalSetupWatchTest::at(&root).recover().unwrap();
    change(|script| script.startups_inline += 1);
}

/// A Restore pressed while another setup holds this Mac's setup records waits quietly, its
/// press queued: it reads no row while it waits and runs once, with its readings, when that
/// setup lets go.
#[test]
fn a_restore_waits_quietly_while_another_setup_holds_the_records() {
    use aterm_cli::macos_doctor::{Action, InitialSetupObserved, Report};
    let _globals = globals();
    let _io = TestIo::install();
    let _healing = Healing::idle();
    change(|script| script.records_held.extend([true, true, true]));
    let root = CLAIM_ROOT.lock().unwrap().clone().unwrap();
    let busy = take_initial_setup_slot().unwrap();
    let mut restores = 0;
    run_setup_restore(busy, |_, holding| {
        restores += 1;
        assert!(
            script().records_held.is_empty(),
            "it ran only once they were let go"
        );
        assert!(script().reads.is_empty(), "no row read while it waited");
        assert!(!SETUP_ACTIVITY.lock().unwrap().subjects.is_empty());
        assert!(holding(), "nothing pressed meanwhile");
        InitialSetupObserved {
            result: Ok(Some(Report {
                action: Action::Restore,
                rows: Vec::new(),
                manual_steps: Vec::new(),
                backup: root.join("inert-backup"),
                restore: None,
                page: None,
            })),
            observation: None,
        }
    });
    assert_eq!(restores, 1);
    assert_eq!(
        SETUP_QUEUED.load(Ordering::Acquire),
        0,
        "its press waits no more"
    );
    assert!(!INITIAL_SETUP_BUSY.load(Ordering::Acquire));
    wait_until("the Restore's fresh readings", observe_idle);
}

#[test]
fn startup_recovery_restore_falls_back_on_its_released_worker_when_spawn_is_refused() {
    use aterm_cli::macos_doctor::{
        Action, InitialSetupObserved, ProvisionalSetupWatchTest, Report,
    };
    let _globals = globals();
    for accepted in [true, false] {
        let _io = TestIo::install();
        let _healing = Healing::idle();
        change(|script| script.startup_spawns.push_back(accepted));
        {
            let mut io = SETUP_IO.write().unwrap();
            io.bring_forward = bring_forward_readable_permissions;
            io.start_startup_inline = continue_readable_permissions_inline;
        }
        let root = CLAIM_ROOT.lock().unwrap().clone().unwrap();
        let busy = take_initial_setup_slot().unwrap();
        let mut restores = 0;
        run_setup_restore(busy, |_, holding| {
            restores += 1;
            assert!(INITIAL_SETUP_BUSY.load(Ordering::Acquire));
            assert!(!SETUP_ACTIVITY.lock().unwrap().subjects.is_empty());
            let _claim = aterm_cli::macos_doctor::claim_initial_setup_rerun_at(&root)
                .unwrap()
                .unwrap();
            assert!(holding(), "nothing pressed meanwhile");
            InitialSetupObserved {
                result: Ok(Some(Report {
                    action: Action::Restore,
                    rows: Vec::new(),
                    manual_steps: Vec::new(),
                    backup: root.join("inert-backup"),
                    restore: None,
                    page: None,
                })),
                observation: None,
            }
        });
        assert_eq!(restores, 1);
        assert!(!INITIAL_SETUP_BUSY.load(Ordering::Acquire));
        assert!(SETUP_ACTIVITY.lock().unwrap().subjects.is_empty());
        assert_eq!(script().startups, 1);
        assert_eq!(
            script().startups_inline,
            usize::from(!accepted),
            "a refused spawn must continue the readable pending setup on this released worker"
        );
        let snapshot = ProvisionalSetupWatchTest::at(&root).snapshot().unwrap();
        assert!(!snapshot.finished);
        assert!(script().runs.is_empty(), "no OS stage runs in this fixture");
        wait_until("the Restore's fresh readings", observe_idle);
    }
}

/// The exceptions digest's set-aside entry goes with the page's note (System State design §3.5):
/// while the saved setup reads as set aside, or cannot be read, it stays; once it reads with
/// nothing set aside (the person chose, or Restore ran), it is cleared, once, and a later
/// set-aside raises it again. At launch an earlier launch's entry may stand, so it is cleared
/// at the first reading with nothing set aside.
#[test]
fn the_set_aside_digest_entry_goes_once_the_person_chooses() {
    let _globals = globals();
    let clears = std::cell::Cell::new(0_usize);
    let read = |recovered| set_aside_as_read(recovered, || clears.set(clears.get() + 1));
    SET_ASIDE_SAID.store(true, Ordering::Release);
    read(Some(true));
    read(None);
    assert_eq!(clears.get(), 0, "the set-aside still stands");
    read(Some(false));
    read(Some(false));
    assert_eq!(clears.get(), 1, "cleared once the person chose");
    note_setup_set_aside(&aterm_cli::macos_doctor::SetAside {
        kept: vec!["first-run.json.unreadable".to_owned()],
        stop_target: 1,
    });
    read(Some(true));
    assert_eq!(clears.get(), 1);
    read(Some(false));
    assert_eq!(
        clears.get(),
        2,
        "a later set-aside's entry goes the same way"
    );
}
