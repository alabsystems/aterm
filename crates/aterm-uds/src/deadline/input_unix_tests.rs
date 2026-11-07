// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const HANG: Duration = Duration::from_secs(60);

fn shell(script: &str) -> Command {
    let mut command =
        crate::spawnfd::command(super::super::stand_in(std::path::Path::new("/bin/sh")));
    command.args(["-c", script]);
    command
}

#[test]
fn complete_input_keeps_both_outputs_and_the_direct_status() {
    let bytes = vec![b'z'; 256 * 1024];
    let mut cmd = shell("\"$1\"; printf err >&2; exit 7");
    cmd.arg("owned-input")
        .arg(super::super::stand_in(std::path::Path::new("/bin/cat")));
    let out = output_with_input_until(
        &mut cmd,
        Secret::from_vec(bytes.clone()),
        Instant::now() + HANG,
        bytes.len(),
    )
    .expect("complete exchange");
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(out.stdout, bytes);
    assert_eq!(out.stderr, b"err");
}

#[test]
fn scoped_input_capture_preserves_complete_natural_output() {
    let mut cmd = shell("printf out; printf err >&2; exit 7");
    let out = output_with_input_until_watching(
        &mut cmd,
        Secret::from_vec(Vec::new()),
        Instant::now() + HANG,
        16,
        || false,
    )
    .expect("complete capture")
    .expect("not cancelled");
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(out.stdout, b"out");
    assert_eq!(out.stderr, b"err");
}

#[test]
fn streaming_keeps_both_pipes_without_a_total_capture_bound_and_closes_input() {
    let mut cmd = shell(
        "if read value; then exit 91; fi; i=0; while [ $i -lt 256 ]; do printf '%s' \"$1\"; printf '%s' \"$2\" >&2; i=$((i+1)); done; exit 7",
    );
    cmd.arg("owned-stream")
        .arg("x".repeat(8192))
        .arg("y".repeat(8192));
    let spawned = std::cell::Cell::new(None);
    let mut counts = [0usize; 2];
    let hang = Instant::now() + HANG;
    let status = super::super::stream_until_stopped(
        &mut cmd,
        None,
        |pid| spawned.set(Some(pid)),
        |stream, chunk| {
            assert!(
                spawned.get().is_some(),
                "spawn observer precedes every output callback"
            );
            let (index, byte) = match stream {
                ChildOutput::Stdout => (0, b'x'),
                ChildOutput::Stderr => (1, b'y'),
            };
            assert!(chunk.iter().all(|actual| *actual == byte));
            counts[index] += chunk.len();
            Ok(())
        },
        || {
            assert!(Instant::now() < hang, "streaming hang detector");
            false
        },
    )
    .expect("complete streaming")
    .expect("not stopped");
    assert_eq!(status.code(), Some(7));
    assert_eq!(counts, [2 * 1024 * 1024; 2]);
    assert_eq!(
        crate::process::process_presence(spawned.get().unwrap().cast_signed()),
        crate::process::ProcessPresence::Absent
    );
}

#[test]
fn streaming_stop_callback_error_and_unwind_retire_the_actual_owned_root() {
    for fault in 0..4 {
        let pid = std::cell::Cell::new(None);
        let bytes = std::cell::Cell::new(0usize);
        let mut cmd = shell("printf ready; exec \"$1\" 600");
        cmd.arg("owned-stream")
            .arg(super::super::stand_in(std::path::Path::new("/bin/sleep")));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::super::stream_until_stopped(
                &mut cmd,
                Some(Instant::now() + HANG),
                |child| {
                    pid.set(Some(child));
                    assert_ne!(fault, 3, "synthetic spawn observer unwind");
                },
                |stream, chunk| {
                    assert_eq!(stream, ChildOutput::Stdout);
                    bytes.set(bytes.get() + chunk.len());
                    if fault == 1 {
                        return Err(io::ErrorKind::InvalidData.into());
                    }
                    assert_ne!(fault, 2, "synthetic output observer unwind");
                    Ok(())
                },
                || bytes.get() >= 5,
            )
        }));
        assert_eq!(
            crate::process::process_presence(
                pid.get().expect("observed actual child").cast_signed()
            ),
            crate::process::ProcessPresence::Absent
        );
        match fault {
            0 => assert!(result.unwrap().expect("cancelled").is_none()),
            1 => assert_eq!(
                result.unwrap().unwrap_err().kind(),
                io::ErrorKind::InvalidData
            ),
            _ => assert!(result.is_err()),
        }
    }
}

