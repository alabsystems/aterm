// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Auto-mode environment offer reader tests (`crate::prompt::fixtures::AUTO_MODE_SETUP*`).

use super::*;
use crate::phase::Phase;
use crate::prompt::fixtures::{
    AUTO_MODE_SETUP, AUTO_MODE_SETUP_40COL, AUTO_MODE_SETUP_60COL, AUTO_MODE_SETUP_FULLSCREEN,
    AUTO_MODE_SETUP_FULLSCREEN_40COL, AUTO_MODE_SETUP_FULLSCREEN_60COL,
    AUTO_MODE_SETUP_FULLSCREEN_SHELL, AUTO_MODE_SETUP_FULLSCREEN_SHELL_MODE, screen,
};
use crate::prompt::parse_prompt_v2;

const ALL: [(&str, &str); 8] = [
    ("inline", AUTO_MODE_SETUP),
    ("inline 60", AUTO_MODE_SETUP_60COL),
    ("inline 40", AUTO_MODE_SETUP_40COL),
    ("fullscreen", AUTO_MODE_SETUP_FULLSCREEN),
    ("fullscreen 60", AUTO_MODE_SETUP_FULLSCREEN_60COL),
    ("fullscreen 40", AUTO_MODE_SETUP_FULLSCREEN_40COL),
    ("fullscreen shell", AUTO_MODE_SETUP_FULLSCREEN_SHELL),
    (
        "fullscreen shell mode",
        AUTO_MODE_SETUP_FULLSCREEN_SHELL_MODE,
    ),
];

/// The title's first row: the whole title, or its first part where a narrow pane wraps it.
fn title_at(rows: &[String]) -> usize {
    rows.iter()
        .position(|r| crate::text::trim(r).starts_with("Teach auto mode about your"))
        .expect("the title row")
}

fn is_offer(rows: &[String]) -> bool {
    parse_prompt_v2(rows).is_some_and(|p| p.kind == PromptKind::AutoModeSetup)
}

fn edit(rows: &[String], from: &str, to: &str) -> Vec<String> {
    let mut out = rows.to_vec();
    let at = out
        .iter()
        .position(|r| crate::text::trim(r) == from)
        .unwrap_or_else(|| panic!("no row {from:?}"));
    let col = crate::phase::leading_spaces(&out[at]);
    out[at] = format!("{}{to}", " ".repeat(col));
    out
}

fn swap(rows: &[String], from: &str, row: &str) -> Vec<String> {
    let mut out = rows.to_vec();
    out[row_of(rows, from)] = row.to_string();
    out
}

fn row_of(rows: &[String], text: &str) -> usize {
    rows.iter()
        .position(|r| crate::text::trim(r) == text)
        .unwrap_or_else(|| panic!("no row {text:?}"))
}

#[test]
fn every_capture_is_read_whole_with_the_focus_on_yes() {
    for (name, text) in ALL {
        let rows = screen(text);
        let p = parse_prompt_v2(&rows).unwrap_or_else(|| panic!("{name}: the offer"));
        assert_eq!(p.kind, PromptKind::AutoModeSetup, "{name}");
        assert_eq!(p.kind.name(), "auto-mode-setup", "{name}");
        assert_eq!(p.title, "Teach auto mode about your environment?", "{name}");
        assert_eq!(
            p.description,
            "Auto mode works better when it knows your environment. Takes about a minute.",
            "{name}"
        );
        assert_eq!(p.select, Select::Digits, "{name}");
        assert_eq!(
            p.cancel.as_ref().map(|c| (c.key.as_str(), c.effect)),
            Some(("Esc", CancelEffect::Back)),
            "{name}: Esc puts the offer off, as Not now does"
        );
        let drawn: Vec<(Option<u8>, &str, Role, bool)> = p
            .options
            .iter()
            .map(|o| (o.n, o.label.as_str(), o.role, o.focused))
            .collect();
        assert_eq!(
            drawn,
            [
                (Some(1), "Yes", Role::Other, true),
                (Some(2), "Not now", Role::Deny, false),
                (Some(3), "Don't show again", Role::Other, false),
            ],
            "{name}"
        );
        assert!(p.options.iter().all(|o| !o.kept), "{name}");
        assert_eq!(rows[p.options[0].row].trim(), "❯ 1. Yes", "{name}");
        let later = p.auto_mode_later().expect("Not now");
        assert_eq!(
            (later.n, later.label.as_str()),
            (Some(2), "Not now"),
            "{name}"
        );
        assert_eq!(
            p.span,
            (
                title_at(&rows),
                row_of(&rows, "Enter to confirm · Esc to cancel")
            ),
            "{name}"
        );
        assert_eq!(
            crate::prompt::prompt_box_span(&rows),
            Some(p.span),
            "{name}"
        );
        assert_eq!(
            crate::prompt::prompt_box_first_row(&rows),
            Some(p.span.0),
            "{name}"
        );
        assert_eq!(
            crate::prompt::title_row(&p, &rows),
            Some(rows[title_at(&rows)].as_str()),
            "{name}"
        );
        let v1 = crate::prompt::parse_prompt(&rows).expect("v1");
        assert_eq!(
            (v1.kind, v1.options.len(), v1.options[1].clone()),
            (p.kind, 3, (2, "Not now".to_string())),
            "{name}"
        );
    }
}

