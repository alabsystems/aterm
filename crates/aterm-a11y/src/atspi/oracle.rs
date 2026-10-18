// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The semantics oracle's replay: the AT-SPI semantics the publisher serves
//! from, held to what registry accesskit_atspi_common 0.19.0 answered.
//!
//! `tests/recorder`, outside the workspace, played the corpus of
//! `tests/oracle/corpus.rs` on the registry crates and wrote
//! `tests/recorded/accesskit-0.24.1-transcript.txt`; this module plays the
//! same corpus through the same transcript code on the semantics compiled
//! here and compares, scenario by scenario (docs/AUDIT-third-party-2026-10-15.md,
//! unit a11y, A11Y.5). A difference names the scenario, the first line that
//! differs, and the scenario's full text up to it.
//!
//! The blank-line table, `tests/recorded/accesskit-0.24.1-blank-lines.txt`,
//! holds the text navigation the scenarios cannot: documents with empty runs
//! anywhere, on which the registry crates' paragraph search could loop
//! forever. Where they answered, the answer must be the same; where they
//! hung, there must be an answer, holding the offset.

/// The model the corpus builds.
mod model {
    pub(crate) use crate::model::*;
}

/// The semantics the transcript plays: this crate's.
mod sem {
    pub(crate) use crate::atspi::semantics::*;

    /// The application context's handle.
    pub(crate) type SharedApp = std::sync::Arc<SharedLock<AppContext>>;
}

#[path = "../../tests/oracle/corpus.rs"]
mod corpus;
#[path = "../../tests/oracle/transcript.rs"]
mod transcript;

use std::sync::mpsc;
use std::time::Duration;

use model::Role;
use sem::Granularity;

const RECORDING: &str = include_str!("../../tests/recorded/accesskit-0.24.1-transcript.txt");
const BLANK_LINES: &str = include_str!("../../tests/recorded/accesskit-0.24.1-blank-lines.txt");

/// The recording's scenarios: each `scenario` line and what follows it.
fn recorded() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in RECORDING.lines().filter(|l| !l.starts_with('#')) {
        if line.starts_with("scenario ") {
            out.push(String::new());
        }
        if let Some(last) = out.last_mut() {
            last.push_str(line);
            last.push('\n');
        }
    }
    out
}

/// The first line where `got` and `want` differ, with its number.
fn first_difference(got: &str, want: &str) -> Option<(usize, String, String)> {
    let mut got_lines = got.lines();
    let mut want_lines = want.lines();
    for n in 1.. {
        match (got_lines.next(), want_lines.next()) {
            (None, None) => return None,
            (g, w) if g == w => {}
            (g, w) => {
                return Some((
                    n,
                    g.unwrap_or("<end>").to_owned(),
                    w.unwrap_or("<end>").to_owned(),
                ));
            }
        }
    }
    None
}

/// The transcript of every scenario, compared with the recording; the
/// failures, each a report.
fn compare(scenarios: Vec<corpus::Scenario>) -> Vec<String> {
    let want = recorded();
    let mut failures = Vec::new();
    if scenarios.len() != want.len() {
        failures.push(format!(
            "{} scenarios played, {} recorded",
            scenarios.len(),
            want.len()
        ));
    }
    for (index, (scenario, want)) in scenarios.into_iter().zip(&want).enumerate() {
        let got = transcript::play(scenario, false);
        if let Some((line, g, w)) = first_difference(&got, want) {
            // Replay that scenario in full, for the text behind the digest.
            let full = corpus::scenarios()
                .into_iter()
                .nth(index)
                .map(|s| transcript::play(s, true))
                .unwrap_or_default();
            let step = got
                .lines()
                .take(line)
                .filter(|l| l.starts_with("step "))
                .count();
            let shown: String = full
                .lines()
                .take_while(|l| {
                    !l.starts_with("step ")
                        || l.split(' ')
                            .nth(1)
                            .and_then(|n| n.parse::<usize>().ok())
                            .is_some_and(|n| n < step)
                })
                .collect::<Vec<_>>()
                .join("\n");
            failures.push(format!(
                "{}: line {line}\n  got:  {g}\n  want: {w}\n{shown}",
                got.lines().next().unwrap_or("?")
            ));
        }
    }
    failures
}

