// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn publication_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        IsolatedCopyPublication {
            const Buggy = 0;
            var phase = 0;
            var current_target = 1;
            var old_target = 1;
            var opened_target = 0;
            var published_target = 0;
            var corrupted = 0;
            action Timeout when (phase == 0) { phase = 1; }
            action Retry when (phase == 1) {
                phase = 2;
                current_target = if Buggy == 1 { old_target } else { 2 };
            }
            action OpenOld when (phase == 2) {
                phase = 3;
                opened_target = old_target;
            }
            action Publish when (phase == 3) {
                phase = 4;
                published_target = current_target;
            }
            action LateWrite when (phase == 4) {
                phase = 5;
                corrupted = if opened_target == published_target { 1 } else { 0 };
            }
            invariant NoLateMutation: corrupted == 0;
        }
    }
}

fn copy_model_step(
    model: &aterm_spec::derive::Model,
    state: &mut aterm_spec::interp::State,
    action: &str,
) {
    let next = model.successors(action, state);
    assert_eq!(next.len(), 1, "{action}: {state:?}");
    *state = next[0].clone();
}

#[test]
fn isolated_copy_proves_and_catches() {
    aterm_spec::verify::prove_and_catch_scalar(&publication_model(), "isolated update copy");
}

// `isolated == false` is the negative control: ditto writes the caller's reusable path
// directly, and success needs no promotion.
fn copy_with_policy(
    isolated: bool,
    args: &[std::ffi::OsString],
    run: impl FnOnce(
        &[std::ffi::OsString],
        &Path,
    ) -> Result<(std::process::ExitStatus, String), crate::verify::HelperFailure>,
) -> Result<(std::process::ExitStatus, String), String> {
    if isolated {
        copy_into_isolated_destination(args, "copy fixture", run)
    } else {
        run(args, Path::new(args.last().unwrap()).parent().unwrap())
            .map_err(|failure| failure.message)
    }
}

