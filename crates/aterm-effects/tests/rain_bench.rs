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
    EffectGeom, MatrixRain, RainConfig, RainSignal, RainTickInput, bake::RainBaker,
    rom::rasterize_master,
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
        bold: false,
        italic: false,
        underline: UnderlineStyle::None,
        strikethrough: false,
        overline: false,
        underline_color: None,
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

/// Semantic literal-output hot-path gate: the production-default material
/// mode is populated from a real mixed text/blank frame, Execute choreography
/// stays active, and every measured emit consumes one WORKING engine tick.
/// This prices the per-quad semantic tape traversal that the classic fixture
/// above deliberately does not exercise.
#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_semantic_literal_tick_worstcase() {
    let (rows, cols) = (50usize, 200usize);
    let g = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: rows as u16,
        cols: cols as u16,
    };
    let mut cells = vec![vec![space_cell(); cols]; rows];
    for (i, cell) in cells[0].iter_mut().enumerate() {
        cell.ch = char::from_u32(33 + (i % 64) as u32).expect("printable ASCII material");
    }
    for (i, cell) in cells[1].iter_mut().enumerate() {
        cell.ch = char::from_u32(33 + ((i + 17) % 64) as u32).expect("printable ASCII material");
    }
    let sizes = vec![LineSize::SingleWidth; rows];
    let mut e = MatrixRain::new(RainConfig {
        enabled: true,
        density: 12,
        output_material: true,
        seed: 19,
        ..RainConfig::default()
    });
    e.rescan_from_cells(&cells, &sizes, &[], rows, cols, BG, 1);
    e.sample_material(&cells, rows, Some((rows as u16 - 1, 0)), &[]);
    assert!(
        e.notes_can_wake(),
        "non-vacuity: the real output-material bank must be populated"
    );

    let input = RainTickInput::default();
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for i in 0..600u64 {
        e.note_activity(i + 1);
        e.note_signal(RainSignal::Execute as u32, 8);
        e.advance_ms(33);
        e.emit(g, &input, &mut q, &mut a);
    }

    let iters = 200usize;
    let mut samples = Vec::with_capacity(iters);
    let (mut qmin, mut qmax) = (usize::MAX, 0usize);
    for i in 0..iters as u64 {
        e.note_activity(10_000 + i);
        e.note_signal(RainSignal::Execute as u32, 8);
        e.advance_ms(33);
        let start = Instant::now();
        let fp = e.emit(g, &input, &mut q, &mut a);
        samples.push(start.elapsed());
        assert_ne!(fp, 0, "semantic literal downpour must emit");
        qmin = qmin.min(q.len());
        qmax = qmax.max(q.len());
    }
    let distinct_tiles = q
        .iter()
        .map(|quad| (quad.ax, quad.ay))
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    samples.sort();
    let median = samples[iters / 2];
    let p90 = samples[iters * 9 / 10];
    println!(
        "bench_semantic_literal_tick_worstcase: median {median:?} (p90 {p90:?}) per tick+emit at 200x50, quads {qmin}..{qmax}, final-frame literal tiles {distinct_tiles}"
    );
    assert!(
        qmax >= 1_200,
        "non-vacuity: expected a dense semantic literal downpour, peak quads {qmax}"
    );
    assert!(
        distinct_tiles >= 8,
        "non-vacuity: expected multiple real material tiles, saw {distinct_tiles}"
    );
    assert!(
        median.as_micros() < 300,
        "semantic literal tick median {median:?} >= 300 us"
    );
    assert!(
        p90.as_micros() < 600,
        "semantic literal tick p90 {p90:?} >= 600 us"
    );
}

