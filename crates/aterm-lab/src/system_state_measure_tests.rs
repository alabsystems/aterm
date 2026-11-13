// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const WHITE: Rgb = [255, 255, 255];
const BLACK: Rgb = [0, 0, 0];

/// An audit reply's head at `scale`, its page 200x100 points at the window's origin.
fn head(scale: f64) -> String {
    format!(
        "app-inspection version=app/v1 revision=1 subject=view view=7 projection=audit\n\
         inspection-source source=presented view=7 window=1 generation=3 geometry=00 scale={scale:.3} viewport=0.0,0.0,200.0,100.0 model-current=true\n\
         paint-audit text-nodes=1 overflow=0 viewport=0.0,0.0,200.0,100.0 nodes=3\n\
         paint-node key=\"page\" kind=group rect=0.0,0.0,200.0,100.0 clip=0.0,0.0,200.0,100.0 visible=true clipped=false role=Application label=\"\" value=- action=- state=- focus-index=- audit-id=- surface=page\n"
    )
}

fn node(key: &str, kind: &str, rect: (f64, f64, f64, f64), extra: &str) -> String {
    let (x, y, w, h) = rect;
    format!(
        "paint-node key={key:?} kind={kind} rect={x:.1},{y:.1},{w:.1},{h:.1} clip=0.0,0.0,200.0,100.0 visible=true clipped=false role=Text label=\"\" value=- action=- state=- focus-index=- audit-id=- {extra}\n"
    )
}

fn text_fit(key: &str, painted: &str, overflow: bool) -> String {
    format!(
        "paint-text key={key:?} kind=text overflow={overflow} clip-truncated=false required=40.0 available=60.0 authored-available=60.0 painted={painted:?}\n"
    )
}

/// A `w`x`h` frame of `ground` with `paint`'s boxes `(x0, y0, x1, y1, colour)` filled in order.
fn frame(w: usize, h: usize, ground: Rgb, paint: &[(usize, usize, usize, usize, Rgb)]) -> Image {
    let mut px = vec![ground; w * h];
    for &(x0, y0, x1, y1, c) in paint {
        for y in y0..y1 {
            for x in x0..x1 {
                px[y * w + x] = c;
            }
        }
    }
    Image::from_pixels(w, h, px)
}

fn check<'a>(checks: &'a [Check], name: &str) -> &'a Check {
    checks.iter().find(|c| c.name == name).expect(name)
}

#[test]
fn wcag_relative_luminance_and_contrast_match_the_standard() {
    use crate::trail_contrast::luminance;
    assert!((luminance(WHITE) - 1.0).abs() < 1e-9);
    assert!(luminance(BLACK).abs() < 1e-9);
    // sRGB 0x80: ((0.502 + 0.055) / 1.055)^2.4 = 0.2159.
    assert!((luminance([128, 128, 128]) - 0.2159).abs() < 1e-3);
    // Pure green weighs 0.7152, red 0.2126 and blue 0.0722.
    assert!((luminance([0, 255, 0]) - 0.7152).abs() < 1e-9);
    assert!((luminance([255, 0, 0]) - 0.2126).abs() < 1e-9);
    assert!((luminance([0, 0, 255]) - 0.0722).abs() < 1e-9);
    assert!((contrast(WHITE, BLACK) - 21.0).abs() < 1e-9);
    assert!(
        (contrast(BLACK, WHITE) - 21.0).abs() < 1e-9,
        "order does not matter"
    );
    assert!((contrast([0x77, 0x77, 0x77], WHITE) - 4.48).abs() < 0.01);
    assert!((contrast([0x76, 0x76, 0x76], WHITE) - 4.54).abs() < 0.01);
    assert!((contrast([0x33, 0x66, 0x99], [0x33, 0x66, 0x99]) - 1.0).abs() < 1e-9);
}

