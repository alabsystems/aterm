// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
#[cfg(target_os = "macos")]
fn native_launchctl_survives_scheduling_delay() {
    let program = aterm_uds::deadline::stand_in(Path::new("/bin/launchctl"));
    let mut command = aterm_uds::spawnfd::command(program);
    command.arg("version");
    let output = launchctl_output(
        &mut command,
        std::time::Instant::now() - std::time::Duration::from_secs(31),
    )
    .unwrap();
    assert!(output.status.success());
    assert!(!output.stdout.is_empty());
    assert!(matches!(
        launchctl_output(
            &mut command,
            std::time::Instant::now() - std::time::Duration::from_secs(61)
        ),
        Err(aterm_uds::deadline::RunError::TimedOut)
    ));
}

#[test]
fn the_installed_app_and_dev_builds_never_share_a_keeper() {
    let installed = place_for(Path::new(INSTALLED_EXECUTABLE));
    assert_eq!(installed.label, INSTALLED_LABEL);
    assert_eq!(installed.socket_name, "keeper.sock");
    assert!(installed.installed);
    let dev = place_for(Path::new("/Users//x/aterm/target/debug/aterm"));
    assert!(dev.label.starts_with(DEV_LABEL_PREFIX), "{dev:?}");
    assert!(!dev.installed);
    assert_ne!(dev.label, installed.label);
    assert_ne!(dev.socket_name, installed.socket_name);
    assert_eq!(
        dev,
        place_for(Path::new("/Users//x/aterm/target/debug/aterm"))
    );
}

#[test]
fn a_copy_installed_in_the_home_applications_is_the_installed_keeper() {
    let home = Path::new("/Users//x");
    let user = place_in(
        Path::new("/Users//x/Applications/aterm.app/Contents/MacOS/aterm"),
        Some(home),
    );
    assert_eq!(user, place_in(Path::new(INSTALLED_EXECUTABLE), Some(home)));
    assert!(user.installed);
    let other = place_in(
        Path::new("/Users//y/Applications/aterm.app/Contents/MacOS/aterm"),
        Some(home),
    );
    assert!(!other.installed, "another login's copy is not this one's");
    assert!(
        !place_in(
            Path::new("/Applications/aterm.app/Contents/MacOS/aterm"),
            None
        )
        .label
        .starts_with(DEV_LABEL_PREFIX)
    );
    assert!(
        !place_in(
            Path::new("x/Applications/aterm.app/Contents/MacOS/aterm"),
            Some(Path::new("x"))
        )
        .installed,
        "a relative HOME names no installed copy"
    );
}

#[test]
fn the_bundle_is_read_off_the_executable_path() {
    assert_eq!(
        own_bundle(Path::new(INSTALLED_EXECUTABLE)),
        Some(PathBuf::from("/Applications/aterm.app"))
    );
    assert_eq!(own_bundle(Path::new("/Users//x/target/debug/aterm")), None);
    assert_eq!(own_bundle(Path::new("/x/Contents/MacOS/aterm")), None);
}

#[test]
fn the_submit_argv_is_plain() {
    assert_eq!(
        submit_argv("l", "/bin/sleep", &["600"], Path::new("/p/k.log")),
        [
            "submit",
            "-l",
            "l",
            "-e",
            "/p/k.log",
            "--",
            "/bin/sleep",
            "600"
        ]
    );
    assert_eq!(
        log_path(Path::new("/p/keeper-dev-0123abcd.sock")),
        Path::new("/p/keeper-dev-0123abcd.sock.log")
    );
    assert_eq!(
        log_path(Path::new("/p/k.log")),
        Path::new("/p/k.log.log"),
        "never the socket itself"
    );
}

/// A test-labelled job is submitted into the GUI domain, runs with its stderr in the log,
/// and after `remove` launchd knows no such job; a guard removes it if an assertion fails.
#[cfg(target_vendor = "apple")]
#[test]
fn a_test_job_is_submitted_and_removed_without_a_trace() {
    struct Guard(String);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = remove(&self.0);
        }
    }
    let label = format!("{TEST_LABEL_PREFIX}.{}.jobtest", std::process::id());
    assert_ne!(label, INSTALLED_LABEL);
    assert_eq!(state(&label).expect("launchctl"), JobState::Absent);
    let scratch = crate::test_scratch_dir("job");
    let log = log_path(&scratch.path().join("k.sock"));
    let guard = Guard(label.clone());
    submit(
        &label,
        "/bin/sh",
        &["-c", "echo keeper-job-stderr >&2; exec /bin/sleep 600"],
        &log,
    )
    .expect("submit");
    let started = std::time::Instant::now();
    let pid = loop {
        if let JobState::Loaded { pid: Some(pid) } = state(&label).expect("launchctl") {
            break pid;
        }
        assert!(started.elapsed().as_secs() < 60, "the job never ran");
        aterm_uds::deadline::poll_yield();
    };
    assert!(aterm_uds::process::pid_alive(pid));
    let started = std::time::Instant::now();
    while !std::fs::read_to_string(&log).is_ok_and(|t| t.contains("keeper-job-stderr")) {
        assert!(
            started.elapsed().as_secs() < 60,
            "the job's stderr never reached {}",
            log.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    remove(&label).expect("remove");
    let started = std::time::Instant::now();
    loop {
        let now = state(&label).expect("launchctl");
        if now == JobState::Absent {
            break;
        }
        assert!(started.elapsed().as_secs() < 60, "never gone: {now:?}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::mem::forget(guard);
    let started = std::time::Instant::now();
    while aterm_uds::process::pid_alive(pid) {
        assert!(
            started.elapsed().as_secs() < 60,
            "the job's process outlived it"
        );
        aterm_uds::deadline::poll_yield();
    }
}
