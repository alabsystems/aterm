// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Free sprites as fx layers (`docs/DESIGN-card-engine-2026-10-19.md` §6,
//! lane B7): a [`FreeSprite`] is one baked art drawn under the Linear law, and
//! [`SpriteLayers`] publishes a frame's sprites into
//! `RenderInput::fx_layers`, one Grid-space layer a sprite, so identity damage
//! ([`crate::RowBands`]) repaints only the bands a sprite left or reached. The
//! companions publish through it, and the renderers' tests build their
//! sprites with it.

use std::sync::Arc;

use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxSlot, FxSpace, FxSprite, PxRect,
};

/// The salt of the window's companion layers ([`SpriteLayers::new`]): the
/// word engine's sprites and the pet's, published together.
pub const WORD_SPRITE_SALT: u64 = 0x636F_6D70_616E_696F;

/// Where a [`FreeSprite`] draws against the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FreeZ {
    /// Under the glyphs (the pet, its departures, a peeking cat): a Linear
    /// [`FxSlot::Under`] layer that knocks its letters out
    /// ([`FxLayer::knock`]).
    UnderText,
    /// Over the glyphs and the word decorations (the flying head and its
    /// notes, the motes, the dog, the nuke): a Linear [`FxSlot::Over`] layer.
    OverText,
}

/// One companion sprite: `art` (straight RGBA) stretched to `w × h` with its
/// top-left at `(x, y)` in grid-interior pixels, drawn only inside `clip` (a
/// peek's visible window, a pane's box; [`PxRect::EVERYWHERE`] for none),
/// each texel tinted by `tint` and faded by `alpha`.
#[derive(Clone, Debug)]
pub struct FreeSprite {
    /// Left edge of the whole art's destination.
    pub x: i32,
    /// Top edge of the whole art's destination.
    pub y: i32,
    /// Destination width of the whole art.
    pub w: u16,
    /// Destination height of the whole art.
    pub h: u16,
    /// The baked art.
    pub art: Arc<FxArt>,
    /// The visible window, grid-interior pixels.
    pub clip: PxRect,
    /// Multiplied into every texel's colour (`0x00RRGGBB`).
    pub tint: u32,
    /// Multiplied into every texel's alpha.
    pub alpha: u8,
    /// Mirror the art horizontally.
    pub flip_x: bool,
    /// Under or over the text.
    pub z: FreeZ,
}

impl PartialEq for FreeSprite {
    #[inline]
    fn eq(&self, o: &Self) -> bool {
        (
            self.x, self.y, self.w, self.h, self.clip, self.tint, self.alpha,
        ) == (o.x, o.y, o.w, o.h, o.clip, o.tint, o.alpha)
            && self.flip_x == o.flip_x
            && self.z == o.z
            && self.art.id() == o.art.id()
    }
}

impl Eq for FreeSprite {}

impl FreeSprite {
    /// All of `art`, 1:1, at `(x, y)`: unclipped, untinted, opaque, over the
    /// text.
    #[inline]
    #[must_use]
    pub fn whole(x: i32, y: i32, art: Arc<FxArt>) -> Self {
        Self {
            x,
            y,
            w: art.w(),
            h: art.h(),
            art,
            clip: PxRect::EVERYWHERE,
            tint: 0x00FF_FFFF,
            alpha: u8::MAX,
            flip_x: false,
            z: FreeZ::OverText,
        }
    }

    /// The pixels it may draw: its box within its clip.
    #[inline]
    #[must_use]
    pub fn visible(&self) -> PxRect {
        PxRect::at(self.x, self.y, u32::from(self.w), u32::from(self.h)).intersect(self.clip)
    }

