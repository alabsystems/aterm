// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Font, FontSettings, with_cmap_record_last};

/// The characters the Stats Deck drew wrong on Monaco — `·` as `∑`, `×`
/// as `◊` — and two more Latin-1 characters the same subtables remap.
const LATIN_1: [char; 4] = ['\u{B7}', '\u{D7}', '\u{B0}', '\u{E9}'];

/// What the face's Unicode subtables map `ch` to, read off aterm-font one
/// subtable at a time: the first that maps it to a glyph the face has, `0`
/// for none, and `None` for a face with no Unicode subtable.
fn unicode_answer(face: &aterm_font::Face<'_>, ch: char) -> Option<u16> {
    let cmap = face.tables().cmap?;
    let mut unicode = cmap
        .subtables
        .into_iter()
        .filter(|s| s.is_unicode())
        .peekable();
    unicode.peek()?;
    Some(
        unicode
            .find_map(|s| {
                s.glyph_index(u32::from(ch))
                    .map(|g| g.0)
                    .filter(|&g| g != 0 && g < face.number_of_glyphs())
            })
            .unwrap_or(0),
    )
}

/// Holds [`Font::unicode_glyph_index`] to [`unicode_answer`] (to the
/// enumerated map for a face with no Unicode subtable) over every char any
/// subtable of `face` maps, and answers the chars where it is not
/// [`Font::lookup_glyph_index`]'s answer.
fn moved_from_the_enumerated_map(font: &Font, face: &aterm_font::Face<'_>) -> Vec<char> {
    let mut chars = Vec::new();
    if let Some(cmap) = face.tables().cmap {
        for subtable in cmap.subtables {
            subtable.codepoints(|cp| chars.extend(char::from_u32(cp)));
        }
    }
    chars.sort_unstable();
    chars.dedup();
    let mut moved = Vec::new();
    for ch in chars {
        let want = unicode_answer(face, ch).unwrap_or_else(|| font.lookup_glyph_index(ch));
        assert_eq!(
            font.unicode_glyph_index(ch),
            want,
            "U+{:04X}: the Unicode subtables' glyph",
            u32::from(ch)
        );
        if want != font.lookup_glyph_index(ch) {
            moved.push(ch);
        }
    }
    moved
}

/// A MACINTOSH SUBTABLE LISTED AFTER THE UNICODE ONE DOES NOT MOVE A CHAR
/// OFF ITS GLYPH.
#[cfg(feature = "embedded-font")]
#[test]
fn a_mac_subtable_listed_last_does_not_remap_latin_1() {
    let shipped_bytes = crate::embedded_font();
    let moved_bytes = with_cmap_record_last(shipped_bytes, 1, 0)
        .expect("DejaVu Sans Mono carries a Mac Roman (1,0) subtable");
    let font = Font::from_bytes(moved_bytes.as_slice(), FontSettings::default())
        .expect("the reordered face parses");
    let face = aterm_font::Face::parse(&moved_bytes, 0).expect("the reordered face parses");
    let shipped =
        Font::from_bytes(shipped_bytes, FontSettings::default()).expect("the embedded face parses");
    let gid = |ch: char| face.glyph_index(ch).map_or(0, |g| g.0);

    assert_eq!(font.lookup_glyph_index('\u{B7}'), gid('\u{2211}'), "· is ∑");
    assert_eq!(font.lookup_glyph_index('\u{D7}'), gid('\u{25CA}'), "× is ◊");
    for ch in LATIN_1 {
        let want = gid(ch);
        assert_ne!(want, 0, "DejaVu Sans Mono maps U+{:04X}", u32::from(ch));
        assert_ne!(
            font.lookup_glyph_index(ch),
            want,
            "the enumerated map remaps U+{:04X}",
            u32::from(ch)
        );
        assert_eq!(
            font.unicode_glyph_index(ch),
            want,
            "U+{:04X}",
            u32::from(ch)
        );
        assert_eq!(
            shipped.lookup_glyph_index(ch),
            want,
            "U+{:04X}",
            u32::from(ch)
        );
    }
    let moved = moved_from_the_enumerated_map(&font, &face);
    let latin_1 = moved
        .iter()
        .filter(|ch| ('\u{A0}'..='\u{FF}').contains(*ch));
    assert!(
        latin_1.count() >= 80,
        "the Mac Roman subtable remaps most of Latin-1: {moved:?}"
    );
    for ch in moved.iter().filter(|ch| !ch.is_control()) {
        assert_eq!(
            font.unicode_glyph_index(*ch),
            shipped.lookup_glyph_index(*ch),
            "U+{:04X}",
            u32::from(*ch)
        );
    }
}

