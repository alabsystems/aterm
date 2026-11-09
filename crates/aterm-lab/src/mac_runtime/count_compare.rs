// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! B14's five-per-mode count comparison. No runtime threshold or verdict is changed here.
//! An exact two-sided permutation Mann–Whitney test enumerates all 252 assignments, retaining
//! ties. Failure to detect a shift is evidence from these ten takes, not proof of equivalence.

use std::collections::BTreeMap;
use std::path::Path;

use aterm_json::{Value, json};

use crate::Fail;

#[derive(Clone, Debug)]
pub(super) struct Row {
    verdict: String,
    values: BTreeMap<String, u64>,
    count: Option<bool>,
}

fn count_values(name: &str, verdict: &str, detail: &str) -> Result<BTreeMap<String, u64>, Fail> {
    let fields: &[&str] = if verdict != "PASS" {
        &[]
    } else if detail.starts_with("SPIN scene=") {
        &[
            "past_deadline_arms",
            "timer_wakes",
            "wakes",
            "frames",
            "needle_fires",
            "needle_frames_owed",
            "needle_episode_left",
        ]
    } else if detail.starts_with("REWIND idle_ms=") {
        &["rewind_wakes", "timer_wakes", "frames"]
    } else if detail.starts_with("COST idle_rss ") {
        &[
            "off_median_bytes",
            "on_median_bytes",
            "extra_bytes",
            "off_app_median_bytes",
            "on_app_median_bytes",
        ]
    } else if detail.starts_with("COST busy_rss ") {
        &["off_bytes", "on_bytes", "extra_bytes", "pool_used"]
    } else {
        &[]
    };
    let mut values = BTreeMap::new();
    for &key in fields {
        let word = detail
            .split_whitespace()
            .find_map(|w| w.strip_prefix(&format!("{key}=")))
            .ok_or_else(|| Fail::die(format!("{name} lacks {key}")))?;
        let word = word
            .strip_prefix("Some(")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(word);
        let n = word
            .parse::<u64>()
            .map_err(|_| Fail::die(format!("{name} has no numeric {key}")))?;
        values.insert(key.to_owned(), n);
    }
    if verdict == "PASS" && detail.starts_with("SPIN scene=") {
        let owners = detail
            .split_once(" owner_fires=")
            .and_then(|(_, s)| s.split_once(" max_owner_fires="))
            .ok_or_else(|| Fail::die(format!("{name} lacks owner counts")))?
            .0;
        for owner in owners.split("(\"").skip(1) {
            let (key, rest) = owner
                .split_once('\"')
                .ok_or_else(|| Fail::die("malformed owner count"))?;
            let n = rest
                .split_once("Some(")
                .and_then(|(_, s)| s.split_once(')'))
                .and_then(|(n, _)| n.parse::<u64>().ok())
                .ok_or_else(|| Fail::die(format!("{name} lacks {key} count")))?;
            values.insert(format!("owner_fires.{key}"), n);
        }
    }
    Ok(values)
}

fn matrix(name: &str) -> &str {
    name.split_once("::").map_or("", |(matrix, _)| matrix)
}

