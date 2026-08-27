// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The engine-owned render SNAPSHOT (`read_image` boundary, REARCH A-3).
//!
//! [`RenderInput`] is the plain-owned, `Terminal`-free value a renderer reads to
//! paint one frame. Historically the CPU renderer reached into `&Terminal`
//! internals to build it (`Renderer::extract_into`); A-3 inverts that boundary so
//! the ENGINE produces the snapshot ([`Terminal::cell_frame_into`]) and the
//! renderer becomes a PURE consumer of this value — no `&Terminal`, no reach into
//! core. Hosting the type HERE (rather than in `aterm-render-api`) lets
//! `aterm-core` build the snapshot without a dependency cycle: `aterm-render-api`
//! re-exports `RenderInput` from here, so every existing
//! `aterm_render::RenderInput` / `aterm_render_api::RenderInput` call site is
//! unchanged.

use crate::grid::LineSize;
use crate::selection::TextSelection;
use crate::terminal::{CursorStyle, RenderCell};
use aterm_hash::FxHashMap;

/// One faded cell of the cursor MOTION TRAIL (the "streaming trailer" effect): a
/// cell the cursor recently swept through, painted in the trail colour at `alpha`
/// coverage over the cell's own background. The whole trail fades out within a
/// few hundred milliseconds of the move, so it reads as a comet behind the cursor
/// and makes cursor motion (e.g. a Ctrl-A/Ctrl-E jump) unmistakable.
///
/// Position is in the SAME viewport coordinate space as
/// [`RenderInput::cursor_row`]/[`RenderInput::cursor_col`] (i.e. after the
/// tab-strip splice and any split-pane offset), so the renderer draws each trail
/// cell with the IDENTICAL `pad + col*cell_w` / `pad + row*cell_h` geometry it
/// uses for the cursor. The renderer is a pure consumer: the host (the windowed
/// frontend) owns the animation clock and hands the renderer the already-resolved
/// `alpha` per cell each frame, so the renderer stays deterministic (CPU/GPU
/// byte-parity is preserved — an empty trail is byte-identical to the pre-trail
/// path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrailCell {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: usize,
    /// Viewport column (0-indexed).
    pub col: usize,
    /// Blend coverage `0..=255` of the trail colour over this cell's background
    /// (`0` = invisible, `255` = the solid trail colour). Drawn OPAQUELY as the
    /// pre-blended result, so the CPU fill and the GPU `REPLACE` quad land the
    /// identical pixel.
    pub alpha: u8,
}

/// One additive LIGHT quad of the "LUMEN WAKE" cursor aurora — a fragment of
/// emitted light (comet body, bloom-crown slab, landing-ring strip, or a spark
/// particle) composited with PREMULTIPLIED ADDITIVE blend (`dst + src`, saturating).
///
/// The host pre-multiplies the colour by its coverage (so `color` is already the
/// light to add) and the renderer simply does a per-channel saturating add — which
/// is order-independent and BYTE-EXACT between the GPU (`One`/`One` blend over the
/// linear `Rgba8Unorm` target) and the CPU (`add_sat`). Additive light only ever
/// BRIGHTENS, so the text underneath stays legible (no smear, no darkening), and an
/// empty `cursor_glow_add` is byte-identical to the pre-aurora render path.
///
/// INVARIANTS the host upholds so the renderer is a dumb, parity-safe consumer:
/// (1) the coordinate convention is PER STREAM — the cursor-effect streams
/// (`cursor_glow_add`, `glow_under`) are WINDOW-ABSOLUTE pixels (the
/// window-space effects layer: the producer folds in the grid origin and clamps
/// to the EFFECTS BOX — the grid plus the chrome head band above it — so the
/// renderer adds NO offset), while the grid-anchored `nova_add` stream stays
/// GRID-INTERIOR (its word_decorations producer knows no window origin; the
/// renderer offsets it like every other grid stream); (2)
/// every quad lies within EXACTLY ONE grid-anchored cell-row band — a halo/ring
/// spilling into a neighbour row is emitted as a SEPARATE quad tagged with that
/// row — so the row-scoped dirty gate + GPU scissor cover it exactly. `row` is
/// a grid-row DAMAGE HINT: an above-grid quad tags row 0, which opens the top
/// band on both presenters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlowQuad {
    /// The viewport row band this quad lives in — a grid-row DAMAGE HINT for
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
    /// PREMULTIPLIED light colour `0x00RRGGBB` — added (saturating) onto the dest.
    pub color: u32,
}

/// How a [`RainHalo`]'s radially-weighted colour composites onto the frame.
///
/// Every legacy stream is [`Add`](HaloMode::Add) — pure premultiplied light,
/// invisible over a white background (you cannot brighten white into smoke).
/// [`Over`](HaloMode::Over) is the light-theme answer: the quad becomes a
/// source-over VEIL whose per-pixel opacity is the radial falloff weight, so
/// grey smoke / pale steam / any darkening effect reads on ANY background.
/// Within one stream, every `Add` quad composites BEFORE every `Over` quad
/// (veils dim light — the GPU's per-mode split draw order; the CPU rasterizer
/// mirrors it), so overlapping mixed-mode halos stay parity-exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HaloMode {
    /// PREMULTIPLIED saturating-add light (CPU `add_sat` == GPU One/One on the
    /// Unorm view) — the historical mode; byte-identical to the pre-mode path.
    #[default]
    Add,
    /// SOURCE-OVER veil: `color` is a STRAIGHT (unpremultiplied) RGB and the
    /// radial falloff weight is the per-pixel ALPHA of a source-over composite
    /// (centre most opaque, fading to 0 at the radii) — light or dark veils
    /// that read on any background (the light-theme smoke/steam law).
    Over,
}

/// A PHOSPHOR rain bright-head halo: like [`GlowQuad`] (a single-row-band
/// premultiplied additive quad) but its light falls off
/// RADIALLY from the head centre instead of filling the rect flat. `color` is
/// the PEAK (centre) light; each covered pixel scales it by an integer
/// elliptical falloff — `weight = ((256 − nsq)² >> 8)` clamped, where
/// `nsq = (dx²·256)/rx² + (dy²·256)/ry²` with `(dx, dy)` the pixel's offset
/// from `(cx, cy)` — then `add_sat`s it (or, in [`HaloMode::Over`], uses the
/// weight as the per-pixel source-over ALPHA of the straight `color`). The
/// falloff is pure integer math so the CPU rasterizer and the GPU
/// `fs_rain_glow`/`fs_rain_glow_over` shaders compute it byte-for-byte
/// identically (the halo-parity contract). Coordinates follow the [`GlowQuad`]
/// convention PER STREAM: cursor-effect streams (`glow_halo`) are
/// WINDOW-ABSOLUTE px (the producer folds in the grid origin); the grid-anchored
/// `rain_add` stream stays grid-interior (the renderer offsets it). `row` is a
/// grid-row DAMAGE HINT (above-grid quads tag row 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RainHalo {
    /// The viewport row band this quad lives in — a grid-row DAMAGE HINT for
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
    /// The halo-centre colour `0x00RRGGBB`. In [`HaloMode::Add`] this is the
    /// PREMULTIPLIED PEAK light (scaled down per pixel by the radial falloff
    /// before the saturating add); in [`HaloMode::Over`] it is a STRAIGHT
    /// (unpremultiplied) veil colour (the falloff scales its OPACITY instead).
    pub color: u32,
    /// Pixel X of the halo CENTRE (the bright core / falloff origin; same
    /// coordinate convention as `x`). May lie outside this quad's row band
    /// (a halo spans up to 3).
    pub cx: u16,
    /// Pixel Y of the halo CENTRE (same convention as `y`).
    pub cy: u16,
    /// Horizontal elliptical falloff half-extent in pixels — light reaches 0 at
    /// this radius. `>= 1` (the emitter guarantees it; 0 would divide by zero).
    pub rx: u16,
    /// Vertical elliptical falloff half-extent in pixels — light reaches 0 at
    /// this radius. `>= 1` (the emitter guarantees it; 0 would divide by zero).
    pub ry: u16,
    /// How the radially-weighted colour composites (defaults to the historical
    /// [`HaloMode::Add`]; [`HaloMode::Over`] is the light-theme veil).
    pub mode: HaloMode,
}

/// How a [`FirePatch`]'s per-pixel field output composites onto the frame.
///
/// [`Add`](FireMode::Add) is the dark-theme flame: the field emits
/// PREMULTIPLIED light (`palette·coverage`) saturating-added onto the dest
/// (CPU `add_sat` == GPU One/One on the Unorm view) — pure emission,
/// byte-invisible over white. [`Over`](FireMode::Over) is the light-theme
/// flame: the SAME field shapes a straight ink palette + per-pixel alpha
/// composited source-over (CPU `over_rgb` == GPU SrcAlpha/OneMinusSrcAlpha on
/// the same Unorm view, the [`HaloMode::Over`] contract) — fire as PAINT that
/// reads on any background. Within one stream every `Add` patch composites
/// BEFORE every `Over` patch (the GPU's per-mode split draw order; the CPU
/// rasterizer mirrors it), so overlapping mixed-mode patches stay parity-exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FireMode {
    /// PREMULTIPLIED saturating-add flame light — the dark-theme mode.
    #[default]
    Add,
    /// SOURCE-OVER ink-fire: straight RGB + field-shaped alpha — the
    /// light-theme mode (readable on white).
    Over,
}

/// One EMBERFORGE **FirePatch**: a per-pixel, pure-integer procedural FIRE
/// FIELD evaluated at every device pixel of the patch quad by BOTH backends —
/// the full-art-scale generalization of the [`RainHalo`] parity trick. The
/// shared field function (`aterm_render::fire_field`, mirrored op-for-op by
/// the GPU `fs_fire_add`/`fs_fire_over` WGSL) maps `(window_px, window_py,
/// patch params)` to a palette colour + coverage with NO state and NO floats,
/// so the CPU rasterizer and the GPU fragment shader land the IDENTICAL byte
/// (the fire-parity contract).
///
/// Quads obey the [`RainHalo`] cursor-stream invariants: WINDOW-ABSOLUTE
/// pixels (the window-space effects layer — the producer folds in the grid
/// origin, the renderer adds NO offset; flames may light the head band above
/// the grid), each within EXACTLY ONE grid-anchored cell-row band (`row` is a
/// grid-row DAMAGE HINT; above-grid patches tag row 0) — a flame spanning rows
/// is emitted as per-row patches. The field itself is a function of ABSOLUTE
/// pixel coordinates plus the patch parameters, so patches sharing a burn
/// (same `base_y`/`peak_h`/`phase`/`temp`/`lean`/`cell_h`) are CONTINUOUS
/// across patch boundaries — zero seams; splitting a wide patch in two is
/// byte-identical. `base_y`, like a halo's `cx`/`cy`, may lie outside the
/// patch's own row band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FirePatch {
    /// The viewport row band this quad lives in — a grid-row DAMAGE HINT for
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
    /// Window-absolute pixel Y of the flame ROOT (field `v = 0`; flames rise
    /// UPWARD, so live pixels have `y < base_y`). Shared across a burn's
    /// patches so the field is continuous vertically.
    pub base_y: u16,
    /// Maximum flame height in pixels — the tongue-height envelope at this
    /// patch. `0` is treated as `1` by the field (total function).
    pub peak_h: u16,
    /// Churn phase in units of 1/1024 s, QUANTIZED by the producer so both
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

/// One textured sprite quad for a renderer-owned animated overlay. A `SpriteQuad` is a rectangle
/// sampled from an RGBA8 sprite atlas (procedurally baked, or supplied by a sprite
/// sheet) and composited SOURCE-OVER (straight alpha) onto the frame — the SAME blend
/// the inline-image / colour-emoji / settings-tray paths already use, so the CPU fill
/// and the GPU sampled quad land the IDENTICAL pixel (parity by construction).
///
/// The host effects engine owns ALL art and animation and
/// hands the renderer a fully-resolved list each frame; the renderer is a dumb,
/// parity-safe consumer. The host upholds the same dirty-gate invariants as
/// [`GlowQuad`]: (1) the dest rect is in GRID-INTERIOR pixels (pad-relative; the
/// renderer offsets by `pad`), clamped to the grid; (2) every quad lies within EXACTLY
/// ONE cell-row band (`row`) — a sprite spanning rows is emitted as per-row slices —
/// so the row-scoped dirty gate + GPU scissor cover it exactly. Atlas coordinates are
/// integer TEXELS (not normalized UV) so the type stays `Copy + Eq` and the damage
/// cache compares it byte-exactly (no float reflexivity hazard).
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

/// One FREE-FLOATING decorative sprite: an arbitrary pixel rectangle at an
/// arbitrary — even off-grid — position, with NO row tag. It is [`SpriteQuad`]
/// with the single-row-band invariant removed: the full `[y, y+h)` pixel extent
/// is authoritative for dirty tracking (the shared dirty computation row-unions
/// every cell-row band the extent overlaps, prev∪cur), so a sprite may span any
/// number of row bands and needs no host-side per-row slicing.
///
/// The dest origin is in GRID-INTERIOR pixels (pad-relative; the renderer
/// offsets by `pad`) and SIGNED: a cat peeking in from outside the grid (its
/// top edge up in the top pad strip, or rising from below the bottom edge) has
/// a negative / past-the-edge origin that `u16` cannot express. Extents and
/// atlas coordinates stay non-negative integer texels (not normalized UV), so
/// the type stays `Copy + Eq` and the damage cache compares it byte-exactly
/// (no float reflexivity hazard) — exactly the [`SpriteQuad`] contract.
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

/// How a [`FreeSprite`] samples its atlas. v1 renderers consume `Nearest` only
/// (the cat regime: host bakes at exact 1:1 dest size); `Linear` is carried in
/// the type and the damage compare from day one but deferred in the renderers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FreeSampler {
    /// NEAREST 1:1 (cats).
    #[default]
    Nearest,
    /// Bilinear (deferred — scaled sprite art).
    Linear,
}

/// An RGBA8 sprite atlas carried alongside a frame's [`SpriteQuad`]s.
/// Shared by `Arc` so cloning a [`RenderInput`] is cheap, and identified by a monotonic
/// `version` so the GPU re-uploads the texture only when the atlas actually changes (a
/// skin/theme switch). Straight-alpha, row-major, length `width*height*4`.
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

