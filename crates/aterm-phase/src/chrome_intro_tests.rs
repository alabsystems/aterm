// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `Claude in Chrome` intro reader tests (`crate::prompt::fixtures::CHROME_INTRO*`).
//!
//! Every screen is measured: the 2026-11-02 incident's worker at 124 columns ([`CHROME_INTRO`]),
//! and the real-Claude lab's captures of Claude Code 2.1.317 (crates/aterm-lab, 2026-11-03) in
//! the inline renderer down to 20 columns, in the fullscreen renderer, in the form drawn once the
//! extension is found, and taller than its pane.

use super::*;
use crate::phase::Phase;
use crate::prompt::fixtures::{
    CHROME_INTRO, CHROME_INTRO_EXTENSION_FOUND, CHROME_INTRO_FULLSCREEN, CHROME_INTRO_HEAD_OFF,
    CHROME_INTRO_INLINE, ink_wrap, screen,
};
use crate::prompt::parse_prompt_v2;

const TITLE: &str = "Claude in Chrome";
const FOOTER: &str = "Enter to confirm · Esc to cancel";
const BODY: &str = "Claude in Chrome works with the Chrome extension to let you control your \
                    browser directly from Claude Code. You can navigate websites, fill forms, \
                    capture screenshots, record GIFs, and debug with console logs and network \
                    requests.";
const REQUIRES: &str = "Requires the Chrome extension. Get started at https://claude.ai/chrome";
const PERMISSIONS: &str = "Site-level permissions are inherited from the Chrome extension. Manage \
                           permissions in the Chrome extension settings to control which sites \
                           Claude can browse, click, and type on.";
const PERMISSIONS_LINKED: &str = "Site-level permissions are inherited from the Chrome \
                                  extension. Manage permissions in the Chrome extension settings \
                                  to control which sites Claude can browse, click, and type on \
                                  (https://clau.de/chrome/permissions).";
const MORE: &str = "For more info, use /chrome or visit https://code.claude.com/docs/en/chrome";

const NOT_FOUND: [&str; 4] = [BODY, REQUIRES, PERMISSIONS, MORE];
const FOUND: [&str; 3] = [BODY, PERMISSIONS_LINKED, MORE];

/// Every whole screen of the intro: its name, its text, its width and whether it is the form drawn
/// once the extension is found.
fn all() -> Vec<(String, &'static str, usize, bool)> {
    let mut out = vec![(
        "inline 124 (the incident)".to_string(),
        CHROME_INTRO,
        124,
        false,
    )];
    for (width, text) in CHROME_INTRO_INLINE {
        out.push((format!("inline {width}"), text, width, false));
    }
    for (width, text) in CHROME_INTRO_FULLSCREEN {
        out.push((format!("fullscreen {width}"), text, width, false));
    }
    for (width, text) in CHROME_INTRO_EXTENSION_FOUND {
        out.push((format!("extension found {width}"), text, width, true));
    }
    out
}

/// One whole screen by its width.
fn inline(width: usize) -> Vec<String> {
    let (_, text) = CHROME_INTRO_INLINE
        .into_iter()
        .find(|&(w, _)| w == width)
        .expect("a capture at that width");
    screen(text)
}

fn is_intro(rows: &[String]) -> bool {
    parse_prompt_v2(rows).is_some_and(|p| p.kind == PromptKind::ChromeIntro)
}

/// The title's first row: the row right under a rule that opens the title.
fn title_at(rows: &[String]) -> usize {
    (1..rows.len())
        .find(|&i| crate::phase::is_rule(&rows[i - 1]) && rows[i].trim().starts_with("Claude in"))
        .expect("the title row")
}

