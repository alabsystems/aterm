// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The actual private constructor, selected with token=Some, against a bounded endpoint.
//! These controls exercise persistence and serialized requests; GUI tests prove admission.

use super::super::relaunch::PlannedCommand;
use super::super::upgrade::{Agent, Dialect, Phase};
use super::super::upgrade_drive::{HarnessToken, Instances, Tell};
use super::*;
use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

#[path = "native_replay_model_tests.rs"]
mod replay_model;

#[path = "native_restart_legacy_tests.rs"]
mod legacy;

#[path = "native_restart_pairing_tests.rs"]
mod pairing;

#[path = "native_completion_recovery_tests.rs"]
mod completion;

#[path = "native_noexec_recovery_tests.rs"]
mod noexec;

const SID: &str = "s-native-transport";

#[test]
fn retired_unissued_recipe_survives_disk_reload_without_any_further_dispatch() {
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = opts(world.path());
    let command = planned();
    let (mut st, _) = state(&command);
    let (mut client, worker) = endpoint(|line| {
        common(line).or_else(|| match request(line) {
            Request::Prepare(_) => {
                Some("OK native-act state=retired reason=no-exec-grant\n".into())
            }
            other => panic!("unissued retirement never dispatches {other:?}"),
        })
    });
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Ok(Recovery::Held)
    );
    let mut kept = st.clone();
    let mut loaded = upgrade_drive::load(&opts, "conversation").unwrap();
    // The storage layer stamps its own progress clock. It is deliberately not
    // the command/debt clock, all of whose fields must still compare exactly.
    assert!(loaded.progress_at > 0 && !loaded.progress_key.is_empty());
    kept.progress_at = loaded.progress_at;
    kept.progress_key.clone_from(&loaded.progress_key);
    assert_eq!(
        loaded, kept,
        "the exact recipe, debt age and retirement hold persist"
    );
    assert!(loaded.native_retired_unissued);
    let Request::Prepare(original) = Request::parse(&loaded.native_prepare).unwrap() else {
        panic!("retained preparation")
    };
    assert!(original.deadline_us > 0);
    assert_eq!(
        recover(&opts, "conversation", &mut client, SID, &mut loaded),
        Ok(Recovery::Held)
    );
    assert_eq!(
        abort(&opts, "conversation", &mut client, SID, &mut loaded),
        None
    );
    assert!(!complete(
        &opts,
        "conversation",
        &mut client,
        SID,
        &mut loaded
    ));
    assert!(!retire(
        &opts,
        "conversation",
        &mut client,
        SID,
        &mut loaded
    ));
    assert!(!launch_unissued(&mut client, SID, &mut loaded).unwrap());
    assert!(!resume_observed(&mut client, SID, &mut loaded).unwrap());
    assert_eq!(loaded, kept);
    // Renewal shares this client. Its direct connection driver also receives
    // retained bindings, and must return before attempting Signal or Resume.
    let mut renewal = loaded.clone();
    renewal.native = Some(Binding {
        id: original.id.clone(),
        capability: binding().capability,
    });
    let (renewal_client, renewal_worker) =
        endpoint(|_| panic!("held renewal dispatched a request"));
    assert!(
        super::super::codex_renewal::drive_connection(
            &opts,
            SID,
            &original,
            renewal.clone(),
            renewal_client,
            true
        )
        .is_none()
    );
    assert!(renewal_worker.join().unwrap().is_empty());
    let saved = upgrade_drive::load(&opts, &format!("renewal-{}", original.id)).unwrap();
    renewal.progress_at = saved.progress_at;
    renewal.progress_key.clone_from(&saved.progress_key);
    assert_eq!(saved, renewal);
    drop(client);
    let seen = worker.join().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|line| matches!(actual_request(line), Some(Request::Prepare(_))))
            .count(),
        1
    );
}

#[test]
fn retirement_at_precommit_is_retained_without_sending_commit() {
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = opts(world.path());
    let command = planned();
    let (mut st, _) = state(&command);
    let (mut client, worker) = endpoint(|line| {
        common(line).or_else(|| match request(line) {
            Request::Prepare(plan) => Some(format!(
                "OK native-act state=admitted id={} cap={}\n",
                plan.id,
                binding().capability
            )),
            Request::Query(_) => Some("OK native-act state=unknown\n".into()),
            Request::Precommit(..) => {
                Some("OK native-act state=retired reason=no-exec-grant\n".into())
            }
            other => panic!("retirement must precede any further request: {other:?}"),
        })
    });
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Ok(Recovery::Admitted)
    );
    let original = st.clone();
    assert!(!complete(&opts, "conversation", &mut client, SID, &mut st));
    assert!(st.native_retired_unissued);
    assert_eq!(st.native_prepare, original.native_prepare);
    assert_eq!(st.native, original.native);
    assert_eq!(st.phase, original.phase);
    upgrade_drive::save_checked(&opts, "conversation", &st).unwrap();
    let mut loaded = upgrade_drive::load(&opts, "conversation").unwrap();
    assert_eq!(
        recover(&opts, "conversation", &mut client, SID, &mut loaded),
        Ok(Recovery::Held)
    );
    drop(client);
    let seen = worker.join().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|line| matches!(actual_request(line), Some(Request::Precommit(..))))
            .count(),
        1
    );
    assert!(
        !seen
            .iter()
            .any(|line| matches!(actual_request(line), Some(Request::Commit(..))))
    );
}

/// The first Prepare reached the host, but its reply did not reach the client.
/// Return exactly what the client durably recorded before that dispatch.
fn lost_prepare(opts: &Opts, command: &PlannedCommand) -> St {
    let (mut st, _) = state(command);
    let (mut client, worker) = endpoint(|line| {
        common(line).or_else(|| match request(line) {
            Request::Prepare(_) => None,
            other => panic!("lost Prepare never dispatches {other:?}"),
        })
    });
    assert!(
        begin(
            opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(command),
            Some(command),
        )
        .is_err()
    );
    drop(client);
    let seen = worker.join().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|line| matches!(actual_request(line), Some(Request::Prepare(_))))
            .count(),
        1
    );
    let saved = upgrade_drive::load(opts, "conversation").unwrap();
    assert_eq!(saved.native_prepare, st.native_prepare);
    assert!(saved.native.is_none() && !saved.native_retired_unissued);
    saved
}

#[test]
fn every_prepare_replay_waits_for_its_state_barrier() {
    let world = aterm_tempfile::tempdir().unwrap();
    let mut opts = opts(world.path());
    let mut st = lost_prepare(&opts, &planned());
    let original = st.clone();
    let expected = Request::parse(&st.native_prepare).unwrap();
    let (mut client, worker) = endpoint(move |line| {
        let request = request(line);
        assert_eq!(
            request, expected,
            "the exact retained candidate must be replayed"
        );
        let Request::Prepare(plan) = request else {
            unreachable!()
        };
        Some(format!(
            "OK native-act state=admitted id={} cap={}\n",
            plan.id,
            binding().capability
        ))
    });

    opts.dry_run = true;
    assert_eq!(
        recover(&opts, "conversation", &mut client, SID, &mut st),
        Err("native-dry-run")
    );
    opts.dry_run = false;
    let fault = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
    for _ in 0..2 {
        assert_eq!(
            recover(&opts, "conversation", &mut client, SID, &mut st),
            Err("native-unsaved")
        );
        assert_eq!(
            abort(&opts, "conversation", &mut client, SID, &mut st),
            None
        );
        assert!(!retire(&opts, "conversation", &mut client, SID, &mut st));
        assert_eq!(st, original);
    }
    drop(fault);
    assert_eq!(
        recover(&opts, "conversation", &mut client, SID, &mut st),
        Ok(Recovery::Admitted)
    );
    assert_eq!(st.native_prepare, original.native_prepare);
    drop(client);
    assert_eq!(
        worker.join().unwrap().len(),
        1,
        "nothing was dispatched while persistence was unavailable"
    );
}

