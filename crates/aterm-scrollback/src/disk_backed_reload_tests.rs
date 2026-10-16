// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Cold-tier reload: each test fills the cold tier, drops the scrollback,
//! reloads it with `with_config` and checks that the reloaded `line_count`
//! drives one operation correctly.

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

/// Every cold line is readable after drop and reload from the same path.
#[test]
fn disk_backed_cold_data_persists_on_reload() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("persist.dtrm");

    let total_lines = 50;

    // Session 1: populate every tier, cold included.
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

    // Session 2: reload from the same path.
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

/// `line_count()` equals the cold tier size after reload.
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

/// Lines pushed after reload land after the cold lines.
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

    let first = sb.get_line(0).unwrap().unwrap();
    assert_eq!(first.to_string(), "Line-0", "oldest cold line at idx 0");

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

/// `truncate(N/2)` after reload trims the oldest cold lines.
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

    assert!(
        sb.get_line(keep).unwrap().is_none(),
        "index {keep} should be out-of-bounds after truncate to {keep}"
    );
}

/// `remove_newest(1)` after reload removes the newest cold line.
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

    let first = sb.get_line(0).unwrap().unwrap();
    assert_eq!(
        first.to_string(),
        "Line-0",
        "oldest line preserved after remove_newest"
    );

    assert!(
        sb.get_line(cold_n - 1).unwrap().is_none(),
        "former last line should be removed"
    );
}

/// `set_line_limit(Some(N/2))` after reload truncates the reloaded cold lines.
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

    let last = sb.get_line(limit - 1).unwrap().unwrap();
    assert!(
        last.to_string().starts_with("Line-"),
        "last line after limit enforcement should be valid, got: '{last}'"
    );

    assert!(
        sb.get_line(limit).unwrap().is_none(),
        "index {limit} should be out-of-bounds after limit enforcement"
    );
}