/// The footer's first and last rows: one row, or the two to four a narrow pane wraps it to.
fn footer_at(rows: &[String]) -> (usize, usize) {
    let first = rows
        .iter()
        .rposition(|r| r.trim().starts_with("Enter to"))
        .expect("the footer");
    let mut joined = String::new();
    for (last, row) in rows.iter().enumerate().skip(first) {
        if !joined.is_empty() {
            joined.push(' ');
        }
        joined.push_str(row.trim());
        if joined == FOOTER {
            return (first, last);
        }
        assert!(FOOTER.starts_with(&joined), "the footer's rows: {joined:?}");
    }
    panic!("the footer's rows end the screen short of it: {joined:?}");
}

fn row_of(rows: &[String], text: &str) -> usize {
    rows.iter()
        .position(|r| r.trim() == text)
        .unwrap_or_else(|| panic!("no row {text:?}"))
}

/// `rows` with `from` replaced by `to` wherever it is drawn.
fn replaced(rows: &[String], from: &str, to: &str) -> Vec<String> {
    let out: Vec<String> = rows.iter().map(|r| r.replace(from, to)).collect();
    assert_ne!(out, rows, "{from:?} is on the screen");
    out
}

/// A composer `width` columns wide whose caret row is `caret`: its frame and the hint under it.
fn composer(width: usize, caret: &str) -> Vec<String> {
    vec![
        "─".repeat(width),
        caret.to_string(),
        "─".repeat(width),
        "  ? for shortcuts".to_string(),
    ]
}

/// The box Claude Code draws at `width` columns: the rule, then the title, each paragraph and the
/// footer at column 2, a blank row apart, each wrapped as Ink wraps it in a box `width - 4` cells
/// wide ([`ink_wrap`]).
fn drawn_box(width: usize, paragraphs: &[&str]) -> Vec<String> {
    let wrap = |text: &str| -> Vec<String> {
        ink_wrap(text, width - 4)
            .into_iter()
            .map(|r| format!("  {r}"))
            .collect()
    };
    let mut out = vec!["─".repeat(width)];
    out.extend(wrap(TITLE));
    for p in paragraphs {
        out.push(String::new());
        out.extend(wrap(p));
    }
    out.push(String::new());
    out.extend(wrap(FOOTER));
    out
}

#[test]
fn every_screen_is_read_whole_with_its_one_input_the_footers_enter() {
    for (name, text, _, found) in all() {
        let rows = screen(text);
        let p = parse_prompt_v2(&rows).unwrap_or_else(|| panic!("{name}: the intro"));
        assert_eq!(p.kind, PromptKind::ChromeIntro, "{name}");
        assert_eq!(p.kind.name(), "chrome-intro", "{name}");
        assert_eq!(
            (p.title.as_str(), p.command.as_str()),
            (TITLE, TITLE),
            "{name}"
        );
        let paragraphs: &[&str] = if found { &FOUND } else { &NOT_FOUND };
        assert_eq!(p.description, paragraphs.join(" "), "{name}");
        assert_eq!(p.select, Select::ArrowsEnter, "{name}");
        assert_eq!(
            p.cancel.as_ref().map(|c| (c.key.as_str(), c.effect)),
            Some(("Esc", CancelEffect::Back)),
            "{name}: Esc dismisses the intro, as Enter does"
        );
        let (footer, last) = footer_at(&rows);
        let drawn: Vec<(Option<u8>, &str, Role, bool, usize)> = p
            .options
            .iter()
            .map(|o| (o.n, o.label.as_str(), o.role, o.focused, o.row))
            .collect();
        assert_eq!(
            drawn,
            [(None, "Enter to confirm", Role::Other, true, footer)],
            "{name}"
        );
        assert!(p.options.iter().all(|o| !o.kept), "{name}");
        let confirm = p.chrome_intro_confirm().expect("the confirm");
        assert_eq!(confirm.label, "Enter to confirm", "{name}");
        assert_eq!(p.span, (title_at(&rows), last), "{name}");
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
            Some(rows[p.span.0].as_str()),
            "{name}"
        );
        let v1 = crate::prompt::parse_prompt(&rows).expect("v1");
        assert_eq!((v1.kind, v1.options.len()), (p.kind, 0), "{name}");
    }
}

