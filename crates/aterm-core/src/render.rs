// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The engine-owned render snapshot (the `read_image` boundary).
//!
//! [`RenderInput`] is the plain-owned, `Terminal`-free value a renderer reads to
//! paint one frame. Historically the CPU renderer reached into `&Terminal`
//! internals to build it (`Renderer::extract_into`); A-3 inverts that boundary so
//! the ENGINE produces the snapshot ([`Terminal::cell_frame_into`]) and the
//! renderer becomes a PURE consumer of this value — no `&Terminal`, no reach into
//! core. Hosting the type HERE lets `aterm-core` build the snapshot without a
//! dependency cycle; `aterm_render::render_api` re-exports it as
//! `aterm_render::RenderInput`.

use crate::grid::LineSize;
use crate::selection::TextSelection;
use crate::terminal::{CursorStyle, RenderCell};
use aterm_hash::FxHashMap;

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
    /// Blend coverage `0..=255` of the trail colour over this cell's background
    /// (`0` = invisible, `255` = solid). Drawn opaquely as the pre-blended
    /// result, so the CPU fill and the GPU `REPLACE` quad land the same pixel.
    pub alpha: u8,
}

/// One light quad of the cursor aurora: a fragment of emitted light (comet
/// body, bloom-crown slab, landing-ring strip, or a spark particle).
///
/// The host pre-multiplies the colour by its coverage, and in additive mode the
/// renderer does a per-channel saturating add: order-independent and byte-exact
/// between the GPU (`One`/`One` over the linear `Rgba8Unorm` target) and the
/// CPU (`add_sat`). Additive light only brightens, so text stays legible.
///
/// Invariants the host upholds:
/// (1) coordinates are per stream: the cursor-effect streams
/// (`cursor_glow_add`, `glow_under`) are window-absolute pixels (the producer
/// folds in the grid origin and clamps to the effects box, the grid plus the
/// chrome head band), while the grid-anchored `nova_add` stream is
/// grid-interior (the renderer offsets it like other grid streams);
/// (2) every quad lies within exactly one grid-anchored cell-row band (a halo
/// spilling into a neighbour row is a separate quad), so the row-scoped dirty
/// gate and GPU scissor cover it. `row` is a grid-row damage hint; above-grid
/// quads tag row 0.
///
/// A quad composites additively (`alpha == 0`, the default) or source-over
/// (`alpha > 0`); see [`GlowQuad::alpha`]. Both are one blend equation, so the
/// renderers need no second pipeline, pass or reorder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlowQuad {
    /// The viewport row band this quad lives in: a grid-row damage hint for
    /// the dirty gate (above-grid quads tag row 0).
    pub row: u16,
    /// Pixel X in this stream's coordinate space. Cursor-effect streams are
    /// window-absolute; grid-anchored streams such as `nova_add` are
    /// grid-interior, as documented on [`GlowQuad`].
    pub x: u16,
    /// Pixel Y in this stream's coordinate space (window-absolute or
    /// grid-interior according to the owning stream).
    pub y: u16,
    /// Quad width in pixels.
    pub w: u16,
    /// Quad height in pixels (kept within the single `row` cell band).
    pub h: u16,
    /// Premultiplied light colour `0x00RRGGBB`, the colour times the coverage
    /// that produced it, in both compositing modes (so
    /// [`alpha`](GlowQuad::alpha) is a separate field).
    pub color: u32,
    /// The source-over opacity, `0..=255`, which also selects the mode. Both
    /// modes are `out = color + dst·(255 − alpha)/255`:
    ///
    /// * `alpha == 0`: premultiplied additive light, `out = dst + color`
    ///   (saturating). The default: the destination keeps its luminance and
    ///   the quad only brightens; order-independent.
    /// * `alpha > 0`: premultiplied source-over paint: the destination is
    ///   attenuated by `1 − alpha/255` before the light lands.
    ///
    /// An additive bed is charged the ground's luminance against its
    /// legibility budget (`ground + light`); source-over spends
    /// `ground·(1 − a)`, allowing more saturation at the same certified
    /// contrast. Additive light is invisible
    /// over white and never darkens; source-over reads on any ground and can
    /// darken one brighter than itself, so a stream that must never darken
    /// stays at `0`.
    pub alpha: u8,
    /// Premultiplied light colour `0x00RRGGBB` at the quad's right edge, the
    /// second end of a horizontal gradient across the quad's `w` columns;
    /// [`color`](GlowQuad::color) is the left edge. A quad with
    /// `color2 == color` and `alpha2 == alpha` is flat. Column `i` (0-based) of
    /// a `w`-wide quad carries, per channel and for the opacity alike,
    ///
    /// ```text
    /// c(i) = (c0·(2w − m) + c1·m + w) / (2w),   m = 2i + 1
    /// ```
    ///
    /// in unsigned integer arithmetic (round half up): the line from `c0` at
    /// the left edge to `c1` at the right edge sampled at the column centre.
    /// `c0 == c1` gives `c(i) == c0` everywhere; the ramp is monotone; `w == 1`
    /// is `(c0 + c1 + 1) / 2`. The CPU rasterizer and both GPU fragment shaders
    /// compute this same integer formula (never a float interpolant), keeping
    /// the backends byte-exact. It makes colour continuous inside a quad (e.g.
    /// a whole-cell rainbow ribbon slab) without adding quads.
    pub color2: u32,
    /// The source-over opacity at the quad's right edge (the ramp's second
    /// end; [`alpha`](GlowQuad::alpha) is the left). The mode is per quad, not
    /// per end: `alpha == 0 ⇔ alpha2 == 0` (debug-asserted by
    /// [`GlowQuad::flat`] and [`GlowQuad::gradient`]). Since the colour is
    /// premultiplied by the same coverage, `c(i) → 0` as `a(i) → 0`, so the
    /// composite is continuous where a ramp reaches `0`.
    pub alpha2: u8,
}

impl GlowQuad {
    /// A flat quad: one colour and one opacity across its width
    /// (`color2 == color`, `alpha2 == alpha`).
    #[must_use]
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

    /// A gradient quad: `(color, alpha)` at the left edge ramping to
    /// `(color2, alpha2)` at the right edge (see [`GlowQuad::color2`]).
    ///
    /// # Panics (debug)
    /// When exactly one of `alpha`/`alpha2` is `0`: a quad is additive at both
    /// ends or source-over at both ends (see [`GlowQuad::alpha2`]).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn gradient(
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
        debug_assert_eq!(
            alpha == 0,
            alpha2 == 0,
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
    pub const fn is_flat(self) -> bool {
        self.color2 == self.color && self.alpha2 == self.alpha
    }

    /// Canonical lossless words used by cursor-effect fingerprints. A flat quad
    /// folds to two words; a gradient quad appends a third, tagged word with
    /// its right edge (`color2 | alpha2 << 32 | GRADIENT_TAG`), so a gradient
    /// never folds as its left-flat twin.
    #[doc(hidden)]
    #[must_use]
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

/// The fingerprint words of one [`GlowQuad`] — two for a flat quad, three for
/// a gradient (see [`GlowQuad::damage_words`]). Iterates the live words only.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlowQuadWords {
    words: [u64; 3],
    len: u8,
}

impl GlowQuadWords {
    /// Bits 48..64 of a gradient quad's third word (`"GR"`), disjoint from
    /// `color2` (bits 0..24) and `alpha2` (32..40). A hash-mixing aid (the
    /// stream's length prefix bounds the fold) that keeps the third word from
    /// resembling a following flat quad's `(row, x, y, w)`.
    const GRADIENT_TAG: u64 = 0x4752_0000_0000_0000;

    /// The live words as a slice.
    #[must_use]
    pub fn as_slice(&self) -> &[u64] {
        &self.words[..self.len as usize]
    }
}

impl IntoIterator for GlowQuadWords {
    type Item = u64;
    type IntoIter = std::iter::Take<std::array::IntoIter<u64, 3>>;

    fn into_iter(self) -> Self::IntoIter {
        self.words.into_iter().take(self.len as usize)
    }
}

/// Which compositing mode a [`GlowQuad`] emitter stamps on its quads: the name
/// of the [`GlowQuad::alpha`] convention, so a rasterizer shared by additive
/// and source-over streams is told which it is producing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlowBlend {
    /// Premultiplied additive light: every quad has `alpha == 0`. The default.
    #[default]
    Add,
    /// Premultiplied source-over paint: every quad carries the coverage that
    /// premultiplied its colour as its `alpha`.
    Over,
}

/// How a [`RainHalo`]'s radially-weighted colour composites onto the frame.
///
/// [`Add`](HaloMode::Add) is premultiplied light, invisible over white.
/// [`Over`](HaloMode::Over) makes the quad a source-over veil whose per-pixel
/// opacity is the radial falloff weight, so smoke or steam reads on any
/// background. Within one stream every `Add` quad composites before every
/// `Over` quad (the GPU's per-mode draw order; the CPU mirrors it), keeping
/// mixed-mode overlaps parity-exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HaloMode {
    /// Premultiplied saturating-add light (CPU `add_sat` == GPU One/One on the
    /// Unorm view).
    #[default]
    Add,
    /// Source-over veil: `color` is straight (unpremultiplied) RGB and the
    /// radial falloff weight is the per-pixel alpha (centre most opaque,
    /// fading to 0 at the radii), readable on any background.
    Over,
}

/// A rain bright-head halo: like [`GlowQuad`] (a single-row-band quad) but its
/// light falls off radially from the head centre. `color` is the peak; each
/// covered pixel scales it by an integer elliptical falloff,
/// `weight = ((256 − nsq)² >> 8)` clamped with
/// `nsq = (dx²·256)/rx² + (dy²·256)/ry²` for the pixel's offset `(dx, dy)` from
/// `(cx, cy)`, then `add_sat`s it (in [`HaloMode::Over`], the weight is the
/// per-pixel source-over alpha of the straight `color`). Pure integer math, so
/// the CPU rasterizer and the GPU `fs_rain_glow`/`fs_rain_glow_over` shaders
/// agree byte for byte. Coordinates follow the [`GlowQuad`] per-stream rule:
/// cursor-effect streams (`glow_halo`) are window-absolute, the grid-anchored
/// `rain_add` stream grid-interior. `row` is a grid-row damage hint
/// (above-grid quads tag row 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RainHalo {
    /// The viewport row band this quad lives in: a grid-row damage hint for
    /// the dirty gate (above-grid quads tag row 0).
    pub row: u16,
    /// Pixel X of the quad (window-absolute for cursor-effect streams;
    /// grid-interior for `rain_add` — see the struct doc).
    pub x: u16,
    /// Pixel Y of the quad (same convention as `x`).
    pub y: u16,
    /// Quad width in pixels.
    pub w: u16,
    /// Quad height in pixels (kept within the single `row` cell band).
    pub h: u16,
    /// The halo-centre colour `0x00RRGGBB`: in [`HaloMode::Add`] the
    /// premultiplied peak light, scaled per pixel by the falloff; in
    /// [`HaloMode::Over`] a straight veil colour whose opacity the falloff
    /// scales.
    pub color: u32,
    /// Pixel X of the halo centre (falloff origin; same convention as `x`).
    /// May lie outside this quad's row band (a halo spans up to 3).
    pub cx: u16,
    /// Pixel Y of the halo centre (same convention as `y`).
    pub cy: u16,
    /// Horizontal elliptical falloff half-extent in pixels — light reaches 0 at
    /// this radius. `>= 1` (the emitter guarantees it; 0 would divide by zero).
    pub rx: u16,
    /// Vertical elliptical falloff half-extent in pixels — light reaches 0 at
    /// this radius. `>= 1` (the emitter guarantees it; 0 would divide by zero).
    pub ry: u16,
    /// How the radially-weighted colour composites (defaults to
    /// [`HaloMode::Add`]; [`HaloMode::Over`] is the light-theme veil).
    pub mode: HaloMode,
}

impl RainHalo {
    /// Canonical lossless words used by cursor-effect fingerprints.
    #[doc(hidden)]
    #[must_use]
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

/// How a [`FirePatch`]'s per-pixel field output composites onto the frame.
///
/// [`Add`](FireMode::Add), the dark-theme flame, emits premultiplied light
/// (`palette·coverage`) saturating-added (CPU `add_sat` == GPU One/One on the
/// Unorm view), invisible over white. [`Over`](FireMode::Over), the
/// light-theme flame, uses the same field for a straight ink palette plus
/// per-pixel alpha, composited source-over (CPU `over_rgb` == GPU
/// SrcAlpha/OneMinusSrcAlpha, the [`HaloMode::Over`] contract). Within one
/// stream every `Add` patch composites before every `Over` patch, keeping
/// mixed-mode overlaps parity-exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FireMode {
    /// Premultiplied saturating-add flame light: the dark-theme mode.
    #[default]
    Add,
    /// Source-over ink-fire: straight RGB + field-shaped alpha, readable on
    /// white.
    Over,
}

/// One fire patch: a per-pixel, pure-integer procedural fire field evaluated
/// at every device pixel of the quad by both backends. The shared field
/// function (`aterm_render::fire_field`, mirrored op-for-op by the GPU
/// `fs_fire_add`/`fs_fire_over` WGSL) maps `(window_px, window_py, patch
/// params)` to a palette colour + coverage with no state and no floats, so
/// both backends land the same byte.
///
/// Quads follow the [`RainHalo`] cursor-stream invariants: window-absolute
/// pixels (flames may light the head band above the grid), each within exactly
/// one grid-anchored cell-row band (`row` is a damage hint; above-grid patches
/// tag row 0), so a flame spanning rows is emitted per row. The field depends
/// on absolute pixel coordinates plus the patch parameters, so patches sharing
/// a burn (same `base_y`/`peak_h`/`phase`/`temp`/`lean`/`cell_h`) are seamless,
/// and splitting a patch changes no byte. `base_y` may lie outside the patch's
/// own row band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FirePatch {
    /// The viewport row band this quad lives in: a grid-row damage hint for
    /// the dirty gate (above-grid patches tag row 0).
    pub row: u16,
    /// Window-absolute pixel X of the quad (the producer already folded in the
    /// grid origin).
    pub x: u16,
    /// Window-absolute pixel Y of the quad.
    pub y: u16,
    /// Quad width in pixels.
    pub w: u16,
    /// Quad height in pixels (kept within the single `row` cell band).
    pub h: u16,
    /// Window-absolute pixel Y of the flame root (field `v = 0`; flames rise,
    /// so live pixels have `y < base_y`). Shared across a burn's patches so the
    /// field is continuous vertically.
    pub base_y: u16,
    /// Maximum flame height in pixels — the tongue-height envelope at this
    /// patch. `0` is treated as `1` by the field (total function).
    pub peak_h: u16,
    /// Churn phase in units of 1/1024 s, quantized by the producer so both
    /// backends see the identical time. All patches of one burn share it.
    pub phase: u32,
    /// Display temperature `0..=255`: palette reach (white confined to the
    /// root at high temp), coverage density, and churn/rise speed.
    pub temp: u8,
    /// Per-cell envelope `0..=255` (head→tail falloff arrives through this);
    /// scales the tongue height.
    pub strength: u8,
    /// Horizontal shear: the field's sample column drifts `lean/4` px per full
    /// rise (tongues drag against the typing direction). Signed.
    pub lean: i8,
    /// Readability ceiling on emitted coverage/alpha `0..=255` — text under
    /// the fire stays legible.
    pub cov_cap: u8,
    /// The cell height in pixels: the field's spatial unit (DPI-independent
    /// flame anatomy). `< 2` is clamped to 2 by the field (total function).
    pub cell_h: u16,
    /// How the field output composites (defaults to the dark-theme
    /// [`FireMode::Add`]; [`FireMode::Over`] is the light-theme ink-fire).
    pub mode: FireMode,
}

