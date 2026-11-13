// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn after_close(dir: &Path, names: &[&str]) -> Slot {
    // CLOEXEC descriptions may survive a sibling fork until its exec. Observe the
    // final close instead of imposing a scheduling budget on an unrelated child.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the final guest-slot description to close",
        || Slot::try_names(dir, names).unwrap(),
    )
}

#[test]
fn only_one_guest_display_is_open_beside_two_guest_slots() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let display = Slot::try_names(dir.path(), &["guest-display"])
        .unwrap()
        .unwrap();
    assert!(
        Slot::try_names(dir.path(), &["guest-display"])
            .unwrap()
            .is_none()
    );
    let a = Slot::try_in(dir.path()).unwrap().unwrap();
    let b = Slot::try_in(dir.path()).unwrap().unwrap();
    assert!(Slot::try_in(dir.path()).unwrap().is_none());
    drop(display);
    drop(after_close(dir.path(), &["guest-display"]));
    drop((a, b));
}

#[test]
fn two_guests_hold_both_slots_and_a_live_child_copy_keeps_its_slot() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let a = Slot::try_in(dir.path()).unwrap().unwrap();
    let b = Slot::try_in(dir.path()).unwrap().unwrap();
    assert!(Slot::try_in(dir.path()).unwrap().is_none());
    // NEGATIVE CONTROL: the parent's drop alone must not admit a third guest.
    let child = std::fs::File::from(a.inherited().unwrap());
    drop(a);
    assert!(Slot::try_in(dir.path()).unwrap().is_none());
    drop(child);
    let c = after_close(dir.path(), &["guest-a", "guest-b"]);
    assert!(Slot::try_in(dir.path()).unwrap().is_none());
    drop((b, c));
    drop(after_close(dir.path(), &["guest-a", "guest-b"]));
}

#[test]
fn the_boot_process_keeps_admission_after_its_parent_guard_closes() {
    use aterm_uds::spawnfd::{Stdio, command};
    let dir = aterm_tempfile::tempdir().unwrap();
    let a = Slot::try_in(dir.path()).unwrap().unwrap();
    let b = Slot::try_in(dir.path()).unwrap().unwrap();
    let mut cmd = command("/bin/cat");
    cmd.inherit_guards(vec![a.inherited().unwrap()]).unwrap();
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = aterm_uds::deadline::cat_echoes(&mut child);
    drop(cmd);
    drop(a);
    // Removing inherit_guards above admits a third guest here (negative control).
    assert!(Slot::try_in(dir.path()).unwrap().is_none());
    drop(input);
    assert!(child.wait().unwrap().success());
    drop(after_close(dir.path(), &["guest-a", "guest-b"]));
    drop(b);
}
