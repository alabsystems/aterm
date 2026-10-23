// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that sealing font sources retains the bundled symbol backstop and color emoji face.

use aterm_render::{FaceId, Renderer, Theme};

fn dejavu() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/DejaVuSansMono.ttf"
    ))
    .expect("bundled DejaVu asset")
}

/// A Nerd Font PUA icon (powerline branch) the bundled Symbols Nerd Font covers
/// and DejaVu Sans Mono does not, so with a `from_bytes` primary only the
/// embedded backstop can serve it.
#[cfg(feature = "embedded-font")]
const NERD_PUA: char = '\u{E0A0}';

fn renderer() -> Renderer {
    Renderer::from_bytes(&dejavu(), 18.0, Theme::default()).expect("fixture parses")
}

fn fingerprint(r: &mut Renderer, ch: char) -> (FaceId, usize, usize, usize) {
    let key = r.glyph_key(ch);
    let source = key.source;
    let img = r.glyph_image(key);
    (
        source,
        img.width(),
        img.height(),
        img.bytes().iter().filter(|&&b| b > 0).count(),
    )
}

/// A colour emoji, `Emoji_Presentation=Yes`: only the colour face can draw it.
#[cfg(not(windows))]
const EMOJI: char = '\u{1F680}';

/// Conservation for the colour-emoji face, the costliest path-admitted source.
#[cfg(not(windows))]
#[test]
fn sealing_a_system_generation_keeps_the_colour_emoji_face() {
    let noto = "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf";
    if !cfg!(target_os = "macos") && !std::path::Path::new(noto).is_file() {
        aterm_spec::host::could_not_run(format!("{noto} is not installed here"));
    }
    let theme = Theme::default();
    let mut unsealed = Renderer::from_system(18.0, theme)
        .expect("no font: build with aterm-render's default embedded-font feature");
    unsealed.debug_block_on_lazy_fallbacks();
    let before = fingerprint(&mut unsealed, EMOJI);
    assert_eq!(
        before.0,
        FaceId::ColorEmoji,
        "the unsealed generation draws U+1F680 from the host's colour face"
    );

    let mut sealed = Renderer::from_system(18.0, theme).expect("the primary resolved once already");
    sealed.seal_admitted_font_sources();
    let (source, w, h, ink) = fingerprint(&mut sealed, EMOJI);
    assert_eq!(
        source,
        FaceId::ColorEmoji,
        "sealing dropped the colour-emoji face: U+{:04X} resolved to {source:?} \
         after the seal but {:?} before it. The seal clears `color_font_paths`, \
         so a candidate not read DURING the seal is unreachable forever",
        EMOJI as u32,
        before.0
    );
    assert!(
        w > 0 && h > 0 && ink > 0,
        "the sealed colour glyph must actually rasterize, got {w}x{h} ink={ink}"
    );
}

/// `rebuild_from_admitted` (zoom, theme flip) takes only a sealed generation,
/// and the rebuilt renderer keeps the backstop too.
#[cfg(feature = "embedded-font")]
#[test]
fn rebuilt_generation_keeps_the_bundled_symbol_face() {
    let mut r = renderer();
    r.seal_admitted_font_sources();
    let before = fingerprint(&mut r, NERD_PUA);
    assert_eq!(before.0, FaceId::RuntimeFallback);

    let mut rebuilt = r
        .rebuild_from_admitted(24.0, Theme::default())
        .expect("a sealed generation rebuilds");
    let (source, w, h, ink) = fingerprint(&mut rebuilt, NERD_PUA);
    assert_eq!(
        source,
        FaceId::RuntimeFallback,
        "a rebuilt (zoomed) generation must keep the bundled symbol face"
    );
    assert!(
        w > 0 && h > 0 && ink > 0,
        "the rebuilt glyph must rasterize, got {w}x{h} ink={ink}"
    );
}
