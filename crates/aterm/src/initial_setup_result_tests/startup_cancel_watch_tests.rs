// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_cli::macos_doctor::{
    InitialSetupAdmissionFault, InitialSetupAdmissionStopTest, ProvisionalSetupWatchTest,
};
use aterm_gui::WorkstationSetupDecision;
use aterm_types::macos_setup::Selection;
use std::cell::Cell;
use std::sync::atomic::Ordering;

struct Startup<'a> {
    root: &'a Path,
    setup: InitialSetupAdmissionStopTest<'a>,
    sent: StartupSent,
    answer: Option<WorkstationSetupDecision>,
    offers: usize,
    saves: usize,
}

impl<'a> Startup<'a> {
    fn new(root: &'a Path, answer: Option<WorkstationSetupDecision>) -> Self {
        Self {
            root,
            setup: InitialSetupAdmissionStopTest::startup(root).unwrap(),
            sent: StartupSent::default(),
            answer,
            offers: 0,
            saves: 0,
        }
    }
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

    fn bind_authorization(&mut self) -> Result<u64, String> {
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
        Ok(self.setup.admission()?.map(|admission| SetupAdmission {
            plan: admission.plan,
            revision: admission.revision,
            completed: admission.completed,
        }))
    }

    fn initial(&mut self) -> Result<Plan, String> {
        Ok(self.setup.plan())
    }

    fn offer(&mut self, plan: Plan) -> WorkstationSetupDecision {
        self.offers += 1;
        assert_eq!(self.offers, 1);
        assert_eq!(plan, self.setup.plan());
        self.answer
            .take()
            .expect("a confirmed run needs no new offer")
    }

    fn authorize(&mut self, plan: &Plan) -> Result<Option<u64>, String> {
        self.saves += 1;
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
        panic!("the scratch admission needs no retry")
    }

    fn backoff(&mut self) {
        panic!("the actual saved prefix is readable and the page answers once")
    }

    fn report_failure(&mut self, why: &str) {
        panic!("unexpected startup failure: {why}")
    }
}

struct LateSignal<'a> {
    root: &'a Path,
    setup: ProvisionalSetupWatchTest<'a>,
    looks: &'a Cell<usize>,
}

impl AlabWatch for LateSignal<'_> {
    fn widens(&mut self) -> Result<bool, String> {
        self.setup.widens()
    }

    fn wait(&mut self, _: Duration, _: Duration) {}

    fn heard(&mut self) -> Result<bool, String> {
        Ok(true)
    }

    fn look(&mut self) -> Result<Option<u64>, String> {
        self.looks.set(self.looks.get() + 1);
        self.setup.upgrade(None)
    }

    fn run(&mut self) {
        let mut host = Startup::new(self.root, None);
        let prepared = prepare_setup(&mut host).unwrap();
        assert!(matches!(prepared, PreparedSetup::Authorized { .. }));
        assert_eq!((host.offers, host.saves), (0, 0));
        run_prepared_setup(prepared, None, std::mem::take(&mut host.sent));
    }
}

fn maintain(root: &Path, looks: &Cell<usize>, how: WatchMaintenance) {
    let watching = SetupWatch::new();
    maintain_setup_watch_with(
        &watching,
        how,
        || ProvisionalSetupWatchTest::at(root).widens(),
        |_| panic!("the common startup uses its released worker for this watch"),
        |reservation| {
            assert_eq!(STARTUP_CLAIMS.load(Ordering::Acquire), 0);
            assert!(!INITIAL_SETUP_BUSY.load(Ordering::Acquire));
            let mut watch = LateSignal {
                root,
                setup: ProvisionalSetupWatchTest::at(root),
                looks,
            };
            run_reserved_alab_watch(reservation, || widen_on_alab_signal(&mut watch));
        },
    );
}

fn records(root: &Path) -> [Vec<u8>; 4] {
    [
        "automatic-plan.json",
        "initial-setup.json",
        "first-run.json",
        "run-revision.json",
    ]
    .map(|name| std::fs::read(root.join(name)).unwrap())
}

fn staged(root: &Path) -> Plan {
    let mut first = InitialSetupAdmissionStopTest::startup(root).unwrap();
    assert!(
        first
            .persist(InitialSetupAdmissionFault::BeforeFinal)
            .is_err()
    );
    let prefix = first.snapshot().unwrap();
    assert!(!prefix.finished && !prefix.permissions_allowed);
    assert!(first.admission().unwrap().is_none());
    assert!(ProvisionalSetupWatchTest::at(root).widens().unwrap());
    first.plan()
}

