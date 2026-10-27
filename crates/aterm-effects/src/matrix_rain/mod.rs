// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Matrix digital-rain engine.

pub mod bake;
mod bitmap_font;
pub mod field;
mod layers;
pub mod rom;
pub mod step;
pub use step::{RainClock, RainFrame, RainLatches, RainScan};

use std::sync::Arc;

use aterm_time::Instant;

use aterm_core::grid::LineSize;
use aterm_core::grid::extra::ImageRef;
use aterm_core::render::FxArt;
use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_hash::{FNV64_OFFSET, fnv_mix_u64};
use aterm_render::{RainHalo, premul_rgb};

use crate::color_math::{hsv2rgb, relative_luminance, rgb2hsv};
use crate::genome::mix;
pub use crate::word_decorations::{EffectGeom, SelView};

pub use bake::{MAX_RAIN_BAKES_PER_TICK, RainBaker};
use field::{
    ColParams, DensityRing, FieldParams, col_params, cycle_active, cycle_at, cycle_start_tick,
    dither, dither_epoch, glyph_epoch, glyph_from_epoch, trail_level,
};
pub use field::{MAX_RAIN_ADD, MAX_RAIN_QUADS, MAX_RAIN_TEXELS, quad_cap};
pub use layers::{RainCell, RainLayers, is_rain_row};
use rom::{RomMaster, decorative_master};

/// Readability ceiling on any rain coverage (body, head, halo) — the
/// `cursor_trail::READABLE_ALPHA_CAP` precedent, pinned by test.
pub const RAIN_ALPHA_CAP: u8 = 135;
/// Floor on the derived/overridden body alpha (below this rain is invisible noise that still costs
/// quads).
pub const RAIN_ALPHA_FLOOR: u8 = 16;
/// Hidden-cursor mask band: the host feeds the last K recently-damaged rows (Claude Code's inline
/// input box lives there).
pub const HIDDEN_CURSOR_BAND_ROWS: usize = 5;
/// Visible-cursor mask band half-height: cursor row ± this many rows.
pub const CURSOR_BAND_ROWS: i32 = 2;

const MATERIAL_CAP: usize = 128;
const MATERIAL_GLYPH_CAP: usize = rom::ROM_GLYPHS;
const MAX_FRAME_CANDIDATES: usize = 64;
const MAX_FRAME_REGIONS: usize = 64;
const SEMANTIC_ENERGY_CAP: u8 = 8;
const SEMANTIC_HOLD_TICKS: u8 = 24;

const CALM_TICK_MS: u32 = 83;
const DWELL_MS: u64 = 1000;
const WORKING_HOLD_MS: u64 = 2000;
const STREAK_WINDOW_MS: u64 = 1000;
const ECHO_DISCOUNT_MS: u64 = 250;
const CONTENT_CREDIT_CAP: u8 = 32;
const DRAIN_TICKS: u32 = 30;
const ALT_SCROLL_QUIET_MS: u64 = 3000;
const BELL_ALERT_MS: u64 = 2000;
const WAVE_MS: u64 = 700;
const WAVE_IGNITIONS_PER_SEC: usize = 2;
const CATCHUP_MS: u64 = 60_000;
const HUE_SEPARATION_DEG: f32 = 18.0;
const DIFF_GREEN: u32 = 0x0021_3A2B;
const MATRIX_HUE_DEG: f32 = 122.0;
const ALERT_HUE_DEG: f32 = 42.0;
const FAIL_HUE_DEG: f32 = 8.0;
const EXIT_FAIL_MS: u64 = 2000;

/// Rain tint family (`[matrix_rain] hue`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RainHue {
    /// The stock matrix green.
    Matrix,
    /// Derive the hue from the theme.
    Theme,
    /// An explicit `0x00RRGGBB` whose hue seeds the ramp.
    Custom(u32),
}

/// Host-reported pane visibility (drives the drain-on-unfocus policy).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RainVisibility {
    /// Focused: full weather range.
    Focused,
    /// Visible but unfocused: capped at CALM and draining to empty.
    VisibleUnfocused,
    /// Hidden (occluded/minimized): drains immediately.
    Hidden,
}

/// The weather machine states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RainWeather {
    /// No emission; the field drains and the host timer disarms.
    Sleep,
    /// 12 Hz drizzle, low density — typing lives here.
    Calm,
    /// Full fps, full density — sustained agent output.
    Working,
}

/// Payload-free choreography signal from an observable agent/tool event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum RainSignal {
    #[default]
    AssistantStream = 0,
    Inspect = 1,
    Modify = 2,
    Execute = 3,
    Network = 4,
    Branch = 5,
    Waiting = 6,
    Success = 7,
    Failure = 8,
    Interrupted = 9,
    TurnStart = 10,
}

impl RainSignal {
    #[must_use]
    #[inline]
    pub const fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            0 => Self::AssistantStream,
            1 => Self::Inspect,
            2 => Self::Modify,
            3 => Self::Execute,
            4 => Self::Network,
            5 => Self::Branch,
            6 => Self::Waiting,
            7 => Self::Success,
            8 => Self::Failure,
            9 => Self::Interrupted,
            10 => Self::TurnStart,
            _ => return None,
        })
    }

    #[inline]
    const fn priority(self) -> u8 {
        match self {
            Self::Failure | Self::Interrupted => 3,
            Self::Success | Self::Waiting => 2,
            Self::Inspect | Self::Modify | Self::Execute | Self::Network | Self::Branch => 1,
            Self::AssistantStream | Self::TurnStart => 0,
        }
    }
}

/// Fully-resolved rain configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RainConfig {
    /// Master gate.
    pub enabled: bool,
    /// Engine tick rate while WORKING, `12..=60`.
    pub fps: u8,
    /// Density knob `1..=12` (scales the weather EMA target).
    pub density: u8,
    /// Fall-speed knob `1..=10` (5 = neutral; shifts the step period).
    pub speed: u8,
    /// Trail-length knob `1..=10` (5 = neutral).
    pub trail: u8,
    /// Body coverage override; `None` derives it from the luminance constraint.
    pub alpha_override: Option<u8>,
    /// Head coverage override; `None` derives.
    pub head_alpha_override: Option<u8>,
    /// Ramp tint family.
    pub hue: RainHue,
    /// Glyph mutation window in ms, `80..=2000`.
    pub mutation_ms: u16,
    /// Idle seconds until mandatory SLEEP, `2..=120`.
    pub idle_secs: u16,
    /// Suppress emission entirely while the alternate screen is active.
    pub suppress_in_alt_screen: bool,
    /// Output material: sample the rain's glyph alphabet from on-screen output into a bounded
    /// dynamic ROM while drawing only in empty cells.
    pub output_material: bool,
    /// Turn-complete wave on the WORKING→CALM edge.
    pub turn_wave: bool,
    /// Visual bell → 2 s amber ALERT hue-ramp.
    pub bell_alert: bool,
    /// Command exit status in the weather (OSC 133/633, host-fed): success fires the finishing
    /// head-sweep.
    pub exit_tint: bool,
    /// Replay seed (0 is a valid seed; demos pin it for reproducibility).
    pub seed: u64,
    /// Theme default background `0x00RRGGBB` (ramp + luminance derivation).
    pub default_bg: u32,
    /// Theme foreground `0x00RRGGBB` (the SGR-2 dim reference the luminance invariant floors
    /// against, and the `hue = "theme"` source).
    pub theme_fg: u32,
    /// Theme accent (its cursor) `0x00RRGGBB`: the `hue = "theme"` source where the foreground has
    /// no hue of its own ([`theme_hue`]).
    pub theme_accent: u32,
}

impl RainConfig {
    /// Default disabled rain configuration.
    pub const DEFAULT: Self = Self {
        enabled: false,
        fps: 30,
        density: 6,
        speed: 5,
        trail: 5,
        alpha_override: None,
        head_alpha_override: None,
        hue: RainHue::Matrix,
        mutation_ms: 133,
        idle_secs: 8,
        suppress_in_alt_screen: false,
        turn_wave: true,
        bell_alert: true,
        exit_tint: true,
        output_material: true,
        seed: 0,
        default_bg: 0x0011_1318,
        theme_fg: 0x00D0_D0D0,
        theme_accent: 0x0050_FA7B,
    };
}

impl Default for RainConfig {
    #[inline]
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The Tier-B live per-tick inputs (snapshot scalars the host read under its terminal lock; the
/// engine never re-locks).
#[derive(Clone, Copy)]
pub struct RainTickInput<'a> {
    /// Visible cursor cell `(row, col)`, `None` when hidden (DECTCEM off).
    pub cursor: Option<(u16, u16)>,
    /// When the cursor is hidden.
    pub hidden_band: &'a [u16],
    /// Live selection view (search highlight is the selection).
    pub sel: Option<SelView<'a>>,
    /// Native scrollback offset: any non-zero value suppresses emission (scrolling back always
    /// yields clean text).
    pub display_offset: i32,
    /// Whether the alternate screen is active (gates `suppress_in_alt_screen` and the scroll-quiet
    /// window).
    pub is_alt_screen: bool,
}

impl RainTickInput<'_> {
    /// Empty per-tick rain input at the live primary screen with no cursor or selection.
    pub const EMPTY: Self = Self {
        cursor: None,
        hidden_band: &[],
        sel: None,
        display_offset: 0,
        is_alt_screen: false,
    };
}

impl Default for RainTickInput<'_> {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

struct EmitCtx<'a> {
    fp: FieldParams,
    geom: EffectGeom,
    input: &'a RainTickInput<'a>,
    cap: usize,
    /// Drain level penalty `0..15` (15 ⇒ fully drained, gated off upstream).
    pen: u32,
    ramp: [u32; 16],
    wave_row: Option<u32>,
    /// Frame-invariant glyph epoch `tick / mq` (hoisted out of the per-quad
    /// `glyph_from_epoch` hash — see [`field::glyph_epoch`]).
    glyph_epoch: u32,
    /// Frame-invariant dither epoch `tick / dq` (hoisted out of the per-cell
    /// `dither` hash — see [`field::dither_epoch`]).
    dither_epoch: u32,
}

#[derive(Clone, Copy)]
struct FrameCandidate {
    top: usize,
    left: usize,
    right: usize,
    side_rows: usize,
    gap_rows: usize,
}

#[derive(Clone, Copy)]
struct FrameRegion {
    top: usize,
    bottom: usize,
    left: usize,
    right: usize,
}

/// The Unicode Box Drawing block — a sound short-circuit for every `frame_*`
/// classifier below, all of whose literals lie in U+2500..=U+257A.
#[inline]
const fn in_box_drawing_block(ch: char) -> bool {
    matches!(ch as u32, 0x2500..=0x257F)
}

#[inline]
const fn frame_top_left(ch: char) -> bool {
    matches!(ch, '┌' | '┏' | '╔' | '╒' | '╓' | '╭')
}

#[inline]
const fn frame_top_right(ch: char) -> bool {
    matches!(ch, '┐' | '┓' | '╗' | '╕' | '╖' | '╮')
}

#[inline]
const fn frame_bottom_left(ch: char) -> bool {
    matches!(ch, '└' | '┗' | '╚' | '╘' | '╙' | '╰')
}

#[inline]
const fn frame_bottom_right(ch: char) -> bool {
    matches!(ch, '┘' | '┛' | '╝' | '╛' | '╜' | '╯')
}

#[inline]
const fn frame_horizontal(ch: char) -> bool {
    matches!(
        ch,
        '─' | '━' | '═' | '┄' | '┅' | '┈' | '┉' | '╌' | '╍' | '╴' | '╶' | '╸' | '╺'
    )
}

#[inline]
const fn frame_vertical(ch: char) -> bool {
    matches!(
        ch,
        '│' | '┃'
            | '║'
            | '┆'
            | '┇'
            | '┊'
            | '┋'
            | '╎'
            | '╏'
            | '├'
            | '┝'
            | '┞'
            | '┟'
            | '┠'
            | '┡'
            | '┢'
            | '┣'
            | '┤'
            | '┥'
            | '┦'
            | '┧'
            | '┨'
            | '┩'
            | '┪'
            | '┫'
            | '╞'
            | '╟'
            | '╠'
            | '╡'
            | '╢'
            | '╣'
            | '┼'
            | '╂'
            | '╋'
            | '╪'
            | '╫'
            | '╬'
    )
}

/// The rain engine.
pub struct MatrixRain {
    cfg: RainConfig,
    seed32: u32,
    tick_ms: u32,
    mq: u32,
    dq: u32,
    sq: u32,
    ramp: [u32; 16],
    ramp_alert: [u32; 16],
    ramp_fail: [u32; 16],
    body_alpha: u8,
    head_alpha: u8,
    /// First observed instant (native hosts); web hosts never set it and
    /// drive `advance_ms` instead.
    epoch: Option<Instant>,
    /// Continuous host time in ms since the epoch.
    clock_ms: u64,
    /// Time already converted into whole engine ticks.
    consumed_ms: u64,
    /// The field tick (advances at the weather-gated engine rate).
    tick: u64,
    pending_key: bool,
    /// Coalesced distance in the grid content clock (bounded).
    pending_content_credit: u8,
    pending_bell: bool,
    pending_alt_scroll: bool,
    /// Host-noted command completion this frame: `Some(failed)`, last wins.
    pending_exit: Option<bool>,
    /// Coalesced payload-free observable-work pulse; higher-priority outcomes
    /// cannot be overwritten by a lower-priority tool ping in the same frame.
    pending_signal: Option<(RainSignal, u8)>,
    /// A submitted turn is an echo-classification boundary even when a higher-
    /// priority outcome replaces its choreography pulse before the next tick.
    pending_turn_start: bool,
    semantic_phase: RainSignal,
    semantic_energy: u8,
    semantic_ticks_left: u8,
    semantic_started_ms: u64,
    semantic_seq: u32,
    weather: RainWeather,
    weather_since_ms: u64,
    /// Density EMA in 10.6 fixed point; `ema += (target - ema)/8` per tick.
    ema_fx: i32,
    density_byte: u8,
    ring: DensityRing,
    last_seq: Option<u64>,
    last_content_ms: Option<u64>,
    content_streak: u32,
    last_key_ms: Option<u64>,
    bell_until_ms: u64,
    fail_until_ms: u64,
    quiet_until_ms: u64,
    visibility: RainVisibility,
    reduced_motion: bool,
    drain_ticks: u32,
    wave_pending: bool,
    /// Active wave: `(start tick, length in ticks)`.
    wave: Option<(u64, u32)>,
    /// Rolling-window ignition timestamps (ms); `u64::MAX` = empty slot.
    ign_ms: [u64; WAVE_IGNITIONS_PER_SEC],
    occ: Vec<u64>,
    occ_rows: usize,
    occ_cols: usize,
    have_scanned: bool,
    last_epoch: u64,
    /// Resident scratch for closed box-drawn TUI surface recognition.
    frame_candidates: Vec<FrameCandidate>,
    frame_regions: Vec<FrameRegion>,
    frame_horizontal_prefix: Vec<usize>,
    col_order: Vec<u16>,
    order_cols: u16,
    order_seed: u32,
    /// Row count the caches were built at (`ColParams` depends on rows, so a
    /// resize must invalidate the cache alongside cols/seed).
    order_rows: u16,
    /// Tick-independent per-column [`ColParams`], rebuilt on the same trigger
    /// as `col_order` (a pure-function memo of the frame's `FieldParams`).
    col_params_cache: Vec<ColParams>,
    rom: Option<RomMaster>,
    baker: RainBaker,
    last_bake_tick: Option<u64>,
    /// Dynamic-ROM tile indices for the program's literal on-screen characters
    /// at the last Tier-A rescan (≤ [`MATERIAL_CAP`], source order retained;
    /// empty ⇒ the classic pure-hash alphabet).
    material: Vec<u8>,
    /// Sorted slot-to-character table currently authored into the ROM prefix
    /// (≤ [`MATERIAL_GLYPH_CAP`]).
    material_chars: Vec<char>,
    /// Resident output-character sampling scratch (cleared + refilled per rescan).
    material_scratch: Vec<char>,
    /// Resident unique/sort scratch used to assign stable dynamic ROM slots.
    material_slots_scratch: Vec<char>,
    /// A live classic-to-literal config change needs one fresh host sample even
    /// when the terminal damage epoch itself did not change.
    material_sample_needed: bool,
    /// Shared native/embedder provenance gate: from the first reported
    /// keystroke through explicit TurnStart, occupancy may refresh but literal
    /// material retains the previous real-output tape.
    material_editing: bool,
    /// The last emit was suppressed by a reading gate (scrolled-back view, or
    /// alt-screen quiet/suppression).
    reading_gated: bool,
    halo_cands: Vec<(u16, u16)>,
    /// Stable row-sort scratch (counting sort): the emitted quads copied out…
    sort_scratch: Vec<RainCell>,
    /// …and the per-row scatter offsets (rows + 1 entries).
    row_starts: Vec<u32>,
    last_emit_nonempty: bool,
}

impl MatrixRain {
    /// Build an engine for `cfg` (clamped defensively; the host resolver owns the real clamps).
    #[inline]
    #[must_use]
    pub fn new(cfg: RainConfig) -> Self {
        let mut e = Self {
            cfg: RainConfig::default(),
            seed32: 0,
            tick_ms: 33,
            mq: 4,
            dq: 12,
            sq: 15,
            ramp: [0; 16],
            ramp_alert: [0; 16],
            ramp_fail: [0; 16],
            body_alpha: RAIN_ALPHA_FLOOR,
            head_alpha: RAIN_ALPHA_FLOOR,
            epoch: None,
            clock_ms: 0,
            consumed_ms: 0,
            tick: 0,
            pending_key: false,
            pending_content_credit: 0,
            pending_bell: false,
            pending_exit: None,
            pending_alt_scroll: false,
            pending_signal: None,
            pending_turn_start: false,
            semantic_phase: RainSignal::AssistantStream,
            semantic_energy: 0,
            semantic_ticks_left: 0,
            semantic_started_ms: 0,
            semantic_seq: 0,
            weather: RainWeather::Calm,
            weather_since_ms: 0,
            ema_fx: 0,
            density_byte: 0,
            ring: DensityRing::default(),
            last_seq: None,
            last_content_ms: None,
            content_streak: 0,
            last_key_ms: None,
            bell_until_ms: 0,
            fail_until_ms: 0,
            quiet_until_ms: 0,
            visibility: RainVisibility::Focused,
            reduced_motion: false,
            drain_ticks: 0,
            wave_pending: false,
            wave: None,
            ign_ms: [u64::MAX; WAVE_IGNITIONS_PER_SEC],
            occ: Vec::new(),
            occ_rows: 0,
            occ_cols: 0,
            have_scanned: false,
            last_epoch: 0,
            frame_candidates: Vec::new(),
            frame_regions: Vec::new(),
            frame_horizontal_prefix: Vec::new(),
            col_order: Vec::new(),
            order_cols: 0,
            order_seed: 0,
            order_rows: 0,
            col_params_cache: Vec::new(),
            rom: None,
            baker: RainBaker::default(),
            last_bake_tick: None,
            material: Vec::new(),
            material_chars: Vec::new(),
            material_scratch: Vec::new(),
            material_slots_scratch: Vec::new(),
            material_sample_needed: true,
            material_editing: false,
            halo_cands: Vec::new(),
            reading_gated: false,
            sort_scratch: Vec::new(),
            row_starts: Vec::new(),
            last_emit_nonempty: false,
        };
        e.set_config(cfg);
        e.reset();
        e
    }

