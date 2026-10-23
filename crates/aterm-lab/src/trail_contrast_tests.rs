// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trail contrast probe tests: PNG round trip, WCAG fixed points, and mask measurements.

use super::*;

use std::sync::atomic::{AtomicU32, Ordering};

const PAGE: Rgb = [0x0d, 0x11, 0x17];
const INK: Rgb = [0xe6, 0xed, 0xf3];
const EDGE: Rgb = [0x7a, 0x80, 0x87];
const CARET: Rgb = [0xff, 0xcc, 0x00];
const RIBBON: Rgb = [0xc8, 0xd0, 0xd8];

const GRID: Grid = Grid {
    ox: 2,
    oy: 1,
    cw: 6,
    ch: 10,
    rows: 3,
    cols: 8,
    source: "args",
};
const W: usize = 52;
const H: usize = 32;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

fn encode(width: u32, height: u32, color: aterm_png::ColorType, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut e = aterm_png::Encoder::new(&mut out, width, height);
    e.set_color(color);
    e.set_depth(aterm_png::BitDepth::Eight);
    let mut w = e.write_header().expect("a PNG header");
    w.write_image_data(data).expect("the PNG's pixels");
    w.finish().expect("the PNG's end");
    out
}

fn encode_rgb(img: &Image) -> Vec<u8> {
    let data: Vec<u8> = img.px.iter().flatten().copied().collect();
    encode(
        u32::try_from(img.width).unwrap(),
        u32::try_from(img.height).unwrap(),
        aterm_png::ColorType::Rgb,
        &data,
    )
}

fn paint(img: &mut Image, b: PxBox, colour: Rgb) {
    for y in b.y0..b.y1 {
        for x in b.x0..b.x1 {
            let (x, y) = (usize::try_from(x).unwrap(), usize::try_from(y).unwrap());
            img.px[y * img.width + x] = colour;
        }
    }
}

fn glyph(img: &mut Image, row: i64, col: i64) {
    let b = GRID.cell_box(row, col, 1);
    paint(
        img,
        PxBox {
            x0: b.x0 + 2,
            y0: b.y0 + 2,
            x1: b.x0 + 4,
            y1: b.y0 + 8,
        },
        INK,
    );
    paint(
        img,
        PxBox {
            x0: b.x0 + 4,
            y0: b.y0 + 2,
            x1: b.x0 + 5,
            y1: b.y0 + 8,
        },
        EDGE,
    );
}

fn clean_frame() -> Image {
    let mut img = Image::from_pixels(W, H, vec![PAGE; W * H]);
    for col in 0..3 {
        glyph(&mut img, 1, col);
    }
    img
}

fn trail_frame() -> Image {
    let mut img = clean_frame();
    paint(&mut img, GRID.cell_box(1, 3, 1), CARET);
    let c = GRID.cell_box(1, 2, 1);
    paint(
        &mut img,
        PxBox {
            x0: c.x0,
            y0: c.y0 + 8,
            x1: c.x1,
            y1: c.y1,
        },
        RIBBON,
    );
    img
}

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "lab-trail-contrast-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn gallery_files(dir: &Path, frame: &Image) -> PathBuf {
    let png = dir.join("x-dark.t0.png");
    std::fs::write(&png, encode_rgb(frame)).unwrap();
    std::fs::write(dir.join("x-dark.t0.clean.png"), encode_rgb(&clean_frame())).unwrap();
    std::fs::write(dir.join("x-dark.t0.text.txt"), "\nabc\n\n").unwrap();
    std::fs::write(
        dir.join("x-dark.t0.cursor.txt"),
        "OK 1 3 1 blinking_block\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("x-dark.dims.txt"),
        "OK 3 8 52 32 cell_w=6 cell_h=10 pad=2 pad_top=1 head=0 tab_rows=0 offset_x=0 offset_y=0 \
         band_left=0 band_top=0 crop_left=0 crop_top=0 layer_gravity=none\n",
    )
    .unwrap();
    png
}

fn field(cell: &J, key: &str) -> f64 {
    cell.get(key)
        .and_then(J::as_f64)
        .unwrap_or_else(|| panic!("{key} in {}", cell.py()))
}

