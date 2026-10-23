// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cursor aurora: pure host-side animation that turns cursor motion into light: a comet of premultiplied additive quads along the swept path, a bloom crown around the head, and per-style particles.

use aterm_time::Instant;
use std::time::Duration;

use aterm_hash::{FNV64_OFFSET, fnv_mix_u64};
use aterm_render::{
    BEAM_LAYERS, BeamClip, BeamScratch, BeamVertex, COMET_LAYERS, CharFg, CometSample,
    FireHaloCell, FireMode, FirePatch, GlowQuad, HaloMode, LASER_LAYERS, PHASER_LAYERS, RainHalo,
    SPARKLE_LAYERS, comet_beam, comet_beam_over, layer_stack_lift, layered_beam_quads_with,
    premul_rgb,
};

use crate::rainbow_kitty::{self as rk, CaretSeam, CompanionImpulse};

/// Host row slots for the content witness: the bands' eight rows ([`rk::witness::WITNESS_ROWS`]), a waiting key's source row and its neighbours ([`rk::witness::ARMING_ROWS`]), and the caret's row when it is outside both sets.
pub const CURSOR_WITNESS_ROWS: usize = RIBBON_LIST_ROWS + 1;

const RIBBON_LIST_ROWS: usize = rk::witness::WITNESS_ROWS + rk::witness::ARMING_ROWS;

use crate::effect_util::{
    STAR_ARM_FINE, STAR_ARM_INK, STAR_ARM_STD, STAR_CORE, STAR_GLINT, STAR_GLINT_COV,
    TWINKLE_FLASH_BOUND_HZ, dust_r, fire_ramp, lerp_rgb, push_fx_quad, push_fx_rect as push_rect,
    push_twinkle_star, star_accent, star_arm, star_arm_px, twinkle_env, twinkle_peak, water_ramp,
};
use crate::spectrum::{SPECTRUM_STOPS, spectrum, spectrum_snap};
use crate::trail_sweep::line_cells_tail;
use crate::typing_momentum::TypingMomentum;

mod bed;
mod companion;
mod custody;
mod emit;
mod ink_ledger;
mod light_body;
mod probe;
mod raster;
mod scroll;
mod seam;
mod sound;
mod spawn;
pub mod spellings;
mod status;
mod style;
mod thermal;
pub(crate) use bed::{
    BedScratch, BedShape, clip_quads_to, dark_bed_budget, dark_bed_ink, lay_dark_bed, night_rgb,
    split_air_runs,
};
pub use companion::*;
pub use custody::*;
use ink_ledger::BandLifts;
pub(crate) use ink_ledger::InkLedger;
pub use ink_ledger::OverInkTier;
pub(crate) use light_body::*;
pub use probe::*;
pub use raster::*;
pub use scroll::*;
pub use sound::*;
pub use status::*;
pub use style::*;

#[derive(Clone, Copy)]
struct Spark {
    row: u16,
    col: u16,
    born_cov: u8,
    pos: f32,
    life: f32,
    typing: bool,
    hue: f32,
    born: Instant,
}

#[derive(Clone, Copy)]
struct WaterSample {
    surface: BeamVertex,
    phase: f32,
    amp: f32,
    line_y: f32,
    row: u16,
    col: u16,
    drip_cov: u8,
    drip_x: f32,
    drip_y: f32,
    drip_h: f32,
}

#[derive(Clone, Copy)]
struct Particle {
    x0: f32,
    y0: f32,
    vx: f32,
    vy: f32,
    gy: f32,
    life: f32,
    hue: f32,
    cov_scale: f32,
    born: Instant,
}

#[derive(Clone, Copy)]
struct Ring {
    cx: f32,
    cy: f32,
    born: Instant,
    life: f32,
    scale: f32,
    caret: Option<(u16, u16)>,
}

