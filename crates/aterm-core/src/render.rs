// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The engine-owned render snapshot: [`RenderInput`] is the plain-owned, `Terminal`-free value
//! a renderer reads to paint one frame. The engine produces it
//! ([`Terminal::cell_frame_into`](crate::terminal::Terminal::cell_frame_into)); hosting the
//! type here avoids a dependency cycle, and `aterm_render::render_api` re-exports it.

use crate::grid::LineSize;
use crate::selection::TextSelection;
use crate::terminal::{CursorStyle, RenderCell};
use aterm_hash::FxHashMap;

// The engine's sprite and bitmap primitive (design: docs/DESIGN-card-engine-2026-10-19.md §4).
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub use crate::render_fx::fx_counts_here;
pub use crate::render_fx::{
    FX_ART_MAX_SIDE, FX_FRAME_ART_TEXELS, FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer,
    FxRamps, FxRect, FxRowRuns, FxRun, FxSlot, FxSpace, FxSprite, FxTexels, PxRect,
    fx_layer_builds, fx_payload_copies,
};

/// One faded cell of the cursor motion trail: a cell the cursor recently swept
/// through, painted in the trail colour at `alpha` coverage over its own
/// background. The trail fades within a few hundred milliseconds, reading as a
/// comet behind the cursor.
///
/// Position is in the same viewport coordinates as
/// [`RenderInput::cursor_row`]/[`RenderInput::cursor_col`] (after the tab-strip
/// splice and any split-pane offset), so trail cells use the cursor's
/// `pad + col*cell_w` / `pad + row*cell_h` geometry. The host owns the clock
/// and resolves `alpha` per cell each frame, keeping the renderer
/// deterministic (CPU/GPU byte parity); an empty trail draws nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrailCell {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: usize,
    /// Viewport column (0-indexed).
    pub col: usize,
    /// Blend coverage `0..=255` of the trail colour over this cell's background, drawn opaque
    /// as the pre-blended result so the CPU fill and the GPU `REPLACE` quad agree.
    pub alpha: u8,
}

/// One light quad of an effect stream (aurora comet, bloom crown, landing ring, sparks, nova,
/// fire body). In additive mode the renderer saturating-adds the premultiplied colour:
/// order-independent and byte-exact between the GPU (`One`/`One` over the linear `Rgba8Unorm`
/// target) and the CPU (`add_sat`).
///
/// Host invariants: (1) coordinates are per stream: the cursor-effect streams
/// (`cursor_glow_add`, `glow_under`) are window-absolute pixels clamped to the effects box,
/// the grid-anchored `nova_add` grid-interior; (2) every quad lies within exactly one cell-row
/// band (a halo spilling into a neighbour row is a separate quad), so the row-scoped dirty gate
/// and GPU scissor cover it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlowQuad {
    /// The row band this quad lives in: a damage hint (above-grid quads tag row 0).
    pub row: u16,
    /// Pixel X in this stream's coordinate space (see the struct doc).
    pub x: u16,
    /// Pixel Y in this stream's coordinate space.
    pub y: u16,
    /// Quad width in pixels.
    pub w: u16,
    /// Quad height in pixels (kept within the single `row` cell band).
    pub h: u16,
    /// Premultiplied colour `0x00RRGGBB` (the colour times its coverage) in both modes.
    pub color: u32,
    /// The source-over opacity `0..=255`, which also selects the mode; both are
    /// `out = color + dst·(255 − alpha)/255`:
    ///
    /// * `alpha == 0`: premultiplied additive light, `out = dst + color` (saturating), the
    ///   default: it only brightens and is order-independent.
    /// * `alpha > 0`: premultiplied source-over paint: the destination is attenuated by
    ///   `1 − alpha/255` first.
    ///
    /// Additive light is invisible over white and never darkens; source-over reads on any
    /// ground but can darken one brighter than itself, so a stream that must never darken
    /// stays at `0`.
    pub alpha: u8,
    /// Premultiplied colour `0x00RRGGBB` at the right edge of a horizontal gradient across the
    /// quad's `w` columns ([`color`](GlowQuad::color) is the left); equal ends are flat.
    /// Column `i` carries, per channel and for the opacity alike,
    ///
    /// ```text
    /// c(i) = (c0·(2w − m) + c1·m + w) / (2w),   m = 2i + 1
    /// ```
    ///
    /// in unsigned integer arithmetic (round half up): the line from `c0` to `c1` sampled at
    /// the column centre (`w == 1` gives `(c0 + c1 + 1) / 2`). The CPU rasterizer and both GPU
    /// shaders compute this same integer formula, never a float interpolant.
    pub color2: u32,
    /// The source-over opacity at the right edge ([`alpha`](GlowQuad::alpha) is the left). The
    /// mode is per quad: `alpha == 0 ⇔ alpha2 == 0`. The colour is premultiplied by the same
    /// coverage, so the composite is continuous where a ramp reaches `0`.
    pub alpha2: u8,
}

impl GlowQuad {
    /// A flat quad: one colour and one opacity across its width
    /// (`color2 == color`, `alpha2 == alpha`).
    #[must_use]
    #[inline]
    pub const fn flat(row: u16, x: u16, y: u16, w: u16, h: u16, color: u32, alpha: u8) -> Self {
        Self {
            row,
            x,
            y,
            w,
            h,
            color,
            alpha,
            color2: color,
            alpha2: alpha,
        }
    }

    /// A gradient quad: `(color, alpha)` at the left edge ramping to `(color2, alpha2)` at
    /// the right (see [`GlowQuad::color2`]).
    ///
    /// # Panics (debug)
    /// When exactly one of `alpha`/`alpha2` is `0`: a quad is additive or source-over at both
    /// ends.
    #[must_use]
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub const fn gradient(
        row: u16,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        color: u32,
        alpha: u8,
        color2: u32,
        alpha2: u8,
    ) -> Self {
        debug_assert!(
            (alpha == 0) == (alpha2 == 0),
            "a GlowQuad is additive at both ends or source-over at both ends"
        );
        Self {
            row,
            x,
            y,
            w,
            h,
            color,
            alpha,
            color2,
            alpha2,
        }
    }

    /// Whether the quad is flat (`color2 == color && alpha2 == alpha`).
    #[must_use]
    #[inline]
    pub const fn is_flat(self) -> bool {
        self.color2 == self.color && self.alpha2 == self.alpha
    }

    /// Canonical lossless words for cursor-effect fingerprints: two for a flat quad, plus a
    /// tagged third word with the right edge for a gradient, so a gradient never folds as its
    /// left-flat twin.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn damage_words(self) -> GlowQuadWords {
        let first = pack_u16x4(self.row, self.x, self.y, self.w);
        let second = self.h as u64 | (self.color as u64) << 16 | (self.alpha as u64) << 48;
        if self.is_flat() {
            GlowQuadWords {
                words: [first, second, 0],
                len: 2,
            }
        } else {
            GlowQuadWords {
                words: [
                    first,
                    second,
                    self.color2 as u64 | (self.alpha2 as u64) << 32 | GlowQuadWords::GRADIENT_TAG,
                ],
                len: 3,
            }
        }
    }
}

/// The fingerprint words of one [`GlowQuad`] (two flat, three gradient); iterates the live
/// words only.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlowQuadWords {
    words: [u64; 3],
    len: u8,
}

impl GlowQuadWords {
    /// Bits 48..64 of a gradient quad's third word (`"GR"`), disjoint from `color2` and
    /// `alpha2`, so the third word never resembles a following flat quad's `(row, x, y, w)`.
    const GRADIENT_TAG: u64 = 0x4752_0000_0000_0000;

    /// The live words as a slice.
    #[must_use]
    #[inline]
    pub fn as_slice(&self) -> &[u64] {
        &self.words[..self.len as usize]
    }
}

impl IntoIterator for GlowQuadWords {
    type Item = u64;
    type IntoIter = std::iter::Take<std::array::IntoIter<u64, 3>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.words.into_iter().take(self.len as usize)
    }
}

/// Which [`GlowQuad::alpha`] convention an emitter stamps, so a rasterizer shared by additive
/// and source-over streams knows which it produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlowBlend {
    /// Premultiplied additive light: every quad has `alpha == 0`. The default.
    #[default]
    Add,
    /// Premultiplied source-over paint: each quad's `alpha` is the coverage that premultiplied
    /// its colour.
    Over,
}

/// How a [`RainHalo`]'s radially-weighted colour composites. `Add` is premultiplied light,
/// invisible over white; `Over` is a source-over veil (smoke, steam) that reads on any
/// background. Within one stream every `Add` quad composites before every `Over` quad (the
/// GPU's per-mode draw order, mirrored by the CPU), keeping overlaps parity-exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HaloMode {
    /// Premultiplied saturating-add light (CPU `add_sat` == GPU One/One on the Unorm view).
    #[default]
    Add,
    /// Source-over veil: straight `color`, the falloff weight as per-pixel alpha.
    Over,
}

/// A radial halo quad: like [`GlowQuad`] (one row band) but its light falls off radially from
/// `(cx, cy)`. Each covered pixel at offset `(dx, dy)` scales `color` by
/// `weight = ((256 − nsq)² >> 8)` (clamped), `nsq = (dx²·256)/rx² + (dy²·256)/ry²`, then
/// `add_sat`s it (under [`HaloMode::Over`] the weight is the source-over alpha). Pure integer
/// math, so the CPU and the GPU `fs_rain_glow`/`fs_rain_glow_over` shaders agree byte for
/// byte. Coordinates follow [`GlowQuad`]'s per-stream rule: `glow_halo` is window-absolute,
/// `rain_add` grid-interior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RainHalo {
    /// The row band this quad lives in: a damage hint (above-grid quads tag row 0).
    pub row: u16,
    /// Pixel X of the quad, in its stream's coordinate space (see the struct doc).
    pub x: u16,
    /// Pixel Y of the quad (same convention as `x`).
    pub y: u16,
    /// Quad width in pixels.
    pub w: u16,
    /// Quad height in pixels (kept within the single `row` cell band).
    pub h: u16,
    /// The halo-centre colour `0x00RRGGBB`: premultiplied peak light ([`HaloMode::Add`]) or
    /// a straight veil colour ([`HaloMode::Over`]), scaled per pixel by the falloff.
    pub color: u32,
    /// Pixel X of the halo centre, the falloff origin; may lie outside this quad's row band.
    pub cx: u16,
    /// Pixel Y of the halo centre (same convention as `y`).
    pub cy: u16,
    /// Horizontal falloff half-extent in pixels (light reaches 0 there); `>= 1`, guaranteed
    /// by the emitter (0 would divide by zero).
    pub rx: u16,
    /// Vertical falloff half-extent in pixels; `>= 1`.
    pub ry: u16,
    /// How the radially-weighted colour composites.
    pub mode: HaloMode,
}

impl RainHalo {
    /// Canonical lossless words used by cursor-effect fingerprints.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn damage_words(self) -> [u64; 3] {
        let mode = match self.mode {
            HaloMode::Add => 0,
            HaloMode::Over => 1,
        };
        [
            pack_u16x4(self.row, self.x, self.y, self.w),
            self.h as u64 | (self.color as u64) << 16 | (self.cx as u64) << 48,
            pack_u16x4(self.cy, self.rx, self.ry, mode),
        ]
    }
}

/// How a [`FirePatch`]'s field output composites. [`Add`](FireMode::Add) (dark theme)
/// saturating-adds premultiplied light; [`Over`](FireMode::Over) (light theme) composites a
/// straight ink palette source-over, the [`HaloMode::Over`] contract. Within one stream every
/// `Add` patch composites before every `Over` patch, keeping overlaps parity-exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FireMode {
    /// Premultiplied saturating-add flame light: the dark-theme mode.
    #[default]
    Add,
    /// Source-over ink-fire (straight RGB, field-shaped alpha), readable on white.
    Over,
}

/// One fire patch: a stateless, float-free procedural fire field evaluated at every device
/// pixel of the quad by both backends (`aterm_render::fire_field`, mirrored by the GPU's
/// `fs_fire_add`/`fs_fire_over`), so they land the same byte. Window-absolute pixels, one
/// cell-row band per patch. The field depends only on absolute pixel coordinates and the
/// parameters, so patches of one burn are seamless and splitting a patch changes no byte;
/// `base_y` may lie outside the patch's own band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FirePatch {
    /// The row band this patch lives in: a damage hint (above-grid patches tag row 0).
    pub row: u16,
    /// Window-absolute pixel X of the quad.
    pub x: u16,
    /// Window-absolute pixel Y of the quad.
    pub y: u16,
    /// Quad width in pixels.
    pub w: u16,
    /// Quad height in pixels (kept within the single `row` cell band).
    pub h: u16,
    /// Window-absolute pixel Y of the flame root (field `v = 0`; flames rise, so live pixels
    /// have `y < base_y`), shared by a burn's patches.
    pub base_y: u16,
    /// Maximum flame height in pixels; `0` is treated as `1`.
    pub peak_h: u16,
    /// Churn phase in 1/1024 s, quantized by the producer; shared by a burn's patches.
    pub phase: u32,
    /// Temperature `0..=255`: palette reach, coverage density, churn and rise speed.
    pub temp: u8,
    /// Per-cell envelope `0..=255` (head→tail falloff), scaling the tongue height.
    pub strength: u8,
    /// Horizontal shear: the sample column drifts `lean/4` px per full rise.
    pub lean: i8,
    /// Ceiling on emitted coverage/alpha `0..=255`, so text under the fire stays legible.
    pub cov_cap: u8,
    /// The cell height in pixels, the field's spatial unit; `< 2` is clamped to 2.
    pub cell_h: u16,
    /// How the field output composites.
    pub mode: FireMode,
}

impl FirePatch {
    /// Canonical lossless words used by cursor-effect fingerprints.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn damage_words(self) -> [u64; 4] {
        let mode = match self.mode {
            FireMode::Add => 0,
            FireMode::Over => 1,
        };
        [
            pack_u16x4(self.row, self.x, self.y, self.w),
            self.h as u64
                | (self.base_y as u64) << 16
                | (self.peak_h as u64) << 32
                | (self.temp as u64) << 48
                | (self.strength as u64) << 56,
            self.phase as u64
                | (self.lean.cast_unsigned() as u64) << 32
                | (self.cov_cap as u64) << 40
                | (self.cell_h as u64) << 48,
            0x6669_7265_0000_0000 | mode,
        ]
    }
}

/// Compact damage identity for a window-absolute cursor-effect stream: whether it changed and
/// which rows it occupied, so a prior-frame cache need not clone thousands of quads. Opt-in:
/// left at [`Default`], consumers compare and scan the vector. The GUI calls
/// [`RenderInput::refresh_cursor_effect_damage`] after its final composition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectStreamDamage {
    /// Two-lane content fingerprint; the explicit length prevents an empty/non-empty alias.
    fingerprint: u128,
    len: usize,
    /// One bit per grid row touched: compact even for an 8k-quad ribbon, and it keeps the
    /// vacated rows after the cache drops the payload.
    rows: Vec<u64>,
    valid: bool,
}

impl EffectStreamDamage {
    /// `clone_from` that keeps `rows`' capacity.
    #[inline]
    fn refill_from(&mut self, source: &Self) {
        let Self {
            fingerprint,
            len,
            rows,
            valid,
        } = self;
        *fingerprint = source.fingerprint;
        *len = source.len;
        rows.clone_from(&source.rows);
        *valid = source.valid;
    }

    /// Whether this metadata was built from its stream's final render payload.
    #[must_use]
    #[inline]
    pub const fn is_valid(&self) -> bool {
        self.valid
    }

    /// Whether the described stream was empty: the only reliable answer for a cached frame,
    /// whose payload [`RenderInput::clone_damage_cache_from`] dropped. `None` without valid
    /// metadata (the payload was cloned and answers directly).
    #[must_use]
    #[inline]
    pub const fn was_empty(&self) -> Option<bool> {
        if self.valid {
            Some(self.len == 0)
        } else {
            None
        }
    }

    /// A content verdict, only when both sides carry trusted metadata.
    #[must_use]
    #[inline]
    pub const fn same_content(&self, other: &Self) -> Option<bool> {
        if self.valid && other.valid {
            Some(self.len == other.len && self.fingerprint == other.fingerprint)
        } else {
            None
        }
    }

    /// Mark the stream's rows into a renderer's dirty scratch; out-of-range bits are ignored.
    #[inline]
    pub fn mark_rows(&self, dirty: &mut [bool]) {
        if !self.valid {
            return;
        }
        for (word_index, &word) in self.rows.iter().enumerate() {
            let mut bits = word;
            while bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                if let Some(mark) = dirty.get_mut(word_index * u64::BITS as usize + bit) {
                    *mark = true;
                }
                bits &= bits - 1;
            }
        }
    }

    #[inline]
    fn refresh<T>(
        &mut self,
        values: &[T],
        grid_rows: usize,
        row_of: impl Fn(&T) -> u16,
        hash_one: impl Fn(&T, &mut EffectStreamFingerprint),
    ) {
        let mut fingerprint = EffectStreamFingerprint::new(values.len());
        self.rows.clear();
        self.rows.resize(grid_rows.div_ceil(u64::BITS as usize), 0);
        for value in values {
            hash_one(value, &mut fingerprint);
            let row = usize::from(row_of(value));
            if let Some(word) = self.rows.get_mut(row / u64::BITS as usize) {
                *word |= 1_u64 << (row % u64::BITS as usize);
            }
        }
        self.fingerprint = fingerprint.finish();
        self.len = values.len();
        self.valid = true;
    }
}

/// A fixed-shape two-lane fingerprint for effect records, not a general hasher. The lanes
/// differ in combination and constants, avoiding the affine relation of `FxHasher::with_seed`
/// used twice.
struct EffectStreamFingerprint {
    first: u64,
    second: u64,
}

impl EffectStreamFingerprint {
    const FIRST_SEED: u64 = 0x243f_6a88_85a3_08d3;
    const SECOND_SEED: u64 = 0x1319_8a2e_0370_7344;
    const FIRST_MUL: u64 = 0x9e37_79b1_85eb_ca87;
    const SECOND_MUL: u64 = 0xc2b2_ae3d_27d4_eb4f;

    #[inline]
    const fn new(len: usize) -> Self {
        let mut fingerprint = Self {
            first: Self::FIRST_SEED,
            second: Self::SECOND_SEED,
        };
        fingerprint.write_pair(len as u64, 0x6c65_6e67_7468_0001);
        fingerprint
    }

    #[inline]
    const fn write_pair(&mut self, first_word: u64, second_word: u64) {
        // Both lanes consume both words; independent lanes let the multiplies issue together.
        self.first =
            (self.first ^ first_word ^ second_word.rotate_left(23)).wrapping_mul(Self::FIRST_MUL);
        self.second = (self.second ^ second_word ^ first_word.rotate_right(17))
            .wrapping_mul(Self::SECOND_MUL);
    }

    #[inline]
    const fn finish(self) -> u128 {
        ((self.first as u128) << 64) | (self.second as u128)
    }
}

#[inline]
const fn pack_u16x4(a: u16, b: u16, c: u16, d: u16) -> u64 {
    a as u64 | (b as u64) << 16 | (c as u64) << 32 | (d as u64) << 48
}

#[inline]
const fn hash_glow_quad(quad: &GlowQuad, hasher: &mut EffectStreamFingerprint) {
    let GlowQuadWords {
        words: [first, second, third],
        len,
    } = quad.damage_words();
    hasher.write_pair(first, second);
    // A gradient's right edge rides a third word; a flat quad writes only the pair.
    if len == 3 {
        hasher.write_pair(third, 0);
    }
}

#[inline]
const fn hash_rain_halo(halo: &RainHalo, hasher: &mut EffectStreamFingerprint) {
    let [first, second, third] = halo.damage_words();
    hasher.write_pair(first, second);
    hasher.write_pair(third, 0);
}

#[inline]
const fn hash_fire_patch(patch: &FirePatch, hasher: &mut EffectStreamFingerprint) {
    let [first, second, third, fourth] = patch.damage_words();
    hasher.write_pair(first, second);
    hasher.write_pair(third, fourth);
}

/// One textured sprite quad for an animated overlay: an atlas rectangle composited source-over
/// (straight alpha), the blend the inline-image and colour-emoji paths use. The dest rect is
/// grid-interior (the renderer offsets by `pad`), clamped to the grid, and lies within one
/// cell-row band (`row`): a sprite spanning rows is emitted as per-row slices. Atlas
/// coordinates are integer texels, so the type stays `Copy + Eq` for byte-exact damage compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpriteQuad {
    /// The viewport row band this quad lives in (its sole row, for the dirty gate).
    pub row: u16,
    /// Grid-interior pixel X of the dest rect (pad-relative).
    pub x: u16,
    /// Grid-interior pixel Y of the dest rect (pad-relative; within the `row` band).
    pub y: u16,
    /// Dest width in pixels.
    pub w: u16,
    /// Dest height in pixels (kept within the single `row` cell band).
    pub h: u16,
    /// Source rect X in the sprite atlas, in texels.
    pub ax: u16,
    /// Source rect Y in the sprite atlas, in texels.
    pub ay: u16,
    /// Source rect width in the sprite atlas, in texels.
    pub aw: u16,
    /// Source rect height in the sprite atlas, in texels.
    pub ah: u16,
    /// Multiply tint `0x00RRGGBB` applied to the sampled texel (`0x00FF_FFFF` = none).
    pub tint: u32,
    /// Extra opacity `0..=255` multiplied onto the sampled alpha (`255` = as-authored).
    pub alpha: u8,
    /// Mirror the sampled sprite horizontally (one baked pose facing either way).
    pub flip_x: bool,
}

/// One free-floating decorative sprite: a pixel rectangle anywhere, even off-grid, with no row
/// tag; its full `[y, y+h)` extent drives dirty tracking, so it may span rows. The origin is
/// grid-interior and signed (a cat peeking in from outside the grid).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FreeSprite {
    /// Grid-interior pixel X of the dest rect (pad-relative; may be negative
    /// for an off-grid peek).
    pub x: i32,
    /// Grid-interior pixel Y of the dest rect (pad-relative; may be negative or past the
    /// bottom edge).
    pub y: i32,
    /// Dest width in pixels (zero is a no-op, rejected in the stamp).
    pub w: u16,
    /// Dest height in pixels (zero is a no-op, rejected in the stamp).
    pub h: u16,
    /// Source rect X in the free atlas, in texels.
    pub ax: u16,
    /// Source rect Y in the free atlas, in texels.
    pub ay: u16,
    /// Source rect width in the free atlas, in texels.
    pub aw: u16,
    /// Source rect height in the free atlas, in texels.
    pub ah: u16,
    /// Multiply tint `0x00RRGGBB` applied to the sampled texel (`0x00FF_FFFF` = none).
    pub tint: u32,
    /// Extra opacity `0..=255` multiplied onto the sampled alpha (`255` = as-authored).
    pub alpha: u8,
    /// Mirror the sampled sprite horizontally, exactly like [`SpriteQuad::flip_x`].
    pub flip_x: bool,
    /// Painter's-order slot: under or over the terminal text.
    pub z: FreeZ,
    /// Sampling regime.
    pub sampler: FreeSampler,
}

/// Where a [`FreeSprite`] sits relative to the terminal text (painter's order, not a depth
/// test); part of the damage compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FreeZ {
    /// Drawn after the cell backgrounds / under-text sprites, before glyphs.
    #[default]
    UnderText,
    /// Drawn after glyphs and word decorations, before the cursor.
    OverText,
}

/// How a [`FreeSprite`] samples its atlas. Renderers implement `Nearest` only (the host bakes
/// at 1:1); `Linear` is carried in the type and the damage compare but not rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FreeSampler {
    /// Nearest 1:1 (cats).
    #[default]
    Nearest,
    /// Bilinear (deferred — scaled sprite art).
    Linear,
}

/// An RGBA8 sprite atlas (straight alpha, row-major, `width*height*4`), shared by `Arc`; the
/// GPU re-uploads only when `version` changes.
#[derive(Debug)]
pub struct SceneAtlas {
    /// Atlas width in texels.
    pub width: u32,
    /// Atlas height in texels.
    pub height: u32,
    /// Straight-alpha RGBA8 pixels.
    pub rgba: Vec<u8>,
    /// Monotonic version; the GPU caches its uploaded texture against this.
    pub version: u64,
}

/// Which small procedural sprite a [`WordDecoration`] paints, rasterized by the renderer
/// (`deco_coverage`) into a coverage mask so CPU and GPU land the same pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecoGlyph {
    /// A 4-pointed sparkle star (the default profanity spark).
    #[default]
    Star4,
    /// A 5-pointed star variant.
    Star5,
    /// A small round twinkle dot.
    Dot,
    /// A plus / cross-shaped sparkle.
    Plus,
    /// A cat's paw print (the feline mark).
    Paw,
    /// A soft annulus-arc ring segment filling the cell: the singularity nova's darkening ring
    /// (additive light cannot darken), one `DecoBlend::Over` stamp per covered cell.
    RingArc,
    /// A full-cell soft-edged square (~1 px ramp): the super nova's dark veil on light
    /// backgrounds, one `DecoBlend::Over` stamp per covered cell.
    Shade,
}

/// How a [`WordDecoration`] composites onto the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecoBlend {
    /// Premultiplied saturating add (`dst + src`) — the profanity sparkle, which
    /// only ever brightens, exactly like [`GlowQuad`].
    #[default]
    Add,
    /// Source-over alpha blend (`dst*(1-a) + src*a`): the feline cat-paw, a
    /// solid stamp tinted by `color` at `alpha`.
    Over,
}

/// One sparkle-word decoration: a small sprite stamped over or near a cell of a matched word,
/// at the cursor's viewport coordinates plus the sub-cell `(dx, dy)`. Render-only: never
/// touches grid cells, copied text or recordings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordDecoration {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: u16,
    /// Viewport column (0-indexed) of the cell this sprite sits on.
    pub col: u16,
    /// Sub-cell horizontal jitter in pixels (sparkle liveliness; `0` for paw).
    pub dx: i8,
    /// Sub-cell vertical jitter in pixels (`0` for paw).
    pub dy: i8,
    /// Which sprite to stamp.
    pub glyph: DecoGlyph,
    /// How to composite it.
    pub blend: DecoBlend,
    /// Tint `0x00RRGGBB`: the light to add (`Add`) or the stamp colour (`Over`).
    pub color: u32,
    /// Coverage `0..=255` (the animation envelope folded in), multiplied onto the sprite mask.
    pub alpha: u8,
}

/// One animated-ink foreground override: `color` is the host-resolved final colour,
/// substituted wherever [`RenderCell::fg`] would tint ink (glyph, combining marks, line
/// decorations); SGR 58 still wins and colour emoji ignore it. [`RenderInput::ink`] is sorted
/// by `(row, col)` with unique cells (renderers walk it in lockstep); a wide glyph is governed
/// by its lead cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InkCell {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: u16,
    /// Viewport column (0-indexed); the lead cell of a wide glyph.
    pub col: u16,
    /// Final `[r, g, b]` to substitute for the cell's resolved `fg`.
    pub color: [u8; 3],
}

