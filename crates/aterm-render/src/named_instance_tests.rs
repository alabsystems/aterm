// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A font name that names a named instance of a variable face draws that instance
//! ([`NamedInstance`]), as a static face draws itself; a name for a role (`SF Mono`,
//! `SF Mono Italic`, `SF Mono Bold`) draws the role's instance.

use super::*;

const WGHT: u32 = u32::from_be_bytes(*b"wght");

/// A variable mono face (`wght` 200..400..700, named instances ExtraLight 200, Light 300,
/// SemiLight 350, Regular 400, SemiBold 600 and Bold 700) checked in for tests.
const CASCADIA_VARIABLE: &[u8] = include_bytes!("../assets/test/CascadiaMono-Variable.ttf");

/// What a request for `name` matches in `bytes`' first face.
fn matched(bytes: &[u8], name: &str) -> Option<NameMatch> {
    let face = aterm_font::Face::parse(bytes, 0).unwrap();
    face_name_match(&face, &normalize_family(name))
}

/// The one-axis instance at `wght`.
fn at_weight(wght: f32) -> NamedInstance {
    NamedInstance::from_fixed(vec![(wght * 65536.0).round() as i32])
}

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

/// What Cascadia draws for `'M'` as the primary under `requests`, on the native raster
/// or the portable one.
fn primary_at(requests: &[(u32, f32)], portable: bool) -> Vec<u8> {
    let mut r = Renderer::from_bytes(CASCADIA_VARIABLE, 24.0, Theme::default()).unwrap();
    if portable {
        r.debug_force_fontdue();
    }
    r.set_font_variations(requests, 0.0);
    drawn(&mut r, 'M', StyleBits::REGULAR).0
}

/// Both rasterizers this platform has: its native one, then the portable one.
fn on_each_rasterizer(build: impl Fn() -> Renderer, check: impl Fn(&mut Renderer, bool, &str)) {
    let mut native = build();
    check(&mut native, false, "native raster");
    let mut portable = build();
    portable.debug_force_fontdue();
    check(&mut portable, true, "portable raster");
}

/// A name for a named instance chooses it (`Cascadia Mono Light`), and a name for what
/// a role draws chooses none: the family, its regular instance (`Cascadia Mono
/// Regular`) and its bold role (`Cascadia Mono Bold`), which follow the role's weight.
/// Before, every name for the face matched the bare face, so `Cascadia Mono Light` drew
/// Regular.
#[test]
fn a_name_for_a_named_instance_chooses_it_and_a_role_name_chooses_none() {
    assert_eq!(
        matched(CASCADIA_VARIABLE, "Cascadia Mono Light"),
        Some(NameMatch::Face(Some(at_weight(300.0))))
    );
    assert_eq!(
        matched(CASCADIA_VARIABLE, "Cascadia Mono SemiBold"),
        Some(NameMatch::Face(Some(at_weight(600.0))))
    );
    assert_eq!(
        matched(CASCADIA_VARIABLE, "Cascadia Mono Regular"),
        Some(NameMatch::Role { bold: false })
    );
    assert_eq!(
        matched(CASCADIA_VARIABLE, "Cascadia Mono Bold"),
        Some(NameMatch::Role { bold: true })
    );
    assert_eq!(
        matched(CASCADIA_VARIABLE, "Cascadia Mono"),
        Some(NameMatch::Family)
    );
    assert_eq!(matched(CASCADIA_VARIABLE, "Cascadia Mono Heavy"), None);
    let (_, _, instance) = best_named_face(
        CASCADIA_VARIABLE,
        Some(&normalize_family("Cascadia Mono ExtraLight")),
        FaceTarget::for_slot(1),
    )
    .expect("the name finds the face");
    assert_eq!(instance, Some(at_weight(200.0)));
}

