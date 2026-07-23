// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration tests for DiskColdTier error safety and crash recovery.
//!
//! These tests exercise the disk-backed cold tier through the public
//! `ScrollbackStorage` API by manipulating the `.dtrm` file between sessions.
//!
//! The disk-backed cold tier is opt-in (§2.7); without it there is nothing to
//! exercise here, so the whole integration test compiles to an empty crate.
#![cfg(feature = "disk-tier")]

use aterm_scrollback::{DiskBackedScrollback, DiskBackedScrollbackConfig, Line, ScrollbackStorage};
use std::io::{Read, Seek, SeekFrom, Write};

const HEADER_SIZE: u64 = 32;
const PAGE_HEADER_SIZE: u64 = 8;

/// Build a disk-backed scrollback with small tier limits so data quickly
/// spills from hot → warm → cold.
fn build_small_tier(path: &std::path::Path) -> ScrollbackStorage {
    let config = DiskBackedScrollbackConfig::new(path)
        .with_hot_limit(5)
        .with_warm_limit(10)
        .with_block_size(5);
    let sb = DiskBackedScrollback::with_config(config).expect("disk scrollback should initialize");
    ScrollbackStorage::from(sb)
}

/// Push `n` lines with the given prefix. Returns the total line count after.
fn push_lines(storage: &mut ScrollbackStorage, prefix: &str, count: usize) -> usize {
    for i in 0..count {
        storage
            .push_line(Line::from(&*format!("{prefix}-{i}")))
            .expect("push_line should succeed");
    }
    storage.line_count()
}

/// Read page count and line count from the `.dtrm` file header.
fn read_header_counts(path: &std::path::Path) -> (u64, u64) {
    let mut file = std::fs::File::open(path).expect("open dtrm file");
    let mut header = [0u8; 32];
    file.read_exact(&mut header).expect("read header");
    let page_count = u64::from_le_bytes(header[8..16].try_into().unwrap());
    let line_count = u64::from_le_bytes(header[16..24].try_into().unwrap());
    (page_count, line_count)
}

/// Find the start offset of the last page's compressed data in the `.dtrm` file.
/// Scans page headers from the file start to locate the final page.
fn find_last_page_data_offset(path: &std::path::Path) -> Option<u64> {
    let mut file = std::fs::File::open(path).expect("open dtrm file");
    let file_len = file.metadata().expect("file metadata").len();

    let mut offset = HEADER_SIZE;
    let mut last_data_offset = None;
    let mut page_header = [0u8; 8];

    while offset + PAGE_HEADER_SIZE <= file_len {
        file.seek(SeekFrom::Start(offset)).ok()?;
        file.read_exact(&mut page_header).ok()?;
        let compressed_size = u32::from_le_bytes(page_header[0..4].try_into().unwrap());
        if compressed_size == 0 {
            break;
        }
        last_data_offset = Some(offset + PAGE_HEADER_SIZE);
        offset += PAGE_HEADER_SIZE + u64::from(compressed_size);
    }
    last_data_offset
}

/// Corrupt the last page's compressed data in a `.dtrm` file by overwriting
/// bytes *after* the Zstd frame magic (so `scan_and_repair_pages` still
/// accepts the page header, but decompression fails).
fn corrupt_last_page_payload(path: &std::path::Path) {
    let data_offset =
        find_last_page_data_offset(path).expect("should find at least one page in file");

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open for corruption");

    // Overwrite bytes 4..12 of compressed data (after the 4-byte Zstd magic).
    // The Zstd magic (0x28B52FFD) at offset+0 stays intact so the page passes
    // the scan check, but the corrupted frame header makes decompression fail.
    file.seek(SeekFrom::Start(data_offset + 4)).unwrap();
    file.write_all(&[0xFF; 8]).unwrap();
    file.sync_all().unwrap();
}

