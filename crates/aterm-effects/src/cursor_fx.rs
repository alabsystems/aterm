// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cursor family's frame step.

use aterm_core::terminal::CursorStyle;
use aterm_render::{GlowQuad, TrailCell};
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
    /// The laser bolt owns the block: the caret's shape is `Bolt`.
    pub bolt_cursor: bool,
    /// A body owns a blinking block, and the shape is pinned steady: the rainbow's, and the
    /// block Laser's bolt stands down to over a letter (`CursorGlow::caret_bolt_stands_down`).
    pub twinkle_cursor: bool,
    /// The momentum glow is hot: this Blinking* style pinned to its Steady* twin (`None` when
    /// cold, or when the style does not blink).
    pub momentum_steady: Option<CursorStyle>,
}

/// Where the streak's head attaches in its cell: a thin bar's insertion point, else the cell
/// centre.
#[inline]
#[must_use]
pub const fn head_dx_for(style: CursorStyle) -> f32 {
    if matches!(style, CursorStyle::BlinkingBar | CursorStyle::SteadyBar) {
        0.08
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
    let ink = ink & 0x00FF_FFFF;
    let table = crate::color_math::relative_luminance_table();
    let lum_ink = crate::color_math::relative_luminance_with(table, ink);
    #[inline]
    const fn contrast_ink(
        table: &crate::color_math::RelativeLuminanceTable,
        lum_ink: f32,
        bg: u32,
    ) -> f32 {
        let lb = crate::color_math::relative_luminance_with(table, bg);
        (lum_ink.max(lb) + 0.05) / (lum_ink.min(lb) + 0.05)
    }
    let target = BED_INK_FLOOR.min(contrast_ink(table, lum_ink, ground));
    #[inline]
    const fn holds_bg(
        table: &crate::color_math::RelativeLuminanceTable,
        lum_ink: f32,
        target: f32,
        bg: u32,
    ) -> bool {
        let lb = crate::color_math::relative_luminance_with(table, bg);
        lum_ink.max(lb) + 0.05 >= target * (lum_ink.min(lb) + 0.05)
    }
    let peak = crate::cursor_trail::BED_PEAK_ALPHA;
    let peak_bg = aterm_render::blend_rgb(ground, color, peak);
    if holds_bg(table, lum_ink, target, peak_bg) {
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
            holds_bg(table, lum_ink, target, mid_bg)
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
/// rainbow kitty's paper caret's too.
#[inline]
#[must_use]
pub const fn caret_page_floor(owner: BlockFillOwner) -> f32 {
    match owner {
        BlockFillOwner::Rainbow => crate::rainbow_kitty::ribbon::PAPER_CARET_CONTRAST,
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
    /// The full-height ribbon body.
    pub ribbon_tall: bool,
    /// The explicit flat ribbon body.
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
            ribbon_tall: !GlowStyle::style_names_underline_ribbon(token),
            ribbon_flat: GlowStyle::style_names_flat_ribbon(token),
            classic_mono: GlowStyle::style_names_classic_mono(raw),
        }
    }

    /// The companion a page's raw spelling names.
    #[inline]
    #[must_use]
    pub const fn pet_of(raw: &str) -> Option<crate::kitty_pet::PetSpecies> {
        if GlowStyle::style_names_any_pet(raw) {
            Some(if GlowStyle::style_names_dog_pet(raw) {
                crate::kitty_pet::PetSpecies::Dog
            } else {
                crate::kitty_pet::PetSpecies::Cat
            })
        } else {
            None
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

/// What an unset `cursor_trail_color` draws for `style`'s trail on a block, by name, where that is
/// not the live cursor's colour.
#[inline]
#[must_use]
pub const fn trail_color_default_name(
    style: GlowStyle,
    cursor_color_pinned: bool,
) -> Option<&'static str> {
    if matches!(style, GlowStyle::Classic) {
        return Some("Shifting colors");
    }
    match own_trail_color(style, cursor_color_pinned) {
        Some((_, name)) => Some(name),
        None => None,
    }
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

impl CursorFx {
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
        let (rod_color, rod_haze) = match glow_cfg.style {
            GlowStyle::Beam => (glow_cfg.color, aterm_render::BEAM_SPACE_HAZE),
            GlowStyle::Sparkle if let Some(gold) = sparkle_paper_rod => {
                (gold, (gold >> 1) & 0x007F_7F7F)
            }
            GlowStyle::Water if !tinted => (0x0032_DCDE, 0x000E_66B4),
            GlowStyle::Fire if !tinted => (0x00FF_9632, 0x0078_1E00),
            GlowStyle::RainbowKitty if !tinted => (0x00FF_66CC, 0x0046_1E64),
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
        // Every light is in: none lies on the caret's letter, and the letter guard has the last
        // word on every other.
        let punched = self.glow.keep_light_off_caret_letter(
            &mut self.glow_scratch,
            cur,
            geom,
            glow_cfg.style,
        );
        let guarded = self
            .glow
            .guard_letters(&mut self.glow_scratch, cur, geom, glow_cfg.style);
        let glow_fp = glow_fp ^ guarded.rotate_left(37) ^ punched.rotate_left(7);
        self.glow
            .ground_the_frame(glow_cfg, geom, &self.glow_scratch);
        // The pet's veil is drawn after the pet's tick, by the body it drew
        // this frame (`EffectsPipeline::veil_pet_letters`).
        self.glow.note_veil_frame(cur, geom);
        let bolt_cursor = rainbow_block
            && glow_cfg.enabled
            && glow_cfg.intensity > 0.0
            && matches!(glow_cfg.style, GlowStyle::Laser);
        let bolt_fill = if bolt_cursor {
            Some(bolt_body(
                glow_cfg.color,
                blaze,
                dark,
                chosen(glow_cfg.color),
            ))
        } else {
            None
        };
        // Over a letter the bolt stands down to a steady block of its own colour, which the
        // renderer cuts the letter out of: a bolt is no block, and drawn over the letter it hid
        // it (looks8 WIDEN move-up: the `!` read 1.00:1).
        let bolt_down = bolt_cursor && self.glow.caret_bolt_stands_down(cur);
        let glow_fp = glow_fp ^ u64::from(bolt_fill.unwrap_or(0)).rotate_left(59);
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
        CursorFxFrame {
            glow_fp,
            trail_fp,
            trail_color: trail_cfg.color,
            block_fill,
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
mod tests {
    use super::*;

    #[test]
    fn the_caret_style_law_is_one_truth_table() {
        use CursorStyle::{Bolt, SteadyBar, SteadyBlock, SteadyUnderline};
        let rows = [
            (false, true, None, Some(SteadyBlock)),
            (false, true, Some(SteadyBlock), Some(SteadyBlock)),
            (false, true, Some(SteadyBar), Some(SteadyBlock)),
            (false, true, Some(SteadyUnderline), Some(SteadyBlock)),
            (false, false, None, None),
            (false, false, Some(SteadyBlock), Some(SteadyBlock)),
            (false, false, Some(SteadyBar), Some(SteadyBar)),
            (false, false, Some(SteadyUnderline), Some(SteadyUnderline)),
            (true, true, Some(SteadyBlock), Some(Bolt)),
            (true, false, None, Some(Bolt)),
        ];
        for (bolt, rainbow, momentum, want) in rows {
            assert_eq!(
                compose_caret_style_override(bolt, rainbow, momentum),
                want,
                "bolt={bolt} rainbow={rainbow} momentum={momentum:?}"
            );
        }
    }

    #[test]
    fn the_momentum_engine_yields_the_caret_cell_to_the_rainbow() {
        assert!(!momentum_glow_allowed(true, true, true));
        assert!(momentum_glow_allowed(true, true, false));
        assert!(
            !momentum_glow_allowed(false, true, false),
            "the user's switch is honoured"
        );
        assert!(
            !momentum_glow_allowed(true, false, false),
            "and so are the host's gates"
        );
    }

    #[test]
    fn every_effect_body_fades_to_the_terminal_cursor_through_one_seam() {
        let terminal_base = 0x0010_2030;
        let pinned_trail_base = 0x0000_FF00;
        let effect_fill = 0x00E0_C090;
        for owner in [
            BlockFillOwner::Rainbow,
            BlockFillOwner::Forge,
            BlockFillOwner::Phaser,
            BlockFillOwner::Bolt,
            BlockFillOwner::Comet,
            BlockFillOwner::Droplet,
            BlockFillOwner::BeamRod,
        ] {
            let identity = matches!(owner, BlockFillOwner::Forge | BlockFillOwner::Droplet);
            let candidate = BlockFill {
                owner,
                fill: effect_fill,
                base: (!identity).then_some(pinned_trail_base),
            };
            assert_eq!(
                project_block_fill(Some(candidate), terminal_base, 1.0),
                Some(candidate),
                "{} must preserve the full-amplitude endpoint",
                owner.label()
            );
            for envelope in [0.75, 0.5, 0.25, 0.001] {
                let projected = project_block_fill(Some(candidate), terminal_base, envelope)
                    .expect("a positive envelope retains body custody");
                assert_eq!(
                    projected.fill,
                    aterm_render::blend_rgb(
                        terminal_base,
                        effect_fill,
                        (envelope * f32::from(u8::MAX)).round() as u8,
                    ),
                    "{} diverged from the shared fade at {envelope}",
                    owner.label()
                );
                assert_eq!(projected.base, candidate.base, "provenance is preserved");
            }
            assert_eq!(
                project_block_fill(Some(candidate), terminal_base, 0.0),
                None,
                "{} must return exact-zero custody to the terminal",
                owner.label()
            );
        }
        assert_eq!(project_block_fill(None, terminal_base, 1.0), None);
        assert_eq!(
            project_block_fill(
                Some(BlockFill {
                    owner: BlockFillOwner::Rainbow,
                    fill: effect_fill,
                    base: Some(pinned_trail_base),
                }),
                terminal_base,
                f32::NAN,
            ),
            None,
            "non-finite envelopes fail closed"
        );
    }

    const fn frame_input(now: Instant) -> CursorFxInput {
        CursorFxInput {
            now,
            cur: Some((2, 3)),
            live_viewport: true,
            cursor_style: CursorStyle::SteadyBar,
            blink_phase: true,
            live_cursor: 0x0050_FA7B,
            cursor_color_pinned: false,
            default_bg: 0x001A_1B26,
            cursor_cell_bg: None,
            geom: Geom {
                cw: 10,
                ch: 20,
                rows: 24,
                cols: 80,
                origin_x: 0,
                origin_y: 0,
                win_w: 800,
                win_h: 480,
                head: 0,
            },
            focused: true,
            shed_envelope: 1.0,
            trail_brightness: 1.0,
            body_allowed: true,
            momentum_glow: true,
            user_tinted: false,
            accent_pinned: false,
        }
    }

    #[test]
    fn the_automatic_glow_colour_law_is_one_truth_table() {
        use crate::cursor_glow::{
            BEAM_DEFAULT_COLOR, COMET_DEFAULT_COLOR, LASER_DEFAULT_COLOR, SPARKLE_DEFAULT_COLOR,
        };
        let live = 0x00FF_40C0;
        for (style, pinned, want) in [
            (GlowStyle::Lumen, false, live),
            (GlowStyle::Lumen, true, live),
            (GlowStyle::RainbowKitty, false, live),
            (GlowStyle::Phaser, false, live),
            (GlowStyle::Beam, false, BEAM_DEFAULT_COLOR),
            (GlowStyle::Beam, true, live),
            (GlowStyle::Comet, false, COMET_DEFAULT_COLOR),
            (GlowStyle::Comet, true, live),
            (GlowStyle::Sparkle, false, SPARKLE_DEFAULT_COLOR),
            (GlowStyle::Sparkle, true, live),
            (GlowStyle::Laser, false, LASER_DEFAULT_COLOR),
            (GlowStyle::Laser, true, LASER_DEFAULT_COLOR),
        ] {
            assert_eq!(
                automatic_glow_color(style, live, pinned),
                want,
                "{style:?} with pinned={pinned}"
            );
        }
        let resolve = |style, color| {
            resolve_glow_config(
                GlowKnobs {
                    enabled: true,
                    color,
                    accent: None,
                    duration_ms: 260,
                    length: 24,
                    intensity: 1.0,
                    radius: 0.6,
                    ring: true,
                },
                GlowSpelling {
                    style: Some(style),
                    beam: true,
                    ribbon_tall: false,
                    ribbon_flat: false,
                    classic_mono: false,
                },
                live,
                true,
                0x00C8_D3F5,
                0x001A_1B26,
                0.5,
            )
        };
        for style in [GlowStyle::Lumen, GlowStyle::Comet, GlowStyle::Laser] {
            let cfg = resolve(style, None);
            assert_eq!(cfg.color, automatic_glow_color(style, live, false));
            assert_eq!(cfg.accent, brighten(cfg.color, 1.5));
            assert_eq!(
                resolve(style, Some(0x0012_3456)).color,
                0x0012_3456,
                "{style:?}: an explicit cursor_trail_color beats the law"
            );
        }
    }

    const fn comet_cfgs(knob: f32) -> (GlowConfig, TrailConfig) {
        let glow = resolve_glow_config(
            GlowKnobs {
                enabled: true,
                color: None,
                accent: None,
                duration_ms: 260,
                length: 24,
                intensity: knob,
                radius: 0.6,
                ring: true,
            },
            GlowSpelling {
                style: Some(GlowStyle::Comet),
                beam: true,
                ribbon_tall: false,
                ribbon_flat: false,
                classic_mono: false,
            },
            0x0050_FA7B,
            true,
            0x00C8_D3F5,
            0x001A_1B26,
            0.5,
        );
        let trail = TrailConfig {
            enabled: true,
            duration: std::time::Duration::from_millis(260),
            max_len: 24,
            color: glow.color,
            intensity: 0.0,
            warmth: 0.0,
        };
        (glow, trail)
    }

    fn typed_comet_burst(knob: f32) -> (Option<u32>, u8, usize) {
        let (glow, trail) = comet_cfgs(knob);
        let mut fx = CursorFx::default();
        let t0 = Instant::now();
        let input = |now, col| CursorFxInput {
            cur: Some((2, col)),
            cursor_style: CursorStyle::SteadyBlock,
            trail_brightness: knob,
            ..frame_input(now)
        };
        fx.tick(&input(t0, 3), &glow, trail);
        let (mut fill, mut alpha_max, mut cells) = (None, 0u8, 0usize);
        for k in 0..20u16 {
            let now = t0 + std::time::Duration::from_millis(60 * (u64::from(k) + 1));
            fx.glow.note_synthetic_typed(now, 1);
            fx.trail.note_synthetic_typed(now);
            fx.cadence.on_keystroke(now);
            let frame = fx.tick(&input(now, 4 + k), &glow, trail);
            fill = frame
                .block_fill
                .filter(|owned| owned.owner == BlockFillOwner::Comet)
                .map(|owned| owned.fill);
            cells += fx.trail_scratch.len();
            let brightest = fx.trail_scratch.iter().map(|cell| cell.alpha).max();
            alpha_max = alpha_max.max(brightest.unwrap_or(0));
        }
        (fill, alpha_max, cells)
    }

    #[test]
    fn the_brightness_dial_dims_the_comet_nucleus_and_its_bed() {
        let luma = |rgb: u32| (rgb >> 16 & 0xff) + (rgb >> 8 & 0xff) + (rgb & 0xff);
        let (full_fill, full_alpha, full_cells) = typed_comet_burst(1.0);
        let full_fill = full_fill.expect("control: a full dial's comet owns the block caret");
        assert!(
            full_cells > 0 && full_alpha > 0,
            "control: a full dial lays a bed under a fast burst"
        );
        let (quarter_fill, quarter_alpha, quarter_cells) = typed_comet_burst(0.25);
        let quarter_fill = quarter_fill.expect("a quarter dial still draws the nucleus");
        assert!(
            luma(quarter_fill) < luma(full_fill),
            "a quarter dial must frost the nucleus less: {quarter_fill:06x} vs {full_fill:06x}"
        );
        assert!(
            quarter_cells > 0 && quarter_alpha < full_alpha,
            "a quarter dial must lay a fainter bed: max alpha {quarter_alpha} vs {full_alpha}"
        );
        let (off_fill, _, off_cells) = typed_comet_burst(0.0);
        assert_eq!(off_fill, None, "a zero dial draws no nucleus");
        assert_eq!(off_cells, 0, "a zero dial lays no bed");
    }

    fn classic_cfg(enabled: bool) -> (GlowConfig, TrailConfig) {
        let glow = GlowConfig {
            ribbon_tall: false,
            enabled,
            style: GlowStyle::Classic,
            intensity: 0.7,
            audible: true,
            radius: 0.6,
            ring: true,
            beam: true,
            head_dx: 0.08,
            ..Default::default()
        };
        let trail = TrailConfig {
            enabled: false,
            duration: std::time::Duration::from_millis(260),
            max_len: 24,
            color: 0x0050_FA7B,
            intensity: 0.0,
            warmth: 0.0,
        };
        (glow, trail)
    }

    #[test]
    fn the_momentum_glow_obeys_its_own_switch() {
        for momentum_glow in [true, false] {
            let mut fx = CursorFx::default();
            let t0 = Instant::now();
            for k in 0..24u64 {
                fx.momentum.on_key(
                    t0 + std::time::Duration::from_millis(k * 40),
                    MOMENTUM_GLOW_TAU_S,
                );
            }
            let now = t0 + std::time::Duration::from_millis(24 * 40);
            let (glow, trail) = classic_cfg(false);
            let frame = fx.tick(
                &CursorFxInput {
                    momentum_glow,
                    ..frame_input(now)
                },
                &glow,
                trail,
            );
            let momentum_owns = frame
                .block_fill
                .is_some_and(|owned| owned.owner == BlockFillOwner::Momentum);
            if momentum_glow {
                assert!(
                    fx.momentum.is_active() || !fx.glow_scratch.is_empty(),
                    "control: a fast hand lights the momentum glow with its switch on"
                );
            } else {
                assert!(
                    fx.glow_scratch.is_empty() && !momentum_owns && frame.momentum_steady.is_none(),
                    "the switch off paints no momentum halo and pins no caret"
                );
            }
        }
    }

    #[test]
    fn a_cooling_momentum_glow_is_a_settling_body() {
        let lane = std::time::Duration::from_micros(16_667);
        let mut fx = CursorFx::default();
        let t0 = Instant::now();
        for k in 0..24u64 {
            fx.momentum.on_key(
                t0 + std::time::Duration::from_millis(k * 60),
                MOMENTUM_GLOW_TAU_S,
            );
        }
        let (glow, trail) = classic_cfg(false);
        let mut now = t0 + std::time::Duration::from_millis(23 * 60);
        loop {
            fx.tick(&frame_input(now), &glow, trail);
            if !fx.bodies_active() {
                break;
            }
            now += lane;
            assert!(
                now.saturating_duration_since(t0) < std::time::Duration::from_secs(10),
                "fixture: the glow's frames never stopped moving"
            );
        }
        assert!(
            !fx.glow_scratch.is_empty(),
            "negative control: the cadence let go with the halo on glass"
        );
        assert!(
            fx.bodies_settling() && fx.momentum.settling() && !fx.rainbow.settling(),
            "the cooling glow holds the lane"
        );
        let next = fx
            .bodies_next_change_deadline(now, lane)
            .expect("a settling glow names its next step");
        assert_eq!(Some(next), fx.momentum.next_change_deadline(now, lane));
        assert!(next >= now + lane, "never sooner than a frame out");
    }

    #[test]
    fn effect_caret_fills_keep_the_page_floor_unless_they_wear_a_pinned_cursor() {
        use crate::color_math::{contrast_ratio, rgb2hsv};
        let owners = [
            BlockFillOwner::Rainbow,
            BlockFillOwner::Forge,
            BlockFillOwner::Phaser,
            BlockFillOwner::Bolt,
            BlockFillOwner::Comet,
            BlockFillOwner::Droplet,
            BlockFillOwner::BeamRod,
            BlockFillOwner::Momentum,
        ];
        for (ground, low, live) in [
            (0x0011_1318u32, 0x0051_0A00u32, 0x0050_FA7Bu32),
            (0x00FF_FFFF, 0x00B4_8CFF, 0x0009_69DA),
        ] {
            for owner in owners {
                let candidate = BlockFill {
                    owner,
                    fill: low,
                    base: None,
                };
                let floored = floor_block_fill(candidate, ground, live, None, false);
                let bar = CARET_PAGE_MIN_CONTRAST;
                assert_eq!(caret_page_floor(owner), bar, "{}", owner.label());
                assert!(
                    contrast_ratio(floored.fill, ground) >= bar,
                    "{} {low:06x} on {ground:06x} left at {:06x}",
                    owner.label(),
                    floored.fill
                );
                assert_eq!((floored.owner, floored.base), (owner, None));
                let (h0, _, _) = rgb2hsv(low);
                let (h1, _, _) = rgb2hsv(floored.fill);
                assert!((h0 - h1).abs() < 4.0, "{}: the hue moved", owner.label());
                let fine = BlockFill {
                    fill: if ground == 0x00FF_FFFF {
                        0x0009_69DA
                    } else {
                        0x00E0_C090
                    },
                    ..candidate
                };
                assert_eq!(floor_block_fill(fine, ground, live, None, false), fine);
            }
        }
        let pinned = 0x0000_00FF;
        for owner in [BlockFillOwner::Rainbow, BlockFillOwner::BeamRod] {
            let worn = BlockFill {
                owner,
                fill: pinned,
                base: Some(pinned),
            };
            assert_eq!(
                floor_block_fill(worn, 0x0011_1318, pinned, Some(pinned), false),
                worn
            );
            assert_ne!(
                floor_block_fill(worn, 0x0011_1318, 0x0050_FA7B, None, false).fill,
                pinned,
                "control: unpinned, the same {} fill is floored",
                owner.label()
            );
        }
        let bolt = BlockFill {
            owner: BlockFillOwner::Bolt,
            fill: 0x00B4_8CFF,
            base: Some(0x00B4_8CFF),
        };
        assert_ne!(
            floor_block_fill(bolt, 0x00FF_FFFF, 0x0000_00FF, Some(0x0000_00FF), false),
            bolt
        );
        let (latte, rosewater) = (0x00EF_F1F5, 0x00DC_8A78);
        let tint = BlockFill {
            owner: BlockFillOwner::Momentum,
            fill: rosewater,
            base: None,
        };
        assert_eq!(floor_block_fill(tint, latte, rosewater, None, false), tint);
        let forge = BlockFill {
            owner: BlockFillOwner::Forge,
            ..tint
        };
        assert!(
            contrast_ratio(
                floor_block_fill(forge, latte, rosewater, None, false).fill,
                latte
            ) >= CARET_PAGE_MIN_CONTRAST
        );
    }

    fn style_cfgs(style: GlowStyle, page: u32) -> (GlowConfig, TrailConfig) {
        let dark = aterm_render::theme_is_dark(page);
        let glow = resolve_glow_config(
            GlowKnobs {
                enabled: true,
                color: None,
                accent: None,
                duration_ms: 260,
                length: 24,
                intensity: 1.0,
                radius: 0.6,
                ring: true,
            },
            GlowSpelling {
                style: Some(style),
                beam: true,
                ribbon_tall: false,
                ribbon_flat: false,
                classic_mono: false,
            },
            if dark { 0x0050_FA7B } else { 0x0009_69DA },
            dark,
            if dark { 0x00C8_D3F5 } else { 0x0024_292F },
            page,
            0.5,
        );
        let trail = TrailConfig {
            enabled: matches!(style, GlowStyle::Comet),
            duration: std::time::Duration::from_millis(260),
            max_len: 24,
            color: glow.color,
            intensity: 0.0,
            warmth: 0.0,
        };
        (glow, trail)
    }

    #[test]
    fn every_effect_caret_keeps_the_page_floor_while_typing_and_at_rest() {
        for page in [0x0011_1318u32, 0x0028_2A36, 0x00FF_FFFF, 0x00FD_F6E3] {
            let dark = aterm_render::theme_is_dark(page);
            for style in [
                GlowStyle::Laser,
                GlowStyle::Lumen,
                GlowStyle::Beam,
                GlowStyle::Sparkle,
                GlowStyle::Water,
                GlowStyle::Comet,
                GlowStyle::Fire,
                GlowStyle::Phaser,
                GlowStyle::RainbowKitty,
            ] {
                let (glow, trail) = style_cfgs(style, page);
                let mut fx = CursorFx::default();
                let t0 = Instant::now();
                let input = |now, col: u16| CursorFxInput {
                    cur: Some((2, col)),
                    cursor_style: CursorStyle::SteadyBlock,
                    live_cursor: if dark { 0x0050_FA7B } else { 0x0009_69DA },
                    default_bg: page,
                    momentum_glow: false,
                    ..frame_input(now)
                };
                let mut hottest = 0.0f32;
                for k in 0..25u16 {
                    let now = if k < 24 {
                        let now = t0 + std::time::Duration::from_millis(50 * u64::from(k));
                        fx.glow.note_synthetic_typed(now, 1);
                        fx.trail.note_synthetic_typed(now);
                        fx.cadence.on_keystroke(now);
                        now
                    } else {
                        t0 + std::time::Duration::from_secs(12)
                    };
                    let frame = fx.tick(&input(now, 4 + k.min(23)), &glow, trail);
                    hottest = hottest.max(fx.glow.blaze());
                    let owned = frame.block_fill.unwrap_or_else(|| {
                        panic!("{style:?} on {page:06x}: a body owns the caret")
                    });
                    let page_ratio = crate::color_math::contrast_ratio(owned.fill, page);
                    let bar = if owned.owner == BlockFillOwner::Rainbow {
                        3.0
                    } else {
                        CARET_PAGE_MIN_CONTRAST
                    };
                    assert!(
                        page_ratio >= bar,
                        "{style:?} on {page:06x} frame {k}: {} fill {:06x} at {page_ratio:.2}:1",
                        owned.owner.label(),
                        owned.fill
                    );
                }
                assert!(
                    hottest > 0.8,
                    "{style:?} on {page:06x}: the burst never ran hot (blaze {hottest:.2})"
                );
            }
        }
    }

    #[test]
    fn the_caret_floor_holds_against_the_cell_it_sits_on() {
        use crate::color_math::contrast_ratio;
        const PAGE: u32 = 0x0011_1318;
        let run = |style: GlowStyle, cell: Option<u32>| -> Vec<u32> {
            let (glow, trail) = style_cfgs(style, PAGE);
            let mut fx = CursorFx::default();
            let t0 = Instant::now();
            let input = |now, col: u16| CursorFxInput {
                cur: Some((2, col)),
                cursor_style: CursorStyle::SteadyBlock,
                default_bg: PAGE,
                cursor_cell_bg: cell,
                momentum_glow: false,
                ..frame_input(now)
            };
            (0..25u16)
                .map(|k| {
                    let now = if k < 24 {
                        let now = t0 + std::time::Duration::from_millis(50 * u64::from(k));
                        fx.glow.note_synthetic_typed(now, 1);
                        fx.trail.note_synthetic_typed(now);
                        fx.cadence.on_keystroke(now);
                        now
                    } else {
                        t0 + std::time::Duration::from_secs(12)
                    };
                    fx.tick(&input(now, 4 + k.min(23)), &glow, trail)
                        .block_fill
                        .unwrap_or_else(|| panic!("{style:?}: a body owns the caret"))
                        .fill
                })
                .collect()
        };
        let (reverse, band) = (0x00D0_D0D0u32, 0x003B_4261u32);
        let mut sunk = false;
        for style in [
            GlowStyle::Fire,
            GlowStyle::Laser,
            GlowStyle::Lumen,
            GlowStyle::Water,
            GlowStyle::RainbowKitty,
        ] {
            for cell in [reverse, band] {
                let bar = if style == GlowStyle::RainbowKitty {
                    caret_page_floor(BlockFillOwner::Rainbow)
                } else {
                    CARET_PAGE_MIN_CONTRAST
                };
                for (k, fill) in run(style, Some(cell)).into_iter().enumerate() {
                    let ratio = contrast_ratio(fill, cell);
                    assert!(
                        ratio >= bar,
                        "{style:?} on cell {cell:06x} frame {k}: {fill:06x} at {ratio:.2}:1"
                    );
                }
            }
            sunk |= run(style, None)
                .into_iter()
                .any(|fill| contrast_ratio(fill, reverse) < 3.0);
        }
        assert!(
            sunk,
            "control: with the page standing in, some caret must sink on the reverse cell"
        );
        let (glow, trail) = style_cfgs(GlowStyle::RainbowKitty, PAGE);
        let pinned = 0x00A0_A0A0;
        let fill = |cell: Option<u32>| {
            CursorFx::default()
                .tick(
                    &CursorFxInput {
                        cursor_style: CursorStyle::SteadyBlock,
                        default_bg: PAGE,
                        cursor_cell_bg: cell,
                        live_cursor: pinned,
                        cursor_color_pinned: true,
                        momentum_glow: false,
                        ..frame_input(Instant::now())
                    },
                    &glow,
                    trail,
                )
                .block_fill
                .expect("the rainbow wears the pinned colour")
                .fill
        };
        let worn = fill(Some(reverse));
        assert_eq!(
            worn,
            fill(None),
            "a pinned colour is the user's on any cell"
        );
        assert!(
            contrast_ratio(worn, reverse) < CARET_PAGE_MIN_CONTRAST,
            "the pinned fill {worn:06x} must sit under the floor on the reverse cell"
        );
    }

    #[test]
    fn the_light_momentum_glow_wears_the_carets_hue_beneath_the_letters() {
        use crate::color_math::rgb2hsv;
        const PAGE: u32 = 0x00FF_FFFF;
        const CARET: u32 = 0x0009_69DA;
        const STYLE: u32 = 0x00CF_222E;
        let deepest_hue = |style: Option<GlowStyle>| {
            let (mut glow, trail) = style_cfgs(style.unwrap_or(GlowStyle::Lumen), PAGE);
            glow.enabled = style.is_some();
            glow.color = STYLE;
            let mut fx = CursorFx::default();
            let now = Instant::now();
            for _ in 0..12 {
                fx.momentum.on_key(now, MOMENTUM_GLOW_TAU_S);
            }
            fx.tick(
                &CursorFxInput {
                    cursor_style: CursorStyle::SteadyBlock,
                    live_cursor: CARET,
                    default_bg: PAGE,
                    user_tinted: style.is_some(),
                    accent_pinned: false,
                    ..frame_input(now)
                },
                &glow,
                trail,
            );
            assert!(
                fx.glow_scratch.iter().all(|q| q.alpha > 0),
                "{style:?}: no additive light on white"
            );
            let deepest = fx
                .glow
                .under_quads()
                .iter()
                .max_by_key(|q| q.alpha)
                .unwrap_or_else(|| panic!("{style:?}: the glow lies beneath the letters"));
            let a = u32::from(deepest.alpha).max(1);
            let ch = |sh: u32| (((deepest.color >> sh) & 0xff) * 255 / a).min(255);
            rgb2hsv((ch(16) << 16) | (ch(8) << 8) | ch(0)).0
        };
        let (caret_hue, _, _) = rgb2hsv(CARET);
        let off = deepest_hue(None);
        assert!(
            (off - caret_hue).abs() < 15.0,
            "off: the glow wears the caret's hue ({off:.0} vs {caret_hue:.0})"
        );
        let lit = deepest_hue(Some(GlowStyle::Lumen));
        assert!(
            (lit - off).abs() > 90.0,
            "control: a lit style's own colour ({lit:.0}) is not the caret's ({off:.0})"
        );
    }

    #[test]
    fn a_pinned_cursor_colour_is_the_caret_at_rest() {
        const PINNED: u32 = 0x00FF_3399;
        for page in [0x0011_1318u32, 0x00FF_FFFF, 0x00F6_F7F9] {
            for style in [
                GlowStyle::Comet,
                GlowStyle::Lumen,
                GlowStyle::Sparkle,
                GlowStyle::Beam,
                GlowStyle::Phaser,
                GlowStyle::RainbowKitty,
            ] {
                let run = |pinned: bool| -> (Vec<Option<u32>>, Option<u32>) {
                    let (mut glow, mut trail) = style_cfgs(style, page);
                    glow.color = PINNED;
                    glow.accent = brighten(PINNED, 1.5);
                    trail.color = PINNED;
                    let mut fx = CursorFx::default();
                    let t0 = Instant::now();
                    let input = |now, col: u16| CursorFxInput {
                        cur: Some((2, col)),
                        cursor_style: CursorStyle::SteadyBlock,
                        live_cursor: PINNED,
                        cursor_color_pinned: pinned,
                        user_tinted: !pinned,
                        accent_pinned: false,
                        default_bg: page,
                        momentum_glow: false,
                        ..frame_input(now)
                    };
                    let fill = |frame: CursorFxFrame| frame.block_fill.map(|owned| owned.fill);
                    let burst = (0..24u16)
                        .map(|k| {
                            let now = t0 + std::time::Duration::from_millis(50 * u64::from(k));
                            fx.glow.note_synthetic_typed(now, 1);
                            fx.trail.note_synthetic_typed(now);
                            fx.cadence.on_keystroke(now);
                            fill(fx.tick(&input(now, 4 + k), &glow, trail))
                        })
                        .collect();
                    let rest = t0 + std::time::Duration::from_secs(12);
                    (burst, fill(fx.tick(&input(rest, 27), &glow, trail)))
                };
                let (burst, rest) = run(true);
                let rest = rest.unwrap_or(PINNED);
                assert_eq!(
                    rest, PINNED,
                    "{style:?} on {page:06x}: the settled caret is {rest:06x}, not the pinned \
                     {PINNED:06x}"
                );
                assert!(
                    burst.iter().flatten().any(|&fill| fill != PINNED),
                    "{style:?} on {page:06x}: the burst animates around the pinned base"
                );
                let (_, unpinned) = run(false);
                assert!(
                    unpinned.is_some_and(|fill| fill != PINNED),
                    "control: {style:?} on {page:06x} rests at its own look's fill, \
                     {unpinned:06x?}"
                );
            }
        }
    }

    /// A COLOUR THE USER CHOSE IS NEVER SATURATED ON PAPER
    /// ([`crate::color_math::light_caret_fill`]): a caret charged on a light page wears a chosen
    /// colour (the trail colour, or a pinned cursor colour) darkened in its own hue and
    /// saturation, so every frame's fill keeps the pin's hue and no more than its chroma, and the
    /// charged caret still reads on the page. Sparkle pinned to `#b0b0c8` charged to navy
    /// `#000079` on paper, chroma 0.475 against the pin's 0.094 (review9fix round 1's judge,
    /// the gallery's `sparkle-pin` on GitHub Light and Default Light). Control:
    /// [`CursorFx::chosen_unread`], which reads no colour as chosen.
    #[test]
    fn a_chosen_colour_charges_in_its_own_hue_and_chroma_on_paper() {
        use crate::color_math::{contrast_ratio, rgb2hsv};
        const PIN: u32 = 0x00B0_B0C8;
        let chroma = |c: u32| {
            let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
            r.max(g).max(b) - r.min(g).min(b)
        };
        let default_light = aterm_types::scheme::builtin("Default Light")
            .expect("Default Light ships")
            .to_theme_parts()
            .bg
            & 0x00FF_FFFF;
        let run = |style: GlowStyle, page: u32, cursor_pin: bool, unread: bool| -> Vec<u32> {
            let (mut glow, mut trail) = style_cfgs(style, page);
            glow.color = PIN;
            glow.accent = brighten(PIN, 1.5);
            trail.color = PIN;
            let mut fx = CursorFx {
                chosen_unread: unread,
                ..CursorFx::default()
            };
            let t0 = Instant::now();
            let input = |now, col: u16| CursorFxInput {
                cur: Some((2, col)),
                cursor_style: CursorStyle::SteadyBlock,
                live_cursor: if cursor_pin { PIN } else { 0x0009_69DA },
                cursor_color_pinned: cursor_pin,
                user_tinted: !cursor_pin,
                accent_pinned: false,
                default_bg: page,
                momentum_glow: false,
                ..frame_input(now)
            };
            (0..24u16)
                .filter_map(|k| {
                    let now = t0 + std::time::Duration::from_millis(50 * u64::from(k));
                    fx.glow.note_synthetic_typed(now, 1);
                    fx.trail.note_synthetic_typed(now);
                    fx.cadence.on_keystroke(now);
                    fx.tick(&input(now, 4 + k), &glow, trail)
                        .block_fill
                        .map(|owned| owned.fill)
                })
                .collect()
        };
        let (pin_hue, _, _) = rgb2hsv(PIN);
        let mut control_saturated = false;
        for page in [0x00FF_FFFF, default_light] {
            for style in [
                GlowStyle::Sparkle,
                GlowStyle::Beam,
                GlowStyle::Lumen,
                GlowStyle::Comet,
                GlowStyle::Laser,
            ] {
                for cursor_pin in [false, true] {
                    let fills = run(style, page, cursor_pin, false);
                    let charged = fills.iter().copied().filter(|&f| f != PIN);
                    assert!(
                        charged.clone().count() > 0,
                        "{style:?} on {page:06x} (cursor pin {cursor_pin}): the burst charges the \
                         caret: {fills:06x?}"
                    );
                    for fill in charged.clone() {
                        let (hue, _, _) = rgb2hsv(fill);
                        assert!(
                            chroma(fill) <= chroma(PIN) + 1 && (hue - pin_hue).abs() < 6.0,
                            "{style:?} on {page:06x} (cursor pin {cursor_pin}): the charged \
                             caret {fill:06x} (chroma {}, hue {hue:.0}) recolours the chosen \
                             {PIN:06x} (chroma {}, hue {pin_hue:.0})",
                            chroma(fill),
                            chroma(PIN)
                        );
                    }
                    let deepest = charged
                        .map(|f| contrast_ratio(f, page))
                        .fold(0.0f32, f32::max);
                    assert!(
                        deepest >= CARET_PAGE_MIN_CONTRAST,
                        "{style:?} on {page:06x} (cursor pin {cursor_pin}): the charged caret \
                         reads at {deepest:.2}:1"
                    );
                    control_saturated |= run(style, page, cursor_pin, true)
                        .into_iter()
                        .any(|f| chroma(f) > 2 * chroma(PIN));
                }
            }
        }
        assert!(
            control_saturated,
            "control: concentrated as a style's own colour, some charged caret on paper wears \
             more than twice the pin's chroma"
        );
    }

    /// A COLOUR THE USER CHOSE IS NEVER RE-SATURATED BY THE TYPING-MOMENTUM GLOW
    /// ([`MomentumGlowConfig::chosen_hue`], [`MomentumGlowConfig::chosen_base`]): beside a
    /// chosen colour (Sparkle's or Classic's pinned trail colour, or a pinned cursor colour with
    /// no trail), every pixel of the paper halo keeps the pin's hue and no more than its chroma,
    /// and a caret it charges in the chosen colour keeps them on paper and on a dark page.
    /// Beside Sparkle pinned to `#b0b0c8` (chroma 0.094) the paper halo was laid up to
    /// `#c1c1ee`, chroma 0.176, its ink walked from the hue's vivid ink (review9fix round 2's
    /// judge, the gallery's `sparkle-pin` on paper), and a pinned cursor colour's charged caret
    /// went navy on paper and amber on a dark page. The three cases take the wiring's three
    /// hues (Sparkle's paper gold, a style's rod, the cursor's own). Control:
    /// [`CursorFx::momentum_chosen_unread`], the momentum glow alone reading no colour as
    /// chosen, which lays each case's paper halo off the pin.
    #[test]
    fn the_momentum_glow_wears_a_chosen_colour_in_its_own_hue_and_chroma() {
        use crate::color_math::rgb2hsv;
        const PIN: u32 = 0x00B0_B0C8;
        let chroma = |c: u32| {
            let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
            r.max(g).max(b) - r.min(g).min(b)
        };
        let (pin_hue, _, _) = rgb2hsv(PIN);
        let default_light = aterm_types::scheme::builtin("Default Light")
            .expect("Default Light ships")
            .to_theme_parts()
            .bg
            & 0x00FF_FFFF;
        // What a burst draws beside the chosen colour: the halo's pixels (paper only: a dark
        // page's halo is additive, the momentum's own warm light), its quads laid as the
        // renderer lays them over a neutral white, so a tinted page's own hue (Default Light's
        // `#f6f7f9`) does not read as the halo's, and the caret fills the momentum glow owns.
        // The halo's quads are the ones a twin with no momentum glow, fed the same frames,
        // does not lay: the momentum glow's alone, not the style's own marks under the letters.
        let run = |style: Option<GlowStyle>, page: u32, unread: bool| -> (Vec<u32>, Vec<u32>) {
            let (mut glow, mut trail) = style_cfgs(style.unwrap_or(GlowStyle::Sparkle), page);
            let cursor_pin = style.is_none();
            if cursor_pin {
                glow.enabled = false;
            } else {
                glow.color = PIN;
                glow.accent = brighten(PIN, 1.5);
                // A pinned colour selects Classic's one-colour face (`resolve_glow_config`).
                glow.classic_mono = matches!(style, Some(GlowStyle::Classic));
                trail.color = PIN;
            }
            let mut fx = CursorFx {
                momentum_chosen_unread: unread,
                ..CursorFx::default()
            };
            let mut twin = CursorFx::default();
            let t0 = Instant::now();
            let geom = frame_input(t0).geom;
            let (w, h) = (usize::from(geom.win_w), usize::from(geom.win_h));
            let (mut halo, mut carets) = (Vec::new(), Vec::new());
            for k in 0..24u16 {
                let now = t0 + std::time::Duration::from_millis(50 * u64::from(k));
                for f in [&mut fx, &mut twin] {
                    f.glow.note_synthetic_typed(now, 1);
                    f.trail.note_synthetic_typed(now);
                    f.cadence.on_keystroke(now);
                }
                fx.momentum
                    .on_key(now, crate::cursor_momentum::MOMENTUM_GLOW_TAU_S);
                let input = |momentum_glow| CursorFxInput {
                    cur: Some((2, 4 + k)),
                    cursor_style: CursorStyle::SteadyBlock,
                    live_cursor: if cursor_pin { PIN } else { 0x0009_69DA },
                    cursor_color_pinned: cursor_pin,
                    user_tinted: !cursor_pin,
                    accent_pinned: false,
                    default_bg: page,
                    momentum_glow,
                    ..frame_input(now)
                };
                if let Some(owned) = fx
                    .tick(&input(true), &glow, trail)
                    .block_fill
                    .filter(|owned| owned.owner == BlockFillOwner::Momentum)
                {
                    carets.push(owned.fill);
                }
                let _ = twin.tick(&input(false), &glow, trail);
                let (laid, style_own) = (fx.glow.under_quads(), twin.glow.under_quads());
                let (own, halo_quads) = laid.split_at(style_own.len().min(laid.len()));
                assert_eq!(
                    own, style_own,
                    "frame {k}: the momentum glow lays its halo after the style's own marks"
                );
                if aterm_render::theme_is_dark(page) {
                    continue;
                }
                const NEUTRAL: u32 = 0x00FF_FFFF;
                let mut px = vec![NEUTRAL; w * h];
                for q in halo_quads {
                    let qw = u32::from(q.w.max(1));
                    let at = |i: u32, a: u32, b: u32| {
                        (a * (2 * qw - (2 * i + 1)) + b * (2 * i + 1) + qw) / (2 * qw)
                    };
                    for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                        for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                            let i = (x - usize::from(q.x)) as u32;
                            let (color, alpha) = if q.is_flat() {
                                (q.color, q.alpha)
                            } else {
                                let lerp = |sh: u32| {
                                    at(i, (q.color >> sh) & 0xff, (q.color2 >> sh) & 0xff) << sh
                                };
                                (
                                    lerp(16) | lerp(8) | lerp(0),
                                    at(i, u32::from(q.alpha), u32::from(q.alpha2)) as u8,
                                )
                            };
                            let slot = &mut px[y * w + x];
                            *slot = aterm_render::over_premul(*slot, color, alpha);
                        }
                    }
                }
                halo.extend(px.into_iter().filter(|&p| p != NEUTRAL));
            }
            (halo, carets)
        };
        let off_pin = |c: u32| {
            let (hue, _, _) = rgb2hsv(c);
            let hue_off = (hue - pin_hue).abs().min(360.0 - (hue - pin_hue).abs());
            chroma(c) > chroma(PIN) + 1 || (chroma(c) >= 8 && hue_off >= 6.0)
        };
        let styles = [Some(GlowStyle::Sparkle), Some(GlowStyle::Classic), None];
        let mut control_halo = [0usize; 3];
        let mut control_carets = 0;
        for page in [0x00FF_FFFF, default_light, 0x0010_1016] {
            for (si, style) in styles.into_iter().enumerate() {
                let at = format!("{style:?} on {page:06x}");
                // Classic's caret wears the theme's cursor colour, which nobody chose; its halo
                // wears the trail's.
                let caret_chosen = !matches!(style, Some(GlowStyle::Classic));
                let (halo, mut carets) = run(style, page, false);
                if !aterm_render::theme_is_dark(page) {
                    assert!(!halo.is_empty(), "{at}: the burst lays a paper halo");
                }
                carets.retain(|_| caret_chosen);
                let off: Vec<String> = halo
                    .iter()
                    .chain(&carets)
                    .filter(|&&c| off_pin(c))
                    .map(|c| format!("{c:06x}"))
                    .collect();
                assert!(
                    off.is_empty(),
                    "{at}: {} momentum pixels or carets leave the pin {PIN:06x}'s hue or pass \
                     its chroma: {:?}",
                    off.len(),
                    &off[..off.len().min(8)]
                );
                if style.is_none() {
                    assert!(
                        carets.iter().any(|&c| c != PIN),
                        "{at}: the burst charges the pinned caret: {carets:06x?}"
                    );
                }
                let (halo, mut carets) = run(style, page, true);
                carets.retain(|_| caret_chosen);
                control_halo[si] += halo.iter().filter(|&&c| off_pin(c)).count();
                control_carets += carets.iter().filter(|&&c| off_pin(c)).count();
            }
        }
        assert!(
            control_halo.iter().all(|&n| n > 0) && control_carets > 0,
            "control: reading no colour as chosen, each style's paper halo ({styles:?}) and the \
             charged caret leave the pin's hue or chroma (halo pixels, carets): \
             {control_halo:?}, {control_carets}"
        );
    }

    #[test]
    fn the_rainbow_kitty_caret_reads_on_every_shipped_light_theme() {
        use crate::color_math::contrast_ratio;
        const BAR: f32 = 3.0;
        let mut light_pages = 0;
        let mut lifted_on_white = Vec::new();
        for name in aterm_types::scheme::builtin_names() {
            let parts = aterm_types::scheme::builtin(name)
                .unwrap_or_else(|| panic!("built-in theme {name} must resolve"))
                .to_theme_parts();
            let page = parts.bg & 0x00FF_FFFF;
            if aterm_render::theme_is_dark(page) {
                continue;
            }
            light_pages += 1;
            let (mut glow, trail) = style_cfgs(GlowStyle::RainbowKitty, page);
            glow.theme_fg = parts.fg & 0x00FF_FFFF;
            let mut fx = CursorFx::default();
            let t0 = Instant::now();
            let input = |now, col: u16| CursorFxInput {
                cur: Some((2, col)),
                cursor_style: CursorStyle::SteadyBlock,
                live_cursor: parts.cursor & 0x00FF_FFFF,
                default_bg: page,
                momentum_glow: false,
                ..frame_input(now)
            };
            let mut hottest = 0.0f32;
            for k in 0..25u16 {
                let now = if k < 24 {
                    let now = t0 + std::time::Duration::from_millis(50 * u64::from(k));
                    fx.glow.note_synthetic_typed(now, 1);
                    fx.cadence.on_keystroke(now);
                    now
                } else {
                    t0 + std::time::Duration::from_secs(12)
                };
                let frame = fx.tick(&input(now, 4 + k.min(23)), &glow, trail);
                let blaze = fx.glow.blaze();
                hottest = hottest.max(blaze);
                let owned = frame
                    .block_fill
                    .unwrap_or_else(|| panic!("{name}: the rainbow owns the caret, frame {k}"));
                assert_eq!(owned.owner, BlockFillOwner::Rainbow, "{name} frame {k}");
                let ratio = contrast_ratio(owned.fill, page);
                assert!(
                    ratio >= BAR,
                    "{name} ({page:06x}) frame {k}: caret {:06x} at {ratio:.2}:1",
                    owned.fill
                );
                if page == 0x00FF_FFFF && k < 24 && blaze > 0.5 {
                    let lifted = crate::cursor_rainbow::lift_to_light_floor(
                        owned.fill,
                        crate::cursor_glow::RAINBOW_CARET_LIGHT_FLOOR,
                    );
                    lifted_on_white.push(contrast_ratio(lifted, page));
                }
            }
            assert!(
                hottest > 0.8,
                "{name}: the burst never ran hot (blaze {hottest:.2})"
            );
        }
        assert!(
            light_pages >= 3,
            "only {light_pages} shipped light themes found"
        );
        assert!(
            !lifted_on_white.is_empty() && lifted_on_white.iter().all(|&r| r < BAR),
            "control: the white lift must sink the caret under {BAR}:1 on white, got \
             {lifted_on_white:?}"
        );
    }

    #[test]
    fn the_momentum_tint_cools_onto_the_plain_cursor_without_a_step() {
        let run = |page: u32, cursor: u32| {
            let glow = GlowConfig {
                theme_fg: 0x0024_292F,
                theme_bg: page,
                dark_theme: false,
                ..classic_cfg(false).0
            };
            let trail = classic_cfg(false).1;
            let mut fx = CursorFx::default();
            let t0 = Instant::now();
            for k in 0..24u64 {
                fx.momentum.on_key(
                    t0 + std::time::Duration::from_millis(k * 40),
                    MOMENTUM_GLOW_TAU_S,
                );
            }
            let input = |now| CursorFxInput {
                cursor_style: CursorStyle::SteadyBlock,
                live_cursor: cursor,
                default_bg: page,
                ..frame_input(now)
            };
            let (mut peak, mut last_hot) = (None, None);
            for i in 0..2000u64 {
                let now = t0 + std::time::Duration::from_millis(23 * 40 + 5 * i);
                let frame = fx.tick(&input(now), &glow, trail);
                match frame.block_fill {
                    Some(owned) => {
                        assert_eq!(owned.owner, BlockFillOwner::Momentum);
                        peak.get_or_insert(owned.fill);
                        last_hot = Some(owned.fill);
                    }
                    None if last_hot.is_some() => break,
                    None => {}
                }
            }
            (
                peak.expect("a fast hand heats the momentum tint"),
                last_hot.expect("the tint cools to cold inside the window"),
            )
        };
        let (_, edge) = run(0x00EF_F1F5, 0x00DC_8A78);
        let step = [16u32, 8, 0]
            .iter()
            .map(|sh| ((edge >> sh) & 0xff).abs_diff((0x00DC_8A78 >> sh) & 0xff))
            .max()
            .unwrap_or(0);
        assert!(
            step <= 2,
            "the last hot frame {edge:06x} must be the plain cursor DC8A78 (step {step})"
        );
        let (peak, _) = run(0x00FF_FFFF, 0x0009_69DA);
        assert!(
            crate::color_math::contrast_ratio(peak, 0x00FF_FFFF) >= CARET_PAGE_MIN_CONTRAST,
            "control: the hot tint {peak:06x} on GitHub Light keeps the page floor"
        );
    }

    #[test]
    fn the_resting_forge_ember_is_floored_on_the_dark_page() {
        const PAGE: u32 = 0x0011_1318;
        let rest = |solved_for: u32| {
            let (glow, trail) = style_cfgs(GlowStyle::Fire, solved_for);
            let mut fx = CursorFx::default();
            let frame = fx.tick(
                &CursorFxInput {
                    cursor_style: CursorStyle::SteadyBlock,
                    default_bg: PAGE,
                    ..frame_input(Instant::now())
                },
                &glow,
                trail,
            );
            let owned = frame.block_fill.expect("the forge owns the fire caret");
            assert_eq!(owned.owner, BlockFillOwner::Forge);
            let raw = fx.glow.forge_fill().expect("the forge always has a colour");
            (owned.fill, raw)
        };
        let (fill, raw) = rest(PAGE);
        assert!(
            crate::color_math::contrast_ratio(fill, PAGE) >= CARET_PAGE_MIN_CONTRAST,
            "the ember {fill:06x} must clear the page floor"
        );
        let (r, g, b) = ((fill >> 16) & 0xff, (fill >> 8) & 0xff, fill & 0xff);
        assert!(r > g && g >= b, "still an ember, got {fill:06x}");
        assert_eq!(
            fill, raw,
            "the forge band already rests at the floor, so the floor keeps its ember"
        );
        let (lifted, dull) = rest(0x00FF_FFFF);
        assert!(
            crate::color_math::contrast_ratio(dull, PAGE) < 2.0,
            "control: the light page's resting ember {dull:06x} is under 2:1 on the dark page"
        );
        assert!(
            crate::color_math::contrast_ratio(lifted, PAGE) >= CARET_PAGE_MIN_CONTRAST,
            "control: the tick floors that ember ({dull:06x} drew {lifted:06x})"
        );
        let (r, g, b) = ((lifted >> 16) & 0xff, (lifted >> 8) & 0xff, lifted & 0xff);
        assert!(
            r > g && g >= b,
            "the floored control is still an ember, got {lifted:06x}"
        );
    }

    #[test]
    fn the_bolt_whitens_on_dark_and_deepens_on_light() {
        use crate::color_math::relative_luminance;
        let violet = crate::cursor_glow::LASER_DEFAULT_COLOR;
        assert_eq!(bolt_body(violet, 0.0, true, false), violet);
        let hot_dark = bolt_body(violet, 1.0, true, false);
        assert!(relative_luminance(hot_dark) > relative_luminance(violet));
        let hot_light = bolt_body(violet, 1.0, false, false);
        assert!(relative_luminance(hot_light) < relative_luminance(violet));
        assert!(
            crate::color_math::contrast_ratio(hot_light, 0x00FF_FFFF) >= 3.0,
            "the charged light bolt {hot_light:06x} keeps 3:1 on white"
        );
    }

    #[test]
    fn the_frame_colour_step_applies_the_law() {
        use crate::cursor_glow::{
            BEAM_DEFAULT_COLOR, COMET_DEFAULT_COLOR, LASER_DEFAULT_COLOR, SPARKLE_DEFAULT_COLOR,
        };
        let live = 0x0050_FA7B;
        let stale = 0x0012_1212;
        let step = |style: GlowStyle, pinned: bool| {
            let (mut glow, mut trail) = classic_cfg(true);
            glow.style = style;
            glow.color = stale;
            trail.color = stale;
            follow_live_cursor(
                &mut glow,
                &mut trail,
                live,
                pinned,
                TrailColor::Automatic,
                false,
                None,
            );
            (glow, trail)
        };
        for (style, unpinned) in [
            (GlowStyle::Lumen, live),
            (GlowStyle::Comet, COMET_DEFAULT_COLOR),
            (GlowStyle::Beam, BEAM_DEFAULT_COLOR),
            (GlowStyle::Sparkle, SPARKLE_DEFAULT_COLOR),
            (GlowStyle::Laser, LASER_DEFAULT_COLOR),
        ] {
            for (pinned, want) in [
                (false, unpinned),
                (
                    true,
                    if style == GlowStyle::Laser {
                        LASER_DEFAULT_COLOR
                    } else {
                        live
                    },
                ),
            ] {
                let (glow, trail) = step(style, pinned);
                assert_eq!(glow.color, want, "{style:?} pinned={pinned}");
                assert_eq!(trail.color, glow.color, "{style:?} pinned={pinned}");
                assert_eq!(
                    glow.accent,
                    brighten(want, 1.5),
                    "{style:?} pinned={pinned}"
                );
            }
        }
        for style in [
            GlowStyle::Lumen,
            GlowStyle::Phaser,
            GlowStyle::RainbowKitty,
            GlowStyle::Sparkle,
            GlowStyle::Fire,
            GlowStyle::Laser,
            GlowStyle::Beam,
            GlowStyle::Water,
            GlowStyle::Comet,
            GlowStyle::Classic,
        ] {
            assert_eq!(
                canonical_glow_color(style).is_some(),
                canonical_glow_color_name(style).is_some(),
                "{style:?}: a hue of its own has a name"
            );
            for pinned in [false, true] {
                let own = own_trail_color(style, pinned);
                assert_eq!(
                    own.is_none(),
                    glow_color_follows_live_cursor(style, pinned),
                    "{style:?} pinned={pinned}"
                );
                if let Some((rgb, _)) = own {
                    assert_eq!(Some(rgb), canonical_glow_color(style), "{style:?}");
                }
            }
        }
        let (mut glow, mut trail) = classic_cfg(true);
        glow.style = GlowStyle::Comet;
        glow.color = 0x00E0_218A;
        trail.color = 0x00E0_218A;
        let accent = glow.accent;
        follow_live_cursor(
            &mut glow,
            &mut trail,
            live,
            true,
            TrailColor::Pinned(0x00E0_218A),
            false,
            None,
        );
        assert_eq!((glow.color, trail.color), (0x00E0_218A, 0x00E0_218A));
        assert_eq!(
            glow.accent, accent,
            "a style drawing its colour keeps its accent"
        );
        for style in [
            GlowStyle::Fire,
            GlowStyle::Water,
            GlowStyle::Phaser,
            GlowStyle::RainbowKitty,
        ] {
            let (mut glow, mut trail) = classic_cfg(true);
            glow.style = style;
            glow.color = 0x00E0_218A;
            follow_live_cursor(
                &mut glow,
                &mut trail,
                live,
                false,
                TrailColor::Pinned(0x00E0_218A),
                false,
                None,
            );
            assert_eq!(glow.color, 0x00E0_218A, "{style:?}");
            assert_eq!(glow.accent, brighten(live, 1.5), "{style:?}");
        }
        let (mut glow, mut trail) = classic_cfg(true);
        glow.style = GlowStyle::Lumen;
        glow.accent = 0x0012_3456;
        follow_live_cursor(
            &mut glow,
            &mut trail,
            live,
            false,
            TrailColor::Automatic,
            true,
            None,
        );
        assert_eq!((glow.color, glow.accent), (live, 0x0012_3456));
        for style in [GlowStyle::Laser, GlowStyle::Classic, GlowStyle::Comet] {
            for pinned in [false, true] {
                let (mut glow, mut trail) = classic_cfg(true);
                glow.style = style;
                glow.color = stale;
                trail.color = stale;
                follow_live_cursor(
                    &mut glow,
                    &mut trail,
                    live,
                    pinned,
                    TrailColor::Cursor,
                    false,
                    None,
                );
                assert_eq!((glow.color, trail.color), (live, live), "{style:?}");
                assert_eq!(glow.accent, brighten(live, 1.5), "{style:?}");
            }
        }
        assert_eq!(TrailColor::Cursor.knob(0x0112_3456), Some(0x0012_3456));
        assert_eq!(TrailColor::Automatic.knob(live), None);
        assert!(TrailColor::Cursor.chosen() && !TrailColor::Automatic.chosen());
    }

    #[test]
    fn the_unset_accent_brightens_the_base_a_window_draws() {
        let (theme_cursor, live, chosen) = (0x0009_69DA, 0x0050_FA7B, 0x00E0_218A);
        let styles = [
            GlowStyle::Lumen,
            GlowStyle::Phaser,
            GlowStyle::RainbowKitty,
            GlowStyle::Sparkle,
            GlowStyle::Fire,
            GlowStyle::Laser,
            GlowStyle::Beam,
            GlowStyle::Water,
            GlowStyle::Comet,
            GlowStyle::Classic,
        ];
        let mut bases = std::collections::HashSet::new();
        for (style, pinned) in styles
            .into_iter()
            .flat_map(|style| [(style, false), (style, true)])
        {
            for setting in [
                TrailColor::Automatic,
                TrailColor::Cursor,
                TrailColor::Pinned(chosen),
            ] {
                let knobs = GlowKnobs {
                    enabled: true,
                    color: setting.knob(theme_cursor),
                    accent: None,
                    duration_ms: 260,
                    length: 24,
                    intensity: 1.0,
                    radius: 0.6,
                    ring: true,
                };
                let spelling = GlowSpelling {
                    style: Some(style),
                    beam: false,
                    ribbon_tall: true,
                    ribbon_flat: false,
                    classic_mono: false,
                };
                let mut glow = resolve_glow_config(
                    knobs,
                    spelling,
                    theme_cursor,
                    true,
                    0x00C8_D3F5,
                    0x001A_1B26,
                    0.5,
                );
                let (_, mut trail) = classic_cfg(true);
                follow_live_cursor(&mut glow, &mut trail, live, pinned, setting, false, None);
                let base = unset_accent_base(style, setting, pinned);
                let expected = match base {
                    AccentBase::Cursor => live,
                    AccentBase::Own(own, _) => own,
                    AccentBase::Trail(color) => color,
                };
                assert_eq!(
                    glow.accent,
                    brighten(expected, 1.5),
                    "{style:?} under {setting:?} pinned={pinned}: {base:?}"
                );
                bases.insert(std::mem::discriminant(&base));
            }
        }
        assert_eq!(bases.len(), 3, "every base is drawn by some style");
    }
}
