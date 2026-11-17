// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::trail_contrast::Rgb;

const WHITE: Rgb = [255, 255, 255];
const GREY: Rgb = [96, 96, 96];
const RED: Rgb = [200, 30, 40];

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { x, y, w, h }
}

/// A laid-out node of `kind` at `at`, saying `label`.
fn node(key: &str, kind: &str, label: &str, at: Rect) -> Node {
    Node {
        key: key.to_string(),
        kind: kind.to_string(),
        rect: at,
        clip: rect(0.0, 0.0, 400.0, 400.0),
        visible: true,
        clipped: false,
        role: "text".to_string(),
        label: label.to_string(),
        disabled: false,
        audit_id: String::new(),
        surface: None,
        fade: None,
    }
}

/// The synthetic frames' width: room for three cells.
const FRAME_W: usize = 96;

fn layout(nodes: Vec<Node>) -> Layout {
    Layout {
        scale: 1.0,
        viewport: rect(0.0, 0.0, FRAME_W as f64, 32.0),
        nodes,
        texts: Vec::new(),
    }
}

/// A shape drawn in a square cell, scaled to the cell as a mark's form is to its slot.
#[derive(Clone, Copy)]
enum Shape {
    /// A disc of radius 3/8 of the cell about its centre (6 px in a 16 px cell).
    Disc,
    /// A square of side 1/2 of the cell about its centre (8 px in a 16 px cell).
    Square,
    /// A right-pointing triangle about the cell's centre, its outline 2 px wide.
    Outlined,
    /// The same triangle, filled.
    Filled,
}

impl Shape {
    fn holds(self, x: f64, y: f64, side: f64) -> bool {
        let half = side / 2.0;
        let (dx, dy) = ((x - half) / half, (y - half) / half);
        // The triangle (-0.6, ±0.75), (0.75, 0) in units of the half cell, and how far inside it
        // a point is, in px.
        let inside = || {
            let left = dx + 0.6;
            let slant = (0.75 * (0.75 - dx) / 1.35 - dy.abs()) * 1.35 / 0.75_f64.hypot(1.35);
            left.min(slant) * half
        };
        match self {
            Shape::Disc => dx.hypot(dy) <= 0.75,
            Shape::Square => dx.abs() <= 0.5 && dy.abs() <= 0.5,
            Shape::Outlined => (-1.0..=1.0).contains(&inside()),
            Shape::Filled => inside() >= -1.0,
        }
    }
}

/// A `FRAME_W` x 32 white frame with a cell per entry, the `i`th at (8 + 32 i, 8), each `side`
/// px across and holding a shape in an ink.
fn frame_of(cells: &[(Shape, usize, Rgb)]) -> Image {
    let (w, h) = (FRAME_W, 32);
    let mut px = vec![WHITE; w * h];
    for (cell, &(shape, side, ink)) in cells.iter().enumerate() {
        let x0 = 8 + cell * 32;
        for y in 0..side {
            for x in 0..side {
                if shape.holds(x as f64 + 0.5, y as f64 + 0.5, side as f64) {
                    px[(8 + y) * w + x0 + x] = ink;
                }
            }
        }
    }
    Image::from_pixels(w, h, px)
}

/// Two 16 px cells, each holding a shape in an ink, and a third holding an outlined triangle in
/// grey: a form neither a disc nor a square is, so each of the first two meets another form.
fn frame(cells: [(Shape, Rgb); 2]) -> Image {
    let [(a, ink_a), (b, ink_b)] = cells;
    frame_of(&[(a, 16, ink_a), (b, 16, ink_b), (Shape::Outlined, 16, GREY)])
}

/// The marks of `cells` (key, label and side), laid out as [`frame_of`] draws them.
fn marks_of(cells: &[(&str, &str, f64)]) -> Layout {
    layout(
        cells
            .iter()
            .enumerate()
            .map(|(i, &(key, label, side))| {
                node(
                    key,
                    "custom",
                    label,
                    rect(8.0 + 32.0 * i as f64, 8.0, side, side),
                )
            })
            .collect(),
    )
}

/// The cells of [`frame`]: the first two saying `a` and `b`, the third `done`.
fn marks(a: &str, b: &str) -> Layout {
    marks_of(&[
        ("specimen/r0/mark", a, 16.0),
        ("specimen/r1/mark", b, 16.0),
        ("specimen/r2/mark", "done", 16.0),
    ])
}

