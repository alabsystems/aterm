// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FONT-GAME conformance: every bundled game-title face is a real, parseable
//! font, and the `game:` virtual-family scheme resolves identically through
//! BOTH resolution paths (startup construction and the off-thread catalog).

use aterm_render::{
    GAME_FONT_MIX_MAX, GAME_FONT_SCHEME, GAME_FONTS, Renderer, Theme, game_face_fit,
    game_font_bytes, game_font_for_family, game_font_mix_for_family, game_mix_face_index,
};

/// FONT-GAME-FIT, the invariant the whole fit exists to guarantee: NO glyph a
/// FITTED game face can draw is wider than the cell it renders into.
///
/// Violating it is not a cosmetic problem — an overrunning glyph paints over its
/// NEIGHBOUR, which is how `minecraft` came out as "m ncraft" in Luckiest Guy
/// (cell from `M` at 0.79 em, `m` drawn at 0.91 em, the `i` buried in the ink).
///
/// Scoped to fitted faces on purpose. An UNFITTED face is one this policy
/// promises to leave exactly as it found it, and Monocraft — which has been
/// rendering happily all along — draws `#` a pixel past its advance by design.
/// Holding it to this invariant would mean changing the one face that was
/// already right, which is precisely what the fit must not do.
#[test]
fn no_fitted_game_glyph_can_overrun_its_cell() {
    for font in GAME_FONTS {
        let bytes = game_font_bytes(font.id).expect("registry id resolves");
        if game_face_fit(bytes).and_then(|fit| fit.cell_advance_em).is_none() {
            continue;
        }
        // Several sizes: the fit is an em fraction, the cell an integer, so the
        // rounding is only provably safe if it is checked across sizes.
        for px in [12.0_f32, 14.0, 16.0, 20.0, 28.0] {
            let mut renderer = Renderer::from_bytes(bytes, px, Theme::default())
                .unwrap_or_else(|e| panic!("{}: {e}", font.id));
            let (cell_w, _) = renderer.cell_size();
            for ch in '!'..='~' {
                let key = renderer.glyph_key(ch);
                // Only glyphs the GAME FACE serves. A code point it does not
                // cover (Hylia Serif has no `|`) falls through the ordinary
                // fallback cascade by design — that is the "never tofu" promise —
                // and such a glyph is placed by the fallback pipeline's own
                // centring, not by the game fit.
                if !matches!(
                    key.source,
                    aterm_render::FaceId::Primary | aterm_render::FaceId::GameMix
                ) {
                    continue;
                }
                let img = renderer.glyph_image(key);
                let right = img.xmin() + img.width() as i32;
                assert!(
                    right <= cell_w as i32,
                    "{} at {px}px: {ch:?} ends at {right} past the {cell_w}px cell \
                     — it would paint over the next character",
                    font.id
                );
                assert!(
                    img.xmin() >= 0,
                    "{} at {px}px: {ch:?} starts left of its cell (xmin {})",
                    font.id,
                    img.xmin()
                );
            }
        }
    }
}

/// The monospaced bundled face takes the IDENTITY fit. Minecraft/Monocraft was
/// already correct in a fixed grid — the policy must not touch its size, its
/// cell, or its weight, so the face the user likes cannot regress.
#[test]
fn the_monospaced_game_face_is_left_exactly_alone() {
    let fit = game_face_fit(game_font_bytes("minecraft").unwrap()).expect("bundled face has a fit");
    assert_eq!(fit.cell_advance_em, None, "no cell override");
    assert!((fit.px_scale - 1.0).abs() < f32::EPSILON, "no size change");
    assert!(!fit.embolden, "no weight change");

    // And end to end: the fitted renderer agrees with a raw one, cell for cell.
    let bytes = game_font_bytes("minecraft").unwrap();
    for px in [13.0_f32, 16.0, 22.0] {
        let fitted = Renderer::from_bytes(bytes, px, Theme::default()).unwrap();
        assert_eq!(
            fitted.cell_size().0,
            fitted.cell_geometry(px).0,
            "monospaced cell width is stable at {px}px"
        );
    }
}

