// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Metrics, emit_summary, text_field, wire};

#[test]
fn the_card_engine_cost_rides_the_summary_and_resets() {
    const KEYS: [&str; 12] = [
        "proc_events",
        "proc_drain_ms",
        "exec_edges",
        "procwatch_fallbacks",
        "procwatch_overflow",
        "holder_cuts",
        "fg_handbacks",
        "parse_holds",
        "parse_lock_waits",
        "parse_lock_wait_ms",
        "status_sweeps",
        "status_sweep_ms",
    ];
    let m = Metrics::new();
    let ctx = wire::ReadCtx::inert(&m, 0);
    let keys = wire::law_tests::body(|s| emit_summary(&ctx, None, s)).keys;
    let at = keys
        .iter()
        .position(|key| key == "pet_world_cells")
        .expect("pet_world_cells is a summary key");
    assert_eq!(keys[at + 1..at + 1 + KEYS.len()], KEYS);
    assert_eq!(keys[at + 1 + KEYS.len()], "fx_glow_quads_last");

    let c = &m.window.command;
    c.note_parse_hold(0);
    c.note_parse_hold(1_500_000);
    c.note_status_sweep(250_000);
    c.proc_events.add(3);
    c.proc_drain_ns.add(42_000);
    let text = wire::render_text(|s| emit_summary(&ctx, None, s));
    for (key, value) in [
        ("proc_events", "3"),
        ("proc_drain_ms", "0.042"),
        ("parse_holds", "2"),
        ("parse_lock_waits", "1"),
        ("parse_lock_wait_ms", "1.500"),
        ("status_sweeps", "1"),
        ("status_sweep_ms", "0.250"),
    ] {
        assert_eq!(text_field(&text, key), Some(value), "{key}: {text}");
    }

    m.reset();
    let text = wire::render_text(|s| emit_summary(&ctx, None, s));
    for key in KEYS {
        let value = text_field(&text, key).expect(key);
        assert!(
            value.bytes().all(|b| b == b'0' || b == b'.'),
            "{key}={value}"
        );
    }
}
