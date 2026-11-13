// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// One attach tally round in the shape [`attach_meta`] reads.
fn tally_round(sessions: u64, lane_meta: u64) -> Value {
    aterm_json::json!({
        "group": format!("attach, {sessions} session(s)"),
        "wait": {
            "format": WAIT_FORMATS[0],
            "rows": {
                "lane_sessions_status": {"count": 1},
                "lane_meta": {"count": lane_meta},
            },
        },
    })
}

/// The attach report shape for E05, with each population's aggregate meta count.
fn attach_report(meta_8: u64, meta_32: u64) -> Value {
    aterm_json::json!({
        "attach": {
            "tallies": {
                "rounds": [tally_round(8, meta_8), tally_round(32, meta_32)],
            },
        },
    })
}

/// The ping-pong report shape for D01, with each window's fleet-mail count.
fn pingpong_report(back_to_back: u64, paced: u64) -> Value {
    aterm_json::json!({
        "pingpong": {
            "handlers": {
                "rows": {
                    "fleet_mail": {"count": back_to_back},
                    "inbox": {"count": 1},
                },
            },
            "paced": {
                "handlers": {
                    "rows": {
                        "fleet_mail": {"count": paced},
                        "inbox": {"count": 1},
                    },
                },
            },
        },
    })
}

/// The messaging report shape for E06, with zero refusals in every window.
fn messaging_report(rate_limit: bool) -> Value {
    let rate_limit = if rate_limit { "on" } else { "off" };
    aterm_json::json!({
        "spec": format!("rate_limit={rate_limit}"),
        "messaging": {
            "latency": {"rate_limited": 0},
            "burst": {"rate_limited": 0},
            "paced_notes": {"rate_limited": 0},
        },
    })
}

/// The attach backlog shape for D10d at both populations' exact bounds.
fn cold_report() -> Value {
    aterm_json::json!({
        "attach": {
            "backlog": {
                "records": BACKLOG,
                "resident_budget": 16_u64 * 1024 * 1024,
                "sizes": [
                    {"sessions": 8, "cold_reads_per_restart": [15, 7, 7]},
                    {"sessions": 32, "cold_reads_per_restart": [63, 31, 31]},
                ],
            },
        },
    })
}

fn findings(run: &LaneRun) -> Vec<String> {
    match &run.status {
        Status::Fail(findings) => findings.clone(),
        other => panic!("expected a failure, got {other:?}"),
    }
}

fn value_of(counts: &[Count], key: &str) -> u64 {
    counts
        .iter()
        .find(|c| c.key == key)
        .unwrap_or_else(|| panic!("no {key} in {counts:?}"))
        .value
}

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("fabric-counts-")
        .suffix(".noindex")
        .tempdir()
        .expect("a scratch directory")
}

/// Every world's words are a run the bench takes, as the fabric lane checks its own.
#[test]
fn every_world_is_a_run_the_bench_takes() {
    for world in worlds() {
        let words = world.args.bench_words(world.args.plan()[0]);
        let spec = bench_spec::parse(&words, Path::new("/scratch"))
            .unwrap_or_else(|why| panic!("{}: {why}", world.name));
        assert!(spec.rate_limit, "{}: {words}", world.name);
        assert!(!spec.broker_process.outside(), "{}: {words}", world.name);
        assert_eq!(world.args.profile, Profile::Dev);
    }
}

/// E05's pre-change aggregate has 160 `@<sid> meta` calls at 8 sessions and 640 at 32. The
/// judge fails it on E05's law; the post-change zeroes hold it.
#[test]
fn the_judge_fails_e05s_a_arm_on_lane_meta() {
    let before = counts(&attach_report(160, 640)).expect("reads");
    assert_eq!(value_of(&before, "attach_8_lane_meta"), 160);
    assert_eq!(value_of(&before, "attach_32_lane_meta"), 640);
    let failed = findings(&judge(&before, None));
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert!(
        failed
            .iter()
            .all(|f| f.contains("lane_meta") && f.contains("E05")),
        "{failed:?}"
    );
    let after = counts(&attach_report(0, 0)).expect("reads");
    assert_eq!(after.len(), 2);
    assert_eq!(value_of(&after, "attach_32_lane_meta"), 0);
    assert_eq!(judge(&after, None).status, Status::Pass);
}

