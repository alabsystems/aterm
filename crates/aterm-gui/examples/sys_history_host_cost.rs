// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M-HIST: the complete cached in-process host handler over six hours of filled engine rings.
//! Parsing, cache/freshness locks, Store authorization, gathering, reply bookkeeping and full
//! serialization are timed. Fixture/probe construction and warm-up are not. Observation-age
//! clocks are fixed and injected; IPC, outer control dispatch and native refresh are unmeasured.
//!
//! `targo --unverified run -p aterm-gui --profile opt-test --features bench-support
//! --example sys_history_host_cost -- --source <commit> --profile opt-test`

#[cfg(debug_assertions)]
#[path = "../../aterm-vitals/examples/sys_history_cost/fixture.rs"]
mod fixture;

#[cfg(debug_assertions)]
mod measure {
    use std::hint::black_box;
    use std::time::Instant;

    use aterm_gui::HistoryPullProbe;

    struct Options {
        samples: usize,
        warmup: usize,
        source: String,
        profile: String,
    }

    fn options() -> Result<Options, String> {
        let mut result = Options {
            samples: 200,
            warmup: 20,
            source: "unrecorded".into(),
            profile: "unrecorded".into(),
        };
        let mut args = std::env::args().skip(1);
        while let Some(flag) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            match flag.as_str() {
                "--samples" => result.samples = value.parse().map_err(|_| "invalid samples")?,
                "--warmup" => result.warmup = value.parse().map_err(|_| "invalid warmup")?,
                "--source" => result.source = value,
                "--profile" => result.profile = value,
                _ => return Err(format!("unknown option {flag}")),
            }
        }
        if !(20..=10_000).contains(&result.samples) || result.warmup > 10_000 {
            return Err("samples must be20..=10000 and warmup0..=10000".into());
        }
        Ok(result)
    }

    fn quantile(sorted: &[f64], percent: usize) -> f64 {
        sorted[(sorted.len() * percent).div_ceil(100).saturating_sub(1)]
    }

    pub(super) fn run() -> Result<(), Box<dyn std::error::Error>> {
        let options = options()?;
        let snapshot = super::fixture::filled_rings();
        let probe = HistoryPullProbe::new(snapshot);
        probe.verify_isolation();
        println!(
            "measurement=M-HIST boundary=cached_host_cmd_sys registration=in_memory mailbox_isolation=checked publication=machine_only age_clock=injected_fixed ipc=unmeasured outer_control_dispatch=unmeasured native_refresh=unmeasured source={} profile={} debug_assertions={} os={} arch={} warmup={} samples={}",
            options.source,
            options.profile,
            cfg!(debug_assertions),
            std::env::consts::OS,
            std::env::consts::ARCH,
            options.warmup,
            options.samples
        );
        let mut worst = 0.0_f64;
        let mut worst_default = 0.0_f64;
        for rows in ["", " cpu mem"] {
            for resolution in ["", " res=1s", " res=10s", " res=1m", " res=10m", " res=1h"] {
                for json in [false, true] {
                    let request = format!(
                        "history{rows}{resolution}{}",
                        if json { " --json" } else { "" }
                    );
                    let proof = probe.pull(&request);
                    let header = proof.lines().next().expect("the host reply header");
                    assert!(
                        header.starts_with("OK ")
                            && header.contains("view=history")
                            && !header.contains(" state="),
                        "a fresh cached reply omits the state field: {proof}"
                    );
                    assert!(proof.ends_with('\n'));
                    let expected_rows = if rows.is_empty() {
                        aterm_vitals::model::Resource::ALL.len()
                    } else {
                        2
                    };
                    let expected_boxes = match resolution {
                        "" => 34,
                        " res=1s" => 10,
                        " res=1m" => 9,
                        _ => 5,
                    };
                    if json {
                        let body: aterm_json::Value = aterm_json::from_str(
                            proof.lines().nth(1).expect("the complete JSON body"),
                        )
                        .expect("the production host must serialize valid JSON");
                        let rows = body["history"]["rows"].as_array().expect("history rows");
                        assert_eq!(rows.len(), expected_rows);
                        for row in rows {
                            assert_eq!(
                                row["boxes"].as_array().expect("history boxes").len(),
                                expected_boxes
                            );
                        }
                    } else {
                        assert_eq!(
                            proof
                                .lines()
                                .filter(|line| line.starts_with("history row="))
                                .count(),
                            expected_rows
                        );
                        assert_eq!(
                            proof
                                .lines()
                                .filter(|line| line.starts_with("box row="))
                                .count(),
                            expected_rows * expected_boxes
                        );
                    }
                    let bytes = proof.len();
                    for _ in 0..options.warmup {
                        black_box(probe.pull(&request));
                    }
                    let mut values = Vec::with_capacity(options.samples);
                    for _ in 0..options.samples {
                        let began = Instant::now();
                        let response = probe.pull(black_box(&request));
                        let elapsed = began.elapsed().as_secs_f64() * 1000.0;
                        assert_eq!(
                            response.len(),
                            bytes,
                            "one original cache observation must remain stable"
                        );
                        black_box(response);
                        values.push(elapsed);
                    }
                    values.sort_by(f64::total_cmp);
                    let p95 = quantile(&values, 95);
                    worst = worst.max(p95);
                    if rows.is_empty() && resolution.is_empty() {
                        worst_default = worst_default.max(p95);
                    }
                    println!(
                        "case rows={} res={} shape={} bytes={bytes} n={} p50_ms={:.6} p95_ms={p95:.6} max_ms={:.6}",
                        if rows.is_empty() { "all" } else { "cpu,mem" },
                        if resolution.is_empty() {
                            "full_6h"
                        } else {
                            resolution.trim_start_matches(" res=")
                        },
                        if json { "json" } else { "text" },
                        options.samples,
                        quantile(&values, 50),
                        values[values.len() - 1]
                    );
                }
            }
        }
        println!(
            "M-HIST boundary=cached_host_cmd_sys worst_p95_ms={worst:.6} default_all_rows_worst_p95_ms={worst_default:.6} budget_ms=2 outcome={}",
            if worst <= 2.0 { "pass" } else { "over_budget" }
        );
        if worst > 2.0 {
            return Err("M-HIST cached in-process host pull exceeds2ms p95".into());
        }
        Ok(())
    }
}

#[cfg(debug_assertions)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    measure::run()
}

#[cfg(not(debug_assertions))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("sys_history_host_cost requires debug assertions; use --profile opt-test".into())
}
