// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wall accounting from execution timestamps. Concurrent Count intervals form a union;
//! adding their individual row times would charge the same wall time several times.

use std::collections::BTreeMap;

use aterm_json::{Value, json};

use crate::Fail;

pub(super) fn read(log: &str) -> Result<Value, Fail> {
    let mut kinds = BTreeMap::new();
    let mut starts = BTreeMap::new();
    let mut rows = BTreeMap::new();
    let mut counts = BTreeMap::<String, Vec<(u64, u64)>>::new();
    let mut timing = 0.0;
    let mut setup_phases = BTreeMap::<String, BTreeMap<String, f64>>::new();
    let mut paint_totals = BTreeMap::<String, f64>::new();
    for line in log.lines() {
        if let Some(span) = line.strip_prefix("measure setup: paint ") {
            let mut fields = BTreeMap::new();
            for word in span.split_whitespace() {
                if let Some((key, value)) = word.split_once('=')
                    && key.ends_with("_s")
                    && fields.insert(key.to_owned(), seconds(value)?).is_some()
                {
                    return Err(Fail::die("duplicate paint span"));
                }
            }
            if !paint_totals.is_empty() && fields.keys().ne(paint_totals.keys()) {
                return Err(Fail::die("paint setup spans are incomplete"));
            }
            for (key, value) in fields {
                let total = paint_totals.entry(key).or_default();
                *total += value;
                if !total.is_finite() {
                    return Err(Fail::die("paint span total overflow"));
                }
            }
        }
        if let Some(span) = line.strip_prefix("measure phase: ") {
            let (subject, span) = span
                .split_once(" phase=")
                .ok_or_else(|| Fail::die("phase lacks subject"))?;
            let (phase, seconds) = span
                .split_once(" seconds=")
                .ok_or_else(|| Fail::die("phase lacks seconds"))?;
            let seconds = self::seconds(seconds)?;
            if setup_phases
                .entry(subject.to_owned())
                .or_default()
                .insert(phase.to_owned(), seconds)
                .is_some()
            {
                return Err(Fail::die("duplicate setup phase"));
            }
        }
        if let Some(header) = line.strip_prefix("measure kind: ") {
            let (row, kind) = header
                .split_once(' ')
                .ok_or_else(|| Fail::die("bad row kind"))?;
            kinds.insert(row.to_owned(), kind.starts_with("Count {"));
        }
        let Some(event) = line.strip_prefix("verify-event: ") else {
            continue;
        };
        let event: Value =
            aterm_json::from_str(event).map_err(|e| Fail::die(format!("measure event: {e}")))?;
        let finish = match event["event"].as_str() {
            Some("measure-row-started") => false,
            Some("measure-row-finished") => true,
            _ => continue,
        };
        let (Some(matrix), Some(row), Some(at)) = (
            event["matrix"].as_str(),
            event["row"].as_str(),
            event["at_unix_ms"].as_u64(),
        ) else {
            return Err(Fail::die(
                "measure boundary lacks its identity or timestamp",
            ));
        };
        let name = format!("{matrix}::{row}");
        if !finish {
            if starts.insert(name, at).is_some() {
                return Err(Fail::die("duplicate measure start"));
            }
            continue;
        }
        let started = starts
            .remove(&name)
            .ok_or_else(|| Fail::die("measure finish lacks its start"))?;
        let seconds = event["seconds"]
            .as_f64()
            .filter(|s| s.is_finite() && *s >= 0.0)
            .ok_or_else(|| Fail::die("invalid row wall"))?;
        if at < started {
            return Err(Fail::die("measure clock ran backwards"));
        }
        let count = kinds.get(&name).copied().unwrap_or(false);
        if count {
            counts
                .entry(matrix.to_owned())
                .or_default()
                .push((started, at));
        } else {
            timing += seconds;
        }
        if rows
            .insert(
                name,
                json!({"kind":if count {"Count"}else{"Timing"},"seconds":seconds}),
            )
            .is_some()
        {
            return Err(Fail::die("duplicate measure finish"));
        }
    }
    if !starts.is_empty() || rows.is_empty() {
        return Err(Fail::die("incomplete measure boundaries"));
    }
    let phases: BTreeMap<_, _> = counts
        .into_iter()
        .map(|(matrix, intervals)| (matrix, union_ms(intervals) as f64 / 1000.0))
        .collect();
    Ok(
        json!({"rows":rows,"count_phase_s":phases,"timing_rows_s":timing,"setup_phases":setup_phases,"paint_setup_totals_s":paint_totals}),
    )
}

fn seconds(value: &str) -> Result<f64, Fail> {
    value
        .parse::<f64>()
        .ok()
        .filter(|s| s.is_finite() && *s >= 0.0)
        .ok_or_else(|| Fail::die("invalid phase wall"))
}

fn union_ms(mut intervals: Vec<(u64, u64)>) -> u64 {
    intervals.sort_unstable();
    let (mut total, mut end) = (0_u64, 0);
    for (from, to) in intervals {
        total = total.saturating_add(to.saturating_sub(from.max(end)));
        end = end.max(to);
    }
    total
}

#[cfg(test)]
#[path = "measure_costs_tests.rs"]
mod tests;
