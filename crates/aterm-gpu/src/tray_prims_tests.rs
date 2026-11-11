// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_prim_is_the_shaders_64_bytes_at_the_shaders_offsets() {
    let p = TrayPrim::new(kind::DISC, [1, 2, 3, 4], &[5.0, 6.0, 7.0], [8, 9, 10, 11]);
    let bytes = aterm_bits::bytes_of(&p);
    assert_eq!(bytes.len(), 64);
    assert_eq!(&bytes[0..4], &kind::DISC.to_ne_bytes());
    assert_eq!(&bytes[8..10], &1i16.to_ne_bytes(), "rect at 8");
    assert_eq!(&bytes[16..20], &5.0f32.to_ne_bytes(), "geometry at 16");
    assert_eq!(&bytes[56..60], &[8, 9, 10, 11], "colour at 56");
    assert_eq!(p.geom[3..], [0.0; 7]);
}

#[test]
fn a_rect_past_i16_is_clamped_and_an_inverted_one_is_empty() {
    let p = TrayPrim::new(kind::FILL, [-40_000, 0, 40_000, 9], &[], [0; 4]);
    assert_eq!(p.rect, [i16::MIN, 0, i16::MAX, 9]);
    assert!(!p.is_empty());
    assert!(TrayPrim::new(kind::FILL, [5, 0, 5, 9], &[], [0; 4]).is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn offset_zero_is_none_whatever_the_caller_passes() {
    let u = TrayPrimsUniform::new((10, 20), (3, 4), &[[9.0, 9.0], [1.5, -2.0]]);
    assert_eq!(u.offsets[0], [0.0, 0.0]);
    assert_eq!(u.offsets[1], [1.5, -2.0]);
    assert_eq!((u.target, u.origin), ([10.0, 20.0], [3, 4]));
}
