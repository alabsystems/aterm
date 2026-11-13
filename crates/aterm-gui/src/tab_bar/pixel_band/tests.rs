// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const CELL_W: usize = 9;
const CELL_H: usize = 21;
const BAND_TOP: usize = 2;

fn geometry(cols: usize, strip_rows: usize) -> BandGeometry {
    BandGeometry {
        cols,
        cell_w: CELL_W,
        cell_h: CELL_H,
        strip_rows,
        band_top_px: BAND_TOP,
        scale: 1.0,
        seam_top_px: None,
        foot_px: 0,
    }
}

fn plain(n: usize) -> Vec<TabStripMetadata> {
    vec![
        TabStripMetadata {
            icon: None,
            dirty: false,
            busy: false,
            attention: ChipLevel::Off,
            conn: None,
            closable: true,
            team: TabTeam::NONE,
            load: 0,
        };
        n
    ]
}

fn band<'a>(
    segments: &'a [TabSegment],
    titles: &'a [String],
    metadata: &'a [TabStripMetadata],
    paint: StripPaint<'a>,
    geometry: BandGeometry,
) -> BandInput<'a> {
    BandInput {
        segments,
        titles,
        metadata,
        paint,
        active: 0,
        palette: Theme::default().into(),
        geometry,
        floor: StripFloor::Ground,
    }
}

fn image_of(rows: &[Vec<(usize, ImageRef)>]) -> (Vec<u8>, usize, usize) {
    let image = &rows[0][0].1.image;
    let ImageFormat::RawRgba8 { width, height } = image.format else {
        panic!("the band is a RawRgba8 raster");
    };
    (image.bytes.clone(), usize::from(width), usize::from(height))
}

fn ink_bbox(
    rgba: &[u8],
    w: usize,
    h: usize,
    px0: usize,
    px1: usize,
) -> Option<(usize, usize, usize, usize)> {
    let mut bbox: Option<(usize, usize, usize, usize)> = None;
    for y in 0..h {
        for x in px0..px1.min(w) {
            if rgba[(y * w + x) * 4 + 3] > 0 {
                bbox = Some(match bbox {
                    None => (x, y, x + 1, y + 1),
                    Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x + 1), d.max(y + 1)),
                });
            }
        }
    }
    bbox
}

fn ink_bbox_off_surfaces(
    rgba: &[u8],
    w: usize,
    h: usize,
    px0: usize,
    px1: usize,
    surfaces: &[[u8; 3]],
) -> Option<(usize, usize, usize, usize)> {
    let far = |px: &[u8], s: &[u8; 3]| {
        px[0]
            .abs_diff(s[0])
            .max(px[1].abs_diff(s[1]))
            .max(px[2].abs_diff(s[2]))
            > 40
    };
    let mut bbox: Option<(usize, usize, usize, usize)> = None;
    for y in 0..h {
        for x in px0..px1.min(w) {
            let px = &rgba[(y * w + x) * 4..(y * w + x) * 4 + 3];
            if surfaces.iter().all(|s| far(px, s)) {
                bbox = Some(match bbox {
                    None => (x, y, x + 1, y + 1),
                    Some((a, b, c, d)) => (a.min(x), b.min(y), c.max(x + 1), d.max(y + 1)),
                });
            }
        }
    }
    bbox
}

fn with_ui_faces() -> bool {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    crate::tray_raster::strip_band_ui_ready()
}

#[test]
fn declines_without_a_ui_face_so_the_cell_strip_is_untouched() {
    crate::tray_raster::clear_ui_fonts_for_test();
    let metadata = plain(2);
    let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
    let titles = vec!["alpha".to_string(), "beta".to_string()];
    let input = band(
        &segments,
        &titles,
        &metadata,
        StripPaint::default(),
        geometry(80, 1),
    );
    assert!(raster_band(&input, &[]).is_none());
}

fn version_strip(
    cols: u16,
    attention: bool,
) -> (Vec<TabSegment>, Vec<TabStripMetadata>, TabSegment) {
    let metadata = plain(2);
    let slot = version_slot_cols(CELL_W, CELL_H + BAND_TOP, 1.0, attention);
    let segments = layout_segments_with_version(cols, 2, &metadata, 0, false, slot);
    let label = segments
        .iter()
        .copied()
        .find(|s| matches!(s.kind, TabHit::Version { .. }))
        .expect("the label fits beside two tabs at this width");
    (segments, metadata, label)
}

#[test]
fn version_slot_cols_cover_the_painted_width() {
    use crate::menu::{VersionRung, strip_version_label};
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    for scale in [1.0f32, 1.25, 1.5, 2.0] {
        let cell_w = (CELL_W as f32 * scale).round() as usize;
        let band_h = ((CELL_H + BAND_TOP) as f32 * scale).round() as usize;
        let label_px = band_label_px(band_h as f32, scale);
        let pad = version_pad_px(scale);
        for attention in [false, true] {
            let slot = version_slot_cols(cell_w, band_h, scale, attention);
            assert_eq!(slot.attention, attention);
            for (rung, w) in [
                (VersionRung::Full, slot.full),
                (VersionRung::Short, slot.short),
            ] {
                let label = strip_version_label(rung, attention).expect("a dev build");
                let need = crate::tray_raster::ui_text_width_for(TextFace::Ui, &label, label_px)
                    + 2.0 * pad;
                let px = |cells: u16| f32::from(cells) * cell_w as f32;
                assert!(
                    px(w) >= need,
                    "{rung:?} at {scale}x: {w} cells ({} px) < {need} px",
                    px(w)
                );
                assert!(
                    px(w - 1) < need,
                    "{rung:?} at {scale}x: {w} cells is more than the ceiling of {need} px"
                );
            }
            assert!(slot.short <= slot.full, "the short rung is never longer");
        }
    }
    assert_eq!(
        version_slot_cols(0, CELL_H + BAND_TOP, 1.0, false),
        VersionSlotCols::NONE,
        "no cell width, no slot"
    );
    crate::tray_raster::clear_ui_fonts_for_test();
    assert_eq!(
        version_slot_cols(CELL_W, CELL_H + BAND_TOP, 1.0, false),
        VersionSlotCols::NONE,
        "no UI face, no slot — the first frames keep today's strip"
    );
}

#[test]
fn the_band_draws_the_version_label_in_the_ui_face() {
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let (segments, metadata, label) = version_strip(132, false);
    let titles = vec!["Hello".to_string(), "World".to_string()];
    let input = band(
        &segments,
        &titles,
        &metadata,
        StripPaint::default(),
        geometry(132, 1),
    );
    let rows = raster_band(&input, &[]).expect("UI faces installed ⇒ a band");
    let covered: Vec<usize> = rows[0].iter().map(|(c, _)| *c).collect();
    assert!(
        (label.start_col..label.end_col).all(|c| covered.contains(&usize::from(c))),
        "the band owns the label's columns"
    );
    let (rgba, w, h) = image_of(&rows);
    let colors = strip_colors_with_active(Theme::default(), None);
    let (seg0, seg1) = (
        usize::from(label.start_col) * CELL_W,
        usize::from(label.end_col) * CELL_W,
    );
    assert!(
        ink_bbox_off_surfaces(&rgba, w, h, seg0, seg1, &[colors.band_bg]).is_some(),
        "the label inks its slot"
    );
    crate::tray_raster::clear_ui_fonts_for_test();
}

fn pixel(rgba: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
    let i = (y * w + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(p, q)| p.abs_diff(q) <= 1)
}

