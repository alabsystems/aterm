// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Missing-font class drain and late face injection tests.

use aterm_render::{FaceId, MISSING_FONT_CLASS_EMOJI, MISSING_FONT_CLASS_TEXT, Renderer, Theme};

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
    r.set_runtime_font_discovery(false);
    r
}

#[test]
fn each_miss_reports_its_own_class() {
    type Probe = fn(&mut Renderer);
    let cases: [(&str, Probe, u8); 5] = [
        (
            "covered + procedural",
            |r| {
                r.glyph_key('a');
                r.glyph_key('░');
            },
            0,
        ),
        (
            "unassigned scalar",
            |r| {
                r.glyph_key('\u{0378}');
            },
            MISSING_FONT_CLASS_TEXT,
        ),
        (
            "emoji via the text path",
            |r| {
                r.glyph_key('😀');
            },
            MISSING_FONT_CLASS_EMOJI,
        ),
        (
            "emoji via the emoji dispatch",
            |r| {
                r.glyph_key_emoji('🚀');
            },
            MISSING_FONT_CLASS_EMOJI,
        ),
        (
            "CJK + emoji accumulate",
            |r| {
                r.glyph_key('中');
                r.glyph_key('😀');
            },
            MISSING_FONT_CLASS_TEXT | MISSING_FONT_CLASS_EMOJI,
        ),
    ];
    for (name, probe, want) in cases {
        let mut r = renderer();
        probe(&mut r);
        assert_eq!(r.take_missing_font_classes(), want, "{name}");
    }
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
fn injecting_the_missing_face_stops_the_class() {
    let mut r = renderer();
    let branch = '\u{E0A0}'; // Powerline branch: Nerd-Symbols-only coverage
    r.glyph_key(branch);
    assert_eq!(r.take_missing_font_classes(), MISSING_FONT_CLASS_TEXT);
    r.set_fallback_bytes(&nerd_symbols())
        .expect("nerd face parses");
    let key = r.glyph_key(branch);
    assert_eq!(key.source, FaceId::Fallback, "served by the injected face");
    assert_eq!(r.take_missing_font_classes(), 0);
}

#[test]
fn still_uncovered_chars_re_fire_after_injection() {
    let mut r = renderer();
    r.glyph_key('\u{0378}');
    assert_eq!(r.take_missing_font_classes(), MISSING_FONT_CLASS_TEXT);
    r.set_fallback_bytes(&nerd_symbols())
        .expect("nerd face parses");
    r.glyph_key('\u{0378}'); // memos cleared; still uncovered → re-fires
    assert_eq!(
        r.take_missing_font_classes(),
        MISSING_FONT_CLASS_TEXT,
        "the class can re-fire, so hosts must latch per class"
    );
}

/// A colour face injected after an emoji drew `.notdef` takes over, because
/// `install_color_font` drops the `keys` memo where the text-path dispatch
/// parked it. This is the injection sequence a web host runs on an
/// emoji-class signal.
#[cfg(target_os = "macos")]
#[test]
fn late_color_face_takes_over_after_notdef() {
    let emoji_bytes = std::fs::read("/System/Library/Fonts/Apple Color Emoji.ttc")
        .expect("macOS ships Apple Color Emoji");
    let mut r = renderer();
    r.glyph_key('😀'); // parks a `.notdef` in the TEXT-path `keys` memo
    assert_eq!(r.take_missing_font_classes(), MISSING_FONT_CLASS_EMOJI);
    r.set_color_font_bytes(emoji_bytes)
        .expect("system emoji face parses");
    let key = r.glyph_key('😀');
    assert_eq!(
        key.source,
        FaceId::ColorEmoji,
        "re-probed through the injected colour face"
    );
    assert_eq!(r.take_missing_font_classes(), 0);
}
