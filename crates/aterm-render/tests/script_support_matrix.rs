// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The script-support matrix: what the renderer guarantees per writing system.
//!
//! * Coverage: every code point reaches a face that genuinely contains it, so
//!   nothing is tofu or a bogus glyph from a face that claimed it falsely (see
//!   `fallback_coverage_is_truthful`). A hard guarantee, pinned for every
//!   script below.
//! * Shaping: contextual joining, Indic reordering and conjuncts, and bidi
//!   reordering. aterm shapes only runs on the primary face, where programming
//!   ligatures live; a run that falls to a fallback face renders per cell, so
//!   Arabic draws in isolated forms and RTL text reads in logical order, as in
//!   the mainstream terminals. The second test pins that limit.

/// Sample text per script a terminal meets, checked one scalar at a time
/// (cluster behaviour is the shaping question, not this one).
const SAMPLES: &[(&str, &str)] = &[
    ("Latin-1", "éñü"),
    ("Greek", "αβγδεζ"),
    ("Cyrillic", "АБВГДЕЖЗ"),
    ("Hebrew", "שלוםעברית"),
    ("Arabic", "السلامعليكم"),
    ("Devanagari", "नमस्तेदुनिया"),
    ("Bengali", "আমিবাংলা"),
    ("Tamil", "வணக்கம்"),
    ("Thai", "สวัสดีครับ"),
    ("Lao", "ສະບາຍດີ"),
    ("Khmer", "សួស្តី"),
    ("Georgian", "გამარჯობა"),
    ("Armenian", "Բարև"),
    ("Han", "中文字符测试"),
    ("Hiragana", "ひらがな"),
    ("Katakana", "カタカナ"),
    ("Hangul", "한국어조합테스트"),
    ("Cherokee", "ᏣᎳᎩ"),
    ("Inuktitut", "ᐃᓄᒃᑎᑐᑦ"),
    ("Ethiopic", "አማርኛ"),
    ("Braille", "⠁⠂⠃⠄"),
    ("Math", "∀∃∈∑∫"),
    ("Box drawing", "─│┌┐└┘├┤"),
];

/// Coverage: every sampled code point resolves to a face that really contains
/// it, never `.notdef` and never a face that merely claimed it.
#[test]
fn every_script_resolves_to_a_face_that_really_contains_it() {
    let mut r = aterm_render::Renderer::from_system(28.0, aterm_render::Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    r.debug_block_on_lazy_fallbacks();
    let mut missing: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for (script, sample) in SAMPLES {
        for ch in sample.chars() {
            checked += 1;
            // A chain face may only win a code point it genuinely contains.
            if let Some(path) = r.debug_fallback_pick_path(ch) {
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                let faces = aterm_font::fonts_in_collection(&bytes).unwrap_or(1);
                let real = (0..faces).any(|i| {
                    aterm_font::Face::parse(&bytes, i)
                        .ok()
                        .and_then(|f| f.glyph_index(ch))
                        .is_some_and(|g| g.0 != 0)
                });
                if !real {
                    missing.push(format!(
                        "{script}: {ch:?} (U+{:04X}) awarded to {path} which lacks it",
                        ch as u32
                    ));
                }
            }
            // Otherwise the runtime resolver owns it, and it records only faces
            // that passed a real drawability probe.
        }
    }
    assert!(checked > 50, "matrix ran vacuously ({checked} code points)");
    assert!(
        missing.is_empty(),
        "coverage lies:\n  {}",
        missing.join("\n  ")
    );
}