/// The verb reads its flags into looks, scales and an out dir, names each frame by look and
/// scale, and refuses the navigator view as not run (exit 3). CONTROL: a view it does not know is
/// a usage error (exit 2), as is a scale other than 1 or 2.
#[test]
fn the_gallery_takes_the_card_and_refuses_the_navigator() {
    let plain = parse_args(&strings(&["/tmp/out.noindex"])).expect("the defaults");
    assert_eq!(
        plain,
        Args {
            looks: vec![Look::Dark, Look::Light, Look::HcDark, Look::HcLight],
            scales: vec![1, 2],
            specimen: None,
            keep: false,
            out: "/tmp/out.noindex".to_string(),
        }
    );
    let chosen = parse_args(&strings(&[
        "--view",
        "card",
        "--looks",
        "light,high-contrast",
        "--looks",
        "hc-dark",
        "--scales",
        "2",
        "--specimen",
        "/x/comprehension_specimen",
        "--keep",
        "/tmp/out.noindex",
    ]))
    .expect("a card run");
    assert_eq!(chosen.looks, [Look::Light, Look::HcDark, Look::HcLight]);
    assert_eq!(chosen.scales, [2]);
    assert_eq!(
        chosen.specimen.as_deref(),
        Some(Path::new("/x/comprehension_specimen"))
    );
    assert!(chosen.keep);
    assert_eq!(frame_name(Look::HcLight, 2), "card.hc-light.2x");
    assert_eq!(frame_name(Look::Dark, 1), "card.dark.1x");

    let navigator = parse_args(&strings(&["--view", "navigator", "/tmp/out.noindex"]))
        .expect_err("the navigator is not run");
    assert_eq!(navigator.code, 3, "{:?}", navigator.msg);
    assert!(
        navigator
            .msg
            .as_deref()
            .is_some_and(|m| m.contains("no navigator view")),
        "{:?}",
        navigator.msg
    );

    let unknown =
        parse_args(&strings(&["--view", "grid", "/tmp/out.noindex"])).expect_err("no such view");
    assert_eq!(unknown.code, 2, "{:?}", unknown.msg);
    let scale =
        parse_args(&strings(&["--scales", "3", "/tmp/out.noindex"])).expect_err("no such scale");
    assert_eq!(scale.code, 2, "{:?}", scale.msg);
    let no_out = parse_args(&[]).expect_err("an out dir is required");
    assert_eq!(no_out.code, 2, "{:?}", no_out.msg);
}

/// The grey judge passes two marks of different words drawn as a disc and a square, also where
/// one is 12 px and the other 16, and fails two of different words drawn alike. CONTROL: the same
/// disc twice under the waiting form's two words, and the disc in two inks, agree, so the judge
/// reads form and not colour; the same disc at 12 px and 16 px under two words comes out alike
/// once resampled and fails, so the judge across sizes tells a form from another and not a size
/// from another; and a mark compared with no mark of another form fails, as does a frame with
/// no two marks.
#[test]
fn the_grey_judge_tells_two_forms_apart_and_one_form_from_itself() {
    let apart_forms = grey_check(
        &frame_of(&[(Shape::Disc, 16, GREY), (Shape::Square, 16, GREY)]),
        &marks_of(&[
            ("specimen/r0/mark", "on track", 16.0),
            ("specimen/r1/mark", "idle", 16.0),
        ]),
    );
    assert!(apart_forms.pass(), "{:?}", apart_forms.failures);
    assert_eq!(apart_forms.judged, 1);
    assert!(
        apart_forms.worst.is_some_and(|w| w >= APART_MIN),
        "{:?}",
        apart_forms.worst
    );
    let across = grey_check(
        &frame_of(&[(Shape::Disc, 12, GREY), (Shape::Square, 16, GREY)]),
        &marks_of(&[
            ("specimen/r0/mark", "on track", 12.0),
            ("specimen/r1/mark", "idle", 16.0),
        ]),
    );
    assert!(across.pass(), "{:?}", across.failures);
    assert_eq!(across.judged, 1, "two sizes are judged");
    let one_form_two_sizes = grey_check(
        &frame_of(&[(Shape::Disc, 12, GREY), (Shape::Disc, 16, GREY)]),
        &marks_of(&[
            ("specimen/r0/mark", "on track", 12.0),
            ("specimen/r1/mark", "idle", 16.0),
        ]),
    );
    assert!(
        !one_form_two_sizes.pass(),
        "one disc at two sizes under two words must fail"
    );
    let lone = grey_check(
        &frame_of(&[(Shape::Disc, 16, GREY)]),
        &marks_of(&[("specimen/r0/mark", "on track", 16.0)]),
    );
    assert!(
        lone.failures
            .iter()
            .any(|f| f.detail.contains("compared with no mark of another form")),
        "{:?}",
        lone.failures
    );
    assert!(
        lone.failures
            .iter()
            .any(|f| f.detail == "no two marks judged"),
        "{:?}",
        lone.failures
    );

    let alike = grey_check(
        &frame([(Shape::Disc, GREY), (Shape::Disc, GREY)]),
        &marks("on track", "idle"),
    );
    assert!(!alike.pass(), "two words drawn as one disc must fail");

    let waiting = grey_check(
        &frame([(Shape::Disc, GREY), (Shape::Disc, GREY)]),
        &marks("quiet", "stuck"),
    );
    assert!(waiting.pass(), "{:?}", waiting.failures);
    let inks = grey_check(
        &frame([(Shape::Disc, GREY), (Shape::Disc, RED)]),
        &marks("quiet", "stuck"),
    );
    assert!(inks.pass(), "{:?}", inks.failures);
    let unread = grey_check(
        &frame([(Shape::Square, RED), (Shape::Square, GREY)]),
        &marks("track unread", "activity unread"),
    );
    assert!(unread.pass(), "{:?}", unread.failures);
    let split = grey_check(
        &frame([(Shape::Disc, GREY), (Shape::Square, GREY)]),
        &marks("quiet", "stuck"),
    );
    assert!(!split.pass(), "one form drawn two ways must fail");
}

