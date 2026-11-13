// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cursor family's frame step.

use aterm_core::terminal::CursorStyle;
use aterm_render::{GlowQuad, RainHalo, TrailCell};
use aterm_time::Instant;

use crate::aos_momentum::MomentumHost;
use crate::cursor_beam::{BeamRodConfig, CursorBeamRod};
use crate::cursor_comet::{CometConfig, CursorComet};
use crate::cursor_droplet::{CursorDroplet, DropletConfig};
use crate::cursor_glow::{
    BlockFill, BlockFillBase, BlockFillOwner, CursorGlow, Geom, GlowConfig, GlowStyle, OverInkTier,
};
use crate::cursor_momentum::{MOMENTUM_GLOW_RADIUS_CELLS, MOMENTUM_GLOW_TAU_S, MomentumGlowConfig};
use crate::cursor_phaser::{CursorPhaser, PhaserConfig};
use crate::cursor_rainbow::{CursorRainbow, RainbowConfig};
use crate::cursor_trail::{CursorTrail, TrailConfig, TypingCadence};

#[derive(Clone, Copy)]
pub(crate) struct FloorFillMemo {
    candidate: BlockFill,
    ground: u32,
    live_cursor: u32,
    pinned_cursor: Option<u32>,
    lettered: bool,
    floored: BlockFill,
}

/// Cursor effect engines and per-frame scratch for one drawing surface.
#[derive(Default)]
pub struct CursorFx {
    /// The aurora — every style's light, the rainbow kitty ribbon included.
    pub glow: CursorGlow,
    /// The cadence comet: the directional `TrailCell` body of the `comet` style (disabled for
    /// every other style).
    pub trail: CursorTrail,
    /// Typing-cadence heat: comet ignition and the rainbow/phaser block energy.
    pub cadence: TypingCadence,
    /// The `rainbow kitty` block body.
    pub rainbow: CursorRainbow,
    /// The `water` block body.
    pub droplet: CursorDroplet,
    /// The typing-momentum glow — every style, every shape, unless the rainbow owns the caret.
    pub momentum: MomentumHost,
    /// The light rod: the bar shape of every style, and the emitter block of the styles with no
    /// bespoke body.
    pub beamrod: CursorBeamRod,
    /// The `comet` block body.
    pub comet: CursorComet,
    /// The `phaser` block body.
    pub phaser: CursorPhaser,
    /// The additive light this tick produced: the aurora first, then every body's halo in tick
    /// order.
    pub glow_scratch: Vec<GlowQuad>,
    /// Test-only: skip momentum halo warming on dark fire and light sparkle,
    /// and leave sparkle's paper caret its dark-theme colour.
    #[cfg(test)]
    pub(crate) momentum_unwarmed: bool,
    /// Test-only: the hue a dark fire's typing halo is warmed toward, in
    /// place of [`crate::cursor_momentum::MOMENTUM_FIRE_EMBER`].
    #[cfg(test)]
    pub(crate) momentum_fire_ember: Option<u32>,
    /// Test-only: leave the rainbow block's halo rings whole over an inked row next to the
    /// caret's, the negative control of [`CursorGlow::hold_off_neighbour_ink`].
    #[cfg(test)]
    pub(crate) rainbow_rim_unheld: bool,
    /// Test-only: read no colour as one the user chose, so paper concentrates a chosen colour as
    /// it does a style's own: the negative control of [`crate::color_math::light_caret_fill`].
    #[cfg(test)]
    pub(crate) chosen_unread: bool,
    /// Test-only: the typing-momentum glow reads no colour as one the user chose
    /// ([`MomentumGlowConfig::chosen_hue`], [`MomentumGlowConfig::chosen_base`]), so its paper
    /// halo wears its hue's vivid ink and its caret is charged toward its hue's pure colour: the
    /// negative control of the momentum glow's chosen-colour law alone.
    #[cfg(test)]
    pub(crate) momentum_chosen_unread: bool,
    /// A test's negative control: a chosen colour's bolt caret keeps the page floor alone, its
    /// halo unread ([`chosen_bolt_over_halo`], [`Self::set_chosen_bolt_lift`]).
    #[cfg(any(test, debug_assertions))]
    pub(crate) chosen_bolt_unlit: bool,
    /// A test's negative control: a chosen colour's bolt lift stops at its clip, short or not
    /// ([`chosen_bolt_over_halo`], [`Self::set_chosen_bolt_past_clip`]).
    #[cfg(any(test, debug_assertions))]
    pub(crate) chosen_bolt_clip_stop: bool,
    /// A test's negative control: Laser's bolt in the theme's own colour on paper keeps the page
    /// floor alone, its halo unread ([`chosen_bolt_over_halo`], [`Self::set_paper_bolt_lift`]).
    #[cfg(any(test, debug_assertions))]
    pub(crate) paper_bolt_unlit: bool,
    /// The frame's under and over streams laid out ([`CursorGlow::under_laid_into`],
    /// [`CursorGlow::out_laid_into`]) for a bolt's read of the light round it
    /// ([`chosen_bolt_over_halo`]); reused across frames.
    pub(crate) bolt_lit_scratch: [Vec<GlowQuad>; 2],
    /// The comet cells this tick produced, projected through load shedding, brightness, and
    /// legibility.
    pub trail_scratch: Vec<TrailCell>,
    /// Whether the page's text holds the GPU's bloom off this tick's frame
    /// ([`aterm_core::render::RenderInput::bloom_gate`], from the glow
    /// config's theme ink and ground).
    pub bloom_held: bool,
    /// Whether the post passes keep their source off the frame's letters
    /// ([`aterm_core::render::BloomGate::SparesLetters`],
    /// [`CursorGlow::bloom_spares_letters`]).
    pub bloom_spares_letters: bool,
    /// Memoised [`bed_legible_share`] for the last `(color, ground, ink)` triple.
    pub(crate) legible_memo: Option<((u32, u32, u32), f32)>,
    /// Memoised [`floor_block_fill`] for the last
    /// `(candidate, ground, live_cursor, pinned_cursor, lettered)`.
    pub(crate) floor_fill_memo: Option<FloorFillMemo>,
    /// The light each body engine's config last carried: the rainbow block's, the momentum
    /// glow's, the comet's and the beam rod's, for the host's dark pin (§15.0.3). Debug and test
    /// builds only.
    #[cfg(any(test, debug_assertions))]
    pub(crate) engine_lights: [crate::cursor_glow::Light; 4],
    /// Test-only: the colour the light rod last drew in, `None` while it drew nothing.
    #[cfg(test)]
    pub(crate) rod_drawn: Option<u32>,
}

/// Everything one [`CursorFx::tick`] reads besides the two folded configs.
#[derive(Clone, Copy)]
pub struct CursorFxInput {
    /// The animation clock (one `Instant` per frame).
    pub now: Instant,
    /// The caret cell the engines chase, `None` while the viewport shows history.
    pub cur: Option<(u16, u16)>,
    /// The frame presents the active grid, not retained scrollback.
    pub live_viewport: bool,
    /// The live cursor shape (a body needs a block; the rod takes a bar).
    pub cursor_style: CursorStyle,
    /// This window's blink phase (unused by the rainbow body; kept on its tick signature).
    pub blink_phase: bool,
    /// The effective live cursor colour, `0x00RRGGBB`.
    pub live_cursor: u32,
    /// Whether that colour was pinned (by config or a program's OSC 12) rather than the theme's
    /// seed.
    pub cursor_color_pinned: bool,
    /// The live default background (DECSCNM-folded), `0x00RRGGBB`: the ground every body solves
    /// its light against.
    pub default_bg: u32,
    /// The background of the cell under the caret (`0x00RRGGBB`), read from the rendered frame.
    pub cursor_cell_bg: Option<u32>,
    /// The window-space effect geometry (grid origin, frame extent).
    pub geom: Geom,
    /// The cursor-effect focus fold (OS focus, the recording pin, the typed wake): the `focused=`
    /// that `trail status` prints.
    pub focused: bool,
    /// The soft load-shed envelope, `0..=1`, applied after the tick to the comet's cell bed and
    /// the caret fill.
    pub shed_envelope: f32,
    /// The user's brightness for the comet's cell bed, `0..=1`.
    pub trail_brightness: f32,
    /// Serious Mode's `CursorBody` allowance.
    pub body_allowed: bool,
    /// The user's typing-momentum glow switch (`cursor_momentum_glow`; a page's glow switch).
    pub momentum_glow: bool,
    /// The user pinned an explicit trail colour (`cursor_trail_color`): the rod then wears it
    /// instead of the style's signature shade.
    pub user_tinted: bool,
    /// The user pinned an explicit accent (`cursor_trail_accent`): the lights that wear it draw
    /// it as itself.
    pub accent_pinned: bool,
}

/// What one [`CursorFx::tick`] produced for the host's frame.
#[derive(Clone, Copy, Debug)]
pub struct CursorFxFrame {
    /// The aurora + every body's fingerprint + the final caret fill, folded (0 when idle-empty).
    pub glow_fp: u64,
    /// The comet's presentation fingerprint (0 when idle-empty).
    pub trail_fp: u64,
    /// The comet colour the presented cells render at.
    pub trail_color: u32,
    /// The caret body this tick resolved through the fill-precedence law and the load-shed
    /// projection.
    pub block_fill: Option<BlockFill>,
    /// A bar or underline caret's colour lifted in lightness to read against the light of its
    /// cell (`CursorGlow::thin_caret_fill`), over [`Self::block_fill`]'s; `None` where it
    /// reads as it is.
    pub thin_fill: Option<u32>,
    /// The laser bolt owns the block: the caret's shape is `Bolt`.
    pub bolt_cursor: bool,
    /// A body owns a blinking block, and the shape is pinned steady: the rainbow's, and the
    /// block Laser's bolt stands down to over a letter (`CursorGlow::caret_bolt_stands_down`).
    pub twinkle_cursor: bool,
    /// The momentum glow is hot: this Blinking* style pinned to its Steady* twin (`None` when
    /// cold, or when the style does not blink).
    pub momentum_steady: Option<CursorStyle>,
}

