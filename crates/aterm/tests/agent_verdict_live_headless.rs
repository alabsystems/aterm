// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance for the SERVER-PUBLISHED agent verdict and program
//! identity (`status program= agent= …`, `EVENT <local> agent <word> rev=<n>
//! gen=<e.s> fp=<hex16>`,
//! `await agent`) — the audit's INT-1 / INT-4 / INT-5 shapes, driven through
//! the one `aterm` binary against a real headless instance.
//!
//! * A FAKE WORKER (a POSIX `sh` script run as `claude` via `exec -a`) draws a
//!   Claude-shaped busy screen, then — when the test creates a go-file — the rm
//!   circuit-breaker box while one transcript row keeps ticking at 5 Hz.
//!   `status agent=prompt` must follow within 500 ms, `EVENT <local> agent
//!   prompt` must be pushed, and `await agent prompt` must latch. NEGATIVE
//!   CONTROL: the shell status FSM's `revision=` — the gate the verdict used to
//!   hang on — does not move across the busy→prompt change, so a classifier
//!   still gated on it would never have seen the box.
//! * A SHELL running `echo "…?"; sleep 30` reads `program=sleep` (the
//!   foreground group's argv[0], no shell integration needed) and `agent=-`
//!   with no attention — a trailing `?` is not a question from a shell.
//!
//! ISOLATION: scratch HOME/XDG roots, a private control socket, a config with
//! every automatic lane off, `--no-reroute`, `SHELL=/bin/sh`
//! (`support/launch_isolation.rs`). A headless instance never reaches
//! WindowServer.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

const SOCKET_POLLS: usize = 300;
const POLL_GAP: Duration = Duration::from_millis(100);
const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(60);
const MAX_SOCK_PATH: usize = 100;

/// One booted headless instance plus its scratch world, torn down on every
/// exit path (Drop runs on panic too).
struct Instance {
    child: Child,
    tmp: PathBuf,
    log: PathBuf,
    sock: String,
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn log_tail(log: &Path) -> String {
    let body = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = body.lines().collect();
    let start = lines.len().saturating_sub(15);
    lines[start..].join("\n")
}

fn is_socket_or_symlink(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_socket() || m.file_type().is_symlink())
        .unwrap_or(false)
}

fn scratch_root(tag: &str) -> Option<PathBuf> {
    let name = format!("atav{tag}-{}", std::process::id());
    for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let tmp = base.join(&name);
        let sock = tmp.join("run/aterm/aterm.sock");
        if sock.as_os_str().len() >= MAX_SOCK_PATH {
            continue;
        }
        if launch_isolation::prepare(&tmp).is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
            continue;
        }
        return Some(tmp);
    }
    None
}

/// Boot one headless instance, 40x120. `None` = an environmental refusal,
/// announced as a SKIP with the log tail.
fn boot(tag: &str) -> Option<Instance> {
    let Some(tmp) = scratch_root(tag) else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return None;
    };
    let log = tmp.join("gui.log");
    let (out, err) = match std::fs::File::create(&log).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("SKIP: cannot open the instance log ({e})");
            let _ = std::fs::remove_dir_all(&tmp);
            return None;
        }
    };
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, &tmp);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .env("ATERM_LINES", "40")
        .env("ATERM_COLUMNS", "120")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: cannot launch aterm --headless ({e})");
            let _ = std::fs::remove_dir_all(&tmp);
            return None;
        }
    };
    let sock_path = tmp.join("run/aterm/aterm.sock");
    let mut inst = Instance {
        child,
        sock: sock_path.to_string_lossy().into_owned(),
        tmp,
        log,
    };
    for _ in 0..SOCKET_POLLS {
        if matches!(inst.child.try_wait(), Ok(Some(_)) | Err(_)) {
            eprintln!(
                "SKIP: aterm --headless exited before binding its socket; log tail:\n{}",
                log_tail(&inst.log)
            );
            return None;
        }
        if is_socket_or_symlink(&sock_path) && launch_isolation::control_listening(&sock_path) {
            return Some(inst);
        }
        std::thread::sleep(POLL_GAP);
    }
    eprintln!(
        "SKIP: control socket never started listening; log tail:\n{}",
        log_tail(&inst.log)
    );
    None
}

fn client_command(inst: &Instance, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    cmd.arg("ctl")
        .arg("--sock")
        .arg(&inst.sock)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    launch_isolation::apply(&mut cmd, &inst.tmp);
    cmd
}

