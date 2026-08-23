// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// FRM-4 pricing bench: the CPU image BLIT path's per-cell DECODED-IMAGE CACHE
// traffic.
//   cargo bench -p aterm-render --bench image_blit
//
// WHY THIS BENCH DID NOT EXIST. `benches/image_diff.rs` prices the row DIFF
// (`compute_dirty_rows` / `ImageEqMemo`) and `aterm-gpu/benches/image_plane.rs`
// prices the GPU plane packer. Neither reaches `Renderer::blit_image_cell`,
// which is the CPU compositor's per-covered-cell image writer and the ONLY
// consumer of `ImageCache`.
//
// WHAT IT PRICES. `blit_image_cell` resolves the decoded image through
// `ImageCache` for EVERY covered cell — historically twice: once as an
// `.is_none()` presence probe and once to take the borrow (the second call
// exists because `get` takes `&mut self`, so the borrow cannot be held across
// the `put` in the miss arm). Each resolve is a linear `Arc::ptr_eq` +
// footprint scan of the entry list followed by a `Vec::remove(idx)` + `push`
// MRU rotation of a ~64-byte tuple. The `(Arc, fp_w, fp_h)` key is CONSTANT
// across every cell of one placement, so on a warm cache all of that is
// re-derivation of a value that changes once per placement.
//
// THE TWO ARMS BRACKET THE SCAN LENGTH, which is `entries.len()` and NOT the
// `MAX_ENTRIES = 64` ceiling (all 4 000 cells of one placement share ONE cache
// entry — one `Arc`, one footprint):
//   * image_blit_1image_4000cells  — the single-placement floor: a 1-entry
//     cache, so each resolve is one `Arc::ptr_eq` plus the rotation. This is
//     what a `kitten icat` / `imgcat` of one picture actually costs.
//   * image_blit_8images_4000cells — the contact-sheet shape (lsix, `icat` over
//     a directory): eight stacked placements, so the cache holds eight entries
//     and a resolve scans up to eight. Same 4 000 covered cells, same blit work,
//     only the scan length differs — so the pair isolates the lookup from the
//     blend.
//
// THE FRAME SHAPE IS THE ONE THAT ACTUALLY REPAINTS. `row_differs_shifted`
// compares `a.images[r]` against `b.images[r]` through `ImageEqMemo`, so a
// SETTLED image row is not dirty and costs zero blits per frame, forever. The
// arms therefore alternate two snapshots that differ in one TEXT column of every
// image row (column `COLS - 1`, outside the placement): the rows are genuinely
// dirty every frame — the animating/scrolling/redrawn-over case — while the
// image `Arc` stays cache-resident, which is exactly the regime the per-cell
// cache traffic lives in.

use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef};
use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, WindowCpu};
use criterion::{Criterion, black_box};
use std::sync::Arc;

/// The viewport and the placement footprint: 40 rows x 100 cols = 4 000 covered
/// cells, comfortably past the 1 000-cell reach bar the image_diff bench uses.
const ROWS: usize = 50;
const COLS: usize = 120;
const IMG_ROWS: usize = 40;
const IMG_COLS: usize = 100;

/// A deterministic gradient-plus-noise RGBA PNG at exactly `w` x `h` pixels.
///
/// Sized to the footprint by the caller so `decode_image_to_footprint` takes its
/// identity path: the bench is pricing the per-cell CACHE traffic and the blend,
/// not a resample. The payload is decoded once (in the untimed warm present) and
/// served from the cache thereafter.
fn footprint_png(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    for y in 0..h {
        for x in 0..w {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let n = s.to_le_bytes();
            rgba.extend_from_slice(&[
                n[0].wrapping_add((x * 255 / w.max(1)) as u8),
                n[1].wrapping_add((y * 255 / h.max(1)) as u8),
                n[2],
                // Partly transparent, so the blit takes the real straight-alpha
                // `blend` on every pixel instead of a saturating overwrite.
                200,
            ]);
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&rgba).expect("png data");
    }
    out
}

/// `input` with the top `IMG_ROWS` rows covered by `imgs`, each placement
/// spanning `rows_per` consecutive rows and `IMG_COLS` columns — the flat
/// per-covered-cell `(col, ImageRef)` list `images_frame_into` emits.
fn with_images(input: &RenderInput, imgs: &[Arc<ImageData>], rows_per: usize) -> RenderInput {
    let mut out = input.clone();
    for (r, row) in out.images.iter_mut().enumerate().take(IMG_ROWS) {
        row.clear();
        let idx = (r / rows_per).min(imgs.len() - 1);
        for c in 0..IMG_COLS {
            row.push((
                c,
                ImageRef {
                    image: imgs[idx].clone(),
                    cell_row: (r - idx * rows_per) as u16,
                    cell_col: c as u16,
                },
            ));
        }
    }
    out
}

