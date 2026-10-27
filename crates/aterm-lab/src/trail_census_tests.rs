// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trail census tests: synthetic frames on a light page, each measure beside its negative control.

use super::*;

use std::sync::atomic::{AtomicU32, Ordering};

const PAGE: Rgb = [0xf6, 0xf7, 0xf9];
const INK: Rgb = [0x2b, 0x2d, 0x33];
/// A clean glass colour (L .838, C .132): 8.72:1 under the ink.
const GLASS: Rgb = [0x42, 0xe0, 0xfd];
/// A clean deep lip (L .469, C .302).
const LIP: Rgb = [0x36, 0x04, 0xff];
/// Olive: mud under both rules (L .554, C .075, h 107.7).
const OLIVE: Rgb = [0x77, 0x76, 0x40];
/// A clean but deep periwinkle (L .646, C .193): 3.97:1 under the ink.
const DEEP: Rgb = [0x7a, 0x7a, 0xff];
const CARET: Rgb = [0x93, 0x02, 0xff];
const WHITE: Rgb = [0xff, 0xff, 0xff];
const SELECT: Rgb = [0xcf, 0xe0, 0xff];
const SPRITE: Rgb = [0x44, 0x44, 0x44];

/// Cells of 10x20 px from (2, 1): 12 columns, 4 rows (row r spans y 1 + 20r ..).
const GRID: Grid = Grid {
    ox: 2,
    oy: 1,
    cw: 10,
    ch: 20,
    rows: 4,
    cols: 12,
    source: "args",
};
const W: usize = 124;
const H: usize = 84;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// A frame being drawn: its colours, row-major.
#[derive(Clone)]
struct Canvas(Vec<Rgb>);

impl Canvas {
    fn get(&self, x: i64, y: i64) -> Rgb {
        self.0[usize::try_from(y).unwrap() * W + usize::try_from(x).unwrap()]
    }

    fn set(&mut self, x: i64, y: i64, colour: Rgb) {
        self.0[usize::try_from(y).unwrap() * W + usize::try_from(x).unwrap()] = colour;
    }

    /// Every pixel of `b`.
    fn paint(&mut self, b: PxBox, colour: Rgb) {
        for y in b.y0..b.y1 {
            for x in b.x0..b.x1 {
                self.set(x, y, colour);
            }
        }
    }

    /// The pixels of `b` that are page in the twin `clean`: an effect drawn under the text.
    fn under(&mut self, clean: &Canvas, b: PxBox, colour: Rgb) {
        for y in b.y0..b.y1 {
            for x in b.x0..b.x1 {
                if clean.get(x, y) == PAGE {
                    self.set(x, y, colour);
                }
            }
        }
    }

    fn image(&self) -> Image {
        Image::from_pixels(W, H, self.0.clone())
    }
}

fn blank() -> Canvas {
    Canvas(vec![PAGE; W * H])
}

fn bx(x0: i64, y0: i64, x1: i64, y1: i64) -> PxBox {
    PxBox { x0, y0, x1, y1 }
}

fn top(row: i64) -> i64 {
    GRID.oy + row * GRID.ch
}

fn x_of(col: i64) -> i64 {
    GRID.ox + col * GRID.cw
}

/// A letter: a 2 px stem through the x-height (6 to 15 px into the row).
fn stem(img: &mut Canvas, row: i64, col: i64) {
    let (x, y) = (x_of(col), top(row));
    img.paint(bx(x + 4, y + 6, x + 6, y + 15), INK);
}

/// A rule glyph along a whole row, 6 px into it.
fn rule(img: &mut Canvas, row: i64) {
    img.paint(
        bx(x_of(0), top(row) + 6, x_of(GRID.cols), top(row) + 7),
        INK,
    );
}

/// A typed line: six letters on row 1, and a rule on row 2.
fn typed_line() -> Canvas {
    let mut img = blank();
    for col in 0..6 {
        stem(&mut img, 1, col);
    }
    rule(&mut img, 2);
    img
}