/// A styled face its name chose an instance of draws exactly that instance, whatever
/// weight is asked of the family: the italic slot named at Light draws what the face
/// draws as the primary at `wght` 300, and still does under `font_weight` 500, by char
/// and by glyph. The bold slot named at SemiBold draws 600, not the bold role's 700.
#[test]
fn a_named_styled_face_draws_its_instance_not_its_roles() {
    let build = || {
        let mut r = Renderer::from_bytes(CASCADIA_VARIABLE, 24.0, Theme::default()).unwrap();
        r.set_named_styled_face(1, CASCADIA_VARIABLE, 0, Some(at_weight(300.0)))
            .unwrap();
        r.set_named_styled_face(0, CASCADIA_VARIABLE, 0, Some(at_weight(600.0)))
            .unwrap();
        r
    };
    on_each_rasterizer(build, |r, portable, label| {
        let light = primary_at(&[(WGHT, 300.0)], portable);
        let semibold = primary_at(&[(WGHT, 600.0)], portable);
        assert_ne!(light, primary_at(&[], portable), "{label}: 300 is not 400");
        assert_ne!(
            semibold,
            primary_at(&[(WGHT, 700.0)], portable),
            "{label}: 600 is not 700"
        );
        for pass in ["unasked", "under font_weight 500"] {
            let (by_char, by_gid) = drawn(r, 'M', StyleBits::ITALIC);
            assert_eq!(by_char, light, "{label}, {pass}: the italic draws Light");
            assert_eq!(by_gid, light, "{label}, {pass}: by glyph too");
            let (by_char, by_gid) = drawn(r, 'M', StyleBits::BOLD);
            assert_eq!(
                by_char, semibold,
                "{label}, {pass}: the bold draws SemiBold"
            );
            assert_eq!(by_gid, semibold, "{label}, {pass}: by glyph too");
            r.set_font_variations(&[(WGHT, 500.0)], 0.0);
        }
    });
}

/// The injected bold face (`font_family_bold` at startup) named at an instance draws it.
#[test]
fn a_named_injected_bold_draws_its_instance() {
    let build = || {
        let mut r = Renderer::from_bytes(CASCADIA_VARIABLE, 24.0, Theme::default()).unwrap();
        r.set_named_bold_face(CASCADIA_VARIABLE, 0, Some(at_weight(600.0)))
            .unwrap();
        r
    };
    on_each_rasterizer(build, |r, portable, label| {
        let semibold = primary_at(&[(WGHT, 600.0)], portable);
        let by_char = r.glyph_key_styled('M', StyleBits::BOLD);
        assert_eq!(
            r.glyph_image(by_char).bytes(),
            &semibold[..],
            "{label}: the injected bold draws SemiBold"
        );
    });
}

/// A primary its name chose an instance of (`font_family = "Cascadia Mono Light"`)
/// draws it, keeps it under `font_weight` and through a zoom's rebuild, its bold is a
/// real bold a weight class above it, and an automatic italic draws at its weight, as a
/// static `JetBrainsMono-Light` finds its `LightItalic` sibling.
#[test]
fn a_named_primary_draws_its_instance_through_a_rebuild() {
    let build = || {
        let face = FaceRef::new("/no/such/CascadiaMono-Variable.ttf", 0)
            .with_instance(Some(at_weight(300.0)));
        let mut r =
            Renderer::from_resolved_font_file(face, CASCADIA_VARIABLE, 24.0, Theme::default())
                .unwrap();
        r.set_styled_face(1, CASCADIA_VARIABLE, 0).unwrap();
        r
    };
    on_each_rasterizer(build, |r, portable, label| {
        let light = primary_at(&[(WGHT, 300.0)], portable);
        for pass in ["unasked", "under font_weight 500"] {
            assert_eq!(
                drawn(r, 'M', StyleBits::REGULAR).0,
                light,
                "{label}, {pass}"
            );
            let (by_char, by_gid) = drawn(r, 'M', StyleBits::ITALIC);
            assert_eq!(
                by_char, light,
                "{label}, {pass}: the italic at the primary's weight"
            );
            assert_eq!(by_gid, light, "{label}, {pass}: by glyph too");
            r.set_font_variations(&[(WGHT, 500.0)], 0.0);
        }
        let bold = primary_at(&[(WGHT, 700.0)], portable);
        assert_eq!(drawn(r, 'M', StyleBits::BOLD).0, bold, "{label}: bold");
    });
    let mut r = build();
    r.seal_admitted_font_sources();
    let mut rebuilt = r.rebuild_from_admitted(24.0, Theme::default()).unwrap();
    assert_eq!(
        drawn(&mut rebuilt, 'M', StyleBits::REGULAR).0,
        primary_at(&[(WGHT, 300.0)], false),
        "a rebuild keeps the instance"
    );
}

