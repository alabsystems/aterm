// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn family_names_resolve_to_the_bundled_faces_only_when_active() {
    for spelling in [
        "JetBrains Mono",
        "jetbrains-mono",
        "JetBrainsMono",
        " JETBRAINS MONO ",
    ] {
        let hit = face_for_family(spelling);
        assert_eq!(hit.is_some(), ACTIVE, "{spelling:?}");
        if let Some((id, _)) = hit {
            assert_eq!(id, JETBRAINS_ID);
        }
    }
    assert_eq!(
        face_for_family("JetBrains Mono Bold Italic").map(|(id, _)| id),
        ACTIVE.then_some(JETBRAINS_BOLD_ITALIC_ID)
    );
    for (spelling, id) in [
        ("Cascadia Mono PL", CASCADIA_ID),
        ("cascadia-mono-pl", CASCADIA_ID),
        ("CascadiaMonoPL", CASCADIA_ID),
        (" CASCADIA MONO PL REGULAR ", CASCADIA_ID),
        ("Cascadia Mono PL Bold", CASCADIA_BOLD_ID),
        ("Cascadia Mono PL Italic", CASCADIA_ITALIC_ID),
        ("Cascadia Mono PL Bold Italic", CASCADIA_BOLD_ITALIC_ID),
        (CASCADIA_ID, CASCADIA_ID),
    ] {
        assert_eq!(
            face_for_family(spelling).map(|(id, _)| id),
            CASCADIA_ACTIVE.then_some(id),
            "{spelling:?}"
        );
    }
    for (spelling, id) in [
        ("Iosevka Term", IOSEVKA_ID),
        ("iosevka-term", IOSEVKA_ID),
        ("IosevkaTerm", IOSEVKA_ID),
        (" IOSEVKA TERM REGULAR ", IOSEVKA_ID),
        ("Iosevka Term Bold", IOSEVKA_BOLD_ID),
        ("Iosevka Term Italic", IOSEVKA_ITALIC_ID),
        ("Iosevka Term Bold Italic", IOSEVKA_BOLD_ITALIC_ID),
        (IOSEVKA_ID, IOSEVKA_ID),
    ] {
        assert_eq!(
            face_for_family(spelling).map(|(id, _)| id),
            IOSEVKA_ACTIVE.then_some(id),
            "{spelling:?}"
        );
    }
    for (spelling, id) in [
        ("Iosevka Term Semibold", IOSEVKA_SEMIBOLD_ID),
        ("iosevka-term-semibold", IOSEVKA_SEMIBOLD_ID),
        ("IosevkaTermSemiBold", IOSEVKA_SEMIBOLD_ID),
        (" IOSEVKA TERM SEMIBOLD REGULAR ", IOSEVKA_SEMIBOLD_ID),
        ("Iosevka Term Semibold Bold", IOSEVKA_SEMIBOLD_BOLD_ID),
        ("Iosevka Term Semibold Italic", IOSEVKA_SEMIBOLD_ITALIC_ID),
        (
            "Iosevka Term Semibold Bold Italic",
            IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID,
        ),
        ("Iosevka Term Extrabold", IOSEVKA_SEMIBOLD_BOLD_ID),
        (
            "Iosevka Term ExtraBold Italic",
            IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID,
        ),
        (IOSEVKA_SEMIBOLD_ID, IOSEVKA_SEMIBOLD_ID),
    ] {
        assert_eq!(
            face_for_family(spelling).map(|(id, _)| id),
            IOSEVKA_SEMIBOLD_ACTIVE.then_some(id),
            "{spelling:?}"
        );
    }
    assert!(face_for_family("Iosevka").is_none());
    assert!(face_for_family("Iosevka Term Medium").is_none());
    assert!(face_for_family("DejaVu Sans Mono").is_none());
    assert!(face_for_family("JetBrains Mono NL").is_none());
    assert!(face_for_family("Cascadia Mono").is_none());
    assert!(face_for_family("Cascadia Code").is_none());
    assert!(face_for_family("Cascadia Code PL").is_none());
    assert!(face_for_family("").is_none());
}

#[test]
fn the_letter_floor_is_the_familys_deepest_latin_letter() {
    let per_em = |id: &str| letter_descent_floor(face_for_id(id).unwrap());
    if IOSEVKA_ACTIVE {
        for id in IOSEVKA_FACES {
            assert_eq!(per_em(id), Some(-0.256), "{id}");
        }
    }
    if IOSEVKA_NL_ACTIVE {
        for id in IOSEVKA_NL_FACES {
            assert_eq!(per_em(id), Some(-0.256), "{id}");
        }
    }
    if IOSEVKA_SEMIBOLD_ACTIVE {
        for id in IOSEVKA_SEMIBOLD_FACES {
            assert_eq!(per_em(id), Some(-0.259), "{id}");
        }
    }
    if !ACTIVE {
        #[cfg(feature = "embedded-font")]
        assert!(letter_descent_floor(crate::embedded_font()).is_none());
        return;
    }
    for id in JETBRAINS_FACES {
        assert_eq!(per_em(id), Some(-0.24), "{id}");
    }
    if CASCADIA_ACTIVE {
        for id in CASCADIA_FACES {
            assert_eq!(per_em(id), Some(-608.0 / 2048.0), "{id}");
        }
    }
    let copy = face_for_id(PRIMARY_ID).unwrap().to_vec();
    assert_eq!(
        letter_descent_floor(&copy),
        letter_descent_floor(face_for_id(PRIMARY_ID).unwrap())
    );
    assert!(letter_descent_floor(face_for_id(DEJAVU_ID).unwrap()).is_none());
    assert!(letter_descent_floor(face_for_id(NOTO_MATH_ID).unwrap()).is_none());
}

#[test]
fn every_bundled_face_parses_and_each_family_is_a_real_monospace_family() {
    let served: Vec<&str> = ALL_IDS
        .iter()
        .copied()
        .filter(|id| face_for_id(id).is_some())
        .collect();
    let expected_count = usize::from(ACTIVE) * 7
        + usize::from(CASCADIA_ACTIVE) * 4
        + usize::from(IOSEVKA_ACTIVE) * 4
        + usize::from(IOSEVKA_NL_ACTIVE) * 4
        + usize::from(IOSEVKA_SEMIBOLD_ACTIVE) * 4;
    assert_eq!(served.len(), expected_count, "{served:?}");
    for id in &served {
        let bytes = face_for_id(id).expect("filtered to served ids");
        let face = aterm_font::Face::parse(bytes, 0).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert!(face.number_of_glyphs() > 100, "{id}");
    }
    type Family = ([&'static str; 4], [&'static str; 4], [(u16, bool); 4]);
    const PLAIN: [(u16, bool); 4] = [(400, false), (700, false), (400, true), (700, true)];
    let mut families: Vec<Family> = Vec::new();
    if ACTIVE {
        families.push((JETBRAINS_FACES, [JETBRAINS_FAMILY; 4], PLAIN));
    }
    if IOSEVKA_ACTIVE {
        families.push((IOSEVKA_FACES, [IOSEVKA_FAMILY; 4], PLAIN));
    }
    if CASCADIA_ACTIVE {
        families.push((CASCADIA_FACES, [CASCADIA_FAMILY; 4], PLAIN));
    }
    if IOSEVKA_NL_ACTIVE {
        // The cut keeps the file's name table: the family is spelled in the identity alone.
        families.push((IOSEVKA_NL_FACES, [IOSEVKA_FAMILY; 4], PLAIN));
    }
    if IOSEVKA_SEMIBOLD_ACTIVE {
        let extrabold = "Iosevka Term Extrabold";
        families.push((
            IOSEVKA_SEMIBOLD_FACES,
            [
                IOSEVKA_SEMIBOLD_FAMILY,
                extrabold,
                IOSEVKA_SEMIBOLD_FAMILY,
                extrabold,
            ],
            [(600, false), (800, false), (600, true), (800, true)],
        ));
    }
    for (ids, names, expected) in families {
        let weights: Vec<(u16, bool)> = ids
            .iter()
            .zip(names)
            .map(|(id, family)| {
                let f = aterm_font::Face::parse(face_for_id(id).unwrap(), 0).unwrap();
                assert!(f.is_monospaced(), "{id} must be monospaced");
                assert!(
                    f.variation_axes().into_iter().next().is_none(),
                    "{id} must be a STATIC face"
                );
                let names = f.names();
                let name = (0..names.len()).find_map(|i| {
                    let n = names.get(i)?;
                    (n.name_id == aterm_font::name_id::FAMILY)
                        .then(|| n.to_string())
                        .flatten()
                });
                assert_eq!(name.as_deref(), Some(family), "{id}: name ID 1");
                (f.weight().to_number(), f.is_italic())
            })
            .collect();
        assert_eq!(weights, expected, "{}", names[0]);
    }
    for id in IOSEVKA_NL_FACES.iter().chain(&IOSEVKA_SEMIBOLD_FACES) {
        if let Some(bytes) = face_for_id(id) {
            assert!(
                !crate::ligature_shaping::font_has_ligature_features(bytes, 0),
                "{id}: the Windows default must carry no ligature feature"
            );
        }
    }
}

#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
use crate::{FaceId, GlyphClass};
#[cfg(feature = "embedded-font")]
use crate::{Renderer, Theme};

/// Whether `id` is a face the Iosevka Term grid treatments reach: the sets Windows has drawn by default, Iosevka Term NL and the Semibold set.
fn windows_iosevka(id: &str) -> bool {
    IOSEVKA_NL_FACES.contains(&id) || IOSEVKA_SEMIBOLD_FACES.contains(&id)
}

#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn default_sealed() -> Renderer {
    let mut r = Renderer::from_system(18.0, Theme::default()).expect("a default face");
    r.seal_admitted_font_sources();
    r
}

/// Iosevka Term as its file is, the face a user gets by name: the glyphs of Windows' default, Iosevka Term NL, with none of the treatments keyed on the default's bytes. It is the CONTROL of each test of one.
#[cfg(all(windows, feature = "embedded-font"))]
fn untreated_iosevka(px: f32) -> Renderer {
    let bytes = face_for_id(IOSEVKA_ID).unwrap();
    assert!(!is_grid_fitted_iosevka(bytes));
    Renderer::from_bytes(bytes, px, Theme::default()).unwrap()
}