#[test]
fn stopped_or_expired_streaming_never_spawns_or_delivers_output() {
    for expired in [false, true] {
        let mut cmd = crate::spawnfd::command("/no-such-owned-stream-program");
        let result = super::super::stream_until_stopped(
            &mut cmd,
            expired.then(Instant::now),
            |_| panic!("no spawn"),
            |_, _| panic!("no output"),
            || true,
        );
        if expired {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        } else {
            assert!(result.expect("stopped before spawn").is_none());
        }
    }
}

#[test]
fn streaming_deadline_after_output_retires_its_owned_root() {
    let began = Instant::now();
    let deadline = began + HANG;
    let ran = std::rc::Rc::new(std::cell::Cell::new(false));
    let clock_ran = ran.clone();
    super::super::set_deadline_clock_for_test(Some(Box::new(move || {
        if clock_ran.get() || began.elapsed() >= HANG {
            deadline + Duration::from_secs(1)
        } else {
            began
        }
    })));
    struct Clock;
    impl Drop for Clock {
        fn drop(&mut self) {
            super::super::set_deadline_clock_for_test(None);
        }
    }
    let _clock = Clock;
    let pid = std::cell::Cell::new(None);
    let mut cmd = shell("printf ready; exec \"$1\" 600");
    cmd.arg("owned-stream")
        .arg(super::super::stand_in(std::path::Path::new("/bin/sleep")));
    let error = super::super::stream_until_stopped(
        &mut cmd,
        Some(deadline),
        |child| pid.set(Some(child)),
        |_, chunk| {
            assert!(!chunk.is_empty());
            ran.set(true);
            Ok(())
        },
        || false,
    )
    .expect_err("deadline after actual execution");
    assert!(
        ran.get(),
        "the clock expired only after the child supplied output"
    );
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(
        crate::process::process_presence(pid.get().unwrap().cast_signed()),
        crate::process::ProcessPresence::Absent
    );
}

#[test]
fn scoped_input_capture_stops_or_unwinds_only_after_retiring_its_owned_root() {
    for panic in [false, true] {
        let dir = std::env::temp_dir().join(format!(
            "uds-input-stop-{}-{panic}.noindex",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let witness = dir.join("pid");
        let _ = std::fs::remove_file(&witness);
        let mut cmd = shell("printf '%s\\n' \"$$\" > \"$0\"; exec \"$1\" 600");
        cmd.arg(&witness)
            .arg(super::super::stand_in(std::path::Path::new("/bin/sleep")));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            output_with_input_until_watching(
                &mut cmd,
                Secret::from_vec(vec![b'z'; 256 * 1024]),
                Instant::now() + HANG,
                16,
                || {
                    if !std::fs::read_to_string(&witness)
                        .ok()
                        .is_some_and(|line| line.trim().parse::<i32>().is_ok())
                    {
                        return false;
                    }
                    assert!(!panic, "synthetic stop callback unwind");
                    true
                },
            )
        }));
        // The witness is from this actual owned child's successful shell execution.
        let pid = std::fs::read_to_string(&witness)
            .expect("child executed")
            .trim()
            .parse::<i32>()
            .unwrap();
        let presence = crate::process::process_presence(pid);
        std::fs::remove_dir_all(&dir).unwrap();
        if panic {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().expect("stopped capture").is_none());
        }
        assert_eq!(presence, crate::process::ProcessPresence::Absent);
    }
}

#[test]
fn preclosed_input_scope_never_spawns() {
    let mut cmd = crate::spawnfd::command("/no-such-owned-input-program");
    assert!(
        output_with_input_until_watching(
            &mut cmd,
            Secret::from_vec(Vec::new()),
            Instant::now() + HANG,
            16,
            || true,
        )
        .expect("closed before spawn")
        .is_none()
    );
}

