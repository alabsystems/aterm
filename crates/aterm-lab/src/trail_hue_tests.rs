// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `trail-hue` tests: the HSV fixed points, the per-pixel judge, and a gallery tree read end to end with a planted
//! green flame and a planted pink spark as the negative controls.

use super::*;

use std::sync::atomic::{AtomicU32, Ordering};

use crate::trail_contrast::Grid;

/// Solarized Dark's page.
const TEAL: Rgb = [0x00, 0x2b, 0x36];
/// Fire's flame.
const FLAME: Rgb = [0xf8, 0x98, 0x19];
/// A gold star.
const GOLD_STAR: Rgb = [0xff, 0xbe, 0x3c];
/// A faint yellow over the teal page: olive green (hue 90).
const OLIVE_GREEN: Rgb = [0x40, 0x56, 0x2a];
/// The teal page a glow only lifted: its own hue.
const LIFTED_TEAL: Rgb = [0x0a, 0x4a, 0x5c];
/// A pink spark.
const PINK: Rgb = [0xf0, 0x5a, 0xb4];
/// Where a flame's base fades into the teal page (solarized-dark/fire-dark.t0 in looks10 round 1): hue 164, C* 10.7.
const FIRE_FRINGE: Rgb = [0x24, 0x3b, 0x35];
/// A flame core's fade over the teal: an olive grey, hue 78, C* 15.3.
const OLIVE_GREY: Rgb = [0x4a, 0x51, 0x39];
/// Sparkle's page-matched rail faded to a grey (context.t120): hue 222, C* 6.4.
const RAIL_GREY: Rgb = [0x29, 0x2d, 0x36];
/// Sparkle's rail's faded end: a brown grey, hue 14, C* 10.7.
const BROWN_GREY: Rgb = [0x4b, 0x37, 0x31];

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
const DIMS: &str = "OK 3 8 52 32 cell_w=6 cell_h=10 pad=2 pad_top=1 head=0 tab_rows=0 offset_x=0 offset_y=0 \
                    band_left=0 band_top=0 crop_left=0 crop_top=0 layer_gravity=none\n";

fn page() -> Image {
    Image::from_pixels(W, H, vec![TEAL; W * H])
}

fn paint(img: &mut Image, (x0, x1, y0, y1): Span, colour: Rgb) -> Image {
    let mut px: Vec<Rgb> = (0..img.height)
        .flat_map(|y| (0..img.width).map(move |x| (x, y)))
        .map(|(x, y)| img.at(x, y))
        .collect();
    for y in y0..y1 {
        for x in x0..x1 {
            let (x, y) = (usize::try_from(x).unwrap(), usize::try_from(y).unwrap());
            px[y * img.width + x] = colour;
        }
    }
    *img = Image::from_pixels(img.width, img.height, px);
    img.clone()
}

