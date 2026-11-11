// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{FaceId, StyleBits, Theme};
use aterm_core::terminal::Terminal;

fn input(bytes: &[u8]) -> RenderInput {
    let mut term = Terminal::new(2, 8);
    term.process(bytes);
    term.cell_frame(2, 8)
}

fn actual_plan(renderer: &mut Renderer, input: &RenderInput) -> Vec<ColumnGlyph> {
    let mut plan = Vec::new();
    let mut breaks = Vec::new();
    ligature_break_cols_into(input, 0, renderer.text_shaping(), &mut breaks);
    renderer.row_glyph_plan(input, 0, &breaks, &mut plan);
    plan
}

/// Both plans come from the actual font/shaper, not an injected glyph-id oracle.
fn plan_transition(renderer: &mut Renderer, before: &RenderInput, after: &RenderInput) {
    let prior_plan = actual_plan(renderer, before);
    let first = renderer.glyph_ink_for(before);
    assert!(Arc::ptr_eq(&first, &renderer.glyph_ink_for(before)));
    let next_plan = actual_plan(renderer, after);
    let shared = prior_plan.len().min(next_plan.len());
    assert_ne!(
        prior_plan[..shared],
        next_plan[..shared],
        "a real shaped prefix changes"
    );
    assert!(first.for_frame(after, renderer.cell_size()).is_none());
    let fresh = renderer.glyph_ink_for(after);
    let frame = fresh.for_frame(after, renderer.cell_size()).unwrap();
    assert!(!Arc::ptr_eq(
        &first.frame.as_ref().unwrap().rows[0],
        &frame.rows[0]
    ));
    let mut changed_observed = 0;
    for (col, planned) in next_plan.iter().copied().enumerate() {
        if let Some((key, coverage)) = frame.coverage(0, col) {
            let cell = &after.cells[0][col];
            let expected = match planned {
                ColumnGlyph::Ligated(gid) => renderer.ligature_key(gid, cell_style(cell)),
                ColumnGlyph::LigatedSlice { gid, k, .. } => {
                    renderer.ligature_slice_key(gid, k, cell_style(cell))
                }
                ColumnGlyph::PerCell => renderer.resolve_cell_key_for_span(
                    after.cluster_at(0, col),
                    cell,
                    materialized_cell_span(&after.cells[0], col),
                ),
            };
            assert_eq!(
                key, expected,
                "fresh evidence follows the actual plan at {col}"
            );
            changed_observed +=
                usize::from(coverage.max > 0 && prior_plan.get(col) != Some(&planned));
        }
    }
    assert!(
        changed_observed > 0,
        "a changed plan column observes real ink with its fresh key"
    );
    assert!(Arc::ptr_eq(&fresh, &renderer.glyph_ink_for(after)));
}

/// Two frames' ink, drawn by one renderer, share one raster table, so a reader that keeps
/// what it read of a glyph by index (the letter guard's ring reads) keeps it from frame to
/// frame. NEGATIVE CONTROL (2026-11-05): `glyph_ink_for` giving each frame a copy of the
/// table (`ink.rasters = ink.rasters.to_vec().into()`) fails it.
#[test]
fn a_frame_s_ink_shares_the_face_s_raster_table() {
    let mut renderer =
        Renderer::from_bytes(crate::embedded_font(), 24.0, Theme::default()).unwrap();
    let first = renderer.glyph_ink_for(&input(b"one"));
    let next = renderer.glyph_ink_for(&input(b"two"));
    assert!(!Arc::ptr_eq(&first, &next), "fixture: two frames' carriers");
    assert!(
        Arc::ptr_eq(first.raster_table(), next.raster_table()),
        "each frame's ink read a table of its own"
    );
}

#[test]
fn bidi_map_changes_reject_old_carriers_and_follow_the_actual_font_plan() {
    let mut renderer =
        Renderer::from_bytes(crate::embedded_font(), 24.0, Theme::default()).unwrap();
    renderer.debug_block_on_lazy_fallbacks();
    let mut visual = input(b"       ");
    visual.cursor_visible = false;
    for (cell, ch) in visual.cells[0]
        .iter_mut()
        .zip("\u{627}\u{628}\u{62d}\u{631}\u{645}==".chars())
    {
        cell.ch = ch;
        assert_ne!(
            renderer.font.lookup_glyph_index(ch),
            0,
            "fixture draws {ch}"
        );
    }
    visual.visual_to_logical.clear();
    let mut logical = visual.clone();
    logical.visual_to_logical = vec![vec![4, 3, 2, 1, 0, 5, 6, 7]];
    plan_transition(&mut renderer, &visual, &logical);
}

