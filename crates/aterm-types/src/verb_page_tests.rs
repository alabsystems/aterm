// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control_verbs::{CATALOG_TEXT_COLUMN, TURN_OPTIONS, VERBS, VerbSpec, spec};

/// The verbs whose entry is a page.
fn paged() -> impl Iterator<Item = (&'static VerbSpec, &'static Page)> {
    VERBS.iter().filter_map(|s| s.page.map(|p| (s, p)))
}

/// The words of `lines`, a piece a long word broke after its `|` joined back to the rest.
fn words(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut glue = false;
    for line in lines {
        for (i, word) in line.split_whitespace().enumerate() {
            match out.last_mut() {
                Some(last) if glue && i == 0 => last.push_str(word),
                _ => out.push(word.to_string()),
            }
        }
        glue = out.last().is_some_and(|w| w.ends_with('|'));
    }
    out
}

/// Whether `needle` stands in `hay` as one run.
fn holds_run(hay: &[String], needle: &[&str]) -> bool {
    needle.is_empty()
        || hay
            .windows(needle.len())
            .any(|w| w.iter().zip(needle).all(|(a, b)| a == b))
}

/// Every page renders as a catalog entry: its first line leads with the name, every later
/// line starts with whitespace (how a reader of `help --full` finds the next entry), every
/// line fits the wrap, the first screen fits one screen, and nothing a row says is lost.
#[test]
fn every_page_renders_whole_within_the_wrap_and_one_screen() {
    assert!(paged().count() >= 10, "the long entries are pages");
    for (verb, page) in paged() {
        assert!(
            verb.detail.is_empty(),
            "{}: a page and a paragraph",
            verb.name
        );
        let mut all = vec![verb.entry_lines()];
        all.extend(page.sections.iter().map(|s| {
            verb.section_lines(s.name)
                .unwrap_or_else(|| panic!("{} {}: a listed section renders", verb.name, s.name))
        }));
        for (lines, head) in all.iter().zip(
            std::iter::once(verb.name.to_string()).chain(
                page.sections
                    .iter()
                    .map(|s| format!("{} {}", verb.name, s.name)),
            ),
        ) {
            assert!(
                lines[0].starts_with(&format!("{head:<w$} ", w = CATALOG_TEXT_COLUMN - 1)),
                "{head}: {lines:#?}"
            );
            for line in lines {
                assert!(
                    line.chars().count() <= ENTRY_WRAP_COLUMNS,
                    "`help {head}` has a line of {} columns: {line}",
                    line.chars().count()
                );
                assert!(
                    line.trim_end() == line && !line.trim().is_empty(),
                    "{head}: {line:?}"
                );
            }
            for line in &lines[1..] {
                assert!(line.starts_with(' '), "{head}: {line:?} starts at column 0");
            }
        }
        let first = &all[0];
        let basics = first
            .iter()
            .take_while(|l| !l.trim_start().starts_with("MORE"))
            .count();
        assert!(
            basics <= 66,
            "`help {}`'s basics are {basics} lines",
            verb.name
        );
        let rendered: Vec<String> = all.iter().flat_map(|l| words(l)).collect();
        for r in [page.usage, page.options, page.reply, page.examples]
            .into_iter()
            .chain(page.sections.iter().map(|s| s.rows))
            .flatten()
        {
            for part in [r.term, r.text] {
                let want: Vec<&str> = part.split_whitespace().collect();
                assert!(holds_run(&rendered, &want), "{}: lost {part:?}", verb.name);
            }
        }
        assert_eq!(verb.full_lines(), all.concat(), "{}", verb.name);
    }
}

/// Each page lists every section in its MORE block, once, and no section's word is a sub-form
/// row of the verb (`help inbox get` is a row, so a section named `get` could not be asked
/// for); an unknown section is none.
#[test]
fn every_section_is_listed_once_and_shadows_no_row() {
    for (verb, page) in paged() {
        let first = verb.entry_lines();
        let more = first
            .iter()
            .position(|l| l.trim_start().starts_with("MORE"))
            .unwrap_or_else(|| panic!("{}: no MORE block", verb.name));
        for (i, s) in page.sections.iter().enumerate() {
            assert!(
                first[more + 1..]
                    .iter()
                    .any(|l| l.trim_start().starts_with(&format!("{} ", s.name))),
                "{} {}: not in MORE",
                verb.name,
                s.name
            );
            assert!(
                page.sections[..i].iter().all(|t| t.name != s.name),
                "{} {}: twice",
                verb.name,
                s.name
            );
            assert!(
                spec(&format!("{} {}", verb.name, s.name)).is_none(),
                "{} {}: a row's name",
                verb.name,
                s.name
            );
            assert!(!s.rows.is_empty(), "{} {}: empty", verb.name, s.name);
        }
        assert!(verb.section_lines("no-such-section").is_none());
    }
    assert!(
        spec("text")
            .and_then(|s| s.section_lines("anything"))
            .is_none()
    );
}

/// `help turn` has an OPTIONS row for every option the server reads before a turn's text
/// ([`TURN_OPTIONS`]), and none for a word it does not read: a new option cannot go unlisted.
#[test]
fn the_turn_page_lists_exactly_the_options_the_server_reads() {
    let page = spec("turn").and_then(|s| s.page).expect("turn is a page");
    let listed: Vec<&str> = page
        .options
        .iter()
        .map(|r| r.term.split_once('=').map_or(r.term, |(k, _)| k))
        .collect();
    assert_eq!(listed, TURN_OPTIONS, "help turn's OPTIONS");
}

/// The renderer: a term within [`TERM_MAX`] keeps its text beside it in one column, a wider
/// one takes its own line, a paragraph wraps at the row indent, and a word wider than a line
/// breaks after a `|` and nowhere else.
#[test]
fn rows_align_wrap_and_break_long_words_after_a_bar() {
    let wide: &'static str = Box::leak("t".repeat(TERM_MAX + 1).into_boxed_str());
    let mut out = Vec::new();
    rows_into(
        &mut out,
        &[
            row("a=<n>", "one"),
            row("bb", "two"),
            row(wide, "below"),
            para("x"),
        ],
    );
    assert_eq!(
        out,
        [
            "    a=<n>  one".to_string(),
            "    bb     two".to_string(),
            format!("    {}", "t".repeat(TERM_MAX + 1)),
            "        below".to_string(),
            "    x".to_string(),
        ]
    );
    let long = format!(
        "<{}>",
        (0..40)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join("|")
    );
    let mut out = Vec::new();
    wrap_into(
        &mut out,
        "    ".to_string(),
        4,
        &format!("lead {long} tail"),
    );
    assert!(
        out.iter().all(|l| l.chars().count() <= ENTRY_WRAP_COLUMNS),
        "{out:#?}"
    );
    assert_eq!(words(&out), ["lead", long.as_str(), "tail"]);
    assert!(out.len() >= 3, "{out:#?}");
}