impl CursorFxFrame {
    /// The colour the host draws the caret's body in this tick: a bar or underline caret's
    /// lifted colour ([`Self::thin_fill`]) over the block's ([`Self::block_fill`]); `None`
    /// leaves the theme's cursor colour. Every host (the pipeline's own publish, the windowed
    /// app's present paths, the Settings preview) reads the caret's colour here, so a fill one
    /// effect adds reaches all of them.
    #[inline]
    #[must_use]
    pub fn caret_fill(&self) -> Option<u32> {
        self.thin_fill.or(self.block_fill.map(|owned| owned.fill))
    }
}

/// A thin bar's insertion point, as a fraction of its cell across: where a bar caret's lights
/// sit ([`head_dx_for`]).
pub const BAR_HEAD_DX: f32 = 0.08;

/// Where the streak's head attaches in its cell: a thin bar's insertion point, else the cell
/// centre.
#[inline]
#[must_use]
pub const fn head_dx_for(style: CursorStyle) -> f32 {
    if matches!(style, CursorStyle::BlinkingBar | CursorStyle::SteadyBar) {
        BAR_HEAD_DX
    } else {
        0.5
    }
}

/// The Fire style's decorative cursor-body fill, through the same gates in every render path.
#[inline]
pub fn forge_cursor_fill(
    cursor_body_allowed: bool,
    glow_cfg: &GlowConfig,
    fill: impl FnOnce() -> Option<u32>,
) -> Option<u32> {
    if cursor_body_allowed
        && glow_cfg.enabled
        && glow_cfg.intensity > 0.0
        && matches!(glow_cfg.style, GlowStyle::Fire)
    {
        fill()
    } else {
        None
    }
}

/// The cursor-body fills one tick can produce (plus the momentum tint), in precedence order.
#[derive(Clone, Copy, Debug)]
pub struct BlockFillSplice {
    pub rainbow: Option<u32>,
    pub forge: Option<u32>,
    pub phaser: Option<u32>,
    pub bolt: Option<u32>,
    pub comet: Option<u32>,
    pub droplet: Option<u32>,
    pub beamrod: Option<u32>,
    /// The typing-momentum tint, last, so a style body always wins the caret.
    pub momentum: Option<u32>,
}

impl Default for BlockFillSplice {
    #[inline]
    fn default() -> Self {
        Self::NONE
    }
}

impl BlockFillSplice {
    /// No cursor-body fill produced.
    pub const NONE: Self = Self {
        rainbow: None,
        forge: None,
        phaser: None,
        bolt: None,
        comet: None,
        droplet: None,
        beamrod: None,
        momentum: None,
    };
}

/// The blink law, by who owns the caret.
#[inline]
#[must_use]
pub const fn compose_caret_style_override(
    bolt: bool,
    rainbow_owns_caret: bool,
    momentum_steady: Option<CursorStyle>,
) -> Option<CursorStyle> {
    if bolt {
        Some(CursorStyle::Bolt)
    } else if rainbow_owns_caret {
        Some(CursorStyle::SteadyBlock)
    } else {
        momentum_steady
    }
}

/// Whether the typing-momentum glow engine runs this frame.
#[inline]
#[must_use]
pub const fn momentum_glow_allowed(
    user_on: bool,
    host_allows: bool,
    rainbow_owns_caret: bool,
) -> bool {
    user_on && host_allows && !rainbow_owns_caret
}

/// Who owns the block cursor's body this frame and the colour they built it from.
#[inline]
#[must_use]
pub const fn resolve_block_fill(
    fills: BlockFillSplice,
    cursor_color: u32,
    trail_color: u32,
    cursor_base_pinned: bool,
) -> Option<BlockFill> {
    #[inline]
    const fn owned(
        owner: BlockFillOwner,
        fill: Option<u32>,
        cursor_color: u32,
        trail_color: u32,
        cursor_base_pinned: bool,
    ) -> Option<BlockFill> {
        let Some(fill) = fill else {
            return None;
        };
        let base = match owner.base_from() {
            BlockFillBase::CursorColor => {
                if cursor_base_pinned {
                    Some(cursor_color)
                } else {
                    None
                }
            }
            BlockFillBase::TrailColor => Some(trail_color),
            BlockFillBase::StyleIdentity | BlockFillBase::White => None,
        };
        Some(BlockFill { owner, fill, base })
    }
    if let Some(b) = owned(
        BlockFillOwner::Rainbow,
        fills.rainbow,
        cursor_color,
        trail_color,
        cursor_base_pinned,
    ) {
        return Some(b);
    }
    if let Some(b) = owned(
        BlockFillOwner::Forge,
        fills.forge,
        cursor_color,
        trail_color,
        cursor_base_pinned,
    ) {
        return Some(b);
    }
    if let Some(b) = owned(
        BlockFillOwner::Phaser,
        fills.phaser,
        cursor_color,
        trail_color,
        cursor_base_pinned,
    ) {
        return Some(b);
    }
    if let Some(b) = owned(
        BlockFillOwner::Bolt,
        fills.bolt,
        cursor_color,
        trail_color,
        cursor_base_pinned,
    ) {
        return Some(b);
    }
    if let Some(b) = owned(
        BlockFillOwner::Comet,
        fills.comet,
        cursor_color,
        trail_color,
        cursor_base_pinned,
    ) {
        return Some(b);
    }
    if let Some(b) = owned(
        BlockFillOwner::Droplet,
        fills.droplet,
        cursor_color,
        trail_color,
        cursor_base_pinned,
    ) {
        return Some(b);
    }
    if let Some(b) = owned(
        BlockFillOwner::BeamRod,
        fills.beamrod,
        cursor_color,
        trail_color,
        cursor_base_pinned,
    ) {
        return Some(b);
    }
    owned(
        BlockFillOwner::Momentum,
        fills.momentum,
        cursor_color,
        trail_color,
        cursor_base_pinned,
    )
}

/// The least WCAG contrast an effect-owned caret fill keeps against the ground under it.
pub const CARET_PAGE_MIN_CONTRAST: f32 = 4.5;

/// The margin past [`CARET_PAGE_MIN_CONTRAST`] Sparkle's gold paper caret keeps for the renderer's
/// rounding.
const SPARKLE_PAPER_CARET_GUARD: f32 = 0.15;

/// THE EMBER, THE EMITTER AND THE DROP KEEP A MARGIN OVER THE PAGE (after-land L3, L4, W8): the
/// margin past [`CARET_PAGE_MIN_CONTRAST`] Fire's forge caret and Phaser's emitter keep against
/// the page ([`caret_page_floor`]), and Water's liquid caret's band
/// (`cursor_droplet::DROPLET_CARET_FLOOR`). Held to the bar itself, each rested on it: Fire's ember
/// read 4.50 at rest on dark and Solarized Dark (`fire-dark.rest`, `fire-dark-idle10.idle10`) and
/// Phaser's emitter 4.51 on Solarized Dark (`phaser-dark.mid`), worst of four captures; the band's
/// edge laid Water's caret at 4.51 there.
pub const EFFECT_CARET_PAGE_GUARD: f32 = 0.15;

/// The least WCAG contrast the comet's cell bed alone leaves a letter it lies under: the theme's
/// ink over the bed's tint of the page.
pub const BED_INK_FLOOR: f32 = 10.0;

/// Whether `glow` is the comet style drawing its own streak (its beam on).
#[inline]
#[must_use]
pub const fn comet_tail_is_its_streak(glow: &GlowConfig) -> bool {
    glow.beam && matches!(glow.style, GlowStyle::Comet)
}

/// Share of the comet bed's opacity at which peak alpha in `color` over `ground` keeps `ink` at
/// [`BED_INK_FLOOR`] contrast.
#[inline]
#[must_use]
pub fn bed_legible_share(color: u32, ground: u32, ink: u32) -> f32 {
    if ink == aterm_core::render::COLOR_UNSET || ground == aterm_core::render::COLOR_UNSET {
        return 1.0;
    }
    let color = color & 0x00FF_FFFF;
    let ground = ground & 0x00FF_FFFF;
    if color == ground {
        return 1.0;
    }
    let ink = ink & 0x00FF_FFFF;
    let table = crate::color_math::relative_luminance_table();
    let lum_ink = crate::color_math::relative_luminance_with(table, ink);
    let lum_ink_p = lum_ink + 0.05;
    #[inline]
    const fn contrast_ink(
        table: &crate::color_math::RelativeLuminanceTable,
        lum_ink: f32,
        lum_ink_p: f32,
        bg: u32,
    ) -> f32 {
        let lb = crate::color_math::relative_luminance_with(table, bg);
        if lum_ink >= lb {
            lum_ink_p / (lb + 0.05)
        } else {
            (lb + 0.05) / lum_ink_p
        }
    }
    let target = BED_INK_FLOOR.min(contrast_ink(table, lum_ink, lum_ink_p, ground));
    #[inline]
    const fn holds_bg(
        table: &crate::color_math::RelativeLuminanceTable,
        lum_ink: f32,
        lum_ink_p: f32,
        target: f32,
        bg: u32,
    ) -> bool {
        let lb = crate::color_math::relative_luminance_with(table, bg);
        if lum_ink >= lb {
            lum_ink_p >= target * (lb + 0.05)
        } else {
            lb + 0.05 >= target * lum_ink_p
        }
    }
    let peak = crate::cursor_trail::BED_PEAK_ALPHA;
    let peak_bg = aterm_render::blend_rgb(ground, color, peak);
    if holds_bg(table, lum_ink, lum_ink_p, target, peak_bg) {
        return 1.0;
    }
    let (mut lo, mut hi) = (0u8, peak);
    let (mut lo_bg, mut hi_bg) = (ground, peak_bg);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        let mid_bg = aterm_render::blend_rgb(ground, color, mid);
        let ok = if mid_bg == hi_bg {
            false
        } else if lo > 0 && mid_bg == lo_bg {
            true
        } else {
            holds_bg(table, lum_ink, lum_ink_p, target, mid_bg)
        };
        if ok {
            lo = mid;
            lo_bg = mid_bg;
        } else {
            hi = mid;
            hi_bg = mid_bg;
        }
    }
    const INV_PEAK: f32 = 1.0 / crate::cursor_trail::BED_PEAK_ALPHA as f32;
    ((lo as f32 - 0.5).max(0.0) * INV_PEAK).clamp(0.0, 1.0)
}

