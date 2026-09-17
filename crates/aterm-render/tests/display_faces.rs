// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FONT-DISPLAY conformance: every bundled display face is a real, parseable
//! font, and the `display:` virtual-family scheme resolves identically through
//! BOTH resolution paths (startup construction and the off-thread catalog).

use aterm_render::{
    DISPLAY_FACE_LEGACY_IDS, DISPLAY_FACE_MIX_MAX, DISPLAY_FACE_SCHEME, DISPLAY_FACES,
    LEGACY_DISPLAY_FACE_SCHEME, Renderer, Theme, display_face_bytes, display_face_canonical_id,
    display_face_fit, display_face_for_family, display_face_mix_for_family, display_mix_face_index,
};

/// FONT-DISPLAY-FIT, the invariant the whole fit exists to guarantee: NO glyph a
/// FITTED display face can draw is wider than the cell it renders into.
///
/// Violating it is not a cosmetic problem — an overrunning glyph paints over its
/// NEIGHBOUR, which is how `pixel` came out as "m ncraft" in Luckiest Guy
/// (cell from `M` at 0.79 em, `m` drawn at 0.91 em, the `i` buried in the ink).
///
/// Scoped to fitted faces on purpose. An UNFITTED face is one this policy
/// promises to leave exactly as it found it, and Monocraft — which has been
/// rendering happily all along — draws `#` a pixel past its advance by design.
/// Holding it to this invariant would mean changing the one face that was
/// already right, which is precisely what the fit must not do.
#[test]
fn no_fitted_display_glyph_can_overrun_its_cell() {
    for font in DISPLAY_FACES {
        let bytes = display_face_bytes(font.id).expect("registry id resolves");
        if display_face_fit(bytes)
            .and_then(|fit| fit.cell_advance_em)
            .is_none()
        {
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
                // Only glyphs the DISPLAY FACE serves. A code point it does not
                // cover falls through the ordinary fallback cascade by design —
                // that is the "never tofu" promise — and such a glyph is placed
                // by the fallback pipeline's own centring, not by the fit.
                if !matches!(
                    key.source,
                    aterm_render::FaceId::Primary | aterm_render::FaceId::DisplayMix
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

/// The monospaced bundled face takes the IDENTITY fit. Monocraft (`pixel`) was
/// already correct in a fixed grid — the policy must not touch its size, its
/// cell, or its weight, so the face the user likes cannot regress.
#[test]
fn the_monospaced_display_face_is_left_exactly_alone() {
    let fit =
        display_face_fit(display_face_bytes("pixel").unwrap()).expect("bundled face has a fit");
    assert_eq!(fit.cell_advance_em, None, "no cell override");
    assert!((fit.px_scale - 1.0).abs() < f32::EPSILON, "no size change");
    assert!(!fit.embolden, "no weight change");

    // And end to end: the fitted renderer agrees with a raw one, cell for cell.
    let bytes = display_face_bytes("pixel").unwrap();
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
        display_face_fit(dejavu).is_none(),
        "the embedded fallback face is not a display face and must not be fitted"
    );
}

/// The proportional faces really do take the fit — a guard against the whole
/// policy silently going inert (e.g. a future asset swap that changes the byte
/// identity the registry lookup depends on).
#[test]
fn proportional_display_faces_are_fitted() {
    for id in ["chunky", "engraved", "bubble"] {
        let fit = display_face_fit(display_face_bytes(id).unwrap()).expect("bundled");
        let em = fit
            .cell_advance_em
            .unwrap_or_else(|| panic!("{id} is proportional and must carry a cell override"));
        // A plausibility band, not a pinned measurement: a display face's
        // widest printable-ASCII extent lands well inside it, and anything
        // outside is a broken measurement rather than an unusual design.
        assert!(
            (0.5..=1.6).contains(&em),
            "{id}: implausible cell advance {em} em"
        );
        assert!(fit.px_scale < 1.0, "{id}: a fitted face rasterizes smaller");
    }
}

/// The face whose weight NEEDED help carries the flag, and the faces that
/// rasterize heavy do not. This pins the intent recorded in
/// `DisplayFace::embolden` so a future edit has to argue with a test.
#[test]
fn weight_boost_is_set_exactly_where_it_is_needed() {
    let flag = |id: &str| DISPLAY_FACES.iter().find(|f| f.id == id).unwrap().embolden;
    assert!(flag("bubble"), "Chewy thins to hairline strokes at body px");
    assert!(!flag("pixel"), "already heavy — dilation would fill it");
    assert!(
        !flag("chunky"),
        "a heavy poster face — dilation would close its counters"
    );
    assert!(
        !flag("engraved"),
        "an inscriptional serif keeps its contrast"
    );
}

/// Every registry face parses and rasterizes a plain ASCII glyph — a corrupt or
/// truncated asset (e.g. a failed download committed by mistake) fails here,
/// never on a user's screen.
#[test]
fn every_bundled_display_face_parses_and_rasterizes() {
    for font in DISPLAY_FACES {
        let bytes = display_face_bytes(font.id).expect("registry id resolves");
        let renderer = Renderer::from_bytes(bytes, 16.0, Theme::default())
            .unwrap_or_else(|error| panic!("display face {:?} failed to parse: {error}", font.id));
        let (cell_w, cell_h) = renderer.cell_size();
        assert!(
            cell_w > 0 && cell_h > 0,
            "display face {:?} produced empty cell metrics",
            font.id
        );
    }
}

/// The registry names NO game and NO franchise. This is the trademark half of
/// the 2026-08-10 ruling, and it is a test rather than a review note because a
/// review note is exactly what failed to hold the line last time: the ids are
/// the most-copied strings in the feature (configs, docs, bug reports), so a
/// re-introduced `minecraft` would spread before anyone re-read the design.
#[test]
fn no_shipped_id_or_label_names_a_game() {
    // The five franchises the pre-rename entries named, plus the two remaining
    // trademarked words those ids were built from.
    let forbidden = [
        "minecraft",
        "roblox",
        "zelda",
        "mario",
        "kart",
        "nintendo",
        "mojang",
        "crossing",
    ];
    for face in DISPLAY_FACES {
        for field in [face.id, face.label, face.face] {
            let lowered = field.to_lowercase();
            for word in forbidden {
                assert!(
                    !lowered.contains(word),
                    "shipped display-face text {field:?} names {word:?}"
                );
            }
        }
    }
}

/// The `display:` scheme is exact: every registry id resolves under the scheme,
/// while bare ids, unknown ids, and real family names never match — an installed
/// font named like a face cannot be shadowed.
#[test]
fn display_scheme_resolution_is_exact() {
    for font in DISPLAY_FACES {
        let family = format!("{DISPLAY_FACE_SCHEME}{}", font.id);
        assert!(
            display_face_for_family(&family).is_some(),
            "{family} resolves"
        );
        assert!(
            display_face_for_family(font.id).is_none(),
            "bare id {:?} must NOT resolve without the scheme",
            font.id
        );
    }
    assert!(display_face_for_family("display:doom").is_none());
    assert!(display_face_for_family("Menlo").is_none());
}

/// MIGRATION: the pre-rename scheme and ids still resolve, to the SAME bytes the
/// new spelling reaches. A theme or config written before the rename keeps
/// rendering the face it asked for instead of silently reverting.
///
/// `mariokart` is the deliberate exception, and it is asserted rather than
/// omitted: its face carried no redistribution grant and has no substitute, so
/// it must resolve to NOTHING — the caller then falls back to the primary font,
/// which is a visible change the user was warned about, not a crash.
#[test]
fn the_legacy_scheme_and_ids_resolve_to_the_renamed_faces() {
    for &(legacy, current) in DISPLAY_FACE_LEGACY_IDS {
        let Some(current) = current else {
            assert_eq!(
                display_face_canonical_id(legacy),
                None,
                "{legacy} is retired"
            );
            assert_eq!(display_face_bytes(legacy), None, "{legacy} ships no bytes");
            assert!(
                display_face_for_family(&format!("{DISPLAY_FACE_SCHEME}{legacy}")).is_none(),
                "{legacy} must not resolve under the scheme either"
            );
            continue;
        };
        assert_eq!(display_face_canonical_id(legacy), Some(current));
        assert_eq!(
            display_face_bytes(legacy),
            display_face_bytes(current),
            "{legacy} must reach the same bytes as {current}"
        );
        // Both schemes, both spellings: four ways to name one face.
        for family in [
            format!("{DISPLAY_FACE_SCHEME}{legacy}"),
            format!("{DISPLAY_FACE_SCHEME}{current}"),
            format!("{LEGACY_DISPLAY_FACE_SCHEME}{legacy}"),
            format!("{LEGACY_DISPLAY_FACE_SCHEME}{current}"),
        ] {
            assert_eq!(
                display_face_for_family(&family),
                display_face_bytes(current),
                "{family} must resolve to {current}"
            );
        }
    }
    // Every legacy id is retired: none may still be a live registry id, or the
    // rename would be half-done and both spellings would be "current".
    for &(legacy, _) in DISPLAY_FACE_LEGACY_IDS {
        assert!(
            !DISPLAY_FACES.iter().any(|face| face.id == legacy),
            "{legacy} is still a shipped id"
        );
    }
}

/// The catalog batch path resolves a `display:` request to the embedded bytes
/// (identity path = the virtual name), so live reload and startup agree.
#[test]
fn catalog_batch_resolves_display_scheme_to_embedded_bytes() {
    let requests = vec!["display:pixel".to_string()];
    let batch = aterm_render::font_catalog::resolve_and_admit(&requests);
    let asset = batch
        .get(0)
        .expect("entry present")
        .as_ref()
        .expect("display face admitted");
    assert_eq!(asset.path, "display:pixel");
    assert_eq!(asset.bytes.as_slice(), display_face_bytes("pixel").unwrap());
}

/// Startup construction honors the scheme too.
#[test]
fn from_system_with_family_honors_display_scheme() {
    let renderer =
        Renderer::from_system_with_family(Some("display:engraved"), 16.0, Theme::default())
            .expect("display face constructs");
    assert_eq!(renderer.primary_source_path(), Some("display:engraved"));
}

/// MIX parsing is exact: 1..=3 distinct known ids joined by `+`; anything
/// else (unknown id, duplicate, over the cap, empty) rejects the WHOLE
/// request — never a silent partial mix.
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
    // The single-face view of a mix is its FIRST face (the primary).
    assert_eq!(
        display_face_for_family("display:pixel+engraved"),
        Some(display_face_bytes("pixel").unwrap())
    );
    for bad in [
        "display:pixel+doom",
        "display:pixel+pixel",
        // One face under two spellings is still one face twice.
        "display:pixel+minecraft",
        "display:chunky+pixel+engraved+bubble",
        "display:",
        "display:+",
        "game:",
    ] {
        assert!(
            display_face_mix_for_family(bad).is_none(),
            "{bad} must reject"
        );
    }
}

/// The per-character pick is deterministic, in range, and actually SPREADS
/// across the mix (every face of a 3-face mix serves some ASCII letter), so
/// the mix is visible rather than collapsing to one font.
#[test]
fn display_mix_pick_is_deterministic_and_spreads() {
    let mut served = [false; 3];
    for ch in ('!'..='~').chain('A'..='z') {
        let pick = display_mix_face_index(ch, 3);
        assert!(pick < 3);
        assert_eq!(pick, display_mix_face_index(ch, 3), "deterministic");
        served[pick] = true;
    }
    assert_eq!(served, [true; 3], "every mix face serves some ASCII char");
    // A single-face "mix" always picks the primary.
    assert_eq!(display_mix_face_index('x', 1), 0);
}

/// End-to-end: a mixed renderer routes each character to the face its pick
/// names — pick 0 stays on the primary, a covered non-zero pick routes to the
/// DisplayMix source — and the routing is stable across repeated lookups.
#[test]
fn mixed_renderer_routes_characters_across_faces() {
    let mut renderer = Renderer::from_system_with_family(
        Some("display:pixel+engraved+chunky"),
        16.0,
        Theme::default(),
    )
    .expect("mix constructs");
    assert_eq!(
        renderer.primary_source_path(),
        Some("display:pixel+engraved+chunky")
    );
    let mut mixed = 0usize;
    let mut primary = 0usize;
    let (cell_w, _) = renderer.cell_size();
    for ch in 'A'..='z' {
        let key = renderer.glyph_key(ch);
        let again = renderer.glyph_key(ch);
        assert_eq!(key, again, "routing is stable for {ch:?}");
        match key.source {
            aterm_render::FaceId::DisplayMix => {
                mixed += 1;
                assert_eq!(key.cell_span, 1);
                let image = renderer.glyph_image(key);
                assert_eq!(
                    image.advance(),
                    cell_w as f32,
                    "DisplayMix has one placement owner: fallback harmony"
                );
                assert!(
                    image.xmin() >= 0 && image.xmin() + image.width() as i32 <= cell_w as i32,
                    "DisplayMix {ch:?} escaped after its single harmony placement"
                );
            }
            _ => primary += 1,
        }
    }
    assert!(mixed > 0, "some letters render from the extra mix faces");
    assert!(primary > 0, "some letters stay on the primary face");
}

/// THE LICENCE GATE, as a test as well as a build failure.
///
/// `build.rs` already refuses to compile the crate when an asset in
/// `assets/game/` has no sibling `<stem>.LICENSE.txt` naming an OFL / Apache /
/// MIT grant. This asserts the property the gate defends from the other side:
/// every face the REGISTRY ships is one whose notice is in the tree and names a
/// grant. Four faces with no grant reached a release branch once because
/// catching them depended on a human opening four files.
#[test]
fn every_registry_face_has_an_open_licence_notice() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/game");
    let mut notices = 0usize;
    for entry in std::fs::read_dir(&dir).expect("asset dir readable") {
        let path = entry.expect("readable entry").path();
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        if !name.ends_with(".LICENSE.txt") {
            let stem = name
                .rsplit_once('.')
                .map_or(name.clone(), |(s, _)| s.into());
            assert!(
                path.with_file_name(format!("{stem}.LICENSE.txt")).is_file(),
                "{name} ships with no sibling licence notice"
            );
            continue;
        }
        notices += 1;
        let text = std::fs::read_to_string(&path)
            .expect("notice readable")
            .to_lowercase();
        assert!(
            ["open font license", "apache license", "mit license"]
                .iter()
                .any(|grant| text.contains(grant)),
            "{name} names no OFL / Apache / MIT grant"
        );
    }
    assert_eq!(
        notices,
        DISPLAY_FACES.len(),
        "one notice per shipped face — an orphan notice means an asset was \
         deleted without its licence, or vice versa"
    );
}

/// THE FIT IS A PROPERTY OF THE FONT, NOT OF THE ALLOCATION. `display_face_fit`
/// identified a bundled face with `std::ptr::eq` against the `DISPLAY_FACES`
/// statics, so it recognised only the ONE copy of the bytes the registry itself
/// holds. Every rebuild path rebuilds from a copy — `rebuild_from_admitted` goes
/// through `shared_parsed_face`'s `Arc::from(bytes)`, `fork_semantic_surface`
/// calls it, and the config worker copies twice more — so each silently dropped
/// the WHOLE policy: px_scale, widest-advance cell, ink centring, embolden
/// headroom.
///
/// It was reachable in the shipped GUI through `rebuild_backend_with_prepared`,
/// which the config/theme hot-reload calls: a user with `display_font` set got
/// the correct fitted grid at launch and permanently lost it the first time the
/// config file was saved or the theme flipped. Measured for `engraved` at 16 px,
/// the rebuilt cell went (21, 19) -> (15, 22) with a widest printable-ASCII
/// raster of 24 px — 9 px of ink over the cell edge, per cell.
///
/// `proportional_display_faces_are_fitted` above guards the policy going inert
/// for the STATIC bytes. This guards the copies, which is where it actually went.
#[test]
fn the_fit_survives_a_rebuild_and_a_copy_of_the_bytes() {
    let bytes = display_face_bytes("engraved").expect("registry id resolves");

    // The root: identity must be the CONTENT. A byte-for-byte copy is the same
    // font and must take the same policy.
    let copy = bytes.to_vec();
    assert!(
        display_face_fit(copy.as_slice()).is_some(),
        "a byte-for-byte copy of a bundled face is the same font and must keep \
         its fit — matching by pointer made the policy an accident of which \
         allocation the caller happened to hold"
    );

    // And the seam that actually carried the loss into the GUI.
    let mut base = Renderer::from_bytes(bytes, 16.0, Theme::default()).expect("engraved at 16px");
    let before = base.cell_size();
    let _ = base.seal_admitted_font_sources();
    let rebuilt = base
        .rebuild_from_admitted(16.0, Theme::default())
        .expect("a sealed generation rebuilds");
    assert_eq!(
        rebuilt.cell_size(),
        before,
        "`rebuild_from_admitted` promises to preserve the font appearance knobs, \
         and the display fit is one of them"
    );

    let forked = base
        .fork_semantic_surface(16.0, Theme::default())
        .expect("a sealed generation forks");
    assert_eq!(
        forked.cell_size(),
        before,
        "a semantic fork rebuilds through the same seam and must not lose the fit"
    );
}

/// A VARIATION CHANGE IS NOT AN EXCUSE TO FORGET THE FIT. `refresh_variations`
/// re-derived the cell with the bare `cell_w_from_advance` — the one derivation
/// that ignores FONT-DISPLAY-FIT — while every sibling site (the constructor,
/// `set_px`, `activate_px`, both `cell_geometry` arms) goes through
/// `fitted_cell_w`. On a fitted face it therefore threw away the widest-advance
/// cell: measured for `engraved` at 16 px, cell 21 -> 13 against a 22 px widest
/// raster.
///
/// It is reachable on the one generation that carries a live fit — STARTUP:
/// `apply_font_config_to_backend` calls `set_font_variations` immediately after
/// construction, so any non-empty `font_variation`/`font_weight`, or a non-zero
/// `font_weight_dark_nudge`, runs it past its early-out.
///
/// Two assertions, because the defect had two distinct faces: the cell must
/// still clear the ink (the overrun this whole policy exists to prevent), and
/// the PURE read must still agree with paint — `cell_geometry` documents that it
/// "equal[s] exactly what the renderer produces once activated to `px`", and
/// while this was broken it reported the fitted 21 against a painted 13, putting
/// grid sizing, hit-testing and IME positioning 8 px per column out of step.
#[test]
fn a_variation_change_keeps_the_fit_and_the_pure_read_agrees() {
    let mut renderer =
        Renderer::from_configured_font_family("display:engraved", 16.0, Theme::default())
            .expect("the display scheme resolves");
    // A heavier instance than the constructor resolved, so the coords really
    // differ and `refresh_variations` runs past its early-out.
    renderer.set_font_variations(&[(aterm_render::variation::WGHT_TAG, 600.0)], 0.0);

    let (cell_w, cell_h) = renderer.cell_size();
    assert_eq!(
        renderer.cell_geometry(16.0),
        (cell_w, cell_h, renderer.baseline()),
        "the pure read must equal what the renderer actually paints"
    );

    for ch in '!'..='~' {
        let key = renderer.glyph_key(ch);
        if !matches!(
            key.source,
            aterm_render::FaceId::Primary | aterm_render::FaceId::DisplayMix
        ) {
            continue;
        }
        let img = renderer.glyph_image(key);
        let right = img.xmin() + img.width() as i32;
        assert!(
            right <= cell_w as i32,
            "after a variation change {ch:?} ends at {right} past the {cell_w}px \
             cell — it would paint over the next character"
        );
    }
}

/// A FACE SWAP RE-PROBES THE FIT. `set_primary_font` re-probes the new bytes'
/// ligature features, OS/2 typo metrics and decoration tables, but left
/// `display_fit` holding the face that had just been replaced — so a swap
/// carried the previous face's px_scale, widest-advance cell, ink centring and
/// embolden headroom onto bytes they were never measured from.
///
/// Both directions, because the defect is symmetric and each has a distinct
/// failure: leaving a fit behind makes the new face render at 0.87 px in a cell
/// sized for someone else, and failing to pick one up reopens the overrun the
/// policy exists to prevent. The comparison is against a renderer CONSTRUCTED
/// from the same bytes, which is the definition of right here — a swap must land
/// exactly where a fresh construction would.
#[test]
fn a_primary_swap_reprobes_the_fit_for_the_new_face() {
    let engraved = display_face_bytes("engraved").expect("registry id resolves");
    let plain = aterm_render::embedded_font();

    let fresh_fitted = Renderer::from_bytes(engraved, 16.0, Theme::default())
        .expect("engraved")
        .cell_size();
    let fresh_plain = Renderer::from_bytes(plain, 16.0, Theme::default())
        .expect("dejavu")
        .cell_size();
    assert_ne!(
        fresh_fitted, fresh_plain,
        "the two faces must differ for this test to be able to fail"
    );

    let mut fitted_to_plain =
        Renderer::from_bytes(engraved, 16.0, Theme::default()).expect("engraved");
    fitted_to_plain
        .set_primary_font(plain)
        .expect("the embedded face installs");
    assert_eq!(
        fitted_to_plain.cell_size(),
        fresh_plain,
        "swapping AWAY from a fitted face must drop its fit — an ordinary user \
         font is one this policy promises to leave exactly as it found it"
    );

    let mut plain_to_fitted = Renderer::from_bytes(plain, 16.0, Theme::default()).expect("dejavu");
    plain_to_fitted
        .set_primary_font(engraved)
        .expect("the display face installs");
    assert_eq!(
        plain_to_fitted.cell_size(),
        fresh_fitted,
        "swapping TO a fitted face must pick its fit up — a swap has to land \
         where a fresh construction from the same bytes would"
    );
}

/// A `display_font` VALUE IS A HUMAN-TYPED CONFIG STRING, so it folds case like
/// every other one. The comparison used to be exact, and the failure was silent
/// in the worst way: an unrecognised id is not an error — it falls through to
/// ordinary `font_family` resolution — so `display_font = "Pixel"` did not warn,
/// did not fail, and did not apply. The setting simply had no effect, which is
/// indistinguishable from never having written it.
///
/// Legacy ids fold too: they are the migration path for configs written a year
/// ago, which is exactly where an unconventional spelling survives.
///
/// The negative control is in the same test: folding must not start ACCEPTING
/// things, only spelling the same id differently.
#[test]
fn a_display_face_id_folds_ascii_case_like_every_other_config_value() {
    for (typed, want) in [
        ("pixel", "pixel"),
        ("Pixel", "pixel"),
        ("PIXEL", "pixel"),
        ("  Engraved  ", "engraved"),
        // A legacy game id, canonicalized to its successor.
        ("Minecraft", "pixel"),
        ("ZELDA", "engraved"),
    ] {
        assert_eq!(
            display_face_canonical_id(typed),
            Some(want),
            "{typed:?} names the {want} face"
        );
    }

    // NEGATIVE CONTROL: case folding must not widen what is accepted.
    for nonsense in ["pixelated", "pix el", "", "mariokart", "MARIOKART"] {
        assert_eq!(
            display_face_canonical_id(nonsense),
            None,
            "{nonsense:?} names no face (mariokart is the retired id with no \
             successor, and folding must not resurrect it)"
        );
    }
}
