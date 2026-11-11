// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use aterm_core::render::{FxBlend, FxContent, FxInk, FxItem, FxRect, FxSlot};

use super::*;

fn layer(id: u64, space: FxSpace, y: i32, h: u16) -> FxLayer {
    FxLayer::new(
        id,
        Arc::new(FxContent::new(
            vec![FxItem::Rect(FxRect {
                x: 0,
                y,
                w: 4,
                h,
                ink: 0,
            })],
            vec![FxInk::add(0x0010_1010); 2],
        )),
        FxSlot::Under,
        space,
        FxBlend::Exact,
    )
}

fn marked(dirty: &[bool]) -> Vec<usize> {
    dirty
        .iter()
        .enumerate()
        .filter_map(|(r, &d)| d.then_some(r))
        .collect()
}

/// The band rule: row 0 opens to the top, the last row to the bottom, and
/// `grid_top` moves every band; a Grid layer is offset by it.
#[test]
fn bands_follow_the_renderer_band_rule() {
    let bands = RowBands::new(7, 10);
    let mut dirty = vec![false; 5];
    bands.mark_span(&mut dirty, -40, 3);
    assert_eq!(marked(&dirty), vec![0], "above the grid is row 0's band");
    dirty.fill(false);
    bands.mark_span(&mut dirty, 16, 18);
    assert_eq!(
        marked(&dirty),
        vec![0, 1],
        "y 16 and 17 lie in [7, 17) and [17, 27)"
    );
    dirty.fill(false);
    bands.mark_span(&mut dirty, 17, 18);
    assert_eq!(marked(&dirty), vec![1]);
    dirty.fill(false);
    bands.mark_span(&mut dirty, 56, 9_000);
    assert_eq!(
        marked(&dirty),
        vec![4],
        "below the grid is the last row's band"
    );
    dirty.fill(false);
    bands.mark_span(&mut dirty, 20, 20);
    assert!(marked(&dirty).is_empty(), "an empty span marks nothing");

    let mut dirty = vec![false; 5];
    mark_layer(&mut dirty, bands, &layer(1, FxSpace::Grid, 10, 1));
    assert_eq!(marked(&dirty), vec![1], "grid y 10 is window y 17");
    dirty.fill(false);
    mark_layer(&mut dirty, bands, &layer(1, FxSpace::Window, 10, 1));
    assert_eq!(marked(&dirty), vec![0], "window y 10 is in row 0's band");

    assert_eq!(bands.rows_px(0..1), (7, 17), "a row's cells, not its band");
    assert_eq!(bands.rows_px(2..4), (27, 47));
    assert_eq!(bands.rows_px(3..5), (37, 57));
    assert_eq!(bands.rows_px(3..3), (0, 0));
    assert_eq!(RowBands::new(7, usize::MAX).rows_px(0..2).1, i32::MAX - 1);

    for top in [1usize << (usize::BITS - 1), usize::MAX] {
        let bands = RowBands::new(top, 10);
        dirty.fill(false);
        mark_layer(&mut dirty, bands, &layer(2, FxSpace::Window, 0, 1));
        assert_eq!(
            marked(&dirty),
            vec![0],
            "the window layer is above a huge grid"
        );
        dirty.fill(false);
        mark_layer(&mut dirty, bands, &layer(3, FxSpace::Grid, 10, 1));
        assert_eq!(
            marked(&dirty),
            vec![1],
            "grid-local row survives its huge origin"
        );
        dirty.fill(false);
        mark_fx_scroll(
            &mut dirty,
            bands,
            (&[layer(4, FxSpace::Grid, 10, 1)], &[]),
            (&[], &[]),
            1,
        );
        assert_eq!(marked(&dirty), vec![0], "the old light slides up one row");
    }
    let bands = RowBands::new(0, usize::MAX);
    dirty.fill(false);
    mark_layer(&mut dirty, bands, &layer(5, FxSpace::Window, 10, 1));
    assert_eq!(
        marked(&dirty),
        vec![0],
        "a huge pitch has a positive first band"
    );
    dirty.fill(false);
    mark_fx_scroll(
        &mut dirty,
        bands,
        (&[layer(6, FxSpace::Window, 10, 1)], &[]),
        (&[], &[]),
        -1,
    );
    assert_eq!(
        marked(&dirty),
        vec![1],
        "a huge-pitch scroll preserves its row shift"
    );
}

