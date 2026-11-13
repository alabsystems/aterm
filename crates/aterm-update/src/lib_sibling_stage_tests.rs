// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{StagedNotify, UpdateStatus, announce_sibling_stage};
use std::sync::{Arc, Mutex};

fn staged(build: Option<u64>) -> UpdateStatus {
    let mut status = UpdateStatus::empty(true, 81, "another aterm process completed".into());
    status.staged_build = build;
    status.staged_version = build.map(|b| format!("0.{b}.0"));
    status
}

#[test]
fn a_skip_announces_a_siblings_newer_stage_once_per_build() {
    let calls: Arc<Mutex<Vec<(u64, String)>>> = Arc::default();
    let sink = Arc::clone(&calls);
    let hook: StagedNotify = Box::new(move |build, version| {
        sink.lock().unwrap().push((build, version));
    });
    let mut announced = None;
    let newer = staged(Some(85));
    assert!(announce_sibling_stage(
        &mut announced,
        81,
        Some(&newer),
        Some(&hook)
    ));
    assert!(!announce_sibling_stage(
        &mut announced,
        81,
        Some(&newer),
        Some(&hook)
    ));
    assert_eq!(*calls.lock().unwrap(), vec![(85, "0.85.0".to_string())]);
    let newest = staged(Some(86));
    assert!(announce_sibling_stage(
        &mut announced,
        81,
        Some(&newest),
        Some(&hook)
    ));
    assert_eq!(calls.lock().unwrap().len(), 2);

    let mut fresh = None;
    for (label, current, status) in [
        ("same build as running", 85, staged(Some(85))),
        ("older than running", 90, staged(Some(85))),
        ("nothing staged", 81, staged(None)),
    ] {
        assert!(
            !announce_sibling_stage(&mut fresh, current, Some(&status), Some(&hook)),
            "{label}"
        );
    }
    assert!(
        !announce_sibling_stage(&mut fresh, 81, None, Some(&hook)),
        "no status"
    );
    assert!(
        !announce_sibling_stage(&mut fresh, 81, Some(&newer), None),
        "no hook"
    );
    assert_eq!(fresh, None, "nothing was latched without an announcement");
    assert_eq!(calls.lock().unwrap().len(), 2);
}
