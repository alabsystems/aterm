// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef, ImageScaling};
use aterm_core::selection::{SelectionSide, SelectionType};
use std::sync::Arc;

fn snapshot(term: &mut Terminal) -> (RenderInput, PetWorldFacts) {
    term.process(b"\x1b]11;#000000\x07");
    let rows = usize::from(term.grid().rows());
    let cols = usize::from(term.grid().cols());
    let input = term.cell_frame(rows, cols);
    let facts = PetWorldFacts::read(term, 7);
    (input, facts)
}

fn observe(term: &mut Terminal) -> PetWorld {
    let (input, facts) = snapshot(term);
    let mut world = PetWorld::default();
    assert!(world.observe(&input, &facts, PetPane::full(&input)));
    world
}

/// A stand's feet window is the same at every row: a stand on row `r` (the resident's
/// `r + 1 - 1.7` tall body less its 0.1 clearance, with that 0.1 as the margin) reaches row
/// `r` and never the row under its feet. Its f32 sum is `r + 1.000001` at some rows (10 and
/// 12 among them), which took the row under the feet there and not at 6 or 20, so the
/// caret's line was a seat by its row number.
#[test]
fn a_stands_feet_window_never_takes_the_row_under_it_at_any_row() {
    let mut term = Terminal::new(30, 40);
    term.process(b"\x1b[1;1Hx");
    let world = observe(&mut term);
    for r in 2..28u16 {
        let r = f32::from(r);
        let feet = PetRect::new(r + 1.0 - 1.7, 3.0, 1.7 - 0.1, 5.2);
        match world.window(feet, 0.1) {
            Window::Cells(r0, _, r1, _) => assert_eq!(
                (r0, r1),
                ((r - 1.0) as usize, r as usize + 1),
                "a stand on row {r}: rows {r0}..{r1}"
            ),
            other => panic!("a stand on row {r}: {other:?}"),
        }
    }
}

/// A row of rule strokes across the pane is a line ([`PetWorld::rule_line`]); a row of text,
/// a short run of strokes and a row of other box drawing are not. Off the map it is unknown.
#[test]
fn a_row_of_rule_strokes_across_the_pane_is_a_rule_line() {
    let mut term = Terminal::new(10, 40);
    let rule = "\u{2500}".repeat(40);
    let most = format!(
        "{} alab claude {}",
        "\u{2500}".repeat(2),
        "\u{2500}".repeat(25)
    );
    let short = "\u{2500}".repeat(20);
    let boxed = "\u{2550}".repeat(40);
    term.process(
        format!(
            "\x1b[2;1H{rule}\x1b[3;1Hhello there\x1b[4;1H{most}\x1b[5;1H{short}\x1b[6;1H{boxed}"
        )
        .as_bytes(),
    );
    let world = observe(&mut term);
    let lines: Vec<Option<bool>> = (0..7).map(|r| world.rule_line(r)).collect();
    assert_eq!(
        lines,
        [
            Some(false),
            Some(true),
            Some(false),
            Some(false),
            Some(false),
            Some(false),
            Some(false)
        ],
        "rows 0..7: blank, a rule, text, a rule a third words, half a rule, a double rule"
    );
    assert_eq!(world.rule_line(10), None, "off the map");
    assert_eq!(
        PetWorld::default().rule_line(1),
        None,
        "before an observation"
    );
}

#[test]
fn nearest_clear_perch_fits_two_blank_rows_and_never_counts_ink() {
    let words = "x".repeat(40);
    let mut term = Terminal::new(12, 40);
    term.process(format!("\x1b[3;1H{words}\x1b[6;1H{words}\x1b[1;1H").as_bytes());
    let world = observe(&mut term);
    let near = PetRect::new(2.3, 10.0, 1.7, 5.0);
    let seat = world
        .nearest_clear_perch(near, 0.1, 20.0, None)
        .expect("the gap between the two lines is clean ground");
    assert!(world.clear(seat, 0.1), "the seat is clear, margin included");
    assert!(
        (3.0..3.3).contains(&seat.row) && (seat.col - 10.0).abs() <= 0.5,
        "the two blank rows 3 and 4, at the stand's column: {seat:?}"
    );
    assert!(
        !world.clear(PetRect::new(3.3, 10.0, 1.7, 5.0), 0.1),
        "negative control: integral feet in the gap touch row 5's words"
    );
    assert_eq!(
        world.nearest_clear_perch(near, 0.1, 1.0, None),
        None,
        "no clean ground within the reach is no answer"
    );
    let mut term = Terminal::new(12, 40);
    term.process(format!("\x1b[3;1H{words}\x1b[6;1H{words}\x1b[9;1H{words}\x1b[1;1H").as_bytes());
    let world = observe(&mut term);
    let between = PetRect::new(4.65, 10.0, 1.7, 5.0);
    let seat = world
        .nearest_clear_perch(between, 0.1, 20.0, None)
        .expect("clean ground on both sides");
    assert!(
        (6.0..6.3).contains(&seat.row),
        "down before up on a tie: {seat:?}"
    );
}

#[test]
fn exclusion_spans_agree_with_the_cell_test() {
    let mut seed = 0x9e37_79b9_u64;
    let mut next = move |n: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (seed >> 33) % n
    };
    let limit = 30;
    for _ in 0..2000 {
        let edge = |n: u64| n as f32 * 0.25 - 2.0;
        let rect = PetRect::new(
            edge(next(48)),
            edge(next(144)),
            0.25 + next(24) as f32 * 0.25,
            0.25 + next(48) as f32 * 0.25,
        );
        for row in 0..10 {
            let span = exclusion_span(rect, row, limit);
            for col in 0..limit {
                let inside = span.is_some_and(|(lo, hi)| lo <= col && col < hi);
                assert_eq!(
                    inside,
                    intersects_cell(rect, row, col),
                    "{rect:?} at ({row}, {col}): span {span:?}"
                );
            }
        }
    }
    assert_eq!(exclusion_span(PetRect::new(0.0, 0.0, 1.0, 1.0), 0, 0), None);
    assert_eq!(
        exclusion_span(PetRect::new(0.0, 0.0, 1.0, 1.0), 1, 30),
        None
    );
    assert_eq!(
        exclusion_span(PetRect::new(0.0, 0.0, 1.0, 1.0), 0, 1),
        Some((0, 1))
    );
    if let Ok(base) = usize::try_from(1_u64 << 60) {
        let step = usize::try_from(1_u64 << 37).expect("same integer domain");
        let limit = base + 8 * step;
        let thin = PetRect::new(0.0, base as f32, 1.0, 1.0);
        assert_eq!(exclusion_span(thin, 0, limit), None);
        let wide = PetRect::new(0.0, base as f32, 1.0, (4 * step) as f32);
        let span = exclusion_span(wide, 0, limit).expect("a wide rect still has interior");
        assert!(span.0 <= base + step && base + step < span.1);
        for col in [
            0,
            base - step,
            base,
            base + step,
            base + 4 * step,
            limit - 1,
        ] {
            assert_eq!(span.0 <= col && col < span.1, intersects_cell(wide, 0, col));
        }
        let mut tiny = Terminal::new(1, 1);
        tiny.process(b"x");
        let (mut input, mut facts) = snapshot(&mut tiny);
        input.cols = limit;
        facts.stamp.surface.cols = limit;
        let mut world = PetWorld::default();
        assert!(world.observe_with_exclusions(&input, &facts, PetPane::full(&input), &[thin]));
        assert_eq!(world.examined_cells(), MAX_WORLD_COLS);
    }
    let mut term = Terminal::new(10, 30);
    term.process(b"\x1b[?25l");
    let (input, facts) = snapshot(&mut term);
    let rects = [
        PetRect::new(0.5, 2.25, 1.0, 3.5),
        PetRect::new(4.0, 10.0, 2.0, 5.0),
        PetRect::new(7.9, 27.5, 3.0, 9.0),
    ];
    let mut world = PetWorld::default();
    assert!(world.observe_with_exclusions(&input, &facts, PetPane::full(&input), &rects));
    for row in 0..10 {
        for col in 0..30 {
            let want = rects.iter().any(|rect| intersects_cell(*rect, row, col));
            assert_eq!(
                world.cell(row, col) == PetCell::Protected,
                want,
                "cell ({row}, {col})"
            );
        }
    }
}