/// `colour`'s band on `row` from `from_top` px into it to its bottom edge, over cells `cols`.
fn band(
    img: &mut Canvas,
    clean: &Canvas,
    row: i64,
    cols: std::ops::Range<i64>,
    from_top: i64,
    colour: Rgb,
) {
    let b = bx(
        x_of(cols.start),
        top(row) + from_top,
        x_of(cols.end),
        top(row + 1),
    );
    img.under(clean, b, colour);
}

/// `colour`'s stroke on `row`'s bottom edge, from `from` to `to` px past it, over cells `cols`.
fn stroke(
    img: &mut Canvas,
    clean: &Canvas,
    row: i64,
    cols: std::ops::Range<i64>,
    from: i64,
    to: i64,
    colour: Rgb,
) {
    let rb = top(row + 1);
    img.under(
        clean,
        bx(x_of(cols.start), rb + from, x_of(cols.end), rb + to),
        colour,
    );
}

fn scene<'a>(
    frame: &'a Image,
    clean: &'a Image,
    caret: Option<PxBox>,
    pets: &[PxBox],
) -> Scene<'a> {
    Scene::new(frame, clean, GRID, PAGE, caret, pets)
}

fn num(v: &J, path: &[&str]) -> i64 {
    let mut at = v;
    for k in path {
        at = at
            .get(k)
            .unwrap_or_else(|| panic!("{k} of {path:?} in {}", v.py()));
    }
    at.as_i64()
        .unwrap_or_else(|| panic!("{path:?} is no integer in {}", v.py()))
}

fn float(v: &J, path: &[&str]) -> f64 {
    let mut at = v;
    for k in path {
        at = at
            .get(k)
            .unwrap_or_else(|| panic!("{k} of {path:?} in {}", v.py()));
    }
    at.as_f64()
        .unwrap_or_else(|| panic!("{path:?} is no number in {}", v.py()))
}

#[test]
fn the_mud_rules_are_the_specs_numbers() {
    for mud in [OLIVE, [0x9a; 3], [0x8b, 0x70, 0x40]] {
        assert!(
            mud_spec(lch(mud)) && mud_broad(lch(mud)),
            "{} is mud",
            hex(mud)
        );
    }
    for clean in [GLASS, LIP, CARET, DEEP, [0xd2, 0xfd, 0x49]] {
        assert!(
            !mud_spec(lch(clean)) && !mud_broad(lch(clean)),
            "{} is clean",
            hex(clean)
        );
    }
    // Each rule's edges, either side.
    assert!(mud_spec((0.95, 0.119, 200.0)) && !mud_spec((0.95, 0.121, 200.0)));
    assert!(
        mud_spec((0.81, 0.2, 100.0))
            && !mud_spec((0.83, 0.2, 100.0))
            && !mud_spec((0.81, 0.2, 130.0))
    );
    assert!(
        mud_spec((0.61, 0.2, 60.0)) && !mud_spec((0.63, 0.2, 60.0)) && !mud_spec((0.61, 0.2, 30.0))
    );
    assert!(
        mud_broad((0.74, 0.09, 200.0))
            && !mud_broad((0.74, 0.11, 200.0))
            && !mud_broad((0.76, 0.09, 200.0))
    );
    assert!(
        mud_broad((0.74, 0.2, 70.0))
            && !mud_broad((0.76, 0.2, 70.0))
            && !mud_broad((0.74, 0.2, 130.0))
    );
    let (l, c, _) = lch(WHITE);
    assert!((l - 1.0).abs() < 1e-3 && c < 1e-3, "white is L 1, C 0");
}

#[test]
fn explain_finds_the_coverage_and_what_it_leaves() {
    let half: Rgb = std::array::from_fn(|k| {
        u8::try_from((u16::from(GLASS[k]) + u16::from(PAGE[k])) / 2).unwrap()
    });
    let (a, off) = explain(half, GLASS, PAGE);
    assert!(
        (a - 0.5).abs() < 0.01 && off < 1.0,
        "half of the glass over the page: {a} {off}"
    );
    assert_eq!(explain(GLASS, GLASS, PAGE), (1.0, 0.0));
    assert_eq!(explain(PAGE, GLASS, PAGE), (0.0, 0.0));
    let (_, off) = explain(OLIVE, GLASS, PAGE);
    assert!(off > LIP_MATCH, "olive is no share of the glass: {off}");
}

