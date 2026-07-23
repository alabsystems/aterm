// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// W6 (per-style fonts + TOML fallback chain) — the machine-checked invariants.
//
// Two-tier proofs, following the presentation-gate idiom:
//   * Tier-0: the ty-checked abstract twins `styled_run_face_model` /
//     `fallback_precedence_model` (aterm-spec/tests/derived_ring_ty.rs) prove
//     the policies over their whole bounded state space AND catch the pre-fix
//     defects (Buggy=1 → counterexample).
//   * Tier-1 (this file): the SAME invariants over the real shipping code —
//     `resolve_styled_face` enumerated over its COMPLETE 2^6 input space (a
//     complete proof: the domain is finite booleans), `fallback_chain_order`
//     over the full presence lattice, and rendered-ink gates that bind the
//     styled-run routing (`row_glyph_plan` / `rasterize`) to real pixels.
//
// PROVEN INVARIANTS (the W6 brief):
//   1. Styled-face resolution is TOTAL: every (style, config) input resolves
//      to a face without panic, falling back to Primary.
//   2. Chain precedence law: explicit TOML entries strictly outrank env
//      aliases, which strictly outrank discovery.
//   3. (Config round-trip for the new keys lives with the config code:
//      aterm-gui/src/prefs.rs `w6_font_keys_round_trip`.)

use aterm_core::terminal::Terminal;
use aterm_render::{
    FaceId, FacePick, Renderer, StyleBits, Theme, fallback_chain_order, resolve_styled_face,
};

/// The committed JetBrains Mono fixture (a liga/calt ligature font), or `None`
/// → SKIP (mirrors tests/ligatures.rs).
fn ligature_test_font() -> Option<Vec<u8>> {
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/jetbrains-mono.ttf"
    );
    std::fs::read(FIXTURE).ok()
}

/// The bundled DejaVu Sans Mono asset (always committed — no SKIP).
fn dejavu_path() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/assets/DejaVuSansMono.ttf").to_string()
}

const BOLD_ITALIC: StyleBits = StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0);

/// INVARIANT 1 (complete proof by exhaustive enumeration): the styled-face
/// policy is TOTAL and WELL-FORMED over its entire 2^6 input space — every
/// (style, injected, coverage) combination resolves without panic; a returned
/// slot is in range and available; the synthetic residual never re-synthesizes
/// a bit the picked face already supplies, nor invents a bit the cell never
/// asked for; REGULAR is always Primary (the fallback).
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
            // TOTALITY: the call itself must not panic, for every input.
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
                    // The face's own bits are never also synthesized.
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
            // Tier-1 bind to the StyledRunFace ty model's RealBoldNeverDilated:
            // when a face that can serve the request's REAL boldness exists
            // (the injection or slot 0 always; slot 2 only for a bold-ITALIC
            // request — it must not slant a plain bold cell), the resolution
            // never synthesizes bold.
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
    // NON-VACUITY: every variant is genuinely reachable.
    assert!(seen_primary && seen_injected && seen_styled);
}