fn image(z_index: i32) -> aterm_core::grid::extra::ImageRef {
    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageScaling};
    aterm_core::grid::extra::ImageRef {
        image: Arc::new(ImageData {
            bytes: Vec::new(),
            format: ImageFormat::Unknown,
            cols: 1,
            rows: 1,
            z_index,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        }),
        cell_row: 0,
        cell_col: 0,
        kitty: None,
    }
}

#[test]
fn image_z_and_effective_cols_invalidate_the_actual_neighboring_ligature() {
    let mut renderer = Renderer::from_bytes(
        include_bytes!("../../tests/fixtures/jetbrains-mono.ttf"),
        24.0,
        Theme::default(),
    )
    .unwrap();
    renderer.debug_block_on_lazy_fallbacks();
    let mut behind = input(b"==");
    behind.cursor_visible = false;
    behind.images = vec![vec![(1, image(-1))]];
    let mut covering = behind.clone();
    covering.images[0][0].1 = image(0);
    plan_transition(&mut renderer, &behind, &covering);
    let fresh = renderer.glyph_ink_for(&covering);
    assert!(
        fresh
            .for_frame(&covering, renderer.cell_size())
            .unwrap()
            .coverage(0, 1)
            .is_none()
    );
    let mut narrow = behind.clone();
    narrow.cols = 1;
    assert_eq!(
        behind.cells, narrow.cells,
        "a narrower extraction retains materialized cells"
    );
    plan_transition(&mut renderer, &behind, &narrow);
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn lcd_rasters_refuse_mono_authority_and_revoke_carriers_across_route_roundtrips() {
    let mut renderer =
        Renderer::from_bytes(crate::embedded_font(), 24.0, Theme::default()).unwrap();
    renderer.debug_block_on_lazy_fallbacks();
    let text = input(b"M");
    let mono = renderer.glyph_ink_for(&text);
    let (key, coverage) = mono
        .for_frame(&text, renderer.cell_size())
        .unwrap()
        .coverage(0, 0)
        .unwrap();
    assert!(coverage.max > 0);
    if crate::hinted::HintMode::portable_forced() {
        assert!(!renderer.set_font_subpixel("rgb"));
        assert_eq!(
            renderer.font_subpixel(),
            "off",
            "portable mode has no LCD route to certify"
        );
        aterm_spec::host::could_not_run(
            "LCD route control needs the native rasterizer, not forced portable coverage",
        );
    }
    assert!(renderer.set_font_subpixel("rgb"));
    assert!(renderer.input_uses_lcd_text(&text));
    assert!(
        renderer.subpx_image(key).is_some(),
        "the actual draw route has a separate LCD raster"
    );
    assert!(mono.for_frame(&text, renderer.cell_size()).is_none());
    let rgb = renderer.glyph_ink_for(&text);
    assert!(
        rgb.for_frame(&text, renderer.cell_size())
            .unwrap()
            .coverage(0, 0)
            .is_none()
    );
    assert!(renderer.set_font_subpixel("bgr"));
    assert!(rgb.for_frame(&text, renderer.cell_size()).is_none());
    let bgr = renderer.glyph_ink_for(&text);
    renderer.set_background_opacity(0.9);
    assert!(bgr.for_frame(&text, renderer.cell_size()).is_none());
    let translucent = renderer.glyph_ink_for(&text);
    assert!(
        translucent
            .for_frame(&text, renderer.cell_size())
            .unwrap()
            .coverage(0, 0)
            .unwrap()
            .1
            .max
            > 0
    );
    renderer.set_background_opacity(1.0);
    assert!(translucent.for_frame(&text, renderer.cell_size()).is_none());
    assert!(renderer.set_font_subpixel("off"));
    let restored = renderer.glyph_ink_for(&text);
    assert!(
        restored
            .for_frame(&text, renderer.cell_size())
            .unwrap()
            .coverage(0, 0)
            .unwrap()
            .1
            .max
            > 0
    );
    assert!(
        mono.for_frame(&text, renderer.cell_size()).is_none(),
        "a settings roundtrip cannot revive old evidence"
    );

    let mut fallback =
        Renderer::from_bytes(crate::embedded_symbols_font(), 24.0, Theme::default()).unwrap();
    fallback.set_fallback_bytes(crate::embedded_font()).unwrap();
    fallback.debug_block_on_lazy_fallbacks();
    assert!(fallback.set_font_subpixel("rgb"));
    let z = input(b"Z");
    let ink = fallback.glyph_ink_for(&z);
    let (key, coverage) = ink
        .for_frame(&z, fallback.cell_size())
        .unwrap()
        .coverage(0, 0)
        .unwrap();
    assert_eq!(key.source, FaceId::Fallback);
    assert!(fallback.subpx_image(key).is_none());
    assert!(
        coverage.max > 0,
        "the genuinely mono fallback remains supported on an LCD frame"
    );
}

#[test]
fn observed_rows_reuse_rasters_and_reject_changed_style_size_and_blend() {
    let mut renderer =
        Renderer::from_bytes(crate::embedded_font(), 24.0, Theme::default()).unwrap();
    renderer.debug_block_on_lazy_fallbacks();
    let regular = input(b"W");
    let first = renderer.glyph_ink_for(&regular);
    let repeated = renderer.glyph_ink_for(&regular);
    assert!(
        Arc::ptr_eq(&first, &repeated),
        "an animation-only frame reuses its observation"
    );
    let (regular_key, _) = first
        .for_frame(&regular, renderer.cell_size())
        .unwrap()
        .coverage(0, 0)
        .unwrap();

    let italic = input(b"\x1b[3mW");
    assert!(first.for_frame(&italic, renderer.cell_size()).is_none());
    let styled = renderer.glyph_ink_for(&italic);
    let (italic_key, coverage) = styled
        .for_frame(&italic, renderer.cell_size())
        .unwrap()
        .coverage(0, 0)
        .unwrap();
    assert!(italic_key.style.contains(StyleBits::ITALIC));
    assert_ne!(
        regular_key, italic_key,
        "the actual italic raster replaces regular evidence"
    );
    assert!(coverage.max > 0);
    let (_, again, _) = renderer.glyph_ink_for(&italic).frame.as_ref().unwrap().rows[0].glyphs[0]
        .as_ref()
        .unwrap()
        .clone();
    let original = &styled.frame.as_ref().unwrap().rows[0].glyphs[0]
        .as_ref()
        .unwrap()
        .1;
    assert!(
        Arc::ptr_eq(original, &again),
        "coverage is shared from its raster entry"
    );

    let old_cell = renderer.cell_size();
    renderer.set_px(26.0);
    let resized = renderer.glyph_ink_for(&italic);
    assert!(styled.for_frame(&italic, renderer.cell_size()).is_none());
    assert_ne!(old_cell, renderer.cell_size());
    let before = resized.frame.as_ref().unwrap().epoch();
    renderer.set_text_blending(TextBlending::Linear);
    let blended = renderer.glyph_ink_for(&italic);
    assert_ne!(before, blended.frame.as_ref().unwrap().epoch());
    assert!(!blended.frame.as_ref().unwrap().corrected());
}

/// After-land F2: a wide glyph's evidence is its lead cell's, read over both its cells: its
/// ink rows are exactly the pixels the renderer inks there (each row's first to last), and
/// its right half holds none of its own. Negative control: [`with_narrow_evidence_only`]
/// holds none for either cell, as before.
#[test]
fn a_wide_glyph_s_evidence_spans_both_its_cells() {
    let mut renderer = Renderer::from_system(24.0, Theme::default()).unwrap();
    renderer.debug_block_on_lazy_fallbacks();
    let mut text = input("\u{4f60}\u{597d}".as_bytes());
    text.cursor_visible = false;
    let (cw, ch) = renderer.cell_size();
    let page = renderer.render_input(&text);
    // The cells' ground: the commonest pixel of the glyphs' boxes.
    let mut counts = std::collections::HashMap::new();
    for y in 0..ch {
        for &px in &page.pixels[y * page.width..y * page.width + 4 * cw] {
            *counts.entry(px).or_insert(0_usize) += 1;
        }
    }
    let bg = counts.into_iter().max_by_key(|&(_, n)| n).unwrap().0;
    let observed = renderer.glyph_ink_for(&text);
    let frame = observed.for_frame(&text, (cw, ch)).expect("stable keys");
    for lead in [0, 2] {
        let (key, coverage) = frame.coverage(0, lead).expect("a wide glyph's evidence");
        assert_eq!(key.cell_span, 2, "fitted to its two cells");
        assert!(coverage.max > 0);
        assert!(
            frame.coverage(0, lead + 1).is_none(),
            "its right half holds none"
        );
        let rows = frame.ink_rows(0, lead).expect("its ink rows");
        assert!(
            rows.iter().any(|&[_, _, x1]| x1 as usize > cw),
            "its ink reaches its right half"
        );
        let x0 = lead * cw;
        for y in 0..ch {
            let inked: Vec<usize> = (0..2 * cw)
                .filter(|&x| page.pixels[y * page.width + x0 + x] != bg)
                .collect();
            let span = rows
                .iter()
                .find(|&&[ry, _, _]| ry as usize == y)
                .map(|&[_, a, z]| (a as usize, z as usize));
            match (inked.first(), inked.last(), span) {
                (None, None, None) => {}
                (Some(&a), Some(&z), Some(span)) => {
                    assert_eq!((a, z + 1), span, "row {y} of the glyph at {lead}");
                }
                other => panic!("row {y} of the glyph at {lead}: {other:?}"),
            }
        }
    }
    let mut narrow = Renderer::from_system(24.0, Theme::default()).unwrap();
    narrow.debug_block_on_lazy_fallbacks();
    let none = with_narrow_evidence_only(|| narrow.glyph_ink_for(&text));
    let frame = none.for_frame(&text, (cw, ch)).expect("stable keys");
    assert!(
        (0..4).all(|col| frame.coverage(0, col).is_none()),
        "control: narrow evidence only holds none for a wide glyph"
    );
}

#[test]
fn actual_fallback_coverage_and_renderer_ownership_are_observed() {
    let mut renderer =
        Renderer::from_bytes(crate::embedded_symbols_font(), 24.0, Theme::default()).unwrap();
    assert_eq!(
        renderer.font.lookup_glyph_index('Z'),
        0,
        "the primary fixture cannot draw Z"
    );
    renderer.set_fallback_bytes(crate::embedded_font()).unwrap();
    renderer.debug_block_on_lazy_fallbacks();
    let text = input(b"Z");
    let observed = renderer.glyph_ink_for(&text);
    let frame = observed.for_frame(&text, renderer.cell_size()).unwrap();
    let (key, coverage) = frame.coverage(0, 0).unwrap();
    assert_eq!(
        key.source,
        FaceId::Fallback,
        "evidence comes from the actual fallback raster"
    );
    assert!(coverage.max > 0);

    let mut other =
        Renderer::from_bytes(crate::embedded_symbols_font(), 24.0, Theme::default()).unwrap();
    other.set_fallback_bytes(crate::embedded_font()).unwrap();
    other.debug_block_on_lazy_fallbacks();
    let peer = other.glyph_ink_for(&text);
    assert_ne!(
        frame.epoch(),
        peer.frame.as_ref().unwrap().epoch(),
        "equal local counters in a different renderer do not identify the same custody"
    );
}

#[test]
fn clipped_coverage_follows_line_height_baseline_and_activate_roundtrips() {
    let mut renderer =
        Renderer::from_bytes(crate::embedded_font(), 24.0, Theme::default()).unwrap();
    renderer.debug_block_on_lazy_fallbacks();
    let text = input(b"_g");
    let original_cell = renderer.cell_size();
    let original = renderer.glyph_ink_for(&text);
    let first_frame = original.for_frame(&text, original_cell).unwrap();
    let first_epoch = first_frame.epoch();
    let first = Arc::clone(&first_frame.rows[0].glyphs[0].as_ref().unwrap().1);

    renderer.set_line_height(1.5);
    let tall = renderer.glyph_ink_for(&text);
    let tall_frame = tall.for_frame(&text, renderer.cell_size()).unwrap();
    assert_ne!(first_epoch, tall_frame.epoch());
    assert!(original.for_frame(&text, renderer.cell_size()).is_none());
    assert!(!Arc::ptr_eq(
        &first,
        &tall_frame.rows[0].glyphs[0].as_ref().unwrap().1
    ));

    renderer.set_adjust_baseline(4);
    let shifted = renderer.glyph_ink_for(&text);
    let shifted_frame = shifted.for_frame(&text, renderer.cell_size()).unwrap();
    assert_ne!(tall_frame.epoch(), shifted_frame.epoch());
    assert!(!Arc::ptr_eq(
        &tall_frame.rows[0].glyphs[0].as_ref().unwrap().1,
        &shifted_frame.rows[0].glyphs[0].as_ref().unwrap().1
    ));

    renderer.set_adjust_baseline(0);
    renderer.set_line_height(1.0);
    renderer.activate_px(26.0);
    let other_size = renderer.glyph_ink_for(&text);
    assert!(other_size.for_frame(&text, renderer.cell_size()).is_some());
    renderer.activate_px(24.0);
    let restored = renderer.glyph_ink_for(&text);
    let restored_frame = restored.for_frame(&text, renderer.cell_size()).unwrap();
    assert_eq!(renderer.cell_size(), original_cell);
    assert_ne!(
        first_epoch,
        restored_frame.epoch(),
        "reset geometry does not revive a stale epoch"
    );
    let last = &restored_frame.rows[0].glyphs[0].as_ref().unwrap().1;
    assert!(!Arc::ptr_eq(&first, last));
    assert_eq!(
        *first, **last,
        "the same final raster geometry measures the same visible coverage"
    );
}

#[test]
fn pending_font_publication_cannot_lend_coverage_until_the_face_is_stable() {
    let mut renderer =
        Renderer::from_bytes(crate::embedded_font(), 24.0, Theme::default()).unwrap();
    renderer.debug_block_on_lazy_fallbacks();
    renderer.debug_land_lazy_parses_on_poll(false);
    let text = input(b"W");
    let stable = renderer.glyph_ink_for(&text);
    assert!(
        stable
            .for_frame(&text, renderer.cell_size())
            .unwrap()
            .coverage(0, 0)
            .is_some()
    );
    let (publish, pending) = std::sync::mpsc::channel();
    renderer.fallback_parse_rx = Some(pending);
    let waiting = renderer.glyph_ink_for(&text);
    assert!(
        waiting.for_frame(&text, renderer.cell_size()).is_none(),
        "a queued face can land during snapshot settling, so pending evidence grants no glass"
    );
    publish.send(Vec::new()).unwrap();
    let settled = renderer.glyph_ink_for(&text);
    assert!(
        settled
            .for_frame(&text, renderer.cell_size())
            .unwrap()
            .coverage(0, 0)
            .is_some(),
        "a completed ordinary face remains observable instead of permanent suppression"
    );
}

#[test]
fn an_unresolved_scalar_does_not_borrow_the_notdef_raster() {
    let mut renderer =
        Renderer::from_bytes(crate::embedded_font(), 24.0, Theme::default()).unwrap();
    renderer.debug_block_on_lazy_fallbacks();
    renderer.set_runtime_font_discovery(false);
    let mut text = input(b"WW");
    text.cells[0][1].ch = '\u{10ffff}';
    assert_eq!(renderer.font.lookup_glyph_index(text.cells[0][1].ch), 0);
    let ink = renderer.glyph_ink_for(&text);
    let frame = ink.for_frame(&text, renderer.cell_size()).unwrap();
    assert!(
        frame.coverage(0, 0).is_some(),
        "ordinary actual ink still has evidence"
    );
    assert!(
        frame.coverage(0, 1).is_none(),
        "a placeholder is not the unresolved glyph's evidence"
    );
}

#[test]
fn a_legacy_only_mapping_does_not_certify_the_unicode_notdef_raster() {
    // Keep all real mapping bodies and outlines. Making the Mac Roman record the
    // Unicode one leaves Omega only in the enumerated legacy map, while W still draws.
    let mut bytes = crate::font::with_cmap_record_last(crate::embedded_font(), 1, 0)
        .expect("the embedded face has a Mac Roman record");
    let u16_at = |data: &[u8], at: usize| u16::from_be_bytes(data[at..at + 2].try_into().unwrap());
    let u32_at = |data: &[u8], at: usize| {
        usize::try_from(u32::from_be_bytes(data[at..at + 4].try_into().unwrap())).unwrap()
    };
    let record = (0..usize::from(u16_at(&bytes, 4)))
        .map(|i| 12 + 16 * i)
        .find(|&at| &bytes[at..at + 4] == b"cmap")
        .unwrap();
    let cmap = u32_at(&bytes, record + 8);
    let unicode: Vec<bool> = {
        let face = aterm_font::Face::parse(&bytes, 0).unwrap();
        face.tables()
            .cmap
            .unwrap()
            .subtables
            .into_iter()
            .map(|subtable| subtable.is_unicode())
            .collect()
    };
    let mut mac_records = 0;
    for (i, was_unicode) in unicode.into_iter().enumerate() {
        let at = cmap + 4 + 8 * i;
        let was_mac = u16_at(&bytes, at) == 1 && u16_at(&bytes, at + 2) == 0;
        if was_mac || was_unicode {
            let platform = if was_mac {
                mac_records += 1;
                0u16
            } else {
                1u16
            };
            bytes[at..at + 2].copy_from_slice(&platform.to_be_bytes());
            bytes[at + 2..at + 4].copy_from_slice(&0u16.to_be_bytes());
        }
    }
    assert_eq!(mac_records, 1);
    let missing = '\u{03a9}';
    assert!(
        aterm_font::Face::parse(&bytes, 0)
            .unwrap()
            .glyph_index(missing)
            .is_none()
    );
    assert!(!crate::procedural::covers(missing) && !crate::procedural::covers_symbol(missing));
    for seq in [
        b"WW".as_slice(),
        b"\x1b[3mWW".as_slice(),
        b"\x1b[1mWW".as_slice(),
    ] {
        let mut renderer = Renderer::from_bytes(&bytes, 24.0, Theme::default()).unwrap();
        renderer.debug_force_fontdue();
        renderer.set_runtime_font_discovery(false);
        let mut shaping = renderer.text_shaping().clone();
        shaping.ligature_mode = aterm_types::text_shaping::LigatureMode::Disabled;
        renderer.set_text_shaping(shaping);
        assert_ne!(
            renderer.font.lookup_glyph_index(missing),
            0,
            "the legacy map names real ink"
        );
        assert_eq!(
            renderer.font.unicode_glyph_index(missing),
            0,
            "the raster's map has no glyph"
        );
        assert_eq!(
            renderer.font.rasterize(missing, 24.0),
            renderer.font.rasterize_indexed(0, 24.0)
        );
        let mut text = input(seq);
        text.cursor_visible = false;
        text.cells[0][1].ch = missing;
        assert!(
            actual_plan(&mut renderer, &text)
                .iter()
                .all(|glyph| *glyph == ColumnGlyph::PerCell)
        );
        let key = renderer.resolve_cell_key_for_span(None, &text.cells[0][1], 1);
        assert_eq!(
            (key.source, key.glyph_class),
            (FaceId::Primary, GlyphClass::Mono)
        );
        assert!(
            renderer
                .glyph_image(key)
                .bytes()
                .iter()
                .any(|&alpha| alpha > 0),
            "the actual tofu raster has ink"
        );
        let observed = renderer.glyph_ink_for(&text);
        let frame = observed.for_frame(&text, renderer.cell_size()).unwrap();
        assert!(
            frame
                .coverage(0, 0)
                .is_some_and(|(_, coverage)| coverage.max > 0),
            "ordinary Unicode ink stays admitted"
        );
        assert!(
            frame.coverage(0, 1).is_none(),
            "legacy coverage cannot certify a Unicode placeholder"
        );
    }
}
