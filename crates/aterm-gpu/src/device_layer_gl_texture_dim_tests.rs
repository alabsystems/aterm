// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{GL_MAX_TEXTURE_DIM, GlMaxTexture};

fn gl_max_texture_dim(raw: i32) -> u32 {
    GlMaxTexture::from_driver(raw).get()
}

/// Bytes of the tallest image plane a texture side `cap` admits, as a u32 product: a raster
/// at the placement side over rasters stacked `cap` rows (`build_image_plane`), `None` where
/// the product wraps.
fn plane_bytes_u32(cap: u32) -> Option<u32> {
    let side = u32::try_from(aterm_render::MAX_PLACEMENT_RASTER_SIDE).expect("16,384 fits");
    side.min(cap).checked_mul(cap)?.checked_mul(4)
}

#[test]
fn a_driver_reporting_65536_is_held_to_the_ceiling() {
    assert_eq!(gl_max_texture_dim(65_536), GL_MAX_TEXTURE_DIM);
    assert_eq!(gl_max_texture_dim(i32::MAX), GL_MAX_TEXTURE_DIM);
    assert_eq!(gl_max_texture_dim(4096), 4096, "a smaller answer stands");
    assert_eq!(gl_max_texture_dim(-1), 0, "a negative answer is no texture");
    assert!(plane_bytes_u32(gl_max_texture_dim(65_536)).is_some());
    // Negative control: the driver's raw answer admits a plane whose bytes wrap a u32.
    assert_eq!(plane_bytes_u32(65_536), None);
}
