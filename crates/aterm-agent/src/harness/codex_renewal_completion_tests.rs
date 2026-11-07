// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::native_act::{NativePlan, Request};
use crate::harness::native_observe::{ObservationBinding, Registration, SavedObservation};

#[path = "codex_renewal_root_tests.rs"]
mod root;

struct Fixture {
    _tmp: aterm_tempfile::TempDir,
    env: crate::harness::cli::Env,
    opts: upgrade_drive::Opts,
    root: Receipt,
    prepare: Prepare,
    state: upgrade_drive::St,
}

impl Fixture {
    fn new(rollback: bool, wrapper: bool, child: bool) -> Self {
        let (tmp, env, mut root, mut prepare) = super::super::tests::fixture();
        let NativePlan::Restart {
            shell,
            launcher,
            forward,
            rollback: back,
            socket,
            ..
        } = &mut prepare.plan
        else {
            unreachable!()
        };
        launcher.inode = 10;
        *socket = tmp
            .path()
            .join("control.sock")
            .to_string_lossy()
            .into_owned();
        forward.program = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        forward.cwd = None;
        let mut back_command = forward.as_ref().clone();
        back_command.image.inode += 1;
        if wrapper {
            for command in [forward.as_mut(), &mut back_command] {
                command.settles = Some(command.image.clone());
                command.image.inode += 20;
            }
        }
        let mut vendor = shell.clone();
        vendor.pid = 40;
        vendor.birth = 41;
        vendor.parent = shell.pid;
        vendor.group = vendor.pid;
        vendor.image = if rollback {
            back_command.settled()
        } else {
            forward.settled()
        }
        .clone();
        let mut attached = vendor.clone();
        attached.image = launcher.clone();
        let observed_shell = shell.clone();
        *back = Some(Box::new(back_command));
        root.native_prepare = Request::prepare(prepare.clone()).encode().unwrap();
        renewal::with_receipt_writer(&env, |writer| {
            renewal::write_receipt(&env, &root)?;
            if child {
                let parent = prepare.id.clone();
                prepare.id = "12".repeat(16);
                prepare.revision += 1;
                prepare.deadline_us += 120_000_000;
                let attempt = writer.propose(&root, &parent, &prepare)?;
                root = writer.publish(&root, &attempt)?;
            }
            root.candidate = Some(renewal::Candidate::of(&attached, !rollback));
            renewal::write_receipt(&env, &root)
        })
        .unwrap()
        .unwrap();
        let observation = SavedObservation {
            registration: Registration {
                id: "56".repeat(16),
                host_epoch: prepare.host_epoch.clone(),
                vendor,
                shell: observed_shell,
            },
            binding: ObservationBinding {
                id: "56".repeat(16),
                host_epoch: prepare.host_epoch.clone(),
                capability: "78".repeat(32),
            },
        };
        assert!(observation.encode().is_some());
        let opts = upgrade_drive::Opts {
            home: env.home.clone().unwrap(),
            state: env.state.clone(),
            account_roster: None,
            sock: None,
            token: None,
            only_sid: Some(root.sid.clone()),
            dry_run: false,
            human_grace_s: 0,
            hand_back: false,
            background: false,
            aterm_state: None,
            ask: false,
            several_logins: false,
            usage_credits: false,
            usage_tap: None,
            instances: upgrade_drive::Instances::Listed(vec![]),
            agents_off: vec![],
            land: false,
            timings: crate::harness::Timings::FAST,
            tell: upgrade_drive::Tell::Off,
            press: None,
        };
        let state = upgrade_drive::St {
            tab: root.sid.clone(),
            native_coordination: true,
            native_completed_prepare: Some(Box::new(prepare.clone())),
            native_observation: Some(observation),
            native_rollback: rollback,
            phase: crate::harness::upgrade::Phase::Relaunched { at_s: 1 },
            ..upgrade_drive::St::default()
        };
        let fixture = Self {
            _tmp: tmp,
            env,
            opts,
            root,
            prepare,
            state,
        };
        fixture.save_native();
        fixture
    }

