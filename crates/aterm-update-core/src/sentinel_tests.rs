// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn sentinel(label: &str) -> (Sentinel, aterm_tempfile::TempDir) {
    let dir = crate::test_scratch_dir(&format!("sentinel-{label}"));
    let path = dir.path().join("boot.sentinel");
    (Sentinel::new(path), dir)
}

/// Which flush each sentinel write takes (aterm-durable's counting seam): the boot
/// sentinel's arm and launch counts stay durable unless the caller orders the arm; a
/// named give-back is only ordered; an ordered counter orders every write.
#[test]
fn each_sentinel_write_names_its_flush() {
    use aterm_durable::seam::Recording;
    const O: Flush = Flush::Ordered;
    const D: Flush = Flush::Durable;
    let (s, dir) = sentinel("flush-tiers");
    let me = LaunchName {
        pid: 7,
        started: (1, 2),
    };
    let writes = |flush: Flush, n: usize| vec![flush; n * 2];
    let recording = Recording::start();
    s.arm(9).unwrap();
    assert_eq!(
        recording.take(),
        writes(D, 1),
        "a durable arm: the file, the entry"
    );
    s.arm_with(9, O).unwrap();
    assert_eq!(recording.take(), writes(O, 1), "an ordered arm");
    assert_eq!(s.observe_launch_by(9, Some(me)).unwrap(), 1);
    assert_eq!(recording.take(), writes(D, 1), "a launch count");
    assert_eq!(s.forgive_launch_of(9, 7, Some((1, 2))).unwrap(), Some(0));
    assert_eq!(recording.take(), writes(O, 1), "a named give-back");
    let counter = Sentinel::ordered(dir.path().join("foreign-trial"));
    counter.restart_count(9).unwrap();
    assert_eq!(counter.observe_launch(9).unwrap(), 1);
    assert_eq!(recording.take(), writes(O, 2), "an ordered counter");
}

#[test]
fn healthy_boot_never_reverts() {
    let (s, _dir) = sentinel("healthy");
    s.arm(1235).unwrap();
    assert_eq!(s.observe_launch(1235).unwrap(), 1);
    s.confirm().unwrap();
    assert!(!s.should_revert(1235, 2), "a confirmed boot never reverts");
    assert_eq!(s.read_state(), None);
}

#[test]
fn crash_loop_triggers_revert_at_max_attempts() {
    let (s, _dir) = sentinel("crash");
    s.arm(1235).unwrap();
    assert_eq!(s.observe_launch(1235).unwrap(), 1);
    assert!(
        !s.should_revert(1235, 2),
        "one unconfirmed boot is not yet a loop"
    );
    assert_eq!(s.observe_launch(1235).unwrap(), 2);
    assert!(s.should_revert(1235, 2), "two unconfirmed boots ⇒ revert");
}

/// A RESUMED ARM STARTS AT ITS COUNT: `arm_at(11, 2)` reads `(11, 2)` with no launch names,
/// so one more counted launch reaches a threshold of 3, while `arm_with` still starts at 0.
/// NEGATIVE CONTROL: an `arm_at` that wrote 0 (as `arm_with` does) fails the first read and
/// leaves `should_revert(11, 3)` false after the launch.
#[test]
fn a_resumed_arm_starts_at_its_count() {
    let (s, _dir) = sentinel("resumed");
    s.arm_at(11, 2, Flush::Durable).unwrap();
    assert_eq!(s.read_state(), Some((11, 2)), "the carried count");
    assert!(s.named_launches(11).is_empty(), "no launch is named yet");
    assert!(
        !s.should_revert(11, 3),
        "two carried launches are not three"
    );
    assert_eq!(s.observe_launch(11).unwrap(), 3);
    assert!(
        s.should_revert(11, 3),
        "the resumed trial's next launch reverts"
    );
    s.arm_with(11, Flush::Durable).unwrap();
    assert_eq!(s.read_state(), Some((11, 0)), "a fresh arm starts at zero");
}

#[test]
fn a_launch_the_killer_forgives_is_not_a_crash() {
    let (s, _dir) = sentinel("forgive");
    s.arm(1235).unwrap();
    assert_eq!(s.observe_launch(1235).unwrap(), 1);
    assert_eq!(s.forgive_launch(1235).unwrap(), 0);
    assert_eq!(s.read_state(), Some((1235, 0)));
    for _ in 0..3 {
        assert_eq!(s.observe_launch(1235).unwrap(), 1);
        assert_eq!(s.forgive_launch(1235).unwrap(), 0);
    }
    assert!(
        !s.should_revert(1235, 3),
        "forgiven launches are not a loop"
    );
    assert_eq!(s.observe_launch(1235).unwrap(), 1);
    assert_eq!(s.observe_launch(1235).unwrap(), 2);
    assert_eq!(
        s.forgive_launch(1235).unwrap(),
        1,
        "an unforgiven died candidate keeps its count"
    );
    assert_eq!(s.forgive_launch(1235).unwrap(), 0);
    assert_eq!(s.forgive_launch(1235).unwrap(), 0, "floored at zero");
    assert_eq!(s.forgive_launch(9999).unwrap(), 0);
    assert_eq!(s.read_state(), Some((1235, 0)));
}

