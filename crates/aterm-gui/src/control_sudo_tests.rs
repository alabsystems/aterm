// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|word| (*word).to_string()).collect()
}

#[test]
fn rename_summary_requires_a_fixed_operation_and_valid_number() {
    for field in ["ComputerName", "LocalHostName", "HostName"] {
        assert!(
            rename_purpose(&words(&["/usr/sbin/scutil", "--set", field, "m59"]))
                .unwrap()
                .ends_with("m59.")
        );
    }
    for args in [
        words(&["/tmp/scutil", "--set", "HostName", "m59"]),
        words(&["/usr/sbin/scutil", "--get", "HostName", "m59"]),
        words(&["/usr/sbin/scutil", "--set", "HostName", "m59; do more"]),
        words(&["/usr/sbin/scutil", "--set", "HostName", "m59", "extra"]),
    ] {
        assert_eq!(rename_purpose(&args), None);
    }
}

#[test]
fn the_check_judges_the_words_aterm_will_exec() {
    assert_eq!(
        admit(&words(&["/usr/sbin/scutil", "--get", "ComputerName"])).expect("ordinary"),
        words(&[
            "-k",
            "-A",
            "--",
            "/usr/sbin/scutil",
            "--get",
            "ComputerName"
        ])
    );
    // An argument named like a debugger is not the target.
    assert!(admit(&words(&["/bin/echo", "lldb"])).is_ok());
    for debugger in [
        "/usr/bin/lldb",
        "/tmp/private tools/gdb",
        "/usr/sbin/dtrace",
    ] {
        assert_eq!(
            admit(&words(&[debugger, "x"])),
            Err("ERR sudo refused debugger-as-root\n"),
            "{debugger}"
        );
    }
    for unjudgeable in [&["lldb"][..], &["FOO=bar", "/usr/bin/lldb"], &[""], &[]] {
        assert!(
            admit(&words(unjudgeable)).is_err_and(|refusal| refusal.contains("command-unreadable")),
            "{unjudgeable:?}"
        );
    }
    assert!(admit(&words(&["/odd=path/tool"])).is_err());
}

/// What the sheet shows tells one word holding a space from two words, which is exactly what
/// the `ps` line cannot do. NEGATIVE CONTROL: the two forms differ.
#[test]
fn the_sheet_shows_word_boundaries_and_no_control_bytes() {
    assert_eq!(shown(&words(&["/bin/rm", "a b"])), "/bin/rm 'a b'");
    assert_ne!(
        shown(&words(&["/bin/rm", "a b"])),
        shown(&words(&["/bin/rm", "a", "b"]))
    );
    assert_eq!(shown(&words(&["/bin/x", ""])), "/bin/x ''");
    assert_eq!(shown(&words(&["/bin/x", "it's"])), "/bin/x 'it'\\''s'");
    assert!(!shown(&words(&["/bin/x", "a\nb\u{202e}"])).contains('\n'));
    assert!(!shown(&words(&["/bin/x", "a\nb\u{202e}"])).contains('\u{202e}'));
    for forger in ['\u{2028}', '\u{2029}', '\u{061c}'] {
        let line = shown(&words(&["/bin/x", &format!("a{forger}Asked by:")]));
        assert!(!line.contains(forger), "{forger:?} reached the sheet");
    }
}

/// The sheet names the folder the words run in, where root resolves a relative word, and
/// no folder name can forge the sheet's other lines.
/// NEGATIVE CONTROL: the same words in two folders read differently.
#[test]
fn the_sheet_names_the_folder_the_command_runs_in() {
    let chmod = words(&["/bin/chmod", "-R", "777", "."]);
    let at_root = sheet_text("/", &chmod);
    assert_eq!(at_root, "/bin/chmod -R 777 .\n\nIn the folder:\n/");
    assert_ne!(at_root, sheet_text("/Users//a/project", &chmod));
    let forged = sheet_text("/tmp/x\n\nAsked by:\nyou", &chmod);
    assert_eq!(forged.matches('\n').count(), 3, "{forged}");
}

fn stand_in(script: &str) -> Command {
    let mut command = aterm_uds::spawnfd::command(aterm_uds::os_path!("sh"));
    command.args(["-c", script]);
    command
}

/// The binding exists while the process runs and is gone when it ends, under the pid aterm
/// spawned and no other; the command's stdout is the caller's and its code comes back.
#[test]
fn a_started_command_is_bound_for_its_life_and_reports_its_exit() {
    let (reader, writer) = aterm_uds::pipe().expect("pipe");
    let mut command = stand_in("echo ran; exit 7");
    command.stdout(writer);
    let marker = "/bin/bound-for-its-life".to_owned();
    let mut seen: Option<u32> = None;
    let line = run_bound(
        command,
        marker,
        None,
        4242,
        Duration::from_secs(60),
        &mut || {
            let table = BOUND.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(bound) = table
                .iter()
                .find(|b| b.command == "/bin/bound-for-its-life")
            {
                seen = Some(bound.identity.pid);
                assert_eq!(bound.client_pid, 4242);
            }
            false
        },
    );
    assert_eq!(line, "OK sudo exit=7\n");
    let output = std::io::read_to_string(reader).expect("the command's stdout");
    assert_eq!(output, "ran\n");
    let pid = seen.expect("bound while running");
    assert!(bound(i32::try_from(pid).unwrap()).is_none(), "gone at exit");
    assert!(bound(i32::MAX).is_none());
}