    fn key(&self) -> String {
        format!("renewal-{}", self.prepare.id)
    }
    fn native_path(&self) -> std::path::PathBuf {
        self.opts
            .state
            .join("upgrade")
            .join(format!("{}.json", self.key()))
    }
    fn receipt_bytes(&self) -> Vec<u8> {
        std::fs::read(renewal::receipt_path(&self.env, &self.root.id).unwrap()).unwrap()
    }
    fn save_native(&self) {
        upgrade_drive::save_checked(&self.opts, &self.key(), &self.state).unwrap();
    }
    fn write_root(&self) {
        renewal::with_receipt_writer(&self.env, |_| renewal::write_receipt(&self.env, &self.root))
            .unwrap()
            .unwrap();
    }
    fn recover(&self) -> Result<CompletionProgress, &'static str> {
        renewal::with_receipt_writer(&self.env, |w| {
            w.recover_completion(&self.root, &self.prepare, &self.opts)
        })
        .map(|result| result.expect("fixture writer acquired"))
    }
    fn noexec(&mut self) {
        self.root.candidate = None;
        self.write_root();
        self.state.native_completed_prepare = None;
        self.state.native_observation = None;
        self.state.native_rollback = false;
        self.state.native_noexec_prepare = Some(Box::new(self.prepare.clone()));
        self.state.phase = crate::harness::upgrade::Phase::Exiting { at_s: 1 };
        self.save_native();
    }
}

#[test]
fn completion_publishes_exact_root_and_child_direct_and_wrapped_outcomes() {
    for rollback in [false, true] {
        for wrapper in [false, true] {
            for child in [false, true] {
                let mut f = Fixture::new(rollback, wrapper, child);
                let CompletionProgress::Published(done) = f.recover().unwrap() else {
                    panic!("not published")
                };
                let mut expected = f.root.clone();
                let vendor = &f
                    .state
                    .native_observation
                    .as_ref()
                    .unwrap()
                    .registration
                    .vendor;
                expected.phase = if rollback {
                    Phase::Retired
                } else {
                    Phase::Committed
                };
                expected.successor_pid = Some(vendor.pid);
                expected.successor_birth = Some(vendor.birth);
                assert_eq!(*done, expected);
                assert_eq!(
                    renewal::read_receipt(&f.env, &f.root.id),
                    Some(expected.clone())
                );
                assert!(
                    f.recover().is_err(),
                    "old snapshot cannot overwrite publication"
                );
                f.root = expected;
                let before = f.receipt_bytes();
                assert_eq!(
                    f.recover(),
                    Ok(CompletionProgress::Published(Box::new(f.root.clone())))
                );
                assert_eq!(
                    f.receipt_bytes(),
                    before,
                    "retry acknowledges without rewriting"
                );
                if !rollback {
                    f.root.phase = Phase::Consumed;
                    f.root.accepted_turn = Some(f.root.intent.wall.clone());
                    f.write_root();
                    assert_eq!(
                        f.recover(),
                        Ok(CompletionProgress::Published(Box::new(f.root.clone())))
                    );
                }
            }
        }
    }
}

#[test]
fn completion_noexec_retires_only_root_and_preserves_child_intent() {
    for child in [false, true] {
        let mut f = Fixture::new(false, false, child);
        f.noexec();
        let before = f.receipt_bytes();
        if child {
            assert_eq!(f.recover(), Ok(CompletionProgress::ChildNotStarted));
            assert_eq!(f.receipt_bytes(), before);
        } else {
            let CompletionProgress::Published(done) = f.recover().unwrap() else {
                panic!("not published")
            };
            let mut expected = f.root.clone();
            expected.phase = Phase::Retired;
            expected.launch_attempted = false;
            assert_eq!(*done, expected);
            f.root = expected;
            assert_eq!(
                f.recover(),
                Ok(CompletionProgress::Published(Box::new(f.root.clone())))
            );
        }
    }
}