/// One per-cell glyph foreground override (fire's dark glyph cores: engulfed cells recoloured
/// toward ember-black). Substituted exactly where an [`InkCell`] would be, before the
/// contrast and selection floors; ink wins on a cell with both. [`RenderInput::char_fg`]
/// keeps [`InkCell`]'s invariants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharFg {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: u16,
    /// Viewport column (0-indexed); the lead cell of a wide glyph.
    pub col: u16,
    /// Final `0x00RRGGBB` to substitute for the cell's resolved `fg`.
    pub fg: u32,
}

/// One contrast-halo cell (fire text legibility): the engulfment weight driving the alpha of
/// the dark ring stamped under the glyph's ink so the letterform separates from the fire. The
/// ink is never recoloured. [`RenderInput::fire_halo`] keeps [`InkCell`]'s invariants;
/// colour-emoji cells are never haloed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FireHaloCell {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: u16,
    /// Viewport column (0-indexed); the lead cell of a wide glyph.
    pub col: u16,
    /// Fire engulfment weight (`0..=255`), scaling the halo's stamp alpha.
    pub strength: u8,
}

/// Sentinel for an unresolved live frame color: the renderer uses its theme. Real colors are
/// `0x00RRGGBB`, so a set high byte is unambiguous.
pub const COLOR_UNSET: u32 = 0xFF00_0000;

/// Sentinel for an explicitly dynamic selected-text foreground (`OSC 21;selection_foreground=`):
/// automatic contrast, so a renderer must not reintroduce its static configured foreground.
pub const COLOR_DYNAMIC: u32 = 0xFE00_0000;

/// One run of columns in a composed split row with its own DEC line size (side-by-side panes
/// may differ in DECDWL/DECDHL). Columns are composite columns: a glyph at column `c` draws at
/// `start_col * cell_w + (c - start_col) * run_cell_w`, clipped to the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSizeSpan {
    /// First composite column of the run (inclusive).
    pub start_col: usize,
    /// One past the last composite column of the run (exclusive).
    pub end_col: usize,
    /// The DEC line size every column in the run renders at.
    pub line_size: LineSize,
}

impl LineSizeSpan {
    /// A run covering `start_col..end_col` (half-open) at `line_size`.
    #[must_use]
    #[inline]
    pub const fn new(start_col: usize, end_col: usize, line_size: LineSize) -> Self {
        Self {
            start_col,
            end_col,
            line_size,
        }
    }
}

/// A host chrome row drawn at pixel resolution: the message band's meter
/// (`docs/DESIGN-unified-messages-2026-09-21.md`), for fills that end inside a cell, gradients
/// and a rail in the row's lowest pixels. Both renderers paint it after the row's cell
/// backgrounds and before its glyphs: [`Self::ground`] over the full band height except
/// [`Self::own`] columns, then [`Self::rail`]. The one cell [`Self::split`] names is drawn
/// twice under complementary clips.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChromeRaster {
    /// The frame row (counted from the top of the composed input, like
    /// [`RenderInput::cells`]).
    pub row: u16,
    /// One `0x00RRGGBB` per frame pixel column, `[0, frame_w)`; empty for no
    /// ground (the cells' own backgrounds stand).
    pub ground: std::sync::Arc<[u32]>,
    /// One `0x00RRGGBB` per frame pixel column for the rail band ([`ChromeRaster::KEEP`]
    /// keeps the ground); empty for no rail.
    pub rail: std::sync::Arc<[u32]>,
    /// The rail band's height in device pixels, at the bottom of the row.
    pub rail_h: u16,
    /// This row's glyphs keep clear of the rail: the renderer lifts them (never past the room
    /// above the tallest letter) and narrows the rail to what remains
    /// (`aterm_render::chrome_fit`); [`Self::rail_h`] is then the maximum.
    pub clear_rail: bool,
    /// Cell columns `[start, end)` that keep their own cell background: the
    /// ground is not painted there.
    pub own: Vec<(u16, u16)>,
    /// The ink split, when the row's fill ends inside a cell.
    pub split: Option<InkSplit>,
    /// Outlined capsules, drawn after the ground and rail and before the glyphs.
    pub rings: Vec<ChromeRing>,
    /// Cells whose glyph is drawn as a band icon instead of from a font; the cell keeps its
    /// character (text grid, screen reader, copy read it), ink and ground. Between two blank
    /// cells the icon draws at capital height and spills into them.
    pub icons: Vec<ChromeIcon>,
}

impl ChromeRaster {
    /// The rail value that keeps the ground under it.
    pub const KEEP: u32 = u32::MAX;

    /// Whether cell column `col` keeps its own background.
    #[must_use]
    #[inline]
    pub fn owns(&self, col: usize) -> bool {
        self.own
            .iter()
            .any(|&(a, b)| (usize::from(a)..usize::from(b)).contains(&col))
    }

    /// The icon cell column `col` draws in place of its glyph, if any.
    #[must_use]
    #[inline]
    pub fn icon_at(&self, col: usize) -> Option<BandIcon> {
        self.icons
            .iter()
            .find(|i| usize::from(i.col) == col)
            .map(|i| i.icon)
    }
}

/// One outlined capsule on a [`ChromeRaster`] row over cells `[start, end)`. The renderer
/// sizes the ring from the cell and the face (`aterm_render::chrome_ring_runs`); its straight
/// floor is the end cells' single underline in `ring`, which the host sets
/// (`aterm_render::chrome_ring_floor_cols`), so descender ink-skip carves it like any rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromeRing {
    /// First cell column of the capsule (inclusive).
    pub start: u16,
    /// One past its last cell column.
    pub end: u16,
    /// The ring's colour, `0x00RRGGBB`.
    pub ring: u32,
    /// The ground inside the ring (`0x00RRGGBB`), the capsule cells' background.
    pub inner: u32,
    /// The band's closing seam (`0x00RRGGBB`) on the stack's last row: the ring draws it
    /// across its two end cells, which carry no underline of their own. `None` elsewhere.
    pub seam: Option<u32>,
}

/// One cell of a [`ChromeRaster`] row whose glyph is a drawn [`BandIcon`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromeIcon {
    /// The cell column.
    pub col: u16,
    /// The icon drawn there.
    pub icon: BandIcon,
}

/// The band's icon set: one drawn icon for every status glyph a message row can carry, so
/// the glyph cell reads as one family whatever font holds `⚠` or `ℹ`. Each stands in for one
/// character ([`Self::for_char`]), drawn from the cell's geometry and the face's stroke weight
/// in the cell's ink.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BandIcon {
    /// `ℹ` — an `i` cut out of a disc.
    Info,
    /// `✓` — a check.
    Success,
    /// `⚠` — `!` cut out of a triangle.
    Warn,
    /// `✕` — a cross.
    Error,
    /// `⇣` — an arrow down onto a tray.
    Download,
    /// `↻` — a turning arrow.
    Update,
    /// `↑` — an arrow up.
    Upload,
    /// `⏸` — two bars.
    Pause,
    /// `✦` — a four-point star.
    Sparkle,
    /// `!` — the fallback mark.
    Alert,
    /// `·` — a dot.
    Dot,
    /// `…` — three dots.
    More,
    /// `⊖`: a bar cut out of a disc, a removal in flight.
    Remove,
}

impl BandIcon {
    /// Every icon, in its index order ([`Self::index`]).
    pub const ALL: [BandIcon; 13] = [
        Self::Info,
        Self::Success,
        Self::Warn,
        Self::Error,
        Self::Download,
        Self::Update,
        Self::Upload,
        Self::Pause,
        Self::Sparkle,
        Self::Alert,
        Self::Dot,
        Self::More,
        Self::Remove,
    ];

    /// The icon that stands in for `ch`, the band's glyph; `None` for any
    /// character outside the set (it keeps its font glyph).
    #[must_use]
    #[inline]
    pub const fn for_char(ch: char) -> Option<Self> {
        Some(match ch {
            '\u{2139}' => Self::Info,
            '\u{2713}' | '\u{2714}' => Self::Success,
            '\u{26a0}' => Self::Warn,
            '\u{2715}' | '\u{2716}' | '\u{2717}' => Self::Error,
            '\u{21e3}' | '\u{2193}' => Self::Download,
            '\u{21bb}' => Self::Update,
            '\u{2191}' => Self::Upload,
            '\u{23f8}' => Self::Pause,
            '\u{2726}' => Self::Sparkle,
            '!' => Self::Alert,
            '\u{00b7}' => Self::Dot,
            '\u{2026}' => Self::More,
            '\u{2296}' => Self::Remove,
            _ => return None,
        })
    }

    /// The character this icon is drawn for (the one a text grid keeps).
    #[must_use]
    #[inline]
    pub const fn ch(self) -> char {
        match self {
            Self::Info => '\u{2139}',
            Self::Success => '\u{2713}',
            Self::Warn => '\u{26a0}',
            Self::Error => '\u{2715}',
            Self::Download => '\u{21e3}',
            Self::Update => '\u{21bb}',
            Self::Upload => '\u{2191}',
            Self::Pause => '\u{23f8}',
            Self::Sparkle => '\u{2726}',
            Self::Alert => '!',
            Self::Dot => '\u{00b7}',
            Self::More => '\u{2026}',
            Self::Remove => '\u{2296}',
        }
    }

    /// A small stable index (a glyph cache key carries it).
    #[must_use]
    #[inline]
    pub const fn index(self) -> u32 {
        self as u32
    }

    /// The icon at `index` ([`Self::index`]).
    #[must_use]
    #[inline]
    pub const fn from_index(index: u32) -> Option<Self> {
        let idx = index as usize;
        if idx < Self::ALL.len() {
            Some(Self::ALL[idx])
        } else {
            None
        }
    }
}

/// Where a chrome row's fill ends inside a cell ([`ChromeRaster::split`]): that cell's glyph
/// pixels left of frame pixel `x` wear `ink` (over `bg`), the rest the cell's own colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InkSplit {
    /// The cell column the split falls in.
    pub col: u16,
    /// The frame pixel the split is at: `[.., x)` is the fill side.
    pub x: u32,
    /// The fill side's ink, `0x00RRGGBB`.
    pub ink: u32,
    /// The fill side's ground under the glyph, `0x00RRGGBB`.
    pub bg: u32,
}

/// One pane-local live default-background interval inside a composed [`RenderInput`] row:
/// panes of a split can resolve different defaults (OSC 11/111, DECSCNM). Producers keep a
/// row's spans sorted by `start_col`, non-overlapping and clipped to [`RenderInput::cols`];
/// [`RenderInput::default_bg_at`] still validates containment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultBgSpan {
    /// First physical composite column owned by this pane.
    pub start_col: usize,
    /// One past the pane's last physical composite column.
    pub end_col: usize,
    /// Pane-resolved live default background (`0x00RRGGBB` or `COLOR_UNSET`).
    pub default_bg: u32,
}

impl DefaultBgSpan {
    /// Construct a pane-local live default-background interval.
    #[must_use]
    #[inline]
    pub const fn new(start_col: usize, end_col: usize, default_bg: u32) -> Self {
        Self {
            start_col,
            end_col,
            default_bg,
        }
    }
}

/// Half-open frame-space bounds for a composed text selection, so a projected pane's
/// multi-row selection cannot span dividers and sibling panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SelectionClip {
    /// First included frame row.
    pub row_start: usize,
    /// One past the last included frame row.
    pub row_end: usize,
    /// First included frame column.
    pub col_start: usize,
    /// One past the last included frame column.
    pub col_end: usize,
}

impl SelectionClip {
    /// Construct a half-open frame-space selection rectangle.
    #[must_use]
    #[inline]
    pub const fn new(row_start: usize, row_end: usize, col_start: usize, col_end: usize) -> Self {
        Self {
            row_start,
            row_end,
            col_start,
            col_end,
        }
    }

    /// Whether one frame-space cell lies inside this rectangle.
    #[must_use]
    #[inline]
    pub const fn contains(self, row: usize, col: usize) -> bool {
        row >= self.row_start && row < self.row_end && col >= self.col_start && col < self.col_end
    }

    /// Shift the rectangle down when host chrome is prepended to a frame.
    #[inline]
    pub const fn translate_rows_down(&mut self, rows: usize) {
        self.row_start = self.row_start.saturating_add(rows);
        self.row_end = self.row_end.saturating_add(rows);
    }
}

/// One pane's selection, relocated into frame coordinates, with its own box and live
/// colours. [`RenderInput::selections`] carries one per pane of a composed split frame; an
/// empty list means the scalar path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneSelection {
    /// The pane's selection with its anchors already moved into frame rows and
    /// columns (pane origin + that pane's `display_offset`).
    pub selection: TextSelection,
    /// The pane's half-open frame-space box; always present, since an unbounded pane
    /// selection would tint its siblings.
    pub clip: SelectionClip,
    /// The pane's live selection background (`0x00RRGGBB`), or [`COLOR_UNSET`] for the
    /// renderer's theme; per entry so an OSC 17 in one pane cannot recolour a sibling.
    pub bg: u32,
    /// The pane's live selected-text foreground, or [`COLOR_UNSET`] /
    /// [`COLOR_DYNAMIC`]; same encoding as [`RenderInput::selection_fg`].
    pub fg: u32,
    /// Whether this pane is unfocused, so its band takes the renderer's inactive selection
    /// colour.
    pub inactive: bool,
}

/// The per-row, per-entry selection key the row-damage diff
/// (`aterm_render::compute_dirty_rows`) compares. Lossless, unlike the hull
/// [`RenderInput::selection_row_span`], which dragging one of two panes' edges inward can
/// leave unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionRowKey {
    /// First selected column on this frame row (inclusive, post-clip).
    pub lo: u16,
    /// Last selected column on this frame row (inclusive, post-clip).
    pub hi: u16,
    /// The entry's live selection background (or [`COLOR_UNSET`]).
    pub bg: u32,
    /// The entry's live selected-text foreground (or [`COLOR_UNSET`] /
    /// [`COLOR_DYNAMIC`]).
    pub fg: u32,
    /// The entry's inactive flag (a different band colour).
    pub inactive: bool,
}

/// The inclusive selected column span one selection puts on one frame row, shared by the
/// scalar path and every [`PaneSelection`] so a highlight and its damage band agree.
/// `display_offset` is the viewport projection (zero for already-relocated entries).
fn selection_row_span_of(
    selection: &TextSelection,
    clip: Option<SelectionClip>,
    display_offset: i32,
    frame_row: usize,
) -> Option<(u16, u16)> {
    use crate::selection::SelectionType;

    if !selection.has_selection() {
        return None;
    }
    let selection_row = i32::try_from(frame_row)
        .unwrap_or(i32::MAX)
        .saturating_sub(display_offset);
    let (mut lo, mut hi) = match selection.selection_type() {
        SelectionType::Lines => {
            let (start_row, _, end_row, _) = selection.normalized_bounds();
            if selection_row < start_row || selection_row > end_row {
                return None;
            }
            (0, u16::MAX)
        }
        SelectionType::Block => {
            let (start_row, start_col, end_row, end_col) = selection.side_adjusted_bounds()?;
            if selection_row < start_row.min(end_row) || selection_row > start_row.max(end_row) {
                return None;
            }
            (
                start_col.min(end_col).saturating_sub(1),
                start_col.max(end_col).saturating_add(1),
            )
        }
        _ => {
            let (start_row, start_col, end_row, end_col) = selection.side_adjusted_bounds()?;
            if selection_row < start_row || selection_row > end_row {
                return None;
            }
            (
                if selection_row == start_row {
                    start_col
                } else {
                    0
                },
                if selection_row == end_row {
                    end_col
                } else {
                    u16::MAX
                },
            )
        }
    };

    if let Some(clip) = clip {
        if frame_row < clip.row_start || frame_row >= clip.row_end || clip.col_start >= clip.col_end
        {
            return None;
        }
        let clip_lo = u16::try_from(clip.col_start).ok()?;
        let clip_hi_exclusive = clip.col_end.min(usize::from(u16::MAX).saturating_add(1));
        if clip_hi_exclusive == 0 {
            return None;
        }
        let clip_hi = u16::try_from(clip_hi_exclusive - 1).unwrap_or(u16::MAX);
        lo = lo.max(clip_lo);
        hi = hi.min(clip_hi);
    }
    (lo <= hi).then_some((lo, hi))
}