#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn resolved_face(r: &mut Renderer, ch: char) -> String {
    let key = r.glyph_key(ch);
    match key.source {
        FaceId::Primary if key.glyph_class == GlyphClass::MonoGid => {
            format!("primary:{}", r.primary_source_path().unwrap_or("-"))
        }
        FaceId::Primary => "NOTDEF".to_string(),
        FaceId::Fallback => format!(
            "fallback:{}",
            r.fallback_pick
                .get(&ch)
                .and_then(|&i| r.fallback_chain.get(i))
                .and_then(|f| f.path.clone())
                .unwrap_or_default()
        ),
        FaceId::SymbolFallback => format!(
            "symbol:{}",
            r.symbol_chain_pick(ch)
                .and_then(|i| r.symbol_chain.get(i))
                .and_then(|f| f.path.clone())
                .unwrap_or_default()
        ),
        other => format!("{other:?}"),
    }
}

#[test]
#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn the_default_primary_is_the_bundled_platform_face() {
    let expected = if cfg!(windows) {
        IOSEVKA_NL_ID
    } else {
        JETBRAINS_ID
    };
    assert_eq!(PRIMARY_ID, expected);
    let r = Renderer::from_system(18.0, Theme::default()).expect("a default face");
    assert_eq!(r.primary_source_path(), Some(expected));
    let (bytes, _) = r.chrome_primary_face().expect("primary bytes");
    assert_eq!(&bytes[..], face_for_id(expected).unwrap());
    let candidates = crate::primary_font_candidate_paths();
    assert_eq!(candidates.first().map(String::as_str), Some(expected));
}

#[test]
#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn a_configured_font_still_wins_over_the_bundled_default() {
    let file = aterm_tempfile::manifest_dir().join("assets/DejaVuSansMono.ttf");
    let path = file.to_str().expect("a UTF-8 package directory");
    let r = Renderer::from_system_with_family(Some(path), 18.0, Theme::default()).unwrap();
    assert_eq!(
        r.primary_source_path(),
        Some(path),
        "a configured path wins"
    );
    let mut families = vec![JETBRAINS_FAMILY];
    if CASCADIA_ACTIVE {
        families.push(CASCADIA_FAMILY);
    }
    if IOSEVKA_SEMIBOLD_ACTIVE {
        families.push(IOSEVKA_SEMIBOLD_FAMILY);
    }
    if IOSEVKA_NL_ACTIVE {
        families.push(IOSEVKA_NL_FAMILY);
    }
    for family in families {
        assert!(
            selectable_families().contains(&family),
            "{family} is bundled but not listed as selectable"
        );
    }
}

#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
const PRIVATE_FONT_HOME: &str = "ATERM_RENDER_TEST_PRIVATE_FONT_HOME";

#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn in_private_font_home(test: &str) -> bool {
    if std::env::var_os(PRIVATE_FONT_HOME).is_some() {
        return true;
    }
    let module = module_path!();
    let path = module
        .split_once("::")
        .map_or_else(|| test.to_owned(), |(_, inner)| format!("{inner}::{test}"));
    let home = crate::test_scratch_dir(&format!("font-home-{test}"));
    let out =
        aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary's own path"))
            .args(["--exact", "--nocapture", "--test-threads=1", &path])
            .env(PRIVATE_FONT_HOME, "1")
            .env("HOME", home.path())
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_DATA_DIRS")
            .output()
            .expect("re-running this test binary on one test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.success() {
        assert!(
            stdout.contains("1 passed"),
            "the child ran no test named {path}\nstdout: {stdout}\nstderr: {stderr}"
        );
        return false;
    }
    let why = stderr.lines().chain(stdout.lines()).find_map(|line| {
        let (_, rest) = line.split_once(aterm_spec::host::COULD_NOT_RUN)?;
        Some(rest.trim_start_matches(|c: char| c == '—' || c.is_whitespace()))
    });
    if let Some(why) = why {
        aterm_spec::host::could_not_run(format!("{why} ({path}, with an empty HOME)"));
    }
    panic!("{path} failed with an empty HOME\nstdout: {stdout}\nstderr: {stderr}");
}

#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn bundled_family_resolves_by_name(
    test: &str,
    family: &str,
    id: &str,
    bold_id: &str,
    cell: (usize, usize),
) {
    if !in_private_font_home(test) {
        return;
    }
    if crate::resolve_font_family(family, crate::FaceTarget::REGULAR).is_some() {
        aterm_spec::host::could_not_run(format!(
            "{family} is installed in this host's system font roots, and an installed \
             copy wins the name"
        ));
    }
    for spelling in [family, id] {
        let r = Renderer::from_system_with_family(Some(spelling), 18.0, Theme::default()).unwrap();
        assert_eq!(r.primary_source_path(), Some(id), "{spelling}");
        assert_eq!(
            crate::resolve_config_font(spelling, crate::FaceTarget::REGULAR)
                .map(|face| face.path)
                .as_deref(),
            Ok(id)
        );
    }
    let batch = crate::font_catalog::resolve_and_admit(&[
        (family.to_string(), crate::FaceTarget::REGULAR),
        (format!("{family} Bold"), crate::FaceTarget::BOLD),
    ]);
    let admitted: Vec<_> = batch
        .entries
        .iter()
        .map(|e| e.result.as_ref().map(|a| a.path.clone()).ok())
        .collect();
    assert_eq!(admitted, [Some(id.to_string()), Some(bold_id.to_string())]);
    assert!(crate::list_fonts().iter().any(|f| f == family), "{family}");
    let info = crate::face_info(family).expect("face_info resolves the bundle");
    let probe = Renderer::from_system_with_family(
        Some(family),
        crate::FaceInfo::PROBE_PX,
        Theme::default(),
    )
    .expect("probe renderer resolves the bundle");
    assert_eq!(info.path, id);
    assert_eq!((info.cell_width, info.cell_height), cell, "{family}");
    assert_eq!(
        (info.cell_width, info.cell_height),
        probe.cell_size(),
        "{family}"
    );
    assert_eq!(info.baseline, probe.baseline(), "{family}");
}

#[test]
#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn jetbrains_mono_resolves_by_name() {
    bundled_family_resolves_by_name(
        "jetbrains_mono_resolves_by_name",
        JETBRAINS_FAMILY,
        JETBRAINS_ID,
        JETBRAINS_BOLD_ID,
        (10, 21),
    );
}

#[test]
#[cfg(all(feature = "embedded-font", windows))]
fn cascadia_mono_pl_resolves_by_name() {
    bundled_family_resolves_by_name(
        "cascadia_mono_pl_resolves_by_name",
        CASCADIA_FAMILY,
        CASCADIA_ID,
        CASCADIA_BOLD_ID,
        (9, 21),
    );
}

#[test]
#[cfg(all(feature = "embedded-font", windows))]
fn iosevka_term_semibold_resolves_by_name() {
    bundled_family_resolves_by_name(
        "iosevka_term_semibold_resolves_by_name",
        IOSEVKA_SEMIBOLD_FAMILY,
        IOSEVKA_SEMIBOLD_ID,
        IOSEVKA_SEMIBOLD_BOLD_ID,
        (8, 20),
    );
}

#[test]
#[cfg(feature = "embedded-font")]
fn iosevka_term_is_selectable_on_every_platform() {
    assert!(selectable_families().contains(&IOSEVKA_FAMILY));
    assert!(crate::list_fonts().iter().any(|f| f == IOSEVKA_FAMILY));
    for id in IOSEVKA_FACES {
        assert!(!yields_private_use(face_for_id(id).unwrap()), "{id}");
    }
}

#[test]
#[cfg(feature = "embedded-font")]
fn iosevka_term_resolves_by_name_on_every_platform() {
    if crate::resolve_font_family(IOSEVKA_FAMILY, crate::FaceTarget::REGULAR).is_some() {
        aterm_spec::host::could_not_run(format!(
            "{IOSEVKA_FAMILY} is installed on this host, and an installed copy wins the name"
        ));
    }
    for spelling in [IOSEVKA_FAMILY, IOSEVKA_ID] {
        assert_eq!(
            crate::resolve_config_font(spelling, crate::FaceTarget::REGULAR)
                .map(|face| face.path)
                .as_deref(),
            Ok(IOSEVKA_ID)
        );
        let mut r =
            Renderer::from_system_with_family(Some(spelling), 18.0, Theme::default()).unwrap();
        r.seal_admitted_font_sources();
        assert_eq!(r.primary_source_path(), Some(IOSEVKA_ID), "{spelling}");
        assert_eq!(r.debug_styled_face_indices(), [Some(0); 3]);
        for (slot, id) in r.styled_faces.iter().zip(&IOSEVKA_FACES[1..]) {
            let face = slot.as_ref().expect("slot filled");
            assert_eq!(&face.bytes[..], face_for_id(id).unwrap(), "{id}");
        }
    }
    let batch = crate::font_catalog::resolve_and_admit(&[
        (IOSEVKA_FAMILY.to_string(), crate::FaceTarget::REGULAR),
        (
            format!("{IOSEVKA_FAMILY} Bold Italic"),
            crate::FaceTarget::BOLD_ITALIC,
        ),
    ]);
    let admitted: Vec<_> = batch
        .entries
        .iter()
        .map(|e| e.result.as_ref().map(|a| a.path.clone()).ok())
        .collect();
    assert_eq!(
        admitted,
        [
            Some(IOSEVKA_ID.to_string()),
            Some(IOSEVKA_BOLD_ITALIC_ID.to_string())
        ]
    );
    let info = crate::face_info(IOSEVKA_FAMILY).expect("face_info resolves the bundle");
    assert_eq!(info.path, IOSEVKA_ID);
    assert_eq!((info.cell_width, info.cell_height), (8, 20));
}

#[cfg(windows)]
const WINDOWS_DEFAULT_PX: f32 = 17.0;
#[cfg(windows)]
const WINDOWS_DEFAULT_LINE_HEIGHT: f32 = 1.1;

#[test]
#[cfg(all(feature = "embedded-font", windows))]
fn cascadia_mono_pl_by_name_at_16_px_keeps_its_9x21_cell() {
    let mut r = cascadia_sealed(16.0);
    r.set_line_height(1.0);
    assert_eq!((r.cell_size(), r.baseline()), ((9, 21), 15));
    assert_eq!(r.debug_styled_face_indices(), [Some(0); 3]);
    for (slot, id) in r.styled_faces.iter().zip(&CASCADIA_FACES[1..]) {
        let face = slot.as_ref().expect("slot filled");
        assert_eq!(&face.bytes[..], face_for_id(id).unwrap(), "{id}");
    }
}

