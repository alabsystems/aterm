// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn an_expired_original_budget_starts_nothing() {
    let world = aterm_tempfile::tempdir().unwrap();
    let marker = world.path().join("must-not-exist");
    let mut command = aterm_uds::spawnfd::command("/usr/bin/touch");
    command.arg(&marker);
    let error = supervise_controller_until(&mut command, Instant::now(), &mut |_| {}).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(!marker.exists());
    assert!(!command.starts_suspended());
}

#[test]
fn a_normal_controller_retires_and_restores_its_command_policy() {
    let world = aterm_tempfile::tempdir().unwrap();
    let marker = world.path().join("ran");
    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    command
        .args(["-c", "printf '%s\\n' \"$$\" > \"$1\"", "controller-proof"])
        .arg(&marker);
    let result = supervise_controller_until(
        &mut command,
        Instant::now() + Duration::from_secs(60),
        &mut |_| {},
    )
    .unwrap();
    assert!(result.root.success());
    assert_eq!(result.signal, None);
    assert!(!command.starts_suspended());
    let group: u32 = std::fs::read_to_string(marker)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        aterm_uds::process::group_presence(group),
        aterm_uds::process::ProcessPresence::Absent
    );
}

#[test]
fn budget_cannot_expire_between_native_capture_and_target_go() {
    use std::cell::Cell;
    use std::os::unix::process::ExitStatusExt as _;
    let mut command = aterm_uds::spawnfd::command("/bin/sleep");
    command.arg("60").default_signals();
    let deadline = Instant::now() + Duration::from_secs(60);
    let observations = Cell::new(0);
    // The first observation admits the attempt. The next observation models
    // capture consuming the SAME original budget, while the actual dedicated
    // child is still stopped before target code. No operation deadline moves.
    let now = || {
        let observation = observations.get() + 1;
        observations.set(observation);
        if observation >= 2 {
            deadline
        } else {
            Instant::now()
        }
    };
    let error = supervise_with(&mut command, deadline, &mut |_| {}, &now).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    let retired = error
        .get_ref()
        .unwrap()
        .downcast_ref::<ControllerDeadline>()
        .unwrap();
    assert_eq!(retired.root_status().signal(), Some(libc::SIGKILL));
    assert!(!command.starts_suspended());
}

fn stubborn_controller(
    world: &std::path::Path,
    root_exits: bool,
) -> (Command, std::path::PathBuf, std::path::PathBuf) {
    let root_ran = world.join("root-ran");
    let member_ran = world.join("member-ran");
    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    command
        .args([
            "-c",
            "trap '' TERM; exec 3<&0; /bin/sh -c 'trap \"\" TERM; printf \"%s\\n\" \"$$\" > \"$1\"; exec /bin/cat <&3' member \"$2\" & while test ! -f \"$2\"; do /bin/sleep 0.01; done; printf '%s\\n' \"$$\" > \"$1\"; if test \"$3\" = exit; then exit 0; fi; while :; do /bin/sleep 1; done",
            "stubborn-controller",
        ])
        .arg(&root_ran)
        .arg(&member_ran)
        .arg(if root_exits { "exit" } else { "stay" })
        .stdin(aterm_uds::spawnfd::Stdio::piped());
    (command, root_ran, member_ran)
}

fn expire_with_actual_stubborn_member(root_exits: bool) {
    let world = aterm_tempfile::tempdir().unwrap();
    let (mut command, root_ran, member_ran) = stubborn_controller(world.path(), root_exits);
    let deadline = Instant::now() + Duration::from_secs(60);
    // Deterministic elapsed-budget fault after BOTH actual processes executed.
    // The original deadline never changes; this clock cannot create a child,
    // resource owner, positive Root result or SDK completion.
    let now = || {
        let exited = !root_exits
            || std::fs::read_to_string(&root_ran)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
                .is_some_and(|pid| {
                    aterm_uds::process::process_presence(pid)
                        == aterm_uds::process::ProcessPresence::Exited
                });
        if root_ran.is_file() && member_ran.is_file() && exited {
            deadline
        } else {
            Instant::now()
        }
    };
    let error = supervise_with(&mut command, deadline, &mut |_| {}, &now).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    let retired = error
        .get_ref()
        .unwrap()
        .downcast_ref::<ControllerDeadline>()
        .unwrap();
    assert_eq!(retired.root_status().success(), root_exits);
    let group: u32 = std::fs::read_to_string(&root_ran)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        aterm_uds::process::group_presence(group),
        aterm_uds::process::ProcessPresence::Absent
    );
    let member: i32 = std::fs::read_to_string(&member_ran)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(matches!(
        aterm_uds::process::process_presence(member),
        aterm_uds::process::ProcessPresence::Exited | aterm_uds::process::ProcessPresence::Absent
    ));
    assert!(!command.starts_suspended());
}

