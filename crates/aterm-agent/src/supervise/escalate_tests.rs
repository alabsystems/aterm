// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const CLAUDE: &dyn ScreenReader = &aterm_phase::ClaudeReader;

fn turn(phase: Phase, rows: Vec<String>) -> Turn {
    Turn {
        phase,
        screen: Screen {
            rows,
            ..Screen::default()
        },
        timed_out: false,
    }
}

#[test]
fn the_attention_names_the_kind_the_subject_and_the_reason() {
    let rows: Vec<String> = include_str!("policy/fixtures/cap-rm.txt")
        .lines()
        .map(str::to_string)
        .collect();
    let t = attention_text(
        CLAUDE,
        &turn(Phase::Prompt, rows),
        "rm circuit breaker (newline reading): unresolved $1",
    );
    assert!(t.starts_with("claude rm-breaker: "), "{t}");
    assert!(
        t.contains("rm -rf $S/$1") && t.contains("unresolved $1"),
        "{t}"
    );
    let trust: Vec<String> = include_str!("policy/fixtures/cap-trust.txt")
        .lines()
        .map(str::to_string)
        .collect();
    let t = attention_text(CLAUDE, &turn(Phase::Prompt, trust), "x");
    assert!(t.starts_with("claude trust: "), "{t}");
    assert!(t.contains("/private/tmp/claude-502/scratch/work1"), "{t}");
}

#[test]
fn an_rm_breaker_boxs_attention_fits_the_keyed_cap_and_stays_closed() {
    let rows: Vec<String> = include_str!("policy/fixtures/cap-rm.txt")
        .lines()
        .map(str::to_string)
        .collect();
    let why = "the box carries a vendor note: Dangerous rm operation on possibly-empty \
               variable path: $S/$1 in `rm -rf $S/$1` (bind $1 and rewrite its $S as \
               \"${S:?}\" or use a literal path) ⚠ Claude Code will automatically deny \
               this request in 1:59, to avoid blocking progress on an unattended session";
    let t = attention_text(CLAUDE, &turn(Phase::Prompt, rows), why);
    assert!(t.len() <= 200, "{} bytes: {t}", t.len());
    assert!(t.ends_with("…)"), "{t}");
    assert!(t.starts_with("claude rm-breaker: "), "{t}");
}

#[test]
fn a_question_dialogs_badge_names_its_question() {
    let rule = "─".repeat(120);
    let mut rows: Vec<String> = [
        "⏺ chunk 3 done",
        "",
        "❯ Stop now and ask me to choose between option A (delete a.txt) and option B.",
        &rule,
        " ☐ a.txt",
        "",
        "What should I do with a.txt?",
        "",
        "❯ 1. Delete a.txt",
        "     Remove the a.txt file",
        "  2. Keep it",
        "     Leave a.txt in place",
        "  3. Type something.",
        &rule,
        "  4. Chat about this",
        "",
        "Enter to select · ↑/↓ to navigate · Esc to cancel",
    ]
    .map(str::to_string)
    .to_vec();
    let t = attention_text(CLAUDE, &turn(Phase::Prompt, rows.clone()), "no rule");
    assert!(t.contains("What should I do with a.txt?"), "{t}");
    assert!(!t.contains("Chat about this"), "{t}");
    rows[6] = "Pick one.".to_string();
    rows[0] = "⏺ Which one should I do?".to_string();
    let t = attention_text(CLAUDE, &turn(Phase::Prompt, rows), "no rule");
    assert!(!t.contains("Which one should I do?"), "{t}");
}

#[test]
fn a_question_read_whole_is_badged_by_its_question() {
    use aterm_phase::prompt::fixtures as f;
    let rows = f::screen(f::QUESTION_TABS_INCIDENT);
    let t = attention_text(
        CLAUDE,
        &turn(Phase::Prompt, rows),
        "a person has begun answering",
    );
    assert!(
        t.starts_with("claude question: On a row with a progress bar, how should its"),
        "{t}"
    );
    assert!(!t.contains('│') && !t.contains("Chat about this"), "{t}");
}

#[test]
fn a_long_subject_and_reason_are_clipped_and_the_whole_fits_256_bytes() {
    let mut rows = aterm_phase::prompt::fixtures::bash_one_row();
    let at = rows
        .iter()
        .position(|r| r == "   git log --oneline -5")
        .expect("row");
    rows[at] = format!("   echo {}", "é".repeat(300));
    let t = attention_text(CLAUDE, &turn(Phase::Prompt, rows), &"why ".repeat(100));
    assert!(t.len() <= ATTENTION_BYTES, "{}", t.len());
    assert!(t.starts_with("claude bash: echo é"), "{t}");
    assert!(t.contains('…'));
}

