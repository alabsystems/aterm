// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{GroupState, LocalProbe};
use crate::interrupt::Proc;

fn proc(pid: i32, pgid: i32, name: &str) -> Proc {
    Proc {
        pid,
        parent: 1,
        pgid,
        name: name.to_string(),
    }
}

/// A group is read from a listing that shows this process: its members by pid and name,
/// empty when none is in it, and unknown when the listing lacks this process.
#[test]
fn a_group_reads_from_a_listing_that_shows_this_process() {
    let procs = || {
        vec![
            proc(40, 40, "zsh"),
            proc(41, 77, "aterm-release"),
            proc(42, 77, "curl"),
            proc(43, 78, "git"),
        ]
    };
    assert_eq!(
        LocalProbe::group_of(procs(), 77, 40),
        GroupState::Members(vec![
            (41, "aterm-release".to_string()),
            (42, "curl".to_string())
        ])
    );
    assert_eq!(LocalProbe::group_of(procs(), 79, 40), GroupState::Empty);
    assert!(matches!(
        LocalProbe::group_of(procs(), 77, 99),
        GroupState::Unknown(why) if why.contains("did not list this process")
    ));
}

/// A live process started through a symlink of another name reads by the path it was
/// started by, as `current_exe` records a publisher's, not by the image the link resolves
/// to, so a live publisher run through such a link is never read as its pid reused.
#[cfg(target_os = "macos")]
#[test]
fn a_process_started_through_a_symlink_reads_by_the_path_it_was_started_by() {
    use super::{PidState, PublisherProbe as _};
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let link = dir.path().join("cut");
    std::os::unix::fs::symlink("/bin/sleep", &link).expect("link sleep as cut");
    let mut child = aterm_uds::spawnfd::command(&link)
        .arg("600")
        .spawn()
        .expect("run sleep through the link");
    let pid = i32::try_from(child.id()).expect("pid");
    let bound = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while aterm_uds::process::thread_ran(pid) != Some(true) {
        if std::time::Instant::now() > bound {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the linked sleep never ran");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let state = LocalProbe.pid_state(child.id());
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        state,
        PidState::Running {
            exe: Some(link.display().to_string())
        }
    );
}