/// Everything a renderer reads for one frame, as plain owned data, emitted by
/// [`crate::terminal::Terminal::cell_frame_into`]. The frontend holds the `Terminal` lock
/// only to extract this, so the PTY reader is not starved while a frame renders.
///
/// `PartialEq`/`Eq` compare rendered content only: the CPU damage cache compares each frame
/// against the cached one, and per-frame metadata ([`snapshot_seq`](RenderInput::snapshot_seq),
/// the continuity tokens, present-time fields) would defeat it. Host-owned effect channels are
/// usually empty.
#[derive(Debug)]
pub struct RenderInput {
    /// Number of visible rows this frame was extracted for.
    pub rows: usize,
    /// Number of columns this frame was extracted for.
    pub cols: usize,
    /// One resolved `RenderCell` row per visible row, in viewport order.
    pub cells: Vec<Vec<RenderCell>>,
    /// Cursor cell row.
    pub cursor_row: usize,
    /// Cursor cell column.
    pub cursor_col: usize,
    /// DECTCEM cursor visibility.
    pub cursor_visible: bool,
    /// The terminal's own DECSCUSR style; the renderer applies the unfocused override.
    pub cursor_style: CursorStyle,
    /// Host/effect cursor shape for this frame, kept apart from
    /// [`cursor_style`](Self::cursor_style) so an effect never overwrites DECSCUSR. Resolution:
    /// backend override (the unfocused HollowBlock) > this > DECSCUSR.
    pub cursor_effect_style_override: Option<CursorStyle>,
    /// The cursor motion trail ([`TrailCell`]), filled and decayed by the host's clock;
    /// `cell_frame_into` leaves it untouched.
    pub cursor_trail: Vec<TrailCell>,
    /// The host-resolved trail colour (`0x00RRGGBB`) blended over each trail cell.
    pub cursor_trail_color: u32,
    /// The cursor aurora's light quads (comet, bloom crown, landing ring, sparks), under the
    /// crisp cursor; window-absolute ([`GlowQuad`] invariants).
    pub cursor_glow_add: Vec<GlowQuad>,
    /// Radial cursor-effect light (fire embers, crown) with the [`RainHalo`] integer falloff,
    /// drawn after the aurora and before the nova and rain streams. Window-absolute, one row
    /// band per quad (a multi-row halo is per-row quads sharing a centre).
    pub glow_halo: Vec<RainHalo>,
    /// Under-glyph additive light (the fire body glyphs sit inside as silhouettes): drawn after
    /// the cell background and under-text sprites, before glyph ink. Pairs with
    /// [`char_fg`](RenderInput::char_fg); window-absolute, one row band per quad.
    pub glow_under: Vec<GlowQuad>,
    /// Per-pixel procedural fire-field patches, drawn at the
    /// [`glow_under`](RenderInput::glow_under) slot so engulfed glyphs read as silhouettes;
    /// [`FireMode::Add`] patches before [`FireMode::Over`]. Window-absolute, one row band each.
    pub fire_patch: Vec<FirePatch>,
    /// Producer-built compact damage metadata for [`cursor_glow_add`](Self::cursor_glow_add).
    pub cursor_glow_add_damage: EffectStreamDamage,
    /// Producer-built compact damage metadata for [`glow_halo`](Self::glow_halo).
    pub glow_halo_damage: EffectStreamDamage,
    /// Producer-built compact damage metadata for [`glow_under`](Self::glow_under).
    pub glow_under_damage: EffectStreamDamage,
    /// Producer-built compact damage metadata for [`fire_patch`](Self::fire_patch).
    pub fire_patch_damage: EffectStreamDamage,
    /// Host-resolved cursor fill override (`0x00RRGGBB`), or `None` for the
    /// theme/OSC-12 colour. Applies to every cursor shape (block, hollow, bar,
    /// underline, Bolt). The host holds an effect-owned fill to its own page
    /// floor before it lands here (aterm-effects `cursor_fx::floor_block_fill`);
    /// the renderer then applies `floor_cursor_fill`, which is the user's
    /// minimum-contrast knob (off by default: the fill is used as given), to the
    /// fill and the block cut-out alike. Host-owned (rainbow, forge, phaser,
    /// lightning, comet, droplet, light rod, momentum).
    pub cursor_fill_override: Option<u32>,
    /// Sparkle-word decorations: small sprites over cells of matched words.
    pub word_decorations: Vec<WordDecoration>,
    /// Animated-ink per-cell fg overrides: final colours sorted by `(row, col)`, unique cells,
    /// substituted for `fg` at every fg consult (SGR 58 still wins) before the contrast and
    /// selection floors.
    pub ink: Vec<InkCell>,
    /// Per-cell final glyph fg overrides (fire's dark glyph cores), substituted at the same
    /// seam as [`ink`](RenderInput::ink), which wins on a cell with both. Sorted by
    /// `(row, col)`, unique cells.
    pub char_fg: Vec<CharFg>,
    /// Per-cell contrast-halo strengths (fire legibility): the engulfment weight of the dark
    /// ring under a glyph's ink. Colour-free, sorted by `(row, col)`, unique cells.
    pub fire_halo: Vec<FireHaloCell>,
    /// Peeking-cat sprites drawn under the glyphs and inline images on both backends. Each
    /// quad lies in one row band ([`SpriteQuad`]; a two-row cat is a head quad in row `r-1`
    /// plus a chin slice in row `r`), dirtied by its `row` tag alone. aterm's own cursor cat
    /// uses [`free_sprites`](RenderInput::free_sprites); this channel stays in the public
    /// contract, held by the GPU parity suites (`cat_parity.rs`).
    pub cat_quads: Vec<SpriteQuad>,
    /// RGBA8 atlas for [`cat_quads`](RenderInput::cat_quads) (the host's `CatBaker`). The GPU
    /// re-uploads only when [`SceneAtlas::version`] changes, so a rebake must bump it.
    pub cat_atlas: Option<std::sync::Arc<SceneAtlas>>,
    /// Free-floating decorative sprites ([`FreeSprite`]): pixel rects with no row tag; dirty
    /// tracking unions each sprite's `[y, y+h)` extent.
    pub free_sprites: Vec<FreeSprite>,
    /// RGBA8 atlas for [`free_sprites`](RenderInput::free_sprites), versioned like
    /// [`cat_atlas`](RenderInput::cat_atlas).
    pub free_atlas: Option<std::sync::Arc<SceneAtlas>>,
    /// Supernova additive light (crown, shockwave ring, rays), like
    /// [`cursor_glow_add`](RenderInput::cursor_glow_add) but grid-interior; one row band per
    /// quad. Drawn after the aurora, under the word decorations and the cursor.
    pub nova_add: Vec<GlowQuad>,
    /// Matrix-rain glyph sprites drawn under text, before
    /// [`cat_quads`](RenderInput::cat_quads) (cats walk on rain). Each quad lies in one row
    /// band (row-only dirty marking would ghost a violator).
    pub rain_quads: Vec<SpriteQuad>,
    /// RGBA8 white-coverage atlas for [`rain_quads`](RenderInput::rain_quads) (the
    /// `RainBaker`'s); versioned like [`cat_atlas`](RenderInput::cat_atlas).
    pub rain_atlas: Option<std::sync::Arc<SceneAtlas>>,
    /// Rain bright-head additive halos, like [`nova_add`](RenderInput::nova_add):
    /// grid-interior pixels, one row band per quad.
    pub rain_add: Vec<RainHalo>,
    /// Retained fx layers ([`FxLayer`]): the engine's sprite and bitmap
    /// primitive, drawn at their [`FxSlot`]s and sliced by the renderer at the
    /// cell-row bands, so a layer spans rows freely. Compared by identity (the
    /// content `Arc`, never its bytes), so a settled layer is a pointer
    /// compare. Host-owned; usually empty.
    pub fx_layers: Vec<FxLayer>,
    /// Host chrome painted over every fx layer, in window pixels (a find bar's
    /// band, a menu card): the renderer subtracts each from every layer's draw
    /// rect, so several compose. Rendered content: compared in `PartialEq`.
    /// Host-owned; usually empty.
    pub fx_occluders: Vec<PxRect>,
    /// Arts a GPU renderer should make resident ahead of the frame that draws
    /// them (the card's arts when a drawer image is published). Draws
    /// nothing; excluded from `PartialEq` and from damage, and not kept in the
    /// damage cache. Host-owned; usually empty.
    pub fx_prefetch: Vec<std::sync::Arc<FxArt>>,
    /// Scrollback offset: viewport row `r` shows live row `r - display_offset`.
    pub display_offset: i32,
    /// Absolute row of the top visible line at extraction (`Grid::base_y()`), under the cells'
    /// lock: `viewport_row = absolute - base_y + display_offset` (e.g. the find bar's
    /// highlights).
    pub base_y: i64,
    /// Revision of top-anchored protected-footer row insertions at extraction, which one
    /// `base_y` delta cannot re-anchor: hosts retaining absolute-row overlays reject stale
    /// geometry with it.
    pub absolute_row_revision: u64,
    /// `Grid::history_renumber_epoch()` at extraction: the wholesale counterpart of
    /// `absolute_row_revision` (a width reflow renumbers every row, a Kitty CSI +T unscroll
    /// drops the newest scrollback lines). A host re-anchoring absolute-row geometry fails
    /// closed on both stamps.
    pub history_renumber_epoch: u64,
    /// The holder fence at extraction ([`Terminal::holder_view`]), resolved to
    /// absolute rows under the same lock as [`base_y`](Self::base_y): which
    /// main-screen rows the current drawer wrote, and whether the alternate
    /// screen is wholly its. A card is decorated only inside it. Metadata,
    /// excluded from `PartialEq` like `base_y`; `HolderView::NONE` (token 0)
    /// where nothing cuts (wasm, gpu-web, a replay).
    ///
    /// [`Terminal::holder_view`]: crate::terminal::Terminal::holder_view
    pub holder: crate::terminal::HolderView,
    /// Sub-row scroll translate (display only): the signed fractional-pixel
    /// residual smooth scrolling banks below one row
    /// (`scroll_frac_px ∈ (-cell_h, cell_h)`). Positive is the `frac` half of
    /// [`scroll_motion::decompose`] for a whole-row glide; negative is the
    /// elastic-overscroll spring at a history end. The renderer shifts the
    /// terminal-content band by this many device px at present time (up for
    /// positive, down for negative), leaving glyph rasters untouched. `0` is an
    /// identity. Present-time only, so excluded from `PartialEq`/`Eq`: a
    /// frac-only change re-translates from the damage cache without dirtying a
    /// cell.
    pub scroll_frac_px: i32,
    /// The terminal-content row band `[grid_top_row, grid_bot_row)`; chrome outside it is
    /// pinned while [`scroll_frac_px`](Self::scroll_frac_px) translates the band.
    /// `grid_bot_row == 0` means no band (no translate).
    pub grid_top_row: usize,
    /// One past the last terminal-content row — see [`grid_top_row`](Self::grid_top_row).
    pub grid_bot_row: usize,
    /// The row just below the viewport, which a positive
    /// [`scroll_frac_px`](Self::scroll_frac_px) glide slides in at the bottom. Stamped on
    /// every fill (the damage-scoped arm included); absent at `display_offset == 0`.
    pub apron_row: ApronRow,
    /// The facts `apron_row` was last extracted under, so an unchanged fill keeps the row
    /// (`Terminal::apron_row_into`). A writer that changes `apron_row` without an extraction
    /// (the split compositor's clear) clears this too.
    pub apron_stamp: Option<ApronStamp>,
    /// Focused-pane effect clip `(x0, y0, x1, y1)` in window-absolute device pixels (`None`
    /// outside a split). Present-time GPU post-fx (bloom halo, heat shimmer) intersect with it
    /// so light never crosses a divider.
    pub fx_clip: Option<(u16, u16, u16, u16)>,
    /// A clone of the active text selection, for per-cell highlighting.
    pub selection: TextSelection,
    /// Optional frame-space box confining [`selection`](Self::selection), so a split's
    /// focused-pane selection cannot tint a divider or sibling.
    pub selection_clip: Option<SelectionClip>,
    /// The live selection background (`0x00RRGGBB`, OSC 17/21); `COLOR_UNSET` uses the
    /// renderer's theme. Separate from the selection so a colour-only change invalidates.
    pub selection_bg: u32,
    /// The live selected-text foreground (`0x00RRGGBB`, OSC 19/21). [`COLOR_UNSET`] uses the
    /// renderer's policy; [`COLOR_DYNAMIC`] selects the automatic WCAG contrast floor.
    pub selection_fg: u32,
    /// Every visible pane's selection in frame coordinates, each with its own clip and
    /// colours. Empty for single-terminal frames, where the scalars above are the authority;
    /// renderer predicates read the list when it is non-empty.
    pub selections: Vec<PaneSelection>,
    /// Per-row, sparse emoji grapheme-cluster strings:
    /// `(col, cluster)` for cells whose combining marks form a ZWJ / skin-tone /
    /// keycap sequence. The renderer shapes each to a colour glyph, or composites
    /// the several glyphs a colour face leaves it as; cells absent here take the
    /// ordinary single-codepoint dispatch.
    pub clusters: Vec<Vec<(usize, Box<str>)>>,
    /// Per-row, sparse combining marks: `(col, marks)`
    /// for cells with diacritics (é, ñ, …). The renderer overlays each mark's
    /// glyph on the base so accents render.
    pub combining: Vec<Vec<(usize, Box<[char]>)>>,
    /// Per-row BiDi visual→logical column map: `visual_to_logical[r][v] == l` means the cell
    /// painted at column `v` of row `r` is logical column `l`. An empty or missing row, and
    /// any column past a row's map, is the identity. The engine fill records each
    /// `apply_bidi_reorder` permutation; a composed row carries each pane's map at its
    /// columns, and a host that paints a row over clears it. The selection is a logical range,
    /// so [`Self::selection_hit`] tests `map[v]`: what is highlighted is what is copied.
    pub visual_to_logical: Vec<Vec<u16>>,
    /// Per-row DEC line size (DECDWL/DECDHL), drawn scaled. On a row with
    /// [`line_size_spans`](Self::line_size_spans) it is only a summary (non-single when any
    /// pane is), for row-level consumers; placement uses
    /// [`line_size_run_at`](Self::line_size_run_at).
    pub line_sizes: Vec<LineSize>,
    /// Per-row DEC line-size runs for composed split rows where panes disagree; non-empty runs
    /// partition `0..cols` ascending. Empty means [`line_sizes`](Self::line_sizes) governs.
    pub line_size_spans: Vec<Vec<LineSizeSpan>>,
    /// Pane-local live default-background intervals for composed split rows: one
    /// [`DefaultBgSpan`] per pane, so opacity, image layering and cursor/trail fallbacks use
    /// the default of the pane that supplied each cell. An empty row uses
    /// [`default_bg`](Self::default_bg).
    ///
    /// scope-waiver: a default background is a colour, not an enforcement
    /// budget; no instance count can make it wrong, and per-pane is correct.
    pub default_bg_spans: Vec<Vec<DefaultBgSpan>>,
    /// Host chrome rows drawn at pixel resolution ([`ChromeRaster`]), at most one per row, in
    /// any order; a changed raster dirties its row.
    pub chrome_rasters: Vec<ChromeRaster>,
    /// Per-row, sparse inline-image placements (`term.images_row(r)`): `(col, ImageRef)` for
    /// every covered cell. The renderer decodes each image once (keyed by its `Arc`) and blits
    /// the cell's tile.
    pub images: Vec<Vec<(usize, aterm_grid::ImageRef)>>,
    /// Wallpaper base layer: a host-prepared RGBA8 image, pre-scaled to the frame (cover-fit)
    /// and pre-dimmed; the renderer only blits. It shows through the base clear, padding and
    /// every unselected cell on its pane's live default bg; selected cells and SGR
    /// backgrounds paint over it (kitty's below-bg-image rule). Compared by `Arc` identity; a
    /// change forces a full repaint, and it disables the scroll-blit rescue.
    pub wallpaper: Option<std::sync::Arc<SceneAtlas>>,
    /// The live default background (`0x00RRGGBB`), DECSCNM folded in; the renderer paints the
    /// padding band and base clear from it, so OSC 11/111 and DECSCNM reach the border.
    /// `COLOR_UNSET` only for a pristine `Terminal::new` (renderers keep their theme).
    pub default_bg: u32,
    /// The live default foreground (`0x00RRGGBB`), the twin of [`Self::default_bg`] (DECSCNM
    /// swaps both or neither). The cursor-effect layer anchors its fresh-typed tint on it.
    /// `COLOR_UNSET` is unknown, never a colour (masked, it reads as black).
    pub default_fg: u32,
    /// The implicit empty cell resolved under the cells' lock. Always concrete, unlike
    /// `default_fg` / `default_bg`, so host row overlays get the live ink and ground without a
    /// second lock.
    pub implicit_blank: RenderCell,
    /// The live cursor colour (`0x00RRGGBB`): OSC 12 or the configured OSC 112 baseline, or the
    /// live OSC 10 foreground while OSC 21 `cursor=` is dynamic. `COLOR_UNSET` keeps the
    /// renderer's theme cursor (a pristine `Terminal::new`).
    pub cursor_color: u32,
    /// The engine's damage epoch under the fill's lock
    /// ([`Terminal::damage_epoch`](crate::terminal::Terminal::damage_epoch)), so a consumer
    /// detects staleness against a later read.
    pub snapshot_seq: u64,
    /// Content-only arrival clock at the fill; damage-only frames do not advance it.
    pub content_seq: u64,
    /// Parser batch sequence at the fill. Unlike the damage epoch it advances even when a batch
    /// joins a pending damage session, so one-shot input evidence can reject intervening
    /// output.
    pub process_sequence: u32,
    /// Present-time latency hint: this frame is an immediate keystroke echo, so the GPU path
    /// defers the bloom halo (a framebuffer copy plus a second `queue.submit`) to the next
    /// settle frame. Must never dirty a cell; the CPU backend ignores it.
    pub input_hot: bool,
    /// Identity of the engine instance that last filled this snapshot (`0`: never), so a
    /// scratch reused across terminals (the split compositor's shared `pane_scratch`) never
    /// serves one pane's retained rows as another's.
    pub terminal_id: u64,
    /// The stamping terminal's extraction generation at fill time (`0`: never filled). A
    /// damage-scoped refill is sound only while no consumer has taken the damage
    /// (`Terminal::take_damage`) since.
    pub extract_gen: u64,
    /// `snapshot_seq` as stamped by the last engine fill. Host mutators that
    /// touch content channels afterwards (prediction ghosts, the IME preedit,
    /// the tab-strip splice) bump `snapshot_seq`, so `snapshot_seq !=
    /// engine_fill_seq` means a host wrote cells since the engine fill, and the
    /// damage-scoped refill falls back to a full refill. Metadata, excluded
    /// from `PartialEq`/`Eq`.
    pub engine_fill_seq: u64,
    /// Whether the stamping terminal's alternate screen was active at fill time: primary and
    /// alt keep independent damage state, so the scoped refill full-refills on a mismatch.
    pub engine_alt: bool,
    /// Which column order the last engine fill left this snapshot's rows in.
    ///
    /// `apply_bidi_reorder` is an in-place, non-idempotent row permutation, so
    /// retained rows may be kept only while they are still in logical order.
    /// [`RowOrder::BidiVisual`] forces the next refill to the full arm; a
    /// pure-LTR frame keeps the scoped arm. Metadata, excluded from
    /// `PartialEq`/`Eq`.
    pub engine_row_order: RowOrder,
    /// Per-row content revision ([`Grid::row_revisions`](crate::grid::Grid::row_revisions)),
    /// captured under the cells' lock, so a consumer diffing two snapshots
    /// (`aterm_render::compute_dirty_rows`) answers "did row `r` change?" with a `u64`
    /// compare. Within one pinned lane ([`row_rev_lane`](Self::row_rev_lane)) a changed row
    /// always has a different entry; an unchanged row may be re-stamped (over-repaint, never a
    /// skip). `0` means unknown: compare content (host-built rows carry it).
    pub row_rev: Vec<u64>,
    /// Identity of the lane [`row_rev`](Self::row_rev) belongs to, or `0` when its stamps must
    /// not be trusted. Only an engine fill sets it (the terminal's extraction identity);
    /// stamps compare only when both lanes are non-zero and equal, since distinct terminals
    /// mint revisions from independent clocks. A consumer must also pin row identity
    /// (`base_y`, `absolute_row_revision`, `engine_alt`, zero `display_offset`,
    /// `engine_row_order`) and `snapshot_seq == engine_fill_seq`.
    pub row_rev_lane: u64,
    /// How many host rows (the in-grid tab strip's chrome rows) have been prepended since the
    /// engine fill: engine row `r` lives at index `r + row_shift`. Physical, not a provenance
    /// claim: it counts rows prepended blessed or not, so an un-splice knows how many to
    /// remove, and `aterm_render::compute_dirty_rows` compares it across frames.
    pub row_shift: usize,
    /// `snapshot_seq` as of the last operation that left this snapshot equal to an engine fill
    /// shifted down by [`row_shift`](Self::row_shift) host rows (the fill itself, or a pure
    /// row prepend on top of one). A second clock rather than a relaxed `engine_fill_seq`: the
    /// strip splice writes no engine row, so the engine's revisions still describe index
    /// `r + row_shift`. Any other host write bumps `snapshot_seq` alone, so both the stamp
    /// gate and the un-splice fail closed. Only an engine fill and
    /// [`note_host_row_prepend`](Self::note_host_row_prepend) advance it; `0` is the
    /// never-blessed sentinel (`empty()`, every composed frame, a prepend over mutated cells).
    pub shifted_fill_seq: u64,
    /// `snapshot_seq` as of the last composed frame a host committed here: the split
    /// compositor's row-retention token, the twin of
    /// [`shifted_fill_seq`](Self::shifted_fill_seq) one level up. Retaining a composite's
    /// unchanged cells is sound only while nothing touched them since the last compose; a host
    /// write bumps `snapshot_seq` alone, so retention fails closed. An engine fill and every
    /// row rebuild ([`invalidate_row_revisions`](Self::invalidate_row_revisions)) reset it to
    /// `0`, the never-blessed sentinel; only [`bless_composed_fill`](Self::bless_composed_fill)
    /// sets anything else.
    pub composed_fill_seq: u64,
}

/// The column order the last engine fill left a [`RenderInput`]'s rows in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowOrder {
    /// No row was permuted: logical order and visual order coincide, for every
    /// row and every column-indexed channel (cells, clusters, combining marks,
    /// images, and the cursor column). This is every frame of a pure-LTR
    /// session.
    #[default]
    Logical,
    /// A row was permuted into BiDi visual order. The permutation is not idempotent, so these
    /// rows may not be retained by a damage-scoped refill.
    BidiVisual,
}

/// The first continuity clause, in the check's short-circuit order, that refused the
/// damage-scoped arm (`FrameRefill::Full`), so a Full-dominated steady state is diagnosable.
///
/// `#[repr(usize)]` keeps `FrameRefill` in rustc's ScalarPair ABI (`Scoped` carries a
/// `usize`), so `cell_frame_damage_scoped_into` returns in two registers on the keystroke path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(usize)]
pub enum FullRefillCause {
    /// The last engine fill permuted a row into BiDi visual order; retained rows would be
    /// double-permuted.
    BidiVisual,
    /// The scratch was never engine-filled (`terminal_id == 0`): every window's first frame.
    ScratchUnstamped,
    /// The scratch was last engine-filled by a different terminal (the split
    /// compositor's shared pane scratch hands off between panes).
    TerminalMismatch,
    /// Another consumer called `take_damage` since the scratch's fill
    /// (`extract_gen` moved), so the tracker's bits no longer describe every
    /// row that changed under this scratch.
    DamageTaken,
    /// A host mutator wrote content channels after the engine fill and bumped
    /// `snapshot_seq` (prediction ghosts, IME preedit, find bar, strip splice).
    HostMutation,
    /// A host row prepend is still in force (`row_shift != 0`): `undo_host_row_prepend` did
    /// not run.
    RowShift,
    /// The alternate-screen bit differs between the scratch and the engine.
    AltScreen,
    /// The scratch's own `rows`/`cols` disagree with the requested frame size —
    /// a resize the scratch has not been refilled at yet.
    ScratchRows,
    /// The scratch's row count (`cells.len()`) disagrees with the requested
    /// rows while its `rows` field agrees: a host prepend that grew the
    /// container without stamping a shift.
    ScratchRowCount,
    /// The requested frame size disagrees with the engine's own grid (a resize
    /// in flight).
    EngineDims,
    /// The viewport is scrolled back and the scrolled continuity proof
    /// (`Terminal::scrolled_scoped_refill_delta`) did not hold: the scratch entered history
    /// from the live bottom, content or row identity moved under the parked viewport, the
    /// tracker holds whole-grid damage, or the net move exposes the whole viewport (which is
    /// also where output under a viewport parked deeper than one screen lands).
    EngineScrolled,
    /// The engine is at offset 0 but the scratch was filled while scrolled
    /// back (the frame that returns to the bottom), so its retained rows use a
    /// different viewport mapping.
    ScratchScrolled,
    /// `base_y` moved: the live grid scrolled, so retained rows shifted out from under their
    /// indices. A scrolling workload reports this on nearly every frame.
    BaseY,
    /// The absolute row revision moved without `base_y` moving: a protected
    /// footer insertion or another row-identity change.
    RowRevision,
    /// The tracker holds `Damage::Full` (a palette recolor, DECSCNM, a reflow,
    /// or another whole-grid invalidation). Nothing is retainable.
    FullDamage,
}

impl FullRefillCause {
    /// Every cause, in discriminant order; the tallies and the metrics wire format are built
    /// from it.
    pub const ALL: [Self; Self::COUNT] = [
        Self::BidiVisual,
        Self::ScratchUnstamped,
        Self::TerminalMismatch,
        Self::DamageTaken,
        Self::HostMutation,
        Self::RowShift,
        Self::AltScreen,
        Self::ScratchRows,
        Self::ScratchRowCount,
        Self::EngineDims,
        Self::EngineScrolled,
        Self::ScratchScrolled,
        Self::BaseY,
        Self::RowRevision,
        Self::FullDamage,
    ];

    /// How many causes exist — the width of a per-cause tally.
    pub const COUNT: usize = 15;

    /// Dense index into a `[_; COUNT]` tally (the discriminant).
    #[must_use]
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Stable snake_case wire label for metrics text and JSON (dashboards key off it).
    #[must_use]
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BidiVisual => "bidi_visual",
            Self::ScratchUnstamped => "scratch_unstamped",
            Self::TerminalMismatch => "terminal_mismatch",
            Self::DamageTaken => "damage_taken",
            Self::HostMutation => "host_mutation",
            Self::RowShift => "row_shift",
            Self::AltScreen => "alt_screen",
            Self::ScratchRows => "scratch_rows",
            Self::ScratchRowCount => "scratch_row_count",
            Self::EngineDims => "engine_dims",
            Self::EngineScrolled => "engine_scrolled",
            Self::ScratchScrolled => "scratch_scrolled",
            Self::BaseY => "base_y",
            Self::RowRevision => "row_revision",
            Self::FullDamage => "full_damage",
        }
    }
}

/// How [`Terminal::cell_frame_damage_scoped_into`](crate::terminal::Terminal::cell_frame_damage_scoped_into)
/// refilled a scratch, so callers and tests can assert a workload reaches the scoped arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRefill {
    /// The whole viewport was re-extracted (continuity break, full damage, first fill).
    Full {
        /// The first continuity clause that refused the scoped arm.
        cause: FullRefillCause,
    },
    /// Only the damage tracker's rows were re-extracted; the rest are byte-identical by the
    /// continuity invariant.
    Scoped {
        /// Viewport rows re-resolved (the tracker's damaged rows, clamped). 0 is legal: a
        /// cursor-only move still restamps the cursor and selection scalars.
        rows_refilled: usize,
    },
}

impl Clone for RenderInput {
    /// A deep copy of every field, `snapshot_seq` included.
    #[inline]
    fn clone(&self) -> Self {
        let mut out = Self::empty();
        out.copy_from(self, true);
        out
    }

    /// Capacity-reusing in-place update, identical to `*self = source.clone()`.
    #[inline]
    fn clone_from(&mut self, source: &Self) {
        self.copy_from(source, true);
    }
}

/// Content equality for the per-row image placements: the derived `ImageRef` equality minus
/// `ImageRef::kitty` (a tag that draws no pixel). A re-transmitting client allocates a fresh
/// `Arc` per transmit, so the deep payload compare is memoized per `Arc` pair for this call
/// (as `aterm-render`'s `ImageEqMemo` does). The raw-pointer key is sound only because the
/// borrowed inputs keep every `Arc` alive for the call: never persist the map.
fn images_eq(
    a: &[Vec<(usize, aterm_grid::ImageRef)>],
    b: &[Vec<(usize, aterm_grid::ImageRef)>],
) -> bool {
    let mut verdicts: FxHashMap<(usize, usize), bool> = FxHashMap::default();
    let mut image_eq = |x: &std::sync::Arc<aterm_grid::ImageData>,
                        y: &std::sync::Arc<aterm_grid::ImageData>|
     -> bool {
        if std::sync::Arc::ptr_eq(x, y) {
            return true;
        }
        let key = (
            std::sync::Arc::as_ptr(x) as usize,
            std::sync::Arc::as_ptr(y) as usize,
        );
        if let Some(&verdict) = verdicts.get(&key) {
            return verdict;
        }
        let verdict = **x == **y;
        verdicts.insert(key, verdict);
        verdict
    };
    a.len() == b.len()
        && a.iter().zip(b).all(|(ra, rb)| {
            ra.len() == rb.len()
                && ra.iter().zip(rb).all(|((ca, ia), (cb, ib))| {
                    ca == cb
                        && ia.cell_row == ib.cell_row
                        && ia.cell_col == ib.cell_col
                        && image_eq(&ia.image, &ib.image)
                })
        })
}

// Rendered-content equality: the CPU damage cache compares each snapshot against the previous
// frame's, so per-frame metadata is excluded (listed at the end). Atlases compare by identity
// (`Arc::as_ptr`), never `version`: a rebuilt engine replays its predecessor's versions with
// different texels, and every rebake publishes a fresh `Arc`.
impl PartialEq for RenderInput {
    fn eq(&self, other: &Self) -> bool {
        self.rows == other.rows
            && self.cols == other.cols
            && self.cells == other.cells
            && self.cursor_row == other.cursor_row
            && self.cursor_col == other.cursor_col
            && self.cursor_visible == other.cursor_visible
            && self.cursor_style == other.cursor_style
            && self.cursor_effect_style_override == other.cursor_effect_style_override
            && self.cursor_trail == other.cursor_trail
            && self.cursor_trail_color == other.cursor_trail_color
            && self.cursor_glow_add == other.cursor_glow_add
            && self.glow_halo == other.glow_halo
            && self.glow_under == other.glow_under
            && self.fire_patch == other.fire_patch
            && self.cursor_fill_override == other.cursor_fill_override
            && self.word_decorations == other.word_decorations
            && self.ink == other.ink
            && self.char_fg == other.char_fg
            && self.fire_halo == other.fire_halo
            && self.cat_quads == other.cat_quads
            && self.cat_atlas.as_ref().map(std::sync::Arc::as_ptr)
                == other.cat_atlas.as_ref().map(std::sync::Arc::as_ptr)
            && self.free_sprites == other.free_sprites
            && self.free_atlas.as_ref().map(std::sync::Arc::as_ptr)
                == other.free_atlas.as_ref().map(std::sync::Arc::as_ptr)
            && self.nova_add == other.nova_add
            && self.rain_quads == other.rain_quads
            && self.rain_atlas.as_ref().map(std::sync::Arc::as_ptr)
                == other.rain_atlas.as_ref().map(std::sync::Arc::as_ptr)
            && self.rain_add == other.rain_add
            // Fx layers by identity (`FxLayer`'s `PartialEq`: the content
            // `Arc` is pointer-compared, like every atlas above).
            && self.fx_layers == other.fx_layers
            && self.fx_occluders == other.fx_occluders
            && self.display_offset == other.display_offset
            && self.selection == other.selection
            && self.selection_clip == other.selection_clip
            && self.selection_bg == other.selection_bg
            && self.selection_fg == other.selection_fg
            && self.selections == other.selections
            && self.clusters == other.clusters
            && self.combining == other.combining
            && visual_to_logical_eq(&self.visual_to_logical, &other.visual_to_logical)
            && self.line_sizes == other.line_sizes
            && self.line_size_spans == other.line_size_spans
            && self.default_bg_spans == other.default_bg_spans
            && images_eq(&self.images, &other.images)
            && self.chrome_rasters == other.chrome_rasters
            && self.wallpaper.as_ref().map(std::sync::Arc::as_ptr)
                == other.wallpaper.as_ref().map(std::sync::Arc::as_ptr)
            && self.default_bg == other.default_bg
            && self.default_fg == other.default_fg
            && self.implicit_blank == other.implicit_blank
            && self.cursor_color == other.cursor_color
        // Not compared (metadata or present-time only, so a change must not
        // force a raster repaint):
        // - `snapshot_seq`, `base_y`, `absolute_row_revision`,
        //   `history_renumber_epoch`, `holder`: host re-anchor metadata.
        // - `scroll_frac_px`, `grid_top_row`, `grid_bot_row`, `apron_row`,
        //   `input_hot`: consumed at present time, outside the damage cache.
        // - `terminal_id`, `extract_gen`, `engine_fill_seq`, `engine_alt`,
        //   `engine_row_order`, `row_rev`, `row_rev_lane`, `shifted_fill_seq`,
        //   `composed_fill_seq`: continuity tokens that advance on every
        //   damaged frame and would defeat the dirty gate they feed.
        // - `fx_prefetch`: residency hints that draw nothing.
    }
}

impl Eq for RenderInput {}

/// [`RenderInput::visual_to_logical`] equality by meaning: a missing row and an empty one are
/// both the identity.
#[inline]
fn visual_to_logical_eq(a: &[Vec<u16>], b: &[Vec<u16>]) -> bool {
    fn row(rows: &[Vec<u16>], r: usize) -> &[u16] {
        rows.get(r).map_or(&[][..], Vec::as_slice)
    }
    (0..a.len().max(b.len())).all(|r| row(a, r) == row(b, r))
}

/// What [`RenderInput::apron_row`] was last extracted under: a fill that finds every term
/// unchanged keeps the row. The row holds resolved colours, so presentation inputs that move
/// no `content_gen` are part of its identity, and so is the terminal (tabs of one window share
/// a scratch while per-grid counters collide).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApronStamp {
    /// `display_offset` at extraction.
    pub offset: usize,
    /// The grid's content generation.
    pub content_gen: u64,
    /// The history renumbering epoch.
    pub renumber_epoch: u64,
    /// Frame shape.
    pub rows: usize,
    /// Frame shape.
    pub cols: usize,
    /// A fingerprint of every presentation input the row is resolved against
    /// that moves no `content_gen` (`Terminal::presentation_fingerprint`).
    pub presentation: u64,
    /// The extracting terminal's identity nonce (`RenderInput::terminal_id`).
    pub terminal: u64,
}

/// The incoming-row apron: the one row just below the viewport, which the sub-row translate
/// reveals ([`RenderInput::apron_row`]). A struct rather than an `Option` so the per-frame
/// refill keeps its Vec capacity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApronRow {
    /// `true` when a row lies below the viewport (`display_offset > 0`); the
    /// remaining fields are meaningful only then.
    pub present: bool,
    /// The row's resolved cells, exactly as a viewport row's
    /// [`RenderInput::cells`] entry.
    pub cells: Vec<RenderCell>,
    /// Its sparse emoji clusters — one [`RenderInput::clusters`] row.
    pub clusters: Vec<(usize, Box<str>)>,
    /// Its sparse combining marks — one [`RenderInput::combining`] row.
    pub combining: Vec<(usize, Box<[char]>)>,
    /// Its DEC line size (`SingleWidth` for a history row).
    pub line_size: LineSize,
    /// The cursor column when the cursor's projected row is exactly this row,
    /// else `None`, so the caret rides in with its row.
    pub cursor_col: Option<usize>,
}

impl ApronRow {
    /// `clone_from` that keeps every Vec's capacity (a derived one allocates fresh Vecs).
    #[inline]
    fn refill_from(&mut self, source: &Self) {
        let Self {
            present,
            cells,
            clusters,
            combining,
            line_size,
            cursor_col,
        } = self;
        *present = source.present;
        cells.clone_from(&source.cells);
        clusters.clone_from(&source.clusters);
        combining.clone_from(&source.combining);
        *line_size = source.line_size;
        *cursor_col = source.cursor_col;
    }

    /// Mark the apron absent, keeping every Vec's capacity for the next fill.
    #[inline]
    pub fn clear(&mut self) {
        self.present = false;
        self.cells.clear();
        self.clusters.clear();
        self.combining.clear();
        self.line_size = LineSize::SingleWidth;
        self.cursor_col = None;
    }
}

impl Default for RenderInput {
    #[inline]
    fn default() -> Self {
        Self::empty()
    }
}

/// Where the producers placed the window-space effect streams (`cursor_glow_add`,
/// `glow_under`, `fire_patch`, `glow_halo`, `fx_clip`) relative to a host row prepend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostRowPixels {
    /// Already placed below the prepended rows (the native window, whose
    /// `effects_origin_win` counts them): pixels stay put; only the row tags are re-derived.
    Placed,
    /// Placed without the prepended rows (the web module, whose effects pipeline has one
    /// chrome origin): every pixel `y` moves down by `rows · cell_h` first, then the tags are
    /// re-derived as under [`Self::Placed`].
    Translate,
}

impl RenderInput {
    /// The pixel-resolution chrome raster of frame row `r`, if the host drew
    /// one ([`ChromeRaster`]).
    #[must_use]
    #[inline]
    pub fn chrome_raster(&self, r: usize) -> Option<&ChromeRaster> {
        self.chrome_rasters.iter().find(|m| usize::from(m.row) == r)
    }

