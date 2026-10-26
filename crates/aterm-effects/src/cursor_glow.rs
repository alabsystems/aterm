// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cursor aurora: pure host-side animation that turns cursor motion into light: a comet of
//! premultiplied additive quads along the swept path, a bloom crown around the head, and
//! per-style particles.

use aterm_time::Instant;
use std::time::Duration;

use aterm_hash::{FNV64_OFFSET, fnv_mix_u64};
use aterm_render::{
    BEAM_LAYERS, BeamClip, BeamRows, BeamScratch, BeamVertex, COMET_LAYERS, CharFg, CometSample,
    FireHaloCell, FireMode, FirePatch, GlowQuad, GlowRamps, HaloMode, LASER_LAYERS, PHASER_LAYERS,
    RainHalo, RampCost, RampSink, SPARKLE_LAYERS, comet_beam, comet_beam_over, layer_stack_lift,
    layered_beam_quads_with, premul_rgb,
};

use crate::rainbow_kitty::{self as rk, CaretSeam, CompanionImpulse};

/// Host row slots for the content witness: the bands' eight rows ([`rk::witness::WITNESS_ROWS`]),
/// a waiting key's source row and its neighbours ([`rk::witness::ARMING_ROWS`]), and the caret's
/// row when it is outside both sets.
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
mod icon_shade;
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
    AirLight, BedScratch, BedShape, air_lit, clip_quads_to, clip_stream_to, dark_bed_budget,
    dark_bed_ink, lay_dark_bed, lit_rail_drop, night_rgb, page_lit, page_lit_ink, split_air_runs,
};
pub use companion::*;
pub use custody::*;
use ink_ledger::BandLifts;
pub(crate) use ink_ledger::InkLedger;
pub use ink_ledger::OverInkTier;
pub(crate) use ink_ledger::{LETTER_GUARD_RING, LetterGuard};
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

const BOLT_MAX_PTS: usize = 17;

#[derive(Clone, Copy)]
struct BoltPts {
    buf: [(f32, f32); BOLT_MAX_PTS],
    len: u8,
}

impl Default for BoltPts {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl BoltPts {
    const EMPTY: Self = Self {
        buf: [(0.0, 0.0); BOLT_MAX_PTS],
        len: 0,
    };

    #[inline]
    const fn as_slice(&self) -> &[(f32, f32)] {
        self.buf.split_at(self.len as usize).0
    }

    #[inline]
    const fn as_mut_slice(&mut self) -> &mut [(f32, f32)] {
        let len = self.len as usize;
        self.buf.split_at_mut(len).0
    }

    #[inline]
    const fn push(&mut self, pt: (f32, f32)) {
        let i = self.len as usize;
        if i < BOLT_MAX_PTS {
            self.buf[i] = pt;
            self.len += 1;
        }
    }

