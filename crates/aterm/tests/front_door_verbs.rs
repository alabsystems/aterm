// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Front-door CLI verb routing tests under a real PTY and piped stdio.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

fn private_root() -> aterm_tempfile::TempDir {
    launch_isolation::scratch_root("aterm-front-door")
}

fn isolated<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
    launch_isolation::apply(cmd, root);
    cmd
}

const DEADLINE: Duration = Duration::from_secs(60);

fn aterm_cmd() -> Command {
    Command::new(launch_isolation::aterm_executable())
}

fn on_a_terminal(root: &Path, args: &[&str]) -> String {
    let mut cmd = aterm_cmd();
    isolated(&mut cmd, root).args(args);
    capture_on_a_terminal(cmd)
}

fn capture_on_a_terminal(mut cmd: Command) -> String {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: both fds come from the successful open above and are owned here — nothing else holds or closes them.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    aterm_pty::set_nonblocking(master.as_raw_fd(), true).expect("make the pty master non-blocking");

    let what = format!("{cmd:?}");
    let mut child = cmd
        .stdin(Stdio::from(slave.try_clone().expect("dup pty slave")))
        .stdout(Stdio::from(slave.try_clone().expect("dup pty slave")))
        .stderr(Stdio::from(slave.try_clone().expect("dup pty slave")))
        .spawn()
        .expect("spawn the child under a pty");
    drop(cmd);

    let mut pty = std::fs::File::from(master);
    let mut out = Vec::new();
    let start = Instant::now();
    loop {
        read_available(&mut pty, &mut out, start);
        match child.try_wait().expect("wait on the child") {
            Some(_) => break,
            None if start.elapsed() >= DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`{what}` did not exit within {DEADLINE:?}");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    read_available(&mut pty, &mut out, start);
    drop(slave);
    String::from_utf8_lossy(&out).replace('\r', "")
}

fn read_available(pty: &mut std::fs::File, out: &mut Vec<u8>, start: Instant) {
    let mut chunk = [0u8; 4096];
    while start.elapsed() < DEADLINE {
        match pty.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(e) => panic!("reading the pty master failed: {e}"),
        }
    }
}

fn through_a_pipe(root: &Path, args: &[&str]) -> String {
    let out = isolated(&mut aterm_cmd(), root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run the aterm binary");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text
}

#[test]
fn every_verb_is_routed_by_the_front_door_at_a_terminal() {
    const PROBE: &str = "--aterm-front-door-routing-probe";
    let first = |s: &str| {
        s.lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let root = private_root();
    for verb in aterm_cli::Verb::ALL {
        let out = on_a_terminal(root.path(), &[verb.name(), PROBE]);
        assert!(
            !out.trim().is_empty(),
            "`aterm {} {PROBE}` wrote nothing at a terminal",
            verb.name()
        );
        assert!(
            !out.contains(&format!("aterm: unknown command {}", verb.name())),
            "`aterm {}` fell through to the SESSION parser at a terminal — the verb is \
             routed below the mode fork, not by the front door; output was {out:?}",
            verb.name()
        );
        let pipe = through_a_pipe(root.path(), &[verb.name(), PROBE]);
        assert_eq!(
            first(&out),
            first(&pipe),
            "`aterm {}` answered differently at a terminal than through a pipe",
            verb.name()
        );
    }
}

#[test]
fn edit_answers_as_aterm_edit_aterm_amacs_and_the_amacs_alias() {
    let root = private_root();
    let aterm = launch_isolation::aterm_executable();
    let aterm = aterm.as_path();
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).expect("a bin dir");
    let alias = bin.join("amacs");
    std::os::unix::fs::symlink(aterm, &alias).expect("lay the amacs alias");
    let file = root.path().join("COMMIT_EDITMSG");
    std::fs::write(&file, "x\n").expect("a file to edit");
    let file = file.to_str().expect("utf8 path");
    let run = |program: &Path, args: &[&str], session: Option<&str>| {
        let mut cmd = Command::new(program);
        isolated(&mut cmd, root.path())
            .args(args)
            .env_remove("TMUX")
            .env_remove("STY");
        if let Some(sid) = session {
            cmd.env("ATERM_PARENT_SESSION_ID", sid);
        }
        // SAFETY: the hook runs in the forked child before `exec` and calls only `setsid`, which is async-signal-safe.
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut cmd, || {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let out = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run amacs");
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code(), text)
    };
    for (program, args) in [
        (aterm, &["edit", "--help"][..]),
        (aterm, &["amacs", "--help"]),
        (alias.as_path(), &["--help"]),
    ] {
        let (code, text) = run(program, args, None);
        assert_eq!(code, Some(0), "{program:?} {args:?}: {text}");
        assert!(
            text.starts_with("amacs - aterm's editor"),
            "{program:?} {args:?}: {text}"
        );
    }
    for (program, args) in [(alias.as_path(), &[file][..]), (aterm, &["edit", file])] {
        let (code, text) = run(program, args, Some("s-1"));
        assert_eq!(code, Some(2), "{program:?} {args:?}: {text}");
        assert!(
            text.starts_with("amacs: no terminal to edit in"),
            "{program:?} {args:?}: {text}"
        );
        let (code, text) = run(program, args, None);
        assert_eq!(code, Some(2), "{program:?} {args:?}: {text}");
        assert!(
            text.starts_with("amacs: no terminal to edit in (/dev/tty: "),
            "{program:?} {args:?}: {text}"
        );
    }
}

#[test]
fn an_uninstalled_roster_tool_names_the_install_not_an_unknown_command() {
    let root = private_root();
    for out in [
        on_a_terminal(root.path(), &["ty"]),
        through_a_pipe(root.path(), &["ty"]),
    ] {
        assert!(out.contains("aterm pkg install ty"), "{out:?}");
        assert!(!out.contains("unknown option"), "{out:?}");
        assert!(!out.contains("unknown command"), "{out:?}");
    }
    for out in [
        on_a_terminal(root.path(), &["targo", "build"]),
        through_a_pipe(root.path(), &["targo", "build"]),
    ] {
        assert!(
            out.contains(
                "targo comes with trust, which is not installed (fix: aterm pkg install trust)"
            ),
            "{out:?}"
        );
        assert!(!out.contains("unknown command"), "{out:?}");
    }
}

/// `caffeine`, aCaffiene's name before 0.145.0, is no verb, but an old login job runs `aterm
/// caffeine serve` at every login and again whenever it fails. `serve` exits 0, which launchd's
/// KeepAlive leaves alone, without reaching the window path (whose unknown argument exits 2);
/// under a scratch HOME it retires nothing (`legacy`'s tests reach the retirement). Every other command line names the new verb and
/// exits 2, and `help caffeine` still says no, so an old mug whose aterm is replaced ends.
#[test]
fn the_old_caffeine_serve_exits_0_and_every_other_word_names_the_new_verb() {
    let root = private_root();
    let run = |args: &[&str]| {
        let out = isolated(&mut aterm_cmd(), root.path())
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run the aterm binary");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (code, stdout, stderr) = run(&["caffeine", "serve"]);
    assert_eq!((code, stdout.as_str()), (Some(0), ""), "{stderr}");
    assert!(
        stderr.starts_with(&format!("{}\n", aterm_acaffiene::legacy::RETIRED)),
        "{stderr}"
    );
    #[cfg(target_os = "macos")]
    assert!(
        stderr.contains("not this login's home"),
        "a scratch HOME retires nothing: {stderr}"
    );
    for args in [
        &["caffeine"][..],
        &["caffeine", "status"],
        &["caffeine", "--help"],
        &["caffeine", "serve", "now"],
    ] {
        let (code, stdout, stderr) = run(args);
        assert_eq!((code, stdout.as_str()), (Some(2), ""), "{args:?}: {stderr}");
        assert_eq!(stderr, format!("{}\n", aterm_acaffiene::legacy::RENAMED));
    }
    let (code, _, stderr) = run(&["help", "caffeine"]);
    assert!(code.is_some_and(|code| code != 0), "{code:?}: {stderr}");
}

#[test]
fn an_answer_into_a_closed_pipe_exits_cleanly() {
    let root = private_root();
    for args in [
        &["--help"][..],
        &["--version"],
        &["help", "ctl"],
        &["list-kitty-commands"],
        &["theme", "list"],
        &["agents", "primer"],
        &["pkg", "--help"],
        &["agh", "--help"],
        &["agh", "help"],
        &["agh"],
        &["update", "--help"],
        &["drive", "--help"],
        &["drive", "help"],
        &["drive"],
        &["drive", "classify", "ls"],
        &["fleet", "--help"],
        &["fleet"],
        &["link", "--help"],
        &["fabric", "--help"],
        &["keeper", "--help"],
        &["acaffiene", "--help"],
    ] {
        let (reader, writer) = std::io::pipe().expect("a pipe");
        drop(reader);
        let out = isolated(&mut aterm_cmd(), root.path())
            .args(args)
            .stdin(Stdio::null())
            .stdout(writer)
            .stderr(Stdio::piped())
            .output()
            .expect("run the aterm binary");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("panicked"),
            "`aterm {}` panicked into a closed pipe: {stderr}",
            args.join(" ")
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "`aterm {}` into a closed pipe: {stderr}",
            args.join(" ")
        );
    }
}

/// An answer a full disk refused (`> /dev/full`, ENOSPC) is said on stderr and exits 1, never
/// the verb's success over an empty file: a script that runs `aterm drive … > out` must see the
/// answer was lost. Only a reader that went away ends an answer quietly
/// ([`an_answer_into_a_closed_pipe_exits_cleanly`]); the verbs a library answers in-process
/// exited 0 here and wrote nothing. Linux, whose /dev/full is that disk.
#[cfg(target_os = "linux")]
#[test]
fn an_answer_a_full_disk_refused_exits_1_and_says_so() {
    let root = private_root();
    for args in [
        &["--help"][..],
        &["agh", "--help"],
        &["drive", "--help"],
        &["drive", "classify", "ls"],
        &["drive", "classify", "rm -rf build"],
        &["fleet", "--help"],
        &["link", "--help"],
        &["fabric", "--help"],
        &["keeper", "--help"],
        &["acaffiene", "--help"],
    ] {
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .expect("open /dev/full");
        let out = isolated(&mut aterm_cmd(), root.path())
            .args(args)
            .stdin(Stdio::null())
            .stdout(full)
            .stderr(Stdio::piped())
            .output()
            .expect("run the aterm binary");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(1),
            "`aterm {}` into a full disk: {stderr}",
            args.join(" ")
        );
        assert!(
            stderr.contains("the answer was not written ("),
            "`aterm {}` says the answer was lost: {stderr}",
            args.join(" ")
        );
    }
}

#[test]
fn drive_reaches_this_images_own_control_client() {
    let root = private_root();
    let sock = root.path().join("nobody-serves.sock");
    let out = isolated(&mut aterm_cmd(), root.path())
        .args(["drive", "--sock"])
        .arg(&sock)
        .args(["phase", "@s-1"])
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .output()
        .expect("run the aterm binary");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    assert!(!text.contains("could not run"), "{text}");
    assert!(
        text.contains(&format!("aterm-ctl: connect {}", sock.display())),
        "the image's own client dialled the socket: {text}"
    );
}

/// A refused `--sandbox` (no OS sandbox off macOS) at a terminal says only that
/// no shell was started: the session banner waits for a shell that HAS spawned.
#[cfg(not(target_os = "macos"))]
#[test]
fn a_refused_sandbox_never_says_the_session_started() {
    let root = private_root();
    let out = on_a_terminal(root.path(), &["--sandbox"]);
    assert!(out.contains("the sandbox exists only on macOS"), "{out:?}");
    assert!(!out.contains("session started"), "{out:?}");
}

#[cfg(target_os = "macos")]
fn cpu_policy_child(root: &Path) -> std::process::Child {
    isolated(&mut aterm_cmd(), root)
        .arg(aterm_sysprobe::cpu_policy::COLLECTOR_VERB)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("fixed native collector")
}

#[cfg(target_os = "macos")]
fn cpu_policy_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
    cpu_policy_exit_with(child, || {})
}

