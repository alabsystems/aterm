// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance for the server-published agent verdict and program identity
//! (`status program= agent= …`, `EVENT <local> agent <word> rev=<n> gen=<e.s> fp=<hex16>`,
//! `await agent`), driven through the one `aterm` binary against a real headless instance.
//! Fake agents are POSIX `sh` scripts run as `claude` via `exec -a`, drawing measured Claude
//! Code screens. Isolation is `support/launch_isolation.rs`'s: scratch HOME/XDG roots, a
//! private control socket, every automatic lane off, `--no-reroute`, `SHELL=/bin/sh`.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::headless_boot;

use headless_boot::{
    Instance, boot_session, client_command, ctl, ctl_ok, field, status, type_line,
};

/// Boot one headless instance, 40x120 ([`headless_boot::boot`]: an environment
/// refusal is COULD NOT RUN; a product that cannot start fails the test).
fn boot(tag: &str) -> Instance {
    boot_rows(tag, 40)
}

/// [`boot`] with `lines` rows.
fn boot_rows(tag: &str, lines: u16) -> Instance {
    headless_boot::boot(
        &format!("atav{tag}"),
        &["--lines", &lines.to_string(), "--columns", "120"],
    )
}

/// How long a wait for something that must happen may take before it is read as a hang
/// (AGENTS.md).
const HANG: Duration = Duration::from_secs(60);
const HANG_MS: &str = "timeout=60000";

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
    // The screen rides along: a status that never moves says nothing of whether
    // the launch line was typed, echoed, run or failed.
    let screen = ctl_ok(inst, &[&format!("@{sid}"), "text"]);
    let tree = headless_boot::process_tree(inst);
    panic!(
        "{what}: not within {within:?}; last status: {last}\nscreen:\n{screen}\n\
         processes under the instance (pid ppid stat wchan etime command):\n{tree}"
    );
}

/// The fake worker: a Claude-shaped busy screen (transcript row, spinner, composer between two
/// rules, footer) redrawn until `go` exists, then the rm circuit-breaker box measured on
/// 2.1.280, its top transcript row ticking every 0.2 s. POSIX sh + printf only.
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

/// The box under a ticking row reaches `status agent=prompt`, `EVENT <local> agent prompt` is
/// pushed, and `await agent prompt` latches, while `revision=` holds still.
#[test]
fn a_box_under_a_ticking_row_is_published_pushed_and_awaitable() {
    let inst = boot("b");
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
    let busy = status_until(&inst, &sid, HANG, "agent=busy", |s| {
        field(s, "agent") == Some("busy")
    });
    let program = status_until(&inst, &sid, HANG, "program=claude", |s| {
        field(s, "program") == Some("claude")
    });
    // Pin the revision once the FSM has published the running job, never on a timer:
    // `agent=busy` can lead the FSM's `running` (held for its 750 ms dwell, then published on
    // a sweep) by most of a second. After that nothing moves the revision while the fake
    // ticks, short of a 5 s stall (`quiet_after`), so the control below still fails for a
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
    // Until the verdict names the box's kind: its frame can be read before its body is drawn
    // (honestly `prompt` with `agent_detail=other`), and the drawn body then moves the detail.
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
    eprintln!("agent=prompt (busy: {busy}; program: {program})");
    assert_eq!(
        field(&prompt, "agent_detail"),
        Some("bash:not-read-only"),
        "{prompt}"
    );
    assert_eq!(field(&prompt, "level"), Some("attention"), "{prompt}");
    assert_eq!(field(&prompt, "why"), Some("prompt"), "{prompt}");
    assert_eq!(field(&prompt, "program"), Some("claude"), "{prompt}");
    // Negative control: the FSM's revision did not move across the change (the screen kept
    // moving, the job kept running), so a verdict gated on it would still say busy.
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
    // Already true: a fresh await answers on the same revision. A wait that
    // parked answers only at its timeout (exit 124, which fails `ctl_ok`).
    let latched = ctl_ok(
        &inst,
        &[&format!("@{sid}"), "await", "agent", "busy,prompt", HANG_MS],
    );
    assert!(
        latched.contains(&format!("OK agent prompt rev={rev}")),
        "{latched}"
    );
    // An unknown word is a usage error, not a wait.
    let bad = ctl(&inst, &[&format!("@{sid}"), "await", "agent", "promtp"]);
    assert!(
        String::from_utf8_lossy(&bad.stdout).contains("ERR usage")
            || String::from_utf8_lossy(&bad.stderr).contains("ERR usage"),
        "{bad:?}"
    );

    // The push.
    let want = format!("EVENT {local} agent prompt rev={rev} gen=");
    let deadline = Instant::now() + HANG;
    let mut seen = Vec::new();
    let mut pushed = false;
    while !pushed && Instant::now() < deadline {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(100)) {
            pushed = line.starts_with(&want) && line.contains(" fp=");
            if line.starts_with("EVENT") || line.starts_with("GAP") {
                seen.push(line);
            }
        }
    }
    let _ = sub.kill();
    let _ = sub.wait();
    assert!(pushed, "no `{want}` on the events stream: {seen:?}");

    // The roster says the same.
    let roster = ctl_ok(&inst, &["sessions"]);
    let row = roster.lines().find(|l| l.contains(&sid)).expect("row");
    assert_eq!(field(row, "program"), Some("claude"), "{row}");
    assert_eq!(field(row, "agent"), Some("prompt"), "{row}");
}