/// The typed line under a glass band on row 1 (2 px under its top, cells 0 to 7) with `lip` hanging 3 px.
fn glass_and_lip(glass: Rgb, lip: Rgb) -> (Canvas, Canvas) {
    let clean = typed_line();
    let mut frame = clean.clone();
    band(&mut frame, &clean, 1, 0..8, 2, glass);
    stroke(&mut frame, &clean, 1, 0..8, 0, 3, lip);
    (frame, clean)
}

#[test]
fn the_band_and_its_lip_are_sorted_and_censused_apart() {
    let (frame, clean) = glass_and_lip(GLASS, LIP);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    assert_eq!(s.band_rows(), vec![1]);
    assert_eq!(s.layer_at(30, 30), (Layer::Band, 1));
    assert_eq!(s.layer_at(30, 41), (Layer::RailLip, 1));
    assert_eq!(s.layer_at(30, 43), (Layer::RailLip, 1));
    assert_eq!(s.layer_at(30, 44), (Layer::None, -1));
    assert_eq!(
        s.layer_at(x_of(1) + 4, 31),
        (Layer::None, -1),
        "a letter is text, not trail"
    );
    let layers = s.layers(&s.core());
    for layer in ["band", "rail_lip"] {
        for part in ["all", "core"] {
            for rule in ["mud_spec", "mud_broad"] {
                assert_eq!(
                    num(&layers, &[layer, part, rule]),
                    0,
                    "{layer} {part} {rule}"
                );
            }
        }
    }
    assert_eq!(num(&layers, &["rail_lip", "all", "px"]), 80 * 3);
    assert_eq!(
        num(&layers, &["rail_lip", "core", "px"]),
        80,
        "one core pixel a column"
    );
    assert!(num(&layers, &["band", "core", "px"]) > 0);
    assert!(num(&layers, &["band", "core", "px"]) < num(&layers, &["band", "all", "px"]));
    assert_eq!(num(&layers, &["caret", "all", "px"]), 0);

    // Negative controls: mud in one layer is that layer's alone.
    let (frame, clean) = glass_and_lip(OLIVE, LIP);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    let layers = s.layers(&s.core());
    assert!(num(&layers, &["band", "core", "mud_spec"]) > 0);
    assert!(num(&layers, &["band", "core", "mud_broad"]) > 0);
    assert_eq!(num(&layers, &["rail_lip", "core", "mud_spec"]), 0);
    let olive = layers
        .get("band")
        .and_then(|b| b.get("all"))
        .and_then(|a| a.get("mud_colours"))
        .unwrap();
    assert_eq!(
        olive.as_arr().unwrap()[0].get("colour"),
        Some(&J::s(hex(OLIVE)))
    );
    let (frame, clean) = glass_and_lip(GLASS, OLIVE);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    let layers = s.layers(&s.core());
    assert!(num(&layers, &["rail_lip", "core", "mud_spec"]) > 0);
    assert_eq!(num(&layers, &["band", "core", "mud_spec"]), 0);
}

#[test]
fn a_stroke_on_the_rows_last_pixel_row_is_counted() {
    let (frame, clean) = glass_and_lip(GLASS, LIP);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    let st = s.stroke();
    assert_eq!(num(&st, &["rb_minus_1_px"]), 0);
    assert_eq!(num(&st, &["rise_max"]), 0);

    // Negative control: the same lip one pixel row higher, on rb-1.
    let clean = typed_line();
    let mut frame = clean.clone();
    band(&mut frame, &clean, 1, 0..8, 2, GLASS);
    stroke(&mut frame, &clean, 1, 0..8, -1, 3, LIP);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    let st = s.stroke();
    assert_eq!(num(&st, &["rb_minus_1_px"]), 80);
    assert_eq!(num(&st, &["rise_max"]), 1);

    // Main's shape: a hairline of body, the rail, then the body melting under it; the melt is band.
    let mut frame = clean.clone();
    band(&mut frame, &clean, 1, 0..8, 2, GLASS);
    stroke(&mut frame, &clean, 1, 0..8, 0, 6, GLASS);
    stroke(&mut frame, &clean, 1, 0..8, 1, 4, LIP);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    assert_eq!(s.layer_at(30, 41), (Layer::Band, 1));
    assert_eq!(s.layer_at(30, 43), (Layer::RailLip, 1));
    assert_eq!(s.layer_at(30, 45), (Layer::Band, 1));
    let st = s.stroke();
    assert_eq!(
        num(
            &st.get("rows").and_then(J::as_arr).unwrap()[0],
            &["hang_depth"]
        ),
        6
    );
}

