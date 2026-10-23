// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows system variable-font grid-fitting tests.

#![cfg(all(windows, feature = "embedded-font"))]

use aterm_render::{Renderer, StyleBits, Theme};

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

fn coverage(r: &mut Renderer, ch: char, style: StyleBits) -> Vec<u8> {
    let key = r.glyph_key_styled(ch, style);
    match r.glyph_image(key) {
        aterm_render::GlyphImage::Mono { bytes, .. }
        | aterm_render::GlyphImage::Rgba { bytes, .. } => bytes.clone(),
    }
}

fn renderer(bytes: &[u8], px: f32, mode: &str) -> Renderer {
    let mut r = Renderer::from_bytes(bytes, px, Theme::default()).expect("system face parses");
    r.set_font_hinting(mode);
    r
}

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
        if px == 12.0 {
            assert!(
                f_full < f_off,
                "{path} @{px}px: grid fitting must cut mid-grey fringe \
                 (off {f_off} -> full {f_full})"
            );
        } else {
            assert!(
                f_full <= f_off,
                "{path} @{px}px: grid fitting must not add mid-grey fringe \
                 (off {f_off} -> full {f_full})"
            );
        }
        let ratio = m_full as f64 / m_off.max(1) as f64;
        assert!(
            (0.75..=1.25).contains(&ratio),
            "{path} @{px}px: coverage mass must be conserved (off {m_off} -> full {m_full})"
        );
    }
}

#[test]
fn bold_stays_bold_when_grid_fitted() {
    let (path, bytes) = variable_system_face();
    let px = 12.0f32;
    let mut off = renderer(&bytes, px, "off");
    let mut full = renderer(&bytes, px, "full");
    let (.., m_reg_off) = histogram(&coverage(&mut off, 'M', StyleBits::REGULAR));
    let (.., m_bold_off) = histogram(&coverage(&mut off, 'M', StyleBits::BOLD));
    let (.., m_reg_full) = histogram(&coverage(&mut full, 'M', StyleBits::REGULAR));
    let (.., m_bold_full) = histogram(&coverage(&mut full, 'M', StyleBits::BOLD));
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
    assert_ne!(
        coverage(&mut off, 'M', StyleBits::BOLD),
        coverage(&mut full, 'M', StyleBits::BOLD),
        "{path}: the BOLD cell must be grid-fitted too"
    );
}

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