    /// Whether it draws nothing: a clear ink, an empty box or art, or a clip
    /// that hides the whole box.
    #[inline]
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.alpha == 0 || self.art.is_empty() || self.visible().is_empty()
    }

    /// The sprite a published layer of [`SpriteLayers`] draws, or `None` for
    /// any other layer.
    #[must_use]
    pub fn of_layer(layer: &FxLayer) -> Option<Self> {
        let z = match (layer.slot, layer.knock) {
            (FxSlot::Under, true) => FreeZ::UnderText,
            (FxSlot::Over, false) => FreeZ::OverText,
            _ => return None,
        };
        if layer.blend != FxBlend::Linear || layer.space != FxSpace::Grid {
            return None;
        }
        let [FxItem::Sprite(sp)] = layer.content.items() else {
            return None;
        };
        let ink = layer.content.palette().get(sp.ink as usize)?;
        Some(Self {
            x: layer.offset.0.saturating_add(sp.x),
            y: layer.offset.1.saturating_add(sp.y),
            w: sp.dw,
            h: sp.dh,
            art: Arc::clone(&sp.art),
            clip: layer.clip,
            tint: ink.color,
            alpha: ink.alpha,
            flip_x: sp.flip_x,
            z,
        })
    }
}

/// Whether `layer` is one [`SpriteLayers`] publishes (what
/// [`FreeSprite::of_layer`] reads back), without reading it back.
#[must_use]
pub fn is_sprite_layer(layer: &FxLayer) -> bool {
    matches!(
        (layer.slot, layer.knock),
        (FxSlot::Under, true) | (FxSlot::Over, false)
    ) && layer.blend == FxBlend::Linear
        && layer.space == FxSpace::Grid
        && matches!(layer.content.items(), [FxItem::Sprite(_)])
}

/// The sprites [`SpriteLayers`] published into `layers`, in draw order.
#[must_use]
pub fn published(layers: &[FxLayer]) -> Vec<FreeSprite> {
    layers.iter().filter_map(FreeSprite::of_layer).collect()
}

/// What a sprite's layer content depends on: everything but where it lands.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Look {
    art: u128,
    w: u16,
    h: u16,
    flip_x: bool,
    tint: u32,
    alpha: u8,
}

impl Look {
    #[inline]
    fn of(s: &FreeSprite) -> Self {
        Self {
            art: s.art.id(),
            w: s.w,
            h: s.h,
            flip_x: s.flip_x,
            tint: s.tint & 0x00FF_FFFF,
            alpha: s.alpha,
        }
    }

    /// The seed of the layer id a sprite of this look under `z` takes: every
    /// field folded through [`layer_id`], so it names the look and not the
    /// sprite's place in the emission order.
    #[inline]
    fn seed(self, z: FreeZ) -> u64 {
        let words = [
            self.art as u64,
            (self.art >> 64) as u64,
            u64::from(self.w)
                | u64::from(self.h) << 16
                | u64::from(self.alpha) << 32
                | u64::from(self.flip_x) << 40
                | u64::from(z == FreeZ::UnderText) << 41,
            u64::from(self.tint),
        ];
        words.iter().fold(0, |acc, &w| layer_id(acc ^ w))
    }
}

/// Publishes a frame's [`FreeSprite`]s as fx layers, one a sprite in emission
/// order: a Grid-space Linear layer whose content is the sprite at the origin,
/// placed by the layer's offset and cut by its clip. A sprite that keeps its
/// look keeps its content's `Arc`, so a sprite that did not change is a layer
/// that did not change (`aterm-render`'s identity damage repaints nothing for
/// it), and one that only moved repaints the bands it left and reached. A
/// layer's id is its sprite's look and z, never its index, so a sprite
/// appearing or leaving earlier in the order changes no other sprite's layer;
/// sprites of one look share the id and damage pairs them in order.
#[derive(Debug, Default)]
pub struct SpriteLayers {
    salt: u64,
    /// The ids this publisher put into the frame last.
    ids: Vec<u64>,
    /// The last publish's contents by look.
    held: Vec<(Look, Arc<FxContent>)>,
    next: Vec<(Look, Arc<FxContent>)>,
}

impl std::fmt::Debug for Look {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Look({:032x} {}x{})", self.art, self.w, self.h)
    }
}

impl SpriteLayers {
    /// The companions' declared CPU-GPU parity: the Linear law's bar. An
    /// opaque texel under an opaque white tint lands its bytes exactly on
    /// every arm (the integer nearest map leaves no sampling tie); a
    /// translucent one blends in linear light, within this.
    pub const PARITY: u8 = FxBlend::Linear.parity();