    /// Swap the configuration.
    #[inline]
    pub fn set_config(&mut self, cfg: RainConfig) {
        let cfg = clamp_config(cfg);
        let material_mode_changed = self.cfg.output_material != cfg.output_material;
        self.cfg = cfg;
        self.seed32 = mix(cfg.seed) as u32;
        self.tick_ms = 1000 / u32::from(cfg.fps);
        let (mq, dq, sq) = FieldParams::quanta(self.tick_ms, u32::from(cfg.mutation_ms));
        (self.mq, self.dq, self.sq) = (mq, dq, sq);
        let hue = ramp_hue(&cfg);
        self.ramp = build_ramp(hue, &cfg);
        self.ramp_alert = luminance_match(build_ramp(ALERT_HUE_DEG, &cfg), self.ramp);
        self.ramp_fail = luminance_match(build_ramp(FAIL_HUE_DEG, &cfg), self.ramp);
        (self.body_alpha, self.head_alpha) = derive_alphas(&cfg, &self.ramp);
        self.baker.restart();
        self.last_bake_tick = None;
        self.order_seed = self.seed32;
        self.order_cols = 0;
        if !cfg.output_material {
            self.clear_material_bank();
            self.material_sample_needed = false;
        } else if material_mode_changed {
            self.material_sample_needed = true;
        }
    }

    /// Clear the dynamic field/weather state (layout transition, master toggle).
    #[inline]
    pub fn reset(&mut self) {
        self.weather = RainWeather::Calm;
        self.weather_since_ms = self.clock_ms;
        let target = i32::from(self.cfg.density) * 7;
        self.ema_fx = target << 6;
        self.density_byte = quantize_density(target);
        self.ring.clear();
        self.ring.push(self.tick, self.density_byte);
        self.last_seq = None;
        self.last_content_ms = None;
        self.content_streak = 0;
        self.last_key_ms = None;
        self.bell_until_ms = 0;
        self.fail_until_ms = 0;
        self.quiet_until_ms = 0;
        self.drain_ticks = 0;
        self.wave_pending = false;
        self.wave = None;
        self.ign_ms = [u64::MAX; WAVE_IGNITIONS_PER_SEC];
        self.pending_key = false;
        self.pending_content_credit = 0;
        self.pending_bell = false;
        self.pending_alt_scroll = false;
        self.pending_exit = None;
        self.pending_signal = None;
        self.pending_turn_start = false;
        self.semantic_phase = RainSignal::AssistantStream;
        self.semantic_energy = 0;
        self.semantic_ticks_left = 0;
        self.semantic_started_ms = self.clock_ms;
        self.semantic_seq = 0;
        self.have_scanned = false;
        self.reading_gated = false;
        self.clear_material_bank();
        self.material_sample_needed = self.cfg.output_material;
        self.halo_cands.clear();
        self.last_emit_nonempty = false;
    }

    #[inline]
    fn clear_material_bank(&mut self) {
        self.material.clear();
        self.material_scratch.clear();
        self.material_slots_scratch.clear();
        if self.material_chars.is_empty() {
            return;
        }
        self.material_chars.clear();
        self.rom = Some(decorative_master().clone());
        self.baker.restart();
        self.last_bake_tick = None;
    }

    #[inline]
    fn finish_material_bake(&mut self) {
        let Some(rom) = self.rom.as_ref() else {
            return;
        };
        while !self.baker.complete() {
            self.baker.bake_tiles(rom);
        }
        self.last_bake_tick = Some(self.tick);
    }

    /// PTY content mutation observed (`Terminal::content_seq()`): sustained deltas drive WORKING.
    #[inline]
    pub const fn note_activity(&mut self, content_seq: u64) {
        let previous = self.last_seq;
        self.last_seq = Some(content_seq);
        let Some(previous) = previous else {
            return;
        };
        if content_seq < previous {
            self.pending_content_credit = 0;
            self.content_streak = 0;
            self.last_content_ms = None;
            return;
        }
        let delta = content_seq.saturating_sub(previous);
        if delta > 0 {
            let cap = CONTENT_CREDIT_CAP as u64;
            let credit = (if delta < cap { delta } else { cap }) as u8;
            let sum = self.pending_content_credit.saturating_add(credit);
            self.pending_content_credit = if sum < CONTENT_CREDIT_CAP {
                sum
            } else {
                CONTENT_CREDIT_CAP
            };
        }
    }

    /// Whether emission is possible right now, so the host can skip the O(rows·cols) rescan/sample.
    #[must_use]
    #[inline]
    pub const fn can_emit(&self) -> bool {
        self.cfg.enabled
            && !self.reduced_motion
            && (matches!(self.visibility, RainVisibility::Focused)
                || self.drain_ticks < DRAIN_TICKS)
    }

    /// Whether literal mode needs a fresh authoritative host sample even if occupancy is already
    /// current (for example after a live mode switch).
    #[must_use]
    #[inline]
    pub const fn needs_material_sample(&self) -> bool {
        self.cfg.output_material && self.material_sample_needed
    }

    /// Restore the host's composer provenance baseline when a web pipeline recreates this lazily-owned engine after an off/on toggle.
    #[inline]
    pub(crate) const fn set_material_editing(&mut self, editing: bool) {
        self.material_editing = editing;
        self.material_sample_needed |= editing && self.cfg.output_material;
    }

    #[cfg(test)]
    pub(crate) const fn material_editing_for_test(&self) -> bool {
        self.material_editing
    }

    /// Enter a reading-only viewport without scanning its translated cells.
    #[inline]
    pub const fn defer_reading(&mut self) {
        self.have_scanned = false;
        self.reading_gated = true;
        self.last_emit_nonempty = false;
    }

    /// The host swapped which grid this engine watches (tab switch, pane focus move, session
    /// migration).
    #[inline]
    pub fn note_grid_replaced(&mut self) {
        self.have_scanned = false;
        self.clear_material_bank();
        self.material_sample_needed = self.cfg.output_material;
        self.material_editing = false;
        self.last_seq = None;
    }

    /// Whether an input note could animate this engine, so the host skips useless wake redraws.
    #[must_use]
    #[inline]
    pub const fn notes_can_wake(&self) -> bool {
        self.cfg.enabled
            && !self.reduced_motion
            && (!self.cfg.output_material || !self.material.is_empty())
    }

    /// A keystroke: keeps CALM alive; keystrokes alone never reach WORKING.
    #[inline]
    pub const fn note_keystroke(&mut self) {
        self.pending_key = true;
        self.material_editing = true;
    }

    /// Visual bell: arms the 2 s amber ALERT hue-ramp (when `bell_alert`).
    #[inline]
    pub const fn note_bell(&mut self) {
        self.pending_bell = true;
    }

    /// A command completed (OSC 133/633 exit code, host-fed): success fires the finishing
    /// head-sweep; failure holds the 2 s ember tint.
    #[inline]
    pub const fn note_exit_status(&mut self, failed: bool) {
        self.pending_exit = Some(failed);
    }

    /// Ingest one payload-free observable-work pulse.
    #[inline]
    pub const fn note_signal(&mut self, code: u32, weight: u32) {
        let Some(signal) = RainSignal::from_code(code) else {
            return;
        };
        if matches!(signal, RainSignal::TurnStart) {
            self.material_editing = false;
        }
        if !self.cfg.enabled || self.reduced_motion {
            return;
        }
        self.pending_turn_start |= matches!(signal, RainSignal::TurnStart);
        let cap = SEMANTIC_ENERGY_CAP as u32;
        let weight = if weight < 1 {
            1
        } else if weight > cap {
            SEMANTIC_ENERGY_CAP
        } else {
            weight as u8
        };
        match self.pending_signal {
            Some((current, current_weight)) if current.priority() > signal.priority() => {
                let max_w = if current_weight > weight {
                    current_weight
                } else {
                    weight
                };
                self.pending_signal = Some((current, max_w));
            }
            _ => self.pending_signal = Some((signal, weight)),
        }
    }

    /// Wheel/PgUp input while in the alt screen.
    #[inline]
    pub const fn note_alt_scroll(&mut self) {
        self.pending_alt_scroll = true;
    }

    /// Host-reported pane visibility (drain-on-unfocus policy).
    #[inline]
    pub const fn set_visibility(&mut self, v: RainVisibility) {
        if matches!(
            (v, self.visibility),
            (RainVisibility::Focused, RainVisibility::Focused)
                | (
                    RainVisibility::VisibleUnfocused,
                    RainVisibility::VisibleUnfocused
                )
                | (RainVisibility::Hidden, RainVisibility::Hidden)
        ) {
            return;
        }
        self.visibility = v;
        match v {
            RainVisibility::Hidden => self.drain_ticks = DRAIN_TICKS,
            RainVisibility::Focused => {
                if !matches!(self.weather, RainWeather::Sleep) {
                    self.drain_ticks = 0;
                }
            }
            RainVisibility::VisibleUnfocused => {}
        }
    }

    /// OS/config reduce-motion: rain emits nothing, fp 0, inactive.
    #[inline]
    pub const fn set_reduced_motion(&mut self, on: bool) {
        if on && !self.reduced_motion {
            self.pending_signal = None;
            self.pending_turn_start = false;
            self.semantic_phase = RainSignal::AssistantStream;
            self.semantic_energy = 0;
            self.semantic_ticks_left = 0;
        }
        self.reduced_motion = on;
    }

    /// True when the grid changed since the last Tier-A scan.
    #[must_use]
    #[inline]
    pub const fn needs_rescan(&self, epoch: u64) -> bool {
        !self.have_scanned || epoch != self.last_epoch
    }

    /// Damage epoch the Tier-A scan last consumed (`None` before the first
    /// [`rescan_from_cells`](Self::rescan_from_cells)).
    #[must_use]
    #[inline]
    pub const fn scanned_epoch(&self) -> Option<u64> {
        if self.have_scanned {
            Some(self.last_epoch)
        } else {
            None
        }
    }

