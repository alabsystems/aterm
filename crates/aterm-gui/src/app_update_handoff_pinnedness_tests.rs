// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::candidate_is_our_child;

#[test]
fn only_a_child_of_ours_pins_its_pid() {
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn a child to own a pid");
    let pid = libc::pid_t::try_from(child.id()).expect("pid fits");
    assert!(
        candidate_is_our_child(pid),
        "a live child of ours pins its number, so -pid still names its group"
    );

    let own = libc::pid_t::try_from(std::process::id()).expect("pid fits");
    assert!(
        !candidate_is_our_child(own),
        "a process that is not our child must never license a group kill"
    );
    assert!(!candidate_is_our_child(1), "launchd is not ours either");

    child.kill().expect("kill the probe child");
    let status = child.wait().expect("the probe left it waitable");
    assert!(!status.success(), "it was killed, not exited cleanly");
}
