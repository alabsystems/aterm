// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `docs/PROCESS.md` against the ladder it documents.
//!
//! WHY (2026-09-13). §5 of PROCESS.md enumerates the `--fast` ladder and said
//! the order "is pinned by `the_fast_ladder_is_the_documented_stages_in_the_documented_order`
//! … so this list cannot drift from the code without that test failing". No
//! test read the document. By the speed round it said "19-stage" in its table,
//! listed 20 stages, called them "the twenty stages" further down, and the
//! ladder printed 29: the atpkg publish tooling, the driver builds and seven
//! objc drives had joined without a line of it moving. This is the test that
//! sentence promised.
//!
//! What it holds the document to, and nothing more: the numbered list in the
//! `--fast` ladder paragraph names `plan()`'s `--fast` titles, in order, each as
//! the first backticked span of its entry (whitespace, and the bold around a
//! span, normalised; prose after the span is the document's own); and every
//! count the section gives for the ladder — `N-stage`, `the N stages` — is the
//! length of that ladder.

use std::path::{Path, PathBuf};

use aterm_verify::cli::Mode;
use aterm_verify::{Ctx, EnvSnapshot, Scope, plan};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/aterm-verify sits two levels below the workspace root")
        .to_path_buf()
}

fn process_md() -> String {
    let path = workspace_root().join("docs/PROCESS.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// §5, from its heading to the next `## ` heading.
fn ladder_section(doc: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in doc.lines() {
        if line.starts_with("## ") {
            if inside {
                break;
            }
            inside = line.contains("The local gate ladder");
            continue;
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    assert!(
        !out.is_empty(),
        "docs/PROCESS.md has no `## … The local gate ladder` section"
    );
    out
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The numbered entries of the list that follows the `--fast` ladder
/// paragraph, as `(number, text)` with continuation lines joined.
fn numbered_list(section: &str) -> Vec<(usize, String)> {
    let lead = "**The `--fast` ladder, in the order it prints.**";
    let start = section
        .find(lead)
        .unwrap_or_else(|| panic!("§5 no longer has the paragraph starting {lead:?}"));
    let mut items: Vec<(usize, String)> = Vec::new();
    let mut in_list = false;
    for line in section[start..].lines() {
        let trimmed = line.trim_start();
        let number = trimmed
            .split_once(". ")
            .filter(|(n, _)| {
                line == trimmed && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
            })
            .map(|(n, rest)| (n.parse::<usize>().expect("digits"), rest));
        match number {
            Some((n, rest)) => {
                in_list = true;
                items.push((n, rest.to_string()));
            }
            None if in_list && line.trim().is_empty() => break,
            None if in_list => {
                let last = items.last_mut().expect("a continuation follows an entry");
                last.1.push(' ');
                last.1.push_str(trimmed);
            }
            None => {}
        }
    }
    items
}

/// An entry's stage title: its first backticked span, whitespace collapsed.
fn entry_title(text: &str) -> Option<String> {
    let text = text.replace("**", "");
    let mut parts = text.splitn(3, '`');
    let _before = parts.next()?;
    let title = parts.next()?;
    parts.next()?; // the closing backtick must exist
    Some(collapse_ws(title))
}

fn number_word(w: &str) -> Option<usize> {
    const ONES: [&str; 20] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ];
    const TENS: [&str; 8] = [
        "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    if !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()) {
        return w.parse().ok();
    }
    if let Some(i) = ONES.iter().position(|o| *o == w) {
        return Some(i);
    }
    let (tens, ones) = w.split_once('-').unwrap_or((w, ""));
    let t = TENS.iter().position(|x| *x == tens)?;
    let o = if ones.is_empty() {
        0
    } else {
        ONES[1..10].iter().position(|x| *x == ones)? + 1
    };
    Some((t + 2) * 10 + o)
}

/// Every count §5 gives the ladder: `N-stage` and `the N stages`, N a
/// numeral or a number word (`twenty-nine` included). `(phrase, N)`.
fn ladder_counts(section: &str) -> Vec<(String, usize)> {
    let text = collapse_ws(&section.replace("**", "").replace('`', ""));
    let words: Vec<String> = text
        .split(' ')
        .map(|w| {
            w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .to_ascii_lowercase()
        })
        .collect();
    let mut out = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if let Some(n) = w.strip_suffix("-stage").and_then(number_word) {
            out.push((w.clone(), n));
        }
        if w == "the"
            && let (Some(n), Some("stages")) = (
                words.get(i + 1).and_then(|x| number_word(x)),
                words.get(i + 2).map(String::as_str),
            )
        {
            out.push((format!("the {} stages", words[i + 1]), n));
        }
    }
    out
}

fn fast_titles() -> Vec<String> {
    let scratch = std::env::temp_dir();
    let ctx = Ctx::new(
        workspace_root(),
        Mode::Fast,
        Scope::workspace(),
        false,
        EnvSnapshot::default(),
        scratch,
    );
    plan::plan(&ctx).into_iter().map(|s| s.title).collect()
}

#[test]
fn the_process_doc_lists_the_fast_ladder_plan_prints_in_its_order() {
    let items = numbered_list(&ladder_section(&process_md()));
    let want = fast_titles();
    for (i, (n, _)) in items.iter().enumerate() {
        assert_eq!(
            *n,
            i + 1,
            "docs/PROCESS.md §5: entry {} is numbered {n}",
            i + 1
        );
    }
    let got: Vec<String> = items
        .iter()
        .map(|(n, text)| {
            entry_title(text).unwrap_or_else(|| {
                panic!("docs/PROCESS.md §5 entry {n} has no backticked stage title: {text:?}")
            })
        })
        .collect();
    let mismatches: Vec<String> = (0..got.len().max(want.len()))
        .filter(|&i| got.get(i) != want.get(i))
        .map(|i| {
            format!(
                "  {}: doc {:?}\n      plan {:?}",
                i + 1,
                got.get(i),
                want.get(i)
            )
        })
        .collect();
    assert!(
        mismatches.is_empty(),
        "docs/PROCESS.md §5 lists {} stages, plan() prints {} for --fast; differing entries:\n{}",
        got.len(),
        want.len(),
        mismatches.join("\n")
    );
}

#[test]
fn every_ladder_count_in_the_process_doc_is_the_length_of_the_ladder() {
    let counts = ladder_counts(&ladder_section(&process_md()));
    let want = fast_titles().len();
    assert!(
        !counts.is_empty(),
        "§5 states no count for the ladder; if that is deliberate, delete this test with it"
    );
    let wrong: Vec<_> = counts.iter().filter(|(_, n)| *n != want).collect();
    assert!(
        wrong.is_empty(),
        "docs/PROCESS.md §5 counts the --fast ladder wrong (plan() prints {want}): {wrong:?}"
    );
}

#[test]
fn the_parsers_read_what_the_doc_writes() {
    let section = "## 5. The local gate ladder\n\
        a **19-stage ladder** and the twenty-nine stages\n\n\
        **The `--fast` ladder, in the order it prints.** blah\n\n\
        1. `build (--workspace)`\n\
        2. **`objc live-class audit (the registered\n    WinitWindowDelegate)`** — prose `other`\n\
        3. `x` — trailing\n   more prose\n\n\
        `--full` appends `y`\n";
    let s = ladder_section(section);
    let items = numbered_list(&s);
    assert_eq!(
        items
            .iter()
            .map(|(n, t)| (*n, entry_title(t).unwrap()))
            .collect::<Vec<_>>(),
        [
            (1, "build (--workspace)".to_string()),
            (
                2,
                "objc live-class audit (the registered WinitWindowDelegate)".to_string()
            ),
            (3, "x".to_string()),
        ]
    );
    assert_eq!(
        ladder_counts(&s),
        [
            ("19-stage".to_string(), 19),
            ("the twenty-nine stages".to_string(), 29)
        ]
    );
    assert_eq!(number_word("nineteen"), Some(19));
    assert_eq!(number_word("twenty"), Some(20));
    assert_eq!(number_word("thirty-one"), Some(31));
    assert_eq!(number_word("stages"), None);
}
