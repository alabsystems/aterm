// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M-HIST's cached model-query boundary: parse, Arc snapshot selection, projection and complete
//! response serialization. Fixture construction and warm-up are outside the timed interval.
//! This does not measure a control socket, GUI lock, scheduling delay or native probe refresh.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use aterm_vitals::model::Snapshot;
use aterm_vitals::sys::view::{render_json, render_text};
use aterm_vitals::sys::{Ctx, Freshness, Machine, Reply, SysQuery, Target};

#[path = "sys_history_cost/fixture.rs"]
mod fixture;

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
        return Err("samples must be 20..=10000 and warmup 0..=10000".into());
    }
    Ok(result)
}

fn pull(snapshot: &Arc<Snapshot>, request: &str) -> Result<String, String> {
    let query = SysQuery::parse(request).map_err(str::to_owned)?;
    let selected = Arc::clone(snapshot);
    let machine = Machine {
        family: "Synthetic",
        ..Machine::default()
    };
    let target = Target::default();
    let ctx = Ctx {
        snap: Some(&selected),
        epoch: 1,
        as_of_unix_ms: selected
            .at_unix_ms
            .try_into()
            .map_err(|_| "negative fixture epoch")?,
        age_ms: Some(0),
        state: Freshness::Fresh,
        machine: &machine,
        target: &target,
        tabs: &[],
        caller_panes: None,
        event_caller: None,
        owner_class: true,
        volume: None,
        roots: &[],
        disk: None,
        stores: &[],
        home: None,
        events: &[],
        gap: None,
        head_seq: selected.seq,
        held: None,
    };
    if query.json {
        let json = render_json(&query, &ctx);
        let body = json.to_text();
        let reply = Reply {
            view: query.view,
            form: query.form,
            scope: None,
            subject: None,
            lines: Vec::new(),
            json: Some(json),
        };
        Ok(format!("{}\n{body}\n", reply.header(&ctx, false)))
    } else {
        let reply = render_text(&query, &ctx);
        let mut result = reply.header(&ctx, false);
        result.push('\n');
        for line in reply.lines {
            result.push_str(&line.text);
            result.push('\n');
        }
        Ok(result)
    }
}

fn quantile(sorted: &[f64], percent: usize) -> f64 {
    sorted[(sorted.len() * percent).div_ceil(100).saturating_sub(1)]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = options()?;
    let snapshot = fixture::filled_rings();
    println!(
        "measurement=M-HIST boundary=cached_model_parse_arc_project_serialize ipc=unmeasured gui_lock=unmeasured native_refresh=unmeasured source={} profile={} debug_assertions={} os={} arch={} warmup={} samples={}",
        options.source,
        options.profile,
        cfg!(debug_assertions),
        std::env::consts::OS,
        std::env::consts::ARCH,
        options.warmup,
        options.samples
    );
    let mut worst_default = 0.0_f64;
    for rows in ["", " cpu mem"] {
        for resolution in ["", " res=1s", " res=10s", " res=1m", " res=10m", " res=1h"] {
            for json in [false, true] {
                let request = format!(
                    "history{rows}{resolution}{}",
                    if json { " --json" } else { "" }
                );
                for _ in 0..options.warmup {
                    black_box(pull(&snapshot, &request)?);
                }
                let mut values = Vec::with_capacity(options.samples);
                let mut bytes = 0;
                for _ in 0..options.samples {
                    let began = Instant::now();
                    let response = pull(black_box(&snapshot), black_box(&request))?;
                    let elapsed = began.elapsed().as_secs_f64() * 1000.0;
                    bytes = response.len();
                    black_box(response);
                    values.push(elapsed);
                }
                values.sort_by(f64::total_cmp);
                let p95 = quantile(&values, 95);
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
        "M-HIST default_all_rows_worst_p95_ms={worst_default:.6} budget_ms=2 outcome={}",
        if worst_default <= 2.0 {
            "pass"
        } else {
            "over_budget"
        }
    );
    if worst_default > 2.0 {
        return Err("M-HIST default cached model pull exceeds 2 ms p95".into());
    }
    Ok(())
}