#[derive(Clone, Copy)]
struct Meteor {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    born: Instant,
    life: f32,
    mom: f32,
    arrived: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VaporKind {
    Steam,
    Smoke,
    Poof,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PoofVoice {
    Swoosh,
    WordPoof,
    Puff,
}

#[derive(Clone, Copy)]
struct Vapor {
    x0: f32,
    y0: f32,
    vx: f32,
    vy: f32,
    gy: f32,
    life: f32,
    seed: f32,
    kind: VaporKind,
    born: Instant,
}

struct Bolt {
    pts: Vec<(f32, f32)>,
    born: Instant,
    life: f32,
    branch: bool,
    crackle: bool,
}

struct OutgoingFade {
    engine: Box<CursorGlow>,
    cfg: GlowConfig,
    anchor: Option<(u16, u16)>,
    started: Instant,
    level0: f32,
}

type BoltJag = (Vec<(f32, f32)>, Vec<(f32, f32)>);

/// Per-window aurora animation state.
#[derive(Default)]
pub struct CursorGlow {
    classic: crate::classic_wake::ClassicWake,
    sparks: Vec<Spark>,
    admission_log: AdmissionLog,
    spawns: u64,
    path_scratch: Vec<(i32, i32)>,
    particles: Vec<Particle>,
    ring: Option<Ring>,
    last_ch: u16,
    last_origin_y: u16,
    coord_band_moves: u64,
    type_press_ring: PressCredits,
    last_committed_type: Option<Instant>,
    momentum: TypingMomentum,
    momentum_pulse: Option<CursorCatMotionPulse>,
    fire_meteors: Vec<Meteor>,
    vapor: Vec<Vapor>,
    last_smoke: Option<Instant>,
    halo_out: Vec<RainHalo>,
    patch_out: Vec<FirePatch>,
    under_out: Vec<GlowQuad>,
    char_out: Vec<CharFg>,
    fire_halo_out: Vec<FireHaloCell>,
    engulf_scratch: Vec<(u16, u16, f32)>,
    bolts: Vec<Bolt>,
    reduced_motion: bool,
    last: Option<(u16, u16)>,
    last_visible: Option<((u16, u16), Instant)>,
    hide_bridge_shown: Option<(u16, u16)>,
    print_anchor: Option<(u16, u16, u64)>,
    print_anchor_tick_seq: Option<u64>,
    print_in_tick: bool,
    print_anchor_glyph: Option<char>,
    caret_hidden: bool,
    print_anchor_seen: u64,
    anchor_rows: [Option<AnchorRow>; ANCHOR_ROWS],
    anchor_rows_clock: u64,
    last_anchor_sweep: Option<(u16, Instant)>,
    wrap_paid_row: Option<u16>,
    last_move: Option<Instant>,
    crown_until: Option<Instant>,
    crown_window_ms: u64,
    last_style: Option<GlowStyle>,
    last_classic_mono: bool,
    hue: f32,
    heat: f32,
    heat_at: Option<Instant>,
    flare: f32,
    coal: f32,
    quench: f32,
    quench_hint: Option<Instant>,
    last_erase: Option<EraseWitness>,
    nav_hint: Option<Instant>,
    type_hint: TypedStamps,
    return_hint: Option<Instant>,
    user_gesture_hint: Option<Instant>,
    insert: InsertSeam,
    in_flight_tally: InFlightTally,
    held_park: Option<HeldPark>,
    torn_lay: Option<TornLay>,
    park_source_intact: Option<(u16, u16)>,
    carried_key_move: Option<((u16, u16), (u16, u16))>,
    park_source_cells: Vec<char>,
    park_source_confirmed: bool,
    last_licensed_row: Option<(u16, Instant)>,
    hand_row: Option<HandRow>,
    unowned_caret: Option<UnownedCaret>,
    newline_hint: Option<Instant>,
    reflow_hint: Option<Instant>,
    kill_hint: Option<Instant>,
    kill_hint_word: bool,
    kill_retreat_pending: Option<Instant>,
    kill_reported_to_v2: Option<Instant>,
    bs_poof_hint: Option<Instant>,
    bs_baseline: Option<(u16, u16)>,
    blink_hint: Option<Instant>,
    ctx_alt: bool,
    pane_columns: Option<(u16, u16)>,
    last_poof: Option<Instant>,
    glyph_palette: Option<GlyphPaletteMemo>,
    erase_mom: TypingMomentum,
    sound_cues: Vec<SoundCue>,
    keyed_clicks: u8,
    keyed_click_at: Option<Instant>,
    key_cue_at: Option<Instant>,
    heat_gain_live: f32,
    heat_tau_live: f32,
    sound_live: bool,
    row_cur: Vec<char>,
    row_prev: Vec<char>,
    row_cur_meta: Option<RowProbe>,
    row_prev_meta: Option<RowProbe>,
    row_shown: Vec<char>,
    row_shown_meta: Option<RowProbe>,
    recent_typed_run: Option<RecentTypedRun>,
    row_above_cur: Vec<char>,
    row_above_prev: Vec<char>,
    row_below_cur: Vec<char>,
    row_below_prev: Vec<char>,
    disp_t: f32,
    flame_phase: f32,
    cursor_temp: f32,
    forge_band: Option<thermal::ForgeBand>,
    last_type: Option<Instant>,
    rng: u32,
    comet_runs: Vec<Vec<CometSample>>,
    comet_run: Vec<CometSample>,
    comet_typing: Vec<Vec<bool>>,
    comet_run_typing: Vec<bool>,
    leap_spans: Vec<(Instant, u16, u16, u16)>,
    water_samples: Vec<WaterSample>,
    water_seen: Vec<u64>,
    water_run: Vec<BeamVertex>,
    bolt_verts: Vec<BeamVertex>,
    bolt_channel: Vec<(f32, f32, f32, bool)>,
    bolt_jag: BoltJag,
    beam_scratch: BeamScratch,
    ink_ledger: InkLedger,
    #[cfg(test)]
    streak_unbudgeted: bool,
    #[cfg(test)]
    bed_unsolved: bool,
    #[cfg(test)]
    over_ink_idle: bool,
    #[cfg(test)]
    over_ink_uniform: bool,
    bolt_onsets: [Option<Instant>; Self::BOLT_FLASHES_PER_S],
    bolt_onset_next: usize,
    #[cfg(test)]
    bolts_unlimited: bool,
    #[cfg(test)]
    fire_flat_feet: bool,
    #[cfg(test)]
    fire_row_unheld: bool,
    #[cfg(test)]
    fire_rises_into_text: bool,
    #[cfg(test)]
    fire_coreless: bool,
    #[cfg(test)]
    laser_aura_unheld: bool,
    #[cfg(test)]
    sparkle_paper_bold_ink: bool,
    #[cfg(test)]
    streak_chords_rows: bool,
    #[cfg(test)]
    leap_split: bool,
    #[cfg(test)]
    heads_square: bool,
    #[cfg(test)]
    fire_core_uncooled: bool,
    #[cfg(test)]
    fire_held_hottest: bool,
    #[cfg(test)]
    fire_kept_strip: bool,
    #[cfg(test)]
    fire_kept_core_from_base: bool,
    #[cfg(test)]
    fire_meteor_joined: bool,
    #[cfg(test)]
    neighbour_hold_hard: bool,
    #[cfg(test)]
    veil_unbound: bool,
    light_verts: Vec<aterm_render::RibbonVertex>,
    light_band_run: Vec<CometSample>,
    bed_scratch: BedScratch,
    air_runs: Vec<Vec<CometSample>>,
    wedge_runs: Vec<Vec<CometSample>>,
    frame_text: FrameText,
    #[cfg(test)]
    light_body_unsolved: bool,
    #[cfg(test)]
    light_glints: Vec<(f32, f32)>,
    fading: Vec<OutgoingFade>,
    fade_scratch: Vec<GlowQuad>,
    ramp_in_at: Option<Instant>,
    last_cfg: Option<GlowConfig>,
    dark_settled: bool,
    v2: rk::Engine,
    v2_beams: Vec<BeamVertex>,
    v2_probe_scratch: Vec<rk::stardust::CellInk>,
    witness_rows: Vec<WitnessRowBuf>,
    witness_rows_n: usize,
    scroll_rows: u16,
    witness_asked: std::cell::Cell<([u16; RIBBON_LIST_ROWS], usize)>,
    v2_caret: CaretSeam,
    v2_stop: Option<(f32, Option<(u16, u16)>)>,
    flow_open: bool,
    v2_cues: Vec<SoundCue>,
}

/// Window-space geometry the host passes in (pixels).
#[derive(Clone, Copy)]
pub struct Geom {
    pub cw: usize,
    pub ch: usize,
    pub rows: usize,
    pub cols: usize,
    /// Grid-interior top-left X in window pixels.
    pub origin_x: u16,
    /// Grid-interior top-left Y in window pixels.
    pub origin_y: u16,
    /// Full frame width in pixels.
    pub win_w: u16,
    /// Full frame height in pixels.
    pub win_h: u16,
    /// Rise allowance above the grid in pixels.
    pub head: u16,
}

impl Geom {
    /// Returns the left edge of the effects box in window pixels.
    #[inline]
    pub(crate) const fn fx_left(&self) -> i32 {
        self.origin_x as i32
    }
    #[inline]
    pub(crate) const fn fx_right(&self) -> i32 {
        self.origin_x as i32 + (self.cols * self.cw) as i32
    }
    #[inline]
    pub(crate) const fn fx_top(&self) -> i32 {
        self.origin_y as i32 - self.head as i32
    }
    #[inline]
    pub(crate) const fn fx_bot(&self) -> i32 {
        self.origin_y as i32 + (self.rows * self.ch) as i32
    }

    /// Returns the window-pixel centre of `(row, col)`.
    #[inline]
    pub(crate) const fn cell_center(&self, row: u16, col: u16) -> (f32, f32) {
        (
            self.origin_x as f32 + (col as f32 + 0.5) * self.cw as f32,
            self.origin_y as f32 + (row as f32 + 0.5) * self.ch as f32,
        )
    }

    /// Returns the beam rasterizer clip for this geometry.
    #[inline]
    pub(crate) const fn beam_clip(&self) -> BeamClip {
        BeamClip {
            x0: self.fx_left(),
            y0: self.fx_top(),
            x1: self.fx_right(),
            y1: self.fx_bot(),
            cell_h: self.ch as i32,
            origin_y: self.origin_y as i32,
        }
    }
}

#[derive(Clone, Copy)]
struct ExactTypedRun {
    origin_row: u16,
    slot: usize,
    source: Option<(u16, u16, u16)>,
    landing_start: u16,
    cells: usize,
    rewrap_start: Option<(u16, u16)>,
}

struct MoveCtx<'a> {
    pr: u16,
    pc: u16,
    cr: u16,
    cc: u16,
    now: Instant,
    cfg: &'a GlowConfig,
    geom: Geom,
    dr_abs: i32,
    dc_abs: i32,
    pane_col0: usize,
    pane_cols: usize,
    shape_wrap: bool,
    fold_laid: usize,
    echo_run: bool,
    re_anchor: bool,
    rainbow_coalesce: bool,
    credit_starved: bool,
    typed_over_cap: bool,
    re_anchor_lays_landing: bool,
    exact_typed_run: Option<ExactTypedRun>,
    landing_foreign: bool,
    soft_wrap: Option<u16>,
    lift: Option<u16>,
    in_flight_licence: bool,
    wrap: bool,
    typed_hinted: bool,
    typed_at: Option<Instant>,
    bs_pair: bool,
    dist: f32,
    typing: bool,
    fire: bool,
    forward: bool,
    deletion: bool,
    navigation: bool,
    nav_taken: Option<Instant>,
}

#[derive(Default)]
struct FrameFingerprint(Option<u64>);

impl FrameFingerprint {
    const GLOW_OUT: u64 = 0x676c_6f77_5f6f_7574;
    const FIRE_PATCH: u64 = 0x6669_7265_7061_7463;
    const GLOW_UNDER: u64 = 0x676c_6f77_756e_6472;
    const CHAR_FG: u64 = 0x6368_6172_5f66_6700;
    const FIRE_HALO: u64 = 0x6669_7265_6861_6c6f;
    const RAIN_HALO: u64 = 0x7261_696e_6861_6c6f;
    const FORGE_FILL: u64 = 0x666f_7267_655f_6669;

    #[inline]
    const fn write(&mut self, word: u64) {
        let prev = match self.0 {
            Some(h) => h,
            None => FNV64_OFFSET,
        };
        self.0 = Some(fnv_mix_u64(prev, word));
    }

    #[inline]
    const fn begin_stream(&mut self, tag: u64, len: usize) {
        self.write(tag);
        self.write(len as u64);
    }

    #[inline]
    fn glow_quads(&mut self, tag: u64, quads: &[GlowQuad]) {
        if quads.is_empty() {
            return;
        }
        self.begin_stream(tag, quads.len());
        let mut hash = match self.0 {
            Some(h) => h,
            None => FNV64_OFFSET,
        };
        let mut qi = 0;
        while qi < quads.len() {
            let words = quads[qi].damage_words();
            let words = words.as_slice();
            let mut wi = 0;
            while wi < words.len() {
                hash = fnv_mix_u64(hash, words[wi]);
                wi += 1;
            }
            qi += 1;
        }
        self.0 = Some(hash);
    }

    #[inline]
    const fn fire_patches(&mut self, patches: &[FirePatch]) {
        if patches.is_empty() {
            return;
        }
        self.begin_stream(Self::FIRE_PATCH, patches.len());
        let mut hash = match self.0 {
            Some(h) => h,
            None => FNV64_OFFSET,
        };
        let mut pi = 0;
        while pi < patches.len() {
            let words = patches[pi].damage_words();
            let mut wi = 0;
            while wi < words.len() {
                hash = fnv_mix_u64(hash, words[wi]);
                wi += 1;
            }
            pi += 1;
        }
        self.0 = Some(hash);
    }

    #[inline]
    const fn chars(&mut self, chars: &[CharFg]) {
        if chars.is_empty() {
            return;
        }
        self.begin_stream(Self::CHAR_FG, chars.len());
        let mut hash = match self.0 {
            Some(h) => h,
            None => FNV64_OFFSET,
        };
        let mut ci = 0;
        while ci < chars.len() {
            let c = &chars[ci];
            hash = fnv_mix_u64(
                hash,
                (c.row as u64) | ((c.col as u64) << 16) | ((c.fg as u64) << 32),
            );
            ci += 1;
        }
        self.0 = Some(hash);
    }

