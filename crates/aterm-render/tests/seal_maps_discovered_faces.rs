// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// macOS only, and deliberately narrower than `cfg(unix)`: the mapping rule is
// the sealed system font volume (`font_file::maps_in_place` —
// `/System/Library/Fonts`), which is where every discovered face lives on a
// Mac and nowhere else. On Linux the discovered faces are copied by design
// (their package roots are rewritten in place), so `copied == 0` cannot hold
// there; on Windows `getrusage` does not exist and the binary would not link.
#![cfg(target_os = "macos")]

//! RESIDENCY PIN: THE SEAL MAPS ITS DISCOVERED FACES, IT DOES NOT COPY THEM.
//!
//! `Renderer::seal_admitted_font_sources` is the windowed launch's font seal.
//! It admits every path-backed face the generation will ever draw from — on a
//! Mac: Hiragino Sans GB (23.5 MB) and Arial Unicode (23.3 MB) for the broad
//! chain, STIX Two Math (0.8 MB) for the symbol slot, and Apple Color Emoji
//! (192 MB) for colour — and it used to `read()` each one whole into an
//! `Arc<Vec<u8>>` it then held for the process's life: ~240 MB of anonymous,
//! per-process, non-evictable heap at every launch, ~80% of the measured
//! 386 MB-vs-87 MB windowed/headless RSS gap, duplicated per window.
//!
//! Now `font_file::admit_font_file` MAPS a file on the system font volume
//! (`PROT_READ`, `MAP_PRIVATE`) instead. Admission is still eager and still
//! inside the seal — the fd is opened, validated and mapped there, and no
//! pathname is touched afterwards — but the pages are file-backed: shared
//! across processes through the page cache, evictable, and faulted in only
//! for the tables and glyphs actually touched.
//!
//! Two observables, both in this test's own process:
//!  * the generation's own report of how its discovered faces are held
//!    (`AdmittedFontSources::discovered_residency`): every byte mapped, none
//!    copied;
//!  * the process's max RSS across the seal: the seal may not grow it by
//!    anything like the size of the files it admitted. The old seal added the
//!    whole ~240 MB; the bound here is 64 MiB, a quarter of that, with room for
//!    the parse-time page faults and allocator noise of a debug build.
//!
//! Then a colour emoji and a CJK ideograph are drawn from the mapped faces,
//! with real ink — the mapping is not a residency trick that lost the glyphs.

use aterm_render::{FaceId, Renderer, Theme};

const MIB: u64 = 1024 * 1024;

/// `struct rusage`, declared here because the workspace's first-party `libc`
/// does not carry `getrusage` and is generated, not hand-edited. The layout is
/// the same on macOS and Linux/glibc for the field this reads: two 16-byte
/// `timeval`s (`ru_utime`, `ru_stime`) and then fourteen `c_long`s, the first
/// of which is `ru_maxrss`. Only that field is read; the rest is padding to the
/// kernel's write size (144 bytes on both), so an undersized out-parameter is
/// impossible.
#[repr(C)]
struct Rusage {
    _times: [u8; 32],
    ru_maxrss: std::os::raw::c_long,
    _rest: [std::os::raw::c_long; 13],
}

unsafe extern "C" {
    fn getrusage(who: std::os::raw::c_int, usage: *mut Rusage) -> std::os::raw::c_int;
}

/// Peak resident set of this process so far. macOS reports bytes, Linux
/// kilobytes; both are monotone, which is all a before/after needs.
fn max_rss_bytes() -> u64 {
    const RUSAGE_SELF: std::os::raw::c_int = 0;
    let mut usage = Rusage {
        _times: [0; 32],
        ru_maxrss: 0,
        _rest: [0; 13],
    };
    // SAFETY: `usage` is a writable 144-byte `struct rusage`, exactly what
    // `getrusage(RUSAGE_SELF, ..)` fills; it writes nothing else.
    let rc = unsafe { getrusage(RUSAGE_SELF, &mut usage) };
    assert_eq!(rc, 0, "getrusage failed");
    let raw = u64::try_from(usage.ru_maxrss).unwrap_or(0);
    if cfg!(target_os = "macos") {
        raw
    } else {
        raw * 1024
    }
}

fn ink(r: &mut Renderer, ch: char) -> (FaceId, usize) {
    let key = r.glyph_key(ch);
    let source = key.source;
    let img = r.glyph_image(key);
    (source, img.bytes().iter().filter(|&&b| b > 0).count())
}

