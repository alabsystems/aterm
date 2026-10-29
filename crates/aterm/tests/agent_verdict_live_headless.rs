// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance tests for server-published agent verdict and program identity.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::headless_boot;

use headless_boot::{
    Instance, boot_session, client_command, ctl, ctl_ok, field, status, subscribed, type_line,
    until_served, watchers,
};

/// Boot one headless instance ([`headless_boot::boot`]).
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

/// How long a wait for something that must happen may take before it is read as a hang (AGENTS.md).
const HANG: Duration = Duration::from_secs(60);
const HANG_MS: &str = "timeout=60000";

/// Poll `status` until `pred` holds.
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
    let screen = ctl_ok(inst, &[&format!("@{sid}"), "text"]);
    let tree = headless_boot::process_tree(inst);
    panic!(
        "{what}: not within {within:?}; last status: {last}\nscreen:\n{screen}\n\
         processes under the instance (pid ppid stat wchan etime command):\n{tree}"
    );
}

/// The fake worker.
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

#[test]
fn a_box_under_a_ticking_row_is_published_pushed_and_awaitable() {
    let inst = boot("b");
    let (local, sid) = boot_session(&inst);
    let script = inst.tmp.join("fake.sh");
    let go = inst.tmp.join("go");
    std::fs::write(&script, FAKE_WORKER).expect("write the fake worker");

    let mut sub = subscribed(&inst, &[&format!("@{sid}"), "events"]);
    let events = sub.stdout.take().expect("subscribe stdout");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(events).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

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

    let watched = watchers(&inst, &sid);
    let waiter = std::thread::spawn({
        let mut cmd = client_command(
            &inst,
            &[&format!("@{sid}"), "await", "agent", "prompt", HANG_MS],
        );
        move || until_served(|| cmd.output().expect("await agent"))
    });
    let armed_by = Instant::now() + HANG;
    while watchers(&inst, &sid) <= watched {
        assert!(!waiter.is_finished(), "the await ended before it armed");
        assert!(Instant::now() < armed_by, "the await never armed");
        std::thread::sleep(Duration::from_millis(20));
    }

    std::fs::write(&go, b"").expect("create the go file");
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
    assert_eq!(
        field(&prompt, "revision"),
        Some(revision.as_str()),
        "NEGATIVE CONTROL: revision= moved, so this run does not prove the gate: {before} / {prompt}"
    );
    let rev = field(&prompt, "agent_rev").unwrap().to_string();
    let agent_gen = field(&prompt, "agent_gen").unwrap_or("-");
    let agent_fp = field(&prompt, "agent_fp").unwrap_or("-");
    assert!(
        agent_gen
            .split_once('.')
            .is_some_and(|(e, s)| { e.parse::<u64>().is_ok() && s.parse::<u64>().is_ok() })
            && agent_fp.len() == 16,
        "{prompt}"
    );

    let waited = waiter.join().expect("await thread");
    let waited = String::from_utf8_lossy(&waited.stdout).into_owned();
    assert!(
        waited.contains("OK agent prompt rev="),
        "await agent: {waited}"
    );
    let latched = ctl_ok(
        &inst,
        &[&format!("@{sid}"), "await", "agent", "busy,prompt", HANG_MS],
    );
    assert!(
        latched.contains(&format!("OK agent prompt rev={rev}")),
        "{latched}"
    );
    let bad = ctl(&inst, &[&format!("@{sid}"), "await", "agent", "promtp"]);
    assert!(
        String::from_utf8_lossy(&bad.stdout).contains("ERR usage")
            || String::from_utf8_lossy(&bad.stderr).contains("ERR usage"),
        "{bad:?}"
    );

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

    let roster = ctl_ok(&inst, &["sessions"]);
    let row = roster.lines().find(|l| l.contains(&sid)).expect("row");
    assert_eq!(field(row, "program"), Some("claude"), "{row}");
    assert_eq!(field(row, "agent"), Some("prompt"), "{row}");
}

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

/// A fake Claude Code that draws its idle frame.
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

#[test]
fn an_agent_that_exits_is_no_agent_from_its_exit_on() {
    let inst = boot("x");
    let (local, sid) = boot_session(&inst);
    let script = inst.tmp.join("fake-exit.sh");
    let die = inst.tmp.join("die");
    std::fs::write(&script, FAKE_EXIT).expect("write the fake");

    let mut sub = subscribed(&inst, &[&format!("@{sid}"), "events"]);
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

/// A fake Claude Code launch, drawn as 2.1.283 draws one (aterm-phase's `LAUNCH_*` fixtures).
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

/// [`FAKE_LAUNCH`] under Claude Code's inline (main-screen) renderer; it exits once `quit` exists.
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

/// Launch `fake` as `claude` in a `lines`-row instance.
fn waits_for_the_composer(tag: &str, lines: u16, fake: &str) {
    let inst = boot_rows(tag, lines);
    let (_, sid) = boot_session(&inst);
    let script = inst.tmp.join("fake-launch.sh");
    std::fs::write(&script, fake).expect("write the fake");
    let go = inst.tmp.join("go");
    launch_fake(&inst, &sid, &script, &go, &inst.tmp.join("quit"));
    let screen = parks_then_latches(&inst, &sid, &go, &format!("{lines} rows"));
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

/// Type the launch of `script` as `claude` (`exec -a`), its go-file `go` and quit-file `quit`.
fn launch_fake(inst: &Instance, sid: &str, script: &Path, go: &Path, quit: &Path) {
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

/// With a fake launched and nothing of its REPL drawn, `await agent idle` parks; once `go`
/// exists it latches. Returns the screen it answered on.
fn parks_then_latches(inst: &Instance, sid: &str, go: &Path, what: &str) -> String {
    let named = status_until(inst, sid, HANG, "program=claude, read as an agent", |s| {
        field(s, "program") == Some("claude") && field(s, "agent") != Some("-")
    });
    let waiter = std::thread::spawn({
        let mut cmd = client_command(
            inst,
            &[&format!("@{sid}"), "await", "agent", "idle", HANG_MS],
        );
        move || until_served(|| cmd.output().expect("await agent idle"))
    });
    let now = ctl(
        inst,
        &[&format!("@{sid}"), "await", "agent", "idle", "timeout=0"],
    );
    let now = String::from_utf8_lossy(&now.stdout).into_owned();
    let before = status(inst, sid);
    assert!(
        now.trim() == "OK timeout" && !waiter.is_finished(),
        "{what}: `await agent idle` answered before the REPL was drawn: {now} {before} \
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

/// A fake Claude Code that draws the rows of the file `$1` top down.
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