/// D01's pre-change aggregate has 1,201 fleet-mailbox transactions over the back-to-back
/// round trips and 301 over the paced ones. The judge fails it on D01's law; the post-change
/// zeroes hold it.
#[test]
fn the_judge_fails_d01s_a_arm_on_fleet_mail() {
    let before = counts(&pingpong_report(1201, 301)).expect("reads");
    assert_eq!(value_of(&before, "pingpong_fleet_mail"), 1201);
    assert_eq!(value_of(&before, "pingpong_paced_fleet_mail"), 301);
    let failed = findings(&judge(&before, None));
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert!(
        failed
            .iter()
            .all(|f| f.contains("fleet_mail") && f.contains("D01")),
        "{failed:?}"
    );
    let after = counts(&pingpong_report(0, 0)).expect("reads");
    assert_eq!(after.len(), 2);
    assert_eq!(judge(&after, None).status, Status::Pass);
}

/// E06's rate-limit-on shape holds with no refusal; writing one into its burst window makes
/// the judge fail it on E06's law. The rate-limit-off shape yields no rate count at all.
#[test]
fn the_judge_fails_a_rate_refusal_and_reads_none_without_the_limit() {
    let mut report = messaging_report(true);
    let committed = counts(&report).expect("reads");
    assert_eq!(committed.len(), 3);
    assert_eq!(value_of(&committed, "messaging_burst_rate_limited"), 0);
    assert_eq!(judge(&committed, None).status, Status::Pass);
    report
        .as_object_mut()
        .and_then(|r| r.get_mut("messaging"))
        .and_then(Value::as_object_mut)
        .and_then(|m| m.get_mut("burst"))
        .and_then(Value::as_object_mut)
        .expect("a burst window")
        .insert("rate_limited".to_string(), Value::from(1u64));
    let refused = counts(&report).expect("reads");
    let failed = findings(&judge(&refused, None));
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(
        failed[0].contains("messaging_burst_rate_limited") && failed[0].contains("E06"),
        "{failed:?}"
    );
    let unlimited = counts(&messaging_report(false)).expect("reads");
    assert!(
        unlimited.iter().all(|c| c.law != Law::RateLimited),
        "{unlimited:?}"
    );
}

/// D10d's exact-bound shape reads 15 then 7 at 8 sessions and 63 then 31 at 32. Writing one
/// more cold read into each 8-session regime makes the judge fail both D10d laws.
#[test]
fn the_judge_holds_d10ds_cold_reads_to_their_bounds() {
    let mut report = cold_report();
    let committed = counts(&report).expect("reads");
    for (key, value) in [
        ("restart_8_cold_reads_first", 15),
        ("restart_8_cold_reads_later", 7),
        ("restart_32_cold_reads_first", 63),
        ("restart_32_cold_reads_later", 31),
    ] {
        assert_eq!(value_of(&committed, key), value, "{key}");
    }
    assert_eq!(judge(&committed, None).status, Status::Pass);
    let sizes = report
        .as_object_mut()
        .and_then(|r| r.get_mut("attach"))
        .and_then(Value::as_object_mut)
        .and_then(|a| a.get_mut("backlog"))
        .and_then(Value::as_object_mut)
        .and_then(|b| b.get_mut("sizes"))
        .and_then(Value::as_array_mut)
        .expect("backlog sizes");
    let reads = sizes[0]
        .as_object_mut()
        .and_then(|s| s.get_mut("cold_reads_per_restart"))
        .and_then(Value::as_array_mut)
        .expect("8 sessions' cold reads");
    reads[0] = Value::from(16u64);
    reads[2] = Value::from(8u64);
    let over = counts(&report).expect("reads");
    let failed = findings(&judge(&over, None));
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert!(
        failed[0].contains("restart_8_cold_reads_first 16 > 15") && failed[0].contains("D10d"),
        "{failed:?}"
    );
    assert!(
        failed[1].contains("restart_8_cold_reads_later 8 > 7"),
        "{failed:?}"
    );
}