#[test]
fn the_offer_reads_as_an_authoritative_prompt_in_both_renderers() {
    for (name, text) in ALL {
        let rows = screen(text);
        let r = crate::reader::read(None, &rows, None);
        assert_eq!(r.phase, Phase::Prompt, "{name}");
        assert!(r.phase_authoritative, "{name}");
        assert_eq!(
            r.prompt.map(|p| p.kind),
            Some(PromptKind::AutoModeSetup),
            "{name}"
        );
    }
}

#[test]
fn the_answers_are_needed_in_their_words_and_order() {
    let rows = screen(AUTO_MODE_SETUP);
    let p = parse_prompt_v2(&rows).expect("the offer");
    let mut other = p.clone();
    other.options[1].label = "Later".to_string();
    assert_eq!(other.auto_mode_later(), None, "an answer in other words");
    let mut kind = p.clone();
    kind.kind = PromptKind::Other;
    assert_eq!(kind.auto_mode_later(), None, "another kind");
    let mut fewer = p;
    fewer.options.pop();
    assert_eq!(fewer.auto_mode_later(), None, "two answers");
}

/// A look-alike: the same box with different words, in either renderer, is not the offer.
#[test]
fn every_condition_is_needed() {
    for (name, text) in [
        ("inline", AUTO_MODE_SETUP),
        ("fullscreen", AUTO_MODE_SETUP_FULLSCREEN),
    ] {
        let rows = screen(text);
        assert!(is_offer(&rows), "{name}: the screen the controls edit");
        let title = "Teach auto mode about your environment?";
        let body = "Auto mode works better when it knows your environment. Takes about a minute.";
        let footer = "Enter to confirm · Esc to cancel";
        let cases: Vec<(&str, Vec<String>)> = vec![
            (
                "another title",
                edit(&rows, title, "Teach auto mode about your project?"),
            ),
            (
                "the setup wizard's body under the same title",
                edit(
                    &rows,
                    body,
                    "Claude Code reads this project, your recent Claude sessions, and optionally \
                     your shell history and other repositories.",
                ),
            ),
            (
                "a body cut short",
                edit(
                    &rows,
                    body,
                    "Auto mode works better when it knows your environment.",
                ),
            ),
            (
                "a first answer in other words",
                edit(&rows, "❯ 1. Yes", "❯ 1. Yes, set it up"),
            ),
            (
                "a second answer in other words",
                edit(&rows, "2. Not now", "  2. Later"),
            ),
            (
                "a third answer in other words",
                edit(&rows, "3. Don't show again", "  3. Never"),
            ),
            (
                "answers out of order",
                edit(&rows, "2. Not now", "  3. Not now"),
            ),
            ("no focused answer", edit(&rows, "❯ 1. Yes", "  1. Yes")),
            (
                "two focused answers",
                edit(&rows, "2. Not now", "❯ 2. Not now"),
            ),
            (
                "a footer in other words",
                edit(&rows, footer, "Enter to select · Esc to cancel"),
            ),
            ("a row left of the title", swap(&rows, body, body)),
        ];
        for (what, edited) in cases {
            assert_eq!(edited.len(), rows.len(), "{name}: {what}");
            assert!(!is_offer(&edited), "{name}: {what}");
        }
        let mut fourth = rows.clone();
        fourth.insert(
            row_of(&rows, "3. Don't show again") + 1,
            "    4. Tell me more".to_string(),
        );
        assert!(!is_offer(&fourth), "{name}: a fourth answer");
        let mut two = rows.clone();
        two.remove(row_of(&rows, "3. Don't show again"));
        assert!(!is_offer(&two), "{name}: two answers");
        let mut ruleless = rows.clone();
        ruleless.remove(row_of(&rows, title) - 1);
        assert!(!is_offer(&ruleless), "{name}: no rule over the title");
    }
}