#[test]
fn the_needs_entry_keeps_its_frame_for_a_long_need_in_any_script() {
    for letter in ["\u{915}", "\u{e01}", "\u{9a8}", "\u{25b8}"] {
        let needs = vec![letter.repeat(80), "sign 0.48.0".to_string()];
        let t = needs_attention_text("Muse Code", &needs, NeedTail::CarryingOn);
        assert!(t.len() <= ATTENTION_BYTES, "{letter}: {}", t.len());
        assert!(
            t.starts_with(&format!("Muse Code needs you: {letter}"))
                && t.ends_with("\u{2026} (+1 more) \u{2014} carrying on"),
            "{letter}: {t}"
        );
    }
    let need = "grant Screen Recording to com.aterm.aterm".to_string();
    assert_eq!(
        needs_attention_text("claude", std::slice::from_ref(&need), NeedTail::CarryingOn),
        "claude needs you: grant Screen Recording to com.aterm.aterm \u{2014} carrying on"
    );
    assert_eq!(
        needs_attention_text("agy", &[need], NeedTail::WaitsOnYou),
        "agy needs you: grant Screen Recording to com.aterm.aterm \u{2014} waits on you"
    );
}

#[test]
fn the_memory_escalation_names_its_conversation_whole_or_not_at_all() {
    const ID: &str = "5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";
    let mut rows = aterm_phase::prompt::fixtures::bash_one_row();
    let at = rows
        .iter()
        .position(|r| r == "   git log --oneline -5")
        .expect("row");
    rows[at] = format!("   echo {}", "x".repeat(200));
    let point = turn(Phase::Prompt, rows);
    let banner = format!("{} (140.4GB)", aterm_phase::anchor!("wall.memory"));
    let flagged =
        format!("claude --dangerously-skip-permissions --model claude-opus-4-1 --resume {ID}");
    let reason = |line: &str| {
        format!(
            "memory critical: restart it{}{line}: {banner}",
            crate::harness::resume::THEN_RESUME_WITH
        )
    };
    let whole_or_absent = |t: &str| {
        t.match_indices("resume with claude")
            .all(|(i, _)| t[i..].contains(&format!("--resume {ID}")))
            && t.match_indices("--resum")
                .all(|(i, _)| t[i..].starts_with(&format!("--resume {ID}")))
    };
    for (name, reason) in [
        ("flagged", reason(&flagged)),
        (
            "flagged, (relaunch is off)",
            format!("{} (relaunch is off)", reason(&flagged)),
        ),
        (
            "behind a failed restart",
            format!(
                "the restart could not be made (failed:signal-refused): {}",
                reason(&flagged)
            ),
        ),
        ("bare", reason(&format!("claude --resume {ID}"))),
    ] {
        let t = attention_text(CLAUDE, &point, &reason);
        assert!(t.len() <= ATTENTION_BYTES, "{name}: {}", t.len());
        assert!(whole_or_absent(&t), "{name}: a cut id: {t}");
        assert!(t.starts_with("claude bash: echo x"), "{name}: {t}");
    }
    let t = attention_text(CLAUDE, &point, &reason(&flagged));
    assert!(
        t.contains(&format!("then resume with claude --resume {ID}")),
        "{t}"
    );
    let label = point_label(CLAUDE, &point);
    let old = fit_reason(&label, &reason(&flagged));
    assert!(
        old.contains("then resume with claude --") && !old.contains(ID),
        "{old}"
    );
}

#[test]
fn a_status_field_is_read_off_the_ok_line() {
    let st = "OK schema=1 sid=- hold=0 fabric=absent\n";
    assert_eq!(status_field(st, "fabric"), Some("absent"));
    assert_eq!(status_field(st, "hold"), Some("0"));
    assert_eq!(status_field(st, "identity"), None);
    assert_eq!(status_field(st, "nope"), None);
}

#[test]
fn every_rm_breaker_kind_is_badged_rm_breaker() {
    use aterm_phase::prompt::fixtures as f;
    let incident = f::screen(f::BOX_RM_UNRESOLVABLE_WORKFLOW);
    let (kind, _) = kind_and_subject(&aterm_phase::ClaudeReader, &turn(Phase::Prompt, incident));
    assert_eq!(kind, "rm-breaker");
    let (kind, _) = kind_and_subject(
        &aterm_phase::ClaudeReader,
        &turn(Phase::Prompt, f::screen(f::BOX_RM)),
    );
    assert_eq!(kind, "rm-breaker");
    let (kind, _) = kind_and_subject(
        &aterm_phase::ClaudeReader,
        &turn(Phase::Prompt, f::bash_one_row()),
    );
    assert_eq!(kind, "bash");
    for (text, want) in [
        (f::BOX_RM_TOO_MANY_2_1_307, "rm-breaker"),
        (f::BOX_RM_TOO_MANY_NO_RM_2_1_307, "rm-breaker"),
        (f::BOX_RM_TOO_MANY_NO_RM_BYPASS_2_1_307, "rm-breaker"),
        (f::BOX_RM_NESTED_2_1_307, "rm-breaker"),
        (f::BOX_RM_MOUNTED_DRIVE_2_1_307, "rm-breaker"),
        (f::BOX_BASH_TOUCH_2_1_307, "bash"),
        (f::BOX_STATEMENT_FOLLOWS_16_2_1_307, "bash"),
        (f::BOX_RM_TOO_MANY_2_1_308, "rm-breaker"),
        (f::BOX_RM_TOO_MANY_NO_RM_2_1_308, "rm-breaker"),
        (f::BOX_RM_MOUNTED_DRIVE_2_1_308, "rm-breaker"),
        (f::BOX_RM_TOO_MANY_NO_RM_BYPASS_2_1_308, "rm-breaker"),
        (f::BOX_RM_NESTED_2_1_308, "rm-breaker"),
        (f::BOX_BASH_TOUCH_2_1_308, "bash"),
        (f::BOX_STATEMENT_FOLLOWS_16_2_1_308, "bash"),
        (f::BOX_RM_PARSE_DIVERGED_2_1_309, "rm-breaker"),
        (f::BOX_BASH_TOUCH_2_1_309, "bash"),
    ] {
        let (kind, _) = kind_and_subject(
            &aterm_phase::ClaudeReader,
            &turn(Phase::Prompt, f::screen(text)),
        );
        assert_eq!(kind, want, "{}", text.lines().next().unwrap_or(""));
    }
}

