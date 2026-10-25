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

use crate::companion::{CompanionOwner, ContrastFallback, GlowOwnership, PetFacts};
use crate::cursor_fx::{CursorFx, CursorFxInput, brighten, compose_caret_style_override};
use crate::cursor_glow::{Geom, GlowConfig, GlowStyle};
use crate::cursor_momentum::MOMENTUM_GLOW_TAU_S;
use crate::cursor_trail::TrailConfig;
#[cfg(test)]
use crate::host::Wake;
use crate::host::{
    CaptureMode, FrameGeom, HostFrameInput, PressOutcome, SingFacts, TerminalFacts, Visibility,
};
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
    let c = term
        .cursor_color()
        .unwrap_or_else(|| term.default_foreground());
    aterm_render::rgb_to_u32([c.r, c.g, c.b])
}

/// The most recent completed command as the rain driver reads it.
#[inline]
fn term_cmd_done(term: &Terminal) -> Option<(u64, i32)> {
    term.last_completed_command()
        .and_then(|m| Some((term.completed_command_seq(), m.exit_code?)))
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
    /// A test's negative control: the pet's veil drawn in the trail's step,
    /// by the body the pet drew a frame before ([`Self::veil_pet_letters`]).
    #[cfg(test)]
    pet_veil_stale: bool,
    /// The opaque comet's own colour, where `set_cursor_trail` gave one.
    trail_body_color: Option<u32>,
    /// Keystrokes noted since the last [`Self::advance`], replayed evenly across its `dt`.
    pending_keys: u32,
    /// Reused row material for the glow engine's row probe (`CursorGlow::observe_row`).
    row_probe_scratch: Vec<char>,
    /// The caret row the flying head last sat in the sky seat over
    /// ([`crate::rainbow_kitty::companion::SeatQuery::held_sky`]).
    flying_sky_row: Option<u16>,

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
    /// The glow style string named a pet at the last [`Self::set_cursor_glow`].
    pet_style_named: bool,
    /// Output-streak engine.
    streak: Option<Box<OutputStreak>>,
    /// The streak knobs, kept across off/on toggles.
    streak_cfg: StreakConfig,
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
            },
            glow_trail_color: crate::cursor_fx::TrailColor::Automatic,
            glow_explicit_accent: false,
            glow_request: None,
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
            #[cfg(test)]
            pet_veil_stale: false,
            pending_keys: 0,
            row_probe_scratch: Vec::new(),
            flying_sky_row: None,
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
            pet_style_named: false,
            streak: None,
            streak_cfg: StreakConfig::default(),
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
        self.clock += Duration::from_secs_f64(dt_ms / 1000.0);
        debug_assert!(self.clock > prev, "the injected clock is monotonic");
        let n = self.pending_keys;
        if n > 0 {
            self.pending_keys = 0;
            let dt = self.clock - prev;
            let mut newest = self.t0 + self.clock;
            let inv_n = 1.0 / f64::from(n);
            for i in 1..=n {
                let at = self.t0 + prev + dt.mul_f64(f64::from(i) * inv_n);
                self.cursor.cadence.on_keystroke(at);
                self.cursor.momentum.on_key(at, MOMENTUM_GLOW_TAU_S);
                self.cursor.glow.note_typed_cells(at, 1);
                newest = at;
            }
            self.cursor.trail.note_typed_batch(newest, n);
        }
        if let Some(rain) = self.rain.as_deref_mut() {
            let total = self.clock.as_millis() as u64;
            rain.advance_ms(total.saturating_sub(self.rain_clock_ms));
            self.rain_clock_ms = total;
        }
    }

    /// The current injected instant (`t0 + clock`).
    #[inline]
    #[must_use]
    pub fn now(&self) -> Instant {
        self.t0 + self.clock
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
            || self.decos.is_active(now)
            || self.companion.needs_frames()
            || self.companion.next_change_deadline(now).is_some()
            || self.rain.as_ref().is_some_and(|r| r.is_active())
            || self.streak.as_ref().is_some_and(|s| s.is_active())
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
        let rain_ms = self
            .rain
            .as_ref()
            .filter(|rain| rain.is_active())
            .map(|rain| rain.next_tick_in_ms() as f64);
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
        [glow_ms, pet_ms, rain_ms, streak_ms, body_ms]
            .into_iter()
            .flatten()
            .reduce(f64::min)
    }

    /// One scheduling verdict ([`Wake`]).
    #[must_use]
    #[cfg(test)]
    pub fn wake(&mut self) -> Wake {
        match self.next_deadline_ms() {
            Some(ms) => Wake::At(self.now() + Duration::from_secs_f64(ms / 1000.0)),
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
    pub fn note_keystroke(&mut self) {
        self.pending_keys = self.pending_keys.saturating_add(1).min(64);
        self.rain_material_editing = true;
        if let Some(rain) = self.rain.as_deref_mut() {
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
        let editing = |me: &mut Self| {
            me.cursor.cadence.on_keystroke(now);
            me.rain_material_editing = true;
            if let Some(rain) = me.rain.as_deref_mut() {
                rain.note_keystroke();
            }
        };
        match kind {
            PetInputKind::Delete => {
                editing(self);
                self.cursor.glow.note_backspace(now);
            }
            PetInputKind::Kill => {
                editing(self);
                self.cursor.glow.note_kill(now, true);
            }
            PetInputKind::KillForward => {
                editing(self);
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

    /// A kitty command was typed (`sit`, `kitty jump`, `good kitty`).
    #[cfg(test)]
    pub fn note_trick(&mut self, trick: aterm_lexicon::Trick, confirmed: bool) {
        self.companion.note_trick(self.now(), trick, confirmed);
    }

    /// The typed line turned into prose, or was aborted.
    #[cfg(test)]
    pub const fn revoke_trick(&mut self) {
        self.companion.revoke_trick();
    }

    /// The whole submitted line was pet talk.
    #[cfg(test)]
    pub fn note_trick_submit(&mut self) {
        self.companion.note_trick_submit(self.now());
    }

    /// Embedded host observed wheel/PgUp while an alternate-screen tui is active.
    #[inline]
    pub fn note_matrix_rain_alt_scroll(&mut self) {
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_alt_scroll();
        }
    }

    /// Payload-free observable agent/tool choreography.
    #[inline]
    pub fn note_matrix_rain_signal(&mut self, code: u32, weight: u32) {
        if code == crate::matrix_rain::RainSignal::TurnStart as u32 {
            self.rain_material_editing = false;
        }
        if let Some(rain) = self.rain.as_deref_mut() {
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
        self.pet_style_named = GlowStyle::style_names_any_pet(style);
        self.companion
            .set_species(if GlowStyle::style_names_dog_pet(style) {
                PetSpecies::Dog
            } else {
                PetSpecies::Cat
            });
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

    /// Set only the glow's trail colour.
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
    pub fn set_cursor_trail(
        &mut self,
        enabled: bool,
        duration_ms: u64,
        length: u32,
        color: Option<u32>,
        theme_cursor: u32,
    ) {
        self.trail_body_color = color.map(|color| color & 0x00FF_FFFF);
        self.trail_cfg = TrailConfig {
            enabled,
            duration: Duration::from_millis(duration_ms.clamp(30, 2000)),
            max_len: (length as usize).clamp(1, 512),
            color: color.unwrap_or(theme_cursor) & 0x00FF_FFFF,
            intensity: 0.0,
            warmth: 0.0,
        };
        if !enabled {
            self.cursor.trail.reset();
        }
    }

    /// A test's negative control: the cursor lights drawn without the letter
    /// guard (`CursorGlow::set_letter_guard`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard(on);
    }

    /// A test's negative control: the guard on, but no page veil under the
    /// letters the pet lies beneath (`CursorGlow::set_pet_veil`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_veil(&mut self, on: bool) {
        self.cursor.glow.set_pet_veil(on);
    }

    /// A test's negative control: the letter guard cuts each cell alone, with
    /// no ramp along the row (`CursorGlow::set_letter_guard_ramp`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_ramp(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard_ramp(on);
    }

    /// A test's negative control: a word gap climbs the letter guard's ramp
    /// from each of its letters (`CursorGlow::set_letter_guard_word_gaps`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_word_gaps(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard_word_gaps(on);
    }

    /// A test's negative control: a dim letter's bar at its bare share, the
    /// letter guard keeping no room over it (`CursorGlow::set_letter_guard_room`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_room(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard_room(on);
    }

    /// A test's negative control: the pet's veil laid over every light, not
    /// beneath them (`CursorGlow::set_pet_veil_beneath`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_veil_beneath(&mut self, on: bool) {
        self.cursor.glow.set_pet_veil_beneath(on);
    }

    /// A test's negative control: the letter guard's cut pieces rounded to
    /// the nearest byte (`CursorGlow::set_letter_guard_cuts_rounded`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_cuts_rounded(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard_cuts_rounded(on);
    }

    /// A test's negative control: the letter guard feathers its cuts into a
    /// rail laid clear of the ring (`CursorGlow::set_letter_guard_rails_clear`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_rails_clear(&mut self, on: bool) {
        self.cursor.glow.set_letter_guard_rails_clear(on);
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

    /// Opt the resident pet in (or out).
    #[inline]
    pub fn set_cursor_pet(&mut self, enabled: bool, seed: u64) {
        self.companion.set_enabled_seed(enabled, seed);
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
    pub fn set_sparkle_classes(&mut self, profanity: bool, feline: bool, emphasis: bool) {
        self.deco_cfg.profanity = profanity;
        self.deco_cfg.feline = feline;
        self.emphasis_enabled = emphasis;
        self.recompute_emphasis();
        self.refresh_sparkle_cfg();
    }

    /// Animated-ink knobs (native `[sparkle_words.ink]`).
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
    pub fn set_sparkle_languages(&mut self, languages: &[&str]) {
        self.languages = if languages.is_empty() {
            vec!["en".to_string()]
        } else {
            languages.iter().map(|s| (*s).to_string()).collect()
        };
        self.rebuild_sparkle(false);
    }

    /// User lexicon-override TOML merged over the builtin.
    pub fn set_sparkle_lexicon_override(&mut self, toml: Option<String>) {
        self.override_toml = toml;
        self.rebuild_sparkle(false);
    }

    /// Folded surfaces to never decorate (native global `deny` + `ignore_words`).
    pub fn set_sparkle_deny(&mut self, words: &[&str]) {
        self.deco_cfg.ignore = words.iter().map(|w| aterm_lexicon::fold(w)).collect();
        self.refresh_sparkle_cfg();
    }

    /// Lexicon build diagnostics.
    #[must_use]
    pub fn sparkle_lexicon_warnings(&self) -> Vec<String> {
        self.custom_spec_warning
            .iter()
            .cloned()
            .chain(
                self.lexicon_conflicts
                    .iter()
                    .filter(|w| lexicon_warning_applies(w, self.deco_cfg.cjk_single_char))
                    .cloned(),
            )
            .collect()
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
    fn rebuild_sparkle(&mut self, force: bool) {
        if self.sparkle.is_none() && !force {
            return;
        }
        let refs: Vec<&str> = self.languages.iter().map(String::as_str).collect();
        let mut over = self.override_toml.clone().unwrap_or_default();
        over.push_str(&self.custom_lexicon);
        let over = (!over.is_empty()).then_some(over);
        let lexicon = aterm_lexicon::Lexicon::with_languages_and_override(&refs, over.as_deref())
            .unwrap_or_else(|_| aterm_lexicon::Lexicon::with_languages(&refs));
        self.lexicon_conflicts = lexicon.conflicts().to_vec();
        self.sparkle = Some(Resolved {
            cfg: self.deco_cfg.clone(),
            lexicon: std::sync::Arc::new(lexicon),
        });
        self.decos.hard_reset();
    }

    /// Master rain switch.
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
        self.streak_cfg.intensity = if reduced { 0.0 } else { 1.0 };
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
        let clamp_alpha =
            |a: u32| a.clamp(u32::from(RAIN_ALPHA_FLOOR), u32::from(RAIN_ALPHA_CAP)) as u8;
        self.rain_cfg = RainConfig {
            enabled: self.rain_cfg.enabled,
            fps: fps.clamp(12, 60) as u8,
            density: density.clamp(1, 12) as u8,
            speed: speed.clamp(1, 10) as u8,
            trail: trail.clamp(1, 10) as u8,
            alpha_override: alpha.map(clamp_alpha),
            head_alpha_override: head_alpha.map(clamp_alpha),
            hue: if hue.eq_ignore_ascii_case("theme") {
                RainHue::Theme
            } else if hue.eq_ignore_ascii_case("custom") {
                hue_color.map_or(RainHue::Matrix, |c| RainHue::Custom(c & 0x00FF_FFFF))
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
        let changed = self
            .cursor_coordinate_space
            .is_some_and(|previous| previous != current);
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
                term_cmd_done(term).map(|(seq, _)| seq),
            );
            input.cursor_trail.clear();
            input.cursor_glow_add.clear();
            input.glow_halo.clear();
            input.fire_patch.clear();
            input.glow_under.clear();
            input.char_fg.clear();
            input.fire_halo.clear();
            #[cfg(test)]
            {
                self.block_fill = None;
            }
            input.cursor_fill_override = None;
            input.cursor_effect_style_override = None;
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
        let cur = (live_viewport && input.cursor_visible)
            .then_some((input.cursor_row as u16, input.cursor_col as u16));
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

        let probe_row = cur.filter(|&(row, _)| {
            let Some(render_row) = input.cells.get(usize::from(row)) else {
                return false;
            };
            let slice = &render_row[..render_row.len().min(input.cols)];
            self.row_probe_scratch.clear();
            self.row_probe_scratch.reserve(input.cols);
            self.row_probe_scratch.extend(
                slice
                    .iter()
                    .map(|cell| if cell.wide { '\0' } else { cell.ch }),
            );
            self.row_probe_scratch.resize(input.cols, ' ');
            true
        });

        let mut trail_cfg = self.trail_cfg;
        trail_cfg.enabled &= animate;
        let glow_geom = self.chrome_geom(rows, cols, cell_w, cell_h);
        let configured_intensity = self.glow_cfg.intensity;
        let configured_head_dx = self.glow_cfg.head_dx;
        self.glow_cfg.intensity *= motion_amp;
        self.glow_cfg.head_dx = crate::cursor_fx::head_dx_for(input.cursor_style);
        let default_bg = if input.default_bg == aterm_core::render::COLOR_UNSET {
            COLD_THEME_BG
        } else {
            input.default_bg & 0x00FF_FFFF
        };
        let pending_keys = std::mem::take(&mut self.pending_keys);
        let term_cursor_color = terminal_cursor_color(term);
        let step = native::step_cursor_family(
            &mut self.cursor,
            &native::CursorStep {
                input: CursorFxInput {
                    now,
                    cur,
                    live_viewport,
                    cursor_style: input.cursor_style,
                    blink_phase: true,
                    live_cursor: term_cursor_color,
                    cursor_color_pinned,
                    default_bg,
                    cursor_cell_bg: cur.and_then(|(row, col)| {
                        input
                            .cells
                            .get(usize::from(row))?
                            .get(usize::from(col))
                            .map(|cell| aterm_render::rgb_to_u32(cell.bg))
                    }),
                    geom: glow_geom,
                    focused,
                    shed_envelope: 1.0,
                    trail_brightness: 1.0,
                    body_allowed: !self.serious,
                    momentum_glow: self.glow_cfg.enabled,
                    user_tinted: self.glow_trail_color.chosen(),
                    accent_pinned: self.glow_explicit_accent,
                },
                glow: &self.glow_cfg,
                trail: trail_cfg,
                animate,
                frame_cells: Some(&input.cells),
                frame_cells_at: (0, 0),
                row_probe: probe_row.map(|(row, caret)| native::RowProbe {
                    row,
                    caret,
                    trust: crate::cursor_glow::ProbeTrust::Full,
                    cols: &self.row_probe_scratch,
                    flanks: None,
                }),
                pending_keys,
                print_anchor: None,
                print_anchor_glyph: None,
                caret_drawn: input.cursor_visible,
            },
            std::iter::empty(),
        );
        let fx = step.frame;
        self.glow_cfg.intensity = configured_intensity;
        self.glow_cfg.head_dx = configured_head_dx;
        let trail_fp = fx.trail_fp;
        let glow_fp = fx.glow_fp;
        self.publish_cursor(input);
        #[cfg(test)]
        if self.pet_veil_stale {
            let _ = self
                .cursor
                .glow
                .veil_pet_letters(&mut input.glow_under, &[]);
        }
        if self.trail_cfg.enabled {
            input.cursor_trail_color = fx.trail_color;
        }
        #[cfg(test)]
        {
            self.block_fill = fx.block_fill;
        }
        input.cursor_fill_override = fx.block_fill.map(|owned| owned.fill);
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
        facts.caret = live_viewport.then_some((input.cursor_row as u16, input.cursor_col as u16));
        facts.cursor_visible = cur.is_some();
        facts.live_viewport = live_viewport;
        facts.display_offset = if cursor_snapshot_coherent {
            input.display_offset
        } else {
            facts.display_offset
        };
        let host = HostFrameInput {
            now,
            visibility: match self.rain_visibility {
                RainVisibility::Focused => Visibility::Focused,
                RainVisibility::VisibleUnfocused => Visibility::VisibleUnfocused,
                RainVisibility::Hidden => Visibility::Hidden,
            },
            reduced_motion: !animate,
            serious: self.serious,
            shed_envelope: 1.0,
            shed_active: false,
            pointer_px: None,
            capture: CaptureMode::Present,
            geometry: FrameGeom {
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
        };
        if self.pet_style_named && self.glow_cfg.enabled && self.companion.enabled() {
            let pet_world = crate::pet_world::PetWorldFacts::read(term, term.render_identity());
            let _ = self.companion.observe_console(
                input,
                &pet_world,
                crate::pet_world::PetPane::full(input),
                &[],
            );
        }
        let pet = self.pet_prepare(PetFacts {
            facts: &facts,
            host: &host,
            glow: GlowOwnership {
                enabled: self.glow_cfg.enabled,
                style: self.glow_cfg.style,
                style_raw_names_pet: self.pet_style_named,
            },
            sing: SingFacts::default(),
            focused,
            obscured: false,
            pane: None,
            room: None,
        });
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
                    let rescan = self.decos.needs_rescan(epoch).then(|| WordRescan {
                        birth: now,
                        cursor: live_viewport
                            .then_some((input.cursor_row as u16, input.cursor_col as u16)),
                    });
                    let fp = self
                        .word_grid(&WordGrid {
                            now,
                            lexicon: &rs.lexicon,
                            cfg: &rs.cfg,
                            geom,
                            cells: &input.cells,
                            line_sizes: &input.line_sizes,
                            base_y: input.base_y,
                            default_bg,
                            epoch,
                            rescan,
                            companion_at: pet.companion,
                            sel: SelView {
                                sel: term.text_selection(),
                                display_offset: term.grid().display_offset() as i32,
                            },
                            focused,
                        })
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
        self.veil_pet_letters(input);
        let streak = self.streak.is_some().then(|| {
            let mut cfg = self.streak_cfg;
            cfg.intensity *= cursor_motion.amplitude(MotionEffect::OutputStreak);
            StreakGrid {
                now,
                cfg,
                seed: self.streak_seed,
                echo: StreakEcho::default(),
                cells: &input.cells,
                cursor: (input.cursor_row, input.cursor_col),
                viewport: (rows, cols),
                content_seq: term.content_seq(),
                geom: glow_geom,
            }
        });
        let streak_fp = self
            .streak_grid(streak.as_ref())
            .map_or(0, |(frame, _)| frame.fp);
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
                        scan: (display_offset == 0
                            && input.cells.len() >= rows
                            && input.line_sizes.len() >= rows)
                            .then_some(RainScan {
                                cells: &input.cells,
                                line_sizes: &input.line_sizes,
                                images: &input.images,
                                rows,
                                cols,
                                default_bg: input.default_bg,
                                epoch,
                            }),
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
                term_cmd_done(term).map(|(seq, _)| seq),
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

#[cfg(test)]
#[path = "pipeline_input_tests.rs"]
mod input_contract_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_render::TrailCell;

    fn rain_heard(p: &EffectsPipeline) -> (crate::matrix_rain::RainSignal, bool) {
        p.rain.as_deref().expect("rain is on").heard_for_test()
    }

    fn page_frame(p: &mut EffectsPipeline, term: &mut Terminal, input: &mut RenderInput) {
        p.advance(16.0);
        term.cell_frame_into(input, 6, 20);
        p.apply(term, input, 8, 16);
    }

    #[test]
    fn web_rain_hears_command_completions_and_execute_edges() {
        use crate::matrix_rain::RainSignal;
        for suppressed_alt in [false, true] {
            let mut p = EffectsPipeline::new();
            p.rain_cfg.suppress_in_alt_screen = true;
            enable_classic_rain(&mut p);
            let mut term = Terminal::new(6, 20);
            let mut input = RenderInput::default();
            page_frame(&mut p, &mut term, &mut input);
            let session = term.render_identity();
            assert_eq!(p.rain_latches.last_cmd, Some((session, 0)));
            assert_ne!(rain_heard(&p).0, RainSignal::Execute, "nothing ran yet");

            term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07");
            if suppressed_alt {
                term.process(b"\x1b[?1049h");
            }
            page_frame(&mut p, &mut term, &mut input);
            assert_eq!(p.rain_latches.shell_executing, Some((session, true)));
            let execute_heard = rain_heard(&p).0 == RainSignal::Execute;
            term.process(b"\x1b]133;D;1\x07");
            page_frame(&mut p, &mut term, &mut input);
            let seq = term.completed_command_seq();
            assert!(seq > 0, "fixture: the shell reported a completion");
            assert_eq!(
                p.rain_latches.last_cmd,
                Some((session, seq)),
                "the latch consumed the completion either way"
            );
            let ember = rain_heard(&p).1;
            if suppressed_alt {
                assert!(
                    !execute_heard && !ember,
                    "a suspended rain hears neither edge, and the completion stays baselined"
                );
                assert!(
                    term.is_alternate_screen(),
                    "fixture: the frames were suppressed"
                );
            } else {
                assert!(execute_heard, "the Execute edge reached the weather");
                assert!(ember, "the failure reached the weather: its ember holds");
            }
        }
    }

    #[test]
    fn a_torn_frame_delivers_the_execute_edge_on_the_next_fresh_snapshot() {
        use crate::matrix_rain::RainSignal;
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        let mut term = Terminal::new(6, 20);
        let mut input = RenderInput::default();
        page_frame(&mut p, &mut term, &mut input);
        p.advance(16.0);
        term.cell_frame_into(&mut input, 6, 20);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 1\r\n\x1b]133;C\x07");
        assert_ne!(
            input.snapshot_seq,
            term.damage_epoch(),
            "fixture: the snapshot is torn"
        );
        p.apply(&mut term, &mut input, 8, 16);
        assert_ne!(
            rain_heard(&p).0,
            RainSignal::Execute,
            "the torn frame drew nothing"
        );
        page_frame(&mut p, &mut term, &mut input);
        assert_eq!(
            rain_heard(&p).0,
            RainSignal::Execute,
            "the edge reached the weather with the next fresh snapshot"
        );
    }

    #[test]
    fn re_enabling_web_rain_replays_no_completion_from_while_it_was_off() {
        for glow_on in [false, true] {
            let mut p = EffectsPipeline::new();
            if glow_on {
                p.set_cursor_glow(
                    true,
                    "lumen",
                    crate::cursor_fx::TrailColor::Automatic,
                    None,
                    400,
                    24,
                    0.9,
                    0.8,
                    true,
                    0x0050_FA7B,
                );
            }
            enable_classic_rain(&mut p);
            let mut term = Terminal::new(6, 20);
            let mut input = RenderInput::default();
            page_frame(&mut p, &mut term, &mut input);
            p.set_matrix_rain_enabled(false);
            assert_eq!(p.enabled_any(), glow_on, "fixture: which off arm runs");
            term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07");
            page_frame(&mut p, &mut term, &mut input);
            term.process(b"\x1b]133;D;1\x07\x1b]133;A\x07$ \x1b]133;B\x07");
            page_frame(&mut p, &mut term, &mut input);
            assert!(term.completed_command_seq() > 0, "fixture: a completion");
            enable_classic_rain(&mut p);
            page_frame(&mut p, &mut term, &mut input);
            assert!(
                !rain_heard(&p).1,
                "glow_on={glow_on}: an off-era failure lights no ember"
            );
            term.process(b"false\r\n\x1b]133;C\x07\x1b]133;D;1\x07");
            page_frame(&mut p, &mut term, &mut input);
            assert!(
                rain_heard(&p).1,
                "glow_on={glow_on}: a failure with the rain on lights the ember"
            );
        }
    }

    #[test]
    fn set_cursor_glow_resolves_through_the_native_config_law() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "lumen",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            f32::NAN,
            f32::INFINITY,
            true,
            0,
        );
        assert_eq!(p.glow_cfg.intensity, 0.0);
        assert_eq!(p.glow_cfg.radius, 0.0);
    }

    use crate::cursor_trail::TypingCadence;
    use aterm_core::render::{FxBlend, FxLayer, FxSlot};
    use aterm_core::terminal::TerminalBuilder;
    use aterm_render::sprites::FreeSprite;

    /// The frame's published companion sprites, in draw order.
    fn free_sprites(input: &RenderInput) -> Vec<FreeSprite> {
        aterm_render::sprites::published(&input.fx_layers)
    }

    /// The frame's published rain rows.
    fn rain_rows(input: &RenderInput) -> Vec<FxLayer> {
        input
            .fx_layers
            .iter()
            .filter(|l| l.blend == FxBlend::Linear && l.slot == FxSlot::Under && !l.knock)
            .cloned()
            .collect()
    }

    fn stamp_effective_terminal_background(term: &Terminal, input: &mut RenderInput) {
        let color = if term.modes().reverse_video() {
            term.default_foreground()
        } else {
            term.default_background()
        };
        input.default_bg = aterm_render::rgb_to_u32([color.r, color.g, color.b]);
    }

    fn commit_ascii(
        pipeline: &mut EffectsPipeline,
        term: &mut Terminal,
        input: &mut RenderInput,
        ch: &[u8],
        dt_ms: f64,
    ) -> u64 {
        assert_eq!(ch.len(), 1, "fixture accepts one ASCII byte");
        assert!(
            pipeline.note_committed_char(term, input, char::from(ch[0])),
            "fixture must price the press at its exact cell width"
        );
        term.process(ch);
        pipeline.advance(dt_ms);
        let (rows, cols) = (input.rows, input.cols);
        term.cell_frame_into(input, rows, cols);
        pipeline.apply(term, input, 10, 19)
    }

    fn validate_cursor_scroll_action(action: &str, event: i64, decision: ContentScrollDelta) {
        let base = aterm_models_effects::cursor_scroll_signal_model();
        let model = aterm_spec::interp::with_consts(&base, &[]);
        let source = model.init_state();
        let mut projected = source.clone();
        projected.insert("event", event);
        match decision {
            ContentScrollDelta::Translate(delta) => {
                assert_eq!(delta, 2, "model fixture uses the exact two-row batch");
                projected.insert("uniform_rows", i64::from(delta));
                projected.insert("decision", 1);
                projected.insert("survivor_y", 1);
                projected.insert("proof_alive", 0);
            }
            ContentScrollDelta::Bands { count, .. } => {
                assert_eq!(count, 1, "model fixture uses the one-band batch");
                projected.insert("epoch", 1);
                projected.insert("band_batches", 1);
                projected.insert("decision", 3);
                projected.insert("survivor_y", 5);
                projected.insert("geometry_alive", 1);
                projected.insert("proof_alive", 0);
            }
            ContentScrollDelta::Invalidate => {
                projected.insert("epoch", 1);
                projected.insert("decision", 2);
                projected.insert("geometry_alive", 0);
                projected.insert("proof_alive", 0);
            }
            other => panic!("model event must produce a host action, got {other:?}"),
        }
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &base,
            &[],
            &source,
            &projected,
            Some(action),
            "EffectsPipeline cumulative cursor-scroll decision",
        );
        assert!(admitted, "real pipeline decision rejected by model: {why}");

        let mut stranded = projected;
        stranded.insert("decision", 0);
        let (admitted, _) = aterm_spec::verify::validate_transition_tiered(
            &base,
            &[],
            &source,
            &stranded,
            Some(action),
            "EffectsPipeline retained-count negative control",
        );
        assert!(
            !admitted,
            "a stranded no-op scroll decision must be rejected"
        );
    }

    fn seed_pipeline_trail(
        pipeline: &mut EffectsPipeline,
        term: &mut Terminal,
        row: u16,
    ) -> (RenderInput, Vec<TrailCell>) {
        pipeline.set_cursor_trail(true, 400, 24, None, 0x0050_FA7B);
        term.process(format!("\x1b[{};2H", row + 1).as_bytes());
        let mut input = term.cell_frame(5, 16);
        pipeline.apply(term, &mut input, 10, 19); // silent scroll baseline + cursor seed
        commit_ascii(pipeline, term, &mut input, b"x", 16.0);
        assert!(
            !input.cursor_trail.is_empty(),
            "fixture must own live geometry"
        );
        let trail = input.cursor_trail.clone();
        (input, trail)
    }

    fn validate_pipeline_layout_prepare(label: &str) {
        let model = aterm_models_misc::layout_coordinate_reset_model();
        let charged = model.successors("Charge", &model.init_state())[0].clone();
        let changed = model.successors("ChangeCoordinate", &charged)[0].clone();
        let mut prepared = changed.clone();
        prepared.insert("charged", 0);
        prepared.insert("bound_coordinate", changed["coordinate"]);
        prepared.insert("prepared", 1);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &changed,
            &prepared,
            Some("Prepare"),
            label,
        );
        assert!(ok, "real pipeline coordinate reset rejected: {why}");

        let mut stale = prepared;
        stale.insert("charged", 1);
        stale.insert("bound_coordinate", charged["bound_coordinate"]);
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &changed,
            &stale,
            Some("Prepare"),
            "EffectsPipeline retained-coordinate negative control",
        );
        assert!(!ok, "a changed pipeline coordinate may not retain geometry");
    }

    #[test]
    fn pipeline_history_uses_the_exact_frame_snapshot_and_retires_cursor_state() {
        let mut term = TerminalBuilder::new()
            .size(5, 16)
            .ring_buffer_size(32)
            .build();
        configure_glass_theme(&mut term);
        let mut pipeline = EffectsPipeline::new();
        rainbow_kitty_glow(&mut pipeline, "rainbow kitty");
        pipeline.set_cursor_pet(true, 0x5EED);
        let (mut input, _) = seed_pipeline_trail(&mut pipeline, &mut term, 2);
        assert!(
            pipeline.cursor.trail.is_active(),
            "fixture owns classic geometry"
        );
        assert!(
            pipeline.cursor.glow.is_active(),
            "fixture owns Glow geometry"
        );
        materialize_pet(&mut pipeline, &mut term, &mut input);
        let pet_brain_pending = pipeline.companion.brain().is_active();
        assert!(pet_brain_pending, "fixture owns a live resident");

        term.process(&b"history\r\n".repeat(12));
        term.scroll_display(1);
        term.cell_frame_into(&mut input, 5, 16);
        assert!(
            input.display_offset > 0,
            "fixture extracted retained history"
        );
        assert!(
            !input.cursor_visible,
            "the exact frame snapshot centrally suppresses the DEC cursor"
        );
        input.cursor_visible = true;
        pipeline.advance(33.0);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert!(
            !pipeline.cursor.trail.is_active()
                && !pipeline.cursor.glow.is_active()
                && input.cursor_trail.is_empty()
                && input.cursor_glow_add.is_empty()
                && input.glow_halo.is_empty()
                && input.fire_patch.is_empty()
                && input.glow_under.is_empty()
                && input.cursor_fill_override.is_none()
                && !input.cursor_visible,
            "history hard-clears every pipeline-owned cursor channel"
        );
        assert!(
            pipeline.cursor_pet_alpha() == 0
                && pipeline.companion_hit_rect().is_none()
                && free_sprites(&input).is_empty(),
            "history takes the pet off glass on the very first frame"
        );
        assert!(
            pipeline.companion.brain().is_active(),
            "…while its brain is still fading: the pet hides, it is not retired"
        );
        for _ in 0..40 {
            if !pipeline.companion.needs_frames() {
                break;
            }
            pipeline.advance(100.0);
            term.cell_frame_into(&mut input, 5, 16);
            pipeline.apply(&mut term, &mut input, 10, 19);
            assert!(
                free_sprites(&input).is_empty(),
                "history never draws the pet"
            );
        }
        assert!(
            !pipeline.companion.needs_frames() && !pipeline.companion.brain().is_active(),
            "hidden-caret ticks let the resident fade out and release the scheduler"
        );

        let model = aterm_models_effects::cursor_viewport_lifecycle_model();
        let charged = model.successors("ChargeLive", &model.init_state())[0].clone();
        let mut history = charged.clone();
        history.insert("live_viewport", 0);
        history.insert("glow_visible", i64::from(pipeline.cursor.glow.is_active()));
        history.insert(
            "trail_visible",
            i64::from(pipeline.cursor.trail.is_active()),
        );
        history.insert(
            "cursor_body_visible",
            i64::from(input.cursor_fill_override.is_some()),
        );
        history.insert("pet_visible", i64::from(pipeline.cursor_pet_alpha() > 0));
        history.insert("pet_brain_pending", i64::from(pet_brain_pending));
        history.insert("base_cursor_visible", i64::from(input.cursor_visible));
        history.insert(
            "pet_brain_ticked",
            i64::from(!pipeline.companion.brain().is_active()),
        );
        history.insert(
            "scheduler_stuck",
            i64::from(pipeline.companion.needs_frames()),
        );
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &charged,
            &history,
            Some("EnterHistory"),
            "EffectsPipeline exact history snapshot",
        );
        assert!(ok, "pipeline history projection rejected: {why}");
        let mut forged_fill = history.clone();
        forged_fill.insert("cursor_body_visible", 1);
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &charged,
            &forged_fill,
            Some("EnterHistory"),
            "EffectsPipeline forged-fill negative control",
        );
        assert!(!ok, "a forged cursor fill cannot survive history");
        let mut retained_pet = history.clone();
        retained_pet.insert("pet_visible", 1);
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &charged,
            &retained_pet,
            Some("EnterHistory"),
            "EffectsPipeline retained-pet negative control",
        );
        assert!(!ok, "a pet body cannot survive history");
        let mut frozen_brain = history;
        frozen_brain.insert("pet_brain_ticked", 0);
        frozen_brain.insert("scheduler_stuck", 1);
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &charged,
            &frozen_brain,
            Some("EnterHistory"),
            "EffectsPipeline frozen-brain negative control",
        );
        assert!(
            !ok,
            "a brain that was not ticked in history pins the scheduler"
        );
    }

    #[test]
    fn every_web_rainbow_kitty_spelling_is_v2() {
        let mut p = EffectsPipeline::new();
        for (style, kitty) in [
            ("rainbow kitty", true),
            ("rainbow kitty pet", true),
            ("nyan rainbow", true),
            ("rainbow kitty v1", false),
            ("kitty v1", false),
        ] {
            rainbow_kitty_glow(&mut p, style);
            let want = if kitty {
                GlowStyle::RainbowKitty
            } else {
                GlowStyle::Lumen
            };
            assert_eq!(p.glow_cfg.style, want, "{style:?}");
        }
        for (style, v2) in [("rainbow kitty", true), ("rainbow kitty v1", false)] {
            let mut p = EffectsPipeline::new();
            rainbow_kitty_glow(&mut p, style);
            let (mut term, mut input) = glass(8, 40);
            for _ in 0..6 {
                commit_ascii(&mut p, &mut term, &mut input, b"k", 33.0);
            }
            assert_eq!(p.cursor.glow.v2_status().is_some(), v2, "{style:?}: v2");
        }
        for (style, named, species) in [
            ("rainbow kitty", true, PetSpecies::Cat),
            ("kitty", true, PetSpecies::Cat),
            ("rainbow kitty pet", true, PetSpecies::Cat),
            ("Rainbow Kitty Underline", true, PetSpecies::Cat),
            ("rainbow dog pet", true, PetSpecies::Dog),
            ("pet dog", true, PetSpecies::Dog),
            ("rainbow kitty flying", false, PetSpecies::Cat),
            ("rainbow", false, PetSpecies::Cat),
            ("lumen", false, PetSpecies::Cat),
        ] {
            rainbow_kitty_glow(&mut p, style);
            assert_eq!(p.pet_style_named, named, "{style:?}: names the pet?");
            assert_eq!(p.companion.species(), species, "{style:?}: which animal");
        }
        for (style, tall) in [
            ("rainbow kitty", true),
            ("rainbow kitty pet", true),
            ("rainbow kitty flying", true),
            ("rainbow kitty tall", true),
            ("rainbow kitty underline", false),
        ] {
            rainbow_kitty_glow(&mut p, style);
            assert_eq!(p.glow_cfg.ribbon_tall, tall, "{style:?}: tall ribbon?");
        }
    }

    fn rainbow_kitty_glow(p: &mut EffectsPipeline, style: &str) {
        p.set_cursor_glow(
            true,
            style,
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            0.9,
            0.8,
            true,
            0x0050_FA7B,
        );
    }

    fn pet_pipeline(seed: u64) -> EffectsPipeline {
        let mut p = EffectsPipeline::new();
        rainbow_kitty_glow(&mut p, "rainbow kitty");
        p.set_cursor_pet(true, seed);
        p
    }

    fn glass(rows: u16, cols: u16) -> (Terminal, RenderInput) {
        let mut term = Terminal::new(rows, cols);
        configure_glass_theme(&mut term);
        let input = term.cell_frame(usize::from(rows), usize::from(cols));
        (term, input)
    }

    fn configure_glass_theme(term: &mut Terminal) {
        let theme = aterm_render::Theme::default();
        let rgb = |color: u32| {
            aterm_core::terminal::Rgb::new(
                ((color >> 16) & 0xff) as u8,
                ((color >> 8) & 0xff) as u8,
                (color & 0xff) as u8,
            )
        };
        term.set_default_foreground(rgb(theme.fg));
        term.set_default_background(rgb(theme.bg));
        term.set_default_selection_background(Some(rgb(theme.selection)));
    }

    fn type_line(
        p: &mut EffectsPipeline,
        term: &mut Terminal,
        input: &mut RenderInput,
        text: &[u8],
        dt_ms: f64,
    ) -> u64 {
        let mut fp = 0;
        for b in text {
            fp = commit_ascii(p, term, input, &[*b], dt_ms);
        }
        fp
    }

    fn idle(
        p: &mut EffectsPipeline,
        term: &mut Terminal,
        input: &mut RenderInput,
        frames: usize,
        dt_ms: f64,
    ) -> u64 {
        let (rows, cols) = (input.rows, input.cols);
        let mut fp = 0;
        for _ in 0..frames {
            p.advance(dt_ms);
            term.cell_frame_into(input, rows, cols);
            fp = p.apply(term, input, 10, 19);
        }
        fp
    }

    const CELL_H: u16 = 19;

    const fn pet_natural_size(cell_h: u16) -> (u16, u16) {
        let nat_h = (crate::kitty_pet::ART_ROWS * cell_h as f32).round();
        let nat_w = (nat_h * crate::kitty_pet::ART_ASPECT).round();
        (nat_w as u16, nat_h as u16)
    }

    fn pet_body_sprite(free: &[FreeSprite], cell_h: u16) -> Option<FreeSprite> {
        let nat = pet_natural_size(cell_h);
        free.iter().find(|s| (s.art.w(), s.art.h()) == nat).cloned()
    }

    fn run_command_block(
        p: &mut EffectsPipeline,
        term: &mut Terminal,
        input: &mut RenderInput,
        exit: i32,
    ) -> u64 {
        term.process(b"\r\n\x1b]133;C\x07out\r\n");
        term.process(format!("\x1b]133;D;{exit}\x07").as_bytes());
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        idle(p, term, input, 1, 33.0)
    }

    #[test]
    fn a_failed_command_hushes_the_caret_jump_fanfare_while_the_pet_grieves() {
        for exit in [0, 1] {
            let mut p = pet_pipeline(7);
            let (mut term, mut input) = glass(24, 80);
            term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
            idle(&mut p, &mut term, &mut input, 1, 33.0);
            materialize_pet(&mut p, &mut term, &mut input);
            type_line(&mut p, &mut term, &mut input, b"kkkkkkkk", 60.0);
            assert!(
                p.cursor.glow.typing_momentum(p.now()) > 0.0,
                "fixture: the run earned momentum before the command ran"
            );

            run_command_block(&mut p, &mut term, &mut input, exit);
            assert_eq!(
                p.companion.grieving(),
                exit != 0,
                "exit {exit}: the droop is owed exactly on a failure"
            );
            term.process(b"\r\n$ ");
            let (momentum, v2_disp) = {
                idle(&mut p, &mut term, &mut input, 1, 33.0);
                (
                    p.cursor.glow.typing_momentum(p.now()),
                    p.cursor.glow.momentum_display(),
                )
            };
            if exit == 0 {
                assert!(
                    momentum > 0.0 && v2_disp > 0.0,
                    "control: a success keeps the earned momentum ({momentum}) and v2 spine ({v2_disp}) through the jump"
                );
            } else {
                assert!(p.companion.grieving(), "the droop outlasts one frame");
                assert_eq!(
                    (momentum, v2_disp),
                    (0.0, 0.0),
                    "a grieving pet hushes the fanfare: neither v1 momentum nor v2 spine fuels the jump meteor"
                );
                for _ in 0..6 {
                    idle(&mut p, &mut term, &mut input, 1, 33.0);
                    if !p.companion.grieving() {
                        break;
                    }
                    assert_eq!(p.cursor.glow.typing_momentum(p.now()), 0.0);
                    assert_eq!(p.cursor.glow.momentum_display(), 0.0);
                }
            }
        }
    }

    #[test]
    fn the_pets_contrast_cursor_is_the_stamped_cursor_colour_or_the_terminals() {
        assert_eq!(pet_contrast_cursor(0x0011_2233, 0x00AA_BBCC), 0x0011_2233);
        assert_eq!(
            pet_contrast_cursor(0xFF11_2233, 0x00AA_BBCC),
            0x0011_2233,
            "a stamped colour's high byte is not ink"
        );
        assert_eq!(
            pet_contrast_cursor(aterm_core::render::COLOR_UNSET, 0x00AA_BBCC),
            0x00AA_BBCC,
            "the sentinel falls back to the resolved colour it was handed"
        );

        let (mut term, _input) = glass(24, 80);
        let fg = term.default_foreground();
        assert_eq!(
            terminal_cursor_color(&term),
            aterm_render::rgb_to_u32([fg.r, fg.g, fg.b]),
            "no OSC 12: the terminal's default foreground, as native resolves it"
        );
        term.process(b"\x1b]12;#00ff00\x07");
        assert_eq!(
            terminal_cursor_color(&term),
            0x0000_FF00,
            "a live OSC 12 is the cursor colour the pet contrasts against"
        );
    }

    #[test]
    fn only_the_owner_fence_rebaselines_the_command_probe_not_a_hide_or_a_resize() {
        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(24, 80);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        idle(&mut p, &mut term, &mut input, 1, 33.0);
        p.set_effects_visibility("hidden");
        p.set_effects_visibility("focused");
        run_command_block(&mut p, &mut term, &mut input, 1);
        assert!(
            p.companion.grieving(),
            "a presentability edge keeps the latch: the failure is felt"
        );

        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(24, 80);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        idle(&mut p, &mut term, &mut input, 1, 33.0);
        term.process(b"\r\n\x1b]133;C\x07out\r\n\x1b]133;D;1\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        p.advance(33.0);
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 12, 24);
        assert!(
            p.companion.grieving(),
            "a resize retires the pet's SURFACE, never its command stream: \
             the failure that just landed is still the pet's news"
        );

        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(24, 80);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        idle(&mut p, &mut term, &mut input, 1, 33.0);
        term.process(b"\r\n\x1b]133;C\x07out\r\n\x1b]133;D;1\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        term.process(b"\x1b[?1049h");
        p.advance(33.0);
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 19);
        assert!(
            !p.companion.grieving(),
            "the owner fence re-baselines the probe: the old screen's \
             completion is not the new owner's news"
        );
    }

    fn materialize_pet(p: &mut EffectsPipeline, term: &mut Terminal, input: &mut RenderInput) {
        for _ in 0..40 {
            commit_ascii(p, term, input, b"k", 33.0);
            if p.cursor_pet_alpha() > 0 {
                break;
            }
        }
        assert!(
            p.cursor_pet_alpha() > 0,
            "fixture: the resident reached the glass"
        );
        assert!(
            pet_body_sprite(&free_sprites(input), CELL_H).is_some(),
            "fixture: its body rides free_sprites"
        );
        assert!(p.companion_hit_rect().is_some(), "fixture: it is clickable");
    }

    #[test]
    fn cursor_pet_off_leaves_every_channel_and_fingerprint_untouched() {
        let mut plain = EffectsPipeline::new();
        rainbow_kitty_glow(&mut plain, "rainbow kitty");
        let mut off = EffectsPipeline::new();
        rainbow_kitty_glow(&mut off, "rainbow kitty");
        off.set_cursor_pet(false, 7);
        assert!(!off.cursor_pet_enabled());
        assert!(!off.note_pointer_px(f32::NAN, 1.0), "poison is dropped");
        assert!(
            off.note_pointer_px(30.0, 40.0),
            "the pointer is a host fact, pet or no pet"
        );
        let (mut ta, mut ia) = glass(8, 40);
        let (mut tb, mut ib) = glass(8, 40);
        for step in 0..40u32 {
            let fa = commit_ascii(&mut plain, &mut ta, &mut ia, b"k", 33.0);
            let fb = commit_ascii(&mut off, &mut tb, &mut ib, b"k", 33.0);
            assert_eq!(
                fa, fb,
                "step {step}: the fold never learns of a pet that is off"
            );
            assert!(free_sprites(&ia).is_empty() && free_sprites(&ib).is_empty());
            assert_eq!(off.cursor_pet_alpha(), 0);
            assert_eq!(off.companion_hit_rect(), None);
            assert_eq!(off.press_px(30.0, 40.0), PressOutcome::Pass);
            assert_eq!(plain.is_active(), off.is_active());
        }
        assert!(
            plain.is_active(),
            "non-vacuous: the trail is live while the pet is off"
        );

        let mut none = EffectsPipeline::new();
        none.set_cursor_pet(true, 7);
        let (mut term, mut input) = glass(8, 40);
        assert_eq!(none.apply(&mut term, &mut input, 10, 19), 0);
        assert!(free_sprites(&input).is_empty());
        assert!(!none.is_active() && !none.enabled_any());
        assert_eq!(none.cursor_pet_alpha(), 0);
        assert!(matches!(none.wake(), Wake::Idle));
        assert!(
            none.cursor_pet_enabled(),
            "the opt-in is remembered for when an owner arrives"
        );
    }

    #[test]
    fn cursor_pet_walks_beside_the_caret_then_sleeps_and_releases_cadence() {
        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(8, 40);
        let mut on_glass_at = None;
        for frame in 1..=12u32 {
            commit_ascii(&mut p, &mut term, &mut input, b"k", 33.0);
            if pet_body_sprite(&free_sprites(&input), CELL_H).is_some() {
                on_glass_at = Some(frame);
                break;
            }
        }
        let frame = on_glass_at.expect("the resident fades in beside the first caret");
        assert!(
            f64::from(frame) * 33.0 <= 400.0,
            "…within its 0.30 s fade-in"
        );
        assert!(p.cursor_pet_alpha() > 0 && p.companion_hit_rect().is_some());
        assert!(
            p.is_active() && p.companion.needs_frames(),
            "a fading-in cat claims the frame lane"
        );
        assert!(matches!(p.wake(), Wake::Frames));
        let x_before = pet_body_sprite(&free_sprites(&input), CELL_H)
            .expect("on glass")
            .x;

        type_line(&mut p, &mut term, &mut input, b"itty walks with me", 33.0);
        idle(&mut p, &mut term, &mut input, 45, 33.0);
        let x_after = pet_body_sprite(&free_sprites(&input), CELL_H)
            .expect("still on glass")
            .x;
        assert!(
            x_after > x_before,
            "the pet followed the caret to the right"
        );

        let fp = idle(&mut p, &mut term, &mut input, 350, 100.0);
        assert!(
            !p.companion.needs_frames(),
            "the resident released the lane"
        );
        assert!(!p.is_active(), "…and nothing else holds it");
        assert_eq!(p.next_deadline_ms(), None);
        assert!(matches!(p.wake(), Wake::Idle));
        let body =
            pet_body_sprite(&free_sprites(&input), CELL_H).expect("a sleeping cat is still drawn");
        assert_eq!(body.alpha, 255, "opaque, at rest");
        assert_eq!(p.cursor_pet_alpha(), 255);
        let sprites = free_sprites(&input).clone();
        assert_ne!(fp, 0, "a drawn resident folds a non-zero key");
        for _ in 0..3 {
            assert_eq!(
                p.apply(&mut term, &mut input, 10, 19),
                fp,
                "no advance ⇒ the same fold"
            );
            assert_eq!(free_sprites(&input), sprites, "byte-stable sprites");
        }
    }

    #[test]
    fn cursor_pet_retires_when_the_style_stops_naming_it() {
        for other in ["lumen", "rainbow kitty flying"] {
            let mut p = pet_pipeline(7);
            let (mut term, mut input) = glass(8, 40);
            materialize_pet(&mut p, &mut term, &mut input);
            let worn = p.companion.brain().worn_pair().expect("dressed");
            rainbow_kitty_glow(&mut p, other);
            idle(&mut p, &mut term, &mut input, 1, 33.0);
            assert!(
                pet_body_sprite(&free_sprites(&input), CELL_H).is_none(),
                "{other}: nothing drawn on the very next frame"
            );
            assert_eq!(p.cursor_pet_alpha(), 0);
            assert_eq!(p.companion_hit_rect(), None);
            assert!(
                !p.companion.brain().is_active(),
                "{other}: retired (a switch), not fading (an exit)"
            );
            assert!(!p.companion.needs_frames(), "{other}: no owed frames");
            assert!(
                p.cursor_pet_enabled(),
                "the opt-in survives; only the owner went"
            );
            rainbow_kitty_glow(&mut p, "rainbow kitty");
            materialize_pet(&mut p, &mut term, &mut input);
            assert_eq!(
                p.companion.brain().worn_pair(),
                Some(worn),
                "{other}: back wearing the seed's coat"
            );
        }

        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(8, 40);
        materialize_pet(&mut p, &mut term, &mut input);
        p.set_cursor_glow(
            false,
            "rainbow kitty",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            0.9,
            0.8,
            true,
            0x0050_FA7B,
        );
        idle(&mut p, &mut term, &mut input, 2, 33.0);
        assert!(free_sprites(&input).is_empty());
        assert!(!p.companion.brain().is_active() && !p.companion.needs_frames());
        assert!(
            !p.is_active() && !p.enabled_any(),
            "the trail master off is all-off again"
        );
    }

    #[test]
    fn cursor_pet_hides_in_history_and_when_unfocused_but_keeps_its_identity() {
        let mut p = pet_pipeline(7);
        let mut term = TerminalBuilder::new()
            .size(8, 40)
            .ring_buffer_size(64)
            .build();
        configure_glass_theme(&mut term);
        let mut input = term.cell_frame(8, 40);
        materialize_pet(&mut p, &mut term, &mut input);
        let worn = p.companion.brain().worn_pair().expect("dressed");

        term.process(&b"history\r\n".repeat(20));
        term.scroll_display(1);
        term.cell_frame_into(&mut input, 8, 40);
        assert!(input.display_offset > 0, "fixture: scrolled into history");
        let mut released = false;
        for _ in 0..40 {
            idle(&mut p, &mut term, &mut input, 1, 100.0);
            assert!(free_sprites(&input).is_empty());
            if !p.companion.needs_frames() {
                released = true;
                break;
            }
        }
        assert!(
            released && !p.companion.brain().is_active(),
            "the hidden brain faded to zero and released the lane"
        );
        assert_eq!(
            p.companion.brain().worn_pair(),
            Some(worn),
            "identity survives history"
        );
        term.scroll_to_bottom();
        materialize_pet(&mut p, &mut term, &mut input);
        assert_eq!(
            p.companion.brain().worn_pair(),
            Some(worn),
            "back in the same coat"
        );

        p.set_focused(false);
        idle(&mut p, &mut term, &mut input, 1, 33.0);
        assert_eq!(p.cursor_pet_alpha(), 0);
        assert!(free_sprites(&input).is_empty());
        assert!(
            !p.companion.brain().is_active() && !p.companion.needs_frames(),
            "an unfocused surface retires the body at once and owes no frames"
        );
        assert_eq!(p.companion.brain().worn_pair(), Some(worn));
        p.set_focused(true);
        materialize_pet(&mut p, &mut term, &mut input);
        assert_eq!(
            p.companion.brain().worn_pair(),
            Some(worn),
            "refocus: the same coat"
        );

        p.set_effects_visibility("hidden");
        assert!(!p.companion.brain().is_active() && !p.companion.needs_frames());
        assert_eq!(p.companion_hit_rect(), None);
        p.set_effects_visibility("focused");
        materialize_pet(&mut p, &mut term, &mut input);
        assert_eq!(
            p.companion.brain().worn_pair(),
            Some(worn),
            "a fresh sighting, same cat"
        );
    }

    #[test]
    fn cursor_pet_body_rect_is_handed_to_the_word_engine() {
        let drive = |with_pet: bool| -> (bool, bool) {
            let mut p = EffectsPipeline::new();
            rainbow_kitty_glow(&mut p, "rainbow kitty");
            p.set_sparkle_enabled(true);
            p.set_sparkle_feline("cat", true, true, false);
            if with_pet {
                p.set_cursor_pet(true, 7);
            }
            let (mut term, mut input) = glass(8, 40);
            term.process(b"\r\n\r\na nice ");
            type_line(&mut p, &mut term, &mut input, b"kitty", 33.0);
            let nat = pet_natural_size(CELL_H);
            let (mut saw_pet, mut saw_other_animal, mut stacked) = (false, false, false);
            for _ in 0..90 {
                idle(&mut p, &mut term, &mut input, 1, 100.0);
                let body = pet_body_sprite(&free_sprites(&input), CELL_H);
                saw_pet |= body.is_some();
                for s in &free_sprites(&input) {
                    // An animal: at least a cell tall (motes are smaller)
                    // and not the pet's own tile.
                    if s.art.h() < CELL_H || (s.art.w(), s.art.h()) == nat {
                        continue;
                    }
                    saw_other_animal = true;
                    if let Some(b) = &body {
                        let ox = (s.x + i32::from(s.w)).min(b.x + i32::from(b.w)) - s.x.max(b.x);
                        let oy = (s.y + i32::from(s.h)).min(b.y + i32::from(b.h)) - s.y.max(b.y);
                        stacked |=
                            ox > 0 && oy > 0 && ox * oy * 3 >= i32::from(s.w) * i32::from(s.h);
                    }
                }
            }
            assert_eq!(saw_pet, with_pet, "the pet is drawn exactly when opted in");
            (saw_other_animal, stacked)
        };
        let (other_animal, stacked) = drive(true);
        assert!(
            !other_animal,
            "the word the pet answers grows no ambient twin"
        );
        assert!(!stacked, "nothing stacks on the pet's body");
        let (control, _) = drive(false);
        assert!(control, "control: with no companion the word-cat peeks");
    }

    #[test]
    fn web_pipeline_reads_off_and_aliases_like_the_native_host() {
        let mut p = EffectsPipeline::new();
        rainbow_kitty_glow(&mut p, "rainbow kitty");
        assert!(p.glow_cfg.enabled);
        assert_eq!(p.glow_cfg.style, GlowStyle::RainbowKitty);
        assert!(p.pet_style_named, "the bare name names the pet");
        rainbow_kitty_glow(&mut p, "nyan rainbow");
        assert_eq!(p.glow_cfg.style, GlowStyle::RainbowKitty);
        assert!(!p.pet_style_named, "nyan rainbow keeps the flying head");
        rainbow_kitty_glow(&mut p, "classic mono");
        assert_eq!(p.glow_cfg.style, GlowStyle::Classic);
        assert!(
            p.glow_cfg.classic_mono,
            "the old spelling still draws the mono face"
        );
        rainbow_kitty_glow(&mut p, "off");
        assert!(!p.glow_cfg.enabled, "`off` disables the trail");
        assert!(!p.pet_style_named);
        rainbow_kitty_glow(&mut p, "lumen");
        assert!(p.glow_cfg.enabled && p.glow_cfg.style == GlowStyle::Lumen);
        rainbow_kitty_glow(&mut p, "plasma");
        assert!(p.glow_cfg.enabled && p.glow_cfg.style == GlowStyle::Lumen);
    }

    #[test]
    fn pointer_and_petting_latch_only_and_need_a_tick() {
        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(8, 40);
        materialize_pet(&mut p, &mut term, &mut input);
        idle(&mut p, &mut term, &mut input, 350, 100.0);
        assert!(
            !p.companion.needs_frames(),
            "fixture: asleep, lane released"
        );

        assert!(p.note_pointer_px(100.0, 50.0));
        assert!(
            !p.note_pointer_px(100.0, 50.0),
            "a still pointer costs no render"
        );
        assert!(!p.note_pointer_px(f32::NAN, 50.0), "poison is dropped");
        assert!(p.note_pointer_leave());
        assert!(!p.note_pointer_leave());

        let rect @ (x0, x1, y0, y1) = p.companion_hit_rect().expect("a sleeping cat is clickable");
        assert_eq!(
            p.press_px((x1 + 40) as f32, (y1 + 40) as f32),
            PressOutcome::Pass,
            "outside the body the host keeps routing"
        );
        assert_eq!(p.companion.brain().pending_pets(), 0);
        let sprites = free_sprites(&input).clone();
        let alpha = p.cursor_pet_alpha();
        assert_eq!(
            p.press_px((x0 + x1) as f32 / 2.0, (y0 + y1) as f32 / 2.0),
            PressOutcome::Pet,
            "inside the body the press is consumed"
        );
        assert_eq!(p.companion.brain().pending_pets(), 1, "one stroke queued");
        assert!(
            p.companion.needs_frames() && p.is_active(),
            "the latch asks for the frame that consumes it"
        );
        assert!(matches!(p.wake(), Wake::Frames));
        assert_eq!(p.cursor_pet_alpha(), alpha, "nothing moved without a tick");
        assert_eq!(p.companion_hit_rect(), Some(rect));
        assert_eq!(free_sprites(&input), sprites);
        let mut consumed_after = None;
        for frame in 1..=180u32 {
            idle(&mut p, &mut term, &mut input, 1, 33.0);
            if p.companion.brain().pending_pets() == 0 {
                consumed_after = Some(frame);
                break;
            }
        }
        assert!(
            consumed_after.is_some_and(|f| f >= 1),
            "the stroke is consumed by a tick, on the ground"
        );
    }

    #[test]
    fn a_typed_trick_latches_through_the_pipeline_and_needs_ticks() {
        use aterm_lexicon::Trick;

        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(8, 40);
        materialize_pet(&mut p, &mut term, &mut input);
        idle(&mut p, &mut term, &mut input, 60, 50.0);
        assert_eq!(p.companion.brain().pending_trick(), None);

        p.note_trick(Trick::Roll, false);
        assert_eq!(p.companion.brain().pending_trick(), Some(Trick::Roll));
        assert!(
            p.companion.needs_frames() && p.is_active(),
            "the latch asks for the frames that perform it"
        );
        p.revoke_trick();
        assert_eq!(p.companion.brain().pending_trick(), None);
        p.revoke_trick(); // idempotent: the listener revokes with nothing pending

        let sprites = free_sprites(&input).clone();
        p.note_trick(Trick::Roll, true);
        assert_eq!(p.companion.brain().pending_trick(), Some(Trick::Roll));
        assert!(matches!(p.wake(), Wake::Frames));
        assert_eq!(free_sprites(&input), sprites, "a note draws nothing");
        let mut rolled = false;
        for _ in 0..120 {
            idle(&mut p, &mut term, &mut input, 1, 33.0);
            rolled |= p.companion.brain().pose_name().starts_with("pet_roll");
        }
        assert_eq!(
            p.companion.brain().pending_trick(),
            None,
            "consumed by ticks"
        );
        assert!(rolled, "and performed: the authored roll reached the glass");

        for pet_talk in [false, true] {
            let mut p = pet_pipeline(7);
            let (mut term, mut input) = glass(24, 80);
            term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
            idle(&mut p, &mut term, &mut input, 1, 33.0);
            materialize_pet(&mut p, &mut term, &mut input);
            if pet_talk {
                p.note_trick_submit();
            }
            run_command_block(&mut p, &mut term, &mut input, 127);
            assert_eq!(
                p.companion.grieving(),
                !pet_talk,
                "pet talk {pet_talk}: only an ordinary failure is grieved"
            );
        }
    }

    #[test]
    fn pointer_under_host_rows_is_read_where_the_frame_draws_the_pet() {
        use crate::kitty_pet::PetAction;
        use aterm_core::render::HostRowPixels;
        use aterm_core::terminal::RenderCell;

        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(8, 40);
        materialize_pet(&mut p, &mut term, &mut input);
        idle(&mut p, &mut term, &mut input, 60, 50.0);
        let emitted = pet_body_sprite(&free_sprites(&input), CELL_H).expect("resident on glass");
        p.set_host_rows_px(CELL_H);
        idle(&mut p, &mut term, &mut input, 1, 50.0);
        assert_eq!(
            pet_body_sprite(&free_sprites(&input), CELL_H).map(|s| s.y),
            Some(emitted.y),
            "the emission does not move: the compose moves it"
        );
        let band_row = vec![RenderCell::default(); input.cols];
        let mut drawn = input.clone();
        drawn.prepend_host_rows(
            1,
            std::iter::once(band_row.as_slice()),
            usize::from(CELL_H),
            0,
            &mut Vec::new(),
            HostRowPixels::Translate,
        );
        let body = pet_body_sprite(&free_sprites(&drawn), CELL_H).expect("the body is drawn");
        assert_eq!(body.y, emitted.y + i32::from(CELL_H), "drawn one row lower");
        let (x0, w) = (body.x as f32, f32::from(body.w));
        let y = body.y as f32 + f32::from(body.h) / 2.0;
        let (rx0, rx1, ry0, ry1) = p.companion_hit_rect().expect("clickable");
        assert_eq!(
            (rx0, rx1, ry0, ry1),
            (
                body.x,
                body.x + i32::from(body.w),
                body.y,
                body.y + i32::from(body.h)
            ),
            "the hit rect is the drawn body, in frame px"
        );

        let before_stroke = p.companion.brain().content();
        let mut answered = false;
        for step in 1..=20 {
            let x = x0 + w * (0.20 + 0.60 * step as f32 / 20.0);
            assert!(p.note_pointer_px(x, y));
            idle(&mut p, &mut term, &mut input, 1, 50.0);
            answered |= p.companion.brain().action() == PetAction::Purr;
        }
        assert!(
            answered,
            "a stroke across the drawn body must reach the brain's stroke detector"
        );
        assert!(
            p.companion.brain().content() > before_stroke + 0.05,
            "the reaction was earned by pointer travel over the drawn body"
        );
        let cx = x0 + w / 2.0;
        assert_eq!(
            p.press_px(cx, body.y as f32 - f32::from(CELL_H) / 2.0),
            PressOutcome::Pass,
            "half a row above the drawn body the host keeps routing"
        );
        assert_eq!(
            p.press_px(cx, y),
            PressOutcome::Pet,
            "a press on the drawn body pets"
        );
    }

    #[test]
    fn reduced_motion_pins_the_web_pet_at_its_station() {
        let jump = |reduced: bool| {
            let mut p = pet_pipeline(7);
            if reduced {
                p.set_reduced_motion(true);
            }
            let (mut term, mut input) = glass(12, 60);
            materialize_pet(&mut p, &mut term, &mut input);
            idle(&mut p, &mut term, &mut input, 60, 33.0);
            let before = pet_body_sprite(&free_sprites(&input), CELL_H).expect("drawn");
            term.process(b"\x1b[10;40H");
            idle(&mut p, &mut term, &mut input, 1, 33.0);
            let first = pet_body_sprite(&free_sprites(&input), CELL_H).expect("drawn");
            idle(&mut p, &mut term, &mut input, 60, 33.0);
            let later = pet_body_sprite(&free_sprites(&input), CELL_H).expect("drawn");
            (before, first, later)
        };
        let (before, first, later) = jump(true);
        assert_ne!(
            (first.x, first.y),
            (before.x, before.y),
            "the station moved"
        );
        assert_eq!(
            (first.x, first.y),
            (later.x, later.y),
            "…in ONE frame: pinned, no walk"
        );
        assert_eq!(
            first.y + i32::from(first.h),
            10 * i32::from(CELL_H),
            "feet on the new baseline: no arc"
        );
        let (_, first, later) = jump(false);
        assert_ne!(
            (first.x, first.y),
            (later.x, later.y),
            "control: full motion is still travelling one frame after the jump"
        );

        let mut p = EffectsPipeline::new();
        p.set_sparkle_reduced_motion(true);
        assert!(p.deco_cfg.reduced_motion);
        p.set_matrix_rain_reduced_motion(false);
        assert!(!p.deco_cfg.reduced_motion);
    }

    #[test]
    fn next_deadline_ms_is_none_while_the_pet_moves_and_exact_while_it_breathes() {
        let mut p = pet_pipeline(7);
        enable_classic_rain(&mut p);
        assert_eq!(
            p.next_deadline_ms(),
            Some(83.0),
            "rain alone: an exact wake"
        );
        assert!(!p.companion.needs_frames());
        let (mut term, mut input) = glass(8, 40);
        materialize_pet(&mut p, &mut term, &mut input);
        assert!(
            p.companion.needs_frames(),
            "fixture: the resident is still moving"
        );
        assert_eq!(
            p.next_deadline_ms(),
            None,
            "a moving pet needs display-rAF cadence"
        );
        assert!(p.is_active());
        assert!(matches!(p.wake(), Wake::Frames));

        let mut settled = false;
        for _ in 0..200 {
            idle(&mut p, &mut term, &mut input, 1, 50.0);
            if !p.companion.needs_frames()
                && !p.cursor.trail.is_active()
                && !p.cursor.glow.is_active()
                && !p.cursor.bodies_active()
                && !p.decos.is_active(p.now())
            {
                settled = true;
                break;
            }
        }
        assert!(
            settled,
            "fixture: the resident and every other frame lane settle within 10 s"
        );
        assert!(
            p.companion.next_change_deadline(p.now()).is_some(),
            "settled: the resident names its next visible step"
        );
        let ms = p
            .next_deadline_ms()
            .expect("settled: an exact wake, not the frame train");
        assert!(
            ms > 0.0 && ms <= 83.0,
            "the earliest of the pet's offer and rain's wake: {ms} ms"
        );
        assert!(p.is_active(), "a breathing pet keeps the scheduler armed");
        assert!(matches!(p.wake(), Wake::At(_)));

        idle(&mut p, &mut term, &mut input, 350, 100.0);
        assert!(
            !p.companion.needs_frames(),
            "asleep: the pet forces nothing"
        );
        assert!(
            p.companion.next_change_deadline(p.now()).is_none(),
            "deep sleep: nothing left to offer"
        );
        assert!(
            p.next_deadline_ms().is_some() || !p.is_active(),
            "what remains is rain's own verdict — an exact wake or idle"
        );
        assert!(!matches!(p.wake(), Wake::Frames));
    }

    #[test]
    fn pipeline_rows_metrics_and_chrome_changes_reset_seeded_geometry() {
        #[derive(Clone, Copy, PartialEq)]
        enum Prep {
            Subject,
            FreshFamily,
            KeepBodies,
        }
        type Frames = Vec<(Vec<GlowQuad>, Vec<TrailCell>)>;
        let run = |style: &str, case: &str, prep: Prep| -> (EffectsPipeline, Frames) {
            let label = format!("{style}/{case}");
            let mut term = Terminal::new(5, 16);
            let mut pipeline = EffectsPipeline::new();
            pipeline.set_cursor_glow(
                true,
                style,
                crate::cursor_fx::TrailColor::Automatic,
                None,
                400,
                24,
                0.9,
                0.8,
                true,
                0x0050_FA7B,
            );
            let (mut input, _) = seed_pipeline_trail(&mut pipeline, &mut term, 2);
            assert!(
                pipeline.cursor.trail.is_active() && pipeline.cursor.glow.is_active(),
                "{label}: fixture owns retained geometry"
            );
            let now = pipeline.now();
            match prep {
                Prep::Subject => {}
                Prep::FreshFamily => {
                    let old = std::mem::take(&mut pipeline.cursor);
                    pipeline.cursor = crate::cursor_fx::CursorFx {
                        glow: old.glow,
                        momentum: old.momentum,
                        cadence: old.cadence,
                        ..Default::default()
                    };
                    pipeline.cursor.glow.curtain(now);
                }
                Prep::KeepBodies => {
                    pipeline.cursor.glow.curtain(now);
                    pipeline.cursor.trail.reset();
                }
            }
            let (cell_w, cell_h) = match case {
                "rows" => {
                    term.resize(6, 16);
                    input = term.cell_frame(6, 16);
                    (10, 19)
                }
                "cell-metrics" => (11, 20),
                "chrome-origin" => {
                    pipeline.set_chrome(3, 7);
                    (10, 19)
                }
                _ => unreachable!(),
            };
            if prep == Prep::KeepBodies {
                pipeline.cursor_coordinate_space = Some(CursorCoordinateSpace {
                    rows: input.rows,
                    cols: input.cols,
                    cell_w,
                    cell_h,
                    chrome_pad: pipeline.chrome_pad,
                    chrome_head: pipeline.chrome_head,
                });
            }
            let mut frames = Frames::new();
            for frame in 0..6 {
                if frame > 0 {
                    pipeline.advance(16.0);
                    let (rows, cols) = (input.rows, input.cols);
                    term.cell_frame_into(&mut input, rows, cols);
                }
                pipeline.apply(&mut term, &mut input, cell_w, cell_h);
                frames.push((input.cursor_glow_add.clone(), input.cursor_trail.clone()));
            }
            (pipeline, frames)
        };
        let styles = [
            "lumen",
            "phaser",
            "rainbow kitty",
            "sparkle",
            "fire",
            "laser",
            "beam",
            "water",
            "comet",
            "classic",
        ];
        for case in ["rows", "cell-metrics", "chrome-origin"] {
            for style in styles {
                let (subject, drawn) = run(style, case, Prep::Subject);
                let (_, fresh) = run(style, case, Prep::FreshFamily);
                assert!(
                    !subject.cursor.trail.is_active()
                        && (style == "rainbow kitty" || !subject.cursor.glow.is_active()),
                    "{style}/{case}: changed coordinate space retained a seeded survivor"
                );
                assert!(
                    !fresh[0].0.is_empty(),
                    "{style}/{case}: fixture — the fresh family draws the caret's body"
                );
                assert!(
                    drawn == fresh,
                    "{style}/{case}: the frames after the space change are not a fresh family's"
                );
                if matches!(style, "water" | "comet") {
                    let (_, kept) = run(style, case, Prep::KeepBodies);
                    assert!(
                        kept != fresh,
                        "{style}/{case}: control — a body that kept its geometry must draw \
                         differently"
                    );
                }
            }
            validate_pipeline_layout_prepare(case);
        }
    }

    #[test]
    fn pipeline_translates_real_zero_and_saturated_scrollback_exactly() {
        for cap in [0usize, 1] {
            let mut term = TerminalBuilder::new()
                .size(5, 16)
                .ring_buffer_size(cap)
                .build();
            if cap == 1 {
                term.process(b"\x1b[5;1H\n");
                assert_eq!(term.grid().scrollback_lines(), 1);
            }
            let mut pipeline = EffectsPipeline::new();
            let (mut input, before_trail) = seed_pipeline_trail(&mut pipeline, &mut term, 3);
            let before_signal = term.content_scroll_state();

            term.process(b"\x1b[5;1H\n\n\x1b[2;3H");
            let after_signal = term.content_scroll_state();
            assert_eq!(
                content_scroll_decision(Some(before_signal), after_signal),
                ContentScrollDelta::Translate(2)
            );
            assert_eq!(term.grid().scrollback_lines(), cap);

            term.cell_frame_into(&mut input, 5, 16);
            pipeline.apply(&mut term, &mut input, 10, 19);
            assert!(
                !before_trail.is_empty(),
                "cap={cap}: fixture must be charged"
            );
            assert!(
                pipeline.cursor.trail.is_active(),
                "cap={cap}: an unowned parser generation must not destroy earned light"
            );
            let translated: Vec<usize> = input.cursor_trail.iter().map(|c| c.row).collect();
            let expected: Vec<usize> = before_trail
                .iter()
                .map(|c| c.row.saturating_sub(2))
                .collect();
            assert_eq!(
                translated, expected,
                "cap={cap}: the comet must translate UP by the scrolled rows, cell for cell"
            );

            if cap == 0 {
                validate_cursor_scroll_action("UniformAtCap", 1, ContentScrollDelta::Translate(2));
            }
        }
    }

    #[test]
    fn pipeline_real_alt_uniform_translates_while_ambiguous_motion_invalidates() {
        fn margined_region_scroll(term: &mut Terminal) {
            term.process(b"\x1b[?69h\x1b[2;6s\x1b[2;4r\x1b[3;1H\x1b[S\x1b[r\x1b[?69l\x1b[3;3H");
        }
        fn alt_scroll(term: &mut Terminal) {
            term.process(b"\x1b[5;1H\n\n\x1b[3;3H");
        }
        fn direct_reset(term: &mut Terminal) {
            term.reset();
        }
        fn checkpoint_restore(term: &mut Terminal) {
            let checkpoint = Terminal::new(5, 16).checkpoint();
            term.restore_checkpoint(&checkpoint);
        }

        type ScrollMutationCase = (
            &'static str,
            i64,
            bool,
            ContentScrollDelta,
            fn(&mut Terminal),
        );
        let cases: [ScrollMutationCase; 4] = [
            (
                "RegionInvalidation",
                2,
                false,
                ContentScrollDelta::Invalidate,
                margined_region_scroll,
            ),
            (
                "AltUniform",
                3,
                true,
                ContentScrollDelta::Translate(2),
                alt_scroll,
            ),
            (
                "ResetInvalidation",
                4,
                false,
                ContentScrollDelta::Invalidate,
                direct_reset,
            ),
            (
                "RestoreInvalidation",
                5,
                false,
                ContentScrollDelta::Invalidate,
                checkpoint_restore,
            ),
        ];

        for (action, event, enter_alt, expected, mutate) in cases {
            let mut term = Terminal::new(5, 16);
            if enter_alt {
                term.process(b"\x1b[?1049h");
            }
            let mut pipeline = EffectsPipeline::new();
            if enter_alt {
                pipeline.set_cursor_glow(
                    true,
                    "lumen",
                    crate::cursor_fx::TrailColor::Automatic,
                    None,
                    400,
                    24,
                    0.7,
                    0.6,
                    true,
                    0x0050_FA7B,
                );
            }
            let (mut input, before_trail) = seed_pipeline_trail(&mut pipeline, &mut term, 2);
            let before = term.content_scroll_state();
            mutate(&mut term);
            let after = term.content_scroll_state();
            let decision = content_scroll_decision(Some(before), after);
            assert_eq!(decision, expected, "{action}");
            validate_cursor_scroll_action(action, event, decision);

            term.cell_frame_into(&mut input, 5, 16);
            pipeline.apply(&mut term, &mut input, 10, 19);
            if let ContentScrollDelta::Translate(rows) = expected {
                assert!(
                    pipeline.cursor.trail.is_active(),
                    "{action} destroyed licensed resident light"
                );
                let expected_cells: Vec<TrailCell> = before_trail
                    .iter()
                    .filter_map(|cell| {
                        cell.row
                            .checked_sub(usize::from(rows))
                            .map(|row| TrailCell { row, ..*cell })
                    })
                    .collect();
                assert_eq!(
                    input.cursor_trail, expected_cells,
                    "{action} must translate the licensed comet exactly with alt content"
                );
            } else {
                assert!(
                    input.cursor_trail.is_empty(),
                    "{action} retained stale cells"
                );
                assert!(
                    !pipeline.cursor.trail.is_active(),
                    "{action} retained trail state"
                );
            }

            if enter_alt {
                let scripted = pipeline.now();
                pipeline.cursor.glow.note_synthetic_typed(scripted, 1);
                pipeline.cursor.trail.note_synthetic_typed(scripted);
                term.grid_mut().cursor_forward(6);
                pipeline.advance(16.0);
                term.cell_frame_into(&mut input, 5, 16);
                pipeline.apply(&mut term, &mut input, 10, 19);
                assert!(
                    !input.cursor_trail.is_empty(),
                    "alt context must survive the translated scroll for classic classification"
                );
                assert!(
                    !input.cursor_glow_add.is_empty() || !input.glow_halo.is_empty(),
                    "alt context must survive the translated scroll for glow classification"
                );
            }
        }
    }

    #[test]
    fn a_band_batch_translates_the_pipeline_engines_instead_of_resetting_them() {
        let mut term = Terminal::new(5, 16);
        let mut pipeline = EffectsPipeline::new();
        let (mut input, before_trail) = seed_pipeline_trail(&mut pipeline, &mut term, 3);
        assert!(!before_trail.is_empty(), "fixture must own live geometry");
        let before = term.content_scroll_state();

        term.process(b"\x1b[2;4r\x1b[4;1H\n\x1b[r\x1b[3;3H");
        let after = term.content_scroll_state();
        let decision = content_scroll_decision(Some(before), after);
        assert_eq!(
            decision,
            ContentScrollDelta::Bands {
                first_seq: before.band_seq,
                count: 1,
            },
            "one region scroll is one explained band move"
        );
        assert_eq!(
            after.invalidation_epoch,
            before.invalidation_epoch + 1,
            "the epoch still bumps: a consumer that cannot translate bands \
             still reads discard"
        );
        assert_eq!(after.band(before.band_seq).delta, -1);
        validate_cursor_scroll_action("RegionBandMove", 6, decision);

        term.cell_frame_into(&mut input, 5, 16);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert!(
            pipeline.cursor.trail.is_active(),
            "an explained band must not destroy earned light"
        );
        let expected: Vec<TrailCell> = before_trail
            .iter()
            .map(|cell| TrailCell {
                row: cell.row - 1,
                ..*cell
            })
            .collect();
        assert_eq!(
            input.cursor_trail, expected,
            "every cell inside the band moves up exactly one row, cell for cell"
        );
    }

    #[test]
    fn pipeline_scroll_baseline_regressions_and_wide_deltas_fail_closed() {
        let mut pipeline = EffectsPipeline::new();
        let baseline = ContentScrollState {
            uniform_up_rows: 10,
            invalidation_epoch: 4,
            ..Default::default()
        };
        let now = pipeline.now();
        assert_eq!(
            pipeline.sync_scroll(baseline, now),
            ScrollChange::default(),
            "a first observation is silent"
        );
        assert_eq!(
            pipeline.scroll_baseline(),
            Some(baseline),
            "…and it is the baseline"
        );
        assert_eq!(pipeline.sync_scroll(baseline, now), ScrollChange::default());

        for invalid in [
            ContentScrollState {
                uniform_up_rows: 9,
                invalidation_epoch: 4,
                ..Default::default()
            },
            ContentScrollState {
                uniform_up_rows: 10,
                invalidation_epoch: 3,
                ..Default::default()
            },
            ContentScrollState {
                uniform_up_rows: 10 + u64::from(u16::MAX) + 1,
                invalidation_epoch: 4,
                ..Default::default()
            },
        ] {
            assert_eq!(
                content_scroll_decision(Some(baseline), invalid),
                ContentScrollDelta::Invalidate
            );
        }
    }

    #[test]
    fn glow_polarity_tracks_live_osc11_and_decscnm_background() {
        let mut pipeline = EffectsPipeline::new();
        pipeline.set_cursor_glow(
            true,
            "fire",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            1.0,
            0.9,
            true,
            0x0050_FA7B,
        );
        let mut term = Terminal::new(2, 8);

        term.process(b"\x1b]11;rgb:ff/ff/ff\x07");
        let mut input = term.cell_frame(2, 8);
        stamp_effective_terminal_background(&term, &mut input);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert!(
            !pipeline.glow_cfg.dark_theme,
            "a live light OSC 11 ground selects the light-theme treatment"
        );

        term.process(b"\x1b]10;rgb:ee/ee/ee\x07\x1b]11;rgb:01/02/03\x07\x1b[?5h");
        term.cell_frame_into(&mut input, 2, 8);
        stamp_effective_terminal_background(&term, &mut input);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert!(
            !pipeline.glow_cfg.dark_theme,
            "DECSCNM makes the live foreground the effective light ground"
        );

        term.process(b"\x1b[?5l");
        term.cell_frame_into(&mut input, 2, 8);
        stamp_effective_terminal_background(&term, &mut input);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert!(
            pipeline.glow_cfg.dark_theme,
            "leaving DECSCNM restores the dark OSC 11 ground"
        );

        input.default_bg = aterm_core::render::COLOR_UNSET;
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert!(
            pipeline.glow_cfg.dark_theme,
            "an unstamped legacy host retains the dark-ground fallback"
        );
    }

    fn stamp_cursor_color(term: &Terminal, input: &mut RenderInput, fallback: u32) {
        input.cursor_color = term.cursor_color().map_or(fallback, |color| {
            aterm_render::rgb_to_u32([color.r, color.g, color.b])
        });
    }

    #[test]
    fn automatic_wake_colors_follow_osc12_resets_and_theme_baselines() {
        let theme_cursor = 0x0011_2233;
        let mut pipeline = EffectsPipeline::new();
        pipeline.set_cursor_glow(
            true,
            "lumen",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            1.0,
            0.9,
            true,
            theme_cursor,
        );
        pipeline.set_cursor_trail(true, 400, 24, None, theme_cursor);
        let mut term = Terminal::new(2, 8);
        term.set_default_cursor_color(Some(aterm_core::terminal::Rgb::new(0x11, 0x22, 0x33)));

        term.process(b"\x1b]12;rgb:aa/00/11\x07");
        let mut input = term.cell_frame(2, 8);
        stamp_cursor_color(&term, &mut input, theme_cursor);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(pipeline.glow_cfg.color, 0x00AA_0011);
        assert_eq!(pipeline.glow_cfg.accent, brighten(0x00AA_0011, 1.5));
        assert_eq!(input.cursor_trail_color, 0x00AA_0011);

        term.process(b"\x1b]112\x07");
        term.cell_frame_into(&mut input, 2, 8);
        stamp_cursor_color(&term, &mut input, theme_cursor);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(pipeline.glow_cfg.color, theme_cursor);
        assert_eq!(input.cursor_trail_color, theme_cursor);

        let new_theme_cursor = 0x0044_5566;
        term.set_default_cursor_color(Some(aterm_core::terminal::Rgb::new(0x44, 0x55, 0x66)));
        term.cell_frame_into(&mut input, 2, 8);
        stamp_cursor_color(&term, &mut input, new_theme_cursor);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(
            pipeline.glow_cfg.color, new_theme_cursor,
            "an automatic wake follows a live theme change without reconfiguration"
        );
        assert_eq!(input.cursor_trail_color, new_theme_cursor);
    }

    #[test]
    fn explicit_wake_colors_and_laser_ignore_live_cursor_recolors() {
        let mut pipeline = EffectsPipeline::new();
        pipeline.set_cursor_glow(
            true,
            "lumen",
            crate::cursor_fx::TrailColor::Pinned(0x0001_0203),
            None,
            400,
            24,
            1.0,
            0.9,
            true,
            0x0011_2233,
        );
        pipeline.set_cursor_trail(true, 400, 24, Some(0x0004_0506), 0x0011_2233);
        let mut term = Terminal::new(2, 8);
        term.process(b"\x1b]12;rgb:aa/bb/cc\x07");
        let mut input = term.cell_frame(2, 8);
        stamp_cursor_color(&term, &mut input, 0x0011_2233);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(pipeline.glow_cfg.color, 0x0001_0203);
        assert_eq!(pipeline.glow_cfg.accent, brighten(0x0001_0203, 1.5));
        assert_eq!(input.cursor_trail_color, 0x0004_0506);

        pipeline.set_cursor_glow(
            true,
            "laser",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            1.0,
            0.9,
            true,
            0x0011_2233,
        );
        pipeline.set_cursor_trail(true, 400, 24, None, 0x0011_2233);
        term.cell_frame_into(&mut input, 2, 8);
        stamp_cursor_color(&term, &mut input, 0x0011_2233);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(
            pipeline.glow_cfg.color,
            crate::cursor_glow::LASER_DEFAULT_COLOR,
            "Laser keeps its canonical storm-violet default hue"
        );
        assert_eq!(
            input.cursor_trail_color,
            crate::cursor_glow::LASER_DEFAULT_COLOR,
            "an automatic opaque trail wears the glow's violet, as the native comet body does"
        );
    }

    #[test]
    fn the_page_colours_its_cursor_as_a_native_window_does() {
        use crate::cursor_fx::TrailColor;
        let (theme_cursor, osc12, chosen) = (0x0011_2233, 0x00AA_BBCC, 0x00E0_218A);
        let mut checked = 0;
        for row in crate::cursor_glow::spellings::TRAIL_STYLES {
            if row.style.is_none() {
                continue;
            }
            for setting in [
                TrailColor::Automatic,
                TrailColor::Cursor,
                TrailColor::Pinned(chosen),
            ] {
                let mut pipeline = EffectsPipeline::new();
                pipeline.set_cursor_glow(
                    true,
                    row.name,
                    setting,
                    None,
                    400,
                    24,
                    1.0,
                    0.6,
                    true,
                    theme_cursor,
                );
                pipeline.set_cursor_trail(true, 400, 24, None, theme_cursor);
                let mut term = Terminal::new(2, 8);
                let mut input = term.cell_frame(2, 8);
                for (osc, live, pinned) in [
                    (&b""[..], theme_cursor, false),
                    (&b"\x1b]12;rgb:aa/bb/cc\x07"[..], osc12, true),
                    (&b"\x1b]112\x07"[..], theme_cursor, false),
                ] {
                    term.process(osc);
                    term.cell_frame_into(&mut input, 2, 8);
                    stamp_cursor_color(&term, &mut input, theme_cursor);
                    pipeline.apply(&mut term, &mut input, 10, 19);
                    let mut glow = crate::cursor_fx::resolve_glow_config(
                        crate::cursor_fx::GlowKnobs {
                            enabled: true,
                            color: setting.knob(theme_cursor),
                            accent: None,
                            duration_ms: 400,
                            length: 24,
                            intensity: 1.0,
                            radius: 0.6,
                            ring: true,
                        },
                        crate::cursor_fx::GlowSpelling::parse(row.name),
                        theme_cursor,
                        true,
                        pipeline.glow_cfg.theme_fg,
                        pipeline.glow_cfg.theme_bg,
                        0.5,
                    );
                    let mut trail = pipeline.trail_cfg;
                    crate::cursor_fx::follow_live_cursor(
                        &mut glow, &mut trail, live, pinned, setting, false, None,
                    );
                    let case = format!("{} {setting:?} live {live:06x}", row.name);
                    assert_eq!(pipeline.glow_cfg.color, glow.color, "{case}");
                    assert_eq!(pipeline.glow_cfg.accent, glow.accent, "{case}");
                    assert_eq!(pipeline.trail_cfg.color, trail.color, "{case}");
                    assert_eq!(input.cursor_trail_color, glow.color, "{case}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 100, "{checked}");

        let mut pipeline = EffectsPipeline::new();
        pipeline.set_cursor_glow(
            true,
            "laser",
            TrailColor::Automatic,
            None,
            400,
            24,
            1.0,
            0.6,
            true,
            theme_cursor,
        );
        pipeline.set_cursor_trail(true, 400, 24, Some(0x0004_0506), theme_cursor);
        let mut term = Terminal::new(2, 8);
        let mut input = term.cell_frame(2, 8);
        stamp_cursor_color(&term, &mut input, theme_cursor);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(input.cursor_trail_color, 0x0004_0506);
        assert_eq!(
            pipeline.glow_cfg.color,
            crate::cursor_glow::LASER_DEFAULT_COLOR
        );

        assert!(pipeline.set_cursor_glow_color("cursor"));
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(pipeline.glow_cfg.color, theme_cursor);
        assert!(!pipeline.set_cursor_glow_color("violetish"));
        assert_eq!(pipeline.glow_trail_color, TrailColor::Cursor);
    }

    #[test]
    fn an_automatic_comet_keeps_its_hue_until_osc12_pins_the_cursor() {
        let theme_cursor = 0x0011_2233;
        let mut pipeline = EffectsPipeline::new();
        pipeline.set_cursor_glow(
            true,
            "comet",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            1.0,
            0.9,
            true,
            theme_cursor,
        );
        let mut term = Terminal::new(2, 8);
        term.set_default_cursor_color(Some(aterm_core::terminal::Rgb::new(0x11, 0x22, 0x33)));
        let comet = crate::cursor_glow::COMET_DEFAULT_COLOR;

        let mut input = term.cell_frame(2, 8);
        stamp_cursor_color(&term, &mut input, theme_cursor);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(
            pipeline.glow_cfg.color, comet,
            "the theme's seed is not a request"
        );
        assert_eq!(pipeline.glow_cfg.accent, brighten(comet, 1.5));

        term.process(b"\x1b]12;rgb:aa/00/11\x07");
        term.cell_frame_into(&mut input, 2, 8);
        stamp_cursor_color(&term, &mut input, theme_cursor);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(pipeline.glow_cfg.color, 0x00AA_0011, "OSC 12 pins it");
        assert_eq!(pipeline.glow_cfg.accent, brighten(0x00AA_0011, 1.5));

        term.process(b"\x1b]112\x07");
        term.cell_frame_into(&mut input, 2, 8);
        stamp_cursor_color(&term, &mut input, theme_cursor);
        pipeline.apply(&mut term, &mut input, 10, 19);
        assert_eq!(
            pipeline.glow_cfg.color, comet,
            "OSC 112 hands the comet its own hue back"
        );
    }

    #[test]
    fn batched_keystrokes_spread_across_the_advance_delta() {
        let mut p = EffectsPipeline::new();
        for _ in 0..6 {
            p.note_keystroke();
        }
        p.advance(360.0);
        let spread = p.cursor.cadence.intensity(p.now());

        let mut hand = TypingCadence::default();
        let t0 = p.t0;
        for i in 1..=6u64 {
            hand.on_keystroke(t0 + Duration::from_millis(60 * i));
        }
        let paced = hand.intensity(t0 + Duration::from_millis(360));
        assert!(
            (spread - paced).abs() < 1e-4,
            "spread replay {spread} must match the true-cadence heat {paced}"
        );

        let mut collapsed = TypingCadence::default();
        let end = t0 + Duration::from_millis(360);
        for _ in 0..6 {
            collapsed.on_keystroke(end);
        }
        assert!(
            spread < collapsed.intensity(end),
            "the spread must ignite less than a zero-gap collapse"
        );
    }

    #[test]
    fn chrome_geom_identity_and_padded_derivation() {
        let mut p = EffectsPipeline::new();
        let g = p.chrome_geom(24, 80, 9, 18);
        assert_eq!((g.origin_x, g.origin_y), (0, 0));
        assert_eq!((g.win_w, g.win_h), (80 * 9, 24 * 18));
        assert_eq!(g.head, 0);

        p.set_chrome(12, 30);
        let g = p.chrome_geom(24, 80, 9, 18);
        assert_eq!((g.origin_x, g.origin_y), (12, 42));
        assert_eq!((g.win_w, g.win_h), (80 * 9 + 24, 24 * 18 + 24 + 30));
        assert_eq!(g.head, 30);

        p.set_chrome(0, 0);
        let g = p.chrome_geom(24, 80, 9, 18);
        assert_eq!((g.origin_x, g.origin_y, g.head), (0, 0, 0));
        assert_eq!((g.win_w, g.win_h), (80 * 9, 24 * 18));
    }

    #[test]
    fn fire_glow_splices_field_streams_into_the_head_band() {
        let (cell_w, cell_h) = (10usize, 19usize);
        let (pad, head) = (12u16, 30u16);
        let origin_y = pad + head; // grid top; the head band is above it
        let mut p = EffectsPipeline::new();
        p.set_chrome(pad, head);
        p.set_cursor_glow(
            true,
            "ember",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            64,
            1.0,
            2.0,
            true,
            0x00FF_8833,
        );
        let mut term = Terminal::new(12, 40);
        let mut input = term.cell_frame(12, 40);
        p.apply(&mut term, &mut input, cell_w, cell_h);
        let (mut any_patch, mut band_patch, mut any_companion) = (false, false, false);
        for _ in 0..3 {
            for ch in [b"a", b"b", b"c", b"d"] {
                commit_ascii(&mut p, &mut term, &mut input, ch, 30.0);
                any_patch |= !input.fire_patch.is_empty();
                band_patch |= input.fire_patch.iter().any(|q| q.y < origin_y);
                any_companion |= !input.glow_under.is_empty()
                    || !input.char_fg.is_empty()
                    || !input.glow_halo.is_empty()
                    || !input.fire_halo.is_empty();
            }
        }
        assert!(any_patch, "a burning fire must splice fire_patch quads");
        assert!(
            band_patch,
            "under chrome the flame field must rise above the grid (y < pad+head)"
        );
        assert!(
            any_companion,
            "a burn must splice at least one companion stream (halo/under/char/rim)"
        );
    }

    #[test]
    fn effects_off_and_non_fire_styles_leave_fire_streams_empty() {
        let mut p = EffectsPipeline::new();
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        input.fire_patch.push(aterm_core::render::FirePatch {
            row: 0,
            x: 1,
            y: 1,
            w: 8,
            h: 8,
            base_y: 20,
            peak_h: 16,
            phase: 7,
            temp: 200,
            strength: 200,
            lean: 0,
            cov_cap: 200,
            cell_h: 19,
            mode: aterm_core::render::FireMode::Add,
        });
        input.glow_halo.push(RainHalo::default());
        input.glow_under.push(GlowQuad::default());
        input.char_fg.push(aterm_core::render::CharFg {
            row: 0,
            col: 0,
            fg: 0x00FF_0000,
        });
        input.fire_halo.push(aterm_core::render::FireHaloCell {
            row: 0,
            col: 0,
            strength: 128,
        });
        input.cursor_fill_override = Some(0x00FE_DCBA);
        let fp = p.apply(&mut term, &mut input, 10, 19);
        assert_eq!(fp, 0, "all-off apply is the identity fingerprint");
        assert!(input.fire_patch.is_empty(), "stale fire_patch cleared");
        assert!(input.glow_halo.is_empty(), "stale glow_halo cleared");
        assert!(input.glow_under.is_empty(), "stale glow_under cleared");
        assert!(input.char_fg.is_empty(), "stale char_fg cleared");
        assert!(input.fire_halo.is_empty(), "stale fire_halo cleared");
        assert_eq!(
            input.cursor_fill_override, None,
            "all-off apply clears a stale forge cursor fill"
        );

        p.set_cursor_glow(
            true,
            "lumen",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            0.9,
            0.9,
            true,
            0x0050_FA7B,
        );
        for ch in [b"a", b"b", b"c"] {
            term.process(ch);
            p.advance(30.0);
            input = term.cell_frame(6, 20);
            p.apply(&mut term, &mut input, 10, 19);
            assert!(input.fire_patch.is_empty(), "lumen never emits fire_patch");
            assert!(input.glow_under.is_empty(), "lumen never emits glow_under");
            assert!(input.char_fg.is_empty(), "lumen never emits char_fg");
            assert!(input.fire_halo.is_empty(), "lumen never emits fire_halo");
        }
    }

    #[test]
    fn unfocused_pane_emits_no_glow_but_focused_one_does() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "lumen",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            0.9,
            0.9,
            true,
            0x0050_FA7B,
        );
        let configured = p.glow_cfg.intensity;
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        let (mut lit, mut fp_focused) = (false, 0u64);
        for ch in [b"a", b"b", b"c"] {
            fp_focused = commit_ascii(&mut p, &mut term, &mut input, ch, 30.0);
            lit |= !input.cursor_glow_add.is_empty();
        }
        assert!(lit, "a focused pane emits cursor light");
        assert_ne!(fp_focused, 0, "emitted light folds a nonzero fingerprint");

        p.set_focused(false);
        for ch in [b"d", b"e", b"f"] {
            let fp = commit_ascii(&mut p, &mut term, &mut input, ch, 30.0);
            assert!(
                input.cursor_glow_add.is_empty(),
                "an unfocused pane emits no aurora"
            );
            assert!(input.glow_halo.is_empty(), "no radial halo while unfocused");
            assert!(input.fire_patch.is_empty(), "no fire field while unfocused");
            assert!(
                input.glow_under.is_empty(),
                "no under-ink flame while unfocused"
            );
            assert!(input.char_fg.is_empty(), "no charred ink while unfocused");
            assert!(
                input.fire_halo.is_empty(),
                "no contrast halo while unfocused"
            );
            assert_eq!(fp, 0, "an unfocused pane folds the identity fingerprint");
        }
        assert_eq!(
            p.glow_cfg.intensity, configured,
            "the stored intensity survives an unfocused stretch byte-unchanged"
        );
        p.set_focused(true);
        let mut relit = false;
        for ch in [b"g", b"h", b"i"] {
            commit_ascii(&mut p, &mut term, &mut input, ch, 30.0);
            relit |= !input.cursor_glow_add.is_empty();
        }
        assert!(relit, "refocus restores the light");
    }

    #[test]
    fn unfocused_glow_settles_so_the_host_stops_rebooking_the_pane() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "lumen",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            0.9,
            0.9,
            true,
            0x0050_FA7B,
        );
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        for ch in [b"a", b"b", b"c"] {
            commit_ascii(&mut p, &mut term, &mut input, ch, 30.0);
        }
        assert!(p.is_active(), "a live focused glow holds the engine active");

        p.set_focused(false);
        for _ in 0..2 {
            p.advance(16.0);
            let mut input = term.cell_frame(6, 20);
            p.apply(&mut term, &mut input, 10, 19);
        }
        assert!(
            !p.is_active(),
            "an idle unfocused pane settles so the shared rAF can drop it"
        );
    }

    #[test]
    fn hidden_visibility_hard_drains_cursor_state_before_refocus_without_apply() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "lumen",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            0.9,
            0.9,
            true,
            0x0050_FA7B,
        );
        p.set_cursor_trail(true, 400, 24, None, 0x0050_FA7B);
        let g = Geom {
            cw: 8,
            ch: 16,
            rows: 6,
            cols: 20,
            origin_x: 0,
            origin_y: 0,
            win_w: 160,
            win_h: 96,
            head: 0,
        };
        let now = Instant::now();
        p.cursor.glow.tick(
            Some((2, 2)),
            now,
            &p.glow_cfg,
            g,
            &mut p.cursor.glow_scratch,
        );
        p.cursor
            .trail
            .tick(Some((2, 2)), now, &p.trail_cfg, &mut p.cursor.trail_scratch);
        let moved = now + Duration::from_millis(1);
        p.cursor.glow.note_synthetic_move(moved);
        p.cursor.trail.note_synthetic_move(moved);
        p.cursor.glow.tick(
            Some((2, 3)),
            moved,
            &p.glow_cfg,
            g,
            &mut p.cursor.glow_scratch,
        );
        p.cursor.trail.tick(
            Some((2, 3)),
            moved,
            &p.trail_cfg,
            &mut p.cursor.trail_scratch,
        );
        assert!(p.cursor.glow.is_active() && p.cursor.trail.is_active());

        let pending = moved + Duration::from_millis(1);
        p.note_keystroke();
        p.cursor.glow.note_synthetic_move(pending);
        p.cursor.trail.note_synthetic_move(pending);
        assert!(
            p.cursor.glow.move_licensed(pending),
            "the scripted note licenses"
        );
        p.set_effects_visibility("hidden");
        assert!(!p.cursor.glow.is_active() && !p.cursor.trail.is_active());
        assert!(
            !p.cursor.glow.move_licensed(pending),
            "a hidden teardown spends every license term"
        );
        assert!(p.cursor.glow_scratch.is_empty() && p.cursor.trail_scratch.is_empty());
        assert_eq!(p.pending_keys, 0, "hidden drops queued cursor-key replay");
        assert_eq!(p.cursor.cadence.sample(pending), (0.0, 0.0));

        p.set_effects_visibility("focused");
        let refocused = pending + Duration::from_millis(1);
        assert_eq!(
            p.cursor.glow.tick(
                Some((2, 3)),
                refocused,
                &p.glow_cfg,
                g,
                &mut p.cursor.glow_scratch,
            ),
            0
        );
        assert_eq!(
            p.cursor.trail.tick(
                Some((2, 3)),
                refocused,
                &p.trail_cfg,
                &mut p.cursor.trail_scratch,
            ),
            0
        );
        assert!(p.cursor.glow_scratch.is_empty() && p.cursor.trail_scratch.is_empty());
    }

    fn typed_page(style: &str, reduced: bool) -> (EffectsPipeline, bool) {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            style,
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            0.9,
            0.9,
            true,
            0x0050_FA7B,
        );
        p.set_reduced_motion(reduced);
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        let mut lit = false;
        for ch in b"abcdefghij" {
            commit_ascii(&mut p, &mut term, &mut input, &[*ch], 30.0);
            lit |= p.cursor_block_fill().is_some()
                || !input.cursor_glow_add.is_empty()
                || input.cursor_effect_style_override.is_some()
                || !input.cursor_trail.is_empty();
        }
        (p, lit)
    }

    const BODY_STYLES: [&str; 4] = ["rainbow kitty", "water", "lumen", "comet"];

    #[test]
    fn reduced_motion_turns_off_the_page_cursor_family() {
        for style in BODY_STYLES {
            let (_, lit) = typed_page(style, false);
            assert!(lit, "{style}: control — a fast hand lights the family");
            let (p, lit) = typed_page(style, true);
            assert!(!lit, "{style}: reduced motion lit the cursor family");
            assert!(
                !p.cursor.bodies_active(),
                "{style}: reduced motion left a body animating"
            );
        }
    }

    #[test]
    fn a_page_keeps_its_wake_until_the_momentum_glow_is_cold() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "classic",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            0.9,
            0.9,
            true,
            0x0050_FA7B,
        );
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        for ch in b"abcdefghij" {
            commit_ascii(&mut p, &mut term, &mut input, &[*ch], 60.0);
        }
        assert!(
            p.cursor_block_fill()
                .is_some_and(|owned| owned.owner == crate::cursor_glow::BlockFillOwner::Momentum),
            "fixture: the typed caret is the momentum glow's"
        );
        let (mut held_by_settle, mut frames) = (false, 0u32);
        loop {
            let dt_ms = match p.wake() {
                Wake::Frames => 16.7,
                Wake::At(at) => {
                    let ms = at.saturating_duration_since(p.now()).as_secs_f64() * 1000.0;
                    assert!(ms > 0.0, "a named wake at or before now is a busy re-arm");
                    ms
                }
                Wake::Idle => break,
            };
            held_by_settle |= p.cursor.momentum.settling() && !p.cursor.bodies_active();
            assert!(
                !p.cursor.momentum.settling() || p.is_active(),
                "a settling glow keeps the page active, or the frame gate skips its step"
            );
            idle(&mut p, &mut term, &mut input, 1, dt_ms);
            frames += 1;
            assert!(frames < 5_000, "the page never went idle");
        }
        assert!(
            held_by_settle,
            "negative control: the frame cadence alone let go of a hot glow"
        );
        assert!(
            !p.cursor.momentum.is_active() && !p.cursor.momentum.settling(),
            "idle only once the glow is cold"
        );
        assert!(
            p.cursor_block_fill().is_none(),
            "the last frame before idle returned the caret to the terminal"
        );
    }

    #[test]
    fn hidden_visibility_retires_the_bodies_before_the_next_apply() {
        for style in BODY_STYLES {
            let (mut p, _) = typed_page(style, false);
            assert!(
                p.cursor.bodies_active(),
                "{style}: control — the typed page has a live body"
            );
            p.set_effects_visibility("hidden");
            assert!(
                !p.cursor.bodies_active(),
                "{style}: a hidden page kept a live body"
            );
            assert!(!p.is_active(), "{style}: a hidden page asked for frames");
        }
    }

    #[test]
    fn refocus_resumes_without_a_phantom_comet() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
        let mut term = Terminal::new(6, 20);
        term.process(b"ab");
        p.advance(30.0);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);

        p.set_focused(false);
        for ch in [b"c", b"d", b"e", b"f", b"g", b"h"] {
            term.process(ch);
            p.advance(30.0);
            let mut input = term.cell_frame(6, 20);
            p.apply(&mut term, &mut input, 10, 19);
        }

        p.set_focused(true);
        p.advance(30.0);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        assert!(
            input.cursor_trail.is_empty(),
            "refocus at a stationary cursor sweeps no cells"
        );
    }

    #[test]
    fn pipeline_typed_keys_light_and_cold_program_motion_stays_dark() {
        let mut typed = EffectsPipeline::new();
        typed.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
        let mut typed_term = Terminal::new(6, 20);
        let mut typed_input = typed_term.cell_frame(6, 20);
        typed.apply(&mut typed_term, &mut typed_input, 10, 19); // seed cursor

        for _ in 0..4 {
            typed.note_keystroke();
            typed_term.process(b"x");
        }
        typed.advance(16.0);
        typed_term.cell_frame_into(&mut typed_input, 6, 20);
        typed.apply(&mut typed_term, &mut typed_input, 10, 19);
        assert!(
            !typed_input.cursor_trail.is_empty(),
            "an rAF batch of real presses licenses its coalesced sweep"
        );

        let mut cold = EffectsPipeline::new();
        cold.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
        let mut cold_term = Terminal::new(6, 20);
        let mut cold_input = cold_term.cell_frame(6, 20);
        cold.apply(&mut cold_term, &mut cold_input, 10, 19); // seed cursor
        cold_term.process(b"\x1b[1;5H");
        cold.advance(16.0);
        cold_term.cell_frame_into(&mut cold_input, 6, 20);
        cold.apply(&mut cold_term, &mut cold_input, 10, 19);
        assert!(
            cold_input.cursor_trail.is_empty(),
            "unwitnessed program CUP movement cannot mint a classic comet"
        );
        assert!(!cold.cursor.trail.is_active());
    }

    #[test]
    fn unfocused_trail_clears_its_comet_but_still_publishes_the_colour() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        let mut swept = false;
        for ch in [b"a", b"b", b"c"] {
            commit_ascii(&mut p, &mut term, &mut input, ch, 30.0);
            swept |= !input.cursor_trail.is_empty();
        }
        assert!(swept, "a focused pane sweeps a comet");
        assert!(
            p.cursor.trail.is_active(),
            "sparks are live before the gate"
        );

        p.set_focused(false);
        p.advance(30.0);
        term.cell_frame_into(&mut input, 6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        assert!(
            input.cursor_trail.is_empty(),
            "the gate clears the comet while its sparks would still be alive"
        );
        assert!(
            !p.cursor.trail.is_active(),
            "the trail settles on its first dark tick"
        );
        assert_ne!(
            input.cursor_trail_color, 0,
            "the colour is still published while unfocused"
        );

        p.advance(1000.0);
        term.cell_frame_into(&mut input, 6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        assert_eq!(
            input.cursor_trail_color, 0x0050_FA7B,
            "the colour is keyed on the stored flag, not the focus-gated local"
        );
    }

    #[test]
    fn focus_does_not_alter_word_decoration_emission() {
        let drive = |focused: bool| {
            let mut p = EffectsPipeline::new();
            p.set_sparkle_enabled(true);
            p.set_focused(focused);
            let mut term = Terminal::new(4, 24);
            term.process(b"\r\n\r\na nice kitty");
            let mut input = RenderInput::default();
            p.advance(10.0);
            term.cell_frame_into(&mut input, 4, 24);
            p.apply(&mut term, &mut input, 10, 20);
            let mut frames = Vec::new();
            for _ in 0..60 {
                p.advance(100.0);
                term.cell_frame_into(&mut input, 4, 24);
                p.apply(&mut term, &mut input, 10, 20);
                frames.push((
                    free_sprites(&input).len(),
                    input.word_decorations.clone(),
                    input.ink.clone(),
                ));
            }
            frames
        };
        let focused = drive(true);
        let unfocused = drive(false);
        assert!(
            focused.iter().any(|(sprites, _, _)| *sprites > 0),
            "the focused cat actually played (else this proves nothing)"
        );
        assert_eq!(
            focused, unfocused,
            "decorations are out of scope for the focus gate (latch-at-birth)"
        );
    }

    #[test]
    fn the_page_floors_its_caret_against_the_cell_under_it() {
        let fill_on = |reverse: bool| {
            let mut p = EffectsPipeline::new();
            p.set_cursor_glow(
                true,
                "fire",
                crate::cursor_fx::TrailColor::Automatic,
                None,
                400,
                24,
                1.0,
                0.9,
                true,
                0x0050_FA7B,
            );
            let mut term = Terminal::new(6, 20);
            term.process(if reverse {
                b"ab\x1b[7mx\x1b[27m\x1b[1;3H"
            } else {
                b"abx\x1b[1;3H"
            });
            let mut input = term.cell_frame(6, 20);
            p.apply(&mut term, &mut input, 10, 19);
            let cell = aterm_render::rgb_to_u32(input.cells[0][2].bg);
            (
                input
                    .cursor_fill_override
                    .expect("the forge owns the caret"),
                cell,
            )
        };
        let (fill, cell) = fill_on(true);
        let (plain, page_cell) = fill_on(false);
        assert_ne!(
            cell, page_cell,
            "SGR 7 gives the caret's cell its own ground"
        );
        let ratio = crate::color_math::contrast_ratio(fill, cell);
        assert!(
            ratio >= crate::cursor_fx::CARET_PAGE_MIN_CONTRAST,
            "the ember {fill:06x} on the reverse cell {cell:06x} reads {ratio:.2}:1"
        );
        assert!(
            crate::color_math::contrast_ratio(plain, cell)
                < crate::cursor_fx::CARET_PAGE_MIN_CONTRAST,
            "control: the page-floored ember {plain:06x} is under the floor on the reverse cell"
        );
    }

    #[test]
    fn fire_forge_cursor_fill_is_spliced_and_cleared_on_toggle() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "fire",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            1.0,
            0.9,
            true,
            0x0050_FA7B,
        );
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        let page = if input.default_bg == aterm_core::render::COLOR_UNSET {
            COLD_THEME_BG
        } else {
            input.default_bg & 0x00FF_FFFF
        };
        assert_eq!(
            input.cursor_fill_override,
            p.cursor.glow.forge_fill().map(|raw| {
                crate::cursor_fx::floor_block_fill(
                    crate::cursor_glow::BlockFill {
                        owner: crate::cursor_glow::BlockFillOwner::Forge,
                        fill: raw,
                        base: None,
                    },
                    page,
                    terminal_cursor_color(&term),
                    None,
                )
                .fill
            }),
            "Fire publishes its warm-metal cursor fill, held to the page floor"
        );
        assert!(
            input.cursor_fill_override.is_some(),
            "Fire's cold forge fill is deliberately still visible"
        );

        p.set_cursor_glow(
            true,
            "lumen",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            400,
            24,
            1.0,
            0.9,
            true,
            0x0050_FA7B,
        );
        term.cell_frame_into(&mut input, 6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        assert_ne!(
            p.cursor_block_fill().map(|owned| owned.owner),
            Some(crate::cursor_glow::BlockFillOwner::Forge),
            "switching away from Fire clears the reused frame's forge fill"
        );
        assert_eq!(
            input.cursor_fill_override,
            p.cursor_block_fill().map(|owned| owned.fill),
            "the reused frame carries the new owner's fill, not the forge's"
        );
    }

    fn composer_toggle_model() -> aterm_spec::derive::Model {
        use aterm_spec::ty_model;
        ty_model! {
            ComposerToggleGate {
                const Buggy = 0;
                var host = 0;
                var present = 0;
                var engine = 0;
                action Key {
                    host = 1;
                    engine = if present == 1 { 1 } else { 0 };
                }
                action TurnStart {
                    host = 0;
                    engine = 0;
                }
                action Enable {
                    present = 1;
                    engine = if Buggy == 1 { 0 } else { host };
                }
                action Disable {
                    present = 0;
                    engine = 0;
                }
                invariant BooleanBounds: host <= 1 && present <= 1 && engine <= 1;
                invariant EngineMatchesHost: engine == if present == 1 { host } else { 0 };
            }
        }
    }

    #[test]
    fn composer_toggle_model_proves_and_real_pipeline_conforms() {
        let model = composer_toggle_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        let mut state = model.init_state();
        let mut pipeline = EffectsPipeline::new();
        for action in ["Key", "Enable", "Disable", "Enable", "TurnStart", "Key"] {
            match action {
                "Key" => pipeline.note_keystroke(),
                "TurnStart" => pipeline
                    .note_matrix_rain_signal(crate::matrix_rain::RainSignal::TurnStart as u32, 4),
                "Enable" => pipeline.set_matrix_rain_enabled(true),
                "Disable" => pipeline.set_matrix_rain_enabled(false),
                _ => unreachable!(),
            }
            assert!(model.fire(action, &mut state));
            assert_eq!(i64::from(pipeline.rain_material_editing), state["host"]);
            assert_eq!(i64::from(pipeline.rain.is_some()), state["present"]);
            let engine_editing = pipeline
                .rain
                .as_deref()
                .is_some_and(MatrixRain::material_editing_for_test);
            assert_eq!(i64::from(engine_editing), state["engine"]);
        }
    }

    /// The trail's published streams lay no light on a word icon's drawn texels, while the sprite
    /// box's transparent margin keeps its light. Negative control: unshaded, the same frames lay
    /// light on the face.
    #[test]
    fn the_published_trail_lays_no_light_on_a_word_icon() {
        use aterm_core::render::FxTexels;
        use aterm_render::sprites::FreeZ;
        // (frames with an icon, lit face texels, lit margin texels).
        let run = |unshaded: bool| -> (usize, usize, usize) {
            let mut p = EffectsPipeline::new();
            rainbow_kitty_glow(&mut p, "rainbow kitty");
            p.set_sparkle_enabled(true);
            p.word_icons_unshaded = unshaded;
            let (mut term, mut input) = glass(8, 40);
            // The caret on the last row, so the Return scrolls the `fox` up
            // into a box no keep-out held yet (`enter-bottom`).
            term.process(b"\r\n\r\n\r\n\r\n\r\n\r\n\r\n");
            idle(&mut p, &mut term, &mut input, 2, 33.0);
            type_line(&mut p, &mut term, &mut input, b"the quick brown fox", 33.0);
            idle(&mut p, &mut term, &mut input, 45, 33.0);
            p.note_console_input(PetInputKind::Submit);
            term.process(b"\r\n");
            let (mut framed, mut face, mut margin) = (0usize, 0usize, 0usize);
            for _ in 0..30 {
                idle(&mut p, &mut term, &mut input, 1, 16.0);
                let icons: Vec<_> = free_sprites(&input)
                    .into_iter()
                    .filter(|s| s.z == FreeZ::UnderText && !s.is_blank())
                    .collect();
                if icons.is_empty() {
                    continue;
                }
                framed += 1;
                // Whether window pixel (x, y) is an icon's texel at least
                // half opaque (`None`: off every icon's visible box).
                let texel = |x: i32, y: i32| -> Option<bool> {
                    icons.iter().find_map(|s| {
                        let v = s.visible();
                        if !(v.x0..v.x1).contains(&x) || !(v.y0..v.y1).contains(&y) {
                            return None;
                        }
                        let (dx, dy) = (x - s.x, y - s.y);
                        let (w, h) = (i32::from(s.w), i32::from(s.h));
                        let (aw, ah) = (i32::from(s.art.w()), i32::from(s.art.h()));
                        let mx = if s.flip_x { w - 1 - dx } else { dx };
                        let sx = ((2 * mx + 1) * aw / (2 * w)).min(aw - 1) as usize;
                        let sy = ((2 * dy + 1) * ah / (2 * h)).min(ah - 1) as usize;
                        let t = s.art.texel(sx, sy);
                        Some(match s.art.texels() {
                            FxTexels::Index8 => t != 0,
                            _ => t >> 24 >= 128,
                        })
                    })
                };
                let mut tally = |x: u16, y: u16, w: u16, h: u16| {
                    for py in i32::from(y)..i32::from(y) + i32::from(h) {
                        for px in i32::from(x)..i32::from(x) + i32::from(w) {
                            match texel(px, py) {
                                Some(true) => face += 1,
                                Some(false) => margin += 1,
                                None => {}
                            }
                        }
                    }
                };
                for q in input.cursor_glow_add.iter().chain(&input.glow_under) {
                    if (q.color | q.color2) & 0x00FF_FFFF != 0 || q.alpha | q.alpha2 != 0 {
                        tally(q.x, q.y, q.w, q.h);
                    }
                }
                for h in &input.glow_halo {
                    tally(h.x, h.y, h.w, h.h);
                }
            }
            (framed, face, margin)
        };
        let (framed, face, margin) = run(false);
        let (control_framed, control_face, _) = run(true);
        assert!(
            framed > 10 && control_framed > 10,
            "fixture: the word pass drew the `fox` ({framed}, {control_framed} frames)"
        );
        assert!(
            control_face > 0,
            "control: unshaded, the Return's light lies on the face"
        );
        assert_eq!(face, 0, "the published trail lays light on the face");
        assert!(
            margin > 0,
            "the box's transparent margin keeps its light (no notch beside the face)"
        );
    }

    /// A FRAME WITHOUT CELLS NAMES ITS WORD ICONS (looks7 M10): a host that
    /// hands no cells runs the word pass from the terminal, and the icons it
    /// draws reach the next frame's sky, ribbon and embers as the grid's
    /// pass's do, where they were never named and the sky drew its stars on
    /// the `fox`. Negative control: unnamed, the glow holds none of them.
    #[test]
    fn a_frame_without_cells_names_its_word_icons() {
        use aterm_render::sprites::FreeZ;
        // (frames that drew an icon, frames whose icons the glow does not hold).
        let run = |unnamed: bool| -> (usize, usize) {
            let mut p = EffectsPipeline::new();
            rainbow_kitty_glow(&mut p, "rainbow kitty");
            p.set_sparkle_enabled(true);
            p.word_icons_unnamed = unnamed;
            let (mut term, mut input) = glass(8, 40);
            term.process(b"\r\n\r\n\r\n\r\n\r\n\r\n\r\n");
            idle(&mut p, &mut term, &mut input, 2, 33.0);
            type_line(&mut p, &mut term, &mut input, b"the quick brown fox", 33.0);
            idle(&mut p, &mut term, &mut input, 45, 33.0);
            p.note_console_input(PetInputKind::Submit);
            term.process(b"\r\n");
            let (rows, cols) = (input.rows, input.cols);
            let (mut framed, mut unheld) = (0, 0);
            for _ in 0..30 {
                p.advance(16.0);
                term.cell_frame_into(&mut input, rows, cols);
                input.cells.clear();
                input.line_sizes.clear();
                let _ = p.apply(&mut term, &mut input, 10, 19);
                let drawn = free_sprites(&input)
                    .iter()
                    .filter(|s| s.z == FreeZ::UnderText && !s.is_blank())
                    .count();
                framed += usize::from(drawn > 0);
                unheld += usize::from(drawn > 0 && p.cursor.glow.word_icon_boxes().len() != drawn);
            }
            (framed, unheld)
        };
        let (framed, unheld) = run(false);
        assert!(
            framed > 10,
            "fixture: the word pass drew the `fox` in {framed} frames"
        );
        assert_eq!(
            unheld, 0,
            "{unheld} of {framed} frames' icons are not the glow's"
        );
        let (control_framed, control_unheld) = run(true);
        assert!(
            control_framed > 10 && control_unheld == control_framed,
            "control: unnamed, {control_unheld} of {control_framed} frames' icons are not the glow's"
        );
    }

    /// THE PET'S VEIL LIES UNDER THE BODY ITS FRAME DREW (looks7 M10): the
    /// page veil a dim theme's letters take over the pet is drawn after the
    /// pet's tick, by the body that tick drew. Drawn in the trail's step it
    /// read the body of the frame before, so a moving pet's veil lay a frame
    /// behind it: page over letters the body had left, the letters it had
    /// reached bare over its coat. A line typed on a dim page under the pet
    /// as it runs on with the caret: every frame's veil runs (the page's
    /// colour at a veil's opacity in the under stream) keep to the box the
    /// frame's body drew. Negative control: drawn in the step, runs stray
    /// outside it.
    #[test]
    fn the_pet_s_veil_lies_under_the_body_its_frame_drew() {
        // (frames with a veil, frames with a run outside the frame's body).
        let run = |stale: bool| -> (usize, usize) {
            let mut p = pet_pipeline(0x5EED);
            p.pet_veil_stale = stale;
            let (mut term, mut input) = glass(8, 40);
            let rgb =
                |c: u32| aterm_core::terminal::Rgb::new((c >> 16) as u8, (c >> 8) as u8, c as u8);
            let page = 0x0011_1318;
            term.set_default_foreground(rgb(0x0085_8585));
            term.set_default_background(rgb(page));
            stamp_effective_terminal_background(&term, &mut input);
            idle(&mut p, &mut term, &mut input, 90, 16.0);
            let pad = 3 + 3;
            let (mut veiled, mut astray) = (0, 0);
            let mut look = |p: &EffectsPipeline, input: &RenderInput| {
                let veil: Vec<&GlowQuad> = input
                    .glow_under
                    .iter()
                    .filter(|q| {
                        q.alpha.max(q.alpha2) >= 32
                            && q.color & 0x00FF_FFFF
                                == aterm_render::premul_rgb(page, q.alpha) & 0x00FF_FFFF
                            && q.color2 & 0x00FF_FFFF
                                == aterm_render::premul_rgb(page, q.alpha2) & 0x00FF_FFFF
                    })
                    .collect();
                if veil.is_empty() {
                    return;
                }
                veiled += 1;
                let Some((x0, x1, y0, y1)) = p.cursor.glow.pet_body_box() else {
                    astray += 1;
                    return;
                };
                astray += usize::from(veil.iter().any(|q| {
                    let (qx0, qy0) = (i32::from(q.x), i32::from(q.y));
                    let (qx1, qy1) = (qx0 + i32::from(q.w), qy0 + i32::from(q.h));
                    qx0 < x0 - pad || qx1 > x1 + pad || qy0 < y0 - pad || qy1 > y1 + pad
                }));
            };
            for b in b"the quick brown fox jumps over the lazy".iter() {
                p.note_console_input(PetInputKind::Text);
                let _ = commit_ascii(&mut p, &mut term, &mut input, &[*b], 16.0);
                look(&p, &input);
                for _ in 0..2 {
                    let _ = idle(&mut p, &mut term, &mut input, 1, 16.0);
                    look(&p, &input);
                }
            }
            for _ in 0..90 {
                let _ = idle(&mut p, &mut term, &mut input, 1, 16.0);
                look(&p, &input);
            }
            (veiled, astray)
        };
        let (veiled, astray) = run(false);
        let (control_veiled, control_astray) = run(true);
        assert!(
            veiled >= 3,
            "fixture: the pet crossed the dim letters in {veiled} frames"
        );
        assert_eq!(
            astray, 0,
            "{astray} of {veiled} frames veil letters off the body"
        );
        assert!(
            control_astray > 0,
            "control: drawn by the frame before, no veil strayed ({control_veiled} veiled)"
        );
    }

    #[test]
    fn the_page_samples_a_peeking_cats_palette_over_its_footprint() {
        let mut p = EffectsPipeline::new();
        p.set_sparkle_enabled(true);
        let mut term = Terminal::new(4, 20);
        term.process(b"\r\n\r\nkitty");
        let mut input = RenderInput::default();
        term.cell_frame_into(&mut input, 4, 20);
        let blank = input.cells[2][0];
        for (r, line) in input.cells.iter_mut().enumerate() {
            line.resize(20, aterm_core::terminal::RenderCell { ch: ' ', ..blank });
            for cell in line {
                cell.bg = if r == 0 { [248, 248, 248] } else { [4, 4, 4] };
            }
        }
        p.advance(10.0);
        p.apply(&mut term, &mut input, 10, 20);
        let (cats, _) = p.decos.scan_verdicts();
        let [key] = cats[..] else {
            panic!("one feline occurrence, got {cats:?}");
        };
        assert_eq!(
            key.background, 4,
            "the footprint spans the light row above the word: {key:?}"
        );
    }

    #[test]
    fn the_page_keeps_a_caret_touching_collision_form_provisional() {
        let mut p = EffectsPipeline::new();
        p.set_sparkle_enabled(true);
        p.set_sparkle_languages(&["ro"]);
        let mut term = Terminal::new(2, 48);
        let mut input = RenderInput::default();
        term.process(b"fut");
        term.cell_frame_into(&mut input, 2, 48);
        p.advance(10.0);
        p.apply(&mut term, &mut input, 10, 20);
        assert!(
            !p.decos.scan_verdicts().1,
            "`fut` under the caret is provisional"
        );

        term.process(b" ");
        term.cell_frame_into(&mut input, 2, 48);
        p.advance(10.0);
        p.apply(&mut term, &mut input, 10, 20);
        assert!(p.decos.scan_verdicts().1, "the delimiter commits it");
    }

    #[test]
    fn v3_web_resolver_pins_classes_and_custom_specs() {
        let mut p = EffectsPipeline::new();
        p.set_sparkle_enabled(true);
        p.set_sparkle_classes(true, true, true);
        let cfg = &p.sparkle.as_ref().expect("on").cfg;
        assert!(cfg.profanity && cfg.feline && cfg.emphasis);
        p.set_sparkle_ink(false, 0.75, 2200, false);
        assert!(!p.sparkle.as_ref().unwrap().cfg.emphasis);
        p.set_sparkle_custom_specs(Some(
            "[[sparkle_words.custom]]\nwords = [\"ultrathink\"]\nink = { colorway = \"rainbow\" }\n"
                .to_string(),
        ));
        let cfg = &p.sparkle.as_ref().unwrap().cfg;
        assert!(
            cfg.emphasis && cfg.spec_table.has_custom(),
            "custom specs keep emphasis scanning with ink off (v3 §6)"
        );
        p.set_sparkle_ink(true, 0.75, 2200, false);
        let mut term = Terminal::new(4, 32);
        term.process(b"go ultrathink now");
        let mut input = RenderInput::default();
        p.advance(10.0);
        term.cell_frame_into(&mut input, 4, 32);
        p.apply(&mut term, &mut input, 10, 20);
        assert_eq!(
            input.ink.len(),
            10,
            "the custom rainbow word inks its 10 lead cells"
        );
        p.set_sparkle_custom_specs(Some("[[sparkle_words.custom".to_string()));
        assert!(
            p.sparkle.as_ref().unwrap().cfg.spec_table.has_custom(),
            "malformed fragment keeps the previous specs"
        );
        assert!(
            p.sparkle_lexicon_warnings()
                .iter()
                .any(|w| w.starts_with("sparkle_words.custom:")),
            "the parse diagnostic surfaces instead of vanishing"
        );
        p.set_sparkle_custom_specs(None);
        assert!(!p.sparkle.as_ref().unwrap().cfg.spec_table.has_custom());
        assert!(
            p.sparkle_lexicon_warnings().is_empty(),
            "a clear drops the malformed-fragment warning"
        );
    }

    #[test]
    fn lexicon_warnings_surface_and_respect_cjk_single_char_opt_in() {
        let mut p = EffectsPipeline::new();
        assert!(
            p.sparkle_lexicon_warnings().is_empty(),
            "off: no lexicon, no warnings"
        );
        p.set_sparkle_enabled(true);
        assert!(
            p.sparkle_lexicon_warnings().is_empty(),
            "the builtin lexicon is conflict-free"
        );
        p.set_sparkle_custom_specs(Some(
            "[[sparkle_words.custom]]\nwords = [\"犬\", \"abc猫\"]\nink = { colorway = \"rainbow\" }\n"
                .to_string(),
        ));
        let warns = p.sparkle_lexicon_warnings();
        assert!(
            warns
                .iter()
                .any(|w| w.contains('犬') && w.contains("cjk_single_char")),
            "FIX 7: the web path surfaces the single-char-CJK conflict, got {warns:?}"
        );
        assert!(
            warns.iter().any(|w| w.contains("abc猫")),
            "FIX 7: the web path surfaces the mixed-script drop, got {warns:?}"
        );
        p.set_sparkle_feline("cat", true, true, true);
        let warns = p.sparkle_lexicon_warnings();
        assert!(
            !warns.iter().any(|w| w.contains('犬')),
            "FIX 8: cjk_single_char = true satisfies the warning, got {warns:?}"
        );
        assert!(
            warns.iter().any(|w| w.contains("abc猫")),
            "unrelated warnings survive the opt-in filter, got {warns:?}"
        );
        p.set_sparkle_feline("cat", true, true, false);
        assert!(
            p.sparkle_lexicon_warnings()
                .iter()
                .any(|w| w.contains('犬'))
        );
        p.set_sparkle_enabled(false);
        assert!(p.sparkle_lexicon_warnings().is_empty());
    }

    #[test]
    fn v3_reset_table_knob_setters_hard_reset_and_alt_suppression_freezes() {
        let mut p = EffectsPipeline::new();
        p.set_sparkle_enabled(true);
        p.set_sparkle_alt_screen_suppression(true);
        p.set_focused(true);
        let mut term = Terminal::new(4, 24);
        term.process(b"\r\n\r\na nice kitty");
        let mut input = RenderInput::default();

        p.advance(10.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20);
        let mut saw_cat = false;
        for _ in 0..60 {
            p.advance(100.0);
            term.cell_frame_into(&mut input, 4, 24);
            p.apply(&mut term, &mut input, 10, 20);
            saw_cat |= !free_sprites(&input).is_empty();
        }
        assert!(saw_cat, "the peek actually played");
        assert!(
            free_sprites(&input).is_empty(),
            "the one-shot is spent (zero sprites at rest)"
        );

        p.set_sparkle_feline("cat", true, true, false);
        p.advance(10.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20); // release present (reveal 0)
        let mut replayed = false;
        for _ in 0..10 {
            p.advance(100.0);
            term.cell_frame_into(&mut input, 4, 24);
            p.apply(&mut term, &mut input, 10, 20);
            replayed |= !free_sprites(&input).is_empty();
        }
        assert!(
            replayed,
            "web knob setters are hard_reset(): the one-shot replays (§1.1 table)"
        );

        let sprites_before = free_sprites(&input).clone();
        assert!(
            !sprites_before.is_empty(),
            "mid-peek before the alt round-trip"
        );
        term.process(b"\x1b[?1049h"); // enter the alternate screen
        p.advance(100.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20);
        assert!(
            free_sprites(&input).is_empty(),
            "suppressed: no decorations"
        );
        term.process(b"\x1b[?1049l"); // long suspension, then back
        p.advance(5000.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20);
        assert_eq!(
            free_sprites(&input).len(),
            sprites_before.len(),
            "thaw resumes the peek (no mass replay, and it did not silently end)"
        );
        // The art, its box and the window it is seen through (the peek's
        // reveal), not where the idle breath has it.
        let peek = |s: &FreeSprite| {
            let v = s.visible();
            (s.w, s.h, s.art.id(), v.y0 - s.y, v.x1 - v.x0, v.y1 - v.y0)
        };
        for (after, before) in free_sprites(&input).iter().zip(sprites_before.iter()) {
            assert_eq!(
                peek(after),
                peek(before),
                "thaw resumes the peek exactly where it froze (no mass replay)"
            );
            assert!(
                (after.x - before.x).abs() <= 2 && (after.y - before.y).abs() <= 2,
                "…and resumes in place, within the idle breath ({:?} vs {:?})",
                (after.x, after.y),
                (before.x, before.y)
            );
        }
    }

    fn synthetic_empty_frame(term: &mut Terminal, input: &mut RenderInput) {
        use aterm_core::terminal::{RenderCell, UnderlineStyle};
        term.cell_frame_into(input, 24, 80);
        let bg = input.default_bg;
        let space = RenderCell {
            ch: ' ',
            fg: [0xD0, 0xD0, 0xD0],
            bg: [(bg >> 16) as u8, (bg >> 8) as u8, bg as u8],
            wide: false,
            emoji_presentation: false,
            text_presentation: false,
            bold: false,
            italic: false,
            underline: UnderlineStyle::None,
            strikethrough: false,
            overline: false,
            underline_color: None,
            overline_color: None,
            script: aterm_core::terminal::Script::Normal,
        };
        for row in &mut input.cells {
            row.clear();
            row.resize(80, space);
        }
    }

    fn enable_classic_rain(pipeline: &mut EffectsPipeline) {
        pipeline.rain_cfg.output_material = false;
        pipeline.set_matrix_rain_enabled(true);
    }

    fn rain_until_visible(p: &mut EffectsPipeline, term: &mut Terminal, input: &mut RenderInput) {
        for _ in 0..120 {
            p.note_keystroke();
            p.advance(50.0);
            synthetic_empty_frame(term, input);
            p.apply(term, input, 10, 20);
            if !rain_rows(input).is_empty() {
                return;
            }
        }
        panic!("rain never appeared on an empty 24x80 grid");
    }

    #[test]
    fn output_streak_parks_until_its_exact_settle_deadline() {
        let mut p = EffectsPipeline::new();
        let t0 = p.now();
        let cfg = StreakConfig {
            enabled: true,
            sound: true,
            idle_secs: 10.0,
            ..StreakConfig::default()
        };
        let geom = p.chrome_geom(24, 80, 10, 20);
        let mut streak = OutputStreak::new(17);
        assert!(!streak.note_output(1, &[(4, 0, 40)], t0, false));
        let spawned = t0 + Duration::from_millis(350);
        assert!(streak.note_output(2, &[(4, 0, 40)], spawned, false));
        let mut out = Vec::new();
        let opened = streak.tick(spawned, geom, &cfg, &mut out);
        assert_eq!(
            opened.cue.map(|cue| cue.sound),
            Some(crate::output_streak::StreakSound::Shimmer)
        );

        for step in 1..=4 {
            out.clear();
            streak.tick(
                spawned + Duration::from_millis(250 * step),
                geom,
                &cfg,
                &mut out,
            );
        }
        let parked = spawned + Duration::from_secs(1);
        assert!(!streak.is_active(), "the last visible comet has retired");
        let due = streak
            .next_change_deadline(parked, cfg.idle_secs)
            .expect("the open episode still owes one Settle");
        assert!(due > parked);

        p.clock = parked.duration_since(p.t0);
        p.streak_cfg = cfg;
        p.streak = Some(Box::new(streak));
        match p.wake() {
            Wake::At(at) => assert_eq!(at, due),
            other => panic!("parked episode must own one exact wake, got {other:?}"),
        }

        let fired = due + Duration::from_millis(1);
        p.clock = fired.duration_since(p.t0);
        out.clear();
        let settled = p
            .streak
            .as_deref_mut()
            .expect("streak retained through deadline")
            .tick(fired, geom, &cfg, &mut out);
        assert_eq!(
            settled.cue.map(|cue| cue.sound),
            Some(crate::output_streak::StreakSound::Settle)
        );
        assert!(matches!(p.wake(), Wake::Idle));
    }

    #[test]
    fn the_page_streak_lights_the_nova_channel_through_apply() {
        let mut term = Terminal::new(24, 80);
        let mut input = RenderInput::default();
        let mut p = EffectsPipeline::new();
        p.set_output_streak(true, 1.0, 9, 3, 10, false, 7);
        p.set_output_streak_theme(true, 0x0000_0000, 0x00FF_FFFF, 0x0088_C0D0);
        let mut lit = 0usize;
        for i in 0..40 {
            p.advance(40.0);
            term.process(format!("line {i}\r\n").as_bytes());
            term.cell_frame_into(&mut input, 24, 80);
            p.apply(&mut term, &mut input, 10, 20);
            lit = lit.max(input.nova_add.len());
        }
        assert!(lit > 0, "the streak must actually put light in nova_add");

        p.set_reduced_motion(true);
        for i in 40..80 {
            p.advance(40.0);
            term.process(format!("line {i}\r\n").as_bytes());
            term.cell_frame_into(&mut input, 24, 80);
            p.apply(&mut term, &mut input, 10, 20);
            assert!(
                input.nova_add.is_empty(),
                "reduced motion must retire the streak"
            );
            assert!(!p.is_active(), "a reduced streak must not request frames");
        }
        p.set_reduced_motion(false);
        let mut restored = false;
        for i in 80..120 {
            p.advance(40.0);
            term.process(format!("line {i}\r\n").as_bytes());
            term.cell_frame_into(&mut input, 24, 80);
            p.apply(&mut term, &mut input, 10, 20);
            restored |= !input.nova_add.is_empty();
        }
        assert!(
            restored,
            "clearing reduced motion restores the configured streak"
        );
    }

    #[test]
    fn rain_enabled_emits_and_repeat_apply_is_byte_stable() {
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        assert!(p.matrix_rain_enabled());
        assert!(p.is_active(), "CALM weather holds the engine active");
        let mut term = Terminal::new(24, 80);
        let mut input = RenderInput::default();
        rain_until_visible(&mut p, &mut term, &mut input);
        let quads = rain_rows(&input).clone();
        let adds = input.rain_add.clone();
        let atlas_version = p.rain.as_ref().map(|r| r.atlas_version());
        let fp1 = p.apply(&mut term, &mut input, 10, 20);
        let fp2 = p.apply(&mut term, &mut input, 10, 20);
        assert_ne!(fp1, 0, "nonempty emission folds a nonzero fp");
        assert_eq!(fp1, fp2, "no advance ⇒ stable fold");
        assert_eq!(rain_rows(&input), quads, "byte-stable quads");
        assert_eq!(input.rain_add, adds, "byte-stable halos");
        assert_eq!(
            p.rain.as_ref().map(|r| r.atlas_version()),
            atlas_version,
            "no tick ⇒ no rebake"
        );
    }

    #[test]
    fn rain_only_uses_engine_deadline_while_frame_effects_keep_raf() {
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        assert_eq!(p.next_deadline_ms(), Some(83.0), "CALM rain is 12 Hz");

        p.advance(30.0);
        assert_eq!(
            p.next_deadline_ms(),
            Some(53.0),
            "unrelated redraws retain the partial rain period"
        );

        p.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
        let now = p.now();
        p.cursor
            .trail
            .tick(Some((4, 4)), now, &p.trail_cfg, &mut p.cursor.trail_scratch);
        p.advance(1.0);
        let now = p.now();
        p.cursor.trail.note_synthetic_move(now);
        p.cursor
            .trail
            .tick(Some((4, 8)), now, &p.trail_cfg, &mut p.cursor.trail_scratch);
        assert!(
            !p.cursor.trail_scratch.is_empty(),
            "the synthetic motion arm must make the deadline control non-vacuous"
        );
        assert!(p.is_active());
        assert_eq!(
            p.next_deadline_ms(),
            None,
            "a live motion effect still requests display-rAF cadence"
        );
    }

    #[test]
    fn embedded_pipeline_freezes_literal_tape_across_multiline_draft() {
        let mut p = EffectsPipeline::new();
        p.set_matrix_rain_enabled(true);
        let mut term = Terminal::new(24, 80);
        let mut input = RenderInput::default();
        term.process(b"\x1b[HREAL OUTPUT 2468\x1b[24;1H");
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);

        let initial = p
            .rain
            .as_deref()
            .expect("enabled rain engine")
            .literal_material_chars_for_test()
            .to_vec();
        for required in ['R', 'E', 'A', 'L', 'O', 'U', 'T', '2', '4', '6', '8'] {
            assert!(
                initial.contains(&required),
                "initial tape contains real output glyph {required:?}"
            );
        }

        for row in 10..17 {
            p.note_keystroke();
            term.process(format!("\x1b[{};1H> private zqjx draft 13579", row + 1).as_bytes());
            term.cell_frame_into(&mut input, 24, 80);
            p.apply(&mut term, &mut input, 10, 20);
        }
        let during_edit = p
            .rain
            .as_deref()
            .expect("enabled rain engine")
            .literal_material_chars_for_test();
        assert_eq!(
            during_edit,
            initial.as_slice(),
            "unsent edits freeze the tape"
        );
        for draft_only in ['p', 'r', 'i', 'v', 'a', 't', 'e', 'z', 'q', 'j', 'x'] {
            assert!(
                !during_edit.contains(&draft_only),
                "draft-only glyph {draft_only:?} never enters literal material"
            );
        }

        p.note_matrix_rain_signal(crate::matrix_rain::RainSignal::TurnStart as u32, 4);
        term.process(b"\x1b[2J\x1b[HREAL response z\x1b[24;1H");
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);
        let after_submit = p
            .rain
            .as_deref()
            .expect("enabled rain engine")
            .literal_material_chars_for_test();
        assert!(
            after_submit.contains(&'z'),
            "TurnStart re-enables sampling for subsequent real output"
        );
    }

    #[test]
    fn live_classic_to_literal_switch_resamples_without_grid_damage() {
        let mut p = EffectsPipeline::new();
        p.set_matrix_rain(
            30,
            6,
            4,
            6,
            None,
            None,
            "matrix",
            None,
            320,
            12,
            false,
            true,
            true,
            false,
            7,
            0x0011_1318,
            0x00D0_D0D0,
        );
        p.set_matrix_rain_enabled(true);
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[HREAL output\x1b[24;1H> draft\x1b[24;8H");
        let mut input = RenderInput::default();
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);
        let epoch = term.damage_epoch();

        p.set_matrix_rain(
            30,
            6,
            4,
            6,
            None,
            None,
            "matrix",
            None,
            320,
            12,
            false,
            true,
            true,
            true,
            7,
            0x0011_1318,
            0x00D0_D0D0,
        );
        term.cell_frame_into(&mut input, 24, 80);
        assert_eq!(input.snapshot_seq, epoch, "fixture has no new grid damage");
        p.apply(&mut term, &mut input, 10, 20);
        let chars = p
            .rain
            .as_deref()
            .expect("enabled rain")
            .literal_material_chars_for_test();
        assert!(chars.contains(&'R') && chars.contains(&'o'));
    }

    #[test]
    fn matrix_rain_theme_updates_cached_and_live_config() {
        let mut p = EffectsPipeline::new();
        p.set_matrix_rain_enabled(true);
        p.set_matrix_rain_theme(0x0012_3456, 0x00AB_CDEF, 0x0013_5799);
        assert_eq!(p.rain_cfg.default_bg, 0x0012_3456);
        assert_eq!(p.rain_cfg.theme_fg, 0x00AB_CDEF);
        assert_eq!(p.rain_cfg.theme_accent, 0x0013_5799);
        let live = p.rain.as_deref().expect("enabled rain").config_for_test();
        assert_eq!(live.default_bg, 0x0012_3456);
        assert_eq!(live.theme_fg, 0x00AB_CDEF);
        assert_eq!(live.theme_accent, 0x0013_5799);
    }

    #[test]
    fn scrollback_reading_skips_scan_and_live_return_rescans() {
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        let mut term = Terminal::new(6, 20);
        for line in 0..20 {
            term.process(format!("line {line}\r\n").as_bytes());
        }
        let mut input = RenderInput::default();
        term.cell_frame_into(&mut input, 6, 20);
        p.apply(&mut term, &mut input, 8, 16);
        let live_epoch = term.damage_epoch();
        assert!(!p.rain.as_deref().unwrap().needs_rescan(live_epoch));

        term.scroll_display(3);
        term.cell_frame_into(&mut input, 6, 20);
        let scrolled_epoch = term.damage_epoch();
        p.apply(&mut term, &mut input, 8, 16);
        assert!(
            p.rain.as_deref().unwrap().needs_rescan(scrolled_epoch),
            "translated scrollback frame is never copied or scanned"
        );

        term.scroll_display(-1000);
        term.cell_frame_into(&mut input, 6, 20);
        let returned_epoch = term.damage_epoch();
        p.apply(&mut term, &mut input, 8, 16);
        assert!(
            !p.rain.as_deref().unwrap().needs_rescan(returned_epoch),
            "returning live rebuilds from the fresh coherent frame"
        );
    }

    #[test]
    fn rain_toggle_off_clears_stale_channels_on_every_off_path() {
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        let mut term = Terminal::new(24, 80);
        let mut input = RenderInput::default();
        rain_until_visible(&mut p, &mut term, &mut input);

        p.set_matrix_rain_enabled(false);
        assert!(!p.is_active(), "dropping the engine disarms the host loop");
        let fp = p.apply(&mut term, &mut input, 10, 20);
        assert_eq!(fp, 0);
        assert!(rain_rows(&input).is_empty());
        assert!(input.rain_add.is_empty());

        p.set_matrix_rain_enabled(true);
        rain_until_visible(&mut p, &mut term, &mut input);
        p.set_matrix_rain_enabled(false);
        p.set_cursor_glow(
            true,
            "lumen",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            260,
            24,
            0.7,
            0.6,
            true,
            0x0050_FA7B,
        );
        p.advance(16.0);
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 10, 20);
        assert!(rain_rows(&input).is_empty());
        assert!(input.rain_add.is_empty());
    }

    #[test]
    fn rain_torn_snapshot_clears_channels_and_fresh_frame_resumes() {
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        let mut term = Terminal::new(24, 80);
        let mut input = RenderInput::default();
        rain_until_visible(&mut p, &mut term, &mut input);

        term.process(b"tear"); // grid damage after the snapshot: `input` is stale
        p.apply(&mut term, &mut input, 10, 20);
        assert!(rain_rows(&input).is_empty(), "torn frame: no rain quads");
        assert!(input.rain_add.is_empty(), "torn frame: no rain halos");

        rain_until_visible(&mut p, &mut term, &mut input);
    }

    #[test]
    fn apply_routes_v2_offer_and_drains_companion_impulse_to_resident_pet() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_pet(true, 0x5EED);
        p.set_cursor_glow(
            true,
            "rainbow kitty",
            crate::cursor_fx::TrailColor::Automatic,
            None,
            260,
            24,
            0.9,
            0.6,
            true,
            0x0050_FA7B,
        );
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[4;3H");
        let mut input = RenderInput::default();
        for _ in 0..30 {
            p.advance(16.0);
            term.cell_frame_into(&mut input, 24, 80);
            p.apply(&mut term, &mut input, 8, 16);
        }
        assert_eq!(p.companion().brain().pending_v2_offer(), (false, false));

        p.advance(16.0);
        p.note_console_input(PetInputKind::Navigate);
        term.process(b"\x1b[4;30H");
        term.cell_frame_into(&mut input, 24, 80);
        p.apply(&mut term, &mut input, 8, 16);

        assert!(
            p.companion().brain().pending_v2_offer().0,
            "apply must offer v2's meteor arrival perk edge to the resident pet"
        );
        assert!(
            p.cursor_mut().glow.take_companion_impulse().is_none(),
            "apply must drain v2's companion impulse after offering it to the resident pet"
        );
    }
}
