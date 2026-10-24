// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Differential transcript test against `smallvec` 1.15.1.

#[path = "transcript/script.rs"]
mod script;

const RECORDED: &str = include_str!("transcript/smallvec-1.15.1.txt");

#[test]
fn the_facade_replays_the_upstream_transcript() {
    let ours = script::run();
    for (i, (mine, upstream)) in ours.lines().zip(RECORDED.lines()).enumerate() {
        assert_eq!(
            mine, upstream,
            "step {i} differs from smallvec 1.15.1 (left: this crate)"
        );
    }
    assert_eq!(
        ours.lines().count(),
        RECORDED.lines().count(),
        "the transcript lengths differ"
    );
}