/// The hollow judge passes an event mark drawn as an outlined triangle, at 12 px and at 16, and
/// reports the smallest hollow in square points. CONTROL: the same triangle filled fails, and so
/// does a frame with no event mark, so the judge never passes on nothing.
#[test]
fn the_hollow_judge_tells_an_outlined_event_mark_from_a_filled_one() {
    let event = |shape: Shape, side: usize| {
        let at = side as f64;
        hollow_check(
            &frame_of(&[(shape, side, GREY), (Shape::Square, 16, GREY)]),
            &marks_of(&[
                ("specimen/h1/mark", "commit", at),
                ("specimen/r2/icon", "ended", 16.0),
            ]),
        )
    };
    for side in [12, 16] {
        let outlined = event(Shape::Outlined, side);
        assert!(outlined.pass(), "{side} px: {:?}", outlined.failures);
        assert_eq!(outlined.judged, 1);
        assert!(
            outlined.worst.is_some_and(|w| w >= HOLLOW_MIN),
            "{side} px: {:?}",
            outlined.worst
        );
        let filled = event(Shape::Filled, side);
        assert!(!filled.pass(), "{side} px: a filled event mark must fail");
    }
    let none = hollow_check(
        &frame([(Shape::Disc, GREY), (Shape::Square, GREY)]),
        &marks("on track", "idle"),
    );
    assert!(!none.pass(), "a frame with no event mark must fail");
}

/// The column judge passes head rows whose topics share one x and whose icons share one right
/// edge, the row with no track mark included. CONTROL: one topic 2 pt right of the rest fails, and
/// so does one icon 2 pt short of the edge.
#[test]
fn the_column_judge_reads_every_topic_at_one_x() {
    let rows = |topic_x: [f64; 3], icon_right: [f64; 3]| {
        let mut nodes = Vec::new();
        for (i, (x, right)) in topic_x.into_iter().zip(icon_right).enumerate() {
            let y = 16.0 + 32.0 * i as f64;
            nodes.push(node(
                &format!("specimen/r{i}/topic"),
                "text",
                "",
                rect(x, y, 200.0, 20.0),
            ));
            nodes.push(node(
                &format!("specimen/r{i}/icon"),
                "custom",
                "idle",
                rect(right - 16.0, y, 16.0, 16.0),
            ));
            if i != 2 {
                nodes.push(node(
                    &format!("specimen/r{i}/mark"),
                    "custom",
                    "on track",
                    rect(16.0, y, 16.0, 16.0),
                ));
            }
        }
        layout(nodes)
    };
    let straight = columns_check(&rows([40.0; 3], [400.0; 3]));
    assert!(straight.pass(), "{:?}", straight.failures);
    assert_eq!(straight.judged, 6);

    let topic = columns_check(&rows([40.0, 42.0, 40.0], [400.0; 3]));
    assert_eq!(topic.failures.len(), 1, "{:?}", topic.failures);
    assert_eq!(topic.failures[0].key, "specimen/r1/topic");
    let icon = columns_check(&rows([40.0; 3], [400.0, 400.0, 398.0]));
    assert_eq!(icon.failures.len(), 1, "{:?}", icon.failures);
    assert_eq!(icon.failures[0].key, "specimen/r2/icon");
}
