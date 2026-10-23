// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where a shrunk fallback symbol sits, and how big a styled one is.

use aterm_render::{GlyphImage, Renderer, StyleBits, Theme};

fn asset(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/assets/bundled/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("bundled asset")
}

fn renderer(px: f32) -> Renderer {
    let mut r = Renderer::from_bytes(&asset("JetBrainsMono-Regular.ttf"), px, Theme::default())
        .expect("primary parses");
    r.set_symbol_fallback_bytes(&asset("NotoSansSymbols2-Regular.ttf"))
        .expect("symbol face parses");
    r
}

fn ink_rows(img: &GlyphImage) -> Option<(i32, i32)> {
    let GlyphImage::Mono {
        width,
        height,
        ymin,
        bytes,
        ..
    } = img
    else {
        panic!("expected a Mono glyph, got {img:?}");
    };
    let (w, h) = (*width, *height);
    let inked: Vec<usize> = (0..h)
        .filter(|&r| bytes[r * w..(r + 1) * w].iter().any(|&b| b > 40))
        .collect();
    let (first, last) = (*inked.first()?, *inked.last()?);
    let h = h as i32;
    Some((ymin + h - last as i32 - 1, ymin + h - first as i32))
}

fn centre2(rows: (i32, i32)) -> i32 {
    rows.0 + rows.1
}

#[test]
fn a_shrunk_media_symbol_keeps_the_primary_discs_centre() {
    for px in [18.0_f32, 26.0] {
        let mut r = renderer(px);
        let disc = r.glyph_key('●');
        let bullet = r.glyph_key('⏺');
        assert_ne!(
            disc.source, bullet.source,
            "the fixture must route ⏺ to the symbol face and ● to the primary"
        );
        let disc_rows = ink_rows(&r.glyph_image(disc).clone()).expect("● has ink");
        let bullet_rows = ink_rows(&r.glyph_image(bullet).clone()).expect("⏺ has ink");
        let (d, b) = (centre2(disc_rows), centre2(bullet_rows));
        assert!(
            (d - b).abs() <= 2,
            "at {px} px ⏺ is centred at {:.1} and ● at {:.1} above the baseline \
             (rows ⏺ {bullet_rows:?}, ● {disc_rows:?}) — the shrink dragged the \
             symbol towards the baseline",
            f64::from(b) / 2.0,
            f64::from(d) / 2.0
        );
    }
}

#[test]
fn an_italic_fallback_symbol_is_as_big_as_the_bold_one() {
    for px in [14.0_f32, 26.0] {
        styled_symbols_share_one_size(px);
    }
}

fn styled_symbols_share_one_size(px: f32) {
    let mut r = renderer(px);
    let (cell_w, _) = r.cell_size();
    let mut checked = 0;
    for ch in ['✔', '✘', '★', '◐', '⏺'] {
        let bold = r.glyph_key_styled(ch, StyleBits::BOLD);
        let italic = r.glyph_key_styled(ch, StyleBits::ITALIC);
        if italic.source != aterm_render::FaceId::SymbolFallback {
            continue;
        }
        checked += 1;
        let (bold, italic) = (r.glyph_image(bold).clone(), r.glyph_image(italic).clone());
        let regular = r.glyph_key(ch);
        let regular = r.glyph_image(regular).clone();
        for (name, styled) in [("bold", &bold), ("italic", &italic)] {
            assert!(
                styled.bytes() != regular.bytes() || styled.width() != regular.width(),
                "{px} px {ch}: the {name} mask is the regular one — the style was dropped"
            );
            assert!(
                styled.xmin() >= 0 && styled.xmin() as usize + styled.width() <= cell_w,
                "{px} px {ch}: {name} cols {}+{} leave the {cell_w} px cell",
                styled.xmin(),
                styled.width()
            );
        }
        let b = ink_rows(&bold).expect("bold has ink");
        let i = ink_rows(&italic).expect("italic has ink");
        let g = ink_rows(&regular).expect("regular has ink");
        let (bh, ih, gh) = (b.1 - b.0, i.1 - i.0, g.1 - g.0);
        assert!(
            (bh - gh).abs() <= 1,
            "{px} px {ch}: bold ink is {bh} rows tall, regular {gh} — the dilation shrank it"
        );
        assert!(
            (bh - ih).abs() <= 1,
            "{px} px {ch}: italic ink is {ih} rows tall, bold {bh} — the shear shrank it"
        );
        assert!(
            (centre2(b) - centre2(i)).abs() <= 2,
            "{ch}: italic sits at rows {i:?}, bold at {b:?}"
        );
    }
    assert!(
        checked >= 3,
        "only {checked} symbols reached the symbol face"
    );
}

fn renderer_with_math(px: f32) -> Renderer {
    let mut r = renderer(px);
    r.add_fallback_bytes(&asset("NotoSansMath-Regular.ttf"))
        .expect("math face parses");
    r
}

#[test]
fn a_shrunk_fallback_letter_stands_on_the_baseline() {
    for px in [14.0_f32, 18.0, 26.0] {
        let mut r = renderer_with_math(px);
        let w = r.glyph_key('W');
        let w_rows = ink_rows(&r.glyph_image(w).clone()).expect("W has ink");
        let mut shrunk = 0;
        for ch in ['𝐖', '𝐌', '𝑊', '𝐀', '𝐇'] {
            let key = r.glyph_key(ch);
            assert_ne!(key.source, w.source, "{ch} must come from the math face");
            let rows = ink_rows(&r.glyph_image(key).clone()).expect("letter has ink");
            shrunk += usize::from(rows.1 - rows.0 < w_rows.1 - w_rows.0);
            assert!(
                (rows.0 - w_rows.0).abs() <= 1,
                "at {px} px {ch} ink rows {rows:?} float off the baseline (W {w_rows:?})"
            );
        }
        assert!(shrunk >= 3, "at {px} px only {shrunk} letters were shrunk");
    }
}

#[test]
fn a_fallback_media_disc_is_as_big_as_the_primary_disc() {
    for px in [12.0_f32, 14.0, 18.0, 26.0, 40.0] {
        let mut r = renderer(px);
        let (cell_w, cell_h) = r.cell_size();
        let baseline = r.baseline();
        let disc = r.glyph_key('●');
        let bullet = r.glyph_key('⏺');
        let d = ink_rows(&r.glyph_image(disc).clone()).expect("● has ink");
        let img = r.glyph_image(bullet).clone();
        let b = ink_rows(&img).expect("⏺ has ink");
        let (dh, bh) = (d.1 - d.0, b.1 - b.0);
        let yardstick = dh.min(i32::try_from(cell_w).expect("cell width"));
        assert!(
            bh * 10 >= yardstick * 8,
            "at {px} px ⏺ is {bh} rows tall, ● {dh}, the cell {cell_w} px wide — \
             the fallback disc is shrunk"
        );
        let GlyphImage::Mono {
            width,
            height,
            xmin,
            ymin,
            ..
        } = img
        else {
            unreachable!()
        };
        let top = baseline - height as i32 - ymin;
        assert!(
            xmin >= 0 && xmin as usize + width <= cell_w,
            "at {px} px ⏺ cols {xmin}+{width} leave the {cell_w} px cell"
        );
        assert!(
            top >= 0 && top as usize + height <= cell_h,
            "at {px} px ⏺ rows {top}+{height} leave the {cell_h} px cell"
        );
    }
}
