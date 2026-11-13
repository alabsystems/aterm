// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::Fake;

const HANG_MS: u64 = 600_000;

#[test]
fn a_stamped_folder_stays_until_its_program_ran_its_hang_passed_or_the_mac_restarted() {
    let sys = Fake::root();
    let home = Path::new("/A/stage/upgrade-1-5");
    assert_eq!(hold_left(&sys, home), None, "nothing was started under it");
    sys.awake_at(5_000_000);
    stamp(&sys, home).unwrap();
    assert_eq!(hold_left(&sys, home), Some(FIRST_EXEC_HANG));
    sys.awake_at(5_000_000 + HANG_MS - 60_000);
    assert_eq!(hold_left(&sys, home), Some(Duration::from_secs(60)));
    sys.awake_at(5_000_000 + HANG_MS);
    assert_eq!(hold_left(&sys, home), None, "its hang has passed");
    sys.awake_at(4_000_000);
    assert_eq!(
        hold_left(&sys, home),
        None,
        "a clock below the stamp: a restart"
    );

    sys.awake_at(5_000_000);
    mark_ran(&sys, home);
    assert_eq!(hold_left(&sys, home), None, "it showed it ran");
    stamp(&sys, home).unwrap();
    assert_eq!(
        hold_left(&sys, home),
        Some(FIRST_EXEC_HANG),
        "a new start takes back the old proof"
    );
}

#[test]
fn a_new_start_takes_its_folder_back_from_an_earlier_uninstalls_reclaim_job() {
    let sys = Fake::root();
    let home = Path::new("/A/state/aterm.app-first-run");
    let mark = home.join(crate::agent::reclaim::KEPT);
    sys.file(&mark.to_string_lossy(), b"1-9");
    sys.awake_at(5_000_000);
    stamp(&sys, home).unwrap();
    assert!(sys.written(&mark.to_string_lossy()).is_none());
    assert_eq!(hold_left(&sys, home), Some(FIRST_EXEC_HANG));

    let claim = home.join(crate::agent::reclaim::CLAIM);
    let sys = Fake::root();
    sys.file(&mark.to_string_lossy(), b"1-9");
    sys.file(&claim.to_string_lossy(), b"1-9");
    sys.file(&home.join(RAN).to_string_lossy(), b"old proof");
    assert_eq!(
        stamp(&sys, home).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "a claim that won before cancellation forbids a new exec"
    );
    assert!(sys.written(&mark.to_string_lossy()).is_none());
    assert!(
        sys.written(&home.join(EXEC_STAMP).to_string_lossy())
            .is_none()
    );
    assert_eq!(
        sys.written(&home.join(RAN).to_string_lossy()),
        Some(b"old proof".to_vec())
    );

    let sys = Fake::root();
    sys.stat_fails(
        &claim.to_string_lossy(),
        std::io::ErrorKind::PermissionDenied,
    );
    assert_eq!(
        stamp(&sys, home).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "an unread claim is no proof that reclamation has finished"
    );
    assert!(
        sys.written(&home.join(EXEC_STAMP).to_string_lossy())
            .is_none()
    );
}

#[test]
fn a_stamp_with_no_reading_holds_and_only_an_exit_or_a_line_is_proof() {
    let sys = Fake::root();
    sys.file("/A/.staging/exec", b"");
    sys.awake_at(u64::MAX);
    assert_eq!(
        hold_left(&sys, Path::new("/A/.staging")),
        Some(FIRST_EXEC_HANG)
    );
    assert!(showed_it_ran(&Ran::exit(1, "refused")));
    assert!(showed_it_ran(&Ran {
        status: None,
        stdout: b"0.1.0\n".to_vec(),
        stderr: Vec::new(),
    }));
    assert!(!showed_it_ran(&Ran {
        status: None,
        stdout: Vec::new(),
        stderr: b"Killed: 9".to_vec(),
    }));
    assert_eq!(minutes(Duration::from_secs(61)), 2);
    assert_eq!(minutes(Duration::from_millis(1)), 1);
}

/// The hold left on `/A/.staging` stamped `stamp` by the root script, on a Mac whose
/// clocks read the boot id `session`, the boot and the last wake at `boot` and `woke`,
/// the wall clock at `now`, and the boot again at `again` (Unix seconds).
fn staged_hold(stamp: &str, session: &str, (boot, woke, now, again): Clock) -> Option<Duration> {
    let mut sys = Fake::root();
    sys.now = now;
    let clock = |sec: u64| format!("{{ sec = {sec}, usec = 0 }} Thu Oct 22 09:00:00 2026\n");
    sys.on(
        "/usr/sbin/sysctl -n kern.bootsessionuuid kern.boottime kern.waketime",
        Ran::ok(&format!("{session}\n{}{}", clock(boot), clock(woke))),
    )
    .on("/usr/sbin/sysctl -n kern.boottime", Ran::ok(&clock(again)));
    sys.file("/A/.staging/exec", stamp.as_bytes());
    hold_left(&sys, Path::new("/A/.staging"))
}

/// `(boot, woke, now, again)`, as [`staged_hold`] takes them.
type Clock = (u64, u64, u64, u64);

#[test]
fn the_root_scripts_stamp_is_aged_by_the_clocks_it_was_read_on() {
    const BOOT: u64 = 1_792_000_000;
    const UP: u64 = 5_000;
    let id = "55C317A3-F793-4F0D-BE04-262D2C6ED332";
    let stamp = format!("{id} {UP} {BOOT} 0\n");
    let at = |up: u64| (BOOT, 0, BOOT + up, BOOT);
    assert_eq!(
        staged_hold(&stamp, id, at(UP + 7_200)),
        None,
        "the negative control: its hang passed two hours ago"
    );
    assert_eq!(staged_hold(&stamp, id, at(UP)), Some(FIRST_EXEC_HANG));
    assert_eq!(
        staged_hold(&stamp, id, at(UP + 9 * 60)),
        Some(Duration::from_secs(60))
    );
    let other = "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0";
    assert_eq!(
        staged_hold(&stamp, other, (BOOT + 9_000, 0, BOOT + 9_030, BOOT + 9_000)),
        None,
        "a restart since the stamp"
    );
    let stepped = BOOT + 1_200;
    assert_eq!(
        staged_hold(&stamp, id, (stepped, 0, stepped + UP + 120, stepped)),
        Some(Duration::from_secs(480)),
        "a step of the wall clock moves the boot time with it"
    );
    assert_eq!(
        staged_hold(&stamp, id, (BOOT, 0, BOOT + UP + 7_200, stepped)),
        Some(FIRST_EXEC_HANG),
        "a step between the reads of the boot time and the wall clock"
    );
    assert_eq!(
        staged_hold(&format!("{id} {UP} {BOOT} 0 more\n"), id, at(UP + 7_200)),
        Some(FIRST_EXEC_HANG),
        "another form, read as no reading"
    );
}

#[test]
fn a_file_is_no_folder_a_program_was_started_from() {
    let live = crate::port::Live::new();
    let tmp = aterm_tempfile::tempdir().unwrap();
    let file = tmp.path().join("aterm-open-1.sig");
    std::fs::write(&file, b"sig").unwrap();
    assert_eq!(hold_left(&live, &file), None);
    let started = tmp.path().join("upgrade-1-5");
    std::fs::create_dir(&started).unwrap();
    stamp(&live, &started).unwrap();
    assert!(hold_left(&live, &started).is_some(), "the negative control");
}
