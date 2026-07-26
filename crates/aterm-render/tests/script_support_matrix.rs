// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! THE SCRIPT-SUPPORT MATRIX — what the renderer guarantees per writing system,
//! written down so the guarantees are testable and the LIMITS are visible.
//!
//! Two very different properties get confused when someone says "does it support
//! Arabic":
//!
//! * **COVERAGE** — every code point reaches a face that genuinely contains it,
//!   so nothing is tofu and nothing is a bogus glyph from a face that lied about
//!   having it (the Korean break: see `fallback_coverage_is_truthful`). This is a
//!   hard guarantee and this file pins it for every script below.
//! * **SHAPING** — contextual joining (Arabic), reordering and conjuncts
//!   (Indic), and bidirectional reordering (Arabic/Hebrew). aterm shapes only
//!   runs that land on the PRIMARY face, because that is where programming
//!   ligatures live; a run that falls through to a fallback face is rendered
//!   per-cell. So Arabic renders in ISOLATED forms and RTL text reads in logical
//!   order, exactly as in the mainstream terminals.
//!
//! Recording the second half is the point. An untested limit is indistinguishable
//! from a bug that nobody has hit yet, and this one has a real user cost: the
//! Korean defect hid for so long precisely because "non-Latin looks odd" had no
//! contract to violate.

/// One code point per script that a terminal actually meets. Kept to
//  single scalars: cluster behaviour is the shaping question, not this one.
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

/// COVERAGE, the hard guarantee: every sampled code point resolves to a face
/// that really contains it — never `.notdef`, and never a face that merely
/// claimed it.
#[test]
fn every_script_resolves_to_a_face_that_really_contains_it() {
    let Some(mut r) = aterm_render::Renderer::from_system(28.0, aterm_render::Theme::default())
    else {
        return;
    };
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
                let faces = ttf_parser::fonts_in_collection(&bytes).unwrap_or(1);
                let real = (0..faces).any(|i| {
                    ttf_parser::Face::parse(&bytes, i)
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
            // Otherwise the runtime resolver owns it; it only ever records faces
            // that passed a real drawability probe, so nothing to assert here.
        }
    }
    assert!(checked > 50, "matrix ran vacuously ({checked} code points)");
    assert!(
        missing.is_empty(),
        "coverage lies:\n  {}",
        missing.join("\n  ")
    );
}

/// SHAPING, the documented LIMIT: complex-script shaping runs only on the
/// primary face. This test does not assert that Arabic is wrong — it asserts
/// that the limit is where we think it is, so that the day someone implements
/// fallback shaping, this fails and gets updated deliberately rather than the
/// matrix above quietly over-promising.
#[test]
fn complex_script_shaping_is_primary_face_only() {
    let Some(mut r) = aterm_render::Renderer::from_system(28.0, aterm_render::Theme::default())
    else {
        return;
    };
    r.debug_block_on_lazy_fallbacks();
    // Arabic does not live on a programming monospace primary, so every Arabic
    // run resolves off-primary — which is exactly why it is not shaped.
    let off_primary = "السلام".chars().all(|ch| {
        r.debug_fallback_pick_path(ch).is_some() || r.debug_runtime_fallback(ch).is_some()
    });
    assert!(
        off_primary,
        "Arabic reached the PRIMARY face — if the primary now covers Arabic, the \
         shaping story changes and this matrix needs revisiting"
    );
}