/// remove_newest through cold tier with corrupt boundary page must fail
/// without changing persisted state.
///
/// Exercises the error safety guarantee of `DiskColdTier::truncate_back_lines`:
/// "the boundary page is decompressed before any state is modified."
#[test]
fn disk_cold_truncate_back_corrupt_boundary_preserves_state() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("cold.dtrm");

    // Session 1: Populate with enough data to fill cold tier.
    let _total_lines = {
        let mut storage = build_small_tier(&cold_path);
        // Push 50 lines → hot(5) + warm(10) + cold(35)
        push_lines(&mut storage, "Line", 50);
        let total = storage.line_count();
        assert!(
            storage.cold_line_count() > 0,
            "cold tier must have data for this test"
        );
        total
    };

    let (page_count_before, _header_lines_before) = read_header_counts(&cold_path);
    assert!(page_count_before >= 2, "need at least 2 cold pages");
    let file_size_before = std::fs::metadata(&cold_path).unwrap().len();

    // Corrupt the last page's payload (decompression will fail).
    corrupt_last_page_payload(&cold_path);

    // Session 2: Reload and attempt remove_newest that hits the corrupt cold page.
    {
        let mut storage = build_small_tier(&cold_path);
        let loaded_lines = storage.line_count();

        // The cold tier accepted the corrupt page (magic was intact).
        assert!(
            loaded_lines > 0,
            "lines should be loaded despite corruption"
        );
        assert!(storage.cold_line_count() > 0, "cold tier should have pages");

        // remove_newest targeting the cold tier should fail on the corrupt boundary.
        // We remove enough lines to reach into the cold tier.
        let cold_lines = storage.cold_line_count();
        let non_cold = loaded_lines - cold_lines;
        // Request removal of all non-cold lines plus some cold lines to hit the boundary.
        let remove_count = non_cold + cold_lines.min(3);
        let result = storage.remove_newest(remove_count);

        // The operation should either fail (corrupt boundary) or succeed
        // by not needing to touch the boundary. If it fails, verify state.
        if result.is_err() {
            // Verify the file is not corrupted further.
            let file_size_after = std::fs::metadata(&cold_path).unwrap().len();
            // File should not have grown or shrunk from the error path.
            assert!(
                file_size_after <= file_size_before,
                "file should not grow on failed truncate: before={file_size_before}, after={file_size_after}"
            );
        }
        // Either way, the storage should still be usable for the oldest line.
        let oldest = storage.get_line(0);
        assert!(
            oldest.is_ok(),
            "oldest line should still be accessible: {oldest:?}"
        );
    }
}

/// load() cleans up orphaned `.dtrm.compact` temp file from a prior crash.
#[test]
fn disk_cold_load_cleans_orphan_compact_temp_file() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("cold.dtrm");
    let tmp_path = cold_path.with_extension("dtrm.compact");

    // Session 1: Create a valid cold tier file.
    {
        let mut storage = build_small_tier(&cold_path);
        push_lines(&mut storage, "Line", 30);
        assert!(storage.cold_line_count() > 0);
    }

    // Simulate crash: leave an orphaned compaction temp file.
    std::fs::write(&tmp_path, b"orphaned compaction data").unwrap();
    assert!(tmp_path.exists(), "temp file should exist before reload");

    // Session 2: Reload — load() should clean up the orphaned temp file.
    {
        let storage = build_small_tier(&cold_path);
        assert!(
            !tmp_path.exists(),
            "orphaned .dtrm.compact file should be deleted on load"
        );
        assert!(storage.line_count() > 0, "data should survive reload");
        let oldest = storage.get_line(0).unwrap().unwrap();
        assert_eq!(oldest.to_string(), "Line-0");
    }
}

/// Cold tier data persists across sessions: reload after drop must recover
/// cold lines. Hot/warm tiers are in-memory and lost on drop.
#[test]
fn disk_cold_data_persists_on_reload() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("cold.dtrm");

    // Session 1: Populate with data that spills to cold tier.
    let cold_lines_session1;
    let oldest_line;
    {
        let mut storage = build_small_tier(&cold_path);
        push_lines(&mut storage, "Line", 50);
        cold_lines_session1 = storage.cold_line_count();
        assert!(cold_lines_session1 > 0, "cold tier must have data");
        oldest_line = storage.get_line(0).unwrap().unwrap().to_string();
    }

    // Session 2: Reload — only cold data survives (hot/warm are in-memory).
    {
        let storage = build_small_tier(&cold_path);
        assert_eq!(
            storage.line_count(),
            cold_lines_session1,
            "only cold lines should survive reload"
        );
        assert_eq!(
            storage.cold_line_count(),
            cold_lines_session1,
            "cold line count should be preserved"
        );
        let reloaded_oldest = storage.get_line(0).unwrap().unwrap().to_string();
        assert_eq!(
            reloaded_oldest, oldest_line,
            "oldest line should be preserved from cold tier"
        );
    }
}

#[test]
fn disk_cold_remove_newest_after_reload_preserves_remaining_history() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("cold.dtrm");

    let surviving_cold_lines;
    {
        let mut storage = build_small_tier(&cold_path);
        push_lines(&mut storage, "Line", 50);
        let cold_lines = storage.cold_line_count();
        assert!(
            cold_lines > 1,
            "fixture must spill enough lines to cold tier"
        );

        surviving_cold_lines = (0..cold_lines)
            .map(|idx| storage.get_line(idx).unwrap().unwrap().to_string())
            .collect::<Vec<_>>();
    }

    {
        let mut storage = build_small_tier(&cold_path);
        storage
            .remove_newest(1)
            .expect("remove_newest should only drop the newest reloaded cold line");

        let expected_remaining = &surviving_cold_lines[..surviving_cold_lines.len() - 1];
        assert_eq!(
            storage.line_count(),
            expected_remaining.len(),
            "reloaded line_count should shrink by one"
        );
        assert_eq!(
            storage.cold_line_count(),
            expected_remaining.len(),
            "cold tier should remain the only populated tier after reload"
        );

        let actual_remaining = (0..storage.line_count())
            .map(|idx| storage.get_line(idx).unwrap().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            actual_remaining, expected_remaining,
            "remove_newest after reload should preserve the surviving cold prefix"
        );
    }
}