#[cfg(all(feature = "embedded-font", windows))]
fn cascadia(px: f32) -> Renderer {
    let r = Renderer::from_system_with_family(Some(CASCADIA_ID), px, Theme::default())
        .expect("the bundled Cascadia Mono PL");
    assert_eq!(r.primary_source_path(), Some(CASCADIA_ID));
    r
}

#[cfg(all(feature = "embedded-font", windows))]
fn cascadia_sealed(px: f32) -> Renderer {
    let mut r = cascadia(px);
    r.seal_admitted_font_sources();
    r
}

#[test]
#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn the_default_cell_holds_its_letters_and_keeps_a_clear_row_between_lines() {
    use crate::{GlyphImage, StyleBits};
    const STYLES: [StyleBits; 4] = [
        StyleBits::REGULAR,
        StyleBits::BOLD,
        StyleBits::ITALIC,
        StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0),
    ];
    let ink = |r: &mut Renderer, ch: char, style: StyleBits| -> (i32, i32) {
        let baseline = r.baseline();
        let key = r.glyph_key_styled(ch, style);
        let GlyphImage::Mono {
            width,
            height,
            ymin,
            bytes,
            ..
        } = r.glyph_image(key)
        else {
            panic!("{ch} {style:?}: a letter rasterizes as coverage");
        };
        let (w, h) = (*width, *height);
        let seen = |y: &usize| bytes[y * w..(y + 1) * w].iter().any(|&a| a >= 32);
        let first = (0..h).find(seen).expect("a letter has ink");
        let last = (0..h).rev().find(seen).expect("a letter has ink");
        let top = baseline - (ymin + i32::try_from(h).unwrap());
        (
            top + i32::try_from(first).unwrap(),
            top + i32::try_from(last).unwrap() + 1,
        )
    };
    #[cfg(windows)]
    let (default_px, default_line_height) = (WINDOWS_DEFAULT_PX, WINDOWS_DEFAULT_LINE_HEIGHT);
    #[cfg(not(windows))]
    let (default_px, default_line_height) = (16.0, 1.0);
    let hanging = "șțȘȚķļņŗģĢĶĻŅŖşţŞŢçÇąęįųgjpqy";
    for px in 8..=40u8 {
        let mut r = Renderer::from_system(f32::from(px), Theme::default()).unwrap();
        r.set_line_height(default_line_height);
        let cell_h = i32::try_from(r.cell_size().1).unwrap();
        for ch in hanging.chars() {
            for style in STYLES {
                let (_, bottom) = ink(&mut r, ch, style);
                assert!(
                    bottom <= cell_h,
                    "{px} px {ch} {style:?}: ink ends at row {bottom} of a {cell_h}-row cell"
                );
            }
        }
    }
    let clear_rows = |px: f32, line_height: f32| -> i32 {
        let mut r = Renderer::from_system(px, Theme::default()).unwrap();
        r.set_line_height(line_height);
        r.seal_admitted_font_sources();
        let cell_h = i32::try_from(r.cell_size().1).unwrap();
        let (mut lowest, mut highest) = (i32::MIN, i32::MAX);
        for style in STYLES {
            for ch in "gjpqy".chars() {
                lowest = lowest.max(ink(&mut r, ch, style).1);
            }
            for ch in "ŠŽČŘŇĎŤÂÊĚÉÈÅÑÄÖ".chars() {
                highest = highest.min(ink(&mut r, ch, style).0);
            }
        }
        cell_h + highest - lowest
    };
    let clear = clear_rows(default_px, default_line_height);
    assert!(
        clear >= 1,
        "{default_px} px on a {default_line_height} line: {clear} clear rows between a \
         row's descenders and the accents of the row under it"
    );
    #[cfg(windows)]
    {
        assert_eq!(clear, 2, "the measured gap moved");
        let own_line = clear_rows(WINDOWS_DEFAULT_PX, 1.0);
        assert!(
            own_line < 1,
            "CONTROL: on its own 1.0 line the default face kept {own_line} clear rows, so \
             this test cannot tell the line height's row from the face's"
        );
    }
}

#[test]
#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn the_bundled_primary_resolves_real_bold_italic_and_bold_italic_faces() {
    let r = default_sealed();
    assert_eq!(r.debug_styled_face_indices(), [Some(0); 3]);
    let styles: Vec<(u16, bool)> = r
        .styled_faces
        .iter()
        .map(|slot| {
            let face = slot.as_ref().expect("slot filled");
            let parsed = aterm_font::Face::parse(&face.bytes, 0).unwrap();
            (parsed.weight().to_number(), parsed.is_italic())
        })
        .collect();
    assert_eq!(styles, [(700, false), (400, true), (700, true)]);
    for (slot, id) in
        r.styled_faces
            .iter()
            .zip([PRIMARY_BOLD_ID, PRIMARY_ITALIC_ID, PRIMARY_BOLD_ITALIC_ID])
    {
        let face = slot.as_ref().expect("slot filled");
        assert_eq!(&face.bytes[..], face_for_id(id).unwrap(), "{id}");
    }
}

#[test]
#[cfg(all(feature = "embedded-font", windows))]
fn the_windows_default_slots_are_the_regular_and_bold_files_without_their_ligature_features() {
    use crate::no_ligatures::{LIGATURE_FEATURES, TAKEN_OUT, without_ligature_features};
    let r = default_sealed();
    let (primary, _) = r.chrome_primary_face().expect("primary bytes");
    let drawn: Vec<&[u8]> = std::iter::once(&primary[..])
        .chain(
            r.styled_faces
                .iter()
                .map(|slot| &slot.as_ref().expect("slot filled").bytes[..]),
        )
        .collect();
    let files: [&[u8]; 4] = [
        include_bytes!("../assets/bundled/IosevkaTerm-Regular.ttf"),
        include_bytes!("../assets/bundled/IosevkaTerm-Bold.ttf"),
        include_bytes!("../assets/bundled/IosevkaTerm-Italic.ttf"),
        include_bytes!("../assets/bundled/IosevkaTerm-BoldItalic.ttf"),
    ];
    for ((face, file), id) in drawn.into_iter().zip(files).zip(IOSEVKA_NL_FACES) {
        assert_eq!(face, face_for_id(id).unwrap(), "{id}");
        let cut = without_ligature_features(file).unwrap_or_else(|| panic!("{id}: nothing cut"));
        assert_eq!(face, &cut[..], "{id}: the build's cut of the bundled file");
        assert_eq!(face.len(), file.len(), "{id}");
        let moved: Vec<usize> = (0..file.len()).filter(|&at| face[at] != file[at]).collect();
        assert!(!moved.is_empty() && moved.len() <= 4, "{id}: {moved:?}");
        let tag = moved[0]..moved[0] + 4;
        assert!(moved.iter().all(|at| tag.contains(at)), "{id}: {moved:?}");
        // A tag differs from `TAKEN_OUT` in its first byte at least, so `moved[0]` starts it.
        assert_eq!(&face[tag.clone()], &TAKEN_OUT, "{id}");
        assert_eq!(
            &file[tag], b"calt",
            "{id}: the one feature record the cut renames"
        );
        assert!(LIGATURE_FEATURES.contains(b"calt"));
        assert_eq!(
            without_ligature_features(face),
            None,
            "{id}: nothing is left to take out"
        );
        assert!(
            crate::ligature_shaping::font_has_ligature_features(file, 0)
                && !crate::ligature_shaping::font_has_ligature_features(face, 0),
            "{id}"
        );
    }
    assert_eq!(without_ligature_features(b"not a font"), None);
    assert_eq!(without_ligature_features(&files[0][..64]), None);
}
#[test]
#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn the_test_set_resolves_in_the_declared_order_and_never_to_notdef() {
    let mut r = default_sealed();
    let primary = format!("primary:{PRIMARY_ID}");
    let dejavu = format!("fallback:{DEJAVU_ID}");
    let sym2 = format!("symbol:{NOTO_SYMBOLS2_ID}");
    let math = format!("symbol:{NOTO_MATH_ID}");
    #[cfg(windows)]
    let expected: &[(char, &str)] = &[
        ('▶', &primary),
        ('◀', &primary),
        ('●', &primary),
        ('✓', &primary),
        ('❯', &primary),
        ('→', &primary),
        ('≡', &primary),
        ('∞', &primary),
        ('≈', &primary),
        ('◐', &primary),
        ('✗', &primary),
        ('⚠', &primary),
        ('⇒', &primary),
        ('⇐', &primary),
        ('⌘', &primary),
        ('➜', &primary),
        ('∴', &primary),
        ('✔', &primary),
        ('✘', &primary),
        ('↵', &primary),
        ('★', &primary),
        ('⚙', &primary),
        ('✻', "Procedural"),
        ('⏴', &sym2),
        ('⏵', &sym2),
        ('⏶', &sym2),
        ('⏷', &sym2),
        ('⏺', &primary),
        ('⏸', &primary),
        ('⊨', &primary),
        ('⨁', &primary),
        ('⩽', &primary),
        ('⟹', &primary),
        ('☎', &dejavu),
        ('✂', &dejavu),
        ('♔', &dejavu),
        ('⮜', &sym2),
        ('⮞', &sym2),
        ('⛏', &sym2),
        ('ℒ', &math),
        ('⏐', &math),
        ('⨍', &math),
    ];
    #[cfg(not(windows))]
    let expected: &[(char, &str)] = &[
        ('▶', &primary),
        ('◀', &primary),
        ('●', &primary),
        ('✓', &primary),
        ('✗', &primary),
        ('❯', &primary),
        ('→', &primary),
        ('⚠', &primary),
        ('≡', &primary),
        ('∞', &primary),
        ('≈', &primary),
        ('◐', &dejavu),
        ('✔', &dejavu),
        ('✘', &dejavu),
        ('↵', &dejavu),
        ('★', &dejavu),
        ('⚙', &dejavu),
        ('⏴', &sym2),
        ('⏵', &sym2),
        ('⏶', &sym2),
        ('⏷', &sym2),
        ('⏺', &sym2),
        ('⏸', &sym2),
        ('⊨', &math),
        ('⨁', &math),
        ('⩽', &math),
        ('⟹', &math),
        ('ℒ', &math),
    ];
    let mut wrong = Vec::new();
    for &(ch, want) in expected {
        let got = resolved_face(&mut r, ch);
        if got != want {
            wrong.push(format!("{ch} U+{:04X}: want {want}, got {got}", ch as u32));
        }
    }
    assert!(
        wrong.is_empty(),
        "chain order violated:\n{}",
        wrong.join("\n")
    );
    for ch in ['⎿', '⏩', '⠀', '⣿'] {
        let got = resolved_face(&mut r, ch);
        assert_ne!(got, "NOTDEF", "{ch} U+{:04X} is tofu", ch as u32);
        assert!(
            !got.starts_with("fallback:bundled") && !got.starts_with("symbol:bundled"),
            "{ch}: an incidental bundled pick ({got})"
        );
    }
}

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn the_windows_prompt_line_resolves_each_icon_to_the_face_that_owns_it() {
    let mut r = default_sealed();
    for ch in [
        '\u{E0B0}', '\u{E0B1}', '\u{E0B2}', '\u{E0B3}', '\u{E0B4}', '\u{E0B6}',
    ] {
        assert_eq!(
            r.glyph_key(ch).source,
            FaceId::Procedural,
            "U+{:04X}",
            ch as u32
        );
    }
    for ch in ['\u{E0A0}', '\u{E0A1}', '\u{E0A2}', '\u{E0A3}'] {
        let key = r.glyph_key(ch);
        assert_eq!(
            (key.source, key.glyph_class),
            (FaceId::Primary, GlyphClass::MonoGid),
            "U+{:04X}: the default face draws its own",
            ch as u32
        );
    }
    for ch in [
        '\u{E0C0}', '\u{E0C4}', '\u{E0D2}', '\u{F418}', '\u{F07C}', '\u{E718}', '\u{E7A8}',
        '\u{F0E7}', '\u{F120}', '\u{F308}', '\u{E5FF}', '\u{F113}', '\u{E000}', '\u{E200}',
        '\u{F013}', '\u{F301}',
    ] {
        assert_embedded_nerd_lane(&mut r, ch);
    }
}

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn the_default_faces_own_powerline_glyphs_stand_inside_the_windows_cell() {
    use crate::StyleBits;
    let mut r = Renderer::from_system(WINDOWS_DEFAULT_PX, Theme::default()).unwrap();
    r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
    r.seal_admitted_font_sources();
    let (cw, ch) = r.cell_size();
    assert_eq!((cw, ch), (9, 23));
    let baseline = r.baseline();
    for style in [StyleBits::REGULAR, StyleBits::BOLD] {
        for c in ['\u{E0A0}', '\u{E0A1}', '\u{E0A2}', '\u{E0A3}'] {
            let key = r.glyph_key_styled(c, style);
            assert_ne!(key.source, FaceId::RuntimeFallback, "U+{:04X}", c as u32);
            let crate::GlyphImage::Mono {
                width,
                height,
                xmin,
                ymin,
                bytes,
                ..
            } = r.glyph_image(key)
            else {
                panic!("U+{:04X}: a coverage raster", c as u32);
            };
            let (w, h) = (*width, *height);
            let top = baseline - (*ymin + i32::try_from(h).unwrap());
            for y in 0..h {
                for x in 0..w {
                    if bytes[y * w + x] == 0 {
                        continue;
                    }
                    let (px, py) = (
                        *xmin + i32::try_from(x).unwrap(),
                        top + i32::try_from(y).unwrap(),
                    );
                    assert!(
                        (0..i32::try_from(cw).unwrap()).contains(&px)
                            && (0..i32::try_from(ch).unwrap()).contains(&py),
                        "U+{:04X} {style:?}: ink at ({px}, {py}) leaves the {cw}x{ch} cell",
                        c as u32
                    );
                }
            }
        }
    }
}

