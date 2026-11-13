// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn call(task: Option<&str>, wait: Option<&str>, held_by: Option<u32>) -> BackgroundCall {
    BackgroundCall {
        pid: 27098,
        pgid: 27090,
        birth: Some(1_792_498_000_123_456),
        episode: 1,
        task: task.map(str::to_string),
        quiet_ms: 31 * 60_000 + 59_999,
        wait: wait.map(str::to_string),
        held_by,
    }
}

#[test]
fn the_told_fact_says_only_a_token_task_and_a_known_wait() {
    let bare = call(None, None, None).told();
    assert_eq!(
        bare,
        "a background call (pid 27098) has made no progress for 31 min"
    );
    let long_task = "b".repeat(TASK_MAX + 1);
    for (task, wait) in [
        (Some("do this; then that"), Some("tell the agent to push")),
        (Some("a\nb"), Some("")),
        (Some(""), Some("unnamed")),
        (Some(long_task.as_str()), Some("named-unparsed")),
    ] {
        assert_eq!(call(task, wait, None).told(), bare, "{task:?} {wait:?}");
    }
    let told = call(Some("bplle5dxj"), Some("mutex"), None).told();
    for said in ["shell task bplle5dxj", "(pid 27098)", "31 min", "(mutex)"] {
        assert!(told.contains(said), "{said:?} in {told}");
    }
}

/// What the call waits on is said from the engine's evidence: the wait's phrase and the process
/// holding it. CONTROL: with no holder named, no holder is said.
#[test]
fn the_told_fact_names_the_wait_and_its_holder() {
    let held = call(Some("bplle5dxj"), Some("lock"), Some(4242)).told();
    assert!(
        held.ends_with("; it waits on a file lock (lock), held by pid 4242"),
        "{held}"
    );
    let pipe = call(None, Some("pipe"), Some(77)).told();
    assert!(pipe.ends_with("(pipe), held by pid 77"), "{pipe}");
    let unnamed = call(None, Some("unnamed"), Some(77)).told();
    assert!(unnamed.ends_with("; it waits on pid 77"), "{unnamed}");
    let control = call(Some("bplle5dxj"), Some("lock"), None).told();
    assert!(!control.contains("held by"), "{control}");
}
