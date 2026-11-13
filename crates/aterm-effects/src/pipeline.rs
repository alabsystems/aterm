// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Clockless effects pipeline driving cursor, word, rain, streak, and [`CompanionOwner`] engines.

use aterm_time::Instant;
use std::time::Duration;

use aterm_core::render::RenderInput;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_render::{GlowQuad, InkCell, RainHalo, WordDecoration, theme_is_dark};

use crate::matrix_rain::{RainCell, RainLayers};
use aterm_render::sprites::{FreeSprite, SpriteLayers, WORD_SPRITE_SALT};

use crate::companion::{CompanionOwner, ContrastFallback, PetFacts, PetOwnership};
use crate::cursor_fx::{CursorFx, CursorFxInput, brighten, compose_caret_style_override};
use crate::cursor_glow::{Geom, GlowConfig, GlowStyle};
use crate::cursor_momentum::MOMENTUM_GLOW_TAU_S;
use crate::cursor_trail::TrailConfig;
#[cfg(test)]
use crate::host::Wake;
use crate::host::{FrameGeom, HostFrameInput, PressOutcome, TerminalFacts, Visibility};
use crate::kitty_pet::{PetInputKind, PetSpecies};
use crate::matrix_rain::{
    MatrixRain, RAIN_ALPHA_CAP, RAIN_ALPHA_FLOOR, RainClock, RainConfig, RainFrame, RainHue,
    RainLatches, RainScan, RainTickInput, RainVisibility,
};
use crate::motion::{MotionEffect, MotionMode, MotionPolicy};
use crate::output_streak::{MAX_COMETS, OutputStreak, StreakConfig, TAIL_MAX, TAIL_MIN};
use crate::rainbow_kitty::TypedClass;
use crate::word_decorations::{DecoConfig, EffectGeom, Resolved, SelView, WordDecorations};

/// The symbols a US layout types with Shift held.
const WEB_SHIFTED_SYMBOLS: &str = "~!@#$%^&*()_+{}|:\"<>?";

#[inline]
const fn is_web_shifted_symbol(b: u8) -> bool {
    matches!(
        b,
        b'~' | b'!'
            | b'@'
            | b'#'
            | b'$'
            | b'%'
            | b'^'
            | b'&'
            | b'*'
            | b'('
            | b')'
            | b'_'
            | b'+'
            | b'{'
            | b'}'
            | b'|'
            | b':'
            | b'"'
            | b'<'
            | b'>'
            | b'?'
    )
}

const _: () = {
    let bytes = WEB_SHIFTED_SYMBOLS.as_bytes();
    let mut b = 0u8;
    while b <= 127 {
        let mut in_slice = false;
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b {
                in_slice = true;
                break;
            }
            i += 1;
        }
        assert!(is_web_shifted_symbol(b) == in_slice);
        b += 1;
    }
};

/// The glyph class a committed web character carries to the rainbow engine.
#[inline]
#[must_use]
pub const fn web_typed_class(ch: char) -> TypedClass {
    match ch {
        ' ' => TypedClass::Space,
        '!' => TypedClass::Bang,
        c if c.is_ascii() => {
            if c.is_ascii_uppercase() {
                TypedClass::Capital
            } else {
                TypedClass::Glyph
            }
        }
        c if c.is_uppercase() => TypedClass::Capital,
        _ => TypedClass::Glyph,
    }
}

/// The coherent cold-start ground pair for `dark_theme: true` (the default palette).
const COLD_THEME_FG: u32 = 0x00C8_D3F5;
const COLD_THEME_BG: u32 = 0x001A_1B26;

/// The frame grid a settling caret body's next step is forecast on in a page
/// ([`crate::cursor_fx::CursorFx::bodies_next_change_deadline`]).
const PAGE_SETTLE_FRAME: Duration = Duration::from_micros(16_667);

/// The cursor colour the pet's contrast sampler uses over blank glass, masked to `0x00RRGGBB`.
#[inline]
#[must_use]
const fn pet_contrast_cursor(input_cursor_color: u32, term_cursor_color: u32) -> u32 {
    if input_cursor_color == aterm_core::render::COLOR_UNSET {
        term_cursor_color & 0x00FF_FFFF
    } else {
        input_cursor_color & 0x00FF_FFFF
    }
}

/// The terminal's own cursor colour, packed (the native `terminal_cursor_rgb`).
#[inline]
#[must_use]
fn terminal_cursor_color(term: &Terminal) -> u32 {
    let c = match term.cursor_color() {
        Some(c) => c,
        None => term.default_foreground(),
    };
    aterm_render::rgb_to_u32([c.r, c.g, c.b])
}

/// The most recent completed command as the rain driver reads it.
#[inline]
fn term_cmd_done(term: &Terminal) -> Option<(u64, i32)> {
    let m = term.last_completed_command()?;
    Some((term.completed_command_seq(), m.exit_code?))
}

/// The sequence number of the most recent completed command with an exit code.
#[inline]
fn term_cmd_done_seq(term: &Terminal) -> Option<u64> {
    let m = term.last_completed_command()?;
    m.exit_code?;
    Some(term.completed_command_seq())
}

#[inline]
const fn clamp_rain_alpha(alpha: Option<u32>) -> Option<u8> {
    let Some(a) = alpha else {
        return None;
    };
    let floor = RAIN_ALPHA_FLOOR as u32;
    let cap = RAIN_ALPHA_CAP as u32;
    let clamped = if a < floor {
        floor
    } else if a > cap {
        cap
    } else {
        a
    };
    Some(clamped as u8)
}

/// Close the terminal's damage session if it is still open.
#[inline]
fn close_open_damage_session(term: &mut Terminal) {
    if term.has_damage() {
        term.take_damage();
    }
}

/// Whether a lexicon build-time conflict still applies under the resolved scan options.
#[inline]
#[must_use]
pub fn lexicon_warning_applies(warning: &str, cjk_single_char: bool) -> bool {
    !(cjk_single_char && warning.contains("requires cjk_single_char = true"))
}

/// Coordinate identity for cursor-owned pixel/cell geometry in an embedder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CursorCoordinateSpace {
    rows: usize,
    cols: usize,
    cell_w: usize,
    cell_h: usize,
    chrome_pad: u16,
    chrome_head: u16,
}

/// `row`'s first `cols` cells as the glow engine's row probe reads a row (`'\0'` at a wide
/// glyph's continuation, blanks to `cols`), into `out`.
fn probe_chars(row: &[aterm_core::terminal::RenderCell], cols: usize, out: &mut Vec<char>) {
    let slice = &row[..row.len().min(cols)];
    out.clear();
    out.reserve(cols);
    let mut i = 0;
    while i < slice.len() {
        let cell = &slice[i];
        out.push(if cell.wide { '\0' } else { cell.ch });
        i += 1;
    }
    out.resize(cols, ' ');
}

/// The scroll decision both hosts take.
#[inline]
const fn content_scroll_decision(
    previous: Option<ContentScrollState>,
    current: ContentScrollState,
) -> ContentScrollDelta {
    ContentScrollState::delta_since(previous, current)
}

/// What a page's [`EffectsPipeline::set_cursor_glow`] asked for, but its trail colour.
#[derive(Clone, Copy, Debug)]
struct GlowRequest {
    knobs: crate::cursor_fx::GlowKnobs,
    spelling: crate::cursor_fx::GlowSpelling,
    theme_cursor: u32,
}

/// Owns the effect engines + scratch and applies them to a frame snapshot.
pub struct EffectsPipeline {
    /// Host-advanced monotonic offset from `t0` (the injected clock).
    clock: Duration,
    /// Epoch captured at construction.
    t0: Instant,
    /// Last non-destructive terminal content-scroll snapshot consumed by this pipeline.
    cursor_scroll_state: Option<ContentScrollState>,
    /// Last frame geometry bound to the retained cursor engines.
    cursor_coordinate_space: Option<CursorCoordinateSpace>,
    /// `(terminal identity, alternate screen)`.
    cursor_owner_space: Option<(u64, bool)>,
    /// Whether the last frame's terminal had a foreground program owning the line.
    program_owns_line: bool,

    /// The cursor family.
    cursor: CursorFx,
    glow_cfg: GlowConfig,
    /// The instant of the last native cursor step and the light its config carried: the frame
    /// light ([`Self::frame_light`]).
    frame_light: Option<(Instant, crate::cursor_glow::Light)>,
    /// The instant of the last composed word pass and each of its panes' light, by session
    /// ([`Self::pane_word_light`]).
    pane_lights: (Option<Instant>, Vec<(u64, crate::cursor_glow::Light)>),
    /// The page's trail colour setting.
    glow_trail_color: crate::cursor_fx::TrailColor,
    /// The page set an accent of its own.
    glow_explicit_accent: bool,
    /// The page's last [`Self::set_cursor_glow`].
    glow_request: Option<GlowRequest>,
    trail_cfg: TrailConfig,
    /// Serious Mode's cursor-body gate.
    serious: bool,
    /// Who owned the block cursor on the last applied frame and the colour its body wore.
    #[cfg(test)]
    block_fill: Option<crate::cursor_glow::BlockFill>,
    /// A test's negative control: the published streams keep the trail's
    /// light on the word icons ([`Self::shade_word_icons`]).
    #[cfg(test)]
    word_icons_unshaded: bool,
    /// A test's negative control: a frame whose host hands no cells runs its
    /// word pass but names none of its icons ([`Self::note_word_icons`]).
    #[cfg(test)]
    word_icons_unnamed: bool,
    /// A test's negative control: every caret-seated light sits at the block's centre whatever
    /// the caret's shape (`head_dx` pinned to `0.5`, [`Self::set_bar_anchor`]).
    #[cfg(any(test, debug_assertions))]
    bar_unanchored: bool,
    /// The opaque comet's own colour, where `set_cursor_trail` gave one.
    trail_body_color: Option<u32>,
    /// Keystrokes noted since the last [`Self::advance`], replayed evenly across its `dt`.
    pending_keys: u32,
    /// Reused row material for the glow engine's row probe (`CursorGlow::observe_row`).
    row_probe_scratch: Vec<char>,
    /// The rows over and under the caret's, read for the row probe's flanks where a test asks
    /// for them ([`Self::set_probe_flanks`]): `None` while it does not.
    #[cfg(any(test, debug_assertions))]
    probe_flanks: Option<(Vec<char>, Vec<char>)>,
    /// The caret row the flying head last sat in the sky seat over
    /// ([`crate::rainbow_kitty::companion::SeatQuery::held_sky`]).
    flying_sky_row: Option<u16>,
    /// How the flying head is fitted to its row ([`crate::rainbow_kitty::companion::HeadFit`]).
    flying_fit: crate::rainbow_kitty::companion::HeadFit,
    /// The size a fitted head goes to this frame, as its route read it, the time, and whether
    /// the host animates.
    flying_fit_goal: Option<(f32, Instant, bool)>,