#[cfg(all(windows, feature = "embedded-font"))]
fn assert_embedded_nerd_lane(r: &mut Renderer, ch: char) {
    use crate::StyleBits;
    assert_eq!(
        r.glyph_key(ch).source,
        FaceId::RuntimeFallback,
        "U+{:04X} ({})",
        ch as u32,
        resolved_face(r, ch)
    );
    let slot = r
        .runtime_fallback
        .embedded_decisions
        .get(&ch)
        .copied()
        .flatten()
        .expect("a sealed generation records the embedded lane");
    assert_eq!(
        r.runtime_fallback.faces[slot].path,
        crate::RuntimeFallback::EMBEDDED_SYMBOLS_PATH,
        "U+{:04X}",
        ch as u32
    );
    for style in [
        StyleBits::BOLD,
        StyleBits::ITALIC,
        StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0),
    ] {
        assert_eq!(
            r.glyph_key_styled(ch, style).source,
            FaceId::RuntimeFallback,
            "U+{:04X} {style:?}",
            ch as u32
        );
    }
}

#[test]
fn exactly_the_cascadia_mono_pl_faces_yield_their_private_use() {
    let mut yielding = Vec::new();
    for &id in ALL_IDS {
        let Some(bytes) = face_for_id(id) else {
            continue;
        };
        let yields = yields_private_use(bytes);
        let copy = bytes.to_vec();
        assert_eq!(
            yields,
            yields_private_use(&copy),
            "{id}: a copy of the bytes is the same face"
        );
        if yields {
            yielding.push(id);
        }
    }
    let expected: &[&str] = if CASCADIA_ACTIVE {
        &PRIVATE_USE_YIELDING_IDS
    } else {
        &[]
    };
    assert_eq!(yielding, expected);
    #[cfg(feature = "embedded-font")]
    assert!(!yields_private_use(crate::embedded_font()));
    #[cfg(feature = "embedded-font")]
    assert!(!yields_private_use(crate::embedded_symbols_font()));
    if let Some(bytes) = face_for_id(CASCADIA_ID) {
        let mut other = bytes.to_vec();
        let last = other.len() - 1;
        other[last] ^= 0xFF;
        assert!(!yields_private_use(&other));
    }
}

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn the_yielded_powerline_glyphs_draw_inside_their_cell_at_16_px() {
    fn boxes(r: &mut Renderer, ch: char) -> ([i32; 4], [i32; 4]) {
        let baseline = r.baseline();
        let key = r.glyph_key(ch);
        let crate::GlyphImage::Mono {
            width,
            height,
            xmin,
            ymin,
            bytes,
            ..
        } = r.glyph_image(key)
        else {
            panic!("U+{:04X}: a coverage raster", ch as u32);
        };
        let (w, h) = (*width, *height);
        let (xmin, top) = (*xmin, baseline - (*ymin + i32::try_from(h).unwrap()));
        let rows: Vec<usize> = (0..h)
            .filter(|y| bytes[y * w..(y + 1) * w].iter().any(|&a| a > 0))
            .collect();
        let cols: Vec<usize> = (0..w)
            .filter(|x| (0..h).any(|y| bytes[y * w + x] > 0))
            .collect();
        let i = |v: usize| i32::try_from(v).unwrap();
        (
            [xmin, top, xmin + i(w), top + i(h)],
            [
                xmin + i(cols[0]),
                top + i(rows[0]),
                xmin + i(*cols.last().unwrap()) + 1,
                top + i(*rows.last().unwrap()) + 1,
            ],
        )
    }
    let mut r = cascadia_sealed(16.0);
    assert_eq!(r.cell_size(), (9, 21));
    assert_eq!(r.baseline(), 15);
    let expected = [
        ('\u{E0A0}', [0, 2, 8, 19]),
        ('\u{E0A2}', [0, 3, 9, 18]),
        ('\u{E0C0}', [0, 6, 8, 15]),
        ('\u{E0D2}', [0, 6, 5, 15]),
    ];
    for (ch, want) in expected {
        assert_eq!(r.glyph_key(ch).source, FaceId::RuntimeFallback);
        let (raster, ink) = boxes(&mut r, ch);
        eprintln!("U+{:04X}: raster {raster:?} ink {ink:?}", ch as u32);
        assert!(
            raster[0] >= 0 && raster[1] >= 0 && raster[2] <= 9 && raster[3] <= 21,
            "U+{:04X}: raster {raster:?} leaves the 9x21 cell",
            ch as u32
        );
        assert_eq!(ink, want, "U+{:04X}: the measured ink box moved", ch as u32);
    }
    let mut closed = cascadia(16.0);
    closed.set_runtime_font_discovery(false);
    closed.seal_admitted_font_sources();
    assert_eq!(closed.glyph_key('\u{E0D2}').source, FaceId::Primary);
    let (_, ink) = boxes(&mut closed, '\u{E0D2}');
    eprintln!("control U+E0D2 (Cascadia Mono PL's own): ink {ink:?}");
    assert_eq!(
        ink,
        [-1, -3, 10, 19],
        "the face's own U+E0D2 moved: this control may control nothing"
    );
}

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn a_face_that_is_not_the_bundled_cascadia_keeps_its_own_private_use_glyphs() {
    use crate::{GlyphClass, StyleBits};
    let branch = '\u{E0A0}';
    let jbm = face_for_id(JETBRAINS_ID).unwrap();
    let cascadia = face_for_id(CASCADIA_ID).unwrap().to_vec();
    let mut r = Renderer::from_bytes(jbm, 16.0, Theme::default()).unwrap();
    r.seal_admitted_font_sources();
    let key = r.glyph_key(branch);
    assert_eq!(
        (key.source, key.glyph_class),
        (FaceId::Primary, GlyphClass::MonoGid),
        "JetBrains Mono maps U+E0A0 and draws it"
    );
    r.set_bold_font(face_for_id(CASCADIA_BOLD_ID).unwrap())
        .unwrap();
    r.set_styled_font_bytes(1, face_for_id(CASCADIA_ITALIC_ID).unwrap())
        .unwrap();
    let bold = r.glyph_key_styled(branch, StyleBits::BOLD);
    assert_eq!(
        bold.source,
        FaceId::Primary,
        "a bold branch is JetBrains Mono's, not the injected Cascadia bold"
    );
    assert_eq!(
        r.glyph_key_styled('a', StyleBits::BOLD).source,
        FaceId::BoldPrimary,
        "the injected bold face still draws its letters"
    );
    assert_eq!(r.styled_face_slot(branch, StyleBits::ITALIC), None);
    assert_eq!(
        r.styled_face_slot('a', StyleBits::ITALIC),
        Some((1, StyleBits::REGULAR))
    );
    r.set_primary_font(&cascadia).unwrap();
    assert_eq!(r.glyph_key(branch).source, FaceId::RuntimeFallback);
    r.set_primary_font(jbm).unwrap();
    assert_eq!(r.glyph_key(branch).source, FaceId::Primary);
}

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn the_yield_waits_for_the_runtime_tier_and_follows_it_open() {
    use crate::{GlyphClass, StyleBits};
    let branch = '\u{E0A0}';
    let mut r = cascadia(16.0);
    r.set_runtime_font_discovery(false);
    r.seal_admitted_font_sources();
    let key = r.glyph_key(branch);
    assert_eq!(
        (key.source, key.glyph_class),
        (FaceId::Primary, GlyphClass::MonoGid)
    );
    assert_eq!(
        r.glyph_key_styled(branch, StyleBits::BOLD).source,
        FaceId::Primary
    );
    assert!(r.set_runtime_font_discovery(true), "the chain widened");
    assert_eq!(r.glyph_key(branch).source, FaceId::RuntimeFallback);
    assert_eq!(
        r.glyph_key_styled(branch, StyleBits::BOLD).source,
        FaceId::RuntimeFallback,
        "the styled memo of the closed chain survived the widening"
    );
}

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn a_yielded_point_breaks_the_shaping_run() {
    use crate::ColumnGlyph;
    use crate::ligature_shaping::{RowShaper, ShapedRun, build_feature_list, engine_features};
    use aterm_core::terminal::Terminal;
    let text = "a\u{E0A0}\u{E0A2}b";
    let cols: u16 = 8;
    let features = engine_features(&build_feature_list(&[], true));
    let face =
        aterm_font::shape::ShapeFace::from_slice(face_for_id(CASCADIA_ID).unwrap(), 0).unwrap();
    let chars: Vec<char> = text.chars().collect();
    let mut buffer = aterm_font::shape::UnicodeBuffer::new();
    let shaped = RowShaper::new(face, &features).shape(text, &chars, false, &mut buffer);
    let Some(ShapedRun::PerColumn(gids)) = shaped else {
        panic!("the control: Cascadia Mono PL changes nothing in {text:?} ({shaped:?})");
    };
    assert!(
        gids[1].is_some(),
        "the control: the branch is not substituted in a run ({gids:?})"
    );

    let plan_of = |r: &mut Renderer, sgr: &str| {
        let mut term = Terminal::new(1, cols);
        term.process(format!("\x1b[?25l{sgr}{text}").as_bytes());
        let input = term.cell_frame(1, usize::from(cols));
        let mut plan = Vec::new();
        r.row_glyph_plan(&input, 0, &[], &mut plan);
        plan
    };
    let per_cell = [ColumnGlyph::PerCell, ColumnGlyph::PerCell];
    let mut r = cascadia_sealed(16.0);
    assert!(
        r.has_ligature_features,
        "Cascadia Mono PL's rows are shaped"
    );
    let plan = plan_of(&mut r, "");
    assert_eq!(plan[1..3], per_cell, "Cascadia Mono PL: {plan:?}");
    for ch in ['\u{E0A0}', '\u{E0A2}'] {
        assert_eq!(r.glyph_key(ch).source, FaceId::RuntimeFallback);
    }

    let jbm = face_for_id(JETBRAINS_ID).unwrap();
    let mut r = Renderer::from_bytes(jbm, 16.0, Theme::default()).unwrap();
    r.seal_admitted_font_sources();
    r.set_bold_font(face_for_id(CASCADIA_BOLD_ID).unwrap())
        .unwrap();
    assert!(r.has_ligature_features, "JetBrains Mono's rows are shaped");
    let plan = plan_of(&mut r, "\x1b[1m");
    let shaped_on = r.run_face_pick(crate::StyleBits::BOLD);
    assert!(
        matches!(shaped_on, crate::FacePick::InjectedBold { .. }),
        "a bold run shapes on the injected Cascadia bold, not {shaped_on:?}"
    );
    assert_eq!(
        plan[1..3],
        per_cell,
        "a bold run on the injected Cascadia bold: {plan:?}"
    );
}

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn the_windows_default_draws_no_programming_ligatures() {
    use crate::ColumnGlyph;
    use crate::ligature_shaping::{RowShaper, build_feature_list, engine_features};
    use aterm_core::terminal::Terminal;
    let text = "=> != === -> <= >=";
    let chars: Vec<char> = text.chars().collect();
    let features = engine_features(&build_feature_list(&[], true));
    let shaped_on = |id: &str| {
        let face = aterm_font::shape::ShapeFace::from_slice(face_for_id(id).unwrap(), 0)
            .expect("the face parses for shaping");
        let mut buffer = aterm_font::shape::UnicodeBuffer::new();
        RowShaper::new(face, &features).shape(text, &chars, false, &mut buffer)
    };
    for id in IOSEVKA_SEMIBOLD_FACES {
        assert_eq!(shaped_on(id), None, "{id} substitutes a glyph in {text:?}");
    }
    for id in IOSEVKA_NL_FACES {
        assert_eq!(shaped_on(id), None, "{id} substitutes a glyph in {text:?}");
    }
    assert!(
        shaped_on(IOSEVKA_ID).is_some(),
        "CONTROL: Iosevka Term, the file the default is cut from, no longer ligates {text:?}"
    );

    let cols = u16::try_from(chars.len()).unwrap();
    let plan_of = |r: &mut Renderer| {
        let mut term = Terminal::new(1, cols);
        term.process(format!("\x1b[?25l{text}").as_bytes());
        let input = term.cell_frame(1, usize::from(cols));
        let mut plan = Vec::new();
        r.row_glyph_plan(&input, 0, &[], &mut plan);
        plan
    };
    let mut r = default_sealed();
    assert!(
        !r.has_ligature_features,
        "the default face advertises liga/calt, so its rows would be shaped"
    );
    let plan = plan_of(&mut r);
    assert_eq!(plan.len(), chars.len());
    assert!(
        plan.iter().all(|c| *c == ColumnGlyph::PerCell),
        "the default face ligates: {plan:?}"
    );
    for &ch in chars.iter().filter(|c| !c.is_whitespace()) {
        let key = r.glyph_key(ch);
        assert_eq!(
            (key.source, key.glyph_class),
            (FaceId::Primary, GlyphClass::MonoGid),
            "{ch}"
        );
    }
    let mut control = untreated_iosevka(WINDOWS_DEFAULT_PX);
    control.seal_admitted_font_sources();
    let plan = plan_of(&mut control);
    assert!(
        plan.iter().any(|c| *c != ColumnGlyph::PerCell),
        "CONTROL: a renderer on Iosevka Term by name planned no ligature: {plan:?}"
    );
}