/// Which small procedural sprite a [`WordDecoration`] paints. These are
/// rasterized by the renderer (not the text font) into a 0/255 coverage mask so
/// the CPU fill and the GPU atlas land identical pixels — the same parity
/// contract as the cursor glow.
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
    /// A water droplet / teardrop (the orca "splash" mark): a round bulb low in the
    /// cell tapering to a point at the top.
    Droplet,
    /// An annulus-arc ring segment filling the cell (Sparkle Words v2, the
    /// SINGULARITY nova's darkening ring): the supernova's additive streams can
    /// only brighten (`nova_add` is One/One), so the magic Singularity variant's
    /// collapse ring is stamped as one `DecoBlend::Over` decoration per covered
    /// cell, each carrying this soft annulus coverage mask tinted dark. Rasterized
    /// by `deco_coverage` like every other sparkle sprite (cell-sized,
    /// parameterized only by the cell geometry), so CPU and GPU sample the one
    /// shared mask.
    RingArc,
    /// A full-cell soft-edged square (Sparkle Words v3 §3.3, the SUPER NOVA's
    /// light-background "eclipse"): additive detonation light is invisible on
    /// light themes, so the escalated nova's dark veil is stamped as one
    /// `DecoBlend::Over` decoration per covered cell, each carrying this
    /// near-solid coverage mask tinted dark — interior at full coverage with a
    /// ~1 px anti-aliased ramp at all four cell borders. Rasterized by
    /// `deco_coverage` like every other sparkle sprite (cell-sized,
    /// parameterized only by the cell geometry), so CPU and GPU sample the one
    /// shared mask.
    Shade,
}

/// How a [`WordDecoration`] composites onto the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecoBlend {
    /// Premultiplied saturating ADD (`dst + src`) — the profanity sparkle, which
    /// only ever brightens, exactly like [`GlowQuad`].
    #[default]
    Add,
    /// Source-OVER alpha blend (`dst*(1-a) + src*a`) — the feline cat-paw, a
    /// solid stamp tinted by `color` at `alpha`.
    Over,
}

/// One "sparkle word" decoration: a small sprite stamped over (or near) a cell
/// of a matched word. The HOST owns the matching + animation and hands the
/// renderer a fully-resolved list each frame; the renderer is a pure consumer
/// (an empty list is byte-identical to the pre-feature path, like the cursor
/// glow). Decorations are render-only — they never touch grid cells,
/// copied text, or recordings.
///
/// Position is in the SAME viewport coordinate space as
/// [`RenderInput::cursor_row`]/[`cursor_col`](RenderInput::cursor_col); the
/// renderer derives the cell origin exactly as it does for the cursor, then
/// offsets by the sub-cell `(dx, dy)` jitter.
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

/// One animated-ink foreground OVERRIDE for a single cell (Sparkle Words v2
/// "animated ink"). `color` is the HOST-resolved FINAL colour — the gradient,
/// specular sweep and legibility guard are pre-folded by the host — so both
/// renderers do ZERO new colour math: they substitute it wherever
/// [`RenderCell::fg`] would tint ink (the glyph blit, combining-mark overlays,
/// underline / strikethrough / overline). An explicit SGR 58 `underline_color`
/// still wins over ink. `GlyphImage::Rgba` (colour emoji) blits ignore `fg` on
/// both backends, so emoji cells are untouched by construction.
///
/// Invariants the host upholds: [`RenderInput::ink`] is sorted by `(row, col)`
/// with unique cells (the renderers walk it in lockstep with their column
/// loops); a wide glyph is governed by its LEAD cell's entry (a continuation
/// column carries no glyph, so an entry there is inert). An EMPTY list is
/// byte-identical to the pre-ink render path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InkCell {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: u16,
    /// Viewport column (0-indexed); the LEAD cell of a wide glyph.
    pub col: u16,
    /// Final `[r, g, b]` to substitute for the cell's resolved `fg`.
    pub color: [u8; 3],
}

/// One EFFECT-owned per-cell glyph-ink foreground OVERRIDE (EMBERFORGE
/// "dark glyph cores": engulfed cells recoloured toward charred ember-black so
/// the letterform reads as a silhouette inside the flame). `fg` is the
/// HOST-resolved FINAL colour — the renderers do ZERO new colour math and
/// substitute it exactly where an [`InkCell`] would substitute (the glyph
/// blit, combining-mark overlays, line decorations — SGR 58 still wins),
/// BEFORE the minimum-contrast / selection fg floors. When a cell carries BOTH
/// an ink entry and a `char_fg` entry, INK WINS (animated ink is a
/// user-visible feature; the effect recolour yields).
///
/// Invariants the host upholds (the [`InkCell`] contract, verbatim):
/// [`RenderInput::char_fg`] is sorted by `(row, col)` with unique cells (the
/// renderers merge-walk it in lockstep with their column loops); a wide glyph
/// is governed by its LEAD cell's entry (a continuation column carries no
/// glyph, so an entry there is inert); `GlyphImage::Rgba` (colour emoji) blits
/// ignore `fg` on both backends, so emoji cells are untouched by construction.
/// An EMPTY list is byte-identical to the pre-char_fg render path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharFg {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: u16,
    /// Viewport column (0-indexed); the LEAD cell of a wide glyph.
    pub col: u16,
    /// Final `0x00RRGGBB` to substitute for the cell's resolved `fg`.
    pub fg: u32,
}

/// One EFFECT-owned CONTRAST-HALO cell (EMBERFORGE text legibility): the fire
/// ENGULFMENT WEIGHT for a glyph cell, driving the alpha of the dark warm
/// dilation ring the renderers stamp under the glyph's ink (over the flame) so
/// the letterform separates from the fire at any brightness. Carries NO colour
/// on purpose — the ink itself is NEVER recoloured (the no-recolor law: the
/// owner vetoed ink recolouring twice, v0.41/v0.42; the halo lives around the
/// strokes, never in them). A COLOR-FREE strength stream: `strength` scales
/// only the halo ring's opacity (`0` a bare rim, `255` the full separator).
///
/// Invariants the host upholds (the [`CharFg`]/[`InkCell`] contract, verbatim):
/// [`RenderInput::fire_halo`] is sorted by `(row, col)` with unique cells (the
/// renderers merge-walk it in lockstep with their column loops); a wide glyph
/// is governed by its LEAD cell's entry; `GlyphImage::Rgba` (colour emoji)
/// cells draw no mono coverage, so they are never haloed by construction. This
/// is a GRID stream (cell-anchored): the tab-strip splice shifts `row` down
/// with the terminal content, exactly like `char_fg`. An EMPTY list is
/// byte-identical to the pre-fire_halo render path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FireHaloCell {
    /// Viewport row (0-indexed from the top of the visible area).
    pub row: u16,
    /// Viewport column (0-indexed); the LEAD cell of a wide glyph.
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

/// One run of columns in a COMPOSED row that carries its own DEC line size.
///
/// A single-pane frame never needs these: [`RenderInput::line_sizes`] governs the
/// whole row and [`RenderInput::line_size_spans`] stays empty — the ordinary,
/// allocation-free path, byte-identical to the pre-span renderer.
///
/// Split-pane composition is the case a row-level line size CANNOT express. Two
/// panes side by side are independent terminals, so the lines they contribute to
/// one composite row may differ in DECDWL/DECDHL state. Collapsing that to a
/// single value means one pane's line size scales the other pane's glyphs, which
/// visibly corrupts the innocent pane.
///
/// Columns are COMPOSITE columns (the destination grid), so `start_col` is also
/// the run's pixel origin in cell units: a glyph at composite column `c` inside
/// this run draws at `start_col * cell_w + (c - start_col) * run_cell_w`, clipped
/// to the run's box. For a run starting at 0 that reduces exactly to the uniform
/// `c * run_cell_w` the renderers used before spans existed.
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
/// A terminal selection is expressed in that terminal's own logical row/column
/// coordinates. When the host projects the focused terminal into a split-pane
/// frame, a multi-row selection's interior rows would otherwise span the whole
/// composed window, including dividers and sibling panes. This half-open
/// rectangle is the renderer-visible authority that confines those predicates to
/// the pane which supplied the selection.
///
/// `RenderInput::selection_clip == None` preserves the historical single-terminal
/// behavior exactly.
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

/// ONE pane's selection, already relocated into FRAME coordinates.
///
/// A composed split frame has as many live selections as it has panes, and each
/// carries its own anchors, its own bounding rectangle and its own live OSC
/// 17/19 colours. The scalar [`RenderInput::selection`] cannot represent that —
/// it is one selection with one clip — so [`RenderInput::selections`] carries
/// the per-pane list beside it, exactly as `default_bg_spans` carries the
/// per-pane refinement of the scalar `default_bg`.
///
/// An EMPTY list is the historical scalar path, byte for byte: every
/// single-terminal frame, wasm, and every direct `cell_frame_into` extraction
/// leaves it empty and reads the scalars.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneSelection {
    /// The pane's selection with its anchors already moved into FRAME rows and
    /// columns (pane origin + that pane's `display_offset`).
    pub selection: TextSelection,
    /// The pane's half-open frame-space box. Always present: a list entry is
    /// always bounded, because a pane that does not bound its selection would
    /// tint its siblings. (The scalar `selection_clip`'s `None` means
    /// "unbounded", which only a single-terminal frame may ever mean.)
    pub clip: SelectionClip,
    /// The pane's LIVE selection background (`0x00RRGGBB`), or [`COLOR_UNSET`]
    /// to delegate to the renderer's theme policy — same encoding as
    /// [`RenderInput::selection_bg`], read per ENTRY so an OSC 17 in one pane
    /// cannot recolour a sibling's band.
    pub bg: u32,
    /// The pane's LIVE selected-text foreground, or [`COLOR_UNSET`] /
    /// [`COLOR_DYNAMIC`] — same encoding as [`RenderInput::selection_fg`].
    pub fg: u32,
    /// Whether this pane is UNFOCUSED, so its band takes the renderer's
    /// inactive selection colour (the same derivation an unfocused WINDOW
    /// gets). Focus is a per-pane property in a split, and painting every
    /// pane's selection in the active colour would erase the only cue for which
    /// highlight the keyboard is about to extend.
    pub inactive: bool,
}

/// The per-row, per-entry selection key the renderer's row-damage diff
/// (`aterm_render::compute_dirty_rows`) compares.
///
/// [`RenderInput::selection_row_span`] is a HULL over every entry and therefore
/// a SCAN WINDOW only: with two panes selected on one row, dragging one pane's
/// edge inward can leave the hull's `(lo, hi)` unmoved, so an equality diff
/// built on it would call two visibly different frames equal and freeze the
/// highlight mid-drag. This key is per ENTRY and carries the colours, so it is
/// lossless in both dimensions.
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

/// The inclusive selected column span ONE selection puts on ONE frame row.
///
/// Extracted verbatim from [`RenderInput::selection_row_span`] so the scalar
/// path and every [`PaneSelection`] entry derive their span from the SAME
/// arithmetic — a second copy is how a pane's highlight and its damage band
/// start disagreeing. `display_offset` is the viewport projection to apply
/// before consulting the selection (zero for entries, whose anchors the host
/// already relocated into frame rows).
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

