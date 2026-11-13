// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
#[cfg(unix)]
fn pending_shim_rename_wakes_waiter_and_exposes_tool() {
    use std::os::unix::fs::symlink;

    let dir = crate::testdir::dir("atpkg-pending-wake-");
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let layout = Layout {
        prefix: dir.clone(),
    };
    let shim = layout.bin_dir().join("ty");
    std::fs::write(&shim, b"#!/bin/sh\n# atpkg pending\n").unwrap();
    let target = dir.join("store/ty/44/bin/ty");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, b"#!/bin/sh\nexit 0\n").unwrap();

    let mut wake = PendingShimWake::new(&layout);
    assert!(
        wake.watch.is_some(),
        "the test requires a live directory watch"
    );
    assert!(crate::which(&layout, "ty").is_none());
    let stage = crate::testdir::dir("atpkg-pending-wake-stage-");
    let staged = stage.join("stage-ty");
    symlink(&target, &staged).unwrap();
    std::fs::rename(staged, &shim).unwrap();
    std::fs::remove_dir_all(&stage).unwrap();

    assert!(wake.wait(aterm_spec::wait::MUST_HAPPEN));
    assert_eq!(crate::which(&layout, "ty"), Some(target));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_retired_promise_or_a_landed_progress_file_wakes_the_waiter() {
    let dir = crate::testdir::dir("atpkg-pending-wake-pass-");
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let layout = Layout {
        prefix: dir.clone(),
    };
    std::fs::write(layout.pass_owed_file(), b"{}").unwrap();
    let mut wake = PendingShimWake::new(&layout);
    assert!(
        wake.watch.is_some(),
        "the test requires a live directory watch"
    );
    std::fs::remove_file(layout.pass_owed_file()).unwrap();
    assert!(
        wake.wait(aterm_spec::wait::MUST_HAPPEN),
        "the promise retired"
    );
    let stage = crate::testdir::dir("atpkg-pending-wake-pass-stage-");
    let staged = stage.join("progress.json.tmp");
    std::fs::write(&staged, b"{}").unwrap();
    std::fs::rename(&staged, layout.progress_file()).unwrap();
    std::fs::remove_dir_all(&stage).unwrap();
    assert!(
        wake.wait(aterm_spec::wait::MUST_HAPPEN),
        "the pass's file landed"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn retiring_takes_the_watches_off_and_keeps_the_descriptor() {
    let dir = crate::testdir::dir("atpkg-pending-wake-retire-");
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let layout = Layout {
        prefix: dir.clone(),
    };
    let mut wake = PendingShimWake::new(&layout);
    let fd = wake
        .watch
        .as_ref()
        .expect("the test requires a live directory watch")
        .watcher
        .inotify_fd();
    let watches = || {
        std::fs::read_to_string(format!("/proc/self/fdinfo/{fd}"))
            .expect("the descriptor is open")
            .lines()
            .filter(|line| line.starts_with("inotify wd:"))
            .count()
    };
    assert_eq!(watches(), 2, "one watch on bin/, one on the prefix");
    wake.retire();
    assert_eq!(watches(), 0, "no watch is left for the close to drop");
    std::fs::remove_dir_all(&dir).unwrap();
}
