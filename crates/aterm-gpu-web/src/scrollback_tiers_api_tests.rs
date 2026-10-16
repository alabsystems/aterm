// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

// aterm-gpu-web's tests of the tiered scrollback surface: `scrollback_tiers_api.rs`, which both web
// crates compile, includes this file into its tests, so these drive this
// crate's own frame path.

use super::*;
use crate::AtermGpuTerminal;

#[test]
fn ctor_attaches_tiered_store_with_the_unified_default_total() {
    let Some(mut t) = AtermGpuTerminal::new_from_system(6, 40, 14.0) else {
        return;
    };
    assert!(
        t.term.scrollback().is_some(),
        "E1: the GPU wasm ctor must attach the tiered store"
    );
    assert_eq!(
        t.term.scrollback_line_limit(),
        Some(aterm_core::scrollback::DEFAULT_LINE_LIMIT),
        "the construction default round-trips as ONE total"
    );
    // Ingest stages; the drain export promotes (the offscreen/GPU render
    // paths call the same hook).
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

/// The JSON wire contract the host glue parses: exact keys, lowercase codec
/// tokens, JSON booleans.
///
/// Feature unification means `TierCapabilities::current()` differs between
/// `--workspace` and `-p` test builds, so every combination is pinned
/// against the pure renderer, and the exported method is only checked to
/// report this build. The shipped lz4/no-spill shape holds structurally:
/// `disk-tier` cannot build for wasm32-unknown-unknown.
#[test]
fn tier_capabilities_json_pins_the_wire_contract() {
    use aterm_core::scrollback::{ColdTierCodec, TierCapabilities};
    let json = |cold_codec, disk_spill| {
        render_tier_capabilities_json(TierCapabilities {
            cold_codec,
            disk_spill,
        })
    };
    assert_eq!(
        json(ColdTierCodec::Lz4, false),
        "{\"coldCodec\":\"lz4\",\"diskSpill\":false}"
    );
    assert_eq!(
        json(ColdTierCodec::Lz4, true),
        "{\"coldCodec\":\"lz4\",\"diskSpill\":true}"
    );
    assert_eq!(
        AtermGpuTerminal::tier_capabilities_json(),
        render_tier_capabilities_json(TierCapabilities::current()),
        "the exported method must report the capabilities of THIS build"
    );
}
