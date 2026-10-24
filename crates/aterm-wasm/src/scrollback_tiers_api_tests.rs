// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use super::*;
use crate::AtermTerminal;

type Host = AtermTerminal;

const FONT: &[u8] = include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf");

fn term() -> Host {
    AtermTerminal::new(
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

fn feed_lines(t: &mut AtermTerminal, n: usize) {
    let mut buf = Vec::new();
    for i in 0..n {
        buf.extend_from_slice(format!("tier line {i}\r\n").as_bytes());
    }
    t.process(&buf);
}

#[test]
fn ingest_stages_and_render_promotes() {
    let mut t = term();
    feed_lines(&mut t, 4_000);
    assert!(
        t.scrollback_backlog_lines() > 0,
        "flood ingest stages scrolled-off lines instead of compressing inline"
    );
    for _ in 0..16 {
        t.render().expect("renderable frame");
    }
    assert_eq!(
        t.scrollback_backlog_lines(),
        0,
        "render() drains the backlog"
    );
    assert!(
        t.term
            .scrollback()
            .map_or(0, aterm_core::scrollback::ScrollbackStorage::line_count)
            > 0,
        "drained lines landed in the tiered store"
    );
}

#[test]
fn a_flood_between_frames_leaves_one_marker() {
    let mut t = term();
    t.set_scrollback_budget(2_000_000);
    for _ in 0..8 {
        feed_lines(&mut t, 10_000);
        t.drain_compress_backlog_on_render();
    }
    while t.drain_scrollback_backlog(0) > 0 {}
    assert!(t.scrollback_truncated_lines() > 0.0, "the flood cut");
    let grid = t.term.grid();
    let markers = (0..grid.scrollback_lines())
        .filter_map(|i| grid.get_history_line(i))
        .filter(|line| line.to_string().starts_with("— aterm dropped "))
        .count();
    assert_eq!(markers, 1, "one flood, one marker");
}

#[test]
fn set_scrollback_limit_caps_the_total_across_tiers() {
    let mut t = term();
    t.set_scrollback_limit(1_500);
    feed_lines(&mut t, 5_000);
    while t.drain_scrollback_backlog(0) > 0 {}
    assert_eq!(
        t.term.scrollback_line_limit(),
        Some(1_500),
        "wasm export takes the ONE total"
    );
    assert_eq!(
        t.term.grid().scrollback_lines(),
        1_500,
        "ring + staged + store together honor the total"
    );
}

#[test]
fn pane_budget_evicts_and_reports() {
    let mut t = term();
    feed_lines(&mut t, 3_000);
    while t.drain_scrollback_backlog(0) > 0 {}
    let before = t
        .term
        .scrollback()
        .map_or(0, aterm_core::scrollback::ScrollbackStorage::line_count);
    assert!(before > 0);
    t.set_scrollback_budget(4_096);
    let after = t
        .term
        .scrollback()
        .map_or(0, aterm_core::scrollback::ScrollbackStorage::line_count);
    assert!(
        after < before,
        "budget shrink evicts oldest store lines ({before} -> {after})"
    );
    assert!(t.scrollback_budget_effective() <= 4_096);
}

#[test]
fn truncation_and_pressure_are_observable_out_of_band() {
    let mut t = term();
    assert_eq!(t.scrollback_truncated_lines(), 0.0);
    t.set_scrollback_budget(4_096);
    feed_lines(&mut t, 3_000);
    while t.drain_scrollback_backlog(0) > 0 {}
    assert!(
        t.scrollback_truncated_lines() > 0.0,
        "budget-pressure evictions register out-of-band"
    );
    assert!(t.scrollback_pressure() <= 2, "pressure is the 0/1/2 scale");
    let oldest = t.row_text(0);
    if let Some(text) = oldest {
        assert!(
            text.is_empty() || text.starts_with("tier line"),
            "no sentinel content injected (got {text:?})"
        );
    }
}