pub(super) fn parse(log: &str) -> Result<BTreeMap<String, Row>, Fail> {
    let mut rows = BTreeMap::new();
    let mut kinds = BTreeMap::new();
    let mut lines = log.lines();
    while let Some(line) = lines.next() {
        if let Some(header) = line.strip_prefix("measure kind: ") {
            let (name, kind) = header
                .split_once(' ')
                .ok_or_else(|| Fail::die("malformed measure kind"))?;
            let count = match kind {
                "Timing" => false,
                "Count { windowed: false }" | "Count { windowed: true }" => true,
                _ => return Err(Fail::die("unknown measure kind")),
            };
            if kinds.insert(name.to_owned(), count).is_some() {
                return Err(Fail::die("duplicate measure kind"));
            }
            continue;
        }
        let Some(header) = line.strip_prefix("measure row: ") else {
            continue;
        };
        let (name, verdict) = header
            .split_once(' ')
            .ok_or_else(|| Fail::die("malformed measure row"))?;
        let detail = lines
            .next()
            .ok_or_else(|| Fail::die("measure row has no detail"))?
            .trim();
        let values = count_values(name, verdict, detail)?;
        if rows
            .insert(
                name.to_owned(),
                Row {
                    verdict: verdict.to_owned(),
                    values,
                    count: kinds.get(name).copied(),
                },
            )
            .is_some()
        {
            return Err(Fail::die(format!("duplicate measure row {name}")));
        }
    }
    if rows.is_empty() {
        return Err(Fail::die("no measure rows in count comparison"));
    }
    if kinds.keys().any(|k| !rows.contains_key(k))
        || rows.keys().any(|name| {
            kinds.keys().any(|k| matrix(k) == matrix(name)) && !kinds.contains_key(name)
        })
    {
        return Err(Fail::die("measure kind and result row sets differ"));
    }
    for (name, row) in &mut rows {
        row.count = kinds.get(name).copied();
    }
    Ok(rows)
}

fn distance(values: &[u64], mask: u16) -> u32 {
    let mut twice_u = 0_u32;
    for (i, a) in values.iter().enumerate() {
        if mask & (1 << i) == 0 {
            continue;
        }
        for (j, b) in values.iter().enumerate() {
            if mask & (1 << j) != 0 {
                continue;
            }
            twice_u += match a.cmp(b) {
                std::cmp::Ordering::Less => 0,
                std::cmp::Ordering::Equal => 1,
                std::cmp::Ordering::Greater => 2,
            };
        }
    }
    twice_u.abs_diff(25)
}

fn rank(a: &[u64], b: &[u64]) -> Value {
    let values: Vec<_> = a.iter().chain(b).copied().collect();
    let observed = distance(&values, 0b11111);
    let extreme = (0_u16..1024)
        .filter(|m| m.count_ones() == 5 && distance(&values, *m) >= observed)
        .count();
    let p = f64::from(u16::try_from(extreme).expect("at most 252 assignments")) / 252.;
    json!({"sequential": a, "together": b, "extreme_assignments": extreme,
        "assignments": 252, "p_two_sided": p, "shift": p < 0.05})
}

pub(super) fn compare(takes: &[(String, BTreeMap<String, Row>)]) -> Result<Value, Fail> {
    let sequential: Vec<_> = takes
        .iter()
        .filter(|(m, _)| m == "sequential")
        .map(|(_, r)| r)
        .collect();
    let together: Vec<_> = takes
        .iter()
        .filter(|(m, _)| m == "together")
        .map(|(_, r)| r)
        .collect();
    if takes.len() != 10 || sequential.len() != 5 || together.len() != 5 {
        return Err(Fail::die(
            "count comparison needs five retained takes per mode",
        ));
    }
    let names: Vec<_> = sequential[0].keys().collect();
    if takes
        .iter()
        .any(|(_, rows)| rows.keys().collect::<Vec<_>>() != names)
    {
        return Err(Fail::die("count comparison row sets differ"));
    }
    let mut table = Vec::new();
    for name in names {
        let a: Vec<_> = sequential.iter().map(|r| &r[name]).collect();
        let b: Vec<_> = together.iter().map(|r| &r[name]).collect();
        let verdicts_a: Vec<_> = a.iter().map(|r| &r.verdict).collect();
        let verdicts_b: Vec<_> = b.iter().map(|r| &r.verdict).collect();
        let keys: Vec<_> = a[0].values.keys().collect();
        let complete = a
            .iter()
            .chain(&b)
            .all(|r| r.values.keys().collect::<Vec<_>>() == keys);
        let passed = a.iter().chain(&b).all(|r| r.verdict == "PASS");
        let declared_timing = a.iter().chain(&b).any(|r| r.count == Some(false));
        let mut metrics = BTreeMap::new();
        if complete {
            for key in keys {
                let av: Vec<_> = a.iter().map(|r| r.values[key]).collect();
                let bv: Vec<_> = b.iter().map(|r| r.values[key]).collect();
                metrics.insert(key.clone(), rank(&av, &bv));
            }
        }
        let shifted = metrics.values().any(|r| r["shift"].as_bool() == Some(true));
        let reason = if declared_timing {
            "declared Timing; solo values cannot requalify concurrent execution"
        } else if !passed {
            "not every retained verdict is PASS"
        } else if !complete {
            "missing count values"
        } else if metrics.is_empty() {
            "timing row or missing count values"
        } else if shifted {
            "count values shift by the exact rank test"
        } else {
            "five PASS verdicts per mode; no count metric shifts at p < 0.05"
        };
        table.push(json!({"row": name, "kind": if !declared_timing && complete && passed && !shifted && !metrics.is_empty() { "Count" } else { "Timing" },
            "reason": reason, "sequential_verdicts": verdicts_a, "together_verdicts": verdicts_b, "metrics": metrics}));
    }
    Ok(
        json!({"schema": "aterm-count-ranks-v1", "test": "exact two-sided permutation Mann-Whitney; ties retained; alpha=0.05 per metric", "rows": table}),
    )
}

