// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::collections::BTreeSet;
use std::path::Path;

use super::*;
use crate::guards::Corpus;
use crate::scan_set::test_fixtures::disk_tree;

/// A made-up word the synthetic extras forbid, standing in for the account word
/// (which no test spells).
const WORD: &str = "zebrafish";

/// A synthetic engine baseline: a home row and a word in sections of their own,
/// and a token shape under the credential header (one the tag exempts).
const BASELINE: &str = "# Content patterns, the file's own header.\n\
                        \n\
                        # Personal / machine paths\n\
                        /Users/[A-Za-z]\n\
                        quokkaword\n\
                        \n\
                        # Credentials. Provider token shapes.\n\
                        # A tagged line is exempt from these.\n\
                        tok_[a-z]{8}\n";

fn spots(found: &[Finding]) -> BTreeSet<String> {
    found
        .iter()
        .map(|f| format!("{}:{}", f.path, f.line))
        .collect()
}

/// The rule over a synthetic checkout of `files`, judged with `baseline`.
fn run(
    name: &str,
    files: &[(String, Vec<u8>)],
    baseline: Result<(String, String), String>,
) -> Vec<Finding> {
    let text: Vec<(String, String)> = files
        .iter()
        .filter(|(_, bytes)| !bytes.contains(&0))
        .map(|(rel, bytes)| (rel.clone(), String::from_utf8_lossy(bytes).into_owned()))
        .collect();
    let root = disk_tree("aterm-census-guards", name, &text);
    for (rel, bytes) in files.iter().filter(|(_, bytes)| bytes.contains(&0)) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("rel has a parent")).expect("mkdir");
        std::fs::write(path, bytes).expect("write the binary");
    }
    let corpus = Corpus::read(&root).expect("the synth tree reads");
    let tree = Tree {
        root: &root,
        corpus: &corpus,
    };
    let mut found = Vec::new();
    held_to(&tree, baseline, &mut found);
    let _ = std::fs::remove_dir_all(&root);
    found
}

fn file(rel: &str, text: impl Into<Vec<u8>>) -> (String, Vec<u8>) {
    (rel.to_string(), text.into())
}

/// What the export selects is held, line by line, as its transforms leave it:
/// the word and an untagged token fail, a tagged token passes, the tag does not
/// reach a row outside the credential section, a home the rewrite doubles
/// passes, a binary is held by its bytes, and a file the export withholds and an
/// unlisted file are not read. A path the rows match fails as a path.
#[test]
fn the_exported_text_is_held_to_the_rows_as_the_transforms_leave_it() {
    let home = concat!("/Us", "ers/someone/proj");
    let files = [
        file(
            MANIFEST,
            "# allowlist\nREADME.md\ncrates\ndocs\n!crates/x/proofs\n",
        ),
        file(EXTRA, format!("# synthetic\n{WORD}\n")),
        file("README.md", format!("ok\nsee {home}\n")),
        file(
            "crates/x/src/lib.rs",
            format!(
                "// clean\n// {WORD}\nlet t = \"tok_abcdefgh\"; // {ALLOW_TAG}\n\
                 let t = \"tok_abcdefgh\";\n// quokkaword {ALLOW_TAG}\n"
            ),
        ),
        file(&format!("crates/x/{WORD}.txt"), "clean\n"),
        file("crates/x/proofs/p.txt", format!("{WORD}\n")),
        file("docs/a.md", format!("{WORD}\n")),
        file("tools/other.sh", format!("{WORD}\n")),
        file("crates/x/assets/a.bin", format!("\0{WORD}\n")),
    ];
    let synthetic = || Ok(("synthetic baseline".to_string(), BASELINE.to_string()));
    let found = run("export-content", &files, synthetic());
    assert_eq!(
        spots(&found),
        [
            "crates/x/assets/a.bin:0",
            "crates/x/src/lib.rs:2",
            "crates/x/src/lib.rs:4",
            "crates/x/src/lib.rs:5",
            "crates/x/zebrafish.txt:0",
        ]
        .map(String::from)
        .into(),
        "{found:#?}"
    );
    assert!(
        found
            .iter()
            .all(|f| !f.detail.contains(WORD) && !f.detail.contains("tok_a")),
        "a finding names the row, never the text: {found:#?}"
    );
    // negative control: the home only passes because the rewrite doubles it
    let raw = Row::new("a home".into(), "/Users/[A-Za-z]").expect("compiles");
    assert!(raw.line.is_match(home));
    // without the baseline, its rows (and the tag's reach) go unjudged
    let without = run("export-content-nobase", &files, Err("none".into()));
    assert_eq!(
        spots(&without),
        [
            "crates/x/assets/a.bin:0",
            "crates/x/src/lib.rs:2",
            "crates/x/zebrafish.txt:0"
        ]
        .map(String::from)
        .into(),
        "{without:#?}"
    );
}