#[test]
fn a_timed_out_writer_cannot_mutate_a_retry_after_stage_or_swap_publication() {
    use std::io::Write;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::mpsc;
    for shape in ["dmg", "zip", "cross-volume"] {
        for isolated in [true, false] {
            let staging = Staging::scratch(&format!("copy-late-{shape}-{isolated}"));
            std::fs::create_dir_all(staging.staged_dir()).unwrap();
            let installed = staging.root.join("aterm.app");
            let destination = match shape {
                "dmg" => staging.staged_dir().join("aterm.app.incoming"),
                "zip" => staging.staged_dir().join("zx-fixture"),
                _ => rollback_path(&installed),
            };
            let bundle_in = |payload: &Path| {
                if shape == "zip" {
                    payload.join("aterm.app")
                } else {
                    payload.to_path_buf()
                }
            };
            let args = ["source".into(), destination.as_os_str().to_os_string()];
            let (open_tx, open_rx) = mpsc::sync_channel(1);
            let (opened_tx, opened_rx) = mpsc::sync_channel(1);
            let (write_tx, write_rx) = mpsc::sync_channel(1);
            let mut old_payload = None;
            let mut writer = None;
            let error = copy_with_policy(isolated, &args, |actual, _| {
                let payload = PathBuf::from(actual.last().unwrap());
                let bundle = bundle_in(&payload);
                std::fs::create_dir_all(&bundle).unwrap();
                std::fs::write(bundle.join("id"), "OLD-COPY").unwrap();
                old_payload = Some(payload);
                // Open the old job's destination only after the retry has materialized,
                // and hold it open across the retry's publication or exchange.
                writer = Some(std::thread::spawn(move || {
                    open_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .open(bundle.join("id"))
                        .unwrap();
                    opened_tx.send(()).unwrap();
                    write_rx
                        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                        .unwrap();
                    file.set_len(0).unwrap();
                    file.write_all(b"LATE-WRITE").unwrap();
                    crate::durable::sync_all(&file).unwrap();
                }));
                Err(crate::verify::HelperFailure {
                    message: "copy deadline elapsed; cleanup not confirmed".to_string(),
                    writer_stopped: false,
                })
            })
            .unwrap_err();
            assert!(error.contains("cleanup not confirmed"), "{error}");
            let old_payload = old_payload.unwrap();
            let mut model = publication_model();
            model
                .consts
                .iter_mut()
                .find(|(name, _)| *name == "Buggy")
                .unwrap()
                .1 = i64::from(!isolated);
            let mut state = model.init_state();
            copy_model_step(&model, &mut state, "Timeout");
            // Real callers reclaim their requested destination after a failure; the
            // isolated attempt lies outside that cleanup.
            remove_path_no_follow(&destination).unwrap();
            if shape == "zip" {
                std::fs::create_dir(&destination).unwrap();
            }
            copy_with_policy(isolated, &args, |actual, _| {
                let payload = PathBuf::from(actual.last().unwrap());
                let bundle = bundle_in(&payload);
                std::fs::create_dir_all(&bundle).unwrap();
                std::fs::write(bundle.join("id"), "NEW").unwrap();
                copy_model_step(&model, &mut state, "Retry");
                assert_eq!(
                    state["current_target"],
                    if payload == old_payload { 1 } else { 2 },
                    "the model must bind actual destination identity"
                );
                if isolated {
                    assert_eq!(payload.parent().unwrap().parent(), destination.parent());
                    assert!(
                        old_payload.exists(),
                        "caller cleanup cannot reclaim a live attempt"
                    );
                }
                open_tx.send(()).unwrap();
                opened_rx
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                    .unwrap();
                copy_model_step(&model, &mut state, "OpenOld");
                Ok((std::process::ExitStatus::from_raw(0), String::new()))
            })
            .unwrap();

            let published = if shape == "cross-volume" {
                std::fs::create_dir(&installed).unwrap();
                std::fs::write(installed.join("id"), "ROLLBACK").unwrap();
                checked_bundle_exchange(
                    &destination,
                    &installed,
                    "copy fixture swap",
                    aterm_durable::Flush::Durable,
                )
                .unwrap();
                installed
            } else {
                let ready = Ready {
                    build_number: 42,
                    version: "0.42.0".to_string(),
                    commit: Some("ab".repeat(20)),
                    dmg_sha256: "cd".repeat(32),
                    team_id: String::new(),
                    staged_at: String::new(),
                    changelog: None,
                    machine_id: None,
                    roster_seq: None,
                    roster_confirmed_at: Some(crate::install::now_rfc3339()),
                };
                // The fixture models already-verified bytes; publication and
                // the apply lock below are the genuine shipping transaction.
                publish_verified_stage(&staging, &bundle_in(&destination), &ready).unwrap();
                staging.staged_app.clone()
            };
            copy_model_step(&model, &mut state, "Publish");
            write_tx.send(()).unwrap();
            writer.unwrap().join().unwrap();
            copy_model_step(&model, &mut state, "LateWrite");
            let actual = std::fs::read_to_string(published.join("id")).unwrap();
            assert_eq!(state["corrupted"], i64::from(actual != "NEW"));
            assert_eq!(model.check_invariant("NoLateMutation", &state), isolated);
            assert_eq!(actual, if isolated { "NEW" } else { "LATE-WRITE" });
            if shape == "cross-volume" {
                assert_eq!(
                    std::fs::read_to_string(destination.join("id")).unwrap(),
                    "ROLLBACK"
                );
            }
            let _ = std::fs::remove_dir_all(&staging.root);
        }
    }
}

/// The reaper's contract: it reclaims an attempt past every writer's ceiling whose
/// caller gave it up or is gone. It never takes a live caller's attempt it has not
/// given up, or one a writer could still be filling.
fn reaper_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        AbandonedCopyReclamation {
            const Buggy = 0;
            var abandoned = 0;
            var owner_gone = 0;
            var lapsed = 0;
            var reaped = 0;
            action Abandon when (abandoned == 0 && owner_gone == 0 && reaped == 0) { abandoned = 1; }
            action OwnerDies when (owner_gone == 0 && reaped == 0) { owner_gone = 1; }
            action Lapse when (lapsed == 0 && reaped == 0) { lapsed = 1; }
            action Reap when (reaped == 0 && ((lapsed == 1 && (abandoned == 1 || owner_gone == 1)) || Buggy == 1)) {
                reaped = 1;
            }
            invariant ReclaimOnlyDisowned: reaped == 0 || abandoned == 1 || owner_gone == 1;
            invariant ReclaimOnlyLapsed: reaped == 0 || lapsed == 1;
        }
    }
}

