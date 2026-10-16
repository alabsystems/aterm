// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cursor family's frame step: the aurora ([`CursorGlow`]), the cadence
//! comet ([`CursorTrail`]) and the block-cursor bodies (rainbow block, water
//! droplet, typing-momentum glow, comet nucleus, phaser emitter, light rod,
//! laser bolt), ticked in one order with one fill-precedence law deciding who
//! owns the caret.
//!
//! Every host calls [`CursorFx::tick`] (`docs/DESIGN-host-boundary-2026-08-30.md`).
//! The host resolves its policy (motion, focus, the load-shed envelope, Serious
//! Mode, config) into a [`CursorFxInput`] and a folded
//! [`GlowConfig`]/[`TrailConfig`], feeds the glow engine its per-frame
//! observations (row probe, print anchor, delivered inserts) between
//! [`CursorFx::begin`] and the tick, and drains the engine's cues (sound, the
//! cat's motion pulse) after it.
//!
//! Clockless: `now` is injected; nothing here reads a clock, a file or a
//! platform type.

use aterm_core::terminal::CursorStyle;
use aterm_render::{GlowQuad, TrailCell};
use aterm_time::Instant;

use crate::cursor_beam::{BeamRodConfig, CursorBeamRod};
use crate::cursor_comet::{CometConfig, CursorComet};
use crate::cursor_droplet::{CursorDroplet, DropletConfig};
use crate::cursor_glow::{
    BlockFill, BlockFillBase, BlockFillOwner, CursorGlow, Geom, GlowConfig, GlowStyle,
};
use crate::cursor_momentum::{
    MOMENTUM_GLOW_RADIUS_CELLS, MOMENTUM_GLOW_TAU_S, MomentumGlow, MomentumGlowConfig,
};
use crate::cursor_phaser::{CursorPhaser, PhaserConfig};
use crate::cursor_rainbow::{CursorRainbow, RainbowConfig};
use crate::cursor_trail::{CursorTrail, TrailConfig, TypingCadence};

/// The cursor family's engines and per-frame scratch, one per drawing surface
/// (a native window, a web page); holds no flash budget.
///
/// Fields are public because the host feeds the engines between frames (a
/// typed glyph, a backspace, a repaint blink, a scroll) and reads them for its
/// sensors (`trail status`, `tone`); the frame step itself is [`Self::tick`].
#[derive(Default)]
pub struct CursorFx {
    /// The aurora — every style's light, the rainbow kitty ribbon included.
    pub glow: CursorGlow,
    /// The cadence comet — the directional `TrailCell` body of the `comet`
    /// style (disabled for every other style).
    pub trail: CursorTrail,
    /// Typing-cadence heat: comet ignition and the rainbow/phaser block energy.
    /// Fed by the host's input path (`on_keystroke`); sampled once per tick.
    pub cadence: TypingCadence,
    /// The `rainbow kitty` block body.
    pub rainbow: CursorRainbow,
    /// The `water` block body.
    pub droplet: CursorDroplet,
    /// The typing-momentum glow — every style, every shape, unless the rainbow
    /// owns the caret.
    pub momentum: MomentumGlow,
    /// The light rod — the bar shape of every style, and the emitter block of
    /// the styles with no bespoke body.
    pub beamrod: CursorBeamRod,
    /// The `comet` block body.
    pub comet: CursorComet,
    /// The `phaser` block body.
    pub phaser: CursorPhaser,
    /// The additive light this tick produced — the aurora first, then every
    /// body's halo appended in tick order. Cleared by the aurora's tick.
    pub glow_scratch: Vec<GlowQuad>,
    /// The comet cells this tick produced, already projected through the
    /// load-shed envelope.
    pub trail_scratch: Vec<TrailCell>,
}

/// Everything one [`CursorFx::tick`] reads besides the two folded configs —
/// the host's per-frame facts and its resolved policy, as plain data.
#[derive(Clone, Copy)]
pub struct CursorFxInput {
    /// The animation clock (one `Instant` per frame).
    pub now: Instant,
    /// The caret cell the engines chase, `None` while the viewport shows
    /// history. The native host does not fold a DECTCEM hide in here; the web
    /// host does (its snapshot drops the caret).
    pub cur: Option<(u16, u16)>,
    /// The frame presents the active grid, not retained scrollback.
    pub live_viewport: bool,
    /// The live cursor shape (a body needs a BLOCK; the rod takes a BAR).
    pub cursor_style: CursorStyle,
    /// This window's blink phase (unused by the rainbow body; kept on its tick
    /// signature).
    pub blink_phase: bool,
    /// The effective live cursor colour, `0x00RRGGBB`: OSC 12 when set, else
    /// the configured/theme cursor colour, else the live foreground.
    pub live_cursor: u32,
    /// Whether that colour was pinned (by config or a program's OSC 12) rather
    /// than the theme's seed. A pinned colour is the rainbow/phaser block's
    /// base; an unpinned caret builds from white (owner, 2026-08-29).
    pub cursor_color_pinned: bool,
    /// The live default background (DECSCNM-folded), `0x00RRGGBB`: the
    /// ground every body solves its light against.
    pub default_bg: u32,
    /// The window-space effect geometry (grid origin, frame extent).
    pub geom: Geom,
    /// The cursor-effect focus fold (OS focus, the recording pin, the typed
    /// wake) — the `focused=` `trail status` prints.
    pub focused: bool,
    /// The motion policy's amplitude for the cursor family (0 under reduced
    /// motion or unfocused demotion); never a load term. Whether the family may
    /// animate at all is [`CursorFx::begin`]'s argument.
    pub amplitude: f32,
    /// The SOFT load-shed envelope, `0..=1`: applied to the bodies' amplitude
    /// and, after the tick, to the comet's presentation and the caret fill.
    pub shed_envelope: f32,
    /// Serious Mode's `CursorBody` allowance. The live viewport and a present
    /// caret are folded in here, by the step.
    pub body_allowed: bool,
    /// The cursor-effects master (native `cursor_trail`; a page's glow switch),
    /// distinct from the trail style: `cursor_trail_style = "off"` under a
    /// master that is on draws no trail, but the typing-momentum glow (on for
    /// every style) still lights.
    pub master: bool,
    /// The user's typing-momentum glow switch (`cursor_momentum_glow`).
    pub momentum_glow: bool,
    /// The user pinned an explicit trail colour (`cursor_trail_color`): the
    /// rod then wears it instead of the style's signature shade.
    pub user_tinted: bool,
}

