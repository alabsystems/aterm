// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Auto-mode environment offer reader tests (`crate::prompt::fixtures::AUTO_MODE_SETUP*`).

use super::*;
use crate::phase::Phase;
use crate::prompt::fixtures::{
    AUTO_MODE_SETUP, AUTO_MODE_SETUP_16COL, AUTO_MODE_SETUP_17COL, AUTO_MODE_SETUP_19COL,
    AUTO_MODE_SETUP_20COL, AUTO_MODE_SETUP_22COL, AUTO_MODE_SETUP_23COL, AUTO_MODE_SETUP_26COL,
    AUTO_MODE_SETUP_34COL, AUTO_MODE_SETUP_40COL, AUTO_MODE_SETUP_60COL,
    AUTO_MODE_SETUP_FULLSCREEN, AUTO_MODE_SETUP_FULLSCREEN_16COL, AUTO_MODE_SETUP_FULLSCREEN_17COL,
    AUTO_MODE_SETUP_FULLSCREEN_19COL, AUTO_MODE_SETUP_FULLSCREEN_20COL,
    AUTO_MODE_SETUP_FULLSCREEN_22COL, AUTO_MODE_SETUP_FULLSCREEN_26COL,
    AUTO_MODE_SETUP_FULLSCREEN_34COL, AUTO_MODE_SETUP_FULLSCREEN_40COL,
    AUTO_MODE_SETUP_FULLSCREEN_60COL, AUTO_MODE_SETUP_FULLSCREEN_SHELL,
    AUTO_MODE_SETUP_FULLSCREEN_SHELL_MODE, screen,
};
use crate::prompt::parse_prompt_v2;

const ALL: [(&str, &str); 23] = [
    ("inline", AUTO_MODE_SETUP),
    ("inline 60", AUTO_MODE_SETUP_60COL),
    ("inline 40", AUTO_MODE_SETUP_40COL),
    ("inline 34", AUTO_MODE_SETUP_34COL),
    ("inline 26", AUTO_MODE_SETUP_26COL),
    ("inline 23", AUTO_MODE_SETUP_23COL),
    ("inline 22", AUTO_MODE_SETUP_22COL),
    ("inline 20", AUTO_MODE_SETUP_20COL),
    ("inline 19", AUTO_MODE_SETUP_19COL),
    ("inline 17", AUTO_MODE_SETUP_17COL),
    ("inline 16", AUTO_MODE_SETUP_16COL),
    ("fullscreen", AUTO_MODE_SETUP_FULLSCREEN),
    ("fullscreen 60", AUTO_MODE_SETUP_FULLSCREEN_60COL),
    ("fullscreen 40", AUTO_MODE_SETUP_FULLSCREEN_40COL),
    ("fullscreen 34", AUTO_MODE_SETUP_FULLSCREEN_34COL),
    ("fullscreen 26", AUTO_MODE_SETUP_FULLSCREEN_26COL),
    ("fullscreen 22", AUTO_MODE_SETUP_FULLSCREEN_22COL),
    ("fullscreen 20", AUTO_MODE_SETUP_FULLSCREEN_20COL),
    ("fullscreen 19", AUTO_MODE_SETUP_FULLSCREEN_19COL),
    ("fullscreen 17", AUTO_MODE_SETUP_FULLSCREEN_17COL),
    ("fullscreen 16", AUTO_MODE_SETUP_FULLSCREEN_16COL),
    ("fullscreen shell", AUTO_MODE_SETUP_FULLSCREEN_SHELL),
    (
        "fullscreen shell mode",
        AUTO_MODE_SETUP_FULLSCREEN_SHELL_MODE,
    ),
];

