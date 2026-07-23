// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Regression tests for #5928: DiskBackedScrollback cold tier reload.
//!
//! Each test populates cold tier data in session 1, drops, reloads via
//! `with_config` in session 2, and exercises one cascading failure mode
//! that was broken when `line_count` was hardcoded to 0.
//!
//! Extracted from disk_backed_tests.rs to stay under 1000 LOC.

use super::*;

/// Helper: populate a cold tier with `total_lines` lines, return cold line count.
fn populate_cold_tier(cold_path: &std::path::Path, total_lines: usize) -> usize {
    let config = DiskBackedScrollbackConfig::new(cold_path)
        .with_hot_limit(5)
        .with_warm_limit(10)
        .with_block_size(5);
    let mut sb = DiskBackedScrollback::with_config(config).unwrap();

    for i in 0..total_lines {
        sb.push_str(&format!("Line-{i}")).unwrap();
    }

    let cold = sb.cold_line_count();
    assert!(cold > 0, "session 1 must evict to cold tier: cold={cold}");
    cold
}

/// Reload a `DiskBackedScrollback` from the given path.
fn reload_from(cold_path: &std::path::Path) -> DiskBackedScrollback {
    let config = DiskBackedScrollbackConfig::new(cold_path)
        .with_hot_limit(5)
        .with_warm_limit(10)
        .with_block_size(5);
    DiskBackedScrollback::with_config(config).unwrap()
}

/// Cold tier data is accessible after drop + reload from the same path.
///
/// Regression test for #5928: `with_config` previously hardcoded `line_count: 0`,
/// making all cold tier data invisible after reload because `get_line()` checks
/// `idx >= self.line_count` and returns `Ok(None)`.
#[test]
fn disk_backed_cold_data_persists_on_reload() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("persist.dtrm");

    let total_lines = 50;

    // Session 1: Populate all tiers including cold.
    {
        let config = DiskBackedScrollbackConfig::new(&cold_path)
            .with_hot_limit(5)
            .with_warm_limit(10)
            .with_block_size(5);
        let mut sb = DiskBackedScrollback::with_config(config).unwrap();

        for i in 0..total_lines {
            sb.push_str(&format!("Line-{i}")).unwrap();
        }

        let cold = sb.cold_line_count();
        assert!(cold > 0, "session 1 must evict to cold tier: cold={cold}");
    }

    // Session 2: Reload from same path.
    {
        let config = DiskBackedScrollbackConfig::new(&cold_path)
            .with_hot_limit(5)
            .with_warm_limit(10)
            .with_block_size(5);
        let sb = DiskBackedScrollback::with_config(config).unwrap();

        let cold = sb.cold_line_count();
        assert!(cold > 0, "cold tier should have data after reload");
        assert_eq!(
            sb.line_count(),
            cold,
            "line_count ({}) must equal cold_line_count ({cold}) after reload \
             (hot and warm are empty)",
            sb.line_count()
        );

        for i in 0..cold {
            let line = sb
                .get_line(i)
                .unwrap_or_else(|e| panic!("get_line({i}) I/O error: {e}"))
                .unwrap_or_else(|| panic!("get_line({i}) returned None — line invisible"));
            assert!(
                line.to_string().starts_with("Line-"),
                "line {i} content mismatch: got '{line}'",
            );
        }

        let first = sb.get_line(0).unwrap().unwrap();
        assert_eq!(first.to_string(), "Line-0", "first line after reload");

        let last = sb.get_line(cold - 1).unwrap().unwrap();
        assert!(
            last.to_string().starts_with("Line-"),
            "last cold line content mismatch: got '{last}'",
        );
    }
}

/// Regression #5928: `line_count()` equals cold tier size after reload.
///
/// With `line_count: 0`, `line_count()` returned 0 despite cold tier having data.
#[test]
fn disk_backed_reload_line_count_matches_cold() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("lc.dtrm");

    let expected_cold = populate_cold_tier(&cold_path, 50);
    let sb = reload_from(&cold_path);

    assert_eq!(sb.cold_line_count(), expected_cold, "cold tier data lost");
    assert_eq!(
        sb.line_count(),
        expected_cold,
        "line_count ({}) must equal cold_line_count ({expected_cold}) after reload",
        sb.line_count()
    );
    assert_eq!(
        sb.hot_line_count(),
        0,
        "hot tier should be empty after reload"
    );
    assert_eq!(
        sb.warm_line_count(),
        0,
        "warm tier should be empty after reload"
    );
}

