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
    for ch in 'A'..='z' {
        let key = renderer.glyph_key(ch);
        let again = renderer.glyph_key(ch);
        assert_eq!(key, again, "routing is stable for {ch:?}");
        match key.source {
            aterm_render::FaceId::DisplayMix => mixed += 1,
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
