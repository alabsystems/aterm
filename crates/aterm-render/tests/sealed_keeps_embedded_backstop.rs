// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! REGRESSION PIN (FONT-2b / the "tofu box"): SEALING A FONT GENERATION CLOSES
//! PATHNAME DISCOVERY, NOT THE BUNDLED SYMBOL FACE.
//!
//! `Renderer::seal_admitted_font_sources` exists so a generation can be rebuilt
//! (zoom, theme flip) with **no font I/O** — it settles every path-backed
//! primary/style/fallback/symbol/emoji source and closes the runtime system
//! resolver, whose candidate scan would otherwise reopen the host mid-frame on
//! the render thread.
//!
//! It used to close the give-up path *wholesale*, which also cut off the
//! GUARANTEED backstop: the Symbols Nerd Font `include_bytes!`'d into the
//! binary. Because every GUI window seals its backend
//! (`aterm-gui/src/app_window.rs`), that made the compiled-in face structurally
//! unreachable in the shipping app — so a code point a **loaded** face covered
//! still rendered `.notdef`, forever.
//!
//! Owner report, 2026-07-24: the Claude Code prompt glyph `❯` (U+276F) drew as a
//! tofu box. On that machine Monaco (the configured primary), SFNSMono, Arial
//! Unicode, Apple Symbols, STIX Two Math and Hiragino Sans GB all lack U+276F;
//! the bundled face has it. Measured `.notdef` fingerprint at 26 px was
//! `(FaceId::Primary, 11x20, ink 145)` — and that is exactly what U+276F drew.
//!
//! The property pinned here is CONSERVATION: *if a face in the resolved chain
//! covers `ch`, the resolver must not return `.notdef`.* Sealing may narrow
//! WHICH faces are in the chain; it may never drop one that is already loaded
//! and needs no I/O to consult.
//!
//! Machine-independent by construction: the primary is loaded with
//! `Renderer::from_bytes`, which leaves every lazy system-font path list empty
//! (only `from_system*` populates them), so the bundled face is the ONLY thing
//! that can serve the probe code point on any host.

use aterm_render::{FaceId, Renderer, Theme};

fn dejavu() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/DejaVuSansMono.ttf"
    ))
    .expect("bundled DejaVu asset")
}

/// A Nerd Font Private-Use-Area icon (powerline branch). The bundled Symbols
/// Nerd Font covers it; DejaVu Sans Mono does not — verified against both
/// assets' cmaps — so with a `from_bytes` primary and no system chain, ONLY the
/// embedded backstop can serve it.
const NERD_PUA: char = '\u{E0A0}';

/// A noncharacter: no font anywhere covers it, so it is the honest `.notdef`
/// control. The fix must not make the resolver invent coverage.
const NONCHARACTER: char = '\u{FDD2}';

fn renderer() -> Renderer {
    Renderer::from_bytes(&dejavu(), 18.0, Theme::default()).expect("fixture parses")
}

/// `(source, width, height, ink)` — the same fingerprint the owner's bug was
/// diagnosed with, so a regression reproduces as an identical-to-`.notdef` row.
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

#[test]
fn sealed_generation_still_reaches_the_bundled_symbol_face() {
    let mut r = renderer();
    r.seal_admitted_font_sources();

    let (source, w, h, ink) = fingerprint(&mut r, NERD_PUA);
    assert_eq!(
        source,
        FaceId::RuntimeFallback,
        "a sealed generation must still route U+{:04X} to the bundled symbol face; \
         got {source:?} — the embedded backstop was cut off by sealing again",
        NERD_PUA as u32
    );
    // The KEY saying RuntimeFallback is not enough: the rasterizer recovers the
    // face from the decision caches, and if that lookup misses it silently draws
    // `.notdef` under a RuntimeFallback-labelled key — a tofu that the source
    // check alone would wave through. Demand real ink.
    assert!(
        w > 0 && h > 0 && ink > 0,
        "the routed glyph must actually rasterize, got {w}x{h} ink={ink}"
    );
}

/// The give-up path must still give up when it genuinely should: sealing may not
/// be repaired by making the resolver claim coverage nothing has.
#[test]
fn sealed_generation_still_reports_a_real_miss() {
    let mut r = renderer();
    r.seal_admitted_font_sources();
    let (source, ..) = fingerprint(&mut r, NONCHARACTER);
    assert_eq!(
        source,
        FaceId::Primary,
        "a noncharacter no face covers must still resolve to the `.notdef` give-up"
    );
}

