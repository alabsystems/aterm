// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Script-support coverage tests across writing systems.

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

#[test]
fn every_script_resolves_to_a_face_that_really_contains_it() {
    let mut r = aterm_render::Renderer::from_system(28.0, aterm_render::Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    r.debug_block_on_lazy_fallbacks();
    let mut missing: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let mut file_picks = 0usize;
    let mut rasterized = 0usize;
    for (script, sample) in SAMPLES {
        for ch in sample.chars() {
            checked += 1;
            if let Some(path) = r.debug_fallback_pick_path(ch) {
                file_picks += 1;
                let bytes = std::fs::read(&path).unwrap_or_else(|e| {
                    panic!(
                        "{script}: {ch:?} (U+{:04X}) picked unreadable {path}: {e}",
                        ch as u32
                    )
                });
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
            let key = r.glyph_key(ch);
            let is_notdef = key.source == aterm_render::FaceId::Primary
                && key.glyph_class == aterm_render::GlyphClass::Mono;
            if !is_notdef {
                rasterized += 1;
                let img = r.glyph_image(key);
                if !img.bytes().iter().any(|&b| b > 0) {
                    missing.push(format!(
                        "{script}: {ch:?} (U+{:04X}) resolved to {:?} ({:?}) with zero ink",
                        ch as u32, key.source, key.glyph_class
                    ));
                }
            }
        }
    }
    assert!(checked > 50, "matrix ran vacuously ({checked} code points)");
    assert!(
        rasterized > 0,
        "no sample code point resolved to a non-.notdef glyph ({file_picks} file picks)"
    );
    assert!(
        missing.is_empty(),
        "coverage lies:\n  {}",
        missing.join("\n  ")
    );
}