/// What one [`CursorFx::tick`] produced for the host's frame; the quads are in
/// [`CursorFx::glow_scratch`] / [`CursorFx::trail_scratch`].
#[derive(Clone, Copy, Debug)]
pub struct CursorFxFrame {
    /// The aurora + every body's fingerprint + the final caret fill, folded
    /// (0 when idle-empty).
    pub glow_fp: u64,
    /// The comet's presentation fingerprint (0 when idle-empty).
    pub trail_fp: u64,
    /// The (OSC-12-rewired, ignition heat-blended) comet colour the presented
    /// cells render at.
    pub trail_color: u32,
    /// The caret body this tick resolved through the fill-precedence law and
    /// the load-shed projection. It becomes status truth only at the host's
    /// final projection seam (a focus outline, a composed clip).
    pub block_fill: Option<BlockFill>,
    /// ⚡ The laser BOLT owns the block: the caret's shape is `Bolt`.
    pub bolt_cursor: bool,
    /// 🌟 The rainbow owns a BLINKING block: the shape is pinned steady.
    pub twinkle_cursor: bool,
    /// ✨ The momentum glow is HOT: this Blinking* style pinned to its Steady*
    /// twin (`None` when cold, or when the style does not blink).
    pub momentum_steady: Option<CursorStyle>,
}

/// Where the streak's head attaches in its cell: a thin bar's insertion point,
/// else the cell centre. With a bar shape (DECSCUSR bar / `cursor_style beam`)
/// the light noses into the bar instead of overshooting it by half a cell.
#[must_use]
pub fn head_dx_for(style: CursorStyle) -> f32 {
    if matches!(style, CursorStyle::BlinkingBar | CursorStyle::SteadyBar) {
        0.08
    } else {
        0.5
    }
}

/// The Fire style's decorative cursor-body fill, through the same gates in
/// every render path. Lazy, so a disabled effect never samples forge state.
pub fn forge_cursor_fill(
    cursor_body_allowed: bool,
    glow_cfg: &GlowConfig,
    fill: impl FnOnce() -> Option<u32>,
) -> Option<u32> {
    (cursor_body_allowed
        && glow_cfg.enabled
        && glow_cfg.intensity > 0.0
        && matches!(glow_cfg.style, GlowStyle::Fire))
    .then(fill)
    .flatten()
}

/// The cursor-body fills one tick can produce (plus the momentum tint), in the
/// order the frame path splices them into `RenderInput::cursor_fill_override`.
/// At most one style body is `Some` (their styles are mutually exclusive); the
/// order matches the splice so an overlap would report the actual winner.
#[derive(Clone, Copy, Debug, Default)]
pub struct BlockFillSplice {
    pub rainbow: Option<u32>,
    pub forge: Option<u32>,
    pub phaser: Option<u32>,
    pub bolt: Option<u32>,
    pub comet: Option<u32>,
    pub droplet: Option<u32>,
    pub beamrod: Option<u32>,
    /// The typing-momentum tint — LAST, so a style body always wins the caret.
    pub momentum: Option<u32>,
}

/// The blink law, by who owns the caret. Precedence, first wins
/// (owner, 2026-09-08):
///
/// 1. the bolt (the `laser` body's own shape);
/// 2. the rainbow owns the caret (`twinkle_cursor`: the `rainbow kitty` block
///    resolved a fill on a blinking block): pinned `SteadyBlock`
///    unconditionally, since its halo, rim and spin already encode momentum;
/// 3. the momentum glow is hot on any other style: a Blinking* style is pinned
///    to its Steady* twin while warm (`momentum_steady`), and `None` once cool,
///    so the configured blink returns.
///
/// Composed only by ordering. One function for every site that spells the
/// caret's shape (every native render path, the introspection mirror, the web
/// pipeline), so a capture cannot contradict the glass.
#[must_use]
pub fn compose_caret_style_override(
    bolt: bool,
    rainbow_owns_caret: bool,
    momentum_steady: Option<CursorStyle>,
) -> Option<CursorStyle> {
    if bolt {
        Some(CursorStyle::Bolt)
    } else if rainbow_owns_caret {
        Some(CursorStyle::SteadyBlock)
    } else {
        // A hot cursor does not blink; a cool one is exactly the configured
        // cursor (owner, 2026-09-08).
        momentum_steady
    }
}

/// Whether the typing-momentum glow engine runs this frame: the user's switch
/// (default on) and the host's gates (serious mode, focus, live viewport,
/// folded into `host_allows`), and not while the rainbow owns the caret. The
/// rainbow already encodes momentum in its rim and halo, so the momentum
/// engine gets `enabled: false`, paints nothing and reports `hot: false`.
#[must_use]
pub fn momentum_glow_allowed(user_on: bool, host_allows: bool, rainbow_owns_caret: bool) -> bool {
    user_on && host_allows && !rainbow_owns_caret
}

