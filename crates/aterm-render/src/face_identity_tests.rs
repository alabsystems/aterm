// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A font is a FACE — a file and the face's collection index — everywhere a
//! renderer loads one ([`crate::FaceRef`]). Collections are built from the
//! bundled single-face JetBrains Mono files ([`crate::font_catalog::ttc_of`])
//! in the shapes Apple ships: `PTMono.ttc` keeps its Bold at face 0 and its
//! Regular at face 1.

use crate::font_catalog::ttc_of;
use crate::{FaceRef, Renderer, StyleBits, Theme};

const JB_REGULAR: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Regular.ttf");
const JB_BOLD: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Bold.ttf");
const JB_ITALIC: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Italic.ttf");
const JB_BOLD_ITALIC: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-BoldItalic.ttf");

/// The coverage `r` draws for `ch` in `style`.
fn drawn(r: &mut Renderer, ch: char, style: StyleBits) -> Vec<u8> {
    let key = if style == StyleBits::REGULAR {
        r.glyph_key(ch)
    } else {
        r.glyph_key_styled(ch, style)
    };
    r.glyph_image(key).bytes().to_vec()
}

/// A primary inside a collection draws ITS face, not the collection's first:
/// `PT Mono` (face 1) draws Regular although face 0 is Bold, and that Bold —
/// another face of the primary's family in the same file — serves bold
/// cells. Before, every reader of the primary opened face 0, so plain text
/// drew bold and BOLD could not differ from it.
#[test]
fn a_primary_inside_a_collection_draws_its_own_face_and_its_sibling_bold() {
    let collection = ttc_of(&[JB_BOLD, JB_REGULAR]);
    let mut r = Renderer::from_resolved_font_file(
        FaceRef::new("/nonexistent/Mono.ttc", 1),
        &collection,
        24.0,
        Theme::default(),
    )
    .expect("face 1 parses");
    r.set_synthetic_styles(false);
    assert_eq!(r.chrome_primary_face().map(|(_, index)| index), Some(1));
    let mut standalone = Renderer::from_bytes(JB_REGULAR, 24.0, Theme::default()).unwrap();
    assert_eq!(
        drawn(&mut r, 'M', StyleBits::REGULAR),
        drawn(&mut standalone, 'M', StyleBits::REGULAR),
        "plain text is the collection's Regular face"
    );
    // The family's Bold — face 0 — is the bold slot, and only it.
    let bold = drawn(&mut r, 'M', StyleBits::BOLD);
    assert_eq!(r.debug_styled_face_indices(), [Some(0), None, None]);
    let mut standalone_bold = Renderer::from_bytes(JB_BOLD, 24.0, Theme::default()).unwrap();
    assert_eq!(bold, drawn(&mut standalone_bold, 'M', StyleBits::REGULAR));
    // Negative control: face 0 is the Bold, so a renderer of face 0 draws
    // plain text bold — the bug.
    let mut first = Renderer::from_face(&collection, 0, 24.0, Theme::default()).unwrap();
    assert_eq!(drawn(&mut first, 'M', StyleBits::REGULAR), bold);
    // A rebuild of the sealed generation keeps the face.
    r.seal_admitted_font_sources();
    let mut rebuilt = r.rebuild_from_admitted(24.0, Theme::default()).unwrap();
    assert_eq!(
        drawn(&mut rebuilt, 'M', StyleBits::REGULAR),
        drawn(&mut standalone, 'M', StyleBits::REGULAR)
    );
}

/// An injected styled face inside a collection draws the face asked for:
/// `font_family_italic = "Menlo Italic"` is `Menlo.ttc`'s face 2. Before,
/// every styled setter parsed face 0, so a named Bold drew Regular.
#[test]
fn an_injected_styled_face_inside_a_collection_draws_that_face() {
    let collection = ttc_of(&[JB_REGULAR, JB_BOLD, JB_ITALIC]);
    let mut r = Renderer::from_bytes(JB_REGULAR, 24.0, Theme::default()).unwrap();
    r.set_synthetic_styles(false);
    r.set_bold_face(&collection, 1).expect("bold face installs");
    r.set_styled_face(1, &collection, 2)
        .expect("italic face installs");
    let mut bold = Renderer::from_bytes(JB_BOLD, 24.0, Theme::default()).unwrap();
    let mut italic = Renderer::from_bytes(JB_ITALIC, 24.0, Theme::default()).unwrap();
    assert_eq!(
        drawn(&mut r, 'M', StyleBits::BOLD),
        drawn(&mut bold, 'M', StyleBits::REGULAR)
    );
    assert_eq!(
        drawn(&mut r, 'M', StyleBits::ITALIC),
        drawn(&mut italic, 'M', StyleBits::REGULAR)
    );
    // Negative control: face 0 of the same bytes is the Regular.
    let mut wrong = Renderer::from_bytes(JB_REGULAR, 24.0, Theme::default()).unwrap();
    wrong.set_synthetic_styles(false);
    wrong.set_bold_face(&collection, 0).unwrap();
    assert_ne!(
        drawn(&mut wrong, 'M', StyleBits::BOLD),
        drawn(&mut bold, 'M', StyleBits::REGULAR)
    );
}

