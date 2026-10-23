// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests for bundled display font faces, cell fitting, and layer mixing.

use aterm_render::{
    DISPLAY_FACE_MIX_MAX, DISPLAY_FACE_SCHEME, DISPLAY_FACES, FaceId, Renderer, Theme,
    display_face_bytes, display_face_canonical_id, display_face_fit, display_face_ids_for_family,
    display_face_mix_for_family, display_face_scale, display_layer_ids, display_mix_face_index,
};

/// Every single face and every 2- and 3-face mix of the registry, in every
/// order: the layers a person can switch on.
fn every_layer() -> Vec<Vec<&'static str>> {
    let ids: Vec<&'static str> = DISPLAY_FACES.iter().map(|face| face.id).collect();
    let mut layers = Vec::new();
    for &a in &ids {
        layers.push(vec![a]);
        for &b in ids.iter().filter(|&&b| b != a) {
            layers.push(vec![a, b]);
            for &c in ids.iter().filter(|&&c| c != a && c != b) {
                layers.push(vec![a, b, c]);
            }
        }
    }
    layers
}

/// Every SET of faces a person can switch on — each single face and each 2-
/// and 3-face mix once, in registry order.
fn every_set() -> Vec<Vec<&'static str>> {
    let rank = |id: &&str| DISPLAY_FACES.iter().position(|face| face.id == *id);
    every_layer()
        .into_iter()
        .filter(|ids| ids.is_sorted_by_key(rank))
        .collect()
}

/// The primaries the layer is drawn over: the embedded DejaVu Sans Mono and
/// the bundled Iosevka Term, whose cells differ (Iosevka's is narrower and
/// shorter), so a fit that only works in one cell fails here.
fn primaries() -> Vec<(&'static str, &'static [u8])> {
    let mut faces = Vec::new();
    #[cfg(feature = "embedded-font")]
    faces.push(("DejaVu Sans Mono", aterm_render::embedded_font()));
    if let Some(iosevka) = aterm_render::bundled::face_for_id(aterm_render::bundled::IOSEVKA_ID) {
        faces.push(("Iosevka Term", iosevka));
    }
    assert!(
        !faces.is_empty(),
        "no primary face compiled into this build"
    );
    faces
}

fn layered(primary: &[u8], ids: &[&str], px: f32) -> Renderer {
    let mut renderer = Renderer::from_bytes(primary, px, Theme::default()).expect("primary face");
    renderer
        .set_display_faces(ids)
        .expect("a bundled display face installs");
    renderer
}

/// THE GRID IS THE PERSON'S FONT'S. A display face is a layer over the
/// primary, so turning one on — or mixing two or three, in either order —
/// leaves the cell and baseline exactly the bare primary's: no column and no
/// row is ever lost to a face.
#[test]
fn a_display_face_never_changes_the_grid() {
    for (name, primary) in primaries() {
        for px in [12.0_f32, 16.0, 24.0] {
            let bare = Renderer::from_bytes(primary, px, Theme::default()).unwrap();
            let want = (bare.cell_size(), bare.baseline());
            for ids in every_set() {
                let renderer = layered(primary, &ids, px);
                assert_eq!(
                    (renderer.cell_size(), renderer.baseline()),
                    want,
                    "{ids:?} over {name} at {px}px changed the grid"
                );
            }
        }
    }
}

/// One set of faces draws ONE way: the layer is kept in registry order, so
/// the order the switches were turned on in changes neither the grid, nor
/// which face a letter takes, nor the glyph it draws.
#[test]
fn the_order_faces_were_switched_on_in_changes_nothing() {
    let (_, primary) = primaries()[0];
    for ids in every_layer() {
        let mut sorted = ids.clone();
        sorted.sort_by_key(|id| DISPLAY_FACES.iter().position(|face| face.id == *id));
        let mut written = layered(primary, &ids, 16.0);
        let mut canonical = layered(primary, &sorted, 16.0);
        assert_eq!(written.display_faces(), sorted, "{ids:?}");
        assert_eq!(display_layer_ids(ids.iter().copied()), sorted);
        assert_eq!(
            (written.cell_size(), written.baseline()),
            (canonical.cell_size(), canonical.baseline()),
            "{ids:?}: the grid"
        );
        for ch in '!'..='~' {
            let (a, b) = (written.glyph_key(ch), canonical.glyph_key(ch));
            assert_eq!(a, b, "{ids:?}: {ch:?} routes alike");
            let (a, b) = (
                raster(written.glyph_image(a)),
                raster(canonical.glyph_image(b)),
            );
            assert_eq!(a, b, "{ids:?}: {ch:?} draws alike");
        }
    }
}