#[test]
#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn a_bundled_chain_face_never_takes_an_emoji_default_or_private_use_point() {
    let mut r = default_sealed();
    let mut checked = 0usize;
    for &id in broad_fallback_ids().iter().chain(symbol_fallback_ids()) {
        let face = aterm_font::Face::parse(face_for_id(id).unwrap(), 0).unwrap();
        let mut incidental = Vec::new();
        for sub in face.tables().cmap.unwrap().subtables {
            if sub.is_unicode() {
                sub.codepoints(|cp| {
                    if let Some(ch) = char::from_u32(cp)
                        && (aterm_grapheme::is_emoji_presentation(ch)
                            || crate::font_chain::is_private_use(ch))
                    {
                        incidental.push(ch);
                    }
                });
            }
        }
        for ch in incidental {
            let got = resolved_face(&mut r, ch);
            assert!(
                got.starts_with("primary:") || !got.contains(SCHEME),
                "U+{:04X} taken by {got}",
                ch as u32
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "no bundled face maps an incidental point; nothing proven"
    );
}

#[test]
#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn a_resolved_bundled_identity_reads_as_its_compiled_in_bytes() {
    let mut cases = Vec::new();
    if crate::resolve_font_family(JETBRAINS_FAMILY, crate::FaceTarget::REGULAR).is_none() {
        cases.extend([
            ("JetBrains Mono", JETBRAINS_ID),
            ("JetBrains Mono Bold", JETBRAINS_BOLD_ID),
            ("JetBrains Mono Italic", JETBRAINS_ITALIC_ID),
            ("JetBrains Mono Bold Italic", JETBRAINS_BOLD_ITALIC_ID),
        ]);
    }
    if CASCADIA_ACTIVE
        && crate::resolve_font_family(CASCADIA_FAMILY, crate::FaceTarget::REGULAR).is_none()
    {
        cases.extend([
            ("Cascadia Mono PL", CASCADIA_ID),
            ("Cascadia Mono PL Bold", CASCADIA_BOLD_ID),
            ("Cascadia Mono PL Italic", CASCADIA_ITALIC_ID),
            ("Cascadia Mono PL Bold Italic", CASCADIA_BOLD_ITALIC_ID),
        ]);
    }
    if IOSEVKA_SEMIBOLD_ACTIVE
        && crate::resolve_font_family(IOSEVKA_SEMIBOLD_FAMILY, crate::FaceTarget::REGULAR).is_none()
    {
        cases.extend([
            ("Iosevka Term Semibold", IOSEVKA_SEMIBOLD_ID),
            ("Iosevka Term Semibold Bold", IOSEVKA_SEMIBOLD_BOLD_ID),
            ("Iosevka Term Semibold Italic", IOSEVKA_SEMIBOLD_ITALIC_ID),
            (
                "Iosevka Term Semibold Bold Italic",
                IOSEVKA_SEMIBOLD_BOLD_ITALIC_ID,
            ),
        ]);
    }
    for (name, id) in cases {
        let resolved = crate::resolve_config_font(name, crate::FaceTarget::REGULAR)
            .expect(name)
            .path;
        assert_eq!(resolved, id);
        let bytes = crate::read_resolved_font(&resolved).expect(name);
        assert_eq!(&bytes[..], face_for_id(id).unwrap(), "{name}");
        assert!(
            crate::font_file::read_font_file(std::path::Path::new(&resolved)).is_err(),
            "the identity is not a file — reading it as one is the bug"
        );
    }
    let file = aterm_tempfile::manifest_dir().join("assets/DejaVuSansMono.ttf");
    let path = file.to_str().expect("a UTF-8 package directory");
    assert_eq!(
        &crate::read_resolved_font(path).unwrap()[..],
        &std::fs::read(path).unwrap()[..]
    );
}

