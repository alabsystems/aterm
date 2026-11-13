// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::kill_group;

/// A group whose members have all ended takes a signal without failing: on macOS its `killpg`
/// answers `EPERM`, which [`aterm_uds::process::settle_group_signal`] settles from the listing.
/// NEGATIVE CONTROL: return `last_os_error` from `kill_group` unsettled and this fails on macOS.
#[test]
fn a_signal_to_a_group_whose_members_ended_is_not_a_failure() {
    let mut leader = aterm_uds::spawnfd::command("/bin/sh");
    leader
        .args(["-c", "exit 0"])
        .setsid(true)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    let mut leader = leader.spawn().expect("a leader");
    let pid = i32::try_from(leader.id()).expect("pid");
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the leader's exit", || {
        leader.exited().expect("its exit, unreaped").then_some(())
    });
    kill_group(pid, libc::SIGKILL).expect("a group with nothing left to signal");
    leader.wait().expect("the leader, reaped");
}