#[test]
fn the_seal_maps_every_discovered_face_and_does_not_grow_rss_by_their_size() {
    let Some(mut r) = Renderer::from_system(18.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let rss_before = max_rss_bytes();
    let started = std::time::Instant::now();
    let sources = r.seal_admitted_font_sources();
    let seal_wall = started.elapsed();
    let rss_after = max_rss_bytes();
    let (mapped, copied) = sources.discovered_residency();
    let grew = rss_after.saturating_sub(rss_before);
    eprintln!(
        "seal: discovered faces mapped={} MiB copied={} MiB; max RSS {} MiB -> {} MiB (+{} MiB); \
         seal wall {seal_wall:?}",
        mapped / MIB,
        copied / MIB,
        rss_before / MIB,
        rss_after / MIB,
        grew / MIB
    );
    if mapped + copied == 0 {
        eprintln!("SKIP: this host has no discovered chain/symbol/emoji face");
        return;
    }

    assert_eq!(
        copied,
        0,
        "the seal COPIED {} MiB of discovered faces into anonymous heap; every built-in \
         candidate lives on the system font volume and must be MAPPED",
        copied / MIB
    );
    assert!(
        grew < 64 * MIB,
        "the seal grew max RSS by {} MiB while admitting {} MiB of faces — the admissions are \
         resident copies, not file-backed mappings",
        grew / MIB,
        mapped / MIB
    );

    // The mapped faces must still DRAW. Emoji first (the biggest file, and the
    // one whose eager admission `sealed_keeps_embedded_backstop` guards); then
    // a CJK ideograph from the broad chain. Either may legitimately be absent
    // on a host without that face, so each asserts only if it resolved there.
    let (emoji_source, emoji_ink) = ink(&mut r, '\u{1F680}');
    if emoji_source == FaceId::ColorEmoji {
        assert!(emoji_ink > 0, "the mapped colour-emoji face drew no ink");
    }
    // The FIRST CJK draw is where the macOS raster path builds its CoreText
    // font for the chain face. `CtFont::new` used to `CFDataCreate` — the
    // COPYING constructor — over the whole 23.5 MB Hiragino collection: the
    // copy faulted every mapped page in AND added 23.5 MB of anonymous heap,
    // per (face, px). With `CFDataCreateWithBytesNoCopy` over the mapping
    // CoreText reads the tables it needs and nothing else, so one glyph costs
    // a few pages, not the file.
    // Pay the process's FIRST CoreText raster before measuring: on macOS the
    // first `CTFontDrawGlyphs`/`CGContextRelease` in a process allocates
    // ~250 MiB of CoreText/CoreGraphics heap once, whatever the font (measured
    // 2026-09-06: 1.0M CFStrings + 504k NSMutableArrays). A windowed process
    // pays it at its post-seal prewarm; it is not the per-font cost this
    // assertion is about.
    let (ascii_source, _) = ink(&mut r, 'A');
    assert_eq!(ascii_source, FaceId::Primary);
    let rss_before_cjk = max_rss_bytes();
    let cjk_key = r.glyph_key('\u{4F60}');
    let rss_after_key = max_rss_bytes();
    let cjk_source = cjk_key.source;
    let cjk_ink = r
        .glyph_image(cjk_key)
        .bytes()
        .iter()
        .filter(|&&b| b > 0)
        .count();
    let rss_after_image = max_rss_bytes();
    eprintln!(
        "draw: CJK glyph_key grew max RSS by {} MiB, glyph_image by {} MiB",
        rss_after_key.saturating_sub(rss_before_cjk) / MIB,
        rss_after_image.saturating_sub(rss_after_key) / MIB
    );
    let cjk_grew = rss_after_image.saturating_sub(rss_before_cjk);
    if matches!(cjk_source, FaceId::Fallback | FaceId::RuntimeFallback) {
        assert!(cjk_ink > 0, "the mapped chain face drew no ink for U+4F60");
        eprintln!(
            "draw: first CJK glyph grew max RSS by {} MiB",
            cjk_grew / MIB
        );
        assert!(
            cjk_grew < 16 * MIB,
            "the first CJK glyph grew max RSS by {} MiB — the CoreText font was built \
             over a COPY of the 23 MB chain face instead of the mapping",
            cjk_grew / MIB
        );
    }
    eprintln!(
        "draw: U+1F680 -> {emoji_source:?} ink={emoji_ink}; U+4F60 -> {cjk_source:?} ink={cjk_ink}"
    );
}
