// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A TURN'S SUBMIT INTO CLAUDE CODE, VERIFIED BY ITS COMPOSER, through the one
//! `aterm` binary against a real `aterm --headless` (P2 of the rl review;
//! `docs/DESIGN-sessions-as-subagents-2026-10-15.md` §2.4). Claude Code
//! 2.1.301 animates its logo's colours at an idle prompt, so a content
//! advance after the Enter — AUTO's `seq` verify at a program's prompt — is
//! no proof the line landed. On a session the server reads as Claude Code,
//! whose composer shows the typed line, the submit is verified by the line
//! LEAVING the composer.
//!
//! A STAND-IN (a POSIX `sh` script run as `claude` with `exec -a`) draws a
//! Claude Code prompt — the logo row, the composer between its two rules, the
//! mode row — recolours the logo every 0.1 s, and echoes what it is typed into
//! its composer. One that IGNORES the Enter keeps the line there, and the
//! manager's follow-up (`turn typed=1 settle=submitted`) answers
//! `submitted=0`; one that TAKES it moves the line above and empties its
//! composer, and answers `submitted=1` — each the negative control of the
//! other. NEGATIVE CONTROL of the verify itself: the ignoring script run as
//! plain `sh` is no agent, keeps AUTO's `seq`, and the animation alone answers
//! `submitted=1` with the line still in its composer: the false proof the
//! composer verify closes.
//!
//! Every wait for something that must happen is a one-minute hang detector;
//! the one short `submit_window=` is the window under test, whose close is the
//! answer, never a latency budget.
//!
//! ISOLATION: `support/launch_isolation.rs` and `support/headless_boot.rs`
//! (scratch HOME and XDG roots, a private control socket, `[harness] enabled =
//! false`, so nothing but the test types into the stand-ins); a refused boot
//! is COULD NOT RUN (`boot_with`), never a pass.

#![cfg(unix)]

use std::time::{Duration, Instant};

use crate::headless_boot;

use headless_boot::{Instance, boot_session, ctl, ctl_ok, field, status, type_line};

/// A wait for something that MUST happen (AGENTS.md: a hang detector).
const HANG: Duration = Duration::from_secs(60);
const HANG_MS: &str = "timeout=60000";

/// The stand-in: `$1` is `takes` or `ignores`. Raw mode first, so a line typed
/// once the composer is drawn reaches the loop byte for byte. One read may hold
/// the line's last letters and the Enter together (a loaded host), so the
/// Enter's branch draws the whole draft too: one that ignores it keeps showing
/// what it was typed, as Claude Code's composer does (it drew `❯ carry o` for
/// good when the `n` came with the Enter).
const COMPOSER: &str = r#"#!/bin/sh
mode="$1"
stty raw -echo 2>/dev/null
esc=$(printf '\033')
rule=$(printf '%80s' '' | sed 's/ /─/g')
printf '%s[?1049h%s[2J' "$esc" "$esc"
printf '%s[1;1H▐▛███▜▌   Claude Code v2.1.301' "$esc"
printf '%s[20;1H%s%s[22;1H%s' "$esc" "$rule" "$esc" "$rule"
printf '%s[23;1H  ⏵⏵ bypass permissions on (shift+tab to cycle)' "$esc"
printf '%s[21;1H❯ ' "$esc"
(
  c=1
  while :; do
    c=$((c % 6 + 1))
    printf '%s7%s[1;1H%s[3%dm▐▛███▜▌%s[0m%s8' "$esc" "$esc" "$esc" "$c" "$esc" "$esc"
    sleep 0.1
  done
) &
painter=$!
trap 'kill $painter 2>/dev/null' EXIT
cr=$(printf '\r')
draft=
out=3
while :; do
  chunk=$(dd bs=256 count=1 2>/dev/null; printf x)
  chunk=${chunk%x}
  [ -z "$chunk" ] && exit 0
  case $chunk in
    *"$cr"*)
      draft="$draft${chunk%%"$cr"*}"
      if [ "$mode" = takes ]; then
        printf '%s[%d;1H❯ %s%s[21;1H%s[2K❯ ' "$esc" "$out" "$draft" "$esc" "$esc"
        out=$((out + 1))
        draft=
      else
        printf '%s[21;1H%s[2K❯ %s' "$esc" "$esc" "$draft"
      fi
      ;;
    *)
      draft="$draft$chunk"
      printf '%s[21;1H%s[2K❯ %s' "$esc" "$esc" "$draft"
      ;;
  esac
done
"#;

