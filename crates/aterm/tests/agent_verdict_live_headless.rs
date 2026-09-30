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
//! * A SHELL running `echo "…?"; sleep 300` reads `program=sleep` (the
//!   foreground group's argv[0], no shell integration needed) and `agent=-`
//!   with no attention — a trailing `?` is not a question from a shell.
//!
//! ISOLATION: scratch HOME/XDG roots, a private control socket, a config with
//! every automatic lane off, `--no-reroute`, `SHELL=/bin/sh`
//! (`support/launch_isolation.rs`). A headless instance opens no window.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

#[path = "support/headless_boot.rs"]
mod headless_boot;
#[path = "support/launch_isolation.rs"]
mod launch_isolation;

use headless_boot::Instance;

/// Boot one headless instance, 40x120 ([`headless_boot::boot`]: `None` is an
/// environment refusal; a product that cannot start fails the test).
fn boot(tag: &str) -> Option<Instance> {
    boot_rows(tag, 40)
}

/// [`boot`] with `lines` rows.
fn boot_rows(tag: &str, lines: u16) -> Option<Instance> {
    headless_boot::boot(
        &format!("atav{tag}"),
        &["--lines", &lines.to_string(), "--columns", "120"],
    )
}

/// A client that has not exited by now is hung: past every [`HANG`] wait a
/// call makes, so a wait that times out answers before it is killed.
const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(120);

/// How long a wait for something that MUST happen may take before it is read
/// as a hang (AGENTS.md: a hang detector is a minute, never a latency budget).
const HANG: Duration = Duration::from_secs(60);
const HANG_MS: &str = "timeout=60000";

/// An `await agent` whose verdict already holds answers at once: the fastest
/// of [`LATCH_TRIES`] asks, `aterm ctl`'s spawn included, within this.
const LATCHED_PROMPTLY: Duration = Duration::from_secs(2);
const LATCH_TRIES: usize = 3;

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
    let busy = status_until(&inst, &sid, HANG, "agent=busy", |s| {
        field(s, "agent") == Some("busy")
    });
    let program = status_until(&inst, &sid, HANG, "program=claude", |s| {
        field(s, "program") == Some("claude")
    });
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
        HANG,
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
            &[&format!("@{sid}"), "await", "agent", "prompt", HANG_MS],
        );
        move || cmd.output().expect("await agent")
    });
    std::thread::sleep(Duration::from_millis(200));

    std::fs::write(&go, b"").expect("create the go file");
    let t0 = Instant::now();
    // The status poll proves WHAT the verdict says; how fast it moved is
    // bounded below, between two pushes, never by this poll's spawns.
    // Until the verdict names the box's kind too: the box's frame can be read
    // before its body is drawn, and that screen is honestly `prompt` with
    // `agent_detail=other` — the drawn body then moves the detail (one more
    // rev and one more push, which everything below reads from this status).
    // A box that never reads as the rm circuit-breaker still fails here, with
    // the last status in the message.
    let prompt = status_until(
        &inst,
        &sid,
        HANG,
        "agent=prompt agent_detail=bash:not-read-only",
        |s| {
            field(s, "agent") == Some("prompt")
                && field(s, "agent_detail") == Some("bash:not-read-only")
        },
    );
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
    // Already true: a fresh await answers at once, on the same revision. A
    // wait that parked answers only at its timeout (exit 124, which fails
    // `ctl_ok`); one answered late — by a later re-check or publication rather
    // than the latch — is late every time it is asked. So it is asked up to
    // [`LATCH_TRIES`] times and the FASTEST, `aterm ctl`'s spawn included,
    // must be within [`LATCHED_PROMPTLY`]: a loaded gate's one slow spawn is
    // asked again (it was one ask within 2 s, then within 30 s).
    let mut fastest = Duration::MAX;
    for _ in 0..LATCH_TRIES {
        let t1 = Instant::now();
        let latched = ctl_ok(
            &inst,
            &[&format!("@{sid}"), "await", "agent", "busy,prompt", HANG_MS],
        );
        let took = t1.elapsed();
        assert!(
            latched.contains(&format!("OK agent prompt rev={rev}")),
            "{latched}"
        );
        fastest = fastest.min(took);
        if fastest < LATCHED_PROMPTLY {
            break;
        }
    }
    assert!(
        fastest < LATCHED_PROMPTLY,
        "latched, not parked: the fastest of {LATCH_TRIES} answered in {fastest:?}"
    );
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
    // Collected until both ends are seen (the loop breaks then): the deadline
    // only ends a stream that never carries one.
    let deadline = Instant::now() + HANG;
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

