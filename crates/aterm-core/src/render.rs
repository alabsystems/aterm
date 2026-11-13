// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The engine-owned render snapshot: [`RenderInput`](crate::render::RenderInput) is the plain-owned, `Terminal`-free value a renderer reads to paint one frame.

use crate::grid::LineSize;
use crate::selection::TextSelection;
use crate::terminal::{CursorStyle, RenderCell};
use aterm_hash::FxHashMap;

#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub use crate::render_fx::fx_counts_here;
pub use crate::render_fx::{
    FX_ART_MAX_SIDE, FX_FRAME_ART_TEXELS, FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer,
    FxRamps, FxRect, FxRowRuns, FxRun, FxSlot, FxSpace, FxSprite, FxTexels, PxRect,
    fx_layer_builds, fx_payload_copies,
};
pub use crate::render_ramps::{
    GlowEnd, GlowRamp, GlowRamps, Laid, RampMark, end_alpha, end_color, glow_end,
};

/// One faded cell of the cursor motion trail, painted in the trail colour at `alpha` over its own background, in the cursor's viewport coordinates (after the tab-strip splice and any split-pane offset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrailCell {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: usize,
    /// Viewport column (0-indexed).
    pub col: usize,
    /// Blend coverage `0..=255` of the trail colour over this cell's background, drawn opaque as the pre-blended result so the CPU fill and the GPU `REPLACE` quad agree.
    pub alpha: u8,
}

/// Whether a frame's text can take the GPU's bloom ([`RenderInput::bloom_gate`]).
///
/// The cursor effects hold it where the theme's text (its default ink on its
/// default ground) keeps only its own reading under the lights, its solid ink
/// under the letter guard's floor on the bare page (a dim theme's). The bloom
/// blurs the over-text light past every cut the guard made on a letter, and
/// such a letter has no room for it: under Comet's bloom Solarized Dark's
/// newest letter read 3.47:1 of its own 4.06, 4.10 without it (looks7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BloomGate {
    /// The bloom draws over the frame's over-text light.
    #[default]
    Open,
    /// The page's text holds the bloom: it draws none.
    Held,
    /// The bloom draws, but the post passes (the bloom and the SDR crown) take
    /// no light from within their reach of a lettered cell: the letter guard
    /// keeps every letter at its bar on the light the frame composites, and the
    /// passes' spill would lift a letter's ground past what it weighed (looks10
    /// L1: Lumen's newest letter read 6.25:1 on the dark page and 4.37 bloomed).
    /// The trail's own lights are drawn whole; only their blur and crown are
    /// withheld near the text.
    SparesLetters,
    /// The page's text holds the bloom ([`Self::Held`]), and the post pass
    /// left, the SDR crown, takes no light from within its reach of a lettered
    /// cell ([`Self::SparesLetters`]): a dim page's letter keeps the share of
    /// its own reading the letter guard weighed on the light the frame
    /// composites, and the crown screened Beam's light over a selected row's
    /// letter on the dim grey from 97.1% of its own reading to 95.8% (round
    /// 27).
    HeldSparesLetters,
}

impl BloomGate {
    /// [`Self::Held`] when `held`, else [`Self::Open`].
    #[must_use]
    #[inline]
    pub const fn of(held: bool) -> Self {
        if held { Self::Held } else { Self::Open }
    }

    /// The gate whose bloom is `held` and whose post passes `spares` the
    /// letters: [`Self::HeldSparesLetters`], [`Self::Held`],
    /// [`Self::SparesLetters`] or [`Self::Open`].
    #[must_use]
    #[inline]
    pub const fn sparing(held: bool, spares: bool) -> Self {
        match (held, spares) {
            (true, true) => Self::HeldSparesLetters,
            (true, false) => Self::Held,
            (false, true) => Self::SparesLetters,
            (false, false) => Self::Open,
        }
    }

    /// Whether the bloom is held.
    #[must_use]
    #[inline]
    pub const fn held(self) -> bool {
        matches!(self, Self::Held | Self::HeldSparesLetters)
    }

    /// Whether the post passes keep their source off the lettered cells.
    #[must_use]
    #[inline]
    pub const fn spares_letters(self) -> bool {
        matches!(self, Self::SparesLetters | Self::HeldSparesLetters)
    }
}

/// One light quad of an effect stream (aurora comet, bloom crown, landing ring, sparks, nova, fire body).
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
    /// The source-over opacity `0..=255`, which also selects the mode.
    pub alpha: u8,
    /// Premultiplied colour `0x00RRGGBB` at the right edge of a horizontal gradient across the quad's `w` columns ([`color`](GlowQuad::color) is the left).
    pub color2: u32,
    /// The source-over opacity at the right edge ([`alpha`](GlowQuad::alpha) is the left).
    pub alpha2: u8,
}

impl GlowQuad {
    /// A flat quad: one colour and one opacity across its width (`color2 == color`, `alpha2 == alpha`).
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

    /// A gradient quad: `(color, alpha)` at the left edge ramping to `(color2, alpha2)` at the right (see [`GlowQuad::color2`]).
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

    /// Canonical lossless words for cursor-effect fingerprints.
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

/// The fingerprint words of one [`GlowQuad`] (two flat, three gradient); iterates the live words only.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlowQuadWords {
    words: [u64; 3],
    len: u8,
}

impl GlowQuadWords {
    const GRADIENT_TAG: u64 = 0x4752_0000_0000_0000;
    /// Tags a ramp's header word ([`EffectStreamDamage::refresh_glow`]).
    const RAMP_TAG: u64 = 0x5241_0000_0000_0000;

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

/// Which [`GlowQuad::alpha`] convention an emitter stamps, so a rasterizer shared by additive and source-over streams knows which it produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlowBlend {
    /// Premultiplied additive light: every quad has `alpha == 0`. The default.
    #[default]
    Add,
    /// Premultiplied source-over paint: each quad's `alpha` is the coverage that premultiplied its colour.
    Over,
}

/// How a [`RainHalo`]'s radially-weighted colour composites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HaloMode {
    /// Premultiplied saturating-add light (CPU `add_sat` == GPU One/One on the Unorm view).
    #[default]
    Add,
    /// Source-over veil: straight `color`, the falloff weight as per-pixel alpha.
    Over,
}

/// A radial halo quad: like [`GlowQuad`] (one row band) but its light falls off radially from `(cx, cy)`.
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
    /// The halo-centre colour `0x00RRGGBB`.
    pub color: u32,
    /// Pixel X of the halo centre, the falloff origin; may lie outside this quad's row band.
    pub cx: u16,
    /// Pixel Y of the halo centre (same convention as `y`).
    pub cy: u16,
    /// Horizontal falloff half-extent in pixels (light reaches 0 there); `>= 1`, guaranteed by the emitter (0 would divide by zero).
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

/// How a [`FirePatch`]'s field output composites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FireMode {
    /// Premultiplied saturating-add flame light: the dark-theme mode.
    #[default]
    Add,
    /// Source-over ink-fire (straight RGB, field-shaped alpha), readable on white.
    Over,
}

/// One fire patch: a stateless, float-free procedural fire field evaluated at every device pixel of the quad by both backends (`aterm_render::fire_field`, mirrored by the GPU's `fs_fire_add`/`fs_fire_over`), so they land the same byte.
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
    /// Window-absolute pixel Y of the flame root (field `v = 0`; flames rise, so live pixels have `y < base_y`), shared by a burn's patches.
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