#[test]
fn what_hangs_into_the_next_row_keeps_clear_of_its_ink() {
    let (frame, clean) = glass_and_lip(GLASS, LIP);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    let st = s.stroke();
    let row = &st.get("rows").and_then(J::as_arr).unwrap()[0];
    assert_eq!(num(row, &["row"]), 1);
    assert_eq!(num(row, &["hang_px"]), 80 * 3);
    assert_eq!(num(row, &["hang_depth"]), 3);
    assert_eq!(num(row, &["hang_cols"]), 80);
    assert_eq!(num(row, &["over_next_ink_px"]), 0);
    // The rule sits 6 px into row 2; the lip ends 3 px into it.
    assert_eq!(num(row, &["next_ink", "clear_min"]), 3);
    assert_eq!(num(row, &["next_ink", "cols"]), 80);

    // Negative control: a rail hanging 8 px, drawn across the rule.
    let clean = typed_line();
    let mut frame = clean.clone();
    band(&mut frame, &clean, 1, 0..8, 2, GLASS);
    frame.paint(bx(x_of(0), 41, x_of(8), 49), LIP);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    let st = s.stroke();
    let row = &st.get("rows").and_then(J::as_arr).unwrap()[0];
    assert_eq!(num(row, &["hang_depth"]), 8);
    assert_eq!(num(row, &["over_next_ink_px"]), 80);
    assert_eq!(num(row, &["next_ink", "clear_min"]), -2);
}

#[test]
fn glyphs_on_the_band_read_against_the_band() {
    let (frame, clean) = glass_and_lip(GLASS, LIP);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    let (g, min) = s.glyphs(None);
    let on_glass = round_to(contrast(INK, GLASS), 2);
    // Six letters under the band, and eight cells of the rule the lip reaches.
    assert_eq!(num(&g, &["cells"]), 14);
    assert_eq!(float(&g, &["min"]), on_glass);
    assert_eq!(float(&g, &["p1"]), on_glass);
    assert_eq!(float(&g, &["median"]), on_glass);
    assert!(min.is_some_and(|m| m > 8.0));
    assert_eq!(num(&g, &["worst", "row"]), 1);

    // Negative control: a band too deep for the ink.
    let (frame, clean) = glass_and_lip(DEEP, LIP);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    let (g, min) = s.glyphs(None);
    assert_eq!(float(&g, &["min"]), round_to(contrast(INK, DEEP), 2));
    assert!(min.is_some_and(|m| m < 4.5));
}

/// A band on row 1 over cells 0 to 9 whose top is `top_at(column offset)` px into the row, the caret at (1, 11).
fn tail_frame(top_at: impl Fn(i64) -> i64) -> (Canvas, Canvas, PxBox) {
    let mut clean = blank();
    for col in 8..10 {
        stem(&mut clean, 1, col);
    }
    let mut frame = clean.clone();
    for i in 0..x_of(10) - x_of(0) {
        let x = x_of(0) + i;
        frame.under(&clean, bx(x, top(1) + top_at(i), x + 1, top(2)), GLASS);
    }
    (frame, clean, GRID.cell_box(1, 11, 1))
}