#[test]
fn abandoned_copy_reaping_requires_a_lapsed_disowned_attempt() {
    let model = reaper_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, "abandoned copy reclamation");
    let dead = {
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    };
    let old = std::time::SystemTime::now()
        - STALE_COPY_ATTEMPT_AFTER
        - std::time::Duration::from_secs(60);
    for abandoned in [false, true] {
        for owner_gone in [false, true] {
            for lapsed in [false, true] {
                reaper_case(&model, dead, old, abandoned, owner_gone, lapsed);
            }
        }
    }
}

fn reaper_case(
    model: &aterm_spec::derive::Model,
    dead: u32,
    old: std::time::SystemTime,
    abandoned: bool,
    owner_gone: bool,
    lapsed: bool,
) {
    let staging = Staging::scratch("copy-reaper");
    let attempt = IsolatedCopy::create(&staging.root.join("destination")).unwrap();
    std::fs::create_dir_all(&attempt.payload).unwrap();
    std::fs::write(attempt.payload.join("id"), "COMPLETE").unwrap();
    if abandoned {
        std::fs::write(attempt.root.join("abandoned"), "1\n").unwrap();
    }
    if owner_gone {
        std::fs::write(attempt.root.join(COPY_OWNER), format!("{dead}\n")).unwrap();
    }
    if lapsed {
        std::fs::File::options()
            .write(true)
            .open(attempt.root.join("copy-layout"))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let mut state = model.init_state();
    state.insert("abandoned", i64::from(abandoned));
    state.insert("owner_gone", i64::from(owner_gone));
    state.insert("lapsed", i64::from(lapsed));
    let admitted = model.action_enabled("Reap", &state);
    // Creating an apply/copy attempt must not recursively sweep predecessors,
    // even when their cleanup is already authorized.
    let next = IsolatedCopy::create(&staging.root.join("next-destination")).unwrap();
    assert!(
        attempt.root.exists(),
        "allocation must leave bulk cleanup to the checker"
    );
    reap_abandoned_copy_attempts(&staging.root);
    assert!(
        next.root.exists(),
        "maintenance cannot reclaim the live new attempt"
    );
    let case = format!("abandoned={abandoned} owner_gone={owner_gone} lapsed={lapsed}");
    assert_eq!(!attempt.root.exists(), admitted, "{case}");
    if admitted {
        copy_model_step(model, &mut state, "Reap");
        assert!(
            model.check_invariant("ReclaimOnlyDisowned", &state),
            "{case}"
        );
        assert!(model.check_invariant("ReclaimOnlyLapsed", &state), "{case}");
        assert!(
            !staging.root.join("destination").exists(),
            "late completion cannot promote abandoned bytes"
        );
    }
    if !abandoned && !owner_gone && lapsed {
        // Negative control: a complete payload, however old, is not authority to
        // delete the live caller's not-yet-promoted output.
        std::fs::remove_dir_all(&attempt.root).unwrap();
        state.insert("reaped", 1);
        assert!(
            !model.check_invariant("ReclaimOnlyDisowned", &state),
            "{case}"
        );
    }
    let _ = std::fs::remove_dir_all(&staging.root);
}

#[test]
fn an_observed_stopped_writer_reclaims_its_failed_attempt_immediately() {
    let staging = Staging::scratch("copy-confirmed-failure");
    let destination = staging.root.join("destination");
    let mut root = None;
    let error = copy_into_isolated_destination(
        &[destination.clone().into_os_string()],
        "failed copy",
        |_, attempt| {
            root = Some(attempt.to_path_buf());
            Err(crate::verify::HelperFailure {
                message: "the owned child was reaped".to_string(),
                writer_stopped: true,
            })
        },
    )
    .unwrap_err();
    assert_eq!(error, "the owned child was reaped");
    assert!(!root.unwrap().exists());
    assert!(!destination.exists());
    let _ = std::fs::remove_dir_all(&staging.root);
}