/// One bounded `aterm ctl --sock <sock> <args…>` call.
fn ctl(inst: &Instance, args: &[&str]) -> Output {
    let mut child = client_command(inst, args).spawn().expect("spawn aterm ctl");
    fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + CLIENT_EXIT_DEADLINE;
    loop {
        match child.try_wait().expect("poll aterm ctl") {
            Some(status) => {
                return Output {
                    status,
                    stdout: stdout.join().expect("stdout drain"),
                    stderr: stderr.join().expect("stderr drain"),
                };
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("aterm ctl {args:?} did not exit in time");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn ctl_ok(inst: &Instance, args: &[&str]) -> String {
    let out = ctl(inst, args);
    assert!(
        out.status.success(),
        "aterm ctl {args:?} failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

/// `key=value` out of a status/sessions line.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(prefix.as_str()))
}

/// The boot session's `(local, sid)` from `sessions`.
fn boot_session(inst: &Instance) -> (u64, String) {
    let body = ctl_ok(inst, &["sessions"]);
    let row = body
        .lines()
        .find(|l| l.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or_else(|| panic!("a session row: {body}"));
    let mut toks = row.split_whitespace();
    let local = toks.next().unwrap().parse().unwrap();
    let sid = toks.next().unwrap().to_string();
    (local, sid)
}

fn status(inst: &Instance, sid: &str) -> String {
    ctl_ok(inst, &[&format!("@{sid}"), "status"])
}

/// Type one line into the session and submit it.
fn type_line(inst: &Instance, sid: &str, line: &str) {
    ctl_ok(inst, &[&format!("@{sid}"), "send", line]);
    ctl_ok(inst, &[&format!("@{sid}"), "key", "enter"]);
}

/// Poll `status` until `pred` holds, returning the matching record, or panic
/// with the last record after `within`.
fn status_until(
    inst: &Instance,
    sid: &str,
    within: Duration,
    what: &str,
    pred: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + within;
    let mut last = String::new();
    while Instant::now() < deadline {
        last = status(inst, sid);
        if pred(&last) {
            return last;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{what}: not within {within:?}; last status: {last}");
}

/// The fake worker: a Claude-shaped busy screen (transcript row, spinner,
/// composer between two rules, footer) redrawn every 0.2 s until `go` exists,
/// then the measured 2.1.280 rm circuit-breaker box with the top transcript
/// row ticking every 0.2 s. POSIX sh + printf only.
const FAKE_WORKER: &str = r#"#!/bin/sh
go="$1"
rule=$(printf '%120s' '' | sed 's/ /─/g')
put() { printf '\033[%d;1H\033[2K%s' "$1" "$2"; }
printf '\033[?1049h\033[2J'
n=0
while [ ! -e "$go" ]; do
  n=$((n+1))
  put 1 "⏺ Working on it."
  put 36 "✶ Deliberating… (${n}s · thinking)"
  put 37 "$rule"
  put 38 "❯ "
  put 39 "$rule"
  put 40 "  ⏵⏵ bypass permissions on · esc to interrupt"
  sleep 0.05
done
printf '\033[2J'
put 5 "❯ Use the Bash tool to run exactly this one line: S=\$PWD/tmp; for p in a b; do set -- \$p; rm -rf \$S/\$1; done"
put 8 "  Removing directories tmp/a and tmp/b"
put 9 "  ⎿  \$ S=\$PWD/tmp; for p in a b; do set -- \$p; rm -rf \$S/\$1; done"
put 11 "$rule"
put 12 " Bash command"
put 14 "   S=\$PWD/tmp; for p in a b; do set -- \$p; rm -rf \$S/\$1; done"
put 15 "   Remove directories tmp/a and tmp/b"
put 17 " │ Dangerous rm operation on possibly-empty variable path: \$S/\$1 in \`rm -rf \$S/\$1\`"
put 20 " Do you want to proceed?"
put 21 " ❯ 1. Yes"
put 22 "   2. No"
put 24 " Esc to cancel · Tab to amend"
k=0
while :; do
  k=$((k+1))
  put 1 "⏺ Monitor tick $k"
  sleep 0.2
done
"#;

/// INT-1 + INT-2, live: the box under a ticking row reaches `status
/// agent=prompt` within 500 ms of being drawn, `EVENT <local> agent prompt`
/// is pushed, and `await agent prompt` latches — while `revision=` holds still.
#[test]
fn a_box_under_a_ticking_row_is_published_pushed_and_awaitable() {
    let Some(inst) = boot("b") else { return };
    let (local, sid) = boot_session(&inst);
    let script = inst.tmp.join("fake.sh");
    let go = inst.tmp.join("go");
    std::fs::write(&script, FAKE_WORKER).expect("write the fake worker");

    // The events stream, opened before anything moves.
    let mut sub = client_command(&inst, &["subscribe", &format!("@{sid}"), "events"])
        .spawn()
        .expect("spawn subscribe");
    let events = sub.stdout.take().expect("subscribe stdout");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(events).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    // Run it as `claude` in a job of its own (the shell's job control gives it
    // its own foreground group, the shape a real `claude` has).
    type_line(
        &inst,
        &sid,
        &format!(
            "/bin/bash -c 'exec -a claude /bin/sh {} {}'",
            script.display(),
            go.display()
        ),
    );
    let busy = status_until(&inst, &sid, Duration::from_secs(10), "agent=busy", |s| {
        field(s, "agent") == Some("busy")
    });
    let program = status_until(
        &inst,
        &sid,
        Duration::from_secs(10),
        "program=claude",
        |s| field(s, "program") == Some("claude"),
    );
    // Let the FSM settle on the running job, then pin the revision.
    std::thread::sleep(Duration::from_millis(1200));
    let before = status(&inst, &sid);
    assert_eq!(field(&before, "agent"), Some("busy"), "{before}");
    let revision = field(&before, "revision").unwrap().to_string();

    // A parked waiter, armed before the box exists.
    let waiter = std::thread::spawn({
        let mut cmd = client_command(
            &inst,
            &[
                &format!("@{sid}"),
                "await",
                "agent",
                "prompt",
                "timeout=8000",
            ],
        );
        move || cmd.output().expect("await agent")
    });
    std::thread::sleep(Duration::from_millis(200));

    std::fs::write(&go, b"").expect("create the go file");
    let t0 = Instant::now();
    let prompt = status_until(
        &inst,
        &sid,
        Duration::from_millis(2000),
        "agent=prompt",
        |s| field(s, "agent") == Some("prompt"),
    );
    let took = t0.elapsed();
    eprintln!("agent=prompt {took:?} after the go-file (busy: {busy}; program: {program})");
    assert!(
        took <= Duration::from_millis(500),
        "the box reached status agent=prompt in {took:?} (> 500 ms): {prompt}"
    );
    assert_eq!(
        field(&prompt, "agent_detail"),
        Some("bash:not-read-only"),
        "{prompt}"
    );
    assert_eq!(field(&prompt, "level"), Some("attention"), "{prompt}");
    assert_eq!(field(&prompt, "why"), Some("prompt"), "{prompt}");
    assert_eq!(field(&prompt, "program"), Some("claude"), "{prompt}");
    // NEGATIVE CONTROL: the old classifier gate. The FSM's revision did not
    // move across the change (the screen kept moving, the job kept running),
    // so a verdict gated on it would still say busy.
    assert_eq!(
        field(&prompt, "revision"),
        Some(revision.as_str()),
        "NEGATIVE CONTROL: revision= moved, so this run does not prove the gate: {before} / {prompt}"
    );
    let rev = field(&prompt, "agent_rev").unwrap().to_string();
    // The verdict names the screen it was read from: a generation and a
    // screen hash, the values a press decided from it fences on.
    let agent_gen = field(&prompt, "agent_gen").unwrap_or("-");
    let agent_fp = field(&prompt, "agent_fp").unwrap_or("-");
    assert!(
        agent_gen
            .split_once('.')
            .is_some_and(|(e, s)| { e.parse::<u64>().is_ok() && s.parse::<u64>().is_ok() })
            && agent_fp.len() == 16,
        "{prompt}"
    );

    // The parked waiter latched.
    let waited = waiter.join().expect("await thread");
    let waited = String::from_utf8_lossy(&waited.stdout).into_owned();
    assert!(
        waited.contains("OK agent prompt rev="),
        "await agent: {waited}"
    );
    // Already true: a fresh await answers at once.
    let t1 = Instant::now();
    let latched = ctl_ok(
        &inst,
        &[
            &format!("@{sid}"),
            "await",
            "agent",
            "busy,prompt",
            "timeout=5000",
        ],
    );
    assert!(
        latched.contains(&format!("OK agent prompt rev={rev}")),
        "{latched}"
    );
    assert!(t1.elapsed() < Duration::from_secs(2), "latched, not parked");
    // An unknown word is a usage error, not a wait.
    let bad = ctl(&inst, &[&format!("@{sid}"), "await", "agent", "promtp"]);
    assert!(
        String::from_utf8_lossy(&bad.stdout).contains("ERR usage")
            || String::from_utf8_lossy(&bad.stderr).contains("ERR usage"),
        "{bad:?}"
    );

    // The push.
    let want = format!("EVENT {local} agent prompt rev={rev} gen=");
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut seen = Vec::new();
    let mut found = false;
    while Instant::now() < deadline {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(100)) {
            found |= line.starts_with(&want) && line.contains(" fp=");
            seen.push(line);
            if found {
                break;
            }
        }
    }
    let _ = sub.kill();
    let _ = sub.wait();
    assert!(found, "no `{want}` on the events stream: {seen:?}");

    // The roster says the same.
    let roster = ctl_ok(&inst, &["sessions"]);
    let row = roster.lines().find(|l| l.contains(&sid)).expect("row");
    assert_eq!(field(row, "program"), Some("claude"), "{row}");
    assert_eq!(field(row, "agent"), Some("prompt"), "{row}");
}

/// INT-4 + INT-5, live: a plain shell running `echo "…?"; sleep 30` is named
/// by its foreground program (`program=sleep`, no shell integration) and is
/// NOT an agent: `agent=-`, never `level=attention`.
#[test]
fn a_shell_question_is_not_an_agent_and_its_program_is_named() {
    let Some(inst) = boot("s") else { return };
    let (_local, sid) = boot_session(&inst);
    type_line(
        &inst,
        &sid,
        "echo \"Checking whether the disk is full?\"; sleep 30",
    );
    let rec = status_until(&inst, &sid, Duration::from_secs(10), "program=sleep", |s| {
        field(s, "program") == Some("sleep")
    });
    // Sample for two seconds: never an agent, never attention.
    let until = Instant::now() + Duration::from_secs(2);
    let mut last = rec;
    while Instant::now() < until {
        assert_eq!(field(&last, "agent"), Some("-"), "{last}");
        assert_ne!(field(&last, "level"), Some("attention"), "{last}");
        assert_eq!(field(&last, "why"), Some("-"), "{last}");
        std::thread::sleep(Duration::from_millis(200));
        last = status(&inst, &sid);
    }
    let screen = ctl_ok(&inst, &[&format!("@{sid}"), "text"]);
    assert!(
        screen.contains("disk is full?"),
        "the question is on screen: {screen}"
    );
}

/// The review's `frame.sh` case, live: a plain shell that `cat`s a captured
/// Claude screen — its question, the full-width rules, the `❯` composer and
/// the footer — is still a shell. The events stream is open throughout, so
/// even a transient agent verdict (a flash while the shell's group is being
/// re-named after the job) would be seen: none may be pushed, and `status`
/// ends at `agent=-` with no attention. NEGATIVE CONTROL: the capture really
/// is on screen, frame and all.
#[test]
fn a_shell_that_cats_a_claude_capture_is_not_an_agent() {
    let Some(inst) = boot("c") else { return };
    let (local, sid) = boot_session(&inst);
    let rule = "\u{2500}".repeat(120);
    let capture = inst.tmp.join("capture.txt");
    std::fs::write(
        &capture,
        format!(
            "\u{23fa} Should I also delete the old logs?\n\n{rule}\n\u{276f} \n{rule}\n  ? for shortcuts\n"
        ),
    )
    .expect("write the capture");
    let mut sub = client_command(&inst, &["subscribe", &format!("@{sid}"), "events"])
        .spawn()
        .expect("spawn subscribe");
    let events = sub.stdout.take().expect("subscribe stdout");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(events).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    type_line(&inst, &sid, &format!("cat {}", capture.display()));
    let reply = ctl_ok(
        &inst,
        &[
            &format!("@{sid}"),
            "await",
            "match",
            "for.shortcuts",
            "timeout=10000",
        ],
    );
    assert!(
        !reply.starts_with("OK timeout"),
        "the capture is up: {reply}"
    );
    // Well past the sweep's 250 ms floor and the program re-resolution.
    let until = Instant::now() + Duration::from_secs(2);
    let mut agent_events = Vec::new();
    while Instant::now() < until {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(100))
            && line.starts_with(&format!("EVENT {local} agent "))
        {
            agent_events.push(line);
        }
    }
    let _ = sub.kill();
    let _ = sub.wait();
    let rec = status(&inst, &sid);
    assert!(
        agent_events.is_empty(),
        "no agent verdict: {agent_events:?} / {rec}"
    );
    assert_eq!(field(&rec, "agent"), Some("-"), "{rec}");
    assert_ne!(field(&rec, "level"), Some("attention"), "{rec}");
    let screen = ctl_ok(&inst, &[&format!("@{sid}"), "text"]);
    assert!(
        screen.contains("Should I also delete the old logs?") && screen.contains(&rule[..30]),
        "NEGATIVE CONTROL: the frame is on screen: {screen}"
    );
}