/// Floor an effect-owned caret fill against the page `ground` ([`caret_page_floor`]), by lightness
/// alone ([`crate::color_math::floor_contrast_keeping_hue`]). Over a `lettered` cell, which a block
/// caret draws cut out of its fill in that ground, the floor is the letter's
/// ([`crate::cursor_glow::caret_letter_min_contrast`]); the momentum tint's stays no higher than
/// the theme's own caret.
#[inline]
#[must_use]
pub const fn floor_block_fill(
    candidate: BlockFill,
    ground: u32,
    live_cursor: u32,
    pinned_cursor: Option<u32>,
    lettered: bool,
) -> BlockFill {
    if let (Some(pinned), Some(base)) = (pinned_cursor, candidate.base)
        && (base & 0x00FF_FFFF) == (pinned & 0x00FF_FFFF)
    {
        return candidate;
    }
    let ground = ground & 0x00FF_FFFF;
    let table = crate::color_math::relative_luminance_table();
    let lg = crate::color_math::relative_luminance_with(table, ground);
    let letter = if lettered {
        crate::cursor_glow::caret_letter_min_contrast()
    } else {
        0.0
    };
    let min = match candidate.owner {
        BlockFillOwner::Momentum => CARET_PAGE_MIN_CONTRAST.max(letter).min(
            crate::color_math::contrast_ratio_from_luminance(
                crate::color_math::relative_luminance_with(table, live_cursor & 0x00FF_FFFF),
                lg,
            ),
        ),
        owner => caret_page_floor(owner).max(letter),
    };
    BlockFill {
        fill: crate::color_math::floor_contrast_keeping_hue_with_ground_lum(
            table,
            candidate.fill,
            lg,
            min,
        ),
        ..candidate
    }
}

/// The page floor [`floor_block_fill`] holds `owner`'s caret to: [`CARET_PAGE_MIN_CONTRAST`], the
/// rainbow kitty's paper caret's too; Fire's ember and Phaser's emitter keep
/// [`EFFECT_CARET_PAGE_GUARD`] past it.
#[inline]
#[must_use]
pub const fn caret_page_floor(owner: BlockFillOwner) -> f32 {
    match owner {
        BlockFillOwner::Rainbow => crate::rainbow_kitty::ribbon::PAPER_CARET_CONTRAST,
        BlockFillOwner::Forge | BlockFillOwner::Phaser => {
            CARET_PAGE_MIN_CONTRAST + EFFECT_CARET_PAGE_GUARD
        }
        _ => CARET_PAGE_MIN_CONTRAST,
    }
}

#[inline]
#[must_use]
const fn bolt_body(color: u32, blaze: f32, dark: bool, chosen: bool) -> u32 {
    let blaze = blaze.clamp(0.0, 1.0);
    if !dark {
        return crate::color_math::light_caret_fill(color, blaze, chosen);
    }
    crate::effect_util::lerp_rgb(color, 0x00FF_FFFF, 0.55 * blaze)
}

/// The least a chosen colour's bolt caret reads against the halo it sits in
/// ([`chosen_bolt_over_halo`]): the legibility bar and a guard for the post passes (the bloom and
/// the crown light the halo round the bolt past what its halos lay).
pub(crate) const CHOSEN_BOLT_HALO_MIN: f32 = CARET_PAGE_MIN_CONTRAST + 0.6;

/// How far past its clip a chosen colour's bolt may go toward white (dark) or black (paper) to
/// read [`CHOSEN_BOLT_HALO_MIN`] against its halo ([`chosen_bolt_over_halo`]): half the way, so
/// it is the pin lit, never white.
const CHOSEN_BOLT_PAST_CLIP: f32 = 0.5;