/// A plain shell running `echo "…?"; sleep 300` is named by its foreground program
/// (`program=sleep`, no shell integration) and is not an agent: `agent=-`, never
/// `level=attention`. The `sleep` outlives every wait; the instance's hangup ends it.
#[test]
fn a_shell_question_is_not_an_agent_and_its_program_is_named() {
    let inst = boot("s");
    let (_local, sid) = boot_session(&inst);
    type_line(
        &inst,
        &sid,
        "echo \"Checking whether the disk is full?\"; sleep 300",
    );
    let rec = status_until(&inst, &sid, HANG, "program=sleep", |s| {
        field(s, "program") == Some("sleep")
    });
    assert_eq!(field(&rec, "agent"), Some("-"), "{rec}");
    assert_ne!(field(&rec, "level"), Some("attention"), "{rec}");
    let screen = ctl_ok(&inst, &[&format!("@{sid}"), "text"]);
    assert!(
        screen.contains("disk is full?"),
        "the question is on screen: {screen}"
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

/// An agent that exits is no agent from its exit on: three times over, a fake Claude Code
/// exits to its bash, `program=` names the shell, `agent=-` is pushed, and no agent verdict is
/// published after the exit (the shell's prompt is never judged under Claude Code's reader).
#[test]
fn an_agent_that_exits_is_no_agent_from_its_exit_on() {
    let inst = boot("x");
    let (local, sid) = boot_session(&inst);
    let script = inst.tmp.join("fake-exit.sh");
    let die = inst.tmp.join("die");
    std::fs::write(&script, FAKE_EXIT).expect("write the fake");

    // One subscription for all three rounds, opened before anything moves.
    let mut sub = client_command(&inst, &["subscribe", &format!("@{sid}"), "events"])
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
                program == "claude" || agent == "-",
                "round {round}: `agent={agent}` (rev {r}) stood at {at:?} under program={program} \
                 — the departed agent's verdict: {seen:?}"
            );
        }
        let gone = format!("EVENT {local} agent - rev=");
        let deadline = Instant::now() + HANG;
        let mut pushed = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(line) = rx.recv_timeout(left) else {
                panic!("round {round}: no `agent -` on the stream: {pushed:?}");
            };
            if line.starts_with(&gone) {
                break;
            }
            pushed.push(line);
        }
    }
    let _ = sub.kill();
    let _ = sub.wait();
}