#[test]
fn nearest_clear_perch_finds_what_an_exhaustive_scan_finds() {
    let mut seed = 0x2545_f491_u64;
    let mut next = move |n: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        (seed >> 33) % n
    };
    let (rows, cols) = (14u16, 60u16);
    for _ in 0..300 {
        let mut term = Terminal::new(rows, cols);
        let mut screen = String::new();
        let mut text = vec![String::new(); usize::from(rows)];
        for r in 0..rows {
            if next(3) == 0 {
                continue;
            }
            let mut line = String::new();
            let len = next(u64::from(cols) + 1);
            while (line.len() as u64) < len {
                let word = 1 + next(6);
                line.push_str(&"w".repeat(word as usize));
                line.push_str(&" ".repeat(1 + next(8) as usize));
            }
            line.truncate(len as usize);
            screen.push_str(&format!("\x1b[{};1H{line}", r + 1));
            text[usize::from(r)] = line;
        }
        screen.push_str("\x1b[?25l");
        term.process(screen.as_bytes());
        let (n_chrome, n_overlays) = (next(4), next(3));
        let mut rect = || {
            PetRect::new(
                next(u64::from(rows)) as f32,
                next(u64::from(cols)) as f32,
                1.0 + next(2) as f32,
                1.0 + next(20) as f32,
            )
        };
        let chrome: Vec<PetRect> = (0..n_chrome).map(|_| rect()).collect();
        let overlays: Vec<PetRect> = (0..n_overlays).map(|_| rect()).collect();
        let (input, facts) = snapshot(&mut term);
        let mut world = PetWorld::default();
        assert!(world.observe_with_overlays(
            &input,
            &facts,
            PetPane::full(&input),
            &chrome,
            &overlays
        ));
        let near = PetRect::new(
            next(130) as f32 * 0.1,
            next(560) as f32 * 0.1,
            1.7,
            5.0 + next(8) as f32 * 0.1,
        );
        let from = (next(2) == 0).then(|| {
            PetRect::new(
                next(124) as f32 * 0.1,
                next(550) as f32 * 0.1,
                1.7,
                near.cols,
            )
        });
        let ink = |r: usize, c: usize| text[r].as_bytes().get(c).is_some_and(|&b| b != b' ');
        let lasting = |r: usize, c: usize| chrome.iter().any(|x| intersects_cell(*x, r, c));
        let hull = |r: usize| {
            let line = text[r].trim_end();
            (line.len() - line.trim_start().len(), line.len())
        };
        let cells = |rect: PetRect, margin: f32| {
            let (r0, c0) = (rect.row - margin, rect.col - margin);
            let (r1, c1) = (rect.row + rect.rows + margin, rect.col + rect.cols + margin);
            (r0 >= 0.0 && c0 >= 0.0 && r1 <= f32::from(rows) && c1 <= f32::from(cols)).then(|| {
                (
                    r0.floor() as usize,
                    c0.floor() as usize,
                    r1.ceil() as usize,
                    c1.ceil() as usize,
                )
            })
        };
        let from_cells = from.and_then(|f| cells(f, 0.0));
        let seat_ok = |seat: PetRect| {
            let Some((r0, c0, r1, c1)) = cells(seat, 0.1) else {
                return false;
            };
            (r0..r1).all(|r| (c0..c1).all(|c| !ink(r, c) && !lasting(r, c)))
                && (r0..r1).all(|r| {
                    let (lo, hi) = hull(r);
                    lo >= hi || c1 <= lo || c0 >= hi
                })
                && from_cells.is_none_or(|_| {
                    let from = from.unwrap();
                    let feet = |r: PetRect| r.row + r.rows;
                    if (feet(seat) - feet(from)).abs() >= 0.5 {
                        return true;
                    }
                    let (a0, b0, a1, b1) = cells(from.union(seat), 0.0).unwrap();
                    (a0..a1).all(|r| (b0..b1).all(|c| !lasting(r, c)))
                })
        };
        for reach in [3.0, 9.0, 64.0] {
            let mut want: Option<f32> = None;
            for band in 0..=(rows - 2) {
                for c in 0..=(cols - 6) {
                    let inset_c = 0.1 + (6.0 - near.cols - 0.2) * 0.5;
                    let seat = PetRect::new(
                        f32::from(band) + 0.15,
                        f32::from(c) + inset_c,
                        near.rows,
                        near.cols,
                    );
                    let d = ((seat.row - near.row) * 2.0).hypot(seat.col - near.col);
                    if d <= reach && seat_ok(seat) {
                        want = Some(want.map_or(d, |w| w.min(d)));
                    }
                }
            }
            let got = world.nearest_clear_perch(near, 0.1, reach, from);
            match (got, want) {
                (None, None) => {}
                (Some(seat), Some(d)) => {
                    assert!(seat_ok(seat), "{seat:?} is rest ground");
                    assert!(
                        world.ground_clear(seat, 0.1) && world.hull_clear(seat, 0.1),
                        "and the world reads it so: {seat:?}"
                    );
                    let got_d = ((seat.row - near.row) * 2.0).hypot(seat.col - near.col);
                    assert!(
                        (got_d - d).abs() < 1e-3,
                        "from {near:?} within {reach} ({from:?}): {seat:?} at {got_d}, the scan's best is {d}"
                    );
                }
                (got, want) => {
                    panic!(
                        "from {near:?} within {reach} ({from:?}): got {got:?}, the scan's best is {want:?}"
                    )
                }
            }
        }
    }
}

#[test]
fn nearest_clear_perch_weighs_rows_and_columns_together() {
    let full = "x".repeat(60);
    let mut term = Terminal::new(14, 60);
    let mut screen = String::new();
    for row in [1, 2, 5, 6, 7, 8, 9, 12, 13, 14] {
        screen.push_str(&format!("\x1b[{row};1H{full}"));
    }
    screen.push_str(&format!("\x1b[3;1H{}", "x".repeat(31)));
    screen.push_str(&format!("\x1b[10;1H{}", "x".repeat(34)));
    screen.push_str("\x1b[13;50H");
    term.process(screen.as_bytes());
    let world = observe(&mut term);
    let near = PetRect::new(10.0, 3.0, 1.7, 5.0);
    assert!(!world.clear(near, 0.1), "fixture: the stand is not clean");
    let seat = world
        .nearest_clear_perch(near, 0.1, 64.0, None)
        .expect("blank ground beside both lines");
    assert!(world.clear(seat, 0.1), "the seat is clear: {seat:?}");
    assert!(
        (9.0..9.3).contains(&seat.row) && seat.col >= 34.0,
        "beside the last line, not eight rows up: {seat:?}"
    );
}

#[test]
fn exact_cells_allow_a_real_gutter_and_protect_the_whole_body() {
    let mut term = Terminal::new(10, 40);
    term.process(b"\x1b[?25l\x1b[4;3Hleft\x1b[4;26Hright");
    let world = observe(&mut term);
    assert!(world.clear(PetRect::new(2.3, 11.0, 1.7, 6.0), 0.2));
    assert!(!world.clear(PetRect::new(2.3, 4.0, 1.7, 6.0), 0.2));
    assert!(!world.clear(PetRect::new(3.0, 3.0, 2.0, 5.0), 0.0));
    assert_eq!(world.cell(4, 3), PetCell::Clear);
}

#[test]
fn cursor_protection_does_not_erase_or_invent_actual_ink() {
    let mut term = Terminal::new(10, 40);
    term.process(b"\x1b[3;5HA\x1b[3;5H");
    let before = observe(&mut term);
    assert_eq!(before.cell(2, 4), PetCell::Protected);
    assert_eq!(before.ink_at(2, 4), Some(true));
    assert_eq!(before.ink_at(2, 5), Some(false));
    term.process(b"\x1b[6;30H");
    let after = observe(&mut term);
    assert_eq!(after.cell(2, 4), PetCell::Ink);
    assert_eq!(after.ink_at(2, 4), before.ink_at(2, 4));
    assert_eq!(after.ink_at(2, 5), before.ink_at(2, 5));
    assert_eq!(after.ink_at(100, 100), None);
}

#[test]
fn outside_the_coverage_window_is_unknown_not_obstructed() {
    let mut term = Terminal::new(100, 300);
    term.process(b"\x1b[100;300H");
    let world = observe(&mut term);
    assert_eq!(world.coverage().row, 36);
    assert_eq!(world.coverage().col, 44);
    let outside = PetRect::new(0.0, 0.0, 2.0, 5.0);
    let inside = PetRect::new(50.0, 60.0, 2.0, 5.0);
    assert_eq!(world.clearance(outside, 0.0), None, "unobserved");
    assert_eq!(world.clearance(inside, 0.0), Some(true));
    assert_eq!(world.ink_clearance(outside, 0.0), None);
    assert_eq!(world.ink_clearance(inside, 0.0), Some(true));
    assert_eq!(world.ink_clearance(inside, f32::NAN), Some(false));
    assert!(!world.clear(outside, 0.0));
    assert!(world.clear(inside, 0.0));
    let mut retired = PetWorld::default();
    assert_eq!(retired.clearance(inside, 0.0), None);
    assert_eq!(retired.clearance_past_caret(inside, 0.0), None);
    retired.retire();
    assert_eq!(retired.clearance(inside, 0.0), None);
    assert_eq!(
        world.clearance(
            PetRect {
                row: f32::NAN,
                ..inside
            },
            0.0
        ),
        Some(false)
    );
    assert_eq!(world.clearance(inside, f32::INFINITY), Some(false));
}

#[test]
fn a_protected_cell_the_map_sees_vetoes_a_body_half_outside_it() {
    let mut term = Terminal::new(100, 300);
    term.process(b"\x1b[37;1H\x1b#6\x1b[100;300H");
    let world = observe(&mut term);
    assert_eq!(world.coverage().row, 36);
    let half = PetRect::new(35.0, 60.0, 2.0, 5.0);
    assert_eq!(world.under_text_clearance_past_caret(half, 0.0), None);
    assert_eq!(
        world.seen_under_text_clearance_past_caret(half),
        Some(false),
        "the seen half stands on a protected row"
    );
    let inside = PetRect::new(97.0, 60.0, 2.0, 5.0);
    assert_eq!(
        world.seen_under_text_clearance_past_caret(inside),
        Some(true),
        "fixture: inside the window and clear"
    );
    let mut plain = Terminal::new(100, 300);
    plain.process(b"\x1b[100;300H");
    let plain = observe(&mut plain);
    assert_eq!(
        plain.seen_under_text_clearance_past_caret(half),
        None,
        "the seen half is clear and the rest is unseen"
    );
    assert_eq!(
        plain.seen_under_text_clearance_past_caret(PetRect::new(0.0, 0.0, 2.0, 5.0)),
        None,
        "wholly unseen"
    );
    assert_eq!(
        plain.seen_under_text_clearance_past_caret(PetRect::new(f32::NAN, 0.0, 2.0, 5.0)),
        Some(false),
        "degenerate is a refusal"
    );
    assert_eq!(
        PetWorld::default().seen_under_text_clearance_past_caret(inside),
        None,
        "a map that saw nothing"
    );
}

#[test]
fn the_caret_is_not_an_obstruction_to_its_own_escort() {
    let mut term = Terminal::new(10, 40);
    term.process(b"\x1b[3;11H");
    let world = observe(&mut term);
    let on_caret = PetRect::new(2.0, 10.0, 1.0, 1.0);
    assert_eq!(world.cell(2, 10), PetCell::Protected);
    assert_eq!(world.clearance(on_caret, 0.0), Some(false));
    assert_eq!(world.clearance_past_caret(on_caret, 0.0), Some(true));
    let ring = PetRect::new(1.0, 9.0, 3.0, 3.0);
    for (r, c) in [(1, 10), (3, 10), (2, 9), (2, 11), (1, 9), (3, 11)] {
        assert_eq!(world.cell(r, c), PetCell::Protected, "ring {r},{c}");
    }
    assert_eq!(world.cell(2, 13), PetCell::Clear, "outside the ring");
    assert_eq!(world.clearance(ring, 0.0), Some(false));
    assert_eq!(world.clearance_past_caret(ring, 0.0), Some(true));
    assert_eq!(world.ink_clearance(ring, 0.0), Some(true));
    assert_eq!(
        world.clearance_past_caret(PetRect::new(0.0, 8.0, 5.0, 5.0), 0.0),
        Some(true)
    );
    term.process(b"\x1b[5;21HX\x1b[5;21H");
    let world = observe(&mut term);
    let on_glyph = PetRect::new(4.0, 20.0, 1.0, 1.0);
    assert_eq!(world.clearance(on_glyph, 0.0), Some(false));
    assert_eq!(world.clearance_past_caret(on_glyph, 0.0), Some(false));
    assert_eq!(world.ink_clearance(on_glyph, 0.0), Some(false));
    term.process(b"\x1b[7;5Hword\x1b[7;12H");
    let world = observe(&mut term);
    assert_eq!(
        world.clearance_past_caret(PetRect::new(6.0, 4.0, 1.0, 9.0), 0.0),
        Some(false)
    );
    assert_eq!(
        world.clearance_past_caret(PetRect::new(6.0, 9.0, 1.0, 4.0), 0.0),
        Some(true)
    );
}