#[test]
fn the_tail_reads_its_steps_risers_and_whiskers() {
    let (frame, clean, caret) = tail_frame(|_| 4);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, Some(caret), &[]);
    let (rows, tail) = s.tops(Some(caret));
    let row = &rows.as_arr().unwrap()[0];
    assert_eq!((num(row, &["max_step"]), num(row, &["risers"])), (0, 0));
    assert_eq!(tail.get("end"), Some(&J::s("left")));
    assert_eq!(num(&tail, &["cols"]), TAIL_CELLS * GRID.cw);
    assert_eq!(
        (
            num(&tail, &["max_step"]),
            num(&tail, &["risers"]),
            num(&tail, &["whiskers"])
        ),
        (0, 0, 0)
    );

    // Negative control: v4's staircase, a 3 px riser every 7 columns from a 2 px tip.
    let (frame, clean, caret) = tail_frame(|i| (18 - 3 * (i / 7)).max(4));
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, Some(caret), &[]);
    let (_, tail) = s.tops(Some(caret));
    assert_eq!(num(&tail, &["max_step"]), 3);
    assert!(num(&tail, &["risers"]) >= 4, "{}", tail.py());
    assert_eq!(num(&tail, &["whiskers"]), 7);

    // The oldest end is the one away from the caret.
    let (frame, clean, _) = tail_frame(|_| 4);
    let left = GRID.cell_box(1, 0, 1);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, Some(left), &[]);
    assert_eq!(s.tops(Some(left)).1.get("end"), Some(&J::s("right")));
}

/// A wrapped cohort: row 1's glass sunk to 7 px over cells 0 to 9, row 2's from `hand_top` px into it over 0 to 5.
fn cohort(hand_top: i64) -> (Canvas, Canvas) {
    let clean = blank();
    let mut frame = clean.clone();
    band(&mut frame, &clean, 1, 0..10, 13, GLASS);
    band(&mut frame, &clean, 2, 0..6, hand_top, GLASS);
    (frame, clean)
}

#[test]
fn a_wrapped_cohorts_seam_reads_its_clear_rows() {
    let (frame, clean) = cohort(4);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    assert_eq!(s.band_rows(), vec![1, 2]);
    let (joins, cohorts) = s.joins();
    let j = &joins.as_arr().unwrap()[0];
    // From row 1's middle (y 31) to row 2's (y 51): y 31-33 over row 1's sunk glass and y 41-44 are clear.
    assert_eq!(num(j, &["seam_px"]), 4);
    assert_eq!(num(j, &["join_clear_rows"]), 7);
    assert_eq!(num(j, &["seam_rows"]), 7);
    assert_eq!(num(j, &["seam_cols"]), 60);
    let c = &cohorts.as_arr().unwrap()[0];
    assert_eq!(
        (num(c, &["coloured_rows"]), num(c, &["px_rows"])),
        (7 + 16, 40)
    );
    // The tail is read on the caret's row; with no caret, on the first band row.
    let hand = GRID.cell_box(2, 6, 1);
    assert_eq!(num(&s.tops(Some(hand)).1, &["row"]), 2);
    assert_eq!(num(&s.tops(None).1, &["row"]), 1);

    // Negative control: the hand row's glass fused to the row above's.
    let (frame, clean) = cohort(0);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    assert_eq!(
        s.layer_at(20, 41),
        (Layer::Band, 2),
        "the hand row's glass, not a stroke"
    );
    let (joins, _) = s.joins();
    let j = &joins.as_arr().unwrap()[0];
    assert_eq!(num(j, &["seam_px"]), 0);
    assert_eq!(num(j, &["join_clear_rows"]), 3);
}

/// A five-pixel cross centred at `x`, `y`.
fn cross(img: &mut Canvas, x: i64, y: i64, colour: Rgb) {
    for (dx, dy) in [(0, -1), (-1, 0), (0, 0), (1, 0), (0, 1)] {
        img.set(x + dx, y + dy, colour);
    }
}