/// CONSERVATION, stated directly: sealing must never turn a code point that
/// RESOLVED into a `.notdef`.
///
/// Deliberately NOT an exact-raster comparison. Sealing legitimately narrows
/// *which* faces are in the chain — unsealed, the host scan may pick an
/// installed system face for this icon; sealed, only the bundled one remains —
/// so the chosen face, and therefore the glyph's metrics, may differ. What may
/// never differ is WHETHER it resolved at all. (An earlier draft of this test
/// asserted raster equality and failed on exactly that distinction: unsealed
/// picked a 22x20 system face, sealed the 11x20 bundled one. Both are correct.)
#[test]
fn sealing_never_downgrades_a_resolved_code_point_to_notdef() {
    let mut unsealed = renderer();
    let notdef = fingerprint(&mut unsealed, NONCHARACTER);
    let before = fingerprint(&mut unsealed, NERD_PUA);

    let mut sealed = renderer();
    sealed.seal_admitted_font_sources();
    let after = fingerprint(&mut sealed, NERD_PUA);

    // Whatever the unsealed chain could do, the sealed chain must still do.
    assert_ne!(
        before.0,
        FaceId::Primary,
        "precondition: the unsealed chain resolves this code point"
    );
    assert_ne!(
        after.0,
        FaceId::Primary,
        "sealing downgraded a RESOLVED code point to the `.notdef` give-up \
         (unsealed {before:?} -> sealed {after:?}) — this is the tofu bug"
    );
    // And the sealed glyph must not merely be labelled: it must not be the
    // `.notdef` raster wearing a fallback face id.
    assert_ne!(
        (after.1, after.2, after.3),
        (notdef.1, notdef.2, notdef.3),
        "the sealed glyph rasterized to the `.notdef` fingerprint {notdef:?}"
    );
}

/// A colour emoji, `Emoji_Presentation=Yes`: only the colour face can draw it.
const EMOJI: char = '\u{1F680}';

/// CONSERVATION, applied to the one source that is admitted BY PATH and is the
/// most expensive in the generation: the COLOUR-EMOJI face.
///
/// `seal_admitted_font_sources` reads and validates the colour-emoji candidate
/// eagerly, and that is the LAST moment it can be admitted at all: the seal then
/// clears `color_font_paths`, and `ensure_color_font` early-returns on an empty
/// list. A generation that reached publication without those bytes resident
/// could never acquire them — every emoji would resolve away from
/// `FaceId::ColorEmoji` for the process's lifetime, with no diagnostic.
///
/// This test exists because the eager read LOOKS like waste and is not.
/// MEASURED on this machine (macOS, opt-level 0): the candidate
/// `/System/Library/Fonts/Apple Color Emoji.ttc` is 192,123,488 B (183 MiB),
/// a warm-cache read of it takes 9.4 ms, and the whole seal takes ~1.82 s — so
/// the read is ~0.5% of the seal, and it already runs concurrently with the two
/// background fallback parses the seal spawns before it. The 183 MiB of
/// resident bytes is real and unconditional; deferring the READ to "the first
/// emoji that needs one" is nonetheless not an optimization available here, it
/// is a silent feature loss, and this is the gate that says so.
///
/// Host-dependent by nature (the face is a system file), so the UNSEALED
/// resolution is the precondition: a host with no colour-emoji face skips.
#[test]
fn sealing_a_system_generation_keeps_the_colour_emoji_face() {
    let theme = Theme::default();
    let Some(mut unsealed) = Renderer::from_system(18.0, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    // Settle the async lazy loads first: emoji dispatch is decided only after
    // every mono face has had its chance to miss.
    unsealed.debug_block_on_lazy_fallbacks();
    let before = fingerprint(&mut unsealed, EMOJI);
    if before.0 != FaceId::ColorEmoji {
        eprintln!(
            "SKIP: no colour-emoji face on this host (U+1F680 -> {:?})",
            before.0
        );
        return;
    }

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

/// A sealed generation is the only kind `rebuild_from_admitted` accepts (zoom /
/// theme flip). The rebuilt renderer must keep the backstop too — otherwise the
/// tofu returns on the first font-size change.
#[test]
fn rebuilt_generation_keeps_the_bundled_symbol_face() {
    let mut r = renderer();
    r.seal_admitted_font_sources();
    // Resolve BEFORE the rebuild so the decision cache is populated and carried.
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