/// THE INVARIANT THE FIT EXISTS FOR: every glyph the layer draws is WHOLE and
/// inside its cell — never wider than the cell (it would paint over its
/// neighbour: "minecraft" drew as "m ncraft"), never above the cell's top
/// (the first row's ink was cut at the window edge and under a pane header)
/// and never below its floor — and never cut to get there: its ink is as tall
/// as the face draws it at the fitted size.
#[test]
fn every_display_glyph_is_drawn_whole_inside_its_cell() {
    let fits: std::collections::HashMap<&str, _> = DISPLAY_FACES
        .iter()
        .map(|face| {
            let bytes = display_face_bytes(face.id).unwrap();
            (face.id, (bytes, display_face_fit(bytes).unwrap()))
        })
        .collect();
    let mut owners = std::collections::HashMap::new();
    let mut own_rows = std::collections::HashMap::new();
    for (name, primary) in primaries() {
        let _shared = Renderer::from_bytes(primary, 16.0, Theme::default()).unwrap();
        for line_height in [1.0_f32, 0.8] {
            for px in [12.0_f32, 14.0, 20.0, 48.0] {
                for ids in every_set() {
                    let mut renderer = layered(primary, &ids, px);
                    renderer.set_line_height(line_height);
                    let (cell_w, cell_h) = renderer.cell_size();
                    let baseline = renderer.baseline();
                    let at = || format!("{ids:?} over {name} at {px}px, line height {line_height}");
                    for ch in ('!'..='~').chain("ÁÉÖÅÇÿ".chars()) {
                        let key = renderer.glyph_key(ch);
                        if key.source != FaceId::DisplayMix {
                            continue;
                        }
                        let img = renderer.glyph_image(key).clone();
                        if img.width() == 0 || img.height() == 0 {
                            continue;
                        }
                        let top = baseline - (img.height() as i32 + img.ymin());
                        assert!(
                            img.xmin() >= 0 && img.xmin() + img.width() as i32 <= cell_w as i32,
                            "{}: {ch:?} spans {}..{} outside the {cell_w}px cell",
                            at(),
                            img.xmin(),
                            img.xmin() + img.width() as i32
                        );
                        assert!(
                            top >= 0 && top + img.height() as i32 <= cell_h as i32,
                            "{}: {ch:?} spans rows {top}..{} outside the {cell_h}px cell",
                            at(),
                            top + img.height() as i32
                        );
                        if !ch.is_ascii() {
                            continue;
                        }
                        let id = ids
                            .iter()
                            .copied()
                            .find(|id| renderer_face_of(&renderer, ch) == *id)
                            .expect("the drawing face is in the layer");
                        let (bytes, fit) = fits[id];
                        let scale = display_face_scale(&fit, px, cell_w, cell_h);
                        let authored = *own_rows
                            .entry((id, (px * scale).to_bits(), ch))
                            .or_insert_with(|| {
                                let own = owners
                                    .entry((id, (px * scale).to_bits()))
                                    .or_insert_with(|| {
                                        Renderer::from_bytes(bytes, px * scale, Theme::default())
                                            .expect("the face draws on its own")
                                    });
                                let own_key = own.glyph_key(ch);
                                let own_img = own.glyph_image(own_key);
                                ink_rows(own_img.width(), own_img.bytes()) as i32
                            });
                        assert!(
                            ink_rows(img.width(), img.bytes()) as i32 >= authored - 1,
                            "{}: {ch:?} from {id} is {} rows, the face draws {authored} — cut",
                            at(),
                            img.height()
                        );
                    }
                }
            }
        }
    }
}

