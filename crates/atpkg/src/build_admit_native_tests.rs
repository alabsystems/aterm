// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual native supervisor controls over private injected admission.

use super::*;
use std::{io::Write as _, os::fd::AsRawFd as _};

#[cfg(target_os = "macos")]
#[test]
fn cleanup_refuses_to_signal_a_different_birth_at_an_owned_childs_pid() {
    use aterm_uds::spawnfd::{self, Stdio};
    let mut child = spawnfd::command("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = aterm_uds::deadline::cat_echoes(&mut child);
    let start = aterm_uds::process::read_process_start(i32::try_from(child.id()).unwrap()).unwrap();
    let sent = signal_observed(
        (child.id(), start.boot.0.checked_add(1).unwrap()),
        libc::SIGKILL,
    );
    drop(input);
    let status = child.wait().unwrap();
    assert!(
        !sent,
        "a different birth does not authorize signalling this child"
    );
    assert!(status.success(), "the actual child exits by EOF: {status}");
}

#[cfg(target_os = "macos")]
#[test]
fn cleanup_distinguishes_an_accepted_signal_from_a_kernel_refusal() {
    use aterm_uds::spawnfd::{self, Stdio};
    use std::os::unix::process::ExitStatusExt as _;
    for (signal, accepted) in [(i32::MAX, false), (libc::SIGTERM, true)] {
        // The cat must die of the SIGTERM it is sent: it starts with SIGTERM at its default, set
        // here, not this test binary's (a `nohup` or `&` runner's ignored one, which every exec
        // keeps).
        let start = crate::platform::Starting::at_defaults(&[libc::SIGTERM]);
        let mut child = start.spawning(|| {
            spawnfd::command("/bin/cat")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap()
        });
        drop(start);
        let input = aterm_uds::deadline::cat_echoes(&mut child);
        let start =
            aterm_uds::process::read_process_start(i32::try_from(child.id()).unwrap()).unwrap();
        let sent = signal_observed((child.id(), start.boot.0), signal);
        drop(input);
        let status = child.wait().unwrap();
        assert_eq!(sent, accepted);
        if accepted {
            assert_eq!(status.signal(), Some(signal));
        } else {
            assert!(
                status.success(),
                "the refused request leaves EOF to end the child"
            );
        }
    }
    assert!(
        !signal_observed((0, 0), 0),
        "a non-positive target supplies no signal receipt"
    );
}

#[cfg(target_os = "macos")]
const NO_FRONT_WORLD: &str = "ATPKG_NATIVE_NO_FRONT_WORLD";

#[cfg(target_os = "macos")]
#[test]
fn no_forwarder_actor_if_requested() {
    use std::os::unix::process::ExitStatusExt as _;
    let Some(world) = std::env::var_os(NO_FRONT_WORLD) else {
        return;
    };
    let world = PathBuf::from(world);
    // SIGTERM at its default action, whatever this test binary was started with: the root this
    // actor sends one must die of it, and a shell started ignoring it can neither trap nor die of
    // it.
    // SAFETY: SIG_DFL is a disposition, no handler of ours; this actor runs alone.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
    }
    let machine = machine();
    let seat = seat(&world, &machine, std::process::id().wrapping_add(94));
    let Admission::Claimed(held) = admit(&test_build(), &world, &seat, &mut |_| {}) else {
        panic!("expected a private claim");
    };
    let mut command = aterm_uds::spawnfd::transparent("/bin/sh").into_command();
    command.args(["-c", "exec 3<&0; (trap '' TERM; exec /bin/cat <&3) & printf '%s\\n' \"$!\" > \"$ATPKG_NATIVE_NO_FRONT_WORLD/left\"; wait"]);
    let status = supervise_with(
        Some(*held), &mut command, &mut |_| {}, || None,
        |child, meter| {
            let pid = aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "leftover started", || {
                std::fs::read_to_string(world.join("left")).ok()?.trim().parse::<u32>().ok()
            });
            let meter = meter.expect("actual admitted work is metered");
            aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "live leftover booked by meter", || {
                meter.read();
                matches!(meter.left_observed(), aterm_capacity::tree_meter::Left::Read(ref members) if members.iter().any(|&(seen, _)| seen == pid)).then_some(())
            });
            std::fs::write(world.join("observed"), "ready").unwrap();
            aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "parent requests root termination", || world.join("end-root").exists().then_some(()));
            // SAFETY: child is this actor's actual owned, unreaped child, pinning its pid.
            assert_eq!(unsafe { libc::kill(i32::try_from(child.id()).unwrap(), libc::SIGTERM) }, 0);
        },
    ).unwrap();
    assert_eq!(status.signal(), Some(libc::SIGTERM));
}