/// THE CHOSEN BOLT READS AGAINST ITS OWN HALO (composed r0's judges; O1(a)): Laser's bolt caret in
/// a colour the user chose sits in that colour's aura, and the page floor alone left it dim
/// against it: under a slate pin (`#20242c`) a grey `#808287` bolt read 4.96:1 on the page but
/// 2.60:1 against its halo `#414248`, and the pink pin's 3.93. Its `fill` is moved in lightness
/// alone (each channel by one amount, toward white on a dark page, toward black on paper, never
/// past the point where a channel meets its end, so its hue and chroma stay the colour's: a
/// pinned `#50fa7b` in a bright halo stays green at its lightest rather than going white; where
/// that end leaves it short, it goes on toward white or black, which keeps its hue, no further
/// than the bar needs and [`CHOSEN_BOLT_PAST_CLIP`] allows: the pink pin's `#ff76c1` read 4.41:1
/// against its halo with the post passes on, composed r2) until it reads
/// [`CHOSEN_BOLT_HALO_MIN`] against the brightest (dark) or darkest (paper) of `page` lit by the
/// frame's light (its under stream's quads, then its over stream's and its halos, [`light_at`]:
/// read over the over stream alone, a paper halo under the letters went unseen and Laser's
/// paper bolt read 3.72:1 against it, composed r2) at every other pixel of the
/// caret's `cell` (the cell's corners and centre alone missed the light beside the bolt and
/// left a keystroke's frame at 4.17): light is not colour (the pin ruling). Control: [`CursorFx::chosen_bolt_unlit`].
#[must_use]
pub(crate) fn chosen_bolt_over_halo(
    fill: u32,
    (under, over, halos): (&[GlowQuad], &[GlowQuad], &[RainHalo]),
    page: u32,
    geom: Geom,
    (row, col): (u16, u16),
    past_clip: bool,
) -> u32 {
    use crate::color_math::contrast_ratio;
    let fill = fill & 0x00FF_FFFF;
    let page = page & 0x00FF_FFFF;
    let dark = aterm_render::theme_is_dark(page);
    let (cw, ch) = (geom.cw as i32, geom.ch as i32);
    let x0 = i32::from(geom.origin_x) + i32::from(col) * cw;
    let y0 = i32::from(geom.origin_y) + i32::from(row) * ch;
    let mut ground = page;
    let mut worst = f32::INFINITY;
    let grid = (1..ch - 1)
        .step_by(2)
        .flat_map(|fy| (1..cw - 1).step_by(2).map(move |fx| (fx, fy)));
    for (fx, fy) in grid {
        let at = (x0 + fx, y0 + fy);
        let lit = light_at(light_at(page, (under, &[]), at), (over, halos), at);
        let c = contrast_ratio(fill, lit);
        if c < worst {
            (worst, ground) = (c, lit);
        }
    }
    if worst >= CHOSEN_BOLT_HALO_MIN {
        return fill;
    }
    let shift = |d: f32| -> u32 {
        let ch = |sh: u32| {
            let c = ((fill >> sh) & 0xff) as f32;
            ((if dark { c + d } else { c - d }).round().clamp(0.0, 255.0) as u32) << sh
        };
        ch(16) | ch(8) | ch(0)
    };
    let meets = |c: u32| contrast_ratio(c, ground) >= CHOSEN_BOLT_HALO_MIN;
    let channels = [(fill >> 16) & 0xff, (fill >> 8) & 0xff, fill & 0xff];
    let room = if dark {
        255 - channels.iter().copied().max().unwrap_or(0)
    } else {
        channels.iter().copied().min().unwrap_or(0)
    } as f32;
    if !meets(shift(room)) && !past_clip {
        return shift(room);
    }
    if !meets(shift(room)) {
        // At its clip and still short (a bright halo, its post passes on): on toward white on a
        // dark page, toward black on paper, which keeps its hue, its chroma given only as far as
        // the bar needs and never more than [`CHOSEN_BOLT_PAST_CLIP`] of the way (light added to
        // the pin, as the blaze adds it; a pinned `#50fa7b` stays green).
        let clipped = shift(room);
        let end = if dark { 0x00FF_FFFF } else { 0 };
        let past = |m: f32| crate::effect_util::lerp_rgb(clipped, end, m);
        if !meets(past(CHOSEN_BOLT_PAST_CLIP)) {
            return past(CHOSEN_BOLT_PAST_CLIP);
        }
        let (mut lo, mut hi) = (0.0f32, CHOSEN_BOLT_PAST_CLIP);
        while hi - lo > 1.0 / 256.0 {
            let mid = 0.5 * (lo + hi);
            if meets(past(mid)) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        return past(hi);
    }
    let (mut lo, mut hi) = (0.0f32, room);
    while hi - lo > 0.5 {
        let mid = 0.5 * (lo + hi);
        if meets(shift(mid)) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    shift(hi)
}

/// `page` as the renderer lays the frame's light over it at window pixel `(x, y)`: each laid
/// quad that covers it, by the renderer's own rule (`over_premul`, which adds at alpha 0, of
/// the quad's colour and opacity where `x` falls across it), then every adding halo (its light
/// added by its falloff) and every veiling one (laid over at its falloff, no more than its
/// cap), as the renderer's `draw_radial_add` lays them. (A veil once read at its falloff times
/// its cap, a halo at a time in the stream's order, read a paper Return flash in the caret's
/// cell `#b8d4f2` where the renderer laid `#93c4ee`, composed r2.)
#[must_use]
pub(crate) fn light_at(
    page: u32,
    (quads, halos): (&[GlowQuad], &[RainHalo]),
    (x, y): (i32, i32),
) -> u32 {
    let mut px = page & 0x00FF_FFFF;
    for q in quads {
        let (qx, qy, qw) = (i32::from(q.x), i32::from(q.y), i32::from(q.w.max(1)));
        if x < qx || x >= qx + qw || y < qy || y >= qy + i32::from(q.h) {
            continue;
        }
        let (i, n) = ((x - qx) as u32, qw as u32);
        let color = aterm_render::glow_lerp_rgb(q.color, q.color2, i, n);
        let alpha = aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, n) as u8;
        px = aterm_render::over_premul(px, color, alpha) & 0x00FF_FFFF;
    }
    // The renderer lays every adding halo, then every veiling one (`draw_radial_add`).
    for mode in [
        aterm_core::render::HaloMode::Add,
        aterm_core::render::HaloMode::Over,
    ] {
        for h in halos.iter().filter(|h| h.mode == mode) {
            let (hx, hy) = (i32::from(h.x), i32::from(h.y));
            if x < hx || x >= hx + i32::from(h.w) || y < hy || y >= hy + i32::from(h.h) {
                continue;
            }
            let (rx, ry) = (i32::from(h.rx), i32::from(h.ry));
            let ny = aterm_render::halo_row_ny(y - i32::from(h.cy), ry * ry);
            if ny >= 256 {
                continue;
            }
            let w = aterm_render::halo_weight(x - i32::from(h.cx), ny, rx * rx);
            if w <= 0 {
                continue;
            }
            let w = w.min(255) as u8;
            px = match mode {
                aterm_core::render::HaloMode::Add => {
                    aterm_render::add_sat(px, aterm_render::premul_rgb(h.color, w))
                }
                aterm_core::render::HaloMode::Over => {
                    aterm_render::over_rgb(px, h.color, w.min(aterm_render::halo_over_cap(h.color)))
                }
            } & 0x00FF_FFFF;
        }
    }
    px
}

/// Project the resolved effect-owned cursor body through adaptive shedding.
#[inline]
#[must_use]
pub fn project_block_fill(
    candidate: Option<BlockFill>,
    terminal_cursor_color: u32,
    envelope: f32,
) -> Option<BlockFill> {
    let envelope = if envelope.is_finite() {
        envelope.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if envelope <= 0.0 {
        return None;
    }
    let mut owned = candidate?;
    owned.fill = aterm_render::blend_rgb(
        terminal_cursor_color,
        owned.fill,
        (envelope * (u8::MAX as f32)).round() as u8,
    );
    Some(owned)
}

/// What a host resolved from a trail-style spelling.
#[derive(Clone, Copy, Debug)]
pub struct GlowSpelling {
    /// The style, or `None` for the resolution that genuinely means "draw nothing" (`off`).
    pub style: Option<GlowStyle>,
    /// The additive beam ([`crate::cursor_glow::style_has_beam_of`]).
    pub beam: bool,
    /// The full-height ribbon body: `false` for the `rainbow` theme, whose shape is the
    /// underline, and unread by every other style.
    pub ribbon_tall: bool,
    /// The flat ribbon body, which no built-in theme picks.
    pub ribbon_flat: bool,
    /// The classic wake's theme-following face (`classic mono`).
    pub classic_mono: bool,
}

impl GlowSpelling {
    /// A page's reading of a raw spelling.
    #[inline]
    #[must_use]
    pub const fn parse(raw: &str) -> Self {
        let token = match crate::cursor_glow::spellings::canonical(raw) {
            Some(c) => c,
            None => raw,
        };
        let style = GlowStyle::parse(token);
        Self {
            style: if matches!(token.as_bytes(), b"off") {
                None
            } else {
                Some(style)
            },
            beam: crate::cursor_glow::style_has_beam_of(style, token),
            ribbon_tall: !matches!(style, GlowStyle::RainbowKitty),
            ribbon_flat: false,
            classic_mono: GlowStyle::style_names_classic_mono(raw),
        }
    }
}

/// The numeric knobs of the cursor family's config.
#[derive(Clone, Copy, Debug)]
pub struct GlowKnobs {
    /// Whether the trail may draw at all (Serious Mode folded in by the host; the style decides
    /// the rest).
    pub enabled: bool,
    /// An explicit trail colour; `None` takes the style's default.
    pub color: Option<u32>,
    /// An explicit accent; `None` brightens the colour.
    pub accent: Option<u32>,
    pub duration_ms: u64,
    pub length: usize,
    pub intensity: f32,
    pub radius: f32,
    pub ring: bool,
}

/// Classic's brightness while its user sets none.
pub const CLASSIC_DEFAULT_INTENSITY: f32 = 0.7;

/// The trail brightness `style` draws at while `cursor_trail_intensity` is unset, in every host.
#[inline]
#[must_use]
pub const fn default_trail_intensity(style: Option<GlowStyle>, classic_mono: bool) -> f32 {
    match style {
        Some(GlowStyle::Classic) if !classic_mono => CLASSIC_DEFAULT_INTENSITY,
        _ => 1.0,
    }
}

#[inline]
const fn finite_clamp_or_off(value: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        0.0
    }
}

/// Brighten a packed `0x00RRGGBB` colour by `factor`.
#[inline]
#[must_use]
pub const fn brighten(color: u32, factor: f32) -> u32 {
    #[inline]
    const fn channel(color: u32, shift: u32, factor: f32) -> u32 {
        ((((color >> shift) & 0xff) as f32) * factor).min(255.0) as u32
    }
    (channel(color, 16, factor) << 16)
        | (channel(color, 8, factor) << 8)
        | channel(color, 0, factor)
}

/// The style's own hue, `0x00RRGGBB`, or `None` for the styles whose automatic colour is the
/// cursor's.
#[inline]
#[must_use]
pub const fn canonical_glow_color(style: GlowStyle) -> Option<u32> {
    use crate::cursor_glow::{
        BEAM_DEFAULT_COLOR, COMET_DEFAULT_COLOR, LASER_DEFAULT_COLOR, SPARKLE_DEFAULT_COLOR,
    };
    match style {
        GlowStyle::Laser => Some(LASER_DEFAULT_COLOR),
        GlowStyle::Beam => Some(BEAM_DEFAULT_COLOR),
        GlowStyle::Comet => Some(COMET_DEFAULT_COLOR),
        GlowStyle::Sparkle => Some(SPARKLE_DEFAULT_COLOR),
        _ => None,
    }
}

/// Whether an automatic glow colour (no explicit `cursor_trail_color`) takes the live cursor
/// colour this frame.
#[inline]
#[must_use]
pub const fn glow_color_follows_live_cursor(style: GlowStyle, cursor_color_pinned: bool) -> bool {
    match style {
        GlowStyle::Laser => false,
        _ => canonical_glow_color(style).is_none() || cursor_color_pinned,
    }
}

/// The automatic glow colour for a frame, `0x00RRGGBB`.
#[inline]
#[must_use]
pub const fn automatic_glow_color(
    style: GlowStyle,
    live_cursor: u32,
    cursor_color_pinned: bool,
) -> u32 {
    let color = match canonical_glow_color(style) {
        Some(hue) if !glow_color_follows_live_cursor(style, cursor_color_pinned) => hue,
        _ => live_cursor,
    };
    color & 0x00ff_ffff
}

/// The cursor family's config law.
#[inline]
#[must_use]
pub const fn resolve_glow_config(
    knobs: GlowKnobs,
    spelling: GlowSpelling,
    theme_cursor: u32,
    dark_theme: bool,
    theme_fg: u32,
    theme_bg: u32,
    head_dx: f32,
) -> GlowConfig {
    use crate::cursor_glow::{KnobContext, TrailKnob};
    let glow_style = match spelling.style {
        Some(style) => style,
        None => GlowStyle::Lumen,
    };
    let default_color = automatic_glow_color(glow_style, theme_cursor, false);
    let color = (match knobs.color {
        Some(c) => c,
        None => default_color,
    }) & 0x00ff_ffff;
    let accent_base = if glow_style.draws_trail_color() {
        color
    } else {
        default_color & 0x00ff_ffff
    };
    let accent = match knobs.accent {
        Some(value) => value & 0x00ff_ffff,
        None => brighten(accent_base, 1.5),
    };
    let widest = KnobContext::widest(false);
    let duration_ms = if knobs.duration_ms < 30 {
        30
    } else if knobs.duration_ms > 2_000 {
        2_000
    } else {
        knobs.duration_ms
    };
    let length = if knobs.length < 1 {
        1
    } else if knobs.length > 512 {
        512
    } else {
        knobs.length
    };
    GlowConfig {
        theme_fg,
        theme_bg,
        enabled: knobs.enabled && spelling.style.is_some(),
        style: glow_style,
        color,
        accent,
        duration: std::time::Duration::from_millis(duration_ms),
        length,
        intensity: finite_clamp_or_off(knobs.intensity, 0.0, 1.0),
        audible: false,
        radius: if glow_style.reads_knob(TrailKnob::Radius, widest) {
            finite_clamp_or_off(knobs.radius, 0.0, 2.0)
        } else {
            0.0
        },
        ring: glow_style.reads_knob(TrailKnob::Ring, widest) && knobs.ring,
        dark_theme,
        beam: spelling.beam,
        head_dx,
        ribbon_tall: spelling.ribbon_tall,
        ribbon_flat: spelling.ribbon_flat,
        classic_mono: spelling.classic_mono
            || (matches!(glow_style, GlowStyle::Classic) && knobs.color.is_some()),
        light: crate::cursor_glow::Light::OFF,
    }
}

/// The plain name Settings gives a style's own hue ([`canonical_glow_color`]).
#[inline]
#[must_use]
pub const fn canonical_glow_color_name(style: GlowStyle) -> Option<&'static str> {
    match style {
        GlowStyle::Laser => Some("Violet"),
        GlowStyle::Beam => Some("Ice blue"),
        GlowStyle::Comet => Some("Pale blue"),
        GlowStyle::Sparkle => Some("Gold"),
        GlowStyle::Lumen
        | GlowStyle::Phaser
        | GlowStyle::RainbowKitty
        | GlowStyle::Fire
        | GlowStyle::Water
        | GlowStyle::Classic => None,
    }
}

/// The colour `style` draws when `cursor_trail_color` is unset and it does not follow the cursor,
/// with its plain name.
#[inline]
#[must_use]
pub const fn own_trail_color(
    style: GlowStyle,
    cursor_color_pinned: bool,
) -> Option<(u32, &'static str)> {
    if glow_color_follows_live_cursor(style, cursor_color_pinned) {
        return None;
    }
    let Some(color) = canonical_glow_color(style) else {
        return None;
    };
    let Some(name) = canonical_glow_color_name(style) else {
        return None;
    };
    Some((color, name))
}

