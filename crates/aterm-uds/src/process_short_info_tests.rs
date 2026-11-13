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