#[test]
fn uncertain_input_wait_retains_the_original_caller_lock_until_actual_reap() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    for fault in [false, true] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("caller.lock");
        let lock = crate::file_lock::LockFile::take(&path, Instant::now(), "busy").unwrap();
        let retained = Arc::new(AtomicBool::new(true));
        struct CallerResource(Arc<AtomicBool>);
        impl Drop for CallerResource {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let resource = CallerResource(retained.clone());
        let mut command =
            crate::spawnfd::command(super::super::stand_in(std::path::Path::new("/bin/sleep")));
        let child = command.arg("600").process_group(0).spawn().unwrap();
        let (notice, event) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .spawn(move || {
                let _lock = lock;
                let _resource = resource;
                let mut run = Run {
                    child,
                    input: None,
                    to_child: None,
                    sent: 0,
                    reaped: false,
                };
                let mut calls = 0;
                run.settle_with(|child| {
                    calls += 1;
                    if fault && calls == 1 {
                        return Err(io::ErrorKind::WouldBlock.into());
                    }
                    if fault && calls == 2 {
                        notice.send(false).unwrap();
                        gate.recv_timeout(HANG).unwrap();
                    }
                    child.wait()
                });
                let reaped = run.reaped && run.child.is_reaped();
                drop(run);
                drop(_resource);
                drop(_lock);
                notice.send(true).unwrap();
                reaped
            })
            .unwrap();
        let returned = event.recv_timeout(HANG);
        let resource_retained = retained.load(Ordering::Acquire);
        let locked = crate::file_lock::LockFile::take(&path, Instant::now(), "busy").is_err();
        // Always release the real worker before assertions, including an unexpected early return.
        let _ = release.send(());
        let reaped = worker.join().expect("owned worker joined");
        let returned = returned.expect("wait retry or completion");
        assert_eq!(returned, !fault);
        assert_eq!(resource_retained, fault);
        assert_eq!(locked, fault);
        assert!(reaped, "the successful wait was the actual Child::wait");
        assert!(!retained.load(Ordering::Acquire));
        assert!(crate::file_lock::LockFile::take(&path, Instant::now(), "busy").is_ok());
    }
}

#[test]
fn early_input_close_preserves_the_direct_exit_instead_of_broken_pipe() {
    let mut cmd = shell("exit 67");
    let out = output_with_input_until(
        &mut cmd,
        Secret::from_vec(vec![b'z'; 256 * 1024]),
        Instant::now() + HANG,
        16,
    )
    .expect("early exit");
    assert_eq!(out.status.code(), Some(67));
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}

#[test]
fn neither_complete_output_is_silently_truncated() {
    for command in ["printf 12345", "printf 12345 >&2"] {
        let mut cmd = shell(command);
        let refused = output_with_input_until(
            &mut cmd,
            Secret::from_vec(Vec::new()),
            Instant::now() + HANG,
            4,
        )
        .expect_err("oversized complete output");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidData, "{command}");
    }
}

#[test]
fn an_expired_input_deadline_does_not_spawn() {
    let mut cmd = crate::spawnfd::command("/no-such-owned-input-program");
    let refused = output_with_input_until(
        &mut cmd,
        Secret::from_vec(vec![b'z'; 16]),
        Instant::now(),
        16,
    )
    .expect_err("expired before spawn");
    assert_eq!(refused.kind(), io::ErrorKind::TimedOut);
}

/// The clock crosses the deadline only after the child proves it ran without reading stdin.
/// A full input pipe cannot block this caller, and cleanup retains the unreaped root's authority.
#[test]
fn a_nonreading_child_cannot_hold_secret_input_past_the_deadline() {
    let dir = std::env::temp_dir().join(format!("uds-owned-input-{}.noindex", std::process::id()));
    std::fs::create_dir_all(&dir).expect("owned scratch");
    let witness = dir.join("pid");
    let _ = std::fs::remove_file(&witness);
    let mut cmd = shell("printf '%s\\n' \"$$\" > \"$0\"; exec \"$1\" 600");
    cmd.arg(&witness)
        .arg(super::super::stand_in(std::path::Path::new("/bin/sleep")));
    let began = Instant::now();
    let deadline = began + HANG;
    let path = witness.clone();
    super::super::set_deadline_clock_for_test(Some(Box::new(move || {
        let ran = std::fs::read_to_string(&path)
            .ok()
            .is_some_and(|line| line.trim().parse::<u32>().is_ok());
        if ran || began.elapsed() >= HANG {
            deadline + Duration::from_secs(1)
        } else {
            began
        }
    })));
    struct Clock;
    impl Drop for Clock {
        fn drop(&mut self) {
            super::super::set_deadline_clock_for_test(None);
        }
    }
    let clock = Clock;
    let refused = output_with_input_until(
        &mut cmd,
        Secret::from_vec(vec![b'z'; 8 * 1024 * 1024]),
        deadline,
        16,
    )
    .expect_err("input deadline");
    drop(clock);
    let pid = std::fs::read_to_string(&witness)
        .expect("the child proved execution before the clock crossed its deadline")
        .trim()
        .parse::<i32>()
        .expect("child pid");
    let presence = crate::process::process_presence(pid);
    std::fs::remove_dir_all(&dir).expect("owned scratch removed after execution");
    assert_eq!(refused.kind(), io::ErrorKind::TimedOut);
    assert!(matches!(
        presence,
        crate::process::ProcessPresence::Absent | crate::process::ProcessPresence::Exited
    ));
}