/// A fake Claude Code launch, drawn as 2.1.283 draws one (aterm-phase's `LAUNCH_*` fixtures):
/// nothing on the screen until `go` exists, then its REPL on the alternate screen (banner,
/// composer between its two rules, mode footer, top row down), the cursor on the caret row at
/// column 2.
///
/// It first writes and erases one cell (`x`, backspace, space, backspace), as a real Claude
/// Code moves the screen the moment it runs. The status sweep names a foreground group when it
/// changes and once more 500 ms later (`PROGRAM_NAME_CONFIRM`), then only when the screen moves
/// (a cursor-only move does not count), so a silent launch read between `exec -a`'s fork and
/// exec would leave `program=` at the interpreter.
const FAKE_LAUNCH: &str = r#"#!/bin/sh
go="$1"
printf 'x\b \b'
rule=$(printf '%120s' '' | sed 's/ /─/g')
while [ ! -e "$go" ]; do sleep 0.05; done
printf '\033[?1049h\033[2J\033[H'
printf ' Claude Code v2.1.283\n\n%s\n❯ \n%s\n  ⏵⏵ bypass permissions on (shift+tab to cycle)\n' "$rule" "$rule"
printf '\033[4;3H'
while :; do sleep 1; done
"#;

/// The same launch under Claude Code's inline (main-screen) renderer, measured on 2.1.283
/// (aterm-phase's `INLINE_*` fixtures): the REPL drawn on the main grid under the launch line,
/// the cursor on the caret row at column 2, the start-up cell as in [`FAKE_LAUNCH`]. Once
/// `quit` exists it exits as Claude Code does: its prompt box left on the grid, the cursor on
/// the row under its footer.
const FAKE_INLINE_LAUNCH: &str = r#"#!/bin/sh
go="$1"
quit="$2"
printf 'x\b \b'
rule=$(printf '%120s' '' | sed 's/ /─/g')
while [ ! -e "$go" ]; do sleep 0.05; done
printf ' Claude Code v2.1.283\n\n%s\n❯ \n%s\n  ⏵⏵ bypass permissions on (shift+tab to cycle)\n' "$rule" "$rule"
printf '\033[3A\033[3G'
while [ ! -e "$quit" ]; do sleep 0.05; done
printf '\033[3B\r'
"#;

/// `await agent idle` on a Claude Code that has started but not drawn its REPL parks (the
/// shell's rows under Claude Code's reader are `agent=unknown`) and latches once the composer
/// is on the screen; a first prompt typed earlier would be lost. In a pane taller than the
/// verdict's 40 rows the REPL leaves blank rows below it, and the verdict reads the last 40
/// drawn rows: the wait latches at 50 rows and at 80. The two instances run side by side.
#[test]
fn await_agent_idle_waits_for_a_composer_above_the_last_40_rows() {
    std::thread::scope(|scope| {
        let runs = [
            scope.spawn(|| waits_for_the_composer("t", 50, FAKE_LAUNCH)),
            scope.spawn(|| waits_for_the_composer("j", 80, FAKE_INLINE_LAUNCH)),
        ];
        for run in runs {
            run.join().expect("a run");
        }
    });
}

