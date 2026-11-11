// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_core::terminal::{RenderCell, UnderlineStyle};

fn infos(buffer: &GlyphBuffer) -> Vec<(u32, u32)> {
    buffer
        .glyph_infos()
        .iter()
        .map(|g| (g.glyph_id, g.cluster))
        .collect()
}

fn positions(buffer: &GlyphBuffer) -> Vec<(i32, i32, i32, i32)> {
    buffer
        .glyph_positions()
        .iter()
        .map(|g| (g.x_advance, g.y_advance, g.x_offset, g.y_offset))
        .collect()
}

#[test]
fn row_shaper_matches_fresh_plans_across_segment_and_font_changes() {
    let mut fonts = vec![
        include_bytes!("../assets/DejaVuSansMono.ttf").to_vec(),
        include_bytes!("../tests/fixtures/jetbrains-mono.ttf").to_vec(),
    ];
    if cfg!(target_os = "macos")
        && let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf")
    {
        fonts.push(bytes);
    }
    let feature_sets = [
        engine_features(&build_feature_list(&[], true)),
        engine_features(&build_feature_list(&[FontFeature::new(*b"zero", 1)], true)),
        engine_features(&build_feature_list(&[FontFeature::new(*b"ss01", 1)], false)),
    ];
    let runs = [
        "function",
        "=>",
        "000",
        "office",
        "éè",
        "Русский",
        "العربية",
        "עברית",
        "देवनागरी",
        "Ελληνικά",
        "漢字",
        "e\u{301}",
        "!=",
        "",
        "0",
        "x",
        "tail",
    ];
    let mut scratch = UnicodeBuffer::new();
    let mut changed_glyphs = false;
    for bytes in &fonts {
        for features in &feature_sets {
            for weight in [350.0, 700.0] {
                let mut face = aterm_font::Face::parse(bytes, 0).expect("fixture face");
                face.set_variation(aterm_font::Tag::from_bytes(b"wght"), weight);
                let mut shaper = RowShaper::new(ShapeFace::new(face.clone()), features);
                let fresh_face = ShapeFace::new(face);
                for run in runs.iter().chain(runs.iter().rev()) {
                    let mut fresh = UnicodeBuffer::new();
                    fresh.push_str(run);
                    fresh.guess_segment_properties();
                    let expected = aterm_font::shape::shape(&fresh_face, features, fresh);
                    let actual = shaper.shape_buffer(run, &mut scratch);
                    assert_eq!(infos(&actual), infos(&expected), "glyphs: {run}");
                    assert_eq!(positions(&actual), positions(&expected), "positions: {run}");
                    scratch = actual.clear();
                    assert!(shaper.plans.len() <= 4);
                    assert!(scratch.is_empty());
                    assert_eq!(scratch.direction(), Direction::Invalid);
                    assert_eq!(scratch.script(), None);
                    let chars = run.chars().collect::<Vec<_>>();
                    changed_glyphs |= shaper.shape(run, &chars, true, &mut scratch).is_some();
                }
            }
        }
    }
    assert!(
        changed_glyphs,
        "the corpus must include actual substitutions"
    );
}

#[test]
fn build_feature_list_puts_the_base_pair_first_then_user_features() {
    let user = [FontFeature::new(*b"cv01", 2), FontFeature::new(*b"liga", 0)];
    #[expect(clippy::type_complexity, reason = "a test's case table")]
    let cases: [(&[FontFeature], bool, &[([u8; 4], u32)]); 3] = [
        (&[], true, &[(*b"liga", 1), (*b"calt", 1)]),
        (
            &user,
            true,
            &[(*b"liga", 1), (*b"calt", 1), (*b"cv01", 2), (*b"liga", 0)],
        ),
        (
            &user[..1],
            false,
            &[(*b"liga", 0), (*b"calt", 0), (*b"cv01", 2)],
        ),
    ];
    for (user, on, want) in cases {
        let got: Vec<([u8; 4], u32)> = build_feature_list(user, on)
            .iter()
            .map(|f| (f.tag, f.value))
            .collect();
        assert_eq!(got, want, "ligatures_on={on} user={user:?}");
    }
}