/// A glyph raster as comparable facts: its box, placement and coverage.
fn raster(img: &aterm_render::GlyphImage) -> (usize, usize, i32, i32, Vec<u8>) {
    (
        img.width(),
        img.height(),
        img.xmin(),
        img.ymin(),
        img.bytes().to_vec(),
    )
}

/// The rows of a coverage mask `width` wide that carry ink.
fn ink_rows(width: usize, coverage: &[u8]) -> usize {
    if width == 0 {
        return 0;
    }
    let rows: Vec<bool> = coverage
        .chunks_exact(width)
        .map(|row| row.iter().any(|&a| a > 0))
        .collect();
    match (
        rows.iter().position(|&ink| ink),
        rows.iter().rposition(|&ink| ink),
    ) {
        (Some(first), Some(last)) => last + 1 - first,
        _ => 0,
    }
}

/// Which layer face draws `ch` (the pick the renderer makes).
fn renderer_face_of(renderer: &Renderer, ch: char) -> &'static str {
    let faces = renderer.display_faces();
    faces[display_mix_face_index(ch, faces.len())]
}

/// The fit does not buy containment by drawing letters small: the scale is
/// set by a typical letter (the median advance) and the ink band, never by
/// the widest glyph (Cinzel's `Q` tail at 1.27 em would have drawn every
/// letter at half size), so over either primary — Iosevka Term's cell is a
/// narrow 0.5 em — a face keeps at least 0.7 of the size asked for.
#[test]
fn a_display_face_keeps_most_of_its_size() {
    for (name, primary) in primaries() {
        for px in [12.0_f32, 16.0, 24.0] {
            let bare = Renderer::from_bytes(primary, px, Theme::default()).unwrap();
            let (cell_w, cell_h) = bare.cell_size();
            for font in DISPLAY_FACES {
                let fit = display_face_fit(display_face_bytes(font.id).unwrap()).unwrap();
                let scale = display_face_scale(&fit, px, cell_w, cell_h);
                assert!(
                    (0.7..=1.0).contains(&scale),
                    "{} over {name} at {px}px is drawn at {scale:.2} of its size",
                    font.id
                );
            }
        }
    }
}

/// The scale is the largest that fits: a roomier cell never shrinks a face,
/// the result never exceeds 1, and a degenerate cell still draws something.
#[test]
fn the_display_scale_is_monotone_and_bounded() {
    let fit = display_face_fit(display_face_bytes("chunky").unwrap()).unwrap();
    let tight = display_face_scale(&fit, 16.0, 8, 12);
    let roomy = display_face_scale(&fit, 16.0, 12, 24);
    assert!(tight <= roomy && roomy <= 1.0, "{tight} {roomy}");
    assert_eq!(display_face_scale(&fit, 16.0, 1, 1), 0.25);
    assert_eq!(display_face_scale(&fit, f32::NAN, 8, 16), 1.0);
}

/// A DEEP DESCENDER RAISES THE LINE INSTEAD OF SHRINKING THE FACE: Chewy's
/// `g` sinks 0.30 em, past DejaVu Sans Mono's 0.24 em descent, while the cell
/// has room to spare above.
#[cfg(feature = "embedded-font")]
#[test]
fn a_deep_descender_raises_the_line_instead_of_shrinking_the_face() {
    let primary = aterm_render::embedded_font();
    let bare = Renderer::from_bytes(primary, 48.0, Theme::default()).unwrap();
    let (cell_w, cell_h) = bare.cell_size();
    let fit = |id: &str| display_face_fit(display_face_bytes(id).unwrap()).unwrap();
    let (bubble, chunky) = (fit("bubble"), fit("chunky"));
    assert_eq!(display_face_scale(&bubble, 48.0, cell_w, cell_h), 1.0);
    let line = aterm_render::display_layer_baseline(
        &[bubble],
        None,
        48.0,
        cell_w,
        cell_h,
        bare.baseline(),
    );
    assert!(line < bare.baseline(), "the line rises for Chewy's `g`");
    let alone = aterm_render::display_layer_baseline(
        &[chunky],
        None,
        48.0,
        cell_w,
        cell_h,
        bare.baseline(),
    );
    assert_eq!(
        alone,
        bare.baseline(),
        "a face that fits keeps the primary's line"
    );
    let mixed = aterm_render::display_layer_baseline(
        &[chunky, bubble],
        None,
        48.0,
        cell_w,
        cell_h,
        bare.baseline(),
    );
    assert_eq!(mixed, line, "a mix stands on one line");
    let mut renderer = layered(primary, &["bubble", "chunky"], 48.0);
    let mut floors = std::collections::BTreeSet::new();
    for ch in ('a'..='z').filter(|ch| "acemnorsuvwxz".contains(*ch)) {
        let key = renderer.glyph_key(ch);
        let img = renderer.glyph_image(key);
        floors.insert(img.ymin() - (bare.baseline() - line));
    }
    assert!(
        floors.iter().all(|&floor| (-2..=1).contains(&floor)),
        "letters stand on the raised line: {floors:?}"
    );
}

