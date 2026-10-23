// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless integration test for the worker held-screen stall deadline and Escape recovery.

#![cfg(unix)]

use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::{child_output, headless_boot, launch_isolation};

use headless_boot::{Instance, boot_session, client_command, ctl, ctl_ok, field};

/// The stand-in `claude`: `--version` answers at once (the helper runs every
/// fake once that way); any other call runs the fake worker under the name
/// `claude` (`exec -a`), from the instance's scratch home.
const STANDIN: &str = "#!/bin/sh\n\
    case \"$1\" in --version) echo '2.1.0 (Claude Code)'; exit 0 ;; esac\n\
    exec /bin/bash -c 'exec -a claude /bin/sh \"$HOME/fake.sh\" \"$HOME/screens\"'\n";

/// The fake worker, in raw mode, drawing as Claude Code draws: a conversation
/// (`ready.top`), then its framed composer (`rule`) holding what is typed,
/// wrapped, the cursor after its last character. At `ready` the first Enter
/// submits what was typed (the harness's LAND instruction) and draws the
/// conversation with it and the worker's clean defense (`defense.top`), its
/// composer empty again: `landed`. The submitted prompt is drawn as Claude
/// Code draws one in the conversation — a `❯ ` row, wrapped at word
/// boundaries onto rows indented two columns — and no busy frame comes
/// between the Enter and the defense, so the harness knows its act was
/// taken only by that row over the answer (`aterm_phase::answered_prompt`,
/// the lab's measured `d1l` answer drawn faster than any read): a prompt
/// split mid-word under a `> ` head reads as no answer, and the land act's
/// defense would land nothing. There an Enter (the manager's follow-up)
/// draws `box`; at `box` an Esc draws `back`. It reads what is there at once
/// (raw mode returns what is pending), so a long instruction is drawn whole
/// in one frame; end of input ends it.
const FAKE: &str = r#"#!/bin/sh
dir="$1"
stty raw -echo
cr=$(printf '\r'); esc=$(printf '\033'); sc=$(printf '\0337'); rc=$(printf '\0338')
rule=$(cat "$dir/rule")
screen() {
  printf '\033[H\033[2J'; cat "$1"; printf '%s\r\n' "$rule"
  if [ -z "$2" ]; then printf '\342\235\257 %s' "$sc"
  else printf '%s\n' "$2" | fold -w 96 | awk -v sc="$sc" 'NR>1{printf "\r\n  %s",$0;next}{printf "\342\235\257 %s",$0} END{printf "%s",sc}'
  fi
  printf '\r\n%s\r\n  ? for shortcuts%s' "$rule" "$rc"
}
draw() { printf '\033[H\033[2J'; cat "$dir/$1.scr"; }
enter() {
  if [ "$at" = ready ]; then
    { cat "$dir/ready.top"
      printf '%s\n' "$buf" | fold -s -w 96 | awk 'NR>1{printf "  %s\r\n",$0;next}{printf "\342\235\257 %s\r\n",$0}'
      printf '\r\n'; cat "$dir/defense.top"; } > "$dir/landed.top"
    at=landed; buf=""; screen "$dir/landed.top" ""
  else at=box; buf=""; draw box; fi
}
feed() {
  rest="$1"
  while [ -n "$rest" ]; do
    case "$at" in
      ready|landed)
        case "$rest" in
          *"$cr"*) buf="$buf${rest%%"$cr"*}"; rest="${rest#*"$cr"}"; enter ;;
          *) buf="$buf$rest"; rest=""; screen "$dir/$at.top" "$buf" ;;
        esac ;;
      box)
        case "$rest" in *"$esc"*) at=back; draw back ;; esac; rest="" ;;
      *) rest="" ;;
    esac
  done
}
at=ready; buf=""; screen "$dir/ready.top" ""
while :; do
  c=$(dd bs=4096 count=1 2>/dev/null; printf .); c="${c%.}"
  [ -z "$c" ] && exit 0
  feed "$c"
done
"#;

/// A wait for something that MUST happen (AGENTS.md: a hang detector).
const HANG: Duration = Duration::from_secs(60);
/// [`HANG`] as a server-side wait's bound.
const HANG_MS: &str = "timeout=60000";
/// The instance's width: every rule the screens draw is this wide.
const COLUMNS: usize = 100;

/// Claude Code's framed composer, empty, under `rows`; the cursor's row (0-based).
fn composed(rows: &[&str]) -> (Vec<String>, usize) {
    let mut out: Vec<String> = rows.iter().map(|r| (*r).to_string()).collect();
    out.push("─".repeat(COLUMNS));
    let caret = out.len();
    out.push("❯ ".to_string());
    out.push("─".repeat(COLUMNS));
    out.push("  ? for shortcuts".to_string());
    (out, caret)
}