impl FirePatch {
    /// Canonical lossless words used by cursor-effect fingerprints.
    #[doc(hidden)]
    #[must_use]
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

/// Compact, producer-built damage identity for a window-absolute cursor-effect
/// stream.
///
/// The cursor streams can hold thousands of single-row quads. A prior-frame
/// damage cache only needs to know whether the stream changed and which rows
/// its previous pixels occupied, so it keeps this instead of cloning and
/// re-walking the payload.
///
/// Opt-in: builders may leave it at [`Default`], and consumers then fall back
/// to comparing and scanning the vector. The GUI calls
/// [`RenderInput::refresh_cursor_effect_damage`] after its final effect
/// composition, so the metadata describes the exact presented payload.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectStreamDamage {
    /// A two-lane content fingerprint. The lanes use distinct update
    /// equations (not two affine transforms of one hash); the explicit length
    /// prevents an empty/non-empty alias.
    fingerprint: u128,
    len: usize,
    /// One bit per grid row touched by the stream: compact even for an
    /// 8k-quad ribbon, and it retains vacated rows after the cache drops the
    /// payload.
    rows: Vec<u64>,
    valid: bool,
}

impl EffectStreamDamage {
    /// `clone_from` that keeps `rows`' capacity (a derived one is
    /// `*self = source.clone()`, a fresh Vec on every per-frame cache copy).
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
    pub const fn is_valid(&self) -> bool {
        self.valid
    }

    /// Whether the stream this metadata describes was empty.
    ///
    /// The only reliable answer for a cached frame:
    /// [`RenderInput::clone_damage_cache_from`] drops the payload of every
    /// stream with valid metadata, so the cached `Vec` is empty regardless.
    /// `None` for hand-built inputs, whose payload was cloned and answers
    /// directly.
    #[must_use]
    pub const fn was_empty(&self) -> Option<bool> {
        if self.valid {
            Some(self.len == 0)
        } else {
            None
        }
    }

    /// Returns a content verdict only when both producers supplied trusted
    /// metadata.  `None` is the compatibility path for hand-built inputs.
    #[must_use]
    pub fn same_content(&self, other: &Self) -> Option<bool> {
        (self.valid && other.valid)
            .then_some(self.len == other.len && self.fingerprint == other.fingerprint)
    }

    /// Mark the previous/current row set into a renderer's dirty scratch. A
    /// stale or malformed row bit is ignored by the bounds check.
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

/// A small fixed-shape fingerprint for effect records, not a general hasher:
/// the producer writes a known sequence of scalars. The two lanes differ in
/// input combination and constants, avoiding the affine relation of
/// `FxHasher::with_seed` used twice on equal-length records.
struct EffectStreamFingerprint {
    first: u64,
    second: u64,
}

impl EffectStreamFingerprint {
    const FIRST_SEED: u64 = 0x243f_6a88_85a3_08d3;
    const SECOND_SEED: u64 = 0x1319_8a2e_0370_7344;
    const FIRST_MUL: u64 = 0x9e37_79b1_85eb_ca87;
    const SECOND_MUL: u64 = 0xc2b2_ae3d_27d4_eb4f;

    fn new(len: usize) -> Self {
        let mut fingerprint = Self {
            first: Self::FIRST_SEED,
            second: Self::SECOND_SEED,
        };
        fingerprint.write_pair(
            u64::try_from(len).unwrap_or(u64::MAX),
            0x6c65_6e67_7468_0001,
        );
        fingerprint
    }

    #[inline]
    fn write_pair(&mut self, first_word: u64, second_word: u64) {
        // Both recurrences consume both packed words; keeping the lanes
        // independent lets their multiplies issue together.
        self.first =
            (self.first ^ first_word ^ second_word.rotate_left(23)).wrapping_mul(Self::FIRST_MUL);
        self.second = (self.second ^ second_word ^ first_word.rotate_right(17))
            .wrapping_mul(Self::SECOND_MUL);
    }

    fn finish(self) -> u128 {
        (u128::from(self.first) << 64) | u128::from(self.second)
    }
}

#[inline]
const fn pack_u16x4(a: u16, b: u16, c: u16, d: u16) -> u64 {
    a as u64 | (b as u64) << 16 | (c as u64) << 32 | (d as u64) << 48
}

fn hash_glow_quad(quad: &GlowQuad, hasher: &mut EffectStreamFingerprint) {
    let GlowQuadWords {
        words: [first, second, third],
        len,
    } = quad.damage_words();
    hasher.write_pair(first, second);
    // A gradient's right edge rides a third word; a flat quad writes exactly
    // the pair it always did (see `GlowQuad::damage_words`).
    if len == 3 {
        hasher.write_pair(third, 0);
    }
}

fn hash_rain_halo(halo: &RainHalo, hasher: &mut EffectStreamFingerprint) {
    let [first, second, third] = halo.damage_words();
    hasher.write_pair(first, second);
    hasher.write_pair(third, 0);
}

fn hash_fire_patch(patch: &FirePatch, hasher: &mut EffectStreamFingerprint) {
    let [first, second, third, fourth] = patch.damage_words();
    hasher.write_pair(first, second);
    hasher.write_pair(third, fourth);
}

/// One textured sprite quad for a renderer-owned animated overlay: a rectangle
/// sampled from an RGBA8 atlas and composited source-over (straight alpha), the
/// blend the inline-image / colour-emoji / settings-tray paths use, so CPU and
/// GPU land the same pixel.
///
/// The host owns the art and animation and hands over a resolved list each
/// frame. It upholds [`GlowQuad`]'s dirty-gate invariants: (1) the dest rect
/// is in grid-interior pixels (the renderer offsets by `pad`), clamped to the
/// grid; (2) every quad lies within exactly one cell-row band (`row`), so a
/// sprite spanning rows is emitted as per-row slices. Atlas coordinates are
/// integer texels, keeping the type `Copy + Eq` for byte-exact damage
/// comparison.
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
    /// Mirror the sampled sprite horizontally (a creature facing left vs right from one
    /// baked pose). The renderer samples `u → 1-u`; CPU and GPU honor it identically.
    pub flip_x: bool,
}

/// One free-floating decorative sprite: an arbitrary pixel rectangle at any
/// position, even off-grid, with no row tag. Unlike [`SpriteQuad`] its full
/// `[y, y+h)` extent drives dirty tracking (every overlapped row band,
/// prev∪cur), so it may span rows without host-side slicing.
///
/// The dest origin is grid-interior pixels (the renderer offsets by `pad`) and
/// signed, so a cat peeking in from outside the grid can have a negative or
/// past-the-edge origin. Extents and atlas coordinates are non-negative
/// integer texels, keeping `Copy + Eq` as for [`SpriteQuad`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FreeSprite {
    /// Grid-interior pixel X of the dest rect (pad-relative; may be negative
    /// for an off-grid peek).
    pub x: i32,
    /// Grid-interior pixel Y of the dest rect (pad-relative; may be negative —
    /// a sprite hanging in from above row 0 — or extend past the bottom edge).
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
    /// Sampling regime (v1 renderers consume `Nearest` only; `Linear` is deferred).
    pub sampler: FreeSampler,
}

/// Where a [`FreeSprite`] sits relative to the terminal text (painter's-order
/// slot, not a depth test). Participates in the byte-exact damage compare, so a
/// same-rect z flip is a real content change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FreeZ {
    /// Drawn after the cell backgrounds / under-text sprites, before glyphs.
    #[default]
    UnderText,
    /// Drawn after glyphs and word decorations, before the cursor.
    OverText,
}

/// How a [`FreeSprite`] samples its atlas. Renderers implement `Nearest` only
/// (the host bakes at exact 1:1 size); `Linear` is carried in the type and the
/// damage compare but not yet rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FreeSampler {
    /// Nearest 1:1 (cats).
    #[default]
    Nearest,
    /// Bilinear (deferred — scaled sprite art).
    Linear,
}

/// An RGBA8 sprite atlas carried with a frame's [`SpriteQuad`]s. Shared by
/// `Arc` so cloning a [`RenderInput`] is cheap; the monotonic `version` makes
/// the GPU re-upload only when the atlas changes. Straight-alpha, row-major,
/// length `width*height*4`.
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

/// Which small procedural sprite a [`WordDecoration`] paints, rasterized by the
/// renderer (not the font) into a 0/255 coverage mask so CPU and GPU land the
/// same pixels.
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
    /// A soft annulus-arc ring segment filling the cell: the singularity nova's
    /// darkening ring. The nova's additive streams can only brighten, so the
    /// ring is stamped as one `DecoBlend::Over` decoration per covered cell,
    /// tinted dark. Rasterized by `deco_coverage` from the cell geometry, so
    /// CPU and GPU share the mask.
    RingArc,
    /// A full-cell soft-edged square: the super nova's dark veil for light
    /// backgrounds (additive light is invisible there), stamped as one
    /// `DecoBlend::Over` decoration per covered cell, tinted dark. Full
    /// coverage inside with a ~1 px anti-aliased ramp at the cell borders;
    /// rasterized by `deco_coverage` from the cell geometry.
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

/// One sparkle-word decoration: a small sprite stamped over or near a cell of
/// a matched word. The host owns matching and animation and hands over a
/// resolved list each frame; an empty list draws nothing. Render-only: never
/// touches grid cells, copied text or recordings.
///
/// Position is in the viewport coordinates of
/// [`RenderInput::cursor_row`]/[`cursor_col`](RenderInput::cursor_col); the
/// renderer derives the cell origin as for the cursor, then offsets by the
/// sub-cell `(dx, dy)`.
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
    /// Tint colour `0x00RRGGBB` (host-resolved; for `Add` this is the light to
    /// add, for `Over` the stamp colour).
    pub color: u32,
    /// Coverage / opacity `0..=255` of the sprite (the host's animation envelope
    /// already folded in; the renderer multiplies the sprite mask by this).
    pub alpha: u8,
}

/// One animated-ink foreground override for a single cell. `color` is the
/// host-resolved final colour (gradient, specular sweep and legibility guard
/// pre-folded), substituted wherever [`RenderCell::fg`] would tint ink (glyph
/// blit, combining-mark overlays, underline / strikethrough / overline). An
/// explicit SGR 58 `underline_color` still wins. `GlyphImage::Rgba` (colour
/// emoji) blits ignore `fg`, so emoji are untouched.
///
/// Host invariants: [`RenderInput::ink`] is sorted by `(row, col)` with unique
/// cells (renderers walk it in lockstep with their column loops); a wide glyph
/// is governed by its lead cell's entry. An empty list changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InkCell {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: u16,
    /// Viewport column (0-indexed); the lead cell of a wide glyph.
    pub col: u16,
    /// Final `[r, g, b]` to substitute for the cell's resolved `fg`.
    pub color: [u8; 3],
}

/// One effect-owned per-cell glyph foreground override (fire "dark glyph
/// cores": engulfed cells recoloured toward ember-black so the letterform
/// reads as a silhouette in the flame). `fg` is the host-resolved final colour,
/// substituted exactly where an [`InkCell`] would be (SGR 58 still wins),
/// before the minimum-contrast / selection fg floors. When a cell has both an
/// ink entry and a `char_fg` entry, ink wins.
///
/// Host invariants as for [`InkCell`]: [`RenderInput::char_fg`] is sorted by
/// `(row, col)` with unique cells; a wide glyph is governed by its lead cell;
/// colour-emoji blits ignore `fg`. An empty list changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharFg {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: u16,
    /// Viewport column (0-indexed); the lead cell of a wide glyph.
    pub col: u16,
    /// Final `0x00RRGGBB` to substitute for the cell's resolved `fg`.
    pub fg: u32,
}

/// One effect-owned contrast-halo cell (fire text legibility): the engulfment
/// weight for a glyph cell, driving the alpha of the dark warm ring the
/// renderers stamp under the glyph's ink so the letterform separates from the
/// fire. Carries no colour: the ink itself is never recoloured; the halo sits
/// around the strokes. `strength` scales only the ring's opacity (`0` a bare
/// rim, `255` the full separator).
///
/// Host invariants as for [`CharFg`]/[`InkCell`]: [`RenderInput::fire_halo`]
/// is sorted by `(row, col)` with unique cells; a wide glyph is governed by
/// its lead cell; colour-emoji cells draw no mono coverage and are never
/// haloed. A grid stream: the tab-strip splice shifts `row` with the terminal
/// content, like `char_fg`. An empty list changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FireHaloCell {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: u16,
    /// Viewport column (0-indexed); the lead cell of a wide glyph.
    pub col: u16,
    /// Fire engulfment weight for this cell (`0..=255`): scales the contrast
    /// halo's stamp alpha — a lick barely rims the glyph, the wall rims firmly.
    pub strength: u8,
}

/// Sentinel for unresolved live frame colors, meaning "the producer did not
/// choose a value — fall back to the renderer's configured theme." Real frame
/// colors are `0x00RRGGBB` (high byte always 0), so a set high byte is
/// unambiguously metadata rather than a color.
pub const COLOR_UNSET: u32 = 0xFF00_0000;

/// Sentinel for an explicitly dynamic selected-text foreground. Unlike
/// [`COLOR_UNSET`], this says the terminal chose automatic contrast for this
/// frame (for example `OSC 21;selection_foreground=`), so a renderer must not
/// reintroduce its static configured foreground.
pub const COLOR_DYNAMIC: u32 = 0xFE00_0000;

/// One run of columns in a composed row that carries its own DEC line size.
///
/// A single-pane frame uses [`RenderInput::line_sizes`] for the whole row and
/// leaves [`RenderInput::line_size_spans`] empty (allocation-free).
///
/// In a split-pane composite, panes side by side are independent terminals
/// whose lines may differ in DECDWL/DECDHL state; a row-level value would let
/// one pane's line size scale the other's glyphs.
///
/// Columns are composite columns, so `start_col` is also the run's pixel
/// origin in cell units: a glyph at composite column `c` in the run draws at
/// `start_col * cell_w + (c - start_col) * run_cell_w`, clipped to the run's
/// box (for a run at 0, just `c * run_cell_w`).
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
    pub const fn new(start_col: usize, end_col: usize, line_size: LineSize) -> Self {
        Self {
            start_col,
            end_col,
            line_size,
        }
    }
}

