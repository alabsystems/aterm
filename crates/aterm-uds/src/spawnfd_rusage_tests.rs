// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::spawnfd::{Stdio, command};

/// The blocking nonterminal path waits on actual kernel exit, preserves the
/// owning wait, and needs neither a polling loop nor another process owner.
#[test]
fn a_stopped_notification_waits_for_real_exit_without_reaping() {
    let mut child = command("/bin/cat")
        .start_suspended(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut resumed = false;
    let seen = wait_after_nonterminal_with(pid, || {
        child.resume().unwrap();
        child.stdin.take();
        resumed = true;
    });
    if !resumed {
        let _ = child.kill();
    }
    let status = child.wait().unwrap();
    assert!(seen.unwrap());
    assert!(resumed);
    assert!(status.success(), "the original owner still reaps its child");
}
