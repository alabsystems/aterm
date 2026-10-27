// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rainbow Kitty v2 producer CPU budget benchmark across §21 scenarios.

#[path = "../tests/alloc_count/mod.rs"]
mod alloc_count;
#[path = "../tests/common/mod.rs"]
mod common;
#[allow(dead_code, unused_imports)]
#[path = "../tests/rainbow_kitty_v2_frame_cost.rs"]
mod frame_cost;

use common::{BUDGET_SCALE_VAR, Ground, budget_scale, scaled_budget_us};
use frame_cost::{
    Report, Scenario, engine_selected, grounds, measure_cell, print_header, print_idle, print_row,
    scenarios,
};

fn measure_all(sc: &Scenario, ground: Ground) -> (Report, Report) {
    let r2 = if engine_selected("v2") {
        let r = measure_cell("v2", sc, ground);
        print_row("v2 ", sc, &r);
        r
    } else {
        Report::default()
    };
    let rs = if engine_selected("v2s") {
        let r = measure_cell("v2s", sc, ground);
        print_row("v2s", sc, &r);
        r
    } else {
        Report::default()
    };
    if engine_selected("v2") {
        print_idle(sc, &r2);
    }
    (r2, rs)
}

fn main() {
    if cfg!(debug_assertions) {
        panic!("the CPU budget is a release-build number: run it with `targo --unverified bench`");
    }
    const P50_BUDGET_US: u64 = 400;
    const P90_BUDGET_US: u64 = 600;
    let scale = budget_scale();
    let p50_budget_us = scaled_budget_us(P50_BUDGET_US, scale);
    let p90_budget_us = scaled_budget_us(P90_BUDGET_US, scale);
    let budget = format!(
        "p50 ≤ {p50_budget_us} µs / p90 ≤ {p90_budget_us} µs \
         (reference {P50_BUDGET_US}/{P90_BUDGET_US} × {BUDGET_SCALE_VAR} {scale})"
    );
    let mut reds: Vec<String> = Vec::new();
    for ground in grounds() {
        print_header(&format!("release build — §18 budget {budget}"), ground);
        for sc in scenarios() {
            let (r2, rs) = measure_all(&sc, ground);
            let at = format!("{} on {}", sc.name, ground.name);
            if engine_selected("v2") && r2.p50() > p50_budget_us {
                reds.push(format!(
                    "[{at}] v2 p50 {} µs > {p50_budget_us} µs",
                    r2.p50()
                ));
            }
            if engine_selected("v2") && r2.p90() > p90_budget_us {
                reds.push(format!(
                    "[{at}] v2 p90 {} µs > {p90_budget_us} µs",
                    r2.p90()
                ));
            }
            if engine_selected("v2s") && rs.p50() > p50_budget_us {
                reds.push(format!(
                    "[{at}] v2-via-seam p50 {} µs > {p50_budget_us} µs",
                    rs.p50()
                ));
            }
            if engine_selected("v2s") && rs.p90() > p90_budget_us {
                reds.push(format!(
                    "[{at}] v2-via-seam p90 {} µs > {p90_budget_us} µs",
                    rs.p90()
                ));
            }
        }
    }
    assert!(
        reds.is_empty(),
        "rainbow kitty v2 CPU budget RED ({budget}):\n  {}",
        reds.join("\n  ")
    );
}