    #[inline]
    const fn from_3(a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> Self {
        let mut pts = Self {
            buf: [(0.0, 0.0); BOLT_MAX_PTS],
            len: 3,
        };
        pts.buf[0] = a;
        pts.buf[1] = b;
        pts.buf[2] = c;
        pts
    }
}

impl std::ops::Deref for BoltPts {
    type Target = [(f32, f32)];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl std::ops::DerefMut for BoltPts {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl PartialEq for BoltPts {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl std::fmt::Debug for BoltPts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (**self).fmt(f)
    }
}

#[derive(Clone, Copy)]
struct Bolt {
    pts: BoltPts,
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

/// Per-window aurora animation state.
#[derive(Default)]
pub struct CursorGlow {
    classic: crate::classic_wake::ClassicWake,
    sparks: Vec<Spark>,
    admission_log: AdmissionLog,
    admission_source: Option<AdmissionSource>,
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
    /// The ramps laid among [`Self::under_out`]'s quads ([`GlowRamps`]).
    under_ramps: GlowRamps,
    /// The ramps laid among the quads of [`Self::tick`]'s `out`.
    out_ramps: GlowRamps,
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
    probed_spans: status::TextSpans,
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
    beam_scratch: BeamScratch,
    ink_ledger: InkLedger,
    /// The frame's last word on legibility ([`Self::guard_letters`]).
    letter_guard: LetterGuard,
    #[cfg(test)]
    streak_unbudgeted: bool,
    #[cfg(test)]
    bed_unsolved: bool,
    /// A test's negative control: the bed held to its budget alone, never
    /// lit at the page's luminance ([`bed::dark_bed_ink_unlit`]).
    #[cfg(any(test, debug_assertions))]
    pub(crate) bed_page_unlit: bool,
    /// A test's negative control: a lit page's bed laid at the page's
    /// luminance in the air too, no [`bed::AirLight`].
    #[cfg(any(test, debug_assertions))]
    bed_air_off: bool,
    /// A test's negative control: a sample in the air by its own cell
    /// alone, beside a letter too ([`bed::air_lit`]).
    #[cfg(any(test, debug_assertions))]
    bed_air_alone: bool,
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
    fire_column_tongues: bool,
    #[cfg(test)]
    fire_even_cores: bool,
    #[cfg(test)]
    fire_core_lift_alone: bool,
    #[cfg(test)]
    fire_filled_unheld: bool,
    #[cfg(test)]
    fire_bloom_unwarmed: bool,
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
    fire_kept_core_from_base: bool,
    /// Test-only: lay the dark flame kept in its row as one crest of the
    /// row's field along the whole line with a tongue over every letter, the
    /// negative control of its tongues in the gaps.
    #[cfg(test)]
    fire_kept_crest: bool,
    /// Test-only: cap the kept flame under a letter's baseline hard, its core
    /// whole a pixel under the band's top and its field eased in over half
    /// the band, the negative control of the cap's ease.
    #[cfg(test)]
    fire_kept_cap_hard: bool,
    /// Test-only: stand a kept tongue in each word gap between the burning
    /// letters, as it stood (looks7 M6's negative control).
    #[cfg(test)]
    fire_gap_tongues: bool,
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
    fire_light_squat: bool,
    /// Test-only: lay a paper tongue's root in the flame's 2 px slabs over a 3 px ease, the
    /// negative control of the root's feather.
    #[cfg(test)]
    fire_light_root_coarse: bool,
    #[cfg(test)]
    light_glints: Vec<(f32, f32)>,
    fading: Vec<OutgoingFade>,
    fade_scratch: Vec<GlowQuad>,
    ramp_in_at: Option<Instant>,
    last_cfg: Option<GlowConfig>,
    dark_settled: bool,
    v2: rk::Engine,
    v2_beams: Vec<BeamVertex>,
    witness_rows: Vec<WitnessRowBuf>,
    witness_rows_n: usize,
    scroll_rows: u16,
    witness_asked: std::cell::Cell<([u16; RIBBON_LIST_ROWS], usize)>,
    v2_caret: CaretSeam,
    v2_stop: Option<(f32, Option<(u16, u16)>)>,
    flow_open: bool,
    v2_cues: Vec<SoundCue>,
    /// Which of the frame's colours the user chose ([`Self::set_colour_pins`]).
    colour_pins: ColourPins,
    /// The word icons the last word pass drew, grid pixels
    /// ([`Self::note_word_icons`]).
    word_icons: Vec<(i32, i32, i32, i32)>,
    /// The last word pass's icons still owe the published streams their
    /// shade ([`Self::shade_word_icons`]).
    word_icons_unshaded: bool,
    /// The pet's drawn body box this frame, window pixels `(x0, x1, y0, y1)`
    /// ([`Self::note_pet_body`]).
    pet_body: Option<(i32, i32, i32, i32)>,
    /// A test's stand-in pet's body box ([`Self::pin_pet_body`]).
    #[cfg(any(test, debug_assertions))]
    pet_body_pinned: Option<(i32, i32, i32, i32)>,
    /// A test's negative control: no page veil under the pet's letters.
    #[cfg(any(test, debug_assertions))]
    pet_veil_off: bool,
    /// A test's cost for the beams' runs in place of the measured one
    /// ([`Self::set_glow_ramps`], [`Self::set_ramp_cost`]).
    #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
    rows_cost: Option<aterm_render::RampCost>,
    /// A test's read: the ramps folded into the quads at the tick's end
    /// ([`Self::fold_glow_ramps`]; unset, this crate's unit tests fold).
    #[cfg(any(test, debug_assertions))]
    fold_rows: Option<bool>,
    /// The caret and geometry the frame's step drew by, which the pet's veil,
    /// drawn after the pet's tick, lays its runs by ([`Self::veil_pet_letters`]).
    veil_frame: Option<(Option<(u16, u16)>, Geom)>,
    /// The veil the last [`Self::veil_pet_letters`] laid, which a host that
    /// projects the trail's streams again without a pet tick (a split
    /// pane's cursor-only frame) lays again ([`Self::pet_veil`]).
    pet_veil: Vec<GlowQuad>,
    /// A test's negative control: the veil over the body's box, not its silhouette.
    #[cfg(any(test, debug_assertions))]
    pet_veil_boxed: bool,
    /// A test's negative control: the veil laid over every light, not
    /// beneath them ([`Self::set_pet_veil_beneath`]).
    #[cfg(any(test, debug_assertions))]
    pet_veil_over: bool,
    /// A test's negative control: the veil cut at its row's edges, with no
    /// rise ([`Self::set_pet_veil_risen`]).
    #[cfg(any(test, debug_assertions))]
    pet_veil_sliced: bool,
    /// A test's negative control: the rise feathered its whole height
    /// however shallow the coat's cut ([`Self::set_pet_veil_feather_deep`]).
    #[cfg(any(test, debug_assertions))]
    pet_veil_deep: bool,
    /// A test's negative control: a word gap's bed keeps its own knot's ink
    /// ([`bed::bridge_word_gaps`]).
    #[cfg(any(test, debug_assertions))]
    bed_gaps_unbridged: bool,
    /// This frame's keep-out boxes, window pixels ([`Self::keep_out_boxes`]).
    keep_out_scratch: Vec<(i32, i32, i32, i32)>,
    /// This frame's lettered rows' spans, window pixels ([`Self::keep_out_boxes`]).
    keep_rows_scratch: Vec<(i32, i32, i32, i32)>,
    /// The grid origin and cell width the last keep-out was laid by, for the
    /// word icons' shade ([`Self::shade_word_icons`]).
    keep_geom: Option<(i32, i32, i32)>,
    /// The word icons' sprite sources, one per [`Self::word_icons`] entry
    /// ([`Self::note_word_icon_sprites`]; empty: each icon is its box).
    word_icon_src: Vec<icon_shade::IconSrc>,
    /// The word icons' shade's working buffers: one icon's drawn rows in
    /// window pixels, the cut quads, ramps and halos, and one light's pieces.
    icon_scratch: Vec<(i32, i32, i32, i32)>,
    icon_quads: Vec<GlowQuad>,
    icon_ramps: aterm_render::GlowRamps,
    icon_halos: Vec<RainHalo>,
    icon_patches: Vec<FirePatch>,
    icon_cut: Vec<(i32, i32, i32, i32)>,
}

/// Which of a frame's glow colours the user chose: a chosen colour is drawn
/// as itself, never re-hued or brightened ([`CursorGlow::set_colour_pins`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColourPins {
    /// `cursor_trail_color` is set (a colour, or the cursor's).
    pub trail: bool,
    /// `cursor_trail_accent` is set.
    pub accent: bool,
}

impl Default for ColourPins {
    #[inline]
    fn default() -> Self {
        Self::NONE
    }
}

impl ColourPins {
    /// Neither colour is pinned.
    pub const NONE: Self = Self {
        trail: false,
        accent: false,
    };
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

struct FrameFingerprint(Option<u64>);

impl Default for FrameFingerprint {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl FrameFingerprint {
    const EMPTY: Self = Self(None);
    const GLOW_OUT: u64 = 0x676c_6f77_5f6f_7574;
    const FIRE_PATCH: u64 = 0x6669_7265_7061_7463;
    const GLOW_UNDER: u64 = 0x676c_6f77_756e_6472;
    const CHAR_FG: u64 = 0x6368_6172_5f66_6700;
    const FIRE_HALO: u64 = 0x6669_7265_6861_6c6f;
    const RAIN_HALO: u64 = 0x7261_696e_6861_6c6f;
    const FORGE_FILL: u64 = 0x666f_7267_655f_6669;
    /// Marks a stream's ramps apart from its quads.
    const RAMPS: u64 = 0x7261_6d70_0000_0000;

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
            let quad_words = quads[qi].damage_words();
            let words = quad_words.as_slice();
            let mut wi = 0;
            while wi < words.len() {
                hash = fnv_mix_u64(hash, words[wi]);
                wi += 1;
            }
            qi += 1;
        }
        self.0 = Some(hash);
    }

