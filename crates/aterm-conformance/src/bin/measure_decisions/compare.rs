// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Offline analysis of the predeclared ABBAABBAAB experiment; no product execution.

use aterm_json::{Value, json};
use std::path::Path;

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn metric(log: &str, key: &str) -> Result<u64, String> {
    let lines: Vec<_> = log
        .lines()
        .filter(|l| l.starts_with("COST idle_rss "))
        .collect();
    let [line] = lines.as_slice() else {
        return Err("expected one idle RSS report".into());
    };
    line.split_whitespace()
        .find_map(|word| word.strip_prefix(&format!("{key}=")))
        .ok_or_else(|| format!("missing {key}"))?
        .parse()
        .map_err(|e| format!("{key}: {e}"))
}

fn passed(log: &str) -> bool {
    log.lines().any(|line| {
        line == "measure row: rewind::an_idle_recording_pane_adds_at_most_one_panes_budget_rss PASS"
    })
}

/// Two-sided exact rank permutation, retaining ties and all 252 balanced assignments.
fn ranks(values: &[u64; 10]) -> (usize, usize) {
    const A: [usize; 5] = [0, 3, 4, 7, 8];
    let ranks = values.map(|value| {
        2 * values.iter().filter(|&&v| v < value).count()
            + values.iter().filter(|&&v| v == value).count()
            + 1
    });
    let total: usize = ranks.iter().sum();
    let observed = (2 * A.iter().map(|&i| ranks[i]).sum::<usize>()).abs_diff(total);
    let mut extreme = 0;
    let mut assignments = 0;
    for mask in 0_u16..1024 {
        if mask.count_ones() != 5 {
            continue;
        }
        assignments += 1;
        let sum: usize = ranks
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, r)| r)
            .sum();
        extreme += usize::from((2 * sum).abs_diff(total) >= observed);
    }
    (extreme, assignments)
}

fn arms(values: &[f64]) -> Value {
    let take = |indices: [usize; 5]| {
        let mut v = indices.map(|i| values[i]);
        v.sort_by(f64::total_cmp);
        json!({"min":v[0],"median":v[2],"max":v[4]})
    };
    json!({"A":take([0,3,4,7,8]),"B":take([1,2,5,6,9])})
}

pub(super) fn run(dir: &Path) -> Result<Value, String> {
    let data: Value =
        aterm_json::from_str(&read(&dir.join("rows.json"))?).map_err(|e| e.to_string())?;
    let rows = data["rows"]
        .as_array()
        .filter(|v| v.len() == 10)
        .ok_or("experiment requires ten takes")?;
    if data["all_exits_zero"].as_bool() != Some(true) {
        return Err("experiment did not finish successfully".into());
    }
    for name in ["warmup.stdout", "warmup-fixed.stdout"] {
        if !passed(&read(&dir.join(name))?) {
            return Err(format!("{name} did not pass"));
        }
    }
    let mut logs = Vec::new();
    let (mut walls, mut cpus, mut row_walls, mut stops, mut loads) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (i, row) in rows.iter().enumerate() {
        let arm = if [0, 3, 4, 7, 8].contains(&i) {
            "A"
        } else {
            "B"
        };
        if row["take"].as_u64() != Some(i as u64 + 1)
            || row["arm"].as_str() != Some(arm)
            || row["exit"].as_u64() != Some(0)
        {
            return Err("take identity, arm or exit differs from the declaration".into());
        }
        let log = read(&dir.join(format!("take-{}.stdout", i + 1)))?;
        if !passed(&log) {
            return Err("a measured row did not pass".into());
        }
        let positive = |v: &Value| {
            v.as_f64()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or_else(|| "missing or invalid timing/load".to_owned())
        };
        walls.push(positive(&row["usage"]["real"])?);
        cpus.push(positive(&row["usage"]["user"])? + positive(&row["usage"]["sys"])?);
        loads.push(positive(&row["host_max_load_1m"])?);
        let finished: Vec<_> = log
            .lines()
            .filter_map(|l| l.strip_prefix("verify-event: "))
            .map(aterm_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|v| v["event"].as_str() == Some("measure-row-finished"))
            .collect();
        let [event] = finished.as_slice() else {
            return Err("expected exactly one finished row".into());
        };
        if event["matrix"].as_str() != Some("rewind")
            || event["row"].as_str()
                != Some("an_idle_recording_pane_adds_at_most_one_panes_budget_rss")
        {
            return Err("finished timing belongs to another row".into());
        }
        row_walls.push(positive(&event["seconds"])?);
        let stopped = metric(&log, "stopping_samples_per_arm")?;
        if metric(&log, "planned_pairs")? != 3
            || ![4, 6].contains(&stopped)
            || (arm == "A" && stopped != 6)
            || metric(&log, "pairs")? != stopped / 2
        {
            return Err("sample plan differs from the declaration".into());
        }
        stops.push(stopped);
        logs.push(log);
    }
    let mut metrics = aterm_json::Map::new();
    let mut shifted = false;
    for key in [
        "off_median_bytes",
        "on_median_bytes",
        "extra_bytes",
        "off_app_median_bytes",
        "on_app_median_bytes",
    ] {
        let values: [u64; 10] = logs
            .iter()
            .map(|log| metric(log, key))
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "metric is incomplete")?;
        let (extreme, assignments) = ranks(&values);
        let p = extreme as f64 / assignments as f64;
        shifted |= p < 0.05;
        metrics.insert(
            key.into(),
            json!({"values":values,"extreme":extreme,"assignments":assignments,"p_two_sided":p}),
        );
    }
    let wall = arms(&walls);
    let row_wall = arms(&row_walls);
    let improvement = |v: &Value| {
        v["B"]["median"].as_f64().expect("computed median")
            < v["A"]["median"].as_f64().expect("computed median")
    };
    let wall_criterion_met = improvement(&wall) && improvement(&row_wall);
    Ok(
        json!({"schema":1,"command_wall_s":wall,"row_wall_s":row_wall,"cpu_s":arms(&cpus),
            "wall_samples_s":walls,"row_samples_s":row_walls,"cpu_samples_s":cpus,
            "host_max_load_1m":loads,"stopping_samples_per_arm":stops,"metrics":metrics,
            "metrics_retain_ranks":!shifted,"wall_criterion_met":wall_criterion_met,
            "interpretation":"observed rank retention, not population equivalence; focused row only",
        }),
    )
}

#[cfg(test)]
#[path = "compare_tests.rs"]
mod tests;