/// One placement's payload, sized to `rows_per` x `IMG_COLS` cells at the
/// renderer's own cell metrics.
fn placement(cw: usize, ch: usize, rows_per: usize, seed: u32) -> Arc<ImageData> {
    Arc::new(ImageData {
        bytes: footprint_png(
            (IMG_COLS * cw) as u32,
            (rows_per * ch) as u32,
            seed,
        ),
        format: ImageFormat::Png,
        cols: IMG_COLS as u16,
        rows: rows_per as u16,
        // `>= 0`: the image OWNS the cell (the iTerm2/Sixel default and Kitty
        // `z=0`), which is the pass-2b blit path — not the `z<0` below-text tier.
        z_index: 0,
        band_lift_px: 0,
    })
}

/// Render `input` through the SHIPPING damage-tracked entry and keep the frame
/// observable, so nothing in the raster can be optimized away.
fn present(r: &mut Renderer, wc: &mut WindowCpu, input: &RenderInput) {
    let view = r.render_input_cached(wc, input);
    black_box(view.width());
    black_box(wc.frame_pixels().as_ptr());
}

/// Warm a window on the pair, then PROVE — two-sided — that a measured frame
/// really blits a multi-thousand-cell placement, and report the state.
///
/// TARGET half: `last_image_cells()` counts `blit_image_cell` entries, a number
/// the cache-traffic change does not move, so the same assertion is valid on
/// both sides of it. A frame that blitted nothing (settled image rows — the
/// default state of a still picture) reports 0 and fails here.
///
/// CONTROL half: the caller passes the IMAGE-FREE twin of the same script; it
/// must report exactly 0. Without it, "the counter is positive" would also be
/// satisfied by a counter that simply counted rows.
fn warm_and_verify(
    label: &str,
    r: &mut Renderer,
    wc: &mut WindowCpu,
    a: &RenderInput,
    b: &RenderInput,
    control_a: &RenderInput,
    control_b: &RenderInput,
) {
    for _ in 0..4 {
        present(r, wc, a);
        present(r, wc, b);
    }
    present(r, wc, a);
    let cells = r.last_image_cells();
    assert!(
        cells as usize >= 1000,
        "{label}: a measured frame blitted {cells} image cells (< 1000) — the \
         workload is not repainting the placement, so it prices nothing"
    );

    let mut ctl = WindowCpu::new();
    for _ in 0..4 {
        present(r, &mut ctl, control_a);
        present(r, &mut ctl, control_b);
    }
    present(r, &mut ctl, control_a);
    let ctl_cells = r.last_image_cells();
    assert_eq!(
        ctl_cells, 0,
        "{label}: the IMAGE-FREE control frame still blitted {ctl_cells} image \
         cells — the counter is not reading the image path"
    );
    println!("REACH {label:<28} | {cells} image cells blitted per frame | control 0");

    // Leave the timed window on the same alternating phase the arm continues from.
    present(r, wc, b);
}

fn main() {
    let mut c = Criterion::default().configure_from_args();
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP bench: no system font available");
        return;
    };
    let (cw, ch) = r.cell_size();

    // The engine builds the base snapshot; the bench only attaches images.
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    let mut line = Vec::with_capacity(COLS + 8);
    for row in 0..ROWS {
        line.clear();
        for col in 0..COLS - 1 {
            line.push(b'a' + ((row + col) % 26) as u8);
        }
        if row + 1 < ROWS {
            line.extend_from_slice(b"\r\n");
        }
        term.process(&line);
    }
    // The one-column difference that keeps every image row DIRTY, written at
    // column `COLS` (1-based) — outside the `IMG_COLS`-wide placement, so the
    // images list is byte-identical between the two snapshots and only the text
    // clause of `row_differs` fires.
    fn stamp(term: &mut Terminal, ch: u8) {
        for row in 0..IMG_ROWS {
            term.process(format!("\x1b[{};{COLS}H", row + 1).as_bytes());
            term.process(&[ch]);
        }
    }
    stamp(&mut term, b'x');
    let plain_a = term.cell_frame(ROWS, COLS);
    stamp(&mut term, b'y');
    let plain_b = term.cell_frame(ROWS, COLS);

    // ONE placement over all 4 000 cells: a 1-entry cache.
    let one = [placement(cw, ch, IMG_ROWS, 1)];
    let one_a = with_images(&plain_a, &one, IMG_ROWS);
    let one_b = with_images(&plain_b, &one, IMG_ROWS);

    // EIGHT stacked placements over the same 4 000 cells: an 8-entry cache, so a
    // resolve scans eight entries instead of one. Same blit work either way.
    let rows_per = IMG_ROWS / 8;
    let many: Vec<Arc<ImageData>> = (0..8u32)
        .map(|i| placement(cw, ch, rows_per, i + 2))
        .collect();
    let many_a = with_images(&plain_a, &many, rows_per);
    let many_b = with_images(&plain_b, &many, rows_per);

    for (label, a, b) in [
        ("image_blit_1image_4000cells", &one_a, &one_b),
        ("image_blit_8images_4000cells", &many_a, &many_b),
    ] {
        let mut win = WindowCpu::new();
        warm_and_verify(label, &mut r, &mut win, a, b, &plain_a, &plain_b);
        let mut flip = false;
        c.bench_function(label, |bench| {
            bench.iter(|| {
                flip = !flip;
                present(&mut r, &mut win, if flip { a } else { b });
            });
        });
    }

    c.final_summary();
}