    /// A publisher whose layer ids are `salt`'s (no two publishers of one
    /// frame share a salt).
    #[inline]
    #[must_use]
    pub const fn new(salt: u64) -> Self {
        Self {
            salt,
            ids: Vec::new(),
            held: Vec::new(),
            next: Vec::new(),
        }
    }

    /// Replace this publisher's layers in `layers` with `sprites`' (appended,
    /// so they draw over every Linear layer already there).
    pub fn publish(&mut self, sprites: &[FreeSprite], layers: &mut Vec<FxLayer>) {
        withdraw(layers, &self.ids);
        self.ids.clear();
        self.next.clear();
        for s in sprites {
            if s.is_blank() || s.w == 0 || s.h == 0 {
                continue;
            }
            let look = Look::of(s);
            let content = match self.held.iter().chain(&self.next).find(|(l, _)| *l == look) {
                Some((_, c)) => Arc::clone(c),
                None => Arc::new(FxContent::new(
                    vec![FxItem::Sprite(FxSprite {
                        x: 0,
                        y: 0,
                        dw: s.w,
                        dh: s.h,
                        flip_x: s.flip_x,
                        art: Arc::clone(&s.art),
                        ink: 0,
                        clip: PxRect::EVERYWHERE,
                    })],
                    vec![FxInk::over(look.tint, s.alpha)],
                )),
            };
            if !self.next.iter().any(|(l, _)| *l == look) {
                self.next.push((look, Arc::clone(&content)));
            }
            let id = layer_id(self.salt ^ look.seed(s.z));
            let (slot, knock) = match s.z {
                FreeZ::UnderText => (FxSlot::Under, true),
                FreeZ::OverText => (FxSlot::Over, false),
            };
            let mut layer = FxLayer::new(id, content, slot, FxSpace::Grid, FxBlend::Linear);
            layer.knock = knock;
            layer.offset = (s.x, s.y);
            layer.clip = s.clip;
            self.ids.push(id);
            layers.push(layer);
        }
        std::mem::swap(&mut self.held, &mut self.next);
    }

    /// Take this publisher's layers out of `layers` and forget its contents.
    #[inline]
    pub fn withdraw(&mut self, layers: &mut Vec<FxLayer>) {
        withdraw(layers, &self.ids);
        self.ids.clear();
        self.held.clear();
    }

    /// The ids of the layers the last publish put into the frame.
    #[inline]
    #[must_use]
    pub fn ids(&self) -> &[u64] {
        &self.ids
    }
}

/// A layer id: `seed` through the splitmix64 finaliser, never `0`.
#[inline]
#[must_use]
pub const fn layer_id(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let r = z ^ (z >> 31);
    if r == 0 { 0xD1B5_4A32_D192_ED03 } else { r }
}

/// Drop every layer whose id is one of `ids` (a publisher taking its layers
/// back out of a frame).
#[inline]
pub fn withdraw(layers: &mut Vec<FxLayer>, ids: &[u64]) {
    if !ids.is_empty() {
        layers.retain(|l| !ids.contains(&l.id));
    }
}

#[cfg(test)]
mod tests {
    use aterm_core::render::FxTexels;

    use super::*;

    fn art(seed: u8) -> Arc<FxArt> {
        crate::fx::bitmap(FxTexels::Rgba8Straight, 3, 2, vec![seed; 3 * 2 * 4]).expect("art")
    }

    fn sprite(x: i32, seed: u8, z: FreeZ) -> FreeSprite {
        FreeSprite {
            z,
            tint: 0x0080_4020,
            alpha: 200,
            ..FreeSprite::whole(x, 5, art(seed))
        }
    }

    #[test]
    fn a_sprite_round_trips_through_its_layer() {
        let mut pubr = SpriteLayers::new(7);
        let mut layers = Vec::new();
        let mut s = sprite(4, 1, FreeZ::UnderText);
        s.clip = PxRect::new(0, 0, 6, 9);
        s.flip_x = true;
        let o = sprite(9, 2, FreeZ::OverText);
        pubr.publish(&[s.clone(), o.clone()], &mut layers);
        assert_eq!(layers.len(), 2);
        assert!(layers[0].knock && layers[0].slot == FxSlot::Under);
        assert!(!layers[1].knock && layers[1].slot == FxSlot::Over);
        assert!(layers.iter().all(is_sprite_layer));
        assert_eq!(published(&layers), vec![s, o]);
    }