    /// A stream's ramps ([`GlowRamps`]): each one's band, box, run and
    /// place ([`aterm_render::GlowRamp::damage_words`]), then the ends it keeps.
    #[inline]
    fn glow_ramps(&mut self, tag: u64, ramps: &GlowRamps) {
        if ramps.is_empty() {
            return;
        }
        self.begin_stream(tag ^ Self::RAMPS, ramps.items.len());
        let mut hash = match self.0 {
            Some(h) => h,
            None => FNV64_OFFSET,
        };
        let items = ramps.items.as_slice();
        let ends = ramps.ends.as_slice();
        let mut ri = 0;
        while ri < items.len() {
            let r = &items[ri];
            let words = r.damage_words();
            let mut wi = 0;
            while wi < words.len() {
                hash = fnv_mix_u64(hash, words[wi]);
                wi += 1;
            }
            let chunks = r.kept_ends(ends).as_chunks::<2>().0;
            let mut ci = 0;
            while ci < chunks.len() {
                let e = &chunks[ci];
                hash = fnv_mix_u64(hash, (e[0] as u64) | ((e[1] as u64) << 32));
                ci += 1;
            }
            ri += 1;
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
    /// The steepest a Fire meteor's streak leans off its row: a leap to another row shallower
    /// than this is drawn this steep, so its streak reaches back this many rows' width at most.
    const METEOR_SLOPE: f32 = 1.6;
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
    /// A tongue in the air feathers in over this share of a cell from its root; at a fifth, every
    /// tongue's hottest light would stand on one ruler-straight floor along the run.
    const FIRE_DARK_AIR_FOOT: f32 = 0.45;
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
    /// How tall a tongue in a lettered row's gap stands against the row's kept rise: its tip
    /// fades to nothing, so it stands past the row's top to show up to the letters' cap height.
    const FIRE_DARK_GAP_RISE: f32 = 1.6;
    /// The least a tongue in a gap inside the burning run stands, as a share of the square root
    /// of the head's strength; held to the run's smoothed heat alone, a gap in a cool run would
    /// stand a candle flame.
    const FIRE_DARK_GAP_FLOOR: f32 = 0.8;
    const FIRE_DARK_CORE_ROOT_PX: f32 = 5.0;
    const FIRE_DARK_KEPT_CORE_COV: f32 = 255.0;
    const FIRE_DARK_KEPT_CORE_ROOT_PX: f32 = 4.0;
    const FIRE_DARK_CORE_CLEAR_PX: f32 = 1.0;
    /// Under a kept letter's baseline, the pixels over which the flame's light eases in from the
    /// letter's hold to the air's; a two-pixel ease steps the light there in a hot bead line.
    const FIRE_DARK_CAP_EASE_PX: f32 = 4.0;
    /// How far a tongue's foot curls up at its side, a share of a cell: its
    /// root rounds, so the run stands on its tongues' own feet.
    const FIRE_DARK_FOOT_CURL: f32 = 0.40;
    /// Where the dark caret bloom's rim and heart sit on [`fire_ramp`]:
    /// an ember's orange-red, an orange.
    const FIRE_BLOOM_EMBER: f32 = 0.46;
    const FIRE_BLOOM_HEART: f32 = 0.52;
    /// A core in the air burns in its tongue's lower body, as tall as this share of the tongue: a
    /// teardrop [`Self::FIRE_DARK_AIR_CORE_W`] of its tongue's half-width wide that narrows to a
    /// point by the power [`Self::FIRE_DARK_AIR_CORE_TAPER`] of its rise. As tall and sharp as a
    /// kept core, every core in the air would stand a thin olive needle past its flame.
    const FIRE_DARK_AIR_CORE_REACH: f32 = 0.45;
    const FIRE_DARK_AIR_CORE_W: f32 = 0.80;
    /// Its heart's light: inside its flame, a core at the column control's
    /// 190 ([`Self::FIRE_DARK_CORE_COV`]) added to the flame's orange burns
    /// no yellow.
    const FIRE_DARK_AIR_CORE_COV: f32 = 255.0;
    const FIRE_DARK_AIR_CORE_TAPER: f32 = 1.1;
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
    /// The highest a tongue in the air stands its root (and its gold core's) off the air's floor,
    /// as a share of a row. The whole tongue rises: on one floor the gold roots stand in a row of
    /// evenly spaced beads, a core lifted alone hangs as a gold stick mid-flame, and cores
    /// shortened, narrowed or set off their axis burn as a tan curtain.
    const FIRE_DARK_ROOT_LIFT: f32 = 0.34;
    /// The step of a tongue's root lift from one tongue to the next along the row, a share of
    /// [`Self::FIRE_DARK_ROOT_LIFT`] taken modulo one (the golden ratio's): any two neighbours
    /// stand at least 0.38 of the lift apart, so no three neighbouring roots share a floor.
    const FIRE_DARK_ROOT_LIFT_STEP: f32 = 0.618_034;
    /// The least height a paper flame tongue keeps; shorter ones are dropped.
    const FIRE_LIGHT_TONGUE_MIN_PX: f32 = 3.0;
    /// The height over which a short paper tongue fades in past
    /// [`Self::FIRE_LIGHT_TONGUE_MIN_PX`].
    const FIRE_LIGHT_TONGUE_EASE_PX: f32 = 5.0;
    /// A paper tongue's widest half-width per pixel of its height (no tongue wider than tall).
    const FIRE_LIGHT_TONGUE_ASPECT: f32 = 0.45;
    /// The least run over which a paper tongue's root eases in, laid a pixel a slab; over 3 px in
    /// the flame's 2 px slabs, a short gap tongue's root is cut flat in two steps.
    const FIRE_LIGHT_ROOT_EASE_PX: f32 = 6.0;
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
    /// The cells a light Comet's typing wedge reaches back behind its head.
    const COMET_LIGHT_WEDGE_CELLS: f32 = 3.5;
    /// The most Trail length grows Sparkle's dark bed under a jump: its wedge scales with Length
    /// from the default up to this many times its own.
    const SPARKLE_LEAP_WEDGE_GROWTH: f32 = 4.0;
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

    #[inline]
    pub(crate) const fn print_anchor_seq(&self) -> Option<u64> {
        match self.print_anchor {
            Some((_, _, seq)) => Some(seq),
            None => None,
        }
    }

    /// Which of the frame's colours ([`GlowConfig::color`], [`GlowConfig::accent`]) the user
    /// chose, so the lights that wear one draw it as itself ([`ColourPins`]).
    #[inline]
    pub const fn set_colour_pins(&mut self, pins: ColourPins) {
        self.colour_pins = pins;
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
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.has_live_motion()
            || self.ember_live()
            || self.kill_hint.is_some()
            || self.bs_poof_hint.is_some()
            || (self.v2.engaged()
                && (self.held_park.is_some()
                    || self.torn_lay.is_some()
                    || self.v2.fingerprint() != 0))
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
            (Some(a), None) => Some(a),
            (None, b) => b,
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
        let v1_due = if self.needs_frame_cadence() {
            Some(now + frame_interval)
        } else if let Some(t) = self.poof_hint_pending() {
            let grace_open = t + POOF_FALLBACK_GRACE_DUR;
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
        if !self.v2.engaged() {
            return v1_due;
        }
        let v2_due = self.v2.next_change_deadline(now);
        let mut earliest = match (v1_due, v2_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, b) => b,
        };
        if let Some(p) = self.held_park {
            let park_expiry = p.at + p.patience() + rk::ARM_MIN;
            let expiry = if p.cross_row {
                let credit_at = match self.type_press_ring.newest_unpaid(now) {
                    Some(at) => at + IN_FLIGHT_PATIENCE_DUR,
                    None => now,
                };
                park_expiry.min(credit_at + rk::ARM_MIN)
            } else {
                park_expiry
            };
            let d = expiry.max(now + rk::ARM_MIN);
            earliest = Some(match earliest {
                Some(e) => e.min(d),
                None => d,
            });
        }
        if let Some(l) = self.torn_lay {
            let d = (l.moved_at + TORN_REST_DUR + rk::ARM_MIN).max(now + rk::ARM_MIN);
            earliest = Some(match earliest {
                Some(e) => e.min(d),
                None => d,
            });
        }
        earliest
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
        self.under_ramps.clear();
        self.out_ramps.clear();
        self.ink_ledger.clear_rails();
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

    /// Resets coordinate-space state and draws Rainbow Kitty's ribbon into the last caret over its
    /// curtain window.
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

    /// Drops all in-flight light and forgets the last cursor position across a coordinate-space
    /// transition.
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
        self.v2_caret = CaretSeam::EMPTY;
        self.v2_stop = None;
        self.flow_open = false;
        self.scroll_rows = 0;
        self.witness_rows_n = 0;
        self.witness_asked.take();
    }

    /// Advance one frame: observe the cursor at `cur` (`Some(row, col)` visible, `None` hidden),
    /// spawn on a move, decay, and emit the current light quads into `out`.
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
    ) -> u64 {
        let print_seq = self.print_anchor_seq();
        self.print_in_tick = print_seq.is_some() && print_seq != self.print_anchor_tick_seq;
        self.print_anchor_tick_seq = print_seq;
        self.last_ch = geom.ch.min(u16::MAX as usize) as u16;
        self.last_origin_y = geom.origin_y;
        if geom.cw != 0 {
            // Every style's word icons shade by it ([`Self::shade_word_icons`]).
            self.keep_geom = Some((
                i32::from(geom.origin_x),
                i32::from(geom.origin_y),
                geom.cw as i32,
            ));
        }
        out.clear();
        self.out_ramps.clear();
        self.halo_out.clear();
        self.patch_out.clear();
        self.under_out.clear();
        self.under_ramps.clear();
        self.char_out.clear();
        self.fire_halo_out.clear();
        self.ink_ledger.close();
        self.ink_ledger.clear_rails();
        if self.frame_text.at != Some(now) {
            self.frame_text.rows.clear();
            self.frame_text.pairs.clear();
        }
        self.momentum_pulse = None;
        let valid_canvas =
            cfg.enabled && geom.cw != 0 && geom.ch != 0 && geom.rows != 0 && geom.cols != 0;
        let v2_active =
            valid_canvas && matches!(cfg.style, GlowStyle::RainbowKitty) && cfg.intensity > 0.0;
        let park_source = match (self.last, cur) {
            (Some((row, col)), Some((next, _)))
                if next != row && self.park_source_unchanged(row, col) =>
            {
                Some((row, col))
            }
            _ => None,
        };
        self.carried_key_move = match (v2_active, park_source, cur) {
            (true, Some(from), Some(to)) if self.key_pushed_text_down(from, Some(to), now) => {
                Some((from, to))
            }
            _ => None,
        };
        self.park_source_intact = if self.carried_key_move.is_none() {
            park_source
        } else {
            None
        };
        self.park_source_confirmed = false;
        let scroll_rows = std::mem::take(&mut self.scroll_rows);
        let scroll_frame = scroll_rows > 0;
        if let Some(p) = self.held_park
            && p.cross_row
        {
            let n = self.witness_rows_n.min(self.witness_rows.len());
            let mut sample = None;
            let mut i = 0;
            while i < n {
                if self.witness_rows[i].row == p.row {
                    sample = Some(&self.witness_rows[i]);
                    break;
                }
                i += 1;
            }
            if let Some(sample) = sample {
                let mut exact = true;
                let mut replaced = false;
                let mut col = 0;
                while col < p.origin {
                    let old = rk::witness::unit_at(&self.park_source_cells, col);
                    let current = rk::witness::unit_at(&sample.cols, col);
                    if old != current {
                        exact = false;
                        replaced |= !current.is_blank();
                        if replaced {
                            break;
                        }
                    }
                    col += 1;
                }
                self.park_source_confirmed = exact;
                if replaced {
                    self.flush_held_park();
                    self.type_hint.clear();
                    self.forget_typed_credits(now);
                }
            }
        }
        let witness_n = std::mem::take(&mut self.witness_rows_n);
        let (asked, asked_n) = self.witness_asked.take();
        self.v2.set_engaged(v2_active);
        if !self.v2.engaged() {
            self.v2_stop = None;
        }
        #[inline]
        const fn visible_at(
            cur: Option<(u16, u16)>,
            now: Instant,
        ) -> Option<((u16, u16), Instant)> {
            match cur {
                Some(c) => Some((c, now)),
                None => None,
            }
        }
        if !valid_canvas {
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
            self.last_visible = visible_at(cur, now);
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
            self.last_visible = visible_at(cur, now);
            self.hide_bridge_shown = None;
            self.store_key_timbre(cfg);
            self.settle_key_seam(cfg, cfg.intensity > 0.0);
            self.rotate_row_probe();
            self.begin_ink_ledger(cfg, geom, cur);
            let spans = self.probed_text_spans();
            let cost = self.rows_cost();
            let fp = self.classic.tick(
                cur,
                now,
                cfg,
                geom,
                &mut rows_sink(out, &mut self.out_ramps, cost),
                &mut self.halo_out,
                &mut rows_sink(&mut self.under_out, &mut self.under_ramps, cost),
                crate::classic_wake::WakeInk {
                    text: TextView::new(spans, Some(&self.frame_text)),
                    ledger: &mut self.ink_ledger,
                },
            );
            if let Some((from, to)) = self.classic.take_spawn() {
                self.spawns += 1;
                self.log_licensed_as(now, from, to, AdmissionRecord::LICENCE_MOTION);
            }
            self.fold_for_tests(out);
            return fp;
        }
        if self.classic.is_active() || self.classic.has_last() {
            self.classic.reset();
        }

        if self.rng == 0 {
            self.rng = 0x9E37_79B9;
        }

        if matches!(self.last_style, Some(s) if s != cfg.style) {
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
                if self.heat > 0.0 {
                    let heat_tau = Self::heat_tau(cfg);
                    self.heat *= crate::color_math::exp_f32(-dt / heat_tau);
                    if self.heat < 0.005 {
                        self.heat = 0.0;
                    }
                }
                if self.flare > 0.0 {
                    self.flare *= crate::color_math::exp_f32(-dt * (1.0 / Self::FLARE_DECAY_TAU));
                    if self.flare < 0.005 {
                        self.flare = 0.0;
                    }
                }
                if self.coal > 0.0 {
                    self.coal *= crate::color_math::exp_f32(-dt * (1.0 / Self::COAL_TAU));
                    if self.coal < 0.005 {
                        self.coal = 0.0;
                    }
                }
                if self.quench > 0.0 {
                    self.quench *= crate::color_math::exp_f32(-dt * (1.0 / Self::QUENCH_TAU));
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
                    let inv_ease = if rise > 0.5 {
                        1.0 / Self::DISP_SLAM_S
                    } else if rise > 0.0 {
                        1.0 / Self::DISP_ATTACK_S
                    } else {
                        (1.0 + 2.0 * self.quench) * (1.0 / Self::DISP_RELEASE_TAU)
                    };
                    self.disp_t += rise * (1.0 - crate::color_math::exp_f32(-dt * inv_ease));
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
                        let inv_temp_tau = if diff > 0.0 {
                            1.0 / Self::TEMP_ATTACK_TAU
                        } else {
                            (1.0 + self.quench) * (1.0 / Self::TEMP_RELEASE_TAU)
                        };
                        self.cursor_temp +=
                            diff * (1.0 - crate::color_math::exp_f32(-dt * inv_temp_tau));
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
        let shown = self.cursor_anchor();
        let caret_carry = match (shown, cur) {
            (Some((pr, _)), Some((cr, _))) => {
                let diff = i32::from(cr) - i32::from(pr);
                if diff >= i32::from(i16::MIN) && diff <= i32::from(i16::MAX) {
                    diff as i16
                } else {
                    0
                }
            }
            _ => 0,
        };
        if witness_n > 0 {
            self.v2.carry_caret(caret_carry);
        }
        let carried = caret_carry.unsigned_abs() > 2
            && (!scroll_frame || i32::from(caret_carry) != i32::from(scroll_rows));
        let mut lifted = false;
        if self.v2.engaged()
            && witness_n > 0
            && (key_in_flight || scroll_frame || carried)
            && let (Some((pr, _)), Some((cr, _))) = (shown, cur)
            && if carried {
                true
            } else if scroll_frame {
                cr.checked_sub(pr) == Some(scroll_rows)
            } else {
                pr.abs_diff(cr) <= 2
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
            && matches!(self.row_cur_meta, Some(m) if m.row == cr && m.at == now)
            && let Some(run) = self.recent_typed_run.as_mut()
            && run.row == pr
            && run.end() == cc
        {
            let col0 = usize::from(run.col0);
            let len = usize::from(run.len);
            let mut matched = true;
            let mut i = 0;
            while i < len {
                let c = match self.row_cur.get(col0 + i) {
                    Some(&ch) => ch,
                    None => ' ',
                };
                if c != run.glyphs[i] {
                    matched = false;
                    break;
                }
                i += 1;
            }
            if matched {
                run.row = cr;
                if let Some((_, Some((row, col)))) = &mut self.v2_stop
                    && (*row, *col) == (pr, pc)
                {
                    *row = cr;
                }
            }
        }
        if cur.is_none()
            && let Some(meta) = self.row_prev_meta
            && self.last == Some((meta.row, meta.caret))
        {
            self.row_shown.clone_from(&self.row_prev);
            self.row_shown_meta = Some(meta);
        }
        let spawn_from = match self.last {
            Some(cell) => Some(cell),
            None => self.hidden_bridge_source(cur, now, cfg, geom),
        };
        self.judge_observed_caret(spawn_from, cur, now, cfg, geom);
        self.carried_key_move = None;
        let cursor_move_observed =
            matches!((spawn_from, cur), (Some((pr, pc)), Some((cr, cc))) if pr != cr || pc != cc);
        if cur.is_some() && !matches!((cur, self.last_visible), (Some(c), Some((v, _))) if c == v) {
            self.wrap_paid_row = None;
        }
        let content_echo = self.same_caret_typed_echo(cur, cursor_move_observed, now, cfg, geom);
        self.echo_anchor_pass(cur, cursor_move_observed || content_echo, now, cfg, geom);
        self.track_shown_caret(cur, now);
        self.settle_torn_lay(cur, now);

        self.poof_scan(now, cfg, geom);

        if !self.sparks.is_empty() {
            self.sparks.retain(|s| Self::spark_resident_at(now, s));
            let spark_cap = Self::MAX_SPARKS;
            if self.sparks.len() > spark_cap {
                let drop = self.sparks.len() - spark_cap;
                self.sparks.drain(0..drop);
            }
        }
        if !self.particles.is_empty() {
            self.particles
                .retain(|p| now.saturating_duration_since(p.born).as_secs_f32() < p.life);
        }
        let mut struck: Option<Meteor> = None;
        if !self.fire_meteors.is_empty() {
            self.fire_meteors.retain_mut(|m| {
                let age = now.saturating_duration_since(m.born).as_secs_f32();
                if age >= m.life {
                    return false;
                }
                if !m.arrived && age >= m.life * Self::METEOR_STRIKE_FRAC {
                    m.arrived = true;
                    struck = Some(*m);
                }
                true
            });
        }
        if !self.vapor.is_empty() {
            self.vapor
                .retain(|v| now.saturating_duration_since(v.born).as_secs_f32() < v.life);
        }
        if matches!(cfg.style, GlowStyle::Fire)
            && self.cursor_temp > Self::SMOKE_TEMP
            && let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
        {
            let interval = 0.28 - 0.18 * self.cursor_temp;
            let due = match self.last_smoke {
                Some(t) => now.saturating_duration_since(t).as_secs_f32() >= interval,
                None => true,
            };
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
        if !self.bolts.is_empty() {
            self.bolts
                .retain(|b| now.saturating_duration_since(b.born).as_secs_f32() < b.life);
        }
        if let Some(r) = self.ring
            && now.saturating_duration_since(r.born).as_secs_f32() >= r.life
        {
            self.ring = None;
        }
        if matches!(self.crown_until, Some(t) if now >= t) {
            self.crown_until = None;
        }

        let glyph_key = (cfg.theme_fg, cfg.theme_bg);
        if !matches!(self.glyph_palette, Some((k, _)) if k == glyph_key) {
            self.glyph_palette = Some((
                glyph_key,
                fresh_ink_glyph_palette(cfg.theme_fg, cfg.theme_bg),
            ));
        }
        let mut halos = std::mem::take(&mut self.halo_out);
        let mut patches = std::mem::take(&mut self.patch_out);
        let mut under = std::mem::take(&mut self.under_out);
        let mut under_ramps = std::mem::take(&mut self.under_ramps);
        let mut out_ramps = std::mem::take(&mut self.out_ramps);
        let mut charred = std::mem::take(&mut self.char_out);
        let mut halo_cells = std::mem::take(&mut self.fire_halo_out);
        let mut engulf = std::mem::take(&mut self.engulf_scratch);
        let mut bolt_verts = std::mem::take(&mut self.bolt_verts);
        let mut bolt_channel = std::mem::take(&mut self.bolt_channel);
        let mut bolt_out = if cfg.dark_theme {
            self.ramp_sink(out, &mut out_ramps)
        } else {
            self.ramp_sink(&mut under, &mut under_ramps)
        };
        self.emit_bolts(
            now,
            cfg,
            geom,
            &mut bolt_out,
            &mut bolt_verts,
            &mut bolt_channel,
        );
        drop(bolt_out);
        self.bolt_channel = bolt_channel;
        self.begin_ink_ledger(cfg, geom, cur);
        let mut out_sink = self.ramp_sink(out, &mut out_ramps);
        let mut under_sink = self.ramp_sink(&mut under, &mut under_ramps);
        self.emit_comet(
            now,
            cfg,
            geom,
            cur,
            &mut out_sink,
            &mut halos,
            &mut under_sink,
        );
        out_sink.keep_laid(Self::MAX_QUADS);
        drop((out_sink, under_sink));
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
            if out_ramps.laid(out.len()) < Self::MAX_QUADS {
                let mut out_sink = self.ramp_sink(out, &mut out_ramps);
                self.emit_fire_meteors(now, cfg, geom, &mut out_sink);
            }
        } else if under_ramps.laid(under.len()) < Self::MAX_QUADS {
            let mut under_sink = self.ramp_sink(&mut under, &mut under_ramps);
            self.emit_fire_meteors(now, cfg, geom, &mut under_sink);
        }
        if under_ramps.laid(under.len()) < Self::MAX_QUADS {
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
            let witness_rows = std::mem::take(&mut self.witness_rows);
            let witness = if witness_n > 0 {
                Some(witness_samples(&witness_rows, witness_n))
            } else {
                None
            };
            let witness_slice = match &witness {
                Some((samples, n)) => &samples[..*n],
                None => &[][..],
            };
            if witness_n > 0 {
                self.v2.carry_caret(caret_carry);
                let pair = match self.held_park {
                    Some(p) if !p.cross_row && p.fresh(now) => {
                        Some(((p.row, p.origin), (p.landing_row, p.landing)))
                    }
                    _ => None,
                };
                let (_, proved) = self.v2.follow_rows_with_park(
                    witness_slice,
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
            self.keep_out_boxes(cfg, geom, cur);
            let mut frame = rk::Frame {
                under: &mut under,
                under_ramps: &mut under_ramps,
                out: &mut *out,
                out_ramps: &mut out_ramps,
                halos: &mut halos,
                beams: &mut self.v2_beams,
                cues: &mut self.v2_cues,
                caret: CaretSeam::EMPTY,
                companion: None,
                fp: 0,
                #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
                rows_cost: self.rows_cost,
            };
            if witness_n > 0 && !scroll_frame {
                self.v2
                    .tick_with_restored_rows(now, geom, &v2_cfg, &mut frame, witness_slice);
            } else {
                self.v2.tick(now, geom, &v2_cfg, &mut frame);
            }
            if let Some(p) = self.held_park.as_mut() {
                p.v2_typed_before = 0;
            }
            self.v2_caret = frame.caret;
            let rail_spans = self.v2.ribbon().rail_spans();
            let mut ri = 0;
            while ri < rail_spans.len() {
                let (from, to) = rail_spans[ri];
                self.ink_ledger.note_rail_laid(from, to);
                ri += 1;
            }
            self.read_fresh_run_origin(now);
            let mut ci = 0;
            while ci < self.v2_cues.len() {
                if self.sound_cues.len() < Self::MAX_SOUND_CUES {
                    self.sound_cues.push(self.v2_cues[ci]);
                }
                ci += 1;
            }
            self.v2_cues.clear();
            let owns_light = match cur {
                Some((r, c)) => self.v2.field_at(r, c).is_some(),
                None => false,
            };
            if owns_light || !matches!(self.v2_stop, Some((_, at)) if at == cur) {
                self.v2_stop = Some((self.v2_caret.field_t, cur));
            }
            if witness_n > 0 && !scroll_frame {
                self.v2.witness_rows(witness_slice, now);
            }
            self.witness_rows = witness_rows;
        } else {
            self.emit_crown(now, cfg, geom, cur, &mut halos, &mut under);
        }
        if out_ramps.laid(out.len()) < Self::MAX_QUADS {
            self.emit_forge_cursor(cfg, geom, cur, out, &mut halos);
        }
        let flying = !matches!(cfg.style, GlowStyle::Fire);
        if out_ramps.laid(out.len()) < Self::MAX_QUADS {
            let (q0, h0) = (out.len(), halos.len());
            if let Some(ring) = &mut self.ring
                && let Some((r, c)) = cur
                && usize::from(r) < geom.rows
                && usize::from(c) < geom.cols
            {
                ring.caret = Some((r, c));
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
        if out_ramps.laid(out.len()) < Self::MAX_QUADS {
            let (q0, h0) = (out.len(), halos.len());
            let mut under_sink = self.ramp_sink(&mut under, &mut under_ramps);
            // The room the stream's ramp lights leave its quads.
            let cap = Self::MAX_QUADS.saturating_sub(out_ramps.lit);
            self.emit_particles(now, cfg, geom, out, &mut halos, &mut under_sink, cap);
            drop(under_sink);
            if flying {
                self.ink_ledger
                    .admit_each_quad(&mut out[q0..], InkLedger::FLYING_FLOOR);
                self.ink_ledger
                    .admit_halos_masked(&mut halos, h0, InkLedger::FLYING_FLOOR);
            }
        }
        self.emit_vapor(now, cfg, geom, &mut halos);
        if matches!(cfg.style, GlowStyle::Fire) && cfg.dark_theme {
            self.hold_halos_off_filled(&mut halos, cur, geom, cfg.theme_bg);
        }
        self.bolt_verts = bolt_verts;
        Self::cap_and_clear_v2_streams(&mut halos);
        self.halo_out = halos;
        self.patch_out = patches;
        self.under_out = under;
        self.under_ramps = under_ramps;
        self.char_out = charred;
        self.fire_halo_out = halo_cells;
        self.engulf_scratch = engulf;

        self.tick_fades(now, geom, (out, &mut out_ramps));

        out_ramps.truncate_laid(out, Self::MAX_QUADS);
        self.under_ramps
            .truncate_laid(&mut self.under_out, Self::MAX_QUADS);

        self.halo_out.truncate(Self::MAX_HALOS);
        let mut fp = FrameFingerprint::EMPTY;
        fp.glow_quads(FrameFingerprint::GLOW_OUT, out);
        fp.glow_ramps(FrameFingerprint::GLOW_OUT, &out_ramps);
        fp.fire_patches(&self.patch_out);
        fp.glow_quads(FrameFingerprint::GLOW_UNDER, &self.under_out);
        fp.glow_ramps(FrameFingerprint::GLOW_UNDER, &self.under_ramps);
        self.out_ramps = out_ramps;
        self.fold_for_tests(out);
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
            && matches!(crown_until, Some(t) if now < t);
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
        let u = now.saturating_duration_since(t0).as_secs_f32() * (1.0 / Self::RAMP_IN_S);
        if u >= 1.0 {
            self.ramp_in_at = None;
            return 1.0;
        }
        let eased = 0.5 - 0.5 * crate::supernova::sin_cos_f32(std::f32::consts::PI * u).1;
        Self::RAMP_IN_FLOOR + (1.0 - Self::RAMP_IN_FLOOR) * eased
    }

    fn tick_fades(
        &mut self,
        now: Instant,
        geom: Geom,
        (out, out_ramps): (&mut Vec<GlowQuad>, &mut GlowRamps),
    ) {
        if self.fading.is_empty() {
            return;
        }
        let mut fades = std::mem::take(&mut self.fading);
        let mut scratch = std::mem::take(&mut self.fade_scratch);
        fades.retain_mut(|f| {
            let u =
                now.saturating_duration_since(f.started).as_secs_f32() * (1.0 / Self::FADE_OUT_S);
            if u >= 1.0 {
                return false;
            }
            let env =
                f.level0 * (0.5 + 0.5 * crate::supernova::sin_cos_f32(std::f32::consts::PI * u).1);
            let mut c = f.cfg;
            c.intensity *= env;
            if c.intensity <= 0.0 {
                return false;
            }
            #[cfg(any(test, debug_assertions, feature = "dev-seams"))]
            {
                f.engine.rows_cost = self.rows_cost;
            }
            f.engine.tick(f.anchor, now, &c, geom, &mut scratch);
            f.engine.sound_cues.clear();
            let contributed = !(scratch.is_empty()
                && f.engine.out_ramps.is_empty()
                && f.engine.halo_out.is_empty()
                && f.engine.patch_out.is_empty()
                && f.engine.under_out.is_empty()
                && f.engine.under_ramps.is_empty()
                && f.engine.char_out.is_empty()
                && f.engine.fire_halo_out.is_empty());
            if contributed {
                out_ramps.append_from(&f.engine.out_ramps, out.len());
                out.extend_from_slice(&scratch);
                self.halo_out.extend_from_slice(&f.engine.halo_out);
                self.patch_out.extend_from_slice(&f.engine.patch_out);
                self.under_ramps
                    .append_from(&f.engine.under_ramps, self.under_out.len());
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
        (x >> 8) as f32 * (1.0 / (1u32 << 24) as f32)
    }
}

#[inline]
const fn witness_samples(
    rows: &[WitnessRowBuf],
    n: usize,
) -> ([rk::witness::RowSample<'_>; CURSOR_WITNESS_ROWS], usize) {
    let mut samples = [rk::witness::RowSample {
        row: u16::MAX,
        cols: &[],
    }; CURSOR_WITNESS_ROWS];
    let n = if n < samples.len() { n } else { samples.len() };
    let n = if n < rows.len() { n } else { rows.len() };
    let mut i = 0;
    while i < n {
        samples[i] = rk::witness::RowSample {
            row: rows[i].row,
            cols: rows[i].cols.as_slice(),
        };
        i += 1;
    }
    (samples, n)
}

#[cfg(test)]
mod tests;
