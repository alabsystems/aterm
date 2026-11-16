// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{BsdShortInfo, child_pids, read_short_info};

#[test]
fn this_process_reads_its_own_short_record_back() {
    let me = i32::try_from(std::process::id()).expect("pid");
    let info = read_short_info(me).expect("this process reads");
    assert_eq!(info.pbsi_pid, me.unsigned_abs());
    // SAFETY: plain getters with no arguments.
    let (ppid, pgid) = unsafe { (libc::getppid(), libc::getpgrp()) };
    assert_eq!(info.pbsi_ppid, ppid.unsigned_abs());
    assert_eq!(info.pbsi_pgid, pgid.unsigned_abs());
    assert_ne!(info.pbsi_status, BsdShortInfo::SZOMB);
    assert_ne!(
        info.pbsi_flags & BsdShortInfo::PROC_FLAG_EXEC,
        0,
        "a test binary was exec'd"
    );
    assert!(read_short_info(0).is_none() && read_short_info(-1).is_none());
    struct StandIn(crate::spawnfd::Child);
    impl Drop for StandIn {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = StandIn(
        crate::spawnfd::command("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn"),
    );
    let pid = i32::try_from(child.0.id()).expect("pid");
    let listed = child_pids(me).expect("the table answers").contains(&pid);
    let _ = child.0.kill();
    let _ = child.0.wait();
    assert!(listed, "a spawned child is listed");
}

/// [`super::exit_unfinished`] says yes only between an exit's start and its end, a window the
/// waits that read it catch (`aterm_spec::wait`'s mid-exit test): a live child, the same child
/// once its exit has finished and it waits to be reaped, and its pid once reaped all read no.
#[test]
fn only_an_exit_in_progress_reads_unfinished() {
    use super::{ProcessPresence, exit_unfinished, process_presence};
    let mut child = crate::spawnfd::command("/bin/cat")
        .stdin(crate::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn");
    let pid = i32::try_from(child.id()).expect("pid");
    assert_eq!(process_presence(pid), ProcessPresence::Present);
    assert!(!exit_unfinished(pid), "a live child");
    drop(child.stdin.take());
    // Until it can be reaped, leaving it unreaped.
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: `info` is a writable `siginfo_t`; `WNOWAIT` leaves the child for the wait below.
    let waited = unsafe {
        libc::waitid(
            libc::P_PID,
            pid.unsigned_abs(),
            info.as_mut_ptr(),
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    assert_eq!(waited, 0, "{}", std::io::Error::last_os_error());
    assert_eq!(process_presence(pid), ProcessPresence::Exited, "a zombie");
    assert!(!exit_unfinished(pid), "a zombie's exit has finished");
    child.wait().expect("reap");
    assert!(!exit_unfinished(pid), "a reaped pid");
}