/// THE SIGN-IN STEP IS NEVER CUT OFF THE BADGE: a lost login's escalation through the badge's
/// own cut ([`attention_text`]: its reason within [`REASON_CELLS`] and within the bytes the
/// wall's label leaves), on agy's 401 and 400, Muse Code's rejected key, and Claude Code's
/// lost login at every length of its wall's words from a short one to past the badge, with
/// `retry_api_errors` off and over a draft (`turn_end::login_gone`).
#[test]
fn a_lost_logins_sign_in_step_survives_the_badges_own_cut() {
    use crate::supervise::SupervisorConfig;
    use crate::supervise::policy::turn_end::{
        self, Composer, TurnEndAction, TurnEndReading, TurnEndState, decide_turn_end,
    };
    let t = std::time::Instant::now() + std::time::Duration::from_secs(10 * 3600);
    let off = SupervisorConfig {
        retry_api_errors: false,
        ..SupervisorConfig::default()
    };
    // Read as `turn_end`'s own tests read each program's screen.
    let badge =
        |program: &str, rows: &[String], composer: Option<Composer>, c: &SupervisorConfig| {
            let claude = program == "claude";
            let reading = aterm_phase::read(Some(program), rows, claude.then_some(2));
            let mut r = TurnEndReading::of(
                &reading,
                rows,
                false,
                Some(std::time::Duration::from_secs(180)),
                None,
                None,
            );
            r.restartable = claude;
            if let Some(composer) = composer {
                r.composer = composer;
            }
            let how = turn_end::sign_in_how(r.program, &r.wall_message);
            let mut st = TurnEndState::default();
            st.observe(&r, t);
            let a = decide_turn_end(&st, &r, c, t);
            let TurnEndAction::Escalate { reason } = &a else {
                panic!("{program}: not an escalation: {a:?}");
            };
            let text = attention_text(
                r.program.reader(),
                &turn(Phase::Idle, rows.to_vec()),
                reason,
            );
            (text, how)
        };
    let screen = aterm_phase::prompt::fixtures::screen;
    for (program, text) in [
        ("agy", aterm_phase::agy::fixtures::lab::ERROR_401),
        ("agy", aterm_phase::agy::fixtures::lab::ERROR_400_KEY),
        ("muse", aterm_phase::muse::fixtures::KEY_REJECTED),
    ] {
        for c in [SupervisorConfig::default(), off.clone()] {
            let (text, how) = badge(program, &screen(text), None, &c);
            assert!(
                text.contains(&how),
                "{program}: the badge {text:?} lost {how:?}"
            );
        }
    }
    let wall_rows = |words: &str| {
        let mut rows = screen(aterm_phase::prompt::fixtures::LOGIN_EXPIRED);
        let at = rows
            .iter()
            .position(|row| row.starts_with("⏺ Login expired"))
            .expect("the wall row");
        rows[at] = format!("⏺ {words}");
        rows
    };
    // From a wall that fits both cuts to one past the cells, in plain and in multibyte words
    // (`—` is three bytes in one cell): between them the bytes the label leaves run out first.
    for (n, filler) in (0..=100).flat_map(|n| [(n, "x"), (n, "—")]) {
        let words = format!("Please run /login · API Error: 401 {}", filler.repeat(n));
        let rows = wall_rows(&words);
        for (composer, c) in [
            (None, &off),
            (Some(Composer::Typed), &SupervisorConfig::default()),
        ] {
            let (text, how) = badge("claude", &rows, composer.clone(), c);
            assert!(
                text.contains(&how) && text.contains("the login is gone"),
                "{n} ({composer:?}): the badge {text:?} lost {how:?}"
            );
        }
    }
}

#[test]
fn the_kinds_field_names_an_owner_by_its_whole_name() {
    for yes in [
        "ask:needs",
        "-:supervisor,ask:needs",
        "wait:-,-:needs,ask:x",
    ] {
        assert!(kinds_name_owner(yes, "needs"), "{yes}");
    }
    for no in ["ask:supervisor,-:-", "ask:needs-2", "ask:a:needs", "-", ""] {
        assert!(!kinds_name_owner(no, "needs"), "{no}");
    }
}