#[test]
fn the_two_cell_glyphs_are_the_measured_fourteen_and_only_windows_iosevka_sets_fit_them() {
    let two_cell = |id: &str| -> Option<Vec<char>> {
        let face = aterm_font::Face::parse(face_for_id(id)?, 0).unwrap();
        let cell = face.glyph_hor_advance(face.glyph_index('M').unwrap())?;
        let mut out = Vec::new();
        for sub in face.tables().cmap.unwrap().subtables {
            if sub.is_unicode() {
                sub.codepoints(|cp| {
                    if let Some(ch) = char::from_u32(cp)
                        && let Some(g) = face.glyph_index(ch).filter(|g| g.0 != 0)
                        && face.glyph_hor_advance(g).is_some_and(|a| a > cell)
                    {
                        out.push(ch);
                    }
                });
            }
        }
        out.sort_unstable();
        out.dedup();
        Some(out)
    };
    let expected: Vec<char> = (0..=0x10_FFFFu32)
        .filter_map(char::from_u32)
        .filter(|&c| is_two_cell_glyph(c))
        .collect();
    assert_eq!(expected.len(), 14);
    for &ch in &expected {
        assert_eq!(aterm_grapheme::char_width(ch), 1, "{ch}: one grid column");
    }
    for id in IOSEVKA_FACES.iter().chain(&IOSEVKA_SEMIBOLD_FACES) {
        if let Some(found) = two_cell(id) {
            assert_eq!(found, expected, "{id}");
        }
    }
    for id in JETBRAINS_FACES.iter().chain(&CASCADIA_FACES) {
        if let Some(found) = two_cell(id) {
            assert!(found.is_empty(), "{id}: {found:?}");
        }
    }
    for &id in ALL_IDS {
        let Some(bytes) = face_for_id(id) else {
            continue;
        };
        let fits = fits_two_cell_glyphs(bytes);
        assert_eq!(fits, windows_iosevka(id), "{id}");
        let copy = bytes.to_vec();
        assert_eq!(fits_two_cell_glyphs(&copy), fits, "{id}: a copy");
    }
}

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn the_windows_defaults_two_cell_glyphs_are_fitted_into_their_column() {
    use aterm_core::terminal::Terminal;
    let cell_of = |r: &mut Renderer, ch: char| {
        let mut term = Terminal::new(1, 4);
        term.process(format!("\x1b[?25l{ch}x").as_bytes());
        let input = term.cell_frame(1, 4);
        let span = crate::materialized_cell_span(&input.cells[0], 0);
        assert_eq!(span, 1, "{ch}: the grid gives it one column");
        let key = r.resolve_cell_key_for_span(None, &input.cells[0][0], span);
        let img = r.glyph_image(key).clone();
        (
            key,
            img.xmin(),
            img.xmin() + i32::try_from(img.width()).unwrap(),
        )
    };
    let mut r = Renderer::from_system(WINDOWS_DEFAULT_PX, Theme::default()).unwrap();
    r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
    r.seal_admitted_font_sources();
    let cw = i32::try_from(r.cell_size().0).unwrap();
    assert_eq!(cw, 9);
    let two_cell = ['\u{27DD}', '\u{27DE}', '\u{2B33}']
        .into_iter()
        .chain('\u{27F5}'..='\u{27FF}');
    for ch in two_cell {
        let (key, left, right) = cell_of(&mut r, ch);
        assert_eq!(
            (key.source, key.cell_span),
            (FaceId::Primary, 1),
            "U+{:04X}",
            ch as u32
        );
        assert!(
            left >= 0 && right <= cw,
            "U+{:04X}: raster columns {left}..{right} leave the {cw}-px column",
            ch as u32
        );
    }
    let (key, ..) = cell_of(&mut r, '⇒');
    assert_eq!((key.source, key.cell_span), (FaceId::Primary, 0), "⇒");

    let mut control = untreated_iosevka(WINDOWS_DEFAULT_PX);
    control.seal_admitted_font_sources();
    let (key, _, right) = cell_of(&mut control, '⟹');
    assert_eq!((key.source, key.cell_span), (FaceId::Primary, 0));
    assert!(
        right > i32::try_from(control.cell_size().0).unwrap(),
        "CONTROL: Iosevka Term by name's own ⟹ fits its column ({right}), so this test \
         cannot see the overflow it guards against"
    );
}

const IOSEVKA_WIDE_CHARACTERS: [char; 38] = [
    '\u{231A}', '\u{231B}', '\u{2329}', '\u{232A}', '\u{23E9}', '\u{23EA}', '\u{23EB}', '\u{23EC}',
    '\u{23F0}', '\u{23F3}', '\u{25FD}', '\u{25FE}', '\u{2630}', '\u{2631}', '\u{2632}', '\u{2633}',
    '\u{2634}', '\u{2635}', '\u{2636}', '\u{2637}', '\u{268A}', '\u{268B}', '\u{268C}', '\u{268D}',
    '\u{268E}', '\u{268F}', '\u{26A1}', '\u{26AA}', '\u{26AB}', '\u{2705}', '\u{274C}', '\u{274E}',
    '\u{27B0}', '\u{27BF}', '\u{2B1B}', '\u{2B1C}', '\u{2B50}', '\u{2B55}',
];

fn wide_characters_of(id: &str) -> Option<Vec<(char, bool)>> {
    let face = aterm_font::Face::parse(face_for_id(id)?, 0).unwrap();
    let cell = face.glyph_hor_advance(face.glyph_index('M').unwrap())?;
    let mut out = Vec::new();
    for sub in face.tables().cmap.unwrap().subtables {
        if sub.is_unicode() {
            sub.codepoints(|cp| {
                if let Some(ch) = char::from_u32(cp)
                    && aterm_grapheme::char_width(ch) == 2
                    && let Some(g) = face.glyph_index(ch).filter(|g| g.0 != 0)
                {
                    out.push((ch, face.glyph_hor_advance(g) == Some(cell)));
                }
            });
        }
    }
    out.sort_unstable();
    out.dedup();
    Some(out)
}

#[test]
fn the_wide_characters_are_the_measured_thirty_eight_and_only_windows_iosevka_sets_yield_them() {
    let expected: Vec<(char, bool)> = IOSEVKA_WIDE_CHARACTERS
        .iter()
        .map(|&ch| (ch, true))
        .collect();
    let emoji = IOSEVKA_WIDE_CHARACTERS
        .iter()
        .filter(|&&ch| aterm_grapheme::is_emoji_presentation(ch))
        .count();
    assert_eq!(emoji, 22);
    for id in IOSEVKA_FACES.iter().chain(&IOSEVKA_SEMIBOLD_FACES) {
        if let Some(found) = wide_characters_of(id) {
            assert_eq!(found, expected, "{id}");
        }
    }
    for (faces, theirs) in [
        (
            JETBRAINS_FACES,
            ['\u{26A1}', '\u{FE62}', '\u{FF5B}', '\u{FF5D}'],
        ),
        (
            CASCADIA_FACES,
            ['\u{25FD}', '\u{25FE}', '\u{2B1B}', '\u{2B1C}'],
        ),
    ] {
        for id in faces {
            if let Some(found) = wide_characters_of(id) {
                let want: Vec<(char, bool)> = theirs.iter().map(|&ch| (ch, true)).collect();
                assert_eq!(found, want, "{id}");
            }
        }
    }
    for &id in ALL_IDS {
        let Some(bytes) = face_for_id(id) else {
            continue;
        };
        let yields = yields_wide_characters(bytes);
        assert_eq!(yields, windows_iosevka(id), "{id}");
        let copy = bytes.to_vec();
        assert_eq!(yields_wide_characters(&copy), yields, "{id}: a copy");
    }
}

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn the_windows_defaults_wide_characters_draw_centred_across_their_two_columns() {
    use aterm_core::terminal::Terminal;
    let ink_of = |r: &mut Renderer, ch: char| {
        let mut term = Terminal::new(1, 4);
        term.process(format!("\x1b[?25l{ch}x").as_bytes());
        let input = term.cell_frame(1, 4);
        let span = crate::materialized_cell_span(&input.cells[0], 0);
        assert_eq!(span, 2, "{ch}: the grid gives it two columns");
        let key = r.resolve_cell_key_for_span(None, &input.cells[0][0], span);
        let img = r.glyph_image(key).clone();
        let (w, h) = (img.width(), img.height());
        let texel = if matches!(key.glyph_class, GlyphClass::Mono | GlyphClass::MonoGid) {
            1
        } else {
            4
        };
        let inked: Vec<i32> = (0..w)
            .filter(|&x| (0..h).any(|y| img.bytes()[(y * w + x) * texel + texel - 1] >= 32))
            .map(|x| img.xmin() + i32::try_from(x).unwrap())
            .collect();
        let left = *inked.first().expect("the glyph draws ink");
        let right = *inked.last().unwrap() + 1;
        (key, left, right)
    };
    let mut r = Renderer::from_system(WINDOWS_DEFAULT_PX, Theme::default()).unwrap();
    r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
    r.seal_admitted_font_sources();
    assert_eq!(r.primary_source_path(), Some(IOSEVKA_NL_ID));
    let mut cascadia = cascadia_sealed(WINDOWS_DEFAULT_PX);
    let cascadia_primary = format!("primary:{CASCADIA_ID}");
    for ch in IOSEVKA_WIDE_CHARACTERS {
        let got = resolved_face(&mut r, ch);
        assert!(
            !got.starts_with("primary:") && got != "NOTDEF",
            "U+{:04X} {ch}: {got}",
            ch as u32
        );
        let before = resolved_face(&mut cascadia, ch);
        if before != cascadia_primary {
            assert_eq!(got, before, "U+{:04X} {ch}", ch as u32);
        }
    }
    let cw = i32::try_from(r.cell_size().0).unwrap();
    assert_eq!(cw, 9);
    for ch in ['✅', '❌', '⭐', '⚡', '⏳'] {
        let (key, left, right) = ink_of(&mut r, ch);
        assert_ne!(key.source, FaceId::Primary, "{ch}");
        assert!(
            left < cw && right > cw && left >= 0 && right <= 2 * cw,
            "{ch}: ink columns {left}..{right} do not span the two {cw}-px columns"
        );
        assert!(
            (left - (2 * cw - right)).abs() <= 1,
            "{ch}: ink columns {left}..{right} are not centred in the {}-px slot",
            2 * cw
        );
    }

    let mut control = untreated_iosevka(WINDOWS_DEFAULT_PX);
    control.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
    control.seal_admitted_font_sources();
    let (key, left, right) = ink_of(&mut control, '✅');
    assert_eq!(key.source, FaceId::Primary);
    assert!(
        left >= 0 && right <= cw,
        "CONTROL: Iosevka Term by name's own ✅ ({left}..{right}) leaves its left column, \
         so this test cannot see the half-width glyph it guards against"
    );
}