    /// Update a damage-cache snapshot, dropping the four large cursor-effect payloads whose
    /// producer supplied compact metadata. A cached frame is never drawn again, only read by
    /// the dirty-row decision, so the metadata is a complete prior; input without metadata is
    /// cloned in full.
    #[inline]
    pub fn clone_damage_cache_from(&mut self, source: &Self) {
        self.copy_from(source, false);
    }

    fn copy_from(&mut self, source: &Self, retain_effect_payloads: bool) {
        self.rows = source.rows;
        self.cols = source.cols;
        self.cells.clone_from(&source.cells);
        self.cursor_row = source.cursor_row;
        self.cursor_col = source.cursor_col;
        self.cursor_visible = source.cursor_visible;
        self.cursor_style = source.cursor_style;
        self.cursor_effect_style_override = source.cursor_effect_style_override;
        self.cursor_trail.clone_from(&source.cursor_trail);
        self.cursor_trail_color = source.cursor_trail_color;
        self.cursor_fill_override = source.cursor_fill_override;

        macro_rules! copy_effect_stream {
            ($field:ident, $damage:ident) => {
                if retain_effect_payloads || !source.$damage.is_valid() {
                    self.$field.clone_from(&source.$field);
                } else {
                    self.$field.clear();
                }
                self.$damage.refill_from(&source.$damage);
            };
        }
        copy_effect_stream!(cursor_glow_add, cursor_glow_add_damage);
        copy_effect_stream!(glow_halo, glow_halo_damage);
        copy_effect_stream!(glow_under, glow_under_damage);
        copy_effect_stream!(fire_patch, fire_patch_damage);

        self.word_decorations.clone_from(&source.word_decorations);
        self.ink.clone_from(&source.ink);
        self.char_fg.clone_from(&source.char_fg);
        self.fire_halo.clone_from(&source.fire_halo);
        self.cat_quads.clone_from(&source.cat_quads);
        self.cat_atlas.clone_from(&source.cat_atlas);
        self.free_sprites.clone_from(&source.free_sprites);
        self.free_atlas.clone_from(&source.free_atlas);
        self.nova_add.clone_from(&source.nova_add);
        self.rain_quads.clone_from(&source.rain_quads);
        self.rain_atlas.clone_from(&source.rain_atlas);
        self.rain_add.clone_from(&source.rain_add);
        // The layers clone their `Arc`s: the cache holds strong references,
        // so the identity it compares against can never be reused (no ABA).
        self.fx_layers.clone_from(&source.fx_layers);
        self.fx_occluders.clone_from(&source.fx_occluders);
        // Prefetch draws nothing, so the damage cache does not keep it.
        if retain_effect_payloads {
            self.fx_prefetch.clone_from(&source.fx_prefetch);
        } else {
            self.fx_prefetch.clear();
        }
        self.display_offset = source.display_offset;
        self.base_y = source.base_y;
        self.absolute_row_revision = source.absolute_row_revision;
        self.history_renumber_epoch = source.history_renumber_epoch;
        self.holder = source.holder;
        self.scroll_frac_px = source.scroll_frac_px;
        self.grid_top_row = source.grid_top_row;
        self.grid_bot_row = source.grid_bot_row;
        self.apron_row.refill_from(&source.apron_row);
        self.apron_stamp = source.apron_stamp;
        self.fx_clip = source.fx_clip;
        self.selection.clone_from(&source.selection);
        self.selection_clip = source.selection_clip;
        self.selection_bg = source.selection_bg;
        self.selection_fg = source.selection_fg;
        self.selections.clone_from(&source.selections);
        self.clusters.clone_from(&source.clusters);
        self.combining.clone_from(&source.combining);
        self.visual_to_logical.clone_from(&source.visual_to_logical);
        self.line_sizes.clone_from(&source.line_sizes);
        self.line_size_spans.clone_from(&source.line_size_spans);
        self.default_bg_spans.clone_from(&source.default_bg_spans);
        self.images.clone_from(&source.images);
        self.wallpaper.clone_from(&source.wallpaper);
        self.default_bg = source.default_bg;
        self.default_fg = source.default_fg;
        self.implicit_blank = source.implicit_blank;
        self.cursor_color = source.cursor_color;
        self.chrome_rasters.clone_from(&source.chrome_rasters);
        self.snapshot_seq = source.snapshot_seq;
        self.content_seq = source.content_seq;
        self.process_sequence = source.process_sequence;
        self.input_hot = source.input_hot;
        self.terminal_id = source.terminal_id;
        self.extract_gen = source.extract_gen;
        self.engine_fill_seq = source.engine_fill_seq;
        self.engine_alt = source.engine_alt;
        self.engine_row_order = source.engine_row_order;
        self.row_rev.clone_from(&source.row_rev);
        self.row_rev_lane = source.row_rev_lane;
        self.row_shift = source.row_shift;
        self.shifted_fill_seq = source.shifted_fill_seq;
        self.composed_fill_seq = source.composed_fill_seq;
    }

    /// Rebuild the four compact cursor-effect damage records, after all frontend composition
    /// (splices, clipping, chrome suppression).
    pub fn refresh_cursor_effect_damage(&mut self) {
        let rows = self.rows;
        self.cursor_glow_add_damage.refresh(
            &self.cursor_glow_add,
            rows,
            |quad| quad.row,
            hash_glow_quad,
        );
        self.glow_halo_damage
            .refresh(&self.glow_halo, rows, |halo| halo.row, hash_rain_halo);
        self.glow_under_damage
            .refresh(&self.glow_under, rows, |quad| quad.row, hash_glow_quad);
        self.fire_patch_damage
            .refresh(&self.fire_patch, rows, |patch| patch.row, hash_fire_patch);
    }

    /// An empty 0×0 snapshot with no allocations: the seed for a scratch buffer that
    /// [`Terminal::cell_frame_into`](crate::terminal::Terminal::cell_frame_into) refills each
    /// frame. The fill overwrites the engine-owned grid, cursor, live colours, selection and
    /// snapshot metadata; hosts stamp or clear their own overlay and presentation fields.
    #[must_use]
    pub fn empty() -> Self {
        RenderInput {
            rows: 0,
            cols: 0,
            cells: Vec::new(),
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: false,
            cursor_style: CursorStyle::default(),
            cursor_effect_style_override: None,
            cursor_trail: Vec::new(),
            cursor_trail_color: 0,
            cursor_glow_add: Vec::new(),
            glow_halo: Vec::new(),
            glow_under: Vec::new(),
            fire_patch: Vec::new(),
            cursor_glow_add_damage: EffectStreamDamage::default(),
            glow_halo_damage: EffectStreamDamage::default(),
            glow_under_damage: EffectStreamDamage::default(),
            fire_patch_damage: EffectStreamDamage::default(),
            cursor_fill_override: None,
            word_decorations: Vec::new(),
            ink: Vec::new(),
            char_fg: Vec::new(),
            fire_halo: Vec::new(),
            cat_quads: Vec::new(),
            cat_atlas: None,
            free_sprites: Vec::new(),
            free_atlas: None,
            nova_add: Vec::new(),
            rain_quads: Vec::new(),
            rain_atlas: None,
            rain_add: Vec::new(),
            fx_layers: Vec::new(),
            fx_occluders: Vec::new(),
            fx_prefetch: Vec::new(),
            display_offset: 0,
            base_y: 0,
            absolute_row_revision: 0,
            history_renumber_epoch: 0,
            holder: crate::terminal::HolderView::NONE,
            scroll_frac_px: 0,
            grid_top_row: 0,
            grid_bot_row: 0,
            apron_row: ApronRow::default(),
            apron_stamp: None,
            fx_clip: None,
            selection: TextSelection::new(),
            selection_clip: None,
            selection_bg: COLOR_UNSET,
            selection_fg: COLOR_UNSET,
            selections: Vec::new(),
            clusters: Vec::new(),
            combining: Vec::new(),
            visual_to_logical: Vec::new(),
            line_sizes: Vec::new(),
            line_size_spans: Vec::new(),
            default_bg_spans: Vec::new(),
            images: Vec::new(),
            wallpaper: None,
            default_bg: COLOR_UNSET,
            default_fg: COLOR_UNSET,
            implicit_blank: RenderCell::default(),
            cursor_color: COLOR_UNSET,
            chrome_rasters: Vec::new(),
            snapshot_seq: 0,
            content_seq: 0,
            process_sequence: 0,
            input_hot: false,
            terminal_id: 0,
            extract_gen: 0,
            engine_fill_seq: 0,
            engine_alt: false,
            engine_row_order: RowOrder::Logical,
            row_rev: Vec::new(),
            row_rev_lane: 0,
            row_shift: 0,
            shifted_fill_seq: 0,
            composed_fill_seq: 0,
        }
    }

    /// Disown the per-row revision lane: the rows are no longer the ones an engine fill
    /// stamped. Every host producer that rebuilds rows (the split compositor) calls this; a
    /// pure row prepend splices the lane instead
    /// ([`note_host_row_prepend`](Self::note_host_row_prepend)). In-place host mutators
    /// self-report through `snapshot_seq != shifted_fill_seq`, but calling it is always sound
    /// (the cost is a whole-grid content compare). Also resets the prepend bookkeeping.
    #[inline]
    pub fn invalidate_row_revisions(&mut self) {
        self.row_rev_lane = 0;
        self.row_rev.clear();
        self.row_shift = 0;
        self.shifted_fill_seq = 0;
        // Every divider-grid fill runs this, so no frame inherits the previous split
        // frame's composed blessing.
        self.composed_fill_seq = 0;
    }

    /// Record that a host just prepended `prepended` rows, after every per-row channel shifted
    /// and after the `snapshot_seq` bump. `blessed` is
    /// [`host_prepend_blessing`](Self::host_prepend_blessing), read before the mutation.
    ///
    /// Blessed, the lane gains `prepended` leading `0`s (unknown), so engine row `r` keeps its
    /// revision at index `r + row_shift`; unblessed, the lane is disowned. `row_shift`
    /// advances either way: it is the row count an un-splice removes.
    #[inline]
    pub fn note_host_row_prepend(&mut self, prepended: usize, blessed: bool) {
        self.row_shift = self.row_shift.saturating_add(prepended);
        // A prepend slides every composed row, so a composed-row ledger no longer applies.
        self.composed_fill_seq = 0;
        if blessed {
            self.row_rev.splice(0..0, (0..prepended).map(|_| 0u64));
            self.shifted_fill_seq = self.snapshot_seq;
        } else {
            self.row_rev_lane = 0;
            self.row_rev.clear();
            self.shifted_fill_seq = 0;
        }
    }

    /// Bless this snapshot as a committed composed frame, so the next compose may retain rows
    /// nothing changed. Called by the compositor after its last cell write and `snapshot_seq`
    /// bump; a later host write bumps `snapshot_seq` alone and so revokes it.
    #[inline]
    pub const fn bless_composed_fill(&mut self) {
        self.composed_fill_seq = self.snapshot_seq;
    }

    /// Whether this snapshot is still, cell for cell, the frame
    /// [`bless_composed_fill`](Self::bless_composed_fill) last blessed: `false` if never
    /// blessed, disowned by a fill or rebuild, or written by a host mutator.
    #[must_use]
    #[inline]
    pub const fn composed_fill_intact(&self) -> bool {
        self.composed_fill_seq != 0 && self.snapshot_seq == self.composed_fill_seq
    }

    /// Whether a host row prepend may keep the revision lane: the snapshot is exactly the last
    /// engine fill (possibly shifted by an earlier blessed prepend) with a lane covering every
    /// row. Read before the prepend mutates anything.
    #[must_use]
    #[inline]
    pub fn host_prepend_blessing(&self) -> bool {
        self.row_rev_lane != 0
            && self.row_rev.len() == self.rows
            && self.cells.len() == self.rows
            && self.snapshot_seq == self.shifted_fill_seq
            && (self.row_shift > 0 || self.snapshot_seq == self.engine_fill_seq)
    }

    /// Undo a blessed host row prepend, restoring the engine fill it was made from, so a
    /// tab-strip window's scratch can take the damage-scoped arm of
    /// [`Terminal::cell_frame_damage_scoped_into`](crate::terminal::Terminal::cell_frame_damage_scoped_into).
    ///
    /// Returns `false`, changing nothing, unless a blessed prepend is the only change since
    /// the engine fill and `cells`, `clusters` and `combining` are all
    /// `engine_rows + row_shift` long. Restores those channels, the row count and the
    /// continuity tokens only: an un-spliced scratch is an intermediate, never a presentable
    /// frame. Drained row buffers go to `pool` (up to `pool_cap`).
    pub fn undo_host_row_prepend(
        &mut self,
        engine_rows: usize,
        pool: &mut Vec<Vec<RenderCell>>,
        pool_cap: usize,
    ) -> bool {
        let strip = self.row_shift;
        if strip == 0
            || self.shifted_fill_seq == 0
            || self.snapshot_seq != self.shifted_fill_seq
            || self.rows != engine_rows.saturating_add(strip)
            || self.cells.len() != self.rows
            || self.clusters.len() != self.rows
            || self.combining.len() != self.rows
        {
            return false;
        }
        for buf in self.cells.drain(0..strip) {
            if pool.len() >= pool_cap {
                break;
            }
            pool.push(buf);
        }
        self.clusters.drain(0..strip);
        self.combining.drain(0..strip);
        // Lenient like `row_rev`: a frame built without maps has none to drain.
        let maps = strip.min(self.visual_to_logical.len());
        self.visual_to_logical.drain(0..maps);
        if self.row_rev.len() >= strip {
            self.row_rev.drain(0..strip);
        }
        self.rows = engine_rows;
        self.row_shift = 0;
        // The cells are the engine's again: the one place the engine-fill token is re-armed.
        self.snapshot_seq = self.engine_fill_seq;
        self.shifted_fill_seq = self.engine_fill_seq;
        // An un-splice restores an engine fill, never a composite.
        self.composed_fill_seq = 0;
        true
    }