    /// Rebuild the Tier-A occupancy bitset from the frame snapshot (the same `cell_frame_into` rows
    /// sparkle scans).
    #[inline]
    #[allow(
        clippy::too_many_arguments,
        reason = "the rescan threads the snapshot rows/line-sizes/images, grid geometry, live default bg, and damage epoch through one call; a wrapper struct would relocate the list, not simplify it"
    )]
    pub fn rescan_from_cells(
        &mut self,
        cells: &[Vec<RenderCell>],
        line_sizes: &[LineSize],
        images: &[Vec<(usize, ImageRef)>],
        rows: usize,
        cols: usize,
        default_bg: u32,
        epoch: u64,
    ) {
        let bits = rows * cols;
        let words = bits.div_ceil(64);
        self.occ.clear();
        self.occ.resize(words, u64::MAX);
        if let Some(last) = self.occ.last_mut() {
            *last = u64::MAX >> (words * 64 - bits);
        }
        self.occ_rows = rows;
        self.occ_cols = cols;
        let bg = [
            (default_bg >> 16) as u8,
            (default_bg >> 8) as u8,
            default_bg as u8,
        ];
        let last_col = cols.saturating_sub(1);
        let mut r = 0;
        while r < rows {
            if line_sizes.get(r) != Some(&LineSize::SingleWidth) {
                self.clear_occ_span(r, 0, last_col);
            }
            r += 1;
        }

        let mut r = 0;
        while r < rows {
            let row_cells: &[RenderCell] = if r < cells.len() {
                cells[r].as_slice()
            } else {
                &[]
            };
            let mut run: Option<(usize, usize)> = None;
            let col_limit = row_cells.len().min(cols);
            let mut c = 0;
            while c < col_limit {
                let cell = &row_cells[c];
                c += 1;
                let col = c - 1;
                let meaningful = cell.ch != ' '
                    || cell.wide
                    || cell.underline != UnderlineStyle::None
                    || cell.strikethrough
                    || cell.overline
                    || cell.bg != bg;
                if !meaningful {
                    continue;
                }
                let (lo, hi) = (col.saturating_sub(1), col + 1);
                match run {
                    Some((start, end)) if lo <= end + 1 => run = Some((start, hi)),
                    Some((start, end)) => {
                        self.clear_occ_neighborhood_span(r, start, end);
                        run = Some((lo, hi));
                    }
                    None => run = Some((lo, hi)),
                }
            }
            if let Some((start, end)) = run {
                self.clear_occ_neighborhood_span(r, start, end);
            }
            if let Some(spans) = images.get(r) {
                let mut si = 0;
                while si < spans.len() {
                    let c = spans[si].0;
                    if c < cols {
                        self.clear_occ_neighborhood_span(r, c.saturating_sub(1), c + 1);
                    }
                    si += 1;
                }
            }
            r += 1;
        }
        self.mask_framed_regions(cells, rows, cols);
        self.have_scanned = true;
        self.last_epoch = epoch;
    }

    /// Sample the program's real on-screen output into the rain's glyph alphabet.
    #[inline]
    pub fn sample_material(
        &mut self,
        cells: &[Vec<RenderCell>],
        rows: usize,
        cursor: Option<(u16, u16)>,
        hidden_band: &[u16],
    ) {
        if !self.cfg.output_material {
            self.clear_material_bank();
            self.material_sample_needed = false;
            return;
        }
        if self.material_editing {
            self.material_sample_needed = true;
            return;
        }
        self.material_sample_needed = false;
        self.material_scratch.clear();
        let mut r = rows;
        'rows: while r > 0 {
            r -= 1;
            if Self::row_banded(r, rows, cursor, hidden_band) {
                continue;
            }
            let Some(row_cells) = cells.get(r) else {
                continue;
            };
            let mut ci = row_cells.len();
            while ci > 0 {
                ci -= 1;
                let cell = &row_cells[ci];
                if cell.ch != ' ' && !cell.wide && rom::material_bitmap(cell.ch).is_some() {
                    self.material_scratch.push(cell.ch);
                    if self.material_scratch.len() == MATERIAL_CAP {
                        break 'rows;
                    }
                }
            }
        }
        self.material_scratch.reverse();
        let n = self.material_scratch.len();
        if n == 0 {
            self.clear_material_bank();
            return;
        }
        debug_assert!(n <= MATERIAL_CAP);

        self.material_slots_scratch.clear();
        let mut index = n;
        while index > 0 {
            index -= 1;
            let ch = self.material_scratch[index];
            if !self.material_slots_scratch.contains(&ch) {
                self.material_slots_scratch.push(ch);
                if self.material_slots_scratch.len() == MATERIAL_GLYPH_CAP {
                    break;
                }
            }
        }
        self.material_slots_scratch.sort_unstable();

        if self.material_chars != self.material_slots_scratch {
            let changed = slot_change_mask(&self.material_chars, &self.material_slots_scratch);
            self.material_chars.clone_from(&self.material_slots_scratch);
            if let Some(rom) = self.rom.as_mut()
                && self.baker.can_rebake()
            {
                let mut rest = changed;
                while rest != 0 {
                    let slot = rest.trailing_zeros() as usize;
                    rest &= rest - 1;
                    rom::reauthor_material_glyph(rom, slot, self.material_chars.get(slot).copied());
                }
                self.baker.rebake_tiles(rom, changed);
                self.last_bake_tick = Some(self.tick);
            } else {
                self.rom = Some(rom::rasterize_material_master(&self.material_chars));
                self.baker.restart();
                self.last_bake_tick = None;
                self.finish_material_bake();
            }
        }

        self.material.clear();
        let mut index = 0;
        while index < n {
            let ch = self.material_scratch[index];
            if let Ok(slot) = self.material_chars.binary_search(&ch) {
                self.material.push(slot as u8);
            }
            index += 1;
        }
    }

    /// Clockless advance for web hosts: accumulate host milliseconds; whole engine ticks are
    /// consumed by the next [`Self::emit`].
    #[inline]
    pub const fn advance_ms(&mut self, dt_ms: u64) {
        self.clock_ms = self.clock_ms.saturating_add(dt_ms);
    }

    /// Native tick: sync the internal clock from the injected instant (the epoch latches on first
    /// call), then emit.
    #[inline]
    pub fn tick(
        &mut self,
        now: Instant,
        geom: EffectGeom,
        input: &RainTickInput<'_>,
        quads: &mut Vec<RainCell>,
        add: &mut Vec<RainHalo>,
    ) -> u64 {
        self.sync_clock(now);
        self.emit(geom, input, quads, add)
    }

    /// Latch the epoch on first call and advance the clock monotonically.
    #[inline]
    fn sync_clock(&mut self, now: Instant) {
        let epoch = match self.epoch {
            Some(epoch) => epoch,
            None => {
                self.epoch = Some(now);
                now
            }
        };
        let ms = now.saturating_duration_since(epoch).as_millis() as u64;
        if ms > self.clock_ms {
            self.clock_ms = ms;
        }
    }

    /// Suspended tick for hosts that skip emission entirely (alt-screen suppression, the perf
    /// load-shed latch).
    #[inline]
    pub fn tick_suspended(&mut self, now: Instant) {
        self.sync_clock(now);
        self.step_suspended();
    }

    /// Clockless core of [`Self::tick_suspended`] (web hosts pair it with [`Self::advance_ms`],
    /// exactly like `emit`).
    #[inline]
    pub const fn step_suspended(&mut self) {
        self.pending_key = false;
        self.pending_content_credit = 0;
        self.advance_engine_ticks();
        self.last_emit_nonempty = false;
    }

    /// Advance whole engine ticks out of the accumulated clock, then fill the frame's rain quads
    /// and additive halos.
    #[inline]
    pub fn emit(
        &mut self,
        geom: EffectGeom,
        input: &RainTickInput<'_>,
        quads: &mut Vec<RainCell>,
        add: &mut Vec<RainHalo>,
    ) -> u64 {
        quads.clear();
        add.clear();
        self.halo_cands.clear();
        if !self.cfg.enabled || self.reduced_motion {
            self.last_emit_nonempty = false;
            return 0;
        }
        self.apply_pending_notes();
        let ticked = self.advance_engine_ticks();
        let reading = input.display_offset != 0
            || (input.is_alt_screen
                && (self.cfg.suppress_in_alt_screen || self.clock_ms < self.quiet_until_ms));
        self.reading_gated = reading;
        let gated = geom.rows == 0
            || geom.cols == 0
            || geom.cell_w == 0
            || geom.cell_h == 0
            || !self.have_scanned
            || self.drain_ticks >= DRAIN_TICKS
            || reading;
        let material_ready = !self.cfg.output_material || !self.material.is_empty();
        if !gated && material_ready {
            self.baker.begin_frame(geom.cell_w, geom.cell_h);
            if !self.baker.complete() && !self.material.is_empty() {
                self.finish_material_bake();
            } else if !self.baker.complete() && (ticked || self.last_bake_tick != Some(self.tick)) {
                if self.rom.is_none() {
                    self.rom = Some(decorative_master().clone());
                }
                if let Some(rom) = self.rom.as_ref() {
                    self.baker.bake_tiles(rom);
                }
                self.last_bake_tick = Some(self.tick);
            }
            self.ensure_col_order(geom.cols, geom.rows);
            self.emit_field(geom, input, quads, add);
            self.row_sort_stable(quads, usize::from(geom.rows));
        }
        let fp = fingerprint(quads, add, self.tick, self.baker.version());
        self.last_emit_nonempty = fp != 0;
        fp
    }

    /// Whether the host should keep the rain timer armed.
    #[must_use]
    #[inline]
    pub const fn is_active(&self) -> bool {
        if !self.cfg.enabled || self.reduced_motion {
            return false;
        }
        if self.cfg.output_material && self.material.is_empty() {
            return false;
        }
        if self.last_emit_nonempty {
            return true;
        }
        if self.reading_gated {
            return false;
        }
        match self.visibility {
            RainVisibility::Focused => {
                !matches!(self.eff_weather(), RainWeather::Sleep) || self.drain_ticks < DRAIN_TICKS
            }
            _ => self.drain_ticks < DRAIN_TICKS,
        }
    }

    /// `glyph`'s baked tile, the art a [`RainCell`] of that glyph draws (`None` until baked).
    #[must_use]
    #[inline]
    pub const fn tile(&self, glyph: u8) -> Option<&Arc<FxArt>> {
        self.baker.tile(glyph)
    }

    /// The glyph tiles a frame's [`RainCell`]s draw ([`RainLayers::publish`]).
    #[must_use]
    #[inline]
    pub const fn baker(&self) -> &RainBaker {
        &self.baker
    }

    /// The tile-set version (folded into the fingerprint; exposed for hosts).
    #[must_use]
    #[inline]
    pub const fn atlas_version(&self) -> u64 {
        self.baker.version()
    }

    /// One-line diagnostic for host introspection (`aterm ctl rain status`).
    #[must_use]
    #[inline]
    pub fn diag_line(&self) -> String {
        let weather = match self.eff_weather() {
            RainWeather::Working => "working",
            RainWeather::Calm => "calm",
            RainWeather::Sleep => "sleep",
        };
        let vis = match self.visibility {
            RainVisibility::Focused => "focused",
            RainVisibility::VisibleUnfocused => "visible",
            RainVisibility::Hidden => "hidden",
        };
        let seq = self.last_seq;
        format!(
            "weather={} density={} tick={} scanned={} material={} emitting={} vis={} drain={} \
             seq={} streak={}",
            weather,
            self.density_byte,
            self.tick,
            self.have_scanned,
            self.material_chars.len(),
            self.last_emit_nonempty,
            vis,
            self.drain_ticks,
            core::fmt::from_fn(|f| match seq {
                Some(s) => write!(f, "{s}"),
                None => f.write_str("none"),
            }),
            self.content_streak,
        )
    }

    /// What the weather has heard from its host, for tests.
    #[cfg(test)]
    pub(crate) const fn heard_for_test(&self) -> (RainSignal, bool) {
        (self.semantic_phase, self.clock_ms < self.fail_until_ms)
    }

    #[cfg(test)]
    pub(crate) const fn literal_material_chars_for_test(&self) -> &[char] {
        self.material_chars.as_slice()
    }

    #[cfg(test)]
    pub(crate) const fn config_for_test(&self) -> RainConfig {
        self.cfg
    }

    #[inline]
    const fn apply_pending_notes(&mut self) {
        let now = self.clock_ms;
        if self.pending_key {
            self.pending_key = false;
            self.last_key_ms = Some(now);
        }
        let turn_start = self.pending_turn_start;
        self.pending_turn_start = false;
        if turn_start {
            self.last_key_ms = None;
            self.last_content_ms = None;
            self.content_streak = 0;
        }
        let credit = self.pending_content_credit;
        self.pending_content_credit = 0;
        if credit > 0 {
            let echo =
                matches!(self.last_key_ms, Some(k) if now.saturating_sub(k) < ECHO_DISCOUNT_MS);
            let content_credit = if echo { 0 } else { credit };
            if content_credit > 0 {
                self.content_streak = match self.last_content_ms {
                    Some(prev) if now.saturating_sub(prev) <= STREAK_WINDOW_MS => {
                        self.content_streak.saturating_add(content_credit as u32)
                    }
                    _ => content_credit as u32,
                };
                self.last_content_ms = Some(now);
            }
        }
        if self.pending_bell {
            self.pending_bell = false;
            if self.cfg.bell_alert {
                self.bell_until_ms = now + BELL_ALERT_MS;
            }
        }
        let exit = self.pending_exit;
        self.pending_exit = None;
        if let Some(failed) = exit {
            // Exit status: success rides the turn-wave machinery
            // (limiter-bounded); failure holds the ember ramp.
            if self.cfg.exit_tint {
                if failed {
                    self.fail_until_ms = now + EXIT_FAIL_MS;
                } else {
                    self.wave_pending = true;
                }
            }
        }
        let pending_sig = self.pending_signal;
        self.pending_signal = None;
        if let Some((signal, energy)) = pending_sig {
            let phase_changed = (self.semantic_phase as u8) != (signal as u8);
            self.semantic_phase = signal;
            self.semantic_energy = if energy < SEMANTIC_ENERGY_CAP {
                energy
            } else {
                SEMANTIC_ENERGY_CAP
            };
            self.semantic_ticks_left = SEMANTIC_HOLD_TICKS;
            self.semantic_started_ms = now;
            if phase_changed {
                self.semantic_seq = self.semantic_seq.wrapping_add(1);
            }
            match signal {
                RainSignal::Success if self.cfg.exit_tint => self.wave_pending = true,
                RainSignal::Failure | RainSignal::Interrupted if self.cfg.exit_tint => {
                    self.fail_until_ms = now + EXIT_FAIL_MS;
                }
                _ => {}
            }
        }
        if self.pending_alt_scroll {
            self.pending_alt_scroll = false;
            self.quiet_until_ms = now + ALT_SCROLL_QUIET_MS;
        }
    }

    /// The current tick-consumption period in ms, for the host's timer: WORKING runs at the fps
    /// knob.
    #[must_use]
    #[inline]
    pub const fn current_period_ms(&self) -> u64 {
        let ms = match self.eff_weather() {
            RainWeather::Working => self.tick_ms,
            RainWeather::Calm | RainWeather::Sleep => {
                if self.tick_ms > CALM_TICK_MS {
                    self.tick_ms
                } else {
                    CALM_TICK_MS
                }
            }
        } as u64;
        if ms > 1 { ms } else { 1 }
    }

    /// Remaining milliseconds until the next engine tick.
    #[must_use]
    #[inline]
    pub const fn next_tick_in_ms(&self) -> u64 {
        let period = self.current_period_ms();
        let backlog = self.clock_ms.saturating_sub(self.consumed_ms);
        let consumed = if backlog < period { backlog } else { period };
        period.saturating_sub(consumed)
    }

    /// Weather capped by visibility (`min`: unfocused ≤ CALM, hidden = SLEEP).
    #[inline]
    const fn eff_weather(&self) -> RainWeather {
        match (self.weather, self.visibility) {
            (w, RainVisibility::Focused) => w,
            (RainWeather::Sleep, _) | (_, RainVisibility::Hidden) => RainWeather::Sleep,
            (_, RainVisibility::VisibleUnfocused) => RainWeather::Calm,
        }
    }

    #[inline]
    const fn advance_engine_ticks(&mut self) -> bool {
        let mut ticked = false;
        let backlog = self.clock_ms.saturating_sub(self.consumed_ms);
        if backlog > CATCHUP_MS {
            let period = self.current_period_ms();
            let skip = (backlog - CATCHUP_MS) / period;
            self.tick += skip;
            self.consumed_ms += skip * period;
        }
        loop {
            let period = self.current_period_ms();
            if self.clock_ms.saturating_sub(self.consumed_ms) < period {
                return ticked;
            }
            self.consumed_ms += period;
            self.step_engine_tick();
            ticked = true;
        }
    }

    #[inline]
    const fn step_engine_tick(&mut self) {
        self.tick += 1;
        let now = self.consumed_ms;
        if self.semantic_ticks_left > 0 && now >= self.semantic_started_ms {
            self.semantic_ticks_left -= 1;
            if self.semantic_ticks_left == 0 {
                self.semantic_phase = RainSignal::AssistantStream;
                self.semantic_energy = 0;
            }
        }
        let desired = self.desired_weather(now);
        if (desired as u8) != (self.weather as u8)
            && now.saturating_sub(self.weather_since_ms) >= DWELL_MS
        {
            // Any transition out of Working is a completed turn, including a
            // direct Working→Sleep jump at the idle_secs minimum (where Calm is
            // skipped).
            if matches!(self.weather, RainWeather::Working)
                && !matches!(desired, RainWeather::Working)
                && self.cfg.turn_wave
                && !self.reduced_motion
                && matches!(self.visibility, RainVisibility::Focused)
            {
                self.wave_pending = true;
            }
            self.weather = desired;
            self.weather_since_ms = now;
        }
        let eff = self.eff_weather();
        let target = (self.cfg.density as i32)
            * match eff {
                RainWeather::Working => 21,
                RainWeather::Calm => 7,
                RainWeather::Sleep => 0,
            };
        self.ema_fx += ((target << 6) - self.ema_fx) / 8;
        let byte = quantize_density(self.ema_fx >> 6);
        if byte != self.density_byte {
            self.density_byte = byte;
            self.ring.push(self.tick, byte);
        }
        if self.wave_pending && self.drain_ticks == 0 {
            let mut live = 0usize;
            let mut i = 0;
            while i < WAVE_IGNITIONS_PER_SEC {
                let t = self.ign_ms[i];
                if t != u64::MAX && now.saturating_sub(t) < 1000 {
                    live += 1;
                }
                i += 1;
            }
            if live < WAVE_IGNITIONS_PER_SEC {
                let idx = if self.ign_ms[0] == u64::MAX {
                    0
                } else if self.ign_ms[1] == u64::MAX {
                    1
                } else if self.ign_ms[0] <= self.ign_ms[1] {
                    0
                } else {
                    1
                };
                self.ign_ms[idx] = now;
                let raw_len = (WAVE_MS / self.current_period_ms()) as u32;
                let len = if raw_len > 1 { raw_len } else { 1 };
                self.wave = Some((self.tick, len));
                self.wave_pending = false;
            }
        }
        if matches!(self.visibility, RainVisibility::Hidden) {
            self.drain_ticks = DRAIN_TICKS;
        } else if !matches!(self.visibility, RainVisibility::Focused)
            || matches!(eff, RainWeather::Sleep)
        {
            let next_drain = self.drain_ticks + 1;
            self.drain_ticks = if next_drain < DRAIN_TICKS {
                next_drain
            } else {
                DRAIN_TICKS
            };
        } else {
            self.drain_ticks = 0;
        }
        if let Some((start, len)) = self.wave
            && self.tick.saturating_sub(start) > len as u64
        {
            self.wave = None;
        }
    }

    #[inline]
    const fn desired_weather(&self, now: u64) -> RainWeather {
        let content_recent = matches!(
            self.last_content_ms,
            Some(t) if now.saturating_sub(t) < WORKING_HOLD_MS
        ) && self.content_streak >= 2;
        if content_recent {
            return RainWeather::Working;
        }
        if self.semantic_ticks_left > 0 {
            match self.semantic_phase {
                RainSignal::Inspect
                | RainSignal::Modify
                | RainSignal::Execute
                | RainSignal::Network
                | RainSignal::Branch => return RainWeather::Working,
                RainSignal::TurnStart
                | RainSignal::Waiting
                | RainSignal::Success
                | RainSignal::Failure
                | RainSignal::Interrupted => return RainWeather::Calm,
                RainSignal::AssistantStream => {}
            }
        }
        let idle_ms = (self.cfg.idle_secs as u64) * 1000;
        let any_recent = matches!(self.last_content_ms, Some(t) if now.saturating_sub(t) < idle_ms)
            || matches!(self.last_key_ms, Some(t) if now.saturating_sub(t) < idle_ms);
        if any_recent {
            RainWeather::Calm
        } else {
            RainWeather::Sleep
        }
    }

    #[inline]
    fn ensure_col_order(&mut self, cols: u16, rows: u16) {
        if cols == self.order_cols && self.order_seed == self.seed32 && rows == self.order_rows {
            return;
        }
        self.order_cols = cols;
        self.order_seed = self.seed32;
        self.order_rows = rows;
        self.col_order.clear();
        self.col_order.extend(0..cols);
        let seed = self.seed32;
        self.col_order
            .sort_by_key(|&c| field::rain_hash32(seed ^ ((u32::from(c) << 1) | 1)));
        let fp = self.field_params(rows);
        self.col_params_cache.clear();
        self.col_params_cache.reserve(usize::from(cols));
        let cols_u32 = u32::from(cols);
        let mut c = 0;
        while c < cols_u32 {
            self.col_params_cache.push(col_params(&fp, c));
            c += 1;
        }
    }

    #[inline]
    const fn field_params(&self, rows: u16) -> FieldParams {
        FieldParams {
            seed32: self.seed32,
            rows: rows as u32,
            tick_ms: self.tick_ms,
            speed: self.cfg.speed as u32,
            trail: self.cfg.trail as u32,
            mq: self.mq,
            dq: self.dq,
            sq: self.sq,
        }
    }

    #[inline]
    fn row_sort_stable(&mut self, quads: &mut [RainCell], rows: usize) {
        if quads.len() < 2 || rows == 0 {
            return;
        }
        self.row_starts.clear();
        self.row_starts.resize(rows + 1, 0);
        let mut qi = 0;
        while qi < quads.len() {
            self.row_starts[usize::from(quads[qi].row) + 1] += 1;
            qi += 1;
        }
        let mut r = 0;
        while r < rows {
            self.row_starts[r + 1] += self.row_starts[r];
            r += 1;
        }
        self.sort_scratch.clear();
        self.sort_scratch.extend_from_slice(quads);
        let mut si = 0;
        while si < self.sort_scratch.len() {
            let q = self.sort_scratch[si];
            let slot = &mut self.row_starts[usize::from(q.row)];
            quads[*slot as usize] = q;
            *slot += 1;
            si += 1;
        }
    }

    #[inline]
    fn emit_field(
        &mut self,
        geom: EffectGeom,
        input: &RainTickInput<'_>,
        quads: &mut Vec<RainCell>,
        add: &mut Vec<RainHalo>,
    ) {
        let ctx = EmitCtx {
            fp: self.field_params(geom.rows),
            geom,
            input,
            cap: quad_cap(u32::from(geom.cell_w), u32::from(geom.cell_h)),
            pen: self.drain_ticks * 15 / DRAIN_TICKS,
            ramp: if self.clock_ms < self.fail_until_ms {
                self.ramp_fail
            } else if self.clock_ms < self.bell_until_ms {
                self.ramp_alert
            } else {
                self.ramp
            },
            wave_row: self.wave.map(|(start, len)| {
                let el = self.tick.saturating_sub(start).min(u64::from(len)) as u32;
                el * u32::from(geom.rows) / len.max(1)
            }),
            glyph_epoch: glyph_epoch(self.tick, self.mq),
            dither_epoch: dither_epoch(self.tick, self.dq),
        };
        let mut i = 0;
        while i < self.col_order.len() {
            let col = self.col_order[i];
            i += 1;
            let q0 = quads.len();
            let h0 = self.halo_cands.len();
            if !self.emit_column(&ctx, col, quads) {
                quads.truncate(q0);
                self.halo_cands.truncate(h0);
                break;
            }
        }
        self.emit_halos(&ctx, add);
    }

    #[inline]
    fn emit_column(&mut self, ctx: &EmitCtx<'_>, col: u16, quads: &mut Vec<RainCell>) -> bool {
        let cw = u32::from(ctx.geom.cell_w);
        let rows = u32::from(ctx.geom.rows);
        let x0 = u32::from(col) * cw;
        if x0 + cw > u32::from(u16::MAX) {
            return true;
        }
        let cp = self.col_params_cache[usize::from(col)];
        let cv = cycle_at(&cp, &ctx.fp, self.tick);
        let density = self.ring.at(cycle_start_tick(&cp, cv.k));
        if !cycle_active(cv.hk, density) {
            return true;
        }
        let mut material_start: Option<Option<usize>> = None;
        let r_hi = cv.head_row.min(rows.saturating_sub(1));
        let r_lo = cv.head_row.saturating_sub(cp.l);
        if r_lo <= r_hi && r_lo < rows {
            let mut r = r_lo;
            while r <= r_hi {
                let cur_r = r;
                r += 1;
                // Cheapest reject first: an occupied cell needs no trail math.
                if !self.occ_bit(cur_r, col) {
                    continue;
                }
                let Some(lvl0) = trail_level(&cp, cv.eff_tick, cv.k, cur_r) else {
                    continue;
                };
                let is_head = cur_r == cv.head_row;
                let Some(lvl) = self.drained_level(ctx, &cp, lvl0, cur_r, is_head) else {
                    continue;
                };
                if !self.cell_unmasked(cur_r, col, ctx.input) {
                    continue;
                }
                if quads.len() >= ctx.cap {
                    return false;
                }
                let head_flash = is_head && lvl == 15;
                let ms = match material_start {
                    Some(ms) => ms,
                    None => {
                        let ms = if self.material.is_empty() {
                            None
                        } else {
                            Some(self.material_start(col))
                        };
                        material_start = Some(ms);
                        ms
                    }
                };
                quads.push(self.cell_quad(ctx, col, cur_r, lvl, head_flash, ms));
                if head_flash && cv.bright {
                    self.halo_cands.push((cur_r as u16, col));
                }
            }
        }
        if let Some(wr) = ctx.wave_row
            && wr < rows
            && self.cell_eligible(wr, col, ctx.input)
            && trail_level(&cp, cv.eff_tick, cv.k, wr).is_none()
        {
            let lvl = 15u32.saturating_sub(ctx.pen);
            if quads.len() >= ctx.cap {
                return false;
            }
            let ms = match material_start {
                Some(ms) => ms,
                None if self.material.is_empty() => None,
                None => Some(self.material_start(col)),
            };
            quads.push(self.cell_quad(ctx, col, wr, lvl, true, ms));
        }
        true
    }

    /// Apply the drain penalty + flicker dither to a base trail level.
    #[inline]
    const fn drained_level(
        &self,
        ctx: &EmitCtx<'_>,
        cp: &ColParams,
        lvl0: u32,
        row: u32,
        is_head: bool,
    ) -> Option<u32> {
        let mut lvl = lvl0 as i64 - ctx.pen as i64;
        if lvl < 0 {
            return None;
        }
        if !is_head {
            let d = lvl - dither(cp.h, row, ctx.dither_epoch) as i64;
            lvl = if d > 0 { d } else { 0 };
        }
        Some(lvl as u32)
    }

    /// One resolved rain cell quad (tile rect from the baker, tint/alpha from
    /// the ramp, `flip_x` from the glyph hash).
    #[inline]
    const fn cell_quad(
        &self,
        ctx: &EmitCtx<'_>,
        col: u16,
        row: u32,
        lvl: u32,
        head: bool,
        material_start: Option<usize>,
    ) -> RainCell {
        let (cw, ch) = (ctx.geom.cell_w, ctx.geom.cell_h);
        let (g, flip_x) = match material_start {
            None => glyph_from_epoch(self.seed32, row, col as u32, ctx.glyph_epoch),
            Some(start) => {
                let ix = self.semantic_material_index(start, col, row, ctx.glyph_epoch);
                (self.material.as_slice()[ix] as u32, false)
            }
        };
        let alpha = if head {
            self.head_alpha
        } else {
            self.alpha_of(lvl)
        };
        let ramp_ix = if (lvl as usize) < 15 {
            lvl as usize
        } else {
            15
        };
        const _: () = assert!(rom::ROM_GLYPHS.is_power_of_two());
        RainCell {
            row: row as u16,
            x: col.wrapping_mul(cw),
            y: (row as u16).wrapping_mul(ch),
            w: cw,
            h: ch,
            glyph: (g & (rom::ROM_GLYPHS as u32 - 1)) as u8,
            tint: ctx.ramp[ramp_ix],
            alpha,
            flip_x,
        }
    }

    /// Map an emitted cell onto the output tape.
    #[inline]
    const fn semantic_material_index(
        &self,
        start: usize,
        col: u16,
        row: u32,
        glyph_epoch: u32,
    ) -> usize {
        let len = self.material.len();
        debug_assert!(len > 0);
        let row = row as usize;
        let epoch = glyph_epoch as usize;
        let third = if len / 3 > 1 { len / 3 } else { 1 };
        let offset = match self.semantic_phase {
            RainSignal::Inspect => row.wrapping_add(epoch),
            RainSignal::Modify if col % 2 == 1 => len.wrapping_sub(row.wrapping_add(epoch) % len),
            RainSignal::Modify => row.wrapping_add(epoch),
            RainSignal::Execute => row.wrapping_add(epoch.wrapping_mul(2)),
            RainSignal::Network => row / 2 + epoch,
            RainSignal::Branch => row
                .wrapping_add(epoch)
                .wrapping_add((col % 3) as usize * third),
            RainSignal::Waiting => row.wrapping_add(epoch / 4),
            RainSignal::Success
            | RainSignal::Failure
            | RainSignal::Interrupted
            | RainSignal::TurnStart => row.wrapping_add(epoch),
            RainSignal::AssistantStream => row.wrapping_add(epoch),
        };
        start.wrapping_add(offset) % len
    }

    /// Where column `col`'s walk starts on the output tape: a hash of the
    /// column (or, under a semantic pulse, of its lane) — constant for every
    /// row of the column this frame.
    #[inline]
    const fn material_start(&self, col: u16) -> usize {
        if matches!(self.semantic_phase, RainSignal::AssistantStream) {
            return field::rain_hash32((col as u32) ^ self.seed32 ^ 0x00C0_FFEE) as usize;
        }
        let base_width = match self.semantic_phase {
            RainSignal::Inspect | RainSignal::Modify => 2,
            RainSignal::Network | RainSignal::Branch => 3,
            RainSignal::Execute
            | RainSignal::Waiting
            | RainSignal::Success
            | RainSignal::Failure
            | RainSignal::Interrupted
            | RainSignal::TurnStart => 4,
            RainSignal::AssistantStream => 1,
        };
        let raw_w = base_width + (self.semantic_energy.saturating_sub(1) / 3) as u16;
        let lane_width = if raw_w < 8 { raw_w } else { 8 };
        let denom = if lane_width > 1 { lane_width } else { 1 };
        let lane = (col / denom) as u32;
        field::rain_hash32(
            lane ^ self.seed32 ^ self.semantic_seq.wrapping_mul(0x9E37_79B9) ^ 0x005E_A11C,
        ) as usize
    }

    /// Body coverage for a trail level: linear in level, capped at the
    /// derived body alpha (level 15 body cells sit just under the head).
    #[inline]
    const fn alpha_of(&self, lvl: u32) -> u8 {
        let a = (self.body_alpha as u32 * (lvl + 1)) / 16;
        if a > 1 { a as u8 } else { 1 }
    }

    #[inline]
    fn mask_framed_regions(&mut self, cells: &[Vec<RenderCell>], rows: usize, cols: usize) {
        #[inline]
        const fn cell_ch_at(row: &[RenderCell], col: usize) -> char {
            if col < row.len() { row[col].ch } else { ' ' }
        }

        self.frame_candidates.clear();
        self.frame_regions.clear();

        let mut r = 0;
        while r < rows {
            let row = if r < cells.len() {
                cells[r].as_slice()
            } else {
                &[]
            };

            let mut prefix_built = false;
            let mut write = 0usize;
            let cand_len = self.frame_candidates.len();
            let mut i = 0;
            while i < cand_len {
                let mut candidate = self.frame_candidates[i];
                i += 1;
                let left = cell_ch_at(row, candidate.left);
                let right = cell_ch_at(row, candidate.right);
                if frame_bottom_left(left) && frame_bottom_right(right) && r > candidate.top + 1 {
                    if !prefix_built {
                        self.frame_horizontal_prefix.resize(cols + 1, 0);
                        self.frame_horizontal_prefix[0] = 0;
                        let mut col = 0;
                        while col < cols {
                            self.frame_horizontal_prefix[col + 1] = self.frame_horizontal_prefix
                                [col]
                                + usize::from(frame_horizontal(cell_ch_at(row, col)));
                            col += 1;
                        }
                        prefix_built = true;
                    }
                    let interior_rows = r - candidate.top - 1;
                    let interior_cols = candidate.right - candidate.left - 1;
                    let horizontal_cols = self.frame_horizontal_prefix[candidate.right]
                        - self.frame_horizontal_prefix[candidate.left + 1];
                    let sides_close =
                        candidate.side_rows.saturating_mul(4) >= interior_rows.saturating_mul(3);
                    let bottom_closes =
                        horizontal_cols.saturating_mul(4) >= interior_cols.saturating_mul(3);
                    if sides_close && bottom_closes {
                        if self.frame_regions.len() == MAX_FRAME_REGIONS {
                            self.occ.fill(0);
                            self.frame_candidates.clear();
                            self.frame_regions.clear();
                            return;
                        }
                        self.frame_regions.push(FrameRegion {
                            top: candidate.top,
                            bottom: r,
                            left: candidate.left,
                            right: candidate.right,
                        });
                    }
                    continue;
                }

                if frame_vertical(left) && frame_vertical(right) {
                    candidate.side_rows += 1;
                } else {
                    candidate.gap_rows += 1;
                    if candidate.gap_rows > 2 {
                        continue;
                    }
                }
                self.frame_candidates[write] = candidate;
                write += 1;
            }
            self.frame_candidates.truncate(write);

            let mut open_left = None;
            let mut has_horizontal = false;
            let col_limit = row.len().min(cols);
            let mut col = 0;
            while col < col_limit {
                let ch = row[col].ch;
                let cur_col = col;
                col += 1;
                if !in_box_drawing_block(ch) {
                    continue;
                }
                if frame_top_left(ch) {
                    open_left = Some(cur_col);
                    has_horizontal = false;
                    continue;
                }
                let Some(left) = open_left else {
                    continue;
                };
                if frame_horizontal(ch) {
                    has_horizontal = true;
                }
                if cur_col >= left + 2 && frame_top_right(ch) {
                    if has_horizontal {
                        if self.frame_candidates.len() == MAX_FRAME_CANDIDATES {
                            self.occ.fill(0);
                            self.frame_candidates.clear();
                            self.frame_regions.clear();
                            return;
                        }
                        self.frame_candidates.push(FrameCandidate {
                            top: r,
                            left,
                            right: cur_col,
                            side_rows: 0,
                            gap_rows: 0,
                        });
                    }
                    open_left = None;
                    has_horizontal = false;
                }
            }
            r += 1;
        }

        let mut i = 0;
        while i < self.frame_regions.len() {
            let region = self.frame_regions[i];
            let mut row = region.top + 1;
            while row < region.bottom {
                self.clear_occ_span(row, region.left + 1, region.right.saturating_sub(1));
                row += 1;
            }
            i += 1;
        }
    }

    #[inline]
    const fn clear_occ_span(&mut self, row: usize, start_col: usize, end_col: usize) {
        if row >= self.occ_rows || start_col > end_col || start_col >= self.occ_cols {
            return;
        }
        let max_col = self.occ_cols - 1;
        let end_col = if end_col < max_col { end_col } else { max_col };
        let first_bit = row * self.occ_cols + start_col;
        let last_bit = row * self.occ_cols + end_col;
        let first_word = first_bit >> 6;
        let last_word = last_bit >> 6;
        let first_mask = u64::MAX << (first_bit & 63);
        let last_mask = u64::MAX >> (63 - (last_bit & 63));
        let occ = self.occ.as_mut_slice();
        if first_word == last_word {
            occ[first_word] &= !(first_mask & last_mask);
            return;
        }
        occ[first_word] &= !first_mask;
        let mut w = first_word + 1;
        while w < last_word {
            occ[w] = 0;
            w += 1;
        }
        occ[last_word] &= !last_mask;
    }

    #[inline]
    const fn clear_occ_neighborhood_span(&mut self, row: usize, start_col: usize, end_col: usize) {
        if self.occ_rows == 0 || self.occ_cols == 0 {
            return;
        }
        let r0 = row.saturating_sub(1);
        let max_r = self.occ_rows - 1;
        let r1_raw = row.saturating_add(1);
        let r1 = if r1_raw < max_r { r1_raw } else { max_r };
        let mut r = r0;
        while r <= r1 {
            self.clear_occ_span(r, start_col, end_col);
            r += 1;
        }
    }

    /// The Tier-A occupancy bit alone (bounds-checked): the cheapest reject,
    /// hoisted ahead of the per-cell trail math in [`Self::emit_column`]; the
    /// trail loop then asks only [`Self::cell_unmasked`].
    #[inline]
    const fn occ_bit(&self, row: u32, col: u16) -> bool {
        let (r, c) = (row as usize, col as usize);
        if r >= self.occ_rows || c >= self.occ_cols {
            return false;
        }
        let bit = r * self.occ_cols + c;
        (self.occ.as_slice()[bit >> 6] >> (bit & 63)) & 1 == 1
    }

    /// Tier-A + Tier-B mask: occupancy bit, cursor band, live selection.
    #[inline]
    fn cell_eligible(&self, row: u32, col: u16, input: &RainTickInput<'_>) -> bool {
        self.occ_bit(row, col) && self.cell_unmasked(row, col, input)
    }

    /// The Tier-B half of [`Self::cell_eligible`]: cursor band, live selection.
    #[inline]
    fn cell_unmasked(&self, row: u32, col: u16, input: &RainTickInput<'_>) -> bool {
        // The composer band `sample_material` protects also holds for
        // emission, even before the damage ring warms. `row < geom.rows`, so it
        // fits a u16.
        if Self::row_banded(row as usize, self.occ_rows, input.cursor, input.hidden_band) {
            return false;
        }
        match input.sel.as_ref() {
            Some(sv) => !sv
                .sel
                .contains(i64::from(row) as i32 - sv.display_offset, col),
            None => true,
        }
    }

    /// The typing-surface band: visible cursor row ± [`CURSOR_BAND_ROWS`]; the
    /// host's recent-damage composer rows (protected even with a visible cursor,
    /// covering multiline input); and, with a hidden cursor and unknown band
    /// (first enable, resize, full damage), the bottom
    /// [`HIDDEN_CURSOR_BAND_ROWS`] rows.
    #[inline]
    const fn row_banded(
        r: usize,
        rows: usize,
        cursor: Option<(u16, u16)>,
        hidden_band: &[u16],
    ) -> bool {
        let row = r as u16;
        if let Some((cr, _)) = cursor
            && (cr.abs_diff(row) as i32) <= CURSOR_BAND_ROWS
        {
            return true;
        }
        let mut i = 0;
        while i < hidden_band.len() {
            if hidden_band[i] == row {
                return true;
            }
            i += 1;
        }
        cursor.is_none() && hidden_band.is_empty() && r + HIDDEN_CURSOR_BAND_ROWS >= rows
    }

    #[inline]
    fn emit_halos(&mut self, ctx: &EmitCtx<'_>, add: &mut Vec<RainHalo>) {
        if self.halo_cands.is_empty() {
            return;
        }
        let (cw, ch) = (i32::from(ctx.geom.cell_w), i32::from(ctx.geom.cell_h));
        if cw <= 0 || ch <= 0 {
            return;
        }
        let grid_w = i32::from(ctx.geom.cols) * cw;
        let grid_h = i32::from(ctx.geom.rows) * ch;
        let premul = premul_rgb(ctx.ramp[15], self.head_alpha / 2);
        if premul == 0 {
            return;
        }
        let cap = i32::from(u16::MAX);
        let rx = cw.clamp(1, cap) as u16;
        let ry = ch.clamp(1, cap) as u16;
        let half_cw = cw / 2;
        let half_ch = ch / 2;
        let cw_plus_half = cw + half_cw;
        let ch_plus_half = ch + half_ch;
        let len = self.halo_cands.len();
        let mut idx = (self.tick / u64::from(self.dq.max(1))) as usize % len;
        let mut step = 0;
        while step < len {
            step += 1;
            let (r, c) = self.halo_cands[idx];
            idx += 1;
            if idx == len {
                idx = 0;
            }
            if add.len() + 3 > MAX_RAIN_ADD {
                return;
            }
            let cell_x = i32::from(c) * cw;
            let cell_y = i32::from(r) * ch;
            let x0 = (cell_x - half_cw).max(0);
            let x1 = (cell_x + cw_plus_half).min(grid_w);
            let y0 = (cell_y - half_ch).max(0);
            let y1 = (cell_y + ch_plus_half).min(grid_h);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let cx = (cell_x + half_cw).clamp(0, cap) as u16;
            let cy = (cell_y + half_ch).clamp(0, cap) as u16;
            let x = x0 as u16;
            let w = (x1 - x0) as u16;
            let mut yy = y0;
            while yy < y1 {
                let band_row = yy / ch;
                let band_end = ((band_row + 1) * ch).min(y1);
                add.push(RainHalo {
                    row: band_row as u16,
                    x,
                    y: yy as u16,
                    w,
                    h: (band_end - yy) as u16,
                    color: premul,
                    cx,
                    cy,
                    rx,
                    ry,
                    ..Default::default()
                });
                yy = band_end;
            }
        }
    }
}

