// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use aterm_spec::wait::MUST_HAPPEN;
use aterm_uds::spawnfd::Stdio;

const FIXTURE: &str = "ATERM_RUNTIME_CAPTURE_FIXTURE";
const LATE_STDOUT: &[u8] = b"late stdout\0\xff\n";
const LATE_STDERR: &[u8] = b"late stderr\0\xfe\n";

fn until() -> Instant {
    Instant::now() + MUST_HAPPEN
}

fn wait_for(path: &Path) {
    let deadline = until();
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn fixture(mode: &str, root: &Path) -> Command {
    let mut cmd = command(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        "runtime_capture::tests::producer_fixture",
        "--nocapture",
    ])
    .env(FIXTURE, mode)
    .env("ATERM_RUNTIME_CAPTURE_ROOT", root);
    cmd
}

#[test]
fn producer_fixture() {
    let Some(mode) = std::env::var_os(FIXTURE) else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("ATERM_RUNTIME_CAPTURE_ROOT").unwrap());
    match mode.to_str().unwrap() {
        "producer" => {
            let _writer = fixture("writer", &root)
                .process_group(0)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            wait_for(&root.join("writer-ready"));
            std::io::stdout().write_all(b"producer stdout\n").unwrap();
            std::io::stderr().write_all(b"producer stderr\n").unwrap();
            std::process::exit(1);
        }
        "writer" => {
            std::fs::write(root.join("writer-ready"), b"ready").unwrap();
            wait_for(&root.join("release-writer"));
            if std::env::var_os("ATERM_RUNTIME_CAPTURE_OVERFLOW").as_deref()
                == Some(std::ffi::OsStr::new("1"))
            {
                let _ = std::io::stdout().write_all(&vec![b'x'; 4097]);
                std::process::exit(0);
            }
            std::io::stdout().write_all(LATE_STDOUT).unwrap();
            std::io::stderr().write_all(LATE_STDERR).unwrap();
            std::process::exit(0);
        }
        "signal" => aterm_uds::process::kill_self_now(),
        "ready-exit" => {
            std::io::stdout().write_all(b"ready-batch-exit\n").unwrap();
            wait_for(&root.join("allow-exit"));
            std::process::exit(1);
        }
        "live" => {
            std::io::stdout()
                .write_all(b"live-producer-ready\n")
                .unwrap();
            wait_for(&root.join("allow-exit"));
            std::process::exit(0);
        }
        _ => panic!("unknown fixture mode"),
    }
}

struct ReleaseWriter(PathBuf);

impl ReleaseWriter {
    fn release(&self) {
        std::fs::write(&self.0, b"release").unwrap();
    }
}

impl Drop for ReleaseWriter {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, b"release");
    }
}

#[test]
fn direct_exit_does_not_seal_output_held_by_a_gated_descendant() {
    gated_descendant(false);
}

#[test]
fn failed_direct_exit_survives_a_gated_descendants_later_output_error() {
    gated_descendant(true);
}

