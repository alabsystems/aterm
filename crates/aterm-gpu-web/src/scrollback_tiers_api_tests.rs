// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

// aterm-gpu-web's tests of the tiered scrollback surface: `scrollback_tiers_api.rs`, which both web
// crates compile, includes this file into its tests, so these drive this
// crate's own frame path.

use super::*;
use crate::AtermGpuTerminal;

/// This crate's host, for the tests `scrollback_tiers_api.rs` shares with
/// aterm-wasm.
type Host = AtermGpuTerminal;

/// The bundled JetBrains Mono: a checked-in face, so no test here needs a
/// system font to construct its terminal.
const FONT: &[u8] = include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf");

/// A 6x40 terminal on the bundled face: the shipped constructor, tiered store
/// attached.
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

/// A flood stages its scrolled-off lines, and the drain export (the hook the
/// offscreen and GPU render paths call) lands them in the tiered store.
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
