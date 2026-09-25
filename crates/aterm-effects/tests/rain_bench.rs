// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// PHOSPHOR perf gates, engine side (matrix-rain design §8): worst-case field
// emission and the progressive ROM bake, as #[ignore]d release benches in the
// repo's manual-timing idiom (warm-up, sorted iteration medians, printed
// numbers, asserted bar). Medians of 3 runs land in
// PROOF_CARRYING_PERFORMANCE.md ("PHOSPHOR"); transcripts under
// proofs/phosphor/.
//
// ```sh
// cargo test -p aterm-effects --release --test rain_bench -- --ignored --nocapture
// ```

use std::time::Instant;

use aterm_core::grid::LineSize;
use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_effects::matrix_rain::{
    EffectGeom, MatrixRain, RainConfig, RainTickInput, bake::RainBaker, rom::rasterize_master,
};
use aterm_render::{RainHalo, SpriteQuad};

const BG: u32 = 0x0011_1318;

fn space_cell() -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [0xD0, 0xD0, 0xD0],
        bg: [0x11, 0x13, 0x18],
        wide: false,
        emoji_presentation: false,
        text_presentation: false,
        bold: false,
        italic: false,
        underline: UnderlineStyle::None,
        strikethrough: false,
        overline: false,
        underline_color: None,
        overline_color: None,
    }
}

/// Build an enabled engine, Tier-A-scanned over an all-empty grid (every cell
/// eligible), and drive it through the PUBLIC weather inputs into a settled
/// WORKING downpour: sustained content deltas every engine tick push the
/// density EMA to its density-12 ceiling (byte 252), and ~20 s of simulated
/// clock lets the per-column cycles re-roll under that density so admission is
/// field-wide (the same downpour the in-crate budget tests pin, reached with
/// no test-only state pokes).
fn downpour(rows: usize, cols: usize) -> MatrixRain {
    let mut e = MatrixRain::new(RainConfig {
        enabled: true,
        density: 12,
        output_material: false,
        seed: 7,
        ..RainConfig::default()
    });
    let cells = vec![vec![space_cell(); cols]; rows];
    let sizes = vec![LineSize::SingleWidth; rows];
    e.rescan_from_cells(&cells, &sizes, &[], rows, cols, BG, 1);
    let g = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: rows as u16,
        cols: cols as u16,
    };
    let (mut q, mut a): (Vec<SpriteQuad>, Vec<RainHalo>) = (Vec::new(), Vec::new());
    for i in 0..600u64 {
        e.note_activity(i + 1); // sustained agent stream => WORKING
        e.advance_ms(33);
        e.emit(g, &RainTickInput::default(), &mut q, &mut a);
    }
    e
}

/// §8 perf gate — `bench_rain_tick_worstcase`: one engine tick + full field
/// emission at the design's worst-case geometry (200 columns x 50 rows, full
/// density-12 WORKING downpour, quad budget saturated so the truncation branch
/// is in play). Bar: median <= 150 µs.
#[test]
#[ignore = "perf gate (design §8): run manually in --release with --ignored --nocapture"]
fn bench_rain_tick_worstcase() {
    let (rows, cols) = (50usize, 200usize);
    let g = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: rows as u16,
        cols: cols as u16,
    };
    let mut e = downpour(rows, cols);
    let input = RainTickInput::default();
    let (mut q, mut a) = (Vec::new(), Vec::new());

    // Warm the measurement loop shape itself.
    for i in 0..8u64 {
        e.note_activity(1000 + i);
        e.advance_ms(33);
        e.emit(g, &input, &mut q, &mut a);
    }
    let iters = 200usize;
    let mut t = Vec::with_capacity(iters);
    let (mut qmin, mut qmax, mut amax) = (usize::MAX, 0usize, 0usize);
    for i in 0..iters as u64 {
        e.note_activity(2000 + i); // keep the WORKING stream alive
        e.advance_ms(33); // exactly one 30 Hz engine tick per emit
        let s = Instant::now();
        let fp = e.emit(g, &input, &mut q, &mut a);
        t.push(s.elapsed());
        assert_ne!(fp, 0, "the downpour must actually emit");
        qmin = qmin.min(q.len());
        qmax = qmax.max(q.len());
        amax = amax.max(a.len());
    }
    t.sort();
    let median = t[iters / 2];
    println!(
        "bench_rain_tick_worstcase: median {median:?} (p90 {:?}) per tick+emit \
         at 200x50 (10x20 px cells), quads {qmin}..{qmax}, halo quads <= {amax}",
        t[iters * 9 / 10],
    );
    assert!(
        qmax >= 1500,
        "non-vacuity: expected a saturated downpour, peak quads {qmax}"
    );
    assert!(
        median.as_nanos() < 150_000,
        "§8 gate: worst-case rain tick {median:?} >= 150 µs"
    );
}

/// §8 perf gate — `bench_rain_bake`: the full 64-tile ROM -> cell-metric
/// white-coverage bake (8 progressive batches of 8, exactly the per-tick
/// amortization the engine ships) plus the published-atlas snapshot. Bar:
/// median <= 3 ms TOTAL for all 64 tiles, at both a standard and a retina
/// cell metric.
#[test]
#[ignore = "perf gate (design §8): run manually in --release with --ignored --nocapture"]
fn bench_rain_bake() {
    let rom = rasterize_master();
    for (cw, ch) in [(10u16, 20u16), (20, 40)] {
        let mut baker = RainBaker::default();
        baker.begin_frame(cw, ch);
        // Warm: two full bakes.
        for _ in 0..2 {
            baker.restart();
            while !baker.complete() {
                baker.bake_tiles(&rom);
            }
            let _ = baker.atlas();
        }
        let iters = 40usize;
        let mut t = Vec::with_capacity(iters);
        let mut batches = 0usize;
        for _ in 0..iters {
            let s = Instant::now();
            baker.restart();
            while !baker.complete() {
                baker.bake_tiles(&rom);
                batches += 1;
            }
            let atlas = baker.atlas();
            t.push(s.elapsed());
            assert!(atlas.is_some(), "a finished bake publishes an atlas");
        }
        t.sort();
        let median = t[iters / 2];
        println!(
            "bench_rain_bake: median {median:?} for 64 tiles at {cw}x{ch} px \
             ({} batches/bake, amortized {:?}/batch incl. atlas publish)",
            batches / iters,
            median / (batches / iters) as u32,
        );
        assert!(
            median.as_millis() < 3,
            "§8 gate: 64-tile bake {median:?} >= 3 ms at {cw}x{ch}"
        );
    }
}