/// The full-style precedence pins (the resolve_styled_face doc-comment order):
/// a real bold-italic face beats the injected bold for BOLD|ITALIC; the
/// injected bold beats the discovered bold sibling for plain BOLD.
#[test]
fn resolve_styled_face_precedence_pins() {
    // Exact bold-italic face wins over injected-bold + synthetic shear.
    assert_eq!(
        resolve_styled_face(BOLD_ITALIC, true, [true, true, true]),
        FacePick::Styled {
            slot: 2,
            synthetic: StyleBits::REGULAR
        }
    );
    // No exact face: the explicit injection wins, shear synthesized.
    assert_eq!(
        resolve_styled_face(BOLD_ITALIC, true, [true, true, false]),
        FacePick::InjectedBold {
            synthetic: StyleBits::ITALIC
        }
    );
    // Plain bold: injection outranks the discovered sibling.
    assert_eq!(
        resolve_styled_face(StyleBits::BOLD, true, [true, false, false]),
        FacePick::InjectedBold {
            synthetic: StyleBits::REGULAR
        }
    );
    // Partial fallbacks for bold-italic without an exact face or injection.
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

/// INVARIANT 2 (the precedence LAW, over the full presence lattice): the chain
/// is exactly `config ++ env ++ discovery` — every config entry precedes every
/// env entry precedes every discovery entry, relative order within each class
/// is preserved, and nothing is dropped or invented. Marker strings make class
/// membership unambiguous.
#[test]
fn fallback_chain_order_precedence_law() {
    let configs: [&[&str]; 3] = [&[], &["c1"], &["c1", "c2"]];
    let envs = [None, Some("e1")];
    let discos: [&[&str]; 3] = [&[], &["d1"], &["d1", "d2"]];
    for cfg in configs {
        for env in envs {
            for disc in discos {
                let cfg_v: Vec<String> = cfg.iter().map(|s| (*s).to_string()).collect();
                let disc_v: Vec<String> = disc.iter().map(|s| (*s).to_string()).collect();
                let out = fallback_chain_order(&cfg_v, env.map(str::to_string), &disc_v);
                // Nothing dropped or invented; order == concatenation.
                let mut expect = cfg_v.clone();
                expect.extend(env.map(str::to_string));
                expect.extend(disc_v.clone());
                assert_eq!(out, expect, "cfg {cfg:?} env {env:?} disc {disc:?}");
                // Class precedence: positions strictly increase config < env < disc.
                let pos = |m: &str| out.iter().position(|x| x == m);
                if let (Some(c), Some(e)) = (pos("c1"), pos("e1")) {
                    assert!(c < e, "config must outrank env");
                }
                if let (Some(e), Some(d)) = (pos("e1"), pos("d1")) {
                    assert!(e < d, "env must outrank discovery");
                }
            }
        }
    }
}

/// Tier-1 bind to the FallbackPrecedence ty model: the real function's FIRST
/// candidate class equals the model's `winner` for every presence combination
/// (1 = config, 2 = env, 3 = discovery; discovery is always non-empty in the
/// shipping candidate lists).
#[test]
fn fallback_chain_order_first_element_matches_model_winner() {
    let disc = vec!["d".to_string()];
    for (cfg_present, env_present) in [(false, false), (true, false), (false, true), (true, true)] {
        let cfg: Vec<String> = if cfg_present {
            vec!["c".into()]
        } else {
            vec![]
        };
        let env = env_present.then(|| "e".to_string());
        let out = fallback_chain_order(&cfg, env, &disc);
        let winner = match out.first().map(String::as_str) {
            Some("c") => 1,
            Some("e") => 2,
            Some("d") => 3,
            other => panic!("unexpected head {other:?}"),
        };
        let expect = if cfg_present {
            1
        } else if env_present {
            2
        } else {
            3
        };
        assert_eq!(
            winner, expect,
            "cfg_present {cfg_present} env_present {env_present}"
        );
    }
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

/// A fixture renderer at 18px on the deterministic fontdue backend.
fn fixture_renderer(bytes: &[u8]) -> Renderer {
    let mut r = Renderer::from_bytes(bytes, 18.0, Theme::default()).expect("fixture parses");
    r.debug_force_fontdue();
    r
}

/// THE RUN-ROUTING GATE (invariant 1 bound to real ink): a bold `=>` ligature
/// run draws from the REAL bold face when one is installed, instead of the
/// regular glyph dilated by synthetic embolden.
///
/// Stand-in trick: the "bold" face is the SAME JetBrains Mono file, so if (and
/// only if) the run is genuinely shaped + rasterized from the installed face
/// with NO synthesis, bold ink is byte-identical to regular ink. The negative
/// control shows the pre-fix behaviour (no real face → dilation → different
/// ink), so the equality is non-vacuous.
#[test]
fn bold_ligature_run_draws_from_real_styled_face() {
    let Some(bytes) = ligature_test_font() else {
        eprintln!("SKIP: no ligature test font fixture");
        return;
    };
    let text_reg: &[u8] = b"a => b";
    let text_bold: &[u8] = b"\x1b[1ma => b";

    // NEGATIVE CONTROL (the pre-fix world): no real bold face anywhere — the
    // bold run and bold cells dilate, so bold ink differs from regular ink.
    let mut plain = fixture_renderer(&bytes);
    let reg = render_row(&mut plain, text_reg);
    let bold_synth = render_row(&mut plain, text_bold);
    assert_ne!(
        reg.pixels, bold_synth.pixels,
        "without a real bold face, bold must be synthesized (differs from regular)"
    );

    // REAL STYLED SLOT (sibling-injection seam): bold run + bold cells all
    // rasterize from the installed slot-0 face — zero synthesis, identical ink.
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

    // INJECTED-BOLD seam (set_bold_font, the web-host injection now wired to
    // config `font_family_bold`): same law through FacePick::InjectedBold.
    let mut injected = fixture_renderer(&bytes);
    injected.set_bold_font(&bytes).expect("bold face installs");
    let bold_injected = render_row(&mut injected, text_bold);
    assert_eq!(
        reg.pixels, bold_injected.pixels,
        "a bold '=>' must draw from the INJECTED bold face, not the dilated regular"
    );
}

/// `font_synthetic_style = false` (W6): with NO real styled face, a bold cell
/// renders with the REGULAR face (no dilation) — and the flag round-trips
/// (re-enabling restores synthesis). The default-on control pins that the flag
/// is what changed the ink.
#[test]
fn synthetic_styles_off_renders_regular() {
    let Some(bytes) = ligature_test_font() else {
        eprintln!("SKIP: no ligature test font fixture");
        return;
    };
    let text_reg: &[u8] = b"abc";
    let text_bold: &[u8] = b"\x1b[1mabc";
    let mut r = fixture_renderer(&bytes);
    let reg = render_row(&mut r, text_reg);
    // CONTROL: default (synthetic on) — bold differs from regular.
    let bold_on = render_row(&mut r, text_bold);
    assert_ne!(
        reg.pixels, bold_on.pixels,
        "default synthesis must show bold"
    );
    // Flag off: bold renders as the regular face.
    assert!(r.set_synthetic_styles(false), "flip reports a change");
    assert!(!r.synthetic_styles());
    let bold_off = render_row(&mut r, text_bold);
    assert_eq!(
        reg.pixels, bold_off.pixels,
        "font_synthetic_style = false must render styled cells with the regular face"
    );
    // Same-value set is a free no-op; re-enabling restores synthesis.
    assert!(!r.set_synthetic_styles(false), "same value is a no-op");
    assert!(r.set_synthetic_styles(true));
    let bold_again = render_row(&mut r, text_bold);
    assert_eq!(bold_on.pixels, bold_again.pixels, "the flag round-trips");
}

/// The CONFIG fallback chain (W6): a `fallback_fonts` path heads the candidate
/// list (config > env alias > discovery — the proven order), actually LOADS on
/// the first primary miss, and the setter's hot-reload no-op guard holds.
#[test]
fn config_fallback_fonts_head_the_chain_and_load() {
    let Some(jb) = ligature_test_font() else {
        eprintln!("SKIP: no ligature test font fixture");
        return;
    };
    let dv_path = dejavu_path();
    let dv = std::fs::read(&dv_path).expect("committed DejaVu asset");
    // Find a probe char the primary (JetBrains Mono) lacks but DejaVu covers,
    // outside the procedural ranges (box drawing / blocks / braille), so the
    // fallback chain is genuinely consulted. Scanned, not hardcoded, so a
    // fixture swap can't silently vacate the test.
    let jb_font = fontdue::Font::from_bytes(jb.as_slice(), fontdue::FontSettings::default())
        .expect("fixture parses");
    let dv_font = fontdue::Font::from_bytes(dv.as_slice(), fontdue::FontSettings::default())
        .expect("asset parses");
    let probe = (0x2000u32..0x2C00)
        .filter(|cp| !(0x2500..=0x25FF).contains(cp) && !(0x2800..=0x28FF).contains(cp))
        .filter_map(char::from_u32)
        .find(|&c| jb_font.lookup_glyph_index(c) == 0 && dv_font.lookup_glyph_index(c) != 0);
    let Some(probe) = probe else {
        eprintln!("SKIP: no char distinguishes the fixtures' coverage");
        return;
    };

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
    // The config chain is parsed OFF-THREAD on native (W8); block it in so the
    // probe observes the LANDED chain instead of racing a provisional miss.
    r.debug_block_on_lazy_fallbacks();
    // The probe char resolves through the config-installed fallback face.
    let key = r.glyph_key(probe);
    assert_eq!(
        key.source,
        FaceId::Fallback,
        "U+{:04X} must resolve via the config fallback chain",
        probe as u32
    );
    // And clearing the config restores (and reports) the discovery-only chain.
    assert!(r.set_config_fallback_fonts(&[]));
    assert_ne!(
        r.debug_fallback_candidate_paths()
            .first()
            .map(String::as_str),
        Some(dv_path.as_str()),
        "clearing the config removes the explicit entry from the head"
    );
}

/// The symbol/emoji config setters (W6): the explicit path heads each
/// candidate list, and the no-op guard holds. Marker paths (never read) keep
/// this order-only test hermetic — no env mutation, no font I/O.
#[test]
fn config_symbol_and_emoji_fonts_head_their_chains() {
    let Some(jb) = ligature_test_font() else {
        eprintln!("SKIP: no ligature test font fixture");
        return;
    };
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