/// A FIRST ROW DRAWN UNDER ANYTHING KEEPS ITS INK: Luckiest Guy's backtick
/// rises 0.93 em over its 0.70 em ascent, so drawn as the primary it inked
/// above its own cell — into the 2 px window padding, cut at the frame edge,
/// and under a split pane's header.
#[cfg(feature = "embedded-font")]
#[test]
fn a_first_row_draws_exactly_like_the_rows_below_it() {
    let text = "`$ LS \"Q\" {J}";
    let mut renderer = layered(aterm_render::embedded_font(), &["chunky"], 48.0);
    let cols = text.chars().count();
    let mut terminal = aterm_core::terminal::Terminal::new(4, cols as u16);
    terminal.process(format!("\x1b[?25l{text}\r\n\r\n\r\n{text}").as_bytes());
    let mut input = terminal.cell_frame(4, cols);
    input.cursor_visible = false;
    let frame = renderer.render_input(&input);
    let (_, cell_h) = renderer.cell_size();
    let width = frame.width;
    let row = |r: usize| -> Vec<u32> {
        let y0 = renderer.pad_top() + r * cell_h;
        (y0..y0 + cell_h)
            .flat_map(|y| frame.pixels[y * width..(y + 1) * width].to_vec())
            .collect()
    };
    assert_eq!(row(0), row(3), "row 0 lost ink row 3 keeps");
}

/// THE CHROME IS THE PERSON'S FONT: Settings' code text, the palette and the
/// Manual editor take their face from the terminal's primary
/// (`chrome_primary_face`).
#[test]
fn the_chrome_face_is_the_primary_under_a_display_layer() {
    let plain = Renderer::from_system(16.0, Theme::default()).expect("a system font");
    let mut layered = Renderer::from_system(16.0, Theme::default()).expect("a system font");
    layered.set_display_faces(&["engraved"]).unwrap();
    let face = |r: &Renderer| {
        r.chrome_primary_face()
            .map(|(bytes, index)| (bytes.to_vec(), index))
    };
    assert_eq!(face(&layered), face(&plain));
    let engraved = display_face_bytes("engraved").unwrap();
    assert!(face(&layered).is_some_and(|(bytes, _)| bytes != engraved));
}

/// A layer cell is not the primary's to shape: a primary with programming
/// ligatures (Iosevka Term) ligates `=>` bare, and under the layer the
/// letters the layer draws stay per-cell, so a primary ligature never
/// replaces display-face letters.
#[test]
fn a_display_cell_is_never_ligated_by_the_primary() {
    let Some(iosevka) = aterm_render::bundled::face_for_id(aterm_render::bundled::IOSEVKA_ID)
    else {
        aterm_spec::host::could_not_run("this build bundles no Iosevka Term (embedded-font off)");
    };
    let text = "a => b";
    let mut terminal = aterm_core::terminal::Terminal::new(1, text.len() as u16);
    terminal.process(format!("\x1b[?25l{text}").as_bytes());
    let input = terminal.cell_frame(1, text.len());
    let plan = |renderer: &mut Renderer| {
        let mut plan = Vec::new();
        renderer.row_glyph_plan(&input, 0, &[], &mut plan);
        plan
    };
    let mut bare = Renderer::from_bytes(iosevka, 16.0, Theme::default()).unwrap();
    assert!(
        plan(&mut bare)
            .iter()
            .any(|glyph| !matches!(glyph, aterm_render::ColumnGlyph::PerCell)),
        "Iosevka Term ligates `=>`, or this test cannot fail"
    );
    let mut layer = layered(iosevka, &["pixel"], 16.0);
    for (col, glyph) in plan(&mut layer).iter().enumerate() {
        let ch = text.as_bytes()[col] as char;
        if layer.glyph_key(ch).source == FaceId::DisplayMix {
            assert!(
                matches!(glyph, aterm_render::ColumnGlyph::PerCell),
                "{ch:?} is the layer's, yet the primary shaped it"
            );
        }
    }
}