#[inline]
const fn clamp_config(mut cfg: RainConfig) -> RainConfig {
    const fn clamp_u8(v: u8, lo: u8, hi: u8) -> u8 {
        if v < lo {
            lo
        } else if v > hi {
            hi
        } else {
            v
        }
    }
    const fn clamp_u16(v: u16, lo: u16, hi: u16) -> u16 {
        if v < lo {
            lo
        } else if v > hi {
            hi
        } else {
            v
        }
    }
    cfg.fps = clamp_u8(cfg.fps, 12, 60);
    cfg.density = clamp_u8(cfg.density, 1, 12);
    cfg.speed = clamp_u8(cfg.speed, 1, 10);
    cfg.trail = clamp_u8(cfg.trail, 1, 10);
    cfg.mutation_ms = clamp_u16(cfg.mutation_ms, 80, 2000);
    cfg.idle_secs = clamp_u16(cfg.idle_secs, 2, 120);
    cfg.alpha_override = match cfg.alpha_override {
        Some(a) => Some(clamp_u8(a, RAIN_ALPHA_FLOOR, RAIN_ALPHA_CAP)),
        None => None,
    };
    cfg.head_alpha_override = match cfg.head_alpha_override {
        Some(a) => Some(clamp_u8(a, RAIN_ALPHA_FLOOR, RAIN_ALPHA_CAP)),
        None => None,
    };
    cfg
}

/// Quantize the EMA byte onto the density staircase (steps of 21 — one
/// WORKING density unit), so ring change-points stay a handful per front.
#[inline]
const fn quantize_density(v: i32) -> u8 {
    let v = if v > 0 { v } else { 0 };
    let q = ((v + 10) / 21) * 21;
    if q < 255 { q as u8 } else { 255 }
}

const THEME_HUE_MIN_CHROMA: f32 = 0.03;

const THEME_HUE_FALLBACK: u32 = 0x003B_8EEA;
const THEME_HUE_FALLBACK_DEG: f32 = rgb2hsv(THEME_HUE_FALLBACK).0;
const DIFF_GREEN_HUE_DEG: f32 = rgb2hsv(DIFF_GREEN).0;

#[inline]
const fn theme_hue(cfg: &RainConfig) -> f32 {
    let (h0, s0, v0) = rgb2hsv(cfg.theme_fg);
    if s0 * v0 >= THEME_HUE_MIN_CHROMA {
        return h0;
    }
    let (h1, s1, v1) = rgb2hsv(cfg.theme_accent);
    if s1 * v1 >= THEME_HUE_MIN_CHROMA {
        return h1;
    }
    THEME_HUE_FALLBACK_DEG
}

#[inline]
const fn rem_euclid_360(x: f32) -> f32 {
    let r = x % 360.0;
    if r < 0.0 {
        let s = r + 360.0;
        if s == 360.0 { 0.0 } else { s }
    } else {
        r
    }
}

#[inline]
const fn ramp_hue(cfg: &RainConfig) -> f32 {
    let hue = match cfg.hue {
        RainHue::Matrix => MATRIX_HUE_DEG,
        RainHue::Theme => theme_hue(cfg),
        RainHue::Custom(c) => rgb2hsv(c).0,
    };
    let diff = DIFF_GREEN_HUE_DEG;
    let mut d = rem_euclid_360(hue - diff);
    if d > 180.0 {
        d -= 360.0;
    }
    if d.abs() >= HUE_SEPARATION_DEG {
        return hue;
    }
    if d >= 0.0 {
        diff + HUE_SEPARATION_DEG
    } else {
        diff - HUE_SEPARATION_DEG
    }
}

#[inline]
const fn build_ramp(hue: f32, cfg: &RainConfig) -> [u32; 16] {
    const INV_15: f32 = 1.0 / 15.0;
    let light_bg = relative_luminance(cfg.default_bg) > 0.5;
    let mut ramp = [0u32; 16];
    let mut i = 0;
    while i < 16 {
        let t = i as f32 * INV_15;
        let (s, v) = if light_bg {
            (0.85 - 0.25 * t, 0.62 - 0.42 * t)
        } else {
            (0.9 - 0.55 * t * t, 0.28 + 0.72 * t)
        };
        ramp[i] = hsv2rgb(hue, s, v);
        i += 1;
    }
    ramp
}

#[inline]
const fn luminance_match(mut ramp: [u32; 16], reference: [u32; 16]) -> [u32; 16] {
    let table = crate::color_math::relative_luminance_table();
    let mut i = 0;
    while i < 16 {
        let lt = crate::color_math::relative_luminance_with(table, reference[i]);
        let (h, s, mut v) = rgb2hsv(ramp[i]);
        let mut step = 0;
        while step < 8 {
            step += 1;
            let l = crate::color_math::relative_luminance_with(table, ramp[i]);
            if (l - lt).abs() < 0.01 {
                break;
            }
            v = (v * (lt + 0.02) / (l + 0.02)).clamp(0.02, 1.0);
            ramp[i] = hsv2rgb(h, s, v);
        }
        i += 1;
    }
    ramp
}

/// Composite `tint` at `alpha` OVER `bg`, integer sRGB-side per channel —
/// the CPU stamp's `(c·a + b·(255-a) + 127)/255` rounding, so the invariant
/// is checked on the bytes the renderer actually lands.
#[inline]
const fn srgb_over(tint: u32, alpha: u8, bg: u32) -> u32 {
    let a = alpha as u32;
    let inv = 255 - a;
    let r = (((tint >> 16) & 0xFF) * a + ((bg >> 16) & 0xFF) * inv + 127) / 255;
    let g = (((tint >> 8) & 0xFF) * a + ((bg >> 8) & 0xFF) * inv + 127) / 255;
    let b = ((tint & 0xFF) * a + (bg & 0xFF) * inv + 127) / 255;
    (r << 16) | (g << 8) | b
}

#[inline]
fn faint_ink(fg: u32, bg: u32) -> u32 {
    let fg_rgb = aterm_types::Rgb::new((fg >> 16) as u8, (fg >> 8) as u8, fg as u8);
    let bg_rgb = aterm_types::Rgb::new((bg >> 16) as u8, (bg >> 8) as u8, bg as u8);
    let dim =
        aterm_core::terminal::color_resolve::dim_toward_bg(fg_rgb, bg_rgb, aterm_types::DIM_FACTOR);
    (u32::from(dim.r) << 16) | (u32::from(dim.g) << 8) | u32::from(dim.b)
}