    #[inline]
    const fn fire_halos(&mut self, cells: &[FireHaloCell]) {
        if cells.is_empty() {
            return;
        }
        self.begin_stream(Self::FIRE_HALO, cells.len());
        let mut hash = match self.0 {
            Some(h) => h,
            None => FNV64_OFFSET,
        };
        let mut ci = 0;
        while ci < cells.len() {
            let c = &cells[ci];
            hash = fnv_mix_u64(
                hash,
                (c.row as u64) | ((c.col as u64) << 16) | ((c.strength as u64) << 32),
            );
            ci += 1;
        }
        self.0 = Some(hash);
    }

    #[inline]
    const fn rain_halos(&mut self, halos: &[RainHalo]) {
        if halos.is_empty() {
            return;
        }
        self.begin_stream(Self::RAIN_HALO, halos.len());
        let mut hash = match self.0 {
            Some(h) => h,
            None => FNV64_OFFSET,
        };
        let mut hi = 0;
        while hi < halos.len() {
            let words = halos[hi].damage_words();
            let mut wi = 0;
            while wi < words.len() {
                hash = fnv_mix_u64(hash, words[wi]);
                wi += 1;
            }
            hi += 1;
        }
        self.0 = Some(hash);
    }

    #[inline]
    const fn forge_fill(&mut self, fill: u32) {
        self.begin_stream(Self::FORGE_FILL, 1);
        self.write(fill as u64);
    }

    #[inline]
    const fn finish(self) -> u64 {
        match self.0 {
            None => 0,
            Some(0) => 1,
            Some(fp) => fp,
        }
    }
}

impl CursorGlow {
    const MAX_QUADS: usize = 16384;
    const MAX_SPARKS: usize = 512;
    const WATER_OWNER_SLOTS: usize = Self::MAX_SPARKS * 2;
    const MAX_PARTICLES: usize = 512;
    const RAINBOW_TYPED_SWEEP_MAX: usize = TYPED_STAMP_DEPTH;
    const ANCHOR_UNPROVEN_ROW_MAX: usize = 32;

    const MAX_BOLTS: usize = 24;
    const BOLT_FLASHES_PER_S: usize = 3;
    const CRACKLE_REFRACTORY_S: f32 = 0.55;
    const CROWN_MS: u64 = 200;
    const CROWN_TYPING_MS: u64 = 350;
    const HEAT_GAIN: f32 = 0.16;
    const HEAT_DECAY_TAU: f32 = 0.9;
    const FLARE_DECAY_TAU: f32 = 0.45;
    const HEAT_GAP_FULL: f32 = 0.09;
    const HEAT_GAP_ZERO: f32 = 0.40;
    const CHAIN_GAP_MAX: f32 = 0.40;
    const CHAIN_MARGIN: f32 = 0.10;
    const CHAIN_LIFE_MAX: f32 = 0.50;
    const FIRE_HEAT_GAIN: f32 = 0.045;
    const FIRE_HEAT_TAU: f32 = 2.8;
    const COAL_TAU: f32 = 6.0;
    const COAL_GAIN: f32 = 0.022;
    const COAL_GAP_FULL: f32 = 0.35;
    const COAL_GAP_ZERO: f32 = 1.50;
    const COAL_FLOOR: f32 = 0.75;
    const QUENCH_GAIN: f32 = 0.28;
    const QUENCH_TAU: f32 = 1.2;
    const QUENCH_COOL: f32 = 0.8;
    const QUENCH_DAMP: f32 = 0.6;
    const QUENCH_HINT_FRESH: f32 = 0.25;
    const NAV_HINT_FRESH: f32 = 0.25;
    /// Freshness window for a typed-glyph classifier and its exact candidate.
    pub(crate) const TYPE_HINT_FRESH: f32 = 0.25;
    /// Rest window before a typed lay is judged against where the caret rests.
    pub(crate) const TORN_REST_S: f32 = 0.032;
    const RETURN_HINT_FRESH: f32 = 0.25;
    const USER_GESTURE_HINT_FRESH: f32 = 0.25;
    /// Freshness window for the delivered-insert licence.
    pub const INSERT_HINT_FRESH: f32 = 2.0;
    /// Freshness window for an unknown-width insert gesture.
    pub const INSERT_GESTURE_HINT_FRESH: f32 = 1.0;
    /// Freshness window for a program's same-row rewrite of a delivered insert.
    pub const INSERT_REWRITE_FRESH: f32 = 2.0;
    /// Freshness window for claiming a refused same-row forward hop on delivery receipt.
    pub const INSERT_RETRO_FRESH: f32 = 0.25;
    /// Maximum cells a same-row forward echo may span under an unknown-width insert gesture.
    pub const INSERT_GESTURE_CELLS: u16 = 32;
    const REFLOW_HINT_FRESH: f32 = 0.40;
    const KILL_HINT_FRESH: f32 = 0.35;
    const BLINK_HINT_FRESH: f32 = 0.5;
    const KILL_HINT_POLL_INTERVAL: Duration = Duration::from_millis(40);
    const POOF_FALLBACK_GRACE: f32 = 0.06;
    const POOF_MIN_GAP: f32 = 0.14;
    const POOF_PROBE_STALE: f32 = 5.0;
    const KILL_QUENCH_GAIN: f32 = 0.55;
    const DISP_ATTACK_S: f32 = 0.34;
    const DISP_SLAM_S: f32 = 0.05;
    const DISP_RELEASE_TAU: f32 = 0.45;
    const FLAME_PHASE_RING: f32 = std::f32::consts::TAU * 640.0;
    /// Base flame phase speed in units per second when cold.
    pub(crate) const FLAME_PHASE_SPEED: f32 = 1.6;
    pub(crate) const FLAME_PHASE_HEAT_SPEED: f32 = 2.2;
    /// Base height flicker rate for a fire tongue in cycles per unit of flame phase.
    pub(crate) const TONGUE_FLICKER_RATE: f32 = 0.45;
    pub(crate) const TONGUE_FLICKER_SPREAD: f32 = 0.33;
    const TEMP_ATTACK_TAU: f32 = 1.5;
    const TEMP_RELEASE_TAU: f32 = 1.4;
    const FORGE_MIN_TEMP: f32 = 0.12;
    const EMBER_POLL_INTERVAL: Duration = Duration::from_millis(90);
    const METEOR_MIN_COLS: i32 = 8;
    const METEOR_CAP: usize = 4;
    const METEOR_RETARGET_S: f32 = 0.12;
    const METEOR_STRIKE_FRAC: f32 = 0.62;
    /// Hard cap on per-frame radial halos.
    pub(crate) const MAX_HALOS: usize = 512;
    const MAX_VAPOR: usize = 128;
    const STEAM_STACK: f32 = 2.0;
    const STEAM_ABOVE_DIM: f32 = 0.65;
    const INK_LEDGER_REACH: i32 = 3;
    const STEAM_ECHO_PUFFS_MAX: usize = 3;
    const SMOKE_TEMP: f32 = 0.35;

