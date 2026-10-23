// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use super::*;
use crate::AtermGpuTerminal;

type Host = AtermGpuTerminal;

const FONT: &[u8] = include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf");

fn term() -> Host {
    AtermGpuTerminal::new(
        6,
        40,
        FONT,
        14.0,
        0x00d8_d8d8,
        0x0018_1818,
        0x00a0_c0ff,
        0x0040_4050,
    )
    .expect("the bundled font")
}

#[test]
fn a_flood_stages_and_the_drain_lands_it_in_the_store() {
    let mut t = term();
    let mut buf = Vec::new();
    for i in 0..4_000 {
        buf.extend_from_slice(format!("gpu tier line {i}\r\n").as_bytes());
    }
    t.process(&buf);
    assert!(t.scrollback_backlog_lines() > 0, "flood ingest stages");
    while t.drain_scrollback_backlog(0) > 0 {}
    assert!(
        t.term
            .scrollback()
            .map_or(0, aterm_core::scrollback::ScrollbackStorage::line_count)
            > 0,
        "drained lines landed in the tiered store"
    );
}
