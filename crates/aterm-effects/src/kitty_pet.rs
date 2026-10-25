// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Autonomous text-plane kitty companion ([`PetBrain`](crate::kitty_pet::PetBrain), [`PetFrame`](crate::kitty_pet::PetFrame)).

use core::f32::consts::TAU;
use core::time::Duration;

use aterm_lexicon::Trick;
use aterm_time::Instant;

use crate::cat_baker::CatColorKey;
use crate::pet_glyphs_gen::{PET_GLYPH_IDS, PET_GLYPHS, PetGlyphId};
use crate::pet_stroke::StrokeDetector;
use crate::rainbow_kitty::ARM_MIN;
use crate::rainbow_kitty::companion::{PetOffer, StarCatch};

mod console_life;
#[cfg(test)]
mod full_screen_tests;
use console_life::ConsoleLife;
pub use console_life::{PetAttention, PetInputKind};

const STATION_LEAD: f32 = 1.0;
const WRAP_HANG_MAX: f32 = 0.6;
const WRAP_HANG_FROM: f32 = 0.7;
const WRAP_HANG_PACE: f32 = 0.25;
const CHASE_GAIN: f32 = 4.0;
const MAX_SPEED: f32 = 26.0;
const CATCH_UP: f32 = 14.0;
const TOP_SPEED_MAX: f32 = 48.0;
const PACE_KEEP: f32 = 0.75;
const TYPING_STEP_MAX: f32 = 2.0;
const AWAIT_AHEAD: f32 = 12.0;
const SPEED_TAU: f32 = 0.11;
const LAUNCH_TAU: f32 = 0.05;
const SETTLE_TICK: f32 = 0.016;
const ARRIVED: f32 = 0.85;
const RUN_SPEED: f32 = 6.5;
const GAIT_HYST: f32 = 1.6;
const FLIP_SPEED: f32 = 1.2;
const STRIDE_CELLS: f32 = 1.7;
const STRIDE_EASE: f32 = 0.09;
const STRIDE_PLANT: f32 = 0.05;
const RUN_STRIDE_CELLS: f32 = 3.4;
const LAT_STRIDE_CELLS: f32 = 1.2;
const RUN_ART_MIN: f32 = 3.0;
const RUN_ART_GRACE: f32 = 0.04;
const STAND_MIN: f32 = RUN_STRIDE_CELLS / MAX_SPEED;
const LAT_SPEED: f32 = 3.4;
const LAT_HYST: f32 = 1.5;
const GAIT_TAU: f32 = 0.25;
const GAIT_REST_TAU: f32 = 0.6;
const WALK_CEIL: f32 = 30.0 * STRIDE_CELLS / 4.0;
const AMBLE_CEIL: f32 = 30.0 * LAT_STRIDE_CELLS / 4.0;
const GAIT_DWELL: f32 = 0.35;
const GAIT_TURN_SETTLE: f32 = 0.08;
const _: () = assert!(
    (LAT_SPEED + LAT_HYST) / LAT_STRIDE_CELLS <= 30.0,
    "the lateral walk cycle must stay at or under half of 60 fps"
);
const _: () = assert!(
    MAX_SPEED / RUN_STRIDE_CELLS <= 30.0,
    "the gallop cycle must stay at or under half of 60 fps"
);
const _: () = assert!(
    WALK_CEIL >= RUN_SPEED + GAIT_HYST && AMBLE_CEIL >= LAT_SPEED + LAT_HYST,
    "a gait's frame-rate ceiling must sit over the band it is chosen under"
);

const VEL_TAU: f32 = 0.35;
const VEL_MAX: f32 = 60.0;
const LEAD_TIME: f32 = 0.25;
const LEAD_MAX: f32 = 4.0;
const FLIGHT_LEAD_MAX: f32 = 2.0 * LEAD_MAX;
const RHYTHM_MOVES: u8 = 3;
const LEAD_STEP: f32 = 0.5;
const RHYTHM_WINDOW: f32 = 0.6;
const CARRY_COAST: f32 = 0.5;
const CARRY_ROOM: f32 = 1.0 + CARRY_COAST;
const KEY_BEAT_BLEND: f32 = 0.5;
const KEY_BEAT_LATE: f32 = 2.0;
const CHASE_GAIN_FAST: f32 = 6.0;
const WALL_RETURN_MARGIN: f32 = LEAD_MAX + 1.5;
const STOP_DECEL: f32 = 90.0;
const WALL_HOP_DUR: f32 = 0.25;
const WALL_HOP_ARC: f32 = 0.55;

const BIG_JUMP_COLS: f32 = 24.0;
const BIG_JUMP_COLS_FRAC: f32 = 0.4;
const BIG_GAP_FRAC: f32 = 0.5;
const JOY_CONTENT: f32 = 0.85;
const BIG_CROUCH_DUR: f32 = 0.28;
const WIGGLE_HZ: f32 = 7.0;
const BIG_FLIGHT_BASE: f32 = 0.45;
const BIG_FLIGHT_PER_CELL: f32 = 0.006;
const BIG_FLIGHT_MAX: f32 = 0.85;
const BIG_ARC_BASE: f32 = 0.75;
const BIG_ARC_PER_CELL: f32 = 0.012;
const BIG_ARC_MAX: f32 = 1.65;
const BIG_LAND_DUR: f32 = 0.40;
const BIG_SQUASH_NEAR: f32 = 0.7;
const BIG_SQUASH_SPAN: f32 = 20.0;
const SKID_CELLS: f32 = 0.8;
const TWO_BOUND_FRAC: f32 = 0.6;
const BOUND_SPLIT: f32 = 0.6;
const TOUCH_LAND_DUR: f32 = 0.13;

const ZEE_EVERY: f32 = 1.2;
const _: () = assert!(
    ZEE_EVERY > 1.0 / 3.0,
    "z births must stay under WCAG 2.3.1's 3 Hz general-flash bound"
);
const ZEE_LIFE: f32 = 2.2;
const ZEE_ALIVE_MAX: usize = 3;
const ZEE_STEPS: f32 = 16.0;
const ZEE_POP_LIFE: f32 = 0.55;
const _: () = assert!(
    ZEE_POP_LIFE < WAKE_DUR,
    "the wake's pop must die inside the stretch that owns the train"
);
const PURR_MOTE_EVERY: f32 = 2.0;
const PURR_MOTE_LIFE: f32 = 1.9;
const PURR_MOTE_STEPS: f32 = 16.0;

const SONG_NOTES: u8 = 4;
const SONG_BEAT: f32 = 0.26;
const _: () = assert!(
    SONG_BEAT < PERK_HOLD,
    "a song whose gap outlasts the perk it rides deals exactly one note"
);
const _: () = assert!(
    SONG_BEAT * (SONG_NOTES as f32) < PURR_MOTE_LIFE,
    "a song's notes must overlap, or it is a queue of meows and not a song"
);
const _: () = assert!(
    (SONG_NOTES as usize) <= PET_MOTES_MAX,
    "a song must fit the mote pool, or its last ♪ is silently dropped"
);

const SETTLE_TURN: f32 = 0.5;
const SIT_FRONT_GAP: f32 = 3.0;

const GAZE_FLIP_BAND: f32 = 0.4;
const SLEEP_FLIP_BAND: f32 = 1.0;
const SETTLE_FLIP_DWELL: f32 = 0.10;
const SLEEP_FLIP_DWELL: f32 = 1.2;
const LOAF_AFTER: f32 = 2.8;

const INK_PAD: f32 = 0.30;
const LETTER_VEIL_FULL: f32 = 0.30;
const LETTER_VEIL_FLOOR: f32 = 0.22;
const LETTER_VEIL_ON_S: f32 = 0.08;
const LETTER_VEIL_OFF_S: f32 = 0.20;
const LETTER_VEIL_DWELL: f32 = 0.20;
/// How long (s) the rest veil takes to go fully on ([`PetBrain::veil_over_letters`]): one ease, never a
/// blink; letters an Enter printed step it on at once.
const LETTER_VEIL_REST_ON_S: f32 = 0.35;
/// How far (rows) a body may reach into a lettered row from below
/// ([`PetBrain::arc_under_letters`]).
const HEAD_REACH_MAX: f32 = 0.25;
/// Clearance (cells) kept between a re-anchored footprint and the ink it fled.
const INK_GAP: f32 = 0.35;
const INK_EVICT_MAX: f32 = 4.0;
const INK_EVICT_SPEED: f32 = 12.0;
const REAIM_MAX_U: f32 = 0.85;
const WRAP_SLOP: f32 = 2.0;

#[inline]
const fn wrap_bar(w: f32) -> f32 {
    let bar = w - WRAP_SLOP;
    if bar > POUNCE_JUMP { bar } else { POUNCE_JUMP }
}
const WATCH_HUG_ROWS: f32 = 2.0;
const ZEE_INK_FADE_BAND: f32 = 0.6;
const PECK_CLEAR: f32 = 0.30;

const POUNCE_GAP: f32 = 14.0;
const SNAP_COLS: f32 = POUNCE_GAP;
const POUNCE_JUMP: f32 = 6.0;
const CROUCH_DUR: f32 = 0.10;
const COIL_PER_CELL: f32 = 0.006;
const COIL_MAX: f32 = 0.22;
const _: () = assert!(
    COIL_MAX < BIG_CROUCH_DUR,
    "the gather ordering law: the pounce's longest coil stays under the big wiggle"
);
const COIL_SQUASH: f32 = 0.14;
const COIL_RELEASE: f32 = 0.04;
const FLIGHT_PER_CELL: f32 = 0.021;
const FLIGHT_MIN: f32 = 0.16;
const FLIGHT_MAX: f32 = 0.42;

const FLIGHT_STALL_MAX: f32 = 0.16;
const ARC_BASE: f32 = 0.38;
const ARC_PER_CELL: f32 = 0.018;
const ARC_MAX: f32 = 1.0;
const LAND_DUR: f32 = 0.26;
const LAND_SQUASH: f32 = 0.24;
const LAND_ATTACK: f32 = 0.05;
const HOP_DUR: f32 = 0.20;
const HOP_ARC: f32 = 0.30;
const HOP_ARC_PER_CELL: f32 = 0.008;
const HOP_ARC_MAX: f32 = 0.72;

const LAUNCH_STRETCH: f32 = 0.14;
const DESCEND_STRETCH: f32 = 0.07;
const LEAP_RISE_T: f32 = 0.07;
const LEAP_DESC_T: f32 = 0.11;
const MIN_HERO: f32 = 0.048;
const HOP_TIGHT: f32 = 4.0;
const HOP_STRETCH: f32 = 0.06;
const LAUNCH_LEAD: f32 = 0.02;
const HANG_P_DOWN: f32 = 2.4;

const HOP_SPEED_MAX: f32 = 55.0;
const BIG_HOP_SPEED_MAX: f32 = 140.0;
const POUNCE_SPAN_MAX: f32 = HOP_SPEED_MAX * FLIGHT_MAX;
const WALL_SPAN_MAX: f32 = HOP_SPEED_MAX * WALL_HOP_DUR;
const QUIET_ONE_BOUND_SECS: f32 = 2.0 * BIG_FLIGHT_MAX;
const ROW_AS_COLS: f32 = 2.0;
const _: () = assert!(
    POUNCE_SPAN_MAX < BIG_JUMP_COLS,
    "the hop tier must end before the screen-crossing latch begins"
);

#[inline]
fn span_eq(dcol: f32, drow: f32) -> f32 {
    dcol.hypot(ROW_AS_COLS * drow)
}

const QUIET_PANE_PER_SEC: f32 = 0.74;

#[inline]
const fn quiet_speed_max(cols: u16) -> f32 {
    (QUIET_PANE_PER_SEC * (cols as f32)).clamp(HOP_SPEED_MAX, BIG_HOP_SPEED_MAX)
}

const STREAM_RUN: u8 = 3;
const STREAM_WINDOW: f32 = 0.5;
const STREAM_LAPSE: f32 = 0.8;

#[inline]
const fn stream_jump(cols: u16) -> f32 {
    POUNCE_GAP.max(0.25 * (cols as f32))
}

const STARTLE_COLS: f32 = 4.0;
const STARTLE_RUN: u8 = 3;
const STARTLE_RUN_WINDOW: f32 = 0.40;
const FLINCH_DUR: f32 = 0.18;
const FLINCH_SQUASH: f32 = 0.10;
const STARTLE_HOLD: f32 = 0.38;
const STARTLE_REACH: f32 = 2.0;
const FROLIC_WINDOW: f32 = 1.30;
const FROLIC_REVERSALS: u8 = 3;
const FROLIC_HOLD: f32 = 0.85;
const PERK_HOLD: f32 = 0.30;
const STAND_REST: f32 = RHYTHM_WINDOW;

const SIT_AFTER: f32 = 1.4;
const SIT_FOLD: f32 = 0.22;
const SLEEP_AFTER: f32 = 22.0;
const WAKE_DUR: f32 = 0.62;
const GROOM_AFTER: f32 = 6.5;
const GROOM_DUR: f32 = 1.70;
const BREATH_PERIOD: f32 = 2.4;
const BREATH_DEPTH: f32 = 0.030;

const CONTENT_PER_CELL: f32 = 0.020;
const CONTENT_DECAY: f32 = 0.070;
const CONTENT_MOVING_DECAY: f32 = 0.020;
const PURR_GATE: f32 = 0.26;
const PURR_HZ: f32 = 1.9;
const PURR_DEPTH: f32 = 0.05;

const BELL_LATCH_TTL: f32 = 0.6;
const CHEER_MIN_MS: u64 = 2000;
const CHEER_BIG_MS: u64 = 30_000;
const CHEER_CONTENT: f32 = 0.18;
const CHEER_STAGE_TRIPS: u8 = 2;
const CHEER_FAST_CONTENT: f32 = 0.06;
const SULK_CONTENT: f32 = 0.25;
const DROOP_HOLD: f32 = 1.6;
const VIGIL_AFTER: f32 = 30.0;
const SULK_FORGIVEN: f32 = 600.0;
const PET_LATCH_TTL: f32 = 2.0;
const PET_LATCH_MAX: u8 = 3;
const V2_PERK_LATE: f32 = 2.0 / 60.0;
const V2_CATCH_TTL: f32 = crate::rainbow_kitty::stardust::StarClass::M1
    .life_s(crate::rainbow_kitty::stardust::StarLane::Strike)
    - crate::rainbow_kitty::stardust::CATCH_FINISH_MS / 1000.0;
const V2_CATCH_COOL: f32 = 2.0 * BAT_HOLD;
const FLOW_LATCH_TTL: f32 = 0.9;

const LEASE_STANDOFF: f32 = LEAD_MAX;
const ROOM_CUE_TTL: f32 = FLOW_LATCH_TTL;
const EAR_FLAT_HOLD: f32 = 2.0 * PERK_HOLD;

const PET_HOLD: f32 = 1.1;
const PET_CONTENT: f32 = 0.08;

const TRICK_SETTLE: f32 = 0.70;
const TRICK_SETTLE_TENTATIVE: f32 = PURSUIT_QUIET;
const TRICK_TTL: f32 = 4.0;
const TRICK_CAP: f32 = 20.0;
const TRICK_HOLD: f32 = 4.0;
const TRICK_PLAY_HOLD: f32 = 2.0 * FROLIC_HOLD;
const TRICK_DUCK: f32 = HIDE_DWELL / 3.0;
const TRICK_CONTENT: f32 = CHEER_FAST_CONTENT;
const TRICK_CONTENT_COOL: f32 = 20.0;
const TRICK_SUBMIT_FORGIVEN: f32 = 2.0;
const _: () = assert!(
    TRICK_SETTLE > RHYTHM_WINDOW
        && TRICK_SETTLE_TENTATIVE >= TRICK_SETTLE
        && TRICK_TTL > TRICK_SETTLE_TENTATIVE + PERK_HOLD
        && TRICK_CAP > TRICK_TTL
);

const WATCH_RISE: f32 = 2.5;
const WATCH_FALL: f32 = 1.2;
const WATCH_GATE: f32 = 0.35;
const WATCH_MAX: f32 = 8.0;

const GAZE_FOLLOW_MIN: f32 = 2.0;
const GAZE_FOLLOW_WINDOW: f32 = 0.3;
const POINTER_RISE: f32 = 3.0;
const POINTER_FALL: f32 = 1.5;
const DASH_SPEED: f32 = 40.0;
const DASH_WINDOW: f32 = 0.25;
const DASH_MIN_T: f32 = 0.35;
const PLAY_CONTENT: f32 = 0.5;
const POUNCE_RANGE: f32 = 20.0;
const POINTER_PLAY_HOLD: f32 = 0.9;

const PURSUIT_RANGE: f32 = 24.0;
const PURSUIT_MIN_SPEED: f32 = 8.0;
const PURSUIT_HEAT: f32 = 0.6;
const PURSUIT_QUIET: f32 = 1.0;
const PURSUIT_MAX_T: f32 = 5.0;
const PURSUIT_COOL: f32 = 6.0;
const PURSUIT_HUG_ROWS: f32 = 2.0;
const PURSUIT_CATCH: f32 = 2.5;
const STALK_RANGE: f32 = 12.0;
const STALK_MIN_SPEED: f32 = 1.0;
const STAKEOUT_MAX: f32 = 6.0;
const GROOM_HOLD: f32 = 1.2;
const LOOK_RANGE: f32 = 24.0;
const LOOK_TTL: f32 = 1.5;

const TENNIS_AFTER: f32 = 2.0;
const TENNIS_LAPSE: f32 = 1.2;
const STUMBLE_SPEED: f32 = 20.0;
const STUMBLE_DUR: f32 = 0.12;
const BRAKE_DIST: f32 = 12.0;
const BRAKE_OVERSHOOT: f32 = 2.0;
const SKID_LEAN: f32 = 0.08;
const SKIP_CONTENT: f32 = 0.6;

const DROP_DUR: f32 = 0.18;
const DROP_SLEEP: f32 = 0.05;
const DROP_STRETCH: f32 = 0.08;
const DROP_SIT: f32 = 0.5;
const SCRABBLE_MISS: f32 = 6.0;
const SCRABBLE_DUR: f32 = 0.38;
const SCRABBLE_PLANT: f32 = 0.14;
const COMMIT_STARE: f32 = 0.25;
const POSE_FLOOR: f32 = 2.0 / 60.0;
const STILL_SECS: f32 = POSE_FLOOR - 0.5 / 60.0;

const BORED_AFTER: f32 = 8.0;
const BORED_UNTIL: f32 = SLEEP_AFTER - 4.0;
const BORED_COOL: f32 = 24.0;
const WRIGGLE_DUR: f32 = 2.2;
const WRIGGLE_BEAT: f32 = 0.35;
const HIDE_RANGE: f32 = 10.0;
const HIDE_DWELL: f32 = 3.5;
const POUNCE_REARM: f32 = 0.1;

const BAT_RANGE: f32 = 10.0;
const BAT_STANDOFF: f32 = 1.5;
const BAT_TTL: f32 = 1.5;
const BAT_HOLD: f32 = 0.6;

const HANDOFF_DEBOUNCE: f32 = 2.5;
const EDGE_FADE: f32 = 0.25;
const DEPART_SPEED: f32 = 18.0;
const DEPART_MAX: f32 = 4.0;
const ARRIVE_IN: f32 = 0.9;
const HOMECOMING_IN: f32 = EDGE_FADE;
const ARRIVE_FLOOR: f32 = 0.06;
const ADOPT_REACH: f32 = 1.0;
const ADOPT_HOLD_REACH: f32 = 0.05;

/// Cap on concurrent departing bodies.
pub const PET_DEPARTURES_MAX: usize = 3;

const FLICK_EVERY: f32 = 3.0;
const FLICK_DUR: f32 = 0.15;
const TWITCH_DUR: f32 = 0.16;
const TWITCH_BOB: f32 = 0.02;

const STRETCH_SPLIT: f32 = 0.55;
const STRETCH_LEAD: f32 = 0.20;
const STRETCH_AMP: f32 = 0.06;

const TAIL_PERIOD: f32 = 2.4;
const TAIL_PERIOD_STEP: f32 = 0.6;
const TAIL_AT: f32 = SIT_FOLD;
const TAIL_FLICK: f32 = 0.22;
const TAIL_TAP: f32 = 0.14;
const TAIL_GAP: f32 = 0.10;
const TAIL_SWING: f32 = 0.11;
const BLINK_PERIOD: f32 = 3.1;
const BLINK_PERIOD_STEP: f32 = 0.5;
const BLINK_AT: f32 = 0.9;
const BLINK_DUR: f32 = 0.10;
const BLINK_GAP: f32 = 0.08;
const YAWN_DUR: f32 = 0.45;
const GLANCE_PERIOD: f32 = 2.6;
const GLANCE_PERIOD_STEP: f32 = 0.7;
const GLANCE_AT: f32 = 0.8;
const LOOK_DUR: f32 = 0.6;
const LOOK_DUR_LONG: f32 = 0.95;
const LOOK_RETURN_MIN: f32 = 0.3;
const _: () = assert!(BLINK_AT >= GLANCE_AT);
const LOAF_PEEK_DUR: f32 = 0.25;

const TAIL_EDGES_FLICK: [f32; 2] = [0.0, TAIL_FLICK];
const TAIL_EDGES_DOUBLE: [f32; 4] = [
    0.0,
    TAIL_TAP,
    TAIL_TAP + TAIL_GAP,
    2.0 * TAIL_TAP + TAIL_GAP,
];
const TAIL_EDGES_SWING: [f32; 6] = [
    0.0,
    TAIL_SWING,
    3.0 * TAIL_SWING,
    5.0 * TAIL_SWING,
    6.0 * TAIL_SWING,
    7.0 * TAIL_SWING,
];
const BLINK_EDGES_ONE: [f32; 2] = [0.0, BLINK_DUR];
const BLINK_EDGES_TWO: [f32; 4] = [
    0.0,
    BLINK_DUR,
    BLINK_DUR + BLINK_GAP,
    2.0 * BLINK_DUR + BLINK_GAP,
];
const GLANCE_EDGES_SHORT: [f32; 2] = [0.0, LOOK_DUR];
const GLANCE_EDGES_LONG: [f32; 2] = [0.0, LOOK_DUR_LONG];

const PX_MARCH_MIN: f32 = 0.0005;
const PX_MARCH_MAX: u32 = 1024;
const PX_BISECT: u32 = 12;
const OFFER_PAST: f32 = 1e-5;

const FADE_IN: f32 = 0.30;
const FADE_OUT: f32 = 0.45;

const BREATH_WINDOW: f32 = 10.0;

/// The pet's art height in cells (the roster's viewbox at the host's bake size).
pub const ART_ROWS: f32 = 1.70;
/// Art aspect (w/h) shared by every pet pose.
pub const ART_ASPECT: f32 = 244.0 / 148.0;
/// The pet's width in columns, given a cell aspect.
#[must_use]
#[inline]
pub const fn art_cols(cell_w: u16, cell_h: u16) -> f32 {
    if cell_w == 0 || cell_h == 0 {
        return 3.0;
    }
    ART_ROWS * ART_ASPECT * (cell_h as f32) / (cell_w as f32)
}

/// Cap on live pet motes.
pub const PET_MOTES_MAX: usize = 4;

/// What a pet mote is drawn as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PetMoteKind {
    /// Landing dust at the heels of a screen-crossing jump.
    Dust,
    /// A light-sleep z, drifting up and away from the sleeper's head.
    Zee,
    /// The startled z popped by waking.
    ZeePop,
    /// A floating ♪ from a purring chest.
    Note,
    /// A floating heart from a purring chest.
    Heart,
}

impl PetMoteKind {
    #[inline]
    const fn rides_offer(self) -> bool {
        matches!(self, Self::Zee | Self::Note | Self::Heart)
    }
}

/// One resolved mote for this frame, in fractional grid cells (centre).
#[derive(Clone, Copy, Debug)]
pub struct PetMoteSprite {
    pub kind: PetMoteKind,
    /// Mote centre, fractional columns/rows from the grid origin.
    pub col: f32,
    pub row: f32,
    /// Scale of the emitter's natural mote tile (grows over life).
    pub scale: f32,
    /// Rotation in radians.
    pub rot: f32,
    /// 0..=255 fade envelope.
    pub alpha: u8,
    /// The ribbon colour under the cat at this ♪/♥'s birth ([`PetBrain::note_v2_offer`]).
    pub rgb: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
struct Mote {
    kind: PetMoteKind,
    born: f64,
    life: f32,
    col: f32,
    row: f32,
    dir: f32,
    seed: u8,
    rgb: Option<u32>,
}

impl Mote {
    #[inline]
    const fn age(&self, clock: f64) -> f32 {
        ((clock - self.born) as f32) / self.life.max(0.01)
    }

    #[inline]
    fn drift(&self, u: f32) -> (f32, f32) {
        let (base, run, sway, climb) = match self.kind {
            PetMoteKind::Zee => (0.30, 0.55, 0.10, 1.05),
            _ => (NOTE_BASE, NOTE_RUN, NOTE_SWAY, NOTE_CLIMB),
        };
        let wobble = (TAU * (u * 0.8 + 0.31 * f32::from(self.seed % 4))).sin();
        (
            self.col + self.dir * (base + run * u) + sway * wobble,
            self.row - climb * u,
        )
    }

    #[inline]
    const fn drift_slope(&self) -> (f32, f32) {
        match self.kind {
            PetMoteKind::Zee => ZEE_DRIFT_SLOPE,
            _ => NOTE_DRIFT_SLOPE,
        }
    }

    #[inline]
    const fn look_steps(&self) -> f32 {
        match self.kind {
            PetMoteKind::Zee => ZEE_STEPS,
            _ => PURR_MOTE_STEPS,
        }
    }

    #[inline]
    fn note_geom(&self, clock: f64) -> Option<((f32, f32), f32)> {
        let u = self.age(clock);
        if !(0.0..1.0).contains(&u) {
            return None;
        }
        let su = look_slice_age(look_slice(u, PURR_MOTE_STEPS), PURR_MOTE_STEPS);
        Some((self.drift(u), note_scale(su)))
    }
}

const ZEE_DRIFT_SLOPE: (f32, f32) = (0.55 + 0.10 * TAU * 0.8, 1.05);
const NOTE_DRIFT_SLOPE: (f32, f32) = (NOTE_RUN + NOTE_SWAY * TAU * 0.8, NOTE_CLIMB);
const NOTE_BASE: f32 = 0.18;
const NOTE_RUN: f32 = 0.30;
const NOTE_CLIMB: f32 = 0.85;

#[inline]
const fn look_slice(u: f32, steps: f32) -> f32 {
    (u * steps).floor().min(steps - 1.0)
}

#[inline]
const fn look_slice_age(slice: f32, steps: f32) -> f32 {
    (slice + 0.5) / steps
}

#[inline]
const fn note_scale(su: f32) -> f32 {
    0.78 + 0.50 * su
}

const NOTE_SCALE_MAX: f32 = note_scale((PURR_MOTE_STEPS - 0.5) / PURR_MOTE_STEPS);

const NOTE_SWAY: f32 = 0.12;

const NOTE_LANES: [(f32, f32); 6] = [
    (0.0, 0.0),
    (1.0, 0.24),
    (2.0, 0.10),
    (3.0, 0.26),
    (-1.0, 0.24),
    (-2.0, 0.10),
];

const CHEER_LANES: usize = 3;

const CHEER_LANES_REACH: (f32, f32) = {
    let mut ahead = 0.0f32;
    let mut up = 0.0f32;
    let mut i = 0;
    while i < CHEER_LANES {
        let (along, rise) = NOTE_LANES[i];
        if along > ahead {
            ahead = along;
        }
        if rise > up {
            up = rise;
        }
        i += 1;
    }
    (ahead, up)
};

const CHEER_COMPACT_STEP: (f32, f32) = (0.31, 0.13);

/// How a landing's ♪/♥ take the air over its stage ([`PetBrain::cheer_air`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheerAir {
    /// Side by side in their [`CHEER_LANES`] lanes ahead of the face.
    Lanes(bool),
    /// The compact spray ([`CHEER_COMPACT_STEP`]).
    Compact,
}

#[inline]
const fn clamp_i32(v: i32, lo: i32, hi: i32) -> i32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

#[inline]
const fn note_lane_step(cell_w: u16, cell_h: u16) -> f32 {
    let tile = (mote_tile_side(cell_h) as f32 * NOTE_SCALE_MAX).round();
    let cw = if cell_w > 1 { cell_w } else { 1 };
    (tile + 3.0) / cw as f32 + 2.0 * NOTE_SWAY
}

/// Natural (square) mote tile side, device px, for a cell `cell_h` px tall.
#[must_use]
#[inline]
pub const fn mote_tile_side(cell_h: u16) -> u16 {
    let ch = if cell_h > 4 { cell_h } else { 4 };
    let side = (ch as f32 * 0.55).round() as u16;
    if side > 6 { side } else { 6 }
}

/// Where a mote centred at `(col, row)` cells, drawn at `scale` of its tile, lands.
#[must_use]
#[inline]
pub const fn mote_box_px(
    (col, row): (f32, f32),
    scale: f32,
    (cell_w, cell_h): (u16, u16),
    (cols, rows): (u16, u16),
) -> Option<(i32, i32, u16)> {
    let side = mote_tile_side(cell_h);
    let dest = clamp_i32((side as f32 * scale).round() as i32, 1, u16::MAX as i32);
    let grid_w = (cols as i32).saturating_mul(cell_w as i32);
    let grid_h = (rows as i32).saturating_mul(cell_h as i32);
    if dest > grid_w || dest > grid_h {
        return None;
    }
    let cx = (col * cell_w as f32).round() as i32;
    let cy = (row * cell_h as f32).round() as i32;
    let max_x = if grid_w > dest { grid_w - dest } else { 0 };
    let max_y = if grid_h > dest { grid_h - dest } else { 0 };
    Some((
        clamp_i32(cx - dest / 2, 0, max_x),
        clamp_i32(cy - dest / 2, 0, max_y),
        dest as u16,
    ))
}

/// One resolved departing body for this frame (the breed handoff's outgoing kitty).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PetDepartureSprite {
    /// The run-cycle frame, already species-skinned (a dog departs as a dog).
    pub pose: PetGlyphId,
    /// Left edge in fractional columns / feet row.
    pub col: f32,
    pub row: f32,
    pub facing_left: bool,
    /// Squash & stretch and lift, [`PetFrame`] semantics.
    pub scale_x: f32,
    pub scale_y: f32,
    pub lift: f32,
    /// 0..=255 fade envelope.
    pub alpha: u8,
    /// The outgoing look's ramp indices.
    pub coat: u8,
    pub iris: u8,
    /// Which ghost this is ([`Departure::id`]), identity only (not in [`PetFrame::fp`]).
    pub id: u32,
    /// Drawn beneath the glyphs.
    pub under_ink: bool,
    /// A re-sighting ghost on its own glass (`Departure::own_glass`).
    pub own_glass: bool,
    /// Which own glass ([`PetBrain::displace_resight`]).
    pub glass: u32,
    /// The palette the ghost's body was drawn in (`Departure::colors`).
    pub colors: Option<CatColorKey>,
}

impl PetDepartureSprite {
    /// The departing body's drawn rect in grid pixels.
    #[must_use]
    #[inline]
    pub const fn body_px(
        &self,
        cell_w: u16,
        cell_h: u16,
        cols: u16,
        rows: u16,
    ) -> Option<(i32, i32, i32, i32)> {
        if self.alpha == 0 || cell_w == 0 || cell_h == 0 {
            return None;
        }
        Some(drawn_body_px(
            (self.col, self.row),
            Stance {
                scale_x: self.scale_x,
                scale_y: self.scale_y,
                lift: self.lift,
            },
            (cell_w, cell_h),
            (cols, rows),
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Stance {
    scale_x: f32,
    scale_y: f32,
    lift: f32,
}

impl Stance {
    const NEUTRAL: Self = Self {
        scale_x: 1.0,
        scale_y: 1.0,
        lift: 0.0,
    };

    #[inline]
    const fn of(frame: &PetFrame) -> Self {
        Self {
            scale_x: frame.scale_x,
            scale_y: frame.scale_y,
            lift: frame.lift,
        }
    }

    #[inline]
    const fn toward(self, to: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let one_minus_t = 1.0 - t;
        Self {
            scale_x: self.scale_x * one_minus_t + to.scale_x * t,
            scale_y: self.scale_y * one_minus_t + to.scale_y * t,
            lift: self.lift * one_minus_t + to.lift * t,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Departure {
    born: f64,
    id: u32,
    coat: u8,
    iris: u8,
    from_col: f32,
    row: f32,
    edge: f32,
    speed: f32,
    still: bool,
    facing_left: bool,
    stride0: f32,
    pose0: PetGlyphId,
    stance0: Stance,
    resight: bool,
    own_glass: Option<u32>,
    own_clear: bool,
    peak: f32,
    colors: Option<CatColorKey>,
    surface: Option<(u64, bool)>,
}

#[derive(Clone, Copy, Debug)]
struct Carried {
    ghost: Departure,
    presence: f32,
    home: bool,
}

#[derive(Clone, Copy, Debug)]
struct AdoptedLook {
    pose: PetGlyphId,
    facing_left: bool,
    stance: Stance,
    eased: f32,
    seat: (f32, f32),
    beneath: Option<(PetGlyphId, bool)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct HeldStand {
    surface: Option<(u64, bool)>,
    caret: (u16, u16),
    lead: f32,
}

#[derive(Clone, Copy, Debug)]
struct PendingResight {
    ghost: Departure,
    at: Instant,
}

impl Departure {
    #[inline]
    const fn life(&self) -> f32 {
        ((self.edge - self.from_col).abs() / self.speed + EDGE_FADE).min(DEPART_MAX)
    }

    #[inline]
    const fn envelope(&self, t: f32) -> f32 {
        let life = self.life();
        let fade_start = life - EDGE_FADE;
        let fade = if t <= fade_start {
            1.0
        } else {
            (1.0 - (t - fade_start) / EDGE_FADE).clamp(0.0, 1.0)
        };
        fade * self.peak.clamp(0.0, 1.0)
    }

    #[inline]
    fn look_at(&self, t: f32) -> (f32, PetGlyphId, bool, Stance) {
        let span = (self.edge - self.from_col).abs();
        let dir = if self.edge < self.from_col { -1.0 } else { 1.0 };
        let run = (self.speed * t).min(span);
        let col = self.from_col + dir * run;
        if self.still {
            return (col, self.pose0, self.facing_left, self.stance0);
        }
        let stride = self.stride0 + run / RUN_STRIDE_CELLS;
        let cycle = PetBrain::CYCLE_RUN;
        let phase = if (0.0..1.0).contains(&stride) {
            stride
        } else {
            stride.rem_euclid(1.0)
        };
        let idx = ((phase * cycle.len() as f32) as usize).min(cycle.len() - 1);
        (col, cycle[idx], dir < 0.0, Stance::NEUTRAL)
    }

    #[inline]
    fn stopped_at(self, t: f32) -> Self {
        if self.still {
            return self;
        }
        let (col, pose0, facing_left, stance0) = self.look_at(t);
        Self {
            from_col: col,
            edge: col,
            still: true,
            pose0,
            facing_left,
            stance0,
            ..self
        }
    }
}

/// How a parked look is owed to land.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PetArrival {
    /// A cat this window has not worn recently.
    Ceremony,
    /// A cat coming home ([`HOMECOMING_IN`]).
    #[default]
    Quiet,
}

/// What [`PetBrain::sync_look`] did with one frame's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncLookOutcome {
    /// The pair the appearance is wearing after the call.
    pub worn: (u8, u8),
    /// True exactly when this call parked a differing pair (a fresh park or a changed one).
    pub parked: bool,
}

/// What the pet is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PetAction {
    /// Curled up.
    Sleep,
    /// The one-shot wake-up stretch on the way out of [`Self::Sleep`].
    Waking,
    /// Settled on its haunch by the caret.
    Sit,
    /// The long dwell: legs tucked, low profile.
    Loaf,
    /// Settled and content.
    Purr,
    /// A sit interrupted by a wash.
    Groom,
    /// Ears up, head high.
    Perk,
    /// On its feet by the caret.
    Stand,
    /// Closing a small gap on foot.
    Walk,
    /// Closing a large gap at speed.
    Run,
    /// Gathered, about to leave the ground.
    Crouch,
    /// Airborne on a pounce or a row hop.
    Leap,
    /// The braced, compressed frame that absorbs a landing.
    Land,
    /// Flinched away from a retreating caret.
    Startle,
    /// Playing: the caret is being fussed with rather than driven.
    Frolic,
    /// The sulk after a failed command.
    Droop,
}

impl PetAction {
    /// True while the pet is settled.
    #[must_use]
    #[inline]
    pub const fn settled(self) -> bool {
        matches!(
            self,
            Self::Sleep
                | Self::Sit
                | Self::Loaf
                | Self::Purr
                | Self::Groom
                | Self::Perk
                | Self::Stand
        )
    }

    /// True while the pet is off the ground.
    #[must_use]
    #[inline]
    pub const fn airborne(self) -> bool {
        matches!(self, Self::Leap)
    }
}

/// What the host observed this frame.
#[derive(Clone, Copy, Debug)]
pub struct PetSense {
    pub now: Instant,
    /// The caret cell, or `None` when the frame has no caret in the pet's coordinate space.
    pub caret: Option<(u16, u16)>,
    /// Whether the emulator paints that caret this frame (`false` for a dectcem hide).
    pub caret_drawn: bool,
    /// The emulator wrapped the caret since the last host read, from the grid.
    pub wrapped: bool,
    pub rows: u16,
    pub cols: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    /// Reduced motion: no chase, no gait, no arcs.
    pub reduced_motion: bool,
    /// The focused pane is streaming this frame (the perk-and-watch).
    pub output_burst: bool,
    /// The mouse pointer in fractional grid cells `(col, row)` of the pet's pane, `None` outside
    /// it.
    pub pointer: Option<(f32, f32)>,
}

/// One frame's resolved pet.
#[derive(Clone, Copy, Debug)]
pub struct PetFrame {
    /// Opacity 0..=255 (0 ⇒ nothing to draw).
    pub alpha: u8,
    /// Presence envelope alone, 0..=255.
    pub lane_alpha: u8,
    pub action: PetAction,
    /// The authored art frame to bake for this present.
    pub pose: PetGlyphId,
    /// The pet's left edge, in fractional columns from the grid's left edge.
    pub col: f32,
    /// The row the pet's feet are on, fractional while hopping between lines.
    pub row: f32,
    /// Extra height above the ground line, in cell heights (`+` = up).
    pub lift: f32,
    /// Draw mirrored: the art is authored facing right.
    pub facing_left: bool,
    /// Dest-rect scale about the sprite centre (squash & stretch).
    pub scale_x: f32,
    pub scale_y: f32,
    /// Purr intensity 0..=1.
    pub purr: f32,
    /// Contentment over [`PURR_GATE`].
    pub contented: bool,
    /// Draw the pet under the glyphs this frame, peering out from behind a word.
    pub under_ink: bool,
    /// The pet's mote lane for this frame (dust, z's, notes), `None`-padded.
    pub motes: [Option<PetMoteSprite>; PET_MOTES_MAX],
    /// The breed handoff's departing bodies for this frame ([`PET_DEPARTURES_MAX`]).
    pub departures: [Option<PetDepartureSprite>; PET_DEPARTURES_MAX],
}

impl PetFrame {
    /// Quantized fingerprint for the host's repaint key.
    #[must_use]
    #[inline]
    pub const fn fp(&self) -> u64 {
        // Keyed on presence, not the composed alpha: the arrival multiply can
        // truncate a faint body to 0 while its ghosts are live.
        if self.lane_alpha == 0 {
            return 0;
        }
        const fn q(v: f32, s: f32) -> u64 {
            ((v * s) as i64) as u64
        }
        let mut h = aterm_hash::FNV64_OFFSET;
        h = aterm_hash::fnv_mix_u64(h, self.alpha as u64);
        h = aterm_hash::fnv_mix_u64(h, self.lane_alpha as u64);
        h = aterm_hash::fnv_mix_u64(h, self.action as u64);
        h = aterm_hash::fnv_mix_u64(h, self.pose as u64);
        h = aterm_hash::fnv_mix_u64(h, q(self.col, 64.0));
        h = aterm_hash::fnv_mix_u64(h, q(self.row, 64.0));
        h = aterm_hash::fnv_mix_u64(h, q(self.lift, 256.0));
        h = aterm_hash::fnv_mix_u64(h, self.facing_left as u64);
        h = aterm_hash::fnv_mix_u64(h, self.under_ink as u64);
        h = aterm_hash::fnv_mix_u64(h, q(self.scale_x, 256.0));
        h = aterm_hash::fnv_mix_u64(h, q(self.scale_y, 256.0));
        h = aterm_hash::fnv_mix_u64(h, q(self.purr, 64.0));
        let mut i = 0;
        while i < self.motes.len() {
            if let Some(m) = self.motes[i] {
                h = aterm_hash::fnv_mix_u64(h, m.kind as u64);
                h = aterm_hash::fnv_mix_u64(h, q(m.col, 64.0));
                h = aterm_hash::fnv_mix_u64(h, q(m.row, 64.0));
                h = aterm_hash::fnv_mix_u64(h, q(m.scale, 64.0));
                h = aterm_hash::fnv_mix_u64(h, q(m.rot, 64.0));
                h = aterm_hash::fnv_mix_u64(h, m.alpha as u64);
                h = aterm_hash::fnv_mix_u64(
                    h,
                    match m.rgb {
                        Some(rgb) => rgb as u64,
                        None => u64::MAX,
                    },
                );
            }
            i += 1;
        }
        let mut j = 0;
        while j < self.departures.len() {
            if let Some(d) = self.departures[j] {
                h = aterm_hash::fnv_mix_u64(h, d.pose as u64);
                h = aterm_hash::fnv_mix_u64(h, q(d.col, 64.0));
                h = aterm_hash::fnv_mix_u64(h, q(d.row, 64.0));
                h = aterm_hash::fnv_mix_u64(h, q(d.lift, 256.0));
                h = aterm_hash::fnv_mix_u64(h, q(d.scale_x, 256.0));
                h = aterm_hash::fnv_mix_u64(h, q(d.scale_y, 256.0));
                h = aterm_hash::fnv_mix_u64(h, d.facing_left as u64);
                h = aterm_hash::fnv_mix_u64(h, d.alpha as u64);
                h = aterm_hash::fnv_mix_u64(h, d.coat as u64);
                h = aterm_hash::fnv_mix_u64(h, d.iris as u64);
                h = aterm_hash::fnv_mix_u64(h, d.under_ink as u64);
                h = aterm_hash::fnv_mix_u64(h, d.own_glass as u64);
                h = aterm_hash::fnv_mix_u64(h, d.glass as u64);
                h = aterm_hash::fnv_mix_u64(
                    h,
                    match d.colors {
                        Some(c) => (c.accent as u64) << 8 | (c.background as u64),
                        None => u64::MAX,
                    },
                );
            }
            j += 1;
        }
        h
    }

    /// The pet's live drawn body this frame in grid pixels.
    #[must_use]
    #[inline]
    pub const fn body_px(
        &self,
        cell_w: u16,
        cell_h: u16,
        cols: u16,
        rows: u16,
    ) -> Option<(i32, i32, i32, i32)> {
        if self.alpha == 0 || cell_w == 0 || cell_h == 0 {
            return None;
        }
        Some(drawn_body_px(
            (self.col, self.row),
            Stance::of(self),
            (cell_w, cell_h),
            (cols, rows),
        ))
    }
}

#[inline]
const fn drawn_body_px(
    (col, row): (f32, f32),
    stance: Stance,
    (cell_w, cell_h): (u16, u16),
    (cols, rows): (u16, u16),
) -> (i32, i32, i32, i32) {
    let nat_h = (ART_ROWS * cell_h as f32).round();
    let nat_w = (nat_h * ART_ASPECT).round();
    let nat_h = clamp_i32(nat_h as i32, 1, u16::MAX as i32);
    let nat_w = clamp_i32(nat_w as i32, 1, u16::MAX as i32);
    let dest_w = clamp_i32(
        (nat_w as f32 * stance.scale_x).round() as i32,
        1,
        u16::MAX as i32,
    );
    let dest_h = clamp_i32(
        (nat_h as f32 * stance.scale_y).round() as i32,
        1,
        u16::MAX as i32,
    );
    let grid_w = (cols as i32).saturating_mul(cell_w as i32);
    let grid_h = (rows as i32).saturating_mul(cell_h as i32);
    let cx = (col * cell_w as f32).round() as i32 + nat_w / 2;
    let baseline =
        ((row + 1.0) * cell_h as f32).round() as i32 - (stance.lift * cell_h as f32).round() as i32;
    let max_x = if grid_w > dest_w { grid_w - dest_w } else { 0 };
    let max_y = if grid_h > dest_h { grid_h - dest_h } else { 0 };
    let x0 = clamp_i32(cx - dest_w / 2, 0, max_x);
    let y0 = clamp_i32(baseline - dest_h, 0, max_y);
    (x0, x0 + dest_w, y0, y0 + dest_h)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PlayLand {
    PointerFrolic,
    BatSwipe { col: f32, row: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum RoomCue {
    Look { col: f32 },
    Nod,
    EarFlat,
    Mail,
}

/// The inbox as the pet sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoomInbox {
    /// The newest row above the handled watermark (`inbox seen`), by per-session id.
    pub unseen: u64,
    /// Some unread row's advisory deadline (`dl=`, ms from its arrival) has passed.
    pub overdue: bool,
    /// A standing `hold on`.
    pub hold: bool,
}

impl RoomInbox {
    /// An empty inbox with no unseen messages, overdue rows, or standing hold.
    pub const EMPTY: Self = Self {
        unseen: 0,
        overdue: false,
        hold: false,
    };
}

impl Default for RoomInbox {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PendingTrick {
    trick: Trick,
    confirmed: bool,
    noted: Instant,
    stamped: Instant,
}

#[derive(Clone, Copy, Debug)]
struct Flight {
    from_col: f32,
    from_row: f32,
    to_col: f32,
    to_row: f32,
    dur: f32,
    arc: f32,
    t: f32,
    reaimed: bool,
    big: bool,
    show: bool,
    hop: bool,
    coiled: bool,
    drop: bool,
    late_move: bool,
    running: bool,
    awaits_wrap: bool,
    dur0: f32,
}

#[derive(Clone, Copy, Debug)]
struct FlightKind {
    big: bool,
    show: bool,
    hop: bool,
    drop: bool,
}

impl FlightKind {
    const PLAIN: Self = Self {
        big: false,
        show: false,
        hop: false,
        drop: false,
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum WrapAhead {
    Approach,
    Due,
    Launched(f32, f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeapPolicy {
    Quiet,
    Show,
    Play,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    Hop,
    Bound,
}

/// Which animal the pet is drawn as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum PetSpecies {
    /// The original companion (`cursor_trail_style = "rainbow kitty pet"`).
    #[default]
    Cat,
    /// The dog (`cursor_trail_style = "rainbow dog pet"`).
    Dog,
}

impl PetSpecies {
    /// Map a cat pose onto this species' equivalent frame.
    #[must_use]
    #[inline]
    pub const fn skin(self, pose: PetGlyphId) -> PetGlyphId {
        match self {
            Self::Cat => pose,
            Self::Dog => DOG_SKIN[pose as usize],
        }
    }
}

const DOG_SKIN: [PetGlyphId; PET_GLYPHS.len()] = {
    const fn starts_with(s: &[u8], prefix: &[u8]) -> bool {
        if s.len() < prefix.len() {
            return false;
        }
        let mut i = 0;
        while i < prefix.len() {
            if s[i] != prefix[i] {
                return false;
            }
            i += 1;
        }
        true
    }
    const fn dog_of(dog: &[u8], cat: &[u8]) -> bool {
        const DOG: &[u8] = b"pet_dog_";
        if !starts_with(dog, DOG) || dog.len() - DOG.len() != cat.len() - 4 {
            return false;
        }
        let mut i = 0;
        while i < cat.len() - 4 {
            if dog[DOG.len() + i] != cat[4 + i] {
                return false;
            }
            i += 1;
        }
        true
    }
    let mut table = [PetGlyphId::PetApex; PET_GLYPHS.len()];
    let mut i = 0;
    while i < PET_GLYPH_IDS.len() {
        let pose = PET_GLYPH_IDS[i];
        let cat = PET_GLYPHS[pose as usize].id.as_bytes();
        table[pose as usize] = pose;
        if starts_with(cat, b"pet_") && !starts_with(cat, b"pet_dog_") {
            let mut j = 0;
            while j < PET_GLYPH_IDS.len() {
                let dog = PET_GLYPH_IDS[j];
                if dog_of(PET_GLYPHS[dog as usize].id.as_bytes(), cat) {
                    table[pose as usize] = dog;
                    break;
                }
                j += 1;
            }
            assert!(
                table[pose as usize] as usize != pose as usize,
                "every cat pose in PET_GLYPHS must have a pet_dog_<tail> counterpart"
            );
        }
        i += 1;
    }
    table
};

/// The pet's decision layer.
pub struct PetBrain {
    console: ConsoleLife,
    species: PetSpecies,
    col: f32,
    row: f32,
    speed: f32,
    facing_left: bool,

    action: PetAction,
    action_t: f32,
    quiet: f32,
    stride: f32,
    clock: f64,
    lane_clock: f64,

    last_caret: Option<(u16, u16)>,
    regrid_caret: Option<(u16, u16)>,
    last_now: Option<Instant>,

    flight: Option<Flight>,
    deferred_hidden_landing: Option<(f32, f32)>,
    retired_flight_lift: Option<(f32, f32)>,
    land_t: f32,
    hop_crouch: bool,
    coil_d: f32,
    flinch_t: f32,

    last_dir: i8,
    reversals: u8,
    reversal_t: f32,
    vhat: f32,
    run_dir: i8,
    run_count: u8,
    run_t: f32,
    lead_room: f32,
    held_lead: f32,
    held_for: Option<(u16, u16)>,
    held_stash: [Option<HeldStand>; 2],
    run_gap: f32,
    vrun: f32,
    rhythm_at: Option<f64>,
    run_pace: f32,
    run_cells: f32,
    run_secs: f32,
    run_step: f32,
    key_beat: f32,
    key_pace: f32,
    wall_side: bool,
    pending_wall_transit: bool,
    wrap_back: bool,
    wrap_ahead: Option<WrapAhead>,
    launch_running: bool,
    land_running: bool,
    retreat_run: u8,
    retreat_gap: f32,
    retreat_bottled: bool,
    ink_base: u16,
    ink_spans: Vec<(u16, u16)>,
    ink_scrolled: bool,
    ink_live: Option<u16>,

    content: f32,
    pending_bell: Option<Instant>,
    pending_cheer: Option<bool>,
    room_cheer: bool,
    room_cheer_trips: u8,
    room_cheer_stage: Option<(f32, f32)>,
    room_cheer_air: Option<CheerAir>,
    room_cheer_leaving: bool,
    pending_sulk: bool,
    pending_flow: Option<Instant>,
    pending_perk: Option<Instant>,
    perk_spent: Option<Instant>,
    pending_catch: Option<StarCatch>,
    caught: Option<StarCatch>,
    catch_cool: f32,
    mote_rgb: Option<u32>,
    pending_pet: u8,
    pet_at: Option<Instant>,
    pet_hold_t: f32,
    song_left: u8,
    song_next: f32,
    stroke: StrokeDetector,
    watch_heat: f32,
    watch_t: f32,
    watch_spent: bool,
    exec_since: Option<Instant>,
    vigil_armed: bool,
    full_screen: bool,
    #[cfg(test)]
    hug_armed: bool,
    vigil_t: f32,
    vigil: bool,
    pending_vigil_pounce: bool,
    vigil_cheer: Option<bool>,
    sulk_at: Option<f64>,
    last_pointer: Option<(f32, f32)>,
    pointer_vx: f32,
    pointer_vy: f32,
    pointer_speed: f32,
    pointer_heat: f32,
    dash_t: f32,
    pointer_armed: bool,
    pending_pointer_pounce: Option<(f32, f32)>,
    play_to: Option<(f32, f32)>,
    play_land: Option<PlayLand>,
    play_frolic: bool,
    play_hold: f32,
    swipe: bool,
    pending_bat: Option<(f32, f32)>,
    bat_at: Option<Instant>,
    pursuit_t: Option<f32>,
    pursuit_cool: f32,
    groom_owed: bool,
    stakeout: bool,
    stakeout_t: f32,
    pending_look: Option<f32>,
    look_at: Option<Instant>,
    bat_greet: bool,
    tennis: bool,
    tennis_last: f64,
    last_frolic: f64,
    stumble_t: f32,
    leg_dist: f32,
    braking: bool,
    brake_over: Option<f32>,
    skid_puff: bool,
    lateral: bool,
    gait_pace: f32,
    gait_dir: f32,
    gait_t: f32,
    gait_v: f32,
    gait_rest: f32,
    tick_dt: f32,
    gallop_at: f64,
    planted: bool,
    departing: bool,
    stand_from: Option<f64>,
    still_since: Option<(f64, PetGlyphId)>,
    still_console: bool,
    stand_ends_leg: bool,
    drew_leg: bool,
    console_posed: bool,
    deferred_arc: Option<(f32, f32, f32, f32, FlightKind)>,

    gallop_since: Option<f64>,
    running: bool,
    pending_drop: bool,
    drop_sit_t: f32,
    drop_stare: bool,
    scrabble: bool,
    commit_t: f32,
    resume: Option<(PetAction, f32)>,
    bored_cool: f32,
    hide_to: Option<f32>,
    hiding: bool,
    hide_t: f32,
    wriggle_t: f32,
    twitch_t: f32,
    twitch_up: bool,
    last_burst: bool,
    droop_seated: bool,
    droop_brief: bool,
    room_lease: bool,
    room_quiet: bool,
    room_hold: bool,
    room_turn: u64,
    room_sibling: bool,
    room_unseen: u64,
    room_overdue: bool,
    room_turn_seeded: bool,
    room_inbox_seeded: bool,
    pending_room: Option<RoomCue>,
    room_at: Option<Instant>,
    /// A room cue (`consume_room`) or word-cat peek (`consume_bat` / `pending_look`) is currently
    /// playing out on glass.
    pub(super) room_cue_act: bool,
    pending_trick: Option<PendingTrick>,
    trick_go: Option<Trick>,
    trick_heard: Option<(Trick, Instant)>,
    trick_act: bool,
    trick_settle: Option<Trick>,
    hold_end_q: f32,
    trick_submit_at: Option<Instant>,
    trick_content_at: f64,
    pending_pounce: bool,
    pending_big_jump: bool,
    big_gather: bool,
    quiet_leap: bool,
    stream: bool,
    stream_run: u8,
    stream_last: f64,
    bound2: Option<(f32, f32, bool)>,
    land_span: f32,
    skid_from: f32,
    skid_dir: f32,
    motes: [Option<Mote>; PET_MOTES_MAX],
    mote_serial: u8,
    departure_serial: u32,
    settle_serial: u8,
    settle_seed: u8,
    mote_mark: i64,
    sit_front: bool,
    peek: bool,
    look_row: i8,
    gaze_dwell: f32,
    alpha: f32,

    colors: Option<CatColorKey>,
    surface: Option<(u64, bool)>,

    worn: Option<(u8, u8)>,
    pending_worn: Option<(u8, u8)>,
    pending_arrival: PetArrival,
    handoff_parked_clock: Option<f64>,
    arrive_t: f32,
    arrive_in: f32,
    arrive_from: f32,
    departures: [Option<Departure>; PET_DEPARTURES_MAX],
    resight: Option<PendingResight>,
    carried: [Option<Carried>; PET_DEPARTURES_MAX],
    last_presence: f32,
    letter_veil: (f32, Option<Instant>, f32),
    letter_cover_since: Option<Instant>,
    /// Since when a body at rest has lain on letters unbroken ([`PetBrain::veil_over_letters`]).
    letter_rest_since: Option<Instant>,
    /// The rest on letters an Enter's own letters stepped the veil on for
    /// ([`PetBrain::veil_over_letters`]).
    letter_rest_stepped: bool,
    /// The key sequence of the Enter whose letters stepped the veil on: one
    /// step an Enter ([`PetBrain::veil_over_letters`]).
    letter_step_seq: u64,
    /// Since when the letters a stepped rest lay on have been gone
    /// ([`PetBrain::veil_over_letters`]).
    letter_gone_since: Option<Instant>,
    /// A gather on letters for a landing on letters whose rest veil is still going on
    /// ([`PetBrain::veil_over_letters`], [`PetBrain::gather_owes_veil`]).
    veil_gather: bool,
    /// A test's negative control ([`Self::set_letters_unseen`]).
    #[cfg(any(test, debug_assertions))]
    letters_unseen: bool,
    /// A test's negative control ([`Self::set_letter_arrival_dwells`]).
    #[cfg(any(test, debug_assertions))]
    letter_arrival_dwells: bool,
    /// A test's negative control ([`Self::set_letter_step_rearms`]).
    #[cfg(any(test, debug_assertions))]
    letter_step_rearms: bool,
    /// The host is presenting a frame the program has not finished ([`Self::note_repainting`]).
    repainting: bool,
    last_drawn: [f32; PET_DEPARTURES_MAX],
    last_grid: (u16, u16),
    last_pose: PetGlyphId,
    last_facing: bool,
    last_stance: Stance,
    adopted_look: Option<AdoptedLook>,
    last_cell_h: u16,
    last_cell_w: u16,
    last_reduced: bool,
    col_at_tick: f32,
    home: f32,
    evicting: bool,
}

impl Default for PetBrain {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl PetBrain {
    /// Construct a fresh resident pet brain in its resting initial state.
    #[must_use]
    #[inline]
    pub const fn new() -> Self {
        Self {
            console: ConsoleLife::new(),
            species: PetSpecies::Cat,
            col: 0.0,
            row: 0.0,
            speed: 0.0,
            facing_left: false,
            action: PetAction::Sleep,
            action_t: 0.0,
            quiet: SLEEP_AFTER,
            stride: 0.0,
            clock: 0.0,
            lane_clock: 0.0,
            last_caret: None,
            regrid_caret: None,
            last_now: None,
            flight: None,
            deferred_hidden_landing: None,
            retired_flight_lift: None,
            land_t: 0.0,
            hop_crouch: false,
            coil_d: CROUCH_DUR,
            flinch_t: 0.0,
            last_dir: 0,
            reversals: 0,
            reversal_t: 0.0,
            vhat: 0.0,
            run_dir: 0,
            run_count: 0,
            run_t: 60.0,
            lead_room: 0.0,
            held_lead: 0.0,
            held_for: None,
            held_stash: [None; 2],
            run_gap: 0.0,
            vrun: 0.0,
            run_pace: 0.0,
            run_cells: 0.0,
            run_secs: 0.0,
            run_step: 0.0,
            rhythm_at: None,
            key_beat: 0.0,
            key_pace: 0.0,
            wall_side: false,
            pending_wall_transit: false,
            wrap_back: false,
            wrap_ahead: None,
            launch_running: false,
            land_running: false,
            retreat_run: 0,
            retreat_gap: 60.0,
            retreat_bottled: false,
            ink_base: 0,
            ink_spans: Vec::new(),
            ink_scrolled: false,
            ink_live: None,
            content: 0.0,
            pending_bell: None,
            pending_cheer: None,
            room_cheer: false,
            room_cheer_trips: 0,
            room_cheer_stage: None,
            room_cheer_air: None,
            room_cheer_leaving: false,
            pending_sulk: false,
            pending_flow: None,
            pending_perk: None,
            perk_spent: None,
            pending_catch: None,
            caught: None,
            catch_cool: 0.0,
            mote_rgb: None,
            pending_pet: 0,
            pet_at: None,
            pet_hold_t: 0.0,
            song_left: 0,
            song_next: 0.0,
            stroke: StrokeDetector::new(),
            watch_heat: 0.0,
            watch_t: 0.0,
            watch_spent: false,
            exec_since: None,
            vigil_armed: false,
            full_screen: false,
            #[cfg(test)]
            hug_armed: false,
            vigil_t: 0.0,
            vigil: false,
            pending_vigil_pounce: false,
            vigil_cheer: None,
            sulk_at: None,
            last_pointer: None,
            pointer_vx: 0.0,
            pointer_vy: 0.0,
            pointer_speed: 0.0,
            pointer_heat: 0.0,
            dash_t: 0.0,
            pointer_armed: true,
            pending_pointer_pounce: None,
            play_to: None,
            play_land: None,
            play_frolic: false,
            play_hold: 0.0,
            swipe: false,
            pending_bat: None,
            bat_at: None,
            pursuit_t: None,
            pursuit_cool: 0.0,
            groom_owed: false,
            stakeout: false,
            stakeout_t: 0.0,
            pending_look: None,
            look_at: None,
            bat_greet: false,
            tennis: false,
            tennis_last: 0.0,
            last_frolic: -1.0e9,
            stumble_t: 0.0,
            leg_dist: 0.0,
            braking: false,
            brake_over: None,
            skid_puff: false,
            lateral: false,
            gait_pace: 0.0,
            gait_dir: 0.0,
            gait_t: 0.0,
            gait_v: 0.0,
            gait_rest: 0.0,
            tick_dt: 0.0,
            gallop_at: 0.0,
            planted: false,
            departing: false,
            stand_from: None,
            still_since: None,
            still_console: false,
            stand_ends_leg: false,
            drew_leg: false,
            console_posed: false,
            deferred_arc: None,
            gallop_since: None,
            running: false,
            pending_drop: false,
            drop_sit_t: 0.0,
            drop_stare: false,
            scrabble: false,
            commit_t: 0.0,
            resume: None,
            bored_cool: 0.0,
            wriggle_t: 0.0,
            hide_to: None,
            hiding: false,
            hide_t: 0.0,
            twitch_t: 0.0,
            twitch_up: false,
            last_burst: false,
            droop_seated: false,
            droop_brief: false,
            room_lease: false,
            room_quiet: false,
            room_hold: false,
            room_turn: 0,
            room_sibling: false,
            room_unseen: 0,
            room_overdue: false,
            room_turn_seeded: false,
            room_inbox_seeded: false,
            pending_room: None,
            room_at: None,
            room_cue_act: false,
            pending_trick: None,
            trick_go: None,
            trick_heard: None,
            trick_act: false,
            trick_settle: None,
            hold_end_q: 0.0,
            trick_submit_at: None,
            trick_content_at: -1.0e9,
            pending_pounce: false,
            pending_big_jump: false,
            big_gather: false,
            quiet_leap: false,
            stream: false,
            stream_run: 0,
            stream_last: 0.0,
            bound2: None,
            land_span: 0.0,
            skid_from: 0.0,
            skid_dir: 0.0,
            motes: [None; PET_MOTES_MAX],
            mote_serial: 0,
            departure_serial: 0,
            settle_serial: 0,
            settle_seed: 0,
            mote_mark: 0,
            sit_front: false,
            peek: false,
            look_row: 0,
            gaze_dwell: 0.0,
            alpha: 0.0,
            colors: None,
            surface: None,
            worn: None,
            pending_worn: None,
            pending_arrival: PetArrival::Ceremony,
            handoff_parked_clock: None,
            arrive_t: 0.0,
            arrive_in: ARRIVE_IN,
            arrive_from: 0.0,
            departures: [None; PET_DEPARTURES_MAX],
            resight: None,
            carried: [None; PET_DEPARTURES_MAX],
            last_presence: 0.0,
            letter_veil: (1.0, None, 1.0),
            letter_cover_since: None,
            letter_rest_since: None,
            letter_rest_stepped: false,
            letter_step_seq: 0,
            letter_gone_since: None,
            veil_gather: false,
            #[cfg(any(test, debug_assertions))]
            letters_unseen: false,
            #[cfg(any(test, debug_assertions))]
            letter_arrival_dwells: false,
            #[cfg(any(test, debug_assertions))]
            letter_step_rearms: false,
            repainting: false,
            last_drawn: [0.0; PET_DEPARTURES_MAX],
            last_grid: (0, 0),
            last_pose: PetGlyphId::PetSleep0,
            last_facing: false,
            last_stance: Stance::NEUTRAL,
            adopted_look: None,
            last_cell_h: 0,
            last_cell_w: 0,
            last_reduced: false,
            col_at_tick: 0.0,
            home: 0.0,
            evicting: false,
        }
    }
}

impl PetBrain {
    /// Where the pet stands for a caret at `(row, col)`, in fractional columns of its left edge.
    #[must_use]
    #[inline]
    pub const fn station(caret_col: u16, cols: u16, width: f32) -> f32 {
        Self::station_with(caret_col, cols, width, 0.0)
    }

    #[inline]
    const fn station_with(caret_col: u16, cols: u16, width: f32, extra: f32) -> f32 {
        let right = caret_col as f32 + STATION_LEAD + extra;
        let limit = cols as f32 - width;
        if limit <= 0.0 {
            return 0.0;
        }
        if right <= limit {
            return right;
        }
        (caret_col as f32 - width - STATION_LEAD - extra).max(0.0)
    }

    /// The rhythm gate is open.
    #[inline]
    pub(super) const fn rhythm_open(&self) -> bool {
        self.run_count >= RHYTHM_MOVES
    }

    #[inline]
    fn lead(&self) -> f32 {
        self.lead_at(self.run_t)
    }

    #[inline]
    fn lead_at(&self, run_t: f32) -> f32 {
        let stand = self.stand();
        stand + self.carry(stand, run_t)
    }

    #[inline]
    const fn stand(&self) -> f32 {
        self.keep_ahead().max(self.held_lead)
    }

    #[inline]
    const fn keep_ahead(&self) -> f32 {
        if self.carrying_run() {
            (self.key_pace * LEAD_TIME)
                .clamp(0.0, LEAD_MAX - CARRY_ROOM)
                .min(self.lead_room)
        } else {
            (self.pace() * LEAD_TIME)
                .clamp(0.0, LEAD_MAX)
                .min(self.lead_room)
        }
    }

    #[inline]
    fn carry(&self, stand: f32, run_t: f32) -> f32 {
        if !self.carrying_run() {
            return 0.0;
        }
        let (walk, coast) = Self::carry_legs_from(stand);
        let due = walk / self.key_pace;
        if run_t <= due {
            self.key_pace * run_t
        } else if coast > 0.0 {
            walk + coast * (1.0 - (-(run_t - due) * self.key_pace / coast).exp())
        } else {
            walk
        }
    }

    #[inline]
    fn carry_speed(&self) -> f32 {
        if !self.carrying_run() {
            return 0.0;
        }
        let (walk, coast) = self.carry_legs();
        let due = walk / self.key_pace;
        if self.run_t <= due {
            self.key_pace
        } else if coast > 0.0 {
            self.key_pace * (-(self.run_t - due) * self.key_pace / coast).exp()
        } else {
            0.0
        }
    }

    #[inline]
    const fn carry_legs(&self) -> (f32, f32) {
        Self::carry_legs_from(self.stand())
    }

    #[inline]
    const fn carry_legs_from(stand: f32) -> (f32, f32) {
        let room = (LEAD_MAX - stand).max(0.0);
        let walk = (room - CARRY_COAST).clamp(0.0, 1.0);
        (walk, (room - walk).min(CARRY_COAST))
    }

    #[inline]
    const fn carrying_run(&self) -> bool {
        self.run_count > RHYTHM_MOVES && self.run_dir > 0 && self.key_pace > 0.0
    }

    #[inline]
    const fn begin_hide(&mut self) {
        self.hide_to = None;
        self.hiding = true;
        self.hide_t = HIDE_DWELL;
        self.set_action(PetAction::Crouch);
    }

    #[inline]
    fn hold_lead(&mut self) {
        let away = self.wall_side
            || self.pursuit_t.is_some()
            || self.play_to.is_some()
            || self.play_land.is_some()
            || self.hide_to.is_some()
            || self.hiding
            || self.room_cheer;
        self.held_lead = if away {
            0.0
        } else if self.carrying_run() {
            self.held_lead
        } else {
            self.lead()
        };
        self.held_for = self.last_caret;
    }

    #[inline]
    const fn pace(&self) -> f32 {
        self.vhat.max(self.vrun).clamp(0.0, VEL_MAX)
    }

    #[inline]
    fn station_speed(&self) -> f32 {
        let (pace, rate) = if self.carrying_run() {
            if self.wall_side {
                return 0.0;
            }
            (self.key_pace, self.carry_speed())
        } else {
            let p = self.pace();
            (p, p)
        };
        rate * ((pace - LAT_SPEED) / (RUN_SPEED - LAT_SPEED)).clamp(0.0, 1.0)
    }

    #[inline]
    const fn live_run_pace(&self) -> f32 {
        if self.rhythm_open() {
            self.run_pace
        } else {
            0.0
        }
    }

    #[inline]
    const fn top_speed(&self) -> f32 {
        (self.live_run_pace() + CATCH_UP).clamp(MAX_SPEED, TOP_SPEED_MAX)
    }

    #[inline]
    const fn run_stride_cells(speed: f32) -> f32 {
        RUN_STRIDE_CELLS * (speed.abs() / MAX_SPEED).max(1.0)
    }

    #[inline]
    const fn runs_down(&self, gap: f32) -> bool {
        self.rhythm_open() && gap * (self.run_dir as f32) > 0.0
    }

    #[inline]
    fn station_velocity(&self) -> f32 {
        if self.run_dir < 0 && self.rhythm_open() {
            let p = self.run_pace;
            return -p * ((p - LAT_SPEED) / (RUN_SPEED - LAT_SPEED)).clamp(0.0, 1.0);
        }
        self.station_speed()
    }

    #[inline]
    fn flight_eta(&self, to_col: f32, to_row: f32, cols: u16) -> f32 {
        let vertical = (to_row - self.row).abs() >= 0.5;
        let span = span_eq(to_col - self.col, to_row - self.row);
        let coil = if matches!(self.action, PetAction::Crouch) {
            (CROUCH_DUR - self.action_t).max(0.0)
        } else {
            CROUCH_DUR
        };
        if span <= POUNCE_SPAN_MAX {
            Self::flight_shape(span, vertical).0 + if vertical { 0.0 } else { coil }
        } else {
            let (dur, _) = Self::big_shape(span);
            coil + dur.max(span / quiet_speed_max(cols))
        }
    }

    fn wrap_back_aim(
        &self,
        (cr, cc): (u16, u16),
        (rows, cols): (u16, u16),
        width: f32,
    ) -> Option<(f32, f32)> {
        if !self.wrap_back || self.run_dir >= 0 || !self.rhythm_open() {
            return None;
        }
        let limit = (f32::from(cols) - width).max(0.0);
        let row = f32::from(cr);
        let mut aim = (f32::from(cc) + STATION_LEAD).min(limit);
        for _ in 0..3 {
            let eta = self.flight_eta(aim, row, cols);
            aim = ((f32::from(cc) - self.run_pace * eta).max(0.0) + STATION_LEAD).min(limit);
        }
        Some(
            self.console_station((aim, row), (rows, cols), width)
                .unwrap_or((aim, row)),
        )
    }

    #[inline]
    const fn at_the_margin(&self, (cr, cc): (u16, u16), cols: u16, width: f32, slack: f32) -> bool {
        cc as f32 + STATION_LEAD + self.standoff() + width + slack > cols as f32
            && match self.ink_span(cr as f32) {
                Some((_, end)) => end <= cc as f32 + 1.5,
                None => true,
            }
    }

    #[inline]
    const fn typing_on(&self) -> bool {
        self.run_dir > 0
            && self.rhythm_open()
            && self.run_pace > 0.0
            && self.run_step <= TYPING_STEP_MAX
            && self.watch_heat < WATCH_GATE
    }

    fn wrap_ahead_aim(
        &mut self,
        (cr, cc): (u16, u16),
        (rows, cols): (u16, u16),
        width: f32,
    ) -> Option<(f32, f32)> {
        let slack = if self.wrap_ahead.is_some() {
            WALL_RETURN_MARGIN
        } else {
            0.0
        };
        if !self.at_the_margin((cr, cc), cols, width, slack)
            || (self.wrap_ahead.is_none() && !self.typing_on())
        {
            self.wrap_ahead = None;
            return None;
        }
        if let Some(WrapAhead::Launched(col, row)) = self.wrap_ahead {
            if (row - f32::from(cr)).abs() >= 0.5
                && !self.ink_overlaps(col, row, width)
                && self
                    .console_station((col, row), (rows, cols), width)
                    .is_none()
            {
                let seat = self.wrap_rest_row(col, row, cr, (rows, cols), width);
                if seat != row {
                    self.wrap_ahead = Some(WrapAhead::Launched(col, seat));
                }
                return Some((col, seat));
            }
            self.wrap_ahead = None;
            return None;
        }
        let limit = (f32::from(cols) - width).max(0.0);
        let aim = (STATION_LEAD + self.lead()).min(limit);
        let below = (cr + 1 < rows)
            .then_some(f32::from(cr + 1))
            .filter(|&next| {
                !self.ink_overlaps(aim, next, width)
                    && self
                        .console_station((aim, next), (rows, cols), width)
                        .is_none()
            });
        let wall = (limit, f32::from(cr));
        if self.console_station(wall, (rows, cols), width).is_some() {
            self.wrap_ahead = None;
            return None;
        }
        let Some(next) = below else {
            self.wrap_ahead = Some(WrapAhead::Approach);
            return Some(wall);
        };
        let due = self.wrap_ahead == Some(WrapAhead::Due)
            || !self.typing_on()
            || f32::from(cols.saturating_sub(cc)) / self.run_pace
                <= self.flight_eta(aim, next, cols);
        if due {
            self.wrap_ahead = Some(WrapAhead::Due);
            self.quiet_leap = true;
            return Some((aim, self.wrap_rest_row(aim, next, cr, (rows, cols), width)));
        }
        self.wrap_ahead = Some(WrapAhead::Approach);
        Some(wall)
    }

    /// THE HEAD LAW AT THE WRAP: the seat a cat leaps ahead to, on the row
    /// under a line typed to the margin, stands its 1.7-row body 0.7 of a row
    /// into that line, so a typing that stops short of the wrap would leave
    /// it at rest with its head behind the line's letters, cut away by them
    /// (the trail gallery's `burst` on every theme: under `he` of the echo
    /// from the burst's end on). Once the typing has stopped, the seat
    /// steps one row further down where the line's letters lie over the body
    /// and that row is clear: the whole body then stands on blank rows, its
    /// head in the row the wrap would fill. While the typing runs the wrap is
    /// coming and the row under the line is where its next letter lands: the
    /// leap keeps it. Inert where the letter reader certifies nothing
    /// ([`Self::observed_last_letter`]) and on the screen's last row.
    fn wrap_rest_row(
        &self,
        col: f32,
        row: f32,
        caret_row: u16,
        (rows, cols): (u16, u16),
        width: f32,
    ) -> f32 {
        let lower = row + 1.0;
        if self.typing_on()
            || (row - f32::from(caret_row) - 1.0).abs() >= 0.5
            || lower >= f32::from(rows)
            || !width.is_finite()
            || width <= 0.0
        {
            return row;
        }
        let (a, b) = (col + INK_PAD - INK_GAP, col + width - INK_PAD);
        match self.observed_last_letter(f32::from(caret_row), a, b) {
            Some(Some(_))
                if self.observed_ink_overlaps(col, lower, width) == Some(false)
                    && self
                        .console_station((col, lower), (rows, cols), width)
                        .is_none() =>
            {
                lower
            }
            _ => row,
        }
    }

    #[inline]
    const fn update_wall(&mut self, caret_col: u16, cols: u16, width: f32) -> bool {
        let cols_f = cols as f32;
        if cols_f - width <= 0.0 {
            // Distinct branch.
            return false;
        }
        let edge = caret_col as f32 + STATION_LEAD + self.standoff() + width;
        let was = self.wall_side;
        if !was && edge > cols_f {
            self.wall_side = true;
        } else if was && edge < cols_f - WALL_RETURN_MARGIN {
            self.wall_side = false;
        }
        was != self.wall_side
    }

    #[inline]
    fn station_now(&self, caret_col: u16, cols: u16, width: f32) -> f32 {
        let limit = cols as f32 - width;
        if limit <= 0.0 {
            return 0.0;
        }
        if self.wall_side {
            return (caret_col as f32 - width - STATION_LEAD - self.standoff()).max(0.0);
        }
        (caret_col as f32 + STATION_LEAD + self.lead() + self.standoff()).clamp(0.0, limit)
    }

    #[inline]
    const fn standoff(&self) -> f32 {
        if self.room_lease { LEASE_STANDOFF } else { 0.0 }
    }

    #[inline]
    const fn waiting_up(&self) -> bool {
        self.room_quiet && self.vigil_armed && self.exec_since.is_some()
    }

    #[inline]
    const fn heat_frozen(&self) -> bool {
        self.room_quiet && !self.last_burst
    }

    /// Choose which animal the pet is drawn as ([`PetSpecies`]).
    #[inline]
    pub const fn set_species(&mut self, species: PetSpecies) {
        self.species = species;
    }

    /// The animal currently being drawn.
    #[must_use]
    #[inline]
    pub const fn species(&self) -> PetSpecies {
        self.species
    }

    /// The host's per-frame ink probe.
    pub fn sense_ink(&mut self, base_row: u16, spans: &[(u16, u16)], live_row: Option<u16>) {
        if self.ink_base == base_row && self.ink_spans.as_slice() == spans {
            self.ink_scrolled = false;
            self.ink_live = live_row;
            return;
        }
        self.ink_scrolled = self.ink_base == base_row
            && !self.ink_spans.is_empty()
            && self.ink_spans.len() == spans.len()
            && self.ink_spans[1..] == spans[..spans.len() - 1]
            && self.ink_spans.iter().any(|&(f, e)| e > f);
        self.ink_base = base_row;
        self.ink_spans.clear();
        self.ink_spans.extend_from_slice(spans);
        self.ink_live = live_row;
    }

    /// Retire every fact that depends on the current terminal surface.
    pub fn retire_coordinate_space(&mut self) {
        let species = self.species;
        let content = self.content;
        let quiet = self.quiet;
        let clock = self.clock;
        let worn = self.pending_worn.or(self.worn);
        let mote_serial = self.mote_serial;
        let departure_serial = self.departure_serial;
        let mote_mark = self.mote_mark;
        let last_frolic = self.last_frolic;
        let bored_cool = self.bored_cool;
        let trick_content_at = self.trick_content_at;
        let mut stroke = std::mem::take(&mut self.stroke);
        if let Some(now) = self.last_now {
            stroke.sample(now, None, 0.0);
        }
        let mut ink_spans = std::mem::take(&mut self.ink_spans);
        ink_spans.clear();
        let console = self.console.retired();
        let left = self
            .held_for
            .filter(|_| self.held_lead > 0.0)
            .map(|caret| HeldStand {
                surface: self.surface,
                caret,
                lead: self.held_lead,
            });
        let mut others = self
            .held_stash
            .into_iter()
            .flatten()
            .filter(|s| s.surface != self.surface);
        let held_stash = match left {
            Some(stand) => [Some(stand), others.next()],
            None => [others.next(), others.next()],
        };

        *self = Self {
            species,
            content,
            quiet,
            clock,
            action: if quiet >= SLEEP_AFTER {
                PetAction::Sleep
            } else {
                PetAction::Sit
            },
            worn,
            mote_serial,
            departure_serial,
            mote_mark,
            last_frolic,
            bored_cool,
            trick_content_at,
            stroke,
            ink_spans,
            console,
            held_stash,
            ..Self::default()
        };
    }

    /// The same surface, re-gridded.
    pub fn rebase_geometry(&mut self) {
        self.regrid_caret = self.last_caret.or(self.regrid_caret);
        self.last_caret = None;
        self.ink_base = 0;
        self.ink_spans.clear();
        self.ink_live = None;
        self.ink_scrolled = false;
        self.last_pointer = None;
        self.pending_pointer_pounce = None;
    }

    fn carry_with_regrid(&mut self, dy: f32, rows: u16) -> bool {
        let last = f32::from(rows.saturating_sub(1));
        let fits = |row: f32| (0.0..=last).contains(&(row + dy));
        let flight_fits = self
            .flight
            .as_ref()
            .is_none_or(|f| fits(f.from_row) && fits(f.to_row));
        let bound2_fits = self.bound2.is_none_or(|(_, r2, _)| fits(r2));
        if !fits(self.row) || !flight_fits || !bound2_fits {
            return false;
        }
        if dy == 0.0 {
            return true;
        }
        self.row += dy;
        if let Some(f) = self.flight.as_mut() {
            f.from_row += dy;
            f.to_row += dy;
        }
        if let Some((_, r2, _)) = self.bound2.as_mut() {
            *r2 += dy;
        }
        if let Some(look) = self.adopted_look.as_mut() {
            look.seat.1 += dy;
        }
        if let Some((_, tr)) = self.play_to.as_mut() {
            *tr = (*tr + dy).clamp(0.0, last);
        }
        if let Some(PlayLand::BatSwipe { row, .. }) = self.play_land.as_mut() {
            *row = (*row + dy).clamp(0.0, last);
        }
        if let Some((_, br)) = self.pending_bat.as_mut() {
            *br = (*br + dy).clamp(0.0, last);
        }
        if let Some((_, sr)) = self.room_cheer_stage.as_mut() {
            *sr = (*sr + dy).clamp(0.0, last);
        }
        if let Some((rr, _)) = self.console.repair_target.as_mut() {
            *rr = (*rr + dy).clamp(0.0, last);
        }
        if let Some((_, lr)) = self.console.last_seat.as_mut() {
            *lr = (*lr + dy).clamp(0.0, last);
        }
        if let Some((sc, sr)) = self.console.rest_seat.get() {
            self.console
                .rest_seat
                .set(Some((sc, (sr + dy).clamp(0.0, last))));
        }
        if let Some((_, row)) = self.deferred_hidden_landing.as_mut() {
            *row = (*row + dy).clamp(0.0, last);
        }
        if let Some(target) = self.console.target.as_mut() {
            target.row = (target.row + dy).clamp(0.0, last);
        }
        if let Some(trip) = self.console.trip.as_mut() {
            trip.from.1 = (trip.from.1 + dy).clamp(0.0, last);
            trip.to.1 = (trip.to.1 + dy).clamp(0.0, last);
        }
        for d in self
            .departures
            .iter_mut()
            .flatten()
            .filter(|d| !d.resight && d.own_glass.is_none())
        {
            d.row = (d.row + dy).clamp(0.0, last);
        }
        for m in self.motes.iter_mut().flatten() {
            m.row += dy;
        }
        true
    }

    /// The re-sighting crossfade.
    pub fn resight_coordinate_space(&mut self) {
        let carried = if self.carried.iter().any(Option::is_some) {
            self.carried
        } else {
            let mut carried = [None; PET_DEPARTURES_MAX];
            let live = self
                .departures
                .iter()
                .zip(self.last_drawn)
                .filter_map(|(d, drawn)| d.map(|d| (d, drawn)));
            for (slot, (d, presence)) in carried.iter_mut().zip(live) {
                let age = (self.lane_clock - d.born) as f32;
                if age < d.life() && presence > 0.0 {
                    let ghost = d.stopped_at(age);
                    let own_clear = match ghost.own_glass {
                        Some(_) => ghost.own_clear,
                        None => self
                            .console_still_rect_verdict(ghost.from_col, ghost.row, ghost.stance0)
                            .unwrap_or(ghost.own_clear),
                    };
                    *slot = Some(Carried {
                        ghost: Departure { own_clear, ..ghost },
                        presence,
                        home: false,
                    });
                }
            }
            carried
        };
        let pending = self.resight.take().or_else(|| {
            let (coat, iris) = self.worn?;
            let at = self.last_now?;
            (self.alpha > 0.0 && self.last_presence > 0.0).then(|| PendingResight {
                ghost: Departure {
                    born: 0.0,
                    id: self.next_departure_id(),
                    coat,
                    iris,
                    from_col: self.col,
                    row: self.row,
                    edge: self.col,
                    speed: DEPART_SPEED,
                    still: true,
                    facing_left: self.last_facing,
                    stride0: self.stride,
                    pose0: self.last_pose,
                    stance0: self.last_stance,
                    resight: true,
                    own_glass: None,
                    own_clear: self.console_still_body_clear(),
                    peak: self.last_presence,
                    colors: self.colors,
                    surface: self.surface,
                },
                at,
            })
        });
        self.retire_coordinate_space();
        self.resight = pending;
        self.carried = carried;
    }

    /// Drop a pending re-sighting ghost.
    #[inline]
    pub const fn forget_resight(&mut self) {
        self.resight = None;
    }

    /// The focus went back to the pane carried ghosts fade in ([`Self::displace_resight`]).
    #[inline]
    pub const fn come_home(&mut self, glass: u32) {
        let mut i = 0;
        while i < self.carried.len() {
            if let Some(carried) = &mut self.carried[i]
                && matches!(carried.ghost.own_glass, Some(own) if own == glass)
            {
                carried.home = true;
            }
            i += 1;
        }
    }

    /// The pending re-sighting ghost's surface left focus.
    #[inline]
    pub const fn displace_resight(&mut self, glass: u32) {
        if let Some(pending) = self.resight.take() {
            self.resight = Some(PendingResight {
                ghost: Departure {
                    own_glass: Some(glass),
                    ..pending.ghost
                },
                ..pending
            });
        }
    }

    /// Drop every ghost standing on own glass `glass`, pending, carried or fading.
    #[inline]
    pub const fn forget_own_glass(&mut self, glass: u32) {
        if matches!(
            self.resight,
            Some(pending) if matches!(pending.ghost.own_glass, Some(own) if own == glass)
        ) {
            self.resight = None;
        }
        let mut i = 0;
        while i < self.carried.len() {
            if matches!(
                self.carried[i],
                Some(c) if matches!(c.ghost.own_glass, Some(own) if own == glass)
            ) {
                self.carried[i] = None;
            }
            i += 1;
        }
        let mut j = 0;
        while j < self.departures.len() {
            if matches!(
                self.departures[j],
                Some(d) if matches!(d.own_glass, Some(own) if own == glass)
            ) {
                self.departures[j] = None;
            }
            j += 1;
        }
    }

    /// A ghost stands, or is about to stand, on its own glass ([`Self::displace_resight`]).
    #[must_use]
    #[inline]
    pub const fn has_own_glass_ghost(&self) -> bool {
        if matches!(self.resight, Some(pending) if pending.ghost.own_glass.is_some()) {
            return true;
        }
        let mut i = 0;
        while i < self.carried.len() {
            if matches!(self.carried[i], Some(c) if c.ghost.own_glass.is_some()) {
                return true;
            }
            i += 1;
        }
        let mut j = 0;
        while j < self.departures.len() {
            if matches!(self.departures[j], Some(d) if d.own_glass.is_some()) {
                return true;
            }
            j += 1;
        }
        false
    }

    /// A ghost stands, is carried to stand, or is about to stand on own glass `glass`
    /// ([`Self::displace_resight`]).
    #[must_use]
    #[inline]
    pub const fn has_ghost_on_glass(&self, glass: u32) -> bool {
        if matches!(
            self.resight,
            Some(pending) if matches!(pending.ghost.own_glass, Some(own) if own == glass)
        ) {
            return true;
        }
        let mut i = 0;
        while i < self.carried.len() {
            if matches!(
                self.carried[i],
                Some(c) if matches!(c.ghost.own_glass, Some(own) if own == glass)
            ) {
                return true;
            }
            i += 1;
        }
        let mut j = 0;
        while j < self.departures.len() {
            if matches!(
                self.departures[j],
                Some(d) if matches!(d.own_glass, Some(own) if own == glass)
            ) {
                return true;
            }
            j += 1;
        }
        false
    }

    fn spend_crossfade(
        &mut self,
        pending: PendingResight,
        carried: [Option<Carried>; PET_DEPARTURES_MAX],
    ) {
        let displaced = pending.ghost.own_glass;
        let glass = |c: &Carried| match (c.ghost.own_glass, displaced) {
            (Some(_), _) if c.home => Some(None),
            (Some(own), _) => Some(Some(own)),
            (None, None) => Some(None),
            (None, Some(left)) => c.ghost.own_clear.then_some(Some(left)),
        };
        let seat = (self.col, self.row);
        let adopted = carried
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.map(|c| (i, c)))
            .filter(|(_, c)| glass(c) == Some(None))
            .filter(|(_, c)| self.worn == Some((c.ghost.coat, c.ghost.iris)))
            .filter(|(_, c)| {
                (c.ghost.from_col - seat.0).abs() <= ADOPT_REACH
                    && (c.ghost.row - seat.1).abs() < 0.5
            })
            .max_by(|(_, a), (_, b)| a.presence.total_cmp(&b.presence));
        if let Some((_, body)) = adopted {
            self.col = body.ghost.from_col;
            self.row = body.ghost.row;
            self.facing_left = body.ghost.facing_left;
            if let Some(colors) = body.ghost.colors
                && body.ghost.surface.is_some()
                && body.ghost.surface == self.surface
            {
                self.colors = Some(colors);
            }
        }
        self.adopted_look = adopted.map(|(_, body)| AdoptedLook {
            pose: body.ghost.pose0,
            facing_left: body.ghost.facing_left,
            stance: body.ghost.stance0,
            eased: 0.0,
            seat: (self.col, self.row),
            beneath: None,
        });
        self.alpha = 1.0;
        self.arrive_t = HOMECOMING_IN;
        self.arrive_in = HOMECOMING_IN;
        let mut handed = adopted.map_or(0.0, |(_, body)| body.presence);
        if pending.ghost.own_glass.is_none() || pending.ghost.own_clear {
            self.spawn_departure(Departure {
                born: self.lane_clock,
                ..pending.ghost
            });
        } else {
            handed += pending.ghost.peak;
        }
        let mut rest = [None; PET_DEPARTURES_MAX];
        for (i, (slot, c)) in rest.iter_mut().zip(carried).enumerate() {
            let Some(c) = c.filter(|_| adopted.is_none_or(|(adopted, _)| adopted != i)) else {
                continue;
            };
            match glass(&c) {
                Some(own) => *slot = Some((c, own)),
                None => handed += c.presence,
            }
        }
        rest.sort_by(|a: &Option<(Carried, Option<u32>)>, b| {
            let presence = |s: &Option<(Carried, Option<u32>)>| s.map_or(-1.0, |(c, _)| c.presence);
            presence(b).total_cmp(&presence(a))
        });
        for (c, own_glass) in rest.into_iter().flatten() {
            if self.departures.iter().all(Option::is_some) {
                handed += c.presence;
                continue;
            }
            self.spawn_departure(Departure {
                born: self.lane_clock,
                resight: true,
                own_glass,
                peak: c.presence,
                ..c.ghost
            });
        }
        self.arrive_from = handed.min(1.0);
    }

    /// True from a failed command's note until its droop has finished playing.
    #[must_use]
    #[inline]
    pub fn grieving(&self) -> bool {
        self.pending_sulk
            || (!self.droop_brief
                && (self.action == PetAction::Droop
                    || matches!(self.resume, Some((PetAction::Droop, _)))))
            || self.console_grieving()
    }

    #[inline]
    const fn ink_span_idx(&self, r: i64) -> Option<(f32, f32)> {
        let Some(diff) = r.checked_sub(self.ink_base as i64) else {
            return None;
        };
        if diff < 0 {
            return None;
        }
        let idx = diff as usize;
        let spans = self.ink_spans.as_slice();
        if idx >= spans.len() {
            return None;
        }
        let (first, end) = spans[idx];
        if end > first {
            Some((first as f32, end as f32))
        } else {
            None
        }
    }

    #[inline]
    const fn ink_span(&self, row: f32) -> Option<(f32, f32)> {
        if !row.is_finite() || row < -0.5 {
            return None;
        }
        self.ink_span_idx(row.round() as i64)
    }

    #[inline]
    const fn caret_at_home(&self, row: u16, col: u16) -> bool {
        if col as f32 <= WRAP_SLOP {
            return true;
        }
        match self.ink_span(row as f32) {
            Some((first, end)) => first <= WRAP_SLOP && end <= col as f32 + 0.5,
            None => false,
        }
    }

    #[inline]
    const fn ink_overlaps(&self, col: f32, row: f32, width: f32) -> bool {
        let Some((first, end)) = self.ink_span(row) else {
            return false;
        };
        let (a, b) = (col + INK_PAD, col + width - INK_PAD);
        b > first
            && a < end
            && match self.observed_ink_overlaps(col, row, width) {
                Some(overlaps) => overlaps,
                None => true,
            }
    }

    #[inline]
    const fn ink_safe_col(&self, want: f32, row: f32, width: f32, cols: u16) -> f32 {
        self.ink_safe_col_for(want, row, width, cols, None)
    }

    pub(super) const fn ink_safe_col_for(
        &self,
        want: f32,
        row: f32,
        width: f32,
        cols: u16,
        caret: Option<(u16, u16)>,
    ) -> f32 {
        const fn clears_caret(caret_col: Option<f32>, col: f32, width: f32) -> bool {
            match caret_col {
                Some(cc) => col >= cc + STATION_LEAD || col + width <= cc - STATION_LEAD,
                None => true,
            }
        }

        if !self.ink_overlaps(want, row, width) {
            return want;
        }
        let Some((first, end)) = self.ink_span(row) else {
            return want;
        };
        let limit = (cols as f32 - width).max(0.0);
        let active_caret = match caret {
            Some(c) => Some(c),
            None => self.last_caret,
        };
        let caret_col = match active_caret {
            Some((cr, cc)) if (cr as f32 - row).abs() < 0.5 => Some(cc as f32),
            _ => None,
        };
        let right = end + INK_GAP;
        let left = first - width - INK_GAP;
        let right_ok = right <= limit && clears_caret(caret_col, right, width);
        let left_ok = left >= 0.0 && clears_caret(caret_col, left, width);
        let beside = match (right_ok, left_ok) {
            (true, true) if (left - want).abs() < (right - want).abs() => left,
            (true, _) => right,
            (false, true) => left,
            (false, false) => want,
        };
        if (beside - want).abs() > INK_EVICT_MAX
            || !clears_caret(caret_col, beside, width)
            || self.ink_overlaps(beside, row, width)
        {
            let right_base = (want + INK_PAD).floor();
            let left_base = (want + width - INK_PAD).ceil() - width;
            let mut fallback = None;
            let max_step = INK_EVICT_MAX as u16;
            let mut step_u16 = 1u16;
            while step_u16 <= max_step {
                let step = step_u16 as f32;
                step_u16 += 1;
                let candidates = [right_base + step, left_base - step];
                let mut ci = 0;
                while ci < 2 {
                    let candidate = candidates[ci];
                    ci += 1;
                    if candidate >= 0.0
                        && candidate <= limit
                        && clears_caret(caret_col, candidate, width)
                        && (candidate - want).abs() <= INK_EVICT_MAX
                        && matches!(
                            self.observed_ink_overlaps(candidate, row, width),
                            Some(false)
                        )
                    {
                        if row < 1.0
                            || matches!(
                                self.observed_ink_overlaps(candidate, row - 1.0, width),
                                Some(false)
                            )
                        {
                            return candidate;
                        }
                        if fallback.is_none() {
                            fallback = Some(candidate);
                        }
                    }
                }
            }
            if let Some(candidate) = fallback {
                return candidate;
            }
            if !clears_caret(caret_col, beside, width) || self.ink_overlaps(beside, row, width) {
                let mut fallback = None;
                let mut step_u16 = INK_EVICT_MAX as u16 + 1;
                while step_u16 < cols {
                    let step = step_u16 as f32;
                    step_u16 += 1;
                    let candidates = [right_base + step, left_base - step];
                    let mut ci = 0;
                    while ci < 2 {
                        let candidate = candidates[ci];
                        ci += 1;
                        if candidate >= 0.0
                            && candidate <= limit
                            && clears_caret(caret_col, candidate, width)
                            && matches!(
                                self.observed_ink_overlaps(candidate, row, width),
                                Some(false)
                            )
                        {
                            if row < 1.0
                                || matches!(
                                    self.observed_ink_overlaps(candidate, row - 1.0, width),
                                    Some(false)
                                )
                            {
                                return candidate;
                            }
                            if fallback.is_none() {
                                fallback = Some(candidate);
                            }
                        }
                    }
                }
                if let Some(candidate) = fallback {
                    return candidate;
                }
            }
        }
        beside
    }

    #[inline]
    const fn ink_stand(&self, want: f32, row: f32, width: f32, cols: u16, rows: u16) -> (f32, f32) {
        self.ink_stand_for(want, row, width, cols, rows, None)
    }

    pub(super) const fn ink_stand_for(
        &self,
        want: f32,
        row: f32,
        width: f32,
        cols: u16,
        rows: u16,
        caret: Option<(u16, u16)>,
    ) -> (f32, f32) {
        if !self.ink_overlaps(want, row, width) {
            return (want, row);
        }
        let beside = self.ink_safe_col_for(want, row, width, cols, caret);
        if (beside - want).abs() <= INK_EVICT_MAX && !self.ink_overlaps(beside, row, width) {
            return (beside, row);
        }
        let rows_f = rows as f32;
        let adj = [row + 1.0, row - 1.0];
        let mut i = 0;
        while i < 2 {
            let r = adj[i];
            i += 1;
            if r >= 0.0 && r < rows_f && !self.ink_overlaps(want, r, width) {
                return (want, r);
            }
        }
        let mut j = 0;
        while j < 2 {
            let r = adj[j];
            j += 1;
            if r >= 0.0 && r < rows_f {
                let near = self.ink_safe_col_for(want, r, width, cols, caret);
                if (near - want).abs() <= INK_EVICT_MAX
                    && matches!(self.observed_ink_overlaps(near, r, width), Some(false))
                {
                    return (near, r);
                }
            }
        }
        (beside, row)
    }

    #[inline]
    const fn is_re_anchor(from_col: f32, to_col: f32, from_row: f32, to_row: f32) -> bool {
        (to_col - from_col).abs() > INK_EVICT_MAX || (to_row - from_row).abs() >= 0.5
    }

    #[inline]
    const fn evict_toward(from: f32, to: f32, dt: f32) -> f32 {
        let delta = to - from;
        let step = INK_EVICT_SPEED * dt.max(0.0);
        if delta.abs() <= step {
            to
        } else {
            from + delta.signum() * step
        }
    }

    const fn caret_delta(
        prev: (u16, u16),
        now: (u16, u16),
        cols: u16,
        wrapped: bool,
        home: bool,
        scrolled: bool,
    ) -> (f32, f32, bool) {
        let dr = now.0 as f32 - prev.0 as f32;
        let dc = now.1 as f32 - prev.1 as f32;
        let w = cols as f32;
        let bar = wrap_bar(w);
        if dr.abs() == 1.0 && dc.signum() != dr.signum() && dc.abs() >= bar {
            (0.0, dc + dr * w, true)
        } else if wrapped && dr == 0.0 && dc.signum() < 0.0 && dc.abs() >= bar {
            (1.0, dc + w, true)
        } else if dr == 0.0 && dc <= -STARTLE_COLS && dc > -SNAP_COLS && (home || scrolled) {
            (1.0, dc, true)
        } else {
            (dr, dc, false)
        }
    }

    #[inline]
    fn station_safe(&self, caret: (u16, u16), cols: u16, rows: u16, width: f32) -> (f32, f32) {
        let want = self.station_now(caret.1, cols, width);
        let stand = self.ink_stand_for(want, f32::from(caret.0), width, cols, rows, Some(caret));
        let stand = self.clear_of_letters_above(stand, caret, cols, width);
        if let Some(station) = self.console_station(stand, (rows, cols), width) {
            return station;
        }
        stand
    }

    /// THE HEAD LAW: a cat standing beside the caret never rests its head on a line of letters
    /// ([`Self::observed_last_letter`], [`INK_GAP`], [`Self::veil_over_letters`]).
    const fn clear_of_letters_above(
        &self,
        (col, row): (f32, f32),
        caret: (u16, u16),
        cols: u16,
        width: f32,
    ) -> (f32, f32) {
        let above = row - 1.0;
        if above < 0.0
            || (row - caret.0 as f32).abs() >= 0.5
            || col < caret.1 as f32
            || !width.is_finite()
            || width <= 0.0
            || self.console_executing()
        {
            return (col, row);
        }
        let limit = (cols as f32 - width).max(0.0);
        let mut x = col;
        let mut step = 0u16;
        while step < cols {
            step += 1;
            let a = x + INK_PAD - INK_GAP;
            let b = x + width - INK_PAD;
            match self.observed_last_letter(above, a, b) {
                None => return (col, row),
                Some(None) => {
                    if x > limit || self.ink_overlaps(x, row, width) {
                        return (col, row);
                    }
                    return (x, row);
                }
                Some(Some(last)) => {
                    x = last as f32 + 1.0 + INK_GAP - INK_PAD;
                    if x > limit {
                        return (col, row);
                    }
                }
            }
        }
        (col, row)
    }

    /// THE HEAD LAW IN FLIGHT ([`HEAD_REACH_MAX`]).
    const fn arc_under_letters(&self, to_col: f32, to_row: f32, width: f32, arc: f32) -> f32 {
        if (to_row - self.row).abs() >= 0.5 || !width.is_finite() || width <= 0.0 {
            return arc;
        }
        let over = self.row.round() - 2.0;
        let (a, b) = (
            self.col.min(to_col) + INK_PAD,
            self.col.max(to_col) + width - INK_PAD,
        );
        let ceiling = 2.0 - ART_ROWS + HEAD_REACH_MAX;
        match self.observed_last_letter(over, a, b) {
            Some(Some(_)) if arc > ceiling => ceiling.max(0.0),
            _ => arc,
        }
    }

    /// The live output edge as a row coordinate.
    #[inline]
    const fn ink_live_row(&self) -> Option<f32> {
        match self.ink_live {
            Some(r) => Some(r as f32),
            None => None,
        }
    }

    const fn zee_ink_fade(&self, col: f32, row: f32) -> f32 {
        if !row.is_finite() {
            return 1.0;
        }
        let band = row.floor() as i64;
        if let Some((first, end)) = self.ink_span_idx(band)
            && col >= first - INK_PAD
            && col <= end + INK_PAD
        {
            return 0.0;
        }
        let mut k = 1i64;
        while k <= 2 {
            if let Some((first, end)) = self.ink_span_idx(band - k)
                && col >= first - INK_PAD
                && col <= end + INK_PAD
            {
                let edge = (band - k + 1) as f32;
                return ((row - edge) / ZEE_INK_FADE_BAND).clamp(0.0, 1.0);
            }
            k += 1;
        }
        1.0
    }

    #[inline]
    const fn mote_px_cell(&self, col: f32, row: f32) -> (f32, f32) {
        let cw = (if self.last_cell_w > 1 {
            self.last_cell_w
        } else {
            1
        }) as f32;
        let ch = (if self.last_cell_h > 1 {
            self.last_cell_h
        } else {
            1
        }) as f32;
        ((col * cw).round() / cw, (row * ch).round() / ch)
    }

    /// The terminal bel rang in the chased pane.
    #[inline]
    pub const fn note_bell(&mut self, now: Instant) {
        self.pending_bell = Some(now);
    }

    /// A shell command completed in the focused pane (OSC 133/633 d, deduped by the host).
    pub fn note_command_done(&mut self, now: Instant, failed: bool, dur_ms: Option<u64>) {
        let pet_talk = self.trick_submit_at.take().is_some_and(|at| {
            now.saturating_duration_since(at).as_secs_f32() <= TRICK_SUBMIT_FORGIVEN
        });
        let forgiven_trick = failed && pet_talk && dur_ms.is_none_or(|dur| dur < CHEER_MIN_MS);
        if !forgiven_trick {
            self.note_console_completion(now, failed);
        }
        let vigil = self.vigil && self.action == PetAction::Crouch && self.vigil_t >= VIGIL_AFTER;
        self.exec_since = None;
        self.vigil_armed = false;
        self.vigil_t = 0.0;
        if forgiven_trick {
            return;
        }
        if failed {
            self.content = (self.content - SULK_CONTENT).max(0.0);
            self.pending_sulk = true;
            self.sulk_at = Some(self.clock);
            return;
        }
        let forgiven = self
            .sulk_at
            .is_some_and(|at| (self.clock - at) as f32 <= SULK_FORGIVEN);
        match dur_ms {
            Some(dur) if dur >= CHEER_MIN_MS => {
                self.content = (self.content + CHEER_CONTENT).min(1.0);
                self.pending_cheer = Some(forgiven || dur >= CHEER_BIG_MS);
                self.pending_vigil_pounce = vigil;
            }
            _ if forgiven => {
                self.content = (self.content + CHEER_CONTENT).min(1.0);
                self.pending_cheer = Some(true);
                self.pending_vigil_pounce = vigil;
            }
            _ => {
                // Under two seconds (or no timestamps at all): the ledger only.
                self.content = (self.content + CHEER_FAST_CONTENT).min(1.0);
            }
        }
        if forgiven {
            self.sulk_at = None;
        }
    }

    /// The shell is executing.
    pub const fn note_executing(&mut self, now: Instant, executing: bool) {
        if executing {
            if self.exec_since.is_none() {
                self.exec_since = Some(now);
            }
        } else {
            self.exec_since = None;
            self.vigil_armed = false;
        }
    }

    /// The host is presenting a frame the program has not finished.
    #[inline]
    pub const fn note_repainting(&mut self, repainting: bool) {
        self.repainting = repainting;
    }

    /// The surface this frame's body stands on ([`Self::surface`]).
    #[inline]
    pub const fn note_surface(&mut self, surface: (u64, bool)) {
        self.surface = Some(surface);
    }

    /// The chased pane is on the alternate screen ([`Self::note_executing`]).
    #[inline]
    pub const fn note_full_screen(&mut self, full_screen: bool) {
        if full_screen && !self.full_screen {
            self.watch_heat = 0.0;
            self.watch_t = 0.0;
            self.watch_spent = false;
            self.vigil_armed = false;
        }
        self.full_screen = full_screen;
    }

    /// The user clicked or gently stroked the cat.
    #[inline]
    pub const fn note_petted(&mut self, now: Instant) {
        let next = self.pending_pet.saturating_add(1);
        self.pending_pet = if next < PET_LATCH_MAX {
            next
        } else {
            PET_LATCH_MAX
        };
        self.pet_at = Some(now);
    }

    /// A kitty command was typed.
    pub fn note_trick(&mut self, now: Instant, trick: Trick, confirmed: bool) {
        let heard = self.trick_heard.take();
        if confirmed
            && self.pending_trick.is_none()
            && self.trick_go.is_none()
            && heard.is_some_and(|(t, at)| {
                t == trick && now.saturating_duration_since(at).as_secs_f32() <= TRICK_CAP
            })
        {
            return;
        }
        self.pending_trick = Some(match self.pending_trick {
            Some(p) if p.trick == trick => PendingTrick {
                confirmed: p.confirmed || confirmed,
                stamped: now,
                ..p
            },
            _ => PendingTrick {
                trick,
                confirmed,
                noted: now,
                stamped: now,
            },
        });
    }

    /// The line turned into prose or was aborted.
    pub const fn revoke_trick(&mut self) {
        if matches!(self.pending_trick, Some(p) if !p.confirmed) {
            self.pending_trick = None;
        }
        if let Some((heard, _)) = self.trick_heard.take()
            && matches!(self.trick_go, Some(go) if go as u8 == heard as u8)
        {
            self.trick_go = None;
        }
    }

    /// The whole submitted line was pet talk (`sit` + Enter).
    #[inline]
    pub const fn note_trick_submit(&mut self, now: Instant) {
        self.trick_submit_at = Some(now);
    }

    /// The latched, not-yet-heard trick, for host tests.
    #[must_use]
    #[inline]
    pub const fn pending_trick(&self) -> Option<Trick> {
        match self.pending_trick {
            Some(p) => Some(p.trick),
            None => None,
        }
    }

    /// The hand entered flow ([`crate::cursor_glow::CursorGlow::take_flow_entry`]).
    #[inline]
    pub const fn note_flow(&mut self, now: Instant) {
        self.pending_flow = Some(now);
    }

    /// Receive the rainbow kitty v2 per-frame offer (`rainbow_kitty::companion`).
    ///
    /// ```
    /// # use aterm_effects::{cursor_glow::Geom, kitty_pet::{PetBrain, PetFrame}, rainbow_kitty::{Engine, companion}};
    /// # use aterm_time::Instant;
    /// # fn offer_after_tick(v2: &mut Engine, pet: &mut PetBrain, pet_frame: &PetFrame, geom: Geom, now: Instant) {
    /// let offer = v2.pet_offer(geom, companion::PetOnGlass::of(pet_frame, geom));
    /// if let Some(star) = pet.note_v2_offer(&offer) { v2.catch_star(star, now); }
    /// # }
    /// ```
    pub fn note_v2_offer(&mut self, offer: &PetOffer) -> Option<StarCatch> {
        self.mote_rgb = offer.mote_rgb;
        if let Some(at) = offer.perk_at
            && self.perk_spent != Some(at)
        {
            self.pending_perk = Some(at);
        }
        if let Some(star) = offer.catch
            && self.catch_cool <= 0.0
        {
            self.pending_catch = Some(star);
        }
        self.caught.take()
    }

    /// The v2 offer's latches, `(perk, catch)`, for host tests.
    #[must_use]
    #[inline]
    pub const fn pending_v2_offer(&self) -> (bool, bool) {
        (self.pending_perk.is_some(), self.pending_catch.is_some())
    }

    /// A controller lease is held.
    #[inline]
    pub const fn note_room_lease(&mut self, held: bool) {
        self.room_lease = held;
    }

    /// The session phase is `Quiet`.
    #[inline]
    pub const fn note_room_quiet(&mut self, quiet: bool) {
        self.room_quiet = quiet;
    }

    /// The newest turn record (`TurnRecord::id`, `status == "settled"`), restated every frame.
    #[inline]
    pub const fn note_room_turn(&mut self, now: Instant, id: u64, settled: bool) {
        let seeded = core::mem::replace(&mut self.room_turn_seeded, true);
        if id == self.room_turn || !seeded {
            self.room_turn = id;
            return;
        }
        self.room_turn = id;
        if settled {
            self.content = (self.content + CHEER_FAST_CONTENT).min(1.0);
            self.pending_room = Some(RoomCue::Nod);
        } else {
            self.pending_room = Some(RoomCue::EarFlat);
        }
        self.room_at = Some(now);
    }

    /// The session's task came down.
    #[inline]
    pub const fn note_room_landed(&mut self) {
        self.content = (self.content + CHEER_CONTENT).min(1.0);
        self.pending_cheer = Some(true);
        self.room_cheer = true;
        self.room_cheer_trips = 0;
        self.room_cheer_stage = None;
        self.room_cheer_air = None;
        self.room_cheer_leaving = false;
    }

    #[inline]
    const fn drop_room_cheer(&mut self) {
        self.room_cheer = false;
        self.room_cheer_air = None;
        self.room_cheer_leaving = false;
    }

    #[cfg(test)]
    fn cheer_motes_aloft(&self) -> usize {
        self.motes
            .iter()
            .flatten()
            .filter(|m| {
                matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart)
                    && (0.0..1.0).contains(&m.age(self.clock))
            })
            .count()
    }

    #[inline]
    const fn cheering(&self) -> bool {
        self.room_cheer && self.room_cheer_in_play()
    }

    #[inline]
    const fn room_cheer_in_play(&self) -> bool {
        if self.room_cheer_leaving
            || self.pending_cheer.is_some()
            || matches!(
                self.action,
                PetAction::Waking | PetAction::Perk | PetAction::Frolic
            )
        {
            return true;
        }
        let mut i = 0;
        while i < self.motes.len() {
            if matches!(
                self.motes[i],
                Some(m) if matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart)
            ) {
                return true;
            }
            i += 1;
        }
        false
    }

    /// A test's negative control ([`Self::observed_last_letter`], [`Self::clear_of_letters_above`],
    /// [`Self::arc_under_letters`], [`Self::veil_over_letters`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letters_unseen(&mut self, unseen: bool) {
        self.letters_unseen = unseen;
    }

    /// A test's negative control: the rest veil ([`Self::veil_over_letters`])
    /// waits [`LETTER_VEIL_DWELL`] on letters an Enter printed and then eases
    /// on over [`LETTER_VEIL_REST_ON_S`], as it did before it stepped on at
    /// once for them.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_arrival_dwells(&mut self, dwells: bool) {
        self.letter_arrival_dwells = dwells;
    }

    /// A test's negative control: the rest veil ([`Self::veil_over_letters`])
    /// steps on for letters under a body at rest whenever the hand's last
    /// key was an Enter, however long ago and however often they come back,
    /// with no hold, as it did before it stepped once an Enter.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_step_rearms(&mut self, rearms: bool) {
        self.letter_step_rearms = rearms;
    }

    /// Whether a cheer is latched and not yet played or wiped.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub const fn cheer_owed(&self) -> bool {
        self.pending_cheer.is_some()
    }

    /// Whether another pane in a split is streaming this frame.
    pub const fn note_room_sibling(&mut self, now: Instant, talking: Option<(f32, f32)>) {
        let level = matches!(talking, Some((c, r)) if c.is_finite() && r.is_finite());
        let edge = level && !self.room_sibling;
        self.room_sibling = level;
        if edge && let Some((col, _)) = talking {
            self.pending_room = Some(RoomCue::Look { col });
            self.room_at = Some(now);
        }
    }

    /// The inbox ([`RoomInbox`]), restated every frame.
    #[inline]
    pub const fn note_room_inbox(&mut self, now: Instant, inbox: RoomInbox) {
        self.room_hold = inbox.hold;
        let seeded = core::mem::replace(&mut self.room_inbox_seeded, true);
        let mail = inbox.unseen > self.room_unseen;
        let overdue = inbox.overdue && !self.room_overdue;
        self.room_unseen = inbox.unseen;
        self.room_overdue = inbox.overdue;
        if !seeded {
            return;
        }
        if overdue {
            self.pending_room = Some(RoomCue::EarFlat);
            self.room_at = Some(now);
        } else if mail {
            self.pending_room = Some(RoomCue::Mail);
            self.room_at = Some(now);
        }
    }

    /// Queued, not-yet-consumed pets.
    #[must_use]
    #[inline]
    pub const fn pending_pets(&self) -> u8 {
        self.pending_pet
    }

    /// An ambient word-cat peek landed with its head at `(col, row)`.
    pub const fn note_peek(&mut self, now: Instant, col: f32, row: f32) {
        // Non-finite coordinates never latch: NaN passes no range gate and
        // would poison the flight target.
        if !col.is_finite() || !row.is_finite() {
            return;
        }
        let dc = (col - self.col).abs();
        let dr = (row - self.row).abs();
        let range = dc.max(dr * 2.0);
        if range > BAT_RANGE {
            // Out of paw reach but within LOOK_RANGE: latch the look tier.
            if range <= LOOK_RANGE {
                self.pending_look = Some(col);
                self.look_at = Some(now);
            }
            return;
        }
        self.bat_greet = !self.mote_serial.is_multiple_of(2);
        self.mote_serial = self.mote_serial.wrapping_add(1);
        self.pending_bat = Some((col, row));
        self.bat_at = Some(now);
    }

    /// Advance one frame and resolve what to draw.
    pub fn tick(&mut self, sense: PetSense) -> PetFrame {
        #[cfg(test)]
        {
            self.hug_armed = false;
        }
        let elapsed = match self.last_now {
            Some(prev) => sense
                .now
                .saturating_duration_since(prev)
                .as_secs_f32()
                .clamp(0.0, 5.0),
            None => 0.0,
        };
        let dt = elapsed.min(0.10);
        self.last_now = Some(sense.now);
        let airborne_at_entry = self.flight.is_some();
        self.age_gesture_windows((elapsed - dt).max(0.0));
        if elapsed > 0.0 {
            self.col_at_tick = self.col;
            self.tick_dt = dt;
        }
        let was_evicting = std::mem::replace(&mut self.evicting, false);
        if self.last_cell_h != sense.cell_h || self.last_cell_w != sense.cell_w {
            self.stroke.sample(sense.now, None, 0.0);
        }
        self.last_cell_h = sense.cell_h;
        self.last_cell_w = sense.cell_w;
        self.last_grid = (sense.cols, sense.rows);
        self.last_reduced = sense.reduced_motion;
        self.clock += f64::from(elapsed);
        self.lane_clock += f64::from(dt);
        self.arrive_t = (self.arrive_t - dt).max(0.0);

        let width = art_cols(sense.cell_w, sense.cell_h);
        let resight = self.resight.take().filter(|pending| {
            sense.now.saturating_duration_since(pending.at) <= Duration::from_secs_f32(EDGE_FADE)
        });
        let carried = core::mem::take(&mut self.carried);
        if self.console_observation_incoherent() {
            return self.emit(sense, width);
        }
        let sense = self.hold_unpainted_caret(sense, dt);
        let regrid_carried = if let Some((cr, _)) = sense.caret
            && let Some((old_row, old_col)) = self.regrid_caret.take()
        {
            if self.held_for == Some((old_row, old_col)) {
                self.held_for = Some((cr, old_col));
            }
            self.carry_with_regrid(f32::from(cr) - f32::from(old_row), sense.rows)
        } else {
            false
        };
        if self.last_caret.is_none()
            && let Some((_, cc)) = sense.caret
        {
            if self.alpha <= 0.0 || sense.reduced_motion {
                self.wall_side =
                    f32::from(cc) + STATION_LEAD + self.standoff() + width > f32::from(sense.cols);
            } else if self.flight.is_none() && self.update_wall(cc, sense.cols, width) {
                self.pending_wall_transit = true;
            }
        }
        self.begin_console_tick(sense, width);
        self.reseat_unshown_console_body(sense, width);
        if let Some(frame) = self.tick_console_resident(sense, width, dt) {
            return frame;
        }
        let was_invisible = self.alpha <= 0.0;
        let Some((cr, cc)) = sense.caret else {
            self.stroke.sample(sense.now, None, width);
            if let Some(flight) = self.flight.take() {
                let u = (flight.t / flight.dur).clamp(0.0, 1.0);
                let lift = Self::flight_lift(flight.arc, u, flight.big);
                self.retired_flight_lift = (lift > 0.0).then_some((lift, lift / FADE_IN));
                self.deferred_hidden_landing = Some((flight.to_col, flight.to_row));
            }
            if self.alpha == 0.0
                && self.last_caret.is_none()
                && self.pending_worn.is_none()
                && self.deferred_hidden_landing.is_none()
                && self.retired_flight_lift.is_none()
                && self.action == PetAction::Sleep
                && self.quiet >= SLEEP_AFTER + BREATH_WINDOW
                && self.speed == 0.0
                && self.stride == self.stride.round()
                && self.pending_bell.is_none()
                && self.pending_cheer.is_none()
                && !self.pending_sulk
                && !self.pending_vigil_pounce
                && self.vigil_cheer.is_none()
                && self.pending_flow.is_none()
                && self.pending_perk.is_none()
                && self.pending_catch.is_none()
                && self.pending_pet == 0
                && self.pet_at.is_none()
                && self.pending_bat.is_none()
                && self.pending_look.is_none()
                && self.pending_room.is_none()
                && self.console_hold_settled()
                && self.pending_trick.is_none()
                && self.trick_go.is_none()
                && !self.trick_act
                && self.trick_settle.is_none()
                && self.motes.iter().all(Option::is_none)
                && self.departures.iter().all(Option::is_none)
            {
                self.quiet += elapsed;
                let max_col = (f32::from(sense.cols) - width).max(0.0);
                let max_row = f32::from(sense.rows.saturating_sub(1));
                self.col = self.col.clamp(0.0, max_col);
                self.row = self.row.clamp(0.0, max_row);
                self.last_presence = 0.0;
                self.last_drawn = [0.0; PET_DEPARTURES_MAX];
                return PetFrame {
                    alpha: 0,
                    lane_alpha: 0,
                    action: self.action,
                    pose: self.species.skin(PetGlyphId::PetSleep0),
                    col: self.col,
                    row: self.row,
                    lift: 0.0,
                    facing_left: self.facing_left,
                    scale_x: 1.0,
                    scale_y: 1.0,
                    purr: 0.0,
                    contented: false,
                    under_ink: self.hiding,
                    motes: [None; PET_MOTES_MAX],
                    departures: [None; PET_DEPARTURES_MAX],
                };
            }
            self.alpha = (self.alpha - dt / FADE_OUT).max(0.0);
            if self.alpha == 0.0 {
                self.colors = None;
                self.quiet_leap = false;
                self.stream = false;
                self.stream_run = 0;
                self.regrid_caret = None;
                if let Some((col, row)) = self.deferred_hidden_landing.take() {
                    self.col = col;
                    self.row = row;
                }
                self.retired_flight_lift = None;
            }
            if self.alpha == 0.0
                && let Some(pair) = self.pending_worn.take()
            {
                self.worn = Some(pair);
            }
            self.quiet += elapsed;
            self.last_caret = None;
            self.run_count = 0;
            self.lead_room = 0.0;
            self.drop_theater();
            self.drop_tricks();
            if self.alpha == 0.0 {
                self.departures = [None; PET_DEPARTURES_MAX];
                self.arrive_t = 0.0;
                self.adopted_look = None;
            }
            self.enter_settled(dt);
            return self.emit(sense, width);
        };
        let resumed_hidden_flight = self.deferred_hidden_landing.take().is_some();
        if !resumed_hidden_flight && let Some((lift, fall)) = self.retired_flight_lift.as_mut() {
            *lift = (*lift - *fall * dt).max(0.0);
            if *lift == 0.0 {
                self.retired_flight_lift = None;
            }
        }
        if was_invisible {
            self.colors = None;
        }
        if self.room_cheer && self.room_cheer_leaving {
            self.alpha = (self.alpha - dt / FADE_OUT).max(0.0);
            if self.alpha == 0.0 {
                let (col, row) = self.station_safe((cr, cc), sense.cols, sense.rows, width);
                self.col = col;
                self.row = row;
                self.home = col;
                self.col_at_tick = col;
                self.drop_room_cheer();
            }
        } else {
            self.alpha = (self.alpha + dt / FADE_IN).min(1.0);
        }

        let prev = self.last_caret;
        if prev.is_none() {
            // A caret seen again where it was keeps the stand its cat held:
            // after a one-frame hide, or on a surface the focus comes back to
            // (`held_stash`). Anywhere else the stand starts afresh.
            if self.held_for != Some((cr, cc)) {
                let surface = self.surface;
                self.held_lead = self
                    .held_stash
                    .iter_mut()
                    .find(|s| s.is_some_and(|s| s.surface == surface && s.caret == (cr, cc)))
                    .and_then(Option::take)
                    .map_or(0.0, |s| s.lead);
            }
            if was_invisible {
                self.wall_side =
                    f32::from(cc) + STATION_LEAD + self.standoff() + width > f32::from(sense.cols);
                let (c, r) = self.station_safe((cr, cc), sense.cols, sense.rows, width);
                self.col = c;
                self.row = r;
                if let Some(pending) = resight.filter(|_| !sense.reduced_motion) {
                    self.spend_crossfade(pending, carried);
                }
            } else {
                let other_row = !regrid_carried && (f32::from(cr) - self.row).abs() >= 0.5;
                let behind = f32::from(cc) < self.col - SNAP_COLS;
                if other_row || behind {
                    self.quiet_leap = true;
                }
            }
        }
        self.last_caret = Some((cr, cc));

        let moved = prev.is_some_and(|(pr, pc)| pr != cr || pc != cc);
        let mut move_dc = 0.0f32;
        let mut move_dr = 0.0f32;
        let mut wrap_landing = false;
        if moved {
            let pcell = prev.expect("moved implies a previous caret");
            let (dr, dc, folded) = Self::caret_delta(
                pcell,
                (cr, cc),
                sense.cols,
                sense.wrapped,
                self.caret_at_home(cr, cc),
                self.ink_scrolled,
            );
            move_dc = dc;
            move_dr = dr;
            if dr != 0.0 || folded || dc.abs() >= stream_jump(sense.cols) {
                self.stream_run = if (self.clock - self.stream_last) as f32 <= STREAM_WINDOW {
                    self.stream_run.saturating_add(1)
                } else {
                    1
                };
                self.stream_last = self.clock;
                if self.stream_run >= STREAM_RUN && !self.stream {
                    self.stream = true;
                    self.pending_pounce = false;
                    self.pending_big_jump = false;
                    self.quiet_leap = false;
                    self.clear_play_intents();
                    let coiled = self.action == PetAction::Crouch
                        && !self.hiding
                        && !self.stakeout
                        && !self.vigil
                        && !self.pending_vigil_pounce
                        && self.play_to.is_none();
                    self.hop_crouch = false;
                    self.big_gather = false;
                    if self.action.settled() || coiled {
                        self.set_action(PetAction::Perk);
                    }
                }
            }
            if !self.stream {
                self.quiet_leap |= folded;
            }
            let was_asleep = self.action == PetAction::Sleep;
            self.on_move(dr, dc, f32::from(cc), sense.cols, sense.reduced_motion);
            if folded && pcell.0 > cr && dc < 0.0 && self.run_dir < 0 && self.rhythm_open() {
                self.wrap_back = true;
            } else if dc > 0.0 || dr != 0.0 {
                self.wrap_back = false;
            }
            let wrapped_on = (folded && dc > 0.0)
                || (cr >= pcell.0
                    && cr <= pcell.0.saturating_add(1)
                    && f32::from(cc) - f32::from(pcell.1) <= -0.5 * f32::from(sense.cols));
            if wrapped_on
                && (self.wrap_ahead.is_some() || self.flight.is_some_and(|f| f.awaits_wrap))
            {
                self.wrap_ahead = None;
                if let Some(f) = self.flight.as_mut().filter(|f| f.awaits_wrap) {
                    f.awaits_wrap = false;
                    self.pending_pounce = false;
                    self.pending_big_jump = false;
                    wrap_landing = true;
                }
            } else if dr != 0.0 || dc.abs() >= POUNCE_JUMP {
                self.wrap_ahead = None;
                if let Some(f) = self.flight.as_mut() {
                    f.awaits_wrap = false;
                }
            }
            if folded {
                self.key_pace = 0.0;
            }
            if was_asleep && self.action == PetAction::Waking {
                self.spawn_wake_pop(width);
            }
            self.quiet = 0.0;
            self.clear_trick_settle();
            self.drop_trick_go();
            self.hide_to = None;
            if self.hiding {
                self.hiding = false;
                if self.action == PetAction::Crouch {
                    self.set_action_keep(PetAction::Stand);
                }
            }
        } else {
            self.quiet += elapsed;
        }

        if sense.reduced_motion {
            self.stroke.sample(sense.now, None, width);
            self.alpha = 1.0;
            let base = (Self::station(cc, sense.cols, width), f32::from(cr));
            let (col, row) = self
                .console_station(base, (sense.rows, sense.cols), width)
                .unwrap_or(base);
            self.col = col;
            self.row = row;
            self.home = col;
            self.col_at_tick = col;
            self.flight = None;
            self.retired_flight_lift = None;
            self.quiet_leap = false;
            self.stream = false;
            self.stream_run = 0;
            self.motes = [None; PET_MOTES_MAX];
            self.content = (self.content + f32::from(self.pending_pet) * PET_CONTENT).min(1.0);
            self.drop_theater();
            self.trick_go = None;
            self.trick_heard = None;
            self.trick_act = false;
            self.clear_trick_settle();
            self.sweep_trick_latch(sense.now);
            if let Some(p) = self.pending_trick {
                if p.trick == Trick::Sleep {
                    if self.quiet >= Self::trick_settle_after(p.confirmed) {
                        self.pending_trick = None;
                        self.quiet = self.quiet.max(SLEEP_AFTER);
                    }
                } else {
                    self.pending_trick = None;
                    self.praise(p.trick);
                }
            }
            if let Some(pair) = self.pending_worn.take() {
                self.worn = Some(pair);
            }
            self.departures = [None; PET_DEPARTURES_MAX];
            self.arrive_t = 0.0;
            self.action = if self.quiet >= SLEEP_AFTER {
                PetAction::Sleep
            } else {
                PetAction::Sit
            };
            return self.emit(sense, width);
        }

        self.age_gesture_windows(dt);
        self.hold_lead();
        if moved && self.rhythm_open() {
            self.rhythm_at = Some(self.clock);
        }

        if !(matches!(
            self.action,
            PetAction::Crouch | PetAction::Perk | PetAction::Startle
        ) && self.still_since.is_some()
            && self.still_hold_fresh())
        {
            self.action_t += dt;
        }
        self.gait_t += dt;
        if matches!(self.action, PetAction::Leap | PetAction::Land) {
            self.gait_pace = 0.0;
            self.gait_rest = GAIT_DWELL;
            self.running = false;
            self.lateral = false;
        }
        self.land_t = (self.land_t - dt).max(0.0);
        self.flinch_t = (self.flinch_t - dt).max(0.0);
        if self.action != PetAction::Waking {
            self.pending_drop = false;
        }
        if !matches!(self.action, PetAction::Land | PetAction::Sit) {
            self.drop_sit_t = 0.0;
        }
        if !matches!(self.action, PetAction::Land | PetAction::Stand) {
            self.commit_t = 0.0;
        }
        if self.action != PetAction::Land {
            self.scrabble = false;
            self.land_t = 0.0;
            self.land_span = 0.0;
            self.skid_dir = 0.0;
            self.land_running = false;
        }
        if !matches!(self.action, PetAction::Leap | PetAction::Land) {
            self.bound2 = None;
            self.resume = None;
        }
        if self.action != PetAction::Loaf && !matches!(self.resume, Some((PetAction::Loaf, _))) {
            self.wriggle_t = 0.0;
        }
        if self.action != PetAction::Purr && !matches!(self.resume, Some((PetAction::Purr, _))) {
            self.pet_hold_t = 0.0;
        }
        if self.action != PetAction::Frolic {
            self.play_frolic = false;
            self.play_hold = 0.0;
            self.swipe = false;
        }
        if self.action != PetAction::Droop && !matches!(self.resume, Some((PetAction::Droop, _))) {
            self.droop_seated = false;
            self.droop_brief = false;
        }
        if self.action != PetAction::Crouch {
            self.hiding = false;
            self.vigil = false;
            self.pending_vigil_pounce = false;
            self.stakeout = false;
            self.play_to = None;
            self.hop_crouch = false;
            self.big_gather = false;
        }
        if self.play_to.is_none() && self.flight.is_none() && self.action != PetAction::Land {
            self.play_land = None;
        }
        if !matches!(self.play_land, Some(PlayLand::PointerFrolic))
            && let Some(big) = self.vigil_cheer.take()
        {
            self.pending_cheer.get_or_insert(big);
        }
        if self.action != PetAction::Perk {
            self.drop_trick_go();
            self.song_left = 0;
        }
        if !self.trick_settle_posed() {
            self.clear_trick_settle();
        }
        if self.trick_act && self.trick_act_over() {
            self.trick_act = false;
        }
        self.pet_hold_t = (self.pet_hold_t - dt).max(0.0);
        if self.song_left > 0 {
            self.song_next = (self.song_next - dt).max(0.0);
            if self.song_next <= 0.0 {
                self.song_left -= 1;
                self.song_next = SONG_BEAT;
                self.mail_beat(width);
            }
        }
        let expired = |at: Option<Instant>, ttl: f32| {
            at.is_some_and(|at| sense.now.saturating_duration_since(at).as_secs_f32() > ttl)
        };
        if expired(self.pending_bell, BELL_LATCH_TTL) {
            self.pending_bell = None;
        }
        if expired(self.pet_at, PET_LATCH_TTL) {
            self.pending_pet = 0;
            self.pet_at = None;
        }
        self.sweep_trick_latch(sense.now);
        if expired(self.pending_flow, FLOW_LATCH_TTL) {
            self.pending_flow = None;
        }
        if expired(self.pending_perk, V2_PERK_LATE) {
            self.pending_perk = None;
        }
        if expired(self.pending_catch.map(|star| star.born), V2_CATCH_TTL) {
            self.pending_catch = None;
        }
        if expired(self.room_at, ROOM_CUE_TTL) {
            self.pending_room = None;
            self.room_at = None;
        }
        self.catch_cool = (self.catch_cool - elapsed).max(0.0);
        if expired(self.bat_at, BAT_TTL) {
            self.pending_bat = None;
            self.bat_at = None;
        }
        if expired(self.look_at, LOOK_TTL) {
            self.pending_look = None;
            self.look_at = None;
        }
        if self.stream && (self.clock - self.stream_last) as f32 > STREAM_LAPSE {
            self.stream = false;
            self.stream_run = 0;
            self.quiet_leap = true;
        }
        self.watch_heat = if sense.output_burst {
            (self.watch_heat + dt * WATCH_RISE).min(1.0)
        } else if self.room_quiet {
            self.watch_heat
        } else {
            (self.watch_heat - dt * WATCH_FALL).max(0.0)
        };
        if self.watch_heat < WATCH_GATE {
            self.watch_t = 0.0;
            self.watch_spent = false;
        }
        self.vigil_t = match self.exec_since {
            Some(at) if self.vigil_armed => sense.now.saturating_duration_since(at).as_secs_f32(),
            _ => 0.0,
        };
        let burst_edge = sense.output_burst && !self.last_burst;
        self.last_burst = sense.output_burst;
        self.twitch_t = (self.twitch_t - dt).max(0.0);
        self.stumble_t = (self.stumble_t - dt).max(0.0);
        self.bored_cool = (self.bored_cool - elapsed).max(0.0);
        if burst_edge
            && self.twitch_t <= 0.0
            && self.watch_heat < WATCH_GATE
            && self.action.settled()
            && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
        {
            self.twitch_t = TWITCH_DUR;
            self.twitch_up = self.mote_serial.is_multiple_of(2);
            self.mote_serial = self.mote_serial.wrapping_add(1);
        }
        let stroke_point = if !was_invisible && prev.is_some() {
            self.stroke_point(&sense, width)
        } else {
            None
        };
        if self.stroke.sample(sense.now, stroke_point, width) {
            self.note_petted(sense.now);
        }
        match sense.pointer {
            Some((px, py)) if px.is_finite() && py.is_finite() => {
                if let Some((lx, ly)) = self.last_pointer
                    && dt > 1e-4
                    && (px != lx
                        || py != ly
                        || self.pointer_vx != 0.0
                        || self.pointer_vy != 0.0
                        || self.pointer_speed != 0.0)
                {
                    let k = 1.0 - (-dt / DASH_WINDOW).exp();
                    let vx_inst = (px - lx) / dt;
                    let vy_inst = (py - ly) / dt;
                    self.pointer_vx += (vx_inst - self.pointer_vx) * k;
                    self.pointer_vy += (vy_inst - self.pointer_vy) * k;
                    let inst = if vx_inst == 0.0 && vy_inst == 0.0 {
                        0.0
                    } else {
                        vx_inst.hypot(vy_inst)
                    };
                    self.pointer_speed += (inst - self.pointer_speed) * k;
                    if px == lx && py == ly && self.pointer_speed < 1e-5 {
                        self.pointer_vx = 0.0;
                        self.pointer_vy = 0.0;
                        self.pointer_speed = 0.0;
                    }
                }
                self.last_pointer = Some((px, py));
                let speed = self.pointer_speed;
                self.pointer_heat = if speed >= GAZE_FOLLOW_MIN / GAZE_FOLLOW_WINDOW {
                    (self.pointer_heat + dt * POINTER_RISE).min(1.0)
                } else {
                    (self.pointer_heat - dt * POINTER_FALL).max(0.0)
                };
                if self.pointer_heat < POUNCE_REARM {
                    // Distinct branch.
                    self.pointer_armed = true;
                }
                self.dash_t = if speed >= DASH_SPEED {
                    self.dash_t + dt
                } else {
                    0.0
                };
                let range = (px - (self.col + width * 0.5)).abs();
                if self.pointer_armed
                    && !self.room_hold
                    && stroke_point.is_none()
                    && self.dash_t >= DASH_MIN_T
                    && self.content >= PLAY_CONTENT
                    && range <= POUNCE_RANGE
                    && !self.pending_pounce
                    && !self.pending_big_jump
                {
                    self.pointer_armed = false;
                    self.dash_t = 0.0;
                    self.pending_pointer_pounce = Some((
                        px + self.pointer_vx * LEAD_TIME,
                        py + self.pointer_vy * LEAD_TIME,
                    ));
                }
                self.pursuit_cool = (self.pursuit_cool - elapsed).max(0.0);
                let dist = (px - (self.col + width * 0.5))
                    .abs()
                    .max((py - self.row).abs() * 2.0);
                match self.pursuit_t {
                    None => {
                        if self.pursuit_cool <= 0.0
                            && !self.room_hold
                            && stroke_point.is_none()
                            && self.pointer_heat >= PURSUIT_HEAT
                            && (PURSUIT_MIN_SPEED..DASH_SPEED).contains(&speed)
                            && dist <= PURSUIT_RANGE
                            && dist > PURSUIT_CATCH
                            && self.content >= PLAY_CONTENT
                            && self.quiet >= PURSUIT_QUIET
                            && self.flight.is_none()
                            && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
                        {
                            self.pursuit_t = Some(0.0);
                        }
                    }
                    Some(t) => {
                        let t = t + dt;
                        self.pursuit_t = Some(t);
                        if self.quiet < PURSUIT_QUIET || self.room_hold {
                            self.pursuit_t = None;
                            self.pursuit_cool = self.pursuit_cool.max(1.0);
                        } else if speed < PURSUIT_MIN_SPEED * 0.5 && dist <= PURSUIT_CATCH {
                            self.pursuit_t = None;
                            self.pursuit_cool = PURSUIT_COOL;
                            self.bat_greet = !self.mote_serial.is_multiple_of(2);
                            self.mote_serial = self.mote_serial.wrapping_add(1);
                            self.pending_bat = Some((px, py));
                            self.bat_at = Some(sense.now);
                        } else if t > PURSUIT_MAX_T || dist > PURSUIT_RANGE * 1.5 {
                            self.pursuit_t = None;
                            self.pursuit_cool = PURSUIT_COOL;
                            self.groom_owed = true;
                        }
                    }
                }
            }
            _ => {
                self.last_pointer = None;
                self.pointer_vx = 0.0;
                self.pointer_vy = 0.0;
                self.pointer_speed = 0.0;
                self.pointer_heat = 0.0;
                self.dash_t = 0.0;
                self.pending_pointer_pounce = None;
                self.pursuit_t = None;
            }
        }

        if self.flight.is_none()
            && self.wrap_ahead.is_none()
            && !(self.typing_on() && self.at_the_margin((cr, cc), sense.cols, width, 0.0))
            && !(self.run_dir < 0 && self.rhythm_open())
            && self.update_wall(cc, sense.cols, width)
        {
            self.pending_wall_transit = true;
        }
        let (target, target_row) = self.station_safe((cr, cc), sense.cols, sense.rows, width);

        if self.flight.is_none()
            && !self.cheering()
            && !(self.action.settled() && (self.pending_pounce || self.pending_big_jump))
            && self.wrap_ahead.is_none()
            && matches!(
                self.action,
                PetAction::Sleep
                    | PetAction::Waking
                    | PetAction::Sit
                    | PetAction::Loaf
                    | PetAction::Purr
                    | PetAction::Groom
                    | PetAction::Perk
                    | PetAction::Stand
                    | PetAction::Startle
                    | PetAction::Frolic
                    | PetAction::Droop
            )
            && (self.ink_overlaps(self.col, self.row, width)
                || self.console_obstructed(sense)
                || (was_evicting
                    && !self.stream
                    && (self.col - target).abs() > f32::EPSILON
                    && (self.row - f32::from(cr)).abs() < 0.5
                    && self.col < f32::from(cc) + STATION_LEAD
                    && self.col + width > f32::from(cc) - STATION_LEAD))
        {
            let watching = self.action == PetAction::Perk
                && self.watch_heat >= WATCH_GATE
                && !self.watch_spent;
            let (safe, safe_row) = if watching {
                (
                    self.ink_safe_col(self.col, self.row, width, sense.cols),
                    self.row,
                )
            } else if self.stream {
                let stand = self.ink_stand(self.col, self.row, width, sense.cols, sense.rows);
                self.console_station(stand, (sense.rows, sense.cols), width)
                    .unwrap_or(stand)
            } else {
                (target, target_row)
            };
            let moved_off = (safe - self.col).abs() > f32::EPSILON;
            let safe_row = if watching { self.row } else { safe_row };
            if !watching && self.action == PetAction::Droop && moved_off {
                self.action_t = 0.0;
            }
            let walks_off = self.action == PetAction::Stand
                && self.commit_t <= 0.0
                && !self.stream
                && (safe_row - self.row).abs() < 0.5
                && (safe - self.col).abs() > ARRIVED;
            if walks_off {
            } else if Self::is_re_anchor(self.col, safe, self.row, safe_row) {
                self.resume = if matches!(
                    self.action,
                    PetAction::Startle | PetAction::Frolic | PetAction::Perk
                ) {
                    None
                } else {
                    Some((self.action, self.action_t))
                };
                self.hop_crouch = false;
                self.big_gather = false;
                self.leave_ground(safe, safe_row, true, sense.cols, width, LeapPolicy::Quiet);
            } else if !watching
                && !self.stream
                && !self.tennis
                && self.commit_t <= 0.0
                && self.drop_sit_t <= 0.0
                && self.wriggle_t <= 0.0
                && !self.groom_owed
                && self.pet_hold_t <= 0.0
                && !self.room_cheer
                && self.pursuit_t.is_none()
                && self.hide_to.is_none()
                && matches!(
                    self.action,
                    PetAction::Stand
                        | PetAction::Sit
                        | PetAction::Loaf
                        | PetAction::Purr
                        | PetAction::Groom
                )
                && (safe - self.col).abs() > ARRIVED
            {
                self.facing_left = safe < self.col;
                self.set_action_keep(PetAction::Walk);
            } else {
                self.col = Self::evict_toward(self.col, safe, dt);
                self.evicting = self.col != safe;
            }
        }

        let mut hugging = false;
        let (target, target_row) = if (self.stream
            || (self.watch_heat >= WATCH_GATE && !self.watch_spent))
            && !self.full_screen
            && !self.pending_pounce
            && !self.pending_big_jump
            && self.flight.is_none()
            && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
            && let Some(live) = self.ink_live_row()
        {
            #[cfg(test)]
            {
                self.hug_armed = true;
            }
            let col = self
                .ink_span(live)
                .map(|(_, end)| {
                    (end + STATION_LEAD).clamp(0.0, (f32::from(sense.cols) - width).max(0.0))
                })
                .unwrap_or(self.col);
            if (live - self.row).abs() >= WATCH_HUG_ROWS
                && !self.console_bars((col, live), sense.rows, width)
            {
                hugging = true;
                self.quiet_leap = true;
                (col, live)
            } else if (live - self.row).abs() >= WATCH_HUG_ROWS {
                (target, target_row)
            } else {
                (self.col, self.row)
            }
        } else {
            (target, target_row)
        };

        let (target, target_row) = if self.pursuit_t.is_some()
            && !self.pending_pounce
            && !self.pending_big_jump
            && self.flight.is_none()
            && let Some((px, py)) = self.last_pointer
        {
            let col = (px + self.pointer_vx * LEAD_TIME - width * 0.5)
                .clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
            let row = if (py - self.row).abs() >= PURSUIT_HUG_ROWS {
                py.round()
                    .clamp(0.0, f32::from(sense.rows.saturating_sub(1)))
            } else {
                self.row
            };
            (col, row)
        } else {
            (target, target_row)
        };

        if self.room_hold {
            self.hide_to = None;
        }
        let (target, target_row) = if let Some(dest) = self.hide_to
            && !self.pending_pounce
            && !self.pending_big_jump
            && self.flight.is_none()
        {
            let dest = dest.clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
            if (dest - self.col).abs() <= ARRIVED && self.speed.abs() < FLIP_SPEED {
                self.begin_hide();
            }
            (dest, self.row)
        } else {
            (target, target_row)
        };

        let watch_far = (target_row - self.row).abs() > WATCH_HUG_ROWS;
        let (target, target_row) = if self.stream && self.flight.is_none() && !hugging && !watch_far
        {
            (self.col, self.row)
        } else {
            (target, target_row)
        };

        if self.flight.is_none()
            && !self.still_fresh()
            && let Some((col, row, dur, arc, kind)) = self.deferred_arc
        {
            self.deferred_arc = None;
            self.still_since = None;
            self.begin_arc(col, row, dur, arc, kind);
            return self.emit(sense, width);
        }

        let (target, target_row) = if self.flight.is_none()
            && !self.stream
            && self.play_land.is_none()
            && self.pursuit_t.is_none()
        {
            if let Some(aim) = self.wrap_back_aim((cr, cc), (sense.rows, sense.cols), width) {
                aim
            } else if let Some(aim) = self.wrap_ahead_aim((cr, cc), (sense.rows, sense.cols), width)
            {
                aim
            } else {
                (target, target_row)
            }
        } else {
            (target, target_row)
        };

        if let Some(mut f) = self.flight {
            // The wrap the bound left for has come: its landing goes to the
            // caret's stand on the row the wrap put it on (the next one, or
            // this one where the wrap scrolled the line up), rebased, row and
            // column, so the drawn arc never jumps.
            if wrap_landing && airborne_at_entry {
                let (aim, aim_row) = self.station_safe((cr, cc), sense.cols, sense.rows, width);
                let t_rem = (f.dur - f.t).max(0.0) + Self::landing_dur(&f);
                let to = (aim + self.station_drift(t_rem))
                    .clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
                let u_now = (f.t / f.dur).clamp(0.0, 1.0);
                let cap = if f.big {
                    quiet_speed_max(sense.cols)
                } else {
                    HOP_SPEED_MAX
                };
                let span = span_eq(to - self.col, aim_row - self.row);
                if u_now <= REAIM_MAX_U && span / (f.dur - f.t).max(1e-3) <= cap {
                    f.from_col = (self.col - to * u_now) / (1.0 - u_now);
                    f.from_row = (self.row - aim_row * u_now) / (1.0 - u_now);
                    f.to_col = to;
                    f.to_row = aim_row;
                }
            }
            if move_dc != 0.0
                && move_dc.abs() < POUNCE_JUMP
                && move_dr == 0.0
                && !f.drop
                && !f.awaits_wrap
                && !self.stream
                && self.play_land.is_none()
                && self.aims_ahead()
            {
                let (aim, aim_row) = self.station_safe((cr, cc), sense.cols, sense.rows, width);
                let t_rem = (f.dur - f.t).max(0.0) + Self::landing_dur(&f);
                let reach = if self.wall_side {
                    self.station_now(sense.cols.saturating_sub(1), sense.cols, width)
                } else {
                    f32::from(sense.cols) - width
                };
                let to = (aim + self.station_drift(t_rem)).clamp(0.0, reach.max(0.0));
                let u_now = (f.t / f.dur).clamp(0.0, 1.0);
                let cap = if f.big {
                    quiet_speed_max(sense.cols)
                } else {
                    HOP_SPEED_MAX
                };
                if let Some((c2, r2, _)) = self.bound2.as_mut() {
                    let delta2 = to - f.to_col;
                    let to_short = if delta2.abs() > SKID_CELLS {
                        to - delta2.signum() * SKID_CELLS
                    } else {
                        to
                    };
                    if (to_short - *c2) * move_dc > 0.0 && (aim_row - *r2).abs() < 0.5 {
                        *c2 = to_short;
                    }
                } else {
                    let to_arc = if f.big && !f.running {
                        let delta = to - self.col;
                        if delta.abs() > SKID_CELLS {
                            to - delta.signum() * SKID_CELLS
                        } else {
                            to
                        }
                    } else {
                        to
                    };
                    let in_time = (to_arc - self.col).abs() / (f.dur - f.t).max(1e-3) <= cap;
                    let onward = (to_arc - f.to_col) * move_dc > 0.0;
                    if onward && (aim_row - f.to_row).abs() < 0.5 && in_time && u_now <= REAIM_MAX_U
                    {
                        f.from_col = (self.col - to_arc * u_now) / (1.0 - u_now);
                        f.to_col = to_arc;
                    }
                }
            }
            if airborne_at_entry && (move_dr.abs() >= 0.5 || move_dc.abs() >= POUNCE_JUMP) {
                self.bound2 = None;
            }
            if move_dc.abs() >= POUNCE_JUMP && !f.drop && !self.stream && self.play_land.is_none() {
                if f.reaimed {
                    f.late_move = true;
                } else {
                    f.reaimed = true;
                    let _ = self.update_wall(cc, sense.cols, width);
                    let t_rem = (f.dur - f.t).max(0.0) + Self::landing_dur(&f);
                    let predict = if self.aims_ahead() {
                        (self.vhat * t_rem).clamp(-FLIGHT_LEAD_MAX, FLIGHT_LEAD_MAX)
                    } else {
                        0.0
                    };
                    let (aim, aim_row) = self.station_safe((cr, cc), sense.cols, sense.rows, width);
                    let to = (aim + predict).clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
                    let to_arc = if f.big && !f.running {
                        let delta = to - self.col;
                        if delta.abs() > SKID_CELLS {
                            to - delta.signum() * SKID_CELLS
                        } else {
                            to
                        }
                    } else {
                        to
                    };
                    let u_now = (f.t / f.dur).clamp(0.0, 1.0);
                    let cap = if f.big {
                        quiet_speed_max(sense.cols)
                    } else {
                        HOP_SPEED_MAX
                    };
                    let same_row = (aim_row - f.to_row).abs() < 0.5;
                    let in_time = (to_arc - self.col).abs() / (f.dur - f.t).max(1e-3) <= cap;
                    if same_row && in_time && u_now <= REAIM_MAX_U {
                        f.from_col = (self.col - to_arc * u_now) / (1.0 - u_now);
                        f.to_col = to_arc;
                        self.pending_pounce = false;
                        self.pending_big_jump = false;
                        self.pending_wall_transit = false;
                        self.quiet_leap = false;
                    }
                    self.bound2 = None;
                }
            }
            if airborne_at_entry {
                let step = elapsed.min(FLIGHT_STALL_MAX);
                let u0 = (f.t / f.dur).clamp(0.0, 1.0);
                if f.awaits_wrap && u0 >= WRAP_HANG_FROM && f.dur < f.dur0 + WRAP_HANG_MAX {
                    let u1 = (u0 + WRAP_HANG_PACE * step / f.dur).min(1.0);
                    f.t += step;
                    f.dur = (f.t / u1.max(1e-3)).min(f.dur0 + WRAP_HANG_MAX).max(f.t);
                } else {
                    f.t += step;
                }
            }
            let u = (f.t / f.dur).clamp(0.0, 1.0);
            self.col = f.from_col + (f.to_col - f.from_col) * u;
            self.row = f.from_row + (f.to_row - f.from_row) * u;
            self.speed = (f.to_col - f.from_col) / f.dur;
            if u >= 1.0 {
                let span = (f.to_col - f.from_col).abs();
                self.content = (self.content + span * CONTENT_PER_CELL).min(1.0);
                self.flight = None;
                self.scrabble = false;
                self.commit_t = 0.0;
                self.land_running = false;
                if f.big && self.bound2.is_some() {
                    self.land_t = TOUCH_LAND_DUR;
                    self.land_span = 0.0;
                } else if f.running || (self.rhythm_open() && !self.stream) {
                    self.land_t = TOUCH_LAND_DUR;
                    self.land_span = 0.0;
                    self.land_running = true;
                } else if f.big {
                    self.land_t = BIG_LAND_DUR;
                    self.land_span = span.max(1.0);
                    self.skid_from = self.col;
                    self.skid_dir = if f.to_col > f.from_col {
                        1.0
                    } else if f.to_col < f.from_col {
                        -1.0
                    } else {
                        0.0
                    };
                    if f.show {
                        self.spawn_dust(width);
                    }
                } else {
                    self.land_t = LAND_DUR;
                    self.land_span = 0.0;
                    let lead_now = if self.aims_ahead() {
                        self.vhat * LAND_DUR
                    } else {
                        0.0
                    };
                    let miss = (f.to_col - (target + lead_now)).abs();
                    if f.reaimed
                        && !f.drop
                        && !self.stream
                        && self.play_land.is_none()
                        && miss >= SCRABBLE_MISS
                        && (self.stride as u32).wrapping_add(u32::from(self.mote_serial)) % 4 == 0
                    {
                        self.land_t = SCRABBLE_DUR;
                        self.scrabble = true;
                        self.skid_dir = if f.to_col < f.from_col
                            || (f.to_col == f.from_col && self.facing_left)
                        {
                            -1.0
                        } else {
                            1.0
                        };
                        self.spawn_dust(width);
                    }
                }
                if f.drop {
                    self.drop_sit_t = DROP_SIT;
                    self.drop_stare = true;
                }
                if f.late_move
                    && !f.drop
                    && !self.stream
                    && self.play_land.is_none()
                    && (target - self.col).abs() >= POUNCE_JUMP
                {
                    self.commit_t = COMMIT_STARE;
                    if f.to_col > f.from_col {
                        self.facing_left = false;
                    } else if f.to_col < f.from_col {
                        self.facing_left = true;
                    }
                }
                self.set_action(PetAction::Land);
            } else {
                f.t = f.t.min(f.dur);
                self.flight = Some(f);
                self.set_action_keep(PetAction::Leap);
            }
            return self.emit(sense, width);
        }

        if self.action == PetAction::Land && self.land_t <= 0.0 {
            self.scrabble = false;
        }

        if self.action == PetAction::Land && self.bound2.is_some() {
            if move_dr.abs() >= 0.5 || move_dc.abs() >= POUNCE_JUMP {
                self.bound2 = None;
            } else if move_dc != 0.0
                && move_dc.abs() < POUNCE_JUMP
                && move_dr == 0.0
                && !self.stream
                && self.play_land.is_none()
                && self.aims_ahead()
            {
                let (aim, aim_row) = self.station_safe((cr, cc), sense.cols, sense.rows, width);
                let t_rem = self.land_t.max(0.0) + BIG_LAND_DUR;
                let reach = if self.wall_side {
                    self.station_now(sense.cols.saturating_sub(1), sense.cols, width)
                } else {
                    f32::from(sense.cols) - width
                };
                let to = (aim + self.station_drift(t_rem)).clamp(0.0, reach.max(0.0));
                let delta2 = to - self.col;
                let to_short = if delta2.abs() > SKID_CELLS {
                    to - delta2.signum() * SKID_CELLS
                } else {
                    to
                };
                if let Some((c2, r2, _)) = self.bound2.as_mut()
                    && (to_short - *c2) * move_dc > 0.0
                    && (aim_row - *r2).abs() < 0.5
                {
                    *c2 = to_short;
                }
            }
        }

        if self.action == PetAction::Land
            && self.land_t <= 0.0
            && let Some((c2, r2, show2)) = self.bound2.take()
        {
            self.begin_big_arc(c2, r2, sense.cols, show2);
            return self.emit(sense, width);
        }

        if self.action == PetAction::Land
            && self.land_t <= 0.0
            && let Some((pose, held)) = self.resume.take()
        {
            self.set_action(pose);
            self.action_t = held;
            return self.emit(sense, width);
        }

        if self.action == PetAction::Waking && self.pending_drop {
            self.pending_drop = false;
            let to_col = self.ink_safe_col(self.col, target_row, width, sense.cols);
            if (target_row - self.row - 1.0).abs() < 0.5
                && (to_col - self.col).abs() < 0.5
                && !self.ink_overlaps(to_col, target_row, width)
            {
                self.hop_crouch = false;
                self.big_gather = false;
                self.begin_arc(
                    to_col,
                    target_row,
                    DROP_DUR,
                    0.0,
                    FlightKind {
                        drop: true,
                        ..FlightKind::PLAIN
                    },
                );
                return self.emit(sense, width);
            }
        }

        if self.action == PetAction::Land && self.land_t <= 0.0 && self.commit_t > 0.0 {
            self.set_action(PetAction::Stand);
            self.speed = 0.0;
            return self.emit(sense, width);
        }

        if self.action == PetAction::Land && self.land_t <= 0.0 && self.drop_sit_t > 0.0 {
            self.sit_front = true;
            self.peek = false;
            self.facing_left = f32::from(cc) < self.col + width * 0.5;
            self.set_action(PetAction::Sit);
            self.speed = 0.0;
            return self.emit(sense, width);
        }

        if self.action == PetAction::Land && self.land_t <= 0.0 && self.play_land.is_some() {
            let owed = self.play_land.take().expect("checked above");
            if !self.pending_pounce && !self.pending_big_jump {
                match owed {
                    PlayLand::PointerFrolic => {
                        self.play_frolic = true;
                        self.play_hold = POINTER_PLAY_HOLD;
                        self.set_action(PetAction::Frolic);
                        if let Some(big) = self.vigil_cheer.take() {
                            self.spawn_cheer_motes(big, width, false);
                        }
                        return self.emit(sense, width);
                    }
                    PlayLand::BatSwipe { col, row } => {
                        self.facing_left = col < self.col + width * 0.5;
                        self.play_frolic = true;
                        self.swipe = !self.bat_greet;
                        self.play_hold = BAT_HOLD;
                        self.set_action(PetAction::Frolic);
                        if !self.bat_greet {
                            self.spawn_bat_dust(col, row);
                        }
                        return self.emit(sense, width);
                    }
                }
            } else if let Some(big) = self.vigil_cheer.take() {
                self.pending_cheer.get_or_insert(big);
            }
        }

        match self.action {
            PetAction::Sit | PetAction::Stand | PetAction::Perk if self.stream && !hugging => {
                // The stream watch sits the cat down: the notice keeps its
                // beat, a stand sits at once, and the sit holds until the
                // events lapse. Ranked above the tennis watch and idle
                // vignettes. The live-edge hug's trip (`hugging`) is exempt,
                // since this hold returns before every door.
                if self.action == PetAction::Stand
                    || (self.action == PetAction::Perk && self.action_t >= PERK_HOLD)
                {
                    self.set_action(PetAction::Sit);
                }
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Stand if self.commit_t > 0.0 => {
                // The commit stare. A third jump abandons it, judged on this
                // tick's move (the latch is already set by the second).
                if move_dc.abs() >= POUNCE_JUMP {
                    self.commit_t = 0.0;
                    if self.action_t < POSE_FLOOR {
                        self.speed = 0.0;
                        return self.emit(sense, width);
                    }
                } else {
                    self.commit_t = (self.commit_t - dt).max(0.0);
                    if self.commit_t <= 0.0 {
                        self.facing_left = target < self.col;
                        self.set_action(PetAction::Perk);
                    }
                    self.speed = 0.0;
                    return self.emit(sense, width);
                }
            }
            PetAction::Sit if self.drop_sit_t > 0.0 => {
                self.drop_sit_t = (self.drop_sit_t - dt).max(0.0);
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Land if self.stream && self.land_t <= 0.0 => {
                self.set_action(PetAction::Sit);
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Crouch if self.big_gather => {
                // The big jump's gather plays through BIG_CROUCH_DUR, then the
                // bound(s) launch at the re-read target. Over ink it is cut to
                // the pounce's quick coil.
                if (target - self.col).abs() > 1e-4 {
                    self.facing_left = target < self.col;
                }
                let dur = if self.ink_overlaps(self.col, self.row, width) {
                    CROUCH_DUR
                } else {
                    BIG_CROUCH_DUR
                };
                if self.action_t >= dur && !self.gather_owes_veil(dur) {
                    self.big_gather = false;
                    self.leave_ground(
                        target,
                        target_row,
                        false,
                        sense.cols,
                        width,
                        LeapPolicy::Show,
                    );
                }
                return self.emit(sense, width);
            }
            PetAction::Crouch if self.hop_crouch => {
                // The row hop's gather: one tick for a hop, [`CROUCH_DUR`] when
                // the predicted span makes it a bound. The target is re-read
                // every tick.
                if (target - self.col).abs() > 1e-4 {
                    self.facing_left = target < self.col;
                }
                let tier = self.predicted_tier(target, target_row, true, sense.cols, width);
                if ((tier == Tier::Hop && self.coil_d <= 0.0) || self.action_t >= CROUCH_DUR)
                    && !self.gather_owes_veil(CROUCH_DUR)
                {
                    self.hop_crouch = false;
                    self.leave_ground(
                        target,
                        target_row,
                        true,
                        sense.cols,
                        width,
                        LeapPolicy::Quiet,
                    );
                } else {
                    self.coil_d = CROUCH_DUR;
                }
                return self.emit(sense, width);
            }
            PetAction::Loaf if self.wriggle_t > 0.0 => {
                self.wriggle_t = (self.wriggle_t - dt).max(0.0);
                self.speed = 0.0;
                if self.wriggle_t <= 0.0 {
                    self.set_action(PetAction::Waking);
                }
                return self.emit(sense, width);
            }
            PetAction::Sit if self.tennis => {
                let rally_out_of_reach =
                    (target_row - self.row).abs() >= 0.5 || (target - self.col).abs() > POUNCE_GAP;
                if rally_out_of_reach {
                    self.tennis = false;
                } else if (self.clock - self.tennis_last) as f32 <= TENNIS_LAPSE {
                    self.speed = 0.0;
                    self.consume_v2_offer(&sense, width);
                    return self.emit(sense, width);
                }
                self.tennis = false;
            }
            PetAction::Crouch if self.vigil || self.pending_vigil_pounce => {
                // Anything else stands the cat up and lets the ladder act this
                // same tick.
                if self.pending_vigil_pounce {
                    self.pending_vigil_pounce = false;
                    self.vigil = false;
                    if let Some((cr, ccol)) = sense.caret {
                        self.vigil_cheer = self.pending_cheer.take();
                        self.quiet = 0.0;
                        let to_col = (f32::from(ccol) - width * 0.5)
                            .clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
                        let to_row =
                            f32::from(cr).clamp(0.0, f32::from(sense.rows.saturating_sub(1)));
                        if (to_col - self.col).abs() > 1e-4 {
                            self.facing_left = to_col < self.col;
                        } else if (f32::from(ccol) - (self.col + width * 0.5)).abs() > 1e-4 {
                            self.facing_left = f32::from(ccol) < self.col + width * 0.5;
                        }
                        self.play_to = Some((to_col, to_row));
                        self.play_land = Some(PlayLand::PointerFrolic);
                        self.pending_pounce = false;
                        self.pending_big_jump = false;
                        self.hop_crouch = false;
                        self.big_gather = false;
                        return self.emit(sense, width);
                    }
                }
                if self.vigil_t >= VIGIL_AFTER
                    && !self.pending_sulk
                    && self.pending_cheer.is_none()
                    && self.pending_trick.is_none()
                {
                    self.facing_left = f32::from(cc) < self.col + width * 0.5;
                    self.speed = 0.0;
                    return self.emit(sense, width);
                }
                self.vigil = false;
                self.set_action_keep(PetAction::Stand);
            }
            PetAction::Crouch if self.hiding => {
                // The dwell is wall time: the still crouch is off the frame
                // train (`needs_frames`), and the tick that serves its offered
                // end must spend all of it, not a clamped `dt`.
                if self.room_hold {
                    self.hiding = false;
                    self.set_action_keep(PetAction::Stand);
                    return self.emit(sense, width);
                }
                self.hide_t -= elapsed;
                if self.hide_t > 0.0 {
                    self.speed = 0.0;
                    return self.emit(sense, width);
                }
                self.hiding = false;
                self.set_action_keep(PetAction::Stand);
                return self.emit(sense, width);
            }
            PetAction::Crouch if self.stakeout => {
                // The stakeout: the hunting crouch at a creeping (or stopped)
                // toy, up to its patience. A latched dash pounce becomes the
                // strike; anything else lapsing stands the cat up.
                if self.room_hold {
                    self.pending_pointer_pounce = None;
                    self.stakeout = false;
                    self.set_action_keep(PetAction::Stand);
                    return self.emit(sense, width);
                }
                if let Some((ac, ar)) = self.pending_pointer_pounce.take() {
                    self.stakeout = false;
                    let to_col =
                        (ac - width * 0.5).clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
                    let to_row = ar.clamp(0.0, f32::from(sense.rows.saturating_sub(1)));
                    if (to_col - self.col).abs() > 1e-4 {
                        self.facing_left = to_col < self.col;
                    } else if (ac - (self.col + width * 0.5)).abs() > 1e-4 {
                        self.facing_left = ac < self.col + width * 0.5;
                    }
                    self.play_to = Some((to_col, to_row));
                    self.play_land = Some(PlayLand::PointerFrolic);
                    self.hop_crouch = false;
                    self.big_gather = false;
                    return self.emit(sense, width);
                }
                self.stakeout_t += dt;
                if self.pursuit_t.is_some() || self.pointer_speed >= PURSUIT_MIN_SPEED {
                    self.stakeout = false;
                    self.set_action_keep(PetAction::Stand);
                    return self.emit(sense, width);
                }
                let live = match self.last_pointer {
                    Some((px, py)) => {
                        let dist = (px - (self.col + width * 0.5))
                            .abs()
                            .max((py - self.row).abs() * 2.0);
                        self.facing_left = px < self.col + width * 0.5;
                        dist <= STALK_RANGE * 1.25 && self.stakeout_t < STAKEOUT_MAX
                    }
                    None => false,
                };
                if live {
                    self.speed = 0.0;
                    return self.emit(sense, width);
                }
                self.stakeout = false;
                self.pursuit_cool = self.pursuit_cool.max(2.0);
                self.set_action_keep(PetAction::Stand);
                return self.emit(sense, width);
            }
            PetAction::Groom if self.groom_owed && self.action_t < GROOM_HOLD => {
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Groom if self.groom_owed => {
                self.groom_owed = false;
            }
            PetAction::Crouch => {
                if self.play_to.is_some() {
                    if self.room_hold && self.vigil_cheer.is_none() {
                        self.play_to = None;
                        self.play_land = None;
                        self.set_action_keep(PetAction::Stand);
                        return self.emit(sense, width);
                    }
                    if self.action_t >= CROUCH_DUR
                        && let Some((tc, tr)) = self.play_to.take()
                    {
                        let vertical = (tr - self.row).abs() >= 0.5;
                        self.leave_ground(tc, tr, vertical, sense.cols, width, LeapPolicy::Play);
                    }
                    return self.emit(sense, width);
                }
                if (target - self.col).abs() > 1e-4 {
                    self.facing_left = target < self.col;
                }
                self.coil_d = if self.quiet_leap {
                    CROUCH_DUR
                } else {
                    Self::coil_dur((target - self.col).abs())
                };
                if self.action_t >= self.coil_d && !self.gather_owes_veil(self.coil_d) {
                    let vertical = (target_row - self.row).abs() >= 0.5;
                    self.leave_ground(
                        target,
                        target_row,
                        vertical,
                        sense.cols,
                        width,
                        LeapPolicy::Quiet,
                    );
                }
                return self.emit(sense, width);
            }
            PetAction::Startle
                if self.action_t < STARTLE_HOLD
                    && !(self.action_t >= FLINCH_DUR
                        && (target - self.col).abs() > STARTLE_REACH
                        && self.runs_down(target - self.col)) =>
            {
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Frolic if self.play_frolic => {
                // A play flourish holds its beat, then the walk home.
                if self.action_t < self.play_hold {
                    self.speed = 0.0;
                    return self.emit(sense, width);
                }
                self.play_frolic = false;
                self.play_hold = 0.0;
                self.swipe = false;
            }
            PetAction::Frolic if self.action_t < FROLIC_HOLD => {
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Droop if self.action_t < self.droop_hold() => {
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Purr if self.pet_hold_t > 0.0 => {
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Waking if self.action_t < WAKE_DUR => {
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            PetAction::Perk if self.action_t < PERK_HOLD => {
                // The double-take never loiters on ink: with travel latched and
                // glyphs underfoot, the hold collapses and the launch ladder
                // takes over this tick.
                if !((self.pending_pounce || self.pending_big_jump)
                    && self.ink_overlaps(self.col, self.row, width))
                {
                    self.speed = 0.0;
                    return self.emit(sense, width);
                }
            }
            PetAction::Perk if self.room_cheer && self.pending_cheer.is_some() => {
                let big = self.pending_cheer.take().expect("checked by the guard");
                self.speed = 0.0;
                let air = self.room_cheer_air.take();
                if let Some(CheerAir::Lanes(facing_left)) = air {
                    self.facing_left = facing_left;
                }
                self.cheer_frolic(big, width, air == Some(CheerAir::Compact));
                return self.emit(sense, width);
            }
            PetAction::Perk if self.trick_go.is_some() => {
                let trick = self.trick_go.take().expect("checked by the guard");
                if self.pending_trick.is_some_and(|p| p.trick == trick) {
                    self.pending_trick = None;
                }
                self.speed = 0.0;
                self.perform_trick(trick, cc, width, dt);
                if self.trick_settle.is_none() {
                    return self.emit(sense, width);
                }
            }
            PetAction::Land if self.land_t > 0.0 => {
                // Landing recovery. A big landing also skids the 0.8 cells the
                // flight aimed short by, ending on the station.
                if self.land_span > 0.0 && self.skid_dir != 0.0 {
                    static SKID_NORM: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
                    let denom = *SKID_NORM.get_or_init(|| 1.0 - (-4.0f32).exp());
                    let u = (1.0 - self.land_t / BIG_LAND_DUR).clamp(0.0, 1.0);
                    let s = (1.0 - (-4.0 * u).exp()) / denom;
                    self.col = (self.skid_from + self.skid_dir * SKID_CELLS * s)
                        .clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
                }
                if self.bound2.is_none() {
                    self.plant_stride(dt);
                }
                self.speed = 0.0;
                return self.emit(sense, width);
            }
            _ => {}
        }

        if self.room_cheer {
            if !self.room_cheer_in_play() {
                self.drop_room_cheer();
            } else if self.pending_cheer.is_some() {
                self.speed = 0.0;
                let asleep = self.action == PetAction::Sleep;
                if self.room_cheer_trips == 0 && self.appear_on_cheer_stage(sense, width) {
                } else if asleep && self.console_stretch_on_blank() {
                    self.quiet = 0.0;
                    self.set_action(PetAction::Waking);
                    self.spawn_wake_pop(width);
                } else {
                    let stage = if self.room_cheer_trips < CHEER_STAGE_TRIPS {
                        let stage = self.cheer_stage(sense, width, self.room_cheer_stage);
                        self.room_cheer_air = stage.map(|s| s.air);
                        stage
                    } else {
                        None
                    };
                    if asleep {
                        self.quiet = 0.0;
                        self.spawn_wake_pop(width);
                    }
                    if let Some((col, row)) = stage.and_then(|s| s.at) {
                        self.room_cheer_trips += 1;
                        self.room_cheer_stage = Some((col, row));
                        if (col - self.col).abs() > 1e-4 {
                            self.facing_left = col < self.col;
                        }
                        self.leave_ground(col, row, true, sense.cols, width, LeapPolicy::Play);
                    } else {
                        self.room_cheer_trips = CHEER_STAGE_TRIPS;
                        if let Some(CheerAir::Lanes(facing_left)) = self.room_cheer_air {
                            self.facing_left = facing_left;
                        }
                        self.set_action(PetAction::Perk);
                    }
                }
                return self.emit(sense, width);
            } else {
                if self.action == PetAction::Frolic {
                    self.set_action(PetAction::Sit);
                }
                self.speed = 0.0;
                return self.emit(sense, width);
            }
        }

        let gap = target - self.col;

        if self.quiet_leap {
            self.pending_big_jump = false;
        }
        if !self.quiet_leap
            && (self.pending_big_jump
                || (self.pursuit_t.is_none() && gap.abs() >= BIG_GAP_FRAC * f32::from(sense.cols)))
            && gap.abs() > ARRIVED
        {
            self.facing_left = gap < 0.0;
            if self.action == PetAction::Perk || self.ink_overlaps(self.col, self.row, width) {
                self.pending_big_jump = false;
                self.pending_pounce = false;
                self.hop_crouch = false;
                self.big_gather = true;
                self.set_action(PetAction::Crouch);
            } else {
                self.pending_big_jump = true;
                self.set_action(PetAction::Perk);
            }
            return self.emit(sense, width);
        }
        self.pending_big_jump = false;

        if (target_row - self.row).abs() >= 0.5 {
            if gap.abs() > 1e-4 {
                self.facing_left = gap < 0.0;
            }
            if self.notices(target, target_row, true, sense.cols, width) {
                self.set_action(PetAction::Perk);
                return self.emit(sense, width);
            }
            self.hop_crouch = true;
            self.big_gather = false;
            self.coil_d =
                if self.predicted_tier(target, target_row, true, sense.cols, width) == Tier::Hop {
                    0.0
                } else {
                    CROUCH_DUR
                };
            self.set_action(PetAction::Crouch);
            return self.emit(sense, width);
        }

        if self.pending_wall_transit {
            self.pending_wall_transit = false;
            if gap.abs() > ARRIVED {
                self.hop_crouch = false;
                if gap.abs() <= WALL_SPAN_MAX {
                    self.begin_arc(
                        target,
                        target_row,
                        WALL_HOP_DUR,
                        WALL_HOP_ARC,
                        FlightKind {
                            hop: true,
                            ..FlightKind::PLAIN
                        },
                    );
                    return self.emit(sense, width);
                }
                self.pending_pounce = true;
            }
        }

        let walk_to_seat = self.console.rest_seated.get() && (target_row - self.row).abs() < 0.5;
        if ((self.pending_pounce && gap.abs() >= POUNCE_JUMP)
            || (self.pursuit_t.is_none()
                && gap.abs() >= POUNCE_GAP
                && !self.runs_down(gap)
                && !walk_to_seat))
            && gap.abs() > ARRIVED
        {
            self.facing_left = gap < 0.0;
            if self.notices(target, target_row, false, sense.cols, width) {
                self.set_action(PetAction::Perk);
                return self.emit(sense, width);
            }
            self.pending_pounce = false;
            self.hop_crouch = false;
            self.big_gather = false;
            self.coil_d = if self.quiet_leap {
                CROUCH_DUR
            } else {
                Self::coil_dur(gap.abs())
            };
            self.set_action(PetAction::Crouch);
            return self.emit(sense, width);
        }
        self.pending_pounce = false;

        let sched = ((self.vhat.abs() - RUN_SPEED) / RUN_SPEED).clamp(0.0, 1.0);
        let gain = CHASE_GAIN + (CHASE_GAIN_FAST - CHASE_GAIN) * sched;
        let on_station = (target - self.station_now(cc, sense.cols, width)).abs() <= 1e-3;
        let limit = f32::from(sense.cols) - width;
        let carrying_run = self.carrying_run();
        let walled = !self.wall_side
            && f32::from(cc) + STATION_LEAD + self.lead() + self.standoff() >= limit;
        let station_v = self.station_velocity();
        let feed = if gap * station_v > 0.0 && on_station && !walled {
            station_v
        } else {
            0.0
        };
        let riding = on_station && !walled && carrying_run;
        let rest = if on_station && !self.wall_side && carrying_run {
            (f32::from(cc) + STATION_LEAD + self.lead_at(f32::INFINITY) + self.standoff())
                .clamp(0.0, limit.max(0.0))
        } else {
            target
        };
        let past_station = riding && gap < 0.0 && rest > self.col;
        let top = self.top_speed();
        let writing_on = match self.key_within(SIT_AFTER) {
            Some(kind) => kind == PetInputKind::Text,
            None => self.last_dir > 0 && self.run_t < SIT_AFTER,
        };
        let awaits_writing = !past_station
            && on_station
            && !self.wall_side
            && gap < -ARRIVED
            && gap > -AWAIT_AHEAD
            && (target_row - self.row).abs() < 0.5
            && writing_on
            && self.pursuit_t.is_none()
            && self.play_land.is_none()
            && !self.stream;
        let want = if past_station {
            self.station_speed().min(top)
        } else if awaits_writing {
            0.0
        } else {
            ((gap.abs() * gain).min(top) * gap.signum() + feed).clamp(-top, top)
        };
        if self.action.settled()
            && self.sit_front
            && self.facing_left
            && on_station
            && move_dc > 0.0
            && gap >= 1.0 - PECK_CLEAR - 0.05
            && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
        {
            self.facing_left = false;
            self.set_action_keep(PetAction::Walk);
        }
        let at_rest = self.action.settled()
            || (!matches!(self.action, PetAction::Walk | PetAction::Run) && self.speed == 0.0);
        let seat_kept = at_rest
            && self.sit_front
            && self.facing_left
            && self.pointer_heat <= 0.0
            && gap.abs() <= ARRIVED
            && self.col
                >= (f32::from(cc) + STATION_LEAD + PECK_CLEAR)
                    .min((f32::from(sense.cols) - width).max(0.0))
                    - 1e-4;
        let stand_held = self.stand_held() && !(at_rest && gap.abs() <= ARRIVED && !riding);
        if stand_held || seat_kept || (self.room_cue_act && at_rest && gap.abs() <= ARRIVED) {
            self.speed = 0.0;
        } else if at_rest && gap.abs() <= ARRIVED && dt > 0.0 && !riding {
            if gap == 0.0 {
                self.speed = 0.0;
            } else {
                let rate = gain * (1.0 - (-SETTLE_TICK / SPEED_TAU).exp());
                self.speed = gap * (1.0 - (-rate * dt).exp()) / dt;
            }
        } else if want != self.speed && dt > 0.0 {
            let tau = if self.vrun >= RUN_SPEED
                && want.abs() > self.speed.abs()
                && want * self.speed >= 0.0
            {
                LAUNCH_TAU
            } else {
                SPEED_TAU
            };
            let k = 1.0 - (-dt / tau).exp();
            self.speed += (want - self.speed) * k;
        }
        let skid_leg = feed == 0.0
            && !carrying_run
            && self.pursuit_t.is_none()
            && self.leg_dist >= BRAKE_DIST
            && self.content >= SKIP_CONTENT;
        if dt > 0.0 && !self.braking && !skid_leg {
            let room = rest - self.col;
            if room * self.speed > 0.0 {
                let half = 0.5 * STOP_DECEL * dt;
                let moving_on = if !riding && feed * self.speed > 0.0 {
                    feed.abs()
                } else {
                    0.0
                };
                let cap = (half * half + 2.0 * STOP_DECEL * room.abs()).sqrt() - half + moving_on;
                if self.speed.abs() > cap {
                    self.speed = cap * self.speed.signum();
                }
            }
        }
        let step = self.speed * dt;
        self.col += step;
        let travelled = step.abs();
        let stride_cells = if self.action == PetAction::Run {
            Self::run_stride_cells(self.speed)
        } else if self.lateral {
            LAT_STRIDE_CELLS
        } else {
            STRIDE_CELLS
        };
        let next_stride = self.stride + travelled / stride_cells;
        self.stride = if (0.0..1024.0).contains(&next_stride) {
            next_stride
        } else {
            next_stride.rem_euclid(1024.0)
        };

        if !self.braking
            && skid_leg
            && (target - self.col).signum() != gap.signum()
            && self.speed.abs() > RUN_SPEED
        {
            self.braking = true;
            self.brake_over = Some(
                (target + BRAKE_OVERSHOOT * gap.signum())
                    .clamp(0.0, (f32::from(sense.cols) - width).max(0.0)),
            );
            self.speed *= 0.15;
            self.stumble_t = STUMBLE_DUR;
            self.skid_puff = false;
            self.skid_dir = gap.signum();
            self.spawn_dust(width);
        }
        if let Some(over) = self.brake_over {
            // Halfway down the run-out the paws throw a second puff, once.
            if !self.skid_puff && (over - self.col).abs() <= 0.5 * BRAKE_OVERSHOOT {
                self.skid_puff = true;
                self.spawn_dust(width);
            }
            if (over - self.col).abs() <= ARRIVED {
                self.brake_over = None;
            }
        }

        let aim = self.brake_over.unwrap_or(if riding && rest > target {
            rest
        } else {
            target
        });
        let prev_col = self.col - step;
        if (aim - self.col).signum() != (aim - prev_col).signum() {
            self.col = aim;
            if riding && self.brake_over.is_none() {
                self.speed = feed.min(self.speed.abs()) * self.speed.signum();
            } else {
                self.speed *= 0.25;
            }
        }

        if (self.action.settled() || matches!(self.action, PetAction::Waking | PetAction::Land))
            && gap.abs() > ARRIVED
        {
            self.facing_left = gap < 0.0;
        } else if self.speed.abs() > FLIP_SPEED {
            self.facing_left = self.speed < 0.0;
        }

        self.home = target;
        let pinned = self.wall_side || target >= f32::from(sense.cols) - width - 1e-3;
        let accelerating = matches!(self.action, PetAction::Walk | PetAction::Run)
            && (target - self.col).abs() > 0.5
            && want.abs() > FLIP_SPEED
            && self.speed.abs() < want.abs();
        let arrived = !accelerating
            && (target - self.col).abs() <= ARRIVED
            && self.speed.abs() < FLIP_SPEED
            && (!riding || pinned || self.carry_speed() < FLIP_SPEED);
        if arrived {
            self.leg_dist = 0.0;
            self.braking = false;
            self.brake_over = None;
            self.content = (self.content - CONTENT_DECAY * elapsed).max(0.0);
            if self.hide_to.is_some() && !self.pending_pounce && !self.pending_big_jump {
                self.begin_hide();
                return self.emit(sense, width);
            }
            if self.consume_stimuli(&sense, cc, width) {
                return self.emit(sense, width);
            }
            if self.consume_v2_offer(&sense, width) {
                self.room_cue_act = true;
                return self.emit(sense, width);
            }
            if self.consume_room(cc, width) {
                self.room_cue_act = true;
                return self.emit(sense, width);
            }
            if self.consume_bat(&sense, width) {
                self.room_cue_act = true;
                return self.emit(sense, width);
            }
            if let Some(lx) = self.pending_look.take() {
                self.look_at = None;
                if !self.room_hold && !matches!(self.action, PetAction::Sleep | PetAction::Waking) {
                    self.facing_left = lx < self.col + width * 0.5;
                    if self.action.settled() && self.action != PetAction::Perk {
                        self.set_action(PetAction::Perk);
                    }
                    self.room_cue_act = true;
                    return self.emit(sense, width);
                }
            }
            if self.consume_pointer_pounce(&sense, width) {
                return self.emit(sense, width);
            }
            if let Some(pair) = self.pending_worn
                && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
                && self
                    .handoff_parked_clock
                    .is_some_and(|at| self.clock - at >= f64::from(HANDOFF_DEBOUNCE))
            {
                let arrival = self.pending_arrival;
                let old = self.worn.unwrap_or(pair);
                self.worn = Some(pair);
                self.pending_worn = None;
                self.handoff_parked_clock = None;
                if old != pair {
                    let quiet = arrival == PetArrival::Quiet;
                    let (edge, speed, arrive_in) = if quiet {
                        (self.col, DEPART_SPEED, HOMECOMING_IN)
                    } else {
                        let limit = (f32::from(sense.cols) - width).max(0.0);
                        let edge = if self.col >= f32::from(cc) {
                            limit
                        } else {
                            0.0
                        };
                        let span = (edge - self.col).abs();
                        (
                            edge,
                            DEPART_SPEED.max(span / (DEPART_MAX - EDGE_FADE)),
                            ARRIVE_IN,
                        )
                    };
                    self.arrive_t = arrive_in;
                    self.arrive_in = arrive_in;
                    self.arrive_from = 0.0;
                    let id = self.next_departure_id();
                    self.spawn_departure(Departure {
                        born: self.lane_clock,
                        id,
                        coat: old.0,
                        iris: old.1,
                        from_col: self.col,
                        row: self.row,
                        edge,
                        speed,
                        still: quiet,
                        facing_left: self.facing_left,
                        stride0: self.stride,
                        pose0: self.last_pose,
                        stance0: self.last_stance,
                        resight: false,
                        own_glass: None,
                        own_clear: false,
                        peak: 1.0,
                        colors: self.colors,
                        surface: self.surface,
                    });
                    if quiet
                        && self.action.settled()
                        && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
                    {
                        self.twitch_t = TWITCH_DUR;
                        self.twitch_up = self.mote_serial.is_multiple_of(2);
                        self.mote_serial = self.mote_serial.wrapping_add(1);
                    }
                }
            }
            if self.groom_owed {
                if self.room_hold {
                    self.groom_owed = false;
                } else if !matches!(self.action, PetAction::Sleep | PetAction::Waking) {
                    if self.action != PetAction::Groom {
                        self.set_action(PetAction::Groom);
                    }
                    return self.emit(sense, width);
                }
            }
            if self.vigil_t >= VIGIL_AFTER
                && !self.vigil
                && self.action.settled()
                && self.action != PetAction::Waking
                && !self.stakeout
                && !self.hiding
                && self.pursuit_t.is_none()
                && self.play_to.is_none()
                && !self.trick_engaged()
            {
                self.vigil = true;
                self.hop_crouch = false;
                self.big_gather = false;
                self.facing_left = f32::from(cc) < self.col + width * 0.5;
                self.speed = 0.0;
                self.set_action(PetAction::Crouch);
                return self.emit(sense, width);
            }
            if self.consume_watch(elapsed, cc) {
                return self.emit(sense, width);
            }
            if self.bored_cool <= 0.0
                && !self.room_lease
                && !self.room_hold
                && self.wriggle_t <= 0.0
                && !self.tennis
                && self.pursuit_t.is_none()
                && !self.stakeout
                && self.pointer_heat <= 0.0
                && self.quiet >= BORED_AFTER
                && self.quiet < BORED_UNTIL
                && self.content >= PLAY_CONTENT
                && self.action.settled()
                && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
            {
                self.bored_cool = BORED_COOL;
                let deal = self.mote_serial % 4;
                self.mote_serial = self.mote_serial.wrapping_add(1);
                if deal == 3
                    && let Some((first, end)) = self.ink_span(self.row)
                    && end - first >= 2.0
                {
                    let dest = (end - width * 0.7).max(first - width * 0.3);
                    if (dest - self.col).abs() <= HIDE_RANGE {
                        self.hide_to = Some(dest);
                        return self.emit(sense, width);
                    }
                }
                if deal == 2 {
                    self.wriggle_t = WRIGGLE_DUR;
                    self.set_action(PetAction::Loaf);
                } else {
                    self.facing_left = f32::from(cc) < self.col + width * 0.5;
                    self.play_frolic = true;
                    self.swipe = true;
                    self.play_hold = BAT_HOLD * if deal == 1 { 2.0 } else { 1.0 };
                    self.set_action(PetAction::Frolic);
                    self.spawn_bat_dust(f32::from(cc), self.row);
                }
                return self.emit(sense, width);
            }
            if !self.stakeout
                && !self.room_hold
                && stroke_point.is_none()
                && self.pursuit_t.is_none()
                && self.pursuit_cool <= 0.0
                && self.quiet >= PURSUIT_QUIET
                && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
                && let Some((px, py)) = self.last_pointer
            {
                let dist = (px - (self.col + width * 0.5))
                    .abs()
                    .max((py - self.row).abs() * 2.0);
                if dist <= STALK_RANGE
                    && dist > PURSUIT_CATCH
                    && self.pointer_speed >= STALK_MIN_SPEED
                    && self.pointer_speed < PURSUIT_MIN_SPEED
                    && self.content >= PLAY_CONTENT
                {
                    self.stakeout = true;
                    self.stakeout_t = 0.0;
                    self.hop_crouch = false;
                    self.big_gather = false;
                    self.facing_left = px < self.col + width * 0.5;
                    self.set_action(PetAction::Crouch);
                    return self.emit(sense, width);
                }
            }
            self.enter_settled(dt);
            let (gaze_col, caret_gaze) = match sense.pointer {
                Some((px, _)) if self.pointer_heat > 0.0 => (px, false),
                _ => (f32::from(cc), true),
            };
            self.settle_gaze(gaze_col, caret_gaze, f32::from(cr), width, elapsed);
            if prev.is_some()
                && !self.room_cue_act
                && self.action == PetAction::Sit
                && self.sit_front
                && self.facing_left
                && self.pointer_heat <= 0.0
                && (f32::from(cr) - self.row).abs() < 0.5
            {
                let clear = (f32::from(cc) + STATION_LEAD + PECK_CLEAR)
                    .min((f32::from(sense.cols) - width).max(0.0));
                if self.col < clear
                    && (clear - target).abs() < ARRIVED
                    && !self.ink_overlaps(clear, self.row, width)
                    && !self.console_bars((clear, self.row), sense.rows, width)
                {
                    self.col = Self::evict_toward(self.col, clear, dt);
                    self.evicting = self.col != clear;
                }
            }
            if !self.room_cue_act {
                self.tend_motes(width);
            }
        } else if stand_held {
            if self.gait_pace != 0.0 && dt > 0.0 {
                self.gait_pace *= (-dt / GAIT_REST_TAU).exp();
            }
            self.gait_rest += dt;
            if matches!(
                self.action,
                PetAction::Walk | PetAction::Run | PetAction::Land
            ) {
                self.set_action_keep(PetAction::Stand);
            }
        } else {
            self.content = (self.content + travelled * CONTENT_PER_CELL).min(1.0);
            self.content = (self.content - CONTENT_MOVING_DECAY * dt).max(0.0);
            self.leg_dist += travelled;
            let v = self.speed.abs();
            let rising = v > self.gait_v;
            self.gait_v = v;
            let fresh = self.gait_rest >= GAIT_DWELL;
            self.gait_rest = 0.0;
            if fresh {
                self.gait_t = self.gait_t.max(GAIT_DWELL);
            }
            if self.speed * self.gait_dir < 0.0 && v > FLIP_SPEED {
                self.gait_pace = v;
                self.gait_t = GAIT_DWELL - GAIT_TURN_SETTLE;
            } else if dt > 0.0 {
                self.gait_pace += (v - self.gait_pace) * (1.0 - (-dt / GAIT_TAU).exp());
            }
            if v > FLIP_SPEED {
                self.gait_dir = self.speed.signum();
            }
            let worn = Self::gait_rank(self.running, self.lateral);
            let fast = if self.running {
                self.gait_pace > RUN_SPEED - GAIT_HYST
            } else {
                self.gait_pace > RUN_SPEED + GAIT_HYST
            };
            let lateral = if self.lateral {
                self.gait_pace < LAT_SPEED + LAT_HYST
            } else {
                self.gait_pace < LAT_SPEED
            };
            let mut asked = Self::gait_rank(fast, lateral);
            if v > WALK_CEIL {
                asked = 2;
            } else if v > AMBLE_CEIL {
                asked = asked.max(1);
            }
            let ceiling = [AMBLE_CEIL, WALK_CEIL, f32::INFINITY][usize::from(worn)];
            if asked != worn
                && (v > ceiling
                    || (self.gait_t >= GAIT_DWELL && (fresh || !(asked < worn && rising))))
            {
                self.running = asked == 2;
                self.lateral = asked == 0;
                self.gait_t = 0.0;
            }
            let fast = self.running;
            if fast
                && self.speed.abs() > STUMBLE_SPEED
                && self.stumble_t <= 0.0
                && self.stride.fract() < travelled / RUN_STRIDE_CELLS
                && (self.stride as u32).wrapping_add(u32::from(self.mote_serial)) % 5 == 0
            {
                self.stumble_t = STUMBLE_DUR;
                self.skid_dir = self.speed.signum();
                self.spawn_dust(width);
            }
            self.set_action_keep(if fast {
                PetAction::Run
            } else {
                PetAction::Walk
            });
        }

        self.emit(sense, width)
    }

    /// Tick for an explicit windowless still capture.
    pub fn tick_static_capture(&mut self, sense: PetSense) -> PetFrame {
        let frame = self.tick(sense);
        if !self.console_observation_incoherent()
            && sense.caret.is_some()
            && frame.alpha == 0
            && self.needs_frames()
        {
            self.alpha = 1.0;
            return self.emit(sense, art_cols(sense.cell_w, sense.cell_h));
        }
        frame
    }

    fn on_move(&mut self, dr: f32, dc: f32, caret_col: f32, cols: u16, reduced: bool) {
        let stood = self.lead();
        let dir = if dc > 0.0 {
            1
        } else if dc < 0.0 {
            -1
        } else {
            0
        };
        if dir != 0 {
            if self.last_dir != 0 && dir != self.last_dir {
                self.reversals = self.reversals.saturating_add(1);
                self.reversal_t = 0.0;
            }
            self.last_dir = dir;
            if dir == self.run_dir && self.run_t <= RHYTHM_WINDOW {
                self.run_count = self.run_count.saturating_add(1);
                self.run_gap = self.run_t;
                self.run_cells = self.run_cells * PACE_KEEP + dc.abs();
                self.run_secs = self.run_secs * PACE_KEEP + self.run_gap;
                self.run_step = self.run_step.max(dc.abs());
                if self.run_secs > 0.0 {
                    self.run_pace = (self.run_cells / self.run_secs).clamp(0.0, VEL_MAX);
                }
            } else {
                self.run_dir = dir;
                self.run_count = 1;
                self.run_gap = 0.0;
                self.run_pace = 0.0;
                self.run_cells = 0.0;
                self.run_secs = 0.0;
                self.run_step = dc.abs();
            }
            self.run_t = 0.0;
            self.lead_room = self.lead_room.max(
                (LEAD_STEP * f32::from(self.run_count.saturating_sub(RHYTHM_MOVES - 1)))
                    .min(LEAD_MAX),
            );
        }
        self.key_beat = if dir > 0 && dr == 0.0 && self.run_gap > 0.0 {
            let gap = self.run_gap / dc;
            if self.key_beat > 0.0 {
                let gap = gap.min(self.key_beat * KEY_BEAT_LATE);
                self.key_beat + (gap - self.key_beat) * KEY_BEAT_BLEND
            } else {
                gap
            }
        } else {
            0.0
        };
        if dr != 0.0 || dc.abs() >= POUNCE_JUMP {
            self.held_lead = 0.0;
        } else {
            self.held_lead = (stood - dc.abs()).max(0.0);
        }
        self.key_pace = 0.0;
        if reduced {
            return;
        }
        if self.stream {
            if self.action == PetAction::Sleep {
                self.set_action(PetAction::Waking);
            }
            return;
        }
        let line_change = dr.abs() >= 1.0;
        self.quiet_leap |= line_change;
        if line_change {
            self.clear_play_intents();
        }
        if dc > 0.0 || line_change {
            self.retreat_bottled = false;
        }
        self.vhat = (self.vhat + dc / VEL_TAU).clamp(-VEL_MAX, VEL_MAX);
        self.vrun = if self.run_gap > 0.0 && dc > 0.0 {
            (dc / self.run_gap).clamp(0.0, VEL_MAX).max(self.vrun)
        } else {
            0.0
        };
        self.key_pace = if self.key_beat > 0.0 {
            (1.0 / self.key_beat).min(VEL_MAX)
        } else {
            0.0
        };

        let width = art_cols(self.last_cell_w.max(1), self.last_cell_h.max(1));
        let caret_encroaches = self
            .last_caret
            .is_some_and(|(cr, _)| (self.row - f32::from(cr)).abs() < 0.5)
            && self.col < caret_col + STATION_LEAD
            && self.col + width > caret_col - STATION_LEAD;

        if self.pending_vigil_pounce && self.action == PetAction::Crouch {
            return;
        }

        if self.tennis {
            if dc.abs() >= POUNCE_JUMP || (dr == 0.0 && dir > 0 && self.run_count > RHYTHM_MOVES) {
                self.tennis = false;
            } else {
                self.tennis_last = self.clock;
                if dc.abs() > 0.0 {
                    self.facing_left = dc < 0.0;
                }
                return;
            }
        }

        let woke = self.action == PetAction::Sleep;
        if woke {
            self.pending_drop = dr == 1.0;
            if self.pending_drop && dc.abs() >= POUNCE_JUMP {
                self.pending_pounce = true;
            }
            self.set_action(PetAction::Waking);
            return;
        }
        if dr == 0.0
            && dir > 0
            && self.run_count >= 2
            && (self.action == PetAction::Waking
                || (caret_encroaches
                    && !self.play_frolic
                    && self.trick_go.is_none()
                    && !self.pending_pounce
                    && !self.pending_big_jump
                    && matches!(
                        self.action,
                        PetAction::Perk | PetAction::Startle | PetAction::Frolic
                    )))
        {
            self.facing_left = false;
            self.flinch_t = 0.0;
            self.set_action(PetAction::Walk);
        }

        let airborne = self.action == PetAction::Leap;
        if dr == 0.0 && dc < 0.0 {
            if dc <= -SNAP_COLS {
                // The snap ([`SNAP_COLS`]): a jump home, not a threat. Face the
                // caret, forget the held-delete run, and latch a quiet jump.
                if !airborne {
                    self.facing_left = caret_col < self.col;
                }
                self.retreat_run = 0;
                self.retreat_gap = 0.0;
                self.pending_pounce = true;
                self.quiet_leap = true;
                self.clear_play_intents();
                return;
            }
            self.retreat_run = if self.retreat_gap <= STARTLE_RUN_WINDOW {
                self.retreat_run.saturating_add(1)
            } else {
                1
            };
            if self.retreat_run == 1 {
                self.retreat_bottled = false;
            }
            self.retreat_gap = 0.0;
            if !airborne {
                self.facing_left = caret_col < self.col;
            }
            if self.retreat_bottled {
                // It follows the caret back now.
                return;
            }
            if dc <= -STARTLE_COLS || self.retreat_run >= STARTLE_RUN {
                self.retreat_bottled = true;
                self.set_action(PetAction::Startle);
            } else {
                self.flinch_t = FLINCH_DUR;
                self.set_action(PetAction::Perk);
            }
            return;
        }

        if self.reversals >= FROLIC_REVERSALS && self.action != PetAction::Frolic {
            self.reversals = 0;
            if (self.clock - self.last_frolic) as f32 <= TENNIS_AFTER {
                self.tennis = true;
                self.tennis_last = self.clock;
                self.sit_front = true;
                self.set_action(PetAction::Sit);
            } else {
                self.last_frolic = self.clock;
                self.set_action(PetAction::Frolic);
            }
            return;
        }

        if dc.abs() >= POUNCE_JUMP {
            self.pending_pounce = true;
            self.clear_play_intents();
            if !line_change
                && (dc.abs() >= BIG_JUMP_COLS.max(BIG_JUMP_COLS_FRAC * f32::from(cols))
                    || (self.action.settled() && self.content >= JOY_CONTENT))
            {
                self.pending_big_jump = true;
            }
            if self.at_rest() && self.action != PetAction::Perk {
                if (caret_col - self.col).abs() > 1e-4 {
                    self.facing_left = caret_col < self.col;
                }
                self.set_action(PetAction::Perk);
            }
        }
    }

    #[inline]
    fn plant_stride(&mut self, dt: f32) {
        let target = self.stride.round();
        if (target - self.stride).abs() < STRIDE_PLANT {
            self.stride = target;
        } else if dt > 0.0 {
            self.stride += (target - self.stride) * (1.0 - (-dt / STRIDE_EASE).exp());
        }
    }

    fn enter_settled(&mut self, dt: f32) {
        if self.console.rest_seated.get()
            && self.pending_trick.is_none()
            && self.quiet < SIT_AFTER + SIT_FOLD
        {
            self.quiet = SIT_AFTER + SIT_FOLD;
        }
        self.speed = 0.0;
        if self.gait_pace != 0.0 && dt > 0.0 {
            self.gait_pace *= (-dt / GAIT_REST_TAU).exp();
        }
        self.gait_rest += dt;
        self.plant_stride(dt);
        self.quiet_leap = false;
        self.drop_stare = false;

        if !self.action.settled() {
            self.settle_serial = self.settle_serial.wrapping_add(1);
            self.settle_seed = self.settle_serial.wrapping_add(self.mote_serial);
        }

        if self.trick_settle.is_some()
            && self.trick_settle != Some(Trick::Sleep)
            && self.quiet >= self.hold_end_q
        {
            self.clear_trick_settle();
        }
        let next = if self.trick_settle == Some(Trick::Down) {
            PetAction::Loaf
        } else if self.trick_settle == Some(Trick::Sit) {
            // Commanded seats are checked above the ladder's sleep, so a cat
            // already past [`SLEEP_AFTER`] still wears the seat for the hold.
            PetAction::Sit
        } else if self.quiet >= SLEEP_AFTER
            && (!self.waiting_up() || self.trick_settle == Some(Trick::Sleep))
        {
            PetAction::Sleep
        } else if self.quiet >= SIT_AFTER && self.room_hold {
            // Distinct branch.
            PetAction::Sit
        } else if self.quiet >= SIT_AFTER {
            let since_sit = self.quiet - SIT_AFTER;
            let groom_after = self.groom_after();
            let cycle = groom_after + GROOM_DUR;
            let in_wash = since_sit >= groom_after && (since_sit % cycle) >= groom_after;
            let second_wash = since_sit >= cycle && since_sit < 2.0 * cycle;
            if in_wash && !(second_wash && self.groom_skip()) {
                PetAction::Groom
            } else {
                let loaf_after = self.loaf_after();
                if self.quiet >= loaf_after {
                    let owed = CONTENT_DECAY * (since_sit).clamp(0.0, loaf_after - SIT_AFTER);
                    if self.content + owed >= PURR_GATE && !self.settle_seed.is_multiple_of(3) {
                        PetAction::Purr
                    } else {
                        PetAction::Loaf
                    }
                } else {
                    PetAction::Sit
                }
            }
        } else if self.action == PetAction::Waking {
            // The wake's stretch ends in the seat it is drawn in.
            PetAction::Sit
        } else {
            PetAction::Stand
        };
        self.set_action_keep(next);
    }

    #[inline]
    const fn purring(&self) -> bool {
        matches!(self.action, PetAction::Purr)
            || (matches!(self.action, PetAction::Sit | PetAction::Loaf)
                && self.content >= PURR_GATE)
    }

    #[inline]
    const fn loaf_after(&self) -> f32 {
        LOAF_AFTER + 0.8 * (self.settle_seed % 3) as f32
    }

    #[inline]
    const fn groom_after(&self) -> f32 {
        GROOM_AFTER - 0.45 * ((self.settle_seed / 3) % 3) as f32
    }

    #[inline]
    const fn groom_skip(&self) -> bool {
        self.settle_seed % 4 == 3
    }

    #[cfg(test)]
    const fn force_seed(&mut self, settle: u8) {
        self.settle_seed = settle;
    }

    #[inline]
    const fn sit_beat(&self, period: f32, at: f32) -> Option<(i64, f32)> {
        self.sit_beat_at(self.quiet - SIT_AFTER, period, at)
    }

    #[inline]
    const fn sit_beat_at(&self, since: f32, period: f32, at: f32) -> Option<(i64, f32)> {
        if since < 0.0 {
            return None;
        }
        let beat = (since / period).floor();
        let phase = since - beat * period - at;
        if phase >= 0.0 {
            Some((beat as i64, phase))
        } else {
            None
        }
    }

    #[inline]
    const fn beat_deal(&self, beat: i64, n: i64) -> i64 {
        (beat + self.settle_seed as i64).rem_euclid(n)
    }

    const fn sit_tail(&self) -> Option<PetGlyphId> {
        let (period, at) = self.tail_clock();
        let (beat, p) = if self.sit_front {
            let Some((beat, p)) = self.sit_beat(period, at) else {
                return None;
            };
            if !self.side_on_holds(p, Self::tail_hand_len(self.beat_deal(beat, 4))) {
                return None;
            }
            (beat, p)
        } else {
            let Some(bp) = self.sit_beat(period, at) else {
                return None;
            };
            bp
        };
        match self.beat_deal(beat, 4) {
            0 => {
                if p < TAIL_FLICK {
                    Some(PetGlyphId::PetSitFlick)
                } else {
                    None
                }
            }
            1 => {
                if p < TAIL_TAP || (p >= TAIL_TAP + TAIL_GAP && p < 2.0 * TAIL_TAP + TAIL_GAP) {
                    Some(PetGlyphId::PetSitFlick)
                } else {
                    None
                }
            }
            2 => match (p / TAIL_SWING).floor() as i64 {
                0 | 6 => Some(PetGlyphId::PetSitFlick),
                1 | 2 | 5 => Some(PetGlyphId::PetSitTailBack),
                3 | 4 => Some(PetGlyphId::PetSitTailLow),
                _ => None,
            },
            _ => None,
        }
    }

    #[inline]
    const fn tail_hand_len(hand: i64) -> f32 {
        match hand {
            0 => TAIL_FLICK,
            1 => 2.0 * TAIL_TAP + TAIL_GAP,
            2 => 7.0 * TAIL_SWING,
            _ => 0.0,
        }
    }

    #[inline]
    const fn tail_clock(&self) -> (f32, f32) {
        if self.sit_front {
            (self.glance_period(), GLANCE_AT)
        } else {
            (
                TAIL_PERIOD + TAIL_PERIOD_STEP * (self.settle_seed % 3) as f32,
                TAIL_AT,
            )
        }
    }

    #[inline]
    const fn tail_room(&self, elapsed: f32, len: f32) -> bool {
        let (period, at) = self.tail_clock();
        let Some((beat, p)) = self.sit_beat(period, at) else {
            return true;
        };
        p - elapsed >= Self::tail_hand_len(self.beat_deal(beat, 4)) && p + len - elapsed <= period
    }

    #[inline]
    const fn glance_period(&self) -> f32 {
        GLANCE_PERIOD + GLANCE_PERIOD_STEP * (self.settle_seed % 3) as f32
    }

    #[inline]
    const fn look_dur(&self) -> f32 {
        if self.loaf_after() - SIT_AFTER >= GLANCE_AT + LOOK_DUR_LONG + LOOK_RETURN_MIN {
            LOOK_DUR_LONG
        } else {
            LOOK_DUR
        }
    }

    #[inline]
    const fn sit_blink_beat(&self) -> Option<(f32, f32)> {
        self.sit_blink_beat_at(self.quiet - SIT_AFTER)
    }

    #[inline]
    const fn sit_blink_shows(&self) -> Option<(f32, f32)> {
        let Some((p, len)) = self.sit_blink_beat() else {
            return None;
        };
        let on_glass = matches!(
            self.last_pose,
            PetGlyphId::PetSitBlink | PetGlyphId::PetSitFrontBlink
        );
        let left = if p < BLINK_DUR {
            BLINK_DUR - p
        } else {
            len - p
        };
        if on_glass || left >= STILL_SECS {
            Some((p, len))
        } else {
            None
        }
    }

    #[inline]
    const fn sit_blink_beat_at(&self, since: f32) -> Option<(f32, f32)> {
        let period = BLINK_PERIOD + BLINK_PERIOD_STEP * ((self.settle_seed / 2) % 3) as f32;
        let Some((beat, p)) = self.sit_beat_at(since, period, BLINK_AT) else {
            return None;
        };
        if self.beat_deal(beat, 3) == 2 {
            let len = 2.0 * BLINK_DUR + BLINK_GAP;
            if p < BLINK_DUR || (p >= BLINK_DUR + BLINK_GAP && p < len) {
                Some((p, len))
            } else {
                None
            }
        } else if p < BLINK_DUR {
            Some((p, BLINK_DUR))
        } else {
            None
        }
    }

    #[inline]
    const fn sit_yawn(&self) -> bool {
        if self.room_hold || !self.settle_seed.is_multiple_of(2) {
            return false;
        }
        let loaf_after = self.loaf_after();
        self.quiet >= loaf_after - YAWN_DUR && self.quiet < loaf_after
    }

    #[inline]
    const fn sit_glance_left(&self) -> Option<f32> {
        if self.pointer_heat > 0.0 {
            return None;
        }
        let Some((beat, p)) = self.sit_beat(self.glance_period(), GLANCE_AT) else {
            return None;
        };
        let len = self.look_dur();
        if p < len && self.beat_deal(beat, 3) != 0 {
            Some(len - p)
        } else {
            None
        }
    }

    #[inline]
    const fn sit_look_left(&self) -> Option<f32> {
        if self.pointer_heat > 0.0 {
            return None;
        }
        if self.sit_yawn() {
            return Some(self.loaf_after() - self.quiet);
        }
        self.sit_glance_left()
    }

    #[inline]
    const fn sit_glance(&self) -> bool {
        self.sit_look_left().is_some()
    }

    #[inline]
    const fn side_on_holds(&self, phase: f32, len: f32) -> bool {
        if !self.sit_front {
            return true;
        }
        match self.sit_look_left() {
            Some(left) => left + phase >= len,
            None => false,
        }
    }

    #[inline]
    const fn coil_dur(span: f32) -> f32 {
        (CROUCH_DUR + span * COIL_PER_CELL).clamp(CROUCH_DUR, COIL_MAX)
    }

    #[inline]
    const fn coil_envelope(t: f32, dur: f32) -> (f32, f32) {
        let q_full = COIL_SQUASH * (dur / COIL_MAX).min(1.0);
        let release0 = (dur - COIL_RELEASE).max(f32::EPSILON);
        if t < release0 {
            let q = q_full * (t / release0).min(1.0);
            (1.0 - q, 1.0 / (1.0 - q))
        } else {
            let r = ((t - release0) / COIL_RELEASE).clamp(0.0, 1.0);
            let (y0, x0) = (1.0 - q_full, 1.0 / (1.0 - q_full));
            let (y1, x1) = (1.0 - 0.6 * LAUNCH_STRETCH, 1.0 + LAUNCH_STRETCH);
            (y0 + (y1 - y0) * r, x0 + (x1 - x0) * r)
        }
    }

    #[inline]
    fn flight_lift(arc: f32, u: f32, big: bool) -> f32 {
        if big {
            if u < 0.5 {
                let s = 1.0 - 2.0 * u;
                arc * (1.0 - s * s * s)
            } else {
                arc * (1.0 - (2.0 * u - 1.0).powf(HANG_P_DOWN))
            }
        } else {
            arc * 4.0 * u * (1.0 - u)
        }
    }

    #[inline]
    const fn pose_schedule(dur: f32) -> (f32, f32) {
        let (rise, desc) = (LEAP_RISE_T, LEAP_DESC_T);
        if rise + desc + MIN_HERO > dur {
            let k = (dur - MIN_HERO) / (rise + desc);
            (rise * k, desc * k)
        } else {
            (rise, desc)
        }
    }

    #[inline]
    const fn flight_pose(f: &Flight) -> PetGlyphId {
        let t = f.t.clamp(0.0, f.dur);
        if t <= 0.0 && f.coiled {
            return PetGlyphId::PetLaunch;
        }
        if f.drop {
            return if t < DROP_SLEEP {
                PetGlyphId::PetSleep1
            } else if t < DROP_SLEEP + DROP_STRETCH {
                PetGlyphId::PetStretch
            } else {
                PetGlyphId::PetLeapDescend
            };
        }
        if f.hop {
            return if t < 0.25 * f.dur {
                PetGlyphId::PetLeapRise
            } else if t < 0.65 * f.dur {
                PetGlyphId::PetHop
            } else {
                PetGlyphId::PetLeapDescend
            };
        }
        if f.dur < 3.0 * MIN_HERO {
            return if t < 0.5 * f.dur {
                PetGlyphId::PetLeapRise
            } else {
                PetGlyphId::PetLeapDescend
            };
        }
        let (rise, desc) = Self::pose_schedule(f.dur);
        if t < rise {
            PetGlyphId::PetLeapRise
        } else if t > f.dur - desc {
            PetGlyphId::PetLeapDescend
        } else if f.big && f.dur - rise - desc >= 4.0 * MIN_HERO && t >= 0.5 * (rise + f.dur - desc)
        {
            PetGlyphId::PetApex
        } else {
            PetGlyphId::PetLeap
        }
    }

    #[inline]
    const fn flight_shape(span: f32, vertical: bool) -> (f32, f32) {
        if vertical {
            (
                (HOP_DUR + span * FLIGHT_PER_CELL * 0.5).clamp(HOP_DUR, FLIGHT_MAX),
                (HOP_ARC + span * HOP_ARC_PER_CELL).min(HOP_ARC_MAX),
            )
        } else {
            (
                (FLIGHT_MIN + span * FLIGHT_PER_CELL).clamp(FLIGHT_MIN, FLIGHT_MAX),
                (ARC_BASE + span * ARC_PER_CELL).min(ARC_MAX),
            )
        }
    }

    #[inline]
    fn station_drift(&self, t: f32) -> f32 {
        if !self.aims_ahead() {
            return 0.0;
        }
        let k = 1.0 - (-t / VEL_TAU).exp();
        let keep_ahead = self.keep_ahead();
        let stand = keep_ahead.max(self.held_lead);
        let lead = stand + self.carry(stand, self.run_t);
        (self.pace() * VEL_TAU * k + keep_ahead * (1.0 - k) - lead).clamp(0.0, FLIGHT_LEAD_MAX)
    }

    #[inline]
    fn hop_aim(&self, to_col: f32, to_row: f32, vertical: bool, cols: u16, width: f32) -> f32 {
        if !self.aims_ahead() {
            return to_col;
        }
        let (dur0, _) = Self::flight_shape(span_eq(to_col - self.col, to_row - self.row), vertical);
        let lead = self.station_drift(dur0 + LAND_DUR);
        (to_col + lead).clamp(0.0, (f32::from(cols) - width).max(0.0))
    }

    #[inline]
    fn predicted_tier(
        &self,
        to_col: f32,
        to_row: f32,
        vertical: bool,
        cols: u16,
        width: f32,
    ) -> Tier {
        let aim = self.hop_aim(to_col, to_row, vertical, cols, width);
        if span_eq(aim - self.col, to_row - self.row) <= POUNCE_SPAN_MAX {
            Tier::Hop
        } else {
            Tier::Bound
        }
    }

    #[inline]
    const fn at_rest(&self) -> bool {
        self.action.settled()
            && !(matches!(self.action, PetAction::Stand) && self.action_t < STAND_REST)
    }

    #[inline]
    fn notices(&self, to_col: f32, to_row: f32, vertical: bool, cols: u16, width: f32) -> bool {
        self.at_rest()
            && self.action != PetAction::Perk
            && !self.drop_stare
            && !self.ink_overlaps(self.col, self.row, width)
            && self.predicted_tier(to_col, to_row, vertical, cols, width) == Tier::Bound
    }

    fn leave_ground(
        &mut self,
        to_col: f32,
        to_row: f32,
        vertical: bool,
        cols: u16,
        width: f32,
        policy: LeapPolicy,
    ) {
        // The aim does not read the quiet-leap latch (its prediction is the
        // decay-gated [`Self::station_drift`]); the latch only rules the show,
        // and is spent at the end of this function.
        if policy != LeapPolicy::Play {
            self.pending_wall_transit = false;
            self.pending_pounce = false;
        }
        self.launch_running = policy == LeapPolicy::Quiet && self.rhythm_open();
        let (aim, span, tier) = match policy {
            LeapPolicy::Show => (to_col, 0.0, Tier::Bound),
            LeapPolicy::Play => {
                let span = span_eq(to_col - self.col, to_row - self.row);
                let tier = if span <= POUNCE_SPAN_MAX {
                    Tier::Hop
                } else {
                    Tier::Bound
                };
                (to_col, span, tier)
            }
            LeapPolicy::Quiet => {
                let aim = self.hop_aim(to_col, to_row, vertical, cols, width);
                let span = span_eq(aim - self.col, to_row - self.row);
                let tier = if span <= POUNCE_SPAN_MAX {
                    Tier::Hop
                } else {
                    Tier::Bound
                };
                (aim, span, tier)
            }
        };
        match tier {
            Tier::Hop => {
                let (dur, arc) = Self::flight_shape(span, vertical);
                let arc = self.arc_under_letters(aim, to_row, width, arc);
                self.begin_arc(
                    aim,
                    to_row,
                    dur,
                    arc,
                    FlightKind {
                        hop: vertical && span <= HOP_TIGHT,
                        ..FlightKind::PLAIN
                    },
                );
            }
            Tier::Bound => {
                self.launch_big(to_col, to_row, cols, width, policy == LeapPolicy::Show);
            }
        }
        self.quiet_leap = false;
        self.drop_stare = false;
        self.launch_running = false;
        if policy != LeapPolicy::Play {
            self.wrap_back = false;
        }
    }

    fn begin_arc(&mut self, to_col: f32, to_row: f32, dur: f32, arc: f32, kind: FlightKind) {
        // NO STILL POSE IS A ONE-FRAME FLASH BEFORE A FLIGHT. A still pose
        // just put on glass (a wake's last beat, a landing's seat, a play
        // bow, a notice) stays on it [`STILL_SECS`] before the cat leaves the
        // ground from it; taken at once, it flashed for a single frame
        // between the pose before it and the leap. The flight leaves on a
        // later tick ([`Self::deferred_arc`]), as aimed, under two 60 Hz
        // frames late, on the words or off them.
        if !kind.drop && self.flight.is_none() && self.still_fresh() {
            self.deferred_arc = Some((to_col, to_row, dur, arc, kind));
            return;
        }
        self.deferred_arc = None;
        let FlightKind {
            big,
            show,
            hop,
            drop,
        } = kind;
        let bound = if big {
            BIG_HOP_SPEED_MAX
        } else {
            HOP_SPEED_MAX
        };
        let span = span_eq(to_col - self.col, to_row - self.row);
        let rate = span / dur.max(0.05);
        debug_assert!(
            rate <= bound + 1e-3,
            "no flight may outrun the animal that flies it: {rate:.1} c/s over \
             the {bound:.0} c/s launch bound ({from:.2} -> {to_col:.2} in \
             {dur:.3}s, big={big})",
            from = self.col
        );
        debug_assert!(
            !show || big,
            "the chosen show is always a bound, never a hop"
        );
        let dur = dur.max(span / bound);
        if to_col > self.col {
            self.facing_left = false;
        } else if to_col < self.col {
            self.facing_left = true;
        }
        self.flight = Some(Flight {
            from_col: self.col,
            from_row: self.row,
            to_col,
            to_row,
            dur: dur.max(0.05),
            arc,
            t: 0.0,
            reaimed: false,
            big,
            show,
            hop,
            coiled: self.action == PetAction::Crouch
                && !self.hiding
                && !self.vigil
                && !self.stakeout,
            drop,
            late_move: false,
            running: self.launch_running,
            awaits_wrap: self.wrap_ahead == Some(WrapAhead::Due),
            dur0: dur.max(0.05),
        });
        if self.wrap_ahead == Some(WrapAhead::Due) {
            self.wrap_ahead = Some(WrapAhead::Launched(to_col, to_row));
        }
        self.set_action(PetAction::Leap);
    }

    #[inline]
    const fn landing_dur(f: &Flight) -> f32 {
        if f.running {
            TOUCH_LAND_DUR
        } else if f.big {
            BIG_LAND_DUR
        } else {
            LAND_DUR
        }
    }

    #[inline]
    const fn big_shape(span: f32) -> (f32, f32) {
        (
            (BIG_FLIGHT_BASE + span * BIG_FLIGHT_PER_CELL).clamp(BIG_FLIGHT_BASE, BIG_FLIGHT_MAX),
            (BIG_ARC_BASE + span * BIG_ARC_PER_CELL).clamp(BIG_ARC_BASE, BIG_ARC_MAX),
        )
    }

    fn begin_big_arc(&mut self, to_col: f32, to_row: f32, cols: u16, show: bool) {
        let span = span_eq(to_col - self.col, to_row - self.row);
        let (dur, arc) = Self::big_shape(span);
        let dur = dur.max(span / quiet_speed_max(cols));
        let mid_row = (self.row + to_row) * 0.5;
        let arc = arc.min((mid_row + 1.0 - ART_ROWS).max(0.0));
        self.begin_arc(
            to_col,
            to_row,
            dur,
            arc,
            FlightKind {
                big: true,
                show,
                ..FlightKind::PLAIN
            },
        );
    }

    fn launch_big(&mut self, to_col: f32, to_row: f32, cols: u16, width: f32, show: bool) {
        let span0 = span_eq(to_col - self.col, to_row - self.row);
        let (dur0, _) = Self::big_shape(span0);
        let dur0 = dur0.max(span0 / quiet_speed_max(cols));
        let drift = if self.play_land.is_none() {
            self.station_drift(dur0 + BIG_LAND_DUR)
        } else {
            0.0
        };
        let mut aim = to_col + drift;
        aim = aim.clamp(0.0, (f32::from(cols) - width).max(0.0));
        let delta = aim - self.col;
        let dir = if delta > 0.0 {
            1.0
        } else if delta < 0.0 {
            -1.0
        } else {
            0.0
        };
        let aim_short = if delta.abs() > SKID_CELLS && !self.launch_running {
            aim - dir * SKID_CELLS
        } else {
            aim
        };
        let split = if show {
            delta.abs() >= TWO_BOUND_FRAC * f32::from(cols)
        } else {
            delta.abs() / quiet_speed_max(cols) > QUIET_ONE_BOUND_SECS
        };
        if split {
            let mid = self.col + delta * BOUND_SPLIT;
            let touch = self.ink_safe_col(mid, to_row, width, cols);
            let (lo, hi) = if delta > 0.0 {
                (self.col + 4.0, aim_short - 4.0)
            } else {
                (aim_short + 4.0, self.col - 4.0)
            };
            if !self.ink_overlaps(touch, to_row, width) && touch >= lo && touch <= hi {
                self.bound2 = Some((aim_short, to_row, show));
                self.begin_big_arc(touch, to_row, cols, show);
            } else {
                self.bound2 = None;
                self.begin_big_arc(aim_short, to_row, cols, show);
            }
        } else {
            self.bound2 = None;
            self.begin_big_arc(aim_short, to_row, cols, show);
        }
    }

    const fn spawn_dust(&mut self, width: f32) {
        let heel = self.col + width * 0.5;
        let mut k = 0u8;
        while k < 3 {
            let seed = self.mote_serial.wrapping_add(k);
            self.spawn_mote(Mote {
                kind: PetMoteKind::Dust,
                born: self.clock,
                life: 0.45 + 0.07 * (k as f32),
                col: heel + 0.23 + 0.17 * ((k % 3) as f32),
                row: self.row + 0.31,
                dir: -self.skid_dir,
                seed,
                rgb: None,
            });
            k += 1;
        }
        self.mote_serial = self.mote_serial.wrapping_add(3);
    }

    const fn spawn_mote(&mut self, mut m: Mote) {
        if matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart) {
            m.rgb = self.mote_rgb;
        }
        let mut i = 0;
        while i < self.motes.len() {
            if self.motes[i].is_none() {
                self.motes[i] = Some(m);
                return;
            }
            i += 1;
        }
    }

    const fn spawn_dealt(&mut self, kind: PetMoteKind, life: f32, (col, row): (f32, f32)) {
        let seed = self.mote_serial;
        self.mote_serial = self.mote_serial.wrapping_add(1);
        self.spawn_mote(Mote {
            kind,
            born: self.clock,
            life,
            col,
            row,
            dir: if self.facing_left { -1.0 } else { 1.0 },
            seed,
            rgb: None,
        });
    }

    fn settle_gaze(
        &mut self,
        target_col: f32,
        caret_gaze: bool,
        caret_row: f32,
        width: f32,
        elapsed: f32,
    ) {
        if !self.action.settled() || self.quiet < SETTLE_TURN {
            self.sit_front = false;
            self.peek = false;
            self.look_row = 0;
            self.gaze_dwell = 0.0;
            return;
        }
        let drow = caret_row - self.row;
        self.look_row = if drow <= -0.5 {
            -1
        } else if drow >= 0.5 {
            1
        } else {
            0
        };
        let asleep = self.action == PetAction::Sleep;
        let gap = self.col - target_col;
        let off = target_col + 0.5 - (self.col + width * 0.5);
        let band = if asleep {
            SLEEP_FLIP_BAND
        } else {
            GAZE_FLIP_BAND
        };
        let side = if off < -band {
            Some(true)
        } else if off > band {
            Some(false)
        } else {
            None
        };
        self.sit_front = gap.abs() < SIT_FRONT_GAP;
        let behind =
            side.is_some_and(|left| left != self.facing_left) && !(caret_gaze && self.sit_front);
        if self.sit_front {
            // The dwell only runs here, where a turn is possible, so a target
            // crossing back rewinds it.
            if behind {
                self.gaze_dwell += elapsed;
            } else {
                self.gaze_dwell = 0.0;
            }
            let dwell = if asleep {
                SLEEP_FLIP_DWELL
            } else {
                SETTLE_FLIP_DWELL
            };
            let deep = asleep && self.quiet >= SLEEP_AFTER + BREATH_WINDOW;
            if behind && !deep && self.gaze_dwell >= dwell {
                self.facing_left = !self.facing_left;
                self.gaze_dwell = 0.0;
            }
            self.peek = false;
        } else {
            self.gaze_dwell = 0.0;
            self.peek = behind;
        }
    }

    #[inline]
    const fn head_anchor(&self, width: f32) -> (f32, f32) {
        let x = if self.facing_left { 0.28 } else { 0.72 };
        (self.col + width * x + 0.19, self.row - 0.61)
    }

    fn tend_motes(&mut self, width: f32) {
        match self.action {
            PetAction::Sleep => {
                // light sleep only: no new z once its life would outlive the
                // breath window, so deep sleep begins with an empty lane.
                if self.quiet > SLEEP_AFTER + BREATH_WINDOW - ZEE_LIFE {
                    return;
                }
                let beat = ((self.quiet - SLEEP_AFTER) / ZEE_EVERY).floor() as i64;
                if beat >= 1
                    && beat != self.mote_mark
                    && self
                        .motes
                        .iter()
                        .flatten()
                        .filter(|m| m.kind == PetMoteKind::Zee)
                        .count()
                        < ZEE_ALIVE_MAX
                {
                    self.mote_mark = beat;
                    self.spawn_dealt(PetMoteKind::Zee, ZEE_LIFE, self.head_anchor(width));
                }
            }
            PetAction::Purr | PetAction::Sit | PetAction::Loaf if self.purring() => {
                let beat = ((self.quiet - SIT_AFTER).max(0.0) / PURR_MOTE_EVERY).floor() as i64;
                if beat >= 1 && beat != self.mote_mark {
                    self.mote_mark = beat;
                    self.spawn_chest_mote(None, width);
                }
            }
            _ => {}
        }
    }

    const fn spawn_wake_pop(&mut self, width: f32) {
        self.spawn_dealt(PetMoteKind::ZeePop, ZEE_POP_LIFE, self.head_anchor(width));
    }

    fn consume_stimuli(&mut self, sense: &PetSense, caret_col: u16, width: f32) -> bool {
        // Fright first: the bell interrupts what the others would start.
        if self.pending_bell.take().is_some() {
            self.quiet = 0.0;
            if self.action == PetAction::Sleep {
                self.set_action(PetAction::Waking);
                self.spawn_wake_pop(width);
            } else {
                self.facing_left = f32::from(caret_col) < self.col;
                self.flinch_t = FLINCH_DUR;
                self.set_action(PetAction::Perk);
            }
            return true;
        }
        if self.pending_sulk {
            if self.action == PetAction::Sleep {
                self.quiet = 0.0;
                self.set_action(PetAction::Waking);
                self.spawn_wake_pop(width);
                return true;
            }
            self.pending_sulk = false;
            self.quiet = 0.0;
            self.droop_seated = matches!(
                self.action,
                PetAction::Sit | PetAction::Purr | PetAction::Groom
            );
            self.droop_brief = false;
            self.set_action(PetAction::Droop);
            return true;
        }
        if let Some(big) = self.pending_cheer {
            if self.action == PetAction::Sleep {
                self.quiet = 0.0;
                self.set_action(PetAction::Waking);
                self.spawn_wake_pop(width);
                return true;
            }
            if self.action != PetAction::Perk {
                self.set_action(PetAction::Perk);
                return true;
            }
            self.pending_cheer = None;
            self.cheer_frolic(big, width, false);
            return true;
        }
        if self.pending_pet > 0 {
            if self.action == PetAction::Sleep {
                self.quiet = 0.0;
                self.set_action(PetAction::Waking);
                self.spawn_wake_pop(width);
                return true;
            }
            let n = self.pending_pet;
            self.pending_pet = 0;
            self.pet_at = None;
            self.content = (self.content + f32::from(n) * PET_CONTENT).min(1.0);
            self.pet_hold_t = PET_HOLD;
            self.set_action(PetAction::Purr);
            self.spawn_pet_hearts(n, width);
            return true;
        }
        if self.consume_trick(sense, width) {
            return true;
        }
        if self.pending_flow.take().is_some() {
            if self.action == PetAction::Sleep {
                return false;
            }
            self.set_action(PetAction::Frolic);
            return true;
        }
        false
    }

    fn cheer_frolic(&mut self, big: bool, width: f32, compact: bool) {
        self.quiet = 0.0;
        self.set_action(PetAction::Frolic);
        self.spawn_cheer_motes(big, width, compact);
    }

    #[inline]
    const fn trick_settle_after(confirmed: bool) -> f32 {
        if confirmed {
            TRICK_SETTLE
        } else {
            TRICK_SETTLE_TENTATIVE
        }
    }

    fn sweep_trick_latch(&mut self, now: Instant) {
        if self.room_hold {
            self.pending_trick = None;
            self.drop_trick_go();
            self.song_left = 0;
            return;
        }
        let Some(p) = self.pending_trick else {
            return;
        };
        let since = |at: Instant| now.saturating_duration_since(at).as_secs_f32();
        if since(p.stamped) > TRICK_TTL || since(p.noted) > TRICK_CAP {
            self.pending_trick = None;
        }
    }

    #[inline]
    const fn clear_trick_settle(&mut self) {
        self.trick_settle = None;
        self.hold_end_q = 0.0;
    }

    #[inline]
    const fn drop_trick_go(&mut self) {
        if self.trick_go.take().is_some() {
            self.trick_heard = None;
        }
    }

    #[inline]
    const fn drop_tricks(&mut self) {
        self.pending_trick = None;
        self.trick_go = None;
        self.trick_heard = None;
        self.trick_act = false;
        self.song_left = 0;
        self.clear_trick_settle();
    }

    /// A kitty command has the cat's attention or body.
    #[inline]
    #[must_use]
    pub const fn trick_engaged(&self) -> bool {
        self.pending_trick.is_some()
            || self.trick_go.is_some()
            || self.trick_act
            || self.trick_settle.is_some()
    }

    #[inline]
    const fn trick_settle_posed(&self) -> bool {
        let Some(trick) = self.trick_settle else {
            return true;
        };
        let pose = match trick {
            Trick::Sleep => PetAction::Sleep,
            Trick::Down => PetAction::Loaf,
            _ => PetAction::Sit,
        };
        self.action as u8 == pose as u8
            || matches!(self.action, PetAction::Walk)
            || matches!(self.resume, Some((held, _)) if held as u8 == pose as u8)
    }

    #[inline]
    const fn trick_act_over(&self) -> bool {
        self.action.settled()
            && !matches!(self.action, PetAction::Perk)
            && self.wriggle_t <= 0.0
            && self.pet_hold_t <= 0.0
            && self.song_left == 0
            && !self.groom_owed
            && self.hide_to.is_none()
            && self.resume.is_none()
    }

    const fn praise(&mut self, trick: Trick) {
        let praised = match trick {
            Trick::Purr | Trick::Treat => true,
            Trick::Sit
            | Trick::Down
            | Trick::Sleep
            | Trick::Stretch
            | Trick::Jump
            | Trick::Play
            | Trick::Roll
            | Trick::Groom
            | Trick::Speak
            | Trick::Sing
            | Trick::Look
            | Trick::Paw
            | Trick::Hide
            | Trick::Boo
            | Trick::Scold => false,
        };
        if praised && (self.clock - self.trick_content_at) as f32 >= TRICK_CONTENT_COOL {
            self.trick_content_at = self.clock;
            self.content = (self.content + TRICK_CONTENT).min(1.0);
        }
    }

    fn consume_trick(&mut self, sense: &PetSense, width: f32) -> bool {
        let Some(p) = self.pending_trick else {
            return false;
        };
        if self.quiet < Self::trick_settle_after(p.confirmed) {
            return false;
        }
        if self.action == PetAction::Sleep {
            if p.trick == Trick::Sleep {
                self.pending_trick = None;
                return false;
            }
            if p.trick == Trick::Stretch {
                self.pending_trick = None;
            }
            self.quiet = 0.0;
            self.set_action(PetAction::Waking);
            self.spawn_wake_pop(width);
            return true;
        }
        self.pending_trick = None;
        self.trick_go = Some(p.trick);
        self.trick_heard = (!p.confirmed).then_some((p.trick, sense.now));
        self.perk_in_place();
        true
    }

    fn perform_trick(&mut self, trick: Trick, caret_col: u16, width: f32, dt: f32) {
        let caret = f32::from(caret_col);
        let toward_caret = caret < self.col + width * 0.5;
        self.trick_act = true;
        self.praise(trick);
        let away = !toward_caret;
        match trick {
            Trick::Sit => {
                self.trick_act = false;
                self.quiet = self.quiet.max(SIT_AFTER);
                self.trick_settle = Some(Trick::Sit);
                self.hold_end_q = self.quiet + TRICK_HOLD;
                self.facing_left = toward_caret;
                self.set_action(PetAction::Sit);
            }
            Trick::Down => {
                self.trick_act = false;
                self.quiet = self.quiet.max(self.loaf_after());
                self.trick_settle = Some(Trick::Down);
                self.hold_end_q = self.quiet + TRICK_HOLD;
                self.set_action(PetAction::Loaf);
            }
            Trick::Sleep => {
                self.trick_act = false;
                self.quiet = self.quiet.max(SLEEP_AFTER);
                self.trick_settle = Some(Trick::Sleep);
                self.enter_settled(dt);
            }
            Trick::Stretch => self.set_action(PetAction::Waking),
            Trick::Jump => {
                self.play_to = Some((self.col, self.row));
                self.play_land = Some(PlayLand::PointerFrolic);
                self.hop_crouch = false;
                self.big_gather = false;
                self.set_action(PetAction::Crouch);
            }
            Trick::Play => {
                self.play_frolic = true;
                self.swipe = false;
                self.play_hold = TRICK_PLAY_HOLD;
                self.set_action(PetAction::Frolic);
            }
            Trick::Roll => {
                self.wriggle_t = WRIGGLE_DUR;
                self.set_action(PetAction::Loaf);
            }
            Trick::Purr => {
                self.facing_left = away;
                self.pet_hold_t = PET_HOLD;
                self.set_action(PetAction::Purr);
                self.spawn_pet_hearts(1, width);
            }
            Trick::Groom => {
                self.groom_owed = true;
                self.set_action(PetAction::Groom);
            }
            Trick::Speak => {
                self.facing_left = away;
                self.mail_beat(width);
            }
            Trick::Sing => {
                self.facing_left = away;
                self.mail_beat(width);
                self.song_left = SONG_NOTES - 1;
                self.song_next = SONG_BEAT;
            }
            Trick::Look => {
                self.facing_left = toward_caret;
                self.perk_in_place();
            }
            Trick::Paw => {
                self.facing_left = toward_caret;
                self.play_frolic = true;
                self.swipe = true;
                self.play_hold = BAT_HOLD;
                self.set_action(PetAction::Frolic);
                self.spawn_bat_dust(caret, self.row);
            }
            Trick::Hide => {
                let behind = self
                    .ink_span(self.row)
                    .filter(|(first, end)| end - first >= 2.0)
                    .map(|(first, end)| (end - width * 0.7).max(first - width * 0.3))
                    .filter(|dest| (dest - self.col).abs() <= HIDE_RANGE);
                if let Some(dest) = behind {
                    self.hide_to = Some(dest);
                } else {
                    self.hiding = true;
                    self.hide_t = TRICK_DUCK;
                    self.hop_crouch = false;
                    self.big_gather = false;
                    self.set_action(PetAction::Crouch);
                }
            }
            Trick::Boo => {
                self.facing_left = caret < self.col;
                self.set_action(PetAction::Startle);
            }
            Trick::Scold => {
                self.droop_seated = self.quiet >= SIT_AFTER;
                self.droop_brief = true;
                self.speed = 0.0;
                self.set_action(PetAction::Droop);
            }
            Trick::Treat => {
                self.facing_left = away;
                self.cheer_frolic(false, width, false);
            }
        }
    }

    const fn clear_play_intents(&mut self) {
        if matches!(self.action, PetAction::Crouch)
            && !self.pending_vigil_pounce
            && (self.vigil || self.stakeout || self.hiding)
        {
            self.set_action_keep(PetAction::Stand);
        }
        self.vigil = false;
        self.pending_catch = None;
        self.pending_room = None;
        self.room_at = None;
        self.room_cue_act = false;
        self.pending_pointer_pounce = None;
        self.play_to = None;
        self.play_land = None;
        if let Some(big) = self.vigil_cheer.take()
            && self.pending_cheer.is_none()
        {
            self.pending_cheer = Some(big);
        }
        self.pursuit_t = None;
        self.pursuit_cool = self.pursuit_cool.max(1.0);
        self.stakeout = false;
        self.pending_look = None;
        self.look_at = None;
        self.groom_owed = false;
        self.hide_to = None;
        self.hiding = false;
    }

    #[inline]
    const fn drop_verdict_latches(&mut self) {
        self.pending_vigil_pounce = false;
        self.vigil_cheer = None;
        self.vigil = false;
    }

    const fn stroke_point(&self, sense: &PetSense, width: f32) -> Option<(f32, f32)> {
        if !self.action.settled()
            || matches!(self.action, PetAction::Perk)
            || self.quiet < PURSUIT_QUIET
            || sense.output_burst
            || self.stream
            || self.watch_heat >= WATCH_GATE
            || self.flight.is_some()
            || self.retired_flight_lift.is_some()
            || self.land_t > 0.0
            || self.speed.abs() > 0.01
            || self.pending_pounce
            || self.pending_big_jump
            || self.pending_wall_transit
            || self.hiding
            || sense.cell_w == 0
            || sense.cell_h == 0
            || sense.rows == 0
            || sense.cols == 0
        {
            return None;
        }
        let Some((x, y)) = sense.pointer else {
            return None;
        };
        let top = (self.row + 1.0 - ART_ROWS).clamp(0.0, (sense.rows as f32 - ART_ROWS).max(0.0));
        if x.is_finite()
            && y.is_finite()
            && x >= self.col + width * 0.15
            && x < self.col + width * 0.85
            && y >= top + ART_ROWS * 0.20
            && y < top + ART_ROWS * 0.85
        {
            Some((x, y))
        } else {
            None
        }
    }

    const fn clear_play(&mut self) {
        if matches!(self.action, PetAction::Crouch)
            && (self.play_to.is_some() || self.stakeout || self.hiding)
        {
            self.set_action_keep(PetAction::Stand);
            self.wriggle_t = 0.0;
        }
        self.last_pointer = None;
        self.pointer_vx = 0.0;
        self.pointer_vy = 0.0;
        self.pointer_speed = 0.0;
        self.pointer_heat = 0.0;
        self.dash_t = 0.0;
        self.pointer_armed = true;
        self.pending_pointer_pounce = None;
        self.play_to = None;
        self.play_land = None;
        self.vigil_cheer = None;
        self.play_frolic = false;
        self.play_hold = 0.0;
        self.swipe = false;
        self.pending_catch = None;
        self.pending_bat = None;
        self.bat_at = None;
        self.pursuit_t = None;
        self.stakeout = false;
        self.pending_look = None;
        self.look_at = None;
        self.room_cue_act = false;
        self.groom_owed = false;
        self.hide_to = None;
        self.hiding = false;
    }

    #[inline]
    pub(super) const fn room_cue_in_play(&self) -> bool {
        if matches!(self.action, PetAction::Perk | PetAction::Frolic)
            || (matches!(self.action, PetAction::Droop) && self.droop_brief)
            || self.twitch_t > 0.0
            || self.play_to.is_some()
            || self.play_land.is_some()
        {
            return true;
        }
        let mut i = 0;
        while i < self.motes.len() {
            if matches!(self.motes[i], Some(m) if matches!(m.kind, PetMoteKind::Note)) {
                return true;
            }
            i += 1;
        }
        false
    }

    #[inline]
    pub(super) const fn drop_room_cue(&mut self) {
        self.room_cue_act = false;
        self.pending_room = None;
        self.room_at = None;
    }

    #[inline]
    fn age_gesture_windows(&mut self, secs: f32) {
        if secs <= 0.0 {
            return;
        }
        self.reversal_t += secs;
        if self.reversal_t > FROLIC_WINDOW {
            self.reversals = 0;
            self.reversal_t = 0.0;
        }
        self.retreat_gap = (self.retreat_gap + secs).min(60.0);
        let decay = if self.vhat != 0.0
            || self.vrun != 0.0
            || self.run_pace != 0.0
            || self.lead_room != 0.0
        {
            let d = (-secs / VEL_TAU).exp();
            self.vhat *= d;
            self.vrun *= d;
            self.run_pace *= d;
            d
        } else {
            1.0
        };
        let carrying = self.carrying_run();
        self.run_t = (self.run_t + secs).min(60.0);
        if self.run_t > RHYTHM_WINDOW {
            if carrying {
                self.held_lead = self.lead_at(RHYTHM_WINDOW);
            }
            self.run_count = 0;
            self.lead_room *= decay;
        }
    }

    const fn drop_theater(&mut self) {
        self.speed = 0.0;
        self.deferred_arc = None;
        self.pending_pounce = false;
        self.pending_big_jump = false;
        self.big_gather = false;
        self.hop_crouch = false;
        self.flinch_t = 0.0;
        self.vhat = 0.0;
        self.pending_wall_transit = false;
        self.pending_bell = None;
        self.pending_cheer = None;
        self.drop_room_cheer();
        self.pending_sulk = false;
        self.pending_flow = None;
        self.drop_verdict_latches();
        self.pending_perk = None;
        self.pending_catch = None;
        self.caught = None;
        self.catch_cool = 0.0;
        self.mote_rgb = None;
        self.pending_pet = 0;
        self.pet_at = None;
        self.pet_hold_t = 0.0;
        self.song_left = 0;
        self.wriggle_t = 0.0;
        self.hide_to = None;
        self.hiding = false;
        self.watch_heat = 0.0;
        self.watch_t = 0.0;
        self.watch_spent = false;
        self.clear_play();
        self.bound2 = None;
        self.land_t = 0.0;
        self.land_span = 0.0;
        self.skid_dir = 0.0;
        self.resume = None;
        self.handoff_parked_clock = None;
        self.twitch_t = 0.0;
        self.last_burst = false;
        self.droop_seated = false;
        self.droop_brief = false;
        self.sit_front = false;
        self.peek = false;
        self.gaze_dwell = 0.0;
        self.look_row = 0;
        self.tennis = false;
        self.stumble_t = 0.0;
        self.leg_dist = 0.0;
        self.braking = false;
        self.brake_over = None;
        self.skid_puff = false;
        self.lateral = false;
        self.gait_pace = 0.0;
        self.gait_dir = 0.0;
        self.gait_t = 0.0;
        self.gait_v = 0.0;
        self.gait_rest = GAIT_DWELL;
        self.planted = false;
        self.departing = false;
        self.stand_from = None;
        self.stand_ends_leg = false;
        self.drew_leg = false;
        self.gallop_since = None;
        self.running = false;
        self.pending_drop = false;
        self.drop_sit_t = 0.0;
        self.drop_stare = false;
        self.commit_t = 0.0;
        self.scrabble = false;
    }

    fn consume_v2_offer(&mut self, sense: &PetSense, width: f32) -> bool {
        let asleep = matches!(self.action, PetAction::Sleep | PetAction::Waking);
        if let Some(at) = self.pending_perk
            && sense.now >= at
        {
            self.pending_perk = None;
            self.perk_spent = Some(at);
            if !asleep && (self.action.settled() || self.arriving()) {
                self.set_action(PetAction::Perk);
                return true;
            }
        }
        let Some(star) = self.pending_catch else {
            return false;
        };
        if asleep || !(self.action.settled() || self.arriving()) {
            // Kept, not dropped: the TTL above retires it.
            return false;
        }
        self.pending_catch = None;
        self.catch_cool = V2_CATCH_COOL;
        self.caught = Some(star);
        self.facing_left = star.col as f32 <= self.col + width * 0.5;
        self.play_frolic = true;
        self.swipe = true;
        self.play_hold = BAT_HOLD;
        self.set_action(PetAction::Frolic);
        true
    }

    fn consume_room(&mut self, caret_col: u16, width: f32) -> bool {
        let Some(cue) = self.pending_room else {
            return false;
        };
        if matches!(self.action, PetAction::Sleep | PetAction::Waking) {
            self.pending_room = None;
            self.room_at = None;
            return false;
        }
        if !self.action.settled() {
            return false;
        }
        self.pending_room = None;
        self.room_at = None;
        let mid = self.col + width * 0.5;
        match cue {
            RoomCue::Look { col } => {
                self.facing_left = col < mid;
                self.perk_in_place();
            }
            RoomCue::Nod => {
                self.facing_left = f32::from(caret_col) < mid;
                self.perk_in_place();
            }
            RoomCue::Mail => {
                self.facing_left = f32::from(caret_col) < mid;
                self.mail_beat(width);
            }
            RoomCue::EarFlat => {
                self.droop_seated = matches!(
                    self.action,
                    PetAction::Sit | PetAction::Purr | PetAction::Groom | PetAction::Loaf
                );
                self.droop_brief = true;
                self.speed = 0.0;
                self.set_action(PetAction::Droop);
            }
        }
        true
    }

    fn mail_beat(&mut self, width: f32) {
        self.perk_in_place();
        self.twitch_t = TWITCH_DUR;
        self.twitch_up = self.mote_serial.is_multiple_of(2);
        self.spawn_chest_mote(Some(PetMoteKind::Note), width);
    }

    #[inline]
    const fn perk_in_place(&mut self) {
        self.set_action(PetAction::Perk);
        self.speed = 0.0;
    }

    #[inline]
    const fn droop_hold(&self) -> f32 {
        if self.droop_brief {
            EAR_FLAT_HOLD
        } else {
            DROOP_HOLD
        }
    }

    #[inline]
    const fn watch_still(&self) -> bool {
        self.heat_frozen()
            && self.watch_heat >= WATCH_GATE
            && !self.watch_spent
            && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
    }

    fn consume_bat(&mut self, sense: &PetSense, width: f32) -> bool {
        let Some((bx, by)) = self.pending_bat else {
            return false;
        };
        if self.room_hold {
            self.pending_bat = None;
            self.bat_at = None;
            return false;
        }
        if matches!(self.action, PetAction::Sleep | PetAction::Waking) {
            // A sleeper ignores the peek — the TTL retires it.
            return false;
        }
        self.pending_bat = None;
        self.bat_at = None;
        let to_col = if self.col + width * 0.5 <= bx {
            bx - BAT_STANDOFF - width
        } else {
            bx + BAT_STANDOFF
        }
        .clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
        let to_row = by.clamp(0.0, f32::from(sense.rows.saturating_sub(1)));
        self.facing_left = if (to_col - self.col).abs() > 1e-4 {
            to_col < self.col
        } else {
            bx < self.col + width * 0.5
        };
        self.play_to = Some((to_col, to_row));
        self.play_land = Some(PlayLand::BatSwipe { col: bx, row: by });
        self.hop_crouch = false;
        self.big_gather = false;
        self.set_action(PetAction::Crouch);
        true
    }

    #[inline]
    const fn spawn_bat_dust(&mut self, col: f32, row: f32) {
        self.spawn_dealt(PetMoteKind::Dust, 0.5, (col + 0.23, row + 0.12));
    }

    fn consume_pointer_pounce(&mut self, sense: &PetSense, width: f32) -> bool {
        let Some((ac, ar)) = self.pending_pointer_pounce.take() else {
            return false;
        };
        if self.room_hold || matches!(self.action, PetAction::Sleep | PetAction::Waking) {
            return false;
        }
        let to_col = (ac - width * 0.5).clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
        let to_row = ar.clamp(0.0, f32::from(sense.rows.saturating_sub(1)));
        if (to_col - self.col).abs() > 1e-4 {
            self.facing_left = to_col < self.col;
        } else if (ac - (self.col + width * 0.5)).abs() > 1e-4 {
            self.facing_left = ac < self.col + width * 0.5;
        }
        self.play_to = Some((to_col, to_row));
        self.play_land = Some(PlayLand::PointerFrolic);
        self.hop_crouch = false;
        self.big_gather = false;
        self.set_action(PetAction::Crouch);
        true
    }

    fn consume_watch(&mut self, elapsed: f32, caret_col: u16) -> bool {
        if self.watch_spent || self.watch_heat < WATCH_GATE {
            return false;
        }
        if matches!(self.action, PetAction::Sleep | PetAction::Waking) {
            return false;
        }
        self.watch_t += elapsed;
        if self.watch_t >= WATCH_MAX {
            self.watch_spent = true;
            self.vigil_armed = self.exec_since.is_some();
            return false;
        }
        self.facing_left = f32::from(caret_col) < self.col;
        self.sit_front = false;
        self.peek = false;
        self.gaze_dwell = 0.0;
        if self.action != PetAction::Perk {
            self.set_action(PetAction::Perk);
        }
        self.speed = 0.0;
        true
    }

    fn spawn_cheer_motes(&mut self, big: bool, width: f32, compact: bool) {
        let n = if big { CHEER_LANES } else { CHEER_LANES - 1 };
        if compact {
            let (col, row) = self.chest_anchor(width);
            for k in 0..n {
                let k = k as f32;
                let kind = self.next_note_kind();
                self.spawn_dealt(
                    kind,
                    PURR_MOTE_LIFE,
                    (
                        col + CHEER_COMPACT_STEP.0 * k,
                        row - CHEER_COMPACT_STEP.1 * k,
                    ),
                );
            }
        } else {
            for _ in 0..n {
                self.spawn_chest_mote(None, width);
            }
        }
    }

    fn spawn_pet_hearts(&mut self, n: u8, width: f32) {
        for _ in 0..n {
            self.spawn_chest_mote(Some(PetMoteKind::Heart), width);
        }
    }

    #[inline]
    const fn chest_anchor(&self, width: f32) -> (f32, f32) {
        let chest_x = if self.facing_left { 0.38 } else { 0.62 };
        (self.col + width * chest_x + 0.23, self.row - 0.27)
    }

    #[inline]
    fn spawn_chest_mote(&mut self, kind: Option<PetMoteKind>, width: f32) {
        let kind = kind.unwrap_or_else(|| self.next_note_kind());
        let at = self.note_lane_birth(kind, self.chest_anchor(width));
        self.spawn_dealt(kind, PURR_MOTE_LIFE, at);
    }

    #[inline]
    const fn next_note_kind(&self) -> PetMoteKind {
        if self.mote_serial.is_multiple_of(2) {
            PetMoteKind::Note
        } else {
            PetMoteKind::Heart
        }
    }

    fn note_lane_birth(&self, kind: PetMoteKind, (col, row): (f32, f32)) -> (f32, f32) {
        let mut live = [Mote {
            kind,
            born: 0.0,
            life: 0.0,
            col: 0.0,
            row: 0.0,
            dir: 0.0,
            seed: 0,
            rgb: None,
        }; PET_MOTES_MAX];
        let mut live_len = 0usize;
        let mut max_live_end = f64::NEG_INFINITY;
        for m in self.motes.into_iter().flatten() {
            if matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart)
                && m.note_geom(self.clock).is_some()
            {
                max_live_end = max_live_end.max(m.born + f64::from(m.life.max(0.01)));
                live[live_len] = m;
                live_len += 1;
            }
        }
        let live = &live[..live_len];
        let cell = (self.last_cell_w.max(1), self.last_cell_h.max(1));
        let cell_w_f = f32::from(cell.0);
        let grid = self.last_grid;
        let grid_w = i32::from(grid.0).saturating_mul(i32::from(cell.0));
        let grid_h = i32::from(grid.1).saturating_mul(i32::from(cell.1));
        let max_dest = ((f32::from(mote_tile_side(cell.1)) * NOTE_SCALE_MAX).round() as i32)
            .clamp(1, i32::from(u16::MAX));
        let max_half = (max_dest + 1) / 2;
        let max_drift = NOTE_BASE + NOTE_RUN + NOTE_SWAY + 0.01;
        let safe_inside_at = |(x, _): (f32, f32)| {
            max_dest <= grid_w
                && max_dest <= grid_h
                && ((x - max_drift) * cell_w_f).floor() as i32 - max_half >= 0
                && ((x + max_drift) * cell_w_f).ceil() as i32 + max_half <= grid_w
        };
        if live.is_empty() && safe_inside_at((col, row)) {
            return (col, row);
        }
        let dir = if self.facing_left { -1.0 } else { 1.0 };
        let step = note_lane_step(cell.0, cell.1);
        const MAX_TICKS: usize = (PURR_MOTE_LIFE * 60.0).ceil() as usize;
        let ticks = MAX_TICKS as u32;
        let mut live_boxes = [([(0i32, 0i32, 0u16); PET_MOTES_MAX], 0usize); MAX_TICKS];
        let mut live_ticks_cached = 0usize;
        let mut clear_of_them = None;
        for &(along, up) in &NOTE_LANES {
            let at = (col + dir * along * step, row - up);
            let safe_inside = safe_inside_at(at);
            if live.is_empty() && safe_inside {
                return at;
            }
            let born = Mote {
                kind,
                born: self.clock,
                life: PURR_MOTE_LIFE,
                col: at.0,
                row: at.1,
                dir,
                seed: self.mote_serial,
                rgb: None,
            };
            let mut meets = false;
            let mut inside = true;
            for k in 0..ticks {
                let clock = self.clock + f64::from(k) / 60.0;
                if safe_inside && clock >= max_live_end {
                    break;
                }
                let Some((centre, scale)) = born.note_geom(clock) else {
                    break;
                };
                let Some(b) = mote_box_px(centre, scale, cell, grid) else {
                    continue;
                };
                if !safe_inside {
                    let half = (i32::from(b.2) + 1) / 2;
                    let cx = (centre.0 * cell_w_f).round() as i32;
                    inside &= cx - half >= 0 && cx + half <= grid_w;
                }
                if clock < max_live_end {
                    let k_idx = k as usize;
                    if k_idx >= live_ticks_cached {
                        let mut boxes = [(0i32, 0i32, 0u16); PET_MOTES_MAX];
                        let mut n = 0usize;
                        for m in live {
                            if let Some((c, s)) = m.note_geom(clock)
                                && let Some(o) = mote_box_px(c, s, cell, grid)
                            {
                                boxes[n] = o;
                                n += 1;
                            }
                        }
                        live_boxes[k_idx] = (boxes, n);
                        live_ticks_cached = k_idx + 1;
                    }
                    let (ref boxes, n) = live_boxes[k_idx];
                    let (bx1, by1) = (b.0 + i32::from(b.2), b.1 + i32::from(b.2));
                    meets |= boxes[..n].iter().any(|o| {
                        b.0 < o.0 + i32::from(o.2)
                            && o.0 < bx1
                            && b.1 < o.1 + i32::from(o.2)
                            && o.1 < by1
                    });
                }
                if meets || (!inside && (live.is_empty() || clear_of_them.is_some())) {
                    break;
                }
            }
            if meets {
                continue;
            }
            if inside {
                return at;
            }
            clear_of_them.get_or_insert(at);
        }
        clear_of_them.unwrap_or((col, row))
    }

    const fn cheer_air(
        &self,
        (col, row): (f32, f32),
        cell: (u16, u16),
        width: f32,
        air: CheerAir,
    ) -> (f32, f32, f32, f32) {
        let ch6 = if cell.1 > 6 { cell.1 } else { 6 };
        let ch1 = if cell.1 > 1 { cell.1 } else { 1 };
        let cw1 = if cell.0 > 1 { cell.0 } else { 1 };
        let reach_px = (ch6 as f32) * NOTE_SCALE_MAX + 1.0;
        let rh = reach_px / (ch1 as f32);
        let rw = reach_px / (cw1 as f32);
        let drift = NOTE_BASE + NOTE_RUN + NOTE_SWAY;
        let bottom = row - 0.27 - 0.3 * NOTE_CLIMB + rh;
        match air {
            CheerAir::Lanes(facing_left) => {
                let step = note_lane_step(cell.0, cell.1);
                let (ahead, up) = CHEER_LANES_REACH;
                let chest = col + width * if facing_left { 0.38 } else { 0.62 } + 0.23;
                let fore = ahead * step + drift + rw;
                let aft = (NOTE_SWAY - NOTE_BASE).max(0.0) + rw;
                let (left, right) = if facing_left {
                    (chest - fore, chest + aft)
                } else {
                    (chest - aft, chest + fore)
                };
                (left, row - 0.27 - up - NOTE_CLIMB - rh, right, bottom)
            }
            CheerAir::Compact => {
                let k = (CHEER_LANES - 1) as f32;
                let (along, up) = (CHEER_COMPACT_STEP.0 * k, CHEER_COMPACT_STEP.1 * k);
                let left = col + width * 0.38 + 0.23 - along - drift - rw;
                let right = col + width * 0.62 + 0.23 + along + drift + rw;
                (left, row - 0.27 - up - NOTE_CLIMB - rh, right, bottom)
            }
        }
    }

    #[inline]
    const fn mote_fade_env(u: f32) -> f32 {
        (u / 0.15).min(1.0) * ((1.0 - u) / 0.35).clamp(0.0, 1.0)
    }

    fn resolve_motes(&mut self) -> [Option<PetMoteSprite>; PET_MOTES_MAX] {
        let mut out = [None; PET_MOTES_MAX];
        let clock = self.clock;
        for (i, slot) in out.iter_mut().enumerate() {
            let Some(m) = self.motes[i] else { continue };
            let u = m.age(clock);
            if !(0.0..1.0).contains(&u) {
                self.motes[i] = None;
                continue;
            }
            let spin = if m.seed % 2 == 0 { 1.0 } else { -1.0 };
            *slot = Some(match m.kind {
                PetMoteKind::Dust => PetMoteSprite {
                    kind: m.kind,
                    col: m.col + m.dir * (0.25 + 0.85 * u) * (0.6 + 0.2 * f32::from(m.seed % 3)),
                    row: m.row - (0.08 + 0.22 * u),
                    scale: 0.5 + 1.3 * u,
                    rot: spin * (0.12 + 0.05 * f32::from(m.seed % 3) + 0.6 * u),
                    alpha: (Self::mote_fade_env(u) * 235.0) as u8,
                    rgb: m.rgb,
                },
                PetMoteKind::Zee | PetMoteKind::ZeePop => {
                    let (col, row, scale, rot, peak, env, ink) = if m.kind == PetMoteKind::Zee {
                        let (col, row) = m.drift(u);
                        let (px_col, px_row) = self.mote_px_cell(col, row);
                        let ink = self.zee_ink_fade(px_col, px_row);
                        if ink <= 0.0 {
                            self.motes[i] = None;
                            continue;
                        }
                        let su = look_slice_age(look_slice(u, ZEE_STEPS), ZEE_STEPS);
                        (
                            col,
                            row,
                            0.55 + 0.60 * su,
                            spin * (0.10 + 0.04 * f32::from(m.seed % 3) + 0.55 * su),
                            220.0,
                            Self::mote_fade_env(su),
                            ink,
                        )
                    } else {
                        let (col, row) = (m.col + m.dir * 0.25 * u, m.row - 0.55 * u);
                        let ink = self.zee_ink_fade(col, row);
                        if ink <= 0.0 {
                            self.motes[i] = None;
                            continue;
                        }
                        (
                            col,
                            row,
                            0.85 + 0.55 * u,
                            spin * (0.14 + 0.8 * u),
                            245.0,
                            Self::mote_fade_env(u),
                            ink,
                        )
                    };
                    PetMoteSprite {
                        kind: m.kind,
                        col,
                        row,
                        scale,
                        rot,
                        alpha: (env * ink * peak) as u8,
                        rgb: m.rgb,
                    }
                }
                PetMoteKind::Note | PetMoteKind::Heart => {
                    let su = look_slice_age(look_slice(u, PURR_MOTE_STEPS), PURR_MOTE_STEPS);
                    let (col, row) = m.drift(u);
                    let scale = note_scale(su);
                    PetMoteSprite {
                        kind: m.kind,
                        col,
                        row,
                        scale,
                        rot: spin * (0.11 + 0.05 * f32::from(m.seed % 3) + 0.4 * su),
                        alpha: (Self::mote_fade_env(su) * 240.0) as u8,
                        rgb: m.rgb,
                    }
                }
            });
        }
        out
    }

    #[inline]
    const fn next_departure_id(&mut self) -> u32 {
        self.departure_serial = self.departure_serial.wrapping_add(1);
        self.departure_serial
    }

    const fn spawn_departure(&mut self, d: Departure) {
        let mut slot = 0;
        let mut oldest_born = f64::INFINITY;
        let mut i = 0;
        while i < self.departures.len() {
            let Some(a) = self.departures[i] else {
                slot = i;
                break;
            };
            if a.born < oldest_born {
                oldest_born = a.born;
                slot = i;
            }
            i += 1;
        }
        self.departures[slot] = Some(d);
    }

    fn resolve_departures(&mut self) -> [Option<PetDepartureSprite>; PET_DEPARTURES_MAX] {
        let mut out = [None; PET_DEPARTURES_MAX];
        for (i, slot) in out.iter_mut().enumerate() {
            let Some(d) = self.departures[i] else {
                continue;
            };
            let t = (self.lane_clock - d.born) as f32;
            let life = d.life();
            if !(0.0..life).contains(&t) {
                self.departures[i] = None;
                continue;
            }
            let alpha = d.envelope(t);
            let (col, pose, facing_left, stance) = d.look_at(t);
            *slot = Some(PetDepartureSprite {
                pose: self.species.skin(pose),
                col,
                row: d.row,
                facing_left,
                scale_x: stance.scale_x,
                scale_y: stance.scale_y,
                lift: stance.lift,
                alpha: (alpha * 255.0) as u8,
                coat: d.coat,
                iris: d.iris,
                id: d.id,
                under_ink: d.own_glass.is_some(),
                own_glass: d.own_glass.is_some(),
                glass: d.own_glass.unwrap_or(0),
                colors: d.colors,
            });
        }
        out
    }

    #[inline]
    const fn set_action(&mut self, a: PetAction) {
        if a as u8 != self.action as u8 {
            self.action = a;
            self.mote_mark = 0;
        }
        self.action_t = 0.0;
    }

    #[inline]
    const fn set_action_keep(&mut self, a: PetAction) {
        if a as u8 != self.action as u8 {
            self.action = a;
            self.action_t = 0.0;
            self.mote_mark = 0;
        }
    }

    fn emit(&mut self, sense: PetSense, width: f32) -> PetFrame {
        let max_col = (f32::from(sense.cols) - width).max(0.0);
        let max_row = f32::from(sense.rows.saturating_sub(1));
        self.col = self.col.clamp(0.0, max_col);
        self.row = self.row.clamp(0.0, max_row);

        let mut scale_x = 1.0f32;
        let mut scale_y = 1.0f32;
        let mut lift = 0.0f32;
        let mut ear_frame = false;
        let tell = self.purring() && !sense.reduced_motion;
        let purr = if tell { self.content } else { 0.0 };
        if self.action != PetAction::Run || self.gallop_since.is_some() {
            self.departing = false;
        } else if !Self::is_leg_pose(self.last_pose)
            && !matches!(
                self.last_pose,
                PetGlyphId::PetStand | PetGlyphId::PetStandEar
            )
        {
            self.departing = true;
        }
        let run_planted = self.run_planted() && !self.departing;

        let pose = match self.action {
            PetAction::Sleep
                if sense.reduced_motion || self.quiet >= SLEEP_AFTER + BREATH_WINDOW =>
            {
                PetGlyphId::PetSleep0
            }
            PetAction::Sleep => {
                let s = Self::breath_sample(self.clock);
                scale_y += s * BREATH_DEPTH;
                scale_x -= s * BREATH_DEPTH * 0.5;
                if s >= 0.0 {
                    PetGlyphId::PetSleep1
                } else {
                    PetGlyphId::PetSleep0
                }
            }
            PetAction::Waking if !self.console_stretch_on_blank() => PetGlyphId::PetSit,
            PetAction::Waking => {
                // The stretch in two halves: the front reach draws out over
                // [`STRETCH_LEAD`] and eases back, then the hind leg reaches
                // back.
                if self.action_t < STRETCH_SPLIT * WAKE_DUR {
                    let k = (self.action_t / STRETCH_LEAD).min(1.0);
                    scale_x *= 1.0 + STRETCH_AMP * (core::f32::consts::PI * k).sin();
                    PetGlyphId::PetStretch
                } else {
                    PetGlyphId::PetStretchHind
                }
            }
            PetAction::Sit if self.trick_settle == Some(Trick::Sit) && !sense.reduced_motion => {
                (scale_x, scale_y) = Self::seat_scale(self.quiet, self.clock, false);
                PetGlyphId::PetSitLookup
            }
            PetAction::Stand | PetAction::Sit if self.room_lease => PetGlyphId::PetPerk,
            PetAction::Stand => {
                (scale_x, scale_y) = Self::stand_scale(self.quiet);
                if self.twitch_t > 0.0 {
                    ear_frame = true;
                    PetGlyphId::PetStandEar
                } else {
                    PetGlyphId::PetStand
                }
            }
            PetAction::Sit if sense.reduced_motion => PetGlyphId::PetSit,
            PetAction::Sit => {
                (scale_x, scale_y) = Self::seat_scale(self.quiet, self.clock, false);
                if self.sit_front && !self.sit_glance() {
                    // Face-on eye contact has its own blink, a one-field
                    // departure from the frame beneath it, so a stare can
                    // break without looking away.
                    if self.sit_blink_shows().is_some() {
                        PetGlyphId::PetSitFrontBlink
                    } else {
                        PetGlyphId::PetSitFront
                    }
                } else if self.peek {
                    PetGlyphId::PetPeekShoulder
                } else if let Some(pose) = self.sit_tail() {
                    pose
                } else if self.sit_yawn() {
                    PetGlyphId::PetYawn
                } else if self.twitch_t > 0.0
                    && self.side_on_holds(TWITCH_DUR - self.twitch_t, TWITCH_DUR)
                    && self.tail_room(TWITCH_DUR - self.twitch_t, TWITCH_DUR)
                    && self
                        .sit_blink_beat_at(self.quiet - SIT_AFTER + self.twitch_t)
                        .is_none()
                {
                    ear_frame = true;
                    if self.twitch_up {
                        PetGlyphId::PetSitEar
                    } else {
                        PetGlyphId::PetSitEarFar
                    }
                } else if self.look_row < 0 {
                    PetGlyphId::PetSitLookup
                } else if self.look_row > 0 {
                    PetGlyphId::PetSitLookdown
                } else if let Some((p, len)) = self.sit_blink_shows()
                    && self.side_on_holds(p, len)
                {
                    PetGlyphId::PetSitBlink
                } else {
                    PetGlyphId::PetSit
                }
            }
            PetAction::Loaf if self.wriggle_t > 0.0 => {
                let beat = ((WRIGGLE_DUR - self.wriggle_t) / WRIGGLE_BEAT) as u32;
                let u = (WRIGGLE_DUR - self.wriggle_t) * 9.0;
                scale_y *= 1.0 + 0.04 * u.sin();
                match beat % 4 {
                    0 => PetGlyphId::PetRoll0,
                    2 => PetGlyphId::PetRoll2,
                    _ => PetGlyphId::PetRoll1,
                }
            }
            PetAction::Loaf => {
                let ((sx, sy), beat, phase) =
                    self.loaf_scale_and_beat(self.quiet, self.clock, false);
                (scale_x, scale_y) = (sx, sy);
                if self.twitch_t > 0.0 {
                    ear_frame = true;
                    PetGlyphId::PetLoafEar
                } else if beat >= 1.0
                    && phase < LOAF_PEEK_DUR
                    && self.beat_deal(beat as i64, 3) == 1
                {
                    PetGlyphId::PetLoafOpen
                } else {
                    PetGlyphId::PetLoaf
                }
            }
            PetAction::Purr if self.pet_hold_t > 0.0 => {
                let u = (1.0 - self.pet_hold_t / PET_HOLD).clamp(0.0, 1.0);
                let warm = self.content >= PURR_GATE;
                let waves = if warm { 2.0 } else { 1.0 };
                let lean = (core::f32::consts::PI * waves * u).sin().powi(2);
                scale_y += 0.07 * lean;
                scale_x -= 0.04 * lean;
                if u < 0.20 {
                    PetGlyphId::PetSitLookup
                } else if warm && (0.35..0.55).contains(&u) {
                    PetGlyphId::PetSitFlick
                } else if !warm && (0.62..0.76).contains(&u) {
                    PetGlyphId::PetSitBlink
                } else {
                    PetGlyphId::PetPurr
                }
            }
            PetAction::Purr => {
                (scale_x, scale_y) = Self::seat_scale(self.quiet, self.clock, false);
                PetGlyphId::PetPurr
            }
            PetAction::Groom => {
                // Alternating washes: the wash's cycle index (as placed by
                // `enter_settled`) picks chest-then-flank on even washes and
                // flank-then-chest on odd. The owed post-chase groom is always
                // the paw lick.
                if self.groom_owed {
                    PetGlyphId::PetGroom
                } else {
                    let since_sit = (self.quiet - SIT_AFTER).max(0.0);
                    let groom_after = self.groom_after();
                    let cycle = groom_after + GROOM_DUR;
                    let into = (since_sit - groom_after).max(0.0);
                    let k = (into / cycle).floor();
                    let u = (into - k * cycle) / GROOM_DUR;
                    if (u < 0.6) != ((k as i64).rem_euclid(2) == 1) {
                        PetGlyphId::PetGroom
                    } else {
                        PetGlyphId::PetGroomFlank
                    }
                }
            }
            PetAction::Perk
                if self.pending_pounce
                    || self.pending_big_jump
                    || self.last_pose == PetGlyphId::PetPerkTurn =>
            {
                PetGlyphId::PetPerkTurn
            }
            PetAction::Perk => PetGlyphId::PetPerk,
            PetAction::Startle => PetGlyphId::PetStartle,
            PetAction::Frolic if self.swipe => {
                // Distinct branch.
                PetGlyphId::PetBat
            }
            PetAction::Frolic => {
                if ((self.action_t / (FROLIC_HOLD * 0.5)) as u32).is_multiple_of(2) {
                    PetGlyphId::PetPlaybow
                } else {
                    PetGlyphId::PetBat
                }
            }
            PetAction::Droop => {
                let u = (self.action_t / self.droop_hold()).clamp(0.0, 1.0);
                scale_y -= 0.06 * u;
                scale_x += 0.03 * u;
                if self.droop_seated {
                    PetGlyphId::PetDroopSit
                } else {
                    PetGlyphId::PetDroop
                }
            }
            PetAction::Crouch if self.big_gather => {
                let dur = if self.ink_overlaps(self.col, self.row, width) {
                    CROUCH_DUR
                } else {
                    BIG_CROUCH_DUR
                };
                let (cy, cx) = Self::coil_envelope(self.action_t, dur);
                scale_y *= cy;
                scale_x *= cx;
                if self.action_t >= dur - LAUNCH_LEAD {
                    PetGlyphId::PetLaunch
                } else if ((self.action_t * WIGGLE_HZ * 2.0) as u32).is_multiple_of(2) {
                    PetGlyphId::PetCrouch
                } else {
                    PetGlyphId::PetCrouchWiggle
                }
            }
            PetAction::Crouch if self.hop_crouch => {
                let (cy, cx) = Self::coil_envelope(self.action_t, CROUCH_DUR);
                scale_y *= cy;
                scale_x *= cx;
                if self.action_t >= self.coil_d - LAUNCH_LEAD {
                    PetGlyphId::PetLaunch
                } else {
                    PetGlyphId::PetCrouch
                }
            }
            PetAction::Crouch
                if !self.hiding && !self.stakeout && !self.vigil && self.play_to.is_none() =>
            {
                let (cy, cx) = Self::coil_envelope(self.action_t, self.coil_d);
                scale_y *= cy;
                scale_x *= cx;
                if self.action_t >= self.coil_d - LAUNCH_LEAD {
                    PetGlyphId::PetLaunch
                } else {
                    PetGlyphId::PetCrouch
                }
            }
            PetAction::Crouch => PetGlyphId::PetCrouch,
            PetAction::Leap => {
                let mut pose = PetGlyphId::PetLeap;
                if let Some(f) = self.flight {
                    let u = (f.t / f.dur).clamp(0.0, 1.0);
                    lift = Self::flight_lift(f.arc, u, f.big);
                    pose = Self::flight_pose(&f);
                    if f.drop
                        && pose == PetGlyphId::PetStretch
                        && !(self.console_stretch_on_blank_at((f.from_col, f.from_row))
                            && self.console_stretch_on_blank_at((f.to_col, f.to_row)))
                    {
                        pose = PetGlyphId::PetSleep1;
                    }
                    let launch = if f.hop { HOP_STRETCH } else { LAUNCH_STRETCH };
                    let stretch = if f.drop || pose == PetGlyphId::PetHop {
                        0.0
                    } else if u < 0.5 {
                        launch * (1.0 - 2.0 * u)
                    } else {
                        DESCEND_STRETCH.min(launch) * (2.0 * u - 1.0)
                    };
                    let dx = (f.to_col - f.from_col).abs() * f32::from(sense.cell_w);
                    let dy = (f.to_row - f.from_row).abs() * f32::from(sense.cell_h);
                    if dx > dy {
                        scale_x += stretch;
                        scale_y -= 0.6 * stretch;
                    } else {
                        scale_y += stretch;
                        scale_x -= 0.6 * stretch;
                    }
                }
                pose
            }
            PetAction::Land => {
                let (dur, q0, pose) = if self.bound2.is_some() {
                    (TOUCH_LAND_DUR, LAND_SQUASH * 0.5, PetGlyphId::PetCrouch)
                } else if self.land_running {
                    (TOUCH_LAND_DUR, LAND_SQUASH * 0.5, PetGlyphId::PetLand)
                } else if self.scrabble {
                    let elapsed = SCRABBLE_DUR - self.land_t;
                    if elapsed >= SCRABBLE_PLANT {
                        scale_x *= 1.0 + 0.03 * (TAU * 3.0 * elapsed).sin();
                    }
                    (SCRABBLE_DUR, LAND_SQUASH, PetGlyphId::PetLand)
                } else if self.land_span > 0.0 {
                    (
                        BIG_LAND_DUR,
                        LAND_SQUASH
                            * (BIG_SQUASH_NEAR
                                + (1.0 - BIG_SQUASH_NEAR)
                                    * (self.land_span / BIG_SQUASH_SPAN).min(1.0)),
                        PetGlyphId::PetLand,
                    )
                } else {
                    (LAND_DUR, LAND_SQUASH, PetGlyphId::PetLand)
                };
                let u = 1.0 - (self.land_t / dur).clamp(0.0, 1.0);
                let a = ((dur - self.land_t) / LAND_ATTACK).clamp(0.0, 1.0);
                let q = q0 * a * (-3.0 * u).exp() * (TAU * 1.35 * u).cos();
                scale_y *= 1.0 - q;
                scale_x *= 1.0 + q * 0.6;
                pose
            }
            PetAction::Walk | PetAction::Run
                if self
                    .brake_over
                    .is_some_and(|over| (over - self.col) * self.speed >= 0.0) =>
            {
                let over = self.brake_over.expect("checked by the guard");
                let w = ((over - self.col).abs() / BRAKE_OVERSHOOT).clamp(0.0, 1.0);
                scale_x *= 1.0 + SKID_LEAN * w;
                scale_y *= 1.0 - 0.4 * SKID_LEAN * w;
                PetGlyphId::PetSkid
            }
            PetAction::Run if run_planted => PetGlyphId::PetStand,
            PetAction::Walk if self.lateral => {
                Self::CYCLE_LWALK[self.gait_index(Self::CYCLE_LWALK.len())]
            }
            PetAction::Walk => Self::CYCLE_WALK[self.gait_index(Self::CYCLE_WALK.len())],
            PetAction::Run => Self::CYCLE_RUN[self.gait_index(Self::CYCLE_RUN.len())],
        };

        if tell {
            let s = Self::swell_sample(self.clock);
            scale_y += s * PURR_DEPTH;
            scale_x -= s * PURR_DEPTH * 0.6;
        }
        if self.flinch_t > 0.0 {
            let q = FLINCH_SQUASH * (self.flinch_t / FLINCH_DUR).clamp(0.0, 1.0);
            scale_y *= 1.0 - q;
            scale_x *= 1.0 + q * 0.6;
        }
        if self.stumble_t > 0.0 {
            let q = 0.18 * (self.stumble_t / STUMBLE_DUR).clamp(0.0, 1.0);
            scale_y *= 1.0 - q;
            scale_x *= 1.0 + q * 0.7;
        }
        if self.action == PetAction::Walk && self.content >= SKIP_CONTENT {
            let whole = if (0.0..4.0).contains(&self.stride) {
                self.stride
            } else {
                self.stride.rem_euclid(4.0)
            };
            if whole < 1.0 {
                lift += 0.07 * (whole * core::f32::consts::PI).sin().max(0.0);
            }
        }

        if self.twitch_t > 0.0 && !ear_frame {
            let q = TWITCH_BOB * (self.twitch_t / TWITCH_DUR).clamp(0.0, 1.0);
            scale_y *= if self.twitch_up { 1.0 + q } else { 1.0 - q };
        }

        let arrival = if self.arrive_t > 0.0 {
            let progress = 1.0 - (self.arrive_t / self.arrive_in).clamp(0.0, 1.0);
            let from = self.arrive_from.clamp(0.0, 1.0);
            (from + (1.0 - from) * progress).max(ARRIVE_FLOOR)
        } else {
            1.0
        };
        lift += self
            .retired_flight_lift
            .map_or(0.0, |(held_lift, _)| held_lift);
        let own = (pose, self.facing_left);
        let ramp = self.arrive_t > 0.0;
        let (col, row, action, arrive_t, arrive_in) = (
            self.col,
            self.row,
            self.action,
            self.arrive_t,
            self.arrive_in,
        );
        let (pose, facing_left) = match self.adopted_look.as_mut() {
            Some(look)
                if !sense.reduced_motion
                    && (col - look.seat.0).abs() <= ADOPT_HOLD_REACH
                    && (row - look.seat.1).abs() <= ADOPT_HOLD_REACH
                    && lift.abs() <= ADOPT_HOLD_REACH
                    && !matches!(
                        action,
                        PetAction::Crouch | PetAction::Leap | PetAction::Land
                    )
                    && (ramp
                        || (!Self::mid_stride(look.pose)
                            && look.beneath.is_none_or(|b| b == own))) =>
            {
                look.beneath = Some(own);
                let ramp_done = 1.0 - (arrive_t / arrive_in).clamp(0.0, 1.0);
                let own_stance = Stance {
                    scale_x,
                    scale_y,
                    lift,
                };
                look.eased = look.eased.max(ramp_done);
                let drawn = look.stance.toward(own_stance, look.eased);
                (scale_x, scale_y, lift) = (drawn.scale_x, drawn.scale_y, drawn.lift);
                (look.pose, look.facing_left)
            }
            _ => {
                self.adopted_look = None;
                own
            }
        };
        self.last_pose = pose;
        self.last_facing = facing_left;
        let alpha_clamped = self.alpha.clamp(0.0, 1.0);
        let mut frame = PetFrame {
            alpha: (alpha_clamped * arrival * 255.0) as u8,
            lane_alpha: (alpha_clamped * 255.0) as u8,
            action: self.action,
            pose: self.species.skin(pose),
            col: self.col,
            row: self.row,
            lift,
            facing_left,
            scale_x,
            scale_y,
            purr,
            contented: self.content >= PURR_GATE,
            under_ink: self.hiding,
            motes: self.resolve_motes(),
            departures: self.resolve_departures(),
        };
        self.finish_console_frame(&mut frame, sense);
        self.hold_still_pose(&mut frame);
        self.veil_over_letters(&mut frame, sense);
        self.last_presence = f32::from(frame.alpha) / 255.0;
        self.last_stance = Stance::of(&frame);
        let lane_alpha_norm = f32::from(frame.lane_alpha) / 255.0;
        self.last_drawn = core::array::from_fn(|i| {
            frame.departures[i].map_or(0.0, |d| f32::from(d.alpha) / 255.0 * lane_alpha_norm)
        });
        frame
    }

    fn hold_still_pose(&mut self, frame: &mut PetFrame) {
        if frame.alpha == 0 {
            self.still_since = None;
            return;
        }
        let mut stand = self.is_stand_pose(frame.pose);
        let travelling = !stand
            && matches!(
                frame.action,
                PetAction::Walk | PetAction::Run | PetAction::Leap | PetAction::Land
            );
        if let Some((_, pose)) = self.still_since
            && pose != frame.pose
            && self.still_hold_fresh()
            && (matches!(
                frame.action,
                PetAction::Crouch | PetAction::Perk | PetAction::Startle
            ) || (!travelling && self.still_held_kind()))
        {
            frame.pose = pose;
            frame.scale_x = 1.0;
            frame.scale_y = 1.0;
            stand = self.is_stand_pose(pose);
        } else if travelling || frame.action == PetAction::Crouch {
            self.still_since = None;
        } else if self.still_since.is_none_or(|(_, pose)| pose != frame.pose) {
            self.still_since = Some((self.clock, frame.pose));
            self.still_console = self.console_posed;
        }
        if stand {
            if self.stand_from.is_none() {
                self.stand_from = Some(self.clock);
                self.stand_ends_leg = self.drew_leg;
            }
        } else {
            self.stand_from = None;
            self.stand_ends_leg = false;
        }
        self.drew_leg = travelling && matches!(frame.action, PetAction::Walk | PetAction::Run);
    }

    /// The letter veil ([`LETTER_VEIL_DWELL`], [`PetFrame::under_ink`], [`LETTER_VEIL_FLOOR`],
    /// [`Self::letter_cover`], [`Self::clear_of_letters_above`], [`LETTER_VEIL_ON_S`],
    /// [`LETTER_VEIL_OFF_S`], [`Self::needs_frames`], [`Self::sense_ink`], [`Self::path_box`]).
    fn veil_over_letters(&mut self, frame: &mut PetFrame, sense: PetSense) {
        if sense.reduced_motion || frame.alpha == 0 {
            self.letter_veil = (1.0, None, 1.0);
            self.letter_cover_since = None;
            self.letter_rest_since = None;
            self.letter_rest_stepped = false;
            self.letter_gone_since = None;
            self.veil_gather = false;
            return;
        }
        let mut body_rect: Option<Option<(i32, i32, i32, i32)>> = None;
        let mut body_px = || {
            *body_rect.get_or_insert_with(|| {
                frame.body_px(sense.cell_w, sense.cell_h, sense.cols, sense.rows)
            })
        };
        let travelling = matches!(
            frame.action,
            PetAction::Walk | PetAction::Run | PetAction::Startle | PetAction::Frolic
        );
        let cover = if travelling
            && !self.ink_spans.is_empty()
            && let Some(rect) = body_px()
        {
            let path = Self::path_box(rect, sense.cell_h);
            self.letter_cover(path, sense.cell_w, sense.cell_h)
        } else {
            0.0
        };
        let cover = if cover > 0.0 {
            let since = *self.letter_cover_since.get_or_insert(sense.now);
            if sense.now.saturating_duration_since(since).as_secs_f32() >= LETTER_VEIL_DWELL {
                cover
            } else {
                0.0
            }
        } else {
            self.letter_cover_since = None;
            0.0
        };
        let k = (cover / LETTER_VEIL_FULL).clamp(0.0, 1.0);
        let walk_target = 1.0 - (1.0 - LETTER_VEIL_FLOOR) * k * k * (3.0 - 2.0 * k);
        let (level, at, _) = self.letter_veil;
        let veiled = level < 1.0;
        let flight = matches!(
            frame.action,
            PetAction::Crouch | PetAction::Leap | PetAction::Land
        );
        let in_place = matches!(frame.action, PetAction::Waking | PetAction::Droop);
        let cell = (sense.cell_w, sense.cell_h);
        let mut gather_on_letters = false;
        let mut keep = false;
        let on_letters = match sense.caret {
            _ if self.console_executing() => Some(false),
            Some((caret_row, _)) if frame.action.settled() => {
                body_px().map_or(Some(false), |rect| self.on_letters(rect, cell, caret_row))
            }
            Some((caret_row, _)) if travelling && level < 1.0 => {
                let here =
                    body_px().map_or(Some(false), |rect| self.on_letters(rect, cell, caret_row));
                if here == Some(false) {
                    here
                } else {
                    keep = true;
                    None
                }
            }
            Some((caret_row, _)) if in_place => {
                let here =
                    body_px().map_or(Some(false), |rect| self.on_letters(rect, cell, caret_row));
                if here == Some(true) {
                    here
                } else if veiled {
                    keep = true;
                    None
                } else {
                    here
                }
            }
            Some(caret) if flight => {
                gather_on_letters = frame.action == PetAction::Crouch
                    && !self.hand_writing()
                    && body_px().and_then(|rect| self.on_letters(rect, cell, caret.0))
                        == Some(true)
                    && self.landing_on_letters(sense, caret) == Some(true);
                if gather_on_letters {
                    Some(true)
                } else if veiled {
                    keep = true;
                    None
                } else {
                    Some(false)
                }
            }
            None if flight || in_place => {
                keep = veiled;
                None
            }
            None if frame.action.settled() => None,
            _ => Some(false),
        };
        // The letters an Enter prints under a body at rest are under its
        // fur in the frame that shows them: the veil steps on there (below),
        // once for that Enter, and holds while they come and go within
        // [`LETTER_VEIL_DWELL`] (output streaming in under the body). Any
        // other arrival, and letters back after the hold let the veil go,
        // yield by the dwell and the ease, never a cut: an Enter's step left
        // armed until the next key would drop the veil in one frame each time
        // streamed lines brought letters back under a resting cat after it
        // had eased up, 3.7 times a second.
        #[cfg(any(test, debug_assertions))]
        let dwells = self.letter_arrival_dwells;
        #[cfg(not(any(test, debug_assertions)))]
        let dwells = false;
        #[cfg(any(test, debug_assertions))]
        let rearms = self.letter_step_rearms;
        #[cfg(not(any(test, debug_assertions)))]
        let rearms = false;
        let enter = if !dwells && self.key_within(f32::INFINITY) == Some(PetInputKind::Submit) {
            self.last_submit_seq()
        } else {
            None
        };
        let holding = on_letters == Some(false)
            && !rearms
            && self.letter_rest_stepped
            && self.letter_rest_since.is_some()
            && {
                let gone = *self.letter_gone_since.get_or_insert(sense.now);
                sense.now.saturating_duration_since(gone).as_secs_f32() < LETTER_VEIL_DWELL
            };
        let rest_target = match on_letters {
            Some(true) => {
                self.letter_gone_since = None;
                let since = *self.letter_rest_since.get_or_insert(sense.now);
                if let Some(seq) = enter
                    && (seq != self.letter_step_seq || rearms)
                {
                    self.letter_step_seq = seq;
                    self.letter_rest_stepped = true;
                }
                if self.letter_rest_stepped
                    || sense.now.saturating_duration_since(since).as_secs_f32() >= LETTER_VEIL_DWELL
                {
                    LETTER_VEIL_FLOOR
                } else {
                    1.0
                }
            }
            Some(false) if holding => level,
            None if keep || self.letter_rest_since.is_some() => level,
            _ => {
                self.letter_rest_since = None;
                self.letter_rest_stepped = false;
                self.letter_gone_since = None;
                1.0
            }
        };
        let resting = rest_target < walk_target;
        let target = walk_target.min(rest_target);
        let dt = at.map_or(f32::INFINITY, |at| {
            sense.now.saturating_duration_since(at).as_secs_f32()
        });
        let level = if self.repainting && at.is_some() {
            level
        } else if dt > LETTER_VEIL_OFF_S && !resting {
            target
        } else if resting && self.letter_rest_stepped {
            // Letters an Enter printed under a body at rest are under its fur
            // now: the veil steps on in the frame that shows them, as the
            // screen itself jumps, one fall that never comes back while they
            // stay. Dwelt on and eased in, the letters an Enter at the bottom
            // row scrolls in under a resting cat would read under 4.5:1 for
            // 384 ms (352 on paper; the trail gallery's `enter-bottom`).
            target
        } else if target < level {
            let on = if resting {
                LETTER_VEIL_REST_ON_S
            } else {
                LETTER_VEIL_ON_S
            };
            (level - dt.min(LETTER_VEIL_REST_ON_S) / on).max(target)
        } else {
            (level + dt / LETTER_VEIL_OFF_S).min(target)
        };
        self.letter_veil = (level, Some(sense.now), target);
        self.veil_gather = gather_on_letters && level > LETTER_VEIL_FLOOR;
        if level >= 1.0 {
            return;
        }
        frame.under_ink = true;
        frame.alpha = (f32::from(frame.alpha) * level).round() as u8;
    }

    /// Whether the hand is writing ([`SIT_AFTER`], [`Self::veil_over_letters`]).
    fn hand_writing(&self) -> bool {
        match self.key_within(SIT_AFTER) {
            Some(kind) => matches!(kind, PetInputKind::Text | PetInputKind::Delete),
            None => self.last_dir != 0 && self.run_t < SIT_AFTER,
        }
    }

    /// Whether a gather of `coil` seconds still waits for its rest veil
    /// ([`Self::veil_over_letters`]).
    #[inline]
    const fn gather_owes_veil(&self, coil: f32) -> bool {
        self.veil_gather && self.action_t < coil + LETTER_VEIL_DWELL + LETTER_VEIL_REST_ON_S
    }

    /// Whether the stand a gather will come down on lays the standing body's head on letters
    /// ([`Self::on_letters`], [`Self::station_safe`]).
    fn landing_on_letters(&self, sense: PetSense, caret: (u16, u16)) -> Option<bool> {
        if sense.cell_w == 0 || sense.cell_h == 0 {
            return Some(false);
        }
        let at = self.flight.map_or_else(
            || {
                self.station_safe(
                    caret,
                    sense.cols,
                    sense.rows,
                    art_cols(sense.cell_w, sense.cell_h),
                )
            },
            |f| (f.to_col, f.to_row),
        );
        let rect = drawn_body_px(
            at,
            Stance::NEUTRAL,
            (sense.cell_w, sense.cell_h),
            (sense.cols, sense.rows),
        );
        self.on_letters(rect, (sense.cell_w, sense.cell_h), caret.0)
    }

    /// Whether a drawn body's box lays its head on letterforms the console observation
    /// certifies ([`INK_PAD`], [`Self::veil_over_letters`]).
    const fn on_letters(
        &self,
        (x0, x1, y0, y1): (i32, i32, i32, i32),
        (cell_w, cell_h): (u16, u16),
        caret_row: u16,
    ) -> Option<bool> {
        if cell_w == 0 || cell_h == 0 || y1 <= y0 || x1 <= x0 {
            return Some(false);
        }
        let (cw, ch) = (cell_w as f32, cell_h as i32);
        let (a, b) = (x0 as f32 / cw + INK_PAD, x1 as f32 / cw - INK_PAD);
        let y1_row = (y1 - 1).div_euclid(ch);
        let cr = caret_row as i32;
        let below = if y1_row < cr { y1_row } else { cr };
        let mut unknown = false;
        let y0_row = y0.div_euclid(ch);
        let mut row = if y0_row > 0 { y0_row } else { 0 };
        while row < below {
            let r_top = row * ch;
            let top = if r_top > y0 { r_top } else { y0 };
            let r_bot = (row + 1) * ch;
            let bottom = if r_bot < y1 { r_bot } else { y1 };
            if (bottom - top) * 4 >= ch {
                match self.observed_last_letter(row as f32, a, b) {
                    Some(Some(_)) => return Some(true),
                    Some(None) => {}
                    None => unknown = true,
                }
            }
            row += 1;
        }
        if !unknown { Some(false) } else { None }
    }

    /// The part of a drawn body's box that travels over letters: the row its feet are on
    /// ([`Self::veil_over_letters`]).
    #[inline]
    const fn path_box((x0, x1, y0, y1): (i32, i32, i32, i32), cell_h: u16) -> (i32, i32, i32, i32) {
        if cell_h == 0 || y1 <= y0 {
            return (x0, x1, y0, y1);
        }
        let ch = cell_h as i32;
        let feet_row_top = (y1 - 1).div_euclid(ch) * ch;
        let top = if y0 > feet_row_top { y0 } else { feet_row_top };
        (x0, x1, top, y1)
    }

    const fn letter_cover(
        &self,
        (x0, x1, y0, y1): (i32, i32, i32, i32),
        cell_w: u16,
        cell_h: u16,
    ) -> f32 {
        if cell_w == 0 || cell_h == 0 || y1 <= y0 || x1 <= x0 {
            return 0.0;
        }
        let (cw, ch) = (cell_w as i32, cell_h as i32);
        let col = x0 as f32 / cw as f32;
        let width = (x1 - x0) as f32 / cw as f32;
        let h = (y1 - y0) as f32;
        let mut cover = 0.0;
        let mut row = y0.div_euclid(ch);
        let last_row = (y1 - 1).div_euclid(ch);
        while row <= last_row {
            let r_top = row * ch;
            let top = if r_top > y0 { r_top } else { y0 };
            let r_bot = (row + 1) * ch;
            let bottom = if r_bot < y1 { r_bot } else { y1 };
            if (bottom - top) * 4 >= ch && self.ink_overlaps(col, row as f32, width) {
                cover += (bottom - top) as f32 / h;
            }
            row += 1;
        }
        cover
    }

    const CYCLE_WALK: [PetGlyphId; 4] = [
        PetGlyphId::PetWalk0,
        PetGlyphId::PetWalk1,
        PetGlyphId::PetWalk2,
        PetGlyphId::PetWalk3,
    ];
    const CYCLE_LWALK: [PetGlyphId; 4] = [
        PetGlyphId::PetLwalk0,
        PetGlyphId::PetLwalk1,
        PetGlyphId::PetLwalk2,
        PetGlyphId::PetLwalk3,
    ];
    const CYCLE_RUN: [PetGlyphId; 4] = [
        PetGlyphId::PetRun0,
        PetGlyphId::PetRun1,
        PetGlyphId::PetRun2,
        PetGlyphId::PetRun3,
    ];

    #[inline]
    const fn mid_stride(pose: PetGlyphId) -> bool {
        Self::is_leg_pose(pose)
            || matches!(
                pose,
                PetGlyphId::PetSkid
                    | PetGlyphId::PetCrouch
                    | PetGlyphId::PetCrouchWiggle
                    | PetGlyphId::PetLaunch
                    | PetGlyphId::PetLeapRise
                    | PetGlyphId::PetLeap
                    | PetGlyphId::PetHop
                    | PetGlyphId::PetApex
                    | PetGlyphId::PetLeapDescend
                    | PetGlyphId::PetLand
            )
    }

    #[inline]
    const fn gait_rank(running: bool, lateral: bool) -> u8 {
        if running {
            2
        } else if lateral {
            0
        } else {
            1
        }
    }

    #[inline]
    const fn leg_speed(&self) -> f32 {
        if self.tick_dt > 0.0 {
            (self.col - self.col_at_tick).abs() / self.tick_dt
        } else {
            0.0
        }
    }

    #[inline]
    const fn under_gallop(&mut self) -> f32 {
        if self.leg_speed() >= RUN_ART_MIN {
            self.gallop_at = self.clock;
            if self.gallop_since.is_none() {
                self.gallop_since = Some(self.clock);
            }
        } else {
            self.gallop_since = None;
        }
        (self.clock - self.gallop_at) as f32
    }

    #[inline]
    const fn run_planted(&mut self) -> bool {
        let under = self.under_gallop();
        if matches!(self.gallop_since, Some(since) if since < self.clock) {
            self.planted = false;
        } else if under > 0.0 && (self.action.settled() || under >= RUN_ART_GRACE) {
            self.planted = true;
        }
        self.planted
    }

    #[inline]
    const fn stand_held(&self) -> bool {
        matches!(self.stand_from, Some(from) if self.clock - from < STAND_MIN as f64)
            || self.still_fresh()
    }

    #[inline]
    const fn still_fresh(&self) -> bool {
        matches!(self.still_since, Some((at, _)) if ((self.clock - at) as f32) < STILL_SECS)
    }

    #[inline]
    const fn is_stand_pose(&self, pose: PetGlyphId) -> bool {
        let p = pose as u8;
        p == self.species.skin(PetGlyphId::PetStand) as u8
            || p == self.species.skin(PetGlyphId::PetStandEar) as u8
    }

    #[inline]
    const fn still_held_kind(&self) -> bool {
        match self.still_since {
            Some((_, p)) => self.still_console || self.is_stand_pose(p),
            None => false,
        }
    }

    #[inline]
    const fn still_hold_fresh(&self) -> bool {
        if self.still_fresh() {
            return true;
        }
        let flinch =
            self.console_posed && matches!(self.console_attention(), PetAttention::Contact);
        self.still_held_kind()
            && !self.still_console
            && !flinch
            && self.stand_ends_leg
            && self.stand_held()
    }

    #[inline]
    const fn arriving(&self) -> bool {
        matches!(
            self.action,
            PetAction::Walk | PetAction::Run | PetAction::Land
        ) && self.flight.is_none()
    }

    #[inline]
    const fn is_leg_pose(pose: PetGlyphId) -> bool {
        matches!(
            pose,
            PetGlyphId::PetWalk0
                | PetGlyphId::PetWalk1
                | PetGlyphId::PetWalk2
                | PetGlyphId::PetWalk3
                | PetGlyphId::PetLwalk0
                | PetGlyphId::PetLwalk1
                | PetGlyphId::PetLwalk2
                | PetGlyphId::PetLwalk3
                | PetGlyphId::PetRun0
                | PetGlyphId::PetRun1
                | PetGlyphId::PetRun2
                | PetGlyphId::PetRun3
        )
    }

    #[inline]
    fn gait_index(&self, len: usize) -> usize {
        let phase = if (0.0..1.0).contains(&self.stride) {
            self.stride
        } else {
            self.stride.rem_euclid(1.0)
        };
        ((phase * len as f32) as usize).min(len - 1)
    }

    /// The current action (for hosts that gate sound or other effects on it).
    #[inline]
    #[must_use]
    pub const fn action(&self) -> PetAction {
        self.action
    }

    /// True while the pet is visible at all (the host's "is there a cat" test).
    #[inline]
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.alpha > 0.0
    }

    /// Drop everything this brain owns because its owner went away.
    pub fn retire_unowned(&mut self) {
        if self.alpha == 0.0
            && self.last_caret.is_none()
            && self.motes.iter().all(Option::is_none)
            && self.departures.iter().all(Option::is_none)
            && !self.needs_frames()
        {
            return;
        }
        *self = self.reborn();
    }

    /// A fresh brain — [`Self::default`].
    #[inline]
    #[must_use]
    pub(crate) fn reborn(&mut self) -> Self {
        let mut ink_spans = std::mem::take(&mut self.ink_spans);
        ink_spans.clear();
        let console = self.console.retired();
        Self {
            console,
            ink_spans,
            departure_serial: self.departure_serial,
            ..Self::default()
        }
    }

    /// Host sync for the pet's identity.
    pub const fn sync_look(&mut self, pair: (u8, u8), arrival: PetArrival) -> SyncLookOutcome {
        match self.worn {
            Some(worn) if self.alpha > 0.0 => {
                let same_pair = pair.0 == worn.0 && pair.1 == worn.1;
                let park = if !same_pair { Some(pair) } else { None };
                let same_pending = match (park, self.pending_worn) {
                    (None, None) => true,
                    (Some(a), Some(b)) => a.0 == b.0 && a.1 == b.1,
                    _ => false,
                };
                let parked = park.is_some() && !same_pending;
                if !same_pending {
                    self.handoff_parked_clock = if park.is_some() {
                        Some(self.clock)
                    } else {
                        None
                    };
                    if park.is_some() {
                        self.pending_arrival = arrival;
                    }
                }
                self.pending_worn = park;
                SyncLookOutcome { worn, parked }
            }
            _ => {
                self.worn = Some(pair);
                self.pending_worn = None;
                self.handoff_parked_clock = None;
                SyncLookOutcome {
                    worn: pair,
                    parked: false,
                }
            }
        }
    }

    /// The `(coat, iris)` the pet is wearing now.
    #[inline]
    #[must_use]
    pub const fn worn_pair(&self) -> Option<(u8, u8)> {
        self.worn
    }

    /// Freeze the first footprint palette sample for this appearance.
    #[inline]
    pub const fn colors_for_appearance(&mut self, sampled: CatColorKey) -> CatColorKey {
        match self.colors {
            Some(colors) => colors,
            None => {
                self.colors = Some(sampled);
                sampled
            }
        }
    }

    /// Return the palette already frozen for this resident appearance.
    #[inline]
    #[must_use]
    pub const fn appearance_colors(&self) -> Option<CatColorKey> {
        self.colors
    }

    /// Retire only the contrast sample.
    pub const fn invalidate_colors(&mut self) {
        self.colors = None;
        let mut i = 0;
        while i < self.departures.len() {
            if let Some(d) = &mut self.departures[i] {
                d.colors = None;
            }
            i += 1;
        }
        let mut j = 0;
        while j < self.carried.len() {
            if let Some(c) = &mut self.carried[j] {
                c.ghost.colors = None;
            }
            j += 1;
        }
        if let Some(pending) = &mut self.resight {
            pending.ghost.colors = None;
        }
    }

    /// Whether the pet needs the host's 60 fps lane this frame.
    #[must_use]
    pub const fn needs_frames(&self) -> bool {
        // A veil at its level is not easing: a still crouch under a word (the
        // hide) holds its floor without the train.
        if self.letter_veil.0 != self.letter_veil.2 && self.alpha > 0.0 && !self.repainting {
            return true;
        }
        if (self.letter_gone_since.is_some()
            || (self.letter_rest_since.is_some() && self.letter_veil.0 > LETTER_VEIL_FLOOR))
            && self.alpha > 0.0
            && !self.repainting
        {
            return true;
        }
        if let Some(frames) = self.console_frames() {
            return frames;
        }
        if self.alpha < 1.0 {
            return self.last_caret.is_some() || self.alpha > 0.0;
        }
        if self.last_caret.is_none() {
            // Opaque with the caret gone: the fade-out's first tick had zero
            // `dt`; the next one is owed.
            return true;
        }
        if self.flight.is_some()
            || self.deferred_hidden_landing.is_some()
            || self.retired_flight_lift.is_some()
            || self.land_t > 0.0
            || self.speed.abs() > 0.01
            || (self.stand_held() && (self.home - self.col).abs() > ARRIVED)
            || self.deferred_arc.is_some()
        {
            return true;
        }
        if self.pet_hold_t > 0.0
            || self.pending_bell.is_some()
            || self.pending_cheer.is_some()
            || self.pending_sulk
            || self.pending_pet > 0
            || self.pending_vigil_pounce
            || self.vigil_cheer.is_some()
            || self.pending_trick.is_some()
            || self.trick_go.is_some()
        {
            return true;
        }
        if self.watch_heat > 0.0 && !self.heat_frozen() {
            return true;
        }
        if self.pointer_heat > 0.0
            || self.pending_pointer_pounce.is_some()
            || self.play_to.is_some()
            || self.play_land.is_some()
            || self.pending_bat.is_some()
            || self.pursuit_t.is_some()
            || self.stakeout
            || self.pending_look.is_some()
            || self.groom_owed
            || self.tennis
            || self.stumble_t > 0.0
            || self.braking
            || self.wriggle_t > 0.0
            || self.song_left > 0
            || self.hide_to.is_some()
            || self.stream
            || self.pending_drop
            || self.drop_sit_t > 0.0
            || self.commit_t > 0.0
            || self.scrabble
        {
            return true;
        }
        let mut i = 0;
        while i < self.departures.len() {
            if self.departures[i].is_some() {
                return true;
            }
            i += 1;
        }
        if self.pending_worn.is_some() && !matches!(self.action, PetAction::Sleep) {
            return true;
        }
        if self.arrive_t > 0.0 {
            // Distinct branch.
            return true;
        }
        if self.twitch_t > 0.0 {
            return true;
        }
        let mut j = 0;
        while j < self.motes.len() {
            if let Some(m) = self.motes[j]
                && !m.kind.rides_offer()
            {
                return true;
            }
            j += 1;
        }
        if self.vigil
            && self.vigil_armed
            && self.exec_since.is_some()
            && matches!(self.action, PetAction::Crouch)
        {
            return false;
        }
        if self.hiding && matches!(self.action, PetAction::Crouch) {
            return false;
        }
        if !self.action.settled() {
            // Distinct branch.
            return true;
        }
        if (matches!(self.action, PetAction::Perk) && !self.watch_still())
            || self.flinch_t > 0.0
            || self.evicting
        {
            return true;
        }
        if self.col != self.col_at_tick
            && (self.home - self.col).abs() * (self.last_cell_w as f32) >= 1.0
        {
            return true;
        }
        false
    }

    /// The coarse offer.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        let console = self.console_deadline(now);
        if self.console_frames().is_some() {
            return console;
        }
        let Some(secs) = self.next_change_secs() else {
            return console;
        };
        let base = self.last_now.unwrap_or(now);
        let at = base + Duration::from_secs_f32((secs + OFFER_PAST).clamp(0.0, 60.0));
        Some(console.map_or(at.max(now + ARM_MIN), |c| c.min(at.max(now + ARM_MIN))))
    }

    fn mote_edges(&self, soon: &mut Soonest) {
        let cw = f32::from(self.last_cell_w.max(1));
        let ch = f32::from(self.last_cell_h.max(1));
        for m in self.motes.iter().flatten().filter(|m| m.kind.rides_offer()) {
            let life = m.life.max(0.01);
            let u0 = m.age(self.clock);
            if !(0.0..1.0).contains(&u0) {
                continue;
            }
            let remain = (1.0 - u0) * life;
            soon.at(remain);
            let steps = m.look_steps();
            soon.at(((look_slice(u0, steps) + 1.0) / steps - u0) * life);
            let horizon = soon.0.map_or(remain, |s| s.min(remain));
            let (sx, sy) = m.drift_slope();
            let slope = (sx / life * cw, sy / life * ch);
            let step = first_px_step((cw, ch), horizon, slope, |t| m.drift(u0 + t / life));
            if let Some(t) = step {
                soon.at(t);
            }
        }
    }

    fn next_change_secs(&self) -> Option<f32> {
        if self.alpha < 1.0 || self.last_caret.is_none() {
            return None;
        }
        if self.needs_frames() {
            // Distinct branch.
            return Some(0.0);
        }
        let q = self.quiet;
        let mut soon = Soonest(None);
        self.mote_edges(&mut soon);
        if self.vigil
            && self.vigil_armed
            && self.exec_since.is_some()
            && self.action == PetAction::Crouch
        {
            return soon.0;
        }
        if self.hiding && self.action == PetAction::Crouch {
            soon.at(self.hide_t);
            return soon.0;
        }
        if self.watch_still() {
            soon.at(WATCH_MAX - self.watch_t);
        }
        if self.waiting_up() && !self.vigil {
            soon.at(VIGIL_AFTER - self.vigil_t);
        }
        if self.last_reduced {
            // Reduced motion draws one still per pose; the only edge left is
            // the ladder's sleep (the reduced arm of `tick` verdicts it).
            if self.action != PetAction::Sleep {
                soon.at(SLEEP_AFTER - q);
            }
            return soon.0;
        }
        if self.action == PetAction::Sleep && q >= SLEEP_AFTER + BREATH_WINDOW {
            // Deep sleep: byte-stable until an input, by law.
            return soon.0;
        }
        let h = (ART_ROWS * f32::from(self.last_cell_h.max(1))).round();
        let w = (h * ART_ASPECT).round();
        let c0 = self.clock;
        if q < SETTLE_TURN {
            soon.at(SETTLE_TURN - q);
        }
        if self.gaze_dwell > 0.0 {
            let dwell = if self.action == PetAction::Sleep {
                SLEEP_FLIP_DWELL
            } else {
                SETTLE_FLIP_DWELL
            };
            soon.at(dwell - self.gaze_dwell);
        }
        if self.action == PetAction::Sleep {
            soon.at(SLEEP_AFTER + BREATH_WINDOW - q);
            let half = f64::from(BREATH_PERIOD) / 2.0;
            soon.at((half - c0 % half) as f32);
            let zee = ((q - SLEEP_AFTER) / ZEE_EVERY).floor().max(0.0) + 1.0;
            let zee_q = SLEEP_AFTER + zee * ZEE_EVERY;
            if zee_q <= SLEEP_AFTER + BREATH_WINDOW - ZEE_LIFE {
                soon.at(zee_q - q);
            }
            let horizon = soon.0.unwrap_or(BREATH_WINDOW);
            let breath = TAU / BREATH_PERIOD * BREATH_DEPTH;
            let step = first_px_step((w, h), horizon, (0.5 * breath * w, breath * h), |t| {
                let s = Self::breath_sample(c0 + f64::from(t));
                (1.0 - s * BREATH_DEPTH * 0.5, 1.0 + s * BREATH_DEPTH)
            });
            if let Some(t) = step {
                soon.at(t);
            }
            return soon.0;
        }
        let seat_held = self.trick_settle.is_some() && self.trick_settle != Some(Trick::Sleep);
        if seat_held {
            soon.at(self.hold_end_q - q);
        }
        if !self.waiting_up() && !seat_held {
            soon.at(SLEEP_AFTER - q);
        }
        let tell = self.purring();
        if tell && self.action != PetAction::Purr {
            soon.at((self.content - PURR_GATE) / CONTENT_DECAY);
        }
        if tell {
            let beat = ((q - SIT_AFTER).max(0.0) / PURR_MOTE_EVERY).floor() + 1.0;
            soon.at(SIT_AFTER + beat * PURR_MOTE_EVERY - q);
        }
        if self.content >= PLAY_CONTENT && !self.room_lease && !self.room_hold {
            let t = self.bored_cool.max(BORED_AFTER - q);
            if q + t < BORED_UNTIL {
                soon.at(t);
            }
        }
        if matches!(
            self.action,
            PetAction::Sit | PetAction::Loaf | PetAction::Purr
        ) && !self.room_hold
            && !seat_held
        {
            let since = q - SIT_AFTER;
            let groom_after = self.groom_after();
            let cycle = groom_after + GROOM_DUR;
            let wash = if since >= groom_after {
                ((since - groom_after) / cycle).floor() + 1.0
            } else {
                0.0
            };
            if !(wash == 1.0 && self.groom_skip()) {
                soon.at(groom_after + wash * cycle - since);
            }
        }
        let swell = TAU * PURR_HZ * PURR_DEPTH;
        let swell_slope = if tell {
            (0.6 * swell * w, swell * h)
        } else {
            (0.0, 0.0)
        };
        match self.action {
            PetAction::Stand => {
                soon.at(SIT_AFTER - q);
                let gather_from = SIT_AFTER - SIT_FOLD;
                if q < gather_from {
                    soon.at(gather_from - q);
                } else {
                    let slope = (0.03 * w / SIT_FOLD, 0.05 * h / SIT_FOLD);
                    let step =
                        first_px_step((w, h), SIT_AFTER - q, slope, |t| Self::stand_scale(q + t));
                    if let Some(t) = step {
                        soon.at(t);
                    }
                }
            }
            PetAction::Sit => {
                let since = q - SIT_AFTER;
                if !self.room_hold && !seat_held {
                    let loaf_after = self.loaf_after();
                    soon.at(loaf_after - q);
                    if self.settle_seed.is_multiple_of(2) {
                        soon.at(loaf_after - YAWN_DUR - q);
                    }
                }
                if !self.room_lease && !seat_held {
                    self.sit_beat_edges(&mut soon);
                }
                let fold_live = since < SIT_FOLD;
                if fold_live || tell {
                    let (fx, fy) = if fold_live {
                        (0.03 * w / SIT_FOLD, 0.05 * h / SIT_FOLD)
                    } else {
                        (0.0, 0.0)
                    };
                    let horizon = if tell {
                        soon.0.unwrap_or(SLEEP_AFTER)
                    } else {
                        SIT_FOLD - since + PX_MARCH_MIN
                    };
                    let slope = (fx + swell_slope.0, fy + swell_slope.1);
                    let step = first_px_step((w, h), horizon, slope, |t| {
                        Self::seat_scale(q + t, c0 + f64::from(t), tell)
                    });
                    if let Some(t) = step {
                        soon.at(t);
                    }
                }
            }
            PetAction::Groom => {
                let since = (q - SIT_AFTER).max(0.0);
                let groom_after = self.groom_after();
                let cycle = groom_after + GROOM_DUR;
                let into = (since - groom_after).max(0.0);
                let start = groom_after + (into / cycle).floor() * cycle;
                soon.at(start + 0.6 * GROOM_DUR - since);
                soon.at(start + GROOM_DUR - since);
            }
            PetAction::Loaf => {
                let loaf_after = self.loaf_after();
                let since = (q - loaf_after).max(0.0);
                let beat = (since / FLICK_EVERY).floor();
                let phase = since - beat * FLICK_EVERY;
                soon.at(loaf_after + (beat + 1.0).max(1.0) * FLICK_EVERY - q);
                for k in [beat, beat + 1.0] {
                    if k >= 1.0 && self.beat_deal(k as i64, 3) == 1 {
                        let open = loaf_after + k * FLICK_EVERY;
                        soon.at(open - q);
                        soon.at(open + LOAF_PEEK_DUR - q);
                    }
                }
                let melt_live = since < SIT_FOLD;
                let thump_live = beat >= 1.0 && phase < FLICK_DUR;
                if melt_live || thump_live || tell {
                    let (mx, my) = if melt_live {
                        (0.02 * w / SIT_FOLD, 0.05 * h / SIT_FOLD)
                    } else {
                        (0.0, 0.0)
                    };
                    let ty = if thump_live {
                        TWITCH_BOB * h * core::f32::consts::PI / FLICK_DUR
                    } else {
                        0.0
                    };
                    let horizon = if tell {
                        soon.0.unwrap_or(SLEEP_AFTER)
                    } else {
                        let melt_end = if melt_live { SIT_FOLD - since } else { 0.0 };
                        let thump_end = if thump_live { FLICK_DUR - phase } else { 0.0 };
                        melt_end.max(thump_end) + PX_MARCH_MIN
                    };
                    let slope = (mx + swell_slope.0, my + ty + swell_slope.1);
                    let step = first_px_step((w, h), horizon, slope, |t| {
                        self.loaf_scale(q + t, c0 + f64::from(t), tell)
                    });
                    if let Some(t) = step {
                        soon.at(t);
                    }
                }
            }
            PetAction::Purr => {
                let owed =
                    CONTENT_DECAY * (q - SIT_AFTER).clamp(0.0, self.loaf_after() - SIT_AFTER);
                soon.at((self.content + owed - PURR_GATE) / CONTENT_DECAY);
                let horizon = soon.0.unwrap_or(SLEEP_AFTER);
                let step = first_px_step((w, h), horizon, swell_slope, |t| {
                    Self::seat_scale(q + t, c0 + f64::from(t), tell)
                });
                if let Some(t) = step {
                    soon.at(t);
                }
            }
            _ => {}
        }
        soon.0
    }

    #[inline]
    const fn sit_tail_edges(&self, beat: i64, swing_fits: bool) -> &'static [f32] {
        if self.sit_front && !self.sit_yawn() && self.beat_deal(beat, 3) == 0 {
            return &[];
        }
        match self.beat_deal(beat, 4) {
            0 => &TAIL_EDGES_FLICK,
            1 => &TAIL_EDGES_DOUBLE,
            2 if swing_fits => &TAIL_EDGES_SWING,
            _ => &[],
        }
    }

    fn sit_beat_edges(&self, soon: &mut Soonest) {
        let since = self.quiet - SIT_AFTER;
        let (period, at) = self.tail_clock();
        let swing_fits = !self.sit_front || self.look_dur() >= 7.0 * TAIL_SWING;
        Self::offer_beats(soon, since, period, at, |beat| {
            self.sit_tail_edges(beat, swing_fits)
        });
        let blink_period = BLINK_PERIOD + BLINK_PERIOD_STEP * f32::from((self.settle_seed / 2) % 3);
        Self::offer_beats(soon, since, blink_period, BLINK_AT, |beat| {
            if self.beat_deal(beat, 3) == 2 {
                &BLINK_EDGES_TWO
            } else {
                &BLINK_EDGES_ONE
            }
        });
        if self.sit_front {
            let look: &[f32] = if self.look_dur() >= LOOK_DUR_LONG {
                &GLANCE_EDGES_LONG
            } else {
                &GLANCE_EDGES_SHORT
            };
            Self::offer_beats(soon, since, self.glance_period(), GLANCE_AT, |beat| {
                if self.beat_deal(beat, 3) == 0 {
                    &[]
                } else {
                    look
                }
            });
        }
    }

    #[inline]
    fn offer_beats<'a>(
        soon: &mut Soonest,
        since: f32,
        period: f32,
        at: f32,
        edges: impl Fn(i64) -> &'a [f32],
    ) {
        let beat = if since < 0.0 {
            0
        } else {
            (since / period).floor() as i64
        };
        for b in [beat, beat + 1] {
            let open = b as f32 * period + at;
            for &edge in edges(b) {
                soon.at(open + edge - since);
            }
        }
    }

    #[inline]
    fn breath_sample(clock: f64) -> f32 {
        let t = (clock % f64::from(BREATH_PERIOD)) as f32;
        (TAU * t / BREATH_PERIOD).sin()
    }

    #[inline]
    fn swell_sample(clock: f64) -> f32 {
        let t = (clock % f64::from(1.0 / PURR_HZ)) as f32;
        (TAU * PURR_HZ * t).sin()
    }

    #[inline]
    const fn stand_scale(quiet: f32) -> (f32, f32) {
        let pre = ((quiet - (SIT_AFTER - SIT_FOLD)) / SIT_FOLD).clamp(0.0, 1.0);
        (1.0 + 0.03 * pre, 1.0 - 0.05 * pre)
    }

    #[inline]
    fn seat_scale(quiet: f32, clock: f64, tell: bool) -> (f32, f32) {
        let fold = (1.0 - (quiet - SIT_AFTER) / SIT_FOLD).clamp(0.0, 1.0);
        let mut scale_x = 1.0 + 0.03 * fold;
        let mut scale_y = 1.0 - 0.05 * fold;
        if tell {
            let s = Self::swell_sample(clock);
            scale_y += s * PURR_DEPTH;
            scale_x -= s * PURR_DEPTH * 0.6;
        }
        (scale_x, scale_y)
    }

    #[inline]
    fn loaf_scale(&self, quiet: f32, clock: f64, tell: bool) -> (f32, f32) {
        self.loaf_scale_and_beat(quiet, clock, tell).0
    }

    #[inline]
    fn loaf_scale_and_beat(&self, quiet: f32, clock: f64, tell: bool) -> ((f32, f32), f32, f32) {
        let loaf_after = self.loaf_after();
        let fold = (1.0 - (quiet - loaf_after) / SIT_FOLD).clamp(0.0, 1.0);
        let mut scale_y = 1.0 + 0.05 * fold;
        let mut scale_x = 1.0 - 0.02 * fold;
        let since = (quiet - loaf_after).max(0.0);
        let beat = (since / FLICK_EVERY).floor();
        let phase = since - beat * FLICK_EVERY;
        if beat >= 1.0 && phase < FLICK_DUR {
            let amp = TWITCH_BOB * (core::f32::consts::PI * phase / FLICK_DUR).sin();
            let dir = if (beat as i64) % 2 == 0 { 1.0 } else { -1.0 };
            scale_y += amp * dir;
        }
        if tell {
            let s = Self::swell_sample(clock);
            scale_y += s * PURR_DEPTH;
            scale_x -= s * PURR_DEPTH * 0.6;
        }
        ((scale_x, scale_y), beat, phase)
    }

    /// Earned contentment 0..=1.
    #[inline]
    #[must_use]
    pub const fn content(&self) -> f32 {
        self.content
    }
}

struct Soonest(Option<f32>);

impl Soonest {
    #[inline]
    const fn at(&mut self, t: f32) {
        if t > 0.0
            && match self.0 {
                None => true,
                Some(best) => t < best,
            }
        {
            self.0 = Some(t);
        }
    }
}

fn first_px_step(
    box_px: (f32, f32),
    horizon: f32,
    slope: (f32, f32),
    scale_at: impl Fn(f32) -> (f32, f32),
) -> Option<f32> {
    if horizon <= 0.0 || (slope.0 <= 0.0 && slope.1 <= 0.0) {
        return None;
    }
    let (w, h) = box_px;
    let key = |t: f32| {
        let (sx, sy) = scale_at(t);
        ((sx * w).round(), (sy * h).round())
    };
    let (sx0, sy0) = scale_at(0.0);
    let (mut vx, mut vy) = (sx0 * w, sy0 * h);
    let rest = (vx.round(), vy.round());
    let mut prev = 0.0f32;
    let mut t = 0.0f32;
    for i in 0..PX_MARCH_MAX {
        if i > 0 {
            let (sx, sy) = scale_at(t);
            (vx, vy) = (sx * w, sy * h);
            if (vx.round(), vy.round()) != rest {
                let (mut lo, mut hi) = (prev, t);
                for _ in 0..PX_BISECT {
                    let mid = 0.5 * (lo + hi);
                    if key(mid) == rest {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                return Some(hi);
            }
        }
        if t >= horizon {
            return None;
        }
        let stride = |v: f32, k: f32, slope: f32| {
            if slope > 0.0 {
                (0.5 - (v - k).abs()).max(0.0) / slope
            } else {
                f32::INFINITY
            }
        };
        let step = stride(vx, rest.0, slope.0).min(stride(vy, rest.1, slope.1));
        prev = t;
        t = (t + step.max(PX_MARCH_MIN)).min(horizon);
    }
    Some(t)
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn every_cat_pose_has_a_distinct_dog_counterpart() {
        let cats: Vec<PetGlyphId> = PET_GLYPH_IDS
            .iter()
            .copied()
            .filter(|id| !PET_GLYPHS[*id as usize].id.starts_with("pet_dog_"))
            .collect();
        assert!(!cats.is_empty(), "the cat roster must not be empty");
        let mut seen = std::collections::HashSet::new();
        for cat in cats {
            let dog = PetSpecies::Dog.skin(cat);
            let (cat_id, dog_id) = (PET_GLYPHS[cat as usize].id, PET_GLYPHS[dog as usize].id);
            assert_ne!(
                dog, cat,
                "{cat_id} fell back to the cat frame — no dog counterpart"
            );
            assert_eq!(
                dog_id,
                &format!("pet_dog_{}", cat_id.strip_prefix("pet_").unwrap()),
                "{cat_id} mapped to the wrong dog pose"
            );
            assert!(seen.insert(dog), "{dog_id} claimed by two cat poses");
        }
        for &id in PET_GLYPH_IDS {
            assert_eq!(PetSpecies::Cat.skin(id), id, "the cat skin is identity");
            let dog = PetSpecies::Dog.skin(id);
            assert_eq!(PetSpecies::Dog.skin(dog), dog, "re-skinning moved {id:?}");
        }
    }

    #[test]
    fn changing_species_preserves_the_pets_state() {
        let t = Instant::now();
        let run = |dog: bool| {
            let mut pet = PetBrain::default();
            for i in 0..40 {
                pet.tick(sense(
                    t + Duration::from_millis(i * 40),
                    Some((4, 10 + i as u16 % 7)),
                ));
            }
            let before = pet.tick(sense(t + Duration::from_millis(1600), Some((4, 20))));
            if dog {
                pet.set_species(PetSpecies::Dog);
            }
            let after = pet.tick(sense(t + Duration::from_millis(1640), Some((4, 20))));
            (pet, before, after)
        };
        let (_, _, cat) = run(false);
        let (pet, before, after) = run(true);
        assert_eq!(pet.species(), PetSpecies::Dog);
        assert_eq!(after.action, before.action, "the action must not reset");
        assert!(
            (after.col - cat.col).abs() < 1e-4 && (after.row - cat.row).abs() < 1e-4,
            "the pet must not move differently when its coat changes: {} against {}",
            after.col,
            cat.col
        );
        assert!(
            PET_GLYPHS[after.pose as usize].id.starts_with("pet_dog_"),
            "the very next frame must already be a dog"
        );
    }

    #[test]
    fn coordinate_retirement_is_dark_idempotent_and_preserves_identity() {
        let mut pet = PetBrain::default();
        let old_now = Instant::now();
        pet.species = PetSpecies::Dog;
        pet.content = 0.65;
        pet.quiet = 4.0;
        pet.clock = 19.0;
        pet.worn = Some((2, 3));
        pet.mote_serial = 7;
        pet.mote_mark = 11;
        pet.last_frolic = 13.0;
        pet.bored_cool = 2.0;

        pet.alpha = 1.0;
        pet.col = 72.0;
        pet.row = 18.0;
        pet.action = PetAction::Run;
        pet.last_caret = Some((18, 70));
        pet.last_now = Some(old_now);
        pet.ink_base = 10;
        pet.ink_spans.extend_from_slice(&[(2, 20), (0, 0)]);
        pet.ink_live = Some(10);
        pet.last_pointer = Some((71.0, 18.0));
        pet.pointer_heat = 1.0;
        pet.pending_pounce = true;
        pet.pending_cheer = Some(true);
        pet.pending_sulk = true;
        pet.deferred_hidden_landing = Some((74.0, 18.0));
        pet.retired_flight_lift = Some((0.75, 2.5));

        pet.retire_coordinate_space();
        assert_eq!(pet.species(), PetSpecies::Dog);
        assert_eq!(pet.content, 0.65);
        assert_eq!(pet.quiet, 4.0);
        assert_eq!(pet.clock, 19.0);
        assert_eq!(pet.worn, Some((2, 3)));
        assert_eq!(pet.mote_serial, 7);
        assert_eq!(pet.mote_mark, 11);
        assert_eq!(pet.last_frolic, 13.0);
        assert_eq!(pet.bored_cool, 2.0);
        assert_eq!(pet.action(), PetAction::Sit);
        assert!(!pet.is_active());
        assert!(!pet.needs_frames());
        assert!(pet.last_caret.is_none() && pet.last_now.is_none());
        assert!(pet.flight.is_none());
        assert!(pet.deferred_hidden_landing.is_none());
        assert!(pet.retired_flight_lift.is_none());
        assert!(pet.ink_spans.is_empty() && pet.ink_live.is_none());
        assert!(pet.last_pointer.is_none());
        assert_eq!(pet.pointer_heat, 0.0);
        assert!(!pet.pending_pounce);
        assert!(pet.pending_cheer.is_none() && !pet.pending_sulk);

        pet.retire_coordinate_space();
        assert_eq!(
            (pet.content, pet.worn, pet.species()),
            (0.65, Some((2, 3)), PetSpecies::Dog)
        );
        let new_now = old_now + Duration::from_secs(1);
        let expected_col = pet.station_now(12, 100, art_cols(10, 20));
        let frame = pet.tick_static_capture(sense(new_now, Some((3, 12))));
        assert_eq!(frame.alpha, 255);
        assert_eq!(frame.row, 3.0);
        assert!((frame.col - expected_col).abs() < f32::EPSILON);
        assert!(
            (frame.col - 72.0).abs() > 1.0,
            "old-pane position was retired"
        );
    }

    #[test]
    fn coordinate_retirement_lands_the_latest_parked_identity() {
        let mut pet = PetBrain::default();
        let now = Instant::now();
        assert_eq!(pet.sync_look((2, 3), PetArrival::Ceremony).worn, (2, 3));
        pet.alpha = 1.0;
        assert_eq!(
            pet.sync_look((7, 8), PetArrival::Ceremony).worn,
            (2, 3),
            "the visible outgoing appearance keeps its old look"
        );
        assert_eq!(pet.worn, Some((2, 3)));
        assert_eq!(pet.pending_worn, Some((7, 8)));

        pet.retire_coordinate_space();
        assert_eq!(pet.worn, Some((7, 8)));
        assert!(pet.pending_worn.is_none());
        assert!(!pet.is_active());

        let frame = pet.tick_static_capture(sense(now, Some((3, 12))));
        assert!(frame.alpha > 0);
        assert_eq!(
            pet.sync_look((7, 8), PetArrival::Ceremony).worn,
            (7, 8),
            "the next lawful sighting wears the latest parked identity"
        );
        assert!(pet.pending_worn.is_none());
    }

    fn at_the_door(t0: Instant) -> (PetBrain, Instant) {
        let mut pet = PetBrain::default();
        let (mut t, _) = type_run(&mut pet, t0, 10, 20, 6, 0.12);
        let end = t + Duration::from_secs_f32(VIGIL_AFTER + 1.0);
        while t < end {
            t += Duration::from_millis(50);
            pet.note_executing(t, true);
            let mut sn = sense(t, Some((10, 20)));
            sn.output_burst = true;
            let _ = pet.tick(sn);
        }
        for _ in 0..20 {
            t += Duration::from_millis(100);
            pet.note_executing(t, true);
            let _ = pet.tick(sense(t, Some((10, 20))));
        }
        assert!(
            pet.vigil && pet.action == PetAction::Crouch,
            "fixture: the cat is at the door ({:?})",
            pet.action
        );
        (pet, t)
    }

    #[test]
    fn a_shed_frame_after_a_verdict_never_strands_the_frame_lane() {
        fn release_frame(t0: Instant, shed_at: Option<usize>, hidden: bool) -> Option<usize> {
            let (mut pet, t) = at_the_door(t0);
            pet.note_command_done(t, false, Some(40_000));
            let mut t_g = t;
            for k in 0..2_000 {
                t_g += Duration::from_millis(16);
                pet.note_executing(t_g, false);
                let mut sn = sense(t_g, Some((10, 20)));
                if shed_at == Some(k) {
                    if hidden {
                        sn.caret = None;
                    } else {
                        sn.reduced_motion = true;
                    }
                }
                let _ = pet.tick(sn);
                if !pet.needs_frames() {
                    return Some(k);
                }
            }
            None
        }

        let t0 = Instant::now();
        assert!(
            release_frame(t0, None, false).is_some(),
            "fixture: the live path releases the lane"
        );
        for k in [0usize, 1, 5, 17, 33] {
            assert!(
                release_frame(t0, Some(k), false).is_some(),
                "one REDUCED frame at {k} pinned the frame lane for good"
            );
            assert!(
                release_frame(t0, Some(k), true).is_some(),
                "one HIDDEN frame at {k} pinned the frame lane for good"
            );
        }
    }

    #[test]
    fn a_red_verdict_on_the_vigil_droops() {
        let (mut pet, t) = at_the_door(Instant::now());
        pet.note_command_done(t, true, Some(40_000));
        let mut t_r = t;
        let mut drooped = false;
        for _ in 0..40 {
            t_r += Duration::from_millis(16);
            pet.note_executing(t_r, false);
            let f = pet.tick(sense(t_r, Some((10, 20))));
            assert!(!f.action.airborne(), "a failure never flies");
            if f.action == PetAction::Droop {
                drooped = true;
                break;
            }
        }
        assert!(drooped, "a non-zero exit droops");
    }

    #[test]
    fn a_success_soon_after_a_sulk_is_a_big_cheer() {
        let t = Instant::now();
        let mut pet = PetBrain::default();
        pet.note_command_done(t, true, Some(9_000));
        assert!(pet.pending_sulk, "the failure sulks");
        pet.pending_sulk = false;
        pet.note_command_done(t, false, Some(4_000));
        assert_eq!(
            pet.pending_cheer,
            Some(true),
            "the fix is the BIG cheer, at four seconds, because of what came \
             four seconds before it"
        );

        let mut cold = PetBrain::default();
        cold.note_command_done(t, false, Some(4_000));
        assert_eq!(
            cold.pending_cheer,
            Some(false),
            "with nothing to forgive, four seconds is an ordinary cheer"
        );
    }

    fn sense(now: Instant, caret: Option<(u16, u16)>) -> PetSense {
        PetSense {
            caret_drawn: true,
            now,
            caret,
            wrapped: false,
            rows: 30,
            cols: 100,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
        }
    }

    fn ptick(
        pet: &mut PetBrain,
        t: Instant,
        caret: (u16, u16),
        ptr: Option<(f32, f32)>,
    ) -> PetFrame {
        let mut s = sense(t, Some(caret));
        s.pointer = ptr;
        pet.tick(s)
    }

    fn stream(
        pet: &mut PetBrain,
        start: Instant,
        caret: (u16, u16),
        secs: f32,
    ) -> (Instant, PetFrame) {
        let mut t = start;
        let end = start + Duration::from_secs_f32(secs);
        let mut s = sense(t, Some(caret));
        s.output_burst = true;
        let mut frame = pet.tick(s);
        while t < end {
            t += Duration::from_millis(16);
            let mut s = sense(t, Some(caret));
            s.output_burst = true;
            frame = pet.tick(s);
        }
        (t, frame)
    }

    fn type_run(
        pet: &mut PetBrain,
        start: Instant,
        row: u16,
        col0: u16,
        keys: u16,
        gap: f32,
    ) -> (Instant, PetFrame) {
        let mut t = start;
        let mut frame = pet.tick(sense(t, Some((row, col0))));
        for k in 1..=keys {
            let until = t + Duration::from_secs_f32(gap);
            while t < until {
                t += Duration::from_millis(16);
                let _ = pet.tick(sense(t, Some((row, col0 + k - 1))));
            }
            t = until;
            frame = pet.tick(sense(t, Some((row, col0 + k))));
        }
        (t, frame)
    }

    fn awake(pet: &mut PetBrain, start: Instant, row: u16, col: u16) -> Instant {
        let (t, _) = type_run(pet, start, row, col, 2, 0.05);
        let (t, _) = idle(pet, t, (row, col + 2), SIT_AFTER + 0.2);
        t
    }

    fn idle(
        pet: &mut PetBrain,
        start: Instant,
        caret: (u16, u16),
        secs: f32,
    ) -> (Instant, PetFrame) {
        let mut t = start;
        let end = start + Duration::from_secs_f32(secs);
        let mut frame = pet.tick(sense(t, Some(caret)));
        while t < end {
            t += Duration::from_millis(16);
            frame = pet.tick(sense(t, Some(caret)));
        }
        (t, frame)
    }

    fn veiled_over(
        pet: &mut PetBrain,
        t: &mut Instant,
        action: PetAction,
        ink: u16,
        secs: f32,
    ) -> u8 {
        let mut spans = vec![(0u16, 0u16); 30];
        spans[29] = (0, ink);
        pet.sense_ink(0, &spans, Some(29));
        let mut faintest = 255u8;
        let end = *t + Duration::from_secs_f32(secs);
        while *t < end {
            *t += Duration::from_millis(16);
            let mut frame = PetFrame {
                alpha: 255,
                lane_alpha: 255,
                action,
                pose: PetGlyphId::PetWalk0,
                col: 20.0,
                row: 29.0,
                lift: 0.0,
                facing_left: false,
                scale_x: 1.0,
                scale_y: 1.0,
                purr: 0.0,
                contented: false,
                under_ink: false,
                motes: [None; PET_MOTES_MAX],
                departures: [None; PET_DEPARTURES_MAX],
            };
            pet.veil_over_letters(&mut frame, sense(*t, Some((29, 70))));
            faintest = faintest.min(frame.alpha);
        }
        faintest
    }

    #[test]
    fn the_pet_never_walks_on_a_line_of_letters() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        assert_eq!(
            veiled_over(&mut pet, &mut t, PetAction::Walk, 0, 0.6),
            255,
            "a walk along a blank line is solid"
        );
        assert_eq!(
            veiled_over(
                &mut pet,
                &mut t,
                PetAction::Walk,
                60,
                LETTER_VEIL_DWELL - 0.04
            ),
            255,
            "a step over letters shorter than the dwell is crossed full"
        );
        let _ = veiled_over(&mut pet, &mut t, PetAction::Walk, 0, 0.3);
        let walked = veiled_over(&mut pet, &mut t, PetAction::Run, 60, 0.6);
        assert!(
            u32::from(walked) * 2 < 255,
            "the cat running on letters is not a ghost: {walked}"
        );
        let _ = veiled_over(&mut pet, &mut t, PetAction::Sit, 60, 0.6);
        assert_eq!(
            veiled_over(&mut pet, &mut t, PetAction::Sit, 60, 0.1),
            255,
            "the settled cat stands full"
        );
        for action in [PetAction::Crouch, PetAction::Leap, PetAction::Land] {
            assert_eq!(
                veiled_over(&mut pet, &mut t, action, 60, 0.6),
                255,
                "a flight over letters keeps its body ({action:?})"
            );
        }
    }

    #[test]
    fn only_a_settled_or_travelling_body_off_the_letters_lifts_the_rest_veil() {
        use crate::pet_world::{PetPane, PetWorldFacts};
        use aterm_core::terminal::Terminal;

        let mut term = Terminal::new(30, 100);
        term.process(format!("\x1b[29;1H{}\x1b[30;3H", "x".repeat(100)).as_bytes());
        let mut pet = PetBrain::default();
        let input = term.cell_frame(30, 100);
        let facts = PetWorldFacts::read(&term, 7);
        pet.observe_console(&input, &facts, PetPane::full(&input));
        pet.set_console_presentable(true);
        let frame = |action: PetAction, lift: f32, scale_y: f32| PetFrame {
            alpha: 255,
            lane_alpha: 255,
            action,
            pose: PetGlyphId::PetStand,
            col: 20.0,
            row: 29.0,
            lift,
            facing_left: false,
            scale_x: 1.0,
            scale_y,
            purr: 0.0,
            contented: false,
            under_ink: false,
            motes: [None; PET_MOTES_MAX],
            departures: [None; PET_DEPARTURES_MAX],
        };
        let mut t = Instant::now();
        let mut run = |pet: &mut PetBrain, f: PetFrame, secs: f32| {
            let end = t + Duration::from_secs_f32(secs);
            let mut most = 0u8;
            let mut last = 0u8;
            while t < end {
                t += Duration::from_millis(16);
                let mut f = f;
                pet.veil_over_letters(&mut f, sense(t, Some((29, 2))));
                most = most.max(f.alpha);
                last = f.alpha;
            }
            (most, last)
        };
        let (_, rested) = run(&mut pet, frame(PetAction::Sit, 0.0, 1.0), 1.0);
        assert!(rested < 64, "fixture: the resting cat is veiled ({rested})");
        let low = 0.7;
        for (action, lift, scale_y) in [
            (PetAction::Waking, 0.0, low),
            (PetAction::Droop, 0.0, low),
            (PetAction::Crouch, 0.0, low),
            (PetAction::Leap, 3.0, 1.0),
            (PetAction::Land, 0.0, low),
        ] {
            let (most, _) = run(&mut pet, frame(action, lift, scale_y), 0.6);
            assert!(
                most <= rested,
                "{action:?} off the letters lifted the rest veil from {rested} to {most}"
            );
        }
        let (most, _) = run(&mut pet, frame(PetAction::Walk, 0.0, 1.0), 0.6);
        assert!(
            most <= rested,
            "a walk on the letters lifted the rest veil from {rested} to {most}"
        );
        let (_, lifted) = run(&mut pet, frame(PetAction::Walk, 0.0, low), 0.6);
        assert_eq!(lifted, 255, "a walk off the letters lifts it");
        let (_, rested) = run(&mut pet, frame(PetAction::Sit, 0.0, 1.0), 1.0);
        assert!(
            rested < 64,
            "back on the letters at rest it is veiled again"
        );
        let (_, lifted) = run(&mut pet, frame(PetAction::Sit, 0.0, low), 0.6);
        assert_eq!(lifted, 255, "a settled body off the letters lifts it");
    }

    #[test]
    fn a_cat_walking_under_a_line_of_ink_keeps_its_body() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut spans = vec![(0u16, 0u16); 30];
        spans[19] = (0, 100);
        pet.sense_ink(0, &spans, Some(19));
        let t = awake(&mut pet, start, 20, 10);
        let mut t = t;
        let mut walked = Vec::new();
        for k in 0..30u16 {
            let until = t + Duration::from_millis(140);
            while t < until {
                t += Duration::from_millis(16);
                pet.sense_ink(0, &spans, Some(19));
                let f = pet.tick(sense(t, Some((20, 12 + k))));
                if matches!(f.action, PetAction::Walk | PetAction::Run) {
                    walked.push(f.alpha);
                }
            }
        }
        assert!(
            walked.len() > 60,
            "fixture: the cat walks ({})",
            walked.len()
        );
        let faintest = walked.iter().copied().min().unwrap_or(0);
        assert!(
            faintest > 200,
            "the walking cat is veiled: alpha down to {faintest} over {} presents",
            walked.len()
        );
    }

    #[test]
    fn a_first_sighting_materialises_at_the_station_not_at_the_origin() {
        let mut pet = PetBrain::default();
        let f = pet.tick(sense(Instant::now(), Some((5, 40))));
        let w = art_cols(10, 20);
        assert!(
            (f.col - PetBrain::station(40, 100, w)).abs() < 0.01,
            "first frame must appear at its station, got col {}",
            f.col
        );
        assert!((f.row - 5.0).abs() < 0.01, "and on the caret's row");
    }

    #[test]
    fn steady_typing_walks_and_fast_typing_runs() {
        let start = Instant::now();
        let mut slow = PetBrain::default();
        let t = awake(&mut slow, start, 3, 5);
        let (_, slow_f) = type_run(&mut slow, t, 3, 7, 10, 0.30);
        let mut fast = PetBrain::default();
        let t = awake(&mut fast, start, 3, 5);
        let (_, fast_f) = type_run(&mut fast, t, 3, 7, 30, 0.045);
        assert_eq!(
            slow_f.action,
            PetAction::Walk,
            "an unhurried cadence walks the pet"
        );
        assert_eq!(
            fast_f.action,
            PetAction::Run,
            "a fast cadence outruns a walk"
        );
    }

    #[test]
    fn the_gait_cycle_is_driven_by_distance_not_by_the_clock() {
        let start = Instant::now();
        let mut near = PetBrain::default();
        let t = awake(&mut near, start, 3, 5);
        let _ = type_run(&mut near, t, 3, 7, 4, 0.05);
        let mut far = PetBrain::default();
        let t = awake(&mut far, start, 3, 5);
        let _ = type_run(&mut far, t, 3, 7, 30, 0.05);
        assert!(
            far.stride > near.stride * 2.0,
            "more travel = more stride ({} vs {})",
            far.stride,
            near.stride
        );
    }

    #[test]
    fn a_low_cadence_host_still_puts_the_pet_to_sleep_on_time() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, _) = type_run(&mut pet, start, 4, 10, 4, 0.05);
        let mut slept_at = None;
        for i in 0..40 {
            t += Duration::from_millis(1000);
            let f = pet.tick(sense(t, Some((4, 14))));
            if f.action == PetAction::Sleep && slept_at.is_none() {
                slept_at = Some(i);
            }
        }
        let i = slept_at.expect("a 1 Hz host must still reach sleep");
        assert!(
            (SLEEP_AFTER as i32 - 2..=SLEEP_AFTER as i32 + 2).contains(&i),
            "slept after {i} one-second frames, expected ~{SLEEP_AFTER}"
        );
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    struct MoteLook {
        kind: PetMoteKind,
        cx: i32,
        cy: i32,
        scale: i32,
        rot: i32,
        alpha: u8,
    }

    impl MoteLook {
        fn within(self, trio: [Option<MoteLook>; 3]) -> bool {
            macro_rules! bracketed {
                ($($field:ident),*) => {
                    $( trio.iter().flatten().any(|b| b.$field == self.$field) )&&*
                };
            }
            bracketed!(cx, cy, scale, rot, alpha)
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Look {
        action: PetAction,
        pose: PetGlyphId,
        box_h: i32,
        box_w: i32,
        row: i32,
        alpha: u8,
        facing_left: bool,
        motes: usize,
        keyed: [Option<MoteLook>; PET_MOTES_MAX],
    }

    impl Look {
        fn within(&self, trio: [&Look; 3]) -> bool {
            macro_rules! bracketed {
                ($($field:ident),*) => {
                    $( trio.iter().any(|b| b.$field == self.$field) )&&*
                };
            }
            bracketed!(action, pose, box_h, box_w, row, alpha, facing_left, motes)
                && self.keyed.iter().enumerate().all(|(i, z)| match z {
                    None => trio.iter().any(|b| b.keyed[i].is_none()),
                    Some(z) => z.within([trio[0].keyed[i], trio[1].keyed[i], trio[2].keyed[i]]),
                })
        }
    }

    fn note_births(f: &PetFrame, births: &mut [Option<f32>; PET_MOTES_MAX]) {
        for (birth, m) in births.iter_mut().zip(f.motes.iter()) {
            match m {
                Some(m) if m.kind.rides_offer() => {
                    if birth.is_none() {
                        *birth = Some(f.col);
                    }
                }
                _ => *birth = None,
            }
        }
    }

    fn zee_shifts(
        pet: &[Option<f32>; PET_MOTES_MAX],
        twin: &[Option<f32>; PET_MOTES_MAX],
    ) -> [f32; PET_MOTES_MAX] {
        let mut out = [0.0; PET_MOTES_MAX];
        for ((out, p), t) in out.iter_mut().zip(pet).zip(twin) {
            if let (Some(p), Some(t)) = (p, t) {
                *out = p - t;
            }
        }
        out
    }

    const NO_SHIFT: [f32; PET_MOTES_MAX] = [0.0; PET_MOTES_MAX];

    fn visible(f: &PetFrame, shift: [f32; PET_MOTES_MAX]) -> Look {
        let h = (ART_ROWS * 20.0).round();
        let w = (h * ART_ASPECT).round();
        let mut keyed = [None; PET_MOTES_MAX];
        for ((slot, m), shift) in keyed.iter_mut().zip(f.motes.iter()).zip(shift) {
            *slot = m
                .as_ref()
                .filter(|m| m.kind.rides_offer())
                .map(|m| MoteLook {
                    kind: m.kind,
                    cx: ((m.col + shift) * 10.0).round() as i32,
                    cy: (m.row * 20.0).round() as i32,
                    scale: (m.scale * 1024.0) as i32,
                    rot: (m.rot * 1024.0) as i32,
                    alpha: m.alpha,
                });
        }
        Look {
            action: f.action,
            pose: f.pose,
            box_h: (f.scale_y * h).round() as i32,
            box_w: (f.scale_x * w).round() as i32,
            row: (f.row * 64.0) as i32,
            alpha: f.alpha,
            facing_left: f.facing_left,
            motes: f.motes.iter().flatten().count(),
            keyed,
        }
    }

    fn same_feet(a: f32, b: f32) -> bool {
        (a - b).abs() * 10.0 < 1.0
    }

    #[derive(Debug, Default)]
    struct Walk {
        offers: u32,
        train: u32,
        idle_wakes: u32,
        slop: u32,
        zee_train: u32,
        zee_offers: u32,
        note_train: u32,
        note_offers: u32,
        note_edges: u32,
        note_births: u32,
        dust_train: u32,
        pop_train: u32,
    }

    fn walk_offers(
        pet: &mut PetBrain,
        twin: &mut PetBrain,
        start: Instant,
        caret: (u16, u16),
        secs: f32,
    ) -> (Walk, Instant) {
        let grid = Duration::from_millis(16);
        let hair = Duration::from_millis(1);
        let end = start + Duration::from_secs_f32(secs);
        let mut walk = Walk::default();
        let mut t = start;
        let mut tb = start;
        let mut pet_birth = [None; PET_MOTES_MAX];
        let mut twin_birth = [None; PET_MOTES_MAX];
        let first = pet.tick(sense(t, Some(caret)));
        note_births(&first, &mut pet_birth);
        let mut last_full = visible(&first, NO_SHIFT);
        let mut last = last_full;
        let mut last_col = first.col;
        let mut b_last = twin.tick(sense(t, Some(caret)));
        note_births(&b_last, &mut twin_birth);
        let mut was_idle = false;
        while t < end {
            let parked = !pet.needs_frames();
            let own = !parked && !needs_frames_without_motes(pet);
            let next = if parked {
                let Some(d) = pet.next_change_deadline(t) else {
                    break;
                };
                assert!(d > t, "a deadline at `now` is a busy re-arm");
                walk.offers += 1;
                d
            } else {
                walk.train += 1;
                t + grid
            };
            if was_idle {
                if next - t <= 2 * hair {
                    walk.slop += 1;
                } else {
                    walk.idle_wakes += 1;
                }
            }
            let before = |twin: &mut PetBrain,
                          twin_birth: &mut [Option<f32>; PET_MOTES_MAX],
                          at: Instant| {
                let f = twin.tick(sense(at, Some(caret)));
                note_births(&f, twin_birth);
                if parked {
                    assert_eq!(
                        visible(&f, zee_shifts(&pet_birth, twin_birth)),
                        last,
                        "the frame changed at +{:?}, before the offer at +{:?} — a late offer",
                        at - start,
                        next - start
                    );
                    assert!(
                        same_feet(f.col, last_col),
                        "the feet moved a pixel at +{:?}, before the offer at +{:?}",
                        at - start,
                        next - start
                    );
                }
                f
            };
            if parked {
                while tb + grid < next {
                    tb += grid;
                    b_last = before(twin, &mut twin_birth, tb);
                }
                if next - hair > tb {
                    tb = next - hair;
                    b_last = before(twin, &mut twin_birth, tb);
                }
            }
            t = next;
            let a = pet.tick(sense(t, Some(caret)));
            let drew = |kinds: &[PetMoteKind]| {
                a.motes
                    .iter()
                    .flatten()
                    .any(|m| kinds.contains(&m.kind) && m.alpha > 0)
            };
            if drew(&[PetMoteKind::Zee]) {
                if parked {
                    walk.zee_offers += 1;
                } else if own {
                    walk.zee_train += 1;
                }
            }
            if drew(&[PetMoteKind::Note, PetMoteKind::Heart]) {
                if parked {
                    walk.note_offers += 1;
                } else if own {
                    walk.note_train += 1;
                }
            }
            if own && drew(&[PetMoteKind::Dust]) {
                walk.dust_train += 1;
            }
            if own && drew(&[PetMoteKind::ZeePop]) {
                walk.pop_train += 1;
            }
            note_births(&a, &mut pet_birth);
            let b_at = twin.tick(sense(t, Some(caret)));
            note_births(&b_at, &mut twin_birth);
            let b_after = if parked {
                tb = t + hair;
                let f = twin.tick(sense(tb, Some(caret)));
                note_births(&f, &mut twin_birth);
                f
            } else {
                tb = t;
                b_at
            };
            let unkey = |mut look: Look, offered: bool| {
                if !offered {
                    look.keyed = [None; PET_MOTES_MAX];
                }
                look
            };
            let full = visible(&a, NO_SHIFT);
            let is_note = |l: &Option<MoteLook>| {
                l.is_some_and(|l| matches!(l.kind, PetMoteKind::Note | PetMoteKind::Heart))
            };
            let mut note_edge = false;
            for (now, prev) in full.keyed.iter().zip(last_full.keyed.iter()) {
                if is_note(now) && !is_note(prev) {
                    walk.note_births += 1;
                }
                if (is_note(now) || is_note(prev)) && now != prev {
                    note_edge = true;
                }
            }
            if parked && note_edge {
                walk.note_edges += 1;
            }
            let key = unkey(full, parked);
            let shift = zee_shifts(&pet_birth, &twin_birth);
            let trio = [
                unkey(visible(&b_last, shift), parked),
                unkey(visible(&b_at, shift), parked),
                unkey(visible(&b_after, shift), parked),
            ];
            assert!(
                key.within([&trio[0], &trio[1], &trio[2]]),
                "golden: at +{:?} the offered frame {key:?} is not the train's frame \
                 ({:?} / {:?} / {:?} a millisecond either side); feet {} vs {}",
                t - start,
                trio[0],
                trio[1],
                trio[2],
                a.col,
                b_at.col
            );
            assert!(
                [&b_last, &b_at, &b_after].iter().any(|b| {
                    (a.scale_y - b.scale_y).abs() < 1e-3 && (a.scale_x - b.scale_x).abs() < 1e-3
                }),
                "golden: the scale at +{:?} drifted ({}, {}) vs ({}, {}) / ({}, {}) / ({}, {})",
                t - start,
                a.scale_x,
                a.scale_y,
                b_last.scale_x,
                b_last.scale_y,
                b_at.scale_x,
                b_at.scale_y,
                b_after.scale_x,
                b_after.scale_y
            );
            assert!(
                pet.needs_frames() || same_feet(a.col, b_at.col),
                "golden: the feet at +{:?} drifted a pixel ({} vs {})",
                t - start,
                a.col,
                b_at.col
            );
            was_idle = parked && full == last_full;
            last_full = full;
            last = unkey(full, !pet.needs_frames());
            last_col = a.col;
            b_last = b_after;
        }
        (walk, t)
    }

    fn sense_cells(now: Instant, caret: Option<(u16, u16)>, cell_w: u16, cell_h: u16) -> PetSense {
        PetSense {
            caret_drawn: true,
            cell_w,
            cell_h,
            ..sense(now, caret)
        }
    }

    #[test]
    fn a_move_that_ends_a_pause_lands_on_windows_the_pause_has_aged() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        let mut col = 12u16;
        for _ in 0..6 {
            t += Duration::from_millis(100);
            col += 1;
            pet.tick(sense(t, Some((4, col))));
        }
        t += Duration::from_secs(4);
        col += 1;
        pet.tick(sense(t, Some((4, col))));
        let fresh = (1.0 / VEL_TAU) * (-0.1f32 / VEL_TAU).exp();
        assert!(
            pet.vhat >= 0.95 * fresh,
            "the key that ended the pause must keep its impulse: vhat {} (fresh impulse {fresh})",
            pet.vhat
        );
    }

    #[test]
    fn the_offer_is_the_renderer_s_own_pixel_step_at_an_odd_cell_height() {
        let start = Instant::now();
        let (cw, ch) = (7u16, 14u16);
        let mut pet = PetBrain::default();
        let mut twin = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        let tt = awake(&mut twin, start, 4, 10);
        assert_eq!(t, tt, "the twins share one clock");
        pet.force_seed(0);
        twin.force_seed(0);
        let mut settled = 0.0f32;
        while settled < SLEEP_AFTER + 0.5 {
            t += Duration::from_millis(16);
            pet.tick(sense_cells(t, Some((4, 12)), cw, ch));
            twin.tick(sense_cells(t, Some((4, 12)), cw, ch));
            settled += 0.016;
        }
        assert_eq!(pet.action, PetAction::Sleep, "fixture: a light sleeper");
        let hair = Duration::from_millis(1);
        let mut twin_t = t;
        let mut twin_box = twin
            .tick(sense_cells(twin_t, Some((4, 12)), cw, ch))
            .body_px(cw, ch, 100, 30);
        let mut checked = 0;
        while checked < 10 {
            if pet.needs_frames() {
                t += Duration::from_millis(16);
                pet.tick(sense_cells(t, Some((4, 12)), cw, ch));
                continue;
            }
            let Some(offer) = pet.next_change_deadline(t) else {
                break;
            };
            let mut first_change = None;
            while twin_t < offer + hair {
                twin_t += hair;
                let f = twin.tick(sense_cells(twin_t, Some((4, 12)), cw, ch));
                let b = f.body_px(cw, ch, 100, 30);
                if b != twin_box {
                    twin_box = b;
                    first_change.get_or_insert(twin_t);
                }
            }
            if let Some(first) = first_change {
                assert!(
                    offer <= first + hair,
                    "offer #{checked} is late: offered +{:?} after the check began, the drawn box changed at +{:?}",
                    offer - t,
                    first - t
                );
            }
            checked += 1;
            t = offer;
            pet.tick(sense_cells(t, Some((4, 12)), cw, ch));
        }
        assert!(checked >= 5, "fixture: the sleeper offered {checked} steps");
    }

    fn settled_pair(start: Instant, quiet: f32) -> (PetBrain, PetBrain, Instant) {
        let mut pet = PetBrain::default();
        let mut twin = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let tt = awake(&mut twin, start, 4, 10);
        assert_eq!(t, tt, "the twins share one clock");
        pet.force_seed(0);
        twin.force_seed(0);
        let more = quiet - (SIT_AFTER + 0.2);
        let (t, _) = idle(&mut pet, t, (4, 12), more);
        let _ = idle(&mut twin, tt, (4, 12), more);
        (pet, twin, t)
    }

    #[test]
    fn a_moving_pet_still_asks_for_every_frame() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 4);
        let (mut perk, mut leap, mut land) = (false, false, false);
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 60))));
            perk |= f.action == PetAction::Perk;
            leap |= f.action == PetAction::Leap;
            land |= f.action == PetAction::Land;
            let moving = !pet.action.settled()
                || pet.action == PetAction::Perk
                || pet.flight.is_some()
                || pet.land_t > 0.0;
            if !moving {
                break;
            }
            assert!(
                pet.needs_frames(),
                "moving ({:?}, flight {}, land_t {}) must hold the frame train",
                f.action,
                pet.flight.is_some(),
                pet.land_t
            );
        }
        assert!(
            perk && leap && land,
            "fixture: the crossing must notice, fly and land (perk {perk}, leap {leap}, land {land})"
        );
        let (t, _) = idle(&mut pet, t, (4, 61), 3.0);
        assert!(!pet.needs_frames(), "settled: parked");
        pet.note_petted(t);
        assert!(
            pet.needs_frames(),
            "a latched pet needs the tick that consumes it"
        );
        let f = pet.tick(sense(t + Duration::from_millis(16), Some((4, 61))));
        assert!(
            pet.pet_hold_t > 0.0 && pet.needs_frames(),
            "the petting hold ({:?}) animates every frame",
            f.action
        );
    }

    #[test]
    fn a_lone_key_on_a_quiet_screen_costs_the_pet_a_counted_census() {
        let start = Instant::now();
        let (mut pet, mut twin, t) = settled_pair(start, 5.0);
        let t = t + Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 13))));
        let _ = twin.tick(sense(t, Some((4, 13))));
        assert!(pet.quiet < 0.1, "fixture: the key reset the quiet clock");
        let (walk, end) = walk_offers(&mut pet, &mut twin, t, (4, 13), 40.0);
        assert_eq!(pet.action, PetAction::Sleep);
        assert!(
            pet.quiet >= SLEEP_AFTER + BREATH_WINDOW,
            "the walk must end in deep sleep, not on a timeout: quiet {}",
            pet.quiet
        );
        let span = (end - t).as_secs_f32();
        eprintln!(
            "pet lane census (one key on a quiet screen → deep sleep): {walk:?} over {span:.1} s; \
             the old train was {} frames",
            (span * 60.0) as u32
        );
        assert_eq!(walk.zee_train, 0, "a z held the frame train ({walk:?})");
        assert!(
            walk.train <= 50 && walk.offers - walk.zee_offers <= 50 && walk.zee_offers <= 300,
            "{walk:?} over {span:.1} s — the old train was {} frames",
            (span * 60.0) as u32
        );
        assert!(
            walk.zee_offers >= 100,
            "the light sleeper's z's ride the offer ({walk:?})"
        );
    }

    fn contented_pair(start: Instant, quiet: f32) -> (PetBrain, PetBrain, Instant) {
        let mut pet = PetBrain::default();
        let mut twin = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let tt = awake(&mut twin, start, 4, 10);
        assert_eq!(t, tt, "the twins share one clock");
        pet.force_seed(0);
        twin.force_seed(0);
        let (t, _) = type_run(&mut pet, t, 4, 12, 60, 0.05);
        let (tt, _) = type_run(&mut twin, tt, 4, 12, 60, 0.05);
        assert_eq!(t, tt, "the twins share one clock");
        let (t, _) = idle(&mut pet, t, (4, 72), quiet);
        let _ = idle(&mut twin, tt, (4, 72), quiet);
        assert!(
            pet.content >= PURR_GATE,
            "fixture: the run earned a purr (content {})",
            pet.content
        );
        (pet, twin, t)
    }

    fn needs_frames_without_motes(pet: &mut PetBrain) -> bool {
        let lane = pet.motes;
        pet.motes = [None; PET_MOTES_MAX];
        let needs = pet.needs_frames();
        pet.motes = lane;
        needs
    }

    #[test]
    fn a_purring_seat_s_notes_and_hearts_ride_the_offer_not_the_frame_train() {
        let start = Instant::now();
        let (mut pet, mut twin, mut t) = contented_pair(start, SIT_AFTER + 0.3);
        assert!(pet.purring(), "fixture: a purring seat ({:?})", pet.action);
        let mut note = None;
        for _ in 0..800 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 72))));
            let _ = twin.tick(sense(t, Some((4, 72))));
            let floated =
                f.motes.iter().flatten().find(|m| {
                    matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart) && m.alpha > 0
                });
            if let Some(m) = floated
                && !needs_frames_without_motes(&mut pet)
            {
                note = Some(*m);
                break;
            }
        }
        let note = note.expect("fixture: the seat floats a ♪ or ♥ while otherwise parked");
        assert!(pet.purring(), "fixture: still purring ({:?})", pet.action);
        assert!(
            !pet.needs_frames(),
            "a purring seat's ♪/♥ ride the offer, not the frame train \
             (a {:?} at ({}, {}), alpha {})",
            note.kind,
            note.col,
            note.row,
            note.alpha
        );
        let d = pet
            .next_change_deadline(t)
            .expect("a live ♪ has a next edge");
        assert!(
            d <= t + Duration::from_secs_f32(PURR_MOTE_LIFE / PURR_MOTE_STEPS + 0.001),
            "the ♪'s next look is at most one slice away: +{:?}",
            d - t
        );
        let (walk, end) = walk_offers(
            &mut pet,
            &mut twin,
            t,
            (4, 72),
            SLEEP_AFTER + BREATH_WINDOW + 2.0,
        );
        assert!(
            pet.quiet >= SLEEP_AFTER + BREATH_WINDOW && pet.motes.iter().all(Option::is_none),
            "the walk must end in deep sleep with an empty lane: quiet {}",
            pet.quiet
        );
        let span = (end - t).as_secs_f32();
        eprintln!(
            "pet lane census (a purring seat, the ♪/♥ on the offer): {walk:?} over {span:.1} s"
        );
        assert_eq!(walk.note_train, 0, "a ♪/♥ held the frame train ({walk:?})");
        assert_eq!(walk.zee_train, 0, "a z held the frame train ({walk:?})");
        let climb_px = (0.85 * 20.0_f32).ceil() + 1.0;
        let sway_px = ((0.30 + 0.40) * 10.0_f32).ceil() + 1.0 + 2.0;
        let per_note = (climb_px + sway_px + PURR_MOTE_STEPS) as u32;
        assert!(
            walk.note_births >= 3,
            "fixture: the ledger buys a run of ♪/♥ ({walk:?})"
        );
        assert!(
            walk.note_edges >= 30 && walk.note_edges <= (walk.note_births + 1) * per_note,
            "the ♪/♥ are drawn on the offer — a whole pixel, a look, a birth, a death at a \
             time: {} edges for {} of them, the bound {per_note} a ♪ ({walk:?})",
            walk.note_edges,
            walk.note_births
        );
        assert!(
            walk.idle_wakes <= 40,
            "{} wakes changed nothing — the tell's offer is too conservative ({walk:?})",
            walk.idle_wakes
        );
    }

    #[test]
    fn a_zero_dt_tick_does_not_park_a_gliding_body() {
        let start = Instant::now();
        let (mut pet, _, t) = contented_pair(start, 2.0);
        pet.col -= 0.5;
        let t = t + Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 73))));
        let t = t + Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 73))));
        let gap_px = (pet.home - pet.col).abs() * 10.0;
        assert!(
            gap_px >= 1.0 && pet.speed.abs() < 0.01,
            "fixture: the body is easing home with a pixel to go ({gap_px} px, speed {})",
            pet.speed
        );
        assert!(pet.needs_frames(), "fixture: the glide holds the lane");
        let _ = pet.tick(sense(t, Some((4, 73))));
        assert!(
            pet.needs_frames(),
            "a zero-dt tick parked a body with {gap_px} px still to glide"
        );
    }

    #[test]
    fn landing_dust_holds_the_train_for_a_bounded_beat_past_the_landing() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 4);
        let caret = (4u16, 90u16);
        let bound = ((0.45 + 0.07 * 2.0 - LAND_DUR) / 0.016).ceil() as u32;
        let (mut alive, mut own, mut landed) = (0u32, 0u32, false);
        for _ in 0..300 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some(caret)));
            landed |= f.action == PetAction::Land;
            let any = f
                .motes
                .iter()
                .flatten()
                .any(|m| m.kind == PetMoteKind::Dust && m.alpha > 0);
            if any {
                alive += 1;
                assert!(pet.needs_frames(), "live dust is motion");
                if !needs_frames_without_motes(&mut pet) {
                    own += 1;
                }
            }
        }
        assert!(landed, "fixture: the jump landed");
        assert!(alive >= 1, "fixture: the landing threw its dust");
        assert!(
            own <= bound,
            "the dust held the frame train {own} frames past the landing, bound {bound}"
        );
        assert!(
            alive <= (0.59 / 0.016) as u32 + 2,
            "the dust outlived its life: {alive} frames"
        );
    }

    #[test]
    fn a_repeating_jump_must_not_pin_the_pet_in_its_double_take() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 4);
        let start_col = pet.tick(sense(t, Some((4, 6)))).col;
        let mut caret = 6u16;
        let mut travelled = 0.0f32;
        let mut last = start_col;
        for _ in 0..60 {
            t += Duration::from_millis(16);
            caret = (caret + 8).min(95);
            let f = pet.tick(sense(t, Some((4, caret))));
            travelled += (f.col - last).abs();
            last = f.col;
        }
        assert!(
            travelled > 5.0,
            "the pet froze through a held jump: {travelled} cells of travel while \
             the caret ran to column {caret}"
        );
    }

    #[test]
    fn a_held_word_jump_to_the_line_end_never_sends_the_cat_a_row_down() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 4);
        let _ = pet.tick(sense(t, Some((4, 6))));
        let mut caret = 6u16;
        let mut lowest = 4.0f32;
        for _ in 0..150 {
            t += Duration::from_millis(16);
            caret = (caret + 8).min(95);
            let f = pet.tick(sense(t, Some((4, caret))));
            lowest = lowest.max(f.row);
        }
        assert!(
            lowest < 4.25,
            "the cat went a row below the caret's (row {lowest:.2}) for a wrap that never came"
        );
        let f = pet.tick(sense(t + Duration::from_millis(16), Some((4, 95))));
        assert!(
            (f.row - 4.0).abs() < 0.01 && f.col > 80.0,
            "fixture: the cat came to the caret on its row, at ({:.2}, {:.2})",
            f.col,
            f.row
        );
    }

    #[test]
    fn a_blink_the_tail_covered_never_shows_its_last_present() {
        let mut pet = PetBrain::default();
        let mut q = SIT_AFTER;
        while pet
            .sit_blink_beat_at(q - SIT_AFTER)
            .is_none_or(|(p, _)| p > 1e-3)
        {
            q += 0.001;
            assert!(q < SIT_AFTER + 60.0, "fixture: the seat blinks");
        }
        let at = |pet: &mut PetBrain, into: f32, last: PetGlyphId| {
            pet.quiet = q + into;
            pet.last_pose = last;
            pet.sit_blink_shows().is_some()
        };
        assert!(
            at(&mut pet, 0.0, PetGlyphId::PetSit),
            "a blink from its start shows"
        );
        assert!(
            !at(&mut pet, BLINK_DUR - 0.008, PetGlyphId::PetSitFlick),
            "the last 8 ms of a blink the tail covered is not drawn"
        );
        assert!(
            at(&mut pet, BLINK_DUR - 0.008, PetGlyphId::PetSitBlink),
            "a blink on glass runs to its end"
        );
        assert!(
            at(
                &mut pet,
                BLINK_DUR - STILL_SECS - 0.002,
                PetGlyphId::PetSitFlick
            ),
            "a covered blink with two frames left still shows"
        );
    }

    #[test]
    fn a_caret_that_hides_and_returns_elsewhere_is_a_new_sighting_not_a_retreat() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 78);
        for _ in 0..60 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, None));
        }
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((5, 3))));
        assert_ne!(
            f.action,
            PetAction::Startle,
            "reappearing elsewhere is not a backspace"
        );
        let w = art_cols(10, 20);
        assert!(
            (f.col - PetBrain::station(3, 100, w)).abs() < 0.01,
            "and it re-materialises at the new station, got col {}",
            f.col
        );
    }

    #[test]
    fn the_first_tick_of_a_fade_in_keeps_the_frame_lane() {
        let mut pet = PetBrain::default();
        let f = pet.tick(sense(Instant::now(), Some((5, 10))));
        assert_eq!(f.alpha, 0, "the ramp has not moved on the seeding tick");
        assert!(
            pet.needs_frames(),
            "but the pet is arriving, so it still needs frames"
        );
    }

    #[test]
    fn the_first_windowless_still_materializes_the_resident_pet() {
        let mut pet = PetBrain::default();
        let f = pet.tick_static_capture(sense(Instant::now(), Some((5, 10))));
        assert_eq!(f.alpha, 255, "a requested still contains the resident pet");
        assert!(pet.is_active(), "status agrees with the captured pixels");
        assert!(
            (f.col - PetBrain::station(10, 100, art_cols(10, 20))).abs() < 0.01,
            "the static capture uses the normal first-sighting station"
        );
    }

    #[test]
    fn the_leap_arc_leaves_the_ground_and_lands_back_on_a_baseline() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        let mut peak = 0.0f32;
        let mut last = 0.0f32;
        for _ in 0..160 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 20))));
            peak = peak.max(f.lift);
            last = f.lift;
            if f.action == PetAction::Land {
                break;
            }
        }
        assert!(
            peak > 0.3,
            "the arc must actually leave the floor, got {peak}"
        );
        assert!(
            last.abs() < 1e-3,
            "and the landing must be exactly on the baseline, got {last}"
        );
    }

    #[test]
    fn a_single_backspace_flinches_without_the_full_bottle() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, _) = type_run(&mut pet, start, 2, 10, 8, 0.06);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((2, 17))));
        assert_ne!(
            f.action,
            PetAction::Startle,
            "one backspace must not fire the whole bottle"
        );
        assert_eq!(f.action, PetAction::Perk, "the flinch is a perk-and-duck");
        assert_eq!(f.pose, PetGlyphId::PetPerk);
        assert!(
            f.scale_y < 1.0 && f.scale_x > 1.0,
            "and the duck must be visible in the envelope: y {} x {}",
            f.scale_y,
            f.scale_x
        );
    }

    #[test]
    fn a_big_retreat_still_bottles() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (t, _) = type_run(&mut pet, start, 2, 10, 8, 0.06);
        let (mut t, _) = idle(&mut pet, t, (2, 18), 0.5);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((2, 13))));
        assert_eq!(f.action, PetAction::Startle);
        assert_eq!(f.pose, PetGlyphId::PetStartle);
        assert!(
            f.facing_left,
            "in open grid the caret retreats to the pet's left"
        );
    }

    #[test]
    fn startle_faces_the_caret_even_at_the_wall() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 93);
        assert!(
            pet.col < 90.0,
            "fixture: the pet must be parked left of the caret, col {}",
            pet.col
        );
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((5, 90))));
        assert_eq!(f.action, PetAction::Startle);
        assert!(
            !f.facing_left,
            "the fright must face the caret, not a compass point"
        );
    }

    #[test]
    fn a_held_backspace_is_one_sustained_fright_not_a_strobe() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, _) = type_run(&mut pet, start, 2, 10, 10, 0.05);
        let mut col = 19u16;
        let mut episodes = 0;
        let mut was = PetAction::Walk;
        for _ in 0..8 {
            t += Duration::from_millis(40);
            col -= 1;
            let f = pet.tick(sense(t, Some((2, col))));
            if f.action == PetAction::Startle && was != PetAction::Startle {
                episodes += 1;
            }
            was = f.action;
        }
        assert_eq!(episodes, 1, "one burst, one fright — got {episodes}");
    }

    #[test]
    fn fussing_back_and_forth_makes_it_frolic() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 6, 20);
        let cols = [23u16, 22, 23, 22, 23];
        let mut frolicked = false;
        for c in cols {
            for _ in 0..6 {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((6, c))));
                if f.action == PetAction::Frolic {
                    frolicked = true;
                }
            }
        }
        assert!(frolicked, "three reversals inside the window is play");
    }

    #[test]
    fn quiet_settles_then_sleeps_and_typing_wakes_it_with_a_stretch() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 18);
        let (t, f) = idle(&mut pet, t, (4, 20), 0.4);
        assert!(
            matches!(f.action, PetAction::Sit | PetAction::Purr),
            "a short quiet sits, got {:?}",
            f.action
        );
        let (mut t, f) = idle(&mut pet, t, (4, 20), SLEEP_AFTER);
        assert_eq!(f.action, PetAction::Sleep, "a long quiet sleeps");
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 21))));
        assert_eq!(f.action, PetAction::Waking);
        assert_eq!(f.pose, PetGlyphId::PetStretch);
    }

    #[test]
    fn a_good_run_earns_a_purr_and_a_cold_start_does_not() {
        let start = Instant::now();
        let mut worked = PetBrain::default();
        let t = awake(&mut worked, start, 3, 2);
        let (t, _) = type_run(&mut worked, t, 3, 4, 40, 0.05);
        let (_, f) = idle(&mut worked, t, (3, 44), SIT_AFTER + 0.2);
        assert_eq!(f.action, PetAction::Sit, "the seat still comes first");
        assert!(f.purr > 0.0, "a sentence of running earns the purr at once");

        let mut idle_cat = PetBrain::default();
        let t = awake(&mut idle_cat, start, 3, 40);
        let (_, g) = idle(&mut idle_cat, t, (3, 42), 0.6);
        assert_eq!(g.action, PetAction::Sit, "sitting down cold does not");
        assert_eq!(g.purr, 0.0);
    }

    #[test]
    fn the_pet_never_leaves_the_viewport() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        let w = art_cols(10, 20);
        for col in [0u16, 99, 0, 99, 50] {
            for _ in 0..40 {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((0, col))));
                assert!(f.col >= 0.0, "left edge escaped: {}", f.col);
                assert!(f.col + w <= 100.0 + 1e-3, "right edge escaped: {}", f.col);
                assert!(f.row >= 0.0 && f.row <= 29.0, "row escaped: {}", f.row);
            }
        }
    }

    #[test]
    fn a_hidden_caret_fades_the_pet_out_and_the_fingerprint_settles_to_zero() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, mut last) = idle(&mut pet, start, (4, 20), 1.0);
        assert!(last.alpha > 0, "fixture: on glass");
        for _ in 0..80 {
            t += Duration::from_millis(16);
            last = pet.tick(sense(t, None));
        }
        assert_eq!(last.alpha, 0, "a hidden caret grounds the pet");
        assert_eq!(last.fp(), 0, "and the repaint key settles");
        assert!(!pet.is_active());
    }

    #[test]
    fn reduced_motion_pins_the_pet_at_its_station_with_no_arc_or_gait() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        let w = art_cols(10, 20);
        let mut fps = std::collections::BTreeSet::new();
        for col in [10u16, 60, 12] {
            for _ in 0..20 {
                t += Duration::from_millis(16);
                let mut s = sense(t, Some((3, col)));
                s.reduced_motion = true;
                let f = pet.tick(s);
                assert_eq!(f.lift, 0.0, "no arcs under reduced motion");
                assert_eq!(f.scale_x, 1.0);
                assert_eq!(f.scale_y, 1.0);
                assert!(matches!(f.action, PetAction::Sit | PetAction::Sleep));
                assert!((f.col - PetBrain::station(col, 100, w)).abs() < 1e-3);
                assert_eq!(f.alpha, 255, "a static pet appears in one sample");
                assert_eq!(
                    pet.home, pet.col,
                    "the static placement is its completed home"
                );
                assert_eq!(pet.col_at_tick, pet.col, "no unfinished glide is recorded");
                assert!(
                    !pet.needs_frames(),
                    "a jump or idle still owes no animation"
                );
                fps.insert(f.fp());
            }
        }
        assert!(
            fps.len() <= 3,
            "a reduced-motion pet must SETTLE (one still frame per caret cell), \
             not animate: {} distinct frames",
            fps.len()
        );

        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((3, 90))));
        assert!(pet.needs_frames(), "re-enabled motion remains owed");
        t += Duration::from_millis(16);
        let mut reduced = sense(t, Some((3, 90)));
        reduced.reduced_motion = true;
        assert_eq!(pet.tick(reduced).alpha, 255);
        assert!(
            !pet.needs_frames(),
            "reducing a moving brain settles its debt"
        );
    }

    #[test]
    fn a_suspended_host_clock_cannot_teleport_the_pet() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (_, f0) = idle(&mut pet, start, (4, 10), 1.0);
        let f1 = pet.tick(sense(start + Duration::from_secs(3600), Some((4, 90))));
        let jump = (f1.col - f0.col).abs();
        assert!(
            jump < 5.0,
            "a clamped dt must bound the per-frame step, moved {jump} cells"
        );
    }

    #[test]
    fn the_chase_converges_without_oscillating_around_the_station() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        let w = art_cols(10, 20);
        let target = PetBrain::station(60, 100, w);
        let mut crossings = 0;
        let mut side = 0i8;
        for _ in 0..200 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((3, 60))));
            let s = if f.col > target + 0.02 {
                1
            } else if f.col < target - 0.02 {
                -1
            } else {
                0
            };
            if s != 0 && side != 0 && s != side {
                crossings += 1;
            }
            if s != 0 {
                side = s;
            }
        }
        assert!(
            crossings <= 1,
            "a follower must not ring: {crossings} crossings"
        );
    }

    #[test]
    fn facing_follows_travel_with_hysteresis() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        let mut f = pet.tick(sense(t, Some((3, 10))));
        for _ in 0..140 {
            t += Duration::from_millis(16);
            f = pet.tick(sense(t, Some((3, 80))));
        }
        assert!(!f.facing_left, "chasing right, facing right");
        for _ in 0..80 {
            t += Duration::from_millis(16);
            f = pet.tick(sense(t, Some((3, 5))));
        }
        assert!(f.facing_left, "chasing left, facing left");
    }

    #[test]
    fn a_mid_appearance_look_sync_parks_until_the_pet_has_faded_out() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 10);
        assert!(pet.is_active(), "the fixture needs a visible pet");
        assert_eq!(
            pet.sync_look((3, 1), PetArrival::Ceremony).worn,
            (3, 1),
            "the first dress applies"
        );
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (3, 1),
            "a repoint mid-appearance keeps the worn coat"
        );
        for _ in 0..120 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, None));
        }
        assert!(!pet.is_active(), "the fade-out must complete");
        for _ in 0..4 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((5, 10))));
        }
        assert!(pet.is_active(), "the next appearance is under way");
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (9, 4),
            "the parked pair landed at zero alpha, so the new appearance wears it"
        );
    }

    #[test]
    fn resident_palette_is_owned_by_one_pet_appearance() {
        let dark = CatColorKey {
            accent: 1,
            background: 0,
        };
        let light = CatColorKey {
            accent: 7,
            background: 3,
        };
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 10);
        assert_eq!(pet.appearance_colors(), None);
        assert_eq!(pet.colors_for_appearance(dark), dark);
        assert_eq!(
            pet.colors_for_appearance(light),
            dark,
            "walking frames keep one stable atlas palette"
        );

        pet.invalidate_colors();
        assert_eq!(
            pet.colors_for_appearance(light),
            light,
            "a host context edge resamples without resetting the brain"
        );
        for _ in 0..120 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, None));
        }
        assert!(!pet.is_active(), "the fixture completes its fade-out");
        assert_eq!(
            pet.appearance_colors(),
            None,
            "zero alpha retires the old appearance palette"
        );

        for _ in 0..4 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((5, 10))));
        }
        assert!(pet.is_active());
        assert_eq!(pet.colors_for_appearance(dark), dark);
    }

    #[test]
    fn stand_is_emitted_between_arrival_and_sit() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (t, _) = type_run(&mut pet, t, 4, 12, 3, 0.06);
        let (t, f) = idle(&mut pet, t, (4, 15), 1.0);
        assert_eq!(f.action, PetAction::Stand, "arrived + quiet < SIT_AFTER");
        assert_eq!(f.pose, PetGlyphId::PetStand, "the shipped art, on glass");
        let (_, g) = idle(&mut pet, t, (4, 15), SIT_AFTER);
        assert!(
            matches!(g.action, PetAction::Sit | PetAction::Purr),
            "the stand folds into the sit, got {:?}",
            g.action
        );
    }

    #[test]
    fn horizontal_flight_stretches_along_the_motion_axis() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        let mut stretched = false;
        for _ in 0..140 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 30))));
            if f.action == PetAction::Leap {
                assert!(
                    f.scale_x >= f.scale_y,
                    "a horizontal leap must never go tall: x {} y {}",
                    f.scale_x,
                    f.scale_y
                );
                if f.scale_x > 1.02 {
                    stretched = true;
                    assert!(f.scale_y < 1.0, "the stretch squashes the height");
                }
            }
        }
        assert!(stretched, "the leap must actually stretch along its travel");
    }

    #[test]
    fn the_gallop_cycle_never_exceeds_half_frame_rate() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 3, 2);
        let mut caret = 4u16;
        let mut first: Option<(f32, f32)> = None;
        let mut last = (0.0f32, 0.0f32);
        for i in 0..240 {
            t += Duration::from_millis(16);
            if i % 3 == 2 {
                caret = (caret + 1).min(95);
            }
            let f = pet.tick(sense(t, Some((3, caret))));
            assert_ne!(
                f.action,
                PetAction::Leap,
                "fixture: this cadence must run the pet, not launch it"
            );
            if f.action == PetAction::Run {
                if let Some((_, prev_s)) = first {
                    let ds = (pet.stride - last.1).rem_euclid(1024.0);
                    assert!(
                        ds <= 0.5,
                        "gallop advanced {ds} cycles in one 16ms frame (> half frame rate)"
                    );
                    let _ = prev_s;
                } else {
                    first = Some((f.col, pet.stride));
                }
                last = (f.col, pet.stride);
            }
        }
        let (c0, s0) = first.expect("a 20 cells/s chase must reach a gallop");
        let cells = (last.0 - c0).abs();
        let cycles = last.1 - s0;
        assert!(cells > 10.0, "need a sustained gallop to measure: {cells}");
        let eff = cells / cycles;
        assert!(
            (eff - RUN_STRIDE_CELLS).abs() < 0.05,
            "the gallop must pace itself by RUN_STRIDE_CELLS ({RUN_STRIDE_CELLS}), measured \
             {eff} cells per cycle"
        );
    }

    fn gait_run(gap: f32, keys: u16) -> Vec<(PetAction, PetGlyphId, f32)> {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 3, 5);
        let mut out = Vec::new();
        for k in 0..keys {
            let until = t + Duration::from_secs_f32(gap);
            while t < until {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((3, 7 + k))));
                out.push((f.action, f.pose, pet.stride));
            }
            t = until;
            let f = pet.tick(sense(t, Some((3, 8 + k))));
            out.push((f.action, f.pose, pet.stride));
        }
        out
    }

    fn cycle_beat(pose: PetGlyphId) -> Option<(bool, usize)> {
        if let Some(i) = PetBrain::CYCLE_LWALK.iter().position(|p| *p == pose) {
            return Some((true, i));
        }
        PetBrain::CYCLE_WALK
            .iter()
            .position(|p| *p == pose)
            .map(|i| (false, i))
    }

    #[test]
    fn a_slow_caret_walks_laterally_and_a_brisk_one_trots() {
        let slow = gait_run(0.45, 12);
        let slow_walk: Vec<PetGlyphId> = slow
            .iter()
            .filter(|(a, _, _)| *a == PetAction::Walk)
            .map(|(_, p, _)| *p)
            .collect();
        assert!(
            slow_walk.len() > 40,
            "fixture: a pecking caret must walk the cat ({} frames)",
            slow_walk.len()
        );
        assert!(
            slow_walk.iter().all(|p| PetBrain::CYCLE_LWALK.contains(p)),
            "a pecked caret is ambled after, never trotted after: {:?}",
            slow_walk
                .iter()
                .filter(|p| !PetBrain::CYCLE_LWALK.contains(p))
                .collect::<Vec<_>>()
        );
        assert!(
            slow.iter().all(|(a, _, _)| *a != PetAction::Run),
            "fixture: and never runs"
        );

        let brisk = gait_run(0.30, 12);
        let brisk_walk: Vec<PetGlyphId> = brisk
            .iter()
            .filter(|(a, _, _)| *a == PetAction::Walk)
            .map(|(_, p, _)| *p)
            .collect();
        assert!(
            brisk_walk.iter().any(|p| PetBrain::CYCLE_WALK.contains(p)),
            "a prose cadence earns the trot"
        );

        let mut prev: Option<usize> = None;
        for (a, pose, _) in &brisk {
            if *a != PetAction::Walk {
                prev = None;
                continue;
            }
            let (_, idx) = cycle_beat(*pose).expect("a walk frame is a walk pose");
            if let Some(p) = prev {
                let step = (idx + 4 - p) % 4;
                assert!(
                    step <= 1,
                    "the gait skipped from beat {p} to {idx} across the flip"
                );
            }
            prev = Some(idx);
        }
    }

    #[test]
    fn a_steady_rhythm_walks_the_cat_on_without_a_stop_at_each_key() {
        for gap in [0.05f32, 0.07, 0.10, 0.14, 0.18, 0.22, 0.28, 0.35, 0.45] {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let mut t = awake(&mut pet, start, 3, 5);
            let (mut stands, mut changes, mut slowest) = (0usize, 0usize, f32::MAX);
            let mut last = None;
            for k in 0..24u16 {
                let until = t + Duration::from_secs_f32(gap);
                while t < until {
                    t += Duration::from_millis(16);
                    let f = pet.tick(sense(t, Some((3, 7 + k))));
                    if k >= 6 {
                        stands += usize::from(f.action == PetAction::Stand);
                        changes += usize::from(last.is_some_and(|a| a != f.action));
                        slowest = slowest.min(pet.speed.abs());
                    }
                    last = Some(f.action);
                }
                t = until;
                let _ = pet.tick(sense(t, Some((3, 8 + k))));
            }
            assert!(
                stands == 0 && changes <= 1 && slowest > 1.0,
                "a {gap} s rhythm walks the cat on: {stands} standing frames, \
                 {changes} gait changes, slowest {slowest:.2} cells/s"
            );
        }
    }

    #[test]
    fn a_late_key_lets_the_cat_coast_on_not_stop_dead() {
        for late in [0.15f32, 0.2, 0.3, 0.45] {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let mut t = awake(&mut pet, start, 3, 5);
            let (mut stands, mut shed) = (0usize, 0.0f32);
            let mut was = None::<f32>;
            let mut stood: Vec<f32> = Vec::new();
            let mut standing: Option<Instant> = None;
            for k in 0..20u16 {
                let gap = if k == 12 { late } else { 0.1 };
                let until = t + Duration::from_secs_f32(gap);
                while t < until {
                    t += Duration::from_millis(16);
                    let f = pet.tick(sense(t, Some((3, 7 + k))));
                    if k >= 6 {
                        let stand = f.pose == PetGlyphId::PetStand;
                        stands += usize::from(stand);
                        match (stand, standing) {
                            (true, None) => standing = Some(t),
                            (false, Some(from)) => {
                                stood.push((t - from).as_secs_f32());
                                standing = None;
                            }
                            _ => {}
                        }
                        if let Some(v) = was {
                            shed = shed.max(v - pet.speed.abs());
                        }
                    }
                    was = Some(pet.speed.abs());
                }
                t = until;
                let _ = pet.tick(sense(t, Some((3, 8 + k))));
                was = Some(pet.speed.abs());
            }
            if late <= 0.15 {
                assert_eq!(
                    stands, 0,
                    "a key {late} s after the last keeps the cat running"
                );
            }
            assert!(
                stood.iter().all(|s| *s >= STAND_MIN),
                "a {late} s gap: a stand under a stride ({STAND_MIN} s): {stood:?}"
            );
            assert!(
                shed < 4.0,
                "a {late} s gap slows the cat, never stops it dead: {shed:.2} cells/s lost in a frame"
            );
        }
    }

    #[test]
    fn a_leg_after_the_caret_was_gone_picks_its_gait_afresh() {
        let hands: [&[f32]; 2] = [
            &[0.12, 0.12, 0.12, 0.3, 0.08, 0.08, 0.08, 0.08, 0.08],
            &[0.15, 0.15, 0.15, 0.15, 0.15, 0.07, 0.07, 0.07, 0.07, 0.07],
        ];
        let family = |pose: PetGlyphId| -> Option<u8> {
            if PetBrain::CYCLE_LWALK.contains(&pose) {
                Some(0)
            } else if PetBrain::CYCLE_WALK.contains(&pose) {
                Some(1)
            } else if PetBrain::CYCLE_RUN.contains(&pose) {
                Some(2)
            } else {
                None
            }
        };
        for (n, gaps) in hands.iter().enumerate() {
            for back in [0.0f32, 0.05] {
                let start = Instant::now();
                let mut pet = PetBrain::default();
                let (t, _) = type_run(&mut pet, start, 3, 5, 4, 0.4);
                let (mut t, _) = idle(&mut pet, t, (3, 9), SIT_AFTER + 0.4);
                for _ in 0..16 {
                    t += Duration::from_millis(16);
                    let _ = pet.tick(sense(t, None));
                }
                let (mut t, _) = idle(&mut pet, t, (3, 9), back);
                let mut worn: Vec<u8> = Vec::new();
                for (key, gap) in (1u16..).zip(gaps.iter()) {
                    let until = t + Duration::from_secs_f32(*gap);
                    while t < until {
                        t += Duration::from_millis(16);
                        let f = pet.tick(sense(t, Some((3, 9 + key))));
                        if let Some(gait) = family(f.pose)
                            && worn.last() != Some(&gait)
                        {
                            assert!(
                                !worn.contains(&gait),
                                "hand {n}, {back} s after the caret came back: back to a gait \
                                 it left (0 amble, 1 walk, 2 run): {worn:?} then {gait}"
                            );
                            worn.push(gait);
                        }
                    }
                }
                assert_eq!(
                    worn.first(),
                    Some(&0),
                    "hand {n}, {back} s after: an amble's pace sets out in the amble, picked \
                     afresh ({worn:?})"
                );
            }
        }
    }

    fn uneven_gaps(seed: u64, n: usize) -> Vec<f32> {
        let mut state = seed;
        (0..n)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (60 + (state >> 33) % 61) as f32 / 1000.0
            })
            .collect()
    }

    #[test]
    fn a_hands_uneven_keys_never_stop_the_cat_mid_stride() {
        for seed in [3u64, 5, 9] {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let mut t = awake(&mut pet, start, 3, 5);
            let (mut stands, mut slow, mut slowest) = (0usize, 0usize, f32::MAX);
            for (k, gap) in (0u16..).zip(uneven_gaps(seed, 40)) {
                let until = t + Duration::from_secs_f32(gap);
                while t + Duration::from_millis(16) < until {
                    t += Duration::from_millis(16);
                    let f = pet.tick(sense(t, Some((3, 7 + k))));
                    if k >= 6 {
                        stands += usize::from(f.action == PetAction::Stand);
                        slow += usize::from(pet.speed.abs() < 1.0);
                        slowest = slowest.min(pet.speed.abs());
                    }
                }
                t = until;
                let _ = pet.tick(sense(t, Some((3, 8 + k))));
            }
            assert!(
                stands == 0 && slow == 0,
                "seed {seed}: uneven keys walk the cat on: {stands} standing frames, \
                 {slow} frames under a cell a second (slowest {slowest:.2} cells/s)"
            );
        }
    }

    #[test]
    fn where_the_cat_rests_does_not_turn_on_the_hosts_cadence() {
        fn rest(gaps: &[f32], paced: bool) -> f32 {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let mut t = awake(&mut pet, start, 3, 5);
            let mut caret = 7u16;
            let mut keys = gaps.iter().scan(t, |at, g| {
                *at += Duration::from_secs_f32(*g);
                Some(*at)
            });
            let mut next_key = keys.next();
            let stop = t + Duration::from_secs_f32(gaps.iter().sum::<f32>() + 3.0);
            loop {
                let frame = Some(t + Duration::from_millis(16));
                let tick = if !paced || pet.needs_frames() {
                    frame
                } else {
                    pet.next_change_deadline(t)
                        .map(|d| d.max(t + Duration::from_millis(1)))
                };
                t = match (tick, next_key) {
                    (Some(a), Some(k)) if a < k => a,
                    (_, Some(k)) => {
                        caret += 1;
                        next_key = keys.next();
                        k
                    }
                    (Some(a), None) if a <= stop => a,
                    _ => break,
                };
                let _ = pet.tick(sense(t, Some((3, caret))));
            }
            pet.col
        }
        let mut late = vec![0.12f32; 30];
        late[15] = 0.5;
        for (name, gaps) in [("a 0.2 s rhythm", vec![0.2f32; 30]), ("a late key", late)] {
            let (flat, paced) = (rest(&gaps, false), rest(&gaps, true));
            assert!(
                (flat - paced).abs() < 0.05,
                "{name}: the cat rests at {flat:.3} ticked every frame, {paced:.3} as the GUI paces it"
            );
        }
    }

    #[test]
    fn a_stand_the_ink_ladder_chose_is_walked_with_not_run_into() {
        fn ink(pet: &mut PetBrain, caret: u16) {
            let mut spans = vec![(0u16, 0u16); 5];
            spans[2] = (0, 100);
            spans[3] = (0, caret + 20);
            spans[4] = (0, 100);
            pet.sense_ink(0, &spans, Some(3));
        }
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 3, 5);
        let c0 = 7u16;
        let settled = t + Duration::from_secs(3);
        while t < settled {
            t += Duration::from_millis(16);
            ink(&mut pet, c0);
            let _ = pet.tick(sense(t, Some((3, c0))));
        }
        let (mut stops, mut slowest) = (0usize, f32::MAX);
        for k in 0..24u16 {
            let until = t + Duration::from_secs_f32(0.1);
            while t < until {
                t += Duration::from_millis(16);
                ink(&mut pet, c0 + k);
                let _ = pet.tick(sense(t, Some((3, c0 + k))));
                if k >= 6 {
                    stops += usize::from(pet.speed.abs() < 1.0);
                    slowest = slowest.min(pet.speed.abs());
                }
            }
            t = until;
            ink(&mut pet, c0 + k + 1);
            let _ = pet.tick(sense(t, Some((3, c0 + k + 1))));
        }
        let last = pet.tick(sense(t + Duration::from_millis(16), Some((3, c0 + 24))));
        assert!(
            last.col >= f32::from(c0 + 24) + 10.0,
            "the cat follows the line's far end, not the caret: at {:.2} for caret {}",
            last.col,
            c0 + 24
        );
        assert_eq!(
            stops, 0,
            "a cat walking with the pushed line never stops on a step (slowest {slowest:.2} cells/s)"
        );
    }

    #[test]
    fn a_typing_rhythm_grows_the_lead_past_the_base_station() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 5);
        let (_, f) = type_run(&mut pet, t, 3, 7, 40, 0.05);
        assert!(
            f.col >= 47.0 + STATION_LEAD + 0.5,
            "a galloping caret must be LED, not chased: pet at {} for caret 47",
            f.col
        );
    }

    #[test]
    fn hunt_and_peck_never_sees_the_lead() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 5);
        let (t, _) = type_run(&mut pet, t, 3, 7, 6, 1.0);
        let (_, f) = idle(&mut pet, t, (3, 13), 0.8);
        assert!(
            (f.col - (13.0 + STATION_LEAD)).abs() <= 0.3,
            "one key a second is not a rhythm: pet at {} for caret 13",
            f.col
        );
    }

    #[test]
    fn alternating_moves_never_lead() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 3, 20);
        let mut max_col = 0.0f32;
        for k in 0..24u16 {
            let c = if k % 2 == 0 { 23 } else { 22 };
            for _ in 0..8 {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((3, c))));
                max_col = max_col.max(f.col);
            }
        }
        assert!(
            max_col <= 23.0 + STATION_LEAD + 0.6,
            "an edited caret must not be led: pet reached {max_col}"
        );
    }

    #[test]
    fn a_pause_leaves_the_cat_where_it_stopped() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 5);
        let (mut t, mut f) = type_run(&mut pet, t, 3, 7, 30, 0.05);
        let (mut last, mut back, mut turned) = (f.col, 0.0f32, false);
        for _ in 0..(2.5 / 0.016) as usize {
            t += Duration::from_millis(16);
            f = pet.tick(sense(t, Some((3, 37))));
            back += (last - f.col).max(0.0);
            turned |= f.facing_left;
            last = f.col;
        }
        assert!(back < 0.01, "no step back on a pause: {back} cells back");
        assert!(!turned, "and no turn");
        assert!(
            f.col >= 37.0 + STATION_LEAD + 1.0 && f.col <= 37.0 + STATION_LEAD + LEAD_MAX,
            "it stays out ahead, where it stopped: pet at {} for caret 37",
            f.col
        );
        assert!(
            f.action.settled(),
            "and the pet settles, got {:?}",
            f.action
        );
    }

    #[test]
    fn the_writing_catches_up_with_a_cat_that_ran_ahead() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 5);
        let (t, _) = type_run(&mut pet, t, 3, 7, 30, 0.05);
        let (t, f) = idle(&mut pet, t, (3, 37), 2.0);
        let seat = f.col;
        assert!(
            seat >= 37.0 + STATION_LEAD + 1.0,
            "fixture: out ahead ({seat})"
        );
        let (t, f) = idle(&mut pet, t, (3, 38), 1.0);
        assert!(
            (f.col - seat).abs() < 0.01,
            "a key short of the cat leaves it seated: {} from {seat}",
            f.col
        );
        let caret = (seat - STATION_LEAD).ceil() as u16 + 1;
        let (t, f) = type_run(&mut pet, t, 3, 38, caret - 38, 1.0);
        let (t, f2) = idle(&mut pet, t, (3, caret), 1.0);
        assert!(
            f2.col > seat && (f2.col - (f32::from(caret) + STATION_LEAD)).abs() <= ARRIVED,
            "caught up, it walks on beside the caret: {} ({:?}) for caret {caret}",
            f2.col,
            f.action
        );
        let (_, f) = idle(&mut pet, t, (4, 2), 2.0);
        assert!(
            (f.col - (2.0 + STATION_LEAD)).abs() <= ARRIVED,
            "a new line starts afresh: pet at {}",
            f.col
        );
    }

    #[test]
    fn a_pounce_mid_rhythm_lands_at_the_predicted_station() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = type_run(&mut pet, t, 4, 12, 20, 0.05);
        let mut caret = 40u16;
        let mut landed: Option<(f32, u16)> = None;
        let mut next_key = t + Duration::from_millis(50);
        for _ in 0..120 {
            t += Duration::from_millis(16);
            if t >= next_key {
                next_key += Duration::from_millis(50);
                caret = (caret + 1).min(95);
            }
            let f = pet.tick(sense(t, Some((4, caret))));
            if f.action == PetAction::Land && landed.is_none() {
                landed = Some((f.col, caret));
            }
        }
        let (col, caret_at_land) = landed.expect("the word jump must pounce");
        assert!(
            col > f32::from(caret_at_land),
            "a predicted aim lands AHEAD of the still-moving caret: landed \
             at {col} while the caret was at {caret_at_land}"
        );
    }

    #[test]
    fn wall_hysteresis_does_not_flip_flop_at_the_crossing() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 92);
        let mut left_frames = 0u32;
        let mut right_frames = 0u32;
        for k in 0..6u16 {
            let c = if k % 2 == 0 { 93 } else { 94 };
            for _ in 0..90 {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((5, c))));
                if f.col < f32::from(c) {
                    left_frames += 1;
                } else {
                    right_frames += 1;
                }
            }
        }
        assert!(left_frames > 400, "the pet must be parked wall-side");
        assert!(
            right_frames == 0,
            "and STAY there through the alternation: {right_frames} frames \
             back on the right of the caret"
        );
    }

    #[test]
    fn the_wall_crossing_transits_with_a_small_arc() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 86);
        let mut airborne = false;
        let mut f = pet.tick(sense(t, Some((5, 91))));
        for _ in 0..90 {
            t += Duration::from_millis(16);
            f = pet.tick(sense(t, Some((5, 94))));
            if f.action == PetAction::Leap && f.lift > 0.0 {
                airborne = true;
            }
        }
        assert!(airborne, "the side flip must arc over the caret");
        assert!(
            f.col < 94.0 - 5.0,
            "and end parked clear on the caret's left, got {}",
            f.col
        );
    }

    #[test]
    fn a_regrid_carries_the_body_with_its_line() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let tall = |t, c| PetSense {
            rows: 40,
            ..sense(t, Some(c))
        };
        let short = |t, c| PetSense {
            rows: 30,
            ..sense(t, Some(c))
        };
        let mut t = start;
        let _ = pet.tick(tall(t, (39, 10)));
        for k in 0..40u16 {
            for _ in 0..6 {
                t += Duration::from_millis(16);
                let _ = pet.tick(tall(t, (39, 10 + k)));
            }
        }
        let f = pet.tick(tall(t, (39, 50)));
        assert!(
            (f.row - 39.0).abs() < 0.01 && f.action == PetAction::Run,
            "fixture: running beside the caret on the last row, got {:?} at row {}",
            f.action,
            f.row
        );
        pet.rebase_geometry();
        let mut actions = Vec::new();
        for i in 0..30u16 {
            t += Duration::from_millis(16);
            let f = pet.tick(short(t, (26, 50 + i / 6)));
            if i == 0 {
                assert!(
                    (f.row - 26.0).abs() < 0.01,
                    "the body goes with its line, got row {}",
                    f.row
                );
            }
            actions.push(f.action);
        }
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, PetAction::Crouch | PetAction::Leap)),
            "no hop back to the line: {actions:?}"
        );
    }

    #[test]
    fn a_regrid_keeps_the_stand_beside_the_caret() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let at = |t, rows, c| PetSense {
            rows,
            ..sense(t, Some(c))
        };
        let mut t = start;
        let _ = pet.tick(at(t, 40, (39, 10)));
        for k in 0..30u16 {
            for _ in 0..6 {
                t += Duration::from_millis(16);
                let _ = pet.tick(at(t, 40, (39, 10 + k)));
            }
        }
        for _ in 0..120 {
            t += Duration::from_millis(16);
            let _ = pet.tick(at(t, 40, (39, 39)));
        }
        let rest = pet.tick(at(t, 40, (39, 39)));
        assert!(
            rest.action.settled() && rest.col > 40.0,
            "fixture: at rest ahead of the caret, got {:?} at col {}",
            rest.action,
            rest.col
        );
        pet.rebase_geometry();
        let mut moved = Vec::new();
        for _ in 0..90 {
            t += Duration::from_millis(16);
            let f = pet.tick(at(t, 30, (26, 39)));
            if (f.col - rest.col).abs() > 0.05 || (f.row - 26.0).abs() > 0.01 {
                moved.push((f.col, f.row, f.action));
            }
        }
        assert!(
            moved.is_empty(),
            "the cat stays where it stood, at col {}: {} frames moved, {:?}",
            rest.col,
            moved.len(),
            &moved[..moved.len().min(4)]
        );
    }

    #[test]
    fn a_regrid_carries_a_body_off_the_carets_line_with_the_content() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let at = |t, rows, c| PetSense {
            rows,
            ..sense(t, Some(c))
        };
        let mut t = start;
        let _ = pet.tick(at(t, 27, (26, 10)));
        for k in 0..30u16 {
            for _ in 0..6 {
                t += Duration::from_millis(16);
                let _ = pet.tick(at(t, 27, (26, 10 + k)));
            }
        }
        for _ in 0..120 {
            t += Duration::from_millis(16);
            let _ = pet.tick(at(t, 27, (26, 39)));
        }
        assert!(
            pet.action.settled() && (pet.row - 26.0).abs() < 0.01,
            "fixture: at rest beside the caret, got {:?} at row {}",
            pet.action,
            pet.row
        );
        pet.row = 25.0;
        pet.rebase_geometry();
        let mut frames = Vec::new();
        for _ in 0..60 {
            t += Duration::from_millis(16);
            frames.push(pet.tick(at(t, 40, (39, 39))));
        }
        assert!(
            (frames[0].row - 38.0).abs() < 0.01,
            "the body goes the rows its line went: row {}",
            frames[0].row
        );
        let far: Vec<(PetAction, f32)> = frames
            .iter()
            .filter(|f| !(37.0..=39.5).contains(&f.row))
            .map(|f| (f.action, f.row))
            .collect();
        assert!(
            far.is_empty(),
            "no leap across the rows after the re-grid: {far:?}"
        );
    }

    fn drive_and_collect(
        pet: &mut PetBrain,
        mut t: Instant,
        caret: (u16, u16),
        ticks: u32,
    ) -> (Vec<PetGlyphId>, Vec<PetFrame>) {
        let mut poses = Vec::new();
        let mut frames = Vec::new();
        for _ in 0..ticks {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some(caret)));
            poses.push(f.pose);
            frames.push(f);
        }
        (poses, frames)
    }

    #[test]
    fn a_half_grid_standing_gap_jumps_once_the_hold_resolves() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let _ = pet.tick(sense(start, Some((3, 5))));
        let (poses, frames) =
            drive_and_collect(&mut pet, start + Duration::from_millis(16), (3, 70), 260);
        assert!(
            poses.contains(&PetGlyphId::PetCrouchWiggle),
            "a half-grid gap earns the gather"
        );
        let last = frames.last().expect("frames");
        assert!(
            (last.col - 71.0).abs() <= SKID_CELLS + 1.0,
            "and the bounds arrive at the station, got {}",
            last.col
        );
    }

    #[test]
    fn a_screen_wide_span_splits_into_two_bounds_unless_the_touch_point_is_inked() {
        for (inked, want) in [(false, 2), (true, 1)] {
            let mut pet = PetBrain::default();
            let t = awake(&mut pet, Instant::now(), 4, 2);
            if inked {
                let spans = vec![(0u16, 0u16), (0, 0), (0, 0), (0, 0), (2, 76)];
                pet.sense_ink(0, &spans, Some(4));
            }
            let (_, frames) = drive_and_collect(&mut pet, t, (4, 78), 260);
            let mut episodes = 0;
            let mut airborne_was = false;
            for f in &frames {
                let airborne = f.action == PetAction::Leap;
                if airborne && !airborne_was {
                    episodes += 1;
                }
                airborne_was = airborne;
                if want == 2 && episodes == 1 && f.action == PetAction::Land {
                    assert_eq!(f.pose, PetGlyphId::PetCrouch, "the touch-land's pose");
                }
            }
            assert_eq!(episodes, want, "inked midfield {inked}: {episodes} bounds");
        }
    }

    #[test]
    fn the_bound_apex_never_clips_the_viewport_top() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 1, 10);
        let (_, frames) = drive_and_collect(&mut pet, t, (1, 60), 220);
        for f in &frames {
            if f.action == PetAction::Leap {
                let top = f.row + 1.0 - ART_ROWS - f.lift;
                assert!(
                    top >= -0.05,
                    "the sprite top left the viewport: row {} lift {}",
                    f.row,
                    f.lift
                );
            }
        }
    }

    fn drop_run(
        pet: &mut PetBrain,
        start: Instant,
        caret: (u16, u16),
        ticks: u32,
    ) -> Vec<PetFrame> {
        let mut t = start;
        let mut out = Vec::new();
        for _ in 0..ticks {
            t += Duration::from_millis(16);
            out.push(pet.tick(sense(t, Some(caret))));
        }
        out
    }

    fn asleep_at(pet: &mut PetBrain, start: Instant, row: u16, col: u16) -> Instant {
        let t = awake(pet, start, row, col);
        let (t, f) = idle(pet, t, (row, col + 2), SLEEP_AFTER + 1.0);
        assert_eq!(f.action, PetAction::Sleep, "fixture: a sleeping cat");
        t
    }

    #[test]
    fn an_enter_drops_the_sleeper_off_the_line() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = asleep_at(&mut pet, start, 4, 18);
        let col0 = pet.col;
        let frames = drop_run(&mut pet, t, (5, 0), 200);

        let f0 = &frames[0];
        assert_eq!(f0.action, PetAction::Leap, "the floor went: the cat falls");
        assert_eq!(f0.pose, PetGlyphId::PetSleep1, "and it is still asleep");
        assert_eq!(f0.lift, 0.0, "a fall has no arc");
        assert!(
            frames.iter().all(|f| f.action != PetAction::Waking),
            "the stretch is spent in the air, never on the old floor"
        );
        assert!(
            frames.iter().all(|f| f.pose != PetGlyphId::PetCrouchWiggle),
            "and a fall is not a show"
        );

        let landed = frames
            .iter()
            .position(|f| f.action == PetAction::Land)
            .expect("the fall must land");
        assert!(
            (frames[landed].row - 5.0).abs() < 0.01,
            "onto the row the line opened, got {}",
            frames[landed].row
        );
        assert!(
            frames[..=landed]
                .iter()
                .all(|f| (f.col - col0).abs() < 0.01),
            "a fall is straight down"
        );

        let last = frames.last().expect("frames");
        assert!(
            last.action.settled()
                && (last.col - PetBrain::station(0, 100, art_cols(10, 20))).abs() < 1.5,
            "and it ends home at its station, got {} ({:?})",
            last.col,
            last.action
        );
        let (_, f) = idle(&mut pet, t, (5, 0), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert_eq!(f.action, PetAction::Sleep);
        assert!(
            !pet.needs_frames(),
            "the drop's latches must all be spent by the time it sleeps"
        );
    }

    #[test]
    fn a_settled_cat_after_a_drop_still_notices() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = asleep_at(&mut pet, start, 4, 18);
        let frames = drop_run(&mut pet, t, (5, 18), 400);
        assert!(
            frames.iter().any(|f| f.action == PetAction::Leap),
            "fixture: the floor went and the cat fell"
        );
        let mut t = t + Duration::from_millis(16 * 400);
        let (t2, f) = idle(&mut pet, t, (5, 18), SIT_AFTER + 0.5);
        t = t2;
        assert!(f.action.settled(), "fixture: settled where it fell");
        assert!(
            !pet.drop_stare,
            "the fall's beat is spent by settling, not carried"
        );
        let mut perked = false;
        let mut launched = false;
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((5, 80))));
            perked |= f.action == PetAction::Perk;
            launched |= f.action == PetAction::Leap;
        }
        assert!(perked, "a settled cat still looks up before a bound");
        assert!(launched, "…and then goes");
    }

    #[test]
    fn only_a_blank_row_just_below_drops_the_sleeper() {
        let start = Instant::now();
        for (name, caret, ink) in [
            ("a caret going up", (3u16, 0u16), false),
            ("a three-line prompt", (7, 0), false),
            ("ink under its own column", (5, 0), true),
        ] {
            let mut pet = PetBrain::default();
            let t = asleep_at(&mut pet, start, 4, 18);
            if ink {
                let mut spans = [(0u16, 0u16); 12];
                spans[5] = (15, 40);
                pet.sense_ink(0, &spans, Some(5));
                assert!(
                    pet.col > 15.0 && pet.col < 40.0,
                    "fixture: ink under the cat"
                );
            }
            let frames = drop_run(&mut pet, t, caret, 40);
            assert_eq!(
                frames[0].action,
                PetAction::Waking,
                "{name}: a wake, not a fall"
            );
            assert_eq!(frames[0].pose, PetGlyphId::PetStretch, "{name}");
            assert!(
                !pet.pending_drop,
                "{name}: nothing is owed a fall afterwards"
            );
        }
    }

    #[test]
    fn a_twice_moved_flight_lands_and_stares_before_it_admits_it() {
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, Instant::now(), 4, 10);
        let mut caret = 30u16;
        let (mut two, mut three) = (false, false);
        let mut frames = Vec::new();
        for _ in 0..300 {
            t += Duration::from_millis(16);
            if let Some(f) = pet.flight {
                let u = f.t / f.dur;
                if !two && u >= 0.3 {
                    caret = 60;
                    two = true;
                } else if two && !three && u >= 0.6 {
                    caret = 20;
                    three = true;
                }
            }
            frames.push(pet.tick(sense(t, Some((4, caret)))));
        }
        assert!(
            two && three,
            "fixture: the caret must move twice in the air"
        );
        let landed = frames
            .iter()
            .position(|f| f.action == PetAction::Land)
            .expect("the pounce lands");
        let stare = frames[landed..]
            .iter()
            .skip_while(|f| f.action == PetAction::Land)
            .take_while(|f| f.action == PetAction::Stand)
            .count();
        assert!(stare > 0, "the cat stands on the empty cell first");
        assert!(
            frames[landed + stare..]
                .iter()
                .any(|f| f.action == PetAction::Perk),
            "then the double-take"
        );
        let last = frames.last().expect("frames");
        assert!(
            (last.col - PetBrain::station(20, 100, art_cols(10, 20))).abs() < 1.5,
            "home at the station it was left with, got {}",
            last.col
        );
        assert_eq!(pet.commit_t, 0.0, "and the beat is spent, not owed");
    }

    #[test]
    fn the_same_typing_still_produces_the_same_cat() {
        let run = |start: Instant| -> Vec<u64> {
            let mut pet = PetBrain::default();
            let mut t = awake(&mut pet, start, 4, 10);
            let mut fps = Vec::new();
            for (row, col, ticks) in [(4u16, 30u16, 90u32), (5, 2, 70), (5, 80, 240), (5, 82, 700)]
            {
                for _ in 0..ticks {
                    t += Duration::from_millis(16);
                    fps.push(pet.tick(sense(t, Some((row, col)))).fp());
                }
            }
            fps
        };
        let a = run(Instant::now());
        let b = run(Instant::now() + Duration::from_millis(7_919));
        assert_eq!(a, b, "the same typing must produce the same cat");
        assert!(
            a.windows(2).any(|w| w[0] != w[1]),
            "and the script actually animates"
        );
    }

    fn sleeper_with_a_live_zee(start: Instant) -> (PetBrain, Instant) {
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + 0.1);
        for _ in 0..400 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12))));
            if f.motes
                .iter()
                .flatten()
                .any(|m| m.kind == PetMoteKind::Zee && m.alpha > 0)
            {
                return (pet, t);
            }
        }
        panic!("fixture: the sleeper never dreamed a visible z");
    }

    #[test]
    fn retire_unowned_kills_a_live_mote_where_withholding_the_caret_does_not() {
        let start = Instant::now();

        let (mut control, mut ct) = sleeper_with_a_live_zee(start);
        ct += Duration::from_millis(16);
        let drifting = control.tick(sense(ct, None));
        assert!(
            drifting.motes.iter().flatten().any(|m| m.alpha > 0),
            "fixture: withholding the caret leaves the mote lane drifting — that \
             is the behaviour this switch exists to overrule"
        );
        assert!(
            control.needs_frames(),
            "…and it is still asking for the 60 fps lane it is about to lose"
        );

        let (mut pet, mut t) = sleeper_with_a_live_zee(start);
        assert!(pet.is_active(), "fixture: a visible pet");
        assert!(
            !pet.needs_frames() && pet.next_change_deadline(t).is_some(),
            "fixture: a drifting z names its next wake"
        );
        pet.retire_unowned();
        assert!(!pet.is_active(), "a retired pet paints nothing");
        assert!(!pet.needs_frames(), "…and owes the scheduler no frames");
        assert!(
            pet.next_change_deadline(t).is_none(),
            "…and names no wake for a z it no longer carries"
        );

        t += Duration::from_millis(16);
        let after = pet.tick(sense(t, None));
        assert_eq!(after.alpha, 0, "the body is gone at once, not fading");
        assert!(
            after.motes.iter().all(Option::is_none),
            "the mote lane must be empty, found {:?}",
            after
                .motes
                .iter()
                .flatten()
                .map(|m| m.kind)
                .collect::<Vec<_>>()
        );
        assert!(
            after.departures.iter().all(Option::is_none),
            "and no departing body either"
        );

        pet.retire_unowned();
        assert!(!pet.needs_frames(), "retirement is idempotent");
        t += Duration::from_millis(16);
        let still = pet.tick(sense(t, None));
        assert_eq!(still.alpha, 0);
        assert!(still.motes.iter().all(Option::is_none));
    }

    #[test]
    fn the_first_step_out_of_a_seat_faces_the_gap() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 20);
        let (mut t, f) = idle(&mut pet, t, (4, 20), SIT_AFTER + 0.2);
        assert!(
            f.facing_left && f.action.settled(),
            "fixture: seated facing left, got {:?} left={}",
            f.action,
            f.facing_left
        );
        for _ in 0..30 {
            t += Duration::from_millis(16);
            let g = pet.tick(sense(t, Some((4, 22))));
            if g.col > f.col + 1e-3 {
                assert!(!g.facing_left, "the first moving frame faces the gap");
                return;
            }
        }
        panic!("fixture: the seat never moved");
    }

    #[test]
    fn a_hidden_pet_takes_a_look_sync_immediately() {
        let mut pet = PetBrain::default();
        assert!(!pet.is_active(), "a fresh pet is hidden");
        assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (9, 4),
            "hidden: no appearance to protect, the swap is free"
        );
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (9, 4),
            "and the agreeing re-sync is a no-op"
        );
    }

    #[test]
    fn a_bell_wakes_the_sleeper() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, f) = idle(&mut pet, start, (4, 10), 1.0);
        assert_eq!(
            f.action,
            PetAction::Sleep,
            "fixture: an untouched window naps"
        );
        pet.note_bell(t);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 10))));
        assert_eq!(
            f.action,
            PetAction::Waking,
            "the bell takes the EXISTING wake path"
        );
        assert_eq!(f.pose, PetGlyphId::PetStretch, "stretch and all");
        assert!(
            f.motes
                .iter()
                .flatten()
                .any(|m| m.kind == PetMoteKind::ZeePop),
            "with the startled z popped ahead of it"
        );
    }

    #[test]
    fn a_bell_flinches_the_settled_pet() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 10);
        let (mut t, _) = idle(&mut pet, t, (3, 12), 0.5);
        pet.note_bell(t);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((3, 12))));
        assert_eq!(
            f.action,
            PetAction::Perk,
            "a bell is a noise, not a held delete: the FLINCH tier, never the bottle"
        );
        assert!(
            f.scale_y < 1.0 && f.scale_x > 1.0,
            "the duck must be visible: y {} x {}",
            f.scale_y,
            f.scale_x
        );
        assert!(
            f.facing_left,
            "and the fright faces the caret (left of the station)"
        );
    }

    #[test]
    fn a_bell_during_flight_expires_unconsumed() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 30))));
        pet.note_bell(t);
        let mut landed = false;
        let mut acted_after_ground = false;
        for _ in 0..160 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 30))));
            if f.action == PetAction::Land {
                landed = true;
            }
            if landed && matches!(f.action, PetAction::Perk | PetAction::Waking) {
                acted_after_ground = true;
            }
        }
        assert!(landed, "fixture: the jump must land inside the window");
        assert!(
            pet.pending_bell.is_none(),
            "the latch must expire per its TTL"
        );
        assert!(
            !acted_after_ground,
            "a bell from before the leap must not fire after it"
        );
    }

    #[test]
    fn reduced_motion_turns_every_stimulus_into_bookkeeping() {
        type Note = fn(&mut PetBrain, Instant);
        type Feel = fn(&mut PetSense, u32);
        type Check = fn(&PetBrain, f32);
        let quiet: Feel = |_, _| {};
        let nothing: Check = |_, _| {};
        let stimuli: [(&str, bool, Note, Feel, Check); 6] = [
            ("bell", false, |pet, t| pet.note_bell(t), quiet, nothing),
            (
                "verdicts",
                false,
                |pet, t| {
                    let c0 = pet.content();
                    pet.note_command_done(t, false, Some(240_000));
                    let warmed = pet.content();
                    assert!(warmed > c0, "verdicts: the ledger still warms");
                    pet.note_command_done(t, true, Some(1000));
                    assert!(pet.content() < warmed, "verdicts: and still cools");
                },
                quiet,
                nothing,
            ),
            (
                "pet",
                false,
                |pet, t| pet.note_petted(t),
                quiet,
                |pet, c0| assert!(pet.content() > c0, "pet: the warmth still lands"),
            ),
            (
                "peek",
                true,
                |pet, t| pet.note_peek(t, 15.0, 3.0),
                quiet,
                nothing,
            ),
            (
                "pointer",
                true,
                |_, _| {},
                |s, k| s.pointer = Some(((59.0 - k as f32).max(2.0), 3.0)),
                nothing,
            ),
            (
                "stream",
                true,
                |_, _| {},
                |s, _| s.output_burst = true,
                nothing,
            ),
        ];
        for (name, warm_live, note, feel, check) in stimuli {
            let mut pet = PetBrain::default();
            let mut t = Instant::now();
            for _ in 0..40 {
                t += Duration::from_millis(16);
                let mut s = sense(t, Some((3, 10)));
                s.reduced_motion = !warm_live;
                let _ = pet.tick(s);
            }
            let c0 = pet.content();
            note(&mut pet, t);
            for k in 0..120u32 {
                t += Duration::from_millis(16);
                let mut s = sense(t, Some((3, 10)));
                s.reduced_motion = true;
                feel(&mut s, k);
                let f = pet.tick(s);
                assert!(
                    matches!(f.action, PetAction::Sit | PetAction::Sleep),
                    "{name}: no action under reduced motion, got {:?}",
                    f.action
                );
                assert_eq!((f.scale_x, f.scale_y), (1.0, 1.0), "{name}: no squash");
                assert!(f.motes.iter().all(Option::is_none), "{name}: no motes");
            }
            check(&pet, c0);
            assert!(
                pet.pending_bell.is_none() && pet.pending_cheer.is_none() && !pet.pending_sulk,
                "{name}: the bell and verdict latches clear as bookkeeping"
            );
            assert_eq!(pet.pending_pets(), 0, "{name}: the pet latch is spent");
            assert_eq!(pet.pet_hold_t, 0.0, "{name}: no purr hold");
            assert!(pet.pending_bat.is_none(), "{name}: the bat latch clears");
            assert_eq!(pet.pointer_heat, 0.0, "{name}: no gaze");
            assert!(pet.pending_pointer_pounce.is_none(), "{name}: no pounce");
            assert_eq!(pet.watch_heat, 0.0, "{name}: no watch heat left behind");
        }
    }

    #[test]
    fn a_long_success_cheers_and_warms() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 10);
        let (mut t, _) = idle(&mut pet, t, (3, 12), 0.3);
        let c0 = pet.content();
        pet.note_command_done(t, false, Some(2500));
        assert!(
            pet.content() > c0 + CHEER_CONTENT - 0.01,
            "the ledger warms at note time"
        );
        let (mut saw_perk, mut saw_frolic, mut saw_note) = (false, false, false);
        for _ in 0..120 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((3, 12))));
            match f.action {
                PetAction::Perk => saw_perk = true,
                PetAction::Frolic => {
                    saw_frolic = true;
                    if f.motes
                        .iter()
                        .flatten()
                        .any(|m| m.kind == PetMoteKind::Note)
                    {
                        saw_note = true;
                    }
                }
                _ => {}
            }
        }
        assert!(saw_perk, "the notice leads the celebration");
        assert!(saw_frolic, "then the frolic");
        assert!(saw_note, "with the one floating note");
    }

    #[test]
    fn a_failure_droops_and_cools() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 2);
        let (t, _) = type_run(&mut pet, t, 3, 4, 30, 0.05);
        let (mut t, _) = idle(&mut pet, t, (3, 34), 0.6);
        let c0 = pet.content();
        assert!(c0 > 0.3, "fixture: the run must have warmed the cat ({c0})");
        pet.note_command_done(t, true, Some(120_000));
        assert!(
            pet.content() <= c0 - SULK_CONTENT + 1e-5,
            "a failure cools harder than a success warms"
        );
        let mut saw_droop = false;
        for _ in 0..160 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((3, 34))));
            if f.action == PetAction::Droop {
                saw_droop = true;
                assert_eq!(
                    f.pose,
                    PetGlyphId::PetDroop,
                    "the sulk wears its own flat-ears frame — off its feet, \
                     the grief interrupted a stand"
                );
                assert!(f.motes.iter().all(Option::is_none), "grief floats NOTHING");
            }
        }
        assert!(saw_droop, "a failure must droop");
    }

    #[test]
    fn a_fast_command_only_nudges() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 10);
        let (mut t, _) = idle(&mut pet, t, (3, 12), 0.4);
        let c0 = pet.content();
        pet.note_command_done(t, false, Some(300));
        assert!(pet.content() > c0, "the ledger still moves");
        for _ in 0..80 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((3, 12))));
            assert!(
                !matches!(
                    f.action,
                    PetAction::Perk | PetAction::Frolic | PetAction::Droop
                ),
                "a bare `ls` earns no choreography, got {:?}",
                f.action
            );
        }
    }

    #[test]
    fn a_cheer_survives_a_hold() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (t, _) = type_run(&mut pet, start, 2, 10, 8, 0.06);
        let (mut t, _) = idle(&mut pet, t, (2, 20), 0.5);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((2, 15))));
        assert_eq!(f.action, PetAction::Startle, "fixture: the hold is live");
        pet.note_command_done(t, false, Some(5000));
        let mut saw_frolic = false;
        for _ in 0..200 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((2, 15))));
            if f.action == PetAction::Frolic {
                saw_frolic = true;
            }
        }
        assert!(
            saw_frolic,
            "the cheer latch must outlive the startle hold and the walk home"
        );
    }

    fn note_cheer_births(pet: &PetBrain, births: &mut Vec<((u64, u8), PetMoteKind)>) {
        for m in pet.motes.iter().flatten() {
            let key = (m.born.to_bits(), m.seed);
            if matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart)
                && !births.iter().any(|&(k, _)| k == key)
            {
                births.push((key, m.kind));
            }
        }
    }

    #[test]
    fn a_landing_wakes_a_dozing_cat_into_the_big_cheer_and_it_settles_after() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 10);
        let (mut t, f) = idle(&mut pet, t, (3, 12), SLEEP_AFTER + 1.0);
        assert_eq!(
            f.action,
            PetAction::Sleep,
            "fixture: it dozed while the agent worked"
        );
        assert!(!pet.needs_frames(), "fixture: a dozing cat owes no frames");
        let c0 = pet.content();
        pet.note_room_landed();
        assert!(
            pet.content() > c0 + CHEER_CONTENT - 0.01,
            "the ledger warms at note time, as a green build's does"
        );
        assert_eq!(pet.pending_cheer, Some(true), "the BIG cheer is latched");
        let mut beats: Vec<PetAction> = vec![f.action];
        let mut births = Vec::new();
        let end = t + Duration::from_secs(6);
        while t < end {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((3, 12))));
            if beats.last() != Some(&f.action) {
                beats.push(f.action);
            }
            note_cheer_births(&pet, &mut births);
        }
        let at = |a: PetAction| beats.iter().position(|&b| b == a);
        let (Some(waking), Some(perk), Some(frolic)) = (
            at(PetAction::Waking),
            at(PetAction::Perk),
            at(PetAction::Frolic),
        ) else {
            panic!("the stretch, the perk and the frolic all play: {beats:?}");
        };
        assert!(
            waking < perk && perk < frolic,
            "a sleeper stretches awake, then perks, then frolics: {beats:?}"
        );
        assert_eq!(births.len(), 3, "three ♪/♥ motes, a build's: {births:?}");
        assert!(
            births.windows(2).all(|w| w[0].1 != w[1].1),
            "♪ and ♥ alternate: {births:?}"
        );
        assert!(
            pet.motes.iter().flatten().all(|m| m.age(pet.clock) >= 1.0),
            "spent"
        );
        assert!(pet.pending_cheer.is_none(), "the cheer is played, not owed");
        assert!(!pet.needs_frames(), "back off the frame train");
        assert!(
            pet.next_change_deadline(t).is_some_and(|d| d > t),
            "and on the settled ladder: its next step named, not the next frame"
        );
    }

    fn note_tiles_meeting(
        f: &PetFrame,
        cell: (u16, u16),
        grid: (u16, u16),
    ) -> (Vec<String>, usize, bool) {
        let tiles: Vec<_> = f
            .motes
            .iter()
            .flatten()
            .filter(|m| matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart) && m.alpha > 0)
            .filter_map(|m| mote_box_px((m.col, m.row), m.scale, cell, grid).map(|b| (m, b)))
            .collect();
        let mut meeting = Vec::new();
        for (i, (ma, a)) in tiles.iter().enumerate() {
            for (mb, b) in &tiles[i + 1..] {
                let (sa, sb) = (i32::from(a.2), i32::from(b.2));
                if a.0 < b.0 + sb && b.0 < a.0 + sa && a.1 < b.1 + sb && b.1 < a.1 + sa {
                    meeting.push(format!(
                        "{:?}@{:.2},{:.2} {a:?} meets {:?}@{:.2},{:.2} {b:?}",
                        ma.kind, ma.col, ma.row, mb.kind, mb.col, mb.row
                    ));
                }
            }
        }
        let grid_w = i32::from(grid.0) * i32::from(cell.0);
        let held = tiles.iter().any(|(m, b)| {
            let cx = (m.col * f32::from(cell.0)).round() as i32;
            let d = i32::from(b.2);
            cx - d / 2 < 0 || cx - d / 2 + d > grid_w
        });
        (meeting, tiles.len(), held)
    }

    #[test]
    fn notes_and_hearts_in_the_air_together_never_overlap() {
        let start = Instant::now();
        let at = |t: Instant, caret: (u16, u16), cell: (u16, u16)| PetSense {
            cell_w: cell.0,
            cell_h: cell.1,
            ..sense(t, Some(caret))
        };
        type Case = (&'static str, u16, usize, fn(&mut PetBrain, Instant));
        let cases: [Case; 9] = [
            ("a big green build", 40, 3, |p, t| {
                p.note_command_done(t, false, Some(CHEER_BIG_MS + 10_000));
            }),
            ("a long green build", 40, 2, |p, t| {
                p.note_command_done(t, false, Some(CHEER_MIN_MS + 2_000));
            }),
            ("a landing", 40, 3, |p, _| p.note_room_landed()),
            ("a typed treat", 40, 2, |p, t| {
                p.note_trick(t, Trick::Treat, true)
            }),
            ("a typed sing", 40, usize::from(SONG_NOTES), |p, t| {
                p.note_trick(t, Trick::Sing, true);
            }),
            ("four pets at once", 40, 4, |p, _| {
                let w = art_cols(p.last_cell_w, p.last_cell_h);
                p.spawn_pet_hearts(4, w);
            }),
            ("a mail ♪ into a cheer", 40, 4, |p, t| {
                p.note_command_done(t, false, Some(CHEER_BIG_MS + 10_000));
            }),
            ("a big cheer at the right edge", 96, 3, |p, _| {
                let w = art_cols(p.last_cell_w, p.last_cell_h);
                p.facing_left = false;
                p.spawn_cheer_motes(true, w, false);
            }),
            ("a big cheer at the left edge", 2, 3, |p, _| {
                let w = art_cols(p.last_cell_w, p.last_cell_h);
                p.facing_left = true;
                p.spawn_cheer_motes(true, w, false);
            }),
        ];
        for cell in [(7, 14), (8, 16), (9, 19), (10, 20), (15, 28), (16, 32)] {
            for (why, col, want, play) in cases {
                for face_left in [false, true] {
                    let mut pet = PetBrain::default();
                    let caret = (6, col);
                    let mut t = start;
                    for _ in 0..30 {
                        t += Duration::from_millis(16);
                        let _ = pet.tick(at(t, caret, cell));
                    }
                    for k in 1..=2 {
                        t += Duration::from_millis(50);
                        let _ = pet.tick(at(t, (6, col + k), cell));
                    }
                    let end = t + Duration::from_secs_f32(SIT_AFTER + 0.5);
                    while t < end {
                        t += Duration::from_millis(16);
                        let _ = pet.tick(at(t, (6, col + 2), cell));
                    }
                    pet.facing_left = face_left;
                    play(&mut pet, t);
                    let mut most = 0;
                    for k in 0..(6.0 / 0.016) as usize {
                        t += Duration::from_millis(16);
                        if why == "a mail ♪ into a cheer" && k == 70 {
                            let w = art_cols(cell.0, cell.1);
                            pet.mail_beat(w);
                        }
                        let f = pet.tick(at(t, (6, col + 2), cell));
                        let (meeting, n, held) = note_tiles_meeting(&f, cell, (100, 30));
                        assert!(
                            meeting.is_empty(),
                            "{why} at {cell:?} facing left {face_left}, frame {k}: {meeting:?}"
                        );
                        assert!(
                            !held,
                            "{why} at {cell:?} facing left {face_left}, frame {k}: a ♪/♥ \
                             held at the pane's side"
                        );
                        most = most.max(n);
                    }
                    assert!(
                        most >= want,
                        "{why} at {cell:?} facing left {face_left}: {most} ♪/♥ in the air \
                         together, {want} wanted"
                    );
                }
            }
        }
    }

    #[test]
    fn under_reduced_motion_a_landing_moves_the_ledger_and_plays_nothing() {
        let reduced = |t: Instant| PetSense {
            reduced_motion: true,
            ..sense(t, Some((3, 12)))
        };
        let mut pet = PetBrain::default();
        let mut t = Instant::now();
        for _ in 0..30 {
            t += Duration::from_millis(16);
            let _ = pet.tick(reduced(t));
        }
        let c0 = pet.content();
        pet.note_room_landed();
        assert!(
            pet.content() > c0 + CHEER_CONTENT - 0.01,
            "the ledger moves"
        );
        for _ in 0..240 {
            t += Duration::from_millis(16);
            let f = pet.tick(reduced(t));
            assert!(
                !matches!(
                    f.action,
                    PetAction::Waking | PetAction::Perk | PetAction::Frolic
                ),
                "no choreography under reduced motion, got {:?}",
                f.action
            );
            assert!(f.motes.iter().all(Option::is_none), "no motes either");
        }
        assert!(pet.pending_cheer.is_none(), "nothing is owed");
        assert!(!pet.needs_frames(), "and no frames are claimed");
    }

    #[test]
    fn a_pet_on_the_head_makes_a_heart() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = idle(&mut pet, t, (4, 12), 0.4);
        let c0 = pet.content();
        pet.note_petted(t);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 12))));
        assert_eq!(
            f.action,
            PetAction::Purr,
            "affection is answered even by a cold cat: the purr-flavored hold"
        );
        assert!(
            f.motes
                .iter()
                .flatten()
                .any(|m| m.kind == PetMoteKind::Heart),
            "with a heart"
        );
        assert!(pet.content() > c0, "and warmth");
        assert!(pet.pet_hold_t > 0.0, "held, not earned — yet");
    }

    #[test]
    fn a_gentle_body_stroke_earns_a_visible_finite_affection_beat() {
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, Instant::now(), 4, 10);
        let (mut t, initial) = idle(&mut pet, t, (4, 12), 1.0);
        let width = art_cols(10, 20);
        let mut frames = Vec::new();
        let mut held_locations = Vec::new();
        for step in 0..=16 {
            t += Duration::from_millis(50);
            frames.push(ptick(
                &mut pet,
                t,
                (4, 12),
                Some((
                    initial.col + width * (0.20 + 0.60 * step as f32 / 16.0),
                    4.1,
                )),
            ));
            if pet.pet_hold_t > 0.0 {
                let f = frames.last().unwrap();
                held_locations.push((f.col, f.row, f.lift));
            }
        }
        for _ in 0..70 {
            t += Duration::from_millis(16);
            frames.push(pet.tick(sense(t, Some((4, 12)))));
            if pet.pet_hold_t > 0.0 {
                let f = frames.last().unwrap();
                held_locations.push((f.col, f.row, f.lift));
            }
        }
        assert!(frames.iter().any(|f| f.action == PetAction::Purr));
        assert!(
            frames.iter().any(|f| {
                f.motes
                    .iter()
                    .flatten()
                    .any(|m| m.kind == PetMoteKind::Heart)
            }),
            "a stroke gives the same heart as a click"
        );
        assert!(
            held_locations
                .iter()
                .all(|p| *p == held_locations[0] && p.2 == 0.0),
            "affection changes expression, never the pet's location"
        );
        assert_eq!(
            pet.pet_hold_t, 0.0,
            "one stroke buys only the existing finite hold"
        );
        let (_, f) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert_eq!(f.action, PetAction::Sleep);
        assert!(!pet.needs_frames());
        assert!(
            pet.next_change_deadline(t + Duration::from_secs(60))
                .is_none()
        );
    }

    #[test]
    fn a_parked_pointer_over_the_body_never_farms_affection_or_frames() {
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, Instant::now(), 4, 10);
        let pointer = Some((pet.col + art_cols(10, 20) * 0.5, 4.1));
        for _ in 0..360 {
            t += Duration::from_millis(100);
            let f = ptick(&mut pet, t, (4, 12), pointer);
            assert_eq!(pet.pending_pets(), 0);
            assert_eq!(pet.pet_hold_t, 0.0);
            assert!(
                !f.motes
                    .iter()
                    .flatten()
                    .any(|m| m.kind == PetMoteKind::Heart)
            );
        }
        assert!(!pet.needs_frames());
        assert!(
            pet.next_change_deadline(t).is_none(),
            "quiet={} action={:?} offer={:?}",
            pet.quiet,
            pet.action,
            pet.next_change_secs()
        );
    }

    #[test]
    fn stroke_progress_cannot_cross_hidden_reduced_or_work_frames() {
        for interrupt in 0..4 {
            let mut pet = PetBrain::default();
            let t = awake(&mut pet, Instant::now(), 4, 10);
            let (mut t, initial) = idle(&mut pet, t, (4, 12), 1.0);
            let width = art_cols(10, 20);
            for step in 0..=16 {
                t += Duration::from_millis(50);
                let mut s = sense(t, Some((4, 12)));
                s.pointer = Some((
                    initial.col + width * (0.20 + 0.60 * step as f32 / 16.0),
                    4.1,
                ));
                if step == 8 {
                    match interrupt {
                        0 => s.caret = None,
                        1 => s.reduced_motion = true,
                        2 => s.caret = Some((4, 13)),
                        _ => s.output_burst = true,
                    }
                }
                pet.tick(s);
                assert_eq!(pet.pending_pets(), 0, "interrupt={interrupt}, step={step}");
                assert_eq!(pet.pet_hold_t, 0.0, "interrupt={interrupt}, step={step}");
            }
        }
    }

    #[test]
    fn gentle_strokes_can_wake_a_sleeper_but_not_a_reduced_motion_pet() {
        for reduced in [false, true] {
            let mut pet = PetBrain::default();
            let start = Instant::now();
            let (mut t, initial) =
                idle(&mut pet, start, (4, 12), SLEEP_AFTER + BREATH_WINDOW + 2.0);
            assert_eq!(initial.action, PetAction::Sleep);
            let width = art_cols(10, 20);
            let mut woke = false;
            for step in 0..=16 {
                t += Duration::from_millis(50);
                let mut s = sense(t, Some((4, 12)));
                s.reduced_motion = reduced;
                s.pointer = Some((
                    initial.col + width * (0.20 + 0.60 * step as f32 / 16.0),
                    4.1,
                ));
                let f = pet.tick(s);
                woke |= f.action == PetAction::Waking;
            }
            assert_eq!(woke, !reduced);
            if reduced {
                assert_eq!(pet.content(), 0.0, "hover is not a reduced-motion click");
                assert_eq!(pet.pending_pets(), 0);
                assert!(!pet.needs_frames());
            } else {
                let mut answered = false;
                for _ in 0..150 {
                    t += Duration::from_millis(16);
                    answered |= pet.tick(sense(t, Some((4, 12)))).action == PetAction::Purr;
                }
                assert!(answered, "the existing wake stretch hands into affection");
            }
        }
    }

    #[test]
    fn a_pet_during_flight_waits_for_the_ground() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 30))));
        pet.note_petted(t);
        let (mut landed, mut purr_mid_air, mut purr_after_land) = (false, false, false);
        for _ in 0..200 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 30))));
            if f.action == PetAction::Land {
                landed = true;
            }
            if f.action == PetAction::Purr {
                if landed {
                    purr_after_land = true;
                } else {
                    purr_mid_air = true;
                }
            }
        }
        assert!(landed, "fixture: the pounce must land");
        assert!(!purr_mid_air, "a pet mid-flight waits for the ground");
        assert!(
            purr_after_land,
            "and is honored on landing (inside its TTL)"
        );
    }

    #[test]
    fn a_stale_pet_expires() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 2);
        pet.note_petted(t);
        let (t, _) = type_run(&mut pet, t, 3, 4, 50, 0.045);
        assert_eq!(pet.pending_pets(), 0, "the latch expired mid-chase");
        let (_, _f) = idle(&mut pet, t, (3, 54), 0.6);
        assert_eq!(
            pet.pet_hold_t, 0.0,
            "no purr hold fires for a click from before the sprint"
        );
    }

    #[test]
    fn wave_one_latches_arm_and_release_the_frame_lane() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (t, _) = type_run(&mut pet, start, 4, 10, 6, 0.05);
        let (mut t, _) = idle(&mut pet, t, (4, 16), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert!(!pet.needs_frames(), "fixture: deeply asleep, lane released");
        pet.note_petted(t);
        assert!(pet.needs_frames(), "a latched pet re-arms the lane");
        let (t2, _) = idle(&mut pet, t, (4, 16), 6.0);
        t = t2;
        assert_eq!(pet.pending_pets(), 0, "consumed on the ground");
        let (_, f) = idle(&mut pet, t, (4, 16), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert_eq!(f.action, PetAction::Sleep);
        assert!(
            !pet.needs_frames(),
            "every wave-1 envelope is finite: the lane releases again"
        );
    }

    #[test]
    fn a_streaming_pane_perks_the_sitter() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (t, f) = idle(&mut pet, t, (4, 12), SIT_AFTER + 0.4);
        assert!(f.action.settled(), "fixture: a settled cat");
        let (_, f) = stream(&mut pet, t, (4, 12), 0.6);
        assert_eq!(
            f.action,
            PetAction::Perk,
            "the pet turns toward the streaming text and watches"
        );
        assert!(
            !f.facing_left || f32::from(12u16) < f.col,
            "and it faces the output (the caret side)"
        );
    }

    #[test]
    fn typing_echo_never_perks() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = idle(&mut pet, t, (4, 12), SIT_AFTER + 0.4);
        for _ in 0..120 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12))));
            assert_ne!(
                f.action,
                PetAction::Perk,
                "no burst, no watch — the sitter just sits"
            );
        }
        assert_eq!(pet.watch_heat, 0.0, "and no heat accumulates");
    }

    #[test]
    fn the_watch_ends_before_sleep_is_due() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (t, _) = idle(&mut pet, t, (4, 12), SIT_AFTER + 0.2);
        let (t, f) = stream(&mut pet, t, (4, 12), WATCH_MAX + 2.0);
        assert_ne!(
            f.action,
            PetAction::Perk,
            "the hard cap ends the stare — a settled still sticker remains"
        );
        assert!(pet.watch_spent, "the cap latched");
        let (t, _) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert!(
            !pet.needs_frames(),
            "the watch is finite: idle-to-zero survives an endless stream"
        );
        let f1 = pet.tick(sense(t + Duration::from_secs(1), Some((4, 12))));
        let f2 = pet.tick(sense(t + Duration::from_secs(2), Some((4, 12))));
        assert_eq!(f1.fp(), f2.fp(), "and the frame is byte-stable");
    }

    #[test]
    fn a_caret_jump_interrupts_the_watch() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (t, _) = idle(&mut pet, t, (4, 12), SIT_AFTER + 0.3);
        let (mut t, f) = stream(&mut pet, t, (4, 12), 1.0);
        assert_eq!(f.action, PetAction::Perk, "fixture: mid-watch");
        let mut saw_travel = false;
        for k in 0..120 {
            t += Duration::from_millis(16);
            let caret = (4u16, 30u16);
            let mut s = sense(t, Some(caret));
            s.output_burst = k < 30;
            let f = pet.tick(s);
            if matches!(f.action, PetAction::Crouch | PetAction::Leap) {
                saw_travel = true;
            }
        }
        assert!(
            saw_travel,
            "the latched jump must pull the watcher off its perch"
        );
    }

    #[test]
    fn a_nearby_peek_gets_batted() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.3);
        pet.mote_serial = 0;
        pet.note_peek(t, 22.0, 4.0);
        assert!(pet.pending_bat.is_some(), "in range: the bat latches");
        let (mut crouched, mut leapt, mut swiped, mut dusted) = (false, false, false, false);
        for _ in 0..150 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 14))));
            match f.action {
                PetAction::Crouch => crouched = true,
                PetAction::Leap => leapt = true,
                PetAction::Frolic if f.pose == PetGlyphId::PetBat => {
                    swiped = true;
                    if f.motes
                        .iter()
                        .flatten()
                        .any(|m| m.kind == PetMoteKind::Dust)
                    {
                        dusted = true;
                    }
                }
                _ => {}
            }
        }
        assert!(crouched && leapt, "travel to the standoff: crouch, fly");
        assert!(swiped, "the swipe: the bat pose, pinned for the hold");
        assert!(dusted, "with one neutral puff at the peek cell");
        let (_, f) = idle(&mut pet, t, (4, 14), 4.0);
        let w = art_cols(10, 20);
        assert!(
            (f.col - PetBrain::station(14, 100, w)).abs() <= ARRIVED + SIT_FRONT_GAP,
            "home by the caret, got col {}",
            f.col
        );
    }

    #[test]
    fn a_far_peek_is_ignored() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.3);
        pet.note_peek(t, 55.0, 4.0);
        assert!(
            pet.pending_bat.is_none(),
            "a far peek is ignored at note time"
        );
        for _ in 0..80 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 14))));
            assert!(
                !matches!(f.action, PetAction::Crouch | PetAction::Leap),
                "and nothing travels, got {:?}",
                f.action
            );
        }
    }

    #[test]
    fn a_stale_peek_expires() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 2);
        let (t, _) = type_run(&mut pet, t, 3, 4, 6, 0.045);
        pet.note_peek(t, pet.col + 5.0, 3.0);
        assert!(pet.pending_bat.is_some(), "fixture: latched in range");
        let (t, _) = type_run(&mut pet, t, 3, 10, 44, 0.045);
        assert!(
            pet.pending_bat.is_none(),
            "the head pulled back long ago: the latch expired mid-chase"
        );
        let mut t2 = t;
        let mut swiped = false;
        for _ in 0..120 {
            t2 += Duration::from_millis(16);
            let f = pet.tick(sense(t2, Some((3, 54))));
            swiped |= f.action == PetAction::Frolic;
        }
        assert!(!swiped, "no bat fires off a dead latch");
    }

    #[test]
    fn work_outranks_the_bat() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.3);
        pet.note_peek(t, 22.0, 4.0);
        let mut first_land: Option<f32> = None;
        let mut swiped_after_land = false;
        for _ in 0..250 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 36))));
            if f.action == PetAction::Land && first_land.is_none() {
                first_land = Some(f.col);
            }
            if f.action == PetAction::Frolic && f.pose == PetGlyphId::PetBat && first_land.is_some()
            {
                swiped_after_land = true;
            }
        }
        let w = art_cols(10, 20);
        let station = PetBrain::station(36, 100, w);
        let land = first_land.expect("the caret jump must fly");
        assert!(
            (land - station).abs() < 6.0,
            "the FIRST flight serves the caret (landed {land}, station {station}), \
             never the toy"
        );
        assert!(
            swiped_after_land,
            "and the bat still fires after the work, inside its TTL"
        );
    }

    #[test]
    fn a_debounced_park_swaps_in_place_and_a_departing_body_runs_off() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(
            pet.sync_look((3, 1), PetArrival::Ceremony).worn,
            (3, 1),
            "the first dress applies"
        );
        let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (3, 1),
            "fixture: the repoint parks mid-appearance"
        );
        let w = art_cols(10, 20);
        let station = PetBrain::station(50, 100, w);
        let mut ghost_frames = 0u32;
        let mut ghost_first: Option<f32> = None;
        let mut ghost_last = 0.0f32;
        let mut swap_frame_moved = false;
        let mut swapped = false;
        let mut prev_col = None::<f32>;
        let mut last_departures_live = false;
        let mut swap_at: Option<Instant> = None;
        let mut prev_alpha = 255u8;
        let mut ramp_frames = 0u32;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 50))));
            let worn = pet.sync_look((9, 4), PetArrival::Ceremony).worn; // the host's per-frame re-sync
            if swap_at.is_none() && worn == (9, 4) {
                swap_at = Some(t);
                assert!(
                    f.alpha > 0 && f.alpha < 64,
                    "the incoming kitty starts faint — but ON glass — at the swap ({})",
                    f.alpha
                );
            }
            match swap_at {
                None => assert_eq!(f.alpha, 255, "before the swap: fully there"),
                Some(at) if at == t => {}
                Some(at) if t.duration_since(at).as_secs_f32() <= ARRIVE_IN => {
                    assert!(f.alpha >= prev_alpha, "the arrival only ever fades UP");
                    ramp_frames += 1;
                }
                Some(_) => assert_eq!(f.alpha, 255, "after the ramp: fully there"),
            }
            prev_alpha = f.alpha;
            assert!(
                (f.col - station).abs() < 4.0,
                "…and never leaves its station ({} vs {station})",
                f.col
            );
            let live: Vec<_> = f.departures.iter().flatten().collect();
            assert!(live.len() <= 1, "one swap spawns exactly one ghost");
            if let Some(d) = live.first() {
                ghost_frames += 1;
                assert_eq!(
                    (d.coat, d.iris),
                    (3, 1),
                    "the ghost wears the OUTGOING look"
                );
                assert_eq!(
                    worn,
                    (9, 4),
                    "…while the live pet already wears the incoming one"
                );
                if ghost_first.is_none() {
                    ghost_first = Some(d.col);
                    if let Some(p) = prev_col {
                        swap_frame_moved = (f.col - p).abs() > 0.01;
                    }
                }
                ghost_last = d.col;
            }
            swapped |= worn == (9, 4);
            prev_col = Some(f.col);
            last_departures_live = f.departures.iter().any(Option::is_some);
        }
        assert!(swapped, "the debounced park lands the new look");
        assert!(!swap_frame_moved, "the swap never moves the live pet");
        assert!(
            ramp_frames >= (ARRIVE_IN / 0.016) as u32 - 2,
            "the arrival ramp ran its whole length ({ramp_frames} frames)"
        );
        let first = ghost_first.expect("the swap spawned a departing body");
        assert!(
            ghost_last > first + 10.0,
            "the ghost genuinely ran toward its edge ({first} → {ghost_last})"
        );
        assert!(
            ghost_frames >= 30 && ghost_frames <= (DEPART_MAX / 0.016) as u32 + 4,
            "the visit is brief and strictly bounded ({ghost_frames} frames)"
        );
        assert!(
            !last_departures_live,
            "every departing body has fully retired by the end"
        );
    }

    #[test]
    fn the_departing_ghost_leaves_at_full_opacity() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(
            pet.sync_look((3, 1), PetArrival::Ceremony).worn,
            (3, 1),
            "the first dress applies"
        );
        let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (3, 1),
            "fixture: the repoint parks"
        );
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 50))));
            if pet.sync_look((9, 4), PetArrival::Ceremony).worn == (9, 4) {
                let d = f
                    .departures
                    .iter()
                    .flatten()
                    .next()
                    .expect("the swap spawned its ghost on the same tick");
                assert_eq!(
                    f.alpha,
                    (ARRIVE_FLOOR * 255.0) as u8,
                    "the incoming body lands at the arrival floor"
                );
                assert_eq!(f.lane_alpha, 255, "…while the LANE is at full presence");
                let composed = u16::from(d.alpha) * u16::from(f.lane_alpha) / 255;
                assert!(
                    composed >= 250,
                    "the ghost leaves at full opacity through the emitter's \
                     own multiply ({composed})"
                );
                return;
            }
        }
        panic!("the debounced park never landed");
    }

    #[test]
    fn a_quiet_redress_never_flips_the_cat() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(
            pet.sync_look((3, 1), PetArrival::Ceremony).worn,
            (3, 1),
            "the first dress applies"
        );
        let (mut t, f) = idle(&mut pet, t, (4, 30), 2.0);
        assert!(f.facing_left, "fixture: the pet must be facing left");
        assert!(f.action.settled(), "fixture: …and settled");
        let out = pet.sync_look((9, 4), PetArrival::Quiet);
        assert_eq!(out.worn, (3, 1), "fixture: the homecoming parks");
        assert!(out.parked, "…and reports the genuine park");
        let mut prev_pose = f.pose;
        let mut prev_facing = f.facing_left;
        let mut worn_pose: Option<PetGlyphId> = None;
        let mut swap_col = 0.0f32;
        let mut swapped = false;
        let mut ghost_frames = 0u32;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 30))));
            let out = pet.sync_look((9, 4), PetArrival::Quiet);
            if !swapped && out.worn == (9, 4) {
                swapped = true;
                swap_col = f.col;
                assert!(prev_facing, "fixture: still facing left at the swap");
                worn_pose = Some(prev_pose);
            }
            if let Some(d) = f.departures.iter().flatten().next() {
                ghost_frames += 1;
                assert!(
                    d.facing_left,
                    "the zero-span ghost holds the live pet's facing — \
                     never the run-derived dir"
                );
                assert_eq!(
                    d.pose,
                    worn_pose.expect("no ghost before the swap"),
                    "…and the cached pose, for its whole fade"
                );
                assert!(
                    (d.col - swap_col).abs() < 1e-3,
                    "…without ever moving ({} vs {swap_col})",
                    d.col
                );
            }
            if !swapped {
                prev_pose = f.pose;
                prev_facing = f.facing_left;
            }
        }
        assert!(swapped, "the debounced park landed");
        let want = (EDGE_FADE / 0.016) as u32;
        assert!(
            ghost_frames >= want - 2 && ghost_frames <= want + 2,
            "the still ghost held for the whole 0.25 s ({ghost_frames} frames)"
        );
    }

    #[test]
    fn a_hidden_caret_fades_the_crossfade_with_the_body_instead_of_cutting_it() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(
            pet.sync_look((3, 1), PetArrival::Ceremony).worn,
            (3, 1),
            "the first dress applies"
        );
        let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Quiet).worn,
            (3, 1),
            "fixture: the homecoming parks"
        );
        let mut born_life = None;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 50))));
            if pet.sync_look((9, 4), PetArrival::Quiet).worn == (9, 4) {
                assert_eq!(f.alpha, (ARRIVE_FLOOR * 255.0) as u8);
                born_life = pet.departures[0].map(|d| (d.born, d.life()));
                break;
            }
        }
        let (born, life) = born_life.expect("the debounced park landed with its ghost");
        let mut prev_lane = 255u8;
        let mut ticks = 0u32;
        loop {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, None));
            ticks += 1;
            if f.lane_alpha == 0 {
                break;
            }
            assert!(f.lane_alpha <= prev_lane, "the lane only fades down");
            prev_lane = f.lane_alpha;
            let age = (pet.lane_clock - born) as f32;
            if age < life - 0.02 {
                let g = f.departures[0].unwrap_or_else(|| {
                    panic!(
                        "tick {ticks}: the ghost (age {age:.3} of {life:.3} s) was dropped while \
                         the body is still on glass at lane {}",
                        f.lane_alpha
                    )
                });
                assert!(g.alpha > 0, "a live ghost has a byte");
            }
            if ticks <= 12 {
                assert!(
                    pet.arrive_t > 0.0 && f.alpha < f.lane_alpha,
                    "tick {ticks}: the arrival ramp was snapped to full ({} of lane {}) under a \
                     fading body",
                    f.alpha,
                    f.lane_alpha
                );
            }
            assert!(ticks < 60, "FADE_OUT is 0.45 s");
        }
        assert!(
            pet.departures.iter().all(Option::is_none) && pet.arrive_t == 0.0,
            "the lane is emptied and the ramp retired once the body is at zero"
        );
    }

    #[test]
    fn a_flight_lands_on_wall_time_through_synchronized_output_holds() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 20);

        pet.begin_arc(40.0, 4.0, 0.45, 1.0, FlightKind::PLAIN);
        let dur = pet.flight.expect("fixture: airborne").dur;

        let mut waited = 0.0f32;
        for _ in 0..40 {
            t += Duration::from_millis(150);
            waited += 0.150;
            let _ = pet.tick(sense(t, Some((4, 20))));
            if pet.flight.is_none() {
                break;
            }
        }

        assert!(
            pet.flight.is_none(),
            "the flight never completed: {waited:.2}s of wall time elapsed \
             against an authored {dur:.2}s duration"
        );
        assert!(
            waited <= dur + 0.16,
            "the arc fell behind wall time: took {waited:.2}s to fly {dur:.2}s \
             (a per-bracket deficit that compounds)"
        );
    }

    #[test]
    fn an_agreeing_re_sync_cannot_downgrade_a_parked_ceremony() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
        let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);

        let out = pet.sync_look((9, 4), PetArrival::Ceremony);
        assert!(out.parked, "fixture: the stranger parks");

        let mut ghost_first: Option<f32> = None;
        let mut ghost_last = 0.0f32;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 50))));
            let _ = pet.sync_look((9, 4), PetArrival::Quiet);
            if let Some(d) = f.departures.iter().flatten().next() {
                ghost_first.get_or_insert(d.col);
                ghost_last = d.col;
            }
        }
        let first = ghost_first.expect("the ceremony spawned a departing body");
        assert!(
            ghost_last > first + 10.0,
            "the ceremony survived the Quiet re-syncs and RAN \
             ({first} -> {ghost_last}); a downgrade leaves a zero-span fade"
        );
    }

    #[test]
    fn a_ghost_reaches_its_door_on_a_wide_pane() {
        let wide = |now: Instant, caret: Option<(u16, u16)>| PetSense {
            caret_drawn: true,
            cols: 200,
            ..sense(now, caret)
        };
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        let _ = pet.tick(wide(t, Some((4, 8))));
        for k in 1..=2u16 {
            let until = t + Duration::from_secs_f32(0.05);
            while t < until {
                t += Duration::from_millis(16);
                let _ = pet.tick(wide(t, Some((4, 8 + k - 1))));
            }
            t = until;
            let _ = pet.tick(wide(t, Some((4, 8 + k))));
        }
        let settle = t + Duration::from_secs_f32(SIT_AFTER + 0.2);
        while t < settle {
            t += Duration::from_millis(16);
            let _ = pet.tick(wide(t, Some((4, 10))));
        }
        assert_eq!(
            pet.sync_look((3, 1), PetArrival::Ceremony).worn,
            (3, 1),
            "the first dress applies"
        );
        let hold = t + Duration::from_secs_f32(0.2);
        while t < hold {
            t += Duration::from_millis(16);
            let _ = pet.tick(wide(t, Some((4, 10))));
        }
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (3, 1),
            "fixture: the repoint parks"
        );
        let w = art_cols(10, 20);
        let door = 200.0 - w;
        let mut first: Option<f32> = None;
        let mut last: Option<f32> = None;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(wide(t, Some((4, 10))));
            let _ = pet.sync_look((9, 4), PetArrival::Ceremony).worn;
            if let Some(d) = f.departures.iter().flatten().next() {
                first.get_or_insert(d.col);
                last = Some(d.col);
            }
        }
        let first = first.expect("the swap spawned a departing body");
        assert!(
            first < 20.0,
            "the ghost was born at the pet's feet by the caret ({first})"
        );
        let last = last.expect("the ghost lived at least one frame");
        assert!(
            (last - door).abs() < 0.5,
            "the ghost's last live frame is AT its door, not a mid-screen \
             dissolve ({last} vs {door})"
        );
    }

    #[test]
    fn one_long_frame_cannot_move_a_ghost_a_screen() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
        let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (3, 1),
            "fixture: the repoint parks"
        );
        let mut before: Option<f32> = None;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 50))));
            let _ = pet.sync_look((9, 4), PetArrival::Ceremony).worn;
            if let Some(d) = f.departures.iter().flatten().next() {
                before = Some(d.col);
                break;
            }
        }
        let before = before.expect("the swap spawned a departing body");
        t += Duration::from_secs(5);
        let f = pet.tick(sense(t, Some((4, 50))));
        let d = f.departures.iter().flatten().next().expect(
            "the ghost survives the stall — the lane aged one clamped \
             frame, not five seconds",
        );
        let moved = d.col - before;
        assert!(moved >= 0.0, "the lane never runs backwards ({moved})");
        assert!(
            moved <= DEPART_SPEED * 0.10 + 1e-3,
            "one long frame moves a ghost at most speed × 0.10 cells ({moved})"
        );
    }

    #[test]
    fn a_round_trip_flip_never_walks() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
        let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
        let _ = pet.sync_look((9, 4), PetArrival::Ceremony).worn;
        let (t2, _) = idle(&mut pet, t, (4, 50), 1.0);
        t = t2;
        let _ = pet.sync_look((3, 1), PetArrival::Ceremony).worn;
        let w = art_cols(10, 20);
        let station = PetBrain::station(50, 100, w);
        for _ in 0..400 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 50))));
            assert!(
                (f.col - station).abs() < 4.0,
                "a round-trip flip must never march the pet off ({} vs {station})",
                f.col
            );
            assert_eq!(f.alpha, 255, "and never fades it");
            assert!(
                f.departures.iter().all(Option::is_none),
                "…and never spawns a departing body"
            );
        }
    }

    #[test]
    fn a_caret_jump_is_served_before_the_swap() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
        let (mut t, _) = idle(&mut pet, t, (4, 12), 0.2);
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (3, 1),
            "fixture: the repoint parks"
        );
        let (t2, _) = idle(&mut pet, t, (4, 12), HANDOFF_DEBOUNCE - 0.3);
        t = t2;
        let mut saw_flight = false;
        let mut ghost_after_flight = false;
        for _ in 0..400 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 90))));
            let worn = pet.sync_look((9, 4), PetArrival::Ceremony).worn;
            if f.action.airborne() {
                saw_flight = true;
                assert_eq!(worn, (3, 1), "no swap mid-air — work first, always");
                assert!(
                    f.departures.iter().all(Option::is_none),
                    "and no ghost spawns mid-air"
                );
            }
            if let Some(d) = f.departures.iter().flatten().next() {
                ghost_after_flight = true;
                assert!(saw_flight, "the ghost is born only after the trip");
                assert_eq!(worn, (9, 4), "the ghost's birth IS the swap");
                assert_eq!((d.coat, d.iris), (3, 1), "it wears the old look");
                assert!(
                    d.col > 50.0,
                    "…and spawned at the pet's NEW station, not the old one ({})",
                    d.col
                );
            }
        }
        assert!(saw_flight, "fixture: the jump really flew the pet");
        assert!(ghost_after_flight, "the handoff fired at the next arrival");
    }

    #[test]
    fn a_restarted_ramp_never_rewinds_an_adopted_stance() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 48);
        assert_eq!(pet.sync_look((3, 1), PetArrival::Quiet).worn, (3, 1));
        let (mut t, _) = idle(&mut pet, t, (4, 50), 0.2);
        pet.adopted_look = Some(AdoptedLook {
            pose: pet.last_pose,
            facing_left: pet.last_facing,
            stance: Stance {
                scale_x: 1.06,
                scale_y: 0.97,
                lift: 0.0,
            },
            eased: 0.0,
            seat: (pet.col, pet.row),
            beneath: None,
        });
        pet.arrive_t = HOMECOMING_IN;
        pet.arrive_in = HOMECOMING_IN;
        pet.arrive_from = 1.0;
        assert!(pet.sync_look((9, 4), PetArrival::Quiet).parked, "fixture");
        pet.handoff_parked_clock = Some(f64::MAX);
        let px = |f: &PetFrame| f.body_px(10, 20, 100, 30).expect("on glass");
        let mut last: Option<(i32, i32, i32, i32)> = None;
        let mut handed_over_worn = false;
        for i in 0..40 {
            if i == 14 {
                pet.handoff_parked_clock = Some(pet.clock - f64::from(HANDOFF_DEBOUNCE));
            }
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 50))));
            if i == 14 {
                assert_eq!(pet.worn, Some((9, 4)), "fixture: the handoff landed");
                handed_over_worn = pet.adopted_look.is_some();
            }
            let now = px(&f);
            if let Some(was) = last {
                let step = [now.0 - was.0, now.1 - was.1, now.2 - was.2, now.3 - was.3];
                assert!(
                    step.iter().all(|d| d.abs() <= 1),
                    "frame {i}: the body snapped {was:?} -> {now:?} (pose {:?})",
                    f.pose
                );
            }
            last = Some(now);
        }
        assert!(
            handed_over_worn,
            "fixture: the look was still worn on the handoff frame"
        );
    }

    #[test]
    fn reduced_motion_swaps_instantly() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((3, 10))));
        }
        assert_eq!(pet.sync_look((3, 1), PetArrival::Ceremony).worn, (3, 1));
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (3, 1),
            "parks while visible"
        );
        t += Duration::from_millis(16);
        let mut s = sense(t, Some((3, 10)));
        s.reduced_motion = true;
        let f = pet.tick(s);
        assert!(
            matches!(f.action, PetAction::Sit | PetAction::Sleep),
            "no walk under reduced motion"
        );
        assert_eq!(
            pet.sync_look((9, 4), PetArrival::Ceremony).worn,
            (9, 4),
            "the parked look applied immediately — theater is motion, \
             the costume is state"
        );
        assert!(
            f.departures.iter().all(Option::is_none),
            "no departing body under reduced motion — departures are theater"
        );
    }

    #[test]
    fn departures_cap_at_the_documented_bound_and_drop_the_oldest() {
        let mut pet = PetBrain::default();
        for k in 0..4u8 {
            pet.lane_clock = f64::from(k) * 0.1;
            pet.spawn_departure(Departure {
                born: pet.lane_clock,
                id: 0,
                coat: k,
                iris: 0,
                from_col: 10.0,
                row: 4.0,
                edge: 90.0,
                speed: DEPART_SPEED,
                still: false,
                facing_left: false,
                stride0: 0.0,
                pose0: PetGlyphId::PetSit,
                stance0: Stance::NEUTRAL,
                resight: false,
                own_glass: None,
                own_clear: false,
                peak: 1.0,
                colors: None,
                surface: None,
            });
        }
        let live: Vec<u8> = pet
            .resolve_departures()
            .iter()
            .flatten()
            .map(|d| d.coat)
            .collect();
        assert_eq!(live.len(), PET_DEPARTURES_MAX, "the cap holds");
        assert!(
            !live.contains(&0) && [1, 2, 3].iter().all(|c| live.contains(c)),
            "the OLDEST ghost was the one dropped ({live:?})"
        );
        pet.lane_clock += f64::from(DEPART_MAX) + 0.1;
        let gone = pet.resolve_departures();
        assert!(gone.iter().all(Option::is_none), "all visits expired");
        assert!(
            pet.departures.iter().all(Option::is_none),
            "…and their slots are freed, not parked"
        );
        assert!(
            !pet.needs_frames(),
            "no lingering animating=true once the ghosts are gone"
        );
    }

    fn warm_settled(pet: &mut PetBrain, start: Instant) -> Instant {
        let t = awake(pet, start, 4, 2);
        let (t, _) = type_run(pet, t, 4, 4, 40, 0.045);
        let (t, _) = idle(pet, t, (4, 44), SIT_AFTER + 0.3);
        assert!(
            pet.content() >= PLAY_CONTENT,
            "fixture: the run must have warmed the cat ({})",
            pet.content()
        );
        t
    }

    #[test]
    fn the_gaze_follows_a_wandering_pointer() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 40);
        let (mut t, f) = idle(&mut pet, t, (4, 42), 0.1);
        assert_eq!(
            f.pose,
            PetGlyphId::PetSitFront,
            "fixture: eye contact at rest"
        );
        assert!(!f.facing_left, "fixture: the escort faces its travel");
        let mut px = 18.0f32;
        let mut frame = f;
        for k in 0..30 {
            t += Duration::from_millis(16);
            px += if k % 10 < 5 { -0.2 } else { 0.2 };
            frame = ptick(&mut pet, t, (4, 42), Some((px, 4.0)));
        }
        assert!(pet.pointer_heat > 0.0, "the wandering built attention");
        assert_eq!(
            frame.pose,
            PetGlyphId::PetPeekShoulder,
            "a far pointer behind the facing earns the over-the-shoulder peek"
        );
        for _ in 0..80 {
            t += Duration::from_millis(16);
            let _ = ptick(&mut pet, t, (4, 42), Some((px, 4.0)));
        }
        assert_eq!(pet.pointer_heat, 0.0, "a parked pointer is furniture");
        assert!(
            pet.sit_front && !pet.peek,
            "the gaze parks back on the caret (eye contact, no peek)"
        );
    }

    const SLEEPER_CARET: (u16, u16) = (4, 12);

    fn a_sleeping_cat(hop_first: bool) -> (PetBrain, Instant, PetFrame) {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let w = art_cols(10, 20);
        let t = awake(&mut pet, start, SLEEPER_CARET.0, SLEEPER_CARET.1);
        let (mut t, mut f) = idle(&mut pet, t, SLEEPER_CARET, SLEEP_AFTER + 1.0);
        assert_eq!(f.action, PetAction::Sleep, "fixture: a sleeping cat");
        assert!(f.facing_left, "fixture: it dozed off facing LEFT");
        if hop_first {
            let inked_to = (f.col + w + 3.0) as u16;
            let spans = vec![(0u16, inked_to); 8];
            pet.sense_ink(0, &spans, Some(SLEEPER_CARET.0));
            for _ in 0..60 {
                t += Duration::from_millis(16);
                f = pet.tick(sense(t, Some(SLEEPER_CARET)));
            }
            assert_eq!(
                f.action,
                PetAction::Sleep,
                "fixture: the hop handed the sleep back"
            );
            assert!(!f.facing_left, "fixture: the hop left it facing RIGHT");
        }
        (pet, t, f)
    }

    fn swing_over_a_sleeper(hop_first: bool) -> (bool, u32, u32, u32) {
        let caret = SLEEPER_CARET;
        let w = art_cols(10, 20);
        let (mut pet, mut t, f) = a_sleeping_cat(hop_first);
        let (mid, row, facing0) = (f.col + w * 0.5, f.row, f.facing_left);
        let (mut flips, mut crossed, mut in_range) = (0u32, 0u32, 0u32);
        let (mut prev, mut side) = (facing0, 0.0f32);
        for i in 0..600u16 {
            let swing = 2.2 * (f32::from(i) * 0.016 * TAU * 1.5).sin();
            t += Duration::from_millis(16);
            let f = ptick(&mut pet, t, caret, Some((mid + swing, row)));
            assert_eq!(
                f.action,
                PetAction::Sleep,
                "fixture: a wandering pointer does not wake it"
            );
            if swing.signum() != side.signum() {
                crossed += 1;
                side = swing;
            }
            if pet.sit_front {
                in_range += 1;
            }
            if f.facing_left != prev {
                flips += 1;
                prev = f.facing_left;
            }
        }
        assert!(
            pet.pointer_heat > 0.0,
            "fixture: the swinging is fast enough to hold the pet's gaze"
        );
        (facing0, flips, crossed, in_range)
    }

    #[test]
    fn a_cursor_swinging_over_a_sleeping_cat_never_flips_it() {
        for hop_first in [false, true] {
            let (facing0, flips, crossed, in_range) = swing_over_a_sleeper(hop_first);
            assert!(
                crossed >= 20,
                "fixture is inert — the pointer crossed the cat only {crossed} \
                 times (facing_left {facing0})"
            );
            assert!(
                in_range >= 100,
                "fixture never reached eye-contact range ({in_range} frames, \
                 facing_left {facing0}) — the turn branch was never entered"
            );
            assert_eq!(
                flips, 0,
                "the sleeping cat (facing_left {facing0}) turned over {flips} \
                 times chasing the cursor"
            );
        }
    }

    fn hover_beside_a_sleeper(
        pet: &mut PetBrain,
        start: Instant,
        at: &PetFrame,
        off: f32,
        secs: f32,
    ) -> Option<u16> {
        let w = art_cols(10, 20);
        let px = at.col + w * 0.5 - 0.5 + off;
        let (mut t, mut turned) = (start, None);
        let frames = (secs / 0.016) as u16;
        for i in 0..frames {
            let py = at.row + 0.8 * (f32::from(i) * 0.016 * TAU * 2.0).sin();
            t += Duration::from_millis(16);
            let g = ptick(pet, t, SLEEPER_CARET, Some((px, py)));
            assert_eq!(g.action, PetAction::Sleep, "fixture: it stays asleep");
            if g.facing_left != at.facing_left && turned.is_none() {
                turned = Some(i);
            }
        }
        assert!(pet.pointer_heat > 0.0, "fixture: the pointer stayed live");
        turned
    }

    #[test]
    fn a_cursor_resting_on_a_sleeping_cat_is_not_a_side_to_turn_to() {
        let (mut pet, t, f) = a_sleeping_cat(true);
        assert!(!f.facing_left, "fixture: asleep facing right");
        assert_eq!(
            hover_beside_a_sleeper(&mut pet, t, &f, -0.6, 9.6),
            None,
            "a cursor resting on the cat turned it over"
        );
    }

    #[test]
    fn a_light_sleeper_comes_round_slowly_and_a_deep_one_not_at_all() {
        let (mut pet, t, f) = a_sleeping_cat(true);
        assert!(!f.facing_left, "fixture: asleep facing right");
        let turned = hover_beside_a_sleeper(&mut pet, t, &f, -1.6, 3.0)
            .expect("a light sleeper still notices what stays put");
        assert!(
            f32::from(turned) * 0.016 >= SLEEP_FLIP_DWELL,
            "it came round on frame {turned} — before the dwell was even paid"
        );

        let (mut pet, t, _) = a_sleeping_cat(true);
        let (t, f) = idle(&mut pet, t, SLEEPER_CARET, BREATH_WINDOW + 1.0);
        assert_eq!(f.action, PetAction::Sleep, "fixture: still asleep");
        assert!(
            pet.quiet >= SLEEP_AFTER + BREATH_WINDOW,
            "fixture: past the breath window (quiet {})",
            pet.quiet
        );
        assert!(!f.facing_left, "fixture: still facing right");
        assert_eq!(
            hover_beside_a_sleeper(&mut pet, t, &f, -1.6, 3.0),
            None,
            "a deeply asleep cat is perfectly still — it does not turn over"
        );
    }

    #[test]
    fn the_gaze_brakes_never_reach_the_waking_chase() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 60);
        let (mut t, f) = idle(&mut pet, t, (4, 60), SLEEP_AFTER + 1.0);
        assert_eq!(f.action, PetAction::Sleep, "fixture: a sleeping cat");
        let asleep_facing = f.facing_left;
        let caret = if asleep_facing { 95 } else { 10 };
        let mut turned = None;
        for i in 0..180u16 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, caret))));
            if f.facing_left != asleep_facing && turned.is_none() {
                turned = Some((i, f.action));
            }
        }
        let (frame, action) = turned.expect("it must turn to chase a caret behind it");
        assert!(
            matches!(action, PetAction::Leap | PetAction::Crouch) || !action.settled(),
            "the turn came from travel, not from the settled gaze (got {action:?})"
        );
        assert!(
            frame < 120,
            "and it turned while getting under way, not {frame} frames later"
        );
    }

    fn dash(
        pet: &mut PetBrain,
        start: Instant,
        caret: (u16, u16),
        from: f32,
        to: f32,
    ) -> (Instant, bool, bool) {
        let mut t = start;
        let mut px = from;
        let (mut leapt, mut frolicked) = (false, false);
        while px > to {
            t += Duration::from_millis(16);
            px -= 1.0;
            let f = ptick(pet, t, caret, Some((px, f32::from(caret.0))));
            leapt |= f.action == PetAction::Leap;
            frolicked |= f.action == PetAction::Frolic;
        }
        (t, leapt, frolicked)
    }

    #[test]
    fn a_dashing_pointer_gets_pounced_only_when_content() {
        let start = Instant::now();
        let mut cold = PetBrain::default();
        let t = awake(&mut cold, start, 4, 12);
        let (t, _) = idle(&mut cold, t, (4, 14), SIT_AFTER + 0.4);
        assert!(cold.content() < PLAY_CONTENT, "fixture: a cold cat");
        let (_, leapt, frolicked) = dash(&mut cold, t, (4, 14), 90.0, 5.0);
        assert!(!leapt && !frolicked, "a cold cat does not play");
        let mut warm = PetBrain::default();
        let t = warm_settled(&mut warm, start);
        let from = warm.col + 45.0;
        let (t, leapt, _) = dash(&mut warm, t, (4, 44), from, 5.0);
        assert!(leapt, "a happy cat pounces the dasher");
        let mut frolicked = false;
        let mut t2 = t;
        for _ in 0..200 {
            t2 += Duration::from_millis(16);
            let f = ptick(&mut warm, t2, (4, 44), Some((5.0, 4.0)));
            frolicked |= f.action == PetAction::Frolic;
        }
        assert!(frolicked, "and frolics one beat on the landing");
    }

    #[test]
    fn the_pet_frolics_once_when_the_hand_finds_its_flow() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 20);
        let (mut t, f) = idle(&mut pet, t, (4, 22), SIT_AFTER + 0.2);
        assert!(f.action.settled(), "fixture: a settled, awake cat");
        assert!(
            f.motes.iter().all(Option::is_none),
            "fixture: no light on glass"
        );

        pet.note_flow(t);
        let (mut frolics, mut lit) = (0u32, 0u32);
        for _ in 0..40u16 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 22))));
            if f.action == PetAction::Frolic {
                frolics += 1;
            }
            lit += f.motes.iter().flatten().count() as u32;
        }
        assert!(frolics > 0, "the cat notices the hand finding its flow");
        assert_eq!(lit, 0, "…and it is a POSE, not a light: {lit} motes flew");

        let settled = idle(&mut pet, t, (4, 22), FROLIC_HOLD + 1.0).0;
        let mut again = 0u32;
        let mut t2 = settled;
        for _ in 0..120u16 {
            t2 += Duration::from_millis(16);
            if pet.tick(sense(t2, Some((4, 22)))).action == PetAction::Frolic {
                again += 1;
            }
        }
        assert_eq!(again, 0, "one entry is one frolic, not a standing state");

        assert!(
            pet.pending_flow.is_none(),
            "the entry latch is spent, and no exit ever sets one"
        );

        let (mut pet, t, f) = a_sleeping_cat(false);
        assert_eq!(f.action, PetAction::Sleep, "fixture: asleep");
        let (mut t, _) = idle(&mut pet, t, SLEEPER_CARET, 0.2);
        pet.note_flow(t);
        for _ in 0..60u16 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some(SLEEPER_CARET)));
            assert_eq!(
                f.action,
                PetAction::Sleep,
                "flow never wakes a sleeping cat"
            );
        }

        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 20);
        let (t, _) = idle(&mut pet, t, (4, 22), 0.2);
        pet.note_flow(t);
        assert!(pet.pending_flow.is_some(), "fixture: the latch is live");
        assert!(
            !pet.needs_frames(),
            "a flow latch buys NO frame — it rides the cadence or expires"
        );
        let (t, _) = idle(&mut pet, t, (4, 22), FLOW_LATCH_TTL + 0.2);
        assert!(pet.pending_flow.is_none(), "an unconsumed latch expires");
        let (_, f) = idle(&mut pet, t, (4, 22), SLEEP_AFTER + BREATH_WINDOW + 1.0);
        assert_eq!(f.action, PetAction::Sleep, "and the cat still settles");
        assert!(
            f.motes.iter().all(Option::is_none) && !pet.needs_frames(),
            "…to exactly zero light, asking for no further frame"
        );
    }

    #[test]
    fn the_cat_walks_home_after_playing() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = warm_settled(&mut pet, start);
        let from = pet.col + 45.0;
        let (t, leapt, _) = dash(&mut pet, t, (4, 44), from, 20.0);
        assert!(leapt, "fixture: the pounce fired");
        let (t, f) = idle(&mut pet, t, (4, 44), 3.5);
        let w = art_cols(10, 20);
        let station = PetBrain::station(44, 100, w);
        assert!(
            (f.col - station).abs() <= ARRIVED + SIT_FRONT_GAP,
            "home by the caret again, got col {} (station {station})",
            f.col
        );
        assert!(f.action.settled(), "and settled, got {:?}", f.action);
        let mut t = t;
        let mut acted = false;
        for _ in 0..600 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 44))));
            acted |= matches!(f.action, PetAction::Frolic | PetAction::Loaf) || pet.wriggle_t > 0.0;
        }
        assert!(acted, "a bored content cat demands attention");
        assert!(pet.bored_cool > 0.0, "and pays the cooldown for it");
    }

    #[test]
    fn caret_work_outranks_pointer_play() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = warm_settled(&mut pet, start);
        pet.pending_pointer_pounce = Some((60.0, 4.0));
        t += Duration::from_millis(16);
        let f = ptick(&mut pet, t, (4, 44), Some((60.0, 4.0)));
        assert_eq!(
            f.action,
            PetAction::Crouch,
            "fixture: the play crouch began"
        );
        let mut frolicked = false;
        for _ in 0..250 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 70))));
            frolicked |= f.action == PetAction::Frolic;
        }
        assert!(
            !frolicked,
            "the caret jump cleared the flourish: work first, no play beat"
        );
        let (_, f) = idle(&mut pet, t, (4, 70), 1.0);
        let w = art_cols(10, 20);
        let station = PetBrain::station(70, 100, w);
        assert!(
            (f.col - station).abs() <= ARRIVED + 0.5,
            "and the pet is at the CARET's station, got {} ({station})",
            f.col
        );
    }

    #[test]
    fn one_pounce_per_dash() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = warm_settled(&mut pet, start);
        let mut px = pet.col + 45.0;
        let mut t = t;
        let mut crouches = 0u32;
        let mut last = pet.action();
        for _ in 0..300 {
            t += Duration::from_millis(16);
            px = (px - 1.0).max(2.0);
            let f = ptick(&mut pet, t, (4, 44), Some((px, 4.0)));
            if f.action == PetAction::Crouch && last != PetAction::Crouch {
                crouches += 1;
            }
            last = f.action;
        }
        assert_eq!(
            crouches, 1,
            "one pounce per dash: the trigger re-arms only under POUNCE_REARM"
        );
    }

    #[test]
    fn pointer_none_clears_heat() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.4);
        let mut px = 30.0f32;
        for _ in 0..40 {
            t += Duration::from_millis(16);
            px += 0.2;
            let _ = ptick(&mut pet, t, (4, 14), Some((px, 4.0)));
        }
        assert!(pet.pointer_heat > 0.0, "fixture: attention is live");
        t += Duration::from_millis(16);
        let _ = ptick(&mut pet, t, (4, 14), None);
        assert_eq!(
            pet.pointer_heat, 0.0,
            "outside the pane the pointer does not exist"
        );
        assert!(pet.last_pointer.is_none(), "and the sensor forgets it");
    }

    fn on_ink(f: &PetFrame, span: (u16, u16), w: f32) -> bool {
        let (first, end) = (f32::from(span.0), f32::from(span.1));
        let (a, b) = (f.col + INK_PAD, f.col + w - INK_PAD);
        b > first && a < end
    }

    #[test]
    fn the_sleeper_follows_the_prompt() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let w = art_cols(10, 20);
        let t = awake(&mut pet, start, 2, 8);
        let (mut t, f) = idle(&mut pet, t, (2, 10), SLEEP_AFTER + 1.0);
        assert_eq!(f.action, PetAction::Sleep, "fixture: a sleeping cat");
        let inked_to = (f.col + w + 2.0) as u16;
        let spans = vec![(0u16, inked_to); 6];
        pet.sense_ink(0, &spans, Some(2));
        t += Duration::from_millis(16);
        let f1 = pet.tick(sense(t, Some((2, inked_to))));
        assert_eq!(
            pet.resume.map(|(a, _)| a),
            Some(PetAction::Waking),
            "typing wakes it, and the hop owes the stretch back"
        );
        assert_eq!(
            f1.action,
            PetAction::Leap,
            "and it is already off the words"
        );
        let mut stretched_clear = false;
        for _ in 0..90 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((2, inked_to))));
            if f.action == PetAction::Waking {
                assert!(
                    !on_ink(&f, (0, inked_to), w),
                    "the stretch plays OFF the ink, not atop the fresh words \
                     (col {}, ink to {inked_to})",
                    f.col
                );
                stretched_clear = true;
            }
        }
        assert!(stretched_clear, "the borrowed stretch must actually play");
    }

    #[test]
    fn a_deep_sleep_re_anchor_hops_then_gives_the_lane_back() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let w = art_cols(10, 20);
        let t = awake(&mut pet, start, 3, 10);
        let (mut t, f) = idle(&mut pet, t, (3, 12), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert_eq!(f.action, PetAction::Sleep);
        assert!(!pet.needs_frames(), "fixture: deep sleep, lane released");
        let inked_to = (f.col + w + 4.0) as u16;
        let spans = vec![(0u16, inked_to); 6];
        pet.sense_ink(0, &spans, Some(3));
        t += Duration::from_millis(16);
        let f2 = pet.tick(sense(t, Some((3, 12))));
        assert_eq!(
            f2.action,
            PetAction::Leap,
            "a shoved cat gets up, got {:?}",
            f2.action
        );
        assert!(pet.needs_frames(), "and the hop is owed frames to draw it");
        let (_, f3) = idle(&mut pet, t, (3, 12), 1.5);
        assert_eq!(f3.action, PetAction::Sleep, "asleep again");
        assert!(
            !on_ink(&f3, (0, inked_to), w),
            "re-anchored off the ink (col {})",
            f3.col
        );
        assert!(
            !pet.needs_frames(),
            "and the lane is released again once the paws are down"
        );
    }

    #[test]
    fn a_growing_line_never_tows_the_cat_along_behind_it() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 12);
        let mut spans = vec![(0u16, 0u16); 8];
        spans[4] = (0, 30);
        pet.sense_ink(0, &spans, Some(4));
        let (t2, settled) = idle(&mut pet, t, (4, 12), 1.2);
        t = t2;
        assert!(
            settled.col < 20.0,
            "the cat stations beside the CARET, not past the end of the line \
             (col {}, line ends at 30)",
            settled.col
        );
        assert!(
            (settled.row - 5.0).abs() < 0.01,
            "and it takes the blank row under the sentence, got row {}",
            settled.row
        );

        let anchor = settled.col;
        for end in [31u16, 32, 33] {
            spans[4] = (0, end);
            pet.sense_ink(0, &spans, Some(4));
            let (t3, f) = idle(&mut pet, t, (4, 12), 0.5);
            t = t3;
            assert!(
                (f.col - anchor).abs() < 0.5,
                "a longer line must not move a cat whose caret stood still \
                 (col {} vs {anchor} when the line reached {end})",
                f.col
            );
        }
    }

    #[test]
    fn the_ink_ladder_prefers_a_blank_row_to_a_distant_stand() {
        let mut pet = PetBrain::default();
        let w = art_cols(10, 20);
        let mut spans = vec![(0u16, 0u16); 8];
        spans[4] = (0, 30);
        pet.sense_ink(0, &spans, Some(4));
        assert_eq!(
            pet.ink_stand(13.0, 4.0, w, 100, 30),
            (13.0, 5.0),
            "a blank row below beats a stand 17 cells away"
        );
        spans[4] = (0, 14);
        pet.sense_ink(0, &spans, Some(4));
        assert_eq!(
            pet.ink_stand(13.0, 4.0, w, 100, 30),
            (14.0 + INK_GAP, 4.0),
            "a near stand is cheaper than a row change"
        );
        let spans = vec![(0u16, 30u16); 8];
        pet.sense_ink(0, &spans, Some(4));
        assert_eq!(
            pet.ink_stand(13.0, 4.0, w, 100, 30),
            (30.0 + INK_GAP, 4.0),
            "a full screen still evicts beside the paragraph"
        );
        assert_eq!(
            pet.ink_stand(40.0, 4.0, w, 100, 30),
            (40.0, 4.0),
            "blank ground under the want is answered first and alone"
        );
    }

    #[test]
    fn sparse_ink_refinement_keeps_real_glyphs_and_unknown_coverage_obstructed() {
        use crate::pet_world::{PetPane, PetWorldFacts};
        use aterm_core::terminal::Terminal;

        let mut term = Terminal::new(8, 100);
        term.process(b"\x1b[5;1Hx\x1b[5;100Hx\x1b[5;13H");
        let mut pet = PetBrain::default();
        let width = art_cols(10, 20);
        let spans = vec![(0, 100); 8];
        pet.sense_ink(0, &spans, Some(4));
        assert!(
            pet.ink_overlaps(13.0, 4.0, width),
            "unobserved hull stays conservative"
        );

        let input = term.cell_frame(8, 100);
        let facts = PetWorldFacts::read(&term, 7);
        pet.observe_console(&input, &facts, PetPane::full(&input));
        pet.set_console_presentable(true);
        assert!(
            !pet.ink_overlaps(13.0, 4.0, width),
            "observed spaces are not glyphs"
        );

        term.process(b"\x1b[5;16Hx\x1b[5;13H");
        let input = term.cell_frame(8, 100);
        let facts = PetWorldFacts::read(&term, 7);
        pet.observe_console(&input, &facts, PetPane::full(&input));
        assert!(
            pet.ink_overlaps(13.0, 4.0, width),
            "actual glyph still obstructs the body"
        );
        let beside = pet.ink_safe_col(13.0, 4.0, width, 100);
        assert!((beside - 13.0).abs() <= INK_EVICT_MAX);
        assert!(!pet.ink_overlaps(beside, 4.0, width));

        let mut pane = PetPane::full(&input);
        pane.cols = 8;
        pet.observe_console(&input, &facts, pane);
        assert_eq!(pet.observed_ink_overlaps(13.0, 4.0, width), None);
        assert!(pet.ink_overlaps(13.0, 4.0, width));

        pet.observe_console(&input, &facts, PetPane::full(&input));
        pet.sense_ink(0, &[], None);
        assert!(
            !pet.ink_overlaps(13.0, 4.0, width),
            "unfed ink seam stays inert"
        );
    }

    #[test]
    fn the_exact_ink_map_reads_the_columns_and_feet_row_the_body_covers() {
        use crate::pet_world::{PetPane, PetWorldFacts};
        use aterm_core::terminal::Terminal;

        let width = art_cols(10, 20);
        assert!(
            (13.0 + INK_PAD - 13.3).abs() < 1e-4 && (13.0 + width - INK_PAD - 18.3).abs() < 0.01,
            "the fixture's shaved footprint is 13.3..18.3: width {width}, pad {INK_PAD}"
        );
        let overlaps = |(row, col): (u16, u16), body: f32, feet: f32, presentable: bool| {
            let mut term = Terminal::new(8, 100);
            term.process(format!("\x1b[{};{}Hx\x1b[8;100H", row + 1, col + 1).as_bytes());
            let mut pet = PetBrain::default();
            let input = term.cell_frame(8, 100);
            let facts = PetWorldFacts::read(&term, 7);
            pet.observe_console(&input, &facts, PetPane::full(&input));
            pet.set_console_presentable(presentable);
            pet.observed_ink_overlaps(body, feet, width)
        };
        let cases = [
            ("12 beside 13", (4, 12), 13.0, 4.0, Some(false)),
            ("13 under 13", (4, 13), 13.0, 4.0, Some(true)),
            ("18 under 13", (4, 18), 13.0, 4.0, Some(true)),
            ("19 beside 13", (4, 19), 13.0, 4.0, Some(false)),
            ("12 under 12.8's margin", (4, 12), 12.8, 4.0, Some(false)),
            ("12 under 12.6", (4, 12), 12.6, 4.0, Some(true)),
            ("19 under 13.5's margin", (4, 19), 13.5, 4.0, Some(false)),
            ("19 under 13.8", (4, 19), 13.8, 4.0, Some(true)),
            ("row 4, feet 4.4", (4, 13), 13.0, 4.4, Some(true)),
            ("row 5, feet 4.6", (5, 13), 13.0, 4.6, Some(true)),
            ("row 4, feet 4.6", (4, 13), 13.0, 4.6, Some(false)),
            ("row 0, feet 0", (0, 13), 13.0, 0.0, Some(true)),
        ];
        let got =
            cases.map(|(what, glyph, body, feet, _)| (what, overlaps(glyph, body, feet, true)));
        assert_eq!(
            got,
            cases.map(|(what, .., answer)| (what, answer)),
            "(the glyph's place, whether the body's ink map holds it)"
        );
        assert_eq!(
            overlaps((4, 13), 13.0, 4.0, false),
            None,
            "the same glyph under the same body, the console in another's custody"
        );
    }

    #[test]
    fn an_unobserved_hull_obstructs_exactly_the_footprints_that_reach_it() {
        let width = art_cols(10, 20);
        assert!(
            (width - 2.0 * INK_PAD - 5.0).abs() < 0.01,
            "the fixture's shaved footprint is five columns: width {width}, pad {INK_PAD}"
        );
        let mut pet = PetBrain::default();
        let mut spans = vec![(0, 0); 8];
        spans[4] = (20, 40);
        pet.sense_ink(0, &spans, Some(4));
        let bodies = [13.0f32, 14.6, 14.8, 39.6, 39.8];
        assert_eq!(
            bodies.map(|col| (col, pet.ink_overlaps(col, 4.0, width))),
            [
                (13.0, false),
                (14.6, false),
                (14.8, true),
                (39.6, true),
                (39.8, false)
            ],
            "(the body's column, whether its footprint reaches the hull [20, 40) on row 4)"
        );
    }

    #[test]
    fn the_caret_delta_folds_only_its_three_seams() {
        type Case = (
            &'static str,
            (u16, u16),
            (u16, u16),
            u16,
            bool,
            bool,
            bool,
            (f32, f32, bool),
        );
        #[rustfmt::skip]
        let cases: [Case; 21] = [
            ("typed past the margin", (5, 99), (6, 0), 100, false, true, false, (0.0, 1.0, true)),
            ("backspaced over the seam", (6, 0), (5, 99), 100, false, false, false, (0.0, -1.0, true)),
            ("a double-width wrap", (5, 98), (6, 0), 100, false, true, false, (0.0, 2.0, true)),
            ("a real row jump", (10, 5), (2, 70), 100, false, false, false, (-8.0, 65.0, false)),
            ("down AND right", (5, 2), (6, 70), 100, false, false, false, (1.0, 68.0, false)),
            ("a 15-cell move in a 20-wide pane", (5, 16), (6, 1), 20, false, true, false, (1.0, -15.0, false)),
            ("a scrolled wrap", (29, 99), (29, 0), 100, true, true, false, (1.0, 1.0, true)),
            ("Home at the last column", (29, 99), (29, 0), 100, false, true, false, (0.0, -99.0, false)),
            ("a mid-line Home in a wrapping frame", (29, 40), (29, 0), 100, true, true, false, (0.0, -40.0, false)),
            ("`ls -la` from the last row", (29, 8), (29, 0), 80, false, true, false, (1.0, -8.0, true)),
            ("…width-blind", (29, 8), (29, 0), 200, false, true, false, (1.0, -8.0, true)),
            ("…home at a double-width margin", (29, 13), (29, 2), 80, false, true, false, (1.0, -11.0, true)),
            ("…at the end of a wide prompt", (23, 20), (23, 12), 80, false, true, false, (1.0, -8.0, true)),
            ("…witnessed by the scroll", (29, 30), (29, 24), 100, false, false, true, (1.0, -6.0, true)),
            ("a backspace under STARTLE_COLS", (29, 27), (29, 24), 100, false, false, true, (0.0, -3.0, false)),
            ("…even onto the margin", (29, 3), (29, 0), 80, false, true, false, (0.0, -3.0, false)),
            ("SNAP_COLS and beyond", (29, 20), (29, 0), 80, false, true, false, (0.0, -20.0, false)),
            ("…the scroll does not widen it", (29, 40), (29, 24), 100, false, false, true, (0.0, -16.0, false)),
            ("a retreat that is not home", (29, 40), (29, 8), 80, false, false, false, (0.0, -32.0, false)),
            ("a mid-line deletion, no witness", (23, 20), (23, 12), 80, false, false, false, (0.0, -8.0, false)),
            ("forward is just typing", (29, 24), (29, 30), 100, false, false, true, (0.0, 6.0, false)),
        ];
        for (name, from, to, cols, wrapped, home, scrolled, want) in cases {
            assert_eq!(
                PetBrain::caret_delta(from, to, cols, wrapped, home, scrolled),
                want,
                "{name}"
            );
        }
    }

    #[test]
    fn the_ink_map_scroll_fact_is_only_true_for_a_one_row_scroll() {
        let up = |m: &[(u16, u16)]| {
            let mut v = m.to_vec();
            v.remove(0);
            v.push((0, 12));
            v
        };
        let page = [(0, 0), (0, 40), (0, 44), (0, 30), (0, 24)];

        let mut pet = PetBrain::default();
        pet.sense_ink(0, &page, Some(4));
        assert!(
            !pet.ink_scrolled,
            "the FIRST map has nothing to be a scroll of"
        );
        pet.sense_ink(0, &up(&page), Some(4));
        assert!(pet.ink_scrolled, "every inked row moved up exactly one");

        let mut pet = PetBrain::default();
        pet.sense_ink(0, &page, Some(4));
        pet.sense_ink(0, &page, Some(4));
        assert!(!pet.ink_scrolled, "a map that did not move is not a scroll");

        let mut pet = PetBrain::default();
        pet.sense_ink(0, &page, Some(4));
        pet.sense_ink(0, &up(&up(&page)), Some(4));
        assert!(!pet.ink_scrolled, "TWO rows is a jump, not a line");

        let mut pet = PetBrain::default();
        pet.sense_ink(0, &page, Some(4));
        pet.sense_ink(1, &up(&page), Some(5));
        assert!(
            !pet.ink_scrolled,
            "a moved window is scrollback, not a new line"
        );

        let mut pet = PetBrain::default();
        pet.sense_ink(0, &page, Some(4));
        pet.sense_ink(0, &up(&page)[1..], Some(4));
        assert!(
            !pet.ink_scrolled,
            "a resize rewrites the map, it does not shift it"
        );

        let blank = [(0u16, 0u16); 5];
        let mut pet = PetBrain::default();
        pet.sense_ink(0, &blank, Some(4));
        pet.sense_ink(0, &up(&blank), Some(4));
        assert!(
            !pet.ink_scrolled,
            "a blank pane has no content to have advanced"
        );

        let mut pet = PetBrain::default();
        pet.sense_ink(0, &page, Some(4));
        pet.retire_coordinate_space();
        pet.sense_ink(0, &up(&page), Some(4));
        assert!(!pet.ink_scrolled, "retirement clears the comparison buffer");
    }

    #[test]
    fn a_prompt_row_that_inks_past_the_caret_still_gets_a_quiet_new_line() {
        let start = Instant::now();
        let run = |suggest: bool| -> (bool, bool) {
            let mut pet = PetBrain::default();
            let tail = u16::from(suggest) * 3;
            let mut spans: Vec<(u16, u16)> = vec![(0, 0); 30];
            for span in spans.iter_mut().take(29).skip(21) {
                *span = (0, 46);
            }
            spans[29] = (0, 24 + tail);
            pet.sense_ink(0, &spans, Some(29));
            let (t, _) = type_run(&mut pet, start, 29, 24, 6, 0.06);
            spans[29] = (0, 30);
            pet.sense_ink(0, &spans, Some(29));
            let (mut t, f) = idle(&mut pet, t, (29, 30), SIT_AFTER + 0.3);
            assert!(f.action.settled(), "fixture: a settled cat");
            assert!(
                f.col > 30.0,
                "fixture: standing clear of the line's ink at {:.2}",
                f.col
            );
            for r in 0..29 {
                spans[r] = spans[r + 1];
            }
            spans[29] = (0, 24 + tail);
            let (mut startled, mut ran) = (false, false);
            for _ in 0..120 {
                t += Duration::from_millis(16);
                pet.sense_ink(0, &spans, Some(29));
                let f = pet.tick(sense(t, Some((29, 24))));
                startled |= f.action == PetAction::Startle;
                ran |= f.action == PetAction::Run;
            }
            (startled, ran)
        };
        assert_eq!(
            run(true),
            (false, false),
            "an autosuggestion does not make the shell's own new line a fright"
        );
        assert_eq!(
            run(false),
            (false, false),
            "…and the bare prompt it must stay equal to"
        );
    }

    #[test]
    fn a_wide_prompt_still_leaves_the_caret_at_home() {
        let mut pet = PetBrain::default();
        assert!(pet.caret_at_home(23, 0));
        assert!(pet.caret_at_home(23, 2));
        assert!(!pet.caret_at_home(23, 12));
        pet.sense_ink(20, &[(0, 0), (0, 0), (0, 0), (0, 17)], Some(23));
        assert!(
            pet.caret_at_home(23, 18),
            "the caret sits past the prompt's trailing blank and is still home"
        );
        assert!(pet.caret_at_home(23, 17), "and hard against its last glyph");
        pet.sense_ink(20, &[(0, 0), (0, 0), (0, 0), (0, 60)], Some(23));
        assert!(
            !pet.caret_at_home(23, 18),
            "ink to the right of the caret is a line, not a prompt"
        );
        pet.sense_ink(20, &[(0, 0), (0, 0), (0, 0), (30, 42)], Some(23));
        assert!(!pet.caret_at_home(23, 42));
    }

    #[test]
    fn a_wide_prompt_s_own_new_line_does_not_startle() {
        let start = Instant::now();
        let run = |inked: bool| -> bool {
            let mut pet = PetBrain::default();
            let mut spans = vec![(0u16, 0u16); 30];
            spans[29] = (0, 12);
            if inked {
                pet.sense_ink(0, &spans, Some(29));
            }
            let (t, _) = type_run(&mut pet, start, 29, 12, 8, 0.06);
            spans[29] = (0, 20);
            if inked {
                pet.sense_ink(0, &spans, Some(29));
            }
            let (mut t, f) = idle(&mut pet, t, (29, 20), SIT_AFTER + 0.3);
            assert!(
                f.action.settled(),
                "fixture: a settled cat beside the line's end"
            );
            assert!(
                f.col > 20.0,
                "fixture: and clear of the line's ink, col {}",
                f.col
            );
            spans[29] = (0, 12);
            if inked {
                pet.sense_ink(0, &spans, Some(29));
            }
            let mut startled = false;
            for _ in 0..90 {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((29, 12))));
                startled |= f.action == PetAction::Startle;
            }
            startled
        };
        assert!(
            !run(true),
            "a twelve-column prompt's own new line is a line change, not a fright"
        );
        assert!(
            run(false),
            "fixture: without the ink seam the margin is all there is, so \
             the same Enter DOES still startle — the assertion above is \
             about the seam, not about the delta"
        );
    }

    #[test]
    fn a_scrolled_wrap_is_a_wrap() {
        let script = |fact: bool| -> Vec<PetAction> {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let mut t = awake(&mut pet, start, 29, 90);
            let mut spans = vec![(0u16, 0u16); 30];
            spans[27] = (0, 100);
            spans[28] = (0, 100);
            let mut caret = 92u16;
            while caret < 99 {
                caret += 1;
                for _ in 0..3 {
                    t += Duration::from_millis(16);
                    spans[29] = (0, caret);
                    pet.sense_ink(0, &spans, Some(29));
                    let mut s = sense(t, Some((29, caret)));
                    s.output_burst = true;
                    let _ = pet.tick(s);
                }
            }
            for _ in 0..50 {
                t += Duration::from_millis(16);
                spans[29] = (0, 100);
                pet.sense_ink(0, &spans, Some(29));
                let mut s = sense(t, Some((29, 99)));
                s.output_burst = true;
                let _ = pet.tick(s);
            }
            let mut actions = Vec::new();
            t += Duration::from_millis(16);
            spans[28] = (0, 100);
            spans[29] = (0, 1);
            pet.sense_ink(0, &spans, Some(29));
            let mut s = sense(t, Some((29, 0)));
            s.output_burst = true;
            s.wrapped = fact;
            actions.push(pet.tick(s).action);
            for i in 1..=45u16 {
                t += Duration::from_millis(16);
                let col = (i / 3).clamp(1, 15);
                spans[29] = (0, col);
                pet.sense_ink(0, &spans, Some(29));
                let mut s = sense(t, Some((29, col)));
                s.output_burst = true;
                actions.push(pet.tick(s).action);
            }
            actions
        };

        let with_fact = script(true);
        assert!(
            !with_fact.contains(&PetAction::Startle),
            "the fact folds the seam — no bottle on a scrolled wrap, got \
             {with_fact:?}"
        );
        assert!(
            !with_fact.contains(&PetAction::Crouch) && !with_fact.contains(&PetAction::Leap),
            "and no big-jump show: the watch keeps the ground and the fold \
             keeps the latch, got {with_fact:?}"
        );
        let blind = script(false);
        assert!(
            !blind.contains(&PetAction::Startle),
            "the snap tier catches what the fact does not see: a same-row \
             retreat of a third of the line is a jump, never a fright, got \
             {blind:?}"
        );
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 29, 60);
        let mut bottled = false;
        for _ in 0..30 {
            t += Duration::from_millis(16);
            bottled |= pet.tick(sense(t, Some((29, 54)))).action == PetAction::Startle;
        }
        assert!(
            bottled,
            "control: the harness must still be able to see a bottle, or \
             the assertions above assert nothing"
        );
    }

    struct Law {
        pet: PetBrain,
        t: Instant,
        last: (f32, f32, u16, u16),
        cols: u16,
        rows: u16,
        spans: Vec<(u16, u16)>,
        live: Option<u16>,
        launches: Vec<Flight>,
        actions: Vec<PetAction>,
        frames: Vec<PetFrame>,
    }

    impl Law {
        fn new(cols: u16, rows: u16, caret: (u16, u16)) -> Self {
            let t = Instant::now();
            let mut pet = PetBrain::default();
            let mut s = sense(t, Some(caret));
            s.cols = cols;
            s.rows = rows;
            let f = pet.tick(s);
            Self {
                pet,
                t,
                last: (f.col, f.row, cols, rows),
                cols,
                rows,
                spans: vec![(0, 0); usize::from(rows)],
                live: None,
                launches: Vec::new(),
                actions: Vec::new(),
                frames: Vec::new(),
            }
        }

        fn ink(&mut self, row: u16, first: u16, end: u16) {
            self.spans[usize::from(row)] = (first, end);
        }

        fn frame(&mut self, caret: Option<(u16, u16)>) -> PetFrame {
            self.t += Duration::from_millis(16);
            self.pet.sense_ink(0, &self.spans, self.live);
            let mut s = sense(self.t, caret);
            s.cols = self.cols;
            s.rows = self.rows;
            let pre = self.pet.flight;
            let f = lawful_tick(&mut self.pet, s, &mut self.last);
            if pre.is_none()
                && let Some(fl) = self.pet.flight
            {
                self.launches.push(fl);
            }
            self.actions.push(f.action);
            self.frames.push(f);
            f
        }

        fn now(&self) -> usize {
            self.actions.len()
        }

        fn onsets(&self, from: usize, a: PetAction) -> usize {
            let prev = if from == 0 {
                None
            } else {
                self.actions.get(from - 1).copied()
            };
            let mut was = prev == Some(a);
            let mut n = 0;
            for &x in &self.actions[from.min(self.actions.len())..] {
                if x == a && !was {
                    n += 1;
                }
                was = x == a;
            }
            n
        }

        fn episodes(&self, from: usize) -> usize {
            self.onsets(from, PetAction::Leap)
        }

        fn touchdown(&self, from: usize) -> Option<usize> {
            if self.pet.flight.is_some() {
                return None;
            }
            let mut last = None;
            for i in from.max(1)..self.actions.len() {
                if self.actions[i - 1] == PetAction::Leap && self.actions[i] != PetAction::Leap {
                    last = Some(i);
                }
            }
            last
        }

        fn dust(&self, from: usize) -> bool {
            self.frames[from.min(self.frames.len())..].iter().any(|f| {
                f.motes
                    .iter()
                    .flatten()
                    .any(|m| m.kind == PetMoteKind::Dust)
            })
        }

        fn wore(&self, from: usize, pose: PetGlyphId) -> bool {
            self.frames[from.min(self.frames.len())..]
                .iter()
                .any(|f| f.pose == pose)
        }

        fn hold(&mut self, caret: Option<(u16, u16)>, frames: u32) {
            for _ in 0..frames {
                let _ = self.frame(caret);
            }
        }

        fn type_to(&mut self, row: u16, from: u16, to: u16, frames_per_key: u32) {
            for c in (from + 1)..=to {
                self.ink(row, 0, c);
                self.hold(Some((row, c)), frames_per_key);
            }
        }

        fn delete_to(&mut self, row: u16, from: u16, to: u16, frames_per_key: u32) {
            for c in (to..from).rev() {
                self.ink(row, 0, c);
                self.hold(Some((row, c)), frames_per_key);
            }
        }

        fn warm(cols: u16, rows: u16, row: u16, col: u16) -> Self {
            let start = col.saturating_sub(16);
            let mid = col.saturating_sub(8);
            let mut w = Self::new(cols, rows, (row, start));
            w.live = Some(row);
            w.ink(row, 0, start);
            w.type_to(row, start, mid, 6);
            w.hold(Some((row, mid)), 150);
            w.type_to(row, mid, col, 6);
            w
        }
    }

    #[test]
    fn no_flight_may_outrun_the_animal_that_flies_it() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        struct Judge {
            in_flight: bool,
            launches: u32,
            fastest: f32,
            aimed: Option<(f32, f32)>,
            fastest_reaim: f32,
        }
        impl Judge {
            fn new() -> Self {
                Judge {
                    in_flight: false,
                    launches: 0,
                    fastest: 0.0,
                    aimed: None,
                    fastest_reaim: 0.0,
                }
            }
            fn judge(&mut self, pet: &PetBrain) {
                match pet.flight {
                    Some(f) => {
                        let rate = span_eq(f.to_col - f.from_col, f.to_row - f.from_row) / f.dur;
                        let bound = if f.big {
                            BIG_HOP_SPEED_MAX
                        } else {
                            HOP_SPEED_MAX
                        };
                        assert!(
                            rate <= bound + 1e-3,
                            "a launched arc outruns the animal that flies it: \
                             {rate:.1} c/s against {bound:.0} (big={})",
                            f.big
                        );
                        if let Some((was_to, was_col)) = self.aimed
                            && (f.to_col - was_to).abs() > 1e-3
                        {
                            let pace = (f.to_col - was_col).abs() / (f.dur - f.t).max(1e-3);
                            assert!(
                                pace <= bound + 1e-3,
                                "a mid-air re-aim outruns the flight it \
                                 rebased: {pace:.1} c/s against {bound:.0} \
                                 (big={})",
                                f.big
                            );
                            self.fastest_reaim = self.fastest_reaim.max(pace);
                        }
                        self.aimed = Some((f.to_col, pet.col));
                        if !self.in_flight {
                            self.launches += 1;
                            self.fastest = self.fastest.max(rate);
                        }
                        self.in_flight = true;
                    }
                    None => {
                        self.in_flight = false;
                        self.aimed = None;
                    }
                }
            }
        }
        let wide = |now: Instant, caret: Option<(u16, u16)>| PetSense {
            caret_drawn: true,
            cols: 200,
            ..sense(now, caret)
        };
        fn awake_wide(pet: &mut PetBrain, start: Instant, row: u16, col: u16) -> Instant {
            let wide = |now: Instant, caret: (u16, u16)| PetSense {
                caret_drawn: true,
                cols: 200,
                ..sense(now, Some(caret))
            };
            let mut t = start;
            let _ = pet.tick(wide(t, (row, col)));
            for k in 1..=2u16 {
                let until = t + Duration::from_secs_f32(0.05);
                while t < until {
                    t += Duration::from_millis(16);
                    let _ = pet.tick(wide(t, (row, col + k - 1)));
                }
                t = until;
                let _ = pet.tick(wide(t, (row, col + k)));
            }
            let settle = t + Duration::from_secs_f32(SIT_AFTER + 0.4);
            while t < settle {
                t += Duration::from_millis(16);
                let _ = pet.tick(wide(t, (row, col + 2)));
            }
            t
        }
        let start = Instant::now();

        let mut pet = PetBrain::default();
        let mut law = Judge::new();
        let mut t = awake(&mut pet, start, 6, 95);
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((6, 55))));
        law.judge(&pet);
        let end = t + Duration::from_secs_f32(3.0);
        let mut last = 0.0f32;
        while t < end {
            t += Duration::from_millis(16);
            last = pet.tick(sense(t, Some((6, 55)))).col;
            law.judge(&pet);
        }
        assert_eq!(
            law.launches, 1,
            "the wall crossing is ONE flight now, not a capped transit plus \
             the remainder"
        );
        assert!(
            law.fastest <= quiet_speed_max(100) + 1e-3,
            "and it is paced (fastest {:.1})",
            law.fastest
        );
        let w = art_cols(10, 20);
        let station = PetBrain::station(55, 100, w);
        assert!(
            (last - station).abs() < 3.0,
            "and the cat arrives on the caret's far side (col {last}, \
             station {station})"
        );

        let mut pet = PetBrain::default();
        let mut law = Judge::new();
        let mut t = awake_wide(&mut pet, start, 6, 8);
        t += Duration::from_millis(16);
        let _ = pet.tick(wide(t, Some((6, 50))));
        law.judge(&pet);
        let end = t + Duration::from_secs_f32(4.0);
        let mut final_col = 0.0f32;
        while t < end {
            t += Duration::from_millis(16);
            final_col = pet.tick(wide(t, Some((6, 50)))).col;
            law.judge(&pet);
        }
        assert_eq!(
            law.launches, 1,
            "fixture: the gap closes in ONE flight, not a jog and a pounce"
        );
        assert!(
            law.fastest > HOP_SPEED_MAX && law.fastest <= quiet_speed_max(200) + 1e-3,
            "and it is a bound, paced against the pane (fastest {:.1})",
            law.fastest
        );
        assert!(
            (final_col - 51.0).abs() < 2.5,
            "and the cat still arrives, got col {final_col}"
        );

        let mut pet = PetBrain::default();
        let mut law = Judge::new();
        let mut t = awake_wide(&mut pet, start, 6, 4);
        t += Duration::from_millis(16);
        let _ = pet.tick(wide(t, Some((6, 199))));
        law.judge(&pet);
        let end = t + Duration::from_secs_f32(4.0);
        while t < end {
            t += Duration::from_millis(16);
            let _ = pet.tick(wide(t, Some((6, 199))));
            law.judge(&pet);
        }
        assert!(
            law.launches >= 1
                && law.fastest > HOP_SPEED_MAX
                && law.fastest <= BIG_HOP_SPEED_MAX + 1e-3,
            "fixture: the crossing must fly at a bound's pace — outrunning \
             any ordinary hop, yet obeying the bound's own bar ({} \
             launches, fastest {:.1})",
            law.launches,
            law.fastest
        );

        let scripts: [(&str, fn()); 5] = [
            ("(a) the ink re-anchor at 200 columns", || {
                let mut w = Law::warm(200, 30, 4, 190);
                w.hold(Some((4, 190)), 150);
                for r in 0..30 {
                    w.ink(r, 8, 200);
                }
                w.hold(Some((4, 190)), 200);
            }),
            ("(d) a sixty-cell jump during the wake stretch", || {
                let mut w = Law::new(200, 30, (3, 5));
                w.hold(Some((3, 5)), 2);
                w.hold(Some((3, 6)), 12);
                w.hold(Some((3, 65)), 200);
            }),
            ("(e) a held backspace from the wall side", || {
                let mut w = Law::warm(100, 30, 5, 99);
                w.delete_to(5, 99, 60, 2);
                w.hold(Some((5, 60)), 200);
            }),
            ("(g) the wall-side Enter that arrives mid row-hop", || {
                let mut w = Law::warm(80, 24, 6, 78);
                w.ink(7, 0, 2);
                w.live = Some(7);
                w.hold(Some((7, 2)), 200);
            }),
            ("(h) a caret that snaps home mid-SHOW", || {
                let mut w = Law::warm(80, 24, 6, 18);
                w.ink(6, 0, 0);
                w.hold(Some((6, 18)), 150);
                w.hold(Some((6, 74)), 56);
                assert!(
                    w.pet.flight.is_some_and(|f| f.show && f.t > 0.25 * f.dur),
                    "fixture: the show's bound is in the air"
                );
                w.hold(Some((6, 2)), 200);
            }),
        ];
        let failures: Vec<String> = scripts
            .iter()
            .filter_map(|(name, script)| {
                catch_unwind(AssertUnwindSafe(script)).err().map(|e| {
                    let msg = e
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| e.downcast_ref::<&str>().map(ToString::to_string))
                        .unwrap_or_else(|| "(no message)".to_string());
                    format!("{name}: {msg}")
                })
            })
            .collect();
        assert!(
            failures.is_empty(),
            "{} of 5 scripts broke the launch-time law:\n  {}",
            failures.len(),
            failures.join("\n  ")
        );
    }

    #[test]
    fn a_re_aim_that_changes_row_is_refused() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 20);
        let caret = (4, 40u16);
        let mut spins = 0;
        while pet.flight.is_none() && spins < 60 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some(caret)));
            spins += 1;
        }
        let f0 = pet.flight.expect("fixture: the word jump pounces");
        assert!(!f0.big, "fixture: a pounce, inside the hop tier: {f0:?}");
        while pet.flight.is_some_and(|f| f.t / f.dur < 0.3) {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some(caret)));
        }
        assert!(pet.flight.is_some(), "fixture: still airborne");
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((5, 2))));
        let f1 = pet.flight.expect("the pounce is still in the air");
        assert!(
            (f1.to_row - 4.0).abs() < 1e-6,
            "a re-aim never changes row, got {f1:?}"
        );
        assert!(
            (f1.to_col - f0.to_col).abs() < 1e-6,
            "nor the column: the row change is refused whole, got {f1:?}"
        );
        let mut launches = 0;
        let mut last = pet.flight;
        let mut final_row = 4.0f32;
        for _ in 0..200 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((5, 2))));
            if last.is_none() && pet.flight.is_some() {
                launches += 1;
            }
            last = pet.flight;
            final_row = f.row;
        }
        assert_eq!(launches, 1, "one flight down to row 5 after the landing");
        assert!(
            (final_row - 5.0).abs() < 0.01,
            "and it ends on row 5, got {final_row}"
        );
    }

    #[test]
    fn the_wall_side_stand_takes_the_blank_row_below() {
        let w = art_cols(10, 20);
        let mut pet = PetBrain::default();
        let mut spans = vec![(0u16, 0u16); 24];
        spans[6] = (0, 78);
        pet.sense_ink(0, &spans, Some(6));
        let (_, row) = pet.station_safe((6, 75), 80, 24, w);
        assert!(
            (row - 7.0).abs() < 0.01,
            "the walled-shut row hands the stand to the blank row below, got row {row}"
        );
        let mut spans = vec![(0u16, 0u16); 24];
        spans[23] = (0, 78);
        pet.sense_ink(0, &spans, Some(23));
        let (_, row) = pet.station_safe((23, 75), 80, 24, w);
        assert!(
            (row - 22.0).abs() < 0.01,
            "and on the last row, to the blank row above, got row {row}"
        );

        for (row, blank) in [(6u16, 7.0f32), (23, 23.0)] {
            let mut p = Probe::new(80);
            p.warm(row, 72);
            p.mark();
            let mut taken = false;
            let mut check = |p: &Probe, f: PetFrame, at: &str| {
                if p.pet.flight.is_none() && (f.row - blank).abs() < 0.01 {
                    taken = true;
                }
                if taken {
                    assert!(
                        (f.row - blank).abs() < 0.01,
                        "row {row}: hopped back toward the words at {at}: row {}",
                        f.row
                    );
                }
            };
            for c in 73..=78u16 {
                p.ink(row, 0, c);
                for _ in 0..6 {
                    let f = p.frame(Some((row, c)));
                    check(&p, f, &format!("caret {c}"));
                }
            }
            for _ in 0..120 {
                let f = p.frame(Some((row, 78)));
                check(&p, f, "the quiet after");
            }
            assert!(taken, "row {row}: the cat took its stand on row {blank}");
            let tr = p.trigger.expect("marked");
            let flights = p.flights.iter().filter(|f| f.at >= tr).count();
            let wanted = usize::from(row == 6);
            assert_eq!(
                flights, wanted,
                "row {row}: {wanted} flight(s), the next line's stand or none: {:?}",
                p.flights
            );
        }
    }

    #[test]
    fn the_wall_transit_never_fires_after_a_flight_that_already_crossed() {
        let mut p = Probe::new(80);
        p.warm(5, 79);
        p.mark();
        assert!(
            !p.pet.wall_side,
            "fixture: typed to the margin, the cat never crossed the caret"
        );
        p.live = Some(6);
        p.ink(6, 0, 0);
        let _ = p.frame(Some((6, 0)));
        assert!(
            !p.pet.wall_side,
            "fixture: the wrap flips the wall side — the transit latched on this frame"
        );
        let mut latched_airborne = false;
        for c in 1..=12u16 {
            p.ink(6, 0, c);
            for _ in 0..8 {
                let _ = p.frame(Some((6, c)));
                latched_airborne |= p.pet.pending_wall_transit && p.pet.flight.is_some();
            }
        }
        p.hold(Some((6, 12)), 2.0);
        assert!(
            !latched_airborne,
            "a flight consumes the transit latch on its way out, never carries it"
        );
        let flights: Vec<&ProbeFlight> = p.flights.iter().collect();
        assert!(!flights.is_empty(), "fixture: the wrap flies");
        assert!(
            flights.iter().all(|f| (f.dur - WALL_HOP_DUR).abs() > 1e-4
                || (f.arc - WALL_HOP_ARC).abs() > 1e-4),
            "no span-blind wall arc after a flight that already crossed: {flights:?}"
        );
        let episodes = flights
            .iter()
            .filter(|f| !f.site.starts_with("bound 2") && !f.site.starts_with("mid-air re-aim"))
            .count();
        assert_eq!(
            episodes, 1,
            "exactly one airborne episode for the line end and the wrap: {flights:?}"
        );
        let last = p.last.expect("frames were drawn");
        assert!(p.pet.flight.is_none(), "and it is down by the end");
        assert!(
            last.col > 12.0 && (last.row - 6.0).abs() < 0.01,
            "stationed on the caret's right on the new row, got ({:.2}, {})",
            last.col,
            last.row
        );
    }

    #[test]
    fn a_snap_home_is_a_jump_not_a_fright() {
        let judge = |w: &Law, from: usize, door: &str| {
            assert_eq!(
                w.onsets(from, PetAction::Startle),
                0,
                "{door}: no fright, got {:?}",
                &w.actions[from..]
            );
            assert!(
                !w.wore(from, PetGlyphId::PetCrouchWiggle),
                "{door}: never the show's gather"
            );
            assert_eq!(
                w.episodes(from),
                1,
                "{door}: one airborne episode: {:?}",
                &w.actions[from..]
            );
            assert!(w.pet.flight.is_none(), "{door}: down by the end");
            assert!(!w.dust(from), "{door}: no dust");
        };

        let mut w = Law::warm(80, 30, 5, 60);
        w.hold(Some((5, 60)), 150);
        assert!(w.pet.action.settled(), "fixture: settled");
        let at = w.now();
        let _ = w.frame(Some((5, 3)));
        assert!(
            w.pet.pending_pounce && w.pet.quiet_leap && !w.pet.pending_big_jump,
            "the snap latches a quiet jump, never the show"
        );
        w.hold(Some((5, 3)), 120);
        judge(&w, at, "open row");

        let mut w = Law::warm(80, 24, 23, 40);
        w.hold(Some((23, 40)), 150);
        let at = w.now();
        let _ = w.frame(Some((23, 3)));
        w.hold(Some((23, 3)), 120);
        judge(&w, at, "last row");
        let (_, row, _, _) = w.last;
        assert!(
            (row - 22.0).abs() < 0.01,
            "the stand is the blank row above the inked line, got {row}"
        );
        let down = w.touchdown(at).expect("landed");
        assert!(
            w.actions[down..].iter().all(|a| *a != PetAction::Startle),
            "and nothing replays a fright after the landing"
        );
    }

    #[test]
    fn a_startle_shoved_off_its_spot_is_not_replayed() {
        let mut w = Law::warm(80, 30, 5, 30);
        w.hold(Some((5, 30)), 150);
        assert!(w.pet.action.settled(), "fixture: settled");
        let at = w.now();
        let _ = w.frame(Some((5, 24)));
        assert_eq!(w.pet.action, PetAction::Startle, "fixture: the fright");
        w.hold(Some((5, 24)), 3);
        w.ink(5, 0, 40);
        w.hold(Some((5, 24)), 120);
        assert_eq!(
            w.onsets(at, PetAction::Startle),
            1,
            "exactly one fright: {:?}",
            &w.actions[at..]
        );
        let down = w.touchdown(at).expect("the eviction hops");
        assert!(
            w.actions[down..].iter().all(|a| *a != PetAction::Startle),
            "never replayed after the hop: {:?}",
            &w.actions[down..]
        );
        assert!(w.pet.action.settled(), "and settled after it");
    }

    #[test]
    fn a_wrapping_build_log_sits_the_cat_down_to_watch() {
        let last = PROBE_ROWS - 1;
        for (burst, above) in [(false, true), (true, true), (false, false)] {
            let door = format!("burst={burst} row-above-inked={above}");
            let mut p = Probe::judged(80);
            p.warm(last, 40);
            p.hold(Some((last, 40)), 2.5);
            p.mark();
            p.log_rows(240, burst, above);
            assert!(p.pet.stream, "{door}: the watch is armed");
            assert!(
                p.episodes <= 2,
                "{door}: at most two airborne episodes, got {}: {:?}",
                p.episodes,
                p.actions
            );
            assert!(
                p.actions
                    .iter()
                    .all(|(at, a)| *a != PetAction::Startle
                        || *at < p.trigger.unwrap_or(0.0) + 0.2),
                "{door}: no fright after 0.2 s: {:?}",
                p.actions
            );
            let still: Vec<&(f32, PetFrame)> =
                p.trace.iter().filter(|(at, _)| *at >= 1.5).collect();
            let (c0, r0) = (still[0].1.col, still[0].1.row);
            assert!(
                still
                    .iter()
                    .all(|(_, f)| (f.col - c0).abs() < 1e-3 && (f.row - r0).abs() < 1e-3),
                "{door}: still from 1.5 s on"
            );
            assert!(
                still.iter().all(|(_, f)| matches!(
                    f.action,
                    PetAction::Perk | PetAction::Sit | PetAction::Leap | PetAction::Land
                )),
                "{door}: sat down to watch: {:?}",
                p.actions
            );
            assert!(p.needs_always, "{door}: the lane is held throughout");
            if !above {
                assert!(
                    (r0 - f32::from(last - 1)).abs() < 0.01,
                    "{door}: seated on the blank row above the log (rule 3), got row {r0}"
                );
            }
            let flights_before = p.flights.len();
            let at_prompt = p.now();
            p.ink(last, 0, 2);
            if above {
                p.ink(last - 1, 0, 80);
            }
            let _ = p.frame(Some((last, 2)));
            p.hold(Some((last, 2)), 3.0);
            let home: Vec<&ProbeFlight> = p.flights[flights_before..].iter().collect();
            assert!(
                (1..=2).contains(&home.len()),
                "{door}: one trip home, at most two bounds: {home:?}"
            );
            assert!(p.pet.flight.is_none(), "{door}: home within 3 s");
            let tr = p.trigger.expect("marked");
            let down = p.touchdown.expect("landed") - (at_prompt - tr);
            assert!(
                down <= 3.0,
                "{door}: paws down within 3 s of the prompt, got {down:.2}"
            );
            let last_f = p.last.expect("frames");
            assert!(
                (last_f.row - f32::from(last)).abs() < 0.01,
                "{door}: home on the last row"
            );
            assert!(!p.pet.stream, "{door}: the watch lapsed");
            p.hold(Some((last, 2)), SLEEP_AFTER + BREATH_WINDOW + 2.0);
            assert!(
                !p.pet.needs_frames(),
                "{door}: the lane is released once settled"
            );
        }
    }

    #[test]
    fn a_top_down_paint_never_hops_every_row() {
        let mut w = Law::warm(80, 30, 0, 10);
        let at = w.now();
        for r in 1..30u16 {
            let _ = w.frame(Some((r, 0)));
        }
        assert!(w.pet.stream, "the paint arms the watch");
        assert!(
            w.episodes(at) <= 2,
            "at most two hops during the paint: {:?}",
            &w.actions[at..]
        );
        let after = w.now();
        w.hold(Some((29, 0)), 200);
        assert!(!w.pet.stream, "the watch lapses");
        assert!(
            (1..=2).contains(&w.episodes(after)),
            "one trip after the lapse: {:?}",
            &w.actions[after..]
        );
        let (_, row, _, _) = w.last;
        assert!(
            w.pet.flight.is_none() && (row - 29.0).abs() < 0.01,
            "home on row 29"
        );
    }

    #[test]
    fn a_held_word_jump_is_not_a_stream() {
        let mut w = Law::new(100, 30, (4, 2));
        w.hold(Some((4, 2)), 2);
        for k in 1..=11u16 {
            let _ = w.frame(Some((4, 2 + 8 * k)));
            assert!(!w.pet.stream, "a held word-jump never arms the watch");
        }
        w.hold(Some((4, 90)), 60);
        assert!(!w.pet.stream);
    }

    #[test]
    fn a_line_change_to_the_right_is_still_a_quiet_crossing() {
        for cols in [80u16, 100] {
            let mut p = Probe::judged(cols);
            p.warm(6, 2);
            p.hold(Some((6, 2)), 2.5);
            p.mark();
            let end = cols - 8;
            p.ink(7, 0, end);
            p.live = Some(7);
            let _ = p.frame(Some((7, end)));
            p.hold(Some((7, end)), 3.0);
            let tr = p.trigger.expect("marked");
            let flights: Vec<&ProbeFlight> = p.flights.iter().filter(|f| f.at >= tr).collect();
            assert!(
                !flights.is_empty(),
                "{cols}: fixture — the line change flies"
            );
            assert!(
                flights.iter().all(|f| !f.show),
                "{cols}: a printed line is not a chosen jump: {flights:?}"
            );
            assert!(!p.wiggle, "{cols}: no gather on a printed line");
            assert!(!p.dust, "{cols}: no dust on a printed line");
            assert!(
                p.onsets(PetAction::Startle) == 0,
                "{cols}: no fright on a printed line: {:?}",
                p.actions
            );
            let bar = quiet_speed_max(cols) * PROBE_FRAME.as_secs_f32() + 1e-3;
            assert!(
                p.max_dcol.0 <= bar,
                "{cols}: {:.3} cells in one frame, the quiet pace allows {bar:.3}",
                p.max_dcol.0
            );
        }
    }

    #[test]
    fn a_new_line_costs_one_flight_and_no_gallop() {
        for cols in [80u16, 200] {
            for from in [10u16, 40, 70] {
                let mut p = Probe::judged(cols);
                p.warm(6, from);
                p.mark();
                p.live = Some(7);
                p.ink(6, 0, from);
                p.ink(7, 0, 2);
                let _ = p.frame(Some((7, 2)));
                p.hold(Some((7, 2)), 3.0);
                assert_eq!(
                    p.episodes, 1,
                    "{cols} cols, Enter from {from}: one new line, one flight — {:?}",
                    p.actions
                );
                assert_eq!(
                    p.onsets(PetAction::Run),
                    0,
                    "{cols} cols, Enter from {from}: no gallop home — {:?}",
                    p.actions
                );
                assert_eq!(
                    p.onsets(PetAction::Startle),
                    0,
                    "{cols} cols, Enter from {from}: and no fright — {:?}",
                    p.actions
                );
            }
        }
    }

    #[test]
    fn a_notice_keeps_its_turned_head_for_the_beat() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 10, 10);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((10, 40))));
        let turned = pet.species.skin(PetGlyphId::PetPerkTurn);
        assert_eq!(f.action, PetAction::Perk, "fixture: the notice");
        assert_eq!(f.pose, turned, "fixture: the head turned to the caret");
        pet.pending_pounce = false;
        pet.pending_big_jump = false;
        let mut beat = 1;
        loop {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((10, 40))));
            if f.action != PetAction::Perk {
                break;
            }
            beat += 1;
            assert_eq!(
                f.pose, turned,
                "the head stays turned for the beat (frame {beat} of the notice)"
            );
        }
        assert!(beat >= 4, "fixture: the beat lasts ({beat} frames)");
    }

    #[test]
    fn the_notice_beat_is_only_paid_by_a_cat_at_rest() {
        let mut rested = Probe::judged(80);
        rested.warm(6, 40);
        rested.hold(Some((6, 40)), 1.5);
        rested.mark();
        let _ = rested.frame(None);
        rested.live = Some(7);
        rested.ink(6, 0, 0);
        rested.ink(7, 0, 2);
        let _ = rested.frame(Some((7, 2)));
        rested.hold(Some((7, 2)), 2.5);
        assert!(
            rested.onsets(PetAction::Perk) >= 1,
            "a cat at rest looks up before the bound: {:?}",
            rested.actions
        );
        assert!(
            rested.onsets(PetAction::Leap) >= 1,
            "fixture: and then crosses"
        );
        let mut typing = Probe::judged(80);
        typing.warm(6, 40);
        typing.mark();
        let _ = typing.frame(None);
        typing.live = Some(7);
        typing.ink(6, 0, 0);
        typing.ink(7, 0, 2);
        let _ = typing.frame(Some((7, 2)));
        typing.hold(Some((7, 2)), 2.5);
        assert_eq!(
            typing.onsets(PetAction::Perk),
            0,
            "a cat mid-chase does not pay the beat: {:?}",
            typing.actions
        );
    }

    #[test]
    fn no_bound_crosses_more_than_half_the_animal_in_a_frame() {
        let animal = art_cols(10, 20);
        for cols in [20u16, 40, 80, 120, 200, 400] {
            let per_frame = quiet_speed_max(cols) / 60.0;
            assert!(
                per_frame <= 0.5 * animal,
                "{cols} cols: a bound steps {per_frame:.2} cells a frame, \
                 {:.2} of the animal's own {animal:.2}-column width",
                per_frame / animal
            );
        }
    }

    #[test]
    fn the_chosen_show_is_paced_like_every_other_bound() {
        for cols in [80u16, 100, 120] {
            let mut p = Probe::judged(cols);
            p.warm(6, 3);
            p.hold(Some((6, 3)), 2.5);
            p.mark();
            let end = cols - 1;
            for r in 4..=8u16 {
                p.ink(r, 0, cols);
            }
            let _ = p.frame(Some((6, end - 1)));
            p.hold(Some((6, end - 1)), 3.0);
            let tr = p.trigger.expect("marked");
            let flights: Vec<&ProbeFlight> = p.flights.iter().filter(|f| f.at >= tr).collect();
            assert!(
                flights.iter().any(|f| f.show),
                "{cols}: fixture — the chosen show plays: {flights:?}"
            );
            for f in &flights {
                let rate = span_eq(f.to_col - f.from_col, f.to_row - f.from_row) / f.dur;
                assert!(
                    rate <= quiet_speed_max(cols) + 1e-3,
                    "{cols}: the show flew at {rate:.1} c/s over the {:.1} c/s pace: {f:?}",
                    quiet_speed_max(cols)
                );
            }
            let bar = quiet_speed_max(cols) * PROBE_FRAME.as_secs_f32() + 1e-3;
            assert!(
                p.max_dcol.0 <= bar,
                "{cols}: {:.3} cells in one frame, the quiet pace allows {bar:.3}",
                p.max_dcol.0
            );
        }
    }

    #[test]
    fn a_burst_of_new_lines_comes_home_quietly() {
        let mut p = Probe::judged(80);
        p.warm(6, 70);
        p.hold(Some((6, 70)), 2.5);
        p.mark();
        for row in 7..=16u16 {
            p.ink(row, 0, 2);
            p.live = Some(row);
            let _ = p.frame(Some((row, 2)));
            p.hold_frames(Some((row, 2)), 2);
        }
        assert!(p.pet.stream, "fixture: the watch is armed by the burst");
        let held = p.flights.len();
        p.hold(Some((16, 2)), 3.0);
        assert!(!p.pet.stream, "fixture: the watch lapsed");
        let home: Vec<&ProbeFlight> = p.flights[held..].iter().collect();
        assert!(!home.is_empty(), "fixture: the cat comes home");
        assert!(
            (home[0].from_col - home[0].to_col).abs() >= BIG_GAP_FRAC * 80.0,
            "fixture: the gap the burst left is the standing-gap clause's: {home:?}"
        );
        assert!(
            home.iter().all(|f| !f.show),
            "the trip home after a burst is quiet, never the show: {home:?}"
        );
        assert!(!p.wiggle, "no butt-wiggle on a held Enter: {:?}", p.actions);
        assert!(!p.dust, "and no landing dust");
        assert!(p.pet.flight.is_none(), "down by the end");
    }

    #[test]
    fn a_hidden_frame_that_hid_no_move_keeps_the_next_show() {
        fn run(hide: bool) -> (bool, bool, bool) {
            let mut p = Probe::new(100);
            p.warm(6, 30);
            p.hold(Some((6, 30)), 2.5);
            assert!(p.pet.action.settled(), "fixture: settled");
            if hide {
                let _ = p.frame(None);
                let _ = p.frame(Some((6, 30)));
                assert!(
                    !p.pet.quiet_leap,
                    "a hidden frame that hid no move latched the quiet leap"
                );
            }
            p.mark();
            let _ = p.frame(Some((6, 95)));
            p.hold(Some((6, 95)), 3.0);
            let tr = p.trigger.expect("marked");
            (
                p.wiggle,
                p.dust,
                p.flights.iter().any(|f| f.at >= tr && f.show),
            )
        }
        assert_eq!(
            run(false),
            (true, true, true),
            "fixture: a chosen jump plays the whole show"
        );
        assert_eq!(
            run(true),
            run(false),
            "a hidden frame with no move behind it changes nothing"
        );
    }

    #[test]
    fn a_short_new_line_does_not_stand_down_the_next_show() {
        fn run(newline: bool) -> (bool, bool, bool) {
            let mut p = Probe::new(100);
            p.warm(23, 5);
            p.hold(Some((23, 5)), 2.5);
            assert!(p.pet.action.settled(), "fixture: settled");
            if newline {
                let _ = p.frame(Some((23, 1)));
                p.hold(Some((23, 1)), 2.5);
                assert!(p.pet.action.settled(), "fixture: settled again");
                assert!(
                    !p.pet.quiet_leap,
                    "a quiet leap no flight consumed outlived the arrival that ended it"
                );
            }
            p.mark();
            let _ = p.frame(Some((23, 90)));
            p.hold(Some((23, 90)), 3.0);
            let tr = p.trigger.expect("marked");
            (
                p.wiggle,
                p.dust,
                p.flights.iter().any(|f| f.at >= tr && f.show),
            )
        }
        assert_eq!(
            run(false),
            (true, true, true),
            "fixture: a chosen jump plays the whole show"
        );
        assert_eq!(
            run(true),
            run(false),
            "a four-column new line the pet never left the ground for cannot \
             spend the next jump's show"
        );
    }

    #[test]
    fn a_seam_crossing_never_bets_past_the_station() {
        let mut p = Probe::new(80);
        p.warm(6, 79);
        assert!(
            p.pet.rhythm_open(),
            "fixture: a real typing run is under way, so the lead is live"
        );
        p.mark();
        p.live = Some(7);
        p.ink(7, 0, 0);
        let _ = p.frame_wrapped(Some((7, 0)));
        p.hold(Some((7, 0)), 3.0);
        let tr = p.trigger.expect("marked");
        let cross = p
            .flights
            .iter()
            .find(|f| f.at >= tr && f.big)
            .expect("the wrap is crossed in one bound");
        let station = PetBrain::station(0, 80, art_cols(10, 20));
        assert!(
            cross.to_col <= station + LEAD_MAX,
            "a seam's bound may not bet past the station's own lead \
             ({station:.2} + {LEAD_MAX}), landed at {:.2}",
            cross.to_col
        );
        assert!(
            !p.actions
                .iter()
                .any(|(t, a)| *t >= tr && *a == PetAction::Run),
            "no gallop home from a wrap: {:?}",
            p.actions
        );
    }

    #[test]
    fn an_enter_during_a_gather_never_latches_the_show() {
        let mut p = Probe::new(80);
        p.warm(6, 41);
        p.ink(14, 0, 24);
        p.live = Some(14);
        let _ = p.frame(Some((14, 24)));
        p.hold_frames(Some((14, 24)), 2);
        assert_eq!(
            p.pet.action,
            PetAction::Crouch,
            "fixture: gathering for the row hop: {:?}",
            p.actions
        );
        assert!(!p.pet.pending_big_jump, "fixture: nothing latched yet");
        p.mark();
        p.ink(15, 0, 4);
        p.live = Some(15);
        let _ = p.frame(Some((15, 60)));
        assert!(
            !p.pet.pending_big_jump,
            "an Enter never latches the screen-crossing jump, whatever its size"
        );
        let mut c = 60u16;
        for _ in 0..10 {
            c += 1;
            let _ = p.frame(Some((15, c)));
            p.hold_frames(Some((15, c)), 5);
        }
        p.hold(Some((15, c)), 4.0);
        let tr = p.trigger.expect("marked");
        assert!(
            p.flights.iter().any(|f| f.at >= tr),
            "fixture: the gather really launches: {:?}",
            p.actions
        );
        assert!(
            !p.wiggle,
            "an Enter is never the chosen show, whoever spent the quiet leap: {:?}",
            p.actions
        );
        assert!(!p.dust, "…and a crossing nobody chose puffs no dust");
        assert!(
            !p.flights.iter().any(|f| f.at >= tr && f.show),
            "…and no flight leaves through the show's door"
        );
    }

    #[test]
    fn a_build_log_under_the_cat_is_not_chased_across_the_pane() {
        let last = PROBE_ROWS - 1;
        let mut p = Probe::judged(80);
        p.warm(last, 40);
        p.hold(Some((last, 40)), 2.5);
        p.mark();
        for k in 0..240u32 {
            for r in (last - 5)..=last {
                p.ink(r, 0, 80);
            }
            p.live = Some(last);
            let caret = if k % 2 == 0 { (last, 79) } else { (last, 0) };
            let _ = p.frame(Some(caret));
        }
        assert!(p.pet.stream, "fixture: the watch is armed");
        assert!(
            p.episodes <= 2,
            "at most two airborne episodes under the watch, got {}: {:?}",
            p.episodes,
            p.actions
        );
        assert_eq!(
            p.onsets(PetAction::Startle),
            0,
            "and no fright: {:?}",
            p.actions
        );
        let cols: Vec<f32> = p
            .trace
            .iter()
            .filter(|(at, _)| *at >= 1.5)
            .map(|(_, f)| f.col)
            .collect();
        let lo = cols.iter().copied().fold(f32::MAX, f32::min);
        let hi = cols.iter().copied().fold(f32::MIN, f32::max);
        assert!(
            hi - lo <= 8.0,
            "the cat is not towed after a caret nobody is chasing ({lo:.1}..{hi:.1})"
        );
    }

    #[test]
    fn every_new_line_reaction_is_lawful_and_brief() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum State {
            Settled,
            Typing,
            Asleep,
            Hidden,
        }

        fn panic_msg(e: &(dyn std::any::Any + Send)) -> String {
            e.downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(ToString::to_string))
                .unwrap_or_else(|| "(no message)".to_string())
        }

        fn judge(name: &str, p: &Probe, bar: f32, row: u16) -> Vec<String> {
            let mut out = Vec::new();
            let mut fail = |msg: String| out.push(format!("{name}: {msg}"));
            if p.episodes > 2 {
                fail(format!("{} airborne episodes: {:?}", p.episodes, p.actions));
            }
            let tr = p.trigger.unwrap_or(f32::MAX);
            let crossed_before = p
                .flights
                .iter()
                .any(|f| f.at < tr && (f.to_row - f32::from(row)).abs() < 0.01);
            if p.episodes == 0 && !crossed_before {
                fail(format!("never flew: {:?}", p.actions));
            }
            if p.episodes > 0 && crossed_before {
                fail(format!(
                    "crossed before the trigger and flew again: {:?}",
                    p.actions
                ));
            }
            let startles = p.onsets(PetAction::Startle);
            if startles > 0 {
                fail(format!("{startles} fright(s): {:?}", p.actions));
            }
            let perks = p.onsets(PetAction::Perk);
            if perks > 1 {
                fail(format!("{perks} notice beats: {:?}", p.actions));
            }
            if p.wiggle {
                fail("the show's gather played".to_string());
            }
            if p.dust {
                fail("dust on a quiet landing".to_string());
            }
            if p.cols == 80 && p.max_dcol.0 >= 1.0 {
                fail(format!(
                    "{:.3} cells in one frame at {:+.2}s ({:?})",
                    p.max_dcol.0, p.max_dcol.1, p.max_dcol.2
                ));
            }
            match p.grounded_at() {
                None if crossed_before && p.pet.flight.is_none() => {}
                None => fail("not on the ground by the end".to_string()),
                Some(t) if t > bar => fail(format!("paws down at {t:.2} s, bar {bar} s")),
                Some(_) => {}
            }
            if let Some(f) = p.last
                && (f.row - f32::from(row)).abs() > 0.01
            {
                fail(format!("ended on row {}, not {row}", f.row));
            }
            out
        }

        let mut failures: Vec<String> = Vec::new();
        let mut check = |name: String, bar: f32, row: u16, build: &mut dyn FnMut() -> Probe| {
            match catch_unwind(AssertUnwindSafe(build)) {
                Err(e) => failures.push(format!("{name}: {}", panic_msg(&*e))),
                Ok(p) => failures.extend(judge(&name, &p, bar, row)),
            }
        };

        for cols in [80u16, 200] {
            for col0 in [10u16, 40, 78] {
                for state in [State::Settled, State::Typing, State::Asleep, State::Hidden] {
                    for prompt_rows in [1u16, 3] {
                        let to_row = 6 + prompt_rows;
                        let pane_crossing = f32::from(cols) / quiet_speed_max(cols);
                        let walled =
                            f32::from(col0) + STATION_LEAD + art_cols(10, 20) > f32::from(cols);
                        let bar = match state {
                            State::Settled => 1.6,
                            State::Typing if walled => 1.4,
                            State::Typing => 1.3,
                            State::Hidden if col0 <= 40 => 1.3,
                            State::Hidden => PERK_HOLD + CROUCH_DUR + pane_crossing,
                            State::Asleep if prompt_rows == 1 => 2.7,
                            State::Asleep => 2.0,
                        };
                        let name = format!(
                            "{cols} cols, Enter from {col0} ({state:?}) onto a {prompt_rows}-row prompt"
                        );
                        check(name, bar, to_row, &mut || {
                            let mut p = Probe::judged(cols);
                            p.warm(6, col0);
                            match state {
                                State::Settled | State::Hidden => p.hold(Some((6, col0)), 2.5),
                                State::Asleep => p.hold(Some((6, col0)), 25.0),
                                State::Typing => {}
                            }
                            p.mark();
                            if state == State::Hidden {
                                let _ = p.frame(None);
                            }
                            for r in 7..=to_row {
                                p.ink(r, 0, 2);
                            }
                            p.live = Some(to_row);
                            let _ = p.frame(Some((to_row, 2)));
                            p.hold(Some((to_row, 2)), 3.0);
                            p
                        });
                    }
                }
            }
            let wrap_bar = if cols == 80 {
                1.6
            } else {
                CROUCH_DUR + 2.0 * BIG_FLIGHT_MAX + LAND_DUR
            };
            check(
                format!("{cols} cols, the typed margin wrap"),
                wrap_bar,
                7,
                &mut || {
                    let mut p = Probe::judged(cols);
                    p.warm(6, cols - 1);
                    p.mark();
                    p.live = Some(7);
                    p.ink(7, 0, 0);
                    let _ = p.frame(Some((7, 0)));
                    p.hold_frames(Some((7, 0)), 5);
                    p.type_to(7, 0, 5);
                    p.hold(Some((7, 5)), 3.0);
                    p
                },
            );
            let last = PROBE_ROWS - 1;
            check(
                format!("{cols} cols, the scrolled Enter after a short command"),
                1.6,
                last,
                &mut || {
                    let mut p = Probe::judged(cols);
                    p.warm(last, 10);
                    p.mark();
                    p.ink(last - 1, 0, 10);
                    p.ink(last, 0, 2);
                    p.live = Some(last);
                    let _ = p.frame(Some((last, 2)));
                    p.hold_frames(Some((last, 2)), 5);
                    p.type_to(last, 2, 6);
                    p.hold(Some((last, 6)), 3.0);
                    p
                },
            );
            check(
                format!("{cols} cols, the scrolled wrap on the last row"),
                wrap_bar,
                last,
                &mut || {
                    let mut p = Probe::judged(cols);
                    p.warm(last, cols - 1);
                    p.mark();
                    p.ink(last - 1, 0, cols);
                    p.ink(last, 0, 0);
                    p.live = Some(last);
                    let _ = p.frame_wrapped(Some((last, 0)));
                    p.hold_frames(Some((last, 0)), 5);
                    p.type_to(last, 0, 5);
                    p.hold(Some((last, 5)), 3.0);
                    p
                },
            );
        }
        assert!(
            failures.is_empty(),
            "{} scenario(s) broke the new-line law:\n  {}",
            failures.len(),
            failures.join("\n  ")
        );
    }

    #[test]
    fn a_one_frame_caret_hide_never_teleports_a_visible_cat() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 20, 60);
        let (t2, before) = idle(&mut pet, t, (20, 60), 0.5);
        t = t2;
        assert!(before.alpha > 200, "fixture: a cat plainly on the glass");

        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, None));
        t += Duration::from_millis(16);
        let after = pet.tick(sense(t, Some((8, 4))));
        assert!(
            after.alpha > 150,
            "one hidden frame must not erase the cat (alpha {})",
            after.alpha
        );
        assert!(
            (after.col - before.col).abs() < 1.0 && (after.row - before.row).abs() < 1.0,
            "a cat the eye can still see has to travel, not blink: \
             ({}, {}) → ({}, {})",
            before.col,
            before.row,
            after.col,
            after.row
        );
    }

    #[test]
    fn a_caret_withheld_mid_flight_holds_its_fade_and_lands_when_dark() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 0, 10);
        let width = art_cols(10, 20);
        let return_caret = (0, 69);
        let target_col = PetBrain::station(return_caret.1, 100, width);
        let target_row = f32::from(return_caret.0);

        pet.begin_arc(
            target_col,
            target_row,
            0.45,
            1.0,
            FlightKind {
                big: true,
                ..FlightKind::PLAIN
            },
        );
        pet.bound2 = Some((target_col + 5.0, target_row, false));
        t += Duration::from_millis(100);
        let mid = pet.tick(sense(t, Some((0, 12))));
        let live = pet.flight.expect("the fixture is genuinely mid-flight");
        assert!(live.t > 0.0 && live.t < live.dur);
        assert!(
            (mid.col - live.to_col).abs() > 10.0,
            "fixture reached the landing instead of the middle: {} -> {}",
            mid.col,
            live.to_col
        );

        let mut hidden_frames = 0;
        loop {
            t += Duration::from_millis(16);
            let hidden = pet.tick(sense(t, None));
            hidden_frames += 1;
            assert!(
                pet.flight.is_none(),
                "a hidden resident retained its old arc"
            );
            assert!(
                pet.bound2.is_none(),
                "a hidden resident retained a second bound"
            );
            if hidden.alpha > 0 {
                assert!((hidden.col - mid.col).abs() < 1e-4);
                assert!(
                    (hidden.row - hidden.lift - (mid.row - mid.lift)).abs() < 1e-4,
                    "withholding the caret moved the visible feet: row/lift \
                     ({}, {}) -> ({}, {})",
                    mid.row,
                    mid.lift,
                    hidden.row,
                    hidden.lift
                );
                assert_eq!(pet.deferred_hidden_landing, Some((target_col, target_row)));
            } else {
                assert!((pet.col - target_col).abs() < 1e-4);
                assert!((pet.row - target_row).abs() < 1e-4);
                assert!(pet.deferred_hidden_landing.is_none());
                break;
            }
            assert!(hidden_frames < 64, "the bounded fade must reach zero");
        }
        assert!(
            hidden_frames > 1,
            "the fixture inspected live hidden frames"
        );

        t += Duration::from_millis(16);
        let returned = pet.tick(sense(t, Some(return_caret)));
        assert!(pet.flight.is_none());
        assert_ne!(returned.action, PetAction::Leap);
        assert!((returned.col - target_col).abs() < 1e-4);
        assert!((returned.row - target_row).abs() < 1e-4);
    }

    #[test]
    fn a_quick_caret_return_cancels_the_deferred_hidden_landing() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 0, 10);
        let width = art_cols(10, 20);
        let return_caret = (0, 69);
        let target_col = PetBrain::station(return_caret.1, 100, width);
        let target_row = f32::from(return_caret.0);

        pet.begin_arc(
            target_col,
            target_row,
            0.45,
            1.0,
            FlightKind {
                big: true,
                ..FlightKind::PLAIN
            },
        );
        t += Duration::from_millis(100);
        let mid = pet.tick(sense(t, Some((0, 12))));
        assert!(pet.flight.is_some());
        assert!((mid.col - target_col).abs() > 10.0);

        let hidden = pet.tick(sense(t, None));
        assert!(hidden.alpha > 0);
        assert!((hidden.col - mid.col).abs() < 1e-4);
        assert!(
            (hidden.row - hidden.lift - (mid.row - mid.lift)).abs() < 1e-4,
            "the first hidden frame must retain the airborne feet baseline"
        );
        assert_eq!(pet.deferred_hidden_landing, Some((target_col, target_row)));
        assert!(
            pet.needs_frames(),
            "a zero-elapsed hide must retain fade/landing scheduler ownership"
        );

        t += Duration::from_millis(16);
        let returned = pet.tick(sense(t, Some(return_caret)));
        assert!(pet.deferred_hidden_landing.is_none());
        assert!((returned.col - hidden.col).abs() < 1.0);
        assert!(
            (returned.row - returned.lift - (hidden.row - hidden.lift)).abs() < 1.0,
            "quick return must resume from the fading feet baseline"
        );
        assert!(
            (returned.col - target_col).abs() > 10.0,
            "a quick return teleported to the deferred landing"
        );
        if let Some(flight) = pet.flight {
            assert!((flight.from_col - hidden.col).abs() < 1.0);
        }

        let mut settled = false;
        for _ in 0..32 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some(return_caret)));
            if pet.retired_flight_lift.is_none() {
                settled = true;
                break;
            }
            assert!(
                pet.needs_frames(),
                "retired lift must keep cadence until it reaches zero"
            );
        }
        assert!(settled, "retired lift must settle within FADE_IN");
    }

    #[test]
    fn a_re_anchor_leaves_the_ground_instead_of_cutting() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let w = art_cols(10, 20);
        let mut t = awake(&mut pet, start, 6, 58);
        let (t2, before) = idle(&mut pet, t, (6, 60), SIT_AFTER + 0.4);
        t = t2;
        assert!(before.action.settled(), "fixture: a settled cat");
        let spans = vec![(0u16, 80u16); 12];
        pet.sense_ink(0, &spans, Some(6));
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((6, 60))));
        assert_eq!(
            f.action,
            PetAction::Leap,
            "the re-anchor leaves the ground, got {:?}",
            f.action
        );
        assert!(
            (f.col - before.col).abs() < 6.0,
            "and it is still near where it was on the first frame of the trip \
             ({} → {})",
            before.col,
            f.col
        );
        let (_, after) = idle(&mut pet, t, (6, 60), 1.2);
        assert!(
            !on_ink(&after, (0, 80), w),
            "the trip ends on blank ground (col {})",
            after.col
        );
        assert!(
            after.action.settled(),
            "and the borrowed pose is handed back, got {:?}",
            after.action
        );
    }

    fn lawful_tick(pet: &mut PetBrain, s: PetSense, last: &mut (f32, f32, u16, u16)) -> PetFrame {
        const DT: f32 = 0.016;
        let (c0, r0, cols0, rows0) = *last;
        let w = art_cols(s.cell_w, s.cell_h);
        let rate = |fl: Option<Flight>| {
            fl.map_or((0.0, 0.0), |f| {
                (
                    (f.to_col - f.from_col).abs() / f.dur,
                    (f.to_row - f.from_row).abs() / f.dur,
                )
            })
        };
        let pre_flight = pet.flight;
        let (pre_c, pre_r) = rate(pre_flight);
        let pre_top = pet.top_speed();
        let f = pet.tick(s);
        let (post_c, post_r) = rate(pet.flight);
        let top = pre_top.max(pet.top_speed());
        let give_col = ((f32::from(cols0) - w).max(0.0) - (f32::from(s.cols) - w).max(0.0)).abs();
        let give_row =
            (f32::from(rows0.saturating_sub(1)) - f32::from(s.rows.saturating_sub(1))).abs();
        let col_bound = top.max(pre_c).max(post_c) * DT + give_col + 1e-3;
        let row_bound = pre_r.max(post_r) * DT + give_row + 1e-3;
        assert!(
            (f.col - c0).abs() <= col_bound,
            "the cat teleported {} cells sideways in one frame (bound {col_bound}): \
             col {c0} → {}, action {:?}",
            (f.col - c0).abs(),
            f.col,
            f.action
        );
        assert!(
            (f.row - r0).abs() <= row_bound,
            "the cat teleported {} rows in one frame (bound {row_bound}): \
             row {r0} → {}, action {:?}",
            (f.row - r0).abs(),
            f.row,
            f.action
        );
        let cap = |big: bool| {
            if big {
                quiet_speed_max(s.cols)
            } else {
                HOP_SPEED_MAX
            }
        };
        match (pre_flight, pet.flight) {
            (None, Some(fl)) => {
                let span = span_eq(fl.to_col - fl.from_col, fl.to_row - fl.from_row);
                let rate = span / fl.dur;
                assert!(
                    rate <= cap(fl.big) + 1e-3,
                    "the flight outruns the animal: launched at {rate:.1} c/s over the {} c/s \
                     cap (span_eq {span:.1} cells in {:.3} s, big {}, ({:.2},r{:.0}) -> \
                     ({:.2},r{:.0}), action {:?})",
                    cap(fl.big),
                    fl.dur,
                    fl.big,
                    fl.from_col,
                    fl.from_row,
                    fl.to_col,
                    fl.to_row,
                    f.action
                );
            }
            (Some(a), Some(b)) if (b.to_col - a.to_col).abs() > 1e-6 => {
                let left = (b.to_col - f.col).abs();
                let t_rem = (b.dur - b.t).max(1e-3);
                let rate = left / t_rem;
                assert!(
                    rate <= cap(b.big) + 1e-3,
                    "the re-aim outruns the flight: {left:.2} cells left to fly in {t_rem:.3} s \
                     is {rate:.1} c/s over the {} c/s cap (to {:.2} -> {:.2}, big {})",
                    cap(b.big),
                    a.to_col,
                    b.to_col,
                    b.big
                );
            }
            _ => {}
        }
        *last = (f.col, f.row, s.cols, s.rows);
        f
    }

    #[test]
    fn the_cat_never_changes_position_without_animating_it() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        let f0 = pet.tick(sense(t, Some((6, 20))));
        let mut last = (f0.col, f0.row, 100u16, 30u16);

        for k in 0..280u16 {
            let col = 20 + k % 70;
            let mut spans = vec![(0u16, 0u16); 20];
            spans[6] = (0, col);
            pet.sense_ink(0, &spans, Some(6));
            t += Duration::from_millis(16);
            let _ = lawful_tick(&mut pet, sense(t, Some((6, col))), &mut last);
        }

        for i in 0..120u16 {
            let spans = vec![(0u16, 0u16); 20];
            pet.sense_ink(0, &spans, Some(6));
            t += Duration::from_millis(16);
            let _ = lawful_tick(&mut pet, sense(t, Some((6, 2 + i % 3))), &mut last);
        }

        for i in 0..240u16 {
            let mut spans = vec![(0u16, 0u16); 20];
            let end = if i % 2 == 0 { 70 } else { 4 };
            spans[4..9].fill((0, end));
            pet.sense_ink(0, &spans, Some(6));
            t += Duration::from_millis(16);
            let _ = lawful_tick(&mut pet, sense(t, Some((6, 3))), &mut last);
        }

        pet.sense_ink(0, &[(0u16, 0u16); 20], Some(6));
        t += Duration::from_millis(16);
        let f = lawful_tick(&mut pet, sense(t, None), &mut last);
        assert!(f.alpha > 100, "fixture: still plainly on the glass");
        for _ in 0..90 {
            t += Duration::from_millis(16);
            let _ = lawful_tick(&mut pet, sense(t, Some((22, 80))), &mut last);
        }

        for (cols, rows, frames) in [(40u16, 12u16, 90), (100, 30, 90)] {
            for _ in 0..frames {
                t += Duration::from_millis(16);
                let mut s = sense(t, Some((6, 20)));
                s.cols = cols;
                s.rows = rows;
                let _ = lawful_tick(&mut pet, s, &mut last);
            }
        }

        for _ in 0..900 {
            t += Duration::from_millis(16);
            let _ = lawful_tick(&mut pet, sense(t, Some((6, 20))), &mut last);
        }
    }

    #[test]
    fn a_re_aim_in_the_last_frames_of_an_arc_is_dropped_not_solved() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 6, 20);
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((6, 20 + POUNCE_JUMP as u16 + 4))));
        let caret = 20 + POUNCE_JUMP as u16 + 4;
        let mut airborne = 0;
        while pet.flight.is_none() && airborne < 30 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((6, caret))));
            airborne += 1;
        }
        let f = pet.flight.expect("fixture: the pounce got off the ground");
        let aimed = f.to_col;
        let mut last = pet.col;
        while pet
            .flight
            .is_some_and(|f| (f.t / f.dur).clamp(0.0, 1.0) <= REAIM_MAX_U)
        {
            t += Duration::from_millis(16);
            last = pet.tick(sense(t, Some((6, caret)))).col;
        }
        assert!(
            pet.flight.is_some(),
            "fixture: still airborne past the bar, with frames left to draw"
        );
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((6, 2))));
        assert!(
            f.col.is_finite() && f.row.is_finite(),
            "the dropped re-aim left the cat somewhere real, got {} / {}",
            f.col,
            f.row
        );
        assert!(
            (f.col - last).abs() < 2.0,
            "and it did not swerve on the way down ({last} → {})",
            f.col
        );
        assert_eq!(
            pet.flight.map(|f| f.to_col),
            Some(aimed),
            "the flight keeps the target it was already landing on"
        );
        for _ in 0..30 {
            t += Duration::from_millis(16);
            let prev = last;
            let f = pet.tick(sense(t, Some((6, 2))));
            last = f.col;
            assert!(
                f.col.is_finite() && (f.col - prev).abs() < 2.0,
                "the arc finished without a teleport ({prev} → {})",
                f.col
            );
        }
    }

    #[test]
    fn a_failure_never_fires_the_party_ring() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = idle(&mut pet, t, (4, 12), 0.5);
        pet.note_command_done(t, true, Some(3000));
        assert!(pet.grieving(), "the note opens the grief window");
        let mut saw_droop = false;
        for _ in 0..200 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12))));
            if f.action == PetAction::Droop {
                saw_droop = true;
                assert!(pet.grieving(), "the droop IS the grief window");
            }
            for m in f.motes.iter().flatten() {
                assert!(
                    !matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart),
                    "no party mote may share the glass with grief"
                );
            }
        }
        assert!(saw_droop, "the droop must have played");
        assert!(!pet.grieving(), "and the window closes when it ends");
    }

    #[test]
    fn grief_replays_beside_the_new_prompt() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let w = art_cols(10, 20);
        let t = awake(&mut pet, start, 4, 10);
        let (mut t, _) = idle(&mut pet, t, (4, 12), 0.5);
        pet.note_command_done(t, true, Some(3000));
        let mut f = pet.tick(sense(t + Duration::from_millis(16), Some((4, 12))));
        t += Duration::from_millis(16);
        for _ in 0..40 {
            t += Duration::from_millis(16);
            f = pet.tick(sense(t, Some((4, 12))));
            if f.action == PetAction::Droop {
                break;
            }
        }
        assert_eq!(f.action, PetAction::Droop, "fixture: mid-droop");
        let inked_to = (f.col + w + 3.0) as u16;
        let spans = vec![(0u16, inked_to); 6];
        pet.sense_ink(0, &spans, Some(4));
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, inked_to))));
        assert_eq!(
            f.action,
            PetAction::Leap,
            "the droop LEAVES the words instead of blinking off them"
        );
        assert_eq!(
            pet.resume,
            Some((PetAction::Droop, 0.0)),
            "and the hop owes the droop back, from the top of its beat"
        );
        let mut replayed = false;
        for _ in 0..90 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, inked_to))));
            assert!(
                pet.grieving(),
                "the grief window stays shut over the fanfare for the whole \
                 trip, got {:?}",
                f.action
            );
            if f.action == PetAction::Droop {
                assert!(
                    !on_ink(&f, (0, inked_to), w),
                    "replayed on blank ground (col {})",
                    f.col
                );
                replayed = true;
                break;
            }
        }
        assert!(replayed, "the borrowed droop must actually come back");
        let (_, f) = idle(&mut pet, t, (4, inked_to), DROOP_HOLD * 0.8);
        assert_eq!(
            f.action,
            PetAction::Droop,
            "the hold restarted so the grief can actually be read"
        );
    }

    #[test]
    fn a_streaming_pane_is_followed_with_or_without_shell_integration() {
        let lag = |burst: bool| -> f32 {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            pet.sense_ink(0, &[(0, 13), (0, 13), (0, 13)], Some(2));
            let t = awake(&mut pet, start, 2, 10);
            let (mut t, f) = idle(&mut pet, t, (2, 12), SIT_AFTER + 0.4);
            assert!(f.action.settled(), "fixture: a settled cat");
            let mut spans = vec![(0u16, 13u16); 3];
            let mut worst = 0.0f32;
            for edge in 3u16..=20 {
                spans.push((0, 14));
                pet.sense_ink(0, &spans, Some(edge));
                for _ in 0..12 {
                    t += Duration::from_millis(16);
                    let mut s = sense(t, Some((edge, 15)));
                    s.output_burst = burst;
                    let f = pet.tick(s);
                    worst = worst.max(f32::from(edge) - f.row);
                }
            }
            worst
        };
        let (without, with) = (lag(false), lag(true));
        assert!(
            without <= 4.0,
            "a log follows without shell integration too, worst lag {without} rows"
        );
        assert!(
            (without - with).abs() < 0.01,
            "and the SAME log gives the same animal either way: \
             {without} without OSC 133, {with} with it"
        );
    }

    #[test]
    fn the_watcher_hugs_the_live_edge() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 2, 10);
        let (mut t, f) = idle(&mut pet, t, (2, 12), SIT_AFTER + 0.4);
        assert!(f.action.settled(), "fixture: a settled cat");
        let mut spans = vec![(0u16, 13u16); 3];
        let mut f = f;
        let mut worst_lag = 0.0f32;
        for edge in 3u16..=10 {
            spans.push((0, 14));
            pet.sense_ink(0, &spans, Some(edge));
            for _ in 0..12 {
                t += Duration::from_millis(16);
                let mut s = sense(t, Some((2, 12)));
                s.output_burst = true;
                f = pet.tick(s);
                worst_lag = worst_lag.max(f32::from(edge) - f.row);
            }
        }
        assert!(
            worst_lag <= 4.0,
            "the watcher must FOLLOW the stream, worst lag {worst_lag} rows"
        );
        assert!(
            10.0 - f.row <= WATCH_HUG_ROWS,
            "mid-stream the lag is bounded by the hug's own hysteresis: \
             feet at row {}, edge 10",
            f.row
        );
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let mut s = sense(t, Some((2, 12)));
            s.output_burst = true;
            f = pet.tick(s);
        }
        assert!(
            (f.row - 10.0).abs() < WATCH_HUG_ROWS,
            "the watcher must hug the newest inked row: feet at row {}, edge 10",
            f.row
        );
        assert!(
            f.col >= 14.0,
            "and stand clear of the line's ink, col {}",
            f.col
        );
    }

    #[test]
    fn the_zees_die_before_the_ink() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let spans = vec![(0u16, 40u16), (0, 40), (0, 40), (0, 0), (0, 0)];
        pet.sense_ink(0, &spans, Some(2));
        let (mut t, f) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + 0.2);
        assert_eq!(f.action, PetAction::Sleep, "fixture: light sleep");
        for _ in 0..((ZEE_EVERY * 3.0 / 0.016) as u32) {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12))));
            for m in f.motes.iter().flatten() {
                if matches!(m.kind, PetMoteKind::Zee | PetMoteKind::ZeePop) && m.alpha > 0 {
                    assert!(
                        m.row >= 3.0,
                        "a visible z must never enter the inked band: row {}",
                        m.row
                    );
                }
            }
        }
    }

    #[test]
    fn the_peck_stance_clears_the_caret_cell() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (t, _) = type_run(&mut pet, t, 4, 12, 3, 0.06);
        let (_, f) = idle(&mut pet, t, (4, 15), SIT_AFTER + 0.5);
        assert_eq!(f.pose, PetGlyphId::PetSitFront, "fixture: the stare");
        assert!(
            f.col >= 15.0 + STATION_LEAD + PECK_CLEAR - 0.01,
            "the face-on sit parks clear of the caret cell, col {}",
            f.col
        );
    }

    #[test]
    fn an_unfed_ink_map_changes_nothing() {
        let start = Instant::now();
        let mut fed = PetBrain::default();
        let mut unfed = PetBrain::default();
        fed.sense_ink(0, &[], None);
        let script: &[(u16, u16, f32)] = &[
            (4, 12, 0.5),
            (4, 40, 1.0),
            (5, 2, 0.8),
            (5, 30, SIT_AFTER + 1.0),
        ];
        let mut t = start;
        for &(row, col, dwell) in script {
            let end = t + Duration::from_secs_f32(dwell);
            while t < end {
                t += Duration::from_millis(16);
                let a = fed.tick(sense(t, Some((row, col))));
                let b = unfed.tick(sense(t, Some((row, col))));
                assert_eq!(a.fp(), b.fp(), "empty map ⇒ identical frames");
            }
        }
    }

    #[test]
    fn tease_earns_a_running_chase_then_dignity() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = warm_settled(&mut pet, start);
        let base = pet.col + 12.0;
        let mut chased = false;
        let mut ran = false;
        let mut groomed = false;
        let mut phase = 0.0f32;
        for _ in 0..700 {
            t += Duration::from_millis(16);
            phase += 0.04;
            let px = base + 8.0 * phase.sin();
            let f = ptick(&mut pet, t, (4, 44), Some((px, 4.0)));
            if chased && pet.pursuit_t.is_none() {
                groomed |= f.action == PetAction::Groom;
            }
            if pet.pursuit_t.is_some() {
                chased = true;
                if matches!(f.action, PetAction::Walk | PetAction::Run) {
                    ran = true;
                }
                assert!(
                    pet.flight.is_none(),
                    "a chase runs on paws: the standing-gap doors stay shut"
                );
            }
        }
        assert!(chased, "the tease must bait a chase");
        assert!(ran, "and the chase must actually run");
        assert!(pet.pursuit_t.is_none(), "stamina breaks the chase off");
        assert!(
            pet.pursuit_cool > 0.0,
            "and the cooldown guards the pet's dignity"
        );
        groomed |= pet.action == PetAction::Groom;
        for _ in 0..500 {
            t += Duration::from_millis(16);
            let f = ptick(&mut pet, t, (4, 44), None);
            groomed |= f.action == PetAction::Groom;
        }
        assert!(groomed, "the break-off owes the groom of dignity");
        assert!(!pet.groom_owed, "and the debt is paid, not pinned");
    }

    #[test]
    fn typing_aborts_the_chase_instantly() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = warm_settled(&mut pet, start);
        let base = pet.col + 12.0;
        let mut phase = 0.0f32;
        for _ in 0..120 {
            t += Duration::from_millis(16);
            phase += 0.04;
            let px = base + 8.0 * phase.sin();
            let _ = ptick(&mut pet, t, (4, 44), Some((px, 4.0)));
            if pet.pursuit_t.is_some() {
                break;
            }
        }
        assert!(pet.pursuit_t.is_some(), "fixture: the chase is live");
        t += Duration::from_millis(16);
        let _ = ptick(&mut pet, t, (4, 52), Some((base, 4.0)));
        assert!(pet.pursuit_t.is_none(), "typing ends the game instantly");
        for _ in 0..30 {
            t += Duration::from_millis(16);
            phase += 0.04;
            let px = base + 8.0 * phase.sin();
            let _ = ptick(&mut pet, t, (4, 52), Some((px, 4.0)));
            assert!(
                pet.pursuit_t.is_none(),
                "the chase stays down while typing is recent"
            );
        }
    }

    #[test]
    fn creeping_toy_pins_the_stakeout() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = warm_settled(&mut pet, start);
        let mut px = pet.col + 8.0;
        let mut staked = false;
        for k in 0..300 {
            t += Duration::from_millis(16);
            px += if k % 40 < 20 { 0.05 } else { -0.05 };
            let f = ptick(&mut pet, t, (4, 44), Some((px, 4.0)));
            if pet.stakeout {
                staked = true;
                assert_eq!(
                    f.action,
                    PetAction::Crouch,
                    "the stakeout is the hunting crouch"
                );
            }
        }
        assert!(staked, "a creeping toy must pin the stakeout");
        t += Duration::from_millis(16);
        let f = ptick(&mut pet, t, (4, 44), None);
        assert!(!pet.stakeout, "no toy, no stakeout");
        assert_ne!(f.action, PetAction::Leap, "and never a phantom launch");
    }

    #[test]
    fn far_peek_gets_a_look_not_a_trip() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, _) = idle(&mut pet, t, (4, 14), SIT_AFTER + 0.4);
        let col0 = pet.col;
        pet.note_peek(t, col0 + 15.0, 4.0);
        assert!(pet.pending_bat.is_none(), "out of paw reach: no visit");
        assert!(pet.pending_look.is_some(), "but well worth a look");
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 14))));
        assert_eq!(f.action, PetAction::Perk, "the look is the double-take");
        assert!(!pet.facing_left, "faced toward the peek");
        assert!(
            (pet.col - col0).abs() < 0.5,
            "and the paws never left the station"
        );
    }

    #[test]
    fn non_finite_inputs_never_poison_the_pose() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 12);
        let (mut t, _) = idle(&mut pet, t, (4, 14), 0.5);
        pet.note_peek(t, f32::NAN, 4.0);
        assert!(pet.pending_bat.is_none() && pet.pending_look.is_none());
        for _ in 0..20 {
            t += Duration::from_millis(16);
            let f = ptick(&mut pet, t, (4, 14), Some((f32::NAN, f32::INFINITY)));
            assert!(f.col.is_finite(), "the pose stays finite");
        }
        assert_eq!(pet.pointer_heat, 0.0, "a NaN pointer is no pointer");
    }

    #[test]
    fn a_long_rally_earns_the_tennis_watch() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = warm_settled(&mut pet, start);
        let (mut col, mut right) = (44i32, true);
        let mut watched = false;
        let (mut faced_left, mut faced_right) = (false, false);
        for _ in 0..60 {
            t += Duration::from_millis(150);
            right = !right;
            col += if right { 3 } else { -3 };
            let f = pet.tick(sense(t, Some((4, col as u16))));
            if pet.tennis {
                watched = true;
                assert_eq!(f.action, PetAction::Sit, "the watch sits");
                faced_left |= pet.facing_left;
                faced_right |= !pet.facing_left;
            }
        }
        assert!(watched, "a sustained rally must earn the watch");
        assert!(
            faced_left && faced_right,
            "and the facing follows the ball both ways"
        );
        let (_, _) = idle(&mut pet, t, (4, col as u16), TENNIS_LAPSE + 0.5);
        assert!(!pet.tennis, "the rally over, the watch stands down");
    }

    #[test]
    fn a_bored_cat_hides_behind_a_word_and_comes_back() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = warm_settled(&mut pet, start);
        let mut spans = [(0u16, 0u16); 12];
        let word_row = pet.row as usize;
        let seat = pet.col.round() as u16;
        spans[word_row] = (seat - 11, seat - 3);
        let mut hid = false;
        let mut under = false;
        for _ in 0..900 {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            if !pet.hiding && pet.hide_to.is_none() {
                pet.mote_serial = 3;
                pet.content = pet.content.max(0.9);
            }
            let f = pet.tick(sense(t, Some((4, 44))));
            if pet.hiding {
                hid = true;
                under |= f.under_ink;
            }
        }
        assert!(hid, "the deal must send the cat behind the word");
        assert!(under, "and back there it draws UNDER the glyphs");
        assert!(!pet.hiding, "the dwell ends the lurk");
        let (_, f) = idle(&mut pet, t, (4, 44), 2.0);
        assert!(!f.under_ink, "surfaced on the walk home");
    }

    fn hide_behind(pet: &mut PetBrain, start: Instant, off: u16) -> (Instant, [(u16, u16); 12]) {
        let mut t = warm_settled(pet, start);
        let mut spans = [(0u16, 0u16); 12];
        let seat = pet.col.round() as u16;
        spans[pet.row as usize] = (seat - 8 - off, seat - off);
        for _ in 0..900 {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            if !pet.hiding && pet.hide_to.is_none() {
                pet.mote_serial = 3;
                pet.content = pet.content.max(0.9);
            }
            let _ = pet.tick(sense(t, Some((4, 44))));
            if pet.hiding {
                return (t, spans);
            }
        }
        panic!("fixture: the deal must send the cat behind the word");
    }

    #[test]
    fn a_hiding_cat_holds_its_crouch_off_the_frame_train() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, spans) = hide_behind(&mut pet, start, 3);
        let hid_at = t;
        while pet.needs_frames() {
            assert!(
                t - hid_at < Duration::from_millis(500),
                "the crouch must leave the train once the veil has eased (veil {:?})",
                pet.letter_veil
            );
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            let _ = pet.tick(sense(t, Some((4, 44))));
        }
        assert!(pet.hiding, "fixture: still behind the word");
        let end = t + Duration::from_secs_f32(pet.hide_t);
        let mut ticks = 0;
        let mut f = None;
        while pet.hiding {
            assert!(!pet.needs_frames(), "the held crouch stays off the train");
            let due = pet
                .next_change_deadline(t)
                .expect("the dwell's end is offered");
            assert!(
                due <= end + Duration::from_millis(50),
                "never past the dwell's end"
            );
            t = due;
            pet.sense_ink(0, &spans, None);
            f = Some(pet.tick(sense(t, Some((4, 44)))));
            ticks += 1;
            assert!(
                ticks <= 20,
                "the dwell is served by its offers, not a train"
            );
        }
        assert!(t >= end, "the dwell is served in full");
        assert_eq!(
            f.map(|f| f.action),
            Some(PetAction::Stand),
            "and the cat stands up"
        );
        let mut owed = false;
        for _ in 0..3 {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            let _ = pet.tick(sense(t, Some((4, 44))));
            owed |= pet.needs_frames();
        }
        assert!(owed, "the walk home is on the train");
    }

    #[test]
    fn a_cat_surfacing_from_a_hide_walks_home_from_rest() {
        for off in [3u16, 6] {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let (mut t, spans) = hide_behind(&mut pet, start, off);
            while pet.hiding {
                t += Duration::from_millis(16);
                pet.sense_ink(0, &spans, None);
                let _ = pet.tick(sense(t, Some((4, 44))));
            }
            let from = pet.col;
            let mut prev = (pet.col, 0.0f32);
            let mut last = None;
            for _ in 0..150 {
                t += Duration::from_millis(16);
                pet.sense_ink(0, &spans, None);
                let f = pet.tick(sense(t, Some((4, 44))));
                assert_ne!(f.action, PetAction::Leap, "off {off}: walked, not hopped");
                let v = (f.col - prev.0) / 0.016;
                if matches!(f.action, PetAction::Stand | PetAction::Crouch) {
                    assert!(
                        (f.col - prev.0).abs() < 1e-4,
                        "off {off}: the {:?} pose slid {}",
                        f.action,
                        f.col - prev.0
                    );
                }
                assert!(
                    (v - prev.1).abs() <= 6.0,
                    "off {off}: the speed jumped {} -> {v} cells/s in one frame",
                    prev.1
                );
                prev = (f.col, v);
                last = Some(f);
            }
            let f = last.expect("ticked");
            assert!(
                f.action.settled() && f.col > from + 2.0,
                "off {off}: home again ({:?} at {} from {from})",
                f.action,
                f.col
            );
        }
    }

    #[test]
    fn a_commit_stare_on_the_letters_steps_off_them_at_once() {
        for off in [3u16, 6] {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let (mut t, spans) = hide_behind(&mut pet, start, off);
            while pet.hiding {
                t += Duration::from_millis(16);
                pet.sense_ink(0, &spans, None);
                let _ = pet.tick(sense(t, Some((4, 44))));
            }
            assert_eq!(
                pet.action,
                PetAction::Stand,
                "off {off}: fixture: surfaced standing"
            );
            assert!(
                pet.ink_overlaps(pet.col, pet.row, art_cols(10, 20)),
                "off {off}: fixture: on the letters"
            );
            pet.commit_t = COMMIT_STARE;
            let from = pet.col;
            let mut left = None;
            for k in 0..20usize {
                t += Duration::from_millis(16);
                pet.sense_ink(0, &spans, None);
                let f = pet.tick(sense(t, Some((4, 44))));
                if (f.col - from).abs() > 1e-3 || f.action == PetAction::Leap {
                    left = Some(k);
                    break;
                }
            }
            assert!(
                left.is_some_and(|k| k <= 1),
                "off {off}: the stare kept the cat on the letters ({left:?} frames)"
            );
        }
    }

    #[test]
    fn a_spent_watch_rearms_once_the_stream_ends() {
        let mut pet = PetBrain {
            watch_heat: 1.0,
            watch_spent: true,
            alpha: 1.0,
            ..PetBrain::default()
        };
        assert!(
            pet.needs_frames(),
            "live heat keeps the lane so the decay can run"
        );
        let start = Instant::now();
        let mut t = start;
        let _ = pet.tick(sense(t, Some((4, 14))));
        for _ in 0..80 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((4, 14))));
        }
        assert!(
            pet.watch_heat < WATCH_GATE,
            "the heat decayed under the gate"
        );
        assert!(!pet.watch_spent, "and the next stream earns a fresh stare");
    }

    #[test]
    fn every_idle_deal_goes_byte_stable_by_the_breath_window() {
        let start = Instant::now();
        for seed in 0..12u8 {
            let mut pet = PetBrain::default();
            let t = awake(&mut pet, start, 4, 10);
            let (mut t, _) = idle(&mut pet, t, (5, 0), 1.6);
            assert!(
                pet.action.settled(),
                "hand {seed}: fixture: settled after the Enter"
            );
            pet.force_seed(seed);
            t += Duration::from_millis(16);
            let mut s = sense(t, Some((5, 0)));
            s.output_burst = true;
            let _ = pet.tick(s);
            let (t, f) = idle(&mut pet, t, (5, 0), SLEEP_AFTER + BREATH_WINDOW + 2.0);
            assert_eq!(f.action, PetAction::Sleep, "hand {seed}: deeply asleep");
            assert!(
                !pet.needs_frames(),
                "hand {seed}: every idle envelope died inside the settle window"
            );
            assert!(pet.is_active(), "hand {seed}: still on screen, just still");
            assert!(
                f.motes.iter().all(Option::is_none),
                "hand {seed}: the mote lane is empty"
            );
            let a = pet.tick(sense(t + Duration::from_secs(1), Some((5, 0))));
            let b = pet.tick(sense(t + Duration::from_secs(2), Some((5, 0))));
            assert_eq!(a.fp(), b.fp(), "hand {seed}: deep sleep is byte-stable");
        }
    }

    #[test]
    fn no_idle_beat_moves_the_body() {
        let start = Instant::now();
        for seed in 0..12u8 {
            let mut pet = PetBrain::default();
            let t = awake(&mut pet, start, 4, 10);
            pet.force_seed(seed);
            let mut t = t;
            let mut anchor: Option<(f32, f32)> = None;
            let mut prev_col: Option<f32> = None;
            let end = t + Duration::from_secs_f32(SLEEP_AFTER + BREATH_WINDOW + 1.0);
            while t < end {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((4, 12))));
                if !f.action.settled() {
                    anchor = None;
                    prev_col = None;
                    continue;
                }
                assert!(
                    f.lift == 0.0,
                    "hand {seed}: a settled {:?} frame lifted the body by {}",
                    f.action,
                    f.lift
                );
                if let Some(c) = prev_col {
                    assert!(
                        (f.col - c).abs() <= 0.01,
                        "hand {seed}: a settled {:?} frame STEPPED the feet by {}",
                        f.action,
                        (f.col - c).abs()
                    );
                }
                prev_col = Some(f.col);
                match anchor {
                    None => anchor = Some((f.col, f.row)),
                    Some((c, r)) => {
                        assert!(
                            (f.row - r).abs() < 1e-6,
                            "hand {seed}: a settled {:?} frame changed the row: {r} -> {}",
                            f.action,
                            f.row
                        );
                        assert!(
                            (f.col - c).abs() <= PECK_CLEAR + 1e-3,
                            "hand {seed}: a settled {:?} frame moved the feet {} cells \
                             — past the seat's own clearance",
                            f.action,
                            (f.col - c).abs()
                        );
                    }
                }
            }
            assert!(
                anchor.is_some(),
                "hand {seed}: fixture: the run ends settled"
            );
        }
    }

    fn pose_runs(
        pet: &mut PetBrain,
        start: Instant,
        caret: (u16, u16),
        secs: f32,
    ) -> (Instant, Vec<(PetGlyphId, f32, u32)>) {
        let mut t = start;
        let end = start + Duration::from_secs_f32(secs);
        let mut runs: Vec<(PetGlyphId, f32, u32)> = Vec::new();
        while t < end {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some(caret)));
            match runs.last_mut() {
                Some(r) if r.0 == f.pose => r.2 += 1,
                _ => runs.push((f.pose, pet.quiet, 1)),
            }
        }
        (t, runs)
    }

    #[test]
    fn reduced_motion_holds_two_still_frames_and_no_motes() {
        let mut pet = PetBrain::default();
        let mut t = Instant::now();
        let mut poses = std::collections::BTreeSet::new();
        for k in 0..((SLEEP_AFTER + 4.0) / 0.016) as u32 {
            t += Duration::from_millis(16);
            let col = 20 + u16::try_from(k.min(60) / 6).unwrap_or(0);
            let mut s = sense(t, Some((4, col)));
            s.reduced_motion = true;
            let f = pet.tick(s);
            poses.insert(f.pose as u8);
            assert_eq!(f.lift, 0.0);
            assert!(
                f.motes.iter().all(Option::is_none),
                "reduced motion must never particle"
            );
        }
        assert_eq!(
            poses,
            [PetGlyphId::PetSit as u8, PetGlyphId::PetSleep0 as u8]
                .into_iter()
                .collect(),
            "reduced motion has exactly two frames: seated and asleep"
        );
    }

    #[test]
    fn every_dealt_idle_frame_holds_at_least_two_ticks() {
        let start = Instant::now();
        let mut seen = std::collections::HashSet::new();
        for seed in 0..12u8 {
            for (label, caret, warm) in [
                ("the plain seat", (4u16, 99u16), (4u16, 97u16)),
                ("eye contact", (4, 14), (4, 12)),
            ] {
                let mut pet = PetBrain::default();
                let t = awake(&mut pet, start, warm.0, warm.1);
                pet.force_seed(seed);
                let (_, runs) = pose_runs(&mut pet, t, caret, SLEEP_AFTER + 1.0);
                seen.extend(runs.iter().map(|(pose, ..)| *pose));
                for (pose, at, ticks) in runs.iter().take(runs.len() - 1) {
                    assert!(
                        *ticks >= 2,
                        "hand {seed} ({label}): {pose:?} flashed for {ticks} tick(s) \
                         at quiet {at}"
                    );
                }
            }
        }
        for beat in [
            PetGlyphId::PetSitBlink,
            PetGlyphId::PetSitFrontBlink,
            PetGlyphId::PetSitFlick,
            PetGlyphId::PetSitTailBack,
            PetGlyphId::PetSitTailLow,
            PetGlyphId::PetYawn,
            PetGlyphId::PetGroomFlank,
            PetGlyphId::PetLoafOpen,
        ] {
            assert!(seen.contains(&beat), "{beat:?} is never dealt in a settle");
        }
    }

    const PROBE_FRAME: Duration = Duration::from_micros(16_667);
    const PROBE_ROWS: u16 = 24;
    const PROBE_STILL: f32 = 0.1;

    #[derive(Clone, Copy, Debug)]
    struct ProbeFlight {
        at: f32,
        site: &'static str,
        from_col: f32,
        to_col: f32,
        from_row: f32,
        to_row: f32,
        dur: f32,
        arc: f32,
        big: bool,
        show: bool,
    }

    struct Probe {
        pet: PetBrain,
        t: Instant,
        t0: Instant,
        cols: u16,
        spans: Vec<(u16, u16)>,
        live: Option<u16>,
        last: Option<PetFrame>,
        trigger: Option<f32>,
        at_trigger: Option<PetFrame>,
        log_from: f32,
        flights: Vec<ProbeFlight>,
        actions: Vec<(f32, PetAction)>,
        max_dcol: (f32, f32, PetAction),
        max_drow: (f32, f32, PetAction),
        settled_since: Option<f32>,
        settle_at: Option<f32>,
        settle_where: (f32, f32, PetAction),
        judge: bool,
        frame_dur: Duration,
        law: (f32, f32, u16, u16),
        dust: bool,
        episodes: u32,
        touchdown: Option<f32>,
        wiggle: bool,
        trace: Vec<(f32, PetFrame)>,
        needs_always: bool,
        home_at: Option<f32>,
    }

    impl Probe {
        fn new(cols: u16) -> Self {
            Self::at(cols, Instant::now())
        }

        fn judged(cols: u16) -> Self {
            let mut p = Self::new(cols);
            p.judge = true;
            p.frame_dur = Duration::from_millis(16);
            p
        }

        fn at(cols: u16, t0: Instant) -> Self {
            Self {
                pet: PetBrain::default(),
                t: t0,
                t0,
                cols,
                spans: vec![(0, 0); usize::from(PROBE_ROWS)],
                live: None,
                last: None,
                trigger: None,
                at_trigger: None,
                log_from: 0.0,
                flights: Vec::new(),
                actions: Vec::new(),
                max_dcol: (0.0, 0.0, PetAction::Sit),
                max_drow: (0.0, 0.0, PetAction::Sit),
                settled_since: None,
                settle_at: None,
                settle_where: (0.0, 0.0, PetAction::Sit),
                judge: false,
                frame_dur: PROBE_FRAME,
                law: (0.0, 0.0, cols, PROBE_ROWS),
                dust: false,
                episodes: 0,
                touchdown: None,
                wiggle: false,
                trace: Vec::new(),
                needs_always: true,
                home_at: None,
            }
        }

        fn onsets(&self, a: PetAction) -> usize {
            let tr = self.trigger.unwrap_or(f32::MAX);
            self.actions
                .iter()
                .filter(|(at, x)| *at >= tr && *x == a)
                .count()
        }

        fn now(&self) -> f32 {
            self.t.saturating_duration_since(self.t0).as_secs_f32()
        }

        fn sense_at(&self, caret: Option<(u16, u16)>) -> PetSense {
            let mut s = sense(self.t, caret);
            s.cols = self.cols;
            s.rows = PROBE_ROWS;
            s
        }

        fn ink(&mut self, row: u16, first: u16, end: u16) {
            self.spans[usize::from(row)] = (first, end);
        }

        fn mark(&mut self) {
            self.trigger = Some(self.now());
            self.at_trigger = self.last;
            self.max_dcol = (0.0, 0.0, PetAction::Sit);
            self.home_at = None;
            self.max_drow = (0.0, 0.0, PetAction::Sit);
            self.settled_since = None;
            self.settle_at = None;
            self.dust = false;
            self.episodes = 0;
            self.touchdown = None;
            self.wiggle = false;
            self.trace.clear();
            self.needs_always = true;
        }

        fn grounded_at(&self) -> Option<f32> {
            if self.pet.flight.is_some() {
                return None;
            }
            self.touchdown
        }

        fn frame(&mut self, caret: Option<(u16, u16)>) -> PetFrame {
            let s = self.sense_at(caret);
            self.frame_with(s, caret)
        }

        fn frame_wrapped(&mut self, caret: Option<(u16, u16)>) -> PetFrame {
            let mut s = self.sense_at(caret);
            s.wrapped = true;
            self.frame_with(s, caret)
        }

        fn frame_with(&mut self, mut s: PetSense, caret: Option<(u16, u16)>) -> PetFrame {
            self.t += self.frame_dur;
            let now = self.now();
            s.now = self.t;
            self.pet.sense_ink(0, &self.spans, self.live);
            let w = art_cols(10, 20);
            let pre_action = self.pet.action;
            let pre_hop = self.pet.hop_crouch;
            let pre_gather = self.pet.big_gather;
            let pre_bound2 = self.pet.bound2.is_some();
            let pre_wall = self.pet.pending_wall_transit;
            let pre_play = self.pet.play_to.is_some();
            let pre_resume = self.pet.resume.is_some();
            let pre_flight = self.pet.flight;
            let pre_on_ink = self.pet.ink_overlaps(self.pet.col, self.pet.row, w);
            if self.last.is_none() {
                let f = self.pet.tick(s);
                self.law = (f.col, f.row, self.cols, PROBE_ROWS);
                self.last = Some(f);
                return f;
            }
            let f = if self.judge {
                lawful_tick(&mut self.pet, s, &mut self.law)
            } else {
                let f = self.pet.tick(s);
                self.law = (f.col, f.row, self.cols, PROBE_ROWS);
                f
            };

            if let Some(fl) = self.pet.flight {
                let launched = pre_flight.is_none();
                let reaimed = pre_flight.is_some_and(|p| (p.to_col - fl.to_col).abs() > 1e-6);
                if (launched || reaimed) && now >= self.log_from {
                    let site = if reaimed {
                        "mid-air re-aim (rebase)"
                    } else if fl.drop {
                        "sleeper's drop (Waking + pending_drop)"
                    } else if fl.big && pre_action == PetAction::Land && pre_bound2 {
                        "bound 2 (bound2 -> begin_big_arc)"
                    } else if pre_action == PetAction::Crouch && pre_gather {
                        "big door (big_gather -> launch_big)"
                    } else if fl.big {
                        "big bound (launch_big)"
                    } else if pre_action == PetAction::Crouch && pre_hop {
                        "row hop door (hop_crouch -> begin_flight vertical)"
                    } else if pre_action == PetAction::Crouch && pre_play {
                        "play (Crouch -> launch)"
                    } else if pre_action == PetAction::Crouch {
                        "pounce door (Crouch -> launch)"
                    } else if self.pet.resume.is_some() && !pre_resume {
                        "ink re-anchor (begin_arc, flight_shape vertical)"
                    } else if pre_on_ink
                        && matches!(
                            pre_action,
                            PetAction::Startle | PetAction::Frolic | PetAction::Perk
                        )
                    {
                        "ink re-anchor (vertical; reaction consumed)"
                    } else if pre_wall {
                        "wall transit (WALL_HOP_DUR arc, span-capped)"
                    } else {
                        "unknown"
                    };
                    self.flights.push(ProbeFlight {
                        at: now,
                        site,
                        from_col: fl.from_col,
                        to_col: fl.to_col,
                        from_row: fl.from_row,
                        to_row: fl.to_row,
                        dur: fl.dur,
                        arc: fl.arc,
                        big: fl.big,
                        show: fl.show,
                    });
                }
            }
            if now >= self.log_from && self.last.is_none_or(|l| l.action != f.action) {
                self.actions.push((now, f.action));
            }
            if let (Some(l), Some(tr)) = (self.last, self.trigger) {
                let dc = (f.col - l.col).abs();
                let dr = (f.row - l.row).abs();
                if dc > self.max_dcol.0 {
                    self.max_dcol = (dc, now, f.action);
                }
                if dc > PROBE_STILL && self.settled_since.is_none() {
                    self.home_at = Some(now - tr);
                }
                if dr > self.max_drow.0 {
                    self.max_drow = (dr, now, f.action);
                }
                if f.action == PetAction::Leap && l.action != PetAction::Leap {
                    self.episodes += 1;
                }
                if f.action != PetAction::Leap && l.action == PetAction::Leap {
                    self.touchdown = Some(now - tr);
                }
                if f.motes
                    .iter()
                    .flatten()
                    .any(|m| m.kind == PetMoteKind::Dust)
                {
                    self.dust = true;
                }
                if f.pose == PetGlyphId::PetCrouchWiggle {
                    self.wiggle = true;
                }
                self.trace.push((now - tr, f));
                self.needs_always &= self.pet.needs_frames();
            }
            if self.trigger.is_some() && self.settle_at.is_none() {
                let ok = caret.is_some_and(|c| {
                    let (tc, tr) = self.pet.station_safe(c, self.cols, PROBE_ROWS, w);
                    f.action.settled()
                        && self.pet.flight.is_none()
                        && (f.col - tc).abs() < 1.0
                        && (f.row - tr).abs() < 0.01
                });
                if ok {
                    let since = *self.settled_since.get_or_insert(now);
                    if now - since >= 0.5 {
                        self.settle_at = Some(since);
                        self.settle_where = (f.col, f.row, f.action);
                    }
                } else {
                    self.settled_since = None;
                }
            }
            self.last = Some(f);
            f
        }

        fn hold_frames(&mut self, caret: Option<(u16, u16)>, n: u32) {
            for _ in 0..n {
                let _ = self.frame(caret);
            }
        }

        fn hold(&mut self, caret: Option<(u16, u16)>, secs: f32) {
            let n = (secs / self.frame_dur.as_secs_f32()).round();
            assert!((0.0..1e6).contains(&n), "probe: bad hold {secs}");
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            self.hold_frames(caret, n as u32);
        }

        fn log_rows(&mut self, frames: u32, burst: bool, above: bool) {
            let last = PROBE_ROWS - 1;
            for k in 0..frames {
                let at_end = k % 2 == 0;
                if above {
                    self.ink(last - 1, 0, self.cols);
                }
                if at_end {
                    self.ink(last, 0, self.cols - 1);
                } else {
                    self.ink(last, 0, 0);
                }
                self.live = Some(last);
                let caret = if at_end {
                    (last, self.cols - 1)
                } else {
                    (last, 0)
                };
                let mut s = self.sense_at(Some(caret));
                s.output_burst = burst;
                s.wrapped = !at_end;
                let _ = self.frame_with(s, Some(caret));
            }
        }

        fn type_to(&mut self, row: u16, from: u16, to: u16) {
            for c in (from + 1)..=to {
                self.ink(row, 0, c);
                let _ = self.frame(Some((row, c)));
                self.hold_frames(Some((row, c)), 5);
            }
        }

        fn warm(&mut self, row: u16, col0: u16) {
            let start = col0.saturating_sub(16);
            let mid = col0.saturating_sub(8);
            self.live = Some(row);
            self.ink(row, 0, start);
            let _ = self.frame(Some((row, start)));
            self.type_to(row, start, mid);
            self.hold(Some((row, mid)), 2.5);
            self.log_from = self.now();
            self.type_to(row, mid, col0);
        }
    }

    fn offered_star(t: Instant, col: i32) -> StarCatch {
        StarCatch {
            x: col as f32 * 10.0 + 5.0,
            y: 4.0 * 20.0 - 3.0,
            born: t,
            col,
        }
    }

    #[test]
    fn a_run_that_stops_for_a_star_stands_a_stride_first() {
        let stand = [PetGlyphId::PetStand, PetGlyphId::PetStandEar];
        let run = |offer_star: bool| {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let t = awake(&mut pet, start, 4, 10);
            let (mut t, _) = type_run(&mut pet, t, 4, 12, 14, 0.07);
            let mut seen: Vec<(Instant, PetAction, PetGlyphId)> = Vec::new();
            let mut offer: Option<PetOffer> = None;
            for _ in 0..90 {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((4, 26))));
                if offer_star && offer.is_none() && stand.contains(&f.pose) {
                    offer = Some(PetOffer {
                        catch: Some(offered_star(t, 34)),
                        ..PetOffer::default()
                    });
                }
                seen.push((t, f.action, f.pose));
                if let Some(offer) = offer.as_ref() {
                    let _ = pet.note_v2_offer(offer);
                }
            }
            seen
        };
        let bat = PetSpecies::Cat.skin(PetGlyphId::PetBat);
        let with = run(true);
        let swipe = with
            .iter()
            .position(|(_, a, p)| *a == PetAction::Frolic && *p == bat)
            .expect("the star is swiped at");
        let stood_from = with[..swipe]
            .iter()
            .rposition(|(_, _, p)| !stand.contains(p))
            .map_or(0, |i| i + 1);
        assert!(
            stood_from < swipe,
            "fixture: the run stops in its stand before the swipe: {:?}",
            &with[swipe.saturating_sub(4)..=swipe]
        );
        assert!(
            PetBrain::is_leg_pose(with[stood_from - 1].2),
            "fixture: the stand ended the run: {:?}",
            &with[stood_from - 1..=swipe]
        );
        let stood = (with[swipe].0 - with[stood_from].0).as_secs_f32();
        assert!(
            stood >= STAND_MIN - 1e-3,
            "the stand before the swipe lasted {:.0} ms, under a stride ({:.0} ms)",
            stood * 1e3,
            STAND_MIN * 1e3
        );
        let without = run(false);
        assert!(
            without.iter().any(|(_, _, p)| stand.contains(p)),
            "negative control: with no star the run stops in the same stand"
        );
    }

    #[test]
    fn a_play_beat_is_kept_through_the_legs_hold() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        let bat = PetSpecies::Cat.skin(PetGlyphId::PetBat);
        pet.set_action(PetAction::Frolic);
        pet.play_frolic = true;
        pet.play_hold = FROLIC_HOLD * 0.5 + 0.012;
        let mut seen: Vec<(Instant, PetAction, PetGlyphId)> = Vec::new();
        for i in 0..60u16 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12 + (i / 6).min(8)))));
            seen.push((t, f.action, f.pose));
        }
        let first = seen
            .iter()
            .position(|(_, _, p)| *p == bat)
            .expect("fixture: the bow swings into its swipe");
        let next = seen[first..]
            .iter()
            .position(|(_, _, p)| *p != bat)
            .map(|i| first + i)
            .expect("fixture: the cat sets out after the caret");
        let on = (seen[next].0 - seen[first].0).as_secs_f32();
        assert!(
            on >= STILL_SECS - 1e-3,
            "the swipe was on glass {:.0} ms before {:?} replaced it",
            on * 1e3,
            seen[next]
        );
    }

    #[test]
    fn the_meteor_s_perk_reaches_the_resident_at_the_landing_s_own_instant() {
        let start = Instant::now();
        let (mut pet, mut twin, mut t) = settled_pair(start, 5.0);
        assert!(pet.action.settled(), "fixture: settled ({:?})", pet.action);
        let perk_at = t + Duration::from_millis(90);
        let offer = PetOffer {
            perk_at: Some(perk_at),
            ..PetOffer::default()
        };
        let mut perk: Option<(Instant, PetFrame, PetFrame)> = None;
        let mut twin_perked = false;
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 12))));
            let g = twin.tick(sense(t, Some((4, 12))));
            assert!(pet.note_v2_offer(&offer).is_none(), "a perk is not a catch");
            twin_perked |= g.action == PetAction::Perk;
            if f.action == PetAction::Perk && perk.is_none() {
                perk = Some((t, f, g));
            }
        }
        let (at, f, g) = perk.expect("the offered perk edge reaches the pet");
        assert!(
            !twin_perked,
            "fixture: nothing but the offer can perk this cat"
        );
        assert!(
            at >= perk_at,
            "the perk landed {:?} BEFORE the arrival — that is the launch, not the landing",
            perk_at - at
        );
        assert!(
            at - perk_at < Duration::from_millis(17),
            "the perk landed {:?} after the arrival — later than the first frame at t0+T",
            at - perk_at
        );
        assert_eq!(
            (f.col.to_bits(), f.row.to_bits(), f.lift.to_bits()),
            (g.col.to_bits(), g.row.to_bits(), g.lift.to_bits()),
            "NO RELOCATION (D13, owner Q14): the offered cat is at ({}, {}) and the \
             un-offered twin at ({}, {}) on the arrival's own frame",
            f.col,
            f.row,
            g.col,
            g.row
        );
    }

    #[test]
    fn a_gold_star_near_a_contented_cat_is_offered_caught_and_spent_once() {
        let start = Instant::now();
        let (mut pet, mut twin, mut t) = contented_pair(start, SIT_AFTER + 0.3);
        assert!(pet.purring(), "fixture: a contented cat ({:?})", pet.action);
        let star = offered_star(t, pet.col.round() as i32 + 4);
        let offer = PetOffer {
            catch: Some(star),
            ..PetOffer::default()
        };
        let mut caught: Vec<(PetFrame, PetFrame)> = Vec::new();
        let mut motes = 0usize;
        for _ in 0..300 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 72))));
            let g = twin.tick(sense(t, Some((4, 72))));
            if let Some(back) = pet.note_v2_offer(&offer) {
                assert_eq!(back, star, "the star handed back is the star offered");
                motes = f.motes.iter().flatten().count();
                caught.push((f, g));
            }
        }
        assert_eq!(
            caught.len(),
            1,
            "the offer stood for 300 frames and the star's life may shorten ONCE: {} catches",
            caught.len()
        );
        let (f, g) = caught[0];
        assert_eq!(
            f.action,
            PetAction::Frolic,
            "the catch is played as the reach, not taken invisibly"
        );
        assert_eq!(
            f.pose,
            PetSpecies::Cat.skin(PetGlyphId::PetBat),
            "the paw is out on the landing frame"
        );
        assert_eq!(
            motes,
            g.motes.iter().flatten().count(),
            "the reach draws NOTHING: it spawned a mote the un-offered twin did not"
        );
        assert_eq!(f.row.to_bits(), g.row.to_bits(), "the reach changed lines");
        assert!(
            (f.col - g.col).abs() < 0.05 && f.lift.to_bits() == g.lift.to_bits(),
            "a reach is not a trip: the reaching cat is at ({}, {}) and the un-offered \
             twin at ({}, {})",
            f.col,
            f.row,
            g.col,
            g.row
        );
        assert!(
            pet.flight.is_none()
                && pet.play_to.is_none()
                && pet.play_land.is_none()
                && !pet.pending_pounce
                && !pet.pending_big_jump,
            "the catch set travel: flight {:?}, to {:?}, land {:?}",
            pet.flight.is_some(),
            pet.play_to,
            pet.play_land.is_some()
        );
    }

    #[test]
    fn the_purr_notes_wear_the_ribbon_s_colour_under_the_cat() {
        const RIBBON: u32 = 0x0000_7FFF;
        let start = Instant::now();
        let (mut lit, mut dark, mut t) = contented_pair(start, SIT_AFTER + 0.3);
        let under = PetOffer {
            mote_t: Some(0.5),
            mote_rgb: Some(RIBBON),
            ..PetOffer::default()
        };
        let none = PetOffer::default();
        let (mut lit_note, mut dark_note) = (None, None);
        for _ in 0..800 {
            t += Duration::from_millis(16);
            let a = lit.tick(sense(t, Some((4, 72))));
            let b = dark.tick(sense(t, Some((4, 72))));
            assert!(lit.note_v2_offer(&under).is_none());
            assert!(dark.note_v2_offer(&none).is_none());
            let purr = |f: &PetFrame| {
                f.motes
                    .iter()
                    .flatten()
                    .find(|m| matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart))
                    .copied()
            };
            if lit_note.is_none() {
                lit_note = purr(&a);
            }
            if dark_note.is_none() {
                dark_note = purr(&b);
            }
            if lit_note.is_some() && dark_note.is_some() {
                break;
            }
        }
        let lit_note = lit_note.expect("fixture: the lit seat floats a ♪ or ♥");
        let dark_note = dark_note.expect("fixture: the dark seat floats a ♪ or ♥");
        assert_eq!(
            lit_note.rgb,
            Some(RIBBON),
            "a {:?} over a lit ribbon wears the ribbon's stop",
            lit_note.kind
        );
        assert_eq!(
            dark_note.rgb, None,
            "a {:?} with no ribbon under the cat keeps the pet's own colour",
            dark_note.kind
        );
    }

    #[test]
    fn the_offer_buys_no_wake() {
        let start = Instant::now();
        let (mut pet, _twin, mut t) = settled_pair(start, 5.0);
        assert!(!pet.needs_frames(), "fixture: a parked cat");
        let horizon = pet
            .next_change_deadline(t)
            .expect("fixture: a settled cat names its next step");
        let perk_at = t + Duration::from_millis(400);
        let offer = PetOffer {
            perk_at: Some(perk_at),
            catch: Some(offered_star(t, pet.col.round() as i32 + 3)),
            mote_t: Some(0.5),
            mote_rgb: Some(0x00FF_0000),
        };
        assert!(
            pet.note_v2_offer(&offer).is_none(),
            "noting is not catching"
        );
        assert!(
            !pet.needs_frames(),
            "an offer must never pin the frame lane"
        );
        assert_eq!(
            pet.next_change_deadline(t),
            Some(horizon),
            "an offer must never move the idle horizon"
        );
        let mut reached = false;
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((4, 12))));
            reached |= pet.note_v2_offer(&offer).is_some();
        }
        assert!(reached, "a latch that buys no wake is still a latch");
    }

    fn drive_room(
        pet: &mut PetBrain,
        start: Instant,
        caret: (u16, u16),
        secs: f32,
        executing: bool,
    ) -> (u32, bool, Option<Instant>, Instant) {
        let end = start + Duration::from_secs_f32(secs);
        let (mut wakes, mut slept, mut crouched) = (0u32, false, None);
        let mut t = start;
        while t < end {
            let next = if pet.needs_frames() {
                t + Duration::from_millis(16)
            } else {
                let Some(d) = pet.next_change_deadline(t) else {
                    break;
                };
                assert!(d > t, "a deadline at `now` is a busy re-arm");
                d
            };
            t = next;
            wakes += 1;
            pet.note_executing(t, executing);
            let f = pet.tick(sense(t, Some(caret)));
            slept |= f.action == PetAction::Sleep;
            if pet.vigil && pet.action == PetAction::Crouch && crouched.is_none() {
                crouched = Some(t);
            }
        }
        (wakes, slept, crouched, t)
    }

    #[test]
    fn a_driven_session_s_cat_keeps_its_distance_and_walks_back_at_release() {
        let start = Instant::now();
        let (mut pet, mut twin, t0) = settled_pair(start, 3.0);
        pet.note_room_lease(true);
        let mut t = t0;
        let mut step_max = 0.0f32;
        let mut last = pet.col;
        for k in 1..=6u16 {
            let until = t + Duration::from_millis(120);
            while t < until {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((4, 12 + k - 1))));
                let _ = twin.tick(sense(t, Some((4, 12 + k - 1))));
                step_max = step_max.max((f.col - last).abs());
                last = f.col;
            }
        }
        let (t1, f) = idle(&mut pet, t, (4, 18), LOAF_AFTER - 0.3);
        let (_, g) = idle(&mut twin, t, (4, 18), LOAF_AFTER - 0.3);
        let t = t1;
        assert!(
            (f.col - g.col - LEASE_STANDOFF).abs() < 0.6,
            "the leased cat stands {} cells past the twin (wanted {LEASE_STANDOFF})",
            f.col - g.col
        );
        assert_eq!(
            f.pose,
            PetSpecies::Cat.skin(PetGlyphId::PetPerk),
            "…wearing the perk on the caret, not the seat ({:?} / {:?})",
            f.action,
            f.pose
        );
        assert!(
            g.action.settled() && g.pose != PetGlyphId::PetPerk,
            "the twin sits"
        );
        assert!(
            step_max <= 1.0 && pet.flight.is_none(),
            "the far stand is walked to, never jumped: max step {step_max} cells"
        );
        pet.note_room_lease(false);
        let mut t = t;
        let mut walked = false;
        let mut airborne = false;
        let mut step_max = 0.0f32;
        let mut last = pet.col;
        let end = t + Duration::from_secs(3);
        while t < end {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 18))));
            let _ = twin.tick(sense(t, Some((4, 18))));
            walked |= matches!(f.action, PetAction::Walk | PetAction::Run);
            airborne |= f.action.airborne();
            step_max = step_max.max((f.col - last).abs());
            last = f.col;
        }
        assert!(
            walked && !airborne,
            "the walk home is a walk (walked {walked}, airborne {airborne})"
        );
        assert!(
            (pet.col - twin.col).abs() < 0.3,
            "…and it arrives beside the twin: {} vs {}",
            pet.col,
            twin.col
        );
        assert!(
            step_max <= 1.0,
            "no teleport on the way home: max step {step_max}"
        );
    }

    #[test]
    fn a_running_job_keeps_the_cat_awake_with_you() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, _) = type_run(&mut pet, start, 10, 20, 6, 0.12);
        let launched = t;
        for _ in 0..60 {
            t += Duration::from_millis(16);
            pet.note_executing(t, true);
            let mut s = sense(t, Some((10, 20)));
            s.output_burst = true;
            let _ = pet.tick(s);
        }
        assert!(
            pet.watch_heat >= WATCH_GATE,
            "fixture: the stream earned the stare"
        );
        pet.note_room_quiet(true);
        let (wakes, slept, crouched, t) = drive_room(&mut pet, t, (10, 20), 40.0, true);
        assert!(!slept, "the cat slept on a running job");
        let at = crouched.expect("the vigil crouch was never entered");
        let since = at.saturating_duration_since(launched).as_secs_f32();
        assert!(
            since <= VIGIL_AFTER + 1.5,
            "the vigil is taken at the half-minute mark, not {since:.1} s"
        );
        assert!(
            pet.vigil && pet.action == PetAction::Crouch,
            "…and held ({:?})",
            pet.action
        );
        assert!(
            !pet.needs_frames() && pet.next_change_deadline(t).is_none(),
            "the held crouch claims no frame and offers nothing"
        );
        assert!(
            wakes <= 120,
            "a quiet job is a handful of wakes and then the crouch, not a train: {wakes}"
        );
        pet.note_command_done(t, false, Some(40_000));
        pet.note_room_quiet(false);
        let mut t = t;
        let mut left_the_ground = false;
        for _ in 0..200 {
            t += Duration::from_millis(16);
            pet.note_executing(t, false);
            left_the_ground |= pet.tick(sense(t, Some((10, 20)))).action.airborne();
        }
        assert!(left_the_ground, "the verdict still buys the strike");
    }

    #[test]
    fn a_sibling_pane_s_burst_earns_one_look() {
        let start = Instant::now();
        let (mut pet, mut twin, mut t) = settled_pair(start, 5.0);
        let mut looks = 0u32;
        let mut first_facing = None;
        let mut was_perk = false;
        let mut f = pet.tick(sense(t, Some((4, 12))));
        for _ in 0..90 {
            t += Duration::from_millis(16);
            pet.note_room_sibling(t, Some((-20.0, 3.0)));
            f = pet.tick(sense(t, Some((4, 12))));
            let g = twin.tick(sense(t, Some((4, 12))));
            if f.action == PetAction::Perk && !was_perk {
                looks += 1;
                first_facing.get_or_insert(f.facing_left);
            }
            was_perk = f.action == PetAction::Perk;
            assert!(
                (f.col - g.col).abs() < 0.05 && f.row.to_bits() == g.row.to_bits(),
                "a look is not a trip"
            );
        }
        assert_eq!(looks, 1, "one rising edge, one look ({looks})");
        assert_eq!(first_facing, Some(true), "…toward the pane that is talking");
        assert!(
            f.action.settled() && f.action != PetAction::Perk,
            "and then back to its seat"
        );
    }

    #[test]
    fn an_unseen_inbox_row_perks_the_cat_and_a_hold_on_sits_it() {
        let start = Instant::now();
        let (mut pet, mut twin, mut t) = settled_pair(start, 2.0);
        pet.note_room_inbox(t, RoomInbox::default());
        let mail = RoomInbox {
            unseen: 7,
            overdue: false,
            hold: false,
        };
        let (mut perks, mut notes, mut twitched, mut was_perk) = (0u32, 0u32, false, false);
        for _ in 0..60 {
            t += Duration::from_millis(16);
            pet.note_room_inbox(t, mail);
            let f = pet.tick(sense(t, Some((4, 12))));
            let g = twin.tick(sense(t, Some((4, 12))));
            if f.action == PetAction::Perk && !was_perk {
                perks += 1;
            }
            was_perk = f.action == PetAction::Perk;
            let note = |f: &PetFrame| {
                f.motes
                    .iter()
                    .flatten()
                    .filter(|m| m.kind == PetMoteKind::Note)
                    .count()
            };
            notes = notes.max((note(&f).saturating_sub(note(&g))) as u32);
            twitched |= pet.twitch_t > 0.0;
        }
        assert_eq!(perks, 1, "one letter, one perk ({perks})");
        assert_eq!(notes, 1, "…and one ♪ of its own ({notes})");
        assert!(twitched, "…with one ear");
        let ledger = pet.content;
        let overdue = RoomInbox {
            overdue: true,
            ..mail
        };
        let mut flat = false;
        for _ in 0..40 {
            t += Duration::from_millis(16);
            pet.note_room_inbox(t, overdue);
            let f = pet.tick(sense(t, Some((4, 12))));
            let _ = twin.tick(sense(t, Some((4, 12))));
            flat |= f.action == PetAction::Droop && pet.droop_brief;
        }
        assert!(flat, "a passed deadline flattens the ears");
        assert!(
            (pet.content - ledger).abs() < 1e-6 && !pet.pending_sulk,
            "…with none of the sulk: ledger {ledger} → {}",
            pet.content
        );
        let hold = RoomInbox {
            hold: true,
            ..overdue
        };
        pet.note_room_inbox(t, hold);
        let (t, f) = idle(&mut pet, t, (4, 12), 10.0);
        let (_, g) = idle(&mut twin, t, (4, 12), 10.0);
        assert_eq!(f.action, PetAction::Sit, "sits tight ({:?})", f.action);
        assert_ne!(
            g.action,
            PetAction::Sit,
            "fixture: the twin has melted ({:?})",
            g.action
        );
        let (t, f) = idle(&mut pet, t, (4, 12), SLEEP_AFTER);
        assert_eq!(f.action, PetAction::Sleep, "a halt does not withhold sleep");
        let (t, _) = idle(&mut pet, t, (4, 12), BREATH_WINDOW + 1.0);
        assert!(
            !pet.needs_frames() && pet.next_change_deadline(t).is_none(),
            "idle → zero, hold or no hold"
        );
    }

    #[test]
    fn the_room_s_facts_buy_no_wake() {
        let start = Instant::now();
        let (mut pet, _twin, t) = settled_pair(start, 5.0);
        assert!(!pet.needs_frames(), "fixture: a parked cat");
        let horizon = pet
            .next_change_deadline(t)
            .expect("fixture: a settled cat names its next step");
        pet.note_room_turn(t, 3, true);
        pet.note_room_inbox(t, RoomInbox::default());
        let check = |pet: &PetBrain, what: &str| {
            assert!(!pet.needs_frames(), "{what} pinned the frame train");
            let d = pet.next_change_deadline(t);
            assert!(
                d.is_none_or(|d| d >= horizon),
                "{what} moved the idle horizon sooner: {:?} vs {horizon:?}",
                d
            );
        };
        pet.note_room_lease(true);
        check(&pet, "a lease");
        pet.note_room_quiet(true);
        check(&pet, "a quiet job");
        pet.note_room_turn(t, 4, true);
        check(&pet, "a settled turn");
        pet.note_room_sibling(t, Some((30.0, 2.0)));
        check(&pet, "a talking sibling");
        pet.note_room_inbox(
            t,
            RoomInbox {
                unseen: 9,
                overdue: false,
                hold: true,
            },
        );
        check(&pet, "unread mail under a hold");
        assert!(pet.pending_room.is_some(), "…and the latch is real");
        let mut t = t;
        let mut answered = false;
        for _ in 0..40 {
            t += Duration::from_millis(16);
            answered |= pet.tick(sense(t, Some((4, 12)))).action == PetAction::Perk;
        }
        assert!(answered, "a latch that buys no wake is still a latch");
    }

    #[test]
    fn the_catch_ttl_is_the_sky_m1_s_catchable_life() {
        use crate::rainbow_kitty::stardust::{CATCH_FINISH_MS, StarClass, StarLane};
        let life = StarClass::M1.life_s(StarLane::Strike);
        let catchable = life - CATCH_FINISH_MS / 1000.0;
        assert!(
            (V2_CATCH_TTL - catchable).abs() <= f32::EPSILON,
            "V2_CATCH_TTL {V2_CATCH_TTL} is not the sky m1's catchable life {catchable} \
             (life {life}, finish {CATCH_FINISH_MS} ms)"
        );
    }

    fn one_key(pet: &mut PetBrain, t: Instant, from: (u16, u16)) -> Instant {
        let t = t + Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((from.0, from.1 + 1))));
        t
    }

    fn watch(
        pet: &mut PetBrain,
        start: Instant,
        caret: (u16, u16),
        secs: f32,
    ) -> (Instant, Vec<PetFrame>) {
        let mut t = start;
        let end = start + Duration::from_secs_f32(secs);
        let mut frames = Vec::new();
        while t < end {
            t += Duration::from_millis(16);
            frames.push(pet.tick(sense(t, Some(caret))));
        }
        (t, frames)
    }

    #[test]
    fn a_trick_latch_arms_and_releases_the_lane_and_survives_a_hold_and_a_flight() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (t, _) = type_run(&mut pet, start, 4, 10, 6, 0.05);
        let (t, _) = idle(&mut pet, t, (4, 16), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert!(!pet.needs_frames(), "fixture: deeply asleep, lane released");
        pet.note_trick(t, Trick::Jump, true);
        assert_eq!(pet.pending_trick(), Some(Trick::Jump));
        assert!(pet.needs_frames(), "a latched trick re-arms the lane");
        let (t, frames) = watch(&mut pet, t, (4, 16), 6.0);
        assert_eq!(pet.pending_trick(), None, "performed on the ground");
        assert!(
            frames.iter().any(|f| f.action == PetAction::Waking),
            "a sleeping cat is addressed: it wakes first, and keeps the latch"
        );
        assert!(
            frames.iter().any(|f| f.action == PetAction::Leap),
            "…then it jumps"
        );
        let (_, f) = idle(&mut pet, t, (4, 16), SLEEP_AFTER + BREATH_WINDOW + 2.0);
        assert_eq!(f.action, PetAction::Sleep);
        assert!(
            !pet.needs_frames(),
            "every envelope the trick rode is finite: the lane releases again"
        );

        let mut pet = PetBrain::default();
        let (t, _) = type_run(&mut pet, start, 2, 10, 8, 0.06);
        let (mut t, _) = idle(&mut pet, t, (2, 20), 0.5);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((2, 15))));
        assert_eq!(f.action, PetAction::Startle, "fixture: the hold is live");
        pet.note_trick(t, Trick::Roll, true);
        let (_, frames) = watch(&mut pet, t, (2, 15), 3.0);
        assert!(
            frames.iter().any(|f| f.pose == PetGlyphId::PetRoll1),
            "the latch outlives the startle hold and the walk home"
        );

        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 40))));
        pet.note_trick(t, Trick::Roll, true);
        let (_, frames) = watch(&mut pet, t, (4, 40), 3.5);
        let landed = frames
            .iter()
            .position(|f| f.action == PetAction::Land)
            .expect("fixture: the jump lands inside the window");
        let rolled = frames
            .iter()
            .position(|f| f.pose == PetGlyphId::PetRoll1)
            .expect("the latch outlives the flight");
        assert!(rolled > landed, "and the verb waits for the ground");
    }

    #[test]
    fn a_tentative_trick_settles_at_one_second_and_a_confirmed_one_at_seven_tenths() {
        let heard_after = |confirmed: bool| -> f32 {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let t = awake(&mut pet, start, 4, 10);
            let key = one_key(&mut pet, t, (4, 12));
            pet.note_trick(key, Trick::Play, confirmed);
            let mut t = key;
            for _ in 0..120 {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((4, 13))));
                if pet.pending_trick().is_none() {
                    assert_eq!(
                        f.action,
                        PetAction::Perk,
                        "the latch is consumed INTO the heard-you beat"
                    );
                    return (t - key).as_secs_f32();
                }
                assert_ne!(
                    f.action,
                    PetAction::Frolic,
                    "nothing plays while the latch still waits"
                );
            }
            panic!("the trick was never heard (confirmed = {confirmed})");
        };
        let confirmed = heard_after(true);
        let tentative = heard_after(false);
        assert!(
            (TRICK_SETTLE..TRICK_SETTLE + 0.04).contains(&confirmed),
            "confirmed: heard {confirmed} s after the last key, want {TRICK_SETTLE}"
        );
        assert!(
            (TRICK_SETTLE_TENTATIVE..TRICK_SETTLE_TENTATIVE + 0.04).contains(&tentative),
            "tentative: heard {tentative} s after the last key, want {TRICK_SETTLE_TENTATIVE}"
        );
    }

    #[test]
    fn a_revoke_drops_only_a_still_pending_tentative_latch() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.revoke_trick();
        assert_eq!(pet.pending_trick(), None, "a revoke of nothing is a no-op");

        pet.note_trick(t, Trick::Roll, false);
        pet.revoke_trick();
        assert_eq!(pet.pending_trick(), None, "a tentative latch is taken back");
        assert!(!pet.needs_frames(), "…and the lane goes with it");
        let (t, frames) = watch(&mut pet, t, (4, 12), 2.0);
        assert!(
            frames.iter().all(|f| f.pose != PetGlyphId::PetRoll1),
            "a revoked trick is never performed"
        );

        pet.note_trick(t, Trick::Roll, true);
        pet.revoke_trick();
        assert_eq!(
            pet.pending_trick(),
            Some(Trick::Roll),
            "a confirmed request is not the listener's to take back"
        );

        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.note_trick(t, Trick::Sit, false);
        pet.note_trick(t, Trick::Sit, true);
        pet.note_trick(t, Trick::Sit, false);
        pet.revoke_trick();
        assert_eq!(pet.pending_trick(), Some(Trick::Sit), "upgraded, and kept");
        pet.note_trick(t, Trick::Jump, false);
        assert_eq!(pet.pending_trick(), Some(Trick::Jump));
    }

    #[test]
    fn a_revoked_trick_leaves_the_frames_byte_identical() {
        let run = |tell: bool| -> Vec<u64> {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let mut t = awake(&mut pet, start, 4, 10);
            let mut fps = Vec::new();
            for (i, (row, col)) in [(4u16, 13u16), (4, 14), (4, 15), (5, 2), (5, 3)]
                .into_iter()
                .enumerate()
            {
                if tell && i == 1 {
                    pet.note_trick(t, Trick::Roll, false);
                }
                if tell && i == 2 {
                    pet.revoke_trick();
                }
                for _ in 0..30 {
                    t += Duration::from_millis(16);
                    fps.push(pet.tick(sense(t, Some((row, col)))).fp());
                }
            }
            for _ in 0..400 {
                t += Duration::from_millis(16);
                fps.push(pet.tick(sense(t, Some((5, 3)))).fp());
            }
            fps
        };
        assert_eq!(run(false), run(true));
    }

    #[test]
    fn a_trick_expires_four_seconds_after_the_last_key_and_never_from_caret_moves() {
        let drive = |typing: bool| -> (f32, bool) {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let t0 = awake(&mut pet, start, 4, 10);
            pet.note_trick(t0, Trick::Roll, true);
            let mut t = t0;
            let mut col = 12u16;
            let mut gone = None;
            let mut rolled = false;
            for step in 0..1500u32 {
                t += Duration::from_millis(16);
                if step % 6 == 0 {
                    col = if col == 12 { 13 } else { 12 };
                    if typing {
                        pet.note_console_input(t, PetInputKind::Text);
                    }
                }
                let f = pet.tick(sense(t, Some((4, col))));
                rolled |= f.pose == PetGlyphId::PetRoll1;
                if gone.is_none() && pet.pending_trick().is_none() {
                    gone = Some((t - t0).as_secs_f32());
                }
            }
            (gone.expect("the latch is finite by construction"), rolled)
        };
        let (gone, rolled) = drive(false);
        assert!(
            (TRICK_TTL..TRICK_TTL + 0.1).contains(&gone),
            "streamed caret moves re-stamp nothing: gone at +{gone} s, want {TRICK_TTL}"
        );
        assert!(!rolled, "and an expired trick is never performed");
        let (gone, rolled) = drive(true);
        assert!(
            (TRICK_CAP..TRICK_CAP + 0.1).contains(&gone),
            "the user's own keys re-stamp it, up to the cap: gone at +{gone} s, want {TRICK_CAP}"
        );
        assert!(!rolled, "the hand never stopped, so the gate never opened");
    }

    #[test]
    fn a_typed_word_and_enter_plays_at_the_new_prompt() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (t, _) = type_run(&mut pet, start, 4, 2, 24, 0.07);
        pet.note_trick(t, Trick::Sit, true);
        let mut t = t + Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((5, 2))));
        assert_eq!(
            pet.pending_trick(),
            Some(Trick::Sit),
            "the snap home clears play intents, never an owed trick"
        );
        let mut flew = false;
        let mut sat_on = None;
        for _ in 0..240 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((5, 2))));
            flew |= f.action.airborne();
            if f.pose == PetGlyphId::PetSitLookup {
                sat_on = Some((f.row, flew, pet.flight.is_none()));
                break;
            }
            if f.action.airborne() {
                assert_eq!(
                    pet.pending_trick(),
                    Some(Trick::Sit),
                    "work outranks a trick: the latch waits out the hop"
                );
            }
        }
        let (row, flew, grounded) = sat_on.expect("the commanded sit plays");
        assert!(flew && grounded, "after the row hop, on the ground");
        assert!((row - 5.0).abs() < 0.01, "at the NEW prompt: row {row}");
    }

    #[test]
    fn a_trick_typed_at_speed_is_performed_where_the_cat_stands() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        let (t, _) = type_run(&mut pet, t, 4, 12, 18, 0.07);
        assert!(
            pet.lead() > 0.5,
            "fixture: the run opened the lead ({})",
            pet.lead()
        );
        pet.note_trick(t, Trick::Roll, true);
        let (_, frames) = watch(&mut pet, t, (4, 30), 4.0);
        let heard = frames
            .iter()
            .position(|f| f.action == PetAction::Perk)
            .expect("heard");
        assert!(
            frames[heard..]
                .iter()
                .any(|f| f.pose == PetGlyphId::PetRoll1),
            "a request that is heard is answered"
        );
    }

    #[test]
    fn a_hidden_caret_reduced_motion_and_a_hold_on_each_drop_a_trick() {
        let start = Instant::now();

        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        pet.note_trick(t, Trick::Roll, true);
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, None));
        assert_eq!(pet.pending_trick(), None, "no caret: dropped at once");
        let (_, frames) = watch(&mut pet, t, (4, 12), 3.0);
        assert!(
            frames.iter().all(|f| f.pose != PetGlyphId::PetRoll1),
            "and never replayed when the caret returns"
        );
        let mut pet = PetBrain::default();
        pet.note_trick(start, Trick::Jump, true);
        let mut t = start;
        for _ in 0..4 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, None));
            assert_eq!(f.alpha, 0, "a typed word never summons");
        }
        assert_eq!(pet.pending_trick(), None);
        assert!(
            !pet.needs_frames(),
            "and a hidden cat owes no frames for it"
        );

        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.note_room_inbox(t, RoomInbox::default());
        pet.note_room_inbox(
            t,
            RoomInbox {
                hold: true,
                ..RoomInbox::default()
            },
        );
        pet.note_trick(t, Trick::Roll, true);
        let (_, frames) = watch(&mut pet, t, (4, 12), 3.0);
        assert_eq!(pet.pending_trick(), None);
        assert!(
            frames.iter().all(|f| f.pose != PetGlyphId::PetRoll1),
            "under a hold the cat sits tight: no games, no tricks"
        );

        let reduced = |t: Instant| {
            let mut s = sense(t, Some((3, 10)));
            s.reduced_motion = true;
            s
        };
        let mut pet = PetBrain::default();
        let mut t = start;
        for _ in 0..40 {
            t += Duration::from_millis(16);
            let _ = pet.tick(reduced(t));
        }
        t += Duration::from_millis(16);
        let mut s = reduced(t);
        s.caret = Some((3, 11));
        let _ = pet.tick(s);
        let still = |t: Instant| {
            let mut s = reduced(t);
            s.caret = Some((3, 11));
            s
        };
        pet.note_trick(t, Trick::Jump, true);
        for _ in 0..90 {
            t += Duration::from_millis(16);
            let f = pet.tick(still(t));
            assert_eq!(
                f.action,
                PetAction::Sit,
                "no choreography under reduced motion"
            );
            assert_eq!((f.scale_x, f.scale_y, f.lift), (1.0, 1.0, 0.0));
            assert!(f.motes.iter().all(Option::is_none));
        }
        assert_eq!(pet.pending_trick(), None, "the latch clears as bookkeeping");
        assert!(!pet.needs_frames());
        pet.note_trick(t, Trick::Sleep, true);
        t += Duration::from_millis(16);
        let f = pet.tick(still(t));
        assert_eq!(
            f.action,
            PetAction::Sleep,
            "a commanded sleep is a pose swap"
        );
        assert_eq!(f.pose, PetGlyphId::PetSleep0);
        assert_eq!(pet.pending_trick(), None);
        assert!(pet.trick_settle.is_none() && !pet.needs_frames());
    }

    #[test]
    fn a_commanded_sleep_ends_byte_stable_and_rides_the_offer() {
        let start = Instant::now();
        let (mut pet, mut twin, t) = settled_pair(start, SIT_AFTER + 0.3);
        pet.note_trick(t, Trick::Sleep, true);
        twin.note_trick(t, Trick::Sleep, true);
        let (_, end) = walk_offers(&mut pet, &mut twin, t, (4, 12), 60.0);
        assert_eq!(pet.action, PetAction::Sleep);
        assert!(
            (end - t).as_secs_f32() < BREATH_WINDOW + 3.0,
            "asleep within the beat, deep within the breath window: {:?}",
            end - t
        );
        assert!(
            pet.quiet >= SLEEP_AFTER + BREATH_WINDOW,
            "the walk ends in deep sleep, not on a timeout: quiet {}",
            pet.quiet
        );
        assert!(pet.next_change_deadline(end).is_none() && !pet.needs_frames());
        let a = pet.tick(sense(end + Duration::from_secs(5), Some((4, 12))));
        let b = pet.tick(sense(end + Duration::from_secs(9), Some((4, 12))));
        assert_eq!(a.fp(), b.fp(), "deep sleep is byte-stable");
        assert_eq!(a.pose, PetGlyphId::PetSleep0);
        let t = end + Duration::from_secs(10);
        let f = pet.tick(sense(t, Some((4, 13))));
        assert_eq!(f.action, PetAction::Waking);
        assert!(pet.trick_settle.is_none());
    }

    #[test]
    fn reduced_motion_mid_roll_releases_the_lane_and_replays_nothing() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.note_trick(t, Trick::Roll, true);
        let (mut t, _) = idle(&mut pet, t, (4, 12), PERK_HOLD + 0.5);
        assert!(pet.wriggle_t > 1.0 && pet.trick_act, "fixture: mid-roll");
        t += Duration::from_millis(16);
        let mut s = sense(t, Some((4, 12)));
        s.reduced_motion = true;
        let f = pet.tick(s);
        assert_eq!(f.action, PetAction::Sit);
        assert_eq!(pet.wriggle_t, 0.0);
        assert!(!pet.trick_act && !pet.trick_engaged());
        assert!(!pet.needs_frames(), "nothing of the roll holds the lane");
        let (_, frames) = watch(&mut pet, t, (4, 12), 6.0);
        assert!(
            frames.iter().all(|f| !matches!(
                f.pose,
                PetGlyphId::PetRoll0 | PetGlyphId::PetRoll1 | PetGlyphId::PetRoll2
            )),
            "and the shed lifting replays nothing"
        );
    }

    #[test]
    fn a_caret_move_interrupts_a_commanded_seat() {
        for trick in [Trick::Sit, Trick::Down] {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let t = awake(&mut pet, start, 4, 10);
            pet.note_trick(t, trick, true);
            let (t, _) = idle(&mut pet, t, (4, 12), PERK_HOLD + 0.5);
            assert_eq!(pet.trick_settle, Some(trick), "fixture: the seat is held");
            let t = one_key(&mut pet, t, (4, 12));
            assert!(pet.trick_settle.is_none() && pet.hold_end_q == 0.0);
            let (_, f) = idle(&mut pet, t, (4, 13), 0.3);
            assert_ne!(f.pose, PetGlyphId::PetSitLookup);
            assert_ne!(
                f.action,
                PetAction::Loaf,
                "the ladder starts over from the key"
            );
        }
    }

    #[test]
    fn a_roll_interrupted_by_an_enter_jump_releases_the_lane_and_never_replays() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.note_trick(t, Trick::Roll, true);
        let (mut t, frames) = watch(&mut pet, t, (4, 12), PERK_HOLD + 0.6);
        assert!(
            frames.iter().any(|f| f.pose == PetGlyphId::PetRoll0),
            "fixture: the roll is under way"
        );
        assert!(pet.wriggle_t > 1.0, "…with most of it still to play");
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((5, 2))));
        let (t, frames) = watch(&mut pet, t, (5, 2), 1.2);
        assert_eq!(
            pet.wriggle_t, 0.0,
            "the roll's pose was taken away: it is over"
        );
        assert!(
            frames.iter().any(|f| f.action.airborne()),
            "fixture: it hopped"
        );
        let (_, frames) = watch(&mut pet, t, (5, 2), 8.0);
        assert!(
            frames
                .iter()
                .any(|f| matches!(f.action, PetAction::Loaf | PetAction::Purr)),
            "fixture: the long dwell is reached"
        );
        assert!(
            frames.iter().all(|f| !matches!(
                f.pose,
                PetGlyphId::PetRoll0 | PetGlyphId::PetRoll1 | PetGlyphId::PetRoll2
            )),
            "a stale roll never replays"
        );
        assert!(!pet.needs_frames(), "and nothing of it pins the lane");
    }

    #[test]
    fn a_pet_only_submit_forgives_the_fast_failure_that_follows() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 3, 2);
        let (t, _) = type_run(&mut pet, t, 3, 4, 30, 0.05);
        let (t, _) = idle(&mut pet, t, (3, 34), 0.3);
        let c0 = pet.content();
        assert!(c0 > 0.3, "fixture: warmth to lose ({c0})");
        pet.note_trick_submit(t);
        let done = t + Duration::from_millis(40);
        pet.note_command_done(done, true, Some(12));
        assert_eq!(pet.content(), c0, "the ledger does not move");
        assert!(
            !pet.pending_sulk && pet.sulk_at.is_none(),
            "no sulk, and none dated"
        );
        assert!(!pet.grieving(), "and no failure hush either");
        let (t, frames) = watch(&mut pet, done, (3, 34), 2.5);
        assert!(
            frames.iter().all(|f| f.action != PetAction::Droop),
            "the cat does not droop at `command not found: sit`"
        );
        pet.note_command_done(t, false, Some(30));
        assert_eq!(
            pet.pending_cheer, None,
            "nothing was forgiven, so nothing is cheered"
        );
        assert!(pet.trick_submit_at.is_none());
    }

    #[test]
    fn an_unrelated_failure_still_sulks() {
        let warm = || {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let t = awake(&mut pet, start, 3, 2);
            let (t, _) = type_run(&mut pet, t, 3, 4, 30, 0.05);
            let (t, _) = idle(&mut pet, t, (3, 34), 0.3);
            (pet, t)
        };
        let (mut pet, t) = warm();
        let c0 = pet.content();
        pet.note_trick_submit(t);
        pet.note_command_done(t + Duration::from_secs(3), true, Some(12));
        assert!(pet.pending_sulk && pet.sulk_at.is_some());
        assert!(pet.content() < c0);
        let (mut pet, t) = warm();
        pet.note_trick_submit(t);
        pet.note_command_done(t + Duration::from_secs(1), true, Some(CHEER_MIN_MS));
        assert!(
            pet.pending_sulk,
            "a program that really ran and failed is grieved"
        );
        let (mut pet, t) = warm();
        pet.note_trick_submit(t);
        pet.note_command_done(t + Duration::from_millis(100), false, Some(12));
        pet.note_command_done(t + Duration::from_millis(300), true, Some(12));
        assert!(pet.pending_sulk, "one stamp forgives one completion");
        let (mut pet, t) = warm();
        pet.note_trick_submit(t);
        pet.note_command_done(t + Duration::from_millis(100), true, None);
        assert!(!pet.pending_sulk);
    }

    #[test]
    fn a_confirmation_after_the_cat_answered_replays_nothing() {
        let start = Instant::now();
        let rolls = |frames: &[PetFrame]| {
            frames
                .windows(2)
                .filter(|w| {
                    w[0].pose != PetGlyphId::PetRoll0
                        && w[1].pose == PetGlyphId::PetRoll0
                        && w[0].action != PetAction::Loaf
                })
                .count()
        };
        let answered = |pet: &mut PetBrain| -> Instant {
            let t = awake(pet, start, 4, 10);
            pet.note_trick(t, Trick::Roll, false);
            let (t, frames) = watch(pet, t, (4, 12), 4.5);
            assert_eq!(rolls(&frames), 1, "fixture: the tentative roll played");
            t
        };
        let mut pet = PetBrain::default();
        let t = answered(&mut pet);
        pet.note_trick(t, Trick::Roll, true);
        assert_eq!(pet.pending_trick(), None, "one phrase, one trick");
        assert!(!pet.needs_frames());
        let (_, frames) = watch(&mut pet, t, (4, 12), 4.0);
        assert_eq!(rolls(&frames), 0, "and nothing replays");
        pet.note_trick(t, Trick::Roll, true);
        assert_eq!(pet.pending_trick(), Some(Trick::Roll));

        let mut pet = PetBrain::default();
        let t = answered(&mut pet);
        pet.revoke_trick();
        pet.note_trick(t, Trick::Roll, true);
        assert_eq!(pet.pending_trick(), Some(Trick::Roll));

        let mut pet = PetBrain::default();
        let t = answered(&mut pet);
        pet.note_trick(t, Trick::Jump, true);
        assert_eq!(pet.pending_trick(), Some(Trick::Jump));

        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.note_trick(t, Trick::Roll, false);
        let (t, _) = idle(&mut pet, t, (4, 12), 0.1);
        assert!(
            pet.trick_go == Some(Trick::Roll),
            "fixture: heard, verb owed"
        );
        let t = one_key(&mut pet, t, (4, 12));
        let (t, frames) = watch(&mut pet, t, (4, 13), 0.5);
        assert!(
            pet.trick_go.is_none(),
            "the perk was taken away, and the verb with it"
        );
        assert_eq!(rolls(&frames), 0);
        pet.note_trick(t, Trick::Roll, true);
        assert_eq!(pet.pending_trick(), Some(Trick::Roll), "still owed");
        let (_, frames) = watch(&mut pet, t, (4, 13), 3.0);
        assert_eq!(rolls(&frames), 1);
    }

    #[test]
    fn typed_praise_is_rationed_and_a_scold_moves_nothing() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        let mut peak = 0.0f32;
        for round in 0..6 {
            let before = pet.content();
            pet.note_trick(
                t,
                if round % 2 == 0 {
                    Trick::Purr
                } else {
                    Trick::Treat
                },
                true,
            );
            let (t2, _) = idle(&mut pet, t, (4, 12), 2.6);
            t = t2;
            peak = peak.max(pet.content() - before);
        }
        assert!(peak <= TRICK_CONTENT + 1e-4, "one nudge at a time: {peak}");
        assert!(
            pet.content() < PURR_GATE,
            "six praises in sixteen seconds buy at most one nudge: {}",
            pet.content()
        );
        let c0 = pet.content();
        pet.note_trick(t, Trick::Scold, true);
        let (_, frames) = watch(&mut pet, t, (4, 12), 2.0);
        assert!(
            frames.iter().any(|f| f.action == PetAction::Droop),
            "fixture: the ear-flat beat played"
        );
        assert!(
            pet.content() <= c0,
            "a scold takes nothing… ({c0} → {})",
            pet.content()
        );
        assert!(
            c0 - pet.content() < CONTENT_DECAY * 2.1,
            "…beyond the two seconds of ordinary spend"
        );
        assert!(!pet.pending_sulk && pet.sulk_at.is_none() && !pet.grieving());
    }

    #[test]
    fn a_sleeper_is_woken_for_a_trick_but_sleep_is_done_and_stretch_is_the_wake() {
        let start = Instant::now();
        let asleep = || {
            let mut pet = PetBrain::default();
            let (t, _) = type_run(&mut pet, start, 4, 10, 6, 0.05);
            let (t, f) = idle(&mut pet, t, (4, 16), SLEEP_AFTER + 1.0);
            assert_eq!(f.action, PetAction::Sleep, "fixture");
            (pet, t)
        };
        let (mut pet, t) = asleep();
        pet.note_trick(t, Trick::Sleep, true);
        let (_, frames) = watch(&mut pet, t, (4, 16), 1.0);
        assert!(
            frames.iter().all(|f| f.action == PetAction::Sleep),
            "`sleep` to a sleeper is a no-op: never woken to be told to sleep"
        );
        assert_eq!(pet.pending_trick(), None);

        let (mut pet, t) = asleep();
        pet.note_trick(t, Trick::Stretch, true);
        let (_, frames) = watch(&mut pet, t, (4, 16), 3.0);
        let mut wakes = 0;
        let mut prev = PetAction::Sleep;
        for f in &frames {
            wakes += u32::from(f.action == PetAction::Waking && prev != PetAction::Waking);
            prev = f.action;
        }
        assert_eq!(wakes, 1, "`stretch` IS the wake: one stretch, not two");

        let (mut pet, t) = asleep();
        pet.note_trick(t, Trick::Paw, true);
        let (_, frames) = watch(&mut pet, t, (4, 16), 3.0);
        let woke = frames.iter().position(|f| f.action == PetAction::Waking);
        let batted = frames.iter().position(|f| f.pose == PetGlyphId::PetBat);
        assert!(
            woke.is_some() && batted.is_some() && woke < batted,
            "woken first, then the verb: {woke:?} / {batted:?}"
        );
    }

    #[test]
    fn every_trick_is_heard_performs_something_observable_and_releases_the_lane() {
        let start = Instant::now();
        for trick in Trick::ALL {
            let mut pet = PetBrain::default();
            let t = awake(&mut pet, start, 4, 10);
            let mut control = PetBrain::default();
            let tc = awake(&mut control, start, 4, 10);
            assert_eq!(t, tc);
            pet.note_trick(t, trick, true);
            let (end, frames) = watch(&mut pet, t, (4, 12), 5.0);
            let (_, plain) = watch(&mut control, tc, (4, 12), 5.0);
            let heard = frames
                .iter()
                .position(|f| f.action == PetAction::Perk)
                .unwrap_or_else(|| panic!("{trick:?}: never heard"));
            assert!(
                plain.iter().all(|f| f.action != PetAction::Perk),
                "fixture: an untold cat does not perk"
            );
            let after = &frames[heard..];
            let saw = |pose: PetGlyphId| after.iter().any(|f| f.pose == pose);
            let did = |action: PetAction| after.iter().any(|f| f.action == action);
            let mote = |kind: PetMoteKind| {
                after
                    .iter()
                    .any(|f| f.motes.iter().flatten().any(|m| m.kind == kind))
            };
            let early = |action: PetAction, frames: &[PetFrame]| {
                frames[..120].iter().any(|f| f.action == action)
            };
            let held_for = |pred: &dyn Fn(&PetFrame) -> bool| {
                let (mut best, mut run) = (0usize, 0usize);
                for f in after {
                    run = if pred(f) { run + 1 } else { 0 };
                    best = best.max(run);
                }
                best as f32 * 0.016
            };
            let seen = match trick {
                Trick::Sit => saw(PetGlyphId::PetSitLookup),
                Trick::Down => early(PetAction::Loaf, &frames) && !early(PetAction::Loaf, &plain),
                Trick::Sleep => {
                    early(PetAction::Sleep, &frames) && !early(PetAction::Sleep, &plain)
                }
                Trick::Stretch => saw(PetGlyphId::PetStretch) && saw(PetGlyphId::PetStretchHind),
                Trick::Jump => {
                    did(PetAction::Leap) && after.iter().any(|f| f.lift >= ARC_BASE - 0.05)
                }
                Trick::Play => saw(PetGlyphId::PetPlaybow) && saw(PetGlyphId::PetBat),
                Trick::Roll => saw(PetGlyphId::PetRoll0) && saw(PetGlyphId::PetRoll1),
                Trick::Purr => {
                    mote(PetMoteKind::Heart)
                        && held_for(&|f| f.action == PetAction::Purr) >= PET_HOLD - 0.05
                }
                Trick::Groom => held_for(&|f| f.pose == PetGlyphId::PetGroom) >= GROOM_HOLD - 0.05,
                Trick::Speak => mote(PetMoteKind::Note),
                Trick::Sing => {
                    let notes = |f: &PetFrame| {
                        f.motes
                            .iter()
                            .flatten()
                            .filter(|m| m.kind == PetMoteKind::Note)
                            .count()
                    };
                    assert!(
                        plain.iter().all(|f| notes(f) == 0),
                        "fixture: an untold cat sings nothing"
                    );
                    let first = after.iter().position(|f| notes(f) >= 1);
                    let all = after
                        .iter()
                        .position(|f| notes(f) >= usize::from(SONG_NOTES));
                    match (first, all) {
                        (Some(a), Some(b)) => {
                            (b - a) as f32 * 0.016
                                >= (f32::from(SONG_NOTES) - 1.0) * SONG_BEAT - 0.05
                        }
                        _ => false,
                    }
                }
                Trick::Look => {
                    let held = after
                        .iter()
                        .take_while(|f| f.action == PetAction::Perk)
                        .count();
                    held as f32 * 0.016 >= 2.0 * PERK_HOLD - 0.05
                }
                Trick::Paw => {
                    mote(PetMoteKind::Dust)
                        && held_for(&|f| f.pose == PetGlyphId::PetBat) >= BAT_HOLD - 0.05
                }
                Trick::Hide => after
                    .iter()
                    .any(|f| f.action == PetAction::Crouch && f.under_ink),
                Trick::Boo => saw(PetGlyphId::PetStartle),
                Trick::Scold => {
                    held_for(&|f| matches!(f.pose, PetGlyphId::PetDroop | PetGlyphId::PetDroopSit))
                        >= EAR_FLAT_HOLD - 0.05
                }
                Trick::Treat => did(PetAction::Frolic) && mote(PetMoteKind::Note),
            };
            assert!(seen, "{trick:?} performed nothing observable");
            assert_eq!(pet.pending_trick(), None, "{trick:?}: consumed");
            let (_, _) = idle(&mut pet, end, (4, 12), 3.0);
            assert!(!pet.needs_frames(), "{trick:?} left the lane pinned");
            assert!(
                !pet.trick_act && pet.trick_go.is_none(),
                "{trick:?} left a flag up"
            );
        }
    }

    #[test]
    fn a_commanded_hide_walks_behind_a_word_in_reach() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 40);
        let mut spans = vec![(0u16, 0u16); 30];
        spans[4] = (30, 38);
        pet.note_trick(t, Trick::Hide, true);
        let mut hid_at = None;
        for _ in 0..300 {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            let f = pet.tick(sense(t, Some((4, 42))));
            if f.under_ink && f.action == PetAction::Crouch {
                hid_at = Some(f.col);
                break;
            }
        }
        let col = hid_at.expect("the cat hides");
        assert!(
            col < 38.0 && col + art_cols(10, 20) > 30.0,
            "behind the word, not where it stood: col {col}"
        );
        t += Duration::from_millis(16);
        pet.sense_ink(0, &spans, None);
        let _ = pet.tick(sense(t, Some((5, 2))));
        t += Duration::from_millis(16);
        pet.sense_ink(0, &spans, None);
        let f = pet.tick(sense(t, Some((5, 2))));
        assert!(!pet.hiding, "the jump home clears the hide");
        let covered = f
            .body_px(10, 20, 100, 30)
            .is_some_and(|rect| pet.letter_cover(rect, 10, 20) > 0.0);
        assert_eq!(
            f.under_ink, covered,
            "the jump home keeps the hide's place under the glyphs"
        );
    }

    #[test]
    fn a_cat_on_vigil_gets_up_to_answer_and_is_not_dragged_back_mid_seat() {
        let (mut pet, mut t) = at_the_door(Instant::now());
        pet.note_trick(t, Trick::Sit, true);
        let mut looked_up = 0u32;
        for _ in 0..200 {
            t += Duration::from_millis(16);
            pet.note_executing(t, true);
            let f = pet.tick(sense(t, Some((10, 20))));
            looked_up += u32::from(f.pose == PetGlyphId::PetSitLookup);
        }
        assert!(
            looked_up > 100,
            "the commanded seat is held, not cut to a frame: {looked_up} frames"
        );
        for _ in 0..120 {
            t += Duration::from_millis(50);
            pet.note_executing(t, true);
            let _ = pet.tick(sense(t, Some((10, 20))));
        }
        assert!(
            pet.vigil && pet.action == PetAction::Crouch,
            "back at the door"
        );
        assert!(!pet.needs_frames());
    }

    #[test]
    fn a_confirmation_inside_the_heard_you_beat_is_still_owed_and_plays_once() {
        let start = Instant::now();
        let rolls = |frames: &[PetFrame]| {
            frames
                .windows(2)
                .filter(|w| {
                    w[0].pose != PetGlyphId::PetRoll0
                        && w[1].pose == PetGlyphId::PetRoll0
                        && w[0].action != PetAction::Loaf
                })
                .count()
        };
        let heard = |pet: &mut PetBrain| -> Instant {
            let t = awake(pet, start, 4, 10);
            pet.note_trick(t, Trick::Roll, false);
            let (t, _) = idle(pet, t, (4, 12), 0.1);
            assert_eq!(pet.trick_go, Some(Trick::Roll), "fixture: heard, verb owed");
            t
        };
        let mut pet = PetBrain::default();
        let t = heard(&mut pet);
        pet.note_trick(t, Trick::Roll, true);
        let t = t + Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((5, 2))));
        assert!(pet.trick_go.is_none(), "the Enter took the beat's verb");
        assert_eq!(
            pet.pending_trick(),
            Some(Trick::Roll),
            "…and the confirmation still owes it"
        );
        let (_, frames) = watch(&mut pet, t, (5, 2), 4.5);
        assert_eq!(rolls(&frames), 1, "played at the new prompt, once");

        let mut pet = PetBrain::default();
        let t = heard(&mut pet);
        pet.note_trick(t, Trick::Roll, true);
        let (_, frames) = watch(&mut pet, t, (4, 12), 7.0);
        assert_eq!(rolls(&frames), 1, "never twice");
        assert_eq!(pet.pending_trick(), None);
        assert!(!pet.trick_engaged());
    }

    #[test]
    fn a_revoke_inside_the_heard_you_beat_takes_the_verb_back() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.note_trick(t, Trick::Roll, false);
        let (t, _) = idle(&mut pet, t, (4, 12), 0.1);
        assert_eq!(pet.trick_go, Some(Trick::Roll), "fixture: heard, verb owed");
        pet.revoke_trick();
        assert!(!pet.trick_engaged(), "nothing of it is left");
        let (t, frames) = watch(&mut pet, t, (4, 12), 3.0);
        assert!(
            frames.iter().all(|f| !matches!(
                f.pose,
                PetGlyphId::PetRoll0 | PetGlyphId::PetRoll1 | PetGlyphId::PetRoll2
            )),
            "a tentative word taken back mid-look is never performed"
        );
        let (_, _) = idle(&mut pet, t, (4, 12), SLEEP_AFTER + BREATH_WINDOW);
        assert!(!pet.needs_frames(), "and the lane is released");

        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.note_trick(t, Trick::Roll, true);
        let (t, _) = idle(&mut pet, t, (4, 12), 0.1);
        pet.revoke_trick();
        let (_, frames) = watch(&mut pet, t, (4, 12), 3.0);
        assert!(frames.iter().any(|f| f.pose == PetGlyphId::PetRoll1));

        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.note_trick(t, Trick::Roll, false);
        let (t, _) = idle(&mut pet, t, (4, 12), PERK_HOLD + 0.4);
        assert!(pet.wriggle_t > 1.0, "fixture: mid-roll");
        pet.revoke_trick();
        let (_, frames) = watch(&mut pet, t, (4, 12), 1.0);
        assert!(
            frames.iter().all(|f| f.action == PetAction::Loaf),
            "a revoke is not a fright"
        );
    }

    #[test]
    fn a_commanded_hide_behind_a_word_at_the_margin_still_ends() {
        let start = Instant::now();
        for (span, caret_col) in [((0u16, 3u16), 4u16), ((88, 100), 97)] {
            let mut pet = PetBrain::default();
            let mut t = awake(&mut pet, start, 4, caret_col - 2);
            let mut spans = vec![(0u16, 0u16); 30];
            spans[4] = span;
            pet.note_trick(t, Trick::Hide, true);
            let mut hid = false;
            for _ in 0..900 {
                t += Duration::from_millis(16);
                pet.sense_ink(0, &spans, None);
                let f = pet.tick(sense(t, Some((4, caret_col))));
                hid |= f.under_ink && f.action == PetAction::Crouch;
            }
            assert!(hid, "{span:?}: the cat hides");
            assert!(
                pet.hide_to.is_none() && !pet.hiding,
                "{span:?}: and the trip ends"
            );
            assert!(!pet.needs_frames(), "{span:?}: the lane is released");
        }
    }

    #[test]
    fn a_surface_switch_drops_a_trick_and_carries_the_praise_cooldown() {
        let start = Instant::now();
        let treat_bump = |pet: &mut PetBrain, t: Instant, caret: (u16, u16)| -> f32 {
            pet.note_trick(t, Trick::Treat, true);
            let mut t = t;
            let mut bump = 0.0f32;
            for _ in 0..150 {
                let before = pet.content();
                t += Duration::from_millis(16);
                let _ = pet.tick(sense(t, Some(caret)));
                bump = bump.max(pet.content() - before);
            }
            bump
        };
        let mut control = PetBrain::default();
        let t = awake(&mut control, start, 8, 20);
        assert!(
            treat_bump(&mut control, t, (8, 22)) > TRICK_CONTENT * 0.9,
            "control: a first treat is seen landing on the ledger"
        );

        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.note_trick(t, Trick::Purr, true);
        let (t, _) = idle(&mut pet, t, (4, 12), 2.0);
        pet.note_trick(t, Trick::Sit, false);
        let (t, _) = idle(&mut pet, t, (4, 12), PERK_HOLD + 0.2);
        assert!(
            pet.trick_settle == Some(Trick::Sit) && pet.trick_heard.is_some(),
            "fixture: a seat is held, and a tentative word was answered"
        );
        pet.retire_coordinate_space();
        assert!(!pet.trick_engaged() && pet.trick_heard.is_none());
        assert_eq!(pet.hold_end_q, 0.0);
        pet.note_trick(t, Trick::Jump, true);
        pet.retire_coordinate_space();
        assert_eq!(pet.pending_trick(), None, "the latch is the old pane's");
        let t = awake(&mut pet, t, 8, 20);
        let bump = treat_bump(&mut pet, t, (8, 22));
        assert!(
            bump < TRICK_CONTENT * 0.5,
            "the cooldown crossed the switch: a second nudge of {bump}"
        );
    }

    #[test]
    fn a_trick_noted_at_a_retired_cat_meets_the_slow_arm_and_is_dropped() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = start;
        let mut a = pet.tick(sense(t, None));
        while pet.quiet < SLEEP_AFTER + BREATH_WINDOW {
            t += Duration::from_secs(4);
            a = pet.tick(sense(t, None));
        }
        t += Duration::from_millis(16);
        let b = pet.tick(sense(t, None));
        assert_eq!(a.fp(), b.fp(), "fixture: retired, and byte-stable");
        assert!(
            pet.alpha == 0.0 && pet.quiet >= SLEEP_AFTER + BREATH_WINDOW,
            "fixture: the shortcut's own state"
        );
        pet.note_trick(t, Trick::Roll, true);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, None));
        assert_eq!(f.alpha, 0, "a typed word never summons");
        assert_eq!(pet.pending_trick(), None, "dropped by the slow arm");
        assert!(!pet.trick_engaged() && !pet.needs_frames());
        let (_, frames) = watch(&mut pet, t, (4, 12), 3.0);
        assert!(
            frames.iter().all(|f| !matches!(
                f.pose,
                PetGlyphId::PetRoll0 | PetGlyphId::PetRoll1 | PetGlyphId::PetRoll2
            )),
            "and nothing is performed when the caret arrives"
        );
    }

    #[test]
    fn a_flinch_under_a_hiding_cat_ends_the_hide() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.note_trick(t, Trick::Hide, true);
        let (mut t, f) = idle(&mut pet, t, (4, 12), PERK_HOLD + 0.2);
        assert!(
            pet.hiding && f.under_ink && f.action == PetAction::Crouch,
            "fixture: ducked"
        );
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 11))));
        assert_eq!(
            f.action,
            PetAction::Perk,
            "fixture: the flinch took the crouch"
        );
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 11))));
        assert!(
            !pet.hiding && !f.under_ink,
            "the hide is over with its pose"
        );
        let (_, _) = idle(&mut pet, t, (4, 11), SLEEP_AFTER + BREATH_WINDOW + 1.0);
        assert!(!pet.needs_frames(), "and nothing of it pins the lane");
    }

    #[test]
    fn a_cat_waiting_up_with_a_job_sleeps_when_told_to() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (mut t, _) = type_run(&mut pet, start, 10, 20, 6, 0.12);
        for _ in 0..60 {
            t += Duration::from_millis(16);
            pet.note_executing(t, true);
            let mut s = sense(t, Some((10, 20)));
            s.output_burst = true;
            let _ = pet.tick(s);
        }
        pet.note_room_quiet(true);
        let (_, slept, crouched, mut t) = drive_room(&mut pet, t, (10, 20), 40.0, true);
        assert!(
            !slept && crouched.is_some() && pet.waiting_up(),
            "fixture: up with the job, at the door"
        );
        pet.note_trick(t, Trick::Sleep, true);
        let mut asleep = 0u32;
        for _ in 0..240 {
            t += Duration::from_millis(16);
            pet.note_executing(t, true);
            let f = pet.tick(sense(t, Some((10, 20))));
            asleep += u32::from(f.action == PetAction::Sleep);
        }
        assert!(asleep > 150, "told to sleep, it sleeps: {asleep} frames");
        assert!(
            pet.action == PetAction::Sleep && !pet.vigil && pet.waiting_up(),
            "…and the door does not take a sleeping cat back"
        );
    }

    #[test]
    fn a_bell_inside_a_commanded_sit_ends_the_seat() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);
        pet.note_trick(t, Trick::Sit, true);
        let (t, f) = idle(&mut pet, t, (4, 12), PERK_HOLD + 0.5);
        assert_eq!(f.pose, PetGlyphId::PetSitLookup, "fixture: seated");
        pet.note_bell(t);
        let (_, frames) = watch(&mut pet, t, (4, 12), 1.2);
        assert!(
            frames.iter().any(|f| f.action == PetAction::Perk),
            "fixture: the bell's flinch played"
        );
        assert!(pet.trick_settle.is_none() && pet.hold_end_q == 0.0);
        let flinched = frames
            .iter()
            .position(|f| f.action == PetAction::Perk)
            .expect("checked above");
        assert!(
            frames[flinched..]
                .iter()
                .all(|f| f.pose != PetGlyphId::PetSitLookup),
            "the ladder has the cat back, from the bell's own zero"
        );
    }

    #[test]
    fn a_purring_cat_s_commanded_seat_rides_the_offer() {
        let start = Instant::now();
        for trick in [Trick::Sit, Trick::Down] {
            let (mut pet, mut twin, t) = contented_pair(start, SIT_AFTER + 0.3);
            assert!(pet.purring(), "fixture: the tell is on");
            pet.note_trick(t, trick, true);
            twin.note_trick(t, trick, true);
            let (walk, _) = walk_offers(&mut pet, &mut twin, t, (4, 72), 5.0);
            assert!(
                pet.trick_settle.is_none(),
                "{trick:?}: the hold ended inside the walk"
            );
            assert!(walk.offers >= 2, "{trick:?}: held on the offer: {walk:?}");

            let (mut pet, mut twin, t) = contented_pair(start, SIT_AFTER + 2.3);
            assert!(
                pet.purring() && pet.mote_mark >= 1,
                "fixture: past the tell's first beat"
            );
            pet.note_trick(t, trick, true);
            twin.note_trick(t, trick, true);
            let (walk, _) = walk_offers(&mut pet, &mut twin, t, (4, 72), 2.5);
            assert_eq!(pet.trick_settle, Some(trick), "{trick:?}: still held");
            assert!(walk.offers >= 2, "{trick:?}: held on the offer: {walk:?}");
        }
    }

    #[test]
    fn a_seat_commanded_off_its_mark_is_taken_after_the_walk_home() {
        for trick in [Trick::Sit, Trick::Down] {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let t = awake(&mut pet, start, 4, 10);
            pet.note_trick(t, trick, true);
            let (t, _) = idle(&mut pet, t, (4, 12), 0.1);
            assert_eq!(pet.trick_go, Some(trick), "fixture: heard, verb owed");
            pet.col += ARRIVED + 0.6;
            let (_, frames) = watch(&mut pet, t, (4, 12), 2.5);
            let walked = frames
                .iter()
                .position(|f| f.action == PetAction::Walk)
                .unwrap_or_else(|| panic!("{trick:?} fixture: the chase walks it home"));
            assert_eq!(
                pet.trick_settle,
                Some(trick),
                "{trick:?}: the level rode the walk"
            );
            let held = frames[walked..]
                .iter()
                .filter(|f| {
                    if trick == Trick::Down {
                        f.action == PetAction::Loaf
                    } else {
                        f.pose == PetGlyphId::PetSitLookup
                    }
                })
                .count();
            assert!(
                held as f32 * 0.016 > 1.0,
                "{trick:?}: and the seat is taken at home, and held: {held} frames"
            );
        }
    }

    #[test]
    fn ink_arriving_under_the_heard_you_beat_drops_the_owed_verb() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);
        pet.note_trick(t, Trick::Roll, true);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((4, 12))));
        assert!(
            f.action == PetAction::Perk && pet.trick_go == Some(Trick::Roll),
            "fixture: heard, verb owed"
        );
        let mut spans = vec![(0u16, 0u16); 30];
        spans[4] = (0, 40);
        let mut hopped = false;
        let mut rolled = false;
        for _ in 0..400 {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            let f = pet.tick(sense(t, Some((4, 12))));
            hopped |= f.action.airborne();
            rolled |= matches!(
                f.pose,
                PetGlyphId::PetRoll0 | PetGlyphId::PetRoll1 | PetGlyphId::PetRoll2
            );
        }
        assert!(hopped, "fixture: the ink re-anchored the cat");
        assert!(pet.trick_go.is_none(), "the verb went with the perk");
        assert!(!rolled, "and is not performed on the words it fled");
        assert!(!pet.trick_engaged());
    }

    #[test]
    fn a_cat_in_the_air_keeps_its_facing_when_the_caret_goes_back_behind_it() {
        for back in [5u16, 9, SNAP_COLS as u16 + 1] {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let mut t = awake(&mut pet, start, 5, 10);
            let mut caret = 40u16;
            let (mut retreated, mut down_at, mut turned) = (false, None, false);
            for _ in 0..150 {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((5, caret))));
                if f.action == PetAction::Leap {
                    assert!(
                        down_at.is_none() && !f.facing_left,
                        "back {back}: flew facing back at col {} (caret {caret})",
                        f.col
                    );
                    if f.col > f32::from(caret - back) + 1.0 && !retreated {
                        caret -= back;
                        retreated = true;
                    }
                } else if retreated {
                    down_at.get_or_insert(f.col);
                    turned |= f.facing_left;
                }
            }
            let down = down_at.expect("fixture: the caret went back in the air");
            if down > f32::from(caret) + STATION_LEAD + ARRIVED {
                assert!(
                    turned,
                    "back {back}: down at {down}, the cat turns to its stand"
                );
            }
        }
    }

    #[test]
    fn typed_on_through_the_margin_the_wrap_is_one_flight_never_over_the_caret() {
        let width = art_cols(10, 20);
        for frames_per_key in [2u32, 3, 6, 8, 12] {
            for (row, rows) in [(5u16, 30u16), (29, 30)] {
                let start = Instant::now();
                let mut pet = PetBrain::default();
                let mut spans = vec![(0u16, 0u16); usize::from(rows)];
                if row > 0 {
                    spans[usize::from(row - 1)] = (0, 100);
                }
                let mut t = awake(&mut pet, start, row, 70);
                let mut flights = 0;
                let mut over = Vec::new();
                let mut last = pet.tick(sense(t, Some((row, 72))));
                let mut zone = false;
                let mut drive = |pet: &mut PetBrain,
                                 t: &mut Instant,
                                 last: &mut PetFrame,
                                 spans: &[(u16, u16)],
                                 caret: (u16, u16),
                                 wrapped: bool,
                                 counting: bool| {
                    *t += Duration::from_millis(16);
                    pet.sense_ink(0, spans, Some(caret.0));
                    let mut s = sense(*t, Some(caret));
                    s.rows = rows;
                    s.wrapped = wrapped;
                    let f = pet.tick(s);
                    if counting {
                        if f.action == PetAction::Leap && last.action != PetAction::Leap {
                            flights += 1;
                        }
                        let landed = last.action == PetAction::Leap && f.action != PetAction::Leap;
                        let on_caret = (f.row - f32::from(caret.0)).abs() < 0.5
                            && f.col < f32::from(caret.1) + 1.0 - INK_PAD
                            && f.col + width > f32::from(caret.1) + INK_PAD;
                        if landed && on_caret {
                            over.push((f.col, caret));
                        }
                    }
                    assert!(
                        !pet.wall_side,
                        "{frames_per_key} frames a key, row {row}: crossed to the wall side at {caret:?}"
                    );
                    *last = f;
                };
                for c in 73..=99u16 {
                    spans[usize::from(row)] = (0, c);
                    zone |= f32::from(c) + STATION_LEAD + width > 100.0;
                    for _ in 0..frames_per_key {
                        drive(&mut pet, &mut t, &mut last, &spans, (row, c), false, zone);
                    }
                }
                let next = if row + 1 < rows {
                    row + 1
                } else {
                    spans.remove(0);
                    spans.push((0, 0));
                    row
                };
                for (i, c) in (0..=12u16).enumerate() {
                    spans[usize::from(next)] = (0, c);
                    for k in 0..frames_per_key {
                        drive(
                            &mut pet,
                            &mut t,
                            &mut last,
                            &spans,
                            (next, c),
                            i == 0 && k == 0 && next == row,
                            true,
                        );
                    }
                }
                for _ in 0..90 {
                    drive(&mut pet, &mut t, &mut last, &spans, (next, 12), false, true);
                }
                assert_eq!(
                    flights, 1,
                    "{frames_per_key} frames a key, row {row}: {flights} flights for one wrap"
                );
                assert!(
                    over.is_empty(),
                    "{frames_per_key} frames a key, row {row}: landed over the caret {over:?}"
                );
                assert!(
                    last.action != PetAction::Leap && (last.row - f32::from(next)).abs() < 0.01,
                    "{frames_per_key} frames a key, row {row}: down on the caret's row, got {:?} at row {}",
                    last.action,
                    last.row
                );
            }
        }
    }

    #[test]
    fn a_cat_running_ahead_keeps_the_wall_and_crosses_once_for_the_wrap() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 78);
        let limit = 100.0 - art_cols(10, 20);
        let mut track = (pet.col, 0.0f32, 0.0f32);
        let tick = |pet: &mut PetBrain, t: &mut Instant, track: &mut (f32, f32, f32), c: u16| {
            *t += Duration::from_millis(16);
            let f = pet.tick(sense(*t, Some((5, c))));
            let v = (f.col - track.0) / 0.016;
            if f.action != PetAction::Leap && f.lift == 0.0 && f.col > limit - 3.0 {
                track.2 = track.2.max((v - track.1).abs());
            }
            *track = (f.col, v, track.2);
            f
        };
        for c in 80..=91u16 {
            for _ in 0..8 {
                let f = tick(&mut pet, &mut t, &mut track, c);
                assert_ne!(
                    f.action,
                    PetAction::Leap,
                    "no crossing on the run (caret {c})"
                );
            }
        }
        assert!(!pet.wall_side, "a full lead does not cross the cat at 91");
        let mut f = pet.tick(sense(t, Some((5, 91))));
        for _ in 0..60 {
            f = tick(&mut pet, &mut t, &mut track, 91);
            assert_ne!(f.action, PetAction::Leap, "the cat waits at the wall");
        }
        assert!(
            f.action.settled() && (f.col - limit).abs() < ARRIVED,
            "it stands against the wall ({:?} at {}, wall {limit})",
            f.action,
            f.col
        );
        assert!(
            track.2 <= 2.0,
            "it slows into the wall: the speed changed {} cells/s in one frame",
            track.2
        );
        for c in 92..=93u16 {
            for _ in 0..6 {
                let f = tick(&mut pet, &mut t, &mut track, c);
                assert_ne!(
                    f.action,
                    PetAction::Leap,
                    "the caret is not there yet ({c})"
                );
            }
        }
        assert!(!pet.wall_side, "fixture: still on the right at 93");
        let mut launches: Vec<(f32, f32)> = Vec::new();
        let mut prev = f;
        let mut step =
            |pet: &mut PetBrain, t: &mut Instant, track: &mut (f32, f32, f32), c: u16| {
                let f = tick(pet, t, track, c);
                if f.action == PetAction::Leap && prev.action != PetAction::Leap {
                    let fl = pet.flight.expect("airborne");
                    launches.push((fl.to_col, fl.to_row));
                }
                assert!(!pet.wall_side, "never over to the caret's left (caret {c})");
                prev = f;
                f
            };
        for _ in 0..60 {
            let _ = step(&mut pet, &mut t, &mut track, 94);
        }
        for k in 0..6u16 {
            let c = if k % 2 == 0 { 93 } else { 94 };
            for _ in 0..60 {
                let _ = step(&mut pet, &mut t, &mut track, c);
            }
        }
        assert_eq!(
            launches.len(),
            1,
            "one flight, to the next line's stand: {launches:?}"
        );
        assert!(
            (launches[0].1 - 6.0).abs() < 0.01 && launches[0].0 < 20.0,
            "the flight goes to the next line's start: {launches:?}"
        );
        assert!(
            (prev.row - 6.0).abs() < 0.01 && prev.action != PetAction::Leap,
            "and the cat waits there, at row {} ({:?})",
            prev.row,
            prev.action
        );
    }

    #[test]
    fn reaction_hold_and_step_aside_clear_the_cursor_cell_when_typing_forward() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 10);
        let mut spans = vec![(0u16, 0u16); 30];
        for _ in 0..60 {
            t += Duration::from_millis(16);
            spans[5] = (0, 12);
            pet.sense_ink(0, &spans, Some(5));
            let _ = pet.tick(sense(t, Some((5, 12))));
        }
        for c in [11u16, 10, 9] {
            t += Duration::from_millis(32);
            spans[5] = (0, c);
            pet.sense_ink(0, &spans, Some(5));
            let _ = pet.tick(sense(t, Some((5, c))));
        }
        assert_eq!(pet.action, PetAction::Startle);

        for c in 10..=16u16 {
            for _ in 0..3 {
                t += Duration::from_millis(16);
                spans[5] = (0, c);
                pet.sense_ink(0, &spans, Some(5));
                let _ = pet.tick(sense(t, Some((5, c))));
            }
        }
        assert_ne!(
            pet.action,
            PetAction::Startle,
            "sustained forward typing into the cat must yield Startle"
        );
        for _ in 0..30 {
            t += Duration::from_millis(16);
            spans[5] = (0, 16);
            pet.sense_ink(0, &spans, Some(5));
            let _ = pet.tick(sense(t, Some((5, 16))));
        }
        assert!(
            pet.col >= 16.0 + STATION_LEAD - ARRIVED,
            "cat must clear the cursor cell after forward typing, got col={}",
            pet.col
        );
    }

    #[test]
    fn first_sighting_near_right_margin_and_ink_clears_the_caret() {
        use crate::pet_world::{PetPane, PetWorldFacts};
        use aterm_core::terminal::Terminal;
        let start = Instant::now();
        let width = art_cols(10, 20);
        let mut pet = PetBrain::default();
        let mut term = Terminal::new(8, 100);
        term.process(b"\x1b[6;96H");
        let input = term.cell_frame(8, 100);
        let facts = PetWorldFacts::read(&term, 7);
        pet.observe_console(&input, &facts, PetPane::full(&input));
        pet.set_console_presentable(true);
        let frame = pet.tick(sense(start, Some((5, 95))));
        assert!(
            frame.col + width <= 95.0 - STATION_LEAD + 1e-3,
            "fresh sighting near right margin must seat wall-side left of caret, got col={}",
            frame.col
        );

        let mut pet2 = PetBrain::default();
        let mut spans = vec![(0u16, 0u16); 30];
        spans[4] = (0, 100);
        spans[5] = (12, 16);
        spans[6] = (0, 100);
        pet2.sense_ink(0, &spans, Some(5));
        let frame2 = pet2.tick(sense(start, Some((5, 10))));
        let clears_caret =
            frame2.col >= 10.0 + STATION_LEAD || frame2.col + width <= 10.0 - STATION_LEAD;
        assert!(
            clears_caret,
            "fresh sighting must never materialize on top of the caret (col={})",
            frame2.col
        );
    }

    #[test]
    fn two_forward_keystrokes_after_backspace_flinch_yield_perk_to_walk() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 10);
        for _ in 0..60 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((5, 12))));
        }
        t += Duration::from_millis(32);
        let _ = pet.tick(sense(t, Some((5, 11))));
        assert_eq!(pet.action, PetAction::Perk);

        for c in [12u16, 13] {
            t += Duration::from_millis(32);
            let _ = pet.tick(sense(t, Some((5, c))));
        }
        assert!(
            matches!(pet.action, PetAction::Walk | PetAction::Run),
            "second forward keystroke encroaching on the cat must yield Perk to chase (got {:?})",
            pet.action
        );
    }

    #[test]
    fn first_key_after_face_on_sit_and_16ms_ink_push_walk_on_paws_not_slide_in_settled_pose() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 15);
        let (mut t, settled) = idle(&mut pet, t, (4, 15), SIT_AFTER + 0.5);
        assert_eq!(settled.pose, PetGlyphId::PetSitFront);
        assert!(pet.facing_left);

        t += Duration::from_millis(16);
        let f1 = pet.tick(sense(t, Some((4, 16))));
        assert_eq!(
            f1.action,
            PetAction::Walk,
            "first key after PetSitFront must wake into Walk, got {:?}",
            f1.action
        );
        assert!(
            !f1.facing_left,
            "first key after PetSitFront must face the forward writing"
        );
        for _ in 0..8 {
            t += Duration::from_millis(16);
            let f = pet.tick(sense(t, Some((4, 16))));
            assert!(
                !(f.action == PetAction::Stand && f.facing_left),
                "must never slide backwards in Stand facing left"
            );
        }

        let mut pet2 = PetBrain::default();
        let mut t2 = awake(&mut pet2, start, 3, 5);
        let mut spans = vec![(0u16, 0u16); 5];
        spans[2] = (0, 100);
        spans[3] = (0, 27);
        spans[4] = (0, 100);
        for _ in 0..180 {
            t2 += Duration::from_millis(16);
            pet2.sense_ink(0, &spans, Some(3));
            let _ = pet2.tick(sense(t2, Some((3, 7))));
        }
        assert!(pet2.action.settled());
        t2 += Duration::from_millis(16);
        spans[3] = (0, 28);
        pet2.sense_ink(0, &spans, Some(3));
        let pushed = pet2.tick(sense(t2, Some((3, 8))));
        assert_eq!(
            pushed.action,
            PetAction::Walk,
            "1-cell ink push on a 16 ms frame must walk in the chase, not slide in {:?}",
            pushed.action
        );
    }

    #[test]
    fn every_stop_brakes_at_any_pace() {
        let limit = 100.0 - art_cols(10, 20);
        for frames_per_key in [8u32, 7, 6, 5, 4] {
            for stop in [84u16, 86, 88, 89, 90, 91, 92, 93] {
                let label = format!(
                    "{:.3} s a key, typing stops at {stop}",
                    frames_per_key as f32 * 0.016
                );
                let start = Instant::now();
                let mut pet = PetBrain::default();
                let mut t = awake(&mut pet, start, 5, 78);
                let mut prev = (pet.col, 0.0f32, false);
                let mut worst = (0.0f32, 0.0f32, 0.0f32);
                let mut back = None;
                let mut moved_at = 0usize;
                let keys = usize::from(stop - 79);
                let rest_frames = 90usize;
                let carets =
                    (80..=stop).flat_map(|c| std::iter::repeat_n(c, frames_per_key as usize));
                for (k, c) in carets
                    .chain(std::iter::repeat_n(stop, rest_frames))
                    .enumerate()
                {
                    t += Duration::from_millis(16);
                    let f = pet.tick(sense(t, Some((5, c))));
                    let v = (f.col - prev.0) / 0.016;
                    let foot = f.action != PetAction::Leap && f.lift == 0.0;
                    if foot && prev.2 && k >= 3 * frames_per_key as usize && v * prev.1 >= 0.0 {
                        let drop = prev.1.abs() - v.abs();
                        if drop > worst.0 {
                            worst = (drop, prev.1, f.col);
                        }
                    }
                    if k >= 3 * frames_per_key as usize && f.col < prev.0 - 1e-3 {
                        back.get_or_insert((k, prev.0, f.col, f.action));
                    }
                    if (f.col - prev.0).abs() > 1e-3 {
                        moved_at = k;
                    }
                    prev = (f.col, v, foot);
                }
                assert!(
                    worst.0 <= 2.0,
                    "{label}: the speed fell {:.2} cells/s in one frame (from {:.2} at col {:.2})",
                    worst.0,
                    worst.1,
                    worst.2
                );
                assert!(back.is_none(), "{label}: the cat stepped back: {back:?}");
                assert!(
                    pet.action.settled() && prev.0 <= limit + 1e-3,
                    "{label}: at rest by the margin ({:?} at {:.2}, wall {limit:.2})",
                    pet.action,
                    prev.0
                );
                let typed = keys * frames_per_key as usize;
                assert!(
                    moved_at < typed + 30,
                    "{label}: still moving {} frames after the last key",
                    moved_at.saturating_sub(typed)
                );
            }
        }
    }

    #[test]
    fn a_cat_at_the_wall_stands_while_the_writing_comes_on() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 78);
        let limit = 100.0 - art_cols(10, 20);
        let mut prev = pet.col;
        let (mut at_wall, mut in_place) = (0u32, 0u32);
        for c in 80..=93u16 {
            for _ in 0..8 {
                t += Duration::from_millis(16);
                let f = pet.tick(sense(t, Some((5, c))));
                let still = (f.col - prev).abs() < 1e-3;
                if (f.col - limit).abs() < 0.05 && still {
                    at_wall += 1;
                    in_place += u32::from(matches!(f.action, PetAction::Walk | PetAction::Run));
                }
                prev = f.col;
            }
        }
        assert!(
            at_wall >= 16,
            "fixture: the cat waits at the wall ({at_wall} frames)"
        );
        assert!(
            in_place <= 2,
            "{in_place} of {at_wall} frames at the wall walk on the spot"
        );
    }

    #[test]
    fn the_walk_into_a_hide_slows_into_the_crouch() {
        for off in [3u16, 6] {
            let start = Instant::now();
            let mut pet = PetBrain::default();
            let mut t = warm_settled(&mut pet, start);
            let mut spans = [(0u16, 0u16); 12];
            let seat = pet.col.round() as u16;
            spans[pet.row as usize] = (seat - 8 - off, seat - off);
            let mut prev = (pet.col, 0.0f32);
            let mut worst = 0.0f32;
            let mut dest = None;
            for _ in 0..900 {
                t += Duration::from_millis(16);
                pet.sense_ink(0, &spans, None);
                if !pet.hiding && pet.hide_to.is_none() {
                    pet.mote_serial = 3;
                    pet.content = pet.content.max(0.9);
                }
                let f = pet.tick(sense(t, Some((4, 44))));
                dest = pet.hide_to.or(dest);
                let v = (f.col - prev.0) / 0.016;
                if dest.is_some()
                    && f.action != PetAction::Leap
                    && f.lift == 0.0
                    && v * prev.1 >= 0.0
                {
                    worst = worst.max(prev.1.abs() - v.abs());
                }
                prev = (f.col, v);
                if pet.hiding {
                    break;
                }
            }
            let dest = dest.expect("fixture: the deal sends the cat behind the word");
            assert!(pet.hiding, "off {off}: the cat hides");
            assert!(
                worst <= 2.0,
                "off {off}: the walk in stopped {worst:.2} cells/s in one frame"
            );
            assert!(
                (pet.col - dest).abs() < 0.1,
                "off {off}: crouched on the spot ({:.2}, the spot {dest:.2})",
                pet.col
            );
        }
    }

    #[test]
    fn a_scrabble_hunts_braced_never_in_the_gallop_s_art() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 5, 20);
        let width = art_cols(10, 20);
        pet.set_action(PetAction::Land);
        pet.scrabble = true;
        let mut wobbled = false;
        let mut k = 0.0f32;
        while k * 0.016 < SCRABBLE_DUR {
            pet.land_t = SCRABBLE_DUR - k * 0.016;
            let f = pet.emit(sense(t, Some((5, 22))), width);
            let id = crate::pet_glyphs_gen::PET_GLYPHS[f.pose as usize].id;
            assert!(
                !id.starts_with("pet_run_"),
                "{:.3} s into the scrabble the still body wears {id}",
                k * 0.016
            );
            wobbled |= (f.scale_x - 1.0).abs() > 1e-3 && k * 0.016 > SCRABBLE_PLANT;
            k += 1.0;
        }
        assert!(wobbled, "negative control: the hunt still wobbles");
    }

    #[test]
    fn face_on_peck_scoot_stops_short_of_adjacent_ink_without_oscillating() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let (cell_w, cell_h) = (9, 20);
        let width = art_cols(cell_w, cell_h);
        let sense_at = |now: Instant, caret: (u16, u16)| PetSense {
            cell_w,
            cell_h,
            ..sense(now, Some(caret))
        };
        let mut t = start;
        let _ = pet.tick(sense_at(t, (4, 14)));
        t += Duration::from_millis(16);
        let _ = pet.tick(sense_at(t, (4, 15)));
        let ink_start = 22u16;
        let mut spans = [(0u16, 0u16); 12];
        spans[4] = (ink_start, ink_start + 4);
        let end = t + Duration::from_secs_f32(SIT_AFTER + 0.6);
        let mut last = None;
        while t < end {
            t += Duration::from_millis(16);
            pet.sense_ink(0, &spans, None);
            last = Some(pet.tick(sense_at(t, (4, 15))));
        }
        let f = last.expect("frames ticked");
        assert_eq!(f.pose, PetGlyphId::PetSitFront);
        assert!(
            !pet.ink_overlaps(f.col, f.row, width),
            "face-on sit must not scoot into adjacent ink: col={}, width={}, ink_start={}",
            f.col,
            width,
            ink_start
        );
        assert!(
            !pet.evicting && !pet.needs_frames(),
            "settled face-on sit beside ink must not get stuck in an eviction/scoot loop"
        );
    }

    #[test]
    fn reduced_motion_and_fade_out_clear_letter_veil_without_pinning_the_frame_train() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 29, 18);
        let faintest = veiled_over(&mut pet, &mut t, PetAction::Run, 60, 0.3);
        assert!(
            faintest < 255 && pet.letter_veil.0 < 1.0,
            "fixture: moving over ink dims the body under the letters"
        );
        let spans = {
            let mut spans = [(0u16, 0u16); 30];
            spans[29] = (0, 60);
            spans
        };

        t += Duration::from_millis(16);
        pet.sense_ink(0, &spans, None);
        let rm = pet.tick(PetSense {
            reduced_motion: true,
            ..sense(t, Some((29, 20)))
        });
        assert_eq!(rm.alpha, 255);
        assert!(!rm.under_ink);
        assert_eq!(pet.letter_veil, (1.0, None, 1.0));
        assert!(!pet.needs_frames());

        let mut pet2 = PetBrain::default();
        let mut t2 = awake(&mut pet2, start, 29, 18);
        let _ = veiled_over(&mut pet2, &mut t2, PetAction::Run, 60, 0.3);
        assert!(pet2.letter_veil.0 < 1.0);
        let mut hidden = None;
        for _ in 0..36 {
            t2 += Duration::from_millis(16);
            let f = pet2.tick(sense(t2, None));
            if f.alpha == 0 {
                hidden = Some(f);
                break;
            }
        }
        assert_eq!(hidden.map(|f| f.alpha), Some(0));
        assert_eq!(pet2.letter_veil, (1.0, None, 1.0));
    }

    #[test]
    fn pet_frame_fp_folds_mote_rgb_and_departure_glass_routing_but_not_departure_id() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 20);
        let mut base = pet.tick(sense(t, Some((4, 20))));
        base.motes[0] = Some(PetMoteSprite {
            kind: PetMoteKind::Note,
            col: 21.0,
            row: 3.5,
            scale: 1.0,
            rot: 0.25,
            alpha: 200,
            rgb: None,
        });
        base.departures[0] = Some(PetDepartureSprite {
            pose: PetGlyphId::PetRun0,
            col: 18.0,
            row: 4.0,
            facing_left: true,
            scale_x: 1.0,
            scale_y: 1.0,
            lift: 0.0,
            alpha: 180,
            coat: 0,
            iris: 0,
            id: 7,
            under_ink: false,
            own_glass: false,
            glass: 0,
            colors: None,
        });
        let fp0 = base.fp();

        let mut with_rgb = base;
        with_rgb.motes[0].as_mut().unwrap().rgb = Some(0xFF00AA);
        assert_ne!(with_rgb.fp(), fp0, "mote rgb tint must move PetFrame::fp");

        let mut with_under_ink = base;
        with_under_ink.departures[0].as_mut().unwrap().under_ink = true;
        assert_ne!(
            with_under_ink.fp(),
            fp0,
            "departure under_ink must move PetFrame::fp"
        );

        let mut with_own_glass = base;
        with_own_glass.departures[0].as_mut().unwrap().own_glass = true;
        assert_ne!(
            with_own_glass.fp(),
            fp0,
            "departure own_glass must move PetFrame::fp"
        );

        let mut with_glass = base;
        with_glass.departures[0].as_mut().unwrap().glass = 42;
        assert_ne!(
            with_glass.fp(),
            fp0,
            "departure glass key must move PetFrame::fp"
        );

        let mut different_id = base;
        different_id.departures[0].as_mut().unwrap().id = 999;
        assert_eq!(
            different_id.fp(),
            fp0,
            "departure id is identity-only and must not move PetFrame::fp"
        );
    }

    #[test]
    fn scrabble_brake_and_stumble_dust_kick_back_along_motion() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 4, 10);

        pet.skid_dir = -1.0;
        pet.mote_serial = 0;
        pet.stride = 0.0;
        pet.motes = [None; PET_MOTES_MAX];
        pet.begin_arc(30.0, 4.0, 0.45, 0.8, FlightKind::PLAIN);
        if let Some(fl) = pet.flight.as_mut() {
            fl.t = fl.dur;
            fl.reaimed = true;
        }
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((4, 10))));
        assert!(pet.scrabble, "scrabble landing armed");
        assert_eq!(pet.skid_dir, 1.0, "rightward scrabble sets +1.0 skid_dir");
        for m in pet
            .motes
            .iter()
            .flatten()
            .filter(|m| m.kind == PetMoteKind::Dust)
        {
            assert_eq!(m.dir, -1.0, "rightward scrabble dust kicks left (-1.0)");
        }

        pet.drop_theater();
        assert_eq!(pet.skid_dir, 0.0, "drop_theater clears skid_dir");
    }

    #[test]
    fn reduced_motion_drops_tennis_stumble_pounce_and_retired_flight_lift() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 4, 10);

        pet.tennis = true;
        pet.stumble_t = STUMBLE_DUR;
        pet.pending_pounce = true;
        pet.retired_flight_lift = Some((0.8, 3.0));
        pet.deferred_hidden_landing = Some((20.0, 4.0));
        pet.droop_brief = true;
        pet.sit_front = true;
        pet.peek = true;
        pet.gaze_dwell = 0.5;
        pet.leg_dist = 8.0;

        let mut s = sense(t + Duration::from_millis(16), Some((4, 10)));
        s.reduced_motion = true;
        let frame = pet.tick(s);
        assert_eq!(frame.lift, 0.0, "reduced motion clears retired_flight_lift");
        assert!(
            (frame.scale_x - 1.0).abs() < 1e-6 && (frame.scale_y - 1.0).abs() < 1e-6,
            "reduced motion clears stumble squash"
        );
        assert!(
            !pet.tennis
                && pet.stumble_t == 0.0
                && !pet.pending_pounce
                && pet.retired_flight_lift.is_none()
                && !pet.droop_brief
                && !pet.sit_front
                && !pet.peek
                && pet.gaze_dwell == 0.0
                && pet.leg_dist == 0.0
        );
        assert!(
            !pet.needs_frames(),
            "reduced motion must not leave tennis, stumble_t or retired_flight_lift pinning needs_frames"
        );
    }

    #[test]
    fn carry_with_regrid_shifts_bound2_adopted_look_and_motes_and_zero_alpha_clears_adopted_look() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 6, 12);

        pet.bound2 = Some((28.0, 6.0, false));
        pet.adopted_look = Some(AdoptedLook {
            pose: PetGlyphId::PetSit,
            facing_left: false,
            stance: Stance::NEUTRAL,
            eased: 0.0,
            seat: (pet.col, 6.0),
            beneath: None,
        });
        pet.motes[0] = Some(Mote {
            kind: PetMoteKind::Zee,
            born: pet.clock,
            life: 1.5,
            col: pet.col,
            row: 6.0,
            dir: 1.0,
            seed: 0,
            rgb: None,
        });

        pet.carry_with_regrid(3.0, 24);
        assert_eq!(pet.row, 9.0, "body row shifted by +3");
        assert_eq!(
            pet.bound2.map(|(_, r2, _)| r2),
            Some(9.0),
            "bound2 target row shifted by +3 with regrid"
        );
        assert_eq!(
            pet.adopted_look.as_ref().map(|l| l.seat.1),
            Some(9.0),
            "adopted_look seat row shifted by +3 so crossfade hold survives regrid"
        );
        assert_eq!(
            pet.motes[0].as_ref().map(|m| m.row),
            Some(9.0),
            "live mote row shifted by +3 with regrid"
        );

        pet.bound2 = Some((28.0, 22.0, false));
        pet.carry_with_regrid(3.0, 24);
        assert_eq!(
            pet.row, 9.0,
            "out-of-bounds bound2 refuses carry_with_regrid"
        );

        pet.alpha = 0.01;
        let _ = pet.tick(sense(t + Duration::from_secs(1), None));
        assert_eq!(pet.alpha, 0.0, "faded to zero alpha");
        assert!(
            pet.adopted_look.is_none(),
            "zero-alpha fade-out clears adopted_look"
        );
    }

    #[test]
    fn split_crossing_mid_air_typing_moves_bound2_not_touch_and_console_carry_shifts_skid_and_adopted_look()
     {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 6, 4);
        let w = art_cols(10, 20);

        pet.launch_big(82.0, 6.0, 100, w, true);
        let touch0 = pet.flight.expect("first bound launched").to_col;
        let bound2_0 = pet.bound2.expect("split into two bounds").0;
        assert!(
            touch0 < bound2_0 - 15.0,
            "touch ({touch0}) is midfield before bound2 ({bound2_0})"
        );
        pet.last_caret = Some((6, 80));

        for c in [81u16, 82, 83] {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((6, c))));
        }
        let touch1 = pet.flight.expect("still in first bound").to_col;
        let bound2_1 = pet.bound2.expect("bound2 still pending").0;
        assert!(
            (touch1 - touch0).abs() < 1e-4,
            "first bound keeps its midfield touch point ({touch0} -> {touch1})"
        );
        assert!(
            bound2_1 > bound2_0 && bound2_1 < bound2_0 + 15.0,
            "bound2 advances with the typing rhythm without overshooting ({bound2_0} -> {bound2_1})"
        );

        let mut frame = pet.emit(sense(t, Some((6, 83))), w);
        pet.home = pet.col;
        let home0 = pet.home;
        pet.skid_from = 20.0;
        pet.brake_over = Some(22.0);
        pet.adopted_look = Some(AdoptedLook {
            pose: PetGlyphId::PetSit,
            facing_left: false,
            stance: Stance::NEUTRAL,
            eased: 0.25,
            seat: (20.0, 6.0),
            beneath: None,
        });
        let to = PetFrame {
            col: frame.col + 3.0,
            row: frame.row + 2.0,
            ..frame
        };
        pet.carry_console_body(&mut frame, to, 2.0);
        assert!(
            (pet.home - (home0 + 3.0)).abs() < 1e-4,
            "home shifted by dx so carry_console_body does not open a false home-col gap"
        );
        assert!(
            (pet.skid_from - 23.0).abs() < 1e-4,
            "skid_from shifted by dx"
        );
        assert_eq!(pet.brake_over, Some(25.0), "brake_over shifted by dx");
        assert_eq!(
            pet.adopted_look.map(|l| l.seat),
            Some((23.0, 8.0)),
            "adopted_look seat shifted by (dx, ground_dy)"
        );

        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((8, 83))));
        assert!(
            pet.bound2.is_none(),
            "mid-flight row change cancels split crossing's second bound"
        );

        pet.flight = None;
        pet.set_action(PetAction::Land);
        pet.land_t = TOUCH_LAND_DUR;
        pet.bound2 = Some((75.0, 8.0, true));
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((10, 83))));
        assert!(
            pet.bound2.is_none(),
            "row change during touch-land cancels bound2"
        );

        pet.row = 10.0;
        pet.flight = None;
        pet.bound2 = None;
        pet.play_to = Some((12.0, 10.0));
        pet.play_land = Some(PlayLand::BatSwipe {
            col: 13.0,
            row: 10.0,
        });
        pet.pending_bat = Some((15.0, 10.0));
        pet.room_cheer_stage = Some((14.0, 10.0));
        pet.pending_pointer_pounce = Some((20.0, 10.0));
        pet.rebase_geometry();
        assert!(pet.pending_pointer_pounce.is_none());
        pet.carry_with_regrid(2.0, 24);
        assert_eq!(pet.play_to, Some((12.0, 12.0)));
        assert_eq!(
            pet.play_land,
            Some(PlayLand::BatSwipe {
                col: 13.0,
                row: 12.0,
            })
        );
        assert_eq!(pet.pending_bat, Some((15.0, 12.0)));
        assert_eq!(pet.room_cheer_stage, Some((14.0, 12.0)));
    }

    #[test]
    fn visible_resighting_preserves_wall_side_hysteresis_and_latches_wall_transit() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 96);
        let (_, _) = idle(&mut pet, t, (5, 96), 0.6);
        t += Duration::from_millis(600);
        assert!(pet.wall_side, "fixture: parked wall-side at col 96");

        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((5, 93))));
        assert!(pet.wall_side, "still wall-side inside hysteresis band");
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, None));
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((5, 93))));
        assert!(
            pet.wall_side && !pet.pending_wall_transit,
            "1-frame caret hide must not clobber wall_side hysteresis on a visible cat"
        );
        pet.rebase_geometry();
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((5, 93))));
        assert!(
            pet.wall_side && !pet.pending_wall_transit,
            "rebase_geometry must not clobber wall_side hysteresis on a visible cat"
        );

        pet.rebase_geometry();
        t += Duration::from_millis(16);
        let mut wide = sense(t, Some((5, 93)));
        wide.cols = 120;
        let f = pet.tick(wide);
        assert!(
            !pet.wall_side && (pet.pending_wall_transit || f.action == PetAction::Leap),
            "widening past WALL_RETURN_MARGIN must hop over the caret"
        );
    }

    #[test]
    fn big_flight_midair_reaim_shortens_for_skid_and_carry_console_body_shifts_in_place_play_to() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 10);
        let (_, _) = idle(&mut pet, t, (5, 10), 0.6);
        t += Duration::from_millis(600);
        let s0 = sense(t, Some((5, 10)));
        let width = art_cols(s0.cell_w, s0.cell_h);

        pet.launch_big(24.0 + STATION_LEAD, 5.0, 100, width, true);
        assert!(pet.flight.is_some_and(|f| f.big) && pet.bound2.is_none());

        pet.last_caret = Some((5, 10));
        pet.vhat = 0.0;
        t += Duration::from_millis(16);
        let mut frame = pet.tick(sense(t, Some((5, 20))));
        let target_station = pet.station_now(20, 100, width);
        let to_col = pet.flight.expect("still airborne after re-aim").to_col;
        assert!(
            (to_col - (target_station - SKID_CELLS)).abs() <= 1e-3,
            "big flight mid-air jump re-aim must shorten f.to_col by SKID_CELLS: got {to_col}, expected {}",
            target_station - SKID_CELLS
        );

        pet.col = 14.0;
        pet.row = 5.0;
        pet.play_to = Some((14.0, 5.0));
        frame.col = 14.0;
        frame.row = 5.0;
        let mut moved = frame;
        moved.col += 3.0;
        moved.row += 2.0;
        pet.carry_console_body(&mut frame, moved, 2.0);
        assert_eq!(
            pet.play_to,
            Some((17.0, 7.0)),
            "in-place play_to must ride along with carry_console_body"
        );
    }

    #[test]
    fn vigil_verdict_with_prompt_jump_or_sleeping_cat_never_strands_frame_lane() {
        let t0 = Instant::now();

        let (mut pet, mut t) = at_the_door(t0);
        pet.note_command_done(t, false, Some(40_000));
        let mut flew = false;
        let mut released = false;
        for _ in 0..2_000 {
            t += Duration::from_millis(16);
            pet.note_executing(t, false);
            let f = pet.tick(sense(t, Some((11, 4))));
            flew |= f.action.airborne();
            if !pet.needs_frames() {
                released = true;
                break;
            }
        }
        assert!(
            flew,
            "vigil verdict with returning prompt jump must still fly"
        );
        assert!(
            released && !pet.pending_vigil_pounce && pet.vigil_cheer.is_none(),
            "vigil verdict with returning prompt jump must release needs_frames()"
        );

        let (mut pet_sleep, mut t2) = at_the_door(t0);
        pet_sleep.note_trick(t2, Trick::Sleep, true);
        for _ in 0..40 {
            t2 += Duration::from_millis(16);
            pet_sleep.note_executing(t2, true);
            let _ = pet_sleep.tick(sense(t2, Some((10, 20))));
        }
        assert!(pet_sleep.action == PetAction::Sleep && !pet_sleep.vigil);
        pet_sleep.note_command_done(t2, false, Some(40_000));
        assert!(
            !pet_sleep.pending_vigil_pounce,
            "sleeping cat not in vigil crouch must not latch pending_vigil_pounce"
        );
    }

    #[test]
    fn play_crouch_faces_target_and_pounce_coil_refreshes_coil_d_every_tick() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 20);
        let (_, _) = idle(&mut pet, t, (5, 20), 0.6);
        t += Duration::from_millis(600);
        let s0 = sense(t, Some((5, 20)));
        let width = art_cols(s0.cell_w, s0.cell_h);

        pet.facing_left = true;
        pet.pending_bat = Some((40.0, 5.0));
        assert!(pet.consume_bat(&s0, width));
        assert!(
            !pet.facing_left && pet.action == PetAction::Crouch,
            "consume_bat to the right must face right on the crouch frame"
        );

        pet.clear_play_intents();
        pet.set_action(PetAction::Sit);
        pet.facing_left = false;
        pet.pending_pointer_pounce = Some((4.0, 5.0));
        assert!(pet.consume_pointer_pounce(&s0, width));
        assert!(
            pet.facing_left && pet.action == PetAction::Crouch,
            "consume_pointer_pounce to the left must face left on the crouch frame"
        );

        pet.clear_play_intents();
        pet.set_action(PetAction::Crouch);
        pet.action_t = 0.01;
        pet.coil_d = CROUCH_DUR;
        pet.quiet_leap = false;
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((5, 36))));
        assert!(
            pet.coil_d > CROUCH_DUR,
            "pounce coil must refresh coil_d while action_t < CROUCH_DUR: got {}",
            pet.coil_d
        );
    }

    #[test]
    fn next_change_secs_withholds_bored_vignette_offer_under_room_lease_or_hold() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let t = awake(&mut pet, start, 5, 20);
        let (_t, _) = idle(&mut pet, t, (5, 20), 6.0);
        pet.set_action(PetAction::Sit);
        pet.quiet = BORED_AFTER - 0.01;
        pet.content = PLAY_CONTENT + 0.05;
        pet.bored_cool = 0.0;
        pet.trick_settle = Some(Trick::Sit);
        pet.hold_end_q = pet.quiet + TRICK_HOLD;

        let free_offer = pet
            .next_change_secs()
            .expect("content cat should offer bored vignette at BORED_AFTER - q");
        assert!(
            (free_offer - 0.01).abs() < 1e-4,
            "free cat should offer bored vignette at 0.01s, got {free_offer}"
        );

        pet.room_hold = true;
        let hold_offer = pet
            .next_change_secs()
            .expect("held cat should offer next purr step");
        assert!(
            hold_offer > 0.03,
            "room_hold suppresses bored vignette and must not offer 0.01s wake: got {hold_offer}"
        );

        pet.room_hold = false;
        pet.room_lease = true;
        let lease_offer = pet
            .next_change_secs()
            .expect("leased cat should offer next purr step");
        assert!(
            lease_offer > 0.03,
            "room_lease suppresses bored vignette and must not offer 0.01s wake: got {lease_offer}"
        );
    }

    #[test]
    fn sleeper_drop_is_not_reaimed_sideways_and_play_bound_ignores_caret_drift() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 25);
        let (t2, _) = idle(&mut pet, t, (5, 25), SLEEP_AFTER + 0.5);
        t = t2;
        assert_eq!(pet.action, PetAction::Sleep);
        let sleep_col = pet.col;

        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((6, 5))));
        assert!(
            pet.flight
                .is_some_and(|f| f.drop && (f.to_col - sleep_col).abs() < 1e-4),
            "sleeper drop must start straight down at sleep_col"
        );
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((6, 13))));
        let drop_flight = pet.flight.expect("drop still airborne");
        assert!(
            !drop_flight.reaimed && (drop_flight.to_col - sleep_col).abs() < 1e-4,
            "sleeper drop must not be mid-air re-aimed sideways: got {:?}",
            drop_flight
        );

        let mut pet2 = PetBrain::default();
        let mut t3 = awake(&mut pet2, start, 5, 10);
        for c in 11..=16 {
            t3 += Duration::from_millis(60);
            let _ = pet2.tick(sense(t3, Some((5, c))));
        }
        assert!(
            pet2.station_drift(0.5) > 0.1,
            "fixture: open typing rhythm has positive station_drift"
        );
        let s3 = sense(t3, Some((5, 16)));
        let width = art_cols(s3.cell_w, s3.cell_h);
        pet2.col = 10.0;
        pet2.row = 5.0;
        pet2.play_land = Some(PlayLand::PointerFrolic);
        pet2.leave_ground(40.0, 5.0, false, s3.cols, width, LeapPolicy::Play);
        let f2 = pet2.flight.expect("play bound launched");
        assert!(
            f2.big && (f2.to_col - (40.0 - SKID_CELLS)).abs() < 1e-3,
            "play bound must aim at play target without caret station_drift: got {}",
            f2.to_col
        );
    }

    #[test]
    fn jump_notice_and_launch_doors_face_target_without_liftoff_snap() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 10);
        let (t2, _) = idle(&mut pet, t, (5, 10), 1.5);
        t = t2;
        pet.facing_left = true;

        t += Duration::from_millis(16);
        let f_perk = pet.tick(sense(t, Some((5, 22))));
        assert!(
            f_perk.action == PetAction::Perk && !f_perk.facing_left,
            "rightward jump notice Perk must face right toward the caret"
        );

        let mut pet2 = PetBrain::default();
        let mut t3 = awake(&mut pet2, start, 5, 10);
        let (t4, _) = idle(&mut pet2, t3, (5, 10), 1.5);
        t3 = t4;
        pet2.facing_left = true;
        pet2.set_action(PetAction::Perk);
        pet2.action_t = PERK_HOLD;
        pet2.pending_big_jump = true;
        t3 += Duration::from_millis(16);
        let f_gather = pet2.tick(sense(t3, Some((5, 65))));
        assert!(
            f_gather.action == PetAction::Crouch && pet2.big_gather && !f_gather.facing_left,
            "big_gather crouch must face the jump target from its first frame"
        );
    }

    #[test]
    fn room_hold_stands_down_in_flight_games_and_queued_trick_go() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 10);
        let (t2, _) = idle(&mut pet, t, (5, 10), 1.5);
        t = t2;

        pet.room_hold = true;
        pet.pursuit_t = Some(0.5);
        pet.stakeout = true;
        pet.set_action(PetAction::Crouch);
        pet.pending_bat = Some((18.0, 5.0));
        pet.bat_at = Some(t);
        pet.pending_pointer_pounce = Some((18.0, 5.0));
        pet.trick_go = Some(Trick::Roll);

        t += Duration::from_millis(16);
        let mut s = sense(t, Some((5, 10)));
        s.pointer = Some((16.0, 5.0));
        let f = pet.tick(s);
        assert!(
            pet.pursuit_t.is_none()
                && !pet.stakeout
                && pet.trick_go.is_none()
                && pet.play_to.is_none()
                && f.action == PetAction::Stand,
            "room_hold must stand down active pursuit, stakeout, pointer pounce, and trick_go"
        );

        pet.pending_bat = Some((18.0, 5.0));
        pet.pending_pointer_pounce = Some((18.0, 5.0));
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((5, 10))));
        assert!(
            pet.pending_bat.is_none()
                && pet.pending_pointer_pounce.is_none()
                && pet.play_to.is_none(),
            "room_hold must drop latched bat and pointer pounce without launching"
        );
    }

    #[test]
    fn play_and_hide_crouches_stand_down_cleanly_on_fresh_input_and_line_change() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 10);
        let (t2, _) = idle(&mut pet, t, (5, 10), 1.5);
        t = t2;

        pet.stakeout = true;
        pet.set_action(PetAction::Crouch);
        pet.action_t = 0.5;
        pet.song_left = 2;
        pet.clear_play();
        pet.drop_tricks();
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((5, 10))));
        assert!(
            !pet.stakeout
                && pet.song_left == 0
                && pet.flight.is_none()
                && !matches!(f.action, PetAction::Crouch | PetAction::Leap),
            "clearing play during stakeout crouch must stand down without launching a leap"
        );

        pet.stakeout = true;
        pet.hiding = true;
        pet.hide_to = Some(30.0);
        pet.set_action(PetAction::Crouch);
        pet.action_t = 0.5;
        t += Duration::from_millis(16);
        let f2 = pet.tick(sense(t, Some((6, 8))));
        assert!(
            !pet.stakeout
                && !pet.hiding
                && pet.hide_to.is_none()
                && f2.action == PetAction::Crouch
                && pet.hop_crouch,
            "line change during stakeout/hiding must clear play intents and enter hop_crouch"
        );

        let mut pet2 = PetBrain::default();
        let mut t3 = awake(&mut pet2, start, 5, 10);
        pet2.hiding = true;
        pet2.hide_to = Some(28.0);
        pet2.set_action(PetAction::Crouch);
        t3 += Duration::from_millis(16);
        let f3 = pet2.tick(sense(t3, Some((5, 11))));
        assert!(
            !pet2.hiding && pet2.hide_to.is_none() && f3.action != PetAction::Crouch,
            "same-row forward keystroke must end hiding and clear hide_to"
        );
    }

    #[test]
    fn interrupted_crouch_land_and_frolic_substates_clear_and_solved_reaim_spends_jump_latch() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 20);
        let (t2, _) = idle(&mut pet, t, (5, 20), 1.5);
        t = t2;

        pet.stakeout = true;
        pet.play_to = Some((25.0, 5.0));
        pet.play_land = Some(PlayLand::PointerFrolic);
        pet.set_action(PetAction::Crouch);
        t += Duration::from_millis(16);
        let f = pet.tick(sense(t, Some((5, 19))));
        assert_eq!(f.action, PetAction::Perk);
        assert!(
            !pet.stakeout && pet.play_to.is_none() && pet.play_land.is_none(),
            "flinching out of a play/stakeout crouch must clear stakeout, play_to, and play_land"
        );

        pet.set_action(PetAction::Land);
        pet.land_t = LAND_DUR;
        pet.bound2 = Some((40.0, 5.0, false));
        pet.resume = Some((PetAction::Sit, 0.3));
        pet.play_land = Some(PlayLand::PointerFrolic);
        t += Duration::from_millis(16);
        let f_land = pet.tick(sense(t, Some((5, 18))));
        assert_eq!(f_land.action, PetAction::Perk);
        assert!(
            pet.land_t == 0.0
                && pet.bound2.is_none()
                && pet.resume.is_none()
                && pet.play_land.is_none(),
            "flinching out of Land must clear land_t, bound2, resume, and play_land"
        );

        let mut pet2 = PetBrain::default();
        let mut t3 = awake(&mut pet2, start, 5, 10);
        t3 += Duration::from_millis(16);
        let _ = pet2.tick(sense(t3, Some((5, 20))));
        while pet2.flight.is_none() {
            t3 += Duration::from_millis(16);
            let _ = pet2.tick(sense(t3, Some((5, 20))));
        }
        t3 += Duration::from_millis(16);
        let _ = pet2.tick(sense(t3, Some((5, 27))));
        assert!(
            pet2.flight.is_some_and(|fl| fl.reaimed)
                && !pet2.pending_pounce
                && !pet2.pending_big_jump,
            "a solved mid-air re-aim must consume pending_pounce and pending_big_jump"
        );
    }

    #[test]
    fn regrid_carry_preserves_quiet_leap_and_repair_target_and_stream_watch_clears_play_intents() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 10);

        pet.row = 4.0;
        pet.action = PetAction::Run;
        pet.console.repair_target = Some((5.0, 10.0));
        pet.console.last_seat = Some((12.0, 4.0));
        pet.rebase_geometry();
        t += Duration::from_millis(16);
        let _ = pet.tick(sense(t, Some((7, 10))));
        assert!(
            (pet.row - 6.0).abs() < 1e-4 && !pet.quiet_leap,
            "regrid carry of a neighbour-row cat must not falsely latch quiet_leap (row={}, quiet_leap={})",
            pet.row,
            pet.quiet_leap
        );
        assert_eq!(pet.console.repair_target, Some((7.0, 10.0)));
        assert_eq!(pet.console.last_seat, Some((12.0, 6.0)));
        pet.console.rest_seat.set(Some((12.0, 4.0)));
        assert!(pet.carry_with_regrid(2.0, 24));
        assert_eq!(pet.console.rest_seat.get(), Some((12.0, 6.0)));

        let mut pet2 = PetBrain::default();
        let mut t2 = awake(&mut pet2, start, 5, 79);
        pet2.pursuit_t = Some(0.5);
        pet2.stakeout = true;
        pet2.play_to = Some((25.0, 5.0));
        pet2.set_action(PetAction::Crouch);
        for step in 0..3 {
            t2 += Duration::from_millis(16);
            let mut s = sense(t2, Some((5, if step % 2 == 0 { 0 } else { 79 })));
            s.wrapped = step % 2 == 0;
            if step % 2 == 1 {
                s.wrapped = false;
            }
            let _ = pet2.tick(s);
        }
        assert!(
            pet2.stream
                && pet2.pursuit_t.is_none()
                && !pet2.stakeout
                && pet2.play_to.is_none()
                && pet2.action == PetAction::Perk,
            "arming stream watch on same-row wraps/jumps must clear play intents and perk out of crouch"
        );
    }

    #[test]
    fn crouch_gathers_track_live_facing_and_room_hold_suppresses_yawn_hide_song_and_play_crouch() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let mut t = awake(&mut pet, start, 5, 30);

        pet.facing_left = false;
        pet.set_action(PetAction::Crouch);
        t += Duration::from_millis(16);
        let f_crouch = pet.tick(sense(t, Some((5, 5))));
        assert_eq!(f_crouch.action, PetAction::Crouch);
        assert!(
            f_crouch.facing_left,
            "pounce crouch must update facing_left when target switches sides mid-coil"
        );

        let mut pet_wall = PetBrain::default();
        let t_wall = awake(&mut pet_wall, start, 5, 0);
        pet_wall.col = 0.0;
        pet_wall.facing_left = false;
        pet_wall.pending_pointer_pounce = Some((0.2, 5.0));
        let width = art_cols(8, 16);
        assert!(pet_wall.consume_pointer_pounce(&sense(t_wall, Some((5, 0))), width));
        assert!(
            pet_wall.facing_left,
            "wall-clamped pointer pounce must face the toy via center fallback"
        );

        let mut pet_hold = PetBrain::default();
        let mut t_hold = awake(&mut pet_hold, start, 5, 10);
        let (t_sat, _) = idle(&mut pet_hold, t_hold, (5, 10), SIT_AFTER + 0.5);
        t_hold = t_sat;
        pet_hold.force_seed(0);
        pet_hold.quiet = pet_hold.loaf_after() - YAWN_DUR * 0.5;
        assert!(
            pet_hold.sit_yawn(),
            "fixture: without room_hold, even seed yawns before loaf"
        );
        pet_hold.room_hold = true;
        assert!(
            !pet_hold.sit_yawn(),
            "room_hold withholds the loaf melt and must suppress sit_yawn"
        );

        pet_hold.hide_to = Some(20.0);
        pet_hold.hiding = true;
        pet_hold.play_to = Some((20.0, 5.0));
        pet_hold.play_land = Some(PlayLand::PointerFrolic);
        pet_hold.song_left = 2;
        pet_hold.pending_look = Some(30.0);
        pet_hold.look_at = Some(t_hold);
        pet_hold.groom_owed = true;
        pet_hold.set_action(PetAction::Crouch);
        t_hold += Duration::from_millis(16);
        let _ = pet_hold.tick(sense(t_hold, Some((5, 10))));
        t_hold += Duration::from_millis(16);
        let f_hold = pet_hold.tick(sense(t_hold, Some((5, 10))));
        assert!(
            pet_hold.hide_to.is_none()
                && !pet_hold.hiding
                && pet_hold.play_to.is_none()
                && pet_hold.play_land.is_none()
                && pet_hold.song_left == 0
                && pet_hold.pending_look.is_none()
                && !pet_hold.groom_owed
                && f_hold.action == PetAction::Sit,
            "room_hold must stand down hide_to, hiding, play_to, song_left, pending_look, and groom_owed"
        );
    }

    #[test]
    fn brief_droop_is_not_grief_and_ended_vigil_wakes_one_tick_to_stand_down() {
        let start = Instant::now();
        let mut pet = PetBrain::default();
        let _ = awake(&mut pet, start, 5, 10);

        pet.droop_brief = true;
        pet.set_action(PetAction::Droop);
        assert!(
            !pet.grieving(),
            "brief ear-flat droop must not report grieving()"
        );
        pet.set_action(PetAction::Leap);
        pet.droop_brief = true;
        pet.resume = Some((PetAction::Droop, 0.2));
        assert!(
            !pet.grieving(),
            "resumed brief ear-flat droop must not report grieving()"
        );
        pet.droop_brief = false;
        assert!(
            pet.grieving(),
            "resumed command-failure sulk droop must report grieving()"
        );
        pet.resume = None;
        pet.set_action(PetAction::Droop);
        assert!(
            pet.grieving(),
            "active command-failure sulk droop must report grieving()"
        );

        let (mut pet_vigil, mut t_vigil) = at_the_door(start);
        assert!(
            !pet_vigil.needs_frames() && pet_vigil.next_change_deadline(t_vigil).is_none(),
            "active vigil crouch must park the frame lane while the job is running"
        );
        pet_vigil.note_executing(t_vigil, false);
        assert!(
            pet_vigil.needs_frames() && pet_vigil.next_change_deadline(t_vigil).is_some(),
            "ending the executing phase on a vigil crouch must wake the frame lane to stand down"
        );
        t_vigil += Duration::from_millis(16);
        let f_stood = pet_vigil.tick(sense(t_vigil, Some((10, 20))));
        assert!(
            f_stood.action != PetAction::Crouch && !pet_vigil.vigil,
            "one tick after executing ends must clear the vigil crouch"
        );

        pet_vigil.deferred_hidden_landing = Some((12.0, 5.0));
        assert!(pet_vigil.carry_with_regrid(3.0, 24));
        assert_eq!(pet_vigil.deferred_hidden_landing, Some((12.0, 8.0)));
    }

    #[test]
    fn retire_unowned_retains_console_world_and_ink_spans_capacity() {
        use crate::pet_world::{PetPane, PetWorldFacts};
        use aterm_core::terminal::Terminal;

        let start = Instant::now();
        let mut pet = PetBrain::default();
        let _ = awake(&mut pet, start, 5, 10);
        pet.sense_ink(0, &[(1, 4), (6, 9)], Some(5));
        let mut term = Terminal::new(24, 80);
        let input = term.cell_frame(24, 80);
        let facts = PetWorldFacts::read(&term, 7);
        pet.observe_console(&input, &facts, PetPane::full(&input));
        let world_ptr: *const crate::pet_world::PetWorld =
            pet.console.world.as_deref().expect("world allocated");
        let spans_cap = pet.ink_spans.capacity();
        assert!(spans_cap >= 2);

        pet.retire_unowned();
        assert!(!pet.is_active());
        assert!(pet.ink_spans.is_empty());
        assert_eq!(pet.ink_spans.capacity(), spans_cap);
        let spare = pet
            .console
            .spare_world
            .as_deref()
            .expect("retired PetWorld should be parked in spare_world");
        assert_eq!(core::ptr::from_ref(spare), world_ptr);
    }
}
