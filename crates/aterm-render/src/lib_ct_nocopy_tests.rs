// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn menlo() -> crate::font::FaceBytes {
    font_file::admit_font_file(std::path::Path::new("/System/Library/Fonts/Menlo.ttc"))
        .expect("macOS ships Menlo.ttc")
}

#[test]
fn the_cf_data_presents_the_handles_bytes_in_place() {
    let bytes = menlo();
    let descs = macos_coretext::CtFaceDescriptors::new(bytes.clone()).expect("Menlo parses");
    assert_eq!(
        descs.data_ptr(),
        bytes.as_ptr(),
        "CoreText was handed a COPY of the face bytes, not the mapping itself"
    );
}

#[test]
fn coreframework_owns_a_handle_until_the_data_dies() {
    let mapped = menlo();
    let arc = std::sync::Arc::new(mapped.to_vec());
    let bytes = crate::font::FaceBytes::Vec(std::sync::Arc::clone(&arc));
    assert_eq!(std::sync::Arc::strong_count(&arc), 2);
    let descs = macos_coretext::CtFaceDescriptors::new(bytes).expect("parses");
    assert_eq!(
        std::sync::Arc::strong_count(&arc),
        3,
        "the CFData's deallocator must hold its own strong handle"
    );
    let font = macos_coretext::CtFont::from_descriptors(&descs, 0, 18.0, &[]).expect("font");
    assert_eq!(
        std::sync::Arc::strong_count(&arc),
        4,
        "a font pins the bytes too"
    );
    let glyph = font.rasterize(36, false).expect("draws");
    assert!(
        glyph.bytes.iter().any(|&b| b > 0),
        "real ink through the in-place data"
    );
    drop(font);
    drop(descs);
    let remaining = std::sync::Arc::strong_count(&arc);
    assert!(
        remaining == 1 || remaining == 2,
        "after every handle of ours is gone only CF's (if it still caches the font) \
         may remain, got {remaining}"
    );
    eprintln!("strong count after drop: {remaining} (2 = CoreText still caches the data font)");
}

#[test]
fn a_second_pixel_size_reuses_the_files_parse() {
    let mut r = Renderer::from_system(16.0, Theme::default()).expect(NO_FONT);
    r.rasterizer = RasterKind::CoreText;
    let bytes = r
        .shaping_primary_bytes
        .clone()
        .expect("system primary retained");
    let gid = aterm_font::Face::parse(&bytes, 0)
        .ok()
        .and_then(|f| f.glyph_index('A'))
        .map(|g| g.0)
        .expect("primary has 'A'");
    let kp = bytes.as_ptr() as usize;
    assert!(
        r.ct_glyph(kp, bytes.clone(), 0, gid, 16.0, &[], 0)
            .is_some()
    );
    assert!(
        r.ct_glyph(kp, bytes.clone(), 0, gid, 24.0, &[], 0)
            .is_some()
    );
    assert!(r.ct_glyph(kp, bytes, 0, gid, 24.0, &[], 0).is_some());
    assert_eq!(r.ct_cache.len(), 2, "one CTFont per pixel size");
    assert_eq!(
        r.ct_descriptors.len(),
        1,
        "one CoreText parse per face file, shared by every size and instance"
    );
}