fn read_json(path: &Path) -> Result<Value, Fail> {
    let bytes = std::fs::read(path).map_err(|e| Fail::die(format!("{}: {e}", path.display())))?;
    aterm_json::from_slice(&bytes).map_err(|e| Fail::die(format!("{}: {e}", path.display())))
}

type Takes = Vec<(String, BTreeMap<String, Row>)>;

fn retained(dir: &Path) -> Result<(Value, Takes), Fail> {
    let root =
        std::fs::canonicalize(dir).map_err(|e| Fail::die(format!("{}: {e}", dir.display())))?;
    let inputs = read_json(&root.join("inputs.json"))?;
    let summary = read_json(&root.join("rows.json"))?;
    for key in [
        "source_commit",
        "astream_commit",
        "bundle_sha256",
        "image",
        "lane",
        "guest",
        "driver",
    ] {
        if inputs[key].is_null() || inputs[key] != summary[key] {
            return Err(Fail::die(format!("inconsistent count evidence: {key}")));
        }
    }
    for key in ["count_mode", "count_policy"] {
        if inputs[key] != summary[key] {
            return Err(Fail::die(format!("inconsistent count evidence: {key}")));
        }
    }
    if summary["complete"].as_bool() != Some(true)
        || summary["all_exits_zero"].as_bool() != Some(true)
        || summary["exit"].as_i64() != Some(0)
        || !matches!(summary["lane"].as_str(), Some("measure" | "count-qualify"))
    {
        return Err(Fail::die(
            "count ranks need a complete successful count experiment, including warm-ups",
        ));
    }
    let rows = summary["rows"]
        .as_array()
        .ok_or_else(|| Fail::die("count experiment has no takes"))?;
    let mut takes = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        if row["exit"].as_i64() != Some(0) || row["success"].as_bool() != Some(true) {
            return Err(Fail::die("a count take's driver did not succeed"));
        }
        if row["take"].as_u64() != Some(u64::try_from(i + 1).expect("take index")) {
            return Err(Fail::die(
                "count takes are missing, reordered or duplicated",
            ));
        }
        let mode = row["count_mode"]
            .as_str()
            .or_else(|| {
                (summary["count_policy"].as_str() == Some("qualified")).then_some("together")
            })
            .filter(|m| matches!(*m, "sequential" | "together"))
            .ok_or_else(|| Fail::die("count take has no execution policy"))?;
        let name = format!("take-{}.verifier.log", i + 1);
        if row["artifacts"]["verifier_log"].as_str() != Some(name.as_str()) {
            return Err(Fail::die("count log does not belong to its take"));
        }
        let path = std::fs::canonicalize(root.join(name))
            .map_err(|e| Fail::die(format!("count log: {e}")))?;
        if path.parent() != Some(root.as_path()) {
            return Err(Fail::die("count log leaves its experiment"));
        }
        let log =
            std::fs::read_to_string(path).map_err(|e| Fail::die(format!("count log: {e}")))?;
        takes.push((mode.to_owned(), parse(&log)?));
    }
    Ok((inputs, takes))
}