#[test]
fn engine_features_mirror_the_list_over_the_whole_run() {
    let list = build_feature_list(
        &[FontFeature::new(*b"cv01", 2), FontFeature::new(*b"liga", 0)],
        true,
    );
    let engine = engine_features(&list);
    assert_eq!(engine.len(), list.len());
    for (e, f) in engine.iter().zip(&list) {
        assert_eq!(e.tag.to_bytes(), f.tag);
        assert_eq!(e.value, f.value);
    }
}

fn cell(ch: char) -> RenderCell {
    RenderCell {
        ch,
        fg: [0, 0, 0],
        bg: [0, 0, 0],
        wide: false,
        emoji_presentation: false,
        text_presentation: false,
        bold: false,
        italic: false,
        underline: UnderlineStyle::None,
        strikethrough: false,
        overline: false,
        underline_color: None,
        overline_color: None,
        script: aterm_core::terminal::Script::Normal,
    }
}

#[test]
fn explicit_presentation_cells_are_not_shapeable() {
    let mut c = cell('\u{1F600}');
    c.text_presentation = true;
    assert!(
        !cell_is_shapeable(&c, false, false),
        "VS15 must stay on the presentation-aware per-cell resolver"
    );
    c.text_presentation = false;
    c.emoji_presentation = true;
    assert!(
        !cell_is_shapeable(&c, false, false),
        "VS16 must stay on the presentation-aware per-cell resolver"
    );
}

#[test]
fn ligature_run_breaks_on_image_cell() {
    let cells = [cell('='), cell('>'), cell('='), cell('='), cell('>')];
    let shapeable: Vec<bool> = (0..cells.len())
        .map(|c| cell_is_shapeable(&cells[c], false, c == 2))
        .collect();
    let shape = |run: &str, chars: &[char], _style: StyleBits| -> Option<ShapedRun> {
        if run == "=>" && chars.len() == 2 {
            Some(ShapedRun::PerColumn(
                vec![Some(900u16), Some(901u16)].into_boxed_slice(),
            ))
        } else {
            None
        }
    };
    let mut out = Vec::new();
    let mut run = String::new();
    let mut run_chars: Vec<char> = Vec::new();
    plan_row_runs(
        &cells,
        cells.len(),
        &shapeable,
        &[],
        &[],
        2,
        |_c| StyleBits::REGULAR,
        shape,
        &mut run,
        &mut run_chars,
        &mut out,
    );
    assert_eq!(out[0], ColumnGlyph::Ligated(900));
    assert_eq!(out[1], ColumnGlyph::Ligated(901));
    assert_eq!(
        out[2],
        ColumnGlyph::PerCell,
        "the image-covered cell must stay per-cell"
    );
    assert_eq!(out[3], ColumnGlyph::Ligated(900));
    assert_eq!(out[4], ColumnGlyph::Ligated(901));
}

#[test]
fn run_boundary_preserves_right_pane_initial_ligature() {
    let cells = [cell('x'), cell('x'), cell('='), cell('>')];
    let shapeable = vec![true; cells.len()];
    let run_boundary_before = [false, false, true, false];
    let shape = |run: &str, chars: &[char], _style: StyleBits| -> Option<ShapedRun> {
        (run == "=>" && chars.len() == 2)
            .then(|| ShapedRun::PerColumn(vec![Some(910), Some(911)].into_boxed_slice()))
    };
    let mut out = Vec::new();
    let (mut run, mut run_chars) = (String::new(), Vec::new());
    plan_row_runs(
        &cells,
        cells.len(),
        &shapeable,
        &run_boundary_before,
        &[],
        2,
        |_c| StyleBits::REGULAR,
        shape,
        &mut run,
        &mut run_chars,
        &mut out,
    );

    assert_eq!(out[0], ColumnGlyph::PerCell);
    assert_eq!(out[1], ColumnGlyph::PerCell);
    assert_eq!(out[2], ColumnGlyph::Ligated(910));
    assert_eq!(out[3], ColumnGlyph::Ligated(911));
}