/// A host chrome row drawn at pixel resolution: the message band's meter
/// (`docs/DESIGN-unified-messages-2026-09-21.md`). A cell background paints
/// whole cells, so a fill edge inside a cell, a gradient along the row, or a
/// rail in the row's lowest pixels is expressed here; both renderers paint it
/// from the same data.
///
/// Painted after the row's cell backgrounds and gutters, before its glyphs:
/// [`Self::ground`] over the row's full band height (gutters included) except
/// under [`Self::own`] columns (chips, which keep their fill), then
/// [`Self::rail`] in the band's lowest [`Self::rail_h`] pixels. Glyphs then
/// draw normally (lifted clear of the rail when [`Self::clear_rail`] asks),
/// except the one cell [`Self::split`] names, drawn twice under complementary
/// clips: left of the split in the split's ink, the rest in the cell's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChromeRaster {
    /// The frame row (counted from the top of the composed input, like
    /// [`RenderInput::cells`]).
    pub row: u16,
    /// One `0x00RRGGBB` per frame pixel column, `[0, frame_w)`; empty for no
    /// ground (the cells' own backgrounds stand).
    pub ground: std::sync::Arc<[u32]>,
    /// One `0x00RRGGBB` per frame pixel column for the rail band, with
    /// [`ChromeRaster::KEEP`] where the band keeps the ground; empty for no
    /// rail.
    pub rail: std::sync::Arc<[u32]>,
    /// The rail band's height in device pixels, at the bottom of the row.
    pub rail_h: u16,
    /// This row's glyphs keep clear of its rail: the renderer fits the rail and
    /// one clear pixel row under the face's lowest ink, lifting the glyphs as
    /// needed but never beyond the room above the tallest letter, and narrowing
    /// the rail to what remains (`aterm_render::chrome_fit`, shared by both
    /// renderers). [`Self::rail_h`] is then the rail's maximum. `false`
    /// elsewhere.
    pub clear_rail: bool,
    /// Cell columns `[start, end)` that keep their own cell background: the
    /// ground is not painted there.
    pub own: Vec<(u16, u16)>,
    /// The ink split, when the row's fill ends inside a cell.
    pub split: Option<InkSplit>,
    /// Outlined capsules: each a ring drawn over the ground with the band's own
    /// ground inside, placed and antialiased by one builder both renderers
    /// paint from (`aterm_render::chrome_ring_runs`), after the ground and the
    /// rail and before the glyphs. Empty on every row without one.
    pub rings: Vec<ChromeRing>,
    /// The cells whose glyph is drawn as a band icon instead of rasterized
    /// from a font: the cell keeps its character (the text grid, a screen
    /// reader, copy and the `messages` verb all read it), its ink and its
    /// ground; only the pixels of its glyph change. Between two blank cells the
    /// icon is drawn at the words' capital height and spills into them. Empty
    /// on every row without one.
    pub icons: Vec<ChromeIcon>,
}

impl ChromeRaster {
    /// The rail value that keeps the ground under it.
    pub const KEEP: u32 = u32::MAX;

    /// Whether cell column `col` keeps its own background.
    #[must_use]
    pub fn owns(&self, col: usize) -> bool {
        self.own
            .iter()
            .any(|&(a, b)| (usize::from(a)..usize::from(b)).contains(&col))
    }

    /// The icon cell column `col` draws in place of its glyph, if any.
    #[must_use]
    pub fn icon_at(&self, col: usize) -> Option<BandIcon> {
        self.icons
            .iter()
            .find(|i| usize::from(i.col) == col)
            .map(|i| i.icon)
    }
}

/// One outlined capsule on a [`ChromeRaster`] row: the cells `[start, end)`
/// carry a ring in `ring` with `inner` inside it; the renderer sizes the ring
/// from the cell and the face (`aterm_render::chrome_ring_runs`: standing on
/// the face's underline at its thickness, inset from the cells' edges,
/// rounded, its corners antialiased against the ground under them). Its
/// straight floor between the two end cells is those cells' single underline
/// in `ring`, which the host sets (`aterm_render::chrome_ring_floor_cols`), so
/// the renderer's descender ink-skip carves it like any rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromeRing {
    /// First cell column of the capsule (inclusive).
    pub start: u16,
    /// One past its last cell column.
    pub end: u16,
    /// The ring's colour, `0x00RRGGBB`.
    pub ring: u32,
    /// The ground inside the ring, `0x00RRGGBB` — the colour the capsule's
    /// cells carry as their background, so its label's glyphs blend on it.
    pub inner: u32,
    /// The band's closing seam, `0x00RRGGBB`, where the ring's row carries it
    /// (the stack's last row): the ring draws it across its two end cells on
    /// the floor's rows, meeting its rounded foot antialiased, and those cells
    /// carry no underline of their own. `None` on every other row.
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

/// The band's icon set: one drawn, antialiased icon for every status glyph a
/// message row can carry, so the row's glyph cell reads as one family instead
/// of whichever font happens to hold `⚠` or `ℹ`. Each stands in for exactly
/// one character of the band's closed glyph set ([`Self::for_char`]); the
/// renderer draws it from the cell's geometry and the face's own stroke
/// weight, in the cell's ink.
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
    pub const fn index(self) -> u32 {
        self as u32
    }

    /// The icon at `index` ([`Self::index`]).
    #[must_use]
    pub fn from_index(index: u32) -> Option<Self> {
        Self::ALL.get(usize::try_from(index).ok()?).copied()
    }
}

/// Where a chrome row's fill ends inside a cell ([`ChromeRaster::split`]):
/// that cell's glyph pixels left of frame pixel `x` wear `ink` (with `bg` as
/// the corrected-alpha operand), the rest the cell's own fg and bg.
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

/// One pane-local live default-background interval inside a composed
/// [`RenderInput`] row.
///
/// A single terminal snapshot uses [`RenderInput::default_bg`] for every cell.
/// A split window can compose panes whose OSC 11 / OSC 111 / DECSCNM state
/// resolves to different defaults, so `start_col..end_col` records the source
/// pane's default for that physical composite interval. Rows without spans, gaps
/// between valid spans (for example a divider), and malformed/out-of-range spans
/// fall back to the scalar `default_bg`.
///
/// Producers must keep each row's spans sorted by `start_col`, non-overlapping,
/// and clipped to [`RenderInput::cols`]; [`RenderInput::default_bg_at`] still
/// validates containment defensively.
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
    pub const fn new(start_col: usize, end_col: usize, default_bg: u32) -> Self {
        Self {
            start_col,
            end_col,
            default_bg,
        }
    }
}

/// Optional frame-space bounds for a composed text selection.
///
/// A terminal selection is in that terminal's own row/column coordinates. When
/// the focused terminal is projected into a split-pane frame, a multi-row
/// selection's interior rows would otherwise span the whole composite,
/// including dividers and sibling panes. This half-open rectangle confines the
/// predicates to the source pane. `RenderInput::selection_clip == None` is the
/// single-terminal behavior.
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
    pub fn translate_rows_down(&mut self, rows: usize) {
        self.row_start = self.row_start.saturating_add(rows);
        self.row_end = self.row_end.saturating_add(rows);
    }
}

/// One pane's selection, relocated into frame coordinates.
///
/// A composed split frame has one live selection per pane, each with its own
/// anchors, bounding rectangle and live OSC 17/19 colours, which the scalar
/// [`RenderInput::selection`] cannot represent. [`RenderInput::selections`]
/// carries the per-pane list, as `default_bg_spans` refines `default_bg`. An
/// empty list means the scalar path (every single-terminal frame, wasm, and
/// direct `cell_frame_into` extraction).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneSelection {
    /// The pane's selection with its anchors already moved into frame rows and
    /// columns (pane origin + that pane's `display_offset`).
    pub selection: TextSelection,
    /// The pane's half-open frame-space box. Always present, since an unbounded
    /// pane selection would tint its siblings (only a single-terminal frame's
    /// scalar `selection_clip` may be `None`).
    pub clip: SelectionClip,
    /// The pane's live selection background (`0x00RRGGBB`), or [`COLOR_UNSET`]
    /// to use the renderer's theme (as [`RenderInput::selection_bg`]); per entry
    /// so an OSC 17 in one pane cannot recolour a sibling's band.
    pub bg: u32,
    /// The pane's live selected-text foreground, or [`COLOR_UNSET`] /
    /// [`COLOR_DYNAMIC`]; same encoding as [`RenderInput::selection_fg`].
    pub fg: u32,
    /// Whether this pane is unfocused, so its band takes the renderer's
    /// inactive selection colour. Focus is per pane in a split, and this is the
    /// cue for which highlight the keyboard will extend.
    pub inactive: bool,
}

/// The per-row, per-entry selection key the renderer's row-damage diff
/// (`aterm_render::compute_dirty_rows`) compares.
///
/// [`RenderInput::selection_row_span`] is a hull over every entry and only a
/// scan window: with two panes selected on one row, dragging one edge inward
/// can leave the hull unchanged. This key is per entry and carries the
/// colours, so it is lossless.
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
    /// The entry's inactive flag — it selects a different band colour, so two
    /// otherwise identical spans still render differently.
    pub inactive: bool,
}