#[test]
fn window_buttons_are_round_and_take_the_hover_wash() {
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let metadata = plain(1);
    let frame = crate::client_frame::ClientFrame {
        minimize: true,
        maximize: true,
        window_menu: true,
        maximized: false,
        edges: true,
        header: true,
        layout: crate::client_frame::ButtonLayout::FALLBACK,
    };
    let caption = CaptionSlot::measure(Some(frame), CELL_W, 1.0);
    let segments =
        layout_segments_with_caption(80, 1, &metadata, 0, false, VersionSlotCols::NONE, caption);
    let close = segments.last().copied().expect("the close button");
    assert_eq!(close.kind, TabHit::Caption(CaptionButton::Close));
    let titles = vec!["one".to_string()];
    let colors = strip_colors_with_active(Theme::default(), None);
    for (hovered, want) in [
        (None, colors.chip_bg),
        (Some(CaptionButton::Close), colors.hover_bg),
        (Some(CaptionButton::Minimize), colors.chip_bg),
    ] {
        let paint = StripPaint {
            caption_hovered: hovered,
            ..StripPaint::default()
        };
        let input = band(&segments, &titles, &metadata, paint, geometry(80, 1));
        let rows = raster_band(&input, &[]).expect("band");
        let (rgba, w, h) = image_of(&rows);
        let cx = (usize::from(close.start_col) + usize::from(close.end_col)) * CELL_W / 2;
        let got = pixel(&rgba, w, cx - 5, h / 2);
        assert!(near(got, want), "{hovered:?}: {got:?} vs {want:?}");
        let mark = pixel(&rgba, w, cx, h / 2);
        assert!(!near(mark, want), "{hovered:?}: no mark at the centre");
        let edge = pixel(&rgba, w, usize::from(close.start_col) * CELL_W + 1, h / 2);
        assert!(
            near(edge, colors.band_bg),
            "{edge:?} vs {:?}",
            colors.band_bg
        );
    }
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn covers_every_column_with_one_band_image_of_the_strip_footprint() {
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let metadata = plain(2);
    let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
    let titles = vec!["alpha".to_string(), "beta".to_string()];
    let input = band(
        &segments,
        &titles,
        &metadata,
        StripPaint::default(),
        geometry(80, 1),
    );
    let rows = raster_band(&input, &[]).expect("UI faces installed ⇒ a band");
    assert_eq!(rows.len(), 1, "one ref list per strip row");
    let cols: Vec<usize> = rows[0].iter().map(|(c, _)| *c).collect();
    assert_eq!(
        cols,
        (0..80).collect::<Vec<_>>(),
        "every column covered, sorted"
    );
    let first = &rows[0][0].1.image;
    assert!(
        rows[0].iter().all(|(_, r)| Arc::ptr_eq(&r.image, first)),
        "one shared image Arc for the whole band"
    );
    assert!(
        rows[0]
            .iter()
            .all(|(c, r)| usize::from(r.cell_col) == *c && r.cell_row == 0),
        "each ref names its own tile"
    );
    let (rgba, w, h) = image_of(&rows);
    let lift = BAND_TOP;
    assert_eq!((w, h), (80 * CELL_W, CELL_H + lift), "canvas vs footprint");
    assert_eq!((first.cols, first.rows, first.z_index), (80, 1, 0));
    assert_eq!(usize::from(first.band_lift_px), lift);
    assert!(
        (0..w * h).all(|i| rgba[i * 4 + 3] == 255),
        "an opaque bar has no see-through pixels"
    );

    let one = plain(1);
    let segments = layout_segments_with_metadata(60, 1, &one, 0, false);
    let titles = vec!["only".to_string()];
    let input = band(
        &segments,
        &titles,
        &one,
        StripPaint::default(),
        geometry(60, 2),
    );
    let rows = raster_band(&input, &[]).expect("band");
    assert_eq!(rows.len(), 2);
    assert!(rows[1].iter().all(|(_, r)| r.cell_row == 1));
    let (_, w, h) = image_of(&rows);
    assert_eq!((w, h), (60 * CELL_W, 2 * CELL_H + lift));
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn rename_segment_is_left_uncovered_for_the_cell_well() {
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let metadata = plain(2);
    let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
    let titles = vec!["alpha".to_string(), "beta".to_string()];
    let paint = StripPaint {
        hovered: None,
        new_tab_hovered: false,
        version_hovered: false,
        caption_hovered: None,
        subtitle: None,
        rename: Some(StripRenameField {
            tab: 0,
            text: "alp",
            cursor: 3,
        }),
    };
    let input = band(&segments, &titles, &metadata, paint, geometry(80, 1));
    let rows = raster_band(&input, &[]).expect("band");
    let edited = segments[0];
    let covered: Vec<usize> = rows[0].iter().map(|(c, _)| *c).collect();
    for col in usize::from(edited.start_col)..usize::from(edited.end_col) {
        assert!(
            !covered.contains(&col),
            "col {col} belongs to the rename well"
        );
    }
    let other = segments[1];
    assert!(covered.contains(&usize::from(other.start_col + 1)));
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn uncoverable_title_falls_back_per_segment_and_keeps_its_icon_rasters() {
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let metadata = plain(2);
    let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
    let titles = vec!["\u{1F680} rocket".to_string(), "beta".to_string()];
    let input = band(
        &segments,
        &titles,
        &metadata,
        StripPaint::default(),
        geometry(80, 1),
    );
    let icon = Arc::new(ImageData {
        bytes: vec![0; 4],
        format: ImageFormat::RawRgba8 {
            width: 1,
            height: 1,
        },
        cols: 1,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    });
    let mk = |col: u16| {
        (
            usize::from(col),
            ImageRef {
                image: Arc::clone(&icon),
                cell_row: 0,
                cell_col: 0,
                kitty: None,
            },
        )
    };
    let kept_col = segments[0].start_col + 1;
    let dropped_col = segments[1].start_col + 1;
    let legacy = vec![mk(kept_col), mk(dropped_col)];
    let rows = raster_band(&input, &legacy).expect("band");
    let find = |col: u16| {
        rows[0]
            .iter()
            .find(|(c, _)| *c == usize::from(col))
            .map(|(_, r)| Arc::ptr_eq(&r.image, &icon))
    };
    assert_eq!(
        find(kept_col),
        Some(true),
        "fallback segment keeps its icon"
    );
    assert_eq!(
        find(dropped_col),
        Some(false),
        "pixel segment is band-covered"
    );
    let mut sorted = rows[0].iter().map(|(c, _)| *c).collect::<Vec<_>>();
    sorted.dedup();
    assert!(
        sorted.windows(2).all(|p| p[0] < p[1]),
        "refs stay sorted by column"
    );
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn a_cjk_title_stays_in_the_band_and_centres_with_its_latin_neighbour() {
    if !crate::tray_raster::install_chrome_fonts_with_cjk_chain_for_test() || !with_ui_faces() {
        crate::tray_raster::clear_ui_fonts_for_test();
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let title = "日本語 shell";
    assert!(
        crate::tray_raster::strip_band_run_coverable(title),
        "the chrome stack draws every char of the title"
    );
    let metadata = plain(2);
    let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
    let titles = vec!["alpha".to_string(), title.to_string()];
    let input = band(
        &segments,
        &titles,
        &metadata,
        StripPaint::default(),
        geometry(80, 1),
    );
    let rows = raster_band(&input, &[]).expect("band");
    let (rgba, w, h) = image_of(&rows);
    let seg = segments[1];
    let probe = usize::from(seg.start_col) + 1;
    assert!(
        rows[0]
            .iter()
            .any(|(c, r)| *c == probe && Arc::ptr_eq(&r.image, &rows[0][0].1.image)),
        "the CJK segment is band-covered, not a fallback range"
    );
    let layout = tab_content_layout(&seg, metadata[1]);
    let label_px = band_label_px(h as f32, 1.0);
    let measure = |s: &str| crate::tray_raster::ui_text_width_for(TextFace::Ui, s, label_px);
    let x0 = f32::from(layout.title_start) * CELL_W as f32;
    let span_px = f32::from(layout.title_end - layout.title_start) * CELL_W as f32 - 1.0;
    let x = label_x(x0, span_px, measure(title));
    let cjk_end = x + measure("日本語");
    let latin_start = x + measure("日本語 ");
    let colors = strip_colors_with_active(Theme::default(), None);
    let surfaces = [colors.band_bg, colors.active_bg, colors.chip_bg];
    let cjk = ink_bbox_off_surfaces(
        &rgba,
        w,
        h,
        x.floor() as usize,
        cjk_end.ceil() as usize,
        &surfaces,
    )
    .expect("the ideographs ink");
    let latin = ink_bbox_off_surfaces(
        &rgba,
        w,
        h,
        latin_start.floor() as usize,
        usize::from(layout.title_end) * CELL_W,
        &surfaces,
    )
    .expect("the Latin inks");
    let centre = |b: (usize, usize, usize, usize)| (b.1 + b.3) as f32 * 0.5;
    assert!(
        (centre(cjk) - centre(latin)).abs() <= 2.0,
        "CJK ink {cjk:?} centres at {} while the Latin's {latin:?} centres at {}",
        centre(cjk),
        centre(latin)
    );
    let d = BandDesign::resolve(&input.geometry, h, label_px);
    assert!(
        (cjk.3 as f32) <= d.card_bot - 1.0,
        "CJK ink {cjk:?} reaches the card's bottom edge {}",
        d.card_bot
    );
    assert!(
        (cjk.1 as f32) >= d.card_top,
        "CJK ink {cjk:?} escapes the card's top edge {}",
        d.card_top
    );
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn a_colour_emoji_title_is_a_band_tab() {
    if !STRIP_WINDOWS_PAINT {
        return;
    }
    let renderer =
        aterm_render::Renderer::from_bytes(aterm_render::embedded_font(), 14.0, Theme::default())
            .expect("embedded renderer");
    crate::tray_raster::install_settled_chrome_fonts_for_test(renderer);
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let label_px = band_label_px((BAND_TOP + CELL_H) as f32, 1.0);
    if crate::tray_raster::chrome_colour_emoji("\u{1F680}", emoji_box_px(label_px)).is_none() {
        crate::tray_raster::clear_ui_fonts_for_test();
        aterm_spec::host::could_not_run("no colour emoji face here for the chrome");
    }
    let title = "\u{1F680} deploy";
    assert!(
        !crate::tray_raster::strip_band_run_coverable(title),
        "CONTROL: the UI stack cannot draw the rocket"
    );
    let metadata = plain(3);
    let segments = layout_segments_with_metadata(90, 3, &metadata, 1, false);
    let titles = vec!["build".to_string(), title.to_string(), "logs".to_string()];
    let input = BandInput {
        active: 1,
        ..band(
            &segments,
            &titles,
            &metadata,
            StripPaint::default(),
            geometry(90, 1),
        )
    };
    let rows = raster_band(&input, &[]).expect("band");
    let covered = |col: u16| rows[0].iter().any(|(c, _)| *c == usize::from(col));
    let seg = segments[1];
    assert!(
        (seg.start_col..seg.end_col).all(covered),
        "the emoji tab's every column is the band's"
    );
    let (rgba, w, h) = image_of(&rows);
    let layout = tab_content_layout(&seg, metadata[1]);
    let colourful = (0..h)
        .flat_map(|y| {
            (usize::from(layout.title_start) * CELL_W..usize::from(layout.title_end) * CELL_W)
                .map(move |x| (x, y))
        })
        .filter(|&(x, y)| {
            let [r, g, b] = pixel(&rgba, w, x, y);
            r.max(g).max(b) - r.min(g).min(b) > 60
        })
        .count();
    assert!(
        colourful > 20,
        "the rocket is drawn in colour: {colourful} px"
    );
    let colors = strip_colors_for(input.palette);
    let inside = usize::from(seg.start_col) * CELL_W + 4;
    assert_eq!(
        pixel(&rgba, w, inside, h - 1),
        colors.active_bg,
        "the selected emoji tab is the tab, to the band's last row"
    );
    let bare =
        aterm_render::Renderer::from_bytes(aterm_render::embedded_font(), 14.0, Theme::default())
            .map(|mut renderer| {
                renderer.set_runtime_font_discovery(false);
                renderer
            })
            .expect("embedded renderer");
    crate::tray_raster::install_settled_chrome_fonts_for_test(bare);
    let rows = raster_band(&input, &[]).expect("band");
    if crate::tray_raster::chrome_colour_emoji("\u{1F680}", emoji_box_px(label_px)).is_none() {
        assert!(
            !rows[0]
                .iter()
                .any(|(c, _)| *c == usize::from(seg.start_col) + 1),
            "CONTROL: without a colour face the title is the cell lane's"
        );
    }
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn a_cjk_title_no_face_maps_falls_to_the_cell_lane() {
    let mut renderer =
        aterm_render::Renderer::from_bytes(aterm_render::embedded_font(), 14.0, Theme::default())
            .expect("embedded renderer");
    renderer.set_runtime_font_discovery(false);
    crate::tray_raster::install_settled_chrome_fonts_for_test(renderer);
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    crate::tray_raster::set_ui_cjk_faces_for_test(None);
    let title = "日本語 shell";
    assert!(
        !crate::tray_raster::strip_band_run_coverable(title),
        "no loaded face carries the ideographs yet"
    );
    let metadata = plain(2);
    let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
    let titles = vec!["alpha".to_string(), title.to_string()];
    let input = band(
        &segments,
        &titles,
        &metadata,
        StripPaint::default(),
        geometry(80, 1),
    );
    let rows = raster_band(&input, &[]).expect("band");
    let covered = |col: u16| rows[0].iter().any(|(c, _)| *c == usize::from(col));
    assert!(
        covered(segments[0].start_col + 1),
        "the Latin segment is band-covered"
    );
    assert!(
        !covered(segments[1].start_col + 1),
        "the CJK segment is left to the cell lane"
    );
    crate::tray_raster::clear_ui_fonts_for_test();
}

/// Each label's east asian faces are its own weight's.
#[cfg(windows)]
#[test]
fn each_labels_east_asian_faces_are_its_own_weights() {
    let Some(_live) = crate::tray_raster::install_unparsed_cjk_chain_for_test() else {
        aterm_spec::host::could_not_run("no CJK fallback chain could be installed here");
    };
    let Some(store) = crate::tray_raster::host_ui_cjk_faces_for_test() else {
        crate::tray_raster::clear_ui_fonts_for_test();
        aterm_spec::host::could_not_run("no host UI CJK faces here");
    };
    crate::tray_raster::set_ui_cjk_faces_for_test(Some(Arc::clone(&store)));
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let (geometry, _) = window_band(100);
    let metadata = plain(3);
    let segments = layout_segments_with_metadata(100, 3, &metadata, 0, false);
    let titles = vec![
        "日本語 notes".to_string(),
        "한국어".to_string(),
        "logs".to_string(),
    ];
    let input = band(
        &segments,
        &titles,
        &metadata,
        StripPaint::default(),
        geometry,
    );
    assert_eq!(input.selected(), 0, "the CJK tab is the selected one");
    assert!(
        store.parsed_files_for_test().is_empty(),
        "CONTROL: a cold store"
    );
    let rows = raster_band(&input, &[]).expect("band");
    assert_eq!(
        store.parsed_files_for_test(),
        ["malgun.ttf#0", "YuGothB.ttc#2"],
        "the quiet Hangul's Regular face and the selected ideographs' Semibold one"
    );
    for tab in [0, 1] {
        let seg = segments
            .iter()
            .find(|s| matches!(s.kind, TabHit::Select(i) if i == tab))
            .expect("the tab");
        assert!(
            rows[0]
                .iter()
                .any(|(c, _)| *c == usize::from(seg.start_col) + 1),
            "tab {tab} is a band title"
        );
    }
    crate::tray_raster::clear_ui_fonts_for_test();
}

fn window_band(cols: usize) -> (BandGeometry, usize) {
    let (band_top, cell_h, foot) = (15usize, 23usize, 6usize);
    let h = band_top + cell_h;
    let geometry = BandGeometry {
        cols,
        cell_w: CELL_W,
        cell_h,
        strip_rows: 1,
        band_top_px: band_top,
        scale: 1.0,
        seam_top_px: Some(h - strip_seam_px(1.0) - foot),
        foot_px: foot,
    };
    let label_px = band_label_px(h as f32, 1.0);
    let d = BandDesign::resolve(&geometry, h, label_px);
    (geometry, (d.baseline - label_px * 0.35).round() as usize)
}

#[test]
fn a_cjk_title_is_a_band_tab_on_its_first_frame_with_its_hairlines() {
    let Some(_live) = crate::tray_raster::install_unparsed_cjk_chain_for_test() else {
        aterm_spec::host::could_not_run("no CJK fallback chain could be installed here");
    };
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    assert!(
        !crate::tray_raster::chrome_ui_reaches_for_test(TextFace::Ui, '日'),
        "CONTROL: the CJK face is not parsed before the first frame"
    );
    let (geometry, cy) = window_band(100);
    let metadata = plain(4);
    let segments = layout_segments_with_metadata(100, 4, &metadata, 0, false);
    let titles = vec![
        "alpha".to_string(),
        "beta".to_string(),
        "日本語 notes".to_string(),
        "gamma".to_string(),
    ];
    let input = band(
        &segments,
        &titles,
        &metadata,
        StripPaint::default(),
        geometry,
    );
    let rows = raster_band(&input, &[]).expect("band");
    let cjk = segments
        .iter()
        .find(|s| matches!(s.kind, TabHit::Select(2)))
        .copied()
        .expect("the CJK tab");
    assert!(
        (cjk.start_col..cjk.end_col).all(|col| rows[0]
            .iter()
            .any(|(c, r)| *c == usize::from(col) && Arc::ptr_eq(&r.image, &rows[0][0].1.image))),
        "the CJK tab's every column is the band's on its first frame"
    );
    assert!(
        crate::tray_raster::chrome_ui_reaches_for_test(TextFace::Ui, '日'),
        "the band had the face its label needs parsed"
    );
    if SELECTED_TAB_IS_GROUND {
        let (rgba, w, _) = image_of(&rows);
        let seam = strip_colors_for_on(input.palette, input.floor)
            .seam
            .expect("a seam");
        for boundary in [cjk.start_col, cjk.end_col] {
            assert_eq!(
                pixel(&rgba, w, usize::from(boundary) * CELL_W, cy),
                seam,
                "a hairline at column {boundary}"
            );
        }
    }
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn quiet_neighbours_of_a_cell_lane_title_keep_their_hairlines() {
    if !SELECTED_TAB_IS_GROUND {
        return;
    }
    let mut renderer =
        aterm_render::Renderer::from_bytes(aterm_render::embedded_font(), 14.0, Theme::default())
            .expect("embedded renderer");
    renderer.set_runtime_font_discovery(false);
    crate::tray_raster::install_settled_chrome_fonts_for_test(renderer);
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let (geometry, cy) = window_band(100);
    let metadata = plain(4);
    let segments = layout_segments_with_metadata(100, 4, &metadata, 0, false);
    let titles = vec![
        "alpha".to_string(),
        "beta".to_string(),
        "logs \u{10FFFD}".to_string(),
        "gamma".to_string(),
    ];
    assert!(!crate::tray_raster::strip_band_run_coverable(&titles[2]));
    let input = band(
        &segments,
        &titles,
        &metadata,
        StripPaint::default(),
        geometry,
    );
    let rows = raster_band(&input, &[]).expect("band");
    let covered = |col: usize| rows[0].iter().any(|(c, _)| *c == col);
    let lane = segments
        .iter()
        .find(|s| matches!(s.kind, TabHit::Select(2)))
        .copied()
        .expect("the cell lane's tab");
    let (start, end) = (usize::from(lane.start_col), usize::from(lane.end_col));
    assert!(!covered(start) && !covered(end - 1), "the cell lane's tab");
    let (rgba, w, _) = image_of(&rows);
    let seam = strip_colors_for_on(input.palette, input.floor)
        .seam
        .expect("a seam");
    let before = start * CELL_W - 1;
    let after = end * CELL_W;
    for x in [before, after] {
        assert!(covered(x / CELL_W), "x={x} is a band column");
        assert_eq!(pixel(&rgba, w, x, cy), seam, "a hairline at x={x}");
    }
    assert_eq!(hairline_x(90.0, 2.0, true, true), Some(89.0));
    assert_eq!(hairline_x(90.0, 2.0, true, false), Some(88.0));
    assert_eq!(hairline_x(90.0, 2.0, false, true), Some(90.0));
    assert_eq!(hairline_x(90.0, 2.0, false, false), None);
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn active_label_the_variable_face_cannot_shape_draws_whole_through_the_cascade() {
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let Some(vf) = crate::tray_raster::strip_band_variable_semibold() else {
        crate::tray_raster::clear_ui_fonts_for_test();
        return;
    };
    let odd = [
        '\u{2713}', '\u{2717}', '\u{2423}', '\u{2318}', '\u{23CE}', '\u{2500}', '\u{2588}',
        '\u{2591}',
    ]
    .into_iter()
    .find(|&ch| !vf.cmap.maps(ch) && crate::tray_raster::strip_band_run_coverable(&ch.to_string()));
    let Some(odd) = odd else {
        crate::tray_raster::clear_ui_fonts_for_test();
        aterm_spec::host::could_not_run(
            "no char here that the cascade draws and the variable face lacks",
        );
    };
    let title = format!("Alpha{odd}Beta");
    assert!(shape_variable_label(&vf, &title, 13.0).is_none());
    assert!(crate::tray_raster::strip_band_run_coverable(&title));
    let metadata = plain(2);
    let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
    let titles = vec![title, "Beta".to_string()];
    let input = band(
        &segments,
        &titles,
        &metadata,
        StripPaint::default(),
        geometry(80, 1),
    );
    let rows = raster_band(&input, &[]).expect("band");
    let (rgba, w, h) = image_of(&rows);
    let layout = tab_content_layout(&segments[0], metadata[0]);
    let span = (
        usize::from(layout.title_start) * CELL_W,
        usize::from(layout.title_end) * CELL_W,
    );
    let (x0, _, x1, _) = ink_bbox(&rgba, w, h, span.0, span.1).expect("active ink");
    assert!(
        x0 >= span.0 && x1 <= span.1,
        "ink stays in span: {x0}..{x1} vs {span:?}"
    );
    let label_px = band_label_px((BAND_TOP + CELL_H) as f32, 1.0);
    let ink_w = (x1 - x0) as f32;
    let stem = crate::tray_raster::ui_text_width_for(TextFace::UiBold, "AlphaBeta", label_px);
    let cut = crate::tray_raster::ui_text_width_for(TextFace::UiBold, "Alpha…", label_px);
    assert!(
        ink_w + 2.0 >= stem,
        "ink {ink_w}px must cover the whole label (stem {stem}px)"
    );
    assert!(
        ink_w > cut + 4.0,
        "ink {ink_w}px must exceed the truncated `Alpha…` ({cut}px)"
    );
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn glyph_level_fit_matches_the_string_fit_glyph_for_glyph() {
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let Some(vf) = crate::tray_raster::strip_band_variable_semibold() else {
        crate::tray_raster::clear_ui_fonts_for_test();
        return;
    };
    let pen = VariablePen::new(&vf).expect("the retained bytes parse");
    let px = 13.0;
    for label in [
        "Windows Terminal Session Title",
        "Overflowing cell cut…",
        "W",
    ] {
        let full = pen.shape(label, px).expect("Latin shapes");
        let spans = [
            4.0,
            20.0,
            55.0,
            full.width - 0.5,
            full.width,
            full.width + 10.0,
        ];
        for span in spans {
            let expected_text = fit_label(label.to_string(), span, |s| {
                pen.shape(s, px).map_or(f32::INFINITY, |run| run.width)
            });
            let expected = pen
                .shape(&expected_text, px)
                .expect("the fitted string shapes");
            let actual = pen
                .fit(pen.shape(label, px).expect("shapes"), span, px)
                .expect("fits");
            let actual_text: String = actual.cuts.iter().map(|cut| cut.ch).collect();
            assert_eq!(actual_text, expected_text, "{label:?} at {span}px");
            assert_eq!(
                actual.glyphs.len(),
                expected.glyphs.len(),
                "{label:?} at {span}px"
            );
            for (a, e) in actual.glyphs.iter().zip(&expected.glyphs) {
                assert_eq!(
                    (a.x(), a.w, a.h, a.ymin),
                    (e.x(), e.w, e.h, e.ymin),
                    "{label:?} at {span}px"
                );
                assert_eq!(a.cov, e.cov, "{label:?} at {span}px");
            }
            assert!(
                (actual.width - expected.width).abs() < 1e-3,
                "{label:?} at {span}px: {} vs {}",
                actual.width,
                expected.width
            );
            assert!(
                actual.width <= span || expected_text == "…",
                "{label:?} at {span}px: fitted run {}px overflows",
                actual.width
            );
        }
    }
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn a_tabs_want_seats_its_title_and_ignores_a_spinner() {
    if !STRIP_WINDOWS_PAINT {
        return;
    }
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let band_h = BAND_TOP + CELL_H;
    let title = "Claude Code: fix the strip floor under splits".to_string();
    let spun: Vec<String> = ["\u{2733}", "\u{2736}", "\u{b7}", "\u{280b}"]
        .iter()
        .map(|frame| format!("{frame} Fix the strip floor under splits"))
        .filter(|title| crate::tray_raster::strip_band_run_coverable(title))
        .collect();
    assert!(spun.len() >= 2, "CONTROL: two frames to compare");
    for frame in ["\u{2733}", "\u{2736}", "\u{b7}", "\u{280b}", "*"] {
        assert_eq!(
            steady_title(&format!("{frame} Fix the strip")),
            "M Fix the strip",
            "the {frame:?} frame"
        );
    }
    for plain in ["logs", "~/src/aterm", "C:\\Users", "1 build"] {
        assert_eq!(steady_title(plain), plain);
    }
    let wants = tab_wants(&spun, &plain(spun.len()), CELL_W, band_h, 1.0);
    assert!(
        wants.windows(2).all(|pair| pair[0] == pair[1]),
        "a spinner's frames move the tab: {wants:?}"
    );
    let short = tab_wants(&["logs".to_string()], &plain(1), CELL_W, band_h, 1.0);
    assert!(short[0] <= MAX_CHIP_COLS, "a short title wants {short:?}");
    let want = tab_wants(std::slice::from_ref(&title), &plain(1), CELL_W, band_h, 1.0)[0];
    assert!(want > MAX_CHIP_COLS, "CONTROL: the title is long: {want}");
    let label = |sizing: TabSizing<'_>| {
        let metadata = plain(1);
        let titles = vec![title.clone()];
        let segments = layout_segments_with_version_sized(
            120,
            1,
            &metadata,
            0,
            false,
            VersionSlotCols::NONE,
            sizing,
        );
        let input = band(
            &segments,
            &titles,
            &metadata,
            StripPaint::default(),
            geometry(120, 1),
        );
        let label_px = band_label_px(band_h as f32, 1.0);
        resolve_band_labels(&input, 0, 120, CELL_W as f32, label_px, true)[0]
            .clone()
            .expect("a label")
    };
    let wants = [want];
    assert_eq!(
        label(TabSizing {
            cap: MAX_CHIP_COLS,
            grow: 48,
            wants: &wants
        }),
        title,
        "the grown tab seats the whole title"
    );
    assert_ne!(label(TabSizing::FIXED), title, "CONTROL: 27 columns cut it");
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn the_selected_title_keeps_its_weight_on_light_and_dark_tabs() {
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    if crate::tray_raster::strip_band_variable_semibold().is_none() {
        crate::tray_raster::clear_ui_fonts_for_test();
        return;
    }
    let luma =
        |c: [u8; 3]| 0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2]);
    let mean_coverage = |theme: Theme| {
        let metadata = plain(2);
        let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
        let titles = vec!["~\\aterm-winperfect".to_string(), "logs".to_string()];
        let input = BandInput {
            palette: theme.into(),
            ..band(
                &segments,
                &titles,
                &metadata,
                StripPaint::default(),
                geometry(80, 1),
            )
        };
        let colors = strip_colors_for(input.palette);
        let rows = raster_band(&input, &[]).expect("band");
        let (rgba, w, h) = image_of(&rows);
        let layout = tab_content_layout(&segments[0], metadata[0]);
        let (ground, ink) = (luma(colors.active_bg), luma(colors.active_fg));
        let d = BandDesign::resolve(&geometry(80, 1), h, band_label_px(h as f32, 1.0));
        let rim = if colors.active_rim.is_some() {
            strip_seam_px(1.0)
        } else {
            0
        };
        let (mut sum, mut n) = (0.0f32, 0usize);
        for y in (d.card_top.ceil() as usize + rim)..(d.card_bot.floor() as usize).min(h) {
            for x in
                usize::from(layout.title_start) * CELL_W..usize::from(layout.title_end) * CELL_W
            {
                let px = pixel(&rgba, w, x, y);
                let t = (luma(px) - ground) / (ink - ground);
                if t > 0.02 {
                    sum += t.min(1.0);
                    n += 1;
                }
            }
        }
        assert!(n > 50, "the selected title inked {n} px");
        sum / n as f32
    };
    let dark = mean_coverage(Theme::default());
    let light = mean_coverage(Theme::from(
        &aterm_types::scheme::builtin("Default Light").expect("builtin"),
    ));
    assert!(
        (dark - light).abs() <= 0.05 * dark.max(light),
        "the selected title's weight differs by theme: dark {dark:.3}, light {light:.3}"
    );
    crate::tray_raster::clear_ui_fonts_for_test();
}

fn window(seg: &TabSegment) -> (u16, f32) {
    let layout = band_content_layout(seg, None, seg.end_col);
    let avail = layout.title_end.saturating_sub(layout.title_start);
    (avail, f32::from(avail) * CELL_W as f32 - 1.0)
}

fn wide_face(_tab: usize, s: &str) -> f32 {
    s.chars().count() as f32 * (CELL_W as f32 * 1.5)
}

#[test]
fn the_pixel_fit_cannot_undo_the_cell_pass_distinctness() {
    let distinct: Vec<String> = ["alpha", "beta", "gamma", "delta"]
        .iter()
        .map(|leaf| format!("service-{leaf}"))
        .collect();
    let identical: Vec<String> = (0..10)
        .map(|_| "user@m17-tower: ~/aterm · Typing a command".to_string())
        .collect();
    for (case, titles, widths) in [
        ("distinct", distinct, &[80u16][..]),
        ("identical", identical, &[80, 130, 200][..]),
    ] {
        let metadata = plain(titles.len());
        for &cols in widths {
            let at = format!("{case} at {cols} cols");
            let segments = layout_segments_with_metadata(cols, titles.len(), &metadata, 0, false);
            let cell = distinct_chip_labels(&segments, &titles, Some(&metadata), 0, None);
            let entries: Vec<BandLabel> = segments
                .iter()
                .filter_map(|seg| match seg.kind {
                    TabHit::Select(i) => Some(BandLabel {
                        tab: i,
                        span_px: window(seg).1,
                        text: cell[i].clone().expect("a seated chip is labelled"),
                    }),
                    _ => None,
                })
                .collect();
            let naive: Vec<String> = entries
                .iter()
                .map(|entry| {
                    fit_label(entry.text.clone(), entry.span_px, |s| {
                        wide_face(entry.tab, s)
                    })
                })
                .collect();
            if case == "distinct" {
                assert!(
                    naive
                        .iter()
                        .enumerate()
                        .any(|(a, x)| { naive.iter().enumerate().any(|(b, y)| a != b && x == y) }),
                    "fixture: the naive per-chip fit must collide: {naive:?}"
                );
            }
            let fitted = fit_labels_distinctly(&entries, 0, &[], &wide_face);
            let is_distinct = case == "distinct";
            if is_distinct {
                assert_eq!(fitted.len(), titles.len(), "{at}: every chip resolved");
                assert_eq!(fitted[0].1, naive[0], "{at}: the ACTIVE chip is re-cut");
            }
            for (n, (tab, text)) in fitted.iter().enumerate() {
                assert!(!is_distinct || !text.is_empty(), "{at}: tab {tab} is mute");
                for (m, (_, other)) in fitted.iter().enumerate() {
                    assert!(
                        n == m || (!is_distinct && text.is_empty()) || text != other,
                        "{at}: two chips paint {text:?}"
                    );
                }
                if *tab != 0 && !text.is_empty() {
                    let position = (tab + 1).to_string();
                    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
                    assert!(
                        if is_distinct {
                            *text == naive[n] || text.starts_with(&position)
                        } else {
                            digits == position || label_says_nothing(text)
                        },
                        "{at}: tab {tab} paints {text:?}"
                    );
                }
                assert!(
                    wide_face(*tab, text) <= entries[n].span_px || text == "…",
                    "{at}: {text:?} would need a SECOND fit at paint time"
                );
            }
        }
    }
}

#[test]
fn the_pixel_fit_repairs_every_label_that_names_nothing_or_another_tab() {
    let squeeze = wide_face(0, "……");
    assert!(label_says_nothing(&fit_label(
        "…-676-2".to_string(),
        squeeze,
        |s| wide_face(0, s)
    )));
    assert_eq!(
        fit_label("…and".to_string(), squeeze, |s| wide_face(1, s)),
        "…",
        "a tail cut keeps its end down to three characters, then the mark"
    );
    assert_eq!(
        fit_label(
            "…\\work\\sub dir".to_string(),
            wide_face(0, "…sub dir"),
            |s| { wide_face(0, s) }
        ),
        "…sub dir",
        "a tail cut sheds from the front, keeping what tells it apart"
    );
    assert_eq!(
        fit_label(
            "2 · …\\work\\sub dir".to_string(),
            wide_face(0, "2 · …sub dir"),
            |s| { wide_face(0, s) }
        ),
        "2 · …sub dir",
        "behind its ordinal too"
    );
    assert_eq!(
        fit_label(
            "Thinking… (esc to interrupt)".to_string(),
            wide_face(0, "Thinking… (esc…"),
            |s| wide_face(0, s)
        ),
        "Thinking… (esc…",
        "a title's own `…` is no cut: it keeps its head"
    );
    let mono = |_tab: usize, s: &str| s.chars().count() as f32 * CELL_W as f32;
    let twin_span = mono(0, "13 · ~\\aterm") + 1.0;
    let distinct = |case: &str, fitted: &[(usize, String)]| {
        for (n, (tab, text)) in fitted.iter().enumerate() {
            for (m, (_, other)) in fitted.iter().enumerate() {
                assert!(
                    n == m || text != other,
                    "{case}: tab {tab} doubles: {fitted:?}"
                );
            }
        }
    };
    type Check<'a> = &'a dyn Fn(&str, &[(usize, String)], &[BandLabel]);
    type Case<'a> = (
        &'a str,
        &'a dyn Fn(usize, &str) -> f32,
        Vec<(usize, f32, &'a str)>,
        usize,
        &'a [&'a str],
        Check<'a>,
    );
    let cases: [Case; 7] = [
        (
            "stranded selection",
            &wide_face,
            vec![(0, squeeze, "…-676-2"), (1, squeeze, "…-676-2")],
            0,
            &[],
            &|case, fitted, entries| {
                assert_eq!(fitted[0].1, "1", "{case}: {fitted:?}");
                distinct(case, fitted);
                for ((tab, text), entry) in fitted.iter().zip(entries) {
                    assert!(wide_face(*tab, text) <= entry.span_px, "{case}: {text:?}");
                }
            },
        ),
        (
            "no room for the number",
            &wide_face,
            vec![(9, wide_face(9, "1") * 0.5, "…-676-2")],
            9,
            &[],
            &|case, fitted, _| assert!(label_says_nothing(&fitted[0].1), "{case}: {fitted:?}"),
        ),
        (
            "stub of marks",
            &wide_face,
            vec![
                (0, 200.0, "…alpha"),
                (1, squeeze, "…and"),
                (2, 200.0, "…gamma"),
            ],
            0,
            &[],
            &|case, fitted, entries| {
                assert_eq!(fitted[1].1, "2", "{case}: {fitted:?}");
                for ((tab, text), entry) in fitted.iter().zip(entries) {
                    assert!(
                        !label_says_nothing(text)
                            || wide_face(*tab, &(tab + 1).to_string()) > entry.span_px,
                        "{case}: tab {tab} paints {text:?} where its number fits"
                    );
                }
            },
        ),
        (
            "a title that reads as a number",
            &wide_face,
            vec![(0, 200.0, "…zulu"), (1, 200.0, "3"), (2, squeeze, "…and")],
            0,
            &[],
            &|case, fitted, _| {
                distinct(case, fitted);
                assert_eq!(fitted[2].1, "3", "{case}: {fitted:?}");
                assert!(fitted[1].1.starts_with('2'), "{case}: {fitted:?}");
            },
        ),
        (
            "a cell-painter fallback",
            &wide_face,
            vec![(1, 200.0, "…~/aterm"), (2, 200.0, "…$HOME/trust")],
            0,
            &["…~/aterm"],
            &|case, fitted, _| {
                assert!(fitted[0].1.starts_with('2'), "{case}: {fitted:?}");
                assert_eq!(fitted[1].1, "…$HOME/trust", "{case}: untouched");
            },
        ),
        (
            "quiet twins",
            &mono,
            vec![
                (5, twin_span, "…gui\\src"),
                (8, twin_span, "~\\aterm"),
                (11, twin_span, "~\\aterm"),
                (12, twin_span, "~\\aterm"),
                (13, twin_span, "C:\\Temp"),
            ],
            5,
            &[],
            &|case, fitted, _| {
                let labels: Vec<&str> = fitted.iter().map(|(_, t)| t.as_str()).collect();
                assert_eq!(
                    labels,
                    [
                        "…gui\\src",
                        "9 · ~\\aterm",
                        "12 · ~\\aterm",
                        "13 · ~\\aterm",
                        "C:\\Temp"
                    ],
                    "{case}"
                );
            },
        ),
        (
            "an uncontested name",
            &mono,
            vec![(5, twin_span, "…gui\\src"), (8, twin_span, "~\\aterm")],
            5,
            &[],
            &|case, fitted, _| assert_eq!(fitted[1].1, "~\\aterm", "{case}"),
        ),
    ];
    for (case, face, entries, active, fallbacks, check) in cases {
        let entries: Vec<BandLabel> = entries
            .into_iter()
            .map(|(tab, span_px, text)| BandLabel {
                tab,
                span_px,
                text: text.to_string(),
            })
            .collect();
        let fitted = fit_labels_distinctly(&entries, active, fallbacks, face);
        check(case, &fitted, &entries);
    }
}

#[test]
fn chip_titles_are_cut_for_the_ui_face_and_paths_keep_their_end() {
    if !with_ui_faces() {
        crate::tray_raster::clear_ui_fonts_for_test();
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let label_px = 13.0;
    if !STRIP_WINDOWS_PAINT {
        assert_eq!(
            ui_cells_per_cell(CELL_W as f32, label_px),
            1.0,
            "Linux keeps cutting titles in terminal cells"
        );
        crate::tray_raster::clear_ui_fonts_for_test();
        return;
    }
    assert!(
        ui_cells_per_cell(CELL_W as f32, label_px) > 1.2,
        "the UI face sets more than a character per 9 px cell"
    );
    let titles = vec!["Settings".to_string(), "x".to_string()];
    let metadata = plain(2);
    let segments = layout_segments_with_metadata(27, 2, &metadata, 0, false);
    let input = band(
        &segments,
        &titles,
        &metadata,
        StripPaint::default(),
        geometry(27, 1),
    );
    let resolved = resolve_band_labels(&input, 0, 27, CELL_W as f32, label_px, true);
    assert_eq!(resolved[0].as_deref(), Some("Settings"), "{resolved:?}");

    let dir = r"C:\Users\m6-an\AppData\Local\Temp\wp-chrome2\work\sub dir".to_string();
    let titles = vec![dir.clone(), dir];
    let segments = layout_segments_with_metadata(100, 2, &metadata, 0, false);
    let input = band(
        &segments,
        &titles,
        &metadata,
        StripPaint::default(),
        geometry(100, 1),
    );
    let resolved = resolve_band_labels(&input, 0, 100, CELL_W as f32, label_px, true);
    for (i, label) in resolved.iter().enumerate() {
        let label = label.as_deref().expect("a painted chip has a label");
        assert!(label.ends_with(r"\sub dir"), "tab {i}: {label:?}");
    }
    assert!(
        resolved[1]
            .as_deref()
            .is_some_and(|l| l.starts_with("2 · ")),
        "the quiet twin is numbered: {resolved:?}"
    );
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn each_tab_is_cut_at_its_own_widest_step() {
    let wide = "W".repeat(40);
    let titles = [wide.as_str(), "Claude Code: reviewing the strip"];
    let labels_at = |widen: f32| -> Vec<Option<String>> {
        let keep = (20.0 * widen + 0.001).floor() as usize;
        titles
            .iter()
            .map(|title| {
                Some(if title.chars().count() <= keep {
                    (*title).to_string()
                } else {
                    title.chars().take(keep - 1).collect::<String>() + "…"
                })
            })
            .collect()
    };
    let width = |s: &str| {
        s.chars()
            .map(|c| if c == 'W' { 14.0 } else { 7.0 })
            .sum::<f32>()
    };
    let overflows = |_: usize, s: &str, span: f32| width(s) > span;
    let cut = widest_fitting_cuts(&[(0, 200.0), (1, 250.0)], 1.5, labels_at, overflows);
    assert_eq!(cut.len(), 2);
    let floor = format!("{}…", "W".repeat(19));
    assert_eq!(
        cut[0].as_deref(),
        Some(floor.as_str()),
        "the floor: {cut:?}"
    );
    assert_eq!(
        cut[1].as_deref(),
        Some("Claude Code: reviewing the st…"),
        "the fitting tab keeps its own step"
    );
    assert_eq!(
        labels_at(1.0)[1].as_deref(),
        Some("Claude Code: review…"),
        "CONTROL"
    );
    let middle = widest_fitting_cuts(&[(1, 180.0)], 1.5, labels_at, overflows);
    assert_eq!(
        middle[1].as_deref(),
        Some("Claude Code: reviewing …"),
        "{middle:?}"
    );
    assert_eq!(middle[0], None);
}

#[test]
fn a_neighbours_title_costs_a_tab_no_characters() {
    if !STRIP_WINDOWS_PAINT {
        crate::tray_raster::clear_ui_fonts_for_test();
        return;
    }
    if !with_ui_faces() {
        crate::tray_raster::clear_ui_fonts_for_test();
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let claude = "Claude Code: reviewing platform_win.rs and the Default Light contrast of bright white on blue";
    let (cw, label_px) = (17.0, 26.0);
    let metadata = plain(5);
    let label_of = |first: &str, active: usize| {
        let titles: Vec<String> = [first, "build", claude, "wsl ~/src", "logs"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let segments = layout_segments_with_metadata(100, 5, &metadata, active, false);
        let input = band(
            &segments,
            &titles,
            &metadata,
            StripPaint::default(),
            geometry(100, 1),
        );
        resolve_band_labels(&input, active, 100, cw, label_px, true)[2]
            .clone()
            .expect("the Claude tab is labelled")
    };
    for active in [0, 2] {
        let beside_logs = label_of("logs-old", active);
        let beside_path = label_of(r"C:\Users\m6-an\aterm", active);
        assert_eq!(beside_path, beside_logs, "active {active}");
        assert!(
            beside_logs.starts_with("Claude Code: revi"),
            "{beside_logs:?}"
        );
    }
    assert_eq!(
        label_of(r"C:\Users\m6-an\aterm", 0),
        label_of(r"C:\Users\m6-an\aterm", 2)
    );
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn the_band_resolves_one_label_per_painted_chip() {
    let titles: Vec<String> = ["alpha", "beta", "gamma"]
        .iter()
        .map(|leaf| format!("user@m17-tower: ~/work/service-{leaf}"))
        .collect();
    let metadata = plain(titles.len());
    let segments = layout_segments_with_metadata(80, titles.len(), &metadata, 0, false);
    let paint = StripPaint {
        rename: Some(StripRenameField {
            tab: 2,
            text: "renaming",
            cursor: 0,
        }),
        ..StripPaint::default()
    };
    let input = band(&segments, &titles, &metadata, paint, geometry(80, 1));
    let resolved = resolve_band_labels(&input, input.selected(), 80, CELL_W as f32, 13.0, true);
    assert_eq!(resolved.len(), titles.len());
    assert!(
        resolved[2].is_none(),
        "the rename well is the cell painter's: {resolved:?}"
    );
    for (i, label) in resolved.iter().enumerate().take(2) {
        let label = label.as_deref().expect("a painted chip has a label");
        assert!(!label.is_empty(), "tab {i} says nothing");
        for (j, other) in resolved.iter().enumerate().take(2) {
            assert!(
                i == j || Some(label) != other.as_deref(),
                "two chips resolved to {label:?}"
            );
        }
    }
}

#[test]
fn the_pixel_fit_keeps_a_command_cuts_file() {
    let measure = |s: &str| s.chars().count() as f32 * 7.0;
    let label = format!("vim -n -R{MIDDLE_MARK}src/platform_win.rs");
    assert_eq!(fit_label(label.clone(), 300.0, measure), label);
    for (span, want) in [
        (210.0, "vim -n - … src/platform_win.rs"),
        (180.0, "vim … src/platform_win.rs"),
        (140.0, "…src/platform_win.rs"),
        (100.0, "…atform_win.rs"),
    ] {
        let fitted = fit_label(label.clone(), span, measure);
        assert_eq!(fitted, want, "{span} px");
        assert!(measure(&fitted) <= span, "{fitted:?} in {span} px");
    }
    for (label, keeps) in [
        ("vim -n … src/x.rs", true),
        ("…src/x.rs", true),
        ("2 · …src/x.rs", true),
        ("vim -n -R -i NO…", false),
        ("Thinking… (esc to interrupt)", false),
        ("logs", false),
        ("…", false),
    ] {
        assert_eq!(cut_keeps_its_end(label), keeps, "{label:?}");
    }
}

const WIN_BANDS: [(usize, usize, usize); 4] =
    [(19, 13, 10), (17, 15, 12), (13, 19, 15), (9, 23, 18)];

fn band_seam_top((band_top, cell_h, underline_y): (usize, usize, usize)) -> usize {
    if STRIP_WINDOWS_PAINT {
        band_top + cell_h - strip_seam_px(1.0)
    } else {
        band_top + underline_y
    }
}

fn win_geometry(cols: usize, win: (usize, usize, usize)) -> BandGeometry {
    let (band_top, cell_h, _) = win;
    BandGeometry {
        cols,
        cell_w: CELL_W,
        cell_h,
        strip_rows: 1,
        band_top_px: band_top,
        scale: 1.0,
        seam_top_px: Some(band_seam_top(win)),
        foot_px: 0,
    }
}

#[test]
fn the_label_stays_in_its_card_with_descender_room_at_every_font_px() {
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let colors = strip_colors_with_active(Theme::default(), None);
    for win in WIN_BANDS {
        let (band_top, cell_h, _) = win;
        let seam_top = band_seam_top(win);
        let metadata = plain(2);
        let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
        let titles = vec!["Apply".to_string(), "gypsy".to_string()];
        let geometry = win_geometry(80, win);
        let input = band(
            &segments,
            &titles,
            &metadata,
            StripPaint::default(),
            geometry,
        );
        let rows = raster_band(&input, &[]).expect("band");
        let (rgba, w, h) = image_of(&rows);
        assert_eq!(
            h,
            band_top + cell_h,
            "the canvas is the WHOLE optical band, lip included"
        );
        let seg = segments[0];
        let layout = tab_content_layout(&seg, metadata[0]);
        let span = (
            usize::from(layout.title_start) * CELL_W,
            usize::from(layout.title_end) * CELL_W,
        );
        let (_, y0, _, y1) = ink_bbox_off_surfaces(
            &rgba,
            w,
            h,
            span.0,
            span.1,
            &[colors.band_bg, colors.active_bg, colors.chip_bg],
        )
        .expect("the active title inks its span");
        let label_px = band_label_px(h as f32, 1.0);
        let d = BandDesign::resolve(&geometry, h, label_px);
        assert!(
            y0 as f32 >= d.card_top - 1.0 && y1 as f32 <= d.card_bot + 1.0,
            "{win:?}: ink {y0}..{y1} escapes its card {}..{}",
            d.card_top,
            d.card_bot
        );
        assert!(
            y1 < seam_top,
            "{win:?}: descenders reach {y1}, seam rule at {seam_top}"
        );
        if d.tabs {
            assert_eq!(d.card_bot, seam_top as f32, "{win:?}: on the seam");
            let (above, below) = (y0 as f32 - d.card_top, d.card_bot - 1.0 - y1 as f32);
            assert!(
                above >= 1.0 && below >= 1.0 && (above - below).abs() <= 3.0,
                "{win:?}: ink {y0}..={y1} in tab {}..{}: {above} above, {below} below",
                d.card_top,
                d.card_bot
            );
        } else {
            let above = d.card_top;
            let below = seam_top as f32 - d.card_bot;
            assert!(
                (above - below).abs() <= 1.0,
                "{win:?}: card {}..{} is not centred over the seam at {seam_top}",
                d.card_top,
                d.card_bot
            );
        }
        let seam = colors.seam.expect("a chrome band has a seam");
        for x in [0, w / 2, w - 1] {
            let at = (seam_top * w + x) * 4;
            assert_eq!(
                &rgba[at..at + 3] == seam.as_slice(),
                STRIP_WINDOWS_PAINT,
                "{win:?}: seam at x={x}"
            );
        }
    }
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn a_selected_tab_opens_into_the_grid_and_quiet_tabs_are_bare() {
    if !SELECTED_TAB_IS_GROUND {
        return;
    }
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let (band_top, cell_h, foot) = (15usize, 23usize, 6usize);
    let h = band_top + cell_h;
    let seam_top = h - strip_seam_px(1.0) - foot;
    let geometry = BandGeometry {
        cols: 90,
        cell_w: CELL_W,
        cell_h,
        strip_rows: 1,
        band_top_px: band_top,
        scale: 1.0,
        seam_top_px: Some(seam_top),
        foot_px: foot,
    };
    for theme in [
        Theme::default(),
        Theme::from(&aterm_types::scheme::builtin("Solarized Light").expect("builtin")),
    ] {
        let colors = strip_colors_with_active(theme, None);
        let seam = colors.seam.expect("a seam");
        let metadata = plain(3);
        let segments = layout_segments_with_metadata(90, 3, &metadata, 0, false);
        let titles = vec!["Apply".to_string(), "gypsy".to_string(), "logs".to_string()];
        let input = band(
            &segments,
            &titles,
            &metadata,
            StripPaint::default(),
            geometry,
        );
        let input = BandInput {
            palette: theme.into(),
            ..input
        };
        let rows = raster_band(&input, &[]).expect("band");
        let (rgba, w, img_h) = image_of(&rows);
        assert_eq!(img_h, h);
        let at = |x: usize, y: usize| pixel(&rgba, w, x, y);
        let mid = |i: usize| {
            let s = segments[i];
            (
                usize::from(s.start_col) * CELL_W + 4,
                usize::from(s.end_col) * CELL_W,
            )
        };
        let (active_x, _) = mid(0);
        let (quiet_x, quiet_end) = mid(1);
        for y in [seam_top, seam_top + 1, h - 1] {
            assert_eq!(at(active_x, y), colors.foot, "the selected tab at y={y}");
        }
        assert_eq!(
            colors.active_bg, colors.foot,
            "the selected tab IS the ground"
        );
        assert_eq!(at(quiet_x, seam_top), seam, "the seam under a quiet tab");
        assert_eq!(at(quiet_x, h - 1), colors.foot, "the foot");
        assert_eq!(
            at(quiet_x, seam_top - 3),
            colors.band_bg,
            "a quiet tab has no card"
        );
        let d = BandDesign::resolve(&geometry, h, band_label_px(h as f32, 1.0));
        let cy = (d.baseline - band_label_px(h as f32, 1.0) * 0.35).round() as usize;
        assert_eq!(at(quiet_end, cy), seam, "a hairline between quiet tabs");
        let (_, active_end) = mid(0);
        assert_eq!(
            at(active_end, cy),
            colors.band_bg,
            "none beside the selection"
        );
        let luma = |c: [u8; 3]| crate::chrome_band::contrast(c, [0, 0, 0]);
        if bg_is_light(colors.foot) {
            assert!(
                luma(colors.active_bg) > luma(colors.band_bg)
                    && luma(colors.hover_bg) < luma(colors.band_bg),
                "light: the selected tab rises toward white, the hover toward the ink"
            );
        }
    }
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn the_selected_tab_joins_what_lies_under_the_seam() {
    if !SELECTED_TAB_IS_GROUND {
        return;
    }
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let (band_top, cell_h, foot) = (15usize, 23usize, 6usize);
    let h = band_top + cell_h;
    let seam_top = h - strip_seam_px(1.0) - foot;
    let geometry = BandGeometry {
        cols: 90,
        cell_w: CELL_W,
        cell_h,
        strip_rows: 1,
        band_top_px: band_top,
        scale: 1.0,
        seam_top_px: Some(seam_top),
        foot_px: foot,
    };
    for theme in [
        Theme::default(),
        Theme::from(&aterm_types::scheme::builtin("Default Light").expect("builtin")),
    ] {
        let ground = strip_colors(theme).foot;
        let band_bg = strip_colors(theme).band_bg;
        let page = crate::settings::Roles::from_theme(theme).surface;
        assert_ne!(page, ground, "CONTROL: the page is not the ground");
        let metadata = plain(3);
        let segments = layout_segments_with_metadata(90, 3, &metadata, 0, false);
        let titles = vec![
            "Settings".to_string(),
            "build".to_string(),
            "logs".to_string(),
        ];
        let paint_over = |floor: StripFloor| {
            let input = BandInput {
                palette: theme.into(),
                floor,
                ..band(
                    &segments,
                    &titles,
                    &metadata,
                    StripPaint::default(),
                    geometry,
                )
            };
            let rows = raster_band(&input, &[]).expect("band");
            image_of(&rows)
        };
        let active_x = usize::from(segments[0].start_col) * CELL_W + 4;
        let quiet_x = usize::from(segments[1].start_col) * CELL_W + 4;
        let seam = strip_colors(theme).seam.expect("a seam");
        let inside = seam_top - 3;
        for (floor, tab, opens, under) in [
            (StripFloor::Ground, ground, true, ground),
            (StripFloor::Surface(ground), ground, true, ground),
            (StripFloor::Surface(page), page, true, page),
            (StripFloor::Surface(band_bg), ground, false, band_bg),
        ] {
            let (rgba, w, _) = paint_over(floor);
            let at = |x: usize, y: usize| pixel(&rgba, w, x, y);
            assert_eq!(at(active_x, inside), tab, "{floor:?}: the selected tab");
            let at_seam = if opens { tab } else { seam };
            assert_eq!(at(active_x, seam_top), at_seam, "{floor:?}: under the tab");
            assert_eq!(
                at(active_x, h - 1),
                under,
                "{floor:?}: the foot under the tab"
            );
            assert_eq!(at(quiet_x, seam_top), seam, "{floor:?}: the seam elsewhere");
            assert_eq!(at(quiet_x, h - 1), under, "{floor:?}: the foot elsewhere");
        }
    }
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn a_selected_tab_the_band_would_swallow_wears_the_seam() {
    if !SELECTED_TAB_IS_GROUND {
        return;
    }
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let light = Theme::from(&aterm_types::scheme::builtin("Default Light").expect("builtin"));
    for (theme, page, rimmed) in [
        (light, true, true),
        (light, false, true),
        (Theme::default(), true, false),
        (Theme::default(), false, false),
    ] {
        let floor = if page {
            StripFloor::Surface(crate::settings::Roles::from_theme(theme).surface)
        } else {
            StripFloor::Ground
        };
        let colors = strip_colors_for_on(theme, floor);
        assert_eq!(
            colors.active_rim.is_some(),
            rimmed,
            "page={page} {:?}: {:.2}:1",
            colors.active_bg,
            crate::chrome_band::contrast(colors.active_bg, colors.band_bg)
        );
        for scale in [1usize, 2] {
            let (band_top, cell_h, foot) = (15 * scale, 23 * scale, 6 * scale);
            let cell_w = CELL_W * scale;
            let h = band_top + cell_h;
            let seam_px = strip_seam_px(scale as f32);
            let seam_top = h - seam_px - foot;
            let geometry = BandGeometry {
                cols: 90,
                cell_w,
                cell_h,
                strip_rows: 1,
                band_top_px: band_top,
                scale: scale as f32,
                seam_top_px: Some(seam_top),
                foot_px: foot,
            };
            let metadata = plain(3);
            let segments = layout_segments_with_metadata(90, 3, &metadata, 0, false);
            let titles = vec![
                "Settings".to_string(),
                "build".to_string(),
                "logs".to_string(),
            ];
            let input = BandInput {
                palette: theme.into(),
                floor,
                ..band(
                    &segments,
                    &titles,
                    &metadata,
                    StripPaint::default(),
                    geometry,
                )
            };
            let rows = raster_band(&input, &[]).expect("band");
            let (rgba, w, _) = image_of(&rows);
            let at = |x: usize, y: usize| pixel(&rgba, w, x, y);
            let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 2);
            let d = BandDesign::resolve(&geometry, h, band_label_px(h as f32, scale as f32));
            let x0 = usize::from(segments[0].start_col) * cell_w + d.gap_h as usize;
            let x1 = usize::from(segments[0].end_col) * cell_w - d.gap_h as usize;
            let seam = colors.seam.expect("a seam");
            let tab = colors.active_bg;
            let inside = seam_top - 3 * scale;
            let case = format!("page={page} {scale}x");
            let mid_x = (x0 + x1) / 2;
            let top = d.card_top as usize;
            let is = |x: usize, y: usize, want: [u8; 3], what: &str| {
                let got = at(x, y);
                assert!(
                    near(got, want),
                    "{case}: {what} at ({x}, {y}): {got:?}, not {want:?}"
                );
            };
            if rimmed {
                for k in 0..seam_px {
                    for y in [inside, seam_top, seam_top + seam_px - 1] {
                        is(x0 + k, y, seam, "the left side");
                        is(x1 - 1 - k, y, seam, "the right side");
                    }
                    is(mid_x, top + k, seam, "the top");
                }
                is(x0 + seam_px, inside, tab, "inside the rim");
                is(mid_x, top + seam_px, tab, "under the top");
                is(x0 + seam_px, seam_top, tab, "the opening");
            } else {
                is(x0, inside, tab, "no rim");
                is(mid_x, top, tab, "no rim on top");
            }
            assert_eq!(at(x0, h - 1), colors.foot, "{case}: the foot under the tab");
            assert_eq!(at(mid_x, h - 1), colors.foot, "{case}: the foot");
        }
    }
    crate::tray_raster::clear_ui_fonts_for_test();
}

#[test]
fn a_compact_tab_keeps_its_descenders_inside() {
    if !SELECTED_TAB_IS_GROUND {
        return;
    }
    if !with_ui_faces() {
        aterm_spec::host::could_not_run("no system UI face here for the strip band");
    }
    let geometry = BandGeometry {
        cols: 80,
        cell_w: CELL_W,
        cell_h: 23,
        strip_rows: 1,
        band_top_px: 2,
        scale: 1.0,
        seam_top_px: Some(24),
        foot_px: 0,
    };
    let colors = strip_colors_with_active(Theme::default(), None);
    let metadata = plain(2);
    let segments = layout_segments_with_metadata(80, 2, &metadata, 0, false);
    let titles = vec!["gypsy jig".to_string(), "Apply".to_string()];
    let input = band(
        &segments,
        &titles,
        &metadata,
        StripPaint::default(),
        geometry,
    );
    let rows = raster_band(&input, &[]).expect("band");
    let (rgba, w, h) = image_of(&rows);
    let layout = tab_content_layout(&segments[0], metadata[0]);
    let (_, y0, _, y1) = ink_bbox_off_surfaces(
        &rgba,
        w,
        h,
        usize::from(layout.title_start) * CELL_W,
        usize::from(layout.title_end) * CELL_W,
        &[colors.band_bg, colors.active_bg, colors.chip_bg],
    )
    .expect("the title inks");
    let d = BandDesign::resolve(&geometry, h, band_label_px(h as f32, 1.0));
    assert!(
        y1 as f32 <= d.card_bot - 2.0,
        "descenders at {y1}, the tab ends at {}",
        d.card_bot
    );
    assert!(
        y0 as f32 >= d.card_top + 1.0,
        "ascenders at {y0}, the tab starts at {}",
        d.card_top
    );
    crate::tray_raster::clear_ui_fonts_for_test();
}
