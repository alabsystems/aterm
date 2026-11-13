// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Ready, Signal};
use std::io::Write as _;
use std::time::Duration;

#[test]
fn the_byte_is_ready_a_bare_close_is_closed_and_a_held_end_is_a_hang() {
    let dir = crate::supervise::test_scratch_path("ready", "signal");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the test's dir");

    let mut ready = Ready::open(&dir).expect("a ready signal");
    let mut child = ready
        .write
        .as_ref()
        .expect("held")
        .try_clone()
        .expect("dup");
    child.write_all(b"R").expect("the byte");
    drop(child);
    assert!(matches!(ready.wait(Duration::from_secs(60)), Signal::Ready));

    let mut closed = Ready::open(&dir).expect("a ready signal");
    let child = closed
        .write
        .as_ref()
        .expect("held")
        .try_clone()
        .expect("dup");
    drop(child);
    assert!(matches!(
        closed.wait(Duration::from_secs(60)),
        Signal::Closed
    ));

    let mut held = Ready::open(&dir).expect("a ready signal");
    let _child = held.write.as_ref().expect("held").try_clone().expect("dup");
    assert!(matches!(
        held.wait(Duration::from_millis(50)),
        Signal::TimedOut
    ));
    assert_eq!(held.args()[0], super::FLAG);
    let _ = std::fs::remove_dir_all(&dir);
}