/// Everything a renderer reads from a `&Terminal` for one frame, snapshotted into
/// plain owned data — the engine emits it via [`crate::terminal::Terminal::cell_frame_into`].
///
/// The windowed frontend holds the `Terminal` mutex only long enough to extract
/// this struct, then renders WITHOUT the lock — so the PTY reader thread is no
/// longer starved for the multi-millisecond duration of a frame (CPU
/// rasterization or GPU encode + readback).
///
/// `PartialEq`/`Eq` are hand-written and compare only the rendered CONTENT (every
/// field EXCEPT pure frame metadata such as
/// [`snapshot_seq`](RenderInput::snapshot_seq) and
/// [`absolute_row_revision`](RenderInput::absolute_row_revision)): the CPU renderer's
/// damage-tracking fast path compares a fresh `RenderInput` against the one it
/// cached last frame — overall and per row — to decide which rows changed (and
/// whether anything changed at all). `snapshot_seq` is pure metadata that
/// advances on every damaged frame, so including it in the comparison would make
/// equality ALWAYS differ and defeat the row-level reuse / dirty-gate; it is
/// therefore excluded. Content equality stays exact (byte-for-byte intent), which
/// is what the no-visual-regression contract requires.
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
    /// NOT baked in here — it lives on the renderer and is applied in
    /// `render_input`.
    pub cursor_style: CursorStyle,
    /// Host/effect-owned cursor shape for this frame. This is kept
    /// separate from [`cursor_style`](Self::cursor_style), so a laser/rainbow
    /// effect never overwrites the terminal's live DECSCUSR state. Renderers
    /// resolve cursor shape as: backend override (for example the unfocused
    /// HollowBlock) > this effect override > terminal DECSCUSR. `None` is the
    /// common, byte-identical terminal-only path.
    pub cursor_effect_style_override: Option<CursorStyle>,
    /// The cursor MOTION TRAIL for this frame (the "streaming trailer" effect):
    /// recently-swept cells fading out behind the cursor. Owned by the host's
    /// animation clock (the windowed frontend fills it each frame and decays it),
    /// not by the engine — `cell_frame_into` leaves it untouched. EMPTY in the
    /// common case (no recent move / the effect disabled), which is byte-identical
    /// to the pre-trail render path. See [`TrailCell`].
    pub cursor_trail: Vec<TrailCell>,
    /// The fully-resolved trail colour (`0x00RRGGBB`) to blend over each trail
    /// cell's background. The host resolves it (config `cursor_trail_color`, else
    /// the effective live cursor: OSC 12/configured baseline, or live OSC 10
    /// foreground while OSC 21 `cursor=` is dynamic) so the renderer needs no
    /// terminal/theme knowledge for the trail. Only consulted when `cursor_trail`
    /// is non-empty.
    pub cursor_trail_color: u32,
    /// Additive LIGHT quads for the "LUMEN WAKE" cursor aurora (comet of emitted
    /// light, bloom crown, landing ring, sparks). Premultiplied + saturating-added
    /// over the frame, UNDER the crisp cursor. WINDOW-ABSOLUTE pixels (the
    /// [`GlowQuad`] invariants; above-grid quads tag row 0). Host-owned
    /// animation; the engine leaves it untouched. EMPTY in the common case →
    /// byte-identical to the pre-aurora path. See [`GlowQuad`].
    pub cursor_glow_add: Vec<GlowQuad>,
    /// GLOW-HALO cursor-effect RADIAL light (EMBERFORGE round embers / crown):
    /// soft elliptical-falloff halos drawn with
    /// [`cursor_glow_add`](RenderInput::cursor_glow_add) — the same premultiplied
    /// saturating-add contract (CPU `add_sat` == GPU One/One on the Unorm view),
    /// but each quad's light falls off RADIALLY from its centre like
    /// [`rain_add`](RenderInput::rain_add) (the [`RainHalo`] integer falloff, so
    /// CPU and GPU stay byte-exact). Drawn immediately AFTER the LUMEN aurora
    /// and BEFORE the nova / rain additive streams. Quads obey the [`RainHalo`]
    /// cursor-stream invariants: WINDOW-ABSOLUTE pixels, each within EXACTLY
    /// ONE grid-anchored row band (a halo spanning rows is emitted as per-row
    /// quads sharing one centre; above-grid quads tag row 0).
    /// Host-owned animation; the engine leaves it untouched. EMPTY in the
    /// common case → byte-identical to the pre-halo render path.
    pub glow_halo: Vec<RainHalo>,
    /// UNDER-GLYPH additive light (EMBERFORGE flame BODY: the fire volume the
    /// glyphs sit inside as dark silhouettes): [`GlowQuad`]s with the SAME
    /// premultiplied saturating-add contract as
    /// [`cursor_glow_add`](RenderInput::cursor_glow_add) (CPU `add_sat` == GPU
    /// One/One on the Unorm view, byte-exact) but drawn at a DIFFERENT z-slot —
    /// after the cell background fill and the under-text sprites, BEFORE the
    /// glyph ink — so the light lands UNDER the letterforms while
    /// tips/embers/beam (`cursor_glow_add`/`glow_halo`/`nova_add`) stay above.
    /// Pairs with [`char_fg`](RenderInput::char_fg), which chars the engulfed
    /// glyphs toward ember-black. Quads obey the [`GlowQuad`] invariants:
    /// WINDOW-ABSOLUTE pixels, each within EXACTLY ONE grid-anchored row band
    /// (above-grid quads tag row 0). Host-owned animation; the engine leaves
    /// it untouched. EMPTY in the common case → byte-identical to the
    /// pre-glow_under render path.
    pub glow_under: Vec<GlowQuad>,
    /// EMBERFORGE per-pixel procedural FIRE FIELD patches (the flame BODY at
    /// full art scale): each [`FirePatch`] quad is evaluated at EVERY device
    /// pixel by the shared pure-integer field function — CPU rasterizer and
    /// GPU fragment shader byte-exact (the fire-parity contract). Drawn at the
    /// [`glow_under`](RenderInput::glow_under) z-slot (after the cell bg +
    /// under-text sprites, BEFORE the glyph ink) so engulfed glyphs read as
    /// charred silhouettes inside the flame volume, [`FireMode::Add`] patches
    /// then [`FireMode::Over`] patches within the stream. Quads obey the
    /// [`FirePatch`] invariants: WINDOW-ABSOLUTE pixels, each within EXACTLY
    /// ONE grid-anchored row band (above-grid patches tag row 0). Host-owned
    /// animation; the engine leaves it untouched. EMPTY in the common case →
    /// byte-identical to the pre-fire render path.
    pub fire_patch: Vec<FirePatch>,
    /// Host-resolved cursor FILL override (`0x00RRGGBB`), or `None` for the ordinary
    /// theme/OSC-12 cursor colour. The override applies to the live cursor shape:
    /// block, hollow, bar, underline, and Bolt all use it. Block glyph cut-outs and
    /// every other shape still run through `floor_cursor_fill`, so cursor contrast
    /// remains intact. Host-owned animation (rainbow, forge, phaser, or lightning);
    /// `None` is byte-identical to the ordinary cursor path.
    pub cursor_fill_override: Option<u32>,
    /// The "sparkle word" decorations for this frame: small sprites stamped over
    /// cells of matched profanity / feline words. Host-owned (the windowed
    /// frontend matches text + drives the animation; `cell_frame_into` leaves it
    /// untouched). EMPTY in the common case (feature off / no match), which is
    /// byte-identical to the pre-feature render path. See [`WordDecoration`].
    pub word_decorations: Vec<WordDecoration>,
    /// Animated-ink per-cell fg OVERRIDES (Sparkle Words v2): host-resolved
    /// final colours, sorted by `(row, col)` with unique cells. Renderers
    /// substitute each entry for its cell's `fg` at every fg consult site
    /// (glyph, combining marks, line decorations — SGR 58 still wins), BEFORE
    /// the minimum-contrast / selection fg floors. Host-owned animation; the
    /// engine leaves it untouched. EMPTY in the common case (feature off / no
    /// match / settled+unchanged) → byte-identical to the pre-ink path. See
    /// [`InkCell`].
    pub ink: Vec<InkCell>,
    /// EFFECT-owned per-cell FINAL glyph-ink fg overrides (EMBERFORGE dark
    /// glyph cores: engulfed letterforms charred toward ember-black inside the
    /// [`glow_under`](RenderInput::glow_under) flame body). Host-resolved
    /// `0x00RRGGBB`, sorted by `(row, col)` with unique cells. Renderers
    /// substitute each entry at the SAME fg-resolution seam as
    /// [`ink`](RenderInput::ink) (glyph, combining marks, line decorations —
    /// SGR 58 still wins), BEFORE the minimum-contrast / selection fg floors;
    /// when a cell carries both, INK WINS (the user-visible feature over the
    /// effect recolour). Host-owned animation; the engine leaves it untouched.
    /// EMPTY in the common case → byte-identical to the pre-char_fg path. See
    /// [`CharFg`].
    pub char_fg: Vec<CharFg>,
    /// EFFECT-owned per-cell CONTRAST-HALO strengths (EMBERFORGE fire
    /// legibility): the fire engulfment weight for each glyph cell the flame
    /// covers, driving the alpha of the dark warm dilation ring stamped under
    /// the glyph ink (over the flame) so the letterform reads against the
    /// blaze. A COLOR-FREE stream — the ink is never recoloured (the
    /// no-recolor law) — sorted by `(row, col)` with unique cells (the
    /// [`CharFg`] walk contract). A GRID stream: the tab-strip splice shifts
    /// rows down with the terminal content, exactly like
    /// [`char_fg`](RenderInput::char_fg). Host-owned animation; the engine
    /// leaves it untouched. EMPTY in the common case → byte-identical to the
    /// pre-fire_halo path. See [`FireHaloCell`].
    pub fire_halo: Vec<FireHaloCell>,
    /// Peeking-cat sprites (Sparkle Words v2), drawn UNDER text — CPU pass 1c
    /// inside `render_row` (between the cell-bg fill and inline images) / GPU
    /// `emit_base_pre` before glyph ink — so the cat sits under the
    /// row-above's glyphs, under the word's own glyphs, and under inline images
    /// on BOTH backends. Each quad lies in exactly one row band (the documented
    /// [`SpriteQuad`] invariant; a cat spanning two rows is emitted as a head
    /// quad in row `r-1` plus a chin-slice quad in row `r`). Sampled from
    /// [`cat_atlas`](RenderInput::cat_atlas) with NEAREST 1:1 (host bakes at
    /// exact destination size.
    /// Host-owned animation; the engine leaves it untouched. EMPTY in the
    /// common case (feature off / no match) → byte-identical to the pre-cat
    /// render path.
    pub cat_quads: Vec<SpriteQuad>,
    /// RGBA8 atlas for [`cat_quads`](RenderInput::cat_quads) (host-baked by the
    /// `CatBaker`, versioned by [`SceneAtlas::version`]:
    /// shared `Arc` so cloning is cheap; the GPU re-uploads only when
    /// [`SceneAtlas::version`] changes (a rebake must repaint). `None` when no
    /// cat is live.
    pub cat_atlas: Option<std::sync::Arc<SceneAtlas>>,
    /// FREE-FLOATING decorative sprites: arbitrary pixel rects at arbitrary
    /// (even off-grid) positions with NO row tag — the [`FreeSprite`] layer.
    /// Dirty tracking row-unions each sprite's true `[y, y+h)` pixel extent
    /// (prev∪cur), so a sprite may span any number of cell-row bands without
    /// host-side per-row slicing. Sampled from
    /// [`free_atlas`](RenderInput::free_atlas) (v1: NEAREST 1:1, the cat
    /// regime). Host-owned animation; the engine leaves it untouched. EMPTY in
    /// the common case (feature off) → byte-identical to the pre-free-layer
    /// render path.
    pub free_sprites: Vec<FreeSprite>,
    /// RGBA8 atlas for [`free_sprites`](RenderInput::free_sprites), versioned
    /// like [`cat_atlas`](RenderInput::cat_atlas): shared `Arc` so cloning is
    /// cheap; the GPU re-uploads only when [`SceneAtlas::version`] changes.
    /// `None` when no free sprite is live.
    pub free_atlas: Option<std::sync::Arc<SceneAtlas>>,
    /// SUPERNOVA additive light quads (Sparkle Words v2): the profanity nova's
    /// crown / shockwave ring / rays as PREMULTIPLIED `0x00RRGGBB` light,
    /// saturating-added over the frame exactly like
    /// [`cursor_glow_add`](RenderInput::cursor_glow_add) (CPU `add_sat` == GPU
    /// One/One on the Unorm view — byte-exact over background). Quads obey the
    /// [`GlowQuad`] invariants: GRID-INTERIOR pixels, each within EXACTLY ONE
    /// grid-anchored row band (the host splits ring chords / ray slabs at row
    /// boundaries), so the row-scoped dirty gate + GPU scissor cover them
    /// exactly. Drawn AFTER
    /// the LUMEN aurora, UNDER the word decorations and the cursor. Host-owned
    /// animation (self-terminating; a settled nova emits nothing); the engine
    /// leaves it untouched. EMPTY in the common case → byte-identical to the
    /// pre-nova render path.
    pub nova_add: Vec<GlowQuad>,
    /// PHOSPHOR rain glyph sprites (Matrix digital rain), drawn UNDER text —
    /// CPU pass 1c before [`cat_quads`](RenderInput::cat_quads) / GPU
    /// `RainUnder` in the same slot
    /// — so cats walk on rain and every glyph stays legible over it. Each quad
    /// lies in EXACTLY ONE row band (the documented [`SpriteQuad`] invariant;
    /// rows-only dirty marking ghosts a band violator). Sampled from
    /// [`rain_atlas`](RenderInput::rain_atlas) with NEAREST 1:1 (the cat
    /// regime: host bakes at exact destination size). Host-owned animation;
    /// the engine leaves it untouched. EMPTY in the common case (feature off)
    /// → byte-identical to the pre-rain render path.
    pub rain_quads: Vec<SpriteQuad>,
    /// RGBA8 white-coverage rain-glyph atlas for
    /// [`rain_quads`](RenderInput::rain_quads) (host-baked by the `RainBaker`,
    /// versioned like [`cat_atlas`](RenderInput::cat_atlas)): shared `Arc` so
    /// cloning is cheap; the GPU re-uploads only when [`SceneAtlas::version`]
    /// changes, so every rebake must bump it (a same-version rebake is
    /// invisible to damage). `None` when no rain is live. The engine leaves it
    /// untouched.
    pub rain_atlas: Option<std::sync::Arc<SceneAtlas>>,
    /// PHOSPHOR bright-head additive halos: PREMULTIPLIED `0x00RRGGBB` light,
    /// saturating-added exactly like [`nova_add`](RenderInput::nova_add) (CPU
    /// `add_sat` == GPU One/One on the Unorm view). Quads are GRID-INTERIOR
    /// pixels (this stream stays grid-anchored: the renderer offsets it by
    /// `(pad, grid_top)`), each within EXACTLY ONE row band. Host-owned
    /// animation; the engine leaves it untouched. EMPTY in the common case →
    /// byte-identical to the pre-rain render path.
    pub rain_add: Vec<RainHalo>,
    /// Scrollback offset: viewport row `r` shows live row `r - display_offset`.
    pub display_offset: i32,
    /// Absolute row of the top visible line at extraction (`Grid::base_y()` =
    /// `oldest_absolute_row + scrollback_lines`), captured under the SAME lock as the
    /// cells so it is consistent with this exact frame. Display-offset-independent
    /// metadata a host uses to map an ABSOLUTE row into this frame's grid
    /// (`viewport_row = absolute - base_y + display_offset`) — e.g. the ⌘F find bar
    /// re-anchors its match highlights to the presented content, so a highlight can't
    /// drift off its line when a program streams output between frames. Pure metadata:
    /// like `snapshot_seq` it does NOT affect the rendered pixels and is EXCLUDED from
    /// `PartialEq` (a base_y-only change from a passive scroll must not force a repaint).
    pub base_y: i64,
    /// Revision of top-anchored protected-footer row insertions at extraction,
    /// captured under the SAME lock as [`base_y`](Self::base_y) and the cells.
    /// Such an insertion cannot be re-anchored with one uniform `base_y` delta.
    /// Hosts retaining absolute-row overlays use this stamp to reject stale
    /// geometry for this exact frame.
    /// Pure metadata: it does not affect rendered pixels and is excluded from
    /// `PartialEq`, like [`snapshot_seq`](Self::snapshot_seq).
    pub absolute_row_revision: u64,
    /// M1b SUB-ROW SCROLL TRANSLATE (display-only): the SIGNED fractional-pixel
    /// residual the smooth-scroll kinematics bank below one whole row
    /// (`scroll_frac_px ∈ (-cell_h, cell_h)`). A POSITIVE value is the `frac` half
    /// of [`scroll_motion::decompose`]'s Euclidean split for a whole-row glide; a
    /// NEGATIVE value is the elastic-overscroll spring displacement at a history
    /// end. The renderer shifts the TERMINAL-CONTENT pixel band by this many device
    /// px at PRESENT time — UP for a positive frac (the incoming row appears at the
    /// bottom), DOWN for a negative frac (the rubber-band bounce sags the content,
    /// exposing a strip at the top) — glyph rasters and shaping are untouched (text
    /// stays raster-exact while it moves). `0` (the default) is the whole-row path:
    /// the translate is a literal identity, byte-for-byte the pre-M1b frame. It is
    /// consumed at present time (not in the content damage diff), so it is
    /// deliberately EXCLUDED from `PartialEq`/`Eq` (like `snapshot_seq`) — a
    /// frac-only change re-translates from the pristine damage cache without
    /// dirtying a cell.
    pub scroll_frac_px: i32,
    /// M1b GRID/CHROME PARTITION: the terminal-content row band `[grid_top_row,
    /// grid_bot_row)` within this frame's rows. Chrome (the prepended tab-strip,
    /// transient edge bars, split-pane dividers) lives OUTSIDE this band and is PINNED —
    /// the [`scroll_frac_px`](Self::scroll_frac_px) translate touches only the grid
    /// band's pixels, so every chrome pixel is invariant under the shift. The
    /// default `grid_bot_row == 0` means "no grid band" ⇒ no translate (the
    /// byte-identical pre-M1b path); the windowed compose path fills these from the
    /// app-chrome splice counts. Excluded from `PartialEq`/`Eq` for the same reason
    /// as `scroll_frac_px`.
    pub grid_top_row: usize,
    /// One past the last terminal-content row — see [`grid_top_row`](Self::grid_top_row).
    pub grid_bot_row: usize,
    /// FOCUSED-PANE effect-clip box `(x0, y0, x1, y1)` in WINDOW-ABSOLUTE
    /// device pixels — the same space as the `cursor_glow_add` stream (split
    /// composition sets it to the focused pane's box; `None` = single-pane /
    /// no clip). PRESENT-TIME GPU post-fx (the bloom halo composite and the
    /// heat-shimmer refraction) intersect their pass regions with it so
    /// blurred light and displaced haze never cross a split divider into a
    /// neighbour pane (split-pane audit). The content pipeline never reads it:
    /// every content-affecting effect stream is already host-clipped before
    /// the renderer sees it. Excluded from `PartialEq`/`Eq` like
    /// [`scroll_frac_px`](Self::scroll_frac_px) — present-time-consumed,
    /// never part of the content damage diff (any real layout change dirties
    /// cells anyway).
    pub fx_clip: Option<(u16, u16, u16, u16)>,
    /// A clone of the active text selection, for per-cell highlighting.
    pub selection: TextSelection,
    /// Optional half-open FRAME-space rectangle that confines
    /// [`selection`](Self::selection). Split composition uses it to prevent the
    /// focused pane's multi-row selection from tinting a divider or sibling pane.
    /// `None` is the historical single-terminal path with no additional clip.
    pub selection_clip: Option<SelectionClip>,
    /// The terminal's LIVE selection-background colour (`0x00RRGGBB`) for this
    /// frame, including OSC 17/21 changes and OSC 117/RIS resets. `COLOR_UNSET`
    /// delegates to the renderer's configured selection theme. Kept separate
    /// from [`selection`](Self::selection) because a colour-only OSC mutation
    /// must invalidate cached pixels even when the selected cell range is
    /// unchanged.
    pub selection_bg: u32,
    /// The terminal's LIVE selected-text foreground (`0x00RRGGBB`) for this
    /// frame, including OSC 19/21 changes and OSC 119/RIS resets.
    /// [`COLOR_UNSET`] delegates to the renderer's configured policy;
    /// [`COLOR_DYNAMIC`] explicitly selects the automatic WCAG contrast floor.
    pub selection_fg: u32,
    /// EVERY visible pane's selection, already relocated into FRAME
    /// coordinates and each carrying its OWN clip and live colours.
    ///
    /// EMPTY is the historical scalar path, byte for byte: the four scalar
    /// fields above are then the whole authority, which is what every
    /// single-terminal frame (and every direct `cell_frame_into` extraction)
    /// produces. A composed split frame fills this instead — one entry per
    /// pane that HAS a selection — because a split has as many live selections
    /// as it has panes and no scalar can carry them. Same idiom, same reason,
    /// as [`default_bg_spans`](Self::default_bg_spans) refining `default_bg`.
    ///
    /// The scalars are still stamped from the FOCUSED pane when this list is
    /// non-empty, so the single-authority consumers that predate splits keep an
    /// unchanged anchor; every RENDERER predicate reads the list when it is
    /// non-empty and the scalars only when it is empty.
    pub selections: Vec<PaneSelection>,
    /// Per-row, sparse emoji grapheme-cluster strings (`term.cluster_row(r)`):
    /// `(col, cluster)` for cells whose combining marks form a ZWJ / skin-tone /
    /// keycap sequence. The renderer shapes each to a single colour glyph; cells
    /// absent here take the ordinary single-codepoint dispatch.
    pub clusters: Vec<Vec<(usize, Box<str>)>>,
    /// Per-row, sparse combining MARKS (`term.combining_row(r)`): `(col, marks)`
    /// for cells with diacritics (é, ñ, …). The renderer overlays each mark's
    /// glyph on the base so accents render.
    pub combining: Vec<Vec<(usize, Box<[char]>)>>,
    /// Per-row DEC line size (DECDWL/DECDHL via `ESC # 3..6`): the renderer draws
    /// double-width / double-height rows scaled. `SingleWidth` (the default) is
    /// the ordinary path.
    ///
    /// AUTHORITATIVE only while the matching [`line_size_spans`](Self::line_size_spans)
    /// row is empty — which is every single-terminal frame, and every split whose
    /// panes all sit on ordinary lines. For a composed row that DOES carry runs
    /// this degrades to a SUMMARY: it reports a non-single value when any pane on
    /// the row is a DEC line, so row-level consumers (the sparkle-word cat
    /// suppressor, the double-height damage gate) keep working unchanged. It is
    /// then NOT a placement input — ask
    /// [`line_size_run_at`](Self::line_size_run_at) instead, which is the only
    /// seam that can answer per column.
    pub line_sizes: Vec<LineSize>,
    /// Per-row column runs carrying a NON-UNIFORM DEC line size, for composed
    /// split-pane frames. EMPTY (the ordinary case) means the row is uniform and
    /// [`line_sizes`](Self::line_sizes) governs it end to end.
    ///
    /// Populated only by the pane compositor, and only for rows where the panes
    /// actually disagree — see [`LineSizeSpan`] and [`Self::line_size_run_at`].
    /// When non-empty for a row, the runs partition `0..cols` in ascending order.
    pub line_size_spans: Vec<Vec<LineSizeSpan>>,
    /// Pane-local live default-background intervals for composed split rows.
    /// Empty/missing rows use [`default_bg`](Self::default_bg) for every column,
    /// preserving the historical single-terminal path. A non-empty row carries
    /// one [`DefaultBgSpan`] per pane so background opacity, deepest Kitty image
    /// layering, cursor fallback, and trail fallback classify cells against the
    /// default of the pane that supplied them rather than one window-wide scalar.
    ///
    /// scope-waiver: the phrase names the value this field REPLACED. A default
    /// background is a colour, not an enforcement budget — no instance count
    /// can make it wrong, and going per-pane here is the fix, not the hazard.
    pub default_bg_spans: Vec<Vec<DefaultBgSpan>>,
    /// Per-row, sparse inline-image placements (`term.images_row(r)`):
    /// `(col, ImageRef)` for every cell covered by an iTerm2 OSC 1337 `File=`
    /// image. The renderer decodes each image once (keyed by the `Arc` inside the
    /// ref) and blits the cell's tile; a covered cell SKIPS its glyph (the bg
    /// still fills). Cells absent here take the ordinary glyph dispatch, so a
    /// frame with no images is byte-identical to the pre-image path.
    pub images: Vec<Vec<(usize, aterm_grid::ImageRef)>>,
    /// WALLPAPER base layer: a host-prepared RGBA8 image, PRE-SCALED to exactly
    /// this frame's pixel dimensions (cover-fit) and PRE-BLENDED toward the theme
    /// background by the configured dim — the renderer only blits, never scales,
    /// so the CPU copy and the GPU 1:1 NEAREST-sampled texture stay byte-exact.
    /// When set, the frame's base (the clear, the padding bands, and every
    /// UNSELECTED cell whose bg resolves to its pane's live default) shows these
    /// texels instead of the flat default background; selected cells and SGR-
    /// colored backgrounds still paint opaquely over it (the kitty
    /// below-bg-image cover rule). Wallpaper pixels are OPAQUE: they replace the
    /// background-opacity glass rather than compositing with it. Compared by
    /// SNAPSHOT IDENTITY (`Arc::as_ptr`) like every atlas — the host publishes a
    /// fresh `Arc` per (source, size, dim) revision, and an identity change
    /// forces a full repaint (the padding strips are not a row the per-row diff
    /// can mark; same law as a live default-bg change). It also disables the E7
    /// scroll-blit rescue: a row blit would drag the fixed backdrop along with
    /// the content. `None` — every pre-wallpaper frame — is byte-identical to
    /// the historical render paths.
    pub wallpaper: Option<std::sync::Arc<SceneAtlas>>,
    /// The LIVE default background colour (`0x00RRGGBB`) for this frame: the engine's
    /// dynamic default-bg already folded with DECSCNM reverse-video, resolved by
    /// [`Terminal::cell_frame_into`](crate::terminal::Terminal::cell_frame_into).
    /// `COLOR_UNSET` is retained only for a pristine, unconfigured `Terminal::new`
    /// so standalone renderers preserve their historical theme fallback; host
    /// configuration, an OSC mutation/reset, or DECSCNM makes this authoritative.
    /// The renderer paints the window PADDING band and the base clear from this
    /// (not the static config theme), so OSC 11 (set default bg) / OSC 111 (reset) and
    /// DECSCNM (DECSET ?5) reach the frame border too, matching the grid interior
    /// (which resolves per-cell from the same live value). Equals the configured bg
    /// until a program changes it, so the default path is byte-identical.
    pub default_bg: u32,
    /// The LIVE default FOREGROUND (`0x00RRGGBB`) for this frame — the twin of
    /// [`Self::default_bg`], resolved through the same OSC 10 / OSC 110 /
    /// DECSCNM policy, so the pair is coherent (DECSCNM swaps BOTH or neither).
    ///
    /// Added for the cursor-effect layer, which needs a real foreground to
    /// anchor the fresh-typed glyph tint on and to bound it against the real
    /// ground; the renderer itself resolves per-cell foregrounds and does not
    /// read this. `COLOR_UNSET` means the producer did not resolve one — a
    /// pristine, unconfigured `Terminal::new` — and every consumer must treat
    /// it as "unknown" rather than masking the sentinel's high byte into a
    /// colour, which would read as pure black.
    pub default_fg: u32,
    /// The LIVE cursor colour (`0x00RRGGBB`) for this frame: an explicit OSC 12
    /// value/configured OSC 112 baseline, or the live OSC 10 foreground while OSC 21
    /// `cursor=` selects dynamic behavior. The terminal snapshot resolves that policy
    /// after host configuration or an OSC color boundary; `COLOR_UNSET` preserves
    /// the renderer-theme cursor for a pristine, unconfigured `Terminal::new`.
    /// The renderer fills the
    /// block/bar/underline cursor from this. The damage gate that un-gates a
    /// cursor-colour-only change (which dirties no cell) lives in
    /// `aterm_render::compute_dirty_rows` (folded into `cursor_changed`); it is also
    /// compared in this type's `PartialEq` for whole-snapshot equality / test parity.
    pub cursor_color: u32,
    /// The engine's monotone damage epoch at snapshot time (A-3 read_image seq):
    /// the value of [`Terminal::damage_epoch`](crate::terminal::Terminal::damage_epoch)
    /// captured under the SAME lock that filled the rest of this snapshot. It is a
    /// version stamp, not rendered content — a consumer that records it can detect
    /// staleness (compare against a later `damage_epoch()`), and because the whole
    /// snapshot is filled under one lock, the value is internally consistent (no
    /// torn read). Deliberately EXCLUDED from `PartialEq`/`Eq` (see the type doc):
    /// it advances every damaged frame, so counting it would defeat the renderer's
    /// content-based damage cache.
    pub snapshot_seq: u64,
    /// Terminal parser batch sequence coherent with this engine-filled
    /// snapshot. Unlike the damage epoch it advances even when another batch
    /// joins an already-pending damage session, so one-shot input evidence can
    /// reject intervening output. Metadata only; excluded from equality.
    pub process_sequence: u32,
    /// PRESENT-TIME latency hint: this frame is an immediate keystroke-echo that
    /// bypasses present coalescing (`input_hot`). The GPU present path uses it to
    /// DEFER the throwaway-copy present-time bloom halo — a whole-framebuffer copy +
    /// a second `queue.submit` that would otherwise run on EVERY keystroke — to
    /// the next settle frame. A haloed comet mid-keystroke is imperceptible, and
    /// the comet is still animating on the settle frame (its `cursor_glow_add`
    /// differs frame-to-frame), so the halo lands ~1 frame (~16 ms) later while the
    /// keystroke itself presents at minimum latency. Like `scroll_frac_px` this is
    /// consumed at PRESENT time only, so it is EXCLUDED from `PartialEq`/`Eq`: it
    /// must never dirty a cell or force a repaint. The CPU backend ignores it.
    pub input_hot: bool,
    /// Identity of the engine instance that last engine-filled this snapshot
    /// (DMG-1 damage carrier), or 0 for a never-engine-filled scratch. Stamped
    /// by `cell_frame_into`/`cell_frame_damage_scoped_into`; consumed by the
    /// damage-scoped refill's continuity check so a scratch reused across
    /// DIFFERENT terminals (the split compositor's shared `pane_scratch`) can
    /// never serve one pane's retained rows as another's. Pure metadata:
    /// EXCLUDED from `PartialEq`/`Eq` like `snapshot_seq`.
    pub terminal_id: u64,
    /// The stamping terminal's extraction generation at fill time (DMG-1
    /// carrier; see `Terminal::take_damage`). A damage-scoped refill is sound
    /// only while this equals the terminal's CURRENT generation — i.e. no
    /// consumer has taken (and thereby reset) the damage tracker since this
    /// scratch was filled, so the tracker's bits are a superset of the rows
    /// that changed under this scratch. 0 = never engine-filled. Metadata,
    /// EXCLUDED from `PartialEq`/`Eq`.
    pub extract_gen: u64,
    /// `snapshot_seq` as stamped by the LAST ENGINE fill (DMG-1 carrier).
    /// Post-extraction host mutators (stream fade, prediction ghosts, the
    /// tab-strip splice) follow the existing discipline of bumping
    /// `snapshot_seq` after touching content channels; `snapshot_seq !=
    /// engine_fill_seq` therefore means "a host wrote cells since the engine
    /// filled this scratch", and the damage-scoped refill falls back to a full
    /// refill instead of preserving rows the engine no longer vouches for.
    /// Metadata, EXCLUDED from `PartialEq`/`Eq`.
    pub engine_fill_seq: u64,
    /// Whether the stamping terminal's ALTERNATE screen was active at fill time
    /// (DMG-1 carrier). Primary and alt grids keep independent damage/content
    /// state, so a swap between fills invalidates row continuity even when
    /// every other token happens to match; the scoped refill compares this bit
    /// against the live terminal and full-refills on mismatch. Metadata,
    /// EXCLUDED from `PartialEq`/`Eq`.
    pub engine_alt: bool,
    /// Which column order the LAST ENGINE fill left this snapshot's rows in
    /// (DMG-1 carrier). Always [`RowOrder::Logical`] in builds without the
    /// `bidi` feature, where the reorder pass does not exist.
    ///
    /// This is the carrier's BiDi continuity token, and it exists because
    /// `apply_bidi_reorder` is an in-place, NON-idempotent row permutation:
    /// retained rows may only be kept while they are still in LOGICAL order,
    /// which is exactly what [`RowOrder::Logical`] records.
    /// [`RowOrder::BidiVisual`] therefore forces the next refill to the full
    /// arm — the permutation is re-derived from scratch — while the
    /// overwhelmingly common pure-LTR frame (no row permuted, so logical ==
    /// visual everywhere) keeps the scoped arm.
    ///
    /// The predecessor of this token was a blanket `bidi_mode != Disabled`
    /// veto, which read as conservative but was a LIVENESS bug: `BiDiMode`'s
    /// `#[default]` is `Implicit`, and `aterm-gui` compiles the `bidi` feature
    /// in, so the veto fired on every frame of the shipping app and the whole
    /// damage-scoped path was dead code there. Metadata, EXCLUDED from
    /// `PartialEq`/`Eq`.
    pub engine_row_order: RowOrder,
    /// D-2 PER-ROW CONTENT REVISION, one entry per row of this snapshot: the
    /// engine's [`Grid::row_revisions`](crate::grid::Grid::row_revisions) value
    /// for the row, captured under the SAME lock as the cells.
    ///
    /// This is the damage fact the engine already knows, carried across the
    /// snapshot boundary so a consumer diffing two snapshots can answer "did row
    /// `r` change?" with a `u64` compare instead of re-deriving it from every
    /// cell of every row. `aterm_render::compute_dirty_rows` is that consumer.
    ///
    /// THE ONE GUARANTEE: for two snapshots of the SAME terminal whose row
    /// identity is pinned (see [`row_rev_lane`](Self::row_rev_lane)), a row whose
    /// content changed between them has DIFFERENT entries here. The converse is
    /// NOT guaranteed — an unchanged row may be re-stamped — so a consumer may
    /// over-repaint but can never skip a changed row.
    ///
    /// `0` is the NO-STAMP sentinel and means UNKNOWN, not "unchanged": a
    /// consumer must answer that row by comparing content. Host-built rows (a
    /// spliced tab strip, a composed pane band) carry it, so a host that
    /// prepends rows without extending this lane fails closed by construction.
    ///
    /// Pure metadata: EXCLUDED from `PartialEq`/`Eq` like `snapshot_seq` — it
    /// advances on every damaged frame, so counting it would defeat the very
    /// dirty-gate it feeds.
    pub row_rev: Vec<u64>,
    /// Identity of the row-revision LANE [`row_rev`](Self::row_rev) belongs to,
    /// or `0` when this snapshot's stamps must not be trusted at all.
    ///
    /// Set to the filling terminal's extraction identity by an ENGINE fill, and
    /// left at (or reset to) `0` by every other producer — `empty()`, and any
    /// host that composes or re-shapes a snapshot's rows. Two snapshots may only
    /// have their stamps compared when both lanes are non-zero AND equal:
    /// distinct terminals mint revisions from independent clocks, so a numeric
    /// collision between two panes' stamps would otherwise read as "unchanged".
    ///
    /// A non-zero lane is NOT on its own sufficient. The consumer must also pin
    /// row IDENTITY (`base_y`, `absolute_row_revision`, `engine_alt`, a zero
    /// `display_offset`, `engine_row_order`) and freedom from post-fill host cell
    /// writes (`snapshot_seq == engine_fill_seq`). Metadata, EXCLUDED from
    /// `PartialEq`/`Eq`.
    pub row_rev_lane: u64,
    /// How many HOST-owned rows have been PREPENDED to this snapshot since the
    /// engine filled it — the in-grid tab strip's chrome rows (D-2 SPLICE).
    ///
    /// Engine row `r` therefore lives at index `r + row_shift`, and indices
    /// `0..row_shift` are host-built chrome that no engine fill ever vouched
    /// for. `0` for every un-spliced snapshot, which is every snapshot on a
    /// platform whose `DEFAULT_TAB_STRIP_ROWS` is 0.
    ///
    /// PHYSICAL, not a claim of provenance: it counts rows actually prepended
    /// whether or not the prepend was blessed (see
    /// [`shifted_fill_seq`](Self::shifted_fill_seq)), so an un-splice always
    /// knows exactly how many rows to remove. Two consumers require it:
    /// `aterm_render::compute_dirty_rows` compares it across frames (a strip
    /// whose ROW COUNT changed slides the engine rows relative to each other,
    /// so the stamps must NOT be compared), and the frontend's un-splice reads
    /// it to invert the prepend before the next engine refill. Metadata,
    /// EXCLUDED from `PartialEq`/`Eq`.
    pub row_shift: usize,
    /// `snapshot_seq` as of the last operation that left this snapshot equal to
    /// an ENGINE fill shifted down by [`row_shift`](Self::row_shift) host rows —
    /// i.e. the engine fill itself, or a pure row PREPEND applied directly on
    /// top of one.
    ///
    /// This is the D-2 SPLICE's provenance token, and it is deliberately a
    /// SECOND clock rather than a relaxation of `engine_fill_seq`. The strip
    /// splice does not write into any engine row — it prepends chrome and slides
    /// the engine's rows down — so the engine's per-row revisions still describe
    /// composed index `r + row_shift` exactly. Every OTHER host mutator (stream
    /// fade, IME preedit overlay, prediction ghosts, the find bar, the config
    /// notice, the settings panel, every in-place band) DOES overwrite engine
    /// cells, and all of them bump `snapshot_seq` by the established discipline
    /// WITHOUT touching this field. So `snapshot_seq != shifted_fill_seq` means
    /// "some host wrote engine cells", and both the D-2 stamp gate and the
    /// frontend's un-splice fail closed — the fail-closed direction is the
    /// default, because only [`note_host_row_prepend`](Self::note_host_row_prepend)
    /// and an engine fill ever advance this.
    ///
    /// `0` is the NEVER-BLESSED sentinel: `empty()`, every composed frame (see
    /// [`invalidate_row_revisions`](Self::invalidate_row_revisions)), and any
    /// prepend applied on top of already-mutated cells. Metadata, EXCLUDED from
    /// `PartialEq`/`Eq`.
    pub shifted_fill_seq: u64,
}

