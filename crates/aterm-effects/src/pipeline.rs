// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The effects pipeline: the one driver of the effect engines. It owns every
//! effect state machine (the cursor family, sparkle words, matrix rain, the
//! output streak and the resident rainbow kitty through [`CompanionOwner`])
//! plus their per-frame channels and coordinate fences, and runs their frame
//! steps onto a [`RenderInput`]. Every host owns one per surface: each native
//! window (`aterm-gui`'s `WindowState::fx`, driving the steps in
//! `pipeline_native.rs`) and each page (`aterm-wasm`, `aterm-gpu-web`, through
//! [`EffectsPipeline::apply`]). The pet's driver lives in `companion.rs`; the
//! pipeline senses for it before the word engine ticks and emits it after
//! (`docs/DESIGN-host-boundary-2026-08-30.md` §3).
//!
//! ## Animation-drive contract (host rAF, engine idle-to-zero)
//!
//! The pipeline is clockless; the host owns time and advances it explicitly:
//!
//! 1. `advance(dt_ms)` accumulates the host's frame delta.
//! 2. Render: snapshot the grid, then [`EffectsPipeline::apply`] fills the
//!    overlay channels for the accumulated instant.
//! 3. [`EffectsPipeline::is_active`] plus
//!    [`EffectsPipeline::next_deadline_ms`] select the drive: display rAF for
//!    frame-rate motion, an exact timer for rain's 12/30 Hz cadence, then idle
//!    once every effect settles.
//!
//! Same `dt` stream + same PTY bytes gives identical frames.
//!
//! ## Defaults
//!
//! Everything is off at construction: `apply` clears the overlay channels and
//! returns fingerprint `0`, so output is byte-identical to a build without
//! effects. An embedder opts in explicitly; its config surface owns the default.

use aterm_time::Instant;
use std::time::Duration;

use aterm_core::render::RenderInput;
use aterm_core::terminal::{ContentScrollDelta, ContentScrollState, Terminal};
use aterm_render::{GlowQuad, InkCell, RainHalo, SpriteQuad, WordDecoration, theme_is_dark};

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

/// The symbols a US layout types with Shift held — what the web host can
/// know of a key's shiftedness once the browser has resolved it to a
/// character (the native seam reads the modifier itself).
const WEB_SHIFTED_SYMBOLS: &str = "~!@#$%^&*()_+{}|:\"<>?";

/// The glyph class a committed web character carries to the rainbow engine —
/// the same table the native seam prices with (`aterm-gui`'s
/// `typed_class_for`): the spacebar's rest, the `!` hero, a capital, and
/// every other printable.
#[inline]
#[must_use]
pub fn web_typed_class(ch: char) -> TypedClass {
    match ch {
        ' ' => TypedClass::Space,
        '!' => TypedClass::Bang,
        c if c.is_uppercase() => TypedClass::Capital,
        _ => TypedClass::Glyph,
    }
}

/// The coherent cold-start ground pair for `dark_theme: true` (the default
/// palette). Seeds the glow fold and replaces an unset host ground
/// (`COLOR_UNSET`). Never `0`/`0`: `fg == bg` is conceal-shaped and the glyph
/// tint stands down on it.
const COLD_THEME_FG: u32 = 0x00C8_D3F5;
const COLD_THEME_BG: u32 = 0x001A_1B26;

/// The frame grid a settling caret body's next step is forecast on in a page
/// ([`crate::cursor_fx::CursorFx::bodies_next_change_deadline`]). A page
/// learns no display rate, so it is a 60 Hz frame: a step is named no
/// sooner than this after `now`, and a faster display presents it on the
/// next rAF after the wake.
const PAGE_SETTLE_FRAME: Duration = Duration::from_micros(16_667);

/// The cursor colour the pet's contrast sampler falls back to over blank
/// glass. The engine snapshot leaves `RenderInput::cursor_color` as the
/// `COLOR_UNSET` sentinel when the frame is not cursor-authoritative, and a
/// sentinel is not a colour.
///
/// The fallback is the terminal's own resolution (`cursor_color()` else the
/// default foreground, as the native call sites use), not the glow's resolved
/// cursor colour, which is effect config and would tint the cat with the
/// wake's palette. Masked to `0x00RRGGBB`.
#[inline]
#[must_use]
fn pet_contrast_cursor(input_cursor_color: u32, term_cursor_color: u32) -> u32 {
    if input_cursor_color == aterm_core::render::COLOR_UNSET {
        term_cursor_color & 0x00FF_FFFF
    } else {
        input_cursor_color & 0x00FF_FFFF
    }
}

/// The terminal's own cursor colour, packed (the native
/// `terminal_cursor_rgb`): the OSC 12 / configured cursor colour, else the
/// default foreground.
#[inline]
#[must_use]
fn terminal_cursor_color(term: &Terminal) -> u32 {
    let c = term
        .cursor_color()
        .unwrap_or_else(|| term.default_foreground());
    aterm_render::rgb_to_u32([c.r, c.g, c.b])
}

/// The most recent completed command as the rain driver reads it:
/// `(completed_command_seq, exit)`, `None` until one reported an exit code.
#[inline]
fn term_cmd_done(term: &Terminal) -> Option<(u64, i32)> {
    term.last_completed_command()
        .and_then(|m| Some((term.completed_command_seq(), m.exit_code?)))
}

/// Close the terminal's damage session if it is still open: the pipeline's
/// half of the web path's single damage consumer.
///
/// The word and rain rescans key off `damage_epoch`, which advances once per
/// session, so a session left open would freeze them. Both web hosts extract
/// through `Terminal::cell_frame_damage_scoped_into`, which consumes the
/// session itself; an unconditional `take_damage` here would bump the
/// extraction generation and force a full refill on the host's next frame.
#[inline]
fn close_open_damage_session(term: &mut Terminal) {
    if term.has_damage() {
        term.take_damage();
    }
}

/// Whether a lexicon build-time conflict still applies under the resolved
/// scan options. The lexicon cannot see scan options, so its single-char-CJK
/// warning ("… requires `cjk_single_char = true` to scan"; the word
/// "requires" is load-bearing) fires unconditionally; once the resolved
/// config enables that opt-in the warning is satisfied. Shared by the native
/// `recompute_sparkle` log and [`EffectsPipeline::sparkle_lexicon_warnings`].
#[inline]
#[must_use]
pub fn lexicon_warning_applies(warning: &str, cjk_single_char: bool) -> bool {
    !(cjk_single_char && warning.contains("requires cjk_single_char = true"))
}

/// Coordinate identity for cursor-owned pixel/cell geometry in an embedder.
/// Any change makes retained endpoints incomparable with the next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CursorCoordinateSpace {
    rows: usize,
    cols: usize,
    cell_w: usize,
    cell_h: usize,
    chrome_pad: u16,
    chrome_head: u16,
}

/// The scroll decision both hosts take (`EffectsPipeline::sync_scroll`
/// natively, `apply` on the page), kept pure so it binds to the derived
/// `cursor_scroll_signal_model`. The table lives in `aterm-core`
/// ([`ContentScrollState::delta_since`]).
#[inline]
fn content_scroll_decision(
    previous: Option<ContentScrollState>,
    current: ContentScrollState,
) -> ContentScrollDelta {
    ContentScrollState::delta_since(previous, current)
}

/// Owns the effect engines + scratch and applies them to a frame snapshot.
pub struct EffectsPipeline {
    /// Host-advanced monotonic offset from `t0` (the injected clock).
    clock: Duration,
    /// Epoch captured at construction; only differences ever matter.
    t0: Instant,
    /// Last non-destructive terminal content-scroll snapshot consumed by this
    /// pipeline. `None` is a silent baseline, never a synthetic scroll.
    cursor_scroll_state: Option<ContentScrollState>,
    /// Last frame geometry bound to the retained cursor engines. The first
    /// sample baselines silently; every later difference retires both engines
    /// and the pet's surface, never its session-keyed latches.
    cursor_coordinate_space: Option<CursorCoordinateSpace>,
    /// `(terminal identity, alternate screen)`: who the retained light belongs
    /// to. The first sample baselines silently; every later difference is the
    /// owner edge ([`Self::fence_owner`]).
    cursor_owner_space: Option<(u64, bool)>,
    /// Whether the last frame's terminal had a foreground program owning the
    /// line (alternate screen, or OSC 133 C), so a page's `submit` is an Enter
    /// into that program's composer that keeps the glow's hand on its row
    /// ([`crate::cursor_glow::CursorGlow::note_return_keeping_hand`]).
    program_owns_line: bool,

    /// The cursor family (aurora, cadence comet, typing cadence, block-cursor
    /// bodies and their scratch) and its frame step ([`CursorFx::tick`]),
    /// shared with the native window.
    cursor: CursorFx,
    glow_cfg: GlowConfig,
    /// `true` when the host left glow color unset, so the wake follows the
    /// frame's live OSC 12 / OSC 112 cursor instead of a pinned color.
    glow_color_from_cursor: bool,
    /// The accent follows the live cursor only when both color and accent were
    /// left automatic; an explicit accent always stays fixed.
    glow_accent_from_cursor: bool,
    trail_cfg: TrailConfig,
    /// Serious Mode's cursor-body gate: no block-cursor body or momentum halo
    /// claims the caret while it holds.
    serious: bool,
    /// Who owned the block cursor on the last applied frame and the colour its
    /// body wore; the tests' witness of the caret's one owner. `None` when the
    /// terminal's own cursor colour painted the caret.
    #[cfg(test)]
    block_fill: Option<crate::cursor_glow::BlockFill>,
    /// Automatic opaque-comet color provenance, retained across frames so
    /// OSC 12 / OSC 112 and live theme changes can recolor an existing trail.
    trail_color_from_cursor: bool,
    /// Keystrokes noted since the last [`Self::advance`], replayed evenly
    /// across its `dt`: web hosts batch key events between rAF callbacks, and
    /// one shared instant would read as infinite typing speed. Capped past the
    /// cadence's saturation point so a paste flood bounds the replay work.
    pending_keys: u32,
    /// Reused row material for the glow engine's row probe
    /// (`CursorGlow::observe_row`); capacity is retained.
    row_probe_scratch: Vec<char>,
    /// The caret row the flying head last sat in the sky seat over
    /// ([`crate::rainbow_kitty::companion::SeatQuery::held_sky`]).
    flying_sky_row: Option<u16>,

    decos: WordDecorations,
    /// Compiled lexicon + resolved config; `None` while sparkle words are off.
    sparkle: Option<Resolved>,
    /// The knobs, kept across off/on toggles (native `DecoConfig` defaults).
    deco_cfg: DecoConfig,
    /// Raw `[sparkle_words.emphasis] enabled`; the resolve gate
    /// `enabled && (ink_enabled || has_custom_specs)` folds into
    /// `deco_cfg.emphasis` (a graphic-only custom word must scan with ink off).
    emphasis_enabled: bool,
    /// Languages whose ambiguous homograph entries are un-gated (native default
    /// `["en"]`); keys the lexicon build.
    languages: Vec<String>,
    /// User lexicon-override TOML (the native `lexicon` + `extra_words` channel).
    override_toml: Option<String>,
    /// The synthesized emphasis-class lexicon fragment for the custom spec words (appended to
    /// `override_toml` at lexicon build).
    custom_lexicon: String,

    /// Embedder window chrome in px for window-absolute emissions: interior
    /// padding on every edge (`pad`) plus the top-only rise band (`head`), the
    /// `[head][pad][grid][pad]` layout aterm-render composes. `0/0` keeps the
    /// identity geometry.
    chrome_pad: u16,
    chrome_head: u16,
    /// Height in px of the rows the host prepends above the grid after `apply`
    /// (the web message band, composed with `HostRowPixels::Translate`). Not
    /// part of the emission geometry (the compose moves every stream down by
    /// it); only the grid origin the frame-px pointer is read against.
    host_rows_px: u16,
    /// Raw `Lexicon::conflicts()` from the last `rebuild_sparkle` (empty while
    /// sparkle is off), surfaced filtered through
    /// [`Self::sparkle_lexicon_warnings`].
    lexicon_conflicts: Vec<String>,
    /// Parse diagnostic from the last [`Self::set_sparkle_custom_specs`] whose
    /// TOML fragment was malformed (`None` after a clean parse or a clear).
    /// Surfaced through [`Self::sparkle_lexicon_warnings`].
    custom_spec_warning: Option<String>,

    deco_scratch: Vec<WordDecoration>,
    ink_scratch: Vec<InkCell>,
    /// Free-overlay channel (peeking cats and gaze dots as `FreeSprite`s).
    free_scratch: Vec<aterm_core::render::FreeSprite>,
    nova_scratch: Vec<GlowQuad>,

    /// The resident pet's driver (`companion.rs`): brain, look verdict,
    /// session-keyed latches, pointer shadow and this frame's hit rect.
    /// Disabled at construction; opted in through [`Self::set_cursor_pet`].
    companion: CompanionOwner,
    /// Which cat the companions wear: the focused pane's claim cache, the tenure gate and the
    /// precedence law ([`crate::companion_look`]). A native window's verdict dresses
    /// `companion` through it; the page dresses its pet with its own setter and never reads it.
    look: crate::companion_look::LookPolicy,
    /// The companions beside the resident pet ([`crate::companions`]): the
    /// flying head, the sing-along, the typed detectors and the dog.
    /// A native window's; the page drives none of them.
    companions: crate::companions::Companions,
    /// The glow style string named a pet at the last
    /// [`Self::set_cursor_glow`] (`GlowSpelling::pet_of`), captured
    /// beside `ribbon_tall` because the parsed enum cannot tell
    /// `rainbow kitty` (the resident) from `rainbow kitty flying` (the earned
    /// head): the pet's ownership law reads the spelling, not the variant.
    pet_style_named: bool,
    /// Output-streak engine; `None` while disabled.
    streak: Option<Box<OutputStreak>>,
    /// The streak knobs, kept across off/on toggles (`enabled` mirrors
    /// `streak.is_some()` — the `rain_cfg` posture).
    streak_cfg: StreakConfig,
    /// The page's streak seed (`set_output_streak`), the one its engine is
    /// built with.
    streak_seed: u64,