/// The rod a bar cursor wears for a style that colours a block from its own palette
/// ([`GlowStyle::draws_trail_color`] false), while no trail colour tints it: its colour, its haze
/// and its plain name. Phaser's rod shifts through the spectrum instead, and every other style's
/// rod wears its trail colour.
#[inline]
#[must_use]
pub const fn own_rod(style: GlowStyle) -> Option<(u32, u32, &'static str)> {
    match style {
        GlowStyle::Water => Some((0x0032_DCDE, 0x000E_66B4, "Aqua")),
        GlowStyle::Fire => Some((0x00FF_9632, 0x0078_1E00, "Amber")),
        GlowStyle::RainbowKitty => Some((0x00FF_66CC, 0x0046_1E64, "Pink")),
        GlowStyle::Lumen
        | GlowStyle::Phaser
        | GlowStyle::Sparkle
        | GlowStyle::Laser
        | GlowStyle::Beam
        | GlowStyle::Comet
        | GlowStyle::Classic => None,
    }
}

/// What an unset `cursor_trail_color` draws where the colour applies ([`unset_trail_color`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsetTrailColor {
    /// The live cursor's colour.
    Cursor,
    /// A colour of the style's own, `0x00RRGGBB`, with its plain name.
    Own(u32, &'static str),
    /// Colours that shift as it draws: Classic's trail, Phaser's rod.
    Shifting,
}

impl UnsetTrailColor {
    /// Settings' name for what an unset trail colour draws ("Shifting colors", a hue's name),
    /// `None` where it is the live cursor's colour.
    #[inline]
    #[must_use]
    pub const fn name(self) -> Option<&'static str> {
        match self {
            Self::Cursor => None,
            Self::Own(_, name) => Some(name),
            Self::Shifting => Some("Shifting colors"),
        }
    }
}

/// What an unset `cursor_trail_color` draws for `style` where the colour applies: the trail, for
/// a style that draws the trail colour on a block, else the bar cursor's rod, the one thing the
/// colour tints there (the frame's rod reads [`own_rod`]). A style's own hue gives way to a
/// pinned cursor colour as [`own_trail_color`] says; a rod's never does.
#[inline]
#[must_use]
pub const fn unset_trail_color(style: GlowStyle, cursor_color_pinned: bool) -> UnsetTrailColor {
    if matches!(style, GlowStyle::Classic) {
        return UnsetTrailColor::Shifting;
    }
    if !style.draws_trail_color() {
        return match own_rod(style) {
            Some((color, _, name)) => UnsetTrailColor::Own(color, name),
            None => UnsetTrailColor::Shifting,
        };
    }
    match own_trail_color(style, cursor_color_pinned) {
        Some((color, name)) => UnsetTrailColor::Own(color, name),
        None => UnsetTrailColor::Cursor,
    }
}

/// What an unset `cursor_trail_color` draws for `style`, by name, where that is not the live
/// cursor's colour ([`unset_trail_color`]).
#[inline]
#[must_use]
pub const fn trail_color_default_name(
    style: GlowStyle,
    cursor_color_pinned: bool,
) -> Option<&'static str> {
    unset_trail_color(style, cursor_color_pinned).name()
}

/// The colour an unset accent (native `cursor_trail_accent`) brightens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccentBase {
    Cursor,
    Own(u32, &'static str),
    Trail(u32),
}

/// What an unset accent brightens for `style` under `color` and `cursor_color_pinned`.
#[inline]
#[must_use]
pub const fn unset_accent_base(
    style: GlowStyle,
    color: TrailColor,
    cursor_color_pinned: bool,
) -> AccentBase {
    match color {
        TrailColor::Pinned(chosen) if style.draws_trail_color() => {
            AccentBase::Trail(chosen & 0x00FF_FFFF)
        }
        TrailColor::Cursor if style.draws_trail_color() => AccentBase::Cursor,
        TrailColor::Automatic | TrailColor::Pinned(_) | TrailColor::Cursor => {
            match own_trail_color(style, cursor_color_pinned) {
                Some((own, name)) => AccentBase::Own(own, name),
                None => AccentBase::Cursor,
            }
        }
    }
}

/// What a host's trail colour setting (native `cursor_trail_color`) says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrailColor {
    Automatic,
    Cursor,
    Pinned(u32),
}

impl TrailColor {
    /// The word for the cursor's colour in a trail colour setting.
    pub const CURSOR_WORD: &'static str = "cursor";

    /// A trail colour setting as written (native `cursor_trail_color`, a page's
    /// `set_cursor_glow_color`).
    #[inline]
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            Some(Self::Automatic)
        } else if raw.eq_ignore_ascii_case(Self::CURSOR_WORD) {
            Some(Self::Cursor)
        } else {
            aterm_types::scheme::parse_hex(raw).map(|color| {
                Self::Pinned(
                    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b),
                )
            })
        }
    }

    /// An explicit colour where one is given, else [`Self::Automatic`]: a colour knob that cannot
    /// say "the cursor's".
    #[inline]
    #[must_use]
    pub const fn automatic_or_pinned(color: Option<u32>) -> Self {
        match color {
            Some(color) => Self::Pinned(color & 0x00FF_FFFF),
            None => Self::Automatic,
        }
    }

    /// The colour [`GlowKnobs::color`] takes for this setting against `cursor`.
    #[inline]
    #[must_use]
    pub const fn knob(self, cursor: u32) -> Option<u32> {
        match self {
            Self::Automatic => None,
            Self::Cursor => Some(cursor & 0x00FF_FFFF),
            Self::Pinned(color) => Some(color & 0x00FF_FFFF),
        }
    }

    /// The same setting with a pinned colour masked to `0x00RRGGBB`.
    #[inline]
    #[must_use]
    pub const fn masked(self) -> Self {
        match self {
            Self::Pinned(color) => Self::Pinned(color & 0x00FF_FFFF),
            other => other,
        }
    }

    /// Whether a colour was chosen at all: the rod wears it ([`CursorFxInput::user_tinted`]) and
    /// Classic draws its one-colour face.
    #[inline]
    #[must_use]
    pub const fn chosen(self) -> bool {
        !matches!(self, Self::Automatic)
    }
}

/// Update `glow` and `trail` colours from the live cursor and `color` setting.
#[inline]
pub const fn follow_live_cursor(
    glow: &mut GlowConfig,
    trail: &mut TrailConfig,
    live_cursor: u32,
    cursor_color_pinned: bool,
    color: TrailColor,
    explicit_accent: bool,
    body_color: Option<u32>,
) {
    let auto = automatic_glow_color(glow.style, live_cursor, cursor_color_pinned);
    let drawn = match color {
        TrailColor::Automatic => Some(auto),
        TrailColor::Cursor => Some(live_cursor & 0x00FF_FFFF),
        TrailColor::Pinned(_) => None,
    };
    if let Some(drawn) = drawn {
        glow.color = drawn;
    }
    trail.color = match body_color {
        Some(body) => body & 0x00FF_FFFF,
        None => glow.color,
    };
    if explicit_accent {
        return;
    }
    if !color.chosen() || !glow.style.draws_trail_color() {
        glow.accent = brighten(auto, 1.5);
    } else if let Some(drawn) = drawn {
        glow.accent = brighten(drawn, 1.5);
    }
}

struct OverInkShares(u64);

impl Default for OverInkShares {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl OverInkShares {
    const EMPTY: Self = Self(0);

    #[inline]
    const fn fold(&mut self, share: f32) {
        if share < 1.0 {
            self.0 = aterm_hash::fnv_mix_u64(self.0 | 1, share.to_bits() as u64);
        }
    }
}

#[inline]
const fn is_block(style: CursorStyle) -> bool {
    matches!(style, CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock)
}

/// A thin caret's shape: `Some(true)` for a bar, `Some(false)` for an underline, `None` for
/// every other.
#[inline]
const fn thin_bar(style: CursorStyle) -> Option<bool> {
    match style {
        CursorStyle::BlinkingBar | CursorStyle::SteadyBar => Some(true),
        CursorStyle::BlinkingUnderline | CursorStyle::SteadyUnderline => Some(false),
        _ => None,
    }
}

impl CursorFx {
    /// A test's negative control: `on` (as shipped), a chosen colour's bolt caret is lifted to
    /// read against its own halo ([`chosen_bolt_over_halo`]); off, it keeps the page floor alone.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_chosen_bolt_lift(&mut self, on: bool) {
        self.chosen_bolt_unlit = !on;
    }

