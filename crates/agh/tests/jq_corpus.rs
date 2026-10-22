// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The jq corpus (the plan's §9.4): the 34 transcript programs, aterm's 14, one refused row
//! per refused construct and the edge rows, each judged against the output jq 1.7.1 printed
//! when the corpus was recorded by hand (`jq -r -c -S`). A row marked `jq_differs` is one of
//! the divergences `agh help jq` lists: its output is what jq 1.7.1 printed for the rewrite its
//! note names, the program that gives agh's answer in jq.

use std::path::Path;

use agh::Kind;
use agh::cli::jq::{compile, render_results};
use aterm_json::Value;

/// The corpus, as recorded.
const CORPUS: &str = include_str!("fixtures/jq/corpus.json");

fn corpus() -> Value {
    aterm_json::from_str(CORPUS).expect("corpus.json is JSON")
}

fn rows() -> Vec<Value> {
    corpus()
        .get("rows")
        .and_then(Value::as_array)
        .cloned()
        .expect("the corpus has rows")
}

fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{row}: no string {key}"))
}

fn input(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/jq/inputs")
        .join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    aterm_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// One row's verdict: `None` when it holds.
fn judge(row: &Value) -> Option<String> {
    let name = text(row, "name");
    let program = text(row, "program");
    if let Some(construct) = row.get("refused") {
        let construct = construct.as_str().unwrap_or_default();
        return match compile(program) {
            Ok(_) => Some(format!("{name}: compiled, but {construct} is refused")),
            Err(e) if e.kind != Kind::Usage => Some(format!("{name}: {:?}, not usage", e.kind)),
            Err(e) if !e.why.contains(construct) => {
                Some(format!("{name}: {} does not name {construct}", e.why))
            }
            Err(_) => None,
        };
    }
    let value = input(text(row, "input"));
    if row.get("fails").and_then(Value::as_bool) == Some(true) {
        // A `fails` row must compile and then fail as it runs: a typo never passes for one.
        let compiled = match compile(program) {
            Ok(compiled) => compiled,
            Err(e) => {
                return Some(format!(
                    "{name}: does not compile ({}), but a fails row must run",
                    e.why
                ));
            }
        };
        return match compiled.run(&value) {
            Ok(results) => Some(format!("{name}: ran to {results:?}, but it fails")),
            Err(e) if e.kind != Kind::Usage => Some(format!("{name}: {:?}, not usage", e.kind)),
            Err(_) => None,
        };
    }
    let ran = compile(program).and_then(|p| p.run(&value));
    let expected = text(row, "output");
    match ran {
        Ok(results) => {
            let got = render_results(&results);
            (got != expected)
                .then(|| format!("{name}: {got:?} where the corpus recorded {expected:?}"))
        }
        Err(e) => Some(format!("{name}: failed: {}", e.why)),
    }
}

#[test]
fn the_corpus_names_its_recording() {
    let corpus = corpus();
    assert_eq!(text(&corpus, "recorded_with"), "jq-1.7.1-apple");
    assert_eq!(text(&corpus, "flags"), "-r -c -S");
}

#[test]
fn every_row_holds() {
    let failures: Vec<String> = rows().iter().filter_map(judge).collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_row_has_a_name_and_one_expectation() {
    let mut names = Vec::new();
    for row in rows() {
        let name = text(&row, "name").to_string();
        let shapes = ["output", "refused", "fails"]
            .into_iter()
            .filter(|key| row.get(key).is_some())
            .count();
        assert_eq!(shapes, 1, "{name}: one of output, refused, fails");
        assert!(!names.contains(&name), "{name} twice");
        names.push(name);
    }
}

#[test]
fn a_differs_note_says_how() {
    for row in rows() {
        for key in ["gojq_differs", "jq_differs"] {
            if let Some(note) = row.get(key) {
                let note = note.as_str().unwrap_or_default();
                assert!(
                    !note.trim().is_empty(),
                    "{}: an empty {key}",
                    text(&row, "name")
                );
            }
        }
    }
}

#[test]
fn the_transcript_and_aterm_programs_are_all_here() {
    let rows = rows();
    let count = |prefix: &str| {
        rows.iter()
            .filter(|row| text(row, "name").starts_with(prefix) && row.get("output").is_some())
            .count()
    };
    assert_eq!(count("t"), 34, "the transcript's programs");
    assert_eq!(count("a"), 14, "aterm's own programs");
}

#[test]
fn a_bad_regex_is_usage_naming_it() {
    let program = compile(".tag_name | test(\"(\")").expect("compiles");
    let e = program
        .run(&input("release.json"))
        .expect_err("an unbalanced group does not compile");
    assert_eq!(e.kind, Kind::Usage);
    assert!(e.why.contains("\"(\""), "{}", e.why);
}

#[test]
fn a_runtime_error_shows_a_long_input_cut_short() {
    // 301 symbols: one past a whole quad, so @base64d fails too.
    let long = Value::from("a".repeat(301));
    for program in ["tonumber", "keys", "@base64d"] {
        let e = compile(program)
            .expect("compiles")
            .run(&long)
            .expect_err("a long string of a's fails");
        assert_eq!(e.kind, Kind::Usage, "{program}");
        assert!(
            e.why.chars().count() < 100,
            "{program}: {} chars",
            e.why.chars().count()
        );
    }
}

#[test]
fn a_stray_byte_names_its_offset() {
    let e = compile(".a * 2").expect_err("* is not in the subset");
    assert_eq!(e.kind, Kind::Usage);
    assert!(e.why.contains('*') && e.why.contains("byte 3"), "{}", e.why);
}

#[test]
fn a_refusal_carries_its_rewrite() {
    let e = compile("$ENV.HOME").expect_err("refused");
    assert!(e.why.contains("$ENV"), "{}", e.why);
    assert_ne!(
        e.next, "agh help jq",
        "a refused row carries its own rewrite"
    );
    let e = compile("if . then 1 else 2 end").expect_err("refused");
    assert_eq!(e.next, "agh help jq");
}

#[test]
fn render_results_is_jq_dash_r() {
    let results = [
        Value::from("raw"),
        aterm_json::json!({"b": 1, "a": [true, null]}),
        Value::from(7u64),
    ];
    assert_eq!(
        render_results(&results),
        "raw\n{\"a\":[true,null],\"b\":1}\n7\n"
    );
    assert_eq!(render_results(&[]), "");
}

#[test]
fn a_program_too_deep_is_usage_never_an_abort() {
    let n = 100_000;
    let programs = [
        "(".repeat(n),
        format!("{}1{}", "(".repeat(n), ")".repeat(n)),
        ".a".repeat(n),
        format!("1{}", " + 1".repeat(n)),
        format!("[{}.a]", ".a, ".repeat(n)),
        "[".repeat(n),
        "{a:".repeat(n),
        "\"\\(".repeat(n),
        format!("{}1", "1 // ".repeat(n)),
        format!("{}.", ". | ".repeat(n)),
        "first(".repeat(n),
        format!("{}.", ". as $x | ".repeat(n)),
        format!(".{}", "[]?".repeat(n)),
        format!("select({}true)", ".a == 1 or ".repeat(n)),
    ];
    for program in &programs {
        let e = compile(program).expect_err("a program this deep is refused");
        assert_eq!(e.kind, Kind::Usage, "{}", &program[..24]);
    }
}

#[test]
fn a_program_just_inside_the_depth_limit_runs() {
    let n = 120;
    let value = input("release.json");
    let cases = [
        (format!("{}1{}", "[".repeat(n), "]".repeat(n)), None),
        (
            format!("{}1{}", "first(".repeat(n), ")".repeat(n)),
            Some("1\n"),
        ),
        (format!("{}1{}", "(".repeat(n), ")".repeat(n)), Some("1\n")),
        (format!(".author{}", ".x?".repeat(n / 2)), Some("null\n")),
        (format!("1{}", " + 1".repeat(n)), Some("121\n")),
        (
            format!("{}1{}", "\"\\(".repeat(n / 2), ")\"".repeat(n / 2)),
            Some("1\n"),
        ),
    ];
    for (program, want) in &cases {
        let results = compile(program)
            .and_then(|p| p.run(&value))
            .unwrap_or_else(|e| panic!("{}: {}", &program[..24], e.why));
        if let Some(want) = want {
            assert_eq!(render_results(&results), *want, "{}", &program[..24]);
        }
    }
}

#[test]
fn a_test_that_outruns_its_step_budget_fails_rather_than_guess() {
    // jq 1.7.1 answers true for both; aterm-regex decides the short one and runs out of
    // steps on the long one, which is usage, never a silent false.
    let program = compile("test(\"(?:x?){2000}z\")").expect("compiles");
    let short = Value::from(format!("{}z", "x".repeat(1000)));
    let results = program.run(&short).expect("decided within the budget");
    assert_eq!(render_results(&results), "true\n");
    let long = Value::from(format!("{}z", "x".repeat(20_000)));
    let e = program
        .run(&long)
        .expect_err("a search past the budget is an error");
    assert_eq!(e.kind, Kind::Usage);
    assert!(e.why.contains("(?:x?){2000}z"), "{}", e.why);
}
