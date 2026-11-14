// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_placed_mask_keeps_its_coverage_and_its_fraction() {
    let (sw, sh) = (10, 14);
    let src = vec![255u8; sw * sh];
    let centre = |(bytes, w, _, left, _): &(Vec<u8>, usize, usize, i32, i32)| {
        let (mut sum, mut weight) = (0.0f64, 0.0f64);
        for (i, &c) in bytes.iter().enumerate() {
            sum += f64::from(c) * (*left as f64 + (i % w) as f64 + 0.5);
            weight += f64::from(c);
        }
        (sum / weight, weight / 255.0)
    };
    let home = resample_mask_placed(&src, sw, sh, 0.7, 3.0, 2.0).expect("placed");
    let (home_x, home_area) = centre(&home);
    assert!(
        (home_area - 0.49 * (sw * sh) as f64).abs() < 0.5,
        "{home_area}"
    );
    for fraction in [0.25f32, 0.5, 0.9375] {
        let moved = resample_mask_placed(&src, sw, sh, 0.7, 3.0 + fraction, 2.0).expect("placed");
        let (x, area) = centre(&moved);
        assert!(
            (x - home_x - f64::from(fraction)).abs() < 0.02,
            "{fraction}: {x} vs {home_x}"
        );
        assert!((area - home_area).abs() < 0.5, "{fraction}: {area}");
    }
    assert!(resample_mask_placed(&src, sw, sh, 0.0, 0.0, 0.0).is_none());
    assert!(resample_mask_placed(&src, sw, sh + 1, 0.7, 0.0, 0.0).is_none());
}

#[cfg(feature = "embedded-font")]
#[test]
fn identical_primary_bytes_share_one_parsed_face() {
    let a = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("embedded font builds a renderer");
    let b = Renderer::from_bytes(embedded_font(), 20.0, Theme::default())
        .expect("embedded font builds a renderer at another px");
    assert!(
        std::sync::Arc::ptr_eq(&a.font, &b.font),
        "identical primary bytes must reuse one parse (px is a rasterize \
         argument, not a property of the parse)"
    );
    let (Some(ra), Some(rb)) = (
        a.shaping_primary_bytes.as_ref(),
        b.shaping_primary_bytes.as_ref(),
    ) else {
        panic!("from_bytes retains the primary bytes");
    };
    assert!(
        std::sync::Arc::ptr_eq(ra, rb),
        "source bytes share one copy"
    );
}

#[test]
fn a_dropped_face_is_released_not_retained() {
    let bytes = display_face_bytes("bubble").expect("bundled display face");
    let weak = {
        let (_, font) = shared_parsed_face(bytes, 0).expect("bundled face parses");
        std::sync::Arc::downgrade(&font)
    };
    assert!(
        weak.upgrade().is_none(),
        "the cache must not keep a face alive after its last holder drops"
    );
    let (_, again) = shared_parsed_face(bytes, 0).expect("re-parse after release");
    assert!(again.horizontal_line_metrics(16.0).is_some());
}

#[cfg(feature = "embedded-font")]
#[test]
fn font_interns_release_replaced_fallback_sources_and_parses() {
    let mut unique = embedded_font().to_vec();
    unique.extend_from_slice(b"font-intern-replaced-fallback-lifetime");
    let source = std::sync::Arc::new(unique);
    let source_weak = std::sync::Arc::downgrade(&source);
    let bytes = crate::font::FaceBytes::Vec(source);
    let face = FallbackFace::from_path_bytes(&bytes, None).expect("admitted face");
    let independent = crate::font::FaceBytes::Vec(std::sync::Arc::new(bytes.to_vec()));
    let peer = FallbackFace::from_path_bytes(&independent, None).expect("same face");
    assert!(face.bytes.ptr_eq(&peer.bytes), "live source dedup");
    let parsed = face.font.get(&face.bytes).expect("materialized face");
    let parsed_weak = std::sync::Arc::downgrade(parsed);
    assert!(
        std::sync::Arc::ptr_eq(parsed, peer.font.get(&peer.bytes).unwrap()),
        "live parsed-face dedup"
    );
    drop(independent);
    drop(bytes);
    drop(face);
    assert!(
        source_weak.upgrade().is_some(),
        "peer still owns the source"
    );
    assert!(parsed_weak.upgrade().is_some(), "peer still owns the parse");
    drop(peer);
    assert!(
        parsed_weak.upgrade().is_none(),
        "index must not own retired parse"
    );
    assert!(
        source_weak.upgrade().is_none(),
        "index must not own retired bytes"
    );

    let next = crate::font::FaceBytes::Static(embedded_font());
    let _next = intern_discovered_font_bytes(&next);
    let _ = interned_parsed_font_lookup(embedded_font());
    assert!(!DISCOVERED_FONT_BYTES.lock().unwrap().iter().any(|entry| {
        matches!(entry, crate::font::WeakFaceBytes::Vec(weak)
            if std::sync::Weak::ptr_eq(weak, &source_weak))
    }));
    assert!(
        !PARSED_FONT_INTERN
            .lock()
            .unwrap()
            .iter()
            .any(|entry| { std::sync::Weak::ptr_eq(entry, &parsed_weak) })
    );
}

#[cfg(feature = "embedded-font")]
#[test]
fn font_interns_follow_renderer_fallback_replacement() {
    let mut bytes = embedded_font().to_vec();
    bytes.extend_from_slice(b"font-intern-renderer-replacement");
    let mut first = Renderer::from_bytes(embedded_font(), 16.0, Theme::default()).unwrap();
    let mut second = Renderer::from_bytes(embedded_font(), 16.0, Theme::default()).unwrap();
    first.set_fallback_bytes(&bytes).unwrap();
    second.set_fallback_bytes(&bytes).unwrap();
    let face = &first.fallback_chain[0];
    let parsed = face.font.get(&face.bytes).unwrap();
    let weak = std::sync::Arc::downgrade(parsed);
    let glyph = parsed.lookup_glyph_index('A');
    assert_ne!(glyph, 0, "fixture has a real glyph");
    first.set_fallback_bytes(embedded_font()).unwrap();
    assert_eq!(weak.upgrade().unwrap().lookup_glyph_index('A'), glyph);
    assert!(
        !first.set_config_fallback_fonts(&[]),
        "unchanged config is inert"
    );
    second.set_fallback_bytes(embedded_font()).unwrap();
    assert!(
        weak.upgrade().is_none(),
        "neither renderer uses the old fallback"
    );
}

#[cfg(feature = "embedded-font")]
#[test]
fn font_interns_converge_on_racing_publish_and_allow_retry() {
    let mut bytes = embedded_font().to_vec();
    bytes.extend_from_slice(b"font-intern-publish-race");
    let barrier = std::sync::Barrier::new(2);
    let faces = std::thread::scope(|scope| {
        let spawn = || {
            scope.spawn(|| {
                intern_parsed_font_keyed(&bytes, || {
                    barrier.wait();
                    crate::font::FaceBytes::Vec(std::sync::Arc::new(bytes.clone()))
                })
                .unwrap()
            })
        };
        let first = spawn();
        let second = spawn();
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert!(
        faces[0].0.ptr_eq(&faces[1].0),
        "publish adopts winning bytes"
    );
    assert!(
        std::sync::Arc::ptr_eq(&faces[0].1, &faces[1].1),
        "one live parse"
    );
    let weak = std::sync::Arc::downgrade(&faces[0].1);
    drop(faces);
    assert!(
        weak.upgrade().is_none(),
        "racing publish owns no retired face"
    );
    let retry = intern_parsed_font(&bytes).expect("reparse after final release");
    assert_ne!(retry.1.lookup_glyph_index('A'), 0);
    for _ in 0..2 {
        assert!(intern_parsed_font(b"invalid-font-intern-retry").is_err());
    }
    assert!(interned_parsed_font_lookup(b"invalid-font-intern-retry").is_none());
}

#[cfg(feature = "embedded-font")]
#[test]
fn a_refused_face_leaves_no_bytes_in_the_discovery_store() {
    let patched = crate::font::FaceBytes::Vec(std::sync::Arc::new(
        patch_num_glyphs(embedded_symbols_font(), 0)
            .expect("the bundled symbols face has a maxp table"),
    ));
    assert!(
        !face_admissible(&patched, 0),
        "fixture guard: admission must actually refuse this face"
    );
    assert!(
        !discovered_font_bytes_contains(&patched),
        "fixture guard: this file must not already be in the store"
    );
    assert!(
        FallbackFace::from_path_bytes(&patched, Some("refused".into())).is_err(),
        "the fixture must actually be refused, or this proves nothing"
    );
    assert!(
        !discovered_font_bytes_contains(&patched),
        "a refused face must not enter the discovery index"
    );
}

#[test]
fn a_materialising_chain_face_adopts_its_own_byte_handle() {
    let bytes = crate::font::FaceBytes::Vec(std::sync::Arc::new(
        display_face_bytes("pixel")
            .expect("bundled display face")
            .to_vec(),
    ));
    let face = FallbackFace::from_path_bytes(&bytes, None)
        .expect("a bundled face is admissible on the discovery path");
    assert!(
        !face.font.is_materialised(),
        "the discovery path defers the parse (that is what makes the byte \
         handle reach the store before the parse does)"
    );
    let parsed = face
        .font
        .get(&face.bytes)
        .expect("the bundled face parses")
        .clone();
    let (stored_bytes, stored_font) =
        interned_parsed_font_lookup(&face.bytes).expect("the parse is published");
    assert!(
        std::sync::Arc::ptr_eq(&stored_font, &parsed),
        "the published parse must be the one the cell latched"
    );
    assert!(
        stored_bytes.ptr_eq(&face.bytes),
        "materialising must ADOPT the face's own byte handle — a second copy \
         of the file is megabytes on any real chain face"
    );
}

/// A text level's channel tables lay what it lays: over every ground of a sweep, each
/// channel of [`TextLevel::over`] is its table's entry for the ground's own channel, at
/// every eleventh level of a dark ink on paper and a light ink on a dark page, corrected
/// and not.
#[test]
fn a_text_level_s_channel_tables_lay_what_it_lays() {
    let mut read = 0;
    for (fg, bg) in [(0x002B_2D33, 0x00F6_F7F9), (0x00E0_1030, 0x0012_3456)] {
        for corrected in [true, false] {
            for t in (0..=255_u8).step_by(11).chain([255]) {
                let level = TextLevel::new(fg, bg, t, corrected);
                let tables = level.channel_tables();
                for dst in (0..0x0100_0000_u32).step_by(0x0001_0F35) {
                    let out = level.over(dst);
                    for (table, sh) in tables.iter().zip([16_u32, 8, 0]) {
                        assert_eq!(
                            u32::from(table[((dst >> sh) & 0xff) as usize]),
                            (out >> sh) & 0xff,
                            "level {t} of #{fg:06x} on #{bg:06x} over #{dst:06x}"
                        );
                    }
                    read += 1;
                }
            }
        }
    }
    assert!(read > 10_000, "fixture: {read} grounds read");
}

#[test]
fn blend_text_pre_equals_blend_text() {
    let colours: [u32; 10] = [
        0x0000_0000,
        0x00FF_FFFF,
        0x0080_8080,
        0x00FF_0000,
        0x0000_FF00,
        0x0000_00FF,
        0x0012_3456,
        0x00AB_CDEF,
        0x007F_7F7F,
        0x00E0_1030,
    ];
    let dsts: [u32; 4] = [0x0000_0000, 0xFF12_3456, 0x8000_00FF, 0x40AB_CDEF];
    for &fg in &colours {
        for &bg in &colours {
            let pre = TextBlendPre::new(fg, bg);
            let mut bank = AlphaMemoBank::default();
            let pair_dsts = [
                bg & 0x00FF_FFFF,
                bg & 0x00FF_FFFF,
                dsts[0],
                dsts[1],
                dsts[2],
                dsts[3],
            ];
            for dst in pair_dsts {
                for t in (1u8..=254).step_by(1) {
                    let reference = blend_text(dst, fg, bg, t, true);
                    let memo = bank.way_for(fg, bg);
                    let hoisted = blend_text_pre(dst, fg, t, memo);
                    assert_eq!(
                        memo.alpha(t).to_bits(),
                        correct_alpha_pre(&pre, f32::from(t) / 255.0).to_bits(),
                        "memoized alpha diverged: fg={fg:#08x} bg={bg:#08x} t={t}"
                    );
                    assert_eq!(
                        hoisted, reference,
                        "hoisted blend diverged: fg={fg:#08x} bg={bg:#08x} dst={dst:#08x} t={t}"
                    );
                }
            }
        }
    }
}

#[test]
fn alpha_memo_survives_way_eviction() {
    let pairs: Vec<(u32, u32)> = (0..ALPHA_MEMO_WAYS as u32 + 3)
        .map(|i| (0x0011_0000 * (i + 1), 0x0000_0033 * (i + 1)))
        .collect();
    let mut bank = AlphaMemoBank::default();
    for _ in 0..4 {
        for &(fg, bg) in &pairs {
            let pre = TextBlendPre::new(fg, bg);
            let memo = bank.way_for(fg, bg);
            for t in [1u8, 7, 64, 128, 200, 254] {
                assert_eq!(
                    memo.alpha(t).to_bits(),
                    correct_alpha_pre(&pre, f32::from(t) / 255.0).to_bits(),
                    "evicted-way memo diverged: fg={fg:#08x} bg={bg:#08x} t={t}"
                );
            }
        }
    }
}

#[test]
fn alpha_memo_generation_wrap_is_sound() {
    let mut memo = AlphaMemo::new();
    memo.rekey(0x00FF_0000, 0x0000_0000);
    let stale = memo.alpha(128);
    memo.epoch = u32::MAX;
    memo.slots[128] = (u64::from(u32::MAX) << 32) | u64::from(stale.to_bits());
    memo.rekey(0x0000_00FF, 0x00FF_FFFF);
    let pre = TextBlendPre::new(0x0000_00FF, 0x00FF_FFFF);
    assert_eq!(
        memo.alpha(128).to_bits(),
        correct_alpha_pre(&pre, 128.0 / 255.0).to_bits(),
        "generation wrap resurrected a stale slot"
    );
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn font_name_table_resolves_by_actual_family_name() {
    let mut found: Option<(std::path::PathBuf, String)> = None;
    for path in font_files() {
        if std::fs::metadata(&path).map_or(u64::MAX, |m| m.len()) > NAME_SCAN_MAX_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(face) = aterm_font::Face::parse(&bytes, 0) else {
            continue;
        };
        let names = face.names();
        let fam = (0..names.len()).find_map(|i| {
            let n = names.get(i)?;
            (n.name_id == aterm_font::name_id::FAMILY)
                .then(|| n.to_string())
                .flatten()
                .filter(|s| !normalize_family(s).is_empty())
        });
        if let Some(fam) = fam {
            found = Some((path, fam));
            break;
        }
    }
    let (path, family) = found.expect("the system font directories hold a named font");
    let want = normalize_family(&family);
    let bytes = std::fs::read(&path).expect("re-read the chosen font");
    assert!(
        best_face(&bytes, Some(&want), FaceTarget::REGULAR).is_some(),
        "a font's own family name ({family:?}) must match its name table"
    );
    assert!(
        best_face(
            &bytes,
            Some("definitely not a real font family zzq"),
            FaceTarget::REGULAR
        )
        .is_none(),
        "a bogus family must not match"
    );
    let resolved = resolve_by_name_table(&want, FaceTarget::REGULAR)
        .expect("name-table scan must find the family");
    assert!(
        std::path::Path::new(&resolved.path).is_file(),
        "resolved path exists: {}",
        resolved.path
    );
    let rb = std::fs::read(&resolved.path).expect("read resolved font");
    let face = aterm_font::Face::parse(&rb, resolved.index).expect("the resolved face parses");
    assert!(
        face_name_match(&face, &want).is_some(),
        "the name-table scan must only return a face that advertises {family:?}"
    );
    assert!(
        resolve_font_family(&family, FaceTarget::REGULAR).is_some(),
        "a real family name must resolve to a font file"
    );
}

#[test]
fn fontdue_path_renders_the_resolved_weight_instance() {
    use crate::variation::WGHT_TAG;
    let (bytes, wght, ch, _) = crate::variation::inked_wght_font();
    let (wmin, wmax) = (wght.min, wght.max);
    let mut r = Renderer::from_system(40.0, Theme::default()).expect(NO_FONT);
    r.set_primary_font(&bytes)
        .expect("the variable face is accepted as primary");
    r.debug_force_fontdue();

    let cov = |r: &mut Renderer, w: f32| -> Option<Vec<u8>> {
        r.set_font_variations(&[(WGHT_TAG, w)], 0.0);
        let k = r.glyph_key(ch);
        match r.glyph_image(k) {
            GlyphImage::Mono { bytes, .. } => Some(bytes.clone()),
            _ => None,
        }
    };
    let light = cov(&mut r, wmin).expect("the light instance is a mono glyph");
    let heavy = cov(&mut r, wmax).expect("the heavy instance is a mono glyph");
    assert!(
        light != heavy,
        "the fontdue render path must produce DIFFERENT coverage for wght {wmin} vs {wmax} \
         (FONT-2: the resolved instance reaches the raster)"
    );
}

use aterm_core::terminal::Terminal;

#[test]
fn premul_and_add_sat_are_exact() {
    assert_eq!(premul_rgb(0x00FF_FFFF, 0), 0);
    assert_eq!(premul_rgb(0x0012_34AB, 255), 0x0012_34AB);
    assert_eq!(premul_rgb(0x00FF_FF00, 128), {
        let m = (255u32 * 128 + 127) / 255;
        (m << 16) | (m << 8)
    });
    assert_eq!(add_sat(0x0010_2030, 0x0001_0203), 0x0011_2233);
    assert_eq!(add_sat(0x00F0_F0F0, 0x0020_2020), 0x00FF_FFFF);
    let (d, a, b) = (0x0030_4050u32, 0x0011_2233u32, 0x0044_5566u32);
    assert_eq!(add_sat(add_sat(d, a), b), add_sat(add_sat(d, b), a));
    assert_eq!(add_sat(0x00AB_CDEF, 0), 0x00AB_CDEF);
}

#[test]
fn over_premul_is_add_sat_at_zero_alpha() {
    for d in 0..=255u32 {
        for c in 0..=255u32 {
            let (dst, premul) = (d << 16 | d << 8 | d, c << 16 | c << 8 | c);
            assert_eq!(
                over_premul(dst, premul, 0),
                add_sat(dst, premul),
                "over_premul is not add_sat at alpha 0 for dst {d} src {c}"
            );
        }
    }
    assert_eq!(
        over_premul(0x0012_34AB, 0x0040_2010, 0),
        add_sat(0x0012_34AB, 0x0040_2010)
    );
    assert_eq!(over_premul(0x00FF_FFFF, 0x0012_34AB, 255), 0x0012_34AB);
    assert_eq!(over_premul(0x0000_0000, 0x0012_34AB, 255), 0x0012_34AB);
    assert_eq!(over_premul(0x00AB_CDEF, 0, 0), 0x00AB_CDEF);
    assert_eq!(over_premul(0x00AB_CDEF, 0, 255), 0);
    for a in 0..255u8 {
        let (lo, hi) = (
            over_premul(0x00C0_C0C0, 0x0020_2020, a),
            over_premul(0x00C0_C0C0, 0x0020_2020, a + 1),
        );
        assert!(
            hi <= lo,
            "over_premul must not brighten as alpha rises: {a} gave {lo:#08x}, \
             {} gave {hi:#08x}",
            a + 1
        );
    }
}
#[test]
fn compute_dirty_rows_tracks_moving_glow() {
    let mut term = Terminal::new(4, 8);
    let mut prev = term.cell_frame(4, 8);
    let mut cur = term.cell_frame(4, 8);
    let (r, r2) = (1u16, 3u16);

    prev.cursor_glow_add = vec![GlowQuad {
        row: r,
        x: 0,
        y: 0,
        w: 1,
        h: 1,
        color: 0x0010_2030,
        alpha: 0,
        color2: 0x0010_2030,
        alpha2: 0,
    }];
    cur.cursor_glow_add = vec![GlowQuad {
        row: r2,
        x: 0,
        y: 0,
        w: 1,
        h: 1,
        color: 0x0010_2030,
        alpha: 0,
        color2: 0x0010_2030,
        alpha2: 0,
    }];

    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path, not FullRepaint");
    };

    assert!(
        dirty[r as usize],
        "prev overlay row {r} must be marked dirty (rebuilt fresh)"
    );
    assert!(
        dirty[r2 as usize],
        "cur overlay row {r2} must be marked dirty (new light composited)"
    );
    assert!(d.glow_changed, "moving glow quad must set glow_changed");
    assert!(
        !d.is_gate_hit(),
        "a changed glow overlay must NOT hit the dirty-gate"
    );
}

#[test]
fn compute_dirty_rows_tracks_moving_trail() {
    let mut term = Terminal::new(4, 8);
    let mut prev = term.cell_frame(4, 8);
    let mut cur = term.cell_frame(4, 8);
    let (r, r2) = (1usize, 3usize);
    prev.cursor_trail = vec![TrailCell {
        row: r,
        col: 2,
        alpha: 120,
    }];
    prev.cursor_trail_color = 0x0050_FA7B;
    cur.cursor_trail = vec![TrailCell {
        row: r2,
        col: 2,
        alpha: 120,
    }];
    cur.cursor_trail_color = 0x0050_FA7B;

    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path, not FullRepaint");
    };
    assert!(
        dirty[r],
        "prev trail row {r} must be marked dirty (restored to bg)"
    );
    assert!(
        dirty[r2],
        "cur trail row {r2} must be marked dirty (comet painted)"
    );
    assert!(
        d.trail_changed,
        "a moving trail cell must set trail_changed"
    );
    assert!(
        !d.is_gate_hit(),
        "a changed trail overlay must NOT hit the dirty-gate"
    );

    let mut settled_prev = term.cell_frame(4, 8);
    let mut settled_cur = term.cell_frame(4, 8);
    settled_prev.cursor_trail = vec![TrailCell {
        row: r,
        col: 2,
        alpha: 90,
    }];
    settled_cur.cursor_trail = vec![TrailCell {
        row: r,
        col: 2,
        alpha: 90,
    }];
    let mut dirty2 = Vec::new();
    if let DirtyDecision::Rows(d2) = compute_dirty_rows(
        &settled_prev,
        &settled_cur,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty2,
    ) {
        assert!(
            !d2.trail_changed,
            "an unchanged trail leaves trail_changed false"
        );
        assert!(d2.is_gate_hit(), "a settled trail is a 0% idle gate-hit");
    } else {
        panic!("identical settled frames must take the row path");
    }
}

#[test]
fn compute_dirty_rows_marks_a_row_whose_bidi_map_changed() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    let mut term = Terminal::new(4, 8);
    term.process(b"\r\n\r\naaaa\r\n");
    let mut prev = term.cell_frame(4, 8);
    prev.selection
        .start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
    prev.selection.update_selection(2, 0, SelectionSide::Right);
    let mut cur = prev.clone();
    cur.visual_to_logical[2] = vec![3, 2, 1, 0];
    assert!(prev.cells == cur.cells && prev.selection == cur.selection);
    assert!(!prev.selection_contains_cell(2, 3, false, false));
    assert!(cur.selection_contains_cell(2, 3, false, false));

    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("a map-only change takes the row-damage path");
    };
    assert!(dirty[2], "the reordered row repaints");
    assert!(
        !dirty[0] && !dirty[1],
        "rows without a map change stay clean"
    );
    assert!(d.any_dirty);
}

#[test]
fn compute_dirty_rows_clip_only_move_repaints_old_and_new_selection_spans() {
    use aterm_core::render::SelectionClip;
    use aterm_core::selection::{SelectionSide, SelectionType};

    let mut term = Terminal::new(4, 8);
    let mut prev = term.cell_frame(4, 8);
    let mut selection = aterm_core::selection::TextSelection::new();
    selection.start_selection(1, 0, SelectionSide::Left, SelectionType::Simple);
    selection.update_selection(2, 7, SelectionSide::Right);
    selection.complete_selection();
    prev.selection = selection;
    prev.selection_clip = Some(SelectionClip::new(1, 3, 0, 3));
    let mut cur = prev.clone();
    cur.selection_clip = Some(SelectionClip::new(1, 3, 4, 8));

    let mut dirty = Vec::new();
    let DirtyDecision::Rows(decision) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("a clip-only selection move must use row damage");
    };
    assert!(
        dirty[0],
        "the predecessor of the first clipped row repaints glyph overshoot"
    );
    assert!(
        dirty[1] && dirty[2],
        "both rows whose old/new clipped column spans differ must repaint"
    );
    assert!(!dirty[3], "a row outside both clips stays cached");
    assert!(decision.any_dirty && !decision.is_gate_hit());
}

#[test]
fn compute_dirty_rows_clip_crossing_wide_lead_repaints() {
    use aterm_core::render::SelectionClip;
    use aterm_core::selection::{SelectionSide, SelectionType};

    let mut term = Terminal::new(3, 8);
    let mut prev = term.cell_frame(3, 8);
    prev.cells[1].resize(8, term.implicit_blank_render_cell());
    prev.cells[1][5].wide = true;
    let mut selection = aterm_core::selection::TextSelection::new();
    selection.start_selection(1, 5, SelectionSide::Left, SelectionType::Block);
    selection.update_selection(1, 5, SelectionSide::Right);
    selection.complete_selection();
    prev.selection = selection;
    prev.selection_clip = Some(SelectionClip::new(1, 2, 4, 6));
    let mut cur = prev.clone();
    cur.selection_clip = Some(SelectionClip::new(1, 2, 5, 6));

    assert!(prev.selection_contains_cell(1, 4, true, false));
    assert!(
        !cur.selection_contains_cell(1, 4, true, false),
        "negative-control precondition: the moved clip removes the snapped lead"
    );

    let mut dirty = Vec::new();
    let DirtyDecision::Rows(decision) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("a clip-only wide-selection change must use row damage");
    };
    assert!(
        dirty[0] && dirty[1],
        "the changed row and upward-glyph predecessor must repaint"
    );
    assert!(!dirty[2], "an unrelated row stays cached");
    assert!(decision.any_dirty && !decision.is_gate_hit());
}

#[cfg(test)]
fn one_row_selection(row: i32, lo: u16, hi: u16) -> aterm_core::selection::TextSelection {
    use aterm_core::selection::{SelectionSide, SelectionType};

    let mut selection = aterm_core::selection::TextSelection::new();
    selection.start_selection(row, lo, SelectionSide::Left, SelectionType::Simple);
    selection.update_selection(row, hi, SelectionSide::Right);
    selection.complete_selection();
    selection
}

#[test]
fn compute_dirty_rows_marks_a_dragged_pane_selection_a_sibling_hull_hides() {
    use aterm_core::render::{PaneSelection, SelectionClip};

    let mut term = Terminal::new(4, 16);
    let left_clip = SelectionClip::new(1, 2, 0, 8);
    let right_clip = SelectionClip::new(1, 2, 8, 16);
    let right_pane = PaneSelection {
        selection: one_row_selection(1, 8, 15),
        clip: right_clip,
        bg: aterm_core::render::COLOR_UNSET,
        fg: aterm_core::render::COLOR_UNSET,
        inactive: true,
    };

    let mut prev = term.cell_frame(4, 16);
    prev.selections = vec![
        PaneSelection {
            selection: one_row_selection(1, 0, 7),
            clip: left_clip,
            bg: aterm_core::render::COLOR_UNSET,
            fg: aterm_core::render::COLOR_UNSET,
            inactive: false,
        },
        right_pane.clone(),
    ];
    let mut cur = prev.clone();
    cur.selections[0].selection = one_row_selection(1, 0, 3);

    assert_eq!(
        prev.selection_row_span(1),
        cur.selection_row_span(1),
        "precondition: the sibling pane pins the hull, so the span diff sees nothing"
    );
    assert_ne!(
        prev.selection_row_keys(1).collect::<Vec<_>>(),
        cur.selection_row_keys(1).collect::<Vec<_>>(),
        "the per-entry key must see the drag the hull hides"
    );
    assert!(
        prev.selection_contains_cell(1, 5, false, false)
            && !cur.selection_contains_cell(1, 5, false, false),
        "precondition: column 5 really does stop painting as selected"
    );

    let mut dirty = Vec::new();
    let DirtyDecision::Rows(decision) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("a selection-only change must stay on the row-damage path");
    };
    assert!(dirty[1], "the dragged row must repaint");
    assert!(
        dirty[0],
        "and its predecessor, for upward glyph-ink overshoot"
    );
    assert!(!dirty[2] && !dirty[3], "unrelated rows stay cached");
    assert!(decision.any_dirty && !decision.is_gate_hit());
}

#[test]
fn compute_dirty_rows_marks_a_per_pane_selection_recolour() {
    use aterm_core::render::{PaneSelection, SelectionClip};

    let mut term = Terminal::new(3, 16);
    let mut prev = term.cell_frame(3, 16);
    prev.selections = vec![
        PaneSelection {
            selection: one_row_selection(1, 0, 7),
            clip: SelectionClip::new(1, 2, 0, 8),
            bg: aterm_core::render::COLOR_UNSET,
            fg: aterm_core::render::COLOR_UNSET,
            inactive: false,
        },
        PaneSelection {
            selection: one_row_selection(1, 8, 15),
            clip: SelectionClip::new(1, 2, 8, 16),
            bg: aterm_core::render::COLOR_UNSET,
            fg: aterm_core::render::COLOR_UNSET,
            inactive: true,
        },
    ];
    let mut cur = prev.clone();
    cur.selections[1].bg = 0x0011_2233;

    assert_eq!(
        prev.selection_row_span(1),
        cur.selection_row_span(1),
        "precondition: a recolour moves no span"
    );
    assert_eq!(
        prev.selection_bg, cur.selection_bg,
        "precondition: the SCALAR colour is untouched — it belongs to no pane here"
    );

    let mut dirty = Vec::new();
    let DirtyDecision::Rows(decision) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("a selection recolour must stay on the row-damage path");
    };
    assert!(
        dirty[1] && dirty[0],
        "the recoloured row and its predecessor"
    );
    assert!(!dirty[2], "an unselected row stays cached");
    assert!(decision.any_dirty && !decision.is_gate_hit());

    let DirtyDecision::Rows(settled) = compute_dirty_rows(
        &prev,
        &prev.clone(),
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("an unchanged frame must stay on the row-damage path");
    };
    assert!(!settled.any_dirty && settled.is_gate_hit());
}

#[test]
fn compute_dirty_rows_selection_color_change_covers_upward_glyph_spill() {
    use aterm_core::selection::{SelectionSide, SelectionType};

    let mut term = Terminal::new(4, 8);
    let mut prev = term.cell_frame(4, 8);
    let mut selection = aterm_core::selection::TextSelection::new();
    selection.start_selection(1, 2, SelectionSide::Left, SelectionType::Simple);
    selection.update_selection(2, 4, SelectionSide::Right);
    selection.complete_selection();
    prev.selection = selection;
    let mut cur = prev.clone();
    cur.selection_bg = 0x0011_2233;
    cur.selection_fg = 0x00aa_bbcc;

    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("selection-colour changes must stay on the row-damage path");
    };
    assert!(
        dirty[0],
        "the predecessor of a selected row must repaint upward glyph ink"
    );
    assert!(dirty[1] && dirty[2], "every selected row must repaint");
    assert!(!dirty[3], "an unselected row after the range stays cached");
    assert!(d.any_dirty && !d.is_gate_hit());

    let mut no_selection_prev = term.cell_frame(4, 8);
    let mut no_selection_cur = no_selection_prev.clone();
    no_selection_prev.selection_bg = 0x0001_0203;
    no_selection_cur.selection_bg = 0x0004_0506;
    let mut empty_dirty = Vec::new();
    let DirtyDecision::Rows(empty) = compute_dirty_rows(
        &no_selection_prev,
        &no_selection_cur,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut empty_dirty,
    ) else {
        panic!("an unused selection colour is not a full-repaint event");
    };
    assert!(
        empty_dirty.iter().all(|dirty| !dirty),
        "no selected pixels exist to repaint"
    );
    assert!(empty.is_gate_hit());
}

#[test]
fn damage_tracks_pane_local_channels() {
    let mut term = Terminal::new(4, 8);
    let mut prev = term.cell_frame(4, 8);
    prev.cursor_visible = false;
    let mut cur = prev.clone();
    prev.line_size_spans = vec![Vec::new(); 4];
    cur.line_size_spans = prev.line_size_spans.clone();
    prev.line_size_spans[2].push(LineSizeSpan::new(0, 4, LineSize::SingleWidth));
    cur.line_size_spans[2].push(LineSizeSpan::new(0, 4, LineSize::DoubleWidth));

    let mut dirty = Vec::new();
    let DirtyDecision::Rows(decision) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("double-width remains row-local and must use row damage");
    };
    assert!(decision.any_dirty);
    assert!(dirty[2], "a changed pane-local DEC mode dirties its row");

    cur.line_size_spans[2][0].line_size = LineSize::DoubleHeightTop;
    assert_eq!(
        compute_dirty_rows(
            &prev,
            &cur,
            false,
            None,
            false,
            None,
            RowBands::new(0, 16),
            &mut dirty
        ),
        DirtyDecision::FullRepaint,
        "a pane-local DECDHL segment has the same cross-row hazard as a full row"
    );

    let mut bg_prev = term.cell_frame(4, 8);
    bg_prev.cursor_visible = false;
    bg_prev.default_bg_spans = vec![Vec::new(); 4];
    bg_prev.default_bg_spans[2].push(DefaultBgSpan::new(0, 4, 0x0011_2233));
    let mut bg_cur = bg_prev.clone();
    bg_cur.default_bg_spans[2][0].default_bg = 0x0044_5566;
    let DirtyDecision::Rows(decision) = compute_dirty_rows(
        &bg_prev,
        &bg_cur,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("pane-local default changes do not recolor the window padding");
    };
    assert!(decision.any_dirty);
    assert_eq!(
        dirty,
        vec![false, false, true, false],
        "only the composed row carrying changed pane provenance repaints"
    );
}

#[test]
fn a_stationary_cursor_restyle_ungates_its_row() {
    let mut term = Terminal::new(4, 8);
    term.process(b"\x1b[2 q\x1b[3;4H");
    let prev = term.cell_frame(4, 8);
    type Restyle = fn(&mut RenderInput);
    let cases: [(&str, Restyle); 3] = [
        ("OSC 12 colour", |f| f.cursor_color = 0x0011_2233),
        ("fill override", |f| {
            f.cursor_fill_override = Some(0x00AA_BBCC);
        }),
        ("effect shape", |f| {
            f.cursor_effect_style_override = Some(CursorStyle::Bolt);
        }),
    ];
    for (name, restyle) in cases {
        let mut cur = prev.clone();
        restyle(&mut cur);
        let mut dirty = Vec::new();
        let DirtyDecision::Rows(d) = compute_dirty_rows(
            &prev,
            &cur,
            true,
            None,
            true,
            None,
            RowBands::new(0, 16),
            &mut dirty,
        ) else {
            panic!("{name}: identical geometry must take the row-damage path");
        };
        assert!(d.cursor_changed, "{name}: the drawn cursor changed");
        assert!(!d.is_gate_hit(), "{name}: a restyle must not gate-hit");
        assert!(dirty[cur.cursor_row], "{name}: the cursor row repaints");
    }

    // Converse (`dirty-rows-exact`): a stationary visible cursor on row 0 stays clean when row 3
    // changes, and a block cursor over an upward-overshooting glyph (`A\u{0302}`) stays exact on
    // CPU when the row above it changes.
    let mut stat_term = Terminal::new(4, 8);
    stat_term.process(b"\x1b[2 q\x1b[1;1H");
    let stat_prev = stat_term.cell_frame(4, 8);
    stat_term.process(b"\x1b[4;1HX\x1b[1;1H");
    let stat_cur = stat_term.cell_frame(4, 8);
    let mut stat_dirty = Vec::new();
    let DirtyDecision::Rows(sd) = compute_dirty_rows(
        &stat_prev,
        &stat_cur,
        true,
        None,
        true,
        None,
        RowBands::new(0, 16),
        &mut stat_dirty,
    ) else {
        panic!("single-row edit with stationary cursor must take row-damage path");
    };
    assert!(!sd.cursor_changed, "stationary cursor did not change");
    assert_eq!(
        stat_dirty,
        vec![false, false, false, true],
        "stationary visible cursor row 0 must not be marked dirty when only row 3 changed"
    );

    let mut r = renderer();
    let mut wc = WindowCpu::new();
    let mut over_term = Terminal::new(4, 12);
    over_term.process("\x1b[2 q\x1b[3;2HA\u{0302}\x1b[3;2H".as_bytes());
    let f0 = over_term.cell_frame(4, 12);
    let _ = r.render_input_cached(&mut wc, &f0);
    over_term.process(b"\x1b[2;1HMW\x1b[3;2H");
    let f1 = over_term.cell_frame(4, 12);
    let cached_px = r.render_input_cached(&mut wc, &f1).pixels().to_vec();
    let fresh_px = r.render_input(&f1).pixels;
    assert_eq!(
        cached_px, fresh_px,
        "block cursor cutout over upward-overshooting glyph must match fresh render after row-above edit"
    );
}

#[test]
fn compute_dirty_rows_selection_block_growth() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    let mut term = Terminal::new(4, 8);
    let template = term.cell_frame(4, 8);
    let mut sel = aterm_core::selection::TextSelection::new();
    sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Block);
    sel.update_selection(2, 3, SelectionSide::Right);
    let mut prev = template.clone();
    prev.selection = sel.clone();
    let mut wider = sel.clone();
    wider.update_selection(2, 4, SelectionSide::Right);
    let mut cur = template.clone();
    cur.selection = wider;

    let mut dirty = Vec::new();
    let DirtyDecision::Rows(_) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("block column growth must stay on the row-damage path");
    };
    assert!(
        dirty[0] && dirty[1] && dirty[2],
        "every block-covered row's span widened → all repaint"
    );
    assert!(!dirty[3], "a row outside the block must NOT repaint");
}

#[test]
fn image_eq_memo_matches_derived_row_equality() {
    use aterm_core::grid::extra::{
        ImageData, ImageFormat, ImageRef, ImageScaling, KittyPlacementTag,
    };
    use std::num::NonZeroU32;
    use std::sync::Arc;
    let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
    let mk = |bytes: Vec<u8>, cols: u16, z: i32| {
        Arc::new(ImageData {
            bytes,
            format: ImageFormat::Png,
            cols,
            rows: 4,
            z_index: z,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        })
    };
    let base = mk(payload.clone(), 8, 0);
    let same_bytes = mk(payload.clone(), 8, 0);
    let mut first_flip = payload.clone();
    first_flip[0] ^= 1;
    let diff_first = mk(first_flip, 8, 0);
    let mut last_flip = payload.clone();
    *last_flip.last_mut().unwrap() ^= 1;
    let diff_last = mk(last_flip, 8, 0);
    let diff_meta = mk(payload.clone(), 9, 0);
    let diff_z = mk(payload.clone(), 8, -1);
    let row = |img: &Arc<ImageData>, col0: usize, tile: u16| {
        (0..3usize)
            .map(|i| {
                (
                    col0 + i,
                    ImageRef {
                        image: img.clone(),
                        cell_row: tile,
                        cell_col: u16::try_from(i).unwrap(),
                        kitty: None,
                    },
                )
            })
            .collect::<Vec<_>>()
    };
    let a = row(&base, 2, 0);
    let tagged = |serial: u32| {
        let mut r = a.clone();
        for (_, iref) in &mut r {
            iref.kitty = Some(KittyPlacementTag {
                image_id: 1,
                placement_id: 0,
                image_serial: NonZeroU32::MIN,
                serial: NonZeroU32::new(serial).unwrap(),
            });
        }
        r
    };
    let untagged = |r: &[(usize, ImageRef)]| {
        r.iter()
            .map(|(c, iref)| {
                (
                    *c,
                    ImageRef {
                        kitty: None,
                        ..iref.clone()
                    },
                )
            })
            .collect::<Vec<_>>()
    };
    let cases = [
        Vec::new(),
        a.clone(),
        row(&base, 3, 0),
        row(&base, 2, 1),
        row(&same_bytes, 2, 0),
        row(&diff_first, 2, 0),
        row(&diff_last, 2, 0),
        row(&diff_meta, 2, 0),
        row(&diff_z, 2, 0),
        tagged(7),
        a[..2].to_vec(),
    ];
    for (i, b) in cases.iter().enumerate() {
        let derived = untagged(&a) == untagged(b);
        let mut memo = ImageEqMemo::default();
        assert_eq!(
            memo.rows_eq(&a, b),
            derived,
            "case {i}: memoized verdict must match the derived equality"
        );
        assert_eq!(
            memo.rows_eq(&a, b),
            derived,
            "case {i}: memo read-back must return the identical verdict"
        );
    }
    assert!(ImageEqMemo::default().rows_eq(&a, &row(&same_bytes, 2, 0)));
    assert!(ImageEqMemo::default().rows_eq(&tagged(7), &tagged(8)));
    assert_ne!(tagged(7), tagged(8), "the tag is part of derived equality");
}

#[test]
fn retransmitted_identical_image_gate_hits_and_changed_payload_does_not() {
    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef, ImageScaling};
    use std::sync::Arc;
    let mut term = Terminal::new(6, 10);
    let template = term.cell_frame(6, 10);
    let payload: Vec<u8> = (0..4096u32)
        .map(|i| (i.wrapping_mul(7) % 256) as u8)
        .collect();
    let mk = |bytes: Vec<u8>| {
        Arc::new(ImageData {
            bytes,
            format: ImageFormat::Png,
            cols: 4,
            rows: 2,
            z_index: 0,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        })
    };
    let fill = |input: &mut RenderInput, img: &Arc<ImageData>| {
        for (r, row) in input.images[..2].iter_mut().enumerate() {
            for c in 0..4usize {
                row.push((
                    c,
                    ImageRef {
                        image: img.clone(),
                        cell_row: u16::try_from(r).unwrap(),
                        cell_col: u16::try_from(c).unwrap(),
                        kitty: None,
                    },
                ));
            }
        }
    };
    let mut prev = template.clone();
    fill(&mut prev, &mk(payload.clone()));
    let mut cur = template.clone();
    fill(&mut cur, &mk(payload.clone()));
    assert!(
        is_unchanged_frame(&prev, false, None, &cur, false, None, RowBands::new(0, 16)),
        "a byte-identical re-transmit must still gate-hit"
    );
    let mut flipped = payload;
    flipped[1000] ^= 0x40;
    let mut changed = template.clone();
    fill(&mut changed, &mk(flipped));
    assert!(
        !is_unchanged_frame(
            &prev,
            false,
            None,
            &changed,
            false,
            None,
            RowBands::new(0, 16)
        ),
        "a genuine payload change must still repaint"
    );
    let mut dirty = Vec::new();
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &changed,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty,
    ) else {
        panic!("an image payload change stays on the row-damage path");
    };
    assert!(d.any_dirty, "changed payload must mark rows dirty");
    assert!(dirty[0] && dirty[1], "both covered rows repaint");
    assert!(!dirty[2], "uncovered rows stay clean");
}

#[test]
fn wallpaper_base_layer_and_damaged_parity() {
    let mut r = renderer();
    let (rows, cols) = (3usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b]11;#111318\x07");
    term.process(b"hi");
    term.process(b"\x1b[2;1H\x1b[44m \x1b[0m");
    let (w, h) = r.frame_size(rows, cols);
    let mut rgba = vec![0u8; w * h * 4];
    for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        px[0] = if (i % w) % 2 == 0 { 0x40 } else { 0x80 };
        px[1] = 0x20;
        px[2] = 0x60;
        px[3] = 0xFF;
    }
    let wallpaper = std::sync::Arc::new(SceneAtlas {
        width: w as u32,
        height: h as u32,
        rgba,
        version: 1,
    });
    let mut wc = WindowCpu::new();
    let mut input = term.cell_frame(rows, cols);
    input.wallpaper = Some(std::sync::Arc::clone(&wallpaper));
    let _ = r.render_input_cached(&mut wc, &input);
    let pixels = wc.cache.as_ref().expect("rendered").pixels.clone();
    let tr = w - 1;
    let expect_tr = if (w - 1) % 2 == 0 {
        0x0040_2060
    } else {
        0x0080_2060
    };
    assert_eq!(pixels[tr], expect_tr, "a blank cell must show the backdrop");
    let (cw, ch) = r.cell_size();
    let (cx, cy) = (r.pad() + cw / 2, r.grid_top() + ch + ch / 2);
    let colored = pixels[cy * w + cx];
    let cell_bg = rgb_to_u32(input.cells[1][0].bg);
    assert_eq!(
        colored, cell_bg,
        "an explicit SGR background must paint opaquely over the wallpaper"
    );
    assert_ne!(colored & 0x00ff_ffff, 0x0040_2060 & 0x00ff_ffff);
    let (bx, by) = (r.pad() + cw * 5 + cw / 2, r.grid_top() + ch * 2 + ch / 2);
    let expect_blank = if (bx % 2) == 0 {
        0x0040_2060
    } else {
        0x0080_2060
    };
    assert_eq!(
        pixels[by * w + bx],
        expect_blank,
        "a default-bg cell must show the backdrop"
    );
    term.process(b"\x1b[3;1Hmt");
    for step in 0..2 {
        if step == 1 {
            term.process(b"\r\nscrolled\r\nagain");
        }
        let mut input = term.cell_frame(rows, cols);
        input.wallpaper = Some(std::sync::Arc::clone(&wallpaper));
        let _ = r.render_input_cached(&mut wc, &input);
        let damaged = wc.cache.as_ref().expect("rendered").pixels.clone();
        let mut fresh = WindowCpu::new();
        let _ = r.render_input_cached(&mut fresh, &input);
        let full = fresh.cache.as_ref().expect("rendered").pixels.clone();
        assert_eq!(
            damaged, full,
            "wallpaper damaged frame (step {step}) must match a full render"
        );
    }
}

#[test]
fn wallpaper_identity_change_forces_full_repaint() {
    let mut term = Terminal::new(3, 8);
    term.process(b"steady");
    let prev = term.cell_frame(3, 8);
    let mut cur = term.cell_frame(3, 8);
    let wallpaper = std::sync::Arc::new(SceneAtlas {
        width: 4,
        height: 4,
        rgba: vec![0xFF; 64],
        version: 1,
    });
    cur.wallpaper = Some(std::sync::Arc::clone(&wallpaper));
    let mut dirty = Vec::new();
    assert!(
        matches!(
            compute_dirty_rows(
                &prev,
                &cur,
                false,
                None,
                false,
                None,
                RowBands::new(0, 16),
                &mut dirty
            ),
            DirtyDecision::FullRepaint
        ),
        "attaching a wallpaper must force the full path"
    );
    let mut prev2 = prev.clone();
    prev2.wallpaper = Some(std::sync::Arc::clone(&wallpaper));
    assert!(
        matches!(
            compute_dirty_rows(
                &prev2,
                &cur,
                false,
                None,
                false,
                None,
                RowBands::new(0, 16),
                &mut dirty
            ),
            DirtyDecision::Rows(_)
        ),
        "the SAME published Arc must stay on the row-damage path"
    );
}

#[test]
fn default_bg_change_forces_full_repaint() {
    let mut term = Terminal::new(4, 8);
    let prev = term.cell_frame(4, 8);
    let mut cur = term.cell_frame(4, 8);
    cur.default_bg = 0x0012_3456;
    let mut dirty = Vec::new();
    assert!(
        matches!(
            compute_dirty_rows(
                &prev,
                &cur,
                false,
                None,
                false,
                None,
                RowBands::new(0, 16),
                &mut dirty
            ),
            DirtyDecision::FullRepaint
        ),
        "a default-bg change must take the full-repaint path so the padding re-clears"
    );
}

#[test]
fn a_cached_frame_that_carried_a_glow_is_not_scroll_blittable() {
    let mut term = Terminal::new(6, 12);
    for i in 0..40 {
        term.process(format!("line {i}\r\n").as_bytes());
    }
    let mut prev = term.cell_frame(6, 12);
    term.scroll_display(3);
    let cur = term.cell_frame(6, 12);

    let mut cached = RenderInput::empty();
    cached.clone_damage_cache_from(&prev);
    let mut dirty = Vec::new();
    let rescued = scroll_blit_plan(
        &cached,
        &cur,
        false,
        None,
        false,
        None,
        RowBands::new(0, 16),
        &mut dirty,
    );
    assert!(
        rescued.is_some(),
        "a rigid scroll of two pure-grid frames must be rescued, or this \
         test is measuring something else"
    );

    prev.cursor_glow_add.push(GlowQuad {
        row: 2,
        x: 0,
        y: 32,
        w: 8,
        h: 16,
        color: 0x0020_4060,
        alpha: 0,
        color2: 0x0020_4060,
        alpha2: 0,
    });
    prev.refresh_cursor_effect_damage();
    let mut cached = RenderInput::empty();
    cached.clone_damage_cache_from(&prev);
    assert!(
        cached.cursor_glow_add.is_empty(),
        "the cache drops the payload — this emptiness IS the trap, and a \
         cache that stopped dropping it would make this test vacuous"
    );
    assert_eq!(
        cached.cursor_glow_add_damage.was_empty(),
        Some(false),
        "the metadata is what remembers"
    );
    assert_eq!(
        scroll_blit_plan(
            &cached,
            &cur,
            false,
            None,
            false,
            None,
            RowBands::new(0, 16),
            &mut dirty
        ),
        None,
        "a previous frame with a glow must not be blitted rigidly"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn styled_cells_use_real_collection_faces_menlo() {
    let mut r = Renderer::from_system_with_family(Some("Menlo"), 32.0, Theme::default())
        .expect("macOS ships Menlo");
    assert_eq!(
        r.primary_path.as_deref(),
        Some("/System/Library/Fonts/Menlo.ttc"),
        "the family Menlo resolves to the system Menlo.ttc"
    );
    fn mono(r: &mut Renderer, key: GlyphKey) -> Vec<u8> {
        match r.glyph_image(key) {
            GlyphImage::Mono { bytes, .. } => bytes.clone(),
            _ => panic!("styled cell should be a Mono glyph"),
        }
    }
    let bi = StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0);
    let k = r.glyph_key_styled('A', StyleBits::BOLD);
    let bold_a = mono(&mut r, k);
    let k = r.glyph_key_styled('A', StyleBits::ITALIC);
    let _ = mono(&mut r, k);
    let k = r.glyph_key_styled('A', bi);
    let _ = mono(&mut r, k);
    assert_eq!(
        r.debug_styled_face_indices(),
        [Some(1), Some(2), Some(3)],
        "Menlo Bold/Italic/Bold-Italic must load from .ttc collection indices 1/2/3"
    );
    let k = r.glyph_key('A');
    let reg_a = mono(&mut r, k);
    assert!(bold_a.iter().any(|&b| b != 0), "bold 'A' must not be blank");
    assert_ne!(reg_a, bold_a, "bold 'A' must differ from regular 'A'");
    let k = r.glyph_key_styled('é', StyleBits::BOLD);
    let bold_eacute = mono(&mut r, k);
    let k = r.glyph_key_styled('È', StyleBits::BOLD);
    let bold_egrave_cap = mono(&mut r, k);
    assert!(
        bold_eacute.iter().any(|&b| b != 0),
        "bold 'é' must not be blank"
    );
    assert_ne!(
        bold_eacute, bold_egrave_cap,
        "bold 'é' must not collapse to bold 'È' (styled path must route by Unicode gid)"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn coretext_placement_binds_to_the_pure_policy() {
    let bytes = std::fs::read("/System/Library/Fonts/Menlo.ttc").expect("macOS ships Menlo.ttc");
    let face = aterm_font::Face::parse(&bytes, 0).expect("parse Menlo face 0");
    let ct = macos_coretext::CtFont::new(
        crate::font::FaceBytes::Vec(std::sync::Arc::new(bytes.clone())),
        0,
        13.7,
        &[],
    )
    .expect("CtFont builds from .ttc");
    let mut fractional_seen = 0usize;
    for ch in ['A', 'g', 'j', '.', '(', 'W', 'i', '@'] {
        let Some(gid) = face.glyph_index(ch).map(|g| g.0) else {
            continue;
        };
        let (ix, iy, iw, ih) = ct.ink_rect(gid);
        if !(iw > 0.0 && ih > 0.0) {
            continue;
        }
        for thicken in [false, true] {
            let pad = if thicken { 2.0_f64 } else { 1.0_f64 };
            let rg = ct.rasterize(gid, thicken).expect("rasterize");
            let (bx, by) = (ix - pad, iy - pad);
            assert_eq!(
                (rg.xmin, rg.ymin),
                (ct_pen_and_bearing(bx).1, ct_pen_and_bearing(by).1),
                "'{ch}' (thicken={thicken}): bearings must be floor(padded bbox origin)"
            );
            assert_eq!(
                (rg.width, rg.height),
                (ct_padded_extent(iw, pad), ct_padded_extent(ih, pad)),
                "'{ch}' (thicken={thicken}): raster box must be the phase-headroom extent"
            );
            assert!(
                rg.bytes.iter().any(|&b| b > 0),
                "'{ch}' (thicken={thicken}): fractional placement must not lose the ink"
            );
        }
        if bx_fract(ix - 1.0) || bx_fract(iy - 1.0) {
            fractional_seen += 1;
        }
    }
    assert!(
        fractional_seen > 0,
        "expected at least one fractional designed bearing at 13.7px"
    );

    fn bx_fract(b: f64) -> bool {
        b.floor() != b
    }
}

#[test]
fn decode_png_expands_indexed_palette_to_rgba() {
    let mut png_bytes = Vec::new();
    {
        let mut enc = aterm_png::Encoder::new(&mut png_bytes, 2, 1);
        enc.set_color(aterm_png::ColorType::Indexed);
        enc.set_depth(aterm_png::BitDepth::Eight);
        enc.set_palette(vec![255, 0, 0, 0, 255, 0]);
        enc.set_trns(vec![255, 0]);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&[0u8, 1u8]).unwrap();
    }
    let (rgba, w, h) = decode_png_rgba8(&png_bytes).expect("indexed PNG must decode to RGBA");
    assert_eq!((w, h), (2, 1));
    assert_eq!(&rgba[0..4], &[255, 0, 0, 255], "palette idx0 → opaque red");
    assert_eq!(rgba[7], 0, "palette idx1 (tRNS) → transparent");
}

#[cfg(target_os = "macos")]
#[test]
fn apple_emoji_strike_walk_preserves_coverage_and_prefers_small_strikes() {
    let bytes = std::fs::read("/System/Library/Fonts/Apple Color Emoji.ttc")
        .expect("macOS ships Apple Color Emoji");
    let face = aterm_font::Face::parse(&bytes, 0).expect("Apple Color Emoji parses");
    let sbix = face
        .tables()
        .sbix
        .expect("Apple Color Emoji is an sbix face");
    let target = 24usize;
    let (mut served, mut small_picks) = (0usize, 0usize);
    for gid in 0..face.number_of_glyphs() {
        let gid = aterm_font::GlyphId(gid);
        let old = face.glyph_raster_image(gid, u16::MAX);
        let new = pick_glyph_raster(&face, gid, target);
        if let Some(old) = old {
            served += 1;
            let new = new.as_ref().unwrap_or_else(|| {
                panic!("gid {gid:?}: served at u16::MAX but lost by the strike walk")
            });
            let carrying: Vec<u16> = sbix
                .strikes
                .into_iter()
                .filter(|s| s.get(gid).is_some())
                .map(|s| s.pixels_per_em)
                .collect();
            let want = select_strike_ppem(&carrying, target as u16)
                .expect("a served glyph has a carrying strike");
            assert_eq!(
                new.pixels_per_em, want,
                "gid {gid:?}: walk picked {} but the law says {want} \
                 (carrying {carrying:?}, target {target})",
                new.pixels_per_em
            );
            if new.pixels_per_em < old.pixels_per_em {
                small_picks += 1;
            }
        }
    }
    assert!(
        served > 0,
        "no glyph served at all — the sweep proved nothing"
    );
    assert!(
        small_picks > 0,
        "every pick equalled the largest strike — the W10 walk changed nothing"
    );
}

fn renderer() -> Renderer {
    Renderer::from_system(16.0, Theme::default()).expect(NO_FONT)
}

#[test]
fn framebuffer_dimensions_reject_overflow_and_excess_storage() {
    assert_eq!(checked_frame_size_px(2, 3, 4, 5, 6, 7), Ok((24, 29)));
    assert_eq!(
        checked_frame_size_px(8192, 8192, 1, 1, 0, 0),
        Ok((8192, 8192))
    );
    assert!(checked_frame_size_px(8192, 8193, 1, 1, 0, 0).is_err());
    for (rows, cols, cw, ch, pad, head) in [
        (1, usize::MAX, 2, 1, 0, 0),
        (usize::MAX, 1, 1, 2, 0, 0),
        (1, 1, 1, 1, usize::MAX, 0),
        (1, 1, 1, 1, 0, usize::MAX),
        (0, usize::MAX, 1, 1, 0, 0),
    ] {
        assert!(checked_frame_size_px(rows, cols, cw, ch, pad, head).is_err());
    }
}

#[test]
fn a_rejected_frame_keeps_the_previous_pixels_and_damage() {
    let mut r = Renderer::from_bytes(embedded_font(), 16.0, Theme::default()).unwrap();
    let mut term = Terminal::new(2, 8);
    term.process(b"kept");
    let input = term.cell_frame(2, 8);
    let mut window = WindowCpu::new();
    let view = r.try_render_input_cached(&mut window, &input).unwrap();
    let dimensions = (view.width(), view.height());
    let pixels = view.pixels().to_vec();
    let damage = window.last_damage();
    let mut oversized = input.clone();
    oversized.cols = usize::MAX;
    assert!(r.try_render_input_cached(&mut window, &oversized).is_err());
    assert_eq!(window.frame_pixels(), pixels);
    assert_eq!(window.last_damage(), damage);
    let view = r.try_render_input_cached(&mut window, &input).unwrap();
    assert_eq!((view.width(), view.height()), dimensions);
    assert_eq!(view.pixels(), pixels);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_owned_snapshot_translates_only_after_its_font_settle_passes() {
    let fresh = || {
        let mut renderer = Renderer::from_bytes(
            include_bytes!("../assets/bundled/CascadiaMonoPL-Regular.ttf"),
            20.0,
            Theme::default(),
        )
        .unwrap();
        renderer.fallback_paths = vec![asset_path("bundled/NotoSansMath-Regular.ttf")];
        renderer.fallback_user_prefix = 0;
        renderer
    };
    let mut term = Terminal::new(4, 8);
    term.process(b"\x1b[?25l");
    for row in 0..4 {
        term.process(
            format!(
                "\x1b[{};1H\x1b[48;2;{};40;70m        ",
                row + 1,
                40 + row * 40
            )
            .as_bytes(),
        );
    }
    let mut input = term.cell_frame(4, 8);
    input.grid_top_row = 0;
    input.grid_bot_row = 4;
    input.scroll_frac_px = 1;
    let mut oracle = fresh();
    let _ = oracle.glyph_key_text('\u{21bb}');
    oracle.debug_block_on_lazy_fallbacks();
    let expected = oracle.render_input(&input);

    let mut renderer = fresh();
    let epoch = renderer.font_epoch;
    let _ = renderer.glyph_key_text('\u{21bb}');
    assert!(renderer.fallback_parse_rx.is_some());
    renderer.debug_land_lazy_parses_on_poll(true);
    let actual = renderer.try_render_input(&input).unwrap();
    assert!(
        renderer.font_epoch > epoch,
        "the first pass lands a new font epoch"
    );
    assert_eq!(
        (actual.width, actual.height),
        (expected.width, expected.height)
    );
    assert_eq!(actual.pixels, expected.pixels);
}

#[test]
fn blend_linear_light_matches_reference() {
    let reference = |bg: u32, fg: u32, t: u8| -> u32 {
        if t == 0 {
            return bg & 0x00ff_ffff;
        }
        if t == 255 {
            return fg & 0x00ff_ffff;
        }
        let a = t as f32 / 255.0;
        let ch = |sh: u32| {
            let b = srgb_to_linear(((bg >> sh) & 0xff) as f32 / 255.0);
            let f = srgb_to_linear(((fg >> sh) & 0xff) as f32 / 255.0);
            (linear_to_srgb(f * a + b * (1.0 - a)) * 255.0).round() as u32
        };
        (ch(16) << 16) | (ch(8) << 8) | ch(0)
    };
    let pairs = [
        (0x000000u32, 0xffffffu32),
        (0xffffff, 0x000000),
        (0x111318, 0xd0d0d0),
        (0x33415e, 0xffffff),
        (0xff5555, 0x282a36),
    ];
    for (bg, fg) in pairs {
        assert_eq!(blend(bg, fg, 0), bg & 0x00ff_ffff, "t=0 must be exact bg");
        assert_eq!(
            blend(bg, fg, 255),
            fg & 0x00ff_ffff,
            "t=255 must be exact fg"
        );
        for t in (0u16..=255).step_by(1) {
            let got = blend(bg, fg, t as u8);
            let want = reference(bg, fg, t as u8);
            for sh in [16, 8, 0] {
                let g = ((got >> sh) & 0xff) as i32;
                let w = ((want >> sh) & 0xff) as i32;
                assert!(
                    (g - w).abs() <= 1,
                    "blend({bg:#08x},{fg:#08x},{t}) ch{sh}: LUT {g} vs ref {w} (>1 LSB)"
                );
            }
        }
    }
}

#[test]
fn opaque_blend_is_foreground_for_every_background() {
    for v in 0..=255u32 {
        let fg = (v << 16) | (v << 8) | v;
        for &bg in &[0x000000u32, 0xFFFFFF, 0x808080, 0x123456, fg ^ 0x00FF_FFFF] {
            assert_eq!(
                blend(bg, fg, 255),
                fg & 0x00FF_FFFF,
                "blend({bg:#08x}, {fg:#08x}, 255) must equal the fg (masked)"
            );
        }
    }
    for &fg in &[0x102030u32, 0xFEDCBA, 0xFF00FF, 0x00FF00, 0xABCDEF] {
        assert_eq!(blend(0x778899, fg, 255), fg & 0x00FF_FFFF);
    }
}

#[test]
fn blit_mono_1x_matches_scaled_oracle() {
    let coverage = |w: usize, h: usize| -> Vec<u8> {
        (0..w * h)
            .map(|n| [0u8, 255, 1, 128, 254, 200][n % 6])
            .collect()
    };
    let mut cases = 0usize;
    for &(w, h) in &[(1usize, 1usize), (3, 2), (5, 4), (8, 3)] {
        let bytes = coverage(w, h);
        for &stride in &[6usize, 10] {
            let rows = 6usize;
            for &px_len in &[stride * rows, stride * 2 + 3, 1, 0] {
                for gx0 in [
                    -3i32,
                    -1,
                    0,
                    1,
                    stride as i32 - 2,
                    stride as i32,
                    stride as i32 + 2,
                ] {
                    for gy0 in [
                        -2i32,
                        -1,
                        0,
                        1,
                        rows as i32 - 1,
                        rows as i32,
                        rows as i32 + 1,
                    ] {
                        for &(c0, c1) in &[(0i32, rows as i32), (1, rows as i32 - 1)] {
                            for &(cx0, cx1) in &[
                                (i32::MIN, i32::MAX),
                                (1, stride as i32 - 2),
                                (-2, 2),
                                (3, 3),
                            ] {
                                for &color in &[0x00FF8040u32, 0x00000000, 0x00FFFFFF] {
                                    for (bg, corrected) in
                                        [(0u32, false), (0x0011_2233, true), (0x0000_0000, true)]
                                    {
                                        let mut a = vec![0x0011_2233u32; px_len];
                                        let mut b = vec![0x0011_2233u32; px_len];
                                        let mut bank_a = AlphaMemoBank::default();
                                        let mut bank_b = AlphaMemoBank::default();
                                        blit_mono_1x(
                                            &mut a,
                                            stride,
                                            gx0,
                                            gy0,
                                            w,
                                            h,
                                            &bytes,
                                            color,
                                            c0,
                                            c1,
                                            cx0,
                                            cx1,
                                            corrected.then(|| bank_a.way_for(color, bg)),
                                        );
                                        blit_mono_scaled(
                                            &mut b,
                                            stride,
                                            gx0,
                                            gy0,
                                            w,
                                            h,
                                            &bytes,
                                            color,
                                            1,
                                            1,
                                            c0,
                                            c1,
                                            cx0,
                                            cx1,
                                            corrected.then(|| bank_b.way_for(color, bg)),
                                        );
                                        assert_eq!(
                                            a, b,
                                            "mono 1x != scaled @ w={w} h={h} stride={stride} \
                                             px_len={px_len} gx0={gx0} gy0={gy0} \
                                             clip=({c0},{c1}) xclip=({cx0},{cx1}) \
                                             color={color:#08x} bg={bg:#08x} \
                                             corrected={corrected}"
                                        );
                                        cases += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        cases > 2000,
        "differential coverage too thin ({cases} cases)"
    );
}

#[test]
fn blit_rgba_1x_matches_scaled_oracle() {
    let texels = |w: usize, h: usize| -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 4);
        for n in 0..w * h {
            let a = [0u8, 255, 1, 200, 128, 254][n % 6];
            v.extend_from_slice(&[
                (n as u8).wrapping_mul(7),
                (n as u8).wrapping_mul(13),
                n as u8,
                a,
            ]);
        }
        v
    };
    let mut cases = 0usize;
    for &(w, h) in &[(1usize, 1usize), (3, 2), (5, 4), (8, 3)] {
        let bytes = texels(w, h);
        for &stride in &[6usize, 10] {
            let rows = 6usize;
            for &px_len in &[stride * rows, stride * 2 + 3, 1, 0] {
                for gx0 in [
                    -3i32,
                    -1,
                    0,
                    1,
                    stride as i32 - 2,
                    stride as i32,
                    stride as i32 + 2,
                ] {
                    for gy0 in [
                        -2i32,
                        -1,
                        0,
                        1,
                        rows as i32 - 1,
                        rows as i32,
                        rows as i32 + 1,
                    ] {
                        for &(c0, c1) in &[(0i32, rows as i32), (1, rows as i32 - 1)] {
                            for &(cx0, cx1) in &[
                                (i32::MIN, i32::MAX),
                                (1, stride as i32 - 2),
                                (-2, 2),
                                (3, 3),
                            ] {
                                let mut a = vec![0x0011_2233u32; px_len];
                                let mut b = vec![0x0011_2233u32; px_len];
                                blit_rgba_1x(
                                    &mut a, stride, gx0, gy0, w, h, &bytes, c0, c1, cx0, cx1,
                                );
                                blit_rgba_scaled(
                                    &mut b, stride, gx0, gy0, w, h, &bytes, 1, 1, c0, c1, cx0, cx1,
                                );
                                assert_eq!(
                                    a, b,
                                    "rgba 1x != scaled @ w={w} h={h} stride={stride} \
                                     px_len={px_len} gx0={gx0} gy0={gy0} clip=({c0},{c1}) \
                                     xclip=({cx0},{cx1})"
                                );
                                cases += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        cases > 700,
        "differential coverage too thin ({cases} cases)"
    );
}

#[test]
fn placement_raster_size_holds_every_footprint_to_the_budget() {
    let (px, side) = (MAX_PLACEMENT_RASTER_PX, MAX_PLACEMENT_RASTER_SIDE);
    for fp in [
        (1, 1),
        (7680, 4320),
        (8192, 4096),
        (side, 2048),
        (2048, side),
    ] {
        assert_eq!(placement_raster_size(fp.0, fp.1), fp, "{fp:?} is within");
    }
    for (fw, fh) in [
        (8193, 4096),
        (22_320, 22_892),
        (side + 1, 1),
        (1, 65_535 * 600),
        (65_535 * 600, 65_535 * 600),
        (3, side * 4),
    ] {
        let (w, h) = placement_raster_size(fw, fh);
        assert!(
            w >= 1 && h >= 1 && w <= fw && h <= fh,
            "{fw}x{fh} -> {w}x{h}"
        );
        assert!(
            w <= side && h <= side && w * h <= px,
            "{fw}x{fh} -> {w}x{h}"
        );
        let skew = (w as u128 * fh as u128).abs_diff(h as u128 * fw as u128);
        assert!(
            w == 1 || h == 1 || skew <= (fw + fh) as u128,
            "{fw}x{fh} -> {w}x{h} is not uniform"
        );
    }
}

#[test]
fn decode_placement_scales_an_over_budget_footprint_down_uniformly() {
    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageScaling};
    let raw = |w: u16, h: u16, px: &[[u8; 4]], scaling, offset| ImageData {
        bytes: px.concat(),
        format: ImageFormat::RawRgba8 {
            width: w,
            height: h,
        },
        cols: 1,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: offset,
        scaling,
        source_rect: None,
    };
    let (red, blue) = ([0xFF, 0, 0, 0xFF], [0, 0, 0xFF, 0xFF]);
    let at = |r: &PlacementRaster, x: usize, y: usize| {
        let i = (y * r.w + x) * 4;
        [r.rgba[i], r.rgba[i + 1], r.rgba[i + 2], r.rgba[i + 3]]
    };

    let fp = (40_000, 4);
    let wide = decode_placement(
        &raw(2, 1, &[red, blue], ImageScaling::Stretch, (0, 0)),
        fp.0,
        fp.1,
    )
    .expect("decodes");
    assert_eq!((wide.w, wide.h), placement_raster_size(fp.0, fp.1));
    assert_eq!(wide.rgba.len(), wide.w * wide.h * 4);
    assert_eq!(at(&wide, 0, 0), red);
    assert_eq!(at(&wide, wide.w - 1, wide.h - 1), blue);

    let fp = (40_000, 40_000);
    let exact = decode_placement(
        &raw(4, 4, &[red; 16], ImageScaling::PixelExact, (0, 0)),
        fp.0,
        fp.1,
    )
    .expect("decodes");
    assert_eq!((exact.w, exact.h), placement_raster_size(fp.0, fp.1));
    assert!(exact.w * exact.h <= MAX_PLACEMENT_RASTER_PX);
    assert_eq!(at(&exact, 0, 0), red);
    assert_eq!(at(&exact, 1, 0)[3], 0, "the rest of the footprint is clear");
    assert_eq!(at(&exact, 0, 1)[3], 0, "the rest of the footprint is clear");

    let offset = decode_placement(
        &raw(4, 4, &[red; 16], ImageScaling::PixelExact, (20_000, 0)),
        fp.0,
        fp.1,
    )
    .expect("decodes");
    let rx = placement_raster_coord(20_000, fp.0, offset.w);
    assert_eq!(at(&offset, rx, 0), red);
    assert_eq!(
        at(&offset, rx - 1, 0)[3],
        0,
        "the strip before the offset is clear"
    );
}

#[test]
fn decode_raw_rgba8_rejects_short_buffer() {
    use aterm_core::grid::extra::ImageFormat;
    let short = vec![0xFFu8; 4];
    assert!(
        decode_image_to_footprint(
            &short,
            ImageFormat::RawRgba8 {
                width: 4,
                height: 4
            },
            8,
            8,
            aterm_core::grid::extra::ImageScaling::Fit,
            None
        )
        .is_none(),
        "a too-short RawRgba8 buffer must decode to None"
    );
}

fn next_frame(ic: &mut ImageCache) {
    ic.begin_frame(std::iter::empty());
}

fn mk_image_arc() -> std::sync::Arc<aterm_core::grid::extra::ImageData> {
    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageScaling};
    std::sync::Arc::new(ImageData {
        bytes: Vec::new(),
        format: ImageFormat::Unknown,
        cols: 1,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    })
}

#[test]
fn image_cache_releases_source_payload_but_pins_its_identity() {
    use std::sync::Arc;
    for rgba in [Vec::new(), vec![0xcc; 4]] {
        let mut source = mk_image_arc();
        Arc::get_mut(&mut source).unwrap().bytes = vec![0x55; 16 * 1024 * 1024];
        let observed = Arc::downgrade(&source);
        let identity = Arc::as_ptr(&source);
        let decoded_bytes = rgba.len();
        let mut cache = ImageCache::default();
        cache.put(source.clone(), 1, 1, PlacementRaster { w: 1, h: 1, rgba });
        assert_eq!(
            Arc::strong_count(&source),
            1,
            "cache owns no source payload"
        );
        assert_eq!(cache.get(&source, 1, 1).unwrap().rgba.len(), decoded_bytes);
        assert_eq!(cache.peek(&source, 1, 1).unwrap().rgba.len(), decoded_bytes);
        assert!(cache.get(&source, 2, 1).is_none());
        assert!(cache.peek(&source, 2, 1).is_none());

        let historical_owner = source.clone();
        drop(source);
        assert!(observed.upgrade().is_some());
        drop(historical_owner);
        assert!(observed.upgrade().is_none(), "source Vec must be released");
        drop(observed);
        let pinned = cache.entries.values().next().expect("one entry");
        assert_eq!(pinned._pin.as_ptr(), identity);
        let replacement = mk_image_arc();
        assert_ne!(
            Arc::as_ptr(&replacement),
            identity,
            "weak cache key pins address"
        );
        assert!(cache.get(&replacement, 1, 1).is_none());
        assert!(cache.peek(&replacement, 1, 1).is_none());
        assert_eq!((cache.entries.len(), cache.bytes), (1, decoded_bytes));
        eprintln!(
            "CPU image cache: 16777216 source bytes released; {decoded_bytes} decoded bytes retained"
        );
    }
}

#[test]
fn image_cache_byte_budget_evicts_lru_and_admits_oversized_entry() {
    let quarter = ImageCache::MAX_BYTES / 4 + 1;
    let mk_decoded = |fill: u8, len: usize| PlacementRaster {
        w: 1,
        h: 1,
        rgba: vec![fill; len],
    };
    let (a, b, c, d) = (
        mk_image_arc(),
        mk_image_arc(),
        mk_image_arc(),
        mk_image_arc(),
    );
    let mut ic = ImageCache::default();
    next_frame(&mut ic);
    ic.put(a.clone(), 1, 1, mk_decoded(1, quarter));
    ic.put(b.clone(), 1, 1, mk_decoded(2, quarter));
    ic.put(c.clone(), 1, 1, mk_decoded(3, quarter));
    next_frame(&mut ic);
    ic.put(d.clone(), 1, 1, mk_decoded(4, quarter));
    assert!(
        ic.get(&a, 1, 1).is_none(),
        "LRU entry evicted at byte budget"
    );
    assert!(ic.get(&b, 1, 1).is_some(), "B retained");
    assert!(ic.get(&c, 1, 1).is_some(), "C retained");
    assert!(ic.get(&d, 1, 1).is_some(), "new entry inserted");
    next_frame(&mut ic);
    let huge = mk_image_arc();
    ic.put(huge.clone(), 1, 1, mk_decoded(5, ImageCache::MAX_BYTES + 1));
    assert!(
        ic.get(&huge, 1, 1).is_some(),
        "over-budget entry cached alone"
    );
    assert!(ic.get(&b, 1, 1).is_none(), "prior entries evicted");
    assert_eq!(ic.entries.len(), 1, "exactly the oversized entry remains");
}

#[test]
fn image_cache_keeps_the_frame_working_set_past_the_entry_cap() {
    let n = ImageCache::MAX_ENTRIES + 17;
    let images: Vec<_> = (0..n).map(|_| mk_image_arc()).collect();
    let tiny = || PlacementRaster {
        w: 1,
        h: 1,
        rgba: vec![0; 4],
    };
    let mut ic = ImageCache::default();
    let mut decodes = 0;
    for _frame in 0..3 {
        next_frame(&mut ic);
        for image in &images {
            let _ = ic.get_or_insert_with(image, 1, 1, || {
                decodes += 1;
                tiny()
            });
        }
    }
    assert_eq!(decodes, n, "each image is decoded once, not once per frame");

    next_frame(&mut ic);
    let more: Vec<_> = (0..2_000).map(|_| mk_image_arc()).collect();
    for image in &more {
        ic.put(image.clone(), 1, 1, tiny());
    }
    assert_eq!(ic.entries.len(), more.len(), "the frame keeps all it drew");
    assert!(ic.get(&more[0], 1, 1).is_some(), "its first image included");
    assert!(
        ic.get(&images[0], 1, 1).is_none(),
        "an earlier frame's images go"
    );
    next_frame(&mut ic);
    ic.put(mk_image_arc(), 1, 1, tiny());
    assert_eq!(ic.entries.len(), ImageCache::MAX_ENTRIES);
}

fn decode_cache_frame_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        DecodeCacheFrameWorkingSet {
            const Buggy = 0;
            const MaxTick = 7;
            var tick = 0;
            var frame_start = 0;
            var a_res = 0;
            var a_tick = 0;
            var a_used = 0;
            var b_res = 0;
            var b_tick = 0;
            var b_used = 0;
            action Begin when (tick + 1 <= MaxTick) {
                tick = if Buggy == 1 { tick } else { tick + 1 };
                frame_start = tick + 1;
                a_used = 0;
                b_used = 0;
            }
            action UseA when (a_res == 1 && tick + 1 <= MaxTick) {
                tick = if a_tick == tick { tick } else { tick + 1 };
                a_tick = if a_tick == tick { a_tick } else { tick + 1 };
                a_used = 1;
            }
            action PutA when (a_res == 0 && tick + 1 <= MaxTick) {
                tick = tick + 1;
                a_tick = tick + 1;
                a_res = 1;
                a_used = 1;
            }
            action EvictA when (a_res == 1 && a_tick + 1 <= frame_start) {
                a_res = 0;
            }
            action UseB when (b_res == 1 && tick + 1 <= MaxTick) {
                tick = if b_tick == tick { tick } else { tick + 1 };
                b_tick = if b_tick == tick { b_tick } else { tick + 1 };
                b_used = 1;
            }
            action PutB when (b_res == 0 && tick + 1 <= MaxTick) {
                tick = tick + 1;
                b_tick = tick + 1;
                b_res = 1;
                b_used = 1;
            }
            action EvictB when (b_res == 1 && b_tick + 1 <= frame_start) {
                b_res = 0;
            }
            invariant FrameKeepsWhatItUses:
                (a_used == 0 || a_res == 1) && (b_used == 0 || b_res == 1);
        }
    }
}

#[test]
fn image_cache_conforms_to_the_frame_working_set_model() {
    let model = decode_cache_frame_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    let model = aterm_spec::interp::with_consts(&model, &[("MaxTick", 1_000)]);
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let big = || PlacementRaster {
        w: 1,
        h: 1,
        rgba: vec![0; ImageCache::MAX_BYTES / 2 + 1],
    };
    let images = [mk_image_arc(), mk_image_arc()];
    let names = [
        ("A", "UseA", "PutA", "EvictA"),
        ("B", "UseB", "PutB", "EvictB"),
    ];
    let resident = |ic: &ImageCache| images.each_ref().map(|i| ic.peek(i, 1, 1).is_some());
    let conforms = |ic: &ImageCache, state: &std::collections::BTreeMap<&str, i64>| {
        let tick = |image| ic.entries[&ImageCache::key(image, 1, 1)].tick as i64;
        let now = resident(ic);
        state["tick"] == ic.tick as i64
            && state["frame_start"] == ic.frame_start as i64
            && state["a_res"] == i64::from(now[0])
            && state["b_res"] == i64::from(now[1])
            && (!now[0] || state["a_tick"] == tick(&images[0]))
            && (!now[1] || state["b_tick"] == tick(&images[1]))
    };
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut fired = std::collections::BTreeSet::new();
    let mut buggy_rejected = 0;
    for _walk in 0..400 {
        let mut ic = ImageCache::default();
        let mut state = model.init_state();
        let mut buggy_state = buggy.init_state();
        let mut buggy_ok = true;
        for _step in 0..24 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let before = resident(&ic);
            let mut steps = Vec::new();
            if seed.is_multiple_of(3) {
                ic.begin_frame(std::iter::empty());
                steps.push("Begin");
            } else {
                let i = (seed / 3 % 2) as usize;
                let (_, use_, put, _) = names[i];
                if ic.get(&images[i], 1, 1).is_some() {
                    steps.push(use_);
                } else {
                    ic.put(images[i].clone(), 1, 1, big());
                    steps.push(put);
                    let after = resident(&ic);
                    for (j, &(_, _, _, evict)) in names.iter().enumerate() {
                        if before[j] && !after[j] {
                            steps.push(evict);
                        }
                    }
                }
            }
            for &step in &steps {
                assert!(
                    model.fire(step, &mut state),
                    "{step} is not enabled: {state:?}"
                );
                fired.insert(step);
                buggy_ok &= buggy.fire(step, &mut buggy_state);
            }
            assert!(conforms(&ic, &state), "after {steps:?}: {state:?}");
            assert!(model.check_invariant("FrameKeepsWhatItUses", &state));
            buggy_ok &= conforms(&ic, &buggy_state);
        }
        buggy_rejected += usize::from(!buggy_ok);
    }
    for action in ["Begin", "UseA", "PutA", "EvictA", "UseB", "PutB", "EvictB"] {
        assert!(fired.contains(action), "the walks never took {action}");
    }
    assert!(
        buggy_rejected > 0,
        "the projection must reject the frame boundary that took no tick"
    );
}

#[test]
fn image_cache_decodes_a_placement_at_its_frame_size() {
    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageScaling};
    let image = |fill: u8| {
        std::sync::Arc::new(ImageData {
            bytes: [fill, 0, 0, 255].repeat(4),
            format: ImageFormat::RawRgba8 {
                width: 2,
                height: 2,
            },
            scaling: ImageScaling::Stretch,
            ..(*mk_image_arc()).clone()
        })
    };
    let fp = (8192, 4096);
    let pictures: Vec<_> = (0..3).map(|i| image(10 * i + 1)).collect();
    let mut ic = ImageCache::default();
    let shown = |ic: &mut ImageCache, n: usize| {
        ic.begin_frame(pictures[..n].iter().map(|p| (p, fp.0, fp.1)));
    };

    shown(&mut ic, 1);
    let full = ic.get_or_decode(&pictures[0], fp.0, fp.1);
    assert_eq!((full.w, full.h), fp, "alone, it decodes at full size");

    shown(&mut ic, 3);
    assert!(
        ic.get(&pictures[0], fp.0, fp.1).is_none(),
        "the full-size raster is not this frame's"
    );
    for picture in &pictures {
        let small = ic.get_or_decode(picture, fp.0, fp.1);
        assert!(small.w * small.h * 4 <= MAX_IMAGE_WORKING_SET_BYTES / 3);
    }
    assert_eq!(ic.entries.len(), 3, "the full-size entry was replaced");
    assert!(ic.bytes <= MAX_IMAGE_WORKING_SET_BYTES);
    shown(&mut ic, 3);
    for picture in &pictures {
        assert!(ic.get(picture, fp.0, fp.1).is_some());
    }

    shown(&mut ic, 1);
    assert!(ic.get(&pictures[0], fp.0, fp.1).is_none());
    let full = ic.get_or_decode(&pictures[0], fp.0, fp.1);
    assert_eq!((full.w, full.h), fp, "alone again, full size again");
}

#[test]
fn frame_raster_cap_fits_the_frame_in_its_working_set() {
    let budget = MAX_IMAGE_WORKING_SET_BYTES / 4;
    let full = MAX_PLACEMENT_RASTER_PX;
    assert_eq!(frame_raster_cap(&mut []), full);
    assert_eq!(
        frame_raster_cap(&mut [full, full]),
        full,
        "two fit as they are"
    );
    for demands in [
        vec![full; 3],
        vec![full; 16],
        vec![1_000, 2_000, full, full, full],
        vec![budget / 2, budget / 2, 1],
        vec![budget / 3 + 1; 3],
        vec![budget, 1],
    ] {
        let mut sorted = demands.clone();
        let cap = frame_raster_cap(&mut sorted);
        let held: usize = demands.iter().map(|&d| d.min(cap)).sum();
        assert!(held <= budget, "{held} px held at cap {cap}");
        assert!(cap >= 1);
        for &d in &demands {
            if d <= budget / demands.len() {
                assert!(d <= cap, "a demand under an even share keeps its size");
            }
        }
    }
}

#[test]
fn placement_raster_size_within_holds_every_footprint_to_its_cap() {
    let fps = [
        (1, 1),
        (7680, 9320),
        (22_320, 22_892),
        (100_000, 30),
        (30, 100_000),
        (16_385, 1),
        (1, 16_385),
        (65_535 * 200, 233),
    ];
    for &(w, h) in &fps {
        assert_eq!(
            placement_raster_size_within(w, h, MAX_PLACEMENT_RASTER_PX),
            placement_raster_size(w, h)
        );
        let (fw, fh) = placement_raster_size(w, h);
        for cap in [1, 2, 7, 1_000, 22_369_621, MAX_PLACEMENT_RASTER_PX] {
            let (rw, rh) = placement_raster_size_within(w, h, cap);
            assert!(rw >= 1 && rh >= 1 && rw <= w && rh <= h, "{w}x{h} @ {cap}");
            assert!(rw * rh <= cap.max(1), "{w}x{h} @ {cap}: {rw}x{rh}");
            assert!(rw * rh <= fw * fh, "{w}x{h} @ {cap}: {rw}x{rh}");
            if fw * fh <= cap {
                assert_eq!((rw, rh), (fw, fh));
            }
        }
    }
}

#[test]
fn font_swap_setters_invalidate_stale_glyph_caches() {
    let mut r = renderer();
    let bytes = system_font_bytes();

    fn seed_glyph(r: &mut Renderer, source: FaceId, ch: char) {
        let key = GlyphKey::mono_char(source, ch, StyleBits::REGULAR, r.px_q);
        r.glyphs.insert(
            key,
            CachedGlyph::new(GlyphImage::Mono {
                width: 1,
                height: 1,
                xmin: 0,
                ymin: 0,
                advance: 0.0,
                bytes: vec![0xFF],
            }),
        );
    }

    seed_glyph(&mut r, FaceId::Fallback, '中');
    r.keys.insert(
        ('中', ScalarPresentation::Default),
        GlyphKey::mono_char(FaceId::Fallback, '中', StyleBits::REGULAR, r.px_q),
    );
    r.set_fallback_bytes(&bytes).expect("valid font installs");
    assert!(
        r.glyphs.is_empty(),
        "set_fallback_bytes must drop stale glyph bitmaps"
    );
    assert!(
        r.keys.is_empty(),
        "set_fallback_bytes drops the per-char key memo"
    );

    seed_glyph(&mut r, FaceId::SymbolFallback, '⏸');
    r.keys.insert(
        ('⏸', ScalarPresentation::Default),
        GlyphKey::mono_char(FaceId::Primary, '⏸', StyleBits::REGULAR, r.px_q),
    );
    r.set_symbol_fallback_bytes(&bytes)
        .expect("valid font installs");
    assert!(
        r.glyphs.is_empty(),
        "set_symbol_fallback_bytes must drop stale glyph bitmaps"
    );
    assert!(
        r.keys.is_empty(),
        "set_symbol_fallback_bytes must drop the per-char key memo"
    );

    seed_glyph(&mut r, FaceId::ColorEmoji, '🚀');
    r.emoji_keys
        .insert('🚀', GlyphKey::rgba_char(FaceId::ColorEmoji, '🚀', r.px_q));
    r.cluster_shapes.insert(
        "👩\u{200d}🚀",
        Some(ClusterShape::Glyph { face: 0, gid: 42 }),
    );
    r.set_color_font_bytes(bytes.clone())
        .expect("valid font installs");
    assert!(
        r.glyphs.is_empty(),
        "set_color_font_bytes must drop stale glyph bitmaps"
    );
    assert!(
        r.emoji_keys.is_empty(),
        "set_color_font_bytes drops the per-char emoji key memo"
    );
    assert!(
        r.cluster_shapes.is_empty(),
        "set_color_font_bytes drops the shaped-cluster memo"
    );

    seed_glyph(&mut r, FaceId::BoldPrimary, 'B');
    r.styled_keys.insert(
        ('B', StyleBits::BOLD, ScalarPresentation::Default),
        GlyphKey::mono_gid(7, StyleBits::BOLD, r.px_q),
    );
    r.set_bold_font(&bytes).expect("valid font installs");
    assert!(
        r.glyphs.is_empty(),
        "set_bold_font must drop stale glyph bitmaps"
    );
    assert!(
        r.styled_keys.is_empty(),
        "set_bold_font drops the styled key memo"
    );
}

fn cell_pixels(
    frame: &Frame,
    col: usize,
    row: usize,
    cw: usize,
    ch: usize,
) -> impl Iterator<Item = u32> + '_ {
    let (x0, y0) = (col * cw, row * ch);
    (y0..y0 + ch).flat_map(move |y| (x0..x0 + cw).map(move |x| frame.pixels[y * frame.width + x]))
}

fn candidate_bytes(p: &str) -> Option<Vec<u8>> {
    bundled::face_for_id(p)
        .map(<[u8]>::to_vec)
        .or_else(|| std::fs::read(p).ok())
}

const BUNDLED_MONO: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Regular.ttf");

fn system_font_bytes() -> Vec<u8> {
    primary_candidate_paths()
        .iter()
        .find_map(|p| candidate_bytes(p))
        .unwrap_or_else(|| BUNDLED_MONO.to_vec())
}

fn present_fallback_paths() -> Vec<String> {
    fallback_discovery_paths()
        .into_iter()
        .filter(|p| std::path::Path::new(p).is_file())
        .collect()
}

fn require_host_colour_emoji() {
    let noto = "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf";
    if !cfg!(any(target_os = "macos", windows)) && !std::path::Path::new(noto).is_file() {
        aterm_spec::host::could_not_run(format!("{noto} is not installed here"));
    }
}

fn asset_path(rel: &str) -> String {
    let file = aterm_tempfile::manifest_dir().join("assets").join(rel);
    file.into_os_string()
        .into_string()
        .expect("a UTF-8 package directory")
}

#[cfg(any(windows, target_os = "linux"))]
fn chain_covers(chain: &[FallbackFace], ch: char) -> bool {
    chain.iter().any(|f| {
        aterm_font::Face::parse(&f.bytes, f.index)
            .ok()
            .and_then(|p| p.glyph_index(ch))
            .is_some_and(|g| g.0 != 0)
    })
}

#[test]
#[cfg(windows)]
fn windows_fallback_chain_covers_the_scripts_it_claims() {
    let paths = present_fallback_paths();
    let chain = build_fallback_chain(&paths, 0);
    let present = |f: &str| std::path::Path::new(&format!("C:\\Windows\\Fonts\\{f}")).is_file();
    let claims: &[(&str, char, &str)] = &[
        ("Han", '\u{4E2D}', "msyh.ttc"),
        ("Kana", '\u{30C6}', "msyh.ttc"),
        ("Hangul", '\u{D55C}', "malgun.ttf"),
        ("Hangul jamo", '\u{1100}', "malgun.ttf"),
        ("Devanagari", '\u{0915}', "Nirmala.ttc"),
        ("Tamil", '\u{0B95}', "Nirmala.ttc"),
        ("Bengali", '\u{0985}', "Nirmala.ttc"),
        ("Telugu", '\u{0C05}', "Nirmala.ttc"),
        ("Malayalam", '\u{0D05}', "Nirmala.ttc"),
        ("Sinhala", '\u{0D85}', "Nirmala.ttc"),
        ("Thai", '\u{0E01}', "LeelawUI.ttf"),
        ("Khmer", '\u{1780}', "LeelawUI.ttf"),
        ("Lao", '\u{0E81}', "LeelawUI.ttf"),
        ("Georgian", '\u{10D0}', "sylfaen.ttf"),
        ("Ethiopic", '\u{1200}', "ebrima.ttf"),
        ("Tifinagh", '\u{2D30}', "ebrima.ttf"),
        ("Vai", '\u{A500}', "ebrima.ttf"),
        ("Cherokee", '\u{13A0}', "gadugi.ttf"),
        ("Canadian Aboriginal", '\u{1401}', "gadugi.ttf"),
        ("Thaana", '\u{0780}', "mvboli.ttf"),
        ("Hebrew", '\u{05D0}', "arial.ttf"),
        ("Arabic", '\u{0627}', "arial.ttf"),
        ("Armenian", '\u{0531}', "arial.ttf"),
    ];
    let mut checked = 0usize;
    let mut absent = Vec::new();
    for (script, ch, face) in claims {
        if !present(face) {
            absent.push(format!("{script} ({face})"));
            continue;
        }
        assert!(
            chain_covers(&chain, *ch),
            "{script} (U+{:04X}) is claimed by {face}, which IS installed, but no \
             chain face covers it — the candidate scan never reached it",
            *ch as u32
        );
        checked += 1;
    }
    eprintln!(
        "chain of {} face(s) covered {checked} script(s)",
        chain.len()
    );
    if !absent.is_empty() {
        aterm_spec::host::could_not_run(format!(
            "not installed here, so unchecked: {}",
            absent.join(", ")
        ));
    }
}

#[cfg(target_os = "linux")]
fn linux_candidates() -> Vec<&'static str> {
    FALLBACK_CANDIDATES
        .iter()
        .copied()
        .filter(|p| p.starts_with("/usr/share/fonts/"))
        .collect()
}

#[cfg(target_os = "linux")]
const LINUX_BROAD_BACKSTOPS: &[&str] = &[
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];

#[cfg(target_os = "linux")]
fn face_coverage(path: &str) -> std::collections::BTreeSet<u32> {
    if let Some(bytes) = bundled::face_for_id(path) {
        return collection_coverage(bytes);
    }
    let Ok(bytes) = std::fs::read(path) else {
        return std::collections::BTreeSet::new();
    };
    collection_coverage(&bytes)
}

#[cfg(target_os = "linux")]
fn collection_coverage(bytes: &[u8]) -> std::collections::BTreeSet<u32> {
    let mut out = std::collections::BTreeSet::new();
    let faces = aterm_font::fonts_in_collection(bytes).unwrap_or(1);
    for i in 0..faces {
        let Ok(f) = aterm_font::Face::parse(bytes, i) else {
            continue;
        };
        let Some(cmap) = f.tables().cmap else {
            continue;
        };
        let mut cps: Vec<u32> = Vec::new();
        for sub in cmap.subtables {
            if sub.is_unicode() {
                sub.codepoints(|cp| cps.push(cp));
            }
        }
        for cp in cps {
            if let Some(c) = char::from_u32(cp)
                && f.glyph_index(c).is_some_and(|g| g.0 != 0)
            {
                out.insert(cp);
            }
        }
    }
    out
}

#[test]
#[cfg(target_os = "linux")]
fn the_linux_per_script_tier_never_shadows_the_broad_cjk_backstop() {
    let cjk_path = LINUX_BROAD_BACKSTOPS[0];
    if !std::path::Path::new(cjk_path).is_file() {
        aterm_spec::host::could_not_run(format!("{cjk_path} is not installed here"));
    }
    let primary = primary_candidate_paths()
        .into_iter()
        .find(|p| bundled::is_bundled_id(p) || std::path::Path::new(p).is_file());
    let Some(primary) = primary else {
        aterm_spec::host::could_not_run("no primary font found");
    };
    let mut ahead_cov = face_coverage(&primary);
    for id in bundled::broad_fallback_ids() {
        ahead_cov.extend(face_coverage(id));
    }
    let cjk_cov = face_coverage(cjk_path);
    let owned: std::collections::BTreeSet<u32> = cjk_cov.difference(&ahead_cov).copied().collect();
    assert!(
        owned.len() > 10_000,
        "the CJK backstop owns only {} code points — this host's fonts are \
         not what this test assumes and it would prove nothing",
        owned.len()
    );
    let linux = linux_candidates();
    let tier = &linux[..linux.len() - LINUX_BROAD_BACKSTOPS.len()];
    let mut checked = 0usize;
    for path in tier {
        if !std::path::Path::new(path).is_file() {
            continue;
        }
        let steal: Vec<u32> = face_coverage(path)
            .intersection(&owned)
            .copied()
            .take(24)
            .collect();
        assert!(
            steal.is_empty(),
            "{path} sits AHEAD of {cjk_path} in the chain and takes {} of the \
             code points only that face should draw: {} — a per-script face \
             may not shadow the broad CJK backstop",
            steal.len(),
            steal
                .iter()
                .map(|c| format!("U+{c:04X}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        checked += 1;
    }
    if checked == 0 {
        aterm_spec::host::could_not_run(
            "none of the Linux per-script tier's faces is installed (fonts-noto-core); \
             nothing proven",
        );
    }

    let mut offenders = 0usize;
    for path in [
        "/usr/share/fonts/truetype/noto/NotoSansMongolian-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansYi-Regular.ttf",
    ] {
        assert!(
            !tier.contains(&path),
            "{path} is in the per-script tier, where it steals CJK punctuation"
        );
        if !std::path::Path::new(path).is_file() {
            continue;
        }
        let steal: Vec<u32> = face_coverage(path).intersection(&owned).copied().collect();
        assert!(
            !steal.is_empty(),
            "{path} was expected to shadow the CJK backstop (that is WHY it is \
             excluded) but takes nothing from it — the shadow check above is \
             therefore vacuous on this host"
        );
        offenders += 1;
        eprintln!(
            "control: {} would have stolen {} code points ({} ...)",
            path.rsplit('/').next().unwrap_or(path),
            steal.len(),
            steal
                .iter()
                .take(6)
                .map(|c| format!("U+{c:04X}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    eprintln!(
        "{checked} tier face(s) shadow nothing the CJK backstop owns; \
         {offenders} known offender(s) confirmed still detectable"
    );
}

#[test]
fn symbol_discovery_drops_system_copies_of_bundled_faces() {
    let paths = symbol_discovery_paths();
    let superseded: Vec<&String> = paths
        .iter()
        .filter(|p| bundled::supersedes_system_face(p))
        .collect();
    assert!(superseded.is_empty(), "{superseded:?}");
    if bundled::ACTIVE {
        assert!(
            !paths
                .iter()
                .any(|p| p.ends_with("/NotoSansSymbols2-Regular.ttf")),
            "{paths:?}"
        );
    }
}

#[test]
fn the_bundled_broad_face_shadows_no_cjk_point() {
    if !bundled::ACTIVE {
        return;
    }
    let coverage = |bytes: &[u8]| -> std::collections::BTreeSet<u32> {
        let face = aterm_font::Face::parse(bytes, 0).unwrap();
        let mut out = std::collections::BTreeSet::new();
        for sub in face.tables().cmap.unwrap().subtables {
            if sub.is_unicode() {
                sub.codepoints(|cp| {
                    if char::from_u32(cp)
                        .and_then(|c| face.glyph_index(c))
                        .is_some_and(|g| g.0 != 0)
                    {
                        out.insert(cp);
                    }
                });
            }
        }
        out
    };
    let primary = coverage(bundled::face_for_id(bundled::PRIMARY_ID).unwrap());
    assert_eq!(bundled::broad_fallback_ids(), [bundled::DEJAVU_ID]);
    let dejavu = coverage(bundled::face_for_id(bundled::DEJAVU_ID).unwrap());
    let leads: Vec<u32> = dejavu.difference(&primary).copied().collect();
    const CJK_BLOCKS: &[(u32, u32)] = &[
        (0x1100, 0x11FF),
        (0x2E80, 0x2FDF),
        (0x3000, 0x33FF),
        (0x3400, 0x4DBF),
        (0x4E00, 0x9FFF),
        (0xA960, 0xA97F),
        (0xAC00, 0xD7FF),
        (0xF900, 0xFAFF),
        (0xFE30, 0xFE4F),
        (0xFF00, 0xFFEF),
        (0x2_0000, 0x3_FFFF),
    ];
    let cjk: Vec<String> = leads
        .iter()
        .filter(|&&c| CJK_BLOCKS.iter().any(|&(a, b)| (a..=b).contains(&c)))
        .map(|c| format!("U+{c:04X}"))
        .collect();
    assert!(cjk.is_empty(), "bundled DejaVu shadows CJK: {cjk:?}");
}

#[test]
#[cfg(target_os = "linux")]
fn linux_script_code_points_dispatch_to_the_fallback_tier_with_real_ink() {
    let mut r = renderer();
    r.seal_admitted_font_sources();
    let candidates = script_face_candidates(SCRIPT_SUPPLEMENT_ALL);
    let mut checked = 0usize;
    let mut unchecked = Vec::new();
    for (bit, &ch) in SCRIPT_SUPPLEMENT_PROBES.iter().enumerate() {
        if !candidates.iter().any(|c| c.covers & (1 << bit) != 0)
            && !r
                .fallback_chain
                .iter()
                .any(|f| chain_covers(std::slice::from_ref(f), ch))
        {
            unchecked.push(format!(
                "U+{:04X} (no installed face draws it here)",
                ch as u32
            ));
            continue;
        }
        if r.primary_unicode_gid(ch).is_some() {
            unchecked.push(format!("U+{:04X} (the primary face carries it)", ch as u32));
            continue;
        }
        let key = r.glyph_key(ch);
        assert_eq!(
            key.source,
            FaceId::Fallback,
            "U+{:04X} ({:?}) is drawn by an installed face but resolved to {:?}, \
             not the broad fallback chain. Primary means the .notdef tofu box; \
             RuntimeFallback means only the UNSEALED path finds it, and every \
             GUI window seals — so on screen that is tofu too.",
            ch as u32,
            aterm_grapheme::script(ch),
            key.source
        );
        let img = r.glyph_image(key);
        assert!(
            img.bytes().iter().any(|&c| c > 0),
            "U+{:04X} rasterized with no coverage at all",
            ch as u32
        );
        eprintln!(
            "U+{:04X} -> {}",
            ch as u32,
            r.debug_fallback_pick_path(ch).unwrap_or_default()
        );
        checked += 1;
    }
    eprintln!("{checked} script(s) dispatched to the fallback chain with real ink");
    if !unchecked.is_empty() {
        aterm_spec::host::could_not_run(format!(
            "unchecked on this host: {}",
            unchecked.join(", ")
        ));
    }
}

#[test]
fn an_emoji_presentation_scalar_a_text_face_maps_still_draws_in_colour() {
    let mut generations = vec![("host default", renderer())];
    if bundled::IOSEVKA_ACTIVE {
        generations.push((
            "bundled Iosevka Term",
            Renderer::from_system_with_family(
                Some(bundled::IOSEVKA_FAMILY),
                16.0,
                Theme::default(),
            )
            .expect(NO_FONT),
        ));
    }
    if bundled::ACTIVE && generations[0].1.primary_path.as_deref() != Some(bundled::JETBRAINS_ID) {
        generations.push((
            "bundled JetBrains Mono",
            Renderer::from_system_with_family(Some(bundled::JETBRAINS_ID), 16.0, Theme::default())
                .expect(NO_FONT),
        ));
    }
    let mut checked = 0usize;
    let mut declined_symbol_face: Option<(char, String)> = None;
    for (label, mut r) in generations {
        r.seal_admitted_font_sources();
        let bundled_primary = r
            .primary_path
            .as_deref()
            .is_some_and(bundled::is_bundled_id);
        for ch in [
            '\u{26A1}',
            '\u{26BD}',
            '\u{26BE}',
            '\u{1F197}',
            '\u{1F195}',
            '\u{1F198}',
            '\u{1F201}',
            '\u{1F251}',
        ] {
            assert!(aterm_grapheme::is_emoji_presentation(ch));
            if !r.color_font_has(ch) {
                continue;
            }
            let primary_maps = r.primary_unicode_gid(ch).is_some();
            let text_face = if primary_maps {
                if !bundled_primary {
                    continue;
                }
                FaceId::Primary
            } else if r.fallback_has(ch) {
                FaceId::Fallback
            } else if let Some(i) = r.symbol_chain_pick(ch) {
                let face = &r.symbol_chain[i];
                assert!(!face.chosen, "{label}: no symbol face was named here");
                if let Some(path) = face.path.clone()
                    && !bundled::is_bundled_id(&path)
                {
                    declined_symbol_face.get_or_insert((ch, path));
                }
                FaceId::SymbolFallback
            } else {
                continue;
            };
            let key = r.glyph_key(ch);
            assert_eq!(
                key.source,
                FaceId::ColorEmoji,
                "{label}: U+{:04X} has a colour glyph but drew from {:?} ({:?})",
                ch as u32,
                key.source,
                r.primary_path
            );
            let text = r.glyph_key_text(ch);
            assert_eq!(
                text.source, text_face,
                "{label}: VS15 U+{:04X} must keep the text face's glyph ({:?})",
                ch as u32, r.primary_path,
            );
            checked += 1;
        }
    }
    if let Some((ch, path)) = declined_symbol_face {
        let mut named = renderer();
        assert!(named.set_config_symbol_font(Some(&path)));
        named.seal_admitted_font_sources();
        if named.primary_unicode_gid(ch).is_none() && !named.fallback_has(ch) {
            assert_eq!(
                named.glyph_key(ch).source,
                FaceId::SymbolFallback,
                "U+{:04X}: the user named {path} as the symbol face, so it keeps \
                 the glyph the built-in chain left to the colour face",
                ch as u32
            );
        }
    }
    if checked == 0 {
        aterm_spec::host::could_not_run(
            "no colour face here, or no text face ahead of it maps a probed emoji",
        );
    }
    eprintln!("{checked} emoji-presentation scalar(s) drew in colour over a text face");
}

#[test]
#[cfg(target_os = "linux")]
fn a_script_supplement_face_answers_only_its_own_script() {
    let mut r = renderer();
    r.seal_admitted_font_sources();
    let supplement: Vec<FallbackFace> = r
        .fallback_chain
        .iter()
        .filter(|f| f.scope.is_some())
        .cloned()
        .collect();
    if supplement.is_empty() {
        aterm_spec::host::could_not_run(
            "the built chain already draws every probed script (or no face does)",
        );
    }
    let mut declined = 0usize;
    for face in &supplement {
        let path = face.path.clone().unwrap_or_default();
        let scope = face.scope.as_deref().expect("filtered on scope");
        let cjk_face = ['\u{4E2D}', '\u{3042}', '\u{30A2}', '\u{D55C}']
            .iter()
            .any(|&letter| scope.scripts.contains(&aterm_grapheme::script(letter)));
        for cp in collection_coverage(&face.bytes) {
            let Some(ch) = char::from_u32(cp) else {
                continue;
            };
            let cjk_forms = (0x3000..=0x30FF).contains(&cp) || (0xFF00..=0xFFEF).contains(&cp);
            let foreign = font_chain::is_private_use(ch)
                || aterm_grapheme::is_emoji_presentation(ch)
                || cp == 0x203B
                || (cjk_forms && !cjk_face);
            if !foreign {
                continue;
            }
            assert!(
                !scope.admits(ch),
                "{path} was admitted for {:?} and may answer U+{cp:04X}, which \
                 belongs to another face",
                scope.scripts
            );
            assert_ne!(
                r.debug_fallback_pick_path(ch).as_deref(),
                Some(path.as_str()),
                "{path} drew U+{cp:04X} outside its script"
            );
            declined += 1;
        }
    }
    if declined == 0 {
        aterm_spec::host::could_not_run(
            "no supplement face here maps a private-use, emoji or CJK point to decline",
        );
    }
    eprintln!(
        "{} supplement face(s) declined {declined} foreign code point(s) their cmaps map",
        supplement.len()
    );
}

#[test]
fn a_script_scope_admits_its_script_and_its_own_blocks_shared_points() {
    let deva = ScriptScope {
        scripts: vec![aterm_grapheme::script('\u{0915}')],
        blocks: vec![0x0900 >> ScriptScope::BLOCK_SHIFT],
    };
    for ch in ['\u{0915}', '\u{094D}', '\u{0964}', '\u{0965}', '\u{A8E0}'] {
        assert!(deva.admits(ch), "U+{:04X}", ch as u32);
    }
    for ch in [
        '\u{0E01}',
        '\u{3008}',
        '\u{203B}',
        '\u{F021}',
        '\u{1F600}',
        'a',
    ] {
        assert!(!deva.admits(ch), "U+{:04X}", ch as u32);
    }
}

#[test]
fn the_script_supplement_picks_a_dedicated_monospace_regular_face() {
    let face = |path: &str, covers: u64, points: u16, monospace: bool, weight, italic| {
        ScriptFaceCandidate {
            path: path.to_string(),
            covers,
            script_points: vec![points; 3],
            monospace,
            weight,
            italic,
        }
    };
    let candidates = [
        face("pan-unicode", 0b111, 90, true, 400, false),
        face("thai-proportional", 0b001, 90, false, 400, false),
        face("thai-mono-bold", 0b001, 90, true, 700, false),
        face("thai-mono", 0b001, 90, true, 400, false),
        face("thai-mono-italic", 0b001, 90, true, 400, true),
        face("thai-mono-partial", 0b001, 60, true, 400, false),
        face("deva-partial", 0b010, 60, false, 400, false),
        face("deva", 0b010, 120, false, 400, false),
    ];
    assert_eq!(
        pick_script_faces(&candidates, 0b111),
        vec![(3, 0b001), (7, 0b010), (0, 0b100)]
    );
    assert_eq!(pick_script_faces(&candidates, 0), vec![]);
    assert_eq!(pick_script_faces(&candidates[..1], 0b011), vec![(0, 0b011)]);
}

#[test]
#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn the_script_supplement_reads_cmaps_only_for_a_probe_its_named_faces_miss() {
    let bit = |probe: char| {
        let i = SCRIPT_SUPPLEMENT_PROBES.iter().position(|&p| p == probe);
        1u64 << i.expect("a supplement probe")
    };
    let (armenian, georgian, mongolian) = ('\u{0531}', '\u{10D0}', '\u{1820}');
    let named = [(bundled::DEJAVU_ID, armenian)];
    let passes = std::cell::Cell::new(0u32);
    let asked = std::cell::Cell::new(0u64);
    let pass = |missing: u64| {
        passes.set(passes.get() + 1);
        asked.set(missing);
        std::sync::Arc::new(Vec::new())
    };

    let faces = supplement_missing_scripts(bit(armenian), &named, pass);
    assert_eq!(
        passes.get(),
        0,
        "a named face closed the only miss, yet the pass ran"
    );
    assert_eq!(faces.len(), 1);
    let scope = faces[0]
        .scope
        .as_deref()
        .expect("a named face is admitted scoped");
    assert!(scope.admits(armenian));
    assert!(
        !scope.admits('a'),
        "scoped to Armenian, it declines the Latin it maps"
    );
    assert!(supplement_missing_scripts(0, &named, pass).is_empty());
    assert_eq!(passes.get(), 0, "nothing missing, yet the pass ran");

    supplement_missing_scripts(bit(armenian) | bit(georgian), &named, pass);
    assert_eq!(passes.get(), 1);
    assert_eq!(
        asked.get(),
        bit(georgian),
        "the pass is asked for the probe the named face left missing, and only it"
    );
    let absent = [("/nonexistent/aterm-test/Absent-Regular.ttf", armenian)];
    assert!(supplement_missing_scripts(bit(armenian), &absent, pass).is_empty());
    assert_eq!(passes.get(), 2);
    let wrong = [(bundled::DEJAVU_ID, mongolian)];
    assert!(supplement_missing_scripts(bit(mongolian), &wrong, pass).is_empty());
    assert_eq!(passes.get(), 3);
}

#[cfg(any(target_os = "linux", feature = "embedded-font"))]
const SCRIPT_SUPPLEMENT_ALL: u64 = (1 << SCRIPT_SUPPLEMENT_PROBES.len()) - 1;

#[test]
#[cfg(feature = "embedded-font")]
fn the_script_census_runs_only_for_a_face_that_draws_a_missing_probe() {
    use crate::test_counters::take_script_censuses;
    let bit = |probe: char| {
        let i = SCRIPT_SUPPLEMENT_PROBES.iter().position(|&p| p == probe);
        1u64 << i.expect("a supplement probe")
    };
    let (armenian, georgian) = ('\u{0531}', '\u{10D0}');
    let scratch = crate::test_scratch_dir("script-census");
    let path = scratch.path().join("DejaVuSansMono.ttf");
    std::fs::write(&path, embedded_font()).unwrap();
    take_script_censuses();

    let every = script_face_candidate(&path, SCRIPT_SUPPLEMENT_ALL)
        .expect("DejaVu Sans Mono draws probed scripts");
    assert_eq!(take_script_censuses(), 1);
    assert_eq!(
        every.covers & (bit(armenian) | bit(georgian)),
        bit(armenian) | bit(georgian),
        "DejaVu Sans Mono draws Armenian and Georgian"
    );

    let bytes = std::fs::read(&path).unwrap();
    let face = aterm_font::Face::parse(&bytes, 0).unwrap();
    let only_others = SCRIPT_SUPPLEMENT_PROBES
        .iter()
        .enumerate()
        .filter(|&(_, &probe)| face.glyph_index(probe).is_none_or(|g| g.0 == 0))
        .fold(0u64, |set, (i, _)| set | (1 << i));
    assert_ne!(only_others, 0);
    assert!(script_face_candidate(&path, only_others).is_none());
    assert_eq!(
        take_script_censuses(),
        0,
        "a face drawing no missing probe was counted code point by code point"
    );

    let mut mapped: Vec<char> = Vec::new();
    for sub in face
        .tables()
        .cmap
        .expect("a cmap")
        .subtables
        .into_iter()
        .filter(|t| t.is_unicode())
    {
        sub.codepoints(|cp| mapped.extend(char::from_u32(cp)));
    }
    mapped.sort_unstable();
    mapped.dedup();
    for (i, &probe) in SCRIPT_SUPPLEMENT_PROBES.iter().enumerate() {
        if every.covers & (1 << i) != 0 {
            let script = aterm_grapheme::script(probe);
            let by_lookup = mapped
                .iter()
                .filter(|&&c| aterm_grapheme::script(c) == script)
                .count();
            assert_eq!(usize::from(every.script_points[i]), by_lookup, "{script:?}");
        }
    }
    let scripts = [
        aterm_grapheme::script(armenian),
        aterm_grapheme::script(georgian),
    ];
    let mut blocks: Vec<u32> = mapped
        .iter()
        .filter(|&&c| scripts.contains(&aterm_grapheme::script(c)))
        .map(|&c| u32::from(c) >> ScriptScope::BLOCK_SHIFT)
        .collect();
    blocks.dedup();
    assert!(!blocks.is_empty());
    assert_eq!(
        ScriptScope::of_face(&bytes, 0, &[armenian, georgian]).blocks,
        blocks
    );

    let one = script_face_candidate(&path, bit(armenian)).expect("it draws Armenian");
    assert_eq!(take_script_censuses(), 1);
    assert_eq!(
        (
            one.covers,
            &one.script_points,
            one.monospace,
            one.weight,
            one.italic
        ),
        (
            every.covers,
            &every.script_points,
            every.monospace,
            every.weight,
            every.italic
        ),
        "a counted face's facts must not depend on which other probes are missing"
    );
}

#[test]
fn a_missing_builtin_linux_font_is_found_where_this_distro_put_it() {
    use std::path::PathBuf;
    let declared = "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf";
    let files: Vec<PathBuf> = [
        "/home/u/.local/share/fonts/Other.ttf",
        "/home/u/.local/share/fonts/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/google-noto/NotoSansSymbols2-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    assert_eq!(
        relocate_builtin_font_path(declared, &files).as_deref(),
        Some("/home/u/.local/share/fonts/NotoSansSymbols2-Regular.ttf"),
        "first catalogue match wins, so a per-user copy shadows a system one"
    );
    assert_eq!(
        relocate_builtin_font_path(declared, &files[2..]).as_deref(),
        Some("/usr/share/fonts/google-noto/NotoSansSymbols2-Regular.ttf"),
        "the Fedora layout of the same file"
    );
    assert_eq!(
        relocate_builtin_font_path(declared, &files[3..]),
        None,
        "never relocated onto its own declared path"
    );
    assert_eq!(relocate_builtin_font_path(declared, &files[..1]), None);
    assert_eq!(
        relocate_builtin_font_path("/System/Library/Fonts/Apple Symbols.ttf", &files),
        None,
        "only the Debian-rooted Linux spellings relocate"
    );
}

#[test]
fn relocation_never_touches_the_user_prefix_or_a_present_file() {
    let absent = format!(
        "{LINUX_BUILTIN_FONT_ROOT}aterm-no-such-dir-{}/DejaVuSans.ttf",
        std::process::id()
    );
    let paths = vec![absent.clone(), absent];
    assert_eq!(
        relocate_missing_builtin_fonts(&paths, 2),
        vec![None, None],
        "both entries are user-supplied"
    );
    let scratch = crate::test_scratch_dir("relocation-present");
    let present = scratch.path().join("probe.ttf");
    std::fs::write(&present, b"present").unwrap();
    let paths = vec![present.to_str().unwrap().to_string()];
    assert_eq!(relocate_missing_builtin_fonts(&paths, 0), vec![None]);
}

#[cfg(feature = "embedded-font")]
#[test]
fn a_relocated_text_only_backstop_keeps_its_gate() {
    let backstop = SYMBOL_TIER_TEXT_ONLY_BACKSTOPS[0];
    let scratch = crate::test_scratch_dir("relocated-backstop");
    let elsewhere = scratch
        .path()
        .join(std::path::Path::new(backstop).file_name().unwrap());
    std::fs::write(&elsewhere, embedded_font()).unwrap();
    let elsewhere = elsewhere.to_str().unwrap().to_string();

    let face = admit_chain_face(backstop, Some(&elsewhere)).expect("admissible");
    assert_eq!(face.path.as_deref(), Some(elsewhere.as_str()));
    assert_eq!(face.relocated_from.as_deref(), Some(backstop));
    assert!(
        symbol_face_is_text_only_backstop(&face),
        "a relocated backstop must stay gated off emoji/PUA code points"
    );

    let plain = admit_chain_face(&elsewhere, None).expect("admissible");
    assert_eq!(plain.relocated_from, None);
    assert!(!symbol_face_is_text_only_backstop(&plain));
}

#[test]
#[cfg(target_os = "linux")]
fn the_symbol_backstop_draws_the_operators_the_reachable_chain_lacks() {
    let backstop = SYMBOL_TIER_TEXT_ONLY_BACKSTOPS[0];
    if !std::path::Path::new(backstop).is_file() {
        aterm_spec::host::could_not_run(format!("{backstop} is not installed here"));
    }
    let mut r = renderer();
    r.seal_admitted_font_sources();
    let backstop_cov = face_coverage(backstop);
    let probes: &[char] = &[
        '\u{2024}', '\u{2043}', '\u{2112}', '\u{2120}', '\u{2216}', '\u{22A8}', '\u{22C8}',
        '\u{22F1}', '\u{27F9}', '\u{2A01}', '\u{2A7D}',
    ];
    let mut checked = 0usize;
    let mut unchecked = Vec::new();
    for &ch in probes {
        if !backstop_cov.contains(&(ch as u32)) {
            unchecked.push(format!(
                "U+{:04X} (this host's backstop lacks it)",
                ch as u32
            ));
            continue;
        }
        let earlier = if r.primary_unicode_gid(ch).is_some() {
            Some(FaceId::Primary)
        } else if r.fallback_has(ch) {
            Some(FaceId::Fallback)
        } else {
            None
        };
        if let Some(tier) = earlier {
            let key = r.glyph_key(ch);
            assert_eq!(key.source, tier, "{ch:?} U+{:04X}", ch as u32);
            assert!(
                r.glyph_image(key).bytes().iter().any(|&c| c > 0),
                "{ch:?} U+{:04X} rasterized with no coverage at all",
                ch as u32
            );
            continue;
        }
        let key = r.glyph_key(ch);
        assert_eq!(
            key.source,
            FaceId::SymbolFallback,
            "{ch:?} U+{:04X} resolved to {:?}. The symbol tier names \
             {backstop} its broad-symbol backstop and this host HAS that \
             file — Primary here means the .notdef box a person cannot tell \
             from any other .notdef box.",
            ch as u32,
            key.source
        );
        let img = r.glyph_image(key);
        assert!(
            img.bytes().iter().any(|&c| c > 0),
            "{ch:?} rasterized with no coverage at all"
        );
        checked += 1;
    }
    if checked == 0 {
        aterm_spec::host::could_not_run(
            "none of the symbol backstop's probe faces is installed here; nothing proven",
        );
    }
    let expected = if bundled::ACTIVE {
        bundled::NOTO_MATH_ID
    } else {
        backstop
    };
    let pick = r
        .symbol_chain_pick('\u{22A8}')
        .and_then(|i| r.symbol_chain.get(i))
        .and_then(|f| f.path.clone());
    assert_eq!(
        pick.as_deref(),
        Some(expected),
        "⊨ must draw from the first symbol-tier face the order names"
    );
    eprintln!("{checked} operator(s) reached the symbol backstop with real ink");
    if !unchecked.is_empty() {
        aterm_spec::host::could_not_run(format!(
            "unchecked on this host: {}",
            unchecked.join(", ")
        ));
    }
}

#[test]
#[cfg(target_os = "linux")]
fn the_symbol_backstop_never_takes_a_code_point_the_colour_face_owns() {
    let backstop = SYMBOL_TIER_TEXT_ONLY_BACKSTOPS[0];
    if !std::path::Path::new(backstop).is_file() {
        aterm_spec::host::could_not_run(format!("{backstop} is not installed here"));
    }
    let mut r = renderer();
    r.seal_admitted_font_sources();
    assert!(
        r.symbol_chain
            .iter()
            .any(|f| f.path.as_deref() == Some(backstop)),
        "{backstop} is installed but is not in the loaded symbol chain, so \
         nothing below can exercise the gate"
    );
    let backstop_cov = face_coverage(backstop);
    let mut checked = 0usize;
    for cp in backstop_cov.iter().copied() {
        let Some(ch) = char::from_u32(cp) else {
            continue;
        };
        if !aterm_grapheme::is_emoji_presentation(ch) || !r.color_font_has(ch) {
            continue;
        }
        let picked = r
            .symbol_chain_pick(ch)
            .and_then(|i| r.symbol_chain.get(i))
            .and_then(|f| f.path.clone());
        assert_ne!(
            picked.as_deref(),
            Some(backstop),
            "{ch:?} U+{cp:04X} defaults to EMOJI presentation and the colour \
             face covers it, but the broad-text backstop took it — the \
             text-only gate on {backstop} is not holding"
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "the backstop shadows no emoji code point on this host, so this law \
         is vacuous here and proves nothing"
    );
    let grin = '\u{1F600}';
    if backstop_cov.contains(&(grin as u32)) && r.color_font_has(grin) {
        assert_eq!(
            r.glyph_key(grin).source,
            FaceId::ColorEmoji,
            "😀 must still render in COLOUR"
        );
    }
    eprintln!("{checked} emoji-default code point(s) stayed off the backstop");
}

#[test]
#[cfg(all(target_os = "linux", feature = "embedded-font"))]
fn the_symbol_backstop_never_takes_a_private_use_icon_the_bundled_face_owns() {
    let backstop = SYMBOL_TIER_TEXT_ONLY_BACKSTOPS[0];
    if !std::path::Path::new(backstop).is_file() {
        aterm_spec::host::could_not_run(format!("{backstop} is not installed here"));
    }
    let mut r = renderer();
    r.seal_admitted_font_sources();
    assert!(
        r.symbol_chain
            .iter()
            .any(|f| f.path.as_deref() == Some(backstop)),
        "{backstop} is installed but is not in the loaded symbol chain, so \
         nothing below can exercise the gate"
    );
    let backstop_cov = face_coverage(backstop);
    let bundled_cov = collection_coverage(embedded_symbols_font());
    let contested: Vec<u32> = backstop_cov
        .iter()
        .copied()
        .filter(|cp| bundled_cov.contains(cp))
        .filter(|cp| char::from_u32(*cp).is_some_and(font_chain::is_private_use))
        .collect();
    assert!(
        !contested.is_empty(),
        "no private code point is mapped by BOTH {backstop} and the bundled \
         Symbols Nerd Font on this host, so this law is vacuous and proves \
         nothing"
    );
    for &cp in &contested {
        let ch = char::from_u32(cp).expect("filtered to real chars above");
        let picked = r
            .symbol_chain_pick(ch)
            .and_then(|i| r.symbol_chain.get(i))
            .and_then(|f| f.path.clone());
        assert_ne!(
            picked.as_deref(),
            Some(backstop),
            "U+{cp:04X} is PRIVATE USE and the bundled Symbols Nerd Font \
             carries it, but the broad-text backstop took it — a private \
             code point's coverage in a broad TEXT face is incidental, and \
             {backstop} draws an unrelated glyph for it"
        );
        assert_ne!(
            r.glyph_key(ch).source,
            FaceId::SymbolFallback,
            "U+{cp:04X} resolved to the symbol tier; the bundled icon face \
             owns this private code point"
        );
    }
    eprintln!(
        "{} private-use icon(s) stayed off the broad-text backstop",
        contested.len()
    );
    let git_branch = '\u{F418}';
    if contested.contains(&(git_branch as u32)) {
        let key = r.glyph_key(git_branch);
        assert_eq!(
            key.source,
            FaceId::RuntimeFallback,
            "oct-git_branch U+F418 must still draw from the runtime tier"
        );
        let slot = r
            .runtime_fallback
            .embedded_decisions
            .get(&git_branch)
            .copied()
            .flatten()
            .expect(
                "a SEALED generation resolves the runtime tier through \
                 `resolve_embedded_only`, so the decision must be in \
                 `embedded_decisions`",
            );
        assert_eq!(
            r.runtime_fallback.faces[slot].path,
            RuntimeFallback::EMBEDDED_SYMBOLS_PATH,
            "oct-git_branch U+F418 must come from the bundled Symbols Nerd \
             Font itself, not from some other runtime face"
        );
        let img = r.glyph_image(key);
        assert!(
            img.bytes().iter().any(|&c| c > 0),
            "oct-git_branch U+F418 rasterized with no coverage at all"
        );
        eprintln!("oct-git_branch U+F418 still draws from the bundled icon face");
    }
}

#[test]
#[cfg(all(target_os = "linux", feature = "embedded-font"))]
fn with_no_cjk_face_the_broad_backstop_leaves_private_use_to_the_icon_face() {
    let backstop = LINUX_BROAD_BACKSTOPS[2];
    if !std::path::Path::new(backstop).is_file() {
        aterm_spec::host::could_not_run(format!("{backstop} is not installed here"));
    }
    let mut r = renderer();
    r.fallback_paths
        .retain(|p| !LINUX_BROAD_BACKSTOPS[..2].contains(&p.as_str()));
    r.seal_admitted_font_sources();
    assert!(
        r.fallback_chain
            .iter()
            .any(|f| f.path.as_deref() == Some(backstop)),
        "{backstop} must end the broad chain of a host with no CJK face"
    );
    let bundled_cov = collection_coverage(embedded_symbols_font());
    let contested: Vec<char> = face_coverage(backstop)
        .into_iter()
        .filter(|cp| bundled_cov.contains(cp))
        .filter_map(char::from_u32)
        .filter(|&ch| font_chain::is_private_use(ch))
        .collect();
    assert!(
        contested.contains(&'\u{F418}'),
        "{backstop} and the Nerd Font must both map oct-git_branch U+F418"
    );
    for &ch in &contested {
        let key = r.glyph_key(ch);
        assert_eq!(
            key.source,
            FaceId::RuntimeFallback,
            "U+{:04X} is a Nerd Font icon {backstop} also maps; it must draw \
             from the bundled icon face, not the broad tier",
            ch as u32
        );
    }
    let git_branch = r.glyph_key('\u{F418}');
    assert!(
        r.glyph_image(git_branch).bytes().iter().any(|&c| c > 0),
        "U+F418 has ink"
    );
    eprintln!(
        "{} private-use icon(s) left to the icon face past the broad backstop",
        contested.len()
    );
}

#[test]
#[cfg(windows)]
fn a_config_fallback_font_does_not_orphan_the_rest_of_the_chain() {
    let arial = "C:\\Windows\\Fonts\\arial.ttf";
    let malgun = "C:\\Windows\\Fonts\\malgun.ttf";
    if !std::path::Path::new(arial).is_file() || !std::path::Path::new(malgun).is_file() {
        aterm_spec::host::could_not_run("needs both arial.ttf and malgun.ttf");
    }
    let mut r = renderer();
    assert!(r.set_config_fallback_fonts(&[arial.to_string()]));
    r.debug_block_on_lazy_fallbacks();
    assert_eq!(
        r.debug_fallback_pick_path('\u{05D0}').as_deref(),
        Some(arial),
        "the config face must head the chain and serve what it covers"
    );
    let primary_weight = r.runtime_probe_ctx().primary_weight;
    let built_in =
        weight_matched_fallback_paths(vec![malgun.to_string()], 0, primary_weight, |p| {
            std::path::Path::new(p).is_file()
        });
    assert_eq!(
        r.debug_fallback_pick_path('\u{D55C}').as_deref(),
        Some(built_in[0].as_str()),
        "한 no longer reaches the built-in Hangul face — one `fallback_fonts` \
         line orphaned the whole built-in chain, and Windows has no broad \
         backstop to catch it, so this is the tofu again"
    );
}

#[cfg(feature = "embedded-font")]
#[test]
fn sealing_a_generation_does_not_fontdue_parse_the_fallback_or_symbol_faces() {
    if present_fallback_paths().is_empty() {
        aterm_spec::host::could_not_run("no built-in fallback candidate exists on this machine");
    }
    let mut r = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("embedded font builds a renderer");
    r.fallback_paths = present_fallback_paths();
    (r.symbol_fallback_paths, r.symbol_user_prefix) = symbol_fallback_candidate_paths(&[]);
    let sources = r.seal_admitted_font_sources();
    assert!(
        !sources.fallback.is_empty(),
        "sealing must still admit the broad fallback chain"
    );
    for face in &r.fallback_chain {
        assert!(
            !face.font.is_materialised(),
            "seal fontdue-parsed chain entry {:?}",
            face.path
        );
    }
    for sym in &r.symbol_chain {
        assert!(
            !sym.font.is_materialised(),
            "seal fontdue-parsed symbol entry {:?}",
            sym.path
        );
    }
}

#[test]
fn lazy_styled_face_probes_only_the_slots_it_needs() {
    const BOLD_ITALIC: StyleBits = StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0);
    for (si, style) in [
        StyleBits::REGULAR,
        StyleBits::BOLD,
        StyleBits::ITALIC,
        BOLD_ITALIC,
    ]
    .into_iter()
    .enumerate()
    {
        for injected in [false, true] {
            for bits in 0u8..8 {
                let covers = [bits & 1 != 0, bits & 2 != 0, bits & 4 != 0];
                let mut probed = [false; 3];
                let lazy = resolve_styled_face_lazy(style, injected, |slot| {
                    probed[slot] = true;
                    covers[slot]
                });
                assert_eq!(
                    lazy,
                    resolve_styled_face(style, injected, covers),
                    "lazy and eager forms disagree at style {si} injected={injected} covers={covers:?}"
                );
                let expected: [bool; 3] = match (si, injected, covers[2]) {
                    (0, _, _) => [false, false, false],
                    (1, true, _) => [false, false, false],
                    (1, false, _) => [true, false, false],
                    (2, _, _) => [false, true, false],
                    (3, _, true) => [false, false, true],
                    (3, true, false) => [false, false, true],
                    (3, false, false) => [true, !covers[0], true],
                    _ => unreachable!("only four styles exist"),
                };
                assert_eq!(
                    probed, expected,
                    "probe set drifted at style {si} injected={injected} covers={covers:?}"
                );
            }
        }
    }
}

fn bundled_path_backed_renderer() -> Renderer {
    let path = aterm_tempfile::manifest_dir().join("assets/bundled/JetBrainsMono-Regular.ttf");
    let mut r = Renderer::from_bytes(BUNDLED_MONO, 32.0, Theme::default())
        .expect("the bundled face builds a renderer");
    r.primary_path = Some(
        path.into_os_string()
            .into_string()
            .expect("a UTF-8 package directory"),
    );
    r
}

#[test]
fn sealing_a_generation_does_not_fontdue_parse_the_styled_siblings() {
    let mut r = bundled_path_backed_renderer();
    let sources = r.seal_admitted_font_sources();
    assert!(
        sources.styled.iter().any(Option::is_some),
        "sealing must still ADMIT the real styled siblings"
    );
    for (slot, face) in r.styled_faces.iter().enumerate() {
        if let Some(sf) = face {
            assert!(
                !sf.font.is_materialised(),
                "seal fontdue-parsed styled slot {slot}"
            );
        }
    }
}

#[test]
fn a_bold_cell_parses_only_the_face_it_actually_draws_from() {
    let mut r = bundled_path_backed_renderer();
    let key = r.glyph_key_styled('A', StyleBits::BOLD);
    let img = r.glyph_image(key);
    let GlyphImage::Mono { bytes, .. } = img else {
        panic!("a bold 'A' is a Mono glyph");
    };
    assert!(bytes.iter().any(|&b| b != 0), "bold 'A' must draw ink");
    assert_eq!(
        r.debug_styled_face_indices()[0],
        Some(0),
        "the bold sibling must have been ROUTED to (else this proves nothing)"
    );
    for (slot, face) in r.styled_faces.iter().enumerate() {
        if let Some(sf) = face {
            assert!(
                !sf.font.is_materialised(),
                "the grid-fitted path drew from the BYTES, yet styled slot \
                 {slot} was fontdue-parsed"
            );
        }
    }

    r.debug_force_fontdue();
    let key = r.glyph_key_styled('A', StyleBits::BOLD);
    let _ = r.glyph_image(key);
    assert!(
        r.styled_faces[0]
            .as_ref()
            .is_some_and(|sf| sf.font.is_materialised()),
        "the fontdue raster path must materialise the face it draws from"
    );
    for slot in [1usize, 2] {
        if let Some(sf) = r.styled_faces[slot].as_ref() {
            assert!(
                !sf.font.is_materialised(),
                "a BOLD cell fontdue-parsed styled slot {slot}"
            );
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn retiring_a_styled_slot_drops_the_pointer_keyed_caches() {
    let mut r = bundled_path_backed_renderer();
    r.ensure_styled_faces();
    assert!(
        r.styled_faces[0].is_some(),
        "the bundled face's real -Bold sibling loads"
    );
    let key = r.glyph_key_styled('A', StyleBits::BOLD);
    let _ = r.glyph_image(key);
    let (bytes, index) = r.styled_faces[0]
        .as_ref()
        .map(|sf| (sf.bytes.clone(), sf.index))
        .expect("slot 0 is loaded");
    let kp = bytes.as_ptr() as usize;
    let (px, mode) = (r.px, r.hint_mode);
    let banked = r
        .hint_bank
        .instance(kp, &bytes, index, px, mode, &[], 0)
        .expect("a bold cell must have banked a hinting instance");
    let hit = r
        .hint_bank
        .instance(kp, &bytes, index, px, mode, &[], 0)
        .expect("the bank must serve the same instance twice");
    assert!(
        std::sync::Arc::ptr_eq(&banked, &hit),
        "the bank must be memoizing for this test to be a test"
    );

    assert!(r.retire_unparsable_styled(0), "a present slot must retire");

    let rebuilt = r
        .hint_bank
        .instance(kp, &bytes, index, px, mode, &[], 0)
        .expect("the instance still builds from the bytes this test holds");
    assert!(
        !std::sync::Arc::ptr_eq(&banked, &rebuilt),
        "the retire freed the slot's bytes but left ADDRESS-keyed instances behind — \
         a later allocation at that address would resurrect them"
    );
}

#[test]
fn a_styled_gid_whose_parse_fails_is_refused_not_drawn_from_another_face() {
    let mut r = bundled_path_backed_renderer();
    r.debug_force_fontdue();
    r.ensure_styled_faces();
    let sf = r.styled_faces[0]
        .as_mut()
        .expect("the bundled face's real -Bold sibling loads");
    let gid = sf.unicode_gid('A').expect("the -Bold sibling maps 'A'");
    sf.font = LazyFontdue::failed_parse();

    let (pm, pb) = r.font.rasterize_indexed(gid, r.px);
    assert!(
        pb.iter().any(|&b| b != 0),
        "the primary's glyph #{gid} must have ink for this test to be a test"
    );

    let key = r.ligature_key(gid, StyleBits::BOLD);
    assert!(
        matches!(r.run_face_pick(key.style), FacePick::Styled { slot: 0, .. }),
        "the key must route to the doomed styled slot"
    );
    let epoch = r.font_epoch;
    let GlyphImage::Mono {
        width,
        height,
        bytes,
        ..
    } = r.glyph_image(key).clone()
    else {
        panic!("a MonoGid key rasterizes to a Mono glyph");
    };
    assert!(
        bytes.iter().all(|&b| b == 0),
        "a gid whose face cannot be opened must draw NO ink; got {} set bytes",
        bytes.iter().filter(|&&b| b != 0).count()
    );
    assert_ne!(
        (width, height, bytes.as_slice()),
        (pm.width, pm.height, pb.as_slice()),
        "the refused gid was drawn from the PRIMARY face — a stranger's glyph"
    );
    assert!(
        r.font_epoch > epoch,
        "the refusal must stamp the frame cache stale so the next frame re-plans"
    );
    assert!(
        r.styled_faces[0].is_some(),
        "the slot must STAY present until a frame boundary: dropping it mid-frame \
         is what lets the rest of the in-flight plan re-pick another face"
    );
    let other = r.ligature_key(gid.wrapping_add(1), StyleBits::BOLD);
    let GlyphImage::Mono { bytes, .. } = r.glyph_image(other).clone() else {
        panic!("a MonoGid key rasterizes to a Mono glyph");
    };
    assert!(
        bytes.iter().all(|&b| b == 0),
        "a second orphaned gid was drawn from another face"
    );

    let mut term = Terminal::new(1, 8);
    term.process(b"ab");
    let input = term.cell_frame(1, 8);
    let mut plan = Vec::new();
    r.row_glyph_plan(&input, 0, &[], &mut plan);
    assert!(
        r.styled_faces[0].is_some(),
        "planning a row must NOT move face presence — only the frame boundary may"
    );

    let _ = r.render_input(&input);
    assert!(
        r.styled_faces[0].is_none(),
        "the pending retire must land at the frame boundary"
    );
    let key = r.glyph_key_styled('A', StyleBits::BOLD);
    let GlyphImage::Mono { bytes, .. } = r.glyph_image(key) else {
        panic!("bold 'A' must be a Mono glyph after the retire");
    };
    assert!(
        bytes.iter().any(|&b| b != 0),
        "bold 'A' must draw ink again once the slot is retired"
    );
}

#[test]
fn a_pending_styled_retire_never_condemns_a_replacement_face() {
    let mut r = bundled_path_backed_renderer();
    r.debug_force_fontdue();
    r.ensure_styled_faces();
    let sf = r.styled_faces[0]
        .as_mut()
        .expect("the bundled face's real -Bold sibling loads");
    let gid = sf.unicode_gid('A').expect("the -Bold sibling maps 'A'");
    sf.font = LazyFontdue::failed_parse();
    let doomed_id = sf.id;

    let _ = r.glyph_image(r.ligature_key(gid, StyleBits::BOLD)).clone();
    assert_eq!(
        r.styled_retire_pending[0],
        Some(doomed_id),
        "the refusal must condemn the face it actually failed to open"
    );

    let primary = r
        .shaping_primary_bytes
        .clone()
        .expect("a byte-loaded primary retains its bytes");
    r.set_styled_font_bytes(0, &primary)
        .expect("the primary's own bytes are a parseable styled face");
    let fresh_id = r.styled_faces[0]
        .as_ref()
        .expect("the injected face took the slot")
        .id;
    assert_ne!(fresh_id, doomed_id, "a new face is a new identity");
    assert_eq!(
        r.styled_retire_pending[0], None,
        "replacing the slot must drop the condemnation with the face it named"
    );

    r.styled_retire_pending[0] = Some(doomed_id);
    assert!(
        !r.apply_pending_styled_retires(),
        "a condemnation whose face has left the slot must retire NOTHING"
    );
    assert!(
        r.styled_faces[0].is_some(),
        "the innocent replacement must survive the boundary"
    );
    assert_eq!(
        r.styled_retire_pending[0], None,
        "a spent condemnation must not linger to fire at a later boundary"
    );

    assert!(
        r.styled_faces[0]
            .as_ref()
            .is_some_and(|sf| sf.parsed().is_some()),
        "the surviving face must be the one that parses"
    );
    assert!(
        matches!(
            r.run_face_pick(StyleBits::BOLD),
            FacePick::Styled { slot: 0, .. }
        ),
        "bold must still be answered by the injected styled face"
    );
    let key = r.glyph_key_styled('A', StyleBits::BOLD);
    let GlyphImage::Mono { bytes, .. } = r.glyph_image(key) else {
        panic!("bold 'A' must be a Mono glyph");
    };
    assert!(
        bytes.iter().any(|&b| b != 0),
        "the surviving face must actually draw"
    );
}

#[test]
fn the_gpu_backends_frame_boundary_lands_a_pending_styled_retire() {
    let mut r = bundled_path_backed_renderer();
    r.debug_force_fontdue();
    r.ensure_styled_faces();
    let sf = r.styled_faces[0]
        .as_mut()
        .expect("the bundled face's real -Bold sibling loads");
    let gid = sf.unicode_gid('A').expect("the -Bold sibling maps 'A'");
    sf.font = LazyFontdue::failed_parse();

    let _ = r.glyph_image(r.ligature_key(gid, StyleBits::BOLD)).clone();
    assert!(
        r.styled_retire_pending[0].is_some(),
        "the refusal must leave a pending retire for the boundary to land"
    );
    r.evict_caches_if_large();
    assert!(
        r.styled_faces[0].is_none(),
        "the GPU backend's per-frame crossing must land the pending retire"
    );
    assert_eq!(
        r.styled_retire_pending[0], None,
        "the condemnation is spent at the boundary"
    );
}

#[test]
fn a_deferred_styled_face_and_the_chrome_share_one_copy_of_the_file() {
    let mut r = bundled_path_backed_renderer();
    let (chrome_bytes, index) = r
        .chrome_bold_face()
        .expect("the bundled face's real -Bold sibling loads");
    let slot_bytes = r.styled_faces[0]
        .as_ref()
        .map(|sf| sf.bytes.clone())
        .expect("chrome_bold_face returned the slot's bytes");
    assert!(
        std::sync::Arc::ptr_eq(&chrome_bytes, &slot_bytes),
        "the chrome must be handed the slot's own handle, not a copy"
    );
    assert!(
        r.styled_faces[0]
            .as_ref()
            .is_some_and(|sf| !sf.font.is_materialised()),
        "the slot must still be UNPARSED — that is the case the store used to miss"
    );

    let (published, chrome_font) =
        shared_parsed_face(&chrome_bytes, index).expect("the bold sibling parses");
    assert!(
        std::sync::Arc::ptr_eq(&published, &slot_bytes),
        "the chrome's parse published a SECOND copy of the -Bold file"
    );

    let styled_font = r.styled_faces[0]
        .as_ref()
        .and_then(StyledFace::parsed)
        .cloned()
        .expect("the admitted sibling parses");
    assert!(
        std::sync::Arc::ptr_eq(&styled_font, &chrome_font),
        "the styled tier re-parsed a face the chrome had already parsed"
    );
    assert!(
        r.styled_faces[0]
            .as_ref()
            .is_some_and(|sf| std::sync::Arc::ptr_eq(&sf.bytes, &slot_bytes)),
        "materialising must not have moved the slot to different bytes"
    );
}

#[test]
fn a_live_face_is_never_evicted_from_the_shared_store() {
    let tag = format!("shared-store test {:?}", std::thread::current().id());
    let face = |i: usize| format!("{tag} face {i}").into_bytes();
    let held: Vec<std::sync::Arc<[u8]>> = (0..40).map(|i| shared_face_bytes(&face(i), 0)).collect();
    for (i, bytes) in held.iter().enumerate() {
        assert!(
            std::sync::Arc::ptr_eq(&shared_face_bytes(&face(i), 0), bytes),
            "face {i} of 40 live ones was evicted and copied"
        );
    }
    drop(held);
    let mut store = SHARED_PARSED_FACES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    shared_faces_prune(&mut store);
    assert!(
        !store.iter().any(|e| e.bytes.starts_with(tag.as_bytes())),
        "a face nobody holds stayed in the store"
    );
}

#[test]
fn format2_coverage_rejects_low_byte_alias_and_keeps_real_two_byte_mapping() {
    fn synthetic_format2_cmap(two_byte_key: bool) -> Vec<u8> {
        fn push_u16(out: &mut Vec<u8>, value: u16) {
            out.extend_from_slice(&value.to_be_bytes());
        }
        fn push_u32(out: &mut Vec<u8>, value: u32) {
            out.extend_from_slice(&value.to_be_bytes());
        }

        let mut subtable = Vec::new();
        push_u16(&mut subtable, 2);
        push_u16(&mut subtable, 0);
        push_u16(&mut subtable, 0);
        for high_byte in 0..256 {
            push_u16(
                &mut subtable,
                if two_byte_key && high_byte == 0x23 {
                    // Subheader 1 (a subHeaderKeys entry is the index × 8).
                    8
                } else {
                    // Subheader 0.
                    0
                },
            );
        }

        if two_byte_key {
            push_u16(&mut subtable, 0x18);
            push_u16(&mut subtable, 1);
            push_u16(&mut subtable, 0);
            push_u16(&mut subtable, 10);
            push_u16(&mut subtable, 0x18);
            push_u16(&mut subtable, 1);
            push_u16(&mut subtable, 0);
            push_u16(&mut subtable, 4);
            push_u16(&mut subtable, 1);
            push_u16(&mut subtable, 2);
        } else {
            push_u16(&mut subtable, 0x18);
            push_u16(&mut subtable, 1);
            push_u16(&mut subtable, 0);
            push_u16(&mut subtable, 2);
            push_u16(&mut subtable, 1);
        }
        let length = u16::try_from(subtable.len()).expect("fixture fits format-2 length");
        subtable[2..4].copy_from_slice(&length.to_be_bytes());

        let mut cmap = Vec::new();
        push_u16(&mut cmap, 0);
        push_u16(&mut cmap, 1);
        push_u16(&mut cmap, 1);
        push_u16(&mut cmap, 25);
        push_u32(&mut cmap, 12);
        cmap.extend_from_slice(&subtable);
        cmap
    }

    fn enumerated_nonzero(cmap: &[u8], ch: char) -> bool {
        let table = aterm_font::cmap::Table::parse(cmap).expect("fixture cmap parses");
        let subtable = table.subtables.get(0).expect("fixture subtable parses");
        let mut found = false;
        subtable.codepoints(|codepoint| {
            if codepoint == ch as u32
                && subtable
                    .glyph_index(codepoint)
                    .is_some_and(|glyph| glyph.0 != 0)
            {
                found = true;
            }
        });
        found
    }

    let aliased = synthetic_format2_cmap(false);
    let alias_table = aterm_font::cmap::Table::parse(&aliased).expect("alias cmap parses");
    let alias_subtable = alias_table.subtables.get(0).expect("alias subtable parses");
    assert_eq!(
        alias_subtable.glyph_index('⌘' as u32),
        Some(aterm_font::GlyphId(1)),
        "raw format-2 query reproduces the low-byte false positive"
    );
    assert!(!enumerated_nonzero(&aliased, '⌘'));
    assert!(!fontdue_format2_enumerates(&aliased, 0, '⌘'));
    assert!(enumerated_nonzero(&aliased, '\u{18}'));
    assert!(fontdue_format2_enumerates(&aliased, 0, '\u{18}'));

    let genuine = synthetic_format2_cmap(true);
    let genuine_table = aterm_font::cmap::Table::parse(&genuine).expect("genuine cmap parses");
    let genuine_subtable = genuine_table
        .subtables
        .get(0)
        .expect("genuine subtable parses");
    assert_eq!(
        genuine_subtable.glyph_index('⌘' as u32),
        Some(aterm_font::GlyphId(2))
    );
    assert!(enumerated_nonzero(&genuine, '⌘'));
    assert!(fontdue_format2_enumerates(&genuine, 0, '⌘'));
}

#[test]
fn styled_coverage_probe_agrees_with_fontdue() {
    let mut paths: Vec<String> = present_fallback_paths();
    paths.extend(
        symbol_discovery_paths()
            .into_iter()
            .filter(|p| std::path::Path::new(p).is_file()),
    );
    paths.extend(
        font_files()
            .into_iter()
            .filter_map(|p| p.to_str().map(str::to_string))
            .filter(|p| {
                std::fs::metadata(p).is_ok_and(|m| m.len() > 0 && m.len() < 2 * 1024 * 1024)
            })
            .take(30),
    );
    paths.sort();
    paths.dedup();
    let sample: Vec<char> = ('\u{20}'..='\u{7E}')
        .chain("éÈàñöÿ£¥§°±µ¶".chars())
        .chain("αβγΔΩ".chars())
        .chain("абвЯ".chars())
        .chain("中文한국어".chars())
        .chain("←→↔∑√∞≈⌘⏸⏹⏺".chars())
        .chain("🚀🎉".chars())
        .chain(['\u{0378}', '\u{E000}', '\u{10FFFD}'])
        .collect();
    let mut checked = 0usize;
    for p in &paths {
        let Ok(bytes) = font_file::read_font_file(std::path::Path::new(p)) else {
            continue;
        };
        let Ok(font) =
            crate::font::Font::from_bytes(bytes.as_slice(), crate::font::FontSettings::default())
        else {
            continue;
        };
        for &ch in &sample {
            assert_eq!(
                fontdue_cmap_covers(&bytes, 0, ch),
                font.lookup_glyph_index(ch) != 0,
                "coverage probe disagreed with the face for {ch:?} on {p}"
            );
        }
        checked += 1;
    }
    assert!(checked > 0, "no font file was readable to check against");
    eprintln!(
        "coverage probe agreed with fontdue on {checked} face(s) x {} chars",
        sample.len()
    );
}

#[cfg(feature = "embedded-font")]
#[test]
fn probing_symbol_coverage_does_not_fontdue_parse_the_symbol_face() {
    let mut r = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("embedded font builds a renderer");
    r.symbol_fallback_paths = vec![asset_path("bundled/NotoSansSymbols2-Regular.ttf")];
    r.symbol_user_prefix = 0;
    r.debug_block_on_lazy_fallbacks();
    let before = r
        .symbol_chain
        .first()
        .map(|f| f.font.clone())
        .expect("the symbol face loads into the chain");
    assert!(
        !before.is_materialised(),
        "the symbol face must arrive unparsed"
    );
    let covered = r.symbol_fallback_has('\u{23F8}', false);
    assert!(
        !before.is_materialised(),
        "probing ⏸ coverage fontdue-parsed the symbol face (covered={covered})"
    );
}

#[cfg(feature = "embedded-font")]
#[test]
fn ready_fallback_faces_admits_only_parsed_faces_and_never_parses() {
    let mut r = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("embedded font builds a renderer");
    r.fallback_paths = present_fallback_paths();
    r.debug_block_on_lazy_fallbacks();
    let deferred = r.fallback_chain.len();
    assert_eq!(
        r.ready_fallback_faces().count(),
        0,
        "a deferred chain face is not ready"
    );
    assert!(
        r.fallback_chain
            .iter()
            .all(|face| !face.font.is_materialised()),
        "reading readiness parsed a deferred face"
    );
    r.add_fallback_bytes(embedded_font())
        .expect("the embedded face injects");
    let ready: Vec<_> = r.ready_fallback_faces().cloned().collect();
    assert_eq!(ready.len(), 1, "the eager injection is ready at once");
    let injected = &r.fallback_chain[deferred];
    assert!(
        std::sync::Arc::ptr_eq(&ready[0], injected.font.parsed().expect("eager cell")),
        "the ready face is the chain's own parse, by identity"
    );
    assert!(
        r.fallback_chain[..deferred]
            .iter()
            .all(|face| !face.font.is_materialised()),
        "the deferred faces stayed deferred"
    );
}

#[cfg(feature = "embedded-font")]
#[test]
fn fallback_coverage_materialises_only_the_face_a_char_needs() {
    let mut r = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("embedded font builds a renderer");
    r.fallback_paths = vec![asset_path("bundled/NotoSansMath-Regular.ttf")];
    r.fallback_user_prefix = 0;
    r.symbol_fallback_paths = vec![asset_path("bundled/NotoSansSymbols2-Regular.ttf")];
    r.symbol_user_prefix = 0;
    r.debug_block_on_lazy_fallbacks();
    let math = r
        .fallback_chain
        .first()
        .cloned()
        .expect("the math face loads");
    let symbols = r
        .symbol_chain
        .first()
        .cloned()
        .expect("the symbol face loads");
    let pause = '\u{23F8}';
    assert!(
        fontdue_cmap_covers(&symbols.bytes, symbols.index, pause)
            && !fontdue_cmap_covers(&math.bytes, math.index, pause),
        "fixture: only the symbol face maps {pause:?}"
    );
    let coverage = r.fallback_coverage();
    assert_eq!(
        coverage.materialise_for("\u{10FFFD}"),
        0,
        "a char no face maps parses nothing"
    );
    assert!(!math.font.is_materialised() && !symbols.font.is_materialised());
    assert_eq!(
        coverage.materialise_for("⏸ ⏸"),
        1,
        "one face, once, for a repeated char"
    );
    assert!(
        symbols.font.is_materialised(),
        "the face that maps ⏸ is parsed"
    );
    assert!(
        !math.font.is_materialised(),
        "the face that does not map ⏸ stays deferred"
    );
    let ready: Vec<_> = r.ready_fallback_faces().cloned().collect();
    assert_eq!(ready.len(), 1);
    assert!(
        std::sync::Arc::ptr_eq(&ready[0], symbols.font.parsed().expect("parsed")),
        "the renderer reads the chrome's parse, by identity"
    );
    assert_eq!(
        coverage.materialise_for("⏸"),
        0,
        "a landed face is not parsed again"
    );
}

#[test]
fn admission_agrees_with_the_first_party_face() {
    let mut paths: Vec<String> = present_fallback_paths();
    paths.extend(
        symbol_discovery_paths()
            .into_iter()
            .filter(|p| std::path::Path::new(p).is_file()),
    );
    paths.extend(
        font_files()
            .into_iter()
            .filter_map(|p| p.to_str().map(str::to_string))
            .filter(|p| {
                std::fs::metadata(p).is_ok_and(|m| m.len() > 0 && m.len() < 2 * 1024 * 1024)
            })
            .take(30),
    );
    paths.sort();
    paths.dedup();
    let mut checked = 0usize;
    for p in &paths {
        let Ok(bytes) = font_file::read_font_file(std::path::Path::new(p)) else {
            continue;
        };
        let predicted = face_admissible(&bytes, 0);
        let actual =
            crate::font::Font::from_bytes(bytes.as_slice(), crate::font::FontSettings::default())
                .is_ok();
        assert_eq!(
            predicted, actual,
            "face_admissible disagreed with the face it admits, for {p}"
        );
        checked += 1;
    }
    assert!(checked > 0, "no font file was readable to check against");
    eprintln!("admission agreed with the first-party face on {checked} face(s)");
}

#[test]
fn the_coverage_scan_gives_each_font_the_same_answer_a_private_buffer_would() {
    let paths: Vec<std::path::PathBuf> = font_files()
        .into_iter()
        .filter(|p| !p.to_str().is_some_and(path_is_last_resort))
        .collect();

    let mut shared_bytes: Vec<u8> = Vec::new();
    let mut shared_cps: Vec<u32> = Vec::new();
    let mut checked = 0usize;
    let mut with_ranges = 0usize;
    for path in &paths {
        let Ok(private) = font_file::read_font_file(path) else {
            continue;
        };
        let want_wi = aterm_font::Face::parse(&private, 0)
            .map(|f| (f.weight().to_number(), f.is_italic()))
            .unwrap_or((400, false));
        let want = font_cmap_ranges(&private, &mut Vec::new());
        drop(private);

        assert!(
            font_file::read_font_file_into(path, &mut shared_bytes).is_ok(),
            "the reused-buffer read failed where a fresh one succeeded for {path:?}"
        );
        let got_wi = aterm_font::Face::parse(&shared_bytes, 0)
            .map(|f| (f.weight().to_number(), f.is_italic()))
            .unwrap_or((400, false));
        let got = font_cmap_ranges(&shared_bytes, &mut shared_cps);

        assert_eq!(want_wi, got_wi, "weight/italic drifted for {path:?}");
        assert_eq!(want, got, "cmap coverage drifted for {path:?}");
        checked += 1;
        with_ranges += usize::from(got.is_some());
    }
    assert!(
        checked > 1,
        "need at least two readable fonts to share state"
    );
    assert!(with_ranges > 0, "no font yielded coverage ranges");
    eprintln!("coverage scan agreed on {checked} face(s), {with_ranges} with ranges");
}

#[cfg(feature = "embedded-font")]
fn patch_num_glyphs(bytes: &[u8], n: u16) -> Option<Vec<u8>> {
    let num_tables = u16::from_be_bytes([*bytes.get(4)?, *bytes.get(5)?]) as usize;
    for i in 0..num_tables {
        let rec = 12 + i * 16;
        if bytes.get(rec..rec + 16)?.starts_with(b"maxp") {
            let off = u32::from_be_bytes(bytes[rec + 8..rec + 12].try_into().ok()?) as usize;
            let mut out = bytes.to_vec();
            out.get_mut(off + 4..off + 6)?
                .copy_from_slice(&n.to_be_bytes());
            return Some(out);
        }
    }
    None
}

#[cfg(feature = "embedded-font")]
#[test]
fn a_face_fontdue_rejected_is_admitted_and_draws() {
    let patched = patch_num_glyphs(embedded_symbols_font(), 1)
        .expect("the bundled symbols face has a maxp table");
    assert!(
        face_admissible(&patched, 0),
        "admission must accept what the first-party face can parse"
    );
    let font =
        crate::font::Font::from_bytes(patched.as_slice(), crate::font::FontSettings::default())
            .expect("the first-party face parses what fontdue refused");
    let (notdef_m, notdef) = font.rasterize_indexed(0, 16.0);
    assert!(
        notdef_m.width > 0 && notdef.iter().any(|&c| c > 0),
        "the symbols face's .notdef must be a real, inked glyph, or this \
         proves nothing about reaching it"
    );
    for ch in ['\u{f005}', '\u{e0b0}', 'A'] {
        assert_eq!(font.lookup_glyph_index(ch), 0, "{ch:?} must be unmapped");
        let (m, cov) = font.rasterize(ch, 16.0);
        assert_eq!(
            (m.width, m.height, m.xmin, m.ymin),
            (
                notdef_m.width,
                notdef_m.height,
                notdef_m.xmin,
                notdef_m.ymin
            ),
            "{ch:?} must draw .notdef"
        );
        assert_eq!(cov, notdef, "{ch:?} must draw .notdef's coverage");
    }
    assert!(
        FallbackFace::from_path_bytes(
            &crate::font::FaceBytes::Vec(std::sync::Arc::new(patched.clone())),
            Some("patched".into())
        )
        .is_ok(),
        "and such a face now enters the chain rather than being refused"
    );
}

#[cfg(feature = "embedded-font")]
#[test]
fn a_chain_face_whose_deferred_parse_fails_is_retired_not_left_shadowing() {
    let mut r = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("embedded font builds a renderer");
    #[cfg(target_os = "macos")]
    {
        r.rasterizer = RasterKind::Fontdue;
    }
    r.fallback_paths.clear();
    r.symbol_fallback_paths.clear();
    let good = FallbackFace::from_bytes(embedded_font(), Some("good".into()))
        .expect("bundled face is admissible");
    let bad = FallbackFace {
        font: LazyFontdue::failed_parse(),
        bytes: crate::font::FaceBytes::Vec(intern_font_bytes_slice(embedded_symbols_font())),
        index: 0,
        path: Some("bad".into()),
        relocated_from: None,
        scope: None,
        chosen: false,
        norm: FaceNorm::default(),
    };
    r.fallback_chain = vec![bad, good];
    r.fallback_pick.insert('Z', 0);
    let key = GlyphKey::mono_char(FaceId::Fallback, 'Z', StyleBits::REGULAR, r.px_q);
    let img = r.glyph_image(key).clone();
    assert_eq!(r.fallback_chain.len(), 1, "the bad entry must be retired");
    assert_eq!(
        r.fallback_chain[0].path.as_deref(),
        Some("good"),
        "the surviving entry must be the good one"
    );
    assert_eq!(
        r.fallback_pick.get(&'Z').copied(),
        Some(0),
        "the pick must be re-probed against the shortened chain"
    );
    assert!(
        img.bytes().iter().any(|&b| b > 0),
        "'Z' must render real coverage from the surviving entry, not blank tofu"
    );
}

#[cfg(feature = "embedded-font")]
#[test]
fn retiring_a_fallback_entry_clears_the_memos_that_outlived_it() {
    let mut r = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("embedded font builds a renderer");
    let first = FallbackFace::from_bytes(embedded_symbols_font(), Some("first".into()))
        .expect("bundled symbols face is admissible");
    let second = FallbackFace::from_bytes(embedded_font(), Some("second".into()))
        .expect("bundled face is admissible");
    let doomed = first.bytes.clone();
    r.fallback_chain = vec![first, second];
    r.fallback_pick.insert('Z', 0);
    let key = GlyphKey::mono_char(FaceId::Fallback, 'Z', StyleBits::REGULAR, r.px_q);
    r.keys.insert(('Z', ScalarPresentation::Default), key);
    let _ = r.glyph_image(key);
    let epoch = r.font_epoch;

    assert!(
        r.retire_unparsable_fallback(&doomed),
        "the entry backed by these bytes must be found"
    );
    assert_eq!(r.fallback_chain.len(), 1);
    assert_eq!(r.fallback_chain[0].path.as_deref(), Some("second"));
    assert!(r.fallback_pick.is_empty(), "stale indices must be dropped");
    assert!(r.keys.is_empty(), "stale routing must be dropped");
    assert!(r.glyphs.is_empty(), "stale rasters must be dropped");
    assert!(r.font_epoch > epoch, "a retire must force a repaint");

    assert!(
        !r.retire_unparsable_fallback(&doomed),
        "retiring an absent face must report that it removed nothing"
    );

    // Negative fallback-miss and symbol-pick memos cache repeated chain probes and auto-invalidate
    // when either chain mutates.
    r.runtime_discovery = false;
    let missing = '\u{10FFFF}';
    assert!(!r.fallback_has(missing));
    assert!(
        r.fallback_miss.chars.contains(&missing),
        "uncovered code point must record a fallback miss"
    );
    assert!(!r.fallback_has(missing));
    r.symbol_chain = vec![
        FallbackFace::from_bytes(embedded_symbols_font(), Some("sym".into()))
            .expect("bundled symbols face is admissible"),
    ];
    assert_eq!(r.symbol_chain_pick(missing), None);
    assert_eq!(r.symbol_pick.entries.get(&missing).copied(), Some(None));
    let sym_hit = r.symbol_chain_pick('\u{E0A0}');
    assert_eq!(
        r.symbol_pick.entries.get(&'\u{E0A0}').copied(),
        Some(sym_hit)
    );
    r.symbol_chain.clear();
    assert_eq!(r.symbol_chain_pick('\u{E0A0}'), None);
}

#[test]
fn glyph_cache_caps_trip_independently() {
    let mut r = renderer();
    let key = r.glyph_key('A');
    let _ = r.glyph_image(key);
    r.deco_mask(DecoGlyph::Dot, 3, 5);
    assert!(!r.glyphs.is_empty() && !r.keys.is_empty());

    for i in 0..=16_384usize {
        r.cluster_shapes.insert(&format!("c{i}"), None);
    }
    r.evict_caches_if_large();
    let (cur, prev) = r.cluster_shapes.generations();
    assert_eq!(
        cur, 0,
        "cluster cap must rotate (demote the current generation)"
    );
    assert!(
        prev > 0,
        "rotation retains the prior generation in prev (not a wipe)"
    );
    assert!(
        !r.glyphs.is_empty() && !r.keys.is_empty(),
        "a cluster overflow must NOT wipe the glyph caches"
    );
    assert!(!r.deco_masks.borrow().is_empty());

    for i in 0..=16_384usize {
        r.shaped_runs
            .insert(StyleBits::REGULAR, format!("r{i}").into_boxed_str(), None);
    }
    r.evict_caches_if_large();
    assert!(
        r.shaped_runs.cur.is_empty(),
        "run cap must rotate (demote the current generation), leaving cur empty"
    );
    assert!(
        !r.shaped_runs.prev.is_empty(),
        "rotation RETAINS the prior generation in prev (not a wipe)"
    );
    assert!(
        !r.glyphs.is_empty(),
        "a shaped-run overflow must NOT wipe the glyph cache"
    );

    for gid in 0..=16_384u16 {
        r.glyphs.insert(
            GlyphKey::mono_gid(gid, StyleBits::REGULAR, r.px_q),
            CachedGlyph::new(GlyphImage::Mono {
                width: 0,
                height: 0,
                xmin: 0,
                ymin: 0,
                advance: 0.0,
                bytes: Vec::new(),
            }),
        );
    }
    r.evict_caches_if_large();
    assert!(r.glyphs.is_empty(), "glyph cap must clear the bitmaps");
    assert!(
        r.keys.is_empty() && r.emoji_keys.is_empty() && r.styled_keys.is_empty(),
        "the key maps are bounded by riding the glyph cap"
    );
    assert!(
        r.deco_masks.borrow().is_empty(),
        "deco masks are bounded by riding the glyph cap"
    );
}

#[test]
fn shaped_run_cache_two_gen_retains_hot_evicts_cold() {
    let mut c = ShapedRunCache::default();
    let hot = "prompt$ ".to_string();

    c.insert(StyleBits::REGULAR, hot.clone().into_boxed_str(), None);
    assert!(matches!(
        c.get(StyleBits::REGULAR, &hot),
        Some(ShapedHit::Cur(_))
    ));

    c.rotate();
    assert!(c.cur.is_empty(), "rotate empties cur");
    assert!(
        matches!(c.get(StyleBits::REGULAR, &hot), Some(ShapedHit::Prev(_))),
        "a run survives one rotation in prev (served, not re-shaped)"
    );

    c.insert(StyleBits::REGULAR, hot.clone().into_boxed_str(), None);
    c.rotate();
    assert!(
        c.get(StyleBits::REGULAR, &hot).is_some(),
        "a run promoted each generation is never evicted"
    );

    let mut cold = ShapedRunCache::default();
    cold.insert(
        StyleBits::REGULAR,
        "cold-run".to_string().into_boxed_str(),
        None,
    );
    cold.rotate();
    cold.rotate();
    assert!(
        cold.get(StyleBits::REGULAR, "cold-run").is_none(),
        "an un-promoted run is evicted after two rotations"
    );

    let mut both = ShapedRunCache::default();
    both.insert(StyleBits::REGULAR, "a".to_string().into_boxed_str(), None);
    both.rotate();
    both.insert(StyleBits::REGULAR, "b".to_string().into_boxed_str(), None);
    both.clear();
    assert!(
        both.cur.is_empty() && both.prev.is_empty(),
        "clear() must wipe BOTH generations so no stale-font run survives"
    );
}

const FLOOD_BUCKET_BITS: u32 = 8;

fn probe_class(h: u64) -> (u64, u64) {
    (h & ((1 << FLOOD_BUCKET_BITS) - 1), h >> 57)
}

fn flood_key(mut i: u32) -> String {
    let mut s = String::from("x");
    for _ in 0..5 {
        s.extend(char::from_u32(0xE0020 + i % 96));
        i /= 96;
    }
    s
}

fn worst_probe_class(keys: &[String], hasher: &impl std::hash::BuildHasher) -> usize {
    let mut classes: FxHashMap<(u64, u64), usize> = FxHashMap::default();
    keys.iter()
        .map(|k| {
            let n = classes
                .entry(probe_class(hasher.hash_one(k.as_str())))
                .or_default();
            *n += 1;
            *n
        })
        .max()
        .unwrap_or(0)
}

#[test]
fn text_caches_spread_a_precomputed_colliding_key_set() {
    use std::hash::BuildHasher as _;
    let mut flood = Vec::with_capacity(128);
    let mut i = 0u32;
    while flood.len() < 128 {
        let k = flood_key(i);
        if probe_class(aterm_hash::FxBuildHasher.hash_one(k.as_str())) == (0, 0) {
            flood.push(k);
        }
        i += 1;
    }
    assert_eq!(
        worst_probe_class(&flood, &aterm_hash::FxBuildHasher),
        flood.len()
    );
    let ordinary: Vec<String> = (0..128u32).map(|j| flood_key(j * 7919 + 13)).collect();

    let mut clusters = TwoGenTextCache::default();
    let mut runs = ShapedRunCache::default();
    for (n, k) in flood.iter().enumerate() {
        clusters.insert(k.as_str().into(), n);
        runs.insert(StyleBits::REGULAR, k.as_str().into(), None);
    }
    for (n, k) in flood.iter().enumerate() {
        assert_eq!(clusters.get(k), Some((&n, false)));
        assert!(matches!(
            runs.get(StyleBits::REGULAR, k),
            Some(ShapedHit::Cur(None))
        ));
    }
    let run_map = &runs.cur[&StyleBits::REGULAR];
    for (cache, hasher) in [
        ("TwoGenTextCache", clusters.cur.hasher()),
        ("ShapedRunCache", run_map.hasher()),
    ] {
        let worst = worst_probe_class(&flood, hasher);
        assert!(
            worst <= 8,
            "{cache}: {worst} of {} precomputed keys share one bucket and \
             tag (ordinary keys: {}), so a lookup among them compares that \
             many strings",
            flood.len(),
            worst_probe_class(&ordinary, hasher)
        );
    }
}

#[test]
#[cfg(target_os = "macos")]
fn runtime_fallback_rejected_candidates_cached_after_resolve() {
    let mut rf = RuntimeFallback::default();
    let ctx = FallbackProbeCtx {
        use_ct: false,
        primary_weight: 400,
        primary_italic: false,
    };
    let ch = '中';
    let idx = rf
        .resolve(ch, ctx)
        .expect("CJK faces ship on every Mac: the runtime resolver finds 中");
    let accepted = rf.faces[idx].path.clone();
    let cands = runtime_fallback_candidate_paths(ch, ctx);
    let accepted_pos = cands
        .iter()
        .position(|p| *p == accepted)
        .expect("accepted face came from the candidate list");
    assert!(
        accepted_pos <= RuntimeFallback::MAX_FACES,
        "中 was accepted after {accepted_pos} rejected candidates, past MAX_FACES"
    );
    for p in &cands[..accepted_pos] {
        assert!(
            rf.faces.iter().any(|f| f.path == *p) || rf.failed_paths.contains(p),
            "candidate {p} was tried and must be cached (face or failed), not re-parsed for the next code point"
        );
    }
    let _ = rf.resolve('文', ctx);
    let mut paths: Vec<String> = rf.faces.iter().map(|f| f.path.clone()).collect();
    let total = paths.len();
    paths.sort();
    paths.dedup();
    assert_eq!(
        paths.len(),
        total,
        "no font path may be parsed into `faces` twice"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn coretext_glyph_orientation_is_top_first_like_fontdue() {
    let bytes = system_font_bytes();
    let px = 32.0;
    fn top_heavy(b: &[u8], w: usize, h: usize) -> bool {
        if w == 0 || h < 2 {
            return false;
        }
        let mid = h / 2;
        let top: u64 = b[..mid * w].iter().map(|&v| u64::from(v)).sum();
        let bot: u64 = b[mid * w..h * w].iter().map(|&v| u64::from(v)).sum();
        top > bot
    }

    let mut fd = Renderer::from_bytes(&bytes, px, Theme::default()).expect("renderer");
    fd.debug_force_fontdue();
    let kfd = fd.glyph_key('T');
    let ifd = fd.glyph_image(kfd).clone();
    assert!(
        top_heavy(ifd.bytes(), ifd.width(), ifd.height()),
        "sanity: upright 'T' is top-heavy under fontdue"
    );

    let mut ct = Renderer::from_bytes(&bytes, px, Theme::default()).expect("renderer");
    ct.rasterizer = RasterKind::CoreText;
    let kct = ct.glyph_key('T');
    let ict = ct.glyph_image(kct).clone();
    assert!(
        top_heavy(ict.bytes(), ict.width(), ict.height()),
        "CoreText 'T' must be top-heavy like fontdue — a row flip would render it (and all text) upside-down"
    );
}

fn prove_faithful_over_all_scalars(bytes: &[u8], label: &str) -> (u64, u64) {
    let oracle = aterm_font::Face::parse(bytes, 0).expect("aterm-font parses the font");
    let fd = crate::font::Font::from_bytes(bytes, crate::font::FontSettings::default())
        .expect("crate::font parses the font");
    let mut r = Renderer::from_bytes(bytes, 16.0, Theme::default()).expect("renderer");
    let (mut covered, mut cmap_divergences) = (0u64, 0u64);
    for cp in 0u32..=0x10_FFFF {
        if (0xD800..=0xDFFF).contains(&cp) {
            continue;
        }
        let c = char::from_u32(cp).expect("non-surrogate is a scalar");
        let Some(want) = oracle.glyph_index(c).map(|g| g.0).filter(|&g| g != 0) else {
            continue;
        };
        covered += 1;
        let fd_gid = fd.lookup_glyph_index(c);
        if fd_gid != 0 && fd_gid != want {
            cmap_divergences += 1;
        }
        if procedural::covers(c) || aterm_grapheme::is_emoji_presentation(c) {
            continue;
        }
        if r.private_use_yields_to_embedded(r.primary_yields_private_use, c) {
            assert_eq!(
                r.glyph_key(c).source,
                FaceId::RuntimeFallback,
                "U+{cp:04X} ({c:?}) is yielded by {label} but not drawn by the runtime tier",
            );
            continue;
        }
        if (r.primary_yields_wide_characters && aterm_grapheme::char_width(c) == 2)
            || (r.primary_yields_small_dingbats && bundled::is_small_dingbat(c))
        {
            assert_ne!(
                r.glyph_key(c).source,
                FaceId::Primary,
                "U+{cp:04X} ({c:?}) is yielded by {label} but still drawn by it, or not at all",
            );
            continue;
        }
        let key = r.glyph_key(c);
        assert_eq!(
            key.source,
            FaceId::Primary,
            "U+{cp:04X} ({c:?}) is covered by {label} but did not resolve to the primary face",
        );
        assert_eq!(
            key.glyph_class,
            GlyphClass::MonoGid,
            "U+{cp:04X} ({c:?}) primary glyph must be Unicode-id-addressed ({label})",
        );
        assert_eq!(
            u16::try_from(key.ch_or_id).unwrap(),
            want,
            "UNFAITHFUL ({label}): U+{cp:04X} ({c:?}) rasterizes glyph id {} but the Unicode cmap says {want}",
            key.ch_or_id,
        );
    }
    assert!(
        covered > 100,
        "{label}: only {covered} covered scalars — proof would be vacuous",
    );
    eprintln!(
        "PROVEN ({label}): {covered} covered scalars ALL render their Unicode glyph \
         (the enumerated-cmap lookup would have diverged on {cmap_divergences})",
    );
    (covered, cmap_divergences)
}

#[test]
fn render_glyph_resolution_refines_unicode_cmap_exhaustive() {
    prove_faithful_over_all_scalars(&system_font_bytes(), "system default");
    #[cfg(feature = "embedded-font")]
    prove_faithful_over_all_scalars(embedded_font(), "embedded DejaVu Sans Mono");
}

#[cfg(target_os = "macos")]
#[test]
fn glyph_fidelity_proof_catches_mac_roman_substitution() {
    let candidates = [
        "/System/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/Monaco.ttf",
    ];
    let mut demonstrated = false;
    for path in candidates {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(oracle) = aterm_font::Face::parse(&bytes, 0) else {
            continue;
        };
        let fd =
            crate::font::Font::from_bytes(bytes.as_slice(), crate::font::FontSettings::default())
                .expect("the face parses");
        let mut diverged: Vec<(char, u16, u16)> = Vec::new();
        for cp in 0xA0u32..=0xFF {
            let c = char::from_u32(cp).unwrap();
            let uni = oracle.glyph_index(c).map(|g| g.0).unwrap_or(0);
            let fdg = fd.lookup_glyph_index(c);
            if uni != 0 && fdg != 0 && uni != fdg {
                diverged.push((c, uni, fdg));
            }
        }
        if diverged.is_empty() {
            continue;
        }
        demonstrated = true;
        let mut r = Renderer::from_bytes(&bytes, 16.0, Theme::default()).expect("renderer");
        for (c, uni, fdg) in &diverged {
            let key = r.glyph_key(*c);
            assert_eq!(key.source, FaceId::Primary, "U+{:04X}", *c as u32);
            assert_eq!(
                key.glyph_class,
                GlyphClass::MonoGid,
                "U+{:04X} must be Unicode-id-addressed",
                *c as u32,
            );
            assert_eq!(
                u16::try_from(key.ch_or_id).unwrap(),
                *uni,
                "U+{:04X}: aterm must use the Unicode glyph {uni}, not fontdue's Mac-Roman {fdg}",
                *c as u32,
            );
            assert_ne!(
                u16::try_from(key.ch_or_id).unwrap(),
                *fdg,
                "U+{:04X}: aterm must NOT reproduce fontdue's Mac-Roman substitution",
                *c as u32,
            );
        }
        let dot = diverged.iter().find(|(c, _, _)| *c == '\u{B7}');
        eprintln!(
            "CAUGHT ({path}): {} Latin-1 codepoints where fontdue diverges from Unicode; \
             aterm renders every one via the Unicode cmap{}",
            diverged.len(),
            dot.map(|(_, u, f)| format!(" — e.g. U+00B7 ‘·’: fontdue→gid{f} (∑), unicode→gid{u}"))
                .unwrap_or_default(),
        );
    }
    assert!(
        demonstrated,
        "Menlo/Monaco carry a Mac Roman subtable fontdue diverges on: the proof is vacuous"
    );
}

#[test]
fn glyph_key_is_cached_and_primary_chars_skip_fallback() {
    let mut r = renderer();
    let k1 = r.glyph_key('M');
    let k2 = r.glyph_key('M');
    assert_eq!(k1, k2);
    assert_eq!(k1.source, FaceId::Primary);
    assert!(
        r.fallback_chain.is_empty(),
        "ASCII lookup must not load the fallback face"
    );
    assert!(
        r.runtime_fallback.decisions.is_empty() && r.runtime_fallback.faces.is_empty(),
        "a primary-covered char must not touch the runtime resolver"
    );
    assert_eq!(r.keys.len(), 1);
}

#[cfg(all(feature = "embedded-font", not(target_arch = "wasm32")))]
#[test]
fn lazy_fallback_first_miss_is_async_and_converges() {
    const UNCOVERED: char = '\u{FFFF}';
    let mut r = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("embedded font builds a renderer");
    assert_eq!(
        r.primary_unicode_gid(UNCOVERED),
        None,
        "precondition: the primary face must not cover the noncharacter"
    );
    let scratch = crate::test_scratch_dir("lazy-fallback");
    let path = scratch.path().join("lazy-fallback.ttf");
    std::fs::write(&path, embedded_font()).expect("temp font written");
    r.fallback_paths = vec![path.to_string_lossy().into_owned()];
    let epoch0 = r.font_epoch;

    let key = r.glyph_key(UNCOVERED);
    assert_eq!(
        key.source,
        FaceId::Primary,
        "while the parse is in flight the char renders provisional .notdef"
    );
    assert!(
        !r.keys
            .contains_key(&(UNCOVERED, ScalarPresentation::Default)),
        "a provisional resolution must NOT be memoized"
    );
    assert!(
        r.fallback_paths.is_empty(),
        "the candidate list is consumed by the kicked parse"
    );

    r.debug_block_on_lazy_fallbacks();
    assert_eq!(
        r.fallback_chain.len(),
        1,
        "the background-parsed face must land in the chain"
    );
    assert!(!r.fallback_parse_pending(), "nothing left in flight");
    assert!(
        r.font_epoch > epoch0,
        "a landed parse must advance the routing epoch"
    );
    let key2 = r.glyph_key(UNCOVERED);
    assert_eq!(key2.source, FaceId::Primary);
    assert!(
        r.keys
            .contains_key(&(UNCOVERED, ScalarPresentation::Default)),
        "the post-landing resolution is final and memoized"
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_fresh_renderers_first_snapshot_paints_every_fallback_glyph() {
    let theme = Theme::default();
    let fresh = || {
        let primary = include_bytes!("../assets/bundled/CascadiaMonoPL-Regular.ttf");
        let mut r = Renderer::from_bytes(primary, 20.0, theme).expect("Cascadia Mono builds");
        r.fallback_paths = vec![asset_path("bundled/NotoSansMath-Regular.ttf")];
        r.fallback_user_prefix = 0;
        r.symbol_fallback_paths = vec![asset_path("bundled/NotoSansSymbols2-Regular.ttf")];
        r.symbol_user_prefix = 0;
        r
    };
    let mut oracle = fresh();
    oracle.debug_block_on_lazy_fallbacks();
    let mut routed = |candidates: &[char], tier: FaceId| {
        candidates
            .iter()
            .copied()
            .find(|&ch| oracle.glyph_key_text(ch).source == tier)
    };
    let broad = routed(&['\u{21BB}', '\u{21E3}', '\u{2715}'], FaceId::Fallback);
    let symbol = routed(&['\u{26A0}', '\u{2139}'], FaceId::SymbolFallback);
    let broad = broad.expect("a band glyph rides the broad chain");
    let symbol = symbol.expect("a band glyph rides the symbol chain");
    let (rows, cols) = (21usize, 60usize);
    let band = [broad, broad, symbol, symbol];
    let mut term = Terminal::new(rows as u16, cols as u16);
    let mut bytes = String::from("\x1b[?25l");
    for (row, ch) in band.iter().enumerate() {
        let seam = row + 1 == band.len();
        bytes.push_str(&format!("\x1b[{};1H", row + 1));
        if seam {
            bytes.push_str("\x1b[4m");
        }
        bytes.push_str(&format!(" \x1b[1m{ch}\x1b[22m Universal Control disabled"));
        if seam {
            bytes.push_str("\x1b[24m");
        }
    }
    for row in band.len()..rows {
        bytes.push_str(&format!("\x1b[{};1H$ claude --version", row + 1));
    }
    term.process(bytes.as_bytes());
    let mut input = term.cell_frame(rows, cols);
    for (row, &ch) in band.iter().enumerate() {
        let cell = &mut input.cells[row][1];
        assert_eq!(cell.ch, ch, "row {row}: the glyph sits at its column");
        cell.text_presentation = true;
    }
    assert!(
        input.cells[band.len() - 1][1].underline != input.cells[0][1].underline
            && input.cells[0][1].underline == Default::default(),
        "the seam is on the last band row only"
    );
    for forced in [true, false] {
        let mut r = fresh();
        r.debug_land_lazy_parses_on_poll(forced);
        assert!(
            r.fallback_chain.is_empty() && !r.fallback_parse_pending(),
            "precondition: a fresh renderer has probed nothing — the chain is lazy"
        );
        let frame = r.render_input(&input);
        let (cw, ch_px) = r.cell_size();
        let (x_pad, y_top) = (r.pad(), r.grid_top());
        assert!(
            frame.width >= x_pad + cols * cw && frame.height >= y_top + rows * ch_px,
            "the frame holds the grid"
        );
        let ink = |col: usize, row: usize| -> usize {
            let c = &input.cells[row][col];
            let ground =
                (u32::from(c.bg[0]) << 16) | (u32::from(c.bg[1]) << 8) | u32::from(c.bg[2]);
            let (x0, y0) = (x_pad + col * cw, y_top + row * ch_px);
            (y0..y0 + ch_px)
                .map(|y| {
                    let line: Vec<u32> = (x0..x0 + cw)
                        .map(|x| frame.pixels[y * frame.width + x])
                        .collect();
                    let non = line.iter().filter(|&&p| p != ground).count();
                    let seam = non == cw && line.iter().all(|&p| p == line[0]);
                    if seam { 0 } else { non }
                })
                .sum()
        };
        assert_eq!(
            ink(0, 0),
            0,
            "forced={forced}: control — the blank band cell beside the glyph has no ink"
        );
        for (row, &ch) in band.iter().enumerate() {
            assert!(
                ink(1, row) > 0,
                "forced={forced}: {ch:?} painted a blank cell on row {row} of a fresh \
                 renderer's first snapshot"
            );
        }
        assert!(
            !r.fallback_parse_pending(),
            "forced={forced}: the snapshot settled the parses it started"
        );
    }
}

#[cfg(feature = "embedded-font")]
#[test]
fn font_epoch_bump_forces_full_repaint_of_cached_frame() {
    let mut r = Renderer::from_bytes(embedded_font(), 16.0, Theme::default())
        .expect("embedded font builds a renderer");
    let mut term = Terminal::new(2, 8);
    term.process(b"hi");
    let input = term.cell_frame(2, 8);
    let mut wc = WindowCpu::new();
    let first: Vec<u32> = r.render_input_cached(&mut wc, &input).pixels().to_vec();
    let cached_epoch = wc.cache.as_ref().expect("cache populated").font_epoch;
    assert_eq!(cached_epoch, r.font_epoch, "cache stamps the render epoch");

    let _ = r.render_input_cached(&mut wc, &input);
    assert_eq!(
        wc.cache.as_ref().expect("cache").font_epoch,
        cached_epoch,
        "an unchanged frame keeps the cached epoch"
    );

    r.font_epoch += 1;
    let repainted: Vec<u32> = r.render_input_cached(&mut wc, &input).pixels().to_vec();
    assert_eq!(
        wc.cache.as_ref().expect("cache").font_epoch,
        r.font_epoch,
        "an epoch mismatch must force the full-repaint path (which re-stamps)"
    );
    assert_eq!(
        first, repainted,
        "the forced repaint is byte-identical when nothing else changed"
    );
}

#[test]
fn record_symbol_is_never_color_emoji() {
    let mut r = renderer();
    r.debug_block_on_lazy_fallbacks();
    let key = r.glyph_key('\u{23FA}');
    eprintln!(
        "⏺ U+23FA resolved to {:?} / {:?}",
        key.source, key.glyph_class
    );
    assert_ne!(
        key.source,
        FaceId::ColorEmoji,
        "⏺ (U+23FA, Emoji_Presentation=No) must not use the colour-emoji face"
    );
    assert_ne!(
        key.glyph_class,
        GlyphClass::Rgba,
        "⏺ must rasterize as a foreground-tinted mono glyph, not a colour bitmap"
    );
    if r.primary_unicode_gid('\u{23FA}').is_some() {
        assert_eq!(
            (key.source, key.glyph_class),
            (FaceId::Primary, GlyphClass::MonoGid),
            "⏺ is the primary's own glyph"
        );
        assert!(
            r.glyph_image(key).bytes().iter().any(|&b| b > 0),
            "⏺ primary glyph must carry real coverage, not be blank"
        );
    } else if r.symbol_fallback_has('\u{23FA}', false) {
        assert_eq!(
            key.source,
            FaceId::SymbolFallback,
            "⏺ should render via the mono symbol fallback when one covers it"
        );
        let img = r.glyph_image(key).clone();
        assert!(
            img.bytes().iter().any(|&b| b > 0),
            "⏺ symbol-fallback glyph must carry real coverage, not be blank"
        );
    }
    for c in ['\u{23F8}', '\u{23F9}'] {
        assert_ne!(
            r.glyph_key(c).source,
            FaceId::ColorEmoji,
            "{c:?} (Emoji_Presentation=No) must not use the colour face"
        );
    }
}

#[test]
fn a_text_point_only_the_colour_face_covers_monochromatizes() {
    require_host_colour_emoji();
    let mut r = Renderer::from_bytes(BUNDLED_MONO, 16.0, Theme::default())
        .expect("the bundled face builds a renderer");
    r.color_font_paths = color_emoji_candidate_paths(&[]);
    assert!(
        r.color_font_has('\u{23FA}'),
        "the host's colour face maps ⏺"
    );
    r.fallback_paths.clear();
    r.symbol_chain.clear();
    r.symbol_fallback_paths = vec!["/nonexistent/no-symbol-font.ttf".to_string()];
    r.symbol_user_prefix = 0;
    r.debug_block_on_lazy_fallbacks();

    let c = [
        '\u{23F2}',
        '\u{23F1}',
        '\u{1F56F}',
        '\u{1F5E8}',
        '\u{1F6E1}',
    ]
    .into_iter()
    .find(|&c| !aterm_grapheme::is_emoji_presentation(c) && !procedural::covers_symbol(c))
    .expect("a default-text point the synthesis does not draw");
    assert!(r.color_font_has(c), "the host's colour face maps {c:?}");
    let key = r.glyph_key(c);
    assert_eq!(
        key.source,
        FaceId::ColorEmojiMono,
        "{c:?} with no mono glyph anywhere must monochromatize the colour glyph"
    );
    assert_eq!(
        key.glyph_class,
        GlyphClass::Mono,
        "the monochromatized {c:?} must be a Mono (foreground-tinted) glyph, not Rgba"
    );
    let img = r.glyph_image(key).clone();
    assert!(
        matches!(img, GlyphImage::Mono { .. }),
        "must be a Mono image"
    );
    assert!(
        img.bytes().iter().any(|&b| b > 0),
        "monochromatized {c:?} must carry real coverage, not be blank tofu"
    );
}

#[cfg(not(windows))]
#[test]
fn default_emoji_still_uses_color_face() {
    require_host_colour_emoji();
    let mut r = renderer();
    r.debug_block_on_lazy_fallbacks();
    let key = r.glyph_key('\u{1F680}');
    assert!(
        r.color_font_has('\u{1F680}'),
        "the host's colour face maps 🚀"
    );
    assert_eq!(
        key.source,
        FaceId::ColorEmoji,
        "🚀 (Emoji_Presentation=Yes) must still render in colour"
    );
    assert_eq!(key.glyph_class, GlyphClass::Rgba);
}

#[test]
fn procedural_chars_dispatch_before_font_lookup() {
    let mut r = renderer();
    let (cw, ch) = r.cell_size();
    let baseline = r.baseline();
    for c in ['─', '│', '┼', '╋', '╬', '╭', '╳', '█', '▚', '░', '\u{28FF}'] {
        let key = r.glyph_key(c);
        assert_eq!(key.source, FaceId::Procedural, "{c:?} must be procedural");
        let img = r.glyph_image(key).clone();
        assert_eq!(
            (img.width(), img.height()),
            (cw, ch),
            "{c:?} must fill the cell"
        );
        assert_eq!(img.xmin(), 0, "{c:?} anchors at the cell's left edge");
        assert_eq!(
            baseline - img.height() as i32 - img.ymin(),
            0,
            "{c:?} anchors at the cell top"
        );
        if !procedural::antialiased(c) {
            assert!(
                img.bytes().iter().all(|&b| b == 0 || b == 255),
                "{c:?} must be hard 0/255 coverage"
            );
        }
        assert!(img.bytes().contains(&255), "{c:?} must draw something");
    }
    assert!(
        r.fallback_chain.is_empty(),
        "procedural dispatch must not load the fallback face"
    );
}

#[test]
fn emoji_clusters_shape_to_colour_glyphs() {
    require_host_colour_emoji();
    let mut r = renderer();
    assert!(
        r.color_font_has('\u{1F44D}'),
        "the host's colour face maps 👍"
    );
    #[cfg(not(windows))]
    let corpus = [
        ("family", "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"),
        ("keycap-1", "\u{31}\u{FE0F}\u{20E3}"),
        ("thumb-skin", "\u{1F44D}\u{1F3FD}"),
        ("flag-US", "\u{1F1FA}\u{1F1F8}"),
    ];
    #[cfg(windows)]
    let corpus = [
        ("keycap-1", "\u{31}\u{FE0F}\u{20E3}"),
        ("thumb-skin", "\u{1F44D}\u{1F3FD}"),
    ];
    for (name, s) in corpus {
        let key = r
            .glyph_key_cluster(s)
            .unwrap_or_else(|| panic!("{name} should shape to a glyph"));
        assert_eq!(
            key.glyph_class,
            GlyphClass::RgbaGid,
            "{name} key is glyph-id-addressed"
        );
        assert_eq!(
            key.source,
            FaceId::ColorEmoji,
            "{name} uses the colour face"
        );
        let img = r.glyph_image(key).clone();
        assert!(
            matches!(img, GlyphImage::Rgba { .. }) && img.width() > 0 && img.height() > 0,
            "{name} colour glyph is empty"
        );
        assert!(
            img.bytes().as_chunks::<4>().0.iter().any(|p| p[3] > 0),
            "{name} colour glyph is fully transparent"
        );
    }
    assert!(
        r.glyph_key_cluster("e\u{0301}").is_none(),
        "Latin diacritic must not shape to colour"
    );
}

/// A SUB-SCRIPTED EMOJI IS DRAWN SMALL ON THE CELL'S MIDDLE: an emoji-presented `⬆` cell in
/// [`Script::Sub`](aterm_core::terminal::Script::Sub) resolves to its colour key with the
/// script bit, and its raster is seven tenths of the full one, every inked row inside
/// [`Script::small_emoji_rows`](aterm_core::terminal::Script::small_emoji_rows), centred in
/// the two-cell box, its colours the full raster's. CONTROL: the normal cell's emoji fills
/// rows outside them.
#[test]
fn a_sub_scripted_emoji_is_drawn_small_on_the_cells_middle() {
    use aterm_core::terminal::Script;
    require_host_colour_emoji();
    let mut r = renderer();
    r.debug_block_on_lazy_fallbacks();
    let arrow = '\u{2B06}';
    assert!(r.color_font_has(arrow), "the host's colour face maps ⬆️");
    let cell = |script| RenderCell {
        ch: arrow,
        emoji_presentation: true,
        script,
        ..RenderCell::default()
    };
    let full_key = r.resolve_cell_key(None, &cell(Script::Normal));
    let small_key = r.resolve_cell_key(None, &cell(Script::Sub { nudge: 3 }));
    assert!(matches!(full_key.glyph_class, GlyphClass::Rgba));
    assert_eq!(
        small_key,
        GlyphKey {
            style: StyleBits(full_key.style.0 | StyleBits::SUB.0),
            ..full_key
        },
        "the colour key with the script bit, its nudge ignored"
    );
    let (baseline, cell_h, box_w) = (r.baseline, r.cell_h, 2 * r.cell_w);
    let rows_of = |img: &GlyphImage| {
        let top = baseline - img.ymin() - img.height() as i32;
        let inked: Vec<i32> = (0..img.height())
            .filter(|&y| (0..img.width()).any(|x| img.bytes()[(y * img.width() + x) * 4 + 3] > 0))
            .map(|y| top + y as i32)
            .collect();
        (inked.first().copied(), inked.last().copied())
    };
    let full = r.glyph_image(full_key).clone();
    let small = r.glyph_image(small_key).clone();
    let (top, rows) = Script::small_emoji_rows(cell_h);
    let (lo, hi) = rows_of(&small);
    let (lo, hi) = (lo.expect("inked"), hi.expect("inked"));
    assert!(
        lo >= top as i32 && hi < (top + rows) as i32,
        "inked rows {lo}..={hi} inside {top}..{}",
        top + rows
    );
    let k = rows as f32 / cell_h as f32;
    assert_eq!(small.height(), (full.height() as f32 * k).round() as usize);
    assert_eq!(small.width(), (full.width() as f32 * k).round() as usize);
    assert_eq!(
        small.xmin(),
        ((box_w - small.width()) / 2) as i32,
        "centred"
    );
    assert!(
        matches!(small, GlyphImage::Rgba { .. }),
        "still drawn in colour"
    );
    let (flo, fhi) = rows_of(&full);
    let (flo, fhi) = (flo.expect("full inked"), fhi.expect("full inked"));
    assert!(
        small.height() < full.height() && (hi - lo) < (fhi - flo),
        "CONTROL: the full emoji ({flo}..={fhi}, h={}) is taller than the small ({lo}..={hi}, h={})",
        full.height(),
        small.height()
    );
}

#[test]
fn subscript_scales_both_ligated_and_composite_colour_clusters() {
    use aterm_core::terminal::Script;
    require_host_colour_emoji();
    let mut r = renderer();
    r.debug_block_on_lazy_fallbacks();
    for (cluster, class) in [
        ("1\u{FE0F}\u{20E3}", GlyphClass::RgbaGid),
        ("\u{1F680}\u{200D}\u{1F355}", GlyphClass::RgbaCluster),
    ] {
        let cell = |script| RenderCell {
            ch: cluster.chars().next().unwrap(),
            emoji_presentation: true,
            script,
            ..RenderCell::default()
        };
        let full_key = r.resolve_cell_key(Some(cluster), &cell(Script::Normal));
        let small_key = r.resolve_cell_key(Some(cluster), &cell(Script::Sub { nudge: -5 }));
        assert_eq!(full_key.glyph_class, class);
        assert_eq!(
            small_key,
            small_emoji_keyed(full_key, Script::Sub { nudge: 0 })
        );
        assert_eq!(small_key.style.script(), StyleBits::SUB);
        let full = r.glyph_image(full_key).clone();
        let small = r.glyph_image(small_key).clone();
        let (top, rows) = Script::small_emoji_rows(r.cell_h);
        let k = rows as f32 / r.cell_h as f32;
        assert_eq!(small.height(), (full.height() as f32 * k).round() as usize);
        assert_eq!(small.width(), (full.width() as f32 * k).round() as usize);
        assert!(
            small.height() < full.height(),
            "CONTROL: normal cluster is full size"
        );
        assert_eq!(small.xmin(), ((2 * r.cell_w - small.width()) / 2) as i32);
        let y0 = r.baseline - small.ymin() - small.height() as i32;
        let inked: Vec<_> = (0..small.height())
            .filter(|&y| {
                (0..small.width()).any(|x| small.bytes()[(y * small.width() + x) * 4 + 3] > 0)
            })
            .map(|y| y0 + y as i32)
            .collect();
        assert!(!inked.is_empty(), "{cluster}: colour raster has ink");
        assert!(
            inked
                .iter()
                .all(|&y| y >= top as i32 && y < (top + rows) as i32)
        );
    }
    let fallback = regional_pair_key("\u{1F1FA}\u{1F1F8}", StyleBits::REGULAR, r.px_q).unwrap();
    assert_eq!(
        small_emoji_keyed(fallback, Script::Sub { nudge: 3 }),
        fallback,
        "a procedural monochrome flag keeps its ordinary key"
    );
}

#[test]
fn an_unligated_emoji_cluster_draws_every_member_in_its_two_cells() {
    const ROCKET_PIZZA: &str = "\u{1F680}\u{200D}\u{1F355}";
    require_host_colour_emoji();
    let mut r = renderer();
    r.debug_block_on_lazy_fallbacks();
    assert!(
        r.color_font_has('\u{1F680}') && r.color_font_has('\u{1F355}'),
        "the host's colour face maps 🚀 and 🍕"
    );
    let key = r
        .glyph_key_cluster(ROCKET_PIZZA)
        .expect("an unligated emoji sequence resolves to a colour key");
    assert_eq!(key.glyph_class, GlyphClass::RgbaCluster);
    assert_eq!(key.source, FaceId::ColorEmoji);
    let img = r.glyph_image(key).clone();
    let GlyphImage::Rgba {
        width,
        height,
        xmin,
        advance,
        ref bytes,
        ..
    } = img
    else {
        panic!("a composite is a colour image");
    };
    let (box_w, box_h) = r.color_emoji_box();
    assert!(width <= box_w && height <= box_h, "fits the two cells");
    assert_eq!(advance, box_w as f32);
    assert_eq!(xmin as usize, (box_w - width) / 2, "centred");

    let face_index = r
        .cluster_shapes
        .run(key.ch_or_id)
        .expect("the resolved composite retains its run")
        .face;
    let bytes_font = r
        .color_face_bytes(face_index)
        .expect("the resolved face stays resident");
    let face = aterm_font::Face::parse(bytes_font, 0).unwrap();
    let visible = |px: &[u8; 4]| if px[3] == 0 { [0; 4] } else { *px };
    let slot = width / 2;
    let colr = ['\u{1F680}', '\u{1F355}']
        .into_iter()
        .all(|member| colr_only(&face, face.glyph_index(member).unwrap()));
    let inked = |x0: usize, x1: usize| {
        (0..height).any(|y| (x0..x1).any(|x| bytes[(y * width + x) * 4 + 3] >= 128))
    };
    assert!(
        !colr || (inked(0, slot) && inked(slot, width)),
        "a COLR run draws a member in each half"
    );
    let slotted: &[(char, usize)] = if colr {
        &[]
    } else {
        &[('\u{1F680}', 0), ('\u{1F355}', slot)]
    };
    for &(member, x_slot) in slotted {
        let gid = face.glyph_index(member).unwrap();
        let (tile, tw, th) = fit_color_glyph(&face, gid, slot, height).unwrap();
        let (x0, y0) = (x_slot + (slot - tw) / 2, (height - th) / 2);
        for y in 0..th {
            let drawn = bytes[((y0 + y) * width + x0) * 4..][..tw * 4]
                .as_chunks::<4>()
                .0;
            let alone = tile[y * tw * 4..][..tw * 4].as_chunks::<4>().0;
            assert!(
                drawn.iter().map(visible).eq(alone.iter().map(visible)),
                "{member} row {y}: {drawn:?} != {alone:?}"
            );
        }
    }

    let frame = |text: &str, r: &mut Renderer| {
        let mut term = Terminal::new(1, 4);
        term.process(format!("\x1b[?25l{text}").as_bytes());
        let input = term.cell_frame(1, 4);
        (input.clone(), r.render_input(&input))
    };
    let (input, composite) = frame(ROCKET_PIZZA, &mut r);
    assert_eq!(input.cluster_at(0, 0), Some(ROCKET_PIZZA));
    assert_eq!(materialized_cell_span(&input.cells[0], 0), 2);
    assert_eq!(
        r.resolve_cell_key(input.cluster_at(0, 0), &input.cells[0][0]),
        key
    );
    let (_, rocket) = frame("\u{1F680}", &mut r);
    assert_ne!(composite.pixels, rocket.pixels, "more than the rocket drew");
}

#[test]
fn a_pair_that_is_no_country_draws_the_faces_glyph_or_both_letters() {
    const NO_FLAG: &str = "\u{1F1E6}\u{1F1E6}";
    require_host_colour_emoji();
    let mut r = renderer();
    r.debug_block_on_lazy_fallbacks();
    assert!(
        r.color_font_has('\u{1F44D}'),
        "the host's colour face maps 👍"
    );
    let mut term = Terminal::new(1, 4);
    term.process(NO_FLAG.as_bytes());
    let input = term.cell_frame(1, 4);
    assert_eq!(input.cluster_at(0, 0), Some(NO_FLAG));
    let cell = &input.cells[0][0];
    let key = r.resolve_cell_key(Some(NO_FLAG), cell);
    let face_index = match key.glyph_class {
        GlyphClass::Rgba | GlyphClass::RgbaGid => key.colour_face(),
        GlyphClass::RgbaCluster => {
            r.cluster_shapes
                .run(key.ch_or_id)
                .expect("the resolved composite retains its run")
                .face
        }
        _ => 0,
    };
    let bytes = r
        .color_face_bytes(face_index)
        .expect("the resolved face stays resident")
        .clone();
    let shaped = shape_colour_cluster(&bytes, NO_FLAG).expect("the colour face shapes");
    let face = aterm_font::Face::parse(&bytes, 0).unwrap();
    let letters: Vec<bool> = shaped
        .iter()
        .map(|&(gid, ..)| colour_drawable(&face, aterm_font::GlyphId(gid as u16)))
        .collect();
    if let [(gid, ..)] = shaped[..] {
        assert!(letters[0], "the face's glyph for the pair is colour");
        assert_eq!(
            key,
            GlyphKey::rgba_gid_in(face_index, gid as u16, r.px_q),
            "the face's glyph for the pair draws"
        );
        return;
    }
    assert_eq!(letters.len(), 2, "the face leaves both letters");
    if letters == [true, true] {
        assert_eq!(
            key.glyph_class,
            GlyphClass::RgbaCluster,
            "both letters draw"
        );
    } else {
        assert_eq!(r.glyph_key_cluster(NO_FLAG), None);
        assert_eq!(
            Some(key),
            regional_pair_key(NO_FLAG, StyleBits::REGULAR, r.px_q),
            "the two letters draw"
        );
        assert_both_cells_inked(&mut r, key);
    }
}

fn assert_both_cells_inked(r: &mut Renderer, key: GlyphKey) {
    let cell_w = r.cell_w;
    let GlyphImage::Mono {
        width,
        height,
        xmin,
        ref bytes,
        ..
    } = *r.glyph_image(key)
    else {
        panic!("{key:?} is not a coverage mask");
    };
    assert_eq!((width, xmin), (2 * cell_w, 0), "two cells from the lead");
    let inked =
        |x0: usize, x1: usize| (0..height).any(|y| (x0..x1).any(|x| bytes[y * width + x] > 128));
    assert!(
        inked(0, cell_w) && inked(cell_w, width),
        "a letter in each cell"
    );
}

#[cfg(windows)]
fn segoe_emoji_fixture(mut r: Renderer) -> Renderer {
    r.ensure_color_chain();
    let Some(face) = r
        .color_chain
        .iter()
        .find(|face| face_family_name(&face.bytes, 0).as_deref() == Some("Segoe UI Emoji"))
    else {
        aterm_spec::host::could_not_run("no Segoe UI Emoji for its Windows rendering contracts");
    };
    let bytes = face.bytes.clone();
    r.color_chain.clear();
    r.color_font_paths.clear();
    r.install_color_font(bytes);
    r.set_runtime_font_discovery(false);
    r
}

#[test]
#[cfg(windows)]
fn a_windows_flag_draws_as_its_two_letters_in_the_primary_face() {
    const JP: &str = "\u{1F1EF}\u{1F1F5}";
    let mut r = segoe_emoji_fixture(renderer());
    r.debug_block_on_lazy_fallbacks();
    assert_eq!(r.glyph_key_cluster(JP), None, "Segoe UI Emoji has no 🇯🇵");
    let mut term = Terminal::new(1, 4);
    term.process(JP.as_bytes());
    let input = term.cell_frame(1, 4);
    assert_eq!(input.cluster_at(0, 0), Some(JP));
    let key = r.resolve_cell_key(Some(JP), &input.cells[0][0]);
    assert_eq!(
        key,
        regional_pair_key(JP, StyleBits::REGULAR, r.px_q).unwrap()
    );
    assert_both_cells_inked(&mut r, key);
    let mut typed = Terminal::new(1, 4);
    typed.process(b"\x1b[?25lJP");
    let mut flag = Terminal::new(1, 4);
    flag.process(format!("\x1b[?25l{JP}").as_bytes());
    let (a, b) = (
        r.render_input(&typed.cell_frame(1, 4)),
        r.render_input(&flag.cell_frame(1, 4)),
    );
    assert!(a.pixels == b.pixels, "🇯🇵 draws exactly as `JP` typed");
}

#[test]
#[cfg(windows)]
fn windows_emoji_draw_in_colour_inside_their_two_cells() {
    let mut r = segoe_emoji_fixture(Renderer::from_system(17.0, Theme::default()).expect(NO_FONT));
    r.set_line_height(1.1);
    r.seal_admitted_font_sources();
    r.debug_block_on_lazy_fallbacks();
    let (cw, ch) = r.cell_size();
    let (slot_w, ch) = (i32::try_from(2 * cw).unwrap(), i32::try_from(ch).unwrap());
    let baseline = r.baseline();
    for (c, vs16) in [('😀', false), ('✅', false), ('⚠', true), ('❤', true)] {
        let key = if vs16 {
            r.glyph_key_emoji(c)
        } else {
            r.glyph_key(c)
        };
        assert_eq!(
            (key.source, key.glyph_class),
            (FaceId::ColorEmoji, GlyphClass::Rgba),
            "{c} (VS16 {vs16}) is not drawn from the colour face"
        );
        let GlyphImage::Rgba {
            width,
            height,
            xmin,
            ymin,
            ref bytes,
            ..
        } = *r.glyph_image(key)
        else {
            panic!("{c}: a colour key draws a colour image");
        };
        let top = baseline - i32::try_from(height).unwrap() - ymin;
        let inked: Vec<(i32, i32)> = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .filter(|&(x, y)| bytes[(y * width + x) * 4 + 3] >= 32)
            .map(|(x, y)| (xmin + x as i32, top + y as i32))
            .collect();
        let (x_lo, x_hi) = (
            inked.iter().map(|p| p.0).min().unwrap(),
            inked.iter().map(|p| p.0).max().unwrap(),
        );
        let (y_lo, y_hi) = (
            inked.iter().map(|p| p.1).min().unwrap(),
            inked.iter().map(|p| p.1).max().unwrap(),
        );
        assert!(
            x_lo >= 1 && x_hi <= slot_w - 2 && y_lo >= 1 && y_hi <= ch - 2,
            "{c}: ink {x_lo}..={x_hi} x {y_lo}..={y_hi} leaves no clear texel \
             inside the {slot_w}x{ch} slot"
        );
        assert!(
            (x_lo - (slot_w - 1 - x_hi)).abs() <= 1,
            "{c}: ink {x_lo}..={x_hi} is not centred in the {slot_w}-px slot"
        );
        let least = if vs16 { 13 } else { 15 };
        assert!(
            x_hi - x_lo + 1 >= least,
            "{c}: drawn {} px wide in an {slot_w}-px slot",
            x_hi - x_lo + 1
        );
    }
    let key = r.glyph_key('😀');
    let img = r.glyph_image(key);
    let yellow = img
        .bytes()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] >= 200 && p[0] >= 200 && p[1] >= 140 && p[2] <= 110)
        .count();
    assert!(
        yellow >= 60,
        "😀 has {yellow} yellow texels: it is not drawn in colour"
    );
}

#[test]
#[cfg(windows)]
fn a_windows_zwj_family_draws_all_its_members() {
    const FAMILY: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let mut r = segoe_emoji_fixture(renderer());
    r.debug_block_on_lazy_fallbacks();
    let key = r
        .glyph_key_cluster(FAMILY)
        .expect("the family is a colour cluster");
    assert_eq!(key.glyph_class, GlyphClass::RgbaCluster);
    let (slot, _) = r.color_emoji_box();
    let GlyphImage::Rgba {
        width,
        height,
        xmin,
        ref bytes,
        ..
    } = *r.glyph_image(key)
    else {
        panic!("a colour cluster draws a colour image");
    };
    let thirds: Vec<bool> = (0..3)
        .map(|third| {
            (0..height).any(|y| {
                (0..width).any(|x| {
                    let col = usize::try_from(xmin).unwrap() + x;
                    col * 3 / slot == third && bytes[(y * width + x) * 4 + 3] >= 128
                })
            })
        })
        .collect();
    assert_eq!(thirds, [true; 3], "a member in each third of the slot");
}

#[test]
#[cfg(windows)]
fn a_built_in_chain_face_comes_in_the_weight_nearest_the_primarys() {
    const YAHEI: &str = "C:\\Windows\\Fonts\\msyh.ttc";
    const YAHEI_BOLD: &str = "C:\\Windows\\Fonts\\msyhbd.ttc";
    const YAHEI_LIGHT: &str = "C:\\Windows\\Fonts\\msyhl.ttc";
    const MALGUN: &str = "C:\\Windows\\Fonts\\malgun.ttf";
    const MALGUN_BOLD: &str = "C:\\Windows\\Fonts\\malgunbd.ttf";
    const ARIAL: &str = "C:\\Windows\\Fonts\\arial.ttf";
    let paths = || -> Vec<String> { [YAHEI, YAHEI, MALGUN, ARIAL].map(String::from).to_vec() };
    let every = |_: &str| true;
    let at = |weight: u16, exists: &dyn Fn(&str) -> bool| {
        weight_matched_fallback_paths(paths(), 1, weight, exists)
    };
    assert_eq!(at(600, &every), [YAHEI, YAHEI_BOLD, MALGUN_BOLD, ARIAL]);
    assert_eq!(at(800, &every), [YAHEI, YAHEI_BOLD, MALGUN_BOLD, ARIAL]);
    assert_eq!(at(400, &every), [YAHEI, YAHEI, MALGUN, ARIAL]);
    assert_eq!(at(500, &every), [YAHEI, YAHEI, MALGUN, ARIAL]);
    assert_eq!(
        at(300, &every),
        [
            YAHEI,
            YAHEI_LIGHT,
            "C:\\Windows\\Fonts\\malgunsl.ttf",
            ARIAL
        ]
    );
    let no_bold = |p: &str| !p.contains("bd.");
    assert_eq!(at(600, &no_bold), [YAHEI, YAHEI, MALGUN, ARIAL]);
}

#[cfg(feature = "embedded-font")]
#[test]
fn injected_bold_face_routes_bold_cells_to_a_real_weight() {
    let bold_bytes =
        std::fs::read(aterm_tempfile::manifest_dir().join("tests/fixtures/jetbrains-mono.ttf"))
            .expect("committed JetBrains Mono fixture");

    let mut r = Renderer::from_bytes(embedded_font(), 16.0, Theme::default()).unwrap();
    let synth = r.glyph_key_styled('M', StyleBits::BOLD);
    assert_eq!(
        synth.source,
        FaceId::Primary,
        "no injected bold face yet → primary"
    );
    assert!(
        synth.style.contains(StyleBits::BOLD),
        "the existing path carries the BOLD bit for sibling/synthetic weight"
    );
    let synth_img = r.glyph_image(synth).clone();

    r.set_bold_font(&bold_bytes).expect("bold face parses");
    let real = r.glyph_key_styled('M', StyleBits::BOLD);
    assert_eq!(
        real.source,
        FaceId::BoldPrimary,
        "bold cell routes to the injected bold face"
    );
    assert_eq!(
        real.glyph_class,
        GlyphClass::MonoGid,
        "bold glyph is id-addressed"
    );
    assert!(
        !real.style.contains(StyleBits::BOLD),
        "BOLD bit dropped — the face is already bold, do not also embolden"
    );

    let real_img = r.glyph_image(real).clone();
    assert_ne!(
        synth_img.bytes(),
        real_img.bytes(),
        "real bold glyph must come from the injected face, not the synthetic primary"
    );

    let bold_ital = r.glyph_key_styled('M', StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0));
    assert_eq!(bold_ital.source, FaceId::BoldPrimary);
    assert!(bold_ital.style.contains(StyleBits::ITALIC));
    assert!(!bold_ital.style.contains(StyleBits::BOLD));

    r.set_bold_font(&bold_bytes).expect("re-inject ok");
    let again = r.glyph_key_styled('M', StyleBits::BOLD);
    assert_eq!(again.source, FaceId::BoldPrimary);
}

#[cfg(feature = "embedded-font")]
#[test]
fn specimen_cell_size_matches_candidate_activation_without_mutating_source() {
    let mut source = Renderer::from_bytes(embedded_font(), 14.0, Theme::default()).unwrap();
    source.set_runtime_font_discovery(false);
    source.set_line_height(1.7);
    let original = source.cell_size();
    for requested_px in [6.0, 12.0, 21.5, 24.0, 36.0, 72.0] {
        for dpi in [1.0, 1.25, 2.0, 3.0] {
            for line_height in [0.8, 1.0, 1.3, 2.0, f32::NAN, f32::INFINITY, f32::MAX] {
                let px = requested_px * dpi;
                let measured = source.specimen_cell_size(px, line_height, &[]);
                let mut painted = source.fork_semantic_surface(px, Theme::default()).unwrap();
                painted.set_line_height(line_height);
                painted.set_font_variations(&[], 0.0);
                painted.activate_px(px);
                assert_eq!(
                    measured,
                    painted.cell_size(),
                    "{requested_px}px at {dpi}dpi with leading {line_height}"
                );
            }
        }
    }
    assert_eq!(source.cell_size(), original);
    assert_eq!(source.line_height(), 1.7);
    assert_ne!(
        source.specimen_cell_size(24.0, 1.0, &[]).1,
        source.cell_geometry(24.0).1,
        "the source's old leading is not the candidate's geometry"
    );

    source.set_line_height(2.0);
    let before = (source.cell_size(), source.line_height(), source.px());
    let target_px = 1_000_000_000.0;
    let mut painted = source
        .fork_semantic_surface(target_px, Theme::default())
        .expect("default leading admits the target size");
    assert_eq!(
        painted.line_height(),
        1.0,
        "inherited double leading was refused"
    );
    for candidate in [f32::NAN, 1.0004] {
        painted.set_line_height(candidate);
        assert_eq!(painted.line_height(), 1.0, "the candidate is a no-op");
        assert_eq!(
            source.specimen_cell_size(target_px, candidate, &[]),
            painted.cell_size(),
            "pure projection follows inherited leading refusal"
        );
    }
    assert_eq!(
        (source.cell_size(), source.line_height(), source.px()),
        before
    );
    source.set_line_height(1.0004);
    let painted = source
        .fork_semantic_surface(10_000.0, Theme::default())
        .unwrap();
    assert_eq!(
        painted.line_height(),
        1.0,
        "near-default inherited leading is a no-op"
    );
    assert_eq!(
        source.specimen_cell_size(10_000.0, f32::NAN, &[]),
        painted.cell_size(),
        "projection uses the inherited no-op policy"
    );
}

#[cfg(feature = "embedded-font")]
#[test]
fn specimen_cell_size_matches_variable_and_default_candidate_activation() {
    use crate::variation::WGHT_TAG;
    let (bytes, axis, _, _) = crate::variation::inked_wght_font();
    let mut source = Renderer::from_bytes(embedded_font(), 14.0, Theme::default()).unwrap();
    source.set_runtime_font_discovery(false);
    source.set_primary_font(&bytes).expect("variable primary");
    let nondefault = if axis.max != axis.def {
        axis.max
    } else {
        axis.min
    };
    source.set_font_variations(&[(WGHT_TAG, nondefault)], 0.0);
    assert!(
        source.var_coords.is_some(),
        "source carries a nondefault instance"
    );
    let original = source.var_coords.clone();
    for requests in [
        vec![],
        vec![(WGHT_TAG, axis.def)],
        vec![(WGHT_TAG, axis.min)],
    ] {
        for px in [12.0, 30.0, 72.0] {
            let measured = source.specimen_cell_size(px, 1.3, &requests);
            let mut painted = source.fork_semantic_surface(px, Theme::default()).unwrap();
            painted.set_line_height(1.3);
            painted.set_font_variations(&requests, 0.0);
            painted.activate_px(px);
            assert_eq!(
                measured,
                painted.cell_size(),
                "candidate {requests:?} at {px}px"
            );
        }
    }
    assert_eq!(
        source.var_coords, original,
        "measurement leaves the source instance intact"
    );
}

#[test]
fn set_line_height_drops_the_cell_box_caches_and_keeps_the_glyph() {
    for invalid in [
        f32::NAN,
        f32::NEG_INFINITY,
        f32::INFINITY,
        f32::MAX,
        0.0,
        -1.0,
    ] {
        assert!(
            Renderer::from_bytes(
                include_bytes!("../assets/DejaVuSansMono.ttf"),
                invalid,
                Theme::default(),
            )
            .is_err(),
            "constructor rejects unusable pixel size {invalid}"
        );
    }
    let mut r = renderer();
    let (w0, h0) = r.cell_size();
    let k = r.glyph_key('M');
    let glyph0 = r.glyph_image(k).clone();
    r.deco_mask(DecoGlyph::Dot, w0, h0);
    assert!(r.glyph_cache_len() > 0 && !r.deco_masks.borrow().is_empty());

    r.set_line_height(1.5);
    assert_eq!(r.glyph_cache_len(), 0, "the glyph cache is dropped");
    assert!(
        r.deco_masks.borrow().is_empty(),
        "the deco masks are dropped"
    );
    let k = r.glyph_key('M');
    let glyph1 = r.glyph_image(k).clone();
    assert_eq!(
        (glyph0.width(), glyph0.height()),
        (glyph1.width(), glyph1.height()),
        "the GLYPH itself is unchanged — only the cell box grew"
    );

    let px0 = r.px();
    let geometry = r.cell_size();
    let mut term = aterm_core::terminal::Terminal::new(2, 4);
    term.process(b"text");
    let input = term.cell_frame(2, 4);
    let valid = r.render_input(&input);
    let cached = r.glyph_cache_len();
    for invalid in [f32::NAN, f32::NEG_INFINITY, f32::INFINITY, f32::MAX] {
        r.set_line_height(invalid);
        r.set_px(invalid);
        r.activate_px(invalid);
        assert_eq!(r.line_height(), 1.5);
        assert_eq!(r.px(), px0);
        assert_eq!(r.cell_size(), geometry);
        assert_eq!(
            r.glyph_cache_len(),
            cached,
            "a refused request keeps rasters"
        );
        let after = r.render_input(&input);
        assert_eq!((after.width, after.height), (valid.width, valid.height));
        assert_eq!(after.pixels, valid.pixels, "invalid geometry {invalid}");
    }
}

#[test]
fn activate_px_light_switch_keeps_atlas_and_matches_set_px() {
    use aterm_core::terminal::Terminal;
    let mut r = renderer();
    let pa = r.px();
    let pb = pa * 2.0;
    let mut term = Terminal::new(1, 8);
    term.process(b"Metrics");
    let input_small = term.cell_frame(1, 8);
    let frame_a0 = r.render_input(&input_small);
    let cache_after_a = r.glyph_cache_len();
    assert!(cache_after_a > 0, "small-size render warmed the atlas");
    let (cwa, cha) = r.cell_size();

    r.activate_px(pb);
    assert_eq!(r.px(), pb, "activate re-targets the active px");
    let (cwb, chb) = r.cell_size();
    assert!(
        cwb > cwa && chb > cha,
        "2× activate grows the cell box: ({cwa},{cha}) -> ({cwb},{chb})"
    );
    assert!(
        r.glyph_cache_len() >= cache_after_a,
        "light switch must NOT drop the resident small-size atlas ({} vs {cache_after_a})",
        r.glyph_cache_len()
    );
    let _frame_b = r.render_input(&term.cell_frame(1, 8));
    assert!(
        r.glyph_cache_len() > cache_after_a,
        "both sizes now coexist in one atlas: {} > {cache_after_a}",
        r.glyph_cache_len()
    );

    r.activate_px(pa);
    assert_eq!(
        r.cell_size(),
        (cwa, cha),
        "switch-back restores the small cell box"
    );
    let frame_a1 = r.render_input(&input_small);
    assert_eq!(
        (frame_a1.width, frame_a1.height),
        (frame_a0.width, frame_a0.height),
        "switch-back frame has the small size's dimensions"
    );
    assert_eq!(
        frame_a1.pixels, frame_a0.pixels,
        "an activate round-trip renders byte-identically to the original small frame"
    );

    let mut fresh = renderer();
    fresh.set_px(pb);
    r.activate_px(pb);
    let frame_b_activated = r.render_input(&term.cell_frame(1, 8));
    let frame_b_fresh = fresh.render_input(&term.cell_frame(1, 8));
    assert_eq!(
        (frame_b_activated.width, frame_b_activated.height),
        (frame_b_fresh.width, frame_b_fresh.height),
        "activate and set_px agree on frame dimensions at pb"
    );
    assert_eq!(
        frame_b_activated.pixels, frame_b_fresh.pixels,
        "activate(pb) renders byte-identically to set_px(pb)"
    );
}

#[test]
fn cell_geometry_matches_activated_metrics() {
    for variable in [false, true] {
        let mut r = renderer();
        if variable {
            let (bytes, axis, _, _) = crate::variation::inked_wght_font();
            r.set_primary_font(&bytes).expect("variable primary");
            let nondefault = if axis.max != axis.def {
                axis.max
            } else {
                axis.min
            };
            r.set_font_variations(&[(variation::WGHT_TAG, nondefault)], 0.0);
            assert!(
                r.geom_coords.is_some(),
                "exercise the variable geometry path"
            );
        }
        r.set_adjust_underline(1, 1);
        let base = r.px();
        for px in [base, base * 1.5, base * 2.0, base * 0.75] {
            let live_before = r.px();
            let geometry = r.cell_geometry(px);
            let decoration = r.deco_metrics_for(px);
            assert_eq!(
                r.px(),
                live_before,
                "cell_geometry must not mutate the live px"
            );
            r.activate_px(px);
            let (cw, ch) = r.cell_size();
            assert_eq!(decoration, r.deco_metrics());
            assert_eq!(
                geometry,
                (cw, ch, r.baseline()),
                "cell_geometry({px}) must equal the activated live geometry"
            );
        }
        let (cw, ch) = r.cell_size();
        let geometry = (cw, ch, r.baseline());
        let px = r.px();
        for invalid in [
            f32::NAN,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::MAX,
            0.0,
            -1.0,
        ] {
            assert_eq!(
                r.cell_geometry(invalid),
                geometry,
                "invalid query {invalid}"
            );
            assert_eq!(
                r.deco_metrics_for(invalid),
                r.deco_metrics(),
                "refused sizes retain decoration scaling too"
            );
            r.set_px(invalid);
            r.activate_px(invalid);
            assert_eq!(r.cell_size(), (cw, ch));
            assert_eq!(r.baseline(), geometry.2);
            assert_eq!(r.px(), px);
        }
        let mut low_px = base;
        let mut high_px = base * 2.0;
        let low_height = r.derive_window_cell_geometry(low_px).unwrap().1;
        for _ in 0..20 {
            let mid = (low_px + high_px) / 2.0;
            if r.derive_window_cell_geometry(mid).unwrap().1 == low_height {
                low_px = mid;
            } else {
                high_px = mid;
            }
        }
        assert!(font_px_unchanged(low_px, high_px));
        r.activate_px(low_px);
        let live = (r.cell_size().0, r.cell_size().1, r.baseline());
        let rounded = r.derive_window_cell_geometry(high_px).unwrap();
        assert_ne!(
            rounded.1, live.1,
            "negative control: this request crosses a grid boundary"
        );
        assert_eq!(
            r.cell_geometry(high_px),
            live,
            "a no-op size query reports live geometry"
        );
        assert_eq!(
            r.deco_metrics_for(high_px),
            r.deco_metrics(),
            "no-op sizes retain decoration scaling too"
        );
        r.activate_px(high_px);
        r.set_px(high_px);
        assert_eq!((r.cell_size().0, r.cell_size().1, r.baseline()), live);
        assert_eq!(r.px(), low_px);
        r.activate_px(base * 2.0);
        assert_eq!(
            r.cell_geometry(high_px),
            rounded,
            "the earlier no-op projection must not survive as a memoized answer"
        );

        if variable {
            let (bytes, axis, _, _) = crate::variation::inked_wght_font();
            let low = [(variation::WGHT_TAG, axis.min)];
            let high = [(variation::WGHT_TAG, axis.max)];
            let low_advance = variation::varied_metrics_px(&bytes, 0, &low, 1.0)
                .expect("minimum-weight metrics")
                .m_advance;
            let high_advance = variation::varied_metrics_px(&bytes, 0, &high, 1.0)
                .expect("maximum-weight metrics")
                .m_advance;
            assert!(
                high_advance > low_advance,
                "the fixture's weight changes its advance"
            );
            let boundary_px = (2.0 * f64::from(i32::MAX)
                / (f64::from(low_advance) + f64::from(high_advance)))
                as f32;
            r.activate_px(base);
            r.set_line_height(0.5);
            r.set_font_variations(&low, 0.0);
            assert!(r.derive_window_cell_geometry(boundary_px).is_some());
            assert!(
                r.set_font_variations(&high, 0.0),
                "ordinary sizes admit the heavy instance"
            );
            assert!(
                r.derive_window_cell_geometry(boundary_px).is_none(),
                "negative control: the heavy instance exceeds the width limit"
            );
            r.set_font_variations(&low, 0.0);
            let key = r.glyph_key('M');
            let _ = r.glyph_image(key);
            let cached = r.glyph_cache_len();
            assert!(cached > 0);
            r.activate_px(boundary_px);
            assert_eq!(
                r.px(),
                boundary_px,
                "the light instance admits the boundary size"
            );
            let before = (r.cell_size(), r.baseline());
            let coords = r.variation_coords().map(<[_]>::to_vec);
            assert!(
                !r.set_font_variations(&high, 0.0),
                "refuse unusable instance geometry"
            );
            assert_eq!((r.cell_size(), r.baseline()), before);
            assert_eq!(r.variation_coords(), coords.as_deref());
            assert_eq!(r.font_variation_requests(), low);
            assert_eq!(r.glyph_cache_len(), cached, "refusal keeps the old rasters");

            let mut other = Renderer::from_bytes(
                include_bytes!("../assets/DejaVuSansMono.ttf"),
                base,
                Theme::default(),
            )
            .unwrap();
            other.set_line_height(0.5);
            other.set_font_variations(&high, 0.0);
            other.activate_px(boundary_px);
            assert_eq!(other.px(), boundary_px);
            let primary = other.shaping_primary_bytes.clone();
            let before = (other.cell_size(), other.baseline());
            assert!(
                other.set_primary_font(&bytes).is_err(),
                "a swap must preflight the inherited heavy instance"
            );
            assert_eq!(other.shaping_primary_bytes, primary);
            assert_eq!((other.cell_size(), other.baseline()), before);
            assert_eq!(other.font_variation_requests(), high);
        }
    }
}

fn ink_cell_rows(img: &GlyphImage, baseline: i32) -> Option<(i32, i32)> {
    let (w, h) = (img.width(), img.height());
    let ink = match img {
        GlyphImage::Mono { bytes, .. } => {
            ink_row_extent(h, |y| bytes[y * w..(y + 1) * w].iter().any(|&a| a > 0))
        }
        GlyphImage::Rgba { bytes, .. } => ink_row_extent(h, |y| {
            bytes[y * w * 4..(y + 1) * w * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[3] > 0)
        }),
    }?;
    let gy0 = baseline - h as i32 - img.ymin();
    Some((gy0 + ink.0 as i32, gy0 + ink.1 as i32))
}

#[test]
fn no_glyph_that_fits_its_cell_is_left_hanging_below_the_cell_floor() {
    let mut r = renderer();
    let mut seated_cases = 0usize;
    for px in [
        11.0_f32, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0, 24.0, 30.0,
    ] {
        r.activate_px(px);
        let (_, cell_h) = r.cell_size();
        let baseline = r.baseline();
        let floor = cell_h as i32;
        for ch in '\u{20}'..='\u{7E}' {
            let key = r.glyph_key(ch);
            let img = r.glyph_image(key);
            let Some((top, bottom)) = ink_cell_rows(img, baseline) else {
                continue;
            };
            assert!(
                top < floor,
                "{ch:?} at {px}px is drawn entirely below its own cell floor \
                 — ink rows {top}..{bottom} of 0..{cell_h} (baseline \
                 {baseline}) — so the band clip erases the character"
            );
            if top < baseline.min(floor) || bottom - top > floor {
                continue;
            }
            seated_cases += 1;
            assert!(
                top >= 0 && bottom <= floor,
                "{ch:?} at {px}px lives wholly below the baseline, so the clip \
                 costs it everything — but its ink is at rows {top}..{bottom} \
                 of 0..{cell_h} (baseline {baseline})"
            );
        }
    }
    assert!(
        seated_cases > 0,
        "clause (b) never fired: no printable ASCII glyph on this face lives \
         wholly below the baseline, so this test proved nothing"
    );
}

#[cfg(feature = "embedded-font")]
#[test]
fn an_underscore_is_as_solid_as_a_hyphen_at_every_size() {
    for px in [12.0_f32, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0, 24.0] {
        let mut r = Renderer::from_bytes(embedded_font(), px, Theme::default()).unwrap();
        let (cw, _) = r.cell_size();
        let mut input = RenderInput::empty();
        input.rows = 1;
        input.cols = 2;
        input.cells = vec![vec![RenderCell::default(); 2]];
        for (i, ch) in "_-".chars().enumerate() {
            input.cells[0][i].ch = ch;
            input.cells[0][i].fg = [255, 255, 255];
            input.cells[0][i].bg = [0, 0, 0];
        }
        let f = r.render_input(&input);
        let bg = f.pixels[0];
        let ink = |cell: usize| -> u32 {
            let x0 = r.pad() + cell * cw;
            let x1 = (x0 + cw).min(f.width);
            (0..f.height)
                .flat_map(|y| (x0..x1).map(move |x| y * f.width + x))
                .map(|i| {
                    let (p, b) = (f.pixels[i], bg);
                    (((p >> 16) & 0xff) as i32 - ((b >> 16) & 0xff) as i32).unsigned_abs()
                })
                .sum()
        };
        let (underscore, hyphen) = (ink(0), ink(1));
        assert!(hyphen > 0, "{px}px: the control glyph must have ink");
        assert!(
            underscore * 4 >= hyphen * 3,
            "{px}px: the underscore is being eaten by the cell floor — {underscore} \
             units of ink against the hyphen's {hyphen}"
        );
    }
}

#[cfg(feature = "embedded-font")]
#[test]
fn a_descender_is_not_lifted_off_the_line_it_shares() {
    for px in [12.0_f32, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0, 24.0, 30.0] {
        let mut r = Renderer::from_bytes(embedded_font(), px, Theme::default()).unwrap();
        let baseline = r.baseline();
        let top = |r: &mut Renderer, ch: char| {
            let key = r.glyph_key(ch);
            ink_cell_rows(r.glyph_image(key), baseline)
                .unwrap_or_else(|| panic!("{ch:?} rasterizes to ink at {px}px"))
                .0
        };
        let flat = top(&mut r, 'n');
        for ch in ['g', 'y', 'p', 'q'] {
            let d = top(&mut r, ch);
            assert!(
                d >= flat,
                "{ch:?} at {px}px starts at cell row {d}, ABOVE the {flat} of \
                 'n' — the descender has been lifted off the shared line"
            );
        }
    }
}

#[test]
fn the_seat_moves_only_ink_the_floor_clip_would_erase() {
    let (cell_h, baseline) = (17usize, 14i32);
    let floor = cell_h as i32;
    let place = |h: usize, last: usize, want_bottom: i32| -> i32 {
        baseline - h as i32 + last as i32 - want_bottom
    };
    for &(h, first, last) in &[(6usize, 1usize, 5usize), (20, 2, 19), (4, 0, 4)] {
        let ink_h = (last - first) as i32;
        for want_bottom in -2..=(floor + 4) {
            let ymin = place(h, last, want_bottom);
            let top = want_bottom - ink_h;
            let seated = seated_ymin((first, last), h, ymin, baseline, cell_h);
            let hangs = want_bottom > floor;
            let on_the_line = top < baseline.min(floor);
            let fits = ink_h <= floor;
            if !hangs || on_the_line || !fits {
                assert_eq!(
                    seated, None,
                    "ink rows {top}..{want_bottom} (h={h}, {first}..{last}) must \
                     keep its natural seat"
                );
                continue;
            }
            let seated = seated.expect("ink wholly below the baseline must be seated");
            assert!(
                seated > ymin,
                "the seat only ever raises ink: {ymin} -> {seated}"
            );
            let gy0 = baseline - h as i32 - seated;
            assert_eq!(
                gy0 + last as i32,
                floor,
                "seated ink rests exactly on the cell floor"
            );
            assert!(
                gy0 + first as i32 >= 0,
                "seated ink never leaves through the cell ceiling"
            );
        }
    }
}

#[test]
fn glyph_ink_box_trims_the_raster_pad_off_the_reported_box() {
    let padded = |rows: &[u8]| GlyphImage::Mono {
        width: 1,
        height: rows.len(),
        xmin: 3,
        ymin: -5,
        advance: 0.0,
        bytes: rows.to_vec(),
    };
    let dot = padded(&[0, 0, 200, 255, 200, 0]);
    assert_eq!(dot.height(), 6, "the reported box is six rows");
    assert_eq!(
        dot.ymin() + i32::try_from(dot.height()).unwrap(),
        1,
        "the reported box's top edge is a row ABOVE the baseline"
    );
    let (ink_h, ink_ymin) = glyph_ink_box(&dot).expect("the dot inks three rows");
    assert_eq!(
        (ink_h, ink_ymin),
        (3, -4),
        "the ink is three rows ending one row below the baseline"
    );
    assert!(
        ink_ymin + i32::try_from(ink_h).unwrap() <= 0,
        "and that ink box is a below-mark, which the reported box is not"
    );
    assert_eq!(glyph_ink_box(&padded(&[7, 0, 0, 0, 0, 9])), Some((6, -5)));
    assert_eq!(glyph_ink_box(&padded(&[0, 0, 0, 0, 0, 0])), None);
    let rgba = GlyphImage::Rgba {
        width: 1,
        height: 3,
        xmin: 0,
        ymin: 4,
        advance: 0.0,
        bytes: vec![9, 9, 9, 0, 9, 9, 9, 255, 9, 9, 9, 0],
    };
    assert_eq!(glyph_ink_box(&rgba), Some((1, 5)));
}

#[test]
fn an_inkless_mark_between_two_acutes_leaves_the_second_acute_where_it_was() {
    let mut r = renderer();
    r.set_line_height(2.0);
    let (acute, inkless) = ('\u{0301}', '\u{0302}');
    let acute_key = r.glyph_key(acute);
    let inkless_key = r.glyph_key(inkless);
    let (w, h, xmin, ymin) = {
        let a = r.glyph_image(acute_key);
        (a.width(), a.height(), a.xmin(), a.ymin())
    };
    assert!(w > 0 && h > 0, "the acute rasterizes to a non-empty box");
    r.glyphs.insert(
        inkless_key,
        CachedGlyph::new(GlyphImage::Mono {
            width: w,
            height: h,
            xmin,
            ymin,
            advance: 0.0,
            bytes: vec![0; w * h],
        }),
    );
    assert_eq!(
        r.glyph_ink_box_cached(inkless_key),
        None,
        "the fixture's box is non-empty but it has no ink — the case at issue"
    );

    let mut mark_rows = |text: &str| {
        let mut render = |text: &str| {
            let mut term = Terminal::new(2, 4);
            term.process(format!("\x1b[?25l\x1b[2;1H{text}").as_bytes());
            r.render_input(&term.cell_frame(2, 4))
        };
        let bare = render("e");
        let f = render(text);
        let w = f.width;
        (0..f.height)
            .filter(|&y| f.pixels[y * w..(y + 1) * w] != bare.pixels[y * w..(y + 1) * w])
            .collect::<Vec<_>>()
    };
    let one = mark_rows("e\u{0301}");
    let two = mark_rows("e\u{0301}\u{0301}");
    let two_around_blank = mark_rows("e\u{0301}\u{0302}\u{0301}");

    assert_ne!(
        one, two,
        "non-vacuity: at this line height the second acute really is lifted \
         clear of the first, so a stack that moved would be visible here"
    );
    assert_eq!(
        two_around_blank, two,
        "an inkless mark must neither paint nor push the acute after it"
    );
}

#[cfg(feature = "embedded-font")]
#[test]
fn set_primary_font_swaps_the_face_and_rerasterizes() {
    let jetbrains =
        std::fs::read(aterm_tempfile::manifest_dir().join("tests/fixtures/jetbrains-mono.ttf"))
            .expect("committed JetBrains Mono fixture");

    let mut r = Renderer::from_bytes(embedded_font(), 16.0, Theme::default()).unwrap();
    let dejavu_m = {
        let k = r.glyph_key('M');
        r.glyph_image(k).clone()
    };
    assert!(r.glyph_cache_len() > 0);

    r.set_primary_font(&jetbrains)
        .expect("JetBrains primary parses");
    assert_eq!(
        r.glyph_cache_len(),
        0,
        "swapping the primary face drops the glyph cache"
    );

    let jb_m = {
        let k = r.glyph_key('M');
        r.glyph_image(k).clone()
    };
    assert_ne!(
        dejavu_m.bytes(),
        jb_m.bytes(),
        "the new primary face must produce a different 'M' glyph"
    );
    r.set_line_height(1.4);
    let (_w, h_tall) = r.cell_size();
    r.set_primary_font(embedded_font()).expect("swap back ok");
    assert_eq!(
        r.line_height(),
        1.4,
        "line-height persists across a font swap"
    );
    let (_w2, h_after) = r.cell_size();
    assert!(h_after > 0 && h_tall > 0);

    let mut hostile = embedded_font().to_vec();
    let tables = u16::from_be_bytes(hostile[4..6].try_into().unwrap());
    let head = hostile[12..]
        .as_chunks::<16>()
        .0
        .iter()
        .take(usize::from(tables))
        .find(|record| &record[..4] == b"head")
        .expect("head table");
    let offset = u32::from_be_bytes(head[8..12].try_into().unwrap()) as usize;
    hostile[offset + 18..offset + 20].copy_from_slice(&16u16.to_be_bytes());
    assert!(
        Renderer::from_bytes(&hostile, 16.0, Theme::default()).is_ok(),
        "negative control: the candidate is a usable font at an ordinary size"
    );
    for px in [20_000_000.0, 100_000_000.0] {
        r.activate_px(px);
        assert_eq!(r.px(), px, "the old face admits the large size");
        let before = (r.cell_size(), r.baseline(), r.glyph_cache_len());
        let primary = r.shaping_primary_bytes.clone();
        assert!(r.set_primary_font(&hostile).is_err());
        assert_eq!((r.cell_size(), r.baseline(), r.glyph_cache_len()), before);
        assert_eq!(
            r.shaping_primary_bytes, primary,
            "rejection retains the old face"
        );
    }
}

#[test]
fn render_input_cached_reuses_buffer_and_matches_render_input() {
    let mut r = renderer();
    let mut wc = WindowCpu::new();
    let mut term = Terminal::new(3, 12);
    term.process(b"reuse me\x1b[31m!\x1b[0m");
    let input = term.cell_frame(3, 12);

    let (ptr1, expected) = {
        let f = r.render_input_cached(&mut wc, &input);
        (f.pixels().as_ptr(), f.pixels().to_vec())
    };
    let ptr2 = {
        let f = r.render_input_cached(&mut wc, &input);
        assert_eq!(
            f.pixels(),
            expected.as_slice(),
            "reused-buffer frame differs frame-to-frame"
        );
        f.pixels().as_ptr()
    };
    assert_eq!(
        ptr1, ptr2,
        "steady-size frame must REUSE the pixel allocation"
    );

    let owned = r.render_input(&input);
    assert_eq!(
        owned.pixels, expected,
        "render_input_cached must match render_input pixels"
    );
    assert_eq!(
        (owned.width, owned.height),
        (input.cols * r.cell_w, input.rows * r.cell_h)
    );
}

#[test]
fn row_glyph_plan_reuses_segment_plans_for_distinct_streamed_tokens() {
    let font = include_bytes!("../tests/fixtures/jetbrains-mono.ttf");
    let mut renderer = Renderer::from_bytes(font, 16.0, Theme::default()).expect("ligature font");
    assert!(
        renderer.has_ligature_features,
        "fixture enables real shaping"
    );
    let mut terminal = Terminal::new(2, 120);
    terminal.process(b"\x1b[?25lalpha beta gamma delta epsilon zeta eta theta iota kappa lambda");
    let input = terminal.cell_frame(2, 120);
    let mut plan = Vec::new();
    let _ = ligature_shaping::take_shape_plan_builds();
    renderer.row_glyph_plan(&input, 0, &[], &mut plan);
    assert_eq!(
        ligature_shaping::take_shape_plan_builds(),
        1,
        "all eleven misses share one Latin plan"
    );
    assert!(
        renderer.shaped_runs.cur_len() >= 11,
        "the row must really miss for distinct tokens"
    );
    renderer.row_glyph_plan(&input, 0, &[], &mut plan);
    assert_eq!(
        ligature_shaping::take_shape_plan_builds(),
        0,
        "warm text creates no face or plan"
    );

    terminal.process("\x1b[2;1Hfresh => output Ελληνικά".as_bytes());
    let input = terminal.cell_frame(2, 120);
    renderer.row_glyph_plan(&input, 1, &[], &mut plan);
    assert_eq!(
        ligature_shaping::take_shape_plan_builds(),
        2,
        "Latin is reused across rows while common-only and Greek build distinct plans"
    );
    assert!(
        plan.iter().any(|g| !matches!(g, ColumnGlyph::PerCell)),
        "the operator must still ligate"
    );

    let mut toggled = renderer.shaping.clone();
    toggled.ligature_mode = aterm_types::text_shaping::LigatureMode::Disabled;
    renderer.set_text_shaping(toggled);
    renderer.set_text_shaping(aterm_types::text_shaping::TextShapingConfig::default());
    renderer.row_glyph_plan(&input, 1, &[], &mut plan);
    assert_eq!(
        ligature_shaping::take_shape_plan_builds(),
        3,
        "a feature change clears the persistent plan memo so Latin, common-only and Greek rebuild"
    );
}

#[test]
fn dropped_cpu_surface_present_forces_full_retry() {
    let mut r = renderer();
    let mut wc = WindowCpu::new();
    let mut term = Terminal::new(3, 12);
    term.process(b"surface retry");
    let input = term.cell_frame(3, 12);

    let expected = r.render_input_cached(&mut wc, &input).pixels().to_vec();
    let _ = r.render_input_cached(&mut wc, &input);
    assert_eq!(wc.last_damage(), DamageOutcome::GateHit);
    wc.set_presented_chrome(true);

    wc.invalidate();
    assert_eq!(wc.last_damage(), DamageOutcome::Full);
    assert!(wc.frame_pixels().is_empty());
    assert!(wc.presented_chrome());

    let retried = r.render_input_cached(&mut wc, &input).pixels().to_vec();
    assert_eq!(wc.last_damage(), DamageOutcome::Full);
    assert_eq!(
        retried, expected,
        "retry fully reconstructs the intended frame"
    );
}

#[test]
fn interleaved_windowcpu_no_cross_contamination() {
    let mut r = renderer();
    let mut term_a = Terminal::new(3, 12);
    term_a.process(b"window A text\x1b[31m!\x1b[0m");
    let input_a = term_a.cell_frame(3, 12);
    let mut term_b = Terminal::new(3, 12);
    term_b.process(b"DIFFERENT b\x1b[42mX\x1b[0m");
    let input_b = term_b.cell_frame(3, 12);

    let expected_a = {
        let mut wc_fresh = WindowCpu::new();
        r.render_input_cached(&mut wc_fresh, &input_a)
            .pixels()
            .to_vec()
    };

    let mut wc_a = WindowCpu::new();
    let mut wc_b = WindowCpu::new();
    {
        let va = r.render_input_cached(&mut wc_a, &input_a);
        assert_eq!(
            va.pixels(),
            expected_a.as_slice(),
            "wc_A first render must equal A"
        );
    }
    {
        let _vb = r.render_input_cached(&mut wc_b, &input_b);
    }
    let va2 = r.render_input_cached(&mut wc_a, &input_a);
    assert_eq!(
        va2.pixels(),
        expected_a.as_slice(),
        "wc_A re-render after an interleaved wc_B frame must still be byte-identical to a fresh render_input(A) — no cross-window contamination / false gate hit"
    );
}

#[test]
fn padding_and_head_inset_the_grid_and_grow_the_framebuffer() {
    let mut r = renderer();
    let (cw, ch) = r.cell_size();
    let (rows, cols) = (3usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"pad me\x1b[31m!\x1b[0m");
    let input = term.cell_frame(rows, cols);
    let base = r.render_input(&input);
    assert_eq!((base.width, base.height), (cols * cw, rows * ch));

    const P: usize = 8;
    const H: usize = 24;
    r.set_pad(P);
    r.set_head(H);
    assert_eq!(r.grid_top(), P + H, "grid_top = pad + head");
    let inset = r.render_input(&input);
    assert_eq!(
        (inset.width, inset.height),
        (base.width + 2 * P, base.height + 2 * P + H),
        "the pad grows both axes, the head the height only"
    );
    let bg = Theme::default().bg;
    for y in 0..inset.height {
        for x in 0..inset.width {
            let got = inset.pixels[y * inset.width + x];
            let (gx, gy) = (x.wrapping_sub(P), y.wrapping_sub(P + H));
            if gx < base.width && gy < base.height {
                assert_eq!(got, base.pixels[gy * base.width + gx], "grid ({gx},{gy})");
            } else {
                assert_eq!(got, bg, "border pixel ({x},{y}) is bg");
            }
        }
    }

    r.set_head(0);
    r.set_pad(0);
    assert_eq!(
        r.render_input(&input).pixels,
        base.pixels,
        "pad 0 and head 0 restore the byte-identical render"
    );
}

#[test]
fn a_bleed_below_a_surfaceless_strip_leaves_the_strip_on_the_padding() {
    let mut r = renderer();
    const P: usize = 8;
    const BAND: u32 = 0x0030_3135;
    let (_, ch) = r.cell_size();
    let (rows, cols) = (4usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"strip row");
    let input = term.cell_frame(rows, cols);
    r.set_pad(P);
    let base = r.render_input(&input);
    let bg = Theme::default().bg;
    let top = r.grid_top();
    let w = base.width;
    let bleed = ChromeBleed {
        rows: 2,
        first: 1,
        color: BAND,
        seam: None,
        top_extends_cells: false,
        row_edges: [None; CHROME_ROW_EDGES],
    };
    r.set_chrome_bleed(Some(bleed));
    let bled = r.render_input(&input);
    assert_eq!(
        r.chrome_extent_px(bled.height),
        top + ch..top + 2 * ch,
        "the presenters continue the band row alone: the lip and the strip \
         row keep the flat remainder bands"
    );
    let at = |px: &[u32], x: usize, y: usize| px[y * w + x];
    for x in 0..w {
        for y in 0..top {
            assert_eq!(
                at(&bled.pixels, x, y),
                bg,
                "row 0 owns no lip below `first` ({x},{y})"
            );
        }
    }
    for y in top..top + ch {
        for x in (0..P).chain(w - P..w) {
            assert_eq!(
                at(&bled.pixels, x, y),
                bg,
                "the strip row keeps the padding ({x},{y})"
            );
        }
    }
    for y in top + ch..top + 2 * ch {
        for x in (0..P).chain(w - P..w) {
            assert_eq!(
                at(&bled.pixels, x, y),
                BAND,
                "the band row bleeds ({x},{y})"
            );
        }
    }
    r.set_chrome_bleed(Some(ChromeBleed { rows: 1, ..bleed }));
    assert_eq!(r.chrome_bleed(), None, "every row skipped: no bleed");
    assert_eq!(r.render_input(&input).pixels, base.pixels);
}

#[test]
fn a_metered_chrome_row_continues_its_edge_cells_into_the_gutters() {
    let mut r = renderer();
    const P: usize = 8;
    const BAND: u32 = 0x0030_3135;
    const SEAM: u32 = 0x0060_6164;
    const FILL: u32 = 0x0050_FA7B;
    const TRACK: u32 = 0x0044_4750;
    let (_, ch) = r.cell_size();
    let (rows, cols) = (4usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"metered row");
    let input = term.cell_frame(rows, cols);
    r.set_pad(P);
    let base = r.render_input(&input);
    let (w, hgt) = (base.width, base.height);
    assert_eq!(r.chrome_extent_px(hgt), 0..0, "no chrome, no extent");
    let bleed = ChromeBleed {
        rows: 2,
        first: 0,
        color: BAND,
        seam: Some(SEAM),
        top_extends_cells: false,
        row_edges: [
            Some(ChromeRowEdges {
                row: 1,
                left: FILL,
                right: TRACK,
            }),
            None,
            None,
        ],
    };
    assert_eq!(bleed.gutter_tones(0), (BAND, BAND));
    assert_eq!(bleed.gutter_tones(1), (FILL, TRACK));
    assert_eq!(
        bleed.gutter_tones(2),
        (BAND, BAND),
        "past the chrome: the band tone"
    );
    r.set_chrome_bleed(Some(bleed));
    let top = r.grid_top();
    assert_eq!(r.chrome_extent_px(hgt), 0..top + 2 * ch);
    assert_eq!(
        r.chrome_extent_px(top + 1),
        0..top + 1,
        "clamped to the frame"
    );
    let bled = r.render_input(&input);
    let at = |px: &[u32], x: usize, y: usize| px[y * w + x];
    let deco = r.deco_metrics();
    let t = deco.underline_t.max(1).min(ch);
    let seam = top + ch + deco.underline_y.min(ch - t);
    for y in top + ch..top + 2 * ch {
        for x in 0..P {
            let want = if (seam..seam + t).contains(&y) {
                SEAM
            } else {
                FILL
            };
            assert_eq!(at(&bled.pixels, x, y), want, "row 1 left gutter ({x},{y})");
        }
        for x in w - P..w {
            let want = if (seam..seam + t).contains(&y) {
                SEAM
            } else {
                TRACK
            };
            assert_eq!(at(&bled.pixels, x, y), want, "row 1 right gutter ({x},{y})");
        }
    }
    let mut wc = WindowCpu::default();
    assert_eq!(r.render_input_cached(&mut wc, &input).pixels(), bled.pixels);
    let mut term2 = Terminal::new(rows as u16, cols as u16);
    term2.process(b"metered row\r\nnow lit");
    let moved = term2.cell_frame(rows, cols);
    r.set_chrome_bleed(Some(ChromeBleed {
        row_edges: [
            Some(ChromeRowEdges {
                row: 1,
                left: FILL,
                right: FILL,
            }),
            None,
            None,
        ],
        ..bleed
    }));
    let fresh = r.render_input(&moved);
    assert_eq!(
        r.render_input_cached(&mut wc, &moved).pixels(),
        fresh.pixels,
        "incremental frame must match a fresh one when a row and its tones change together"
    );
}

#[test]
fn a_band_icon_draws_in_place_of_the_glyph_and_keeps_the_cell() {
    let mut r = renderer();
    const P: usize = 8;
    const BG: u32 = 0x0030_3135;
    let (cw, ch) = r.cell_size();
    r.set_pad(P);
    for icon in BandIcon::ALL {
        let mut term = Terminal::new(3, 6);
        let mut bytes = [0u8; 4];
        term.process(icon.ch().encode_utf8(&mut bytes).as_bytes());
        term.process(b" ab");
        let mut input = term.cell_frame(3, 6);
        for cell in &mut input.cells[0] {
            cell.fg = [0xf1, 0xfa, 0x8c];
            cell.bg = [0x30, 0x31, 0x35];
        }
        let mut blank_in = input.clone();
        blank_in.cells[0][0].ch = ' ';
        let blank = r.render_input(&blank_in);
        let mut drawn_in = input.clone();
        drawn_in.chrome_rasters.push(ChromeRaster {
            row: 0,
            ground: std::sync::Arc::from(Vec::new()),
            rail: std::sync::Arc::from(Vec::new()),
            rail_h: 0,
            clear_rail: false,
            own: Vec::new(),
            split: None,
            rings: Vec::new(),
            icons: vec![ChromeIcon { col: 0, icon }],
        });
        let drawn = r.render_input(&drawn_in);
        assert_eq!(
            drawn_in.cells[0][0].ch,
            icon.ch(),
            "{icon:?}: the cell keeps its char"
        );
        let (w, top) = (drawn.width, r.grid_top());
        let px = |f: &Frame, x: usize, y: usize| f.pixels[y * w + x] & 0x00ff_ffff;
        let mut ink = 0usize;
        let mut partial = false;
        for y in 0..drawn.height {
            for x in 0..w {
                let in_cell = (P..P + cw).contains(&x) && (top..top + ch).contains(&y);
                let (a, b) = (px(&blank, x, y), px(&drawn, x, y));
                if in_cell {
                    if b != BG {
                        ink += 1;
                        partial |= b != 0x00f1_fa8c;
                    }
                } else {
                    if !(P..P + cw).contains(&x) {
                        assert_eq!(a, b, "{icon:?}: pixel ({x},{y}) outside the cell moved");
                    }
                }
            }
        }
        assert!(ink > 2, "{icon:?}: the icon draws ({ink} px)");
        let thin = procedural::mark_stroke(cw, ch, r.icon_metrics(false)) == 1.0;
        assert!(
            partial
                || matches!(icon, BandIcon::Pause | BandIcon::Alert)
                || (icon == BandIcon::DottedRing && matches!(ink, 4 | 8))
                || (icon == BandIcon::InsetDiamond && thin),
            "{icon:?}: antialiased edges, or ◌'s four or eight dots each a whole pixel ({ink} \
             px), or ◈ with a one-pixel stroke ({thin}) as whole staircases \
             (procedural::icons holds it to that)"
        );
        let regular = r.chrome_icon_key(drawn_in.chrome_raster(0), &drawn_in.cells[0], 0);
        assert_eq!(
            regular,
            Some(band_icon_key(icon, StyleBits::REGULAR, false, r.px_q))
        );
        assert_eq!(
            r.chrome_icon_key(drawn_in.chrome_raster(0), &drawn_in.cells[0], 1),
            None
        );
        let mut bold = drawn_in.clone();
        bold.cells[0][0].bold = true;
        let heavy = r.render_input(&bold);
        let count = |f: &Frame| {
            (top..top + ch)
                .flat_map(|y| (P..P + cw).map(move |x| (x, y)))
                .filter(|&(x, y)| px(f, x, y) != BG)
                .count()
        };
        assert!(count(&heavy) > 2, "{icon:?}: the bold icon draws");
        assert!(
            count(&heavy) >= count(&drawn),
            "{icon:?}: a bold cell's icon never draws lighter"
        );
    }
    let mut parted_own = std::collections::BTreeSet::new();
    let mut parted_spilled = std::collections::BTreeSet::new();
    let mut semibold_default = false;
    for px in [16.0f32, 20.0, 24.0, 28.0, 32.0, 40.0, 48.0] {
        let mut r = Renderer::from_system(px, Theme::default()).expect(NO_FONT);
        semibold_default = r.primary_source_path() == Some(bundled::IOSEVKA_SEMIBOLD_ID);
        r.set_pad(P);
        let (cw, ch) = r.cell_size();
        let top = r.grid_top();
        let span = |f: &Frame, cells: usize| {
            (top..top + ch)
                .flat_map(|y| (P..P + cells * cw).map(move |x| f.pixels[y * f.width + x]))
                .collect::<Vec<u32>>()
        };
        for icon in BandIcon::ALL {
            let raster = |col: u16| ChromeRaster {
                row: 0,
                ground: std::sync::Arc::from(Vec::new()),
                rail: std::sync::Arc::from(Vec::new()),
                rail_h: 0,
                clear_rail: false,
                own: Vec::new(),
                split: None,
                rings: Vec::new(),
                icons: vec![ChromeIcon { col, icon }],
            };
            let mut term = Terminal::new(3, 6);
            let mut bytes = [0u8; 4];
            term.process(icon.ch().encode_utf8(&mut bytes).as_bytes());
            let mut input = term.cell_frame(3, 6);
            input.chrome_rasters.push(raster(0));
            let regular = r.render_input(&input);
            input.cells[0][0].bold = true;
            let bold = r.render_input(&input);
            if span(&regular, 1) != span(&bold, 1) {
                parted_own.insert(format!("{icon:?}"));
            }
            let mut term = Terminal::new(3, 6);
            term.process(b"   ");
            let mut input = term.cell_frame(3, 6);
            input.cells[0][1].ch = icon.ch();
            input.chrome_rasters.push(raster(1));
            assert_eq!(
                r.chrome_icon_key(input.chrome_raster(0), &input.cells[0], 1),
                Some(band_icon_key(icon, StyleBits::REGULAR, true, r.px_q)),
                "{icon:?}: between blanks the key spills"
            );
            let regular = r.render_input(&input);
            input.cells[0][1].bold = true;
            let bold = r.render_input(&input);
            if span(&regular, 3) != span(&bold, 3) {
                parted_spilled.insert(format!("{icon:?}"));
            }
        }
    }
    let (parted, layout) = if semibold_default {
        (&parted_spilled, "spilled between blanks")
    } else {
        (&parted_own, "in its own cell")
    };
    for icon in BandIcon::ALL {
        if !matches!(icon, BandIcon::Sparkle | BandIcon::Disc | BandIcon::Diamond) {
            assert!(
                parted.contains(&format!("{icon:?}")),
                "{icon:?}: a bold cell drew it at the regular stem at every size, {layout}"
            );
        }
    }
}

#[test]
fn a_band_icon_spills_into_its_blank_neighbours_and_no_further() {
    let mut r = renderer();
    const P: usize = 8;
    const BG: u32 = 0x0030_3135;
    let (cw, ch) = r.cell_size();
    r.set_pad(P);
    let top = r.grid_top();
    let baseline = usize::try_from(r.baseline()).unwrap();
    let cap = r.icon_metrics(true).cap;
    for icon in BandIcon::ALL {
        let frame = |r: &mut Renderer, g: char, right: char, drawn: bool| {
            let mut term = Terminal::new(3, 8);
            term.process(b"   Ab\r\nxyz");
            let mut input = term.cell_frame(3, 8);
            input.cells[0][1].ch = g;
            input.cells[0][2].ch = right;
            for cell in &mut input.cells[0] {
                cell.fg = [0xf1, 0xfa, 0x8c];
                cell.bg = [0x30, 0x31, 0x35];
                cell.bold = true;
            }
            if drawn {
                input.chrome_rasters.push(ChromeRaster {
                    row: 0,
                    ground: std::sync::Arc::from(Vec::new()),
                    rail: std::sync::Arc::from(Vec::new()),
                    rail_h: 0,
                    clear_rail: false,
                    own: Vec::new(),
                    split: None,
                    rings: Vec::new(),
                    icons: vec![ChromeIcon { col: 1, icon }],
                });
            }
            let key = r.chrome_icon_key(input.chrome_raster(0), &input.cells[0], 1);
            (r.render_input(&input), key)
        };
        let (blank, _) = frame(&mut r, ' ', ' ', false);
        let (drawn, key) = frame(&mut r, icon.ch(), ' ', true);
        assert_eq!(
            key,
            Some(band_icon_key(icon, StyleBits::BOLD, true, r.px_q)),
            "{icon:?}: between blanks the key spills"
        );
        let w = drawn.width;
        let px = |f: &Frame, x: usize, y: usize| f.pixels[y * w + x] & 0x00ff_ffff;
        let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0, usize::MAX, 0);
        for y in 0..drawn.height {
            for x in 0..w {
                let (a, b) = (px(&blank, x, y), px(&drawn, x, y));
                let in_three = (P..P + 3 * cw).contains(&x) && (top..top + ch).contains(&y);
                if !in_three {
                    assert_eq!(
                        a, b,
                        "{icon:?}: pixel ({x},{y}) outside the three cells moved"
                    );
                } else if b != BG {
                    (x0, x1) = (x0.min(x), x1.max(x + 1));
                    (y0, y1) = (y0.min(y), y1.max(y + 1));
                }
            }
        }
        assert!(x1 > x0, "{icon:?}: the icon draws");
        assert!(
            y1 <= top + baseline + 1,
            "{icon:?}: no ink below the baseline's row ({y1})"
        );
        if matches!(icon, BandIcon::Info | BandIcon::Warn) {
            assert!(
                x0 < P + cw && x1 > P + 2 * cw,
                "{icon:?}: the badge spills into both blank cells ({x0}..{x1})"
            );
            assert!(
                (y1 - y0) as f32 > cap,
                "{icon:?}: the badge is over the capital height ({} vs {cap})",
                y1 - y0
            );
        }
        let (_, key) = frame(&mut r, icon.ch(), 'x', true);
        assert_eq!(
            key,
            Some(band_icon_key(icon, StyleBits::BOLD, false, r.px_q)),
            "{icon:?}: beside ink the key is the cell's own"
        );
    }
}

#[test]
fn a_spilled_icon_stays_between_the_faces_tallest_letter_and_its_baseline() {
    for px in [12.0f32, 14.0, 16.0, 20.0, 24.0, 27.0, 32.0] {
        let mut r = Renderer::from_system(px, Theme::default()).expect(NO_FONT);
        let (cw, ch) = r.cell_size();
        let head = r.ascii_ink_room().0;
        let baseline = usize::try_from(r.baseline()).unwrap();
        for icon in BandIcon::ALL {
            for style in [StyleBits::REGULAR, StyleBits::BOLD] {
                let key = band_icon_key(icon, style, true, r.px_q);
                let GlyphImage::Mono {
                    width,
                    height,
                    bytes,
                    ..
                } = r.glyph_image(key)
                else {
                    panic!("{icon:?}: a Mono raster");
                };
                assert_eq!(
                    (*width, *height),
                    (procedural::band_icon_width(cw, !icon.is_glass_mark()), ch)
                );
                let rows: Vec<usize> = (0..*height)
                    .filter(|&y| bytes[y * width..(y + 1) * width].iter().any(|&c| c > 0))
                    .collect();
                let (y0, y1) = (rows[0], rows[rows.len() - 1]);
                let at = format!("{icon:?} {style:?} at {px} px ({cw}x{ch})");
                assert!(y0 >= head, "{at}: ink at row {y0} above the head {head}");
                assert!(
                    y1 <= baseline,
                    "{at}: ink at row {y1} below the baseline {baseline}"
                );
            }
        }
    }
}

/// The drawn glass marks read as one set with the default face's own: the ring family matches
/// its `●`, and `⤓` lies within a row of the face's own `⤓` where it has one (Iosevka), else of
/// its `↓` (SF Mono has no `⤓`). `⤓` stands on the baseline at the capitals' height, which
/// hinting snaps with the digits beside it (`⤓18:00`), not with the arrows.
#[test]
fn the_glass_marks_read_as_one_set_at_1x_and_2x() {
    let ink_box = |w: usize, bytes: &[u8]| {
        let h = bytes.len() / w;
        let inked = |x: usize, y: usize| bytes[y * w + x] > 0;
        let xs: Vec<usize> = (0..w).filter(|&x| (0..h).any(|y| inked(x, y))).collect();
        let ys: Vec<usize> = (0..h).filter(|&y| (0..w).any(|x| inked(x, y))).collect();
        (xs[0], xs[xs.len() - 1], ys[0], ys[ys.len() - 1])
    };
    let size = |(x0, x1, y0, y1): (usize, usize, usize, usize)| (x1 - x0 + 1, y1 - y0 + 1);
    let near = |a: usize, b: usize| a.abs_diff(b) <= 1;
    const FAMILY: [BandIcon; 12] = [
        BandIcon::Disc,
        BandIcon::Ring,
        BandIcon::HalfDisc,
        BandIcon::DottedRing,
        BandIcon::SlashedRing,
        BandIcon::Fisheye,
        BandIcon::InsetDiamond,
        BandIcon::House,
        BandIcon::Diamond,
        BandIcon::Fork,
        BandIcon::Enter,
        BandIcon::Reset,
    ];
    for px in [12.0f32, 24.0] {
        let mut r = Renderer::from_system(px, Theme::default()).expect(NO_FONT);
        r.debug_block_on_lazy_fallbacks();
        let (cw, ch) = r.cell_size();
        #[cfg(target_os = "macos")]
        let baseline = r.baseline();
        for (style, bold) in [(StyleBits::REGULAR, false), (StyleBits::BOLD, true)] {
            let own = |r: &mut Renderer, c: char| {
                let key = r.glyph_key_styled(c, style);
                (matches!(key.source, FaceId::Primary | FaceId::BoldPrimary)
                    && r.primary_unicode_gid(c).is_some())
                .then(|| match r.glyph_image(key) {
                    GlyphImage::Mono { width, bytes, .. } => Some(size(ink_box(*width, bytes))),
                    GlyphImage::Rgba { .. } => None,
                })
                .flatten()
            };
            let dot = own(&mut r, '\u{25cf}');
            let arrow = own(&mut r, '\u{2193}');
            let land = own(&mut r, '\u{2913}');
            let at = format!("{px} px ({cw}x{ch}) {style:?}");
            let drawn = |r: &mut Renderer, icon| {
                let key = band_icon_key(icon, style, true, r.px_q);
                assert_eq!(
                    key,
                    band_icon_key(icon, style, false, r.px_q),
                    "{at}: {icon:?} never spills"
                );
                let GlyphImage::Mono { width, bytes, .. } = r.glyph_image(key) else {
                    panic!("{at}: {icon:?} is a Mono raster");
                };
                assert_eq!(*width, cw, "{at}: {icon:?} is drawn in its own cell");
                ink_box(*width, bytes)
            };
            let boxes: Vec<_> = FAMILY.into_iter().map(|i| drawn(&mut r, i)).collect();
            assert!(
                boxes.iter().all(|b| *b == boxes[0]),
                "{at}: ● ○ ◐ ◌ ⊘ ◉ ◈ ⌂ ◆ ⎇ ⎆ ↺ measure {boxes:?}"
            );
            let (w, h) = size(boxes[0]);
            if let Some((dw, dh)) = dot {
                assert!(
                    near(w, dw.min(cw)) && near(h, dh.min(cw)),
                    "{at}: the family {w}x{h}, the face's ● {dw}x{dh} in a {cw} px cell"
                );
            }
            let face_arrow = land.map(|s| ('⤓', s)).or(arrow.map(|s| ('↓', s)));
            if let Some((name, (_, ah))) = face_arrow {
                let (_, th) = size(drawn(&mut r, BandIcon::ToBar));
                assert!(near(th, ah), "{at}: ⤓ {th} rows, the face's {name} {ah}");
            }
            let m = r.icon_metrics(bold);
            let control = |m: procedural::IconMetrics| {
                ink_box(
                    cw,
                    &procedural::band_icon_coverage(BandIcon::Disc, cw, ch, m, false),
                )
            };
            let shorter = procedural::IconMetrics {
                disc: m.disc - 3.0,
                ..m
            };
            assert_ne!(
                control(shorter),
                boxes[0],
                "{at}: control, a disc off another face's ●"
            );
            #[cfg(target_os = "macos")]
            if let (Some(_), Some((_, ah))) = (dot, arrow) {
                let family = {
                    let (_, _, _, y1) = boxes[0];
                    (w, h, y1 as i32 - baseline)
                };
                let font = |r: &mut Renderer, c: char| {
                    let key = r.glyph_key_styled(c, style);
                    assert!(
                        !matches!(key.source, FaceId::Primary | FaceId::BoldPrimary),
                        "{px} px: the default face has {c}: the control no longer holds"
                    );
                    let GlyphImage::Mono {
                        width,
                        height,
                        ymin,
                        bytes,
                        ..
                    } = r.glyph_image(key)
                    else {
                        panic!("{px} px: {c} is a Mono glyph");
                    };
                    let b = ink_box(*width, bytes);
                    let (gw, gh) = size(b);
                    (gw, gh, b.3 as i32 - *height as i32 - *ymin)
                };
                let marks: &[char] = if bold {
                    &['\u{25d0}', '\u{25cc}', '\u{25c9}', '\u{25c8}', '\u{25c6}']
                } else {
                    &[
                        '\u{25d0}', '\u{25cc}', '\u{25c9}', '\u{25c8}', '\u{2302}', '\u{25c6}',
                    ]
                };
                for &c in marks {
                    let glyph = font(&mut r, c);
                    assert_ne!(
                        glyph, family,
                        "{px} px {style:?}: control, the font's {c} (w, h, lowest row) beside the family's"
                    );
                }
                let (_, th, _) = font(&mut r, '\u{2913}');
                assert!(
                    !near(th, ah),
                    "{px} px {style:?}: control, the font's ⤓ {th} rows beside ↓ {ah}"
                );
            }
        }
    }
}

#[test]
fn a_glass_mark_keeps_one_ink_box_whatever_its_neighbours_hold() {
    const P: usize = 8;
    const COLS: usize = 5;
    let faces: &[Option<&str>] = if cfg!(target_os = "macos") {
        &[None, Some("Menlo")]
    } else {
        &[None]
    };
    let rows: [[char; 2]; 4] = [
        [' ', ' '],
        ['\u{2502}', '\u{2502}'],
        ['\u{221e}', ' '],
        ['H', 'H'],
    ];
    for &face in faces {
        for px in [12.0f32, 24.0] {
            let mut r =
                Renderer::from_system_with_family(face, px, Theme::default()).expect(NO_FONT);
            if let Some(name) = face {
                assert!(
                    r.primary_source_path().is_some_and(|p| p.contains(name)),
                    "{name} is the primary face"
                );
            }
            r.debug_block_on_lazy_fallbacks();
            r.set_pad(P);
            let (cw, ch) = r.cell_size();
            let top = r.grid_top();
            let ink = |r: &mut Renderer, around: [char; 2], icon: BandIcon, bold: bool| {
                let frame = |r: &mut Renderer, drawn: bool| {
                    let mut input = Terminal::new(1, COLS as u16).cell_frame(1, COLS);
                    input.cells[0].resize(COLS, RenderCell::default());
                    input.cells[0][1].ch = around[0];
                    input.cells[0][2].ch = if drawn { icon.ch() } else { ' ' };
                    input.cells[0][3].ch = around[1];
                    for cell in &mut input.cells[0] {
                        cell.fg = [0x20, 0x20, 0x20];
                        cell.bg = [0xf0, 0xf0, 0xf0];
                        cell.bold = bold;
                    }
                    if drawn {
                        input.chrome_rasters.push(ChromeRaster {
                            row: 0,
                            ground: std::sync::Arc::from(Vec::new()),
                            rail: std::sync::Arc::from(Vec::new()),
                            rail_h: 0,
                            clear_rail: false,
                            own: Vec::new(),
                            split: None,
                            rings: Vec::new(),
                            icons: vec![ChromeIcon { col: 2, icon }],
                        });
                    }
                    r.render_input(&input)
                };
                let (blank, drawn) = (frame(r, false), frame(r, true));
                let (x_cell, w) = ((P + 2 * cw) as i32, drawn.width);
                let (mut b, mut outside) = ((i32::MAX, i32::MIN, i32::MAX, i32::MIN), false);
                for (i, (&a, &d)) in blank.pixels.iter().zip(&drawn.pixels).enumerate() {
                    if a & 0x00ff_ffff == d & 0x00ff_ffff {
                        continue;
                    }
                    let (x, y) = ((i % w) as i32 - x_cell, (i / w) as i32 - top as i32);
                    outside |= !(0..cw as i32).contains(&x) || !(0..ch as i32).contains(&y);
                    b = (b.0.min(x), b.1.max(x + 1), b.2.min(y), b.3.max(y + 1));
                }
                assert!(b.0 < b.1, "{icon:?} draws");
                (b, outside)
            };
            for bold in [false, true] {
                for icon in BandIcon::ALL.into_iter().filter(|i| i.is_glass_mark()) {
                    let at = format!("{face:?} {px} px ({cw}x{ch}) bold {bold} {icon:?}");
                    let (alone, out) = ink(&mut r, rows[0], icon, bold);
                    assert!(
                        !out,
                        "{at}: between blanks it inks outside its cell {alone:?}"
                    );
                    for around in &rows[1..] {
                        let (beside, out) = ink(&mut r, *around, icon, bold);
                        assert!(!out, "{at}: beside {around:?} it inks outside its cell");
                        assert_eq!(
                            beside, alone,
                            "{at}: beside {around:?} its ink box moved from the one between blanks"
                        );
                    }
                }
                let (alone, spilled) = ink(&mut r, rows[0], BandIcon::Info, bold);
                let (beside, kept) = ink(&mut r, rows[3], BandIcon::Info, bold);
                assert!(
                    spilled && !kept,
                    "{face:?} {px} px: control, ℹ spills only between blanks"
                );
                assert_ne!(alone, beside, "{face:?} {px} px: control, ℹ's two sizes");
            }
        }
    }
}

#[test]
fn a_bold_glass_mark_is_its_regular_ones_size() {
    let faces: &[Option<&str>] = if cfg!(target_os = "macos") {
        &[None, Some("Menlo")]
    } else {
        &[None]
    };
    let measure = |r: &mut Renderer, icon: BandIcon, style| {
        let key = band_icon_key(icon, style, false, r.px_q);
        let GlyphImage::Mono { width, bytes, .. } = r.glyph_image(key) else {
            panic!("{icon:?}: a Mono raster");
        };
        let w = *width;
        let (mut b, mut ink) = ((usize::MAX, 0, usize::MAX, 0), 0u32);
        for (i, &c) in bytes.iter().enumerate().filter(|&(_, &c)| c > 0) {
            let (x, y) = (i % w, i / w);
            b = (b.0.min(x), b.1.max(x + 1), b.2.min(y), b.3.max(y + 1));
            ink += u32::from(c);
        }
        assert!(b.0 < b.1, "{icon:?} draws");
        (b, ink)
    };
    for &face in faces {
        let (mut compared, mut stem_parted) = (0usize, Vec::new());
        for px in 10..=36 {
            let mut r = Renderer::from_system_with_family(face, px as f32, Theme::default())
                .expect(NO_FONT);
            if let Some(name) = face {
                assert!(
                    r.primary_source_path().is_some_and(|p| p.contains(name)),
                    "{name} is the primary face"
                );
            }
            r.debug_block_on_lazy_fallbacks();
            let (cw, ch) = r.cell_size();
            let (mr, mb) = (r.icon_metrics(false), r.icon_metrics(true));
            if (mr.cap, mr.disc, mr.head, mr.baseline) != (mb.cap, mb.disc, mb.head, mb.baseline) {
                continue;
            }
            for icon in BandIcon::ALL.into_iter().filter(|i| i.is_glass_mark()) {
                let (regular, regular_ink) = measure(&mut r, icon, StyleBits::REGULAR);
                let (bold, bold_ink) = measure(&mut r, icon, StyleBits::BOLD);
                if icon == BandIcon::ToBar {
                    if bold != regular {
                        stem_parted.push(px);
                    }
                    continue;
                }
                let at = format!(
                    "{face:?} {px} px ({cw}x{ch}) {icon:?}, stems {:.2} and {:.2}",
                    mr.stem, mb.stem
                );
                assert_eq!(
                    bold, regular,
                    "{at}: the bold ink box (x0, x1, y0, y1) is not the regular one's"
                );
                assert!(
                    bold_ink >= regular_ink,
                    "{at}: bold ink {bold_ink} under the regular {regular_ink}"
                );
                compared += 1;
            }
        }
        assert!(
            compared > 0,
            "{face:?}: no size from 10 to 36 px measures its weights alike, so no bold mark \
             was compared"
        );
        if cfg!(target_os = "macos") {
            assert!(
                !stem_parted.is_empty(),
                "{face:?}: control, ⤓ keeps its stroke's centre and parts its weights"
            );
        }
    }
}

#[test]
fn no_icon_inks_over_the_tallest_letter_on_a_real_face() {
    let faces: &[Option<&str>] = if cfg!(target_os = "macos") {
        &[
            None,
            Some("Menlo"),
            Some("Courier New"),
            Some("Andale Mono"),
            Some("Monaco"),
        ]
    } else {
        &[None]
    };
    let (mut measured, mut flush, mut absent) = (0usize, 0usize, Vec::new());
    for &face in faces {
        for px in 8..=16 {
            let mut r = Renderer::from_system_with_family(face, px as f32, Theme::default())
                .expect(NO_FONT);
            if let Some(name) = face
                && !r.primary_source_path().is_some_and(|p| p.contains(name))
            {
                absent.push(name);
                break;
            }
            r.debug_block_on_lazy_fallbacks();
            let (cw, ch) = r.cell_size();
            for (bold, style) in [(false, StyleBits::REGULAR), (true, StyleBits::BOLD)] {
                let ceiling = r.icon_metrics(bold).head.max(1.0) as usize;
                for icon in BandIcon::ALL {
                    for spill in [false, true] {
                        let key = band_icon_key(icon, style, spill, r.px_q);
                        let GlyphImage::Mono { width, bytes, .. } = r.glyph_image(key) else {
                            panic!("{icon:?}: a Mono raster");
                        };
                        let y0 = bytes
                            .chunks(*width)
                            .position(|row| row.iter().any(|&c| c > 0));
                        let y0 = y0.expect("the icon draws");
                        assert!(
                            y0 >= ceiling,
                            "{face:?} {px} px ({cw}x{ch}) bold {bold} spill {spill} {icon:?}: \
                             ink on row {y0}, over row {ceiling}"
                        );
                        measured += 1;
                        flush += usize::from(y0 == ceiling);
                    }
                }
            }
        }
    }
    assert_eq!(
        measured,
        (faces.len() - absent.len()) * 9 * 2 * BandIcon::ALL.len() * 2,
        "every face but {absent:?} at every size, weight and spill"
    );
    assert!(flush > 0, "control: no icon reaches its ceiling row");
    if !absent.is_empty() {
        aterm_spec::host::could_not_run(format!("{absent:?} not on this host: unmeasured"));
    }
}

#[test]
fn a_spilled_icon_repaints_with_its_neighbours() {
    let mut r = renderer();
    r.set_pad(8);
    let base = |left: char, right: char, bg2: [u8; 3], icon: Option<BandIcon>, row1: &str| {
        let mut term = Terminal::new(3, 10);
        term.process(format!("   Title\r\n{row1}").as_bytes());
        let mut input = term.cell_frame(3, 10);
        input.cells[0][0].ch = left;
        input.cells[0][1].ch = '\u{26a0}';
        input.cells[0][2].ch = right;
        for cell in &mut input.cells[0] {
            cell.fg = [0xf1, 0xfa, 0x8c];
            cell.bg = [0x30, 0x31, 0x35];
        }
        input.cells[0][2].bg = bg2;
        if let Some(icon) = icon {
            input.chrome_rasters.push(ChromeRaster {
                row: 0,
                ground: std::sync::Arc::from(Vec::new()),
                rail: std::sync::Arc::from(Vec::new()),
                rail_h: 0,
                clear_rail: false,
                own: Vec::new(),
                split: None,
                rings: Vec::new(),
                icons: vec![ChromeIcon { col: 1, icon }],
            });
        }
        input
    };
    let dark = [0x30, 0x31, 0x35];
    let frames = [
        base(' ', ' ', dark, Some(BandIcon::Warn), "abc"),
        base(' ', ' ', [0x50, 0x51, 0x55], Some(BandIcon::Warn), "abc"),
        base(' ', ' ', [0x50, 0x51, 0x55], Some(BandIcon::Warn), "abd"),
        base('x', ' ', dark, Some(BandIcon::Warn), "abd"),
        base(' ', ' ', dark, Some(BandIcon::Warn), "abd"),
        base(' ', ' ', dark, Some(BandIcon::Info), "abd"),
        base(' ', ' ', dark, None, "abd"),
        base(' ', ' ', dark, Some(BandIcon::Warn), "abd"),
    ];
    let mut wc = WindowCpu::default();
    for (i, input) in frames.iter().enumerate() {
        let fresh = r.render_input(input);
        assert_eq!(
            r.render_input_cached(&mut wc, input).pixels(),
            fresh.pixels,
            "frame {i}: the damaged path matches a fresh render"
        );
    }
}

#[test]
fn an_outlined_capsule_rings_over_the_ground() {
    let mut r = renderer();
    const P: usize = 8;
    const FILL: u32 = 0x0050_FA7B;
    const RING: u32 = 0x00BD_93F9;
    const INNER: u32 = 0x0030_3135;
    let (cw, ch) = r.cell_size();
    let mut term = Terminal::new(3, 12);
    term.process(b"            ");
    let mut input = term.cell_frame(3, 12);
    for (c, cell) in input.cells[0].iter_mut().enumerate() {
        cell.bg = if (4..10).contains(&c) {
            [0x30, 0x31, 0x35]
        } else {
            [0x50, 0xfa, 0x7b]
        };
    }
    r.set_pad(P);
    let w = 12 * cw + 2 * P;
    let ring = ChromeRing {
        start: 4,
        end: 10,
        ring: RING,
        inner: INNER,
        seam: None,
    };
    assert_eq!(chrome_ring_floor_cols(&ring), 5..9);
    input.chrome_rasters.push(ChromeRaster {
        row: 0,
        ground: (0..w).map(|_| FILL).collect(),
        rail: std::sync::Arc::from(Vec::new()),
        rail_h: 0,
        clear_rail: false,
        own: Vec::new(),
        split: None,
        rings: vec![ring],
        icons: Vec::new(),
    });
    let (fy0, fy1) = r.chrome_ring_floor();
    let by0 = (ch - fy1).min(ch / 8);
    let top = r.grid_top();
    let mid = P + 7 * cw;
    let open = r.render_input(&input);
    let px = |f: &Frame, x: usize, y: usize| f.pixels[y * f.width + x] & 0x00ff_ffff;
    assert_eq!(
        px(&open, mid, top + fy1 - 1),
        INNER,
        "the floor is the cells'"
    );
    for cell in &mut input.cells[0][5..9] {
        cell.underline = UnderlineStyle::Single;
        cell.underline_color = Some([0xBD, 0x93, 0xF9]);
    }
    let f = r.render_input(&input);
    if by0 > 0 {
        assert_eq!(
            px(&f, mid, top + by0 - 1),
            FILL,
            "the ground runs on above the ring"
        );
    }
    assert_eq!(px(&f, mid, top + by0), RING, "the ring's top line");
    for y in fy0..fy1 {
        assert_eq!(
            px(&f, mid, top + y),
            RING,
            "the ring's floor on the underline ({y})"
        );
    }
    assert_eq!(px(&f, mid, top + fy1), FILL, "nothing under the floor");
    assert_eq!(
        px(&f, mid, top + ch / 2),
        INNER,
        "the capsule's ground inside"
    );
    assert_eq!(
        px(&f, P + 4 * cw, top + ch / 2),
        FILL,
        "the ground beside its end"
    );
    let blended = (P + 4 * cw..P + 5 * cw)
        .flat_map(|x| (top..top + ch).map(move |y| (x, y)))
        .any(|(x, y)| ![FILL, RING, INNER].contains(&px(&f, x, y)));
    assert!(blended, "the rounded end is antialiased");
    let (lx0, lx1) = (P + 5 * cw, P + 9 * cw);
    for (x0, x1, ya, yb, color) in
        chrome_ring_runs(&input.chrome_rasters[0], f.width, P, cw, ch, (fy0, fy1))
    {
        for y in ya..yb {
            for x in x0..x1 {
                let want = if (fy0..fy1).contains(&y) && (lx0..lx1).contains(&x) {
                    RING
                } else {
                    color
                };
                assert_eq!(px(&f, x, top + y), want, "({x},{y})");
            }
        }
    }
}

#[test]
fn a_ringed_primary_stands_on_the_seam_and_its_descenders_carve_its_floor() {
    const P: usize = 8;
    const TRACK: u32 = 0x004D_4E51;
    const SEAM: u32 = 0x00A0_A2A8;
    const RING: u32 = 0x0050_FA7B;
    const INNER: u32 = 0x0030_3135;
    const LABEL: u32 = 0x00F1_FA8C;
    let rgb = |c: u32| {
        let [_, r, g, b] = c.to_be_bytes();
        [r, g, b]
    };
    for size in [16.0f32, 24.0] {
        let mut r = Renderer::from_system(size, Theme::default()).expect(NO_FONT);
        r.set_pad(P);
        let (cw, ch) = r.cell_size();
        let cols = 19usize;
        let mut term = Terminal::new(2, cols as u16);
        term.process(b"Paste   Stop paste ");
        let mut input = term.cell_frame(2, cols);
        input.cells[0].resize(cols, RenderCell::default());
        input.cursor_visible = false;
        let ring = ChromeRing {
            start: 7,
            end: 19,
            ring: RING,
            inner: INNER,
            seam: Some(SEAM),
        };
        let floor_cols = chrome_ring_floor_cols(&ring);
        assert_eq!(floor_cols, 8..18);
        for (c, cell) in input.cells[0].iter_mut().enumerate() {
            let ringed = (7..19).contains(&c);
            cell.fg = rgb(LABEL);
            cell.bg = rgb(if ringed { INNER } else { TRACK });
            if !(c == 7 || c == 18) {
                cell.underline = UnderlineStyle::Single;
                cell.underline_color = Some(rgb(if floor_cols.contains(&c) { RING } else { SEAM }));
            }
        }
        let w = cols * cw + 2 * P;
        input.chrome_rasters.push(ChromeRaster {
            row: 0,
            ground: (0..w).map(|_| TRACK).collect(),
            rail: std::sync::Arc::from(Vec::new()),
            rail_h: 0,
            clear_rail: false,
            own: vec![(7, 19)],
            split: None,
            rings: vec![ring],
            icons: Vec::new(),
        });
        let f = r.render_input(&input);
        let top = r.grid_top();
        let px = |x: usize, y: usize| f.pixels[(top + y) * f.width + x] & 0x00ff_ffff;
        let (fy0, fy1) = r.chrome_ring_floor();
        for y in 0..ch {
            assert_eq!(
                px(P + cw * 6 + cw / 2, y) == SEAM,
                (fy0..fy1).contains(&y),
                "{size}: the seam's rows ({y})"
            );
        }
        for c in [7usize, 18] {
            let px_row: Vec<u32> = (fy0..fy1)
                .flat_map(|y| (P + c * cw..P + (c + 1) * cw).map(move |x| (x, y)))
                .map(|(x, y)| px(x, y))
                .collect();
            assert!(!px_row.contains(&TRACK), "{size}: no gap in end cell {c}");
            assert!(
                px_row.contains(&SEAM),
                "{size}: the seam runs into cell {c}"
            );
            assert!(
                px_row.iter().any(|&p| p != SEAM && p != RING),
                "{size}: the seam meets the foot antialiased in cell {c}"
            );
        }
        for y in 0..ch {
            let (x0, x1) = if (fy0..fy1).contains(&y) {
                (P + 8 * cw, P + 18 * cw)
            } else {
                (P + 7 * cw, P + 19 * cw)
            };
            for x in x0..x1 {
                assert_ne!(px(x, y), SEAM, "{size}: seam inside the pill at ({x},{y})");
            }
        }
        let floor_px = |c: usize| {
            (fy0..fy1)
                .flat_map(|y| (P + c * cw..P + (c + 1) * cw).map(move |x| (x, y)))
                .map(|(x, y)| px(x, y))
                .collect::<Vec<u32>>()
        };
        assert!(
            floor_px(10).iter().all(|&c| c == RING),
            "{size}: the floor under `o`"
        );
        let reaches =
            |c: usize| (fy1..ch).any(|y| (P + c * cw..P + (c + 1) * cw).any(|x| px(x, y) != TRACK));
        for c in [11usize, 13] {
            let under = floor_px(c);
            if !reaches(c) {
                eprintln!("{size}: the face's `p` stops above the floor; nothing to carve");
                continue;
            }
            assert!(
                under.contains(&INNER),
                "{size}: the floor is carved round the `p` in cell {c}: {under:x?}"
            );
            assert!(
                under.iter().any(|&c| c != RING && c != INNER),
                "{size}: the descender's ink crosses the floor in cell {c}"
            );
        }
    }
}

#[test]
fn the_ring_builders_fast_path_matches_supersampling_every_pixel() {
    fn reference(
        m: &ChromeRaster,
        w: usize,
        pad: usize,
        cw: usize,
        ch: usize,
        floor: (usize, usize),
    ) -> Vec<Option<u32>> {
        let lin = |v: u32| {
            let c = v as f32 / 255.0;
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let enc = |l: f32| {
            let l = l.clamp(0.0, 1.0);
            let c = if l <= 0.003_130_8 {
                12.92 * l
            } else {
                1.055f32.mul_add(l.powf(1.0 / 2.4), -0.055)
            };
            (c * 255.0).round().clamp(0.0, 255.0) as u32
        };
        let split = |c: u32| [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff].map(lin);
        let mut out = vec![None; w * ch];
        let by1 = floor.1.clamp(1, ch);
        let (fy0, fy1) = (floor.0.min(by1), by1);
        let t = (fy1 - fy0).clamp(1, (ch / 6).max(1)) as f32;
        let by0 = (ch - by1).min(ch / 8);
        for ring in &m.rings {
            let (start, end) = (usize::from(ring.start), usize::from(ring.end));
            let bx0 = pad + start * cw + cw / 4;
            let bx1 = (pad + end * cw).saturating_sub(cw / 4).min(w);
            let (sx0, sx1) = (pad + start * cw, (pad + end * cw).min(w));
            let cols = chrome_ring_floor_cols(ring);
            let (lx0, lx1) = (pad + cols.start * cw, pad + cols.end * cw);
            let (cx, cy) = ((bx0 + bx1) as f32 * 0.5, (by0 + by1) as f32 * 0.5);
            let (hw, hh) = ((bx1 - bx0) as f32 * 0.5, (by1 - by0) as f32 * 0.5);
            let rad = hh.min(hw);
            let sd = |px: f32, py: f32| {
                let qx = (px - cx).abs() - (hw - rad);
                let qy = (py - cy).abs() - (hh - rad);
                qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - rad
            };
            for y in by0..by1 {
                for x in sx0..sx1 {
                    let floored = (fy0..fy1).contains(&y) && (lx0..lx1).contains(&x);
                    let seamed = ring.seam.filter(|_| (fy0..fy1).contains(&y) && !floored);
                    let (mut o, mut rg, mut i) = (0u32, 0u32, 0u32);
                    for sy in 0..4 {
                        for sx in 0..4 {
                            let d = sd(
                                x as f32 + (sx as f32 + 0.5) / 4.0,
                                y as f32 + (sy as f32 + 0.5) / 4.0,
                            );
                            if d > 0.0 {
                                o += 1;
                            } else if d > -t && !floored {
                                rg += 1;
                            } else {
                                i += 1;
                            }
                        }
                    }
                    out[y * w + x] = match (o, rg, i) {
                        (16, _, _) => seamed,
                        (_, 16, _) => Some(ring.ring),
                        (_, _, 16) => Some(ring.inner),
                        _ => {
                            let (a, b, c) = (
                                split(seamed.unwrap_or(m.ground[x])),
                                split(ring.ring),
                                split(ring.inner),
                            );
                            let k = |n: usize| {
                                enc((a[n] * o as f32 + b[n] * rg as f32 + c[n] * i as f32) / 16.0)
                            };
                            Some((k(0) << 16) | (k(1) << 8) | k(2))
                        }
                    };
                }
            }
        }
        out
    }
    for (cw, ch, floor) in [
        (12usize, 24usize, (21usize, 22usize)),
        (8, 17, (14, 15)),
        (20, 40, (34, 37)),
        (10, 20, (17, 19)),
    ] {
        let pad = 6;
        let w = 16 * cw + 2 * pad;
        let m = ChromeRaster {
            row: 0,
            ground: (0..w)
                .map(|x| {
                    let g = u32::try_from(x * 255 / w).unwrap();
                    (0x50 << 16) | (g << 8) | (255 - g)
                })
                .collect(),
            rail: std::sync::Arc::from(Vec::new()),
            rail_h: 0,
            clear_rail: false,
            own: Vec::new(),
            split: None,
            rings: vec![
                ChromeRing {
                    start: 1,
                    end: 9,
                    ring: 0x00BD_93F9,
                    inner: 0x0030_3135,
                    seam: (cw != 8).then_some(0x00A0_A2A8),
                },
                ChromeRing {
                    start: 10,
                    end: 12,
                    ring: 0x00FF_B86C,
                    inner: 0x0028_2A36,
                    seam: None,
                },
                ChromeRing {
                    start: 13,
                    end: 15,
                    ring: 0x00FF_B86C,
                    inner: 0x0028_2A36,
                    seam: Some(0x0066_6666),
                },
            ],
            icons: Vec::new(),
        };
        let mut got = vec![None; w * ch];
        for (x0, x1, ya, yb, c) in chrome_ring_runs(&m, w, pad, cw, ch, floor) {
            for y in ya..yb {
                for x in x0..x1 {
                    assert!(
                        got[y * w + x].is_none(),
                        "{cw}x{ch}: ({x},{y}) painted twice"
                    );
                    got[y * w + x] = Some(c);
                }
            }
        }
        let want = reference(&m, w, pad, cw, ch, floor);
        for (i, (g, e)) in got.iter().zip(&want).enumerate() {
            assert_eq!(
                g,
                e,
                "{cw}x{ch} floor {floor:?}: pixel ({}, {})",
                i % w,
                i / w
            );
        }
    }
}

#[test]
fn the_seams_ink_skip_probes_a_drawn_icons_ink() {
    let mut r = Renderer::from_bytes(BUNDLED_MONO, 16.0, Theme::default())
        .expect("the bundled face builds a renderer");
    let (cw, ch) = r.cell_size();
    let baseline = r.baseline();
    for (line, c) in [("g ", 0usize), (" g ", 1)] {
        for icon in BandIcon::ALL {
            let mut term = Terminal::new(1, 3);
            term.process(line.as_bytes());
            let mut input = term.cell_frame(1, 3);
            input.cells[0].resize(3, RenderCell::default());
            if c == 0 {
                input.cells[0][1].ch = 'x';
            }
            for cell in &mut input.cells[0] {
                cell.underline = UnderlineStyle::Single;
            }
            let (mut ink, mut spans) = (Vec::new(), Vec::new());
            let mut probe = |r: &mut Renderer, input: &RenderInput, c: usize| {
                r.underline_keep_spans_into(
                    input,
                    0,
                    c,
                    ColumnGlyph::PerCell,
                    c * cw,
                    cw,
                    &mut ink,
                    &mut spans,
                )
            };
            assert!(
                probe(&mut r, &input, c),
                "the font's `g` carves the seam before any icon draws"
            );
            input.chrome_rasters.push(ChromeRaster {
                row: 0,
                ground: std::sync::Arc::from(Vec::new()),
                rail: std::sync::Arc::from(Vec::new()),
                rail_h: 0,
                clear_rail: false,
                own: Vec::new(),
                split: None,
                rings: Vec::new(),
                icons: vec![ChromeIcon {
                    col: c as u16,
                    icon,
                }],
            });
            let key = r
                .chrome_icon_key(input.chrome_raster(0), &input.cells[0], c)
                .expect("an icon cell");
            assert_eq!(
                key.ch_or_id & CHROME_ICON_SPILL_BIT != 0,
                c == 1 && !icon.is_glass_mark(),
                "{icon:?}: a badge spilled only between blanks, a mark never"
            );
            let img = r.glyph_image(key);
            let (gw, gh, ymin) = (img.width(), img.height(), img.ymin());
            let gy0 = baseline - gh as i32 - ymin;
            let lowest = (0..gh)
                .rev()
                .find(|&gy| {
                    (0..gw).any(|gx| match img {
                        GlyphImage::Mono { bytes, .. } => bytes[gy * gw + gx] > 0,
                        GlyphImage::Rgba { bytes, .. } => bytes[(gy * gw + gx) * 4 + 3] > 0,
                    })
                })
                .map(|gy| gy0 + gy as i32);
            assert!(
                lowest.is_some_and(|y| y <= baseline),
                "{icon:?} ({cw}x{ch}, col {c}): the icon stays above the underline ({lowest:?})"
            );
            assert!(
                !probe(&mut r, &input, c),
                "{icon:?} ({cw}x{ch}, col {c}): the seam under a drawn icon is not carved"
            );
        }
    }
}

#[test]
fn chrome_raster_visitor_matches_per_pixel_ownership_and_rail_oracle() {
    let color = |x: usize| match x % 9 {
        0..=2 => 0x0012_3456,
        3 => ChromeRaster::KEEP,
        4..=6 => 0x00ab_cdef,
        _ => x as u32,
    };
    let mut raster = ChromeRaster {
        row: 0,
        ground: (0..37).map(color).collect(),
        rail: (0..23).map(|x| color(x + 3)).collect(),
        rail_h: 3,
        clear_rail: false,
        own: Vec::new(),
        split: None,
        rings: Vec::new(),
        icons: Vec::new(),
    };
    let ownership = [
        vec![],
        vec![(1, 3)],
        vec![(3, 6), (1, 2), (2, 5)],
        vec![(0, 20)],
        vec![(4, 2)],
    ];
    for own in ownership {
        raster.own = own;
        for rail_h in [0, 3] {
            raster.rail_h = rail_h;
            for w in [0, 1, 7, 23, 37, 43] {
                for pad in [0, 2, 11] {
                    for cell_w in [0, 1, 3, 7] {
                        for cols in [0, 1, 4, 8] {
                            let mut ground = vec![None; w];
                            let mut rail = vec![None; w];
                            let mut last: Option<(usize, u32, bool)> = None;
                            visit_chrome_raster_runs(
                                &raster,
                                w,
                                pad,
                                cell_w,
                                cols,
                                |x0, x1, c, is_rail| {
                                    assert!(x0 < x1 && x1 <= w);
                                    if let Some((end, previous_c, was_rail)) = last {
                                        assert!(!was_rail || is_rail, "ground precedes rail");
                                        if was_rail == is_rail {
                                            assert!(x0 >= end, "ascending disjoint runs");
                                            assert!(x0 != end || c != previous_c, "maximal run");
                                        }
                                    }
                                    last = Some((x1, c, is_rail));
                                    let pixels = if is_rail { &mut rail } else { &mut ground };
                                    for pixel in &mut pixels[x0..x1] {
                                        assert!(pixel.replace(c).is_none(), "painted only once");
                                    }
                                },
                            );
                            let advance = cell_w.max(1);
                            for x in 0..w {
                                let owned = x < pad + cols * advance
                                    && raster.own.iter().any(|&(a, b)| {
                                        x >= pad + usize::from(a) * advance
                                            && x < pad + usize::from(b) * advance
                                    });
                                let expected_ground =
                                    raster.ground.get(x).copied().filter(|_| !owned);
                                let expected_rail =
                                    raster.rail.get(x).copied().filter(|&c| {
                                        rail_h > 0 && !owned && c != ChromeRaster::KEEP
                                    });
                                assert_eq!(ground[x], expected_ground, "ground pixel {x}");
                                assert_eq!(rail[x], expected_rail, "rail pixel {x}");
                            }
                        }
                    }
                }
            }
        }
    }
    raster.ground = std::sync::Arc::from([]);
    raster.rail = std::sync::Arc::from([]);
    visit_chrome_raster_runs(&raster, 100, 3, 8, 10, |_, _, _, _| {
        panic!("empty source arrays emit no runs");
    });
}

#[test]
fn a_chrome_raster_paints_its_ground_rail_and_split_to_the_pixel() {
    let mut r = renderer();
    const P: usize = 8;
    const FILL: u32 = 0x0050_FA7B;
    const TRACK: u32 = 0x0044_4750;
    const RAIL: u32 = 0x00F1_FA8C;
    const OWN: u32 = 0x0012_3456;
    const LEFT_INK: u32 = 0x0000_0000;
    let (cw, ch) = r.cell_size();
    let (rows, cols) = (3usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"MMMMMMMMMMMM");
    let mut input = term.cell_frame(rows, cols);
    for (c, cell) in input.cells[0].iter_mut().enumerate() {
        cell.fg = [255, 255, 255];
        cell.bg = if c == 9 {
            [0x12, 0x34, 0x56]
        } else {
            [0x44, 0x47, 0x50]
        };
    }
    r.set_pad(P);
    let base = r.render_input(&input);
    let w = base.width;
    let top = r.grid_top();
    let edge = P + 4 * cw + cw / 2;
    let ground: std::sync::Arc<[u32]> = (0..w)
        .map(|x| {
            if x < edge {
                FILL
            } else if x == edge {
                0x004A_A066
            } else {
                TRACK
            }
        })
        .collect();
    let rail: std::sync::Arc<[u32]> = (0..w)
        .map(|x| if x < edge { RAIL } else { ChromeRaster::KEEP })
        .collect();
    input.chrome_rasters.push(ChromeRaster {
        row: 0,
        ground,
        rail,
        rail_h: 3,
        clear_rail: false,
        own: vec![(9, 10)],
        split: Some(InkSplit {
            col: 4,
            x: u32::try_from(edge).unwrap(),
            ink: LEFT_INK,
            bg: FILL,
        }),
        rings: Vec::new(),
        icons: Vec::new(),
    });
    let lit = r.render_input(&input);
    let at = |x: usize, y: usize| lit.pixels[y * w + x] & 0x00ff_ffff;
    for y in top..top + ch - 3 {
        assert_eq!(at(0, y), FILL, "the left gutter is the fill");
        assert_eq!(at(w - 1, y), TRACK, "the right gutter is the track");
    }
    assert_eq!(at(edge - 1, top), FILL);
    assert_eq!(at(edge, top), 0x004A_A066, "the one antialiased pixel");
    assert_eq!(at(edge + 1, top), TRACK);
    assert_eq!(at(P + 9 * cw + 1, top), OWN, "a chip keeps its own fill");
    for y in top + ch - 3..top + ch {
        assert_eq!(at(1, y), RAIL, "the rail from the window's left edge");
        assert_eq!(at(edge + 2, y), TRACK, "no rail past the level");
    }
    let (x0, x1) = (P + 4 * cw, P + 5 * cw);
    let lum = |px: u32| ((px >> 16) & 0xff) + ((px >> 8) & 0xff) + (px & 0xff);
    let dark_left = (x0..edge).any(|x| (top..top + ch - 3).any(|y| lum(at(x, y)) < 200));
    let light_right = (edge + 1..x1).any(|x| (top..top + ch - 3).any(|y| lum(at(x, y)) > 600));
    assert!(dark_left, "the fill side's ink left of the split");
    assert!(light_right, "the cell's own ink right of it");
    assert!(
        (edge + 1..x1).all(|x| (top..top + ch - 3).all(|y| lum(at(x, y)) >= lum(TRACK))),
        "no dark ink right of the split"
    );
    let mut wc = WindowCpu::default();
    let mut plain = input.clone();
    plain.chrome_rasters.clear();
    let _ = r.render_input_cached(&mut wc, &plain);
    assert_eq!(r.render_input_cached(&mut wc, &input).pixels(), lit.pixels);
    let mut moved = input.clone();
    let shifted: std::sync::Arc<[u32]> = (0..w)
        .map(|x| if x < edge + 5 { FILL } else { TRACK })
        .collect();
    moved.chrome_rasters[0].ground = shifted;
    let fresh = r.render_input(&moved);
    assert_eq!(
        r.render_input_cached(&mut wc, &moved).pixels(),
        fresh.pixels,
        "a raster-only change repaints its row"
    );
    assert_eq!(r.render_input(&plain).pixels, base.pixels);
}

#[test]
fn a_clear_rail_row_keeps_its_descenders_off_the_rail() {
    let fitted = [16.0f32, 24.0, 32.0]
        .into_iter()
        .filter(|&font_px| clear_rail_at(font_px))
        .count();
    assert!(fitted > 0, "no size of this face has room for a clear rail");
}

fn clear_rail_at(font_px: f32) -> bool {
    let mut r = Renderer::from_system(font_px, Theme::default()).expect(NO_FONT);
    const P: usize = 8;
    const TRACK: u32 = 0x0030_3135;
    const RAIL: u32 = 0x00F2_FD4A;
    const SEAM: [u8; 3] = [0x9e, 0x9e, 0xa1];
    let (cw, ch) = r.cell_size();
    let (rows, cols) = (3usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"gypq (jgy)'");
    let mut input = term.cell_frame(rows, cols);
    input.cursor_visible = false;
    for cell in &mut input.cells[0] {
        cell.fg = [0xd0, 0xd0, 0xd0];
        cell.bg = [0x30, 0x31, 0x35];
    }
    r.set_pad(P);
    let plain = r.render_input(&input);
    let w = plain.width;
    let top = r.grid_top();
    let rail_px = u16::try_from(ch.div_ceil(8).max(2)).unwrap();
    let raster = |clear_rail: bool, lit: bool| ChromeRaster {
        row: 0,
        ground: std::sync::Arc::from(Vec::new()),
        rail: (0..w)
            .map(|_| if lit { RAIL } else { ChromeRaster::KEEP })
            .collect(),
        rail_h: rail_px,
        clear_rail,
        own: Vec::new(),
        split: None,
        rings: Vec::new(),
        icons: Vec::new(),
    };
    let grid = P..P + input.cells[0].len().min(cols) * cw;
    let px = |f: &Frame, x: usize, y: usize| f.pixels[y * w + x] & 0x00ff_ffff;
    let inked = |f: &Frame, y: usize| grid.clone().any(|x| px(f, x, y) != TRACK);
    let mut lifted_in = input.clone();
    lifted_in.chrome_rasters.push(raster(true, true));
    let room = r.chrome_room_for(&lifted_in);
    if room.0 + room.1 < 2 {
        assert!(
            !band::seam_can_rail(ch, Some(room)),
            "{font_px}: {room:?} cannot pay for a rail, so the band lays none"
        );
        return false;
    }
    let (lift, rail) = chrome_fit(&lifted_in.chrome_rasters[0], room);
    assert!(
        (1..=usize::from(rail_px)).contains(&rail) && lift <= room.0,
        "{font_px}: {room:?} → lift {lift}, rail {rail}"
    );
    assert_eq!(
        r.chrome_room_for(&input),
        (0, 0),
        "no clear-rail row, no room asked"
    );
    let mut unlifted = input.clone();
    unlifted.chrome_rasters.push(raster(false, false));
    let unlifted = r.render_input(&unlifted);
    assert_eq!(
        unlifted.pixels, plain.pixels,
        "an unlit, unlifted raster is no raster"
    );
    assert!(
        (top..top + room.0).all(|y| !inked(&unlifted, y)),
        "the head room is free of ink"
    );
    if room.1 == 0 {
        assert!(
            inked(&unlifted, top + ch - 1),
            "descenders reach the lowest row"
        );
    }
    let lifted = r.render_input(&lifted_in);
    for y in top..top + ch - rail - 1 {
        for x in grid.clone() {
            assert_eq!(
                px(&lifted, x, y),
                px(&unlifted, x, y + lift),
                "({x},{y}): the lifted ink is the row's own, moved up"
            );
        }
    }
    assert!(
        !inked(&lifted, top + ch - rail - 1),
        "{font_px}: one clear row above the rail"
    );
    for y in top + ch - rail..top + ch {
        for x in 0..w {
            assert_eq!(px(&lifted, x, y), RAIL, "({x},{y}): the rail, no ink on it");
        }
    }
    let deco = r.deco_metrics();
    let t = deco.underline_t.max(1);
    let uy = top + deco.underline_y.min(ch - t);
    let mut bare = input.clone();
    bare.chrome_rasters.push(raster(true, false));
    let bare = r.render_input(&bare);
    let mut seamed = input.clone();
    for cell in &mut seamed.cells[0] {
        cell.underline = UnderlineStyle::Single;
        cell.underline_color = Some(SEAM);
    }
    seamed.chrome_rasters.push(raster(true, false));
    let seamed = r.render_input(&seamed);
    let seam = (u32::from(SEAM[0]) << 16) | (u32::from(SEAM[1]) << 8) | u32::from(SEAM[2]);
    let ink_near = |x: usize| {
        (x - 1..=x + 1)
            .any(|xx| (uy - 1..(uy + t + 1).min(top + ch)).any(|yy| px(&bare, xx, yy) != TRACK))
    };
    let mut breaks = 0;
    for x in grid.clone() {
        if px(&seamed, x, uy) != seam {
            breaks += 1;
            assert!(
                ink_near(x),
                "{font_px}: the seam breaks at {x} with no ink near it"
            );
        }
    }
    assert!(
        breaks < grid.len(),
        "{font_px}: the seam is drawn ({breaks} breaks)"
    );
    let mut wc = WindowCpu::default();
    let _ = r.render_input_cached(&mut wc, &input);
    assert_eq!(
        r.render_input_cached(&mut wc, &lifted_in).pixels(),
        lifted.pixels
    );
    assert_eq!(
        r.render_input_cached(&mut wc, &input).pixels(),
        plain.pixels
    );
    true
}

#[test]
fn a_scroll_under_a_chrome_raster_keeps_the_blit_and_matches_a_fresh_frame() {
    let mut r = renderer();
    const P: usize = 8;
    r.set_pad(P);
    let (rows, cols) = (8usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    for i in 0..80 {
        term.process(format!("line {i} gyp tail\r\n").as_bytes());
    }
    let probe = r.render_input(&term.cell_frame(rows, cols));
    let w = probe.width;
    let raster = |row: u16, level: usize| ChromeRaster {
        row,
        ground: (0..w)
            .map(|x| if x < level { 0x0050_FA7B } else { 0x0044_4750 })
            .collect(),
        rail: (0..w)
            .map(|x| {
                if x < level {
                    0x00F1_FA8C
                } else {
                    ChromeRaster::KEEP
                }
            })
            .collect(),
        rail_h: 3,
        clear_rail: false,
        own: Vec::new(),
        split: None,
        rings: Vec::new(),
        icons: Vec::new(),
    };
    let frame = |term: &mut Terminal| {
        let mut input = term.cell_frame(rows, cols);
        for cell in &mut input.cells[0] {
            cell.ch = 'M';
            cell.fg = [255, 255, 255];
            cell.bg = [0x44, 0x47, 0x50];
        }
        input.chrome_rasters.push(raster(0, w / 3));
        input.chrome_rasters.push(raster(3, w / 2));
        input
    };
    let mut wc = WindowCpu::default();
    let _ = r.render_input_cached(&mut wc, &frame(&mut term));
    let mut blits = 0;
    for (step, notch) in [3i32, 3, -2, 1, -3, 2].into_iter().enumerate() {
        term.scroll_display(notch);
        let input = frame(&mut term);
        let warm = r.render_input_cached(&mut wc, &input).pixels().to_vec();
        let mut fresh = renderer();
        fresh.set_pad(P);
        assert_eq!(
            warm,
            fresh.render_input(&input).pixels,
            "scroll-blit under a raster != fresh frame @ step {step} ({notch})"
        );
        if matches!(wc.last_damage(), DamageOutcome::Scroll { .. }) {
            blits += 1;
        }
    }
    assert!(blits >= 5, "the blit carries the scroll ({blits}/6)");
}

#[test]
fn a_chrome_bleed_carries_the_band_and_its_seam_into_the_padding() {
    let mut r = renderer();
    const P: usize = 8;
    const H: usize = 6;
    const BAND: u32 = 0x0030_3135;
    const SEAM: u32 = 0x0060_6164;
    let (_, ch) = r.cell_size();
    let (rows, cols) = (3usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"chrome band");
    let input = term.cell_frame(rows, cols);
    r.set_pad(P);
    r.set_head(H);
    let base = r.render_input(&input);
    let bg = Theme::default().bg;
    let top = r.grid_top();
    let (w, hgt) = (base.width, base.height);

    assert_eq!(r.chrome_bleed(), None, "no bleed by default");
    r.set_chrome_bleed(Some(ChromeBleed {
        rows: 1,
        first: 0,
        color: BAND,
        seam: Some(SEAM),
        top_extends_cells: false,
        row_edges: [None; CHROME_ROW_EDGES],
    }));
    let bled = r.render_input(&input);
    assert_eq!(
        (bled.width, bled.height),
        (w, hgt),
        "a bleed is pixels only"
    );
    let at = |px: &[u32], x: usize, y: usize| px[y * w + x];

    for y in 0..top {
        for x in 0..w {
            assert_eq!(at(&bled.pixels, x, y), BAND, "top strip ({x},{y})");
            assert_eq!(at(&base.pixels, x, y), bg, "…and was bg before");
        }
    }
    let deco = r.deco_metrics();
    let t = deco.underline_t.max(1).min(ch);
    let seam_y0 = top + deco.underline_y.min(ch - t);
    for y in top..top + ch {
        let want = if (seam_y0..seam_y0 + t).contains(&y) {
            SEAM
        } else {
            BAND
        };
        for x in (0..P).chain(w - P..w) {
            assert_eq!(at(&bled.pixels, x, y), want, "row-0 gutter ({x},{y})");
            assert_eq!(at(&base.pixels, x, y), bg, "…and was bg before");
        }
    }
    for y in top..hgt {
        for x in P..w - P {
            assert_eq!(
                at(&bled.pixels, x, y),
                at(&base.pixels, x, y),
                "grid pixel ({x},{y}) must not move"
            );
        }
    }
    for y in top + ch..hgt {
        for x in (0..P).chain(w - P..w) {
            assert_eq!(at(&bled.pixels, x, y), bg, "row-1+ gutter ({x},{y})");
        }
    }

    let mut wc = WindowCpu::default();
    assert_eq!(r.render_input_cached(&mut wc, &input).pixels(), bled.pixels);
    let mut term2 = Terminal::new(rows as u16, cols as u16);
    term2.process(b"chrome band\r\nsecond");
    let moved = term2.cell_frame(rows, cols);
    let fresh = r.render_input(&moved);
    assert_eq!(
        r.render_input_cached(&mut wc, &moved).pixels(),
        fresh.pixels,
        "incremental frame must match a fresh one with a bleed in force"
    );

    r.set_chrome_bleed(Some(ChromeBleed {
        rows: 0,
        first: 0,
        color: BAND,
        seam: None,
        top_extends_cells: false,
        row_edges: [None; CHROME_ROW_EDGES],
    }));
    assert_eq!(r.chrome_bleed(), None, "a zero-row bleed is no bleed");
    assert_eq!(r.render_input(&input).pixels, base.pixels);
}

#[test]
fn top_extends_cells_continues_each_column_through_the_head_strip() {
    let mut r = renderer();
    const P: usize = 8;
    const H: usize = 10;
    const BAND: u32 = 0x0030_3135;
    const CHIP: [u8; 3] = [0x52, 0x52, 0x56];
    const CHIP_PX: u32 = 0x0052_5256;
    let (cw, _) = r.cell_size();
    let (rows, cols) = (3usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"chrome band");
    let input = {
        let mut input = term.cell_frame(rows, cols);
        for cell in input.cells[0].iter_mut().take(5).skip(1) {
            cell.bg = CHIP;
        }
        input
    };
    r.set_pad(P);
    r.set_head(H);
    let top = r.grid_top();
    assert!(
        top > 0,
        "the strip above the grid must exist to be extended"
    );

    let flat = {
        r.set_chrome_bleed(Some(ChromeBleed {
            rows: 1,
            first: 0,
            color: BAND,
            seam: None,
            top_extends_cells: false,
            row_edges: [None; CHROME_ROW_EDGES],
        }));
        r.render_input(&input)
    };
    let extended = {
        r.set_chrome_bleed(Some(ChromeBleed {
            rows: 1,
            first: 0,
            color: BAND,
            seam: None,
            top_extends_cells: true,
            row_edges: [None; CHROME_ROW_EDGES],
        }));
        r.render_input(&input)
    };
    let w = flat.width;
    assert_eq!((extended.width, extended.height), (w, flat.height));
    let at = |px: &[u32], x: usize, y: usize| px[y * w + x];

    let chip_x = P + cw * 2 + cw / 2;
    for y in 0..top {
        assert_eq!(at(&flat.pixels, chip_x, y), BAND, "flat strip at y={y}");
        assert_eq!(
            at(&extended.pixels, chip_x, y),
            CHIP_PX,
            "extended strip continues the chip column at y={y}"
        );
    }
    let plain_x = P + cw * 7 + cw / 2;
    assert_ne!(
        at(&extended.pixels, plain_x, 0),
        at(&extended.pixels, chip_x, 0),
        "the strip above the grid is per-column, not one flat tone"
    );
    assert_eq!(
        at(&flat.pixels, plain_x, 0),
        at(&flat.pixels, chip_x, 0),
        "…which is exactly what the flat fill could not express"
    );
    assert_eq!(
        &extended.pixels[0..w],
        &extended.pixels[(top - 1) * w..top * w],
        "every line of the extended strip is the same surface"
    );
    for x in [0usize, P - 1, w - 1] {
        assert_eq!(at(&extended.pixels, x, 0), BAND, "gutter x={x} stays band");
        assert_eq!(at(&extended.pixels, x, top - 1), BAND, "gutter x={x}");
    }
    assert_eq!(
        &extended.pixels[top * w..],
        &flat.pixels[top * w..],
        "everything from the grid down is byte-identical"
    );
}

#[test]
fn row_pixel_band_extends_both_grid_edges() {
    let mut r = renderer();
    const P: usize = 8;
    const H: usize = 24;
    r.set_pad(P);
    r.set_head(H);
    let (_, ch) = r.cell_size();
    let rows = 5usize;
    let h = rows * ch + 2 * P + H;
    let grid_top = P + H;
    assert_eq!(r.row_pixel_band(0, rows, h), (0, grid_top + ch));
    assert_eq!(
        r.row_pixel_band(2, rows, h),
        (grid_top + 2 * ch, grid_top + 3 * ch)
    );
    assert_eq!(
        r.row_pixel_band(rows - 1, rows, h),
        (grid_top + (rows - 1) * ch, h)
    );
    assert_eq!(r.row_pixel_band(0, 1, ch + 2 * P + H), (0, ch + 2 * P + H));
    r.set_pad(0);
    r.set_head(0);
    let hi = rows * ch;
    assert_eq!(r.row_pixel_band(0, rows, hi), (0, ch));
    assert_eq!(r.row_pixel_band(2, rows, hi), (2 * ch, 3 * ch));
    assert_eq!(
        r.row_pixel_band(rows - 1, rows, hi),
        ((rows - 1) * ch, rows * ch)
    );
    let (y0, y1) = r.row_pixel_band(rows + 3, rows + 4, hi);
    assert!(y0 >= y1, "past-frame row must be empty (got {y0}..{y1})");
}

#[cfg(not(windows))]
#[test]
fn vs15_text_presentation_is_one_cell_and_does_not_overpaint_adjacent_cell() {
    require_host_colour_emoji();
    let grin = '\u{1F600}';
    let mut r = renderer();
    assert!(r.color_font_has(grin), "the host's colour face maps 😀");
    r.debug_block_on_lazy_fallbacks();

    let default_key = r.glyph_key(grin);
    assert!(
        matches!(default_key.glyph_class, GlyphClass::Rgba)
            && default_key.source == FaceId::ColorEmoji,
        "😀 takes the colour two-cell path: {default_key:?}"
    );

    let mut vs15_term = Terminal::new(1, 4);
    vs15_term.process("\x1b[?25l😀\u{FE0E}\x1b[2GX".as_bytes());
    let vs15 = vs15_term.cell_frame(1, 4);
    let cell = vs15.cells[0][0];
    assert!(cell.text_presentation);
    assert!(!cell.emoji_presentation);
    assert_eq!(materialized_cell_span(&vs15.cells[0], 0), 1);
    assert_eq!(vs15.cells[0][1].ch, 'X');

    let text_key = r.resolve_cell_key_for_span(None, &cell, 1);
    assert_ne!(
        text_key, default_key,
        "default and explicit-text forms must not alias in the resolve cache"
    );
    assert_ne!(text_key.source, FaceId::ColorEmoji);
    assert!(
        matches!(text_key.glyph_class, GlyphClass::Mono | GlyphClass::MonoGid),
        "VS15 must resolve to foreground-tinted text coverage: {text_key:?}"
    );
    r.keys.clear();
    r.styled_keys.clear();
    let text_first = r.resolve_cell_key_for_span(None, &cell, 1);
    let default_after_text = r.glyph_key(grin);
    assert_eq!(text_first, text_key);
    assert_eq!(
        default_after_text, default_key,
        "a text-first memo entry must not poison later default emoji"
    );

    let (cw, ch) = r.cell_size();
    for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut styled = cell;
        styled.bold = bold;
        styled.italic = italic;
        let key = r.resolve_cell_key_for_span(None, &styled, 1);
        assert_ne!(key.source, FaceId::ColorEmoji);
        assert!(matches!(
            key.glyph_class,
            GlyphClass::Mono | GlyphClass::MonoGid
        ));
        let img = r.glyph_image(key).clone();
        assert!(
            img.xmin() >= 0 && img.xmin() + img.width() as i32 <= cw as i32,
            "VS15 style bold={bold} italic={italic} escaped one-cell box: {img:?}"
        );
        assert!(img.height() <= ch);
    }

    let vs15_frame = r.render_input(&vs15);
    let mut control_term = Terminal::new(1, 4);
    control_term.process(b"\x1b[?25l X");
    let control = control_term.cell_frame(1, 4);
    let control_frame = r.render_input(&control);
    let cell_band = |frame: &Frame, col: usize| -> Vec<u32> {
        (0..ch)
            .flat_map(|y| {
                let start = y * frame.width + col * cw;
                frame.pixels[start..start + cw].iter().copied()
            })
            .collect()
    };
    assert_ne!(
        cell_band(&vs15_frame, 0),
        cell_band(&control_frame, 0),
        "VS15 assertion must be non-vacuous: the text glyph drew no pixels"
    );
    assert_eq!(
        cell_band(&vs15_frame, 1),
        cell_band(&control_frame, 1),
        "one-cell VS15 glyph overpainted the independently-rendered adjacent X"
    );

    let mut vs16_term = Terminal::new(1, 5);
    vs16_term.process("\x1b[?25l\u{2764}\u{FE0F}X".as_bytes());
    let vs16 = vs16_term.cell_frame(1, 5);
    assert!(vs16.cells[0][0].emoji_presentation);
    assert!(!vs16.cells[0][0].text_presentation);
    assert_eq!(materialized_cell_span(&vs16.cells[0], 0), 2);
    assert_eq!(
        r.resolve_cell_key_for_span(None, &vs16.cells[0][0], 2)
            .glyph_class,
        GlyphClass::Rgba
    );

    let mut bare_term = Terminal::new(1, 5);
    bare_term.process("\x1b[?25l😀X".as_bytes());
    let bare = bare_term.cell_frame(1, 5);
    assert!(!bare.cells[0][0].text_presentation);
    assert_eq!(materialized_cell_span(&bare.cells[0], 0), 2);
    assert_eq!(
        r.resolve_cell_key_for_span(None, &bare.cells[0][0], 2),
        default_key
    );

    let mut cjk_term = Terminal::new(1, 5);
    cjk_term.process("\x1b[?25l中X".as_bytes());
    let cjk = cjk_term.cell_frame(1, 5);
    assert!(!cjk.cells[0][0].text_presentation);
    assert!(!cjk.cells[0][0].emoji_presentation);
    assert_eq!(materialized_cell_span(&cjk.cells[0], 0), 2);
}

#[test]
fn tag_character_draws_nothing_over_its_base() {
    let mut r = renderer();
    r.debug_block_on_lazy_fallbacks();
    let frame_for = |r: &mut Renderer, text: &str| {
        let mut term = Terminal::new(1, 4);
        term.process(format!("\x1b[?25l{text}").as_bytes());
        let input = term.cell_frame(1, 4);
        let attached = term
            .grid()
            .cell_extra(0, 0)
            .map_or(0, |e| e.combining().len());
        let frame = r.render_input(&input);
        (input, frame, attached)
    };
    for (marked, plain) in [("A\u{E0001}X", "AX"), ("e\u{E0100}X", "eX")] {
        let (input, marked_frame, attached) = frame_for(&mut r, marked);
        let (_, plain_frame, _) = frame_for(&mut r, plain);
        assert_eq!(
            attached, 1,
            "{marked:?}: the mark attached to the base cell"
        );
        assert_eq!(input.cells[0][1].ch, 'X', "{marked:?}: X is adjacent");
        assert!(
            input.combining_at(0, 0).is_none() && input.cluster_at(0, 0).is_none(),
            "{marked:?}: the mark is neither an overlay nor a cluster"
        );
        assert_eq!(
            marked_frame.pixels, plain_frame.pixels,
            "{marked:?} must render byte-identically to {plain:?}"
        );
    }

    let (_, acute, acute_attached) = frame_for(&mut r, "e\u{0301}X");
    let (_, bare, _) = frame_for(&mut r, "eX");
    assert_eq!(acute_attached, 1, "the acute attached to its base cell");
    assert_ne!(
        acute.pixels, bare.pixels,
        "non-vacuity: a harness that cannot see a combining acute cannot \
         see a tag character either, and every assertion above would pass \
         for the wrong reason"
    );
}

#[test]
fn subdivision_flag_resolves_to_a_colour_glyph_or_the_bare_flag() {
    const ENGLAND: &str = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}";
    let mut r = renderer();
    r.debug_block_on_lazy_fallbacks();
    let mut term = Terminal::new(1, 4);
    term.process(format!("\x1b[?25l{ENGLAND}X").as_bytes());
    let input = term.cell_frame(1, 4);
    assert_eq!(input.cluster_at(0, 0), Some(ENGLAND));
    assert!(input.combining_at(0, 0).is_none());
    assert_eq!(materialized_cell_span(&input.cells[0], 0), 2);
    assert_eq!(input.cells[0][2].ch, 'X');
    let cell = input.cells[0][0];
    let key = r.resolve_cell_key(Some(ENGLAND), &cell);
    if r.shape_cluster(ENGLAND).is_some() {
        assert_eq!(
            key.source,
            FaceId::ColorEmoji,
            "the colour face ligated gbeng"
        );
        assert_eq!(
            key.glyph_class,
            GlyphClass::RgbaGid,
            "a shaped cluster is keyed by its glyph id on the colour face"
        );
    } else {
        eprintln!("colour face has no subdivision ligature: bare-flag branch");
        assert_eq!(
            key,
            r.resolve_cell_key(None, &cell),
            "a colour face without the ligature falls back to the bare flag"
        );
    }
}

#[test]
fn vs15_primary_owned_emoji_has_distinct_contained_raster_keys() {
    let font = include_bytes!("../assets/DejaVuSansMono.ttf");
    let mut r = Renderer::from_bytes(font, 18.0, Theme::default()).expect("fixture primary");
    r.set_bold_font(font).expect("fixture injected bold");
    r.set_styled_font_bytes(1, font)
        .expect("fixture real italic");
    r.set_styled_font_bytes(2, font)
        .expect("fixture real bold-italic");

    let bolt = '\u{26A1}';
    assert!(aterm_grapheme::is_emoji_presentation(bolt));
    let default_key = r.glyph_key(bolt);
    assert_eq!(default_key.source, FaceId::Primary);
    assert_eq!(default_key.glyph_class, GlyphClass::MonoGid);
    assert_eq!(default_key.cell_span, 0);
    let raw = r.glyph_image(default_key).clone();
    assert!(raw.width() >= 2, "fixture must have reducible primary ink");

    r.cell_w = (raw.width() / 2).max(1);
    r.clear_glyph_images();
    let raw = r.glyph_image(default_key).clone();
    assert!(
        raw.xmin() < 0 || raw.xmin() + raw.width() as i32 > r.cell_w as i32,
        "negative control must begin with an overwide primary bitmap: {raw:?}"
    );

    let mut term = Terminal::new(1, 4);
    term.process("\x1b[?25l⚡\u{FE0E}\x1b[2GX".as_bytes());
    let input = term.cell_frame(1, 4);
    let cell = input.cells[0][0];
    assert!(cell.text_presentation);
    assert_eq!(materialized_cell_span(&input.cells[0], 0), 1);
    assert_eq!(input.cells[0][1].ch, 'X');

    let text_after_default = r.resolve_cell_key_for_span(None, &cell, 1);
    assert_eq!(text_after_default.cell_span, 1);
    assert_ne!(text_after_default, default_key);
    assert_eq!(text_after_default.source, default_key.source);
    assert_eq!(text_after_default.glyph_class, default_key.glyph_class);
    assert_eq!(text_after_default.ch_or_id, default_key.ch_or_id);

    r.keys.clear();
    let text_first = r.resolve_cell_key_for_span(None, &cell, 1);
    let default_after_text = r.glyph_key(bolt);
    assert_eq!(text_first, text_after_default);
    assert_eq!(default_after_text, default_key);

    let (cw, ch) = r.cell_size();
    for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut styled = cell;
        styled.bold = bold;
        styled.italic = italic;
        let key = r.resolve_cell_key_for_span(None, &styled, 1);
        assert_eq!(key.cell_span, 1);
        assert!(matches!(key.source, FaceId::Primary | FaceId::BoldPrimary));
        let img = r.glyph_image(key).clone();
        assert!(
            img.xmin() >= 0 && img.xmin() + img.width() as i32 <= cw as i32,
            "VS15 primary style bold={bold} italic={italic} escaped one cell: {img:?}"
        );
        assert!(img.height() <= ch);
    }

    let vs15_frame = r.render_input(&input);
    let mut control_term = Terminal::new(1, 4);
    control_term.process(b"\x1b[?25l X");
    let control_frame = r.render_input(&control_term.cell_frame(1, 4));
    let cell_band = |frame: &Frame, col: usize| -> Vec<u32> {
        (0..ch)
            .flat_map(|y| {
                let start = y * frame.width + col * cw;
                frame.pixels[start..start + cw].iter().copied()
            })
            .collect()
    };
    assert_ne!(
        cell_band(&vs15_frame, 0),
        cell_band(&control_frame, 0),
        "primary-owned VS15 glyph must draw non-vacuous pixels"
    );
    assert_eq!(
        cell_band(&vs15_frame, 1),
        cell_band(&control_frame, 1),
        "primary-owned VS15 glyph overpainted the adjacent X"
    );
}

#[test]
fn font_family_spellings_misses_and_paths_resolve() {
    for spelling in [
        "JetBrains Mono",
        "JetBrainsMono",
        "jetbrains-mono",
        "Jet_Brains Mono",
        "JetBrains\u{00A0}Mono",
    ] {
        assert_eq!(normalize_family(spelling), "jetbrainsmono", "{spelling:?}");
    }
    assert_eq!(
        normalize_family("M\u{00DC}NCHEN\u{00A0}Mono"),
        "m\u{00FC}nchenmono"
    );
    for miss in ["", "   ", "NoSuchFamilyXYZ123"] {
        assert_eq!(
            resolve_font_family(miss, FaceTarget::REGULAR),
            None,
            "{miss:?}"
        );
    }
    assert!(
        Renderer::from_system_with_family(Some("NoSuchFamilyXYZ123"), 16.0, Theme::default())
            .is_some(),
        "an unknown family never makes the renderer fail to build"
    );
    if let Some(existing) = FONT_CANDIDATES
        .iter()
        .find(|p| std::path::Path::new(p).is_file())
    {
        assert_eq!(
            resolve_font_family(existing, FaceTarget::REGULAR).map(|face| face.path),
            Some((*existing).to_string())
        );
    }
}

#[cfg(target_os = "macos")]
fn renderer_no_configured_fallbacks() -> Renderer {
    let mut r = Renderer::from_bytes(&system_font_bytes(), 16.0, Theme::default())
        .expect("the primary face builds a renderer");
    r.fallback_paths.clear();
    r.symbol_fallback_paths.clear();
    r.color_font_paths.clear();
    r
}

#[test]
#[cfg(target_os = "macos")]
fn runtime_fallback_resolves_cjk() {
    let mut r = renderer_no_configured_fallbacks();
    let key = r.glyph_key('中');
    assert_eq!(
        key.source,
        FaceId::RuntimeFallback,
        "中 with no configured fallback must come from the runtime resolver"
    );
    let img = r.glyph_image(key);
    assert!(img.width() > 0 && img.height() > 0, "中 rasterized empty");
    assert!(
        img.bytes().iter().any(|&c| c > 0),
        "中 has no coverage from the runtime fallback face"
    );
}

#[test]
fn runtime_fallback_decision_cache_is_bounded() {
    let mut rf = RuntimeFallback::default();
    for i in 0..RuntimeFallback::MAX_DECISIONS as u32 {
        if let Some(c) = char::from_u32(0xE000 + i) {
            rf.decisions.insert(c, None);
        }
    }
    assert!(rf.decisions.len() >= RuntimeFallback::MAX_DECISIONS);
    let ctx = FallbackProbeCtx {
        use_ct: false,
        primary_weight: 400,
        primary_italic: false,
    };
    let _ = rf.resolve('\u{FFFF}', ctx);
    assert!(
        rf.decisions.len() <= RuntimeFallback::MAX_DECISIONS,
        "decision cache must stay within MAX_DECISIONS"
    );
    assert!(
        rf.decisions.contains_key(&'\u{FFFF}'),
        "the triggering lookup must remain cached after the bound resets the map"
    );
}

fn solid_rgba_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
    let mut out = Vec::new();
    {
        let mut enc = aterm_png::Encoder::new(&mut out, w, h);
        enc.set_color(aterm_png::ColorType::Rgba);
        enc.set_depth(aterm_png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&rgba).expect("png data");
    }
    out
}

fn lcg_rgba(len: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

#[test]
fn identity_footprint_returns_decode_unchanged() {
    let src = lcg_rgba(6 * 4 * 4);
    assert_eq!(
        resample_rgba(&src, 6, 4, 6, 4),
        src,
        "1:1 resample is the source"
    );

    let png = solid_rgba_png(8, 4, [12, 200, 99]);
    let (decoded, w, h) = decode_png_rgba8(&png).expect("8x4 PNG decodes");
    let fp = decode_image_to_footprint(
        &png,
        aterm_core::grid::extra::ImageFormat::Png,
        w,
        h,
        aterm_core::grid::extra::ImageScaling::Fit,
        None,
    )
    .expect("identity footprint decodes");
    assert_eq!(fp, decoded, "footprint at source size is the decode itself");
}

#[test]
fn background_opacity_keeps_glyph_pixels_opaque() {
    let mut r = Renderer::from_system(32.0, Theme::default()).expect(NO_FONT);
    let (cw, ch) = r.cell_size();
    let mut term = Terminal::new(1, 2);
    term.set_default_background(aterm_core::terminal::Rgb {
        r: 0x11,
        g: 0x13,
        b: 0x18,
    });
    term.process(b"\x1b[?25l\x1b[38;2;255;255;255mX\x1b[0m");
    let input = term.cell_frame(1, 2);
    r.set_background_opacity(0.5);
    let frame = r.render_input(&input);
    assert!(
        cell_pixels(&frame, 0, 0, cw, ch).any(|p| p == 0x00ff_ffff),
        "a full-coverage glyph pixel must be opaque raw fg"
    );
    let t = 127;
    assert!(
        cell_pixels(&frame, 0, 0, cw, ch).any(|p| p >> 24 == t),
        "bg-only pixels of the glyph cell must stay translucent"
    );
}

#[test]
fn translucency_floors_glyphs_without_a_configured_floor() {
    let mut r = Renderer::from_system(32.0, Theme::default()).expect(NO_FONT);
    let (cw, ch) = r.cell_size();
    let mut term = Terminal::new(1, 2);
    term.set_default_background(aterm_core::terminal::Rgb {
        r: 0x11,
        g: 0x13,
        b: 0x18,
    });
    term.process(b"\x1b[?25l\x1b[38;2;42;45;53mX\x1b[0m");
    let input = term.cell_frame(1, 2);
    let (fg, bg) = (0x002A_2D35_u32, 0x0011_1318_u32);
    assert!(
        contrast_ratio(fg, bg) < 4.5,
        "precondition: the raw fg is sub-AA"
    );

    r.set_background_opacity(1.0);
    let opaque = r.render_input(&input);
    assert!(
        cell_pixels(&opaque, 0, 0, cw, ch).any(|p| p == fg),
        "an opaque frame paints the unfloored fg at full coverage"
    );

    r.set_background_opacity(0.6);
    assert_eq!(
        r.minimum_contrast(),
        0.0,
        "the configured floor is untouched"
    );
    assert_eq!(r.effective_minimum_contrast(), 4.5);
    let glass = r.render_input(&input);
    let best = cell_pixels(&glass, 0, 0, cw, ch)
        .filter(|p| p >> 24 == 0)
        .map(|p| contrast_ratio(p & 0x00ff_ffff, bg))
        .fold(0.0_f32, f32::max);
    assert!(best >= 4.5, "glass floored the glyph to only {best}:1");
    assert!(
        cell_pixels(&glass, 0, 0, cw, ch)
            .zip(cell_pixels(&opaque, 0, 0, cw, ch))
            .any(|(a, b)| a & 0x00ff_ffff != b & 0x00ff_ffff),
        "the floor must change the glyph's ink"
    );
}

#[test]
fn renderer_effective_floor_survives_rebuild_raw() {
    let mut r = renderer();
    r.seal_admitted_font_sources();
    r.set_minimum_contrast(2.0);
    r.set_background_opacity(0.6);
    assert_eq!(r.effective_minimum_contrast(), 4.5);
    let mut rebuilt = r
        .rebuild_from_admitted(18.0, Theme::default())
        .expect("a sealed generation rebuilds");
    assert_eq!(rebuilt.minimum_contrast(), 2.0, "raw value retained");
    assert_eq!(rebuilt.effective_minimum_contrast(), 4.5);
    rebuilt.set_background_opacity(1.0);
    assert_eq!(
        rebuilt.effective_minimum_contrast(),
        2.0,
        "the raise is not baked in"
    );
}

#[test]
fn selection_palette_precedence() {
    let mut r = renderer();
    let theme = Theme::default();
    let custom = 0x0012_3456;
    r.set_selection_inactive(true);
    r.set_selection_inactive_bg(Some(custom));
    assert_eq!(
        r.effective_selection_bg(),
        custom,
        "the override wins unfocused"
    );
    r.set_selection_inactive(false);
    assert_eq!(
        r.effective_selection_bg(),
        theme.selection,
        "…and not focused"
    );

    let mut input = RenderInput::empty();
    input.default_bg = 0x00e0_d0c0;
    input.selection_bg = 0x0060_4020;
    input.selection_fg = 0x0012_3456;
    assert_eq!(r.selection_palette(&input).scalar().bg, input.selection_bg);
    assert_eq!(
        r.selection_palette(&input).scalar().fg,
        Some(input.selection_fg)
    );
    r.set_selection_fg(Some(0x00ab_cdef));
    input.selection_fg = aterm_core::render::COLOR_DYNAMIC;
    assert_eq!(
        r.selection_palette(&input).scalar().fg,
        None,
        "an engine-requested dynamic foreground overrides the renderer setting"
    );
    input.selection_fg = aterm_core::render::COLOR_UNSET;
    assert_eq!(
        r.selection_palette(&input).scalar().fg,
        Some(0x00ab_cdef),
        "an unresolved producer still delegates to the renderer setting"
    );
    input.selection_bg = aterm_core::render::COLOR_UNSET;
    assert_eq!(
        r.selection_palette(&input).scalar().bg,
        r.effective_selection_bg(),
        "an unresolved producer still delegates to the configured policy"
    );
    input.selection_bg = 0x0060_4020;
    r.set_selection_inactive(true);
    r.set_selection_inactive_bg(None);
    assert_eq!(
        r.selection_palette(&input).scalar().bg,
        derive_inactive_selection_bg(input.selection_bg, input.default_bg)
    );
    r.set_selection_inactive_bg(Some(custom));
    assert_eq!(r.selection_palette(&input).scalar().bg, custom);
}

#[test]
fn font_interns_release_injected_bytes_and_prune_churn() {
    std::thread::spawn(|| {
        for n in 0..128_u8 {
            let bytes = vec![n; 1024];
            let owned = intern_font_bytes(bytes.clone());
            let borrowed = intern_font_bytes_slice(&bytes);
            assert!(std::sync::Arc::ptr_eq(&owned, &borrowed));
            let weak = std::sync::Arc::downgrade(&owned);
            drop(owned);
            assert!(weak.upgrade().is_some(), "another pane retains its font");
            drop(borrowed);
            assert!(
                weak.upgrade().is_none(),
                "the index must not retain font bytes"
            );
            FONT_BYTES_INTERN.with(|store| assert_eq!(store.borrow().len(), 1));
        }
    })
    .join()
    .unwrap();
}

#[cfg(feature = "embedded-font")]
#[test]
fn sealed_generation_rebuilds_after_all_source_paths_disappear() {
    let scratch = crate::test_scratch_dir("admitted-font-rebuild");
    let path = scratch.path().join("PreparedMono-Regular.ttf");
    std::fs::write(&path, embedded_font()).expect("write admitted primary fixture");

    let theme = Theme::default();
    let mut renderer = Renderer::from_resolved_font_file(
        FaceRef::new(path.to_string_lossy().into_owned(), 0),
        embedded_font(),
        16.0,
        theme,
    )
    .expect("primary fixture parses");
    for slot in 0..3 {
        renderer
            .set_styled_font_bytes(slot, embedded_font())
            .expect("styled fixture parses");
    }
    renderer
        .set_bold_font(embedded_font())
        .expect("injected bold fixture parses");
    renderer
        .set_fallback_bytes(embedded_font())
        .expect("fallback fixture parses");
    renderer
        .set_symbol_fallback_bytes(embedded_font())
        .expect("symbol fixture parses");
    renderer
        .set_color_font_arc(intern_font_bytes(embedded_font().to_vec()))
        .expect("emoji fixture parses");

    assert!(
        renderer.rebuild_from_admitted(20.0, theme).is_err(),
        "negative control: a generation must be sealed before reconstruction"
    );
    let before = renderer.seal_admitted_font_sources();
    assert!(
        before.sealed,
        "worker seal is represented in the exact snapshot"
    );
    assert!(before.primary.is_some());
    assert!(before.injected_bold.is_some());
    assert!(before.styled.iter().all(Option::is_some));
    assert_eq!(before.fallback.len(), 1);
    assert_eq!(before.symbol.len(), 1);
    assert!(
        before
            .emoji
            .first()
            .is_some_and(|head| head.bytes.bytes() == embedded_font())
    );

    std::fs::remove_dir_all(scratch.path()).expect("remove every source pathname");
    drop(scratch);
    let rebuilt = renderer
        .rebuild_from_admitted(24.0, theme)
        .expect("sealed rebuild uses retained bytes only");
    assert_eq!(
        rebuilt.admitted_font_sources(),
        before,
        "zoom rebuild must preserve the exact complete source generation"
    );
    assert_eq!(
        rebuilt.cell_geometry(24.0),
        rebuilt.cell_geometry(rebuilt.px)
    );

    let semantic = renderer
        .fork_semantic_surface(14.0, theme)
        .expect("sealed semantic fork uses retained bytes only");
    assert_eq!(
        semantic.admitted_font_sources(),
        before,
        "semantic fork must not resurrect any lazy candidate path"
    );
}