    /// Prepend `strip` host rows (a tab strip, a message band, a presence row): every per-row
    /// channel, the cursor, the selections and every row-tagged effect stream move down by
    /// `strip` rows, the chrome rows are copied from `strip_rows`, and the revision lane is
    /// spliced so [`undo_host_row_prepend`](Self::undo_host_row_prepend) can remove them.
    ///
    /// `cell_h` is the pixel cell height. `grid_top` is the grid-interior top before the
    /// prepend (`pad_top + head`): a window-space quad above `grid_top + strip·cell_h` takes a
    /// chrome row's damage tag. `pixels` says where the window-space producers placed their
    /// streams. `pool` supplies reclaimed row buffers. The web host and `aterm-gui` share this.
    #[allow(
        clippy::too_many_lines,
        clippy::cast_possible_truncation,
        reason = "moved verbatim from aterm-gui's splice (ruling 331), whose crate does not run \
                  these pedantic lints: one pass over every channel, and a row tag is a u16 by the \
                  stream contract — a frame has far fewer than 65,536 rows"
    )]
    pub fn prepend_host_rows<'a>(
        &mut self,
        strip: usize,
        strip_rows: impl Iterator<Item = &'a [RenderCell]>,
        cell_h: usize,
        grid_top: usize,
        pool: &mut Vec<Vec<RenderCell>>,
        pixels: HostRowPixels,
    ) {
        if strip == 0 {
            return;
        }
        // Read before anything moves; `note_host_row_prepend` shifts the lane at the end.
        let blessed = self.host_prepend_blessing();
        self.cells.splice(
            0..0,
            strip_rows.map(|src| match pool.pop() {
                Some(mut buf) => {
                    buf.clear();
                    buf.extend_from_slice(src);
                    buf
                }
                None => src.to_vec(),
            }),
        );
        // Per-row channels gain empty/default rows so indices stay aligned with `cells`;
        // `(0..strip).map` is exact-size, so each splice shifts the tail once.
        self.clusters.splice(0..0, (0..strip).map(|_| Vec::new()));
        self.combining.splice(0..0, (0..strip).map(|_| Vec::new()));
        // Chrome rows are painted in logical order: identity maps, and every
        // permuted terminal row's map moves down with its cells.
        self.visual_to_logical
            .splice(0..0, (0..strip).map(|_| Vec::new()));
        self.images.splice(0..0, (0..strip).map(|_| Vec::new()));
        self.line_sizes
            .splice(0..0, (0..strip).map(|_| LineSize::SingleWidth));
        self.line_size_spans
            .splice(0..0, (0..strip).map(|_| Vec::new()));
        self.default_bg_spans
            .splice(0..0, (0..strip).map(|_| Vec::new()));
        // The band's pixel-resolution chrome rows are row-tagged: they move down
        // with the rows they belong to.
        for m in &mut self.chrome_rasters {
            m.row = m
                .row
                .saturating_add(u16::try_from(strip).unwrap_or(u16::MAX));
        }
        // The cursor, selection anchors and trail cells move down with the terminal content.
        self.cursor_row += strip;
        self.selection
            .translate_rows_for_presentation(i32::try_from(strip).unwrap_or(i32::MAX));
        if let Some(clip) = &mut self.selection_clip {
            clip.translate_rows_down(strip);
        }
        for pane in &mut self.selections {
            pane.selection
                .translate_rows_for_presentation(i32::try_from(strip).unwrap_or(i32::MAX));
            pane.clip.translate_rows_down(strip);
        }
        for t in &mut self.cursor_trail {
            t.row += strip;
        }
        // Window-space streams under `Placed` keep their pixels; only the damage row tag is
        // re-derived from the pixel y against the composed frame's bands (row r spans
        // `grid_top + r·cell_h ..`, row 0 open to the window top), so a quad above the
        // terminal grid lands in its strip row and an in-grid quad on its shifted row.
        let dy = u16::try_from(strip * cell_h).unwrap_or(u16::MAX);
        let shift_tag = |y: u16| (usize::from(y).saturating_sub(grid_top) / cell_h.max(1)) as u16;
        // Under `Translate` every pixel y (quad top, flame root, halo centre, the effect clip)
        // first moves down by `pixel_dy`; the tag rule then applies as under `Placed`.
        let pixel_dy = if pixels == HostRowPixels::Translate {
            dy
        } else {
            0
        };
        if let Some((_, y0, _, y1)) = &mut self.fx_clip {
            *y0 = y0.saturating_add(pixel_dy);
            *y1 = y1.saturating_add(pixel_dy);
        }
        for q in &mut self.cursor_glow_add {
            q.y = q.y.saturating_add(pixel_dy);
            q.row = shift_tag(q.y);
        }
        for q in &mut self.glow_under {
            q.y = q.y.saturating_add(pixel_dy);
            q.row = shift_tag(q.y);
        }
        // Fire patches: the quad and its flame-root pixel move by `pixel_dy`.
        for q in &mut self.fire_patch {
            q.y = q.y.saturating_add(pixel_dy);
            q.base_y = q.base_y.saturating_add(pixel_dy);
            q.row = shift_tag(q.y);
        }
        // Charred-ink and contrast-halo overrides are cell-tagged (grid streams).
        for c in &mut self.char_fg {
            c.row = c.row.saturating_add(strip as u16);
        }
        for c in &mut self.fire_halo {
            c.row = c.row.saturating_add(strip as u16);
        }
        // Radial halos: the quad and its falloff centre move by `pixel_dy`.
        for h in &mut self.glow_halo {
            h.y = h.y.saturating_add(pixel_dy);
            h.cy = h.cy.saturating_add(pixel_dy);
            h.row = shift_tag(h.y);
        }
        // Sparkle-word decorations are cell-row tagged (no pixel y); shift the row down.
        for d in &mut self.word_decorations {
            d.row = d.row.saturating_add(strip as u16);
        }
        // Animated-ink overrides are cell-row tagged too, emitted pre-splice.
        for c in &mut self.ink {
            c.row = c.row.saturating_add(strip as u16);
        }
        // Peeking-cat quads carry a row band and a pixel-y dest rect: shift both.
        for q in &mut self.cat_quads {
            q.row = q.row.saturating_add(strip as u16);
            q.y = q.y.saturating_add(dy);
        }
        // Free-overlay sprites are pixel rects with no row tag: shift the signed
        // dest y alone (dirty tracking re-derives the covered bands).
        let dy_free = i32::from(dy);
        for s in &mut self.free_sprites {
            s.y = s.y.saturating_add(dy_free);
        }
        // Supernova additive quads are a grid stream (grid-relative pixels):
        // shift both the row tag and the pixel y.
        for q in &mut self.nova_add {
            q.row = q.row.saturating_add(strip as u16);
            q.y = q.y.saturating_add(dy);
        }
        // Rain sprites carry a row band and a pixel-y dest rect: shift both.
        for q in &mut self.rain_quads {
            q.row = q.row.saturating_add(strip as u16);
            q.y = q.y.saturating_add(dy);
        }
        // Rain head halos: also shift the falloff centre, or the radial light
        // misregisters against its quad.
        for q in &mut self.rain_add {
            q.row = q.row.saturating_add(strip as u16);
            q.y = q.y.saturating_add(dy);
            q.cy = q.cy.saturating_add(dy);
        }
        // Fx layers carry no row tag: O(layers). A layer's offset and its
        // clip share its space, so both move: a Window layer by `pixel_dy`
        // (the window streams' rule), a Grid layer by `dy` (the grid
        // streams'). The occluders are window pixels and follow `pixel_dy`
        // like the effect clip.
        let (fx_window_dy, fx_grid_dy) = (i32::from(pixel_dy), i32::from(dy));
        for layer in &mut self.fx_layers {
            let d = match layer.space {
                FxSpace::Window => fx_window_dy,
                FxSpace::Grid => fx_grid_dy,
            };
            layer.offset.1 = layer.offset.1.saturating_add(d);
            layer.clip = layer.clip.translate(0, d);
        }
        for o in &mut self.fx_occluders {
            *o = o.translate(0, fx_window_dy);
        }
        self.rows += strip;
        // The strip changes the presented pixels: the renderer's content cache must see it.
        self.snapshot_seq = self.snapshot_seq.wrapping_add(1);
        // Splice or disown the revision lane per the blessing read at the top.
        self.note_host_row_prepend(strip, blessed);
    }

    /// Whether any selection is live (the scalar one or any pane entry); gates the sparkle
    /// freeze, ligature breaks and the scroll-blit veto.
    #[must_use]
    #[inline]
    pub fn any_selection(&self) -> bool {
        self.selection.has_selection() || !self.selections.is_empty()
    }

    /// Frame row `frame_row`'s BiDi visual→logical map
    /// ([`visual_to_logical`](Self::visual_to_logical)); empty when the row is
    /// in logical order.
    #[must_use]
    #[inline]
    pub fn visual_to_logical_row(&self, frame_row: usize) -> &[u16] {
        self.visual_to_logical
            .get(frame_row)
            .map_or(&[][..], Vec::as_slice)
    }

    /// The logical frame column of the cell painted at (`frame_row`,
    /// `frame_col`): `frame_col` itself unless BiDi reordered that row.
    #[must_use]
    #[inline]
    pub fn logical_col(&self, frame_row: usize, frame_col: usize) -> usize {
        self.visual_to_logical_row(frame_row)
            .get(frame_col)
            .map_or(frame_col, |&l| usize::from(l))
    }

    /// Where frame row `frame_row` paints the cell of logical column
    /// `logical`, the inverse of [`Self::logical_col`]: `logical` itself
    /// unless BiDi reordered that row. A caret kept in logical columns is
    /// painted here, on the cell it names.
    #[must_use]
    #[inline]
    pub fn visual_col(&self, frame_row: usize, logical: usize) -> usize {
        let map = self.visual_to_logical_row(frame_row);
        u16::try_from(logical)
            .ok()
            .and_then(|l| map.iter().position(|&m| m == l))
            .unwrap_or(logical)
    }

    /// Which selection paints a frame-space cell, or `None` when unselected: the one renderer
    /// predicate, applying the viewport (`display_offset`) projection before consulting
    /// [`TextSelection`]. With [`selections`](Self::selections) empty it is the scalar test
    /// (`Some(0)`); otherwise the first entry whose clip and selection accept the cell (pane
    /// clips are disjoint).
    ///
    /// On a BiDi-reordered row the selection (a logical range) is tested at the cell's
    /// logical column, so the highlight lands on the cells the copy reads; the clip (a pane
    /// box no permutation leaves) at the painted column. The reorder moves a wide glyph's
    /// lead and continuation as one unit, so the wide flags hold for the logical cell too.
    #[must_use]
    #[inline]
    pub fn selection_hit(
        &self,
        frame_row: usize,
        frame_col: usize,
        is_wide: bool,
        is_wide_continuation: bool,
    ) -> Option<usize> {
        if self.selections.is_empty() {
            // Scalar path, clip first: runs per painted cell on every single-terminal frame.
            if self
                .selection_clip
                .is_some_and(|clip| !clip.contains(frame_row, frame_col))
            {
                return None;
            }
            let selection_row = i32::try_from(frame_row)
                .unwrap_or(i32::MAX)
                .saturating_sub(self.display_offset);
            let Ok(selection_col) = u16::try_from(self.logical_col(frame_row, frame_col)) else {
                return None;
            };
            return self
                .selection
                .contains_cell(selection_row, selection_col, is_wide, is_wide_continuation)
                .then_some(0);
        }
        // Entry anchors are already in frame rows (pane origin + its `display_offset`).
        let selection_row = i32::try_from(frame_row).unwrap_or(i32::MAX);
        let Ok(selection_col) = u16::try_from(self.logical_col(frame_row, frame_col)) else {
            return None;
        };
        self.selections.iter().position(|entry| {
            entry.clip.contains(frame_row, frame_col)
                && entry.selection.contains_cell(
                    selection_row,
                    selection_col,
                    is_wide,
                    is_wide_continuation,
                )
        })
    }

    /// Whether a frame-space cell is selected: the boolean face of [`Self::selection_hit`].
    #[must_use]
    #[inline]
    pub fn selection_contains_cell(
        &self,
        frame_row: usize,
        frame_col: usize,
        is_wide: bool,
        is_wide_continuation: bool,
    ) -> bool {
        self.selection_hit(frame_row, frame_col, is_wide, is_wide_continuation)
            .is_some()
    }

    /// The inclusive selected column span on one frame row, after the viewport projection and
    /// any split-composition clip.
    ///
    /// A scan window, not an equality key: with [`selections`](Self::selections) non-empty it
    /// is the hull over every pane's span, which suits the sparse-tail guards that re-test
    /// each cell; damage diffs use [`Self::selection_row_key`]. A wide glyph snapped whole
    /// may paint one column past `hi`, but that cell is always inside the materialized prefix,
    /// never the omitted tail cell the window exists to catch. The span is in logical columns;
    /// the BiDi reorder permutes only the materialized prefix, so past it logical and painted
    /// columns agree.
    #[must_use]
    #[inline]
    pub fn selection_row_span(&self, frame_row: usize) -> Option<(u16, u16)> {
        self.selection_row_keys(frame_row)
            .map(|key| (key.lo, key.hi))
            .reduce(|(a_lo, a_hi), (b_lo, b_hi)| (a_lo.min(b_lo), a_hi.max(b_hi)))
    }

    /// The lossless per-row selection damage key: one [`SelectionRowKey`] per selection that
    /// reaches `frame_row`, with its colours. Damage diffs compare this; unlike
    /// [`Self::selection_row_span`] it never merges panes. Writes into `out` (cleared first).
    #[inline]
    pub fn selection_row_key(&self, frame_row: usize, out: &mut Vec<SelectionRowKey>) {
        out.clear();
        out.extend(self.selection_row_keys(frame_row));
    }

    /// Every selection's [`SelectionRowKey`] on `frame_row`, in list order (the scalar
    /// selection alone when [`selections`](Self::selections) is empty): the one walk behind
    /// the row span and the row key.
    #[inline]
    fn selection_row_keys(&self, frame_row: usize) -> impl Iterator<Item = SelectionRowKey> + '_ {
        let scalar = if self.selections.is_empty() {
            selection_row_span_of(
                &self.selection,
                self.selection_clip,
                self.display_offset,
                frame_row,
            )
            .map(|(lo, hi)| SelectionRowKey {
                lo,
                hi,
                bg: self.selection_bg,
                fg: self.selection_fg,
                inactive: false,
            })
        } else {
            None
        };
        scalar
            .into_iter()
            .chain(self.selections.iter().filter_map(move |entry| {
                selection_row_span_of(&entry.selection, Some(entry.clip), 0, frame_row).map(
                    |(lo, hi)| SelectionRowKey {
                        lo,
                        hi,
                        bg: entry.bg,
                        fg: entry.fg,
                        inactive: entry.inactive,
                    },
                )
            }))
    }

    /// Drop every host-owned visual effect layer, leaving only terminal cell
    /// content (grid, cursor position/colour, selection, images): the cursor
    /// trail and shape override, cursor glow and halos, under-glyph flame light,
    /// fire patches, charred-ink and contrast-halo overrides, sparkle-word
    /// decorations, animated ink, cat sprites, supernova light, rain (atlas
    /// `Arc`s nulled) and the fx layers with their occluders and prefetch. Used
    /// by the `image plain` introspection capture.
    pub fn clear_overlays(&mut self) {
        self.cursor_effect_style_override = None;
        self.cursor_trail.clear();
        self.cursor_glow_add.clear();
        self.glow_halo.clear();
        self.glow_under.clear();
        self.fire_patch.clear();
        // These records describe the vectors above: reset, they fail closed to exact vector
        // comparison until a producer rebuilds them.
        self.cursor_glow_add_damage = EffectStreamDamage::default();
        self.glow_halo_damage = EffectStreamDamage::default();
        self.glow_under_damage = EffectStreamDamage::default();
        self.fire_patch_damage = EffectStreamDamage::default();
        self.cursor_fill_override = None;
        self.word_decorations.clear();
        self.ink.clear();
        self.char_fg.clear();
        self.fire_halo.clear();
        self.cat_quads.clear();
        self.cat_atlas = None;
        self.free_sprites.clear();
        self.free_atlas = None;
        self.nova_add.clear();
        self.rain_quads.clear();
        self.rain_atlas = None;
        self.rain_add.clear();
        self.fx_layers.clear();
        self.fx_occluders.clear();
        self.fx_prefetch.clear();
        self.wallpaper = None;
    }

    /// The emoji grapheme-cluster string at viewport cell `(row, col)`, if the frame captured
    /// one (a ZWJ / skin-tone / keycap sequence). The per-row list is sorted by column, one
    /// entry per column (re-sorted after a BiDi reorder or pane compose), so this
    /// binary-searches: the GPU atlas loops stay O(cols·log cols) on a dense row.
    #[must_use]
    #[inline]
    pub fn cluster_at(&self, row: usize, col: usize) -> Option<&str> {
        let row = self.clusters.get(row)?;
        row.binary_search_by_key(&col, |(c, _)| *c)
            .ok()
            .map(|i| row[i].1.as_ref())
    }

    /// The combining marks to overlay at viewport cell `(row, col)`, if any (binary search,
    /// as [`cluster_at`](Self::cluster_at)).
    #[must_use]
    #[inline]
    pub fn combining_at(&self, row: usize, col: usize) -> Option<&[char]> {
        let row = self.combining.get(row)?;
        row.binary_search_by_key(&col, |(c, _)| *c)
            .ok()
            .map(|i| row[i].1.as_ref())
    }

    /// The live default background governing viewport cell `(row, col)`: the owning pane's
    /// [`DefaultBgSpan`] on a composed frame, else (missing rows, divider gaps, malformed
    /// spans) the frame-wide [`default_bg`](Self::default_bg). May be `COLOR_UNSET`, which
    /// renderers resolve against their theme.
    #[must_use]
    #[inline]
    pub fn default_bg_at(&self, row: usize, col: usize) -> u32 {
        let Some(spans) = self.default_bg_spans.get(row) else {
            return self.default_bg;
        };
        let i = spans.partition_point(|span| span.end_col <= col);
        spans
            .get(i)
            .filter(|span| {
                span.start_col < span.end_col
                    && span.end_col <= self.cols
                    && span.start_col <= col
                    && col < span.end_col
            })
            .map_or(self.default_bg, |span| span.default_bg)
    }

    /// The topmost inline-image layer covering viewport cell `(row, col)`, if
    /// any ([`images_at`](Self::images_at) has them all).
    #[must_use]
    #[inline]
    pub fn image_at(&self, row: usize, col: usize) -> Option<&aterm_grid::ImageRef> {
        self.images_at(row, col).last().map(|(_, image)| image)
    }

    /// Every inline-image layer covering viewport cell `(row, col)`, bottom
    /// first — the order both renderers draw them in.
    #[must_use]
    #[inline]
    pub fn images_at(&self, row: usize, col: usize) -> &[(usize, aterm_grid::ImageRef)] {
        self.images
            .get(row)
            .map_or(&[], |row| images_in_row_at(row, col))
    }

    /// The DEC line-size run governing (`row`, `col`): its line size and the column range
    /// `start_col..end_col` it spans, which both renderers use for placement and as the clip,
    /// so a double-width run never bleeds past its pane. An empty
    /// [`line_size_spans`](Self::line_size_spans) row yields the uniform run `0..cols`; an
    /// out-of-range column falls back to it rather than panicking.
    #[must_use]
    #[inline]
    pub fn line_size_run_at(&self, row: usize, col: usize) -> (LineSize, usize, usize) {
        let uniform = || {
            (
                self.line_sizes.get(row).copied().unwrap_or_default(),
                0,
                self.cols,
            )
        };
        let Some(spans) = self.line_size_spans.get(row) else {
            return uniform();
        };
        if spans.is_empty() {
            return uniform();
        }
        // A row with runs ignores `line_sizes[row]` (only a summary); a column no run claims
        // (a divider seam, padding) renders single-width.
        let gap = (LineSize::SingleWidth, 0, self.cols);
        // Ascending, non-overlapping: the last run starting at or before `col` is the only
        // candidate.
        match spans.binary_search_by_key(&col, |s| s.start_col) {
            Ok(i) => (spans[i].line_size, spans[i].start_col, spans[i].end_col),
            Err(0) => gap,
            Err(i) => {
                let s = &spans[i - 1];
                if col < s.end_col {
                    (s.line_size, s.start_col, s.end_col)
                } else {
                    gap
                }
            }
        }
    }

    /// Whether an image layer at (`row`,`col`) is drawn over the text (`z_index >= 0`, the
    /// default) and so hides the glyph; a Kitty `z < 0` image draws behind it.
    #[must_use]
    #[inline]
    pub fn image_hides_glyph_at(&self, row: usize, col: usize) -> bool {
        self.images_at(row, col)
            .iter()
            .any(|(_, image)| image.image.z_index >= 0)
    }

    /// Composite an IME preedit (the in-flight composition) into this frame at the cursor.
    /// Frame-only: no terminal state changes; the committed text arrives through the PTY echo.
    ///
    /// * drawn from the cursor cell in its colors with a single underline, no bold/italic;
    /// * wide characters take a lead cell plus a `wide` continuation;
    /// * truncated at the right edge (no wrap); the platform popover shows the full text;
    /// * zero-width characters merge into the preceding cell;
    /// * the cursor moves to the caret: `caret_byte` is the platform's byte offset (winit
    ///   `Ime::Preedit`'s cursor start); `None` or past-the-end parks it after the last
    ///   composed cell, clamped to the final column.
    ///
    /// `cjk` selects East-Asian-Ambiguous width, as the write path's `char_width` does. A
    /// snapshot row carries only its written prefix, so the overlay materializes the cells it
    /// paints, padding any gap with the live implicit blank (a clone of the last written cell
    /// would smear its background).
    pub fn overlay_ime_preedit(&mut self, preedit: &str, caret_byte: Option<usize>, cjk: bool) {
        if preedit.is_empty() || self.cursor_row >= self.rows {
            return;
        }
        let row = self.cursor_row;
        let cols = self.cols;
        // Resolved before the row borrow: `default_bg_at` reads `self`.
        let implicit = self.implicit_blank_at(row, self.cursor_col);
        let Some(cells) = self.cells.get_mut(row) else {
            return;
        };
        // The style seed: the cursor cell's colors; past the written prefix the live implicit
        // blank (a default `RenderCell` is black on black); with no resolved defaults, the
        // row's last real cell.
        let seed = cells
            .get(self.cursor_col)
            .copied()
            .or(implicit)
            .or_else(|| cells.last().copied())
            .unwrap_or_default();
        // The pad between the row's end and the cursor: the seed's colors, no decoration.
        let pad = RenderCell {
            ch: ' ',
            fg: seed.fg,
            bg: seed.bg,
            ..RenderCell::default()
        };
        let start_col = self.cursor_col;
        // A composition starting on a wide character's continuation takes its lead too, or the
        // renderer draws the old glyph over it. Read before the walk overwrites it.
        let straddled_lead =
            (start_col > 0 && cells.get(start_col).is_some_and(|c| c.wide)).then(|| start_col - 1);
        let (end_col, caret_col) = walk_ime_preedit(
            preedit,
            caret_byte,
            cjk,
            start_col,
            cols,
            |ch, col, width| {
                // Materialize the ragged tail through this character's cells; the walk
                // proves `col + width <= cols`.
                if cells.len() < col + width {
                    cells.resize(col + width, pad);
                }
                let lead = RenderCell {
                    ch,
                    fg: seed.fg,
                    bg: seed.bg,
                    underline: crate::terminal::UnderlineStyle::Single,
                    ..RenderCell::default()
                };
                if let Some(cell) = cells.get_mut(col) {
                    *cell = lead;
                }
                if width == 2
                    && let Some(cont) = cells.get_mut(col + 1)
                {
                    *cont = RenderCell {
                        ch: ' ',
                        wide: true,
                        ..lead
                    };
                }
            },
        );
        // A cell that held a multi-scalar grapheme also has it in `clusters` (and marks in
        // `combining`), which both renderers prefer over `ch`: drop those.
        if end_col > start_col {
            let mut lo = start_col;
            let mut hi = end_col;
            if let Some(lead) = straddled_lead
                && let Some(cell) = cells.get_mut(lead)
            {
                *cell = pad;
                lo = lead;
            }
            // A composition that stops inside a wide character strands its
            // continuation; blank it too.
            if cells.get(end_col).is_some_and(|c| c.wide) {
                cells[end_col] = pad;
                hi = end_col + 1;
            }
            let covered = lo..hi;
            if let Some(clusters) = self.clusters.get_mut(row) {
                clusters.retain(|(c, _)| !covered.contains(c));
            }
            if let Some(combining) = self.combining.get_mut(row) {
                combining.retain(|(c, _)| !covered.contains(c));
            }
        }
        self.cursor_col = caret_col;
    }

    /// Paint one predictive-echo ghost — `aterm-predict`'s guess that the program
    /// will echo `ch` at logical column `col` of frame row `row` — over this
    /// frame. Returns whether it was painted. The one painter every host's ghost
    /// pass goes through (`aterm-gui`'s single-pane and composed paths), and the
    /// one `aterm-predict`'s safety theorem runs to build the DISPLAYED view.
    ///
    /// Pure frame compositing, like [`Self::overlay_ime_preedit`]. What it may
    /// change is the guess's own cell and nothing else, which is what lets the
    /// predictor promise that a prediction never overwrites content outside the
    /// cells it predicted:
    /// * the cell is found the way reconciliation reads it, through
    ///   [`Self::visual_col`]: on a BiDi row already in visual order the logical
    ///   column is not the frame column, and painting the logical one paints some
    ///   other cell;
    /// * a guess at or past `cols` is dropped (a composed pane's blit does not clip
    ///   to its sub-rect, so it would bleed into the neighbour); a missing row too;
    /// * a guess on EITHER half of a wide glyph is refused. A guess is one cell
    ///   wide, and the wide glyph's two cells are drawn as one: painting its lead
    ///   leaves the continuation with nothing to draw (the right half vanishes),
    ///   and painting the continuation draws the guess under the lead's overflowing
    ///   bitmap. Either changes a cell the guess did not predict, so the guess
    ///   stays dark (it is still tracked; its echo, or its expiry, settles it);
    /// * past a trimmed row's end the row is extended with `blank` (the terminal's
    ///   implicit blank, not the last cell's SGR background);
    /// * the glyph drops inherited geometry and SGR and its fg is dimmed halfway
    ///   toward its bg (tentative, legible on any theme), and the cell's grapheme
    ///   cluster and combining marks go, since both renderers prefer them to `ch`
    ///   and would draw the old grapheme over the ghost.
    pub fn paint_prediction_ghost(
        &mut self,
        row: usize,
        col: usize,
        ch: char,
        cols: usize,
        blank: RenderCell,
    ) -> bool {
        if !self.prediction_ghost_paintable(row, col, cols) {
            return false;
        }
        let frame_col = self.visual_col(row, col);
        let Some(cells) = self.cells.get_mut(row) else {
            return false;
        };
        if cells.len() <= frame_col {
            cells.resize(frame_col + 1, blank);
        }
        let Some(cell) = cells.get_mut(frame_col) else {
            return false;
        };
        *cell = ghost_cell(*cell, ch);
        self.drop_marks(row, frame_col);
        true
    }

    /// Drop the grapheme cluster and combining marks of frame cell
    /// (`row`, `frame_col`): both renderers prefer them to the cell's `ch`, so a
    /// cell a prediction repaints would draw the old grapheme over the new one.
    #[inline]
    fn drop_marks(&mut self, row: usize, frame_col: usize) {
        if let Some(clusters) = self.clusters.get_mut(row) {
            clusters.retain(|&(c, _)| c != frame_col);
        }
        if let Some(combining) = self.combining.get_mut(row) {
            combining.retain(|&(c, _)| c != frame_col);
        }
    }

    /// Paint a predictive-echo SPAN, `aterm-predict`'s state chain on one row:
    /// each `(col, cell)` of `cells` (logical columns, strictly increasing) is
    /// painted as its [`SpanCell`] says, ALL OR NOTHING. Returns whether it
    /// painted; `false` leaves the frame untouched. An empty span paints nothing
    /// and is painted (a pure motion's state changes no cell).
    ///
    /// A chain is a STATE: a glyph typed, or erased, inside a line moves the
    /// text right of the cursor one cell, and a part of a state is a frame no
    /// program draws. So every cell is checked before any is written, and the
    /// span is refused when:
    /// * the row is missing, drawn in another order (BiDi: a logical column is
    ///   not its frame column, and moved text would land elsewhere) or at
    ///   another size (DEC double width), or a column is at or past `cols`;
    /// * the columns are not strictly increasing;
    /// * a painted cell would SPLIT a wide glyph of the frame: a continuation
    ///   painted without its lead, or a lead without its continuation (both
    ///   halves painted replace the glyph as a unit);
    /// * a moved cell's source is not a narrow cell without marks (a shift
    ///   moves the plain text the predictor saw there, never half a glyph), or
    ///   the moved cells reorder their sources (a shift keeps the text's order).
    ///
    /// Then every source is read before the cell it stands on is written: a
    /// chain can shift both ways (a glyph typed, an erase further along), and the
    /// moved cells are painted leftward-moving first, left to right, then
    /// rightward-moving, right to left, then the typed and vacated cells, which
    /// read nothing, and a moved cell whose source is its own column, which no
    /// pass before it writes. Because the moved cells keep their order, no
    /// source is written before it is read.
    ///
    /// * [`SpanCell::Typed`] is a ghost, as [`Self::paint_prediction_ghost`]
    ///   paints one: its glyph dimmed toward the cell's background, inherited
    ///   SGR and geometry dropped.
    /// * [`SpanCell::Moved`] is the source cell itself, in its own colours and
    ///   attributes (the text is certain, only its place is speculative), if it
    ///   holds the expected glyph; else that glyph as a typed ghost, so the
    ///   painted text is the span's whatever the frame holds.
    /// * [`SpanCell::Vacated`] is `blank`, the terminal's implicit blank.
    ///
    /// Past a trimmed row's end the row is extended with `blank`, as the ghost
    /// painter extends it, and a source past it reads as `blank`. Each painted
    /// cell's grapheme cluster and combining marks go.
    pub fn paint_prediction_span<I>(
        &mut self,
        row: usize,
        cells: I,
        cols: usize,
        blank: RenderCell,
    ) -> bool
    where
        I: IntoIterator<Item = (usize, SpanCell)>,
        I::IntoIter: Clone + DoubleEndedIterator,
    {
        let cells = cells.into_iter();
        if !self.prediction_span_paintable(row, cells.clone(), cols) {
            return false;
        }
        // The furthest column the span writes: the row grows to it first, with
        // the terminal's blank, so every write below lands inside it. (A source
        // past the row's end reads that blank, before or after the growth.)
        if let Some(furthest) = cells.clone().map(|(c, _)| c).max()
            && let Some(frame_row) = self.cells.get_mut(row)
            && frame_row.len() <= furthest
        {
            frame_row.resize(furthest + 1, blank);
        }
        let write = |input: &mut Self, col: usize, cell: RenderCell| {
            if let Some(target) = input.cells.get_mut(row).and_then(|r| r.get_mut(col)) {
                *target = cell;
            }
            input.drop_marks(row, col);
        };
        let moved = |input: &Self, col: usize, from: usize, ch: char| {
            let cell = input
                .cells
                .get(row)
                .and_then(|r| r.get(from))
                .copied()
                .unwrap_or(blank);
            if cell.ch == ch {
                cell
            } else {
                let at = input
                    .cells
                    .get(row)
                    .and_then(|r| r.get(col))
                    .copied()
                    .unwrap_or(blank);
                ghost_cell(at, ch)
            }
        };
        for (col, cell) in cells.clone() {
            if let SpanCell::Moved { from, ch } = cell
                && col < from
            {
                let painted = moved(self, col, from, ch);
                write(self, col, painted);
            }
        }
        for (col, cell) in cells.clone().rev() {
            if let SpanCell::Moved { from, ch } = cell
                && col > from
            {
                let painted = moved(self, col, from, ch);
                write(self, col, painted);
            }
        }
        // A moved cell whose source is its own column moves nothing: its source
        // is no other cell's, and none of the passes above writes it, so it is
        // read here as it stands, and painted as every moved cell is.
        for (col, cell) in cells {
            let painted = match cell {
                SpanCell::Typed(ch) => {
                    let at = self
                        .cells
                        .get(row)
                        .and_then(|r| r.get(col))
                        .copied()
                        .unwrap_or(blank);
                    ghost_cell(at, ch)
                }
                SpanCell::Vacated => blank,
                SpanCell::Moved { from, ch } if from == col => moved(self, col, from, ch),
                SpanCell::Moved { .. } => continue,
            };
            write(self, col, painted);
        }
        true
    }

    /// Whether [`Self::paint_prediction_span`] would paint `cells` on `row`:
    /// its refusal, asked without painting (see there for every condition).
    #[must_use]
    pub fn prediction_span_paintable<I>(&self, row: usize, cells: I, cols: usize) -> bool
    where
        I: IntoIterator<Item = (usize, SpanCell)>,
    {
        let Some(frame_row) = self.cells.get(row) else {
            return false;
        };
        if !self.visual_to_logical_row(row).is_empty()
            || self
                .line_sizes
                .get(row)
                .is_some_and(|size| *size != LineSize::SingleWidth)
        {
            return false;
        }
        let continuation = |c: usize| frame_row.get(c).is_some_and(|cell| cell.wide);
        let marked = |c: usize| {
            self.clusters
                .get(row)
                .is_some_and(|marks| marks.iter().any(|(at, _)| *at == c))
                || self
                    .combining
                    .get(row)
                    .is_some_and(|marks| marks.iter().any(|(at, _)| *at == c))
        };
        let mut previous: Option<usize> = None;
        let mut previous_source: Option<usize> = None;
        // A painted continuation needs its lead painted just before it; a
        // painted lead needs its continuation painted just after it, which the
        // next cell is checked for (`owed`).
        let mut owed: Option<usize> = None;
        for (col, cell) in cells {
            if col >= cols || previous.is_some_and(|p| col <= p) {
                return false;
            }
            if let Some(need) = owed.take()
                && need != col
            {
                return false;
            }
            if continuation(col) && previous != col.checked_sub(1) {
                return false;
            }
            if continuation(col + 1) {
                owed = Some(col + 1);
            }
            if let SpanCell::Moved { from, .. } = cell {
                if from >= cols
                    || continuation(from)
                    || continuation(from + 1)
                    || marked(from)
                    || previous_source.is_some_and(|p| from <= p)
                {
                    return false;
                }
                previous_source = Some(from);
            }
            previous = Some(col);
        }
        owed.is_none()
    }

    /// Whether [`Self::paint_prediction_ghost`] would paint a ghost at logical
    /// column `col` of frame row `row`: the column is inside `cols`, the row
    /// exists, and neither the cell nor the one after it is a wide glyph's
    /// continuation. The painter's own refusal, with nothing painted, for a host
    /// that must paint a set of ghosts ALL OR NONE (a state chain of predictive
    /// editing: a part of a state is a frame no program draws).
    #[must_use]
    #[inline]
    pub fn prediction_ghost_paintable(&self, row: usize, col: usize, cols: usize) -> bool {
        if col >= cols {
            return false;
        }
        let frame_col = self.visual_col(row, col);
        let Some(cells) = self.cells.get(row) else {
            return false;
        };
        let continuation = |c: usize| cells.get(c).is_some_and(|cell| cell.wide);
        !(continuation(frame_col) || continuation(frame_col + 1))
    }

    /// The frame's implicit blank at viewport cell (`row`, `col`): a space in
    /// the live default colours (OSC 10/11, their resets, DECSCNM, and on a
    /// composed split frame the per-pane
    /// [`default_bg_spans`](Self::default_bg_spans)), as the terminal paints an
    /// unwritten column.
    ///
    /// `None` when the producer resolved no defaults ([`COLOR_UNSET`]): the
    /// sentinel is not a colour, and masking its high byte would read as black.
    #[inline]
    fn implicit_blank_at(&self, row: usize, col: usize) -> Option<RenderCell> {
        let (fg, bg) = (self.default_fg, self.default_bg_at(row, col));
        if fg == COLOR_UNSET || bg == COLOR_UNSET {
            return None;
        }
        // `0x00RRGGBB` big-endian: a byte split, so nothing can truncate.
        let rgb = |c: u32| {
            let [_, r, g, b] = c.to_be_bytes();
            [r, g, b]
        };
        Some(RenderCell {
            ch: ' ',
            fg: rgb(fg),
            bg: rgb(bg),
            ..RenderCell::default()
        })
    }
}

/// The column walk an IME composition takes (widths under the ambiguous-width mode, right-edge
/// truncation, where the caret byte lands), shared by the overlay's ink (`emit` per character
/// that fits: scalar, column, width) and [`ime_preedit_caret_col`]. Returns
/// `(end_col, caret_col)`, the caret clamped to the row.
fn walk_ime_preedit(
    preedit: &str,
    caret_byte: Option<usize>,
    cjk: bool,
    start_col: usize,
    cols: usize,
    mut emit: impl FnMut(char, usize, usize),
) -> (usize, usize) {
    let mut col = start_col;
    let mut caret_col: Option<usize> = None;
    let caret = caret_byte.unwrap_or(preedit.len());
    for (byte, ch) in preedit.char_indices() {
        if caret_col.is_none() && byte >= caret {
            caret_col = Some(col);
        }
        let width = if cjk {
            aterm_grapheme::char_width_cjk(ch)
        } else {
            aterm_grapheme::char_width(ch)
        };
        // Zero-width characters (a combining mark off a dead key) merge into the preceding
        // cell, as the write path does.
        if width == 0 {
            continue;
        }
        if col + width > cols {
            break; // truncate: a wide char never straddles the edge
        }
        emit(ch, col, width);
        col += width;
    }
    (col, caret_col.unwrap_or(col).min(cols.saturating_sub(1)))
}

/// The composition's caret column: what [`RenderInput::overlay_ime_preedit`] leaves in
/// `cursor_col`, without painting. The host anchors the OS candidate window
/// (`set_ime_cursor_area`) here; the engine cursor still sits at the composition's start.
/// `start_col` and `cols` are in the overlay's coordinate space (pane-local for a split leaf).
#[must_use]
#[inline]
pub fn ime_preedit_caret_col(
    preedit: &str,
    caret_byte: Option<usize>,
    cjk: bool,
    start_col: usize,
    cols: usize,
) -> usize {
    walk_ime_preedit(preedit, caret_byte, cjk, start_col, cols, |_, _, _| {}).1
}

/// The entries of one row's sparse image list (`RenderInput::images[row]`,
/// ascending by column) that cover column `col`, bottom layer first.
#[must_use]
#[inline]
pub fn images_in_row_at(
    row: &[(usize, aterm_grid::ImageRef)],
    col: usize,
) -> &[(usize, aterm_grid::ImageRef)] {
    let start = row.partition_point(|(c, _)| *c < col);
    let len = row[start..].partition_point(|(c, _)| *c == col);
    &row[start..start + len]
}

#[cfg(test)]
mod ime_preedit_tests {
    use super::RenderInput;
    use crate::terminal::{RenderCell, UnderlineStyle};

    fn frame(rows: usize, cols: usize, cursor: (usize, usize)) -> RenderInput {
        let blank = RenderCell {
            ch: ' ',
            fg: [200, 200, 200],
            bg: [10, 10, 10],
            ..Default::default()
        };
        RenderInput {
            rows,
            cols,
            cells: vec![vec![blank; cols]; rows],
            cursor_row: cursor.0,
            cursor_col: cursor.1,
            cursor_visible: true,
            ..Default::default()
        }
    }

