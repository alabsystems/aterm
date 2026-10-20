// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CORPUS'S SCREENS (plan 5B-2): every `see <word> [<detail>]
//! fixture=<name>` line of aterm-slog's `.seq` replay corpus
//! (`crates/aterm-slog/tests/sequences`, read from the directory: a file
//! added there is checked here) names a screen of `aterm-phase`'s fixtures
//! (main's set) that presence's one adapter ([`agent_verdict`], the mapping
//! the sweep publishes) reads, under the file's `program=`, to exactly the
//! line's word and detail (one token, as the replay reads it: a
//! corpus detail is a `ShortId`). So the verdicts a `.seq` replays through the
//! session log are ones a real screen gives.
//!
//! NEGATIVE CONTROLS: a line with another word, another detail, a detail
//! dropped, or another program each fail.

use super::*;

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../aterm-slog/tests/sequences");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../aterm-phase/src/fixtures");

/// One `see … fixture=` line.
#[derive(Clone, Debug)]
struct Seen {
    at: String,
    program: String,
    word: String,
    detail: Option<String>,
    fixture: String,
}

/// The `see … fixture=` lines of one `.seq` file's text.
fn seen(name: &str, text: &str) -> Vec<Seen> {
    let mut program = None;
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let mut l = raw.trim();
        if l.starts_with("seq ") {
            program = l
                .split_whitespace()
                .find_map(|t| t.strip_prefix("program="))
                .map(str::to_string);
            continue;
        }
        if let Some(rest) = l.strip_prefix("at ") {
            l = rest.split_once(' ').map_or("", |(_, r)| r.trim());
        }
        let toks: Vec<&str> = l.split_whitespace().collect();
        let (Some(&"see"), Some(fixture)) = (
            toks.first(),
            toks.iter().find_map(|t| t.strip_prefix("fixture=")),
        ) else {
            continue;
        };
        let rest: Vec<&str> = toks[1..]
            .iter()
            .filter(|t| !t.contains('='))
            .copied()
            .collect();
        out.push(Seen {
            at: format!("{name}:{}", i + 1),
            program: program.clone().unwrap_or_default(),
            word: rest.first().map(|w| (*w).to_string()).unwrap_or_default(),
            detail: rest.get(1).map(|d| (*d).to_string()),
            fixture: fixture.to_string(),
        });
    }
    out
}

/// What presence's adapter reads the line's screen as, against what the
/// line says.
fn check(s: &Seen) -> Result<(), String> {
    let path = format!("{FIXTURES}/{}.txt", s.fixture);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {path}: {e}", s.at))?;
    let rows = aterm_phase::prompt::fixtures::screen(&text);
    let v = agent_verdict(
        Some(&s.program),
        false,
        None,
        &rows,
        Cursor::Unknown,
        Instant::now(),
    );
    if (v.word(), v.detail()) != (s.word.as_str(), s.detail.clone()) {
        return Err(format!(
            "{}: {} reads as {} {:?}, the line says {} {:?}",
            s.at,
            s.fixture,
            v.word(),
            v.detail(),
            s.word,
            s.detail
        ));
    }
    Ok(())
}

/// Every `.seq` file of the corpus, by name.
fn corpus() -> Vec<(String, String)> {
    let mut all: Vec<(String, String)> = std::fs::read_dir(CORPUS)
        .expect("the corpus")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "seq"))
        .map(|p| {
            let name = p
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .into_owned();
            (name, std::fs::read_to_string(&p).expect("a corpus file"))
        })
        .collect();
    all.sort();
    all
}

/// Every screen the corpus names reads, through presence's mapping, as its
/// line says; the corpus names screens of each word it replays from one.
#[test]
fn every_corpus_screen_reads_as_its_line_says() {
    let all: Vec<Seen> = corpus().iter().flat_map(|(n, t)| seen(n, t)).collect();
    let errs: Vec<String> = all.iter().filter_map(|s| check(s).err()).collect();
    assert!(errs.is_empty(), "{}", errs.join("\n"));
    let words: std::collections::BTreeSet<&str> = all.iter().map(|s| s.word.as_str()).collect();
    for w in ["busy", "idle", "prompt", "question", "survey"] {
        assert!(words.contains(w), "no corpus screen reads {w}: {words:?}");
    }
    assert!(
        words.iter().any(|w| w.starts_with("wall:")),
        "no wall screen: {words:?}"
    );
}

/// NEGATIVE CONTROLS: a line changed in each way fails.
#[test]
fn a_corpus_line_with_the_wrong_word_fails() {
    let all: Vec<Seen> = corpus().iter().flat_map(|(n, t)| seen(n, t)).collect();
    let with_detail = all
        .iter()
        .find(|s| s.detail.is_some())
        .expect("a line with a detail");
    check(with_detail).expect("the shipping line");
    let bare = all
        .iter()
        .find(|s| s.detail.is_none())
        .expect("a line with none");
    check(bare).expect("the shipping line");
    let wrong = |f: &dyn Fn(&mut Seen), s: &Seen| {
        let mut x = s.clone();
        f(&mut x);
        check(&x).is_err()
    };
    let other = |w: &str| if w == "busy" { "idle" } else { "busy" }.to_string();
    assert!(wrong(&|s| s.word = other(&s.word), bare), "another word");
    assert!(
        wrong(&|s| s.word = other(&s.word), with_detail),
        "another word"
    );
    assert!(
        wrong(&|s| s.detail = Some("edit".into()), with_detail),
        "another detail"
    );
    assert!(wrong(&|s| s.detail = None, with_detail), "a detail dropped");
    assert!(
        wrong(&|s| s.detail = Some("x".into()), bare),
        "a detail added"
    );
    assert!(
        wrong(&|s| s.program = "zsh".into(), bare),
        "a shell is no agent"
    );
}
