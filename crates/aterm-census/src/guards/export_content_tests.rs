// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use std::collections::BTreeSet;
use std::path::Path;

use super::*;
use crate::guards::Corpus;
use crate::scan_set::test_fixtures::disk_tree;

const WORD: &str = "zebrafish";

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

fn run(
    name: &str,
    files: &[(String, Vec<u8>)],
    baseline: Result<(String, String), String>,
) -> Vec<Finding> {
    let text: Vec<(String, String)> = files
        .iter()
        .filter(|(_, bytes)| !bytes.contains(&0))
        .filter_map(|(rel, bytes)| Some((rel.clone(), String::from_utf8(bytes.clone()).ok()?)))
        .collect();
    let root = disk_tree("aterm-census-guards", name, &text);
    let raw = |rel: &String| !text.iter().any(|(text, _)| text == rel);
    for (rel, bytes) in files.iter().filter(|(rel, _)| raw(rel)) {
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
    let raw = Row::new("a home".into(), "/Users/[A-Za-z]").expect("compiles");
    assert!(raw.line.is_match(home));
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

#[test]
fn a_nul_is_binary_only_where_the_engines_grep_looks_for_one() {
    let token = format!("let t = \"tok_abcdefgh\"; // {ALLOW_TAG}");
    let head = format!("{token}\n{WORD}\n");
    let nul_at = |at: usize| format!("{head}{}\0\n", "A".repeat(at - head.len()));
    let files = [
        file(MANIFEST, "crates\n"),
        file(EXTRA, format!("{WORD}\n")),
        file("crates/x/inside.dat", nul_at(GREP_HEAD - 1)),
        file("crates/x/after.dat", nul_at(GREP_HEAD)),
    ];
    let synthetic = Ok(("synthetic baseline".to_string(), BASELINE.to_string()));
    let found = run("export-content-late-nul", &files, synthetic);
    assert_eq!(
        spots(&found),
        ["crates/x/after.dat:2", "crates/x/inside.dat:0"]
            .map(String::from)
            .into(),
        "{found:#?}"
    );
    assert_eq!(
        found.len(),
        3,
        "inside: both rows at line 0, the tag clearing neither; after: the tagged line \
         clears: {found:#?}"
    );
}

#[test]
fn a_line_that_is_not_utf8_is_read_as_the_engine_decodes_it() {
    let tagged = |e: &[u8]| {
        [
            b"let t = \"tok_abcdefgh\"; // ".as_slice(),
            e,
            b" ",
            ALLOW_TAG.as_bytes(),
            b"\n",
        ]
        .concat()
    };
    let (latin1, utf8) = (tagged(b"\xe9"), tagged("\u{e9}".as_bytes()));
    let late_nul = |mut text: Vec<u8>| {
        text.resize(GREP_HEAD, b'A');
        text.extend_from_slice(b"\0\n");
        text
    };
    let mut plain = latin1.clone();
    plain.extend_from_slice(b"// caf\xe9, which matches nothing\n");
    plain.extend_from_slice(&[WORD.as_bytes(), b" \xe9\n"].concat());
    plain.extend_from_slice(b"let t = \"tok_abcdefgh\"; // caf\xe9\n");
    plain.extend_from_slice(
        &[
            b"let t = \"tok_abcdefgh\"; // caf\xe9 quokkaword ".as_slice(),
            ALLOW_TAG.as_bytes(),
            b"\n",
        ]
        .concat(),
    );
    let files = [
        file(MANIFEST, "crates\n"),
        file(EXTRA, format!("{WORD}\n")),
        file("crates/x/latin1.txt", plain),
        file("crates/x/late-latin1.dat", late_nul(latin1)),
        file("crates/x/utf8.txt", utf8.clone()),
        file("crates/x/late-utf8.dat", late_nul(utf8)),
    ];
    let synthetic = Ok(("synthetic baseline".to_string(), BASELINE.to_string()));
    let found = run("export-content-not-utf8", &files, synthetic);
    assert_eq!(
        spots(&found),
        [
            "crates/x/latin1.txt:3",
            "crates/x/latin1.txt:4",
            "crates/x/latin1.txt:5",
        ]
        .map(String::from)
        .into(),
        "each line as the engine reads it, its grep matching bytes and its output decoded lossily: \
         a tagged line is exempt from a credential row, and an untagged credential, or \
         another row's match on a tagged line, is a hit: {found:#?}"
    );
    assert!(
        found
            .iter()
            .all(|f| f.detail.contains("replace the text") && !f.detail.contains("UTF-8")),
        "each is an ordinary hit: {found:#?}"
    );
}

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

#[test]
fn the_credential_section_is_read_as_the_engine_reads_it() {
    assert_eq!(credentials(BASELINE), ["tok_[a-z]{8}"].into());
    let text = "# Credentials\nA\nB\n\n# Credentialsx\nC\n# Credentials. a header after a pattern\nD\n\
                # Paths\nB\n\n# Credentials.\n# second header line\nE\n";
    assert_eq!(credentials(text), ["A", "D", "E"].into());
}

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

fn utf16(little: bool, text: &str) -> Vec<u8> {
    let mut out = if little {
        vec![0xff, 0xfe]
    } else {
        vec![0xfe, 0xff]
    };
    for unit in text.encode_utf16() {
        out.extend(if little {
            unit.to_le_bytes()
        } else {
            unit.to_be_bytes()
        });
    }
    out
}

fn utf32(little: bool, text: &str) -> Vec<u8> {
    let mut out = if little {
        vec![0xff, 0xfe, 0, 0]
    } else {
        vec![0, 0, 0xfe, 0xff]
    };
    for c in text.chars() {
        out.extend(if little {
            u32::from(c).to_le_bytes()
        } else {
            u32::from(c).to_be_bytes()
        });
    }
    out
}

#[test]
fn a_compressed_file_is_held_by_what_the_engine_decodes_it_to() {
    use crate::export::plaintext::tests::{
        ZST_CLEAN, ZST_DICTIONARY, ZST_WORD, ZST_WORD_IN_BOUND, ZST_WORD_PAST_BOUND, gzip_member,
    };
    let stamp = *b"zqvx";
    let mut skipped = vec![0x50, 0x2a, 0x4d, 0x18, 4, 0, 0, 0];
    skipped.extend(stamp);
    skipped.extend(ZST_CLEAN);
    let word = gzip_member(format!("a {WORD}\n").as_bytes(), [0; 4], &[]);
    let split = [
        gzip_member(b"a zeb", [0; 4], &[]),
        gzip_member(b"rafish\n", [0; 4], &[]),
    ]
    .concat();
    let mut trailing = gzip_member(b"clean\n", stamp, &[]);
    trailing.extend(b"junk");
    let frames = |frames: &[&[u8]]| {
        let mut out = Vec::new();
        for frame in frames {
            aterm_zstd::frame::write_raw(&mut out, frame, aterm_zstd::frame::Checksum::Xxh64)
                .expect("a frame");
        }
        out
    };
    let files = [
        file(MANIFEST, "crates\n"),
        file(EXTRA, format!("{WORD}\nzqvx\n")),
        file("crates/x/word.gz", word.clone()),
        file(
            "crates/x/clean.gz",
            gzip_member(b"a zebra fish\n", [0; 4], &[]),
        ),
        file(
            "crates/x/name.gz",
            gzip_member(
                b"clean\n",
                [0; 4],
                &[(0x08, format!("{WORD}.txt").as_bytes())],
            ),
        ),
        file("crates/x/stamp.gz", gzip_member(b"clean\n", stamp, &[])),
        file("crates/x/broken.gz", word[..14].to_vec()),
        file("crates/x/split.gz", split.clone()),
        file("crates/x/cut.gz", split[..split.len() - 4].to_vec()),
        file("crates/x/trailing.gz", trailing),
        file("crates/x/split.zst", frames(&[b"zebra", b"fish\n"])),
        file("crates/x/word.zst", ZST_WORD),
        file("crates/x/clean.zst", ZST_CLEAN),
        file("crates/x/in-bound.zst", ZST_WORD_IN_BOUND),
        file("crates/x/past-bound.zst", ZST_WORD_PAST_BOUND),
        file("crates/x/skipped.zst", skipped),
        file(
            "crates/x/trail-clean.zst",
            [ZST_CLEAN, b"junkjunk"].concat(),
        ),
        file("crates/x/trail-word.zst", [ZST_WORD, b"junkjunk"].concat()),
        file("crates/x/dictionary.zst", ZST_DICTIONARY),
    ];
    for (rel, bytes) in &files[2..] {
        assert!(
            bytes.contains(&0),
            "{rel} is binary: the transforms leave it as it is"
        );
        let raw = bytes.windows(WORD.len()).any(|w| w == WORD.as_bytes());
        assert_eq!(
            raw,
            rel.ends_with("name.gz"),
            "{rel}: only a header field spells it"
        );
    }
    let found = run("export-content-streams", &files, Err("none".into()));
    assert_eq!(
        spots(&found),
        [
            "crates/x/broken.gz:0",
            "crates/x/cut.gz:0",
            "crates/x/in-bound.zst:0",
            "crates/x/name.gz:0",
            "crates/x/skipped.zst:0",
            "crates/x/split.gz:0",
            "crates/x/split.zst:0",
            "crates/x/trail-word.zst:0",
            "crates/x/trailing.gz:0",
            "crates/x/word.gz:0",
            "crates/x/word.zst:0",
        ]
        .map(String::from)
        .into(),
        "stamp.gz decodes whole, so the engine no longer greps its header's bytes: {found:#?}"
    );
    let said = |rel: &str| -> Vec<&str> {
        found
            .iter()
            .filter(|f| f.path == format!("crates/x/{rel}"))
            .map(|f| f.detail.as_str())
            .collect()
    };
    let is = |rel: &str, what: &str| {
        let details = said(rel);
        assert!(
            details.len() == 1 && details[0].contains(what),
            "{rel}: {what}: {details:#?}"
        );
    };
    for rel in [
        "word.gz",
        "name.gz",
        "split.gz",
        "word.zst",
        "skipped.zst",
        "split.zst",
    ] {
        is(rel, "what it decodes to matches");
    }
    for rel in ["cut.gz", "in-bound.zst", "trail-word.zst"] {
        is(rel, "before its stream stops where the engine's does");
    }
    is("trailing.gz", "its bytes match");
    is("broken.gz", "not judged");
}

#[test]
fn a_bom_file_is_searched_whole_as_the_engines_python_reads_it() {
    let files = [
        file(MANIFEST, "crates\n"),
        file(EXTRA, format!("{WORD}\nquokka(/|$)\n\\bnumbat\\b\n")),
        file("crates/x/le.txt", utf16(true, &format!("a {WORD}\n"))),
        file("crates/x/le-clean.txt", utf16(true, "a zebra fish\n")),
        file("crates/x/be32.txt", utf32(false, WORD)),
        file("crates/x/le32-clean.txt", utf32(true, "clean\n")),
        file("crates/x/end.txt", utf16(true, "x quokka\n")),
        file("crates/x/mid.txt", utf16(true, "x quokka\nmore\n")),
        file("crates/x/wide.txt", utf16(false, "\u{e9} numbat\n")),
        file("crates/x/wide-clean.txt", utf16(false, "\u{e9} wombat\n")),
        file("crates/x/ascii.txt", utf16(false, "a numbat\n")),
    ];
    let found = run("export-content-bom", &files, Err("none".into()));
    assert_eq!(
        spots(&found),
        [
            "crates/x/ascii.txt:0",
            "crates/x/be32.txt:0",
            "crates/x/end.txt:0",
            "crates/x/le.txt:0",
            "crates/x/wide.txt:0",
        ]
        .map(String::from)
        .into(),
        "{found:#?}"
    );
    let wide: Vec<_> = found
        .iter()
        .filter(|f| f.path.ends_with("wide.txt"))
        .collect();
    assert!(
        wide.len() == 1 && wide[0].detail.contains("not judged"),
        "{wide:#?}"
    );
}

#[test]
fn a_row_reads_alike_in_python_only_where_both_engines_agree() {
    for (pattern, want) in [
        ("zebrafish", Python::Alike),
        ("~/(clean|trust)(/|$)", Python::Alike),
        ("/home/[a-z]+/(ay|ty)([^a-z0-9-]|$)", Python::Alike),
        ("a$|b", Python::Alike),
        ("(a$)|b", Python::Alike),
        ("x\\.y[\\-_]", Python::Alike),
        ("(?:ab)+", Python::Alike),
        ("\\bqterm\\b", Python::AsciiAlike),
        ("[\\d]{4}", Python::AsciiAlike),
        ("(?i)zebra", Python::AsciiAlike),
        ("(a$|b)c", Python::Apart),
        ("a$\\n", Python::Apart),
        ("(a$)*", Python::Apart),
        ("\\<zebra\\>", Python::Apart),
        ("[[:alpha:]]x", Python::Apart),
        ("a{,3}", Python::Apart),
        ("(?s)a.b", Python::Apart),
        ("(?=a)", Python::Apart),
        ("\u{e9}t\u{e9}", Python::Apart),
        ("\\z", Python::Apart),
    ] {
        assert_eq!(python(pattern), want, "{pattern}");
    }
    let row = Row::new("a row".into(), "a[^b]c").expect("compiles");
    assert!(
        row.line.is_match("a\nc"),
        "a negated class holds a newline, as in Python"
    );
    let row = Row::new("a row".into(), "a.c").expect("compiles");
    assert!(!row.line.is_match("a\nc"), "a dot does not, as in Python");
}