/// Settings names what draws: an automatic style a primary named at an instance moves
/// to another named instance is called by that instance's name (the italic slot under
/// `Cascadia Mono Light` draws, and is called, `Cascadia Mono Light`), and one at its
/// `fvar` default by the name its table gives it (`Cascadia Mono Regular`, an upright
/// file in the slot). Before, the first was called by its role's words.
#[test]
fn an_automatic_style_is_called_by_the_named_instance_it_draws() {
    let named = |pinned: Option<NamedInstance>| {
        let face = FaceRef::new("/no/such/CascadiaMono-Variable.ttf", 0).with_instance(pinned);
        let mut r =
            Renderer::from_resolved_font_file(face, CASCADIA_VARIABLE, 24.0, Theme::default())
                .unwrap();
        r.set_styled_face(1, CASCADIA_VARIABLE, 0).unwrap();
        r.seal_admitted_font_sources();
        r.drawn_faces()
            .expect("a sealed generation names its faces")
            .styles[1]
            .face
            .clone()
    };
    assert_eq!(
        named(Some(at_weight(300.0))).as_deref(),
        Some("Cascadia Mono Light")
    );
    assert_eq!(named(None).as_deref(), Some("Cascadia Mono Regular"));
}

/// A role name chooses no instance in its own style's row, where it follows the role's
/// weight, and its role's instance in another's, where it draws what it names and the
/// row says it is another style: `Cascadia Mono Bold` in the Bold row is the bold role,
/// in the Italic row the 700 instance, which does not suit it. Before, a role name
/// drew the row's role whatever it named, so the Italic row drew Regular and said
/// "Cascadia Mono Bold" with no word of another style.
#[test]
fn a_role_named_in_another_styles_row_draws_what_it_names() {
    let find = |target: FaceTarget| {
        best_named_face(
            CASCADIA_VARIABLE,
            Some(&normalize_family("Cascadia Mono Bold")),
            target,
        )
        .expect("the name finds the face")
        .2
    };
    assert_eq!(find(FaceTarget::for_slot(0)), None);
    let elsewhere = find(FaceTarget::for_slot(1));
    assert_eq!(elsewhere, Some(at_weight(700.0)));
    assert!(!FaceTarget::for_slot(1).suits_face_in(CASCADIA_VARIABLE, 0, elsewhere.as_ref()));
    assert!(FaceTarget::for_slot(0).suits_face_in(CASCADIA_VARIABLE, 0, None));
}

/// Whether a face suits a style row is judged by what it draws there: a variable
/// family named in the Bold row draws its bold role (700) and suits it, as the
/// Cascadia instance named at SemiBold does; one named at Light does not. Before, the
/// face's `fvar` default (400) judged them all, so `font_family_bold = "Cascadia
/// Mono"` was refused as another style while it drew bold.
#[test]
fn a_face_suits_a_style_by_the_weight_it_draws_there() {
    let bold = FaceTarget::for_slot(0);
    assert!(bold.suits_face_in(CASCADIA_VARIABLE, 0, None));
    assert!(bold.suits_face_in(CASCADIA_VARIABLE, 0, Some(&at_weight(600.0))));
    assert!(!bold.suits_face_in(CASCADIA_VARIABLE, 0, Some(&at_weight(300.0))));
    assert!(FaceTarget::REGULAR.suits_face_in(CASCADIA_VARIABLE, 0, Some(&at_weight(300.0))));
    assert!(!FaceTarget::REGULAR.suits_face_in(CASCADIA_VARIABLE, 0, Some(&at_weight(600.0))));
}

/// A private font directory holding `files`, removed when dropped.
struct FontDir(std::path::PathBuf);

impl FontDir {
    fn new(tag: &str, files: &[(&str, &[u8])]) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "aterm-named-instance-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, bytes) in files {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        Self(dir)
    }

    fn files(&self) -> Vec<std::path::PathBuf> {
        let mut files: Vec<_> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        files.sort();
        files
    }
}

impl Drop for FontDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The instance each name a picker offers in style slot `slot` draws, by its file,
/// resolved as the config resolves it (the bounded catalog) and drawn in that slot's
/// role.
fn offered_draws(dir: &FontDir, slot: usize, names: &[String]) -> Vec<(String, VarCoords)> {
    let target = FaceTarget::for_slot(slot);
    let requests: Vec<(String, FaceTarget)> =
        names.iter().map(|name| (name.clone(), target)).collect();
    let batch = font_catalog::resolve_and_admit_in_dirs(
        &requests,
        std::slice::from_ref(&dir.0),
        font_catalog::Limits::default(),
    );
    let host = Renderer::from_bytes(CASCADIA_VARIABLE, 24.0, Theme::default()).unwrap();
    names
        .iter()
        .zip(batch.entries)
        .map(|(name, entry)| {
            let admitted = entry
                .result
                .unwrap_or_else(|error| panic!("{name:?} is offered but not found: {error}"));
            let instance = host.family_face_instance(
                &admitted.bytes,
                admitted.index,
                admitted.instance.as_ref(),
                FaceRole::of_slot(slot),
            );
            (admitted.path, instance.drawn)
        })
        .collect()
}

