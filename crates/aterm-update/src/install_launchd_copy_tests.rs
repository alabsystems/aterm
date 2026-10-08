// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

/// How long a fixture that must finish may take: a hang detector, never a
/// latency budget. Each fixture wait ends as soon as the fixture does, so a
/// passing test pays none of it; a wedged fixture fails after a minute.
const FIXTURE_FINISHES: Duration = Duration::from_secs(60);

fn budget_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        LaunchdCopyDeadline {
            const Buggy = 0;
            var now = 0;
            var deadline = 3;
            var cleanup = 0;
            action Tick when (now <= 3) { now = now + 1; }
            action Cleanup when (cleanup == 0) {
                cleanup = 1;
                deadline = if Buggy == 1 { now + 4 } else { 4 };
            }
            invariant NoRenewedDeadline: deadline <= 4;
        }
    }
}

#[test]
fn launchd_copy_deadline_proves_and_catches_renewed_cleanup_budget() {
    let model = budget_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, "launchd copy deadline");
    let started = Instant::now();
    let limit = Duration::from_secs(4);
    let budget = LaunchdCopyBudget::new(started, limit);
    for seconds in 0..=4 {
        let now = started + Duration::from_secs(seconds);
        for cleanup in [false, true] {
            let mut state = model.init_state();
            state.insert("now", i64::try_from(seconds).unwrap());
            state.insert("cleanup", i64::from(cleanup));
            let until = budget.deadline(cleanup);
            state.insert(
                "deadline",
                i64::try_from(until.duration_since(started).as_secs()).unwrap(),
            );
            assert!(model.check_invariant("NoRenewedDeadline", &state));
            assert!(
                until.saturating_duration_since(now)
                    <= limit.saturating_sub(now.duration_since(started))
            );
            if cleanup && seconds > 0 {
                // Historical per-operation timeout: a fresh deadline after
                // submit has already consumed part of the caller's budget.
                state.insert(
                    "deadline",
                    i64::try_from((now + limit).duration_since(started).as_secs()).unwrap(),
                );
                assert!(!model.check_invariant("NoRenewedDeadline", &state));
            }
        }
    }
}

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