/// Compact damage identity for a window-absolute cursor-effect stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectStreamDamage {
    /// Two-lane content fingerprint; the explicit length prevents an empty/non-empty alias.
    fingerprint: u128,
    len: usize,
    /// One bit per grid row touched: compact even for an 8k-quad ribbon, and it keeps the vacated rows after the cache drops the payload.
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

    /// Whether the described stream was empty.
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

    /// [`Self::refresh`] for a glow stream: its quads, then each of its ramps
    /// ([`GlowRamps`]) in its own words ([`GlowRamp::damage_words`], its run
    /// and its place among the quads included) and the ends it keeps, its
    /// band's row marked.
    #[inline]
    fn refresh_glow(&mut self, quads: &[GlowQuad], ramps: &GlowRamps, grid_rows: usize) {
        if ramps.is_empty() {
            self.refresh(quads, grid_rows, |quad| quad.row, hash_glow_quad);
            return;
        }
        let len = quads.len() + ramps.items.len();
        let mut fingerprint = EffectStreamFingerprint::new(len);
        self.rows.clear();
        self.rows.resize(grid_rows.div_ceil(u64::BITS as usize), 0);
        let rows = &mut self.rows;
        let mut mark = |row: u16| {
            let row = usize::from(row);
            if let Some(word) = rows.get_mut(row / u64::BITS as usize) {
                *word |= 1_u64 << (row % u64::BITS as usize);
            }
        };
        for quad in quads {
            hash_glow_quad(quad, &mut fingerprint);
            mark(quad.row);
        }
        for ramp in &ramps.items {
            let [band, run, at] = ramp.damage_words();
            fingerprint.write_pair(band, run | GlowQuadWords::RAMP_TAG);
            let (ends, rest) = ramp.kept_ends(&ramps.ends).as_chunks::<4>();
            for e in ends {
                fingerprint.write_pair(
                    u64::from(e[0]) | u64::from(e[1]) << 32,
                    u64::from(e[2]) | u64::from(e[3]) << 32,
                );
            }
            let last = match *rest {
                [a, b, ..] => u64::from(a) | u64::from(b) << 32,
                _ => 0,
            };
            fingerprint.write_pair(at, last);
            mark(ramp.row);
        }
        self.fingerprint = fingerprint.finish();
        self.len = len;
        self.valid = true;
    }
}

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

/// An RGBA8 sprite atlas (straight alpha, row-major, `width*height*4`), shared by `Arc`.
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

/// Which small procedural sprite a [`WordDecoration`] paints, rasterized by the renderer (`deco_coverage`) into a coverage mask so CPU and GPU land the same pixels.
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
    /// A soft annulus-arc ring segment filling the cell: the singularity nova's darkening ring (additive light cannot darken), one `DecoBlend::Over` stamp per covered cell.
    RingArc,
    /// A full-cell soft-edged square (~1 px ramp): the super nova's dark veil on light backgrounds, one `DecoBlend::Over` stamp per covered cell.
    Shade,
}

/// How a [`WordDecoration`] composites onto the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecoBlend {
    /// Premultiplied saturating add (`dst + src`) — the profanity sparkle, which only ever brightens, exactly like [`GlowQuad`].
    #[default]
    Add,
    /// Source-over alpha blend (`dst*(1-a) + src*a`): the feline cat-paw, a solid stamp tinted by `color` at `alpha`.
    Over,
}

/// One sparkle-word decoration.
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

/// One animated-ink foreground override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InkCell {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: u16,
    /// Viewport column (0-indexed); the lead cell of a wide glyph.
    pub col: u16,
    /// Final `[r, g, b]` to substitute for the cell's resolved `fg`.
    pub color: [u8; 3],
}

/// One per-cell glyph foreground override (fire's dark glyph cores: engulfed cells recoloured toward ember-black).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharFg {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: u16,
    /// Viewport column (0-indexed); the lead cell of a wide glyph.
    pub col: u16,
    /// Final `0x00RRGGBB` to substitute for the cell's resolved `fg`.
    pub fg: u32,
}

/// One contrast-halo cell (fire text legibility).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FireHaloCell {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: u16,
    /// Viewport column (0-indexed); the lead cell of a wide glyph.
    pub col: u16,
    /// Fire engulfment weight (`0..=255`), scaling the halo's stamp alpha.
    pub strength: u8,
}

/// Sentinel for an unresolved live frame color: the renderer uses its theme.
pub const COLOR_UNSET: u32 = 0xFF00_0000;

/// Sentinel for an explicitly dynamic selected-text foreground (`OSC 21;selection_foreground=`).
pub const COLOR_DYNAMIC: u32 = 0xFE00_0000;

/// One run of columns in a composed split row with its own DEC line size (side-by-side panes may differ in DECDWL/DECDHL).
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

/// A host chrome row drawn at pixel resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChromeRaster {
    /// The frame row (counted from the top of the composed input, like [`RenderInput::cells`]).
    pub row: u16,
    /// One `0x00RRGGBB` per frame pixel column, `[0, frame_w)`; empty for no ground (the cells' own backgrounds stand).
    pub ground: std::sync::Arc<[u32]>,
    /// One `0x00RRGGBB` per frame pixel column for the rail band ([`ChromeRaster::KEEP`] keeps the ground); empty for no rail.
    pub rail: std::sync::Arc<[u32]>,
    /// The rail band's height in device pixels, at the bottom of the row.
    pub rail_h: u16,
    /// This row's glyphs keep clear of the rail.
    pub clear_rail: bool,
    /// Cell columns `[start, end)` that keep their own cell background: the ground is not painted there.
    pub own: Vec<(u16, u16)>,
    /// The ink split, when the row's fill ends inside a cell.
    pub split: Option<InkSplit>,
    /// Outlined capsules, drawn after the ground and rail and before the glyphs.
    pub rings: Vec<ChromeRing>,
    /// Cells whose glyph is drawn as a band icon instead of from a font.
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

/// One outlined capsule on a [`ChromeRaster`] row over cells `[start, end)` (`aterm_render::chrome_ring_runs`, `aterm_render::chrome_ring_floor_cols`).
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
    /// The band's closing seam (`0x00RRGGBB`) on the stack's last row.
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

/// The band's icon set: one drawn icon for every status glyph a message row can carry, so the glyph cell reads as one family whatever font holds `⚠` or `ℹ`. The glass's marks ([`Self::glass_mark`]) and the need pictograms ([`Self::is_pictogram`]) are drawn in the same family.
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
    /// `●` — a disc: the harness switch on.
    Disc,
    /// `○` — a ring: the harness switch off, Autopilot off.
    Ring,
    /// `◐` — the ring with its left half filled: the harness switch turning on or off, a chip's click
    /// in flight.
    HalfDisc,
    /// `◌` — the ring drawn as dots: a light whose mode is not known.
    DottedRing,
    /// `⊘` — the ring struck through: no harness runs here.
    SlashedRing,
    /// `⤓` — an arrow down onto a bar: Autopilot's Land.
    ToBar,
    /// `◉` — the ring with a disc at its centre: the effort a fact menu shows.
    Fisheye,
    /// `◈` — a diamond, the ring's size, with a small one filled at its centre: a fact menu's top-effort key row.
    InsetDiamond,
    /// `⌂` — a house, the ring's size: the path a fact menu shows.
    House,
    /// `◆` — a diamond filled to the ring's box: the model a fact menu shows.
    Diamond,
    /// `⎇` — a line that steps up and runs on beside its foot's own, in the ring's box: the
    /// branch line D shows.
    Fork,
    /// `⎆` — an arrow entering a diamond outline, in the ring's box: the account line D's
    /// fallback unit and the Account card's items show.
    Enter,
    /// `↺` — the ring opened across its top left, the open line's top end an arrowhead turning
    /// back leftward, in the ring's box (`↻`'s turning arrow, [`Self::Update`], mirrored): the
    /// reset line E's chart and tail, line D's fallback unit and the Account card's history charts show.
    Reset,
    /// A padlock, the need pictogram ([`Self::is_pictogram`]) for a tab that waits for you: a password, an approval, an answer.
    Lock,
    /// An hourglass, its sand run down, the need pictogram for a stuck job.
    Hourglass,
    /// A drive, drawn as a drum, the need pictogram for a volume that fills within the hour.
    Drive,
}

impl BandIcon {
    /// Every icon, in its index order ([`Self::index`]).
    pub const ALL: [BandIcon; 29] = [
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
        Self::Disc,
        Self::Ring,
        Self::HalfDisc,
        Self::DottedRing,
        Self::SlashedRing,
        Self::ToBar,
        Self::Fisheye,
        Self::InsetDiamond,
        Self::House,
        Self::Diamond,
        Self::Fork,
        Self::Enter,
        Self::Reset,
        Self::Lock,
        Self::Hourglass,
        Self::Drive,
    ];

    /// THE GLASS'S DRAWN MARKS.
    #[must_use]
    #[inline]
    pub const fn glass_mark(ch: char) -> Option<Self> {
        Some(match ch {
            '\u{25cf}' => Self::Disc,
            '\u{25cb}' => Self::Ring,
            '\u{25d0}' => Self::HalfDisc,
            '\u{25cc}' => Self::DottedRing,
            '\u{2298}' => Self::SlashedRing,
            '\u{2913}' => Self::ToBar,
            '\u{25c9}' => Self::Fisheye,
            '\u{25c8}' => Self::InsetDiamond,
            '\u{2302}' => Self::House,
            '\u{25c6}' => Self::Diamond,
            '\u{2387}' => Self::Fork,
            '\u{2386}' => Self::Enter,
            '\u{21ba}' => Self::Reset,
            _ => return None,
        })
    }

