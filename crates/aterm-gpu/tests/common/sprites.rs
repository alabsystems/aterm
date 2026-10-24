// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GPU suites' sprite fixtures: straight-RGBA arts, and the companion
//! layers ([`aterm_render::sprites`]) a frame carries its sprites in.

use std::sync::Arc;

use aterm_core::render::{FxArt, FxLayer, FxTexels, RenderInput};
use aterm_render::sprites::{FreeSprite, FreeZ, SpriteLayers};

/// The fixtures' publisher salt.
pub const SALT: u64 = 0x7465_7374_2D67_7075;

/// A `w × h` straight-RGBA art whose texel `(x, y)` is `texel(x, y)`.
pub fn art_of(w: u16, h: u16, texel: impl Fn(u16, u16) -> [u8; 4]) -> Arc<FxArt> {
    let mut data = Vec::with_capacity(usize::from(w) * usize::from(h) * 4);
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&texel(x, y));
        }
    }
    Arc::new(FxArt::new(w, h, FxTexels::Rgba8Straight, data).expect("a sized art"))
}

/// Texel `(x, y)` of the old suites' 64×64 sprite sheet: a distinct colour a
/// texel, opaque in its top half (`y < 32`), translucent below.
#[must_use]
pub fn sheet_texel(x: u16, y: u16) -> [u8; 4] {
    let (x, y) = (u32::from(x), u32::from(y));
    let a = if y < 32 {
        255
    } else {
        (60 + (x * 3) % 180) as u8
    };
    [
        (x * 37 + y * 11) as u8,
        (x * 5 + y * 53) as u8,
        (x * 29 + y * 3) as u8,
        a,
    ]
}

/// The `w × h` cut of the sheet at `(ax, ay)`, as its own art: what a sprite
/// of the old path sampled 1:1 from that sub-rectangle.
pub fn cut(ax: u16, ay: u16, w: u16, h: u16) -> Arc<FxArt> {
    art_of(w, h, |x, y| sheet_texel(ax + x, ay + y))
}

/// The cut [`cut`] takes, every texel opaque.
pub fn opaque(ax: u16, ay: u16, w: u16, h: u16) -> Arc<FxArt> {
    art_of(w, h, |x, y| {
        let [r, g, b, _] = sheet_texel(ax + x, ay + y);
        [r, g, b, 255]
    })
}

/// One straight colour all over.
pub fn solid(rgba: [u8; 4], w: u16, h: u16) -> Arc<FxArt> {
    art_of(w, h, |_, _| rgba)
}

/// All of `art`, 1:1 at `(x, y)`, under the text.
pub fn under(x: i32, y: i32, art: Arc<FxArt>) -> FreeSprite {
    FreeSprite {
        z: FreeZ::UnderText,
        ..FreeSprite::whole(x, y, art)
    }
}

/// All of `art`, 1:1 at `(x, y)`, over the text.
pub fn over(x: i32, y: i32, art: Arc<FxArt>) -> FreeSprite {
    FreeSprite::whole(x, y, art)
}

/// The layers a fresh publisher makes of `sprites`: a frame that extends its
/// fx layers with clones of these each time carries the same layers, as a
/// settled companion's frames do.
pub fn layers_of(sprites: &[FreeSprite]) -> Vec<FxLayer> {
    let mut layers = Vec::new();
    SpriteLayers::new(SALT).publish(sprites, &mut layers);
    layers
}

/// `input` with `sprites` published into its fx layers by a fresh publisher.
pub fn put(input: &mut RenderInput, sprites: &[FreeSprite]) {
    SpriteLayers::new(SALT).publish(sprites, &mut input.fx_layers);
}

/// A publisher that lives across frames, as the companions' does: a sprite
/// that kept its look keeps its layer's content, so a settled frame's layers
/// are its predecessor's and the dirty gate can hold.
pub struct Publisher(SpriteLayers);

impl Default for Publisher {
    fn default() -> Self {
        Self(SpriteLayers::new(SALT))
    }
}

impl Publisher {
    /// `input` with `sprites` published into its fx layers.
    pub fn put(&mut self, input: &mut RenderInput, sprites: &[FreeSprite]) {
        self.0.publish(sprites, &mut input.fx_layers);
    }
}