fn plan_pieces(text: &str, order: &[u16]) -> (Vec<String>, Vec<ColumnGlyph>) {
    let cells: Vec<RenderCell> = text.chars().map(cell).collect();
    let shapeable = vec![true; cells.len()];
    let mut seen = Vec::new();
    let shape = |run: &str, chars: &[char], _s: StyleBits| -> Option<ShapedRun> {
        seen.push(run.to_string());
        let gids = (0..chars.len()).map(|i| u16::try_from(100 + i).ok());
        Some(ShapedRun::PerColumn(gids.collect()))
    };
    let mut out = Vec::new();
    let (mut run, mut run_chars) = (String::new(), Vec::new());
    plan_row_runs(
        &cells,
        cells.len(),
        &shapeable,
        &[],
        order,
        2,
        |_c| StyleBits::REGULAR,
        shape,
        &mut run,
        &mut run_chars,
        &mut out,
    );
    (seen, out)
}

fn ligated(gids: &[u16]) -> Vec<ColumnGlyph> {
    gids.iter().map(|&g| ColumnGlyph::Ligated(g)).collect()
}

#[test]
fn a_backwards_rtl_stretch_is_shaped_in_logical_order() {
    let visual = "\u{627}\u{628}\u{62D}\u{631}\u{645}==";
    let (seen, out) = plan_pieces(visual, &[4, 3, 2, 1, 0, 5, 6]);
    assert_eq!(seen, ["\u{645}\u{631}\u{62D}\u{628}\u{627}", "=="]);
    assert_eq!(out, ligated(&[104, 103, 102, 101, 100, 100, 101]));
    for order in [&[][..], &[0, 1, 2, 3, 4, 5, 6]] {
        let (seen, out) = plan_pieces(visual, order);
        assert_eq!(seen, [visual], "{order:?}");
        assert_eq!(out, ligated(&[100, 101, 102, 103, 104, 105, 106]));
    }
}

#[test]
fn rtl_level_neutrals_keep_the_painted_order() {
    let (seen, out) = plan_pieces("\u{5D3}\u{5D2}==\u{5D1}\u{5D0}", &[5, 4, 3, 2, 1, 0]);
    assert_eq!(seen, ["\u{5D2}\u{5D3}", "==", "\u{5D0}\u{5D1}"]);
    assert_eq!(out, ligated(&[101, 100, 100, 101, 101, 100]));
}

#[test]
fn plan_row_runs_expands_collapsed_to_slices() {
    let cells = [
        cell('x'),
        cell(' '),
        cell('<'),
        cell('='),
        cell('>'),
        cell(' '),
        cell('y'),
    ];
    let shapeable: Vec<bool> = cells
        .iter()
        .map(|c| cell_is_shapeable(c, false, false))
        .collect();
    let shape = |_run: &str, chars: &[char], _s: StyleBits| -> Option<ShapedRun> {
        (chars.len() == 3).then_some(ShapedRun::Collapsed { gid: 700, n: 3 })
    };
    let mut out = Vec::new();
    let (mut run, mut run_chars) = (String::new(), Vec::new());
    plan_row_runs(
        &cells,
        cells.len(),
        &shapeable,
        &[],
        &[],
        2,
        |_c| StyleBits::REGULAR,
        shape,
        &mut run,
        &mut run_chars,
        &mut out,
    );
    assert_eq!(out[0], ColumnGlyph::PerCell, "lone 'x' stays per-cell");
    assert_eq!(out[1], ColumnGlyph::PerCell, "space");
    assert_eq!(
        out[2],
        ColumnGlyph::LigatedSlice {
            gid: 700,
            k: 0,
            n: 3
        }
    );
    assert_eq!(
        out[3],
        ColumnGlyph::LigatedSlice {
            gid: 700,
            k: 1,
            n: 3
        }
    );
    assert_eq!(
        out[4],
        ColumnGlyph::LigatedSlice {
            gid: 700,
            k: 2,
            n: 3
        }
    );
    assert_eq!(out[5], ColumnGlyph::PerCell, "space");
    assert_eq!(out[6], ColumnGlyph::PerCell, "lone 'y' stays per-cell");
}

