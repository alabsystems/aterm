// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies that turn submission into Claude Code settles only when the typed line leaves the
//! composer.

#![cfg(unix)]

use std::time::{Duration, Instant};

use crate::headless_boot;

use aterm_types::control_verbs::TURN_ECHO_CAP_MS;
use headless_boot::{Instance, boot_session, ctl, ctl_ok, field, status, type_line};

const HANG: Duration = Duration::from_secs(60);
const HANG_MS: &str = "timeout=60000";
/// Longer than a `turn` gives the echo of a text its program has read.
pub(crate) const PAST_THE_ECHO_CAP: Duration = Duration::from_millis(TURN_ECHO_CAP_MS + 1_000);
/// Shorter than the cap by less than a `late` stand-in's draw comes after its read, so a line
/// read this long after its typing is drawn past the cap.
const INSIDE_THE_ECHO_CAP: Duration = Duration::from_millis(TURN_ECHO_CAP_MS - 1_000);

/// Claude Code's frame around a composer, a logo that repaints whatever the input loop does, and
/// that loop, which reads nothing while `$0.hold` stands. Its first word is `takes` for a
/// composer whose Enter moves the line up to the next history row (row 3 first) and empties it.
/// Its second word is `still` for a logo that stands still while the hold does, as a starved
/// Claude Code, one process, draws nothing while it reads nothing, or `late` for a loop that
/// draws what it read 1.5 s after the read. Its third word is `bracketed` for a composer that
/// turns DEC 2004 on, as Claude Code does: a paste is the text between its brackets, however its
/// reads split it, an Enter inside an open paste is text (drawn `⏎`), never a submit, and each
/// paste is counted once it closes, on row 24 (`bracketed pastes: <n>`). The shell reads bytes
/// (`LC_ALL=C`), so a read that ends inside a character is taken as it comes.
pub(crate) const COMPOSER: &str = r#"#!/bin/sh
mode="$1"
LC_ALL=C
export LC_ALL
stty raw -echo 2>/dev/null
esc=$(printf '\033')
pastes=0
inpaste=0
carry=
[ "$3" = bracketed ] && printf '%s[?2004h' "$esc"
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
    [ "$2" = still ] && [ -e "$0.hold" ] ||
      printf '%s7%s[1;1H%s[3%dm▐▛███▜▌%s[0m%s8' "$esc" "$esc" "$esc" "$c" "$esc" "$esc"
    sleep 0.1
  done
) &
painter=$!
trap 'kill $painter 2>/dev/null' EXIT
cr=$(printf '\r')
draft=
out=3
while [ -e "$0.hold" ]; do sleep 0.1; done
while :; do
  chunk=$(dd bs=256 count=1 2>/dev/null; printf x)
  chunk=${chunk%x}
  [ -z "$chunk" ] && exit 0
  [ "$2" = late ] && sleep 1.5
  if [ "$3" = bracketed ]; then
    chunk=$carry$chunk
    carry=
    # A read that ends part-way into a bracket keeps that part for the next read.
    case $chunk in
      *"$esc"|*"$esc["|*"$esc[2"|*"$esc[20"|*"$esc[20"[01])
        carry=$esc${chunk##*"$esc"}
        chunk=${chunk%"$esc"*}
        ;;
    esac
    rest=$chunk
    chunk=
    closed=$pastes
    while [ -n "$rest" ]; do
      if [ "$inpaste" = 1 ]; then
        case $rest in
          *"$esc[201~"*)
            part=${rest%%"$esc[201~"*}
            rest=${rest#*"$esc[201~"}
            inpaste=0
            pastes=$((pastes + 1))
            ;;
          *) part=$rest rest= ;;
        esac
        while :; do
          case $part in
            *"$cr"*) part="${part%%"$cr"*}⏎${part#*"$cr"}" ;;
            *) break ;;
          esac
        done
      else
        case $rest in
          *"$esc[200~"*)
            part=${rest%%"$esc[200~"*}
            rest=${rest#*"$esc[200~"}
            inpaste=1
            ;;
          *) part=$rest rest= ;;
        esac
      fi
      chunk=$chunk$part
    done
    [ "$pastes" != "$closed" ] &&
      printf '%s7%s[24;1H%s[2Kbracketed pastes: %d%s8' "$esc" "$esc" "$esc" "$pastes" "$esc"
  fi
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

fn spawn(inst: &Instance) -> String {
    let body = ctl_ok(inst, &["spawn"]);
    body.split_whitespace()
        .find(|w| w.starts_with("s-"))
        .unwrap_or_else(|| panic!("a sid: {body}"))
        .to_string()
}