#[test]
fn audit_fields_read_debug_strings_and_pass_over_bare_words() {
    let f = fields(
        r#"key="a \"b\" \\ c" kind=text label="caf\u{e9} \u{1f408}" state=enabled:false focused:true range=0..10 painted="x y""#,
    );
    let get = |name: &str| field(&f, name).map(str::to_owned);
    assert_eq!(get("key").as_deref(), Some(r#"a "b" \ c"#));
    assert_eq!(get("kind").as_deref(), Some("text"));
    assert_eq!(get("label").as_deref(), Some("café 🐈"));
    assert_eq!(get("state").as_deref(), Some("enabled:false"));
    assert_eq!(get("focused"), None, "a bare word is no field");
    assert_eq!(get("painted").as_deref(), Some("x y"));
}

#[test]
fn a_layout_reads_scale_viewport_nodes_and_fits() {
    let text = format!(
        "{}{}{}",
        head(2.0),
        node("t", "text", (8.0, 12.0, 60.0, 20.0), "surface=-"),
        text_fit("t", "Hello", false)
    );
    let layout = parse_layout(&text).unwrap();
    assert!((layout.scale - 2.0).abs() < 1e-9);
    assert_eq!(
        layout.viewport,
        Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 100.0
        }
    );
    assert_eq!(layout.nodes.len(), 2);
    assert_eq!(layout.nodes[0].surface.as_deref(), Some("page"));
    assert_eq!(layout.nodes[1].kind, "text");
    assert_eq!(layout.texts[0].painted, "Hello");
    assert!(names_surfaces(&layout));
    assert!(
        parse_layout("paint-node key=\"x\"").is_err(),
        "no source line"
    );
    assert_eq!(
        Rect::parse("1.5,2,3,4").map(|r| r.px(2.0)),
        Some(PxBox {
            x0: 3,
            y0: 4,
            x1: 9,
            y1: 12
        })
    );
}

/// A page of `#f6f6f6` with one 80x60 pt card at (20, 20) of `card`, one text node in it painted
/// `ink`, and one 16x16 glyph slot at (30, 42) holding a mark of `glyph` on a row wash of
/// `wash`, at scale 1.
fn page(card: Rgb, ink: Rgb, glyph: Rgb, wash: Rgb) -> (Image, Layout) {
    let text = format!(
        "{}{}{}{}{}",
        head(1.0),
        node("card", "group", (20.0, 20.0, 80.0, 60.0), "surface=card"),
        node("card/title", "text", (28.0, 24.0, 60.0, 12.0), "surface=-"),
        text_fit("card/title", "Mac settings", false),
        node(
            "card/glyph",
            "custom",
            (30.0, 42.0, 16.0, 16.0),
            "surface=-"
        ),
    );
    let image = frame(
        200,
        100,
        [0xf6, 0xf6, 0xf6],
        &[
            (20, 20, 100, 80, card),
            // The words: strokes of ink on the card.
            (30, 27, 34, 33, ink),
            (40, 27, 44, 33, ink),
            // The row's wash, inside the card's padding; the glyph is a 6x6 mark in its slot.
            (26, 36, 94, 64, wash),
            (35, 47, 41, 53, glyph),
        ],
    );
    (image, parse_layout(&text).unwrap())
}

#[test]
fn text_passes_dark_ink_and_fails_pale_ink() {
    let (img, layout) = page(WHITE, [0x22, 0x22, 0x22], BLACK, WHITE);
    let checks = measure(&img, &layout);
    let text = check(&checks, "text");
    assert!(text.pass(), "{text:?}");
    assert_eq!(text.judged, 1);
    assert!(text.worst.unwrap() > 15.0);
    // The negative control: #999 on white is 2.85:1.
    let (img, layout) = page(WHITE, [0x99, 0x99, 0x99], BLACK, WHITE);
    let checks = measure(&img, &layout);
    let text = check(&checks, "text");
    assert!(!text.pass());
    assert_eq!(text.failures[0].key, "card/title");
    assert!((text.failures[0].value - 2.85).abs() < 0.01, "{text:?}");
}

#[test]
fn glyph_is_judged_against_its_wash_and_the_card() {
    // A dark glyph on a pale wash on a white card.
    let (img, layout) = page(WHITE, BLACK, [0x30, 0x30, 0x30], [0xee, 0xf2, 0xff]);
    let checks = measure(&img, &layout);
    let glyph = check(&checks, "glyph");
    assert!(glyph.pass(), "{glyph:?}");
    assert_eq!(glyph.judged, 1);
    assert!(glyph.failures.is_empty());
    // The negative control: a mid-grey glyph on a mid-grey wash is under 3:1 against the wash,
    // though it clears 3:1 against the white card.
    let (img, layout) = page(WHITE, BLACK, [0x80, 0x80, 0x80], [0xb0, 0xb0, 0xb0]);
    let checks = measure(&img, &layout);
    let glyph = check(&checks, "glyph");
    assert!(!glyph.pass(), "{glyph:?}");
    assert!(contrast([0x80, 0x80, 0x80], WHITE) > 3.0);
    assert!(glyph.failures[0].value < 3.0);
    assert!(glyph.failures[0].detail.contains("#b0b0b0"));
    // A glyph that paints nothing is blank, not a failure.
    let (img, layout) = page(WHITE, BLACK, [0xee, 0xf2, 0xff], [0xee, 0xf2, 0xff]);
    let glyph = measure(&img, &layout).remove(1);
    assert!(glyph.pass());
    assert_eq!(glyph.skipped, vec![("blank", 1)]);
}

#[test]
fn a_card_must_stand_off_the_page() {
    let (img, layout) = page([0xd8, 0xd8, 0xd8], BLACK, BLACK, [0xd8, 0xd8, 0xd8]);
    let checks = measure(&img, &layout);
    let card = check(&checks, "card");
    assert!(card.pass(), "{card:?}");
    assert_eq!(card.judged, 1);
    // The negative control: white on #f6f6f6 is 1.08:1, the light-mode card that melts away.
    let (img, layout) = page(WHITE, BLACK, BLACK, WHITE);
    let checks = measure(&img, &layout);
    let card = check(&checks, "card");
    assert!(!card.pass());
    assert!((card.failures[0].value - 1.08).abs() < 0.01, "{card:?}");
    assert!(card.failures[0].detail.contains("#ffffff on page #f6f6f6"));
}

#[test]
fn a_painted_surface_the_audit_names_a_card_is_judged_as_one_and_never_as_a_mark() {
    // The setup screen paints its list's card as a custom node under a plain group.
    let layout_of = |surface: &str| {
        parse_layout(&format!(
            "{}{}{}",
            head(1.0),
            node("list", "group", (20.0, 20.0, 80.0, 60.0), "surface=none"),
            node("list/surface", "custom", (20.0, 20.0, 80.0, 60.0), surface),
        ))
        .unwrap()
    };
    let grey = [0xd8, 0xd8, 0xd8];
    let img = frame(200, 100, [0xf6, 0xf6, 0xf6], &[(20, 20, 100, 80, grey)]);
    let checks = measure(&img, &layout_of("surface=card"));
    let card = check(&checks, "card");
    assert!(card.pass(), "{card:?}");
    assert_eq!(card.judged, 1);
    assert_eq!(check(&checks, "glyph").judged, 0);
    // The negative control: the same surface unnamed is no card, and is read as a mark.
    let checks = measure(&img, &layout_of("surface=-"));
    assert_eq!(check(&checks, "card").judged, 0);
    assert_eq!(check(&checks, "glyph").judged, 1);
    // A white one melts into the page: the check sees it now.
    let img = frame(200, 100, [0xf6, 0xf6, 0xf6], &[(20, 20, 100, 80, WHITE)]);
    assert!(!check(&measure(&img, &layout_of("surface=card")), "card").pass());
}

#[test]
fn words_marks_and_cards_are_read_above_a_lists_fade() {
    let fade_from = |layout: &mut Layout, top: f64| {
        for node in &mut layout.nodes[1..] {
            node.fade = Some((top, 100.0));
        }
    };
    // Pale words fail (the negative control); under the list's fade they are dimmed by design,
    // and so is a mark.
    let (img, mut layout) = page(WHITE, [0x99, 0x99, 0x99], [0xcc, 0xcc, 0xcc], WHITE);
    assert!(!check(&measure(&img, &layout), "text").pass());
    assert!(!check(&measure(&img, &layout), "glyph").pass());
    fade_from(&mut layout, 26.0);
    let checks = measure(&img, &layout);
    assert!(check(&checks, "text").pass());
    assert_eq!(check(&checks, "text").skipped, vec![("faded", 1)]);
    assert!(check(&checks, "glyph").pass());
    assert_eq!(check(&checks, "glyph").skipped, vec![("faded", 1)]);
    // A fade that starts below them dims nothing they show.
    fade_from(&mut layout, 70.0);
    assert!(!check(&measure(&img, &layout), "text").pass());
    // A card whose lower part the list fades into the page: read whole, it melts (the negative
    // control); read above the fade, it stands off the page.
    let (_, mut layout) = page([0xd8, 0xd8, 0xd8], BLACK, BLACK, [0xd8, 0xd8, 0xd8]);
    let grey = [0xd8, 0xd8, 0xd8];
    let img = frame(
        200,
        100,
        [0xf6, 0xf6, 0xf6],
        &[
            (20, 20, 100, 80, grey),
            (30, 27, 34, 33, BLACK),
            (40, 27, 44, 33, BLACK),
            // From 40 pt down the list has faded everything into the page.
            (0, 40, 200, 100, [0xf6, 0xf6, 0xf6]),
        ],
    );
    let card = check(&measure(&img, &layout), "card").clone();
    assert!(!card.pass(), "{card:?}");
    layout.nodes[1].fade = Some((40.0, 100.0));
    let card = check(&measure(&img, &layout), "card").clone();
    assert!(card.pass(), "{card:?}");
    assert!(card.failures.is_empty() && card.judged == 1);
    // A sliver of a card wholly inside the fade shows no surface: passed over, not read as the
    // page against itself.
    layout.nodes[1].fade = Some((10.0, 100.0));
    let card = check(&measure(&img, &layout), "card").clone();
    assert!(card.pass(), "{card:?}");
    assert_eq!((card.judged, card.skipped.clone()), (0, vec![("faded", 1)]));
}

#[test]
fn painted_word_bands_distinguish_padding_from_even_a_small_cut_through_words() {
    let judge = |top: f64, band: &str| {
        let line = node("t", "text", (8.0, 12.0, 40.0, 36.0), "surface=-").replace(
            "clip=0.0,0.0,200.0,100.0",
            &format!("clip=8.0,{top},40.0,{}", 48.0 - top),
        );
        let text = text_fit("t", "Words", false)
            .replace("painted=", &format!("word-band={band} painted="));
        clip_check(&parse_layout(&format!("{}{line}{text}", head(1.0))).unwrap())
    };
    assert!(
        judge(20.0, "23.0,39.0").pass(),
        "only the line box's padding was cut"
    );
    assert!(
        !judge(20.0, "18.0,39.0").pass(),
        "the edge cuts the word band"
    );
    assert!(
        !judge(14.0, "13.0,39.0").pass(),
        "even a small cut through words fails"
    );
    assert!(
        !judge(20.0, "-").pass(),
        "missing evidence keeps the line-box check"
    );
    assert!(
        !judge(20.0, "NaN,39.0").pass(),
        "invalid evidence keeps the line-box check"
    );
    let hidden = judge(20.0, "13.0,19.0");
    assert!(hidden.pass());
    assert_eq!(hidden.skipped, vec![("hidden", 1)]);
}

#[test]
fn clipped_or_ellipsised_words_fail_and_scrolled_away_words_do_not() {
    let fit = |overflow: bool, clipped: bool, visible: bool| {
        let line = node("t", "text", (8.0, 8.0, 40.0, 20.0), "surface=-")
            .replace("clipped=false", &format!("clipped={clipped}"))
            .replace("visible=true", &format!("visible={visible}"));
        let layout = parse_layout(&format!(
            "{}{line}{}",
            head(1.0),
            text_fit("t", "Words…", overflow)
        ))
        .unwrap();
        clip_check(&layout)
    };
    let fine = fit(false, false, true);
    assert!(fine.pass());
    assert_eq!(fine.judged, 1);
    // The negative controls: an ellipsis, and a clip edge through the words.
    let ellipsised = fit(true, false, true);
    assert!(!ellipsised.pass());
    assert!(ellipsised.failures[0].detail.contains("ellipsised"));
    let cut = fit(false, true, true);
    assert!(!cut.pass());
    assert!(cut.failures[0].detail.contains("clip edge"));
    let hidden = fit(true, true, false);
    assert!(hidden.pass());
    assert_eq!(hidden.skipped, vec![("hidden", 1)]);
    // A scroll edge that cuts the words fails them here (and the text check passes over them);
    // one that trims only a line box's padding does not.
    let scrolled = |clip_h: f64, fade: &str| {
        let line = node(
            "t",
            "text",
            (8.0, 80.0, 40.0, 30.0),
            &format!("surface=- {fade}"),
        )
        .replace(
            "clip=0.0,0.0,200.0,100.0",
            &format!("clip=8.0,80.0,40.0,{clip_h:.1}"),
        );
        let layout = parse_layout(&format!(
            "{}{line}{}",
            head(1.0),
            text_fit("t", "Desktop", false)
        ))
        .unwrap();
        let (frame, _) = page(WHITE, BLACK, BLACK, WHITE);
        (clip_check(&layout), text_check(&frame, &layout))
    };
    let (clip, text) = scrolled(13.0, "fade=-");
    assert!(!clip.pass());
    assert!(clip.failures[0].detail.contains("scroll edge"), "{clip:?}");
    assert_eq!(text.skipped, vec![("cut", 1)]);
    let (clip, _) = scrolled(29.6, "fade=-");
    assert!(clip.pass(), "{clip:?}");
    // At the list's faded bottom edge the cut is the list's own cue that more lies below, as
    // the audit's sliced-marks rule has it; an edge above the fade is a hard cut still.
    let (clip, _) = scrolled(13.0, "fade=90.0,110.0");
    assert!(clip.pass(), "{clip:?}");
    assert_eq!(clip.skipped, vec![("faded", 1)]);
    let (clip, _) = scrolled(13.0, "fade=96.0,110.0");
    assert!(!clip.pass(), "{clip:?}");
}

#[test]
fn a_control_backdrop_on_a_card_is_not_judged_as_a_card_on_the_page() {
    let (img, _) = page([0xd8, 0xd8, 0xd8], BLACK, BLACK, [0xd8, 0xd8, 0xd8]);
    let text = format!(
        "{}{}{}",
        head(1.0),
        node("card", "group", (20.0, 20.0, 80.0, 60.0), "surface=card"),
        node(
            "card/choice",
            "group",
            (60.0, 24.0, 36.0, 12.0),
            "surface=card"
        ),
    );
    let layout = parse_layout(&text).unwrap();
    let card = check(&measure(&img, &layout), "card").clone();
    assert!(card.pass(), "{card:?}");
    assert_eq!(card.judged, 1);
    assert_eq!(card.skipped, vec![("nested", 1)]);
}

#[test]
fn the_rhythm_holds_tops_and_heights_to_four_points() {
    let rhythm = |nodes: &[(f64, f64, f64, f64)]| {
        let mut text = head(1.0);
        for (i, r) in nodes.iter().enumerate() {
            text.push_str(&node(&format!("n{i}"), "group", *r, "surface=none"));
        }
        rhythm_check(&parse_layout(&text).unwrap())
    };
    let on = rhythm(&[
        (0.0, 8.0, 100.0, 44.0),
        (0.0, 52.0, 100.0, 32.0),
        (0.0, 84.0, 50.0, 1.0),
    ]);
    assert!(on.pass(), "{on:?}");
    assert_eq!(
        on.judged, 2,
        "the hairline and the full-height page are not judged"
    );
    // The negative controls: a top 2 pt off the grid, and a 30 pt height.
    let off = rhythm(&[(0.0, 10.0, 100.0, 44.0), (0.0, 56.0, 100.0, 30.0)]);
    assert!(!off.pass());
    assert_eq!(off.failures.len(), 2);
    assert!((off.worst.unwrap() - 2.0).abs() < 1e-9);
    assert!((off_grid(3.0) - 1.0).abs() < 1e-9);
    assert!((off_grid(-1.0) - 1.0).abs() < 1e-9);
    assert!(off_grid(12.05) < RHYTHM_SLACK);
}

#[test]
fn solid_ink_ignores_a_lone_antialiasing_pixel() {
    let ground = WHITE;
    let px = [WHITE, WHITE, [0x10; 3], [0x55; 3], [0x55; 3], WHITE];
    assert_eq!(solid_ink(&px, ground), Some([0x55; 3]));
    assert_eq!(solid_ink(&[WHITE, [0x10; 3]], ground), Some([0x10; 3]));
    assert_eq!(solid_ink(&[WHITE, WHITE], ground), None);
}
