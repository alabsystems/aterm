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
    /// Cut after `child` is killed (fields drop after `Drop::drop`), and closed by
    /// the kernel if this test process dies first: the instance goes with it.
    _lifeline: aterm_uds::lifeline::Lifeline,
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
        .args(launch_isolation::control_sock(&tmp))
        .args(["--lines", "40", "--columns", "120"])
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    let lifeline = launch_isolation::lifeline(&mut cmd, &tmp);
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
        _lifeline: lifeline,
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
///
/// THE 500 MS IS TIMED BETWEEN TWO PUSHES (2026-09-24). It used to run from the
/// test writing the go-file to a polled `status` answering `prompt`, so it also
/// timed the fake worker's `sleep` spawn before it noticed the file and one
/// `aterm ctl` spawn per poll. At a load of ~40 that read 539 ms while the
/// verdict itself had flipped 24 ms before the read. Both ends are now read
/// off ONE `screen,events,ts` subscription opened before anything moves: the
/// first frame that shows the box, and the pushed `agent prompt` event, each
/// timed by the SERVER's `T <local> <t_us>` wake stamp. Stamped on arrival
/// here instead, each end would also time the `aterm ctl` relay process and
/// this test's reader thread, scheduling that load stretches while the
/// product is right: the proxy error this test was rewritten to shed.
#[test]
fn a_box_under_a_ticking_row_is_published_pushed_and_awaitable() {
    let Some(inst) = boot("b") else { return };
    let (local, sid) = boot_session(&inst);
    let script = inst.tmp.join("fake.sh");
    let go = inst.tmp.join("go");
    std::fs::write(&script, FAKE_WORKER).expect("write the fake worker");

    // The events stream, opened before anything moves.
    // Screen frames AND events on one connection, every wake stamped by the
    // SERVER (`ts`): the box's first frame and the verdict's push are the two
    // ends of the latency this test bounds.
    let streams = "screen,events,ts";
    let mut sub = client_command(&inst, &["subscribe", &format!("@{sid}"), streams])
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
    // Pin the revision once the FSM has PUBLISHED the running job, never on
    // a timer. `agent=busy` is the timeline's verdict and can lead the FSM's
    // `running` — held for its 750 ms dwell, then published on a sweep — by
    // most of a second; a fixed 1.2 s sleep raced that publication, and a
    // loaded gate whose event loop fell half a second behind pinned before
    // it, so `revision=` moved by one at the prompt on a correct server (the
    // load-sensitive test audit of 2026-09-27). Once `running` with movement
    // is out, nothing moves the revision while the fake keeps ticking short
    // of a 5 s stall (`quiet_after`), so the control below still fails for a
    // gate that bumps it when the box is drawn.
    let before = status_until(
        &inst,
        &sid,
        Duration::from_secs(20),
        "phase=running with content_activity",
        |s| {
            field(s, "phase") == Some("running")
                && field(s, "reasons")
                    .is_some_and(|r| r.split(',').any(|r| r == "content_activity"))
        },
    );
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
    // The status poll proves WHAT the verdict says; how fast it moved is
    // bounded below, between two pushes, never by this poll's spawns.
    let prompt = status_until(&inst, &sid, Duration::from_secs(10), "agent=prompt", |s| {
        field(s, "agent") == Some("prompt")
    });
    let took = t0.elapsed();
    eprintln!("agent=prompt {took:?} after the go-file (busy: {busy}; program: {program})");
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

    // The push, and the latency between the box's first frame and it.
    let want = format!("EVENT {local} agent prompt rev={rev} gen=");
    // `T <local> <t_us>` opens every wake that writes to this channel, on the
    // server's subscriber thread; the frames after it carry that instant.
    let stamp = format!("T {local} ");
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut seen = Vec::new();
    let mut wake_us: Option<u64> = None;
    let mut boxed_at: Option<u64> = None;
    let mut pushed_at: Option<u64> = None;
    while Instant::now() < deadline {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(100)) {
            if let Some(us) = line.strip_prefix(&stamp).and_then(|t| t.parse().ok()) {
                wake_us = Some(us);
                continue;
            }
            if boxed_at.is_none() && line.contains("Do you want to proceed?") {
                boxed_at = wake_us;
            }
            if pushed_at.is_none() && line.starts_with(&want) && line.contains(" fp=") {
                pushed_at = wake_us;
            }
            // Both ends, in either order: a wake snapshots the screen under one
            // terminal lock and drains the timeline under another, so the sweep
            // can publish the verdict in between and the EVENT reaches the wire
            // one wake before the frame showing the box. That is a latency of
            // zero (saturating), not a missing box.
            if pushed_at.is_some() && boxed_at.is_some() {
                break;
            }
            if line.starts_with("EVENT") || line.starts_with("GAP") {
                seen.push(line);
            }
        }
    }
    let _ = sub.kill();
    let _ = sub.wait();
    let pushed_at =
        pushed_at.unwrap_or_else(|| panic!("no `{want}` on the events stream: {seen:?}"));
    let boxed_at = boxed_at.expect("the box's first frame was pushed on the screen stream");
    let latency = Duration::from_micros(pushed_at.saturating_sub(boxed_at));
    eprintln!("the verdict was pushed {latency:?} after the box's first frame");
    assert!(
        latency <= Duration::from_millis(500),
        "the box's verdict was pushed {latency:?} after its first frame (> 500 ms): {prompt}"
    );

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

