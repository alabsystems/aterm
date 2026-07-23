// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! E1 (lazy host fonts): the missing-font CLASS drain. A poll-based host
//! (web/wasm) cannot read font files itself; the renderer reports WHICH
//! injectable face class a `.notdef` char needed so the host fetches and
//! injects only that class — an ASCII-only session never pays the
//! multi-hundred-MB emoji/CJK payload.
//!
//! Deterministic on any machine: `Renderer::from_bytes` leaves the lazy
//! system-font path lists empty (only `from_system*` populates them) and every
//! test disables the M3 runtime resolver, exactly like a web host.

use aterm_render::{
    FaceId, MISSING_FONT_CLASS_EMOJI, MISSING_FONT_CLASS_TEXT, Renderer, Theme,
};

fn dejavu() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/DejaVuSansMono.ttf"
    ))
    .expect("bundled DejaVu asset")
}

fn nerd_symbols() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/SymbolsNerdFontMono-Regular.ttf"
    ))
    .expect("bundled Nerd Symbols asset")
}

fn renderer() -> Renderer {
    let mut r = Renderer::from_bytes(&dejavu(), 18.0, Theme::default()).expect("fixture parses");
    // Machine-independent `.notdef`: never consult the system resolver (the
    // same posture the wasm hosts run with — no filesystem there).
    r.set_runtime_font_discovery(false);
    r
}

#[test]
fn covered_chars_report_nothing() {
    let mut r = renderer();
    r.glyph_key('a');
    r.glyph_key('░'); // procedural source
    assert_eq!(r.take_missing_font_classes(), 0);
}

#[test]
fn text_miss_reports_text_class_once_and_drains() {
    let mut r = renderer();
    r.glyph_key('\u{0378}'); // unassigned scalar: no face anywhere covers it
    assert_eq!(r.take_missing_font_classes(), MISSING_FONT_CLASS_TEXT);
    assert_eq!(r.take_missing_font_classes(), 0, "drain resets");
    r.glyph_key('\u{0378}'); // memoized `.notdef` — no re-record
    assert_eq!(r.take_missing_font_classes(), 0);
}

#[test]
fn emoji_miss_reports_emoji_class() {
    let mut r = renderer();
    r.glyph_key('😀');
    assert_eq!(r.take_missing_font_classes(), MISSING_FONT_CLASS_EMOJI);
}

#[test]
fn emoji_dispatch_path_reports_emoji_class_too() {
    let mut r = renderer();
    r.glyph_key_emoji('🚀');
    assert_eq!(r.take_missing_font_classes(), MISSING_FONT_CLASS_EMOJI);
}

#[test]
fn classes_accumulate_across_chars() {
    let mut r = renderer();
    r.glyph_key('中'); // CJK: not in DejaVu Sans Mono
    r.glyph_key('😀');
    assert_eq!(
        r.take_missing_font_classes(),
        MISSING_FONT_CLASS_TEXT | MISSING_FONT_CLASS_EMOJI
    );
}

#[test]
fn injecting_the_missing_face_stops_the_class() {
    let mut r = renderer();
    let branch = '\u{E0A0}'; // Powerline branch: Nerd-Symbols-only coverage
    r.glyph_key(branch);
    assert_eq!(r.take_missing_font_classes(), MISSING_FONT_CLASS_TEXT);
    r.set_fallback_bytes(&nerd_symbols()).expect("nerd face parses");
    // The installer cleared the per-char memos, so the char re-probes and the
    // injected face now covers it: no new miss.
    let key = r.glyph_key(branch);
    assert_eq!(key.source, FaceId::Fallback, "served by the injected face");
    assert_eq!(r.take_missing_font_classes(), 0);
}

#[test]
fn still_uncovered_chars_re_fire_after_injection() {
    let mut r = renderer();
    r.glyph_key('\u{0378}');
    assert_eq!(r.take_missing_font_classes(), MISSING_FONT_CLASS_TEXT);
    r.set_fallback_bytes(&nerd_symbols()).expect("nerd face parses");
    r.glyph_key('\u{0378}'); // memos cleared; still uncovered → re-fires
    assert_eq!(
        r.take_missing_font_classes(),
        MISSING_FONT_CLASS_TEXT,
        "the class can re-fire, so hosts must latch per class"
    );
}

/// macOS system-font test (skips elsewhere): a colour face injected AFTER an
/// emoji already rendered `.notdef` must take over — `install_color_font`
/// drops the `keys` memo (where the TEXT-path dispatch parked the `.notdef`),
/// or the code point would never re-probe. This is the exact lazy-injection
/// sequence a web host performs on an EMOJI-class signal.
#[test]
fn late_color_face_takes_over_after_notdef() {
    let Ok(emoji_bytes) = std::fs::read("/System/Library/Fonts/Apple Color Emoji.ttc") else {
        return; // covered by the fixture-only class-signal tests above
    };
    let mut r = renderer();
    r.glyph_key('😀'); // parks a `.notdef` in the TEXT-path `keys` memo
    assert_eq!(r.take_missing_font_classes(), MISSING_FONT_CLASS_EMOJI);
    r.set_color_font_bytes(emoji_bytes).expect("system emoji face parses");
    let key = r.glyph_key('😀');
    assert_eq!(
        key.source,
        FaceId::ColorEmoji,
        "re-probed through the injected colour face"
    );
    assert_eq!(r.take_missing_font_classes(), 0);
}
