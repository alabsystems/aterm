// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WINDOWS CRISPNESS: grid fitting on the platform that shipped with none.
//!
//! Before this wave the Windows raster chain fell through to a raw outline
//! fill — stems landed at fractional pixel phases and horizontal features
//! straddled two rows — on the one platform whose reference renderers (Windows
//! Terminal, VS Code, conhost) are all DirectWrite-hinted in the next window.
//!
//! The seam itself (`font_hinting` full ⇄ off actually changes coverage while
//! never moving the cell box) is bound cross-platform in
//! `font_hinting_config.rs`. THIS file binds the half that is Windows-specific
//! and that a static test font cannot reach: the platform default face,
//! `C:\Windows\Fonts\CascadiaMono.ttf`, is a VARIABLE font with no separate
//! Bold file, so
//!
//! * the regular cut is an `fvar` instance, and
//! * bold is the `wght`≈700 instance of the SAME bytes (W9),
//!
//! and both have to be grid-fitted AT THEIR INSTANCE. A hinter pinned to the
//! font's default location would still "work" — it would just silently draw
//! the wrong weight — which is precisely the failure
//! [`bold_stays_bold_when_grid_fitted`] exists to catch.
//!
//! DELIBERATELY NOT SKIPPABLE. A test that quietly returns when it cannot find
//! its fixture passes forever and proves nothing, so this one FAILS with the
//! inventory in the message instead. Every Windows 11 image ships Cascadia
//! Mono, and Windows 10 ships Bahnschrift, so the requirement — one `.ttf`
//! under `%WINDIR%\Fonts` with a `wght` axis reaching a bold cut — is met by
//! any desktop install this app targets.

#![cfg(all(windows, feature = "embedded-font"))]

use aterm_render::{Renderer, StyleBits, Theme};

/// Locate a VARIABLE system face to instantiate: Cascadia Mono first (it is
/// the platform default the shipping candidate list picks), then any other
/// `%WINDIR%\Fonts` face whose `wght` axis reaches a real bold cut — the
/// precondition W9's `vf_bold_coords` needs before bold is an INSTANCE rather
/// than synthetic dilation, which is what these tests are about.
fn variable_system_face() -> (String, Vec<u8>) {
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
    let mut candidates: Vec<String> = vec![format!("{dir}\\Fonts\\CascadiaMono.ttf")];
    let mut scanned = 0usize;
    if let Ok(rd) = std::fs::read_dir(format!("{dir}\\Fonts")) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ttf"))
                && let Some(s) = p.to_str()
            {
                scanned += 1;
                candidates.push(s.to_string());
            }
        }
    }
    for path in candidates {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let usable = aterm_render::variation::probe(&bytes, 0).is_some_and(|p| {
            p.axes.iter().any(|a| {
                a.tag == aterm_render::variation::WGHT_TAG
                    && a.max >= a.def + 100.0
                    && a.max >= 600.0
            })
        });
        if usable {
            eprintln!("windows_grid_fit: variable face = {path}");
            return (path, bytes);
        }
    }
    panic!(
        "no variable font with a bold-reaching `wght` axis under {dir}\\Fonts \
         ({scanned} .ttf files scanned) — Cascadia Mono (Win11) or Bahnschrift (Win10) \
         is expected on any desktop Windows this app targets"
    );
}

/// Coverage histogram of one rasterized glyph: `(solid, fringe, inked, mass)`.
/// `solid` = texels at >= 240/255 (a whole pixel of ink), `fringe` = the
/// 40..=215 mid-greys a smeared stem is made of, `mass` = total coverage,
/// which grid fitting redistributes but must not manufacture.
fn histogram(cov: &[u8]) -> (usize, usize, usize, u64) {
    let mut solid = 0;
    let mut fringe = 0;
    let mut inked = 0;
    let mut mass = 0u64;
    for &a in cov {
        mass += u64::from(a);
        if a >= 8 {
            inked += 1;
        }
        if a >= 240 {
            solid += 1;
        } else if (40..=215).contains(&a) {
            fringe += 1;
        }
    }
    (solid, fringe, inked, mass)
}

/// The coverage mask the renderer would blit for `ch` at the current mode.
fn coverage(r: &mut Renderer, ch: char, style: StyleBits) -> Vec<u8> {
    let key = r.glyph_key_styled(ch, style);
    match r.glyph_image(key) {
        aterm_render::GlyphImage::Mono { bytes, .. }
        | aterm_render::GlyphImage::Rgba { bytes, .. } => bytes.clone(),
    }
}

/// Build a renderer on the variable system face at `px`, in `mode`.
fn renderer(bytes: &[u8], px: f32, mode: &str) -> Renderer {
    let mut r = Renderer::from_bytes(bytes, px, Theme::default()).expect("system face parses");
    r.set_font_hinting(mode);
    r
}

