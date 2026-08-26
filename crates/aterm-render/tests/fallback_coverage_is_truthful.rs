// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! THE FALLBACK-COVERAGE GATE.
//!
//! A chain face may only WIN a code point it genuinely covers. The gate exists
//! because the tier used to ask fontdue — whose char lookup mis-selects a Mac
//! Roman subtable on Apple `.ttc` fonts, exactly as `primary_unicode_gid`
//! documents for the tier above it — so a face could claim a character it does
//! not have and then draw whatever the bogus id pointed at.
//!
//! On a stock macOS that is not hypothetical: the default CJK fallback is
//! `Hiragino Sans GB.ttc`, a CHINESE face covering no Hangul at all, and it
//! claimed 어 / 스 / 트. `한국어 조합 테스트` rendered `한국糯 조합 테陇腋` —
//! confident, deterministic Han glyphs standing in for Korean, while the
//! syllables fontdue happened not to claim fell through to a face that really
//! covers them and looked perfectly fine. A half-correct line is the worst kind
//! of wrong: it reads as a font-taste problem, not a bug.

/// Every winner must really cover its code point, across the scripts a terminal
/// actually meets. Skips cleanly where no system fonts resolve (CI images).
#[test]
fn a_chain_face_only_wins_a_code_point_it_really_covers() {
    let Some(mut r) = aterm_render::Renderer::from_system(28.0, aterm_render::Theme::default())
    else {
        return;
    };
    r.debug_block_on_lazy_fallbacks();
    // Korean first — the reported break — then the scripts that share the CJK
    // fallback with it, then a spread of other non-Latin writing systems.
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
        // Ask the UNICODE cmap directly, at every face of the collection: if NO
        // face has the character, the chain had no business claiming it.
        let faces = ttf_parser::fonts_in_collection(&bytes).unwrap_or(1);
        let covered = (0..faces).any(|i| {
            ttf_parser::Face::parse(&bytes, i)
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

/// The reported line, end to end: every syllable must land on a face that has
/// it, and they must not be split across faces that disagree about the script.
#[test]
fn korean_resolves_consistently_to_a_face_that_has_hangul() {
    let Some(mut r) = aterm_render::Renderer::from_system(28.0, aterm_render::Theme::default())
    else {
        return;
    };
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
    // NON-VACUITY. `homes` is EMPTY when no face resolves any syllable — i.e.
    // when the whole word is `.notdef` tofu, which is strictly worse than the
    // split above. `0 <= 1` holds, so this test passed happily through exactly
    // that state: on Windows the built-in chain carried no Hangul face at all
    // and `한국어` rendered as three boxes while this gate stayed green. Only
    // assert where a Hangul face is known to be installed, so a genuinely
    // font-less host still skips instead of failing.
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

/// Whether this host has a face that is known to carry Hangul, read from the
/// FILESYSTEM rather than from the candidate list — asking the list would turn
/// "someone deleted the Hangul face from the chain" into a silent skip, i.e.
/// precisely the regression the caller exists to catch.
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