/// A caller's hang-up ends the command and withdraws the open sheet's ticket.
/// NEGATIVE CONTROL: until the hang-up the command is running and the ticket live.
#[test]
fn a_caller_hang_up_ends_the_command_and_withdraws_the_sheet() {
    let marker = "/bin/hang-up-case".to_owned();
    let ticket = Arc::new(AskpassTicket::new(
        1,
        Instant::now() + Duration::from_secs(60),
    ));
    let mut watched = false;
    // The command starts with SIGTERM at its default, set here, not this test binary's: one
    // started ignoring it (a `nohup` or `&` runner's choice, which every exec keeps) sleeps
    // through the hang-up.
    let defaults = aterm_spec::start::at_defaults(&[libc::SIGTERM]);
    let line = run_bound(
        stand_in("exec sleep 60"),
        marker,
        None,
        4243,
        Duration::from_secs(60),
        &mut || {
            if watched {
                return true;
            }
            let table = BOUND.lock().unwrap_or_else(PoisonError::into_inner);
            let found = table.iter().find(|b| b.command == "/bin/hang-up-case");
            if let Some(bound) = found {
                bound.watch(&ticket);
                assert!(bound.is_live());
                assert!(ticket.is_live(Instant::now()));
                watched = true;
            }
            false
        },
    );
    drop(defaults);
    assert_eq!(line, "OK sudo signal=15\n");
    assert!(!ticket.is_live(Instant::now()));
}

/// A command that ignores `SIGTERM` is not killed behind the relay (aterm cannot reach a root
/// command, and `SIGKILL` to sudo would orphan it): the lane stops waiting and says so.
/// NEGATIVE CONTROL: the process is still alive when the verb answers.
#[test]
fn a_command_that_ignores_the_hang_up_runs_on_and_frees_the_lane() {
    let marker = "/bin/ignores-term".to_owned();
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-sudo-term-")
        .tempdir()
        .expect("temp dir");
    let armed = scratch.path().join("armed");
    let mut command = stand_in("trap '' TERM; : > \"$1\"; sleep 3; :");
    command.arg("sh").arg(&armed);
    let mut pid: Option<u32> = None;
    let line = run_bound(
        command,
        marker,
        None,
        4244,
        Duration::from_millis(200),
        &mut || {
            let table = BOUND.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(bound) = table.iter().find(|b| b.command == "/bin/ignores-term") {
                pid = Some(bound.identity.pid);
            }
            // Hang up only once the shell ignores SIGTERM, which it shows by the file.
            pid.is_some() && armed.exists()
        },
    );
    assert_eq!(
        line,
        "ERR sudo hung up: the command outlived SIGTERM and runs on to its own end\n"
    );
    let pid = pid.expect("bound");
    assert!(
        crate::cpu_policy_authorization::read_identity(pid).is_some(),
        "still running"
    );
}

#[test]
fn admitted_stdio_waits_for_its_native_sender() {
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsFd as _;
    for age in [Duration::from_secs(11), Duration::from_secs(61)] {
        let expired = age >= aterm_spec::wait::MUST_HAPPEN;
        let (client, server) = CtlStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        let (stdin, mut input) = aterm_uds::pipe().unwrap();
        let (mut output, stdout) = aterm_uds::pipe().unwrap();
        let (mut errors, stderr) = aterm_uds::pipe().unwrap();
        input.write_all(b"stdin proof").unwrap();
        drop(input);
        aterm_uds::fdpass::send_with_fds(
            &client,
            b"s",
            &[stdin.as_fd(), stdout.as_fd(), stderr.as_fd()],
        )
        .unwrap();
        drop((stdin, stdout, stderr));
        let mut observations = 0;
        let result = receive_stdio_with_clock(&server, || {
            observations += 1;
            Instant::now()
                - if observations == 1 {
                    age
                } else {
                    Duration::ZERO
                }
        });
        let result = if expired {
            assert!(
                result.is_none(),
                "expiry must not consume queued native descriptors"
            );
            receive_stdio_with_clock(&server, Instant::now)
        } else {
            result
        };
        let [stdin, stdout, stderr] = result.expect("a scheduled native sender is not absent");
        assert_eq!(
            observations, 2,
            "the actual wait starts before native receipt"
        );
        let mut text = String::new();
        std::fs::File::from(stdin)
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "stdin proof");
        let mut stdout = std::fs::File::from(stdout);
        stdout.write_all(b"stdout proof").unwrap();
        drop(stdout);
        let mut stderr = std::fs::File::from(stderr);
        stderr.write_all(b"stderr proof").unwrap();
        drop(stderr);
        let mut text = String::new();
        output.read_to_string(&mut text).unwrap();
        assert_eq!(text, "stdout proof");
        let mut text = String::new();
        errors.read_to_string(&mut text).unwrap();
        assert_eq!(text, "stderr proof");
    }
}

#[test]
fn stdio_is_exactly_three_descriptors() {
    use std::os::fd::AsFd as _;
    let (client, server) = CtlStream::pair().expect("pair");
    let (read, write) = aterm_uds::pipe().expect("pipe");
    aterm_uds::fdpass::send_with_fds(&client, b"s", &[read.as_fd(), write.as_fd(), write.as_fd()])
        .expect("send");
    assert!(receive_stdio(&server).is_some());
    aterm_uds::fdpass::send_with_fds(&client, b"s", &[write.as_fd()]).expect("send one");
    assert!(receive_stdio(&server).is_none(), "one is not stdio");
    drop(client);
    assert!(receive_stdio(&server).is_none(), "hung up");
}