#[test]
fn a_completed_cold_predecessor_keeps_the_new_asked_prepare_exact_across_lost_ack() {
    use super::super::native_act::{ClaimOutcome, ColdBilling, ColdIntent};
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = opts(world.path());
    let command = planned();
    let mut st = lost_prepare(&opts, &command);
    let Some(Request::Prepare(mut predecessor)) = Request::parse(&st.native_prepare) else {
        panic!("actual saved preparation");
    };
    predecessor.duty = Scope::Relaunch;
    let NativePlan::Restart { cold, rollback, .. } = &mut predecessor.plan else {
        unreachable!()
    };
    *cold = Some(Box::new(ColdIntent {
        login: None,
        home: None,
        auth_observed: true,
        route: super::super::accounts::AuthRoute::Stored,
        conversation: Some("conversation".into()),
        profile: None,
        mode: super::super::accounts::Mode::Wall,
        billing: ColdBilling::Keep,
    }));
    *rollback = None;
    st.native_prepare = Request::prepare(*predecessor.clone()).encode().unwrap();
    // This endpoint control qualifies client persistence/replay only. The real host
    // controls independently prove that a cold terminal ACK can grant succession.
    assert_eq!(
        terminal_reply(
            &mut st,
            &Reply::ColdRestartCommitted {
                claim: ClaimOutcome::Builtin,
                observation: None,
                intent: Box::new(super::super::accounts_pool::ExecutionIntent::Stored {
                    namespace: "11".repeat(32),
                    login: None,
                    owner: None,
                    generation: "22".repeat(32),
                }),
            }
        ),
        Some(Recovery::Completed)
    );
    let expected_predecessor = predecessor.id.clone();
    let completed = st.clone();
    let (mut client, worker) = endpoint(|line| {
        assert_eq!(line, format!("@{SID} native-act constructors"));
        Some("OK native-act state=constructors plans=-\n".into())
    });
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Err("native-asked-constructor-disabled")
    );
    assert_eq!(st, completed);
    drop(client);
    assert_eq!(
        worker.join().unwrap(),
        [format!("@{SID} native-act constructors")]
    );
    let mut no_private_token = opts.clone();
    no_private_token.token = None;
    let (mut client, worker) = endpoint(|_| panic!("no private authority dispatches no request"));
    assert_eq!(
        begin(
            &no_private_token,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Err("native-asked-upgrade-unqualified")
    );
    assert_eq!(st, completed);
    drop(client);
    assert!(worker.join().unwrap().is_empty());
    let (mut client, worker) = endpoint(move |line| {
        if line == format!("@{SID} native-act constructors") {
            return Some("OK native-act state=constructors plans=asked-upgrade\n".into());
        }
        common(line).or_else(|| match request(line) {
            Request::Prepare(plan) => {
                assert_eq!(plan.constructor(), Constructor::AskedUpgrade);
                assert_eq!(plan.asked.unwrap().predecessor(), expected_predecessor);
                None
            }
            other => panic!("lost asked Prepare never dispatches {other:?}"),
        })
    });
    assert!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        )
        .is_err()
    );
    drop(client);
    let seen = worker.join().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|line| matches!(actual_request(line), Some(Request::Prepare(_))))
            .count(),
        1
    );
    let saved = st.native_prepare.clone();
    let mut loaded = upgrade_drive::load(&opts, "conversation").unwrap();
    assert_eq!(loaded.native_prepare, saved);
    for reply in [
        format!(
            "OK native-act state=pending id={} cap={}\n",
            "fe".repeat(16),
            binding().capability
        ),
        "OK native-act state=unknown\n".into(),
    ] {
        let expected = saved.clone();
        let (mut client, worker) = endpoint(move |line| {
            assert_eq!(line, format!("@{SID} native-act {expected}"));
            Some(reply.clone())
        });
        assert!(matches!(
            recover(&opts, "conversation", &mut client, SID, &mut loaded),
            Err("native-unknown")
        ));
        assert_eq!(loaded.native_prepare, saved);
        assert!(loaded.native.is_none());
        drop(client);
        assert_eq!(worker.join().unwrap().len(), 1);
    }
    let expected = saved.clone();
    let (mut client, worker) = endpoint(move |line| {
        assert_eq!(line, format!("@{SID} native-act {expected}"));
        let Request::Prepare(plan) = request(line) else {
            unreachable!()
        };
        Some(format!(
            "OK native-act state=admitted id={} cap={}\n",
            plan.id,
            binding().capability
        ))
    });
    let fault = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
    assert_eq!(
        recover(&opts, "conversation", &mut client, SID, &mut loaded),
        Err("native-unsaved")
    );
    assert_eq!(loaded.native_prepare, saved);
    drop(fault);
    assert_eq!(
        recover(&opts, "conversation", &mut client, SID, &mut loaded),
        Ok(Recovery::Admitted)
    );
    assert_eq!(loaded.native_prepare, saved);
    drop(client);
    assert_eq!(worker.join().unwrap().len(), 1);
}

#[test]
fn a_model_seeded_upgrade_directory_can_persist_before_native_dispatch() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    for mode in [0o700, 0o755] {
        let world = aterm_tempfile::tempdir().unwrap();
        let opts = opts(world.path());
        let directory = opts.state.join("upgrade");
        let models = directory.join("models.json");
        super::super::upgrade_models::load_or_seed(&models, 1).unwrap();
        let model_bytes = std::fs::read(&models).unwrap();
        // The shared model writer permits an existing owned directory. Pin its
        // historical readable mode independently of this test process's umask.
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(std::fs::metadata(&directory).unwrap().mode() & 0o777, mode);
        let command = planned();
        let (mut st, _) = state(&command);
        let saved_path = directory.join("conversation.json");
        let (mut client, worker) = endpoint(move |line| {
            common(line).or_else(|| match request(line) {
                Request::Prepare(plan) => {
                    assert!(saved_path.is_file(), "persistence precedes dispatch");
                    Some(format!(
                        "OK native-act state=admitted id={} cap={}\n",
                        plan.id,
                        binding().capability
                    ))
                }
                other => panic!("preparation never dispatches {other:?}"),
            })
        });
        let result = begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command),
        );
        drop(client);
        let seen = worker.join().unwrap();
        assert_eq!(
            result,
            Ok(Recovery::Admitted),
            "owned upgrade mode {mode:o}"
        );
        assert_eq!(
            seen.iter()
                .filter(|line| matches!(actual_request(line), Some(Request::Prepare(_))))
                .count(),
            1
        );
        assert_eq!(std::fs::metadata(&directory).unwrap().mode() & 0o777, 0o700);
        assert_eq!(std::fs::read(&models).unwrap(), model_bytes);
        let mut loaded = upgrade_drive::load(&opts, "conversation").unwrap();
        assert_eq!(loaded.native_prepare, st.native_prepare);
        let expected = Request::parse(&loaded.native_prepare).unwrap();
        let (mut client, worker) = endpoint(move |line| {
            let actual = request(line);
            assert_eq!(actual, expected, "recovery preserves the exact preparation");
            let Request::Prepare(plan) = actual else {
                unreachable!()
            };
            Some(format!(
                "OK native-act state=admitted id={} cap={}\n",
                plan.id,
                binding().capability
            ))
        });
        assert_eq!(
            recover(&opts, "conversation", &mut client, SID, &mut loaded),
            Ok(Recovery::Admitted)
        );
        drop(client);
        assert_eq!(worker.join().unwrap().len(), 1);
    }
}

#[test]
fn private_native_persistence_never_follows_a_link_or_replaces_a_non_directory() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    for symlink in [true, false] {
        let world = aterm_tempfile::tempdir().unwrap();
        let opts = opts(world.path());
        std::fs::create_dir_all(&opts.state).unwrap();
        let directory = opts.state.join("upgrade");
        let target = world.path().join("outside-upgrade");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(target.join("sentinel"), b"untouched").unwrap();
        if symlink {
            std::os::unix::fs::symlink(&target, &directory).unwrap();
        } else {
            std::fs::write(&directory, b"not a directory").unwrap();
        }
        let command = planned();
        let (mut st, _) = state(&command);
        let (mut client, worker) = endpoint(|line| {
            common(line).or_else(|| panic!("refused storage dispatched a native request"))
        });
        let result = begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command),
        );
        drop(client);
        let seen = worker.join().unwrap();
        assert_eq!(result, Err("native-unsaved"));
        assert!(
            !seen
                .iter()
                .any(|line| matches!(actual_request(line), Some(Request::Prepare(_))))
        );
        assert_eq!(std::fs::metadata(&target).unwrap().mode() & 0o777, 0o755);
        assert_eq!(
            std::fs::read(target.join("sentinel")).unwrap(),
            b"untouched"
        );
        if symlink {
            assert!(
                std::fs::symlink_metadata(&directory)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        } else {
            assert_eq!(std::fs::read(&directory).unwrap(), b"not a directory");
        }
    }
}

