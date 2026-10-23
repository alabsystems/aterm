// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::terminal::Terminal;
use aterm_render::{FaceId, FacePick, Renderer, StyleBits, Theme, resolve_styled_face};

/// The committed JetBrains Mono fixture (a liga/calt ligature font).
fn ligature_test_font() -> Vec<u8> {
    include_bytes!("fixtures/jetbrains-mono.ttf").to_vec()
}

/// The bundled DejaVu Sans Mono asset (always committed, so no skip).
fn dejavu_path() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/assets/DejaVuSansMono.ttf").to_string()
}

const BOLD_ITALIC: StyleBits = StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0);

#[test]
fn resolve_styled_face_is_total_and_wellformed() {
    let styles = [
        StyleBits::REGULAR,
        StyleBits::BOLD,
        StyleBits::ITALIC,
        BOLD_ITALIC,
    ];
    let (mut seen_primary, mut seen_injected, mut seen_styled) = (false, false, false);
    for style in styles {
        for mask in 0u8..16 {
            let injected = mask & 8 != 0;
            let covers = [mask & 1 != 0, mask & 2 != 0, mask & 4 != 0];
            let pick = resolve_styled_face(style, injected, covers);
            match pick {
                FacePick::Primary => seen_primary = true,
                FacePick::InjectedBold { synthetic } => {
                    seen_injected = true;
                    assert!(injected, "InjectedBold only when the injected face exists");
                    assert!(
                        style.contains(StyleBits::BOLD),
                        "the injected face is BOLD — only a bold request may pick it"
                    );
                    assert!(
                        !synthetic.contains(StyleBits::BOLD),
                        "never re-embolden the real bold face"
                    );
                    assert_eq!(
                        synthetic.0 & !style.0,
                        0,
                        "synthetic ⊆ requested style ({style:?})"
                    );
                }
                FacePick::Styled { slot, synthetic } => {
                    seen_styled = true;
                    assert!(slot < 3, "slot in range");
                    assert!(covers[slot], "a picked slot must be available");
                    assert_eq!(synthetic.0 & !style.0, 0, "synthetic ⊆ requested style");
                    match slot {
                        0 => assert!(!synthetic.contains(StyleBits::BOLD)),
                        1 => assert!(!synthetic.contains(StyleBits::ITALIC)),
                        _ => assert_eq!(synthetic, StyleBits::REGULAR),
                    }
                }
            }
            if style == StyleBits::REGULAR {
                assert_eq!(
                    pick,
                    FacePick::Primary,
                    "an unstyled cell never routes to a styled face"
                );
            }
            let bold_capable =
                injected || covers[0] || (style.contains(StyleBits::ITALIC) && covers[2]);
            if style.contains(StyleBits::BOLD) && bold_capable {
                let synthesizes_bold = match pick {
                    FacePick::Primary => true, // full style synthesized
                    FacePick::InjectedBold { synthetic } | FacePick::Styled { synthetic, .. } => {
                        synthetic.contains(StyleBits::BOLD)
                    }
                };
                assert!(
                    !synthesizes_bold,
                    "a real bold-capable face is present — the resolution must not \
                     dilate (style {style:?}, injected {injected}, covers {covers:?})"
                );
            }
        }
    }
    assert!(seen_primary && seen_injected && seen_styled);
}

#[test]
fn resolve_styled_face_precedence_pins() {
    assert_eq!(
        resolve_styled_face(BOLD_ITALIC, true, [true, true, true]),
        FacePick::Styled {
            slot: 2,
            synthetic: StyleBits::REGULAR
        }
    );
    assert_eq!(
        resolve_styled_face(BOLD_ITALIC, true, [true, true, false]),
        FacePick::InjectedBold {
            synthetic: StyleBits::ITALIC
        }
    );
    assert_eq!(
        resolve_styled_face(StyleBits::BOLD, true, [true, false, false]),
        FacePick::InjectedBold {
            synthetic: StyleBits::REGULAR
        }
    );
    assert_eq!(
        resolve_styled_face(BOLD_ITALIC, false, [true, false, false]),
        FacePick::Styled {
            slot: 0,
            synthetic: StyleBits::ITALIC
        }
    );
    assert_eq!(
        resolve_styled_face(BOLD_ITALIC, false, [false, true, false]),
        FacePick::Styled {
            slot: 1,
            synthetic: StyleBits::BOLD
        }
    );
}