/// The follow-up the skills teach, the window `window_ms` per press.
fn follow_up(inst: &Instance, sid: &str, presses: u32, window_ms: u64) -> String {
    let presses = format!("presses={presses}");
    let window = format!("submit_window={window_ms}");
    let out = ctl(
        inst,
        &[
            &format!("@{sid}"),
            "turn",
            "typed=1",
            "settle=submitted",
            &presses,
            &window,
            HANG_MS,
            "carry on",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    stderr
        .lines()
        .find(|l| l.contains("submitted="))
        .unwrap_or_else(|| {
            panic!(
                "no verdict: stdout={:?} stderr={stderr:?}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
        .to_string()
}

/// The sid a `spawn` minted.
fn spawn(inst: &Instance) -> String {
    let body = ctl_ok(inst, &["spawn"]);
    body.split_whitespace()
        .find(|w| w.starts_with("s-"))
        .unwrap_or_else(|| panic!("a sid: {body}"))
        .to_string()
}

/// Start the stand-in in `sid` as `program` (`claude`: `exec -a`; else plain
/// `sh`) in `mode`, once the shell has drawn its prompt, and return once its
/// composer is drawn — and, as `claude`, the server reads it as an agent.
fn start(inst: &Instance, sid: &str, script: &std::path::Path, claude: bool, mode: &str) {
    ctl_ok(
        inst,
        &[&format!("@{sid}"), "await", "match", r"[$#]\s*$", HANG_MS],
    );
    let line = if claude {
        format!(
            "/bin/bash -c 'exec -a claude /bin/sh {} {mode}'",
            script.display()
        )
    } else {
        format!("/bin/sh {} {mode}", script.display())
    };
    type_line(inst, sid, &line);
    ctl_ok(inst, &[&format!("@{sid}"), "await", "match", "^❯", HANG_MS]);
    if claude {
        let deadline = Instant::now() + HANG;
        loop {
            let s = status(inst, sid);
            if field(&s, "program") == Some("claude") && field(&s, "agent") != Some("-") {
                break;
            }
            assert!(Instant::now() < deadline, "never read as Claude Code: {s}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// The composer's row (21 of 24), as `text` shows it.
fn composer(inst: &Instance, sid: &str) -> String {
    let screen = ctl_ok(inst, &[&format!("@{sid}"), "text"]);
    screen
        .lines()
        .nth(20)
        .unwrap_or_else(|| panic!("no row 21: {screen}"))
        .to_string()
}

#[test]
fn a_follow_up_into_claude_code_is_submitted_only_when_its_composer_takes_it() {
    let inst = headless_boot::boot_with(
        "atts",
        |_, cmd| {
            cmd.args(["--lines", "24", "--columns", "80"]);
        },
        |_| true,
    );
    let script = inst.tmp.join("composer.sh");
    std::fs::write(&script, COMPOSER).expect("write the stand-in");
    let (_, ignores) = boot_session(&inst);
    let takes = spawn(&inst);
    let plain = spawn(&inst);
    start(&inst, &ignores, &script, true, "ignores");
    start(&inst, &takes, &script, true, "takes");
    start(&inst, &plain, &script, false, "ignores");

    // Claude Code that ignores the Enter: the line stays, nothing verified.
    let verdict = follow_up(&inst, &ignores, 1, 2_000);
    assert!(
        verdict.contains("submitted=0 status=timeout"),
        "{verdict}\n{}",
        ctl_ok(&inst, &[&format!("@{ignores}"), "text"])
    );
    assert_eq!(composer(&inst, &ignores), "❯ carry on");

    // Claude Code that takes it: the composer empties, the turn answers.
    let verdict = follow_up(&inst, &takes, 1, 60_000);
    assert!(
        verdict.contains("submitted=1 status=submitted"),
        "{verdict}\n{}",
        ctl_ok(&inst, &[&format!("@{takes}"), "text"])
    );
    assert_eq!(composer(&inst, &takes), "❯");
    let screen = ctl_ok(&inst, &[&format!("@{takes}"), "text"]);
    assert!(
        screen.lines().any(|r| r == "❯ carry on"),
        "the line moved above: {screen}"
    );

    // NEGATIVE CONTROL: no agent read, AUTO's `seq`: the animation answers.
    let verdict = follow_up(&inst, &plain, 1, 60_000);
    assert!(
        verdict.contains("submitted=1 status=submitted"),
        "{verdict}\n{}",
        ctl_ok(&inst, &[&format!("@{plain}"), "text"])
    );
    assert_eq!(
        composer(&inst, &plain),
        "❯ carry on",
        "the repaint alone verified a line still in the composer"
    );
}