    /// Whether this icon is one of the glass's marks ([`Self::glass_mark`]): drawn in its own cell at one size whatever its neighbours hold, where a band's badge spills into blank neighbours.
    #[must_use]
    #[inline]
    pub const fn is_glass_mark(self) -> bool {
        Self::glass_mark(self.ch()).is_some()
    }

    /// Whether this icon is one of the need pictograms (a need's kind: [`Self::Lock`], [`Self::Hourglass`], [`Self::Drive`]): drawn only where a painter names it (`DrawPrim::BandIcon`, a raster's [`ChromeIcon`]) and never for a character, so neither [`Self::for_char`] nor [`Self::glass_mark`] gives it. Its [`Self::ch`] is the stand-in a text grid keeps under it. Like a band's badge it spills into blank neighbours.
    #[must_use]
    #[inline]
    pub const fn is_pictogram(self) -> bool {
        matches!(self, Self::Lock | Self::Hourglass | Self::Drive)
    }

    /// The icon that stands in for `ch`, the band's glyph; `None` for any character outside the set (it keeps its font glyph).
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

    /// The character this icon is drawn for (the one a text grid keeps). A pictogram's ([`Self::is_pictogram`]) is a one-cell stand-in that neither [`Self::for_char`] nor [`Self::glass_mark`] maps back: `⚿` for [`Self::Lock`] (Unicode's padlock, U+1F512, is a wide emoji), `⧖` for [`Self::Hourglass`] and `🖴` for [`Self::Drive`].
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
            Self::Disc => '\u{25cf}',
            Self::Ring => '\u{25cb}',
            Self::HalfDisc => '\u{25d0}',
            Self::DottedRing => '\u{25cc}',
            Self::SlashedRing => '\u{2298}',
            Self::ToBar => '\u{2913}',
            Self::Fisheye => '\u{25c9}',
            Self::InsetDiamond => '\u{25c8}',
            Self::House => '\u{2302}',
            Self::Diamond => '\u{25c6}',
            Self::Fork => '\u{2387}',
            Self::Enter => '\u{2386}',
            Self::Reset => '\u{21ba}',
            Self::Lock => '\u{26bf}',
            Self::Hourglass => '\u{29d6}',
            Self::Drive => '\u{1f5b4}',
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

/// Where a chrome row's fill ends inside a cell ([`ChromeRaster::split`]).
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

/// One pane-local live default-background interval inside a composed [`RenderInput`] row.
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

/// Half-open frame-space bounds for a composed text selection, so a projected pane's multi-row selection cannot span dividers and sibling panes.
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

/// One pane's selection, relocated into frame coordinates, with its own box and live colours.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneSelection {
    /// The pane's selection with its anchors already moved into frame rows and columns (pane origin + that pane's `display_offset`).
    pub selection: TextSelection,
    /// The pane's half-open frame-space box; always present, since an unbounded pane selection would tint its siblings.
    pub clip: SelectionClip,
    /// The pane's live selection background (`0x00RRGGBB`), or [`COLOR_UNSET`] for the renderer's theme.
    pub bg: u32,
    /// The pane's live selected-text foreground, or [`COLOR_UNSET`] / [`COLOR_DYNAMIC`]; same encoding as [`RenderInput::selection_fg`].
    pub fg: u32,
    /// Whether this pane is unfocused, so its band takes the renderer's inactive selection colour.
    pub inactive: bool,
}

/// The per-row, per-entry selection key the row-damage diff (`aterm_render::compute_dirty_rows`) compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionRowKey {
    /// First selected column on this frame row (inclusive, post-clip).
    pub lo: u16,
    /// Last selected column on this frame row (inclusive, post-clip).
    pub hi: u16,
    /// The entry's live selection background (or [`COLOR_UNSET`]).
    pub bg: u32,
    /// The entry's live selected-text foreground (or [`COLOR_UNSET`] / [`COLOR_DYNAMIC`]).
    pub fg: u32,
    /// The entry's inactive flag (a different band colour).
    pub inactive: bool,
}

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

