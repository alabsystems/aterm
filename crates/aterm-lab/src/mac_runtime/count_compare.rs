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

/// The setup comparison uses the same exact rank calculation but never changes row kinds.
pub(super) fn settle_records(dir: &Path) -> Result<Value, Fail> {
    setup_records(
        dir,
        "measure-settle",
        "rewind::an_idle_recording_pane_adds_at_most_one_panes_budget_rss",
    )
}

pub(super) fn wait_records(dir: &Path) -> Result<Value, Fail> {
    let input = read_json(&dir.join("inputs.json"))?;
    let lane = match input["lane"].as_str() {
        Some("count-waits") => "count-waits",
        Some("count-waits-only") => "count-waits-only",
        _ => return Err(Fail::die("unknown Count-readiness experiment")),
    };
    setup_records(
        dir,
        lane,
        "rewind::twenty_busy_recording_panes_add_at_most_the_pool",
    )
}

fn setup_records(dir: &Path, lane: &str, planted_row: &str) -> Result<Value, Fail> {
    let dir = &std::fs::canonicalize(dir).map_err(|e| Fail::die(format!("settle record: {e}")))?;
    let log = |file: &str| -> Result<String, Fail> {
        let path = std::fs::canonicalize(dir.join(file))
            .map_err(|e| Fail::die(format!("settle log: {e}")))?;
        if path.parent() != Some(dir.as_path()) {
            return Err(Fail::die("settle log leaves its experiment"));
        }
        std::fs::read_to_string(path).map_err(|e| Fail::die(format!("settle log: {e}")))
    };
    let inputs = read_json(&dir.join("inputs.json"))?;
    let summary = read_json(&dir.join("rows.json"))?;
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
            return Err(Fail::die(format!("inconsistent settle evidence: {key}")));
        }
    }
    if summary["complete"].as_bool() != Some(true) || summary["lane"].as_str() != Some(lane) {
        return Err(Fail::die(
            "settle comparison needs the complete matched experiment",
        ));
    }
    let mut takes = Vec::new();
    let mut costs = Vec::new();
    let rows = summary["rows"]
        .as_array()
        .ok_or_else(|| Fail::die("missing settle takes"))?;
    for (i, row) in rows.iter().enumerate() {
        if row["take"].as_u64() != Some((i + 1) as u64)
            || row["exit"].as_i64() != Some(0)
            || row["success"].as_bool() != Some(true)
        {
            return Err(Fail::die("a settle take is missing or its driver failed"));
        }
        let mode = match row["settle_mode"].as_str() {
            Some("baseline") => "sequential",
            Some("candidate") => "together",
            _ => return Err(Fail::die("unknown settle mode")),
        };
        if "ABBAABBAAB".as_bytes().get(i).copied()
            != Some(if mode == "sequential" { b'A' } else { b'B' })
        {
            return Err(Fail::die(
                "settle takes differ from the declared ABBAABBAAB plan",
            ));
        }
        let file = format!("take-{}.verifier.log", i + 1);
        if row["artifacts"]["verifier_log"].as_str() != Some(file.as_str()) {
            return Err(Fail::die("settle log does not belong to its take"));
        }
        let mut text = log(&file)?;
        let parsed = parse(&text)?;
        // Direct bench drivers keep their phase clocks on stderr; the full verifier
        // captures both streams in its human log. Retain the same spans for either driver.
        if lane == "count-waits-only" {
            text.push('\n');
            text.push_str(&log(&format!("take-{}.stderr", i + 1))?);
        }
        let wall = super::measure_costs::read(&text)?;
        if wall["rows"]
            .as_object()
            .is_none_or(|rows| rows.keys().ne(parsed.keys()))
        {
            return Err(Fail::die("measure wall and verdict row sets differ"));
        }
        costs.push(json!({"take":i+1,"mode":row["settle_mode"],"wall":wall}));
        takes.push((mode.to_owned(), parsed));
    }
    for name in ["warmup", "warmup-candidate"] {
        let row = read_json(&dir.join(format!("{name}.json")))?;
        if row["exit"].as_i64() != Some(0) || row["success"].as_bool() != Some(true) {
            return Err(Fail::die("a settle warm-up driver failed"));
        }
        let warm = parse(&log(&format!("{name}.verifier.log"))?)?;
        if warm.values().any(|r| r.verdict != "PASS")
            || takes.first().is_none_or(|(_, t)| t.keys().ne(warm.keys()))
        {
            return Err(Fail::die("a settle warm-up row failed"));
        }
    }
    let control = read_json(&dir.join("negative-control.json"))?;
    let control_rows = parse(&log("negative-control.stdout")?)?;
    if control["expected_failure"].as_bool() != Some(true)
        || control_rows
            .get(planted_row)
            .is_none_or(|r| r.verdict != "FAIL")
    {
        return Err(Fail::die("the planted RSS regression was not caught"));
    }
    let mut report = compare(&takes)?;
    let mut whole = BTreeMap::new();
    for mode in ["baseline", "candidate"] {
        let selected: Vec<_> = rows
            .iter()
            .filter(|r| r["settle_mode"].as_str() == Some(mode))
            .collect();
        let wall: Vec<_> = selected
            .iter()
            .filter_map(|r| r["usage"]["real"].as_f64())
            .collect();
        let cpu: Vec<_> = selected
            .iter()
            .filter_map(|r| Some(r["usage"]["user"].as_f64()? + r["usage"]["sys"].as_f64()?))
            .collect();
        let wall = super::spread(&wall);
        let cpu = super::spread(&cpu);
        if wall.is_null() || cpu.is_null() {
            return Err(Fail::die("settle takes lack complete wall or CPU readings"));
        }
        whole.insert(mode.to_owned(), json!({"wall_s":wall,"cpu_s":cpu}));
    }
    let object = report.as_object_mut().expect("comparison object");
    for row in object
        .get_mut("rows")
        .expect("rows")
        .as_array_mut()
        .expect("comparison rows")
    {
        let row = row.as_object_mut().expect("comparison row");
        let a = row
            .remove("sequential_verdicts")
            .expect("baseline verdicts");
        let b = row.remove("together_verdicts").expect("candidate verdicts");
        row.insert("baseline_verdicts".into(), a);
        row.insert("candidate_verdicts".into(), b);
        row.remove("kind");
        row.remove("reason");
        for metric in row
            .get_mut("metrics")
            .expect("metrics")
            .as_object_mut()
            .expect("metrics object")
            .values_mut()
        {
            let metric = metric.as_object_mut().expect("metric object");
            let a = metric.remove("sequential").expect("baseline values");
            let b = metric.remove("together").expect("candidate values");
            metric.insert("baseline".into(), a);
            metric.insert("candidate".into(), b);
        }
    }
    object.insert("schema".into(), json!("aterm-settle-ranks-v1"));
    object.insert("inputs".into(), inputs);
    object.insert("wall_takes".into(), json!(costs));
    object.insert("whole_check".into(), json!(whole));
    Ok(report)
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
#[path = "count_compare_tests.rs"]
mod tests;
