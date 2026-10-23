// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for autohinter outline orientation, script styles, and entry-point refusals.

use super::font::HintFont;
use super::outline::{Orientation, Outline};
use super::shape::Shaper;
use super::style::GlyphStyleMap;
use super::{GlyphId, HintingInstance, Target};

const DEJAVU: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-render/assets/DejaVuSansMono.ttf"
));
const SUNUWAR: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-render/tests/fixtures/NotoSansSunuwar-Regular.otf"
));

fn outline_of(font: &HintFont, ch: char) -> Outline {
    let gid = font.charmap().map(ch).expect("the face maps the character");
    let glyph = font.outline(gid).expect("the glyph has an outline source");
    let mut outline = Outline::default();
    outline.fill(font, &glyph, &[]).expect("the outline loads");
    outline
}

#[test]
fn outlines_wind_as_their_format_does() {
    let tt = HintFont::new(DEJAVU, 0).expect("DejaVu reads");
    assert_eq!(
        outline_of(&tt, 'O').orientation,
        Some(Orientation::CounterClockwise)
    );
    let ps = HintFont::new(SUNUWAR, 0).expect("Noto Sans Sunuwar reads");
    let gid = ps
        .charmap()
        .map('\u{11BC0}')
        .expect("Sunuwar's first letter");
    let glyph = ps.outline(gid).expect("a CFF outline source");
    let mut outline = Outline::default();
    outline
        .fill(&ps, &glyph, &[])
        .expect("the CFF outline loads");
    assert!(!outline.points.is_empty(), "the CFF glyph has points");
    assert_eq!(outline.orientation, Some(Orientation::Clockwise));
}

#[test]
fn glyph_styles_follow_the_scripts() {
    let font = HintFont::new(DEJAVU, 0).expect("DejaVu reads");
    let shaper = Shaper::new(&font);
    let map = GlyphStyleMap::new(font.glyph_count(), &shaper);
    let style_of = |ch: char| {
        let gid = font.charmap().map(ch).expect("mapped");
        map.style(gid)
            .and_then(|s| s.style_class())
            .map(|c| c.script.name)
    };
    assert_eq!(style_of('A'), Some("Latin"));
    assert_eq!(style_of('ж'), Some("Cyrillic"));
    assert_eq!(style_of('Ω'), Some("Greek"));
    assert_eq!(style_of('ب'), Some("Arabic"));
    let acute = font.charmap().map('\u{301}').expect("mapped");
    assert!(map.style(acute).is_some_and(|s| s.is_non_base()));
    assert_eq!(
        map.style(GlyphId::new(0))
            .and_then(|s| s.style_class())
            .map(|c| c.script.name),
        Some("CJKV ideographs")
    );
    let past = GlyphId::new(font.glyph_count());
    assert!(map.style(past).is_none(), "no style past the last glyph");
}

#[test]
fn entry_points_refuse_what_cannot_be_hinted() {
    for px in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert!(HintingInstance::new(DEJAVU, 0, px, &[], Target::Normal).is_none());
    }
    assert!(HintingInstance::new(b"not a font", 0, 12.0, &[], Target::Normal).is_none());
    assert!(HintingInstance::new(DEJAVU, 1, 12.0, &[], Target::Normal).is_none());
    let hint = HintingInstance::new(DEJAVU, 0, 12.0, &[], Target::Normal).expect("builds");
    let mut recorded = super::path::Recorded::default();
    let n = HintFont::new(DEJAVU, 0).expect("reads").glyph_count();
    let past = u16::try_from(n).expect("a 16-bit glyph count");
    assert!(hint.draw(DEJAVU, 0, past, &mut recorded).is_none());
    assert!(recorded.is_empty(), "a refused glyph draws nothing");
}