/// The column order a [`RenderInput`]'s rows are in, as left by the last ENGINE
/// fill (DMG-1 carrier; see [`RenderInput::engine_row_order`]).
///
/// A two-variant state rather than a bool on purpose: it names the PROPERTY the
/// damage-scoped refill's continuity proof needs — "these retained rows are
/// still in the order a fresh logical fill would produce" — instead of a
/// yes/no about an implementation detail of the BiDi pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowOrder {
    /// No row was permuted: logical order and visual order coincide, for every
    /// row and every column-indexed channel (cells, clusters, combining marks,
    /// images, and the cursor column). This is every frame of a pure-LTR
    /// session, and every frame at all in a build without the `bidi` feature.
    #[default]
    Logical,
    /// At least one row was permuted into BiDi VISUAL order by
    /// `apply_bidi_reorder`. Rows in this state may not be retained by a
    /// damage-scoped refill: the permutation is not idempotent, so re-applying
    /// it to an already-visual row would double-permute it.
    BidiVisual,
}

/// WHICH continuity clause refused the damage-scoped arm — the per-cause
/// attribution `FrameRefill::Full` carries.
///
/// The scoped refill's validity check is a conjunction of independent clauses
/// (see
/// [`Terminal::cell_frame_damage_scoped_into`](crate::terminal::Terminal::cell_frame_damage_scoped_into)),
/// every one of them conservative. Before this existed a caller could see THAT
/// the chain broke — `frame_refills_full` climbing — but never WHICH clause
/// broke it, so a Full-dominated steady state was undiagnosable: the fix for a
/// host mutator that forgot its seq bump, a scratch shared across panes, and a
/// window whose row count disagrees with its engine are three completely
/// different repairs behind one number.
///
/// The variant is the FIRST clause that refused, in the check's own
/// short-circuit order — not the set of all clauses that would have refused. A
/// frame that both scrolled and took full damage reports the scroll, because
/// that is the clause the predicate stopped at.
///
/// A fieldless enum: it costs a register, never an allocation or a format, so
/// it is affordable on the per-presented-frame path that produces it.
///
/// `#[repr(usize)]` IS LOAD-BEARING, and the reason is an ABI cliff rather
/// than taste. `FrameRefill::Scoped` carries a `usize`; with a `u8` cause the
/// two variants' payloads are scalars of DIFFERENT widths, which drops the
/// enum out of rustc's ScalarPair ABI and makes `cell_frame_damage_scoped_into`
/// return through memory (an `x8` indirect result on AArch64) instead of two
/// registers. That was measured, not assumed: `#[repr(u8)]` cost +2.68% on
/// `keystroke_tick/scoped_extract/50x200` in 7/7 paired rounds — a real
/// regression on the arm this whole carrier exists to make fast — and
/// widening the discriminant to `usize` returned it to +0.10%, inside the
/// identical-binary control. The enum never lives anywhere but a register and
/// a tally index, so the eight bytes cost nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(usize)]
pub enum FullRefillCause {
    /// The last engine fill permuted at least one row into BiDi visual order
    /// (`engine_row_order != Logical`). The permutation is not idempotent, so
    /// retained rows would be double-permuted. Costs the frame AFTER an RTL row
    /// is on screen, and recovers once no row reorders.
    BidiVisual,
    /// The scratch carries no terminal identity (`terminal_id == 0`): it has
    /// never been engine-filled. Every first frame of a window reports this
    /// once, by construction.
    ScratchUnstamped,
    /// The scratch was last engine-filled by a DIFFERENT terminal. The split
    /// compositor's shared pane scratch does this on every hand-off; a steady
    /// state dominated by it means panes are alternating through one buffer
    /// that could instead be per-pane.
    TerminalMismatch,
    /// Another consumer called `take_damage` since the scratch's fill
    /// (`extract_gen` moved), so the tracker's bits no longer describe every
    /// row that changed under this scratch.
    DamageTaken,
    /// A HOST mutator wrote content channels after the engine filled the
    /// scratch and bumped `snapshot_seq` (stream fade, prediction ghosts, find
    /// bar, strip splice — the ghost-paint bump discipline). The most
    /// actionable cause: it names a specific per-frame mutator as the thing
    /// standing between this workload and the scoped arm.
    HostMutation,
    /// The scratch still has a host row PREPEND in force (`row_shift != 0`), so
    /// engine row `r` sits at index `r + row_shift` and retaining "row r" would
    /// serve a different row's content. The tab-strip splice's inverse
    /// (`undo_host_row_prepend`) did not run or could not run.
    RowShift,
    /// The alternate-screen bit differs between the scratch and the engine.
    AltScreen,
    /// The scratch's own `rows`/`cols` disagree with the requested frame size —
    /// a resize the scratch has not been refilled at yet.
    ScratchRows,
    /// The scratch's row COUNT (`cells.len()`) disagrees with the requested
    /// rows while its `rows` field agrees: a host prepend that grew the
    /// container without stamping a shift, the shape a tab strip leaves behind.
    ScratchRowCount,
    /// The REQUESTED frame size disagrees with the engine's own grid — the
    /// window and its terminal are momentarily out of step (a resize in
    /// flight).
    EngineDims,
    /// The engine's viewport is scrolled back (`display_offset != 0`). Tracker
    /// rows are live-grid rows and coincide with viewport rows only at offset
    /// 0.
    EngineScrolled,
    /// The engine is at offset 0 but the SCRATCH was filled while scrolled
    /// back, so its retained rows are viewport rows from a different mapping.
    /// The frame that returns to the bottom.
    ScratchScrolled,
    /// `base_y` moved: the live grid scrolled, so retained rows shifted out
    /// from under their indices (scroll damage marks only the exposed strip).
    /// The cause a scrolling workload — a build log, a `cat` of a large file —
    /// reports on nearly every frame, and honestly so.
    BaseY,
    /// The absolute row revision moved without `base_y` moving: a protected
    /// footer insertion or another row-identity change.
    RowRevision,
    /// The tracker holds `Damage::Full` — a palette recolor, DECSCNM, a reflow,
    /// or any other whole-grid invalidation. Nothing is retainable.
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
/// that re-resolved only the tracker's damaged rows. Returned (rather than
/// logged) so callers, benches, and the differential oracle can assert REACH —
/// a damage-scoped workload that silently degrades to `Full` every frame is a
/// perf regression the type makes visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRefill {
    /// The whole viewport was re-extracted (continuity break, full damage, or
    /// first fill). Always sound; the cost is the pre-carrier status quo.
    Full {
        /// WHICH continuity clause refused the scoped arm — the first one to
        /// refuse, in the check's short-circuit order. Carried rather than
        /// logged for the same reason the arm itself is: a caller that only
        /// learns THAT the chain broke cannot tell a forgotten seq bump from a
        /// scrolling workload, and those are opposite conclusions.
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
        RenderInput {
            rows: self.rows,
            cols: self.cols,
            cells: self.cells.clone(),
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
            cursor_visible: self.cursor_visible,
            cursor_style: self.cursor_style,
            cursor_effect_style_override: self.cursor_effect_style_override,
            cursor_trail: self.cursor_trail.clone(),
            cursor_trail_color: self.cursor_trail_color,
            cursor_glow_add: self.cursor_glow_add.clone(),
            glow_halo: self.glow_halo.clone(),
            glow_under: self.glow_under.clone(),
            fire_patch: self.fire_patch.clone(),
            cursor_fill_override: self.cursor_fill_override,
            word_decorations: self.word_decorations.clone(),
            ink: self.ink.clone(),
            char_fg: self.char_fg.clone(),
            fire_halo: self.fire_halo.clone(),
            cat_quads: self.cat_quads.clone(),
            cat_atlas: self.cat_atlas.clone(),
            free_sprites: self.free_sprites.clone(),
            free_atlas: self.free_atlas.clone(),
            nova_add: self.nova_add.clone(),
            rain_quads: self.rain_quads.clone(),
            rain_atlas: self.rain_atlas.clone(),
            rain_add: self.rain_add.clone(),
            display_offset: self.display_offset,
            base_y: self.base_y,
            absolute_row_revision: self.absolute_row_revision,
            scroll_frac_px: self.scroll_frac_px,
            grid_top_row: self.grid_top_row,
            grid_bot_row: self.grid_bot_row,
            fx_clip: self.fx_clip,
            selection: self.selection.clone(),
            selection_clip: self.selection_clip,
            selection_bg: self.selection_bg,
            selection_fg: self.selection_fg,
            selections: self.selections.clone(),
            clusters: self.clusters.clone(),
            combining: self.combining.clone(),
            line_sizes: self.line_sizes.clone(),
            line_size_spans: self.line_size_spans.clone(),
            default_bg_spans: self.default_bg_spans.clone(),
            images: self.images.clone(),
            wallpaper: self.wallpaper.clone(),
            default_bg: self.default_bg,
            default_fg: self.default_fg,
            cursor_color: self.cursor_color,
            snapshot_seq: self.snapshot_seq,
            process_sequence: self.process_sequence,
            input_hot: self.input_hot,
            terminal_id: self.terminal_id,
            extract_gen: self.extract_gen,
            engine_fill_seq: self.engine_fill_seq,
            engine_alt: self.engine_alt,
            engine_row_order: self.engine_row_order,
            row_rev: self.row_rev.clone(),
            row_rev_lane: self.row_rev_lane,
            row_shift: self.row_shift,
            shifted_fill_seq: self.shifted_fill_seq,
        }
    }

