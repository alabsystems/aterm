// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg(target_os = "macos")]

//! Verify `Renderer::seal_admitted_font_sources` maps discovered faces without copying them.

use aterm_render::{FaceId, Renderer, Theme};

const MIB: u64 = 1024 * 1024;

/// `struct rusage`, declared here because the workspace's generated `libc`
/// does not carry `getrusage`: two 16-byte `timeval`s, then fourteen `c_long`s,
/// the first `ru_maxrss`, 144 bytes in all, the kernel's write size.
#[repr(C)]
struct Rusage {
    _times: [u8; 32],
    ru_maxrss: std::os::raw::c_long,
    _rest: [std::os::raw::c_long; 13],
}

unsafe extern "C" {
    fn getrusage(who: std::os::raw::c_int, usage: *mut Rusage) -> std::os::raw::c_int;
}

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
    if !crate::alone::in_own_process(
        module_path!(),
        "the_seal_maps_every_discovered_face_and_does_not_grow_rss_by_their_size",
    ) {
        return;
    }
    let mut r = Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    let on_disk = |paths: &[String]| paths.iter().any(|p| std::path::Path::new(p).is_file());
    if !(on_disk(r.debug_fallback_candidate_paths())
        || on_disk(r.debug_symbol_candidate_paths())
        || on_disk(r.debug_emoji_candidate_paths()))
    {
        aterm_spec::host::could_not_run(
            "this host has none of the chain/symbol/emoji candidate files",
        );
    }
    let rss_before = max_rss_bytes();
    let sources = r.seal_admitted_font_sources();
    let rss_after = max_rss_bytes();
    let (mapped, copied) = sources.discovered_residency();
    let grew = rss_after.saturating_sub(rss_before);
    eprintln!(
        "seal: discovered faces mapped={} MiB copied={} MiB; max RSS {} MiB -> {} MiB (+{} MiB)",
        mapped / MIB,
        copied / MIB,
        rss_before / MIB,
        rss_after / MIB,
        grew / MIB
    );
    assert!(
        mapped + copied > 0,
        "the seal admitted none of the candidate files this host has"
    );

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

    let (emoji_source, emoji_ink) = ink(&mut r, '\u{1F680}');
    if emoji_source == FaceId::ColorEmoji {
        assert!(emoji_ink > 0, "the mapped colour-emoji face drew no ink");
    }
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