/// Styled terminal cells use the character-addressed raster path. Reordering
/// cmap records must not turn their middle dots into summation signs.
#[cfg(feature = "embedded-font")]
#[test]
fn styled_latin_1_pixels_ignore_mac_roman_cmap_order() {
    use crate::{GlyphImage, Renderer, StyleBits, Theme};

    fn pixels(
        renderer: &mut Renderer,
        ch: char,
        style: StyleBits,
    ) -> (usize, usize, i32, i32, u32, Vec<u8>) {
        let key = renderer.glyph_key_styled(ch, style);
        let GlyphImage::Mono {
            width,
            height,
            xmin,
            ymin,
            advance,
            bytes,
        } = renderer.glyph_image(key)
        else {
            panic!("Latin-1 text must produce a monochrome glyph");
        };
        (
            *width,
            *height,
            *xmin,
            *ymin,
            advance.to_bits(),
            bytes.clone(),
        )
    }

    let original = crate::embedded_font();
    let reordered = with_cmap_record_last(original, 1, 0).unwrap();
    let parsed = Font::from_bytes(reordered.as_slice(), FontSettings::default()).unwrap();
    assert_ne!(
        parsed.lookup_glyph_index('·'),
        parsed.unicode_glyph_index('·'),
        "the fixture must exercise the legacy mapping conflict"
    );
    for px in [12.0, 18.0, 26.0] {
        let mut expected = Renderer::from_bytes(original, px, Theme::default()).unwrap();
        let mut actual = Renderer::from_bytes(&reordered, px, Theme::default()).unwrap();
        #[cfg(target_os = "macos")]
        {
            expected.rasterizer = crate::RasterKind::Fontdue;
            actual.rasterizer = crate::RasterKind::Fontdue;
        }
        for ch in LATIN_1 {
            for style in [
                StyleBits::REGULAR,
                StyleBits::BOLD,
                StyleBits::ITALIC,
                StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0),
            ] {
                assert_eq!(
                    pixels(&mut actual, ch, style),
                    pixels(&mut expected, ch, style),
                    "U+{:04X}, style {}, {px}px",
                    u32::from(ch),
                    style.0
                );
            }
        }
    }
}

/// NOTHING A FACE DRAWS RIGHT TODAY MOVES. The embedded faces carry a Mac
/// Roman subtable BEFORE their Unicode ones, so the enumeration already
/// gives every printable char its Unicode glyph; the Unicode answer agrees
/// with it on all of them and differs only on control codes the Mac
/// subtable maps and the Unicode ones do not.
#[cfg(feature = "embedded-font")]
#[test]
fn the_unicode_answer_keeps_every_printable_char_of_the_shipped_faces() {
    for (name, bytes) in [
        ("DejaVu Sans Mono", crate::embedded_font()),
        ("Symbols Nerd Font Mono", crate::embedded_symbols_font()),
        (
            "Iosevka Term",
            crate::bundled::face_for_id(crate::bundled::IOSEVKA_ID)
                .expect("the bundled Iosevka Term"),
        ),
    ] {
        let font = Font::from_bytes(bytes, FontSettings::default()).expect(name);
        let face = aterm_font::Face::parse(bytes, 0).expect(name);
        let moved = moved_from_the_enumerated_map(&font, &face);
        assert!(
            moved.iter().all(|ch| ch.is_control()),
            "{name}: only control codes move: {moved:?}"
        );
    }
}

/// MONACO DRAWS LATIN-1 BY ITS UNICODE SUBTABLE.
#[cfg(target_os = "macos")]
#[test]
fn monaco_resolves_latin_1_through_its_unicode_subtable() {
    let path = "/System/Library/Fonts/Monaco.ttf";
    let Ok(bytes) = std::fs::read(path) else {
        aterm_spec::host::could_not_run(format!("no {path} on this Mac"));
    };
    let font = Font::from_bytes(bytes.as_slice(), FontSettings::default()).expect(path);
    let face = aterm_font::Face::parse(&bytes, 0).expect(path);
    let grid = |ch: char| face.glyph_index(ch).map_or(0, |g| g.0);
    for ch in LATIN_1 {
        assert_ne!(grid(ch), 0, "Monaco maps U+{:04X}", u32::from(ch));
    }
    for ch in '\u{A0}'..='\u{FF}' {
        assert_eq!(
            font.unicode_glyph_index(ch),
            grid(ch),
            "U+{:04X}",
            u32::from(ch)
        );
    }
    moved_from_the_enumerated_map(&font, &face);
}

/// A PARSE OUTLINES NO GLYPH: a glyph's box is outlined the first time
/// its metrics are asked for, and kept.
#[cfg(feature = "embedded-font")]
#[test]
fn a_parse_outlines_no_glyph_until_asked_for_it() {
    let bytes = crate::embedded_font();
    let _ = aterm_font::take_spent();
    let font = Font::from_bytes(bytes, FontSettings::default()).expect("the embedded face");
    assert_eq!(
        aterm_font::take_spent().outline_steps,
        0,
        "the parse outlined"
    );

    let face = aterm_font::Face::parse(bytes, 0).expect("the embedded face");
    let a = face.glyph_index('A').expect("A").0;
    let ink = face
        .glyph_bounding_box(aterm_font::GlyphId(a))
        .expect("A has ink");
    let advance = face.glyph_hor_advance(aterm_font::GlyphId(a)).expect("A");
    let _ = aterm_font::take_spent();
    let metrics = font.metrics_indexed(a, 32.0);
    assert!(
        aterm_font::take_spent().outline_steps > 0,
        "A outlined when asked for"
    );
    assert_eq!(font.metrics_indexed(a, 32.0), metrics);
    assert_eq!(aterm_font::take_spent().outline_steps, 0, "A outlined once");

    let scale = font.scale_factor(32.0);
    let (xmin, ymin, width, height) =
        super::ink_box_px([ink.x_min, ink.y_min, ink.x_max, ink.y_max], scale);
    assert_eq!(
        metrics,
        super::Metrics {
            xmin,
            ymin,
            width,
            height,
            advance_width: scale * f32::from(advance),
        }
    );
}