/// The captures where the footer wraps: onto two rows from 34 columns down to 20, three at 19 and
/// 17, four at 16.
const WRAPPED_FOOTER: [(&str, &str); 15] = [
    ("inline 34", AUTO_MODE_SETUP_34COL),
    ("inline 26", AUTO_MODE_SETUP_26COL),
    ("inline 23", AUTO_MODE_SETUP_23COL),
    ("inline 22", AUTO_MODE_SETUP_22COL),
    ("inline 20", AUTO_MODE_SETUP_20COL),
    ("inline 19", AUTO_MODE_SETUP_19COL),
    ("inline 17", AUTO_MODE_SETUP_17COL),
    ("inline 16", AUTO_MODE_SETUP_16COL),
    ("fullscreen 34", AUTO_MODE_SETUP_FULLSCREEN_34COL),
    ("fullscreen 26", AUTO_MODE_SETUP_FULLSCREEN_26COL),
    ("fullscreen 22", AUTO_MODE_SETUP_FULLSCREEN_22COL),
    ("fullscreen 20", AUTO_MODE_SETUP_FULLSCREEN_20COL),
    ("fullscreen 19", AUTO_MODE_SETUP_FULLSCREEN_19COL),
    ("fullscreen 17", AUTO_MODE_SETUP_FULLSCREEN_17COL),
    ("fullscreen 16", AUTO_MODE_SETUP_FULLSCREEN_16COL),
];

/// The captures where the third answer wraps: onto two rows from 22 columns down to 17, three at
/// 16.
const WRAPPED_ANSWER: [(&str, &str); 10] = [
    ("inline 22", AUTO_MODE_SETUP_22COL),
    ("inline 20", AUTO_MODE_SETUP_20COL),
    ("inline 19", AUTO_MODE_SETUP_19COL),
    ("inline 17", AUTO_MODE_SETUP_17COL),
    ("inline 16", AUTO_MODE_SETUP_16COL),
    ("fullscreen 22", AUTO_MODE_SETUP_FULLSCREEN_22COL),
    ("fullscreen 20", AUTO_MODE_SETUP_FULLSCREEN_20COL),
    ("fullscreen 19", AUTO_MODE_SETUP_FULLSCREEN_19COL),
    ("fullscreen 17", AUTO_MODE_SETUP_FULLSCREEN_17COL),
    ("fullscreen 16", AUTO_MODE_SETUP_FULLSCREEN_16COL),
];

const FOOTER: &str = "Enter to confirm · Esc to cancel";

/// The title's first row: the whole title, or its first part where a narrow pane wraps it.
fn title_at(rows: &[String]) -> usize {
    rows.iter()
        .position(|r| crate::text::trim(r).starts_with("Teach auto"))
        .expect("the title row")
}