#[test]
fn the_semantics_answer_as_the_registry_crates_did() {
    let failures = compare(corpus::scenarios());
    assert!(
        failures.is_empty(),
        "{} scenario(s) differ from the recording:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// The negative control: the recording is no rubber stamp. A changed word
/// boundary in one row of the first fixture, and a changed name in its tab
/// strip, each make that scenario, and only it, differ.
#[test]
fn a_changed_tree_is_caught() {
    let want = recorded();
    assert!(
        want.len() > 40 && RECORDING.lines().filter(|l| l.contains(" object ")).count() > 1000,
        "the recording holds the scenarios and their events"
    );
    for change in ["word starts", "tab name"] {
        let mut scenarios = corpus::scenarios();
        let corpus::Step::Open { update, .. } = &mut scenarios[0].steps[0] else {
            panic!("the first fixture opens a window")
        };
        let (_, node) = update
            .nodes
            .iter_mut()
            .find(|(_, n)| match change {
                "word starts" => n.role() == Role::TextRun,
                _ => n.role() == Role::Tab,
            })
            .expect("the grid has rows and tabs");
        match change {
            "word starts" => node.set_word_starts(vec![0u8, 1]),
            _ => node.set_label("renamed"),
        }
        let failures = compare(scenarios);
        assert_eq!(
            failures.len(),
            1,
            "{change}: exactly the changed scenario differs"
        );
        assert!(
            failures[0].starts_with("scenario fixture terminal:"),
            "{change}: {}",
            failures[0].lines().next().unwrap_or_default()
        );
    }
}

/// `f`'s result, on its own thread: the wait is a one-minute hang detector,
/// so a question that never returns fails the test.
fn within_a_minute<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(60))
        .expect("every blank-line question is answered")
}

/// The offsets a `GetStringAtOffset` answer, as the table writes it
/// (`("text", start, end)`), spans.
fn answered_range(answer: &str) -> Option<(i32, i32)> {
    let inner = answer.strip_prefix('(')?.strip_suffix(')')?;
    let mut tail = inner.rsplitn(3, ", ");
    let end = tail.next()?.parse().ok()?;
    let start = tail.next()?.parse().ok()?;
    Some((start, end))
}

/// The blank-line table asked of `answer`: each recorded answer must come
/// back the same, and where the registry crates hung, `answer` must give a
/// range that holds the offset. The failures, a row each.
fn compare_blank_lines(
    answer: impl Fn(&corpus::Document, i32, Granularity) -> String,
) -> Vec<String> {
    let rows: Vec<&str> = BLANK_LINES
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect();
    let mut failures = Vec::new();
    let mut asked = 0;
    for document in corpus::blank_line_documents() {
        for offset in 0..=document.len() {
            for g in transcript::BLANK_LINE_GRANULARITIES {
                let key = transcript::blank_line_key(&document, offset, g);
                let row = rows.get(asked).copied().unwrap_or("<end>");
                asked += 1;
                let Some(want) = row.strip_prefix(&key).and_then(|r| r.strip_prefix(' ')) else {
                    failures.push(format!("{key}: the recording has `{row}`"));
                    continue;
                };
                let got = answer(&document, offset, g);
                let held = if want == "hang" {
                    answered_range(&got)
                        .is_some_and(|(start, end)| start <= offset && offset <= end)
                } else {
                    got == want
                };
                if !held {
                    failures.push(format!("{key}: got {got}, recorded {want}"));
                }
            }
        }
    }
    if asked != rows.len() {
        failures.push(format!("{asked} rows asked, {} recorded", rows.len()));
    }
    failures
}

#[test]
fn blank_lines_answer_as_the_registry_crates_did_and_where_they_hung() {
    let failures = within_a_minute(|| compare_blank_lines(transcript::blank_line_answer));
    assert!(
        failures.is_empty(),
        "{} blank-line row(s) differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The blank-line table's negative control: it holds both kinds of row. A
/// changed answer where the registry crates answered is caught, and so is the
/// answer the first carve gave where they hung (the paragraph before a blank
/// line, which does not hold the offset).
#[test]
fn a_changed_blank_line_answer_is_caught() {
    let hung = BLANK_LINES.lines().filter(|l| l.ends_with(" hang")).count();
    assert!(
        hung > 50 && BLANK_LINES.lines().count() > 1000 + hung,
        "the table holds rows of both kinds"
    );
    let answered = "[\"a b\"] words @0 Word";
    let misplaced = "[\"x\\n\", \"\", \"a b\"] @3 Paragraph";
    for (key, recorded, wrong) in [
        (answered, "(\"a \", 0, 2)", "(\"a\", 0, 1)"),
        (misplaced, "hang", "(\"x\\n\", 0, 2)"),
    ] {
        assert!(
            BLANK_LINES
                .lines()
                .any(|l| l == format!("{key} {recorded}")),
            "{key} is recorded as {recorded}"
        );
        let failures = within_a_minute(move || {
            compare_blank_lines(|document, offset, g| {
                if transcript::blank_line_key(document, offset, g) == key {
                    wrong.to_owned()
                } else {
                    transcript::blank_line_answer(document, offset, g)
                }
            })
        });
        assert_eq!(failures.len(), 1, "{key}: exactly the changed row fails");
        assert!(failures[0].starts_with(key), "{}", failures[0]);
    }
}