#[test]
fn the_intro_reads_as_an_authoritative_prompt_in_both_renderers() {
    for (name, text, _, _) in all() {
        let mut rows = screen(text);
        for pad in [0, 29] {
            rows.extend(std::iter::repeat_n(String::new(), pad));
            let r = crate::reader::read(None, &rows, None);
            assert_eq!(r.phase, Phase::Prompt, "{name} +{pad}");
            assert!(r.phase_authoritative, "{name} +{pad}");
            assert_eq!(
                r.prompt.map(|p| p.kind),
                Some(PromptKind::ChromeIntro),
                "{name} +{pad}"
            );
            let claude = crate::reader::read(Some("claude"), &rows, None);
            assert_eq!(
                claude.prompt.map(|p| p.kind),
                Some(PromptKind::ChromeIntro),
                "{name} +{pad}: Claude Code's reader"
            );
        }
    }
}

/// Each capture is the box [`drawn_box`] draws at its width (the room Ink wraps the paragraphs in
/// is the pane less two columns each side), which holds the fixtures' port of Ink's wrap
/// ([`ink_wrap`]) to what Claude Code drew.
#[test]
fn every_capture_is_the_box_ink_draws() {
    for (name, text, width, found) in all() {
        let rows = screen(text);
        let paragraphs: &[&str] = if found { &FOUND } else { &NOT_FOUND };
        let want = drawn_box(width, paragraphs);
        let rule = title_at(&rows) - 1;
        assert_eq!(
            rows.get(rule..rule + want.len()),
            Some(want.as_slice()),
            "{name}"
        );
    }
}

#[test]
fn the_confirm_is_needed_whole() {
    let rows = screen(CHROME_INTRO);
    let p = parse_prompt_v2(&rows).expect("the intro");
    let mut other = p.clone();
    other.options[0].label = "Enter to select".to_string();
    assert_eq!(other.chrome_intro_confirm(), None, "another label");
    let mut unfocused = p.clone();
    unfocused.options[0].focused = false;
    assert_eq!(unfocused.chrome_intro_confirm(), None, "no focus");
    let mut numbered = p.clone();
    numbered.options[0].n = Some(1);
    assert_eq!(numbered.chrome_intro_confirm(), None, "a numbered option");
    let mut kind = p.clone();
    kind.kind = PromptKind::Other;
    assert_eq!(kind.chrome_intro_confirm(), None, "another kind");
    let mut two = p;
    two.options.push(two.options[0].clone());
    assert_eq!(two.chrome_intro_confirm(), None, "two options");
}