#[test]
fn a_mark_close_over_ink_is_in_accent_position_and_a_counter_is_no_mark() {
    let mut clean = blank();
    stem(&mut clean, 1, 2);
    stem(&mut clean, 1, 5);
    // An 'o' on cell 8: a 1 px ring round x 4..8, y 7..14 of the cell.
    let (ox, oy) = (x_of(8), top(1));
    clean.paint(bx(ox + 2, oy + 6, ox + 8, oy + 15), INK);
    clean.paint(bx(ox + 3, oy + 7, ox + 7, oy + 14), PAGE);
    let mut frame = clean.clone();
    // Over the stem on cell 2, 2 px above its ink (y 27): accent position.
    cross(&mut frame, x_of(2) + 4, 23, LIP);
    // Over the stem on cell 5, 10 px above it: clear.
    cross(&mut frame, x_of(5) + 4, 15, LIP);
    // The band seen through the 'o'.
    frame.paint(bx(ox + 3, oy + 7, ox + 7, oy + 14), GLASS);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    let stars = s.stars();
    assert_eq!(num(&stars, &["count"]), 2, "{}", stars.py());
    assert_eq!(num(&stars, &["accent"]), 1);
    let marks = stars.get("marks").and_then(J::as_arr).unwrap();
    let close = marks
        .iter()
        .find(|m| m.get("accent") == Some(&J::Bool(true)))
        .unwrap();
    assert_eq!(num(close, &["gap"]), 2);
    assert_eq!(float(close, &["over_ink"]), 0.67);
    let clear = marks
        .iter()
        .find(|m| m.get("accent") == Some(&J::Bool(false)))
        .unwrap();
    assert_eq!(num(clear, &["gap"]), 10);
    assert_eq!(
        s.layer_at(ox + 4, oy + 10),
        (Layer::Other, -1),
        "the counter is no mark"
    );
}

/// The caret on cell (1, 9) over a letter: a 2 px ink keyline round `CARET`, the letter cut out in `cut`.
fn caret_frame(cut: Rgb) -> (Canvas, Canvas, PxBox) {
    let mut clean = blank();
    stem(&mut clean, 1, 9);
    let mut frame = clean.clone();
    let b = GRID.cell_box(1, 9, 1);
    frame.paint(b, INK);
    frame.paint(bx(b.x0 + 2, b.y0 + 2, b.x1 - 2, b.y1 - 2), CARET);
    for y in b.y0..b.y1 {
        for x in b.x0..b.x1 {
            if clean.get(x, y) == INK {
                frame.set(x, y, cut);
            }
        }
    }
    (frame, clean, b)
}

#[test]
fn the_caret_reads_its_fill_keyline_and_cut_out() {
    let (frame, clean, b) = caret_frame(WHITE);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, Some(b), &[]);
    let c = s.caret((1, 9), b);
    assert_eq!(c.get("fill"), Some(&J::s(hex(CARET))));
    assert_eq!(c.get("keyline"), Some(&J::s(hex(INK))));
    assert_eq!(c.get("cut_out"), Some(&J::s(hex(WHITE))));
    assert_eq!(
        float(&c, &["cut_out_contrast"]),
        round_to(contrast(WHITE, CARET), 2)
    );
    assert_eq!(
        float(&c, &["cut_out_local", "min"]),
        round_to(contrast(WHITE, CARET), 2)
    );
    let layers = s.layers(&s.core());
    // The keyline is fg grey, mud by colour; the caret's core is its fill alone.
    assert!(num(&layers, &["caret", "all", "mud_spec"]) > 0);
    assert_eq!(num(&layers, &["caret", "core", "mud_spec"]), 0);
    assert!(num(&layers, &["caret", "core", "px"]) > 0);

    // Negative control: a cut-out the fill swallows.
    let (frame, clean, b) = caret_frame([0x8a, 0x00, 0xf0]);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, Some(b), &[]);
    assert!(float(&s.caret((1, 9), b), &["cut_out_contrast"]) < 1.5);

    let clean = blank().image();
    let s = scene(&clean, &clean, Some(b), &[]);
    assert_eq!(s.caret((1, 9), b).get("drawn"), Some(&J::Bool(false)));
}