/// The footer's first and last rows: one row, or the two to four a narrow pane wraps it to.
fn footer_at(rows: &[String]) -> (usize, usize) {
    let first = rows
        .iter()
        .rposition(|r| crate::text::trim(r).starts_with("Enter to"))
        .expect("the footer");
    let mut joined = String::new();
    for (last, row) in rows.iter().enumerate().skip(first) {
        if !joined.is_empty() {
            joined.push(' ');
        }
        joined.push_str(crate::text::trim(row));
        if joined == FOOTER {
            return (first, last);
        }
        assert!(FOOTER.starts_with(&joined), "the footer's rows: {joined:?}");
    }
    panic!("the footer's rows end the screen short of it: {joined:?}");
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
        assert_eq!(p.span, (title_at(&rows), footer_at(&rows).1), "{name}");
        assert_eq!(p.footer.as_deref(), Some(FOOTER), "{name}");
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

/// A narrow pane wraps the footer onto two to four rows: it is read joined, in its own words, its
/// rows together at one column and where Claude Code draws them, the box's span ending on the
/// last. A wrapped footer that is not the offer's is not the offer.
#[test]
fn a_wrapped_footer_is_read_joined_and_only_in_its_words() {
    for (name, text) in WRAPPED_FOOTER {
        let rows = screen(text);
        let (first, last) = footer_at(&rows);
        assert!(first < last, "{name}: the capture wraps the footer");
        let p = parse_prompt_v2(&rows).unwrap_or_else(|| panic!("{name}: the offer"));
        assert_eq!(p.kind, PromptKind::AutoModeSetup, "{name}");
        assert_eq!(
            p.span.1, last,
            "{name}: the box ends on the footer's last row"
        );
        let fullscreen = rows[last + 1..].iter().any(|r| !r.trim().is_empty());
        let at = |row: usize, to: String| -> Vec<String> {
            let mut out = rows.clone();
            out[row] = to;
            out
        };
        let reworded = |from: &str, to: &str| -> Vec<String> {
            let row = (first..=last)
                .find(|&k| rows[k].contains(from))
                .unwrap_or_else(|| panic!("{name}: {from:?} in the footer"));
            at(row, rows[row].replace(from, to))
        };
        let without = |row: usize| -> Vec<String> {
            let mut out = rows.clone();
            out.remove(row);
            out
        };
        let mut cases: Vec<(String, Vec<String>)> = vec![
            (
                "another box's footer, wrapped".to_string(),
                reworded("confirm", "select"),
            ),
            (
                "the footer's rest in other words".to_string(),
                reworded("cancel", "go back"),
            ),
            ("the footer short its last row".to_string(), without(last)),
            ("the footer short its first row".to_string(), without(first)),
        ];
        for k in first + 1..=last {
            let mut apart = rows.clone();
            apart.insert(k, String::new());
            cases.push((format!("the footer's rows apart over row {k}"), apart));
            cases.push((
                format!("the footer's row {k} further in"),
                at(k, format!("  {}", rows[k])),
            ));
            cases.push((
                format!("the footer's row {k} at the pane's edge"),
                at(k, rows[k].trim_start().to_string()),
            ));
        }
        if fullscreen {
            let mut said = rows.clone();
            said.insert(last + 1, "✻ Cooked for 0s".to_string());
            cases.push((
                "a transcript row between the footer and the composer".to_string(),
                said,
            ));
            let mut gap = rows.clone();
            gap.insert(last + 1, String::new());
            cases.push((
                "a blank row between the footer and the composer".to_string(),
                gap,
            ));
        } else {
            let mut under = rows.clone();
            under.push("  ⏵⏵ auto mode on".to_string());
            cases.push(("a row under the footer".to_string(), under));
        }
        for (what, edited) in cases {
            assert!(!is_offer(&edited), "{name}: {what}");
            let r = crate::reader::read(None, &edited, None);
            assert_ne!(
                r.prompt.map(|p| p.kind),
                Some(PromptKind::AutoModeSetup),
                "{name}: {what}"
            );
        }
    }
}

/// The footer drawn on more rows than the pane wraps it to, in a pane wide enough to hold it on
/// one or in a narrow one, is no wrap Claude Code draws, and the box over it is not the offer;
/// nor is a narrow pane's box quoted in the transcript, above a turn's end.
#[test]
fn a_footer_drawn_on_rows_that_are_no_wrap_is_not_the_offer() {
    let split = |rows: &[String], into: &[&str]| -> Vec<String> {
        let (first, last) = footer_at(rows);
        let mut out = rows.to_vec();
        out.splice(first..=last, into.iter().map(|r| (*r).to_string()));
        out
    };
    for (name, text) in [
        ("inline", AUTO_MODE_SETUP),
        ("inline 40", AUTO_MODE_SETUP_40COL),
        ("fullscreen", AUTO_MODE_SETUP_FULLSCREEN),
        ("fullscreen 40", AUTO_MODE_SETUP_FULLSCREEN_40COL),
    ] {
        let rows = screen(text);
        assert!(is_offer(&rows), "{name}: the screen the controls edit");
        for into in [
            &["  Enter to confirm · Esc to", "  cancel"][..],
            &["  Enter to confirm", "  · Esc to", "  cancel"],
            &["  Enter to", "  confirm ·", "  Esc to", "  cancel"],
        ] {
            assert!(
                !is_offer(&split(&rows, into)),
                "{name}: the footer split onto {} rows where it fits",
                into.len()
            );
        }
    }
    for (name, text) in [
        ("inline 26", AUTO_MODE_SETUP_26COL),
        ("inline 19", AUTO_MODE_SETUP_19COL),
        ("fullscreen 26", AUTO_MODE_SETUP_FULLSCREEN_26COL),
        ("fullscreen 19", AUTO_MODE_SETUP_FULLSCREEN_19COL),
    ] {
        let rows = screen(text);
        assert!(is_offer(&rows), "{name}: the screen the control edits");
        let into = ["  Enter to", "  confirm ·", "  Esc to", "  cancel"];
        assert!(
            !is_offer(&split(&rows, &into)),
            "{name}: the footer split onto more rows than the pane wraps it to"
        );
    }
    let full = screen(AUTO_MODE_SETUP_FULLSCREEN_34COL);
    let rule = title_at(&full) - 1;
    let (_, last) = footer_at(&full);
    let mut quoted: Vec<String> = full[..rule].to_vec();
    quoted.extend(full[rule..=last].iter().cloned());
    quoted.push(String::new());
    quoted.push("✻ Cooked for 0s".to_string());
    quoted.push(String::new());
    quoted.extend(full[last + 1..].iter().cloned());
    assert!(
        !is_offer(&quoted),
        "the narrow box's rows above a turn's end"
    );
}

/// A pane 22 columns wide or narrower wraps the third answer's label to its column, onto two rows
/// or (at 16) three: it is read joined, and only in its words, its rows together at that column.
#[test]
fn a_wrapped_answer_is_read_joined_and_only_in_its_words() {
    for (name, text) in WRAPPED_ANSWER {
        let rows = screen(text);
        let third = rows
            .iter()
            .position(|r| crate::text::trim(r).starts_with("3. Don't"))
            .unwrap_or_else(|| panic!("{name}: the third answer"));
        let end = (third + 1..rows.len())
            .find(|&k| crate::text::trim(&rows[k]).is_empty())
            .unwrap_or_else(|| panic!("{name}: the blank row under the answers"));
        let rest = third + 1..end;
        assert!(!rest.is_empty(), "{name}: the capture wraps it");
        assert!(
            rest.clone()
                .all(|k| crate::phase::leading_spaces(&rows[k]) == 7),
            "{name}: at the label's column"
        );
        let p = parse_prompt_v2(&rows).unwrap_or_else(|| panic!("{name}: the offer"));
        assert_eq!(
            (p.options[2].label.as_str(), p.options[2].row),
            ("Don't show again", third),
            "{name}"
        );
        let at = |row: usize, to: String| -> Vec<String> {
            let mut out = rows.clone();
            out[row] = to;
            out
        };
        let mut cases: Vec<(String, Vec<String>)> = vec![(
            "its rest in other words".to_string(),
            at(end - 1, format!("{} later", rows[end - 1])),
        )];
        for k in rest {
            let drawn = crate::text::trim(&rows[k]);
            cases.push((
                format!("its row {k} further in"),
                at(k, format!("        {drawn}")),
            ));
            cases.push((
                format!("its row {k} at the digit's column"),
                at(k, format!("    {drawn}")),
            ));
            let mut apart = rows.clone();
            apart.insert(k, String::new());
            cases.push((format!("its rows apart over row {k}"), apart));
        }
        for (what, edited) in cases {
            assert!(!is_offer(&edited), "{name}: {what}");
        }
    }
    let whole = screen(AUTO_MODE_SETUP_23COL);
    assert_eq!(
        whole[row_of(&whole, "3. Don't show again")]
            .trim_end()
            .len(),
        23,
        "at 23 columns the third answer runs whole to the pane's edge"
    );
    let rows = screen(AUTO_MODE_SETUP);
    let third = row_of(&rows, "3. Don't show again");
    let mut split = rows.clone();
    split[third] = "    3. Don't show".to_string();
    split.insert(third + 1, "       again".to_string());
    assert!(!is_offer(&split), "the third answer split where it fits");
    let rows = screen(AUTO_MODE_SETUP_19COL);
    let third = row_of(&rows, "3. Don't show");
    let mut more = rows.clone();
    more[third] = "    3. Don't".to_string();
    more.insert(third + 1, "       show".to_string());
    assert!(
        !is_offer(&more),
        "the third answer split onto more rows than the pane wraps it to"
    );
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