/// NEGATIVE CONTROLS: a look-alike (one word changed anywhere, a link or the command changed, a
/// paragraph missing or added, an answer list added, the title alone, no rule over the title,
/// rows further in or apart), in either renderer, is not the intro.
#[test]
fn every_condition_is_needed() {
    for (name, text) in [
        ("inline", CHROME_INTRO),
        ("fullscreen", CHROME_INTRO_FULLSCREEN[0].1),
    ] {
        let rows = screen(text);
        assert!(is_intro(&rows), "{name}: the screen the controls edit");
        let title = title_at(&rows);
        let (footer, _) = footer_at(&rows);
        let mut cases: Vec<(&str, Vec<String>)> = vec![
            (
                "one word of the body changed",
                replaced(&rows, "control your", "steer your"),
            ),
            (
                "one word of the permissions changed",
                replaced(&rows, "click, and type on.", "click, and scroll on."),
            ),
            (
                "another store link",
                replaced(&rows, "https://claude.ai/chrome", "https://claude.ai/edge"),
            ),
            (
                "another command",
                replaced(&rows, "use /chrome or", "use /browser or"),
            ),
            (
                "another docs link",
                replaced(&rows, "docs/en/chrome", "docs/en/browser"),
            ),
            (
                "a footer in other words",
                replaced(&rows, FOOTER, "Enter to select · Esc to cancel"),
            ),
        ];
        let mut retitled = rows.clone();
        retitled[title] = "  Claude in Firefox".to_string();
        cases.push(("another title", retitled));
        let requires = row_of(&rows, REQUIRES);
        let mut missing = rows.clone();
        missing.drain(requires - 1..=requires);
        cases.push(("the Requires paragraph missing", missing));
        let mut added = rows.clone();
        added.splice(
            footer..footer,
            ["  Chrome works best signed in.", ""].map(str::to_string),
        );
        cases.push(("a paragraph added", added));
        let mut answers = rows.clone();
        answers.splice(
            footer..footer,
            ["  ❯ 1. Yes", "    2. No", ""].map(str::to_string),
        );
        cases.push(("a numbered answer list added", answers));
        let mut alone: Vec<String> = rows[..=title].to_vec();
        alone.push(String::new());
        alone.extend(rows[footer..].iter().cloned());
        cases.push(("the title alone", alone));
        let mut ruleless = rows.clone();
        ruleless.remove(title - 1);
        cases.push(("no rule over the title", ruleless));
        let mut further = rows.clone();
        further[requires] = format!(" {}", rows[requires]);
        cases.push(("a paragraph further in", further));
        let mut footer_in = rows.clone();
        footer_in[footer] = format!(" {}", rows[footer]);
        cases.push(("the footer further in", footer_in));
        let mut apart = rows.clone();
        apart.insert(requires, String::new());
        cases.push(("two blank rows between paragraphs", apart));
        let mut tight = rows.clone();
        tight.remove(footer - 1);
        cases.push(("no blank row over the footer", tight));
        let mut untitled = rows.clone();
        untitled.insert(title, String::new());
        cases.push(("a blank row between the rule and the title", untitled));
        for (what, edited) in cases {
            assert!(!is_intro(&edited), "{name}: {what}");
            let r = crate::reader::read(None, &edited, None);
            assert_ne!(
                r.prompt.map(|p| p.kind),
                Some(PromptKind::ChromeIntro),
                "{name}: {what}"
            );
        }
        // The extension-found form's permissions closing under the Requires paragraph is
        // neither form.
        let mut mixed = rows.clone();
        let perm = mixed
            .iter()
            .rposition(|r| r.trim().ends_with("click, and type on."))
            .expect("the permissions' last row");
        mixed[perm] = mixed[perm].replace(
            "click, and type on.",
            "click, and type on (https://clau.de/chrome/permissions).",
        );
        assert!(!is_intro(&mixed), "{name}: the two forms mixed");
    }
}

/// The footer is the last drawn row: Claude Code draws the intro before its REPL, with nothing
/// under it, in both renderers. A row under the footer, a footer drawn above the box's last
/// paragraph, the box under a composer (the `❯` composer's or a shell-mode `!` one's frame
/// closing over its title) and the box right on a composer's top rule, layouts the intro is never
/// drawn in, are not the intro.
#[test]
fn the_intro_must_sit_where_claude_code_draws_it() {
    let rows = screen(CHROME_INTRO);
    let mut under = rows.clone();
    under.push("  ? for shortcuts".to_string());
    assert!(!is_intro(&under), "a row under the footer");
    let (footer, _) = footer_at(&rows);
    let mut moved = rows.clone();
    let more = row_of(&rows, MORE);
    moved.swap(footer, more);
    assert!(!is_intro(&moved), "the footer above the last paragraph");
    let rule = title_at(&rows) - 1;
    for caret in ["❯", "!"] {
        let mut panel: Vec<String> = rows[..rule].to_vec();
        panel.extend(["─".repeat(124), caret.to_string()]);
        panel.extend(rows[rule..].iter().cloned());
        assert!(
            !is_intro(&panel),
            "the box under a `{caret}` composer, the frame's bottom rule over its title"
        );
        let mut over = rows.clone();
        over.extend(composer(124, caret));
        assert!(
            !is_intro(&over),
            "the box on a `{caret}` composer's top rule"
        );
        let r = crate::reader::read(None, &over, None);
        assert_ne!(
            r.prompt.map(|p| p.kind),
            Some(PromptKind::ChromeIntro),
            "the box on a `{caret}` composer's top rule"
        );
    }
}