#[test]
fn retirement_replays_a_lost_prepare_ack_without_launching_its_command() {
    for already_retired in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let opts = opts(world.path());
        let command = planned();
        let mut st = lost_prepare(&opts, &command);
        let original = st.clone();
        let expected = Request::parse(&st.native_prepare).unwrap();
        let Request::Prepare(plan) = &expected else {
            panic!("saved Prepare")
        };
        let recovered = Binding {
            id: plan.id.clone(),
            capability: binding().capability,
        };
        let recovered_copy = recovered.clone();
        let (mut client, worker) = endpoint(move |line| match request(line) {
            Request::Prepare(plan) => {
                assert_eq!(Request::Prepare(plan), expected, "only the exact replay");
                Some(if already_retired {
                    "OK native-act state=retired reason=no-exec-grant\n".into()
                } else {
                    format!(
                        "OK native-act state=admitted id={} cap={}\n",
                        recovered.id, recovered.capability
                    )
                })
            }
            Request::Reconcile(binding) => {
                assert!(!already_retired);
                assert_eq!(binding, recovered);
                Some("OK native-act state=retired reason=no-exec-grant\n".into())
            }
            other => panic!("retirement recovery never dispatches {other:?}"),
        });
        assert!(!retire(&opts, "conversation", &mut client, SID, &mut st));
        assert!(st.native_retired_unissued);
        let mut expected_state = original;
        expected_state.native_retired_unissued = true;
        if !already_retired {
            expected_state.native = Some(recovered_copy);
        }
        assert_eq!(st, expected_state, "all original debt survives");
        upgrade_drive::save_checked(&opts, "conversation", &st).unwrap();
        let mut loaded = upgrade_drive::load(&opts, "conversation").unwrap();
        assert!(loaded.native_retired_unissued);
        assert!(!retire(
            &opts,
            "conversation",
            &mut client,
            SID,
            &mut loaded
        ));
        drop(client);
        let seen = worker.join().unwrap();
        assert_eq!(seen.len(), if already_retired { 1 } else { 2 });
    }
}

fn bound_record(opts: &Opts) -> St {
    let mut st = lost_prepare(opts, &planned());
    let Request::Prepare(plan) = Request::parse(&st.native_prepare).unwrap() else {
        panic!("saved Prepare")
    };
    st.native = Some(Binding {
        id: plan.id.clone(),
        capability: binding().capability,
    });
    st
}

#[test]
fn recovery_rejects_a_binding_without_its_matching_canonical_preparation() {
    for invalid in ["absent", "malformed", "non-prepare", "other-operation"] {
        let world = aterm_tempfile::tempdir().unwrap();
        let opts = opts(world.path());
        let mut st = bound_record(&opts);
        match invalid {
            "absent" => st.native_prepare.clear(),
            "malformed" => st.native_prepare = "unreadable retained preparation".into(),
            "non-prepare" => {
                st.native_prepare = Request::Query(st.native.clone().unwrap()).encode().unwrap();
            }
            "other-operation" => st.native.as_mut().unwrap().id = "fe".repeat(16),
            _ => unreachable!(),
        }
        let original = st.clone();
        let before = std::fs::read(opts.state.join("upgrade/conversation.json")).unwrap();
        let (mut client, worker) = endpoint(|_| panic!("invalid recovery dispatched a request"));
        assert_eq!(
            recover(&opts, "conversation", &mut client, SID, &mut st),
            Err("native-plan"),
            "{invalid}"
        );
        assert_eq!(st, original, "{invalid}: debt must be preserved");
        assert_eq!(
            std::fs::read(opts.state.join("upgrade/conversation.json")).unwrap(),
            before,
            "{invalid}: invalid recovery must not republish the record"
        );
        drop(client);
        assert!(worker.join().unwrap().is_empty());
    }
}

#[test]
fn empty_native_retirement_is_already_complete_even_in_dry_run() {
    let world = aterm_tempfile::tempdir().unwrap();
    let mut opts = opts(world.path());
    opts.dry_run = true;
    let mut st = St::default();
    let before = st.clone();
    let (mut client, worker) = endpoint(|_| panic!("empty retirement dispatched a request"));
    assert!(retire(&opts, "conversation", &mut client, SID, &mut st));
    assert_eq!(st, before);
    assert!(!opts.state.exists());
    drop(client);
    assert!(worker.join().unwrap().is_empty());
}

#[test]
fn a_loaded_binding_cannot_reconcile_before_its_own_durability_barrier() {
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = opts(world.path());
    let st = bound_record(&opts);
    let fault = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
    assert!(upgrade_drive::save_checked(&opts, "conversation", &st).is_err());
    let mut loaded = upgrade_drive::load(&opts, "conversation").unwrap();
    assert_eq!(
        loaded.native, st.native,
        "rename was visible before its barrier"
    );
    // The kept prepare replays first; its admission reconciles the binding.
    let replay = Request::parse(&st.native_prepare).unwrap();
    let binding = st.native.clone().unwrap();
    let expected = Request::Reconcile(binding.clone());
    let allowed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let at_server = std::sync::Arc::clone(&allowed);
    let (mut client, worker) = endpoint(move |line| {
        assert!(
            at_server.load(std::sync::atomic::Ordering::Acquire),
            "retirement reached the wire before the directory barrier was repaired"
        );
        let request = request(line);
        if request == replay {
            return Some(format!(
                "OK native-act state=admitted id={} cap={}\n",
                binding.id, binding.capability
            ));
        }
        assert_eq!(request, expected);
        None
    });
    let mut dry = opts.clone();
    dry.dry_run = true;
    assert!(!retire(&dry, "conversation", &mut client, SID, &mut loaded));
    assert!(!retire(
        &opts,
        "conversation",
        &mut client,
        SID,
        &mut loaded
    ));
    assert_eq!(loaded.native, st.native);
    drop(fault);
    allowed.store(true, std::sync::atomic::Ordering::Release);
    assert!(!retire(
        &opts,
        "conversation",
        &mut client,
        SID,
        &mut loaded
    ));
    drop(client);
    assert_eq!(
        worker.join().unwrap().len(),
        2,
        "only the durable attempt leaves: its replay and its reconciliation"
    );
}

#[test]
fn a_stale_bound_record_cannot_reconcile_over_a_newer_status_writer() {
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = opts(world.path());
    let st = bound_record(&opts);
    upgrade_drive::save_checked(&opts, "conversation", &st).unwrap();
    let mut stale = upgrade_drive::load(&opts, "conversation").unwrap();
    let mut current = upgrade_drive::load(&opts, "conversation").unwrap();
    current.wait = "newer writer".into();
    upgrade_drive::save_checked(&opts, "conversation", &current).unwrap();
    let (mut client, worker) = endpoint(|_| panic!("stale state dispatched retirement"));
    assert!(!retire(&opts, "conversation", &mut client, SID, &mut stale));
    drop(client);
    assert!(worker.join().unwrap().is_empty());
    assert_eq!(
        upgrade_drive::load(&opts, "conversation").unwrap().wait,
        "newer writer"
    );
}

#[test]
fn unresolved_prepare_replay_preserves_debt_and_cannot_reconcile_another_id() {
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = opts(world.path());
    let command = planned();
    let original = lost_prepare(&opts, &command);
    let expected = Request::parse(&original.native_prepare).unwrap();
    let Request::Prepare(plan) = &expected else {
        panic!("saved Prepare")
    };
    let different_id = format!(
        "{}{}",
        if plan.id.starts_with('f') { '0' } else { 'f' },
        &plan.id[1..]
    );
    for response in [
        None,
        Some("ERR native host epoch changed\n".to_string()),
        Some("OK native-act state=unknown\n".to_string()),
        Some(format!(
            "OK native-act state=pending id={} cap={}\n",
            plan.id,
            binding().capability
        )),
        Some(format!(
            "OK native-act state=admitted id={} cap={}\n",
            different_id,
            binding().capability
        )),
    ] {
        // Each reply case starts from the current durable generation. A replay
        // saves before sending, so the preceding case made `original` stale.
        let mut st = upgrade_drive::load(&opts, "conversation").unwrap();
        assert_eq!(st, original, "reloading preserves the unresolved debt");
        let expected = expected.clone();
        let (mut client, worker) = endpoint(move |line| {
            assert_eq!(request(line), expected, "only the exact Prepare may leave");
            response.clone()
        });
        assert!(!retire(&opts, "conversation", &mut client, SID, &mut st));
        assert_eq!(st, original);
        drop(client);
        assert_eq!(worker.join().unwrap().len(), 1);
    }
}

fn binding() -> Binding {
    Binding {
        id: "ab".repeat(16),
        capability: "cd".repeat(32),
    }
}