/// The headline: on the platform default VARIABLE face, at the desktop
/// 12px, grid fitting concentrates the stem — more whole-ink texels, fewer
/// mid-grey ones — instead of the raw outline fill Windows used to ship.
#[test]
fn variable_primary_is_grid_fitted_at_12px() {
    let (path, bytes) = variable_system_face();
    for px in [12.0f32, 13.0, 16.0] {
        let off = coverage(&mut renderer(&bytes, px, "off"), 'l', StyleBits::REGULAR);
        let full = coverage(&mut renderer(&bytes, px, "full"), 'l', StyleBits::REGULAR);
        assert_ne!(
            off, full,
            "{path} @{px}px: hinting must change the raster (it did nothing on Windows before)"
        );
        let (s_off, f_off, _, m_off) = histogram(&off);
        let (s_full, f_full, _, m_full) = histogram(&full);
        assert!(
            s_full > s_off,
            "{path} @{px}px: grid fitting must raise whole-ink texels \
             (off {s_off} -> full {s_full})"
        );
        assert!(
            f_full < f_off,
            "{path} @{px}px: grid fitting must cut mid-grey fringe \
             (off {f_off} -> full {f_full})"
        );
        // Ink is REDISTRIBUTED, not invented: a "crisper" raster that simply
        // painted more would be a weight change wearing crispness' clothes.
        let ratio = m_full as f64 / m_off.max(1) as f64;
        assert!(
            (0.75..=1.25).contains(&ratio),
            "{path} @{px}px: coverage mass must be conserved (off {m_off} -> full {m_full})"
        );
    }
}

/// The instance half, and the reason `hinted_glyph` carries `(coords, slot)`
/// at all: Cascadia Mono has no Bold FILE, so a bold cell draws the `wght`≈700
/// INSTANCE. Grid-fitting it must fit THAT outline — a hinter built at the
/// font's default location would quietly hand back the 400 cut, and bold would
/// stop being bold. Bound as "the hinted bold still out-inks the hinted
/// regular by as much as the unhinted pair does".
#[test]
fn bold_stays_bold_when_grid_fitted() {
    let (path, bytes) = variable_system_face();
    let px = 12.0f32;
    let mut off = renderer(&bytes, px, "off");
    let mut full = renderer(&bytes, px, "full");
    // 'M' has two diagonals plus two stems: the most weight-sensitive ASCII
    // glyph, and one every mono face draws.
    let (.., m_reg_off) = histogram(&coverage(&mut off, 'M', StyleBits::REGULAR));
    let (.., m_bold_off) = histogram(&coverage(&mut off, 'M', StyleBits::BOLD));
    let (.., m_reg_full) = histogram(&coverage(&mut full, 'M', StyleBits::REGULAR));
    let (.., m_bold_full) = histogram(&coverage(&mut full, 'M', StyleBits::BOLD));
    // Precondition: this face really does have a heavier bold cut to lose.
    assert!(
        m_bold_off > m_reg_off,
        "{path}: fixture precondition — the unhinted bold must out-ink regular \
         (regular {m_reg_off}, bold {m_bold_off})"
    );
    let gain_off = m_bold_off as f64 / m_reg_off.max(1) as f64;
    let gain_full = m_bold_full as f64 / m_reg_full.max(1) as f64;
    assert!(
        gain_full >= gain_off * 0.9,
        "{path}: the grid-fitted bold lost its weight — hinting reset the \
         instance to the fvar default (bold/regular ink ratio {gain_off:.3} unhinted \
         vs {gain_full:.3} hinted)"
    );
    // And it is genuinely grid-fitted, not merely unchanged.
    assert_ne!(
        coverage(&mut off, 'M', StyleBits::BOLD),
        coverage(&mut full, 'M', StyleBits::BOLD),
        "{path}: the BOLD cell must be grid-fitted too"
    );
}

/// Grid fitting is a COVERAGE change only: the cell box the grid is laid out
/// on must be identical in every mode, at every size. (The advance the hinted
/// path reports is the linear `hmtx` value by construction — see
/// `hinted::hinted_glyph_raster` — and this is the renderer-level twin.)
#[test]
fn grid_geometry_is_mode_invariant() {
    let (path, bytes) = variable_system_face();
    for px in [12.0f32, 13.0, 16.0] {
        let base = renderer(&bytes, px, "off").cell_size();
        for mode in ["full", "light", "native", "off"] {
            assert_eq!(
                renderer(&bytes, px, mode).cell_size(),
                base,
                "{path} @{px}px: mode {mode:?} moved the cell box"
            );
        }
    }
}