/// [`COMPOSER`] started at `sid`'s shell prompt in `mode`, as Claude Code (its argv0 `claude`,
/// once aterm reads it so) where `claude` says.
pub(crate) fn start(
    inst: &Instance,
    sid: &str,
    script: &std::path::Path,
    claude: bool,
    mode: &str,
) {
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

fn boot(prefix: &str) -> (Instance, std::path::PathBuf) {
    let inst = headless_boot::boot_with(
        prefix,
        |_, cmd| {
            cmd.args(["--lines", "24", "--columns", "80"]);
        },
        |_| true,
    );
    let script = inst.tmp.join("composer.sh");
    std::fs::write(&script, COMPOSER).expect("write the stand-in");
    (inst, script)
}

fn composer(inst: &Instance, sid: &str) -> String {
    let screen = ctl_ok(inst, &[&format!("@{sid}"), "text"]);
    screen
        .lines()
        .nth(20)
        .unwrap_or_else(|| panic!("no row 21: {screen}"))
        .to_string()
}

/// Once the session holds the follow-up's line, unread.
fn typed(inst: &Instance, sid: &str) {
    let deadline = Instant::now() + HANG;
    loop {
        let s = status(inst, sid);
        if field(&s, "input_bytes") == Some("8") {
            return;
        }
        assert!(Instant::now() < deadline, "the line was never typed: {s}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The composer row once it reads `want`, or what it last read when the one-minute hang
/// detector ends: the stand-in reads its input in chunks, so a verdict can land while the
/// rest of a line it keeps is still being drawn.
fn composer_settles(inst: &Instance, sid: &str, want: &str) -> String {
    let deadline = Instant::now() + HANG;
    loop {
        let row = composer(inst, sid);
        if row == want || Instant::now() >= deadline {
            return row;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_follow_up_into_claude_code_is_submitted_only_when_its_composer_takes_it() {
    let (inst, script) = boot("atts");
    let (_, ignores) = boot_session(&inst);
    let takes = spawn(&inst);
    let plain = spawn(&inst);
    start(&inst, &ignores, &script, true, "ignores");
    start(&inst, &takes, &script, true, "takes");
    start(&inst, &plain, &script, false, "ignores");

    std::thread::scope(|scope| {
        let ignored = scope.spawn(|| {
            let verdict = follow_up(&inst, &ignores, 1, 2_000);
            assert!(
                verdict.contains("submitted=0 status=timeout"),
                "{verdict}\n{}",
                ctl_ok(&inst, &[&format!("@{ignores}"), "text"])
            );
            assert_eq!(
                composer_settles(&inst, &ignores, "❯ carry on"),
                "❯ carry on"
            );
        });

        let taken = scope.spawn(|| {
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
        });

        let plained = scope.spawn(|| {
            let verdict = follow_up(&inst, &plain, 1, 60_000);
            assert!(
                verdict.contains("submitted=1 status=submitted"),
                "{verdict}\n{}",
                ctl_ok(&inst, &[&format!("@{plain}"), "text"])
            );
            assert_eq!(
                composer_settles(&inst, &plain, "❯ carry on"),
                "❯ carry on",
                "the repaint alone verified a line still in the composer"
            );
        });
        for run in [ignored, taken, plained] {
            run.join()
                .unwrap_or_else(|why| std::panic::resume_unwind(why));
        }
    });
}

/// A starved Claude Code's echo comes later than any cap on it. While the line stands unread the
/// Enter waits for it, so a repaint verifies nothing, and the composer alone judges the Enter: the
/// stand-in that ignores it, its logo still while it reads nothing, answers `submitted=0` and the
/// one that takes it, its logo repainting all along, `submit_by=composer`.
#[test]
fn a_line_claude_code_has_not_read_is_pressed_once_its_composer_shows_it() {
    let (inst, script) = boot("attu");
    let hold = inst.tmp.join("composer.sh.hold");
    std::fs::write(&hold, "").expect("hold the stand-ins");
    let (_, ignores) = boot_session(&inst);
    let takes = spawn(&inst);
    start(&inst, &ignores, &script, true, "ignores still");
    start(&inst, &takes, &script, true, "takes");

    std::thread::scope(|scope| {
        let ignored = scope.spawn(|| follow_up(&inst, &ignores, 1, 2_000));
        let taken = scope.spawn(|| follow_up(&inst, &takes, 1, 60_000));
        for sid in [&ignores, &takes] {
            typed(&inst, sid);
        }
        std::thread::sleep(PAST_THE_ECHO_CAP);
        std::fs::remove_file(&hold).expect("feed the stand-ins");

        let verdict = ignored.join().expect("the ignored turn");
        assert!(
            verdict.contains("submitted=0 status=timeout"),
            "{verdict}\n{}",
            ctl_ok(&inst, &[&format!("@{ignores}"), "text"])
        );
        assert_eq!(composer(&inst, &ignores), "❯ carry on");
        let verdict = taken.join().expect("the taken turn");
        assert!(
            verdict.contains("submitted=1 status=submitted")
                && verdict.contains(" submit_by=composer"),
            "{verdict}\n{}",
            ctl_ok(&inst, &[&format!("@{takes}"), "text"])
        );
        assert_eq!(composer(&inst, &takes), "❯");
    });
}

/// A starved Claude Code can read the line while its echo settles, before `turn` first looks at
/// the composer, and draw it only past the cap the typing started. The first look that finds the
/// line read gives the echo its cap again, so a repaint verifies nothing and the composer that
/// keeps the line answers `submitted=0`.
#[test]
fn a_line_read_before_any_look_is_pressed_once_its_composer_shows_it() {
    let (inst, script) = boot("attl");
    let hold = inst.tmp.join("composer.sh.hold");
    std::fs::write(&hold, "").expect("hold the stand-in");
    let (_, late) = boot_session(&inst);
    start(&inst, &late, &script, true, "ignores late");

    std::thread::scope(|scope| {
        let ignored = scope.spawn(|| follow_up(&inst, &late, 1, 2_000));
        typed(&inst, &late);
        std::thread::sleep(INSIDE_THE_ECHO_CAP);
        std::fs::remove_file(&hold).expect("feed the stand-in");

        let verdict = ignored.join().expect("the turn");
        assert!(
            verdict.contains("submitted=0 status=timeout"),
            "{verdict}\n{}",
            ctl_ok(&inst, &[&format!("@{late}"), "text"])
        );
        assert_eq!(composer_settles(&inst, &late, "❯ carry on"), "❯ carry on");
    });
}
