// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// IMG-1 pricing bench: the per-frame dirty-row diff cost when an inline image
// is RE-TRANSMITTED — fresh `Arc<ImageData>`, byte-identical payload, the
// yazi/ranger preview-redraw and kitty still-scene video shape.
//
// `row_differs`' images clause used to hit the derived deep equality at EVERY
// covered cell whenever the prev/cur Arcs were pointer-distinct (std's `Arc`
// eq only short-circuits on pointer-EQUAL), restarting the payload memcmp per
// cell: O(covered_cells × payload_bytes) per present — for this bench's shape
// (4 000 covered cells × 8 MiB) that is up to ~32 GB of memcmp for ONE frame
// diff. The call-scoped `ImageEqMemo` prices the deep compare once per
// DISTINCT Arc pair instead, so the same diff pays ONE 8 MiB compare.
//   cargo bench -p aterm-render --bench image_diff
//
// Targets:
//   * image_diff_retransmit_8mib_4000cells — THE pathological shape (the fix's
//     headline number: per-covered-cell memcmp → one compare per Arc pair).
//   * image_diff_steady_same_arc_4000cells — no re-transmit (same Arc both
//     frames): the `Arc::ptr_eq` floor. A regression fence — this must stay
//     ~free in both the old and new worlds.
//   * image_diff_no_images_50x120 — THE COMMON FRAME (no images at all): the
//     regression fence for rerouting a per-row, per-frame function through the
//     memo. Must stay flat.
//   * image_diff_payload_differs_first_byte_4000cells — cheap-inequality
//     floor: the payloads differ at byte 0, so even the old per-cell compare
//     exited immediately. Fences that the memo cannot REGRESS the
//     cheap-difference case (one early-exit compare + one memo insert).
//
// The result verdicts are guarded two-sidedly below (gate-hit for the
// re-transmit, repaint for the changed payload), so the workload provably
// reaches the deep-equality path it prices — a diff that compared pointers
// would fail the first guard; one that skipped the compare would fail the
// second.

use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef};
use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;
use aterm_render::{DirtyDecision, compute_dirty_rows, is_unchanged_frame};
use criterion::{Criterion, black_box};
use std::sync::Arc;

/// Grid and footprint of the workload: a 50×120 viewport with a 40-row ×
/// 100-col image footprint = 4 000 covered cells (≥ the 1 000-cell reach bar),
/// carrying an 8 MiB payload (the raw-RGBA kitty video / large-PNG scale).
const ROWS: usize = 50;
const COLS: usize = 120;
const IMG_ROWS: usize = 40;
const IMG_COLS: usize = 100;
const PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

/// A deterministic 8 MiB pseudo-payload. The diff never decodes — bytes are
/// compared, not interpreted — so the content only needs to be big and
/// deterministic, not a valid PNG.
fn payload() -> Vec<u8> {
    (0..PAYLOAD_BYTES).map(|i| (i % 251) as u8).collect()
}

/// A fresh `Arc<ImageData>` around `bytes` — each call is one "transmission"
/// (a distinct allocation, exactly what a client re-send produces).
fn transmit(bytes: Vec<u8>) -> Arc<ImageData> {
    Arc::new(ImageData {
        bytes,
        format: ImageFormat::Png,
        cols: IMG_COLS as u16,
        rows: IMG_ROWS as u16,
        z_index: 0,
    })
}

/// A full-viewport snapshot whose images field covers `IMG_ROWS × IMG_COLS`
/// cells with tiles of `img` — the per-cell shape `images_frame_into` emits.
fn input_with(template: &RenderInput, img: &Arc<ImageData>) -> RenderInput {
    let mut input = template.clone();
    for (r, row) in input.images.iter_mut().enumerate().take(IMG_ROWS) {
        for c in 0..IMG_COLS {
            row.push((
                c,
                ImageRef {
                    image: img.clone(),
                    cell_row: r as u16,
                    cell_col: c as u16,
                },
            ));
        }
    }
    input
}