#[test]
fn slice_clipping_matches_geometric_pixels_at_signed_extremes() {
    for cell_w in [1usize, 8, 31] {
        for raster_w in [0usize, 1, 7, 23] {
            let height = 3;
            let raster: Vec<u8> = (0..raster_w * height)
                .map(|i| (i % 251 + 1) as u8)
                .collect();
            for xmin in [i32::MIN, -32, -1, 0, 1, 32, i32::MAX] {
                for k in [i64::MIN, -4, -1, 0, 1, 4, i64::MAX] {
                    let tile = extract_cell_slice(&raster, raster_w, height, xmin, cell_w, k);
                    let expected: Vec<u8> = (0..height)
                        .flat_map(|row| {
                            let raster = &raster;
                            (0..cell_w).map(move |col| {
                                let source =
                                    i128::from(k) * cell_w as i128 - i128::from(xmin) + col as i128;
                                if source >= 0 && source < raster_w as i128 {
                                    raster[row * raster_w + source as usize]
                                } else {
                                    0
                                }
                            })
                        })
                        .collect();
                    assert_eq!(
                        tile, expected,
                        "cell={cell_w} raster={raster_w} xmin={xmin} k={k}"
                    );
                }
            }
        }
    }
}

#[test]
fn extract_cell_slice_reassembles_wide_raster() {
    let (raster_w, height, cell_w) = (23usize, 4usize, 8usize);
    let raster: Vec<u8> = (0..raster_w * height)
        .map(|i| (i % 251 + 1) as u8)
        .collect();
    for &xmin in &[0i32, 3, 8, -2, -9] {
        let lo_cell = (xmin.min(0)).div_euclid(cell_w as i32);
        let hi = xmin + raster_w as i32;
        let n = ((hi - lo_cell * cell_w as i32) as usize).div_ceil(cell_w) + 1;
        for row in 0..height {
            for s in 0..raster_w {
                let dest = s as i64 + xmin as i64;
                let k = dest.div_euclid(cell_w as i64);
                let j = (dest - k * cell_w as i64) as usize;
                let kk = (k - lo_cell as i64) as usize;
                assert!(kk < n);
                let tile = extract_cell_slice(&raster, raster_w, height, xmin, cell_w, k);
                assert_eq!(tile.len(), cell_w * height);
                assert_eq!(
                    tile[row * cell_w + j],
                    raster[row * raster_w + s],
                    "xmin={xmin} row={row} s={s} must round-trip through tile k={k}"
                );
            }
        }
    }
    assert!(extract_cell_slice(&raster, raster_w, height, 0, 0, 0).is_empty());
    assert!(extract_cell_slice(&raster, raster_w, 0, 0, cell_w, 0).is_empty());
    assert!(extract_cell_slice(&[], raster_w, height, 0, cell_w, 0).is_empty());
}

#[test]
fn map_shaped_run_rejects_duplicate_cluster_collision() {
    fn glyph(glyph_id: u32, cluster: u32) -> GlyphInfo {
        let mut info = GlyphInfo::default();
        info.glyph_id = glyph_id;
        info.cluster = cluster;
        info
    }

    let bytes = include_bytes!("../tests/fixtures/jetbrains-mono.ttf");
    let face = aterm_font::Face::parse(bytes, 0).expect("fixture face");
    let run = "==";
    let run_chars = ['=', '='];

    let one_to_one = [glyph(900, 0), glyph(901, 1)];
    assert_eq!(
        map_shaped_run(&face, run, &run_chars, false, &one_to_one),
        Some(ShapedRun::PerColumn(
            vec![Some(900), Some(901)].into_boxed_slice()
        ))
    );

    let colliding = [glyph(900, 0), glyph(901, 0)];
    assert_eq!(
        map_shaped_run(&face, run, &run_chars, false, &colliding),
        None
    );
}