/// INT-4 + INT-5, live: a plain shell running `echo "…?"; sleep 300` is named
/// by its foreground program (`program=sleep`, no shell integration) and is
/// NOT an agent: `agent=-`, never `level=attention`. The `sleep` outlives every
/// wait on it by far (it was 30 s, under a one-minute wait), so the sample
/// below is taken while it still runs; the instance's hangup ends it.
#[test]
fn a_shell_question_is_not_an_agent_and_its_program_is_named() {
    let Some(inst) = boot("s") else { return };
    let (_local, sid) = boot_session(&inst);
    type_line(
        &inst,
        &sid,
        "echo \"Checking whether the disk is full?\"; sleep 300",
    );
    let rec = status_until(&inst, &sid, HANG, "program=sleep", |s| {
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
            HANG_MS,
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

/// A fake Claude Code that draws its idle frame — the cursor on its caret
/// row at column 2, where Claude Code keeps it — waits for `die` to exist,
/// then clears the screen (as Claude restores the terminal) and exits.
const FAKE_EXIT: &str = r#"#!/bin/sh
die="$1"
rule=$(printf '%120s' '' | sed 's/ /─/g')
printf '\033[2J\033[H'
printf '⏺ Done.\n\n%s\n❯ \n%s\n  ? for shortcuts\n' "$rule" "$rule"
printf '\033[3A\033[3G'
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
    status_until(&inst, &sid, HANG, "program=bash", |s| {
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
        let running = status_until(&inst, &sid, HANG, "program=claude, read as an agent", |s| {
            field(s, "program") == Some("claude")
                && matches!(field(s, "agent"), Some("idle" | "question"))
        });
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
            if t0.elapsed() > HANG {
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
    let deadline = Instant::now() + HANG;
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

/// A fake Claude Code LAUNCH, drawn as 2.1.283 draws one (measured frame by
/// frame on 2026-09-26: aterm-phase's `LAUNCH_*` fixtures): nothing of it on
/// the screen — the shell's rows stand, as they do between the launch (or
/// the folder-trust dialog, which it erases) and the REPL — until `go`
/// exists; then its REPL on the alternate screen: the banner, the composer
/// between its two rules, the mode footer, drawn from the top row down, and
/// the terminal's cursor on the caret row at column 2, where Claude Code
/// keeps it (measured with the cursor on 2026-09-27).
const FAKE_LAUNCH: &str = r#"#!/bin/sh
go="$1"
rule=$(printf '%120s' '' | sed 's/ /─/g')
while [ ! -e "$go" ]; do sleep 0.05; done
printf '\033[?1049h\033[2J\033[H'
printf ' Claude Code v2.1.283\n\n%s\n❯ \n%s\n  ⏵⏵ bypass permissions on (shift+tab to cycle)\n' "$rule" "$rule"
printf '\033[4;3H'
while :; do sleep 1; done
"#;

/// The same launch under Claude Code's INLINE renderer (its classic
/// main-screen one, measured on 2.1.283 on 2026-09-26: aterm-phase's
/// `INLINE_*` fixtures): the REPL drawn on the MAIN grid under the launch
/// line, the rows below it left blank, the cursor on the caret row at column
/// 2. Once `quit` exists it exits as Claude Code does: its prompt box left
/// on the main grid, the cursor on the row under its footer, where the
/// shell's prompt comes back (measured, 2026-09-27).
const FAKE_INLINE_LAUNCH: &str = r#"#!/bin/sh
go="$1"
quit="$2"
rule=$(printf '%120s' '' | sed 's/ /─/g')
while [ ! -e "$go" ]; do sleep 0.05; done
printf ' Claude Code v2.1.283\n\n%s\n❯ \n%s\n  ⏵⏵ bypass permissions on (shift+tab to cycle)\n' "$rule" "$rule"
printf '\033[3A\033[3G'
while [ ! -e "$quit" ]; do sleep 0.05; done
printf '\033[3B\r'
"#;

/// NEW-1 of the live e2e (2026-09-26), live: `await agent idle` on a Claude
/// Code that has started but not drawn its REPL PARKS — the server reads
/// the shell's rows under Claude Code's reader as `agent=unknown`, not
/// `idle` — and latches only once the composer is on the screen. On the old
/// reader those rows read idle, the wait answered at once, and a first
/// prompt typed then into Claude Code 2.1.283 was lost (measured: 5 of 5
/// after the trust dialog). THE CONTROL: the REPL drawn, the same wait
/// latches, and the screen it answered on holds the composer.
#[test]
fn await_agent_idle_waits_for_claude_codes_composer() {
    waits_for_the_composer("l", 40, FAKE_LAUNCH);
}

/// The same in a pane TALLER than the verdict's 40 rows (the review of
/// 2026-09-26): the REPL drawn from the top row down on the alternate
/// screen, and under the launch line on the main grid (the inline
/// renderer), leaves blank rows below it, and the server read the grid's
/// last 40 rows — the prompt box's caret above them — so the wait answered
/// `OK timeout` (8 of 8 live inline launches at 150x50). The verdict reads
/// the last 40 DRAWN rows: the wait latches on the REPL at 50 rows and at
/// 80.
#[test]
fn await_agent_idle_waits_for_a_composer_above_the_last_40_rows() {
    waits_for_the_composer("t", 50, FAKE_LAUNCH);
    waits_for_the_composer("i", 50, FAKE_INLINE_LAUNCH);
    waits_for_the_composer("j", 80, FAKE_INLINE_LAUNCH);
}

/// Launch `fake` as `claude` in a `lines`-row instance: `await agent idle`
/// parks while nothing of the REPL is drawn (`agent=unknown`), and latches
/// on the REPL once it is.
fn waits_for_the_composer(tag: &str, lines: u16, fake: &str) {
    let Some(inst) = boot_rows(tag, lines) else {
        return;
    };
    let (_, sid) = boot_session(&inst);
    let script = inst.tmp.join("fake-launch.sh");
    std::fs::write(&script, fake).expect("write the fake");
    let go = inst.tmp.join("go");
    launch_fake(&inst, &sid, &script, &go, &inst.tmp.join("quit"));
    let screen = parks_then_latches(&inst, &sid, &go, &format!("{lines} rows"));
    // The geometry under test: in a pane taller than 40 rows, the caret sits
    // above the grid's last 40 rows (40 or more rows below it).
    let below = screen
        .lines()
        .rev()
        .position(|r| r.starts_with('❯'))
        .expect("the caret");
    if lines > 40 {
        assert!(
            below >= 40,
            "{lines} rows: {below} rows under the caret: {screen}"
        );
    }
}

/// THE SAME TAB, RELAUNCHED (the review of 2026-09-26): Claude Code's
/// inline renderer run again in the tab it exited in leaves the previous
/// run's prompt box on the main grid above the new launch line, and the
/// server — reading that box by its frame alone — published `agent=idle`
/// before the new REPL was drawn: `await agent idle` answered at once, and
/// a draft typed then was lost (3 of 3 after the folder-trust dialog, 1 of
/// 3 without it, on 2.1.283). Live, in a 50-row pane: a first fake run
/// draws its REPL and exits, its box left on the grid; the second launch in
/// the same tab reads `agent=unknown` — the cursor under its launch line,
/// in no prompt box — and the wait parks until the new REPL is drawn, then
/// latches on it: the caret it answered on is under the SECOND launch line.
/// THE CONTROL: the first run in the fresh tab latches the same way.
#[test]
fn await_agent_idle_waits_for_the_relaunched_repl_in_the_same_tab() {
    let Some(inst) = boot_rows("r", 50) else {
        return;
    };
    let (_, sid) = boot_session(&inst);
    let script = inst.tmp.join("fake-inline.sh");
    std::fs::write(&script, FAKE_INLINE_LAUNCH).expect("write the fake");
    let (go1, quit1) = (inst.tmp.join("go1"), inst.tmp.join("quit1"));
    launch_fake(&inst, &sid, &script, &go1, &quit1);
    parks_then_latches(&inst, &sid, &go1, "the first run");
    std::fs::write(&quit1, b"").expect("the first run's exit");
    status_until(
        &inst,
        &sid,
        HANG,
        "the first run exited to its shell",
        |s| field(s, "program") != Some("claude") && field(s, "agent") == Some("-"),
    );
    let between = ctl_ok(&inst, &[&format!("@{sid}"), "text"]);
    assert_eq!(
        between.lines().filter(|r| r.starts_with('❯')).count(),
        1,
        "the first run's prompt box is left on the grid: {between}"
    );
    let (go2, quit2) = (inst.tmp.join("go2"), inst.tmp.join("quit2"));
    launch_fake(&inst, &sid, &script, &go2, &quit2);
    let screen = parks_then_latches(&inst, &sid, &go2, "the relaunch");
    let rows: Vec<&str> = screen.lines().collect();
    let launches: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].contains("exec -a claude"))
        .collect();
    let carets: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].starts_with('❯'))
        .collect();
    assert_eq!(
        (launches.len(), carets.len()),
        (2, 2),
        "two launches, two prompt boxes: {screen}"
    );
    assert!(
        carets[0] < launches[1] && launches[1] < carets[1],
        "the old box above the second launch line, the new one under it: {screen}"
    );
}

/// Type the launch of `script` as `claude` (`exec -a`), its go-file `go` and
/// quit-file `quit`.
fn launch_fake(inst: &Instance, sid: &str, script: &Path, go: &Path, quit: &Path) {
    // THE SHELL FIRST. A line typed before bash has drawn its prompt is echoed
    // twice: once raw by the tty in canonical mode, then again when readline
    // starts and redisplays the pending input after its prompt. The merge
    // contract measured it on 2026-09-27 under a loaded machine: the relaunch
    // test counted three launch lines for two launches and failed. Every launch
    // waits for a prompt row (`$ ` at the end, the default PS1) — the same wait a
    // person makes before typing.
    ctl_ok(
        inst,
        &[&format!("@{sid}"), "await", "match", r"[$#]\s*$", HANG_MS],
    );
    type_line(
        inst,
        sid,
        &format!(
            "/bin/bash -c 'exec -a claude /bin/sh {} {} {}'",
            script.display(),
            go.display(),
            quit.display()
        ),
    );
}

/// With a fake launched and nothing of its REPL drawn: once the server names
/// it an agent, `await agent idle` PARKS (`agent=unknown`) for 1.5 s; then
/// `go` is created, the REPL is drawn, and the wait latches on it. Returns
/// the screen it answered on, which holds the REPL.
fn parks_then_latches(inst: &Instance, sid: &str, go: &Path, what: &str) -> String {
    let named = status_until(inst, sid, HANG, "program=claude, read as an agent", |s| {
        field(s, "program") == Some("claude") && field(s, "agent") != Some("-")
    });
    // A waiter parked on idle while nothing of the REPL is drawn.
    let waiter = std::thread::spawn({
        let mut cmd = client_command(
            inst,
            &[&format!("@{sid}"), "await", "agent", "idle", HANG_MS],
        );
        move || cmd.output().expect("await agent idle")
    });
    std::thread::sleep(Duration::from_millis(1500));
    let before = status(inst, sid);
    assert!(
        !waiter.is_finished(),
        "{what}: `await agent idle` answered before the REPL was drawn: {before} \
         (named: {named})\n{}",
        ctl_ok(inst, &[&format!("@{sid}"), "text"])
    );
    assert_eq!(field(&before, "program"), Some("claude"), "{before}");
    assert_eq!(field(&before, "agent"), Some("unknown"), "{what}: {before}");
    std::fs::write(go, b"").expect("create the go file");
    let waited = waiter.join().expect("await thread");
    let waited = String::from_utf8_lossy(&waited.stdout).into_owned();
    assert!(
        waited.contains("OK agent idle rev="),
        "{what}: await agent: {waited}"
    );
    let screen = ctl_ok(inst, &[&format!("@{sid}"), "text"]);
    assert!(
        screen.lines().any(|r| r.starts_with('❯')) && screen.contains("Claude Code v2.1.283"),
        "{what}: the wait answered on the REPL: {screen}"
    );
    screen
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
    let deadline = Instant::now() + HANG;
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
                    .recv_timeout(HANG)
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