fn endpoint(
    answer: impl FnMut(&str) -> Option<String> + Send + 'static,
) -> (Client, std::thread::JoinHandle<Vec<String>>) {
    let (near, far) = UnixStream::pair().unwrap();
    for stream in [&near, &far] {
        stream
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        stream
            .set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
    }
    let worker = std::thread::spawn(move || {
        let mut answer = answer;
        let mut writer = far.try_clone().unwrap();
        let mut reader = std::io::BufReader::new(far);
        let mut seen = Vec::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let line = line.trim_end_matches(['\r', '\n']).to_string();
            seen.push(line.clone());
            let Some(reply) = answer(&line) else {
                break;
            };
            writer.write_all(reply.as_bytes()).unwrap();
        }
        seen
    });
    (crate::RelayClient::new(near), worker)
}

fn common(line: &str) -> Option<String> {
    if line == format!("@{SID} native-act rejection-domain") {
        return Some("ERR usage: native-act <prepare|precommit|commit|query|signal|resume> <bounded fields>\n".into());
    }
    if line == format!("@{SID} native-act constructors") {
        return Some("OK native-act state=constructors plans=private-restart\n".into());
    }
    if line == "help native-act" {
        return Some("OK 0\n".into());
    }
    if line == format!("@{SID} status") {
        return Some(
            "OK supervisor_revision=7 human_seq=0 human_ms=- hold=0 hand=- input=clear\n".into(),
        );
    }
    if line == format!("@{SID} native-act info") {
        return Some(format!(
            "OK native-act state=info epoch={} native-act=1 handoff=blocked clock=monotonic-us own-image=grok-alab:upgrade\n",
            "34".repeat(16)
        ));
    }
    None
}

fn request(line: &str) -> Request {
    Request::parse(
        line.strip_prefix(&format!("@{SID} native-act "))
            .expect("typed native request"),
    )
    .unwrap()
}

#[test]
fn an_unavailable_rejection_domain_never_silently_downgrades_a_new_prepare() {
    for answer in [None, Some("OK native-act state=unknown\n")] {
        let home = aterm_tempfile::tempdir().unwrap();
        let options = opts(home.path());
        let command = planned();
        let (mut st, _) = state(&command);
        let (mut client, worker) = endpoint(move |line| {
            if line == format!("@{SID} native-act rejection-domain") {
                answer.map(str::to_owned)
            } else {
                common(line)
            }
        });
        assert_eq!(
            begin(
                &options,
                &mut client,
                &mut st,
                "conversation",
                Scope::Relaunch,
                &command.target,
                Some(&command),
                Some(&command)
            ),
            Err("native-rejection-domain-unavailable")
        );
        assert!(st.native_prepare.is_empty());
        drop(client);
        let seen = worker.join().unwrap();
        assert!(
            !seen
                .iter()
                .any(|line| line.contains(" native-act prepare "))
        );
        let (mut retry, worker) = endpoint(|line| {
            if line == format!("@{SID} native-act rejection-domain") {
                return Some(format!(
                    "OK native-act state=rejection-domain domain={}\n",
                    "ab".repeat(32)
                ));
            }
            common(line).or_else(|| match request(line) {
                Request::Prepare(plan) => {
                    assert_eq!(plan.rejection_domain, Some("ab".repeat(32)));
                    Some(format!(
                        "OK native-act state=admitted id={} cap={}\n",
                        plan.id,
                        binding().capability
                    ))
                }
                other => panic!("unexpected retry: {other:?}"),
            })
        });
        assert_eq!(
            begin(
                &options,
                &mut retry,
                &mut st,
                "conversation",
                Scope::Relaunch,
                &command.target,
                Some(&command),
                Some(&command)
            ),
            Ok(Recovery::Admitted)
        );
        drop(retry);
        worker.join().unwrap();
    }
}

fn actual_request(line: &str) -> Option<Request> {
    Request::parse(line.strip_prefix(&format!("@{SID} native-act "))?)
}

fn opts(dir: &Path) -> Opts {
    Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        account_roster: None,
        sock: Some(dir.join("native.sock").to_string_lossy().into_owned()),
        token: Some(HarnessToken::new("private-memory-fixture-token")),
        only_sid: Some(SID.into()),
        dry_run: false,
        human_grace_s: 0,
        hand_back: false,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        usage_tap: None,
        instances: Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        timings: crate::harness::Timings::FAST,
        tell: Tell::Off,
        press: None,
    }
}

fn planned() -> PlannedCommand {
    PlannedCommand {
        account_intent: None,
        dialect: Dialect::Zsh,
        heal: None,
        cwd: None,
        target: std::env::current_exe().unwrap(),
        argv: Vec::new(),
        env: Vec::new(),
        unset: None,
    }
}

#[test]
fn a_cold_completion_keeps_the_selected_intent_and_the_original_seed_with_or_without_observation() {
    use super::super::native_act::{ClaimOutcome, ColdBilling, ColdIntent};
    use super::super::native_observe::{ObservationBinding, Registration, SavedObservation};
    let vendor = Process::observe(std::process::id()).unwrap();
    let mut shell = vendor.clone();
    shell.pid = if vendor.pid == 2 { 3 } else { 2 };
    shell.group = shell.pid;
    let original = Prepare {
        rejection_domain: None,
        asked: None,
        id: "ab".repeat(16),
        host_epoch: "ef".repeat(16),
        holder: "aterm-harness@fixture".into(),
        revision: 1,
        expected_human_seq: Some(0),
        deadline_us: 1_000_000,
        duty: Scope::Relaunch,
        plan: NativePlan::Restart {
            process: vendor.clone(),
            shell: shell.clone(),
            launcher: vendor.image.clone(),
            socket: "/fixture-native.sock".into(),
            forward: Box::new(planned().capture().unwrap()),
            rollback: None,
            observation: None,
            cold: Some(Box::new(ColdIntent {
                login: None,
                home: None,
                auth_observed: true,
                route: crate::harness::accounts::AuthRoute::Stored,
                conversation: None,
                profile: None,
                mode: super::super::accounts::Mode::Wall,
                billing: ColdBilling::Keep,
            })),
        },
        boot: None,
        custody: None,
    };
    let intent = super::super::accounts_pool::ExecutionIntent::Stored {
        namespace: "11".repeat(32),
        login: Some("/selected-account".into()),
        owner: Some("work".into()),
        generation: "22".repeat(32),
    };
    let observed = SavedObservation {
        registration: Registration {
            id: "98".repeat(16),
            host_epoch: original.host_epoch.clone(),
            vendor,
            shell,
        },
        binding: ObservationBinding {
            id: "98".repeat(16),
            host_epoch: original.host_epoch.clone(),
            capability: "76".repeat(32),
        },
    };
    for observation in [None, Some(Box::new(observed))] {
        let encoded = Request::prepare(original.clone()).encode().unwrap();
        let mut st = St {
            native_prepare: encoded,
            native: Some(binding()),
            phase: Phase::Exiting { at_s: 1 },
            ..St::default()
        };
        assert!(st.account_intent.is_none());
        let receipt = Reply::ColdRestartCommitted {
            claim: ClaimOutcome::Builtin,
            observation: observation.clone(),
            intent: Box::new(intent.clone()),
        };
        assert_eq!(terminal_reply(&mut st, &receipt), Some(Recovery::Completed));
        assert_eq!(st.account_intent.as_ref(), Some(&intent));
        assert_eq!(st.native_completed_prepare.as_deref(), Some(&original));
        assert_eq!(st.native_observation.as_ref(), observation.as_deref());
        assert!(st.native.is_none() && st.native_prepare.is_empty());
        let encoded = st.to_json();
        let record: aterm_json::Value = aterm_json::from_str(&encoded).unwrap();
        assert_eq!(
            record.get("native_completed_prepare").unwrap().as_str(),
            Request::prepare(original.clone())
                .encode_legacy_cold()
                .as_deref(),
            "an older supervisor can still decode its completed observation"
        );
        assert_eq!(
            St::from_json(&encoded).unwrap().native_completed_prepare,
            st.native_completed_prepare
        );
        let completed = st.clone();
        assert_eq!(terminal_reply(&mut st, &receipt), Some(Recovery::Completed));
        assert_eq!(st.account_intent, completed.account_intent);
        assert_eq!(
            st.native_completed_prepare,
            completed.native_completed_prepare
        );
    }
}

/// The test process stands in for both the agent and its shell: its own image is one nothing
/// relinks while it runs. Its launcher's is not: `verify`'s own test compile relinks the
/// `aterm-verify` it runs from, and [`Process::observe`] refuses a process whose file was replaced.
fn state(command: &PlannedCommand) -> (St, super::super::upgrade_drive::Unsent) {
    let process = Process::observe(std::process::id()).expect("actual test process");
    let mut st = St {
        agent: Agent::Claude,
        phase: Phase::Announced { at_s: 1, asks: 1 },
        source: "native".into(),
        from: "1.0.1".into(),
        to: "1.0.2".into(),
        ..St::default()
    };
    let line = command.capture().unwrap().legacy_line().unwrap();
    let back = st.signalled(
        process.pid,
        &super::super::upgrade_drive::kernel_start(process.pid).unwrap_or_default(),
        process.pid,
        SID,
        line.clone(),
        2,
    );
    st.back_line = line;
    (st, back)
}