#[test]
fn completion_poll_never_writes_unobserved_or_unrelated_records() {
    for fault in 0..5 {
        let mut f = Fixture::new(false, false, false);
        match fault {
            0 => f.state.native_observation = None,
            1 => f.state.tab = "different-session".into(),
            2 => {
                f.state
                    .native_completed_prepare
                    .as_mut()
                    .unwrap()
                    .deadline_us += 1
            }
            3 => {
                f.state
                    .native_observation
                    .as_mut()
                    .unwrap()
                    .registration
                    .vendor
                    .birth += 1
            }
            4 => f.root.candidate.as_mut().unwrap().parent += 1,
            _ => unreachable!(),
        }
        f.save_native();
        f.write_root();
        let native = std::fs::read(f.native_path()).unwrap();
        let receipt = f.receipt_bytes();
        if fault <= 2 {
            assert_eq!(f.recover(), Ok(CompletionProgress::Pending));
        } else {
            assert!(f.recover().is_err());
        }
        assert_eq!(std::fs::read(f.native_path()).unwrap(), native);
        assert_eq!(f.receipt_bytes(), receipt);
    }
}

#[test]
fn completion_distinguishes_absent_active_and_foreign_completion() {
    let mut f = Fixture::new(false, false, true);
    std::fs::remove_file(f.native_path()).unwrap();
    assert_eq!(f.recover(), Ok(CompletionProgress::Absent));
    f.state.native_completed_prepare.as_mut().unwrap().id = f.root.id.clone();
    f.state.native_prepare = Request::prepare(f.prepare.clone()).encode().unwrap();
    f.state.phase = crate::harness::upgrade::Phase::Exiting { at_s: 1 };
    f.save_native();
    let native = std::fs::read(f.native_path()).unwrap();
    assert_eq!(f.recover(), Ok(CompletionProgress::Pending));
    assert_eq!(std::fs::read(f.native_path()).unwrap(), native);
    f.state.native_completed_prepare = None;
    f.state.native_observation = None;
    f.save_native();
    assert_eq!(f.recover(), Ok(CompletionProgress::Active));
}

#[test]
fn completion_native_lock_refusal_precedes_receipt_publication() {
    let f = Fixture::new(false, false, false);
    let native = std::fs::read(f.native_path()).unwrap();
    let receipt = f.receipt_bytes();
    let lock = aterm_uds::file_lock::LockFile::take(
        &f.opts
            .state
            .join("upgrade")
            .join(format!("{}.lock", f.key())),
        std::time::Instant::now(),
        "completion publication test",
    )
    .unwrap();
    aterm_uds::file_lock::with_lock_wait_observer(
        |_| panic!("completion recovery waited with the account writer held"),
        || assert!(f.recover().is_err()),
    );
    assert_eq!(std::fs::read(f.native_path()).unwrap(), native);
    assert_eq!(f.receipt_bytes(), receipt);
    drop(lock);
    assert!(matches!(f.recover(), Ok(CompletionProgress::Published(_))));
}

#[test]
fn completion_cannot_reopen_a_same_id_terminal_contradiction_as_active() {
    let mut f = Fixture::new(false, false, false);
    f.state.native_prepare = Request::prepare(f.prepare.clone()).encode().unwrap();
    f.state
        .native_completed_prepare
        .as_mut()
        .unwrap()
        .deadline_us += 1;
    f.state.phase = crate::harness::upgrade::Phase::Exiting { at_s: 1 };
    f.save_native();
    let native = std::fs::read(f.native_path()).unwrap();
    let receipt = f.receipt_bytes();
    assert_eq!(f.recover(), Ok(CompletionProgress::Pending));
    assert_eq!(std::fs::read(f.native_path()).unwrap(), native);
    assert_eq!(f.receipt_bytes(), receipt);
}

