// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn exit_code_259_is_not_alive() {
    let mut child = std::process::Command::new("cmd")
        .args(["/c", "exit 259"])
        .spawn()
        .expect("spawn");
    let pid = child.id();
    let status = child.wait().expect("wait");
    assert_eq!(status.code(), Some(259), "child exited with the sentinel");
    assert!(
        !super::pid_alive(pid),
        "exit code 259 must not be misread as STILL_ACTIVE"
    );
    drop(child);
}

#[test]
fn a_child_born_after_a_wall_reading_reads_later_and_none_once_exited() {
    use crate::spawnfd::Stdio;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    let on_wall = |birth: super::ProcessBirth| {
        UNIX_EPOCH + Duration::from_secs(birth.seconds) + Duration::from_micros(birth.microseconds)
    };
    let moment = SystemTime::now();
    let deadline = Instant::now() + Duration::from_secs(60);
    while SystemTime::now().duration_since(moment).unwrap_or_default() < Duration::from_millis(2) {
        assert!(Instant::now() < deadline, "wall clock did not advance");
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut child = crate::spawnfd::command("cmd")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cmd");
    let pid = i32::try_from(child.id()).expect("a pid");
    let born = super::read_process_birth(pid);
    let after = SystemTime::now();
    let _ = child.kill();
    let _ = child.wait();
    let born = on_wall(born.expect("a live child's birth"));
    assert!(born > moment, "{born:?} after {moment:?}");
    assert!(born <= after, "{born:?} by {after:?}");
    let own = i32::try_from(std::process::id()).expect("a pid");
    let mine = on_wall(super::read_process_birth(own).expect("this process's birth"));
    assert!(mine < moment, "{mine:?} before {moment:?}");
    assert_eq!(super::read_process_birth(pid), None, "an exited process");
    assert_eq!(super::read_process_birth(1), None, "never pid 1");
    assert_eq!(super::read_process_birth(0), None, "never pid 0");
    drop(child);
}

#[test]
fn the_raw_creation_filetime_is_the_untruncated_procstart() {
    const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
    let own = i32::try_from(std::process::id()).expect("a pid");
    let ticks = super::read_creation_filetime(own).expect("this process's creation time");
    assert!(ticks > UNIX_EPOCH_TICKS, "after 1970: {ticks}");
    let decimal = ticks.to_string();
    assert_eq!(decimal.parse::<u64>(), Ok(ticks));
    let birth = super::read_process_birth(own).expect("birth");
    let since = ticks - UNIX_EPOCH_TICKS;
    assert_eq!(since / 10_000_000, birth.seconds);
    assert_eq!(since % 10_000_000 / 10, birth.microseconds);
    assert_eq!(super::read_creation_filetime(1), None, "never pid 1");
    assert_eq!(super::read_creation_filetime(0), None, "never pid 0");
}
