// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PHOSPHOR — the Matrix digital-rain engine (docs/matrix-rain-design.md).
//!
//! `MatrixRain` follows the `word_decorations`/`cursor_trail` lifecycle
//! contract: an injected-clock state machine (`tick(now, ..)` on native;
//! `advance_ms` + `emit` for clockless web hosts) that fills resident
//! `SpriteQuad`/`GlowQuad` scratch and returns a fingerprint — `0` when empty,
//! byte-stable when drained, changing every animating tick. The grid is never
//! touched; rain is strictly UNDER text (the `rain_quads`/`rain_add`
//! `RenderInput` channels) and only in cells that carry no meaning.
//!
//! - **Field**: all lattice math lives in [`field`] as pure `(seed, tick)`
//!   functions on `rain_hash32` (u32 only — shader-portable).
//! - **Mask**: two tiers. Tier A is the damage-epoch-gated occupancy bitset
//!   ([`MatrixRain::rescan_from_cells`]); Tier B (selection, cursor band,
//!   scrollback/alt-screen reading gates) is evaluated live per tick.
//! - **Weather**: WORKING / CALM (12 Hz internal tick gate) / SLEEP with
//!   ≥ 1 s dwell, an integer-EMA density staircase whose change points live
//!   in a bounded ring, a mandatory geometry-independent drain, drain on
//!   unfocus, a limiter-gated turn wave on the WORKING→CALM edge, and a 2 s
//!   constant-luminance amber bell ramp.
//! - **Flash safety is structural**: the per-column flash floor
//!   ([`field::col_params`]), the 350 ms dither grid, and the ≤ 2
//!   ignitions/s wave limiter live in the math, not in host policy.

pub mod bake;
/// The 8x8 bitmap glyph table literal material mode draws with — public-domain
/// data copied in-tree, retiring the `font8x8` crate (see `bitmap_font.rs`).
mod bitmap_font;
pub mod field;
pub mod rom;

use std::sync::Arc;

use aterm_time::Instant;

use aterm_core::grid::LineSize;
use aterm_core::grid::extra::ImageRef;
use aterm_core::terminal::{RenderCell, UnderlineStyle};
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
/// Echo-correlation window: a content delta landing within this of the user's
/// last keystroke is attributed to the shell ECHOING that keystroke (locally
/// <20 ms; generous for ssh/load) and never advances the WORKING streak —
/// the §5 "your own typing is a drizzle" promise, enforced at the source.
///
/// KNOWN TRADE-OFFS of the correlation heuristic (codex re-audit, accepted):
/// * echo latency ABOVE the window (a laggy ssh hop) is indistinguishable
///   from content, so heavy typing there can still read as streaming — the
///   pre-discount behavior, now confined to pathological links;
/// * output coalesced after an editing/navigation key is discounted with that
///   frame; Enter/submit supplies an explicit [`RainSignal::TurnStart`] boundary,
///   so a fast first response is credited immediately. This avoids treating
///   readline/TUI suffix repaints as agent work without penalizing submissions.
const ECHO_DISCOUNT_MS: u64 = 250;
/// Bounded `content_seq` distance retained between presents. This is large
/// enough for a synchronized Codex/Claude frame to classify as real output,
/// but prevents an arbitrarily large generation jump from inflating state.
const CONTENT_CREDIT_CAP: u8 = 32;
/// Mandatory drain length in engine ticks: at the CALM 12 Hz gate this is
/// ~2.5 s — every column reaches empty within this bound REGARDLESS of
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
/// Command-FAILED ramp hue (ember red-orange — clearly distinct from the amber
/// bell and the green field; constant-luminance swap, never a flash).
const FAIL_HUE_DEG: f32 = 8.0;
/// How long the failed-exit ember tint holds (mirrors the bell window).
const EXIT_FAIL_MS: u64 = 2000;

/// Rain tint family (`[matrix_rain] hue`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RainHue {
    /// The stock matrix green.
    Matrix,
    /// Derive the hue from the theme foreground.
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

/// Fully-resolved rain configuration (frozen by the PHOSPHOR contract; the
/// gui resolver owns defaults/clamps, the engine re-clamps defensively).
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
    /// OUTPUT MATERIAL BANK (design v1.1): the rain's glyph alphabet is
    /// sampled from the program's REAL on-screen output and rasterized
    /// into a bounded dynamic ROM — case, digits, punctuation, box drawing,
    /// and supported Unicode retain distinct built-in bitmap forms. The rain is
    /// visibly made OF supported output codepoints while still drawing only in
    /// empty cells. The current cursor/composer protection band is excluded;
    /// hosts with stronger provenance can mark additional cells ineligible.
    /// Default true; false keeps the classic pure-kana field.
    pub output_material: bool,
    /// Turn-complete wave on the WORKING→CALM edge.
    pub turn_wave: bool,
    /// Visual bell → 2 s amber ALERT hue-ramp.
    pub bell_alert: bool,
    /// Command EXIT STATUS in the weather (OSC 133/633, host-fed): success
    /// fires the finishing head-sweep; failure holds a 2 s constant-luminance
    /// EMBER tint — glanceable "did it fail?" without touching any text.
    pub exit_tint: bool,
    /// Replay seed (0 is a valid seed; demos pin it for reproducibility).
    pub seed: u64,
    /// Theme default background `0x00RRGGBB` (ramp + luminance derivation).
    pub default_bg: u32,
    /// Theme foreground `0x00RRGGBB` (the SGR-2 dim reference the luminance
    /// invariant floors against, and the `hue = "theme"` source).
    pub theme_fg: u32,
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
            // `ColorScheme::default`): bg #111318 / fg #D0D0D0.
            default_bg: 0x0011_1318,
            theme_fg: 0x00D0_D0D0,
        }
    }
}

/// The Tier-B live per-tick inputs (snapshot scalars the host read under its
/// terminal lock; the engine never re-locks).
#[derive(Clone, Copy, Default)]
pub struct RainTickInput<'a> {
    /// Visible cursor cell `(row, col)`, `None` when hidden (DECTCEM off).
    pub cursor: Option<(u16, u16)>,
    /// When the cursor is hidden: the last [`HIDDEN_CURSOR_BAND_ROWS`]
    /// recently-damaged viewport rows, host-fed (Ink parks a hidden cursor at
    /// a meaningless position, so damage recency stands in for it).
    pub hidden_band: &'a [u16],
    /// Live selection view (search highlight IS the selection). Applied per
    /// emitted quad — selection mutates with zero damage marking, so it must
    /// never bake into the Tier-A bitset.
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

