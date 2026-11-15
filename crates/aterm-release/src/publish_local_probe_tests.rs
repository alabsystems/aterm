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

/// libproc's group listing reads as the group only where it proves itself: members are the group
/// alive and named, and an empty listing is the group empty only when it is whole and the same
/// lister shows this process in its own group. NEGATIVE CONTROL: a failed libproc read (`None`),
/// an own-group read that fails or lacks this process, and an incomplete empty listing are each
/// unknown, never "no members"; read any of them as `Empty` and a resume reclaims a fence whose
/// publisher's children may still be running.
#[cfg(target_os = "macos")]
#[test]
fn a_libproc_group_listing_is_empty_only_where_it_proves_itself() {
    use aterm_uds::process::GroupMembers;
    let listing = |pids: &[i32], complete: bool| GroupMembers {
        pids: pids.to_vec(),
        complete,
    };
    let me = 40;
    let shows_me = || Some(listing(&[39, 40], true));
    let name = |pid: u32| format!("p{pid}");
    let unknown = |state: GroupState, why: &str| {
        assert!(
            matches!(&state, GroupState::Unknown(reason) if reason.contains(why)),
            "{state:?} should be unknown because {why}"
        );
    };

    assert_eq!(
        LocalProbe::group_listed(Some(listing(&[41, 42], true)), shows_me, me, name),
        GroupState::Members(vec![(41, "p41".to_string()), (42, "p42".to_string())])
    );
    assert_eq!(
        LocalProbe::group_listed(Some(listing(&[41], false)), || None, me, name),
        GroupState::Members(vec![(41, "p41".to_string())]),
        "a partial listing with members is the group alive, and needs no own-group proof"
    );
    assert_eq!(
        LocalProbe::group_listed(Some(listing(&[], true)), shows_me, me, name),
        GroupState::Empty
    );

    unknown(
        LocalProbe::group_listed(None, shows_me, me, name),
        "could not list the group",
    );
    unknown(
        LocalProbe::group_listed(Some(listing(&[], true)), || None, me, name),
        "could not list this process's own group",
    );
    unknown(
        LocalProbe::group_listed(
            Some(listing(&[], true)),
            || Some(listing(&[39], true)),
            me,
            name,
        ),
        "did not show this process",
    );
    unknown(
        LocalProbe::group_listed(Some(listing(&[], false)), shows_me, me, name),
        "incompletely",
    );
}

/// The real lister: a running child that leads its own group is read as that group's member,
/// by its pid and kernel short name, and once it is killed (by its own pid) and reaped, the same
/// group reads as empty, not unknown.
#[cfg(target_os = "macos")]
#[test]
fn a_live_group_lists_its_member_and_a_reaped_one_reads_empty() {
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .process_group(0)
        .spawn()
        .expect("run sleep in its own process group");
    let pid = child.id();
    let ran = i32::try_from(pid).expect("pid");
    let bound = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while aterm_uds::process::thread_ran(ran) != Some(true) {
        if std::time::Instant::now() > bound {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the sleep never ran");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let live = LocalProbe::members_of(pid);
    let _ = child.kill();
    child.wait().expect("reap the sleep");
    assert_eq!(live, GroupState::Members(vec![(pid, "sleep".to_string())]));
    assert_eq!(LocalProbe::members_of(pid), GroupState::Empty);
}