/// Who owns the block cursor's body this frame and the colour they built it
/// from: the reading behind `trail status`'s `block_fill=…` fields.
///
/// A body effect's fill becomes `RenderInput::cursor_fill_override`, which
/// `draw_cursor` and its GPU twin apply instead of `frame_cursor(input)`, so
/// while it is `Some` the caret is not the terminal's cursor colour.
///
/// Two colours: `fill` is what the override carried to the renderer; `base` is
/// what the host handed that owner to build from (the resolved cursor or trail
/// colour, per [`BlockFillOwner::base_from`]; `None` for owners whose style is
/// their colour). A body that ignores its base shows as the two disagreeing.
#[must_use]
pub fn resolve_block_fill(
    fills: BlockFillSplice,
    cursor_color: u32,
    trail_color: u32,
    cursor_base_pinned: bool,
) -> Option<BlockFill> {
    let owned = |owner: BlockFillOwner, fill: Option<u32>| {
        fill.map(|fill| BlockFill {
            owner,
            fill,
            base: match owner.base_from() {
                // Report what was handed: a `CursorColor` owner with no pinned
                // colour was handed `None` and built from white, so its base is
                // `None` here too.
                BlockFillBase::CursorColor => cursor_base_pinned.then_some(cursor_color),
                BlockFillBase::TrailColor => Some(trail_color),
                // Handed nothing, and the row says so: the identity ramp is
                // the whole story for these two.
                BlockFillBase::StyleIdentity | BlockFillBase::White => None,
            },
        })
    };
    owned(BlockFillOwner::Rainbow, fills.rainbow)
        .or_else(|| owned(BlockFillOwner::Forge, fills.forge))
        .or_else(|| owned(BlockFillOwner::Phaser, fills.phaser))
        .or_else(|| owned(BlockFillOwner::Bolt, fills.bolt))
        .or_else(|| owned(BlockFillOwner::Comet, fills.comet))
        .or_else(|| owned(BlockFillOwner::Droplet, fills.droplet))
        .or_else(|| owned(BlockFillOwner::BeamRod, fills.beamrod))
        .or_else(|| owned(BlockFillOwner::Momentum, fills.momentum))
}

/// Project the resolved effect-owned cursor body through adaptive shedding.
///
/// Runs after owner precedence, so every body and presentation path shares one
/// fade law. The endpoint is always the terminal cursor colour (`BlockFill::base`
/// is provenance and may be a pinned trail colour the renderer does not
/// restore). At exact zero the override disappears. Accessibility and Serious
/// Mode are hard zeros because those gates produce no candidate.
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
    candidate.map(|mut owned| {
        owned.fill = aterm_render::blend_rgb(
            terminal_cursor_color,
            owned.fill,
            (envelope * f32::from(u8::MAX)).round() as u8,
        );
        owned
    })
}

/// What a host resolved from a trail-style spelling. The native app resolves it
/// once per config generation against its alias table and Trail Pack catalog;
/// a page parses a raw spelling with [`GlowSpelling::parse`].
#[derive(Clone, Copy, Debug)]
pub struct GlowSpelling {
    /// The style, or `None` for the resolutions that genuinely mean "draw
    /// nothing" (`off`, a `pack:` naming no loaded pack).
    pub style: Option<GlowStyle>,
    /// The resolved Trail Pack when `style` is [`GlowStyle::Custom`].
    pub pack: Option<crate::cursor_glow::TrailParams>,
    /// The additive beam ([`crate::cursor_glow::style_has_beam_of`]).
    pub beam: bool,
    /// The v0.43 full-height ribbon body (every spelling but `… underline`).
    pub ribbon_tall: bool,
    /// The explicit `… flat` ribbon body.
    pub ribbon_flat: bool,
    /// The classic wake's theme-following face (`classic mono`).
    pub classic_mono: bool,
}

impl GlowSpelling {
    /// A page's reading of a raw spelling: [`GlowStyle::parse`] (an unknown
    /// spelling reads as Lumen) and the spelling forks, with no pack catalog.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let style = GlowStyle::parse(raw);
        Self {
            style: Some(style),
            pack: None,
            beam: crate::cursor_glow::style_has_beam(raw),
            ribbon_tall: !GlowStyle::style_names_underline_ribbon(raw),
            ribbon_flat: GlowStyle::style_names_flat_ribbon(raw),
            classic_mono: GlowStyle::style_names_classic_mono(raw),
        }
    }
}

/// The numeric knobs of the cursor family's config (native `cursor_trail_*`,
/// a page's `set_cursor_glow` arguments).
#[derive(Clone, Copy, Debug)]
pub struct GlowKnobs {
    /// The master (with Serious Mode folded in by the host).
    pub enabled: bool,
    /// An explicit trail colour; `None` takes the style's default.
    pub color: Option<u32>,
    /// An explicit accent; `None` brightens the colour 1.5×.
    pub accent: Option<u32>,
    pub duration_ms: u64,
    pub length: usize,
    pub intensity: f32,
    pub radius: f32,
    pub ring: bool,
}

/// A non-finite knob is OFF, never NaN in the engine; a finite one clamps.
fn finite_clamp_or_off(value: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        0.0
    }
}