/// A reorder of two unchanged overlapping layers changes their overlap's
/// pixels, so it is damage; a repeated id pairs in order.
#[test]
fn a_reorder_of_equal_layers_marks_them() {
    let bands = RowBands::new(0, 10);
    let (a, b) = (
        layer(1, FxSpace::Window, 0, 5),
        layer(2, FxSpace::Window, 30, 5),
    );
    let mut dirty = vec![false; 6];
    mark_layer_changes(
        &mut dirty,
        bands,
        &[a.clone(), b.clone()],
        &[a.clone(), b.clone()],
    );
    assert!(marked(&dirty).is_empty(), "equal layers mark nothing");
    mark_layer_changes(
        &mut dirty,
        bands,
        &[a.clone(), b.clone()],
        &[b.clone(), a.clone()],
    );
    assert_eq!(marked(&dirty), vec![0, 3], "a swap marks both");

    let (c, d) = (
        layer(9, FxSpace::Window, 10, 5),
        layer(9, FxSpace::Window, 40, 5),
    );
    let mut moved = d.clone();
    moved.offset.1 += 10;
    let mut dirty = vec![false; 6];
    mark_layer_changes(&mut dirty, bands, &[c.clone(), d], &[c, moved]);
    assert_eq!(
        marked(&dirty),
        vec![4, 5],
        "the k-th of an id pairs with the k-th"
    );
}

#[test]
fn large_interleaved_duplicates_pair_insertions_and_removals_by_occurrence() {
    let prev: Vec<_> = (0..32)
        .map(|i| layer(i % 8, FxSpace::Window, (i * 10) as i32, 5))
        .collect();
    let mut cur = prev.clone();
    cur.remove(0);
    for (before, after) in [(&prev, &cur), (&cur, &prev)] {
        let mut dirty = vec![false; 34];
        mark_layer_changes(&mut dirty, RowBands::new(0, 10), before, after);
        assert_eq!(marked(&dirty), vec![0, 8, 16, 24]);
    }
}

#[test]
fn large_population_changes_leave_the_unchanged_survivor_clean() {
    let many: Vec<_> = (0..32)
        .map(|i| layer(i, FxSpace::Window, (i * 10) as i32, 5))
        .collect();
    let one = vec![many[17].clone()];
    for (before, after) in [(&many, &one), (&one, &many)] {
        let mut dirty = vec![false; 34];
        mark_layer_changes(&mut dirty, RowBands::new(0, 10), before, after);
        assert_eq!(
            marked(&dirty),
            (0..32).filter(|&i| i != 17).collect::<Vec<_>>()
        );
    }
}

