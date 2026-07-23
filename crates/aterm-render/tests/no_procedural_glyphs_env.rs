// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `ATERM_NO_PROCEDURAL_GLYPHS=1` escape hatch: with the var set, box
//! drawing dispatches to the FONT (primary face), not the procedural source.
//!
//! Runs in its own test binary so `std::env::set_var` is safe: no other
//! thread reads the environment concurrently (the same convention as the
//! aterm-containment env tests).

use aterm_render::{FaceId, Renderer, Theme};

#[test]
fn env_var_restores_font_glyphs_for_box_drawing() {
    // SAFETY: single-threaded test binary — no concurrent env reads.
    unsafe {
        std::env::set_var("ATERM_NO_PROCEDURAL_GLYPHS", "1");
    }

    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system mono font found");
        return;
    };
    for ch in ['─', '│', '┼', '█', '\u{28FF}'] {
        let key = r.glyph_key(ch);
        assert_ne!(
            key.source,
            FaceId::Procedural,
            "{ch:?} must fall back to font dispatch under ATERM_NO_PROCEDURAL_GLYPHS=1"
        );
    }
}
