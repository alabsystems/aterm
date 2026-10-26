// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A variable styled face draws the instance of its role ([`FaceInstance`]), as the
//! primary draws its own, never its `fvar` default.

use super::*;

const WGHT: u32 = u32::from_be_bytes(*b"wght");

/// A variable mono face (`wght` 200..400..700) checked in for tests.
const CASCADIA_VARIABLE: &[u8] = include_bytes!("../assets/test/CascadiaMono-Variable.ttf");

/// The coverage `r` draws for `ch` in `style`, char- and glyph-addressed.
fn drawn(r: &mut Renderer, ch: char, style: StyleBits) -> (Vec<u8>, Vec<u8>) {
    let by_char = if style == StyleBits::REGULAR {
        r.glyph_key(ch)
    } else {
        r.glyph_key_styled(ch, style)
    };
    let by_char = r.glyph_image(by_char).bytes().to_vec();
    r.ensure_styled_faces();
    let gid = match r.run_face_pick(style) {
        FacePick::Styled { slot, .. } => r.styled_faces[slot]
            .as_mut()
            .and_then(|face| face.unicode_gid(ch)),
        _ => r.primary_unicode_gid(ch),
    }
    .expect("the face maps the char");
    let by_gid = r.ligature_key(gid, style);
    (by_char, r.glyph_image(by_gid).bytes().to_vec())
}

/// Both rasterizers this platform has: its native one, then the portable one.
fn on_each_rasterizer(build: impl Fn() -> Renderer, check: impl Fn(&mut Renderer, &str)) {
    let mut native = build();
    check(&mut native, "native raster");
    let mut portable = build();
    portable.debug_force_fontdue();
    check(&mut portable, "portable raster");
}

/// A styled face takes the requests the primary takes: under `font_weight` 300 the
/// italic slot draws what the same face draws as the primary under it, by char and by
/// glyph, and moves again when the request does. Before, a styled face drew its `fvar`
/// default whatever was asked, so the italic stayed at 400 beside a 300 upright.
#[test]
fn a_variable_styled_face_follows_the_weight_asked_of_its_family() {
    let build = || {
        let mut r = Renderer::from_bytes(CASCADIA_VARIABLE, 24.0, Theme::default()).unwrap();
        r.set_styled_face(1, CASCADIA_VARIABLE, 0).unwrap();
        r
    };
    on_each_rasterizer(build, |r, label| {
        let as_primary = |requests: &[(u32, f32)]| {
            let mut alone =
                Renderer::from_bytes(CASCADIA_VARIABLE, 24.0, Theme::default()).unwrap();
            if label == "portable raster" {
                alone.debug_force_fontdue();
            }
            alone.set_font_variations(requests, 0.0);
            let (by_char, _) = drawn(&mut alone, 'M', StyleBits::REGULAR);
            by_char
        };
        let default_weight = as_primary(&[]);
        let (by_char, by_gid) = drawn(r, 'M', StyleBits::ITALIC);
        assert_eq!(by_char, default_weight, "{label}: unasked, as the primary");
        assert_eq!(by_gid, default_weight, "{label}: unasked, by glyph");

        assert!(r.set_font_variations(&[(WGHT, 300.0)], 0.0));
        let light = as_primary(&[(WGHT, 300.0)]);
        assert_ne!(light, default_weight, "{label}: 300 is not 400");
        let (by_char, by_gid) = drawn(r, 'M', StyleBits::ITALIC);
        assert_eq!(by_char, light, "{label}: the italic moves with the weight");
        assert_eq!(by_gid, light, "{label}: by glyph too");
    });
}

/// A bold-italic cell the italic face serves draws that face's bold instance, the
/// weight the primary's bold draws, with no synthetic bold left over: before, it was
/// a dilation of the italic's default instance.
#[test]
fn a_bold_italic_cell_draws_the_italic_faces_bold_instance() {
    let build = || {
        let mut r = Renderer::from_bytes(CASCADIA_VARIABLE, 24.0, Theme::default()).unwrap();
        r.set_styled_face(1, CASCADIA_VARIABLE, 0).unwrap();
        r
    };
    on_each_rasterizer(build, |r, label| {
        let mut alone = Renderer::from_bytes(CASCADIA_VARIABLE, 24.0, Theme::default()).unwrap();
        if label == "portable raster" {
            alone.debug_force_fontdue();
        }
        let (bold, _) = drawn(&mut alone, 'M', StyleBits::BOLD);
        let (regular, _) = drawn(&mut alone, 'M', StyleBits::REGULAR);
        assert_ne!(bold, regular, "{label}: a real bold instance");
        let (by_char, by_gid) = drawn(r, 'M', StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0));
        assert_eq!(by_char, bold, "{label}: the bold instance, by char");
        assert_eq!(by_gid, bold, "{label}: and by glyph");
    });
}

