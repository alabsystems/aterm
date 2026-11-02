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