/// An older aterm's Cancel of its first-launch review of a staged offer, read from its records:
/// the startup offers nothing and runs nothing, and a late ALab signal widens nothing; a later
/// explicit choice is a new authority, which runs.
#[test]
fn startup_cancel_watch_an_older_cancel_of_a_staged_offer_cannot_run_on_a_late_signal() {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    *CLAIM_ROOT.lock().unwrap() = Some(root.path().to_owned());
    let plan = staged(root.path());
    InitialSetupAdmissionStopTest::startup(root.path())
        .unwrap()
        .older_cancel()
        .unwrap();
    let cancelled = records(root.path());
    // An offer would panic, since nobody scripted an answer for it.
    let mut host = Startup::new(root.path(), None);
    let prepared = prepare_setup(&mut host).unwrap();
    assert!(matches!(prepared, PreparedSetup::Finished));
    assert_eq!((host.offers, host.saves), (0, 0));
    let looks = Cell::new(0);
    run_prepared_startup_with(prepared, None, std::mem::take(&mut host.sent), |how| {
        maintain(root.path(), &looks, how);
    });
    assert!(
        script().runs.is_empty(),
        "a late ALab signal must not dispatch the setup whose review was cancelled"
    );
    assert_eq!(looks.get(), 0);
    assert_eq!(records(root.path()), cancelled);
    assert!(
        !ProvisionalSetupWatchTest::at(root.path()).widens().unwrap(),
        "nothing widens a cancelled setup"
    );

    // A later explicit choice is a new authority, not this cancelled offer.
    let chosen = InitialSetupAdmissionStopTest::restart(root.path()).unwrap();
    let revision = chosen.choose().unwrap();
    let claim = StartupClaim::claim().unwrap().unwrap();
    let mut sent = StartupSent::default();
    sent.send();
    sent.answered();
    run_prepared_startup_with(
        PreparedSetup::Authorized { claim, revision },
        None,
        sent,
        |how| maintain(root.path(), &looks, how),
    );
    let resumed = chosen.snapshot().unwrap();
    assert_eq!(script().runs, [revision]);
    assert_eq!(resumed.revision, revision);
    assert_eq!(resumed.plan, plan);
    assert!(!resumed.provisional, "the later choice is the person's");
    assert_eq!(
        looks.get(),
        0,
        "fleet membership preserves the later choice"
    );
    clear_setup_run(revision);
}

/// An older aterm's Cancel holds until the person asks again: the launch that reads it offers
/// nothing and admits nothing, aterm's permission steps stay held, and every group reads Leave
/// as is (design §3.3).
#[test]
fn startup_cancel_watch_an_older_cancel_is_not_offered_again() {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    *CLAIM_ROOT.lock().unwrap() = Some(root.path().to_owned());
    InitialSetupAdmissionStopTest::startup(root.path())
        .unwrap()
        .older_cancel()
        .unwrap();
    // An offer would panic, since nobody scripted an answer for it.
    let mut next = Startup::new(root.path(), None);
    let prepared = prepare_setup(&mut next).unwrap();
    let finished = matches!(prepared, PreparedSetup::Finished);
    drop(prepared);
    let snapshot = next.setup.snapshot().unwrap();
    assert_eq!(
        (next.offers, finished, snapshot.permissions_allowed),
        (0, true, false),
        "the Cancel holds past its launch: no offer, no admission"
    );
    assert!(snapshot.cancelled && snapshot.plan.selection.is_empty());
}

/// After an older aterm's Cancel, Set up now applies the plan its review showed, and a group
/// press sets up that group alone (design §3.3).
#[test]
fn startup_cancel_watch_set_up_now_after_an_older_cancel_applies_the_plan_its_review_showed() {
    let _globals = globals();
    let _io = TestIo::install();
    for group in [None, Some(aterm_types::macos_setup::Setting::CapsLock)] {
        let root = aterm_tempfile::tempdir().unwrap();
        *CLAIM_ROOT.lock().unwrap() = Some(root.path().to_owned());
        let first = Startup::new(root.path(), None);
        first.setup.older_cancel().unwrap();
        let reviewed = first.setup.plan();
        let admitted = match group {
            None => first.setup.set_up_now(),
            Some(setting) => first.setup.choose_group(&[setting]),
        };
        assert!(admitted.unwrap().is_some(), "{group:?}: admitted");
        let saved = first.setup.saved_plan().unwrap().unwrap();
        match group {
            None => assert_eq!(saved, reviewed, "Set up now: the plan the review showed"),
            Some(setting) => {
                let mut alone = Selection::none();
                alone.set(setting, true);
                assert_eq!(saved.selection, alone, "a group press: that group alone");
            }
        }
        let snapshot = first.setup.snapshot().unwrap();
        assert!(!snapshot.cancelled, "{group:?}: the Cancel gave way");
        assert_eq!(
            snapshot.permissions_allowed,
            saved
                .selection
                .contains(aterm_types::macos_setup::Setting::Permissions)
        );
    }
}

#[test]
fn startup_cancel_watch_an_accepted_provisional_offer_still_widens_once() {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    *CLAIM_ROOT.lock().unwrap() = Some(root.path().to_owned());
    let plan = staged(root.path());
    let mut host = Startup::new(root.path(), Some(WorkstationSetupDecision::Begin(plan)));
    let prepared = prepare_setup(&mut host).unwrap();
    let PreparedSetup::Authorized { revision, .. } = &prepared else {
        panic!("the accepted offer must admit its exact run")
    };
    let first = *revision;
    assert_eq!((host.offers, host.saves), (1, 1));
    let looks = Cell::new(0);
    run_prepared_startup_with(prepared, None, std::mem::take(&mut host.sent), |how| {
        maintain(root.path(), &looks, how);
    });
    let snapshot = host.setup.snapshot().unwrap();
    assert_eq!(snapshot.plan.selection, Selection::all());
    assert_eq!(script().runs, [first, snapshot.revision]);
    assert!(snapshot.revision > first && snapshot.run_allowed);
    assert_eq!(looks.get(), 1);
    clear_setup_run(snapshot.revision);
}
