// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::path::Path;

use super::*;
use crate::guards::Corpus;

const WORD: &str = "zebrafish";

const HOME: &str = concat!("/Us", "ers/someone");
const CHECKOUT: &str = concat!("~", "/trust");

fn leaky() -> String {
    let cwd = format!("\x1b]7;file://host{HOME}/proj\x07");
    let title = format!("\x1b]0;{CHECKOUT}\x07");
    ptylog_text(&[
        ("O", cwd.as_bytes()),
        ("O", b"\x1b[1mzebr\x1b[22mafish@host ~ % "),
        ("I", b"ls\r"),
        (
            "O",
            "\x1b[38;2;1;2;3mTest 1.0 \u{b7} Claude Max\r\n".as_bytes(),
        ),
        (
            "O",
            b"claude --resume 1b2c3d4e-5f60-4a7b-8c9d-0e1f2a3b4c5d\r\n",
        ),
        ("O", title.as_bytes()),
    ])
}

fn scrubbed() -> String {
    ptylog_text(&[
        ("O", b"\x1b]7;file://host/Users//owner/proj\x07"),
        ("O", b"\x1b[1mowne\x1b[22mr0000@host ~ % "),
        ("I", b"ls\r"),
        (
            "O",
            "\x1b[38;2;1;2;3mTest 1.0 \u{b7} Sample App\r\n".as_bytes(),
        ),
        (
            "O",
            b"claude --resume 00000001-0000-4000-8000-000000000001\r\n",
        ),
        ("O", b"\x1b]0;~/myapp\x07"),
    ])
}

const NO_BASELINE: &str = "no engine baseline in tests";

fn rows() -> Vec<Row> {
    let mut found = Vec::new();
    let mut rows = file_rows(
        &format!("# synthetic\n{WORD}\n"),
        EXTRA,
        EXTRA,
        CAPTURE_PRIVACY.id,
        &mut found,
    );
    assert!(found.is_empty(), "{found:?}");
    rows.extend(capture_rows());
    rows
}

fn judged(rel: &str, text: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    judge(rel, text, &rows(), &mut out);
    out
}

fn spots(found: &[Finding]) -> BTreeSet<String> {
    found
        .iter()
        .map(|f| format!("{}:{}", f.path, f.line))
        .collect()
}

#[test]
fn a_leaky_capture_fails_at_each_record_and_its_scrubbed_twin_passes() {
    let found = judged("t/leaky.ptylog", &leaky());
    assert_eq!(
        spots(&found),
        ["1", "2", "4", "5", "6"]
            .map(|l| format!("t/leaky.ptylog:{l}"))
            .into(),
        "{found:#?}"
    );
    assert!(
        found
            .iter()
            .any(|f| f.line == 2 && f.detail.contains(EXTRA)),
        "the word an SGR splits is the export's row: {found:#?}"
    );
    assert!(
        found
            .iter()
            .all(|f| !f.detail.contains(WORD) && !f.detail.contains(HOME)),
        "a finding names the row, never the text: {found:#?}"
    );
    let clean = judged("t/scrubbed.ptylog", &scrubbed());
    assert!(clean.is_empty(), "{clean:#?}");
}

#[test]
fn the_rule_reads_the_checkouts_rows_and_judges_every_capture_it_holds() {
    use crate::scan_set::test_fixtures::disk_tree;
    let files = [
        (EXTRA.to_string(), format!("# synthetic\n{WORD}\n")),
        ("crates/x/src/lib.rs".to_string(), "// x\n".to_string()),
        ("crates/x/tests/fixtures/leaky.ptylog".to_string(), leaky()),
        (
            "crates/x/tests/fixtures/scrubbed.ptylog".to_string(),
            scrubbed(),
        ),
        (
            "crates/y/testdata/leaky.reads".to_string(),
            format!("# comment {WORD}\n\\e[1mzeb\\e[0mrafish\\r\\n\nok\n"),
        ),
    ];
    let root = disk_tree("aterm-census-guards", "capture-privacy", &files);
    let corpus = Corpus::read(&root).expect("the synth tree reads");
    let tree = Tree {
        root: &root,
        corpus: &corpus,
    };
    let mut found = Vec::new();
    held_to(&tree, Err(NO_BASELINE.to_string()), &mut found);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        spots(&found),
        [
            "crates/x/tests/fixtures/leaky.ptylog:1",
            "crates/x/tests/fixtures/leaky.ptylog:2",
            "crates/x/tests/fixtures/leaky.ptylog:4",
            "crates/x/tests/fixtures/leaky.ptylog:5",
            "crates/x/tests/fixtures/leaky.ptylog:6",
            "crates/y/testdata/leaky.reads:2",
        ]
        .map(String::from)
        .into(),
        "{found:#?}"
    );
}

#[test]
fn a_record_the_guard_cannot_decode_is_a_finding_not_a_pass() {
    let found = judged("t/bad.ptylog", "1 O 6162\n2 O 6g\n");
    assert_eq!(spots(&found), ["t/bad.ptylog:2".to_string()].into());
    for (text, line) in [
        ("1 O 6162\n2 X 6162\n", 2),
        ("1 O 6162 63\n", 1),
        ("1 O 61\n2\n", 2),
    ] {
        assert_eq!(
            spots(&judged("t/odd.ptylog", text)),
            [format!("t/odd.ptylog:{line}")].into(),
            "{text:?}"
        );
    }
    let found = judged("t/bad.reads", "ab\\q\n");
    assert_eq!(spots(&found), ["t/bad.reads:1".to_string()].into());
    assert!(judged("t/fence.ptylog", "1 C\n2 O 6162\n").is_empty());
}