/// The font list offers a variable family's roles and the named instances no role
/// draws, each where its own weight lists it, and no two names it offers draw the
/// same: Cascadia's Bold row offers `Cascadia Mono Bold` (the bold role) and
/// `Cascadia Mono SemiBold` (its 600 instance), and finds each again at what it names.
#[test]
fn the_font_list_offers_no_two_names_that_draw_the_same() {
    let dir = FontDir::new(
        "cascadia",
        &[("CascadiaMono-Variable.ttf", CASCADIA_VARIABLE)],
    );
    let families = font_catalog::monospace_families_in(&dir.files(), &[]);
    assert_eq!(families.installed, ["Cascadia Mono"], "{families:?}");
    assert_eq!(
        families.bold,
        ["Cascadia Mono Bold", "Cascadia Mono SemiBold"],
        "{families:?}"
    );
    assert!(families.italic.is_empty() && families.bold_italic.is_empty());
    let draws = offered_draws(&dir, 0, &families.bold);
    assert_ne!(draws[0], draws[1], "{draws:?}");
    assert_eq!(draws[0].1.as_deref(), Some(&[(WGHT, 700.0)][..]));
    assert_eq!(draws[1].1.as_deref(), Some(&[(WGHT, 600.0)][..]));
}

/// SF Mono's italic file named at its Light Italic instance (`font_family_italic = "SF
/// Mono Light Italic"`, which the old font list offered) draws Light Italic, about 0.8
/// of the upright Regular's ink as CoreText's own Light Italic is, and is called that;
/// a bold italic cell it serves draws its bold instance. Before, the name drew Regular
/// Italic while Settings said "SF Mono Light Italic".
#[cfg(target_os = "macos")]
#[test]
fn sf_mono_light_italic_by_name_draws_light_italic() {
    let upright = "/System/Library/Fonts/SFNSMono.ttf";
    let italic = std::fs::read("/System/Library/Fonts/SFNSMonoItalic.ttf").expect("macOS ships it");
    let (_, index, instance) = best_named_face(
        &italic,
        Some(&normalize_family("SF Mono Light Italic")),
        FaceTarget::for_slot(1),
    )
    .expect("the name finds SF Mono's italic");
    assert!(instance.is_some(), "the name chooses an instance");
    let bytes = std::fs::read(upright).expect("macOS ships SF Mono");
    let build = || {
        let mut r = Renderer::from_resolved_font_file(
            FaceRef::new(upright, 0),
            &bytes,
            40.0,
            Theme::default(),
        )
        .expect("SF Mono parses");
        r.set_named_styled_face(1, &italic, index, instance.clone())
            .unwrap();
        r
    };
    on_each_rasterizer(build, |r, _, label| {
        let ink = |r: &mut Renderer, style: StyleBits| -> f64 {
            "mmnnMM"
                .chars()
                .map(|ch| {
                    let (by_char, _) = drawn(r, ch, style);
                    by_char.iter().map(|&c| f64::from(c)).sum::<f64>()
                })
                .sum()
        };
        let light = ink(r, StyleBits::ITALIC) / ink(r, StyleBits::REGULAR);
        assert!(
            (0.74..0.86).contains(&light),
            "{label}: Light Italic/Regular ink {light}"
        );
    });
    let mut r = build();
    r.seal_admitted_font_sources();
    let faces = r
        .drawn_faces()
        .expect("a sealed generation names its faces");
    assert_eq!(
        faces.styles[1].face.as_deref(),
        Some("SF Mono Light Italic")
    );
    assert_eq!(faces.styles[1].synthetic, StyleBits::REGULAR);
    assert_eq!(faces.styles[2].face.as_deref(), Some("SF Mono Bold Italic"));
    assert_eq!(faces.styles[2].synthetic, StyleBits::REGULAR);
}

