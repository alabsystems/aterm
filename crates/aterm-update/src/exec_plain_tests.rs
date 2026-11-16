// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_uds::spawnfd::Stdio;

const HELPER_ENV: &str = "ATERM_EXEC_PLAIN_TEST_HELPER";

fn exec_command(program: impl AsRef<std::ffi::OsStr>) -> aterm_uds::spawnfd::StdCommand {
    #[expect(
        clippy::disallowed_types,
        reason = "exercise the raw exec boundary and its SIGPIPE control; this command is never spawned"
    )]
    std::process::Command::new(program)
}

fn sigpipe_is_ignored() -> bool {
    sigpipe_disposition().is_ok_and(|action| action.sa_sigaction == libc::SIG_IGN)
}

#[test]
fn a_successful_plain_exec_replaces_the_process_without_changing_its_pid() {
    let mode = std::env::var(HELPER_ENV).unwrap_or_default();
    if matches!(mode.as_str(), "successful-exec" | "successful-pipe") {
        assert!(
            sigpipe_is_ignored(),
            "Rust's helper starts with SIGPIPE ignored"
        );
        let mut command = exec_command(aterm_uds::deadline::stand_in(std::path::Path::new(
            "/bin/sh",
        )));
        let script = if mode == "successful-pipe" {
            "printf 'EXEC-AFTER=%s\\n' \"$$\"; kill -s PIPE \"$$\"; printf 'SURVIVED-PIPE\\n'"
        } else {
            "printf 'EXEC-AFTER=%s\\n' \"$$\""
        };
        command
            .args(["-c", script])
            .env_remove("BASH_ENV")
            .env_remove("ENV");
        println!("\nEXEC-BEFORE={}", std::process::id());
        io::Write::flush(&mut io::stdout()).expect("the original PID is written before exec");
        let error = exec_keeping_sigpipe(&mut command);
        panic!("a successful exec must not return: {error}");
    }
    for mode in ["successful-exec", "successful-pipe"] {
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        command
        .args([
            "--exact",
            "exec_plain::tests::a_successful_plain_exec_replaces_the_process_without_changing_its_pid",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER_ENV, mode);
        let output = aterm_uds::deadline::output_while_process_progressing(
            &mut command,
            aterm_spec::wait::MUST_HAPPEN,
            "the successful exec helper",
        )
        .expect("the replacement process completes");
        if mode == "successful-pipe" {
            use std::os::unix::process::ExitStatusExt as _;
            assert_eq!(output.status.signal(), Some(libc::SIGPIPE), "{output:?}");
            assert!(
                !String::from_utf8_lossy(&output.stdout).contains("SURVIVED-PIPE"),
                "{output:?}"
            );
        } else {
            assert!(output.status.success(), "{output:?}");
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let pid = |prefix: &str| {
            stdout
                .lines()
                .find_map(|line| line.strip_prefix(prefix))
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or_else(|| panic!("missing {prefix:?} PID in {stdout}"))
        };
        let original = pid("EXEC-BEFORE=");
        assert_ne!(original, 0);
        assert_eq!(pid("EXEC-AFTER="), original, "exec keeps its PID: {stdout}");
    }
}

#[test]
fn a_pre_exec_refusal_restores_sigpipe_and_reopens_the_gate() {
    if std::env::var(HELPER_ENV).as_deref() == Ok("hook-refusal") {
        assert!(sigpipe_is_ignored());
        let mut command = exec_command(aterm_uds::deadline::stand_in(std::path::Path::new(
            "/usr/bin/printf",
        )));
        command.arg("UNSAFE-EXEC-AFTER-HOOK-REFUSAL\\n");
        // SAFETY: the hook only reads a signal disposition and returns an errno;
        // neither operation allocates or takes a lock after a fork.
        unsafe {
            command.pre_exec(|| {
                let disposition = sigpipe_disposition()?;
                let errno = if disposition.sa_sigaction == libc::SIG_DFL {
                    libc::EACCES
                } else {
                    libc::EINVAL
                };
                Err(io::Error::from_raw_os_error(errno))
            });
        }
        let error = exec_keeping_sigpipe(&mut command);
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
        assert!(sigpipe_is_ignored());
        assert!(aterm_sysprobe::exec_gate::reading().is_some());
        println!("HOOK-REFUSED-AND-RESTORED");
        return;
    }
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "exec_plain::tests::a_pre_exec_refusal_restores_sigpipe_and_reopens_the_gate",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER_ENV, "hook-refusal");
    let output = aterm_uds::deadline::output_while_process_progressing(
        &mut command,
        aterm_spec::wait::MUST_HAPPEN,
        "the refused pre-exec hook helper",
    )
    .expect("the isolated hook helper completes");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("HOOK-REFUSED-AND-RESTORED"), "{stdout}");
    assert!(
        !stdout.contains("UNSAFE-EXEC-AFTER-HOOK-REFUSAL"),
        "{stdout}"
    );
}