    #[test]
    fn ascii_preedit_lands_underlined_at_the_cursor_and_the_caret_trails() {
        let mut f = frame(4, 10, (1, 3));
        f.overlay_ime_preedit("ab", None, false);
        assert_eq!(f.cells[1][3].ch, 'a');
        assert_eq!(f.cells[1][4].ch, 'b');
        assert_eq!(f.cells[1][3].underline, UnderlineStyle::Single);
        assert_eq!(
            f.cells[1][3].fg,
            [200, 200, 200],
            "cursor cell's colors seed the style"
        );
        assert_eq!(f.cursor_col, 5, "caret parks after the composition");
        assert_eq!(
            f.cells[1][2].ch, ' ',
            "nothing left of the cursor is touched"
        );
    }

    #[test]
    fn the_platform_caret_offset_places_the_cursor_mid_composition() {
        let mut f = frame(2, 10, (0, 0));
        // Caret between 'a' and 'b' (byte offset 1).
        f.overlay_ime_preedit("ab", Some(1), false);
        assert_eq!(f.cursor_col, 1);
    }

    #[test]
    fn an_empty_preedit_is_byte_identical() {
        let mut f = frame(2, 10, (0, 4));
        let before = f.clone();
        f.overlay_ime_preedit("", None, false);
        assert_eq!(f, before);
    }

    #[test]
    fn zero_width_combining_marks_advance_nothing() {
        let mut f = frame(2, 10, (0, 0));
        // 'e' + COMBINING ACUTE (U+0301, width 0): one visible column.
        f.overlay_ime_preedit("e\u{0301}", None, false);
        assert_eq!(f.cells[0][0].ch, 'e');
        assert_eq!(f.cursor_col, 1);
    }
}

/// Overlay tests on real `Terminal` extractions, whose rows are ragged: a row holds only its
/// written prefix, and at a shell prompt the cursor sits at its end.
#[cfg(test)]
mod ime_preedit_ragged_row_tests {
    use super::{COLOR_UNSET, RenderInput};
    use crate::terminal::{RenderCell, Rgb, Terminal, UnderlineStyle};

    /// A terminal at a prompt: `$ ` written, cursor parked after it, with host-configured
    /// colors (resolved `default_fg` / `default_bg`, not `COLOR_UNSET`).
    fn prompt_frame(rows: u16, cols: u16) -> RenderInput {
        let mut term = Terminal::new(rows, cols);
        term.set_default_foreground(Rgb::new(0xC0, 0xC5, 0xCE));
        term.set_default_background(Rgb::new(0x1B, 0x1D, 0x1E));
        term.process(b"$ ");
        let mut f = RenderInput::empty();
        term.cell_frame_into(&mut f, rows.into(), cols.into());
        f
    }

    /// Composing at a prompt paints the composition on the ragged row, not
    /// only a cursor move.
    #[test]
    fn a_composition_at_the_prompt_caret_is_painted_not_swallowed() {
        let mut f = prompt_frame(4, 20);
        f.overlay_ime_preedit("日本", None, false);
        let row: String = f.cells[0].iter().map(|c| c.ch).collect();
        assert_eq!(row, "$ 日 本 ", "the composition lands right of the prompt");
        assert_eq!(f.cells[0][2].ch, '日');
        assert!(
            f.cells[0][3].wide,
            "the wide continuation is materialized too"
        );
        assert_eq!(f.cells[0][4].ch, '本');
        assert!(f.cells[0][5].wide);
        assert_eq!(f.cursor_col, 6, "and the caret trails the text it can see");
    }

    /// The caret never advances over cells the composition left unwritten.
    #[test]
    fn the_caret_never_advances_past_what_was_actually_drawn() {
        let mut f = prompt_frame(4, 20);
        f.overlay_ime_preedit("にほん", None, false);
        assert_eq!(f.cursor_col, 8);
        assert!(
            f.cells[0].len() >= f.cursor_col,
            "every column the caret crossed is materialized: len={} caret={}",
            f.cells[0].len(),
            f.cursor_col
        );
    }

    /// Materialized cells take the frame's live implicit blank as their style
    /// seed, not a default `RenderCell` (black on black).
    #[test]
    fn the_style_seed_falls_back_to_the_live_default_colors() {
        let mut f = prompt_frame(4, 20);
        f.overlay_ime_preedit("ab", None, false);
        assert_eq!(
            f.cells[0][2].fg,
            [0xC0, 0xC5, 0xCE],
            "live default foreground"
        );
        assert_eq!(
            f.cells[0][2].bg,
            [0x1B, 0x1D, 0x1E],
            "live default background"
        );
        assert_ne!(
            f.cells[0][2].fg, f.cells[0][2].bg,
            "composing text must not be painted onto its own colour"
        );
        assert_eq!(f.cells[0][2].underline, UnderlineStyle::Single);
    }

    /// A producer that resolved no defaults (a pristine `Terminal::new`, a standalone
    /// renderer harness) still gets legible composition: the row's own last cell seeds the
    /// colors rather than a black placeholder.
    #[test]
    fn an_unresolved_frame_seeds_from_the_rows_last_real_cell() {
        let mut f = RenderInput {
            rows: 2,
            cols: 10,
            cells: vec![
                vec![RenderCell {
                    ch: '$',
                    fg: [11, 22, 33],
                    bg: [44, 55, 66],
                    ..Default::default()
                }],
                Vec::new(),
            ],
            cursor_row: 0,
            cursor_col: 1,
            ..Default::default()
        };
        assert_eq!(f.default_bg, COLOR_UNSET, "the unresolved producer case");
        f.overlay_ime_preedit("x", None, false);
        assert_eq!(f.cells[0][1].ch, 'x');
        assert_eq!(f.cells[0][1].fg, [11, 22, 33]);
        assert_eq!(f.cells[0][1].bg, [44, 55, 66]);
    }

    /// A cursor parked right of the row's end leaves a gap, padded with the
    /// live implicit blank: no glyph and no underline.
    #[test]
    fn the_gap_before_the_caret_is_padded_with_undecorated_blanks() {
        let mut f = prompt_frame(4, 20);
        f.cursor_col = 5; // row is 2 cells long; columns 2..5 do not exist yet
        f.overlay_ime_preedit("z", None, false);
        assert_eq!(f.cells[0].len(), 6);
        for col in 2..5 {
            assert_eq!(f.cells[0][col].ch, ' ', "col {col} is a blank, not a glyph");
            assert_eq!(
                f.cells[0][col].underline,
                UnderlineStyle::None,
                "col {col} is padding, not composition"
            );
            assert_eq!(f.cells[0][col].bg, [0x1B, 0x1D, 0x1E], "in the live ground");
        }
        assert_eq!(f.cells[0][5].ch, 'z');
        assert_eq!(f.cells[0][5].underline, UnderlineStyle::Single);
    }

    /// Truncation must not grow the row. A composition that does not fit stops
    /// at the edge, and the cells it never reached stay unmaterialized.
    #[test]
    fn a_truncated_composition_materializes_only_what_it_drew() {
        let mut f = prompt_frame(2, 5);
        // Columns 2-3 hold '日'; '本' would need 4-5 and the row is 5 wide.
        f.overlay_ime_preedit("日本", None, false);
        assert_eq!(f.cells[0].len(), 4, "the straddling char extended nothing");
        assert_eq!(f.cells[0][2].ch, '日');
        assert_eq!(f.cursor_col, 4, "the caret clamps inside the row");
    }

    /// Composing over a multi-scalar grapheme drops its `clusters` entry, which
    /// both renderers would otherwise paint over the composition.
    #[test]
    fn composing_over_a_grapheme_drops_its_stale_cluster() {
        let mut term = Terminal::new(2, 10);
        // Regional indicators J+P: one two-column flag cluster at column 0.
        term.process("\u{1F1EF}\u{1F1F5}xy\r".as_bytes());
        let mut f = RenderInput::empty();
        term.cell_frame_into(&mut f, 2, 10);
        assert_eq!(f.cluster_at(0, 0), Some("\u{1F1EF}\u{1F1F5}"));
        assert_eq!(
            (f.cursor_row, f.cursor_col),
            (0, 0),
            "CR parked at the flag"
        );

        f.overlay_ime_preedit("a", None, false);
        assert_eq!(f.cells[0][0].ch, 'a');
        assert_eq!(
            f.cluster_at(0, 0),
            None,
            "the covered cell's grapheme goes with it"
        );
        assert_eq!(f.cluster_at(0, 2), None, "and nothing else is disturbed");
    }
}

/// Edge cases of row materialization, driven from real `Terminal` extractions.
#[cfg(test)]
mod ime_preedit_neighbour_tests {
    use super::RenderInput;
    use crate::terminal::{Rgb, Terminal, UnderlineStyle};

    /// A host-configured terminal (resolved defaults, as every windowed frame
    /// has) after `bytes`.
    fn painted_frame(rows: u16, cols: u16, bytes: &[u8]) -> RenderInput {
        let mut term = Terminal::new(rows, cols);
        term.set_default_foreground(Rgb::new(0xC0, 0xC5, 0xCE));
        term.set_default_background(Rgb::new(0x1B, 0x1D, 0x1E));
        term.process(bytes);
        let mut f = RenderInput::empty();
        term.cell_frame_into(&mut f, rows.into(), cols.into());
        f
    }

    /// A full row: `resize` never fires, and the seed comes from the existing
    /// cursor cell, not the live default.
    #[test]
    fn a_full_row_keeps_its_length_and_still_seeds_from_its_own_cursor_cell() {
        let mut f = painted_frame(2, 10, b"abc\x1b[31;44mdefghij\r\x1b[4G");
        assert_eq!(f.cells[0].len(), 10, "fixture: the row is FULL");
        assert_eq!((f.cursor_row, f.cursor_col), (0, 3));
        let (fg, bg) = (f.cells[0][3].fg, f.cells[0][3].bg);
        assert_ne!(
            (fg, bg),
            ([0xC0, 0xC5, 0xCE], [0x1B, 0x1D, 0x1E]),
            "fixture: the cursor cell must NOT be wearing the frame defaults, or \
             this cannot tell the two seeds apart"
        );

        f.overlay_ime_preedit("XY", None, false);
        assert_eq!(f.cells[0].len(), 10, "materializing never grows a full row");
        assert_eq!(f.cells[0][3].ch, 'X');
        assert_eq!(f.cells[0][4].ch, 'Y');
        assert_eq!(
            (f.cells[0][3].fg, f.cells[0][3].bg),
            (fg, bg),
            "the cursor cell still wins the seed"
        );
        assert_eq!(f.cells[0][5].ch, 'f', "nothing right of it moved");
        assert_eq!(f.cursor_col, 5);
    }

    /// Column 0 of a row with no written prefix (`cells[row]` empty): a wrapped prompt's second
    /// line, or any line of a cleared screen.
    #[test]
    fn a_composition_at_column_zero_of_an_unwritten_row_is_painted() {
        let mut f = painted_frame(3, 20, b"$ \r\n");
        assert!(
            f.cells[1].is_empty(),
            "fixture: the row has no written prefix at all"
        );
        assert_eq!((f.cursor_row, f.cursor_col), (1, 0));

        f.overlay_ime_preedit("日", None, false);
        assert_eq!(f.cells[1].len(), 2);
        assert_eq!(f.cells[1][0].ch, '日');
        assert!(f.cells[1][1].wide, "the continuation is materialized too");
        assert_eq!(f.cells[1][0].underline, UnderlineStyle::Single);
        assert_eq!(
            f.cells[1][0].bg,
            [0x1B, 0x1D, 0x1E],
            "an empty row has no last cell — the live default is the only seed"
        );
        assert_eq!(f.cursor_col, 2);
    }

    /// A wide char with exactly one column left: nothing is painted or
    /// materialized; only the caret is clamped.
    #[test]
    fn a_wide_composition_with_one_column_left_paints_nothing_and_grows_nothing() {
        let mut f = painted_frame(2, 8, b"$ ");
        f.cursor_col = 7;
        let before = f.clone();
        f.overlay_ime_preedit("日", None, false);
        assert_eq!(f.cells, before.cells, "not one cell was touched");
        assert_eq!(f.cursor_col, 7, "and the caret stays on the cell it was on");
    }

    /// The platform caret at byte 0: the composition is painted in full and the
    /// cursor sits at its start.
    #[test]
    fn the_platform_caret_at_offset_zero_anchors_at_the_composition_start() {
        let mut f = painted_frame(2, 20, b"$ ");
        f.overlay_ime_preedit("にほん", Some(0), false);
        assert_eq!(f.cursor_col, 2, "the composition's first column");
        assert_eq!(f.cells[0][2].ch, 'に', "and all of it is still painted");
        assert_eq!(f.cells[0][6].ch, 'ん');
    }

    /// The `cjk` argument (DECSET 8840 / `ambiguous_width_double`) reaches the width call, so
    /// the composition occupies the columns the committed text would.
    #[test]
    fn the_ambiguous_width_flag_widens_the_composition_it_is_given() {
        let mut narrow = painted_frame(2, 20, b"$ ");
        narrow.overlay_ime_preedit("±", None, false);
        assert_eq!(
            narrow.cells[0].len(),
            3,
            "EA-Ambiguous is narrow by default"
        );
        assert!(!narrow.cells[0][2].wide);

        let mut wide = painted_frame(2, 20, b"$ ");
        wide.overlay_ime_preedit("±", None, true);
        assert_eq!(
            wide.cells[0].len(),
            4,
            "and double under the CJK width mode"
        );
        assert!(wide.cells[0][3].wide);
        assert_eq!(wide.cursor_col, 4);
    }

    /// Composing onto a wide character's continuation clears the whole
    /// character, as the write path would; a stranded continuation would be a
    /// blank in the old cell's background.
    #[test]
    fn composing_onto_a_wide_characters_lead_takes_its_continuation_too() {
        let mut f = painted_frame(2, 10, "日本\r".as_bytes());
        assert!(f.cells[0][1].wide, "fixture: 日 owns columns 0 and 1");
        assert_eq!(f.cursor_col, 0);

        f.overlay_ime_preedit("a", None, false);
        assert_eq!(f.cells[0][0].ch, 'a');
        assert!(!f.cells[0][1].wide, "the orphaned continuation is cleared");
        assert_eq!(f.cells[0][1].ch, ' ');
        assert_eq!(
            f.cells[0][1].underline,
            UnderlineStyle::None,
            "and it is not dressed as composition"
        );
        assert_eq!(f.cells[0][2].ch, '本', "the next character is untouched");
    }

    /// Composing onto a wide character's lead clears its continuation; a
    /// stranded lead would redraw the old glyph over the composition.
    #[test]
    fn composing_onto_a_wide_characters_continuation_takes_its_lead_too() {
        let mut f = painted_frame(2, 10, "日本\r\x1b[2G".as_bytes());
        assert_eq!(
            f.cursor_col, 1,
            "fixture: the caret sits on 日's continuation"
        );

        f.overlay_ime_preedit("a", None, false);
        assert_eq!(f.cells[0][1].ch, 'a');
        assert_eq!(
            f.cells[0][0].ch, ' ',
            "the lead cannot stay — it would be redrawn over the composition"
        );
        assert!(!f.cells[0][0].wide);
        assert_eq!(f.cells[0][2].ch, '本', "and 本 is untouched");
    }

    /// The cluster drop follows the cells it clears: a multi-scalar wide
    /// grapheme composed onto from its continuation column has its `clusters`
    /// entry at the lead, one column left of where the composition starts.
    #[test]
    fn composing_onto_a_graphemes_continuation_drops_the_cluster_at_its_lead() {
        let mut term = Terminal::new(2, 10);
        term.process("\u{1F1EF}\u{1F1F5}xy\r\u{1b}[2G".as_bytes());
        let mut f = RenderInput::empty();
        term.cell_frame_into(&mut f, 2, 10);
        assert_eq!(f.cluster_at(0, 0), Some("\u{1F1EF}\u{1F1F5}"));
        assert_eq!(
            f.cursor_col, 1,
            "fixture: caret on the flag's second column"
        );

        f.overlay_ime_preedit("a", None, false);
        assert_eq!(f.cells[0][1].ch, 'a');
        assert_eq!(
            f.cluster_at(0, 0),
            None,
            "the cleared lead's grapheme goes with it"
        );
    }
}

/// [`ime_preedit_caret_col`] (the candidate-window anchor) and
/// [`RenderInput::overlay_ime_preedit`] (the ink) agree on every shape the
/// shared `walk_ime_preedit` has a branch for.
#[cfg(test)]
mod ime_preedit_caret_query_tests {
    use super::{RenderInput, ime_preedit_caret_col};
    use crate::terminal::{Rgb, Terminal};

    fn prompt(cols: u16, cursor_col: usize) -> RenderInput {
        let mut term = Terminal::new(2, cols);
        term.set_default_foreground(Rgb::new(0xC0, 0xC5, 0xCE));
        term.set_default_background(Rgb::new(0x1B, 0x1D, 0x1E));
        term.process(b"$ ");
        let mut f = RenderInput::empty();
        term.cell_frame_into(&mut f, 2, cols.into());
        f.cursor_col = cursor_col;
        f
    }

    #[test]
    fn the_caret_query_returns_exactly_the_column_the_paint_lands_on() {
        // (preedit, caret byte, cjk, start column, frame cols)
        let cases: &[(&str, Option<usize>, bool, usize, u16)] = &[
            ("にほん", None, false, 2, 20),      // caret trails
            ("にほん", Some(0), false, 2, 20),   // caret at the start
            ("にほん", Some(3), false, 2, 20),   // caret between に and ほ
            ("ab", Some(1), false, 3, 20),       // narrow, mid-composition
            ("にほんご", None, false, 2, 8),     // truncated at the edge
            ("日", None, false, 7, 8),           // not one column fits
            ("e\u{0301}", None, false, 0, 20),   // a zero-width combining mark
            ("±", None, true, 2, 20),            // EA-Ambiguous, CJK mode
            ("±", None, false, 2, 20),           // …and narrow mode
            ("", None, false, 2, 20),            // no composition at all
            ("にほん", Some(999), false, 2, 20), // caret past the end
        ];
        for &(preedit, caret, cjk, start, cols) in cases {
            let mut f = prompt(cols, start);
            f.overlay_ime_preedit(preedit, caret, cjk);
            let queried = ime_preedit_caret_col(preedit, caret, cjk, start, usize::from(cols));
            assert_eq!(
                queried, f.cursor_col,
                "anchor and ink disagree for {preedit:?} caret={caret:?} cjk={cjk} \
                 start={start} cols={cols}"
            );
        }
    }
}

/// One cell of a predictive-echo span ([`RenderInput::paint_prediction_span`]):
/// what `aterm-predict`'s state chain shows there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanCell {
    /// A glyph a key typed, whose echo has not come: painted as a ghost.
    Typed(char),
    /// The glyph `ch` the frame shows at logical column `from` of the same row,
    /// moved here by a shift: painted as that cell, in its own style.
    Moved {
        /// The column the glyph stands on in the unpainted frame.
        from: usize,
        /// The glyph the predictor expects there.
        ch: char,
    },
    /// A cell an erase vacated: the terminal's blank.
    Vacated,
}

/// `cell` repainted as a predictive-echo ghost of `ch`: the glyph, its
/// foreground dimmed halfway toward the cell's background (tentative, legible
/// on any theme), its background kept, inherited SGR and geometry dropped. The
/// one ghost style ([`RenderInput::paint_prediction_ghost`],
/// [`RenderInput::paint_prediction_span`]).
#[inline]
const fn ghost_cell(cell: RenderCell, ch: char) -> RenderCell {
    let (fg, bg) = (cell.fg, cell.bg);
    RenderCell {
        ch,
        fg: [
            fg[0].midpoint(bg[0]),
            fg[1].midpoint(bg[1]),
            fg[2].midpoint(bg[2]),
        ],
        wide: false,
        emoji_presentation: false,
        text_presentation: false,
        bold: false,
        italic: false,
        underline: crate::terminal::UnderlineStyle::None,
        strikethrough: false,
        overline: false,
        underline_color: None,
        ..cell
    }
}

#[cfg(test)]
mod prediction_ghost_tests {
    use super::RenderInput;
    use crate::terminal::{RenderCell, Rgb, Terminal};

    fn frame(rows: u16, cols: u16, bytes: &[u8]) -> RenderInput {
        let mut term = Terminal::new(rows, cols);
        term.set_default_foreground(Rgb::new(0xC0, 0xC5, 0xCE));
        term.set_default_background(Rgb::new(0x1B, 0x1D, 0x1E));
        term.process(bytes);
        let mut f = RenderInput::empty();
        term.cell_frame_into(&mut f, rows.into(), cols.into());
        f
    }

    fn blank() -> RenderCell {
        RenderCell {
            fg: [0xC0, 0xC5, 0xCE],
            bg: [0x1B, 0x1D, 0x1E],
            ..RenderCell::default()
        }
    }

    /// Every cell but `(row, col)` is what it was.
    fn only_changed(before: &RenderInput, after: &RenderInput, at: Option<(usize, usize)>) {
        for (r, (b, a)) in before.cells.iter().zip(&after.cells).enumerate() {
            for c in 0..b.len().max(a.len()) {
                if Some((r, c)) != at {
                    assert_eq!(b.get(c), a.get(c), "({r}, {c}) changed");
                }
            }
        }
    }

    /// The ghost replaces its own cell (glyph, dimmed fg, no inherited SGR) and
    /// nothing else, over a styled row.
    #[test]
    fn a_ghost_changes_its_own_cell_and_no_other() {
        let before = frame(2, 10, b"ab\x1b[1;31mcde\x1b[0mfg");
        let mut after = before.clone();
        assert!(after.paint_prediction_ghost(0, 3, 'x', 10, blank()));
        only_changed(&before, &after, Some((0, 3)));
        let (old, new) = (before.cells[0][3], after.cells[0][3]);
        assert_eq!(new.ch, 'x');
        assert!(old.bold && !new.bold, "inherited SGR dropped");
        assert_eq!(new.bg, old.bg);
        assert_ne!(new.fg, old.fg, "dimmed toward the bg");
    }

    /// Either half of a wide glyph is refused: painting the lead leaves the
    /// continuation with nothing to draw, painting the continuation draws under the
    /// lead's bitmap. Both change a cell the guess never predicted.
    #[test]
    fn a_ghost_never_paints_over_half_of_a_wide_glyph() {
        let before = frame(1, 10, "ab日c".as_bytes());
        assert!(
            before.cells[0][3].wide && !before.cells[0][2].wide,
            "fixture: the wide glyph's lead at 2, its continuation at 3"
        );
        for col in [2, 3] {
            let mut after = before.clone();
            assert!(
                !after.paint_prediction_ghost(0, col, 'x', 10, blank()),
                "column {col} is half of the wide glyph"
            );
            only_changed(&before, &after, None);
        }
        let mut after = before.clone();
        assert!(
            after.paint_prediction_ghost(0, 1, 'x', 10, blank()),
            "control: the narrow cell beside it paints"
        );
        only_changed(&before, &after, Some((0, 1)));
    }

    /// On a BiDi row already in visual order the guess's logical column is found
    /// where reconciliation reads it ([`RenderInput::visual_col`]).
    #[test]
    fn a_ghost_lands_on_the_visual_cell_of_its_logical_column() {
        let mut before = frame(1, 10, b"cba");
        // Visual column v shows logical column map[v]: logical 0 is painted at 2.
        before.visual_to_logical = vec![vec![2, 1, 0]];
        let mut after = before.clone();
        assert!(after.paint_prediction_ghost(0, 0, 'x', 10, blank()));
        only_changed(&before, &after, Some((0, 2)));
        assert_eq!(after.cells[0][2].ch, 'x');
    }

    /// A cell's combining marks (and grapheme cluster) are drawn in preference to
    /// its `ch`, so they go with the glyph the ghost replaces — and only there.
    #[test]
    fn a_ghost_drops_its_cells_combining_marks_only() {
        let before = frame(1, 10, "e\u{301}xo\u{308}".as_bytes());
        let marked: Vec<usize> = before.combining[0].iter().map(|&(c, _)| c).collect();
        assert_eq!(marked, [0, 2], "fixture: marks on columns 0 and 2");
        let mut after = before.clone();
        assert!(after.paint_prediction_ghost(0, 0, 'q', 10, blank()));
        let marked: Vec<usize> = after.combining[0].iter().map(|&(c, _)| c).collect();
        assert_eq!(marked, [2], "column 2 keeps its mark");
    }

    /// The painter's refusal, asked without painting, is the painter's: on every
    /// column of a row holding a wide glyph, past `cols` and on a missing row,
    /// `prediction_ghost_paintable` answers what `paint_prediction_ghost` then
    /// does, and asking changes nothing.
    #[test]
    fn a_ghost_s_refusal_can_be_asked_without_painting() {
        let before = frame(2, 6, "ab日c".as_bytes());
        for (row, col) in (0..3).flat_map(|r| (0..8).map(move |c| (r, c))) {
            let asked = before.prediction_ghost_paintable(row, col, 6);
            let mut after = before.clone();
            assert_eq!(
                asked,
                after.paint_prediction_ghost(row, col, 'x', 6, blank()),
                "({row}, {col})"
            );
        }
        let probed = before.clone();
        let _ = probed.prediction_ghost_paintable(0, 2, 6);
        assert_eq!(probed.cells, before.cells, "asking paints nothing");
    }

    /// Past a trimmed row's end the row is padded with the terminal blank; at or
    /// past `cols`, or on a missing row, nothing is painted.
    #[test]
    fn a_ghost_pads_a_short_row_and_clips_to_the_frame() {
        let mut f = frame(2, 6, b"$ ");
        assert_eq!(f.cells[0].len(), 2, "fixture: a trimmed row");
        assert!(f.paint_prediction_ghost(0, 4, 'x', 6, blank()));
        assert_eq!(f.cells[0].len(), 5);
        assert_eq!((f.cells[0][2], f.cells[0][3]), (blank(), blank()));
        assert_eq!(f.cells[0][4].ch, 'x');
        let before = f.clone();
        assert!(!f.paint_prediction_ghost(0, 6, 'y', 6, blank()));
        assert!(!f.paint_prediction_ghost(9, 0, 'y', 6, blank()));
        assert_eq!(f.cells, before.cells);
    }

    use super::SpanCell::{Moved, Typed, Vacated};

    /// The frame's text on row 0, as a string.
    fn text(f: &RenderInput) -> String {
        f.cells[0].iter().map(|c| c.ch).collect()
    }

    /// A SHIFT: `x` typed before `c` of `ab` + bold red `cd`. The typed glyph is
    /// a ghost (dimmed, no SGR); the text it moved is the very cells it came
    /// from, bold and red; nothing outside the span changes.
    #[test]
    fn a_span_moves_text_in_its_own_style_and_types_its_glyph() {
        let before = frame(1, 10, b"ab\x1b[1;31mcd\x1b[0mef");
        let mut after = before.clone();
        let span = [
            (2, Typed('x')),
            (3, Moved { from: 2, ch: 'c' }),
            (4, Moved { from: 3, ch: 'd' }),
            (5, Moved { from: 4, ch: 'e' }),
            (6, Moved { from: 5, ch: 'f' }),
        ];
        assert!(after.paint_prediction_span(0, span, 10, blank()));
        assert_eq!(text(&after), "abxcdef");
        let (x, c) = (after.cells[0][2], after.cells[0][3]);
        assert!(
            !x.bold && x.fg != before.cells[0][2].fg,
            "the typed glyph: a ghost"
        );
        assert_eq!(c, before.cells[0][2], "`c` in its own style, moved");
        assert_eq!(after.cells[0][4], before.cells[0][3]);
        assert_eq!(
            after.cells[0][..2],
            before.cells[0][..2],
            "left of the span"
        );
    }