fn wrapper_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        LaunchdCopyOnce {
            const Buggy = 0;
            var claimed = 0;
            var complete = 0;
            var copies = 0;
            var replayed = 0;
            var removed = 0;
            action Start when (claimed == 0 && removed == 0) {
                claimed = 1;
                copies = 1;
            }
            action Complete when (claimed == 1 && complete == 0) { complete = 1; }
            action Replay when (complete == 1 && replayed == 0) {
                replayed = 1;
                copies = if Buggy == 1 { 2 } else { copies };
            }
            action Remove when (complete == 1 && removed == 0) { removed = 1; }
            invariant OneCopy: copies <= 1;
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
fn isolated_copy_and_one_shot_wrapper_prove_and_catch() {
    for model in [publication_model(), wrapper_model()] {
        aterm_spec::verify::prove_and_catch_scalar(&model, "isolated update copy");
    }
}

// The negative control replays the old shipping policy: ditto receives the
// caller's reusable path directly, and returning success needs no promotion.
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
                // Open the old job's absolute destination only AFTER the
                // retry has materialized, then keep that actual file open
                // across the retry's publication/atomic exchange.
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
            // The real callers reclaim their requested destination after
            // failure. The isolated attempt must remain outside that cleanup.
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
                checked_bundle_exchange(&destination, &installed, "copy fixture swap").unwrap();
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

#[test]
fn the_real_launchd_wrapper_cannot_copy_again_after_publishing_status() {
    use std::os::unix::process::ExitStatusExt;
    for guarded in [true, false] {
        let staging = Staging::scratch(&format!("copy-wrapper-replay-{guarded}"));
        let cleanup = staging.root.join("cleanup-fixture");
        std::fs::write(&cleanup, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&cleanup, std::fs::Permissions::from_mode(0o700)).unwrap();
        let destination = staging.root.join("result");
        let calls = staging.root.join("copies");
        let script = if guarded {
            LAUNCHD_COPY_WRAPPER.to_string()
        } else {
            LAUNCHD_COPY_WRAPPER.replace("/bin/mkdir \"$st.once\" 2>/dev/null || exit 0; ", "")
        };
        assert_eq!(script == LAUNCHD_COPY_WRAPPER, guarded);
        let run_wrapper = |root: &Path, payload: &Path| {
            let (exit, stderr) = crate::verify::status_bounded_with_stderr(
                aterm_uds::spawnfd::command("/bin/sh")
                    .arg("-c")
                    .arg(&script)
                    .arg("fixture-wrapper")
                    .arg(root.join("status"))
                    .arg("fixture-label")
                    .arg(&cleanup)
                    .arg("/bin/sh")
                    .arg("-c")
                    .arg(r#"/bin/mkdir -p "$1"; printf NEW > "$1/id"; printf 'copy\n' >> "$2""#)
                    .arg("fixture-copy")
                    .arg(payload)
                    .arg(&calls),
                "one-shot wrapper fixture",
                FIXTURE_FINISHES,
            )
            .unwrap();
            assert!(exit.success(), "{stderr}");
        };
        let mut model = wrapper_model();
        model
            .consts
            .iter_mut()
            .find(|(name, _)| *name == "Buggy")
            .unwrap()
            .1 = i64::from(!guarded);
        let mut state = model.init_state();
        let mut old_paths = None;
        copy_into_isolated_destination(
            &[destination.clone().into_os_string()],
            "wrapper copy",
            |args, root| {
                let payload = Path::new(args.last().unwrap());
                run_wrapper(root, payload);
                copy_model_step(&model, &mut state, "Start");
                assert_eq!(std::fs::read_to_string(root.join("status")).unwrap(), "0\n");
                copy_model_step(&model, &mut state, "Complete");
                // Replay the exact shipping shell after its terminal status is
                // visible, before the parent gets to rename the completed tree.
                run_wrapper(root, payload);
                copy_model_step(&model, &mut state, "Replay");
                assert_eq!(
                    state["copies"],
                    i64::try_from(std::fs::read_to_string(&calls).unwrap().lines().count())
                        .unwrap()
                );
                assert_eq!(model.check_invariant("OneCopy", &state), guarded);
                old_paths = Some((root.to_path_buf(), payload.to_path_buf()));
                Ok((std::process::ExitStatus::from_raw(0), String::new()))
            },
        )
        .unwrap();
        copy_model_step(&model, &mut state, "Remove");
        if guarded {
            let (root, payload) = old_paths.unwrap();
            assert!(!root.exists());
            run_wrapper(&root, &payload);
            assert_eq!(std::fs::read_to_string(&calls).unwrap(), "copy\n");
            assert!(!root.exists(), "a replay cannot recreate a retired attempt");
        }
        assert_eq!(
            std::fs::read_to_string(destination.join("id")).unwrap(),
            "NEW"
        );
        let _ = std::fs::remove_dir_all(&staging.root);
    }
}

/// The reaper's contract: settled reclaims an attempt its caller abandoned once
/// the fenced writer published a terminal status; lapsed reclaims one past every
/// writer's ceiling whose caller gave it up or is gone. Neither takes a live
/// caller's attempt it has not given up, or one a writer could still be filling.
fn reaper_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        AbandonedCopyReclamation {
            const Buggy = 0;
            var abandoned = 0;
            var complete = 0;
            var fenced = 1;
            var owner_gone = 0;
            var lapsed = 0;
            var reaped = 0;
            action Abandon when (abandoned == 0 && owner_gone == 0 && reaped == 0) { abandoned = 1; }
            action Complete when (complete == 0 && reaped == 0) { complete = 1; }
            action LoseFence when (fenced == 1 && reaped == 0) { fenced = 0; }
            action OwnerDies when (owner_gone == 0 && reaped == 0) { owner_gone = 1; }
            action Lapse when (lapsed == 0 && reaped == 0) { lapsed = 1; }
            action Reap when (reaped == 0 && ((abandoned == 1 && complete == 1 && fenced == 1) || (lapsed == 1 && (abandoned == 1 || owner_gone == 1)) || Buggy == 1)) {
                reaped = 1;
            }
            invariant ReclaimOnlyDisowned: reaped == 0 || abandoned == 1 || owner_gone == 1;
            invariant ReclaimOnlySettledOrLapsed: reaped == 0 || (complete == 1 && fenced == 1) || lapsed == 1;
        }
    }
}