fn encode_rgb(img: &Image) -> Vec<u8> {
    let data: Vec<u8> = (0..img.height)
        .flat_map(|y| (0..img.width).map(move |x| (x, y)))
        .flat_map(|(x, y)| img.at(x, y))
        .collect();
    let mut out = Vec::new();
    let mut e = aterm_png::Encoder::new(
        &mut out,
        u32::try_from(img.width).unwrap(),
        u32::try_from(img.height).unwrap(),
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
    let dir = std::env::temp_dir().join(format!("lab-trail-hue-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// One gallery frame of `style` with its clean twin (the bare page), status, cursor (row 1, col 3) and dims.
fn write_frame(dir: &Path, stem: &str, frame: &str, style: &str, img: &Image) {
    std::fs::write(dir.join(format!("{stem}.{frame}.png")), encode_rgb(img)).unwrap();
    std::fs::write(
        dir.join(format!("{stem}.{frame}.clean.png")),
        encode_rgb(&page()),
    )
    .unwrap();
    std::fs::write(
        dir.join(format!("{stem}.{frame}.status.txt")),
        format!(
            "trail style=\"{style}\" resolved={style} pet_body=none word_icons=40,50,20,30 selection=front\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join(format!("{stem}.{frame}.cursor.txt")),
        "OK 1 3 1 blinking_block\n",
    )
    .unwrap();
    std::fs::write(dir.join(format!("{stem}.dims.txt")), DIMS).unwrap();
}

#[test]
fn hsv_names_the_hues_the_rulings_read() {
    let (h, s, v) = hsv([255, 0, 0]);
    assert!((h, s, v) == (0.0, 1.0, 1.0), "{h} {s} {v}");
    assert!((hsv([0, 255, 0]).0 - 120.0).abs() < 1e-9);
    assert!((hsv(TEAL).0 - 192.2).abs() < 0.1, "{}", hsv(TEAL).0);
    assert!(
        (hsv(OLIVE_GREEN).0 - 90.0).abs() < 1e-9,
        "{}",
        hsv(OLIVE_GREEN).0
    );
    assert_eq!(hsv([90, 90, 90]).1, 0.0, "a grey has no saturation");
    assert!((hue_gap(350.0, 10.0) - 20.0).abs() < 1e-9);
    assert!(Ruling::FireNeverGreen.forbids(hsv(OLIVE_GREEN).0));
    assert!(!Ruling::FireNeverGreen.forbids(hsv(FLAME).0));
    assert!(!Ruling::SparkleGold.forbids(hsv(GOLD_STAR).0));
    assert!(Ruling::SparkleGold.forbids(hsv(PINK).0));
    assert_eq!(Ruling::of_style("\"fire\""), Some(Ruling::FireNeverGreen));
    assert_eq!(Ruling::of_style("classic"), None);
}

/// The judge counts the pixels a trail turned to a forbidden hue and nothing else: a flame on the teal page keeps
/// Fire's family, the page a glow only lifted keeps its own hue, and the masks (the caret's cell, an icon) keep their
/// pixels out. Negative control: an olive-green fringe counts for Fire and a pink spark for Sparkle.
#[test]
fn the_judge_counts_only_a_hue_the_trail_brought() {
    let clean = page();
    let mut img = page();
    paint(&mut img, (2, 12, 2, 6), FLAME);
    paint(&mut img, (14, 24, 2, 6), GOLD_STAR);
    let fair = paint(&mut img, (26, 36, 2, 6), LIFTED_TEAL);
    assert_eq!(judge(&fair, &clean, Ruling::FireNeverGreen, &[]).px, 0);
    assert_eq!(
        judge(&fair, &clean, Ruling::SparkleGold, &[(2, 12, 2, 6)]).px,
        0,
        "the gold star is gold and the lifted teal the page's own hue"
    );
    let green = paint(&mut img, (2, 6, 20, 22), OLIVE_GREEN);
    let off = judge(&green, &clean, Ruling::FireNeverGreen, &[]);
    assert_eq!(off.px, 8, "{off:?}");
    assert_eq!(
        off.bbox,
        Some(PxBox {
            x0: 2,
            y0: 20,
            x1: 6,
            y1: 22
        })
    );
    assert_eq!(off.bins[9], 8, "hue 90 lands in the 90 bin");
    assert_eq!(
        judge(&green, &clean, Ruling::FireNeverGreen, &[(0, 10, 18, 24)]).px,
        0,
        "a masked fringe does not count"
    );
    let pink = paint(&mut img, (40, 44, 20, 22), PINK);
    assert_eq!(
        judge(&pink, &clean, Ruling::SparkleGold, &[(2, 12, 2, 6)]).px,
        8 + 8,
        "the olive fringe and the pink spark are both off gold (the flame is masked)"
    );
}

/// The visibility floor: the tinted greys where a flame or Sparkle's rail fades into the teal page read as no hue
/// (CIELAB chroma under [`CHROMA_MIN`]) and do not count, while the olive a viewer sees as green does. Negative
/// control: with the floor at 0 the greys count for their rulings.
#[test]
fn the_judge_counts_only_a_hue_a_viewer_can_see() {
    for c in [FIRE_FRINGE, OLIVE_GREY, RAIL_GREY, BROWN_GREY] {
        assert!(lab_chroma(c) < CHROMA_MIN, "{c:?}: C* {}", lab_chroma(c));
        let (_, s, v) = hsv(c);
        assert!(s >= SAT_MIN && v >= VAL_MIN, "{c:?} passes the HSV reading");
    }
    assert!(lab_chroma(OLIVE_GREEN) >= CHROMA_MIN);
    assert!(
        (lab_chroma(TEAL) - 14.5).abs() < 0.2,
        "{}",
        lab_chroma(TEAL)
    );
    let clean = page();
    let mut img = page();
    paint(&mut img, (2, 6, 2, 4), FIRE_FRINGE);
    paint(&mut img, (8, 12, 2, 4), OLIVE_GREY);
    paint(&mut img, (14, 18, 2, 4), RAIL_GREY);
    let greys = paint(&mut img, (20, 24, 2, 4), BROWN_GREY);
    assert_eq!(judge(&greys, &clean, Ruling::FireNeverGreen, &[]).px, 0);
    assert_eq!(judge(&greys, &clean, Ruling::SparkleGold, &[]).px, 0);
    assert_eq!(
        judge_seen(&greys, &clean, Ruling::FireNeverGreen, &[], 0.0).px,
        16,
        "control: the fringe and the olive grey are green with no floor"
    );
    assert_eq!(
        judge_seen(&greys, &clean, Ruling::SparkleGold, &[], 0.0).px,
        32,
        "control: every grey is off gold with no floor"
    );
    let olive = paint(&mut img, (30, 34, 10, 12), OLIVE_GREEN);
    assert_eq!(
        judge(&olive, &clean, Ruling::FireNeverGreen, &[]).px,
        8,
        "the olive a viewer sees as green still counts"
    );
}

/// A gallery tree read end to end: Fire's flame and Sparkle's gold stars pass, a classic frame is skipped, a green
/// pixel in the caret's cell or under a word icon does not count. Negative control: a planted olive-green fringe in
/// a Fire frame and a pink spark in a Sparkle frame are each counted off their family, and `--max 1` fails on two.
#[test]
fn a_gallery_is_read_and_a_planted_green_flame_is_caught() {
    let dir = scratch("tree");
    let mut fire = page();
    paint(&mut fire, (2, 30, 12, 20), FLAME);
    // The caret's cell (row 1, col 3: x 20..26, y 11..21) and the word icon (x 40..50, y 20..30) are masked.
    paint(&mut fire, (21, 25, 12, 14), OLIVE_GREEN);
    let fire = paint(&mut fire, (42, 46, 22, 24), OLIVE_GREEN);
    let mut sparkle = page();
    let sparkle = paint(&mut sparkle, (2, 18, 22, 26), GOLD_STAR);
    write_frame(&dir, "fire-dark", "t0", "fire", &fire);
    write_frame(&dir, "sparkle-dark", "t0", "sparkle", &sparkle);
    write_frame(&dir, "classic-dark", "t0", "classic", &fire);
    let args = |extra: &[&str]| {
        let mut v = vec![dir.to_string_lossy().into_owned()];
        v.extend(extra.iter().map(|s| (*s).to_string()));
        parse_args(&v).expect("args")
    };
    let int = |out: &J, k: &str| out.get(k).and_then(J::as_i64);
    let clean = report(&args(&[])).expect("a report");
    assert_eq!(int(&clean, "frames"), Some(2), "{}", render(&clean));
    assert_eq!(int(&clean, "skipped"), Some(1));
    assert_eq!(int(&clean, "off_family"), Some(0), "{}", render(&clean));
    assert_eq!(int(&clean, "fire_green_px"), Some(0));
    assert_eq!(int(&clean, "sparkle_off_gold_px"), Some(0));
    let mut planted = fire.clone();
    let planted = paint(&mut planted, (2, 10, 24, 26), OLIVE_GREEN);
    write_frame(&dir, "fire-dark", "t120", "fire", &planted);
    let mut spark = sparkle.clone();
    let spark = paint(&mut spark, (30, 34, 2, 4), PINK);
    write_frame(&dir, "sparkle-dark", "t120", "sparkle", &spark);
    let caught = report(&args(&["--max", "1"])).expect("a report");
    assert_eq!(int(&caught, "off_family"), Some(2), "{}", render(&caught));
    assert_eq!(int(&caught, "fire_green_px"), Some(16));
    assert_eq!(int(&caught, "fire_green_frames"), Some(1));
    assert_eq!(int(&caught, "sparkle_off_gold_px"), Some(8));
    assert_eq!(int(&caught, "sparkle_off_gold_frames"), Some(1));
    let listed = caught.get("listed").and_then(J::as_arr).expect("listed");
    assert_eq!(
        listed
            .iter()
            .filter_map(|f| f.get("frame").and_then(J::as_str))
            .collect::<Vec<_>>(),
        ["fire-dark.t120.png", "sparkle-dark.t120.png"]
    );
    assert_eq!(
        caught.get("failures").and_then(J::as_arr).map(<[J]>::len),
        Some(1),
        "two frames off their family over --max 1 fail"
    );
    let lenient = report(&args(&["--min-px", "10"])).expect("a report");
    assert_eq!(
        int(&lenient, "off_family"),
        Some(1),
        "the 8 px spark is under --min-px 10, the 16 px fringe is not"
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(GRID.cell_box(1, 3, 1).x0, 20);
}

#[test]
fn the_flags_are_checked() {
    assert!(parse_args(&[]).is_err());
    assert!(parse_args(&["x".into(), "--max".into(), "-1".into()]).is_err());
    assert!(parse_args(&["x".into(), "--min-px".into(), "lots".into()]).is_err());
    assert!(parse_args(&["x".into(), "--list".into(), "some".into()]).is_err());
    assert!(parse_args(&["x".into(), "--bogus".into()]).is_err());
    let a = parse_args(&["x".into(), "--list".into(), "all".into()]).expect("args");
    assert!(a.list_all);
    assert_eq!(a.min_px, 1);
}