    /// Text moving both ways in one span (an insert, and an erase further on):
    /// every source is read before the cell it stands on is written. Rightward
    /// `b`, leftward `g` and `h`, a vacated cell after them.
    ///
    /// The text alone cannot tell the order: a moved cell whose source was
    /// written first falls back to a ghost of the glyph it expects, which reads
    /// the same. So every moved glyph is bold red, which a ghost never is, and
    /// each must land as the very cell it came from.
    #[test]
    fn a_span_reads_every_source_before_writing_it() {
        let before = frame(1, 12, b"a\x1b[1;31mb\x1b[0mcdef\x1b[1;31mgh\x1b[0mij");
        let mut f = before.clone();
        let span = [
            (1, Typed('x')),
            (2, Moved { from: 1, ch: 'b' }),
            (5, Moved { from: 6, ch: 'g' }),
            (6, Moved { from: 7, ch: 'h' }),
            (7, Vacated),
        ];
        assert!(f.paint_prediction_span(0, span, 12, blank()));
        assert_eq!(text(&f), "axbdegh ij");
        for (to, from) in [(2, 1), (5, 6), (6, 7)] {
            assert!(before.cells[0][from].bold, "fixture: ({from}) is bold");
            assert_eq!(
                f.cells[0][to], before.cells[0][from],
                "({to}) is the cell it came from, in its own style"
            );
        }
    }

    /// A moved cell whose source is its own column is painted as every moved
    /// cell is: itself, where it holds the glyph the span expects, else a ghost
    /// of that glyph. Nothing else changes.
    #[test]
    fn a_span_cell_moved_onto_itself_is_painted() {
        let before = frame(1, 10, b"ab\x1b[1;31mcd\x1b[0m");
        let mut same = before.clone();
        assert!(same.paint_prediction_span(0, [(2, Moved { from: 2, ch: 'c' })], 10, blank()));
        assert_eq!(same.cells, before.cells, "the cell it is");
        let mut other = before.clone();
        assert!(other.paint_prediction_span(0, [(2, Moved { from: 2, ch: 'z' })], 10, blank()));
        assert_eq!(text(&other), "abzd");
        assert!(!other.cells[0][2].bold, "a ghost of the expected glyph");
        only_changed(&before, &other, Some((0, 2)));
    }

    /// A span never SPLITS a wide glyph: painting either half alone is refused,
    /// and both halves together replace the glyph as a unit. A wide glyph is no
    /// moved cell's source. Asking (`prediction_span_paintable`) answers what
    /// painting does, and paints nothing.
    #[test]
    fn a_span_never_splits_a_wide_glyph() {
        let before = frame(1, 10, "ab日c".as_bytes());
        assert!(before.cells[0][3].wide, "fixture: the continuation at 3");
        for span in [
            vec![(2, Typed('x'))],
            vec![(3, Typed('x'))],
            vec![(1, Moved { from: 2, ch: '日' })],
            vec![(1, Typed('x')), (3, Typed('y'))],
        ] {
            let mut after = before.clone();
            assert!(
                !before.prediction_span_paintable(0, span.clone(), 10),
                "{span:?}"
            );
            assert!(!after.paint_prediction_span(0, span.clone(), 10, blank()));
            assert_eq!(after.cells, before.cells, "{span:?}: nothing painted");
        }
        let mut after = before.clone();
        let whole = [(2, Typed('x')), (3, Typed('y'))];
        assert!(before.prediction_span_paintable(0, whole, 10));
        assert!(after.paint_prediction_span(0, whole, 10, blank()));
        assert_eq!(text(&after), "abxyc");
        assert!(!after.cells[0][3].wide, "the glyph replaced as a unit");
    }

    /// A span is refused on a row drawn in another order (BiDi) or at another
    /// size, past `cols`, on a missing row, with columns out of order, and with
    /// moved cells that reorder their sources or move a marked cell.
    #[test]
    fn a_span_is_refused_where_its_cells_are_not_what_it_names() {
        let plain = frame(2, 10, b"abcd");
        let mut reordered = plain.clone();
        reordered.visual_to_logical = vec![vec![1, 0, 2, 3], Vec::new()];
        let mut double = plain.clone();
        double.line_sizes = vec![
            crate::grid::LineSize::DoubleWidth,
            crate::grid::LineSize::SingleWidth,
        ];
        let marked = frame(1, 10, "ae\u{301}cd".as_bytes());
        let ok = [(1, Moved { from: 2, ch: 'c' })];
        assert!(plain.prediction_span_paintable(0, ok, 10), "control");
        assert!(!reordered.prediction_span_paintable(0, ok, 10), "BiDi");
        assert!(!double.prediction_span_paintable(0, ok, 10), "double width");
        assert!(
            !plain.prediction_span_paintable(0, [(10, Typed('x'))], 10),
            "cols"
        );
        assert!(!plain.prediction_span_paintable(5, ok, 10), "missing row");
        assert!(
            !plain.prediction_span_paintable(0, [(2, Vacated), (1, Vacated)], 10),
            "out of order"
        );
        assert!(
            !plain.prediction_span_paintable(
                0,
                [
                    (0, Moved { from: 2, ch: 'c' }),
                    (1, Moved { from: 1, ch: 'b' })
                ],
                10
            ),
            "reordered sources"
        );
        assert!(
            !marked.prediction_span_paintable(0, [(0, Moved { from: 1, ch: 'e' })], 10),
            "a marked source"
        );
    }

    /// A moved cell whose source does not hold the glyph the span expects is
    /// painted as that glyph, a ghost: the painted text is the span's whatever
    /// the frame holds. A painted cell's marks go; a short row is padded with
    /// the terminal's blank; an empty span paints nothing and is painted.
    #[test]
    fn a_span_paints_its_own_text_whatever_the_frame_holds() {
        let mut f = frame(1, 10, "ae\u{301}b".as_bytes());
        let span = [
            (1, Moved { from: 2, ch: 'z' }),
            (2, Typed('q')),
            (5, Vacated),
        ];
        assert!(f.paint_prediction_span(0, span, 10, blank()));
        assert_eq!(text(&f), "azq   ");
        assert!(f.combining[0].is_empty(), "the mark on `e` went with it");
        assert_eq!(f.cells[0][3], blank(), "padded");
        let before = f.clone();
        assert!(f.paint_prediction_span(0, [], 10, blank()));
        assert_eq!(f.cells, before.cells);
    }
}

#[cfg(test)]
mod line_size_run_tests {
    use super::{LineSizeSpan, RenderInput};
    use crate::grid::LineSize;

    fn input(rows: usize, cols: usize) -> RenderInput {
        let mut i = RenderInput::empty();
        i.rows = rows;
        i.cols = cols;
        i.line_sizes = vec![LineSize::SingleWidth; rows];
        i.line_size_spans = vec![Vec::new(); rows];
        i
    }

    /// A uniform row (every single-pane frame) yields the row's own line size
    /// over the whole grid.
    #[test]
    fn empty_spans_yield_the_row_line_size_over_the_whole_row() {
        let mut i = input(2, 10);
        i.line_sizes[1] = LineSize::DoubleWidth;
        assert_eq!(i.line_size_run_at(0, 0), (LineSize::SingleWidth, 0, 10));
        assert_eq!(i.line_size_run_at(0, 9), (LineSize::SingleWidth, 0, 10));
        assert_eq!(i.line_size_run_at(1, 4), (LineSize::DoubleWidth, 0, 10));
    }

    /// Boundaries: `start_col` is inclusive and `end_col` exclusive, and the
    /// binary search must land correctly on a run's first column, its last, and
    /// the gap between two runs.
    #[test]
    fn run_boundaries_are_half_open_and_gaps_fall_back() {
        let mut i = input(1, 12);
        i.line_size_spans[0] = vec![
            LineSizeSpan {
                start_col: 0,
                end_col: 3,
                line_size: LineSize::DoubleHeightTop,
            },
            LineSizeSpan {
                start_col: 8,
                end_col: 12,
                line_size: LineSize::DoubleWidth,
            },
        ];
        assert_eq!(i.line_size_run_at(0, 0).0, LineSize::DoubleHeightTop);
        assert_eq!(i.line_size_run_at(0, 2).0, LineSize::DoubleHeightTop);
        // 3..8 is the gap between the two panes.
        assert_eq!(i.line_size_run_at(0, 3), (LineSize::SingleWidth, 0, 12));
        assert_eq!(i.line_size_run_at(0, 7), (LineSize::SingleWidth, 0, 12));
        assert_eq!(i.line_size_run_at(0, 8).0, LineSize::DoubleWidth);
        assert_eq!(i.line_size_run_at(0, 11).0, LineSize::DoubleWidth);
    }

    /// On a row with runs, `line_sizes[row]` is only a summary; placement
    /// ignores it, so a divider seam between panes renders single-width even
    /// when the summary is double-width.
    #[test]
    fn a_spanned_row_never_inherits_the_row_level_summary() {
        let mut i = input(1, 11);
        // The compositor sets both: the run (authoritative) and the summary.
        i.line_sizes[0] = LineSize::DoubleWidth;
        i.line_size_spans[0] = vec![LineSizeSpan {
            start_col: 6,
            end_col: 11,
            line_size: LineSize::DoubleWidth,
        }];
        for c in 0..6 {
            assert_eq!(
                i.line_size_run_at(0, c),
                (LineSize::SingleWidth, 0, 11),
                "column {c} is unclaimed composite space, not the summary's width"
            );
        }
        assert_eq!(i.line_size_run_at(0, 6), (LineSize::DoubleWidth, 6, 11));
    }

    /// The renderers index this from clamped-framebuffer loops, so an
    /// out-of-range row or column must fall back, never panic.
    #[test]
    fn out_of_range_lookups_fall_back_instead_of_panicking() {
        let mut i = input(1, 4);
        i.line_size_spans[0] = vec![LineSizeSpan {
            start_col: 0,
            end_col: 2,
            line_size: LineSize::DoubleWidth,
        }];
        assert_eq!(i.line_size_run_at(0, 99), (LineSize::SingleWidth, 0, 4));
        assert_eq!(i.line_size_run_at(9, 0), (LineSize::SingleWidth, 0, 4));
    }
}

#[cfg(test)]
mod z_index_tests {
    use super::RenderInput;
    use aterm_grid::{ImageData, ImageFormat, ImageRef, ImageScaling, KittyPlacementTag};
    use std::num::NonZeroU32;
    use std::sync::Arc;