    decos: WordDecorations,
    /// Compiled lexicon + resolved config.
    sparkle: Option<Resolved>,
    /// The knobs, kept across off/on toggles (native `DecoConfig` defaults).
    deco_cfg: DecoConfig,
    /// Raw `[sparkle_words.emphasis] enabled`.
    emphasis_enabled: bool,
    /// Languages whose ambiguous homograph entries are un-gated (native default `["en"]`).
    languages: Vec<String>,
    /// User lexicon-override TOML (the native `lexicon` + `extra_words` channel).
    override_toml: Option<String>,
    /// The synthesized emphasis-class lexicon fragment for the custom spec words.
    custom_lexicon: String,

    /// Embedder window chrome in px for window-absolute emissions.
    chrome_pad: u16,
    chrome_head: u16,
    /// Height in px of the rows the host prepends above the grid after `apply`.
    host_rows_px: u16,
    /// Raw `Lexicon::conflicts()` from the last `rebuild_sparkle` (empty while sparkle is off).
    lexicon_conflicts: Vec<String>,
    /// Parse diagnostic from the last [`Self::set_sparkle_custom_specs`] whose TOML fragment was
    /// malformed.
    custom_spec_warning: Option<String>,

    deco_scratch: Vec<WordDecoration>,
    ink_scratch: Vec<InkCell>,
    /// Free-overlay channel (peeking cats and companions as [`FreeSprite`]s).
    free_scratch: Vec<FreeSprite>,
    /// Publishes `free_scratch` into the frame's fx layers.
    sprite_layers: SpriteLayers,
    nova_scratch: Vec<GlowQuad>,

    /// The resident pet's driver (`companion.rs`).
    companion: CompanionOwner,
    /// Which cat the companions wear.
    look: crate::companion_look::LookPolicy,
    /// The companions beside the resident pet ([`crate::companions`]).
    companions: crate::companions::Companions,
    /// Output-streak engine.
    streak: Option<Box<OutputStreak>>,
    /// The streak knobs, kept across off/on toggles.
    streak_cfg: StreakConfig,
    /// Host reduce-motion gate specific to the output streak (`set_output_streak_reduced_motion`).
    streak_reduced_motion: bool,
    /// The page's streak seed (`set_output_streak`), the one its engine is built with.
    streak_seed: u64,

    /// Phosphor rain engine.
    rain: Option<Box<MatrixRain>>,
    /// The rain knobs, kept across off/on toggles.
    rain_cfg: RainConfig,
    /// Last host-reported visibility, replayed onto a lazily-built engine.
    rain_visibility: RainVisibility,
    /// Composer provenance survives while the lazily-owned rain engine is absent.
    rain_material_editing: bool,
    /// The rain driver's session-keyed edges.
    rain_latches: RainLatches,
    /// Whole milliseconds already fed to rain's accumulator.
    rain_clock_ms: u64,
    rain_scratch: Vec<RainCell>,
    /// Publishes `rain_scratch` into the frame's fx layers.
    rain_layers: RainLayers,
    rain_add_scratch: Vec<RainHalo>,

    /// Scratch for a composed frame's per-pane word pass before it is translated into the pane.
    pane_deco: Vec<WordDecoration>,
    pane_ink: Vec<InkCell>,
    pane_free: Vec<FreeSprite>,
    pane_nova: Vec<GlowQuad>,
    /// The flying head's body drawn into [`Self::pane_free`], pane pixels,
    /// until [`Self::place_pane_sprites`] places it in the window grid.
    pane_head: Option<(i32, i32, i32, i32)>,
    /// Output streak per visible terminal pane of a composed frame, keyed `(session, pane slot)`.
    streak_panes: std::collections::BTreeMap<(u64, usize), OutputStreak>,
}

