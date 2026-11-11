// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The frame palette's texture is the layout `cell.glsl`'s `fx_ink` reads:
/// entry `j` at `(j % FX_PALETTE_ROW, j / FX_PALETTE_ROW)`, holding the
/// word's little-endian bytes, over whole rows and a partial last row.
#[test]
fn the_fx_palette_texture_holds_entry_j_at_its_texel() {
    let dev = crate::gl::test_device();
    let row = fx_gpu::FX_PALETTE_ROW;
    let mut tex = None;
    let mut heights = Vec::new();
    for n in [3usize, 1024, 2500, 7] {
        let words: Vec<u32> = (0..n as u32)
            .map(|j| j.wrapping_mul(0x9e37_79b9) ^ (j << 7))
            .collect();
        write_fx_palette(&dev, &mut tex, &words).expect("written");
        let t = tex.as_ref().expect("minted");
        heights.push(t.height);
        let rows = n.div_ceil(row as usize) as u32;
        let bytes = dev
            .read_rgba8(t, row, rows, row as usize * 4)
            .expect("read back");
        for (j, w) in words.iter().enumerate() {
            assert_eq!(bytes[4 * j..4 * j + 4], w.to_le_bytes(), "entry {j} of {n}");
        }
    }
    assert_eq!(heights, [1, 1, 4, 4], "reused while it fits");
}

/// The palette ring writes three distinct textures in turn, so no frame
/// rewrites a texture either of the two frames before it bound.
#[test]
fn the_fx_palette_ring_never_rewrites_the_last_two_frames_texture() {
    let mut live = GlArmLive::on(crate::gl::test_device()).expect("the arm's resources");
    let names: Vec<u32> = (0..7u32)
        .map(|f| {
            let slot = live.upload_fx_palette(&[f]).expect("written");
            live.fx_palettes[slot].as_ref().expect("written").name
        })
        .collect();
    for w in names.windows(3) {
        assert!(
            w[0] != w[1] && w[1] != w[2] && w[0] != w[2],
            "three frames in a row wrote {w:?} ({names:?})"
        );
    }
    assert_eq!(names[..4], [names[0], names[1], names[2], names[0]]);
}