/// Recompute historical ranks without executing the product. An optional
/// candidate is a later default-policy validation on the same bundle and guest shape.
pub(super) fn records(record: &Path, candidate: Option<&Path>) -> Result<Value, Fail> {
    let (inputs, mut takes) = retained(record)?;
    let mut sources = vec![inputs["source_commit"].clone()];
    if let Some(candidate) = candidate {
        let (other, newer) = retained(candidate)?;
        for key in [
            "bundle_sha256",
            "image",
            "lane",
            "guest",
            "driver",
            "compiler",
            "astream_commit",
        ] {
            if inputs[key] != other[key] {
                return Err(Fail::die(format!("count comparison changes {key}")));
            }
        }
        if newer.len() != 5 || newer.iter().any(|(m, _)| m != "together") {
            return Err(Fail::die(
                "candidate needs five retained default-policy takes",
            ));
        }
        takes.retain(|(m, _)| m == "sequential");
        takes.extend(newer);
        sources.push(other["source_commit"].clone());
    }
    let report = compare(&takes)?;
    Ok(
        json!({"schema": report["schema"], "test": report["test"], "source_commits": sources,
        "bundle_sha256": inputs["bundle_sha256"], "image": inputs["image"], "guest": inputs["guest"], "rows": report["rows"]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_planted_concurrency_shift_stays_timing_even_when_every_verdict_passes() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        use std::time::Duration;
        // No sleeps or latency assumptions: the partner stays until the row reads its value.
        // Its solo value is zero, its concurrent value one. Waits are minute hang detectors.
        let planted = |together| {
            let active = AtomicBool::new(false);
            if together {
                let (ready, wait_ready) = mpsc::channel();
                let (release, wait_release) = mpsc::channel();
                std::thread::scope(|scope| {
                    let active = &active;
                    scope.spawn(move || {
                        active.store(true, Ordering::SeqCst);
                        ready.send(()).unwrap();
                        wait_release.recv_timeout(Duration::from_secs(60)).unwrap();
                    });
                    wait_ready.recv_timeout(Duration::from_secs(60)).unwrap();
                    let value = u64::from(active.load(Ordering::SeqCst));
                    release.send(()).unwrap();
                    value
                })
            } else {
                u64::from(active.load(Ordering::SeqCst))
            }
        };
        let mut takes = Vec::new();
        for mode in ["sequential", "together"] {
            for _ in 0..5 {
                let n = planted(mode == "together");
                let log = format!(
                    "measure row: fixture::planted PASS\nREWIND idle_ms=10000 rewind_wakes={n} timer_wakes=0 frames=0\nmeasure row: fixture::stable PASS\nREWIND idle_ms=10000 rewind_wakes=0 timer_wakes=0 frames=0\n"
                );
                takes.push((mode.into(), parse(&log).unwrap()));
            }
        }
        let report = compare(&takes).unwrap();
        let rows = report["rows"].as_array().unwrap();
        assert_eq!(rows[0]["kind"].as_str(), Some("Timing"));
        assert_eq!(
            rows[0]["metrics"]["rewind_wakes"]["extreme_assignments"].as_u64(),
            Some(2)
        );
        assert_eq!(rows[1]["kind"].as_str(), Some("Count"));
        assert_eq!(
            rows[1]["metrics"]["rewind_wakes"]["p_two_sided"].as_f64(),
            Some(1.)
        );
        // Missing takes cannot qualify. A refused take cannot qualify even with equal values.
        assert!(compare(&takes[..9]).is_err());
        takes[0].1.get_mut("fixture::stable").unwrap().verdict = "COULD NOT RUN".into();
        assert_eq!(
            compare(&takes).unwrap()["rows"][1]["kind"].as_str(),
            Some("Timing")
        );
        takes[0].1.remove("fixture::stable");
        assert!(compare(&takes).is_err());
    }

    #[test]
    fn malformed_or_missing_values_never_become_zero() {
        assert!(parse("empty").is_err());
        assert!(
            parse(
                "measure row: x PASS\nREWIND idle_ms=1000 rewind_wakes=None frames=0 timer_wakes=0"
            )
            .is_err()
        );
        assert!(parse("measure row: x PASS\nREWIND idle_ms=1000 frames=0 timer_wakes=0").is_err());
        assert!(parse("measure row: x PASS\ntext\nmeasure row: x PASS\ntext").is_err());
    }

    #[test]
    fn solo_timing_values_never_requalify_the_row_for_concurrent_execution() {
        let log = "measure kind: x Timing\nmeasure row: x PASS\nREWIND idle_ms=10000 rewind_wakes=0 frames=0 timer_wakes=0\n";
        let takes: Vec<_> = (0..10)
            .map(|i| {
                (
                    if i < 5 { "sequential" } else { "together" }.into(),
                    parse(log).unwrap(),
                )
            })
            .collect();
        let report = compare(&takes).unwrap();
        assert_eq!(report["rows"][0]["kind"].as_str(), Some("Timing"));
        assert_eq!(
            report["rows"][0]["metrics"]["frames"]["p_two_sided"].as_f64(),
            Some(1.)
        );
        let mut takes = takes;
        takes[0].1 = parse("measure kind: x Count { windowed: false }\nmeasure row: x COULD NOT RUN\nSPIN COULD NOT RUN: no subject\n").unwrap();
        assert_eq!(
            compare(&takes).unwrap()["rows"][0]["kind"].as_str(),
            Some("Timing")
        );
        assert!(parse("measure kind: absent Timing\nmeasure row: x PASS\ntext").is_err());
        assert!(parse("measure kind: x unknown\nmeasure row: x PASS\ntext").is_err());
        assert!(parse("measure kind: spin::a Timing\nmeasure row: spin::a PASS\ntext\nmeasure row: spin::b PASS\ntext").is_err());
        assert!(parse("measure kind: spin::a Timing\nmeasure row: spin::a PASS\ntext\nmeasure row: paint::b PASS\ntext").is_ok());
        assert_eq!(
            parse("measure row: x PASS\ntext\nmeasure kind: x Timing\n").unwrap()["x"].count,
            Some(false)
        );
    }

    #[test]
    fn retained_evidence_requires_one_bundle_and_take_owned_logs() {
        let tmp = aterm_tempfile::Builder::new()
            .prefix("b14-rank-evidence-")
            .tempdir()
            .unwrap();
        let fixture = |name: &str, default: bool| {
            let dir = tmp.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            let mut inputs = json!({"source_commit": name, "astream_commit": "astream", "bundle_sha256": "one-bundle", "image": "one-image", "lane": "measure", "guest": {"cpus": 8}, "driver": "App", "compiler": "16387"});
            inputs.as_object_mut().unwrap().insert(
                if default {
                    "count_policy"
                } else {
                    "count_mode"
                }
                .into(),
                json!(if default { "qualified" } else { "interleaved" }),
            );
            let mut rows = Vec::new();
            for i in 1..=if default { 5 } else { 10 } {
                let log = format!("take-{i}.verifier.log");
                std::fs::write(dir.join(&log), "measure row: x PASS\nREWIND idle_ms=10000 rewind_wakes=0 frames=0 timer_wakes=0\n").unwrap();
                let mut row = json!({"take": i, "exit": 0, "success": true, "artifacts": {"verifier_log": log}});
                if !default {
                    row.as_object_mut().unwrap().insert(
                        "count_mode".into(),
                        json!(if i <= 5 { "sequential" } else { "together" }),
                    );
                }
                rows.push(row);
            }
            let mut summary = inputs.clone();
            summary
                .as_object_mut()
                .unwrap()
                .insert("complete".into(), json!(true));
            summary
                .as_object_mut()
                .unwrap()
                .insert("all_exits_zero".into(), json!(true));
            summary
                .as_object_mut()
                .unwrap()
                .insert("exit".into(), json!(0));
            summary
                .as_object_mut()
                .unwrap()
                .insert("rows".into(), json!(rows));
            std::fs::write(dir.join("inputs.json"), inputs.to_string()).unwrap();
            std::fs::write(dir.join("rows.json"), summary.to_string()).unwrap();
            dir
        };
        let controls = fixture("controls", false);
        let candidate = fixture("candidate", true);
        assert_eq!(
            records(&controls, None).unwrap()["rows"][0]["kind"].as_str(),
            Some("Count")
        );
        assert_eq!(
            records(&controls, Some(&candidate)).unwrap()["rows"][0]["kind"].as_str(),
            Some("Count")
        );
        let focused = fixture("focused", false);
        for lane in ["count-qualify", "unknown"] {
            for file in ["inputs.json", "rows.json"] {
                let path = focused.join(file);
                let mut record = read_json(&path).unwrap();
                record
                    .as_object_mut()
                    .unwrap()
                    .insert("lane".into(), json!(lane));
                std::fs::write(path, record.to_string()).unwrap();
            }
            if lane == "count-qualify" {
                assert_eq!(
                    records(&focused, None).unwrap()["rows"][0]["kind"].as_str(),
                    Some("Count")
                );
            } else {
                assert!(records(&focused, None).is_err());
            }
        }
        let failed = fixture("failed-driver", false);
        let path = failed.join("rows.json");
        let mut summary = read_json(&path).unwrap();
        summary
            .as_object_mut()
            .unwrap()
            .insert("all_exits_zero".into(), json!(false));
        std::fs::write(&path, summary.to_string()).unwrap();
        assert!(records(&failed, None).is_err());
        summary
            .as_object_mut()
            .unwrap()
            .insert("all_exits_zero".into(), json!(true));
        summary
            .as_object_mut()
            .unwrap()
            .get_mut("rows")
            .unwrap()
            .as_array_mut()
            .unwrap()[0]
            .as_object_mut()
            .unwrap()
            .insert("exit".into(), json!(1));
        std::fs::write(path, summary.to_string()).unwrap();
        assert!(records(&failed, None).is_err());
        let mut input = read_json(&candidate.join("inputs.json")).unwrap();
        input
            .as_object_mut()
            .unwrap()
            .insert("bundle_sha256".into(), json!("another-bundle"));
        std::fs::write(candidate.join("inputs.json"), input.to_string()).unwrap();
        // A mixed snapshot refuses even before the cross-experiment bundle check.
        assert!(records(&controls, Some(&candidate)).is_err());
        let mut summary = read_json(&candidate.join("rows.json")).unwrap();
        summary
            .as_object_mut()
            .unwrap()
            .insert("bundle_sha256".into(), json!("another-bundle"));
        std::fs::write(candidate.join("rows.json"), summary.to_string()).unwrap();
        assert!(records(&controls, Some(&candidate)).is_err());
        let mut summary = read_json(&controls.join("rows.json")).unwrap();
        std::fs::copy(
            controls.join("take-1.verifier.log"),
            tmp.path().join("outside"),
        )
        .unwrap();
        summary
            .as_object_mut()
            .unwrap()
            .get_mut("rows")
            .unwrap()
            .as_array_mut()
            .unwrap()[0]
            .as_object_mut()
            .unwrap()
            .get_mut("artifacts")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("verifier_log".into(), json!("../outside"));
        std::fs::write(controls.join("rows.json"), summary.to_string()).unwrap();
        assert!(records(&controls, None).is_err());
    }
}