#[inline]
fn derive_alphas(cfg: &RainConfig, ramp: &[u32; 16]) -> (u8, u8) {
    #[inline]
    const fn head_ok(
        table: &crate::color_math::RelativeLuminanceTable,
        tint15: u32,
        bg: u32,
        lb: f32,
        bound: f32,
        a: u8,
    ) -> bool {
        (crate::color_math::relative_luminance_with(table, srgb_over(tint15, a, bg)) - lb).abs()
            < bound
    }
    #[inline]
    const fn level_ok(
        table: &crate::color_math::RelativeLuminanceTable,
        ramp: &[u32; 16],
        bg: u32,
        lb: f32,
        bound: f32,
        body: u8,
    ) -> bool {
        let mut lvl = 16u32;
        while lvl > 0 {
            lvl -= 1;
            let raw_a = (body as u32 * (lvl + 1)) / 16;
            let a = (if raw_a > 1 { raw_a } else { 1 }) as u8;
            if (crate::color_math::relative_luminance_with(
                table,
                srgb_over(ramp[lvl as usize], a, bg),
            ) - lb)
                .abs()
                >= bound
            {
                return false;
            }
        }
        true
    }
    let table = crate::color_math::relative_luminance_table();
    let bg = cfg.default_bg;
    let lb = crate::color_math::relative_luminance_with(table, bg);
    let plain =
        (0.5 * (crate::color_math::relative_luminance_with(table, cfg.theme_fg) - lb)).abs();
    let faint =
        (crate::color_math::relative_luminance_with(table, faint_ink(cfg.theme_fg, bg)) - lb).abs();
    let bound = plain.min(faint);
    let derived_head = if cfg.head_alpha_override.is_none() || cfg.alpha_override.is_none() {
        let mut h = RAIN_ALPHA_CAP;
        loop {
            if head_ok(table, ramp[15], bg, lb, bound, h) || h == RAIN_ALPHA_FLOOR {
                break h;
            }
            h -= 1;
        }
    } else {
        RAIN_ALPHA_CAP
    };
    let mut body = match cfg.alpha_override {
        Some(b) => b,
        None => {
            // `level_ok(body)` checks `lvl == 15` (`a == body`) first, which is `head_ok`,
            // so no `body > derived_head` can satisfy `level_ok`.
            let mut b = derived_head;
            loop {
                if level_ok(table, ramp, bg, lb, bound, b) || b == RAIN_ALPHA_FLOOR {
                    break b;
                }
                b -= 1;
            }
        }
    };
    let head = match cfg.head_alpha_override {
        Some(h) => h.clamp(body, RAIN_ALPHA_CAP),
        None => {
            if derived_head < body {
                body = derived_head;
            }
            derived_head.max(body)
        }
    };
    (body, head)
}

/// Which of the 64 dynamic-ROM slots change character between two sorted slot
/// tables (bit `i` ⇒ slot `i` must be re-authored and re-baked).
#[inline]
const fn slot_change_mask(old: &[char], new: &[char]) -> u64 {
    let mut mask = 0u64;
    let mut slot = 0;
    while slot < rom::ROM_GLYPHS {
        let same = match (slot < old.len(), slot < new.len()) {
            (true, true) => old[slot] == new[slot],
            (false, false) => true,
            _ => false,
        };
        if !same {
            mask |= 1u64 << slot;
        }
        slot += 1;
    }
    mask
}

