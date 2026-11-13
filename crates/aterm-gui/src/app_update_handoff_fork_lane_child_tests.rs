// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::process::CommandExt as _;

fn stand_in(cloexec: bool) -> OwnedFd {
    let null = std::fs::File::open("/dev/null").expect("open /dev/null");
    let cmd = if cloexec {
        libc::F_DUPFD_CLOEXEC
    } else {
        libc::F_DUPFD
    };
    // SAFETY: F_DUPFD(_CLOEXEC) on a descriptor this test owns.
    let fd = unsafe { libc::fcntl(null.as_raw_fd(), cmd, 64) };
    assert!(fd >= 64, "a stand-in at 64 or above: {fd}");
    // SAFETY: a fresh descriptor this test exclusively owns.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

fn child_holds(numbers: &[i32], hook: Option<Vec<i32>>) -> String {
    let probe = numbers
        .iter()
        .map(|&fd| {
            let held = aterm_spec::host::sh_held(fd);
            format!("{held} && got=\"$got held:{fd}\"; ")
        })
        .collect::<String>();
    let Some(handed) = hook else {
        let dir = aterm_tempfile::tempdir().expect("a temp dir");
        let report = dir.path().join("holds");
        let status = aterm_uds::spawnfd::transparent("/bin/sh")
            .args([
                "-c",
                &format!("got=; {probe}exec >\"$1\" </dev/null; printf '%s end\\n' \"$got\""),
                "sh",
            ])
            .arg(&report)
            .status()
            .expect("run the control");
        assert!(status.success(), "{status}");
        return std::fs::read_to_string(&report)
            .expect("the control's report")
            .trim()
            .to_string();
    };
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.args(["-c", &format!("got=; {probe}printf '%s end\\n' \"$got\"")]);
    let strip = aterm_uds::spawnfd::InheritStrip::prepare();
    // SAFETY: the hook runs between fork and exec; see the function.
    unsafe {
        cmd.pre_exec(move || super::fork_lane_child_holds(strip, &handed));
    }
    let out = cmd.output().expect("run the stand-in successor");
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn the_fork_lane_successor_holds_what_it_is_handed_and_nothing_else() {
    let stray = stand_in(false);
    let handed = stand_in(true);
    let (s, h) = (stray.as_raw_fd(), handed.as_raw_fd());
    assert_eq!(
        child_holds(&[s, h], None),
        format!("held:{s} end"),
        "CONTROL: the transparent spawn carries the stray and not the flagged one"
    );
    assert_eq!(
        child_holds(&[s, h], Some(vec![h])),
        format!("held:{h} end"),
        "the successor holds the handed descriptor and not the stray"
    );
    drop((stray, handed));
}

#[test]
fn the_cold_lane_exec_hands_over_nothing_this_process_holds() {
    const STAGE: &str = "ATERM_TEST_COLD_EXEC_HAND_OVER";
    if let Ok(stage) = std::env::var(STAGE) {
        let stray = stand_in(false);
        let fd = stray.as_raw_fd();
        let _lifeline = (stage == "hand-over").then(super::cold_exec_hand_over);
        let error = std::process::Command::new("/bin/sh")
            .args(["-c", &aterm_spec::host::held_probe(fd)])
            .exec();
        panic!("the probe did not replace this helper: {error}");
    }
    let probe = |stage: &str| {
        let mut helper = crate::test_alone::StandIn::this_test();
        helper.command().env(STAGE, stage);
        helper
            .spawn()
            .selected()
            .status
            .and_then(|status| status.code())
    };
    assert_eq!(
        probe("plain"),
        Some(3),
        "CONTROL: a plain exec hands the probe the stray"
    );
    assert_eq!(
        probe("hand-over"),
        Some(0),
        "the cold lane's exec handed the probe the stray"
    );
}