fn main() {
    let mut c = Criterion::default().configure_from_args();

    // The engine builds the base snapshot; the bench mutates only `images`.
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    let template = term.cell_frame(ROWS, COLS);

    let bytes = payload();
    let prev = input_with(&template, &transmit(bytes.clone()));
    // The RE-TRANSMIT: a DISTINCT Arc carrying byte-identical content.
    let cur_retransmit = input_with(&template, &transmit(bytes.clone()));
    // The same-Arc steady frame (no re-transmit happened).
    let cur_same_arc = {
        let (_c0, first) = &prev.images[0][0];
        input_with(&template, &first.image)
    };
    // A first-byte payload change: the cheap-inequality floor.
    let mut changed = bytes;
    changed[0] ^= 1;
    let cur_diff = input_with(&template, &transmit(changed));

    // REACH GUARDS (two-sided). (1) The footprint is big enough to price the
    // per-covered-cell multiplier the memo removes.
    let covered: usize = prev.images.iter().map(Vec::len).sum();
    assert!(
        covered >= 1000,
        "workload must cover >= 1000 cells to have reach (got {covered})"
    );
    // (2) POSITIVE: the re-transmit gate-hits — the diff really consulted
    // cross-Arc CONTENT equality (pointer identity alone would say "differs").
    assert!(
        is_unchanged_frame(&prev, false, None, &cur_retransmit, false, None, 16),
        "byte-identical re-transmit must gate-hit"
    );
    // (3) NEGATIVE: a genuine payload change still repaints — the deep compare
    // was priced, not dropped.
    assert!(
        !is_unchanged_frame(&prev, false, None, &cur_diff, false, None, 16),
        "a changed payload must not gate-hit"
    );

    let mut dirty: Vec<bool> = Vec::new();
    c.bench_function("image_diff_retransmit_8mib_4000cells", |b| {
        b.iter(|| {
            let d = compute_dirty_rows(
                black_box(&prev),
                black_box(&cur_retransmit),
                false,
                None,
                false,
                None,
                16,
                &mut dirty,
            );
            assert!(matches!(d, DirtyDecision::Rows(_)));
            black_box(&dirty);
        });
    });

    c.bench_function("image_diff_steady_same_arc_4000cells", |b| {
        b.iter(|| {
            let d = compute_dirty_rows(
                black_box(&prev),
                black_box(&cur_same_arc),
                false,
                None,
                false,
                None,
                16,
                &mut dirty,
            );
            assert!(matches!(d, DirtyDecision::Rows(_)));
            black_box(&dirty);
        });
    });

    c.bench_function("image_diff_payload_differs_first_byte_4000cells", |b| {
        b.iter(|| {
            let d = compute_dirty_rows(
                black_box(&prev),
                black_box(&cur_diff),
                false,
                None,
                false,
                None,
                16,
                &mut dirty,
            );
            assert!(matches!(d, DirtyDecision::Rows(_)));
            black_box(&dirty);
        });
    });

    // FENCE — THE COMMON FRAME: no images at all. `row_differs_shifted` runs for
    // every row of every frame, and IMG-1 reroutes its images clause from the
    // derived `Vec` `==` through `ImageEqMemo::rows_eq`. An image-free frame
    // must not pay a byte for that: `rows_eq` is a `0 == 0` length compare
    // there and the `FxHashMap` never allocates (no insert can happen without
    // a cross-`Arc` pair). This target prices that claim instead of asserting
    // it — it is the one path the rest of the suite cannot reach
    // (`image_diff_steady_same_arc_4000cells` still places 4 000 image cells,
    // `image_plane` only calls the static `pack_image_plane`, and
    // `hyperlink_screen` never renders a frame).
    let mut free_term = Terminal::new(ROWS as u16, COLS as u16);
    for r in 0..ROWS {
        free_term.process(format!("\x1b[{};1H", r + 1).as_bytes());
        free_term.process(b"lorem ipsum dolor sit amet consectetur adipiscing elit sed do");
    }
    let free_prev = free_term.cell_frame(ROWS, COLS);
    // One changed cell: the row scan runs in full and returns Rows, so the
    // images clause is consulted for every row (a gate hit would still consult
    // it, but Rows is the shape the dirty path actually takes).
    free_term.process(b"\x1b[1;1HX");
    let free_cur = free_term.cell_frame(ROWS, COLS);
    // REACH GUARDS: image-free on BOTH sides (or this is not the common frame),
    // and a real row-diff verdict (not the FullRepaint early-out, which would
    // return before the row loop and measure nothing).
    assert!(
        free_prev.images.iter().all(|row| row.is_empty())
            && free_cur.images.iter().all(|row| row.is_empty()),
        "the image-free fence must carry no images at all"
    );
    assert!(
        matches!(
            compute_dirty_rows(
                &free_prev, &free_cur, false, None, false, None, 16, &mut dirty
            ),
            DirtyDecision::Rows(_)
        ),
        "the image-free fence must reach the per-row diff, not FullRepaint"
    );
    c.bench_function("image_diff_no_images_50x120", |b| {
        b.iter(|| {
            let d = compute_dirty_rows(
                black_box(&free_prev),
                black_box(&free_cur),
                false,
                None,
                false,
                None,
                16,
                &mut dirty,
            );
            assert!(matches!(d, DirtyDecision::Rows(_)));
            black_box(&dirty);
        });
    });

    c.final_summary();
}
