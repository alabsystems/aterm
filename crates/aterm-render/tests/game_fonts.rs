// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FONT-GAME conformance: every bundled game-title face is a real, parseable
//! font, and the `game:` virtual-family scheme resolves identically through
//! BOTH resolution paths (startup construction and the off-thread catalog).

use aterm_render::{
    GAME_FONT_MIX_MAX, GAME_FONT_SCHEME, GAME_FONTS, Renderer, Theme, game_font_bytes,
    game_font_for_family, game_font_mix_for_family, game_mix_face_index,
};

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