#[test]
fn deadline_cancels_an_actual_stubborn_controller_and_descendant() {
    expire_with_actual_stubborn_member(false);
}

#[test]
fn a_zero_exit_leader_does_not_hide_its_stubborn_descendant_after_deadline() {
    expire_with_actual_stubborn_member(true);
}

const UNWIND_WORLD: &str = "ATPKG_CONTROLLER_UNWIND_WORLD";

#[test]
fn unwind_actor_if_requested() {
    let Some(world) = std::env::var_os(UNWIND_WORLD) else {
        return;
    };
    let world = std::path::PathBuf::from(world);
    let (mut command, root_ran, member_ran) = stubborn_controller(&world, false);
    let deadline = Instant::now() + Duration::from_secs(60);
    let now = || {
        if root_ran.is_file() && member_ran.is_file() {
            panic!("actual controller unwind fault");
        }
        Instant::now()
    };
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        supervise_with(&mut command, deadline, &mut |_| {}, &now)
    }));
    assert!(unwound.is_err());
    let group: u32 = std::fs::read_to_string(&root_ran)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        aterm_uds::process::group_presence(group),
        aterm_uds::process::ProcessPresence::Absent
    );
    assert!(!command.starts_suspended());
    std::fs::write(
        world.join("retired"),
        b"actual unwind and native group retirement",
    )
    .unwrap();
}

#[test]
fn unwinding_keeps_original_custody_until_actual_group_retirement() {
    let world = aterm_tempfile::tempdir().unwrap();
    // The deliberate panic poisons the actor's actual signal-forwarding mutex.
    // Keep that global disposition state isolated from this test process and
    // the independent epoch controls, without weakening their lock checks.
    let output = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "build_admit::controller::until::tests::unwind_actor_if_requested",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(UNWIND_WORLD, world.path())
        .setsid(true)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated native unwind actor failed: {output:?}"
    );
    assert!(world.path().join("retired").is_file());
    let group: u32 = std::fs::read_to_string(world.path().join("root-ran"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        aterm_uds::process::group_presence(group),
        aterm_uds::process::ProcessPresence::Absent
    );
    // A caught actor panic must leave this process's original forwarding turn
    // usable; the full suite found five later epoch controls otherwise refused.
    let _turn = crate::platform::FORWARDING_TURN.lock().unwrap();
}

const CAPTURE_SIGNAL_WORLD: &str = "ATPKG_CONTROLLER_CAPTURE_SIGNAL_WORLD";

#[test]
fn queued_capture_signal_actor_if_requested() {
    use std::os::unix::process::ExitStatusExt as _;
    let Some(world) = std::env::var_os(CAPTURE_SIGNAL_WORLD) else {
        return;
    };
    let world = std::path::PathBuf::from(world);
    // SAFETY: this test actor is re-executed alone. Its actual SIGTERM must
    // reach the front rather than inherit a shell's ignored disposition.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
    }
    let mut command = aterm_uds::spawnfd::command("/usr/bin/touch");
    command.arg(world.join("must-not-run")).default_signals();
    let mut first = true;
    let mut capture = |spawned: &mut Spawned, front: &Front| {
        if first {
            first = false;
            // SAFETY: signal this actor itself, with its real front armed.
            assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGTERM) }, 0);
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "actual cancellation queued before capture retry",
                || (front.observed() == Some(libc::SIGTERM)).then_some(()),
            );
            return Err(io::Error::other("injected first native capture refusal"));
        }
        // The original operation budget still has a full extra minute: this
        // is a hang detector for missing cancellation, not a latency budget.
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "stopped original child retired after queued cancellation",
            || spawned.child_mut().exited().unwrap().then_some(()),
        );
        let cohort = Cohort::of_spawned(spawned)?;
        let status = spawned.child_mut().wait()?;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        Ok(cohort)
    };
    let result = supervise_with_capture(
        &mut command,
        Instant::now() + Duration::from_secs(120),
        &mut |_| {},
        &Instant::now,
        &mut capture,
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert!(!world.join("must-not-run").exists());
    std::fs::write(
        world.join("retired"),
        b"actual queued SIGTERM, no GO, actual SIGKILL and native group retirement",
    )
    .unwrap();
}

#[test]
fn queued_signal_during_capture_retries_retires_without_go() {
    let world = aterm_tempfile::tempdir().unwrap();
    let output = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "build_admit::controller::until::tests::queued_capture_signal_actor_if_requested",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CAPTURE_SIGNAL_WORLD, world.path())
        .setsid(true)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated native signal actor failed: {output:?}"
    );
    assert!(world.path().join("retired").is_file());
    assert!(!world.path().join("must-not-run").exists());
}
