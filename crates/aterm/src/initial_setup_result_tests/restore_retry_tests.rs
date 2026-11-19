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
    checkpoint: Option<u64>,
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
        let mut checkpoint = None;
        let result = self.sent.observe_repair_with(|published| {
            self.setup
                .bring_forward_progress_error(self.brought == 1, |revision| {
                    checkpoint = Some(revision);
                    published(revision);
                })
        });
        if self.brought == 1 {
            assert!(
                result.is_err(),
                "the actual Progress namespace flush failed"
            );
            self.checkpoint = Some(checkpoint.expect("the renamed Progress was readable"));
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
            assert_eq!(
                checkpoint, self.checkpoint,
                "the same Progress is recovered"
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
        let revision = self.setup.revision()?;
        assert_eq!(Some(revision), self.checkpoint);
        Ok(revision)
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
            plan: admitted.plan,
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
        checkpoint: None,
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
    change(|script| {
        script.restored_setup_needs_startup = Err("the restored setup records could not be read");
        script.startup_spawns.push_back(false);
    });
    let next_sent = SETUP_SENT.load(Ordering::Acquire) + 1;
    offer_again_after_restore(reserve_restore_startup());
    assert_eq!(script().startup_reservations, [Some(next_sent)]);
    let reserved = script()
        .inline_startup_reservation
        .expect("the refused spawn continued on the Restore worker");
    assert_eq!(reserved, next_sent);
    host.sent = StartupSent::reserved(reserved);
    let prepared = prepare_setup(&mut host).unwrap();
    let PreparedSetup::Authorized { revision, .. } = &prepared else {
        panic!("a repaired bring-forward must dispatch its admitted permissions");
    };
    let revision = *revision;
    assert!(revision > host.checkpoint.unwrap(), "Begin owns a new run");
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
    assert!(host.setup.snapshot().unwrap().run_allowed);
    run_prepared_setup(prepared, None, std::mem::take(&mut host.sent));
    assert_eq!(script().runs, [revision]);
    assert_eq!(STARTUP_CLAIMS.load(Ordering::Acquire), 0);
    assert!(!INITIAL_SETUP_BUSY.load(Ordering::Acquire));
    clear_setup_run(revision);
}

#[test]
fn a_stop_covers_the_pending_restore_review_before_or_after_dispatch() {
    let _globals = globals();
    for in_preflight in [true, false] {
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut host = startup(root.path(), StopAt::Never);
        let original_revision = host.setup.revision().unwrap();
        let next_sent = SETUP_SENT.load(Ordering::Acquire) + 1;
        if in_preflight {
            SETUP_IO.write().unwrap().restored_setup_needs_startup = || {
                let reserved = SETUP_SENT.load(Ordering::Acquire);
                assert!(!stop_covers(reserved));
                let _ = stop_initial_setup();
                assert!(stop_covers(reserved));
                Err("the restored setup records could not be read".into())
            };
        } else {
            change(|script| {
                script.restored_setup_needs_startup =
                    Err("the restored setup records could not be read");
            });
        }
        offer_again_after_restore(reserve_restore_startup());
        if in_preflight {
            assert!(script().startup_reservations.is_empty());
        } else {
            assert_eq!(script().startup_reservations, [Some(next_sent)]);
            let _ = stop_initial_setup();
            host.sent = StartupSent::reserved(next_sent);
            assert!(matches!(
                prepare_setup(&mut host).unwrap(),
                PreparedSetup::Unavailable
            ));
        }
        assert!(stop_covers(next_sent));
        assert_eq!(script().inline_startup_reservation, None);
        assert_eq!(
            (host.brought, host.binds, host.offers, host.saves),
            (0, 0, 0, 0)
        );
        assert_eq!(host.setup.revision().unwrap(), original_revision);
        assert!(host.setup.admission().unwrap().is_none());
        assert!(script().runs.is_empty());
    }
}

#[test]
fn a_stopped_restore_starts_no_recovery_when_its_status_is_unread() {
    use aterm_cli::macos_doctor::InitialSetupObserved;
    use std::cell::Cell;

    thread_local! {
        static UNREAD_RESTORE_STATUS: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
    }

    let _globals = globals();
    let _io = TestIo::install();
    assert_eq!(STARTUP_RUNS.load(Ordering::Acquire), 0);
    drop(StartupRunning::enter());
    change(|script| {
        script.restored_setup_needs_startup = Err("the restored setup records could not be read");
        script.startup_spawns.push_back(false);
    });
    SETUP_IO.write().unwrap().snapshot = || {
        UNREAD_RESTORE_STATUS.with(|reads| {
            // Only the Restore's own publications are unread. Background preference
            // rereads keep the ordinary scratch snapshot, even if they finish later.
            let Some((mut held, mut released)) = reads.get() else {
                return TestIo::snapshot();
            };
            if INITIAL_SETUP_BUSY.load(Ordering::Acquire) {
                held += 1;
            } else {
                released += 1;
            }
            reads.set(Some((held, released)));
            Err(InitialSetupSnapshotError::Read(
                "unread after Restore".into(),
            ))
        })
    };
    let busy = take_initial_setup_slot().unwrap();
    let next_sent = SETUP_SENT.load(Ordering::Acquire) + 1;
    run_setup_restore(busy, |_, holding| {
        assert!(
            holding(),
            "the Restore holds the records and its operation begins"
        );
        assert!(!stop_covers(next_sent));
        let _ = stop_initial_setup();
        assert!(
            stop_covers(next_sent),
            "Stop covers the Restore's already-reserved startup"
        );
        UNREAD_RESTORE_STATUS.set(Some((0, 0)));
        InitialSetupObserved {
            result: Err("the Restore could not read its saved settings".into()),
            observation: None,
        }
    });
    let (held, released) = UNREAD_RESTORE_STATUS.take().unwrap();
    assert!(
        held > 0,
        "the unread Restore result was published with the slot held"
    );
    assert!(
        released > 0,
        "dropping the slot also published unread status"
    );
    assert!(!INITIAL_SETUP_BUSY.load(Ordering::Acquire));
    assert_eq!(STARTUP_RUNS.load(Ordering::Acquire), 0);
    let script = script();
    assert_eq!(script.restore_preflight_reads, 0);
    assert!(script.startup_reservations.is_empty());
    assert_eq!(script.inline_startup_reservation, None);
    assert!(script.runs.is_empty());
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
        assert!(!resumed.finished && resumed.run_allowed);
        clear_setup_run(revision);
    }
}
