// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verifies that anti-aliased procedural glyphs have hard `0`/`255` cell-edge texels.

use aterm_render::procedural;

fn sizes() -> Vec<(usize, usize)> {
    let mut v: Vec<(usize, usize)> = (1..=16usize)
        .flat_map(|w| (1..=16usize).map(move |h| (w, h)))
        .collect();
    v.extend_from_slice(&[
        (9, 19),
        (10, 20),
        (11, 21),
        (12, 24),
        (17, 33),
        (20, 8),
        (24, 48),
        (32, 15),
    ]);
    v
}

fn aa_chars() -> impl Iterator<Item = char> {
    (0x256Du32..=0x2573)
        .chain(0x1FB3C..=0x1FB6F)
        .chain(0xE0B0..=0xE0BF)
        .map(|cp| char::from_u32(cp).unwrap())
}

#[test]
fn aa_family_cell_edge_texels_are_hard() {
    for (w, h) in sizes() {
        for ch in aa_chars() {
            assert!(
                procedural::antialiased(ch),
                "U+{:04X} must be in the AA family set",
                u32::from(ch)
            );
            let cov = procedural::coverage(ch, w, h)
                .unwrap_or_else(|| panic!("{ch:?} must be procedural at {w}x{h}"));
            let edge_idx = (0..w)
                .flat_map(|x| [x, (h - 1) * w + x])
                .chain((0..h).flat_map(|y| [y * w, y * w + w - 1]));
            for i in edge_idx {
                assert!(
                    cov[i] == 0 || cov[i] == 255,
                    "{ch:?} at {w}x{h}: SOFT cell-edge texel {} at index {i} — \
                     the seam-tiling law is broken",
                    cov[i]
                );
            }
        }
    }
}
