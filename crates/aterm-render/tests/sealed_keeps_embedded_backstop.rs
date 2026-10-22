// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sealing a font generation closes pathname discovery, not the bundled symbol
//! face.
//!
//! `Renderer::seal_admitted_font_sources` lets a generation be rebuilt (zoom,
//! theme flip) with no font I/O: it settles every path-backed source and closes
//! the runtime system resolver, whose candidate scan would otherwise reopen the
//! host mid-frame on the render thread. Every GUI window seals its backend, so
//! the sealed chain must keep the Symbols Nerd Font compiled into the binary,
//! the guaranteed backstop (Monaco, SF Mono and the stock macOS fallback faces
//! all lack `❯` U+276F; the bundled face has it). The property: if a face in
//! the resolved chain covers `ch`, the resolver does not return `.notdef`;
//! sealing may narrow the chain but never drop a loaded face that needs no I/O.
//!
//! Machine-independent: `Renderer::from_bytes` leaves every lazy system-font
//! path list empty, so the bundled face is the only thing that can serve the
//! probe code point.

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

/// `(source, width, height, ink)`: a regression reads as the `.notdef` row.
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
/// The seal admits the colour-emoji chain eagerly because that is its last
/// chance: the seal then clears `color_font_paths`, and nothing loads a colour
/// face from a path after it, so a deferred read would lose emoji for the
/// process's life with no diagnostic. Admission maps the file, so it is cheap
/// (`seal_maps_discovered_faces.rs`).
///
/// Host-dependent (a system face): a host without one reports COULD NOT RUN.
/// Not on Windows: Segoe UI Symbol maps a monochrome 🚀, which outranks the
/// colour face by design.
#[cfg(not(windows))]
#[test]
fn sealing_a_system_generation_keeps_the_colour_emoji_face() {
    // Every macOS ships a colour face; Linux has one when Noto Color Emoji is
    // installed.
    let noto = "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf";
    if !cfg!(target_os = "macos") && !std::path::Path::new(noto).is_file() {
        aterm_spec::host::could_not_run(format!("{noto} is not installed here"));
    }
    let theme = Theme::default();
    let mut unsealed = Renderer::from_system(18.0, theme)
        .expect("no font: build with aterm-render's default embedded-font feature");
    // Settle the lazy loads first: emoji dispatch is decided only after every
    // mono face has had its chance to miss.
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
    // Resolve before the rebuild, so the decision cache is populated and carried.
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