fn gated_descendant(overflow: bool) {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let directory = tmp.path().join("phase");
    std::fs::create_dir(&directory).unwrap();
    let mut producer = fixture("producer", tmp.path());
    producer.env(
        "ATERM_RUNTIME_CAPTURE_OVERFLOW",
        if overflow { "1" } else { "0" },
    );
    let direct_gone = Arc::new(AtomicBool::new(false));
    let (observed, observation) = mpsc::channel();
    let (completed, completion) = mpsc::channel();
    std::thread::scope(|scope| {
        let release = ReleaseWriter(tmp.path().join("release-writer"));
        let gone = Arc::clone(&direct_gone);
        let naturally_exited = Arc::clone(&direct_gone);
        let directory = &directory;
        scope.spawn(move || {
            let mut noted = false;
            let result = capture(
                &mut producer,
                directory,
                until(),
                4096,
                || {
                    if gone.load(Ordering::SeqCst) && !noted {
                        observed.send(()).unwrap();
                        noted = true;
                    }
                    false
                },
                |status| {
                    assert_eq!(status.code(), Some(1));
                    naturally_exited.store(true, Ordering::SeqCst);
                },
            );
            completed.send(result).unwrap();
        });
        // The primitive has observed the actual direct exit with WNOWAIT and reached
        // its drain loop while the descendant still holds both pipes behind the gate.
        observation.recv_timeout(MUST_HAPPEN).unwrap();
        assert!(matches!(
            completion.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(!directory.join("capture.json").exists());

        // Negative control: the old cat-of-a-regular-file protocol returns EOF here,
        // while a proven exited producer still has a descendant holding both pipes.
        let early_stdout = std::fs::read(directory.join("stdout")).unwrap();
        let early_stderr = std::fs::read(directory.join("stderr")).unwrap();
        assert!(
            !early_stdout
                .windows(LATE_STDOUT.len())
                .any(|bytes| bytes == LATE_STDOUT)
        );
        assert!(
            !early_stderr
                .windows(LATE_STDERR.len())
                .any(|bytes| bytes == LATE_STDERR)
        );
        release.release();
        let receipt = completion.recv_timeout(MUST_HAPPEN).unwrap().unwrap();
        assert_eq!(receipt.complete, !overflow);
        assert_eq!((receipt.exit, receipt.signal), (Some(1), None));
        let stdout = std::fs::read(directory.join("stdout")).unwrap();
        let stderr = std::fs::read(directory.join("stderr")).unwrap();
        if overflow {
            assert!(receipt.error.as_ref().unwrap().contains("stdout exceeds"));
            assert!(stdout.len() <= 4096);
        } else {
            assert!(stdout.ends_with(LATE_STDOUT));
            assert!(stderr.ends_with(LATE_STDERR));
            assert_ne!(stdout, early_stdout);
            assert_ne!(stderr, early_stderr);
        }
        assert_eq!(receipt.stdout_bytes, stdout.len());
        assert_eq!(receipt.stderr_bytes, stderr.len());
        let published: aterm_json::Value =
            aterm_json::from_slice(&std::fs::read(directory.join("capture.json")).unwrap())
                .unwrap();
        assert_eq!(published["complete"].as_bool(), Some(!overflow));
        assert_eq!(published["exit"].as_i64(), Some(1));
    });
}

fn emitter(stdout: &Path, stderr: &Path) -> Command {
    let mut cmd = command(aterm_uds::deadline::stand_in(Path::new("/bin/sh")));
    cmd.args(["-c", r#""$1" "$2"; "$1" "$3" >&2; exit 1"#, "producer"])
        .arg(aterm_uds::deadline::stand_in(Path::new("/bin/cat")))
        .arg(stdout)
        .arg(stderr);
    cmd
}

#[test]
fn either_output_cap_refuses_completion_and_keeps_bounded_prefixes() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let exact = tmp.path().join("exact");
    let oversized = tmp.path().join("oversized");
    let empty = tmp.path().join("empty");
    let maximum = 4096;
    let bytes: Vec<_> = (0_u8..=255).cycle().take(maximum).collect();
    std::fs::write(&exact, &bytes).unwrap();
    std::fs::write(&oversized, [bytes.as_slice(), b"!"].concat()).unwrap();
    std::fs::write(&empty, b"").unwrap();
    for (name, stdout, stderr, complete, stream) in [
        ("exact", &exact, &exact, true, ""),
        ("stdout", &oversized, &empty, false, "stdout"),
        ("stderr", &empty, &oversized, false, "stderr"),
    ] {
        let directory = tmp.path().join(name).with_extension("phase");
        std::fs::create_dir(&directory).unwrap();
        let receipt = capture(
            &mut emitter(stdout, stderr),
            &directory,
            until(),
            maximum,
            || false,
            |_| {},
        )
        .unwrap();
        assert_eq!(receipt.complete, complete);
        assert!(receipt.stdout_bytes <= maximum && receipt.stderr_bytes <= maximum);
        if complete {
            assert_eq!((receipt.exit, receipt.signal), (Some(1), None));
            assert_eq!(std::fs::read(directory.join("stdout")).unwrap(), bytes);
            assert_eq!(std::fs::read(directory.join("stderr")).unwrap(), bytes);
        } else {
            assert!(receipt.error.as_ref().unwrap().contains(stream));
            assert_eq!(receipt.driver_exit(), crate::exit::COULD_NOT_RUN as u8);
        }
        assert!(directory.join("capture.json").is_file());
    }
}

#[test]
fn direct_signal_is_retained_and_cancellation_never_claims_eof() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    for (name, cancel) in [("signal", false), ("cancelled", true)] {
        let directory = tmp.path().join(name);
        std::fs::create_dir(&directory).unwrap();
        let receipt = capture(
            &mut fixture("signal", tmp.path()),
            &directory,
            until(),
            MAX_COMPLETE_STDOUT_BYTES,
            || cancel,
            |_| {},
        )
        .unwrap();
        assert_eq!(receipt.complete, !cancel);
        if cancel {
            assert_eq!((receipt.exit, receipt.signal), (None, None));
            assert!(receipt.error.is_some());
        } else {
            assert_eq!((receipt.exit, receipt.signal), (None, Some(libc::SIGKILL)));
            assert_eq!(receipt.driver_exit(), 128 + libc::SIGKILL as u8);
        }
    }
}

#[test]
fn ended_driver_returns_refusal_and_retains_partial_output_and_diagnostics() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("started"), b"started").unwrap();
    std::fs::write(tmp.path().join("stdout"), b"partial stdout\0\xff").unwrap();
    std::fs::write(tmp.path().join("stderr"), b"partial stderr\0\xfe").unwrap();
    std::fs::write(
        tmp.path().join("driver.stderr"),
        b"cannot publish receipt\n",
    )
    .unwrap();
    let mut observations = 0;
    let mut pauses = 0;
    let mut diagnostics = Vec::new();
    let bytes = await_capture(
        tmp.path(),
        until(),
        || {
            observations += 1;
            Ok(DriverState {
                ended: observations == 2,
                description: b"owned job: not running, runs=1\n".to_vec(),
            })
        },
        || pauses += 1,
        &mut diagnostics,
    )
    .unwrap();
    assert_eq!(observations, 2);
    assert_eq!(pauses, 1);
    let receipt: aterm_json::Value = aterm_json::from_slice(&bytes).unwrap();
    assert_eq!(receipt["complete"].as_bool(), Some(false));
    assert!(receipt["exit"].is_null() && receipt["signal"].is_null());
    assert_eq!(receipt["stdout_bytes"].as_u64(), Some(16));
    assert_eq!(receipt["stderr_bytes"].as_u64(), Some(16));
    assert_eq!(
        std::fs::read(tmp.path().join("stdout")).unwrap(),
        b"partial stdout\0\xff"
    );
    assert_eq!(
        std::fs::read(tmp.path().join("stderr")).unwrap(),
        b"partial stderr\0\xfe"
    );
    assert!(diagnostics.ends_with(b"cannot publish receipt\n"));
    assert!(diagnostics.starts_with(b"owned job: not running"));
    assert!(
        !tmp.path().join("capture.json").exists(),
        "a refusal is not a replacement completion artifact"
    );
}