/// The inclusive selected column span one selection puts on one frame row.
/// Shared by [`RenderInput::selection_row_span`] and every [`PaneSelection`],
/// so a highlight and its damage band cannot disagree. `display_offset` is the
/// viewport projection (zero for entries, whose anchors the host already
/// relocated).
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
/// [`crate::terminal::Terminal::cell_frame_into`].
///
/// The windowed frontend holds the `Terminal` mutex only to extract this, then
/// renders without the lock, so the PTY reader is not starved for a frame's
/// duration.
///
/// `PartialEq`/`Eq` are hand-written and compare only rendered content,
/// excluding frame metadata such as [`snapshot_seq`](RenderInput::snapshot_seq),
/// [`content_seq`](RenderInput::content_seq) and
/// [`absolute_row_revision`](RenderInput::absolute_row_revision). The CPU
/// renderer's damage tracking compares a fresh `RenderInput` against the cached
/// one, overall and per row; `snapshot_seq` advances every damaged frame and
/// would defeat that. Content equality stays exact.
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
    /// The terminal's own DECSCUSR style. The frontend's unfocused override is
    /// applied by the renderer in `render_input`, not here.
    pub cursor_style: CursorStyle,
    /// Host/effect-owned cursor shape for this frame, separate from
    /// [`cursor_style`](Self::cursor_style) so an effect never overwrites the
    /// terminal's DECSCUSR state. Resolution order: backend override (e.g. the
    /// unfocused HollowBlock) > this > terminal DECSCUSR. Usually `None`.
    pub cursor_effect_style_override: Option<CursorStyle>,
    /// The cursor motion trail for this frame: recently swept cells fading out
    /// behind the cursor. Filled and decayed by the host's animation clock;
    /// `cell_frame_into` leaves it untouched. Usually empty. See [`TrailCell`].
    pub cursor_trail: Vec<TrailCell>,
    /// The resolved trail colour (`0x00RRGGBB`) blended over each trail cell's
    /// background. The host resolves it (config `cursor_trail_color`, else the
    /// effective live cursor: OSC 12/configured baseline, or live OSC 10
    /// foreground while OSC 21 `cursor=` is dynamic). Only consulted when
    /// `cursor_trail` is non-empty.
    pub cursor_trail_color: u32,
    /// Additive light quads for the cursor aurora (comet, bloom crown, landing
    /// ring, sparks), premultiplied and saturating-added under the crisp
    /// cursor. Window-absolute pixels ([`GlowQuad`] invariants). Host-owned;
    /// usually empty.
    pub cursor_glow_add: Vec<GlowQuad>,
    /// Radial cursor-effect light (fire embers / crown): elliptical-falloff
    /// halos with the [`cursor_glow_add`](RenderInput::cursor_glow_add)
    /// saturating-add contract and the [`RainHalo`] integer falloff (so CPU and
    /// GPU stay byte-exact). Drawn right after the aurora and before the nova /
    /// rain streams. [`RainHalo`] cursor-stream invariants: window-absolute
    /// pixels, one row band per quad (a multi-row halo is per-row quads sharing
    /// a centre). Host-owned; usually empty.
    pub glow_halo: Vec<RainHalo>,
    /// Under-glyph additive light (the fire body the glyphs sit inside as dark
    /// silhouettes): [`GlowQuad`]s with the
    /// [`cursor_glow_add`](RenderInput::cursor_glow_add) contract, drawn after
    /// the cell background and under-text sprites but before glyph ink, while
    /// tips/embers/beam stay above. Pairs with
    /// [`char_fg`](RenderInput::char_fg). [`GlowQuad`] invariants:
    /// window-absolute pixels, one row band per quad. Host-owned; usually
    /// empty.
    pub glow_under: Vec<GlowQuad>,
    /// Per-pixel procedural fire-field patches (the flame body at full art
    /// scale), each [`FirePatch`] evaluated at every device pixel by the shared
    /// integer field function (CPU and GPU byte-exact). Drawn at the
    /// [`glow_under`](RenderInput::glow_under) slot so engulfed glyphs read as
    /// silhouettes; [`FireMode::Add`] patches before [`FireMode::Over`].
    /// [`FirePatch`] invariants: window-absolute pixels, one row band per
    /// patch. Host-owned; usually empty.
    pub fire_patch: Vec<FirePatch>,
    /// Producer-built compact damage metadata for
    /// [`cursor_glow_add`](Self::cursor_glow_add). Transport metadata, excluded
    /// from whole-frame content equality.
    pub cursor_glow_add_damage: EffectStreamDamage,
    /// Producer-built compact damage metadata for [`glow_halo`](Self::glow_halo).
    pub glow_halo_damage: EffectStreamDamage,
    /// Producer-built compact damage metadata for [`glow_under`](Self::glow_under).
    pub glow_under_damage: EffectStreamDamage,
    /// Producer-built compact damage metadata for [`fire_patch`](Self::fire_patch).
    pub fire_patch_damage: EffectStreamDamage,
    /// Host-resolved cursor fill override (`0x00RRGGBB`), or `None` for the
    /// theme/OSC-12 colour. Applies to every cursor shape (block, hollow, bar,
    /// underline, Bolt); block glyph cut-outs and other shapes still go through
    /// `floor_cursor_fill`, keeping contrast. Host-owned (rainbow, forge,
    /// phaser, lightning).
    pub cursor_fill_override: Option<u32>,
    /// Sparkle-word decorations for this frame: small sprites over cells of
    /// matched profanity / feline words. Host-owned (`cell_frame_into` leaves
    /// it untouched); usually empty. See [`WordDecoration`].
    pub word_decorations: Vec<WordDecoration>,
    /// Animated-ink per-cell fg overrides: host-resolved final colours, sorted
    /// by `(row, col)` with unique cells, substituted for `fg` at every fg
    /// consult site (glyph, combining marks, line decorations; SGR 58 still
    /// wins) before the minimum-contrast / selection floors. Host-owned;
    /// usually empty. See [`InkCell`].
    pub ink: Vec<InkCell>,
    /// Effect-owned per-cell final glyph fg overrides (fire dark glyph cores
    /// inside the [`glow_under`](RenderInput::glow_under) body). Host-resolved
    /// `0x00RRGGBB`, sorted by `(row, col)` with unique cells, substituted at
    /// the same seam as [`ink`](RenderInput::ink) before the contrast floors;
    /// ink wins when a cell has both. Host-owned; usually empty. See
    /// [`CharFg`].
    pub char_fg: Vec<CharFg>,
    /// Effect-owned per-cell contrast-halo strengths (fire legibility): the
    /// engulfment weight driving the dark ring under each covered glyph's ink.
    /// Colour-free (the ink is never recoloured), sorted by `(row, col)` with
    /// unique cells. A grid stream: the tab-strip splice shifts rows like
    /// [`char_fg`](RenderInput::char_fg). Host-owned; usually empty. See
    /// [`FireHaloCell`].
    pub fire_halo: Vec<FireHaloCell>,
    /// Peeking-cat sprites drawn under text (CPU pass 1c in `render_row`,
    /// between cell-bg fill and inline images; GPU `emit_base_pre` before glyph
    /// ink), so a cat sits under the glyphs and inline images on both backends.
    /// Each quad lies in one row band ([`SpriteQuad`]; a two-row cat is a head
    /// quad in row `r-1` plus a chin slice in row `r`). Sampled from
    /// [`cat_atlas`](RenderInput::cat_atlas) nearest 1:1. Host-owned; usually
    /// empty.
    ///
    /// aterm's own cursor cat does not use this channel: it is emitted as
    /// [`free_sprites`](RenderInput::free_sprites) (one `FreeSprite` per cat
    /// plus its gaze dots), and aterm's GUI only clears this one. The dirty
    /// rules differ: a cat quad is marked by its `row` tag alone, a free sprite
    /// by its full pixel extent. The channel stays in the renderer's public
    /// contract and is held by the parity suites (`cat_parity.rs`,
    /// `free_parity.rs`).
    pub cat_quads: Vec<SpriteQuad>,
    /// RGBA8 atlas for [`cat_quads`](RenderInput::cat_quads), baked by the
    /// host's `CatBaker`. Shared `Arc`; the GPU re-uploads only when
    /// [`SceneAtlas::version`] changes (a rebake must bump it). `None` when no
    /// cat is live.
    pub cat_atlas: Option<std::sync::Arc<SceneAtlas>>,
    /// Free-floating decorative sprites ([`FreeSprite`]): arbitrary pixel
    /// rects at any position with no row tag. Dirty tracking row-unions each
    /// sprite's `[y, y+h)` extent (prev∪cur), so no per-row slicing is needed.
    /// Sampled from [`free_atlas`](RenderInput::free_atlas) nearest 1:1.
    /// Host-owned; usually empty.
    pub free_sprites: Vec<FreeSprite>,
    /// RGBA8 atlas for [`free_sprites`](RenderInput::free_sprites), versioned
    /// like [`cat_atlas`](RenderInput::cat_atlas): shared `Arc` so cloning is
    /// cheap; the GPU re-uploads only when [`SceneAtlas::version`] changes.
    /// `None` when no free sprite is live.
    pub free_atlas: Option<std::sync::Arc<SceneAtlas>>,
    /// Supernova additive light quads: the profanity nova's crown, shockwave
    /// ring and rays as premultiplied light, saturating-added like
    /// [`cursor_glow_add`](RenderInput::cursor_glow_add). [`GlowQuad`]
    /// invariants with grid-interior pixels, one row band per quad (ring chords
    /// and ray slabs split at row boundaries). Drawn after the aurora, under the
    /// word decorations and the cursor. Host-owned and self-terminating;
    /// usually empty.
    pub nova_add: Vec<GlowQuad>,
    /// Matrix-rain glyph sprites drawn under text (CPU pass 1c before
    /// [`cat_quads`](RenderInput::cat_quads); GPU `RainUnder` in the same
    /// slot), so cats walk on rain and glyphs stay legible. Each quad lies in
    /// one row band ([`SpriteQuad`]; row-only dirty marking would ghost a
    /// violator). Sampled from [`rain_atlas`](RenderInput::rain_atlas) nearest
    /// 1:1. Host-owned; usually empty.
    pub rain_quads: Vec<SpriteQuad>,
    /// RGBA8 white-coverage rain-glyph atlas for
    /// [`rain_quads`](RenderInput::rain_quads), baked by the `RainBaker`.
    /// Shared `Arc`; the GPU re-uploads only when [`SceneAtlas::version`]
    /// changes, so every rebake must bump it. `None` when no rain is live.
    pub rain_atlas: Option<std::sync::Arc<SceneAtlas>>,
    /// Rain bright-head additive halos, saturating-added like
    /// [`nova_add`](RenderInput::nova_add). Grid-interior pixels (the renderer
    /// offsets by `(pad, grid_top)`), one row band per quad. Host-owned;
    /// usually empty.
    pub rain_add: Vec<RainHalo>,
    /// Scrollback offset: viewport row `r` shows live row `r - display_offset`.
    pub display_offset: i32,
    /// Absolute row of the top visible line at extraction (`Grid::base_y()` =
    /// `oldest_absolute_row + scrollback_lines`), captured under the same lock
    /// as the cells. A host maps an absolute row into this frame with
    /// `viewport_row = absolute - base_y + display_offset` (e.g. the find bar's
    /// match highlights). Metadata, excluded from `PartialEq` like
    /// `snapshot_seq`, so a passive scroll does not force a repaint.
    pub base_y: i64,
    /// Revision of top-anchored protected-footer row insertions at extraction,
    /// captured under the same lock as [`base_y`](Self::base_y). Such an
    /// insertion cannot be re-anchored by one `base_y` delta, so hosts retaining
    /// absolute-row overlays reject stale geometry with it. Metadata, excluded
    /// from `PartialEq`.
    pub absolute_row_revision: u64,
    /// `Grid::history_renumber_epoch()` at extraction, captured under the same
    /// lock as [`base_y`](Self::base_y).
    ///
    /// The wholesale counterpart of `absolute_row_revision`: a width reflow
    /// renumbers every retained row and a Kitty CSI +T unscroll drops the
    /// newest scrollback lines, neither a uniform `base_y` delta and neither
    /// moving `absolute_row_revision`. A host re-anchoring retained absolute-row
    /// geometry must fail closed on both stamps. Metadata, excluded from
    /// `PartialEq`.
    pub history_renumber_epoch: u64,
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
    /// The terminal-content row band `[grid_top_row, grid_bot_row)` in this
    /// frame. Chrome (tab strip, edge bars, split dividers) lies outside and is
    /// pinned: the [`scroll_frac_px`](Self::scroll_frac_px) translate moves only
    /// the grid band. `grid_bot_row == 0` means no grid band (no translate); the
    /// windowed compose path fills these from the chrome splice counts.
    /// Excluded from `PartialEq`/`Eq` like `scroll_frac_px`.
    pub grid_top_row: usize,
    /// One past the last terminal-content row — see [`grid_top_row`](Self::grid_top_row).
    pub grid_bot_row: usize,
    /// The incoming-row apron: the one row just below the viewport at
    /// extraction, which a positive [`scroll_frac_px`](Self::scroll_frac_px)
    /// glide slides in at the bottom, so the exposed strip shows real pixels.
    /// Stamped on every fill
    /// ([`Terminal::cell_frame_into`](crate::terminal::Terminal::cell_frame_into),
    /// damage-scoped arm included, since it lives at viewport index `rows`);
    /// empty (`present == false`) at `display_offset == 0`. The apron at offset
    /// `d` is the bottom viewport row at offset `d - 1`. Present-time only, so
    /// excluded from `PartialEq`/`Eq` and the content damage diff.
    pub apron_row: ApronRow,
    /// The facts `apron_row` was last extracted under ([`ApronStamp`]), so an
    /// unchanged fill keeps the row instead of re-reading history
    /// (`Terminal::apron_row_into`). The cursor scalar is absent (always
    /// refreshed). Excluded from `PartialEq`, copied by `copy_from`. A writer
    /// changing `apron_row` without an extraction (the split compositor's
    /// clear) must clear this too.
    pub apron_stamp: Option<ApronStamp>,
    /// Focused-pane effect clip `(x0, y0, x1, y1)` in window-absolute device
    /// pixels (the `cursor_glow_add` space); the focused pane's box in a split,
    /// `None` otherwise. Present-time GPU post-fx (bloom halo, heat shimmer)
    /// intersect their regions with it so light and haze never cross a
    /// divider. Content streams are host-clipped already. Excluded from
    /// `PartialEq`/`Eq`.
    pub fx_clip: Option<(u16, u16, u16, u16)>,
    /// A clone of the active text selection, for per-cell highlighting.
    pub selection: TextSelection,
    /// Optional half-open frame-space rectangle confining
    /// [`selection`](Self::selection), so a split's focused-pane selection
    /// cannot tint a divider or sibling. `None` means no extra clip.
    pub selection_clip: Option<SelectionClip>,
    /// The live selection background (`0x00RRGGBB`), including OSC 17/21
    /// changes and OSC 117/RIS resets; `COLOR_UNSET` uses the renderer's
    /// theme. Separate from [`selection`](Self::selection) so a colour-only
    /// change invalidates cached pixels.
    pub selection_bg: u32,
    /// The live selected-text foreground (`0x00RRGGBB`), including OSC 19/21
    /// changes and OSC 119/RIS resets. [`COLOR_UNSET`] uses the renderer's
    /// policy; [`COLOR_DYNAMIC`] selects the automatic WCAG contrast floor.
    pub selection_fg: u32,
    /// Every visible pane's selection in frame coordinates, each with its own
    /// clip and live colours.
    ///
    /// Empty for single-terminal frames (and direct `cell_frame_into`
    /// extractions), where the four scalars above are the authority. A
    /// composed split frame fills one entry per pane with a selection, as
    /// [`default_bg_spans`](Self::default_bg_spans) refines `default_bg`. The
    /// scalars are still stamped from the focused pane; renderer predicates
    /// read the list when non-empty and the scalars otherwise.
    pub selections: Vec<PaneSelection>,
    /// Per-row, sparse emoji grapheme-cluster strings (`term.cluster_row(r)`):
    /// `(col, cluster)` for cells whose combining marks form a ZWJ / skin-tone /
    /// keycap sequence. The renderer shapes each to a colour glyph, or composites
    /// the several glyphs a colour face leaves it as; cells absent here take the
    /// ordinary single-codepoint dispatch.
    pub clusters: Vec<Vec<(usize, Box<str>)>>,
    /// Per-row, sparse combining marks (`term.combining_row(r)`): `(col, marks)`
    /// for cells with diacritics (é, ñ, …). The renderer overlays each mark's
    /// glyph on the base so accents render.
    pub combining: Vec<Vec<(usize, Box<[char]>)>>,
    /// Per-row DEC line size (DECDWL/DECDHL via `ESC # 3..6`), drawn scaled.
    /// `SingleWidth` is the default.
    ///
    /// Authoritative only while the matching
    /// [`line_size_spans`](Self::line_size_spans) row is empty. For a composed
    /// row with runs it is a summary (non-single when any pane on the row is a
    /// DEC line), so row-level consumers (the cat suppressor, the double-height
    /// damage gate) keep working; for placement use
    /// [`line_size_run_at`](Self::line_size_run_at).
    pub line_sizes: Vec<LineSize>,
    /// Per-row column runs with non-uniform DEC line size, for composed split
    /// frames. Empty means [`line_sizes`](Self::line_sizes) governs the row.
    /// Filled only by the pane compositor for rows where panes disagree (see
    /// [`LineSizeSpan`], [`Self::line_size_run_at`]); non-empty runs partition
    /// `0..cols` in ascending order.
    pub line_size_spans: Vec<Vec<LineSizeSpan>>,
    /// Pane-local live default-background intervals for composed split rows.
    /// Empty rows use [`default_bg`](Self::default_bg) for every column. A
    /// non-empty row has one [`DefaultBgSpan`] per pane, so background
    /// opacity, Kitty image layering, cursor fallback and trail fallback use
    /// the default of the pane that supplied each cell.
    ///
    /// scope-waiver: a default background is a colour, not an enforcement
    /// budget; no instance count can make it wrong, and per-pane is correct.
    pub default_bg_spans: Vec<Vec<DefaultBgSpan>>,
    /// Host chrome rows drawn at pixel resolution ([`ChromeRaster`], the
    /// message band's meter), at most one per row, in any order; usually
    /// empty. Rendered content: compared in `PartialEq`, and a changed raster
    /// dirties its row (`aterm_render::compute_dirty_rows`).
    pub chrome_rasters: Vec<ChromeRaster>,
    /// Per-row, sparse inline-image placements (`term.images_row(r)`):
    /// `(col, ImageRef)` for every cell covered by an iTerm2 OSC 1337 `File=`
    /// image. The renderer decodes each image once (keyed by the ref's `Arc`)
    /// and blits the cell's tile; a covered cell skips its glyph (the bg still
    /// fills).
    pub images: Vec<Vec<(usize, aterm_grid::ImageRef)>>,
    /// Wallpaper base layer: a host-prepared RGBA8 image, pre-scaled to this
    /// frame's pixel size (cover-fit) and pre-blended toward the theme
    /// background by the configured dim; the renderer only blits (CPU copy,
    /// GPU 1:1 nearest), staying byte-exact. When set, the base (clear, padding
    /// bands, and every unselected cell whose bg is its pane's live default)
    /// shows these texels; selected cells and SGR backgrounds still paint
    /// opaquely over it (the kitty below-bg-image rule). Wallpaper pixels are
    /// opaque, replacing the background-opacity glass. Compared by `Arc`
    /// identity like every atlas; the host publishes a fresh `Arc` per (source,
    /// size, dim), and a change forces a full repaint (padding strips are not
    /// rows). It also disables the scroll-blit rescue, which would drag the
    /// backdrop with the content.
    pub wallpaper: Option<std::sync::Arc<SceneAtlas>>,
    /// The live default background (`0x00RRGGBB`): the engine's dynamic
    /// default bg folded with DECSCNM, resolved by
    /// [`Terminal::cell_frame_into`](crate::terminal::Terminal::cell_frame_into).
    /// `COLOR_UNSET` only for a pristine unconfigured `Terminal::new`, so
    /// standalone renderers keep their theme fallback. The renderer paints the
    /// padding band and base clear from this, so OSC 11 / OSC 111 and DECSCNM
    /// reach the border too, matching the grid interior.
    pub default_bg: u32,
    /// The live default foreground (`0x00RRGGBB`), the twin of
    /// [`Self::default_bg`] through the same OSC 10 / OSC 110 / DECSCNM policy
    /// (DECSCNM swaps both or neither).
    ///
    /// Used by the cursor-effect layer to anchor and bound the fresh-typed
    /// glyph tint; the renderer resolves per-cell foregrounds itself.
    /// `COLOR_UNSET` (a pristine `Terminal::new`) must be treated as unknown,
    /// never masked into a colour (it would read as black).
    pub default_fg: u32,
    /// The exact implicit empty cell resolved under the same terminal lock as
    /// `cells`. Unlike `default_fg` / `default_bg`, this is always concrete:
    /// those scalar fields retain `COLOR_UNSET` for an unconfigured terminal's
    /// renderer fallback. Host row overlays use this when they need the live
    /// terminal ink and ground without a second, potentially contended lock or
    /// a colour read from a newer frame.
    pub implicit_blank: RenderCell,
    /// The live cursor colour (`0x00RRGGBB`): an explicit OSC 12 value or the
    /// configured OSC 112 baseline, or the live OSC 10 foreground while OSC 21
    /// `cursor=` selects dynamic behaviour. `COLOR_UNSET` keeps the renderer's
    /// theme cursor for a pristine, unconfigured `Terminal::new`. The renderer
    /// fills the block/bar/underline cursor from this; a cursor-colour-only
    /// change is un-gated by `aterm_render::compute_dirty_rows` (via
    /// `cursor_changed`), and the field is compared in `PartialEq`.
    pub cursor_color: u32,
    /// The engine's monotone damage epoch at snapshot time: the value of
    /// [`Terminal::damage_epoch`](crate::terminal::Terminal::damage_epoch)
    /// captured under the same lock that filled the rest of this snapshot, so a
    /// consumer can detect staleness against a later `damage_epoch()`. A
    /// version stamp, not content: excluded from `PartialEq`/`Eq`, since it
    /// advances every damaged frame and would defeat the content damage cache.
    pub snapshot_seq: u64,
    /// Content-only arrival clock coherent with this engine-filled snapshot.
    /// Unlike [`snapshot_seq`](Self::snapshot_seq), damage-only frames cannot
    /// advance it. Metadata only; excluded from equality.
    pub content_seq: u64,
    /// Terminal parser batch sequence coherent with this engine-filled
    /// snapshot. Unlike the damage epoch it advances even when another batch
    /// joins an already-pending damage session, so one-shot input evidence can
    /// reject intervening output. Metadata only; excluded from equality.
    pub process_sequence: u32,
    /// Present-time latency hint: this frame is an immediate keystroke echo that
    /// bypasses present coalescing. The GPU present path uses it to defer the
    /// present-time bloom halo (a whole-framebuffer copy plus a second
    /// `queue.submit`) to the next settle frame, so the keystroke presents at
    /// minimum latency and the halo lands about one frame later. Present-time
    /// only, so excluded from `PartialEq`/`Eq`: it must never dirty a cell. The
    /// CPU backend ignores it.
    pub input_hot: bool,
    /// Identity of the engine instance that last engine-filled this snapshot,
    /// or 0 for a never-engine-filled scratch. Stamped by
    /// `cell_frame_into`/`cell_frame_damage_scoped_into`; the damage-scoped
    /// refill checks it so a scratch reused across different terminals (the
    /// split compositor's shared `pane_scratch`) never serves one pane's
    /// retained rows as another's. Metadata, excluded from `PartialEq`/`Eq`.
    pub terminal_id: u64,
    /// The stamping terminal's extraction generation at fill time (see
    /// `Terminal::take_damage`). A damage-scoped refill is sound only while
    /// this equals the terminal's current generation: no consumer has taken
    /// (and reset) the damage tracker since this scratch was filled. 0 = never
    /// engine-filled. Metadata, excluded from `PartialEq`/`Eq`.
    pub extract_gen: u64,
    /// `snapshot_seq` as stamped by the last engine fill. Host mutators that
    /// touch content channels afterwards (stream fade, prediction ghosts, the
    /// tab-strip splice) bump `snapshot_seq`, so `snapshot_seq !=
    /// engine_fill_seq` means a host wrote cells since the engine fill, and the
    /// damage-scoped refill falls back to a full refill. Metadata, excluded
    /// from `PartialEq`/`Eq`.
    pub engine_fill_seq: u64,
    /// Whether the stamping terminal's alternate screen was active at fill
    /// time. Primary and alt grids keep independent damage state, so a swap
    /// between fills invalidates row continuity; the scoped refill full-refills
    /// on mismatch. Metadata, excluded from `PartialEq`/`Eq`.
    pub engine_alt: bool,
    /// Which column order the last engine fill left this snapshot's rows in.
    /// Always [`RowOrder::Logical`] in builds without the `bidi` feature.
    ///
    /// `apply_bidi_reorder` is an in-place, non-idempotent row permutation, so
    /// retained rows may be kept only while they are still in logical order.
    /// [`RowOrder::BidiVisual`] forces the next refill to the full arm; a
    /// pure-LTR frame keeps the scoped arm. Metadata, excluded from
    /// `PartialEq`/`Eq`.
    pub engine_row_order: RowOrder,
    /// Per-row content revision, one entry per row: the engine's
    /// [`Grid::row_revisions`](crate::grid::Grid::row_revisions) value, captured
    /// under the same lock as the cells, so a consumer diffing two snapshots
    /// (`aterm_render::compute_dirty_rows`) can answer "did row `r` change?"
    /// with a `u64` compare.
    ///
    /// For two snapshots of the same terminal whose row identity is pinned
    /// (see [`row_rev_lane`](Self::row_rev_lane)), a row whose content changed
    /// has different entries here. The converse does not hold (an unchanged
    /// row may be re-stamped), so a consumer may over-repaint but never skips a
    /// changed row.
    ///
    /// `0` means unknown, not "unchanged": the consumer compares content.
    /// Host-built rows (a spliced tab strip, a composed pane band) carry it.
    /// Metadata, excluded from `PartialEq`/`Eq`.
    pub row_rev: Vec<u64>,
    /// Identity of the row-revision lane [`row_rev`](Self::row_rev) belongs to,
    /// or `0` when the stamps must not be trusted.
    ///
    /// An engine fill sets it to the terminal's extraction identity; every
    /// other producer (`empty()`, any host that composes or re-shapes rows)
    /// leaves it `0`. Stamps compare only when both lanes are non-zero and
    /// equal, since distinct terminals mint revisions from independent clocks.
    ///
    /// The consumer must also pin row identity (`base_y`,
    /// `absolute_row_revision`, `engine_alt`, a zero `display_offset`,
    /// `engine_row_order`) and the absence of post-fill host writes
    /// (`snapshot_seq == engine_fill_seq`). Metadata, excluded from
    /// `PartialEq`/`Eq`.
    pub row_rev_lane: u64,
    /// How many host-owned rows (the in-grid tab strip's chrome rows) have been
    /// prepended since the engine filled this snapshot: engine row `r` lives
    /// at index `r + row_shift`. `0` for every un-spliced snapshot.
    ///
    /// Physical, not a provenance claim: it counts rows actually prepended,
    /// blessed or not (see [`shifted_fill_seq`](Self::shifted_fill_seq)), so an
    /// un-splice knows how many rows to remove. `aterm_render::compute_dirty_rows`
    /// compares it across frames (a changed strip row count means stamps must
    /// not be compared), and the frontend's un-splice reads it. Metadata,
    /// excluded from `PartialEq`/`Eq`.
    pub row_shift: usize,
    /// `snapshot_seq` as of the last operation that left this snapshot equal
    /// to an engine fill shifted down by [`row_shift`](Self::row_shift) host
    /// rows: the engine fill itself, or a pure row prepend on top of one.
    ///
    /// A second clock rather than a relaxation of `engine_fill_seq`: the strip
    /// splice writes no engine row, so the engine's per-row revisions still
    /// describe composed index `r + row_shift`. Every other host mutator
    /// overwrites engine cells and bumps `snapshot_seq` without touching this
    /// field, so `snapshot_seq != shifted_fill_seq` means some host wrote
    /// engine cells, and both the stamp gate and the un-splice fail closed.
    /// Only [`note_host_row_prepend`](Self::note_host_row_prepend) and an engine
    /// fill advance it.
    ///
    /// `0` is the never-blessed sentinel: `empty()`, every composed frame (see
    /// [`invalidate_row_revisions`](Self::invalidate_row_revisions)), and any
    /// prepend applied on top of already-mutated cells. Metadata, excluded from
    /// `PartialEq`/`Eq`.
    pub shifted_fill_seq: u64,
    /// `snapshot_seq` as of the last composed frame a host committed into this
    /// snapshot: the split compositor's row-retention token, the twin of
    /// [`shifted_fill_seq`](Self::shifted_fill_seq) one level up.
    ///
    /// The compositor retains a settled composite's unchanged cells by not
    /// rewriting them, which is sound only while nothing touched them since the
    /// last compose. Every host mutator that writes into a composite bumps
    /// `snapshot_seq` without touching this field, so `snapshot_seq !=
    /// composed_fill_seq` means some host wrote into it and retention fails
    /// closed. An engine fill and every host rebuild of the rows
    /// ([`invalidate_row_revisions`](Self::invalidate_row_revisions)) reset it
    /// to `0`, so a snapshot that changed hands is never mistaken for the
    /// composite a ledger describes.
    ///
    /// `0` is the never-blessed sentinel and the only value a producer other
    /// than [`bless_composed_fill`](Self::bless_composed_fill) leaves here.
    /// Metadata, excluded from `PartialEq`/`Eq`.
    pub composed_fill_seq: u64,
}

