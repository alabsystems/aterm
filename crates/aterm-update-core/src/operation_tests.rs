// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn operation_deadline_expires_at_the_boundary_and_cannot_outlive_cancellation() {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let stop = OperationState {
        deadline: Some(deadline),
        ..OperationState::default()
    };
    assert!(!stop.requested_at(deadline - std::time::Duration::from_nanos(1)));
    assert!(stop.requested_at(deadline));
    assert!(stop.requested_at(deadline + std::time::Duration::from_secs(1)));
    stop.requested.store(true, Ordering::Release);
    assert!(stop.requested_at(deadline - std::time::Duration::from_secs(1)));
    let unbounded = OperationState::default();
    assert!(!unbounded.requested_at(deadline));
}

#[test]
fn operation_pipe_cancels_its_owned_helper_and_scope_drop_releases_observer() {
    const CHILD: &str = "ATERM_OPERATION_CANCEL_TEST_CHILD";
    // The watcher may cancel as soon as the witness appears. Rename a
    // complete record into place so cancellation cannot leave an empty PID.
    const PUBLISH_PID: &str =
        r#"printf '%s\n' "$$" > "$0.tmp" && /bin/mv "$0.tmp" "$0" && exec "$1" 600"#;
    if std::env::var_os(CHILD).is_some() {
        use std::os::fd::OwnedFd;
        let dir = aterm_tempfile::tempdir().unwrap();
        let witness = dir.path().join("pid");
        let (read, write) = aterm_uds::pipe().unwrap();
        let scope = OperationCancellation::watch(OwnedFd::from(read)).unwrap();
        assert!(operation_active());
        assert!(!operation_cancelled());
        assert!(operation_defers_maintenance());
        let path = witness.clone();
        let closer = std::thread::spawn(move || {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while !path.is_file() && std::time::Instant::now() < until {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            drop(write);
            assert!(path.is_file(), "helper execution hang detector");
        });
        let mut cmd = aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(
            std::path::Path::new("/bin/sh"),
        ));
        cmd.args(["-c", PUBLISH_PID])
            .arg(&witness)
            .arg(aterm_uds::deadline::stand_in(std::path::Path::new(
                "/bin/sleep",
            )));
        let error = run_operation_command(&mut cmd).unwrap_err();
        closer.join().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(operation_cancelled());
        assert!(check_operation().is_err());
        let mut absent = aterm_uds::spawnfd::command("/no-such-cancelled-helper");
        assert_eq!(
            run_operation_command(&mut absent).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        let pid = std::fs::read_to_string(&witness)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            aterm_uds::process::process_presence(pid),
            aterm_uds::process::ProcessPresence::Absent
        );
        drop(scope);
        assert!(!operation_active());
        assert!(!operation_cancelled());
        // Normal scope retirement must not wait for the client's still-open writer.
        let (read, held) = aterm_uds::pipe().unwrap();
        let scope = OperationCancellation::watch(OwnedFd::from(read)).unwrap();
        drop(scope);
        assert!(!operation_cancelled());
        drop(held);
        let (read, held) = aterm_uds::pipe().unwrap();
        let scope = OperationCancellation::watch(OwnedFd::from(read)).unwrap();
        let witness = dir.path().join("inflight-pid");
        let path = witness.clone();
        let helper = std::thread::spawn(move || {
            let mut cmd = aterm_uds::spawnfd::command(aterm_uds::deadline::stand_in(
                std::path::Path::new("/bin/sh"),
            ));
            cmd.args(["-c", PUBLISH_PID])
                .arg(path)
                .arg(aterm_uds::deadline::stand_in(std::path::Path::new(
                    "/bin/sleep",
                )));
            run_operation_command(&mut cmd)
        });
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !witness.is_file() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let ran = witness.is_file();
        drop(scope);
        let pid = std::fs::read_to_string(&witness)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            aterm_uds::process::process_presence(pid),
            aterm_uds::process::ProcessPresence::Absent,
            "scope retirement retained the helper's owning wait"
        );
        assert_eq!(
            helper.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert!(ran, "helper execution hang detector");
        drop(held);

        let scope = OperationCancellation::until(std::time::Instant::now()).unwrap();
        assert!(operation_active());
        assert!(operation_cancelled());
        assert!(operation_defers_maintenance());
        assert!(check_operation().is_err());
        let mut absent = aterm_uds::spawnfd::command("/no-such-expired-helper");
        assert_eq!(
            run_operation_command(&mut absent).unwrap_err().kind(),
            io::ErrorKind::Interrupted,
            "the expired scope refuses before spawning another helper"
        );
        assert!(OperationCancellation::until(std::time::Instant::now()).is_err());
        drop(scope);
        assert!(!operation_active());
        assert!(!operation_cancelled());
        let scope = OperationCancellation::until(
            std::time::Instant::now() + std::time::Duration::from_secs(3600),
        )
        .unwrap();
        assert!(!operation_cancelled());
        assert!(!operation_defers_maintenance());
        drop(scope);
        assert!(
            !operation_active(),
            "normal exit retires without waiting for the budget"
        );
        return;
    }
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    cmd.args(["--exact", "operation::tests::operation_pipe_cancels_its_owned_helper_and_scope_drop_releases_observer", "--nocapture"])
        .env(CHILD, "1");
    let output = aterm_uds::deadline::output_until(
        &mut cmd,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