    /// [`super::images_eq`] matches derived equality with Kitty tags stripped; at the
    /// `RenderInput` level a re-transmit (fresh `Arc`s, same bytes) is equal and a one-byte
    /// payload change is not.
    #[test]
    fn images_eq_memo_matches_derived_equality() {
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 253) as u8).collect();
        let mk = |bytes: Vec<u8>, z: i32| {
            Arc::new(ImageData {
                bytes,
                format: ImageFormat::Png,
                cols: 3,
                rows: 2,
                z_index: z,
                band_lift_px: 0,
                cell_offset_px: (0, 0),
                scaling: ImageScaling::Fit,
                source_rect: None,
            })
        };
        let fill = |img: &Arc<ImageData>| -> Vec<Vec<(usize, ImageRef)>> {
            (0..2u16)
                .map(|r| {
                    (0..3u16)
                        .map(|c| {
                            (
                                usize::from(c),
                                ImageRef {
                                    image: img.clone(),
                                    cell_row: r,
                                    cell_col: c,
                                    kitty: None,
                                },
                            )
                        })
                        .collect()
                })
                .collect()
        };
        let base = mk(payload.clone(), 0);
        let a = fill(&base);
        let mut flipped = payload.clone();
        flipped[17] ^= 0x80;
        // The same cells stamped by a Kitty placement: identity, not pixels.
        let tagged = |serial: u32| {
            let mut t = a.clone();
            for (_, iref) in t.iter_mut().flatten() {
                iref.kitty = Some(KittyPlacementTag {
                    image_id: 1,
                    placement_id: 0,
                    image_serial: NonZeroU32::MIN,
                    serial: NonZeroU32::new(serial).unwrap(),
                });
            }
            t
        };
        let untagged = |rows: &[Vec<(usize, ImageRef)>]| {
            rows.iter()
                .map(|row| {
                    row.iter()
                        .map(|(c, iref)| {
                            (
                                *c,
                                ImageRef {
                                    kitty: None,
                                    ..iref.clone()
                                },
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let cases = [
            Vec::new(),                     // empty vs covered
            a.clone(),                      // same Arcs (ptr_eq path)
            fill(&mk(payload.clone(), 0)),  // re-transmit: distinct Arc, equal bytes
            fill(&mk(flipped, 0)),          // one payload byte differs
            fill(&mk(payload.clone(), -1)), // z-only difference
            tagged(7),                      // tag-only difference — EQUAL
            a[..1].to_vec(),                // row-count mismatch
        ];
        for (i, b) in cases.iter().enumerate() {
            assert_eq!(
                super::images_eq(&a, b),
                untagged(&a) == untagged(b),
                "case {i}: memoized equality must match the derived equality"
            );
        }
        // A Kitty re-put under a new serial is equal, though the derived
        // `ImageRef` equality (which sees the tag) says otherwise.
        assert!(super::images_eq(&tagged(7), &tagged(8)));
        assert_ne!(tagged(7), tagged(8), "the tag is part of derived equality");

        // RenderInput-level pin of both directions.
        let mut left = RenderInput::empty();
        left.images = a.clone();
        let mut right = RenderInput::empty();
        right.images = fill(&mk(payload.clone(), 0));
        assert_eq!(
            left, right,
            "a byte-identical re-transmit (fresh Arcs) must compare EQUAL"
        );
        let mut changed = payload;
        changed[4095] ^= 1;
        let mut unequal = RenderInput::empty();
        unequal.images = fill(&mk(changed, 0));
        assert_ne!(
            left, unequal,
            "a last-byte payload change must still compare UNEQUAL"
        );
    }

    fn image_ref(z: i32) -> ImageRef {
        ImageRef {
            image: Arc::new(ImageData {
                bytes: Vec::new(),
                format: ImageFormat::Png,
                cols: 1,
                rows: 1,
                z_index: z,
                band_lift_px: 0,
                cell_offset_px: (0, 0),
                scaling: ImageScaling::Fit,
                source_rect: None,
            }),
            cell_row: 0,
            cell_col: 0,
            kitty: None,
        }
    }

    #[test]
    fn image_hides_glyph_only_when_z_is_nonnegative() {
        let mut input = RenderInput::empty();
        // col 0: z=0 (over text, default) — hides; col 1: z=-1 (behind) — does not;
        // col 2: z=5 (over) — hides; col 3: no image.
        input.images = vec![vec![
            (0, image_ref(0)),
            (1, image_ref(-1)),
            (2, image_ref(5)),
        ]];
        assert!(
            input.image_hides_glyph_at(0, 0),
            "z=0 image hides the glyph"
        );
        assert!(
            !input.image_hides_glyph_at(0, 1),
            "z<0 image draws BEHIND text — glyph still paints"
        );
        assert!(
            input.image_hides_glyph_at(0, 2),
            "z>0 image hides the glyph"
        );
        assert!(
            !input.image_hides_glyph_at(0, 3),
            "no image at the column — nothing hides the glyph"
        );
    }

    #[test]
    fn per_column_accessors_binary_search_sparse_sorted_lists() {
        // The per-row (col, _) lists are sorted by column and binary-searched: populated
        // columns (0, 5, 100) resolve their entry, gaps return None.
        let mut input = RenderInput::empty();
        input.clusters = vec![vec![
            (0, "a".into()),
            (5, "flag".into()),
            (100, "zwj".into()),
        ]];
        input.combining = vec![vec![
            (0, vec!['\u{0301}'].into()),
            (5, vec!['\u{0308}'].into()),
            (100, vec!['\u{0327}'].into()),
        ]];
        input.images = vec![vec![(5, image_ref(0)), (100, image_ref(2))]];

        assert_eq!(input.cluster_at(0, 0), Some("a"));
        assert_eq!(input.cluster_at(0, 5), Some("flag"));
        assert_eq!(input.cluster_at(0, 100), Some("zwj"));
        assert_eq!(input.cluster_at(0, 4), None, "gap column has no cluster");
        assert_eq!(input.cluster_at(0, 101), None, "past-the-end column");

        assert_eq!(input.combining_at(0, 5), Some(['\u{0308}'].as_slice()));
        assert_eq!(input.combining_at(0, 100), Some(['\u{0327}'].as_slice()));
        assert_eq!(input.combining_at(0, 50), None, "gap column has no marks");

        assert_eq!(input.image_at(0, 5).map(|r| r.image.z_index), Some(0));
        assert_eq!(input.image_at(0, 100).map(|r| r.image.z_index), Some(2));
        assert!(input.image_at(0, 0).is_none(), "col 0 has no image");
        assert!(input.image_hides_glyph_at(0, 100), "z=2 image hides glyph");
    }
}

#[cfg(test)]
mod cursor_override_tests {
    use super::RenderInput;

    #[test]
    fn cursor_effect_style_override_is_content_cloned_and_cleared_with_overlays() {
        use crate::terminal::CursorStyle;

        let mut source = RenderInput::empty();
        source.cursor_style = CursorStyle::SteadyUnderline;
        source.cursor_effect_style_override = Some(CursorStyle::Bolt);

        let cloned = source.clone();
        assert_eq!(cloned.cursor_effect_style_override, Some(CursorStyle::Bolt));
        assert_eq!(cloned, source);

        let mut reused = RenderInput::empty();
        reused.clone_from(&source);
        assert_eq!(reused.cursor_effect_style_override, Some(CursorStyle::Bolt));
        assert_eq!(reused, source);

        let mut terminal_only = source.clone();
        terminal_only.cursor_effect_style_override = None;
        assert_ne!(
            source, terminal_only,
            "a stationary effect cursor shape changes rendered content"
        );

        source.clear_overlays();
        assert_eq!(source.cursor_effect_style_override, None);
        assert_eq!(
            source.cursor_style,
            CursorStyle::SteadyUnderline,
            "clearing host overlays must restore, not overwrite, DECSCUSR"
        );
        assert_eq!(source, terminal_only);
    }
}

#[cfg(test)]
mod default_bg_span_tests {
    use super::{DefaultBgSpan, PaneSelection, RenderInput, SelectionClip};

    #[test]
    fn lookup_uses_owning_pane_and_falls_back_for_gaps_or_malformed_spans() {
        let mut input = RenderInput::empty();
        input.rows = 2;
        input.cols = 10;
        input.default_bg = 0x0001_0203;
        input.default_bg_spans = vec![
            vec![
                DefaultBgSpan::new(0, 4, 0x0011_2233),
                DefaultBgSpan::new(5, 10, 0x0044_5566),
            ],
            vec![DefaultBgSpan::new(2, 99, 0x0077_8899)],
        ];

        assert_eq!(input.default_bg_at(0, 0), 0x0011_2233);
        assert_eq!(input.default_bg_at(0, 3), 0x0011_2233);
        assert_eq!(
            input.default_bg_at(0, 4),
            input.default_bg,
            "divider gap inherits the frame scalar"
        );
        assert_eq!(input.default_bg_at(0, 5), 0x0044_5566);
        assert_eq!(input.default_bg_at(0, 9), 0x0044_5566);
        assert_eq!(
            input.default_bg_at(1, 2),
            input.default_bg,
            "out-of-bounds producer spans fail closed to the scalar"
        );
        assert_eq!(
            input.default_bg_at(99, 0),
            input.default_bg,
            "missing rows retain the historical scalar path"
        );
    }

    #[test]
    fn spans_are_render_content_and_survive_both_clone_paths() {
        let mut source = RenderInput::empty();
        source.rows = 1;
        source.cols = 8;
        source.selection_bg = 0x0001_0203;
        source.selection_fg = 0x0004_0506;
        source.selection_clip = Some(SelectionClip::new(0, 1, 4, 8));
        source.selections = vec![PaneSelection {
            selection: crate::selection::TextSelection::new(),
            clip: SelectionClip::new(0, 1, 4, 8),
            bg: 0x0001_0203,
            fg: 0x0004_0506,
            inactive: true,
        }];
        source.default_bg_spans = vec![vec![
            DefaultBgSpan::new(0, 4, 0x0011_2233),
            DefaultBgSpan::new(4, 8, 0x0044_5566),
        ]];

        let cloned = source.clone();
        assert_eq!(cloned.default_bg_spans, source.default_bg_spans);
        assert_eq!(cloned.selection_bg, source.selection_bg);
        assert_eq!(cloned.selection_fg, source.selection_fg);
        assert_eq!(cloned.selection_clip, source.selection_clip);
        assert_eq!(cloned.selections, source.selections);
        assert_eq!(cloned, source);

        let mut reused = RenderInput::empty();
        reused.default_bg_spans = vec![vec![DefaultBgSpan::new(0, 99, 0x00aa_bbcc)]];
        reused.selections = vec![PaneSelection {
            selection: crate::selection::TextSelection::new(),
            clip: SelectionClip::new(9, 9, 9, 9),
            bg: 0,
            fg: 0,
            inactive: false,
        }];
        reused.clone_from(&source);
        assert_eq!(reused.default_bg_spans, source.default_bg_spans);
        assert_eq!(reused.selection_bg, source.selection_bg);
        assert_eq!(reused.selection_fg, source.selection_fg);
        assert_eq!(reused.selection_clip, source.selection_clip);
        assert_eq!(reused.selections, source.selections);
        assert_eq!(reused, source);

        let mut changed = source.clone();
        changed.default_bg_spans[0][0].default_bg ^= 0x0001_0101;
        assert_ne!(
            changed, source,
            "pane-local default provenance changes rendered pixels"
        );

        let mut changed = source.clone();
        changed.selection_bg ^= 0x0001_0101;
        assert_ne!(changed, source, "live selection bg changes rendered pixels");

        let mut changed = source.clone();
        changed.selection_fg ^= 0x0001_0101;
        assert_ne!(changed, source, "live selection fg changes rendered pixels");

        let mut changed = source.clone();
        changed.selection_clip = Some(SelectionClip::new(0, 1, 5, 8));
        assert_ne!(
            changed, source,
            "split selection bounds change rendered pixels"
        );

        // Every field of a per-pane entry paints, so every one moves `PartialEq`.
        let mut changed = source.clone();
        changed.selections[0].clip = SelectionClip::new(0, 1, 5, 8);
        assert_ne!(changed, source, "a pane's selection box changes pixels");

        let mut changed = source.clone();
        changed.selections[0].bg ^= 0x0001_0101;
        assert_ne!(changed, source, "a pane's live selection bg changes pixels");

        let mut changed = source.clone();
        changed.selections[0].fg ^= 0x0001_0101;
        assert_ne!(changed, source, "a pane's live selection fg changes pixels");

        let mut changed = source.clone();
        changed.selections[0].inactive = false;
        assert_ne!(
            changed, source,
            "a pane's focus selects a different band colour"
        );

        let mut changed = source.clone();
        changed.selections.clear();
        assert_ne!(
            changed, source,
            "dropping the list hands the frame back to the scalar authority"
        );
    }
}

#[cfg(test)]
mod selection_clip_tests {
    use super::{COLOR_UNSET, PaneSelection, RenderInput, SelectionClip, SelectionRowKey};
    use crate::selection::{SelectionSide, SelectionType, TextSelection};

    #[test]
    fn multiline_selection_is_confined_to_its_frame_rectangle() {
        let mut input = RenderInput::empty();
        input.rows = 5;
        input.cols = 14;
        input
            .selection
            .start_selection(1, 7, SelectionSide::Left, SelectionType::Simple);
        input.selection.update_selection(3, 8, SelectionSide::Right);
        input.selection.complete_selection();
        input.selection_clip = Some(SelectionClip::new(1, 4, 6, 11));

        assert!(input.selection_contains_cell(1, 7, false, false));
        assert!(input.selection_contains_cell(2, 6, false, false));
        assert!(input.selection_contains_cell(2, 10, false, false));
        assert!(input.selection_contains_cell(3, 8, false, false));

        assert!(
            !input.selection_contains_cell(2, 5, true, false),
            "wide-cell snapping may not escape through the clip's left edge"
        );
        assert!(
            !input.selection_contains_cell(2, 11, false, true),
            "a wide continuation may not escape through the clip's right edge"
        );
        assert!(
            !input.selection_contains_cell(0, 7, false, false),
            "rows above the focused pane stay unselected"
        );
        assert!(
            !input.selection_contains_cell(4, 7, false, false),
            "rows below the focused pane stay unselected"
        );
    }

    /// Two panes selected on one frame row: the predicate resolves each cell to
    /// the pane that owns it, the hull spans both (a scan window), and the
    /// per-entry key keeps them apart (an equality key).
    #[test]
    fn per_pane_selections_resolve_per_cell_while_the_span_is_a_hull() {
        let pane = |lo: u16, hi: u16, clip: SelectionClip, bg: u32| {
            let mut selection = TextSelection::new();
            selection.start_selection(1, lo, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(1, hi, SelectionSide::Right);
            selection.complete_selection();
            PaneSelection {
                selection,
                clip,
                bg,
                fg: COLOR_UNSET,
                inactive: bg != COLOR_UNSET,
            }
        };

        let mut input = RenderInput::empty();
        input.rows = 3;
        input.cols = 12;
        input.selections = vec![
            pane(0, 3, SelectionClip::new(1, 2, 0, 5), COLOR_UNSET),
            pane(7, 11, SelectionClip::new(1, 2, 7, 12), 0x0011_2233),
        ];

        assert_eq!(input.selection_hit(1, 2, false, false), Some(0));
        assert_eq!(input.selection_hit(1, 9, false, false), Some(1));
        assert_eq!(
            input.selection_hit(1, 6, false, false),
            None,
            "the divider gap between the two panes belongs to neither"
        );
        assert_eq!(
            input.selection_hit(0, 2, false, false),
            None,
            "a row outside both boxes is unselected"
        );

        assert_eq!(
            input.selection_row_span(1),
            Some((0, 11)),
            "the span is the HULL over both panes — a scan window, not a key"
        );
        let mut key = Vec::new();
        input.selection_row_key(1, &mut key);
        assert_eq!(
            key,
            vec![
                SelectionRowKey {
                    lo: 0,
                    hi: 3,
                    bg: COLOR_UNSET,
                    fg: COLOR_UNSET,
                    inactive: false,
                },
                SelectionRowKey {
                    lo: 7,
                    hi: 11,
                    bg: 0x0011_2233,
                    fg: COLOR_UNSET,
                    inactive: true,
                },
            ],
            "the key keeps the two panes' spans and colours separate"
        );
        input.selection_row_key(2, &mut key);
        assert!(key.is_empty(), "a row no pane reaches keys empty");
    }

    /// An empty list is the scalar path: the scalar selection is the whole
    /// authority, and `any_selection` agrees.
    #[test]
    fn an_empty_pane_list_leaves_the_scalar_path_untouched() {
        let mut input = RenderInput::empty();
        input.rows = 2;
        input.cols = 4;
        input
            .selection
            .start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
        input.selection.update_selection(0, 2, SelectionSide::Right);
        input.selection.complete_selection();

        assert!(input.any_selection());
        assert_eq!(input.selection_hit(0, 1, false, false), Some(0));
        assert_eq!(input.selection_row_span(0), Some((1, 2)));

        // A non-empty list replaces the scalar authority entirely.
        input.selections = vec![PaneSelection {
            selection: TextSelection::new(),
            clip: SelectionClip::new(0, 2, 0, 4),
            bg: COLOR_UNSET,
            fg: COLOR_UNSET,
            inactive: false,
        }];
        assert!(
            !input.selection_contains_cell(0, 1, false, false),
            "the list is the authority once it is non-empty"
        );
        assert!(input.any_selection(), "…and it still counts as a selection");

        // The list clause of `any_selection` alone: scalar empty, one live entry (an
        // unfocused pane). The scroll-blit veto, sparkle freeze and ligature break ask it.
        let mut list_only = RenderInput::empty();
        list_only.rows = 2;
        list_only.cols = 4;
        assert!(
            !list_only.any_selection(),
            "precondition: nothing selected at all"
        );
        let mut only = TextSelection::new();
        only.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        only.update_selection(0, 2, SelectionSide::Right);
        only.complete_selection();
        list_only.selections = vec![PaneSelection {
            selection: only,
            clip: SelectionClip::new(0, 2, 0, 4),
            bg: COLOR_UNSET,
            fg: COLOR_UNSET,
            inactive: true,
        }];
        assert!(
            !list_only.selection.has_selection(),
            "the scalar is empty — only the list carries a selection"
        );
        assert!(
            list_only.any_selection(),
            "an unfocused pane's selection must still gate the frame-wide passes"
        );
    }
}

#[cfg(test)]
mod rain_channel_tests {
    use super::{
        CharFg, FireHaloCell, FireMode, FirePatch, GlowQuad, HaloMode, RainHalo, RenderInput,
        SceneAtlas, SpriteQuad,
    };
    use std::sync::Arc;

    fn rain_atlas(version: u64, fill: u8) -> Arc<SceneAtlas> {
        Arc::new(SceneAtlas {
            width: 2,
            height: 2,
            rgba: vec![fill; 2 * 2 * 4],
            version,
        })
    }

    fn rain_quad(row: u16) -> SpriteQuad {
        SpriteQuad {
            row,
            x: 4,
            y: row * 16,
            w: 8,
            h: 16,
            ax: 0,
            ay: 0,
            aw: 8,
            ah: 16,
            tint: 0x0000_FF44,
            alpha: 96,
            flip_x: true,
        }
    }

    fn rain_halo(row: u16) -> RainHalo {
        RainHalo {
            row,
            x: 4,
            y: row * 16,
            w: 8,
            h: 16,
            color: 0x0020_6030,
            cx: 8,
            cy: row * 16 + 8,
            rx: 8,
            ry: 8,
            mode: HaloMode::Add,
        }
    }

    fn under_quad(row: u16) -> GlowQuad {
        GlowQuad {
            row,
            x: 2,
            y: row * 16 + 3,
            w: 12,
            h: 10,
            color: 0x0060_2008,
            // Additive light (see `GlowQuad::alpha`).
            alpha: 0,
            color2: 0x0060_2008,
            alpha2: 0,
        }
    }

    fn char_fg(row: u16, col: u16) -> CharFg {
        CharFg {
            row,
            col,
            fg: 0x0018_0C04,
        }
    }

    fn fire_halo(row: u16, col: u16) -> FireHaloCell {
        FireHaloCell {
            row,
            col,
            strength: 160,
        }
    }

    fn fire_patch(row: u16) -> FirePatch {
        FirePatch {
            row,
            x: 4,
            y: row * 16,
            w: 24,
            h: 16,
            base_y: (row + 1) * 16,
            peak_h: 40,
            phase: 2048,
            temp: 128,
            strength: 200,
            lean: -24,
            cov_cap: 90,
            cell_h: 16,
            mode: FireMode::Add,
        }
    }

    fn populated() -> RenderInput {
        let mut input = RenderInput::empty();
        input.rain_quads = vec![rain_quad(1), rain_quad(2)];
        input.rain_atlas = Some(rain_atlas(7, 0xFF));
        input.rain_add = vec![rain_halo(1)];
        input.glow_halo = vec![rain_halo(2)];
        input.glow_under = vec![under_quad(1), under_quad(2)];
        input.fire_patch = vec![fire_patch(1), fire_patch(2)];
        input.char_fg = vec![char_fg(1, 3), char_fg(1, 4)];
        input.fire_halo = vec![fire_halo(1, 3), fire_halo(1, 4)];
        input
    }

    /// `composed_fill_seq`: only `bless_composed_fill` arms it, and every other
    /// producer revokes it through the `snapshot_seq` discipline, without
    /// naming the field.
    #[test]
    fn the_composed_fill_blessing_is_revoked_by_every_other_producer() {
        let mut input = RenderInput::empty();
        assert!(
            !input.composed_fill_intact(),
            "a never-blessed snapshot must not claim to be a composite"
        );

        input.snapshot_seq = 41;
        input.bless_composed_fill();
        assert!(input.composed_fill_intact(), "the blessing did not take");

        // 1. A host cell write: one `snapshot_seq` bump; no mutator touches the token.
        let mut host = input.clone();
        host.snapshot_seq = host.snapshot_seq.wrapping_add(1);
        assert!(
            !host.composed_fill_intact(),
            "a host mutator's `snapshot_seq` bump must revoke the blessing — \
             this is the clause every unenumerated composite producer relies on"
        );

        // 2. A row rebuild, which every divider-grid fill opens with.
        let mut rebuilt = input.clone();
        rebuilt.invalidate_row_revisions();
        assert!(
            !rebuilt.composed_fill_intact(),
            "a row rebuild must revoke the blessing"
        );

        // 3. A host row prepend, blessed or not: it slides every composed row.
        for blessed in [false, true] {
            let mut spliced = input.clone();
            spliced.note_host_row_prepend(1, blessed);
            assert!(
                !spliced.composed_fill_intact(),
                "a {} row prepend must revoke the blessing — composed row `r` is \
                 no longer the row a ledger indexed",
                if blessed { "blessed" } else { "disowned" }
            );
        }

        // 4. The un-splice, which restores an engine fill, never a composite.
        let mut unspliced = RenderInput::empty();
        unspliced.rows = 1;
        unspliced.cells = vec![Vec::new()];
        unspliced.clusters = vec![Vec::new()];
        unspliced.combining = vec![Vec::new()];
        unspliced.row_rev = vec![0];
        unspliced.row_rev_lane = 9;
        unspliced.snapshot_seq = 7;
        unspliced.engine_fill_seq = 7;
        unspliced.shifted_fill_seq = 7;
        assert!(unspliced.host_prepend_blessing());
        unspliced.snapshot_seq = 8;
        unspliced.note_host_row_prepend(1, true);
        unspliced.cells.insert(0, Vec::new());
        unspliced.clusters.insert(0, Vec::new());
        unspliced.combining.insert(0, Vec::new());
        unspliced.rows = 2;
        unspliced.bless_composed_fill();
        let mut pool = Vec::new();
        assert!(unspliced.undo_host_row_prepend(1, &mut pool, 1));
        assert!(
            !unspliced.composed_fill_intact(),
            "an un-splice restores an engine fill and must not leave a composed \
             blessing standing over it"
        );
    }

    /// Atlases compare by identity (`Arc::as_ptr`), since baker versions replay
    /// across rebuilt engines: a fresh publish is unequal, and the same `Arc`
    /// re-presented stays equal.
    #[test]
    fn partial_eq_sees_atlas_identity() {
        let base = populated();

        let mut version_bumped = base.clone();
        version_bumped.rain_atlas = Some(rain_atlas(8, 0xFF));
        assert_ne!(base, version_bumped, "a fresh atlas publish must be seen");

        // A rebuilt engine replays version numbers with different texels;
        // identity, not version, catches it.
        let mut same_version_rebake = base.clone();
        same_version_rebake.rain_atlas = Some(rain_atlas(7, 0x00));
        assert_ne!(
            base, same_version_rebake,
            "a same-version DIFFERENT-Arc publish (rebuilt engine) compares UNEQUAL"
        );

        // The steady state: the same published snapshot re-presented.
        let mut same_arc = base.clone();
        same_arc.rain_atlas.clone_from(&base.rain_atlas);
        assert_eq!(
            base, same_arc,
            "re-presenting the same published Arc stays EQUAL (resting rain is free)"
        );
    }

    #[test]
    fn damage_cache_keeps_effect_revisions_and_drops_large_payloads() {
        let mut source = populated();
        source.rows = 4;
        source.refresh_cursor_effect_damage();

        let mut cached = RenderInput::empty();
        cached.cursor_glow_add = vec![under_quad(3); 64];
        cached.glow_halo = vec![rain_halo(3); 64];
        cached.glow_under = vec![under_quad(3); 64];
        cached.fire_patch = vec![fire_patch(3); 64];
        cached.clone_damage_cache_from(&source);

        assert!(cached.cursor_glow_add.is_empty());
        assert!(cached.glow_halo.is_empty());
        assert!(cached.glow_under.is_empty());
        assert!(cached.fire_patch.is_empty());
        assert!(cached.cursor_glow_add_damage.is_valid());
        assert!(cached.glow_halo_damage.is_valid());
        assert!(cached.glow_under_damage.is_valid());
        assert!(cached.fire_patch_damage.is_valid());

        let mut dirty = vec![false; source.rows];
        cached.glow_under_damage.mark_rows(&mut dirty);
        assert_eq!(dirty, [false, true, true, false]);
        assert_eq!(
            source
                .glow_under_damage
                .same_content(&cached.glow_under_damage),
            Some(true),
            "the cache carries the exact producer revision"
        );
    }

    /// The canonical record words cover every renderer-visible field (producer damage and the
    /// cursor scheduler both fingerprint with them).
    #[test]
    fn cursor_effect_damage_words_cover_every_visible_field() {
        macro_rules! assert_fields_change {
            ($name:literal, $base:ident, $($field:literal => $changed:expr),+ $(,)?) => {{
                let base_words = $base.damage_words();
                $(
                    assert_ne!(
                        ($changed).damage_words(),
                        base_words,
                        concat!($name, "::", $field)
                    );
                )+
            }};
        }

        let glow = under_quad(1);
        assert_fields_change!(
            "GlowQuad",
            glow,
            "row" => GlowQuad { row: 9, ..glow },
            "x" => GlowQuad { x: 9, ..glow },
            "y" => GlowQuad { y: 9, ..glow },
            "w" => GlowQuad { w: 9, ..glow },
            "h" => GlowQuad { h: 9, ..glow },
            "color" => GlowQuad { color: 0x65_4321, ..glow },
            "alpha" => GlowQuad { alpha: 9, ..glow },
            "color2" => GlowQuad { color2: 0x65_4321, ..glow },
            "alpha2" => GlowQuad { alpha2: 9, ..glow },
        );

        let rain = rain_halo(1);
        assert_fields_change!(
            "RainHalo",
            rain,
            "row" => RainHalo { row: 10, ..rain },
            "x" => RainHalo { x: 10, ..rain },
            "y" => RainHalo { y: 10, ..rain },
            "w" => RainHalo { w: 10, ..rain },
            "h" => RainHalo { h: 10, ..rain },
            "color" => RainHalo { color: 0x65_4321, ..rain },
            "cx" => RainHalo { cx: 10, ..rain },
            "cy" => RainHalo { cy: 10, ..rain },
            "rx" => RainHalo { rx: 10, ..rain },
            "ry" => RainHalo { ry: 10, ..rain },
            "mode" => RainHalo { mode: HaloMode::Over, ..rain },
        );

        let fire = fire_patch(1);
        assert_fields_change!(
            "FirePatch",
            fire,
            "row" => FirePatch { row: 14, ..fire },
            "x" => FirePatch { x: 14, ..fire },
            "y" => FirePatch { y: 14, ..fire },
            "w" => FirePatch { w: 14, ..fire },
            "h" => FirePatch { h: 14, ..fire },
            "base_y" => FirePatch { base_y: 14, ..fire },
            "peak_h" => FirePatch { peak_h: 14, ..fire },
            "phase" => FirePatch { phase: 14, ..fire },
            "temp" => FirePatch { temp: 14, ..fire },
            "strength" => FirePatch { strength: 14, ..fire },
            "lean" => FirePatch { lean: 14, ..fire },
            "cov_cap" => FirePatch { cov_cap: 14, ..fire },
            "cell_h" => FirePatch { cell_h: 14, ..fire },
            "mode" => FirePatch { mode: FireMode::Over, ..fire },
        );
    }

    /// The damage cache keeps only a row bitmap for an 8k-quad `glow_under` stream, and the
    /// compact revision still sees a payload change.
    #[test]
    fn damage_record_covers_8k_glow_under_without_retaining_its_payload() {
        const QUADS: usize = 8_191;
        const ROWS: usize = 128;

        let mut source = RenderInput::empty();
        source.rows = ROWS;
        source.glow_under = (0..QUADS)
            .map(|index| GlowQuad {
                row: (index % ROWS) as u16,
                x: index as u16,
                y: (index % ROWS * 16) as u16,
                w: 12,
                h: 10,
                color: 0x0060_2008,
                alpha: 0,
                color2: 0x0060_2008,
                alpha2: 0,
            })
            .collect();
        source.refresh_cursor_effect_damage();

        let mut cache = RenderInput::empty();
        cache.clone_damage_cache_from(&source);
        assert!(cache.glow_under.is_empty(), "the cache retained 8k quads");
        assert_eq!(
            cache.glow_under_damage.rows.len(),
            ROWS.div_ceil(u64::BITS as usize),
            "one bit per grid row, not one entry per quad"
        );
        assert!(
            cache.glow_under_damage.rows.len() * std::mem::size_of::<u64>()
                < source.glow_under.len() * std::mem::size_of::<GlowQuad>(),
            "the compact cache must be smaller than the payload it replaces"
        );

        let mut changed = source.clone();
        changed.glow_under[QUADS - 1].color ^= 1;
        changed.refresh_cursor_effect_damage();
        assert_eq!(
            source
                .glow_under_damage
                .same_content(&changed.glow_under_damage),
            Some(false),
            "the compact damage revision must detect a payload change"
        );
    }
}

#[cfg(test)]
mod host_row_prepend_tests {
    use super::{FirePatch, GlowQuad, HostRowPixels, RainHalo, RenderInput};
    use crate::terminal::RenderCell;

    const CH: usize = 10;
    const TOP: usize = 6;

    /// Window y of terminal row `row`'s top, before any prepend.
    fn y(row: usize) -> u16 {
        u16::try_from(TOP + row * CH).expect("small")
    }

    /// A 4-row frame whose window-space streams each carry a quad on a
    /// terminal row (and the aurora one above the grid too), plus the
    /// focused pane's effect clip.
    fn frame() -> RenderInput {
        let blank = RenderCell {
            ch: ' ',
            ..Default::default()
        };
        RenderInput {
            rows: 4,
            cols: 3,
            cells: vec![vec![blank; 3]; 4],
            cursor_glow_add: vec![
                GlowQuad {
                    row: 2,
                    y: y(2) + 1,
                    h: 4,
                    ..Default::default()
                },
                GlowQuad {
                    row: 0,
                    y: 2,
                    h: 3,
                    ..Default::default()
                },
            ],
            glow_under: vec![GlowQuad {
                row: 2,
                y: y(2),
                h: 4,
                ..Default::default()
            }],
            fire_patch: vec![FirePatch {
                row: 2,
                y: y(2),
                h: 10,
                base_y: y(3),
                ..Default::default()
            }],
            glow_halo: vec![RainHalo {
                row: 1,
                y: y(1),
                h: 10,
                cy: y(1) + 5,
                rx: 1,
                ry: 1,
                ..Default::default()
            }],
            fx_clip: Some((0, y(0), 30, y(4))),
            ..Default::default()
        }
    }

    /// `f`'s window-space pixels moved down by `dy` — what a producer whose
    /// origin already counted the prepended rows would have emitted.
    fn placed_below(mut f: RenderInput, dy: u16) -> RenderInput {
        for q in &mut f.cursor_glow_add {
            q.y += dy;
        }
        for q in &mut f.glow_under {
            q.y += dy;
        }
        for q in &mut f.fire_patch {
            q.y += dy;
            q.base_y += dy;
        }
        for h in &mut f.glow_halo {
            h.y += dy;
            h.cy += dy;
        }
        if let Some((_, y0, _, y1)) = &mut f.fx_clip {
            *y0 += dy;
            *y1 += dy;
        }
        f
    }

    /// `Translate` is "placed below, then placed": a web frame whose producers never counted
    /// the prepended rows splices exactly as a native frame whose producers did, and `Placed`
    /// moves no window-space pixel. Negative control: `Placed` on the web frame leaves the
    /// aurora on its old row.
    #[test]
    fn translate_is_the_native_splice_of_a_frame_placed_below_the_rows() {
        let strip = 2;
        let dy = u16::try_from(strip * CH).expect("small");
        let rows = vec![vec![RenderCell::default(); 3]; strip];
        let prepend = |mut f: RenderInput, pixels| {
            f.prepend_host_rows(
                strip,
                rows.iter().map(Vec::as_slice),
                CH,
                TOP,
                &mut Vec::new(),
                pixels,
            );
            f
        };
        let web = prepend(frame(), HostRowPixels::Translate);
        let native = prepend(placed_below(frame(), dy), HostRowPixels::Placed);
        assert!(web == native, "every channel the splice moves");
        assert_eq!(web.cursor_glow_add, native.cursor_glow_add);
        assert_eq!(web.glow_under, native.glow_under);
        assert_eq!(web.fire_patch, native.fire_patch);
        assert_eq!(web.glow_halo, native.glow_halo);
        assert_eq!(web.fx_clip, native.fx_clip);
        // Every in-grid quad lands on its terminal row, now `strip` lower; the
        // aurora above the grid lands on the chrome row its pixel lies in.
        assert_eq!(web.cursor_glow_add[0].row, 4);
        assert_eq!(web.cursor_glow_add[1].row, 1);
        assert_eq!(web.glow_under[0].row, 4);
        assert_eq!(web.fire_patch[0].row, 4);
        assert_eq!(web.fire_patch[0].base_y, y(3) + dy);
        assert_eq!(web.glow_halo[0].row, 3);
        assert_eq!(web.glow_halo[0].cy, y(1) + 5 + dy);
        let placed = prepend(frame(), HostRowPixels::Placed);
        assert_eq!(
            placed.cursor_glow_add[0].y,
            y(2) + 1,
            "Placed moves no pixel"
        );
        assert_eq!(placed.fx_clip, frame().fx_clip);
        assert_eq!(
            placed.cursor_glow_add[0].row, 2,
            "the negative control: the web frame under Placed keeps its old row"
        );
    }
    /// A prepend moves each BiDi row's visual→logical map down with its cells and selection;
    /// the chrome rows get none.
    #[test]
    fn a_prepend_moves_each_bidi_row_map_with_its_row() {
        use crate::selection::{SelectionSide, SelectionType};
        let mut f = frame();
        // Row 1 painted `a`, then a reversed two-cell run: logical 2 at visual 1.
        f.visual_to_logical = vec![Vec::new(), vec![0, 2, 1], Vec::new(), Vec::new()];
        f.selection
            .start_selection(1, 2, SelectionSide::Left, SelectionType::Simple);
        f.selection.update_selection(1, 2, SelectionSide::Right);
        f.selection.complete_selection();
        assert!(f.selection_contains_cell(1, 1, false, false));
        assert!(!f.selection_contains_cell(1, 2, false, false));

        let strip = 2;
        let rows = vec![vec![RenderCell::default(); 3]; strip];
        f.prepend_host_rows(
            strip,
            rows.iter().map(Vec::as_slice),
            CH,
            TOP,
            &mut Vec::new(),
            HostRowPixels::Placed,
        );
        assert!(f.visual_to_logical_row(0).is_empty() && f.visual_to_logical_row(1).is_empty());
        assert_eq!(f.visual_to_logical_row(3), [0, 2, 1]);
        assert!(
            f.selection_contains_cell(3, 1, false, false),
            "the highlight moved down with its row"
        );
        assert!(!f.selection_contains_cell(3, 2, false, false));
    }

    /// Equality reads the map by meaning: no entry and an empty entry are both
    /// the identity, while two different orders of a row are different frames.
    #[test]
    fn frames_compare_bidi_maps_by_meaning() {
        let plain = frame();
        let mut filled = frame();
        filled.visual_to_logical = vec![Vec::new(); 4];
        assert!(plain == filled);
        let mut reordered = frame();
        reordered.visual_to_logical = vec![Vec::new(), vec![1, 0]];
        assert!(plain != reordered);
    }
}

#[cfg(test)]
mod fx_input_tests {
    use std::sync::Arc;

    use super::{
        FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace, FxTexels,
        HostRowPixels, PxRect, RenderInput,
    };
    use crate::terminal::RenderCell;

    fn content() -> Arc<FxContent> {
        Arc::new(FxContent::new(
            vec![FxItem::Rect(FxRect {
                x: 1,
                y: 2,
                w: 3,
                h: 4,
                ink: 0,
            })],
            vec![FxInk::add(0x0010_2030); 2],
        ))
    }

    fn art() -> Arc<FxArt> {
        Arc::new(FxArt::new(1, 1, FxTexels::Index8, vec![1]).expect("sized"))
    }

    /// A frame carrying a clipped Window layer, a clipped Grid layer, an
    /// unclipped Window layer, an occluder and a prefetched art.
    fn frame(c: &Arc<FxContent>) -> RenderInput {
        let mut window = FxLayer::new(1, c.clone(), FxSlot::Over, FxSpace::Window, FxBlend::Exact);
        window.clip = PxRect::new(-3, 2, 60, 25);
        let mut grid = FxLayer::new(2, c.clone(), FxSlot::Under, FxSpace::Grid, FxBlend::Exact);
        grid.offset = (5, 7);
        grid.clip = PxRect::new(0, 0, 40, 30);
        let unclipped = FxLayer::new(3, c.clone(), FxSlot::Glow, FxSpace::Window, FxBlend::Exact);
        RenderInput {
            rows: 4,
            cols: 3,
            cells: vec![vec![RenderCell::default(); 3]; 4],
            fx_layers: vec![window, grid, unclipped],
            fx_occluders: vec![PxRect::new(0, 10, 50, 20)],
            fx_prefetch: vec![art()],
            ..Default::default()
        }
    }

    /// Frames compare their layers by identity and their occluders by value,
    /// and never read the prefetch. Each `!=` is a negative control: equal
    /// bytes behind another `Arc`, a moved offset, another slot and a moved
    /// occluder are each a different frame.
    #[test]
    fn fx_channels_compare_by_identity_and_prefetch_draws_nothing() {
        let c = content();
        let a = frame(&c);
        let mut b = frame(&c);
        b.fx_prefetch.clear();
        assert!(a == b, "same Arcs, prefetch ignored");
        let mut twin = frame(&content());
        twin.fx_prefetch.clone_from(&a.fx_prefetch);
        assert!(
            a != twin,
            "equal bytes behind another Arc are another layer"
        );
        let mut moved = frame(&c);
        moved.fx_layers[0].offset.0 += 1;
        assert!(a != moved);
        let mut slot = frame(&c);
        slot.fx_layers[0].slot = FxSlot::Glow;
        assert!(a != slot);
        let mut occ = frame(&c);
        occ.fx_occluders[0] = PxRect::new(0, 11, 50, 20);
        assert!(a != occ);
    }

    /// The damage cache keeps the layers' `Arc`s (so the identity it compares
    /// against can never be reused) and the occluders, and drops the
    /// prefetch; a full clone keeps all three; `clear_overlays` clears all
    /// three.
    #[test]
    fn the_damage_cache_keeps_the_layer_arcs_and_drops_the_prefetch() {
        let c = content();
        let src = frame(&c);
        let mut cache = RenderInput::default();
        cache.clone_damage_cache_from(&src);
        assert!(cache == src);
        assert!(Arc::ptr_eq(&cache.fx_layers[1].content, &c));
        assert_eq!(cache.fx_occluders, src.fx_occluders);
        assert!(cache.fx_prefetch.is_empty(), "prefetch is not cached");
        assert_eq!(
            Arc::strong_count(&c),
            7,
            "frame (3 layers), cache (3) and the local hold it"
        );
        let full = src.clone();
        assert!(Arc::ptr_eq(&full.fx_prefetch[0], &src.fx_prefetch[0]));
        let mut plain = src.clone();
        plain.clear_overlays();
        assert!(plain.fx_layers.is_empty() && plain.fx_occluders.is_empty());
        assert!(plain.fx_prefetch.is_empty());
    }

    /// A prepend moves a Window layer and the occluders by the window streams'
    /// `pixel_dy` (zero under `Placed`) and a Grid layer by the grid streams'
    /// `dy`, offset and clip together, each clip in its layer's own space; an
    /// unbounded clip stays unbounded. Negative control: under `Placed` the
    /// Window layers and the occluders stay.
    #[test]
    fn a_prepend_moves_each_layer_in_its_own_space() {
        let (strip, ch, top) = (2, 10, 6);
        let dy = i32::try_from(strip * ch).expect("small");
        let rows = vec![vec![RenderCell::default(); 3]; strip];
        let c = content();
        let prepend = |pixels| {
            let mut f = frame(&c);
            f.prepend_host_rows(
                strip,
                rows.iter().map(Vec::as_slice),
                ch,
                top,
                &mut Vec::new(),
                pixels,
            );
            f
        };
        let before = frame(&c);
        for (pixels, wdy) in [(HostRowPixels::Translate, dy), (HostRowPixels::Placed, 0)] {
            let f = prepend(pixels);
            assert_eq!(f.fx_layers[0].offset, (0, wdy), "{pixels:?}: Window offset");
            assert_eq!(
                f.fx_layers[0].clip,
                PxRect::new(-3, 2 + wdy, 60, 25 + wdy),
                "{pixels:?}: Window clip"
            );
            assert_eq!(f.fx_layers[2].offset, (0, wdy), "{pixels:?}: unclipped");
            assert_eq!(f.fx_layers[2].clip, PxRect::EVERYWHERE, "{pixels:?}");
            assert_eq!(
                f.fx_layers[1].offset,
                (5, 7 + dy),
                "{pixels:?}: Grid offset"
            );
            assert_eq!(
                f.fx_layers[1].clip,
                before.fx_layers[1].clip.translate(0, dy),
                "{pixels:?}: Grid clip"
            );
            assert_eq!(
                f.fx_occluders,
                vec![PxRect::new(0, 10 + wdy, 50, 20 + wdy)],
                "{pixels:?}: occluders"
            );
            assert!(
                Arc::ptr_eq(&f.fx_layers[0].content, &c),
                "content untouched"
            );
        }
    }
}
