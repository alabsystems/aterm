// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn pressure_line(index: usize) -> String {
    format!("pressure-line-{index:03}-{}", "x".repeat(48))
}

fn yellow_budget_for(current_budgeted_bytes: usize) -> usize {
    let mut budget = current_budgeted_bytes
        .saturating_mul(PRESSURE_YELLOW_DENOMINATOR)
        .saturating_add(PRESSURE_YELLOW_NUMERATOR - 1)
        .saturating_div(PRESSURE_YELLOW_NUMERATOR);
    if budget <= current_budgeted_bytes {
        budget = current_budgeted_bytes.saturating_add(1);
    }
    budget
}

#[test]
fn scrollback_pressure_level_tracks_yellow_and_red_thresholds() {
    let memory_budget = 64 * 1024;
    let yellow_threshold =
        yellow_pressure_threshold(memory_budget).expect("positive budget has threshold");

    assert!(
        yellow_threshold > 0 && yellow_threshold < memory_budget,
        "test fixture should produce a real yellow watermark"
    );
    assert_eq!(
        pressure_level_from_budget(yellow_threshold.saturating_sub(1), memory_budget),
        ScrollbackPressureLevel::Green,
        "bytes below the yellow watermark should stay green"
    );
    assert_eq!(
        pressure_level_from_budget(yellow_threshold, memory_budget),
        ScrollbackPressureLevel::Yellow,
        "bytes at the yellow watermark should enter yellow pressure"
    );
    assert!(
        yellow_threshold < memory_budget,
        "yellow pressure must remain below the hard budget"
    );
    assert_eq!(
        pressure_level_from_budget(memory_budget, memory_budget),
        ScrollbackPressureLevel::Red,
        "bytes at the hard budget should enter red pressure"
    );
}

#[test]
fn scrollback_yellow_watermark_promotes_hot_block_before_hot_limit() {
    let mut sb = Scrollback::with_block_size(64, 256, 1_000_000, 4);
    for index in 0..4 {
        sb.push_str(&pressure_line(index));
    }

    assert_eq!(
        sb.hot_line_count(),
        4,
        "fixture should fill exactly one hot block"
    );
    assert_eq!(
        sb.warm_line_count(),
        0,
        "fixture should start with no warm data"
    );
    assert!(
        sb.hot_line_count() < sb.hot_limit(),
        "fixture must stay below the normal hot-limit promotion boundary"
    );

    let yellow_budget = yellow_budget_for(sb.budgeted_bytes);
    assert!(
        yellow_pressure_threshold(yellow_budget).expect("positive budget has threshold")
            <= sb.budgeted_bytes,
        "fixture budget should enter the yellow watermark"
    );

    sb.set_memory_budget(yellow_budget)
        .expect("memory budget update should succeed");

    assert_eq!(
        sb.hot_line_count(),
        0,
        "yellow pressure should pre-compress the hot block"
    );
    assert_eq!(
        sb.warm_line_count(),
        4,
        "yellow pressure should move one full block to warm"
    );
}

#[test]
fn disk_backed_yellow_watermark_promotes_hot_block_before_hot_limit() {
    let temp_dir = aterm_tempfile::tempdir().expect("create temp dir");
    let cold_path = temp_dir.path().join("pressure-scrollback.dtrm");
    let config = DiskBackedScrollbackConfig::new(&cold_path)
        .with_hot_limit(64)
        .with_warm_limit(256)
        .with_block_size(4);
    let mut sb = DiskBackedScrollback::with_config(config).expect("create disk scrollback");

    for index in 0..4 {
        sb.push_str(&pressure_line(index))
            .expect("push into disk-backed scrollback");
    }

    assert_eq!(
        sb.hot_line_count(),
        4,
        "fixture should fill exactly one hot block"
    );
    assert_eq!(
        sb.warm_line_count(),
        0,
        "fixture should start with no warm data"
    );
    assert!(
        sb.hot_line_count() < sb.hot_limit(),
        "fixture must stay below the normal hot-limit promotion boundary"
    );

    let yellow_budget = yellow_budget_for(sb.budgeted_bytes());
    assert!(
        yellow_pressure_threshold(yellow_budget).expect("positive budget has threshold")
            <= sb.budgeted_bytes(),
        "fixture budget should enter the yellow watermark"
    );

    sb.set_memory_budget(yellow_budget)
        .expect("memory budget update should succeed");

    assert_eq!(
        sb.hot_line_count(),
        0,
        "yellow pressure should pre-compress the hot block"
    );
    assert_eq!(
        sb.warm_line_count(),
        4,
        "yellow pressure should move one full block to warm"
    );
}