#[test]
fn a_png_decodes_to_the_colours_it_was_encoded_from() {
    let path = Path::new("t.png");
    let rgb: Vec<u8> = (0..4 * 3 * 3)
        .map(|i| u8::try_from(i * 7 % 256).unwrap())
        .collect();
    let img = Image::decode(path, &encode(4, 3, aterm_png::ColorType::Rgb, &rgb)).unwrap();
    assert_eq!((img.width, img.height), (4, 3));
    let flat: Vec<u8> = img.px.iter().flatten().copied().collect();
    assert_eq!(flat, rgb);

    let rgba: Vec<u8> = vec![10, 20, 30, 0, 40, 50, 60, 255];
    let img = Image::decode(path, &encode(2, 1, aterm_png::ColorType::Rgba, &rgba)).unwrap();
    assert_eq!(img.px, vec![[10, 20, 30], [40, 50, 60]]);

    let img = Image::decode(
        path,
        &encode(2, 1, aterm_png::ColorType::Grayscale, &[0, 200]),
    )
    .unwrap();
    assert_eq!(img.px, vec![[0, 0, 0], [200, 200, 200]]);
}

#[test]
fn what_is_not_a_png_is_refused() {
    let path = Path::new("t.png");
    assert!(
        Image::decode(path, b"GIF89a")
            .unwrap_err()
            .contains("not a PNG")
    );
    let mut cut = encode(2, 2, aterm_png::ColorType::Rgb, &[7; 12]);
    cut.truncate(cut.len() - 20);
    assert!(
        Image::decode(path, &cut)
            .unwrap_err()
            .contains("does not decode")
    );
}

#[test]
fn contrast_is_wcag_2s_ratio() {
    let (black, white) = ([0, 0, 0], [255, 255, 255]);
    assert!((contrast(black, white) - 21.0).abs() < 1e-9);
    assert!((contrast(INK, INK) - 1.0).abs() < 1e-12);
    assert_eq!(contrast(INK, PAGE).to_bits(), contrast(PAGE, INK).to_bits());
    assert_eq!(round_to(contrast([0x76; 3], white), 2), 4.54);
    assert!(contrast([0x77; 3], white) < 4.5);
}

#[test]
fn rounding_is_pythons_round() {
    assert_eq!(round_to(0.125, 2), 0.12);
    assert_eq!(round_to(2.675, 2), 2.67);
    assert_eq!(round_to(0.0625, 3), 0.062);
    assert_eq!(round_to(1.0 / 3.0, 3), 0.333);
}

#[test]
fn a_dims_reply_gives_the_grid_and_an_unknown_origin_is_refused() {
    let real = "OK 14 72 1008 364 session=0 cell_w=14 cell_h=26 font_px=22.00 offset_x=0 offset_y=0 \
                band_left=0 band_top=0 crop_left=0 crop_top=0 pad=12 pad_top=2 head=0 tab_rows=0 \
                layer_gravity=none";
    let g = grid_from_dims(real).unwrap();
    assert_eq!(
        (g.ox, g.oy, g.cw, g.ch, g.rows, g.cols, g.source),
        (12, 2, 14, 26, 14, 72, "dims")
    );
    let shifted = real.replace("offset_x=0", "offset_x=3");
    assert!(grid_from_dims(&shifted).unwrap_err().contains("offset_x=3"));
    let chrome = real.replace("tab_rows=0", "tab_rows=1");
    assert!(grid_from_dims(&chrome).unwrap_err().contains("tab_rows=1"));
    assert!(grid_from_dims(&real.replace("cell_w=14 ", "")).is_err());
    assert!(grid_from_dims("ERR no session").is_err());
}

#[test]
fn wide_glyphs_take_two_cells_and_find_counts_columns() {
    let lines = vec!["$ echo 你好 fox".to_string()];
    let cells = screen_cells(&lines);
    let at = |c: char| cells.iter().find(|g| g.ch == c).map(|g| (g.col, g.width));
    assert_eq!(at('你'), Some((7, 2)));
    assert_eq!(at('好'), Some((9, 2)));
    assert_eq!(at('f'), Some((12, 1)));
    let found = find_cells(&lines, "好 f");
    let cols: Vec<i64> = found.iter().map(|g| g.col).collect();
    assert_eq!(cols, vec![9, 11, 12]);
    assert_eq!(find_cells(&["aaa".to_string()], "aa").len(), 4);
    assert!(find_cells(&lines, "").is_empty());
}

