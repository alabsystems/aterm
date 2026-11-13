// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(unix)]
#[test]
#[ignore = "owned helper child; the native helper parent invokes this exact test"]
fn owned_required_helper_child() {
    let ready = std::env::var_os("ATERM_TEST_UPDATE_HELPER_READY").expect("owned ready path");
    std::fs::write(ready, b"the actual updater helper ran\n").unwrap();
    assert!(
        std::env::var_os("ATERM_TEST_UPDATE_HELPER_REFUSES").is_none(),
        "native helper refusal"
    );
    println!("native helper completed");
}

#[cfg(unix)]
#[test]
fn a_required_update_helper_waits_for_its_native_completion() {
    for (age, refuses, applying) in [
        (31, false, false),
        (61, false, false),
        (31, true, false),
        (31, false, true),
        (61, false, true),
        (31, true, true),
    ] {
        let age = Duration::from_secs(age);
        let expired = age >= aterm_spec::wait::MUST_HAPPEN;
        let started = Instant::now() - age;
        let _budget = applying.then(|| ApplyBudget::start_at(APPLY_BUDGET, started));
        let root = crate::paths::scratch_dir("verify-helper");
        let ready = root.path().join("native-ready");
        let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        child
            .args([
                "verify::tests::owned_required_helper_child",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env("ATERM_TEST_UPDATE_HELPER_READY", &ready);
        if refuses {
            child.env("ATERM_TEST_UPDATE_HELPER_REFUSES", "1");
        }
        let result = output_bounded_started(&mut child, "owned required helper", started);
        if expired {
            let refusal = result.expect_err("an expired hang detector admits no new helper");
            assert!(crate::is_passing_refusal(&refusal));
            assert!(!ready.exists());
            continue;
        }
        let output = result.unwrap_or_else(|why| {
            panic!("native helper rejected at age {age:?}, applying={applying}: {why}")
        });
        assert_eq!(
            std::fs::read(&ready).unwrap(),
            b"the actual updater helper ran\n"
        );
        if refuses {
            assert_eq!(
                output.status.code(),
                Some(101),
                "the native refusal remains a nonzero exit"
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains("native helper refusal"));
        } else {
            assert!(output.status.success());
            assert!(String::from_utf8_lossy(&output.stdout).contains("native helper completed"));
        }
    }
    let root = crate::paths::scratch_dir("verify-helper");
    let ready = root.path().join("outside-apply-budget");
    let _budget = ApplyBudget::start(Duration::ZERO);
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args([
            "verify::tests::owned_required_helper_child",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env("ATERM_TEST_UPDATE_HELPER_READY", &ready);
    let refusal = output_bounded(&mut child, "owned apply helper")
        .expect_err("the original aggregate budget still binds");
    assert!(crate::is_passing_refusal(&refusal));
    assert!(refusal.contains("verification budget"));
    assert!(!ready.exists());
}

/// The aggregate budget binds before a helper's own ceiling (the rejection names
/// the budget), so sixteen serial helpers are bounded by a number.
#[cfg(unix)]
#[test]
fn a_helper_timeout_is_a_passing_refusal() {
    let budget = {
        let _budget = ApplyBudget::start(Duration::from_millis(50));
        output_bounded(Command::new("/bin/sleep").arg("30"), "a slow helper")
            .expect_err("a helper past the budget is a rejection")
    };
    assert!(crate::is_passing_refusal(&budget), "{budget}");
    assert!(budget.contains("verification budget"), "{budget}");
    assert!(
        crate::is_passing_refusal(&timed_out("codesign", false)),
        "the helper's own ceiling is the same moment"
    );
    let copy = status_bounded_with_stderr_observed(
        Command::new("/bin/sleep").arg("30"),
        "ditto",
        Duration::from_millis(100),
    )
    .expect_err("a copy past its limit is a failure")
    .message;
    assert!(crate::is_passing_refusal(&copy), "{copy}");

    let refused = output_bounded(&mut Command::new("/usr/bin/false"), "a refusing helper")
        .expect("the helper ran to an exit");
    assert!(!refused.status.success());
    let verdict = format!(
        "codesign --verify (structural) failed: {}",
        String::from_utf8_lossy(&refused.stderr).trim()
    );
    assert!(!crate::is_passing_refusal(&verdict), "{verdict}");
}

#[test]
fn only_a_moment_s_io_errors_are_passing() {
    for passing in [libc::EAGAIN, libc::ENOMEM, libc::EMFILE, libc::ENFILE] {
        let error = std::io::Error::from_raw_os_error(passing);
        assert!(io_error_is_passing(&error), "{error}");
        assert!(crate::is_passing_refusal(&spawn_refusal(
            "codesign", &error
        )));
    }
    let lock = std::io::Error::new(std::io::ErrorKind::TimedOut, "held for more than 10s");
    assert!(crate::is_passing_refusal(&lock_wait_refusal("lock", &lock)));
    for verdict in [libc::ENOENT, libc::EACCES, libc::ENOLCK, libc::EINVAL] {
        let error = std::io::Error::from_raw_os_error(verdict);
        assert!(!io_error_is_passing(&error), "{error}");
        assert!(!crate::is_passing_refusal(&lock_wait_refusal(
            "lock", &error
        )));
    }
    let missing = output_bounded(
        &mut Command::new("/nonexistent/aterm-update-helper"),
        "a missing helper",
    )
    .expect_err("a helper that does not exist cannot start");
    assert!(!crate::is_passing_refusal(&missing), "{missing}");
}

/// A check-thread helper ends at its own limit. The check's `hdiutil` runs
/// hold the check lane and the machine-wide `checker.lock`, so one that never
/// returned would park the checker: through `output_within` a child past its
/// limit is killed, reaped within a short reserve, and reported; one that
/// finishes is collected.
#[cfg(unix)]
#[test]
fn a_check_helper_past_its_limit_is_killed_and_reported() {
    let error = output_within(
        Command::new("/bin/sleep").arg("30"),
        "sleep",
        Duration::from_millis(300),
    )
    .expect_err("a helper past its limit is a failure");
    assert!(crate::is_passing_refusal(&error), "{error}");
    let out = output_within(
        Command::new("/bin/echo").arg("hi"),
        "echo",
        aterm_spec::wait::MUST_HAPPEN,
    )
    .expect("a helper inside its limit runs");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
}

/// The budget restores whatever was in force when it drops, so a nested or
/// sequential apply cannot inherit a spent deadline.
#[cfg(unix)]
#[test]
fn the_budget_guard_restores_the_previous_deadline() {
    assert!(APPLY_DEADLINE.with(|d| d.get()).is_none());
    {
        let _outer = ApplyBudget::start(Duration::from_secs(60));
        let outer = APPLY_DEADLINE.with(|d| d.get()).expect("outer budget");
        {
            let _inner = ApplyBudget::start(Duration::from_millis(10));
            assert!(APPLY_DEADLINE.with(|d| d.get()).expect("inner") < outer);
        }
        assert_eq!(APPLY_DEADLINE.with(|d| d.get()), Some(outer));
    }
    assert!(APPLY_DEADLINE.with(|d| d.get()).is_none());
}

/// A slow-but-honest copy must not spend the budget the checks after it need.
#[cfg(unix)]
#[test]
fn extending_the_budget_pays_back_disk_time() {
    let _budget = ApplyBudget::start(Duration::from_secs(1));
    let before = APPLY_DEADLINE.with(|d| d.get()).expect("budget");
    ApplyBudget::extend(Duration::from_secs(30));
    let after = APPLY_DEADLINE.with(|d| d.get()).expect("budget");
    assert!(after >= before + Duration::from_secs(29));
}

#[test]
fn helper_cleanup_requires_an_observed_exit_and_never_renews_the_deadline() {
    use std::os::unix::process::ExitStatusExt;
    let model = aterm_spec::ty_model! {
        BoundedHelperCleanup {
            const Buggy = 0;
            var killed = 0;
            var observed = 0;
            var stopped = 0;
            action Kill when (killed == 0) {
                killed = 1;
                stopped = if Buggy == 1 { 1 } else { 0 };
            }
            action ObserveExit when (killed == 1 && observed == 0) {
                observed = 1;
                stopped = 1;
            }
            action LoseWait when (killed == 1 && observed == 0) { observed = 2; }
            invariant StopRequiresExit: stopped == 0 || observed == 1;
        }
    };
    aterm_spec::verify::prove_and_catch_scalar(&model, "bounded helper cleanup");
    struct FakeChild {
        exit: u8,
        kill_error: bool,
        kills: usize,
        deadline: Instant,
        late: usize,
    }
    impl HelperChild for FakeChild {
        fn request_kill(&mut self) -> std::io::Result<()> {
            self.kills += 1;
            if self.kill_error {
                Err(std::io::Error::other("kill refused"))
            } else {
                Ok(())
            }
        }
        fn observe_exit(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
            match self.exit {
                1 => Ok(Some(std::process::ExitStatus::from_raw(0))),
                2 => Err(std::io::Error::other("wait failed")),
                _ => {
                    if Instant::now() >= self.deadline {
                        self.late += 1;
                    }
                    if self.late > 1 {
                        return Err(std::io::Error::other("still looked at past the deadline"));
                    }
                    Ok(None)
                }
            }
        }
    }
    for exit in 0..=2 {
        for kill_error in [false, true] {
            let started = Instant::now();
            let deadline = started + Duration::from_millis(15);
            let mut child = FakeChild {
                exit,
                kill_error,
                kills: 0,
                deadline,
                late: 0,
            };
            let stopped = stop_helper_before(&mut child, deadline);
            assert_eq!(child.kills, 1);
            assert!(
                child.late <= 1,
                "cleanup must not wait unboundedly after a failed kill: {} looks past \
                 its deadline",
                child.late
            );
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "cleanup must not wait unboundedly after a failed kill: {:?}",
                started.elapsed()
            );
            let mut state = model.successors("Kill", &model.init_state())[0].clone();
            if exit > 0 {
                let action = if exit == 1 { "ObserveExit" } else { "LoseWait" };
                state = model.successors(action, &state)[0].clone();
            }
            assert_eq!(state["stopped"], i64::from(stopped));
            assert!(model.check_invariant("StopRequiresExit", &state));
            if exit != 1 {
                state.insert("stopped", 1);
                assert!(!model.check_invariant("StopRequiresExit", &state));
            }
        }
    }
}

/// A helper that outlives its limit is refused as a timeout, and a failure that says its
/// writer stopped names a helper already reaped, so the copy path it wrote is free to reclaim:
/// its pid, taken as it started, names no process, not even a zombie. A spawn refused starts
/// none. Whether the stop lands inside the call's cleanup reserve (250 ms at most) is the
/// Mac's load, not a fact this test can hold: a stop the call did not see is its safe answer
/// (the copy stays isolated), and [`stop_helper_before_reaps_the_helper_it_reports_stopped`]
/// holds the stop itself under the hang detector.
///
/// NEGATIVE CONTROL (2026-11-06): `stop_helper_before` answering stopped without its kill
/// fails `assert_gone` ("pid N still exists").
#[test]
fn timed_out_owned_helper_is_reaped_before_its_copy_path_is_reclaimable() {
    let staging = crate::paths::Staging::scratch("owned-helper-reap");
    HELPER_STARTED.with(|started| started.set(None));
    let failure = status_bounded_with_stderr_observed(
        Command::new("/bin/sleep").arg("30"),
        "owned copy fixture",
        Duration::from_secs(2),
    )
    .unwrap_err();
    assert!(crate::is_passing_refusal(&failure.message), "{failure:?}");
    let pid = HELPER_STARTED
        .with(std::cell::Cell::take)
        .expect("the helper started");
    if failure.writer_stopped {
        assert_gone(i32::try_from(pid).expect("a pid fits an i32"));
    }
    let absent = status_bounded_with_stderr_observed(
        &mut Command::new(staging.root.join("no-such-helper")),
        "absent helper",
        Duration::from_secs(2),
    )
    .unwrap_err();
    assert!(absent.writer_stopped, "a refused spawn created no writer");
    assert_eq!(HELPER_STARTED.with(std::cell::Cell::get), None);
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// The stop of a real helper that will not finish: it is killed and reaped, so the process it
/// reports stopped is gone, judged by the pid of the child this test spawned itself. Its
/// deadline is the one-minute hang detector, not the call's cleanup reserve.
///
/// NEGATIVE CONTROL (2026-11-06): `stop_helper_before` answering stopped without its kill
/// fails `assert_gone` ("pid N still exists").
#[test]
fn stop_helper_before_reaps_the_helper_it_reports_stopped() {
    let mut child = Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn /bin/sleep");
    let pid = i32::try_from(child.id()).expect("a pid fits an i32");
    let deadline = crate::instant_after(Instant::now(), aterm_spec::wait::MUST_HAPPEN);
    assert!(
        stop_helper_before(&mut child, deadline),
        "a killed helper exits within the hang detector"
    );
    assert_gone(pid);
}

/// `pid` names no process, a zombie included: it was reaped.
fn assert_gone(pid: i32) {
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "pid {pid} still exists");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

/// The tail of the child's stderr comes back with its status; a child that floods stderr
/// is drained past the pipe's capacity, and only its last bytes are kept.
#[cfg(unix)]
#[test]
fn status_bounded_keeps_the_tail_of_stderr_without_wedging_on_a_flood() {
    let (status, stderr) = status_bounded_with_stderr_observed(
        Command::new("/bin/sh")
            .arg("-c")
            .arg("echo 'first line' >&2; echo 'ditto: No space left on device' >&2; exit 1"),
        "a copy that fails",
        Duration::from_secs(60),
    )
    .expect("the child ran");
    assert!(!status.success());
    assert_eq!(
        last_stderr_line(&stderr),
        Some("ditto: No space left on device"),
        "{stderr:?}"
    );

    let (status, stderr) = status_bounded_with_stderr_observed(
        Command::new("/bin/sh")
            .arg("-c")
            .arg("i=0; while [ $i -lt 2048 ]; do echo 'a line of stderr noise, sixty-four bytes long, give or take..' >&2; i=$((i+1)); done; echo 'the last line' >&2"),
        "a chatty copy",
        Duration::from_secs(60),
    )
    .expect("a chatty child must not wedge the poll loop");
    assert!(status.success());
    assert!(
        stderr.len() <= STDERR_KEEP,
        "only the tail is kept: {}",
        stderr.len()
    );
    assert_eq!(last_stderr_line(&stderr), Some("the last line"));

    let (status, stderr) = status_bounded_with_stderr_observed(
        &mut Command::new("/usr/bin/true"),
        "a quiet copy",
        Duration::from_secs(60),
    )
    .expect("ran");
    assert!(status.success() && stderr.is_empty() && last_stderr_line(&stderr).is_none());
}

#[test]
fn signing_identifier_parsed_from_codesign_output() {
    let sample = "Executable=/x\nIdentifier=com.aterm.aterm\nTeamIdentifier=ABCDE12345\n";
    assert_eq!(parse_signing_identifier(sample).unwrap(), "com.aterm.aterm");
    let reordered = "TeamIdentifier=ABCDE12345\nIdentifier=com.aterm.aterm.dev\n";
    assert_eq!(
        parse_signing_identifier(reordered).unwrap(),
        "com.aterm.aterm.dev"
    );
    assert!(parse_signing_identifier("TeamIdentifier=ABCDE12345\n").is_err());
    assert!(parse_signing_identifier("Identifier=\n").is_err());
}

#[test]
fn team_id_is_read_from_codesign_output_and_fails_closed_on_adhoc_or_missing() {
    let sample =
        "Executable=/x\nIdentifier=com.aterm.aterm\nTeamIdentifier=ABCDE12345\nSealed Resources\n";
    assert_eq!(parse_team_id(sample).unwrap(), "ABCDE12345");
    assert!(parse_team_id("TeamIdentifier=not set\n").is_err());
    assert!(parse_team_id("Identifier=com.aterm.aterm\n").is_err());
    assert!(parse_team_id("TeamIdentifier=\n").is_err());
}

#[test]
fn verify_bundle_refuses_a_garbage_pin_and_another_signer() {
    verify_bundle(Path::new("/nonexistent"), "AB\"CD").unwrap_err();
    aterm_uds::deadline::require_trust_service();
    let refused = verify_bundle(Path::new("/bin/ls"), "ABCDE12345").unwrap_err();
    assert!(!crate::is_passing_refusal(&refused), "{refused}");
}

#[test]
fn only_a_codesign_moment_is_passing() {
    use aterm_update_core::codesign::CodesignError;
    let refused = |code: Option<i32>, stderr: &str| CodesignError::Refused {
        code,
        stderr: stderr.to_string(),
    };
    for passing in [
        refused(None, ""),
        refused(None, "aterm.app: code object is not signed at all\n"),
        refused(
            Some(1),
            "aterm.app: internal error in Code Signing subsystem\n",
        ),
        refused(Some(1), "aterm.app: Permission denied\n"),
        refused(Some(1), "aterm.app: Too many open files\n"),
        refused(
            Some(1),
            "aterm.app: the code cannot be read by the verifier (file system permissions \
             etc.)\n",
        ),
        CodesignError::Wait(std::io::Error::from_raw_os_error(libc::EINTR)),
    ] {
        let pinned = codesign_refusal(passing, Path::new("aterm.app"), false);
        assert!(crate::is_passing_refusal(&pinned), "{pinned}");
    }
    let structural = codesign_exit_refusal(
        "codesign --verify (structural)",
        Path::new("aterm.app"),
        refused(
            Some(1),
            "aterm.app: internal error in Code Signing subsystem\n",
        ),
    );
    assert!(crate::is_passing_refusal(&structural), "{structural}");

    let verdicts = [
        (
            Some(3),
            "aterm.app: code failed to satisfy specified code requirement(s)\n",
        ),
        (Some(1), "aterm.app: code object is not signed at all\n"),
        (
            Some(1),
            "aterm.app: invalid signature (code or signature have been modified)\n",
        ),
        (
            Some(1),
            "aterm.app: bundle format unrecognized, invalid, or unsuitable\n",
        ),
        (
            Some(1),
            "aterm.app: a sealed resource is missing or invalid\nfile modified: \
             /Applications/aterm.app/Contents/Resources/aterm-handoff-policy.toml\n",
        ),
        (
            Some(1),
            "aterm.app: a sealed resource is missing or invalid\nfile added: \
             /Applications/aterm.app/Contents/Resources/x: Permission denied\n",
        ),
        (
            Some(1),
            "aterm.app: invalid Info.plist (plist or signature have been modified)\n\
             In architecture: arm64e\n",
        ),
        (
            Some(1),
            "aterm.app: invalid resource directory (directory or signature have been \
             modified)\n",
        ),
        (
            Some(1),
            "aterm.app: unsealed contents present in the bundle root\n",
        ),
        (Some(1), "aterm.app: nested code is modified or invalid\n"),
        (Some(1), "aterm.app: a verdict nobody has measured yet\n"),
        (Some(1), "aterm.app: No such file or directory\n"),
        (
            Some(1),
            "aterm.app: invalid destination for symbolic link in bundle\nfile modified: \
             /Applications/aterm.app/Contents/Resources/h: Permission denied\n",
        ),
        (
            Some(1),
            "--prepared:aterm.app/Contents/Helpers/h\naterm.app: Permission denied\n\
             aterm.app: nested code is modified or invalid\n",
        ),
        (Some(1), ""),
        (Some(2), "codesign: unrecognized option\n"),
    ];
    for (code, stderr) in verdicts {
        let line = codesign_refusal(refused(code, stderr), Path::new("aterm.app"), false);
        assert!(!crate::is_passing_refusal(&line), "{line}");
        let structural = codesign_exit_refusal(
            "codesign --verify (structural)",
            Path::new("aterm.app"),
            refused(code, stderr),
        );
        assert!(!crate::is_passing_refusal(&structural), "{structural}");
    }
}

#[test]
fn a_verdict_that_quotes_a_refusal_key_is_still_a_verdict() {
    use aterm_update_core::codesign::CodesignError;
    use std::os::unix::process::ExitStatusExt as _;
    let exited = std::process::ExitStatus::from_raw(1 << 8);
    let bundle = aterm_tempfile::Builder::new()
        .prefix("aterm-quoted-key-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let app = bundle.path().join("aterm.app");
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    std::fs::write(app.join("Contents/MacOS/aterm"), "").unwrap();
    for key in [
        crate::PASSING_REFUSAL_KEY,
        crate::INSTALL_LOCATION_REFUSAL_KEY,
        crate::ROSTER_UNCONFIRMED_KEY,
        crate::NOT_INSTALLED_KEY,
        crate::install::ROLLBACK_SOURCE_REFUSAL_KEY,
        crate::github::STALE_ANCHOR_KEY,
    ] {
        let said = format!("/x/Outer.app/Contents/Helpers/q: {key}.app");
        let output = |code: i32, stdout: &str| std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: format!("{said}\n").into_bytes(),
        };
        let refusals = [
            codesign_exit_refusal(
                "codesign --verify",
                Path::new("Outer.app"),
                CodesignError::Refused {
                    code: Some(1),
                    stderr: format!(
                        "Outer.app: main executable failed strict validation\n\
                         In subcomponent: {said}\n"
                    ),
                },
            ),
            spctl_verdict(exited, &said).unwrap_err(),
            plist_read_refusal("CFBundleVersion", &output(1, "")),
            probe_starts(&app, 9_999_999, |_| Ok(output(1, ""))).unwrap_err(),
            probe_starts(&app, 9_999_999, |_| Ok(output(0, &said))).unwrap_err(),
        ];
        for refusal in refusals {
            assert!(!refusal.contains(key), "{key}: {refusal}");
            assert!(
                refusal.contains("Helpers/q: "),
                "the quote reads: {refusal}"
            );
            assert!(!crate::is_passing_refusal(&refusal), "{refusal}");
            assert!(!crate::is_install_location_refusal(&refusal), "{refusal}");
            assert!(
                !crate::install::is_rollback_source_refusal(&refusal),
                "{refusal}"
            );
            assert!(
                !crate::github::is_stale_anchor_refusal(&refusal),
                "{refusal}"
            );
            assert!(!crate::refusal_needs_person(&refusal), "{refusal}");
        }
    }
    let moment = codesign_exit_refusal(
        "codesign --verify",
        Path::new("Outer.app"),
        CodesignError::Refused {
            code: None,
            stderr: String::new(),
        },
    );
    assert!(crate::is_passing_refusal(&moment), "{moment}");
}

/// A start probe whose candidate a signal ended never saw it exit: a verdict on the
/// build that names the signal ([`START_PROBE_SIGNALLED`]), so its candidate stays where
/// it ran (`install::probe_left_unproven`); one that exited with a failure is a plain
/// verdict.
#[test]
fn a_signal_ending_the_start_probe_is_a_verdict_that_says_so() {
    use std::os::unix::process::ExitStatusExt as _;
    let bundle = aterm_tempfile::Builder::new()
        .prefix("aterm-probe-signal-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let app = bundle.path().join("aterm.app");
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    std::fs::write(app.join("Contents/MacOS/aterm"), "").unwrap();
    let output = |status| std::process::Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    let killed = probe_starts(&app, 1, |_| {
        Ok(output(std::process::ExitStatus::from_raw(libc::SIGKILL)))
    })
    .unwrap_err();
    assert!(!crate::is_passing_refusal(&killed), "{killed}");
    assert!(killed.starts_with(START_PROBE_SIGNALLED), "{killed}");
    let exited = probe_starts(&app, 1, |_| {
        Ok(output(std::process::ExitStatus::from_raw(1 << 8)))
    })
    .unwrap_err();
    assert!(!crate::is_passing_refusal(&exited), "{exited}");
    assert!(!exited.starts_with(START_PROBE_SIGNALLED), "{exited}");
}

/// A tampered bundle named like a machine fault is a verdict, measured on the real
/// codesign through the structural lane on ad hoc signed `.app`s: a dangling symlink
/// added after signing (codesign's whole word is `<app>: No such file or
/// directory`); a symlink out of the bundle named `h: Permission denied`, read from
/// stderr or both streams; and nested helpers named `h: Permission denied`,
/// `h\nX: Permission denied` and after the passing key, with a sibling re-signed
/// after the seal, which codesign names on `--verbose=2` progress lines before its
/// verdict.
#[cfg(target_os = "macos")]
#[test]
fn a_tampered_bundle_named_like_a_machine_fault_is_a_verdict() {
    use std::os::unix::fs::PermissionsExt as _;
    aterm_uds::deadline::require_trust_service();
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-codesign-tamper-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let unsigned = |name: &str| unsigned_app(root.path(), name);
    let verdict = |app: &Path, words: &str| {
        let refusal = verify_bundle_policy(app, "").expect_err(words);
        assert!(refusal.contains(words), "{refusal}");
        assert!(!crate::is_passing_refusal(&refusal), "{refusal}");
    };

    let clean = unsigned("Clean.app");
    aterm_uds::deadline::sign_ad_hoc(&clean);
    assert_eq!(verify_bundle_policy(&clean, ""), Ok(()));

    let dangling = unsigned("Dangling.app");
    aterm_uds::deadline::sign_ad_hoc(&dangling);
    std::os::unix::fs::symlink(
        "/nonexistent/aterm-dangling",
        dangling.join("Contents/Resources/dangling"),
    )
    .unwrap();
    verdict(&dangling, ": No such file or directory");

    let named = unsigned("Named.app");
    std::os::unix::fs::symlink(
        "/etc/hosts",
        named.join("Contents/Resources/h: Permission denied"),
    )
    .unwrap();
    aterm_uds::deadline::sign_ad_hoc(&named);
    verdict(&named, "invalid destination for symbolic link in bundle");
    let out = output_bounded(&mut structural_command(&named), STRUCTURAL).unwrap();
    let both = aterm_update_core::codesign::CodesignError::Refused {
        code: out.status.code(),
        stderr: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        ),
    };
    assert!(!both.judged_nothing_at(&named), "{both:?}");

    let nested = unsigned("Nested.app");
    let helpers = nested.join("Contents/Helpers");
    std::fs::create_dir_all(&helpers).unwrap();
    for name in [
        "h: Permission denied".to_string(),
        "h\nX: Permission denied".to_string(),
        format!("q: {}", crate::PASSING_REFUSAL_KEY),
        "z".to_string(),
    ] {
        std::fs::copy("/bin/ls", helpers.join(&name)).unwrap();
        aterm_uds::deadline::sign_ad_hoc(&helpers.join(name));
    }
    aterm_uds::deadline::sign_ad_hoc(&nested);
    let resigned = output_bounded(
        aterm_uds::spawnfd::command("/usr/bin/codesign")
            .args(["--force", "--sign", "-", "--identifier", "other"])
            .arg(helpers.join("z")),
        "codesign --sign (ad hoc)",
    )
    .unwrap();
    assert!(resigned.status.success(), "{resigned:?}");
    verdict(&nested, "nested code is modified or invalid");

    let unreadable = unsigned("Unreadable.app");
    aterm_uds::deadline::sign_ad_hoc(&unreadable);
    let r = unreadable.join("Contents/Resources/r");
    std::fs::set_permissions(&r, std::fs::Permissions::from_mode(0o000)).unwrap();
    let premise = std::fs::read(&r).is_err();
    let moment = verify_bundle_policy(&unreadable, "");
    std::fs::set_permissions(&r, std::fs::Permissions::from_mode(0o644)).unwrap();
    if premise {
        let refusal = moment.expect_err("an unreadable sealed resource");
        assert!(crate::is_passing_refusal(&refusal), "{refusal}");
    }
}

/// A minimal unsigned `.app` at `root/name`: `/bin/ls` as its main executable `A`, an
/// `Info.plist` naming it, and one resource, `Contents/Resources/r`.
#[cfg(target_os = "macos")]
fn unsigned_app(root: &Path, name: &str) -> std::path::PathBuf {
    let app = root.join(name);
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
    std::fs::copy("/bin/ls", app.join("Contents/MacOS/A")).unwrap();
    std::fs::write(
        app.join("Contents/Info.plist"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
         <key>CFBundleExecutable</key><string>A</string>\
         <key>CFBundleIdentifier</key><string>t.a</string></dict></plist>\n",
    )
    .unwrap();
    std::fs::write(app.join("Contents/Resources/r"), "x\n").unwrap();
    app
}

/// A bundle codesign cannot read to an answer is a verdict, refused before any
/// helper runs.
#[cfg(target_os = "macos")]
#[test]
fn a_bundle_codesign_cannot_read_to_an_answer_is_a_verdict() {
    aterm_uds::deadline::require_trust_service();
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-codesign-fifo-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let fifo = |at: &Path| {
        let _ = std::fs::remove_file(at);
        let path = std::ffi::CString::new(at.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `path` is a valid NUL-terminated path for the call's duration.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o644) }, 0, "{at:?}");
    };
    let signed = |name: &str| {
        let app = unsigned_app(root.path(), name);
        aterm_uds::deadline::sign_ad_hoc(&app);
        app
    };
    let clean = signed("Clean.app");
    assert_eq!(verify_bundle_policy(&clean, ""), Ok(()));
    let inside = unsigned_app(root.path(), "Inside.app");
    std::fs::create_dir(inside.join("Contents/Resources/real")).unwrap();
    std::os::unix::fs::symlink("real", inside.join("Contents/Resources/link")).unwrap();
    aterm_uds::deadline::sign_ad_hoc(&inside);
    assert_eq!(verify_bundle_policy(&inside, ""), Ok(()));

    let seal = signed("Seal.app");
    fifo(&seal.join("Contents/_CodeSignature/CodeResources"));
    let exe = signed("Exe.app");
    fifo(&exe.join("Contents/MacOS/A"));
    let outside = root.path().join("outside");
    fifo(&outside);
    let linked = signed("Linked.app");
    std::fs::remove_file(linked.join("Contents/Resources/r")).unwrap();
    std::os::unix::fs::symlink(&outside, linked.join("Contents/Resources/r")).unwrap();
    let named = signed("Named.app");
    let key = crate::PASSING_REFUSAL_KEY;
    fifo(&named.join(format!("Contents/Resources/q: {key}")));
    let moved_out = |name: &str, dir: &str, at: &str| {
        let app = signed(name);
        let out = root.path().join(format!("{name}.out"));
        std::fs::rename(app.join(dir), &out).unwrap();
        std::os::unix::fs::symlink(&out, app.join(dir)).unwrap();
        fifo(&out.join(at));
        app
    };
    let contents = moved_out("Contents.app", "Contents", "_CodeSignature/CodeResources");
    let macos = moved_out("MacOS.app", "Contents/MacOS", "A");
    let sealed = moved_out("Sealed.app", "Contents/_CodeSignature", "CodeResources");

    let _budget = ApplyBudget::start(Duration::from_secs(5));
    let verdict = |refusal: &str| {
        refusal.contains("which no signed bundle holds") && !crate::is_passing_refusal(refusal)
    };
    for (app, team) in [
        (&seal, ""),
        (&seal, "ABCDE12345"),
        (&exe, ""),
        (&linked, ""),
        (&named, ""),
        (&named, "ABCDE12345"),
        (&contents, ""),
        (&macos, ""),
        (&sealed, "ABCDE12345"),
    ] {
        let refusal = verify_bundle_policy(app, team).expect_err("a bundle it cannot read");
        assert!(verdict(&refusal), "{app:?} {team:?}: {refusal}");
    }
    let checks = check_bundle(&seal, "", true);
    assert!(
        checks.build.as_ref().is_err_and(|why| verdict(why)),
        "{:?}",
        checks.build
    );
}

/// A bundle check runs several helpers on the bundle, and a signal ending the
/// team id's (`codesign -d`), Gatekeeper's (`spctl`) or a sealed read's
/// (`PlistBuddy`) is as much a moment as one ending the first, and so is a wait
/// on any of them that failed.
#[cfg(unix)]
#[test]
fn a_signal_ending_a_bundle_check_helper_is_no_verdict() {
    use std::os::unix::process::ExitStatusExt as _;
    let killed = std::process::ExitStatus::from_raw(libc::SIGKILL);
    let rejected = std::process::ExitStatus::from_raw(3 << 8);
    let spctl = spctl_verdict(killed, "").expect_err("a killed spctl did not pass");
    assert!(crate::is_passing_refusal(&spctl), "{spctl}");
    let spctl = spctl_verdict(rejected, "aterm.app: rejected\nsource=Unnotarized")
        .expect_err("a rejection");
    assert!(!crate::is_passing_refusal(&spctl), "{spctl}");
    let plist = |status| std::process::Output {
        status,
        stdout: Vec::new(),
        stderr: b"Print: Entry Does Not Exist\n".to_vec(),
    };
    let reads = |status| {
        [
            build_number_from(plist(status)).map(|_| ()),
            short_version_from(plist(status)).map(|_| ()),
            git_commit_from(plist(status)).map(|_| ()),
        ]
    };
    for read in reads(killed) {
        let refusal = read.expect_err("a killed read did not pass");
        assert!(crate::is_passing_refusal(&refusal), "{refusal}");
    }
    let exited = std::process::ExitStatus::from_raw(1 << 8);
    for read in reads(exited) {
        let refusal = read.expect_err("a failed read");
        assert!(!crate::is_passing_refusal(&refusal), "{refusal}");
    }
    let wait = run_refusal(
        "PlistBuddy CFBundleVersion",
        RunError::Wait(std::io::Error::from_raw_os_error(libc::EINTR)),
        String::new(),
    );
    assert!(crate::is_passing_refusal(&wait), "{wait}");

    aterm_uds::deadline::require_trust_service();
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-team-id-moment-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let exe = root.path().join("aterm.app/Contents/MacOS/aterm");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::copy("/bin/ls", &exe).unwrap();
    std::fs::write(
        root.path().join("aterm.app/Contents/Info.plist"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
         <key>CFBundleExecutable</key><string>aterm</string></dict></plist>\n",
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o000)).unwrap();
    let premise = std::fs::read(&exe).is_err();
    let unread = team_id(&root.path().join("aterm.app"));
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let refusal = team_id(&root.path().join("aterm.app")).expect_err("no team id");
    assert!(!crate::is_passing_refusal(&refusal), "{refusal}");
    if !premise {
        aterm_uds::host::could_not_run(
            "a mode-0 executable read anyway: root, or a file system that ignores modes",
        );
    }
    let refusal = unread.expect_err("an unreadable bundle has no team id");
    assert!(crate::is_passing_refusal(&refusal), "{refusal}");
}

/// An ad-hoc-signed `aterm.app` under `root` whose Info.plist carries `plist`
/// (the `<dict>` body) and whose executable is a script exiting 0.
#[cfg(target_os = "macos")]
fn ad_hoc_bundle(root: &Path, plist: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let app = root.join("aterm.app");
    let exe = app.join("Contents/MacOS/aterm");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        app.join("Contents/Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleExecutable</key><string>aterm</string>\
             <key>CFBundleIdentifier</key><string>com.aterm.fixture</string>\
             {plist}</dict></plist>\n"
        ),
    )
    .unwrap();
    aterm_uds::deadline::sign_ad_hoc(&app);
    app
}

/// Every helper of a bundle check answers for itself, and the verdict is the
/// serial one.
#[cfg(target_os = "macos")]
#[test]
fn each_helper_of_a_bundle_check_answers_for_itself_and_the_verdict_is_the_serial_one() {
    aterm_uds::deadline::require_trust_service();
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-check-bundle-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let app = ad_hoc_bundle(
        root.path(),
        "<key>CFBundleVersion</key><string>777</string>\
         <key>ATermGitCommit</key><string>0123456789abcdef0123456789abcdef01234567</string>\
         <key>CFBundleShortVersionString</key><string>1.2.3</string>",
    );
    let checks = check_bundle(&app, "", true);
    assert_eq!(checks.policy, Ok(()));
    assert_eq!(checks.build, Ok(777));
    assert_eq!(
        checks.commit.as_deref(),
        Ok("0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(checks.short_version, Some(Ok("1.2.3".to_string())));
    assert!(checks.team.is_none(), "the structural tier reads no team");
    let pinned = check_bundle(&app, "ABCDE12345", false);
    let policy = pinned.policy.expect_err("an ad-hoc bundle fails the pin");
    assert!(
        policy.starts_with("codesign --verify (team-pinned requirement) failed: "),
        "{policy}"
    );
    assert!(!crate::is_passing_refusal(&policy), "{policy}");
    let team = pinned.team.expect("a pinned check reads the team");
    assert!(
        team.expect_err("ad hoc has no team")
            .contains("TeamIdentifier not set"),
        "the team helper ran and answered"
    );
    assert_eq!(pinned.build, Ok(777));
    assert_eq!(pinned.short_version, None, "not asked for");
    let unreadable = ad_hoc_bundle(
        &root.path().join("unreadable"),
        "<key>CFBundleVersion</key><string>seven</string>",
    );
    let checks = check_bundle(&unreadable, "", false);
    assert_eq!(checks.policy, Ok(()));
    let build = checks.build.expect_err("not an integer");
    assert!(build.contains("is not an integer"), "{build}");
    assert!(
        checks
            .commit
            .expect_err("absent")
            .starts_with("read ATermGitCommit")
    );
}

/// Several bundles checked at once each get their own answers. `check_bundles`
/// starts every bundle's helpers together (the boot apply's staged and installed
/// bundles); each bundle's answers are its own helpers', in the order given, and
/// the same as `check_bundle` of it alone: a broken seal refuses only its own
/// bundle, and its reads still answer.
#[cfg(target_os = "macos")]
#[test]
fn several_bundles_checked_at_once_each_get_their_own_answers() {
    aterm_uds::deadline::require_trust_service();
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-check-bundles-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let plist = |build: u32| {
        format!(
            "<key>CFBundleVersion</key><string>{build}</string>\
             <key>ATermGitCommit</key><string>0123456789abcdef0123456789abcdef0123456{}</string>\
             <key>CFBundleShortVersionString</key><string>1.2.{build}</string>",
            build % 10
        )
    };
    let a = ad_hoc_bundle(&root.path().join("a"), &plist(777));
    let broken = ad_hoc_bundle(&root.path().join("broken"), &plist(779));
    std::fs::write(broken.join("Contents/MacOS/aterm"), "#!/bin/sh\nexit 1\n").unwrap();
    let b = ad_hoc_bundle(&root.path().join("b"), &plist(778));
    let apps = [a.as_path(), broken.as_path(), b.as_path()];
    let together = check_bundles(&apps, "", true);
    assert_eq!(together.len(), 3);
    for (app, checks) in apps.iter().zip(&together) {
        let alone = check_bundle(app, "", true);
        assert_eq!(checks.policy.is_ok(), alone.policy.is_ok(), "{app:?}");
        assert_eq!(checks.build, alone.build, "{app:?}");
        assert_eq!(checks.commit, alone.commit, "{app:?}");
        assert_eq!(checks.short_version, alone.short_version, "{app:?}");
    }
    assert_eq!(together[0].policy, Ok(()));
    assert_eq!(together[0].build, Ok(777));
    let seal = together[1].policy.as_ref().expect_err("a broken seal");
    assert!(!crate::is_passing_refusal(seal), "{seal}");
    assert_eq!(together[1].build, Ok(779), "its reads still answer");
    assert_eq!(together[2].policy, Ok(()));
    assert_eq!(together[2].build, Ok(778));
    assert_eq!(
        together[2].commit.as_deref(),
        Ok("0123456789abcdef0123456789abcdef01234568")
    );
    for checks in check_bundles(&[a.as_path(), b.as_path()], "AB\"CD", false) {
        assert!(
            checks.policy.is_err() && checks.build.is_err(),
            "{checks:?}"
        );
    }
}

/// An expired bundle-check budget returns passing refusals naming the budget.
/// The budget is already spent before the bundle walk ([`unanswerable_entry`])
/// can classify the unwritten Info.plist FIFO. No helper is dispatched to open
/// it, and the check returns without waiting for a writer.
#[cfg(target_os = "macos")]
#[test]
fn a_bundle_check_helper_that_never_answers_is_a_passing_refusal_at_the_budget() {
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-check-bundle-fifo-")
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let app = root.path().join("aterm.app");
    std::fs::create_dir_all(app.join("Contents")).unwrap();
    let plist = std::ffi::CString::new(
        app.join("Contents/Info.plist")
            .into_os_string()
            .into_encoded_bytes(),
    )
    .unwrap();
    // SAFETY: a NUL-terminated path that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(plist.as_ptr(), 0o600) }, 0);
    let begun = Instant::now();
    let checks = {
        let _budget = ApplyBudget::start(Duration::ZERO);
        check_bundle(&app, "", false)
    };
    assert!(
        begun.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "the check did not come back"
    );
    for read in [checks.build.map(drop), checks.commit.map(drop)] {
        let refusal = read.expect_err("a read that never answered is a refusal");
        assert!(refusal.contains("verification budget"), "{refusal}");
        assert!(crate::is_passing_refusal(&refusal), "{refusal}");
    }
}

/// A helper that prints more than a pipe holds is read while it runs, so it
/// ends and is collected rather than stalling against the wait until its
/// deadline.
#[cfg(unix)]
#[test]
fn a_helper_printing_more_than_a_pipe_holds_is_collected() {
    let out = output_bounded(
        Command::new("/bin/sh")
            .arg("-c")
            .arg("head -c 300000 /dev/zero; head -c 300000 /dev/zero >&2"),
        "a chatty helper",
    )
    .expect("a chatty helper runs to its exit");
    assert!(out.status.success());
    assert_eq!((out.stdout.len(), out.stderr.len()), (300_000, 300_000));
}