/// A fake Claude Code that draws its idle frame, waits for `die` to exist,
/// then clears the screen (as Claude restores the terminal) and exits.
const FAKE_EXIT: &str = r#"#!/bin/sh
die="$1"
rule=$(printf '%120s' '' | sed 's/ /─/g')
printf '\033[2J\033[H'
printf '⏺ Done.\n\n%s\n❯ \n%s\n  ? for shortcuts\n' "$rule" "$rule"
while [ ! -e "$die" ]; do sleep 0.05; done
rm -f "$die"
printf '\033[2J\033[H'
exit 0
"#;

/// AN AGENT'S EXIT IS NO AGENT AT ONCE (2026-09-24; lane U's open item for
/// the server — the relaunch on exit got some exits late or never). Three
/// times over, a fake Claude Code exits back to its bash: within 1 s of the
/// exit `program=` names the shell and `agent=-`, and — the defect — NO
/// agent verdict is published after the exit: the look that first saw the
/// shell's group read the departed agent's name, judged the shell's prompt
/// under Claude Code's reader and published its verdict again — `program=bash
/// agent=idle` stood for a further look (measured: 250 ms, three rounds of
/// three) and the in-GUI host went on seeing the agent. Under the shell,
/// `agent=` is `-` from the first read that names it.
///
/// THE 1 S IS TIMED BETWEEN TWO PUSHES (the load-sensitive test audit of
/// 2026-09-27), as its sibling above was on 2026-09-24: from the SERVER's wake
/// stamp on the first frame after the exit (the fake's final clear) to the
/// stamp on the pushed `agent -`, both off ONE `screen,events,ts`
/// subscription. It used to run from the test writing `die` to a polled
/// `status`, so it also timed the fake's `sleep 0.05` loop and one `aterm ctl`
/// spawn per poll — three times per run. The polls still say WHAT the verdict
/// is, under a deadline that only a hang reaches. And the verdict is pinned
/// once two reads a sweep apart agree, not after a fixed 600 ms a slow sweep
/// could outlast and then move the rev on a correct server.
#[test]
fn an_agent_that_exits_is_no_agent_from_its_exit_on() {
    let Some(inst) = boot("x") else { return };
    let (local, sid) = boot_session(&inst);
    let script = inst.tmp.join("fake-exit.sh");
    let die = inst.tmp.join("die");
    std::fs::write(&script, FAKE_EXIT).expect("write the fake");

    // One subscription for all three rounds, opened before anything moves.
    let mut sub = client_command(
        &inst,
        &["subscribe", &format!("@{sid}"), "screen,events,ts"],
    )
    .spawn()
    .expect("spawn subscribe");
    let stream = sub.stdout.take().expect("subscribe stdout");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    type_line(&inst, &sid, "/bin/bash --norc --noprofile -i");
    status_until(&inst, &sid, Duration::from_secs(10), "program=bash", |s| {
        field(s, "program") == Some("bash")
    });
    type_line(&inst, &sid, "PS1='$ '");
    for round in 0..3 {
        type_line(
            &inst,
            &sid,
            &format!(
                "/bin/bash -c 'exec -a claude /bin/sh {} {}'",
                script.display(),
                die.display()
            ),
        );
        let running = status_until(
            &inst,
            &sid,
            Duration::from_secs(10),
            "program=claude, read as an agent",
            |s| {
                field(s, "program") == Some("claude")
                    && matches!(field(s, "agent"), Some("idle" | "question"))
            },
        );
        let rev = settled_agent_rev(&inst, &sid);
        // Everything pushed so far is the fake's life; the stream is read from
        // its exit on.
        while rx.try_recv().is_ok() {}
        std::fs::write(&die, b"").expect("the exit");
        let t0 = Instant::now();
        let mut seen: Vec<(Duration, String, String, u64)> = Vec::new();
        let gone = loop {
            let s = status(&inst, &sid);
            let program = field(&s, "program").unwrap_or("?").to_string();
            let agent = field(&s, "agent").unwrap_or("?").to_string();
            let r: u64 = field(&s, "agent_rev")
                .and_then(|r| r.parse().ok())
                .unwrap_or(0);
            if seen
                .last()
                .is_none_or(|(_, p, a, rv)| (p, a, rv) != (&program, &agent, &r))
            {
                seen.push((t0.elapsed(), program.clone(), agent.clone(), r));
            }
            if program == "bash" && agent == "-" {
                break true;
            }
            if t0.elapsed() > Duration::from_secs(20) {
                break false;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        eprintln!("round {round}: {seen:?} (running: {running})");
        assert!(gone, "round {round}: never program=bash agent=-: {seen:?}");
        for (at, program, agent, r) in &seen {
            assert!(
                (*r == rev && program == "claude") || agent == "-",
                "round {round}: `agent={agent}` (rev {r}) stood at {at:?} under program={program} \
                 — the departed agent's verdict: {seen:?}"
            );
        }
        let took = exit_to_no_agent(&rx, local, round);
        eprintln!("round {round}: agent - pushed {took:?} after the exit's first frame");
        assert!(
            took <= Duration::from_secs(1),
            "round {round}: `agent -` was pushed {took:?} after the exit's first frame: {seen:?}"
        );
    }
    let _ = sub.kill();
    let _ = sub.wait();
}

/// The session's `agent_rev` once two reads 600 ms apart — more than a sweep's
/// 250 ms floor — agree, so a verdict still settling is not pinned.
fn settled_agent_rev(inst: &Instance, sid: &str) -> u64 {
    let read = || -> u64 {
        field(&status(inst, sid), "agent_rev")
            .and_then(|r| r.parse().ok())
            .expect("agent_rev")
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last = read();
    loop {
        std::thread::sleep(Duration::from_millis(600));
        let now = read();
        if now == last {
            return now;
        }
        assert!(
            Instant::now() < deadline,
            "agent_rev never settled: {last} -> {now}"
        );
        last = now;
    }
}

/// From the SERVER's wake stamps on one `screen,events,ts` stream: the first
/// screen frame without the fake's footer (its final clear, or bash's prompt)
/// to the pushed `agent -`. Either order: a wake snapshots the screen before it
/// drains the timeline, so the event can reach the wire one wake before the
/// frame showing its cause — a latency of zero (saturating), not a miss.
fn exit_to_no_agent(rx: &std::sync::mpsc::Receiver<String>, local: u64, round: usize) -> Duration {
    let stamp = format!("T {local} ");
    let screen = format!("DELTA {local} seq=");
    let gone = format!("EVENT {local} agent - rev=");
    let deadline = Instant::now() + Duration::from_secs(20);
    let (mut wake_us, mut exited_at, mut gone_at) = (None::<u64>, None, None);
    let mut seen = Vec::new();
    while exited_at.is_none() || gone_at.is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(line) = rx.recv_timeout(left) else {
            panic!("round {round}: no exit frame and `agent -` on the stream: {seen:?}");
        };
        if let Some(us) = line.strip_prefix(&stamp).and_then(|t| t.parse().ok()) {
            wake_us = Some(us);
            continue;
        }
        if line.starts_with(&screen) && line.contains(" screen ") {
            let rows: usize = line
                .rsplit(' ')
                .next()
                .and_then(|n| n.parse().ok())
                .expect("a screen DELTA names its row count");
            let mut footer = false;
            for _ in 0..rows {
                let row = rx
                    .recv_timeout(Duration::from_secs(20))
                    .expect("a screen DELTA's rows follow its header");
                footer |= row.contains("? for shortcuts");
            }
            if !footer && exited_at.is_none() {
                exited_at = wake_us;
            }
            continue;
        }
        if gone_at.is_none() && line.starts_with(&gone) {
            gone_at = wake_us;
        }
        if line.starts_with("EVENT") || line.starts_with("GAP") {
            seen.push(line);
        }
    }
    let (exited_at, gone_at) = (exited_at.unwrap(), gone_at.unwrap());
    Duration::from_micros(gone_at.saturating_sub(exited_at))
}
