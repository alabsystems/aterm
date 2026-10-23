// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Font face collection identity tests ([`crate::FaceRef`], [`crate::font_catalog::ttc_of`]).

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
    let bold = drawn(&mut r, 'M', StyleBits::BOLD);
    assert_eq!(r.debug_styled_face_indices(), [Some(0), None, None]);
    let mut standalone_bold = Renderer::from_bytes(JB_BOLD, 24.0, Theme::default()).unwrap();
    assert_eq!(bold, drawn(&mut standalone_bold, 'M', StyleBits::REGULAR));
    let mut first = Renderer::from_face(&collection, 0, 24.0, Theme::default()).unwrap();
    assert_eq!(drawn(&mut first, 'M', StyleBits::REGULAR), bold);
    r.seal_admitted_font_sources();
    let mut rebuilt = r.rebuild_from_admitted(24.0, Theme::default()).unwrap();
    assert_eq!(
        drawn(&mut rebuilt, 'M', StyleBits::REGULAR),
        drawn(&mut standalone, 'M', StyleBits::REGULAR)
    );
}

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
    let mut wrong = Renderer::from_bytes(JB_REGULAR, 24.0, Theme::default()).unwrap();
    wrong.set_synthetic_styles(false);
    wrong.set_bold_face(&collection, 0).unwrap();
    assert_ne!(
        drawn(&mut wrong, 'M', StyleBits::BOLD),
        drawn(&mut bold, 'M', StyleBits::REGULAR)
    );
}

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

    let mut lone = Renderer::from_bytes(JB_REGULAR, 16.0, Theme::default()).unwrap();
    lone.seal_admitted_font_sources();
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
    let rebuilt = injected
        .rebuild_from_admitted(20.0, Theme::default())
        .unwrap();
    assert_eq!(rebuilt.drawn_faces(), injected.drawn_faces());
}