/// A face that is NOT bundled never gets re-fitted: the policy keys off the
/// registry bytes, so a user's own proportional font keeps the historical
/// `M`-advance cell and their existing config keeps rendering as it did.
#[test]
fn a_non_bundled_face_is_never_fitted() {
    let dejavu = aterm_render::embedded_font();
    assert!(
        game_face_fit(dejavu).is_none(),
        "the embedded fallback face is not a game font and must not be fitted"
    );
}

/// The proportional faces really do take the fit — a guard against the whole
/// policy silently going inert (e.g. a future asset swap that changes the byte
/// identity the registry lookup depends on).
#[test]
fn proportional_game_faces_are_fitted() {
    for id in ["roblox", "zelda", "mariokart", "animal-crossing"] {
        let fit = game_face_fit(game_font_bytes(id).unwrap()).expect("bundled");
        let em = fit
            .cell_advance_em
            .unwrap_or_else(|| panic!("{id} is proportional and must carry a cell override"));
        // The band's ceiling clears Mario Kart F2, whose advances run past
        // 1 em by DESIGN (median 1.22 em, `M` at 1.56) — a uniformly wide
        // face, not a broken measurement. Anything past it is a real bug.
        assert!(
            (0.5..=1.6).contains(&em),
            "{id}: implausible cell advance {em} em"
        );
        assert!(fit.px_scale < 1.0, "{id}: a fitted face rasterizes smaller");
    }
}

/// The face whose weight NEEDED help carries the flag, and the faces that
/// rasterize heavy do not. This pins the intent recorded in
/// `GameFont::embolden` so a future edit has to argue with a test.
#[test]
fn weight_boost_is_set_exactly_where_it_is_needed() {
    let flag = |id: &str| GAME_FONTS.iter().find(|f| f.id == id).unwrap().embolden;
    assert!(
        flag("animal-crossing"),
        "FinkHeavy thins to hairline strokes at body px"
    );
    assert!(!flag("minecraft"), "already heavy — dilation would fill it");
    assert!(!flag("roblox"), "UltraBold — dilation would close its counters");
    assert!(!flag("mariokart"), "solid lettering — already heavy");
}

/// Every registry face parses and rasterizes a plain ASCII glyph — a corrupt or
/// truncated asset (e.g. a failed download committed by mistake) fails here,
/// never on a user's screen.
#[test]
fn every_bundled_game_face_parses_and_rasterizes() {
    for font in GAME_FONTS {
        let bytes = game_font_bytes(font.id).expect("registry id resolves");
        let renderer = Renderer::from_bytes(bytes, 16.0, Theme::default())
            .unwrap_or_else(|error| panic!("game font {:?} failed to parse: {error}", font.id));
        let (cell_w, cell_h) = renderer.cell_size();
        assert!(
            cell_w > 0 && cell_h > 0,
            "game font {:?} produced empty cell metrics",
            font.id
        );
    }
}

/// The `game:` scheme is exact: every registry id resolves under the scheme,
/// while bare ids, unknown ids, and real family names never match — a system
/// font named like a game cannot be shadowed.
#[test]
fn game_scheme_resolution_is_exact() {
    for font in GAME_FONTS {
        let family = format!("{GAME_FONT_SCHEME}{}", font.id);
        assert!(game_font_for_family(&family).is_some(), "{family} resolves");
        assert!(
            game_font_for_family(font.id).is_none(),
            "bare id {:?} must NOT resolve without the scheme",
            font.id
        );
    }
    assert!(game_font_for_family("game:doom").is_none());
    assert!(game_font_for_family("Menlo").is_none());
}