    /// PHOSPHOR rain engine; `None` while disabled so an off pipeline carries
    /// zero rain state (the zero-cost-off posture).
    rain: Option<Box<MatrixRain>>,
    /// The rain knobs, kept across off/on toggles (`enabled` mirrors
    /// `rain.is_some()` — the `deco_cfg` posture).
    rain_cfg: RainConfig,
    /// Last host-reported visibility, replayed onto a lazily-built engine;
    /// `Focused` is also every effect's focus gate.
    rain_visibility: RainVisibility,
    /// Composer provenance survives while the lazily-owned rain engine is
    /// absent, then seeds a recreated engine on the next enable.
    rain_material_editing: bool,
    /// The rain driver's session-keyed edges (exit-status latch, the shell's
    /// Execute rising edge), the same [`RainLatches`] the native window keeps.
    rain_latches: RainLatches,
    /// Whole milliseconds already fed to rain's accumulator; fractional rAF
    /// deltas carry over in `clock`.
    rain_clock_ms: u64,
    rain_scratch: Vec<SpriteQuad>,
    rain_add_scratch: Vec<RainHalo>,

    /// Scratch for a composed frame's per-pane word pass before it is
    /// translated into the pane and appended to the window's channels
    /// ([`Self::word_panes`]).
    pane_deco: Vec<WordDecoration>,
    pane_ink: Vec<InkCell>,
    pane_free: Vec<aterm_core::render::FreeSprite>,
    pane_nova: Vec<GlowQuad>,
    /// Output streak per visible terminal pane of a composed frame, keyed
    /// `(session, pane slot)`. The single grid's engine is [`Self::streak`].
    streak_panes: std::collections::BTreeMap<(u64, usize), OutputStreak>,
}

impl Default for EffectsPipeline {
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
                // The cold-start style is Lumen, which has no ribbon; `apply`
                // overwrites this from the real spelling.
                ribbon_tall: false,
                ribbon_flat: false,
                // Lumen is not the classic style; `apply` re-derives this.
                classic_mono: false,
                // Coherent cold-start pair (see `COLD_THEME_FG`); overwritten
                // each frame by the fold in `apply`.
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
            glow_color_from_cursor: true,
            glow_accent_from_cursor: true,
            trail_cfg: TrailConfig {
                enabled: false,
                duration: Duration::from_millis(260),
                max_len: 24,
                color: 0x0050_FA7B,
                intensity: 0.0,
                warmth: 0.0,
            },
            trail_color_from_cursor: true,
            serious: false,
            #[cfg(test)]
            block_fill: None,
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

    /// Set the embedder's window chrome (per-edge `pad` + top-only `head`, px)
    /// so effect emissions are window-absolute within the padded frame. `0/0`
    /// restores the identity layout.
    pub fn set_chrome(&mut self, pad: u16, head: u16) {
        self.chrome_pad = pad;
        self.chrome_head = head;
    }

    /// Set the height in px of the rows the host prepends above the grid after
    /// the next `apply`, so the frame-px pointer is read against the grid where
    /// it is drawn. Call before every `apply`; the pet's hit rect is taken in
    /// that frame's space. Emissions do not move (the compose translates them)
    /// and nothing is retired. `0` (the default) means nothing is prepended.
    pub fn set_host_rows_px(&mut self, px: u16) {
        self.host_rows_px = px;
    }

    /// Window-space geometry for a `rows`×`cols` grid of `cell_w`×`cell_h`
    /// cells under the configured chrome: origin `(pad, pad + head)`, frame
    /// `(grid + 2·pad, grid + 2·pad + head)`.
    fn chrome_geom(&self, rows: usize, cols: usize, cell_w: usize, cell_h: usize) -> Geom {
        let pad = self.chrome_pad;
        let head = self.chrome_head;
        let grid_w = u16::try_from(cols * cell_w).unwrap_or(u16::MAX);
        let grid_h = u16::try_from(rows * cell_h).unwrap_or(u16::MAX);
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

    /// Advance the injected clock by `dt_ms` (host rAF delta), replaying the
    /// keystrokes noted since the last step and feeding rain its whole elapsed
    /// milliseconds. Negative/NaN deltas are ignored.
    pub fn advance(&mut self, dt_ms: f64) {
        if !(dt_ms.is_finite() && dt_ms > 0.0) {
            return;
        }
        let prev = self.clock;
        self.clock += Duration::from_secs_f64(dt_ms / 1000.0);
        debug_assert!(self.clock > prev, "the injected clock is monotonic");
        // Replay pending keystrokes evenly across the elapsed delta, the last
        // at the new now. The instants stay within `(prev, clock]`, so the
        // result is still deterministic in the dt stream.
        let n = self.pending_keys;
        if n > 0 {
            self.pending_keys = 0;
            let dt = self.clock - prev;
            let mut newest = self.t0 + self.clock;
            for i in 1..=n {
                let at = self.t0 + prev + dt.mul_f64(f64::from(i) / f64::from(n));
                self.cursor.cadence.on_keystroke(at);
                // Typing momentum builds at each key's replayed instant.
                self.cursor.momentum.on_key(at, MOMENTUM_GLOW_TAU_S);
                // One key, one one-cell typed credit; the glow's fixed-size
                // credit ring bounds a paste flood.
                self.cursor.glow.note_typed_cells(at, 1);
                newest = at;
            }
            // The trail admits movement only with host provenance: replay the
            // batch at the newest key's instant. A 3+ key batch is classed as
            // coalesced typing, not a repaint re-anchor.
            self.cursor.trail.note_typed_batch(newest, n);
        }
        // Feed rain the delta of the clock's ms floor so sub-ms fractions
        // carry over.
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

    /// `true` while any effect is animating. Hosts must also consult
    /// [`Self::next_deadline_ms`]: rain runs at its own 12/30 Hz cadence.
    ///
    /// The pet contributes `needs_frames()` or a named next visible step, never
    /// the brain's `is_active` (a cat exists): a sleeping cat pins nothing.
    /// A settling caret body (the rainbow caret's colour release, the momentum
    /// glow's cool-down) counts: it still owes steps, paced by
    /// [`Self::next_deadline_ms`].
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

    /// Milliseconds until the next scheduled engine wake, or `None` when an
    /// active effect needs display-rAF cadence. Rain-only animation returns the
    /// time to its next tick; a glow whose only live term is the cooling forge
    /// ember (or a pending kill hint) returns its coarse poll; the streak arms
    /// only its idle cutoff or episode-settle crossing; a settling caret body
    /// names its next visible step. Settled one-shots arm no deadline.
    ///
    /// `&mut` because the rainbow caret's step is a memoised dry run of its
    /// tick ([`CursorFx::bodies_next_change_deadline`]).
    #[must_use]
    pub fn next_deadline_ms(&mut self) -> Option<f64> {
        let now = self.now();
        // Every block-cursor body animates at display cadence while its
        // frames still move (the native frame-cadence set); once they stop it
        // is settling, paced by its named step below, then inactive.
        if self.cursor.trail.is_active()
            || self.cursor.bodies_active()
            || self.decos.is_active(now)
            || self.companion.needs_frames()
        {
            return None;
        }
        // Moving glow light needs rAF; the ember/kill-hint tail paces at the
        // coarse poll. With a zero `frame_interval`, "deadline not in the
        // future" means "needs rAF".
        let glow_ms = match self.cursor.glow.next_change_deadline(now, Duration::ZERO) {
            Some(d) if d > now => Some(d.duration_since(now).as_secs_f64() * 1000.0),
            Some(_) => return None, // live motion → display-rAF
            None => None,           // glow settled
        };
        // A settled pet names the instant of its next visible step (breath,
        // blink, tail beat); an overdue offer is due now.
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
        // A settling caret body (the rainbow caret's colour release, the
        // momentum glow's cool-down) names its next visible step, never
        // sooner than a frame out; the native coarse arm reads the same.
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

    /// One scheduling verdict ([`Wake`]): `Frames` while a frame-cadence lane
    /// is live, `At` for an exact engine wake, `Idle` once everything settled.
    #[must_use]
    #[cfg(test)]
    pub fn wake(&mut self) -> Wake {
        match self.next_deadline_ms() {
            Some(ms) => Wake::At(self.now() + Duration::from_secs_f64(ms / 1000.0)),
            None if self.is_active() => Wake::Frames,
            None => Wake::Idle,
        }
    }

    /// Focus gate: an unfocused embedder fires no cat idle one-shots. Rain maps
    /// it onto its drain ladder (focused = full, unfocused = calm cap + drain);
    /// hidden arrives via [`Self::set_effects_visibility`].
    pub fn set_focused(&mut self, focused: bool) {
        self.set_rain_visibility(if focused {
            RainVisibility::Focused
        } else {
            RainVisibility::VisibleUnfocused
        });
    }

    /// Tri-state pane visibility: `"focused"` (full profile),
    /// `"visible_unfocused"` (calm cap + drain), `"hidden"` (hard drain even
    /// while the host keeps calling `advance`). Unknown states parse as focused.
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

    /// Record the visibility and replay it onto a live engine (a lazily-built
    /// one receives it at construction).
    fn set_rain_visibility(&mut self, v: RainVisibility) {
        if v == RainVisibility::Hidden && self.rain_visibility != RainVisibility::Hidden {
            // Hidden is a hard cursor-coordinate boundary: a host may refocus
            // without an intervening `apply`, and retained light would
            // resurrect old geometry. Block bodies, momentum and cadence go
            // too, or they would ask for frames the page cannot show.
            self.cursor.glow.reset();
            self.cursor.retire_bodies();
            self.pending_keys = 0;
            // The pet retires its surface only (same terminal, so its pending
            // news survives) and returns as a fresh sighting in the same coat.
            self.companion.retire_coordinate_space();
        }
        self.rain_visibility = v;
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.set_visibility(v);
        }
    }

    /// Register one text-blind keystroke for cursor-wake ignition: fast,
    /// sustained calls heat the cadence and rainbow activity spines, but cannot
    /// prove a cursor relocation. Keystrokes are queued and replayed by the
    /// next [`Self::advance`], spread evenly across its `dt`; `apply` flushes
    /// any stragglers at the current instant. Also freezes literal rain
    /// material sampling until the host reports `TurnStart` (signal code 10);
    /// occupancy keeps tracking the live grid.
    pub fn note_keystroke(&mut self) {
        // Cap far past cadence saturation (heat clamps at knee_hi ≈ 7 gains):
        // bounds the advance-side replay under a paste flood.
        self.pending_keys = self.pending_keys.saturating_add(1).min(64);
        self.rain_material_editing = true;
        // Rain weather: keystrokes keep `Calm` alive (never reach `Working`).
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_keystroke();
        }
    }

    /// Register one committed typed scalar: [`Self::note_keystroke`] with the glyph, so the
    /// press is priced at its true cell width (the terminal's own rule, `aterm_grapheme` width
    /// under the live ambiguous-width mode). A web embedder calls this from the
    /// keydown/composition handler that dispatched the key's bytes. The press credit budget
    /// counts cells: a CJK commit that banked one credit could not coalesce its two-cell echo.
    ///
    /// Returns whether the press was priced exactly. A scalar with no exact width (control,
    /// zero-width, or wider than two cells) degrades to [`Self::note_keystroke`] and reports
    /// `false`; both paths license the echo.
    #[must_use]
    pub fn note_committed_char(
        &mut self,
        term: &mut Terminal,
        _input: &RenderInput,
        ch: char,
    ) -> bool {
        let ctrl = ch.is_control();
        if !ctrl {
            // A scalar commit is one input event even when it has no advance
            // (for example a combining mark). Its echo must not mint another.
            self.note_console_input(PetInputKind::Text);
        }
        let width = if term.modes().ambiguous_width_double {
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
        // A printable scalar: the momentum glow's one feed (native: the
        // printable arm only — never a modifier, a delete or a navigation).
        self.cursor.momentum.on_key(now, MOMENTUM_GLOW_TAU_S);
        self.rain_material_editing = true;
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_keystroke();
        }
        // The glyph class matters: a space rests the synth, a capital or `!`
        // earns its hero star (the same table as the native seam).
        let class = web_typed_class(ch);
        let shifted = ch.is_uppercase()
            || (ch.is_ascii() && WEB_SHIFTED_SYMBOLS.as_bytes().contains(&(ch as u8)));
        self.cursor
            .glow
            .note_typed_glyph(now, width as u16, shifted, class);
        self.cursor.trail.note_typed(now);
        true
    }

    /// One accepted input event, including an entire committed IME run or
    /// paste. Call once after transport admission, never from output parsing
    /// or composition preview. Admission does not establish an actual edit.
    /// [`Self::note_committed_char`] already reports a scalar's Text event.
    ///
    /// The kind reaches the cursor engines with the native semantics: `delete`
    /// is the erase (the ribbon retracts), `navigate` licenses the hop without
    /// a typed credit, `submit` is Enter (inert until its flight is observed;
    /// into a running program's composer it keeps the hand on its row),
    /// `paste` the gesture. A page that only calls `note_keystroke` gets
    /// every key treated as a typed glyph.
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

    /// Visual bell: the rain engine's 2 s amber alert hue-ramp (gated by the
    /// `bell_alert` knob) and the resident pet's startle.
    pub fn note_bell(&mut self) {
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_bell();
        }
        self.companion.note_bell(self.now());
    }

    /// A kitty command was typed (`sit`, `kitty jump`, `good kitty`), as a
    /// page's `crate::typed_tricks::TrickListener` reported it; `confirmed` is
    /// the listener's `addressed`. Latch only: the pet performs it on a later
    /// frame once the caret is still, and a pipeline with no resident drops
    /// it. The latch re-arms [`Self::is_active`].
    #[cfg(test)]
    pub fn note_trick(&mut self, trick: aterm_lexicon::Trick, confirmed: bool) {
        self.companion.note_trick(self.now(), trick, confirmed);
    }

    /// The typed line turned into prose, or was aborted, before a tentative
    /// kitty command was performed: take it back. Idempotent.
    #[cfg(test)]
    pub fn revoke_trick(&mut self) {
        self.companion.revoke_trick();
    }

    /// The whole submitted line was pet talk: the pet does not grieve the
    /// fast `command not found` the shell is about to report.
    #[cfg(test)]
    pub fn note_trick_submit(&mut self) {
        self.companion.note_trick_submit(self.now());
    }

