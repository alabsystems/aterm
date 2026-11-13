// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_empty_marker_is_removed_at_once_and_a_crash_record_is_kept() {
    let dir = std::env::temp_dir().join(format!("aterm-marker-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let clean = dir.join("crash-signal-1-1.log");
    let crashed = dir.join("crash-signal-2-2.log");
    std::fs::write(&clean, b"").unwrap();
    std::fs::write(&crashed, "aterm: fatal signal 11\n").unwrap();

    assert!(remove_marker_if_empty(&clean), "an empty marker is removed");
    assert!(!clean.exists(), "gone at once, not merely delete-pending");
    assert!(!remove_marker_if_empty(&crashed), "a crash record is kept");
    assert!(crashed.exists());
    assert!(!remove_marker_if_empty(&dir.join("crash-signal-3-3.log")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(windows)]
#[test]
fn release_without_an_armed_marker_is_a_harmless_no_op() {
    let _one_at_a_time = PROCESS_MARKER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    remove_marker_on_clean_exit();
    remove_marker_on_clean_exit();
    assert_eq!(
        imp_windows::MARKER_HANDLE.load(std::sync::atomic::Ordering::SeqCst),
        -1
    );
}

/// The clean exit end to end, armed for real in the real log dir: while armed the marker is
/// held against deletion, and the release closes the handle and removes the file.
#[cfg(windows)]
#[test]
fn the_clean_exit_takes_the_armed_marker_with_it() {
    use std::sync::atomic::Ordering;
    let _one_at_a_time = PROCESS_MARKER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    imp_windows::install();
    let path = imp_windows::MARKER_PATH.get().cloned();
    if imp_windows::MARKER_HANDLE.load(Ordering::SeqCst) != -1 {
        let path = path.as_ref().expect("an open marker has its path parked");
        assert!(path.exists(), "armed: {}", path.display());
        assert!(
            std::fs::remove_file(path).is_err(),
            "held against a sibling's sweep"
        );
    }
    remove_marker_on_clean_exit();
    assert_eq!(imp_windows::MARKER_HANDLE.load(Ordering::SeqCst), -1);
    if let Some(path) = path {
        assert!(
            !path.exists(),
            "the clean exit took its own marker: {}",
            path.display()
        );
    }
}