#[test]
fn fresh_private_constructor_keeps_legacy_before_any_prepare_when_kind_is_disabled_or_absent() {
    for reply in [
        "OK native-act state=constructors plans=-\n",
        "OK native-act state=constructors plans=own-image\n",
        "ERR usage: unknown native-act verb\n",
        "OK native-act state=constructors plans=private-restart,unknown\n",
    ] {
        let world = aterm_tempfile::tempdir().unwrap();
        let opts = opts(world.path());
        let command = planned();
        let (mut st, _) = state(&command);
        let (mut client, worker) = endpoint(move |line| {
            (line == format!("@{SID} native-act constructors")).then(|| reply.to_string())
        });
        let result = begin(
            &opts,
            &mut client,
            &mut st,
            SID,
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command),
        );
        drop(client);
        let seen = worker.join().unwrap();
        assert_eq!(result, Ok(Recovery::Legacy));
        assert_eq!(seen, [format!("@{SID} native-act constructors")]);
        assert!(!st.native_coordination);
        assert!(st.native_prepare.is_empty());
        assert!(st.native.is_none());
        assert!(
            !opts.state.exists(),
            "no preparation/persistence precedes enabled-kind discovery"
        );
    }
}

#[test]
fn unproven_completed_history_never_restores_raw_fallback_after_native_coordination() {
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = opts(world.path());
    let command = planned();
    let (mut st, back) = state(&command);
    st.native_coordination = true;
    let bound = bound_record(&opts);
    st.native = bound.native;
    st.native_prepare = bound.native_prepare;
    assert_eq!(
        terminal_reply(
            &mut st,
            &Reply::Decision(Decision::ProvenNotCommittedRollbackSafe)
        ),
        Some(Recovery::Cancelled)
    );
    st.unsent(back);
    let (mut client, worker) = endpoint(|line| {
        (line == format!("@{SID} native-act constructors"))
            .then(|| "OK native-act state=constructors plans=-\n".into())
    });
    let result = begin(
        &opts,
        &mut client,
        &mut st,
        SID,
        Scope::Upgrade,
        &command.target,
        Some(&command),
        Some(&command),
    );
    drop(client);
    let seen = worker.join().unwrap();
    assert_eq!(result, Err("native-asked-predecessor-unproven"));
    assert!(st.native_coordination);
    assert!(seen.is_empty());
    let mut no_private_token = opts.clone();
    no_private_token.token = None;
    let (mut client, worker) =
        endpoint(|_| panic!("no authority means no new native or raw request"));
    let result = begin(
        &no_private_token,
        &mut client,
        &mut st,
        SID,
        Scope::Upgrade,
        &command.target,
        Some(&command),
        Some(&command),
    );
    drop(client);
    assert!(worker.join().unwrap().is_empty());
    assert_eq!(result, Err("native-asked-predecessor-unproven"));
}

#[test]
fn private_token_constructor_retains_both_plans_and_original_budget_through_rollback() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = opts(dir.path());
    let command = planned();
    let (mut st, _) = state(&command);
    let original = std::sync::Arc::new(std::sync::Mutex::new(None::<Prepare>));
    let kept = original.clone();
    let durable_opts = opts.clone();
    let mut issued = None;
    let (mut client, worker) = endpoint(move |line| {
        if let Some(reply) = common(line) {
            return Some(reply);
        }
        let reply = match request(line) {
            Request::Prepare(plan) => {
                let mut saved = kept.lock().unwrap();
                if let Some(before) = saved.as_ref() {
                    assert_eq!(before, &*plan);
                } else {
                    *saved = Some((*plan).clone());
                }
                let cap = Binding {
                    id: plan.id,
                    capability: binding().capability,
                };
                issued = Some(cap.clone());
                format!(
                    "OK native-act state=admitted id={} cap={}\n",
                    cap.id, cap.capability
                )
            }
            Request::Signal(cap) => {
                assert_eq!(Some(cap), issued);
                "OK native-act state=signalled\n".into()
            }
            Request::Resume(cap, phase, _, seq) => {
                assert_eq!(Some(cap), issued);
                assert_eq!(seq, 0);
                format!("OK native-act state=resumed phase={}\n", phase.word())
            }
            Request::Query(_) => "OK native-act state=resumed phase=rollback\n".into(),
            Request::Precommit(_, phase) => {
                assert_eq!(phase, ImagePhase::Rollback);
                "OK native-act state=precommitted\n".into()
            }
            Request::Commit(_, phase, version) => {
                assert!(
                    upgrade_drive::load(&durable_opts, "conversation")
                        .unwrap()
                        .native_rollback,
                    "the actual durable phase precedes the host Commit, including its lost ACK"
                );
                assert_eq!(phase, ImagePhase::Rollback);
                assert_eq!(version, "1.0.1");
                "OK native-act state=committed claim=builtin revision=-\n".into()
            }
            other => panic!("unexpected constructor request {other:?}"),
        };
        Some(reply)
    });
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Ok(Recovery::Admitted)
    );
    let prepared = st.native_prepare.clone();
    let cap = st.native.clone().unwrap();
    assert!(matches!(signal(&mut client, SID, &cap), Terminated::Sent));
    assert!(resume(&mut client, SID, &cap, ImagePhase::Forward, "1.1", 0).is_ok());
    st.relaunched(3);
    assert!(st.roll_back(4, 0));
    assert!(st.native_rollback);
    assert_eq!(
        recover(&opts, "conversation", &mut client, SID, &mut st),
        Ok(Recovery::Admitted)
    );
    assert_eq!(
        st.native_prepare, prepared,
        "rollback never creates another id or deadline"
    );
    assert!(resume(&mut client, SID, &cap, phase(&st), "1.2", 0).is_ok());
    // A lost Resume ACK / stale local checkpoint cannot erase the host's
    // already accepted Rollback phase. Query must recover it before Precommit.
    st.native_rollback = false;
    assert!(complete(&opts, "conversation", &mut client, SID, &mut st));
    assert!(st.native.is_none() && st.native_prepare.is_empty());
    assert!(
        st.native_rollback,
        "completed rollback remains phase-specific evidence"
    );
    upgrade_drive::save_checked(&opts, "conversation", &st).unwrap();
    assert!(
        upgrade_drive::load(&opts, "conversation")
            .unwrap()
            .native_rollback,
        "durable completion must not become a forward success"
    );
    let prepared_plan = original.lock().unwrap().clone().unwrap();
    let NativePlan::Restart {
        forward,
        rollback: Some(back),
        ..
    } = prepared_plan.plan
    else {
        panic!("both fixed commands required");
    };
    assert_eq!(forward, back);
    drop(client);
    let seen = worker.join().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|line| matches!(actual_request(line), Some(Request::Signal(_))))
            .count(),
        1
    );
    assert!(seen.iter().all(|line| line == "help native-act"
        || line.ends_with(" status")
        || line.contains(" native-act ")));
}

#[test]
fn queued_prepare_safe_receipt_restores_unsent() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = opts(dir.path());
    let command = planned();
    let (mut st, back) = state(&command);
    let mut prepares = 0;
    let (mut client, worker) = endpoint(move |line| {
        if let Some(reply) = common(line) {
            return Some(reply);
        }
        let Request::Prepare(plan) = request(line) else {
            panic!("only the original prepare can be replayed");
        };
        prepares += 1;
        Some(if prepares == 1 {
            format!(
                "OK native-act state=pending id={} cap={}\n",
                plan.id,
                binding().capability
            )
        } else {
            "OK native-act state=rollback-safe\n".into()
        })
    });
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Err("native-predecessors")
    );
    assert!(!st.native_prepare.is_empty());
    assert_eq!(
        abort(&opts, "conversation", &mut client, SID, &mut st),
        Some(Recovery::Cancelled)
    );
    st.unsent(back);
    assert_eq!(st.phase, Phase::Announced { at_s: 1, asks: 1 });
    assert!(st.native.is_none() && st.native_prepare.is_empty());
    upgrade_drive::save_checked(&opts, "conversation", &st).unwrap();
    drop(client);
    worker.join().unwrap();
}