/// Everything a renderer reads for one frame, as plain owned data, emitted by [`crate::terminal::Terminal::cell_frame_into`].
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
    /// Host/effect cursor shape for this frame, kept apart from [`cursor_style`](Self::cursor_style) so an effect never overwrites DECSCUSR.
    pub cursor_effect_style_override: Option<CursorStyle>,
    /// The cursor motion trail ([`TrailCell`]), filled and decayed by the host's clock; `cell_frame_into` leaves it untouched.
    pub cursor_trail: Vec<TrailCell>,
    /// The host-resolved trail colour (`0x00RRGGBB`) blended over each trail cell.
    pub cursor_trail_color: u32,
    /// The cursor aurora's light quads (comet, bloom crown, landing ring, sparks), under the crisp cursor.
    pub cursor_glow_add: Vec<GlowQuad>,
    /// The ramps drawn among [`cursor_glow_add`](Self::cursor_glow_add)'s quads ([`GlowRamps`]).
    pub cursor_glow_add_ramps: GlowRamps,
    /// Radial cursor-effect light (fire embers, crown) with the [`RainHalo`] integer falloff, drawn after the aurora and before the nova and rain streams.
    pub glow_halo: Vec<RainHalo>,
    /// Under-glyph additive light (the fire body glyphs sit inside as silhouettes).
    pub glow_under: Vec<GlowQuad>,
    /// The ramps drawn among [`glow_under`](Self::glow_under)'s quads ([`GlowRamps`]).
    pub glow_under_ramps: GlowRamps,
    /// Per-pixel procedural fire-field patches, drawn at the [`glow_under`](RenderInput::glow_under) slot so engulfed glyphs read as silhouettes.
    pub fire_patch: Vec<FirePatch>,
    /// Producer-built compact damage metadata for [`cursor_glow_add`](Self::cursor_glow_add).
    pub cursor_glow_add_damage: EffectStreamDamage,
    /// Producer-built compact damage metadata for [`glow_halo`](Self::glow_halo).
    pub glow_halo_damage: EffectStreamDamage,
    /// Producer-built compact damage metadata for [`glow_under`](Self::glow_under).
    pub glow_under_damage: EffectStreamDamage,
    /// Producer-built compact damage metadata for [`fire_patch`](Self::fire_patch).
    pub fire_patch_damage: EffectStreamDamage,
    /// Host-resolved cursor fill override (`0x00RRGGBB`), or `None` for the theme/OSC-12 colour.
    pub cursor_fill_override: Option<u32>,
    /// Whether the page's text can take the GPU's bloom ([`BloomGate`]).
    pub bloom_gate: BloomGate,
    /// Sparkle-word decorations: small sprites over cells of matched words.
    pub word_decorations: Vec<WordDecoration>,
    /// Animated-ink per-cell fg overrides.
    pub ink: Vec<InkCell>,
    /// Per-cell final glyph fg overrides (fire's dark glyph cores), substituted at the same seam as [`ink`](RenderInput::ink), which wins on a cell with both.
    pub char_fg: Vec<CharFg>,
    /// Per-cell contrast-halo strengths (fire legibility): the engulfment weight of the dark ring under a glyph's ink.
    pub fire_halo: Vec<FireHaloCell>,
    /// Supernova additive light (crown, shockwave ring, rays), like [`cursor_glow_add`](RenderInput::cursor_glow_add) but grid-interior.
    pub nova_add: Vec<GlowQuad>,
    /// Rain bright-head additive halos, like [`nova_add`](RenderInput::nova_add): grid-interior pixels, one row band per quad.
    pub rain_add: Vec<RainHalo>,
    /// Retained fx layers ([`FxLayer`]).
    pub fx_layers: Vec<FxLayer>,
    /// Host chrome painted over every fx layer but its own ([`FxLayer::chrome`]), in window pixels (a find bar's band, a menu card).
    pub fx_occluders: Vec<PxRect>,
    /// Arts a GPU renderer should make resident ahead of the frame that draws them (the card's arts when a drawer image is published).
    pub fx_prefetch: Vec<std::sync::Arc<FxArt>>,
    /// Scrollback offset: viewport row `r` shows live row `r - display_offset`.
    pub display_offset: i32,
    /// Absolute row of the top visible line at extraction (`Grid::base_y()`), under the cells' lock.
    pub base_y: i64,
    /// Revision of top-anchored protected-footer row insertions at extraction, which one `base_y` delta cannot re-anchor.
    pub absolute_row_revision: u64,
    /// `Grid::history_renumber_epoch()` at extraction.
    pub history_renumber_epoch: u64,
    /// The holder fence at extraction ([`Terminal::holder_view`](crate::terminal::Terminal::holder_view)), resolved to absolute rows under the same lock as [`base_y`](Self::base_y).
    pub holder: crate::terminal::HolderView,
    /// [`Terminal::screen_mutations`](crate::terminal::Terminal::screen_mutations) at extraction, under the same lock (the dev card probe dates a snapshot by it).
    pub screen_mutations: u64,
    /// Sub-row scroll translate (display only).
    pub scroll_frac_px: i32,
    /// The terminal-content row band `[grid_top_row, grid_bot_row)` translated by [`scroll_frac_px`](Self::scroll_frac_px) (`0` = no band).
    pub grid_top_row: usize,
    /// One past the last terminal-content row — see [`grid_top_row`](Self::grid_top_row).
    pub grid_bot_row: usize,
    /// The row just below the viewport, which a positive [`scroll_frac_px`](Self::scroll_frac_px) glide slides in at the bottom.
    pub apron_row: ApronRow,
    /// The facts `apron_row` was last extracted under, so an unchanged fill keeps the row (`Terminal::apron_row_into`).
    pub apron_stamp: Option<ApronStamp>,
    /// Focused-pane effect clip `(x0, y0, x1, y1)` in window-absolute device pixels (`None` outside a split).
    pub fx_clip: Option<(u16, u16, u16, u16)>,
    /// A clone of the active text selection, for per-cell highlighting.
    pub selection: TextSelection,
    /// Optional frame-space box confining [`selection`](Self::selection), so a split's focused-pane selection cannot tint a divider or sibling.
    pub selection_clip: Option<SelectionClip>,
    /// The live selection background (`0x00RRGGBB`, OSC 17/21); `COLOR_UNSET` uses the renderer's theme.
    pub selection_bg: u32,
    /// The live selected-text foreground (`0x00RRGGBB`, OSC 19/21).
    pub selection_fg: u32,
    /// Every visible pane's selection in frame coordinates, each with its own clip and colours.
    pub selections: Vec<PaneSelection>,
    /// Per-row, sparse emoji grapheme-cluster strings.
    pub clusters: Vec<Vec<(usize, Box<str>)>>,
    /// Per-row, sparse combining marks: `(col, marks)` for cells with diacritics (é, ñ, …).
    pub combining: Vec<Vec<(usize, Box<[char]>)>>,
    /// Per-row BiDi visual→logical column map.
    pub visual_to_logical: Vec<Vec<u16>>,
    /// Per-row DEC line size (DECDWL/DECDHL), drawn scaled.
    pub line_sizes: Vec<LineSize>,
    /// Per-row DEC line-size runs for composed split rows where panes disagree; non-empty runs partition `0..cols` ascending.
    pub line_size_spans: Vec<Vec<LineSizeSpan>>,
    /// Pane-local live default-background intervals for composed split rows.
    pub default_bg_spans: Vec<Vec<DefaultBgSpan>>,
    /// Host chrome rows drawn at pixel resolution ([`ChromeRaster`]), at most one per row, in any order.
    pub chrome_rasters: Vec<ChromeRaster>,
    /// The emptied icon lists of retired chrome rasters ([`Self::retire_chrome_rasters`]), kept for the host's next ones ([`Self::chrome_icon_list`]) so a refilled frame's icons allocate nothing. Scratch storage, not frame content: no renderer reads it, equality ignores it, a clone starts with none and `clone_from` keeps the destination's own.
    pub chrome_icon_spares: Vec<Vec<ChromeIcon>>,
    /// Per-row, sparse inline-image placements (`term.images_row(r)`): `(col, ImageRef)` for every covered cell.
    pub images: Vec<Vec<(usize, aterm_grid::ImageRef)>>,
    /// Wallpaper base layer: a host-prepared RGBA8 image, pre-scaled to the frame (cover-fit) and pre-dimmed; the renderer only blits.
    pub wallpaper: Option<std::sync::Arc<SceneAtlas>>,
    /// The live default background (`0x00RRGGBB`), DECSCNM folded in.
    pub default_bg: u32,
    /// The live default foreground (`0x00RRGGBB`), the twin of [`Self::default_bg`] (DECSCNM swaps both or neither).
    pub default_fg: u32,
    /// The implicit empty cell resolved under the cells' lock.
    pub implicit_blank: RenderCell,
    /// The live cursor colour (`0x00RRGGBB`).
    pub cursor_color: u32,
    /// The engine's damage epoch under the fill's lock ([`Terminal::damage_epoch`](crate::terminal::Terminal::damage_epoch)), so a consumer detects staleness against a later read.
    pub snapshot_seq: u64,
    /// Content-only arrival clock at the fill; damage-only frames do not advance it.
    pub content_seq: u64,
    /// When the screen last changed at the fill ([`Terminal::drawn_at`](crate::terminal::Terminal::drawn_at), the parse's clock), read under the same lock as [`content_seq`](Self::content_seq): a frame that first sees output long after it was drawn is showing history, not arrival.
    pub drawn_at: Option<aterm_time::Instant>,
    /// Parser batch sequence at the fill.
    pub process_sequence: u32,
    /// Present-time latency hint.
    pub input_hot: bool,
    /// Identity of the engine instance that last filled this snapshot (`0`: never), so a scratch reused across terminals (the split compositor's shared `pane_scratch`) never serves one pane's retained rows as another's.
    pub terminal_id: u64,
    /// The stamping terminal's extraction generation at fill time (`0`: never filled).
    pub extract_gen: u64,
    /// `snapshot_seq` as stamped by the last engine fill.
    pub engine_fill_seq: u64,
    /// Whether the stamping terminal's alternate screen was active at fill time.
    pub engine_alt: bool,
    /// The stamping terminal's active grid at fill time
    /// ([`Grid::instance`](crate::grid::Grid::instance)): [`content_seq`](Self::content_seq) is
    /// that grid's clock and starts over on a new one, so this names whose clock it is.
    pub grid_instance: u64,
    /// Which column order the last engine fill left this snapshot's rows in.
    pub engine_row_order: RowOrder,
    /// Per-row content revision ([`Grid::row_revisions`](crate::grid::Grid::row_revisions)), captured under the cells' lock, so a consumer diffing two snapshots (`aterm_render::compute_dirty_rows`) answers "did row `r` change?" with a `u64` compare.
    pub row_rev: Vec<u64>,
    /// Identity of the lane [`row_rev`](Self::row_rev) belongs to, or `0` when its stamps must not be trusted.
    pub row_rev_lane: u64,
    /// How many host rows (the in-grid tab strip's chrome rows) have been prepended since the engine fill.
    pub row_shift: usize,
    /// `snapshot_seq` as of the last operation that left this snapshot equal to an engine fill shifted down by [`row_shift`](Self::row_shift) host rows (the fill itself, or a pure row prepend on top of one).
    pub shifted_fill_seq: u64,
    /// `snapshot_seq` as of the last composed frame a host committed here.
    pub composed_fill_seq: u64,
}

/// What a host row prepend keeps of the snapshot it lands on: read before the prepend ([`RenderInput::host_prepend_blessing`]) and recorded after it ([`RenderInput::note_host_row_prepend`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrependBlessing {
    /// No engine fill stamped the snapshot, or a host wrote into it since: the prepend disowns the revision lane and cannot be undone.
    Disowned,
    /// The snapshot is an engine fill shifted down by host rows, but its revision lane does not vouch for its rows (a scrolled-back fill, one taken under whole-grid damage): the prepend drops the lane and can still be undone ([`RenderInput::undo_host_row_prepend`]).
    Shift,
    /// An engine fill shifted down by host rows whose revision lane vouches for every row: the prepend keeps the lane and can be undone.
    Lane,
}

impl PrependBlessing {
    /// Whether the prepend keeps the revision lane.
    #[must_use]
    #[inline]
    pub const fn keeps_lane(self) -> bool {
        matches!(self, Self::Lane)
    }
}

/// The column order the last engine fill left a [`RenderInput`]'s rows in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowOrder {
    /// No row was permuted: logical order and visual order coincide, for every row and every column-indexed channel (cells, clusters, combining marks, images, and the cursor column).
    #[default]
    Logical,
    /// A row was permuted into BiDi visual order. The permutation is not idempotent, so these rows may not be retained by a damage-scoped refill.
    BidiVisual,
}

