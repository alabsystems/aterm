// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_types::control_verbs::{
    CATALOG_TEXT_COLUMN, ENTRY_WRAP_COLUMNS, VERBS, catalog_lines, spec,
};

#[test]
fn help_short_form_is_the_summary_catalog() {
    let h = super::cmd_help("");
    let mut lines = h.lines();
    let status = lines.next().unwrap();
    assert_eq!(
        status,
        format!("OK {} aterm introspection protocol v1", VERBS.len() + 3)
    );
    let header: Vec<&str> = lines.clone().take(3).collect();
    assert!(header.iter().all(|l| l.starts_with("# ")), "{header:?}");
    assert!(
        header[0].contains("ERR") && header[0].contains("empty"),
        "framing line documents the OK/ERR/empty framing: {}",
        header[0]
    );
    let rows: Vec<&str> = lines.skip(3).collect();
    let want: Vec<String> = catalog_lines().collect();
    assert_eq!(rows, want, "the rows are exactly the table's summary rows");
    assert!(
        !h.contains("image --meta"),
        "a full entry leaked into the short form"
    );
    assert_eq!(super::cmd_help("  "), h);
}

#[test]
fn help_verb_form_is_the_wrapped_full_entry() {
    let h = super::cmd_help("image");
    let mut lines = h.lines();
    let status = lines.next().unwrap();
    let rows: Vec<&str> = lines.collect();
    assert_eq!(status, format!("OK {}", rows.len()));
    assert!(rows.len() > 5, "image's entry must wrap into several rows");
    let gutter = " ".repeat(CATALOG_TEXT_COLUMN);
    for row in &rows[1..] {
        assert!(
            row.starts_with(&gutter),
            "continuation row not indented: {row:?}"
        );
    }
    for row in &rows {
        assert!(
            row.chars().count() <= ENTRY_WRAP_COLUMNS,
            "row wider than {ENTRY_WRAP_COLUMNS}: {row:?}"
        );
    }
    let joined: Vec<&str> = std::iter::once(&rows[0][CATALOG_TEXT_COLUMN..])
        .chain(rows[1..].iter().map(|r| &r[CATALOG_TEXT_COLUMN..]))
        .collect();
    assert_eq!(joined.join(" "), spec("image").unwrap().help_line());
    assert_eq!(super::cmd_help(" image "), h, "the tail is trimmed");
}

#[test]
fn help_rejects_unknown_verbs_and_bad_tails() {
    assert_eq!(
        super::cmd_help("nosuch"),
        "ERR unknown verb 'nosuch' (help lists them)\n"
    );
    let inbox_get = super::cmd_help("inbox get");
    assert!(inbox_get.starts_with("OK "), "{inbox_get}");
    assert!(
        inbox_get
            .lines()
            .nth(1)
            .is_some_and(|row| row.starts_with("inbox get ")),
        "{inbox_get}"
    );
    assert_eq!(
        super::cmd_help(" inbox \t get "),
        inbox_get,
        "any separator"
    );
    for tail in [
        "-h",
        "--jsn",
        "image --full",
        "--full image",
        "--json image",
        "a b",
        "inbox nope",
        "inbox get extra",
    ] {
        assert_eq!(
            super::cmd_help(tail),
            "ERR usage: help [<verb> [<section>] | --full | --json]\n",
            "tail {tail:?}"
        );
    }
}

/// `help <verb> <section>` answers that part of a long entry, framed as an entry is; a word
/// the verb's page has no section for is refused by name.
#[test]
fn help_verb_section_is_one_part_of_a_long_entry() {
    let turn = spec("turn").expect("turn ships");
    let page = turn.page.expect("turn's entry is a page");
    assert!(!page.sections.is_empty());
    for section in page.sections {
        let h = super::cmd_help(&format!(" turn \t{} ", section.name));
        let mut lines = h.lines();
        let status = lines.next().unwrap();
        let rows: Vec<&str> = lines.collect();
        assert_eq!(status, format!("OK {}", rows.len()));
        assert_eq!(rows, turn.section_lines(section.name).expect("listed"));
    }
    assert_eq!(
        super::cmd_help("turn nope"),
        "ERR unknown section 'nope' of turn (help turn lists them)\n"
    );
    let first = super::cmd_help("turn");
    let named = page.sections.iter().all(|section| {
        first
            .lines()
            .any(|l| l.trim_start().starts_with(&format!("{} ", section.name)))
    });
    assert!(named, "help turn lists its sections: {first}");
}

#[test]
fn help_full_is_the_header_and_every_catalog_row_in_order() {
    let got = super::cmd_help("--full");
    assert_eq!(super::cmd_help(" --full "), got, "the tail is trimmed");
    assert!(!got.trim().is_empty(), "help --full is empty");
    let mut rest = got.as_str();
    for row in aterm_types::control_verbs::catalog_lines_full() {
        let at = rest
            .find(&row)
            .unwrap_or_else(|| panic!("help --full lost (or reordered) the row: {row}"));
        rest = &rest[at + row.len()..];
    }
}
