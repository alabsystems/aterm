// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::fd::FromRawFd as _;
use std::os::unix::process::ExitStatusExt as _;
const ACTOR: &str = "metadata_prelude::tests::native_prelude_actor";
const DELAYED_ACTOR: &str = "metadata_prelude::tests::native_delayed_prelude_actor";
const TARGET: &str = "metadata_prelude::tests::native_no_fork_target";
const REPLACEMENT: &str = "metadata_prelude::tests::native_exec_replacement";
struct ChildGuard(spawnfd::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn native_prelude_actor() {
    let Some(fd) = std::env::var_os("ATERM_TEST_METADATA_PRELUDE_FD") else {
        return;
    };
    let args = vec![
        OsString::from(VERB),
        OsString::from(format!("--control-fd={}", fd.to_str().unwrap())),
    ];
    let result = entry(&args).expect("private prelude route");
    std::process::exit(if result == std::process::ExitCode::SUCCESS {
        0
    } else {
        75
    });
}

#[test]
fn native_delayed_prelude_actor() {
    let Some(fd) = std::env::var_os("ATERM_TEST_METADATA_PRELUDE_FD") else {
        return;
    };
    let args = vec![
        OsString::from(VERB),
        OsString::from(format!("--control-fd={}", fd.to_str().unwrap())),
    ];
    let result = entry_at(&args, Instant::now() - Duration::from_secs(31)).unwrap();
    std::process::exit(if result == std::process::ExitCode::SUCCESS {
        0
    } else {
        75
    });
}

#[test]
fn actual_native_preparation_survives_scheduling_delay() {
    crate::runtime::test_endpoint().unwrap();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let marker = scratch.path().join("delayed-target-ran");
    let (result, _) = run_selected_actor(&marker, None, DELAYED_ACTOR);
    assert!(result.success());
    assert!(marker.is_file());
}
#[test]
fn native_no_fork_target() {
    let Some(marker) = std::env::var_os("ATERM_TEST_METADATA_MARKER") else {
        return;
    };
    assert!(no_fork(std::process::id()).unwrap());
    assert_eq!(
        std::env::args_os().next().unwrap(),
        OsStr::new("original-selected-argv0")
    );
    assert_eq!(unsafe { libc::getsid(0) }, std::process::id() as i32);
    assert_eq!(
        unsafe { libc::setsid() },
        -1,
        "a selected session leader cannot detach"
    );
    assert_eq!(
        unsafe { libc::setpgid(0, libc::getpgid(libc::getppid())) },
        -1,
        "selected metadata cannot leave its owned group"
    );
    assert_eq!(unsafe { libc::getpgid(0) }, std::process::id() as i32);
    if let Ok(mode) = std::env::var("ATERM_TEST_METADATA_EXEC") {
        let marker = std::path::Path::new(&marker);
        std::fs::write(
            marker.with_extension("attempt"),
            std::process::id().to_string(),
        )
        .unwrap();
        let mut attempt = spawnfd::command(std::env::current_exe().unwrap());
        attempt
            .arg0("original-selected-argv0")
            .args(["--exact", REPLACEMENT, "--nocapture"])
            .env_clear()
            .env("ATERM_TEST_METADATA_REPLACEMENT", marker);
        match mode.as_str() {
            "control" => {}
            "session" => {
                attempt.setsid(true);
            }
            "own-group" => {
                attempt.process_group(0);
            }
            "parent-group" => {
                attempt.process_group(unsafe { libc::getpgid(libc::getppid()) });
            }
            _ => panic!("unknown native exec control"),
        }
        let error = attempt.exec();
        assert_eq!(
            error.raw_os_error(),
            Some(libc::EPERM),
            "actual SETEXEC escape flags must be denied, not merely unsupported: {error}"
        );
        assert_eq!(unsafe { libc::getsid(0) }, std::process::id() as i32);
        assert_eq!(unsafe { libc::getpgid(0) }, std::process::id() as i32);
        assert!(no_fork(std::process::id()).unwrap());
        std::fs::write(marker.with_extension("denied"), b"returned EPERM").unwrap();
        return;
    }
    assert_eq!(
        std::env::var("CARGO_MAKEFLAGS").unwrap(),
        "-j --jobserver-auth=3,4"
    );
    for (fd, access) in [(3, libc::O_RDONLY), (4, libc::O_WRONLY)] {
        let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
        assert!(std::os::unix::fs::FileTypeExt::is_fifo(
            &file.metadata().unwrap().file_type()
        ));
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_ACCMODE,
            access
        );
    }
    // This actual target is past SETEXEC and final GO. Neither fork nor
    // native posix_spawn may create an unowned/escaped descendant.
    let result = unsafe { libc::fork() };
    if result == 0 {
        unsafe { libc::_exit(93) };
    }
    assert_eq!(result, -1, "metadata target unexpectedly forked");
    if let Ok(mut child) = spawnfd::command("/usr/bin/true").spawn() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("metadata target unexpectedly posix_spawned");
    }
    let fd = std::env::var("ATERM_TEST_METADATA_PRELUDE_FD")
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_GETFD) },
        -1,
        "prelude control descriptor leaked into selected image"
    );
    std::fs::write(
        marker,
        "actual target denied fork and posix_spawn after same-pid exec",
    )
    .unwrap();
}
#[test]
fn native_exec_replacement() {
    let Some(marker) = std::env::var_os("ATERM_TEST_METADATA_REPLACEMENT") else {
        return;
    };
    assert!(no_fork(std::process::id()).unwrap());
    assert_eq!(
        std::env::args_os().next().unwrap(),
        OsStr::new("original-selected-argv0")
    );
    std::fs::write(
        marker,
        format!(
            "{} {} {}",
            std::process::id(),
            unsafe { libc::getsid(0) },
            unsafe { libc::getpgid(0) }
        ),
    )
    .unwrap();
}
#[test]
fn an_unavailable_initial_stop_stays_owned_without_go_or_a_new_deadline() {
    crate::runtime::test_endpoint().unwrap();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let marker = scratch.path().join("target-ran");
    let mut target = spawnfd::command(std::env::current_exe().unwrap());
    target
        .args(["--exact", TARGET, "--nocapture"])
        .env_clear()
        .env("ATERM_TEST_METADATA_MARKER", &marker)
        .current_dir(scratch.path());
    let deadline = Instant::now() + Duration::from_secs(60);
    let (mut owner, mut prelude) = Owner::new(&target, &[], deadline).unwrap();
    prelude
        .inherit_guards_at(vec![owner.extra().unwrap()])
        .unwrap();
    owner.configured(&mut prelude).unwrap();
    let mut child = ChildGuard(prelude.spawn().unwrap());
    loop {
        if let Some(stop) = child.0.try_job_event().unwrap() {
            assert!(matches!(
                stop.acknowledge().unwrap(),
                Some(spawnfd::ChildEvent::Stopped(
                    spawnfd::ChildStop::SuspendedStart
                ))
            ));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "initial stopped-at-exec observation"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    // Consume the actual notification while leaving the original process stopped.
    // No scheduling race is required to make the next observation unavailable.
    assert!(
        !owner
            .poll(&mut child.0, |_| panic!(
                "no final-image validation before the first stop"
            ))
            .unwrap()
    );
    assert_eq!(owner.deadline, deadline);
    assert!(!owner.started && !owner.ready && owner.original.is_none());
    assert!(owner.retirement.is_none() && owner.written == 0);
    assert!(!marker.exists());
    owner.deadline = Instant::now() - Duration::from_secs(1);
    assert_eq!(
        owner
            .poll(&mut child.0, |_| panic!("expired preparation grants no GO"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert!(!marker.exists());
}

#[test]
fn actual_same_pid_two_holds_apply_no_fork_before_target_go() {
    crate::runtime::test_endpoint().unwrap();
    assert!(
        !no_fork(std::process::id()).unwrap(),
        "unsandboxed allow control"
    );
    let scratch = aterm_tempfile::tempdir().unwrap();
    let marker = scratch.path().join("target-ran");
    let (result, _) = run_selected(&marker, None);
    assert!(
        result.success(),
        "actual target result {result:?}, signal {:?}",
        result.signal()
    );
    assert!(marker.is_file());
    // SETEXEC applies group/session attrs after image activation on this
    // OS; denial can terminate that SAME process rather than return errno.
    // Keep destructive attempts separate and prove a no-attrs replacement
    // really executes under the identical no-fork/session-leader profile.
    for mode in ["control", "session", "own-group", "parent-group"] {
        let marker = scratch.path().join(mode);
        let (result, pid) = run_selected(&marker, Some(mode));
        assert_eq!(
            std::fs::read_to_string(marker.with_extension("attempt")).unwrap(),
            pid.to_string()
        );
        if mode == "control" {
            assert!(
                result.success(),
                "same-profile SETEXEC allow control {result:?}"
            );
            assert_eq!(
                std::fs::read_to_string(&marker).unwrap(),
                format!("{pid} {pid} {pid}")
            );
        } else {
            assert!(!marker.exists(), "SETEXEC escape replacement ran");
            assert!(
                result.signal() == Some(libc::SIGKILL)
                    || (result.success()
                        && std::fs::read(marker.with_extension("denied")).unwrap()
                            == b"returned EPERM"),
                "group/session denial was neither exact EPERM nor actual fatal exec rejection: {result:?}"
            );
        }
    }
}
fn run_selected(marker: &std::path::Path, mode: Option<&str>) -> (std::process::ExitStatus, u32) {
    run_selected_actor(marker, mode, ACTOR)
}

fn run_selected_actor(
    marker: &std::path::Path,
    mode: Option<&str>,
    actor: &str,
) -> (std::process::ExitStatus, u32) {
    let mut target = spawnfd::command(std::env::current_exe().unwrap());
    target
        .arg0("original-selected-argv0")
        .args(["--exact", TARGET, "--nocapture"])
        .env_clear()
        .env("ATERM_TEST_METADATA_MARKER", marker)
        .current_dir(marker.parent().unwrap());
    if let Some(mode) = mode {
        target.env("ATERM_TEST_METADATA_EXEC", mode);
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    let (mut owner, _) = Owner::new(&target, &[3, 4], deadline).unwrap();
    let mut prelude = spawnfd::command(std::env::current_exe().unwrap());
    prelude
        .args(["--exact", actor, "--nocapture"])
        .env_clear()
        .env("ATERM_TEST_METADATA_MARKER", marker)
        .env("ATERM_TEST_METADATA_PRELUDE_FD", owner.target.to_string())
        .env("CARGO_MAKEFLAGS", "-j --jobserver-auth=3,4")
        .current_dir(marker.parent().unwrap())
        .setsid(true)
        .start_suspended(true);
    if let Some(mode) = mode {
        prelude.env("ATERM_TEST_METADATA_EXEC", mode);
    }
    let extra = owner.extra().unwrap();
    let (read, write) = aterm_uds::pipe().unwrap();
    prelude
        .inherit_guards_at(vec![
            extra,
            (OwnedFd::from(read), 3),
            (OwnedFd::from(write), 4),
        ])
        .unwrap();
    owner.configured(&mut prelude).unwrap();
    let mut guard = ChildGuard(prelude.spawn().unwrap());
    let child = &mut guard.0;
    let original = process::read_lineage(child.id() as i32).unwrap();
    assert!(!marker.exists());
    loop {
        if owner
            .poll(child, |pid| {
                assert_eq!(pid, original.identity.pid as u32);
                assert!(no_fork(pid)?);
                Ok(())
            })
            .unwrap()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual second stopped-at-exec hold"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let selected = process::read_lineage(child.id() as i32).unwrap();
    assert_eq!(selected.identity.birth, original.identity.birth);
    assert_eq!(selected.uniqueid, original.uniqueid);
    assert_eq!(selected.pgid, original.pgid);
    assert_ne!(selected.identity.idversion, original.identity.idversion);
    assert!(
        !marker.exists(),
        "trusted preparation is not selected target GO"
    );
    child.resume().unwrap();
    let result = child.wait().unwrap();
    (result, child.id())
}