/// A file holding a NUL is one the transforms leave as it is and the engine's
/// grep reads as bytes (`-a`): its match is a binary hit, at no line, which no
/// tag clears. So a home the rewrite would double and a tagged token both fail
/// in one, where their text twins pass; a row it matches on two lines is one
/// finding; a word a NUL splits is no match, as grep reads it.
#[test]
fn a_binary_file_is_held_by_its_bytes_at_no_line_and_no_tag_clears_it() {
    let home = concat!("/Us", "ers/someone/proj");
    let token = format!("let t = \"tok_abcdefgh\"; // {ALLOW_TAG}");
    let files = [
        file(MANIFEST, "crates\n"),
        file(EXTRA, format!("{WORD}\n")),
        file(
            "crates/x/a.grid",
            format!("\0\0\x03\0{WORD} %\n\x01{WORD}\n\0"),
        ),
        file("crates/x/home.grid", format!("\0{home} %\n")),
        file("crates/x/home.txt", format!("{home} %\n")),
        file("crates/x/tok.grid", format!("\0{token}\n")),
        file("crates/x/tok.txt", format!("{token}\n")),
        file("crates/x/split.grid", "\0zebra\0fish\n"),
    ];
    let synthetic = Ok(("synthetic baseline".to_string(), BASELINE.to_string()));
    let found = run("export-content-binary", &files, synthetic);
    assert_eq!(
        spots(&found),
        [
            "crates/x/a.grid:0",
            "crates/x/home.grid:0",
            "crates/x/tok.grid:0"
        ]
        .map(String::from)
        .into(),
        "{found:#?}"
    );
    assert_eq!(
        found.len(),
        3,
        "one finding per row a file matches: {found:#?}"
    );
    assert!(
        found
            .iter()
            .all(|f| !f.detail.contains(WORD) && !f.detail.contains("tok_a")),
        "a finding names the row, never the text: {found:#?}"
    );
}

/// A checkout without a manifest (a public snapshot) publishes every file
/// outside the central denies; one whose manifest names a pathspec this guard
/// cannot follow is a finding at that line, not a guess, and one the engine
/// refuses is a finding, never an export of nothing that passes: at the line it
/// refuses, or at line 0 for a manifest that selects no file (the engine's "no
/// tracked files matched", its "empty" and "no allowlist" among them).
#[test]
fn without_a_manifest_everything_outside_the_denies_is_held() {
    let files = [
        file(EXTRA, format!("{WORD}\n")),
        file("crates/x/src/lib.rs", "// x\n"),
        file("tools/other.sh", format!("{WORD}\n")),
        file("docs/a.md", format!("{WORD}\n")),
    ];
    let found = run("export-content-public", &files, Err("none".into()));
    assert_eq!(
        spots(&found),
        ["tools/other.sh:1".to_string()].into(),
        "{found:#?}"
    );
    let mut odd = files.to_vec();
    odd.push(file(MANIFEST, "tools\n!tools/[ab]\n"));
    let found = run("export-content-odd", &odd, Err("none".into()));
    assert_eq!(
        spots(&found),
        [format!("{MANIFEST}:2")].into(),
        "{found:#?}"
    );
    for (manifest, spot) in [
        ("tools\n!\n", 2),
        ("", 0),
        ("# a comment\n\n", 0),
        ("!crates/y\n", 0),
        ("docs\n!crates/y\n", 0),
        ("./docs/a.md\npublish\n.github/\n", 0),
        ("././docs\n", 0),
        ("tools\n!.\n", 0),
        ("tools\n!././\n", 0),
        ("tools\n!tools\n", 0),
        ("tools\n!*\n", 0),
        ("toolz\n", 0),
    ] {
        let mut refused = files.to_vec();
        refused.push(file(MANIFEST, manifest));
        let found = run("export-content-refused", &refused, Err("none".into()));
        assert_eq!(
            spots(&found),
            [format!("{MANIFEST}:{spot}")].into(),
            "{manifest:?}: {found:#?}"
        );
    }
}

/// The credential section is the engine's: its header's first line, a blank
/// line ending it, a pattern listed elsewhere too never exempt.
#[test]
fn the_credential_section_is_read_as_the_engine_reads_it() {
    assert_eq!(credentials(BASELINE), ["tok_[a-z]{8}"].into());
    let text = "# Credentials\nA\nB\n\n# Credentialsx\nC\n# Credentials. a header after a pattern\nD\n\
                # Paths\nB\n\n# Credentials.\n# second header line\nE\n";
    assert_eq!(credentials(text), ["A", "D", "E"].into());
}

/// The checkout's own rows refuse the account word planted in a test line, and
/// its spelling in halves passes: what `verify census` says before a push.
#[test]
fn the_checkouts_own_rows_refuse_a_planted_account_word() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let Ok(extra) = std::fs::read_to_string(root.join(EXTRA)) else {
        return; // a public snapshot withholds publish/
    };
    let word = concat!("aya", "tes");
    let files = [
        file(EXTRA, extra),
        file(
            "crates/x/src/lib_tests.rs",
            format!(
                "let halves = concat!(\"aya\", \"tes\");\nlet p = \"/private/tmp/{word}-scratch\";\n"
            ),
        ),
    ];
    let found = run("export-content-live-rows", &files, Err("none".into()));
    assert_eq!(
        spots(&found),
        ["crates/x/src/lib_tests.rs:2".to_string()].into(),
        "{found:#?}"
    );
}

