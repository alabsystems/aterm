// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Replays the AT-SPI semantics oracle corpus against the recorded transcripts.

mod model {
    pub(crate) use crate::model::*;
}

mod sem {
    pub(crate) use crate::atspi::semantics::NodeIdentity as NodeId;
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
    let jobs = std::sync::Mutex::new(scenarios.into_iter().zip(&want).enumerate());
    let found = std::sync::Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let next = jobs
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .next();
                    let Some((index, (scenario, want))) = next else {
                        break;
                    };
                    if let Some(report) = difference(index, scenario, want) {
                        found
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push((index, report));
                    }
                }
            });
        }
    });
    let mut found = found
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    found.sort_by_key(|(index, _)| *index);
    failures.extend(found.into_iter().map(|(_, report)| report));
    failures
}

fn difference(index: usize, scenario: corpus::Scenario, want: &str) -> Option<String> {
    let got = transcript::play(scenario, false);
    let (line, g, w) = first_difference(&got, want)?;
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
    Some(format!(
        "{}: line {line}\n  got:  {g}\n  want: {w}\n{shown}",
        got.lines().next().unwrap_or("?")
    ))
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

fn within_a_minute<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(60))
        .expect("every blank-line question is answered")
}

fn answered_range(answer: &str) -> Option<(i32, i32)> {
    let inner = answer.strip_prefix('(')?.strip_suffix(')')?;
    let mut tail = inner.rsplitn(3, ", ");
    let end = tail.next()?.parse().ok()?;
    let start = tail.next()?.parse().ok()?;
    Some((start, end))
}

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
