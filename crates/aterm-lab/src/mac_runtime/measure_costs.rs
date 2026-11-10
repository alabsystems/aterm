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
mod tests {
    use super::*;

    #[test]
    fn concurrent_wall_is_a_union_and_an_unfinished_row_is_not_zero() {
        assert_eq!(union_ms(vec![(10, 30), (10, 20), (15, 25), (40, 50)]), 30);
        assert_ne!(
            union_ms(vec![(10, 30), (10, 20)]),
            30,
            "negative control: overlapping row walls must not be added"
        );
        let boundary = |event, row, at, seconds| {
            format!(
                "verify-event: {}\n",
                json!({"event":event,"matrix":"test","row":row,"at_unix_ms":at,"seconds":seconds})
            )
        };
        let mut log = "measure kind: test::a Count { windowed: false }\nmeasure kind: test::b Count { windowed: true }\n".to_owned();
        log += &boundary("measure-row-started", "a", 1000, 0.0);
        log += &boundary("measure-row-started", "b", 1000, 0.0);
        log += &boundary("measure-row-finished", "a", 11000, 10.0);
        assert!(read(&log).is_err());
        log += &boundary("measure-row-finished", "b", 21000, 20.0);
        log += &boundary("measure-row-started", "solo", 21000, 0.0);
        log += &boundary("measure-row-finished", "solo", 26000, 5.0);
        let costs = read(&log).unwrap();
        assert_eq!(costs["count_phase_s"]["test"].as_f64(), Some(20.0));
        assert_eq!(costs["timing_rows_s"].as_f64(), Some(5.0));
        assert_eq!(costs["rows"]["test::solo"]["kind"].as_str(), Some("Timing"));
        log += "measure setup: paint scene=Prompt capture_s=2 pacing_s=1\nmeasure setup: paint scene=Plane capture_s=3 pacing_s=1\n";
        assert_eq!(
            read(&log).unwrap()["paint_setup_totals_s"]["capture_s"].as_f64(),
            Some(5.)
        );
        assert_eq!(
            read(&log).unwrap()["paint_setup_totals_s"]["pacing_s"].as_f64(),
            Some(2.)
        );
        assert!(read(&log.replace("capture_s=3", "capture_s=NaN")).is_err());
        assert!(read(&log.replace("capture_s=3 pacing_s=1", "capture_s=3")).is_err());
        assert!(read(&log.replace("capture_s=3", "capture_s=3 capture_s=3")).is_err());
        assert!(
            read(
                &log.replace("capture_s=2", "capture_s=1e308")
                    .replace("capture_s=3", "capture_s=1e308")
            )
            .is_err()
        );
        log += "measure phase: row=busy_rss record=false phase=ready seconds=9.5\n";
        assert_eq!(
            read(&log).unwrap()["setup_phases"]["row=busy_rss record=false"]["ready"].as_f64(),
            Some(9.5)
        );
        assert!(read(&log.replace("seconds=9.5", "seconds=NaN")).is_err());
        log += "measure phase: row=busy_rss record=false phase=ready seconds=0.5\n";
        assert!(
            read(&log).is_err(),
            "a duplicate phase cannot overwrite the first span"
        );
    }
}