/// The first continuity clause, in the check's short-circuit order, that refused the damage-scoped arm (`FrameRefill::Full`), so a Full-dominated steady state is diagnosable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(usize)]
pub enum FullRefillCause {
    /// The last engine fill permuted a row into BiDi visual order; retained rows would be double-permuted.
    BidiVisual,
    /// The scratch was never engine-filled (`terminal_id == 0`): every window's first frame.
    ScratchUnstamped,
    /// The scratch was last engine-filled by a different terminal (the split compositor's shared pane scratch hands off between panes).
    TerminalMismatch,
    /// Another consumer called `take_damage` since the scratch's fill (`extract_gen` moved), so the tracker's bits no longer describe every row that changed under this scratch.
    DamageTaken,
    /// A host mutator wrote content channels after the engine fill and bumped `snapshot_seq` (prediction ghosts, IME preedit, find bar, strip splice).
    HostMutation,
    /// A host row prepend is still in force (`row_shift != 0`): `undo_host_row_prepend` did not run.
    RowShift,
    /// The alternate-screen bit differs between the scratch and the engine.
    AltScreen,
    /// The scratch's own `rows`/`cols` disagree with the requested frame size — a resize the scratch has not been refilled at yet.
    ScratchRows,
    /// The scratch's row count (`cells.len()`) disagrees with the requested rows while its `rows` field agrees: a host prepend that grew the container without stamping a shift.
    ScratchRowCount,
    /// The requested frame size disagrees with the engine's own grid (a resize in flight).
    EngineDims,
    /// The viewport is scrolled back and the scrolled continuity proof (`Terminal::scrolled_scoped_refill_delta`) did not hold: the scratch entered history from the live bottom, content or row identity moved under the parked viewport, the tracker holds whole-grid damage, or the net move exposes the whole viewport (which is also where output under a viewport parked deeper than one screen lands).
    EngineScrolled,
    /// The engine is at offset 0 but the scratch was filled while scrolled back (the frame that returns to the bottom), so its retained rows use a different viewport mapping.
    ScratchScrolled,
    /// `base_y` moved: the live grid scrolled, so retained rows shifted out from under their indices.
    BaseY,
    /// The absolute row revision moved without `base_y` moving: a protected footer insertion or another row-identity change.
    RowRevision,
    /// The tracker holds `Damage::Full` (a palette recolor, DECSCNM, a reflow, or another whole-grid invalidation).
    FullDamage,
}

impl FullRefillCause {
    /// Every cause, in discriminant order; the tallies and the metrics wire format are built from it.
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

/// How [`Terminal::cell_frame_damage_scoped_into`](crate::terminal::Terminal::cell_frame_damage_scoped_into) refilled a scratch, so callers and tests can assert a workload reaches the scoped arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRefill {
    /// The whole viewport was re-extracted (continuity break, full damage, first fill).
    Full {
        /// The first continuity clause that refused the scoped arm.
        cause: FullRefillCause,
    },
    /// Only the damage tracker's rows were re-extracted; the rest are byte-identical by the continuity invariant.
    Scoped {
        /// Viewport rows re-resolved (the tracker's damaged rows, clamped). 0 is legal: a cursor-only move still restamps the cursor and selection scalars.
        rows_refilled: usize,
    },
}

impl Clone for RenderInput {
    /// A deep copy of every field, `snapshot_seq` and the other metadata included, but
    /// [`Self::chrome_icon_spares`]: the new value starts with none, because those lists are
    /// destination-local scratch.
    #[inline]
    fn clone(&self) -> Self {
        let mut out = Self::empty();
        out.copy_from(self, true);
        out
    }

    /// Capacity-reusing in-place update: afterwards every field, metadata included, holds what
    /// `*self = source.clone()` would leave in it, but [`Self::chrome_icon_spares`], which keeps
    /// the destination's own lists.
    #[inline]
    fn clone_from(&mut self, source: &Self) {
        self.copy_from(source, true);
    }
}

/// Replace one resolved extra only when its payload changed, then advance the live prefix.
pub(crate) fn reuse_frame_extra<T: ?Sized>(
    out: &mut Vec<(usize, Box<T>)>,
    written: &mut usize,
    col: usize,
    same: impl FnOnce(&T) -> bool,
    make: impl FnOnce() -> Box<T>,
) {
    if let Some((old_col, payload)) = out.get_mut(*written) {
        if !same(payload.as_ref()) {
            *payload = make();
        }
        *old_col = col;
    } else {
        out.push((col, make()));
    }
    *written += 1;
}

fn clone_box_extras<T: ?Sized + PartialEq>(
    target: &mut Vec<(usize, Box<T>)>,
    source: &[(usize, Box<T>)],
) where
    Box<T>: Clone,
{
    let mut written = 0;
    for (col, payload) in source {
        reuse_frame_extra(
            target,
            &mut written,
            *col,
            |old| old == payload.as_ref(),
            || payload.clone(),
        );
    }
    target.truncate(written);
}

fn clone_box_rows<T: ?Sized + PartialEq>(
    target: &mut Vec<Vec<(usize, Box<T>)>>,
    source: &[Vec<(usize, Box<T>)>],
) where
    Box<T>: Clone,
{
    target.resize_with(source.len(), Vec::new);
    for (target, source) in target.iter_mut().zip(source) {
        clone_box_extras(target, source);
    }
}

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
            && self.cursor_glow_add_ramps == other.cursor_glow_add_ramps
            && self.glow_halo == other.glow_halo
            && self.glow_under == other.glow_under
            && self.glow_under_ramps == other.glow_under_ramps
            && self.fire_patch == other.fire_patch
            && self.cursor_fill_override == other.cursor_fill_override
            && self.bloom_gate == other.bloom_gate
            && self.word_decorations == other.word_decorations
            && self.ink == other.ink
            && self.char_fg == other.char_fg
            && self.fire_halo == other.fire_halo
            && self.nova_add == other.nova_add
            && self.rain_add == other.rain_add
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
    }
}

impl Eq for RenderInput {}

/// [`RenderInput::visual_to_logical`] equality by meaning: a missing row and an empty one are both the identity.
#[inline]
fn visual_to_logical_eq(a: &[Vec<u16>], b: &[Vec<u16>]) -> bool {
    fn row(rows: &[Vec<u16>], r: usize) -> &[u16] {
        rows.get(r).map_or(&[][..], Vec::as_slice)
    }
    (0..a.len().max(b.len())).all(|r| row(a, r) == row(b, r))
}

/// What [`RenderInput::apron_row`] was last extracted under: a fill that finds every term unchanged keeps the row.
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
    /// A fingerprint of every presentation input the row is resolved against that moves no `content_gen` (`Terminal::presentation_fingerprint`).
    pub presentation: u64,
    /// The extracting terminal's identity nonce (`RenderInput::terminal_id`).
    pub terminal: u64,
}

/// The incoming-row apron: the one row just below the viewport, which the sub-row translate reveals ([`RenderInput::apron_row`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApronRow {
    /// `true` when a row lies below the viewport (`display_offset > 0`); the remaining fields are meaningful only then.
    pub present: bool,
    /// The row's resolved cells, exactly as a viewport row's [`RenderInput::cells`] entry.
    pub cells: Vec<RenderCell>,
    /// Its sparse emoji clusters — one [`RenderInput::clusters`] row.
    pub clusters: Vec<(usize, Box<str>)>,
    /// Its sparse combining marks — one [`RenderInput::combining`] row.
    pub combining: Vec<(usize, Box<[char]>)>,
    /// Its DEC line size (`SingleWidth` for a history row).
    pub line_size: LineSize,
    /// The cursor column when the cursor's projected row is exactly this row, else `None`, so the caret rides in with its row.
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
        clone_box_extras(clusters, &source.clusters);
        clone_box_extras(combining, &source.combining);
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

/// Where the producers placed the window-space effect streams (`cursor_glow_add`, `glow_under`, `fire_patch`, `glow_halo`, `fx_clip`) relative to a host row prepend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostRowPixels {
    /// Already placed below the prepended rows (the native window, whose `effects_origin_win` counts them): pixels stay put; only the row tags are re-derived.
    Placed,
    /// Placed without the prepended rows (the web module, whose effects pipeline has one chrome origin): every pixel `y` moves down by `rows · cell_h` first, then the tags are re-derived as under [`Self::Placed`].
    Translate,
}