    const PHASER_CHAIN_KEYS: f32 = 4.5;
    const PHASER_CHAIN_GAP_MAX: f32 = 0.75;
    const PHASER_CHAIN_LIFE_MAX: f32 = 3.5;
    const LASER_RESIDUAL: f32 = 0.30;
    const LIGHT_BOLT_ALPHA_CAP: f32 = 144.0;
    const LASER_RIPPLE_OMEGA: f32 = 18.0;
    const FIRE_STREAK_COV_CAP: f32 = 108.0;
    const FIRE_HEAD_COV_CAP: f32 = 168.0;
    const FIRE_SUBCOLS: i32 = 6;
    const FIRE_SLICE_PX: i32 = 2;
    const FRAME_TEXT_RISE: i32 = 3;
    const FIRE_DARK_AIR_CAP_COLD: f32 = 96.0;
    const FIRE_DARK_AIR_CAP_HEAT: f32 = 64.0;
    const FIRE_DARK_AIR_RANKS: [f32; 6] = [1.0, 0.92, 0.82, 0.68, 0.5, 0.32];
    const FIRE_DARK_AIR_PEAK: f32 = 2.4;
    const FIRE_DARK_AIR_TEMP: f32 = 0.72;
    const FIRE_DARK_AIR_TONGUE_CAP: f32 = 208.0;
    const FIRE_DARK_ROOT: f32 = 0.20;
    const FIRE_DARK_AIR_FOOT: f32 = 0.2;
    const FIRE_DARK_SPACING: f32 = 0.85;
    const FIRE_DARK_HALF_W: f32 = 0.70;
    const FIRE_DARK_TONGUE_SHAPE: f32 = 1.43;
    const FIRE_DARK_LOW_BAND: f32 = 0.80;
    const FIRE_DARK_AIR_FIELD_REACH: f32 = 1.8;
    const FIRE_DARK_CORE_REACH: f32 = 0.70;
    const FIRE_DARK_CORE_W: f32 = 0.55;
    const FIRE_DARK_CORE_COV: f32 = 190.0;
    const FIRE_DARK_CORE_YELLOW_COV: f32 = 150.0;
    const FIRE_DARK_CORE_AMBER: f32 = 0.62;
    const FIRE_DARK_KEPT_CORE_YELLOW_COV: f32 = 120.0;
    const FIRE_METEOR_FEATHER: f32 = 0.5;
    const FIRE_METEOR_HEAD_EASE: f32 = 0.8;
    const FIRE_DARK_GAP_TONGUE: f32 = 0.95;
    const FIRE_DARK_CORE_ROOT_PX: f32 = 5.0;
    const FIRE_DARK_KEPT_CORE_COV: f32 = 255.0;
    const FIRE_DARK_KEPT_CORE_ROOT_PX: f32 = 4.0;
    const FIRE_DARK_CORE_CLEAR_PX: f32 = 1.0;
    const FIRE_DARK_FOOT_CURL: f32 = 0.25;
    const FIRE_DARK_AIR_LEAD: f32 = 0.45;
    const FIRE_DARK_ROW_ROOT: f32 = 0.98;
    const FIRE_DARK_ROW_HEADROOM: f32 = 0.06;
    const FIRE_DARK_ROW_TOP_FADE: f32 = 0.28;
    const FIRE_DARK_ROW_FOOT: f32 = 0.18;
    const FIRE_DARK_ROW_FIELD_REACH: f32 = 3.4;
    const FIRE_DARK_KEPT_REACH: f32 = 1.7;
    const FIRE_DARK_ROW_RISE_COLD: f32 = 0.8;
    const FIRE_DARK_ROW_TEMP: f32 = 0.2;
    const FIRE_DARK_ROW_BAR: f32 = 6.2;
    const FIRE_DARK_ROW_EASE: f32 = 0.25;
    const FIRE_DARK_HELD_CELLS: usize = 256;
    /// Heat levels solved for each letter's ceiling when a dark flame is kept in its row.
    pub(super) const FIRE_DARK_HELD_LEVELS: usize = 15;
    const LEAP_BIRTH_SKIP_CELLS: f32 = 1.5;
    const FLAME_SNUFF_S: f32 = 0.18;
    const SPARK_SNUFF_S: f32 = 0.08;
    const STRIKE_DEBRIS_REACH: f32 = 0.35;
    const FIRE_EMBER_COV: f32 = 0.85;
    const FIRE_EMBER_R: f32 = 0.04;
    const FIRE_EMBER_R_HOT: f32 = 0.05;
    const LIGHT_VEIL_SAT: f32 = 0.75;
    /// Contrast floor a light landing veil preserves for theme text over its centre.
    pub(crate) const LIGHT_VEIL_BAR: f32 = 4.8;
    /// Horizontal reach in cells of a light landing veil on either side of the caret's centre.
    pub(crate) const LIGHT_VEIL_REACH_CELLS: f32 = 1.5;
    const FIRE_LIGHT_HEADROOM: f32 = 0.08;
    const FIRE_LIGHT_ALPHA: f32 = 240.0;
    const FIRE_LIGHT_ROOT: f32 = 0.70;
    const FIRE_LIGHT_RISE_COLD: f32 = 2.3;
    const FIRE_LIGHT_RISE_HOT: f32 = 0.85;
    const FIRE_LIGHT_SPACING: f32 = 0.85;
    const FIRE_LIGHT_HALF_W: f32 = 0.62;
    const FIRE_LIGHT_KNOT_PX: f32 = 2.0;
    const FIRE_LIGHT_SLAB_PX: f32 = 2.0;
    const FIRE_LIGHT_REF_RED: u32 = 0x00CF_222E;
    const LIGHT_METEOR_ALPHA_CAP: f32 = 144.0;
    const LIGHT_METEOR_RAMP: (f32, f32) = (0.25, 0.50);
    const FIRE_LIGHT_LANDING: u32 = 0x00F2_9A4A;
    const LIGHT_EMBER_INKS: (u32, u32) = (0x00B4_3A0C, 0x00F2_8420);
    const STREAK_INK_FLOOR: f32 = 4.5;
    const STREAK_REF_INK: u32 = 0x00D0_D0D0;
    const STREAK_REF_GROUND: u32 = 0x0011_1318;
    const WATER_WAKE_COV_CAP: f32 = 88.0;
    const LIGHT_WATER_ALPHA_CAP: f32 = 160.0;
    const GRAIN_OVER_GLYPH_DIM: f32 = 0.30;
    const LIGHT_GRAIN_GAIN: f32 = 1.2;
    const MOON_DUST_RATIO: f32 = 2.0;
    const SPARKLE_DARK_LIFT: f32 = 1.35;
    const SPARKLE_DARK_GROW: f32 = 1.3;
    const COMET_LIGHT_GLINTS: [(f32, bool); 3] = [(1.5, true), (2.3, false), (3.1, true)];
    const LIGHT_BAND_TAIL_CELLS: f32 = 1.5;
    const LIGHT_BAND_HEAD_KNOTS: usize = 6;
    const COMET_LIGHT_GLINT_ARM: f32 = 0.19;
    const COMET_LIGHT_COMA_R: f32 = 0.5;
    const COMET_LIGHT_COMA_RIM: f32 = 0.11;
    const COMET_LIGHT_GLINT_RISE: (f32, f32) = (0.072, 0.10);
    const COMET_LIGHT_GLINT_HIGH: f32 = 0.075;
    const GLYPH_BAND: (f32, f32) = (0.12, 0.86);
    const COMET_DARK_DUST_HIGH: f32 = 0.02;
    const COMET_DARK_DUST_LOW: f32 = 0.95;
    const COMET_DARK_DUST_SHARE: f32 = 0.4;
    const COMET_DARK_DUST_JITTER: f32 = 0.04;
    const COMET_LIGHT_GLINT_LOW: f32 = 0.885;
    const WATER_LINE_DY: f32 = 0.10;
    const WATER_SWELL: (f32, f32) = (0.08, 0.12);
    const WATER_KNOTS: usize = 4;
    const LIGHT_WATER_DROP_INKS: (u32, u32) = (0x0010_6EB4, 0x0036_A8E2);
    const WATER_DROP_SOFT: f32 = 1.6;
    const MOON_GOLD: u32 = 0x00FF_BE3C;
    const MOON_FADE_POW: f32 = 0.4;
    const SPARKLE_PAPER_RIM: u32 = 0x00B8_6E00;
    const SPARKLE_PAPER_CORE: u32 = 0x00F5_B618;
    const SPARKLE_PAPER_STAR: u32 = 0x00A6_6000;
    const SPARKLE_PAPER_MOON: u32 = 0x00C4_7C00;
    const SPARKLE_PAPER_BEAD: f32 = 0.85;
    const LASER_AURA_COV: f32 = 0.42;
    /// Distance in cells from the path's head over which the laser aura keeps its peak.
    pub(super) const LASER_AURA_FULL_CELLS: f32 = 0.5;
    /// Distance in cells from the path's head at which the laser aura fades to zero.
    pub(super) const LASER_AURA_REACH_CELLS: f32 = 2.0;
    const CRACKLE_TRAIL_CELLS: usize = 12;
    const BEAM_CHAIN_KEYS: f32 = 3.5;
    const BEAM_CHAIN_GAP_MAX: f32 = 0.75;
    const BEAM_CHAIN_LIFE_MAX: f32 = 1.2;
    const WATER_CHAIN_KEYS: f32 = 4.0;
    const WATER_CHAIN_LIFE_MAX: f32 = 1.2;

    /// Returns the last visible cursor cell owned by this engine.
    #[inline]
    #[must_use]
    pub const fn cursor_anchor(&self) -> Option<(u16, u16)> {
        match (self.last, self.last_visible) {
            (Some(cell), _) | (None, Some((cell, _))) => Some(cell),
            (None, None) => None,
        }
    }

    /// Sets the reduced-motion flag and forwards it to Rainbow Kitty when engaged.
    #[inline]
    pub fn set_reduced_motion(&mut self, reduced: bool) {
        self.reduced_motion = reduced;
        if self.v2.engaged()
            && let Some(at) = self.heat_at
        {
            self.v2.on_event(rk::Event::ReducedMotion(reduced), at);
        }
    }

    /// Forwards a window OS focus change to Rainbow Kitty when engaged.
    #[inline]
    pub fn note_focus(&mut self, focused: bool, now: Instant) {
        if self.v2.engaged() {
            self.v2.on_event(rk::Event::Focus(focused), now);
        }
    }

    /// Records a host repaint blink epoch advance.
    #[inline]
    pub const fn note_repaint_blink(&mut self, now: Instant) {
        self.unsettle();
        self.blink_hint = Some(now);
    }

    /// Clears any pending repaint blink hint across a terminal switch.
    #[inline]
    pub const fn clear_blink(&mut self) {
        self.blink_hint = None;
    }

    /// Records whether the probed terminal is on the alternate screen this frame.
    #[inline]
    pub const fn note_context(&mut self, alt: bool) {
        self.ctx_alt = alt;
    }

    /// Records the focused pane's `(first_row, rows)` span in tick coordinates.
    #[inline]
    pub const fn note_pane_rows(&mut self, first_row: u16, rows: usize) {
        let pane_rows = if rows > 0 && rows <= u16::MAX as usize {
            Some((first_row, rows as u16))
        } else {
            None
        };
        self.v2.set_pane_rows(pane_rows);
    }

