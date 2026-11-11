// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/scrollback_tiers_api_tests.rs"
));

#[test]
fn ctor_attaches_tiered_store_with_the_unified_default_total() {
    let t = term();
    assert!(
        t.term.scrollback().is_some(),
        "E1: the web ctor must attach the tiered store"
    );
    assert_eq!(
        t.term.scrollback_line_limit(),
        Some(aterm_core::scrollback::DEFAULT_LINE_LIMIT),
        "the construction default round-trips as ONE total"
    );
}

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
        Host::tier_capabilities_json(),
        render_tier_capabilities_json(TierCapabilities::current()),
        "the exported method must report the capabilities of THIS build"
    );
}
