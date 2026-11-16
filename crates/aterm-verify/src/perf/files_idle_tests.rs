// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A drive's output: libtest's lead-in, then the report line with every counter of both spans at
/// `value`, and the drive's shape `shape`.
fn output(shape: &[(&str, u64)], value: u64) -> String {
    let span = || {
        Value::Object(
            COUNTERS
                .iter()
                .map(|counter| ((*counter).to_owned(), Value::from(value)))
                .collect(),
        )
    };
    let mut report = aterm_json::Map::new();
    report.insert("schema".to_owned(), Value::from(1u64));
    for (key, n) in shape {
        report.insert((*key).to_owned(), Value::from(*n));
    }
    report.insert("first".to_owned(), span());
    report.insert("idle".to_owned(), span());
    format!(
        "running 1 test\ntest {TEST} ... {PREFIX}{}\nok\n",
        Value::Object(report)
    )
}

fn findings(run: &LaneRun) -> Vec<String> {
    match &run.status {
        Status::Fail(findings) => findings.clone(),
        other => panic!("expected a failure, got {other:?}"),
    }
}

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("files-idle-")
        .suffix(".noindex")
        .tempdir()
        .expect("a scratch directory")
}

/// The drive's line reads as one count per counter and span, keyed `<span>_<counter>`; a line
/// that lacks a counter, holds one this lane does not know, or comes from a drive of another
/// shape is refused rather than judged.
/// NEGATIVE CONTROL: with `parse_report`'s `SHAPE` check never refusing (`if got != Some(want)
/// && false`), the drive of 13 passes reads, and `` called `Result::unwrap_err()` on an `Ok`
/// value: [Count { key: "first_projections", value: 7 }, … ``.
#[test]
fn the_report_reads_every_count_and_refuses_another_drive() {
    let counts = parse_report(&output(&SHAPE, 7)).expect("reads");
    assert_eq!(counts.len(), SPANS.len() * COUNTERS.len());
    assert!(counts.iter().all(|c| c.value == 7));
    assert_eq!(counts[0].key, "first_projections");
    assert_eq!(counts[COUNTERS.len()].key, "idle_projections");

    let mut shorter = SHAPE;
    shorter[0].1 = 13;
    let why = parse_report(&output(&shorter, 7)).unwrap_err();
    assert!(why.contains("passes Some(13), not 61"), "{why}");

    let without = output(&SHAPE, 7).replace(",\"puts\":7", "");
    let why = parse_report(&without).unwrap_err();
    assert!(why.contains("no `puts`"), "{why}");
    let unknown = output(&SHAPE, 7).replace("\"puts\":7", "\"puts\":7,\"opens\":1");
    let why = parse_report(&unknown).unwrap_err();
    assert!(why.contains("`opens`"), "{why}");

    let twice = format!("{0}{0}", output(&SHAPE, 7));
    assert!(parse_report(&twice).is_err());
    assert!(parse_report("running 1 test\nok\n").is_err());
}

/// A count above its baseline fails, one equal or below passes, a count the baseline lacks fails,
/// a baseline count no longer reported fails, and with no baseline nothing is judged.
/// NEGATIVE CONTROL: with `judge`'s arm for a count above its baseline never taken
/// (`Some(base) if c.value > base && false`), that count passes as equal, and `` expected a
/// failure, got Pass `` fails the test.
#[test]
fn a_count_above_its_baseline_fails() {
    let count = |key: &str, value| Count {
        key: key.to_owned(),
        value,
    };
    let baseline: Value =
        aterm_json::from_str("{\"_comment\": \"\", \"idle_projections\": 12}").expect("parses");
    let failed = findings(&judge(&[count("idle_projections", 13)], Some(&baseline)));
    assert_eq!(failed, ["idle_projections 13 > its baseline 12"]);
    for held in [12, 0] {
        assert_eq!(
            judge(&[count("idle_projections", held)], Some(&baseline)).status,
            Status::Pass
        );
    }
    let failed = findings(&judge(
        &[count("idle_projections", 12), count("idle_puts", 1)],
        Some(&baseline),
    ));
    assert!(
        failed[0].contains("idle_puts 1 has no baseline"),
        "{failed:?}"
    );
    let failed = findings(&judge(&[], Some(&baseline)));
    assert!(failed[0].contains("holds idle_projections"), "{failed:?}");
    assert!(matches!(
        judge(&[count("idle_projections", 13)], None).status,
        Status::NotJudged(_)
    ));
}

/// A recording writes every count by key beside its comment, and the file it wrote judges the
/// same counts green and every count one higher red.
/// NEGATIVE CONTROL: with `judge`'s arm for a count above its baseline never taken (as above),
/// the higher counts pass, and `` expected a failure, got Pass `` fails the test.
#[test]
fn a_recording_judges_its_own_counts_green() {
    let dir = scratch();
    let path = dir.path().join(BASELINE_FILE);
    let counts = parse_report(&output(&SHAPE, 3)).expect("reads");
    let recorded = conclude_using(&counts, true, &path, super::super::publication::publish);
    assert_eq!(recorded.status, Status::Recorded(path.clone()));
    let written = read_baseline(&path).expect("reads").expect("written");
    assert_eq!(
        written
            .get("idle_carried_authenticated")
            .and_then(Value::as_u64),
        Some(3)
    );
    assert!(written.get("_comment").and_then(Value::as_str).is_some());
    assert_eq!(
        conclude_using(&counts, false, &path, super::super::publication::publish).status,
        Status::Pass
    );
    let more = parse_report(&output(&SHAPE, 4)).expect("reads");
    let failed = findings(&conclude_using(
        &more,
        false,
        &path,
        super::super::publication::publish,
    ));
    assert_eq!(failed.len(), SPANS.len() * COUNTERS.len(), "{failed:?}");
}