#[test]
fn unknown_prepare_replay_keeps_original_dispatch() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = opts(dir.path());
    let command = planned();
    let (mut st, _) = state(&command);
    let (mut client, worker) =
        endpoint(|line| common(line).or_else(|| Some("OK native-act state=unknown\n".into())));
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Err("native-unknown")
    );
    let before = st.clone();
    assert_eq!(
        recover(&opts, "conversation", &mut client, SID, &mut st),
        Err("native-unknown")
    );
    assert_eq!(st, before);
    drop(client);
    worker.join().unwrap();
}

#[test]
fn an_unadmitted_answer_drops_only_its_own_prepare_and_the_next_begin_plans_afresh() {
    let command = planned();
    let options = |dir: &aterm_tempfile::TempDir| opts(dir.path());
    // An answer naming another id is not about this record.
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = options(&dir);
    let lost = lost_prepare(&opts, &command);
    let mut st = lost.clone();
    let (mut client, worker) = endpoint(|line| match request(line) {
        Request::Prepare(_) => native_act::unadmitted_reply(&"cd".repeat(16)),
        other => panic!("only the saved prepare replays: {other:?}"),
    });
    assert_eq!(
        recover(&opts, "conversation", &mut client, SID, &mut st),
        Err("native-unknown")
    );
    assert_eq!(
        abort(&opts, "conversation", &mut client, SID, &mut st),
        None
    );
    assert_eq!(st, lost);
    drop(client);
    assert_eq!(worker.join().unwrap().len(), 2);
    // The host's proof about this id ends an abort and a retirement alike.
    for retiring in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let opts = options(&dir);
        let mut st = lost_prepare(&opts, &command);
        let (mut client, worker) = endpoint(|line| match request(line) {
            Request::Prepare(plan) => native_act::unadmitted_reply(&plan.id),
            other => panic!("only the saved prepare replays: {other:?}"),
        });
        let ended = if retiring {
            retire(&opts, "conversation", &mut client, SID, &mut st)
        } else {
            abort(&opts, "conversation", &mut client, SID, &mut st) == Some(Recovery::Unadmitted)
        };
        drop(client);
        let seen = worker.join().unwrap();
        assert!(ended, "retiring={retiring} seen={seen:?} st={st:?}");
        assert_eq!(seen.len(), 1);
        assert!(st.native_prepare.is_empty() && st.native.is_none() && !st.native_rollback);
        assert!(st.native_coordination);
        assert!(st.native_completed_prepare.is_none() && st.native_noexec_prepare.is_none());
    }
    // A begin answered so saves the dropped prepare, and the next one mints a fresh id.
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = options(&dir);
    let (mut st, _) = state(&command);
    let mut prepares = 0;
    let (mut client, worker) = endpoint(move |line| {
        if line == format!("@{SID} native-act rejection-domain") {
            return Some(format!(
                "OK native-act state=rejection-domain domain={}\n",
                "ab".repeat(32)
            ));
        }
        common(line).or_else(|| {
            let Request::Prepare(plan) = request(line) else {
                panic!("only prepares are dispatched");
            };
            assert_eq!(plan.rejection_domain.as_ref(), Some(&"ab".repeat(32)));
            prepares += 1;
            Some(if prepares == 1 {
                native_act::unadmitted_reply(&plan.id).unwrap()
            } else {
                format!(
                    "OK native-act state=admitted id={} cap={}\n",
                    plan.id,
                    binding().capability
                )
            })
        })
    });
    let mut begin_relaunch = |st: &mut St| {
        begin(
            &opts,
            &mut client,
            st,
            "conversation",
            Scope::Relaunch,
            &command.target,
            Some(&command),
            Some(&command),
        )
    };
    let admission = begin_relaunch(&mut st);
    assert_eq!(admission, Ok(Recovery::Unadmitted));
    assert_eq!(refusal_word(admission), "native-unadmitted");
    assert!(st.native_prepare.is_empty());
    assert!(
        upgrade_drive::load(&opts, "conversation")
            .unwrap()
            .native_prepare
            .is_empty()
    );
    assert_eq!(begin_relaunch(&mut st), Ok(Recovery::Admitted));
    drop(client);
    let ids = worker
        .join()
        .unwrap()
        .iter()
        .filter_map(|line| match actual_request(line) {
            Some(Request::Prepare(plan)) => Some(plan.id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "a fresh plan has a fresh id");
}

/// `lost_prepare`'s record with its kept prepare made a kind a later host epoch can answer
/// `lapsed` (a typed asked upgrade, or a cold crash restart), saved as this writer's own.
fn lost_lapsable(opts: &Opts, command: &PlannedCommand, asked: bool) -> St {
    let mut st = lost_prepare(opts, command);
    if asked {
        super::fixture::retain_asked(&mut st, None);
    } else {
        super::fixture::retain_cold(&mut st);
    }
    upgrade_drive::save_checked(opts, "conversation", &st).unwrap();
    st
}

fn prepared_ids(seen: &[String]) -> Vec<String> {
    seen.iter()
        .filter_map(|line| match actual_request(line) {
            Some(Request::Prepare(plan)) => Some(plan.id),
            _ => None,
        })
        .collect()
}

#[test]
fn a_kept_operation_replays_before_any_new_plan_and_its_answer_is_saved_without_minting() {
    let command = planned();
    for asked in [false, true] {
        for lapse in [true, false] {
            let dir = aterm_tempfile::tempdir().unwrap();
            let opts = opts(dir.path());
            let mut st = lost_lapsable(&opts, &command, asked);
            let id = retained_prepare(&st).unwrap().id;
            let (mut client, worker) = endpoint(move |line| {
                common(line).or_else(|| match request(line) {
                    Request::Prepare(plan) if lapse => native_act::lapsed_reply(&plan.id),
                    Request::Prepare(plan) => Some(format!(
                        "OK native-act state=admitted id={} cap={}\n",
                        plan.id,
                        binding().capability
                    )),
                    other => panic!("only the kept prepare replays: {other:?}"),
                })
            });
            // An asked upgrade is the Upgrade lane's own begin; a cold crash's is the Relaunch
            // lane's, whose replay comes before any constructor gate.
            let duty = if asked {
                Scope::Upgrade
            } else {
                Scope::Relaunch
            };
            let admission = begin(
                &opts,
                &mut client,
                &mut st,
                "conversation",
                duty,
                &command.target,
                Some(&command),
                Some(&command),
            );
            drop(client);
            assert_eq!(
                prepared_ids(&worker.join().unwrap()),
                std::slice::from_ref(&id),
                "asked={asked} lapse={lapse}: the kept id replays and nothing new is minted"
            );
            let saved = upgrade_drive::load(&opts, "conversation").unwrap();
            if lapse {
                assert_eq!(admission, Ok(Recovery::Lapsed));
                assert!(st.native_prepare.is_empty() && st.native.is_none());
                assert!(saved.native_prepare.is_empty() && saved.native.is_none());
            } else {
                assert_eq!(admission, Err("native-unresolved-operation"));
                let recovered = Some(Binding {
                    id: id.clone(),
                    capability: binding().capability,
                });
                assert_eq!(st.native, recovered);
                assert_eq!(saved.native, recovered, "the recovered binding is saved");
                assert_eq!(saved.native_prepare, st.native_prepare);
            }
        }
    }
}

/// A minted prepare carries this boot and its issuer journal's custody, the stamps a later host
/// epoch's `lapsed` answer reads; a home whose journal holds no record yet stamps no custody.
#[test]
fn a_new_prepare_is_stamped_with_this_boot_and_its_issuer_journals_custody() {
    use super::super::native_receipts::{self, testing};
    let command = planned();
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = opts(dir.path());
    let mint = |session: &str| {
        let (mut st, _) = state(&command);
        let (mut client, worker) = endpoint(|line| {
            common(line).or_else(|| match request(line) {
                Request::Prepare(plan) => Some(format!(
                    "OK native-act state=admitted id={} cap={}\n",
                    plan.id,
                    binding().capability
                )),
                other => panic!("only the new prepare is dispatched: {other:?}"),
            })
        });
        let admission = begin(
            &opts,
            &mut client,
            &mut st,
            session,
            Scope::Relaunch,
            &command.target,
            Some(&command),
            Some(&command),
        );
        drop(client);
        assert_eq!(admission, Ok(Recovery::Admitted));
        let mut minted = worker
            .join()
            .unwrap()
            .iter()
            .filter_map(|line| match actual_request(line) {
                Some(Request::Prepare(plan)) => Some(*plan),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(minted.len(), 1);
        assert_eq!(*retained_prepare(&st).unwrap(), minted[0], "the saved wire");
        minted.remove(0)
    };
    let first = mint("conversation");
    assert!(first.boot.is_some(), "this machine's boot reads");
    assert_eq!(first.boot, native_act::BootStamp::current());
    assert_eq!(first.custody, None);
    // Its admission recorded as the host records it: the next prepare names that journal.
    testing::Journal::new(&opts.home)
        .record(
            &first,
            &Binding {
                id: first.id.clone(),
                capability: binding().capability,
            },
            testing::Append::Prepare,
        )
        .unwrap();
    let custody = native_receipts::custody_for_home(&opts.home).unwrap();
    assert!(custody.is_some());
    let next = mint("next-conversation");
    assert_eq!(next.boot, first.boot);
    assert_eq!(next.custody, custody);
}

#[test]
fn a_lapsed_answer_naming_another_operation_changes_nothing() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = opts(dir.path());
    let lost = lost_lapsable(&opts, &planned(), false);
    let id = retained_prepare(&lost).unwrap().id;
    let elsewhere = format!(
        "{}{}",
        if id.starts_with('f') { '0' } else { 'f' },
        &id[1..]
    );
    let mut st = lost.clone();
    let (mut client, worker) = endpoint(move |line| match request(line) {
        Request::Prepare(_) => native_act::lapsed_reply(&elsewhere),
        other => panic!("only the saved prepare replays: {other:?}"),
    });
    assert_eq!(
        recover(&opts, "conversation", &mut client, SID, &mut st),
        Err("native-unknown")
    );
    assert_eq!(
        abort(&opts, "conversation", &mut client, SID, &mut st),
        None
    );
    assert!(!retire(&opts, "conversation", &mut client, SID, &mut st));
    assert_eq!(st, lost);
    drop(client);
    assert_eq!(worker.join().unwrap().len(), 3);
}

#[test]
fn a_bound_record_replays_its_prepare_and_a_lapse_ends_its_retirement_or_abort() {
    for retiring in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let opts = opts(dir.path());
        let mut st = lost_lapsable(&opts, &planned(), false);
        st.native = Some(Binding {
            id: retained_prepare(&st).unwrap().id,
            capability: binding().capability,
        });
        upgrade_drive::save_checked(&opts, "conversation", &st).unwrap();
        let mut expected = st.clone();
        expected.native = None;
        expected.native_prepare.clear();
        expected.native_rollback = false;
        let (mut client, worker) = endpoint(|line| match request(line) {
            Request::Prepare(plan) => native_act::lapsed_reply(&plan.id),
            other => panic!("a lapse needs nothing more: {other:?}"),
        });
        let ended = if retiring {
            retire(&opts, "conversation", &mut client, SID, &mut st)
        } else {
            abort(&opts, "conversation", &mut client, SID, &mut st) == Some(Recovery::Lapsed)
        };
        drop(client);
        assert_eq!(worker.join().unwrap().len(), 1, "retiring={retiring}");
        assert!(ended, "retiring={retiring}");
        assert_eq!(st, expected, "retiring={retiring}");
    }
}

#[test]
fn private_constructor_refuses_current_person_before_prepare_and_never_sends_raw_signal() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut opts = opts(dir.path());
    opts.human_grace_s = 120;
    let command = planned();
    let (mut st, _) = state(&command);
    let (mut client, worker) = endpoint(|line| {
        if line.ends_with(" status") {
            Some("OK supervisor_revision=7 human_seq=1 human_ms=0 hand=- input=clear\n".into())
        } else {
            common(line)
        }
    });
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Err("native-person-or-hand")
    );
    assert!(st.native_prepare.is_empty());
    drop(client);
    let seen = worker.join().unwrap();
    assert!(!seen.iter().any(|line| line.contains(" native-act prepare")));

    let (mut client, worker) = endpoint(|_| Some("ERR denied native operation authority\n".into()));
    assert!(matches!(
        signal(&mut client, SID, &binding()),
        Terminated::Refused(_)
    ));
    drop(client);
    let seen = worker.join().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(matches!(request(&seen[0]), Request::Signal(_)));
}

