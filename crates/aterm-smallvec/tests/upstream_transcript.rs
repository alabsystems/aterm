// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The recorded-answer differential: `transcript/script.rs` run against this
//! crate must print, line for line, what it printed against the crates.io
//! `smallvec` 1.15.1 it replaces (`transcript/smallvec-1.15.1.txt`, recorded
//! with the features aterm's graph enables; the script's header says how).
//!
//! Each line pins one step's result, contents, length, `spilled()`, capacity
//! consistency, slice-equal `Hash` and element drop order. The script covers
//! the constructors and conversions once each, then a seeded operation fuzz
//! at inline sizes 3, 1 and 0.

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