/// Render `text` (may contain SGR escapes) on one row with the given renderer.
fn render_row(r: &mut Renderer, text: &[u8]) -> aterm_render::Frame {
    let (rows, cols) = (1usize, 16usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l"); // hide the cursor: compare glyph ink only
    term.process(text);
    let input = term.cell_frame(rows, cols);
    r.render_input(&input)
}

/// A fixture renderer at 18px on the deterministic portable rasterizer.
fn fixture_renderer(bytes: &[u8]) -> Renderer {
    let mut r = Renderer::from_bytes(bytes, 18.0, Theme::default()).expect("fixture parses");
    r.debug_force_fontdue();
    r
}

#[test]
fn bold_ligature_run_draws_from_real_styled_face() {
    let bytes = ligature_test_font();
    let text_reg: &[u8] = b"a => b";
    let text_bold: &[u8] = b"\x1b[1ma => b";

    let mut plain = fixture_renderer(&bytes);
    let reg = render_row(&mut plain, text_reg);
    let bold_synth = render_row(&mut plain, text_bold);
    assert_ne!(
        reg.pixels, bold_synth.pixels,
        "without a real bold face, bold must be synthesized (differs from regular)"
    );

    let mut styled = fixture_renderer(&bytes);
    styled
        .set_styled_font_bytes(0, &bytes)
        .expect("styled face installs");
    let bold_real = render_row(&mut styled, text_bold);
    assert_eq!(
        reg.pixels, bold_real.pixels,
        "a bold '=>' must draw from the REAL bold face (same bytes ⇒ same ink), \
         not the dilated regular"
    );

    let mut injected = fixture_renderer(&bytes);
    injected.set_bold_font(&bytes).expect("bold face installs");
    let bold_injected = render_row(&mut injected, text_bold);
    assert_eq!(
        reg.pixels, bold_injected.pixels,
        "a bold '=>' must draw from the INJECTED bold face, not the dilated regular"
    );
}

#[test]
fn synthetic_styles_off_renders_regular() {
    let bytes = ligature_test_font();
    let text_reg: &[u8] = b"abc";
    let text_bold: &[u8] = b"\x1b[1mabc";
    let mut r = fixture_renderer(&bytes);
    let reg = render_row(&mut r, text_reg);
    let bold_on = render_row(&mut r, text_bold);
    assert_ne!(
        reg.pixels, bold_on.pixels,
        "default synthesis must show bold"
    );
    assert!(r.set_synthetic_styles(false), "flip reports a change");
    assert!(!r.synthetic_styles());
    let bold_off = render_row(&mut r, text_bold);
    assert_eq!(
        reg.pixels, bold_off.pixels,
        "font_synthetic_style = false must render styled cells with the regular face"
    );
    assert!(!r.set_synthetic_styles(false), "same value is a no-op");
    assert!(r.set_synthetic_styles(true));
    let bold_again = render_row(&mut r, text_bold);
    assert_eq!(bold_on.pixels, bold_again.pixels, "the flag round-trips");
}

#[test]
fn config_fallback_fonts_head_the_chain_and_load() {
    let jb = ligature_test_font();
    let dv_path = dejavu_path();
    let dv = std::fs::read(&dv_path).expect("committed DejaVu asset");
    let jb_font = aterm_render::font::Font::from_bytes(
        jb.as_slice(),
        aterm_render::font::FontSettings::default(),
    )
    .expect("fixture parses");
    let dv_font = aterm_render::font::Font::from_bytes(
        dv.as_slice(),
        aterm_render::font::FontSettings::default(),
    )
    .expect("asset parses");
    let probe = (0x2000u32..0x2C00)
        .filter(|cp| !(0x2500..=0x25FF).contains(cp) && !(0x2800..=0x28FF).contains(cp))
        .filter_map(char::from_u32)
        .find(|&c| jb_font.lookup_glyph_index(c) == 0 && dv_font.lookup_glyph_index(c) != 0);
    let probe = probe.expect("a char distinguishes the fixtures' coverage");

    let mut r = fixture_renderer(&jb);
    assert!(
        r.set_config_fallback_fonts(std::slice::from_ref(&dv_path)),
        "a new config chain reports a change"
    );
    assert_eq!(
        r.debug_fallback_candidate_paths()
            .first()
            .map(String::as_str),
        Some(dv_path.as_str()),
        "the explicit config entry heads the candidate list"
    );
    assert!(
        !r.set_config_fallback_fonts(std::slice::from_ref(&dv_path)),
        "an unchanged reload is a free no-op (no cache churn)"
    );
    r.debug_block_on_lazy_fallbacks();
    let key = r.glyph_key(probe);
    assert_eq!(
        key.source,
        FaceId::Fallback,
        "U+{:04X} must resolve via the config fallback chain",
        probe as u32
    );
    assert!(r.set_config_fallback_fonts(&[]));
    assert_ne!(
        r.debug_fallback_candidate_paths()
            .first()
            .map(String::as_str),
        Some(dv_path.as_str()),
        "clearing the config removes the explicit entry from the head"
    );
}

#[test]
fn config_symbol_and_emoji_fonts_head_their_chains() {
    let jb = ligature_test_font();
    let mut r = fixture_renderer(&jb);
    let sym = "/nonexistent/aterm-w6-symbol-marker.otf";
    let emo = "/nonexistent/aterm-w6-emoji-marker.ttc";

    assert!(r.set_config_symbol_font(Some(sym)));
    assert_eq!(
        r.debug_symbol_candidate_paths().first().map(String::as_str),
        Some(sym)
    );
    assert!(!r.set_config_symbol_font(Some(sym)), "no-op guard");
    assert!(r.set_config_symbol_font(None));
    assert_ne!(
        r.debug_symbol_candidate_paths().first().map(String::as_str),
        Some(sym)
    );

    assert!(r.set_config_emoji_font(Some(emo)));
    assert_eq!(
        r.debug_emoji_candidate_paths().first().map(String::as_str),
        Some(emo)
    );
    assert!(!r.set_config_emoji_font(Some(emo)), "no-op guard");
    assert!(r.set_config_emoji_font(None));
    assert_ne!(
        r.debug_emoji_candidate_paths().first().map(String::as_str),
        Some(emo)
    );
}