#[test]
fn completion_cannot_publish_an_old_roots_result_over_a_current_child() {
    let f = Fixture::new(false, false, true);
    let Request::Prepare(root_prepare) = Request::parse(&f.root.native_prepare).unwrap() else {
        unreachable!()
    };
    let mut root_state = f.state.clone();
    root_state.native_record = Default::default();
    root_state.native_completed_prepare = Some(root_prepare.clone());
    let key = format!("renewal-{}", root_prepare.id);
    upgrade_drive::save_checked(&f.opts, &key, &root_state).unwrap();
    let root_path = f.opts.state.join("upgrade").join(format!("{key}.json"));
    let root_native = std::fs::read(&root_path).unwrap();
    let child_native = std::fs::read(f.native_path()).unwrap();
    let receipt = f.receipt_bytes();
    assert!(
        renewal::with_receipt_writer(&f.env, |w| {
            w.recover_completion(&f.root, &root_prepare, &f.opts)
        })
        .is_err()
    );
    assert_eq!(std::fs::read(root_path).unwrap(), root_native);
    assert_eq!(std::fs::read(f.native_path()).unwrap(), child_native);
    assert_eq!(f.receipt_bytes(), receipt);
}

#[test]
fn completion_rejects_a_non_restart_request_even_when_native_record_is_absent() {
    let mut f = Fixture::new(false, false, false);
    let NativePlan::Restart { process, .. } = &f.prepare.plan else {
        unreachable!()
    };
    f.prepare.plan = NativePlan::OwnImage {
        child: process.clone(),
        target: process.image.clone(),
        version: "1.2.3".into(),
    };
    f.prepare.duty = crate::harness::contract::Scope::Upgrade;
    f.prepare.expected_human_seq = None;
    f.root.native_prepare = Request::prepare(f.prepare.clone()).encode().unwrap();
    f.write_root();
    std::fs::remove_file(f.native_path()).unwrap();
    let receipt = f.receipt_bytes();
    assert_eq!(f.recover(), Err("completion-changed"));
    assert_eq!(f.receipt_bytes(), receipt);
}

#[test]
fn completion_receipt_failure_requires_fresh_acknowledged_retry() {
    // The first directory barrier precedes rename; the next follows rename.
    for real_barriers in [0, 1] {
        let mut f = Fixture::new(false, false, false);
        let before = f.receipt_bytes();
        let result = renewal::with_receipt_writer(&f.env, |w| {
            w.recover_completion_using(&f.root, &f.prepare, &f.opts, |next| {
                let _failure = aterm_durable::seam::FailingDirs::after(
                    real_barriers,
                    aterm_durable::seam::EIO,
                );
                renewal::write_receipt(&f.env, next)
            })
        });
        assert!(result.is_err());
        let current = renewal::read_receipt(&f.env, &f.root.id).unwrap();
        if real_barriers == 0 {
            assert_eq!(current, f.root);
            assert_eq!(f.receipt_bytes(), before);
        } else {
            assert_eq!(current.phase, Phase::Committed);
            assert!(f.recover().is_err());
        }
        f.root = current;
        assert!(matches!(f.recover(), Ok(CompletionProgress::Published(_))));
    }
}

#[test]
fn completion_rejects_stale_revoked_and_wrong_store_without_native_ack() {
    for fault in 0..5 {
        let mut f = Fixture::new(false, false, false);
        let native = std::fs::read(f.native_path()).unwrap();
        match fault {
            0 => {
                let mut newer = f.root.clone();
                newer.generation += 1;
                renewal::with_receipt_writer(&f.env, |_| renewal::write_receipt(&f.env, &newer))
                    .unwrap()
                    .unwrap();
            }
            1 => {
                renewal::with_receipt_writer(&f.env, |w| {
                    w.cancel(&f.root, renewal::CancellationKind::OwnerStop, 5, 5)
                })
                .unwrap()
                .unwrap();
            }
            2 => f.prepare.deadline_us += 1,
            3 => f.opts.state = f.opts.state.join("different"),
            4 => f.opts.dry_run = true,
            _ => unreachable!(),
        }
        let receipt = f.receipt_bytes();
        assert!(f.recover().is_err());
        assert_eq!(
            std::fs::read(
                f.env
                    .state
                    .join("upgrade")
                    .join(format!("{}.json", f.key()))
            )
            .unwrap(),
            native
        );
        assert_eq!(f.receipt_bytes(), receipt);
    }
}