/// Every match of a pattern holds one of its anchors, so a line holding none
/// cannot match: checked over the shapes the export's rows take (literals,
/// alternatives, `[Xx]` letters, groups, classes, quantifiers, assertions,
/// escapes), each with text it matches. Shapes with no run of two, a flag, or a
/// code point spelled in hex have no anchors and are read whole.
#[test]
fn every_match_of_a_row_holds_one_of_its_anchors() {
    for (pattern, samples, want) in [
        ("zebrafish", &["a zebrafish here"][..], &["zebrafish"][..]),
        (
            "Zebra|ZEBRA|(^|[^-])zebra",
            &["Zebra", "ZEBRA", "zebra", "a zebra"],
            &["Zebra", "ZEBRA", "zebra"],
        ),
        (
            "_[Qq][Pp][Uu]|[Ee][Vv] [Ss][Tt]",
            &["x_qPu", "EV st"],
            &["_qpu", "ev st"],
        ),
        (
            "qterm-|\\bqTerm\\b|QTerm[A-Z]",
            &["qterm-", "a qTerm.", "QTermX"],
            &["qterm-", "qTerm", "QTerm"],
        ),
        (
            "/Hm/[a-z]+/(one|two)([^a-z0-9-]|$)",
            &["/Hm/x/one", "/Hm/xy/two/"],
            &["/Hm/"],
        ),
        ("~/(one|two)(/|$)", &["~/one/"], &["~/"]),
        (
            "-----OPEN [A-Z ]*SECRET THING-----",
            &["-----OPEN A B SECRET THING-----"],
            &["SECRET THING-----"],
        ),
        ("qq[opsur]_[A-Za-z0-9+/=]{4}", &["qqo_ab/="], &["qq"]),
        ("x\\.y+z", &["x.yyz"], &["x."]),
        ("[[:alpha:]]{2}ab|cd", &["xyab", "cd"], &["ab", "cd"]),
        // `\<` and `\>` are word edges, holding no character
        ("\\<zebra\\>", &["a zebra here"], &["zebra"]),
        ("\\<ab\\>|cd\\>", &["x ab y", "xcd"], &["ab", "cd"]),
    ] {
        let re = Regex::new(pattern).expect("compiles");
        let found = anchors(pattern).unwrap_or_else(|| panic!("{pattern} has anchors"));
        let texts: Vec<&str> = found.iter().map(|(a, _)| a.as_str()).collect();
        let want: Vec<String> = want
            .iter()
            .map(|w| {
                if found
                    .iter()
                    .any(|(a, fold)| *fold && a == &w.to_ascii_lowercase())
                {
                    w.to_ascii_lowercase()
                } else {
                    (*w).to_string()
                }
            })
            .collect();
        assert_eq!(texts, want, "{pattern}");
        for sample in samples {
            assert!(re.is_match(sample), "{pattern} matches {sample}");
            let lower = sample.to_ascii_lowercase();
            assert!(
                found.iter().any(|(a, fold)| if *fold {
                    lower.contains(a.as_str())
                } else {
                    sample.contains(a.as_str())
                }),
                "{sample} holds an anchor of {pattern}: {found:?}"
            );
        }
    }
    for unanchored in [
        "ab?c",
        "a|bc",
        "(?i)abc",
        "[ab]c",
        "a.b",
        "(ab)",
        "\\x41BC",
        "tok\\x5fsecret",
        "tok\\x{5f}secret",
        "\\u0041BC",
        "\\U00000041BC",
    ] {
        assert!(anchors(unanchored).is_none(), "{unanchored}");
    }
    // what the hex rows match, which a whole read finds
    for (pattern, sample) in [("\\x41BC", "ABC"), ("tok\\x5fsecret", "a tok_secret")] {
        let row = Row::new("hex".into(), pattern).expect("compiles");
        assert!(row.line.is_match(sample), "{pattern}");
        let held = Held::new(vec![&row], vec![false]);
        let text = format!("one\n{sample}\n");
        assert!(
            held.candidates(text.as_bytes()).contains(&(0, 1)),
            "{pattern} reads the line it matches"
        );
    }
}

/// The one pass over the bytes names each line an anchor sits on, either case
/// for a folded one, and a row without anchors is read on every line of a file
/// its regex matches somewhere.
#[test]
fn the_candidates_are_the_lines_an_anchor_sits_on() {
    let rows = [
        Row::new("word".into(), "zebrafish").expect("compiles"),
        Row::new("folded".into(), "[Qq][Uu][Oo]").expect("compiles"),
        Row::new("whole".into(), "a.c").expect("compiles"),
    ];
    let held = Held::new(rows.iter().collect(), vec![false; 3]);
    assert_eq!(held.whole, [2]);
    let text = b"one\nzebrafish QUO\nquo\nabc\n\nzebra";
    assert_eq!(
        held.candidates(text),
        [(0, 1), (1, 1), (1, 2)]
            .into_iter()
            .chain((0..=5).map(|l| (2, l)))
            .collect()
    );
    assert_eq!(held.candidates(b"one\nzebrafish\nlast"), [(0, 1)].into());
}