#[test]
fn a_frame_measured_against_its_own_clean_twin_changes_nothing() {
    let clean = clean_frame();
    for col in 0..3 {
        let cell = J::Obj(measure_cell(&clean, Some(&clean), &GRID, 1, col, 1));
        assert_eq!(cell.get("method").and_then(J::as_str), Some("mask"));
        assert_eq!(field(&cell, "contrast"), field(&cell, "clean_contrast"));
        assert_eq!(field(&cell, "contrast"), round_to(contrast(INK, PAGE), 2));
        assert_eq!(cell.get("core_px").and_then(J::as_i64), Some(12));
        assert_eq!(field(&cell, "local_contrast"), field(&cell, "contrast"));
        assert_eq!(field(&cell, "local_min"), field(&cell, "contrast"));
        assert_eq!(cell.get("local_under").and_then(J::as_i64), Some(0));
        assert_eq!(cell.get("local_px").and_then(J::as_i64), Some(12));
    }
    let blank = J::Obj(measure_cell(&clean, Some(&clean), &GRID, 0, 5, 1));
    assert_eq!(blank.get("blank"), Some(&J::Bool(true)));
    let diff = diff_vs_clean(&clean, &clean, None);
    assert_eq!(diff.get("px").and_then(J::as_i64), Some(0));
    assert_eq!(diff.get("outside_caret_bbox"), Some(&J::Null));
}

/// The ribbon under `c` meets the glyph's foot: the whole cell's median ground is still the page, but the foot's row
/// reads against the ribbon next to it, and the local measure (the one the thresholds judge) says so. `a` is untouched.
#[test]
fn a_ribbon_under_a_glyph_counts_against_it_and_nothing_else() {
    let (clean, frame) = (clean_frame(), trail_frame());
    let a = J::Obj(measure_cell(&frame, Some(&clean), &GRID, 1, 0, 1));
    let c = J::Obj(measure_cell(&frame, Some(&clean), &GRID, 1, 2, 1));
    assert_eq!(field(&a, "contrast"), field(&a, "clean_contrast"));
    assert_eq!(field(&a, "local_contrast"), field(&a, "clean_contrast"));
    assert_eq!(field(&c, "clean_contrast"), field(&a, "clean_contrast"));
    assert_eq!(field(&c, "contrast"), field(&c, "clean_contrast"));
    assert_eq!(
        field(&c, "local_contrast"),
        round_to(contrast(INK, RIBBON), 2)
    );
    assert_eq!(c.get("local_under").and_then(J::as_i64), Some(2));
    assert_eq!(judged(&c), Some(round_to(contrast(INK, RIBBON), 2)));
    let caret_box = GRID.cell_box(1, 3, 1);
    let diff = diff_vs_clean(&frame, &clean, Some(caret_box));
    assert_eq!(diff.get("outside_caret_px").and_then(J::as_i64), Some(12));
    assert_eq!(diff.get("px").and_then(J::as_i64), Some(72));
    let cb = GRID.cell_box(1, 2, 1);
    let want = J::Arr(vec![
        J::Int(cb.x0),
        J::Int(cb.y0 + 8),
        J::Int(cb.x1 - 1),
        J::Int(cb.y1 - 1),
    ]);
    assert_eq!(diff.get("outside_caret_bbox"), Some(&want));
}

#[test]
fn a_glyph_on_a_bed_of_its_own_colour_is_unreadable() {
    let clean = clean_frame();
    let mut frame = clean.clone();
    let b = GRID.cell_box(1, 1, 1);
    for y in b.y0..b.y1 {
        for x in b.x0..b.x1 {
            let (xu, yu) = (usize::try_from(x).unwrap(), usize::try_from(y).unwrap());
            if clean.at(xu, yu) == PAGE {
                frame.px[yu * W + xu] = RIBBON;
            }
        }
    }
    let cell = J::Obj(measure_cell(&frame, Some(&clean), &GRID, 1, 1, 1));
    assert_eq!(
        cell.get("ground").and_then(J::as_str),
        Some(hex(RIBBON).as_str())
    );
    assert_eq!(field(&cell, "contrast"), round_to(contrast(INK, RIBBON), 2));
    assert!(field(&cell, "contrast") < 1.5);
    let coarse = J::Obj(measure_cell(&clean, None, &GRID, 1, 1, 1));
    assert_eq!(coarse.get("method").and_then(J::as_str), Some("extreme"));
    assert_eq!(
        coarse.get("ground").and_then(J::as_str),
        Some(hex(PAGE).as_str())
    );
}

