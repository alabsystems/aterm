// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// NEGATIVE CONTROL: the original push gate cannot carry a sign failure
/// once push won. The independent completion must still wake the engine.
#[test]
fn a_sign_failure_after_the_push_wakes_the_draft_wait_and_reset_clears_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let gate = PublishGate::at(dir.path().join("07-publish.gate"));
    assert!(gate.push().unwrap());
    assert!(!gate.stop().unwrap());
    gate.sign_finished("red").unwrap();
    assert_eq!(gate.word().unwrap().as_deref(), Some("push"));
    assert_eq!(
        std::fs::read_to_string(gate.path.with_extension("sign")).unwrap(),
        "red"
    );
    gate.reset().unwrap();
    assert!(gate.word().unwrap().is_none());
    assert!(!gate.path.with_extension("sign").exists());
}