/// The column order a [`RenderInput`]'s rows are in, as left by the last
/// engine fill (see [`RenderInput::engine_row_order`]): whether retained rows
/// are still in the order a fresh logical fill would produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowOrder {
    /// No row was permuted: logical order and visual order coincide, for every
    /// row and every column-indexed channel (cells, clusters, combining marks,
    /// images, and the cursor column). This is every frame of a pure-LTR
    /// session, and every frame at all in a build without the `bidi` feature.
    #[default]
    Logical,
    /// At least one row was permuted into BiDi visual order by
    /// `apply_bidi_reorder`. The permutation is not idempotent, so these rows
    /// may not be retained by a damage-scoped refill.
    BidiVisual,
}

/// Which continuity clause refused the damage-scoped arm, carried by
/// `FrameRefill::Full`.
///
/// The scoped refill's validity check (see
/// [`Terminal::cell_frame_damage_scoped_into`](crate::terminal::Terminal::cell_frame_damage_scoped_into))
/// is a conjunction of independent clauses; the variant names the first
/// clause that refused, in the check's short-circuit order, so a
/// Full-dominated steady state is diagnosable.
///
/// `#[repr(usize)]` matters: `FrameRefill::Scoped` carries a `usize`, and a
/// narrower cause drops the enum out of rustc's ScalarPair ABI, making
/// `cell_frame_damage_scoped_into` return through memory instead of two
/// registers on the keystroke path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(usize)]
pub enum FullRefillCause {
    /// The last engine fill permuted at least one row into BiDi visual order
    /// (`engine_row_order != Logical`); retained rows would be double-permuted.
    /// Recovers once no row reorders.
    BidiVisual,
    /// The scratch carries no terminal identity (`terminal_id == 0`): it has
    /// never been engine-filled. Every first frame of a window reports this
    /// once, by construction.
    ScratchUnstamped,
    /// The scratch was last engine-filled by a different terminal (the split
    /// compositor's shared pane scratch hands off between panes).
    TerminalMismatch,
    /// Another consumer called `take_damage` since the scratch's fill
    /// (`extract_gen` moved), so the tracker's bits no longer describe every
    /// row that changed under this scratch.
    DamageTaken,
    /// A host mutator wrote content channels after the engine fill and bumped
    /// `snapshot_seq` (stream fade, prediction ghosts, find bar, strip splice).
    HostMutation,
    /// A host row prepend is still in force (`row_shift != 0`), so engine row
    /// `r` sits at index `r + row_shift`; the tab-strip splice's inverse
    /// (`undo_host_row_prepend`) did not run.
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
    /// The engine's viewport is scrolled back (`display_offset != 0`) and the
    /// scrolled continuity proof (`Terminal::scrolled_scoped_refill_delta`) did
    /// not hold: the scratch entered history from the live bottom, content or
    /// row identity moved under the parked viewport, the tracker holds
    /// whole-grid damage, or the net move exposes the whole viewport. An output
    /// batch under a viewport parked deeper than one screen also lands here:
    /// the pin to `display_offset == 0` marks `Damage::Full` for a move of
    /// `>= rows`, indistinguishable from a palette/DECSCNM change in the batch.
    EngineScrolled,
    /// The engine is at offset 0 but the scratch was filled while scrolled
    /// back (the frame that returns to the bottom), so its retained rows use a
    /// different viewport mapping.
    ScratchScrolled,
    /// `base_y` moved: the live grid scrolled, so retained rows shifted out
    /// from under their indices (scroll damage marks only the exposed strip).
    /// The cause a scrolling workload — a build log, a `cat` of a large file —
    /// reports on nearly every frame, and honestly so.
    BaseY,
    /// The absolute row revision moved without `base_y` moving: a protected
    /// footer insertion or another row-identity change.
    RowRevision,
    /// The tracker holds `Damage::Full` (a palette recolor, DECSCNM, a reflow,
    /// or another whole-grid invalidation). Nothing is retainable.
    FullDamage,
}

impl FullRefillCause {
    /// Every cause, in discriminant order. The tally arrays and the metrics
    /// wire format are built from this, so a new clause cannot be added without
    /// its counter appearing.
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

    /// Dense index into a `[_; COUNT]` tally. The discriminant itself, so the
    /// lookup is a zero-extend.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Stable wire label, snake_case, for metrics text and JSON. Stable across
    /// releases: a dashboard keys off these.
    #[must_use]
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
/// refilled a scratch: the full-viewport fallback, or the damage-scoped arm
/// that re-resolved only the tracker's damaged rows. Returned so callers and
/// tests can assert that a damage-scoped workload actually reaches the scoped
/// arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRefill {
    /// The whole viewport was re-extracted (continuity break, full damage, or
    /// first fill). Always sound; the cost is the pre-carrier status quo.
    Full {
        /// Which continuity clause refused the scoped arm (the first one, in
        /// the check's short-circuit order).
        cause: FullRefillCause,
    },
    /// Only the damage tracker's rows were re-extracted; every other row was
    /// left as-is in the scratch (byte-identical by the continuity invariant).
    Scoped {
        /// Number of viewport rows re-resolved (== the tracker's damaged-row
        /// count clamped to the viewport). 0 is legal: a cursor-only move
        /// damages nothing yet still restamps cursor/selection scalars.
        rows_refilled: usize,
    },
}

impl Clone for RenderInput {
    /// A fresh deep copy of every field (`snapshot_seq` included). Equivalent to a
    /// derived `clone`; used by the snapshot seed paths.
    fn clone(&self) -> Self {
        let mut out = Self::empty();
        out.copy_from(self, true);
        out
    }

    /// Capacity-reusing in-place update: delegates to each field's
    /// `clone_from` so a stable-dimension frame reuses the destination's
    /// allocations instead of deep-cloning a fresh grid. The result is
    /// identical to `*self = source.clone()`.
    fn clone_from(&mut self, source: &Self) {
        self.copy_from(source, true);
    }
}

/// Content equality for the per-row inline-image placements (the `images`
/// clause of [`RenderInput`]'s `PartialEq`): column, tile and payload, i.e.
/// the derived `ImageRef` equality minus `ImageRef::kitty`, a placement tag
/// that draws no pixel.
///
/// `ImageRef`'s derived `Eq` deep-compares the payload whenever the two
/// `Arc`s are distinct, and a re-transmitting client allocates a fresh `Arc`
/// per transmit, so a plain `Vec` compare costs covered cells × payload bytes.
/// The verdict per (left, right) `Arc` pair is memoized for this call, so the
/// cost scales with distinct images. `aterm-render`'s `ImageEqMemo` does the
/// same.
///
/// The raw-pointer key is sound because the memo lives only for this call and
/// the borrowed inputs keep every `Arc` alive, so no address is reused by a
/// different image; never persist the map across calls.
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
        // The one deep compare this pair pays: the derived `ImageData`
        // equality (payload and every layout field).
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