/// The collections macOS ships resolve to the faces their names name:
/// `PT Mono` is `PTMono.ttc`'s Regular (face 1, after its Bold), `Menlo
/// Bold` is `Menlo.ttc`'s face 1, and `Courier` asked for its bold slot is
/// `Courier.ttc`'s Bold — a face with no `OS/2` table, whose style is only in
/// `head.macStyle`.
#[cfg(target_os = "macos")]
#[test]
fn macos_collections_resolve_to_the_named_faces() {
    use crate::{FaceTarget, resolve_config_font};
    for (request, target, file, index) in [
        ("PT Mono", FaceTarget::REGULAR, "PTMono.ttc", 1),
        ("PTMono-Bold", FaceTarget::REGULAR, "PTMono.ttc", 0),
        ("Menlo", FaceTarget::REGULAR, "Menlo.ttc", 0),
        ("Menlo Bold", FaceTarget::BOLD, "Menlo.ttc", 1),
        ("Menlo", FaceTarget::ITALIC, "Menlo.ttc", 2),
        ("Courier", FaceTarget::BOLD, "Courier.ttc", 1),
    ] {
        let face = resolve_config_font(request, target)
            .unwrap_or_else(|error| aterm_spec::host::could_not_run(format!("{request}: {error}")));
        assert!(face.path.ends_with(file), "{request}: {face:?}");
        assert_eq!(face.index, index, "{request}: {face:?}");
    }
}

/// A sealed generation reports what each style draws with, by the pick the
/// rasterizer makes: a collection's own Bold, Italic and Bold Italic faces;
/// an injected bold for bold, and for bold italic that bold with a synthetic
/// italic over it; the regular face with a synthetic style when no real face
/// serves it, or drawn regular with synthesis off. An unsealed generation
/// reports nothing, since its styled faces are not discovered yet.
#[test]
fn a_sealed_generation_reports_the_faces_each_style_draws() {
    use crate::{DrawnFaces, StyleDraw};
    let draw = |face: Option<&str>, synthetic: StyleBits| StyleDraw {
        face: face.map(str::to_string),
        synthetic,
    };
    let collection = ttc_of(&[JB_REGULAR, JB_BOLD, JB_ITALIC, JB_BOLD_ITALIC]);
    let mut family = Renderer::from_resolved_font_file(
        FaceRef::new("/nonexistent/Mono.ttc", 0),
        &collection,
        16.0,
        Theme::default(),
    )
    .unwrap();
    assert_eq!(family.drawn_faces(), None, "unsealed: not known yet");
    family.seal_admitted_font_sources();
    let report = family.drawn_faces().expect("sealed").clone();
    assert_eq!(report.regular.as_deref(), Some("JetBrains Mono"));
    assert!(report.regular_has_ligatures, "JetBrains Mono has `calt`");
    assert_eq!(
        report.styles,
        [
            draw(Some("JetBrains Mono Bold"), StyleBits::REGULAR),
            draw(Some("JetBrains Mono Italic"), StyleBits::REGULAR),
            draw(Some("JetBrains Mono Bold Italic"), StyleBits::REGULAR),
        ]
    );

    // A face with no styled siblings: everything synthetic over it, and with
    // synthesis off drawn regular.
    let mut lone = Renderer::from_bytes(JB_REGULAR, 16.0, Theme::default()).unwrap();
    lone.seal_admitted_font_sources();
    // The same face with its `GSUB` record renamed has no ligatures to report
    // (the negative control for `regular_has_ligatures`).
    let mut plain = JB_REGULAR.to_vec();
    let tables = usize::from(u16::from_be_bytes([plain[4], plain[5]]));
    let gsub = (0..tables)
        .map(|index| 12 + 16 * index)
        .find(|record| &plain[*record..*record + 4] == b"GSUB")
        .expect("the fixture has a GSUB table");
    plain[gsub + 3] = b'c';
    let mut plain = Renderer::from_bytes(&plain, 16.0, Theme::default()).unwrap();
    plain.seal_admitted_font_sources();
    assert!(!plain.drawn_faces().unwrap().regular_has_ligatures);
    let bold_italic = StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0);
    assert_eq!(
        lone.drawn_faces().map(|drawn| drawn.styles.clone()),
        Some([
            draw(None, StyleBits::BOLD),
            draw(None, StyleBits::ITALIC),
            draw(None, bold_italic),
        ])
    );
    lone.set_synthetic_styles(false);
    assert!(lone.drawn_faces().is_some_and(|drawn| {
        drawn
            .styles
            .iter()
            .all(|style| style.face.is_none() && style.synthetic == StyleBits::REGULAR)
    }));

    // An injected bold serves bold, and bold italic with a synthetic italic.
    let mut injected = Renderer::from_bytes(JB_REGULAR, 16.0, Theme::default()).unwrap();
    injected.set_bold_face(JB_BOLD, 0).unwrap();
    injected.seal_admitted_font_sources();
    let DrawnFaces { styles, .. } = injected.drawn_faces().unwrap().clone();
    assert_eq!(
        styles[0],
        draw(Some("JetBrains Mono Bold"), StyleBits::REGULAR)
    );
    assert_eq!(
        styles[2],
        draw(Some("JetBrains Mono Bold"), StyleBits::ITALIC)
    );
    // A rebuild of the sealed generation reports the same.
    let rebuilt = injected
        .rebuild_from_admitted(20.0, Theme::default())
        .unwrap();
    assert_eq!(rebuilt.drawn_faces(), injected.drawn_faces());
}