/// The catalog batch path resolves a `game:` request to the embedded bytes
/// (identity path = the virtual name), so live reload and startup agree.
#[test]
fn catalog_batch_resolves_game_scheme_to_embedded_bytes() {
    let requests = vec!["game:minecraft".to_string()];
    let batch = aterm_render::font_catalog::resolve_and_admit(&requests);
    let asset = batch
        .get(0)
        .expect("entry present")
        .as_ref()
        .expect("game font admitted");
    assert_eq!(asset.path, "game:minecraft");
    assert_eq!(
        asset.bytes.as_slice(),
        game_font_bytes("minecraft").unwrap()
    );
}

/// Startup construction honors the scheme too.
#[test]
fn from_system_with_family_honors_game_scheme() {
    let renderer = Renderer::from_system_with_family(Some("game:zelda"), 16.0, Theme::default())
        .expect("game face constructs");
    assert_eq!(renderer.primary_source_path(), Some("game:zelda"));
}

/// MIX parsing is exact: 1..=3 distinct known ids joined by `+`; anything
/// else (unknown id, duplicate, over the cap, empty) rejects the WHOLE
/// request — never a silent partial mix.
#[test]
fn game_mix_family_parsing_is_exact() {
    let solo = game_font_mix_for_family("game:minecraft").expect("single id");
    assert_eq!(solo.len(), 1);
    let duo = game_font_mix_for_family("game:minecraft+zelda").expect("two ids");
    assert_eq!(duo.len(), 2);
    assert_eq!(duo[0], game_font_bytes("minecraft").unwrap());
    assert_eq!(duo[1], game_font_bytes("zelda").unwrap());
    let trio = game_font_mix_for_family("game:roblox+mariokart+animal-crossing").unwrap();
    assert_eq!(trio.len(), GAME_FONT_MIX_MAX);
    // The single-face view of a mix is its FIRST face (the primary).
    assert_eq!(
        game_font_for_family("game:minecraft+zelda"),
        Some(game_font_bytes("minecraft").unwrap())
    );
    for bad in [
        "game:minecraft+doom",
        "game:minecraft+minecraft",
        "game:roblox+minecraft+zelda+mariokart",
        "game:",
        "game:+",
    ] {
        assert!(game_font_mix_for_family(bad).is_none(), "{bad} must reject");
    }
}

/// The per-character pick is deterministic, in range, and actually SPREADS
/// across the mix (every face of a 3-face mix serves some ASCII letter), so
/// the mix is visible rather than collapsing to one font.
#[test]
fn game_mix_pick_is_deterministic_and_spreads() {
    let mut served = [false; 3];
    for ch in ('!'..='~').chain('A'..='z') {
        let pick = game_mix_face_index(ch, 3);
        assert!(pick < 3);
        assert_eq!(pick, game_mix_face_index(ch, 3), "deterministic");
        served[pick] = true;
    }
    assert_eq!(served, [true; 3], "every mix face serves some ASCII char");
    // A single-face "mix" always picks the primary.
    assert_eq!(game_mix_face_index('x', 1), 0);
}

/// End-to-end: a mixed renderer routes each character to the face its pick
/// names — pick 0 stays on the primary, a covered non-zero pick routes to the
/// GameMix source — and the routing is stable across repeated lookups.
#[test]
fn mixed_renderer_routes_characters_across_faces() {
    let mut renderer = Renderer::from_system_with_family(
        Some("game:minecraft+zelda+roblox"),
        16.0,
        Theme::default(),
    )
    .expect("mix constructs");
    assert_eq!(
        renderer.primary_source_path(),
        Some("game:minecraft+zelda+roblox")
    );
    let mut mixed = 0usize;
    let mut primary = 0usize;
    for ch in 'A'..='z' {
        let key = renderer.glyph_key(ch);
        let again = renderer.glyph_key(ch);
        assert_eq!(key, again, "routing is stable for {ch:?}");
        match key.source {
            aterm_render::FaceId::GameMix => mixed += 1,
            _ => primary += 1,
        }
    }
    assert!(mixed > 0, "some letters render from the extra mix faces");
    assert!(primary > 0, "some letters stay on the primary face");
}