/// The rows, then the cursor moved to `(row, col)` (0-based).
fn render(rows: &[String], cursor: (usize, usize)) -> String {
    format!(
        "{}\x1b[{};{}H",
        rows.join("\r\n"),
        cursor.0 + 1,
        cursor.1 + 1
    )
}

/// The task's turn as Claude Code ends it: its report, a done claim in
/// ordinary words (no token), so its manager's wait skips the short turn's
/// back-off and the LAND instruction goes at once.
const TASK_TURN: [&str; 4] = [
    "❯ Fix the parser.",
    "",
    "⏺ Fixed the parser; the suite is green. The task is complete.",
    "",
];
/// The worker's answer to the LAND instruction: a clean defense.
const DEFENSE: [&str; 2] = [
    "⏺ Nothing left: the parser fix is verified, committed and on the remote main branch.",
    "",
];
/// The follow-up the manager types.
const FOLLOW_UP: &str = "go on with the docs";

/// Write the fake and its three screens into the instance's scratch home.
fn write_worker(home: &Path) {
    let dir = home.join("screens");
    std::fs::create_dir_all(&dir).expect("the screens' folder");
    std::fs::write(home.join("fake.sh"), FAKE).expect("the fake");
    let top = |rows: &[&str]| rows.iter().map(|r| format!("{r}\r\n")).collect::<String>();
    std::fs::write(dir.join("ready.top"), top(&TASK_TURN)).expect("ready");
    std::fs::write(dir.join("defense.top"), top(&DEFENSE)).expect("the defense");
    std::fs::write(dir.join("rule"), "─".repeat(COLUMNS)).expect("the rule");
    let mut held: Vec<String> = TASK_TURN.iter().map(|r| (*r).to_string()).collect();
    held.extend(
        [
            &format!("❯ {FOLLOW_UP}"),
            "",
            "We're changing auto mode to no longer charge for classifier requests in Claude Code.",
            "However, this session isn't eligible because your requests go through 127.0.0.1:4000,",
            "which isn't compatible with this update.",
            "Nothing breaks: auto mode keeps working, and its classifier requests are billed as before.",
            "To fix it and access the new version of auto mode, ask your gateway to implement:",
            "https://code.claude.com/docs/en/auto-mode-classifier-billing",
            "",
            "Enter to continue · Esc to cancel",
        ]
        .map(str::to_string),
    );
    let at = held.len() - 1;
    std::fs::write(dir.join("box.scr"), render(&held, (at, 0))).expect("box");
    let mut back: Vec<&str> = TASK_TURN.to_vec();
    let asked = format!("❯ {FOLLOW_UP}");
    back.extend([asked.as_str(), "", "⏺ Wrote the docs.", ""]);
    let (back, caret) = composed(&back);
    std::fs::write(dir.join("back.scr"), render(&back, (caret, 2))).expect("back");
}

/// One headless instance whose harness supervises its workers, the held
/// deadline `held_ms`, the stand-in first on its PATH.
#[track_caller]
fn boot(tag: &str, held_ms: &str) -> Instance {
    headless_boot::boot_with(
        &format!("atwh{tag}"),
        |tmp, cmd| {
            let cfg = tmp.join("cfg/aterm/aterm.toml");
            let base = std::fs::read_to_string(&cfg).expect("the isolation config");
            assert!(base.contains(launch_isolation::HARNESS_OFF), "{base}");
            let base = base.replace(launch_isolation::HARNESS_OFF, "");
            std::fs::write(
                &cfg,
                format!("{base}[harness]\nrelaunch = false\nupgrade = false\n"),
            )
            .expect("write the config");
            let bin = tmp.join("bin");
            headless_boot::standin(&bin.join("claude"), STANDIN);
            write_worker(&tmp.join("home"));
            std::fs::create_dir_all(tmp.join("work")).expect("the worker's folder");
            cmd.env("SHELL", "/bin/zsh")
                .env(
                    "PATH",
                    format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
                )
                .env("ATERM_STATE_HOME", tmp.join("state"))
                .env("ATERM_LOG", "info")
                .env("ATERM_TASK_HELD_MS", held_ms)
                .args(["--lines", "30", "--columns", &COLUMNS.to_string()]);
        },
        |_| true,
    )
}

