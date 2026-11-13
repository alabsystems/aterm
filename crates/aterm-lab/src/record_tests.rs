// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_stream_yields_frames_with_their_rows_and_events_whole() {
    let mut p = StreamParser::new();
    let lines = [
        "sub 0 s-168a1e0595c70920336a",
        "T 0 9952521",
        "DELTA 0 seq=8 screen 1",
        "lab$",
        "DELTA 0 seq=19 screen 3",
        "lab$ echo hi",
        "hi",
        "lab$",
        "EVENT 0 block-complete 0 exit=0",
    ];
    let items: Vec<Item> = lines.iter().filter_map(|l| p.line(l)).collect();
    assert_eq!(
        items,
        vec![
            Item::Frame(8, vec!["lab$".into()]),
            Item::Frame(19, vec!["lab$ echo hi".into(), "hi".into(), "lab$".into()]),
            Item::Event("EVENT 0 block-complete 0 exit=0".into()),
        ]
    );
    assert_eq!(p.t(), Some(9_952_521));
}