/// A narrow pane wraps the title as it wraps the body: each is read joined, in its own words, at
/// the box's column.
#[test]
fn a_wrapped_title_is_read_joined_and_only_in_its_words() {
    for (name, text) in [
        ("inline 40", AUTO_MODE_SETUP_40COL),
        ("fullscreen 40", AUTO_MODE_SETUP_FULLSCREEN_40COL),
    ] {
        let rows = screen(text);
        let first = title_at(&rows);
        assert_eq!(
            rows[first].trim(),
            "Teach auto mode about your",
            "{name}: the capture wraps the title"
        );
        assert!(is_offer(&rows), "{name}");
        assert!(
            !is_offer(&edit(&rows, "environment?", "project?")),
            "{name}: a wrapped title in other words"
        );
        let mut apart = rows.clone();
        apart.insert(first + 1, String::new());
        assert!(!is_offer(&apart), "{name}: the title's rows apart");
        let mut further = rows.clone();
        further[first + 1] = format!("  {}", rows[first + 1]);
        assert!(!is_offer(&further), "{name}: a title row further in");
        let body = rows
            .iter()
            .position(|r| r.trim().starts_with("your environment."))
            .expect("the body's second row");
        let mut body_further = rows.clone();
        body_further[body] = format!("  {}", rows[body]);
        assert!(!is_offer(&body_further), "{name}: a body row further in");
    }
}

/// Over a shell-mode composer (`!` typed during the turn) the offer sits right on that composer's
/// top rule, as it does on the `❯` one.
#[test]
fn the_fullscreen_offer_reads_over_a_shell_mode_composer() {
    let rows = screen(AUTO_MODE_SETUP_FULLSCREEN_SHELL_MODE);
    let caret = rows
        .iter()
        .rposition(|r| r.trim_end() == "!")
        .expect("the shell-mode caret");
    let footer = row_of(&rows, "Enter to confirm · Esc to cancel");
    assert_eq!(footer + 2, caret, "the footer right on the composer's rule");
    let r = crate::reader::read(None, &rows, None);
    assert_eq!(r.phase, Phase::Prompt);
    assert_eq!(r.prompt.map(|p| p.kind), Some(PromptKind::AutoModeSetup));
    let mut uncareted = rows.clone();
    uncareted[caret] = "!x".to_string();
    assert!(!is_offer(&uncareted), "a `!`-led row that is no caret");
    let mut apart = rows.clone();
    apart.insert(footer + 1, String::new());
    assert!(
        !is_offer(&apart),
        "a blank row between the footer and the shell-mode composer"
    );
}

#[test]
fn the_offer_must_sit_where_claude_code_draws_it() {
    let inline = screen(AUTO_MODE_SETUP);
    let mut under = inline.clone();
    under.push("  ⏵⏵ auto mode on (shift+tab to cycle)".to_string());
    assert!(!is_offer(&under), "a row under the inline footer");
    let full = screen(AUTO_MODE_SETUP_FULLSCREEN);
    let footer = row_of(&full, "Enter to confirm · Esc to cancel");
    let mut apart = full.clone();
    apart.insert(footer + 1, String::new());
    assert!(
        !is_offer(&apart),
        "a blank row between the footer and the composer"
    );
    let mut said = full.clone();
    said.insert(footer + 1, "✻ Cooked for 0s".to_string());
    assert!(
        !is_offer(&said),
        "a transcript row between the footer and the composer"
    );
}

/// The offer's words quoted in the transcript (a reply that shows it, a pasted capture) are not
/// the offer.
#[test]
fn a_quoted_offer_in_the_transcript_is_not_the_offer() {
    let full = screen(AUTO_MODE_SETUP_FULLSCREEN);
    let rule = row_of(&full, "Teach auto mode about your environment?") - 1;
    let footer = row_of(&full, "Enter to confirm · Esc to cancel");
    let quoted: Vec<String> = full[rule..=footer].to_vec();
    let mut rows: Vec<String> = full[..rule].to_vec();
    rows.extend(quoted.iter().cloned());
    rows.push(String::new());
    rows.push("✻ Cooked for 0s".to_string());
    rows.push(String::new());
    rows.extend(full[footer + 1..].iter().cloned());
    assert!(!is_offer(&rows), "the box's rows above a turn's end");
    let r = crate::reader::read(None, &rows, None);
    assert_ne!(r.prompt.map(|p| p.kind), Some(PromptKind::AutoModeSetup));
    let mut replied: Vec<String> = full[..rule].to_vec();
    replied.push(format!("⏺ {}", quoted[1].trim()));
    replied.extend(quoted[2..].iter().map(|r| format!("  {r}")));
    replied.extend(full[footer + 1..].iter().cloned());
    assert!(
        !is_offer(&replied),
        "the box's words in a reply right over the composer"
    );
    let mut draft = full.clone();
    let caret = full
        .iter()
        .rposition(|r| r.trim() == "❯")
        .expect("the caret");
    draft[caret] = "❯ Enter to confirm · Esc to cancel".to_string();
    draft.drain(rule..=footer);
    assert!(!is_offer(&draft), "the footer's words as a draft");
}

#[test]
fn the_fullscreen_offer_reads_over_a_running_shell() {
    let rows = screen(AUTO_MODE_SETUP_FULLSCREEN_SHELL);
    let r = crate::reader::read(None, &rows, None);
    assert_eq!(r.phase, Phase::Prompt);
    assert_eq!(r.prompt.map(|p| p.kind), Some(PromptKind::AutoModeSetup));
}