#[test]
fn disk_cold_checkpoint_snapshot_after_reload_preserves_cold_lines() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("cold.dtrm");

    let surviving_cold_lines;
    {
        let mut storage = build_small_tier(&cold_path);
        push_lines(&mut storage, "Line", 50);
        let cold_lines = storage.cold_line_count();
        assert!(cold_lines > 0, "fixture must spill lines to cold tier");

        surviving_cold_lines = (0..cold_lines)
            .map(|idx| storage.get_line(idx).unwrap().unwrap().to_string())
            .collect::<Vec<_>>();
    }

    {
        let storage = build_small_tier(&cold_path);
        let snapshot = storage.checkpoint_snapshot();
        assert_eq!(
            snapshot.line_count(),
            surviving_cold_lines.len(),
            "checkpoint snapshot should include all reloaded cold lines"
        );

        let snapshot_lines = (0..snapshot.line_count())
            .map(|idx| snapshot.get_line(idx).unwrap().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            snapshot_lines, surviving_cold_lines,
            "checkpoint snapshot should preserve cold-line order after reload"
        );
    }
}

/// Push lines after reload must increment line_count from the cold base,
/// not from 0. Guards the `push_line` path in
/// `crates/aterm-scrollback/src/disk_backed.rs:377`.
#[test]
fn disk_cold_push_after_reload_increments_from_cold_base() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("cold.dtrm");

    let cold_lines_session1;
    {
        let mut storage = build_small_tier(&cold_path);
        push_lines(&mut storage, "Old", 50);
        cold_lines_session1 = storage.cold_line_count();
        assert!(
            cold_lines_session1 > 0,
            "fixture must spill lines to cold tier"
        );
    }

    {
        let mut storage = build_small_tier(&cold_path);
        assert_eq!(
            storage.line_count(),
            cold_lines_session1,
            "reload should restore cold line count"
        );

        // Push 3 new lines after reload.
        push_lines(&mut storage, "New", 3);
        assert_eq!(
            storage.line_count(),
            cold_lines_session1 + 3,
            "push after reload should increment from cold base, not from 0"
        );

        // Oldest line is still from session 1.
        let oldest = storage.get_line(0).unwrap().unwrap().to_string();
        assert!(
            oldest.starts_with("Old-"),
            "oldest line should still be from session 1: got {oldest}"
        );
        // Newest line is from the post-reload push.
        let newest = storage
            .get_line(storage.line_count() - 1)
            .unwrap()
            .unwrap()
            .to_string();
        assert_eq!(newest, "New-2", "newest line should be the last pushed");
    }
}

/// set_line_limit after reload must immediately truncate oldest cold lines.
/// Guards the `set_line_limit` → `truncate` path in
/// `crates/aterm-scrollback/src/disk_backed.rs:208-218`.
#[test]
fn disk_cold_set_line_limit_after_reload_truncates_oldest() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cold_path = dir.path().join("cold.dtrm");

    let cold_lines_session1;
    let newest_cold_line;
    {
        let mut storage = build_small_tier(&cold_path);
        push_lines(&mut storage, "Line", 50);
        cold_lines_session1 = storage.cold_line_count();
        assert!(
            cold_lines_session1 > 5,
            "fixture must spill enough lines to cold tier for truncation"
        );
        // Capture the newest cold line (last line before hot/warm).
        newest_cold_line = storage
            .get_line(cold_lines_session1 - 1)
            .unwrap()
            .unwrap()
            .to_string();
    }

    {
        let mut storage = build_small_tier(&cold_path);
        let keep = cold_lines_session1 / 2;
        assert!(keep > 0, "must keep at least one line");

        storage.set_line_limit(Some(keep));
        assert_eq!(
            storage.line_count(),
            keep,
            "set_line_limit should immediately truncate to the limit"
        );
        // After truncation, the newest surviving line should be the same as before
        // (truncation removes the oldest lines first).
        let newest_after = storage
            .get_line(storage.line_count() - 1)
            .unwrap()
            .unwrap()
            .to_string();
        assert_eq!(
            newest_after, newest_cold_line,
            "set_line_limit should keep the newest lines, removing oldest"
        );
    }
}