/// Start a worker on `task` as session `manager` runs `aterm ctl @self spawn
/// agent=claude -`, from the instance's work folder: its sid.
fn spawn_worker(inst: &Instance, manager: &str, task: &str) -> String {
    let mut cmd = client_command(
        inst,
        &["@self", "spawn", "agent=claude", "raise=false", "-"],
    );
    cmd.env("ATERM_PARENT_SESSION_ID", manager)
        .current_dir(inst.tmp.join("work"))
        .stdin(Stdio::piped());
    let out = headless_boot::until_served(|| {
        let mut child = cmd.spawn().expect("spawn aterm ctl");
        let mut stdin = child.stdin.take().expect("its stdin");
        stdin.write_all(task.as_bytes()).expect("write the task");
        drop(stdin);
        child_output::wait_and_read(child, Instant::now() + headless_boot::CLIENT_EXIT_DEADLINE)
            .expect("aterm ctl exits")
    });
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "spawn agent=claude: {stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
        .split_whitespace()
        .find(|w| w.starts_with("s-"))
        .unwrap_or_else(|| panic!("a sid: {stdout}"))
        .to_string()
}

/// The worker as the instance sees it, for a failed wait.
fn diagnosis(inst: &Instance, worker: &str) -> String {
    let ask = |args: &[&str]| String::from_utf8_lossy(&ctl(inst, args).stdout).into_owned();
    format!(
        "screen:\n{}\nstatus: {}\ntask: {}\nlog:\n{}",
        ask(&[&format!("@{worker}"), "text", "trim"]),
        ask(&[&format!("@{worker}"), "status"]),
        ask(&[&format!("@{worker}"), "task"]),
        headless_boot::log_tail(&inst.log),
    )
}

/// `aterm ctl await task <target> timeout=60000`: the one row it prints, or
/// a panic with the worker's diagnosis.
fn await_task(inst: &Instance, target: &str, worker: &str) -> String {
    let out = ctl(inst, &["await", "task", target, HANG_MS]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "await task {target}: {stdout} {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        diagnosis(inst, worker)
    );
    stdout
        .lines()
        .find(|l| l.starts_with('@'))
        .unwrap_or_else(|| panic!("a row: {stdout}"))
        .to_string()
}

/// Undo the wire's pct-encoding.
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn a_worker_held_at_a_screen_no_rule_knows_is_stalled_and_its_manager_answered() {
    let inst = boot("a", "3000");
    let (_, manager) = boot_session(&inst);
    let worker = spawn_worker(&inst, &manager, "Fix the parser.");

    let done = await_task(&inst, &format!("@{worker}:0"), &worker);
    assert_eq!(field(&done, "state"), Some("done"), "{done}");

    let turn = ctl(
        &inst,
        &[
            &format!("@{worker}"),
            "turn",
            "typed=1",
            "settle=submitted",
            FOLLOW_UP,
        ],
    );
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&turn.stdout),
        String::from_utf8_lossy(&turn.stderr)
    );
    assert!(turn.status.success(), "turn: {said}");
    let verdict = said
        .lines()
        .find(|l| l.contains(" task_rev="))
        .unwrap_or_else(|| panic!("a worker's verdict: {said}"));
    assert_eq!(field(verdict, "submitted"), Some("1"), "{verdict}");
    let found = field(verdict, "task_rev").expect("task_rev=").to_string();

    let unknown = ctl(
        &inst,
        &[&format!("@{worker}"), "await", "agent", "unknown", HANG_MS],
    );
    assert!(
        unknown.status.success()
            && String::from_utf8_lossy(&unknown.stdout).starts_with("OK agent unknown"),
        "the notice reads agent=unknown: {:?}\n{}",
        unknown,
        diagnosis(&inst, &worker)
    );

    let row = await_task(&inst, &format!("@{worker}:{found}"), &worker);
    assert_eq!(field(&row, "state"), Some("stalled"), "{row}");
    let attention = pct_decode(field(&row, "attention").expect("attention="));
    assert!(
        attention.starts_with("the worker is held: ")
            && attention.contains("agent=unknown")
            && attention.contains("Esc to cancel"),
        "{attention}"
    );
    let record = ctl_ok(&inst, &[&format!("@{worker}"), "task"]);
    assert!(
        record.contains(" state=stalled ") && record.contains(" why=held"),
        "{record}"
    );

    ctl_ok(&inst, &[&format!("@{worker}"), "key", "escape"]);
    let give_up = Instant::now() + HANG;
    loop {
        let record = ctl_ok(&inst, &[&format!("@{worker}"), "task"]);
        let moved = match (field(&record, "state"), field(&record, "why")) {
            (Some("working"), why) => {
                assert_eq!(why, Some("moved"), "moved on, known started: {record}");
                true
            }
            (Some("done"), _) => true,
            _ => false,
        } && field(&record, "attention") == Some("-");
        if moved {
            break;
        }
        assert!(
            Instant::now() < give_up,
            "the stall did not end within {HANG:?}: {record}\n{}",
            diagnosis(&inst, &worker)
        );
        std::thread::sleep(headless_boot::POLL_GAP);
    }
}