    /// Records the focused pane's `(first_col, cols)` span in tick coordinates.
    #[inline]
    pub const fn note_pane_columns(&mut self, first_col: u16, cols: usize) {
        self.pane_columns = if cols > 0 && cols <= u16::MAX as usize {
            Some((first_col, cols as u16))
        } else {
            None
        };
        self.v2.set_pane(self.pane_columns);
    }

    /// Returns whether any light or pending witness keeps the animation timer armed.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.has_live_motion()
            || self.ember_live()
            || self.poof_hint_pending().is_some()
            || (self.v2.engaged() && self.v2.fingerprint() != 0)
            || (self.v2.engaged() && self.held_park.is_some())
            || (self.v2.engaged() && self.torn_lay.is_some())
    }

    #[inline]
    const fn ember_live(&self) -> bool {
        self.cursor_temp > Self::FORGE_MIN_TEMP
            || (matches!(self.last_style, Some(GlowStyle::Fire))
                && self.disp_t > Self::FORGE_MIN_TEMP)
    }

    #[inline]
    fn poof_hint_pending(&self) -> Option<Instant> {
        match (self.kill_hint, self.bs_poof_hint) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    #[inline]
    const fn poof_erase_witnessed(&self) -> bool {
        match (self.bs_baseline, self.row_cur_meta) {
            (Some((brow, bfill)), Some(cur)) => {
                matches!(cur.trust, ProbeTrust::Full) && cur.row == brow && cur.fill < bfill
            }
            _ => false,
        }
    }

    #[inline]
    const fn has_live_motion(&self) -> bool {
        self.needs_frame_cadence() || !self.vapor.is_empty()
    }

    /// Returns whether live geometry changes fast enough to require frame cadence.
    #[inline]
    #[must_use]
    pub const fn needs_frame_cadence(&self) -> bool {
        self.classic.is_active() || self.modern_needs_frame_cadence()
    }

    #[inline]
    const fn modern_needs_frame_cadence(&self) -> bool {
        !self.sparks.is_empty()
            || !self.particles.is_empty()
            || !self.fire_meteors.is_empty()
            || !self.bolts.is_empty()
            || self.ring.is_some()
            || (self.crown_until.is_some() && !self.v2.engaged())
            || !self.fading.is_empty()
            || self.v2.needs_frame_cadence()
    }

    /// Returns the deadline for the next visible change, or `None` when idle.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant, frame_interval: Duration) -> Option<Instant> {
        let v2_due = if self.v2.engaged() {
            self.v2.next_change_deadline(now)
        } else {
            None
        };
        let v1_due = if self.needs_frame_cadence() {
            Some(now + frame_interval)
        } else if let Some(t) = self.poof_hint_pending() {
            let grace_open = t + Duration::from_secs_f32(Self::POOF_FALLBACK_GRACE);
            let poll = now + Self::KILL_HINT_POLL_INTERVAL;
            Some(if grace_open > now {
                grace_open.min(poll)
            } else {
                poll
            })
        } else if !self.vapor.is_empty() || self.ember_live() {
            Some(now + Self::EMBER_POLL_INTERVAL)
        } else {
            None
        };
        let park_due = self.held_park.filter(|_| self.v2.engaged()).map(|p| {
            let park_expiry = p.at + p.patience() + rk::ARM_MIN;
            let credit_expiry = p.cross_row.then(|| {
                self.type_press_ring
                    .newest_unpaid(now)
                    .map_or(now, |at| at + Duration::from_secs_f32(IN_FLIGHT_PATIENCE_S))
                    + rk::ARM_MIN
            });
            park_expiry
                .min(credit_expiry.unwrap_or(park_expiry))
                .max(now + rk::ARM_MIN)
        });
        let torn_due = self.torn_lay.filter(|_| self.v2.engaged()).map(|l| {
            (l.moved_at + Duration::from_secs_f32(Self::TORN_REST_S) + rk::ARM_MIN)
                .max(now + rk::ARM_MIN)
        });
        [v1_due, v2_due, park_due, torn_due]
            .into_iter()
            .flatten()
            .min()
    }

    #[inline]
    fn clear_transient_state(&mut self) {
        self.clear_modern_transient_state();
        self.classic.reset();
    }

    #[inline]
    fn clear_modern_transient_state(&mut self) {
        self.recent_typed_run = None;
        self.park_source_intact = None;
        self.carried_key_move = None;
        self.retire_all_movement_provenance();
        self.torn_lay = None;
        self.clear_visual_geometry();
        self.last_erase = None;
        self.kill_hint = None;
        self.kill_retreat_pending = None;
        self.kill_reported_to_v2 = None;
        self.blink_hint = None;
        self.insert.teardown();
        self.last_licensed_row = None;
        self.hand_row = None;
        self.unowned_caret = None;
        self.bs_poof_hint = None;
        self.bs_baseline = None;
        self.last_poof = None;
        self.row_cur.clear();
        self.row_prev.clear();
        self.row_shown.clear();
        self.row_cur_meta = None;
        self.row_prev_meta = None;
        self.row_shown_meta = None;
        self.clear_neighbor_rows();
    }

    #[inline]
    fn clear_visual_geometry(&mut self) {
        self.fading.clear();
        self.ramp_in_at = None;
        self.sparks.clear();
        self.particles.clear();
        self.fire_meteors.clear();
        self.vapor.clear();
        self.last_smoke = None;
        self.bolts.clear();
        self.ring = None;
        self.crown_until = None;
        self.halo_out.clear();
        self.patch_out.clear();
        self.under_out.clear();
        self.char_out.clear();
        self.fire_halo_out.clear();
        self.v2_beams.clear();
    }

    #[inline]
    fn settle_key_seam(&mut self, cfg: &GlowConfig, drew: bool) {
        let open = drew || cfg.audible;
        if !open {
            self.clear_sound_ledger();
        }
        self.sound_live = open;
    }

    #[inline]
    const fn unsettle(&mut self) {
        self.dark_settled = false;
    }

    /// Resets coordinate-space state and draws Rainbow Kitty's ribbon into the last caret over its curtain window.
    #[inline]
    pub fn curtain(&mut self, now: Instant) {
        self.reset_space(Some(now));
    }

    /// Hides Rainbow Kitty's next frame while the viewport shows history.
    #[inline]
    pub const fn hide_v2(&mut self) -> bool {
        if !self.v2.engaged() {
            return false;
        }
        self.v2.hide_next_frame();
        true
    }

    /// Returns whether Rainbow Kitty owns the frame.
    #[inline]
    #[must_use]
    pub const fn v2_owns_frame(&self) -> bool {
        self.v2.engaged()
    }

    /// Returns whether this engine reads the rows above and below the caret's row.
    #[inline]
    #[must_use]
    pub const fn reads_neighbor_rows(&self) -> bool {
        self.v2_owns_frame()
            || self.last_classic_mono
            || matches!(self.last_style, Some(s) if !matches!(s, GlowStyle::Classic))
    }

    /// Returns whether a one-frame hide is armed on Rainbow Kitty.
    #[inline]
    #[must_use]
    pub const fn v2_hide_armed(&self) -> bool {
        self.v2.hide_armed()
    }

    /// Drops all in-flight light and forgets the last cursor position across a coordinate-space transition.
    #[inline]
    pub fn reset(&mut self) {
        self.reset_space(None);
    }

    fn reset_space(&mut self, curtain: Option<Instant>) {
        self.unsettle();
        self.clear_transient_state();
        self.clear_sound_ledger();
        self.clear_thermals();
        self.last = None;
        self.last_visible = None;
        self.hide_bridge_shown = None;
        self.forget_anchor_rows();
        self.print_in_tick = false;
        self.wrap_paid_row = None;
        self.last_move = None;
        self.crown_window_ms = Self::CROWN_MS;
        self.ctx_alt = false;
        self.pane_columns = None;
        match curtain {
            Some(now) if self.v2.engaged() => self.v2.curtain(now),
            _ => self.v2.set_engaged(false),
        }
        self.v2_caret = CaretSeam::default();
        self.v2_stop = None;
        self.flow_open = false;
        self.scroll_rows = 0;
        self.witness_rows_n = 0;
        self.witness_asked.take();
    }