/// The monospaced bundled face (Monocraft, `pixel`) is not centred by its ink
/// and takes no weight boost: its own bearings and weight are kept.
#[test]
fn the_monospaced_display_face_keeps_its_bearings_and_weight() {
    let fit =
        display_face_fit(display_face_bytes("pixel").unwrap()).expect("bundled face has a fit");
    assert!(!fit.proportional, "the advance is centred, not the ink");
    assert!(!fit.embolden, "no weight change");
}

/// A face that is NOT bundled is never a display face: the fit keys off the
/// registry bytes, and the layer takes registry ids only, so a person's own
/// font is the primary and draws as itself.
#[test]
fn only_a_bundled_face_is_ever_a_display_face() {
    #[cfg(feature = "embedded-font")]
    assert!(
        display_face_fit(aterm_render::embedded_font()).is_none(),
        "the embedded fallback face is not a display face and must not be fitted"
    );
    let (_, primary) = primaries()[0];
    let renderer = layered(primary, &["Menlo", "doom", "mariokart"], 16.0);
    assert!(renderer.display_faces().is_empty());
}

/// The proportional faces take the fit, so an asset swap that changes the bytes the registry
/// lookup matches cannot leave the policy inert.
#[test]
fn proportional_display_faces_are_fitted() {
    for id in ["chunky", "engraved", "bubble"] {
        let fit = display_face_fit(display_face_bytes(id).unwrap()).expect("bundled");
        assert!(fit.proportional, "{id} is proportional and takes the fit");
        assert!(
            fit.ink_top_em > 0.5 && fit.ink_bottom_em < 0.0,
            "{id}: {fit:?}"
        );
        assert!(fit.letter_advance_em > 0.3, "{id}: {fit:?}");
    }
}

/// The `display:` scheme is exact: every registry id resolves under it; bare ids, unknown
/// ids and real family names never match, so an installed font cannot be shadowed.
#[test]
fn display_scheme_resolution_is_exact() {
    for font in DISPLAY_FACES {
        let family = format!("{DISPLAY_FACE_SCHEME}{}", font.id);
        assert_eq!(
            display_face_ids_for_family(&family),
            Some(vec![font.id]),
            "{family} resolves"
        );
        assert!(
            display_face_ids_for_family(font.id).is_none(),
            "bare id {:?} must NOT resolve without the scheme",
            font.id
        );
    }
    assert!(display_face_ids_for_family("display:doom").is_none());
    assert!(display_face_ids_for_family("Menlo").is_none());
}

/// A `display:` family is a request for the layer, never a face: no resolver
/// answers it — the catalogue batch and the config resolver both refuse it,
/// saying where display faces are set — so the primary is always a real font.
#[test]
fn no_resolver_answers_a_display_family_with_a_face() {
    let requests = vec![(
        "display:pixel".to_string(),
        aterm_render::FaceTarget::REGULAR,
    )];
    let batch = aterm_render::font_catalog::resolve_and_admit(&requests);
    let refusal = batch
        .get(0)
        .expect("entry present")
        .as_ref()
        .expect_err("a display family admits no face");
    assert!(refusal.contains("display_font"), "{refusal}");
    let refusal =
        aterm_render::resolve_config_font("display:pixel", aterm_render::FaceTarget::REGULAR)
            .expect_err("a display family resolves to no face");
    assert!(refusal.contains("display_font"), "{refusal}");
}