#[test]
fn an_unread_signal_disposition_refuses_before_exec_and_reopens_the_gate() {
    if std::env::var(HELPER_ENV).as_deref() == Ok("unread-signal") {
        let mut command = exec_command(aterm_uds::deadline::stand_in(std::path::Path::new(
            "/usr/bin/printf",
        )));
        command.arg("UNSAFE-EXEC-WITH-UNREAD-SIGNAL\\n");
        let error = exec_reading_sigpipe(&mut command, || {
            Err(io::Error::from_raw_os_error(libc::EPERM))
        });
        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
        assert!(
            sigpipe_is_ignored(),
            "a refused exec changes no disposition"
        );
        assert!(aterm_sysprobe::exec_gate::reading().is_some());
        println!("REFUSED-UNREAD-SIGNAL-AND-REOPENED");
        return;
    }
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "exec_plain::tests::an_unread_signal_disposition_refuses_before_exec_and_reopens_the_gate",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER_ENV, "unread-signal");
    let output = aterm_uds::deadline::output_while_process_progressing(
        &mut command,
        aterm_spec::wait::MUST_HAPPEN,
        "the unread-signal exec helper",
    )
    .expect("the isolated signal helper completes");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    assert!(
        !stdout.contains("UNSAFE-EXEC-WITH-UNREAD-SIGNAL"),
        "{stdout}"
    );
    assert!(
        stdout.contains("REFUSED-UNREAD-SIGNAL-AND-REOPENED"),
        "{stdout}"
    );
}

#[test]
fn a_failed_plain_exec_leaves_sigpipe_ignored_where_command_exec_does_not() {
    if zz_helper_entry_if_asked() {
        return;
    }
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "exec_plain::tests::a_failed_plain_exec_leaves_sigpipe_ignored_where_command_exec_does_not",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER_ENV, "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = aterm_uds::deadline::output_while_process_progressing(
        &mut command,
        aterm_spec::wait::MUST_HAPPEN,
        "the failed-exec signal helper",
    )
    .expect("the helper runs");
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.success(), "{text}");
    for want in [
        "PRECONDITION sigpipe_ignored=true",
        "raw Command::exec errno=Some(2) sigpipe_ignored=false",
        "exec_keeping_sigpipe errno=Some(2) sigpipe_ignored=true",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
}

#[test]
fn a_plain_exec_refuses_an_unsettled_process_table_read() {
    if std::env::var(HELPER_ENV).as_deref() == Ok("held-read") {
        let read = aterm_sysprobe::exec_gate::reading().expect("the read enters an open gate");
        let mut command = exec_command(aterm_uds::deadline::stand_in(std::path::Path::new(
            "/usr/bin/printf",
        )));
        command.arg("UNSAFE-EXEC-PASSED-THE-READ\\n");
        let error = exec_keeping_sigpipe(&mut command);
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock, "{error}");
        assert!(
            sigpipe_is_ignored(),
            "a refused exec changes no disposition"
        );
        drop(read);
        assert!(aterm_sysprobe::exec_gate::reading().is_some());
        let error = exec_keeping_sigpipe(&mut exec_command("/nonexistent/exec-plain-probe"));
        assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
        assert!(sigpipe_is_ignored());
        assert!(aterm_sysprobe::exec_gate::reading().is_some());
        println!("BLOCKED-THEN-REOPENED");
        return;
    }
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "exec_plain::tests::a_plain_exec_refuses_an_unsettled_process_table_read",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER_ENV, "held-read")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = aterm_uds::deadline::output_while_process_progressing(
        &mut command,
        aterm_spec::wait::MUST_HAPPEN,
        "the isolated gate helper",
    )
    .expect("the isolated gate helper completes");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stdout: {stdout}; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!stdout.contains("UNSAFE-EXEC-PASSED-THE-READ"), "{stdout}");
    assert!(stdout.contains("BLOCKED-THEN-REOPENED"), "{stdout}");
}

fn zz_helper_entry_if_asked() -> bool {
    if std::env::var_os(HELPER_ENV).is_none() {
        return false;
    }
    println!();
    println!("PRECONDITION sigpipe_ignored={}", sigpipe_is_ignored());
    let mut raw = exec_command("/nonexistent/exec-plain-probe");
    let error = {
        let _closed = aterm_sysprobe::exec_gate::close(aterm_sysprobe::exec_gate::SETTLES_WITHIN)
            .expect("the raw signal oracle closes its process-read gate");
        #[expect(
            clippy::disallowed_methods,
            reason = "the standard-library signal oracle is isolated and its process-read gate is closed"
        )]
        let error = raw.exec();
        error
    };
    println!(
        "raw Command::exec errno={:?} sigpipe_ignored={}",
        error.raw_os_error(),
        sigpipe_is_ignored()
    );
    let ignore = libc::sigaction {
        sa_sigaction: libc::SIG_IGN,
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: a valid disposition for SIGPIPE; a null `oldact` is allowed.
    unsafe { libc::sigaction(libc::SIGPIPE, &ignore, std::ptr::null_mut()) };
    let mut kept = exec_command("/nonexistent/exec-plain-probe");
    let error = exec_keeping_sigpipe(&mut kept);
    println!(
        "exec_keeping_sigpipe errno={:?} sigpipe_ignored={}",
        error.raw_os_error(),
        sigpipe_is_ignored()
    );
    true
}