/// A count inside its law but above its baseline fails, one below passes, and a missing
/// baseline judges the laws alone and says so.
#[test]
fn the_baseline_ratchets_inside_the_law() {
    let count = |value| Count {
        key: "restart_8_cold_reads_first".to_string(),
        value,
        law: Law::ColdFirst,
        bound: 15,
    };
    let baseline: Value =
        aterm_json::from_str("{\"restart_8_cold_reads_first\": 8}").expect("parses");
    let failed = findings(&judge(&[count(9)], Some(&baseline)));
    assert!(failed[0].contains("> its baseline 8"), "{failed:?}");
    assert_eq!(judge(&[count(8)], Some(&baseline)).status, Status::Pass);
    assert_eq!(judge(&[count(7)], Some(&baseline)).status, Status::Pass);
    let alone = judge(&[count(15)], None);
    assert_eq!(alone.status, Status::Pass);
    assert_eq!(alone.unjudged.len(), 1, "{:?}", alone.unjudged);
}

/// A recording writes every count by key, refuses one past its law, and the file it wrote
/// judges the same counts green.
#[test]
fn a_recording_refuses_a_broken_law_and_judges_what_it_wrote() {
    let dir = scratch();
    let path = dir.path().join(BASELINE_FILE);
    let held = counts(&attach_report(0, 0)).expect("reads");
    let recorded = conclude_using(&held, true, &path, super::super::publication::publish);
    assert_eq!(recorded.status, Status::Recorded(path.clone()));
    let written = read_baseline(&path).expect("reads").expect("written");
    assert_eq!(
        written.get("attach_32_lane_meta").and_then(Value::as_u64),
        Some(0)
    );
    assert!(written.get("_comment").and_then(Value::as_str).is_some());
    assert_eq!(
        conclude_using(&held, false, &path, super::super::publication::publish).status,
        Status::Pass
    );
    let broken = counts(&attach_report(160, 640)).expect("reads");
    let refused = conclude_using(&broken, true, &path, super::super::publication::publish);
    assert!(
        findings(&refused)[0].starts_with("REFUSING to record"),
        "{refused:?}"
    );
    let still = read_baseline(&path).expect("reads").expect("kept");
    assert_eq!(
        still, written,
        "a refused recording leaves the baseline as it was"
    );
}

/// A tally round that counted no `sessions status` batch is not an attach the law is of: the
/// judge refuses to read it rather than pass it at 0.
#[test]
fn a_tally_round_with_no_status_batch_is_refused() {
    let mut report = attach_report(0, 0);
    let rows = report
        .as_object_mut()
        .and_then(|r| r.get_mut("attach"))
        .and_then(Value::as_object_mut)
        .and_then(|a| a.get_mut("tallies"))
        .and_then(Value::as_object_mut)
        .and_then(|t| t.get_mut("rounds"))
        .and_then(Value::as_array_mut)
        .and_then(|r| r.get_mut(0))
        .and_then(Value::as_object_mut)
        .and_then(|r| r.get_mut("wait"))
        .and_then(Value::as_object_mut)
        .and_then(|w| w.get_mut("rows"))
        .and_then(Value::as_object_mut)
        .expect("the first round's rows");
    rows.remove("lane_sessions_status");
    let why = counts(&report).unwrap_err();
    assert!(why.contains("sessions status"), "{why}");
}