#[test]
fn ordinary_ink_and_protected_pixels_have_distinct_placement_planes() {
    let mut term = Terminal::new(12, 40);
    term.process(b"\x1b[?25l\x1b[3;11H\x1b[41m \x1b[0m\x1b[4;11H\x1b[4m \x1b[0m");
    term.text_selection_mut()
        .start_selection(5, 10, SelectionSide::Left, SelectionType::Simple);
    term.text_selection_mut()
        .update_selection(5, 12, SelectionSide::Right);
    let (mut input, facts) = snapshot(&mut term);
    input.images.resize_with(input.rows, Vec::new);
    input.images[7] = vec![(
        10,
        ImageRef {
            image: Arc::new(ImageData {
                bytes: Vec::new(),
                format: ImageFormat::Png,
                cols: 1,
                rows: 1,
                z_index: -1,
                band_lift_px: 0,
                cell_offset_px: (0, 0),
                scaling: ImageScaling::Fit,
                source_rect: None,
            }),
            cell_row: 0,
            cell_col: 0,
            kitty: None,
        },
    )];
    let mut world = PetWorld::default();
    assert!(world.observe(&input, &facts, PetPane::full(&input)));
    for row in [2, 3, 5, 7] {
        assert!(
            !world.clear(PetRect::new(row as f32, 10.0, 1.0, 1.0), 0.0),
            "row {row}"
        );
    }
    assert!(world.clear(PetRect::new(9.0, 10.0, 1.0, 1.0), 0.0));
    assert_eq!(world.selection_target(), Some((5.0, 12.0)));
    for row in [2, 3] {
        assert!(
            world.under_text_clear(PetRect::new(row as f32, 10.0, 1.0, 1.0), 0.0),
            "colored backgrounds and decorated text permit an under-text body"
        );
    }
    for row in [5, 7] {
        assert!(
            !world.under_text_clear(PetRect::new(row as f32, 10.0, 1.0, 1.0), 0.0),
            "selection and images remain protected in both planes"
        );
    }
    for (row, inked) in [(2, true), (3, true), (5, true), (7, true), (9, false)] {
        assert_eq!(
            world.ink_clearance(PetRect::new(row as f32, 10.0, 1.0, 1.0), 0.0),
            Some(!inked),
            "row {row}"
        );
    }
    let blank = PetRect::new(9.0, 10.0, 1.0, 1.0);
    assert!(world.observe_with_exclusions(&input, &facts, PetPane::full(&input), &[blank]));
    assert_eq!(world.ink_clearance(blank, 0.0), Some(false));
}

#[test]
fn typed_ink_reserve_is_ink_not_a_protected_surface() {
    let mut term = Terminal::new(12, 40);
    term.process(b"\x1b[6;11H");
    let (input, facts) = snapshot(&mut term);
    let guess = PetRect::new(5.0, 10.0, 1.0, 2.0);
    let far = PetRect::new(9.0, 20.0, 1.0, 1.0);
    let ring = PetRect::new(5.0, 11.0, 1.0, 1.0);
    let typed = [guess, far];
    let mut world = PetWorld::default();
    assert!(world.observe_with_reserve(
        &input,
        &facts,
        PetPane::full(&input),
        PetReserve {
            protected: &[],
            overlays: &[],
            ink: &typed,
            blanked: &[],
            painted_over: None,
        }
    ));
    assert_eq!(world.cell(9, 20), PetCell::Ink);
    assert_eq!(
        world.ink_at(9, 20),
        Some(false),
        "the glyph plane is the grid's"
    );
    assert_eq!(world.ink_clearance(far, 0.0), Some(false), "drawn under");
    assert_eq!(world.under_text_clearance(far, 0.0), Some(true));
    assert_eq!(world.under_text_clearance_past_caret(far, 0.0), Some(true));
    assert_eq!(world.clearance(far, 0.0), Some(false));
    assert_eq!(world.clearance_past_caret(far, 0.0), Some(false));
    assert_eq!(world.cell(5, 11), PetCell::Protected, "the caret's ring");
    assert_eq!(world.under_text_clearance_past_caret(ring, 0.0), Some(true));
    assert_eq!(
        world.clearance_past_caret(ring, 0.0),
        Some(false),
        "a typed ring cell is not blank"
    );
    assert_eq!(world.ink_clearance(ring, 0.0), Some(false));

    assert!(world.observe_with_reserve(
        &input,
        &facts,
        PetPane::full(&input),
        PetReserve {
            protected: &[],
            overlays: &typed,
            ink: &[],
            blanked: &[],
            painted_over: None,
        }
    ));
    assert_eq!(world.cell(9, 20), PetCell::Protected);
    assert_eq!(world.under_text_clearance_past_caret(far, 0.0), Some(false));
    assert_eq!(
        world.under_text_clearance_past_caret(ring, 0.0),
        Some(false)
    );

    let nan = [PetRect::new(f32::NAN, 0.0, 1.0, 1.0)];
    assert!(!world.observe_with_reserve(
        &input,
        &facts,
        PetPane::full(&input),
        PetReserve {
            protected: &[],
            overlays: &[],
            ink: &nan,
            blanked: &[],
            painted_over: None,
        }
    ));
    assert_eq!(world.stamp(), None);
}

/// GLASS THE HOST BLANKS IS BLANK ([`PetReserve::blanked`]): the program's letters under it
/// read as the blank the person sees, in the occupancy, glyph and hull planes alike, while
/// a protected rect, an overlay or typed ink over the same cells still holds them, and the
/// letters past it stay ink.
#[test]
fn blanked_glass_hides_the_programs_ink_and_yields_to_the_hosts_own() {
    let mut term = Terminal::new(12, 60);
    term.process(b"\x1b[6;1Hthe program's words run on past the host's\x1b[1;1H");
    let (input, facts) = snapshot(&mut term);
    let blank = [PetRect::new(5.0, 10.0, 1.0, 20.0)];
    let seen = |reserve: PetReserve<'_>| {
        let mut world = PetWorld::default();
        assert!(world.observe_with_reserve(&input, &facts, PetPane::full(&input), reserve));
        world
    };
    let world = seen(PetReserve {
        blanked: &blank,
        ..PetReserve::default()
    });
    for col in [10, 14, 29] {
        assert_eq!(world.cell(5, col), PetCell::Clear, "col {col}");
        assert_eq!(world.ink_at(5, col), Some(false), "col {col}");
        assert_eq!(world.letter_at(5, col), Some(false), "col {col}");
    }
    assert!(world.clear(PetRect::new(5.0, 12.0, 1.0, 15.0), 0.0));
    for col in [0, 9, 30, 41] {
        assert_eq!(
            world.cell(5, col),
            PetCell::Ink,
            "the letters beside it, col {col}"
        );
    }
    assert_eq!(world.hull[5], (0, 42), "the hull spans what the row shows");
    let mut hidden = PetWorld::default();
    assert!(hidden.observe_with_reserve(
        &input,
        &facts,
        PetPane::full(&input),
        PetReserve {
            blanked: &[PetRect::new(5.0, 20.0, 1.0, 40.0)],
            ..PetReserve::default()
        },
    ));
    assert_eq!(
        hidden.hull[5],
        (0, 19),
        "a blanked tail ends the hull at the letters it leaves"
    );
    assert_eq!(
        seen(PetReserve::default()).cell(5, 14),
        PetCell::Ink,
        "CONTROL"
    );
    let over = [PetRect::new(5.0, 12.0, 1.0, 4.0)];
    for (what, reserve, class) in [
        (
            "the host's chrome",
            PetReserve {
                protected: &over,
                blanked: &blank,
                ..PetReserve::default()
            },
            PetCell::Protected,
        ),
        (
            "an overlay",
            PetReserve {
                overlays: &over,
                blanked: &blank,
                ..PetReserve::default()
            },
            PetCell::Protected,
        ),
        (
            "typed ink",
            PetReserve {
                ink: &over,
                blanked: &blank,
                ..PetReserve::default()
            },
            PetCell::Ink,
        ),
    ] {
        let world = seen(reserve);
        assert_eq!(world.cell(5, 13), class, "{what} over the blank holds it");
        assert_eq!(
            world.cell(5, 20),
            PetCell::Clear,
            "{what}: the rest stays blank"
        );
    }
    let nan = [PetRect::new(f32::NAN, 0.0, 1.0, 1.0)];
    let mut refused = PetWorld::default();
    assert!(!refused.observe_with_reserve(
        &input,
        &facts,
        PetPane::full(&input),
        PetReserve {
            blanked: &nan,
            ..PetReserve::default()
        },
    ));
}