#[test]
fn selection_under_the_band_is_counted() {
    let mut clean = blank();
    clean.paint(bx(x_of(3), top(1), x_of(6), top(2)), SELECT);
    // The band over the selection.
    let mut frame = clean.clone();
    frame.paint(bx(x_of(0), top(1) + 2, x_of(8), top(2)), GLASS);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    let sel = s.selection(None);
    assert_eq!((num(&sel, &["cells"]), num(&sel, &["px"])), (3, 600));
    assert_eq!(num(&sel, &["under_band_px"]), 3 * 10 * 18);
    assert_eq!(num(&sel, &["under_band_cells"]), 3);
    assert_eq!(
        num(&s.selection(Some(GLASS)), &["cells"]),
        0,
        "--selection names one colour"
    );

    // Negative control: the band yields to the selection.
    let mut frame = clean.clone();
    frame.under(&clean, bx(x_of(0), top(1) + 2, x_of(8), top(2)), GLASS);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[]);
    assert_eq!(s.band_rows(), vec![1]);
    assert_eq!(num(&s.selection(None), &["under_band_px"]), 0);
}

/// Glass and a lip on row 1 over cells 0 to 9, a pet's box over x 42..62, y 30..50 drawn as a 2 px ring (the rest
/// of the box transparent), and the lip through the box unless `clip`.
fn pet_frame(clip: bool) -> (Canvas, Canvas, PxBox) {
    let clean = blank();
    let mut frame = clean.clone();
    let pet = bx(42, 30, 62, 50);
    band(&mut frame, &clean, 1, 0..10, 2, GLASS);
    stroke(&mut frame, &clean, 1, 0..10, 0, 3, LIP);
    if clip {
        frame.paint(bx(pet.x0, 41, pet.x1, 44), PAGE);
    }
    frame.paint(bx(pet.x0, pet.y0, pet.x1, pet.y0 + 2), SPRITE);
    frame.paint(bx(pet.x0, pet.y1 - 2, pet.x1, pet.y1), SPRITE);
    frame.paint(bx(pet.x0, pet.y0, pet.x0 + 2, pet.y1), SPRITE);
    frame.paint(bx(pet.x1 - 2, pet.y0, pet.x1, pet.y1), SPRITE);
    (frame, clean, pet)
}

#[test]
fn a_lip_under_the_pet_shows_through_its_box() {
    let (frame, clean, pet) = pet_frame(false);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[pet]);
    let pets = s.pets(&[pet]);
    let p = &pets.as_arr().unwrap()[0];
    assert_eq!(num(p, &["stroke_rows"]), 3);
    assert_eq!(num(p, &["lip_px"]), 16 * 3);
    assert_eq!(num(p, &["lip_cols"]), 16);

    // Negative control: the lip clipped through the pet's span.
    let (frame, clean, pet) = pet_frame(true);
    let (f, c) = (frame.image(), clean.image());
    let s = scene(&f, &c, None, &[pet]);
    let pets = s.pets(&[pet]);
    let p = &pets.as_arr().unwrap()[0];
    assert_eq!(num(p, &["stroke_rows"]), 3);
    assert_eq!(num(p, &["lip_px"]), 0);
}

