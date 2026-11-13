// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::tap;

fn said<'a>(prompt: &'a str, said: &'a str, status: Option<&'a str>) -> Words<'a> {
    Words {
        prompt,
        said,
        status,
    }
}

/// The row under the last composer bottom rule of `rows` (the rule spans the whole width).
fn under_the_rule(rows: &[String]) -> Option<&str> {
    let rule = rows
        .iter()
        .rposition(|r| !r.is_empty() && r.chars().all(|c| c == '\u{2500}'))?;
    rows.get(rule + 1).map(String::as_str)
}

/// The stand-in draws its statusLine's row where Claude Code 2.1.318 drew it in every
/// measured screen of aterm-phase's `claude-2.1.318-tap-row-*` fixtures (the row under the
/// composer's bottom rule, from column 2, cut with `…` at `cols − 3` in the 30-column pane),
/// and a silent command's screen has no row there. The drawn rows read as the tap's own words
/// ([`tap::parse_line`], the glass's claim) at 170, 100 and 70 columns and not at 30.
/// NEGATIVE CONTROLS: the row drawn uncut at 30 columns is not the measured one, and with a
/// row the silent screen's mode row would not sit under the rule.
#[test]
fn the_status_row_is_where_claude_2_1_318_draws_it() {
    const WEEK: &str = "week 37% left \u{b7} resets Sat 10:00";
    let cwd = std::path::Path::new("/tmp/af/m00/home");
    let fixtures = aterm_tempfile::manifest_dir().join("../aterm-phase/src/fixtures");
    for (name, words, claimed) in [
        ("claude-2.1.318-tap-row-week.txt", Some(WEEK), true),
        ("claude-2.1.318-tap-row-week-100col.txt", Some(WEEK), true),
        ("claude-2.1.318-tap-row-week-70col.txt", Some(WEEK), true),
        ("claude-2.1.318-tap-row-week-30col.txt", Some(WEEK), false),
        (
            "claude-2.1.318-tap-row-no-figure.txt",
            Some(tap::NO_WEEK),
            true,
        ),
        ("claude-2.1.318-tap-row-silent-command.txt", None, false),
    ] {
        let path = fixtures.join(name);
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let measured: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
        let measured_row = under_the_rule(&measured).expect(name);
        let cols = measured
            .iter()
            .rev()
            .find(|r| r.starts_with('\u{2500}'))
            .map(|r| r.chars().count())
            .expect(name);
        let drawn = screen(Mode::Idle, 3, cols, 40, cwd, &said("p", "s", words));
        let row = under_the_rule(&drawn).expect(name);
        match words {
            Some(_) => assert_eq!(row, measured_row, "{name} at {cols} columns"),
            None => {
                assert!(row.starts_with("  \u{23f5}\u{23f5}"), "{name}: {row:?}");
                assert!(measured_row.starts_with("  \u{23f5}\u{23f5}"), "{name}");
                let with = screen(Mode::Idle, 3, cols, 40, cwd, &said("p", "s", Some(WEEK)));
                assert_ne!(
                    under_the_rule(&with),
                    Some(measured_row),
                    "CONTROL: a row would stand where the silent screen has none"
                );
            }
        }
        let parsed = row.strip_prefix("  ").and_then(tap::parse_line);
        assert_eq!(parsed.is_some(), claimed, "{name}: {row:?}");
    }
    assert_ne!(
        format!("  {WEEK}"),
        status_row(WEEK, 30),
        "CONTROL: uncut, the 30-column row would run into the last two columns"
    );
}

/// The stand-in draws its rules in Claude's rule grey and its statusLine's row in the
/// statusLine's ink, and the rest in the terminal's own: the composer's two rules, the row
/// under the bottom one where a statusLine printed one, never the prompt or the mode row.
/// NEGATIVE CONTROL: with no statusLine row the mode row stands under the bottom rule, and it
/// keeps the terminal's ink.
#[test]
fn the_rules_and_the_status_row_wear_claudes_inks() {
    let cwd = std::path::Path::new("/tmp/af/m00/home");
    for mode in [Mode::Idle, Mode::Busy] {
        let rows = screen(
            mode,
            3,
            100,
            30,
            cwd,
            &said("p", "s", Some("week 95% left")),
        );
        let inks: Vec<Option<Ink>> = (0..rows.len()).map(|at| Ink::of(&rows, at)).collect();
        assert_eq!(
            inks[25..],
            [
                Some(Ink::Rule),
                None,
                Some(Ink::Rule),
                Some(Ink::Status),
                None
            ],
            "{mode:?}: {rows:?}"
        );
        assert_eq!(rows[28], "  week 95% left");
        let silent = screen(mode, 3, 100, 30, cwd, &said("p", "s", None));
        assert!(silent[29].starts_with("  \u{23f5}\u{23f5}"), "{silent:?}");
        assert_eq!(
            Ink::of(&silent, 29),
            None,
            "CONTROL: {mode:?}: the mode row"
        );
        assert_eq!(Ink::of(&silent, 28), Some(Ink::Rule));
    }
}