#[test]
fn a_real_completion_published_during_observation_wins_over_driver_ended() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let receipt = Receipt {
        schema: "aterm-runtime-capture-v1",
        complete: true,
        exit: Some(0),
        signal: None,
        stdout_bytes: 0,
        stderr_bytes: 0,
        error: None,
    };
    let bytes = await_capture(
        tmp.path(),
        until(),
        || {
            publish(tmp.path(), &receipt).unwrap();
            Ok(DriverState {
                ended: true,
                description: Vec::new(),
            })
        },
        || panic!("a published completion does not wait"),
        &mut Vec::new(),
    )
    .unwrap();
    let result: aterm_json::Value = aterm_json::from_slice(&bytes).unwrap();
    assert_eq!(result["complete"].as_bool(), Some(true));
}

#[test]
fn ended_requires_own_top_level_launchd_state_and_a_completed_run() {
    let target = "gui/501/systems.alab.aterm.runtime.take-1";
    let print = |body| format!("{target} = {{\n{body}}}\n");
    assert!(driver_ended(
        target,
        &print("\tstate = not running\n\truns = 1\n\tlast exit code = 3\n")
    ));
    assert!(driver_ended(
        target,
        &print("\tstate = not running\n\truns = 1\n\tlast terminating signal = 9\n")
    ));
    assert!(!driver_ended(
        target,
        &print("\tstate = not running\n\truns = 0\n")
    ));
    assert!(!driver_ended(
        target,
        &print("\tstate = not running\n\truns = 1\n\tpid = unread\n")
    ));
    assert!(!driver_ended(
        target,
        &print(
            "\tstate = running\n\truns = 1\n\tpid = 42\n\tsection = {\n\t\tstate = not running\n\t}\n"
        )
    ));
    assert!(!driver_ended(
        "gui/501/another-job",
        &print("\tstate = not running\n\truns = 1\n")
    ));
}

#[test]
fn publication_cannot_replace_an_existing_completion() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let stale = b"existing completion evidence\n";
    std::fs::write(tmp.path().join("capture.json"), stale).unwrap();
    let receipt = Receipt {
        schema: "aterm-runtime-capture-v1",
        complete: true,
        exit: Some(0),
        signal: None,
        stdout_bytes: 0,
        stderr_bytes: 0,
        error: None,
    };
    assert_eq!(
        publish(tmp.path(), &receipt).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        std::fs::read(tmp.path().join("capture.json")).unwrap(),
        stale
    );
}

