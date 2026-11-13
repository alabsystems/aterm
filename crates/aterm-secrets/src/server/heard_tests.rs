// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn failed_epoch_writes_never_admit_a_run_or_create_a_clean_restart() {
    static FAIL: AtomicBool = AtomicBool::new(true);
    fn write(path: &Path, bytes: &[u8], mode: u32) -> crate::Result<()> {
        if FAIL.load(Ordering::SeqCst) {
            return Err(Error::new("injected snapshot flush failure"));
        }
        write_snapshot(path, bytes, mode)
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let mut history = History::new();
    let start = 1_800_000_000;
    {
        let _unavailable = aterm_durable::seam::FailingDirs::with(aterm_durable::seam::ENOTSUP);
        assert!(
            Coverage::begin(root.path(), start, &mut history).is_err(),
            "an unavailable directory flush admitted a run"
        );
    }
    // The failed flush may already have renamed DIRTY, but no read was admitted.
    std::fs::remove_file(root.path().join(SNAPSHOT)).unwrap();
    assert!(Coverage::begin_using(root.path(), start, &mut history, write).is_err());
    assert!(
        !root.path().join(SNAPSHOT).exists(),
        "a failed DIRTY write admitted a run"
    );
    FAIL.store(false, Ordering::SeqCst);
    let mut coverage = Coverage::begin_using(root.path(), start, &mut history, write).unwrap();
    assert_eq!(coverage.complete_since(start + WINDOW), Some(start));
    let dirty = std::fs::read(root.path().join(SNAPSHOT)).unwrap();
    FAIL.store(true, Ordering::SeqCst);
    assert!(coverage.finish(&history, start + WINDOW).is_err());
    assert_eq!(std::fs::read(root.path().join(SNAPSHOT)).unwrap(), dirty);
    let mut restarted = Coverage::begin(root.path(), start + WINDOW + 1, &mut history).unwrap();
    assert_eq!(
        restarted.complete_since(start + WINDOW + 1),
        None,
        "a failed CLEAN write preserved coverage"
    );
    assert_eq!(
        restarted.complete_since(start + 2 * WINDOW + 1),
        Some(start + WINDOW + 1)
    );
    restarted.finish(&history, start + 2 * WINDOW + 1).unwrap();
}