#[test]
fn sentinel_for_another_build_is_preserved_and_not_counted() {
    let (s, _dir) = sentinel("stale");
    s.arm(1000).unwrap();
    assert_eq!(s.observe_launch(2000).unwrap(), 0);
    assert_eq!(s.read_state(), Some((1000, 0)));
    assert!(!s.should_revert(2000, 2));
}

/// A failed flush of the sentinel's folder fails the arm (the swap it precedes does not
/// happen) and takes its record back, a stale one it replaced with it, so a failed arm
/// leaves no build armed for a trial its caller never starts; it fails no count, give-back
/// or restarted count, which are in place once renamed. A folder the volume cannot flush
/// fails nothing. With the record left in place, as the arm's first policy did, a failed
/// arm left the build armed.
#[test]
fn a_failed_folder_flush_fails_the_arm_and_not_a_count() {
    use aterm_durable::seam::{EIO, ENOTSUP, FailingDirs};
    let (s, dir) = sentinel("dir-flush-fails");
    s.arm(8).unwrap();
    {
        let _eio = FailingDirs::with(EIO);
        assert_eq!(
            s.arm_with(9, Flush::Ordered).map_err(|e| e.raw_os_error()),
            Err(Some(EIO)),
            "an arm not ordered ahead of the swap is no arm"
        );
        assert_eq!(s.read_state(), None, "the failed arm is taken back");
        assert!(!s.path.exists(), "and leaves no file behind");
        assert_eq!(s.arm(9).map_err(|e| e.raw_os_error()), Err(Some(EIO)));
        assert_eq!(s.read_state(), None);
    }
    s.arm(9).unwrap();
    {
        let _eio = FailingDirs::with(EIO);
        assert_eq!(s.observe_launch(9).unwrap(), 1, "the count is in place");
        assert_eq!(s.forgive_launch(9).unwrap(), 0, "and so is its give-back");
        assert_eq!(s.read_state(), Some((9, 0)));
        let me = LaunchName {
            pid: 7,
            started: (1, 2),
        };
        assert_eq!(s.observe_launch_by(9, Some(me)).unwrap(), 1);
        assert_eq!(
            s.forgive_launch_of(9, me.pid, Some(me.started)).unwrap(),
            Some(0)
        );
        assert!(s.named_launches(9).is_empty());
        let counter = Sentinel::ordered(dir.path().join("foreign-trial"));
        counter
            .restart_count(9)
            .expect("a restarted count is in place too");
        assert_eq!(counter.read_state(), Some((9, 0)));
    }
    {
        let _refused = FailingDirs::with(ENOTSUP);
        s.arm(10)
            .expect("a folder the volume cannot flush is no failure");
    }
    assert_eq!(s.read_state(), Some((10, 0)));
}

#[test]
fn a_prelanding_arm_failure_preserves_the_previous_record() {
    let (s, dir) = sentinel("arm-temp-obstructed");
    s.arm(8).unwrap();
    let tmp = dir
        .path()
        .join(format!(".boot-sentinel.tmp-{}", std::process::id()));
    std::fs::create_dir(&tmp).unwrap();
    assert!(s.arm(9).is_err());
    assert_eq!(s.read_state(), Some((8, 0)));
    assert!(tmp.is_dir());
    std::fs::remove_dir(&tmp).unwrap();

    let dir_target = Sentinel::new(dir.path().join("obstructed-target"));
    std::fs::create_dir(&dir_target.path).unwrap();
    assert!(dir_target.arm(9).is_err());
    assert!(
        !tmp.exists(),
        "a failed rename removes the temporary sentinel file"
    );
}

#[test]
fn corrupt_sentinel_does_not_force_a_revert() {
    let (s, _dir) = sentinel("corrupt");
    std::fs::write(&s.path, "not a sentinel").unwrap();
    assert_eq!(s.read_state(), None);
    assert!(
        !s.should_revert(1235, 1),
        "garbage never forces the disruptive revert"
    );
}

#[test]
fn only_a_present_marker_that_does_not_read_is_unreadable() {
    let (s, dir) = sentinel("unreadable");
    assert!(
        !s.unreadable(),
        "no marker is no trial, not an unreadable one"
    );
    s.arm(1235).unwrap();
    assert!(!s.unreadable(), "an armed marker reads");
    std::fs::write(&s.path, [0u8; 16]).unwrap();
    assert!(
        s.unreadable(),
        "a zero-filled marker (a cut write) does not"
    );
    s.confirm().unwrap();
    assert!(!s.unreadable(), "a confirmed marker is gone");
    std::fs::create_dir(&s.path).unwrap();
    assert!(
        !s.unreadable(),
        "a directory at the path is no marker that does not read"
    );
    #[cfg(not(unix))]
    let _ = &dir;
    #[cfg(unix)]
    {
        std::fs::remove_dir(&s.path).unwrap();
        let decoy = dir.path().join("decoy.sentinel");
        std::fs::write(&decoy, "1235 5").unwrap();
        std::os::unix::fs::symlink(&decoy, &s.path).unwrap();
        assert_eq!(
            s.read_state(),
            None,
            "a symlinked sentinel must never be followed"
        );
        assert!(!s.unreadable(), "a symlink is not an unreadable marker");
    }
}

