// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rainbow Kitty v2's §18 CPU budget (`docs/design/RAINBOW-KITTY-V2.md`): p50
//! and p90 producer CPU per tick on every §21 scenario, for the engine alone
//! and through the seam, on the harness whose deterministic laws
//! `tests/rainbow_kitty_v2_frame_cost.rs` holds every run.
//!
//! ```sh
//! targo --unverified bench -p aterm-effects --bench rainbow_kitty_v2_frame_cost
//! # on a host with no §18 measurement on record, scale the two absolute
//! # budgets (and nothing else; see `common::budget_scale`):
//! RK_FRAME_COST_BUDGET_SCALE=2 targo --unverified bench -p aterm-effects \
//!     --bench rainbow_kitty_v2_frame_cost
//! ```

// The suite's per-thread counting allocator and shared support, mounted at
// this binary's root where the harness reaches them (`crate::alloc_count`,
// `crate::common`), as `effects_suite` mounts them.
#[path = "../tests/alloc_count/mod.rs"]
mod alloc_count;
#[path = "../tests/common/mod.rs"]
mod common;
// The harness, mounted without its laws, which most of its helpers serve.
#[allow(dead_code, unused_imports)]
#[path = "../tests/rainbow_kitty_v2_frame_cost.rs"]
mod frame_cost;

use common::{BUDGET_SCALE_VAR, budget_scale, scaled_budget_us};
use frame_cost::{
    Report, Scenario, engine_selected, measure_cell, print_header, print_idle, print_row, scenarios,
};

/// One row set: the engine alone and v2 through the seam. An engine
/// `RK_FRAME_COST_ONLY` leaves out reports empty, which the gates treat as
/// nothing to hold.
fn measure_all(sc: &Scenario) -> (Report, Report) {
    let r2 = if engine_selected("v2") {
        let r = measure_cell("v2", sc);
        print_row("v2 ", sc, &r);
        r
    } else {
        Report::default()
    };
    let rs = if engine_selected("v2s") {
        let r = measure_cell("v2s", sc);
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

/// §18's CPU budget — release only, like `cursor_bench`: p50 ≤ 400 µs,
/// p90 ≤ 600 µs per tick including the ribbon, on every §21 scenario, for
/// the engine alone and through the seam, measured in the same harness on
/// the same script.
///
/// The two numbers are the reference machine's (§18 of
/// docs/design/RAINBOW-KITTY-V2.md); every v2 run the doc records is at most
/// p50 137 / p90 151 µs. `RK_FRAME_COST_BUDGET_SCALE` (`common::budget_scale`) scales the two
/// reference numbers, for the engine alone and through the seam, on a host
/// with no measurement on record, and nothing else: the zero-alloc,
/// idle → zero and cap laws the harness's test holds are allocation counts and a
/// scripted clock, never the wall clock, and never read it.
fn main() {
    if cfg!(debug_assertions) {
        panic!("the CPU budget is a release-build number: run it with `targo --unverified bench`");
    }
    // The reference machine's numbers (§18); the host's thresholds are these
    // times `RK_FRAME_COST_BUDGET_SCALE`, and the header and the red print
    // both, so a red names the budget it was measured against.
    const P50_BUDGET_US: u64 = 400;
    const P90_BUDGET_US: u64 = 600;
    let scale = budget_scale();
    let p50_budget_us = scaled_budget_us(P50_BUDGET_US, scale);
    let p90_budget_us = scaled_budget_us(P90_BUDGET_US, scale);
    let budget = format!(
        "p50 ≤ {p50_budget_us} µs / p90 ≤ {p90_budget_us} µs \
         (reference {P50_BUDGET_US}/{P90_BUDGET_US} × {BUDGET_SCALE_VAR} {scale})"
    );
    print_header(&format!("release build — §18 budget {budget}"));
    let mut reds: Vec<String> = Vec::new();
    for sc in scenarios() {
        let (r2, rs) = measure_all(&sc);
        if engine_selected("v2") && r2.p50() > p50_budget_us {
            reds.push(format!(
                "[{}] v2 p50 {} µs > {p50_budget_us} µs",
                sc.name,
                r2.p50()
            ));
        }
        if engine_selected("v2") && r2.p90() > p90_budget_us {
            reds.push(format!(
                "[{}] v2 p90 {} µs > {p90_budget_us} µs",
                sc.name,
                r2.p90()
            ));
        }
        // The shipping path: v2 THROUGH `CursorGlow`'s seam is held to the
        // same absolute budget as the engine alone.
        if engine_selected("v2s") && rs.p50() > p50_budget_us {
            reds.push(format!(
                "[{}] v2-via-seam p50 {} µs > {p50_budget_us} µs",
                sc.name,
                rs.p50()
            ));
        }
        if engine_selected("v2s") && rs.p90() > p90_budget_us {
            reds.push(format!(
                "[{}] v2-via-seam p90 {} µs > {p90_budget_us} µs",
                sc.name,
                rs.p90()
            ));
        }
    }
    assert!(
        reds.is_empty(),
        "rainbow kitty v2 CPU budget RED ({budget}):\n  {}",
        reds.join("\n  ")
    );
}
