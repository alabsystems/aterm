// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fallback-coverage tests for Unicode cmap selection.

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
    if hangul_face_installed() {
        assert_eq!(
            resolved,
            syllables.len(),
            "a Hangul face IS installed, but only {resolved} of {} syllables \
             resolved to any face — the unresolved ones render as tofu",
            syllables.len()
        );
    } else if resolved == 0 {
        aterm_spec::host::could_not_run("no Hangul fallback face installed on this host");
    }
}

fn hangul_face_installed() -> bool {
    const KNOWN_HANGUL_FACES: &[&str] = &[
        "C:\\Windows\\Fonts\\malgun.ttf",
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        "/System/Library/Fonts/Supplemental/AppleGothic.ttf",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ];
    KNOWN_HANGUL_FACES
        .iter()
        .any(|p| std::path::Path::new(p).is_file())
}