#[test]
fn named_launches_and_forgive_launch_of_match_pid_start_time_and_cap_at_32() {
    let (s, _dir) = sentinel("named-launches");
    assert!(s.named_launches(42).is_empty());
    assert_eq!(s.forgive_launch_of(42, 10, None).unwrap(), None);

    s.arm(42).unwrap();
    let first = LaunchName {
        pid: 100,
        started: (10, 20),
    };
    let reused_pid = LaunchName {
        pid: 100,
        started: (30, 40),
    };
    assert_eq!(s.observe_launch_by(42, Some(first)).unwrap(), 1);
    assert_eq!(s.observe_launch_by(42, Some(reused_pid)).unwrap(), 2);
    assert_eq!(s.named_launches(42), vec![first, reused_pid]);
    assert!(s.named_launches(99).is_empty(), "other build sees none");
    assert_eq!(s.forgive_launch_of(99, 100, None).unwrap(), None);
    assert_eq!(
        s.forgive_launch_of(42, 100, Some((99, 99))).unwrap(),
        None,
        "wrong start time leaves count and names untouched"
    );
    assert_eq!(s.read_state(), Some((42, 2)));
    assert_eq!(
        s.forgive_launch_of(42, 100, Some((30, 40))).unwrap(),
        Some(1),
        "exact (pid, started) removes the matching entry"
    );
    assert_eq!(s.named_launches(42), vec![first]);
    assert_eq!(
        s.forgive_launch_of(42, 100, None).unwrap(),
        Some(0),
        "started = None matches remaining pid entry"
    );
    assert!(s.named_launches(42).is_empty());

    // Malformed name tokens are ignored without corrupting the count or valid names.
    std::fs::write(
        &s.path,
        "42 3 7:1:2 bad 8:9:10:extra +9:1:2 7:+1:2 11:12:13",
    )
    .unwrap();
    assert_eq!(s.read_state(), Some((42, 3)));
    assert_eq!(
        s.named_launches(42),
        vec![
            LaunchName {
                pid: 7,
                started: (1, 2),
            },
            LaunchName {
                pid: 11,
                started: (12, 13),
            },
        ]
    );
    std::fs::write(&s.path, "+42 3").unwrap();
    assert_eq!(s.read_state(), None);
    std::fs::write(&s.path, "42 +3").unwrap();
    assert_eq!(s.read_state(), None);

    // Exceeding MAX_NAMED_LAUNCHES (32) evicts the oldest launch names first.
    s.arm(42).unwrap();
    for idx in 0..35u32 {
        let name = LaunchName {
            pid: 1000 + idx,
            started: (i64::from(idx), 0),
        };
        assert_eq!(s.observe_launch_by(42, Some(name)).unwrap(), idx + 1);
    }
    let kept = s.named_launches(42);
    assert_eq!(kept.len(), MAX_NAMED_LAUNCHES);
    assert_eq!(kept.first().map(|n| n.pid), Some(1003));
    assert_eq!(kept.last().map(|n| n.pid), Some(1034));
}

/// NEGATIVE CONTROL: `arm` and `confirm` refuse symlinked parent directories and symlinked
/// sentinel paths without writing through or unlinking the symlink.
#[cfg(unix)]
#[test]
fn arm_and_confirm_refuse_symlinked_parent_and_non_regular_target() {
    let (s, dir) = sentinel("symlink-guards");
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, "keep").unwrap();
    std::os::unix::fs::symlink(&victim, &s.path).unwrap();

    assert!(s.arm(77).is_err(), "arm must refuse a symlink target");
    assert!(
        std::fs::symlink_metadata(&s.path)
            .unwrap()
            .file_type()
            .is_symlink(),
        "arm must leave the symlink untouched"
    );
    assert!(
        s.confirm().is_err(),
        "confirm must refuse to unlink a symlink target"
    );
    assert!(
        std::fs::symlink_metadata(&s.path)
            .unwrap()
            .file_type()
            .is_symlink(),
        "confirm must leave the symlink in place"
    );
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    std::fs::remove_file(&s.path).unwrap();

    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("boot.sentinel"), "99 1").unwrap();
    let linked_dir = dir.path().join("linked-parent");
    std::os::unix::fs::symlink(&outside, &linked_dir).unwrap();
    let via_link = Sentinel::new(linked_dir.join("boot.sentinel"));
    assert!(
        via_link.arm(88).is_err(),
        "arm must refuse a symlinked parent directory"
    );
    assert!(
        via_link.confirm().is_err(),
        "confirm must refuse a symlinked parent directory"
    );
    assert_eq!(
        std::fs::read_to_string(outside.join("boot.sentinel")).unwrap(),
        "99 1"
    );
}