/// Startup construction honors the scheme as a layer: the default font is the
/// primary — its path, its grid — and the face is drawn over it.
#[test]
fn from_system_with_family_draws_a_display_family_over_the_default_font() {
    let plain = Renderer::from_system(16.0, Theme::default()).expect("a system font");
    let renderer =
        Renderer::from_system_with_family(Some("display:engraved"), 16.0, Theme::default())
            .expect("display face constructs");
    assert_eq!(renderer.primary_source_path(), plain.primary_source_path());
    assert_eq!(renderer.cell_size(), plain.cell_size());
    assert_eq!(renderer.display_faces(), vec!["engraved"]);
}

/// Mix parsing is exact: 1..=3 distinct known ids joined by `+`; anything else rejects the
/// whole request, never a partial mix.
#[test]
fn display_mix_family_parsing_is_exact() {
    let solo = display_face_mix_for_family("display:pixel").expect("single id");
    assert_eq!(solo.len(), 1);
    let duo = display_face_mix_for_family("display:pixel+engraved").expect("two ids");
    assert_eq!(duo.len(), 2);
    assert_eq!(duo[0], display_face_bytes("pixel").unwrap());
    assert_eq!(duo[1], display_face_bytes("engraved").unwrap());
    let trio = display_face_mix_for_family("display:chunky+bubble+engraved").unwrap();
    assert_eq!(trio.len(), DISPLAY_FACE_MIX_MAX);
    for bad in [
        "display:pixel+doom",
        "display:pixel+pixel",
        "display:pixel+Pixel",
        "display:chunky+pixel+engraved+bubble",
        "display:",
        "display:+",
    ] {
        assert!(
            display_face_mix_for_family(bad).is_none(),
            "{bad} must reject"
        );
    }
}

/// End-to-end: a layered renderer routes every letter to the layer (each face
/// covers ASCII), none to the primary, and the routing is stable across
/// repeated lookups; each routed glyph owns exactly its cell.
#[test]
fn a_layer_routes_every_letter_across_its_faces() {
    let mut renderer = Renderer::from_system_with_family(
        Some("display:pixel+engraved+chunky"),
        16.0,
        Theme::default(),
    )
    .expect("mix constructs");
    assert_eq!(
        renderer.display_faces(),
        vec!["chunky", "pixel", "engraved"]
    );
    let (cell_w, _) = renderer.cell_size();
    let mut served = std::collections::BTreeSet::new();
    for ch in ('A'..='Z').chain('a'..='z') {
        let key = renderer.glyph_key(ch);
        assert_eq!(key, renderer.glyph_key(ch), "routing is stable for {ch:?}");
        assert_eq!(
            key.source,
            FaceId::DisplayMix,
            "{ch:?} stays on the primary"
        );
        assert_eq!(key.cell_span, 1);
        served.insert(renderer_face_of(&renderer, ch));
        let image = renderer.glyph_image(key);
        assert_eq!(image.advance(), cell_w as f32, "{ch:?} owns its cell");
    }
    assert_eq!(served.len(), 3, "every face of the mix draws some letter");
}

/// With ligatures on (the default), a display face alone or mixed never puts `.notdef` on
/// screen: Monocraft's operator ligatures shape to glyph 0, so those runs stay per-cell.
#[test]
fn display_face_operators_never_shape_to_notdef() {
    let operators = "a => b -> c != d <= e >= f";
    let families = DISPLAY_FACES
        .iter()
        .map(|face| format!("{DISPLAY_FACE_SCHEME}{}", face.id))
        .chain([format!("{DISPLAY_FACE_SCHEME}pixel+engraved+bubble")]);
    for family in families {
        let mut renderer = Renderer::from_system_with_family(Some(&family), 16.0, Theme::default())
            .unwrap_or_else(|| panic!("{family} constructs"));
        let mut terminal = aterm_core::terminal::Terminal::new(1, operators.len() as u16);
        terminal.process(format!("\x1b[?25l{operators}").as_bytes());
        let input = terminal.cell_frame(1, operators.len());
        let mut plan = Vec::new();
        renderer.row_glyph_plan(&input, 0, &[], &mut plan);
        for (col, glyph) in plan.iter().enumerate() {
            let gid = match glyph {
                aterm_render::ColumnGlyph::PerCell => continue,
                aterm_render::ColumnGlyph::Ligated(gid)
                | aterm_render::ColumnGlyph::LigatedSlice { gid, .. } => *gid,
            };
            assert_ne!(
                gid,
                0,
                "{family}: column {col} ({:?}) would draw .notdef",
                operators.as_bytes()[col] as char
            );
        }
        for ch in ['=', '>', '-', '!', '<'] {
            let key = renderer.glyph_key(ch);
            assert!(
                renderer.glyph_image(key).width() > 0,
                "{family}: {ch:?} draws nothing"
            );
        }
    }
}

