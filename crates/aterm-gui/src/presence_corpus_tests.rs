// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies that every `see <word> [<detail>] fixture=<name>` line in
//! `aterm-slog`'s `.seq` corpus matches [`agent_verdict`] on the named
//! `aterm-phase` fixture, and that every `key=<k> [chose=<c>]` an input line
//! carries is what the frame of the `see` line before it chooses for that key
//! (fix E8, [`crate::session_log::FrameKeys`]).

use super::*;

/// aterm-slog's sequence corpus.
fn corpus_dir() -> std::path::PathBuf {
    aterm_tempfile::manifest_dir().join("../aterm-slog/tests/sequences")
}

/// aterm-phase's recorded screens.
fn fixtures_dir() -> std::path::PathBuf {
    aterm_tempfile::manifest_dir().join("../aterm-phase/src/fixtures")
}

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
    let path = fixtures_dir().join(format!("{}.txt", s.fixture));
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("{}: {}: {e}", s.at, path.display()))?;
    let rows = aterm_phase::prompt::fixtures::screen(&text);
    let v = agent_verdict(
        Some(&s.program),
        false,
        None,
        &rows,
        Cursor::Unknown,
        "",
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
    let mut all: Vec<(String, String)> = std::fs::read_dir(corpus_dir())
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

/// One input line's `key=` and `chose=` (`person …` or `admit …`), with the
/// fixture of the `see` line before it (`None`: it named none).
#[derive(Clone, Debug)]
struct Keyed {
    at: String,
    key: String,
    chose: Option<String>,
    fixture: Option<String>,
}

/// The keyed input lines of one `.seq` file's text.
fn keyed(name: &str, text: &str) -> Vec<Keyed> {
    let mut fixture = None;
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let mut l = raw.trim();
        if let Some(rest) = l.strip_prefix("at ") {
            l = rest.split_once(' ').map_or("", |(_, r)| r.trim());
        }
        let toks: Vec<&str> = l.split_whitespace().collect();
        let field = |k: &str| {
            toks.iter()
                .find_map(|t| t.strip_prefix(k))
                .map(str::to_string)
        };
        match toks.first() {
            Some(&"see") => fixture = field("fixture="),
            Some(&("person" | "admit")) => {
                if let Some(key) = field("key=") {
                    out.push(Keyed {
                        at: format!("{name}:{}", i + 1),
                        key,
                        chose: field("chose="),
                        fixture: fixture.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// The class of a `.seq` file's `key=` word (`aterm-slog`'s
/// `seq_replay::KEYS`).
fn class_of(word: &str) -> Option<crate::session_log::KeyClass> {
    use crate::session_log::KeyClass;
    match word {
        "enter" => Some(KeyClass::Enter),
        "esc" => Some(KeyClass::Esc),
        "down" | "text" => Some(KeyClass::Other),
        d => d
            .parse::<u8>()
            .ok()
            .filter(|n| (1..=9).contains(n))
            .map(KeyClass::Digit),
    }
}

/// What the frame of the line's fixture chooses for its key, against what
/// the line says.
fn check_key(k: &Keyed) -> Result<(), String> {
    let fixture = k
        .fixture
        .as_deref()
        .ok_or_else(|| format!("{}: a keyed input after a `see` with no fixture=", k.at))?;
    let class = class_of(&k.key).ok_or_else(|| format!("{}: key={}", k.at, k.key))?;
    let got = crate::session_log::keys::tests::frame(fixture)
        .and_then(|f| f.chose(class))
        .map(aterm_slog::Chose::word);
    if got != k.chose.as_deref() {
        return Err(format!(
            "{}: key={} on {fixture} chooses {got:?}, the line says {:?}",
            k.at, k.key, k.chose
        ));
    }
    Ok(())
}

/// Every keyed input of the corpus chooses, on the screen its frame names,
/// what its line says; the corpus keys each choice and an unplaced key.
#[test]
fn every_corpus_key_chooses_what_its_line_says() {
    let all: Vec<Keyed> = corpus().iter().flat_map(|(n, t)| keyed(n, t)).collect();
    let errs: Vec<String> = all.iter().filter_map(|k| check_key(k).err()).collect();
    assert!(errs.is_empty(), "{}", errs.join("\n"));
    let chosen: std::collections::BTreeSet<Option<&str>> =
        all.iter().map(|k| k.chose.as_deref()).collect();
    for c in [Some("approve"), Some("decline"), Some("answer"), None] {
        assert!(
            chosen.contains(&c),
            "no corpus key chooses {c:?}: {chosen:?}"
        );
    }
}

/// NEGATIVE CONTROLS: a keyed line changed in each way fails.
#[test]
fn a_corpus_key_with_the_wrong_choice_fails() {
    let all: Vec<Keyed> = corpus().iter().flat_map(|(n, t)| keyed(n, t)).collect();
    let chosen = all
        .iter()
        .find(|k| k.chose.as_deref() == Some("approve"))
        .expect("an approving key");
    check_key(chosen).expect("the shipping line");
    let wrong = |f: &dyn Fn(&mut Keyed)| {
        let mut x = chosen.clone();
        f(&mut x);
        check_key(&x).is_err()
    };
    assert!(
        wrong(&|k| k.chose = Some("decline".into())),
        "another choice"
    );
    assert!(wrong(&|k| k.chose = None), "a choice dropped");
    assert!(
        wrong(&|k| k.key = "down".into()),
        "a key that chooses nothing"
    );
    assert!(wrong(&|k| k.fixture = None), "no screen behind it");
    assert!(
        wrong(&|k| k.fixture = Some("claude-2.1.282-box-tool-default-no".into())),
        "Enter on a focused No"
    );
}
