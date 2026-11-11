// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::rom::rasterize_master;
use super::*;

fn baked(cw: u16, ch: u16) -> RainBaker {
    let rom = rasterize_master();
    let mut b = RainBaker::default();
    b.begin_frame(cw, ch);
    while !b.complete() {
        assert!(b.bake_tiles(&rom));
    }
    b
}

fn tile_peak(b: &RainBaker, g: u8) -> u8 {
    let art = b.tile(g).expect("baked tile");
    assert_eq!((art.w(), art.h()), (b.cell_w, b.cell_h), "one cell a tile");
    art.data()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|t| t[3])
        .max()
        .unwrap_or(0)
}

/// Verify that all 64 tiles carry ink across small and retina cell metrics.
#[test]
fn all_tiles_nonempty_at_live_metrics() {
    for (cw, ch) in [(7u16, 14u16), (10, 20), (20, 40)] {
        let b = baked(cw, ch);
        for g in 0..ROM_GLYPHS as u8 {
            assert!(tile_peak(&b, g) >= 64, "tile {g} too faint at {cw}x{ch}");
            assert!(
                b.tile(g)
                    .expect("baked")
                    .data()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|t| t[..3] == [255, 255, 255]),
                "tile {g}: white coverage, so a tint paints exactly"
            );
        }
    }
}

/// Verify that rebuilt bakers replay the same version sequence and bake the same tiles.
#[test]
fn rebuilt_bakers_replay_versions_and_tiles() {
    let rom = rasterize_master();
    let mut first = Vec::new();
    let mut first_ids = Vec::new();
    for run in 0..2 {
        let mut b = RainBaker::default();
        b.begin_frame(10, 20);
        let mut seq = vec![b.version()];
        while !b.complete() {
            b.bake_tiles(&rom);
            seq.push(b.version());
        }
        let ids: Vec<u128> = (0..ROM_GLYPHS as u8)
            .map(|g| b.tile(g).expect("baked").id())
            .collect();
        if run == 0 {
            (first, first_ids) = (seq, ids);
        } else {
            assert_eq!(seq, first, "rebuilt baker replays the version sequence");
            assert_eq!(ids, first_ids, "…and bakes the same tiles");
        }
    }
}

/// Verify that `begin_frame` with unchanged metrics preserves bake progress.
#[test]
fn same_metric_begin_frame_keeps_progress() {
    let rom = rasterize_master();
    let mut b = RainBaker::default();
    b.begin_frame(10, 20);
    b.bake_tiles(&rom);
    let (tile, ver) = (b.next_tile, b.version());
    assert!(b.tile(0).is_some() && b.tile(MAX_RAIN_BAKES_PER_TICK as u8).is_none());
    b.begin_frame(10, 20);
    assert_eq!((b.next_tile, b.version()), (tile, ver));
}