/// Damage-rescan gate for a frame-heavy TUI: forty simultaneous titled boxes
/// exercise candidate tracking, side validation, region recognition, and
/// word-wise interior masking over the full 200x50 grid. Closed boxes must
/// mask the only cells not already removed by border clearance; opening their
/// bottoms proves that the zero-output check is not vacuous.
#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_framed_tui_rescan() {
    let (rows, cols) = (50usize, 200usize);
    let mut cells = vec![vec![space_cell(); cols]; rows];
    let (top, bottom, box_w) = (1usize, 44usize, 5usize);
    for left in (0..cols).step_by(box_w) {
        let right = left + box_w - 1;
        cells[top][left].ch = '╭';
        cells[top][left + 1].ch = '─';
        cells[top][left + 2].ch = 'T';
        cells[top][left + 3].ch = '─';
        cells[top][right].ch = '╮';
        for row in cells.iter_mut().take(bottom).skip(top + 1) {
            row[left].ch = '│';
            row[right].ch = '│';
        }
        cells[bottom][left].ch = '╰';
        cells[bottom][left + 1].ch = '─';
        cells[bottom][left + 2].ch = '─';
        cells[bottom][left + 3].ch = '─';
        cells[bottom][right].ch = '╯';
    }
    let sizes = vec![LineSize::SingleWidth; rows];
    let mut e = downpour(rows, cols);

    for epoch in 2..12 {
        e.rescan_from_cells(&cells, &sizes, &[], rows, cols, BG, epoch);
    }
    let iters = 300usize;
    let mut samples = Vec::with_capacity(iters);
    for i in 0..iters {
        let start = Instant::now();
        e.rescan_from_cells(&cells, &sizes, &[], rows, cols, BG, 12 + i as u64);
        samples.push(start.elapsed());
    }
    samples.sort();
    let median = samples[iters / 2];
    let p90 = samples[iters * 9 / 10];
    println!(
        "bench_framed_tui_rescan: median {median:?} (p90 {p90:?}) for 40 closed titled panels at 200x50"
    );
    assert!(
        median.as_micros() < 750,
        "frame-heavy rescan median {median:?} >= 750 us"
    );
    assert!(
        p90.as_micros() < 1_500,
        "frame-heavy rescan p90 {p90:?} >= 1.5 ms"
    );

    // Adversarial discovery shape from the review: thousands of left corners
    // with no matching right corner. The detector must remain one scan per row
    // rather than restarting a suffix search at every left corner.
    let adversarial_cols = 4096usize;
    let mut unmatched = vec![vec![space_cell(); adversarial_cols]; rows];
    for col in (0..adversarial_cols).step_by(2) {
        unmatched[1][col].ch = '╭';
    }
    let adversarial_sizes = vec![LineSize::SingleWidth; rows];
    let mut adversarial = MatrixRain::new(RainConfig {
        enabled: true,
        output_material: false,
        ..RainConfig::default()
    });
    let mut adversarial_samples = Vec::with_capacity(50);
    for epoch in 0..50u64 {
        let start = Instant::now();
        adversarial.rescan_from_cells(
            &unmatched,
            &adversarial_sizes,
            &[],
            rows,
            adversarial_cols,
            BG,
            epoch,
        );
        adversarial_samples.push(start.elapsed());
    }
    adversarial_samples.sort();
    let adversarial_p90 = adversarial_samples[45];
    println!(
        "bench_framed_tui_rescan: unmatched-left p90 {adversarial_p90:?} at {adversarial_cols}x{rows}"
    );
    assert!(
        adversarial_p90.as_millis() < 10,
        "linear unmatched-left scan p90 {adversarial_p90:?} >= 10 ms"
    );

    let g = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: rows as u16,
        cols: cols as u16,
    };
    let input = RainTickInput::default();
    let (mut q, mut a) = (Vec::new(), Vec::new());
    e.advance_ms(33);
    assert_eq!(
        e.emit(g, &input, &mut q, &mut a),
        0,
        "closed frame interiors must be fully masked"
    );
    assert!(q.is_empty());

    for cell in &mut cells[bottom] {
        cell.ch = ' ';
    }
    e.rescan_from_cells(&cells, &sizes, &[], rows, cols, BG, u64::MAX);
    let mut saw_open_field = false;
    for i in 0..60u64 {
        e.note_activity(20_000 + i);
        e.advance_ms(33);
        e.emit(g, &input, &mut q, &mut a);
        saw_open_field |= !q.is_empty();
    }
    assert!(
        saw_open_field,
        "non-vacuity: opening the same panels must restore eligible interior rain"
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

/// Literal-output refresh gate: scan a worst-case 200x50 nonblank viewport,
/// select the latest bounded material tape, author 64 real character masters,
/// and atomically bake the complete Retina atlas. This is the expensive edge
/// (a genuinely changed visible charset), not the steady-state frame path.
#[test]
#[ignore = "perf gate: run manually in --release with --ignored --nocapture"]
fn bench_literal_material_refresh() {
    let (rows, cols) = (50usize, 200usize);
    let sizes = vec![LineSize::SingleWidth; rows];
    let build_cells = |first: u32| {
        (0..rows)
            .map(|r| {
                (0..cols)
                    .map(|c| {
                        let mut cell = space_cell();
                        cell.ch = char::from_u32(first + ((r * cols + c) % 64) as u32)
                            .expect("ASCII material character");
                        cell
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    // Two 64-character ASCII alphabets that differ by one glyph. Alternating
    // them guarantees every measured iteration takes the changed-charset path.
    let a = build_cells(33);
    let b = build_cells(34);

    let mut e = MatrixRain::new(RainConfig {
        enabled: true,
        density: 12,
        output_material: true,
        seed: 11,
        ..RainConfig::default()
    });
    e.rescan_from_cells(&a, &sizes, &[], rows, cols, BG, 1);
    let g = EffectGeom {
        cell_w: 20,
        cell_h: 40,
        rows: rows as u16,
        cols: cols as u16,
    };
    let (mut q, mut halos) = (Vec::new(), Vec::new());
    e.sample_material(&a, rows, None, &[]);
    e.emit(g, &RainTickInput::default(), &mut q, &mut halos);

    for cells in [&a, &b, &a, &b] {
        e.sample_material(cells, rows, None, &[]);
    }
    let iters = 100usize;
    let mut samples = Vec::with_capacity(iters);
    let mut previous_version = e.atlas_version();
    for i in 0..iters {
        let cells = if i.is_multiple_of(2) { &a } else { &b };
        let start = Instant::now();
        e.sample_material(cells, rows, None, &[]);
        let atlas = e.rain_atlas();
        samples.push(start.elapsed());
        assert!(
            atlas.is_some(),
            "literal refresh publishes the complete atlas"
        );
        let version = e.atlas_version();
        assert_ne!(
            version, previous_version,
            "charset edge must publish a new atlas"
        );
        previous_version = version;
    }
    samples.sort();
    let median = samples[iters / 2];
    let p90 = samples[iters * 9 / 10];
    println!(
        "bench_literal_material_refresh: median {median:?} (p90 {p90:?}) for scan + 64 literal glyph Retina bake + atlas publish at 200x50"
    );
    assert!(
        median.as_micros() < 2_000,
        "literal material refresh {median:?} >= 2 ms"
    );
    assert!(
        p90.as_micros() < 2_000,
        "literal material refresh p90 {p90:?} >= 2 ms"
    );
}