fn encode_rgb(img: &Canvas) -> Vec<u8> {
    let data: Vec<u8> = img.0.iter().flatten().copied().collect();
    let mut out = Vec::new();
    let mut e = aterm_png::Encoder::new(
        &mut out,
        u32::try_from(W).unwrap(),
        u32::try_from(H).unwrap(),
    );
    e.set_color(aterm_png::ColorType::Rgb);
    e.set_depth(aterm_png::BitDepth::Eight);
    let mut w = e.write_header().expect("a PNG header");
    w.write_image_data(&data).expect("the PNG's pixels");
    w.finish().expect("the PNG's end");
    out
}

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("lab-trail-census-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// `frame` and `clean` written as a gallery frame with its dims and cursor sidecars.
fn gallery_files(dir: &Path, frame: &Canvas, clean: &Canvas) -> PathBuf {
    let png = dir.join("x-light.t0.png");
    std::fs::write(&png, encode_rgb(frame)).unwrap();
    std::fs::write(dir.join("x-light.t0.clean.png"), encode_rgb(clean)).unwrap();
    std::fs::write(
        dir.join("x-light.t0.cursor.txt"),
        "OK 1 11 1 blinking_block\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("x-light.dims.txt"),
        "OK 4 12 124 84 cell_w=10 cell_h=20 pad=2 pad_top=1 head=0 tab_rows=0 offset_x=0 offset_y=0 \
         band_left=0 band_top=0 crop_left=0 crop_top=0 layer_gravity=none\n",
    )
    .unwrap();
    png
}

#[test]
fn the_report_reads_the_sidecars_writes_the_pixels_and_judges_its_thresholds() {
    let dir = scratch("report");
    let (frame, clean) = glass_and_lip(GLASS, LIP);
    let png = gallery_files(&dir, &frame, &clean);
    let png_s = png.to_string_lossy().into_owned();
    let tsv = dir.join("pixels.tsv");
    let a = parse_args(&args(&[
        &png_s,
        "--pixels",
        &tsv.to_string_lossy(),
        "--mud-max",
        "0",
        "--glyph-min",
        "4.5",
    ]))
    .unwrap();
    let r = report(&a).unwrap();
    assert_eq!(r.get("band_rows"), Some(&J::Arr(vec![J::Int(1)])));
    assert_eq!(
        r.get("grid").and_then(|g| g.get("source")),
        Some(&J::s("dims"))
    );
    assert_eq!(num(&r, &["caret", "row"]), 1);
    assert_eq!(
        r.get("caret").and_then(|c| c.get("drawn")),
        Some(&J::Bool(false))
    );
    assert_eq!(r.get("failures"), Some(&J::Arr(Vec::new())));
    let text = std::fs::read_to_string(&tsv).unwrap();
    let mut lines = text.lines();
    assert!(
        lines
            .next()
            .unwrap()
            .starts_with("x\ty\tlayer\trow\tcolour\tL\tC\th")
    );
    let sorted = frame.0.iter().zip(&clean.0).filter(|(f, c)| f != c).count();
    assert_eq!(lines.count(), sorted, "every changed pixel is a row");
    let rendered = render(&r, 0);
    assert_eq!(
        J::parse(&rendered).unwrap(),
        J::parse(&compact(&r)).unwrap()
    );

    // Negative controls: mud in the band, and a band too deep for the ink.
    for (glass, failure) in [(OLIVE, "band core: "), (DEEP, "glyph minimum ")] {
        let (frame, clean) = glass_and_lip(glass, LIP);
        let png = gallery_files(&dir, &frame, &clean);
        let r = report(
            &parse_args(&args(&[
                &png.to_string_lossy(),
                "--mud-max",
                "0",
                "--glyph-min",
                "4.5",
            ]))
            .unwrap(),
        )
        .unwrap();
        let failures = r.get("failures").and_then(J::as_arr).unwrap();
        assert!(
            failures
                .iter()
                .any(|f| f.as_str().is_some_and(|f| f.starts_with(failure))),
            "{failure}: {}",
            r.py()
        );
    }

    std::fs::remove_file(dir.join("x-light.t0.clean.png")).unwrap();
    assert!(
        report(&parse_args(&args(&[&png_s])).unwrap())
            .unwrap_err()
            .starts_with("no clean twin")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn arguments_parse_and_a_bad_one_is_a_usage_error() {
    let a = parse_args(&args(&[
        "--pet",
        "1,2,3,4",
        "f.png",
        "--pet",
        "5,6,7,8",
        "--caret-pad",
        "2",
        "--mud-max",
        "0",
    ]))
    .unwrap();
    assert_eq!(a.png, "f.png");
    assert_eq!(a.pets, vec!["1,2,3,4", "5,6,7,8"]);
    assert_eq!((a.caret_pad, a.mud_max), (2, Some(0)));
    assert_eq!(parse_box("1,2,3,4"), Ok(bx(1, 2, 3, 4)));
    assert!(parse_box("3,2,1,4").is_err());
    assert!(parse_box("1,2,3").is_err());
    for bad in [
        &[][..],
        &["a.png", "b.png"],
        &["f.png", "--bogus"],
        &["f.png", "--mud-max", "some"],
        &["f.png", "--caret-pad", "-1"],
        &["f.png", "--glyph-min"],
    ] {
        let e = parse_args(&args(bad)).expect_err(&format!("{bad:?} is refused"));
        assert_eq!(e.code, 2, "{bad:?}: {:?}", e.msg);
    }
}
