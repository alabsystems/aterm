// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The suite's sprite fixtures: straight-RGBA arts, and the companion layers
//! ([`aterm_render::sprites`]) a frame carries its sprites in.

use std::sync::Arc;

use aterm_core::render::{FxArt, FxTexels, RenderInput};
use aterm_render::sprites::{FreeSprite, FreeZ, SpriteLayers};

/// The fixtures' publisher salt.
pub(crate) const SALT: u64 = 0x7465_7374_2D73_7072;

/// A `w × h` straight-RGBA art whose texel `(x, y)` is `texel(x, y)`.
pub(crate) fn art_of(w: u16, h: u16, texel: impl Fn(u16, u16) -> [u8; 4]) -> Arc<FxArt> {
    let mut data = Vec::with_capacity(usize::from(w) * usize::from(h) * 4);
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&texel(x, y));
        }
    }
    Arc::new(FxArt::new(w, h, FxTexels::Rgba8Straight, data).expect("a sized art"))
}

/// Opaque, a distinct colour a texel.
pub(crate) fn patterned(w: u16, h: u16) -> Arc<FxArt> {
    art_of(w, h, |x, y| {
        let (x, y) = (u32::from(x), u32::from(y));
        [
            (x * 37 + y * 11) as u8,
            (x * 5 + y * 53) as u8,
            (x * 29 + y * 3) as u8,
            255,
        ]
    })
}

/// One straight colour all over.
pub(crate) fn solid(rgba: [u8; 4], w: u16, h: u16) -> Arc<FxArt> {
    art_of(w, h, |_, _| rgba)
}

/// All of `art`, 1:1 at `(x, y)`, under the text.
pub(crate) fn under(x: i32, y: i32, art: Arc<FxArt>) -> FreeSprite {
    FreeSprite {
        z: FreeZ::UnderText,
        ..FreeSprite::whole(x, y, art)
    }
}

/// `input` with `sprites` published into its fx layers by a fresh publisher.
pub(crate) fn put(input: &mut RenderInput, sprites: &[FreeSprite]) {
    SpriteLayers::new(SALT).publish(sprites, &mut input.fx_layers);
}