#[test]
fn completed_forward_receipt_can_only_prepare_its_fixed_old_slot_as_a_distinct_crash() {
    use super::super::native_observe::{ObservationBinding, Registration, SavedObservation};
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = opts(dir.path());
    let command = planned();
    let (mut st, _) = state(&command);
    let plans = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Prepare>::new()));
    let recorded = plans.clone();
    let (mut client, worker) = endpoint(move |line| {
        if let Some(reply) = common(line) {
            return Some(reply);
        }
        let Request::Prepare(plan) = request(line) else {
            panic!("no effect may be dispatched by preparation");
        };
        recorded.lock().unwrap().push((*plan).clone());
        Some(format!(
            "OK native-act state=admitted id={} cap={}\n",
            plan.id,
            binding().capability
        ))
    });
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Ok(Recovery::Admitted)
    );
    let original = plans.lock().unwrap()[0].clone();
    let process = Process::observe(std::process::id()).unwrap();
    let saved = SavedObservation {
        registration: Registration {
            id: "98".repeat(16),
            host_epoch: original.host_epoch.clone(),
            vendor: process.clone(),
            shell: process.clone(),
        },
        binding: ObservationBinding {
            id: "98".repeat(16),
            host_epoch: original.host_epoch.clone(),
            capability: "76".repeat(32),
        },
    };
    // The endpoint mocks a positive host outcome; GUI conformance proves the actual watch.
    assert_eq!(
        terminal_reply(&mut st, &Reply::RestartCommitted(Box::new(saved.clone()))),
        Some(Recovery::Completed)
    );
    assert!(st.roll_back(upgrade_drive::now_s(), 0));
    let attempt = st.failed_start_kept.clone();
    for case in 0..3 {
        let mut wrong = st.clone();
        match case {
            0 => {
                wrong
                    .native_observation
                    .as_mut()
                    .unwrap()
                    .registration
                    .host_epoch = "ff".repeat(16)
            }
            1 => {
                wrong
                    .native_observation
                    .as_mut()
                    .unwrap()
                    .registration
                    .vendor
                    .image
                    .inode += 1
            }
            _ => wrong.line.push(' '),
        }
        assert_eq!(
            begin_completed_rollback(&opts, &mut client, &mut wrong, "conversation"),
            Err("native-completed-recipe-changed")
        );
    }
    let mut active = st.clone();
    active.native_prepare = Request::prepare(original.clone()).encode().unwrap();
    // The replay saves the kept operation's admission, so it keeps a record of its own.
    assert_eq!(
        begin_completed_rollback(&opts, &mut client, &mut active, "kept-operation"),
        Err("native-unresolved-operation")
    );
    assert_eq!(
        plans
            .lock()
            .unwrap()
            .iter()
            .map(|plan| plan.id.as_str())
            .collect::<Vec<_>>(),
        [original.id.as_str(); 2],
        "negative controls send no new prepare: a kept one only replays"
    );
    assert_eq!(
        begin_completed_rollback(&opts, &mut client, &mut st, "conversation"),
        Ok(Recovery::Admitted)
    );
    let next = plans.lock().unwrap()[2].clone();
    assert_ne!(next.id, original.id);
    assert_eq!(st.native_completed_prepare.as_deref(), Some(&original));
    assert_eq!(
        st.failed_start_kept, attempt,
        "native ids never reset the session's retry policy"
    );
    let NativePlan::Restart {
        rollback: Some(old),
        ..
    } = &original.plan
    else {
        panic!("old slot");
    };
    let NativePlan::Restart {
        process,
        observation: Some(observation),
        forward,
        rollback: None,
        ..
    } = &next.plan
    else {
        panic!("fixed fresh crash plan");
    };
    assert_eq!(forward, old);
    assert_eq!(process, &saved.registration.vendor);
    assert_eq!(observation, &saved.binding);
    assert_eq!(
        upgrade_drive::load(&opts, "conversation")
            .unwrap()
            .native_observation,
        Some(saved)
    );
    drop(client);
    let seen = worker.join().unwrap();
    assert!(!seen.iter().any(|line| matches!(
        actual_request(line),
        Some(Request::Signal(_) | Request::Resume(..))
    )));
}