// Hand-written equality: compare rendered content only. The CPU damage cache
// compares the incoming snapshot against the previous frame's, so metadata
// that changes every frame (`snapshot_seq` and the fields listed at the end)
// is excluded.
//
// Atlases compare by snapshot identity (`Arc::as_ptr`), never `version`: baker
// versions are deterministic per engine instance, so a rebuilt engine replays
// its predecessor's versions with different texels. Every rebake publishes a
// fresh `Arc` and a stable frame re-presents the same one.
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
            && self.display_offset == other.display_offset
            && self.selection == other.selection
            && self.selection_clip == other.selection_clip
            && self.selection_bg == other.selection_bg
            && self.selection_fg == other.selection_fg
            && self.selections == other.selections
            && self.clusters == other.clusters
            && self.combining == other.combining
            && self.line_sizes == other.line_sizes
            && self.line_size_spans == other.line_size_spans
            && self.default_bg_spans == other.default_bg_spans
            // Images: memoized cross-`Arc` content equality (see `images_eq`).
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
        //   `history_renumber_epoch`: host re-anchor metadata.
        // - `scroll_frac_px`, `grid_top_row`, `grid_bot_row`, `apron_row`,
        //   `input_hot`: consumed at present time, outside the damage cache.
        // - `terminal_id`, `extract_gen`, `engine_fill_seq`, `engine_alt`,
        //   `engine_row_order`, `row_rev`, `row_rev_lane`, `shifted_fill_seq`,
        //   `composed_fill_seq`: continuity tokens that advance on every
        //   damaged frame and would defeat the dirty gate they feed.
    }
}

impl Eq for RenderInput {}

/// What [`RenderInput::apron_row`] was last extracted under: when a fill finds
/// every term unchanged it keeps the row instead of re-reading history.
///
/// The row holds resolved colours, so the palette, default colours, reverse
/// video, style policy and BiDi modes are part of its identity (their mutators
/// only mark `Damage::Full`, not `content_gen`). Tabs of one window share a
/// scratch while per-grid counters collide numerically, so the filling
/// terminal is part of the memo too.
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

/// The incoming-row apron: the one row just below the viewport, carried on
/// [`RenderInput::apron_row`]. A struct rather than an `Option` so the
/// per-frame refill keeps its Vec capacity (`present` is the presence bit).
/// This is content the sub-row translate reveals, not the scroll-blit
/// overshoot band.
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
    /// `clone_from` that keeps every Vec's capacity (a derived one is
    /// `*self = source.clone()`, fresh Vecs on every per-frame cache copy).
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
    fn default() -> Self {
        Self::empty()
    }
}

/// Where a host row prepend's window-space effect streams (`cursor_glow_add`,
/// `glow_under`, `fire_patch`, `glow_halo` and the `fx_clip` box) were placed
/// by their producers; the two hosts that prepend rows place them differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostRowPixels {
    /// Already placed below the prepended rows: the producers' origin includes
    /// them (the native window, whose `effects_origin_win` counts the strip and
    /// the band), so the pixels stay put and only the damage row tags are
    /// re-derived.
    Placed,
    /// Placed against the frame without the prepended rows (the web module,
    /// whose effects pipeline has one chrome origin, `(pad, pad + head)`):
    /// every pixel `y` those streams carry moves down by `rows · cell_h`
    /// first, then the tags are re-derived as under [`Self::Placed`].
    Translate,
}

impl RenderInput {
    /// The pixel-resolution chrome raster of frame row `r`, if the host drew
    /// one ([`ChromeRaster`]).
    #[must_use]
    pub fn chrome_raster(&self, r: usize) -> Option<&ChromeRaster> {
        self.chrome_rasters.iter().find(|m| usize::from(m.row) == r)
    }

    /// Update a damage-cache snapshot without retaining the four large
    /// cursor-effect payloads when their producer supplied compact metadata.
    ///
    /// A cached frame is never drawn again: it is read only by the dirty-row
    /// decision.  For these streams the metadata is therefore a complete prior
    /// representation (content revision plus occupied rows).  Hand-built input
    /// without metadata remains fully cloned, preserving the public fallback.
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
        self.display_offset = source.display_offset;
        self.base_y = source.base_y;
        self.absolute_row_revision = source.absolute_row_revision;
        self.history_renumber_epoch = source.history_renumber_epoch;
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

    /// Rebuild the four compact cursor-effect damage records after the final
    /// frontend composition (splices, clipping and chrome suppression all
    /// precede this call).
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

    /// An empty 0×0 snapshot with no allocations — the seed for a persistent
    /// scratch buffer that [`Terminal::cell_frame_into`](crate::terminal::Terminal::cell_frame_into)
    /// refills in place each frame (C-1). Cursor scalars default to off/origin and
    /// `snapshot_seq` to 0. `cell_frame_into` overwrites the engine-owned grid,
    /// cursor (including its live colour), live implicit blank/background,
    /// selection (including its live colours), and snapshot metadata; hosts must stamp or
    /// clear their own overlay and presentation-transform fields on each frame.
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
            display_offset: 0,
            base_y: 0,
            absolute_row_revision: 0,
            history_renumber_epoch: 0,
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

    /// Disown the per-row revision lane: this snapshot's rows are no longer the
    /// rows an engine fill stamped.
    ///
    /// Every host producer that rebuilds rows (the split compositor) must call
    /// this. A pure row prepend (the in-grid tab strip) instead splices the
    /// lane via [`note_host_row_prepend`](Self::note_host_row_prepend), falling
    /// back here when it cannot prove the shift is the only change. In-place
    /// host mutators need not call it (they self-report through
    /// `snapshot_seq != shifted_fill_seq`), but calling it is always sound; the
    /// only cost is a whole-grid content compare.
    ///
    /// Also resets the prepend bookkeeping: `row_shift` to 0 and
    /// `shifted_fill_seq` to its never-blessed `0`.
    pub fn invalidate_row_revisions(&mut self) {
        self.row_rev_lane = 0;
        self.row_rev.clear();
        self.row_shift = 0;
        self.shifted_fill_seq = 0;
        // The rows are no longer the composite a retention ledger describes
        // either; every divider-grid fill runs this, so a native tab, a capture
        // or a mixed composite cannot inherit the previous split frame's blessing.
        self.composed_fill_seq = 0;
    }

    /// Record that a host just prepended `prepended` rows (the in-grid tab
    /// strip), splicing the revision lane to match instead of disowning it.
    ///
    /// Call it after every per-row channel has been shifted and after the
    /// `snapshot_seq` bump. `blessed` is the caller's proof, taken before it
    /// mutated anything, that the snapshot was still exactly an engine fill (or
    /// a blessed shift of one); see
    /// [`host_prepend_blessing`](Self::host_prepend_blessing).
    ///
    /// Blessed, the lane gains `prepended` leading `0`s (unknown), so the
    /// chrome rows fall back to a content compare while engine row `r` keeps
    /// its revision at index `r + row_shift`. Unblessed, the lane is disowned.
    /// `row_shift` advances either way: it is the physical row count an
    /// un-splice must remove.
    pub fn note_host_row_prepend(&mut self, prepended: usize, blessed: bool) {
        self.row_shift = self.row_shift.saturating_add(prepended);
        // A prepend slides every composed row down, so a retention ledger
        // indexed by composed row no longer describes them.
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

    /// Bless this snapshot as a committed composed frame: its cells are exactly
    /// what the compositor just wrote, so the next compose may retain rows
    /// nothing changed.
    ///
    /// Called once by the compositor, after its last cell write and its
    /// `snapshot_seq` bump. Any host that writes cells afterwards bumps
    /// `snapshot_seq` alone and thereby revokes this.
    pub fn bless_composed_fill(&mut self) {
        self.composed_fill_seq = self.snapshot_seq;
    }

    /// Whether this snapshot is still, cell for cell, the composed frame
    /// [`bless_composed_fill`](Self::bless_composed_fill) last blessed.
    ///
    /// `false` if never blessed, disowned by an engine fill or divider-grid
    /// rebuild, or written by a host mutator. Read at the top of a compose.
    #[must_use]
    pub fn composed_fill_intact(&self) -> bool {
        self.composed_fill_seq != 0 && self.snapshot_seq == self.composed_fill_seq
    }

    /// Whether a host row prepend about to be applied may keep the revision
    /// lane: the snapshot is exactly what the last engine fill left (possibly
    /// shifted by an earlier blessed prepend) with a lane covering every row.
    ///
    /// Read before the prepend mutates anything, and handed to
    /// [`note_host_row_prepend`](Self::note_host_row_prepend).
    #[must_use]
    pub fn host_prepend_blessing(&self) -> bool {
        self.row_rev_lane != 0
            && self.row_rev.len() == self.rows
            && self.cells.len() == self.rows
            && self.snapshot_seq == self.shifted_fill_seq
            && (self.row_shift > 0 || self.snapshot_seq == self.engine_fill_seq)
    }

    /// Undo a blessed host row prepend, restoring the engine fill it was made
    /// from, so a tab-strip window's scratch can go back to
    /// [`Terminal::cell_frame_damage_scoped_into`](crate::terminal::Terminal::cell_frame_damage_scoped_into)
    /// and take the damage-scoped arm.
    ///
    /// Returns `false`, changing nothing, unless something was prepended, the
    /// prepend was blessed and is the only change since the engine fill
    /// (`snapshot_seq == shifted_fill_seq`, non-zero), and the three retained
    /// channels are all exactly `engine_rows + row_shift` long.
    ///
    /// Restores `cells`, `clusters`, `combining`, the row count and the
    /// continuity tokens only; the cursor, selections and effect streams are
    /// restamped by the next fill, so an un-spliced scratch is an intermediate,
    /// never a presentable frame.
    ///
    /// The drained chrome-row buffers go to `pool` (up to `pool_cap` entries)
    /// for reuse; the drain removes the whole range regardless of the cap.
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
        if self.row_rev.len() >= strip {
            self.row_rev.drain(0..strip);
        }
        self.rows = engine_rows;
        self.row_shift = 0;
        // The cells are again the engine's, so the engine-fill token is
        // re-armed; this is the only place it may be.
        self.snapshot_seq = self.engine_fill_seq;
        self.shifted_fill_seq = self.engine_fill_seq;
        // An un-splice restores an engine fill, never a composite.
        self.composed_fill_seq = 0;
        true
    }