#[cfg(target_os = "macos")]
#[test]
fn cleanup_without_a_forwarder_ends_observed_work_before_releasing_the_claim() {
    use aterm_uds::{
        process::{self, ProcessPresence},
        spawnfd::{self, Child, Stdio},
    };
    struct Actor {
        child: Child,
        world: PathBuf,
    }
    impl Drop for Actor {
        fn drop(&mut self) {
            let _ = std::fs::write(self.world.join("end-root"), "go");
            self.child.stdin.take();
            if matches!(self.child.exited(), Ok(false)) {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
        }
    }
    let world = aterm_tempfile::tempdir().unwrap();
    let mut actor = Actor {
        child: spawnfd::command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "build_admit::tests::native::no_forwarder_actor_if_requested",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(NO_FRONT_WORLD, world.path())
            .env(TREE_END_GRACE_ENV, "0")
            .setsid(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
        world: world.path().to_owned(),
    };
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "actual leftover observed",
        || world.path().join("observed").exists().then_some(()),
    );
    let pid: i32 = std::fs::read_to_string(world.path().join("left"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let claims_before = Ledger::at(world.path().join("ledger"))
        .list()
        .unwrap()
        .len();
    std::fs::write(world.path().join("end-root"), "go").unwrap();
    let status = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "frontless supervisor ends",
        || actor.child.try_wait().unwrap(),
    );
    let leftover = process::process_presence(pid);
    // Keep stdin open until the observation: only cleanup, not EOF, can have ended this cat.
    actor.child.stdin.take();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "fixture leftover ends",
        || {
            matches!(
                process::process_presence(pid),
                ProcessPresence::Absent | ProcessPresence::Exited
            )
            .then_some(())
        },
    );
    let claims_after = Ledger::at(world.path().join("ledger"))
        .list()
        .unwrap()
        .len();
    assert!(status.success());
    assert_eq!(claims_before, 1);
    assert_eq!(claims_after, 0);
    assert!(
        matches!(leftover, ProcessPresence::Absent | ProcessPresence::Exited),
        "observed work still runs after claim release: {leftover:?}"
    );
}

fn jobserver_style_handle() -> std::io::PipeReader {
    let (reader, mut writer) = std::io::pipe().unwrap();
    writer.write_all(b"shared-jobserver-handle\n").unwrap();
    drop(writer);
    // SAFETY: the pipe reader is owned here and remains open through the native child wait.
    // Only its close-on-exec flag changes, so the actual child must inherit this handle.
    let flags = unsafe {
        let flags = libc::fcntl(reader.as_raw_fd(), libc::F_GETFD);
        assert!(flags >= 0);
        assert_eq!(
            libc::fcntl(reader.as_raw_fd(), libc::F_SETFD, flags & !libc::FD_CLOEXEC),
            0
        );
        flags
    };
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    reader
}

#[test]
fn native_supervision_keeps_inherited_jobserver_style_handles_and_command_context() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let machine = machine();
    let seat = seat(dir.path(), &machine, std::process::id().wrapping_add(91));
    let Admission::Claimed(held) = admit(&test_build(), dir.path(), &seat, &mut |_| {}) else {
        panic!("expected a private claim");
    };
    let reader = jobserver_style_handle();
    let output = dir.path().join("native-build-output");
    let mut command = aterm_uds::spawnfd::transparent("/bin/sh").into_command();
    command
        .args([
            "-c",
            &format!(
                "printf '%s|%s\\n' \"$1\" \"$ATPKG_NATIVE_CONTEXT\"; /bin/pwd -P; /bin/cat /dev/fd/{}",
                reader.as_raw_fd()
            ),
            "native-build",
            "argument",
        ])
        .env("ATPKG_NATIVE_CONTEXT", "preserved")
        .current_dir(dir.path())
        .stdout(std::fs::File::create(&output).unwrap());
    let status = supervise(Some(*held), &mut command, &mut |_| {}).unwrap();
    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(output).unwrap(),
        format!(
            "argument|preserved\n{}\nshared-jobserver-handle\n",
            dir.path().canonicalize().unwrap().display()
        )
    );
    assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
}

#[test]
fn native_spawn_failure_releases_only_the_unused_ordinary_claim() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let machine = machine();
    let seat = seat(dir.path(), &machine, std::process::id().wrapping_add(92));
    let Admission::Claimed(held) = admit(&test_build(), dir.path(), &seat, &mut |_| {}) else {
        panic!("expected a private claim");
    };
    let mut command =
        aterm_uds::spawnfd::transparent(dir.path().join("missing-native-build")).into_command();
    let mut said = Vec::new();
    let error = supervise(Some(*held), &mut command, &mut |line| {
        said.push(line.to_owned());
    })
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
    assert!(
        Records::load(&dir.path().join("targo-memory.json"))
            .rows()
            .is_empty()
    );
    assert!(said.is_empty());
}

#[test]
fn the_actual_claimed_launch_port_preserves_the_inherited_handle() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = aterm_tempfile::tempdir().unwrap();
    let machine = machine();
    let seat = seat(dir.path(), &machine, std::process::id().wrapping_add(93));
    let reader = jobserver_style_handle();
    let target = dir.path().join("targo-probe");
    let output = dir.path().join("launch-port-output");
    std::fs::write(
        &target,
        format!(
            "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$1\" \"$2\" \"$ATPKG_NATIVE_CONTEXT\" > \"$ATPKG_NATIVE_OUTPUT\"\n/bin/cat /dev/fd/{} >> \"$ATPKG_NATIVE_OUTPUT\"\n",
            reader.as_raw_fd()
        ),
    ).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
    let env = [
        ("ATPKG_NATIVE_CONTEXT".into(), "preserved".into()),
        (
            "ATPKG_NATIVE_OUTPUT".into(),
            output.clone().into_os_string(),
        ),
    ];
    let status = run_if_claimed_at(
        &target,
        &words(&["--unverified", "build"]),
        Tool::Targo,
        &env,
        dir.path(),
        &seat,
    )
    .expect("a compiling call claims and runs");
    assert_eq!(status, std::process::ExitCode::SUCCESS);
    assert_eq!(
        std::fs::read_to_string(output).unwrap(),
        "--unverified|build|preserved\nshared-jobserver-handle\n"
    );
    assert!(seat.ledger.as_ref().unwrap().list().unwrap().is_empty());
}