    /// CAPACITY-REUSING in-place update — the persistent-snapshot path the GPU
    /// present + CPU damage caches use to store the prior frame each changed frame.
    /// The derived `clone_from` falls back to `*self = source.clone()`, which
    /// deep-clones a fresh grid and drops the old one every call; this override
    /// delegates to each field's `clone_from` so `Vec::clone_from` reuses the
    /// destination's existing allocation for the common prefix (inner per-row Vecs
    /// recurse), so a stable-dimension frame reallocates NOTHING for the grid. The
    /// result is byte-for-byte identical to `*self = source.clone()`; only the
    /// allocation lifetime changes, so the same dirty sets follow from the same
    /// stored snapshot. (Ported from the prior render-api location under A-3.)
    fn clone_from(&mut self, source: &Self) {
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
        // (split-pane audit) `cursor_fill_override` was missing here — the
        // one field `clone` copied that this override didn't, violating the
        // byte-for-byte contract above with a stale forge fill in the stored
        // prior frame.
        self.cursor_fill_override = source.cursor_fill_override;
        self.cursor_glow_add.clone_from(&source.cursor_glow_add);
        self.glow_halo.clone_from(&source.glow_halo);
        self.glow_under.clone_from(&source.glow_under);
        self.fire_patch.clone_from(&source.fire_patch);
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
        self.scroll_frac_px = source.scroll_frac_px;
        self.grid_top_row = source.grid_top_row;
        self.grid_bot_row = source.grid_bot_row;
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
        self.cursor_color = source.cursor_color;
        self.snapshot_seq = source.snapshot_seq;
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
    }
}