/// The intro's words quoted in the transcript (a reply that shows it, a pasted capture) above a
/// live composer are not the intro, nor are the footer's words typed as a draft.
#[test]
fn a_quoted_intro_in_the_transcript_is_not_the_intro() {
    let rows = screen(CHROME_INTRO);
    let rule = title_at(&rows) - 1;
    let (_, last) = footer_at(&rows);
    let quoted: Vec<String> = rows[rule..=last].to_vec();
    let mut turn: Vec<String> = rows[..rule].to_vec();
    turn.extend(quoted.iter().cloned());
    turn.extend(["", "✻ Cooked for 0s", ""].map(str::to_string));
    turn.extend(composer(124, "❯"));
    assert!(!is_intro(&turn), "the box's rows above a turn's end");
    let r = crate::reader::read(None, &turn, None);
    assert_ne!(r.prompt.map(|p| p.kind), Some(PromptKind::ChromeIntro));
    let mut replied: Vec<String> = rows[..rule].to_vec();
    replied.push(format!("⏺ {}", quoted[1].trim()));
    replied.extend(quoted[2..].iter().map(|r| format!("  {r}")));
    replied.extend(composer(124, "❯"));
    assert!(
        !is_intro(&replied),
        "the box's words in a reply right over the composer"
    );
    let mut draft: Vec<String> = rows[..rule].to_vec();
    draft.extend(composer(124, &format!("❯ {FOOTER}")));
    assert!(!is_intro(&draft), "the footer's words as a draft");
    let mut over = rows.clone();
    over.extend(["", "⏺ That was the intro.", ""].map(str::to_string));
    over.extend(composer(124, "❯"));
    assert!(!is_intro(&over), "the box above a reply and a composer");
}

/// THE PANE TOO SHORT: an intro taller than its pane (20 columns in a 40-row pane, 16 in a 59-row
/// one) has its rule, title and first rows scrolled off. It is read cut, `head_off_screen`, so a
/// reader reads the rows above it again and a supervisor joins its head from the history; its
/// confirm is never pressed off a cut read.
#[test]
fn an_intro_taller_than_its_pane_is_read_cut_and_never_confirmed() {
    for (width, text) in CHROME_INTRO_HEAD_OFF {
        let rows = screen(text);
        assert!(
            rows.iter().any(|r| r.trim().ends_with("cancel")),
            "{width}: PRECONDITION the footer is on the screen"
        );
        assert!(
            !rows.iter().any(|r| r.trim() == TITLE),
            "{width}: PRECONDITION the title scrolled off"
        );
        let p = parse_prompt_v2(&rows).unwrap_or_else(|| panic!("{width}: the cut intro"));
        assert_eq!(p.kind, PromptKind::ChromeIntro, "{width}");
        assert!(p.head_off_screen, "{width}");
        assert_eq!(p.chrome_intro_confirm(), None, "{width}: never pressed cut");
        assert_eq!(p.description, NOT_FOUND.join(" "), "{width}");
        let r = crate::reader::read(Some("claude"), &rows, None);
        assert!(
            r.prompt
                .is_some_and(|p| p.kind == PromptKind::ChromeIntro && p.head_off_screen),
            "{width}"
        );
    }
}