impl RenderInput {
    /// The pixel-resolution chrome raster of frame row `r`, if the host drew one ([`ChromeRaster`]).
    #[must_use]
    #[inline]
    pub fn chrome_raster(&self, r: usize) -> Option<&ChromeRaster> {
        self.chrome_rasters.iter().find(|m| usize::from(m.row) == r)
    }

    /// Retire every chrome raster, keeping each one's icon list, emptied, for [`Self::chrome_icon_list`]: at most [`Self::rows`] lists in all (a frame holds at most one raster per row; past that bound the oldest spares are dropped first), stacked so they are handed back in their rasters' order, so a host that lays the same icons in the same order again (the band's badges through `aterm_render::band::compose_band`, then the marks) gets each raster's own list back, allocates nothing, and the spares stay bounded.
    #[inline]
    pub fn retire_chrome_rasters(&mut self) {
        let keep = self.rows;
        let current = self
            .chrome_rasters
            .iter()
            .filter(|raster| raster.icons.capacity() > 0)
            .count()
            .min(keep);
        let old_keep = keep - current;
        if self.chrome_icon_spares.len() > old_keep {
            let discard = self.chrome_icon_spares.len() - old_keep;
            drop(self.chrome_icon_spares.drain(..discard));
        }
        let current_at = self.chrome_icon_spares.len();
        for raster in self.chrome_rasters.drain(..) {
            let mut icons = raster.icons;
            if icons.capacity() > 0 && self.chrome_icon_spares.len() < keep {
                icons.clear();
                self.chrome_icon_spares.push(icons);
            }
        }
        // `chrome_icon_list` pops: reverse only the just-retired lists so the next frame's first
        // taker gets the first retired raster's list, its second the second's, and so on. Older
        // unused spares stay underneath.
        self.chrome_icon_spares[current_at..].reverse();
    }

    /// An empty icon list for a new chrome raster: one [`Self::retire_chrome_rasters`] kept (the last retirement's in its rasters' order, then older ones), else a new one.
    #[must_use]
    #[inline]
    pub fn chrome_icon_list(&mut self) -> Vec<ChromeIcon> {
        self.chrome_icon_spares.pop().unwrap_or_default()
    }

    /// Update a damage-cache snapshot, dropping the four large cursor-effect payloads whose producer supplied compact metadata.
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
        self.bloom_gate = source.bloom_gate;

        macro_rules! copy_effect_stream {
            ($damage:ident, $($field:ident),+) => {
                if retain_effect_payloads || !source.$damage.is_valid() {
                    $(self.$field.clone_from(&source.$field);)+
                } else {
                    $(self.$field.clear();)+
                }
                self.$damage.refill_from(&source.$damage);
            };
        }
        copy_effect_stream!(
            cursor_glow_add_damage,
            cursor_glow_add,
            cursor_glow_add_ramps
        );
        copy_effect_stream!(glow_halo_damage, glow_halo);
        copy_effect_stream!(glow_under_damage, glow_under, glow_under_ramps);
        copy_effect_stream!(fire_patch_damage, fire_patch);