#[test]
fn completion_published_before_an_observation_error_is_retained() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let receipt = Receipt {
        schema: "aterm-runtime-capture-v1",
        complete: true,
        exit: Some(1),
        signal: None,
        stdout_bytes: 0,
        stderr_bytes: 0,
        error: None,
    };
    let mut diagnostics = Vec::new();
    let bytes = await_capture(
        tmp.path(),
        until(),
        || {
            publish(tmp.path(), &receipt).unwrap();
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "injected observation error after publication",
            ))
        },
        || panic!("a published completion does not wait"),
        &mut diagnostics,
    )
    .unwrap();
    let result: aterm_json::Value = aterm_json::from_slice(&bytes).unwrap();
    assert_eq!(result["complete"].as_bool(), Some(true));
    assert_eq!(result["exit"].as_i64(), Some(1));
    assert!(diagnostics.is_empty());
}

fn wait_for_terminal_status(pid: u32) -> i32 {
    let deadline = until();
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: pid comes from this invocation's actual spawned callback. The producer
        // remains owned and unreaped by the streaming primitive while this output callback
        // runs; WNOWAIT consumes neither status nor that identity reservation.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        if result == 0 {
            // SAFETY: waitid initialized this zeroed record on success.
            let info = unsafe { info.assume_init() };
            if info.si_signo != 0 {
                assert_eq!(info.si_signo, libc::SIGCHLD);
                assert_eq!(info.si_code, libc::CLD_EXITED);
                // SAFETY: SIGCHLD selects the initialized pid/status arms.
                assert_eq!(unsafe { info.si_pid() }, pid as i32);
                // SAFETY: the initialized SIGCHLD terminal status is this owned child's.
                return unsafe { info.si_status() };
            }
        } else {
            assert_eq!(
                io::Error::last_os_error().kind(),
                io::ErrorKind::Interrupted
            );
        }
        assert!(Instant::now() < deadline, "released producer did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_ready_batch_error_observes_natural_exit_before_cleanup() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let pid = std::cell::Cell::new(0);
    let observed = std::cell::Cell::new(None);
    let mut stdout = Vec::new();
    let error = stream_until_stopped_observing_exit(
        &mut fixture("ready-exit", tmp.path()),
        Some(until()),
        |child| pid.set(child),
        |stream, bytes| {
            if matches!(stream, ChildOutput::Stdout) {
                stdout.extend_from_slice(bytes);
                if stdout
                    .windows(b"ready-batch-exit\n".len())
                    .any(|bytes| bytes == b"ready-batch-exit\n")
                {
                    std::fs::write(tmp.path().join("allow-exit"), b"release")?;
                    assert_eq!(wait_for_terminal_status(pid.get()), 1);
                    // Still in the same ready callback: no loop-top observation can run
                    // between the real terminal exit above and this injected error.
                    return Err(io::Error::other("injected ready-batch output error"));
                }
            }
            Ok(())
        },
        || false,
        |status| observed.set(Some(status)),
    )
    .unwrap_err();
    assert!(error.to_string().contains("ready-batch output error"));
    assert_eq!(observed.get().and_then(|status| status.code()), Some(1));
}

#[test]
fn cleanup_of_a_live_producer_never_reports_its_induced_exit() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let observed = std::cell::Cell::new(None);
    let mut stdout = Vec::new();
    let error = stream_until_stopped_observing_exit(
        &mut fixture("live", tmp.path()),
        Some(until()),
        |_| {},
        |stream, bytes| {
            if matches!(stream, ChildOutput::Stdout) {
                stdout.extend_from_slice(bytes);
                if stdout
                    .windows(b"live-producer-ready\n".len())
                    .any(|bytes| bytes == b"live-producer-ready\n")
                {
                    // The actual producer reached its gate and has no released exit path.
                    // Only the primitive's retirement can end it after this error.
                    return Err(io::Error::other("injected live-producer output error"));
                }
            }
            Ok(())
        },
        || false,
        |status| observed.set(Some(status)),
    )
    .unwrap_err();
    assert!(error.to_string().contains("live-producer output error"));
    assert!(
        observed.get().is_none(),
        "cleanup SIGPIPE/SIGKILL is not a producer verdict"
    );
    assert!(!tmp.path().join("allow-exit").exists());
}
