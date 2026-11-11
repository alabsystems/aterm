// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const PARTS: [NukePart; 3] = [NukePart::Cap, NukePart::Stem, NukePart::Skirt];

#[test]
fn natural_sizes_stay_inside_the_baker_slot() {
    for (cw, ch) in [(7u16, 14u16), (14, 32), (8, 16), (30, 20), (40, 12)] {
        for part in PARTS {
            let (w, h) = nuke_nat_size(part, cw, ch);
            assert!(
                u32::from(w) <= 4 * u32::from(ch) && u32::from(h) <= 2 * u32::from(ch),
                "{part:?} at cell {cw}x{ch} baked {w}x{h}, outside the slot"
            );
            assert!(w >= 4 && h >= 4, "{part:?} degenerate at cell {cw}x{ch}");
        }
    }
    for (cw, ch) in [(0u16, 0u16), (1, 1)] {
        for part in PARTS {
            let (w, h) = nuke_nat_size(part, cw, ch);
            let eff_h = u32::from(ch.max(1));
            assert!(u32::from(w) <= 4 * eff_h && u32::from(h) <= 2 * eff_h);
            assert!(w >= 1 && h >= 1);
        }
    }
}

#[test]
fn parts_live_only_in_their_phase() {
    assert!(
        nuke_draw(0, NukePart::Stem).is_none(),
        "stem before the flash"
    );
    assert!(
        nuke_draw(0, NukePart::Cap).is_none(),
        "cap before the flash"
    );
    assert!(
        nuke_draw(FLASH_END_MS, NukePart::Stem).is_some(),
        "the stem must rise once the flash ends"
    );
    assert!(
        nuke_draw(CAP_BLOOM_START_MS, NukePart::Cap).is_some(),
        "the cap must bloom at its edge"
    );
    const {
        assert!(CAP_BLOOM_START_MS < STEM_RISE_END_MS);
    }
    for part in PARTS {
        assert!(
            nuke_draw(NUKE_TOTAL_MS, part).is_none(),
            "{part:?} outlived the detonation"
        );
    }
    assert!(nuke_core_draw(CAP_BLOOM_START_MS - 1).is_none());
    assert!(nuke_core_draw(CAP_BLOOM_START_MS).is_some());
    assert!(
        nuke_core_draw(NUKE_TOTAL_MS - 1).is_none(),
        "core must die before the dust"
    );
    let core = nuke_core_draw(1_400).expect("core mid-bloom");
    let cap = nuke_draw(1_400, NukePart::Cap).expect("cap mid-bloom");
    assert!(
        core.sx < cap.sx && core.sy < cap.sy,
        "the core sits INSIDE the cap"
    );
    assert_ne!(core.tint, cap.tint, "the core must be hotter than the dust");
    for t in (0..NUKE_TOTAL_MS + 200).step_by(10) {
        if let Some(d) = nuke_core_draw(t) {
            assert!(d.sx.is_finite() && d.sy.is_finite() && d.dy_cells.is_finite());
            assert!(
                (0.0..=1.0).contains(&d.alpha),
                "core alpha {} at {t}ms",
                d.alpha
            );
            assert!(d.sx > 0.0 && d.sy > 0.0);
        }
    }
}

#[test]
fn the_whole_window_is_finite_and_cools() {
    let hot = nuke_draw(CAP_BLOOM_START_MS, NukePart::Cap).expect("cap at bloom");
    let cold = nuke_draw(NUKE_TOTAL_MS - 1, NukePart::Cap).expect("cap at the end");
    assert_ne!(hot.tint, cold.tint, "the cap never cooled");

    for t in (0..NUKE_TOTAL_MS + 200).step_by(10) {
        for part in PARTS {
            let Some(d) = nuke_draw(t, part) else {
                continue;
            };
            assert!(
                d.sx.is_finite() && d.sy.is_finite() && d.dy_cells.is_finite(),
                "{part:?} at {t}ms produced a non-finite transform"
            );
            assert!(
                (0.0..=1.0).contains(&d.alpha),
                "{part:?} at {t}ms had alpha {} outside 0..=1",
                d.alpha
            );
            assert!(
                d.sx > 0.0 && d.sy > 0.0,
                "{part:?} at {t}ms scaled to nothing"
            );
        }
    }
}
