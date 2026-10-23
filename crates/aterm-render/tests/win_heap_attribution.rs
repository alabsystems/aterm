// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg(windows)]

#[expect(
    dead_code,
    reason = "the report half is the `full_font_stack_heap` bench's; this gate reads `measure`"
)]
#[path = "common/heapprof.rs"]
mod heapprof;

#[global_allocator]
static PROF: heapprof::Prof = heapprof::Prof;

use aterm_render::{FaceId, Renderer, Theme};

const MAX_SEAL_LIVE_KB: i64 = 4 * 1024;

const MAX_SEAL_TRANSIENT_KB: i64 = 4 * 1024;

#[test]
fn the_font_seal_maps_its_system_faces_and_keeps_no_heap_copy() {
    let mut r = Renderer::from_system(14.0, Theme::default())
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
    let (sources, live_kb, peak_kb) = heapprof::measure(|| r.seal_admitted_font_sources());
    let (mapped, copied) = sources.discovered_residency();
    eprintln!(
        "seal: live +{live_kb} kB, own transient peak +{peak_kb} kB; discovered faces \
         mapped {} kB, copied {} kB",
        mapped / 1024,
        copied / 1024
    );
    assert!(
        mapped + copied > 0,
        "the seal admitted none of the candidate files this host has, so the bounds \
         below would pass vacuously"
    );
    assert_eq!(
        copied,
        0,
        "the seal COPIED {} kB of discovered faces into the heap; every built-in \
         Windows chain/symbol/colour face lives in the system font directory and must \
         be MAPPED (`font_file::maps_in_place`)",
        copied / 1024
    );
    assert!(
        live_kb <= MAX_SEAL_LIVE_KB,
        "the font seal kept {live_kb} kB of heap (cap {MAX_SEAL_LIVE_KB} kB). Either a \
         face is a heap copy again, or a chain/symbol/colour face is being \
         fontdue-PARSED eagerly. See docs/measured/win-heap-2026-08-29.md"
    );
    assert!(
        peak_kb - live_kb <= MAX_SEAL_TRANSIENT_KB,
        "the font seal allocated {} kB it did not keep (cap {MAX_SEAL_TRANSIENT_KB} kB). \
         The discovery read is copying its buffer instead of handing it to \
         `intern_discovered_font_bytes`. See docs/measured/win-heap-2026-08-29.md §5",
        peak_kb - live_kb
    );

    let mut unchecked = Vec::new();
    for (ch, what, files) in [
        ('\u{D55C}', "Hangul", &["malgun.ttf"][..]),
        ('\u{4F60}', "a CJK ideograph", &["msyh.ttc"][..]),
        (
            '\u{1F680}',
            "an emoji",
            &["seguiemj.ttf", "seguisym.ttf"][..],
        ),
    ] {
        let fonts = std::path::Path::new("C:\\Windows\\Fonts");
        if !files.iter().any(|file| fonts.join(file).is_file()) {
            unchecked.push(format!("{what} (none of {files:?})"));
            continue;
        }
        let key = r.glyph_key(ch);
        let source = key.source;
        let ink = r
            .glyph_image(key)
            .bytes()
            .iter()
            .filter(|&&b| b > 0)
            .count();
        eprintln!("draw: U+{:04X} -> {source:?} ink={ink}", u32::from(ch));
        assert_ne!(
            source,
            FaceId::Primary,
            "{what} fell back to the primary face: the mapped {files:?} was lost"
        );
        assert!(ink > 0, "{what} drew no ink from its mapped face {files:?}");
    }
    if !unchecked.is_empty() {
        aterm_spec::host::could_not_run(format!("no face here to draw {}", unchecked.join(", ")));
    }
}
