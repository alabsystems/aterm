// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use std::path::Path;

use aterm_uds::spawnfd::Stdio;

use super::{in_tree, start_of};

/// The pid as `in_tree` and `start_of` take it.
fn pid(raw: u32) -> i32 {
    i32::try_from(raw).expect("a pid that fits an i32")
}

/// The walk climbs from this process: this process and its parent are in the tree, at
/// their own starts, and a live child of this process, at its own start, is not. So a
/// grant names a spawner the asking helper descends from, never any live process whose
/// pid and start match (the model's `GrantPidBound`, against the real walk).
#[test]
fn the_ancestry_walk_holds_ancestors_and_not_a_live_child() {
    let me = std::process::id();
    let my_start = start_of(pid(me)).expect("this process's start");
    assert!(in_tree(me, my_start), "this process is in its own tree");
    assert!(!in_tree(me, my_start + 1), "a recycled pid never matches");
    let parent = aterm_uds::process::read_process_parent(pid(me)).expect("the parent");
    let parent_start = start_of(pid(parent)).expect("the parent's start");
    assert!(in_tree(parent, parent_start), "the parent is an ancestor");
    let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
    let mut child = aterm_uds::spawnfd::command(&cat)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the child");
    let stdin = aterm_uds::deadline::cat_echoes(&mut child);
    let child_pid = child.id();
    let child_start = start_of(pid(child_pid)).expect("the live child's start");
    let outside = in_tree(child_pid, child_start);
    drop(stdin);
    let _ = child.wait();
    assert!(!outside, "a live child is outside the asker's ancestry");
}