#[test]
fn reused_observations_match_fresh_maps_after_coverage_changes() {
    let mut reused = PetWorld::default();
    for (rows, cols, caret, present_rows, ink, coherent) in [
        (64, 256, (0, 0), 64, true, true),
        (8, 20, (0, 0), 8, false, true),
        (100, 300, (99, 299), 50, true, true),
        (100, 300, (0, 0), 0, false, true),
        (57, 151, (0, 0), 57, false, true),
        (12, 40, (0, 0), 12, true, false),
        (64, 256, (63, 255), 64, false, true),
    ] {
        let mut term = Terminal::new(rows, cols);
        if ink {
            term.process(&vec![b'x'; usize::from(rows) * usize::from(cols)]);
        }
        term.process(format!("\x1b[{};{}H", caret.0 + 1, caret.1 + 1).as_bytes());
        let (mut input, mut facts) = snapshot(&mut term);
        if !ink {
            for row in &mut input.cells {
                row.clear();
            }
        }
        input.cells.truncate(present_rows);
        if !coherent {
            facts.stamp.content_seq = facts.stamp.content_seq.wrapping_add(1);
        }
        let pane = PetPane::full(&input);
        let mut fresh = PetWorld::default();
        assert_eq!(reused.observe(&input, &facts, pane), coherent);
        assert_eq!(fresh.observe(&input, &facts, pane), coherent);
        assert_eq!(reused.stamp(), fresh.stamp());
        assert_eq!(reused.coverage(), fresh.coverage());
        assert_eq!(reused.examined_cells(), fresh.examined_cells());
        let anchor_state = |world: &PetWorld| {
            world.anchors().map(|anchor| {
                anchor.map(|a| {
                    (
                        a.surface,
                        a.block_id,
                        a.absolute_row,
                        a.row,
                        a.col,
                        a.state,
                        a.exit_code,
                    )
                })
            })
        };
        assert_eq!(anchor_state(&reused), anchor_state(&fresh));
        for row in 0..=pane.rows {
            for col in 0..=pane.cols {
                assert_eq!(
                    (
                        reused.cell(row, col),
                        reused.ink_at(row, col),
                        reused.letter_at(row, col),
                        reused.locomotion_ink_at(row, col),
                    ),
                    (
                        fresh.cell(row, col),
                        fresh.ink_at(row, col),
                        fresh.letter_at(row, col),
                        fresh.locomotion_ink_at(row, col),
                    ),
                    "{rows}x{cols}, present_rows={present_rows}, at ({row},{col})"
                );
                for (height, width) in [(1.0, 1.0), (3.0, 5.0)] {
                    let rect = PetRect::new(row as f32, col as f32, height, width);
                    assert_eq!(
                        (
                            reused.clearance(rect, 0.0),
                            reused.under_text_clearance(rect, 0.0),
                            reused.clearance_past_caret(rect, 0.0),
                            reused.under_text_clearance_past_caret(rect, 0.0),
                            reused.ink_clearance(rect, 0.0),
                        ),
                        (
                            fresh.clearance(rect, 0.0),
                            fresh.under_text_clearance(rect, 0.0),
                            fresh.clearance_past_caret(rect, 0.0),
                            fresh.under_text_clearance_past_caret(rect, 0.0),
                            fresh.ink_clearance(rect, 0.0),
                        ),
                        "{rows}x{cols}, present_rows={present_rows}, rect={rect:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn wide_continuations_combining_and_expanded_lines_are_not_blank() {
    let mut term = Terminal::new(10, 40);
    term.process("\x1b[?25l\x1b[3;11H界\x1b[4;11H \u{301}\x1b[6;1H\x1b#6X".as_bytes());
    let world = observe(&mut term);
    assert_ne!(world.cell(2, 11), PetCell::Clear);
    assert_ne!(world.cell(3, 10), PetCell::Clear);
    assert_ne!(
        world.cell(5, 30),
        PetCell::Clear,
        "expanded row has no certified geometry"
    );
    assert_eq!(world.cell(7, 30), PetCell::Clear);
}

#[test]
fn budget_overflow_and_missing_rows_are_unknown_not_empty() {
    let mut term = Terminal::new(100, 300);
    term.process(b"\x1b[100;300H");
    let (mut input, facts) = snapshot(&mut term);
    let mut world = PetWorld::default();
    assert!(world.observe(&input, &facts, PetPane::full(&input)));
    assert!(world.examined_cells() <= MAX_WORLD_CELLS);
    assert_eq!(world.coverage().row, 36);
    assert_eq!(world.coverage().col, 44);
    assert_eq!(world.cell(0, 0), PetCell::Unknown);
    assert!(!world.clear(PetRect::new(0.0, 0.0, 2.0, 5.0), 0.0));
    assert!(world.clear(PetRect::new(50.0, 60.0, 2.0, 5.0), 0.0));
    assert_eq!(
        world.ink_clearance(PetRect::new(50.0, 60.0, 2.0, 5.0), 0.0),
        Some(true)
    );
    input.cells.truncate(50);
    assert!(world.observe(&input, &facts, PetPane::full(&input)));
    assert_eq!(world.cell(50, 60), PetCell::Unknown);
    assert!(!world.clear(PetRect::new(50.0, 60.0, 2.0, 5.0), 0.0));
    assert_eq!(
        world.ink_clearance(PetRect::new(50.0, 60.0, 2.0, 5.0), 0.0),
        Some(false)
    );
}

/// THE WINDOW ABOUT THE BODY READS ITS ROWS, PAST THE MAP'S COLUMNS ([`PetWorld::body_window`]):
/// on a pane 100 rows by 300 columns, the caret at its top left (the caret's window rows
/// 0..64 and columns 0..256), the console asks for rows 70..90 about a body centred on
/// column 290. The window reads [`MAX_BODY_ROWS`] of them from the first (70..82), over
/// [`MAX_WORLD_COLS`] columns about that column kept on the pane (44..300), and the map counts
/// its cells with the caret's window's: a letter at row 75 in the pane's last column, past
/// the caret's window's columns, is read there, and one at row 75 in column 44, its first;
/// one in column 43, before it, one at row 69, before its first row, and one at row 85, past
/// its last, are read by neither window. Asked for no rows, the window reads none, and
/// neither does row 75. Asked for rows the caret's window reads (rows 10..20 there), or on a
/// pane within the map's budget (60 rows by 80 columns, asked for rows 40..52), it reads
/// none either, and the map examines the caret's window's cells alone. Of rows the caret's
/// window reads in part, the window leaves out those it reads where it reads the window's
/// columns: about column 100 (columns 0..256, the caret's window's), rows 58..70 read rows
/// 64..70; about column 290 (columns 44..300, past it) rows 60..70 read all ten, so a
/// letter at row 62 in the pane's last column is read. On a pane 100 rows by 72 columns,
/// the caret at row 45 (the caret's window rows 12..76), rows 9..18 read rows 9..12, and rows
/// 70..82 read rows 76..82.
/// The window about the body answers the clearance readers on a pane past the map's rows alone
/// ([`PetWorld::body_clearance`]): on panes 80 x 100, 100 x 80 and 160 x 72 (rows by columns),
/// the caret four rows short of the foot and the body's rows at the top, outside the caret's
/// window, a box on letters there is not clear and one on blank cells is, on the live plane
/// and the ground. With the window reading its glyphs alone
/// ([`crate::kitty_pet::OldPath::BodysWindowReadsItsGround`] reverted) it reads neither, and
/// the ground reader counts neither clear; its glyphs read the same either way.
#[test]
fn the_window_about_the_body_reads_its_ground_past_the_maps_rows() {
    for (rows, cols) in [(80, 100), (100, 80), (160, 72)] {
        let mut term = Terminal::new(rows, cols);
        term.process(format!("\x1b[4;10Hxyz\x1b[{};40H", rows - 4).as_bytes());
        let (input, facts) = snapshot(&mut term);
        let pane = PetPane::full(&input);
        for ground in [true, false] {
            let mut world = PetWorld::default();
            world.read_the_bodys_ground(ground);
            assert!(world.observe_with_exclusions_near(
                &input,
                &facts,
                pane,
                PetReserve::protected(&[]),
                (None, Some((0, 10, 12)), None),
            ));
            let why = format!("{rows} x {cols}, its ground read {ground}");
            assert_eq!(
                world.coverage(),
                PetPane {
                    row: usize::from(rows) - MAX_WORLD_ROWS,
                    col: 0,
                    rows: MAX_WORLD_ROWS,
                    cols: usize::from(cols)
                },
                "{why}"
            );
            assert_eq!(
                world.body_window(),
                PetPane {
                    row: 0,
                    col: 0,
                    rows: 10,
                    cols: usize::from(cols)
                },
                "{why}"
            );
            let on = PetRect::new(2.0, 9.0, 2.0, 3.0);
            let clear = PetRect::new(5.0, 20.0, 2.0, 3.0);
            let read = |clear: bool| ground.then_some(clear);
            assert_eq!(world.clearance(on, 0.0), read(false), "{why}");
            assert_eq!(world.clearance(clear, 0.0), read(true), "{why}");
            assert!(!world.ground_clear(on, 0.0), "{why}");
            assert_eq!(world.ground_clear(clear, 0.0), ground, "{why}");
            assert_eq!(world.locomotion_ink_in_span(3, 9, 12), Some(true), "{why}");
        }
    }
}

#[test]
fn the_window_about_the_body_reads_its_rows_past_the_maps_columns() {
    let mut term = Terminal::new(100, 300);
    term.process(
        b"\x1b[70;300Hx\x1b[76;300Hx\x1b[86;300Hx\x1b[76;45Hy\x1b[76;44Hy\x1b[63;300Hz\x1b[1;1H",
    );
    let (input, facts) = snapshot(&mut term);
    let pane = PetPane::full(&input);
    let mut world = PetWorld::default();
    let read = |world: &mut PetWorld, keep| {
        assert!(world.observe_with_exclusions_near(
            &input,
            &facts,
            pane,
            PetReserve::protected(&[]),
            (None, keep, None),
        ));
    };
    read(&mut world, Some((70, 90, 290)));
    assert_eq!(
        world.coverage(),
        PetPane {
            row: 0,
            col: 0,
            rows: MAX_WORLD_ROWS,
            cols: MAX_WORLD_COLS
        }
    );
    assert_eq!(
        world.body_window(),
        PetPane {
            row: 70,
            col: 44,
            rows: MAX_BODY_ROWS,
            cols: MAX_WORLD_COLS
        }
    );
    assert_eq!(
        world.examined_cells(),
        (MAX_WORLD_ROWS + MAX_BODY_ROWS) * MAX_WORLD_COLS
    );
    assert_eq!(world.locomotion_ink_in_span(75, 290, 300), Some(true));
    assert_eq!(world.letter_at(75, 299), Some(true));
    assert_eq!(world.last_letter_in_span(75, 44, 300), Some(Some(299)));
    assert_eq!(world.ink_row_bits(75, 280, 300), Some(1 << 19));
    assert_eq!(world.locomotion_ink_in_span(75, 44, 45), Some(true));
    assert_eq!(world.locomotion_ink_in_span(75, 45, 299), Some(false));
    assert_eq!(world.locomotion_ink_in_span(75, 43, 44), None);
    assert_eq!(world.letter_at(75, 43), None);
    assert_eq!(world.locomotion_ink_in_span(69, 299, 300), None);
    assert_eq!(world.locomotion_ink_in_span(85, 299, 300), None);
    read(&mut world, None);
    assert_eq!(world.body_window().rows, 0);
    assert_eq!(world.locomotion_ink_in_span(75, 290, 300), None);
    assert_eq!(world.examined_cells(), MAX_WORLD_ROWS * MAX_WORLD_COLS);
    read(&mut world, Some((10, 20, 40)));
    assert_eq!(world.body_window().rows, 0);
    assert_eq!(world.examined_cells(), MAX_WORLD_ROWS * MAX_WORLD_COLS);
    read(&mut world, Some((58, 70, 100)));
    assert_eq!(
        world.body_window(),
        PetPane {
            row: MAX_WORLD_ROWS,
            col: 0,
            rows: 70 - MAX_WORLD_ROWS,
            cols: MAX_WORLD_COLS
        }
    );
    assert_eq!(world.examined_cells(), 70 * MAX_WORLD_COLS);
    read(&mut world, Some((60, 70, 290)));
    assert_eq!(
        world.body_window(),
        PetPane {
            row: 60,
            col: 44,
            rows: 10,
            cols: MAX_WORLD_COLS
        }
    );
    assert_eq!(
        world.examined_cells(),
        (MAX_WORLD_ROWS + 10) * MAX_WORLD_COLS
    );
    assert_eq!(world.letter_at(62, 299), Some(true));
    assert_eq!(world.locomotion_ink_in_span(62, 250, 300), Some(true));

    let mut term = Terminal::new(100, 72);
    term.process(b"\x1b[45;58H");
    let (input, facts) = snapshot(&mut term);
    let pane = PetPane::full(&input);
    for (keep, rows) in [((9, 18, 30), 9..12), ((70, 82, 30), 76..82)] {
        assert!(world.observe_with_exclusions_near(
            &input,
            &facts,
            pane,
            PetReserve::protected(&[]),
            (None, Some(keep), None),
        ));
        assert_eq!(
            (world.coverage().row, world.coverage().rows),
            (12, MAX_WORLD_ROWS)
        );
        assert_eq!(
            world.body_window(),
            PetPane {
                row: rows.start,
                col: 0,
                rows: rows.len(),
                cols: 72
            },
            "rows {keep:?}"
        );
        assert_eq!(world.examined_cells(), (MAX_WORLD_ROWS + rows.len()) * 72);
    }

    let mut term = Terminal::new(60, 80);
    term.process(b"\x1b[46;41Hx\x1b[1;1H");
    let (input, facts) = snapshot(&mut term);
    let pane = PetPane::full(&input);
    assert!(world.observe_with_exclusions_near(
        &input,
        &facts,
        pane,
        PetReserve::protected(&[]),
        (None, Some((40, 52, 40)), None),
    ));
    assert_eq!(world.body_window().rows, 0);
    assert_eq!(world.examined_cells(), 60 * 80);
    assert_eq!(world.letter_at(45, 40), Some(true));
}

/// A READ ALONG THE BODY'S ROW PAST EITHER WINDOW IS READ AS FAR AS A WINDOW READS IT
/// ([`PetWorld::span_read_about`], ATERM-KI-1107-3): on a pane 100 rows by 300 columns, the
/// caret at its top left (the caret's window rows 0..64 and columns 0..256) and the window
/// about the body rows 2..9 about column 290 (columns 44..300), a letter at row 4 in column
/// 270. Along row 4, columns 13..297 are read whole by neither window, and read nothing:
/// about columns 13..18, which the caret's window alone reads, they are read as far as its
/// last column (13..256); about columns 280..285, which the window about the body alone
/// reads, from its first (44..297), the letter among them; about columns 100..110, which both
/// read, as far as the one that reads more of them (44..297). Columns 100..200, which the
/// caret's window reads whole, are read whole. A span past the pane's last column is read as
/// on a pane within the budget, not at all, wherever the body stands: columns 13..301 about
/// columns 13..18 (the caret's window's alone), 100..110 (both windows') and 280..285 (the
/// window about the body's alone), and columns 250..310 about columns 260..270. About
/// columns outside the span, or on a row neither window reads (row 70), nothing is read
/// either. A flight's landing the caret's window
/// does not read (rows 70..78 about column 290) places the window about the body there,
/// though the rows asked for about the body (2..9 about column 15) are the caret's window's,
/// and a letter at row 72 in column 290 is read; a landing it reads (rows 10..18 about
/// column 25) leaves the window as those rows ask for it: none.
#[test]
fn a_read_along_the_row_past_either_window_is_read_as_far_as_a_window_reads_it() {
    let mut term = Terminal::new(100, 300);
    term.process(b"\x1b[5;271Hx\x1b[73;291Hy\x1b[1;1H");
    let (input, facts) = snapshot(&mut term);
    let pane = PetPane::full(&input);
    let mut world = PetWorld::default();
    let read = |world: &mut PetWorld, keep, landing| {
        assert!(world.observe_with_exclusions_near(
            &input,
            &facts,
            pane,
            PetReserve::protected(&[]),
            (None, Some(keep), landing),
        ));
        world.body_window()
    };
    let window = |row, rows, col| PetPane {
        row,
        col,
        rows,
        cols: MAX_WORLD_COLS,
    };
    assert_eq!(read(&mut world, (2, 9, 290), None), window(2, 7, 44));
    assert_eq!(world.coverage(), window(0, MAX_WORLD_ROWS, 0));
    assert_eq!(world.locomotion_ink_in_span(4, 13, 297), None);
    assert_eq!(
        world.span_read_about(4, (13, 297), (13, 18)),
        Some((13, 256))
    );
    assert_eq!(
        world.span_read_about(4, (13, 297), (280, 285)),
        Some((44, 297))
    );
    assert_eq!(world.locomotion_ink_in_span(4, 44, 297), Some(true));
    assert_eq!(world.last_letter_in_span(4, 44, 297), Some(Some(270)));
    assert_eq!(
        world.span_read_about(4, (13, 297), (100, 110)),
        Some((44, 297))
    );
    assert_eq!(
        world.span_read_about(4, (100, 200), (120, 130)),
        Some((100, 200))
    );
    for core in [(13, 18), (100, 110), (280, 285)] {
        assert_eq!(world.span_read_about(4, (13, 301), core), None, "{core:?}");
    }
    assert_eq!(world.span_read_about(4, (250, 310), (260, 270)), None);
    assert_eq!(world.span_read_about(4, (13, 100), (200, 210)), None);
    assert_eq!(world.span_read_about(70, (13, 297), (280, 285)), None);
    let landing = PetPane {
        row: 70,
        col: 285,
        rows: 8,
        cols: 10,
    };
    assert_eq!(
        read(&mut world, (2, 9, 15), Some(landing)),
        window(70, 8, 44)
    );
    assert_eq!(world.letter_at(72, 290), Some(true));
    let landing = PetPane {
        row: 10,
        col: 20,
        rows: 8,
        cols: 10,
    };
    assert_eq!(read(&mut world, (2, 9, 15), Some(landing)).rows, 0);
    assert_eq!(world.letter_at(72, 290), None);
}

#[test]
fn stale_frame_and_nonfinite_geometry_fail_closed() {
    let mut term = Terminal::new(10, 40);
    term.process(b"\x1b[?25l");
    let (input, old_facts) = snapshot(&mut term);
    let mut world = PetWorld::default();
    assert!(world.observe(&input, &old_facts, PetPane::full(&input)));
    let clear = PetRect::new(3.0, 10.0, 2.0, 6.0);
    assert!(world.clear(clear, 0.2));
    assert!(!world.clear(
        PetRect {
            row: f32::NAN,
            ..clear
        },
        0.2
    ));
    assert!(!world.clear(clear, f32::INFINITY));
    let mut large_term = Terminal::new(100, 300);
    large_term.process(b"\x1b[?25l");
    let large_world = observe(&mut large_term);
    assert!(large_world.coverage().row > 0 && large_world.coverage().col > 0);
    for world in [&world, &large_world] {
        let coverage = world.coverage();
        let body = PetRect::new(
            coverage.row as f32 + 3.0,
            coverage.col as f32 + 10.0,
            2.0,
            6.0,
        );
        let target = (body.foot_row(), body.col);
        assert_eq!(world.perch_near(target, body, 0.0, 8.0), Some(body));
        assert_eq!(world.nearest_under_text_perch(body, 0.0, 8.0), Some(body));
        let tall = PetRect {
            rows: f32::MAX,
            ..body
        };
        assert!(tall.valid());
        assert_eq!(
            world.perch_near((-f32::MAX, body.col), tall, 0.0, 8.0),
            None
        );
        assert_eq!(
            world.perch_near(
                target,
                PetRect {
                    cols: f32::MAX,
                    ..body
                },
                0.0,
                8.0
            ),
            None
        );
    }
    term.process(b"\x1b[4;11Hnew text");
    let new_facts = PetWorldFacts::read(&term, 7);
    assert!(!world.observe(&input, &new_facts, PetPane::full(&input)));
    assert!(!world.clear(clear, 0.0));
    assert!(world.stamp().is_none());
}

#[test]
fn selected_history_centers_the_budget_without_a_fake_caret() {
    let mut term = Terminal::new(100, 300);
    for _ in 0..110 {
        term.process(b"line\r\n");
    }
    term.scroll_display(10);
    term.text_selection_mut()
        .start_selection(-4, 20, SelectionSide::Left, SelectionType::Simple);
    term.text_selection_mut()
        .update_selection(-2, 25, SelectionSide::Right);
    let (input, facts) = snapshot(&mut term);
    assert_eq!(input.display_offset, 10);
    let mut world = PetWorld::default();
    assert!(world.observe(&input, &facts, PetPane::full(&input)));
    assert_eq!(world.selection_target(), Some((8.0, 25.0)));
    assert_eq!(world.coverage().row, 0);
    assert!(world.selection_rect().is_some());
    assert_eq!(world.cell(8, 24), PetCell::Protected);
    assert_ne!(
        facts.stamp.top_absolute_row,
        term.grid().visible_to_absolute(0)
    );
}

#[test]
fn visible_history_selection_survives_an_offscreen_active_endpoint() {
    let mut term = Terminal::new(100, 80);
    for _ in 0..140 {
        term.process(b"row\r\n");
    }
    term.scroll_display(20);
    for (start, end, wanted_row) in [(-10, 90, 99.0), (90, -10, 10.0)] {
        term.text_selection_mut().start_selection(
            start,
            2,
            SelectionSide::Left,
            SelectionType::Simple,
        );
        term.text_selection_mut()
            .update_selection(end, 7, SelectionSide::Right);
        let world = observe(&mut term);
        let (row, col) = world
            .selection_target()
            .expect("visible selected intersection");
        assert_eq!(row, wanted_row);
        assert_eq!(world.cell(row as usize, col as usize), PetCell::Protected);
        assert_eq!(world.examined_cells(), MAX_WORLD_ROWS * 80);
        assert!(row as usize >= world.coverage().row);
        assert!((row as usize) < world.coverage().row + world.coverage().rows);
    }
    let (mut input, facts) = snapshot(&mut term);
    input.selection_clip = Some(SelectionClip::new(20, 40, 20, 30));
    let mut world = PetWorld::default();
    assert!(world.observe(&input, &facts, PetPane::full(&input)));
    assert_eq!(world.selection_target(), Some((20.0, 20.0)));
    assert!(!world.clear(PetRect::new(20.0, 20.0, 1.0, 1.0), 0.0));
    term.text_selection_mut()
        .start_selection(-80, 2, SelectionSide::Left, SelectionType::Simple);
    term.text_selection_mut()
        .update_selection(-60, 7, SelectionSide::Right);
    let world = observe(&mut term);
    assert_eq!(
        world.selection_target(),
        None,
        "entirely offscreen is no interest"
    );
    assert_eq!(world.selection_rect(), None);
}

#[test]
fn real_blocks_resolve_through_scroll_but_not_a_new_surface() {
    let mut term = Terminal::new(12, 50);
    term.process(
        b"\x1b[?25l\x1b]133;A\x07$ \x1b]133;B\x07build\r\n\x1b]133;C\x07result\r\n\x1b]133;D;0\x07",
    );
    let world = observe(&mut term);
    let anchor = *world
        .anchors()
        .iter()
        .flatten()
        .next()
        .expect("actual OSC block");
    assert_eq!(anchor.exit_code, Some(0));
    term.process(b"\x1b[12;1H\n");
    let world = observe(&mut term);
    let resolved = world.resolve(anchor).expect("same retained absolute row");
    assert_eq!(resolved.row, anchor.row - 1.0);
    assert_eq!(resolved.absolute_row, anchor.absolute_row);
    assert_eq!(
        resolved.surface.absolute_row_revision, anchor.surface.absolute_row_revision,
        "ordinary full-screen scroll is not an absolute-row splice"
    );
    let (input, mut facts) = snapshot(&mut term);
    facts.stamp.surface.history_renumber_epoch += 1;
    let mut changed = PetWorld::default();
    assert!(changed.observe(&input, &facts, PetPane::full(&input)));
    assert!(changed.resolve(anchor).is_none());
    term.process(b"\x1b[2;10r\x1b[10;1H\n");
    assert!(
        observe(&mut term).resolve(anchor).is_none(),
        "a genuine interior-region scroll cannot carry retained anchors"
    );
    term.process(b"\x1b[?1049h");
    let alt = observe(&mut term);
    assert!(alt.anchors().iter().all(Option::is_none));
    assert!(alt.resolve(anchor).is_none());
}

#[test]
fn a_full_clear_moves_the_running_blocks_anchor_onto_what_followed_it() {
    let mut term = Terminal::new(12, 50);
    term.process(
        b"\x1b[?25l\x1b[9;1H\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07compiling\r\n",
    );
    let running = |world: &PetWorld| {
        world
            .anchors()
            .iter()
            .flatten()
            .find(|a| a.state == BlockState::Executing)
            .map(|a| (a.block_id, a.absolute_row, a.col))
    };
    let id = term.current_block().expect("running").id;
    let output_row = term
        .current_block()
        .and_then(|b| b.output_start_row)
        .expect("output started");
    assert_eq!(
        running(&observe(&mut term)),
        Some((id, output_row, 9.0)),
        "CONTROL: the output line it had"
    );
    term.process(b"\x1b[H\x1b[2Jafter-1\r\nafter-2");
    let (block, row, col) = running(&observe(&mut term)).expect("still running");
    assert_eq!((block, row, col), (id, 1, 7.0));
    assert!(row < output_row, "above where its output had started");
    term.process(b"\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    let (_, facts) = snapshot(&mut term);
    for block in facts.blocks.iter().flatten() {
        let output = block.output_row.unwrap_or(block.start_row);
        assert!(
            block.start_row <= output && output <= block.end_row,
            "a forward range: {block:?}"
        );
    }
    let finished = observe(&mut term)
        .anchors()
        .iter()
        .flatten()
        .find(|a| a.block_id == id)
        .map(|a| (a.state, a.exit_code, a.absolute_row, a.col));
    assert_eq!(finished, Some((BlockState::Complete, Some(0), 1, 7.0)));
}

#[test]
fn explicit_progress_is_sampled_without_claiming_completion() {
    let mut term = Terminal::new(10, 40);
    term.process(b"printed 100% error\x1b]9;4;1;100\x07");
    let world = observe(&mut term);
    assert_eq!(world.progress(), Some(TaskbarProgress::Normal(100)));
    assert!(world.anchors().iter().all(Option::is_none));
    assert_eq!(PetWorldFacts::read(&term, 7).completed_seq, 0);
    term.process(b"\x1b]9;4;4;70\x07");
    assert_eq!(
        observe(&mut term).progress(),
        Some(TaskbarProgress::Paused(70))
    );
}

#[test]
fn a_step_off_protected_ground_keeps_the_nearest_column() {
    let mut term = Terminal::new(12, 60);
    let (input, facts) = snapshot(&mut term);
    let mut world = PetWorld::default();
    let chips = [PetRect::new(5.0, 30.0, 1.0, 29.0)];
    assert!(world.observe_with_exclusions(&input, &facts, PetPane::full(&input), &chips));
    let body = PetRect::new(5.15, 27.4, 1.7, 5.2);
    assert!(
        !world.under_text_clear(body, 0.0),
        "fixture: the chips are on the body"
    );
    let perch = world
        .nearest_under_text_perch(body, 0.1, 6.0)
        .expect("a perch in reach");
    assert!(world.under_text_clear(perch, 0.1));
    assert!(
        (perch.row - 6.15).abs() < 1e-4 && perch.col == body.col,
        "one row down in its own column, not {perch:?}"
    );
}

#[test]
fn perch_search_finds_a_right_pocket_and_rejects_dense_screen() {
    let mut term = Terminal::new(12, 50);
    term.process(b"\x1b[?25l\x1b[5;1Houtput block edge");
    let world = observe(&mut term);
    let body = PetRect::new(3.3, 25.0, 1.7, 6.0);
    let perch = world
        .perch_near((4.0, 17.0), body, 0.2, 20.0)
        .expect("right pocket");
    assert!(world.clear(perch, 0.2));
    assert!(perch.col >= 17.0);
    term.process(b"\x1b[H");
    term.process(&vec![b'X'; 12 * 50]);
    let world = observe(&mut term);
    assert!(world.nearest_perch(body, 0.2, 50.0).is_none());
}

fn adversarial_sidecars(input: &mut RenderInput) {
    let cols = input.cols;
    input.default_bg_spans = vec![
        Vec::new(),
        vec![
            DefaultBgSpan::new(0, 20, 0x0011_2233),
            DefaultBgSpan::new(20, cols, 0x0044_5566),
        ],
        vec![
            DefaultBgSpan::new(0, 19, 0x0077_8899),
            DefaultBgSpan::new(20, cols, 0x00AA_BBCC),
        ],
        vec![
            DefaultBgSpan::new(0, 0, 0x00AA_AAAA),
            DefaultBgSpan::new(3, 2, 0x00BB_BBBB),
            DefaultBgSpan::new(10, 20, 0x00CC_CCCC),
            DefaultBgSpan::new(30, cols + 60, 0x00DD_DDDD),
        ],
        vec![
            DefaultBgSpan::new(20, cols, 0x0011_1111),
            DefaultBgSpan::new(0, 10, 0x0022_2222),
        ],
    ];
    input.line_sizes = vec![
        LineSize::SingleWidth,
        LineSize::DoubleWidth,
        LineSize::SingleWidth,
        LineSize::DoubleHeightTop,
        LineSize::DoubleHeightBottom,
    ];
    input.line_size_spans = vec![
        Vec::new(),
        vec![
            LineSizeSpan::new(0, 20, LineSize::DoubleWidth),
            LineSizeSpan::new(20, cols, LineSize::SingleWidth),
        ],
        vec![
            LineSizeSpan::new(5, 10, LineSize::DoubleHeightTop),
            LineSizeSpan::new(20, 25, LineSize::DoubleWidth),
        ],
        vec![LineSizeSpan::new(10, 10, LineSize::DoubleWidth)],
        vec![
            LineSizeSpan::new(0, 10, LineSize::DoubleWidth),
            LineSizeSpan::new(0, 20, LineSize::DoubleHeightBottom),
        ],
        vec![
            LineSizeSpan::new(20, 30, LineSize::DoubleWidth),
            LineSizeSpan::new(0, 10, LineSize::DoubleHeightBottom),
        ],
    ];
    input.clusters = vec![
        Vec::new(),
        vec![(3, "a".into()), (4, "bb".into()), (cols - 1, "tail".into())],
        vec![(5, "x".into()), (5, "y".into())],
        vec![(9, "z".into()), (2, "w".into())],
    ];
    input.combining = vec![
        vec![(0, vec!['\u{301}'].into()), (7, vec!['\u{308}'].into())],
        Vec::new(),
        vec![(11, vec!['\u{327}'].into()), (11, vec!['\u{300}'].into())],
    ];
    let image = |col: usize| {
        (
            col,
            ImageRef {
                image: Arc::new(ImageData {
                    bytes: Vec::new(),
                    format: ImageFormat::Png,
                    cols: 1,
                    rows: 1,
                    z_index: 0,
                    band_lift_px: 0,
                    cell_offset_px: (0, 0),
                    scaling: ImageScaling::Fit,
                    source_rect: None,
                }),
                cell_row: 0,
                cell_col: u16::try_from(col).expect("test column"),
                kitty: None,
            },
        )
    };
    input.images = vec![
        Vec::new(),
        vec![image(0), image(1), image(30)],
        vec![image(30), image(1)],
    ];
}

#[test]
fn the_row_cursors_answer_exactly_what_the_per_cell_lookups_do() {
    let mut term = Terminal::new(12, 40);
    term.process(b"\x1b[?25l");
    let (mut input, _) = snapshot(&mut term);
    adversarial_sidecars(&mut input);
    for row in 0..input.rows + 2 {
        let mut scan = RowScan::new(&input, row);
        for col in 0..input.cols + 2 {
            let at = (row, col);
            assert_eq!(
                scan.default_bg(col),
                input.default_bg_at(row, col),
                "default_bg at {at:?}"
            );
            assert_eq!(
                scan.line_size(col),
                input.line_size_run_at(row, col).0,
                "line size at {at:?}"
            );
            assert_eq!(
                scan.cluster(col),
                input.cluster_at(row, col),
                "cluster at {at:?}"
            );
            assert_eq!(
                scan.combining(col),
                input.combining_at(row, col),
                "combining at {at:?}"
            );
            assert_eq!(
                scan.image(col).map(std::ptr::from_ref),
                input.image_at(row, col).map(std::ptr::from_ref),
                "image at {at:?}"
            );
        }
    }
}

#[test]
fn a_pane_local_default_background_decides_ink_column_by_column() {
    let mut term = Terminal::new(10, 40);
    term.process(b"\x1b[?25l");
    let (mut input, facts) = snapshot(&mut term);
    let pane_bg = 0x0012_3456;
    input.default_bg_spans = vec![Vec::new(); input.rows];
    input.default_bg_spans[5] = vec![
        DefaultBgSpan::new(0, 20, 0x00FF_FFFF),
        DefaultBgSpan::new(20, input.cols, pane_bg),
    ];
    input.cells[5] = vec![
        RenderCell {
            bg: [0x12, 0x34, 0x56],
            ..RenderCell::default()
        };
        input.cols
    ];
    let mut world = PetWorld::default();
    assert!(world.observe(&input, &facts, PetPane::full(&input)));
    assert_eq!(world.cell(5, 0), PetCell::Ink);
    assert_eq!(world.cell(5, 19), PetCell::Ink);
    assert_eq!(world.cell(5, 20), PetCell::Clear);
    assert_eq!(world.cell(5, 39), PetCell::Clear);
}

#[test]
fn review_row_cursors_survive_skipped_and_sparse_query_orders() {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for trial in 0..400u32 {
        let mut term = Terminal::new(8, 24);
        term.process(b"\x1b[?25l");
        let (mut input, _) = snapshot(&mut term);
        let cols = input.cols;
        let mut bg_rows = Vec::new();
        let mut ls_rows = Vec::new();
        let mut cl_rows = Vec::new();
        let mut cb_rows = Vec::new();
        let mut im_rows = Vec::new();
        for _ in 0..input.rows {
            let n = (rnd() % 5) as usize;
            let mut bg = Vec::new();
            let mut cur = 0usize;
            for _ in 0..n {
                let start = cur + (rnd() % 3) as usize;
                let end = start + (rnd() % 8) as usize;
                bg.push(DefaultBgSpan::new(
                    start.min(cols + 4),
                    end.min(cols + 4),
                    (rnd() % 0x00FF_FFFF) as u32,
                ));
                cur = end;
            }
            if rnd().is_multiple_of(4) {
                bg.reverse();
            }
            if rnd().is_multiple_of(6) && bg.len() > 1 {
                bg.swap(0, 1);
            }
            bg_rows.push(bg);

            let n = (rnd() % 4) as usize;
            let mut ls = Vec::new();
            let mut cur = 0usize;
            for _ in 0..n {
                let start = cur + (rnd() % 3) as usize;
                let end = start + (rnd() % 6) as usize;
                ls.push(LineSizeSpan::new(
                    start.min(cols + 4),
                    end.min(cols + 4),
                    match rnd() % 4 {
                        0 => LineSize::SingleWidth,
                        1 => LineSize::DoubleWidth,
                        2 => LineSize::DoubleHeightTop,
                        _ => LineSize::DoubleHeightBottom,
                    },
                ));
                cur = end;
            }
            if rnd().is_multiple_of(5) {
                ls.reverse();
            }
            if rnd().is_multiple_of(7) && !ls.is_empty() {
                let d = ls[0];
                ls.push(d);
            }
            ls_rows.push(ls);

            let cols_of = |rnd: &mut dyn FnMut() -> u64| {
                let n = (rnd() % 6) as usize;
                let mut v: Vec<usize> = Vec::new();
                let mut c = 0usize;
                for _ in 0..n {
                    c += (rnd() % 5) as usize;
                    v.push(c.min(cols + 3));
                }
                if rnd().is_multiple_of(5) {
                    v.reverse();
                }
                if rnd().is_multiple_of(6) && !v.is_empty() {
                    v.push(v[0]);
                }
                v
            };
            let a = cols_of(&mut rnd);
            let b = cols_of(&mut rnd);
            let d = cols_of(&mut rnd);
            cl_rows.push(
                a.into_iter()
                    .map(|c| (c, format!("g{c}").into_boxed_str()))
                    .collect::<Vec<_>>(),
            );
            cb_rows.push(
                b.into_iter()
                    .map(|c| (c, vec!['\u{301}'].into_boxed_slice()))
                    .collect::<Vec<_>>(),
            );
            im_rows.push(
                d.into_iter()
                    .map(|c| {
                        (
                            c,
                            ImageRef {
                                image: Arc::new(ImageData {
                                    bytes: Vec::new(),
                                    format: ImageFormat::Png,
                                    cols: 1,
                                    rows: 1,
                                    z_index: 0,
                                    band_lift_px: 0,
                                    cell_offset_px: (0, 0),
                                    scaling: ImageScaling::Fit,
                                    source_rect: None,
                                }),
                                cell_row: 0,
                                cell_col: u16::try_from(c.min(65535)).unwrap_or(0),
                                kitty: None,
                            },
                        )
                    })
                    .collect::<Vec<_>>(),
            );
        }
        if trial % 3 == 0 {
            ls_rows.truncate(input.rows.saturating_sub(2));
            cl_rows.truncate(input.rows.saturating_sub(3));
        }
        input.default_bg_spans = bg_rows;
        input.line_size_spans = ls_rows;
        input.clusters = cl_rows;
        input.combining = cb_rows;
        input.images = im_rows;

        for row in 0..input.rows + 1 {
            let mut scan = RowScan::new(&input, row);
            let mut col = 0usize;
            while col < cols + 2 {
                if rnd().is_multiple_of(2) {
                    assert_eq!(
                        scan.default_bg(col),
                        input.default_bg_at(row, col),
                        "trial {trial} default_bg ({row},{col})"
                    );
                }
                if rnd().is_multiple_of(2) {
                    assert_eq!(
                        scan.line_size(col),
                        input.line_size_run_at(row, col).0,
                        "trial {trial} line_size ({row},{col})"
                    );
                }
                if rnd().is_multiple_of(2) {
                    assert_eq!(
                        scan.cluster(col),
                        input.cluster_at(row, col),
                        "trial {trial} cluster ({row},{col})"
                    );
                }
                if rnd().is_multiple_of(2) {
                    assert_eq!(
                        scan.combining(col),
                        input.combining_at(row, col),
                        "trial {trial} combining ({row},{col})"
                    );
                }
                if rnd().is_multiple_of(2) {
                    assert_eq!(
                        scan.image(col).map(std::ptr::from_ref),
                        input.image_at(row, col).map(std::ptr::from_ref),
                        "trial {trial} image ({row},{col})"
                    );
                }
                col += 1 + (rnd() % 3) as usize;
            }
        }
    }
}

#[test]
fn review_sparse_tail_implicit_cell_tracks_a_mid_row_background_seam() {
    let mut term = Terminal::new(10, 40);
    term.process(b"\x1b[?25l");
    let (mut input, facts) = snapshot(&mut term);
    input.default_bg_spans = vec![Vec::new(); input.rows];
    input.default_bg_spans[5] = vec![
        DefaultBgSpan::new(0, 20, 0x00FF_FFFF),
        DefaultBgSpan::new(20, input.cols, 0x0012_3456),
    ];
    input.cells[5] = Vec::new();
    let mut world = PetWorld::default();
    assert!(world.observe(&input, &facts, PetPane::full(&input)));
    for col in [0, 19, 20, 39] {
        assert_eq!(
            world.cell(5, col),
            PetCell::Clear,
            "sparse tail at column {col} is its own pane's EMPTY, not ink"
        );
    }
}

#[test]
fn block_anchor_frontier_picks_last_glyph_and_perch_near_rejects_unobserved_world() {
    let mut term = Terminal::new(10, 40);
    term.process(b"\x1b[?25l\x1b[2;4Hfirst\x1b[4;10Hsecond\x1b[4;20Htail\x1b[7;5Houtside");
    let (input, mut facts) = snapshot(&mut term);
    let top = facts.stamp.top_absolute_row;
    facts.blocks[0] = Some(PetBlock {
        id: 1,
        state: BlockState::Complete,
        start_row: top + 1,
        end_row: top + 5,
        output_row: Some(top + 1),
        exit_code: Some(0),
    });
    facts.blocks[1] = Some(PetBlock {
        id: 2,
        state: BlockState::Executing,
        start_row: top + 7,
        end_row: top + 9,
        output_row: None,
        exit_code: None,
    });
    let mut world = PetWorld::default();
    let body = PetRect::new(0.0, 2.0, 1.7, 5.0);
    assert_eq!(world.nearest_perch(body, 0.1, 8.0), None);
    assert!(world.observe(&input, &facts, PetPane::full(&input)));
    let a0 = world.anchors()[0].expect("populated block anchor");
    assert_eq!((a0.absolute_row, a0.row, a0.col), (top + 3, 3.0, 23.0));
    let a1 = world.anchors()[1].expect("blank block anchor");
    assert_eq!((a1.absolute_row, a1.row, a1.col), (top + 7, 7.0, 0.0));
    let perch = world
        .nearest_perch(body, 0.1, 8.0)
        .expect("perch inside coverage");
    assert!(world.clear(perch, 0.1));
}

#[test]
fn default_world_allocates_buffers_lazily_on_demand() {
    let mut world = PetWorld::default();
    assert_eq!(world.cells.capacity(), 0);
    assert_eq!(world.glyphs.capacity(), 0);
    assert_eq!(world.blocked_prefix.capacity(), 0);
    assert_eq!(world.protected_prefix.capacity(), 0);
    assert_eq!(world.hull.capacity(), 0);
    assert_eq!(world.ground_blocked_prefix.capacity(), 0);
    assert_eq!(world.ground_protected_prefix.capacity(), 0);

    let mut term = Terminal::new(10, 40);
    let (input, mut stale_facts) = snapshot(&mut term);
    stale_facts.stamp.content_seq = stale_facts.stamp.content_seq.wrapping_add(1);
    assert!(!world.observe(&input, &stale_facts, PetPane::full(&input)));
    assert_eq!(
        world.cells.capacity(),
        0,
        "incoherent snapshot must not allocate buffers"
    );

    let (input, facts) = snapshot(&mut term);
    assert!(world.observe(&input, &facts, PetPane::full(&input)));
    assert!(world.cells.len() == MAX_WORLD_CELLS);
    assert_eq!(
        world.ground_blocked_prefix.capacity(),
        0,
        "ground plane stays unallocated without selection or transient overlay"
    );
    assert_eq!(world.ground_protected_prefix.capacity(), 0);

    let overlay = [PetRect::new(1.0, 2.0, 2.0, 4.0)];
    assert!(world.observe_with_overlays(&input, &facts, PetPane::full(&input), &[], &overlay));
    assert_eq!(
        world.ground_blocked_prefix.len(),
        (MAX_WORLD_ROWS + 1) * (MAX_WORLD_COLS + 1)
    );
    assert_eq!(
        world.ground_protected_prefix.len(),
        (MAX_WORLD_ROWS + 1) * (MAX_WORLD_COLS + 1)
    );
    let cells_ptr = world.cells.as_ptr();
    let ground_ptr = world.ground_blocked_prefix.as_ptr();
    world.retire();
    assert_eq!(world.cells.as_ptr(), cells_ptr);
    assert_eq!(world.ground_blocked_prefix.as_ptr(), ground_ptr);
}

#[test]
fn every_source_a_reading_reads_moves_its_key() {
    let mut term = Terminal::new(20, 80);
    term.process(b"\x1b[3;1Hhello\x1b[5;3H");
    let (input, facts) = snapshot(&mut term);
    let pane = PetPane::full(&input);
    let rect = [PetRect::new(1.0, 2.0, 1.0, 3.0)];
    let reserve = PetReserve {
        protected: &rect,
        overlays: &rect,
        ink: &rect,
        blanked: &rect,
        painted_over: None,
    };
    const fn consumed(i: &RenderInput) -> (&RenderInput, PetFill) {
        (i, PetFill::Consumed)
    }
    // A reading noted with no window read: the windows a world reads move the reading only
    // through the window about the body, which none of these asks for.
    const UNREAD: (PetPane, PetPane) = (PetPane::EMPTY, PetPane::EMPTY);
    let mut reading = PetReading::new();
    reading.note(consumed(&input), &facts, pane, reserve, None, UNREAD);
    assert!(
        reading.holds(
            consumed(&input),
            &facts,
            pane,
            reserve,
            (Some((3.0, 4.0)), None, None)
        ),
        "the same screen is the same reading (a centre moves no window of a pane in budget)"
    );
    type Edit = fn(&mut RenderInput, &mut PetWorldFacts);
    let edits: [(&str, Edit); 25] = [
        ("a consuming fill", |i, _| i.extract_gen += 1),
        ("new damage", |i, _| {
            i.snapshot_seq += 1;
            i.engine_fill_seq += 1;
        }),
        ("a host write", |i, _| i.snapshot_seq += 1),
        ("new content", |i, _| i.content_seq += 1),
        ("a parser batch", |i, _| i.process_sequence += 1),
        ("a screen mutation", |i, _| i.screen_mutations += 1),
        ("another terminal", |i, _| i.terminal_id += 1),
        ("the other screen", |i, _| i.engine_alt = !i.engine_alt),
        ("the rows", |i, _| i.rows += 1),
        ("the columns", |i, _| i.cols += 1),
        ("the row count", |i, _| {
            i.cells.pop();
        }),
        ("a scroll", |i, _| i.display_offset += 1),
        ("the base", |i, _| i.base_y += 1),
        ("the row revision", |i, _| i.absolute_row_revision += 1),
        ("a renumbering", |i, _| i.history_renumber_epoch += 1),
        ("the caret's row", |i, _| i.cursor_row += 1),
        ("the caret's column", |i, _| i.cursor_col += 1),
        ("the caret's visibility", |i, _| {
            i.cursor_visible = !i.cursor_visible
        }),
        ("the theme's background", |i, _| i.default_bg ^= 1),
        ("the theme's foreground", |i, _| i.default_fg ^= 1),
        ("the blank cell", |i, _| i.implicit_blank.bg[0] ^= 1),
        ("a selection", |i, _| {
            i.selection
                .start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
        }),
        ("a selection clip", |i, _| {
            i.selection_clip = Some(SelectionClip::new(0, 1, 0, 1));
        }),
        ("a pane's selection", |i, _| {
            i.selections.push(aterm_core::render::PaneSelection {
                selection: TextSelection::new(),
                clip: SelectionClip::new(0, 1, 0, 1),
                bg: 0,
                fg: 0,
                inactive: false,
            });
        }),
        ("the facts", |_, f| f.completed_seq += 1),
    ];
    for (what, edit) in edits {
        let (mut changed, mut changed_facts) = (input.clone(), facts);
        edit(&mut changed, &mut changed_facts);
        assert!(
            !reading.holds(
                consumed(&changed),
                &changed_facts,
                pane,
                reserve,
                (None, None, None)
            ),
            "{what} moves the reading"
        );
    }
    let narrower = PetPane {
        cols: pane.cols - 1,
        ..pane
    };
    assert!(
        !reading.holds(
            consumed(&input),
            &facts,
            narrower,
            reserve,
            (None, None, None)
        ),
        "the pane moves the reading"
    );
    let other = [PetRect::new(1.0, 2.0, 1.0, 4.0)];
    for (what, moved) in [
        (
            "protected",
            PetReserve {
                protected: &other,
                ..reserve
            },
        ),
        (
            "overlays",
            PetReserve {
                overlays: &other,
                ..reserve
            },
        ),
        (
            "typed ink",
            PetReserve {
                ink: &other,
                ..reserve
            },
        ),
        (
            "blanked glass",
            PetReserve {
                blanked: &other,
                ..reserve
            },
        ),
    ] {
        assert!(
            !reading.holds(consumed(&input), &facts, pane, moved, (None, None, None)),
            "the host's {what} move the reading"
        );
    }
    assert!(
        !reading.holds(
            (&input, PetFill::Unconsumed),
            &facts,
            pane,
            reserve,
            (None, None, None)
        ),
        "an unconsumed fill is always read"
    );
    let retained = |prior| PetFill::Retained { prior };
    let mut refilled = input.clone();
    refilled.extract_gen += 1;
    assert!(
        reading.holds(
            (&refilled, retained(input.extract_gen)),
            &facts,
            pane,
            reserve,
            (None, None, None)
        ),
        "a fill that re-extracted no row of the cells read keeps the reading"
    );
    assert!(
        !reading.holds(
            (&refilled, retained(input.extract_gen + 1)),
            &facts,
            pane,
            reserve,
            (None, None, None)
        ),
        "a fill that re-extracted no row of other cells moves the reading"
    );
    let mut kept = PetReading::new();
    kept.note(consumed(&input), &facts, pane, reserve, None, UNREAD);
    kept.keep(&refilled);
    let mut next = refilled.clone();
    next.extract_gen += 1;
    assert!(
        kept.holds(
            (&next, retained(refilled.extract_gen)),
            &facts,
            pane,
            reserve,
            (None, None, None)
        ),
        "a kept reading is named by the fill that kept it"
    );
    assert!(
        !kept.holds(
            (&next, retained(input.extract_gen)),
            &facts,
            pane,
            reserve,
            (None, None, None)
        ),
        "a kept reading is no longer named by the fill it was taken from"
    );
    let mut host_wrote_after = refilled.clone();
    host_wrote_after.snapshot_seq += 1;
    assert!(
        !reading.holds(
            (&host_wrote_after, retained(input.extract_gen)),
            &facts,
            pane,
            reserve,
            (None, None, None)
        ),
        "a retained scratch a host wrote into is read"
    );
    {
        use aterm_core::render::{FrameRefill, FullRefillCause};
        assert_eq!(
            PetFill::after_refill(7, FrameRefill::Scoped { rows_refilled: 0 }),
            retained(7),
            "a fill that re-extracted no row names the fill before it"
        );
        assert_eq!(
            PetFill::after_refill(7, FrameRefill::Scoped { rows_refilled: 1 }),
            PetFill::Consumed,
            "a fill that re-extracted a row names only itself"
        );
        assert_eq!(
            PetFill::after_refill(
                7,
                FrameRefill::Full {
                    cause: FullRefillCause::DamageTaken
                }
            ),
            PetFill::Consumed,
            "a full refill names only itself"
        );
    }
    let mut host_wrote = input.clone();
    host_wrote.snapshot_seq += 1;
    reading.note(consumed(&host_wrote), &facts, pane, reserve, None, UNREAD);
    assert!(
        !reading.holds(
            consumed(&host_wrote),
            &facts,
            pane,
            reserve,
            (None, None, None)
        ),
        "a scratch a host wrote into is never kept"
    );

    let mut tall = Terminal::new(70, 80);
    let (input, facts) = snapshot(&mut tall);
    let pane = PetPane::full(&input);
    reading.note(
        consumed(&input),
        &facts,
        pane,
        reserve,
        Some((30.0, 4.0)),
        UNREAD,
    );
    assert!(reading.holds(
        consumed(&input),
        &facts,
        pane,
        reserve,
        (Some((30.0, 4.0)), None, None)
    ));
    assert!(
        !reading.holds(
            consumed(&input),
            &facts,
            pane,
            reserve,
            (Some((31.0, 4.0)), None, None)
        ),
        "past the budget, the held centre moves the window"
    );

    // Past the budget, the window about the body moves the reading where it is read, and
    // only there ([`PetWorld::body_window`]). On a pane 100 rows tall with the caret at its
    // top (the caret's window reads rows 0..64), rows 70..80 asked for are read, so rows
    // 71..81 move the reading; rows 10..20, which the caret's window reads, are not, so rows
    // 11..21 leave it as it is, and rows 60..70, whose rows 64..70 are read, move it.
    let mut tall = Terminal::new(100, 80);
    let (input, facts) = snapshot(&mut tall);
    let pane = PetPane::full(&input);
    let none = PetReserve::default();
    let mut world = PetWorld::default();
    let mut read = |reading: &mut PetReading, keep| {
        assert!(world.observe_with_exclusions_near(&input, &facts, pane, none, (None, keep, None)));
        let windows = (world.coverage(), world.body_window());
        reading.note(consumed(&input), &facts, pane, none, None, windows);
        world.body_window()
    };
    let asks = |rows: (usize, usize)| (None, Some((rows.0, rows.1, 40)), None);
    assert_eq!(
        read(&mut reading, Some((70, 80, 40))),
        PetPane {
            row: 70,
            col: 0,
            rows: 10,
            cols: 80
        }
    );
    assert!(
        reading.holds(consumed(&input), &facts, pane, none, asks((70, 80))),
        "the same window about the body is the same reading"
    );
    assert!(
        !reading.holds(consumed(&input), &facts, pane, none, asks((71, 81))),
        "past the budget, a window about the body that is read moves the reading"
    );
    assert_eq!(read(&mut reading, Some((10, 20, 40))).rows, 0);
    assert!(
        reading.holds(consumed(&input), &facts, pane, none, asks((11, 21))),
        "rows the caret's window reads move no reading"
    );
    assert!(
        !reading.holds(consumed(&input), &facts, pane, none, asks((60, 70))),
        "rows past the caret's window move it"
    );
}