#[test]
fn the_views_join_a_word_an_sgr_splits_and_end_one_a_cursor_move_ends() {
    let stream = b"ab\x1b[1mcd\x1b[5Gef\x1b]0;title\x07gh\r\nij\x1b(Bk";
    let joined = View::printable(stream, Read::Joined);
    assert_eq!(joined.bytes, b"abcdef\n0;title\ngh\nijk");
    let spaced = View::printable(stream, Read::Spaced);
    assert_eq!(spaced.bytes, b"ab\ncd\nef\n0;title\ngh\nijk");
    assert_eq!(joined.from[2], 6);
    let raw = View::raw(stream);
    assert_eq!(raw.lines().count(), 2);
}

#[test]
fn a_finding_names_the_record_holding_the_match_not_the_first() {
    let text = ptylog_text(&[("O", b"plain "), ("O", b"text "), ("O", b"zebrafish")]);
    let found = judged("t/late.ptylog", &text);
    assert_eq!(spots(&found), ["t/late.ptylog:3".to_string()].into());
}

#[test]
fn the_exports_rows_compile_and_every_capture_row_does() {
    assert_eq!(capture_rows().len(), CAPTURE_ROWS.len());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = match std::fs::read_to_string(root.join(EXTRA)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            assert!(!notes(&root).is_empty(), "an absent {EXTRA} is noted");
            return;
        }
        Err(e) => panic!("{EXTRA}: {e}"),
    };
    let mut found = Vec::new();
    let rows = file_rows(&text, EXTRA, EXTRA, CAPTURE_PRIVACY.id, &mut found);
    assert!(found.is_empty(), "{found:#?}");
    assert!(rows.len() >= 2, "{} rows", rows.len());
}

#[test]
fn without_the_exports_rows_the_capture_rows_still_hold() {
    use crate::scan_set::test_fixtures::disk_tree;
    let files = [
        ("crates/x/src/lib.rs".to_string(), "// x\n".to_string()),
        ("crates/x/tests/fixtures/leaky.ptylog".to_string(), leaky()),
        (
            "crates/x/tests/fixtures/scrubbed.ptylog".to_string(),
            scrubbed(),
        ),
    ];
    let root = disk_tree("aterm-census-guards", "capture-privacy-public", &files);
    let corpus = Corpus::read(&root).expect("the synth tree reads");
    let tree = Tree {
        root: &root,
        corpus: &corpus,
    };
    let mut found = Vec::new();
    held_to(&tree, Err(NO_BASELINE.to_string()), &mut found);
    let noted = notes(&root);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        spots(&found),
        ["1", "4", "5", "6"]
            .map(|l| format!("crates/x/tests/fixtures/leaky.ptylog:{l}"))
            .into(),
        "{found:#?}"
    );
    assert!(noted.iter().any(|n| n.contains(EXTRA)), "{noted:?}");
}

#[test]
fn the_redaction_shapes_pass_every_capture_row() {
    let rows = capture_rows();
    for redacted in [
        "/Users//owner/aterm/AGENTS.md",
        "/home//owner/aterm",
        "claude --resume 00000001-0000-4000-8000-000000000012",
        "Test 1.0 with xhigh effort \u{b7} Sample App",
        "~/myapp000",
        "owner0@hostmachine0000 aterm % ",
    ] {
        for row in &rows {
            assert!(
                !row.line.is_match(redacted),
                "{} matches {redacted:?}",
                row.source
            );
        }
    }
}

#[test]
fn the_edited_view_reads_a_word_fixed_while_it_was_typed() {
    let echo = View::printable(b"zebx\x08 \x08rafish\r\n\x08ok", Read::Edited);
    assert_eq!(echo.bytes, b"zebrafish\nok");
    let typed = View::printable(b"zebx\x7frafish", Read::Edited);
    assert_eq!(typed.bytes, b"zebrafish");
    let joined = View::printable(b"zebx\x08 \x08rafish", Read::Joined);
    assert_eq!(joined.bytes, b"zebx\n \nrafish");
    let found = judged(
        "t/fixed.ptylog",
        &ptylog_text(&[
            ("I", b"zebx\x7frafish\r"),
            ("O", b"zebx\x08 \x08rafish\r\n"),
        ]),
    );
    assert_eq!(
        spots(&found),
        ["t/fixed.ptylog:1", "t/fixed.ptylog:2"]
            .map(String::from)
            .into(),
        "{found:#?}"
    );
}

#[test]
fn the_engines_baseline_rows_are_held_when_the_host_has_them() {
    use crate::scan_set::test_fixtures::disk_tree;
    let files = [
        (EXTRA.to_string(), format!("# synthetic\n{WORD}\n")),
        ("crates/x/src/lib.rs".to_string(), "// x\n".to_string()),
        (
            "crates/x/tests/fixtures/base.ptylog".to_string(),
            ptylog_text(&[("O", b"plain "), ("O", b"\x1b[1mquok\x1b[0mkaword\r\n")]),
        ),
    ];
    let root = disk_tree("aterm-census-guards", "capture-privacy-baseline", &files);
    let corpus = Corpus::read(&root).expect("the synth tree reads");
    let tree = Tree {
        root: &root,
        corpus: &corpus,
    };
    let mut found = Vec::new();
    held_to(
        &tree,
        Ok((
            "synthetic baseline".to_string(),
            "# rows\nquokkaword\n".to_string(),
        )),
        &mut found,
    );
    let mut without = Vec::new();
    held_to(&tree, Err(NO_BASELINE.to_string()), &mut without);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        spots(&found),
        ["crates/x/tests/fixtures/base.ptylog:2".to_string()].into(),
        "{found:#?}"
    );
    assert!(
        found
            .iter()
            .all(|f| f.detail.contains("the engine's baseline:2")),
        "{found:#?}"
    );
    assert!(without.is_empty(), "{without:#?}");
}
