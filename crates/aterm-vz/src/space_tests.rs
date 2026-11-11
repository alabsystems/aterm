// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_volume_under_the_floor_is_named_with_its_figure() {
    let at = |bytes| Free {
        bytes,
        volume: "/System/Volumes/Data (container:disk3)".into(),
    };
    assert_eq!(below_bring_up(&at(BRING_UP_FLOOR)), None);
    assert!(below_bring_up(&at(BRING_UP_FLOOR - 1)).is_some());
    let words = below_bring_up(&at(12 << 30)).unwrap();
    assert!(
        words.starts_with("NOT RUN — the volume holding aterm-linux.img, /System/Volumes/Data")
            && words.contains("has 12.0 GiB free, under the 20.0 GiB a bring-up needs")
            && words.ends_with("nothing was cloned or baked"),
        "{words}"
    );
    assert_eq!(gib(4_617_089_843), "4.3 GiB");
}

#[test]
fn a_running_vm_below_its_floor_is_stopped_in_the_designs_words() {
    let at = |bytes| Free {
        bytes,
        volume: "/System/Volumes/Data (container:disk3)".into(),
    };
    assert_eq!(below_run(&at(RUN_FLOOR)), None);
    assert_eq!(
        below_run(&at(3 << 30)).as_deref(),
        Some("the volume holding aterm-linux.img has 3.0 GiB free; VM stopped")
    );
    assert_eq!(
        below_run(&at(12 << 30)),
        None,
        "the negative control: a run above its floor goes on, below the bring-up's"
    );
}

#[test]
fn a_path_not_there_yet_is_read_through_its_resolved_ancestor() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let real = std::fs::canonicalize(dir.path()).unwrap();
    let link = real.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    // Unresolved, a path under `/tmp` matches the root volume's mount, not the volume
    // mounted under `/private/tmp`.
    assert_eq!(
        resolved(&link.join("live/aterm-linux.img")),
        real.join("live/aterm-linux.img")
    );
    assert_eq!(resolved(&link), real);
    assert_eq!(resolved(Path::new("relative/x")), Path::new("relative/x"));
}

#[test]
fn this_hosts_scratch_volume_reads() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let free = read(dir.path()).unwrap();
    assert!(free.bytes > 0 && !free.volume.is_empty(), "{free:?}");
}