#[cfg(all(windows, feature = "embedded-font"))]
fn ink_of(r: &mut Renderer, ch: char) -> (usize, usize) {
    let key = r.glyph_key(ch);
    let img = r.glyph_image(key);
    let w = img.width();
    let inked = |y: usize| img.bytes()[y * w..(y + 1) * w].iter().any(|&b| b > 40);
    let rows = (0..img.height()).filter(|&y| inked(y)).count();
    (img.bytes().iter().filter(|&&b| b > 40).count(), rows)
}

#[cfg(all(windows, feature = "embedded-font"))]
fn windows_default_sealed() -> Renderer {
    let mut r = Renderer::from_system(WINDOWS_DEFAULT_PX, Theme::default()).unwrap();
    r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
    r.seal_admitted_font_sources();
    assert_eq!(r.primary_source_path(), Some(IOSEVKA_NL_ID));
    r
}

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn the_windows_defaults_spinner_and_footer_glyphs_outweigh_its_asterisk() {
    let mut r = windows_default_sealed();
    let (cw, chh) = r.cell_size();
    let (star, _) = ink_of(&mut r, '*');
    let mut sums = Vec::new();
    for ch in ['✢', '✳', '✶', '✻', '✽'] {
        let key = r.glyph_key(ch);
        assert_eq!(key.source, FaceId::Procedural, "{ch}: drawn from the cell");
        let crate::GlyphImage::Mono { bytes, .. } = r.glyph_image(key).clone() else {
            panic!("{ch}: a coverage mask");
        };
        let inked = bytes.iter().filter(|&&c| c > 0).count();
        let solid = bytes.iter().filter(|&&c| c >= 230).count();
        assert_eq!(bytes.iter().max(), Some(&255), "{ch}: full ink");
        assert!(10 * solid >= 3 * inked, "{ch}: {solid} of {inked} solid");
        sums.push(bytes.iter().map(|&c| u32::from(c)).sum::<u32>());
    }
    let (lo, hi) = (sums.iter().min(), sums.iter().max());
    assert!(
        hi.zip(lo).is_some_and(|(hi, lo)| 2 * hi <= 3 * lo),
        "frame ink {sums:?}"
    );
    for ch in ['✢', '✳', '✶', '✻', '✽', '⏴', '⏵', '⏶', '⏷'] {
        let key = r.glyph_key(ch);
        assert_ne!(key.source, FaceId::Primary, "{ch}: drawn by the primary");
        let img = r.glyph_image(key).clone();
        let top = r.baseline() - img.height() as i32 - img.ymin();
        assert!(
            img.xmin() >= 0
                && img.xmin() as usize + img.width() <= cw
                && top >= 0
                && top as usize + img.height() <= chh,
            "{ch}: leaves its {cw}x{chh} cell"
        );
    }
    for ch in ['✢', '✳', '✶', '✻', '✽'] {
        let (ink, _) = ink_of(&mut r, ch);
        assert!(ink > star, "{ch}: {ink} ink texels, the face's `*` {star}");
    }
    let (spinner, _) = ink_of(&mut r, '✻');
    assert!(
        2 * spinner >= 3 * star,
        "✻: {spinner} ink texels, not half again the `*`'s {star}"
    );
    let (_, rows) = ink_of(&mut r, '⏵');
    assert!(rows >= 9, "⏵ is {rows} rows tall");
}

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn the_windows_default_keeps_its_faces_spinner_frames_where_they_are_crisp() {
    let dejavu = format!("fallback:{DEJAVU_ID}");
    for px in [17.0, 21.0, 26.0, 30.0, 32.0, 34.0, 42.5] {
        let mut r = Renderer::from_system(px, Theme::default()).unwrap();
        r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
        r.seal_admitted_font_sources();
        let (cw, chh) = r.cell_size();
        let crisp = px >= SPINNER_STARS_CRISP_PX;
        for ch in ['✢', '✳', '✶', '✻', '✽'] {
            let face = resolved_face(&mut r, ch);
            if !crisp {
                assert_eq!(face, "Procedural", "{ch} at {px} px: drawn from the cell");
                continue;
            }
            assert_eq!(face, dejavu, "{ch} at {px} px: the face's own glyph");
            let key = r.glyph_key(ch);
            let img = r.glyph_image(key).clone();
            let top = r.baseline() - img.height() as i32 - img.ymin();
            assert!(
                img.xmin() >= 0
                    && img.xmin() as usize + img.width() <= cw
                    && top >= 0
                    && top as usize + img.height() <= chh,
                "{ch} at {px} px: leaves its {cw}x{chh} cell"
            );
            if ch != '✳' {
                let own = r.fitted_fallback_regular(key, ch).5;
                let inked = own.iter().filter(|&&c| c > 0).count();
                let solid = own.iter().filter(|&&c| c >= 230).count();
                assert!(
                    5 * solid >= inked,
                    "{ch} at {px} px: {solid} of {inked} inked texels solid"
                );
            }
        }
    }
}

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn the_windows_default_evens_its_faces_spinner_frames() {
    use crate::procedural::{SPINNER_REST, SPINNER_STARS};
    let ink = |b: &[u8]| b.iter().map(|&c| f32::from(c)).sum::<f32>() / 255.0;
    let spread = |inks: &[f32]| {
        let (lo, hi) = inks
            .iter()
            .fold((f32::MAX, 0.0f32), |(lo, hi), &i| (lo.min(i), hi.max(i)));
        hi / lo
    };
    let rest_at = SPINNER_STARS
        .iter()
        .position(|&ch| ch == SPINNER_REST)
        .unwrap();
    // The default, and the Semibold set by name, whose heavier `*` the frames need no move for.
    for family in [None, Some(IOSEVKA_SEMIBOLD_ID)] {
        for px in [32.0, 34.0, 42.5, 51.0] {
            let mut r = Renderer::from_system_with_family(family, px, Theme::default()).unwrap();
            r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
            r.seal_admitted_font_sources();
            assert_eq!(
                r.primary_source_path(),
                Some(family.unwrap_or(IOSEVKA_NL_ID))
            );
            let (cw, chh) = r.cell_size();
            let (mut drawn, mut face) = (Vec::new(), Vec::new());
            let mut rest_is_the_faces = false;
            for ch in SPINNER_STARS {
                let key = r.glyph_key(ch);
                assert_ne!(key.source, FaceId::Procedural, "{ch} at {px} px");
                let own = r.fitted_fallback_regular(key, ch).5;
                let img = r.glyph_image(key).clone();
                let top = r.baseline() - img.height() as i32 - img.ymin();
                assert!(
                    img.xmin() >= 0
                        && img.xmin() as usize + img.width() <= cw
                        && top >= 0
                        && top as usize + img.height() <= chh,
                    "{ch} at {px} px: leaves its {cw}x{chh} cell"
                );
                assert_eq!(img.bytes().iter().max(), Some(&255), "{ch} at {px} px");
                if ch != '✳' {
                    let inked = img.bytes().iter().filter(|&&c| c > 0).count();
                    let solid = img.bytes().iter().filter(|&&c| c >= 230).count();
                    assert!(
                        7 * solid >= inked,
                        "{ch} at {px} px: {solid} of {inked} solid"
                    );
                }
                if ch == SPINNER_REST {
                    rest_is_the_faces = img.bytes() == own;
                }
                drawn.push(ink(img.bytes()));
                face.push(ink(&own));
            }
            assert!(
                spread(&drawn) <= SPINNER_FRAME_SPREAD + 0.01,
                "{family:?} at {px} px: frame inks {drawn:?}"
            );
            assert!(
                spread(&face) >= 2.0,
                "control, {family:?} at {px} px: {face:?}"
            );
            // Claude Code's cycle shows the primary's own `*` where `✳` stands. The frames
            // stay within the cycle's spread of it, or within the rest's own ratio to it where
            // the rest, kept as the face draws it, lies past that.
            let key = r.glyph_key('*');
            let star = ink(r.glyph_image(key).bytes());
            let cycle = |frames: &[f32]| [frames[0], star, frames[2], frames[3], frames[4]];
            assert!(
                rest_is_the_faces,
                "{family:?}: {SPINNER_REST} at {px} px is the face's own"
            );
            let bound = SPINNER_CYCLE_SPREAD.max(face[rest_at] / star);
            assert!(
                spread(&cycle(&drawn)) <= bound + 0.01,
                "{family:?} at {px} px: Claude Code's cycle {:?} past {bound}",
                cycle(&drawn)
            );
            assert!(
                spread(&cycle(&face)) >= 2.0,
                "control, {family:?} at {px} px: the face's own cycle {:?}",
                cycle(&face)
            );
            if px != 34.0 {
                continue;
            }
            // Evened about the rest alone, the heaviest frame's ink.
            let uncapped =
                crate::evened_ink(&face, face[4], Some(face[rest_at]), SPINNER_FRAME_SPREAD)
                    .expect("the face's frames span more than the spread");
            if family.is_some() {
                // The Semibold's `*` holds every evened frame within the cycle's spread.
                assert!(
                    spread(&cycle(&drawn)) <= SPINNER_CYCLE_SPREAD,
                    "the Semibold's cycle {:?}",
                    cycle(&drawn)
                );
                assert!((drawn[4] - uncapped).abs() <= 0.02 * uncapped, "{drawn:?}");
            } else {
                // The Regular's lighter `*` does not hold even the rest within it, so the
                // heavier frame comes down to the rest (evened alone it measured 1.74 of the
                // `*`).
                assert!(
                    face[rest_at] > SPINNER_CYCLE_SPREAD * star,
                    "{face:?} {star}"
                );
                assert!(
                    uncapped > 1.05 * face[rest_at] && uncapped > 1.7 * star,
                    "control: evened alone the heaviest frame holds {uncapped}"
                );
                assert!(
                    drawn[4] <= 1.01 * face[rest_at],
                    "the heaviest frame {} outweighs the rest {}",
                    drawn[4],
                    face[rest_at]
                );
            }
        }
    }
}