/// Launch `fake` as `claude` in a `lines`-row instance: `await agent idle`
/// parks while nothing of the REPL is drawn (`agent=unknown`), and latches
/// on the REPL once it is.
fn waits_for_the_composer(tag: &str, lines: u16, fake: &str) {
    let inst = boot_rows(tag, lines);
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

/// Claude Code's inline renderer, relaunched in the tab it exited in, leaves the previous
/// run's prompt box above the new launch line; a verdict read from that box's frame would
/// answer `await agent idle` before the new REPL is drawn. In a 50-row pane the second launch
/// reads `agent=unknown` and the wait latches on the new REPL, whose caret is under the second
/// launch line. Control: the first run latches the same way.
#[test]
fn await_agent_idle_waits_for_the_relaunched_repl_in_the_same_tab() {
    let inst = boot_rows("r", 50);
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
    // The shell first: a line typed before bash draws its prompt is echoed twice (raw by the
    // tty in canonical mode, again when readline redisplays the pending input), so every
    // launch waits for a prompt row, as a person would.
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

/// With a fake launched and nothing of its REPL drawn, `await agent idle` parks
/// (`agent=unknown`); then `go` is created and the wait latches on the REPL.
/// Returns the screen it answered on.
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

/// A fake Claude Code that draws the rows of the file `$1` top down (one per
/// grid row) and sits until the file `$2` exists.
const FAKE_SCREEN: &str = r#"#!/bin/sh
printf '\033[?1049h\033[2J'
r=0
while IFS= read -r line; do
  r=$((r+1))
  printf '\033[%d;1H%s' "$r" "$line"
done < "$1"
while [ ! -e "$2" ]; do sleep 0.1; done
printf '\033[?1049l'
"#;

/// A Claude screen at its weekly limit with a background shell counted on it (`· 1 shell ·`,
/// `1 shell still running`) is `agent=wall:usage-weekly`, not busy, and
/// `await agent wall:usage-weekly` latches. Negative control: the same screen with the
/// agent's spinner under it is `agent=busy`.
#[test]
fn a_weekly_wall_behind_a_shell_is_a_wall_not_busy() {
    const WALL: &str = include_str!(
        "../../aterm-phase/src/fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
    );
    let inst = boot("w");
    let (_local, sid) = boot_session(&inst);
    let script = inst.tmp.join("fake-screen.sh");
    std::fs::write(&script, FAKE_SCREEN).expect("write the fake");
    let draw = |name: &str, live: bool| {
        let rows: Vec<String> = aterm_agent::supervise::prompt::fixtures::screen(WALL)
            .into_iter()
            .map(|r| {
                if live && r.starts_with("✻ Sautéed") {
                    "✻ Cooking… (12s · ↓ 1.1k tokens · esc to interrupt)".to_string()
                } else if live && r.contains("bypass permissions on") {
                    "  ⏵⏵ bypass permissions on · esc to interrupt · 1 shell".to_string()
                } else {
                    r
                }
            })
            .collect();
        let file = inst.tmp.join(name);
        std::fs::write(&file, rows.join("\n")).expect("write the screen");
        file
    };
    let quit = inst.tmp.join("quit");
    let launch = |file: &Path| {
        ctl_ok(
            &inst,
            &[&format!("@{sid}"), "await", "match", r"[$#]\s*$", HANG_MS],
        );
        type_line(
            &inst,
            &sid,
            &format!(
                "/bin/bash -c 'exec -a claude /bin/sh {} {} {}'",
                script.display(),
                file.display(),
                quit.display()
            ),
        );
    };
    launch(&draw("wall.txt", false));
    let rec = status_until(&inst, &sid, HANG, "agent=wall:usage-weekly", |s| {
        field(s, "agent") == Some("wall:usage-weekly")
    });
    assert_eq!(field(&rec, "program"), Some("claude"), "{rec}");
    let held = ctl_ok(
        &inst,
        &[
            &format!("@{sid}"),
            "await",
            "agent",
            "wall:usage-weekly",
            HANG_MS,
        ],
    );
    assert!(held.starts_with("OK"), "the wait latches: {held}");
    // Negative control: the live screen is busy.
    std::fs::write(&quit, "").expect("quit");
    status_until(&inst, &sid, HANG, "the fake gone", |s| {
        field(s, "program") != Some("claude")
    });
    std::fs::remove_file(&quit).expect("rearm");
    launch(&draw("live.txt", true));
    let rec = status_until(&inst, &sid, HANG, "agent=busy", |s| {
        field(s, "agent") == Some("busy")
    });
    assert_eq!(field(&rec, "program"), Some("claude"), "{rec}");
}