        self.word_decorations.clone_from(&source.word_decorations);
        self.ink.clone_from(&source.ink);
        self.char_fg.clone_from(&source.char_fg);
        self.fire_halo.clone_from(&source.fire_halo);
        self.nova_add.clone_from(&source.nova_add);
        self.rain_add.clone_from(&source.rain_add);
        self.fx_layers.clone_from(&source.fx_layers);
        self.fx_occluders.clone_from(&source.fx_occluders);
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
        self.screen_mutations = source.screen_mutations;
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
        clone_box_rows(&mut self.clusters, &source.clusters);
        clone_box_rows(&mut self.combining, &source.combining);
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
        self.drawn_at = source.drawn_at;
        self.process_sequence = source.process_sequence;
        self.input_hot = source.input_hot;
        self.terminal_id = source.terminal_id;
        self.extract_gen = source.extract_gen;
        self.engine_fill_seq = source.engine_fill_seq;
        self.engine_alt = source.engine_alt;
        self.grid_instance = source.grid_instance;
        self.engine_row_order = source.engine_row_order;
        self.row_rev.clone_from(&source.row_rev);
        self.row_rev_lane = source.row_rev_lane;
        self.row_shift = source.row_shift;
        self.shifted_fill_seq = source.shifted_fill_seq;
        self.composed_fill_seq = source.composed_fill_seq;
    }

    /// Rebuild the four compact cursor-effect damage records, after all frontend composition (splices, clipping, chrome suppression).
    pub fn refresh_cursor_effect_damage(&mut self) {
        let rows = self.rows;
        self.cursor_glow_add_damage.refresh_glow(
            &self.cursor_glow_add,
            &self.cursor_glow_add_ramps,
            rows,
        );
        self.glow_halo_damage
            .refresh(&self.glow_halo, rows, |halo| halo.row, hash_rain_halo);
        self.glow_under_damage
            .refresh_glow(&self.glow_under, &self.glow_under_ramps, rows);
        self.fire_patch_damage
            .refresh(&self.fire_patch, rows, |patch| patch.row, hash_fire_patch);
    }

    /// An empty 0×0 snapshot with no allocations.
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
            cursor_glow_add_ramps: GlowRamps::default(),
            glow_halo: Vec::new(),
            glow_under: Vec::new(),
            glow_under_ramps: GlowRamps::default(),
            fire_patch: Vec::new(),
            cursor_glow_add_damage: EffectStreamDamage::default(),
            glow_halo_damage: EffectStreamDamage::default(),
            glow_under_damage: EffectStreamDamage::default(),
            fire_patch_damage: EffectStreamDamage::default(),
            cursor_fill_override: None,
            bloom_gate: BloomGate::Open,
            word_decorations: Vec::new(),
            ink: Vec::new(),
            char_fg: Vec::new(),
            fire_halo: Vec::new(),
            nova_add: Vec::new(),
            rain_add: Vec::new(),
            fx_layers: Vec::new(),
            fx_occluders: Vec::new(),
            fx_prefetch: Vec::new(),
            display_offset: 0,
            base_y: 0,
            absolute_row_revision: 0,
            history_renumber_epoch: 0,
            holder: crate::terminal::HolderView::NONE,
            screen_mutations: 0,
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
            chrome_icon_spares: Vec::new(),
            snapshot_seq: 0,
            content_seq: 0,
            drawn_at: None,
            process_sequence: 0,
            input_hot: false,
            terminal_id: 0,
            extract_gen: 0,
            engine_fill_seq: 0,
            engine_alt: false,
            grid_instance: 0,
            engine_row_order: RowOrder::Logical,
            row_rev: Vec::new(),
            row_rev_lane: 0,
            row_shift: 0,
            shifted_fill_seq: 0,
            composed_fill_seq: 0,
        }
    }

    /// Disown the per-row revision lane: the rows are no longer the ones an engine fill stamped.
    #[inline]
    pub fn invalidate_row_revisions(&mut self) {
        self.row_rev_lane = 0;
        self.row_rev.clear();
        self.row_shift = 0;
        self.shifted_fill_seq = 0;
        self.composed_fill_seq = 0;
    }

    /// Record that a host just prepended `prepended` rows, after every per-row channel shifted and after the `snapshot_seq` bump; `blessed` is what [`host_prepend_blessing`](Self::host_prepend_blessing) said before the prepend.
    #[inline]
    pub fn note_host_row_prepend(&mut self, prepended: usize, blessed: PrependBlessing) {
        self.row_shift = self.row_shift.saturating_add(prepended);
        self.composed_fill_seq = 0;
        if blessed.keeps_lane() {
            self.row_rev.splice(0..0, (0..prepended).map(|_| 0u64));
        } else {
            self.row_rev_lane = 0;
            self.row_rev.clear();
        }
        self.shifted_fill_seq = match blessed {
            PrependBlessing::Disowned => 0,
            PrependBlessing::Shift | PrependBlessing::Lane => self.snapshot_seq,
        };
    }

    /// Bless this snapshot as a committed composed frame, so the next compose may retain rows nothing changed.
    #[inline]
    pub const fn bless_composed_fill(&mut self) {
        self.composed_fill_seq = self.snapshot_seq;
    }

    /// Whether this snapshot is still, cell for cell, the frame [`bless_composed_fill`](Self::bless_composed_fill) last blessed.
    #[must_use]
    #[inline]
    pub const fn composed_fill_intact(&self) -> bool {
        self.composed_fill_seq != 0 && self.snapshot_seq == self.composed_fill_seq
    }

    /// What a host row prepend on this snapshot may keep: the revision lane and its inverse, only its inverse (an engine fill whose lane is off), or neither.
    #[must_use]
    #[inline]
    pub const fn host_prepend_blessing(&self) -> PrependBlessing {
        if self.cells.len() != self.rows
            || self.snapshot_seq != self.shifted_fill_seq
            || (self.row_shift == 0 && self.snapshot_seq != self.engine_fill_seq)
        {
            PrependBlessing::Disowned
        } else if self.row_rev_lane != 0 && self.row_rev.len() == self.rows {
            PrependBlessing::Lane
        } else if self.terminal_id != 0 {
            PrependBlessing::Shift
        } else {
            PrependBlessing::Disowned
        }
    }

    /// Undo a blessed host row prepend ([`PrependBlessing::Shift`] or [`PrependBlessing::Lane`]), restoring the engine fill it was made from, so a tab-strip or message-band window's scratch can take a damage-scoped arm of [`Terminal::cell_frame_damage_scoped_into`](crate::terminal::Terminal::cell_frame_damage_scoped_into), scrolled back or not.
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
        let maps = strip.min(self.visual_to_logical.len());
        self.visual_to_logical.drain(0..maps);
        if self.row_rev.len() >= strip {
            self.row_rev.drain(0..strip);
        }
        self.rows = engine_rows;
        self.row_shift = 0;
        self.snapshot_seq = self.engine_fill_seq;
        self.shifted_fill_seq = self.engine_fill_seq;
        self.composed_fill_seq = 0;
        true
    }

    /// Prepend `strip` host rows (a tab strip, a message band, a presence row).
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
        self.clusters.splice(0..0, (0..strip).map(|_| Vec::new()));
        self.combining.splice(0..0, (0..strip).map(|_| Vec::new()));
        self.visual_to_logical
            .splice(0..0, (0..strip).map(|_| Vec::new()));
        self.images.splice(0..0, (0..strip).map(|_| Vec::new()));
        self.line_sizes
            .splice(0..0, (0..strip).map(|_| LineSize::SingleWidth));
        self.line_size_spans
            .splice(0..0, (0..strip).map(|_| Vec::new()));
        self.default_bg_spans
            .splice(0..0, (0..strip).map(|_| Vec::new()));
        for m in &mut self.chrome_rasters {
            m.row = m
                .row
                .saturating_add(u16::try_from(strip).unwrap_or(u16::MAX));
        }
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
        let dy = u16::try_from(strip * cell_h).unwrap_or(u16::MAX);
        let shift_tag = |y: u16| (usize::from(y).saturating_sub(grid_top) / cell_h.max(1)) as u16;
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
        for r in self
            .cursor_glow_add_ramps
            .items
            .iter_mut()
            .chain(&mut self.glow_under_ramps.items)
        {
            r.y = r.y.saturating_add(pixel_dy);
            r.row = shift_tag(r.y);
        }
        for q in &mut self.fire_patch {
            q.y = q.y.saturating_add(pixel_dy);
            q.base_y = q.base_y.saturating_add(pixel_dy);
            q.row = shift_tag(q.y);
        }
        for c in &mut self.char_fg {
            c.row = c.row.saturating_add(strip as u16);
        }
        for c in &mut self.fire_halo {
            c.row = c.row.saturating_add(strip as u16);
        }
        for h in &mut self.glow_halo {
            h.y = h.y.saturating_add(pixel_dy);
            h.cy = h.cy.saturating_add(pixel_dy);
            h.row = shift_tag(h.y);
        }
        for d in &mut self.word_decorations {
            d.row = d.row.saturating_add(strip as u16);
        }
        for c in &mut self.ink {
            c.row = c.row.saturating_add(strip as u16);
        }
        for q in &mut self.nova_add {
            q.row = q.row.saturating_add(strip as u16);
            q.y = q.y.saturating_add(dy);
        }
        for q in &mut self.rain_add {
            q.row = q.row.saturating_add(strip as u16);
            q.y = q.y.saturating_add(dy);
            q.cy = q.cy.saturating_add(dy);
        }
        let (fx_window_dy, fx_grid_dy) = (i32::from(pixel_dy), i32::from(dy));
        for layer in &mut self.fx_layers {
            let d = match layer.space {
                FxSpace::Window => fx_window_dy,
                FxSpace::Grid => fx_grid_dy,
            };
            layer.offset.1 = layer.offset.1.saturating_add(d);
            layer.clip = layer.clip.translate(0, d);
            // The chrome a layer stands on is named by its occluder, which moves in window pixels.
            layer.chrome = layer.chrome.map(|o| o.translate(0, fx_window_dy));
        }
        for o in &mut self.fx_occluders {
            *o = o.translate(0, fx_window_dy);
        }
        self.rows += strip;
        self.snapshot_seq = self.snapshot_seq.wrapping_add(1);
        self.note_host_row_prepend(strip, blessed);
    }

    /// Whether any selection is live (the scalar one or any pane entry).
    #[must_use]
    #[inline]
    pub const fn any_selection(&self) -> bool {
        self.selection.has_selection() || !self.selections.is_empty()
    }

    /// Frame row `frame_row`'s BiDi visual→logical map ([`visual_to_logical`](Self::visual_to_logical)).
    #[must_use]
    #[inline]
    pub fn visual_to_logical_row(&self, frame_row: usize) -> &[u16] {
        self.visual_to_logical
            .get(frame_row)
            .map_or(&[][..], Vec::as_slice)
    }

    /// The logical frame column of the cell painted at (`frame_row`, `frame_col`): `frame_col` itself unless BiDi reordered that row.
    #[must_use]
    #[inline]
    pub fn logical_col(&self, frame_row: usize, frame_col: usize) -> usize {
        self.visual_to_logical_row(frame_row)
            .get(frame_col)
            .map_or(frame_col, |&l| usize::from(l))
    }

    /// Where frame row `frame_row` paints the cell of logical column `logical`, the inverse of [`Self::logical_col`].
    #[must_use]
    #[inline]
    pub fn visual_col(&self, frame_row: usize, logical: usize) -> usize {
        let map = self.visual_to_logical_row(frame_row);
        u16::try_from(logical)
            .ok()
            .and_then(|l| map.iter().position(|&m| m == l))
            .unwrap_or(logical)
    }

    /// Which selection paints a frame-space cell, or `None` when unselected.
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

    /// The inclusive selected column span on one frame row, after the viewport projection and any split-composition clip.
    #[must_use]
    #[inline]
    pub fn selection_row_span(&self, frame_row: usize) -> Option<(u16, u16)> {
        self.selection_row_keys(frame_row)
            .map(|key| (key.lo, key.hi))
            .reduce(|(a_lo, a_hi), (b_lo, b_hi)| (a_lo.min(b_lo), a_hi.max(b_hi)))
    }

    /// Every selection's [`SelectionRowKey`] on `frame_row`, in list order (the scalar selection alone when [`selections`](Self::selections) is empty), borrowed without allocating.
    #[inline]
    pub fn selection_row_keys(
        &self,
        frame_row: usize,
    ) -> impl Iterator<Item = SelectionRowKey> + '_ {
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

    /// A test's read of the whole additive glow stream: `cursor_glow_add` with every lit ramp row the quad it stands for, in draw order.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn glow_add_laid(&self) -> Vec<GlowQuad> {
        let mut all = Vec::new();
        self.cursor_glow_add_ramps
            .expand_into(&self.cursor_glow_add, &mut all);
        all
    }

    /// A test's read of the whole under-ink glow stream: `glow_under` with every lit ramp row the quad it stands for, in draw order.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn glow_under_laid(&self) -> Vec<GlowQuad> {
        let mut all = Vec::new();
        self.glow_under_ramps
            .expand_into(&self.glow_under, &mut all);
        all
    }

    /// Drop every host-owned visual effect layer, leaving only terminal cell content (grid, cursor position/colour, selection, images).
    pub fn clear_overlays(&mut self) {
        self.cursor_effect_style_override = None;
        self.cursor_trail.clear();
        self.cursor_glow_add.clear();
        self.cursor_glow_add_ramps.clear();
        self.glow_halo.clear();
        self.glow_under.clear();
        self.glow_under_ramps.clear();
        self.fire_patch.clear();
        self.cursor_glow_add_damage = EffectStreamDamage::default();
        self.glow_halo_damage = EffectStreamDamage::default();
        self.glow_under_damage = EffectStreamDamage::default();
        self.fire_patch_damage = EffectStreamDamage::default();
        self.cursor_fill_override = None;
        self.bloom_gate = BloomGate::Open;
        self.word_decorations.clear();
        self.ink.clear();
        self.char_fg.clear();
        self.fire_halo.clear();
        self.nova_add.clear();
        self.rain_add.clear();
        self.fx_layers.clear();
        self.fx_occluders.clear();
        self.fx_prefetch.clear();
        self.wallpaper = None;
    }

    /// The emoji grapheme-cluster string at viewport cell `(row, col)`, if the frame captured one (a ZWJ / skin-tone / keycap sequence).
    #[must_use]
    #[inline]
    pub fn cluster_at(&self, row: usize, col: usize) -> Option<&str> {
        let row = self.clusters.get(row)?;
        row.binary_search_by_key(&col, |(c, _)| *c)
            .ok()
            .map(|i| row[i].1.as_ref())
    }

    /// The combining marks to overlay at viewport cell `(row, col)`, if any (binary search, as [`cluster_at`](Self::cluster_at)).
    #[must_use]
    #[inline]
    pub fn combining_at(&self, row: usize, col: usize) -> Option<&[char]> {
        let row = self.combining.get(row)?;
        row.binary_search_by_key(&col, |(c, _)| *c)
            .ok()
            .map(|i| row[i].1.as_ref())
    }

    /// The live default background governing viewport cell `(row, col)`.
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

    /// The topmost inline-image layer covering viewport cell `(row, col)`, if any ([`images_at`](Self::images_at) has them all).
    #[must_use]
    #[inline]
    pub fn image_at(&self, row: usize, col: usize) -> Option<&aterm_grid::ImageRef> {
        self.images_at(row, col).last().map(|(_, image)| image)
    }

    /// Every inline-image layer covering viewport cell `(row, col)`, bottom first — the order both renderers draw them in.
    #[must_use]
    #[inline]
    pub fn images_at(&self, row: usize, col: usize) -> &[(usize, aterm_grid::ImageRef)] {
        self.images
            .get(row)
            .map_or(&[], |row| images_in_row_at(row, col))
    }

    /// The DEC line-size run governing (`row`, `col`).
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
        let gap = (LineSize::SingleWidth, 0, self.cols);
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

    /// Whether an image layer at (`row`,`col`) is drawn over the text (`z_index >= 0`, the default) and so hides the glyph.
    #[must_use]
    #[inline]
    pub fn image_hides_glyph_at(&self, row: usize, col: usize) -> bool {
        self.images_at(row, col)
            .iter()
            .any(|(_, image)| image.image.z_index >= 0)
    }

    /// Composite an IME preedit (the in-flight composition) into this frame at the cursor.
    pub fn overlay_ime_preedit(&mut self, preedit: &str, caret_byte: Option<usize>, cjk: bool) {
        if preedit.is_empty() || self.cursor_row >= self.rows {
            return;
        }
        let row = self.cursor_row;
        let cols = self.cols;
        let implicit = self.implicit_blank_at(row, self.cursor_col);
        let Some(cells) = self.cells.get_mut(row) else {
            return;
        };
        let seed = cells
            .get(self.cursor_col)
            .copied()
            .or(implicit)
            .or_else(|| cells.last().copied())
            .unwrap_or_default();
        let pad = RenderCell {
            ch: ' ',
            fg: seed.fg,
            bg: seed.bg,
            ..RenderCell::default()
        };
        let start_col = self.cursor_col;
        let straddled_lead =
            (start_col > 0 && cells.get(start_col).is_some_and(|c| c.wide)).then(|| start_col - 1);
        let (end_col, caret_col) = walk_ime_preedit(
            preedit,
            caret_byte,
            cjk,
            start_col,
            cols,
            |ch, col, width| {
                // Materialize the ragged tail through this character's cells; the walk proves `col + width <= cols`.
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
        if end_col > start_col {
            let mut lo = start_col;
            let mut hi = end_col;
            if let Some(lead) = straddled_lead
                && let Some(cell) = cells.get_mut(lead)
            {
                *cell = pad;
                lo = lead;
            }
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

    /// Paint one predictive-echo ghost.
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

    /// Drop the grapheme cluster and combining marks of frame cell (`row`, `frame_col`): both renderers prefer them to the cell's `ch`, so a cell a prediction repaints would draw the old grapheme over the new one.
    #[inline]
    fn drop_marks(&mut self, row: usize, frame_col: usize) {
        if let Some(clusters) = self.clusters.get_mut(row) {
            clusters.retain(|&(c, _)| c != frame_col);
        }
        if let Some(combining) = self.combining.get_mut(row) {
            combining.retain(|&(c, _)| c != frame_col);
        }
    }

    /// Paint a predictive-echo SPAN, `aterm-predict`'s state chain on one row.
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

    /// Whether [`Self::paint_prediction_span`] would paint `cells` on `row`.
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

    /// Whether [`Self::paint_prediction_ghost`] would paint a ghost at logical column `col` of frame row `row`.
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

    /// The frame's implicit blank at viewport cell (`row`, `col`): a space in the live default colours (per pane on a composed frame), as the terminal paints an unwritten column.
    #[inline]
    fn implicit_blank_at(&self, row: usize, col: usize) -> Option<RenderCell> {
        let (fg, bg) = (self.default_fg, self.default_bg_at(row, col));
        if fg == COLOR_UNSET || bg == COLOR_UNSET {
            return None;
        }
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
        if width == 0 {
            continue;
        }
        if col + width > cols {
            break;
        }
        emit(ch, col, width);
        col += width;
    }
    (col, caret_col.unwrap_or(col).min(cols.saturating_sub(1)))
}

/// The composition's caret column: what [`RenderInput::overlay_ime_preedit`] leaves in `cursor_col`, without painting.
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

/// The entries of one row's sparse image list (`RenderInput::images[row]`, ascending by column) that cover column `col`, bottom layer first.
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
#[path = "render_ime_preedit_tests.rs"]
mod ime_preedit_tests;

/// Overlay tests on real `Terminal` extractions, whose rows are ragged: a row holds only its written prefix, and at a shell prompt the cursor sits at its end.
#[cfg(test)]
#[path = "render_ime_preedit_ragged_row_tests.rs"]
mod ime_preedit_ragged_row_tests;

/// Edge cases of row materialization, driven from real `Terminal` extractions.
#[cfg(test)]
#[path = "render_ime_preedit_neighbour_tests.rs"]
mod ime_preedit_neighbour_tests;

/// [`ime_preedit_caret_col`] (the candidate-window anchor) and [`RenderInput::overlay_ime_preedit`] (the ink) agree on every shape the shared `walk_ime_preedit` has a branch for.
#[cfg(test)]
#[path = "render_ime_preedit_caret_query_tests.rs"]
mod ime_preedit_caret_query_tests;

/// One cell of a predictive-echo span ([`RenderInput::paint_prediction_span`]): what `aterm-predict`'s state chain shows there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanCell {
    /// A glyph a key typed, whose echo has not come: painted as a ghost.
    Typed(char),
    /// The glyph `ch` the frame shows at logical column `from` of the same row, moved here by a shift: painted as that cell, in its own style.
    Moved {
        /// The column the glyph stands on in the unpainted frame.
        from: usize,
        /// The glyph the predictor expects there.
        ch: char,
    },
    /// A cell an erase vacated: the terminal's blank.
    Vacated,
}

/// `cell` repainted as a predictive-echo ghost of `ch`: the glyph, its foreground dimmed halfway toward the cell's background (tentative, legible on any theme), its background kept, inherited SGR and geometry dropped.
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
#[path = "render_prediction_ghost_tests.rs"]
mod prediction_ghost_tests;

#[cfg(test)]
#[path = "render_line_size_run_tests.rs"]
mod line_size_run_tests;

#[cfg(test)]
#[path = "render_z_index_tests.rs"]
mod z_index_tests;

#[cfg(test)]
#[path = "render_cursor_override_tests.rs"]
mod cursor_override_tests;

#[cfg(test)]
#[path = "render_default_bg_span_tests.rs"]
mod default_bg_span_tests;

#[cfg(test)]
#[path = "render_selection_clip_tests.rs"]
mod selection_clip_tests;

#[cfg(test)]
#[path = "render_rain_channel_tests.rs"]
mod rain_channel_tests;

#[cfg(test)]
#[path = "render_host_row_prepend_tests.rs"]
mod host_row_prepend_tests;

#[cfg(test)]
#[path = "render_fx_input_tests.rs"]
mod fx_input_tests;

#[cfg(test)]
#[path = "render_band_icon_tests.rs"]
mod band_icon_tests;