#[inline]
const fn fingerprint(quads: &[RainCell], add: &[RainHalo], tick: u64, atlas_version: u64) -> u64 {
    if quads.is_empty() && add.is_empty() {
        return 0;
    }
    let mut h = fnv_mix_u64(FNV64_OFFSET, atlas_version);
    let mut i = 0;
    while i < quads.len() {
        let q = &quads[i];
        let a = (q.row as u64) | ((q.x as u64) << 16) | ((q.y as u64) << 32) | ((q.w as u64) << 48);
        let b = (q.h as u64) | ((q.glyph as u64) << 16);
        let c = (q.tint as u64) | ((q.alpha as u64) << 48) | ((q.flip_x as u64) << 56);
        h = fnv_mix_u64(h, a);
        h = fnv_mix_u64(h, b);
        h = fnv_mix_u64(h, c);
        i += 1;
    }
    h = fnv_mix_u64(h, tick.wrapping_mul(0x9E37_79B1));
    let mut j = 0;
    while j < add.len() {
        let g = &add[j];
        let a = (g.row as u64) | ((g.x as u64) << 16) | ((g.y as u64) << 32) | ((g.w as u64) << 48);
        let b = (g.h as u64) | ((g.color as u64) << 16);
        h = fnv_mix_u64(h, a);
        h = fnv_mix_u64(h, b);
        j += 1;
    }
    h
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use super::*;

    const fn cfg_on() -> RainConfig {
        RainConfig {
            enabled: true,
            output_material: false,
            seed: 7,
            ..RainConfig::DEFAULT
        }
    }

    const fn literal_cfg_on() -> RainConfig {
        RainConfig {
            output_material: true,
            ..cfg_on()
        }
    }

    const fn geom(rows: u16, cols: u16, cw: u16, ch: u16) -> EffectGeom {
        EffectGeom {
            cell_w: cw,
            cell_h: ch,
            rows,
            cols,
        }
    }

    const fn space_cell(bg: [u8; 3]) -> RenderCell {
        RenderCell {
            ch: ' ',
            fg: [0xD0, 0xD0, 0xD0],
            bg,
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
        }
    }

    const BG: u32 = 0x0011_1318;

    const fn bg3() -> [u8; 3] {
        [0x11, 0x13, 0x18]
    }

    fn scan_empty(e: &mut MatrixRain, rows: usize, cols: usize) {
        let cells = vec![vec![space_cell(bg3()); cols]; rows];
        let sizes = vec![LineSize::SingleWidth; rows];
        e.rescan_from_cells(&cells, &sizes, &[], rows, cols, BG, 1);
    }

    const fn idle() -> RainTickInput<'static> {
        RainTickInput::EMPTY
    }

    const fn pour(e: &mut MatrixRain) {
        e.density_byte = 252;
        e.ring.push(e.tick, 252);
    }

    #[test]
    fn emits_over_trimmed_blank_rows() {
        let (rows, cols) = (10usize, 40usize);
        let block = RenderCell {
            ch: '█',
            ..space_cell(bg3())
        };
        let cells = vec![vec![block; 12]];
        let sizes = vec![LineSize::SingleWidth; rows];
        let mut e = MatrixRain::new(RainConfig {
            enabled: true,
            density: 12,
            output_material: false,
            seed: 0xA7E2_11D3,
            default_bg: BG,
            ..RainConfig::default()
        });
        e.rescan_from_cells(&cells, &sizes, &[], rows, cols, BG, 1);
        pour(&mut e);
        let g = EffectGeom {
            cell_w: 9,
            cell_h: 18,
            rows: rows as u16,
            cols: cols as u16,
        };
        let (mut q, mut a) = (Vec::new(), Vec::new());
        let mut any = false;
        for _ in 0..20 {
            e.note_activity(e.tick + 1);
            step(&mut e, g, &idle(), 33, &mut q, &mut a);
            if !q.is_empty() {
                any = true;
            }
            assert!(
                q.iter().all(|quad| !(quad.row == 0 && (quad.x / 9) < 12)),
                "rain painted over the occupied block glyphs on row 0"
            );
        }
        assert!(any, "rain must emit over the trimmed blank rows");
    }

    fn step(
        e: &mut MatrixRain,
        g: EffectGeom,
        input: &RainTickInput<'_>,
        dt: u64,
        q: &mut Vec<RainCell>,
        a: &mut Vec<RainHalo>,
    ) -> u64 {
        e.advance_ms(dt);
        e.emit(g, input, q, a)
    }

    fn select_all(rows: i32, cols: u16) -> aterm_core::selection::TextSelection {
        use aterm_core::selection::{SelectionSide, SelectionType};
        let mut s = aterm_core::selection::TextSelection::new();
        s.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
        s.update_selection(rows - 1, cols - 1, SelectionSide::Right);
        s.complete_selection();
        s
    }

    #[test]
    fn deterministic_across_engines_and_clock_paths() {
        let g = geom(40, 60, 8, 16);
        let mut a = MatrixRain::new(cfg_on());
        let mut b = MatrixRain::new(cfg_on());
        scan_empty(&mut a, 40, 60);
        scan_empty(&mut b, 40, 60);
        let t0 = Instant::now();
        let (mut qa, mut aa) = (Vec::new(), Vec::new());
        let (mut qb, mut ab) = (Vec::new(), Vec::new());
        let mut rained = false;
        for i in 0..200u64 {
            if i % 3 == 0 {
                a.note_activity(i);
                b.note_activity(i);
            }
            if i % 7 == 0 {
                a.note_keystroke();
                b.note_keystroke();
            }
            let now = t0 + Duration::from_millis(i * 33);
            let fa = a.tick(now, g, &idle(), &mut qa, &mut aa);
            let fb = step(
                &mut b,
                g,
                &idle(),
                if i == 0 { 0 } else { 33 },
                &mut qb,
                &mut ab,
            );
            assert_eq!(fa, fb, "instant vs dt fingerprint diverged at step {i}");
            assert_eq!(qa, qb, "instant vs dt quads diverged at step {i}");
            assert_eq!(aa, ab, "instant vs dt halos diverged at step {i}");
            rained |= !qa.is_empty();
        }
        assert!(rained, "non-vacuous: the drive actually rained");
    }

    #[test]
    fn budget_bound_holds_and_truncation_fires() {
        for (cw, ch) in [(4u16, 8u16), (20, 40)] {
            let cap = quad_cap(u32::from(cw), u32::from(ch));
            let g = geom(200, 500, cw, ch);
            let mut e = MatrixRain::new(RainConfig {
                density: 12,
                ..cfg_on()
            });
            scan_empty(&mut e, 200, 500);
            pour(&mut e);
            let (mut q, mut a) = (Vec::new(), Vec::new());
            let mut hit_cap = false;
            for _ in 0..60 {
                e.note_keystroke();
                step(&mut e, g, &idle(), 83, &mut q, &mut a);
                assert!(q.len() <= cap, "quad budget exceeded: {} > {cap}", q.len());
                assert!(a.len() <= MAX_RAIN_ADD, "halo budget exceeded");
                if q.len() >= cap - 202 {
                    hit_cap = true;
                }
            }
            assert!(hit_cap, "truncation branch never exercised at {cw}x{ch}");
        }
    }

    #[test]
    fn step_stable_damage_shape() {
        let g = geom(50, 40, 8, 16);
        let mut e = MatrixRain::new(RainConfig {
            density: 12,
            ..cfg_on()
        });
        scan_empty(&mut e, 50, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for i in 0..300u64 {
            e.note_activity(i + 1);
            step(&mut e, g, &idle(), 33, &mut q, &mut a);
        }
        assert_eq!(e.weather, RainWeather::Working);
        let mut seq = 1000u64;
        loop {
            let t = e.tick;
            if !q.is_empty()
                && !(t + 1).is_multiple_of(u64::from(e.mq))
                && !(t + 1).is_multiple_of(u64::from(e.dq))
                && !(t + 1).is_multiple_of(u64::from(e.sq))
            {
                break;
            }
            seq += 1;
            e.note_activity(seq);
            step(&mut e, g, &idle(), 33, &mut q, &mut a);
            assert!(seq < 1200, "never found an aligned raining tick");
        }
        let snap = |quads: &[RainCell]| {
            let mut m: HashMap<u16, HashMap<u16, RainCell>> = HashMap::new();
            for s in quads {
                m.entry(s.x / 8).or_default().insert(s.row, *s);
            }
            m
        };
        let t_first = snap(&q);
        let tick_before = e.tick;
        e.note_activity(seq + 100);
        step(&mut e, g, &idle(), 33, &mut q, &mut a);
        assert_eq!(e.tick, tick_before + 1, "exactly one engine tick elapsed");
        let t_second = snap(&q);
        let fp = FieldParams {
            seed32: e.seed32,
            rows: 50,
            tick_ms: e.tick_ms,
            speed: 5,
            trail: 5,
            mq: e.mq,
            dq: e.dq,
            sq: e.sq,
        };
        let empty = HashMap::new();
        for col in 0..40u16 {
            let p = col_params(&fp, u32::from(col)).p;
            let before = t_first.get(&col).unwrap_or(&empty);
            let after = t_second.get(&col).unwrap_or(&empty);
            let mut changed = 0usize;
            for r in 0..50u16 {
                if before.get(&r) != after.get(&r) {
                    changed += 1;
                }
            }
            let bound = 3 + 15usize.div_ceil(p as usize);
            assert!(
                changed <= bound,
                "col {col}: {changed} changed cells > bound {bound} (p={p})"
            );
        }
    }

    fn hue_gap(a: f32, b: f32) -> f32 {
        let d = (a - b).rem_euclid(360.0);
        d.min(360.0 - d)
    }

    #[test]
    fn theme_hue_on_a_neutral_fg_is_not_the_failure_tint() {
        let cfg = RainConfig {
            hue: RainHue::Theme,
            ..cfg_on()
        };
        assert!(
            hue_gap(rgb2hsv(cfg.theme_fg).0, FAIL_HUE_DEG) < 30.0,
            "negative control: the default fg's own hue already avoids the failure tint"
        );
        let hue = ramp_hue(&cfg);
        assert!(
            hue_gap(hue, FAIL_HUE_DEG) >= 30.0,
            "default-theme rain hue {hue:.1}° is within 30° of the failure tint"
        );
        let solarized = RainConfig {
            theme_fg: 0x0065_7B83,
            ..cfg
        };
        assert!(hue_gap(ramp_hue(&solarized), rgb2hsv(0x0065_7B83).0) < 0.5);
        let monochrome = RainConfig {
            theme_fg: 0x00C0_C0C0,
            theme_accent: 0x00FF_FFFF,
            ..cfg
        };
        let blue = ramp_hue(&monochrome);
        assert!(hue_gap(blue, rgb2hsv(THEME_HUE_FALLBACK).0) < 0.5);
        assert!(hue_gap(blue, FAIL_HUE_DEG) >= 30.0);
    }

    #[test]
    fn luminance_invariant_on_both_stock_themes() {
        for (bg, fg, name) in [
            (0x0011_1318u32, 0x00D0_D0D0u32, "stock dark"),
            (0x00FF_FFFFu32, 0x001F_2328u32, "GitHub Light"),
        ] {
            let e = MatrixRain::new(RainConfig {
                default_bg: bg,
                theme_fg: fg,
                ..cfg_on()
            });
            let lb = relative_luminance(bg);
            let bound = (0.5 * (relative_luminance(fg) - lb)).abs();
            assert!(
                (RAIN_ALPHA_FLOOR..=RAIN_ALPHA_CAP).contains(&e.body_alpha),
                "{name}: body alpha out of range"
            );
            assert!(
                e.head_alpha >= e.body_alpha && e.head_alpha <= RAIN_ALPHA_CAP,
                "{name}: head alpha out of clamp"
            );
            for lvl in 0..=15u32 {
                let dl = (relative_luminance(srgb_over(e.ramp[lvl as usize], e.alpha_of(lvl), bg))
                    - lb)
                    .abs();
                assert!(
                    dl < bound,
                    "{name}: level {lvl} breaks the invariant ({dl} >= {bound})"
                );
            }
            let dh = (relative_luminance(srgb_over(e.ramp[15], e.head_alpha, bg)) - lb).abs();
            assert!(
                dh < bound,
                "{name}: head breaks the invariant ({dh} >= {bound})"
            );
        }
    }

    #[test]
    fn rain_stays_under_real_faint_ink() {
        use aterm_core::terminal::color_resolve::dim_toward_bg;
        let pack =
            |c: aterm_types::Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
        let (mut lifted, mut receded) = (false, false);
        for name in [
            "Default",
            "Aterm Night",
            "Aterm Day",
            "Linen",
            "Default Light",
        ] {
            let scheme = aterm_types::scheme::builtin(name).expect("built-in");
            let (fg, bg) = (scheme.foreground, scheme.background);
            let faint = dim_toward_bg(fg, bg, aterm_types::DIM_FACTOR);
            let (fg, bg, faint) = (pack(fg), pack(bg), pack(faint));
            let lb = relative_luminance(bg);
            let plain = (0.5 * (relative_luminance(fg) - lb)).abs();
            let real = (relative_luminance(faint) - lb).abs();
            lifted |= real > plain * 1.5;
            receded |= real < plain * 0.7;
            let e = MatrixRain::new(RainConfig {
                default_bg: bg,
                theme_fg: fg,
                ..cfg_on()
            });
            for lvl in 0..=15u32 {
                let dl = (relative_luminance(srgb_over(e.ramp[lvl as usize], e.alpha_of(lvl), bg))
                    - lb)
                    .abs();
                assert!(dl < real, "{name}: level {lvl} at {dl} >= faint {real}");
            }
            let dh = (relative_luminance(srgb_over(e.ramp[15], e.head_alpha, bg)) - lb).abs();
            assert!(dh < real, "{name}: head at {dh} >= faint {real}");
        }
        assert!(
            lifted,
            "a light theme's faint ink sits past the plain blend"
        );
        assert_eq!(
            receded,
            aterm_core::terminal::color_resolve::FAINT_SRGB_BLEND,
            "a dark theme's faint ink sits short of the plain blend exactly where it is the \
             sRGB blend"
        );
    }

    #[test]
    fn alpha_cap_is_pinned() {
        let e = MatrixRain::new(RainConfig {
            alpha_override: Some(200),
            head_alpha_override: Some(3),
            ..cfg_on()
        });
        assert_eq!(e.body_alpha, RAIN_ALPHA_CAP);
        assert_eq!(e.head_alpha, RAIN_ALPHA_CAP, "head clamps up to body");
    }

    #[test]
    fn hidden_pane_drains_immediately() {
        let g = geom(60, 80, 8, 16);
        let mut e = MatrixRain::new(RainConfig {
            density: 12,
            ..cfg_on()
        });
        scan_empty(&mut e, 60, 80);
        pour(&mut e);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for i in 0..40u64 {
            e.note_activity(i + 1);
            step(&mut e, g, &idle(), 33, &mut q, &mut a);
        }
        assert!(!q.is_empty(), "engine should be raining before the drain");
        e.set_visibility(RainVisibility::Hidden);
        assert_eq!(step(&mut e, g, &idle(), 83, &mut q, &mut a), 0);
        assert!(q.is_empty(), "hidden pane drains immediately");
    }

    #[test]
    fn fingerprint_liveness_and_stability() {
        let g = geom(40, 60, 8, 16);
        let mut e = MatrixRain::new(RainConfig {
            density: 12,
            ..cfg_on()
        });
        scan_empty(&mut e, 40, 60);
        pour(&mut e);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        let mut prev = step(&mut e, g, &idle(), 0, &mut q, &mut a);
        let mut seen_nonempty = 0;
        for _ in 0..30 {
            e.note_keystroke();
            let fp = step(&mut e, g, &idle(), 83, &mut q, &mut a);
            if !q.is_empty() {
                seen_nonempty += 1;
                assert_ne!(fp, prev, "animating fp must change every tick");
                assert_ne!(fp, 0, "nonempty emission is never fp 0");
            }
            prev = fp;
        }
        assert!(seen_nonempty >= 10, "liveness test needs a raining field");
    }

    #[test]
    fn even_quad_counts_do_not_cancel() {
        let quad = RainCell {
            row: 3,
            x: 8,
            y: 48,
            w: 8,
            h: 16,
            glyph: 0,
            tint: 0x0022_CC44,
            alpha: 90,
            flip_x: true,
        };
        let pair = [quad, quad];
        assert_ne!(fingerprint(&pair, &[], 10, 1), 0);
        assert_ne!(
            fingerprint(&pair, &[], 10, 1),
            fingerprint(&pair, &[], 11, 1),
            "the frame term must survive an even quad count"
        );
        assert_eq!(fingerprint(&[], &[], 10, 1), 0, "empty emission is fp 0");
    }

    #[test]
    fn occupancy_mask_predicates() {
        let mut e = MatrixRain::new(cfg_on());
        let rows = 12usize;
        let cols = 8usize;
        let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
        cells[0][1].ch = 'x';
        cells[2][2].ch = '　';
        cells[2][3].wide = true;
        cells[2][4].underline = UnderlineStyle::Single;
        cells[2][5].bg = [0x30, 0x30, 0x30];
        cells[2][6].strikethrough = true;
        cells[2][7].overline = true;
        let mut sizes = vec![LineSize::SingleWidth; rows];
        sizes[1] = LineSize::DoubleWidth;
        let img_row: Vec<(usize, ImageRef)> = Vec::new();
        let mut images = vec![img_row; rows];
        images[3].push((
            3,
            ImageRef {
                image: std::sync::Arc::new(aterm_core::grid::extra::ImageData {
                    bytes: Vec::new(),
                    format: aterm_core::grid::extra::ImageFormat::Png,
                    cols: 1,
                    rows: 1,
                    z_index: 0,
                    band_lift_px: 0,
                    cell_offset_px: (0, 0),
                    scaling: aterm_core::grid::extra::ImageScaling::Fit,
                    source_rect: None,
                }),
                cell_row: 0,
                cell_col: 0,
                kitty: None,
            },
        ));
        e.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 1);
        let band = [11u16];
        let inp = RainTickInput {
            hidden_band: &band,
            ..RainTickInput::default()
        };
        assert!(
            e.cell_eligible(0, 4, &inp),
            "distant plain space is eligible"
        );
        assert!(
            !e.cell_eligible(0, 0, &inp),
            "one-cell gap beside a glyph is semantic clearance"
        );
        assert!(!e.cell_eligible(0, 1, &inp), "glyph cell is masked");
        assert!(!e.cell_eligible(1, 0, &inp), "DECDWL row is wholly masked");
        assert!(!e.cell_eligible(2, 2, &inp), "wide lead is masked");
        assert!(!e.cell_eligible(2, 3, &inp), "wide continuation is masked");
        assert!(!e.cell_eligible(2, 4, &inp), "underlined cell is masked");
        assert!(!e.cell_eligible(2, 5, &inp), "non-default bg is masked");
        assert!(!e.cell_eligible(2, 6, &inp), "strikethrough cell is masked");
        assert!(!e.cell_eligible(2, 7, &inp), "overlined cell is masked");
        assert!(!e.cell_eligible(3, 3, &inp), "image span is masked");
        assert!(
            !e.cell_eligible(3, 4, &inp),
            "cell beside the image is halo clearance"
        );
        assert!(e.cell_eligible(4, 6, &inp), "distant cell remains eligible");
    }

    #[test]
    fn closed_tui_frame_protects_default_background_interior() {
        let (rows, cols) = (14usize, 26usize);
        let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
        cells[1][1].ch = '╭';
        cells[1][22].ch = '╮';
        for cell in &mut cells[1][2..22] {
            cell.ch = '─';
        }
        for (offset, ch) in "Claude Code".chars().enumerate() {
            cells[1][5 + offset].ch = ch;
        }
        for row in cells.iter_mut().take(9).skip(2) {
            row[1].ch = '│';
            row[22].ch = '│';
        }
        cells[9][1].ch = '╰';
        cells[9][22].ch = '╯';
        for cell in &mut cells[9][2..22] {
            cell.ch = '─';
        }
        let sizes = vec![LineSize::SingleWidth; rows];
        let images = vec![Vec::new(); rows];
        let mut rain = MatrixRain::new(cfg_on());
        rain.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 1);
        let band = [13u16];
        let input = RainTickInput {
            hidden_band: &band,
            ..RainTickInput::default()
        };
        assert!(
            !rain.cell_eligible(5, 12, &input),
            "blank cells inside a closed titled panel are semantic UI"
        );
        assert!(
            rain.cell_eligible(11, 12, &input),
            "open terminal field below the panel remains rain-eligible"
        );

        for cell in &mut cells[9][2..22] {
            cell.ch = 'x';
        }
        rain.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 2);
        assert!(
            rain.cell_eligible(5, 12, &input),
            "corner glyphs around text must not close a framed surface"
        );

        cells[9][1].ch = ' ';
        cells[9][22].ch = ' ';
        for cell in &mut cells[9][2..22] {
            cell.ch = ' ';
        }
        rain.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 3);
        assert!(
            rain.cell_eligible(5, 12, &input),
            "an unclosed drawing must not hide ordinary terminal space"
        );
    }

    #[test]
    fn frame_detector_handles_many_unmatched_left_corners_without_masking() {
        let (rows, cols) = (8usize, 1024usize);
        let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
        for col in (0..cols).step_by(2) {
            cells[1][col].ch = '╭';
        }
        let sizes = vec![LineSize::SingleWidth; rows];
        let images = vec![Vec::new(); rows];
        let mut rain = MatrixRain::new(cfg_on());
        rain.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 1);
        let band = [7u16];
        let input = RainTickInput {
            hidden_band: &band,
            ..RainTickInput::default()
        };
        assert!(
            rain.cell_eligible(4, 511, &input),
            "unmatched top-left glyphs are not a closed UI surface"
        );
    }

    #[test]
    fn frame_region_overflow_fails_closed_for_the_rescan() {
        let (rows, cols) = ((MAX_FRAME_REGIONS + 1) * 3, 10usize);
        let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
        let draw = |cells: &mut [Vec<RenderCell>], index: usize| {
            let top = index * 3;
            cells[top][0].ch = '╭';
            cells[top][1].ch = '─';
            cells[top][2].ch = '╮';
            cells[top + 1][0].ch = '│';
            cells[top + 1][2].ch = '│';
            cells[top + 2][0].ch = '╰';
            cells[top + 2][1].ch = '─';
            cells[top + 2][2].ch = '╯';
        };
        for index in 0..MAX_FRAME_REGIONS {
            draw(&mut cells, index);
        }
        let sizes = vec![LineSize::SingleWidth; rows];
        let images = vec![Vec::new(); rows];
        let band = [(rows - 1) as u16];
        let input = RainTickInput {
            hidden_band: &band,
            ..RainTickInput::default()
        };
        let mut rain = MatrixRain::new(cfg_on());
        rain.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 1);
        assert!(
            rain.cell_eligible(100, 8, &input),
            "the bounded detector preserves open field at its exact capacity"
        );

        draw(&mut cells, MAX_FRAME_REGIONS);
        rain.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 2);
        assert!(
            !rain.cell_eligible(100, 8, &input),
            "a prospective 65th region suppresses rain instead of leaking into UI"
        );
    }

    #[test]
    fn material_bank_samples_output_not_typing() {
        let rows = 6usize;
        let cols = 8usize;
        let mut e = MatrixRain::new(literal_cfg_on());
        let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
        for (i, ch) in "0f3a".chars().enumerate() {
            cells[0][i].ch = ch;
        }
        for (i, ch) in "hunter2".chars().enumerate() {
            cells[3][i].ch = ch;
        }
        e.sample_material(&cells, rows, Some((3, 0)), &[]);
        assert_eq!(e.material.len(), 4, "only the hex row was sampled");
        let decoded = |engine: &MatrixRain| -> String {
            engine
                .material
                .iter()
                .map(|&slot| engine.material_chars[usize::from(slot)])
                .collect()
        };
        assert_eq!(decoded(&e), "0f3a", "literal source order is retained");
        assert_eq!(e.material_chars, vec!['0', '3', 'a', 'f']);

        e.sample_material(&cells, rows, None, &[3]);
        assert_eq!(e.material.len(), 4, "hidden band excludes the secret row");
        assert_eq!(decoded(&e), "0f3a");

        let mut bottom_secret = vec![vec![space_cell(bg3()); cols]; rows];
        for (i, ch) in "hunter2".chars().enumerate() {
            bottom_secret[rows - 1][i].ch = ch;
        }
        e.sample_material(&bottom_secret, rows, None, &[]);
        assert!(
            e.material.is_empty(),
            "unknown composer band ⇒ the bottom rows are never sampled"
        );

        e.sample_material(&cells, rows, Some((5, 0)), &[]);
        assert!(!e.material.is_empty());
        let mut off = e.cfg;
        off.output_material = false;
        e.set_config(off);
        assert!(
            e.material.is_empty(),
            "output_material=false takes effect on the live reload"
        );
        let mut back_on = e.cfg;
        back_on.output_material = true;
        e.set_config(back_on);
        assert!(
            e.needs_material_sample(),
            "classic-to-literal reload requests a host sample without damage"
        );
        e.sample_material(&cells, rows, Some((5, 0)), &[]);
        assert!(!e.needs_material_sample());

        let mut cfg = cfg_on();
        cfg.output_material = false;
        let mut e2 = MatrixRain::new(cfg);
        e2.sample_material(&cells, rows, Some((3, 0)), &[]);
        assert!(
            e2.material.is_empty(),
            "output_material=false stays classic"
        );
    }

    #[test]
    fn literal_material_slots_are_bounded_stable_and_never_faked() {
        let rows = 4usize;
        let mut e = MatrixRain::new(literal_cfg_on());
        let mut cells = vec![vec![space_cell(bg3()); 96]; rows];
        for (cell, ch) in cells[0].iter_mut().zip('!'..='~') {
            cell.ch = ch;
        }
        e.sample_material(&cells, rows, Some((3, 0)), &[]);
        assert!(e.material_chars.len() <= MATERIAL_GLYPH_CAP);
        assert!(
            e.material
                .iter()
                .all(|&slot| usize::from(slot) < e.material_chars.len())
        );
        assert!(
            e.material_chars.iter().any(char::is_ascii_lowercase),
            "recent-slot selection must not sort/truncate lowercase away"
        );

        let mut small = vec![vec![space_cell(bg3()); 8]; rows];
        for (i, ch) in "Codex".chars().enumerate() {
            small[0][i].ch = ch;
        }
        e.sample_material(&small, rows, Some((3, 0)), &[]);
        let version = e.baker.version();
        let slots = e.material_chars.clone();
        for (i, ch) in "xedoC".chars().enumerate() {
            small[0][i].ch = ch;
        }
        e.sample_material(&small, rows, Some((3, 0)), &[]);
        assert_eq!(e.material_chars, slots, "same charset keeps stable slots");
        assert_eq!(
            e.baker.version(),
            version,
            "reordering/frequency alone never rebakes the atlas"
        );

        small[0][0].ch = '🐈';
        for cell in &mut small[0][1..] {
            cell.ch = ' ';
        }
        e.sample_material(&small, rows, Some((3, 0)), &[]);
        assert!(e.material.is_empty(), "unsupported glyph is omitted");
        assert!(
            e.material_chars.is_empty(),
            "unsupported glyph is never substituted"
        );
    }

    #[test]
    fn literal_material_tail_matches_an_exhaustive_forward_walk() {
        fn forward_tail(
            cells: &[Vec<RenderCell>],
            rows: usize,
            cursor: Option<(u16, u16)>,
            hidden_band: &[u16],
        ) -> Vec<char> {
            let mut all: Vec<char> = Vec::new();
            for r in 0..rows {
                let row = r as u16;
                let cursor_banded =
                    cursor.is_some_and(|(cr, _)| i32::from(cr.abs_diff(row)) <= CURSOR_BAND_ROWS);
                let remembered_banded = hidden_band.contains(&row);
                let fallback_banded = cursor.is_none()
                    && hidden_band.is_empty()
                    && r + HIDDEN_CURSOR_BAND_ROWS >= rows;
                if cursor_banded || remembered_banded || fallback_banded {
                    continue;
                }
                let Some(row_cells) = cells.get(r) else {
                    continue;
                };
                for cell in row_cells {
                    if cell.ch != ' ' && !cell.wide && rom::material_bitmap(cell.ch).is_some() {
                        all.push(cell.ch);
                    }
                }
            }
            if all.len() > MATERIAL_CAP {
                all.drain(..all.len() - MATERIAL_CAP);
            }
            all
        }

        let (rows, cols) = (16usize, 24usize);
        let pattern = ['A', 'b', '3', '{', '}', 'z', '7'];
        let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
        for (r, row_cells) in cells.iter_mut().enumerate() {
            for (c, cell) in row_cells.iter_mut().enumerate() {
                match (r + c) % 7 {
                    0 => {}
                    1 => cell.ch = '\u{1F408}',
                    2 => {
                        cell.ch = pattern[(r * cols + c) % pattern.len()];
                        cell.wide = true;
                    }
                    _ => cell.ch = pattern[(r * cols + c) % pattern.len()],
                }
            }
        }
        cells[9].truncate(5);
        cells.truncate(rows - 1);

        for (cursor, band) in [
            (Some((2u16, 0u16)), &[][..]),
            (Some((2, 0)), &[7u16, 8][..]),
            (None, &[][..]),
            (None, &[11u16][..]),
        ] {
            let mut e = MatrixRain::new(literal_cfg_on());
            e.sample_material(&cells, rows, cursor, band);
            let expected = forward_tail(&cells, rows, cursor, band);
            assert_eq!(
                e.material_scratch, expected,
                "bottom-up sampling must reproduce the forward walk's tail \
                 (cursor {cursor:?}, band {band:?})"
            );
            let sampled: Vec<char> = e
                .material
                .iter()
                .map(|&slot| e.material_chars[usize::from(slot)])
                .collect();
            assert_eq!(
                sampled, expected,
                "the emitted tape follows the same source order"
            );
        }
    }

    #[test]
    fn literal_mode_never_emits_a_decorative_fallback() {
        let (rows, cols) = (30usize, 40usize);
        let g = geom(rows as u16, cols as u16, 8, 16);
        let mut e = MatrixRain::new(literal_cfg_on());
        scan_empty(&mut e, rows, cols);
        let blank = vec![vec![space_cell(bg3()); cols]; rows];
        e.sample_material(&blank, rows, Some((rows as u16 - 1, 0)), &[]);
        pour(&mut e);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for _ in 0..30 {
            step(&mut e, g, &idle(), 33, &mut q, &mut a);
        }
        assert!(q.is_empty(), "no real material means no glyph rain");
        assert!(
            !e.is_active(),
            "empty literal mode must not keep a timer armed"
        );

        let mut real = blank.clone();
        for (cell, ch) in real[0].iter_mut().zip("Codex42".chars()) {
            cell.ch = ch;
        }
        e.sample_material(&real, rows, Some((rows as u16 - 1, 0)), &[]);
        for _ in 0..30 {
            step(&mut e, g, &idle(), 33, &mut q, &mut a);
        }
        assert!(
            !q.is_empty(),
            "supported literal output arms the real field"
        );
        assert!(q.iter().all(|quad| !quad.flip_x));

        e.sample_material(&blank, rows, Some((rows as u16 - 1, 0)), &[]);
        step(&mut e, g, &idle(), 33, &mut q, &mut a);
        assert!(
            q.is_empty(),
            "clearing real output cannot reveal fake glyphs"
        );
    }

    #[test]
    fn real_codex_and_claude_frames_feed_literal_output_not_composer() {
        const CLAUDE_FRAME: &[u8] = concat!(
            "\x1b[?1049h\x1b[?2026h\x1b[2J\x1b[H",
            "Claude Code v2.1.206",
            "\x1b[4;4HTips for getting started",
            "\x1b[7;4HWhat's new: directory path suggestions",
            "\x1b[11;1HSafe mode: customizations disabled",
            "\x1b[20;1H> write a test for lib.rs\x1b[20;26H",
            "\x1b[?2026l"
        )
        .as_bytes();
        const CODEX_FRAME: &[u8] = concat!(
            "\x1b[2J\x1b[HOpenAI Codex v0.144.0",
            "\x1b[4;3HWorking directory: /workspace/aterm",
            "\x1b[7;3HReviewing renderer and matrix output",
            "\x1b[10;3HTests completed successfully",
            "\x1b[20;1H> refactor the secret composer draft\x1b[20;35H"
        )
        .as_bytes();

        let sampled = |bytes: &[u8], expect_alt: bool| -> String {
            let rows = 20usize;
            let cols = 80usize;
            let mut term = aterm_core::terminal::Terminal::new(rows as u16, cols as u16);
            term.process(bytes);
            assert_eq!(term.is_alternate_screen(), expect_alt);
            let frame = term.cell_frame(rows, cols);
            let cursor = frame
                .cursor_visible
                .then_some((frame.cursor_row as u16, frame.cursor_col as u16));
            let mut rain = MatrixRain::new(literal_cfg_on());
            rain.sample_material(&frame.cells, rows, cursor, &[]);
            rain.material
                .iter()
                .map(|&slot| rain.material_chars[usize::from(slot)])
                .collect()
        };

        let claude = sampled(CLAUDE_FRAME, true);
        assert!(
            claude.contains("ClaudeCodev2.1.206"),
            "literal Claude header: {claude}"
        );
        assert!(claude.contains("directorypathsuggestions"));
        assert!(
            !claude.contains("writeatestforlib.rs"),
            "composer excluded: {claude}"
        );

        let codex = sampled(CODEX_FRAME, false);
        assert!(
            codex.contains("OpenAICodexv0.144.0"),
            "literal Codex header: {codex}"
        );
        assert!(codex.contains("Testscompletedsuccessfully"));
        assert!(
            !codex.contains("secretcomposerdraft"),
            "composer excluded: {codex}"
        );
    }

    #[test]
    fn emission_never_rains_bottom_band_when_ring_unknown() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        pour(&mut e);
        let input = RainTickInput {
            cursor: None,
            hidden_band: &[],
            ..RainTickInput::default()
        };
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for _ in 0..30 {
            step(&mut e, g, &input, 33, &mut q, &mut a);
        }
        assert!(!q.is_empty(), "field pours above the band");
        let floor = 30 - HIDDEN_CURSOR_BAND_ROWS as u16;
        assert!(
            q.iter().all(|quad| quad.row < floor),
            "no quad may land in the bottom-{HIDDEN_CURSOR_BAND_ROWS} fallback band"
        );
    }

    #[test]
    fn bell_and_exit_survive_suspension_activity_does_not() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        e.note_bell();
        e.note_exit_status(true);
        e.note_keystroke();
        for _ in 0..5 {
            e.advance_ms(83);
            e.step_suspended();
        }
        assert_eq!(e.bell_until_ms, 0, "not applied while suspended");
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert!(
            e.bell_until_ms > 0,
            "the bell alert survives the suspension"
        );
        assert!(e.fail_until_ms > 0, "the ember exit tint survives too");
        assert!(
            e.last_key_ms.is_none(),
            "activity notes were dropped, not deferred"
        );
    }

    #[test]
    fn unfocused_drained_engine_disarms_despite_streaming() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for i in 0..60u64 {
            e.note_activity(i + 1);
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        assert_eq!(e.weather, RainWeather::Working);
        e.set_visibility(RainVisibility::VisibleUnfocused);
        let mut inactive_at = None;
        for i in 0..120u64 {
            e.note_activity(1000 + i);
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
            if !e.is_active() {
                inactive_at = Some(i);
                break;
            }
        }
        assert!(
            inactive_at.is_some(),
            "unfocused pane must disarm after its bounded drain"
        );
        assert!(q.is_empty(), "drained frame is empty");
        e.set_visibility(RainVisibility::Focused);
        assert!(e.is_active(), "refocus resumes an awake weather");
    }

    #[test]
    fn can_emit_probe_tracks_visibility_and_motion() {
        let mut e = MatrixRain::new(cfg_on());
        assert!(e.can_emit(), "focused + full motion");
        e.set_reduced_motion(true);
        assert!(!e.can_emit(), "reduced motion cannot emit");
        e.set_reduced_motion(false);
        e.set_visibility(RainVisibility::Hidden);
        assert!(!e.can_emit(), "hidden pane past its (instant) drain");
        e.set_visibility(RainVisibility::Focused);
        assert!(e.can_emit(), "refocus resets the drain");
    }

    #[test]
    fn exit_status_drives_wave_and_ember() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        e.note_exit_status(false);
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert!(
            e.wave_pending || e.wave.is_some(),
            "success fires the finishing sweep"
        );

        let mut cfg = cfg_on();
        cfg.exit_tint = false;
        let mut e2 = MatrixRain::new(cfg);
        scan_empty(&mut e2, 30, 40);
        e2.note_exit_status(true);
        step(&mut e2, g, &idle(), 83, &mut q, &mut a);
        assert_eq!(e2.fail_until_ms, 0, "knob off ⇒ inert");
        assert!(!e2.wave_pending, "knob off ⇒ no sweep either");
    }

    #[test]
    fn material_remaps_emitted_glyphs_unmirrored() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(literal_cfg_on());
        scan_empty(&mut e, 30, 40);
        pour(&mut e);
        e.material_chars = vec!['7'];
        e.rom = Some(rom::rasterize_material_master(&e.material_chars));
        e.baker.restart();
        e.material = vec![0];
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for _ in 0..30 {
            step(&mut e, g, &idle(), 33, &mut q, &mut a);
        }
        assert!(!q.is_empty(), "field emits under pour");
        for quad in &q {
            assert_eq!(quad.glyph, 0, "every rained glyph is the material tile");
            assert!(!quad.flip_x, "exact-form tiles are never mirrored");
        }

        e.material_chars = vec!['2', '7'];
        e.rom = Some(rom::rasterize_material_master(&e.material_chars));
        e.baker.restart();
        e.material = vec![0u8; 64];
        e.material.extend(std::iter::repeat_n(1u8, 64));
        let (mut q2, mut a2) = (Vec::new(), Vec::new());
        for _ in 0..30 {
            step(&mut e, g, &idle(), 33, &mut q2, &mut a2);
        }
        let front = q2.iter().any(|qd| qd.glyph == 0);
        let back = q2.iter().any(|qd| qd.glyph == 1);
        assert!(
            front && back,
            "both table halves must rain (front={front}, back={back}, n={})",
            q2.len()
        );
    }

    #[test]
    fn literal_material_flows_in_source_order_down_each_column() {
        let mut e = MatrixRain::new(literal_cfg_on());
        e.baker.begin_frame(8, 16);
        e.material_chars = vec!['a', 'b', 'c'];
        e.material = vec![0, 1, 2];
        let input = RainTickInput::default();
        let ctx = EmitCtx {
            fp: FieldParams {
                seed32: e.seed32,
                rows: 8,
                tick_ms: 33,
                speed: 5,
                trail: 5,
                mq: 4,
                dq: 12,
                sq: 15,
            },
            geom: geom(8, 8, 8, 16),
            input: &input,
            cap: 64,
            pen: 0,
            ramp: e.ramp,
            wave_row: None,
            glyph_epoch: 0,
            dither_epoch: 0,
        };
        let start = Some(e.material_start(3));
        let a = e.cell_quad(&ctx, 3, 2, 10, false, start);
        let b = e.cell_quad(&ctx, 3, 3, 10, false, start);
        let slot_a = usize::from(a.glyph);
        let slot_b = usize::from(b.glyph);
        assert_eq!(slot_b, (slot_a + 1) % 3);
        assert!(!a.flip_x && !b.flip_x, "literal text is never mirrored");
    }

    #[test]
    fn semantic_pulses_are_bounded_prioritized_and_shape_real_tape_lanes() {
        let mut rain = MatrixRain::new(cfg_on());
        rain.material = (0..12).collect();

        rain.note_signal(RainSignal::Failure as u32, 99);
        rain.note_signal(RainSignal::Inspect as u32, 1);
        rain.apply_pending_notes();
        assert_eq!(rain.semantic_phase, RainSignal::Failure);
        assert_eq!(rain.semantic_energy, SEMANTIC_ENERGY_CAP);
        assert_eq!(rain.semantic_ticks_left, SEMANTIC_HOLD_TICKS);

        rain.note_signal(RainSignal::Inspect as u32, 4);
        rain.apply_pending_notes();
        assert_eq!(rain.semantic_phase, RainSignal::Inspect);
        let inspect_seq = rain.semantic_seq;
        rain.semantic_ticks_left = 1;
        rain.note_signal(RainSignal::Inspect as u32, 6);
        rain.apply_pending_notes();
        assert_eq!(
            rain.semantic_seq, inspect_seq,
            "repeated evidence extends one phase without reseeding its lanes"
        );
        assert_eq!(rain.semantic_ticks_left, SEMANTIC_HOLD_TICKS);
        let index = |rain: &MatrixRain, col, row, epoch| {
            rain.semantic_material_index(rain.material_start(col), col, row, epoch)
        };
        assert_eq!(
            index(&rain, 0, 3, 2),
            index(&rain, 1, 3, 2),
            "inspect forms a coherent two-column scout lane"
        );

        rain.note_signal(RainSignal::Modify as u32, 1);
        rain.apply_pending_notes();
        assert_ne!(
            index(&rain, 0, 2, 1),
            index(&rain, 1, 2, 1),
            "modify counterflows adjacent real-output sequences"
        );

        let before = rain.semantic_phase;
        rain.note_signal(u32::MAX, 8);
        rain.apply_pending_notes();
        assert_eq!(rain.semantic_phase, before, "unknown codes are inert");
    }

    #[test]
    fn semantic_work_wakes_drained_weather_without_a_gap() {
        let g = geom(30, 40, 8, 16);
        let mut rain = MatrixRain::new(cfg_on());
        scan_empty(&mut rain, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for _ in 0..160 {
            step(&mut rain, g, &idle(), 83, &mut q, &mut a);
        }
        assert_eq!(rain.weather, RainWeather::Sleep);
        assert!(!rain.is_active(), "settled glass is wake-free");

        rain.note_signal(RainSignal::Inspect as u32, 3);
        step(&mut rain, g, &idle(), 83, &mut q, &mut a);
        assert_eq!(rain.weather, RainWeather::Working);
        assert!(
            rain.is_active(),
            "observable work re-arms the bounded field"
        );
        assert!(!q.is_empty(), "the wake produces a real frame, not a gap");
    }

    #[test]
    fn live_selection_and_cursor_band_mask() {
        let g = geom(40, 60, 8, 16);
        let mut e = MatrixRain::new(RainConfig {
            density: 12,
            ..cfg_on()
        });
        scan_empty(&mut e, 40, 60);
        pour(&mut e);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for _ in 0..10 {
            e.note_keystroke();
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        assert!(!q.is_empty(), "field must be raining for the mask test");
        let sel = select_all(40, 60);
        let sel_input = RainTickInput {
            sel: Some(SelView {
                sel: &sel,
                display_offset: 0,
            }),
            ..idle()
        };
        e.note_keystroke();
        let fp = step(&mut e, g, &sel_input, 83, &mut q, &mut a);
        assert_eq!(fp, 0, "selection must suppress every quad");
        assert!(q.is_empty());
        let remembered = [20u16, 21];
        let cur_input = RainTickInput {
            cursor: Some((10, 5)),
            hidden_band: &remembered,
            ..idle()
        };
        e.note_keystroke();
        step(&mut e, g, &cur_input, 83, &mut q, &mut a);
        assert!(!q.is_empty());
        assert!(
            q.iter()
                .all(|s| !(8..=12).contains(&s.row) && !remembered.contains(&s.row)),
            "cursor and remembered multiline composer rows must be masked"
        );
        let band = [35u16, 36, 37, 38, 39];
        let hid_input = RainTickInput {
            hidden_band: &band,
            ..idle()
        };
        e.note_keystroke();
        step(&mut e, g, &hid_input, 83, &mut q, &mut a);
        assert!(
            q.iter().all(|s| s.row < 35),
            "hidden-cursor damaged band must be masked"
        );
    }

    #[test]
    fn reading_gates_suppress_emission() {
        let g = geom(40, 60, 8, 16);
        let mut e = MatrixRain::new(RainConfig {
            density: 12,
            ..cfg_on()
        });
        scan_empty(&mut e, 40, 60);
        pour(&mut e);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for _ in 0..5 {
            e.note_keystroke();
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        assert!(!q.is_empty());
        let back = RainTickInput {
            display_offset: 3,
            ..idle()
        };
        assert_eq!(step(&mut e, g, &back, 83, &mut q, &mut a), 0);
        assert!(!e.is_active(), "reading + empty frame ⇒ disarmed");
        let alt = RainTickInput {
            is_alt_screen: true,
            ..idle()
        };
        e.note_alt_scroll();
        assert_eq!(step(&mut e, g, &alt, 83, &mut q, &mut a), 0);
        for _ in 0..40 {
            e.note_keystroke();
            step(&mut e, g, &alt, 83, &mut q, &mut a);
        }
        assert!(!q.is_empty(), "scroll-quiet window must expire");
        assert!(e.is_active(), "a live emit re-arms");
        let mut e2 = MatrixRain::new(RainConfig {
            density: 12,
            suppress_in_alt_screen: true,
            ..cfg_on()
        });
        scan_empty(&mut e2, 40, 60);
        pour(&mut e2);
        for _ in 0..10 {
            e2.note_keystroke();
            assert_eq!(step(&mut e2, g, &alt, 83, &mut q, &mut a), 0);
        }
    }

    #[test]
    fn weather_transitions_with_dwell_hysteresis() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for _ in 0..60 {
            e.note_keystroke();
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
            assert_ne!(
                e.weather,
                RainWeather::Working,
                "keystrokes must cap at CALM"
            );
        }
        assert_eq!(e.weather, RainWeather::Calm);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let mut became_working_at = None;
        for i in 0..60u64 {
            e.note_activity(i + 1);
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
            if e.weather == RainWeather::Working {
                became_working_at = Some(i * 83);
                break;
            }
        }
        let at = became_working_at.expect("stream must reach WORKING");
        assert!(at >= 900, "dwell violated: WORKING after only {at} ms");
        let since = e.weather_since_ms;
        let mut left_working_at = None;
        for _ in 0..120u64 {
            e.note_keystroke();
            step(&mut e, g, &idle(), 33, &mut q, &mut a);
            if e.weather != RainWeather::Working {
                left_working_at = Some(e.weather_since_ms);
                break;
            }
        }
        let left = left_working_at.expect("stream end must decay to CALM");
        assert!(
            left.saturating_sub(since) >= DWELL_MS,
            "dwell violated on the way down"
        );
        assert_eq!(e.weather, RainWeather::Calm);
    }

    #[test]
    fn typing_echo_never_pours() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        let mut seq = 1u64;
        e.note_activity(seq);
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        for i in 0..120u64 {
            seq += u64::from(CONTENT_CREDIT_CAP);
            e.note_keystroke();
            e.note_activity(seq);
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
            assert_ne!(
                e.weather,
                RainWeather::Working,
                "echoed typing must never pour (step {i})"
            );
        }
        assert_eq!(e.weather, RainWeather::Calm, "typing keeps CALM alive");
        for _ in 0..40 {
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        assert!(e.wave.is_none(), "no turn wave from a typing pause");
        for _ in 0..60u64 {
            seq += 1;
            e.note_activity(seq);
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        assert_eq!(
            e.weather,
            RainWeather::Working,
            "clean agent output still pours"
        );
    }

    #[test]
    fn suspended_tick_starves_to_sleep_and_disarms() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for i in 0..60u64 {
            e.note_activity(i + 1);
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        assert_eq!(e.weather, RainWeather::Working);
        assert!(e.is_active(), "an awake engine keeps the timer armed");
        for _ in 0..145 {
            e.note_keystroke();
            e.advance_ms(83);
            e.step_suspended();
        }
        assert_eq!(e.weather, RainWeather::Sleep, "notes starve to SLEEP");
        assert!(
            !e.is_active(),
            "a drained suspended engine must disarm (no perpetual wakes)"
        );
    }

    #[test]
    fn turn_wave_fires_on_working_to_sleep() {
        let g = geom(30, 40, 8, 16);
        let mut cfg = cfg_on();
        cfg.idle_secs = 2;
        cfg.turn_wave = true;
        let mut e = MatrixRain::new(cfg);
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for i in 0..60u64 {
            e.note_activity(i + 1);
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        assert_eq!(e.weather, RainWeather::Working);
        let mut saw_wave = false;
        for _ in 0..80 {
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
            saw_wave |= e.wave.is_some();
        }
        assert!(saw_wave, "turn wave must fire on a direct WORKING→SLEEP");
        assert_eq!(e.weather, RainWeather::Sleep);
    }

    #[test]
    fn turn_wave_limiter_caps_ignitions() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        let mut starts: Vec<u64> = Vec::new();
        let mut last_wave: Option<(u64, u32)> = None;
        for i in 0..30u64 {
            e.wave_pending = true;
            e.note_keystroke();
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
            if e.wave != last_wave
                && let Some((start, _)) = e.wave
                && last_wave.is_none_or(|(s, _)| s != start)
            {
                starts.push(i * 83);
            }
            last_wave = e.wave;
        }
        assert!(
            starts.len() >= 3,
            "delayed waves must still eventually grant"
        );
        for w in starts.windows(3) {
            assert!(
                w[2] - w[0] >= 900,
                "3 wave ignitions within a rolling second: {starts:?}"
            );
        }
    }

    #[test]
    fn off_states_emit_nothing_and_disarm() {
        let g = geom(30, 40, 8, 16);
        for disabled in [false, true] {
            let mut e = MatrixRain::new(RainConfig {
                density: 12,
                enabled: !disabled,
                ..cfg_on()
            });
            scan_empty(&mut e, 30, 40);
            pour(&mut e);
            e.set_reduced_motion(!disabled);
            let (mut q, mut a) = (Vec::new(), Vec::new());
            for i in 0..10u64 {
                e.note_activity(i);
                assert_eq!(step(&mut e, g, &idle(), 83, &mut q, &mut a), 0);
                assert!(q.is_empty() && a.is_empty(), "disabled={disabled}");
                assert!(!e.is_active(), "disabled={disabled}");
            }
            if disabled {
                assert!(e.tile(0).is_none(), "no tile is baked while disabled");
            }
        }
    }

    #[test]
    fn needs_rescan_follows_the_damage_epoch_and_grid_replacement() {
        let mut e = MatrixRain::new(cfg_on());
        assert!(e.needs_rescan(1), "unscanned engine always rescans");
        scan_empty(&mut e, 10, 10);
        assert!(!e.needs_rescan(1), "an equal epoch holds (the collision)");
        assert!(e.needs_rescan(2), "epoch change re-arms the rescan");
        e.note_grid_replaced();
        assert!(
            e.needs_rescan(1),
            "grid replacement re-arms the rescan at the SAME epoch"
        );
    }

    #[test]
    fn tiny_pane_emits_without_panicking() {
        for rows in [1u16, 2] {
            let mut e = MatrixRain::new(cfg_on());
            scan_empty(&mut e, usize::from(rows), 20);
            pour(&mut e);
            let g = geom(rows, 20, 8, 16);
            let (mut q, mut a) = (Vec::new(), Vec::new());
            for _ in 0..120 {
                e.note_keystroke();
                e.advance_ms(83);
                e.emit(g, &idle(), &mut q, &mut a);
                for quad in &q {
                    assert!(quad.row < rows, "emission stays inside the real rows");
                }
            }
        }
    }

    #[test]
    fn note_grid_replaced_clears_the_bank_and_releases_the_editing_latch() {
        let mut e = MatrixRain::new(literal_cfg_on());
        scan_empty(&mut e, 10, 10);
        let mut cells = vec![vec![space_cell(bg3()); 10]; 10];
        for (i, ch) in "0f3a".chars().enumerate() {
            cells[0][i].ch = ch;
        }
        e.sample_material(&cells, 10, None, &[]);
        assert!(!e.literal_material_chars_for_test().is_empty());
        e.note_keystroke();
        assert!(e.material_editing_for_test(), "keystroke latches Editing");
        e.note_grid_replaced();
        assert!(
            e.literal_material_chars_for_test().is_empty(),
            "old grid's literal alphabet must not rain over the new session"
        );
        assert!(e.needs_material_sample(), "fresh sample re-armed");
        assert!(
            !e.material_editing_for_test(),
            "the latch belongs to the OLD grid's draft — the swap releases it"
        );
        e.sample_material(&cells, 10, None, &[]);
        assert!(
            !e.literal_material_chars_for_test().is_empty(),
            "the new grid's output refills the alphabet immediately"
        );
    }

    #[test]
    fn progressive_bake_through_the_engine() {
        let mut e = MatrixRain::new(RainConfig {
            density: 12,
            ..cfg_on()
        });
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        let g = geom(30, 40, 8, 16);
        step(&mut e, g, &idle(), 0, &mut q, &mut a);
        let v1 = e.atlas_version();
        assert!(
            e.tile(0).is_some() && !e.baker.complete(),
            "the first tick bakes a first batch of tiles"
        );
        for _ in 0..8 {
            e.note_keystroke();
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        assert!(e.baker.complete());
        assert!(e.atlas_version() > v1);
        let settled = e.atlas_version();
        e.note_keystroke();
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert_eq!(e.atlas_version(), settled);
        step(&mut e, geom(30, 40, 10, 20), &idle(), 83, &mut q, &mut a);
        assert_ne!(e.atlas_version(), settled);
    }

    #[test]
    fn quads_are_row_banded_and_tile_aligned() {
        let g = geom(40, 60, 8, 16);
        let mut e = MatrixRain::new(RainConfig {
            density: 12,
            ..cfg_on()
        });
        scan_empty(&mut e, 40, 60);
        pour(&mut e);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for _ in 0..10 {
            e.note_keystroke();
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        assert!(!q.is_empty());
        for s in &q {
            assert_eq!((s.w, s.h), (8, 16));
            assert!(
                e.tile(s.glyph).is_some_and(|t| (t.w(), t.h()) == (8, 16)),
                "every cell draws a baked cell-sized tile 1:1"
            );
            assert_eq!(
                u32::from(s.y),
                u32::from(s.row) * 16,
                "one-row-band invariant"
            );
            assert!(s.row < 40 && s.x < 8 * 60);
            assert!(s.alpha <= RAIN_ALPHA_CAP);
        }
        for hq in &a {
            let band = (u32::from(hq.y) / 16) as u16;
            assert_eq!(hq.row, band, "halo quads honor the row band");
            assert_eq!(
                (u32::from(hq.y) + u32::from(hq.h) - 1) / 16,
                u32::from(band),
                "halo band never spills"
            );
        }
    }

    fn composer_material_gate_model() -> aterm_spec::derive::Model {
        use aterm_spec::ty_model;
        ty_model! {
            ComposerMaterialGate {
                const Buggy = 0;
                var editing = 0;
                var sampled = 0;
                action Key {
                    editing = 1;
                    sampled = 0;
                }
                action TurnStart {
                    editing = 0;
                }
                action TrySample {
                    sampled = if editing == 0 { 1 } else { Buggy };
                }
                invariant BooleanBounds: editing <= 1 && sampled <= 1;
                invariant NoUnsentDraftSample: editing + sampled <= 1;
            }
        }
    }

    #[test]
    fn composer_material_gate_model_proves_and_catches_draft_sampling() {
        let model = composer_material_gate_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    #[test]
    fn real_material_sampling_gate_conforms_and_defers_refresh() {
        let model = composer_material_gate_model();
        let mut state = model.init_state();
        let mut rain = MatrixRain::new(literal_cfg_on());
        let mut cells = vec![vec![space_cell(bg3()); 8]; 6];
        cells[0][0].ch = 'R';

        rain.sample_material(&cells, 6, Some((5, 0)), &[]);
        assert!(model.fire("TrySample", &mut state));
        assert_eq!(i64::from(!rain.material_editing), state["sampled"]);
        assert_eq!(rain.literal_material_chars_for_test(), &['R']);

        rain.note_keystroke();
        assert!(model.fire("Key", &mut state));
        assert_eq!(i64::from(rain.material_editing), state["editing"]);
        cells[0][0].ch = 'z';
        rain.sample_material(&cells, 6, Some((5, 0)), &[]);
        assert!(model.fire("TrySample", &mut state));
        assert_eq!(i64::from(!rain.material_editing), state["sampled"]);
        assert_eq!(
            rain.literal_material_chars_for_test(),
            &['R'],
            "an unsent edit retains the previous real-output tape"
        );
        assert!(
            rain.needs_material_sample(),
            "the trusted refresh stays pending"
        );

        rain.note_signal(RainSignal::TurnStart as u32, 4);
        assert!(model.fire("TurnStart", &mut state));
        assert_eq!(i64::from(rain.material_editing), state["editing"]);
        rain.sample_material(&cells, 6, Some((5, 0)), &[]);
        assert!(model.fire("TrySample", &mut state));
        assert_eq!(i64::from(!rain.material_editing), state["sampled"]);
        assert_eq!(rain.literal_material_chars_for_test(), &['z']);
    }

    fn semantic_pulse_model() -> aterm_spec::derive::Model {
        use aterm_spec::ty_model;
        ty_model! {
            SemanticRainPulse {
                const Cap = 8;
                const Hold = 24;
                const Buggy = 0;
                var phase = 0;
                var energy = 0;
                var ttl = 0;
                var pending = 0;
                var pending_phase = 0;
                var pending_energy = 0;
                var pending_pri = 0;
                var turn = 0;
                var wave = 0;
                var fail = 0;
                var reduced = 0;
                action QueueAssistant when (reduced == 0) {
                    pending = 1;
                    pending_phase = if pending == 1 && pending_pri > 0 { pending_phase } else { 0 };
                    pending_energy = if pending == 1 && pending_pri > 0 && pending_energy > 2 { pending_energy } else { 2 };
                    pending_pri = if pending == 1 && pending_pri > 0 { pending_pri } else { 0 };
                }
                action QueueInspect when (reduced == 0) {
                    pending = 1;
                    pending_phase = if pending == 1 && pending_pri > 1 { pending_phase } else { 1 };
                    pending_energy = if pending == 1 && pending_pri > 1 && pending_energy > 3 { pending_energy } else { 3 };
                    pending_pri = if pending == 1 && pending_pri > 1 { pending_pri } else { 1 };
                }
                action QueueModify when (reduced == 0) {
                    pending = 1;
                    pending_phase = if pending == 1 && pending_pri > 1 { pending_phase } else { 2 };
                    pending_energy = if pending == 1 && pending_pri > 1 && pending_energy > 5 { pending_energy } else { 5 };
                    pending_pri = if pending == 1 && pending_pri > 1 { pending_pri } else { 1 };
                }
                action QueueExecute when (reduced == 0) {
                    pending = 1;
                    pending_phase = if pending == 1 && pending_pri > 1 { pending_phase } else { 3 };
                    pending_energy = if pending == 1 && pending_pri > 1 && pending_energy > 6 { pending_energy } else { 6 };
                    pending_pri = if pending == 1 && pending_pri > 1 { pending_pri } else { 1 };
                }
                action QueueNetwork when (reduced == 0) {
                    pending = 1;
                    pending_phase = if pending == 1 && pending_pri > 1 { pending_phase } else { 4 };
                    pending_energy = if pending == 1 && pending_pri > 1 && pending_energy > 5 { pending_energy } else { 5 };
                    pending_pri = if pending == 1 && pending_pri > 1 { pending_pri } else { 1 };
                }
                action QueueBranch when (reduced == 0) {
                    pending = 1;
                    pending_phase = if pending == 1 && pending_pri > 1 { pending_phase } else { 5 };
                    pending_energy = if pending == 1 && pending_pri > 1 && pending_energy > 6 { pending_energy } else { 6 };
                    pending_pri = if pending == 1 && pending_pri > 1 { pending_pri } else { 1 };
                }
                action QueueWaiting when (reduced == 0) {
                    pending = 1;
                    pending_phase = if pending == 1 && pending_pri > 2 { pending_phase } else { 6 };
                    pending_energy = if pending == 1 && pending_pri > 2 && pending_energy > 2 { pending_energy } else { 2 };
                    pending_pri = if pending == 1 && pending_pri > 2 { pending_pri } else { 2 };
                }
                action QueueSuccess when (reduced == 0) {
                    pending = 1;
                    pending_phase = if pending == 1 && pending_pri > 2 { pending_phase } else { 7 };
                    pending_energy = if pending == 1 && pending_pri > 2 && pending_energy > 6 { pending_energy } else { 6 };
                    pending_pri = if pending == 1 && pending_pri > 2 { pending_pri } else { 2 };
                }
                action QueueFailure when (reduced == 0) {
                    pending = 1;
                    pending_phase = 8;
                    pending_energy = if Buggy == 1 { Cap + 1 } else { Cap };
                    pending_pri = 3;
                }
                action QueueInterrupted when (reduced == 0) {
                    pending = 1;
                    pending_phase = 9;
                    pending_energy = 7;
                    pending_pri = 3;
                }
                action QueueTurnStart when (reduced == 0) {
                    pending = 1;
                    pending_phase = if pending == 1 && pending_pri > 0 { pending_phase } else { 10 };
                    pending_energy = if pending == 1 && pending_pri > 0 && pending_energy > 4 { pending_energy } else { 4 };
                    pending_pri = if pending == 1 && pending_pri > 0 { pending_pri } else { 0 };
                    turn = 1;
                }
                action Apply when (pending == 1) {
                    phase = pending_phase;
                    energy = pending_energy;
                    ttl = Hold;
                    wave = if pending_phase == 7 { 1 } else { wave };
                    fail = if pending_phase > 7 && pending_phase <= 9 { 1 } else { fail };
                    pending = 0;
                    pending_phase = 0;
                    pending_energy = 0;
                    pending_pri = 0;
                    turn = 0;
                }
                action Tick when (ttl > 0) {
                    phase = if ttl == 1 { 0 } else { phase };
                    energy = if ttl == 1 { 0 } else { energy };
                    ttl = ttl - 1;
                }
                action Reduce {
                    reduced = 1;
                    phase = 0;
                    energy = 0;
                    ttl = 0;
                    pending = 0;
                    pending_phase = 0;
                    pending_energy = 0;
                    pending_pri = 0;
                    turn = 0;
                }
                action Restore { reduced = 0; }
                action Reset {
                    phase = 0;
                    energy = 0;
                    ttl = 0;
                    pending = 0;
                    pending_phase = 0;
                    pending_energy = 0;
                    pending_pri = 0;
                    turn = 0;
                    wave = 0;
                    fail = 0;
                }
                invariant PhaseBound: phase <= 10;
                invariant PendingPhaseBound: pending_phase <= 10;
                invariant EnergyBound: energy <= Cap;
                invariant PendingEnergyBound: pending_energy <= Cap;
                invariant TtlBound: ttl <= Hold;
                invariant PriorityBound: pending_pri <= 3;
                invariant BooleanBounds: pending <= 1 && turn <= 1 && wave <= 1 && fail <= 1 && reduced <= 1;
            }
        }
    }

    #[test]
    fn semantic_pulse_derived_model_proves_and_catches_overflow() {
        let model = semantic_pulse_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    #[test]
    fn semantic_pulse_real_engine_conforms_across_all_phases() {
        let model = semantic_pulse_model();
        let cases = [
            (RainSignal::AssistantStream, 2, "QueueAssistant"),
            (RainSignal::Inspect, 3, "QueueInspect"),
            (RainSignal::Modify, 5, "QueueModify"),
            (RainSignal::Execute, 6, "QueueExecute"),
            (RainSignal::Network, 5, "QueueNetwork"),
            (RainSignal::Branch, 6, "QueueBranch"),
            (RainSignal::Waiting, 2, "QueueWaiting"),
            (RainSignal::Success, 6, "QueueSuccess"),
            (RainSignal::Failure, 8, "QueueFailure"),
            (RainSignal::Interrupted, 7, "QueueInterrupted"),
            (RainSignal::TurnStart, 4, "QueueTurnStart"),
        ];
        for (code, (signal, weight, action)) in cases.into_iter().enumerate() {
            assert_eq!(RainSignal::from_code(code as u32), Some(signal));
            let mut state = model.init_state();
            let mut rain = MatrixRain::new(cfg_on());
            rain.note_signal(code as u32, weight);
            assert!(model.fire(action, &mut state));
            let (pending, pending_energy) = rain.pending_signal.expect("queued signal");
            assert_eq!(i64::from(pending as u8), state["pending_phase"]);
            assert_eq!(i64::from(pending_energy), state["pending_energy"]);
            assert_eq!(i64::from(rain.pending_turn_start), state["turn"]);

            rain.apply_pending_notes();
            assert!(model.fire("Apply", &mut state));
            assert_eq!(i64::from(rain.semantic_phase as u8), state["phase"]);
            assert_eq!(i64::from(rain.semantic_energy), state["energy"]);
            assert_eq!(i64::from(rain.semantic_ticks_left), state["ttl"]);
            assert_eq!(i64::from(rain.wave_pending), state["wave"]);
            assert_eq!(i64::from(rain.fail_until_ms > rain.clock_ms), state["fail"]);
        }
        assert_eq!(RainSignal::from_code(cases.len() as u32), None);
    }

    #[test]
    fn semantic_pulse_same_priority_replaces_phase_and_energy() {
        let model = semantic_pulse_model();
        let mut state = model.init_state();
        let mut rain = MatrixRain::new(cfg_on());

        rain.note_signal(RainSignal::Execute as u32, 6);
        assert!(model.fire("QueueExecute", &mut state));
        rain.note_signal(RainSignal::Inspect as u32, 3);
        assert!(model.fire("QueueInspect", &mut state));

        let (pending, energy) = rain.pending_signal.expect("replacement signal");
        assert_eq!(pending, RainSignal::Inspect);
        assert_eq!(energy, 3, "same-priority replacement does not retain heat");
        assert_eq!(i64::from(pending as u8), state["pending_phase"]);
        assert_eq!(i64::from(energy), state["pending_energy"]);

        rain.apply_pending_notes();
        assert!(model.fire("Apply", &mut state));
        assert_eq!(rain.semantic_phase, RainSignal::Inspect);
        assert_eq!(rain.semantic_energy, 3);
        assert_eq!(i64::from(rain.semantic_energy), state["energy"]);
    }

    #[test]
    fn semantic_pulse_real_engine_priority_motion_reset_and_expiry_conform() {
        let model = semantic_pulse_model();
        let mut state = model.init_state();
        let mut rain = MatrixRain::new(cfg_on());

        rain.note_signal(RainSignal::Failure as u32, 8);
        assert!(model.fire("QueueFailure", &mut state));
        rain.note_signal(RainSignal::Inspect as u32, 3);
        assert!(model.fire("QueueInspect", &mut state));
        rain.note_signal(RainSignal::TurnStart as u32, 4);
        assert!(model.fire("QueueTurnStart", &mut state));
        let (pending, energy) = rain.pending_signal.expect("priority-coalesced signal");
        assert_eq!(pending, RainSignal::Failure);
        assert_eq!(i64::from(pending as u8), state["pending_phase"]);
        assert_eq!(i64::from(energy), state["pending_energy"]);
        assert_eq!(i64::from(rain.pending_turn_start), state["turn"]);

        rain.apply_pending_notes();
        assert!(model.fire("Apply", &mut state));
        for _ in 0..SEMANTIC_HOLD_TICKS {
            rain.step_engine_tick();
            assert!(model.fire("Tick", &mut state));
        }
        assert_eq!(rain.semantic_phase, RainSignal::AssistantStream);
        assert_eq!(rain.semantic_ticks_left, 0);

        rain.note_signal(RainSignal::Inspect as u32, 3);
        assert!(model.fire("QueueInspect", &mut state));
        rain.set_reduced_motion(true);
        assert!(model.fire("Reduce", &mut state));
        assert!(rain.pending_signal.is_none());
        rain.note_signal(RainSignal::Failure as u32, 8);
        assert!(rain.pending_signal.is_none(), "reduced motion drops pulses");
        rain.set_reduced_motion(false);
        assert!(model.fire("Restore", &mut state));

        rain.note_signal(RainSignal::Success as u32, 6);
        assert!(model.fire("QueueSuccess", &mut state));
        rain.apply_pending_notes();
        assert!(model.fire("Apply", &mut state));
        rain.reset();
        assert!(model.fire("Reset", &mut state));
        assert_eq!(i64::from(rain.semantic_phase as u8), state["phase"]);
        assert_eq!(i64::from(rain.semantic_energy), state["energy"]);
        assert_eq!(i64::from(rain.semantic_ticks_left), state["ttl"]);
    }

    fn rain_activity_credit_model() -> aterm_spec::derive::Model {
        use aterm_spec::ty_model;
        ty_model! {
            RainActivityCredit {
                const Cap = 32;
                const Buggy = 0;
                var credit = 0;
                var real = 0;
                var turn = 0;
                action ObserveBurst {
                    credit = if Buggy == 1 { Cap + 1 } else { Cap };
                }
                action NoteTurn { turn = 1; }
                action ApplyEcho when (credit > 0 && turn == 0) {
                    real = 0;
                    credit = 0;
                }
                action ApplyTurn when (credit > 0 && turn == 1) {
                    real = credit;
                    credit = 0;
                    turn = 0;
                }
                action ApplyClean when (credit > 0 && turn == 0) {
                    real = credit;
                    credit = 0;
                }
                action Rebase {
                    credit = 0;
                    real = 0;
                    turn = 0;
                }
                invariant CreditBound: credit <= Cap;
                invariant RealBound: real <= Cap;
                invariant TurnBound: turn <= 1;
            }
        }
    }

    #[test]
    fn rain_activity_credit_derived_model_proves_and_catches_overflow() {
        let m = rain_activity_credit_model();
        aterm_spec::verify::prove_and_catch_scalar(&m, m.name);
    }

    #[test]
    fn rain_activity_credit_real_engine_conforms_to_model() {
        let m = rain_activity_credit_model();
        let mut st = m.init_state();
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());

        e.note_activity(100);
        e.note_keystroke();
        e.note_activity(10_000);
        assert!(m.fire("ObserveBurst", &mut st));
        assert_eq!(i64::from(e.pending_content_credit), st["credit"]);
        assert!(m.check_invariant("CreditBound", &st));

        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert!(m.fire("ApplyEcho", &mut st));
        assert_eq!(e.pending_content_credit, 0);
        assert_eq!(i64::from(e.content_streak), st["real"]);
        assert_eq!(st["real"], 0);

        e.note_keystroke();
        e.note_signal(RainSignal::TurnStart as u32, 4);
        assert!(m.fire("NoteTurn", &mut st));
        e.note_activity(10_000 + u64::from(CONTENT_CREDIT_CAP));
        assert!(m.fire("ObserveBurst", &mut st));
        assert_eq!(i64::from(e.pending_content_credit), st["credit"]);
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert!(m.fire("ApplyTurn", &mut st));
        assert_eq!(i64::from(e.content_streak), st["real"]);
        assert_eq!(st["real"], i64::from(CONTENT_CREDIT_CAP));

        for _ in 0..13 {
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        e.note_activity(10_000 + 2 * u64::from(CONTENT_CREDIT_CAP));
        assert!(m.fire("ObserveBurst", &mut st));
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert!(m.fire("ApplyClean", &mut st));
        assert_eq!(i64::from(e.content_streak), st["real"]);

        e.note_activity(2);
        assert!(m.fire("Rebase", &mut st));
        assert_eq!(i64::from(e.pending_content_credit), st["credit"]);
        assert_eq!(i64::from(e.content_streak), st["real"]);
        assert!(m.check_invariant("CreditBound", &st));
        assert!(m.check_invariant("RealBound", &st));
    }

    #[test]
    fn rain_lifecycle_conformance_real_engine_projects_onto_model() {
        let m = aterm_models_effects::rain_lifecycle_model();
        let mut st = m.init_state();
        let g = geom(24, 60, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 24, 60);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        let check_inv = |st: &std::collections::BTreeMap<&'static str, i64>| {
            for name in [
                "NoUnlicensedRain",
                "CanReachIdle",
                "StateBounded",
                "DrainBounded",
            ] {
                assert!(m.check_invariant(name, st), "invariant {name} must hold");
            }
        };

        e.note_activity(1);
        e.note_activity(2);
        step(&mut e, g, &idle(), 100, &mut q, &mut a);
        assert_eq!(
            e.drain_ticks, 0,
            "a focused awake pane carries no drain fuel"
        );
        assert!(e.is_active());
        assert!(
            m.fire("Activity", &mut st),
            "the model admits the enable Activity"
        );
        assert_eq!(st["state"], 1, "enable enters Raining");
        assert_eq!(st["drained"], 0);
        check_inv(&st);

        e.set_visibility(RainVisibility::VisibleUnfocused);
        assert_eq!(e.drain_ticks, 0, "unfocus does not itself spend drain fuel");
        assert!(m.fire("StartDrain", &mut st));
        assert_eq!(st["state"], 2, "unfocus enters Draining");
        assert_eq!(st["drained"], 0);
        check_inv(&st);

        let mut prev = e.drain_ticks;
        let mut guard = 0;
        while e.drain_ticks < DRAIN_TICKS {
            step(&mut e, g, &idle(), u64::from(CALM_TICK_MS), &mut q, &mut a);
            let d = e.drain_ticks;
            for _ in prev..d {
                assert!(
                    m.fire("DrainTick", &mut st),
                    "the model admits each drain tick"
                );
            }
            assert_eq!(
                st["drained"],
                i64::from(d),
                "model fuel tracks real drain_ticks"
            );
            assert_eq!(
                st["state"],
                if d >= DRAIN_TICKS { 0 } else { 2 },
                "Draining until the fixed drain bound, then Idle"
            );
            check_inv(&st);
            prev = d;
            guard += 1;
            assert!(guard < 200, "the drain must terminate");
        }
        assert_eq!(
            st["state"], 0,
            "the drain lands Idle at exactly DRAIN_TICKS"
        );
        assert_eq!(st["drained"], i64::from(DRAIN_TICKS));

        assert_ne!(
            e.weather,
            RainWeather::Sleep,
            "still awake after a ~2.5 s drain (idle_secs default 8 s)"
        );
        e.set_visibility(RainVisibility::Focused);
        assert_eq!(e.drain_ticks, 0, "a licensed refocus clears the drain fuel");
        assert!(
            m.fire("Activity", &mut st),
            "the licensed resume is an Activity"
        );
        assert_eq!(st["state"], 1, "resume re-enters Raining");
        check_inv(&st);
        assert_eq!(st["rains"], st["acts"]);
    }

    #[test]
    fn a_slept_drained_pane_does_not_relight_on_refocus() {
        let g = geom(24, 60, 8, 16);
        let mut e = MatrixRain::new(RainConfig {
            enabled: true,
            seed: 7,
            idle_secs: 2,
            ..RainConfig::default()
        });
        scan_empty(&mut e, 24, 60);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        e.note_activity(1);
        e.note_activity(2);
        let mut guard = 0;
        while !(e.weather == RainWeather::Sleep && e.drain_ticks >= DRAIN_TICKS) {
            step(&mut e, g, &idle(), u64::from(CALM_TICK_MS), &mut q, &mut a);
            guard += 1;
            assert!(guard < 500, "the pane must sleep and drain");
        }
        assert!(!e.is_active(), "a slept, drained pane is inactive");
        e.set_visibility(RainVisibility::Hidden);
        assert_eq!(e.drain_ticks, DRAIN_TICKS);
        e.set_visibility(RainVisibility::Focused);
        assert_eq!(
            e.drain_ticks, DRAIN_TICKS,
            "a refocus of a SLEEPING pane keeps the drain spent — no phantom relight"
        );
        assert!(
            !e.is_active(),
            "the refocused slept pane stays inactive until real activity"
        );
    }
}