/// Brighten a packed `0x00RRGGBB` by `factor` (the auto-accent law).
pub fn brighten(color: u32, factor: f32) -> u32 {
    let channel = |shift: u32| ((((color >> shift) & 0xff) as f32) * factor).min(255.0) as u32;
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

/// The cursor family's config law: the one construction of a [`GlowConfig`]
/// from a spelling and knobs, for every host (native window, its Settings
/// preview, the web pipeline). The live terminal differs from a preview only
/// in the injected theme/geometry facts (`dark_theme`, the ground pair,
/// `head_dx`), which the frame path folds per frame.
///
/// Defaults: Laser storm violet, Sparkle starlight gold, Beam and Comet their
/// canonical hues, every other style the theme's cursor colour; an explicit
/// colour overrides any. `beam` is the tube alone (`radius` 0, no ring).
/// `audible` is `false` at construction: only a frame knows whose window a key
/// lands in (native `sound_policy::fold_window_audibility`).
#[must_use]
pub fn resolve_glow_config(
    knobs: GlowKnobs,
    spelling: GlowSpelling,
    theme_cursor: u32,
    dark_theme: bool,
    theme_fg: u32,
    theme_bg: u32,
    head_dx: f32,
) -> GlowConfig {
    use crate::cursor_glow::{
        BEAM_DEFAULT_COLOR, COMET_DEFAULT_COLOR, LASER_DEFAULT_COLOR, SPARKLE_DEFAULT_COLOR,
    };
    // Off / missing-pack values carry a concrete enum because GlowConfig is
    // POD; `enabled = false` is the sole engine gate.
    let glow_style = spelling.style.unwrap_or(GlowStyle::Lumen);
    let default_color = match glow_style {
        GlowStyle::Laser => LASER_DEFAULT_COLOR,
        GlowStyle::Beam => BEAM_DEFAULT_COLOR,
        GlowStyle::Comet => COMET_DEFAULT_COLOR,
        GlowStyle::Sparkle => SPARKLE_DEFAULT_COLOR,
        _ => theme_cursor & 0x00ff_ffff,
    };
    let color = knobs.color.unwrap_or(default_color) & 0x00ff_ffff;
    let accent = knobs
        .accent
        .map(|value| value & 0x00ff_ffff)
        .unwrap_or_else(|| brighten(color, 1.5));
    let beam_only = glow_style == GlowStyle::Beam;
    GlowConfig {
        theme_fg,
        theme_bg,
        enabled: knobs.enabled && spelling.style.is_some(),
        style: glow_style,
        color,
        accent,
        duration: std::time::Duration::from_millis(knobs.duration_ms.clamp(30, 2_000)),
        length: knobs.length.clamp(1, 512),
        intensity: finite_clamp_or_off(knobs.intensity, 0.0, 1.0),
        audible: false,
        radius: if beam_only {
            0.0
        } else {
            finite_clamp_or_off(knobs.radius, 0.0, 2.0)
        },
        ring: !beam_only && knobs.ring,
        dark_theme,
        beam: spelling.beam,
        head_dx,
        pack: spelling.pack,
        ribbon_tall: spelling.ribbon_tall,
        ribbon_flat: spelling.ribbon_flat,
        classic_mono: spelling.classic_mono,
    }
}

/// The colour `style` keeps for its own when `cursor_trail_color` is unset,
/// and its plain name: Laser's storm violet. Every other style's automatic
/// colour follows the live cursor colour ([`follow_live_cursor`]), so
/// Settings can say which one a row's default is.
#[must_use]
pub fn own_trail_color(style: GlowStyle) -> Option<(u32, &'static str)> {
    matches!(style, GlowStyle::Laser).then_some((crate::cursor_glow::LASER_DEFAULT_COLOR, "Violet"))
}

/// AUTOMATIC TRAIL COLOURS FOLLOW THE LIVE CURSOR — the one step every host
/// runs between [`resolve_glow_config`] and the tick (a native window, its
/// Settings preview), so a preview draws the colour the terminal does. With
/// no explicit `cursor_trail_color` (`color_pinned` false) the glow and the
/// comet body take the live cursor colour (OSC 12, else the theme's), and,
/// with no explicit accent, a brightened accent; a style with a colour of
/// its own ([`own_trail_color`]) keeps it, and an explicit colour stays.
pub fn follow_live_cursor(
    glow: &mut GlowConfig,
    trail: &mut TrailConfig,
    live_cursor: u32,
    color_pinned: bool,
    accent_pinned: bool,
) {
    if color_pinned || own_trail_color(glow.style).is_some() {
        return;
    }
    let live = live_cursor & 0x00FF_FFFF;
    glow.color = live;
    trail.color = live;
    if !accent_pinned {
        glow.accent = brighten(live, 1.5);
    }
}

/// A body's additive light on a light ground (the comet's coma, the beam's
/// rod): it cannot show on the paper and would only bleach the letters and
/// the trail's ink around the caret, so the quads it appended from `from` on
/// are dropped there; the body's block fill is kept.
fn drop_additive_from(scratch: &mut Vec<GlowQuad>, from: usize) {
    let mut keep = from;
    for i in from..scratch.len() {
        if scratch[i].alpha > 0 {
            scratch.swap(keep, i);
            keep += 1;
        }
    }
    scratch.truncate(keep);
}

/// A BLOCK cursor shape — the one every block body needs.
#[inline]
fn is_block(style: CursorStyle) -> bool {
    matches!(style, CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock)
}

impl CursorFx {
    /// The per-frame preamble, before the host feeds the glow engine its row
    /// probe, print anchor and delivered inserts:
    ///
    /// * **the history fence**: cursor effects live in active-grid coordinates
    ///   and a history viewport shows other rows, so both engines retire before
    ///   projecting anything, except Rainbow Kitty, which is hidden (its clocks
    ///   run and nothing is written, so the band is where it should be when the
    ///   live viewport returns);
    /// * **the fresh-ink reduced-motion arm**: the motion policy folded into the
    ///   engine's step-fade seam.
    pub fn begin(&mut self, live_viewport: bool, animate: bool) {
        if !live_viewport {
            if !self.glow.hide_v2() {
                self.glow.reset();
            }
            self.trail.reset();
        }
        self.glow.set_reduced_motion(!animate);
    }

    /// Retire every body engine, the comet, the momentum rate and the typing
    /// cadence: Serious Mode's cursor drain, and the web pipeline's all-off arm
    /// and hidden-page edge. The aurora is the caller's (its drain is a `reset`
    /// that keeps cumulative sensors).
    pub fn retire_bodies(&mut self) {
        self.retire_body_geometry();
        self.momentum.reset();
        self.cadence = TypingCadence::default();
    }

    /// The caret's coordinate space was replaced (re-grid, new owner, alternate
    /// screen swap, torn frame): every body's caret geometry and the comet's
    /// cells retire with this frame's scratch. The aurora (whose law differs by
    /// seam), the cadence and the momentum rate (facts about the hand) are the
    /// caller's.
    pub fn retire_body_geometry(&mut self) {
        self.trail.reset();
        self.rainbow = CursorRainbow::default();
        self.droplet = CursorDroplet::default();
        self.beamrod = CursorBeamRod::default();
        self.comet = CursorComet::default();
        self.phaser = CursorPhaser::default();
        self.glow_scratch.clear();
        self.trail_scratch.clear();
    }

    /// Content-scroll law, whole-plane arm: the content moved up by `rows`, so
    /// both engines translate their retained light with the text (earned light
    /// leaves by decay, translation or reset, never by someone else's output).
    /// The row probe is fenced so pre-scroll bytes never compare against the
    /// new row.
    pub fn scroll_translate(&mut self, rows: u16) {
        self.glow.note_scroll(rows);
        self.trail.note_scroll(rows);
        self.glow.drop_row_probe();
    }

    /// Content-scroll law, row-band arm: every invalidating batch since the last
    /// snapshot was explained by the `count` row-band moves from `first_seq`
    /// (an inline viewport's region scroll), replayed oldest first. The row
    /// probe is fenced as under a whole-plane translation.
    pub fn scroll_bands(
        &mut self,
        current: &aterm_core::terminal::ContentScrollState,
        first_seq: u64,
        count: u8,
    ) {
        for i in 0..u64::from(count) {
            let m = current.band(first_seq + i);
            self.glow.note_band_move(m.top, m.bottom, m.delta);
            self.trail.note_band_move(m.top, m.bottom, m.delta);
        }
        self.glow.drop_row_probe();
    }

    /// Content-scroll law, invalidation arm: the retained coordinates no longer
    /// share one transform, so every live point retires: Rainbow Kitty's ribbon
    /// through its 0.24 s curtain (a reset for other styles), and the comet at
    /// once.
    pub fn scroll_invalidate(&mut self, now: Instant) {
        self.glow.curtain(now);
        self.trail.reset();
    }

    /// Any BODY (not the aurora, not the comet) still animating — the part of
    /// the scheduler's frame-cadence set this step owns.
    #[must_use]
    pub fn bodies_active(&self) -> bool {
        self.rainbow.is_active()
            || self.droplet.is_active()
            || self.momentum.is_active()
            || self.beamrod.is_active()
            || self.comet.is_active()
            || self.phaser.is_active()
    }

    /// A body that still owes frames but not the frame cadence: the rainbow
    /// caret's colour release once its fingerprint stops moving
    /// ([`CursorRainbow::settling`]). Disjoint from [`Self::bodies_active`]; a
    /// host paces it by [`Self::bodies_next_change_deadline`] on its coarse
    /// one-shot arm.
    #[must_use]
    pub fn bodies_settling(&self) -> bool {
        self.rainbow.settling()
    }

    /// The instant of a settling body's next visible step (see
    /// [`CursorRainbow::next_change_deadline`]), `None` when none is settling.
    /// `&mut` because it is a memoised dry run of the body's tick.
    #[must_use]
    pub fn bodies_next_change_deadline(
        &mut self,
        now: Instant,
        frame_interval: std::time::Duration,
    ) -> Option<Instant> {
        if self.rainbow.settling() {
            self.rainbow.next_change_deadline(now, frame_interval)
        } else {
            None
        }
    }

    /// The frame step: the aurora, then every block body off the aurora's
    /// post-tick state, then the comet, then the caret's one owner.
    ///
    /// `glow_cfg` is the host's folded config (motion amplitude × shed on the
    /// intensity, live-cursor rewire, the ground, `head_dx`); `trail_cfg`
    /// likewise, `enabled` folded by the motion policy and its colour rewired.
    /// The comet's ignition from the typing cadence is stamped here.
    pub fn tick(
        &mut self,
        f: &CursorFxInput,
        glow_cfg: &GlowConfig,
        mut trail_cfg: TrailConfig,
    ) -> CursorFxFrame {
        let now = f.now;
        let cur = f.cur;
        let geom = f.geom;
        let body_amp = f.amplitude * f.shed_envelope;
        let dark = aterm_render::theme_is_dark(f.default_bg);
        let glow_fp = self
            .glow
            .tick(cur, now, glow_cfg, geom, &mut self.glow_scratch);
        // The master is folded in once for the whole block-body family. The
        // host resolves `cursor_trail` (with the brightness dial and the
        // motion/load-shed amplitude) into `GlowConfig::enabled` + `intensity`;
        // the fire body, light rod and laser bolt read that pair, but the
        // rainbow, droplet, comet and phaser bodies read only the style. This
        // one per-frame gate stops them running with the master off, and a
        // hot-reloaded master takes effect next frame.
        let body_allowed = f.body_allowed && f.live_viewport && cur.is_some();
        // The forge cursor fill (fire style): the block heats along the
        // black-body ramp with sustained momentum and cools to the theme fill.
        // Its colour is already folded into `glow_fp`.
        let forge_fill = forge_cursor_fill(body_allowed, glow_cfg, || self.glow.forge_fill());
        let rainbow_block = body_allowed
            && glow_cfg.enabled
            && glow_cfg.intensity > 0.0
            && f.focused
            && is_block(f.cursor_style);
        let pinned_base = f.cursor_color_pinned.then_some(f.live_cursor);
        // Typing-reactive rainbow cursor (the `rainbow kitty` block): the fill
        // moves from its base toward a spinning rainbow and a halo blooms,
        // scaled by typing momentum, cooling to a dim ember. It wears a pinned
        // cursor colour at rest and builds from white otherwise; only the
        // host's pinned verdict tells OSC 12 from the theme's seed
        // (owner, 2026-08-29).
        let rainbow_cfg = RainbowConfig {
            enabled: matches!(glow_cfg.style, GlowStyle::RainbowKitty) && rainbow_block,
            intensity: body_amp,
            blinking: matches!(f.cursor_style, CursorStyle::BlinkingBlock),
            base: pinned_base,
            // The ribbon emitter is the colour authority at the nozzle, so the
            // hot block cannot run a plausible-but-different rainbow beside
            // the trail it leads.
            head_rgb: self.glow.rainbow_head_rgb(glow_cfg),
            // The caret cools with its trail: the ribbon's eased display spine
            // (floored by v2's landing re-light), not the 220 ms cadence alone.
            paint: Some(self.glow.caret_paint(now)),
            // The page the caret's light lands on — the same ground the ribbon
            // solves against.
            ground: Some(f.default_bg & 0x00FF_FFFF),
            // The caret seam (§7.1): when a v2 meteor's frame-0 flare fired,
            // while its spring-snap relax is live; `None` is the identity.
            flare_at: self.glow.caret_flare_at(),
        };
        // One cadence decay per frame, and none when nothing listens: only the
        // rainbow/phaser bodies read the energy (each early-outs when disabled
        // or at zero intensity), and a disabled comet reads no ignited fields.
        // Cadence reads are pure, so a re-enable samples what an
        // always-sampling build would.
        let cadence_heard = trail_cfg.enabled
            || (rainbow_block
                && rainbow_cfg.intensity > 0.0
                && matches!(glow_cfg.style, GlowStyle::RainbowKitty | GlowStyle::Phaser));
        let (energy, warmth) = if cadence_heard {
            self.cadence.sample(now)
        } else {
            (0.0, 0.0)
        };
        // One field (`docs/design/RAINBOW-TRAIL-ONE-STORY.md` §2.1): body, halo,
        // kitty ribbon and sparkle rail resolve one spectrum band this frame
        // from the glow engine's family clock.
        let family_phase = self.glow.rainbow_phase();
        let family_field = self.glow.rainbow_field();
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
        // The rainbow owns the caret: with the rainbow live on a blinking block
        // the shape is pinned steady, typing or idle. Reduced motion / load shed
        // leave `fill` None, restoring the plain blink.
        let twinkle_cursor = rainbow_frame.fill.is_some() && rainbow_cfg.blinking;
        let rainbow_owns_caret = rainbow_frame.fill.is_some();
        let glow_fp = glow_fp ^ rainbow_frame.fp.rotate_left(23);
        // Liquid droplet cursor (the `water` body), riding the aurora's surge
        // read after its tick.
        let droplet_frame = self.droplet.tick(
            cur,
            now,
            self.glow.blaze(),
            geom,
            &DropletConfig {
                enabled: matches!(glow_cfg.style, GlowStyle::Water) && rainbow_block,
                intensity: body_amp,
            },
            &mut self.glow_scratch,
        );
        let glow_fp = glow_fp ^ droplet_frame.fp.rotate_left(47);
        // Typing-momentum glow: a warm additive halo that brightens with key
        // rate and cools in silence, on any style and shape; a hot cursor does
        // not blink (owner, 2026-09-08).
        let momentum_frame = self.momentum.tick(
            cur,
            now,
            geom,
            &MomentumGlowConfig {
                // Gated by the effects master, not the style: it is its own
                // effect on every style, `off` included.
                enabled: momentum_glow_allowed(
                    f.momentum_glow,
                    body_allowed && f.focused && f.master,
                    rainbow_owns_caret,
                ),
                intensity: body_amp,
                tau_s: MOMENTUM_GLOW_TAU_S,
                radius_cells: MOMENTUM_GLOW_RADIUS_CELLS,
                base: f.live_cursor,
                dark_theme: glow_cfg.dark_theme,
                block: is_block(f.cursor_style),
            },
            &mut self.glow_scratch,
        );
        let momentum_steady = momentum_frame
            .hot
            .then_some(match f.cursor_style {
                CursorStyle::BlinkingBlock => CursorStyle::SteadyBlock,
                CursorStyle::BlinkingUnderline => CursorStyle::SteadyUnderline,
                CursorStyle::BlinkingBar => CursorStyle::SteadyBar,
                other => other,
            })
            .filter(|s| *s != f.cursor_style);
        let glow_fp = glow_fp ^ momentum_frame.fp.rotate_left(53);
        // Comet nucleus cursor (the `comet` body), riding the aurora's blaze in
        // the post-OSC-12 glow colours.
        let comet_from = self.glow_scratch.len();
        let comet_frame = self.comet.tick(
            cur,
            now,
            self.glow.blaze(),
            geom,
            &CometConfig {
                enabled: matches!(glow_cfg.style, GlowStyle::Comet) && rainbow_block,
                intensity: body_amp,
                color: glow_cfg.color,
                accent: glow_cfg.accent,
            },
            &mut self.glow_scratch,
        );
        if !dark {
            drop_additive_from(&mut self.glow_scratch, comet_from);
        }
        let glow_fp = glow_fp ^ comet_frame.fp.rotate_left(29);
        // Phaser emitter cursor: the block is the emitter, its fill locked to
        // the beam's rolling hue over the same pinned-or-white base as the
        // rainbow block.
        let phaser_frame = self.phaser.tick(
            cur,
            now,
            self.glow.beam_hue(),
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
        let glow_fp = glow_fp ^ phaser_frame.fp.rotate_left(53);
        // Light-rod cursor, every style's shape-completion seam: a thin bar
        // becomes a rod of the style's light (keeping its DECSCUSR shape), and
        // styles without a bespoke block body (lumen, sparkle, beam, trail
        // packs) take the charged emitter block. `Classic` draws a plain caret.
        let bar_shape = body_allowed
            && f.focused
            && matches!(
                f.cursor_style,
                CursorStyle::BlinkingBar | CursorStyle::SteadyBar
            );
        let emitter_block = matches!(
            glow_cfg.style,
            GlowStyle::Beam | GlowStyle::Lumen | GlowStyle::Sparkle | GlowStyle::Custom
        );
        // The rod's tint: the config colour where it already carries the style
        // identity, the signature shade where the style paints outside it.
        let (rod_color, rod_haze) = match glow_cfg.style {
            GlowStyle::Beam => (glow_cfg.color, aterm_render::BEAM_SPACE_HAZE),
            GlowStyle::Water if !f.user_tinted => (0x0032_DCDE, 0x000E_66B4), // droplet crest over deep ocean
            GlowStyle::Fire if !f.user_tinted => (0x00FF_9632, 0x0078_1E00),  // ember over char
            GlowStyle::RainbowKitty if !f.user_tinted => (0x00FF_66CC, 0x0046_1E64), // the cat's pink over dusk purple
            GlowStyle::Phaser if !f.user_tinted => {
                let c = crate::color_math::hsv2rgb(self.glow.beam_hue(), 0.75, 0.95);
                (c, (c >> 1) & 0x007F_7F7F)
            }
            _ => (glow_cfg.color, (glow_cfg.color >> 1) & 0x007F_7F7F),
        };
        let beamrod_from = self.glow_scratch.len();
        let beamrod_frame = self.beamrod.tick(
            cur,
            now,
            self.glow.blaze(),
            geom,
            &BeamRodConfig {
                enabled: glow_cfg.enabled
                    && glow_cfg.intensity > 0.0
                    && (bar_shape || (rainbow_block && emitter_block)),
                intensity: body_amp,
                color: rod_color,
                haze: rod_haze,
                bar: bar_shape,
                shimmer: matches!(glow_cfg.style, GlowStyle::Sparkle),
            },
            &mut self.glow_scratch,
        );
        if !dark {
            drop_additive_from(&mut self.glow_scratch, beamrod_from);
        }
        let glow_fp = glow_fp ^ beamrod_frame.fp.rotate_left(17);
        // Lightning-bolt cursor (the `laser` block): a jagged bolt in the beam's
        // hue, whitened by typing heat and the strike flare, cooling as it
        // calms.
        let bolt_cursor = rainbow_block
            && glow_cfg.enabled
            && glow_cfg.intensity > 0.0
            && matches!(glow_cfg.style, GlowStyle::Laser);
        let bolt_fill = bolt_cursor.then(|| {
            let k = 0.55 * self.glow.blaze();
            let mix = |sh: u32| {
                let c = ((glow_cfg.color >> sh) & 0xff) as f32;
                (c + (255.0 - c) * k).min(255.0) as u32
            };
            (mix(16) << 16) | (mix(8) << 8) | mix(0)
        });
        let glow_fp = glow_fp ^ u64::from(bolt_fill.unwrap_or(0)).rotate_left(59);
        // The cadence-comet trail off the same caret cell, ignited from the
        // frame's one cadence sample; idle decays to 0, so a steady screen
        // produces no cells.
        crate::cursor_trail::ignite(&mut trail_cfg, energy, warmth);
        let _ = self
            .trail
            .tick(cur, now, &trail_cfg, &mut self.trail_scratch);
        // Ignition is baked at spawn; adaptive shedding is presentation opacity,
        // projected after the tick so resident cells fade with new ones, and
        // the fingerprint covers the exact alphas consumers copy.
        let trail_fp = crate::cursor_trail::project_trail_presentation(
            &mut self.trail_scratch,
            trail_cfg.color,
            f.shed_envelope,
        );
        // One candidate from the fill precedence, projected through the
        // envelope; a focus outline or composed clip can still keep it off the
        // frame.
        let block_fill = project_block_fill(
            resolve_block_fill(
                BlockFillSplice {
                    rainbow: rainbow_frame.fill,
                    forge: forge_fill,
                    phaser: phaser_frame.fill,
                    bolt: bolt_fill,
                    comet: comet_frame.fill,
                    droplet: droplet_frame.fill,
                    momentum: momentum_frame.fill,
                    beamrod: beamrod_frame.fill,
                },
                f.live_cursor,
                glow_cfg.color,
                f.cursor_color_pinned,
            ),
            f.live_cursor,
            f.shed_envelope,
        );
        // The opaque replacement channel's FINAL RGB must move the repaint key
        // even when an engine's raw fill rounded to the same adjacent sample.
        let glow_fp = block_fill.map_or(glow_fp, |owned| {
            glow_fp
                .wrapping_mul(1_000_003)
                .wrapping_add(u64::from(owned.fill) | (1 << 32))
        });
        CursorFxFrame {
            glow_fp,
            trail_fp,
            trail_color: trail_cfg.color,
            block_fill,
            bolt_cursor,
            twinkle_cursor,
            momentum_steady,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The caret-style law, as one truth table of
    /// `compose_caret_style_override(bolt, rainbow, momentum)`.
    #[test]
    fn the_caret_style_law_is_one_truth_table() {
        use CursorStyle::{Bolt, SteadyBar, SteadyBlock, SteadyUnderline};
        let rows = [
            // LAW 1, the rainbow half: under the rainbow, Blink is never the
            // caret's — regardless of momentum temperature.
            (false, true, None, Some(SteadyBlock)),
            (false, true, Some(SteadyBlock), Some(SteadyBlock)),
            (false, true, Some(SteadyBar), Some(SteadyBlock)),
            (false, true, Some(SteadyUnderline), Some(SteadyBlock)),
            // LAW 1, the other half: under a classic style the peer's law
            // stands — cool: no override; warm: the Steady* twin.
            (false, false, None, None),
            (false, false, Some(SteadyBlock), Some(SteadyBlock)),
            (false, false, Some(SteadyBar), Some(SteadyBar)),
            (false, false, Some(SteadyUnderline), Some(SteadyUnderline)),
            // The bolt outranks both — the `laser` body's own shape.
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

    /// The momentum ENGINE yields the caret cell to the rainbow; without the
    /// rainbow it is exactly the user's switch ANDed with the host's gates.
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
                // Deliberately unlike the terminal endpoint: Comet/Bolt/BeamRod
                // may carry this exact pinned trail-colour provenance.
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

    fn frame_input(now: Instant) -> CursorFxInput {
        CursorFxInput {
            now,
            cur: Some((2, 3)),
            live_viewport: true,
            cursor_style: CursorStyle::SteadyBar,
            blink_phase: true,
            live_cursor: 0x0050_FA7B,
            cursor_color_pinned: false,
            default_bg: 0x001A_1B26,
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
            amplitude: 1.0,
            shed_envelope: 1.0,
            body_allowed: true,
            master: true,
            momentum_glow: true,
            user_tinted: false,
        }
    }

    fn classic_cfg(enabled: bool) -> (GlowConfig, TrailConfig) {
        let glow = GlowConfig {
            ribbon_tall: false,
            ribbon_flat: false,
            classic_mono: false,
            theme_fg: 0x00C8_D3F5,
            theme_bg: 0x001A_1B26,
            dark_theme: true,
            enabled,
            style: GlowStyle::Classic,
            color: 0x0050_FA7B,
            accent: 0x0078_FFB8,
            duration: std::time::Duration::from_millis(260),
            length: 24,
            intensity: 0.7,
            audible: true,
            radius: 0.6,
            ring: true,
            beam: true,
            head_dx: 0.08,
            pack: None,
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

    /// The typing-momentum glow obeys the effects master: with
    /// `cursor_trail = false`, fast typing paints no halo and claims no caret.
    /// Negative control: with the master on and the style off, the same keys
    /// light it.
    #[test]
    fn the_momentum_glow_obeys_the_effects_master() {
        for master in [true, false] {
            let mut fx = CursorFx::default();
            let t0 = Instant::now();
            for k in 0..24u64 {
                fx.momentum.on_key(
                    t0 + std::time::Duration::from_millis(k * 40),
                    MOMENTUM_GLOW_TAU_S,
                );
            }
            let now = t0 + std::time::Duration::from_millis(24 * 40);
            // The style is `off` either way: only the master differs.
            let (glow, trail) = classic_cfg(false);
            let frame = fx.tick(
                &CursorFxInput {
                    master,
                    ..frame_input(now)
                },
                &glow,
                trail,
            );
            let momentum_owns = frame
                .block_fill
                .is_some_and(|owned| owned.owner == BlockFillOwner::Momentum);
            if master {
                assert!(
                    fx.momentum.is_active() || !fx.glow_scratch.is_empty(),
                    "control: a fast hand lights the momentum glow with the master on"
                );
            } else {
                assert!(
                    fx.glow_scratch.is_empty() && !momentum_owns && frame.momentum_steady.is_none(),
                    "the master off paints no momentum halo and pins no caret"
                );
            }
        }
    }

    #[test]
    fn head_dx_noses_into_a_bar_and_centres_everything_else() {
        assert_eq!(head_dx_for(CursorStyle::SteadyBar), 0.08);
        assert_eq!(head_dx_for(CursorStyle::BlinkingBar), 0.08);
        assert_eq!(head_dx_for(CursorStyle::SteadyBlock), 0.5);
        assert_eq!(head_dx_for(CursorStyle::BlinkingUnderline), 0.5);
    }

    /// Automatic trail colours follow the live cursor, as a window and its
    /// Settings preview both run them: the glow, the comet body and the
    /// brightened accent take the cursor colour. Laser keeps its own violet,
    /// and an explicit colour (or accent) stays as it was set.
    #[test]
    fn automatic_trail_colours_follow_the_live_cursor() {
        let live = 0x0050_FA7B;
        for style in [
            GlowStyle::Comet,
            GlowStyle::Beam,
            GlowStyle::Sparkle,
            GlowStyle::Lumen,
        ] {
            let (mut glow, mut trail) = classic_cfg(true);
            glow.style = style;
            glow.color = 0x009E_D6FF;
            trail.color = 0x009E_D6FF;
            follow_live_cursor(&mut glow, &mut trail, live, false, false);
            assert_eq!((glow.color, trail.color), (live, live), "{style:?}");
            assert_eq!(glow.accent, brighten(live, 1.5), "{style:?}");
        }
        // Laser keeps its own colour.
        let (mut glow, mut trail) = classic_cfg(true);
        glow.style = GlowStyle::Laser;
        glow.color = crate::cursor_glow::LASER_DEFAULT_COLOR;
        follow_live_cursor(&mut glow, &mut trail, live, false, false);
        assert_eq!(glow.color, crate::cursor_glow::LASER_DEFAULT_COLOR);
        assert!(own_trail_color(GlowStyle::Laser).is_some());
        assert!(own_trail_color(GlowStyle::Comet).is_none());
        // An explicit colour stays; an explicit accent stays.
        let (mut glow, mut trail) = classic_cfg(true);
        glow.style = GlowStyle::Comet;
        glow.color = 0x00E0_218A;
        trail.color = 0x00E0_218A;
        follow_live_cursor(&mut glow, &mut trail, live, true, false);
        assert_eq!((glow.color, trail.color), (0x00E0_218A, 0x00E0_218A));
        let (mut glow, mut trail) = classic_cfg(true);
        glow.style = GlowStyle::Comet;
        glow.accent = 0x0012_3456;
        follow_live_cursor(&mut glow, &mut trail, live, false, true);
        assert_eq!((glow.color, glow.accent), (live, 0x0012_3456));
    }
}