/// A body over the top of a glyph, on less than half its ground: the whole-cell median ground stays the page (the
/// gap: the cell measure reads it clean), while each ink pixel's own ring of ground lies on the body and the local
/// measure takes the glyph under the bar. A body kept a ring of page away from the ink (the knock-out's hairline)
/// reads clean locally too.
#[test]
fn a_body_over_part_of_a_glyph_is_judged_by_the_ground_next_to_its_ink() {
    let clean = clean_frame();
    let b = GRID.cell_box(1, 1, 1);
    let lay = |rows: std::ops::Range<i64>| {
        let mut frame = clean.clone();
        for y in rows {
            for x in b.x0..b.x1 {
                let (xu, yu) = (usize::try_from(x).unwrap(), usize::try_from(y).unwrap());
                if clean.at(xu, yu) == PAGE {
                    frame.px[yu * W + xu] = RIBBON;
                }
            }
        }
        J::Obj(measure_cell(&frame, Some(&clean), &GRID, 1, 1, 1))
    };
    // On the glyph's top row and above it.
    let over = lay(b.y0..b.y0 + 3);
    assert_eq!(
        field(&over, "contrast"),
        field(&over, "clean_contrast"),
        "negative control: the whole-cell ground misses a body on under half the ground"
    );
    assert_eq!(
        field(&over, "local_contrast"),
        round_to(contrast(INK, RIBBON), 2)
    );
    assert!(field(&over, "local_min") < LEGIBLE);
    // The top row's two, and the stroke pixel below them whose nearest ground (2 px out) is mostly the body.
    assert_eq!(over.get("local_under").and_then(J::as_i64), Some(3));
    // Two pixels clear of the ink: the ring next to every ink pixel is still the page.
    let clear = lay(b.y0..b.y0 + 1);
    assert_eq!(
        field(&clear, "local_contrast"),
        field(&clear, "clean_contrast")
    );
    assert_eq!(clear.get("local_under").and_then(J::as_i64), Some(0));
}