/// Measure stable animation, reorder, identity churn, appearance and
/// disappearance. No timing bound: compare the same matching workloads.
#[test]
#[ignore = "measurement: run with opt-test and --ignored --exact --nocapture"]
fn layer_identity_changes_report_damage_work() {
    const FRAMES: usize = 10_000;
    for kind in ["move", "reverse", "replace", "add", "remove"] {
        for count in [1, 8, 32, 128] {
            let mut prev: Vec<_> = (0..count)
                .map(|i| {
                    let id = if kind == "move" { i / 2 } else { i };
                    // Even, opaque ids; the odd counterparts are disjoint.
                    let id = (id as u64)
                        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                        .wrapping_mul(2);
                    layer(id, FxSpace::Window, (i * 10) as i32, 5)
                })
                .collect();
            let mut cur = prev.clone();
            match kind {
                "move" => cur[count / 2].offset.1 += 10,
                "reverse" => cur.reverse(),
                "replace" => cur.iter_mut().for_each(|layer| layer.id |= 1),
                "add" => prev.clear(),
                "remove" => cur.clear(),
                _ => unreachable!("closed measurement cases"),
            }
            let bands = RowBands::new(0, 10);
            let mut dirty = vec![false; count + 2];
            let mut step = |frame: usize| {
                let (before, after) = if frame & 1 == 0 {
                    (&prev, &cur)
                } else {
                    (&cur, &prev)
                };
                dirty.fill(false);
                std::hint::black_box(mark_fx_changes(
                    std::hint::black_box(&mut dirty),
                    bands,
                    (std::hint::black_box(before), &[]),
                    (std::hint::black_box(after), &[]),
                ));
                std::hint::black_box(&dirty);
            };
            for frame in 0..6 {
                step(frame);
            }
            let start = std::time::Instant::now();
            for frame in 0..FRAMES {
                step(frame);
            }
            let elapsed = start.elapsed();
            let want: Vec<_> = match kind {
                "move" => vec![count / 2, count / 2 + 1],
                "reverse" if count == 1 => Vec::new(),
                _ => (0..count).collect(),
            };
            assert_eq!(marked(&dirty), want, "{kind}, {count} layers");
            println!(
                "layer-id damage: kind={kind} layers={count} frames={FRAMES} elapsed={elapsed:?}"
            );
        }
    }
}

/// The band of a row is i128's quotient on both sides of where the
/// distance below the grid stops fitting u64's one-instruction divide.
#[test]
fn a_band_is_the_same_on_either_side_of_u64() {
    let bands = RowBands::new(3, 7);
    let rows = usize::MAX;
    let edge = i128::from(u64::MAX);
    for d in [0, 6, 7, 700, edge - 1, edge, edge + 1, edge * 9] {
        let q = d / 7;
        let want = if q > (rows - 1) as i128 {
            rows - 1
        } else {
            q as usize
        };
        assert_eq!(bands.band_of(3 + d, rows), want, "{d} px below the grid");
    }
}

/// A changed layer marks the rows each of its items stands on, never the
/// rows between two items far apart (the hull would); its clip still cuts
/// an item, and a Grid layer still moves down by `grid_top`.
#[test]
fn a_changed_layer_marks_its_items_rows_not_their_hull() {
    let bands = RowBands::new(0, 10);
    let item = |y: i32| {
        FxItem::Rect(FxRect {
            x: 0,
            y,
            w: 4,
            h: 3,
            ink: 0,
        })
    };
    let sky = |space| {
        FxLayer::new(
            7,
            Arc::new(FxContent::new(
                vec![item(2), item(71)],
                vec![FxInk::add(0x0010_1010); 2],
            )),
            FxSlot::Glow,
            space,
            FxBlend::Exact,
        )
    };
    let mut dirty = vec![false; 10];
    mark_layer_changes(&mut dirty, bands, &[], &[sky(FxSpace::Window)]);
    assert_eq!(marked(&dirty), vec![0, 7], "two stars, two rows");

    let mut dirty = vec![false; 10];
    dirty[1..7].fill(true);
    mark_layer(&mut dirty, bands, &sky(FxSpace::Window));
    assert_eq!(
        marked(&dirty),
        (0..8).collect::<Vec<_>>(),
        "the walk stops only once the last open band is marked"
    );

    let mut cut = sky(FxSpace::Window);
    cut.clip = PxRect::new(0, 60, 100, 100);
    let mut dirty = vec![false; 10];
    mark_layer_changes(&mut dirty, bands, &[], &[cut]);
    assert_eq!(marked(&dirty), vec![7], "the clip cuts the item above it");

    let low = RowBands::new(15, 10);
    let mut dirty = vec![false; 10];
    mark_layer_changes(&mut dirty, low, &[sky(FxSpace::Window)], &[]);
    assert_eq!(
        marked(&dirty),
        vec![0, 5],
        "a Window layer under a 15 px head"
    );
    let mut dirty = vec![false; 10];
    mark_layer_changes(&mut dirty, low, &[sky(FxSpace::Grid)], &[]);
    assert_eq!(
        marked(&dirty),
        vec![0, 7],
        "a Grid layer sits grid_top lower"
    );
}
