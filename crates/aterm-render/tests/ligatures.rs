// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Programming-ligature gate for the CPU renderer: with a ligature font
// (JetBrains Mono), rendering "a => b != c" must produce
// DIFFERENT ink than the same text with ligatures disabled (proving the shaped
// ligature glyph is actually used), AND a control row of "a = > b" (operators
// separated by spaces, so no run forms) must render IDENTICALLY with ligatures
// on or off (proving the run coalescing breaks on spaces and does not corrupt
// non-ligature text).

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;
use aterm_render::{LigatureMode, Renderer, TextShapingConfig, Theme};

// The ligature test font: $ATERM_LIGATURE_TEST_FONT if set and readable, else the committed
// JetBrains Mono fixture. The override is a dedicated test var, not a general font setting,
// because several tests assert fixture-specific shapes (`=>` ligates, `zero` is present).
fn ligature_test_font() -> Vec<u8> {
    if let Ok(path) = std::env::var("ATERM_LIGATURE_TEST_FONT")
        && let Ok(bytes) = std::fs::read(&path)
    {
        return bytes;
    }
    include_bytes!("fixtures/jetbrains-mono.ttf").to_vec()
}

fn renderer(mode: LigatureMode) -> Renderer {
    let mut r = Renderer::from_bytes(&ligature_test_font(), 18.0, Theme::default())
        .expect("the ligature test font parses");
    r.set_text_shaping(TextShapingConfig {
        ligature_mode: mode,
        ..Default::default()
    });
    r
}