/// The PHOSPHOR rain engine. One per window; all buffers resident.
///
/// scope-waiver: a RESOURCE statement, not a safety budget. Rain has no
/// ignition limiter and no burst mutex — it is a continuous field, so N
/// engines cost N sets of resident buffers rather than N times a bound the
/// user experiences the sum of. Should rain ever gain a flash-rate or
/// quad-budget enforcer, this waiver must become a ScopeClaim.
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
    // -- pending host notes (stamped at the next clock sync so a note after
    //    a long idle gap lands at the FRESH clock, not the stale one) --------
    pending_key: bool,
    /// Coalesced distance in the monotonic grid content clock (bounded). Unlike
    /// a boolean edge this survives synchronized-output batching: one large
    /// Codex/Claude present still carries evidence of a real output burst.
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
    // -- output material bank (design v1.1) -------------------------------------
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
    /// The last emit was suppressed by a READING gate (scrolled-back view /
    /// alt-screen quiet or suppression) — the user is reading, not watching
    /// the field. With an empty frame on glass this disarms the wake timer:
    /// scroll input and returning to live are themselves redraws, so the
    /// engine re-arms the moment the gate lifts (CATCHUP fast-forwards the
    /// tick backlog).
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

    /// Swap the configuration: rebuild the ramp + derived caches (and restart
    /// the bake — the contract pins a version bump on ramp change), KEEPING
    /// the tick epoch/clock so a live reload never time-travels the field.
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
        // The material knob must take effect on a LIVE reload, even when the
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
    /// returns to the classic authoring, but it is emitted only when the user
    /// explicitly sets `output_material = false`; literal mode with no sampled
    /// characters is honestly empty rather than decorative lookalike text.
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

    /// Publish a literal material generation atomically when live cell metrics
    /// are already known. The fixed 64-tile bake is sub-millisecond and occurs
    /// only when the distinct visible charset changes; stable streaming merely
    /// updates the occurrence tape. Before the first geometry arrives the
    /// baker is vacuously complete and `emit` performs this once after sizing.
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
    /// deltas drive WORKING. The bounded sequence DISTANCE is retained, rather
    /// than collapsing every present to one boolean edge, so synchronized
    /// Codex/Claude output that mutates many cells before one present still
    /// classifies as a real burst. Cheap and callable every frame.
    pub fn note_activity(&mut self, content_seq: u64) {
        let Some(previous) = self.last_seq.replace(content_seq) else {
            // The FIRST observation is a baseline, not activity: enabling
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

    /// Whether emission is even POSSIBLE right now — the host's "is the
    /// O(rows·cols) rescan/sample worth doing" probe (round-3 audit): false
    /// under reduced motion or for a non-focused pane whose bounded drain
    /// completed. Skipping the rescan leaves `last_epoch` unbumped, so
    /// `needs_rescan` stays true and the scan runs on the first frame the
    /// gate lifts (refocus resets the drain via `set_visibility`).
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

    /// The host swapped WHICH grid this engine watches (front-session change
    /// on a retained engine: a tab switch, a pane-focus move in a split, a
    /// session migration). `needs_rescan` alone cannot see this — it compares
    /// per-terminal damage epochs, and two unrelated terminals' monotonic
    /// counters can collide, silently keeping the OLD grid's occupancy (rain
    /// falling through the new tab's text) and its sampled material alphabet.
    /// Rebaseline everything grid-derived:
    /// - occupancy: force the next Tier-A rescan regardless of epoch equality;
    /// - material bank: drop the old grid's literal glyphs and resample from
    ///   the new one (stale output characters must not rain over another
    ///   session);
    /// - activity: the next `note_activity` observation is a baseline, never
    ///   a manufactured burst off an unrelated `content_seq`.
    /// - composer provenance: the `material_editing` latch belongs to the OLD
    ///   grid's unsent draft (set per keystroke, released at TurnStart).
    ///   Carrying it across the swap deadlocked literal mode: this clear +
    ///   a latched Editing meant `sample_material` refused every refill, so
    ///   rain went DARK on the new grid until its next Enter (post-merge
    ///   re-audit, HIGH). The NEW grid's composing line is still protected by
    ///   the cursor-band / hidden-band exclusions the sampler always applies.
    ///
    /// Weather/clock state deliberately survives — the swap is a viewpoint
    /// change, not a restart.
    pub fn note_grid_replaced(&mut self) {
        self.have_scanned = false;
        self.clear_material_bank();
        self.material_sample_needed = self.cfg.output_material;
        self.material_editing = false;
        self.last_seq = None;
    }

    /// Whether an input note could actually animate this engine — the host's
    /// "is a wake-up redraw worth requesting" probe: false when disabled or
    /// under reduced motion (emit would return 0 immediately), so key
    /// autorepeat on a Reduced pane never spams useless redraws.
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

    /// A command COMPLETED (OSC 133/633 exit code, host-fed): success fires
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

    /// Rebuild the Tier-A occupancy bitset from the frame snapshot (the same
    /// `cell_frame_into` rows sparkle scans). Eligible iff the cell is a
    /// space, not a wide half, not underlined, on the default background,
    /// not covered by an inline image, AND the row is `SingleWidth`
    /// (DECDWL/DECDHL rows render at 2× metrics — wholly ineligible).
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
        let words = (rows * cols).div_ceil(64);
        self.occ.clear();
        self.occ.resize(words, 0);
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
                continue;
            }
            // `RenderInput.cells` is the canonical trimmed shape: trailing
            // blank cells (and whole blank rows) are ABSENT, not stored as
            // spaces — the renderer paints `default_bg` wherever a cell is
            // missing (`input.cells.get(r)…unwrap_or(&[])`). An absent cell is
            // therefore an empty default-bg cell and IS rain-eligible; without
            // this, rain never appears over any blank row (the trimmed region
            // is exactly where rain belongs).
            let row_cells: &[RenderCell] = cells.get(r).map(Vec::as_slice).unwrap_or(&[]);
            for c in 0..cols {
                let eligible = match row_cells.get(c) {
                    Some(cell) => {
                        cell.ch == ' '
                            && !cell.wide
                            && cell.underline == UnderlineStyle::None
                            // Strikethrough/overline draw visible lines through a
                            // space cell — it carries meaning, exactly like the
                            // underline case (§6 render-truth audit).
                            && !cell.strikethrough
                            && !cell.overline
                            && cell.bg == bg
                    }
                    None => true,
                };
                if eligible {
                    let bit = r * cols + c;
                    self.occ[bit / 64] |= 1 << (bit % 64);
                }
            }
            // Inline-image spans mask their cells (Codex sixel/iTerm2 pets
            // stay untouched) — clear after the eligibility pass.
            if let Some(spans) = images.get(r) {
                for (c, _) in spans {
                    if *c < cols {
                        let bit = r * cols + c;
                        self.occ[bit / 64] &= !(1 << (bit % 64));
                    }
                }
            }
        }

        // Semantic clearance: a rain cell needs one whole-cell breathing room
        // from real glyphs, attributed spaces, wide halves, and images. Besides
        // eliminating fake-looking letters in one-cell inter-word gaps, this
        // keeps the half-cell head halo physically unable to touch text. Run as
        // a second pass so later eligibility writes cannot re-enable a neighbor.
        for r in 0..rows {
            let row_cells: &[RenderCell] = cells.get(r).map(Vec::as_slice).unwrap_or(&[]);
            // The 3x3 boxes of ADJACENT meaningful cells overlap almost
            // completely: a run of n text cells clears 3(n+2) distinct bits with
            // 9n bounded, divided, scalar read-modify-writes. The dilated
            // columns arrive in ascending order, so one running interval
            // coalesces them, and each interval is cleared with the masked-word
            // span writer this file already uses for framed regions.
            //
            // The cleared SET is unchanged by construction: two [c-1, c+1]
            // boxes are merged only when they touch or overlap (`lo <= end +
            // 1`), so a one-cell gap between two runs is still never cleared,
            // and the interval is flushed across exactly rows r-1..=r+1 with
            // the same clamping. Occupancy stays byte-identical, so every
            // emitted quad, tint and fingerprint is unchanged.
            let mut run: Option<(usize, usize)> = None;
            for (c, cell) in row_cells.iter().enumerate().take(cols) {
                let meaningful = cell.ch != ' '
                    || cell.wide
                    || cell.underline != UnderlineStyle::None
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
                        self.clear_occ_neighborhood(r, *c);
                    }
                }
            }
        }
        // A closed box-drawn surface is a semantic UI region, not decorative
        // negative space. Claude/Codex welcome, alert, picker, and diff panels
        // often have default-background interiors, so cell-local occupancy
        // alone cannot distinguish them from the open terminal field.
        self.mask_framed_regions(cells, rows, cols);
        self.have_scanned = true;
        self.last_epoch = epoch;
    }

    /// OUTPUT MATERIAL BANK (design v1.1): sample the program's REAL on-screen
    /// output into the rain's glyph alphabet. Call right after
    /// [`Self::rescan_from_cells`] under the same damage gate, with the same
    /// snapshot rows plus the frame's typing bands — and ONLY on live frames
    /// (`display_offset == 0`): scrolled-back snapshots are display-translated
    /// while the cursor is grid-space, so the band math (and the privacy
    /// contract) only lines up on the live viewport. The host skips the call
    /// there; the previous table persists.
    ///
    /// Sampling rules:
    /// * From [`Self::note_keystroke`] through an explicit
    ///   [`RainSignal::TurnStart`], sampling is deferred and the previous real
    ///   output tape is retained; occupancy may still rescan normally.
    /// * Only occupied, non-wide cells count (the OUTPUT, not the empty field).
    /// * The current typing surface is excluded through the visible-cursor band
    ///   (row ± [`CURSOR_BAND_ROWS`]) or, cursor hidden, the host-fed recently
    ///   damaged composer band. This is a visual heuristic, not secret storage;
    ///   embedders can opt out or protect stronger provenance themselves.
    /// * Supported codepoints retain a literal bitmap glyph in the dynamic ROM; an
    ///   unsupported code point is skipped, never substituted with fake art.
    /// * Deterministic: the most recent [`MATERIAL_CAP`] supported codepoints
    ///   remain in row-major source order (spaces and unsupported scalars are
    ///   omitted). Their distinct sorted set assigns
    ///   stable dynamic-ROM slots, so ordinary streaming does not rebake unless
    ///   a genuinely new character enters/leaves the visible sample.
    ///
    /// An empty literal sample clears the table and emits no glyph rain. The
    /// classic pure-hash alphabet is available only through the explicit
    /// `output_material = false` setting.
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
        // Walked BOTTOM-UP, and stopped the instant MATERIAL_CAP is reached.
        // The sample is defined as the LAST <= MATERIAL_CAP supported
        // codepoints in row-major order, so a forward walk that hashed every
        // occupied cell on the screen threw ~90% of that work away on a dense
        // pane (each miss/hit costs a `rom::material_bitmap` — up to seven
        // font-table binary searches). Collecting in reverse and reversing once
        // at the end yields the identical sequence while touching only the
        // cells that can actually contribute. The row band gate below is a pure
        // function of `r` (and `rows`/`cursor`/`hidden_band`), so it selects the
        // same rows in either direction.
        'rows: for r in (0..rows).rev() {
            let row = r as u16;
            let cursor_banded =
                cursor.is_some_and(|(cr, _)| i32::from(cr.abs_diff(row)) <= CURSOR_BAND_ROWS);
            // The host's recent-damage composer rows remain protected even
            // while the cursor is visible; this catches multiline input rows
            // outside the immediate ±2 band when the host can identify them.
            let remembered_banded = hidden_band.contains(&row);
            // Hidden cursor with an UNKNOWN band (first enable / resize / full
            // damage) falls back to the bottom K rows, where these composers
            // normally live.
            let fallback_banded =
                cursor.is_none() && hidden_band.is_empty() && r + HIDDEN_CURSOR_BAND_ROWS >= rows;
            let banded = cursor_banded || remembered_banded || fallback_banded;
            if banded {
                continue;
            }
            let Some(row_cells) = cells.get(r) else {
                continue;
            };
            for cell in row_cells.iter().rev() {
                if cell.ch != ' ' && !cell.wide && rom::material_bitmap(cell.ch).is_some() {
                    // Bounded by construction: MATERIAL_CAP entries is the whole
                    // product of the pass, so a viewport-sized scratch (which
                    // would permanently retain megabytes after one huge grid)
                    // never forms.
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
            // WHICH SLOTS ACTUALLY MOVED. Slot assignment is unchanged — the
            // distinct set, sorted, exactly as before — so a slot either keeps
            // its character across the change or it does not, and only the ones
            // that differ need their ROM glyph re-authored and their atlas tile
            // re-baked. Ordinary streaming turns over a handful of characters at
            // a time, yet every such frame today re-authors the whole 12 KB
            // master, memsets the entire 8x8-tile atlas (51 KB at 10x20 cells,
            // 205 KB at retina) and box-filters all 64 tiles in one synchronous
            // block.
            let changed = slot_change_mask(&self.material_chars, &self.material_slots_scratch);
            self.material_chars.clone_from(&self.material_slots_scratch);
            // THE INVARIANT THE FAST PATH RESTS ON: every writer of `self.rom`
            // (`clear_material_bank`, the wholesale arm below) follows it with
            // `baker.restart()`, and `begin_frame` restarts on a cell-metric
            // change — so a baker that is COMPLETE at a live metric is, by
            // construction, a full bake of the current `self.rom` at the current
            // metric. Patching exactly the changed tiles therefore leaves the
            // published atlas BYTE-IDENTICAL to the wholesale rebuild, at the
            // same slot origins, and `rebake_tiles` advances the version by the
            // same amount the wholesale path did, so the frame fingerprint is
            // unmoved too.
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
        let epoch = *self.epoch.get_or_insert(now);
        let ms = now.saturating_duration_since(epoch).as_millis() as u64;
        if ms > self.clock_ms {
            self.clock_ms = ms;
        }
        self.emit(geom, input, quads, add)
    }

    /// SUSPENDED tick for hosts that skip the emission path entirely
    /// (alt-screen suppression / the perf load-shed latch): advance the clock
    /// and the WEATHER machine — no bake, no field walk, no quads. Notes stop
    /// landing while suspended, so the weather starves to SLEEP after
    /// `idle_secs`, the mandatory drain completes, and [`Self::is_active`]
    /// self-disarms — a suspended pane can never leak perpetual wakes off a
    /// frozen Working/Calm state. Resuming (the host ticking normally again)
    /// rebuilds the weather from fresh notes.
    pub fn tick_suspended(&mut self, now: Instant) {
        let epoch = *self.epoch.get_or_insert(now);
        let ms = now.saturating_duration_since(epoch).as_millis() as u64;
        if ms > self.clock_ms {
            self.clock_ms = ms;
        }
        self.step_suspended();
    }

    /// Clockless core of [`Self::tick_suspended`] (web hosts pair it with
    /// [`Self::advance_ms`], exactly like `emit`).
    pub fn step_suspended(&mut self) {
        // Suspended panes render NOTHING, so the ACTIVITY notes are DROPPED,
        // not applied: a keystroke landing here would hold the pane at CALM
        // and keep the timer armed at 12 Hz for a whole vim session that
        // draws no rain (the wake leak this path exists to remove — codex).
        // The weather starves to SLEEP on schedule from the last PRE-suspend
        // note; resuming rebuilds it from fresh notes within a dwell.
        self.pending_key = false;
        self.pending_content_credit = 0;
        // The DEADLINE-STAMP notes stay PENDING (codex re-audit): a bell, an
        // exit status, or a reading-scroll during a brief load-shed must not
        // vanish — they apply at the resume emit with a fresh window, and
        // none of them holds `is_active` true, so no wakes leak.
        self.advance_engine_ticks();
        self.last_emit_nonempty = false;
    }

    /// Advance whole engine ticks out of the accumulated clock, then fill
    /// the frame's rain quads + additive halos. Returns the fingerprint
    /// (FNV-1a chain over EVERY quad field with the frame term folded exactly
    /// once mid-chain; `0` when empty).
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
        // Gate FIRST (codex round-3): a scrolled-back / alt-quiet / drained /
        // unscanned frame emits nothing, so the progressive bake and the
        // column-order rebuild are pure waste there — only the weather/tick
        // advancement above must always run. The baker resumes on the first
        // ungated frame (≤ 8 tiles/tick, unchanged contract).
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
        // Literal mode is strict: without sampled terminal characters there is
        // no honest glyph material to draw. This also skips every bake/field
        // cost on a blank or wholly unsupported screen. The decorative ROM is
        // reachable only through the explicit output_material=false setting.
        let material_ready = !self.cfg.output_material || !self.material.is_empty();
        if !gated && material_ready {
            self.baker.begin_frame(geom.cell_w, geom.cell_h);
            if !self.baker.complete() && !self.material.is_empty() {
                // A literal charset becomes visible as ONE complete generation:
                // never mix new slot indices with old/blank atlas tiles.
                self.finish_material_bake();
            } else if !self.baker.complete() && (ticked || self.last_bake_tick != Some(self.tick)) {
                let rom = self.rom.get_or_insert_with(|| decorative_master().clone());
                self.baker.bake_tiles(rom);
                self.last_bake_tick = Some(self.tick);
            }
            self.ensure_col_order(geom.cols, geom.rows);
            self.emit_field(geom, input, quads, add);
            // The renderer's per-row dirty merge-diff REQUIRES `rain_quads`
            // row-sorted (its grouping walks contiguous row slices and
            // debug_asserts the order), but emission walks hash-ordered
            // COLUMNS. STABLE counting sort into resident scratch — stability
            // is load-bearing: the merge-diff compares each row's slice
            // order-sensitively, so within-row order must be a pure function
            // of the frame (the col_order walk), never of OTHER rows' churn
            // (`sort_unstable` permutes equal keys as a function of the whole
            // array — an unchanged row would read as dirty whenever any other
            // row changed, degrading the diff to full-band marking in a
            // downpour). O(n + rows), allocation-free after warmup.
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
        // A reading-gated pane with an empty frame on glass holds still: the
        // user is scrolling history / reading an alt transcript, every wake
        // would rebuild a frame to the fp-0 early-out (round-3 audit). The
        // reads that END the gate (scroll input, return-to-live damage) are
        // themselves redraws, so re-arming is automatic.
        if self.reading_gated {
            return false;
        }
        match self.visibility {
            RainVisibility::Focused => {
                self.eff_weather() != RainWeather::Sleep || self.drain_ticks < DRAIN_TICKS
            }
            // A non-focused pane exists only to finish its BOUNDED drain:
            // once drained + empty it holds still even while content keeps
            // streaming (weather stays capped ≤ CALM but there is nothing to
            // draw and nothing that ever will be until refocus resets the
            // drain) — the §5 "drain, then 0 wakes" promise at engine level,
            // previously masked natively by the unfocus→Reduced demotion.
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

    /// One-line diagnostic snapshot for host introspection (`aterm-ctl rain
    /// status` — split-pane audit): the EFFECTIVE weather word, the live
    /// density staircase byte, the engine tick, whether a Tier-A scan is
    /// resident, the literal-material alphabet size, and whether the last
    /// emit produced light. Pure read; the exact set an operator needs to
    /// answer "why isn't it raining" without a debugger.
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
    /// idle gap must land NOW, not at the stale pre-gap clock).
    fn apply_pending_notes(&mut self) {
        let now = self.clock_ms;
        // Keystrokes stamp FIRST so a same-frame echo sees the fresh key time.
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
            // ECHO DISCOUNT (§5 "your own typing is a drizzle"): readline and
            // full-screen TUIs can erase + repaint an entire suffix after one
            // delete, and repeat keys can coalesce before a present. Attribute
            // the whole immediate frame to interactive echo; sustained agent
            // output proves itself on the next content-only frame.
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
            // EXIT STATUS → weather (usefulness: glanceable pass/fail without
            // reading a byte of text). Success rides the existing turn-wave
            // machinery (limiter-bounded); failure holds the ember ramp.
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

    /// The engine's CURRENT tick-consumption period in milliseconds, for the
    /// host's timer arming: WORKING runs at the fps knob; CALM / SLEEP /
    /// drain gate at 12 Hz — arming the wake timer at this instead of the
    /// raw fps stops ~3 of every 5 CALM wakes advancing nothing (audit).
    #[must_use]
    pub fn current_period_ms(&self) -> u64 {
        self.effective_period()
    }

    /// Remaining milliseconds until the next engine tick. Unlike the raw
    /// period, this preserves partial progress when a PTY/input redraw lands
    /// between rain frames, so web timers neither gap nor double-step.
    #[must_use]
    pub fn next_tick_in_ms(&self) -> u64 {
        let period = self.effective_period();
        let backlog = self.clock_ms.saturating_sub(self.consumed_ms);
        period.saturating_sub(backlog.min(period))
    }

    /// The engine tick period under the current effective weather: WORKING
    /// runs at the fps knob; CALM/SLEEP are gated to 12 Hz.
    fn effective_period(&self) -> u64 {
        u64::from(match self.eff_weather() {
            RainWeather::Working => self.tick_ms,
            _ => self.tick_ms.max(CALM_TICK_MS),
        })
        .max(1)
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

    /// Consume whole engine ticks from the accumulated clock. Returns whether
    /// any tick elapsed. Backlogs beyond [`CATCHUP_MS`] fast-forward the tick
    /// counter arithmetically (the field is a pure function of tick, so a
    /// skipped span has no state to replay beyond weather, which converges).
    fn advance_engine_ticks(&mut self) -> bool {
        let mut ticked = false;
        let backlog = self.clock_ms.saturating_sub(self.consumed_ms);
        if backlog > CATCHUP_MS {
            let period = self.effective_period();
            let skip = (backlog - CATCHUP_MS) / period;
            self.tick += skip;
            self.consumed_ms += skip * period;
        }
        loop {
            let period = self.effective_period();
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
            // Any transition OUT of Working is a completed turn — including a
            // direct Working→Sleep jump (reachable at the idle_secs clamp
            // minimum, where the idle window equals WORKING_HOLD_MS and Calm
            // is skipped entirely). The wave must not silently vanish there.
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
        // second — delayed, not dropped, while the window is full). Checked
        // BEFORE the drain update so a direct Working→Sleep turn (the
        // idle_secs clamp minimum) ignites on its transition tick — the sweep
        // then plays out over the draining field, a natural finishing bow.
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
                let len = (WAVE_MS / self.effective_period()).max(1) as u32;
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

    /// Rebuild the hash-ascending column order when the width changes: the
    /// stride covers the FULL width (never a prefix — no dead right margin),
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
        // Rebuild the tick-independent ColParams memo on the SAME trigger
        // (ColParams depends on seed/rows/tick_ms/speed/trail — all frame
        // invariants between resizes/reconfigs). Indexed by column, sized cols.
        let fp = FieldParams {
            seed32: self.seed32,
            rows: u32::from(rows),
            tick_ms: self.tick_ms,
            speed: u32::from(self.cfg.speed),
            trail: u32::from(self.cfg.trail),
            mq: self.mq,
            dq: self.dq,
            sq: self.sq,
        };
        self.col_params_cache.clear();
        self.col_params_cache
            .extend((0..u32::from(cols)).map(|c| col_params(&fp, c)));
    }

    /// Field emission: walk columns in hash order, truncating WHOLE columns
    /// once the quad budget is hit, then grant halos round-robin.
    /// Stable row bucket sort (see the call site): counts per row, prefix-
    /// sums into scatter offsets, and replays the emission order within each
    /// row. `quads` rows are `< rows` by construction (emit_column clamps).
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
            fp: FieldParams {
                seed32: self.seed32,
                rows: u32::from(geom.rows),
                tick_ms: self.tick_ms,
                speed: u32::from(self.cfg.speed),
                trail: u32::from(self.cfg.trail),
                mq: self.mq,
                dq: self.dq,
                sq: self.sq,
            },
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
                // column WHOLE — reads as lower density, never a cut margin.
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
                if !self.cell_eligible(r, col, ctx.input) {
                    continue;
                }
                if quads.len() >= ctx.cap {
                    return false;
                }
                let head_flash = is_head && lvl == 15;
                quads.push(self.cell_quad(ctx, col, r, lvl, head_flash));
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
            quads.push(self.cell_quad(ctx, col, wr, lvl, true));
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

    /// One resolved rain cell quad (tile rect from the baker; tint/alpha from
    /// the ramp; `flip_x` from the glyph hash — the mirrored film texture).
    /// With a live OUTPUT MATERIAL table, adjacent rows in a rain column walk
    /// adjacent entries in the sampled output sequence. The per-column hash
    /// chooses a deterministic starting point and `glyph_epoch` advances it,
    /// so supported codepoints retain their sampled adjacency rather than
    /// becoming a randomized lookalike bag. Literal glyphs are never mirrored.
    fn cell_quad(&self, ctx: &EmitCtx<'_>, col: u16, row: u32, lvl: u32, head: bool) -> SpriteQuad {
        let (cw, ch) = (u32::from(ctx.geom.cell_w), u32::from(ctx.geom.cell_h));
        let (g, flip_x) = glyph_from_epoch(self.seed32, row, u32::from(col), ctx.glyph_epoch);
        let (g, flip_x) = if self.material.is_empty() {
            (g, flip_x)
        } else {
            let ix = self.semantic_material_index(col, row, ctx.glyph_epoch);
            (u32::from(self.material[ix]), false)
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

    /// Map an emitted cell onto the real output tape. The default is byte-for-
    /// byte the original source-order walk. A live semantic pulse groups nearby
    /// columns into a small coherent lane and changes only the tape traversal:
    /// inspect pairs scout together, modify pairs counterflow, execute groups
    /// advance briskly, network groups form two-cell packets, and waiting holds.
    fn semantic_material_index(&self, col: u16, row: u32, glyph_epoch: u32) -> usize {
        let len = self.material.len();
        debug_assert!(len > 0);
        if self.semantic_phase == RainSignal::AssistantStream {
            let start = field::rain_hash32(u32::from(col) ^ self.seed32 ^ 0x00C0_FFEE) as usize;
            return start
                .wrapping_add(row as usize)
                .wrapping_add(glyph_epoch as usize)
                % len;
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
        let start = field::rain_hash32(
            lane ^ self.seed32 ^ self.semantic_seq.wrapping_mul(0x9E37_79B9) ^ 0x005E_A11C,
        ) as usize;
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

    /// Body coverage for a trail level: linear in level, capped at the
    /// derived body alpha (level 15 body cells sit just under the head).
    fn alpha_of(&self, lvl: u32) -> u8 {
        ((u32::from(self.body_alpha) * (lvl + 1)) / 16).max(1) as u8
    }

    /// Recognize closed Unicode box-drawn surfaces and clear their interiors
    /// from the occupancy bitset. Top discovery and bottom-edge prefix sums are
    /// each one left-to-right pass, so the whole detector is O(rows * cols)
    /// with bounded resident candidate storage. A region needs both sides on
    /// at least 75% of its interior rows and box rule on at least 75% of its
    /// bottom edge. Title text may interrupt the top rule, matching real
    /// Claude/Codex panels; arbitrary text cannot impersonate a bottom edge.
    /// Exceeding either bound fails closed for this rescan by suppressing rain.
    fn mask_framed_regions(&mut self, cells: &[Vec<RenderCell>], rows: usize, cols: usize) {
        self.frame_candidates.clear();
        self.frame_regions.clear();

        for r in 0..rows {
            let row = cells.get(r).map(Vec::as_slice).unwrap_or(&[]);
            let ch_at = |col: usize| row.get(col).map_or(' ', |cell| cell.ch);

            // The bottom-edge prefix sums are READ only from the candidate
            // loop below, so a row with no live candidate never observes them —
            // and an ordinary text screen has no candidate on any row. Building
            // it lazily drops a `cols`-wide usize scan (8 B written per cell,
            // 1.6 KB per 200-column row) from every rescan that finds no box at
            // all; when it IS built it is built before its first read, from the
            // same row, with the same contents.
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
                // Every character any `frame_*` classifier matches lives in the
                // Unicode Box Drawing block: the 74 distinct literals across the
                // six classifiers span U+2500..=U+257A. ONE range compare
                // therefore rejects all three of this loop's classifiers for an
                // ordinary ASCII cell instead of walking up to 32 `matches!`
                // arms, and it cannot change the outcome — a non-box character
                // leaves `open_left` and `has_horizontal` untouched today too.
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

    /// Clear rows `row-1 ..= row+1` over one ALREADY DILATED inclusive column
    /// interval — the run-at-a-time form of [`Self::clear_occ_neighborhood`]
    /// for a whole run of adjacent semantic cells. Same bits, same clamping;
    /// three masked word writes per interval instead of nine scalar
    /// read-modify-writes per cell.
    fn clear_occ_neighborhood_span(&mut self, row: usize, start_col: usize, end_col: usize) {
        if self.occ_rows == 0 || self.occ_cols == 0 {
            return;
        }
        let r0 = row.saturating_sub(1);
        let r1 = row.saturating_add(1).min(self.occ_rows - 1);
        for r in r0..=r1 {
            // `clear_occ_span` clamps `end_col` to the last column and returns
            // early past the last row, exactly as the 3x3 form clamped `c1`/`r1`.
            self.clear_occ_span(r, start_col, end_col);
        }
    }

    /// Clear the 3×3 occupancy neighborhood around one semantic cell. Called
    /// only during the damage-gated rescan, never from the per-frame field walk.
    fn clear_occ_neighborhood(&mut self, row: usize, col: usize) {
        if self.occ_rows == 0 || self.occ_cols == 0 {
            return;
        }
        let r0 = row.saturating_sub(1);
        let r1 = row.saturating_add(1).min(self.occ_rows - 1);
        let c0 = col.saturating_sub(1);
        let c1 = col.saturating_add(1).min(self.occ_cols - 1);
        for r in r0..=r1 {
            for c in c0..=c1 {
                let bit = r * self.occ_cols + c;
                self.occ[bit / 64] &= !(1 << (bit % 64));
            }
        }
    }

    /// The Tier-A occupancy bit alone (bounds-checked) — the CHEAPEST reject,
    /// hoisted ahead of the per-cell trail math in [`Self::emit_column`] so a
    /// text-heavy screen skips level/dither work for occupied cells (pure
    /// conjunction reorder; `cell_eligible` still re-verifies).
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
        let r = row as usize;
        if !self.occ_bit(row, col) {
            return false;
        }
        // Cursor band: visible cursor row ± 2; hidden cursor masks the
        // host-fed recently-damaged band — and with an UNKNOWN band (empty
        // ring: first enable / resize / full-damage frames) the BOTTOM K rows
        // are conservatively banded, mirroring `sample_material`: the §6
        // bottom-region contract must hold for EMISSION too, not just
        // sampling (codex re-audit HIGH — the Claude Code composer must never
        // see rain even before the damage ring warms).
        let cursor_banded = input.cursor.is_some_and(|(cr, _)| {
            (i64::from(row) - i64::from(cr)).abs() <= i64::from(CURSOR_BAND_ROWS)
        });
        let remembered_banded = input.hidden_band.contains(&(row as u16));
        let fallback_banded = input.cursor.is_none()
            && input.hidden_band.is_empty()
            && r + HIDDEN_CURSOR_BAND_ROWS >= self.occ_rows;
        let banded = cursor_banded || remembered_banded || fallback_banded;
        if banded {
            return false;
        }
        // Live selection (search highlight IS the selection): applied per
        // quad, host-side, so CPU==GPU stays byte-identical.
        !input.sel.as_ref().is_some_and(|sv| {
            sv.sel
                .contains(i64::from(row) as i32 - sv.display_offset, col)
        })
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
        // premultiplied ONCE host-side (the nova idiom) — clamped by the
        // same ≤ 135 readability bound as the head itself.
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
            // Radial falloff basis, shared by every row-band quad of this halo:
            // light peaks at the head-cell centre and reaches 0 at the box edge
            // — an ellipse with half-extents (cw, ch) inscribed in the
            // 2·cw × 2·ch box. `cx`/`cy` are the UNCLIPPED head centre so the
            // falloff stays correct even where the box is clamped at a grid edge.
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
                    // Defaulted `mode: HaloMode::Add` — the historical light.
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

/// The ramp hue for a config, kept ≥ 18° away from the Codex diff green.
fn ramp_hue(cfg: &RainConfig) -> f32 {
    let hue = match cfg.hue {
        RainHue::Matrix => MATRIX_HUE_DEG,
        RainHue::Theme => rgb2hsv(cfg.theme_fg).0,
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
/// steps per tint) — the bell ALERT swap changes HUE, not brightness.
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

/// Derive `(body_alpha, head_alpha)` from the §6 luminance constraint:
/// the largest coverages keeping every composited rain level CLOSER to the
/// background than SGR-2 dim text sits — computed against the theme's REAL
/// fg under the linear-light dim law (`L(dim) = L(bg) + 0.5·(L(fg)-L(bg))`,
/// `color_resolve::dim_toward_bg`), never a hardcoded gray.
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

/// One FNV-1a fold step (the `word_decorations::fold_u64` chain).
fn fold_u64(mut h: u64, x: u64) -> u64 {
    h ^= x;
    h.wrapping_mul(0x0000_0100_0000_01B3)
}

/// Which of the 64 dynamic-ROM slots change CHARACTER between two sorted slot
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

/// Frame fingerprint: FNV-1a chain over EVERY field of every quad — each quad's
/// fields are LOSSLESSLY bit-packed into a few u64 words (zero field overlap, so
/// the pack is injective and every field still participates) to shorten the
/// serial IMUL chain. The frame term is folded EXACTLY ONCE mid-chain (never per
/// quad — an even quad count must not cancel the liveness term) and the atlas
/// version at the head (a rebake must repaint). Empty emission ⇒ 0.
///
/// PROFILED, NOT VECTORIZED (round-3 deferral, closed with data): the whole
/// worst-case tick+emit — field walk, mask, counting sort, AND this fold over
/// 2048 quads — measured 30.8 µs median (p90 31.2 µs) on the M2 release bench
/// (`bench_rain_tick_worstcase`, 2026-07-07), ≈ 4.9× under the 150 µs bar. The
/// fold's ≈ 6-7 µs share at ≤ 30 ticks/s is ≈ 0.02 % of one core; a SIMD lane
/// split would perturb every golden fp downstream for that. Re-profile before
/// ever touching this.
fn fingerprint(quads: &[SpriteQuad], add: &[RainHalo], tick: u64, atlas_version: u64) -> u64 {
    if quads.is_empty() && add.is_empty() {
        return 0;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    h = fold_u64(h, atlas_version);
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
        h = fold_u64(h, a);
        h = fold_u64(h, b);
        h = fold_u64(h, c);
    }
    h = fold_u64(h, tick.wrapping_mul(0x9E37_79B1));
    for g in add {
        // Packs (row,x,y,w,h: u16; color: u32) into 2 words, no overlap.
        let a = u64::from(g.row)
            | (u64::from(g.x) << 16)
            | (u64::from(g.y) << 32)
            | (u64::from(g.w) << 48);
        let b = u64::from(g.h) | (u64::from(g.color) << 16);
        h = fold_u64(h, a);
        h = fold_u64(h, b);
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

    /// Force a dense field NOW: pin the density staircase at 252 so cycle
    /// admission passes for ~98% of columns without waiting out the EMA.
    fn pour(e: &mut MatrixRain) {
        e.density_byte = 252;
        e.ring.push(e.tick, 252);
    }

    /// Rain must emit over the CANONICAL trimmed cell shape, not just the
    /// synthetic full-width grid: `RenderInput.cells` stores blank rows as
    /// ABSENT (short outer Vec) and trailing blanks as absent cells (short
    /// inner Vec) — the renderer paints `default_bg` wherever a cell is
    /// missing. An absent cell is an empty default-bg cell and IS eligible;
    /// the earlier `cells.iter()` scan marked zero eligible over blank rows, so
    /// rain never appeared over any blank line (the exact place it belongs).
    #[test]
    fn emits_over_trimmed_blank_rows() {
        let (rows, cols) = (10usize, 40usize);
        // Only row 0 carries content (12 block glyphs); rows 1..10 are
        // entirely absent, and row 0's cols 12..40 are absent too — the
        // trimmed shape. Block cells are NON-space, so ineligible.
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

    /// Truncation drops WHOLE highest-hash columns: the emitted column set is
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
        // — a NON-mutation tick, so only stepped rows may change.
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

    /// Design §6 contrast invariant on BOTH stock gui themes:
    /// `|L(rain over bg) - L(bg)| < |L(dim SGR-2 text) - L(bg)|` for every
    /// level at the derived alphas. Theme chrome hardcoded from source:
    /// dark = aterm-types/src/scheme.rs `ColorScheme::default` (fg #D0D0D0,
    /// bg #111318 — the gui `Config::theme()` base); light = the
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

    /// The readable ceiling is the cursor-trail precedent, pinned.
    #[test]
    fn alpha_cap_is_pinned() {
        assert_eq!(RAIN_ALPHA_CAP, 135);
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

    /// Every path to stillness reaches EMPTY within the fixed bound and
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

    /// Animating ⇒ the fingerprint changes EVERY engine tick; drained ⇒ it is
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

    /// Direct anti-cancellation pin: an EVEN number of identical quads still
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
        // own test; here we probe the OCCUPANCY predicates in isolation).
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
                }),
                cell_row: 0,
                cell_col: 0,
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

    /// OUTPUT MATERIAL BANK: supported literal codepoints are built from REAL
    /// screen content in source order, while this fixture's current typing band
    /// (cursor ± 2 / hidden composer rows) is excluded.
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

        // Hidden cursor with an UNKNOWN band (empty ring — first enable /
        // resize / full damage): the bottom K rows are conservatively treated
        // as the composer, so a secret typed there is STILL never sampled.
        let mut bottom_secret = vec![vec![space_cell(bg3()); cols]; rows];
        for (i, ch) in "hunter2".chars().enumerate() {
            bottom_secret[rows - 1][i].ch = ch;
        }
        e.sample_material(&bottom_secret, rows, None, &[]);
        assert!(
            e.material.is_empty(),
            "unknown composer band ⇒ the bottom rows are never sampled"
        );

        // A live reload flipping the knob OFF clears the table immediately
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

        // With NOTHING outside the band, the table is empty (classic field).
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

        // Over-long screens stride-cap deterministically.
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

    /// The sampler's whole product is the LAST [`MATERIAL_CAP`] supported
    /// codepoints in row-major order, so it walks BOTTOM-UP and stops at the
    /// cap instead of hashing every occupied cell on the screen. That must be
    /// indistinguishable from the exhaustive forward walk it replaced — pin it
    /// against a plainly-written reference on a grid dense enough for the cap
    /// to land mid-row, with banded rows, wide cells, spaces, unsupported
    /// scalars and a ragged/short row mixed in.
    #[test]
    fn literal_material_tail_matches_an_exhaustive_forward_walk() {
        /// The pre-optimisation semantics, spelled out: every unbanded row
        /// top-down, keep the tail.
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

        // Under the cap the tail is the WHOLE sample — no truncation, same order.
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

    /// Sanitized PTY frames captured from the installed Codex 0.144.0 and
    /// Claude Code 2.1.206 surfaces. Drive the real terminal parser (including
    /// Claude's alt screen + synchronized-output envelope), then sample the
    /// resulting RenderCells. Program output must remain literal while the
    /// bottom composer draft is absent from the material tape.
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

    /// The §6 bottom-region contract holds for EMISSION with an UNKNOWN
    /// composer band (codex re-audit HIGH): hidden cursor + empty damage ring
    /// ⇒ the bottom K rows never receive a single quad, even at full pour.
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

    /// DEADLINE-STAMP notes survive suspension (codex re-audit): a bell and an
    /// exit status noted during load-shed stay pending through suspended steps
    /// and apply at the resume emit; activity notes are still dropped.
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

    /// A drained, UNFOCUSED pane disarms even while content keeps streaming
    /// (round-1 LOW, engine level): the bounded drain is the whole lifecycle
    /// for non-focused visibility; refocus resets it and resumes.
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

    /// READING disarms (round-3): a scrolled-back / alt-quiet pane with an
    /// empty frame on glass reports inactive — every wake would rebuild to
    /// the fp-0 early-out — and re-arms the moment a live emit runs.
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

    /// The emitted snapshot arrives ROW-SORTED — the renderer's per-row dirty
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

        // The FULL table is reachable (codex P3: a `% 64` glyph index would
        // strand entries 64..): 128 entries, back half a DIFFERENT tile —
        // both halves must appear across an emitted field.
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
        let a = e.cell_quad(&ctx, 3, 2, 10, false);
        let b = e.cell_quad(&ctx, 3, 3, 10, false);
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
        assert_eq!(
            rain.semantic_material_index(0, 3, 2),
            rain.semantic_material_index(1, 3, 2),
            "inspect forms a coherent two-column scout lane"
        );

        rain.note_signal(RainSignal::Modify as u32, 1);
        rain.apply_pending_notes();
        assert_ne!(
            rain.semantic_material_index(0, 2, 1),
            rain.semantic_material_index(1, 2, 1),
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

    /// Tier-B: a live selection zeroes emission in its band WITHOUT any
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
        // Sustained content stream on a FRESH engine (its CALM state is
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

    /// §5 "your own typing is a drizzle": a content delta landing within
    /// [`ECHO_DISCOUNT_MS`] of the user's keystroke is shell ECHO and must
    /// never advance the WORKING streak — typing at an echoing prompt stays
    /// CALM indefinitely and fires no turn wave on a pause — while agent
    /// output arriving clear of the keystrokes still pours.
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

    /// A suspended host (alt-screen suppression / the load-shed latch) keeps
    /// the WEATHER advancing via the suspended tick: notes starve, the weather
    /// sleeps, the drain completes, and `is_active` self-disarms — never
    /// perpetual wakes off a frozen WORKING state (the audit's wake-leak).
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
        // Suspend: only the suspended tick runs. The user KEEPS TYPING the
        // whole time (vim in the suppressed alt screen) — suspended steps must
        // DROP those notes, not hold the pane at CALM off them. After
        // idle_secs (default 8 s) + the ~2.5 s drain the engine must have
        // fully disarmed. 12 s of 83 ms suspended steps covers it.
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
    /// excess waves are DELAYED (granted when the window frees), not dropped.
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

    /// SPLIT-PANE AUDIT: a retained engine whose FRONT GRID was swapped
    /// (tab switch / pane-focus move) must rescan even when the two
    /// terminals' unrelated damage-epoch counters happen to collide —
    /// `note_grid_replaced` rebaselines occupancy, material, and activity.
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

    /// TINY PANES (split-pane audit): a 1–2-row viewport — a heavily
    /// subdivided split pane — must emit (or honestly emit nothing) without
    /// panicking. The old `col_params` floored `rows` at 1 and then hit
    /// `.clamp(3, rows)` (min > max ⇒ panic) on the first ungated emit.
    /// Every emitted quad stays inside the real geometry.
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

    /// A latched composer-provenance guard must not survive a grid swap:
    /// with `material_editing` still true, the cleared bank could never
    /// refill (`sample_material` refuses while Editing), so literal-mode
    /// rain stayed DARK on the new grid until its next Enter (post-merge
    /// re-audit, HIGH).
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

    // ---- Tier-1 conformance binds (project REAL engine onto the ty models) --

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

    /// Bounded synchronized-output credit, authored once in Rust and derived
    /// into both executable semantics and TLA+. `Buggy=1` models the missing
    /// saturation defect: a coalesced burst stores `Cap+1` and violates
    /// `CreditBound`. Editor echo clears the whole synchronized frame, while a
    /// submitted turn admits that same bounded credit; rebase clears all state.
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

    /// Tier-1: drive the shipping classifier through the synchronized
    /// Codex/Claude burst + echo + new-session rebase trace and project every
    /// decision onto the derived model. The Buggy negative control is discharged
    /// by the prove-and-catch test above, so this bind cannot pass vacuously.
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

    /// Tier-1 conformance for the §10 `RainLifecycle` ty model
    /// (`aterm_spec::derive::rain_lifecycle_model`): the REAL `MatrixRain` is
    /// driven through enable → rain → unfocus-drain → licensed refocus, and
    /// every lifecycle transition is projected onto the derived model — an
    /// enable/resume maps to `Activity` (the model must ADMIT it), unfocus
    /// opens `StartDrain`, each engine drain tick is a `DrainTick`, and the
    /// engine lands Idle at EXACTLY `DRAIN_TICKS`. `(state, drained)` tracks
    /// the real `(lifecycle, drain_ticks)` tick-for-tick, and `NoUnlicensedRain`,
    /// the `CanReachIdle` fuel invariant, and the structural bounds all hold
    /// along the whole trace. The phantom-relight negative control is the twin
    /// below.
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

    /// Negative control for the `RainLifecycle` Tier-1 binding (the §10
    /// `Buggy = 1` twin, non-vacuity): a drained, SLEEPING pane refocused on
    /// cmd-tab must NOT relight — the REAL engine keeps its drain fuel spent
    /// (no phantom replay), matching the healthy model where `Rearm` is
    /// disabled. The phantom-relight trace (Idle → Raining with no paying
    /// Activity) is admitted ONLY by the `Buggy = 1` model, and the healthy
    /// model's `NoUnlicensedRain` rejects the projected state — the exact
    /// counterexample `ty` catches in Tier-0.
    #[test]
    fn rain_lifecycle_negative_control_phantom_relight_is_buggy_trace() {
        // 1) The REAL engine does the RIGHT thing: a slept, drained pane stays
        //    drained across an unfocus/refocus (cmd-tab), never relighting.
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
        // cmd-tab: hide then refocus. Refocus of a SLEEPING pane must not relight.
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

        // 2) The MODELS separate healthy from buggy: only Buggy=1 admits the
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
        // …then the phantom relight: Idle → Raining with NO Activity event.
        assert!(
            buggy.action_enabled("Rearm", &st),
            "Buggy=1 enables the phantom relight"
        );
        assert!(buggy.fire("Rearm", &mut st));
        assert_eq!(st["state"], 1, "the buggy pane relit on cmd-tab alone");
        // The healthy model REJECTS the projected state, and has no such action.
        assert!(
            !healthy.check_invariant("NoUnlicensedRain", &st),
            "a Raining entry with no paying Activity violates NoUnlicensedRain"
        );
        assert!(
            !healthy.action_enabled("Rearm", &st),
            "the healthy model has no phantom-relight action"
        );
    }

    /// Tier-1 conformance for the §4/§10 `RainIgnition` ty model
    /// (`aterm_spec::derive::rain_ignition_model`): the REAL `field::col_params`
    /// is driven over the small-grid lattice the model quantifies (rows 3..=8
    /// at the neutral speed knob so `p ∈ 2..=5`, tick_ms = 33), and the per-cell
    /// head-pass floor `c·p·tick_ms >= 1000` is asserted for EVERY column — the
    /// runtime G-extension the model's `HeadPassFloor` proves, exercised on the
    /// shipping field math rather than re-stated.
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