#[cfg(all(windows, feature = "embedded-font"))]
fn ink_band(r: &mut Renderer, ch: char, style: crate::StyleBits) -> (i32, i32, f32) {
    let baseline = r.baseline();
    let key = r.glyph_key_styled(ch, style);
    let img = r.glyph_image(key).clone();
    let (w, h) = (img.width(), img.height());
    let seen = |y: &usize| img.bytes()[y * w..(y + 1) * w].iter().any(|&a| a >= 32);
    let first = (0..h).find(seen).expect("ink");
    let last = (0..h).rev().find(seen).expect("ink");
    let top = baseline - (img.ymin() + i32::try_from(h).unwrap());
    let (a, b) = (
        top + i32::try_from(first).unwrap(),
        top + i32::try_from(last).unwrap() + 1,
    );
    (a, b, (a + b) as f32 / 2.0)
}

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn the_windows_defaults_asterisk_sits_level_with_the_spinner_frames() {
    use crate::StyleBits;
    let mut r = windows_default_sealed();
    let (_, _, star) = ink_band(&mut r, '*', StyleBits::REGULAR);
    for ch in ['·', '+', '✻'] {
        let (_, _, level) = ink_band(&mut r, ch, StyleBits::REGULAR);
        assert!(
            (star - level).abs() <= 0.5,
            "17 px: `*` centred on row {star}, `{ch}` on {level}"
        );
    }
    let mut off = Vec::new();
    for px in (8..=51u8).map(f32::from) {
        let mut r = Renderer::from_system(px, Theme::default()).unwrap();
        r.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
        r.seal_admitted_font_sources();
        for style in STYLES {
            let (a, b, star) = ink_band(&mut r, '*', style);
            let (c, d, plus) = ink_band(&mut r, '+', style);
            let bound = if style.contains(StyleBits::ITALIC) {
                1.5
            } else {
                1.0
            };
            if (star - plus).abs() > bound {
                off.push(format!("{px} px {style:?}: `*` {a}..{b}, `+` {c}..{d}"));
            }
        }
    }
    assert!(off.is_empty(), "{off:#?}");
    let mut control = untreated_iosevka(WINDOWS_DEFAULT_PX);
    control.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
    control.seal_admitted_font_sources();
    let (_, _, star) = ink_band(&mut control, '*', StyleBits::REGULAR);
    let (_, _, plus) = ink_band(&mut control, '+', StyleBits::REGULAR);
    assert!(
        plus - star >= 4.0,
        "CONTROL: Iosevka Term by name's `*` (row {star}) is not raised above its `+` \
         ({plus}), so this test cannot see the hop it guards against"
    );
}

#[cfg(all(windows, feature = "embedded-font"))]
const STYLES: [crate::StyleBits; 4] = [
    crate::StyleBits::REGULAR,
    crate::StyleBits::BOLD,
    crate::StyleBits::ITALIC,
    crate::StyleBits(crate::StyleBits::BOLD.0 | crate::StyleBits::ITALIC.0),
];

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn only_the_windows_iosevka_faces_lower_their_own_asterisk() {
    use crate::hinted::{HintBank, HintMode, hinted_glyph_raster};
    let lowered_of = |id: &str| {
        let bytes = face_for_id(id).unwrap();
        let lowered = lowered_asterisk(bytes).unwrap_or_else(|| panic!("{id}"));
        let copy = bytes.to_vec();
        assert_eq!(lowered_asterisk(&copy), Some(lowered), "{id}: a copy");
        let face = aterm_font::Face::parse(bytes, 0).unwrap();
        assert_eq!(
            face.glyph_index('*').map(|g| g.0),
            Some(lowered.gid),
            "{id}"
        );
        (bytes, lowered, face.is_italic())
    };
    for id in IOSEVKA_SEMIBOLD_FACES {
        lowered_of(id);
    }
    let mut r = windows_default_sealed();
    for (id, style) in IOSEVKA_NL_FACES.into_iter().zip(STYLES) {
        let (bytes, lowered, italic) = lowered_of(id);
        let inst = HintBank::default()
            .instance(1, bytes, 0, WINDOWS_DEFAULT_PX, HintMode::Full, &[], 0)
            .unwrap();
        let (w, h, xmin, ymin, _, own) =
            hinted_glyph_raster(bytes, 0, lowered.gid, WINDOWS_DEFAULT_PX, &inst, &[]).unwrap();
        let key = r.glyph_key_styled('*', style);
        let img = r.glyph_image(key).clone();
        assert_eq!(
            (img.width(), img.height(), img.bytes()),
            (w, h, own.as_slice()),
            "{id}: the face's own raster"
        );
        assert_eq!(img.ymin(), ymin - 5, "{id}: 5 rows lower");
        let left = xmin - img.xmin();
        let reach = if italic { 0..=1 } else { 0..=0 };
        assert!(reach.contains(&left), "{id}: moved {left} px left");
    }
    for id in IOSEVKA_FACES
        .iter()
        .chain(&CASCADIA_FACES)
        .chain(&JETBRAINS_FACES)
        .chain(&[DEJAVU_ID])
    {
        let bytes = face_for_id(id).unwrap();
        assert_eq!(lowered_asterisk(bytes), None, "{id}");
    }
}

#[test]
#[cfg(all(windows, feature = "embedded-font"))]
fn the_windows_defaults_cjk_comes_in_the_cut_nearest_its_weight() {
    // Han and kana come from the face the user's languages read first (`han_reading_order`):
    // Microsoft YaHei, whose cut nearest a 600 primary is its Bold, or (for a user who reads
    // Japanese first) Yu Gothic, whose nearest is its Medium.
    let (han_heavy, han_plain, han_gain) = if crate::win_locale::user_reads_japanese_first() {
        ("YuGothM.ttc", "YuGothR.ttc", 5)
    } else {
        ("msyhbd.ttc", "msyh.ttc", 6)
    };
    let fonts = crate::windows_fonts_dir();
    let needed = [han_heavy, han_plain, "malgun.ttf", "malgunbd.ttf"];
    if let Some(absent) = needed.iter().find(|f| !fonts.join(f).is_file()) {
        aterm_spec::host::could_not_run(format!("{absent} is not installed here"));
    }
    let mut regular = windows_default_sealed();
    let mut heavy = Renderer::from_system_with_family(
        Some(IOSEVKA_SEMIBOLD_ID),
        WINDOWS_DEFAULT_PX,
        Theme::default(),
    )
    .unwrap();
    heavy.set_line_height(WINDOWS_DEFAULT_LINE_HEIGHT);
    heavy.seal_admitted_font_sources();
    assert_eq!(heavy.primary_source_path(), Some(IOSEVKA_SEMIBOLD_ID));
    for (ch, bold, plain, gain) in [
        ('中', han_heavy, han_plain, han_gain),
        ('確', han_heavy, han_plain, han_gain),
        ('つ', han_heavy, han_plain, han_gain),
        ('한', "malgunbd.ttf", "malgun.ttf", 5),
    ] {
        let got = heavy.debug_fallback_pick_path(ch).unwrap_or_default();
        assert!(
            got.ends_with(bold),
            "{ch} (Semibold primary): drawn from {got}, not {bold}"
        );
        let got = regular.debug_fallback_pick_path(ch).unwrap_or_default();
        assert!(
            got.ends_with(plain),
            "{ch} (the Regular default): drawn from {got}, not {plain}"
        );
        let ((dark, _), (light, _)) = (ink_of(&mut heavy, ch), ink_of(&mut regular, ch));
        assert!(
            5 * dark > gain * light,
            "{ch}: the heavier cut inks {dark} texels, the Regular {light}"
        );
    }
}

#[test]
#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn the_i_ching_line_symbols_skip_the_baseline_bar_and_sit_at_mid_line() {
    let mut r = default_sealed();
    let dejavu = format!("fallback:{DEJAVU_ID}");
    for ch in ('\u{2630}'..='\u{2637}').chain('\u{268A}'..='\u{268F}') {
        let got = resolved_face(&mut r, ch);
        assert_ne!(got, dejavu, "{ch} U+{:04X}", ch as u32);
        assert_ne!(got, "NOTDEF", "{ch} U+{:04X}", ch as u32);
    }
    #[cfg(windows)]
    {
        let rows = |r: &mut Renderer, ch: char| {
            let key = r.glyph_key(ch);
            let img = r.glyph_image(key);
            let (w, h) = (img.width(), img.height());
            let inked: Vec<i32> = (0..h)
                .filter(|&y| img.bytes()[y * w..(y + 1) * w].iter().any(|&b| b > 40))
                .map(|y| img.ymin() + (h - 1 - y) as i32)
                .collect();
            (
                inked[0].min(inked[inked.len() - 1]),
                inked[0].max(inked[inked.len() - 1]),
            )
        };
        let (bar_lo, bar_hi) = rows(&mut r, '⚊');
        let (tri_lo, tri_hi) = rows(&mut r, '☰');
        let (bar, tri) = (bar_lo + bar_hi, tri_lo + tri_hi);
        assert!(
            (bar - tri).abs() <= 4,
            "⚊ rows {bar_lo}..={bar_hi} are not about the middle of ☰'s {tri_lo}..={tri_hi}"
        );
    }
}

#[test]
#[cfg(all(feature = "embedded-font", not(target_os = "macos")))]
fn the_nerd_font_precedes_the_bundled_noto_faces_where_both_draw() {
    #[cfg(windows)]
    let mut r = cascadia_sealed(18.0);
    #[cfg(not(windows))]
    let mut r = default_sealed();
    let contested = ['⏻', '⏼', '⏽', '⏾', '☰', '⭘'];
    for ch in contested {
        let noto_has = [NOTO_SYMBOLS2_ID, NOTO_MATH_ID].iter().any(|id| {
            aterm_font::Face::parse(face_for_id(id).unwrap(), 0)
                .unwrap()
                .glyph_index(ch)
                .is_some_and(|g| g.0 != 0)
        });
        assert!(noto_has, "{ch}: not contested — the test proves nothing");
        let got = resolved_face(&mut r, ch);
        assert!(
            !got.starts_with("symbol:bundled:Noto"),
            "{ch} U+{:04X}: a bundled Noto face took a Nerd Font point ({got})",
            ch as u32
        );
        assert_ne!(got, "NOTDEF", "{ch} is tofu");
    }
    assert_eq!(
        resolved_face(&mut r, '⏵'),
        format!("symbol:{NOTO_SYMBOLS2_ID}")
    );
}