fn render(mode: LigatureMode, text: &[u8]) -> aterm_render::Frame {
    let mut r = renderer(mode);
    let (rows, cols) = (1usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // Hide the cursor so the cursor overlay never perturbs the comparison.
    term.process(b"\x1b[?25l");
    term.process(text);
    let input = term.cell_frame(rows, cols);
    r.render_input(&input)
}

/// Ligated "a => b != c" inks differently from the same text with ligatures off.
#[test]
fn ligated_render_differs_from_unligated() {
    let text = b"a => b != c";
    let (on, off) = (
        render(LigatureMode::Enabled, text),
        render(LigatureMode::Disabled, text),
    );
    assert_eq!(
        (on.width, on.height),
        (off.width, off.height),
        "ligature mode must not change frame dimensions"
    );
    assert_ne!(
        on.pixels, off.pixels,
        "ligated 'a => b != c' must ink differently than the unligated render \
         — the ligature glyph was not used"
    );
}

/// Control: in "a = > b" spaces break every run, so the frame is byte-identical with
/// ligatures on or off.
#[test]
fn spaced_operators_render_identically() {
    let text = b"a = > b";
    let (on, off) = (
        render(LigatureMode::Enabled, text),
        render(LigatureMode::Disabled, text),
    );
    assert_eq!(
        on.pixels, off.pixels,
        "spaced operators 'a = > b' must render identically on/off — no run should ligate"
    );
}

/// Render `text` under `mode` with a Simple selection over columns `[sel_start, sel_end]`
/// (inclusive) on the only row, driven through the public selection API.
fn render_with_selection(
    mode: LigatureMode,
    text: &[u8],
    sel_start: u16,
    sel_end: u16,
) -> aterm_render::Frame {
    let mut r = renderer(mode);
    let (rows, cols) = (1usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    term.process(text);
    // A Left-sided start and a Right-sided end include both endpoints.
    let sel = term.text_selection_mut();
    sel.start_selection(0, sel_start, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(0, sel_end, SelectionSide::Right);
    sel.complete_selection();
    let input = term.cell_frame(rows, cols);
    r.render_input(&input)
}

/// Column `c`'s full-height pixel band in a 1-row, `cols`-column frame.
fn column_band(frame: &aterm_render::Frame, c: usize, cols: usize) -> Vec<u32> {
    let cw = frame.width / cols;
    let x0 = c * cw;
    let mut out = Vec::with_capacity(cw * frame.height);
    for y in 0..frame.height {
        let row = y * frame.width;
        out.extend_from_slice(&frame.pixels[row + x0..row + x0 + cw]);
    }
    out
}

/// A ligature spanning a selection boundary renders per cell: with only the first cell of
/// `=>` selected, that cell shows the selection background and the per-cell glyph.
#[test]
fn ligature_breaks_on_selection_boundary() {
    // "a=>b": '=' at col 1, '>' at col 2 form the arrow run; select only col 1.
    let text = b"a=>b";
    let (no_sel, sel_on, sel_off) = (
        render(LigatureMode::Enabled, text),
        render_with_selection(LigatureMode::Enabled, text, 1, 1),
        render_with_selection(LigatureMode::Disabled, text, 1, 1),
    );
    assert_eq!(
        (no_sel.width, no_sel.height),
        (sel_on.width, sel_on.height),
        "a selection must not change frame dimensions"
    );
    // (a) Breaking the run on the selection boundary changes the render.
    assert_ne!(
        no_sel.pixels, sel_on.pixels,
        "selecting half of '=>' must change the render — the ligature spanned the \
         selection boundary instead of breaking on it"
    );
    // (b) The selected cell carries the theme selection background.
    let theme_sel = Theme::default().selection;
    let band = column_band(&sel_on, 1, 16);
    assert!(
        band.contains(&theme_sel),
        "selected cell of '=>' must show the selection background (0x{theme_sel:06X})"
    );
    // (c) The selected cell renders the per-cell '=', byte-identical to the ligatures-off
    // render; a ligature spanning the boundary would show its empty placeholder there.
    assert_eq!(
        column_band(&sel_on, 1, 16),
        column_band(&sel_off, 1, 16),
        "selected cell must render per-cell (== ligatures-off) — the ligature \
         painted its placeholder across the selection boundary instead of breaking"
    );
}

#[test]
fn selection_before_ligature_preserves_unselected_ligature() {
    // "a=>b": select only col 0 ('a'). The unselected '=>' at cols 1..=2 must
    // still ligate rather than having its leading '=' excluded as a post-selection break.
    let text = b"a=>b";
    let sel_on = render_with_selection(LigatureMode::Enabled, text, 0, 0);
    let sel_off = render_with_selection(LigatureMode::Disabled, text, 0, 0);
    assert_ne!(
        column_band(&sel_on, 1, 16),
        column_band(&sel_off, 1, 16),
        "unselected '=>' immediately after a selected cell must still ligate"
    );
}

/// A ligature run breaks on an SGR style change: plain and bold `=>` differ, and `=` plain
/// with `>` bold matches neither.
#[test]
fn ligature_breaks_on_style_change() {
    // Uniform plain "=>", uniform bold "=>", and a split "=>" ('=' plain, '>' bold).
    let (plain, bold, split) = (
        render(LigatureMode::Enabled, b"a\x1b[0m=>"),
        render(LigatureMode::Enabled, b"a\x1b[1m=>\x1b[0m"),
        render(LigatureMode::Enabled, b"a\x1b[0m=\x1b[1m>\x1b[0m"),
    );
    // Bold weight differs from plain (synthetic embolden widens strokes).
    assert_ne!(
        plain.pixels, bold.pixels,
        "bold '=>' must ink differently than plain '=>'"
    );
    // A mid-operator style change leaves two cells that cannot ligate alone, so it matches
    // neither uniform-style arrow.
    assert_ne!(
        split.pixels, plain.pixels,
        "a style change mid-'=>' must prevent the all-plain arrow ligature"
    );
    assert_ne!(
        split.pixels, bold.pixels,
        "a style change mid-'=>' must prevent the all-bold arrow ligature"
    );
}

/// A synthetic-italic `=>` inks differently from a regular one: the italic bit survives the
/// shaped-run cache key `(run, style)` and the `mono_gid(gid, style)` ligature glyph key.
#[test]
fn italic_ligature_differs_from_regular() {
    let (regular, italic) = (
        render(LigatureMode::Enabled, b"a\x1b[0m=>"),
        render(LigatureMode::Enabled, b"a\x1b[3m=>\x1b[0m"),
    );
    assert_eq!(
        (regular.width, regular.height),
        (italic.width, italic.height),
        "italic must not change frame dimensions"
    );
    assert_ne!(
        regular.pixels, italic.pixels,
        "an italic '=>' must ink differently than a regular '=>' — the ITALIC style \
         bit must persist through the shaped-run cache key and the mono_gid ligature \
         glyph (synthetic slant), not be dropped on the ligature path"
    );
}

// The user's `font_features` (ss01/cv01/zero/stylistic sets) reach the shaper
// (`aterm_font::shape`), checked through the shared feature-list seam and an end-to-end
// shape on DejaVu.

use aterm_types::text_shaping::{FontFeature, FontFeatureSet};

/// The embedded last-resort face (DejaVu Sans Mono). Its GSUB has the discretionary `dlig`
/// feature, which ligates "fi" into one glyph: an observable user feature.
const DEJAVU: &[u8] = include_bytes!("../assets/DejaVuSansMono.ttf");

/// `s` shaped on face 0 of `bytes` under `features`, each over the whole run
/// (the renderer's convention), by the shaping engine called directly.
fn shape_gids(bytes: &[u8], s: &str, features: &[FontFeature]) -> Vec<u32> {
    use aterm_font::shape::{Feature, ShapeFace, UnicodeBuffer};
    let face = ShapeFace::from_slice(bytes, 0).unwrap();
    let features: Vec<Feature> = features
        .iter()
        .map(|f| Feature::new(aterm_font::Tag::from_bytes(&f.tag), f.value))
        .collect();
    let mut buf = UnicodeBuffer::new();
    buf.push_str(s);
    let out = aterm_font::shape::shape(&face, &features, buf);
    out.glyph_infos().iter().map(|i| i.glyph_id).collect()
}

/// The renderer resolves `TextShapingConfig.font_features` into its feature array: a
/// primary-font (`font_id == 0`) feature changes shaping, one scoped to another font does
/// not.
#[test]
fn renderer_applies_config_font_features() {
    // A config carrying a primary-font 'dlig' feature.
    let cfg = TextShapingConfig {
        font_features: vec![FontFeatureSet {
            font_id: 0,
            features: vec![FontFeature::new(*b"dlig", 1)],
        }],
        ..Default::default()
    };
    // A second font_id (1) must NOT leak into the primary face's features.
    let cfg_other_font = TextShapingConfig {
        font_features: vec![FontFeatureSet {
            font_id: 1,
            features: vec![FontFeature::new(*b"dlig", 1)],
        }],
        ..Default::default()
    };

    let mut r = Renderer::from_bytes(DEJAVU, 18.0, Theme::default()).unwrap();

    // The dlig config changes the resolved array's shaping of "fi".
    r.set_text_shaping(cfg);
    let dlig_gids = shape_gids(DEJAVU, "fi", r.resolved_features_for_test());

    r.set_text_shaping(TextShapingConfig::default());
    let base_gids = shape_gids(DEJAVU, "fi", r.resolved_features_for_test());
    assert_ne!(
        base_gids, dlig_gids,
        "configuring a primary-font 'dlig' feature must change the renderer's \
         resolved shaping (base={base_gids:?}, dlig={dlig_gids:?})"
    );

    // A feature scoped to another font_id leaves the primary face's array at the default.
    r.set_text_shaping(cfg_other_font);
    let other_gids = shape_gids(DEJAVU, "fi", r.resolved_features_for_test());
    assert_eq!(
        base_gids, other_gids,
        "a feature scoped to font_id != 0 must not apply to the primary face"
    );
}

/// With a user font feature configured, `row_glyph_plan` shapes even when ligatures are
/// off or the font has no liga/calt, so the feature is not swallowed: an explicit user
/// `calt=1` re-forms the test font's `=>` under `LigatureMode::Disabled`.
#[test]
fn user_feature_reaches_plan_with_ligatures_off() {
    use aterm_render::ColumnGlyph;
    let bytes = ligature_test_font();
    let mut term = Terminal::new(2, 8);
    term.process(b"a => b"); // "=>" occupies cols 2..=3; spaces break the run elsewhere
    let input = term.cell_frame(2, 8);
    let mut r = Renderer::from_bytes(&bytes, 18.0, Theme::default()).unwrap();

    // Ligatures off, no user features: every column stays PerCell.
    r.set_text_shaping(TextShapingConfig {
        ligature_mode: LigatureMode::Disabled,
        ..Default::default()
    });
    let mut off = Vec::new();
    r.row_glyph_plan(&input, 0, &[], &mut off);
    assert!(
        off.iter().all(|g| *g == ColumnGlyph::PerCell),
        "ligatures off + no features must stay all-PerCell, got {off:?}"
    );

    // Ligatures off with an explicit user liga/calt=1: the planner shapes, and the user
    // value wins over the zeroed base, so a Ligated column appears.
    r.set_text_shaping(TextShapingConfig {
        ligature_mode: LigatureMode::Disabled,
        font_features: vec![FontFeatureSet {
            font_id: 0,
            features: vec![FontFeature::new(*b"liga", 1), FontFeature::new(*b"calt", 1)],
        }],
        ..Default::default()
    });
    let mut on = Vec::new();
    r.row_glyph_plan(&input, 0, &[], &mut on);
    assert!(
        on.iter().any(|g| matches!(g, ColumnGlyph::Ligated(_))),
        "a user font_feature must reach the plan even with LigatureMode::Disabled, got {on:?}"
    );
}

/// `unsupported_user_feature_tags` (behind the GUI's "font_features had no effect"
/// warning) reports a configured tag the font's GSUB lacks and omits one it advertises:
/// DejaVu Sans Mono has `dlig` but not `ss99`.
#[test]
fn unsupported_user_feature_tags_reports_only_missing() {
    let mut r = Renderer::from_bytes(DEJAVU, 18.0, Theme::default()).unwrap();
    r.set_text_shaping(TextShapingConfig {
        font_features: vec![FontFeatureSet {
            font_id: 0,
            features: vec![FontFeature::new(*b"dlig", 1), FontFeature::new(*b"ss99", 1)],
        }],
        ..Default::default()
    });
    let missing = r.unsupported_user_feature_tags();
    assert_eq!(
        missing,
        vec![*b"ss99"],
        "dlig is advertised (supported); ss99 is not — only ss99 is reported"
    );

    // No configured features -> nothing to report.
    r.set_text_shaping(TextShapingConfig::default());
    assert!(r.unsupported_user_feature_tags().is_empty());
}

/// An effective font feature does not drag an adjacent procedural (box-drawing) cell onto
/// the primary-by-glyph-id path: with `zero` on, the `0`s shape while `│` stays `PerCell`
/// and keeps its `FaceId::Procedural` rendering.
#[test]
fn feature_does_not_drag_box_drawing_into_primary_face() {
    use aterm_render::ColumnGlyph;
    let bytes = ligature_test_font();
    let mut term = Terminal::new(2, 6);
    term.process("│0│0".as_bytes()); // box-drawing U+2502 interleaved with zeros
    let input = term.cell_frame(2, 6);
    let mut r = Renderer::from_bytes(&bytes, 18.0, Theme::default()).unwrap();
    r.set_text_shaping(TextShapingConfig {
        font_features: vec![FontFeatureSet {
            font_id: 0,
            features: vec![FontFeature::new(*b"zero", 1)],
        }],
        ..Default::default()
    });
    let mut out = Vec::new();
    r.row_glyph_plan(&input, 0, &[], &mut out);
    assert_eq!(
        out[0],
        ColumnGlyph::PerCell,
        "box-drawing │ must stay procedural, not be drawn from the primary face: {out:?}"
    );
    assert_eq!(
        out[2],
        ColumnGlyph::PerCell,
        "second │ must stay procedural: {out:?}"
    );
    assert!(
        matches!(out[1], ColumnGlyph::Ligated(_)),
        "the 0 must shape (slashed zero) despite the adjacent box cell: {out:?}"
    );
    assert!(
        matches!(out[3], ColumnGlyph::Ligated(_)),
        "the second 0 must shape too: {out:?}"
    );
}

// Merged N:1 ligatures (`admit_collapsed`, config `merged_ligatures`): a run whose shaping
// collapses several cells into one wide glyph is sliced into per-cell tiles. Any face with
// Latin `fi`/`fl` ligatures collapses them under `liga`; the checked-in Luckiest Guy
// (`assets/game`) is one.

/// Luckiest Guy's bytes, plus the wide gid and cell count its `fi` collapses to under the
/// renderer's base feature list.
fn merged_ligature_font() -> (Vec<u8>, u16, u16) {
    let bytes = include_bytes!("../assets/game/LuckiestGuy-Regular.ttf").to_vec();
    let feats = aterm_render::ligature_shaping::build_feature_list(&[], true);
    let [gid] = shape_gids(&bytes, "fi", &feats)[..] else {
        panic!("Luckiest Guy's `fi` must shape to one ligature glyph");
    };
    let gid = u16::try_from(gid).expect("a glyph id fits u16");
    (bytes, gid, 2)
}

fn merged_renderer(bytes: &[u8], admit: bool) -> Renderer {
    let mut r = Renderer::from_bytes(bytes, 18.0, Theme::default()).unwrap();
    r.set_text_shaping(TextShapingConfig {
        ligature_mode: LigatureMode::Enabled,
        admit_collapsed: admit,
        ..Default::default()
    });
    r
}

fn frame(r: &mut Renderer, text: &[u8], rows: usize, cols: usize) -> aterm_render::Frame {
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l"); // hide cursor
    term.process(text);
    let input = term.cell_frame(rows, cols);
    r.render_input(&input)
}

/// A merged `fi` with the flag on inks differently from the flag off (separate `f`/`i`
/// glyphs) and from ligatures disabled; with the flag off the frame is byte-identical to
/// ligatures disabled.
#[test]
fn merged_ligature_slices_reach_pixels() {
    let (bytes, _gid, _n) = merged_ligature_font();
    let mut on = merged_renderer(&bytes, true);
    let mut off = merged_renderer(&bytes, false);
    let mut nolig = {
        let mut r = Renderer::from_bytes(&bytes, 18.0, Theme::default()).unwrap();
        r.set_text_shaping(TextShapingConfig {
            ligature_mode: LigatureMode::Disabled,
            admit_collapsed: true,
            ..Default::default()
        });
        r
    };
    let f_on = frame(&mut on, b"fi", 1, 4);
    let f_off = frame(&mut off, b"fi", 1, 4);
    let f_nolig = frame(&mut nolig, b"fi", 1, 4);
    assert_eq!((f_on.width, f_on.height), (f_off.width, f_off.height));
    assert_ne!(
        f_on.pixels, f_off.pixels,
        "the sliced merged ligature must ink differently than the per-cell fi"
    );
    assert_ne!(
        f_on.pixels, f_nolig.pixels,
        "the sliced merged ligature must ink differently than ligatures-off"
    );
    assert_eq!(
        f_off.pixels, f_nolig.pixels,
        "admit_collapsed OFF must be byte-identical to ligatures-off (collapse declined)"
    );
}

/// The per-cell slice tiles (`rasterize_ligature_slice` via `glyph_image`) reassemble the
/// wide glyph byte-exactly on real font bytes: each slice column reproduces the wide
/// raster's column, folding its `xmin`.
#[test]
fn merged_ligature_slice_tiles_reassemble_wide_glyph() {
    use aterm_render::{GlyphImage, StyleBits};
    let (bytes, gid, _n) = merged_ligature_font();
    let mut r = merged_renderer(&bytes, true);
    let (cell_w, _cell_h) = r.cell_size();
    // The wide glyph, rasterized once.
    let parent = r.ligature_key(gid, StyleBits::REGULAR);
    let (pw, ph, pxmin, pbytes) = match r.glyph_image(parent).clone() {
        GlyphImage::Mono {
            width,
            height,
            xmin,
            bytes,
            ..
        } => (width, height, xmin, bytes),
        GlyphImage::Rgba { .. } => panic!("the wide `fi` ligature must be Mono coverage"),
    };
    assert!(pw > 0 && ph > 0, "wide glyph must have ink");
    // Slices span every cell the wide glyph's ink touches (run-origin [xmin, xmin+pw)).
    for s in 0..pw {
        let dest = s as i64 + pxmin as i64; // run-origin column
        if dest < 0 {
            continue; // ink left of the run origin has no owning cell
        }
        let k = (dest / cell_w as i64) as u16;
        let j = (dest % cell_w as i64) as usize;
        let tile = match r.glyph_image(r.ligature_slice_key(gid, k, StyleBits::REGULAR)) {
            GlyphImage::Mono { width, bytes, .. } => {
                assert_eq!(*width, cell_w, "slice tile is one cell wide");
                bytes.clone()
            }
            GlyphImage::Rgba { .. } => panic!("slice must be Mono coverage"),
        };
        for row in 0..ph {
            assert_eq!(
                tile[row * cell_w + j],
                pbytes[row * pw + s],
                "wide column {s} (row {row}) must round-trip through slice k={k} col {j}"
            );
        }
    }
}

/// A block cursor over a merged ligature changes only its own cell: each slice tile is
/// cell-local, so the cut-out (clipped to the cursor rect) never touches the neighbour.
#[test]
fn merged_ligature_block_cursor_recolours_only_its_cell() {
    let (bytes, _gid, _n) = merged_ligature_font();
    let mut r = merged_renderer(&bytes, true);
    let (cell_w, _) = r.cell_size();
    let pad = r.pad();
    // Cursorless baseline (cursor hidden).
    let base = frame(&mut r, b"fi", 1, 4);
    let w = base.width;

    // "fi" with a steady block cursor on `col`.
    let cursor_frame = |r: &mut Renderer, col: u16| -> aterm_render::Frame {
        let mut term = Terminal::new(1, 4);
        term.process(b"\x1b[2 q"); // steady block cursor
        term.process(b"fi");
        term.process(format!("\x1b[1;{}H", col + 1).as_bytes()); // move cursor to col
        let input = term.cell_frame(1, 4);
        r.render_input(&input)
    };

    // A cell's pixel band is columns [pad + c*cell_w, pad + (c+1)*cell_w).
    let differs_in_band = |a: &aterm_render::Frame, b: &aterm_render::Frame, c: usize| -> bool {
        let (x0, x1) = (pad + c * cell_w, pad + (c + 1) * cell_w);
        for y in 0..a.height {
            for x in x0..x1.min(w) {
                if a.pixels[y * w + x] != b.pixels[y * w + x] {
                    return true;
                }
            }
        }
        false
    };
    let unchanged_outside = |a: &aterm_render::Frame, b: &aterm_render::Frame, c: usize| -> bool {
        let (x0, x1) = (pad + c * cell_w, pad + (c + 1) * cell_w);
        for y in 0..a.height {
            for x in 0..w {
                if (x0..x1).contains(&x) {
                    continue;
                }
                if a.pixels[y * w + x] != b.pixels[y * w + x] {
                    return false;
                }
            }
        }
        true
    };

    let cur0 = cursor_frame(&mut r, 0);
    assert!(
        differs_in_band(&base, &cur0, 0),
        "the block cursor on cell 0 must change cell 0"
    );
    assert!(
        unchanged_outside(&base, &cur0, 0),
        "the block cursor on cell 0 must NOT change any pixel outside cell 0 (no bleed onto the sliced ligature's other cell)"
    );

    let cur1 = cursor_frame(&mut r, 1);
    assert!(
        differs_in_band(&base, &cur1, 1),
        "the block cursor on cell 1 must change cell 1"
    );
    assert!(
        unchanged_outside(&base, &cur1, 1),
        "the block cursor on cell 1 must NOT change any pixel outside cell 1"
    );
}

/// `admit_collapsed` only adds the N:1 branch: on JetBrains Mono (which never collapses)
/// the frame is byte-identical with the flag on or off.
#[test]
fn admit_collapsed_is_byte_identical_on_one_to_one_font() {
    let bytes = ligature_test_font();
    let mut on = merged_renderer(&bytes, true);
    let mut off = merged_renderer(&bytes, false);
    let f_on = frame(&mut on, b"a => b != c == d", 1, 16);
    let f_off = frame(&mut off, b"a => b != c == d", 1, 16);
    assert_eq!(
        f_on.pixels, f_off.pixels,
        "admit_collapsed must not perturb a 1:1 (JetBrains) ligature font"
    );
}