fn publication_case(state: &aterm_spec::interp::State) -> (i64, i64, i64) {
    let case = state["case"];
    let failure = state["failure"];
    let mut f = Fixture::new(case == 1, false, case == 3);
    match case {
        0 | 1 => {}
        2 | 3 => f.noexec(),
        4 => {
            f.state.native_observation = None;
            f.save_native();
        }
        5 => std::fs::remove_file(f.native_path()).unwrap(),
        6 => {
            let mut newer = f.root.clone();
            newer.generation += 1;
            renewal::with_receipt_writer(&f.env, |_| renewal::write_receipt(&f.env, &newer))
                .unwrap()
                .unwrap();
        }
        7 => {
            f.root.candidate.as_mut().unwrap().parent += 1;
            f.write_root();
        }
        8 => {
            f.state.tab = "different-session".into();
            f.save_native();
        }
        _ => unreachable!(),
    }
    let native = std::fs::read(f.native_path()).ok();
    let receipt = f.receipt_bytes();
    let _lock = (failure == 1).then(|| {
        aterm_uds::file_lock::LockFile::take(
            &f.opts
                .state
                .join("upgrade")
                .join(format!("{}.lock", f.key())),
            std::time::Instant::now(),
            "publication conformance",
        )
        .unwrap()
    });
    let result = renewal::with_receipt_writer(&f.env, |w| {
        w.recover_completion_using(&f.root, &f.prepare, &f.opts, |next| {
            let _failure = (2 <= failure).then(|| {
                aterm_durable::seam::FailingDirs::after(
                    u32::from(failure == 3),
                    aterm_durable::seam::EIO,
                )
            });
            renewal::write_receipt(&f.env, next)
        })
    });
    let ack = matches!(
        result,
        Ok(Some(
            CompletionProgress::Published(_) | CompletionProgress::ChildNotStarted
        ))
    );
    (
        i64::from(std::fs::read(f.native_path()).ok() != native),
        i64::from(f.receipt_bytes() != receipt),
        i64::from(ack),
    )
}

pub(crate) fn publication_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "NextCase" => {
            next.insert("case", state["case"] + 1);
        }
        "NextFailure" => {
            next.insert("failure", state["failure"] + 1);
        }
        "Recover" => {
            let (native, visible, acknowledged) = publication_case(state);
            next.insert("done", 1);
            next.insert("native_ack", native);
            next.insert("visible", visible);
            next.insert("acknowledged", acknowledged);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn completion_publication_refines_real_journals_and_detects_boundary_mutants() {
    let model = aterm_models_harness::renewal_completion_publication_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = publication_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} {action:?} -> {next:?}"
                );
            }
        }
    }
    for (case, failure, field, value, law) in [
        (6, 0, "acknowledged", 1, "ExactReceipt"),
        (0, 1, "visible", 1, "NativeBeforeReceipt"),
        (0, 3, "acknowledged", 1, "DurableReturn"),
        (3, 0, "visible", 1, "ChildPreserved"),
        (4, 0, "native_ack", 1, "SilentUnknown"),
        (0, 0, "acknowledged", 0, "Available"),
    ] {
        let mut state = model.init_state();
        state.insert("case", case);
        state.insert("failure", failure);
        let mut wrong = publication_transition(&state, "Recover");
        wrong.insert(field, value);
        assert!(!model.check_invariant(law, &wrong), "{law}");
        assert!(!model.successors("Recover", &state).contains(&wrong));
    }
}