impl Default for EffectsPipeline {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl EffectsPipeline {
    /// Everything off; no lexicon is compiled until sparkle words are enabled.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self {
            clock: Duration::ZERO,
            t0: Instant::now(),
            cursor_scroll_state: None,
            cursor_coordinate_space: None,
            cursor_owner_space: None,
            program_owns_line: false,
            cursor: CursorFx::default(),
            glow_cfg: GlowConfig {
                ribbon_tall: false,
                ribbon_flat: false,
                classic_mono: false,
                theme_fg: COLD_THEME_FG,
                theme_bg: COLD_THEME_BG,
                dark_theme: true,
                enabled: false,
                style: GlowStyle::Lumen,
                color: 0x0050_FA7B,
                accent: brighten(0x0050_FA7B, 1.5),
                duration: Duration::from_millis(260),
                length: 24,
                intensity: 0.7,
                audible: true,
                radius: 0.6,
                ring: true,
                beam: true,
                head_dx: 0.5,
                light: crate::cursor_glow::Light::OFF,
            },
            glow_trail_color: crate::cursor_fx::TrailColor::Automatic,
            glow_explicit_accent: false,
            glow_request: None,
            frame_light: None,
            pane_lights: (None, Vec::new()),
            trail_cfg: TrailConfig {
                enabled: false,
                duration: Duration::from_millis(260),
                max_len: 24,
                color: 0x0050_FA7B,
                intensity: 0.0,
                warmth: 0.0,
            },
            trail_body_color: None,
            serious: false,
            #[cfg(test)]
            block_fill: None,
            #[cfg(test)]
            word_icons_unshaded: false,
            #[cfg(test)]
            word_icons_unnamed: false,
            #[cfg(any(test, debug_assertions))]
            bar_unanchored: false,
            pending_keys: 0,
            row_probe_scratch: Vec::new(),
            #[cfg(any(test, debug_assertions))]
            probe_flanks: None,
            flying_sky_row: None,
            flying_fit: crate::rainbow_kitty::companion::HeadFit::WHOLE,
            flying_fit_goal: None,
            decos: WordDecorations::default(),
            sparkle: None,
            deco_cfg: DecoConfig::default(),
            emphasis_enabled: true,
            languages: vec!["en".to_string()],
            override_toml: None,
            custom_lexicon: String::new(),
            lexicon_conflicts: Vec::new(),
            custom_spec_warning: None,
            deco_scratch: Vec::new(),
            ink_scratch: Vec::new(),
            free_scratch: Vec::new(),
            sprite_layers: SpriteLayers::new(WORD_SPRITE_SALT),
            nova_scratch: Vec::new(),
            companion: CompanionOwner::default(),
            look: crate::companion_look::LookPolicy::default(),
            companions: crate::companions::Companions::default(),
            streak: None,
            streak_cfg: StreakConfig::default(),
            streak_reduced_motion: false,
            streak_seed: 0,
            rain: None,
            rain_cfg: RainConfig::default(),
            rain_visibility: RainVisibility::Focused,
            rain_material_editing: false,
            rain_latches: RainLatches::default(),
            rain_clock_ms: 0,
            rain_scratch: Vec::new(),
            rain_layers: RainLayers::default(),
            rain_add_scratch: Vec::new(),
            chrome_pad: 0,
            chrome_head: 0,
            host_rows_px: 0,
            pane_deco: Vec::new(),
            pane_ink: Vec::new(),
            pane_free: Vec::new(),
            pane_nova: Vec::new(),
            pane_head: None,
            streak_panes: std::collections::BTreeMap::new(),
        }
    }

    /// Set the embedder's window chrome.
    #[inline]
    pub const fn set_chrome(&mut self, pad: u16, head: u16) {
        self.chrome_pad = pad;
        self.chrome_head = head;
    }

    /// Set the height in px of the rows the host prepends above the grid after the next `apply`.
    #[inline]
    pub const fn set_host_rows_px(&mut self, px: u16) {
        self.host_rows_px = px;
    }

    /// Window-space geometry for a `rows`×`cols` grid of `cell_w`×`cell_h` cells under the
    /// configured chrome.
    #[inline]
    const fn chrome_geom(&self, rows: usize, cols: usize, cell_w: usize, cell_h: usize) -> Geom {
        let pad = self.chrome_pad;
        let head = self.chrome_head;
        let w = cols.saturating_mul(cell_w);
        let h = rows.saturating_mul(cell_h);
        let grid_w = if w > u16::MAX as usize {
            u16::MAX
        } else {
            w as u16
        };
        let grid_h = if h > u16::MAX as usize {
            u16::MAX
        } else {
            h as u16
        };
        Geom {
            cw: cell_w,
            ch: cell_h,
            rows,
            cols,
            origin_x: pad,
            origin_y: pad.saturating_add(head),
            win_w: grid_w.saturating_add(pad.saturating_mul(2)),
            win_h: grid_h
                .saturating_add(pad.saturating_mul(2))
                .saturating_add(head),
            head,
        }
    }

    /// Advance the injected clock by `dt_ms` (host rAF delta).
    #[inline]
    pub fn advance(&mut self, dt_ms: f64) {
        if !(dt_ms.is_finite() && dt_ms > 0.0) {
            return;
        }
        let prev = self.clock;
        let delta = Duration::try_from_secs_f64(dt_ms / 1000.0).unwrap_or(Duration::MAX);
        if delta.is_zero() {
            return;
        }
        let candidate = prev.saturating_add(delta);
        self.clock = if self.t0.checked_add(candidate).is_some() {
            candidate
        } else {
            let ceiling = crate::effect_util::saturating_add_duration(self.t0, Duration::MAX);
            ceiling.saturating_duration_since(self.t0)
        };
        if self.clock <= prev {
            return;
        }
        let n = self.pending_keys;
        if n > 0 {
            self.pending_keys = 0;
            let dt = self.clock - prev;
            let mut newest = crate::effect_util::saturating_add_duration(self.t0, self.clock);
            let inv_n = 1.0 / f64::from(n);
            let mut i = 1;
            while i <= n {
                let at = crate::effect_util::saturating_add_duration(
                    self.t0,
                    prev.saturating_add(dt.mul_f64(f64::from(i) * inv_n)),
                );
                self.cursor.cadence.on_keystroke(at);
                self.cursor.momentum.on_key(at, MOMENTUM_GLOW_TAU_S);
                self.cursor.glow.note_typed_cells(at, 1);
                newest = at;
                i += 1;
            }
            self.cursor.trail.note_typed_batch(newest, n);
        }
        if let Some(rain) = self.rain.as_deref_mut() {
            let total = u64::try_from(self.clock.as_millis()).unwrap_or(u64::MAX);
            rain.advance_ms(total.saturating_sub(self.rain_clock_ms));
            self.rain_clock_ms = total;
        }
    }

    /// The current injected instant (`t0 + clock`).
    #[inline]
    #[must_use]
    pub fn now(&self) -> Instant {
        crate::effect_util::saturating_add_duration(self.t0, self.clock)
    }

    /// `true` while any effect is animating.
    #[inline]
    #[must_use]
    pub fn is_active(&self) -> bool {
        let now = self.now();
        self.cursor.trail.is_active()
            || self.cursor.glow.is_active()
            || self.cursor.bodies_active()
            || self.cursor.bodies_settling()
            || self.companion.needs_frames()
            || matches!(self.rain.as_ref(), Some(r) if r.is_active())
            || matches!(self.streak.as_ref(), Some(s) if s.is_active())
            || self.decos.is_active(now)
            || self.companion.next_change_deadline(now).is_some()
    }

    /// Milliseconds until the next scheduled engine wake.
    #[inline]
    #[must_use]
    pub fn next_deadline_ms(&mut self) -> Option<f64> {
        let now = self.now();
        if self.cursor.trail.is_active()
            || self.cursor.bodies_active()
            || self.decos.is_active(now)
            || self.companion.needs_frames()
        {
            return None;
        }
        let glow_ms = match self.cursor.glow.next_change_deadline(now, Duration::ZERO) {
            Some(d) if d > now => Some(d.duration_since(now).as_secs_f64() * 1000.0),
            Some(_) => return None, // live motion → display-rAF
            None => None,           // glow settled
        };
        let pet_ms = match self.companion.next_change_deadline(now) {
            Some(d) if d > now => Some(d.duration_since(now).as_secs_f64() * 1000.0),
            Some(_) => return None, // a step due now → display-rAF
            None => None,           // deep asleep, hidden or gone
        };
        let rain_ms = match &self.rain {
            Some(rain) if rain.is_active() => Some(rain.next_tick_in_ms() as f64),
            _ => None,
        };
        let streak_ms = if let Some(streak) = self.streak.as_deref() {
            match streak.next_change_deadline(now, self.streak_cfg.idle_secs) {
                Some(d) if d > now => Some(d.duration_since(now).as_secs_f64() * 1000.0),
                Some(_) if streak.is_active() => return None, // visible comet → display-rAF
                Some(_) => Some(0.0), // overdue idle/Settle → immediate one-shot
                None => None,
            }
        } else {
            None
        };
        let body_ms = if self.cursor.bodies_settling() {
            match self
                .cursor
                .bodies_next_change_deadline(now, PAGE_SETTLE_FRAME)
            {
                Some(d) if d > now => Some(d.duration_since(now).as_secs_f64() * 1000.0),
                Some(_) => return None, // a step due now → display-rAF
                None => None,
            }
        } else {
            None
        };
        let cands = [glow_ms, pet_ms, rain_ms, streak_ms, body_ms];
        let mut best: Option<f64> = None;
        let mut i = 0;
        while i < cands.len() {
            if let Some(ms) = cands[i] {
                best = Some(match best {
                    Some(prev) => prev.min(ms),
                    None => ms,
                });
            }
            i += 1;
        }
        best
    }

    /// One scheduling verdict ([`Wake`]).
    #[must_use]
    #[cfg(test)]
    pub fn wake(&mut self) -> Wake {
        match self.next_deadline_ms() {
            Some(ms) => {
                let dur = if ms.is_finite() && ms > 0.0 {
                    Duration::try_from_secs_f64(ms / 1000.0).unwrap_or(Duration::MAX)
                } else {
                    Duration::ZERO
                };
                Wake::At(crate::effect_util::saturating_add_duration(self.now(), dur))
            }
            None if self.is_active() => Wake::Frames,
            None => Wake::Idle,
        }
    }

    /// Focus gate: an unfocused embedder fires no cat idle one-shots.
    #[inline]
    pub fn set_focused(&mut self, focused: bool) {
        self.set_rain_visibility(if focused {
            RainVisibility::Focused
        } else {
            RainVisibility::VisibleUnfocused
        });
    }

    /// Tri-state pane visibility.
    #[inline]
    pub fn set_effects_visibility(&mut self, state: &str) {
        let v = if state.eq_ignore_ascii_case("hidden") {
            RainVisibility::Hidden
        } else if state.eq_ignore_ascii_case("visible_unfocused")
            || state.eq_ignore_ascii_case("unfocused")
        {
            RainVisibility::VisibleUnfocused
        } else {
            RainVisibility::Focused
        };
        self.set_rain_visibility(v);
    }

    /// Record the visibility and replay it onto a live engine.
    #[inline]
    fn set_rain_visibility(&mut self, v: RainVisibility) {
        if v == RainVisibility::Hidden && self.rain_visibility != RainVisibility::Hidden {
            self.cursor.glow.reset();
            self.cursor.retire_bodies();
            self.pending_keys = 0;
            self.companion.retire_coordinate_space();
        }
        self.rain_visibility = v;
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.set_visibility(v);
        }
    }

    /// Register one text-blind keystroke for cursor-wake ignition.
    #[inline]
    pub const fn note_keystroke(&mut self) {
        let next = self.pending_keys.saturating_add(1);
        self.pending_keys = if next < 64 { next } else { 64 };
        self.rain_material_editing = true;
        if let Some(rain) = &mut self.rain {
            rain.note_keystroke();
        }
    }

    /// Register one committed typed scalar.
    #[inline]
    #[must_use]
    pub fn note_committed_char(
        &mut self,
        term: &mut Terminal,
        _input: &RenderInput,
        ch: char,
    ) -> bool {
        let ctrl = if ch.is_ascii() {
            ch.is_ascii_control()
        } else {
            ch.is_control()
        };
        if !ctrl {
            self.note_console_input(PetInputKind::Text);
        }
        let width = if ch.is_ascii() {
            usize::from(!ctrl)
        } else if term.modes().ambiguous_width_double {
            aterm_grapheme::char_width_cjk(ch)
        } else {
            aterm_grapheme::char_width(ch)
        };
        if ctrl || !(1..=2).contains(&width) {
            self.note_keystroke();
            return false;
        }
        let now = self.now();
        self.cursor.cadence.on_keystroke(now);
        self.cursor.momentum.on_key(now, MOMENTUM_GLOW_TAU_S);
        self.rain_material_editing = true;
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_keystroke();
        }
        let class = web_typed_class(ch);
        let shifted =
            class == TypedClass::Capital || (ch.is_ascii() && is_web_shifted_symbol(ch as u8));
        self.cursor
            .glow
            .note_typed_glyph(now, width as u16, shifted, class);
        self.cursor.trail.note_typed(now);
        true
    }

    /// One accepted input event, including an entire committed IME run or paste.
    #[inline]
    pub fn note_console_input(&mut self, kind: PetInputKind) {
        let now = self.now();
        self.companion.note_console_input(now, kind);
        match kind {
            PetInputKind::Delete => {
                self.cursor.cadence.on_keystroke(now);
                self.rain_material_editing = true;
                if let Some(rain) = self.rain.as_deref_mut() {
                    rain.note_keystroke();
                }
                self.cursor.glow.note_backspace(now);
            }
            PetInputKind::Kill => {
                self.cursor.cadence.on_keystroke(now);
                self.rain_material_editing = true;
                if let Some(rain) = self.rain.as_deref_mut() {
                    rain.note_keystroke();
                }
                self.cursor.glow.note_kill(now, true);
            }
            PetInputKind::KillForward => {
                self.cursor.cadence.on_keystroke(now);
                self.rain_material_editing = true;
                if let Some(rain) = self.rain.as_deref_mut() {
                    rain.note_keystroke();
                }
                self.cursor.glow.note_kill(now, false);
            }
            PetInputKind::Navigate => self.cursor.glow.note_navigation(now),
            PetInputKind::Submit if self.program_owns_line => {
                self.cursor.glow.note_return_keeping_hand(now);
            }
            PetInputKind::Submit => self.cursor.glow.note_return(now),
            PetInputKind::Paste => self.cursor.glow.note_user_gesture(now),
            PetInputKind::Text => {}
        }
    }

    /// Visual bell: the rain engine's 2 s amber alert hue-ramp (gated by the `bell_alert` knob).
    #[inline]
    pub fn note_bell(&mut self) {
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_bell();
        }
        self.companion.note_bell(self.now());
    }

    /// Embedded host observed wheel/PgUp while an alternate-screen tui is active.
    #[inline]
    pub const fn note_matrix_rain_alt_scroll(&mut self) {
        if let Some(rain) = &mut self.rain {
            rain.note_alt_scroll();
        }
    }

    /// Payload-free observable agent/tool choreography.
    #[inline]
    pub const fn note_matrix_rain_signal(&mut self, code: u32, weight: u32) {
        if code == crate::matrix_rain::RainSignal::TurnStart as u32 {
            self.rain_material_editing = false;
        }
        if let Some(rain) = &mut self.rain {
            rain.note_signal(code, weight);
        }
    }

    /// Whether any effect surface is enabled (sparkle and rain count while quiescent).
    #[inline]
    #[must_use]
    pub const fn enabled_any(&self) -> bool {
        self.glow_cfg.enabled
            || self.trail_cfg.enabled
            || self.sparkle.is_some()
            || self.rain.is_some()
            || self.companion.needs_frames()
            || self.streak.is_some()
    }

    /// Configure the lumen cursor aurora (the additive `cursor_glow_add` channel).
    #[inline]
    #[allow(
        clippy::too_many_arguments,
        reason = "one call mirrors the native GlowConfig resolution; a builder would relocate the list"
    )]
    pub fn set_cursor_glow(
        &mut self,
        enabled: bool,
        style: &str,
        color: crate::cursor_fx::TrailColor,
        accent: Option<u32>,
        duration_ms: u64,
        length: u32,
        intensity: f32,
        radius: f32,
        ring: bool,
        theme_cursor: u32,
    ) {
        let color = color.masked();
        self.glow_trail_color = color;
        self.glow_explicit_accent = accent.is_some();
        self.glow_request = Some(GlowRequest {
            knobs: crate::cursor_fx::GlowKnobs {
                enabled,
                color: None,
                accent,
                duration_ms,
                length: length as usize,
                intensity,
                radius,
                ring,
            },
            spelling: crate::cursor_fx::GlowSpelling::parse(style),
            theme_cursor,
        });
        self.resolve_glow();
        if !enabled {
            self.cursor.glow.reset();
        }
    }

    /// Test seam: draw the rainbow's ribbon body in a shape no built-in theme picks (the tall
    /// body, or the tall body held flat), until the next [`Self::set_cursor_glow`]. The `rainbow`
    /// theme is the underline; the other bodies stay engine data the replays still judge.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_ribbon_shape(&mut self, tall: bool, flat: bool) {
        if let Some(request) = self.glow_request.as_mut() {
            request.spelling.ribbon_tall = tall;
            request.spelling.ribbon_flat = flat;
        }
        self.resolve_glow();
    }

    /// Set only the glow's trail colour.
    #[inline]
    pub fn set_cursor_glow_color(&mut self, color: &str) -> bool {
        let Some(color) = crate::cursor_fx::TrailColor::parse(color) else {
            return false;
        };
        self.glow_trail_color = color;
        self.resolve_glow();
        true
    }

    /// The glow's config from the page's last [`Self::set_cursor_glow`] and its trail colour
    /// ([`crate::cursor_fx::resolve_glow_config`]).
    #[inline]
    const fn resolve_glow(&mut self) {
        let Some(request) = self.glow_request else {
            return;
        };
        self.glow_cfg = crate::cursor_fx::resolve_glow_config(
            crate::cursor_fx::GlowKnobs {
                color: self.glow_trail_color.knob(request.theme_cursor),
                ..request.knobs
            },
            request.spelling,
            request.theme_cursor,
            true,
            self.glow_cfg.theme_fg,
            self.glow_cfg.theme_bg,
            0.5,
        );
        self.glow_cfg.audible = true;
    }

    /// Configure the classic opaque comet trail (the `cursor_trail` channel).
    #[inline]
    pub fn set_cursor_trail(
        &mut self,
        enabled: bool,
        duration_ms: u64,
        length: u32,
        color: Option<u32>,
        theme_cursor: u32,
    ) {
        self.trail_body_color = color.map(|c| c & 0x00FF_FFFF);
        self.trail_cfg = TrailConfig {
            enabled,
            duration: Duration::from_millis(duration_ms.clamp(30, 2000)),
            max_len: (length as usize).clamp(1, 512),
            color: match color {
                Some(c) => c,
                None => theme_cursor,
            } & 0x00FF_FFFF,
            intensity: 0.0,
            warmth: 0.0,
        };
        if !enabled {
            self.cursor.trail.reset();
        }
    }

    /// A test's negative control: a pinned trail colour's paper inks are its hue's vivid ink
    /// (`CursorGlow::set_chosen_ink`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_chosen_ink(&mut self, on: bool) {
        self.cursor.glow.set_chosen_ink(on);
    }

    /// A test's negative control: a typed light lives its tuned life whatever the duration
    /// (`CursorGlow::set_typed_life_scaled`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_typed_life_scaled(&mut self, on: bool) {
        self.cursor.glow.set_typed_life_scaled(on);
    }

    /// A test's negative control: Comet's dark dust stays ice under a pinned trail colour
    /// (`CursorGlow::set_comet_pin_dust`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_comet_pin_dust(&mut self, on: bool) {
        self.cursor.glow.set_comet_pin_dust(on);
    }

    /// A test's negative control: Comet lays its icy nucleus and dense-row seat under a
    /// pinned trail colour (`CursorGlow::set_comet_pin_head`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_comet_pin_head(&mut self, on: bool) {
        self.cursor.glow.set_comet_pin_head(on);
    }

    /// A test's negative control: the Rainbow Kitty's band under the caret crosses the bar's
    /// whole cell (`CursorGlow::set_ribbon_bar_band`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_ribbon_bar_band(&mut self, on: bool) {
        self.cursor.glow.set_ribbon_bar_band(on);
    }

    /// A test's negative control: a pinned trail's lights on a dark page keep the hue and
    /// chroma each body laid them in, not the pin's (`CursorGlow::set_pin_chroma`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pin_chroma(&mut self, on: bool) {
        self.cursor.glow.set_pin_chroma(on);
    }

    /// A test's negative control: the pinned lights' hold cuts every light to the pin's share
    /// at its luminance, as composed r2's did (`CursorGlow::set_pin_hold_two_sided`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pin_hold_two_sided(&mut self, on: bool) {
        self.cursor.glow.set_pin_hold_two_sided(on);
    }

    /// A test's negative control: `on` (as shipped), paper Lumen's sparks live several keys so
    /// its tracer reaches back over the letters just typed and lingers; off, they live as
    /// briefly as the dark face's, as composed r2's did (paper Lumen at rest was Off).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_lumen_paper_tracer(&mut self, on: bool) {
        self.cursor.glow.lumen_paper_brief = !on;
    }

    /// A test's negative control: `on` (as shipped), Classic's one-colour face lays the page's
    /// neutral as its night ground on a tinted dark page; off, it solves the ground toward its
    /// lights' own hue, as composed r2's did (a maroon disc round the caret on Solarized Dark).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_classic_ground_neutral(&mut self, on: bool) {
        self.cursor.glow.classic_ground_own_hue = !on;
        self.cursor.glow.forget_night_ground();
    }

    /// A test's negative control: `on` (as shipped), Classic's one-colour face lays a chosen
    /// colour's dark bed and rail at its own chroma; off, it floors their saturation to the
    /// bed's 0.9 and the rail's 1.0, as before composed O10(a).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_classic_pin_own_chroma(&mut self, on: bool) {
        self.cursor.glow.set_classic_pin_own_chroma(on);
    }

    /// A test's negative control: `on` (as shipped), Beam's body under a lit page's letters is a
    /// page shade the letter guard holds by itself; off, it is weighed and cut with the lights in
    /// its cells (`CursorGlow::set_lit_body_a_shade`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_lit_body_a_shade(&mut self, on: bool) {
        self.cursor.glow.set_lit_body_a_shade(on);
    }

    /// A test's negative control: `on` (as shipped), Classic's one-colour rail raises a chosen
    /// colour's saturation by the page's grey less its tint along the colour's hue; off, by the
    /// page's whole brightness, as before the after-land lows' W10b.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_classic_rail_tint_read(&mut self, on: bool) {
        self.cursor.glow.set_classic_rail_tint_read(on);
    }

    /// A test's negative control: Classic's one-colour face adds its light straight onto a
    /// tinted dark page and mixes its bed toward the page (`CursorGlow::classic_pin_grounded`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_classic_pin_ground(&mut self, on: bool) {
        self.cursor.glow.classic_pin_ground_off = !on;
    }

    /// A test's negative control: a bar or underline caret is drawn in its own colour on
    /// whatever light its cell holds (`CursorGlow::set_thin_caret_read`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_thin_caret_read(&mut self, on: bool) {
        self.cursor.glow.set_thin_caret_read(on);
    }

    /// The ground the last frame read its bar or underline caret against, after any hold
    /// (`CursorGlow::hold_light_for_thin_caret`). Debug and test builds only.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn thin_caret_ground(&self) -> Option<u32> {
        self.cursor.glow.thin_caret_read.map(|(_, ground)| ground)
    }

    /// A test's host: with `on`, [`Self::apply`]'s row probe carries the rows over and under
    /// the caret's (`RowProbe::flanks`), read off the frame's cells, as the window's host hands
    /// them (`aterm-gui`'s `app_render`). [`Self::apply`] hands none by itself, so a row whose
    /// next row holds letters is not known to (the rainbow ribbon draws its rail under it), and
    /// no star lands by a row nobody read: a replay that is to draw what the window draws on
    /// such a row asks for them.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub fn set_probe_flanks(&mut self, on: bool) {
        self.probe_flanks = on.then(Default::default);
    }

    /// A test's negative control: `on == false` pins the frame's `head_dx` to `0.5`, so every
    /// light the caret seats sits at the block's centre behind a bar too.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_bar_anchor(&mut self, on: bool) {
        self.bar_unanchored = !on;
    }

    /// A test's negative control: a chosen colour's bolt caret keeps the page floor alone
    /// (`CursorFx::set_chosen_bolt_lift`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_chosen_bolt_lift(&mut self, on: bool) {
        self.cursor.set_chosen_bolt_lift(on);
    }

    /// A test's negative control: a chosen colour's bolt short of its bar at its clip stops
    /// there (`CursorFx::set_chosen_bolt_past_clip`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_chosen_bolt_past_clip(&mut self, on: bool) {
        self.cursor.set_chosen_bolt_past_clip(on);
    }

    /// A test's negative control: Laser's bolt in the theme's own colour on paper keeps the page
    /// floor alone (`CursorFx::set_paper_bolt_lift`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_paper_bolt_lift(&mut self, on: bool) {
        self.cursor.set_paper_bolt_lift(on);
    }

    /// A test's negative control: no bed's rail lies in its own row's foot
    /// band (`CursorGlow::set_bed_row_rails`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_bed_row_rails(&mut self, on: bool) {
        self.cursor.glow.set_bed_row_rails(on);
    }

    /// A test's negative control: the host's post passes take light from
    /// beside the letters too (`CursorGlow::set_bloom_spares_letters`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_bloom_spares_letters(&mut self, on: bool) {
        self.cursor.glow.set_bloom_spares_letters(on);
    }

    /// A test's negative control: the cursor lights drawn without the letter
    /// guard (`CursorGlow::set_letter_guard`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard(on);
    }

    /// A test's negative control: the dark landing ring's rim is laid over the
    /// text, as it was (`CursorGlow::set_dark_rim_over`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_dark_rim_over(&mut self, over: bool) {
        self.cursor.glow.set_dark_rim_over(over);
    }

    /// A test's negative control: the dark landing ring's rim keeps `share`
    /// of its light under the letters (`CursorGlow::set_dark_rim_under_letters`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_dark_rim_under_letters(&mut self, share: f32) {
        self.cursor.glow.set_dark_rim_under_letters(share);
    }

    /// A test's negative control: the dark landing ring's rim is drawn
    /// however little of it keeps its whole light
    /// (`CursorGlow::set_dark_rim_fragments`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_dark_rim_fragments(&mut self, on: bool) {
        self.cursor.glow.set_dark_rim_fragments(on);
    }

    /// A test's negative control: the letter guard passes Classic's spectrum
    /// face by on paper too, as it did
    /// (`CursorGlow::set_letter_guard_classic_paper`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_classic_paper(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard_classic_paper(on);
    }

    /// A test's negative control: Classic's paper landing ring laid over the
    /// letters as it was (`CursorGlow::set_classic_paper_ring_cut`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_classic_paper_ring_cut(&mut self, on: bool) {
        self.cursor.glow.set_classic_paper_ring_cut(on);
    }

    /// A test's negative control: Classic's one-colour shade up a dim page
    /// laid faint and cut with the lights, as it was
    /// (`CursorGlow::set_classic_faint_shade`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_classic_faint_shade(&mut self, on: bool) {
        self.cursor.glow.set_classic_faint_shade(on);
    }

    /// A test's negative control: the letter guard cuts each cell alone, with
    /// no ramp along the row (`CursorGlow::set_letter_guard_ramp`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_ramp(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard_ramp(on);
    }

    /// A test's negative control: every letter read by its solid ink against its grounds' least
    /// and greatest luminance, none by its glyph's own pixels
    /// (`CursorGlow::set_letter_guard_ring`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_ring(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard_ring(on);
    }

    /// A test's negative control: each page shade kept whole, unjudged
    /// (`CursorGlow::set_letter_guard_shades`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_shades(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard_shades(on);
    }

    /// A test's negative control: each letter under a page shade held to its raster bar, not the
    /// lower of that and its own reading (`CursorGlow::set_letter_guard_shades_to_own`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_shades_to_own(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard_shades_to_own(on);
    }

    /// A test's negative control: the letter guard blind to the cursor trail's fill
    /// (`CursorGlow::set_letter_guard_trail`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_trail(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard_trail(on);
    }

    /// A test's negative control: a letter the guard reads by its solid ink keeps its bare
    /// share, no room over it (`CursorGlow::set_letter_guard_room`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_room(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard_room(on);
    }

    /// A test's negative control: one law that keeps the letter under the
    /// caret off (`CursorGlow::set_caret_letter_law`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_caret_letter_law(
        &mut self,
        law: crate::cursor_glow::CaretLetterLaw,
        on: bool,
    ) {
        self.cursor.glow.set_caret_letter_law(law, on);
    }

    /// A test's negative control: the letter guard feathers its cuts into a
    /// rail laid clear of the ring (`CursorGlow::set_letter_guard_rails_clear`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_rails_clear(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard_rails_clear(on);
    }

    /// A test's negative control: the letter guard judges every cell
    /// afresh every frame (`CursorGlow::set_letter_guard_reuse`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_reuse(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard_reuse(on);
    }

    /// A test's counters: the cells the letter guard judged afresh, those
    /// whose verdict it reused, and the frames it took whole from the frame
    /// before (`CursorGlow::letter_guard_work`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn letter_guard_work(&self) -> (u64, u64, u64) {
        self.cursor.glow.letter_guard_work()
    }

    /// A test's negative control: the frame's combining marks unread, a
    /// marked cell short by its base letter alone
    /// (`CursorGlow::set_frame_marks_unread`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_frame_marks_unread(&mut self, on: bool) {
        self.cursor.glow.set_frame_marks_unread(on);
    }

    /// A test's negative control: a lit page's bed laid at the page's
    /// luminance in the air too (`CursorGlow::set_bed_air_light`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_bed_air_light(&mut self, on: bool) {
        self.cursor.glow.set_bed_air_light(on);
    }

    /// A test's negative control: a bed sample in the air by its own cell's
    /// text alone (`CursorGlow::set_bed_air_beside_text`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_bed_air_beside_text(&mut self, on: bool) {
        self.cursor.glow.set_bed_air_beside_text(on);
    }

    /// A test's negative control: the dark bed held to its budget alone,
    /// never lit at the page's luminance (`CursorGlow::set_bed_page_lit`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_bed_page_lit(&mut self, on: bool) {
        self.cursor.glow.set_bed_page_lit(on);
    }

    /// Opt the resident pet in (or out). The pet is the host's `cursor_pet` setting: it walks
    /// beside every trail style and beside the plain cursor.
    #[inline]
    pub fn set_cursor_pet(&mut self, enabled: bool, seed: u64) {
        self.companion.set_enabled_seed(enabled, seed);
    }

    /// Which animal the resident pet is ([`crate::cursor_pet`]).
    #[inline]
    pub const fn set_cursor_pet_species(&mut self, species: PetSpecies) {
        self.companion.set_species(species);
    }

    /// The host's stable reduce-motion preference for every effect.
    #[inline]
    pub fn set_reduced_motion(&mut self, on: bool) {
        self.deco_cfg.reduced_motion = on;
        self.refresh_sparkle_cfg();
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.set_reduced_motion(on);
        }
    }

    /// The theme or palette authority changed (`set_theme` on the web hosts).
    #[inline]
    pub const fn invalidate_companion_colors(&mut self) {
        self.companion.invalidate_colors();
    }

    /// The pointer moved to `(x, y)` in frame px.
    #[inline]
    pub const fn note_pointer_px(&mut self, x: f32, y: f32) -> bool {
        self.companion.set_pointer(Some((x, y)))
    }

    /// The pointer left the surface.
    #[inline]
    pub const fn note_pointer_leave(&mut self) -> bool {
        self.companion.set_pointer(None)
    }

    /// A left press at `(x, y)` in frame px, asked first.
    #[inline]
    pub fn press_px(&mut self, x: f32, y: f32) -> PressOutcome {
        self.companion.press(self.now(), (x, y))
    }

    /// The host's pet opt-in as last set.
    #[inline]
    #[must_use]
    pub const fn cursor_pet_enabled(&self) -> bool {
        self.companion.enabled()
    }

    /// The alpha the last `apply` put on glass (`0` = no pet drawn).
    #[inline]
    #[must_use]
    pub const fn cursor_pet_alpha(&self) -> u8 {
        self.companion.alpha()
    }

    /// Count of console input intentions, independent of their later echoes.
    #[inline]
    #[must_use]
    pub const fn cursor_pet_console_input_seq(&self) -> u64 {
        self.companion.brain().console_input_seq()
    }

    /// Last classified console input intention.
    #[inline]
    #[must_use]
    pub const fn cursor_pet_console_input_kind(&self) -> Option<PetInputKind> {
        self.companion.brain().console_input_kind()
    }

    /// The pet's drawn body this frame in frame px `(x0, x1, y0, y1)`.
    #[inline]
    #[must_use]
    pub const fn companion_hit_rect(&self) -> Option<(i32, i32, i32, i32)> {
        self.companion.hit_rect()
    }

    /// How far (rows) the resident companion's head law draws its body down its row this frame,
    /// and the share of its height it fits it to
    /// ([`crate::kitty_pet::PetBrain::head_sink_fit`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn companion_head_sink_fit(&self) -> (f32, f32) {
        self.companion.brain().head_sink_fit()
    }

    /// A test's negative control: the resident companion keeps its old paths
    /// ([`crate::kitty_pet::PetBrain::set_old_paths`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_old_paths(&mut self, old: bool) {
        self.companion.set_pet_old_paths(old);
    }

    /// A test's negative control: the resident companion keeps the path it
    /// kept before `law` ([`crate::kitty_pet::PetBrain::set_old_path`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_old_path(&mut self, law: crate::kitty_pet::OldPath, old: bool) {
        self.companion.set_pet_old_path(law, old);
    }

    /// The word icons the last frame's word pass drew, grid px `(x0, x1, y0,
    /// y1)`: a test's reading of what the companion's flight keeps off.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub fn word_icon_boxes(&self) -> &[(i32, i32, i32, i32)] {
        self.cursor.glow.word_icon_boxes()
    }

    /// Who owned the block cursor on the last applied frame and its body colour.
    #[cfg(test)]
    #[inline]
    #[must_use]
    pub(crate) const fn cursor_block_fill(&self) -> Option<crate::cursor_glow::BlockFill> {
        self.block_fill
    }

    /// Serious Mode's cursor-body gate.
    #[inline]
    pub const fn set_serious(&mut self, on: bool) {
        self.serious = on;
    }

    /// Master sparkle-words switch.
    #[inline]
    pub fn set_sparkle_enabled(&mut self, on: bool) {
        if on {
            self.rebuild_sparkle(true);
        } else {
            self.sparkle = None;
            self.lexicon_conflicts.clear();
            self.decos.hard_reset();
        }
    }

    /// `true` while the sparkle master is on.
    #[inline]
    #[must_use]
    pub const fn sparkle_enabled(&self) -> bool {
        self.sparkle.is_some()
    }

    /// Per-class gates (native `[sparkle_words.<class>] enabled`).
    #[inline]
    pub fn set_sparkle_classes(&mut self, profanity: bool, feline: bool, emphasis: bool) {
        self.deco_cfg.profanity = profanity;
        self.deco_cfg.feline = feline;
        self.emphasis_enabled = emphasis;
        self.recompute_emphasis();
        self.refresh_sparkle_cfg();
    }

    /// Animated-ink knobs (native `[sparkle_words.ink]`).
    #[inline]
    pub fn set_sparkle_ink(&mut self, enabled: bool, strength: f32, sweep_ms: u32, loop_: bool) {
        let floor = if loop_ { 600 } else { 350 };
        self.deco_cfg.ink_enabled = enabled;
        self.deco_cfg.ink_strength = strength.clamp(0.0, 1.0);
        self.deco_cfg.ink_sweep_ms = sweep_ms.clamp(floor, 6000);
        self.deco_cfg.ink_loop = loop_;
        self.recompute_emphasis();
        self.refresh_sparkle_cfg();
    }

    /// Force the static path (no twinkle/jitter/pulse; novas collapse to the static glint).
    #[inline]
    pub fn set_sparkle_reduced_motion(&mut self, on: bool) {
        self.set_reduced_motion(on);
    }

    /// Alt-screen suppression (native `[sparkle_words] suppress_in_alt_screen`, default off).
    #[inline]
    pub fn set_sparkle_alt_screen_suppression(&mut self, on: bool) {
        self.deco_cfg.suppress_in_alt_screen = on;
        self.refresh_sparkle_cfg();
    }

    /// Feline knobs (native `[sparkle_words.feline]`).
    #[inline]
    pub fn set_sparkle_feline(
        &mut self,
        style: &str,
        magic: bool,
        allow_bare_cat: bool,
        cjk_single_char: bool,
    ) {
        self.deco_cfg.feline_style = if style.eq_ignore_ascii_case("paw") {
            crate::word_decorations::FelineStyle::Paw
        } else {
            crate::word_decorations::FelineStyle::Cat
        };
        self.deco_cfg.feline_magic = magic;
        self.deco_cfg.allow_bare_cat = allow_bare_cat;
        self.deco_cfg.cjk_single_char = cjk_single_char;
        self.refresh_sparkle_cfg();
    }

    /// Profanity knobs (native `[sparkle_words.profanity]`).
    #[inline]
    #[allow(
        clippy::too_many_arguments,
        reason = "one call mirrors the native profanity table; a builder would relocate the list"
    )]
    pub fn set_sparkle_profanity(
        &mut self,
        style: &str,
        density: u32,
        anim_ms: u32,
        jitter: i8,
        intensity: f32,
        magic: bool,
        supernova_chance: u32,
    ) {
        self.deco_cfg.profanity_style = if style.eq_ignore_ascii_case("sparkle") {
            crate::word_decorations::ProfanityStyle::Sparkle
        } else if style.eq_ignore_ascii_case("nova") {
            crate::word_decorations::ProfanityStyle::Nova
        } else {
            crate::word_decorations::ProfanityStyle::Rainbow
        };
        self.deco_cfg.density = density.clamp(1, 12);
        self.deco_cfg.anim_ms = u64::from(anim_ms).clamp(350, 10_000);
        self.deco_cfg.jitter = jitter.clamp(0, 6);
        self.deco_cfg.intensity = intensity.clamp(0.0, 1.0);
        self.deco_cfg.profanity_magic = magic;
        self.deco_cfg.supernova_chance = supernova_chance.min(100) as u8;
        self.refresh_sparkle_cfg();
    }

    /// The `[[sparkle_words.custom]]` TOML fragment (as the native config carries it).
    #[inline]
    pub fn set_sparkle_custom_specs(&mut self, toml: Option<String>) {
        let entries = match toml.as_deref().map(crate::spec::parse_custom_toml) {
            None => {
                self.custom_spec_warning = None;
                Vec::new() // explicit clear
            }
            Some(Ok(entries)) => {
                self.custom_spec_warning = None;
                entries
            }
            Some(Err(e)) => {
                self.custom_spec_warning = Some(format!("sparkle_words.custom: {e}"));
                return;
            }
        };
        let (table, lexicon) = crate::spec::build_custom(&entries);
        self.deco_cfg.spec_table = table;
        self.custom_lexicon = lexicon;
        self.recompute_emphasis();
        self.rebuild_sparkle(false);
    }

    /// The emphasis resolve, as in the native resolver.
    #[inline]
    const fn recompute_emphasis(&mut self) {
        self.deco_cfg.emphasis = self.emphasis_enabled
            && (self.deco_cfg.ink_enabled || self.deco_cfg.spec_table.has_custom());
    }

    /// Languages whose ambiguous homograph entries un-gate.
    #[inline]
    pub fn set_sparkle_languages(&mut self, languages: &[&str]) {
        self.languages.clear();
        if languages.is_empty() {
            self.languages.push("en".to_string());
        } else {
            self.languages.reserve(languages.len());
            let mut i = 0;
            while i < languages.len() {
                self.languages.push(languages[i].to_string());
                i += 1;
            }
        }
        self.rebuild_sparkle(false);
    }

    /// User lexicon-override TOML merged over the builtin.
    #[inline]
    pub fn set_sparkle_lexicon_override(&mut self, toml: Option<String>) {
        self.override_toml = toml;
        self.rebuild_sparkle(false);
    }

    /// Folded surfaces to never decorate (native global `deny` + `ignore_words`).
    #[inline]
    pub fn set_sparkle_deny(&mut self, words: &[&str]) {
        self.deco_cfg.ignore.clear();
        self.deco_cfg.ignore.reserve(words.len());
        let mut i = 0;
        while i < words.len() {
            self.deco_cfg.ignore.insert(aterm_lexicon::fold(words[i]));
            i += 1;
        }
        self.refresh_sparkle_cfg();
    }

    /// Lexicon build diagnostics.
    #[inline]
    #[must_use]
    pub fn sparkle_lexicon_warnings(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(
            usize::from(self.custom_spec_warning.is_some()) + self.lexicon_conflicts.len(),
        );
        if let Some(w) = &self.custom_spec_warning {
            out.push(w.clone());
        }
        let mut i = 0;
        while i < self.lexicon_conflicts.len() {
            let w = &self.lexicon_conflicts[i];
            if lexicon_warning_applies(w, self.deco_cfg.cjk_single_char) {
                out.push(w.clone());
            }
            i += 1;
        }
        out
    }

    /// Re-resolve the active config.
    #[inline]
    fn refresh_sparkle_cfg(&mut self) {
        if let Some(rs) = self.sparkle.as_mut() {
            rs.cfg = self.deco_cfg.clone();
            self.decos.hard_reset();
        }
    }

    /// (Re)compile the lexicon + resolved config.
    #[inline]
    fn rebuild_sparkle(&mut self, force: bool) {
        if self.sparkle.is_none() && !force {
            return;
        }
        let mut refs = Vec::with_capacity(self.languages.len());
        let mut i = 0;
        while i < self.languages.len() {
            refs.push(self.languages[i].as_str());
            i += 1;
        }
        let mut over = self.override_toml.clone().unwrap_or_default();
        over.push_str(&self.custom_lexicon);
        let over_ref = if over.is_empty() {
            None
        } else {
            Some(over.as_str())
        };
        let lexicon = match aterm_lexicon::Lexicon::with_languages_and_override(&refs, over_ref) {
            Ok(lex) => lex,
            Err(_) => aterm_lexicon::Lexicon::with_languages(&refs),
        };
        self.lexicon_conflicts = lexicon.conflicts().to_vec();
        self.sparkle = Some(Resolved {
            cfg: self.deco_cfg.clone(),
            lexicon: std::sync::Arc::new(lexicon),
        });
        self.decos.hard_reset();
    }

    /// Master rain switch.
    #[inline]
    pub fn set_matrix_rain_enabled(&mut self, on: bool) {
        self.rain_cfg.enabled = on;
        if !on {
            self.rain = None;
        } else if self.rain.is_none() {
            let mut rain = Box::new(MatrixRain::new(self.rain_cfg));
            rain.set_visibility(self.rain_visibility);
            rain.set_reduced_motion(self.deco_cfg.reduced_motion);
            rain.set_material_editing(self.rain_material_editing);
            self.rain_clock_ms = self.clock.as_millis() as u64;
            self.rain = Some(rain);
        }
    }

    /// `true` while the rain master is on.
    #[inline]
    #[must_use]
    pub const fn matrix_rain_enabled(&self) -> bool {
        self.rain.is_some()
    }

    /// Configure the output streak (`crate::output_streak`).
    #[inline]
    #[allow(
        clippy::too_many_arguments,
        reason = "one call mirrors the native StreakConfig resolution; a builder would relocate the list"
    )]
    pub fn set_output_streak(
        &mut self,
        enabled: bool,
        intensity: f32,
        tail: u32,
        max_streaks: u32,
        idle_secs: u32,
        sound: bool,
        seed: u64,
    ) {
        self.streak_cfg = StreakConfig {
            enabled,
            intensity: if intensity.is_finite() {
                intensity.clamp(0.0, 1.0)
            } else {
                0.0
            },
            tail: tail.clamp(u32::from(TAIL_MIN), u32::from(TAIL_MAX)) as u16,
            max_streaks: max_streaks.clamp(1, MAX_COMETS as u32) as u8,
            idle_secs: idle_secs.clamp(2, 120) as f32,
            sound,
            ..self.streak_cfg
        };
        self.streak_seed = seed;
        if enabled {
            if self.streak.is_none() {
                self.streak = Some(Box::new(OutputStreak::new(seed)));
            }
        } else {
            self.streak = None;
        }
    }

    /// Fold the resolved theme into the streak.
    #[inline]
    pub const fn set_output_streak_theme(
        &mut self,
        dark_theme: bool,
        theme_bg: u32,
        theme_fg: u32,
        theme_cursor: u32,
    ) {
        self.streak_cfg.dark_theme = dark_theme;
        self.streak_cfg.theme_bg = theme_bg & 0x00FF_FFFF;
        self.streak_cfg.theme_fg = theme_fg & 0x00FF_FFFF;
        self.streak_cfg.theme_cursor = theme_cursor & 0x00FF_FFFF;
    }

    /// Reduced motion for the streak.
    #[inline]
    pub const fn set_output_streak_reduced_motion(&mut self, reduced: bool) {
        self.streak_reduced_motion = reduced;
    }

    /// Forward one keystroke to the streak's echo discount.
    #[inline]
    pub fn note_output_streak_keystroke(&mut self) {
        let now = self.now();
        if let Some(s) = self.streak.as_deref_mut() {
            s.note_keystroke(now);
        }
    }

    /// Configure the rain knobs (native `[matrix_rain]`), kept across off/on toggles.
    #[inline]
    #[allow(
        clippy::too_many_arguments,
        reason = "one call mirrors the native matrix_rain table; a builder would relocate the list"
    )]
    pub fn set_matrix_rain(
        &mut self,
        fps: u32,
        density: u32,
        speed: u32,
        trail: u32,
        alpha: Option<u32>,
        head_alpha: Option<u32>,
        hue: &str,
        hue_color: Option<u32>,
        mutation_ms: u32,
        idle_secs: u32,
        suppress_in_alt_screen: bool,
        turn_wave: bool,
        bell_alert: bool,
        output_material: bool,
        seed: u64,
        default_bg: u32,
        theme_fg: u32,
    ) {
        self.rain_cfg = RainConfig {
            enabled: self.rain_cfg.enabled,
            fps: fps.clamp(12, 60) as u8,
            density: density.clamp(1, 12) as u8,
            speed: speed.clamp(1, 10) as u8,
            trail: trail.clamp(1, 10) as u8,
            alpha_override: clamp_rain_alpha(alpha),
            head_alpha_override: clamp_rain_alpha(head_alpha),
            hue: if hue.eq_ignore_ascii_case("theme") {
                RainHue::Theme
            } else if hue.eq_ignore_ascii_case("custom") {
                match hue_color {
                    Some(c) => RainHue::Custom(c & 0x00FF_FFFF),
                    None => RainHue::Matrix,
                }
            } else {
                RainHue::Matrix
            },
            mutation_ms: mutation_ms.clamp(80, 2000) as u16,
            idle_secs: idle_secs.clamp(2, 120) as u16,
            suppress_in_alt_screen,
            turn_wave,
            bell_alert,
            exit_tint: true,
            output_material,
            seed,
            default_bg: default_bg & 0x00FF_FFFF,
            theme_fg: theme_fg & 0x00FF_FFFF,
            theme_accent: self.rain_cfg.theme_accent,
        };
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.set_config(self.rain_cfg);
        }
    }

    /// Update theme-derived rain colors without the host replaying every matrix-rain knob after a
    /// live theme change.
    #[inline]
    pub fn set_matrix_rain_theme(&mut self, default_bg: u32, theme_fg: u32, theme_accent: u32) {
        let default_bg = default_bg & 0x00FF_FFFF;
        let theme_fg = theme_fg & 0x00FF_FFFF;
        let theme_accent = theme_accent & 0x00FF_FFFF;
        if self.rain_cfg.default_bg == default_bg
            && self.rain_cfg.theme_fg == theme_fg
            && self.rain_cfg.theme_accent == theme_accent
        {
            return;
        }
        self.rain_cfg.default_bg = default_bg;
        self.rain_cfg.theme_fg = theme_fg;
        self.rain_cfg.theme_accent = theme_accent;
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.set_config(self.rain_cfg);
        }
    }

    /// Reduce-motion for rain, which sets it for every effect ([`Self::set_reduced_motion`]).
    #[inline]
    pub fn set_matrix_rain_reduced_motion(&mut self, on: bool) {
        self.set_reduced_motion(on);
    }

    /// Bind cursor geometry to this exact frame.
    #[inline]
    fn sync_cursor_coordinate_space(
        &mut self,
        current: CursorCoordinateSpace,
        scroll: ContentScrollState,
    ) -> bool {
        let changed = matches!(
            self.cursor_coordinate_space,
            Some(previous) if previous != current
        );
        self.cursor_coordinate_space = Some(current);
        if changed {
            self.cursor.retire_body_geometry();
            let now = self.now();
            self.cursor.glow.curtain(now);
            self.cursor_scroll_state = Some(scroll);
            self.companion.rebase_geometry();
            self.decos.rebase_kitty_cursor_placement();
            self.flying_sky_row = None;
        }
        changed
    }

    /// Run every enabled effect for the current injected instant and fill the overlay channels.
    ///
    /// The web host's frame latches no light scheme (`docs/design/PRISM-GLASS.md` §15.2 1.11):
    /// its glow config's light stays [`crate::cursor_glow::Light::OFF`] on every page, light
    /// ones included, so it draws main's frame and no light arm, and its frame light
    /// ([`Self::frame_light`]) is OFF.
    #[inline]
    pub fn apply(
        &mut self,
        term: &mut Terminal,
        input: &mut RenderInput,
        cell_w: usize,
        cell_h: usize,
    ) -> u64 {
        let content_scroll_state = term.content_scroll_state();
        let coordinate_space = CursorCoordinateSpace {
            rows: input.rows,
            cols: input.cols,
            cell_w,
            cell_h,
            chrome_pad: self.chrome_pad,
            chrome_head: self.chrome_head,
        };
        if !self.enabled_any() {
            self.cursor_scroll_state = Some(content_scroll_state);
            self.cursor_coordinate_space = Some(coordinate_space);
            self.cursor_owner_space = Some((term.render_identity(), term.is_alternate_screen()));
            self.companion.retire();
            self.cursor.retire_bodies();
            self.rain_latches.observe_without_engine(
                term.render_identity(),
                term.shell_state() == aterm_core::terminal::ShellState::Executing,
                term_cmd_done_seq(term),
            );
            input.cursor_trail.clear();
            input.cursor_glow_add.clear();
            input.cursor_glow_add_ramps.clear();
            input.glow_halo.clear();
            input.fire_patch.clear();
            input.glow_under.clear();
            input.glow_under_ramps.clear();
            input.char_fg.clear();
            input.fire_halo.clear();
            #[cfg(test)]
            {
                self.block_fill = None;
            }
            input.cursor_fill_override = None;
            input.cursor_effect_style_override = None;
            // The last lit frame's gate goes with its light (a terminal that never lit one reads Open).
            input.bloom_gate = aterm_core::render::BloomGate::Open;
            input.word_decorations.clear();
            input.ink.clear();
            self.sprite_layers.withdraw(&mut input.fx_layers);
            input.nova_add.clear();
            self.rain_layers.withdraw(&mut input.fx_layers);
            input.rain_add.clear();
            return 0;
        }
        let coordinate_changed =
            self.sync_cursor_coordinate_space(coordinate_space, content_scroll_state);
        let alt = term.is_alternate_screen();
        self.program_owns_line =
            alt || term.shell_state() == aterm_core::terminal::ShellState::Executing;
        let now = self.now();
        let owner_changed = self.fence_owner((term.render_identity(), alt), now);
        let scroll = self.sync_scroll(content_scroll_state, now);
        let scrolled = !coordinate_changed && !owner_changed && scroll.translated_rows > 0;
        self.cursor.glow.note_context(alt);
        self.cursor.trail.note_context(alt);
        let (rows, cols) = (input.rows, input.cols);
        let cursor_snapshot_coherent = input.process_sequence
            == term.pipeline_timestamps().process_sequence
            && input.terminal_id == term.render_identity()
            && input.engine_alt == alt;
        if !cursor_snapshot_coherent {
            self.cursor.glow.reset();
        }
        let live_viewport = cursor_snapshot_coherent && input.display_offset == 0;
        if !live_viewport {
            input.cursor_visible = false;
        }
        let focused = self.rain_visibility == RainVisibility::Focused;
        let cursor_motion =
            MotionPolicy::resolve(MotionMode::Auto, self.deco_cfg.reduced_motion, focused);
        let motion_amp = cursor_motion.amplitude(MotionEffect::CursorGlow);
        let animate = cursor_motion.animate(MotionEffect::CursorGlow);
        let cur = if live_viewport && input.cursor_visible {
            Some((input.cursor_row as u16, input.cursor_col as u16))
        } else {
            None
        };
        // Main's page, and no light: the web host latches no scheme (§15.2 1.11), so nothing
        // here writes `glow_cfg.light`, which stays OFF as `new` and `resolve_glow` leave it.
        self.glow_cfg.dark_theme =
            input.default_bg == aterm_core::render::COLOR_UNSET || theme_is_dark(input.default_bg);
        self.glow_cfg.theme_fg = input.default_fg;
        self.glow_cfg.theme_bg = input.default_bg;
        let cursor_color_pinned = term.cursor_color_recoloured();
        if input.cursor_color != aterm_core::render::COLOR_UNSET {
            crate::cursor_fx::follow_live_cursor(
                &mut self.glow_cfg,
                &mut self.trail_cfg,
                input.cursor_color & 0x00FF_FFFF,
                cursor_color_pinned,
                self.glow_trail_color,
                self.glow_explicit_accent,
                self.trail_body_color,
            );
        }

        let probe_row = if let Some((row, caret)) = cur
            && let Some(render_row) = input.cells.get(usize::from(row))
        {
            probe_chars(render_row, input.cols, &mut self.row_probe_scratch);
            Some((row, caret))
        } else {
            None
        };
        // The flanks a test's host asked for: each neighbour row the frame holds, off the grid
        // where it holds none (the window's `row > 0` and `row + 1 < rows`).
        #[cfg(any(test, debug_assertions))]
        let flank_rows = match (probe_row, self.probe_flanks.as_mut()) {
            (Some((row, _)), Some((above, below))) => {
                let row = usize::from(row);
                let over = row.checked_sub(1).and_then(|r| input.cells.get(r));
                let under = input.cells.get(row + 1).filter(|_| row + 1 < input.rows);
                if let Some(cells) = over {
                    probe_chars(cells, input.cols, above);
                }
                if let Some(cells) = under {
                    probe_chars(cells, input.cols, below);
                }
                Some((over.is_some(), under.is_some()))
            }
            _ => None,
        };

        let mut trail_cfg = self.trail_cfg;
        trail_cfg.enabled &= animate;
        let glow_geom = self.chrome_geom(rows, cols, cell_w, cell_h);
        let configured_intensity = self.glow_cfg.intensity;
        let configured_head_dx = self.glow_cfg.head_dx;
        self.glow_cfg.intensity *= motion_amp;
        self.glow_cfg.head_dx = crate::cursor_fx::head_dx_for(input.cursor_style);
        #[cfg(any(test, debug_assertions))]
        if self.bar_unanchored {
            self.glow_cfg.head_dx = 0.5;
        }
        let default_bg = if input.default_bg == aterm_core::render::COLOR_UNSET {
            COLD_THEME_BG
        } else {
            input.default_bg & 0x00FF_FFFF
        };
        let pending_keys = std::mem::take(&mut self.pending_keys);
        let term_cursor_color = terminal_cursor_color(term);
        // The frame light, as `cursor_step` records it (the step borrows `self.cursor` alone).
        self.frame_light = Some((now, self.glow_cfg.light));
        let step = native::step_cursor_family(
            &mut self.cursor,
            &native::CursorStep::new(
                CursorFxInput {
                    now,
                    cur,
                    live_viewport,
                    cursor_style: input.cursor_style,
                    blink_phase: true,
                    live_cursor: term_cursor_color,
                    cursor_color_pinned,
                    default_bg,
                    cursor_cell_bg: if let Some((row, col)) = cur
                        && let Some(r) = input.cells.get(usize::from(row))
                        && let Some(cell) = r.get(usize::from(col))
                    {
                        Some(aterm_render::rgb_to_u32(cell.bg))
                    } else {
                        None
                    },
                    geom: glow_geom,
                    focused,
                    shed_envelope: 1.0,
                    trail_brightness: 1.0,
                    body_allowed: !self.serious,
                    momentum_glow: self.glow_cfg.enabled,
                    user_tinted: self.glow_trail_color.chosen(),
                    accent_pinned: self.glow_explicit_accent,
                },
                &self.glow_cfg,
                trail_cfg,
            )
            .with_animate(animate)
            .with_frame_cells(Some(&input.cells))
            .with_frame_cells_cols(input.cols)
            .with_frame_marks(&input.combining)
            .with_row_probe(match probe_row {
                Some((row, caret)) => Some(native::RowProbe {
                    row,
                    caret,
                    trust: crate::cursor_glow::ProbeTrust::Full,
                    cols: &self.row_probe_scratch,
                    #[cfg(any(test, debug_assertions))]
                    flanks: match (flank_rows, self.probe_flanks.as_ref()) {
                        (Some((over, under)), Some((above, below))) => Some((
                            over.then_some(above.as_slice()),
                            under.then_some(below.as_slice()),
                        )),
                        _ => None,
                    },
                    #[cfg(not(any(test, debug_assertions)))]
                    flanks: None,
                }),
                None => None,
            })
            .with_pending_keys(pending_keys)
            .with_caret_drawn(input.cursor_visible),
            std::iter::empty(),
        );
        let fx = step.frame;
        self.glow_cfg.intensity = configured_intensity;
        self.glow_cfg.head_dx = configured_head_dx;
        let trail_fp = fx.trail_fp;
        let glow_fp = fx.glow_fp;
        self.publish_cursor(input);
        if self.trail_cfg.enabled {
            input.cursor_trail_color = fx.trail_color;
        }
        #[cfg(test)]
        {
            self.block_fill = fx.block_fill;
        }
        input.cursor_fill_override = fx.caret_fill();
        input.cursor_effect_style_override =
            compose_caret_style_override(fx.bolt_cursor, fx.twinkle_cursor, fx.momentum_steady);

        self.open_words();
        let geom = EffectGeom {
            cell_w: cell_w as u16,
            cell_h: cell_h as u16,
            rows: rows as u16,
            cols: cols as u16,
        };
        let mut facts = TerminalFacts::read(term, term.render_identity(), scrolled);
        facts.caret = if live_viewport {
            Some((input.cursor_row as u16, input.cursor_col as u16))
        } else {
            None
        };
        facts.cursor_visible = cur.is_some();
        facts.live_viewport = live_viewport;
        facts.display_offset = if cursor_snapshot_coherent {
            input.display_offset
        } else {
            facts.display_offset
        };
        let host = HostFrameInput::new(
            now,
            FrameGeom {
                rows: geom.rows,
                cols: geom.cols,
                cell_w: geom.cell_w,
                cell_h: geom.cell_h,
                origin_px: (
                    i32::from(self.chrome_pad),
                    i32::from(self.chrome_pad)
                        + i32::from(self.chrome_head)
                        + i32::from(self.host_rows_px),
                ),
            },
        )
        .with_visibility(match self.rain_visibility {
            RainVisibility::Focused => Visibility::Focused,
            RainVisibility::VisibleUnfocused => Visibility::VisibleUnfocused,
            RainVisibility::Hidden => Visibility::Hidden,
        })
        .with_reduced_motion(!animate)
        .with_serious(self.serious);
        if self.companion.enabled() {
            let pet_world = crate::pet_world::PetWorldFacts::read(term, term.render_identity());
            let _ = self.companion.observe_console(
                input,
                &pet_world,
                crate::pet_world::PetPane::full(input),
                &[],
            );
        }
        let pet = self.pet_prepare(
            PetFacts::new(
                &facts,
                &host,
                PetOwnership {
                    on: self.companion.enabled(),
                },
            )
            .with_focused(focused),
        );
        if self.companion.grieving() {
            self.cursor.glow.hush_fanfare(now);
        }
        if self.cursor.glow.take_flow_entry() {
            self.companion.note_flow(now);
        }
        let pet = self.companion.tick(pet);
        self.offer_pet_v2(&pet.pet, glow_geom, (0, 0), now);
        let _ = self.cursor.glow.take_companion_impulse();

        // The word pass and the pet refill the sprite channel from empty (it is
        // published, not handed over, so nothing else empties it).
        self.free_scratch.clear();
        let mut damage_consumed = false;
        let deco_fp = match self.sparkle.take() {
            None => 0,
            Some(rs) => {
                let fp = if alt && rs.cfg.suppress_in_alt_screen {
                    self.suspend_words(now);
                    0
                } else if input.cells.len() >= rows && input.line_sizes.len() >= rows {
                    let epoch = term.damage_epoch();
                    let rescan = if self.decos.needs_rescan(epoch) {
                        Some(WordRescan {
                            birth: now,
                            cursor: if live_viewport {
                                Some((input.cursor_row as u16, input.cursor_col as u16))
                            } else {
                                None
                            },
                        })
                    } else {
                        None
                    };
                    let fp = self
                        .word_grid(
                            &WordGrid::new(
                                now,
                                &rs.lexicon,
                                &rs.cfg,
                                geom,
                                &input.cells,
                                &input.line_sizes,
                                SelView {
                                    sel: term.text_selection(),
                                    display_offset: term.grid().display_offset() as i32,
                                },
                            )
                            .with_base_y(input.base_y)
                            .with_default_bg(default_bg)
                            .with_epoch(epoch)
                            .with_rescan(rescan)
                            .with_companion_at(pet.companion)
                            .with_focused(focused),
                        )
                        .fp;
                    close_open_damage_session(term);
                    damage_consumed = true;
                    fp
                } else {
                    self.decos.thaw(now);
                    let epoch = term.damage_epoch();
                    if self.decos.needs_rescan(epoch) {
                        self.decos
                            .rescan(term, rows, cols, &rs.lexicon, &rs.cfg, epoch, now);
                    }
                    close_open_damage_session(term);
                    damage_consumed = true;
                    let sel_view = SelView {
                        sel: term.text_selection(),
                        display_offset: term.grid().display_offset() as i32,
                    };
                    let fp = self.decos.tick(
                        now,
                        &rs.cfg,
                        geom,
                        pet.companion,
                        Some(sel_view),
                        focused,
                        &mut self.deco_scratch,
                        &mut self.ink_scratch,
                        &mut self.free_scratch,
                        &mut self.nova_scratch,
                    );
                    // The word pass from the terminal names its icons to the
                    // next frame's sky, ribbon and embers as the grid's does
                    // (looks7 M10): unnamed, a host that hands no cells laid
                    // stars and embers on the `fox`.
                    #[cfg(test)]
                    let named = !self.word_icons_unnamed;
                    #[cfg(not(test))]
                    let named = true;
                    if named {
                        self.note_word_icons(0);
                    }
                    fp
                };
                self.sparkle = Some(rs);
                fp
            }
        };
        self.forward_peek_cues(now, (geom.cell_w, geom.cell_h));
        let (pet_fp, _sync) = self.companion.emit(
            &pet,
            &input.cells,
            ContrastFallback {
                bg: default_bg,
                cursor: pet_contrast_cursor(input.cursor_color, term_cursor_color),
                accent: self.glow_cfg.accent,
            },
            &mut self.decos,
            &mut self.free_scratch,
        );
        let deco_fp = deco_fp ^ pet_fp ^ pet.fp.rotate_left(53);
        std::mem::swap(&mut input.word_decorations, &mut self.deco_scratch);
        std::mem::swap(&mut input.ink, &mut self.ink_scratch);
        self.sprite_layers
            .publish(&self.free_scratch, &mut input.fx_layers);
        self.shade_word_icons(input);
        let streak_fp = if let Some(streak) = self.streak.as_deref_mut() {
            let mut cfg = self.streak_cfg;
            if self.streak_reduced_motion {
                cfg.intensity = 0.0;
            } else {
                cfg.intensity *= cursor_motion.amplitude(MotionEffect::OutputStreak);
            }
            streak.note_output_cells(
                &input.cells,
                (input.cursor_row, input.cursor_col),
                (rows, cols),
                term.content_seq(),
                now,
                false,
            );
            streak.tick(now, glow_geom, &cfg, &mut self.nova_scratch).fp
        } else {
            0
        };
        std::mem::swap(&mut input.nova_add, &mut self.nova_scratch);

        let rain_session = term.render_identity();
        let shell_executing = term.shell_state() == aterm_core::terminal::ShellState::Executing;
        let rain_fp = if let Some(rain) = self.rain.as_deref_mut() {
            let epoch = term.damage_epoch();
            let torn = input.snapshot_seq != epoch;
            let display_offset = term.grid().display_offset() as i32;
            if display_offset != 0 {
                rain.defer_reading();
            }
            if !damage_consumed {
                close_open_damage_session(term);
            }
            if torn {
                rain.note_activity(term.content_seq());
                self.rain_scratch.clear();
                self.rain_add_scratch.clear();
                0
            } else {
                let sel = SelView {
                    sel: term.text_selection(),
                    display_offset,
                };
                let execute_edge = self
                    .rain_latches
                    .note_shell_executing(rain_session, shell_executing);
                rain.host_frame(
                    &RainFrame {
                        clock: RainClock::Accumulated,
                        suspended: term.is_alternate_screen()
                            && self.rain_cfg.suppress_in_alt_screen,
                        animate: !self.deco_cfg.reduced_motion,
                        visibility: self.rain_visibility,
                        session: rain_session,
                        content_seq: term.content_seq(),
                        execute_edge,
                        cmd_done: term_cmd_done(term),
                        scan: if display_offset == 0
                            && input.cells.len() >= rows
                            && input.line_sizes.len() >= rows
                        {
                            Some(RainScan {
                                cells: &input.cells,
                                line_sizes: &input.line_sizes,
                                images: &input.images,
                                rows,
                                cols,
                                default_bg: input.default_bg,
                                epoch,
                            })
                        } else {
                            None
                        },
                        cursor: cur,
                        geom,
                        tick: RainTickInput {
                            cursor: cur,
                            hidden_band: &[],
                            sel: Some(sel),
                            display_offset,
                            is_alt_screen: term.is_alternate_screen(),
                        },
                    },
                    &mut self.rain_latches,
                    &mut self.rain_scratch,
                    &mut self.rain_add_scratch,
                )
            }
        } else {
            self.rain_latches.observe_without_engine(
                rain_session,
                shell_executing,
                term_cmd_done_seq(term),
            );
            self.rain_scratch.clear();
            self.rain_add_scratch.clear();
            0
        };
        match self.rain.as_deref() {
            Some(rain) => {
                self.rain_layers
                    .publish(&self.rain_scratch, rain.baker(), &mut input.fx_layers);
            }
            None => self.rain_layers.withdraw(&mut input.fx_layers),
        }
        std::mem::swap(&mut input.rain_add, &mut self.rain_add_scratch);

        trail_fp
            ^ glow_fp.rotate_left(21)
            ^ deco_fp.rotate_left(42)
            ^ rain_fp.rotate_left(11)
            ^ streak_fp.rotate_left(33)
    }
}

#[path = "pipeline_native.rs"]
mod native;
pub use native::{
    CanvasInk, CursorStep, CursorStepOut, Delivery, Flanks, FlyingSeat, NovaInto, PaneDrain,
    PaneGrid, RowProbe, ScrollChange, StreakEcho, StreakGrid, StreakKnobs, StreakPane, TornBand,
    WordGrid, WordGridOut, WordPane, WordPanes, WordPanesOut, WordRescan,
};

#[cfg(any(test, debug_assertions))]
mod state_words;

#[cfg(test)]
#[path = "pipeline_input_tests.rs"]
mod input_contract_tests;

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;