/// REVIEW FINDING (2026-11-03): at 20 to 24 columns the intro is taller than the 40-row live zone
/// the GUI and the supervisor classify, so in a pane tall enough to show it whole it was read
/// nowhere: the zone cut its rule and title off. Every whole capture cut at every row a block
/// starts below reads cut, its footer's Enter on the footer's row of the cut and its span ending
/// on the cut's last drawn row, as a head joined from the history must match; the 20-column
/// capture's live zone in a 60-row pane reads cut, and its whole screen whole.
#[test]
fn every_capture_cut_at_any_row_reads_cut() {
    for (name, text, _, found) in all() {
        let rows = screen(text);
        let paragraphs: &[&str] = if found { &FOUND } else { &NOT_FOUND };
        let title = title_at(&rows);
        let (footer, last) = footer_at(&rows);
        let mut cuts = 0;
        for k in title..footer - 1 {
            let cut = &rows[k..];
            let p = parse_prompt_v2(cut).unwrap_or_else(|| panic!("{name} cut at {k}"));
            assert_eq!(p.kind, PromptKind::ChromeIntro, "{name} cut at {k}");
            assert!(p.head_off_screen, "{name} cut at {k}");
            assert_eq!(p.chrome_intro_confirm(), None, "{name} cut at {k}");
            assert_eq!(p.span.1, last - k, "{name} cut at {k}");
            assert_eq!(
                p.options.iter().map(|o| o.row).collect::<Vec<_>>(),
                [footer - k],
                "{name} cut at {k}"
            );
            assert!(
                [NOT_FOUND.join(" "), FOUND.join(" ")].contains(&p.description),
                "{name} cut at {k}: {}",
                p.description
            );
            if k <= title_at(&rows) + 2 {
                assert_eq!(p.description, paragraphs.join(" "), "{name} cut at {k}");
            }
            cuts += 1;
        }
        assert!(cuts >= 8, "{name}: {cuts} cuts");
    }
    let mut tall = inline(20);
    let foot = tall.len();
    tall.resize(60, String::new());
    let zone = &tall[crate::reader::live_zone_start(&tall, 40)..];
    assert!(
        zone.len() < foot,
        "PRECONDITION: the live zone cuts the intro"
    );
    let cut = parse_prompt_v2(zone).expect("the zone's cut intro");
    assert!(cut.head_off_screen && cut.chrome_intro_confirm().is_none());
    let whole = parse_prompt_v2(&tall).expect("the whole screen's intro");
    assert!(!whole.head_off_screen && whole.chrome_intro_confirm().is_some());
}

/// NEGATIVE CONTROLS for the cut read: a cut in other words, further in, with a first row that
/// starts inside a word that fits (no wrap Ink draws), its blocks apart, a ruleless title under a blank first row
/// (no cut: the rule stands right over the title), a rule above it, or its footer's rows in other
/// words, is no intro.
#[test]
fn a_cut_look_alike_is_not_the_intro() {
    let rows = inline(40);
    let k = row_of(&rows, "Requires the Chrome extension. Get");
    let cut: Vec<String> = rows[k..].to_vec();
    assert!(
        parse_prompt_v2(&cut).is_some_and(|p| p.head_off_screen),
        "CONTROL: the cut reads"
    );
    let mut cases: Vec<(&str, Vec<String>)> = vec![
        ("one word changed", replaced(&cut, "Manage", "Change")),
        (
            "another docs link",
            replaced(&cut, "docs/en/chro", "docs/en/brow"),
        ),
        (
            "a footer in other words",
            replaced(&cut, FOOTER, "Enter to select · Esc to cancel"),
        ),
    ];
    let mut further = cut.clone();
    further[0] = format!(" {}", cut[0]);
    cases.push(("its first row further in", further));
    let mut broken = cut.clone();
    broken[0] = "  rome extension. Get".to_string();
    cases.push(("a first row inside a word that fits", broken));
    let mut apart = cut.clone();
    apart.insert(1, String::new());
    cases.push(("a paragraph's rows apart", apart));
    let mut ruled = cut.clone();
    ruled.insert(0, "─".repeat(40));
    cases.push(("a rule over the cut", ruled));
    let mut untitled = vec![String::new()];
    untitled.extend(rows[title_at(&rows)..].iter().cloned());
    cases.push(("a ruleless title under a blank row", untitled));
    let mut transcript = vec!["⏺ Here is the intro:".to_string()];
    transcript.extend(cut.iter().cloned());
    cases.push(("a transcript row over it", transcript));
    for (what, edited) in cases {
        assert!(!is_intro(&edited), "{what}");
    }
}