#[test]
fn the_report_reads_the_sidecars_and_turns_ratios_into_failures() {
    let dir = scratch("report");
    let png = gallery_files(&dir, &trail_frame());
    let png_s = png.to_string_lossy().into_owned();
    let a = parse_args(&args(&[&png_s, "--find", "abc", "--min", "3"])).unwrap();
    let r = report(&a).unwrap();
    assert_eq!(
        r.get("grid")
            .and_then(|g| g.get("source"))
            .and_then(J::as_str),
        Some("dims")
    );
    assert_eq!(r.get("page").and_then(J::as_str), Some(hex(PAGE).as_str()));
    let cells = r.get("cells").and_then(J::as_arr).unwrap();
    let chars: Vec<&str> = cells
        .iter()
        .filter_map(|c| c.get("char")?.as_str())
        .collect();
    assert_eq!(chars, vec!["a", "b", "c"]);
    let caret = r.get("caret").unwrap();
    assert_eq!(
        caret.get("colour").and_then(J::as_str),
        Some(hex(CARET).as_str())
    );
    assert_eq!(caret.get("coverage").and_then(J::as_f64), Some(1.0));
    assert_eq!(
        caret.get("shape").and_then(J::as_str),
        Some("blinking_block")
    );
    let newest: Vec<i64> = caret
        .get("newest")
        .and_then(J::as_arr)
        .unwrap()
        .iter()
        .filter_map(|c| c.get("col")?.as_i64())
        .collect();
    assert_eq!(newest, vec![2, 1, 0]);
    // `c`'s foot reads against the ribbon next to it (local_contrast), under 3:1.
    assert_eq!(
        r.get("failures"),
        Some(&J::Arr(vec![J::s("1 measured glyph cell(s) under 3.00:1")]))
    );
    let lenient = parse_args(&args(&[&png_s, "--find", "ab", "--min", "3"])).unwrap();
    assert_eq!(
        report(&lenient).unwrap().get("failures"),
        Some(&J::Arr(Vec::new()))
    );

    let strict = parse_args(&args(&[
        &png_s,
        "--find",
        "abc",
        "--min",
        "21",
        "--caret-min",
        "21",
    ]))
    .unwrap();
    let failures = report(&strict).unwrap();
    let failures = failures.get("failures").and_then(J::as_arr).unwrap();
    assert_eq!(failures.len(), 2);
    assert!(
        failures[0]
            .as_str()
            .unwrap()
            .starts_with("3 measured glyph cell(s) under 21.00:1")
    );
    assert!(failures[1].as_str().unwrap().starts_with("caret "));

    let coarse =
        report(&parse_args(&args(&[&png_s, "--row", "1", "--no-clean"])).unwrap()).unwrap();
    assert_eq!(coarse.get("clean"), Some(&J::Null));
    assert!(coarse.get("diff_vs_clean").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_report_renders_one_cell_per_line_and_parses_back() {
    let dir = scratch("render");
    let png = gallery_files(&dir, &trail_frame());
    let r = report(&parse_args(&args(&[&png.to_string_lossy()])).unwrap()).unwrap();
    let text = render(&r, 0);
    assert_eq!(J::parse(&text).unwrap(), J::parse(&compact(&r)).unwrap());
    let cells = r.get("cells").and_then(J::as_arr).unwrap().len();
    let cell_lines = text
        .lines()
        .filter(|l| l.contains("\"selector\": "))
        .count();
    assert_eq!(cell_lines, cells);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_grid_that_does_not_fit_or_a_missing_grid_is_an_error() {
    let dir = scratch("grid");
    let png = gallery_files(&dir, &trail_frame());
    let png_s = png.to_string_lossy().into_owned();
    let off = parse_args(&args(&[
        &png_s, "--origin", "2,1", "--cell", "6x10", "--cells", "9:0",
    ]))
    .unwrap();
    assert_eq!(
        report(&off).unwrap_err(),
        "cell 9,0 is outside the 8x3 grid"
    );
    let tall = dir.join("tall.dims.txt");
    std::fs::write(&tall, "OK 5 8 52 52 cell_w=6 cell_h=10 pad=2 pad_top=1\n").unwrap();
    let past = parse_args(&args(&[&png_s, "--dims", &tall.to_string_lossy()])).unwrap();
    assert!(
        report(&past)
            .unwrap_err()
            .contains("outside the 52x32 frame")
    );
    std::fs::remove_file(dir.join("x-dark.dims.txt")).unwrap();
    let none = parse_args(&args(&[&png_s])).unwrap();
    assert!(report(&none).unwrap_err().starts_with("no grid"));
    let half = parse_args(&args(&[&png_s, "--origin", "2,1"])).unwrap();
    assert!(report(&half).unwrap_err().contains("go together"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn arguments_parse_as_the_probe_always_took_them() {
    let a = parse_args(&args(&[
        "--find",
        "fox",
        "f.png",
        "--find",
        "the",
        "--row",
        "4",
        "--min",
        "7",
        "--no-clean",
    ]))
    .unwrap();
    assert_eq!(a.png, "f.png");
    assert_eq!(a.find, vec!["fox", "the"]);
    assert_eq!(a.rows, vec![4]);
    assert_eq!(a.min, Some(7.0));
    assert_eq!(a.newest, 3);
    assert!(a.no_clean);
    assert_eq!(
        parse_args(&args(&["f.png", "--newest", "0"]))
            .unwrap()
            .newest,
        0
    );

    for bad in [
        &["--row", "x", "f.png"][..],
        &[],
        &["a.png", "b.png"],
        &["f.png", "--bogus"],
        &["f.png", "--min"],
        &["f.png", "--caret-min", "high"],
    ] {
        let e = parse_args(&args(bad)).expect_err(&format!("{bad:?} is refused"));
        assert_eq!(e.code, 2, "{bad:?}: {:?}", e.msg);
    }
}