    /// Embedded host observed wheel/PgUp while an alternate-screen TUI is
    /// active: hold the rain still during transcript reading.
    pub fn note_matrix_rain_alt_scroll(&mut self) {
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_alt_scroll();
        }
    }

    /// Payload-free observable agent/tool choreography. The host sends only a
    /// stable enum code + bounded weight; literal glyphs still come exclusively
    /// from the protected terminal snapshot.
    pub fn note_matrix_rain_signal(&mut self, code: u32, weight: u32) {
        if code == crate::matrix_rain::RainSignal::TurnStart as u32 {
            self.rain_material_editing = false;
        }
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.note_signal(code, weight);
        }
    }

    /// Whether any effect surface is enabled (sparkle and rain count while
    /// quiescent). The pet has no master of its own: its term is the brain
    /// still owing frames, so it gets the one frame it needs to see its owner
    /// go before the all-off arm retires it.
    #[inline]
    #[must_use]
    pub fn enabled_any(&self) -> bool {
        self.glow_cfg.enabled
            || self.trail_cfg.enabled
            || self.sparkle.is_some()
            || self.rain.is_some()
            || self.companion.needs_frames()
            || self.streak.is_some()
    }

    // --- cursor wake ------------------------------------------------------

    /// Configure the lumen cursor aurora (the additive `cursor_glow_add`
    /// channel), with the native knobs and clamps: `style` is any
    /// `cursor_trail_style` spelling the native app accepts
    /// ([`crate::cursor_glow::spellings`]: its canonical rows and aliases,
    /// read the same way), where `off` draws nothing and an unknown spelling
    /// draws lumen; `color = None` uses the
    /// style default (Laser/Beam/Sparkle/Comet have canonical hues, others
    /// derive from `theme_cursor`), which `apply` re-resolves each frame
    /// against the live cursor ([`crate::cursor_fx::automatic_glow_color`]);
    /// `accent = None` brightens that color 1.5×;
    /// `duration_ms` clamps 30..=2000, `length` 1..=512, `intensity` 0..=1,
    /// `radius` 0..=2.
    #[allow(
        clippy::too_many_arguments,
        reason = "one call mirrors the native GlowConfig resolution; a builder would relocate the list"
    )]
    pub fn set_cursor_glow(
        &mut self,
        enabled: bool,
        style: &str,
        color: Option<u32>,
        accent: Option<u32>,
        duration_ms: u64,
        length: u32,
        intensity: f32,
        radius: f32,
        ring: bool,
        theme_cursor: u32,
    ) {
        let color_from_cursor = color.is_none();
        let accent_from_cursor = color_from_cursor && accent.is_none();
        self.glow_color_from_cursor = color_from_cursor;
        self.glow_accent_from_cursor = accent_from_cursor;
        // The pet is a spelling of the style: the parsed enum cannot tell
        // `rainbow kitty` (resident) from `rainbow kitty flying`. The species
        // (`rainbow dog pet`) is a spelling too, applied at the owner's next
        // sense.
        self.pet_style_named = GlowStyle::style_names_any_pet(style);
        self.companion
            .set_species(if GlowStyle::style_names_dog_pet(style) {
                PetSpecies::Dog
            } else {
                PetSpecies::Cat
            });
        let spelling = crate::cursor_fx::GlowSpelling::parse(style);
        // The config rule both hosts use
        // ([`crate::cursor_fx::resolve_glow_config`]). The ground pair is
        // carried forward so a reconfigure does not flip a light theme to the
        // dark default for a frame; `apply` attaches the cursor shape per frame.
        self.glow_cfg = crate::cursor_fx::resolve_glow_config(
            crate::cursor_fx::GlowKnobs {
                enabled,
                color,
                accent,
                duration_ms,
                length: length as usize,
                intensity,
                radius,
                ring,
            },
            spelling,
            theme_cursor,
            true,
            self.glow_cfg.theme_fg,
            self.glow_cfg.theme_bg,
            0.5,
        );
        // A page has no audio host and drops every cue it would drain; the engine's key seam
        // stays open.
        self.glow_cfg.audible = true;
        if !enabled {
            self.cursor.glow.reset();
        }
    }

    /// Configure the legacy opaque comet trail (the `cursor_trail` channel).
    /// `color` `None` = the theme cursor, re-tinted each frame to the live
    /// cursor (OSC 12 / OSC 112) whatever the glow style. Clamps mirror the
    /// native accessors.
    pub fn set_cursor_trail(
        &mut self,
        enabled: bool,
        duration_ms: u64,
        length: u32,
        color: Option<u32>,
        theme_cursor: u32,
    ) {
        self.trail_color_from_cursor = color.is_none();
        self.trail_cfg = TrailConfig {
            enabled,
            duration: Duration::from_millis(duration_ms.clamp(30, 2000)),
            max_len: (length as usize).clamp(1, 512),
            color: color.unwrap_or(theme_cursor) & 0x00FF_FFFF,
            // Gentle baseline; `apply` heats a per-frame copy from the typing cadence.
            intensity: 0.0,
            warmth: 0.0,
        };
        if !enabled {
            self.cursor.trail.reset();
        }
    }

    // --- the resident pet ---------------------------------------------------

    /// Opt the resident pet in (or out). The look is
    /// `KittyLook::for_launch(seed)` over a host-minted seed, so the engine
    /// stays clockless and dieless. `enabled = false` retires the body, hit
    /// rect and frame lane. Off at construction; resets the look to the launch
    /// rung.
    pub fn set_cursor_pet(&mut self, enabled: bool, seed: u64) {
        self.companion.set_enabled_seed(enabled, seed);
    }

    /// The host's stable reduce-motion preference for every effect: the cursor
    /// family draws nothing, sparkle words go static, rain stands down, and the
    /// pet is pinned at its station (drawn, but no chase, gait or arc). Not a
    /// load-shed term. [`Self::set_sparkle_reduced_motion`] and
    /// [`Self::set_matrix_rain_reduced_motion`] alias this setter.
    pub fn set_reduced_motion(&mut self, on: bool) {
        self.deco_cfg.reduced_motion = on;
        self.refresh_sparkle_cfg();
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.set_reduced_motion(on);
        }
    }

    /// The theme or palette authority changed (`set_theme` on the web hosts):
    /// retire the pet's frozen contrast sample so its next appearance
    /// re-samples the glass; behaviour, position and breed handoff continue.
    pub fn invalidate_companion_colors(&mut self) {
        self.companion.invalidate_colors();
    }

    /// The pointer moved to `(x, y)` in frame px (host-prepended rows
    /// included, see [`Self::set_host_rows_px`]). Returns `true` iff the stored
    /// pointer changed, so an idle hover costs no render. Non-finite samples
    /// are dropped.
    pub fn note_pointer_px(&mut self, x: f32, y: f32) -> bool {
        self.companion.set_pointer(Some((x, y)))
    }

    /// The pointer left the surface. Returns `true` iff there was one.
    pub fn note_pointer_leave(&mut self) -> bool {
        self.companion.set_pointer(None)
    }

    /// A left press at `(x, y)` in frame px, asked first: [`PressOutcome::Pet`]
    /// means the press landed on the drawn cat and is consumed (a stroke, never
    /// a selection); [`PressOutcome::Pass`] means the host keeps routing. The
    /// stroke latches and re-arms the frame lane; the next `apply` acts on it.
    pub fn press_px(&mut self, x: f32, y: f32) -> PressOutcome {
        self.companion.press(self.now(), (x, y))
    }

    /// The host's pet opt-in as last set.
    #[inline]
    #[must_use]
    pub fn cursor_pet_enabled(&self) -> bool {
        self.companion.enabled()
    }

    /// The alpha the last `apply` put on glass (`0` = no pet drawn) — the
    /// observability twin of the hit rect.
    #[inline]
    #[must_use]
    pub fn cursor_pet_alpha(&self) -> u8 {
        self.companion.alpha()
    }

    /// Count of console input intentions, independent of their later echoes.
    #[inline]
    #[must_use]
    pub fn cursor_pet_console_input_seq(&self) -> u64 {
        self.companion.console_input_seq()
    }

    /// Last classified console input intention; observing it never ticks.
    #[inline]
    #[must_use]
    pub fn cursor_pet_console_input_kind(&self) -> Option<PetInputKind> {
        self.companion.console_input_kind()
    }

    /// The pet's drawn body this frame in frame px `(x0, x1, y0, y1)`, the space
    /// [`Self::press_px`] reads; `None` when nothing is drawn.
    #[inline]
    #[must_use]
    pub fn companion_hit_rect(&self) -> Option<(i32, i32, i32, i32)> {
        self.companion.hit_rect()
    }

    /// Who owned the block cursor on the last applied frame and its body colour
    /// ([`crate::cursor_glow::BlockFill`]); `None` when the terminal's own
    /// cursor colour painted it.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn cursor_block_fill(&self) -> Option<crate::cursor_glow::BlockFill> {
        self.block_fill
    }

    /// Serious Mode's cursor-body gate: while on, no block-cursor body claims
    /// the caret. Off by default. The native window folds Serious Mode into its
    /// per-frame step inputs instead.
    pub fn set_serious(&mut self, on: bool) {
        self.serious = on;
    }

    // --- sparkle words ----------------------------------------------------

    /// Master sparkle-words switch. Enabling compiles the lexicon (languages +
    /// any override TOML) once; disabling drops every occurrence/episode and
    /// clears the channels next frame (the `toggle_sparkle_words` panic-off).
    pub fn set_sparkle_enabled(&mut self, on: bool) {
        if on {
            self.rebuild_sparkle(true);
        } else {
            self.sparkle = None;
            // Master off is a hard reset (done marks cleared); the lexicon
            // diagnostics drop with the lexicon.
            self.lexicon_conflicts.clear();
            self.decos.hard_reset();
        }
    }

    /// `true` while the sparkle master is on.
    #[inline]
    #[must_use]
    pub fn sparkle_enabled(&self) -> bool {
        self.sparkle.is_some()
    }

    /// Per-class gates (native `[sparkle_words.<class>] enabled`). `emphasis`
    /// resolves as `enabled && (ink_enabled || has_custom_specs)`, as natively.
    pub fn set_sparkle_classes(&mut self, profanity: bool, feline: bool, emphasis: bool) {
        self.deco_cfg.profanity = profanity;
        self.deco_cfg.feline = feline;
        self.emphasis_enabled = emphasis;
        self.recompute_emphasis();
        self.refresh_sparkle_cfg();
    }

    /// Animated-ink knobs (native `[sparkle_words.ink]`): `strength` clamps
    /// 0..=1; `sweep_ms` clamps 350..=6000, with a 600 floor while `loop_`
    /// (a flash-safety margin).
    pub fn set_sparkle_ink(&mut self, enabled: bool, strength: f32, sweep_ms: u32, loop_: bool) {
        let floor = if loop_ { 600 } else { 350 };
        self.deco_cfg.ink_enabled = enabled;
        self.deco_cfg.ink_strength = strength.clamp(0.0, 1.0);
        self.deco_cfg.ink_sweep_ms = sweep_ms.clamp(floor, 6000);
        self.deco_cfg.ink_loop = loop_;
        // Emphasis re-resolves: custom specs keep it scanning with ink off (a graphic-only
        // custom word must still fire).
        self.recompute_emphasis();
        self.refresh_sparkle_cfg();
    }

    /// Force the static path (no twinkle/jitter/pulse; novas collapse to the
    /// static glint). An alias of [`Self::set_reduced_motion`], so it reaches
    /// every effect.
    pub fn set_sparkle_reduced_motion(&mut self, on: bool) {
        self.set_reduced_motion(on);
    }

    /// Alt-screen suppression (native `[sparkle_words] suppress_in_alt_screen`,
    /// default off): when on, full-screen apps render with no decorations.
    pub fn set_sparkle_alt_screen_suppression(&mut self, on: bool) {
        self.deco_cfg.suppress_in_alt_screen = on;
        self.refresh_sparkle_cfg();
    }

    /// Feline knobs (native `[sparkle_words.feline]`): `style = "cat"` emits
    /// the authored cat; legacy `"paw"` is ink-only and emits no paw graphic.
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

    /// Profanity knobs (native `[sparkle_words.profanity]`): `style` =
    /// "rainbow" (animated rainbow ink, the default) | "nova" | "sparkle";
    /// clamps mirror the native resolver: `density` 1..=12, `anim_ms`
    /// 350..=10000 (the WCAG 2.3.1 twinkle floor), `jitter` 0..=6, `intensity`
    /// 0..=1. `supernova_chance` (0..=100, 0 disables) is the supernova
    /// escalation chance, consulted only under `style = "rainbow"`.
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

    /// The `[[sparkle_words.custom]]` TOML fragment (as the native config
    /// carries it): per-word effect specs. Words are appended to the emphasis
    /// class (CJK as `cjk = true` entries); their specs override class
    /// defaults and bypass class gates. Malformed TOML keeps the previous
    /// specs and surfaces the parse error through
    /// [`Self::sparkle_lexicon_warnings`]. `None` clears.
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
                // Keep the previous table/lexicon untouched — the caller sees
                // the diagnostic instead of a silently-cleared working config.
                self.custom_spec_warning = Some(format!("sparkle_words.custom: {e}"));
                return;
            }
        };
        let (table, lexicon) = crate::spec::build_custom(&entries);
        self.deco_cfg.spec_table = table;
        self.custom_lexicon = lexicon;
        self.recompute_emphasis();
        // Custom words extend the lexicon too: a full rebuild, hard reset included.
        self.rebuild_sparkle(false);
    }

    /// The emphasis resolve, as in the native resolver: `enabled && (ink_enabled ||
    /// has_custom_specs)`.
    fn recompute_emphasis(&mut self) {
        self.deco_cfg.emphasis = self.emphasis_enabled
            && (self.deco_cfg.ink_enabled || self.deco_cfg.spec_table.has_custom());
    }

    /// Languages whose ambiguous homograph entries un-gate (native
    /// `languages`; non-ambiguous forms always load). Rebuilds the lexicon if
    /// sparkle is enabled.
    pub fn set_sparkle_languages(&mut self, languages: &[&str]) {
        self.languages = if languages.is_empty() {
            vec!["en".to_string()]
        } else {
            languages.iter().map(|s| (*s).to_string()).collect()
        };
        self.rebuild_sparkle(false);
    }

    /// User lexicon-override TOML merged over the builtin (the native
    /// `lexicon` file + per-class `extra_words` channel — same `[[entry]]`
    /// schema). A malformed override falls back to the builtin-only lexicon
    /// (the native fail-open posture).
    pub fn set_sparkle_lexicon_override(&mut self, toml: Option<String>) {
        self.override_toml = toml;
        self.rebuild_sparkle(false);
    }

    /// Folded surfaces to never decorate (native global `deny` +
    /// `ignore_words`), replacing the current set.
    pub fn set_sparkle_deny(&mut self, words: &[&str]) {
        self.deco_cfg.ignore = words.iter().map(|w| aterm_lexicon::fold(w)).collect();
        self.refresh_sparkle_cfg();
    }

    /// Lexicon build diagnostics: user/custom surfaces that can never scan as
    /// written (single-char CJK without the opt-in, mixed-script, multi-word)
    /// and cross-class collisions from the last rebuild. Empty while sparkle is
    /// off or the lexicon is clean. Filtered by the current resolved config (a
    /// "requires `cjk_single_char = true`" warning drops once that opt-in is
    /// on). Also carries the parse diagnostic from a malformed
    /// [`Self::set_sparkle_custom_specs`] fragment.
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

    /// Re-resolve the active config (knob change; lexicon reuse) and hard-reset
    /// the per-occurrence state, as the native config reload does: `done_marks`
    /// clears and every one-shot replays after a knob change.
    fn refresh_sparkle_cfg(&mut self) {
        if let Some(rs) = self.sparkle.as_mut() {
            rs.cfg = self.deco_cfg.clone();
            self.decos.hard_reset();
        }
    }

    /// (Re)compile the lexicon + resolved config. `force` builds even when
    /// currently off (the master-enable path).
    fn rebuild_sparkle(&mut self, force: bool) {
        if self.sparkle.is_none() && !force {
            return;
        }
        let refs: Vec<&str> = self.languages.iter().map(String::as_str).collect();
        // Synthesized custom-word entries append to the user's override so
        // custom words scan; conflicts are captured off the lexicon and
        // surfaced through `sparkle_lexicon_warnings`.
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
        // A lexicon rebuild is a config reload: hard reset.
        self.decos.hard_reset();
    }

    // --- matrix rain --------------------------------------------------------

    /// Master rain switch. Enabling lazily builds the engine (ROM raster and
    /// first atlas bake are one-time warmup); disabling drops it, so the
    /// channels scrub on the next `apply` and no rain state remains.
    pub fn set_matrix_rain_enabled(&mut self, on: bool) {
        self.rain_cfg.enabled = on;
        if !on {
            self.rain = None;
        } else if self.rain.is_none() {
            let mut rain = Box::new(MatrixRain::new(self.rain_cfg));
            rain.set_visibility(self.rain_visibility);
            rain.set_reduced_motion(self.deco_cfg.reduced_motion);
            rain.set_material_editing(self.rain_material_editing);
            // The engine's clock starts at zero: anchor the ms feed here so
            // the first `advance` delivers only the post-enable delta.
            self.rain_clock_ms = self.clock.as_millis() as u64;
            self.rain = Some(rain);
        }
    }

    /// `true` while the rain master is on.
    #[inline]
    #[must_use]
    pub fn matrix_rain_enabled(&self) -> bool {
        self.rain.is_some()
    }

    /// Configure the output streak (`crate::output_streak`). Bands are clamped
    /// here as well as in the engine: `intensity` 0..=1 (0 means reduced
    /// motion: reset, not dimmed), `tail` 4..=14, `max_streaks` 1..=4,
    /// `idle_secs` 2..=120.
    ///
    /// Disabling drops the engine entirely; the knobs survive, so re-enabling
    /// restores the same configuration.
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

    /// Fold the resolved theme into the streak. Called per frame so OSC 11/12
    /// recolors apply, without resetting the engine: a theme switch re-tints
    /// comets in flight rather than killing them.
    pub fn set_output_streak_theme(
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

    /// Reduced motion for the streak: amplitude zero, which the engine treats as a full reset
    /// (pin, not ease): no quads, no cue, fingerprint 0.
    pub fn set_output_streak_reduced_motion(&mut self, reduced: bool) {
        self.streak_cfg.intensity = if reduced { 0.0 } else { 1.0 };
    }

    /// Forward one keystroke to the streak's echo discount. Without it the
    /// `input_hot` arm still works but the sharper keystroke-stamp arm is lost.
    pub fn note_output_streak_keystroke(&mut self) {
        let now = self.now();
        if let Some(s) = self.streak.as_deref_mut() {
            s.note_keystroke(now);
        }
    }

    /// Configure the rain knobs (native `[matrix_rain]`), kept across off/on
    /// toggles. Clamps mirror the native resolver: `fps` 12..=60, `density`
    /// 1..=12, `speed`/`trail` 1..=10, alphas 16..=135 (`None` derives from the
    /// luminance constraint; `head_alpha` is floored at the body alpha),
    /// `mutation_ms` 80..=2000, `idle_secs` 2..=120. `hue` ∈
    /// matrix|theme|custom (custom reads `hue_color`, unknown → matrix).
    /// `output_material`'s sampler freezes from `note_keystroke` through an
    /// explicit TurnStart, so unsent multiline drafts keep the previous
    /// real-output tape. A live engine re-resolves immediately.
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
            // The page has no `exit_tint` knob: it stays on, fed by the terminal's OSC 133 marks.
            exit_tint: true,
            // When enabled, the coherent RenderCell snapshot supplies literal
            // output codepoints; the Editing gate above withholds unsent drafts.
            output_material,
            seed,
            default_bg: default_bg & 0x00FF_FFFF,
            theme_fg: theme_fg & 0x00FF_FFFF,
            // The accent arrives with the theme ([`Self::set_matrix_rain_theme`]).
            theme_accent: self.rain_cfg.theme_accent,
        };
        if let Some(rain) = self.rain.as_deref_mut() {
            rain.set_config(self.rain_cfg);
        }
    }

    /// Update theme-derived rain colors without requiring the host to replay
    /// every matrix-rain knob after a live theme change.
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

    /// Reduce-motion for rain: the engine emits nothing, fp 0, inactive. An
    /// alias of [`Self::set_reduced_motion`].
    pub fn set_matrix_rain_reduced_motion(&mut self, on: bool) {
        self.set_reduced_motion(on);
    }

    /// Bind cursor geometry to this exact frame. A changed cell grid, metric,
    /// or chrome origin invalidates every retained cell/pixel endpoint before
    /// the next cursor sample; the current scroll snapshot is re-baselined so
    /// the same layout transition cannot also replay as content motion.
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
            // The space was replaced: the comet and the bodies retire, and the
            // rainbow kitty band takes its curtain (a reset for other styles).
            self.cursor.retire_body_geometry();
            let now = self.now();
            self.cursor.glow.curtain(now);
            self.cursor_scroll_state = Some(scroll);
            // Surface only: the pet's facts measured against the old grid (last caret, ink map,
            // hit rect) go; the session-keyed command/content latches stay. `retire_owner()`
            // belongs to the owner fence ([`Self::fence_owner`]): here it would swallow a
            // completion pending across a resize. A resize replaces no terminal, so the body
            // is rebased, not retired (retiring blinks the cat out and re-sights it elsewhere).
            self.companion.rebase_geometry();
            self.decos.rebase_kitty_cursor_placement();
            self.flying_sky_row = None;
        }
        changed
    }

    // --- per-frame application ---------------------------------------------

    /// Run every enabled effect for the current injected instant and fill the
    /// overlay channels of `input` (a fresh `cell_frame` snapshot of `term`).
    /// `cell_w`/`cell_h` are the renderer's device-pixel cell metrics.
    ///
    /// With everything disabled the channels are cleared (a reused scratch may
    /// carry the previous frame's overlays) and `0` returns.
    ///
    /// Returns a fingerprint that changes on every visible effect change
    /// (stable when settled), for host-side repaint keys.
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
            // Disabled effects have no coordinate-bearing state to mutate, but
            // still consume a silent baseline so re-enabling cannot replay a
            // historical scroll as current-frame motion.
            self.cursor_scroll_state = Some(content_scroll_state);
            self.cursor_coordinate_space = Some(coordinate_space);
            self.cursor_owner_space = Some((term.render_identity(), term.is_alternate_screen()));
            // An opted-in pet with no owner (trail master off) retires here, so
            // an off pipeline carries no pet state. The cursor bodies retire
            // too: a charged body would keep the host loop armed for nothing.
            self.companion.retire();
            self.cursor.retire_bodies();
            // No rain engine runs, but its latches keep observing, so a rain
            // enabled later reads neither the running command's start nor a
            // completion from before as news.
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
            input.cat_quads.clear();
            input.cat_atlas = None;
            input.free_sprites.clear();
            input.free_atlas = None;
            input.nova_add.clear();
            input.rain_quads.clear();
            input.rain_atlas = None;
            input.rain_add.clear();
            return 0;
        }
        // This fence must precede every cursor probe/context/tick: the terminal
        // snapshot and its overlays then describe one coherent coordinate
        // space for the entire frame.
        let coordinate_changed =
            self.sync_cursor_coordinate_space(coordinate_space, content_scroll_state);
        let alt = term.is_alternate_screen();
        self.program_owns_line =
            alt || term.shell_state() == aterm_core::terminal::ShellState::Executing;
        // The owner fence: the geometry fence above is a surface fact, this
        // one is the command stream changing hands.
        let now = self.now();
        let owner_changed = self.fence_owner((term.render_identity(), alt), now);
        // A translation (new scrollback rows) feeds the pet's `scrolled` term.
        // A changed coordinate space or owner re-baselined the snapshot and
        // takes no scroll reading this frame.
        let scroll = self.sync_scroll(content_scroll_state, now);
        let scrolled = !coordinate_changed && !owner_changed && scroll.translated_rows > 0;
        // Invalidation resets both engines and their screen context; re-feed
        // it before any replay/tick so an alt-screen TUI never hits the
        // main-screen re-anchor classifier for a frame.
        self.cursor.glow.note_context(alt);
        self.cursor.trail.note_context(alt);
        let (rows, cols) = (input.rows, input.cols);
        let cursor_snapshot_coherent = input.process_sequence
            == term.pipeline_timestamps().process_sequence
            && input.terminal_id == term.render_identity()
            && input.engine_alt == alt;
        if !cursor_snapshot_coherent {
            // `input` and `term` straddle two parser generations/owners, so
            // cursor effects cannot combine them. Seed nothing and clear every
            // retained cursor channel for this apply.
            self.cursor.glow.reset();
        }
        // Use the RenderInput snapshot, not a second live read, so the cell
        // plane and the cursor decision come from the same instant.
        let live_viewport = cursor_snapshot_coherent && input.display_offset == 0;
        if !live_viewport {
            input.cursor_visible = false;
        }
        // Motion policy ([`MotionPolicy::resolve`]): the page's reduce-motion
        // preference stands in for the OS flag, and an unfocused page is a
        // still. Reduced motion zeroes the cursor family's amplitude and
        // switches the comet off. A page sheds no load.
        let focused = self.rain_visibility == RainVisibility::Focused;
        let cursor_motion =
            MotionPolicy::resolve(MotionMode::Auto, self.deco_cfg.reduced_motion, focused);
        let motion_amp = cursor_motion.amplitude(MotionEffect::CursorGlow);
        let animate = cursor_motion.animate(MotionEffect::CursorGlow);
        let cur = (live_viewport && input.cursor_visible)
            .then_some((input.cursor_row as u16, input.cursor_col as u16));
        // Fire/Water/Vapor pick additive vs contrast from this frame's
        // background (hosts stamp OSC 11 + DECSCNM). An unset field keeps the
        // dark-ground default rather than reading the sentinel as a colour.
        self.glow_cfg.dark_theme =
            input.default_bg == aterm_core::render::COLOR_UNSET || theme_is_dark(input.default_bg);
        // The resolved pair for the fresh-typed glyph tint, passed unmasked so
        // the engine recognises the unset sentinel and stands the layer down;
        // it masks real values itself.
        self.glow_cfg.theme_fg = input.default_fg;
        self.glow_cfg.theme_bg = input.default_bg;
        // Was the cursor colour pinned (a program's live OSC 12; a page has no
        // config colour) rather than the theme's seed? Read BEFORE the colour
        // fold below, which needs it, and handed to the step as the same fact.
        let cursor_color_pinned = term.cursor_color_recoloured();
        // As natively, one colour law ([`crate::cursor_fx::automatic_glow_color`]):
        // an automatic glow colour follows the live cursor for the styles with
        // no hue of their own; Beam, Comet and Sparkle keep their canonical hue
        // until the cursor colour is pinned (and return to it on OSC 112);
        // Laser keeps storm violet; explicit colors stay fixed. The page's
        // opaque trail is its own effect (`set_cursor_trail`, configured apart
        // from the glow style), so an automatic one follows the live cursor
        // whatever the glow style.
        if input.cursor_color != aterm_core::render::COLOR_UNSET {
            let live = input.cursor_color & 0x00FF_FFFF;
            if self.glow_color_from_cursor {
                let color = crate::cursor_fx::automatic_glow_color(
                    self.glow_cfg.style,
                    live,
                    cursor_color_pinned,
                );
                self.glow_cfg.color = color;
                if self.glow_accent_from_cursor {
                    self.glow_cfg.accent = brighten(color, 1.5);
                }
            }
            if self.trail_color_from_cursor {
                self.trail_cfg.color = live;
            }
        }

        // The coherent row for the glow engine's row probe (the poof lane,
        // `CursorGlow::poof_scan`); it must land before either engine observes
        // this frame's cursor delta.
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

        // The cursor family's frame (`native::step_cursor_family`, as the native window runs
        // it): history fence, aurora, block-cursor bodies, comet and the caret's one owner. The
        // comet config is the stored one with the live cursor colour; a still page emits none.
        let mut trail_cfg = self.trail_cfg;
        trail_cfg.enabled &= animate;
        // Window-space layout under the embedder's chrome; 0/0 chrome is the
        // identity layout.
        let glow_geom = self.chrome_geom(rows, cols, cell_w, cell_h);
        // Scale intensity in place and restore it after the tick: only
        // `set_cursor_glow` may change the user's dial. `intensity <= 0` is
        // the engine's unfocus channel (it cools the thermal integrators
        // instead of wiping them).
        // The head attaches where the live shape asks.
        let configured_intensity = self.glow_cfg.intensity;
        let configured_head_dx = self.glow_cfg.head_dx;
        self.glow_cfg.intensity *= motion_amp;
        self.glow_cfg.head_dx = crate::cursor_fx::head_dx_for(input.cursor_style);
        let default_bg = if input.default_bg == aterm_core::render::COLOR_UNSET {
            COLD_THEME_BG
        } else {
            input.default_bg & 0x00FF_FFFF
        };
        // Straggler keystrokes replay at the current instant so a host that
        // renders without advancing still ignites this frame. The page has no
        // flanking rows, delivery receipts or echo anchor.
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
                    // Unused by the rainbow block (its twinkle rides momentum).
                    blink_phase: true,
                    // The terminal's own resolution of its cursor colour (OSC 12,
                    // else the default foreground) — the native `live_cursor_rgb`.
                    live_cursor: term_cursor_color,
                    cursor_color_pinned,
                    default_bg,
                    // The caret's cell in the frame this page renders: the
                    // background its block cuts the glyph out in.
                    cursor_cell_bg: cur.and_then(|(row, col)| {
                        input
                            .cells
                            .get(usize::from(row))?
                            .get(usize::from(col))
                            .map(|cell| aterm_render::rgb_to_u32(cell.bg))
                    }),
                    geom: glow_geom,
                    focused,
                    // A page has no load-shed latch.
                    shed_envelope: 1.0,
                    // The page's opaque trail takes no brightness dial: the
                    // glow's `intensity` is the aurora's and the bodies', not
                    // this separately configured trail's.
                    trail_brightness: 1.0,
                    body_allowed: !self.serious,
                    // A page's glow switch is also its momentum-glow switch.
                    momentum_glow: self.glow_cfg.enabled,
                    user_tinted: !self.glow_color_from_cursor,
                },
                glow: &self.glow_cfg,
                trail: trail_cfg,
                animate,
                // The page's own cells: the dark streak reads the rows it
                // crosses there.
                frame_cells: Some(&input.cells),
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
        // Hand over the cursor family's streams ([`Self::publish_cursor`]):
        // glow streams moved, comet cells copied. Styles that do not emit a
        // stream leave it empty.
        self.publish_cursor(input);
        // Keyed on the stored flag, not the focus-gated local: the colour is config, so
        // unfocusing must not freeze it at a stale publish.
        if self.trail_cfg.enabled {
            input.cursor_trail_color = fx.trail_color;
        }
        // The caret's one owner (forge fill, rainbow block, droplet, comet
        // nucleus, phaser emitter, laser bolt, light rod or momentum tint) and
        // its shape ride the RenderInput seam the renderer applies instead of
        // the terminal's cursor colour. Stamped every enabled frame so a style
        // toggle cannot leave a stale fill in the reused scratch.
        #[cfg(test)]
        {
            self.block_fill = fx.block_fill;
        }
        input.cursor_fill_override = fx.block_fill.map(|owned| owned.fill);
        input.cursor_effect_style_override =
            compose_caret_style_override(fx.bolt_cursor, fx.twinkle_cursor, fx.momentum_steady);

        // The resident pet, sensed before the word engine ticks: open the host frame (one-body
        // rule, bake budget), then the owner runs its level rules and ticks the brain
        // unconditionally, so its yield box is this frame's body when the word engine decides
        // what an ambient cat may stack on. Caret and viewport facts follow the pipeline's
        // snapshot-coherence rule.
        self.open_words();
        let geom = EffectGeom {
            cell_w: cell_w as u16,
            cell_h: cell_h as u16,
            rows: rows as u16,
            cols: cols as u16,
        };
        let mut facts = TerminalFacts::read(term, term.render_identity(), scrolled);
        // The snapshot's caret, hidden or not (`TerminalFacts::caret`): a pet
        // must keep chasing a caret a TUI hid for its repaint. The reader's
        // `frame_unfinished` stands: a coherent snapshot is the same parser
        // generation, and an incoherent one hands over no caret at all.
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
            // Motion policy only (the host preference with focus folded in);
            // never a load term.
            reduced_motion: !animate,
            // The one Serious Mode input: the pet leaves the glass with the
            // caret's bodies.
            serious: self.serious,
            shed_envelope: 1.0,
            shed_active: false,
            // The owner holds the pointer it was handed through
            // `note_pointer_px`; the per-frame field is for hosts that feed
            // it with the frame.
            pointer_px: None,
            capture: CaptureMode::Present,
            geometry: FrameGeom {
                rows: geom.rows,
                cols: geom.cols,
                cell_w: geom.cell_w,
                cell_h: geom.cell_h,
                // The grid's origin where the frame draws it (below the chrome
                // and host-prepended rows): the pointer's space. Used only for
                // the pointer and hit rect; sprites stay grid-relative.
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
            // The web has no song and no flying head: the pet owns every
            // frame it is admitted to.
            sing: SingFacts::default(),
            focused,
            // No app surface ever covers the page's grid, no pane is
            // composed, and the page has no room to tell the pet about.
            obscured: false,
            pane: None,
            room: None,
        });
        // A failed command's droop window hushes the caret-jump fanfare: the
        // rainbow momentum is zeroed every frame the brain grieves. The glow
        // already ticked, so the hush lands one tick later, as natively. Read
        // between `prepare` (which notes the completion) and `tick`.
        if self.companion.grieving() {
            self.cursor.glow.hush_fanfare(now);
        }
        if self.cursor.glow.take_flow_entry() {
            self.companion.note_flow(now);
        }
        let pet = self.companion.tick(pet);
        self.offer_pet_v2(&pet.pet, glow_geom, (0, 0), now);
        let _ = self.cursor.glow.take_companion_impulse();

        // Sparkle words: the one grid's word pass ([`Self::word_grid`]) over the frame already
        // resolved into `input`, rescanning only when the damage epoch moved. The alt screen is
        // decorated unless the knob suppresses it (freeze/thaw, not a reset). The config is
        // taken out for the pass and put back.
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
                            // The companion's body rect (`CompanionOwner`):
                            // the feline word under the pet's body yields to
                            // it; every other occurrence keeps its peek.
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
                    // A host that applied without a populated cell snapshot:
                    // the term walk, then the same tick.
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
                    self.decos.tick(
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
                    )
                };
                self.sparkle = Some(rs);
                fp
            }
        };
        // The word engine's positioned peek landings, drained right after its
        // tick (they clear at the next tick's start), reach the brain as
        // fractional cells. The brain range-checks them and a TTL retires them.
        self.forward_peek_cues(now, (geom.cell_w, geom.cell_h));
        // The pet's emission, outside the sparkle branch, into the scratch the
        // swap below publishes (after the sparkle-off and alt-suppressed arms
        // cleared it). The contrast fallbacks are resolved colours: an unset
        // host ground masks back to the cold-start ground, and the ink slot is
        // the terminal's cursor colour (`pet_contrast_cursor`).
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
        // `emit` already rotates its pet-cursor term; the brain's own frame
        // fingerprint rides beside it so a frame that moved the pet without
        // drawing it (a fade's last step) still moves the repaint key.
        let deco_fp = deco_fp ^ pet_fp ^ pet.fp.rotate_left(53);
        std::mem::swap(&mut input.word_decorations, &mut self.deco_scratch);
        std::mem::swap(&mut input.ink, &mut self.ink_scratch);
        // The engine produces no legacy cat quads; clear stale ones from a
        // reused host snapshot.
        input.cat_quads.clear();
        input.cat_atlas = None;
        // The atlas Arc rides only when free sprites do, so cat-free frames
        // stay byte-identical (checked on the input side after the swap).
        std::mem::swap(&mut input.free_sprites, &mut self.free_scratch);
        input.free_atlas = if input.free_sprites.is_empty() {
            None
        } else {
            self.decos.free_atlas()
        };
        // Output streak (`streak_grid`): appended to the nova channel after the
        // word pass, which owns the channel's clear, so neither engine clears
        // the other's quads; the fingerprints fold independently. The page
        // notes keystrokes directly, so no echo rides the frame.
        // `content_seq` advances on content mutation only. The page has no
        // audio host, so the cue is dropped.
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

        // Rain: occupancy rescans on grid change (damage epoch). A snapshot whose `snapshot_seq`
        // trails the live epoch is torn, and rain from it would land under text, so it emits
        // nothing; the latches advance only on a frame that reaches the driver, so an OSC 133 C
        // edge on a torn frame arrives a frame late rather than lost. With no engine the latches
        // keep observing, so an enable replays no stale start or completion.
        let rain_session = term.render_identity();
        let shell_executing = term.shell_state() == aterm_core::terminal::ShellState::Executing;
        let rain_fp = if let Some(rain) = self.rain.as_deref_mut() {
            let epoch = term.damage_epoch();
            let torn = input.snapshot_seq != epoch;
            let display_offset = term.grid().display_offset() as i32;
            if display_offset != 0 {
                // A scrollback viewport is display-translated while the cursor
                // remains grid-relative. Do no O(rows*cols) work for a frame
                // that emit must suppress; returning live forces a fresh scan.
                rain.defer_reading();
            }
            // Close the damage session when rain is on but sparkle is off or
            // suppressed (else the epoch freezes); never twice per apply, and
            // never one the host's extraction already consumed.
            if !damage_consumed {
                close_open_damage_session(term);
            }
            if torn {
                // Torn snapshot (see above): emit nothing, but the
                // agent-output clock still registers.
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
                // The rain driver's frame ([`MatrixRain::host_frame`], shared with the native
                // window).
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
                            // No damage-recency tracking on the web path: a hidden cursor
                            // protects no extra band here.
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
        std::mem::swap(&mut input.rain_quads, &mut self.rain_scratch);
        std::mem::swap(&mut input.rain_add, &mut self.rain_add_scratch);
        // The atlas Arc rides only when rain output does, so rain-free frames
        // stay byte-identical (checked input-side after the swap).
        input.rain_atlas = if input.rain_quads.is_empty() && input.rain_add.is_empty() {
            None
        } else {
            self.rain.as_mut().and_then(|r| r.rain_atlas())
        };

        // One folded fingerprint (rotations keep equal per-engine fps from
        // cancelling); host repaint keys only need "changed vs stable".
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
    CanvasInk, CursorStep, CursorStepOut, Delivery, Flanks, NovaInto, PaneDrain, PaneGrid,
    RowProbe, ScrollChange, StreakEcho, StreakGrid, StreakKnobs, StreakPane, TornBand, WordGrid,
    WordGridOut, WordPane, WordPanes, WordPanesOut, WordRescan,
};

#[cfg(test)]
#[path = "pipeline_input_tests.rs"]
mod input_contract_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_render::TrailCell;

    /// What the page's rain engine has heard (`MatrixRain::heard_for_test`).
    fn rain_heard(p: &EffectsPipeline) -> (crate::matrix_rain::RainSignal, bool) {
        p.rain.as_deref().expect("rain is on").heard_for_test()
    }

    /// One frame of the page: advance, extract, apply.
    fn page_frame(p: &mut EffectsPipeline, term: &mut Terminal, input: &mut RenderInput) {
        p.advance(16.0);
        term.cell_frame_into(input, 6, 20);
        p.apply(term, input, 8, 16);
    }

    /// A page's rain hears a command start and finish (OSC 133 C / D) through the native
    /// `MatrixRain::host_frame`, observed on the engine (Execute phase, failure ember). Negative
    /// control: the same sequence on a suppressed alternate screen.
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

            // A command runs (OSC 133 C); in the control, a full-screen one
            // that enters the alternate screen after the command starts.
            term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07");
            if suppressed_alt {
                term.process(b"\x1b[?1049h");
            }
            page_frame(&mut p, &mut term, &mut input);
            assert_eq!(p.rain_latches.shell_executing, Some((session, true)));
            // …and fails (OSC 133 D;1).
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

    /// A torn frame delays the Execute edge but does not lose it: the frame
    /// the grid outran draws no rain and does not advance the latch, so the
    /// OSC 133 C reaches the weather with the next fresh snapshot.
    #[test]
    fn a_torn_frame_delivers_the_execute_edge_on_the_next_fresh_snapshot() {
        use crate::matrix_rain::RainSignal;
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        let mut term = Terminal::new(6, 20);
        let mut input = RenderInput::default();
        page_frame(&mut p, &mut term, &mut input);
        // The command starts after this frame's extraction.
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

    /// Re-enabling rain replays no news: a command that finishes while rain is
    /// off is history to the engine an enable builds. Covers both off frames:
    /// every effect off, and rain alone off beside a live glow.
    #[test]
    fn re_enabling_web_rain_replays_no_completion_from_while_it_was_off() {
        for glow_on in [false, true] {
            let mut p = EffectsPipeline::new();
            if glow_on {
                p.set_cursor_glow(
                    true,
                    "lumen",
                    None,
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
            // Control: a failure after the enable is still news.
            term.process(b"false\r\n\x1b]133;C\x07\x1b]133;D;1\x07");
            page_frame(&mut p, &mut term, &mut input);
            assert!(
                rain_heard(&p).1,
                "glow_on={glow_on}: a failure with the rain on lights the ember"
            );
        }
    }

    /// A page's `set_cursor_glow` is a wasm boundary: a non-finite intensity
    /// or radius turns the glow off, never NaN in the engine.
    #[test]
    fn set_cursor_glow_resolves_through_the_native_config_law() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "lumen",
            None,
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
    use aterm_core::render::FreeSprite;
    use aterm_core::terminal::TerminalBuilder;

    fn stamp_effective_terminal_background(term: &Terminal, input: &mut RenderInput) {
        let color = if term.modes().reverse_video() {
            term.default_foreground()
        } else {
            term.default_background()
        };
        input.default_bg = aterm_render::rgb_to_u32([color.r, color.g, color.b]);
    }

    /// Drive one synchronously committed scalar through the embedded host's
    /// post-delivery seam, so tests claim only light a real press licensed.
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
                // StartY 3 + Delta 2: the band carries its marks down, the
                // direction a reverse index at a region top moves them.
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

        // Negative control: the no-op decision strands stale coordinates and must not validate
        // for any motion event.
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
        // The pet's owner: the trail master on a pet spelling, plus the seed door, so the
        // Tier-1 bind below projects the real resident.
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
        // The brain owes a tick as history is entered: the model's `pet_brain_pending`, read
        // from the real resident.
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
        // A deliberately stale raw bit still cannot bypass the pipeline's
        // independent coordinate-class gate.
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
                && input.free_sprites.is_empty(),
            "history takes the pet off glass on the very first frame"
        );
        assert!(
            pipeline.companion.brain().is_active(),
            "…while its brain is still fading: the pet hides, it is not retired"
        );
        // The brain ticks unconditionally: fed `caret: None` on history frames
        // it fades to zero and releases the frame lane. An unticked brain keeps
        // its alpha (the negative control below).
        for _ in 0..40 {
            if !pipeline.companion.needs_frames() {
                break;
            }
            pipeline.advance(100.0);
            term.cell_frame_into(&mut input, 5, 16);
            pipeline.apply(&mut term, &mut input, 10, 19);
            assert!(input.free_sprites.is_empty(), "history never draws the pet");
        }
        assert!(
            !pipeline.companion.needs_frames() && !pipeline.companion.brain().is_active(),
            "hidden-caret ticks let the resident fade out and release the scheduler"
        );

        // Tier 1 for the pipeline's projection of the cursor viewport
        // lifecycle: glow, classic trail, body/fill, DEC cursor, and the pet
        // projected from the real companion (alpha, fade-out, `needs_frames`).
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
        // Pet negative controls: a body retained on glass in history, and an
        // unticked brain (still opaque, lane pinned).
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

    // ── the resident pet ────────────────────────────────────────────────────

    /// A page's `set_cursor_glow` resolves every spelling through the native
    /// law: each rainbow-kitty spelling names the style (and engages v2 with
    /// no flag, matching the native default) while `… v1` is an unknown word
    /// that falls to the embedder's default; every pet alias names the pet
    /// (cat or dog by species) while the flying head and other looks do not;
    /// and the ribbon is tall unless `underline` is asked for.
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

    /// The rainbow-kitty trail with the pet spelling `style` — the owner's
    /// two terms besides the opt-in.
    fn rainbow_kitty_glow(p: &mut EffectsPipeline, style: &str) {
        p.set_cursor_glow(
            true,
            style,
            None,
            None,
            400,
            24,
            0.9,
            0.8,
            true,
            0x0050_FA7B,
        );
    }

    /// The page's boot posture: a `rainbow kitty` trail and the pet opted in
    /// with `seed`.
    fn pet_pipeline(seed: u64) -> EffectsPipeline {
        let mut p = EffectsPipeline::new();
        rainbow_kitty_glow(&mut p, "rainbow kitty");
        p.set_cursor_pet(true, seed);
        p
    }

    /// A fresh `rows`×`cols` glass with its first coherent frame extracted.
    fn glass(rows: u16, cols: u16) -> (Terminal, RenderInput) {
        let mut term = Terminal::new(rows, cols);
        configure_glass_theme(&mut term);
        let input = term.cell_frame(usize::from(rows), usize::from(cols));
        (term, input)
    }

    fn configure_glass_theme(term: &mut Terminal) {
        // Web constructors configure the terminal too; an unconfigured
        // `COLOR_UNSET` background is unknown to PetWorld.
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
        // Keep the inherited cursor color: the contrast test below exercises
        // the foreground fallback.
        term.set_default_selection_background(Some(rgb(theme.selection)));
    }

    /// Type `text` through the committed-scalar seam, one byte per `dt_ms`
    /// frame; returns the last fold.
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

    /// `frames` quiet frames of `dt_ms` each — no input, a fresh coherent
    /// snapshot per frame; returns the last fold.
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

    /// The fixtures' cell height (`commit_ascii` applies at 10×19).
    const CELL_H: u16 = 19;

    /// The pet's natural tile at `cell_h` — `ART_ROWS` tall, `ART_ASPECT`
    /// wide, the exact size `pet_cursor` bakes the body at.
    fn pet_natural_size(cell_h: u16) -> (u16, u16) {
        let nat_h = (crate::kitty_pet::ART_ROWS * f32::from(cell_h)).round();
        let nat_w = (nat_h * crate::kitty_pet::ART_ASPECT).round();
        (nat_w as u16, nat_h as u16)
    }

    /// The pet's body sprite in `free`: the one whose atlas tile is the natural
    /// body size (motes are `0.55·cell_h` squares, word-cats bake at their own
    /// size).
    fn pet_body_sprite(free: &[FreeSprite], cell_h: u16) -> Option<FreeSprite> {
        let nat = pet_natural_size(cell_h);
        free.iter().copied().find(|s| (s.aw, s.ah) == nat)
    }

    /// One OSC 133 prompt → command → output → `D;<exit>` → prompt round
    /// trip, then one frame: the completion lands in the pet's latch and the
    /// fresh prompt's caret jump lands in the glow.
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

    /// A failed command's droop window hushes the caret-jump fanfare every
    /// frame: the rainbow momentum is zeroed, so the failure's prompt jump
    /// cannot re-arm the party ring. Control: the same round trip with `D;0`
    /// keeps the momentum through the jump.
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
            // The fresh prompt's caret jump, then a frame.
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
                // …and every frame of the droop, not just its first.
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

    /// The contrast sampler's cursor slot: a stamped cursor colour is masked to
    /// `0x00RRGGBB`; the `COLOR_UNSET` sentinel falls back to the terminal's
    /// own resolution (`cursor_color()` else the default foreground), never
    /// the glow's cursor colour.
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

        // …and that colour is the terminal's, resolved native's way.
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

    /// The pipeline's three fences and the session-keyed command probe: the Hidden edge and the
    /// geometry fence are surface only, so a completion across them is still felt; the owner
    /// fence ([`EffectsPipeline::fence_owner`]) is the one edge that nulls the probe, so the
    /// old stream's last completion is a silent baseline.
    #[test]
    fn only_the_owner_fence_rebaselines_the_command_probe_not_a_hide_or_a_resize() {
        // Across the Hidden edge: felt.
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

        // Across the geometry fence (a resize on the very frame that would have carried the
        // completion): still felt.
        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(24, 80);
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        idle(&mut p, &mut term, &mut input, 1, 33.0);
        term.process(b"\r\n\x1b]133;C\x07out\r\n\x1b]133;D;1\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        p.advance(33.0);
        term.cell_frame_into(&mut input, 24, 80);
        // New cell metrics commit a new coordinate space in this very apply.
        p.apply(&mut term, &mut input, 12, 24);
        assert!(
            p.companion.grieving(),
            "a resize retires the pet's SURFACE, never its command stream: \
             the failure that just landed is still the pet's news"
        );

        // Across the owner fence (the same completion, with the terminal swapping to its
        // alternate screen instead): baselined.
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

    /// Type until the resident is on glass (bounded: it fades in over 0.30 s).
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
            pet_body_sprite(&input.free_sprites, CELL_H).is_some(),
            "fixture: its body rides free_sprites"
        );
        assert!(p.companion_hit_rect().is_some(), "fixture: it is clickable");
    }

    /// With the pet never opted in, or opted out (pointer and press included),
    /// the free channel and fold are byte-identical to a twin without the pet;
    /// the all-off pipeline is fingerprint `0` with every channel clear.
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
            assert!(ia.free_sprites.is_empty() && ib.free_sprites.is_empty());
            assert!(ia.free_atlas.is_none() && ib.free_atlas.is_none());
            assert_eq!(off.cursor_pet_alpha(), 0);
            assert_eq!(off.companion_hit_rect(), None);
            assert_eq!(off.press_px(30.0, 40.0), PressOutcome::Pass);
            assert_eq!(plain.is_active(), off.is_active());
        }
        assert!(
            plain.is_active(),
            "non-vacuous: the trail is live while the pet is off"
        );

        // The all-off pipeline with the opt-in set: fingerprint 0, every
        // channel clear, no lane — the opt-in waits for an owner.
        let mut none = EffectsPipeline::new();
        none.set_cursor_pet(true, 7);
        let (mut term, mut input) = glass(8, 40);
        assert_eq!(none.apply(&mut term, &mut input, 10, 19), 0);
        assert!(input.free_sprites.is_empty() && input.free_atlas.is_none());
        assert!(!none.is_active() && !none.enabled_any());
        assert_eq!(none.cursor_pet_alpha(), 0);
        assert!(matches!(none.wake(), Wake::Idle));
        assert!(
            none.cursor_pet_enabled(),
            "the opt-in is remembered for when an owner arrives"
        );
    }

    /// The resident on the web: within its 0.30 s fade-in a body rides
    /// `free_sprites` (with the atlas) and claims the frame lane; it walks
    /// right with the caret; after 35 s of quiet it sleeps on glass and
    /// releases the lane, with byte-stable sprites and a fixed fold.
    #[test]
    fn cursor_pet_walks_beside_the_caret_then_sleeps_and_releases_cadence() {
        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(8, 40);
        let mut on_glass_at = None;
        for frame in 1..=12u32 {
            commit_ascii(&mut p, &mut term, &mut input, b"k", 33.0);
            if pet_body_sprite(&input.free_sprites, CELL_H).is_some() {
                on_glass_at = Some(frame);
                break;
            }
        }
        let frame = on_glass_at.expect("the resident fades in beside the first caret");
        assert!(
            f64::from(frame) * 33.0 <= 400.0,
            "…within its 0.30 s fade-in"
        );
        assert!(input.free_atlas.is_some(), "the atlas rides with the body");
        assert!(p.cursor_pet_alpha() > 0 && p.companion_hit_rect().is_some());
        assert!(
            p.is_active() && p.companion.needs_frames(),
            "a fading-in cat claims the frame lane"
        );
        assert!(matches!(p.wake(), Wake::Frames));
        let x_before = pet_body_sprite(&input.free_sprites, CELL_H)
            .expect("on glass")
            .x;

        type_line(&mut p, &mut term, &mut input, b"itty walks with me", 33.0);
        idle(&mut p, &mut term, &mut input, 45, 33.0);
        let x_after = pet_body_sprite(&input.free_sprites, CELL_H)
            .expect("still on glass")
            .x;
        assert!(
            x_after > x_before,
            "the pet followed the caret to the right"
        );

        // 35 s of quiet: SLEEP_AFTER + BREATH_WINDOW have passed. Asleep on
        // glass, and the lane is released.
        let fp = idle(&mut p, &mut term, &mut input, 350, 100.0);
        assert!(
            !p.companion.needs_frames(),
            "the resident released the lane"
        );
        assert!(!p.is_active(), "…and nothing else holds it");
        assert_eq!(p.next_deadline_ms(), None);
        assert!(matches!(p.wake(), Wake::Idle));
        let body =
            pet_body_sprite(&input.free_sprites, CELL_H).expect("a sleeping cat is still drawn");
        assert_eq!(body.alpha, 255, "opaque, at rest");
        assert_eq!(p.cursor_pet_alpha(), 255);
        let sprites = input.free_sprites.clone();
        assert_ne!(fp, 0, "a drawn resident folds a non-zero key");
        for _ in 0..3 {
            assert_eq!(
                p.apply(&mut term, &mut input, 10, 19),
                fp,
                "no advance ⇒ the same fold"
            );
            assert_eq!(input.free_sprites, sprites, "byte-stable sprites");
        }
    }

    /// Ownership is a level: once the raw style stops naming the pet (`lumen`,
    /// `rainbow kitty flying`) the resident is retired on the next frame (no
    /// body, rect, fade or owed frames); the opt-in survives and naming it
    /// again restores the seed's coat. The trail master is an owner term too.
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
                pet_body_sprite(&input.free_sprites, CELL_H).is_none(),
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
            None,
            None,
            400,
            24,
            0.9,
            0.8,
            true,
            0x0050_FA7B,
        );
        idle(&mut p, &mut term, &mut input, 2, 33.0);
        assert!(input.free_sprites.is_empty() && input.free_atlas.is_none());
        assert!(!p.companion.brain().is_active() && !p.companion.needs_frames());
        assert!(
            !p.is_active() && !p.enabled_any(),
            "the trail master off is all-off again"
        );
    }

    /// Hiding is not retiring: history takes the pet off glass while its brain
    /// fades and releases the lane (its first frame is
    /// `pipeline_history_uses_the_exact_frame_snapshot_and_retires_cursor_state`);
    /// an unfocused surface retires the body; hidden is the hard coordinate
    /// boundary. After each the resident returns in the same coat.
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

        // History.
        term.process(&b"history\r\n".repeat(20));
        term.scroll_display(1);
        term.cell_frame_into(&mut input, 8, 40);
        assert!(input.display_offset > 0, "fixture: scrolled into history");
        let mut released = false;
        for _ in 0..40 {
            idle(&mut p, &mut term, &mut input, 1, 100.0);
            assert!(input.free_sprites.is_empty());
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

        // Unfocused: the surface retires the body at once (no fade).
        p.set_focused(false);
        idle(&mut p, &mut term, &mut input, 1, 33.0);
        assert_eq!(p.cursor_pet_alpha(), 0);
        assert!(input.free_sprites.is_empty());
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

        // Hidden: the hard coordinate boundary, without an intervening apply.
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

    /// One cat per caret: the word engine's tick receives the pet's live body
    /// and caret (`CompanionOnGlass::at_body`), so the feline word the pet
    /// answers grows no ambient twin. Control: with the pet off the word-cat
    /// grows inside the peek window.
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
                let body = pet_body_sprite(&input.free_sprites, CELL_H);
                saw_pet |= body.is_some();
                for s in &input.free_sprites {
                    // An animal: at least a cell tall (motes are smaller)
                    // and not the pet's own tile.
                    if s.ah < CELL_H || (s.aw, s.ah) == nat {
                        continue;
                    }
                    saw_other_animal = true;
                    if let Some(b) = body {
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

    /// A page's `set_cursor_glow` reads the native vocabulary: an alias of
    /// the pet draws the rainbow kitty with its pet, the owner's `nyan
    /// rainbow` the flying head, the old `classic mono` spelling Classic's
    /// mono face, and `off` draws nothing. Controls: `lumen` and an unknown
    /// word stay lit (an unknown word is the page's documented Lumen
    /// fallback).
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

    /// The pointer and stroke only latch: the pointer is value-shadowed (still
    /// reports no change, poison dropped, leaving is one change); a press
    /// outside the body passes, one inside is consumed and queues a stroke
    /// that re-arms the lane; nothing on glass moves until the tick.
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
        let sprites = input.free_sprites.clone();
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
        assert_eq!(input.free_sprites, sprites);
        // Only ticks consume it — a sleeping cat stretches awake first, then
        // takes the stroke on the ground (the brain's own 6 s bound).
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

    /// A typed kitty command only latches, on the pipeline's clock: the note
    /// moves the latch and asks for frames, nothing moves until the ticks that
    /// perform it, a revoke takes a tentative one back, and the pet-only
    /// submit reaches the brain's exit-127 forgiveness.
    #[test]
    fn a_typed_trick_latches_through_the_pipeline_and_needs_ticks() {
        use aterm_lexicon::Trick;

        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(8, 40);
        materialize_pet(&mut p, &mut term, &mut input);
        idle(&mut p, &mut term, &mut input, 60, 50.0);
        assert_eq!(p.companion.brain().pending_trick(), None);

        // Tentative, then revoked: the latch moves and is taken back whole.
        p.note_trick(Trick::Roll, false);
        assert_eq!(p.companion.brain().pending_trick(), Some(Trick::Roll));
        assert!(
            p.companion.needs_frames() && p.is_active(),
            "the latch asks for the frames that perform it"
        );
        p.revoke_trick();
        assert_eq!(p.companion.brain().pending_trick(), None);
        p.revoke_trick(); // idempotent: the listener revokes with nothing pending

        // Confirmed: nothing moves without a tick, then the cat rolls over.
        let sprites = input.free_sprites.clone();
        p.note_trick(Trick::Roll, true);
        assert_eq!(p.companion.brain().pending_trick(), Some(Trick::Roll));
        assert!(matches!(p.wake(), Wake::Frames));
        assert_eq!(input.free_sprites, sprites, "a note draws nothing");
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

        // The pet-only submit, against the grief gate's own round trip: the
        // `command not found` that follows a line of pet talk is forgiven,
        // and the identical failure without the stamp is grieved.
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

    /// The pointer under host rows: a host that prepends rows above the grid
    /// after `apply` draws the pet that much lower and hands pointer and
    /// presses in frame px. With `set_host_rows_px`, a stroke across the drawn
    /// body reaches the brain, a press on it pets, and the hit rect answers in
    /// the same space; the emission itself does not move.
    #[test]
    fn pointer_under_host_rows_is_read_where_the_frame_draws_the_pet() {
        use crate::kitty_pet::PetAction;
        use aterm_core::render::HostRowPixels;
        use aterm_core::terminal::RenderCell;

        let mut p = pet_pipeline(7);
        let (mut term, mut input) = glass(8, 40);
        materialize_pet(&mut p, &mut term, &mut input);
        idle(&mut p, &mut term, &mut input, 60, 50.0);
        let emitted = pet_body_sprite(&input.free_sprites, CELL_H).expect("resident on glass");
        p.set_host_rows_px(CELL_H);
        idle(&mut p, &mut term, &mut input, 1, 50.0);
        assert_eq!(
            pet_body_sprite(&input.free_sprites, CELL_H).map(|s| s.y),
            Some(emitted.y),
            "the emission does not move: the compose moves it"
        );
        // The host's compose: one band row prepended, the sprites translated.
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
        let body = pet_body_sprite(&drawn.free_sprites, CELL_H).expect("the body is drawn");
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
        // Presses last: a stroke queued by a press would answer for the
        // pointer above.
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

    /// Reduced motion is a station pin: a program-owned caret jump moves the drawn pet to its
    /// new station in one frame (no walk or arc), where full motion walks over many. The
    /// per-effect setters alias the one preference.
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
            let before = pet_body_sprite(&input.free_sprites, CELL_H).expect("drawn");
            term.process(b"\x1b[10;40H");
            idle(&mut p, &mut term, &mut input, 1, 33.0);
            let first = pet_body_sprite(&input.free_sprites, CELL_H).expect("drawn");
            idle(&mut p, &mut term, &mut input, 60, 33.0);
            let later = pet_body_sprite(&input.free_sprites, CELL_H).expect("drawn");
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

    /// The pet's cadence in the scheduler: moving (`needs_frames`), no exact deadline even with
    /// rain's 12 Hz wake armed; settled, its next visible step folds into the exact wake beside
    /// rain's; in deep sleep it forces nothing.
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
        // Just arrived: the body is still coming in — motion.
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

        // Let every other frame-cadence lane settle — the wake, the comet,
        // the word engine — and the resident come to rest on its seat.
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

    /// The torn-space rule against an independent oracle: a changed cell grid, cell metric or
    /// chrome origin leaves a cursor family with no geometry of the old space (bodies fresh,
    /// aurora curtained, momentum and cadence kept). The oracle is a struct update over
    /// `CursorFx::default()`, not `retire_body_geometry`, so a forgotten body draws a different
    /// frame. Every style runs; negative control: with the bodies' geometry kept, the droplet
    /// and comet nucleus frames differ.
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
                None,
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
                // Rainbow Kitty's band leaves through its 0.24 s curtain,
                // which the fresh family draws too; every other aurora resets.
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

            // Move the cursor to the bottom inside this parser batch, scroll
            // twice, then leave it where the translated anchor lands. The
            // signal must classify an exact translation, and the resident comet
            // must move with the text: earned light leaves by decay,
            // `note_scroll` translation or reset, never by someone else's
            // output.
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
        // A margined (DECLRMM) scroll up moves a rectangle, not a row band, so
        // the grid poisons its band record and this stays the ambiguous-motion
        // case (`RegionInvalidation`). A plain interior region scroll is an
        // explained band: see
        // `a_band_batch_translates_the_pipeline_engines_instead_of_resetting_them`.
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
                // Mode entry is a coordinate-space transition of its own. The
                // case under test is a scroll wholly inside the alt screen.
                term.process(b"\x1b[?1049h");
            }
            let mut pipeline = EffectsPipeline::new();
            if enter_alt {
                pipeline.set_cursor_glow(
                    true,
                    "lumen",
                    None,
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
                // The exact alt translation keeps the engines' alt context, so a
                // following licensed typed jump keeps the TUI classification
                // instead of being misread as a main-screen re-anchor.
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

    /// An inline viewport (e.g. Codex) streams a transcript line by scrolling a DECSTBM region:
    /// its whole row motion is one exact [`RowBandMove`](aterm_core::terminal::RowBandMove), so
    /// the decision is `Bands` and the licensed comet moves with its text. Vacuity control: the
    /// margined sibling, `pipeline_real_alt_uniform_translates_while_ambiguous_motion_invalidates`.
    #[test]
    fn a_band_batch_translates_the_pipeline_engines_instead_of_resetting_them() {
        let mut term = Terminal::new(5, 16);
        let mut pipeline = EffectsPipeline::new();
        let (mut input, before_trail) = seed_pipeline_trail(&mut pipeline, &mut term, 3);
        assert!(!before_trail.is_empty(), "fixture must own live geometry");
        let before = term.content_scroll_state();

        // Rows 1..=3 (0-based) slide up by one: a line feed on the bottom row of
        // an interior region, the shape a transcript line takes once the inline
        // viewport is pinned to the screen bottom.
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
            None,
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
            None,
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
            Some(0x0001_0203),
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
            None,
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
            input.cursor_trail_color, 0x00AA_BBCC,
            "an independently enabled automatic opaque trail still follows OSC 12"
        );
    }

    /// The web host's half of the one colour law: an automatic comet keeps
    /// its glacial hue over the theme's seed (which the host stamps as the
    /// frame's cursor colour), wears a program's OSC 12, and returns to the
    /// glacial hue on OSC 112. The lumen rows above are the negative control:
    /// a style with no hue of its own follows the seed too.
    #[test]
    fn an_automatic_comet_keeps_its_hue_until_osc12_pins_the_cursor() {
        let theme_cursor = 0x0011_2233;
        let mut pipeline = EffectsPipeline::new();
        pipeline.set_cursor_glow(
            true,
            "comet",
            None,
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

    /// Keystrokes batched between `advance` calls do not collapse to a zero
    /// inter-key gap: `advance` spreads them across its `dt`, so a batching web
    /// host ignites the cadence like a host that saw real arrival times.
    #[test]
    fn batched_keystrokes_spread_across_the_advance_delta() {
        // 6 keys physically spread over 360ms (60ms human cadence), delivered
        // as one batch: the spread reconstruction must match a hand-paced
        // cadence fed the true arrival times.
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

        // A zero-gap collapse (all 6 at one instant) over-ignites; the spread stays strictly
        // cooler than it.
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

    /// 0/0 chrome yields the identity geometry (origin 0, win == grid
    /// extents); padded chrome offsets the grid interior and grows the frame
    /// per the `[head][pad][grid][pad]` layout.
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

        // Back to identity: 0/0 restores the chrome-free geometry exactly.
        p.set_chrome(0, 0);
        let g = p.chrome_geom(24, 80, 9, 18);
        assert_eq!((g.origin_x, g.origin_y, g.head), (0, 0, 0));
        assert_eq!((g.win_w, g.win_h), (80 * 9, 24 * 18));
    }

    /// A fire-style glow burning on the top row under chrome lands per-pixel
    /// fire patches in the RenderInput, with the flame field rising above the
    /// grid into the head band (window-absolute coords from the tick's
    /// `chrome_geom`).
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
            None,
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
        // Type on row 0 (the burn ignites on observed cursor motion), then let
        // flames rise toward the chrome-relaxed clamp (`fx_top = pad`).
        for _ in 0..3 {
            for ch in [b"a", b"b", b"c", b"d"] {
                commit_ascii(&mut p, &mut term, &mut input, ch, 30.0);
                any_patch |= !input.fire_patch.is_empty();
                band_patch |= input.fire_patch.iter().any(|q| q.y < origin_y);
                // The splice carries the burn's companion streams too (frames
                // hot enough to engulf ink carry under-glow/charred/halo/rim).
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

    /// An all-off `apply()` clears every fire/halo stream a reused scratch may
    /// carry, and an enabled non-fire style never emits them.
    #[test]
    fn effects_off_and_non_fire_styles_leave_fire_streams_empty() {
        let mut p = EffectsPipeline::new();
        let mut term = Terminal::new(6, 20);
        let mut input = term.cell_frame(6, 20);
        // Simulate a reused scratch carrying stale fire-frame state.
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

        // An enabled non-fire style ticks the glow but emits no fire streams.
        p.set_cursor_glow(
            true,
            "lumen",
            None,
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

    /// The focus gate: the engine expects unfocus as amplitude 0, so an
    /// unfocused but visible split pane must not animate at full strength. The
    /// focused half runs first so the unfocused assertion is not vacuous. The
    /// gate scales the stored intensity in place and restores it before
    /// `apply` returns, so refocus comes back at full strength: only
    /// `set_cursor_glow` may change the user's value.
    #[test]
    fn unfocused_pane_emits_no_glow_but_focused_one_does() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "lumen",
            None,
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

        // Unfocused, still ticking, still moving: the gate must zero all of it.
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

    /// A dark pane must settle, or the host's shared rAF keeps re-booking it.
    /// Non-fire style (fire keeps a bounded ember tail). Two gated ticks: the
    /// first arms the lazy-cooling clock, the second cools through it.
    #[test]
    fn unfocused_glow_settles_so_the_host_stops_rebooking_the_pane() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "lumen",
            None,
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
            None,
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

        // No hidden-frame apply occurred. Refocus alone must not resurrect
        // resident geometry or a spent license; the first sample only seeds
        // the new coordinate owner.
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

    /// A page in `style`, typed into fast (ten witnessed keys) with the
    /// page's reduce-motion preference as given, and whether any frame lit
    /// the cursor family (a caret owner, a glow quad, a caret-shape override
    /// or a comet cell).
    fn typed_page(style: &str, reduced: bool) -> (EffectsPipeline, bool) {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            style,
            None,
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

    /// The page's reduce-motion preference turns the whole cursor family off,
    /// as Reduce Motion does in a window (amplitude 0, no comet): no glow, no
    /// caret-owning body, no momentum halo, no caret-shape override. Each
    /// style's unreduced run is the control.
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

    /// A page keeps its wake through the momentum glow's cool-down. After a
    /// typed burst under `classic` (no body of its own, so the caret's tint
    /// and halo are the momentum glow's), the page is driven as an embedder's
    /// loop drives it — a rAF frame on `Frames`, a timer to the instant on
    /// `At` — and goes `Idle` only after a frame with no momentum caret and a
    /// cold glow. While it settles the page stays `is_active`: the bindings'
    /// frame gate (`FrameGateKey::effects_active`) skips an inactive page's
    /// render, so a timer wake would never apply the step it was armed for.
    /// Negative control: on the way down the frame-cadence set
    /// (`bodies_active`) lets go while the glow is still hot, so only the
    /// settle arm held the wake there.
    #[test]
    fn a_page_keeps_its_wake_until_the_momentum_glow_is_cold() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "classic",
            None,
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

    /// Hiding the page retires the block bodies, momentum and cadence with the
    /// aurora before any `apply`, so `is_active()` does not ask for frames a
    /// hidden page cannot show. The typed page before the edge is the control.
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

    /// Refocus fires no comet across the ground the cursor covered while dark: the gated
    /// branches keep `last` current (a `reset()` would null it and create the phantom).
    #[test]
    fn refocus_resumes_without_a_phantom_comet() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_trail(true, 260, 24, None, 0x0050_FA7B);
        let mut term = Terminal::new(6, 20);
        term.process(b"ab");
        p.advance(30.0);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);

        // Travel a long way while dark.
        p.set_focused(false);
        for ch in [b"c", b"d", b"e", b"f", b"g", b"h"] {
            term.process(ch);
            p.advance(30.0);
            let mut input = term.cell_frame(6, 20);
            p.apply(&mut term, &mut input, 10, 19);
        }

        // Refocus without moving: a tracked `last` means nothing to sweep.
        p.set_focused(true);
        p.advance(30.0);
        let mut input = term.cell_frame(6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        assert!(
            input.cursor_trail.is_empty(),
            "refocus at a stationary cursor sweeps no cells"
        );
    }

    /// The embedded host mirrors native input provenance: an rAF batch of typed keys licenses
    /// its coalesced sweep, a relocation caused only by program output stays dark, and a key
    /// pressed while output moves the caret is licensed
    /// (`docs/design/EFFECTS-LICENSE-REDESIGN.md`).
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

    /// The trail's gated branch is a hard clear, but its colour is config and keeps being
    /// published on unfocused frames (exactly: a cold cadence leaves it untouched).
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

        // One short dark frame, well inside the 260 ms spark life. Fresh input
        // each frame, so a skipped colour publish leaves the default rather
        // than a stale value.
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

        // Once the cadence goes cold `ignite` leaves the colour untouched, so
        // the published value is exactly the configured one.
        p.advance(1000.0);
        term.cell_frame_into(&mut input, 6, 20);
        p.apply(&mut term, &mut input, 10, 19);
        assert_eq!(
            input.cursor_trail_color, 0x0050_FA7B,
            "the colour is keyed on the stored flag, not the focus-gated local"
        );
    }

    /// Word decorations are not focus-gated (entrances latch at birth, so a
    /// refocus replays nothing): focused and unfocused pipelines driven in
    /// lockstep emit identical decoration frames.
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
                    input.free_sprites.len(),
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

    /// The page floors its caret against the cell the caret sits on, read from
    /// the frame it renders, as the native host does: Fire's ember on a
    /// reverse-video cell (SGR 7, so the cell's ground is the default ink)
    /// keeps 4.5:1 against that cell. Negative control: the same caret on a
    /// plain cell of the same row is floored against the page and reads under
    /// that floor on the reverse cell, so the cell is what moved it.
    #[test]
    fn the_page_floors_its_caret_against_the_cell_under_it() {
        let fill_on = |reverse: bool| {
            let mut p = EffectsPipeline::new();
            p.set_cursor_glow(
                true,
                "fire",
                None,
                None,
                400,
                24,
                1.0,
                0.9,
                true,
                0x0050_FA7B,
            );
            let mut term = Terminal::new(6, 20);
            // A reverse-video `x` at column 2, the caret parked on it.
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

    /// The embedded hosts use this pipeline as their sole overlay producer, so
    /// Fire's warm-metal cursor body must ride the same RenderInput field as it
    /// does in the native GUI. A live style toggle must scrub it immediately
    /// (the all-off frame's scrub is
    /// `effects_off_and_non_fire_styles_leave_fire_streams_empty`).
    #[test]
    fn fire_forge_cursor_fill_is_spliced_and_cleared_on_toggle() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_glow(
            true,
            "fire",
            None,
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
        // The page the step floored against: this frame's default
        // background, or the cold theme's when the frame names none.
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
            None,
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

    /// The page runs the window's word pass: a peeking cat's palette is sampled
    /// over the cells its body covers, not its word's row alone, so a light
    /// row above a dark one gets the mixed (band 4) treatment.
    #[test]
    fn the_page_samples_a_peeking_cats_palette_over_its_footprint() {
        let mut p = EffectsPipeline::new();
        p.set_sparkle_enabled(true);
        let mut term = Terminal::new(4, 20);
        term.process(b"\r\n\r\nkitty");
        let mut input = RenderInput::default();
        term.cell_frame_into(&mut input, 4, 20);
        // Trailing blanks are trimmed from a row; the footprint reads cells.
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

    /// The page's scan knows the caret: a collision form the caret still
    /// touches is a word being typed, provisional until a delimiter lands.
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

    /// `set_sparkle_classes` sets the per-class gates; `set_sparkle_custom_specs`
    /// registers overrides and keeps the emphasis class scanning with ink off;
    /// the custom rainbow word inks end-to-end through `apply`.
    #[test]
    fn v3_web_resolver_pins_classes_and_custom_specs() {
        let mut p = EffectsPipeline::new();
        p.set_sparkle_enabled(true);
        p.set_sparkle_classes(true, true, true);
        let cfg = &p.sparkle.as_ref().expect("on").cfg;
        assert!(cfg.profanity && cfg.feline && cfg.emphasis);
        // Ink off kills emphasis only while no custom specs exist.
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
        // End-to-end: ink back on, the custom rainbow word inks via apply.
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
        // Malformed fragment: the previous specs are kept and the parse error
        // surfaces on the lexicon-warnings channel.
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
        // An explicit `None` clears the specs and the stale diagnostic.
        p.set_sparkle_custom_specs(None);
        assert!(!p.sparkle.as_ref().unwrap().cfg.spec_table.has_custom());
        assert!(
            p.sparkle_lexicon_warnings().is_empty(),
            "a clear drops the malformed-fragment warning"
        );
    }

    /// `rebuild_sparkle` surfaces `Lexicon::conflicts()` via
    /// `sparkle_lexicon_warnings()`; the single-char-CJK warning is filtered
    /// once the resolved config enables `cjk_single_char`, while unrelated
    /// warnings survive.
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
        // A single-char CJK custom word (scans only under the opt-in) plus a
        // mixed-script one (dropped at insert, can never scan).
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
        // Enable the opt-in (a cfg refresh, not a rebuild): the requires-warning
        // disappears; the unscannable mixed-script warning stays.
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
        // Flipping the opt-in back off re-applies it (filtered at read, so a
        // knob change never needs a rebuild to stay honest).
        p.set_sparkle_feline("cat", true, true, false);
        assert!(
            p.sparkle_lexicon_warnings()
                .iter()
                .any(|w| w.contains('犬'))
        );
        // Master off drops the diagnostics with the lexicon they describe.
        p.set_sparkle_enabled(false);
        assert!(p.sparkle_lexicon_warnings().is_empty());
    }

    /// Reset-table parity with the native config reload: the web knob setters
    /// hard-reset, so a finished one-shot replays after a knob change; a
    /// suppressed-alt-screen round trip is freeze/thaw (episodes resume, done
    /// marks untouched).
    #[test]
    fn v3_reset_table_knob_setters_hard_reset_and_alt_suppression_freezes() {
        let mut p = EffectsPipeline::new();
        p.set_sparkle_enabled(true);
        // Configured up front: a knob setter is itself a hard_reset, so the
        // freeze/thaw stanza below must not flip knobs mid-episode.
        p.set_sparkle_alt_screen_suppression(true);
        p.set_focused(true);
        let mut term = Terminal::new(4, 24);
        term.process(b"\r\n\r\na nice kitty");
        let mut input = RenderInput::default();

        // Drive the one-shot to Done: the peek worst case is < 5 s.
        p.advance(10.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20);
        let mut saw_cat = false;
        for _ in 0..60 {
            p.advance(100.0);
            term.cell_frame_into(&mut input, 4, 24);
            p.apply(&mut term, &mut input, 10, 20);
            saw_cat |= !input.free_sprites.is_empty();
        }
        assert!(saw_cat, "the peek actually played");
        assert!(
            input.free_sprites.is_empty(),
            "the one-shot is spent (zero sprites at rest)"
        );

        // Knob setter = hard_reset: the same word plays again.
        p.set_sparkle_feline("cat", true, true, false);
        p.advance(10.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20); // release present (reveal 0)
        let mut replayed = false;
        for _ in 0..10 {
            p.advance(100.0);
            term.cell_frame_into(&mut input, 4, 24);
            p.apply(&mut term, &mut input, 10, 20);
            replayed |= !input.free_sprites.is_empty();
        }
        assert!(
            replayed,
            "web knob setters are hard_reset(): the one-shot replays (§1.1 table)"
        );

        // Suppressed alt screen = freeze/thaw: the mid-peek cat resumes (it neither restarts
        // nor silently completes while suppressed).
        let sprites_before = input.free_sprites.clone();
        assert!(
            !sprites_before.is_empty(),
            "mid-peek before the alt round-trip"
        );
        term.process(b"\x1b[?1049h"); // enter the alternate screen
        p.advance(100.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20);
        assert!(input.free_sprites.is_empty(), "suppressed: no decorations");
        term.process(b"\x1b[?1049l"); // long suspension, then back
        p.advance(5000.0);
        term.cell_frame_into(&mut input, 4, 24);
        p.apply(&mut term, &mut input, 10, 20);
        // A mass replay would show as a different reveal (a restarted entrance
        // re-crops the source and re-grows the dest height; a silently
        // completed peek emits nothing), so the check is over the reveal.
        //
        // The dest origin gets a small tolerance: a settled word-cat breathes
        // (`CatIdlePose`, a ±2 px translation on the dwell clock).
        assert_eq!(
            input.free_sprites.len(),
            sprites_before.len(),
            "thaw resumes the peek (no mass replay, and it did not silently end)"
        );
        for (after, before) in input.free_sprites.iter().zip(sprites_before.iter()) {
            assert_eq!(
                (after.w, after.h, after.ax, after.ay, after.aw, after.ah),
                (
                    before.w, before.h, before.ax, before.ay, before.aw, before.ah
                ),
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

    /// A 24×80 snapshot of `term` whose cells are all materialized default-bg
    /// spaces (every cell rain-eligible). A fresh terminal's rows are
    /// unmaterialized and scan as ineligible, so the real stamp runs first
    /// (geometry, cursor, selection, `snapshot_seq`) and only the cells are
    /// synthesized.
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
        // These lifecycle tests intentionally exercise the opt-in decorative
        // ROM on an empty grid; literal mode correctly needs sampled output.
        pipeline.rain_cfg.output_material = false;
        pipeline.set_matrix_rain_enabled(true);
    }

    /// Drive an enabled-rain pipeline on the all-eligible grid until quads land
    /// in `input` (calm ticks at 12 Hz; per-frame keystrokes hold calm against
    /// the idle-to-sleep mandate).
    fn rain_until_visible(p: &mut EffectsPipeline, term: &mut Terminal, input: &mut RenderInput) {
        for _ in 0..120 {
            p.note_keystroke();
            p.advance(50.0);
            synthetic_empty_frame(term, input);
            p.apply(term, input, 10, 20);
            if !input.rain_quads.is_empty() {
                return;
            }
        }
        panic!("rain never appeared on an empty 24x80 grid");
    }

    /// After the last comet leaves, an open streak episode does not pin rAF:
    /// the pipeline exposes the analytic low-crossing as one exact wake whose
    /// tick emits one Settle and returns the scheduler to Idle.
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

        // `MAX_DT_S` is 250 ms; four bounded ticks retire even the 900 ms
        // comet while momentum is still above EPISODE_LOW.
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

    /// The page's streak reaches the glass: with the streak enabled, streamed
    /// output lines put light in the shared `nova_add` channel through
    /// `apply`.
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

        // The page-wide preference retires live comets and forbids new ones,
        // without overwriting the configured amplitude needed when it clears.
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

    /// Enable + advance + apply rains on an empty grid, the atlas rides only
    /// alongside output, and a repeat `apply` with no `advance` is byte-stable
    /// (same quads, same fold).
    #[test]
    fn rain_enabled_emits_and_repeat_apply_is_byte_stable() {
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        assert!(p.matrix_rain_enabled());
        assert!(p.is_active(), "CALM weather holds the engine active");
        let mut term = Terminal::new(24, 80);
        let mut input = RenderInput::default();
        rain_until_visible(&mut p, &mut term, &mut input);
        assert!(input.rain_atlas.is_some(), "the atlas rides with the quads");
        let quads = input.rain_quads.clone();
        let adds = input.rain_add.clone();
        let atlas_version = input.rain_atlas.as_ref().map(|a| a.version);
        let fp1 = p.apply(&mut term, &mut input, 10, 20);
        let fp2 = p.apply(&mut term, &mut input, 10, 20);
        assert_ne!(fp1, 0, "nonempty emission folds a nonzero fp");
        assert_eq!(fp1, fp2, "no advance ⇒ stable fold");
        assert_eq!(input.rain_quads, quads, "byte-stable quads");
        assert_eq!(input.rain_add, adds, "byte-stable halos");
        assert_eq!(
            input.rain_atlas.as_ref().map(|a| a.version),
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

    /// The pipeline keeps its prior real-output tape throughout an arbitrarily
    /// tall unsent edit: seven visible draft rows never enter material, even
    /// those far from the cursor. Submit releases the gate and a later real
    /// response refreshes the tape.
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

    /// Every off path scrubs stale rain from a reused snapshot: the
    /// `!enabled_any` return, and the rain-absent arm while another effect
    /// keeps the enabled path alive.
    #[test]
    fn rain_toggle_off_clears_stale_channels_on_every_off_path() {
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        let mut term = Terminal::new(24, 80);
        let mut input = RenderInput::default();
        rain_until_visible(&mut p, &mut term, &mut input);

        // Master off with nothing else enabled: the early return scrubs.
        p.set_matrix_rain_enabled(false);
        assert!(!p.is_active(), "dropping the engine disarms the host loop");
        let fp = p.apply(&mut term, &mut input, 10, 20);
        assert_eq!(fp, 0);
        assert!(input.rain_quads.is_empty());
        assert!(input.rain_add.is_empty());
        assert!(input.rain_atlas.is_none());

        // Rain off while the glow keeps the enabled path alive: the
        // rain-absent arm scrubs the reused snapshot the same way.
        p.set_matrix_rain_enabled(true);
        rain_until_visible(&mut p, &mut term, &mut input);
        p.set_matrix_rain_enabled(false);
        p.set_cursor_glow(
            true,
            "lumen",
            None,
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
        assert!(input.rain_quads.is_empty());
        assert!(input.rain_add.is_empty());
        assert!(input.rain_atlas.is_none());
    }

    /// A torn snapshot (`snapshot_seq` trails the damage epoch) emits no rain
    /// and scrubs the channels; the next fresh snapshot resumes emission.
    #[test]
    fn rain_torn_snapshot_clears_channels_and_fresh_frame_resumes() {
        let mut p = EffectsPipeline::new();
        enable_classic_rain(&mut p);
        let mut term = Terminal::new(24, 80);
        let mut input = RenderInput::default();
        rain_until_visible(&mut p, &mut term, &mut input);

        term.process(b"tear"); // grid damage after the snapshot: `input` is stale
        p.apply(&mut term, &mut input, 10, 20);
        assert!(input.rain_quads.is_empty(), "torn frame: no rain quads");
        assert!(input.rain_add.is_empty(), "torn frame: no rain halos");
        assert!(input.rain_atlas.is_none(), "torn frame: no atlas rides");

        // A fresh snapshot re-anchors (rescan re-fires on the new epoch).
        rain_until_visible(&mut p, &mut term, &mut input);
    }

    #[test]
    fn apply_routes_v2_offer_and_drains_companion_impulse_to_resident_pet() {
        let mut p = EffectsPipeline::new();
        p.set_cursor_pet(true, 0x5EED);
        p.set_cursor_glow(
            true,
            "rainbow kitty",
            None,
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

        // Licensed same-row jump >= JUMP_MIN_CELLS mints a v2 meteor on `apply`.
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