/// Content equality for the per-row inline-image placements (the `images`
/// clause of [`RenderInput`]'s `PartialEq`), with cross-`Arc` payload verdicts
/// memoized per CALL (IMG-1).
///
/// WHY: `ImageRef`'s derived `Eq` deep-compares `ImageData` — `bytes` included
/// — whenever the two `Arc`s are pointer-DISTINCT (std's `Arc` eq only
/// short-circuits on pointer-EQUAL), and a re-transmitting client allocates a
/// FRESH `Arc` per transmit. The derived `Vec` equality therefore restarted
/// that memcmp at EVERY covered cell: O(covered_cells × payload_bytes) per
/// whole-input compare (e.g. ~10k covered cells × ~8 MB byte-identical kitty
/// video payloads ≈ 86 GB of memcmp for ONE equality check). The verdict for
/// a (left-Arc, right-Arc) PAIR is constant within one call, so it is
/// memoized: each distinct pair deep-compares ONCE — same verdict, the cost
/// scales with distinct images instead of covered cells × bytes. (The per-row
/// dirty diff in `aterm-render` carries the identical memo — `ImageEqMemo` —
/// for the identical reason.)
///
/// SOUNDNESS of the raw-pointer key: the memo lives only for this call, and
/// the two borrowed inputs keep every referenced `Arc` alive throughout — an
/// address can never be freed and reused by a DIFFERENT image while its
/// verdict is cached. That ABA hazard is why the map must never be persisted
/// across calls. `FxHashMap` allocates nothing until the first insert, so
/// image-free and same-`Arc` (steady) inputs pay nothing here.
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
        // The one deep compare this pair pays — the UNCHANGED derived
        // `ImageData` equality (bytes + format + cols + rows + z).
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

// Hand-written equality: compare rendered CONTENT only, NOT `snapshot_seq`.
// The CPU damage cache (`aterm-render`) compares the incoming snapshot against the
// previous frame's to decide which rows are dirty; `snapshot_seq` is metadata that
// changes every damaged frame, so including it would make every frame compare
// unequal and defeat row-level reuse. Every content field is itself `Eq`.
//
// ATLASES compare by SNAPSHOT IDENTITY (`Arc::as_ptr`), never `version`
// (split-pane audit): baker versions are deterministic PER ENGINE INSTANCE
// (the fingerprint contract), so a rebuilt engine replays its predecessor's
// version sequence with different texels — version-equality would call two
// different-content frames "equal", the exact stale-atlas aliasing the
// audit outlawed. Every rebake publishes a fresh `Arc`, and a stable frame
// re-presents the same `Arc`, so identity is both sound and stable — the
// same law as the dirty-row gates and the GPU texture cache.
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
            // Images: memoized cross-`Arc` content equality — same verdict as
            // the derived compare, priced per DISTINCT image pair instead of
            // per covered cell (see `images_eq`).
            && images_eq(&self.images, &other.images)
            && self.wallpaper.as_ref().map(std::sync::Arc::as_ptr)
                == other.wallpaper.as_ref().map(std::sync::Arc::as_ptr)
            && self.default_bg == other.default_bg
            && self.default_fg == other.default_fg
            && self.cursor_color == other.cursor_color
        // `snapshot_seq` intentionally NOT compared — see the impl comment.
        // `scroll_frac_px` / `grid_top_row` / `grid_bot_row` are also NOT compared:
        // they are consumed at PRESENT time (the sub-row translate re-derives the
        // presented pixels from the pristine damage cache each frame), not in the
        // content damage diff. Comparing them would force a full repaint on a
        // frac-only change (a drag frame) that dirties no cell — the translate needs
        // only a re-present, not a re-raster. See `RenderInput::scroll_frac_px`.
        // `input_hot` is likewise NOT compared: it is a present-time bloom-defer hint
        // (see its doc), so a hot→settle transition must not by itself force a repaint
        // — the animating comet already differs frame-to-frame while a halo is pending.
        // `base_y` and `absolute_row_revision` are NOT compared either: they are
        // host-consumed re-anchor metadata (like `snapshot_seq`) that do not affect
        // rendered pixels, so metadata-only changes must not force a raster repaint.
        // `terminal_id` / `extract_gen` / `engine_fill_seq` / `engine_alt` /
        // `engine_row_order` (DMG-1
        // damage carrier) are extraction-continuity tokens, not rendered content:
        // comparing them would make every scoped refill compare unequal and defeat
        // the very dirty-gate they exist to feed — same law as `snapshot_seq`.
        // `row_rev` / `row_rev_lane` (D-2 per-row revision) are excluded for the
        // SAME reason and it is load-bearing: the stamps advance on every damaged
        // frame, so folding them into content equality would make two snapshots of
        // an unchanged screen compare unequal and turn every gate hit into a
        // repaint — the exact opposite of what they exist to buy.
    }
}

impl Eq for RenderInput {}

impl Default for RenderInput {
    fn default() -> Self {
        Self::empty()
    }
}

