// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn report(json: &str) -> Value {
    aterm_json::from_str(json).expect("test JSON parses")
}

#[test]
fn the_release_build_and_the_opt_test_build_name_their_profiles_and_the_harness() {
    assert_eq!(
        Build::Release.args(),
        [
            "--release",
            "-q",
            "-p",
            "aterm-bench",
            "--features",
            "aos-harness",
            "--example",
            "aos_harness"
        ]
    );
    assert_eq!(
        Build::OptTest.args(),
        [
            "--profile",
            "opt-test",
            "-q",
            "-p",
            "aterm-bench",
            "--features",
            "aos-harness",
            "--example",
            "aos_harness"
        ]
    );
    assert_eq!(Build::Release.dir(), "release");
    assert_eq!(Build::OptTest.dir(), "opt-test");
}

#[test]
fn reports_of_one_story_name_its_digest_and_another_story_is_refused() {
    let a = report(r#"{"story":{"digest":"00f0"}}"#);
    let b = report(r#"{"story":{"digest":"00f0"}}"#);
    assert_eq!(one_story(&[&a, &b]), Ok("00f0".to_string()));
    let other = report(r#"{"story":{"digest":"beef"}}"#);
    let why = one_story(&[&a, &other]).unwrap_err();
    assert!(why.contains("beef") && why.contains("not matched"), "{why}");
    let none = report(r#"{"story":{}}"#);
    assert_eq!(
        one_story(&[&a, &none]),
        Err("report 1 names no story digest".to_string())
    );
    assert_eq!(
        one_story(&[]),
        Err("a report names no story digest".to_string())
    );
}

#[test]
fn each_cold_number_is_summarized_by_its_median_min_and_max_over_the_runs() {
    let run = |ns: &str, ghz: f64| {
        (
            "replay",
            report(&format!(r#"{{"cold":{{"ns":{ns},"ghz":{ghz}}}}}"#)),
            String::new(),
        )
    };
    let cold = vec![
        run(r#"{"replay":30,"first":null}"#, 2.0),
        run(r#"{"replay":10}"#, 3.0),
        run(r#"{"replay":20}"#, 4.0),
        run(r#"{"replay":40}"#, 1.0),
    ];
    let rows = cold_rows(&cold);
    assert_eq!(
        rows,
        vec![
            ColdRow {
                op: "replay".into(),
                key: "replay".into(),
                median: 20.0,
                min: 10.0,
                max: 40.0,
                runs: 4,
            },
            ColdRow {
                op: "replay".into(),
                key: "ghz".into(),
                median: 2.0,
                min: 1.0,
                max: 4.0,
                runs: 4,
            },
        ]
    );
}

#[test]
fn every_process_must_reach_the_exported_head_and_the_first_that_does_not_is_named() {
    let export = report(r#"{"export":{"state":"aa","chain":"cc"}}"#);
    let import = |rs: &str, rc: &str, s: &str, c: &str| {
        report(&format!(
            r#"{{"import":{{"recorded_state":"{rs}","recorded_chain":"{rc}","state":"{s}","chain":"{c}"}}}}"#
        ))
    };
    let replay = |s: &str, c: &str| {
        (
            "replay",
            report(&format!(
                r#"{{"cold":{{"digests":{{"state":"{s}","chain":"{c}"}}}}}}"#
            )),
            String::new(),
        )
    };
    let checkpoint = |s: &str| {
        (
            "checkpoint",
            report(&format!(
                r#"{{"cold":{{"digests":{{"state":"{s}","checkpoint":"01"}}}}}}"#
            )),
            String::new(),
        )
    };
    let good = import("aa", "cc", "aa", "cc");
    let cold = vec![
        replay("aa", "cc"),
        checkpoint("aa"),
        replay("aa", "cc"),
        checkpoint("aa"),
    ];
    assert_eq!(
        cross_process(&export, &good, &cold),
        Ok(Head {
            state: "aa".into(),
            chain: "cc".into(),
            agreeing: 4,
        })
    );
    assert_eq!(
        cross_process(&export, &import("aa", "cc", "ab", "cc"), &cold),
        Err(
            "the import's replay reports state ab, not the exported head's aa: the processes disagree"
                .to_string()
        )
    );
    assert_eq!(
        cross_process(&export, &import("aa", "cd", "aa", "cc"), &cold),
        Err(
            "the file the import read reports chain cd, not the exported head's cc: the processes disagree"
                .to_string()
        )
    );
    let bent = vec![replay("aa", "cc"), checkpoint("aa"), replay("aa", "c0")];
    assert_eq!(
        cross_process(&export, &good, &bent),
        Err(
            "cold replay run 2 reports chain c0, not the exported head's cc: the processes disagree"
                .to_string()
        )
    );
    let bent = vec![replay("aa", "cc"), checkpoint("a0")];
    assert_eq!(
        cross_process(&export, &good, &bent),
        Err(
            "cold checkpoint run 1 reports state a0, not the exported head's aa: the processes disagree"
                .to_string()
        )
    );
    assert_eq!(
        cross_process(&export, &good, &[replay("aa", "cc")]),
        Err("no cold checkpoint run reports a digest".to_string())
    );
    assert_eq!(
        cross_process(&export, &report(r#"{"import":{}}"#), &cold),
        Err("the file the import read reports no state digest".to_string())
    );
    assert_eq!(
        cross_process(&report(r#"{"export":{"state":"aa"}}"#), &good, &cold),
        Err("the export reports no chain digest".to_string())
    );
    let no_digests = vec![(
        "replay",
        report(r#"{"cold":{"ns":{"replay":3}}}"#),
        String::new(),
    )];
    assert_eq!(
        cross_process(&export, &good, &no_digests),
        Err("cold replay run 1 reports no state digest".to_string())
    );
}

#[test]
fn a_missing_number_reads_as_a_question_mark_and_a_whole_one_has_no_decimals() {
    let v = report(r#"{"a":{"b":3,"c":2.375}}"#);
    assert_eq!(num(&v, "/a/b"), "3");
    assert_eq!(num(&v, "/a/c"), "2.4");
    assert_eq!(num(&v, "/a/d"), "?");
    assert_eq!(text(&v, "/a/b"), "?");
}