/// A TOKEN FORWARD'S CRASH ROLLS BACK FROM THE BUILD IT SETTLED ON
/// (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.4): a token forward's program is aterm's
/// wrapper, which execs the store build its `--` names in the same pid, so the vendor the host
/// observes once the forward completes is that build (`RestartCommand::settled`), never the
/// wrapper. When the build then crashes, the completed operation prepares its fixed old slot.
/// CONTROL: a vendor still on the wrapper's image is not what the forward settled on, and is
/// refused as a changed recipe, sending nothing.
///
/// NEGATIVE CONTROL (2026-11-06, restored with cp from a backup plus touch, confirmed with cmp):
/// with `begin_completed_rollback` holding the vendor to `forward.image` again, the vendor still on
/// the wrapper is admitted: `left: Ok(Admitted)`, `right: Err("native-completed-recipe-changed")`.
#[test]
fn a_completed_token_forward_rolls_back_from_the_build_it_settled_on() {
    use super::super::accounts::TokenEntry;
    use super::super::accounts_pool::ExecutionIntent;
    use super::super::native_observe::{ObservationBinding, Registration, SavedObservation};
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = opts(dir.path());
    let back = planned();
    let wrapper = dir.path().join("aterm");
    std::fs::write(&wrapper, b"aterm's wrapper").unwrap();
    std::fs::set_permissions(
        &wrapper,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let build = std::env::current_exe().unwrap();
    let forward = PlannedCommand {
        target: wrapper,
        argv: super::super::token_route::VERB
            .iter()
            .map(|word| (*word).to_owned())
            .chain([
                "work".to_owned(),
                "--".to_owned(),
                build.to_str().unwrap().to_owned(),
            ])
            .collect(),
        account_intent: Some(ExecutionIntent::Token {
            namespace: "ab".repeat(32),
            login: None,
            owner: Some("work".into()),
            entry: TokenEntry::parse("aterm-claude-token-ac-fedcba9876543210").unwrap(),
        }),
        ..planned()
    };
    let captured = forward.capture().expect("a token command");
    let settled = Image::at(&build).expect("the build's image");
    assert_eq!(
        captured.settled(),
        &settled,
        "CONTROL: it settles on the build"
    );
    assert_ne!(
        captured.image, settled,
        "CONTROL: its program is the wrapper"
    );
    let (mut st, _) = state(&back);
    st.line = captured.legacy_line().unwrap();
    let plans = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Prepare>::new()));
    let recorded = plans.clone();
    let (mut client, worker) = endpoint(move |line| {
        if let Some(reply) = common(line) {
            return Some(reply);
        }
        let Request::Prepare(plan) = request(line) else {
            panic!("no effect may be dispatched by preparation");
        };
        recorded.lock().unwrap().push((*plan).clone());
        Some(format!(
            "OK native-act state=admitted id={} cap={}\n",
            plan.id,
            binding().capability
        ))
    });
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Upgrade,
            &forward.target,
            Some(&forward),
            Some(&back)
        ),
        Ok(Recovery::Admitted)
    );
    let original = plans.lock().unwrap()[0].clone();
    let vendor = Process::observe(std::process::id()).unwrap();
    assert_eq!(vendor.image, settled, "CONTROL: the vendor runs the build");
    let saved = SavedObservation {
        registration: Registration {
            id: "98".repeat(16),
            host_epoch: original.host_epoch.clone(),
            vendor: vendor.clone(),
            shell: vendor,
        },
        binding: ObservationBinding {
            id: "98".repeat(16),
            host_epoch: original.host_epoch.clone(),
            capability: "76".repeat(32),
        },
    };
    assert_eq!(
        terminal_reply(&mut st, &Reply::RestartCommitted(Box::new(saved))),
        Some(Recovery::Completed)
    );
    assert!(st.roll_back(upgrade_drive::now_s(), 0));
    let mut on_wrapper = st.clone();
    on_wrapper
        .native_observation
        .as_mut()
        .unwrap()
        .registration
        .vendor
        .image = captured.image.clone();
    assert_eq!(
        begin_completed_rollback(&opts, &mut client, &mut on_wrapper, "conversation"),
        Err("native-completed-recipe-changed"),
        "CONTROL: a vendor on the wrapper"
    );
    assert_eq!(
        plans.lock().unwrap().len(),
        1,
        "the control sends no prepare"
    );
    assert_eq!(
        begin_completed_rollback(&opts, &mut client, &mut st, "conversation"),
        Ok(Recovery::Admitted)
    );
    let next = plans.lock().unwrap()[1].clone();
    let (
        NativePlan::Restart {
            rollback: Some(old),
            ..
        },
        NativePlan::Restart {
            forward: rolled, ..
        },
    ) = (&original.plan, &next.plan)
    else {
        panic!("restart plans");
    };
    assert_eq!(rolled, old, "the fixed old slot");
    drop(client);
    let _ = worker.join().unwrap();
}

#[test]
fn resume_only_driver_starts_with_observation_and_never_reissues_signal_on_lost_reply() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = opts(dir.path());
    let command = planned();
    let (mut st, _) = state(&command);
    let (mut client, worker) = endpoint(|line| {
        if let Some(reply) = common(line) {
            return Some(reply);
        }
        match request(line) {
            Request::Prepare(plan) => Some(format!(
                "OK native-act state=admitted id={} cap={}\n",
                plan.id,
                binding().capability
            )),
            _ => panic!("only preparation expected"),
        }
    });
    assert_eq!(
        begin(
            &opts,
            &mut client,
            &mut st,
            "conversation",
            Scope::Relaunch,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Ok(Recovery::Admitted)
    );
    drop(client);
    worker.join().unwrap();
    let Some(Request::Prepare(prepare)) = Request::parse(&st.native_prepare) else {
        panic!("exact admitted operation");
    };
    for signal_original in [false, true, false] {
        // A closed connection is unknown, so the exact retained admission is
        // replayed. The Resume-only path must not send Signal even on retry.
        let key = format!("renewal-{}", prepare.id);
        let current = upgrade_drive::load_record(&opts, &key)
            .unwrap()
            .unwrap_or_else(|| st.clone());
        let (client, worker) = endpoint(|_| None);
        assert!(
            super::super::codex_renewal::drive_connection(
                &opts,
                SID,
                &prepare,
                current,
                client,
                signal_original,
            )
            .is_none()
        );
        let seen = worker.join().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            matches!(request(&seen[0]), Request::Signal(_)),
            signal_original
        );
        if !signal_original {
            assert!(matches!(request(&seen[0]), Request::Query(_)));
        }
    }
}

#[test]
fn rollback_phase_checkpoint_failure_never_sends_precommit_or_commit() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let opts = opts(dir.path());
    let seed = aterm_tempfile::tempdir().unwrap();
    let mut st = bound_record(&self::opts(seed.path()));
    std::fs::write(&opts.state, b"obstruct owned test state directory").unwrap();
    st.native_rollback = true;
    let original = st.clone();
    let (mut client, worker) = endpoint(|line| {
        assert!(
            matches!(request(line), Request::Query(_)),
            "failed checkpoint must deny every mutation"
        );
        Some("OK native-act state=unknown\n".into())
    });
    assert!(!complete(&opts, "conversation", &mut client, SID, &mut st));
    assert_eq!(st, original);
    drop(client);
    assert_eq!(worker.join().unwrap().len(), 1);
}

/// NEGATIVE CONTROL: passive observation persistence refuses symlinked `state` or
/// `state/native-observations` directories without reading or writing through the symlink.
#[test]
fn passive_observation_refuses_symlinked_state_directories() {
    use super::super::native_observe::{ObservationBinding, Registration};
    use super::observation::{Observed, read, save};

    let dir = aterm_tempfile::tempdir().unwrap();
    let real_opts = opts(&dir.path().join("real"));
    let proc = Process::observe(std::process::id()).expect("actual test process");
    let observed = Observed {
        registration: Registration {
            id: "12".repeat(16),
            host_epoch: "34".repeat(16),
            vendor: proc.clone(),
            shell: proc,
        },
        binding: Some(ObservationBinding {
            id: "12".repeat(16),
            host_epoch: "34".repeat(16),
            capability: "cd".repeat(32),
        }),
    };
    save(&real_opts, SID, &observed).expect("real state directory saves");
    assert!(read(&real_opts, SID).is_some());

    let linked_state = opts(&dir.path().join("linked-state"));
    std::fs::create_dir_all(linked_state.state.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real_opts.state, &linked_state.state).unwrap();
    assert!(read(&linked_state, SID).is_none());
    assert!(save(&linked_state, "other-tab", &observed).is_err());
    assert!(read(&real_opts, "other-tab").is_none());

    let linked_sub = opts(&dir.path().join("linked-sub"));
    std::fs::create_dir_all(&linked_sub.state).unwrap();
    std::os::unix::fs::symlink(
        real_opts.state.join("native-observations"),
        linked_sub.state.join("native-observations"),
    )
    .unwrap();
    assert!(read(&linked_sub, SID).is_none());
    assert!(save(&linked_sub, "other-tab", &observed).is_err());
    assert!(read(&real_opts, "other-tab").is_none());
}