    #[test]
    fn an_unchanged_sprite_is_an_unchanged_layer_and_a_move_keeps_the_content() {
        let mut pubr = SpriteLayers::new(7);
        let mut a = Vec::new();
        let s = sprite(4, 1, FreeZ::UnderText);
        pubr.publish(std::slice::from_ref(&s), &mut a);
        let mut b = a.clone();
        pubr.publish(std::slice::from_ref(&s), &mut b);
        assert_eq!(a, b, "identity equality: the same layer");
        let mut c = b.clone();
        pubr.publish(&[FreeSprite { x: 5, ..s.clone() }], &mut c);
        assert_ne!(b, c, "a move is a changed layer");
        assert!(
            Arc::ptr_eq(&b[0].content, &c[0].content),
            "…over the same content"
        );
        let mut d = c.clone();
        pubr.publish(&[FreeSprite { alpha: 9, ..s }], &mut d);
        assert!(
            !Arc::ptr_eq(&c[0].content, &d[0].content),
            "a new look is new content"
        );
    }

    #[test]
    fn content_is_shared_only_across_one_whole_look() {
        let mut pubr = SpriteLayers::new(7);
        let s = sprite(4, 1, FreeZ::OverText);
        for (what, changed) in [
            (
                "a mirror",
                FreeSprite {
                    flip_x: true,
                    ..s.clone()
                },
            ),
            (
                "a tint",
                FreeSprite {
                    tint: 0x0010_2030,
                    ..s.clone()
                },
            ),
            ("a width", FreeSprite { w: 6, ..s.clone() }),
            ("a height", FreeSprite { h: 4, ..s.clone() }),
        ] {
            let (mut a, mut b) = (Vec::new(), Vec::new());
            pubr.publish(std::slice::from_ref(&s), &mut a);
            pubr.publish(std::slice::from_ref(&changed), &mut b);
            assert!(
                !Arc::ptr_eq(&a[0].content, &b[0].content),
                "{what} of the same art is new content"
            );
            assert_eq!(published(&b), vec![changed], "{what} draws as itself");
        }
    }

    #[test]
    fn a_sprites_layer_id_is_its_look_not_its_place_in_the_order() {
        let mut pubr = SpriteLayers::new(7);
        let (a, b) = (
            sprite(0, 1, FreeZ::OverText),
            sprite(9, 2, FreeZ::UnderText),
        );
        let mut both = Vec::new();
        pubr.publish(&[a.clone(), b.clone()], &mut both);
        let mut alone = Vec::new();
        pubr.publish(std::slice::from_ref(&b), &mut alone);
        assert_eq!(
            alone[0], both[1],
            "b is the same layer with a gone from before it"
        );
        let mut moved = Vec::new();
        pubr.publish(&[FreeSprite { x: 3, ..b }], &mut moved);
        assert_eq!(moved[0].id, alone[0].id, "a move keeps the id");
        assert_ne!(both[0].id, both[1].id, "two looks, two ids");
    }

    #[test]
    fn a_publish_replaces_only_its_own_layers() {
        let mut words = SpriteLayers::new(1);
        let mut other = SpriteLayers::new(2);
        let mut layers = Vec::new();
        words.publish(&[sprite(0, 1, FreeZ::OverText)], &mut layers);
        other.publish(&[sprite(1, 2, FreeZ::OverText)], &mut layers);
        words.publish(&[], &mut layers);
        assert_eq!(published(&layers).len(), 1);
        assert_eq!(
            published(&layers)[0].x,
            1,
            "the other publisher's sprite stays"
        );
        let blank = FreeSprite {
            alpha: 0,
            ..sprite(0, 1, FreeZ::OverText)
        };
        let hidden = FreeSprite {
            clip: PxRect::new(50, 50, 60, 60),
            ..sprite(0, 1, FreeZ::OverText)
        };
        words.publish(&[blank, hidden], &mut layers);
        assert_eq!(
            published(&layers).len(),
            1,
            "a blank sprite publishes no layer"
        );
        other.withdraw(&mut layers);
        assert!(layers.is_empty());
    }
}