    /// A test's negative control: `on` (as shipped), Laser's bolt in the theme's own colour on
    /// paper is lifted to read against its own halo as a chosen colour's is
    /// ([`chosen_bolt_over_halo`]); off, it keeps the page floor alone.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_paper_bolt_lift(&mut self, on: bool) {
        self.paper_bolt_unlit = !on;
    }

    /// A test's negative control: `on` (as shipped), a chosen colour's bolt still short of its
    /// bar at its clip goes on toward white or black ([`chosen_bolt_over_halo`]); off, it stops
    /// at the clip.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_chosen_bolt_past_clip(&mut self, on: bool) {
        self.chosen_bolt_clip_stop = !on;
    }

    /// The light each body engine's config last carried (the rainbow block's, the momentum
    /// glow's, the comet's and the beam rod's): for the host's dark pin (§15.0.3). Debug and test
    /// builds only.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn engine_lights(&self) -> [crate::cursor_glow::Light; 4] {
        self.engine_lights
    }

    /// A test's read of this tick's whole additive stream: [`Self::glow_scratch`] with its ramps ([`CursorGlow::out_laid`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub fn glow_laid(&self) -> Vec<GlowQuad> {
        self.glow.out_laid(&self.glow_scratch)
    }

    /// The per-frame preamble, before the host feeds the glow engine its row probe, print anchor
    /// and delivered inserts.
    #[inline]
    pub fn begin(&mut self, live_viewport: bool, animate: bool) {
        if !live_viewport {
            if !self.glow.hide_v2() {
                self.glow.reset();
            }
            self.trail.reset();
        }
        self.glow.set_reduced_motion(!animate);
    }

    /// Retire every body engine, the comet, the momentum rate and the typing cadence.
    #[inline]
    pub fn retire_bodies(&mut self) {
        self.retire_body_geometry();
        self.momentum.reset();
        self.cadence = TypingCadence::DEFAULT;
    }

    /// The caret's coordinate space was replaced (re-grid, new owner, alternate screen swap, torn
    /// frame).
    #[inline]
    pub fn retire_body_geometry(&mut self) {
        self.trail.reset();
        self.rainbow.reset();
        self.droplet.reset();
        self.beamrod.reset();
        self.comet.reset();
        self.phaser.reset();
        self.glow_scratch.clear();
        self.glow.clear_out_ramps();
        self.trail_scratch.clear();
    }

    /// Content-scroll law, whole-plane arm.
    #[inline]
    pub fn scroll_translate(&mut self, rows: u16) {
        self.glow.note_scroll(rows);
        self.trail.note_scroll(rows);
        self.glow.drop_row_probe();
    }

    /// Content-scroll law, row-band arm.
    #[inline]
    pub fn scroll_bands(
        &mut self,
        current: &aterm_core::terminal::ContentScrollState,
        first_seq: u64,
        count: u8,
    ) {
        let n = u64::from(count);
        let mut i = 0;
        while i < n {
            let m = current.band(first_seq + i);
            self.glow.note_band_move(m.top, m.bottom, m.delta);
            self.trail.note_band_move(m.top, m.bottom, m.delta);
            i += 1;
        }
        self.glow.drop_row_probe();
    }

    /// The primary screen was cleared (`clear`, Ctrl-L): every light the
    /// trail laid on the old screen's letters goes in the frame they go
    /// ([`CursorGlow::note_screen_cleared`]); the caret's momentum glow, the
    /// caret's own, stays.
    #[inline]
    pub fn screen_cleared(&mut self) {
        self.glow.note_screen_cleared();
        self.trail.reset();
    }

    /// Content-scroll law, invalidation arm.
    #[inline]
    pub fn scroll_invalidate(&mut self, now: Instant) {
        self.glow.curtain(now);
        self.trail.reset();
    }

    /// Any body (not the aurora, not the cadence comet) still animating: the part of the
    /// scheduler's frame-cadence set this step owns.
    #[inline]
    #[must_use]
    pub fn bodies_active(&self) -> bool {
        self.rainbow.is_active()
            || self.droplet.is_active()
            || self.momentum.is_active()
            || self.beamrod.is_active()
            || self.comet.is_active()
            || self.phaser.is_active()
    }

    /// A body that still owes frames but not the frame cadence, once its fingerprint stops moving
    /// frame to frame.
    #[inline]
    #[must_use]
    pub fn bodies_settling(&self) -> bool {
        self.rainbow.settling() || self.momentum.settling()
    }

    /// The earliest next visible step of the settling bodies, or `None` when none is settling.
    #[inline]
    #[must_use]
    pub fn bodies_next_change_deadline(
        &mut self,
        now: Instant,
        frame_interval: std::time::Duration,
    ) -> Option<Instant> {
        let rainbow = if self.rainbow.settling() {
            self.rainbow.next_change_deadline(now, frame_interval)
        } else {
            None
        };
        let momentum = if self.momentum.settling() {
            self.momentum.next_change_deadline(now, frame_interval)
        } else {
            None
        };
        match (rainbow, momentum) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, b) => b,
        }
    }

    /// The frame's [`aterm_core::render::RenderInput::bloom_gate`]: held where the
    /// page's text holds the bloom, else sparing the letters where the style's
    /// light is guarded.
    #[inline]
    #[must_use]
    pub const fn bloom_gate(&self) -> aterm_core::render::BloomGate {
        aterm_core::render::BloomGate::sparing(self.bloom_held, self.bloom_spares_letters)
    }

    /// The frame step: the aurora, then every block body off the aurora's post-tick state, then
    /// the comet, then the caret's one owner.
    ///
    /// `#[inline]` on wasm32 alone, as is [`CursorGlow::tick`] under it. The page reaches this
    /// step only through `#[inline]` `EffectsPipeline::apply` and the generic
    /// `step_cursor_family`, which both compile in the binding crate, so there the whole step
    /// compiles in that crate, which the release profile builds at opt-level `z`, not in this
    /// crate at 3: 560 KB off each web module (decision 5 of
    /// `docs/DESIGN-host-boundary-2026-08-30.md`). Natively neither stepper is `#[inline]`, so a
    /// caller's test build carries no unoptimized copy of either.
    #[cfg_attr(target_arch = "wasm32", inline)]
    pub fn tick(
        &mut self,
        f: &CursorFxInput,
        glow_cfg: &GlowConfig,
        mut trail_cfg: TrailConfig,
    ) -> CursorFxFrame {
        let now = f.now;
        let cur = f.cur;
        let geom = f.geom;
        let body_amp = finite_clamp_or_off(glow_cfg.intensity, 0.0, 1.0);
        let bed_brightness = finite_clamp_or_off(f.trail_brightness, 0.0, 1.0);
        trail_cfg.enabled &= bed_brightness > 0.0;
        let dark = aterm_render::theme_is_dark(f.default_bg);
        self.bloom_held = self.glow.holds_bloom(glow_cfg.theme_fg, glow_cfg.theme_bg);
        self.glow.set_colour_pins(crate::cursor_glow::ColourPins {
            trail: f.user_tinted,
            accent: f.accent_pinned,
        });
        self.glow
            .set_thin_caret(f.focused && thin_bar(f.cursor_style).is_some());
        let glow_fp = self
            .glow
            .tick(cur, now, glow_cfg, geom, &mut self.glow_scratch);

        // A glow that is off draws no guarded light, so the frame keeps main's gate.
        self.bloom_spares_letters =
            glow_cfg.enabled && self.glow.bloom_spares_letters(glow_cfg.style);
        let body_allowed = f.body_allowed && f.live_viewport && cur.is_some();
        let forge_fill = forge_cursor_fill(body_allowed, glow_cfg, || self.glow.forge_fill());
        let rainbow_block = body_allowed
            && glow_cfg.enabled
            && glow_cfg.intensity > 0.0
            && f.focused
            && is_block(f.cursor_style);
        let pinned_base = if f.cursor_color_pinned {
            Some(f.live_cursor)
        } else {
            None
        };
        let rainbow_enabled = matches!(glow_cfg.style, GlowStyle::RainbowKitty) && rainbow_block;
        let rainbow_cfg = RainbowConfig {
            enabled: rainbow_enabled,
            intensity: body_amp,
            blinking: matches!(f.cursor_style, CursorStyle::BlinkingBlock),
            base: pinned_base,
            head_rgb: if rainbow_enabled {
                self.glow.rainbow_head_rgb(glow_cfg)
            } else {
                None
            },
            paint: if rainbow_enabled {
                Some(self.glow.caret_paint(now))
            } else {
                None
            },
            ground: Some(f.default_bg & 0x00FF_FFFF),
            flare_at: if rainbow_enabled {
                self.glow.caret_flare_at()
            } else {
                None
            },
            light: glow_cfg.light,
        };
        #[cfg(any(test, debug_assertions))]
        {
            self.engine_lights[0] = rainbow_cfg.light;
        }
        let cadence_heard = trail_cfg.enabled
            || (rainbow_block
                && rainbow_cfg.intensity > 0.0
                && matches!(glow_cfg.style, GlowStyle::RainbowKitty | GlowStyle::Phaser));
        let (energy, warmth) = if cadence_heard {
            self.cadence.sample(now)
        } else {
            (0.0, 0.0)
        };
        let (family_phase, family_field) = if rainbow_enabled {
            (self.glow.rainbow_phase(), self.glow.rainbow_field())
        } else {
            (0.0, 0.0)
        };
        let rainbow_from = self.glow_scratch.len();
        let rainbow_frame = self.rainbow.tick_with_family_phase(
            cur,
            now,
            energy,
            family_phase,
            family_field,
            f.blink_phase,
            dark,
            geom,
            &rainbow_cfg,
            &mut self.glow_scratch,
        );
        let twinkle_cursor = rainbow_frame.fill.is_some() && rainbow_cfg.blinking;
        let rainbow_owns_caret = rainbow_frame.fill.is_some();
        // On a dark page the block's halo rings keep off an inked or filled row next to the
        // caret's, as the momentum glow does: their top bars would lay the caret's light along
        // the foot of a prompt's powerline segment above it.
        #[cfg(test)]
        let rim_held = !self.rainbow_rim_unheld;
        #[cfg(not(test))]
        let rim_held = true;
        let rim_print = if dark && rim_held && self.glow_scratch.len() > rainbow_from {
            self.glow.hold_off_neighbour_ink(
                &mut self.glow_scratch[rainbow_from..],
                cur,
                geom,
                glow_cfg.theme_bg,
            )
        } else {
            0
        };
        let glow_fp = glow_fp ^ rainbow_frame.fp.rotate_left(23) ^ rim_print.rotate_left(29);
        let blaze = self.glow.blaze();
        let beam_hue = self.glow.beam_hue();
        let body_from = self.glow_scratch.len();
        let droplet_frame = self.droplet.tick(
            cur,
            now,
            blaze,
            geom,
            &DropletConfig {
                enabled: matches!(glow_cfg.style, GlowStyle::Water) && rainbow_block,
                intensity: body_amp,
                ground: Some(f.default_bg & 0x00FF_FFFF),
            },
            &mut self.glow_scratch,
        );
        let mut shares = OverInkShares::EMPTY;
        let body_to = self.glow_scratch.len();

        if body_to > body_from {
            shares.fold(self.glow.admit_over_ink(
                &mut self.glow_scratch,
                body_from..body_to,
                OverInkTier::Body,
            ));
        }
        let glow_fp = glow_fp ^ droplet_frame.fp.rotate_left(47);
        let bar_shape = body_allowed
            && f.focused
            && matches!(
                f.cursor_style,
                CursorStyle::BlinkingBar | CursorStyle::SteadyBar
            );
        let tinted = f.user_tinted
            && glow_cfg.style.reads_knob(
                crate::cursor_glow::TrailKnob::Color,
                crate::cursor_glow::KnobContext {
                    bar: bar_shape,
                    ..crate::cursor_glow::KnobContext::widest(bar_shape)
                },
            );
        #[cfg(test)]
        let warm = !self.momentum_unwarmed;
        #[cfg(not(test))]
        let warm = true;
        // Sparkle's caret on paper is its paper gold deepened in its hue, not its dark-theme
        // peach, which deepens to a mustard brown beside the gold its puff wears. A colour the
        // user chose is its own.
        let sparkle_paper_rod = if warm
            && !dark
            && !tinted
            && !f.cursor_color_pinned
            && matches!(glow_cfg.style, GlowStyle::Sparkle)
        {
            Some(crate::cursor_glow::CursorGlow::sparkle_paper_gold_at(
                glow_cfg, false, 1.0,
            ))
        } else {
            None
        };
        // A colour the user chose: the pinned cursor colour, or the trail colour a style that
        // reads it wears. Paper darkens it in its own hue and saturation and never saturates it
        // ([`crate::color_math::light_caret_fill`]).
        #[cfg(test)]
        let chosen_unread = self.chosen_unread;
        #[cfg(not(test))]
        let chosen_unread = false;
        let chosen = |c: u32| {
            let c = c & 0x00FF_FFFF;
            !chosen_unread
                && ((tinted && c == glow_cfg.color & 0x00FF_FFFF)
                    || matches!(pinned_base, Some(p) if (p & 0x00FF_FFFF) == c))
        };
        // A style that colours a block from its own palette wears its own rod on a bar until a
        // trail colour tints it: what Settings names as the unset colour ([`unset_trail_color`]).
        let own = if tinted {
            None
        } else {
            own_rod(glow_cfg.style)
        };
        let (rod_color, rod_haze) = match glow_cfg.style {
            GlowStyle::Beam => (glow_cfg.color, aterm_render::BEAM_SPACE_HAZE),
            GlowStyle::Sparkle if let Some(gold) = sparkle_paper_rod => {
                (gold, (gold >> 1) & 0x007F_7F7F)
            }
            _ if let Some((color, haze, _)) = own => (color, haze),
            GlowStyle::Phaser if !tinted => {
                let hue = if dark {
                    beam_hue
                } else {
                    crate::color_math::light_spectrum_hue(beam_hue)
                };
                let c = crate::cursor_glow::hsv2rgb(hue, 0.75, 0.95);
                (c, (c >> 1) & 0x007F_7F7F)
            }
            _ => (glow_cfg.color, (glow_cfg.color >> 1) & 0x007F_7F7F),
        };
        let momentum_enabled = momentum_glow_allowed(
            f.momentum_glow,
            body_allowed && f.focused,
            rainbow_owns_caret,
        );
        let momentum_from = self.glow_scratch.len();
        // Sparkle on paper wears its gold: the glow's hue and the caret's
        // charge are the paper gold its body wears, not the peach of its
        // dark-theme colour, which deepens to brown (looks6 N7). A pinned
        // cursor colour or the user's own trail colour is never recoloured.
        let sparkle_paper_gold = if glow_cfg.enabled {
            sparkle_paper_rod
        } else {
            None
        };
        let momentum_base = sparkle_paper_gold.unwrap_or(f.live_cursor);
        // The hue the paper glow wears, and whether it is a colour the user chose: the cursor's
        // with no trail, Sparkle's paper gold (a shade of a chosen trail colour where one is
        // pinned), else the rod's.
        let (momentum_hue, momentum_hue_chosen) = if !glow_cfg.enabled {
            (f.live_cursor & 0x00FF_FFFF, chosen(f.live_cursor))
        } else if warm && matches!(glow_cfg.style, GlowStyle::Sparkle) && !dark {
            match sparkle_paper_rod {
                Some(rod) => (rod, false),
                None => (
                    crate::cursor_glow::CursorGlow::sparkle_paper_gold_at(glow_cfg, tinted, 1.0),
                    chosen(glow_cfg.color),
                ),
            }
        } else {
            (rod_color, chosen(rod_color))
        };
        // A test's control: the momentum glow reads no colour as chosen, as before it did.
        #[cfg(test)]
        let momentum_reads_chosen = !self.momentum_chosen_unread;
        #[cfg(not(test))]
        let momentum_reads_chosen = true;
        let momentum_cfg = MomentumGlowConfig {
            enabled: momentum_enabled,
            intensity: body_amp,
            tau_s: MOMENTUM_GLOW_TAU_S,
            radius_cells: MOMENTUM_GLOW_RADIUS_CELLS,
            base: momentum_base,
            dark_theme: dark,
            light: glow_cfg.light,
            hue: momentum_hue,
            theme_fg: glow_cfg.theme_fg,
            theme_bg: glow_cfg.theme_bg,
            block: is_block(f.cursor_style),
            bar: glow_cfg.head_dx < 0.5,
            glyph_span: if momentum_enabled && !dark {
                match cur {
                    Some(cell) => self.glow.caret_row_glyphs(cell),
                    None => None,
                }
            } else {
                None
            },
            light_vivid: sparkle_paper_gold.is_some(),
            chosen_hue: momentum_reads_chosen && momentum_hue_chosen,
            chosen_base: momentum_reads_chosen && chosen(momentum_base),
        };
        #[cfg(any(test, debug_assertions))]
        {
            self.engine_lights[1] = momentum_cfg.light;
        }
        let momentum_frame = self.momentum.tick(
            cur,
            now,
            geom,
            &momentum_cfg,
            &mut self.glow_scratch,
            self.glow.under_sink(),
        );
        let momentum_to = self.glow_scratch.len();

        self.glow
            .hold_classic_momentum(glow_cfg, &mut self.glow_scratch[momentum_from..momentum_to]);
        let held_print = if dark && momentum_to > momentum_from {
            if warm && glow_cfg.enabled && !tinted && matches!(glow_cfg.style, GlowStyle::Fire) {
                #[cfg(test)]
                let ember = self
                    .momentum_fire_ember
                    .unwrap_or(crate::cursor_momentum::MOMENTUM_FIRE_EMBER);
                #[cfg(not(test))]
                let ember = crate::cursor_momentum::MOMENTUM_FIRE_EMBER;
                crate::cursor_momentum::warm_halo(
                    &mut self.glow_scratch[momentum_from..momentum_to],
                    ember,
                    crate::cursor_momentum::MOMENTUM_FIRE_WARMTH,
                );
            }
            self.glow.hold_off_neighbour_ink(
                &mut self.glow_scratch[momentum_from..momentum_to],
                cur,
                geom,
                glow_cfg.theme_bg,
            )
        } else {
            0
        };
        let momentum_steady = if momentum_frame.hot {
            match f.cursor_style {
                CursorStyle::BlinkingBlock => Some(CursorStyle::SteadyBlock),
                CursorStyle::BlinkingUnderline => Some(CursorStyle::SteadyUnderline),
                CursorStyle::BlinkingBar => Some(CursorStyle::SteadyBar),
                _ => None,
            }
        } else {
            None
        };
        let glow_fp = glow_fp ^ momentum_frame.fp.rotate_left(53) ^ held_print.rotate_left(31);
        let body_from = self.glow_scratch.len();
        let comet_cfg = CometConfig {
            enabled: matches!(glow_cfg.style, GlowStyle::Comet) && rainbow_block,
            intensity: body_amp,
            color: glow_cfg.color,
            accent: glow_cfg.accent,
            dark_theme: dark,
            light: glow_cfg.light,
            theme_fg: glow_cfg.theme_fg,
            theme_bg: glow_cfg.theme_bg,
            pinned: matches!(pinned_base, Some(c) if (c & 0x00FF_FFFF) == (glow_cfg.color & 0x00FF_FFFF)),
            chosen: chosen(glow_cfg.color),
        };
        #[cfg(any(test, debug_assertions))]
        {
            self.engine_lights[2] = comet_cfg.light;
        }
        let comet_frame = self.comet.tick(
            cur,
            now,
            blaze,
            geom,
            &comet_cfg,
            &mut self.glow_scratch,
            self.glow.under_sink(),
        );
        let body_to = self.glow_scratch.len();

        if body_to > body_from {
            shares.fold(self.glow.admit_over_ink(
                &mut self.glow_scratch,
                body_from..body_to,
                OverInkTier::Body,
            ));
        }
        let glow_fp = glow_fp ^ comet_frame.fp.rotate_left(29);
        let body_from = self.glow_scratch.len();
        let phaser_frame = self.phaser.tick(
            cur,
            now,
            beam_hue,
            energy,
            dark,
            geom,
            &PhaserConfig {
                enabled: matches!(glow_cfg.style, GlowStyle::Phaser) && rainbow_block,
                intensity: body_amp,
                base: pinned_base,
            },
            &mut self.glow_scratch,
        );
        let body_to = self.glow_scratch.len();

        if body_to > body_from {
            shares.fold(self.glow.admit_over_ink(
                &mut self.glow_scratch,
                body_from..body_to,
                OverInkTier::Body,
            ));
        }
        let glow_fp = glow_fp ^ phaser_frame.fp.rotate_left(53);
        let emitter_block = matches!(
            glow_cfg.style,
            GlowStyle::Beam | GlowStyle::Lumen | GlowStyle::Sparkle
        );
        let body_from = self.glow_scratch.len();
        let beamrod_cfg = BeamRodConfig {
            enabled: glow_cfg.enabled
                && glow_cfg.intensity > 0.0
                && (bar_shape || (rainbow_block && emitter_block)),
            intensity: body_amp,
            color: rod_color,
            haze: rod_haze,
            bar: bar_shape,
            shimmer: matches!(glow_cfg.style, GlowStyle::Sparkle),
            dark_theme: dark,
            light: glow_cfg.light,
            theme_fg: glow_cfg.theme_fg,
            theme_bg: glow_cfg.theme_bg,
            pinned: matches!(pinned_base, Some(c) if (c & 0x00FF_FFFF) == (rod_color & 0x00FF_FFFF)),
            chosen: chosen(rod_color),
        };
        #[cfg(any(test, debug_assertions))]
        {
            self.engine_lights[3] = beamrod_cfg.light;
        }
        #[cfg(test)]
        {
            self.rod_drawn = beamrod_cfg
                .enabled
                .then_some(beamrod_cfg.color & 0x00FF_FFFF);
        }
        let beamrod_frame = self.beamrod.tick(
            cur,
            now,
            blaze,
            geom,
            &beamrod_cfg,
            &mut self.glow_scratch,
            self.glow.under_sink(),
        );
        let body_to = self.glow_scratch.len();

        if body_to > body_from {
            shares.fold(self.glow.admit_over_ink(
                &mut self.glow_scratch,
                body_from..body_to,
                OverInkTier::Body,
            ));
        }
        if momentum_to > momentum_from {
            shares.fold(self.glow.admit_over_ink(
                &mut self.glow_scratch,
                momentum_from..momentum_to,
                OverInkTier::Momentum,
            ));
        }
        let glow_fp =
            glow_fp ^ shares.0.rotate_left(41) ^ self.glow.over_ink_print().rotate_left(13);
        let glow_fp = glow_fp ^ beamrod_frame.fp.rotate_left(17);
        // Every light is in. Under a pinned trail colour on a dark page each wears the pin's hue
        // and chroma at its own light (`CursorGlow::hold_pinned_lights`), before anything
        // weighs it. None lies on the caret's letter, and the letter guard has the last word on
        // every other.
        self.glow
            .hold_pinned_lights(glow_cfg, &mut self.glow_scratch);
        let punched = self.glow.keep_light_off_caret_letter(
            &mut self.glow_scratch,
            cur,
            geom,
            glow_cfg.style,
        );
        // The cursor trail's cells, under every light: the letter guard reads each letter's
        // ring on the ground the trail fills (it lays its cells before the frame's glow).
        if trail_cfg.enabled {
            crate::cursor_trail::ignite(&mut trail_cfg, energy, warmth);
        }
        let _ = self
            .trail
            .tick(cur, now, &trail_cfg, &mut self.trail_scratch);
        let legible = if self.trail_scratch.is_empty() {
            1.0
        } else {
            let key = (trail_cfg.color, f.default_bg, glow_cfg.theme_fg);
            match self.legible_memo {
                Some((prev, share)) if prev == key => share,
                _ => {
                    let share = bed_legible_share(key.0, key.1, key.2);
                    self.legible_memo = Some((key, share));
                    share
                }
            }
        };
        let trail_fp = crate::cursor_trail::project_trail_presentation(
            &mut self.trail_scratch,
            trail_cfg.color,
            f.shed_envelope * bed_brightness * legible,
        );
        // A focused block caret fills its cell; a bar, an underline or an unfocused outline
        // leaves its cell's ground beside a neighbour's letter.
        let caret_block = f.focused && is_block(f.cursor_style);
        let guarded = self.glow.guard_letters(
            &mut self.glow_scratch,
            (cur, caret_block),
            (geom, glow_cfg),
            (&self.trail_scratch, trail_cfg.color),
        );
        let glow_fp = glow_fp ^ guarded.rotate_left(37) ^ punched.rotate_left(7);
        self.glow
            .ground_the_frame(glow_cfg, geom, &self.glow_scratch);
        let bolt_cursor = rainbow_block
            && glow_cfg.enabled
            && glow_cfg.intensity > 0.0
            && matches!(glow_cfg.style, GlowStyle::Laser);
        #[cfg(any(test, debug_assertions))]
        let (bolt_unlit, bolt_clip_stop, paper_unlit) = (
            self.chosen_bolt_unlit,
            self.chosen_bolt_clip_stop,
            self.paper_bolt_unlit,
        );
        #[cfg(not(any(test, debug_assertions)))]
        let (bolt_unlit, bolt_clip_stop, paper_unlit) = (false, false, false);
        let bolt_fill = if bolt_cursor {
            let fill = bolt_body(glow_cfg.color, blaze, dark, chosen(glow_cfg.color));
            // THE PAPER BOLT READS AGAINST ITS OWN HALO (composed r1's access judge, O5(a)): in
            // the theme's own colour on paper Laser's bolt `#7834f9` sat in its lavender halo
            // `#cab9ec` at 3.72:1 as the steam's Backspaces fed it (the trail gallery's
            // `laser-light-steam.bs09` read 4.41 on GitHub Light, 4.40 on Default Light). No
            // ruling excuses it there: it is lifted in lightness alone as a chosen colour's is.
            // The dark bolt keeps its ruled light.
            let lift = if chosen(glow_cfg.color) {
                !bolt_unlit
            } else {
                !dark && !paper_unlit
            };
            Some(match cur {
                Some(cell) if lift => {
                    let [mut under, mut laid] = std::mem::take(&mut self.bolt_lit_scratch);
                    self.glow.under_laid_into(&mut under);
                    self.glow.out_laid_into(&self.glow_scratch, &mut laid);
                    let lifted = chosen_bolt_over_halo(
                        fill,
                        (&under, &laid, self.glow.halos()),
                        f.cursor_cell_bg.unwrap_or(f.default_bg),
                        geom,
                        cell,
                        !bolt_clip_stop,
                    );
                    self.bolt_lit_scratch = [under, laid];
                    lifted
                }
                _ => fill,
            })
        } else {
            None
        };
        // Over a letter the bolt stands down to a steady block of its own colour, which the
        // renderer cuts the letter out of: a bolt is no block, and drawn over the letter it hid
        // it (looks8 WIDEN move-up: the `!` read 1.00:1).
        let bolt_down = bolt_cursor && self.glow.caret_bolt_stands_down(cur);
        let glow_fp = glow_fp ^ u64::from(bolt_fill.unwrap_or(0)).rotate_left(59);
        // A bar or underline caret reads against the light of its own cell (a trail cell's
        // blend under it, the lights over it): where its colour at its lightest (dark) or
        // darkest (paper) cannot, that light is held there
        // (`CursorGlow::hold_light_for_thin_caret`); its colour is lifted below.
        let thin = match (cur, f.focused, thin_bar(f.cursor_style)) {
            (Some(cell), true, Some(bar)) => Some(crate::cursor_glow::ThinCaret {
                cell,
                bar,
                page: f.cursor_cell_bg.unwrap_or(f.default_bg) & 0x00FF_FFFF,
                pin: f.cursor_color_pinned.then_some(f.live_cursor & 0x00FF_FFFF),
            }),
            _ => None,
        };
        let under = thin.map(|caret| {
            let (row, col) = (usize::from(caret.cell.0), usize::from(caret.cell.1));
            match self
                .trail_scratch
                .iter()
                .find(|t| t.row == row && t.col == col)
            {
                Some(t) if trail_cfg.enabled => {
                    aterm_render::blend_rgb(caret.page, trail_cfg.color & 0x00FF_FFFF, t.alpha)
                        & 0x00FF_FFFF
                }
                _ => caret.page,
            }
        });
        let held = self.glow.hold_light_for_thin_caret(
            &mut self.glow_scratch,
            thin.zip(under),
            geom,
            glow_cfg.style,
        );
        let glow_fp = glow_fp ^ held.rotate_left(29);
        let momentum_fill = match (momentum_frame.fill, sparkle_paper_gold) {
            // The gold caret keeps the page bar, deepened in its hue.
            (Some(fill), Some(_)) => Some(crate::color_math::floor_contrast_keeping_hue(
                fill,
                f.cursor_cell_bg.unwrap_or(f.default_bg) & 0x00FF_FFFF,
                CARET_PAGE_MIN_CONTRAST + SPARKLE_PAPER_CARET_GUARD,
            )),
            (fill, _) => fill,
        };
        let block_fill = if let Some(owned) = resolve_block_fill(
            BlockFillSplice {
                rainbow: rainbow_frame.fill,
                forge: forge_fill,
                phaser: phaser_frame.fill,
                bolt: bolt_fill,
                comet: comet_frame.fill,
                droplet: droplet_frame.fill,
                momentum: momentum_fill,
                beamrod: beamrod_frame.fill,
            },
            f.live_cursor,
            glow_cfg.color,
            f.cursor_color_pinned,
        ) {
            let lettered = self.glow.caret_fill_keeps_letter(cur);
            let ground = f.cursor_cell_bg.unwrap_or(f.default_bg);
            let floored = match self.floor_fill_memo {
                Some(memo)
                    if memo.candidate == owned
                        && memo.ground == ground
                        && memo.live_cursor == f.live_cursor
                        && memo.pinned_cursor == pinned_base
                        && memo.lettered == lettered =>
                {
                    memo.floored
                }
                _ => {
                    let floored =
                        floor_block_fill(owned, ground, f.live_cursor, pinned_base, lettered);
                    self.floor_fill_memo = Some(FloorFillMemo {
                        candidate: owned,
                        ground,
                        live_cursor: f.live_cursor,
                        pinned_cursor: pinned_base,
                        lettered,
                        floored,
                    });
                    floored
                }
            };
            project_block_fill(Some(floored), f.live_cursor, f.shed_envelope)
        } else {
            None
        };
        let glow_fp = match block_fill {
            Some(owned) => glow_fp
                .wrapping_mul(1_000_003)
                .wrapping_add(u64::from(owned.fill) | (1 << 32)),
            None => glow_fp,
        };
        // The thin caret's colour, lifted in lightness to read against its cell's light.
        let thin_fill = self
            .glow
            .thin_caret_fill(block_fill.map_or(f.live_cursor, |owned| owned.fill));
        let glow_fp = match thin_fill {
            Some(fill) => glow_fp.rotate_left(11) ^ (u64::from(fill) | (1 << 33)),
            None => glow_fp,
        };
        CursorFxFrame {
            glow_fp,
            trail_fp,
            trail_color: trail_cfg.color,
            block_fill,
            thin_fill,
            bolt_cursor: bolt_cursor && !bolt_down,
            twinkle_cursor: twinkle_cursor || bolt_down,
            momentum_steady,
        }
    }
}

#[cfg(test)]
#[path = "cursor_fx_frame_tests.rs"]
mod frame_tests;

#[cfg(test)]
#[path = "cursor_fx_tests.rs"]
mod tests;
