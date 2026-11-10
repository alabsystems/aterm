// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Offline audit of capped RSS decisions on retained complete measuring logs. Runs no product.

#[cfg(unix)]
#[path = "measure_decisions/compare.rs"]
mod compare;

#[cfg(unix)]
fn main() -> Result<(), String> {
    use aterm_conformance::measure::rewind_cost::{added_bytes, idle_decision, idle_verdict};
    use aterm_conformance::measure::starvation::ShapeVerdict;
    use aterm_json::{Value, json};
    let args: Vec<_> = std::env::args().skip(1).collect();
    if let [flag, dir] = args.as_slice()
        && flag == "--compare"
    {
        println!("{}", compare::run(std::path::Path::new(dir))?);
        return Ok(());
    }
    let account_only = args.first().is_some_and(|flag| flag == "--account");
    let mut records = Vec::new();
    let mut errors = 0;
    let mut controls_failed = 0;
    for path in args.iter().skip(usize::from(account_only)) {
        let log = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let mut rows = Vec::new();
        let mut outer_rows = Vec::new();
        let mut setup = Vec::new();
        let mut samples = Vec::new();
        let mut reports = Vec::new();
        let mut phases = Vec::new();
        for line in log.lines() {
            if let Some(span) = line.strip_prefix("measure setup: paint ") {
                let mut fields = aterm_json::Map::new();
                for word in span.split_whitespace() {
                    if let Some((key, value)) = word.split_once('=') {
                        fields.insert(
                            key.into(),
                            if key.ends_with("_s") {
                                let seconds: f64 = value.parse().map_err(|e| format!("{e}"))?;
                                if !seconds.is_finite() || seconds < 0.0 {
                                    return Err("invalid setup span".into());
                                }
                                json!(seconds)
                            } else {
                                json!(value)
                            },
                        );
                    }
                }
                setup.push(Value::Object(fields));
            }
            if let Some(span) = line.strip_prefix("measure phase: ") {
                let (subject, span) = span.split_once(" phase=").ok_or("phase lacks identity")?;
                let (phase, seconds) = span.split_once(" seconds=").ok_or("phase lacks time")?;
                let seconds: f64 = seconds.parse().map_err(|e| format!("{e}"))?;
                if !seconds.is_finite() || seconds < 0.0 {
                    return Err("invalid phase span".into());
                }
                phases.push(json!({"subject":subject,"phase":phase,"seconds":seconds}));
            }
            if let Some(sample) = line.strip_prefix("measure-sample: ") {
                let sample: Value = aterm_json::from_str(sample).map_err(|e| e.to_string())?;
                samples.push(sample);
            }
            if ["PAINT ", "SPIN ", "AOS ", "REWIND ", "COST "]
                .iter()
                .any(|prefix| line.starts_with(prefix))
            {
                reports.push(line);
            }
            if let Some(event) = line.strip_prefix("verify-event: ") {
                let event: Value = aterm_json::from_str(event).map_err(|e| e.to_string())?;
                if event["event"].as_str() == Some("measure-row-finished") {
                    rows.push(event);
                } else if event["event"].as_str() == Some("row-finished") {
                    outer_rows.push(event);
                }
            }
        }
        let mut decisions = Vec::new();
        if account_only && rows.is_empty() && outer_rows.is_empty() {
            return Err(format!("{path}: no measured row boundaries to account"));
        }
        for line in log
            .lines()
            .filter(|l| !account_only && l.starts_with("COST idle_rss "))
        {
            let vector = |key| -> Result<Vec<u64>, String> {
                let tail = line
                    .split_once(&format!("{key}=["))
                    .ok_or_else(|| format!("missing {key}"))?
                    .1;
                let values = tail
                    .split_once(']')
                    .ok_or_else(|| format!("unclosed {key}"))?
                    .0;
                aterm_json::from_str(&format!("[{values}]")).map_err(|e| e.to_string())
            };
            let scalar = |key| -> Result<&str, String> {
                line.split_whitespace()
                    .find_map(|w| w.strip_prefix(&format!("{key}=")))
                    .ok_or_else(|| format!("missing {key}"))
            };
            let off = vector("off_bytes")?;
            let on = vector("on_bytes")?;
            let app = vector("on_app_bytes")?;
            let max: u64 = scalar("max_extra_bytes")?
                .parse()
                .map_err(|e| format!("{e}"))?;
            let by_default: bool = scalar("records_by_default")?
                .parse()
                .map_err(|e| format!("{e}"))?;
            let cap: usize = scalar("planned_pairs")
                .or_else(|_| scalar("pairs"))?
                .parse::<usize>()
                .map_err(|e| format!("{e}"))?
                .checked_mul(2)
                .ok_or("sample cap overflow")?;
            if off.len() != cap || on.len() != cap || app.len() != cap {
                return Err(format!("{path}: replay needs a complete capped sample"));
            }
            let replay = |on: &[u64]| -> Result<(usize, ShapeVerdict), String> {
                (2..=cap)
                    .step_by(2)
                    .find_map(|n| {
                        idle_decision(&off[..n], &on[..n], &app[..n], cap, max, by_default)
                            .map(|v| (n, v))
                    })
                    .ok_or_else(|| "no decision at cap".into())
            };
            let expected = idle_verdict(&off, &on, &app, max, by_default);
            let (stopped, actual) = replay(&on)?;
            errors += usize::from(actual != expected);
            // Shift every on-arm reading by enough that the old finite median judge fails
            // just one byte past the row's bound, preserving the recorded take's spread/order.
            let median = |values: &[u64]| {
                let mut sorted = values.to_vec();
                sorted.sort_unstable();
                sorted[(sorted.len() - 1) / 2]
            };
            let target = median(&off)
                .checked_add(max)
                .and_then(|v| v.checked_add(1))
                .ok_or("shift overflow")?;
            let center = median(&on);
            let shifted: Vec<_> = on
                .iter()
                .map(|&v| {
                    if target >= center {
                        v.checked_add(target - center)
                    } else {
                        v.checked_sub(center - target)
                    }
                })
                .collect::<Option<_>>()
                .ok_or("shift overflow")?;
            let (planted_stop, planted) = replay(&shifted)?;
            let planted_full = idle_verdict(&off, &shifted, &app, max, by_default);
            if added_bytes(&off, &shifted) != max + 1 || planted_full != ShapeVerdict::Fail {
                return Err("negative control did not exceed the bound by one byte".into());
            }
            controls_failed += usize::from(planted == ShapeVerdict::Fail);
            errors += usize::from(planted != planted_full);
            decisions.push(json!({
                "cap_per_arm": cap, "stopping_samples_per_arm": stopped,
                "full_verdict": format!("{expected:?}"), "early_verdict": format!("{actual:?}"),
                "shifted_extra_bytes": max + 1, "shifted_stopping_samples_per_arm": planted_stop,
                "shifted_verdict": format!("{planted:?}"),
            }));
        }
        if !account_only && decisions.len() != 1 {
            return Err(format!("{path}: expected exactly one idle RSS row"));
        }
        records.push(
            json!({"path":path,"rows":rows,"outer_rows":outer_rows,"idle_rss":decisions,
                "paint_setup_in_execution_order":setup, "phases":phases,
                "samples":samples,"reports":reports,
            }),
        );
    }
    if records.is_empty() {
        return Err("usage: measure-decisions [--account] LOG... | --compare DIR".into());
    }
    println!(
        "{}",
        json!({
            "schema":1, "instrument":if account_only {"measure-row-account"} else {"finite-cap-completion-replay"}, "takes":records.len(),
            "decision_disagreements":if account_only {Value::Null} else {json!(errors)},
            "shifted_controls_failed":if account_only {Value::Null} else {json!(controls_failed)},
            "population_error_rate":Value::Null, "records":records,
        })
    );
    if errors != 0 {
        return Err("replay changed a verdict".into());
    }
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    eprintln!("measure-decisions requires Unix conformance instruments");
    std::process::exit(3);
}