    /// Advance one frame: observe the cursor at `cur` (`Some(row, col)` visible, `None` hidden), spawn on a move, decay, and emit the current light quads into `out`.
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
    ) -> u64 {
        let print_seq = self.print_anchor.map(|(_, _, seq)| seq);
        self.print_in_tick = print_seq.is_some() && print_seq != self.print_anchor_tick_seq;
        self.print_anchor_tick_seq = print_seq;
        self.last_ch = geom.ch.min(u16::MAX as usize) as u16;
        self.last_origin_y = geom.origin_y;
        out.clear();
        self.halo_out.clear();
        self.patch_out.clear();
        self.under_out.clear();
        self.char_out.clear();
        self.fire_halo_out.clear();
        self.ink_ledger.close();
        if self.frame_text.at != Some(now) {
            self.frame_text.rows.clear();
            self.frame_text.pairs.clear();
        }
        self.momentum_pulse = None;
        let valid_canvas =
            cfg.enabled && geom.cw != 0 && geom.ch != 0 && geom.rows != 0 && geom.cols != 0;
        let v2_active =
            valid_canvas && matches!(cfg.style, GlowStyle::RainbowKitty) && cfg.intensity > 0.0;
        let park_source = self.last.filter(|&(row, col)| {
            cur.is_some_and(|(next, _)| next != row) && self.park_source_unchanged(row, col)
        });
        self.carried_key_move = if v2_active {
            park_source
                .zip(cur)
                .filter(|&(from, to)| self.key_pushed_text_down(from, Some(to), now))
        } else {
            None
        };
        self.park_source_intact = park_source.filter(|_| self.carried_key_move.is_none());
        self.park_source_confirmed = false;
        let scroll_rows = std::mem::take(&mut self.scroll_rows);
        let scroll_frame = scroll_rows > 0;
        if let Some(p) = self.held_park.filter(|p| p.cross_row)
            && let Some(sample) = self.witness_rows[..self.witness_rows_n]
                .iter()
                .find(|s| s.row == p.row)
        {
            let mut exact = true;
            let mut replaced = false;
            for col in 0..p.origin {
                let old = rk::witness::unit_at(&self.park_source_cells, col);
                let current = rk::witness::unit_at(&sample.cols, col);
                if old != current {
                    exact = false;
                    replaced |= !current.is_blank();
                    if replaced {
                        break;
                    }
                }
            }
            self.park_source_confirmed = exact;
            if replaced {
                self.flush_held_park();
                self.type_hint.clear();
                self.forget_typed_credits(now);
            }
        }
        let witness_n = std::mem::take(&mut self.witness_rows_n);
        let (asked, asked_n) = self.witness_asked.take();
        self.v2.set_engaged(v2_active);
        if !self.v2.engaged() {
            self.v2_stop = None;
        }
        if !valid_canvas {
            // Full zero includes any in-flight crossfade. The master switch (and
            if cfg.enabled {
                self.store_key_timbre(cfg);
                self.settle_key_seam(cfg, false);
            } else {
                self.sound_live = false;
            }
            if !self.dark_settled {
                self.clear_transient_state();
                if !self.sound_live {
                    self.clear_sound_ledger();
                }
                self.clear_thermals();
            }
            self.dark_settled = !self.sound_live;
            self.last = cur;
            self.last_visible = cur.map(|c| (c, now));
            self.hide_bridge_shown = None;
            return 0;
        }
        self.dark_settled = false;

        if matches!(cfg.style, GlowStyle::Classic) {
            self.last_classic_mono = cfg.classic_mono;
            self.kill_hint = None;
            self.kill_retreat_pending = None;
            self.kill_reported_to_v2 = None;
            self.bs_poof_hint = None;
            self.bs_baseline = None;
            if self.modern_needs_frame_cadence() || !self.vapor.is_empty() || self.ember_live() {
                self.clear_modern_transient_state();
                self.clear_thermals();
            }
            self.last = cur;
            self.last_visible = cur.map(|c| (c, now));
            self.hide_bridge_shown = None;
            self.store_key_timbre(cfg);
            self.settle_key_seam(cfg, cfg.intensity > 0.0);
            self.rotate_row_probe();
            self.begin_ink_ledger(cfg, geom, cur);
            let spans = self.probed_text_spans();
            let fp = self.classic.tick(
                cur,
                now,
                cfg,
                geom,
                out,
                &mut self.halo_out,
                &mut self.under_out,
                crate::classic_wake::WakeInk {
                    text: TextView::new(spans, Some(&self.frame_text)),
                    ledger: &mut self.ink_ledger,
                },
            );
            if let Some((from, to)) = self.classic.take_spawn() {
                self.spawns += 1;
                self.log_licensed_as(now, from, to, AdmissionRecord::LICENCE_MOTION);
            }
            return fp;
        }
        if self.classic.is_active() || self.classic.has_last() {
            self.classic.reset();
        }

        if self.rng == 0 {
            self.rng = 0x9E37_79B9;
        }

        if self.last_style.is_some_and(|s| s != cfg.style) {
            self.begin_style_fade(now);
        }
        self.last_style = Some(cfg.style);
        self.last_classic_mono = false;
        if self.last_cfg.as_ref() != Some(cfg) {
            self.last_cfg = Some(*cfg);
        }
        let ramped_cfg;
        let cfg = {
            let scale = self.ramp_in_scale(now);
            if scale < 1.0 {
                ramped_cfg = GlowConfig {
                    classic_mono: false,
                    intensity: cfg.intensity * scale,
                    ..*cfg
                };
                &ramped_cfg
            } else {
                cfg
            }
        };

        if let Some(t0) = self.heat_at {
            let elapsed = now.saturating_duration_since(t0);
            let dt = elapsed.as_secs_f32();
            if dt > 0.0 {
                // Fire cools on its own slower τ; other styles use 0.9 s. Shared
                if self.heat > 0.0 {
                    let heat_tau = Self::heat_tau(cfg);
                    self.heat *= (-dt / heat_tau).exp();
                    if self.heat < 0.005 {
                        self.heat = 0.0;
                    }
                }
                if self.flare > 0.0 {
                    self.flare *= (-dt / Self::FLARE_DECAY_TAU).exp();
                    if self.flare < 0.005 {
                        self.flare = 0.0;
                    }
                }
                if self.coal > 0.0 {
                    self.coal *= (-dt / Self::COAL_TAU).exp();
                    if self.coal < 0.005 {
                        self.coal = 0.0;
                    }
                }
                if self.quench > 0.0 {
                    self.quench *= (-dt / Self::QUENCH_TAU).exp();
                    if self.quench < 0.005 {
                        self.quench = 0.0;
                    }
                }
                let target = self
                    .heat
                    .max(self.flare)
                    .max(self.coal * Self::COAL_FLOOR)
                    .clamp(0.0, 1.0)
                    * (1.0 - Self::QUENCH_DAMP * self.quench);
                let rise = target - self.disp_t;
                if rise != 0.0 {
                    let ease = if rise > 0.5 {
                        Self::DISP_SLAM_S
                    } else if rise > 0.0 {
                        Self::DISP_ATTACK_S
                    } else {
                        Self::DISP_RELEASE_TAU / (1.0 + 2.0 * self.quench)
                    };
                    self.disp_t += rise * (1.0 - (-dt / ease).exp());
                    if self.disp_t < 0.005 {
                        self.disp_t = 0.0;
                    }
                }
                self.flame_phase +=
                    dt * (Self::FLAME_PHASE_SPEED + Self::FLAME_PHASE_HEAT_SPEED * self.disp_t);
                if self.flame_phase >= Self::FLAME_PHASE_RING {
                    self.flame_phase -= Self::FLAME_PHASE_RING;
                }
                if matches!(cfg.style, GlowStyle::Fire) {
                    let diff = self.disp_t - self.cursor_temp;
                    if diff != 0.0 {
                        let temp_tau = if diff > 0.0 {
                            Self::TEMP_ATTACK_TAU
                        } else {
                            Self::TEMP_RELEASE_TAU / (1.0 + self.quench)
                        };
                        self.cursor_temp += diff * (1.0 - (-dt / temp_tau).exp());
                        if self.cursor_temp < 0.005 {
                            self.cursor_temp = 0.0;
                        }
                    }
                } else {
                    self.cursor_temp = 0.0;
                }
            }
        }
        self.heat_at = Some(now);
        self.refresh_forge_band(cfg);
        self.store_key_timbre(cfg);

        if cfg.intensity <= 0.0 {
            self.settle_key_seam(cfg, false);
            self.clear_transient_state();
            self.track_shown_caret(cur, now);
            return 0;
        }
        self.settle_key_seam(cfg, true);

        self.expire_seam_holds(now);
        if let (Some(hand), Some((caret_row, _))) = (self.hand_row.as_mut(), cur) {
            hand.settle(caret_row);
        }
        let key_in_flight = self.key_in_flight(now);
        let shown = self
            .last
            .or_else(|| self.last_visible.map(|(cell, _)| cell));
        let caret_carry = match (shown, cur) {
            (Some((pr, _)), Some((cr, _))) => {
                i16::try_from(i32::from(cr) - i32::from(pr)).unwrap_or(0)
            }
            _ => 0,
        };
        let carried = caret_carry.unsigned_abs() > 2
            && (!scroll_frame || i32::from(caret_carry) != i32::from(scroll_rows));
        if witness_n > 0 {
            self.v2.carry_caret(caret_carry);
        }
        let mut lifted = false;
        if self.v2.engaged()
            && witness_n > 0
            && let (Some((pr, _)), Some((cr, _))) = (shown, cur)
            && (key_in_flight || scroll_frame || carried)
            && if carried {
                true
            } else if scroll_frame {
                cr.checked_sub(pr) == Some(scroll_rows)
            } else {
                pr.abs_diff(cr) == 1
            }
        {
            let (samples, n) = witness_samples(&self.witness_rows, witness_n);
            if let Some(run) = self.v2.hand_line_follow(&samples[..n], Some(cr))
                && run.row == pr
                && pr.checked_add_signed(run.dr) == Some(cr)
            {
                self.lift_hand_row(pr, cr);
                self.v2.lift_hand(run);
                lifted = true;
            }
        }
        if !lifted
            && self.v2.engaged()
            && let (Some((pr, pc)), Some((cr, cc))) = (shown, cur)
            && scroll_frame
            && cr.checked_sub(pr) == Some(scroll_rows)
            && let Some(ins) = self.insert.fresh(now)
            && ins.known
            && ins.row == Some(pr)
            && cc.checked_sub(pc) == Some(ins.cells)
        {
            self.lift_hand_row(pr, cr);
            self.v2.lift_caret(pr, cr);
        }
        if !key_in_flight
            && let (Some((pr, pc)), Some((cr, cc))) = (shown, cur)
            && pc == cc
            && pr != cr
            && self
                .row_cur_meta
                .is_some_and(|m| m.row == cr && m.at == now)
            && let Some(run) = self.recent_typed_run.as_mut()
            && run.row == pr
            && run.end() == cc
            && (0..usize::from(run.len)).all(|i| {
                self.row_cur
                    .get(usize::from(run.col0) + i)
                    .copied()
                    .unwrap_or(' ')
                    == run.glyphs[i]
            })
        {
            run.row = cr;
            if let Some((_, Some((row, col)))) = &mut self.v2_stop
                && (*row, *col) == (pr, pc)
            {
                *row = cr;
            }
        }
        if cur.is_none()
            && let Some(meta) = self
                .row_prev_meta
                .filter(|m| self.last == Some((m.row, m.caret)))
        {
            self.row_shown.clone_from(&self.row_prev);
            self.row_shown_meta = Some(meta);
        }
        let spawn_from = self
            .last
            .or_else(|| self.hidden_bridge_source(cur, now, cfg, geom));
        self.judge_observed_caret(spawn_from, cur, now, cfg, geom);
        self.carried_key_move = None;
        let cursor_move_observed = spawn_from
            .zip(cur)
            .is_some_and(|((pr, pc), (cr, cc))| pr != cr || pc != cc);
        if cur.is_some() && cur != self.last_visible.map(|(cell, _)| cell) {
            self.wrap_paid_row = None;
        }
        let content_echo = self.same_caret_typed_echo(cur, cursor_move_observed, now, cfg, geom);
        self.echo_anchor_pass(cur, cursor_move_observed || content_echo, now, cfg, geom);
        self.track_shown_caret(cur, now);
        self.settle_torn_lay(cur, now);

        self.poof_scan(now, cfg, geom);

        self.sparks.retain(|s| Self::spark_resident_at(now, s));
        let spark_cap = Self::MAX_SPARKS;
        if self.sparks.len() > spark_cap {
            let drop = self.sparks.len() - spark_cap;
            self.sparks.drain(0..drop);
        }
        self.particles
            .retain(|p| now.saturating_duration_since(p.born).as_secs_f32() < p.life);
        self.fire_meteors
            .retain(|m| now.saturating_duration_since(m.born).as_secs_f32() < m.life);
        self.vapor
            .retain(|v| now.saturating_duration_since(v.born).as_secs_f32() < v.life);
        if matches!(cfg.style, GlowStyle::Fire)
            && self.cursor_temp > Self::SMOKE_TEMP
            && let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
        {
            let interval = 0.28 - 0.18 * self.cursor_temp;
            let due = self
                .last_smoke
                .is_none_or(|t| now.saturating_duration_since(t).as_secs_f32() >= interval);
            if due && self.vapor.len() < Self::MAX_VAPOR {
                let (r0, r1, r2) = (self.frand(), self.frand(), self.frand());
                let cell = geom.ch as f32;
                self.vapor.push(Vapor {
                    x0: geom.origin_x as f32 + (cc as f32 + 0.3 + 0.4 * r0) * geom.cw as f32,
                    y0: geom.origin_y as f32 + cr as f32 * cell,
                    vx: (r1 - 0.5) * 0.25 * cell,
                    vy: -(0.28 + 0.22 * r2) * cell,
                    gy: -0.10 * cell,
                    life: 1.2 + r0 * 1.0,
                    seed: r1,
                    kind: VaporKind::Smoke,
                    born: now,
                });
                self.last_smoke = Some(now);
            }
        }
        let mut struck: Option<Meteor> = None;
        for m in &mut self.fire_meteors {
            if !m.arrived
                && now.saturating_duration_since(m.born).as_secs_f32()
                    >= m.life * Self::METEOR_STRIKE_FRAC
            {
                m.arrived = true;
                struck = Some(*m);
            }
        }
        if let Some(m) = struck {
            self.flare = 1.0;
            if cfg.ring {
                let scale = crate::rainbow_kitty::timing::impact(
                    (m.x1 - m.x0).abs() / geom.cw.max(1) as f32,
                );
                self.ring = Some(Ring {
                    cx: m.x1,
                    cy: m.y1,
                    born: now,
                    life: classic_ring_life(0.18, scale),
                    scale,
                    caret: None,
                });
            }
            if matches!(cfg.style, GlowStyle::Fire) {
                self.meteor_strike_fountain(&m, now, geom);
            }
        }
        self.bolts
            .retain(|b| now.saturating_duration_since(b.born).as_secs_f32() < b.life);
        if let Some(r) = self.ring
            && now.saturating_duration_since(r.born).as_secs_f32() >= r.life
        {
            self.ring = None;
        }
        if self.crown_until.is_some_and(|t| now >= t) {
            self.crown_until = None;
        }

        let glyph_key = (cfg.theme_fg, cfg.theme_bg);
        if self.glyph_palette.as_ref().map(|&(k, _)| k) != Some(glyph_key) {
            self.glyph_palette = Some((
                glyph_key,
                fresh_ink_glyph_palette(cfg.theme_fg, cfg.theme_bg),
            ));
        }
        let mut halos = std::mem::take(&mut self.halo_out);
        let mut patches = std::mem::take(&mut self.patch_out);
        let mut under = std::mem::take(&mut self.under_out);
        let mut charred = std::mem::take(&mut self.char_out);
        let mut halo_cells = std::mem::take(&mut self.fire_halo_out);
        let mut engulf = std::mem::take(&mut self.engulf_scratch);
        let mut bolt_verts = std::mem::take(&mut self.bolt_verts);
        let mut bolt_channel = std::mem::take(&mut self.bolt_channel);
        let bolt_out = if cfg.dark_theme {
            &mut *out
        } else {
            &mut under
        };
        self.emit_bolts(now, cfg, geom, bolt_out, &mut bolt_verts, &mut bolt_channel);
        self.bolt_channel = bolt_channel;
        self.begin_ink_ledger(cfg, geom, cur);
        self.emit_comet(now, cfg, geom, cur, out, &mut halos, &mut under);
        out.truncate(Self::MAX_QUADS);
        let mut ledger = std::mem::take(&mut self.ink_ledger);
        self.emit_flames(
            now,
            cfg,
            geom,
            &mut patches,
            &mut charred,
            &mut halo_cells,
            &mut engulf,
            &mut under,
            &mut ledger,
        );
        self.ink_ledger = ledger;
        if cfg.dark_theme {
            if out.len() < Self::MAX_QUADS {
                self.emit_fire_meteors(now, cfg, geom, out);
            }
        } else if under.len() < Self::MAX_QUADS {
            self.emit_fire_meteors(now, cfg, geom, &mut under);
        }
        if under.len() < Self::MAX_QUADS {
            self.emit_water(now, cfg, geom, cur, &mut under);
        }
        if self.v2.engaged() {
            Self::reserve_v2_scratch(&mut under, out, &mut halos);
            if self.v2_beams.capacity() < Self::V2_BEAM_SCRATCH {
                self.v2_beams.reserve(Self::V2_BEAM_SCRATCH);
            }
            if self.v2_cues.capacity() < Self::V2_CUE_SCRATCH {
                self.v2_cues.reserve(Self::V2_CUE_SCRATCH);
            }
            let v2_cfg = rk::Config::from_glow(cfg, self.reduced_motion);
            if witness_n > 0 {
                let (samples, n) = witness_samples(&self.witness_rows, witness_n);
                self.v2.carry_caret(caret_carry);
                let park = self.held_park.filter(|p| !p.cross_row && p.fresh(now));
                let pair = park.map(|p| ((p.row, p.origin), (p.landing_row, p.landing)));
                let (_, proved) = self.v2.follow_rows_with_park(
                    &samples[..n],
                    &asked[..asked_n],
                    now,
                    pair,
                    key_in_flight && cur.is_none(),
                );
                if proved && let Some(p) = self.held_park.as_mut() {
                    p.content_followed = true;
                }
                if let Some((from, to)) = self.v2.take_hand_lift() {
                    self.lift_hand_row(from, to);
                }
            }
            let mut frame = rk::Frame {
                under: &mut under,
                out: &mut *out,
                halos: &mut halos,
                beams: &mut self.v2_beams,
                cues: &mut self.v2_cues,
                caret: CaretSeam::default(),
                companion: None,
                fp: 0,
            };
            if witness_n > 0 && !scroll_frame {
                let (samples, n) = witness_samples(&self.witness_rows, witness_n);
                self.v2
                    .tick_with_restored_rows(now, geom, &v2_cfg, &mut frame, &samples[..n]);
            } else {
                self.v2.tick(now, geom, &v2_cfg, &mut frame);
            }
            self.v2_caret = frame.caret;
            self.read_fresh_run_origin(now);
            for cue in self.v2_cues.drain(..) {
                if self.sound_cues.len() < Self::MAX_SOUND_CUES {
                    self.sound_cues.push(cue);
                }
            }
            let owns_light = cur.is_some_and(|(r, c)| self.v2.field_at(r, c).is_some());
            if owns_light || self.v2_stop.is_none_or(|(_, at)| at != cur) {
                self.v2_stop = Some((self.v2_caret.field_t, cur));
            }
            if witness_n > 0 && !scroll_frame {
                let (samples, n) = witness_samples(&self.witness_rows, witness_n);
                self.v2.witness_rows(&samples[..n], now);
            }
        } else {
            self.emit_crown(now, cfg, geom, cur, &mut halos, &mut under);
        }
        if out.len() < Self::MAX_QUADS {
            self.emit_forge_cursor(cfg, geom, cur, out, &mut halos);
        }
        let flying = !matches!(cfg.style, GlowStyle::Fire);
        if out.len() < Self::MAX_QUADS {
            let (q0, h0) = (out.len(), halos.len());
            if let Some(ring) = &mut self.ring
                && let Some(at) =
                    cur.filter(|&(r, c)| usize::from(r) < geom.rows && usize::from(c) < geom.cols)
            {
                ring.caret = Some(at);
            }
            self.emit_ring(now, cfg, geom, cur, out, &mut halos);
            if flying {
                let to = out.len();
                self.ink_ledger
                    .admit_masked(out, q0..to, InkLedger::FLYING_FLOOR);
                self.ink_ledger
                    .admit_halos_masked(&mut halos, h0, InkLedger::FLYING_FLOOR);
            }
        }
        if out.len() < Self::MAX_QUADS {
            let (q0, h0) = (out.len(), halos.len());
            self.emit_particles(now, cfg, geom, out, &mut halos, &mut under);
            if flying {
                self.ink_ledger
                    .admit_each_quad(&mut out[q0..], InkLedger::FLYING_FLOOR);
                self.ink_ledger
                    .admit_halos_masked(&mut halos, h0, InkLedger::FLYING_FLOOR);
            }
        }
        self.emit_vapor(now, cfg, geom, &mut halos);
        self.bolt_verts = bolt_verts;
        Self::cap_and_clear_v2_streams(&mut halos);
        self.halo_out = halos;
        self.patch_out = patches;
        self.under_out = under;
        self.char_out = charred;
        self.fire_halo_out = halo_cells;
        self.engulf_scratch = engulf;

        self.tick_fades(now, geom, out);

        if out.len() > Self::MAX_QUADS {
            out.truncate(Self::MAX_QUADS);
        }
        self.under_out.truncate(Self::MAX_QUADS);

        self.halo_out.truncate(Self::MAX_HALOS);
        let mut fp = FrameFingerprint::default();
        fp.glow_quads(FrameFingerprint::GLOW_OUT, out);
        fp.fire_patches(&self.patch_out);
        fp.glow_quads(FrameFingerprint::GLOW_UNDER, &self.under_out);
        fp.chars(&self.char_out);
        fp.fire_halos(&self.fire_halo_out);
        fp.rain_halos(&self.halo_out);

        if matches!(cfg.style, GlowStyle::Fire)
            && self.cursor_temp > Self::FORGE_MIN_TEMP
            && let Some(fill) = self.forge_fill()
        {
            fp.forge_fill(fill);
        }
        fp.finish()
    }

    const FADE_OUT_S: f32 = 0.25;
    const RAMP_IN_S: f32 = 0.12;
    const RAMP_IN_FLOOR: f32 = 0.15;
    const FADE_CAP: usize = 2;

    fn begin_style_fade(&mut self, now: Instant) {
        let level0 = self.ramp_in_scale(now);
        self.ramp_in_at = Some(now);
        self.sound_cues.clear();
        self.clear_keyed_clicks();
        let Some(mut cfg) = self.last_cfg else {
            return;
        };
        let crown_until = self.crown_until.take();
        let last_smoke = self.last_smoke.take();
        let has_crown = !matches!(cfg.style, GlowStyle::RainbowKitty)
            && self.last.is_some()
            && crown_until.is_some_and(|t| now < t);
        let has_forge = matches!(cfg.style, GlowStyle::Fire)
            && self.last.is_some()
            && (self.cursor_temp > Self::FORGE_MIN_TEMP || self.disp_t > Self::FORGE_MIN_TEMP);
        if self.sparks.is_empty()
            && self.particles.is_empty()
            && self.fire_meteors.is_empty()
            && self.vapor.is_empty()
            && self.bolts.is_empty()
            && self.ring.is_none()
            && !has_crown
            && !has_forge
        {
            self.flame_phase = 0.0;
            return;
        }
        if matches!(cfg.style, GlowStyle::RainbowKitty) {
            cfg.style = GlowStyle::Comet;
        }
        let mut ghost = Box::<CursorGlow>::default();
        ghost.sparks = std::mem::take(&mut self.sparks);
        ghost.particles = std::mem::take(&mut self.particles);
        ghost.fire_meteors = std::mem::take(&mut self.fire_meteors);
        ghost.vapor = std::mem::take(&mut self.vapor);
        ghost.bolts = std::mem::take(&mut self.bolts);
        ghost.ring = self.ring.take();
        ghost.crown_until = crown_until;
        ghost.crown_window_ms = self.crown_window_ms;
        ghost.last_move = self.last_move;
        ghost.last_smoke = last_smoke;
        ghost.reduced_motion = self.reduced_motion;
        ghost.heat = self.heat;
        ghost.heat_at = self.heat_at;
        ghost.flare = self.flare;
        ghost.coal = self.coal;
        ghost.quench = self.quench;
        ghost.disp_t = self.disp_t;
        ghost.cursor_temp = self.cursor_temp;
        ghost.flame_phase = self.flame_phase;
        ghost.momentum = self.momentum;
        ghost.hue = self.hue;
        ghost.rng = self.rng;
        ghost.last = self.last;
        ghost.last_style = Some(cfg.style);
        ghost.last_cfg = Some(cfg);
        self.flame_phase = 0.0;
        if self.fading.len() >= Self::FADE_CAP {
            self.fading.remove(0);
        }
        self.fading.push(OutgoingFade {
            anchor: self.last,
            engine: ghost,
            cfg,
            started: now,
            level0,
        });
    }

    #[inline]
    fn ramp_in_scale(&mut self, now: Instant) -> f32 {
        let Some(t0) = self.ramp_in_at else {
            return 1.0;
        };
        let u = now.saturating_duration_since(t0).as_secs_f32() / Self::RAMP_IN_S;
        if u >= 1.0 {
            self.ramp_in_at = None;
            return 1.0;
        }
        let eased = 0.5 - 0.5 * (std::f32::consts::PI * u).cos();
        Self::RAMP_IN_FLOOR + (1.0 - Self::RAMP_IN_FLOOR) * eased
    }

    fn tick_fades(&mut self, now: Instant, geom: Geom, out: &mut Vec<GlowQuad>) {
        if self.fading.is_empty() {
            return;
        }
        let mut fades = std::mem::take(&mut self.fading);
        let mut scratch = std::mem::take(&mut self.fade_scratch);
        fades.retain_mut(|f| {
            let u = now.saturating_duration_since(f.started).as_secs_f32() / Self::FADE_OUT_S;
            if u >= 1.0 {
                return false;
            }
            let env = f.level0 * (0.5 + 0.5 * (std::f32::consts::PI * u).cos());
            let mut c = f.cfg;
            c.intensity *= env;
            if c.intensity <= 0.0 {
                return false;
            }
            f.engine.tick(f.anchor, now, &c, geom, &mut scratch);
            f.engine.sound_cues.clear();
            let contributed = !(scratch.is_empty()
                && f.engine.halo_out.is_empty()
                && f.engine.patch_out.is_empty()
                && f.engine.under_out.is_empty()
                && f.engine.char_out.is_empty()
                && f.engine.fire_halo_out.is_empty());
            if contributed {
                out.extend_from_slice(&scratch);
                self.halo_out.extend_from_slice(&f.engine.halo_out);
                self.patch_out.extend_from_slice(&f.engine.patch_out);
                self.under_out.extend_from_slice(&f.engine.under_out);
                self.char_out.extend_from_slice(&f.engine.char_out);
                self.fire_halo_out
                    .extend_from_slice(&f.engine.fire_halo_out);
            }
            contributed
        });
        self.fade_scratch = scratch;
        self.fading = fades;
    }

    #[inline]
    const fn frand(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }
}

#[inline]
fn witness_samples(
    rows: &[WitnessRowBuf],
    n: usize,
) -> ([rk::witness::RowSample<'_>; CURSOR_WITNESS_ROWS], usize) {
    let mut samples = [rk::witness::RowSample {
        row: u16::MAX,
        cols: &[],
    }; CURSOR_WITNESS_ROWS];
    let n = n.min(samples.len()).min(rows.len());
    for (sample, slot) in samples.iter_mut().zip(&rows[..n]) {
        *sample = rk::witness::RowSample {
            row: slot.row,
            cols: &slot.cols,
        };
    }
    (samples, n)
}

#[cfg(test)]
mod tests;
