// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ProcessPresence, process_presence, read_birth};
use std::time::{Duration, Instant};

#[test]
fn group_query_refuses_process_and_all_process_selectors() {
    for group in [0, 1, u32::MAX] {
        assert_eq!(super::group_presence(group), ProcessPresence::Unknown);
    }
}

/// A child that has exited and is not yet reaped (a zombie) reads `Exited`, a live one
/// `Present`, and the zombie reads no birth. Negative control: answer `Present` whenever the
/// pid answers signal zero, and the zombie reads `Present`.
#[test]
fn an_exited_unreaped_child_reads_exited() {
    let mut child = crate::spawnfd::command("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn /bin/sleep");
    let pid = i32::try_from(child.id()).expect("pid");
    let live = process_presence(pid);
    let _ = child.kill();
    // The exit is waited for without reaping it, under the one-minute hang detector.
    let deadline = Instant::now() + Duration::from_secs(60);
    let exited = loop {
        // SAFETY: every field of `siginfo_t` is an integer or a pointer, so all-zero is valid.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: a non-blocking `WNOWAIT` `waitid` of this test's own child into `info`.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                libc::id_t::try_from(pid).expect("pid"),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        // SAFETY: `info` was zeroed and then filled by `waitid`.
        if rc == 0 && unsafe { info.si_pid() } == pid {
            break true;
        }
        if rc != 0 || Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let zombie = process_presence(pid);
    let birth = read_birth(pid);
    let _ = child.wait();
    assert_eq!(live, ProcessPresence::Present, "a live child");
    assert!(exited, "the killed child exited within its hang detector");
    assert_eq!(zombie, ProcessPresence::Exited, "an unreaped zombie");
    assert_eq!(birth, None, "a zombie reads no birth");
}