/// A valid E05-shaped report with its first tally round's `wait.format` set to `format`
/// (`None` removes it).
fn with_wait_format(format: Option<&str>) -> Value {
    let mut report = attach_report(0, 0);
    let wait = report
        .as_object_mut()
        .and_then(|r| r.get_mut("attach"))
        .and_then(Value::as_object_mut)
        .and_then(|a| a.get_mut("tallies"))
        .and_then(Value::as_object_mut)
        .and_then(|t| t.get_mut("rounds"))
        .and_then(Value::as_array_mut)
        .and_then(|r| r.get_mut(0))
        .and_then(Value::as_object_mut)
        .and_then(|r| r.get_mut("wait"))
        .and_then(Value::as_object_mut)
        .expect("the first round's wait tally");
    match format {
        Some(f) => {
            wait.insert("format".to_string(), Value::String(f.to_string()));
        }
        None => {
            wait.remove("format");
        }
    }
    report
}

/// A tally round in a wait-tally format the lane was not written against is refused, and so
/// is one that names none: the report leaves out every row that counted nothing, so a row
/// renamed or split in a later format would read as absent and pass E05's law at 0.
#[test]
fn a_tally_round_in_an_unknown_wait_format_is_refused() {
    let why = counts(&with_wait_format(Some("wait-tally v=9"))).unwrap_err();
    assert!(why.contains("`wait-tally v=9`"), "{why}");
    let why = counts(&with_wait_format(None)).unwrap_err();
    assert!(why.contains("names no wait-tally format"), "{why}");
    for format in WAIT_FORMATS {
        assert!(counts(&with_wait_format(Some(format))).is_ok(), "{format}");
    }
}

/// The committed baseline holds every key the lane's worlds report and no other, each inside
/// its law.
#[test]
fn the_committed_baseline_holds_every_expected_key_inside_its_law() {
    let path = super::super::floors::golden_dir().join(BASELINE_FILE);
    let baseline = read_baseline(&path).expect("reads").expect("committed");
    let mut keys: Vec<&String> = baseline
        .as_object()
        .expect("an object")
        .keys()
        .filter(|k| *k != "_comment")
        .collect();
    keys.sort();
    let mut want = expected();
    want.sort();
    assert_eq!(keys, want.iter().collect::<Vec<_>>());
    for n in ATTACH {
        let n = u64::from(n);
        let at = |key: String| baseline.get(&key).and_then(Value::as_u64).expect("a count");
        assert_eq!(at(format!("attach_{n}_lane_meta")), 0);
        assert!(at(format!("restart_{n}_cold_reads_first")) < 2 * n);
        assert!(at(format!("restart_{n}_cold_reads_later")) < n);
    }
}

/// The backlog outgrows a 16 MiB window, so the restarts read the seeded notes cold; a
/// report whose backlog fits its window is refused.
#[test]
fn the_attach_world_reaches_the_cold_path() {
    assert!(u64::from(BACKLOG) * bench_spec::FILL_RECORD_BYTES as u64 > 16 * 1024 * 1024);
    assert_eq!(cold_path_reached(&cold_report()), Ok(()));
    let small: Value = aterm_json::from_str(
        "{\"attach\": {\"backlog\": {\"records\": 1, \"resident_budget\": 16777216}}}",
    )
    .expect("parses");
    assert!(cold_path_reached(&small).unwrap_err().contains("fits"));
}

#[test]
fn spec_words_and_groups_read_as_the_bench_writes_them() {
    let report: Value =
        aterm_json::from_str("{\"spec\": \"samples=200 rate_limit=on tally=on\"}").expect("parses");
    assert_eq!(spec_word(&report, "rate_limit"), Some("on"));
    assert_eq!(spec_word(&report, "rate"), None);
    assert_eq!(spec_word(&report, "broker_process"), None);
    assert_eq!(attach_sessions("attach, 32 session(s)"), Some(32));
    assert_eq!(attach_sessions("handoff, 2 session(s)"), None);
}
