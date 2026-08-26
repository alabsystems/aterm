// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a mouse DRAG costs the WF-1 settled-frame gate.
//!
//! `selection_extend` is the only `note_host_visual_change` caller a host
//! invokes at pointer cadence — every other one is config-time or one-shot. It
//! bumps `host_visual_gen` unconditionally, so a pointer that jitters INSIDE a
//! single cell reopens the gate for a frame whose only outcome is a
//! `DamageOutcome::GateHit` and zero present bands: a full `cell_frame_into`
//! resolve plus a full `compute_dirty_rows` row walk, to conclude nothing
//! changed.
//!
//! The A/B is by BUILD, not by arm — the same source is measured against the
//! two `aterm-wasm` revisions — so this file calls only APIs that exist in
//! both, and its in-bench assertions are the ones TRUE OF BOTH:
//!
//! - `jitter`: the host re-asserts the SAME (row, col) every frame. The
//!   assertion pins that the resolved selection really is identical, i.e. this
//!   arm is genuine cell-identical jitter and not a disguised move.
//! - `one_cell_move`: a real cell crossing every frame — the two-sided control.
//!   The assertion pins that the selection really does move AND that the gate
//!   stays open (`!last_render_skipped()`), which must hold before AND after;
//!   a patch that made this arm gate would be losing a real update.
//!
//! Gate behaviour itself (jitter must gate after the fix, must not before) is
//! pinned by the permanent unit tests in `crates/aterm-wasm/src/lib.rs`, not
//! here, precisely because that assertion cannot be true of both builds.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use aterm_wasm::AtermTerminal;

/// A terminal with a screenful of real content, rendered once so the renderer's
/// caches are warm and the first-frame cost is out of the measurement.
fn warm_term(rows: usize, cols: usize) -> Option<AtermTerminal> {
    let mut t = AtermTerminal::new_from_system(rows as u16, cols as u16, 14.0)?;
    for r in 0..rows {
        t.process(format!("line {r} \x1b[3{}mcolored\x1b[0m text here\r\n", r % 8).as_bytes());
    }
    t.render();
    Some(t)
}

/// The resolved selection as plain cell coordinates — the thing the renderer's
/// row diff actually compares. `None` when there is no selection.
fn span(t: &AtermTerminal) -> Option<(u16, u16, u16, u16)> {
    t.selection_range()
        .map(|r| (r.start_x(), r.start_y(), r.end_x(), r.end_y()))
}

fn wasm_selection_drag(c: &mut Criterion) {
    let sizes = [(24usize, 80usize), (50, 200)];
    let mut g = c.benchmark_group("wasm_selection_drag");

    for (rows, cols) in sizes {
        let label = format!("{rows}x{cols}");
        let row = (rows / 2) as i32;
        let anchor = (cols / 4) as u16;
        let head = anchor + 4;

        // 1) JITTER: the pointer moves sub-cell, so the host re-asserts the
        //    SAME cell every frame. This is the arm the fix targets.
        if let Some(mut t) = warm_term(rows, cols) {
            t.selection_start(row, anchor);
            t.selection_extend(row, head);
            t.render();
            let before = span(&t);
            assert!(before.is_some(), "jitter arm must have a live selection");
            t.selection_extend(row, head);
            // REACH GUARD (true of BOTH builds): a redundant extend leaves the
            // resolved selection byte-identical.
            assert_eq!(
                span(&t),
                before,
                "jitter arm must not move the selection by a cell"
            );
            t.render();
            g.bench_function(BenchmarkId::new("jitter", &label), |b| {
                b.iter(|| {
                    t.selection_extend(black_box(row), black_box(head));
                    t.render();
                    black_box(t.width());
                });
            });
        }

        // 2) ONE_CELL_MOVE: the two-sided control — a genuine cell crossing
        //    every frame, which must NEVER gate in either build.
        if let Some(mut t) = warm_term(rows, cols) {
            t.selection_start(row, anchor);
            t.selection_extend(row, head);
            t.render();
            let before = span(&t);
            t.selection_extend(row, head + 1);
            // REACH GUARD (true of BOTH builds): this arm really does move.
            assert_ne!(
                span(&t),
                before,
                "control arm must move the selection by a cell"
            );
            t.render();
            assert!(
                !t.last_render_skipped(),
                "a genuine one-cell move must never be gated away"
            );
            let mut tick = 0u32;
            g.bench_function(BenchmarkId::new("one_cell_move", &label), |b| {
                b.iter(|| {
                    tick = tick.wrapping_add(1);
                    let col = if tick.is_multiple_of(2) {
                        head
                    } else {
                        head + 1
                    };
                    t.selection_extend(black_box(row), black_box(col));
                    t.render();
                    black_box(t.width());
                });
            });
        }
    }
    g.finish();
}

criterion_group!(benches, wasm_selection_drag);
criterion_main!(benches);