impl RenderInput {
    /// An empty 0×0 snapshot with no allocations — the seed for a persistent
    /// scratch buffer that [`Terminal::cell_frame_into`](crate::terminal::Terminal::cell_frame_into)
    /// refills in place each frame (C-1). Cursor scalars default to off/origin and
    /// `snapshot_seq` to 0. `cell_frame_into` overwrites the engine-owned grid,
    /// cursor (including its live colour), live implicit background, selection
    /// (including its live colours), and snapshot metadata; hosts must stamp or
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
            scroll_frac_px: 0,
            grid_top_row: 0,
            grid_bot_row: 0,
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
            cursor_color: COLOR_UNSET,
            snapshot_seq: 0,
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
        }
    }

    /// Disown the D-2 per-row revision lane: this snapshot's rows are no longer
    /// the rows an engine fill stamped, so nothing may be concluded from their
    /// revisions.
    ///
    /// The seam every HOST producer that REBUILDS rows must call — the split
    /// compositor, which composes the grid out of several terminals' panes and
    /// leaves row `r` meaning something entirely different; without this the
    /// stale lane would answer for rows it never saw.
    ///
    /// A pure row PREPEND (the in-grid tab strip) is the ONE re-shaping that no
    /// longer lands here: it does not rewrite any engine row, it slides them
    /// down by a known count, so
    /// [`note_host_row_prepend`](Self::note_host_row_prepend) SPLICES the lane
    /// instead of disowning it (prepending the `0`/UNKNOWN sentinel for the
    /// chrome rows). That path still falls back here whenever it cannot prove
    /// the shift is the only thing that happened.
    ///
    /// Host mutators that write cells IN PLACE (stream fade, the IME preedit
    /// overlay, prediction ghosts) do not need this — they keep row identity and
    /// already self-report through `snapshot_seq != shifted_fill_seq`, which the
    /// consumer's gate requires — but calling it is always sound: the only cost
    /// of a disowned lane is the exact whole-grid compare the lane exists to
    /// avoid.
    ///
    /// The prepend bookkeeping is reset with the lane: a composed frame's rows
    /// are not "an engine fill shifted down", so `row_shift` is meaningless for
    /// it and `shifted_fill_seq` returns to its never-blessed `0`.
    pub fn invalidate_row_revisions(&mut self) {
        self.row_rev_lane = 0;
        self.row_rev.clear();
        self.row_shift = 0;
        self.shifted_fill_seq = 0;
    }

    /// Record that a host just PREPENDED `prepended` rows to this snapshot (the
    /// in-grid tab strip), splicing the D-2 revision lane to match instead of
    /// disowning it.
    ///
    /// Call it from the prepend itself, AFTER every per-row channel has been
    /// shifted and after the `snapshot_seq` bump the prepend owes the renderer's
    /// content cache. `blessed` is the caller's proof, taken BEFORE it mutated
    /// anything, that the snapshot was still exactly an engine fill (or an
    /// already-blessed shift of one) — see
    /// [`host_prepend_blessing`](Self::host_prepend_blessing), which is that
    /// predicate.
    ///
    /// Blessed, the lane gains `prepended` leading `0`s. `0` is the NO-STAMP
    /// sentinel and means UNKNOWN, never "unchanged", so the chrome rows fall
    /// back to the exact content compare — which is the right answer, because
    /// the chrome IS host-written on every frame — while engine row `r` keeps
    /// its revision at its new index `r + row_shift`. Unblessed, the lane is
    /// disowned outright: some host had already overwritten engine cells, so
    /// the stamps no longer describe the rows they are attached to.
    ///
    /// `row_shift` advances either way, because it is the physical row count an
    /// un-splice must remove, not a claim about provenance.
    pub fn note_host_row_prepend(&mut self, prepended: usize, blessed: bool) {
        self.row_shift = self.row_shift.saturating_add(prepended);
        if blessed {
            self.row_rev.splice(0..0, (0..prepended).map(|_| 0u64));
            self.shifted_fill_seq = self.snapshot_seq;
        } else {
            self.row_rev_lane = 0;
            self.row_rev.clear();
            self.shifted_fill_seq = 0;
        }
    }

    /// Whether a host row PREPEND about to be applied to this snapshot may keep
    /// the D-2 revision lane — i.e. whether the snapshot is, right now, exactly
    /// what the last engine fill left (possibly already shifted by an earlier
    /// blessed prepend) with a lane that still covers every row.
    ///
    /// Read BEFORE the prepend mutates anything, and handed to
    /// [`note_host_row_prepend`](Self::note_host_row_prepend).
    #[must_use]
    pub fn host_prepend_blessing(&self) -> bool {
        self.row_rev_lane != 0
            && self.row_rev.len() == self.rows
            && self.cells.len() == self.rows
            && self.snapshot_seq == self.shifted_fill_seq
            && (self.row_shift > 0 || self.snapshot_seq == self.engine_fill_seq)
    }

    /// UNDO a blessed host row prepend, restoring this snapshot to the engine
    /// fill it was made from — the inverse the frontend runs on its persistent
    /// scratch before handing it back to
    /// [`Terminal::cell_frame_damage_scoped_into`](crate::terminal::Terminal::cell_frame_damage_scoped_into),
    /// so a tab-strip window can take the damage-scoped arm at all.
    ///
    /// Returns `false` — changing NOTHING — unless every clause of the inverse
    /// holds: something was prepended, the prepend was BLESSED and is still the
    /// only thing that has happened since the engine fill
    /// (`snapshot_seq == shifted_fill_seq`, non-zero), and the three channels a
    /// damage-scoped refill RETAINS are all exactly `engine_rows + row_shift`
    /// long. Any doubt costs only the full refill this is trying to avoid.
    ///
    /// SCOPE, stated exactly: this restores the channels a scoped refill retains
    /// — `cells`, `clusters`, `combining` — plus the row count and the
    /// continuity tokens. It does NOT un-shift `cursor_row`, the selections, or
    /// the row-tagged effect streams, because the very next fill restamps every
    /// one of them unconditionally (`cell_frame_fill` re-reads the cursor, the
    /// selection, the clip and the pane list; the frontend re-publishes every
    /// effect stream per frame). An un-spliced scratch is therefore an
    /// INTERMEDIATE, never a presentable frame.
    ///
    /// The drained chrome-row buffers are handed to `pool` (up to `pool_cap`
    /// entries) so the next prepend reuses their capacity; the drain removes the
    /// whole range regardless of the cap.
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
        // The snapshot's CELLS are once again byte-for-byte the engine's, so the
        // DMG-1 token that says exactly that becomes true again. This is the one
        // place it may be re-armed, and only after the clauses above have proven
        // the prepend was the sole host write.
        self.snapshot_seq = self.engine_fill_seq;
        self.shifted_fill_seq = self.engine_fill_seq;
        true
    }

    /// Whether ANY selection is live on this frame — the scalar one or any pane
    /// entry. The gate every renderer uses before entering selection-aware work
    /// (the sparkle freeze, the ligature breaks, the E7 scroll-blit veto).
    #[must_use]
    #[inline]
    pub fn any_selection(&self) -> bool {
        self.selection.has_selection() || !self.selections.is_empty()
    }

    /// Which selection paints a frame-space cell, as an index usable with
    /// [`Self::selection_colors_at`] — `None` when the cell is unselected.
    ///
    /// This is the single renderer predicate: CPU and GPU callers pass the frame
    /// row/column they are about to paint, and this method performs the viewport
    /// (`display_offset`) projection before consulting [`TextSelection`].
    ///
    /// With [`selections`](Self::selections) EMPTY it is exactly the historical
    /// scalar test and answers `Some(0)`; with entries present it returns the
    /// FIRST entry whose clip and selection both accept the cell. Pane clips are
    /// disjoint by construction (each is its pane's box), so "first" is also
    /// "only" — the scan order is fixed purely so the answer is deterministic if
    /// a host ever hands over overlapping boxes.
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
            // The historical scalar path, in its original order (clip first, so a
            // clipped-away cell still costs one rectangle test) — this runs per
            // painted cell on every single-terminal frame.
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
        // Entry anchors were relocated into FRAME rows by the host (pane origin +
        // that pane's own `display_offset`), so no second viewport projection
        // applies here; a composed frame's own `display_offset` is zero for
        // exactly this reason.
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
    /// logical selection and any host composition clip.
    ///
    /// The boolean face of [`Self::selection_hit`] — every caller that paints a
    /// cell without needing to know WHICH pane owns it. One predicate, two
    /// backends: CPU/GPU parity holds by construction because both go through
    /// here.
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

    /// The `(bg, fg, inactive)` selection colour authority behind the index
    /// [`Self::selection_hit`] returned — the entry's own live OSC 17/19 state
    /// in a split, the scalar fields on a single-terminal frame. Colours are
    /// still RESOLVED by the renderer (theme, active/inactive policy); this only
    /// says which terminal's values feed that resolution.
    #[must_use]
    #[inline]
    pub fn selection_colors_at(&self, index: usize) -> (u32, u32, bool) {
        match self.selections.get(index) {
            Some(entry) => (entry.bg, entry.fg, entry.inactive),
            None => (self.selection_bg, self.selection_fg, false),
        }
    }

    /// The inclusive selected column span on one FRAME row after applying the
    /// viewport projection and optional split-composition clip.
    ///
    /// **SCAN WINDOW, not an equality key.** With
    /// [`selections`](Self::selections) non-empty this is the HULL over every
    /// pane's span on the row, so two panes selected on one row report one
    /// merged `(lo, hi)`. That is correct for the only two callers — the CPU and
    /// GPU sparse-tail guards, which re-test every cell with
    /// [`Self::selection_contains_cell`] — and WRONG for any diff, because a
    /// hull can stay fixed while a pane's real span moves inside it. Damage
    /// diffs use [`Self::selection_row_key`].
    ///
    /// The span is content-independent, so it does not in general cover the
    /// painted run: [`TextSelection::contains_cell`] snaps a double-width glyph
    /// whole for EVERY selection kind, and an edge landing on a lead paints the
    /// continuation one column past `hi`. That widened cell can nonetheless
    /// never be the omitted tail cell this window exists to catch, and the
    /// guarantee is structural rather than a property of how the grid fills
    /// itself: widening fires only when the OTHER half is present in `cells` —
    /// every renderer site spells `is_wide_lead` as
    /// `cells.get(c + 1).is_some_and(|n| n.wide)` and takes the continuation bit
    /// off a materialized cell — so a widened cell is by construction inside the
    /// materialized prefix, and the sparse-tail loop, which passes both width
    /// flags `false`, cannot widen at all. Block still pads one column on each
    /// side, harmlessly: its `lo`/`hi` are rectangle edges shared by every row.
    #[must_use]
    pub fn selection_row_span(&self, frame_row: usize) -> Option<(u16, u16)> {
        if self.selections.is_empty() {
            return selection_row_span_of(
                &self.selection,
                self.selection_clip,
                self.display_offset,
                frame_row,
            );
        }
        self.selections
            .iter()
            .filter_map(|entry| {
                selection_row_span_of(&entry.selection, Some(entry.clip), 0, frame_row)
            })
            .reduce(|(a_lo, a_hi), (b_lo, b_hi)| (a_lo.min(b_lo), a_hi.max(b_hi)))
    }

    /// The LOSSLESS per-row selection damage key: one
    /// [`SelectionRowKey`] per selection that reaches frame row `frame_row`, in
    /// list order, with the colours that band will paint in.
    ///
    /// This is what a damage diff must compare. Unlike
    /// [`Self::selection_row_span`] it never merges two panes' spans, so
    /// dragging one pane's edge inward while a sibling holds a selection on the
    /// same row changes the key even though the hull is unmoved — the frozen
    /// highlight that a hull-based diff produces. Carrying the colours folds the
    /// old scalar `selection_bg`/`selection_fg` damage trigger in, which likewise
    /// goes lossy the moment colours are per-pane.
    ///
    /// Writes into `out` (cleared first) so a per-frame diff allocates once.
    pub fn selection_row_key(&self, frame_row: usize, out: &mut Vec<SelectionRowKey>) {
        out.clear();
        if self.selections.is_empty() {
            if let Some((lo, hi)) = selection_row_span_of(
                &self.selection,
                self.selection_clip,
                self.display_offset,
                frame_row,
            ) {
                out.push(SelectionRowKey {
                    lo,
                    hi,
                    bg: self.selection_bg,
                    fg: self.selection_fg,
                    inactive: false,
                });
            }
            return;
        }
        for entry in &self.selections {
            if let Some((lo, hi)) =
                selection_row_span_of(&entry.selection, Some(entry.clip), 0, frame_row)
            {
                out.push(SelectionRowKey {
                    lo,
                    hi,
                    bg: entry.bg,
                    fg: entry.fg,
                    inactive: entry.inactive,
                });
            }
        }
    }

    /// Drop every host-owned VISUAL BLING layer, leaving only the terminal cell content
    /// (grid + cursor position/colour + selection + images). Empties the cursor motion
    /// trail and cursor-shape override, the LUMEN cursor glow, the GLOW-HALO
    /// radial cursor-effect light,
    /// the EMBERFORGE under-glyph flame-body light, the per-pixel FIRE-FIELD
    /// patches (`fire_patch`) and the charred glyph-ink fg
    /// overrides, the fire contrast-halo strengths (`fire_halo`),
    /// the sparkle-word decorations, the animated ink fg
    /// overrides, the peeking-cat sprites (quads + atlas Arc nulled), the supernova
    /// additive light, the PHOSPHOR rain (quads + additive halos + atlas Arc
    /// nulled). Used
    /// by the `image plain` introspection capture so an AI reads the bare screen — the
    /// bling is architecturally a set of separate layers, so suppressing it is exactly
    /// this. The cell grid is untouched.
    pub fn clear_overlays(&mut self) {
        self.cursor_effect_style_override = None;
        self.cursor_trail.clear();
        self.cursor_glow_add.clear();
        self.glow_halo.clear();
        self.glow_under.clear();
        self.fire_patch.clear();
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
    /// historical frame-wide [`default_bg`](Self::default_bg). The returned
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

    /// The inline-image reference covering viewport cell `(row, col)`, if any.
    /// A cell with an image SKIPS its glyph on both the CPU and GPU paths; the
    /// renderer blits the image tile instead. Used by both renderers to stay in
    /// lockstep on the image-vs-glyph precedence rule. (Sorted by column — see
    /// [`cluster_at`](Self::cluster_at) — so this binary-searches.)
    #[must_use]
    pub fn image_at(&self, row: usize, col: usize) -> Option<&aterm_grid::ImageRef> {
        let row = self.images.get(row)?;
        row.binary_search_by_key(&col, |(c, _)| *c)
            .ok()
            .map(|i| &row[i].1)
    }

    /// The DEC line-size RUN governing (`row`, `col`): its line size and the
    /// composite column range `start_col..end_col` it spans.
    ///
    /// This is the ONE seam both renderers use to place a glyph horizontally, so
    /// they cannot disagree about a composed row. The returned range is also the
    /// clip box: a double-width run must not bleed past its own pane.
    ///
    /// Fast path — and the only path a single-pane frame ever takes — is an empty
    /// [`line_size_spans`](Self::line_size_spans) row, which yields the uniform
    /// run `0..cols` at [`line_sizes`](Self::line_sizes)`[row]`. That reduces the
    /// caller's arithmetic to exactly the pre-span `col * cell_w`.
    ///
    /// A row with spans returns the run containing `col`. Spans partition
    /// `0..cols`, so a miss is impossible for an in-range column; an out-of-range
    /// column falls back to the uniform run rather than panicking, because the
    /// renderers index this from clamped-framebuffer loops.
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
        // A row WITH runs does not consult `line_sizes[row]`: that value is only a
        // summary for row-level consumers (see the field docs), and a column no
        // run claims is unclaimed composite space — a divider seam, or padding
        // beside a pane — which renders single-width by definition.
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

    /// Whether the image at (`row`,`col`), if any, HIDES the cell's glyph — i.e. it
    /// is drawn OVER the text (`z_index >= 0`, the default). A Kitty `z < 0` image is
    /// drawn BEHIND the text, so it does NOT hide the glyph and this returns `false`.
    /// Both renderers gate glyph drawing on this, so the image/text z-order matches.
    #[must_use]
    pub fn image_hides_glyph_at(&self, row: usize, col: usize) -> bool {
        self.image_at(row, col)
            .is_some_and(|r| r.image.z_index >= 0)
    }

    /// Composite an IME PREEDIT (the in-flight composition text) into this
    /// frame at the cursor — the standard inline-at-the-caret presentation
    /// every native macOS text surface gives CJK and dead-key input, which
    /// aterm previously approximated with a `title [‹preedit›]` indicator in
    /// the WINDOW TITLE, a corner of the screen nobody composing text is
    /// looking at.
    ///
    /// PURE FRAME COMPOSITING. `cells` is a per-frame PROJECTION of the grid,
    /// so overwriting cells here touches no terminal state: the composition is
    /// visible exactly while the host keeps passing it, and vanishes the frame
    /// after commit/cancel with nothing to undo. (The committed text then
    /// arrives through the ordinary PTY echo path like any typed input.)
    ///
    /// Presentation rules:
    /// * drawn from the cursor cell, in the cursor cell's colors, single
    ///   UNDERLINE (the IME convention for uncommitted text), no bold/italic
    ///   inheritance — composition text is its own thing, not a continuation
    ///   of whatever SGR state the prompt happened to leave;
    /// * wide characters (the common case — CJK) occupy a lead cell plus a
    ///   `wide` continuation, same contract the renderers already honor;
    /// * TRUNCATED at the right edge (no wrap): a v1 honesty choice — the
    ///   composition popover shows the full text, and wrapping would need the
    ///   next row's ownership story;
    /// * zero-width characters (combining marks fresh off a dead key) merge
    ///   into the preceding cell like the write path would — they advance
    ///   nothing and draw nothing of their own here;
    /// * the CURSOR moves to the caret — `caret_byte` is the byte offset the
    ///   platform reported (winit `Ime::Preedit`'s cursor start); `None` (or
    ///   past-the-end) parks it after the last composed cell, clamped to the
    ///   final column.
    ///
    /// `cjk` selects East-Asian-Ambiguous width, mirroring the write path's
    /// `char_width` so the composition occupies exactly the columns the
    /// committed text will.
    ///
    /// # The row is RAGGED
    ///
    /// A snapshot row carries only the row's WRITTEN PREFIX
    /// (`render_row_into_impl` pushes `view.len()` cells, not `cols`), and at a
    /// shell prompt the cursor sits EXACTLY at its end — `$ ` is a two-cell row
    /// with the cursor at column 2. Writing through `cells[col]` there landed on
    /// nothing at all: the composition painted zero glyphs and the caret still
    /// jumped its width, so the dominant case — composing at a prompt, which is
    /// where people type — showed the OS candidate window hovering over a grid
    /// that never changed. So the overlay MATERIALIZES the cells it paints, the
    /// way `paint_prediction_ghosts` does for speculative echo, padding any gap
    /// with the frame's live implicit blank rather than a clone of the last
    /// written cell (whose SGR background would smear across the sparse tail).
    pub fn overlay_ime_preedit(&mut self, preedit: &str, caret_byte: Option<usize>, cjk: bool) {
        if preedit.is_empty() || self.cursor_row >= self.rows {
            return;
        }
        let row = self.cursor_row;
        let cols = self.cols;
        // Resolved BEFORE the row borrow: `default_bg_at` reads `self`.
        let implicit = self.implicit_blank_at(row, self.cursor_col);
        let Some(cells) = self.cells.get_mut(row) else {
            return;
        };
        // The style seed: the cursor cell's resolved colors (theme-correct by
        // construction — they went through the same palette/DECSCNM resolution
        // as every other cell this frame). Past the written prefix there IS no
        // cursor cell, and `unwrap_or_default()` handed back a BLACK-ON-BLACK
        // placeholder — so even once the cells were materialized the
        // composition would have been painted invisible. The live implicit
        // blank is the honest answer (it is what the terminal paints at an
        // unwritten column); the row's last real cell covers a producer that
        // resolved no defaults at all (a pristine `Terminal::new`).
        let seed = cells
            .get(self.cursor_col)
            .copied()
            .or(implicit)
            .or_else(|| cells.last().copied())
            .unwrap_or_default();
        // The pad for the gap between the row's end and the cursor: a blank in
        // the seed's colors with every decoration DROPPED — those columns are
        // not part of the composition and must not pick up its underline.
        let pad = RenderCell {
            ch: ' ',
            fg: seed.fg,
            bg: seed.bg,
            ..RenderCell::default()
        };
        let start_col = self.cursor_col;
        // A composition that STARTS on a wide character's continuation column is
        // overwriting that character's right half — exactly what the write path
        // does when you type into it — so its LEAD must go too. Left standing,
        // the renderer draws the old glyph across both columns and paints it
        // straight over the composition's first cell: the same failure the
        // `clusters` drop below fixes for multi-scalar graphemes, which plain
        // wide characters do not go through. Read BEFORE the walk overwrites it.
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
                let lead = crate::terminal::RenderCell {
                    ch,
                    fg: seed.fg,
                    bg: seed.bg,
                    wide: false,
                    emoji_presentation: false,
                    text_presentation: false,
                    bold: false,
                    italic: false,
                    underline: crate::terminal::UnderlineStyle::Single,
                    strikethrough: false,
                    overline: false,
                    underline_color: None,
                };
                if let Some(cell) = cells.get_mut(col) {
                    *cell = lead;
                }
                if width == 2
                    && let Some(cont) = cells.get_mut(col + 1)
                {
                    *cont = crate::terminal::RenderCell {
                        ch: ' ',
                        wide: true,
                        underline: crate::terminal::UnderlineStyle::Single,
                        ..lead
                    };
                }
            },
        );
        // The overlay puts its scalar in `ch`, but a cell that previously held a
        // MULTI-SCALAR grapheme also has that text in `clusters` (and its marks
        // in `combining`), and both renderers prefer those over `ch` when they
        // resolve the glyph. Left behind, composing over a flag or a ZWJ emoji
        // would paint the OLD grapheme on top of the new composition. Free in
        // the common case: a row with no complex graphemes has an empty list.
        if end_col > start_col {
            let mut lo = start_col;
            let mut hi = end_col;
            if let Some(lead) = straddled_lead
                && let Some(cell) = cells.get_mut(lead)
            {
                *cell = pad;
                lo = lead;
            }
            // The mirror at the far end: a composition that STOPS inside a wide
            // character leaves that character's continuation behind with its
            // lead already overwritten — a stranded blank wearing the old cell's
            // background, one column wider than the composition really is.
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

    /// The frame's IMPLICIT BLANK at viewport cell (`row`, `col`): the cell the
    /// terminal itself paints at a column no program has written yet — a space
    /// in the LIVE default colours, which already carry OSC 10/11, their OSC
    /// 110/111 resets, DECSCNM, and (on a composed split frame) the per-pane
    /// [`default_bg_spans`](Self::default_bg_spans) refinement.
    ///
    /// `None` when the producer resolved no defaults ([`COLOR_UNSET`] — a
    /// pristine `Terminal::new`, or a standalone renderer harness): the sentinel
    /// is not a colour, and masking its high byte off would read as pure black.
    /// Callers fall back to the row's own cells rather than invent one.
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

/// THE ONE column walk an IME composition takes: every character's width under
/// the frame's ambiguous-width mode, the right-edge truncation, and where the
/// platform's caret byte lands among them.
///
/// [`RenderInput::overlay_ime_preedit`] paints through `emit` (called once per
/// character that FITS, with its scalar, its starting column and its width);
/// [`ime_preedit_caret_col`] passes a no-op and keeps only the caret. Sharing
/// the walk is the point: the composition's ink and the rectangle the OS
/// candidate window is anchored on are then the same arithmetic, so they cannot
/// drift apart the way two hand-kept copies would.
///
/// Returns `(end_col, caret_col)` — the column the composition stopped at
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

/// WHERE THE COMPOSITION'S OWN CARET IS, in columns — the same answer
/// [`RenderInput::overlay_ime_preedit`] leaves in `cursor_col`, resolved without
/// painting anything.
///
/// The OS candidate window is anchored on the caret RECTANGLE the host reports
/// (`set_ime_cursor_area`), and an in-flight composition has not reached the PTY
/// yet — so the ENGINE cursor still sits at the column the composition STARTS
/// at, and anchoring there walks the candidate list left by the whole width of
/// what you have typed so far. Japanese phrase input routinely composes twenty
/// columns before committing; the list ends up under the beginning of the
/// phrase instead of under the cursor. The find well and the rename well already
/// anchor on their own splice's caret cell — this is the grid's version of the
/// same answer, for the split, single-pane and capture paths alike.
///
/// `start_col` is the frame's cursor column and `cols` the frame width, both in
/// the same coordinate space the overlay would paint in (pane-local for a split
/// leaf).
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

/// THE RAGGED ROW — the case every one of the fixtures above misses.
///
/// `frame()` hands the overlay a rectangular `cols`-wide grid, which no real
/// snapshot is: `render_row_into_impl` materializes a row's WRITTEN PREFIX and
/// stops. At a shell prompt the cursor sits exactly at that prefix's end, so
/// `cells[cursor_col]` did not exist, `cells.get_mut(col)` swallowed every
/// write, and the composition was invisible in the one place people compose.
/// These drive the overlay from a REAL `Terminal` extraction so the raggedness
/// is the engine's, not a fixture's opinion of it.
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

    /// The row really is short — if this ever fails the rest of the module is
    /// testing a case that no longer exists, not a fix that still works.
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

    /// THE REGRESSION. Composing at a prompt painted NOTHING: the row ended at
    /// the caret, every `cells.get_mut(col)` missed, and only the cursor moved.
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

    /// Materializing is not enough on its own: `unwrap_or_default()` seeded the
    /// style from a placeholder `RenderCell`, whose colors are BLACK ON BLACK.
    /// Composed text drawn in it would be painted and still invisible, so the
    /// seed falls back to the frame's live implicit blank.
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

    /// A cursor parked RIGHT of the row's end (a tab, or a program that moved
    /// it there) leaves a gap. It is padded with the live implicit blank — no
    /// glyph and, critically, no underline: those columns are not the
    /// composition and must not be dressed as it.
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

    /// Composing OVER a multi-scalar grapheme. The overlay sets `ch`, but both
    /// renderers resolve such a cell's glyph from `clusters` — so a stale entry
    /// paints the OLD grapheme on top of the composition.
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

/// THE NEIGHBOURS OF THE RAGGED-ROW FIX — the cases materializing the row could
/// plausibly have broken, driven (like the module above) from real `Terminal`
/// extractions so the geometry is the engine's.
///
/// Materialization only ever runs when `cells.len() < col + width`, so a row
/// that is already full must come out exactly as it did before the fix; and the
/// right-edge gate runs BEFORE the `resize`, so a composition that does not fit
/// must not grow the row on its way to being truncated. Column 0 of a row with
/// NO written prefix is the extreme of the ragged case (`cells[row]` is an empty
/// `Vec`, not a short one), and the `cjk` argument — the one input no test in
/// this file exercised — has to reach the width call it selects.
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

    /// A FULL row: `resize` never fires, and the seed must still come from the
    /// cursor cell that genuinely exists there — not from the live default the
    /// ragged fallback introduced. This is the pre-fix path, unchanged.
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

    /// WIDER THAN THE ROW'S REMAINDER: the composition grows the row to the
    /// frame's width and stops there. The gate proves `col + width <= cols`
    /// before the `resize`, so truncation can never extend past the frame.
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

    /// A WIDE char with exactly one column left. The right-edge gate fires
    /// before the `resize`, so nothing is painted AND nothing is materialized —
    /// the frame is byte-identical but for the caret clamp.
    #[test]
    fn a_wide_composition_with_one_column_left_paints_nothing_and_grows_nothing() {
        let mut f = painted_frame(2, 8, b"$ ");
        f.cursor_col = 7;
        let before = f.clone();
        f.overlay_ime_preedit("日", None, false);
        assert_eq!(f.cells, before.cells, "not one cell was touched");
        assert_eq!(f.cursor_col, 7, "and the caret stays on the cell it was on");
    }

    /// The platform caret at byte 0 — the composition is painted in full and
    /// the cursor sits at its START, not after it. (The caret is resolved on the
    /// same walk that materializes, so the two cannot disagree.)
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

    /// Composing ONTO a wide character. The overlay's contract is that the
    /// composition occupies exactly the columns the committed text will, and the
    /// write path takes the WHOLE wide character with it — so neither half may
    /// be left standing. A stranded CONTINUATION is a blank wearing the old
    /// cell's background, one column wider than the composition really is.
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

    /// The other half of the same rule, and the worse of the two: a stranded
    /// LEAD is re-drawn across BOTH its columns, painting the old glyph straight
    /// over the composition's first cell — the composition is invisible.
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

    /// The cluster drop follows the cells it clears: a MULTI-SCALAR wide
    /// grapheme composed onto from its continuation column has its `clusters`
    /// entry at the LEAD, one column left of where the composition starts.
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

/// THE ANCHOR AND THE INK ARE ONE WALK.
///
/// [`ime_preedit_caret_col`] is what the OS candidate window is anchored on;
/// [`RenderInput::overlay_ime_preedit`] is what the user sees. They run the same
/// `walk_ime_preedit`, and this is the proof that they cannot disagree — across
/// every shape the walk has a branch for.
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

    /// The whole point of the query, stated once: while a composition is in
    /// flight the ENGINE cursor has not moved — the committed text has not
    /// reached the PTY — so the engine column is where the composition BEGINS,
    /// and the caret is that plus everything typed so far.
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

    /// The uniform row is the ONLY path a single-pane frame takes, and it must
    /// reduce to the pre-span behaviour exactly: the row's own line size, over
    /// the whole grid, so callers compute `col * cell_w` as before.
    #[test]
    fn empty_spans_yield_the_row_line_size_over_the_whole_row() {
        let mut i = input(2, 10);
        i.line_sizes[1] = LineSize::DoubleWidth;
        assert_eq!(i.line_size_run_at(0, 0), (LineSize::SingleWidth, 0, 10));
        assert_eq!(i.line_size_run_at(0, 9), (LineSize::SingleWidth, 0, 10));
        assert_eq!(i.line_size_run_at(1, 4), (LineSize::DoubleWidth, 0, 10));
    }

    /// A run governs exactly its own columns; a column outside every run falls
    /// back to single width, which is what an unclaimed composite column renders
    /// as. This is the property that keeps one pane's DECDWL off its neighbour.
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

    /// On a row that carries runs, `line_sizes[row]` is only a SUMMARY for
    /// row-level consumers — it says "some pane here is a DEC line". Placement
    /// must NOT inherit it: a divider seam between two panes is unclaimed
    /// composite space and renders single-width, even though the summary is
    /// double-width. This is the exact confusion the old row-level field caused.
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
    use aterm_grid::{ImageData, ImageFormat, ImageRef};
    use std::sync::Arc;

    /// IMG-1 DIFFERENTIAL ORACLE for [`super::images_eq`]: the memoized
    /// whole-field compare must agree with the derived `Vec<Vec<…>>` equality
    /// on every shape — and the two load-bearing directions are pinned at the
    /// `RenderInput` level: a re-transmit (fresh `Arc`s, identical bytes)
    /// still compares EQUAL (repaint suppression preserved), while a one-byte
    /// payload change still compares UNEQUAL (the deep compare is priced per
    /// distinct Arc pair, never dropped).
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
        let cases = [
            Vec::new(),                     // empty vs covered
            a.clone(),                      // same Arcs (ptr_eq path)
            fill(&mk(payload.clone(), 0)),  // re-transmit: distinct Arc, equal bytes
            fill(&mk(flipped, 0)),          // one payload byte differs
            fill(&mk(payload.clone(), -1)), // z-only difference
            a[..1].to_vec(),                // row-count mismatch
        ];
        for (i, b) in cases.iter().enumerate() {
            assert_eq!(
                super::images_eq(&a, b),
                a == *b,
                "case {i}: memoized equality must match the derived equality"
            );
        }

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
            }),
            cell_row: 0,
            cell_col: 0,
        }
    }

    #[test]
    fn image_hides_glyph_only_when_z_is_nonnegative() {
        let mut input = RenderInput::empty();
        // col 0: z=0 (over text, default) — hides; col 1: z=-1 (behind) — does NOT;
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
        // The per-row (col, _) lists are sorted by column with one entry per column
        // (built ascending; re-sorted after BiDi reorder / pane compose). The
        // accessors binary-search that order, so verify they resolve the RIGHT entry
        // at each populated column — including GAPS between columns — and return None
        // for absent columns. A regression to a linear scan would still pass this;
        // a BROKEN binary search (e.g. an unsorted list) would miss the gapped entries.
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
    /// the pane that owns it, the hull spans BOTH (a scan window), and the
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
            input.selection_colors_at(1),
            (0x0011_2233, COLOR_UNSET, true),
            "colours come from the entry the hit named"
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

    /// An EMPTY list is the historical scalar path, byte for byte: the scalar
    /// selection is still the whole authority, and `any_selection` agrees.
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
        assert_eq!(
            input.selection_colors_at(0),
            (COLOR_UNSET, COLOR_UNSET, false)
        );

        // A non-empty list REPLACES the scalar authority entirely.
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

        // The LIST clause of `any_selection` on its own — scalar EMPTY, one live
        // entry. This is the shape a composed frame takes for an UNFOCUSED pane, and
        // it is what the renderer's frame gates read: `scroll_blittable_content`'s
        // E7 blit veto, the additive-sparkle freeze, and the ligature-break pass all
        // ask `any_selection()`. Before the list existed they asked
        // `selection.has_selection()`, which is FALSE here — so without this clause
        // an unfocused pane's highlight could be scroll-blitted straight over.
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

    /// The `image plain` introspection contract: `clear_overlays` strips the
    /// rain like every other bling layer — both quad Vecs cleared AND the
    /// atlas Arc nulled (a dangling atlas would keep the GPU upload alive).
    /// The GLOW-HALO cursor-effect stream IS bling too, so it is stripped
    /// alongside — as are the EMBERFORGE under-glyph light (`glow_under`) and
    /// the charred glyph-ink overrides (`char_fg`).
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

    /// The damage cache compares snapshots with `PartialEq`; rain IS content,
    /// so a change in any channel must compare unequal — and the atlas is
    /// compared by SNAPSHOT IDENTITY (`Arc::as_ptr`, split-pane audit): baker
    /// versions replay across rebuilt engines with different texels, so a
    /// version key would alias stale content. Every rebake publishes a fresh
    /// `Arc` (a new publish must compare unequal), and a stable frame
    /// re-presents the SAME `Arc` (which must stay equal, or a resting rain
    /// field would defeat row-level reuse).
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

        // The contrast-halo STRENGTH is content too (it scales the stamp
        // alpha): a swelling/decaying engulfment must miss the damage gate.
        let mut fire_halo_changed = base.clone();
        fire_halo_changed.fire_halo[1].strength = 40;
        assert_ne!(
            base, fire_halo_changed,
            "fire_halo strength change must be seen"
        );

        let mut version_bumped = base.clone();
        version_bumped.rain_atlas = Some(rain_atlas(8, 0xFF));
        assert_ne!(base, version_bumped, "a fresh atlas publish must be seen");

        // A REBUILT engine deterministically replays version numbers with
        // different texels — identity (not version) must catch it.
        let mut same_version_rebake = base.clone();
        same_version_rebake.rain_atlas = Some(rain_atlas(7, 0x00));
        assert_ne!(
            base, same_version_rebake,
            "a same-version DIFFERENT-Arc publish (rebuilt engine) compares UNEQUAL"
        );

        // The stable steady state: the SAME published snapshot re-presented.
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

        // Non-empty destination: clone_from must OVERWRITE stale rain, not merge.
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
}
