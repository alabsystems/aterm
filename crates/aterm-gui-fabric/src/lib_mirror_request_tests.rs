// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{MirrorReport, parse_link_mirror};
use aterm_types::fabric_grammar::{MirrorReason, MirrorState};

/// The `link mirror` lines `aterm-link`'s bridge_mirror.rs sent as literals before it built
/// them from [`MirrorState`] and [`MirrorReason`] (F8-3), read once from that file: the wire
/// bytes the endpoint has been answering.
const SENT_AS_LITERALS: [&str; 7] = [
    "link mirror on",
    "link mirror unretained",
    "link mirror stopped reason=pin",
    "link mirror capped reason=total",
    "link mirror stopped reason=endpoint",
    "link mirror budget",
    "link mirror stopped reason=refused",
];

#[test]
fn every_mirror_line_a_bridge_builds_is_the_bytes_it_sent_and_the_endpoint_reads_it_back() {
    let reported = [
        MirrorState::On,
        MirrorState::Unretained,
        MirrorState::Budget,
    ];
    let built: Vec<(String, MirrorState, &str)> = reported
        .iter()
        .map(|s| (s.request(), *s, ""))
        .chain(
            MirrorReason::ALL
                .iter()
                .map(|r| (r.request(), r.state(), r.word())),
        )
        .collect();
    let mut sent: Vec<&str> = SENT_AS_LITERALS.to_vec();
    let mut lines: Vec<&str> = built.iter().map(|(l, ..)| l.as_str()).collect();
    sent.sort_unstable();
    lines.sort_unstable();
    assert_eq!(
        lines, sent,
        "the bridge's lines are the bytes it sent as literals"
    );
    for (line, state, reason) in &built {
        let words = line
            .strip_prefix("link mirror ")
            .unwrap_or_else(|| panic!("{line}: a `link mirror` line"));
        assert_eq!(
            parse_link_mirror(words.split(' ')),
            Some(MirrorReport {
                state: *state,
                reason: (*reason).to_string()
            }),
            "{line}: the endpoint reads back the state and reason the bridge built"
        );
    }
    for own in [MirrorState::Starting, MirrorState::Unserved] {
        let line = own.request();
        let words = line.strip_prefix("link mirror ").expect("a mirror line");
        assert_eq!(
            parse_link_mirror(words.split(' ')),
            None,
            "{line}: the endpoint's own state is no bridge's report"
        );
    }
}