/// A narrow pane wraps the paragraphs and the footer: each is read joined, in its own words, its
/// rows together at the box's column.
#[test]
fn a_narrow_panes_wraps_are_read_joined_and_only_in_their_words() {
    let rows = inline(20);
    assert!(is_intro(&rows));
    let title = title_at(&rows);
    assert_eq!(rows[title].trim(), TITLE, "the title fills the 16-cell box");
    let (first, last) = footer_at(&rows);
    assert_eq!(
        (rows[first].trim(), rows[last].trim()),
        ("Enter to confirm", "· Esc to cancel"),
        "the footer on two rows"
    );
    let mut cases: Vec<(String, Vec<String>)> = Vec::new();
    let mut footer = rows.clone();
    footer[last] = "  · Esc to go back".to_string();
    cases.push(("the footer's rest in other words".to_string(), footer));
    let mut footer_in = rows.clone();
    footer_in[last] = format!(" {}", rows[last]);
    cases.push(("the footer's rest further in".to_string(), footer_in));
    let body = row_of(&rows, "works with the");
    let mut body_further = rows.clone();
    body_further[body] = format!("  {}", rows[body]);
    cases.push(("a body row further in".to_string(), body_further));
    let mut body_apart = rows.clone();
    body_apart.insert(body, String::new());
    cases.push(("a paragraph's rows apart".to_string(), body_apart));
    let mut body_said = rows.clone();
    body_said[body] = "  works with this".to_string();
    cases.push(("a wrapped row in other words".to_string(), body_said));
    for (what, edited) in cases {
        assert!(!is_intro(&edited), "{what}");
    }
}

/// A link wider than a narrow pane's box is broken at the box's edge and read joined with no
/// space; a break short of the edge, or of a word that fits, is no wrap Ink draws.
#[test]
fn a_link_is_read_across_the_rows_the_box_breaks_it_onto() {
    let rows = inline(40);
    assert!(is_intro(&rows));
    let head = row_of(&rows, "https://code.claude.com/docs/en/chro");
    assert_eq!(rows[head + 1].trim(), "me", "the link broken at the edge");
    let mut short = rows.clone();
    short[head] = "  https://code.claude.com/docs/en/chr".to_string();
    short[head + 1] = "  ome".to_string();
    assert!(!is_intro(&short), "the link broken short of the edge");
    let mut spaced = rows.clone();
    spaced[head + 1] = "  me.".to_string();
    assert!(!is_intro(&spaced), "the link's rest in other words");
    let narrow = inline(20);
    let store = row_of(&narrow, "https://claude.a");
    assert_eq!(
        narrow[store + 1].trim(),
        "i/chrome",
        "the store link broken at a 16-cell box's edge"
    );

    // The reader's wrap, against the fixtures' port of Ink's (`ink_wrap`).
    let row = |text: &str| format!("  {text}");
    let run = |texts: &[&str]| -> Vec<String> { texts.iter().map(|t| row(t)).collect() };
    assert!(drawn_as(&run(&["abc", "def"]), "abc def", 5), "a word wrap");
    assert_eq!(ink_wrap("abcdefg", 5), ["abcde", "fg"]);
    assert!(
        drawn_as(&run(&["abcde", "fg"]), "abcdefg", 5),
        "a hard break"
    );
    assert!(
        !drawn_as(&run(&["abcd", "efg"]), "abcdefg", 5),
        "a break short of the edge"
    );
    assert!(
        !drawn_as(&run(&["xy abc", "de"]), "xy abcde", 6),
        "a word that fits, broken"
    );
    assert_eq!(ink_wrap("abcde fghijkl", 5), ["abcde", "fghi", "jkl"]);
    assert!(
        drawn_as(&run(&["abcde", "fghi", "jkl"]), "abcde fghijkl", 5),
        "a hard break after an exactly full row, its head's cell unused"
    );
    assert!(!drawn_as(&run(&["abc", ""]), "abc", 5), "an empty row");
    assert!(
        !drawn_as(&run(&["abc"]), "abc def", 5),
        "the text cut short"
    );
}