/// `status_line` reads the statusLine command and its refresh from either spelling of
/// `--settings` (aterm's tap value among them), not past a literal `--`; a settings value that
/// names no command, or a flag that is not `--settings`, names none.
#[test]
fn the_settings_name_the_status_command_and_its_refresh() {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    let tap_val =
        tap::settings_value(std::path::Path::new("/usr/local/bin/aterm")).expect("tap value");
    let named = StatusLine {
        command: "/usr/local/bin/aterm harness tap".into(),
        every: Some(Duration::from_secs(10)),
    };
    assert_eq!(
        status_line(&s(&["idle", "--settings", &tap_val])),
        Some(named.clone())
    );
    assert_eq!(
        status_line(&s(&[&format!("--settings={tap_val}"), "idle"])),
        Some(named)
    );
    assert_eq!(
        status_line(&s(&[
            "--settings",
            r#"{"statusLine":{"type":"command","command":"x"}}"#
        ])),
        Some(StatusLine {
            command: "x".into(),
            every: None,
        })
    );
    assert_eq!(status_line(&s(&["--", "--settings", &tap_val])), None);
    assert_eq!(
        status_line(&s(&["--settings", r#"{"model":"opus"}"#])),
        None
    );
    assert_eq!(status_line(&s(&["--model", "opus"])), None);
}

/// One run hands the command the payload on its stdin and `CLAUDE_PID` naming the stand-in,
/// and its row is the first line it printed; the payload carries the seeded `rate_limits` as
/// the tap reads them ([`tap::Reading::of_stdin`]). A command that prints nothing, or only a
/// blank line, has no row (Claude Code then loses the row). NEGATIVE CONTROL: a seed that is
/// not a JSON object leaves the payload without `rate_limits`, and the tap reads no figure.
#[test]
fn a_run_hands_the_payload_and_shows_the_first_line() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let kept = scratch.path().join("payload");
    let limits = r#"{"seven_day":{"used_percentage":63.0,"resets_at":1793000000}}"#;
    let payload = status_payload(Some("s"), scratch.path(), Some(limits));
    let row = run_status_line(
        &format!(
            "cat > '{}'; printf 'week %s\\nsecond\\n' \"$CLAUDE_PID\"",
            kept.display()
        ),
        &payload,
    );
    assert_eq!(row, Some(format!("week {}", std::process::id())));
    assert_eq!(std::fs::read_to_string(&kept).expect("handed"), payload);
    let vars = tap::TapVars {
        sid: Some("s-01".into()),
        pid: Some("7".into()),
        ..tap::TapVars::default()
    };
    let read = tap::Reading::of_stdin(&payload, &vars, 1_792_900_000).expect("a reading");
    let week = read.seven_day.expect("the seeded week");
    assert_eq!((week.x10(), week.resets_at), (630, 1_793_000_000));
    assert_eq!(run_status_line("true", &payload), None);
    assert_eq!(run_status_line("printf '\\n'", &payload), None);
    let unseeded = status_payload(None, scratch.path(), Some("[1, 2]"));
    assert!(!unseeded.contains("rate_limits"), "{unseeded}");
    let read = tap::Reading::of_stdin(&unseeded, &vars, 1_792_900_000).expect("a reading");
    assert!(!read.has_figures(), "CONTROL: no seed, no figure");
}

#[test]
fn every_frame_fills_its_screen_and_busy_says_esc_to_interrupt() {
    let cwd = std::path::Path::new("/tmp/af/m00/home");
    for cols in [20, 46, 60, 100] {
        for mode in [Mode::Busy, Mode::Question, Mode::Prompt, Mode::Idle] {
            let rows = screen(
                mode,
                7,
                cols,
                30,
                cwd,
                &said("a prompt", HOSTILE_SAID, None),
            );
            assert_eq!(rows.len(), 30, "{mode:?}: {cols}");
            assert!(
                rows.iter().all(|r| r.chars().count() <= cols),
                "{mode:?}: {cols}"
            );
            if matches!(mode, Mode::Busy | Mode::Idle) {
                assert_eq!(rows.iter().rposition(|row| row == "❯ "), Some(27));
                assert!(rows[28].chars().all(|ch| ch == '─'));
            }
            assert!(
                rows.iter().all(|r| !r.chars().any(char::is_control)),
                "{mode:?}"
            );
        }
    }
    let busy = screen(Mode::Busy, 7, 100, 30, cwd, &said("p", "s", None));
    assert!(busy[29].contains("esc to interrupt"), "{busy:?}");
    let idle = screen(Mode::Idle, 7, 100, 30, cwd, &said("p", "s", None));
    assert!(!idle[29].contains("esc to interrupt"), "{idle:?}");
}

#[test]
fn a_followed_mode_is_its_files_word_and_idle_without_one() {
    let scratch = aterm_tempfile::tempdir().expect("exclusive mode file");
    let file = scratch.path().join("mode");
    assert_eq!(followed_mode(&file), Mode::Idle, "no file yet");
    for (word, mode) in [
        ("busy\n", Mode::Busy),
        ("prompt", Mode::Prompt),
        ("question\n", Mode::Question),
        ("idle", Mode::Idle),
        ("busy", Mode::Busy),
        ("asleep", Mode::Idle),
    ] {
        std::fs::write(&file, word).expect("write the mode");
        assert_eq!(followed_mode(&file), mode, "{word:?}");
    }
}

#[test]
fn the_hostile_index_says_the_hostile_words() {
    assert_eq!(words(HOSTILE).1, HOSTILE_SAID);
    assert_ne!(words(1).1, HOSTILE_SAID);
}

/// A replay plays a ptylog's output records, each at its time from the log's first record
/// (an input record's time included), and skips the input records and a line that is no
/// record.
#[test]
fn a_replay_plays_the_output_records_at_their_times() {
    let log = "1000000000 O 1b5b3f3130343968\n\
               1005000000 I 61\n\
               no record here\n\
               1250000000 O 61\n\
               1250000001 O 6\n";
    assert_eq!(
        ptylog_output(log),
        vec![
            (Duration::ZERO, b"\x1b[?1049h".to_vec()),
            (Duration::from_millis(250), b"a".to_vec()),
        ]
    );
}