    /// Prepend `strip` host rows (a tab strip, a message band, a presence row)
    /// to this snapshot: every per-row channel, the cursor, the selections and
    /// every row-tagged effect stream move down by `strip` rows, the chrome
    /// rows are copied from the borrowed `strip_rows`, and the revision lane is
    /// spliced ([`note_host_row_prepend`](Self::note_host_row_prepend)) so
    /// [`undo_host_row_prepend`](Self::undo_host_row_prepend) can remove them
    /// before the next refill.
    ///
    /// `cell_h` is the pixel cell height, used to shift grid streams' pixel
    /// quads. `grid_top` is the grid-interior top before the prepend
    /// (`pad_top + head`): a window-space quad above `grid_top + strip·cell_h`
    /// re-derives its damage tag onto a chrome row. `pixels` says where the
    /// window-space streams' producers put them ([`HostRowPixels`]).
    ///
    /// `pool` supplies reclaimed `cells` row buffers (no allocation after
    /// warmup; an empty pool falls back to a fresh copy). Each container is
    /// shifted with one `splice`, since this runs on the present path. The web
    /// host and `aterm-gui` share this splice.
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
        // The revision lane is in engine row space, so it must shift with the
        // other channels; `note_host_row_prepend` (after every channel has
        // moved) does that. The blessing is read here, before anything moves.
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
        // Per-row sparse / sized data: prepend empty/default rows so indices stay aligned
        // with `cells`. `clusters`/`combining`/`images` are sparse (empty vecs);
        // `line_sizes` defaults to single-width. `(0..strip).map` keeps the iterators
        // exact-size so each splice shifts the tail exactly once.
        self.clusters.splice(0..0, (0..strip).map(|_| Vec::new()));
        self.combining.splice(0..0, (0..strip).map(|_| Vec::new()));
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
        // The cursor (terminal-grid row) is now `strip` rows lower in the window;
        // the selection anchors and motion-trail cells move down with it so they
        // stay on terminal content instead of repainting the new strip row.
        self.cursor_row += strip;
        self.selection
            .translate_rows_for_presentation(i32::try_from(strip).unwrap_or(i32::MAX));
        if let Some(clip) = &mut self.selection_clip {
            clip.translate_rows_down(strip);
        }
        // Every pane's selection moves with the splice too.
        for pane in &mut self.selections {
            pane.selection
                .translate_rows_for_presentation(i32::try_from(strip).unwrap_or(i32::MAX));
            pane.clip.translate_rows_down(strip);
        }
        for t in &mut self.cursor_trail {
            t.row += strip;
        }
        // Window-space streams: under `Placed` the pixels already include the
        // strip band and are not shifted. Only the damage row tag moves, and it
        // is re-derived from the pixel y against the composed frame's bands
        // (row r spans `grid_top + r·cell_h ..`, row 0 opening to the window
        // top), so a quad above the terminal grid lands in its strip row's band
        // and an in-grid quad on its shifted terminal row.
        let dy = u16::try_from(strip * cell_h).unwrap_or(u16::MAX);
        let shift_tag = |y: u16| (usize::from(y).saturating_sub(grid_top) / cell_h.max(1)) as u16;
        // Under `Translate` the producers placed them without these rows, so
        // every pixel y (quad top, flame root, halo centre, the effect clip)
        // first moves down by `pixel_dy` (zero under `Placed`); the tag rule
        // then applies as for `Placed`.
        let pixel_dy = if pixels == HostRowPixels::Translate {
            dy
        } else {
            0
        };
        if let Some((_, y0, _, y1)) = &mut self.fx_clip {
            *y0 = y0.saturating_add(pixel_dy);
            *y1 = y1.saturating_add(pixel_dy);
        }
        // Aurora quads are window-absolute pixel rects tagged with a row.
        for q in &mut self.cursor_glow_add {
            q.y = q.y.saturating_add(pixel_dy);
            q.row = shift_tag(q.y);
        }
        // Under-ink flame body: same pixel-rect shape as the aurora.
        for q in &mut self.glow_under {
            q.y = q.y.saturating_add(pixel_dy);
            q.row = shift_tag(q.y);
        }
        // Per-pixel fire patches: the quad and its flame-root pixel move by
        // `pixel_dy`; the row tag follows the quad.
        for q in &mut self.fire_patch {
            q.y = q.y.saturating_add(pixel_dy);
            q.base_y = q.base_y.saturating_add(pixel_dy);
            q.row = shift_tag(q.y);
        }
        // Charred-ink overrides are cell-tagged like ink (a grid stream).
        for c in &mut self.char_fg {
            c.row = c.row.saturating_add(strip as u16);
        }
        // Fire contrast-halo strengths are cell-tagged like char_fg.
        for c in &mut self.fire_halo {
            c.row = c.row.saturating_add(strip as u16);
        }
        // Radial cursor-effect halos: the quad and its falloff centre move by
        // `pixel_dy`; the row tag follows.
        for h in &mut self.glow_halo {
            h.y = h.y.saturating_add(pixel_dy);
            h.cy = h.cy.saturating_add(pixel_dy);
            h.row = shift_tag(h.y);
        }
        // Sparkle-word decorations are cell-row tagged (no pixel y); shift the row down.
        for d in &mut self.word_decorations {
            d.row = d.row.saturating_add(strip as u16);
        }
        // Animated-ink overrides are cell-row tagged too; emitted pre-splice (the one
        // splice rule) so the uniform shift keeps them on their matched glyphs.
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
        self.rows += strip;
        // The strip changes the presented pixels; bump the snapshot seq so the renderer's
        // content cache sees the new frame.
        self.snapshot_seq = self.snapshot_seq.wrapping_add(1);
        // Record the prepend: under the blessing read at the top the lane gains
        // `strip` leading unknowns and the provenance token follows
        // `snapshot_seq`; unblessed, the lane is disowned. `row_shift` records
        // the physical prepend either way.
        self.note_host_row_prepend(strip, blessed);
    }

    /// Whether any selection is live on this frame (the scalar one or any pane
    /// entry). Gates selection-aware render work (the sparkle freeze, ligature
    /// breaks, the scroll-blit veto).
    #[must_use]
    #[inline]
    pub fn any_selection(&self) -> bool {
        self.selection.has_selection() || !self.selections.is_empty()
    }

    /// Which selection paints a frame-space cell, or `None` when unselected.
    ///
    /// The single renderer predicate: CPU and GPU pass the frame row/column,
    /// and this applies the viewport (`display_offset`) projection before
    /// consulting [`TextSelection`]. With [`selections`](Self::selections)
    /// empty it is the scalar test and answers `Some(0)`; otherwise it returns
    /// the first entry whose clip and selection accept the cell (pane clips are
    /// disjoint, so first is only).
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
            // Scalar path, clip first; runs per painted cell on every
            // single-terminal frame.
            if self
                .selection_clip
                .is_some_and(|clip| !clip.contains(frame_row, frame_col))
            {
                return None;
            }
            let selection_row = i32::try_from(frame_row)
                .unwrap_or(i32::MAX)
                .saturating_sub(self.display_offset);
            let Ok(selection_col) = u16::try_from(frame_col) else {
                return None;
            };
            return self
                .selection
                .contains_cell(selection_row, selection_col, is_wide, is_wide_continuation)
                .then_some(0);
        }
        // Entry anchors are already in frame rows (pane origin + that pane's
        // `display_offset`), so no viewport projection applies here.
        let selection_row = i32::try_from(frame_row).unwrap_or(i32::MAX);
        let Ok(selection_col) = u16::try_from(frame_col) else {
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

    /// Whether a frame-space cell is selected after applying both the terminal's
    /// logical selection and any host composition clip: the boolean face of
    /// [`Self::selection_hit`].
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

    /// The inclusive selected column span on one frame row after applying the
    /// viewport projection and optional split-composition clip.
    ///
    /// A scan window, not an equality key: with [`selections`](Self::selections)
    /// non-empty it is the hull over every pane's span on the row. That suits
    /// the CPU and GPU sparse-tail guards, which re-test each cell with
    /// [`Self::selection_contains_cell`]; damage diffs use
    /// [`Self::selection_row_key`].
    ///
    /// The span is content-independent: [`TextSelection::contains_cell`] snaps
    /// a double-width glyph whole, so an edge on a lead paints the continuation
    /// one column past `hi`. That cell is always inside the materialized prefix
    /// (widening needs the other half present in `cells`), so it is never the
    /// omitted tail cell this window exists to catch.
    #[must_use]
    pub fn selection_row_span(&self, frame_row: usize) -> Option<(u16, u16)> {
        self.selection_row_keys(frame_row)
            .map(|key| (key.lo, key.hi))
            .reduce(|(a_lo, a_hi), (b_lo, b_hi)| (a_lo.min(b_lo), a_hi.max(b_hi)))
    }

    /// The lossless per-row selection damage key: one [`SelectionRowKey`] per
    /// selection that reaches `frame_row`, in list order, with the colours that
    /// band paints in.
    ///
    /// Damage diffs compare this. Unlike [`Self::selection_row_span`] it never
    /// merges two panes' spans, so moving one pane's edge inside an unchanged
    /// hull still changes the key, and per-pane colour changes are included.
    ///
    /// Writes into `out` (cleared first) so a per-frame diff allocates once.
    pub fn selection_row_key(&self, frame_row: usize, out: &mut Vec<SelectionRowKey>) {
        out.clear();
        out.extend(self.selection_row_keys(frame_row));
    }

    /// Every selection's [`SelectionRowKey`] on `frame_row`, in list order —
    /// the scalar selection alone when [`selections`](Self::selections) is
    /// empty, which replaces it otherwise. The one walk behind both
    /// [`Self::selection_row_span`] and [`Self::selection_row_key`].
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
    /// decorations, animated ink, cat sprites, supernova light and rain (atlas
    /// `Arc`s nulled). Used by the `image plain` introspection capture.
    pub fn clear_overlays(&mut self) {
        self.cursor_effect_style_override = None;
        self.cursor_trail.clear();
        self.cursor_glow_add.clear();
        self.glow_halo.clear();
        self.glow_under.clear();
        self.fire_patch.clear();
        // These records describe the vectors above, so a plain-image capture
        // must fail closed to exact vector comparison until a producer rebuilds
        // them for a later decorated frame.
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
        self.wallpaper = None;
    }

    /// The emoji grapheme-cluster string at viewport cell `(row, col)`, if this
    /// frame captured one there (a ZWJ / skin-tone / keycap sequence). Used by
    /// the GPU atlas builder to resolve keys exactly as the CPU blit does.
    ///
    /// The per-row list is sorted by column with one entry per column (built
    /// ascending; re-sorted after any BiDi reorder / pane compose), so this binary
    /// searches — a fully-dense row (a cluster on every cell) resolves in
    /// O(cols·log cols) across the GPU atlas loops instead of the O(cols²) a
    /// per-column linear scan gives.
    #[must_use]
    pub fn cluster_at(&self, row: usize, col: usize) -> Option<&str> {
        let row = self.clusters.get(row)?;
        row.binary_search_by_key(&col, |(c, _)| *c)
            .ok()
            .map(|i| row[i].1.as_ref())
    }

    /// The combining marks to overlay at viewport cell `(row, col)`, if any.
    /// (The list is sorted by column — see [`cluster_at`](Self::cluster_at) — so
    /// this binary-searches.)
    #[must_use]
    pub fn combining_at(&self, row: usize, col: usize) -> Option<&[char]> {
        let row = self.combining.get(row)?;
        row.binary_search_by_key(&col, |(c, _)| *c)
            .ok()
            .map(|i| row[i].1.as_ref())
    }

    /// The live default background governing viewport cell `(row, col)`.
    ///
    /// Composed split frames can carry one sorted [`DefaultBgSpan`] per pane.
    /// Missing/empty rows, divider gaps, and malformed spans fall back to the
    /// frame-wide [`default_bg`](Self::default_bg). The returned
    /// value remains `COLOR_UNSET` when that is what the producer supplied;
    /// renderers resolve that sentinel against their configured theme.
    #[must_use]
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
    pub fn image_at(&self, row: usize, col: usize) -> Option<&aterm_grid::ImageRef> {
        self.images_at(row, col).last().map(|(_, image)| image)
    }

    /// Every inline-image layer covering viewport cell `(row, col)`, bottom
    /// first — the order both renderers draw them in.
    #[must_use]
    pub fn images_at(&self, row: usize, col: usize) -> &[(usize, aterm_grid::ImageRef)] {
        self.images
            .get(row)
            .map_or(&[], |row| images_in_row_at(row, col))
    }

    /// The DEC line-size run governing (`row`, `col`): its line size and the
    /// composite column range `start_col..end_col` it spans.
    ///
    /// Both renderers place glyphs horizontally through this, and the range is
    /// also the clip box, so a double-width run never bleeds past its pane. An
    /// empty [`line_size_spans`](Self::line_size_spans) row (every single-pane
    /// frame) yields the uniform run `0..cols` at
    /// [`line_sizes`](Self::line_sizes)`[row]`. Spans partition `0..cols`; an
    /// out-of-range column falls back to the uniform run rather than panicking.
    #[must_use]
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
        // A row with runs ignores `line_sizes[row]` (only a summary), and a
        // column no run claims (a divider seam, padding beside a pane) renders
        // single-width.
        let gap = (LineSize::SingleWidth, 0, self.cols);
        // Ascending, non-overlapping: the last run starting at or before `col`
        // is the only candidate.
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

    /// Whether an image at (`row`,`col`) hides the cell's glyph: one of its
    /// layers is drawn over the text (`z_index >= 0`, the default). A Kitty
    /// `z < 0` image is drawn behind the text and does not hide it. Both
    /// renderers gate glyph drawing on this.
    #[must_use]
    pub fn image_hides_glyph_at(&self, row: usize, col: usize) -> bool {
        self.images_at(row, col)
            .iter()
            .any(|(_, image)| image.image.z_index >= 0)
    }

    /// Composite an IME preedit (the in-flight composition text) into this
    /// frame at the cursor, inline at the caret.
    ///
    /// Pure frame compositing: `cells` is a per-frame projection of the grid,
    /// so no terminal state changes; the composition shows while the host
    /// keeps passing it, and the committed text arrives through the PTY echo.
    ///
    /// Presentation rules:
    /// * drawn from the cursor cell, in the cursor cell's colors, single
    ///   underline (the IME convention for uncommitted text), no bold/italic
    ///   inheritance;
    /// * wide characters occupy a lead cell plus a `wide` continuation;
    /// * truncated at the right edge (no wrap); the platform popover shows the
    ///   full text;
    /// * zero-width characters (combining marks from a dead key) merge into
    ///   the preceding cell and advance nothing;
    /// * the cursor moves to the caret: `caret_byte` is the platform's byte
    ///   offset (winit `Ime::Preedit`'s cursor start); `None` or past-the-end
    ///   parks it after the last composed cell, clamped to the final column.
    ///
    /// `cjk` selects East-Asian-Ambiguous width, mirroring the write path's
    /// `char_width` so the composition occupies the columns the committed text
    /// will.
    ///
    /// A snapshot row carries only its written prefix (`render_row_into_impl`
    /// pushes `view.len()` cells), and at a shell prompt the cursor sits at its
    /// end. The overlay therefore materializes the cells it paints, padding any
    /// gap with the frame's live implicit blank rather than a clone of the last
    /// written cell, whose background would smear across the tail.
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
        // The style seed: the cursor cell's resolved colors. Past the written
        // prefix there is no cursor cell, so the live implicit blank is used
        // (a default `RenderCell` is black on black); the row's last real cell
        // covers a producer that resolved no defaults (a pristine
        // `Terminal::new`).
        let seed = cells
            .get(self.cursor_col)
            .copied()
            .or(implicit)
            .or_else(|| cells.last().copied())
            .unwrap_or_default();
        // The pad for the gap between the row's end and the cursor: a blank in
        // the seed's colors with every decoration dropped, so it does not pick
        // up the composition's underline.
        let pad = RenderCell {
            ch: ' ',
            fg: seed.fg,
            bg: seed.bg,
            ..RenderCell::default()
        };
        let start_col = self.cursor_col;
        // A composition that starts on a wide character's continuation column
        // overwrites its right half, so its lead goes too; otherwise the
        // renderer draws the old glyph across both columns over the
        // composition. Read before the walk overwrites it.
        let straddled_lead =
            (start_col > 0 && cells.get(start_col).is_some_and(|c| c.wide)).then(|| start_col - 1);
        let (end_col, caret_col) = walk_ime_preedit(
            preedit,
            caret_byte,
            cjk,
            start_col,
            cols,
            |ch, col, width| {
                // Materialize the ragged tail up to (and including) the cells
                // this character occupies — the walk proves `col + width <=
                // cols`, so the row never grows past the frame's width.
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
        // A cell that held a multi-scalar grapheme also has it in `clusters`
        // (and marks in `combining`), which both renderers prefer over `ch`;
        // drop those so the old grapheme is not painted over the composition.
        // Free when the row has no complex graphemes.
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

    /// The frame's implicit blank at viewport cell (`row`, `col`): a space in
    /// the live default colours (OSC 10/11, their resets, DECSCNM, and on a
    /// composed split frame the per-pane
    /// [`default_bg_spans`](Self::default_bg_spans)), as the terminal paints an
    /// unwritten column.
    ///
    /// `None` when the producer resolved no defaults ([`COLOR_UNSET`]): the
    /// sentinel is not a colour, and masking its high byte would read as black.
    fn implicit_blank_at(&self, row: usize, col: usize) -> Option<RenderCell> {
        let (fg, bg) = (self.default_fg, self.default_bg_at(row, col));
        if fg == COLOR_UNSET || bg == COLOR_UNSET {
            return None;
        }
        // `0x00RRGGBB` big-endian: the high byte is the (zero) tag, the rest
        // are the channels — a byte split, so nothing can truncate.
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

/// The column walk an IME composition takes: every character's width under
/// the frame's ambiguous-width mode, the right-edge truncation, and where the
/// platform's caret byte lands.
///
/// [`RenderInput::overlay_ime_preedit`] paints through `emit` (called once per
/// character that fits, with its scalar, starting column and width);
/// [`ime_preedit_caret_col`] passes a no-op and keeps only the caret, so the
/// ink and the candidate-window anchor share one computation.
///
/// Returns `(end_col, caret_col)`: the column the composition stopped at
/// (truncation included) and the caret's final, row-clamped column.
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
        // Zero-width characters (a combining mark fresh off a dead key) merge
        // into the preceding cell like the write path would: nothing to draw,
        // nothing to advance.
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

/// The composition's own caret column: the same answer
/// [`RenderInput::overlay_ime_preedit`] leaves in `cursor_col`, without
/// painting.
///
/// The OS candidate window is anchored on the caret rectangle the host
/// reports (`set_ime_cursor_area`). While a composition is in flight the
/// engine cursor still sits where the composition starts, so anchoring there
/// would leave the candidate list under the start of the phrase.
///
/// `start_col` is the frame's cursor column and `cols` the frame width, in the
/// overlay's coordinate space (pane-local for a split leaf).
#[must_use]
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
    fn cjk_preedit_occupies_lead_plus_wide_continuation() {
        let mut f = frame(4, 10, (0, 2));
        f.overlay_ime_preedit("日本", None, false);
        assert_eq!(f.cells[0][2].ch, '日');
        assert!(
            f.cells[0][3].wide,
            "continuation column carries the wide flag"
        );
        assert_eq!(f.cells[0][4].ch, '本');
        assert!(f.cells[0][5].wide);
        assert_eq!(
            f.cells[0][3].underline,
            UnderlineStyle::Single,
            "the underline spans the continuation"
        );
        assert_eq!(f.cursor_col, 6);
    }

    #[test]
    fn preedit_truncates_at_the_edge_and_a_wide_char_never_straddles_it() {
        let mut f = frame(2, 5, (0, 2));
        // '日' is width 2: cols 2-3 fit, the second '本' (cols 4-5) would
        // straddle the edge and must not be half-drawn.
        f.overlay_ime_preedit("日本", None, false);
        assert_eq!(f.cells[0][2].ch, '日');
        assert_eq!(
            f.cells[0][4].ch, ' ',
            "the straddling char is truncated whole"
        );
        assert_eq!(f.cursor_col, 4, "caret clamps inside the row");
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

/// Overlay tests on real `Terminal` extractions, whose rows are ragged: a row
/// holds only its written prefix, and at a shell prompt the cursor sits at its
/// end.
#[cfg(test)]
mod ime_preedit_ragged_row_tests {
    use super::{COLOR_UNSET, RenderInput};
    use crate::terminal::{RenderCell, Rgb, Terminal, UnderlineStyle};

    /// A terminal at a prompt: `$ ` written, cursor parked after it. Colors are
    /// host-configured, as every windowed frame's are, so the frame carries
    /// resolved `default_fg` / `default_bg` rather than the `COLOR_UNSET` a
    /// pristine `Terminal::new` reports.
    fn prompt_frame(rows: u16, cols: u16) -> RenderInput {
        let mut term = Terminal::new(rows, cols);
        term.set_default_foreground(Rgb::new(0xC0, 0xC5, 0xCE));
        term.set_default_background(Rgb::new(0x1B, 0x1D, 0x1E));
        term.process(b"$ ");
        let mut f = RenderInput::empty();
        term.cell_frame_into(&mut f, rows.into(), cols.into());
        f
    }

    /// The extracted prompt row really is short, so the tests below exercise
    /// the ragged case.
    #[test]
    fn the_fixture_row_stops_at_the_written_prefix() {
        let f = prompt_frame(4, 20);
        assert_eq!(f.cells[0].len(), 2, "a snapshot row is the written prefix");
        assert_eq!(
            (f.cursor_row, f.cursor_col),
            (0, 2),
            "and the prompt's caret sits exactly at its end"
        );
        assert_ne!(f.default_bg, COLOR_UNSET, "a configured host resolves both");
        assert_ne!(f.default_fg, COLOR_UNSET);
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

    /// The caret must not run ahead of ink. Before the fix these two disagreed:
    /// the cursor advanced by the composition's width over cells that stayed
    /// unwritten, parking the block cursor in blank space four columns out.
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

    /// A producer that resolved NO defaults (a pristine `Terminal::new`, a
    /// standalone renderer harness) still gets legible composition: the row's
    /// own last cell seeds the colors rather than a black placeholder.
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

/// Edge cases of row materialization, driven from real `Terminal`
/// extractions: a full row is left as is, a composition that does not fit
/// does not grow the row before truncation, column 0 of an empty row works,
/// and the `cjk` argument reaches the width call.
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

    /// Column 0 of a row with NO written prefix at all — `cells[row]` is empty,
    /// the extreme of the raggedness. This is the second line of a wrapped
    /// prompt, and every line of a freshly cleared screen.
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

    /// Wider than the row's remainder: the composition grows the row to the
    /// frame's width and stops there; the gate proves `col + width <= cols`
    /// before the `resize`.
    #[test]
    fn a_composition_longer_than_the_row_remainder_stops_at_the_frame_edge() {
        let mut f = painted_frame(2, 8, b"$ ");
        // に ほ ん take columns 2..8; ご would need 8..10.
        f.overlay_ime_preedit("にほんご", None, false);
        assert_eq!(
            f.cells[0].len(),
            8,
            "grown to the frame width, never past it"
        );
        let row: String = f.cells[0].iter().map(|c| c.ch).collect();
        assert_eq!(row, "$ に ほ ん ");
        assert_eq!(f.cursor_col, 7, "the caret clamps to the last column");
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

    /// The `cjk` argument — DECSET 8840 / `ambiguous_width_double`, read off the
    /// same `Terminal` under the same lock as the extraction — must reach the
    /// width call, or the composition would occupy different columns than the
    /// committed text.
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

    /// While a composition is in flight the engine cursor has not moved, so
    /// the caret is the engine column plus everything typed so far.
    #[test]
    fn the_anchor_moves_with_the_composition_the_engine_cursor_does_not() {
        let engine_col = 2;
        let mut last = engine_col;
        for phrase in ["に", "にほ", "にほん", "にほんご"] {
            let caret = ime_preedit_caret_col(phrase, None, false, engine_col, 40);
            assert!(
                caret > last,
                "{phrase:?} must anchor right of {last} (got {caret})"
            );
            last = caret;
        }
        assert_eq!(
            last, 10,
            "four full-width kana past the prompt — eight columns the engine \
             cursor knows nothing about"
        );
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

    /// A run governs exactly its own columns; a column outside every run is
    /// single width, which keeps one pane's DECDWL off its neighbour.
    #[test]
    fn a_run_governs_only_its_own_columns() {
        let mut i = input(1, 11);
        i.line_size_spans[0] = vec![LineSizeSpan {
            start_col: 6,
            end_col: 11,
            line_size: LineSize::DoubleWidth,
        }];
        for c in 0..6 {
            assert_eq!(
                i.line_size_run_at(0, c),
                (LineSize::SingleWidth, 0, 11),
                "column {c} sits outside the run"
            );
        }
        for c in 6..11 {
            assert_eq!(
                i.line_size_run_at(0, c),
                (LineSize::DoubleWidth, 6, 11),
                "column {c} sits inside the run"
            );
        }
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

    /// Differential check of [`super::images_eq`] against the derived
    /// equality of the placements with Kitty tags stripped (a tag-only
    /// difference is equal). At the `RenderInput` level, a re-transmit (fresh
    /// `Arc`s, identical bytes) compares equal and a one-byte payload change
    /// compares unequal.
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
        // The per-row (col, _) lists are sorted by column, one entry per
        // column, and the accessors binary-search them: check each populated
        // column (with gaps between) resolves the right entry and absent
        // columns return None.
        let mut input = RenderInput::empty();
        // Sparse, ascending columns with gaps (0, 5, 100) as a reordered/dense row
        // would produce after the re-sort.
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
mod line_size_span_tests {
    use super::{LineSizeSpan, RenderInput};
    use crate::grid::LineSize;

    #[test]
    fn spans_are_render_content_and_survive_both_clone_paths() {
        let mut source = RenderInput::empty();
        source.rows = 1;
        source.cols = 9;
        source.line_sizes = vec![LineSize::SingleWidth];
        source.line_size_spans = vec![vec![
            LineSizeSpan::new(0, 4, LineSize::DoubleWidth),
            LineSizeSpan::new(5, 9, LineSize::SingleWidth),
        ]];

        let cloned = source.clone();
        assert_eq!(cloned.line_size_spans, source.line_size_spans);
        assert_eq!(cloned, source);

        let mut reused = RenderInput::empty();
        reused.line_size_spans = vec![vec![LineSizeSpan::new(0, 99, LineSize::DoubleHeightBottom)]];
        reused.clone_from(&source);
        assert_eq!(reused.line_size_spans, source.line_size_spans);
        assert_eq!(reused, source);

        let mut changed = source.clone();
        changed.line_size_spans[0][0].line_size = LineSize::SingleWidth;
        assert_ne!(
            changed, source,
            "pane-local DEC geometry changes rendered pixels"
        );
    }

    #[test]
    fn cursor_fill_override_is_content_cloned_and_cleared_with_overlays() {
        let mut source = RenderInput::empty();
        source.cursor_fill_override = Some(0x0012_3456);

        let mut reused = RenderInput::empty();
        reused.clone_from(&source);
        assert_eq!(reused.cursor_fill_override, source.cursor_fill_override);
        assert_eq!(reused, source);

        let bare = RenderInput::empty();
        assert_ne!(
            source, bare,
            "a stationary cursor fill color changes rendered content"
        );

        source.clear_overlays();
        assert_eq!(source.cursor_fill_override, None);
        assert_eq!(source, bare);
    }

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

        // Every field of a per-pane entry paints, so every one moves `PartialEq`
        // — a silent drop here is a stale highlight in the renderer's cached
        // prior frame, the exact class `clone_from`'s doc records.
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

        // The list clause of `any_selection` alone: scalar empty, one live entry
        // (an unfocused pane in a composed frame). The scroll-blit veto, the
        // sparkle freeze and the ligature-break pass all ask `any_selection()`.
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

        // 1. Any host cell write. The established discipline is one
        // `snapshot_seq` bump and nothing else — no mutator touches the token.
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

    #[test]
    fn absolute_row_revision_is_cloned_but_not_render_content() {
        let base = RenderInput::empty();
        let mut stamped = base.clone();
        stamped.absolute_row_revision = 7;

        assert_eq!(
            base, stamped,
            "absolute-row revision is host metadata, not raster content"
        );
        assert_eq!(stamped.clone().absolute_row_revision, 7);

        let mut reused = RenderInput::empty();
        reused.clone_from(&stamped);
        assert_eq!(
            reused.absolute_row_revision, 7,
            "capacity-reusing snapshots preserve the frame stamp"
        );
    }

    /// The `image plain` contract: `clear_overlays` clears the rain quads and
    /// nulls its atlas `Arc` (a dangling atlas would keep the GPU upload
    /// alive), and also strips `glow_halo`, `glow_under` and `char_fg`.
    #[test]
    fn clear_overlays_strips_all_three_rain_channels() {
        let mut input = populated();
        input.clear_overlays();
        assert!(input.rain_quads.is_empty(), "rain quads must be cleared");
        assert!(input.rain_atlas.is_none(), "rain atlas Arc must be nulled");
        assert!(input.rain_add.is_empty(), "rain halos must be cleared");
        assert!(input.glow_halo.is_empty(), "glow halos must be cleared");
        assert!(input.glow_under.is_empty(), "glow_under must be cleared");
        assert!(input.fire_patch.is_empty(), "fire_patch must be cleared");
        assert!(input.char_fg.is_empty(), "char_fg must be cleared");
        assert!(input.fire_halo.is_empty(), "fire_halo must be cleared");
        assert_eq!(input, RenderInput::empty(), "stripped == bare empty frame");
    }

    /// Rain is content, so a change in any channel compares unequal. The atlas
    /// compares by identity (`Arc::as_ptr`), since baker versions replay
    /// across rebuilt engines: a fresh publish is unequal, and the same `Arc`
    /// re-presented stays equal.
    #[test]
    fn partial_eq_sees_rain_content_and_atlas_identity() {
        let base = populated();

        let mut quads_changed = base.clone();
        quads_changed.rain_quads[1].y += 16;
        assert_ne!(
            base, quads_changed,
            "rain_quads content change must be seen"
        );

        let mut add_changed = base.clone();
        add_changed.rain_add[0].color = 0x0001_0101;
        assert_ne!(base, add_changed, "rain_add change must be seen");

        let mut halo_changed = base.clone();
        halo_changed.glow_halo[0].rx += 1;
        assert_ne!(base, halo_changed, "glow_halo change must be seen");

        // The blend mode IS content: an Add ember becoming an Over veil must
        // miss the damage gate (defaults pinned so legacy streams stay Add).
        assert_eq!(HaloMode::default(), HaloMode::Add, "legacy halos stay Add");
        let mut mode_changed = base.clone();
        mode_changed.glow_halo[0].mode = HaloMode::Over;
        assert_ne!(base, mode_changed, "glow_halo mode change must be seen");

        let mut under_changed = base.clone();
        under_changed.glow_under[0].color = 0x0001_0101;
        assert_ne!(base, under_changed, "glow_under change must be seen");

        // The fire field's phase IS content (the animation clock both backends
        // key the field on), as is its blend mode (defaults pinned so legacy
        // streams stay Add — the dark-theme emission).
        assert_eq!(FireMode::default(), FireMode::Add, "legacy fire stays Add");
        let mut fire_changed = base.clone();
        fire_changed.fire_patch[0].phase += 16;
        assert_ne!(base, fire_changed, "fire_patch phase change must be seen");
        let mut fire_mode_changed = base.clone();
        fire_mode_changed.fire_patch[1].mode = FireMode::Over;
        assert_ne!(
            base, fire_mode_changed,
            "fire_patch mode change must be seen"
        );

        let mut char_fg_changed = base.clone();
        char_fg_changed.char_fg[1].fg = 0x0000_0000;
        assert_ne!(base, char_fg_changed, "char_fg change must be seen");

        // The contrast-halo strength is content too (it scales the stamp
        // alpha).
        let mut fire_halo_changed = base.clone();
        fire_halo_changed.fire_halo[1].strength = 40;
        assert_ne!(
            base, fire_halo_changed,
            "fire_halo strength change must be seen"
        );

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

    /// The capacity-reusing damage-cache path: `clone_from` omissions compile
    /// fine (unlike `clone`'s struct literal), so pin that all three channels
    /// actually arrive — a miss stores a stale channel and silently corrupts
    /// the dirty diff.
    #[test]
    fn clone_from_copies_all_three_rain_channels() {
        let source = populated();

        // Non-empty destination: clone_from must overwrite stale rain, not merge.
        let mut dst = RenderInput::empty();
        dst.rain_quads = vec![rain_quad(9); 8];
        dst.rain_atlas = Some(rain_atlas(99, 0xAA));
        dst.rain_add = vec![rain_halo(9); 8];
        dst.glow_halo = vec![rain_halo(9); 8];
        dst.glow_under = vec![under_quad(9); 8];
        dst.fire_patch = vec![fire_patch(9); 8];
        dst.char_fg = vec![char_fg(9, 9); 8];
        dst.fire_halo = vec![fire_halo(9, 9); 8];
        dst.clone_from(&source);
        assert_eq!(dst.rain_quads, source.rain_quads);
        assert_eq!(
            dst.rain_atlas.as_ref().map(std::sync::Arc::as_ptr),
            source.rain_atlas.as_ref().map(std::sync::Arc::as_ptr),
            "clone_from carries the SAME published Arc (identity, the eq law)"
        );
        assert_eq!(dst.rain_add, source.rain_add);
        assert_eq!(dst.glow_halo, source.glow_halo);
        assert_eq!(dst.glow_under, source.glow_under);
        assert_eq!(dst.fire_patch, source.fire_patch);
        assert_eq!(dst.char_fg, source.char_fg);
        assert_eq!(dst.fire_halo, source.fire_halo);
        assert_eq!(dst, source, "clone_from == clone, byte-for-byte");
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

    /// The canonical record-word schema covers every renderer-visible field.
    /// Both producer damage and the cursor scheduler consume these methods, so
    /// this one exhaustive table prevents either fingerprint from drifting.
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
        // A flat quad folds to two words; a gradient appends a third (see
        // `GlowQuad::damage_words`).
        assert_eq!(glow.damage_words().as_slice().len(), 2);
        assert_eq!(
            GlowQuad {
                color2: 0x65_4321,
                ..glow
            }
            .damage_words()
            .as_slice()
            .len(),
            3
        );
        assert_eq!(
            glow.damage_words().as_slice(),
            &[
                super::pack_u16x4(glow.row, glow.x, glow.y, glow.w),
                glow.h as u64 | (glow.color as u64) << 16 | (glow.alpha as u64) << 48,
            ][..],
            "a flat quad's words are the pre-gradient pair, bit for bit"
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

    /// The producer retains only a row bitmap for the hot 8k `glow_under`
    /// shape. Field coverage is pinned cheaply in the canonical-word test
    /// above; this test owns the large-stream memory and end-to-end control.
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

    /// `Translate` is "placed below, then placed": a web frame whose producers
    /// never counted the prepended rows comes out of the splice exactly as a
    /// native frame whose producers did (every stream, tag and the clip), and
    /// `Placed` moves no window-space pixel. Negative control: `Placed` on the
    /// web frame leaves the aurora on its old row.
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
}
