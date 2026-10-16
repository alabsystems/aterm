// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Matrix digital-rain engine (`docs/matrix-rain-design.md`).
//!
//! `MatrixRain` follows the `word_decorations`/`cursor_trail` lifecycle
//! contract: an injected-clock state machine (`tick(now, ..)` natively;
//! `advance_ms` + `emit` for clockless web hosts) that fills resident
//! `SpriteQuad`/`GlowQuad` scratch and returns a fingerprint (`0` when empty,
//! stable when drained, changing every animating tick). The grid is never
//! touched; rain is strictly under text (the `rain_quads`/`rain_add`
//! `RenderInput` channels) and only in cells that carry no meaning.
//!
//! - Field: all lattice math lives in [`field`] as pure `(seed, tick)`
//!   functions on `rain_hash32` (u32 only, shader-portable).
//! - Mask: Tier A is the damage-epoch-gated occupancy bitset
//!   ([`MatrixRain::rescan_from_cells`]); Tier B (selection, cursor band,
//!   scrollback/alt-screen reading gates) is evaluated live per tick.
//! - Weather: WORKING / CALM (12 Hz tick gate) / SLEEP with ≥ 1 s dwell, an
//!   integer-EMA density staircase, a mandatory geometry-independent drain,
//!   drain on unfocus, a limiter-gated turn wave on the WORKING→CALM edge, and
//!   a 2 s constant-luminance amber bell ramp.
//! - Flash safety is structural: the per-column flash floor
//!   ([`field::col_params`]), the 350 ms dither grid and the ≤ 2 ignitions/s
//!   wave limiter live in the math, not in host policy.

pub mod bake;
mod bitmap_font;
pub mod field;
pub mod rom;
pub mod step;
pub use step::{RainClock, RainFrame, RainLatches, RainScan};

use std::sync::Arc;

use aterm_time::Instant;

use aterm_core::grid::LineSize;
use aterm_core::grid::extra::ImageRef;
use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_hash::{FNV64_OFFSET, fnv_mix_u64};
use aterm_render::{RainHalo, SceneAtlas, SpriteQuad, premul_rgb};

use crate::color_math::{hsv2rgb, relative_luminance, rgb2hsv};
use crate::genome::mix;
pub use crate::word_decorations::{EffectGeom, SelView};

pub use bake::{MAX_RAIN_BAKES_PER_TICK, RAIN_TILE_GRID, RainBaker};
use field::{
    ColParams, DensityRing, FieldParams, col_params, cycle_active, cycle_at, cycle_start_tick,
    dither, dither_epoch, glyph_epoch, glyph_from_epoch, trail_level,
};
pub use field::{MAX_RAIN_ADD, MAX_RAIN_QUADS, MAX_RAIN_TEXELS, quad_cap};
use rom::{RomMaster, decorative_master};

/// Readability ceiling on any rain coverage (body, head, halo) — the
/// `cursor_trail::READABLE_ALPHA_CAP` precedent, pinned by test.
pub const RAIN_ALPHA_CAP: u8 = 135;
/// Floor on the derived/overridden body alpha (below this rain is invisible
/// noise that still costs quads).
pub const RAIN_ALPHA_FLOOR: u8 = 16;
/// Hidden-cursor mask band: the host feeds the last K recently-damaged rows
/// (Claude Code's inline input box lives there); K is pinned here.
pub const HIDDEN_CURSOR_BAND_ROWS: usize = 5;
/// Visible-cursor mask band half-height: cursor row ± this many rows.
pub const CURSOR_BAND_ROWS: i32 = 2;

/// Output-material sequence cap: the latest 128 supported codepoints outside
/// the current protection bands remain in source order, retaining bounded
/// adjacency without viewport-sized storage.
const MATERIAL_CAP: usize = 128;
/// The material atlas reuses the fixed 64-tile ROM footprint. A sampled screen
/// can therefore contain at most 64 distinct literal glyphs; occurrences still
/// retain their frequency/order in [`MATERIAL_CAP`].
const MATERIAL_GLYPH_CAP: usize = rom::ROM_GLYPHS;
/// Bounded simultaneous box-drawn TUI frames tracked during one damage rescan.
/// Real panes rarely contain more than a handful; the cap makes hostile grids
/// unable to turn frame recognition into quadratic work.
const MAX_FRAME_CANDIDATES: usize = 64;
const MAX_FRAME_REGIONS: usize = 64;
/// Observable-work pulse bounds. Signals carry no payload, license only a
/// bounded activity window, and cannot allocate or rebake the atlas.
const SEMANTIC_ENERGY_CAP: u8 = 8;
const SEMANTIC_HOLD_TICKS: u8 = 24;

/// CALM/SLEEP internal tick period (12 Hz) — keystrokes alone never tick
/// faster than this.
const CALM_TICK_MS: u32 = 83;
/// Weather dwell hysteresis: a state must hold this long before switching.
const DWELL_MS: u64 = 1000;
/// How long after the last content delta the WORKING desire persists.
const WORKING_HOLD_MS: u64 = 2000;
/// Content deltas this close together extend the streaming streak; WORKING
/// requires a streak ≥ 2 (a lone delta is not "sustained streaming").
const STREAK_WINDOW_MS: u64 = 1000;
/// Echo-correlation window: a content delta within this of the user's last
/// keystroke is attributed to the shell echoing it (locally < 20 ms; generous
/// for ssh/load) and never advances the WORKING streak, so your own typing is
/// a drizzle.
///
/// Trade-offs: echo latency above the window (a laggy ssh hop) reads as
/// content; output coalesced after an editing/navigation key is discounted
/// with that frame, while Enter/submit supplies an explicit
/// [`RainSignal::TurnStart`] boundary so a fast first response still counts.
const ECHO_DISCOUNT_MS: u64 = 250;
/// Bounded `content_seq` distance retained between presents: enough for a
/// synchronized agent frame to classify as real output, without letting a huge
/// generation jump inflate state.
const CONTENT_CREDIT_CAP: u8 = 32;
/// Mandatory drain length in engine ticks: at the CALM 12 Hz gate this is
/// ~2.5 s — every column reaches empty within this bound regardless of
/// geometry (`C·p` alone is 6–16 s at 50 rows).
const DRAIN_TICKS: u32 = 30;
/// Alt-screen scroll-quiet window (design §6): wheel/PgUp input while in the
/// alt screen suppresses emission this long. A pinned const, not a knob.
const ALT_SCROLL_QUIET_MS: u64 = 3000;
/// Bell ALERT overlay duration: the amber constant-luminance ramp swap.
const BELL_ALERT_MS: u64 = 2000;
/// Turn-wave sweep duration (≤ 700 ms, design §5).
const WAVE_MS: u64 = 700;
/// Turn-wave limiter: at most this many wave ignitions per rolling second
/// (the `flash_limiter_model` shape — WCAG 2.3.1).
const WAVE_IGNITIONS_PER_SEC: usize = 2;
/// Backlog cap: a huge clock gap (window hidden for hours) fast-forwards the
/// tick counter arithmetically instead of stepping millions of engine ticks.
/// Below this bound, advance-by-dt and tick-stepping are exactly equivalent.
const CATCHUP_MS: u64 = 60_000;
/// Codex diff-band green (design §6): the ramp hue keeps ≥ this angular
/// distance from it so rain never reads as diff semantics.
const HUE_SEPARATION_DEG: f32 = 18.0;
const DIFF_GREEN: u32 = 0x0021_3A2B;
/// The stock "matrix" ramp hue (green, already separated from `DIFF_GREEN`).
const MATRIX_HUE_DEG: f32 = 122.0;
/// Bell ALERT ramp hue (amber).
const ALERT_HUE_DEG: f32 = 42.0;
/// Command-failed ramp hue (ember red-orange — clearly distinct from the amber
/// bell and the green field; constant-luminance swap, never a flash).
const FAIL_HUE_DEG: f32 = 8.0;
/// How long the failed-exit ember tint holds (mirrors the bell window).
const EXIT_FAIL_MS: u64 = 2000;

/// Rain tint family (`[matrix_rain] hue`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RainHue {
    /// The stock matrix green.
    Matrix,
    /// Derive the hue from the theme: its foreground, or, where the
    /// foreground is achromatic, its accent (`RainConfig::theme_accent`) or
    /// the stock ANSI blue ([`theme_hue`]).
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

/// The weather machine states (design §5). Ordered so a visibility cap is
/// `min(state, cap)`: `Sleep < Calm < Working`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RainWeather {
    /// No emission; the field drains and the host timer disarms.
    Sleep,
    /// 12 Hz drizzle, low density — typing lives here.
    Calm,
    /// Full fps, full density — sustained agent output.
    Working,
}

/// Payload-free choreography signal from an observable agent/tool event. The
/// literal glyph source remains the composer's protected visible terminal
/// snapshot; this enum changes only how those real glyphs form coherent lanes.
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
    pub fn from_code(code: u32) -> Option<Self> {
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

    fn priority(self) -> u8 {
        match self {
            Self::Failure | Self::Interrupted => 3,
            Self::Success | Self::Waiting => 2,
            Self::Inspect | Self::Modify | Self::Execute | Self::Network | Self::Branch => 1,
            Self::AssistantStream | Self::TurnStart => 0,
        }
    }
}

/// Fully-resolved rain configuration. The gui resolver owns defaults and
/// clamps; the engine re-clamps defensively.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RainConfig {
    /// Master gate. Disabled ⇒ empty channels, fp 0, inactive.
    pub enabled: bool,
    /// Engine tick rate while WORKING, `12..=60`.
    pub fps: u8,
    /// Density knob `1..=12` (scales the weather EMA target).
    pub density: u8,
    /// Fall-speed knob `1..=10` (5 = neutral; shifts the step period).
    pub speed: u8,
    /// Trail-length knob `1..=10` (5 = neutral).
    pub trail: u8,
    /// Body coverage override; `None` derives it from the §6 luminance
    /// constraint. Clamped `16..=135`.
    pub alpha_override: Option<u8>,
    /// Head coverage override; `None` derives. Clamped `alpha..=135`.
    pub head_alpha_override: Option<u8>,
    /// Ramp tint family.
    pub hue: RainHue,
    /// Glyph mutation window in ms, `80..=2000`.
    pub mutation_ms: u16,
    /// Idle seconds until mandatory SLEEP, `2..=120`. There is no
    /// `idle = "keep"`: no configuration animates forever.
    pub idle_secs: u16,
    /// Suppress emission entirely while the alternate screen is active.
    pub suppress_in_alt_screen: bool,
    /// Output material: the rain's glyph alphabet is sampled from the program's
    /// real on-screen output and rasterized into a bounded dynamic ROM (case,
    /// digits, punctuation, box drawing and supported Unicode keep distinct
    /// bitmaps), while still drawing only in empty cells. The cursor/composer
    /// protection band is excluded; hosts can mark more cells ineligible.
    /// Default true; false keeps the classic kana field.
    pub output_material: bool,
    /// Turn-complete wave on the WORKING→CALM edge.
    pub turn_wave: bool,
    /// Visual bell → 2 s amber ALERT hue-ramp.
    pub bell_alert: bool,
    /// Command exit status in the weather (OSC 133/633, host-fed): success
    /// fires the finishing head-sweep; failure holds a 2 s constant-luminance
    /// ember tint.
    pub exit_tint: bool,
    /// Replay seed (0 is a valid seed; demos pin it for reproducibility).
    pub seed: u64,
    /// Theme default background `0x00RRGGBB` (ramp + luminance derivation).
    pub default_bg: u32,
    /// Theme foreground `0x00RRGGBB` (the SGR-2 dim reference the luminance
    /// invariant floors against, and the `hue = "theme"` source).
    pub theme_fg: u32,
    /// Theme accent (its cursor) `0x00RRGGBB`: the `hue = "theme"` source
    /// where the foreground has no hue of its own ([`theme_hue`]).
    pub theme_accent: u32,
}

impl Default for RainConfig {
    fn default() -> Self {
        Self {
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
            // Stock dark theme chrome (aterm-types/src/scheme.rs,
            // `ColorScheme::default`): bg #111318 / fg #D0D0D0 / cursor #50FA7B.
            default_bg: 0x0011_1318,
            theme_fg: 0x00D0_D0D0,
            theme_accent: 0x0050_FA7B,
        }
    }
}

/// The Tier-B live per-tick inputs (snapshot scalars the host read under its
/// terminal lock; the engine never re-locks).
#[derive(Clone, Copy, Default)]
pub struct RainTickInput<'a> {
    /// Visible cursor cell `(row, col)`, `None` when hidden (DECTCEM off).
    pub cursor: Option<(u16, u16)>,
    /// When the cursor is hidden: the last [`HIDDEN_CURSOR_BAND_ROWS`] recently
    /// damaged viewport rows, host-fed (Ink parks a hidden cursor somewhere
    /// meaningless, so damage recency stands in for it).
    pub hidden_band: &'a [u16],
    /// Live selection view (search highlight is the selection). Applied per
    /// emitted quad: selection changes mark no damage, so it never bakes into
    /// the Tier-A bitset.
    pub sel: Option<SelView<'a>>,
    /// Native scrollback offset: any non-zero value suppresses emission
    /// (scrolling back always yields clean text).
    pub display_offset: i32,
    /// Whether the alternate screen is active (gates `suppress_in_alt_screen`
    /// and the scroll-quiet window).
    pub is_alt_screen: bool,
}

/// Per-frame emission context (config + geometry resolved once per tick).
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
fn in_box_drawing_block(ch: char) -> bool {
    matches!(ch as u32, 0x2500..=0x257F)
}

#[inline]
fn frame_top_left(ch: char) -> bool {
    matches!(ch, '┌' | '┏' | '╔' | '╒' | '╓' | '╭')
}

#[inline]
fn frame_top_right(ch: char) -> bool {
    matches!(ch, '┐' | '┓' | '╗' | '╕' | '╖' | '╮')
}

#[inline]
fn frame_bottom_left(ch: char) -> bool {
    matches!(ch, '└' | '┗' | '╚' | '╘' | '╙' | '╰')
}

#[inline]
fn frame_bottom_right(ch: char) -> bool {
    matches!(ch, '┘' | '┛' | '╝' | '╛' | '╜' | '╯')
}

#[inline]
fn frame_horizontal(ch: char) -> bool {
    matches!(
        ch,
        '─' | '━' | '═' | '┄' | '┅' | '┈' | '┉' | '╌' | '╍' | '╴' | '╶' | '╸' | '╺'
    )
}

