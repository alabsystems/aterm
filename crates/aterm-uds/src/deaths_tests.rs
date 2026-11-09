// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_signal_is_read_from_a_wait_status_and_an_exit_or_a_stop_is_none() {
    assert_eq!(signal_of(libc::SIGKILL), Some(libc::SIGKILL));
    assert_eq!(signal_of(libc::SIGTERM), Some(libc::SIGTERM));
    // A core-dump bit beside the signal.
    assert_eq!(signal_of(0x80 | libc::SIGSEGV), Some(libc::SIGSEGV));
    assert_eq!(signal_of(0), None, "exit 0");
    assert_eq!(signal_of(3 << 8), None, "exit 3");
    assert_eq!(signal_of((libc::SIGSTOP << 8) | 0x7f), None, "stopped");
}

#[test]
fn a_line_names_the_reaper_the_child_and_the_signal() {
    assert_eq!(line(10, "aterm", 42, 9), "10 aterm reaped 42 signal 9\n");
}

fn shell(script: &str) -> crate::spawnfd::Command {
    let mut cmd = crate::spawnfd::command(Path::new("/bin/sh"));
    cmd.args(["-c", script])
        .stdin(crate::spawnfd::Stdio::null())
        .stdout(crate::spawnfd::Stdio::null())
        .stderr(crate::spawnfd::Stdio::null());
    cmd
}

/// The journal's lines, empty when it was never written.
fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Set, this process is [`a_spawnfd_child_a_signal_ends_is_journaled_and_one_that_exits_is_not`]'s
/// child that SIGTERM ends.
const TERMED: &str = "ATERM_UDS_DEATHS_TERMED";

/// The child [`a_spawnfd_child_a_signal_ends_is_journaled_and_one_that_exits_is_not`] waits on with
/// accounting, ended by its own SIGTERM. It sets SIGTERM's default first: a test binary a `&` or
/// `nohup` runner started ignoring it, as some do, passes that on through every exec, and a shell
/// started ignoring a signal can neither trap nor die of it.
#[test]
fn a_child_its_own_sigterm_ends_if_asked() {
    if std::env::var_os(TERMED).is_none() {
        return;
    }
    // SAFETY: SIG_DFL is a disposition, no handler; then a signal to this process itself, which
    // runs this test alone.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
        libc::kill(libc::getpid(), libc::SIGTERM);
    }
    std::thread::sleep(std::time::Duration::from_secs(60));
    panic!("SIGTERM did not end the child");
}

#[test]
fn a_spawnfd_child_a_signal_ends_is_journaled_and_one_that_exits_is_not() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("deaths.log");
    forced(Some(path.clone()), || {
        // The wait without accounting.
        let status = shell("kill -9 $$").spawn().unwrap().wait().unwrap();
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(libc::SIGKILL)
        );
        // The wait with accounting.
        let usage = crate::spawnfd::command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "deaths::tests::a_child_its_own_sigterm_ends_if_asked",
                "--test-threads=1",
            ])
            .env(TERMED, "1")
            .stdin(crate::spawnfd::Stdio::null())
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .unwrap()
            .wait_with_rusage()
            .unwrap();
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&usage.status),
            Some(libc::SIGTERM)
        );
        // The negative control: an exit, however it fails, is no death.
        let status = shell("exit 3").spawn().unwrap().wait().unwrap();
        assert_eq!(status.code(), Some(3));
    });
    let got = lines(&path);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got[0].ends_with(" signal 9"), "{got:?}");
    assert!(got[1].ends_with(" signal 15"), "{got:?}");
    assert!(
        got.iter()
            .all(|l| l.starts_with(&format!("{} ", std::process::id()))),
        "{got:?}"
    );
}

#[test]
fn with_no_journal_named_a_killed_child_writes_nothing() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("deaths.log");
    forced(None, || {
        let status = shell("kill -9 $$").spawn().unwrap().wait().unwrap();
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(libc::SIGKILL)
        );
    });
    assert!(!path.exists());
}

#[test]
fn a_journal_at_a_symlink_is_not_written_through() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::write(&target, "").unwrap();
    let link = dir.path().join("deaths.log");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    forced(Some(link), || note(1234, libc::SIGKILL));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "");
    // The control: the same note at a plain path is written.
    let plain = dir.path().join("plain.log");
    forced(Some(plain.clone()), || note(1234, libc::SIGKILL));
    assert!(lines(&plain)[0].ends_with("reaped 1234 signal 9"));
}