/// A styled face is named for the instance it draws: SF Mono's italic file, whose
/// name table names only its Light Italic default, draws "SF Mono Italic" and "SF Mono
/// Bold Italic" with nothing synthetic, as SF Mono's upright draws "SF Mono Bold".
#[cfg(target_os = "macos")]
#[test]
fn sf_monos_styles_are_named_for_the_instances_they_draw() {
    let path = "/System/Library/Fonts/SFNSMono.ttf";
    let bytes = std::fs::read(path).expect("macOS ships SF Mono");
    let mut r =
        Renderer::from_resolved_font_file(FaceRef::new(path, 0), &bytes, 24.0, Theme::default())
            .expect("SF Mono parses");
    r.seal_admitted_font_sources();
    let faces = r
        .drawn_faces()
        .expect("a sealed generation names its faces");
    let named = |slot: usize| {
        (
            faces.styles[slot].face.as_deref(),
            faces.styles[slot].synthetic,
        )
    };
    assert_eq!(named(0), (Some("SF Mono Bold"), StyleBits::REGULAR));
    assert_eq!(named(1), (Some("SF Mono Italic"), StyleBits::REGULAR));
    assert_eq!(named(2), (Some("SF Mono Bold Italic"), StyleBits::REGULAR));
}

/// SF Mono's italic file is variable as its upright is, both defaulting to Light: its
/// regular weight is its "Regular Italic" named instance (`YAXS` 324.33, `wght` 400),
/// at which it draws as much ink as the upright Regular beside it, and its bold italic
/// as much as the bold. Before, the italic drew its Light default, about 0.8 of the
/// upright's ink, and bold italic dilated that Light.
#[cfg(target_os = "macos")]
#[test]
fn sf_mono_italic_draws_the_weight_of_its_upright() {
    let italic = std::fs::read("/System/Library/Fonts/SFNSMonoItalic.ttf").expect("macOS ships it");
    let probe = variation::probe(&italic, 0).expect("SF Mono's italic is variable");
    let regular = probe.regular_coords.expect("its Regular Italic instance");
    let wght = probe.axes.iter().position(|axis| axis.tag == WGHT).unwrap();
    assert!((regular[wght] - 400.0).abs() < 0.01, "{regular:?}");

    let path = "/System/Library/Fonts/SFNSMono.ttf";
    let bytes = std::fs::read(path).expect("macOS ships SF Mono");
    let build = || {
        Renderer::from_resolved_font_file(FaceRef::new(path, 0), &bytes, 40.0, Theme::default())
            .expect("SF Mono parses")
    };
    on_each_rasterizer(build, |r, label| {
        let ink = |r: &mut Renderer, style: StyleBits| -> f64 {
            "mmnnMM"
                .chars()
                .map(|ch| {
                    let (by_char, _) = drawn(r, ch, style);
                    by_char.iter().map(|&c| f64::from(c)).sum::<f64>()
                })
                .sum()
        };
        let italic = ink(r, StyleBits::ITALIC) / ink(r, StyleBits::REGULAR);
        let bold_italic =
            ink(r, StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0)) / ink(r, StyleBits::BOLD);
        assert!(
            (0.95..1.05).contains(&italic),
            "{label}: italic/regular ink {italic}"
        );
        assert!(
            (0.95..1.05).contains(&bold_italic),
            "{label}: bold italic/bold ink {bold_italic}"
        );
    });
}

/// The font list offers a variable styled face by the instance it draws in that role:
/// SF Mono's italic file is "SF Mono Italic" (its Light Italic default is offered by
/// that instance's own name, `named_instance_tests`), and that name finds the file. A
/// static styled face keeps its own full name.
#[cfg(target_os = "macos")]
#[test]
fn a_variable_italic_is_offered_and_found_by_the_name_it_draws() {
    let files = [
        std::path::PathBuf::from("/System/Library/Fonts/SFNSMono.ttf"),
        std::path::PathBuf::from("/System/Library/Fonts/SFNSMonoItalic.ttf"),
    ];
    let families = font_catalog::monospace_families_in(&files, &[]);
    assert_eq!(
        families.italic.first().map(String::as_str),
        Some("SF Mono Italic")
    );

    let italic = std::fs::read(&files[1]).unwrap();
    let face = aterm_font::Face::parse(&italic, 0).unwrap();
    assert_eq!(
        face_name_match(&face, &normalize_family("SF Mono Italic")),
        Some(NameMatch::Role { bold: false })
    );
    let upright = std::fs::read(&files[0]).unwrap();
    let upright = aterm_font::Face::parse(&upright, 0).unwrap();
    assert_eq!(
        face_name_match(&upright, &normalize_family("SF Mono Italic")),
        None
    );

    let static_italic = include_bytes!("../assets/bundled/JetBrainsMono-Italic.ttf");
    let face = aterm_font::Face::parse(static_italic, 0).unwrap();
    assert!(font_catalog::variable_roles(&face).is_empty());
}