#[cfg(target_os = "macos")]
fn cpu_policy_exit_with(
    child: &mut std::process::Child,
    mut drain: impl FnMut(),
) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        drain();
        if let Some(status) = child.try_wait().expect("owned collector wait") {
            return status;
        }
        if start.elapsed() >= DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            panic!("fixed collector did not exit within existing front-door deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "macos")]
fn cpu_policy_hello(child: &mut std::process::Child) -> (std::fs::File, Vec<u8>) {
    let output = std::fs::File::from(OwnedFd::from(child.stdout.take().unwrap()));
    cpu_policy_hello_from(child, output)
}

#[cfg(target_os = "macos")]
fn cpu_policy_hello_from(
    child: &mut std::process::Child,
    mut output: std::fs::File,
) -> (std::fs::File, Vec<u8>) {
    aterm_pty::set_nonblocking(output.as_raw_fd(), true).unwrap();
    let start = Instant::now();
    let mut bytes = Vec::new();
    while !bytes.contains(&b'\n') && start.elapsed() < DEADLINE {
        read_available(&mut output, &mut bytes, start);
        std::thread::sleep(Duration::from_millis(10));
    }
    if !bytes.contains(&b'\n') {
        let _ = child.kill();
        let _ = child.wait();
        panic!("collector did not emit its identity before asking for an ACK");
    }
    (output, bytes)
}

#[test]
#[cfg(target_os = "macos")]
fn cpu_policy_native_dispatch_rejects_operands_before_gui_startup() {
    let root = private_root();
    let mut child = isolated(&mut aterm_cmd(), root.path())
        .args([aterm_sysprobe::cpu_policy::COLLECTOR_VERB, "unexpected"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = std::fs::File::from(OwnedFd::from(child.stdout.take().unwrap()));
    let mut stderr = std::fs::File::from(OwnedFd::from(child.stderr.take().unwrap()));
    aterm_pty::set_nonblocking(stdout.as_raw_fd(), true).unwrap();
    aterm_pty::set_nonblocking(stderr.as_raw_fd(), true).unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let started = Instant::now();
    let status = cpu_policy_exit_with(&mut child, || {
        read_available(&mut stdout, &mut out, started);
        read_available(&mut stderr, &mut err, started);
    });
    read_available(&mut stdout, &mut out, started);
    read_available(&mut stderr, &mut err, started);
    assert_eq!(status.code(), Some(2));
    assert!(String::from_utf8_lossy(&err).contains("takes no operands"));
    assert!(out.is_empty());
}

#[test]
#[cfg(target_os = "macos")]
fn cpu_policy_native_handshake_keeps_actual_credentials_and_bound_capture() {
    use std::io::Write;
    let root = private_root();
    let mut child = cpu_policy_child(root.path());
    let (mut output, mut bytes) = cpu_policy_hello(&mut child);
    let hello: aterm_json::Value =
        aterm_json::from_slice(bytes.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(hello["phase"].as_str(), Some("collector_started"));
    assert_eq!(hello["pid"].as_u64(), Some(u64::from(child.id())));
    assert_eq!(
        hello["effective_uid"].as_u64(),
        Some(u64::from(unsafe { libc::geteuid() }))
    );
    assert_eq!(
        hello["parent_pid"].as_u64(),
        Some(u64::from(std::process::id()))
    );
    child.stdin.as_mut().unwrap().write_all(&[1]).unwrap();
    let capture_started = Instant::now();
    let status = cpu_policy_exit_with(&mut child, || {
        read_available(&mut output, &mut bytes, capture_started)
    });
    read_available(&mut output, &mut bytes, capture_started);
    let lines = bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 2, "identity and one bounded capture");
    let value: aterm_json::Value = aterm_json::from_slice(lines[1]).unwrap();
    let capture = aterm_sysprobe::cpu_policy::decode_capture(&value).expect("complete codec");
    assert_eq!(capture.collector.pid, child.id());
    assert_eq!(
        capture.collector.start_s.to_string(),
        hello["start_s"].as_str().unwrap()
    );
    assert_eq!(capture.effective_uid, unsafe { libc::geteuid() });
    if capture.effective_uid != 0 {
        assert!(!status.success());
        assert!(!capture.complete());
        assert_eq!(
            capture.error.as_deref(),
            Some("permission_denied:root_collection_required")
        );
        assert!(capture.rows.is_empty());
    }
}

#[test]
#[cfg(target_os = "macos")]
fn cpu_policy_native_parent_pipe_closure_ends_the_collector() {
    use std::io::Write;
    let root = private_root();
    let (reader, mut writer) = std::io::pipe().unwrap();
    let mut child = isolated(&mut aterm_cmd(), root.path())
        .arg(aterm_sysprobe::cpu_policy::COLLECTOR_VERB)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(writer.try_clone().unwrap()))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (_output, _hello) =
        cpu_policy_hello_from(&mut child, std::fs::File::from(OwnedFd::from(reader)));
    // Block final output even when the unprivileged capture immediately refuses.
    // Keep the write end open so normal EOF cannot stand in for collector exit.
    cpu_policy_fill_output(&mut writer);
    child.stdin.as_mut().unwrap().write_all(&[1]).unwrap();
    drop(child.stdin.take());
    let status = cpu_policy_exit(&mut child);
    assert_eq!(
        status.code(),
        Some(aterm_sysprobe::cpu_policy::PARENT_LOSS_EXIT),
        "parent-pipe closure is enforced by the live watchdog after ACK"
    );
}

#[cfg(target_os = "macos")]
fn cpu_policy_fill_output(writer: &mut (impl std::io::Write + AsRawFd)) {
    aterm_pty::set_nonblocking(writer.as_raw_fd(), true).unwrap();
    let bytes = [b'x'; 4096];
    let mut written = 0;
    loop {
        match writer.write(&bytes) {
            Ok(n) => {
                assert!(n > 0);
                written += n;
                assert!(written < 16 * 1024 * 1024);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("fill own pipe: {error}"),
        }
    }
    loop {
        match writer.write(b"x") {
            Ok(1) => {
                written += 1;
                assert!(written < 16 * 1024 * 1024);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("fill last pipe byte: {other:?}"),
        }
    }
    aterm_pty::set_nonblocking(writer.as_raw_fd(), false).unwrap();
}

#[test]
#[cfg(target_os = "macos")]
fn cpu_policy_native_watchdog_exits_even_when_identity_stdout_is_blocked() {
    let root = private_root();
    let (reader, mut writer) = std::io::pipe().unwrap();
    cpu_policy_fill_output(&mut writer);
    let mut child = isolated(&mut aterm_cmd(), root.path())
        .arg(aterm_sysprobe::cpu_policy::COLLECTOR_VERB)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Keep both the full pipe's reader and the parent input alive: neither EOF
    // nor a SIGPIPE can substitute for the collector's own deadline.
    let status = cpu_policy_exit(&mut child);
    assert_eq!(
        status.code(),
        Some(aterm_sysprobe::cpu_policy::DEADLINE_EXIT),
        "intrinsic watchdog, not the outer test limit"
    );
    drop(reader);
    drop(child.stdin.take());
}
