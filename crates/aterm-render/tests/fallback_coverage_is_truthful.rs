// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Fallback-coverage gate: a chain face may only win a code point its Unicode cmap
//! really covers. fontdue's char lookup can mis-select a Mac Roman subtable on Apple
//! `.ttc` fonts (as `primary_unicode_gid` documents), letting a face such as
//! `Hiragino Sans GB.ttc`, which has no Hangul, claim Korean syllables and draw Han glyphs.

/// Every winner must really cover its code point, across the scripts a terminal
/// actually meets.
#[test]
fn a_chain_face_only_wins_a_code_point_it_really_covers() {
    let mut r = aterm_render::Renderer::from_system(28.0, aterm_render::Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    r.debug_block_on_lazy_fallbacks();
    // Korean, the scripts sharing its CJK fallback, then other non-Latin scripts.
    let samples = "한국어조합테스트中文字符测试日本語カタカナひらがな\
                   АБВГДЕЖЗ αβγδεζ עברית العربية हिन्दी বাংলা ไทย ᐃᓄᒃᑎᑐᑦ ⠁⠂⠃";
    let mut checked = 0usize;
    for ch in samples
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_ascii())
    {
        let Some(path) = r.debug_fallback_pick_path(ch) else {
            continue; // no chain face claimed it — the runtime tier's problem, not this gate's
        };
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        // Ask the Unicode cmap of every face in the collection directly.
        let faces = aterm_font::fonts_in_collection(&bytes).unwrap_or(1);
        let covered = (0..faces).any(|i| {
            aterm_font::Face::parse(&bytes, i)
                .ok()
                .and_then(|f| f.glyph_index(ch))
                .is_some_and(|g| g.0 != 0)
        });
        assert!(
            covered,
            "{ch:?} (U+{:04X}) was awarded to {path}, which does not contain it in \
             any face of its Unicode cmap — the winner will draw a bogus glyph",
            ch as u32
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "gate ran vacuously — no code point reached a chain face"
    );
}

/// Every syllable of a Korean line lands on one face that has it.
#[test]
fn korean_resolves_consistently_to_a_face_that_has_hangul() {
    let mut r = aterm_render::Renderer::from_system(28.0, aterm_render::Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    r.debug_block_on_lazy_fallbacks();
    let mut homes = std::collections::BTreeSet::new();
    let mut resolved = 0usize;
    let syllables: Vec<char> = "한국어 조합 테스트"
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    for ch in &syllables {
        if let Some(path) = r.debug_fallback_pick_path(*ch) {
            homes.insert(path);
            resolved += 1;
        }
    }
    assert!(
        homes.len() <= 1,
        "one Korean word was split across {} faces {homes:?} — the classic \
         symptom of a face claiming coverage it does not have",
        homes.len()
    );
    // Non-vacuity: an empty `homes` (every syllable tofu) also passes the check above, so
    // where a Hangul face is installed every syllable must resolve.
    if hangul_face_installed() {
        assert_eq!(
            resolved,
            syllables.len(),
            "a Hangul face IS installed, but only {resolved} of {} syllables \
             resolved to any face — the unresolved ones render as tofu",
            syllables.len()
        );
    }
}

/// Whether a face known to carry Hangul is on disk. Read from the filesystem, not the
/// candidate list, so dropping the Hangul face from the chain fails instead of skipping.
fn hangul_face_installed() -> bool {
    const KNOWN_HANGUL_FACES: &[&str] = &[
        // Windows
        "C:\\Windows\\Fonts\\malgun.ttf",
        // macOS (Arial Unicode carries the syllables)
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        // Linux
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ];
    KNOWN_HANGUL_FACES
        .iter()
        .any(|p| std::path::Path::new(p).is_file())
}