#[test]
fn abandoned_copy_reaping_requires_terminal_status_and_the_one_shot_fence() {
    let model = reaper_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, "abandoned copy reclamation");
    let dead = {
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    };
    let old = std::time::SystemTime::now()
        - STALE_COPY_ATTEMPT_AFTER
        - std::time::Duration::from_secs(60);
    for abandoned in [false, true] {
        for completed in ["absent", "malformed", "complete"] {
            for fenced in [false, true] {
                for owner_gone in [false, true] {
                    for lapsed in [false, true] {
                        reaper_case(
                            &model, dead, old, abandoned, completed, fenced, owner_gone, lapsed,
                        );
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn reaper_case(
    model: &aterm_spec::derive::Model,
    dead: u32,
    old: std::time::SystemTime,
    abandoned: bool,
    completed: &str,
    fenced: bool,
    owner_gone: bool,
    lapsed: bool,
) {
    let staging = Staging::scratch("copy-reaper");
    let attempt = IsolatedCopy::create(&staging.root.join("destination")).unwrap();
    let label = format!("systems.alab.aterm-update.unpack-{}", "ab".repeat(16));
    std::fs::write(attempt.root.join("launchd.job"), &label).unwrap();
    if abandoned {
        std::fs::write(attempt.root.join("abandoned"), "1\n").unwrap();
    }
    if fenced {
        std::fs::create_dir(attempt.root.join(format!(".{label}.status.once"))).unwrap();
    }
    match completed {
        "complete" => std::fs::write(attempt.root.join(format!(".{label}.status")), "0\n").unwrap(),
        "malformed" => {
            std::fs::write(attempt.root.join(format!(".{label}.status")), "unfinished").unwrap()
        }
        _ => {}
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
    state.insert("complete", i64::from(completed == "complete"));
    state.insert("fenced", i64::from(fenced));
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
    let case = format!(
        "abandoned={abandoned} completed={completed} fenced={fenced} \
         owner_gone={owner_gone} lapsed={lapsed}"
    );
    assert_eq!(!attempt.root.exists(), admitted, "{case}");
    if admitted {
        copy_model_step(model, &mut state, "Reap");
        assert!(
            model.check_invariant("ReclaimOnlyDisowned", &state),
            "{case}"
        );
        assert!(
            model.check_invariant("ReclaimOnlySettledOrLapsed", &state),
            "{case}"
        );
        assert!(
            !staging.root.join("destination").exists(),
            "late completion cannot promote abandoned bytes"
        );
    }
    if !abandoned && !owner_gone && completed == "complete" && fenced {
        // Negative control: completion alone — however old — is not authority
        // to delete the live caller's not-yet-promoted output.
        std::fs::remove_dir_all(&attempt.root).unwrap();
        state.insert("reaped", 1);
        assert!(
            !model.check_invariant("ReclaimOnlyDisowned", &state),
            "{case}"
        );
    }
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A copy attempt nobody can finish is reclaimed in the end: a caller killed
/// mid-copy never marks its attempt, and a launchd copy stopped by `launchctl
/// remove` never writes its status. An attempt whose caller is gone or gave it
/// up, and that has outlived every copy's ceiling by a wide margin, has no writer
/// left to protect.
#[test]
fn an_attempt_whose_caller_or_writer_died_is_eventually_reclaimed() {
    let staging = Staging::scratch("copy-lapsed");
    let dead = {
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    };
    let old = std::time::SystemTime::now()
        - STALE_COPY_ATTEMPT_AFTER
        - std::time::Duration::from_secs(60);
    let label = format!("systems.alab.aterm-update.unpack-{}", "cd".repeat(16));
    let attempt = |name: &str, owner: u32, aged: bool, launchd_cut: bool| {
        let attempt = IsolatedCopy::create(&staging.root.join(name)).unwrap();
        std::fs::create_dir_all(&attempt.payload).unwrap();
        std::fs::write(attempt.payload.join("partial"), "bytes").unwrap();
        std::fs::write(attempt.root.join(COPY_OWNER), format!("{owner}\n")).unwrap();
        if launchd_cut {
            std::fs::write(attempt.root.join("launchd.job"), &label).unwrap();
            std::fs::create_dir(attempt.root.join(format!(".{label}.status.once"))).unwrap();
            std::fs::write(attempt.root.join("abandoned"), "1\n").unwrap();
        }
        if aged {
            std::fs::File::options()
                .write(true)
                .open(attempt.root.join("copy-layout"))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        attempt.root
    };
    let live = std::process::id();
    let caller_killed = attempt("killed", dead, true, false);
    let launchd_cut = attempt("launchd-cut", live, true, true);
    // Negative controls: a live caller that has not given its attempt up (it may
    // be stopped, and will still promote it), and either cut while young enough
    // that a writer could still be finishing.
    let live_caller = attempt("live-caller", live, true, false);
    let young_killed = attempt("young-killed", dead, false, false);
    let young_cut = attempt("young-cut", live, false, true);

    reap_abandoned_copy_attempts(&staging.root);
    assert!(
        !caller_killed.exists(),
        "a dead caller's attempt is reclaimed"
    );
    assert!(!launchd_cut.exists(), "a cut launchd copy is reclaimed");
    assert!(
        live_caller.exists(),
        "a live caller's attempt is never reclaimed"
    );
    assert!(
        young_killed.exists(),
        "a young attempt may still have a writer"
    );
    assert!(
        young_cut.exists(),
        "a young attempt may still have a writer"
    );
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

fn fake_launchctl(staging: &Staging, mode: &str) -> std::path::PathBuf {
    let script = staging.root.join("launchctl-fixture");
    // No launchd mutation and no inherited background child.
    std::fs::write(
        &script,
        r##"#!/bin/sh
dir="${0%/*}"
printf '%s\n' "$1" >> "$dir/calls"
IFS= read -r mode < "$dir/mode"
if [ "$1" = remove ]; then exit 0; fi
# submit -l LABEL -e STDERR -- /bin/sh -c WRAPPER argv0 STATUS LABEL launchctl ditto ...
shift 3
err="$2"
shift 3
st="$5"
case "$mode" in
    complete)
        /usr/bin/head -c 65536 /dev/zero > "$err"
        printf '\ncopy failed: fixture disk full\n' >> "$err"
        printf '1\n' > "$st"
        ;;
    malformed) printf 'not-an-exit-status\n' > "$st" ;;
esac
exit 0
"##,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    // The first exec is not the one under test: this provenance-tracked process
    // just wrote the script, so its first exec waits on syspolicyd's assessment
    // (seconds to over a minute under load) while later execs cost nothing. Run
    // the fixture once here as an unmeasured `remove` and clear its receipt, so
    // the calls a test reads are the runner's alone.
    std::fs::write(staging.root.join("mode"), "assess").unwrap();
    let assessed = std::process::Command::new(&script)
        .arg("remove")
        .stdin(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(
        assessed.success(),
        "the fixture's unbounded first exec: {assessed}"
    );
    std::fs::remove_file(staging.root.join("calls")).unwrap();
    std::fs::write(staging.root.join("mode"), mode).unwrap();
    script
}

#[test]
fn launchd_completion_keeps_only_the_error_tail_and_rejects_malformed_status() {
    let staging = Staging::scratch("launchd-result-tail");
    let launchctl = fake_launchctl(&staging, "complete");
    let (status, stderr) = ditto_via_launchd_using(
        &[],
        "fixture unpack",
        FIXTURE_FINISHES,
        &staging.root,
        &launchctl,
    )
    .unwrap();
    assert!(!status.success());
    assert!(stderr.len() <= 512);
    assert_eq!(
        crate::verify::last_stderr_line(&stderr),
        Some("copy failed: fixture disk full")
    );
    assert!(std::fs::read_dir(&staging.root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".systems.alab.")
    }));
    std::fs::write(staging.root.join("mode"), "malformed").unwrap();
    let error = ditto_via_launchd_using(
        &[],
        "fixture unpack",
        FIXTURE_FINISHES,
        &staging.root,
        &launchctl,
    )
    .unwrap_err();
    assert!(error.contains("malformed exit status"), "{error}");
    assert_eq!(
        std::fs::read_to_string(staging.root.join("calls")).unwrap(),
        "submit\nsubmit\nremove\n"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}
