// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_cli::macos_doctor::{
    InitialSetupAdmissionFault, InitialSetupAdmissionStopTest,
    set_aside_unreadable_setup_at_observing, setup_is_newer,
};
use std::sync::atomic::Ordering;

struct Startup<'a> {
    root: &'a Path,
    setup: InitialSetupAdmissionStopTest<'a>,
    sent: StartupSent,
    reads: usize,
    backoffs: usize,
    failures: Vec<String>,
    rejections: Vec<String>,
}

impl Startup<'_> {
    fn read_started(&mut self) {
        self.reads += 1;
        assert_eq!(
            self.reads, 1,
            "a newer record is a terminal refusal, not another startup attempt"
        );
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

    fn set_aside(&mut self) -> Result<(), String> {
        let root = self.root;
        self.sent
            .observe_repair_with(|published| {
                set_aside_unreadable_setup_at_observing(root, false, published)
            })
            .map(|_| ())
    }

    fn bring_forward(&mut self) -> Result<bool, String> {
        self.read_started();
        let result = self.sent.observe_repair_with(|published| {
            self.setup.bring_forward_progress_error(false, published)
        });
        assert!(result.as_ref().is_err_and(|why| setup_is_newer(why)));
        result.map(|outcome| !matches!(outcome, aterm_cli::macos_doctor::BroughtForward::Busy))
    }

    fn bind_authorization(&mut self) -> Result<u64, String> {
        panic!("a newer setup is refused before any review binds")
    }

    fn revision(&mut self) -> Result<u64, String> {
        panic!("a newer setup is never adopted")
    }

    fn claim(&mut self) -> Result<Option<Self::Claim>, String> {
        panic!("a newer setup is never claimed")
    }

    fn finished(&mut self) -> Result<bool, String> {
        panic!("a newer setup is refused before a completion read")
    }

    fn admission(&mut self) -> Result<Option<SetupAdmission>, String> {
        panic!("a newer setup is never resumed")
    }

    fn initial(&mut self) -> Result<Plan, String> {
        panic!("a newer setup is never offered")
    }

    fn offer(&mut self, _: Plan) -> aterm_gui::WorkstationSetupDecision {
        panic!("a newer setup is never offered")
    }

    fn authorize(&mut self, _: &Plan) -> Result<Option<u64>, String> {
        panic!("a newer setup is never rewritten")
    }

    fn authorization_pending(&self) -> bool {
        false
    }

    fn wait_holding_claim(&mut self) -> bool {
        panic!("a newer setup is never admitted")
    }

    fn backoff(&mut self) {
        self.backoffs += 1;
        assert_eq!(
            self.backoffs, 1,
            "the next attempt detects an invalid retry"
        );
    }

    fn report_failure(&mut self, why: &str) {
        self.failures.push(why.to_owned());
    }

    fn report_rejection(&mut self, why: &str) {
        self.rejections.push(why.to_owned());
    }
}

fn future_field(root: &Path, name: &str, field: &str) {
    let path = root.join(name);
    let mut value: aterm_json::Value =
        aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert(field.into(), aterm_json::Value::from(2));
    std::fs::write(path, aterm_json::to_vec(&value).unwrap()).unwrap();
}

fn saved(root: &Path) -> [Option<Vec<u8>>; 4] {
    [
        "automatic-plan.json",
        "initial-setup.json",
        "first-run.json",
        "run-revision.json",
    ]
    .map(|name| std::fs::read(root.join(name)).ok())
}

fn exercise(counter_after_checkpoint: bool) {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    let mut sent = StartupSent::default();
    sent.send();
    let mut setup = if counter_after_checkpoint {
        InitialSetupAdmissionStopTest::restored_before_run(root.path()).unwrap()
    } else {
        InitialSetupAdmissionStopTest::startup(root.path()).unwrap()
    };
    let checkpoint = if counter_after_checkpoint {
        assert!(
            sent.observe_repair_with(|published| {
                setup.bring_forward_progress_error(true, published)
            })
            .is_err()
        );
        let checkpoint = setup.revision().unwrap();
        future_field(root.path(), "run-revision.json", "version");
        Some(checkpoint)
    } else {
        setup
            .persist(InitialSetupAdmissionFault::None)
            .unwrap()
            .unwrap();
        future_field(root.path(), "initial-setup.json", "format");
        None
    };
    let before = saved(root.path());
    let mut host = Startup {
        root: root.path(),
        setup,
        sent,
        reads: 0,
        backoffs: 0,
        failures: Vec::new(),
        rejections: Vec::new(),
    };
    assert!(prepare_setup(&mut host).is_err_and(|why| setup_is_newer(&why)));
    assert_eq!((host.reads, host.backoffs), (1, 0));
    assert!(host.failures.is_empty());
    assert_eq!(host.rejections.len(), 1);
    assert!(setup_is_newer(&host.rejections[0]));
    assert_eq!(saved(root.path()), before, "newer records are left intact");
    let saves = stop_saves();
    assert!(saves.under_way.iter().any(|debt| debt.unconfirmed));
    if let Some(checkpoint) = checkpoint {
        assert!(
            saves
                .under_way
                .iter()
                .any(|debt| debt.target == Some(checkpoint) && debt.owned),
            "a later version refusal does not erase an earlier exact checkpoint"
        );
    }
    drop(saves);
    assert_eq!(STARTUP_CLAIMS.load(Ordering::Acquire), 0);
    assert!(script().runs.is_empty());
}

#[test]
fn startup_newer_a_future_progress_format_refuses_without_another_attempt() {
    exercise(false);
}

#[test]
fn startup_newer_a_future_counter_keeps_earlier_checkpoint_evidence_without_retrying() {
    exercise(true);
}

#[test]
fn startup_newer_a_general_retry_step_refuses_an_actual_future_record() {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    let mut setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
    setup
        .persist(InitialSetupAdmissionFault::None)
        .unwrap()
        .unwrap();
    future_field(root.path(), "initial-setup.json", "format");
    let before = saved(root.path());
    let mut host = Startup {
        root: root.path(),
        setup,
        sent: StartupSent::default(),
        reads: 0,
        backoffs: 0,
        failures: Vec::new(),
        rejections: Vec::new(),
    };
    host.send();
    let result = retry_setup_step(&mut host, |host| {
        host.read_started();
        host.setup
            .snapshot()
            .map(|_| ())
            .map_err(|why| why.to_string())
    });
    assert!(result.is_err_and(|why| setup_is_newer(&why)));
    assert_eq!((host.reads, host.backoffs), (1, 0));
    assert!(host.failures.is_empty());
    assert_eq!(host.rejections.len(), 1);
    assert!(setup_is_newer(&host.rejections[0]));
    assert_eq!(saved(root.path()), before);
    assert!(script().runs.is_empty());
}