/// SF Mono's font list: each role once (`SF Mono Italic`, `SF Mono Bold`, `SF Mono
/// Bold Italic`) and each other named instance where its weight lists it (`SF Mono
/// Light Italic`, `SF Mono Heavy`), and no two names in a row's list draw the same.
#[cfg(target_os = "macos")]
#[test]
fn sf_monos_font_list_offers_each_instance_once() {
    let dir = FontDir::new(
        "sf-mono",
        &[
            (
                "SFNSMono.ttf",
                &std::fs::read("/System/Library/Fonts/SFNSMono.ttf").unwrap(),
            ),
            (
                "SFNSMonoItalic.ttf",
                &std::fs::read("/System/Library/Fonts/SFNSMonoItalic.ttf").unwrap(),
            ),
        ],
    );
    let families = font_catalog::monospace_families_in(&dir.files(), &[]);
    assert_eq!(families.installed, ["SF Mono"]);
    assert_eq!(families.bold, ["SF Mono Bold", "SF Mono Heavy"]);
    assert_eq!(
        families.italic,
        [
            "SF Mono Italic",
            "SF Mono Light Italic",
            "SF Mono Medium Italic",
            "SF Mono Semibold Italic",
        ]
    );
    assert_eq!(
        families.bold_italic,
        ["SF Mono Bold Italic", "SF Mono Heavy Italic"]
    );
    for (slot, names) in [
        (0, &families.bold),
        (1, &families.italic),
        (2, &families.bold_italic),
    ] {
        let draws = offered_draws(&dir, slot, names);
        for (i, a) in draws.iter().enumerate() {
            for b in &draws[i + 1..] {
                assert_ne!(a, b, "slot {slot}: two names draw the same: {draws:?}");
            }
        }
    }
}

/// A styled face drawn at an instance no `fvar` record names is called by its family and
/// the style it draws, not by the name its table gives the `fvar` default: Cascadia at
/// 650 is "Cascadia Mono Bold" and at 450 "Cascadia Mono", never "Cascadia Mono
/// Regular" (a recorded instance keeps its record's name). Before, every unrecorded
/// instance took the default's name.
#[test]
fn an_instance_no_record_names_is_called_by_the_style_it_draws() {
    let named = |pinned: NamedInstance| {
        let face = FaceRef::new("/no/such/CascadiaMono-Variable.ttf", 0);
        let mut r =
            Renderer::from_resolved_font_file(face, CASCADIA_VARIABLE, 24.0, Theme::default())
                .unwrap();
        r.set_named_styled_face(1, CASCADIA_VARIABLE, 0, Some(pinned))
            .unwrap();
        r.seal_admitted_font_sources();
        r.drawn_faces()
            .expect("a sealed generation names its faces")
            .styles[1]
            .face
            .clone()
    };
    assert_eq!(
        named(at_weight(650.0)).as_deref(),
        Some("Cascadia Mono Bold")
    );
    assert_eq!(named(at_weight(450.0)).as_deref(), Some("Cascadia Mono"));
    assert_eq!(
        named(at_weight(300.0)).as_deref(),
        Some("Cascadia Mono Light"),
        "CONTROL: a recorded instance"
    );
}

/// SF Mono's italic file named "SF Mono Bold Italic" in the Italic row draws its bold
/// role's instance (a weight no `fvar` record holds), and Settings calls it, and the
/// bold italic it serves, "SF Mono Bold Italic". Before, both were called "SF Mono Light
/// Italic", the name its table gives the `fvar` default.
#[cfg(target_os = "macos")]
#[test]
fn sf_mono_bold_italic_in_the_italic_row_is_called_what_it_draws() {
    let upright = "/System/Library/Fonts/SFNSMono.ttf";
    let italic = std::fs::read("/System/Library/Fonts/SFNSMonoItalic.ttf").expect("macOS ships it");
    let (_, index, instance) = best_named_face(
        &italic,
        Some(&normalize_family("SF Mono Bold Italic")),
        FaceTarget::for_slot(1),
    )
    .expect("the name finds SF Mono's italic");
    assert!(
        instance.is_some(),
        "another style's role chooses its instance"
    );
    let bytes = std::fs::read(upright).expect("macOS ships SF Mono");
    let mut r =
        Renderer::from_resolved_font_file(FaceRef::new(upright, 0), &bytes, 40.0, Theme::default())
            .expect("SF Mono parses");
    r.set_named_styled_face(1, &italic, index, instance)
        .unwrap();
    r.seal_admitted_font_sources();
    let faces = r
        .drawn_faces()
        .expect("a sealed generation names its faces");
    assert_eq!(faces.styles[1].face.as_deref(), Some("SF Mono Bold Italic"));
    assert_eq!(faces.styles[2].face.as_deref(), Some("SF Mono Bold Italic"));
}