#[inline]
fn frame_vertical(ch: char) -> bool {
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

/// The rain engine. One per window; all buffers resident.
pub struct MatrixRain {
    cfg: RainConfig,
    // -- config-derived caches (rebuilt by `set_config`) --------------------
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
    // -- clock ---------------------------------------------------------------
    /// First observed instant (native hosts); web hosts never set it and
    /// drive `advance_ms` instead.
    epoch: Option<Instant>,
    /// Continuous host time in ms since the epoch.
    clock_ms: u64,
    /// Time already converted into whole engine ticks.
    consumed_ms: u64,
    /// The field tick (advances at the weather-gated engine rate).
    tick: u64,
    // -- pending host notes (stamped at the next clock sync) ---------------------
    pending_key: bool,
    /// Coalesced distance in the grid content clock (bounded). Unlike a boolean
    /// edge it survives synchronized-output batching, so one large agent
    /// present still carries evidence of a burst.
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
    // -- weather --------------------------------------------------------------
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
    // -- turn wave ------------------------------------------------------------
    wave_pending: bool,
    /// Active wave: `(start tick, length in ticks)`.
    wave: Option<(u64, u32)>,
    /// Rolling-window ignition timestamps (ms); `u64::MAX` = empty slot.
    ign_ms: [u64; WAVE_IGNITIONS_PER_SEC],
    // -- Tier-A occupancy bitset ----------------------------------------------
    occ: Vec<u64>,
    occ_rows: usize,
    occ_cols: usize,
    have_scanned: bool,
    last_epoch: u64,
    /// Resident scratch for closed box-drawn TUI surface recognition.
    frame_candidates: Vec<FrameCandidate>,
    frame_regions: Vec<FrameRegion>,
    frame_horizontal_prefix: Vec<usize>,
    // -- hash-stride column order (ascending hash = the truncation prefix) ----
    col_order: Vec<u16>,
    order_cols: u16,
    order_seed: u32,
    /// Row count the caches were built at (`ColParams` depends on rows, so a
    /// resize must invalidate the cache alongside cols/seed).
    order_rows: u16,
    /// Tick-independent per-column [`ColParams`], rebuilt on the same trigger
    /// as `col_order` (a pure-function memo of the frame's `FieldParams`).
    col_params_cache: Vec<ColParams>,
    // -- ROM + baker ------------------------------------------------------------
    rom: Option<RomMaster>,
    baker: RainBaker,
    last_bake_tick: Option<u64>,
    // -- output material bank ----------------------------------------------------
    /// Dynamic-ROM tile indices for the program's literal on-screen characters
    /// at the last Tier-A rescan (≤ [`MATERIAL_CAP`], source order retained;
    /// empty ⇒ the classic pure-hash alphabet).
    material: Vec<u8>,
    /// Sorted slot-to-character table currently authored into the ROM prefix
    /// (≤ [`MATERIAL_GLYPH_CAP`]). A stable set does not trigger a rebake when
    /// only occurrence frequency/order changes during streaming.
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
    /// alt-screen quiet/suppression). With an empty frame on glass this disarms
    /// the wake timer; scroll input and returning to live are redraws that
    /// re-arm it (CATCHUP fast-forwards the backlog).
    reading_gated: bool,
    // -- per-tick scratch (resident) -------------------------------------------
    halo_cands: Vec<(u16, u16)>,
    /// Stable row-sort scratch (counting sort): the emitted quads copied out…
    sort_scratch: Vec<SpriteQuad>,
    /// …and the per-row scatter offsets (rows + 1 entries).
    row_starts: Vec<u32>,
    last_emit_nonempty: bool,
}

impl MatrixRain {
    /// Build an engine for `cfg` (clamped defensively; the host resolver owns
    /// the real clamps).
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
            frame_candidates: Vec::with_capacity(MAX_FRAME_CANDIDATES),
            frame_regions: Vec::with_capacity(MAX_FRAME_REGIONS),
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

    /// Swap the configuration: rebuild the ramp and derived caches (and restart
    /// the bake, since a ramp change bumps the version), keeping the tick
    /// epoch/clock so a live reload never time-travels the field.
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
        self.order_cols = 0; // force a col_order rebuild
        // The material knob must take effect on a live reload, even when the
        // visible grid has not produced another damage epoch.
        if !cfg.output_material {
            self.clear_material_bank();
            self.material_sample_needed = false;
        } else if material_mode_changed {
            self.material_sample_needed = true;
        }
    }

    /// Clear the dynamic field/weather state (layout transition, master
    /// toggle). Keeps the config, clock, and tick epoch.
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

    /// Clear the literal material without changing weather/clock state. The ROM
    /// returns to the classic authoring, emitted only when
    /// `output_material = false`; literal mode with nothing sampled is empty.
    /// Repeated empty samples are allocation- and bake-free.
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

    /// Publish a literal material generation once cell metrics are known. The
    /// fixed 64-tile bake is sub-millisecond and happens only when the distinct
    /// visible charset changes; before the first geometry the baker is
    /// vacuously complete and `emit` does this once after sizing.
    fn finish_material_bake(&mut self) {
        let Some(rom) = self.rom.as_ref() else {
            return;
        };
        while !self.baker.complete() {
            self.baker.bake_tiles(rom);
        }
        self.last_bake_tick = Some(self.tick);
    }

    /// PTY content mutation observed (`Terminal::content_seq()`): sustained
    /// deltas drive WORKING. The bounded sequence distance is retained, so
    /// synchronized output that mutates many cells before one present still
    /// counts as a burst. Cheap; callable every frame.
    pub fn note_activity(&mut self, content_seq: u64) {
        let Some(previous) = self.last_seq.replace(content_seq) else {
            // The first observation is a baseline, not activity: enabling
            // rain over a static screen must not read as streaming.
            return;
        };
        if content_seq < previous {
            // A tab/session replacement can install a fresh grid whose clock
            // starts lower. Rebase without manufacturing a giant wrapped burst.
            self.pending_content_credit = 0;
            self.content_streak = 0;
            self.last_content_ms = None;
            return;
        }
        let delta = content_seq.saturating_sub(previous);
        if delta > 0 {
            let credit = delta.min(u64::from(CONTENT_CREDIT_CAP)) as u8;
            self.pending_content_credit = self
                .pending_content_credit
                .saturating_add(credit)
                .min(CONTENT_CREDIT_CAP);
        }
    }

    /// Whether emission is possible right now, so the host can skip the
    /// O(rows·cols) rescan/sample: false under reduced motion or for a
    /// non-focused pane whose drain completed. Skipping leaves `last_epoch`
    /// unbumped, so the scan runs on the first frame the gate lifts.
    #[must_use]
    pub fn can_emit(&self) -> bool {
        self.cfg.enabled
            && !self.reduced_motion
            && (self.visibility == RainVisibility::Focused || self.drain_ticks < DRAIN_TICKS)
    }

    /// Whether literal mode needs a fresh authoritative host sample even if
    /// occupancy is already current (for example after a live mode switch).
    #[must_use]
    pub fn needs_material_sample(&self) -> bool {
        self.cfg.output_material && self.material_sample_needed
    }

    /// Restore the host's composer provenance baseline when a web pipeline
    /// recreates this lazily-owned engine after an off/on toggle.
    pub(crate) fn set_material_editing(&mut self, editing: bool) {
        self.material_editing = editing;
        self.material_sample_needed |= editing && self.cfg.output_material;
    }

    #[cfg(test)]
    pub(crate) fn material_editing_for_test(&self) -> bool {
        self.material_editing
    }

    /// Enter a reading-only viewport without scanning its translated cells.
    /// Returning to live must rebuild occupancy from a fresh coherent frame.
    pub fn defer_reading(&mut self) {
        self.have_scanned = false;
        self.reading_gated = true;
        self.last_emit_nonempty = false;
    }

    /// The host swapped which grid this engine watches (tab switch, pane focus
    /// move, session migration). Two terminals' damage epochs can collide, so
    /// rebaseline everything grid-derived: force the next Tier-A rescan, drop
    /// and resample the material bank, make the next `note_activity` a
    /// baseline, and release the `material_editing` latch (the old grid's
    /// draft; kept, it would leave the new grid dark until its next Enter — the
    /// cursor/hidden-band exclusions still protect the new composer). Weather
    /// and clock survive: the swap is a viewpoint change, not a restart.
    pub fn note_grid_replaced(&mut self) {
        self.have_scanned = false;
        self.clear_material_bank();
        self.material_sample_needed = self.cfg.output_material;
        self.material_editing = false;
        self.last_seq = None;
    }

    /// Whether an input note could animate this engine, so the host skips
    /// useless wake redraws: false when disabled or under reduced motion.
    #[must_use]
    pub fn notes_can_wake(&self) -> bool {
        self.cfg.enabled
            && !self.reduced_motion
            && (!self.cfg.output_material || !self.material.is_empty())
    }

    /// A keystroke: keeps CALM alive; keystrokes alone never reach WORKING.
    /// Literal sampling also fails closed until explicit TurnStart, so an
    /// arbitrarily tall unsent draft cannot become rain material.
    pub fn note_keystroke(&mut self) {
        self.pending_key = true;
        self.material_editing = true;
    }

    /// Visual bell: arms the 2 s amber ALERT hue-ramp (when `bell_alert`).
    pub fn note_bell(&mut self) {
        self.pending_bell = true;
    }

    /// A command completed (OSC 133/633 exit code, host-fed): success fires
    /// the finishing head-sweep; failure holds the 2 s ember tint. Gated by
    /// the `exit_tint` knob at apply time. Last completion in a frame wins.
    pub fn note_exit_status(&mut self, failed: bool) {
        self.pending_exit = Some(failed);
    }

    /// Ingest one payload-free observable-work pulse. `code` is
    /// [`RainSignal`] and `weight` is a saturating visual-coherence hint. No
    /// text, command, path, URL, prompt, or tool input is retained here.
    pub fn note_signal(&mut self, code: u32, weight: u32) {
        let Some(signal) = RainSignal::from_code(code) else {
            return;
        };
        // Provenance release is independent of animation policy: reduced
        // motion may reject the visual pulse, but submit still ends Editing.
        if signal == RainSignal::TurnStart {
            self.material_editing = false;
        }
        if !self.cfg.enabled || self.reduced_motion {
            return;
        }
        self.pending_turn_start |= signal == RainSignal::TurnStart;
        let weight = weight.clamp(1, u32::from(SEMANTIC_ENERGY_CAP)) as u8;
        match self.pending_signal {
            Some((current, current_weight)) if current.priority() > signal.priority() => {
                self.pending_signal = Some((current, current_weight.max(weight)));
            }
            _ => self.pending_signal = Some((signal, weight)),
        }
    }

    /// Wheel/PgUp input while in the alt screen: stamps the scroll-quiet
    /// deadline (reading a fullscreen transcript never summons a downpour).
    pub fn note_alt_scroll(&mut self) {
        self.pending_alt_scroll = true;
    }

    /// Host-reported pane visibility (drain-on-unfocus policy).
    pub fn set_visibility(&mut self, v: RainVisibility) {
        if v == self.visibility {
            return;
        }
        self.visibility = v;
        match v {
            // Hidden drains immediately: the next frame is already empty.
            RainVisibility::Hidden => self.drain_ticks = DRAIN_TICKS,
            // Refocus resumes only when the weather itself is awake; a
            // sleeping pane stays drained (no phantom replay on cmd-tab).
            RainVisibility::Focused => {
                if self.weather != RainWeather::Sleep {
                    self.drain_ticks = 0;
                }
            }
            RainVisibility::VisibleUnfocused => {}
        }
    }

    /// OS/config reduce-motion: rain emits nothing, fp 0, inactive.
    pub fn set_reduced_motion(&mut self, on: bool) {
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
    pub fn needs_rescan(&self, epoch: u64) -> bool {
        !self.have_scanned || epoch != self.last_epoch
    }

    /// The damage epoch the Tier-A scan last consumed, `None` before the first
    /// [`rescan_from_cells`](Self::rescan_from_cells), so a host can assert the
    /// scan advances (neither `needs_rescan` nor `diag_line`'s sticky `scanned=`
    /// shows a host that stopped feeding it).
    #[must_use]
    pub fn scanned_epoch(&self) -> Option<u64> {
        self.have_scanned.then_some(self.last_epoch)
    }

    /// Rebuild the Tier-A occupancy bitset from the frame snapshot (the same
    /// `cell_frame_into` rows sparkle scans). Eligible iff the cell is a space,
    /// not a wide half, not underlined, on the default background, not under an
    /// inline image, and the row is `SingleWidth` (DECDWL/DECDHL rows are
    /// wholly ineligible).
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
        // Every cell starts eligible. `RenderInput.cells` is trimmed: trailing
        // blank cells and whole blank rows are absent, and the renderer paints
        // `default_bg` there, so an absent cell is empty and eligible (blank
        // rows are exactly where rain belongs). A present ineligible cell or an
        // inline-image cell is semantic, and the clearance pass below clears
        // it with its neighbours.
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
        for r in 0..rows {
            if line_sizes
                .get(r)
                .is_none_or(|ls| *ls != LineSize::SingleWidth)
            {
                self.clear_occ_span(r, 0, cols.saturating_sub(1));
            }
        }

        // Semantic clearance: a rain cell needs one whole cell of room from
        // real glyphs, attributed spaces, wide halves and images, which also
        // keeps the half-cell head halo off text.
        for r in 0..rows {
            let row_cells: &[RenderCell] = cells.get(r).map(Vec::as_slice).unwrap_or(&[]);
            // Adjacent meaningful cells' 3x3 boxes overlap, and the dilated
            // columns arrive in ascending order, so one running interval
            // coalesces them and each interval is cleared with the masked-word
            // span writer. Boxes merge only when they touch (`lo <= end + 1`), so
            // a one-cell gap is never cleared and the cleared set is the same
            // as clearing each box.
            let mut run: Option<(usize, usize)> = None;
            for (c, cell) in row_cells.iter().enumerate().take(cols) {
                let meaningful = cell.ch != ' '
                    || cell.wide
                    || cell.underline != UnderlineStyle::None
                    // Strikethrough/overline draw visible lines through a
                    // space, so it carries meaning like underline.
                    || cell.strikethrough
                    || cell.overline
                    || cell.bg != bg;
                if !meaningful {
                    continue;
                }
                let (lo, hi) = (c.saturating_sub(1), c + 1);
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
                for (c, _) in spans {
                    if *c < cols {
                        self.clear_occ_neighborhood_span(r, c.saturating_sub(1), *c + 1);
                    }
                }
            }
        }
        // A closed box-drawn surface is a semantic UI region: agent welcome,
        // alert, picker and diff panels often have default-background
        // interiors that cell-local occupancy cannot tell from open field.
        self.mask_framed_regions(cells, rows, cols);
        self.have_scanned = true;
        self.last_epoch = epoch;
    }

    /// Sample the program's real on-screen output into the rain's glyph
    /// alphabet. Call right after [`Self::rescan_from_cells`] under the same
    /// damage gate, with the same rows plus the frame's typing bands, and only
    /// on live frames (`display_offset == 0`): scrolled-back snapshots are
    /// display-translated while the cursor is grid-space, so the band math only
    /// lines up live. The previous table persists otherwise.
    ///
    /// Sampling rules:
    /// * From [`Self::note_keystroke`] through an explicit
    ///   [`RainSignal::TurnStart`], sampling is deferred and the previous
    ///   output tape kept; occupancy still rescans.
    /// * Only occupied, non-wide cells count.
    /// * The typing surface is excluded via the visible-cursor band (row ±
    ///   [`CURSOR_BAND_ROWS`]) or, cursor hidden, the host-fed recently
    ///   damaged composer band. A visual heuristic, not secret storage.
    /// * Supported codepoints get a literal bitmap glyph in the dynamic ROM;
    ///   unsupported ones are skipped, never substituted.
    /// * Deterministic: the most recent [`MATERIAL_CAP`] supported codepoints
    ///   stay in row-major source order; their distinct sorted set assigns
    ///   stable ROM slots, so streaming rebakes only when a character enters or
    ///   leaves the visible sample.
    ///
    /// An empty sample clears the table and emits no glyph rain; the classic
    /// alphabet is only reachable via `output_material = false`.
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
        // Walked bottom-up and stopped at MATERIAL_CAP: the sample is the last
        // MATERIAL_CAP supported codepoints in row-major order, so collecting
        // in reverse (and reversing once) touches only the cells that
        // contribute. The row band gate is a pure function of `r`, so it
        // selects the same rows either way.
        'rows: for r in (0..rows).rev() {
            if Self::row_banded(r, rows, cursor, hidden_band) {
                continue;
            }
            let Some(row_cells) = cells.get(r) else {
                continue;
            };
            for cell in row_cells.iter().rev() {
                if cell.ch != ' ' && !cell.wide && rom::material_bitmap(cell.ch).is_some() {
                    // Bounded by MATERIAL_CAP, so no viewport-sized scratch is
                    // ever retained.
                    self.material_scratch.push(cell.ch);
                    if self.material_scratch.len() == MATERIAL_CAP {
                        break 'rows;
                    }
                }
            }
        }
        // Newest-first -> oldest->newest source order, the order the slot bake
        // and `semantic_material_index` read.
        self.material_scratch.reverse();
        let n = self.material_scratch.len();
        if n == 0 {
            self.clear_material_bank();
            return;
        }
        debug_assert!(n <= MATERIAL_CAP);

        self.material_slots_scratch.clear();
        // Prefer the most recently visible distinct characters when a very
        // rich screen exceeds the 64-slot atlas. Only after choosing that set
        // do we sort it for stable slot numbers; sorting first would bias the
        // bank toward low ASCII and silently discard lowercase output.
        for index in (0..n).rev() {
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
            // Which slots changed. Slot assignment is the distinct set, sorted,
            // so only slots whose character differs need their ROM glyph
            // re-authored and atlas tile re-baked; streaming usually turns over
            // a handful of characters.
            let changed = slot_change_mask(&self.material_chars, &self.material_slots_scratch);
            self.material_chars.clone_from(&self.material_slots_scratch);
            // Every writer of `self.rom` restarts the baker and `begin_frame`
            // restarts on a metric change, so a baker complete at a live metric
            // is a full bake of the current ROM. Patching only the changed tiles
            // therefore yields the same atlas bytes as a full rebuild, and
            // `rebake_tiles` advances the version by the same amount, so the
            // fingerprint is unchanged.
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
        for index in 0..n {
            let ch = self.material_scratch[index];
            if let Ok(slot) = self.material_chars.binary_search(&ch) {
                self.material.push(slot as u8);
            }
        }
    }

    /// Clockless advance for web hosts: accumulate host milliseconds; whole
    /// engine ticks are consumed by the next [`Self::emit`].
    pub fn advance_ms(&mut self, dt_ms: u64) {
        self.clock_ms = self.clock_ms.saturating_add(dt_ms);
    }

    /// Native tick: sync the internal clock from the injected instant (the
    /// epoch latches on first call), then emit. Field math never sees the
    /// `Instant` — determinism is `(seed, tick)` only.
    pub fn tick(
        &mut self,
        now: Instant,
        geom: EffectGeom,
        input: &RainTickInput<'_>,
        quads: &mut Vec<SpriteQuad>,
        add: &mut Vec<RainHalo>,
    ) -> u64 {
        self.sync_clock(now);
        self.emit(geom, input, quads, add)
    }

    /// Latch the epoch on first call and advance the clock monotonically.
    fn sync_clock(&mut self, now: Instant) {
        let epoch = *self.epoch.get_or_insert(now);
        let ms = now.saturating_duration_since(epoch).as_millis() as u64;
        if ms > self.clock_ms {
            self.clock_ms = ms;
        }
    }

    /// Suspended tick for hosts that skip emission entirely (alt-screen
    /// suppression, the perf load-shed latch): advance the clock and the
    /// weather only, with no bake, field walk or quads. Notes stop landing, so
    /// the weather starves to SLEEP after `idle_secs`, the drain completes, and
    /// [`Self::is_active`] self-disarms. Resuming rebuilds the weather from
    /// fresh notes.
    pub fn tick_suspended(&mut self, now: Instant) {
        self.sync_clock(now);
        self.step_suspended();
    }

    /// Clockless core of [`Self::tick_suspended`] (web hosts pair it with
    /// [`Self::advance_ms`], exactly like `emit`).
    pub fn step_suspended(&mut self) {
        // Activity notes are dropped while suspended: a keystroke would hold the
        // pane at CALM and keep a 12 Hz timer armed for a pane that draws
        // nothing. The weather starves to SLEEP from the last pre-suspend note.
        self.pending_key = false;
        self.pending_content_credit = 0;
        // Deadline-stamp notes (bell, exit status, reading scroll) stay pending
        // and apply at the resume emit with a fresh window; none holds
        // `is_active` true.
        self.advance_engine_ticks();
        self.last_emit_nonempty = false;
    }

    /// Advance whole engine ticks out of the accumulated clock, then fill the
    /// frame's rain quads and additive halos. Returns the fingerprint (FNV-1a
    /// over every quad field with the frame term folded once; `0` when empty).
    pub fn emit(
        &mut self,
        geom: EffectGeom,
        input: &RainTickInput<'_>,
        quads: &mut Vec<SpriteQuad>,
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
        // Gate first: a scrolled-back, alt-quiet, drained or unscanned frame
        // emits nothing, so the bake and column-order rebuild are skipped; only
        // weather/tick advancement always runs. The baker resumes on the next
        // ungated frame (≤ 8 tiles/tick).
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
        // Literal mode is strict: without sampled characters there is nothing
        // to draw, which also skips all bake/field cost on a blank or
        // unsupported screen.
        let material_ready = !self.cfg.output_material || !self.material.is_empty();
        if !gated && material_ready {
            self.baker.begin_frame(geom.cell_w, geom.cell_h);
            if !self.baker.complete() && !self.material.is_empty() {
                // A literal charset becomes visible as one complete generation:
                // never mix new slot indices with old/blank atlas tiles.
                self.finish_material_bake();
            } else if !self.baker.complete() && (ticked || self.last_bake_tick != Some(self.tick)) {
                let rom = self.rom.get_or_insert_with(|| decorative_master().clone());
                self.baker.bake_tiles(rom);
                self.last_bake_tick = Some(self.tick);
            }
            self.ensure_col_order(geom.cols, geom.rows);
            self.emit_field(geom, input, quads, add);
            // The renderer's per-row dirty merge-diff needs `rain_quads`
            // row-sorted, but emission walks hash-ordered columns. A stable
            // counting sort into resident scratch keeps within-row order a
            // function of that row alone (an unstable sort would make unchanged
            // rows read dirty whenever other rows changed). O(n + rows),
            // allocation-free after warmup.
            self.row_sort_stable(quads, usize::from(geom.rows));
        }
        let fp = fingerprint(quads, add, self.tick, self.baker.version());
        self.last_emit_nonempty = fp != 0;
        fp
    }

    /// Whether the host should keep the rain timer armed. Sleep + drained +
    /// empty ⇒ `false` (the timer disarms; idle is 0 wakes).
    #[must_use]
    pub fn is_active(&self) -> bool {
        if !self.cfg.enabled || self.reduced_motion {
            return false;
        }
        if self.cfg.output_material && self.material.is_empty() {
            return false;
        }
        if self.last_emit_nonempty {
            return true;
        }
        // A reading-gated pane with an empty frame holds still; the reads that
        // end the gate (scroll input, return to live) are redraws that re-arm.
        if self.reading_gated {
            return false;
        }
        match self.visibility {
            RainVisibility::Focused => {
                self.eff_weather() != RainWeather::Sleep || self.drain_ticks < DRAIN_TICKS
            }
            // A non-focused pane exists only to finish its bounded drain: once
            // drained and empty it holds still even while content streams, until
            // refocus resets the drain.
            _ => self.drain_ticks < DRAIN_TICKS,
        }
    }

    /// The versioned 64-tile atlas for this frame's `rain_quads`. `None`
    /// until the first bake — rain-free frames carry no atlas.
    pub fn rain_atlas(&mut self) -> Option<Arc<SceneAtlas>> {
        self.baker.atlas()
    }

    /// The atlas version (folded into the fingerprint; exposed for hosts).
    #[must_use]
    pub fn atlas_version(&self) -> u64 {
        self.baker.version()
    }

    /// One-line diagnostic for host introspection (`aterm ctl rain status`):
    /// effective weather, density byte, tick, Tier-A scan residency, literal
    /// alphabet size, whether the last emit produced light, visibility, drain
    /// ticks, content seq and streak. Pure read.
    #[must_use]
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
            self.last_seq
                .map_or_else(|| "none".into(), |s| s.to_string()),
            self.content_streak,
        )
    }

    /// What the weather has heard from its host, for tests: the semantic
    /// phase the last applied signal set (an Execute edge sets
    /// [`RainSignal::Execute`]) and whether a failed command's ember hold is
    /// live. Both move only when a note reached the engine and a frame
    /// applied it.
    #[cfg(test)]
    pub(crate) fn heard_for_test(&self) -> (RainSignal, bool) {
        (self.semantic_phase, self.clock_ms < self.fail_until_ms)
    }

    #[cfg(test)]
    pub(crate) fn literal_material_chars_for_test(&self) -> &[char] {
        &self.material_chars
    }

    #[cfg(test)]
    pub(crate) fn config_for_test(&self) -> RainConfig {
        self.cfg
    }

    // -- internals -------------------------------------------------------------

    /// Stamp pending host notes at the freshest clock (a note after a long
    /// idle gap lands now, not at the stale pre-gap clock).
    fn apply_pending_notes(&mut self) {
        let now = self.clock_ms;
        // Keystrokes stamp first so a same-frame echo sees the fresh key time.
        if self.pending_key {
            self.pending_key = false;
            self.last_key_ms = Some(now);
        }
        // Enter/submit starts a new observable turn. Its first real response can
        // land in the same present as the echoed newline, so it must not inherit
        // the editor-key discount. Reset the prior turn's streak before applying
        // this frame's content credit; a genuine coalesced response then counts.
        if std::mem::take(&mut self.pending_turn_start) {
            self.last_key_ms = None;
            self.last_content_ms = None;
            self.content_streak = 0;
        }
        let credit = std::mem::take(&mut self.pending_content_credit);
        if credit > 0 {
            // Echo discount: readline and TUIs can repaint a whole suffix after
            // one delete, and repeat keys coalesce before a present, so the
            // whole immediate frame is attributed to echo; sustained agent output
            // proves itself on the next content-only frame.
            let echo = self
                .last_key_ms
                .is_some_and(|k| now.saturating_sub(k) < ECHO_DISCOUNT_MS);
            let content_credit = if echo { 0 } else { credit };
            if content_credit > 0 {
                self.content_streak = match self.last_content_ms {
                    Some(prev) if now.saturating_sub(prev) <= STREAK_WINDOW_MS => self
                        .content_streak
                        .saturating_add(u32::from(content_credit)),
                    _ => u32::from(content_credit),
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
        if let Some(failed) = self.pending_exit.take() {
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
        if let Some((signal, energy)) = self.pending_signal.take() {
            let phase_changed = self.semantic_phase != signal;
            self.semantic_phase = signal;
            self.semantic_energy = energy.min(SEMANTIC_ENERGY_CAP);
            self.semantic_ticks_left = SEMANTIC_HOLD_TICKS;
            self.semantic_started_ms = now;
            if phase_changed {
                // Repeated evidence for one still-active phase extends its
                // bounded hold without reseeding the tape lanes or popping art.
                self.semantic_seq = self.semantic_seq.wrapping_add(1);
            }
            // Agent hook outcomes reuse the same limiter-safe visual language
            // as authenticated OSC 133 command outcomes.
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

    /// The current tick-consumption period in ms, for the host's timer: WORKING
    /// runs at the fps knob; CALM, SLEEP and drain gate at 12 Hz, so the timer
    /// does not wake for ticks that advance nothing.
    #[must_use]
    pub fn current_period_ms(&self) -> u64 {
        u64::from(match self.eff_weather() {
            RainWeather::Working => self.tick_ms,
            _ => self.tick_ms.max(CALM_TICK_MS),
        })
        .max(1)
    }

    /// Remaining milliseconds until the next engine tick. Unlike the raw
    /// period, this preserves partial progress when a PTY/input redraw lands
    /// between rain frames, so web timers neither gap nor double-step.
    #[must_use]
    pub fn next_tick_in_ms(&self) -> u64 {
        let period = self.current_period_ms();
        let backlog = self.clock_ms.saturating_sub(self.consumed_ms);
        period.saturating_sub(backlog.min(period))
    }

    /// Weather capped by visibility (`min`: unfocused ≤ CALM, hidden = SLEEP).
    fn eff_weather(&self) -> RainWeather {
        let cap = match self.visibility {
            RainVisibility::Focused => RainWeather::Working,
            RainVisibility::VisibleUnfocused => RainWeather::Calm,
            RainVisibility::Hidden => RainWeather::Sleep,
        };
        self.weather.min(cap)
    }

    /// Consume whole engine ticks from the accumulated clock; returns whether
    /// any elapsed. Backlogs beyond [`CATCHUP_MS`] fast-forward arithmetically
    /// (the field is a pure function of tick; only weather is replayed, and it
    /// converges).
    fn advance_engine_ticks(&mut self) -> bool {
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

    /// One engine tick: weather dwell + EMA staircase + drain + wave limiter.
    fn step_engine_tick(&mut self) {
        self.tick += 1;
        let now = self.consumed_ms;
        if self.semantic_ticks_left > 0 && now >= self.semantic_started_ms {
            self.semantic_ticks_left -= 1;
            if self.semantic_ticks_left == 0 {
                self.semantic_phase = RainSignal::AssistantStream;
                self.semantic_energy = 0;
            }
        }
        // Weather transitions with ≥ 1 s dwell hysteresis.
        let desired = self.desired_weather(now);
        if desired != self.weather && now.saturating_sub(self.weather_since_ms) >= DWELL_MS {
            // Any transition out of Working is a completed turn, including a
            // direct Working→Sleep jump at the idle_secs minimum (where Calm is
            // skipped).
            if self.weather == RainWeather::Working
                && desired != RainWeather::Working
                && self.cfg.turn_wave
                && !self.reduced_motion
                && self.visibility == RainVisibility::Focused
            {
                self.wave_pending = true;
            }
            self.weather = desired;
            self.weather_since_ms = now;
        }
        let eff = self.eff_weather();
        // Density EMA staircase: 10.6 fixed point, `ema += (target - ema)/8`.
        let target = i32::from(self.cfg.density)
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
        // Turn wave through the flash limiter (≤ 2 ignitions per rolling
        // second; delayed, not dropped). Checked before the drain update so a
        // direct Working→Sleep turn ignites on its transition tick and plays
        // over the draining field.
        if self.wave_pending && self.drain_ticks == 0 {
            let live = self
                .ign_ms
                .iter()
                .filter(|&&t| t != u64::MAX && now.saturating_sub(t) < 1000)
                .count();
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
                let len = (WAVE_MS / self.current_period_ms()).max(1) as u32;
                self.wave = Some((self.tick, len));
                self.wave_pending = false;
            }
        }
        // Mandatory drain: unfocused or sleeping panes decay to empty within
        // DRAIN_TICKS regardless of geometry.
        if self.visibility == RainVisibility::Hidden {
            self.drain_ticks = DRAIN_TICKS;
        } else if self.visibility != RainVisibility::Focused || eff == RainWeather::Sleep {
            self.drain_ticks = (self.drain_ticks + 1).min(DRAIN_TICKS);
        } else {
            self.drain_ticks = 0;
        }
        if let Some((start, len)) = self.wave
            && self.tick.saturating_sub(start) > u64::from(len)
        {
            self.wave = None;
        }
    }

    /// The weather the inputs ask for at `now` (before dwell/caps).
    fn desired_weather(&self, now: u64) -> RainWeather {
        let content_recent = self
            .last_content_ms
            .is_some_and(|t| now.saturating_sub(t) < WORKING_HOLD_MS)
            && self.content_streak >= 2;
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
        let idle_ms = u64::from(self.cfg.idle_secs) * 1000;
        let any_recent = [self.last_content_ms, self.last_key_ms]
            .iter()
            .flatten()
            .any(|&t| now.saturating_sub(t) < idle_ms);
        if any_recent {
            RainWeather::Calm
        } else {
            RainWeather::Sleep
        }
    }

    /// Rebuild the hash-ascending column order on a width, row or seed change: the
    /// stride covers the full width (never a prefix — no dead right margin),
    /// and budget truncation drops whole highest-hash columns from the tail.
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
        // Rebuild the per-column ColParams memo on the same trigger (it depends
        // only on seed/rows/tick_ms/speed/trail). Indexed by column.
        let fp = self.field_params(rows);
        self.col_params_cache.clear();
        self.col_params_cache
            .extend((0..u32::from(cols)).map(|c| col_params(&fp, c)));
    }

    fn field_params(&self, rows: u16) -> FieldParams {
        FieldParams {
            seed32: self.seed32,
            rows: u32::from(rows),
            tick_ms: self.tick_ms,
            speed: u32::from(self.cfg.speed),
            trail: u32::from(self.cfg.trail),
            mq: self.mq,
            dq: self.dq,
            sq: self.sq,
        }
    }

    /// Stable row bucket sort (see the call site): per-row counts, prefix sums
    /// into scatter offsets, and replay of emission order within each row.
    /// `quads` rows are `< rows` (emit_column clamps).
    fn row_sort_stable(&mut self, quads: &mut [SpriteQuad], rows: usize) {
        if quads.len() < 2 || rows == 0 {
            return;
        }
        self.row_starts.clear();
        self.row_starts.resize(rows + 1, 0);
        for q in quads.iter() {
            self.row_starts[usize::from(q.row) + 1] += 1;
        }
        for r in 0..rows {
            self.row_starts[r + 1] += self.row_starts[r];
        }
        self.sort_scratch.clear();
        self.sort_scratch.extend_from_slice(quads);
        for q in &self.sort_scratch {
            let slot = &mut self.row_starts[usize::from(q.row)];
            quads[*slot as usize] = *q;
            *slot += 1;
        }
    }

    fn emit_field(
        &mut self,
        geom: EffectGeom,
        input: &RainTickInput<'_>,
        quads: &mut Vec<SpriteQuad>,
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
            // Frame invariants: self.tick/mq/dq are constant across the emit
            // (mq/dq only rebuilt in set_config; tick only advanced in tick).
            glyph_epoch: glyph_epoch(self.tick, self.mq),
            dither_epoch: dither_epoch(self.tick, self.dq),
        };
        for i in 0..self.col_order.len() {
            let col = self.col_order[i];
            let q0 = quads.len();
            let h0 = self.halo_cands.len();
            if !self.emit_column(&ctx, col, quads) {
                // Over budget: drop this (and every later = higher-hash)
                // column whole — reads as lower density, never a cut margin.
                quads.truncate(q0);
                self.halo_cands.truncate(h0);
                break;
            }
        }
        self.emit_halos(&ctx, add);
    }

    /// Emit one column's lit cells. Returns `false` when the quad budget was
    /// hit (the caller rolls the column back).
    fn emit_column(&mut self, ctx: &EmitCtx<'_>, col: u16, quads: &mut Vec<SpriteQuad>) -> bool {
        let cw = u32::from(ctx.geom.cell_w);
        let rows = u32::from(ctx.geom.rows);
        let x0 = u32::from(col) * cw;
        if x0 + cw > u32::from(u16::MAX) {
            return true; // beyond the u16 pixel space; nothing to draw
        }
        // Tick-independent memo (rebuilt with col_order); ColParams is Copy so
        // this copies out — no borrow held while pushing to `quads`.
        let cp = self.col_params_cache[usize::from(col)];
        let cv = cycle_at(&cp, &ctx.fp, self.tick);
        let density = self.ring.at(cycle_start_tick(&cp, cv.k));
        if !cycle_active(cv.hk, density) {
            return true;
        }
        // The output-tape walk's start is a function of the column alone:
        // hashed once here, not once per lit row.
        let material_start = (!self.material.is_empty()).then(|| self.material_start(col));
        let r_hi = cv.head_row.min(rows.saturating_sub(1));
        let r_lo = cv.head_row.saturating_sub(cp.l);
        if r_lo <= r_hi && r_lo < rows {
            for r in r_lo..=r_hi {
                // Cheapest reject first: an occupied cell needs no trail math.
                if !self.occ_bit(r, col) {
                    continue;
                }
                let Some(lvl0) = trail_level(&cp, cv.eff_tick, cv.k, r) else {
                    continue;
                };
                let is_head = r == cv.head_row;
                let Some(lvl) = self.drained_level(ctx, &cp, lvl0, r, is_head) else {
                    continue;
                };
                if !self.cell_unmasked(r, col, ctx.input) {
                    continue;
                }
                if quads.len() >= ctx.cap {
                    return false;
                }
                let head_flash = is_head && lvl == 15;
                quads.push(self.cell_quad(ctx, col, r, lvl, head_flash, material_start));
                if head_flash && cv.bright {
                    self.halo_cands.push((r as u16, col));
                }
            }
        }
        // Turn wave: a phase-aligned extra head sweeping the viewport over
        // every active column (skipping rows the trail already lit).
        if let Some(wr) = ctx.wave_row
            && wr < rows
            && trail_level(&cp, cv.eff_tick, cv.k, wr).is_none()
            && self.cell_eligible(wr, col, ctx.input)
        {
            let lvl = 15u32.saturating_sub(ctx.pen);
            if quads.len() >= ctx.cap {
                return false;
            }
            quads.push(self.cell_quad(ctx, col, wr, lvl, true, material_start));
        }
        true
    }

    /// Apply the drain penalty + flicker dither to a base trail level.
    fn drained_level(
        &self,
        ctx: &EmitCtx<'_>,
        cp: &ColParams,
        lvl0: u32,
        row: u32,
        is_head: bool,
    ) -> Option<u32> {
        let mut lvl = i64::from(lvl0) - i64::from(ctx.pen);
        if lvl < 0 {
            return None;
        }
        if !is_head {
            lvl = (lvl - i64::from(dither(cp.h, row, ctx.dither_epoch))).max(0);
        }
        Some(lvl as u32)
    }

    /// One resolved rain cell quad (tile rect from the baker, tint/alpha from
    /// the ramp, `flip_x` from the glyph hash). With a live output-material
    /// table, adjacent rows in a column walk adjacent entries of the sampled
    /// output sequence from a per-column hash start advanced by `glyph_epoch`,
    /// so codepoints keep their sampled adjacency. Literal glyphs are never
    /// mirrored. `material_start` is [`Self::material_start`], `Some` exactly
    /// while the table is live.
    fn cell_quad(
        &self,
        ctx: &EmitCtx<'_>,
        col: u16,
        row: u32,
        lvl: u32,
        head: bool,
        material_start: Option<usize>,
    ) -> SpriteQuad {
        let (cw, ch) = (u32::from(ctx.geom.cell_w), u32::from(ctx.geom.cell_h));
        let (g, flip_x) = match material_start {
            None => glyph_from_epoch(self.seed32, row, u32::from(col), ctx.glyph_epoch),
            Some(start) => {
                let ix = self.semantic_material_index(start, col, row, ctx.glyph_epoch);
                (u32::from(self.material[ix]), false)
            }
        };
        let (ax, ay) = self.baker.tile_origin(g);
        let alpha = if head {
            self.head_alpha
        } else {
            self.alpha_of(lvl)
        };
        SpriteQuad {
            row: row as u16,
            x: (u32::from(col) * cw) as u16,
            y: (row * ch) as u16,
            w: cw as u16,
            h: ch as u16,
            ax,
            ay,
            aw: cw as u16,
            ah: ch as u16,
            tint: ctx.ramp[(lvl as usize).min(15)],
            alpha,
            flip_x,
        }
    }

    /// Map an emitted cell onto the output tape. By default this is the plain
    /// source-order walk. A live semantic pulse groups nearby columns into a
    /// coherent lane and changes only the traversal: inspect pairs scout
    /// together, modify pairs counterflow, execute groups advance briskly,
    /// network groups form two-cell packets, waiting holds. `start` is
    /// [`Self::material_start`].
    fn semantic_material_index(&self, start: usize, col: u16, row: u32, glyph_epoch: u32) -> usize {
        let len = self.material.len();
        debug_assert!(len > 0);
        let row = row as usize;
        let epoch = glyph_epoch as usize;
        let offset = match self.semantic_phase {
            RainSignal::Inspect => row.wrapping_add(epoch),
            RainSignal::Modify if col % 2 == 1 => len.wrapping_sub(row.wrapping_add(epoch) % len),
            RainSignal::Modify => row.wrapping_add(epoch),
            RainSignal::Execute => row.wrapping_add(epoch.wrapping_mul(2)),
            RainSignal::Network => row / 2 + epoch,
            RainSignal::Branch => row
                .wrapping_add(epoch)
                .wrapping_add(usize::from(col % 3) * (len / 3).max(1)),
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
    fn material_start(&self, col: u16) -> usize {
        if self.semantic_phase == RainSignal::AssistantStream {
            return field::rain_hash32(u32::from(col) ^ self.seed32 ^ 0x00C0_FFEE) as usize;
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
        let lane_width =
            (base_width + u16::from(self.semantic_energy.saturating_sub(1) / 3)).min(8);
        let lane = u32::from(col / lane_width.max(1));
        field::rain_hash32(
            lane ^ self.seed32 ^ self.semantic_seq.wrapping_mul(0x9E37_79B9) ^ 0x005E_A11C,
        ) as usize
    }

    /// Body coverage for a trail level: linear in level, capped at the
    /// derived body alpha (level 15 body cells sit just under the head).
    fn alpha_of(&self, lvl: u32) -> u8 {
        ((u32::from(self.body_alpha) * (lvl + 1)) / 16).max(1) as u8
    }

    /// Recognize closed Unicode box-drawn surfaces and clear their interiors
    /// from the occupancy bitset. Top discovery and bottom-edge prefix sums are
    /// one pass each, so it is O(rows * cols) with bounded candidate storage. A
    /// region needs both sides on ≥ 75% of interior rows and box rule on ≥ 75%
    /// of its bottom edge. Title text may interrupt the top rule; arbitrary
    /// text cannot impersonate a bottom edge. Exceeding a bound suppresses rain
    /// for this rescan.
    fn mask_framed_regions(&mut self, cells: &[Vec<RenderCell>], rows: usize, cols: usize) {
        self.frame_candidates.clear();
        self.frame_regions.clear();

        for r in 0..rows {
            let row = cells.get(r).map(Vec::as_slice).unwrap_or(&[]);
            let ch_at = |col: usize| row.get(col).map_or(' ', |cell| cell.ch);

            // The bottom-edge prefix sums are read only by the candidate loop,
            // so build them only when a candidate exists (an ordinary text
            // screen has none).
            if !self.frame_candidates.is_empty() {
                self.frame_horizontal_prefix.resize(cols + 1, 0);
                self.frame_horizontal_prefix[0] = 0;
                for col in 0..cols {
                    self.frame_horizontal_prefix[col + 1] = self.frame_horizontal_prefix[col]
                        + usize::from(frame_horizontal(ch_at(col)));
                }
            }

            let mut write = 0usize;
            for i in 0..self.frame_candidates.len() {
                let mut candidate = self.frame_candidates[i];
                let left = ch_at(candidate.left);
                let right = ch_at(candidate.right);
                if frame_bottom_left(left) && frame_bottom_right(right) && r > candidate.top + 1 {
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
            for col in 0..cols {
                let ch = ch_at(col);
                // One range compare rejects ordinary cells (see `in_box_drawing_block`).
                if !in_box_drawing_block(ch) {
                    continue;
                }
                if frame_top_left(ch) {
                    open_left = Some(col);
                    has_horizontal = false;
                    continue;
                }
                let Some(left) = open_left else {
                    continue;
                };
                if frame_horizontal(ch) {
                    has_horizontal = true;
                }
                if col >= left + 2 && frame_top_right(ch) {
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
                            right: col,
                            side_rows: 0,
                            gap_rows: 0,
                        });
                    }
                    open_left = None;
                    has_horizontal = false;
                }
            }
        }

        for i in 0..self.frame_regions.len() {
            let region = self.frame_regions[i];
            for row in region.top + 1..region.bottom {
                self.clear_occ_span(row, region.left + 1, region.right.saturating_sub(1));
            }
        }
    }

    /// Clear one inclusive row span with at most two masked word writes plus a
    /// zero fill. This keeps a large framed panel O(rows + bitset words), not a
    /// per-cell branch in every animation tick.
    fn clear_occ_span(&mut self, row: usize, start_col: usize, end_col: usize) {
        if row >= self.occ_rows || start_col > end_col || start_col >= self.occ_cols {
            return;
        }
        let end_col = end_col.min(self.occ_cols - 1);
        let first_bit = row * self.occ_cols + start_col;
        let last_bit = row * self.occ_cols + end_col;
        let first_word = first_bit / 64;
        let last_word = last_bit / 64;
        let first_mask = u64::MAX << (first_bit % 64);
        let last_mask = u64::MAX >> (63 - (last_bit % 64));
        if first_word == last_word {
            self.occ[first_word] &= !(first_mask & last_mask);
            return;
        }
        self.occ[first_word] &= !first_mask;
        self.occ[first_word + 1..last_word].fill(0);
        self.occ[last_word] &= !last_mask;
    }

    /// Clear rows `row-1 ..= row+1` over one already dilated inclusive column
    /// interval (clamped to the grid) — the 3×3 neighborhood of every
    /// semantic cell in a run, as three masked word writes per interval.
    fn clear_occ_neighborhood_span(&mut self, row: usize, start_col: usize, end_col: usize) {
        if self.occ_rows == 0 || self.occ_cols == 0 {
            return;
        }
        let r0 = row.saturating_sub(1);
        let r1 = row.saturating_add(1).min(self.occ_rows - 1);
        for r in r0..=r1 {
            // `clear_occ_span` clamps `end_col` to the last column.
            self.clear_occ_span(r, start_col, end_col);
        }
    }

    /// The Tier-A occupancy bit alone (bounds-checked): the cheapest reject,
    /// hoisted ahead of the per-cell trail math in [`Self::emit_column`]; the
    /// trail loop then asks only [`Self::cell_unmasked`].
    #[inline]
    fn occ_bit(&self, row: u32, col: u16) -> bool {
        let (r, c) = (row as usize, usize::from(col));
        if r >= self.occ_rows || c >= self.occ_cols {
            return false;
        }
        let bit = r * self.occ_cols + c;
        (self.occ[bit / 64] >> (bit % 64)) & 1 == 1
    }

    /// Tier-A + Tier-B mask: occupancy bit, cursor band, live selection.
    fn cell_eligible(&self, row: u32, col: u16, input: &RainTickInput<'_>) -> bool {
        self.occ_bit(row, col) && self.cell_unmasked(row, col, input)
    }

    /// The Tier-B half of [`Self::cell_eligible`]: cursor band, live selection.
    fn cell_unmasked(&self, row: u32, col: u16, input: &RainTickInput<'_>) -> bool {
        // The composer band `sample_material` protects also holds for
        // emission, even before the damage ring warms. `row < geom.rows`, so it
        // fits a u16.
        if Self::row_banded(row as usize, self.occ_rows, input.cursor, input.hidden_band) {
            return false;
        }
        // Live selection (search highlight is the selection): applied per
        // quad, host-side, so CPU==GPU stays byte-identical.
        !input.sel.as_ref().is_some_and(|sv| {
            sv.sel
                .contains(i64::from(row) as i32 - sv.display_offset, col)
        })
    }

    /// The typing-surface band: visible cursor row ± [`CURSOR_BAND_ROWS`]; the
    /// host's recent-damage composer rows (protected even with a visible cursor,
    /// covering multiline input); and, with a hidden cursor and unknown band
    /// (first enable, resize, full damage), the bottom
    /// [`HIDDEN_CURSOR_BAND_ROWS`] rows.
    fn row_banded(r: usize, rows: usize, cursor: Option<(u16, u16)>, hidden_band: &[u16]) -> bool {
        let row = r as u16;
        cursor.is_some_and(|(cr, _)| i32::from(cr.abs_diff(row)) <= CURSOR_BAND_ROWS)
            || hidden_band.contains(&row)
            || (cursor.is_none() && hidden_band.is_empty() && r + HIDDEN_CURSOR_BAND_ROWS >= rows)
    }

    /// Grant bright-head halos round-robin (start offset rotates on the
    /// dither grid so no fixed column monopolizes the budget), whole halos
    /// only, hard-capped at [`MAX_RAIN_ADD`].
    fn emit_halos(&mut self, ctx: &EmitCtx<'_>, add: &mut Vec<RainHalo>) {
        if self.halo_cands.is_empty() {
            return;
        }
        let (cw, ch) = (i32::from(ctx.geom.cell_w), i32::from(ctx.geom.cell_h));
        let grid_w = i32::from(ctx.geom.cols) * cw;
        let grid_h = i32::from(ctx.geom.rows) * ch;
        // Halo light rides the head tint at half the head coverage,
        // premultiplied once, within the same ≤ 135 readability bound.
        let premul = premul_rgb(ctx.ramp[15], self.head_alpha / 2);
        if premul == 0 {
            return;
        }
        let len = self.halo_cands.len();
        let start = (self.tick / u64::from(self.dq.max(1))) as usize % len;
        for i in 0..len {
            let (r, c) = self.halo_cands[(start + i) % len];
            // A halo spans at most 3 row bands; grant whole halos only.
            if add.len() + 3 > MAX_RAIN_ADD {
                return;
            }
            let x0 = (i32::from(c) * cw - cw / 2).max(0);
            let x1 = (i32::from(c) * cw + cw + cw / 2).min(grid_w);
            let y0 = (i32::from(r) * ch - ch / 2).max(0);
            let y1 = (i32::from(r) * ch + ch + ch / 2).min(grid_h);
            if x1 <= x0 || y1 <= y0 || ch == 0 {
                continue;
            }
            // Radial falloff shared by every row-band quad of this halo: peaks at
            // the head-cell centre, zero at the box edge (an ellipse with
            // half-extents (cw, ch)). `cx`/`cy` are unclipped so the falloff
            // stays correct where the box is clamped.
            let cap = i32::from(u16::MAX);
            let cx = (i32::from(c) * cw + cw / 2).clamp(0, cap) as u16;
            let cy = (i32::from(r) * ch + ch / 2).clamp(0, cap) as u16;
            let rx = cw.clamp(1, cap) as u16;
            let ry = ch.clamp(1, cap) as u16;
            // Row-band split (the one-row-band invariant).
            let mut yy = y0;
            while yy < y1 {
                let band_row = yy / ch;
                let band_end = ((band_row + 1) * ch).min(y1);
                add.push(RainHalo {
                    row: band_row as u16,
                    x: x0 as u16,
                    y: yy as u16,
                    w: (x1 - x0) as u16,
                    h: (band_end - yy) as u16,
                    color: premul,
                    cx,
                    cy,
                    rx,
                    ry,
                    // `mode` defaults to `HaloMode::Add`: additive light.
                    ..Default::default()
                });
                yy = band_end;
            }
        }
    }
}

/// Defensive clamps (the gui resolver owns the real ones; the engine must be
/// safe for any embedder input).
fn clamp_config(mut cfg: RainConfig) -> RainConfig {
    cfg.fps = cfg.fps.clamp(12, 60);
    cfg.density = cfg.density.clamp(1, 12);
    cfg.speed = cfg.speed.clamp(1, 10);
    cfg.trail = cfg.trail.clamp(1, 10);
    cfg.mutation_ms = cfg.mutation_ms.clamp(80, 2000);
    cfg.idle_secs = cfg.idle_secs.clamp(2, 120);
    cfg.alpha_override = cfg
        .alpha_override
        .map(|a| a.clamp(RAIN_ALPHA_FLOOR, RAIN_ALPHA_CAP));
    cfg.head_alpha_override = cfg
        .head_alpha_override
        .map(|a| a.clamp(RAIN_ALPHA_FLOOR, RAIN_ALPHA_CAP));
    cfg
}

/// Quantize the EMA byte onto the density staircase (steps of 21 — one
/// WORKING density unit), so ring change-points stay a handful per front.
fn quantize_density(v: i32) -> u8 {
    (((v.max(0) + 10) / 21) * 21).min(255) as u8
}

/// Below this HSV chroma (`max − min` of the channels, 0..1) a colour has no
/// hue to give: `rgb2hsv` reports 0° (red) for an exact grey, so a neutral
/// foreground would otherwise seed saturated red rain a few degrees from
/// [`FAIL_HUE_DEG`], the tint that signals a failed command.
const THEME_HUE_MIN_CHROMA: f32 = 0.03;

/// The stock palette's ANSI blue (slot 4), the `hue = "theme"` seed of last
/// resort when neither the foreground nor the accent has a hue.
const THEME_HUE_FALLBACK: u32 = 0x003B_8EEA;

/// The `hue = "theme"` seed: the theme foreground, else its accent (the
/// cursor), else [`THEME_HUE_FALLBACK`], skipping any whose chroma is under
/// [`THEME_HUE_MIN_CHROMA`].
fn theme_hue(cfg: &RainConfig) -> f32 {
    [cfg.theme_fg, cfg.theme_accent]
        .into_iter()
        .map(rgb2hsv)
        .find(|&(_, s, v)| s * v >= THEME_HUE_MIN_CHROMA)
        .map_or_else(|| rgb2hsv(THEME_HUE_FALLBACK).0, |(h, _, _)| h)
}

/// The ramp hue for a config, kept ≥ 18° away from the Codex diff green.
fn ramp_hue(cfg: &RainConfig) -> f32 {
    let hue = match cfg.hue {
        RainHue::Matrix => MATRIX_HUE_DEG,
        RainHue::Theme => theme_hue(cfg),
        RainHue::Custom(c) => rgb2hsv(c).0,
    };
    let diff = rgb2hsv(DIFF_GREEN).0;
    let mut d = (hue - diff).rem_euclid(360.0);
    if d > 180.0 {
        d -= 360.0;
    }
    if d.abs() >= HUE_SEPARATION_DEG {
        return hue;
    }
    // Shift to the separation boundary on the side the hue already leans.
    if d >= 0.0 {
        diff + HUE_SEPARATION_DEG
    } else {
        diff - HUE_SEPARATION_DEG
    }
}

/// 16 ramp tints for a hue: light/dark polarity from the background
/// luminance (on light themes rain darkens toward the head; on dark themes
/// it brightens), head (index 15) desaturated toward the pale film flash.
fn build_ramp(hue: f32, cfg: &RainConfig) -> [u32; 16] {
    let light_bg = relative_luminance(cfg.default_bg) > 0.5;
    let mut ramp = [0u32; 16];
    for (i, slot) in ramp.iter_mut().enumerate() {
        let t = i as f32 / 15.0;
        let (s, v) = if light_bg {
            (0.85 - 0.25 * t, 0.62 - 0.42 * t)
        } else {
            (0.9 - 0.55 * t * t, 0.28 + 0.72 * t)
        };
        *slot = hsv2rgb(hue, s, v);
    }
    ramp
}

/// Re-value one ramp to match another's per-level luminance (≤ 8 correction
/// steps per tint) — the bell ALERT swap changes hue, not brightness.
fn luminance_match(mut ramp: [u32; 16], reference: [u32; 16]) -> [u32; 16] {
    for (slot, target) in ramp.iter_mut().zip(reference.iter()) {
        let lt = relative_luminance(*target);
        let (h, s, mut v) = rgb2hsv(*slot);
        for _ in 0..8 {
            let l = relative_luminance(*slot);
            if (l - lt).abs() < 0.01 {
                break;
            }
            v = (v * (lt + 0.02) / (l + 0.02)).clamp(0.02, 1.0);
            *slot = hsv2rgb(h, s, v);
        }
    }
    ramp
}

/// Composite `tint` at `alpha` OVER `bg`, integer sRGB-side per channel —
/// the CPU stamp's `(c·a + b·(255-a) + 127)/255` rounding, so the invariant
/// is checked on the bytes the renderer actually lands.
fn srgb_over(tint: u32, alpha: u8, bg: u32) -> u32 {
    let a = u32::from(alpha);
    let ch = |sh: u32| {
        let t = (tint >> sh) & 0xFF;
        let b = (bg >> sh) & 0xFF;
        (t * a + b * (255 - a) + 127) / 255
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// Derive `(body_alpha, head_alpha)` from the luminance constraint: the largest
/// coverages keeping every composited rain level closer to the background than
/// SGR-2 dim text, using the theme's real fg.
///
/// Faint text is `color_resolve::dim_toward_bg`: a linear-light mix
/// `L(dim) = L(bg) + m·(L(fg)-L(bg))` whose mix `m` is `color_resolve::faint_mix`,
/// the opacity (default 0.5) raised, when needed, until the faint ink keeps
/// `contrast(fg, bg)^opacity` (dark ink on a light ground would otherwise sink
/// into the mid-greys). The bound here still takes `m = 0.5`, the plain blend:
/// `faint_mix` only ever raises the mix, so faint text sits at least that far
/// from the background, and rain held under the plain blend stays closer to the
/// background than faint text on every theme. It is conservative (tighter than
/// the real faint ink on light themes, where the floor lifts `m`), never
/// optimistic, and needs no contrast solve per theme.
fn derive_alphas(cfg: &RainConfig, ramp: &[u32; 16]) -> (u8, u8) {
    let bg = cfg.default_bg;
    let lb = relative_luminance(bg);
    let bound = (0.5 * (relative_luminance(cfg.theme_fg) - lb)).abs();
    let level_ok = |body: u8| {
        (0..=15u32).all(|lvl| {
            let a = ((u32::from(body) * (lvl + 1)) / 16).max(1) as u8;
            (relative_luminance(srgb_over(ramp[lvl as usize], a, bg)) - lb).abs() < bound
        })
    };
    let head_ok = |a: u8| (relative_luminance(srgb_over(ramp[15], a, bg)) - lb).abs() < bound;
    let derive = |ok: &dyn Fn(u8) -> bool| {
        (RAIN_ALPHA_FLOOR..=RAIN_ALPHA_CAP)
            .rev()
            .find(|&a| ok(a))
            .unwrap_or(RAIN_ALPHA_FLOOR)
    };
    let mut body = cfg.alpha_override.unwrap_or_else(|| derive(&level_ok));
    let head = match cfg.head_alpha_override {
        Some(h) => h.clamp(body, RAIN_ALPHA_CAP),
        None => {
            // Jointly satisfiable ordering: if the pale head tint binds
            // tighter than the body, both drop to the head's bound.
            let h = derive(&head_ok);
            if h < body {
                body = h;
            }
            h.max(body)
        }
    };
    (body, head)
}

/// Which of the 64 dynamic-ROM slots change character between two sorted slot
/// tables (bit `i` ⇒ slot `i` must be re-authored and re-baked). A slot past the
/// end of a table holds the decorative glyph, so "absent from both" is unchanged
/// and "present in exactly one" is a change.
fn slot_change_mask(old: &[char], new: &[char]) -> u64 {
    let mut mask = 0u64;
    for slot in 0..rom::ROM_GLYPHS {
        if old.get(slot) != new.get(slot) {
            mask |= 1u64 << slot;
        }
    }
    mask
}

/// Frame fingerprint: FNV-1a over every field of every quad, each quad packed
/// losslessly into a few u64 words (injective, so every field participates).
/// The frame term folds exactly once mid-chain (never per quad, so an even
/// quad count cannot cancel it) and the atlas version at the head (a rebake
/// must repaint). Empty emission ⇒ 0. Not vectorized: changing the fold would
/// perturb every golden fingerprint for a negligible share of frame time.
fn fingerprint(quads: &[SpriteQuad], add: &[RainHalo], tick: u64, atlas_version: u64) -> u64 {
    if quads.is_empty() && add.is_empty() {
        return 0;
    }
    let mut h = fnv_mix_u64(FNV64_OFFSET, atlas_version);
    for q in quads {
        // Packs (row,x,y,w,h,ax,ay,aw,ah: u16; tint: u32; alpha: u8; flip_x:
        // bool) into 3 words with no bit overlap — all 185 bits preserved.
        let a = u64::from(q.row)
            | (u64::from(q.x) << 16)
            | (u64::from(q.y) << 32)
            | (u64::from(q.w) << 48);
        let b = u64::from(q.h)
            | (u64::from(q.ax) << 16)
            | (u64::from(q.ay) << 32)
            | (u64::from(q.aw) << 48);
        let c = u64::from(q.ah)
            | (u64::from(q.tint) << 16)
            | (u64::from(q.alpha) << 48)
            | (u64::from(q.flip_x) << 56);
        h = fnv_mix_u64(h, a);
        h = fnv_mix_u64(h, b);
        h = fnv_mix_u64(h, c);
    }
    h = fnv_mix_u64(h, tick.wrapping_mul(0x9E37_79B1));
    for g in add {
        // Packs (row,x,y,w,h: u16; color: u32) into 2 words, no overlap.
        let a = u64::from(g.row)
            | (u64::from(g.x) << 16)
            | (u64::from(g.y) << 32)
            | (u64::from(g.w) << 48);
        let b = u64::from(g.h) | (u64::from(g.color) << 16);
        h = fnv_mix_u64(h, a);
        h = fnv_mix_u64(h, b);
    }
    h
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use super::field::rain_hash32;
    use super::*;

    // ---- fixtures ----------------------------------------------------------

    fn cfg_on() -> RainConfig {
        RainConfig {
            enabled: true,
            // Most field/weather tests exercise the explicit classic mode.
            // Literal-mode tests opt in through `literal_cfg_on` and must
            // supply actual sampled material before emission.
            output_material: false,
            seed: 7,
            ..RainConfig::default()
        }
    }

    fn literal_cfg_on() -> RainConfig {
        RainConfig {
            output_material: true,
            ..cfg_on()
        }
    }

    fn geom(rows: u16, cols: u16, cw: u16, ch: u16) -> EffectGeom {
        EffectGeom {
            cell_w: cw,
            cell_h: ch,
            rows,
            cols,
        }
    }

    fn space_cell(bg: [u8; 3]) -> RenderCell {
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

    fn bg3() -> [u8; 3] {
        [0x11, 0x13, 0x18]
    }

    /// Rescan an all-empty grid (every cell rain-eligible).
    fn scan_empty(e: &mut MatrixRain, rows: usize, cols: usize) {
        let cells = vec![vec![space_cell(bg3()); cols]; rows];
        let sizes = vec![LineSize::SingleWidth; rows];
        e.rescan_from_cells(&cells, &sizes, &[], rows, cols, BG, 1);
    }

    fn idle() -> RainTickInput<'static> {
        RainTickInput::default()
    }

    /// Force a dense field now: pin the density staircase at 252 so cycle
    /// admission passes for ~98% of columns without waiting out the EMA.
    fn pour(e: &mut MatrixRain) {
        e.density_byte = 252;
        e.ring.push(e.tick, 252);
    }

    /// Rain emits over the canonical trimmed cell shape: blank rows are absent
    /// (short outer Vec) and trailing blanks are absent cells (short inner Vec),
    /// which the renderer paints `default_bg`. Absent cells are eligible.
    #[test]
    fn emits_over_trimmed_blank_rows() {
        let (rows, cols) = (10usize, 40usize);
        // Only row 0 carries content (12 block glyphs); rows 1..10 are
        // entirely absent, and row 0's cols 12..40 are absent too — the
        // trimmed shape. Block cells are non-space, so ineligible.
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
            // No quad may land in row 0's occupied cols 0..12 (block glyphs).
            assert!(
                q.iter().all(|quad| !(quad.row == 0 && (quad.x / 9) < 12)),
                "rain painted over the occupied block glyphs on row 0"
            );
        }
        assert!(any, "rain must emit over the trimmed blank rows");
    }

    /// One emit step after `dt` host milliseconds.
    fn step(
        e: &mut MatrixRain,
        g: EffectGeom,
        input: &RainTickInput<'_>,
        dt: u64,
        q: &mut Vec<SpriteQuad>,
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

    // ---- determinism -------------------------------------------------------

    /// Same seed + same tick stream ⇒ identical emission, across (a) two
    /// engines, and (b) the injected-Instant path vs the advance-by-dt path.
    #[test]
    fn deterministic_across_engines_and_clock_paths() {
        let g = geom(40, 60, 8, 16);
        let mut a = MatrixRain::new(cfg_on());
        let mut b = MatrixRain::new(cfg_on());
        let mut c = MatrixRain::new(cfg_on());
        scan_empty(&mut a, 40, 60);
        scan_empty(&mut b, 40, 60);
        scan_empty(&mut c, 40, 60);
        let t0 = Instant::now();
        let (mut qa, mut aa) = (Vec::new(), Vec::new());
        let (mut qb, mut ab) = (Vec::new(), Vec::new());
        let (mut qc, mut ac) = (Vec::new(), Vec::new());
        for i in 0..200u64 {
            if i % 3 == 0 {
                a.note_activity(i);
                b.note_activity(i);
                c.note_activity(i);
            }
            if i % 7 == 0 {
                a.note_keystroke();
                b.note_keystroke();
                c.note_keystroke();
            }
            // A rides the Instant path; B and C ride advance-by-dt.
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
            let fc = step(
                &mut c,
                g,
                &idle(),
                if i == 0 { 0 } else { 33 },
                &mut qc,
                &mut ac,
            );
            assert_eq!(fa, fb, "instant vs dt fingerprint diverged at step {i}");
            assert_eq!(qa, qb, "instant vs dt quads diverged at step {i}");
            assert_eq!(aa, ab, "instant vs dt halos diverged at step {i}");
            assert_eq!(
                (fb, &qb, &ab),
                (fc, &qc, &ac),
                "twin engines diverged at step {i}"
            );
        }
        // Non-vacuous: the drive actually rained at some point.
        assert!(a.last_emit_nonempty || a.tick > 0);
    }

    // ---- budgets -----------------------------------------------------------

    /// The emission never exceeds the geometry-derived quad cap or the halo
    /// budget, and the truncation branch actually fires (a downpour on a big
    /// grid wants far more quads than the cap).
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
                e.note_keystroke(); // keep CALM alive (no sleep drain mid-test)
                step(&mut e, g, &idle(), 83, &mut q, &mut a);
                assert!(q.len() <= cap, "quad budget exceeded: {} > {cap}", q.len());
                assert!(a.len() <= MAX_RAIN_ADD, "halo budget exceeded");
                // Max quads a single truncated column could have contributed.
                if q.len() >= cap - 202 {
                    hit_cap = true;
                }
            }
            assert!(hit_cap, "truncation branch never exercised at {cw}x{ch}");
        }
    }

    /// Truncation drops whole highest-hash columns: the emitted column set is
    /// a prefix of the hash-ascending column order.
    #[test]
    fn truncation_drops_whole_highest_hash_columns() {
        let g = geom(200, 500, 4, 8);
        let mut e = MatrixRain::new(RainConfig {
            density: 12,
            ..cfg_on()
        });
        scan_empty(&mut e, 200, 500);
        pour(&mut e);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert!(q.len() >= 2048 - 202, "expected a truncated downpour");
        let seed = e.seed32;
        let hash_of = |col: u16| rain_hash32(seed ^ ((u32::from(col) << 1) | 1));
        let max_emitted = q.iter().map(|s| hash_of(s.x / 4)).max().unwrap();
        // Every column whose hash exceeds the emitted maximum must be absent
        // (it was dropped whole) — no partial column ever survives.
        let emitted: std::collections::HashSet<u16> = q.iter().map(|s| s.x / 4).collect();
        for col in 0..500u16 {
            if hash_of(col) > max_emitted {
                assert!(
                    !emitted.contains(&col),
                    "column {col} should be dropped whole"
                );
            }
        }
    }

    // ---- damage shape -------------------------------------------------------

    /// Step-stable trail (design §4): on a non-mutation, non-dither-boundary
    /// tick, the changed cells per column are bounded by the head, the
    /// expiring tail, the former head (its dither re-arms), and the
    /// `ceil(15/p)` level-bucket crossings — never the whole band.
    #[test]
    fn step_stable_damage_shape() {
        let g = geom(50, 40, 8, 16);
        let mut e = MatrixRain::new(RainConfig {
            density: 12,
            ..cfg_on()
        });
        scan_empty(&mut e, 50, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        // Sustained content stream ⇒ WORKING (33ms engine ticks after the
        // CALM warmup + dwell), and a dense settled field.
        for i in 0..300u64 {
            e.note_activity(i + 1);
            step(&mut e, g, &idle(), 33, &mut q, &mut a);
        }
        assert_eq!(e.weather, RainWeather::Working);
        // Advance to a tick `t` where `t` and `t+1` share every field
        // quantum: same mutation bucket ((t+1) % mq != 0), same dither
        // bucket ((t+1) % dq != 0), same stammer window ((t+1) % sq != 0)
        // — a non-mutation tick, so only stepped rows may change.
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
        let snap = |quads: &[SpriteQuad]| {
            let mut m: HashMap<u16, HashMap<u16, SpriteQuad>> = HashMap::new();
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

    // ---- luminance invariant -------------------------------------------------

    /// Angular distance between two hues, degrees in `0..=180`.
    fn hue_gap(a: f32, b: f32) -> f32 {
        let d = (a - b).rem_euclid(360.0);
        d.min(360.0 - d)
    }

    /// `hue = "theme"` on the default theme (a neutral grey fg) is not the
    /// failure tint: an achromatic fg hands the hue to the accent, so the rain
    /// is not within 30° of [`FAIL_HUE_DEG`]. Negative control: the grey fg's
    /// own HSV hue, the old seed, is 0°, 8° from it.
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
        // A fg with a hue keeps it; a grey fg and a grey accent fall back to
        // the stock ANSI blue.
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

    /// Contrast invariant on both stock gui themes:
    /// `|L(rain over bg) - L(bg)| < |L(dim SGR-2 text) - L(bg)|` for every
    /// level at the derived alphas. Dark = `ColorScheme::default` in
    /// aterm-types/src/scheme.rs (fg #D0D0D0, bg #111318); light = the
    /// "GitHub Light" builtin (fg #1F2328, bg #FFFFFF).
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

    /// The plain-blend bound is conservative against the faint ink aterm-core
    /// actually draws (`dim_toward_bg`, whose `faint_mix` floor only raises
    /// the mix): every rain level stays closer to the background than real
    /// faint text, dark and light. The control: on a light theme the floor
    /// lifts faint ink well past the plain blend, so the two are not the same
    /// rule.
    #[test]
    fn plain_blend_bound_is_conservative_against_real_faint_ink() {
        use aterm_core::terminal::color_resolve::dim_toward_bg;
        let pack =
            |c: aterm_types::Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
        let mut lifted = false;
        for name in ["Default", "Aterm Day", "Linen", "Default Light"] {
            let scheme = aterm_types::scheme::builtin(name).expect("built-in");
            let (fg, bg) = (scheme.foreground, scheme.background);
            let faint = dim_toward_bg(fg, bg, aterm_types::DIM_FACTOR);
            let (fg, bg, faint) = (pack(fg), pack(bg), pack(faint));
            let lb = relative_luminance(bg);
            let plain = (0.5 * (relative_luminance(fg) - lb)).abs();
            let real = (relative_luminance(faint) - lb).abs();
            // u8 quantization may land a hair under the exact blend.
            assert!(
                real >= plain - 0.005,
                "{name}: faint {real} < plain {plain}"
            );
            lifted |= real > plain * 1.5;
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
    }

    /// The readable ceiling (the cursor-trail precedent) holds against
    /// out-of-range overrides.
    #[test]
    fn alpha_cap_is_pinned() {
        // Overrides clamp into 16..=135 and head >= body.
        let e = MatrixRain::new(RainConfig {
            alpha_override: Some(200),
            head_alpha_override: Some(3),
            ..cfg_on()
        });
        assert_eq!(e.body_alpha, 135);
        assert_eq!(e.head_alpha, 135, "head clamps up to body");
    }

    // ---- drain ---------------------------------------------------------------

    /// Every path to stillness reaches empty within the fixed bound and
    /// disarms `is_active` (mandatory sleep — no idle="keep" exists).
    #[test]
    fn drain_reaches_empty_within_bound_from_every_state() {
        let g = geom(60, 80, 8, 16);
        // (scenario name, prep) — each returns a raining engine.
        let rain_engine = || {
            let mut e = MatrixRain::new(RainConfig {
                density: 12,
                idle_secs: 2,
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
            e
        };
        // (a) idle → SLEEP: idle_secs + dwell + drain, generously bounded.
        {
            let mut e = rain_engine();
            let (mut q, mut a) = (Vec::new(), Vec::new());
            let mut empty_at = None;
            for i in 0..120u64 {
                let fp = step(&mut e, g, &idle(), 83, &mut q, &mut a);
                if fp == 0 && empty_at.is_none() {
                    empty_at = Some(i * 83);
                }
            }
            let at = empty_at.expect("idle engine must drain");
            assert!(at <= 6500, "idle drain took {at} ms");
            assert!(!e.is_active(), "drained engine must disarm the timer");
            // fp is byte-stable at 0 once drained.
            for _ in 0..5 {
                assert_eq!(step(&mut e, g, &idle(), 83, &mut q, &mut a), 0);
            }
        }
        // (b) unfocused → bounded drain even under continuing activity.
        {
            let mut e = rain_engine();
            e.set_visibility(RainVisibility::VisibleUnfocused);
            let (mut q, mut a) = (Vec::new(), Vec::new());
            let mut empty_at = None;
            for i in 0..80u64 {
                e.note_activity(1000 + i);
                let fp = step(&mut e, g, &idle(), 83, &mut q, &mut a);
                if fp == 0 && empty_at.is_none() {
                    empty_at = Some(i * 83);
                }
            }
            let at = empty_at.expect("unfocused engine must drain");
            assert!(at <= 3500, "unfocus drain took {at} ms");
        }
        // (c) hidden → immediate.
        {
            let mut e = rain_engine();
            e.set_visibility(RainVisibility::Hidden);
            let (mut q, mut a) = (Vec::new(), Vec::new());
            assert_eq!(step(&mut e, g, &idle(), 83, &mut q, &mut a), 0);
            assert!(q.is_empty(), "hidden pane drains immediately");
        }
    }

    // ---- fingerprint liveness --------------------------------------------------

    /// Animating ⇒ the fingerprint changes every engine tick; drained ⇒ it is
    /// the stable 0. Even quad counts must not cancel the frame term.
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

    /// Direct anti-cancellation pin: an even number of identical quads still
    /// yields a live, tick-varying fingerprint (the frame term folds once
    /// mid-chain, never per quad).
    #[test]
    fn even_quad_counts_do_not_cancel() {
        let quad = SpriteQuad {
            row: 3,
            x: 8,
            y: 48,
            w: 8,
            h: 16,
            ax: 0,
            ay: 0,
            aw: 8,
            ah: 16,
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

    // ---- mask -----------------------------------------------------------------

    /// Tier-A occupancy rules: space / wide / underline / non-default bg /
    /// image span / DECDWL line size, plus one-cell semantic clearance.
    #[test]
    fn occupancy_mask_predicates() {
        let mut e = MatrixRain::new(cfg_on());
        // 12 rows so the Tier-B band can live far from the probed cells (the
        // hidden-cursor fallback banding the bottom K rows is pinned by its
        // own test; here we probe the occupancy predicates in isolation).
        let rows = 12usize;
        let cols = 8usize;
        let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
        cells[0][1].ch = 'x'; // real glyph
        cells[2][2].ch = '　'; // wide lead (non-space char anyway)
        cells[2][3].wide = true; // wide continuation half
        cells[2][4].underline = UnderlineStyle::Single;
        cells[2][5].bg = [0x30, 0x30, 0x30]; // themed pill background
        cells[2][6].strikethrough = true; // SGR 9 line through a space
        cells[2][7].overline = true; // SGR 53 line over a space
        let mut sizes = vec![LineSize::SingleWidth; rows];
        sizes[1] = LineSize::DoubleWidth; // DECDWL: whole row ineligible
        // Inline image covering (3,3).
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

        cells[9][1].ch = ' ';
        cells[9][22].ch = ' ';
        for cell in &mut cells[9][2..22] {
            cell.ch = ' ';
        }
        rain.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 2);
        assert!(
            rain.cell_eligible(5, 12, &input),
            "an unclosed drawing must not hide ordinary terminal space"
        );
    }

    #[test]
    fn frame_detector_rejects_text_as_a_bottom_edge() {
        let (rows, cols) = (13usize, 26usize);
        let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
        cells[1][1].ch = '╭';
        cells[1][22].ch = '╮';
        for cell in &mut cells[1][2..22] {
            cell.ch = '─';
        }
        for row in cells.iter_mut().take(10).skip(2) {
            row[1].ch = '│';
            row[22].ch = '│';
        }
        cells[8][1].ch = '╰';
        cells[8][22].ch = '╯';
        for cell in &mut cells[8][2..22] {
            cell.ch = 'x';
        }
        cells[10][1].ch = '╰';
        cells[10][22].ch = '╯';
        for cell in &mut cells[10][2..22] {
            cell.ch = '─';
        }

        let sizes = vec![LineSize::SingleWidth; rows];
        let images = vec![Vec::new(); rows];
        let mut rain = MatrixRain::new(cfg_on());
        rain.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 1);
        let band = [12u16];
        let input = RainTickInput {
            hidden_band: &band,
            ..RainTickInput::default()
        };
        assert!(
            rain.cell_eligible(5, 12, &input),
            "corner glyphs around text must not close a framed surface"
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

    /// Output material bank: supported literal codepoints come from real screen
    /// content in source order, and the typing band (cursor ± 2 / hidden composer
    /// rows) is excluded.
    #[test]
    fn material_bank_samples_output_not_typing() {
        let rows = 6usize;
        let cols = 8usize;
        let mut e = MatrixRain::new(literal_cfg_on());
        // Hex-ish output on row 0; a "secret" on row 3 (the cursor band).
        let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
        for (i, ch) in "0f3a".chars().enumerate() {
            cells[0][i].ch = ch;
        }
        for (i, ch) in "hunter2".chars().enumerate() {
            cells[3][i].ch = ch;
        }
        // Cursor on row 3: rows 1..=5 are banded (±2) — only row 0 samples.
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

        // Hidden cursor: the composer band excludes row 3 the same way.
        e.sample_material(&cells, rows, None, &[3]);
        assert_eq!(e.material.len(), 4, "hidden band excludes the secret row");
        assert_eq!(decoded(&e), "0f3a");

        // Hidden cursor with an unknown band (empty ring — first enable /
        // resize / full damage): the bottom K rows are conservatively treated
        // as the composer, so a secret typed there is still never sampled.
        let mut bottom_secret = vec![vec![space_cell(bg3()); cols]; rows];
        for (i, ch) in "hunter2".chars().enumerate() {
            bottom_secret[rows - 1][i].ch = ch;
        }
        e.sample_material(&bottom_secret, rows, None, &[]);
        assert!(
            e.material.is_empty(),
            "unknown composer band ⇒ the bottom rows are never sampled"
        );

        // A live reload flipping the knob off clears the table immediately
        // (emission only consults emptiness — no waiting for the next damage).
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
        e.set_config(back_on); // restore for the cases below
        assert!(
            e.needs_material_sample(),
            "classic-to-literal reload requests a host sample without damage"
        );
        e.sample_material(&cells, rows, Some((5, 0)), &[]);
        assert!(!e.needs_material_sample());

        // With nothing outside the band, the table is empty.
        let mut only_secret = vec![vec![space_cell(bg3()); cols]; rows];
        for (i, ch) in "hunter2".chars().enumerate() {
            only_secret[3][i].ch = ch;
        }
        e.sample_material(&only_secret, rows, Some((3, 0)), &[]);
        assert!(e.material.is_empty(), "typing-only screens sample nothing");

        // The knob off ⇒ always empty.
        let mut cfg = cfg_on();
        cfg.output_material = false;
        let mut e2 = MatrixRain::new(cfg);
        e2.sample_material(&cells, rows, Some((3, 0)), &[]);
        assert!(
            e2.material.is_empty(),
            "output_material=false stays classic"
        );

        // Over-long screens cap at MATERIAL_CAP.
        let mut big = vec![vec![space_cell(bg3()); 64]; 8];
        for row in big.iter_mut().take(8) {
            for cell in row.iter_mut() {
                cell.ch = '7';
            }
        }
        e.sample_material(&big, 8, None, &[]);
        assert!(e.material.len() <= MATERIAL_CAP);
        assert!(!e.material.is_empty());
        assert_eq!(e.material_chars, vec!['7']);
        assert!(decoded(&e).chars().all(|ch| ch == '7'));
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

        let pattern = ['A', 'b', '3', '{', '}'];
        let mut long = vec![vec![space_cell(bg3()); 220]; rows];
        for (index, cell) in long[0].iter_mut().enumerate() {
            cell.ch = pattern[index % pattern.len()];
        }
        e.sample_material(&long, rows, Some((3, 0)), &[]);
        let expected: String = (220 - MATERIAL_CAP..220)
            .map(|index| pattern[index % pattern.len()])
            .collect();
        let actual: String = e
            .material
            .iter()
            .map(|&slot| e.material_chars[usize::from(slot)])
            .collect();
        assert_eq!(
            actual, expected,
            "the fixed ring preserves the exact source tail"
        );
        assert_eq!(e.material_scratch.len(), MATERIAL_CAP);

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

    /// The sampler walks bottom-up and stops at [`MATERIAL_CAP`]; it must match
    /// an exhaustive forward reference on a grid dense enough for the cap to
    /// land mid-row, with banded rows, wide cells, spaces, unsupported scalars
    /// and a short row mixed in.
    #[test]
    fn literal_material_tail_matches_an_exhaustive_forward_walk() {
        /// Reference: every unbanded row top-down, keep the tail.
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
                    0 => {}                     // stays a space
                    1 => cell.ch = '\u{1F408}', // unsupported: never sampled
                    2 => {
                        cell.ch = pattern[(r * cols + c) % pattern.len()];
                        cell.wide = true; // wide: never sampled
                    }
                    _ => cell.ch = pattern[(r * cols + c) % pattern.len()],
                }
            }
        }
        // A trimmed row and a grid shorter than `rows` are legal input; both
        // directions must skip exactly the same cells.
        cells[9].truncate(5);
        cells.truncate(rows - 1);

        for (cursor, band) in [
            (Some((2u16, 0u16)), &[][..]),
            (Some((2, 0)), &[7u16, 8][..]),
            (None, &[][..]),      // hidden cursor -> bottom-K fallback band
            (None, &[11u16][..]), // hidden cursor with a host-fed band
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

        // Under the cap the tail is the whole sample — no truncation, same order.
        let mut sparse = vec![vec![space_cell(bg3()); cols]; rows];
        for (c, cell) in sparse[8].iter_mut().take(9).enumerate() {
            cell.ch = pattern[c % pattern.len()];
        }
        let mut e = MatrixRain::new(literal_cfg_on());
        e.sample_material(&sparse, rows, Some((2, 0)), &[]);
        assert_eq!(
            e.material_scratch,
            forward_tail(&sparse, rows, Some((2, 0)), &[]),
            "a sub-cap sample keeps every hit in source order"
        );
        assert!(e.material_scratch.len() < MATERIAL_CAP);
    }

    /// Literal mode has no decorative escape hatch: until supported output is
    /// sampled, and again after that output disappears, the field is empty.
    /// The classic ROM remains available only through the explicit knob.
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

        let mut classic = MatrixRain::new(cfg_on());
        scan_empty(&mut classic, rows, cols);
        pour(&mut classic);
        for _ in 0..30 {
            step(&mut classic, g, &idle(), 33, &mut q, &mut a);
        }
        assert!(!q.is_empty(), "the explicit classic setting still works");
    }

    /// Sanitized PTY frames captured from Codex and Claude Code surfaces, run
    /// through the real parser (including Claude's alt screen and synchronized
    /// output): program output stays literal while the bottom composer draft is
    /// absent from the material tape.
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

    /// The bottom-region contract holds for emission with an unknown composer
    /// band: hidden cursor + empty damage ring means the bottom K rows receive
    /// no quad, even at full pour.
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

    /// Deadline-stamp notes survive suspension: a bell and an exit status noted
    /// during load-shed stay pending and apply at the resume emit; activity
    /// notes are dropped.
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
        // Resume: the stamps apply at the first real emit.
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

    /// A drained, unfocused pane disarms even while content streams; refocus
    /// resets the drain and resumes.
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
        // Keep streaming content the whole time — the drain must still
        // complete and the engine must still disarm.
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
        // Refocus while the weather is awake: the drain resets and the
        // field resumes (is_active true again).
        e.set_visibility(RainVisibility::Focused);
        assert!(e.is_active(), "refocus resumes an awake weather");
    }

    /// Reading disarms: a scrolled-back or alt-quiet pane with an empty frame
    /// reports inactive, and re-arms when a live emit runs.
    #[test]
    fn reading_gate_disarms_and_rearms() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for i in 0..30u64 {
            e.note_activity(i + 1);
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        assert!(e.is_active(), "awake and live");
        // Scroll back: the first gated emit erases the field (fp 0) and the
        // engine disarms even though the weather is still awake.
        let scrolled = RainTickInput {
            display_offset: 5,
            ..RainTickInput::default()
        };
        step(&mut e, g, &scrolled, 83, &mut q, &mut a);
        assert!(q.is_empty(), "reading frame is empty");
        assert!(!e.is_active(), "reading + empty frame ⇒ disarmed");
        // Return to live (a redraw by construction): active again.
        e.note_activity(1000);
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert!(e.is_active(), "live emit re-arms");
    }

    /// `can_emit` — the host's rescan-worth probe: false under reduced motion
    /// and for a non-focused pane past its drain; true for a focused pane.
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

    /// Exit status → weather: success queues the finishing sweep, failure
    /// holds the ember ramp, and the `exit_tint` knob keeps both inert.
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
        e.note_exit_status(true);
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert!(e.fail_until_ms > 0, "failure holds the ember tint");

        let mut cfg = cfg_on();
        cfg.exit_tint = false;
        let mut e2 = MatrixRain::new(cfg);
        scan_empty(&mut e2, 30, 40);
        e2.note_exit_status(true);
        step(&mut e2, g, &idle(), 83, &mut q, &mut a);
        assert_eq!(e2.fail_until_ms, 0, "knob off ⇒ inert");
        assert!(!e2.wave_pending, "knob off ⇒ no sweep either");
    }

    /// The emitted snapshot arrives row-sorted — the renderer's per-row dirty
    /// merge-diff walks contiguous row slices and debug_asserts this order
    /// (emission itself walks hash-ordered columns; `emit` sorts).
    #[test]
    fn emitted_quads_are_row_sorted() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        pour(&mut e);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for _ in 0..30 {
            step(&mut e, g, &idle(), 33, &mut q, &mut a);
            assert!(
                q.is_sorted_by_key(|quad| quad.row),
                "rain_quads must be row-sorted for the dirty merge-diff"
            );
        }
        assert!(!q.is_empty(), "the pour actually emitted");
    }

    /// With a live literal-material table every emitted rain quad draws a table
    /// tile unmirrored; both halves of the ordered 128-character tape are
    /// reachable across the field.
    #[test]
    fn material_remaps_emitted_glyphs_unmirrored() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(literal_cfg_on());
        scan_empty(&mut e, 30, 40);
        pour(&mut e);
        // Force a single-entry literal ROM: everything rains as '7'.
        e.material_chars = vec!['7'];
        e.rom = Some(rom::rasterize_material_master(&e.material_chars));
        e.baker.restart();
        e.material = vec![0];
        let (mut q, mut a) = (Vec::new(), Vec::new());
        // Enough steps for heads + trails to populate.
        for _ in 0..30 {
            step(&mut e, g, &idle(), 33, &mut q, &mut a);
        }
        assert!(!q.is_empty(), "field emits under pour");
        let (ax7, ay7) = e.baker.tile_origin(0);
        for quad in &q {
            assert_eq!(
                (quad.ax, quad.ay),
                (ax7, ay7),
                "every rained glyph is the material tile"
            );
            assert!(!quad.flip_x, "exact-form tiles are never mirrored");
        }

        // The full table is reachable (a `% 64` index would strand entries
        // 64..): 128 entries with a different back-half tile, both halves must
        // appear.
        e.material_chars = vec!['2', '7'];
        e.rom = Some(rom::rasterize_material_master(&e.material_chars));
        e.baker.restart();
        e.material = vec![0u8; 64];
        e.material.extend(std::iter::repeat_n(1u8, 64));
        let (mut q2, mut a2) = (Vec::new(), Vec::new());
        for _ in 0..30 {
            step(&mut e, g, &idle(), 33, &mut q2, &mut a2);
        }
        let (ax2, ay2) = e.baker.tile_origin(1);
        let front = q2.iter().any(|qd| (qd.ax, qd.ay) == (ax7, ay7));
        let back = q2.iter().any(|qd| (qd.ax, qd.ay) == (ax2, ay2));
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
        let slot_a = usize::from(a.ax) / 8;
        let slot_b = usize::from(b.ax) / 8;
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
    fn semantic_signal_numeric_abi_is_contiguous_and_pinned() {
        let signals = [
            RainSignal::AssistantStream,
            RainSignal::Inspect,
            RainSignal::Modify,
            RainSignal::Execute,
            RainSignal::Network,
            RainSignal::Branch,
            RainSignal::Waiting,
            RainSignal::Success,
            RainSignal::Failure,
            RainSignal::Interrupted,
            RainSignal::TurnStart,
        ];
        for (code, signal) in signals.into_iter().enumerate() {
            assert_eq!(signal as usize, code);
            assert_eq!(RainSignal::from_code(code as u32), Some(signal));
        }
        assert_eq!(RainSignal::from_code(signals.len() as u32), None);
    }

    #[test]
    fn semantic_work_wakes_drained_weather_then_expires_without_gaps() {
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

        for _ in 0..SEMANTIC_HOLD_TICKS {
            step(&mut rain, g, &idle(), 83, &mut q, &mut a);
        }
        assert_eq!(rain.semantic_phase, RainSignal::AssistantStream);
        assert_eq!(rain.semantic_ticks_left, 0);
    }

    /// Tier-B: a live selection zeroes emission in its band without any
    /// rescan (frozen damage epoch — selection never marks damage), and the
    /// cursor band masks ±2 rows (visible) or the host-fed damaged band
    /// (hidden cursor).
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
        // Full-viewport selection, same damage epoch ⇒ zero quads.
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
        // Visible cursor at row 10: rows 8..=12 carry nothing. Host-fed
        // multiline composer rows remain protected at the same time.
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
        // Hidden cursor: the host-fed damaged band is masked instead.
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

    /// Reading gates: scrollback (display_offset), alt-screen suppression
    /// knob, and the alt-screen scroll-quiet deadline.
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
        // Scrollback: any display offset yields clean text.
        let back = RainTickInput {
            display_offset: 3,
            ..idle()
        };
        assert_eq!(step(&mut e, g, &back, 83, &mut q, &mut a), 0);
        // Alt screen + wheel: the 3s scroll-quiet window gates emission…
        let alt = RainTickInput {
            is_alt_screen: true,
            ..idle()
        };
        e.note_alt_scroll();
        assert_eq!(step(&mut e, g, &alt, 83, &mut q, &mut a), 0);
        // …and expires: 3s later the alt screen rains again.
        for _ in 0..40 {
            e.note_keystroke();
            step(&mut e, g, &alt, 83, &mut q, &mut a);
        }
        assert!(!q.is_empty(), "scroll-quiet window must expire");
        // suppress_in_alt_screen pins the alt screen off permanently.
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

    // ---- weather ---------------------------------------------------------------

    /// Keystrokes alone cap at CALM; sustained content deltas reach WORKING;
    /// transitions respect the ≥1s dwell.
    #[test]
    fn weather_transitions_with_dwell_hysteresis() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        // 5 s of typing: never WORKING.
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
        // Sustained content stream on a fresh engine (its CALM state is
        // seconds-zero, so the up-transition itself proves the dwell):
        // WORKING, but never before 1 s in CALM.
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
        // Stream stops: CALM again after hold+dwell, never before 1s.
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

    /// Your own typing is a drizzle: a content delta within
    /// [`ECHO_DISCOUNT_MS`] of a keystroke is echo and never advances the
    /// WORKING streak, so typing stays CALM and fires no turn wave on a pause,
    /// while agent output clear of keystrokes still pours.
    #[test]
    fn typing_echo_never_pours() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        // Seed the observed grid clock, then model 10 s of typing/deleting at
        // ~12 Hz. Each key can repaint 32 cells (erase + suffix redraw), but the
        // whole immediate frame is interactive echo, not an agent stream.
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
        // The typist pauses: no spurious turn-complete wave (there was no
        // WORKING to complete).
        for _ in 0..40 {
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        assert!(e.wave.is_none(), "no turn wave from a typing pause");
        // Agent output clear of the keystrokes still pours.
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

    /// A synchronized TUI frame can coalesce a large interactive redraw after
    /// an editing key. Discount that whole frame, but an explicit submitted-turn
    /// boundary makes a same-present fast response count immediately. A clock
    /// rollback is a rebase, never a wrapped burst.
    #[test]
    fn coalesced_edit_echo_yields_to_submitted_turn_and_seq_rebases() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());

        e.note_activity(100); // baseline
        e.note_keystroke();
        e.note_activity(10_000); // large readline/TUI redraw after the key
        assert_eq!(
            e.pending_content_credit, CONTENT_CREDIT_CAP,
            "coalesced credit is hard-bounded"
        );
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert_eq!(
            e.content_streak, 0,
            "the complete immediate redraw is interactive echo"
        );

        // Enter and the first assistant/tool response can be coalesced into a
        // single fast present. TurnStart cancels only the editor-key discount;
        // the real bounded content distance must survive in full.
        e.note_keystroke();
        e.note_signal(RainSignal::TurnStart as u32, 4);
        e.note_activity(10_000 + u64::from(CONTENT_CREDIT_CAP));
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert_eq!(
            e.content_streak,
            u32::from(CONTENT_CREDIT_CAP),
            "submitted-turn output is not mistaken for editor echo"
        );
        for _ in 0..12 {
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        assert_eq!(e.weather, RainWeather::Working);

        e.note_activity(2); // fresh session/grid with a lower clock
        assert_eq!(e.pending_content_credit, 0, "rollback rebases without work");
        assert_eq!(
            e.content_streak, 0,
            "rollback clears the old session streak"
        );
        assert!(e.last_content_ms.is_none());
    }

    /// A suspended host keeps the weather advancing via the suspended tick:
    /// notes starve, the weather sleeps, the drain completes, and `is_active`
    /// self-disarms rather than waking forever off a frozen WORKING state.
    #[test]
    fn suspended_tick_starves_to_sleep_and_disarms() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        // Reach WORKING with a clean stream.
        for i in 0..60u64 {
            e.note_activity(i + 1);
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        assert_eq!(e.weather, RainWeather::Working);
        assert!(e.is_active(), "an awake engine keeps the timer armed");
        // Suspend while the user keeps typing (vim in the suppressed alt
        // screen): suspended steps drop those notes. After idle_secs (8 s) plus
        // the ~2.5 s drain the engine must disarm; 12 s of 83 ms steps covers
        // it.
        for _ in 0..145 {
            e.note_keystroke(); // typing throughout — dropped while suspended
            e.advance_ms(83);
            e.step_suspended();
        }
        assert_eq!(e.weather, RainWeather::Sleep, "notes starve to SLEEP");
        assert!(
            !e.is_active(),
            "a drained suspended engine must disarm (no perpetual wakes)"
        );
    }

    /// The turn wave survives the `idle_secs` clamp minimum: at 2 s the idle
    /// window equals WORKING_HOLD_MS, so a finished turn jumps WORKING→SLEEP
    /// directly (CALM is skipped) — the wave must still ignite on that
    /// transition tick, playing out over the draining field.
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
        // Silence: the turn ends. WORKING→SLEEP in one step at this clamp —
        // the completion wave must still fire.
        let mut saw_wave = false;
        for _ in 0..80 {
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
            saw_wave |= e.wave.is_some();
        }
        assert!(saw_wave, "turn wave must fire on a direct WORKING→SLEEP");
        assert_eq!(e.weather, RainWeather::Sleep);
    }

    /// The WORKING→CALM edge fires at most 2 turn waves per rolling second;
    /// excess waves are delayed (granted when the window frees), not dropped.
    #[test]
    fn turn_wave_limiter_caps_ignitions() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        let mut starts: Vec<u64> = Vec::new();
        let mut last_wave: Option<(u64, u32)> = None;
        // Demand a wave every other tick — far above the limiter budget.
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

    /// Reduced motion: nothing is emitted, fp 0, engine inactive.
    #[test]
    fn reduced_motion_emits_nothing() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(RainConfig {
            density: 12,
            ..cfg_on()
        });
        scan_empty(&mut e, 30, 40);
        pour(&mut e);
        e.set_reduced_motion(true);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for i in 0..10u64 {
            e.note_activity(i);
            assert_eq!(step(&mut e, g, &idle(), 83, &mut q, &mut a), 0);
            assert!(q.is_empty() && a.is_empty());
            assert!(!e.is_active());
        }
    }

    /// Disabled engine: byte-identical off (empty, fp 0, inactive).
    #[test]
    fn disabled_engine_is_byte_identical_off() {
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(RainConfig {
            enabled: false,
            ..cfg_on()
        });
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        assert_eq!(step(&mut e, g, &idle(), 100, &mut q, &mut a), 0);
        assert!(q.is_empty() && a.is_empty());
        assert!(!e.is_active());
        assert!(e.rain_atlas().is_none(), "no atlas is baked while disabled");
    }

    // ---- rescan gate --------------------------------------------------------

    #[test]
    fn needs_rescan_follows_the_damage_epoch() {
        let mut e = MatrixRain::new(cfg_on());
        assert!(e.needs_rescan(1), "unscanned engine always rescans");
        scan_empty(&mut e, 10, 10);
        assert!(!e.needs_rescan(1));
        assert!(e.needs_rescan(2), "epoch change re-arms the rescan");
    }

    /// A retained engine whose front grid was swapped (tab switch, pane focus
    /// move) rescans even when the two terminals' damage epochs collide:
    /// `note_grid_replaced` rebaselines occupancy, material and activity.
    #[test]
    fn note_grid_replaced_forces_rescan_despite_epoch_collision() {
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 10, 10);
        assert!(
            !e.needs_rescan(1),
            "scanned engine holds on an equal epoch (the collision)"
        );
        e.note_grid_replaced();
        assert!(
            e.needs_rescan(1),
            "grid replacement re-arms the rescan at the SAME epoch"
        );
    }

    /// A 1–2-row viewport (a heavily split pane) emits, or emits nothing,
    /// without panicking (`col_params` must not call `.clamp(3, rows)` with
    /// min > max). Every emitted quad stays inside the geometry.
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

    /// A latched composer-provenance guard does not survive a grid swap: with
    /// `material_editing` still true, the cleared bank could never refill and
    /// literal rain would stay dark until the next Enter.
    #[test]
    fn note_grid_replaced_releases_the_editing_latch() {
        let mut e = MatrixRain::new(literal_cfg_on());
        e.note_keystroke();
        assert!(e.material_editing_for_test(), "keystroke latches Editing");
        e.note_grid_replaced();
        assert!(
            !e.material_editing_for_test(),
            "the latch belongs to the OLD grid's draft — the swap releases it"
        );
        // And the released latch actually lets the new grid sample.
        let mut cells = vec![vec![space_cell(bg3()); 10]; 10];
        for (i, ch) in "0f3a".chars().enumerate() {
            cells[0][i].ch = ch;
        }
        e.sample_material(&cells, 10, None, &[]);
        assert!(
            !e.literal_material_chars_for_test().is_empty(),
            "the new grid's output refills the alphabet immediately"
        );
    }

    /// The material alphabet is grid-derived state: swapping grids drops the
    /// old session's sampled characters and re-arms a fresh literal sample.
    #[test]
    fn note_grid_replaced_clears_the_material_bank() {
        let mut e = MatrixRain::new(literal_cfg_on());
        scan_empty(&mut e, 10, 10);
        let mut cells = vec![vec![space_cell(bg3()); 10]; 10];
        for (i, ch) in "0f3a".chars().enumerate() {
            cells[0][i].ch = ch;
        }
        e.sample_material(&cells, 10, None, &[]);
        assert!(!e.literal_material_chars_for_test().is_empty());
        e.note_grid_replaced();
        assert!(
            e.literal_material_chars_for_test().is_empty(),
            "old grid's literal alphabet must not rain over the new session"
        );
        assert!(e.needs_material_sample(), "fresh sample re-armed");
    }

    // ---- ROM / baker via the engine -------------------------------------------

    /// The engine bakes progressively (≤8 tiles per engine tick), publishes a
    /// versioned atlas, and rebakes on a cell-metric change.
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
            e.rain_atlas().is_some(),
            "first tick publishes a partial atlas"
        );
        // 8 engine ticks complete the 64 tiles; the version bumps per batch.
        for _ in 0..8 {
            e.note_keystroke();
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        assert!(e.baker.complete());
        assert!(e.atlas_version() > v1);
        let settled = e.atlas_version();
        // Steady state: no further version churn.
        e.note_keystroke();
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert_eq!(e.atlas_version(), settled);
        // A cell-metric change restarts the bake (new version, incomplete).
        step(&mut e, geom(30, 40, 10, 20), &idle(), 83, &mut q, &mut a);
        assert_ne!(e.atlas_version(), settled);
    }

    /// Emitted quads reference real tiles: atlas rects are `cell_w × cell_h`
    /// inside the 8×8 tile grid, and every quad stays in its row band.
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
            assert_eq!((s.aw, s.ah), (8, 16));
            assert_eq!(s.ax % 8, 0, "tile-aligned atlas x");
            assert_eq!(s.ay % 16, 0, "tile-aligned atlas y");
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

    // ---- conformance binds (the real engine projected onto the ty models) -----

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
        for (signal, weight, action) in cases {
            let mut state = model.init_state();
            let mut rain = MatrixRain::new(cfg_on());
            rain.note_signal(signal as u32, weight);
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

    /// Bounded synchronized-output credit. `Buggy=1` is the missing-saturation negative
    /// control (a coalesced burst stores `Cap+1`, violating `CreditBound`). Editor echo
    /// clears the whole synchronized frame, a submitted turn admits the same bounded
    /// credit, and rebase clears all state.
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

    /// Conformance: the shipping classifier runs the synchronized burst + echo + rebase
    /// trace, each decision projected onto the model (its negative control is the
    /// prove-and-catch test above).
    #[test]
    fn rain_activity_credit_real_engine_conforms_to_model() {
        let m = rain_activity_credit_model();
        let mut st = m.init_state();
        let g = geom(30, 40, 8, 16);
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, 30, 40);
        let (mut q, mut a) = (Vec::new(), Vec::new());

        e.note_activity(100); // seed the real content clock
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

        // A submitted turn cancels the editor echo discount even when its first
        // real output shares the immediate post-Enter present.
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

        // Once the correlation window closes, content-only work also remains
        // fully credited and agrees with ApplyClean.
        for _ in 0..13 {
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
        }
        e.note_activity(10_000 + 2 * u64::from(CONTENT_CREDIT_CAP));
        assert!(m.fire("ObserveBurst", &mut st));
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert!(m.fire("ApplyClean", &mut st));
        assert_eq!(i64::from(e.content_streak), st["real"]);

        e.note_activity(2); // a new session/grid starts at a lower sequence
        assert!(m.fire("Rebase", &mut st));
        assert_eq!(i64::from(e.pending_content_credit), st["credit"]);
        assert_eq!(i64::from(e.content_streak), st["real"]);
        assert!(m.check_invariant("CreditBound", &st));
        assert!(m.check_invariant("RealBound", &st));
    }

    /// Conformance for the `RainLifecycle` model
    /// (`aterm_spec::derive::rain_lifecycle_model`): the real engine runs enable → rain →
    /// unfocus-drain → licensed refocus, each transition projected onto the model
    /// (enable/resume → `Activity`, unfocus → `StartDrain`, drain tick → `DrainTick`),
    /// reaching Idle at exactly `DRAIN_TICKS` with the model's invariants holding throughout.
    /// The phantom-relight negative control is below.
    #[test]
    fn rain_lifecycle_conformance_real_engine_projects_onto_model() {
        let m = aterm_spec::derive::rain_lifecycle_model();
        let mut st = m.init_state();
        let g = geom(24, 60, 8, 16);
        // Enabled + Focused (the default visibility) over an all-empty grid.
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

        // ── enable → Raining (an Activity event) ──
        // A focused, awake pane emits (the Calm baseline) with zero drain fuel.
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

        // ── unfocus → StartDrain (Draining opens; no fuel spent yet) ──
        e.set_visibility(RainVisibility::VisibleUnfocused);
        assert_eq!(e.drain_ticks, 0, "unfocus does not itself spend drain fuel");
        assert!(m.fire("StartDrain", &mut st));
        assert_eq!(st["state"], 2, "unfocus enters Draining");
        assert_eq!(st["drained"], 0);
        check_inv(&st);

        // ── drain to empty: every engine tick is one DrainTick ──
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

        // ── licensed refocus while the weather is still awake → Activity ──
        // A Calm pane (not yet asleep) resumes on refocus — a licensed event
        // that pays for the Raining re-entry (contrast the phantom control).
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
        // NoUnlicensedRain stays tight: two Activities, two Raining entries.
        assert_eq!(st["rains"], st["acts"]);
    }

    /// Negative control for the `RainLifecycle` binding: a drained, sleeping
    /// pane refocused on cmd-tab must not relight (the real engine keeps its
    /// drain fuel spent). The phantom-relight trace (Idle → Raining with no
    /// paying Activity) is admitted only by the `Buggy = 1` model, and the
    /// healthy model's `NoUnlicensedRain` rejects the projected state.
    #[test]
    fn rain_lifecycle_negative_control_phantom_relight_is_buggy_trace() {
        // 1) The real engine keeps a slept, drained pane drained across an
        //    unfocus/refocus (cmd-tab).
        let g = geom(24, 60, 8, 16);
        let mut e = MatrixRain::new(RainConfig {
            enabled: true,
            seed: 7,
            idle_secs: 2, // reach SLEEP quickly for the test
            ..RainConfig::default()
        });
        scan_empty(&mut e, 24, 60);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        e.note_activity(1);
        e.note_activity(2);
        // Idle-hold with no further activity: the weather sleeps, then the
        // focused pane drains to empty (eff == Sleep drains even while focused).
        let mut guard = 0;
        while !(e.weather == RainWeather::Sleep && e.drain_ticks >= DRAIN_TICKS) {
            step(&mut e, g, &idle(), u64::from(CALM_TICK_MS), &mut q, &mut a);
            guard += 1;
            assert!(guard < 500, "the pane must sleep and drain");
        }
        assert!(!e.is_active(), "a slept, drained pane is inactive");
        // cmd-tab: hide then refocus. Refocus of a sleeping pane must not relight.
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

        // 2) The models separate healthy from buggy: only Buggy=1 admits the
        //    phantom relight, and the healthy model rejects the projected state.
        let healthy = aterm_spec::derive::rain_lifecycle_model();
        let mut buggy = aterm_spec::derive::rain_lifecycle_model();
        for c in &mut buggy.consts {
            if c.0 == "Buggy" {
                c.1 = 1;
            }
        }
        // Drive the buggy model through one full episode to Idle…
        let mut st = buggy.init_state();
        assert!(buggy.fire("Activity", &mut st));
        assert!(buggy.fire("StartDrain", &mut st));
        for _ in 0..DRAIN_TICKS {
            assert!(buggy.fire("DrainTick", &mut st));
        }
        assert_eq!(st["state"], 0, "the buggy episode reaches Idle");
        // …then the phantom relight: Idle → Raining with no Activity event.
        assert!(
            buggy.action_enabled("Rearm", &st),
            "Buggy=1 enables the phantom relight"
        );
        assert!(buggy.fire("Rearm", &mut st));
        assert_eq!(st["state"], 1, "the buggy pane relit on cmd-tab alone");
        // The healthy model rejects the projected state, and has no such action.
        assert!(
            !healthy.check_invariant("NoUnlicensedRain", &st),
            "a Raining entry with no paying Activity violates NoUnlicensedRain"
        );
        assert!(
            !healthy.action_enabled("Rearm", &st),
            "the healthy model has no phantom-relight action"
        );
    }

    /// Conformance for the `RainIgnition` model
    /// (`aterm_spec::derive::rain_ignition_model`): the real `field::col_params`
    /// is driven over the model's small-grid lattice (rows 3..=8 at the neutral
    /// speed knob, so `p ∈ 2..=5`, tick_ms = 33), asserting the per-cell
    /// head-pass floor `c·p·tick_ms >= 1000` for every column.
    #[test]
    fn rain_ignition_conformance_real_col_params_hold_the_flash_floor() {
        use super::field::{FieldParams, col_params};
        let tick_ms = 33u32;
        let (mq, dq, sq) = FieldParams::quanta(tick_ms, 133);
        let mut checked = 0u32;
        for rows in 3u32..=8 {
            for seed in [0u32, 7, 0xDEAD_BEEF, 0x1234_5678, 0xA7E2_11D3] {
                let fp = FieldParams {
                    seed32: seed,
                    rows,
                    tick_ms,
                    speed: 5, // neutral: p ∈ 2..=5, the model's period range
                    trail: 5,
                    mq,
                    dq,
                    sq,
                };
                for col in 0..80u32 {
                    let cp = col_params(&fp, col);
                    assert!(
                        (2..=5).contains(&cp.p),
                        "neutral speed keeps p in 2..=5 (got {})",
                        cp.p
                    );
                    assert!(
                        cp.c * cp.p * tick_ms >= 1000,
                        "flash floor: rows={rows} seed={seed:#x} col={col} c={} p={} -> {} ms < 1000",
                        cp.c,
                        cp.p,
                        cp.c * cp.p * tick_ms
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(
            checked,
            6 * 5 * 80,
            "the full small-grid lattice was exercised"
        );
    }
}