/// THE LAYER SURVIVES EVERY REBUILD: `rebuild_from_admitted` (a zoom, a theme
/// flip, the config hot-reload) and the semantic fork rebuild the renderer from
/// copies, and each must carry the layer — the same faces, drawing the same
/// glyphs in the same cell.
#[test]
fn the_layer_survives_a_rebuild_and_a_fork() {
    let mut base =
        Renderer::from_system_with_family(Some("display:engraved+bubble"), 16.0, Theme::default())
            .expect("a system font");
    let before = base.cell_size();
    let _ = base.seal_admitted_font_sources();
    let key = base.glyph_key('Q');
    let glyph = raster(base.glyph_image(key));
    for (how, mut renderer) in [
        (
            "rebuild",
            base.rebuild_from_admitted(16.0, Theme::default())
                .expect("a sealed generation rebuilds"),
        ),
        (
            "fork",
            base.fork_semantic_surface(16.0, Theme::default())
                .expect("a sealed generation forks"),
        ),
    ] {
        assert_eq!(
            renderer.display_faces(),
            vec!["engraved", "bubble"],
            "{how}"
        );
        assert_eq!(renderer.cell_size(), before, "{how}");
        let key = renderer.glyph_key('Q');
        assert_eq!(raster(renderer.glyph_image(key)), glyph, "{how}");
    }
}

/// A VARIATION CHANGE OR A NEW PRIMARY REFITS THE LAYER TO THE NEW CELL: the
/// fit is computed from the cell the glyph is drawn into, so whatever the
/// primary's geometry becomes, the pure read agrees with paint and every layer
/// glyph stays inside the new cell.
#[test]
fn the_layer_follows_the_primary_into_a_new_cell() {
    let mut renderer =
        Renderer::from_system_with_family(Some("display:engraved"), 16.0, Theme::default())
            .expect("the display scheme resolves");
    renderer.set_font_variations(&[(aterm_render::variation::WGHT_TAG, 600.0)], 0.0);
    let primaries = primaries();
    let mut swaps = vec![None];
    swaps.extend(primaries.iter().map(|(_, bytes)| Some(*bytes)));
    for swap in swaps {
        if let Some(bytes) = swap {
            renderer.set_primary_font(bytes).expect("the face installs");
            let fresh = Renderer::from_bytes(bytes, 16.0, Theme::default()).unwrap();
            assert_eq!(renderer.cell_size(), fresh.cell_size());
        }
        let (cell_w, cell_h) = renderer.cell_size();
        assert_eq!(
            renderer.cell_geometry(16.0),
            (cell_w, cell_h, renderer.baseline()),
            "the pure read must equal what the renderer actually paints"
        );
        for ch in '!'..='~' {
            let key = renderer.glyph_key(ch);
            if key.source != FaceId::DisplayMix {
                continue;
            }
            let img = renderer.glyph_image(key);
            assert!(
                img.xmin() >= 0 && img.xmin() + img.width() as i32 <= cell_w as i32,
                "{ch:?} leaves the {cell_w}px cell"
            );
        }
    }
}

/// A `display_font` VALUE IS A HUMAN-TYPED CONFIG STRING, so it folds case like
/// every other one.
#[test]
fn a_display_face_id_folds_ascii_case_like_every_other_config_value() {
    for (typed, want) in [
        ("pixel", "pixel"),
        ("Pixel", "pixel"),
        ("PIXEL", "pixel"),
        ("  Engraved  ", "engraved"),
    ] {
        assert_eq!(
            display_face_canonical_id(typed),
            Some(want),
            "{typed:?} names the {want} face"
        );
    }

    for nonsense in ["pixelated", "pix el", ""] {
        assert_eq!(
            display_face_canonical_id(nonsense),
            None,
            "{nonsense:?} names no face"
        );
    }
}