/// Regression #5928: `push_line` after reload increments from correct base.
///
/// With `line_count: 0`, pushed lines were inaccessible because `line_count`
/// was too small to route `get_line` past the cold tier to the hot tier.
#[test]
fn disk_backed_reload_push_increments_correctly() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("push.dtrm");

    let cold_n = populate_cold_tier(&cold_path, 50);
    let mut sb = reload_from(&cold_path);

    sb.push_str("NewLine-A").unwrap();
    sb.push_str("NewLine-B").unwrap();

    assert_eq!(
        sb.line_count(),
        cold_n + 2,
        "line_count should be cold({cold_n}) + 2 new lines"
    );

    // Oldest cold line still accessible at index 0.
    let first = sb.get_line(0).unwrap().unwrap();
    assert_eq!(first.to_string(), "Line-0", "oldest cold line at idx 0");

    // New lines appear after cold data.
    let new_a = sb.get_line(cold_n).unwrap().unwrap();
    assert_eq!(
        new_a.to_string(),
        "NewLine-A",
        "first pushed line after reload"
    );

    let new_b = sb.get_line(cold_n + 1).unwrap().unwrap();
    assert_eq!(
        new_b.to_string(),
        "NewLine-B",
        "second pushed line after reload"
    );
}

/// Regression #5928: `truncate(N/2)` after reload trims oldest cold lines.
///
/// With `line_count: 0`, `truncate(N/2)` was a no-op because `N/2 >= 0`.
#[test]
fn disk_backed_reload_truncate_keeps_newest() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("trunc.dtrm");

    let cold_n = populate_cold_tier(&cold_path, 50);
    let mut sb = reload_from(&cold_path);

    let keep = cold_n / 2;
    sb.truncate(keep).unwrap();

    assert_eq!(
        sb.line_count(),
        keep,
        "line_count should be {keep} after truncate"
    );

    // After truncate keeping `keep` newest, the new index 0 is the former
    // index (cold_n - keep).
    let first = sb.get_line(0).unwrap().unwrap();
    assert!(
        first.to_string().starts_with("Line-"),
        "first line after truncate should be a valid Line-N, got: '{first}'"
    );

    let last = sb.get_line(keep - 1).unwrap().unwrap();
    assert!(
        last.to_string().starts_with("Line-"),
        "last line after truncate should be valid, got: '{last}'"
    );

    // Out-of-bounds after truncate returns None.
    assert!(
        sb.get_line(keep).unwrap().is_none(),
        "index {keep} should be out-of-bounds after truncate to {keep}"
    );
}

/// Regression #5928: `remove_newest(1)` after reload removes from the correct end.
///
/// With `line_count: 0`, `remove_newest(1)` hit the `self.line_count == 0` early
/// return and silently did nothing, despite cold tier having real data.
#[test]
fn disk_backed_reload_remove_newest_preserves_oldest() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("rm_newest.dtrm");

    let cold_n = populate_cold_tier(&cold_path, 50);
    let mut sb = reload_from(&cold_path);

    sb.remove_newest(1).unwrap();

    assert_eq!(
        sb.line_count(),
        cold_n - 1,
        "line_count should be {cold_n}-1 after remove_newest(1)"
    );

    // Oldest cold line still accessible.
    let first = sb.get_line(0).unwrap().unwrap();
    assert_eq!(
        first.to_string(),
        "Line-0",
        "oldest line preserved after remove_newest"
    );

    // Former last line is gone.
    assert!(
        sb.get_line(cold_n - 1).unwrap().is_none(),
        "former last line should be removed"
    );
}

/// Regression #5928: `set_line_limit(Some(N/2))` after reload truncates immediately.
///
/// With `line_count: 0`, the `self.line_count > max` check always failed, so
/// `set_line_limit` never enforced the limit on existing cold data.
#[test]
fn disk_backed_reload_set_line_limit_truncates_immediately() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("limit.dtrm");

    let cold_n = populate_cold_tier(&cold_path, 50);
    let mut sb = reload_from(&cold_path);

    let limit = cold_n / 2;
    sb.set_line_limit(Some(limit));

    assert_eq!(
        sb.line_count(),
        limit,
        "line_count should be {limit} after set_line_limit({limit})"
    );

    // Newest lines (kept by truncation) are accessible.
    let last = sb.get_line(limit - 1).unwrap().unwrap();
    assert!(
        last.to_string().starts_with("Line-"),
        "last line after limit enforcement should be valid, got: '{last}'"
    );

    // Out-of-bounds returns None.
    assert!(
        sb.get_line(limit).unwrap().is_none(),
        "index {limit} should be out-of-bounds after limit enforcement"
    );
}
