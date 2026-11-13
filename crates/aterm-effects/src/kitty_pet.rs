// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Autonomous text-plane kitty companion ([`PetBrain`](crate::kitty_pet::PetBrain), [`PetFrame`](crate::kitty_pet::PetFrame)).

use core::f32::consts::TAU;
use core::time::Duration;

use aterm_core::terminal::caret_path::CARET_PATH_LEN;
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
mod seat;
#[cfg(any(test, debug_assertions))]
mod state_words;
#[cfg(test)]
mod steam_tests;
use console_life::ConsoleLife;
pub use console_life::{PetAttention, PetInputKind};
pub use seat::{SeatArt, SeatShadow};

const STATION_LEAD: f32 = 1.0;
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
const SETTLE_STEP_FACTOR: f32 = 1.0 - crate::color_math::exp_f32(-SETTLE_TICK / SPEED_TAU);
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
/// The fastest pace, cells a second, a landing re-aimed ahead of the first keys
/// typed while the cat was in the air takes the hand to keep up ([`PetBrain::tick`]'s
/// first keys): a fast typist's 50 ms a key.
const FIRST_KEYS_PACE: f32 = 20.0;
/// The room, cells, a landing re-aimed ahead of the first keys keeps over the
/// caret's reach at its touchdown: the run's start from the landing.
const FIRST_KEYS_ROOM: f32 = 1.0;
const RHYTHM_MOVES: u8 = 3;
const LEAD_STEP: f32 = 0.5;
const RHYTHM_WINDOW: f32 = 0.6;
const RHYTHM_WINDOW_DUR: Duration = Duration::from_millis(600);
const CARRY_COAST: f32 = 0.5;
const CARRY_ROOM: f32 = 1.0 + CARRY_COAST;
const KEY_BEAT_BLEND: f32 = 0.5;
const KEY_BEAT_LATE: f32 = 2.0;
const CHASE_GAIN_FAST: f32 = 6.0;
/// A cat already on the next line waits there for the wrap due within this many seconds
/// at the writing's pace ([`PetBrain::wrap_ahead_aim`]): up a row to the caret's and down
/// again at the wrap is two flights in that time where none is needed.
const WRAP_WAIT: f32 = 1.5;
/// The most rows a page moves up in one frame that [`PetBrain::sense_ink`] reads as one rise
/// ([`PetBrain::ride_the_page`]).
const PAGE_RISE_ROWS: usize = 3;

/// The rows a page's ink map moved up by from `old` to `new` ([`PetBrain::sense_ink`]): the
/// fewest, up to [`PAGE_RISE_ROWS`], that every lettered row of `old` below the top went up by
/// to the span it held, where `new` did not keep them all in place. A blank row may come up
/// written: an answer printed on the caret's empty line scrolls up with the rows over it, and
/// the rows that came on at the foot are the page's own. 0 for none, a map of another height,
/// or one with no lettered row to move.
#[inline]
fn page_rise(old: &[(u16, u16)], new: &[(u16, u16)]) -> u16 {
    let n = old.len();
    if n != new.len() {
        return 0;
    }
    let lettered = |&(first, end): &(u16, u16)| end > first;
    let held = |k: usize| {
        let mut any = false;
        for (i, span) in old.iter().enumerate().skip(k) {
            if lettered(span) {
                if new[i - k] != *span {
                    return false;
                }
                any = true;
            }
        }
        any
    };
    if held(0) {
        return 0;
    }
    (1..=PAGE_RISE_ROWS.min(n.saturating_sub(1)))
        .find(|&k| held(k))
        .map_or(0, |k| k as u16)
}

/// How much further ahead (cells) the station stands for writing that steps two cells a key
/// ([`PetBrain::wide_step`]).
const WIDE_STEP_LEAD: f32 = 2.0;
/// The latest steps of a run that must each move the caret more than a cell for the run to be
/// writing in wide glyphs ([`PetBrain::wide_run`]): the latest alone, so the first wide key of
/// `echo 你好世界` already stands the cat clear, and a lone wide glyph (or two keys echoed in
/// one present) holds the wide station only until the next one-cell key.
const WIDE_RUN_STEPS: u8 = 1;
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
const LETTER_VEIL_DWELL_DUR: Duration = Duration::from_millis(200);
/// How long (s) the rest veil takes to go fully on ([`PetBrain::veil_over_letters`]): one ease, never a
/// blink; letters an Enter printed step it on at once.
const LETTER_VEIL_REST_ON_S: f32 = 0.35;
/// One leg in this many that sets out while the hand is not writing crosses letters in front of
/// them, whole and undimmed ([`PetBrain::deal_front_pass`]); the rest go under them.
const FRONT_PASS_EVERY: u8 = 3;
/// How far (rows) a body's head keeps under a lettered row it bounds beneath
/// ([`PetBrain::arc_under_letters`]): a pixel or so of a row's height. It once reached a
/// quarter of a row into it, and a bound along the grid's last row under a command's echo laid
/// its ears on the echo's letters (review9's gallery, `enter-bottom`).
const HEAD_CLEAR: f32 = 0.04;
/// How far (rows) a body is drawn down its own row where its head would lie on the ink of the
/// row above ([`head_law`]): its height past one row, and a breath's swell, so its box's top
/// keeps to its own row.
const SINK_ROWS: f32 = ART_ROWS - 1.0 + 0.08;
/// The share of its height a body is drawn at where its head would lie on the ink of the row
/// above and the row under its feet holds ink too, so no blank row takes it down ([`head_law`]):
/// its box one row tall less a breath's swell, its feet where they stand.
const ROW_FIT: f32 = (1.0 - 0.08) / ART_ROWS;
/// THE BOX KEEPS OFF THE ROWS BESIDE ITS OWN: how a body standing on a row is drawn, `(sink,
/// fit)`: how far (rows) down its own row, and the share of its height it is drawn at. `above`:
/// whether the row above its feet holds ink over the body's columns; `below`: whether the row
/// under them does. The body stands [`ART_ROWS`] tall, so on its row it reaches into the row
/// above. Where that row holds none, the body is drawn as it stands. Where it holds ink, the
/// body is drawn [`SINK_ROWS`] down its row into the blank row under (its head in its own row,
/// its feet in the row under), and where the row under holds ink too (a prompt between two
/// lettered rows, a line edited inside a page) it is fitted to its own row ([`ROW_FIT`]),
/// whole and smaller, its feet where they stand: either way its box lies on no lettered cell
/// of either row (review9fix round 1's `steam`, where the prompt sits between two lettered
/// rows and the box lay on the row above's letters in 107 of 112 frames). The terminal's pet
/// reads it through [`PetBrain::head_target`], which holds a fitted body fitted along its row
/// ([`PetBrain::ease_head`]).
#[inline]
#[must_use]
pub const fn head_law(above: bool, below: bool) -> (f32, f32) {
    match (above, below) {
        (true, false) => (SINK_ROWS, 1.0),
        (true, true) => (0.0, ROW_FIT),
        (false, _) => (0.0, 1.0),
    }
}
/// How fast (rows a second) the body is drawn down its row and back ([`head_law`]):
/// [`SINK_ROWS`] in about a tenth of a second, one ease, never a jump.
const SINK_RATE: f32 = 8.0;
/// How fast (its height's share a second) the body is fitted to its row and back ([`head_law`]):
/// from whole to [`ROW_FIT`] in about a tenth of a second, one ease with the sink's.
const FIT_RATE: f32 = 4.5;
/// How long (seconds) the head law's longer ease takes, the sink's [`SINK_ROWS`] at
/// [`SINK_RATE`] or the fit's to [`ROW_FIT`] at [`FIT_RATE`]: the law reads the row above as
/// far ahead of a running body as it runs in this time ([`PetBrain::ease_head`]), so the body
/// is down or fitted by the time its box reaches a word.
const HEAD_EASE_S: f32 = {
    let sink = SINK_ROWS / SINK_RATE;
    let fit = (1.0 - ROW_FIT) / FIT_RATE;
    if sink > fit { sink } else { fit }
};
/// How far (cells) ahead of a body at rest the head law reads the row above, the way the
/// writing will carry it ([`PetBrain::ease_head`]): as far as a hop along the row at its
/// fastest ([`HOP_SPEED_MAX`]) takes it in [`HEAD_EASE_S`]. The first keys' spring off a gap
/// left the ground as the sink began, and its box met the next word's letters 0.7 of a cell
/// away (review9fix round 3's review).
const HEAD_REST_REACH: f32 = HOP_SPEED_MAX * HEAD_EASE_S;
const _: () = assert!(
    TOP_SPEED_MAX <= HOP_SPEED_MAX,
    "a walk set off the writing's way must reach no further in the ease than the rest read"
);
/// The share of its width a stretch drawn from the body's centre reaches past each of its
/// edges, at its widest (a launch's, [`LAUNCH_STRETCH`]): the head law reads that far past the
/// body ([`PetBrain::ease_head`]). A cat seated a twentieth of a cell past a line's end on the
/// grid's last row breathed its box two pixels onto the line's last letter.
const HEAD_STRETCH_PAD: f32 = 0.5 * LAUNCH_STRETCH;
/// Where a seat at `x` for a body `width` wide reads the row above for its head
/// ([`PetBrain::head_seat_along`], [`PetBrain::clear_of_letters_above`]): from as far before
/// it as a stretch drawn from the body's centre reaches ([`HEAD_STRETCH_PAD`]), as the head law
/// reads it ([`PetBrain::ease_head`]), or [`INK_GAP`] less [`INK_PAD`] where that is further
/// (`stretch` false: that alone, [`OldPath::SeatClearsTheStretch`] reverted). A seat the head
/// law would fit is no seat: the body is drawn whole where it rests.
#[inline]
const fn head_seat_from(x: f32, width: f32, stretch: bool) -> f32 {
    let gap = INK_GAP - INK_PAD;
    let pad = HEAD_STRETCH_PAD * width;
    if stretch && pad > gap {
        x - pad
    } else {
        x - gap
    }
}
/// The most cells of a word's trailing marks (`."` or `),`) a seat steps past as past the word
/// ([`PetBrain::word_marks_end`]).
const WORD_MARKS_MAX: usize = 2;
/// How far (cells, rows counted as [`ROW_AS_COLS`]) from its stand beside the caret the head
/// law may seat the cat ([`PetBrain::clear_of_letters_above`]): two pounces' gap. Further
/// away it is not by the caret.
const HEAD_SEAT_REACH: f32 = 2.0 * POUNCE_GAP;
/// How many rows under the caret's the head law looks for a seat
/// ([`PetBrain::clear_of_letters_above`]): the end of the line under the caret's, and a
/// composer's bottom rule and the footer under it.
const HEAD_SEAT_ROWS: u8 = 3;
/// How much further (cells) than the stand the ink left it at a seat under the caret's row
/// may lie for the head law to take it ([`PetBrain::clear_of_letters_above`]): a short hop.
const HEAD_SEAT_SLACK: f32 = POUNCE_JUMP;
/// How long (seconds) a caret the hand walks or a program moves stays put before the head law
/// seats the cat under the box near it ([`PetBrain::clear_of_letters_above`]): past a scrub's
/// pauses between words, short of the beat before typing begins where the caret went.
const HEAD_SEAT_SETTLE: f32 = 0.5;
/// The least step (cells) a seat search takes along a row ([`PetBrain::head_seat_along`]).
const SEAT_STEP_MIN: f32 = 1.0 / 16.0;
/// Clearance (cells) kept between a re-anchored footprint and the ink it fled.
const INK_GAP: f32 = 0.35;
const INK_EVICT_MAX: f32 = 4.0;
const INK_EVICT_SPEED: f32 = 12.0;
const REAIM_MAX_U: f32 = 0.85;
/// How far through a flight (`t / dur`) a caret's change of row still re-aims its landing
/// ([`PetBrain::retime`]): a prompt a program moves down as the cat comes down to it draws
/// the descent on to where the prompt went, short of the touchdown itself.
const ROW_REAIM_MAX_U: f32 = 0.98;
/// How near its end a share of a flight's way counts as run for a rebase
/// ([`PetBrain::rebased_from`]).
const REBASE_SHARE_EPS: f32 = 1e-3;
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
/// The longest a bound after the caret (the writing's wrap's crossing, an Enter's, a cleared
/// line's) is in the air, seconds ([`PetBrain::brisk_shape`]): an 80-column line's at the
/// animal's own pace ([`BIG_HOP_SPEED_MAX`]), so the crossing takes as long at any width.
const BRISK_DUR_MAX: f32 = 0.55;
/// The fastest a bound after the caret goes, cells a second ([`PetBrain::brisk_shape`]):
/// under its body a 60 Hz frame (a 15 x 28 cell's body is 5.2 cells; the effects present at
/// 60 Hz on a 120 Hz panel), on the big bound's high arc, the pace a 0.55 s crossing of a
/// 150-column line needs (260). The crossing the kitty motion doc calls a teleport went 1.45
/// bodies a frame under a one-cell arc.
const BRISK_SPEED_MAX: f32 = 280.0;
const _: () = assert!(
    BRISK_SPEED_MAX / 60.0 <= art_cols(15, 28) && BRISK_SPEED_MAX >= BIG_HOP_SPEED_MAX,
    "the wrap's crossing moves less than its body a 60 Hz frame"
);
const POUNCE_SPAN_MAX: f32 = HOP_SPEED_MAX * FLIGHT_MAX;
const WALL_SPAN_MAX: f32 = HOP_SPEED_MAX * WALL_HOP_DUR;
const QUIET_ONE_BOUND_SECS: f32 = 2.0 * BIG_FLIGHT_MAX;
const ROW_AS_COLS: f32 = 2.0;
const _: () = assert!(
    POUNCE_SPAN_MAX < BIG_JUMP_COLS,
    "the hop tier must end before the screen-crossing latch begins"
);

#[inline]
const fn span_eq(dcol: f32, drow: f32) -> f32 {
    crate::color_math::hypot_f32(dcol, ROW_AS_COLS * drow)
}

/// THE CARET'S CELL IS NO SEAT: whether a body drawn across the columns `(left, right)` on the
/// caret's row lies on the caret's cell `cc`. A box is its art and a transparent border, which
/// the caret's cell may meet: the box's first cell (by the margin, where a fitted body is drawn
/// at its whole box's right edge facing into the line, the fitted cat's fur began 17 of the
/// box's 40 px in on the trail gallery's 14 x 26 px cells, clear of a caret 0.81 of a cell under
/// the box) and [`INK_PAD`] at its end. Read past them, the 72-column `last-row` wall seat,
/// whose caret reads 4.5 and more, would be no seat; read short of them, at 71 columns the bar
/// caret's cell under the cat's fur read 3.71 ([`PetBrain::wall_meets_the_caret`]).
#[inline]
pub const fn lies_on_the_caret((left, right): (f32, f32), cc: u16) -> bool {
    let c = cc as f32;
    c > left && c < right - INK_PAD
}

/// The chosen show's and play's pace, a share of the pane a second ([`quiet_speed_max`]): a
/// bound after the caret is brisk ([`PetBrain::brisk_shape`]).
const QUIET_PANE_PER_SEC: f32 = 0.74;

#[inline]
const fn quiet_speed_max(cols: u16) -> f32 {
    (QUIET_PANE_PER_SEC * (cols as f32)).clamp(HOP_SPEED_MAX, BIG_HOP_SPEED_MAX)
}

const STREAM_RUN: u8 = 3;
const STREAM_WINDOW: f32 = 0.5;
const STREAM_LAPSE: f32 = 0.8;

// The settling caret. A program that repaints walks the caret about in a burst: Claude
// Code submitting the prompts queued behind a turn lays its transcript out again, a DEC
// 2026 frame every 50 ms or so, and walks its composer from row 22 to row 36 in eight
// moves over 424 ms. A flight launched at the first of them landed on a row the caret
// had left, and the cat leapt again. Two caret jumps the hand did not make
// ([`PetBrain::hand_moved`]), the second within [`SETTLE`] of the first, are a burst. A
// coil gathered for it holds its crouch ([`PetBrain::coil_waits`]) until the caret has
// stayed put for [`SETTLE`], re-aiming as it goes, and the cat leaves once, for where the
// caret settled; the ink eviction and the live-edge hug wait it out too, and a stream
// watch the burst armed stands down as it settles ([`PetBrain::repaint_settled`]). A lone
// jump, the hand's own moves and a stream slower than a line per [`SETTLE`] never make a
// coil wait. A burst that goes on past [`SETTLE_CAP`] is a stream, and the watch has it.

/// Seconds a caret a program is moving must stay put before a coil gathered for it
/// leaves; two of its jumps closer than this are one burst. Longer than the 50 to 100 ms
/// between a repainting program's frames.
const SETTLE: f32 = 0.15;
/// How many of a burst's own intervals between its jumps the caret must stay put for to have
/// rested ([`PetBrain::settle`]): a program that writes on at its own pace has not rested while
/// it keeps that pace, its next write late by a frame or two.
const SETTLE_RHYTHM: f32 = 2.0;
/// The longest a burst's own pace makes the caret stay put to have rested ([`PetBrain::settle`]):
/// the wait a program's lone jump along its row is given ([`LONE_SETTLE`]).
const SETTLE_RHYTHM_MAX: f32 = LONE_SETTLE;
/// Seconds from a burst's first jump after which no coil waits for it.
const SETTLE_CAP: f32 = 0.9;
/// A key pressed this recently moved the caret itself ([`PetBrain::hand_moved`]): its
/// echo, late by a slow program or link.
const HAND_ECHO: f32 = 0.3;
/// Seconds a paste keeps the caret's burst open ([`PetBrain::paste_holding`]): the
/// program lays the pasted text out, and a program that pastes and submits (a harness, an
/// agent's driver) presses Enter within this, its repaint carrying the caret on, as a person
/// who pastes a command presses Return (120 to 800 ms after it, r16 K3-3). The coil
/// gathered for the paste's own jump waits it out and leaves once, for where the caret
/// settles: launched at the paste's jump, it came down on the row the submit's repaint
/// had just left and leapt again. As long as a burst holds a coil at all ([`SETTLE_CAP`]).
const PASTE_SETTLE: f32 = SETTLE_CAP;
/// [`PASTE_SETTLE`] with [`OldPath::PasteHoldsForItsReturn`] reverted.
const PASTE_SETTLE_OLD: f32 = 0.5;
/// Seconds a coil waits out a lone jump a program made along the caret's row
/// ([`PetBrain::lone_settling`]): something it wrote on the line and may take straight back
/// (a prompt it types and submits, a placeholder it shows and clears). Leapt after at once,
/// the cat was in the air when the caret went back, and came down where it had gone.
const LONE_SETTLE: f32 = 0.4;
/// The least span (cells) along its own row a gather launches for
/// ([`PetBrain::bound_came_short`]): a target that came back within it while a big jump was
/// pending, or a gather held, is walked to.
const BOUND_SPAN_MIN: f32 = 0.5 * POUNCE_JUMP;
const HAND_ECHO_DUR: Duration = console_life::dur_from_secs_f32(HAND_ECHO);
/// Seconds the writing's wrap stays the writing's own for the bound that follows it
/// ([`PetBrain::writing_wrapped`]): the cat's gather and launch come well within it.
const WRITING_WRAP_FRESH: f32 = SETTLE_CAP;

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
const SIT_AFTER_DUR: Duration = Duration::from_millis(1400);
const SIT_FOLD: f32 = 0.22;
const SIT_AFTER_FOLD_DUR: Duration = Duration::from_millis(1620);
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

const BELL_LATCH_TTL_DUR: Duration = Duration::from_millis(600);
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
const V2_PERK_LATE_DUR: Duration = console_life::dur_from_secs_f32(V2_PERK_LATE);
const V2_CATCH_TTL: f32 = crate::rainbow_kitty::stardust::StarClass::M1
    .life_s(crate::rainbow_kitty::stardust::StarLane::Strike)
    - crate::rainbow_kitty::stardust::CATCH_FINISH_MS / 1000.0;
const V2_CATCH_TTL_DUR: Duration = console_life::dur_from_secs_f32(V2_CATCH_TTL);
const V2_CATCH_COOL: f32 = 2.0 * BAT_HOLD;
const FLOW_LATCH_TTL: f32 = 0.9;
const FLOW_LATCH_TTL_DUR: Duration = console_life::dur_from_secs_f32(FLOW_LATCH_TTL);

const LEASE_STANDOFF: f32 = LEAD_MAX;
const ROOM_CUE_TTL_DUR: Duration = FLOW_LATCH_TTL_DUR;
const EAR_FLAT_HOLD: f32 = 2.0 * PERK_HOLD;

const PET_HOLD: f32 = 1.1;
const PET_CONTENT: f32 = 0.08;

const TRICK_SETTLE: f32 = 0.70;
const TRICK_SETTLE_TENTATIVE: f32 = PURSUIT_QUIET;
const TRICK_TTL: f32 = 4.0;
const TRICK_TTL_DUR: Duration = Duration::from_secs(TRICK_TTL as u64);
const TRICK_CAP: f32 = 20.0;
const TRICK_CAP_DUR: Duration = Duration::from_secs(TRICK_CAP as u64);
const TRICK_HOLD: f32 = 4.0;
const TRICK_PLAY_HOLD: f32 = 2.0 * FROLIC_HOLD;
const TRICK_DUCK: f32 = HIDE_DWELL / 3.0;
const TRICK_CONTENT: f32 = CHEER_FAST_CONTENT;
const TRICK_CONTENT_COOL: f32 = 20.0;
const TRICK_SUBMIT_FORGIVEN: f32 = 2.0;
const TRICK_SUBMIT_FORGIVEN_DUR: Duration = Duration::from_secs(TRICK_SUBMIT_FORGIVEN as u64);
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
const GAZE_FOLLOW_SPEED_MIN: f32 = GAZE_FOLLOW_MIN / GAZE_FOLLOW_WINDOW;
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
pub(super) const LOOK_TTL_DUR: Duration = Duration::from_millis((LOOK_TTL * 1000.0) as u64);

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
/// The air a cat the writing reaches keeps past the cell its next step lands on, cells
/// ([`PetBrain::writing_clear`]).
const WRITING_AIR: f32 = 0.25;
/// The step a late frame's missed span is lived in ([`PetBrain::live_late_span`]): a steady
/// 60 Hz host's frame.
const LIVE_STEP: Duration = Duration::from_micros(16_667);
/// The longest span a late frame is lived through in steps ([`PetBrain::live_late_span`]): a
/// starved machine's stalls of its main thread (0.9 to 5.6 s, a window under a background QoS)
/// with room; a host away longer (a hidden window, a sleeping machine) meets the brain where it
/// was.
const LATE_SPAN_MAX: Duration = Duration::from_secs(8);
/// The longest span lived through whose caret the caret's path does not time
/// ([`PetBrain::live_late_span`]): its caret's move is met at the span's first step, a guess a
/// longer span only makes worse; and every span's, before a stall was lived through
/// ([`OldPath::StallIsLived`]).
const LATE_SPAN_UNTIMED_MAX: Duration = Duration::from_secs(1);
/// The most of a tick's span its motion spends ([`PetBrain::advance`]): a frame later than this
/// is lived in steps ([`PetBrain::live_late_span`]).
const STEP_DT_MAX: Duration = Duration::from_millis(100);
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
pub(super) const BAT_TTL_DUR: Duration = Duration::from_millis((BAT_TTL * 1000.0) as u64);
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
const TWITCH_COOL: f32 = 4.0;
const TWITCH_BOB: f32 = 0.02;
const ROOM_SIBLING_QUIET_DUR: Duration = Duration::from_secs(4);

const STRETCH_SPLIT: f32 = 0.55;
const STRETCH_LEAD: f32 = 0.20;
const STRETCH_AMP: f32 = 0.06;
/// The wake stretch's last moment, which rides the frame train ([`PetBrain::waking_moves`]): the
/// tick that ends the stretch hands the cat to the ladder, and a walk it starts takes its first
/// step from a frame that asked for the next, as on a host that ticks every frame.
const WAKE_HANDOFF: f32 = 1.0 / 60.0;

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
/// How long (s) the frame takes the body off the glass once the wall's law hides it with its box
/// on the line's letters or the caret ([`PetBrain::cut_on_the_letters`]): three frames at 60 Hz,
/// drawn at three quarters, a half and a quarter of the fade's alpha, and off in the fourth. The
/// fewest that still read as a fade: its steps span about 64 ms, longer than the eye's temporal
/// summation at a screen's light (about 50 ms), where two steps of a third (48 ms) or one of a
/// half (32 ms) are seen as one step, a pop. And at the writing's 20 keys a second it ends as the
/// next key's letter reaches the art: typed on without a pause or a run of spaces into the margin
/// of a full page's last row, the line met the art's ink in none of those three frames on 13 x 29
/// and 11 x 28 px cells, and would have from a fourth at two blank cells or fewer (the trail
/// gallery's `last-row`, Linux arm64, 2026-11-09).
const FADE_OFF_THE_LETTERS: f32 = 0.064;

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

/// Where a dust puff is born, in rows below the top of the feet row: on the ground line at the
/// paws, which stand on the row's foot (`row + 1`). The puff grows as it drifts up, so its lowest
/// pixel holds near that line all its life and never spills into the row below. Born at 0.31 it
/// floated at the cat's mid-back, and a stumble's puff left behind a running cat read as a stray
/// grey dot (dark steam t120).
const DUST_SEAT: f32 = 0.94;

/// How far (cells) behind the body a puff it kicked up keeps its whole look, and how far behind
/// it is gone ([`PetBrain::dust_by_the_heel`]): dust reads as kicked up only while it is at the
/// heel it came from. A stumble's puff lives half a second and a sprinting cat runs fifteen
/// cells in that time, so it stayed on the page far behind the cat as a stray grey ball (`steam`
/// t120, x382-401, the four companions on all four pages). The older seat hid that only by
/// accident, on that page: the console drops a mote whose box meets ink ([`console_life`]'s
/// `mote_clears`), the box a tile's radius round its centre, and born at the cat's mid-back the
/// puff's box reached the lettered row two over its paws a fifth of a second in; on a blank page
/// the same puff stayed.
const DUST_HEEL_NEAR: f32 = 1.0;
const DUST_HEEL_FAR: f32 = 4.0;

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

    /// Whether a mote of this kind the console hid waits at the brain's offer, which names the
    /// moment it could be drawn again or dropped ([`PetBrain::hidden_mote_offer`]), rather than
    /// on the frame train: each kind whose path and clearance the offer follows. The pop a wake
    /// startles holds the train only while it is drawn; landing dust holds it hidden too.
    #[inline]
    const fn offers_hidden(self) -> bool {
        matches!(self, Self::Zee | Self::ZeePop | Self::Note | Self::Heart)
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
    const fn drift(&self, u: f32) -> (f32, f32) {
        if matches!(self.kind, PetMoteKind::ZeePop) {
            return (self.col + self.dir * POP_RUN * u, self.row - POP_CLIMB * u);
        }
        let (base, run, sway, climb) = match self.kind {
            PetMoteKind::Zee => (0.30, 0.55, 0.10, 1.05),
            _ => (NOTE_BASE, NOTE_RUN, NOTE_SWAY, NOTE_CLIMB),
        };
        let (wobble, _) =
            crate::supernova::sin_cos_f32(TAU * (u * 0.8 + 0.31 * (self.seed & 3) as f32));
        (
            self.col + self.dir * (base + run * u) + sway * wobble,
            self.row - climb * u,
        )
    }

    #[inline]
    const fn drift_slope(&self) -> (f32, f32) {
        match self.kind {
            PetMoteKind::Zee => ZEE_DRIFT_SLOPE,
            PetMoteKind::ZeePop => (POP_RUN, POP_CLIMB),
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
    const fn note_geom(&self, clock: f64) -> Option<((f32, f32), f32)> {
        let u = self.age(clock);
        if !(u >= 0.0 && u < 1.0) {
            return None;
        }
        let su = look_slice_age(look_slice(u, PURR_MOTE_STEPS), PURR_MOTE_STEPS);
        Some((self.drift(u), note_scale(su)))
    }
}

const ZEE_DRIFT_SLOPE: (f32, f32) = (0.55 + 0.10 * TAU * 0.8, 1.05);
/// How far the pop a wake startles ([`PetMoteKind::ZeePop`]) drifts over its life, in columns
/// along its heading and in rows up.
const POP_RUN: f32 = 0.25;
const POP_CLIMB: f32 = 0.55;
const NOTE_DRIFT_SLOPE: (f32, f32) = (NOTE_RUN + NOTE_SWAY * TAU * 0.8, NOTE_CLIMB);
const NOTE_BASE: f32 = 0.18;
const NOTE_RUN: f32 = 0.30;
const NOTE_CLIMB: f32 = 0.85;

/// Where beat `n` of a clock beating every `every` seconds from `origin` falls.
#[inline]
const fn beat_at(origin: f32, every: f32, n: i64) -> f32 {
    origin + n as f32 * every
}

/// The beats a clock at `q` has reached: the most `n` whose [`beat_at`] is at or before `q`.
///
/// A beat's birth counts with this and its offer waits for `beat_at(.., n + 1)`, the same `f32`
/// sum, so a tick at the offer counts the beat. `floor((q - origin) / every)` alone can come a
/// beat short at `q == beat_at(.., n)` (26.8 rounds to 26.7999992, which is 3.9999993 beats of
/// 1.2 s past 22), while the offer, at zero seconds, drops out.
#[inline]
const fn beats_reached(q: f32, origin: f32, every: f32) -> i64 {
    let mut n = ((q - origin) * (1.0 / every)).floor() as i64;
    while beat_at(origin, every, n + 1) <= q {
        n += 1;
    }
    while beat_at(origin, every, n) > q {
        n -= 1;
    }
    n
}

/// The beat a clock at `q` comes to next, from beat 1, which its offer waits for.
#[inline]
const fn next_beat(q: f32, origin: f32, every: f32) -> i64 {
    let n = beats_reached(q, origin, every);
    if n < 1 { 1 } else { n + 1 }
}

#[inline]
const fn look_slice(u: f32, steps: f32) -> f32 {
    (u * steps).floor().min(steps - 1.0)
}

#[inline]
const fn look_slice_age(slice: f32, steps: f32) -> f32 {
    (slice + 0.5) * (1.0 / steps)
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

/// How far round a cheer's lanes (cells) a note already rising counts as one of its own
/// ([`PetBrain::spawn_cheer_motes`]).
const CHEER_AIR_SLACK: f32 = 2.0;

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
    /// The edge its width keeps ([`PetFrame::stretch_from`]): the body's own, so a ghost left
    /// standing where the body stood draws the box it drew.
    pub stretch_from: StretchFrom,
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
                from: self.stretch_from,
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
    /// The edge the width grows from ([`PetFrame::stretch_from`]).
    from: StretchFrom,
}

impl Stance {
    const NEUTRAL: Self = Self {
        scale_x: 1.0,
        scale_y: 1.0,
        lift: 0.0,
        from: StretchFrom::Centre,
    };

    #[inline]
    const fn of(frame: &PetFrame) -> Self {
        Self {
            scale_x: frame.scale_x,
            scale_y: frame.scale_y,
            lift: frame.lift,
            from: frame.stretch_from,
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
            from: to.from,
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
            (1.0 - (t - fade_start) * (1.0 / EDGE_FADE)).clamp(0.0, 1.0)
        };
        fade * self.peak.clamp(0.0, 1.0)
    }

    #[inline]
    const fn look_at(&self, t: f32) -> (f32, PetGlyphId, bool, Stance) {
        let span = (self.edge - self.from_col).abs();
        let dir = if self.edge < self.from_col { -1.0 } else { 1.0 };
        let run = (self.speed * t).min(span);
        let col = self.from_col + dir * run;
        if self.still {
            return (col, self.pose0, self.facing_left, self.stance0);
        }
        let stride = self.stride0 + run * (1.0 / RUN_STRIDE_CELLS);
        let cycle = PetBrain::CYCLE_RUN;
        let phase = if stride >= 0.0 && stride < 1.0 {
            stride
        } else {
            let r = stride - stride.floor();
            if r >= 1.0 { 0.0 } else { r }
        };
        let idx_raw = (phase * cycle.len() as f32) as usize;
        let idx = if idx_raw < cycle.len() - 1 {
            idx_raw
        } else {
            cycle.len() - 1
        };
        (col, cycle[idx], dir < 0.0, Stance::NEUTRAL)
    }

    #[inline]
    const fn stopped_at(self, t: f32) -> Self {
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

/// Where a drawn body's width grows from ([`PetFrame::stretch_from`], [`drawn_body_px`]). At
/// the body's unstretched width every rule draws the same box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StretchFrom {
    /// About the art's centre: both edges move.
    #[default]
    Centre,
    /// The unstretched box's left edge stays, and the width reaches right.
    Left,
    /// The unstretched box's right edge stays, and the width reaches left.
    Right,
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
    /// Dest-rect scale about the sprite centre (squash & stretch), or from the edge
    /// [`Self::stretch_from`] keeps.
    pub scale_x: f32,
    pub scale_y: f32,
    /// Which edge of its unstretched box the drawn width keeps
    /// ([`PetBrain::keep_stretch_off_caret`], [`PetBrain::keep_stretch_off_letters`]): the
    /// centre, but for a body stretched beside the caret or beside letters.
    pub stretch_from: StretchFrom,
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
        #[inline]
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
        if !matches!(self.stretch_from, StretchFrom::Centre) {
            h = aterm_hash::fnv_mix_u64(h, 0x5354_0000 | self.stretch_from as u64);
        }
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
                if !matches!(d.stretch_from, StretchFrom::Centre) {
                    h = aterm_hash::fnv_mix_u64(h, 0x5354_0000 | d.stretch_from as u64);
                }
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
    let dest_w = if stance.scale_x == 1.0 {
        nat_w
    } else {
        clamp_i32(
            (nat_w as f32 * stance.scale_x).round() as i32,
            1,
            u16::MAX as i32,
        )
    };
    let dest_h = if stance.scale_y == 1.0 {
        nat_h
    } else {
        clamp_i32(
            (nat_h as f32 * stance.scale_y).round() as i32,
            1,
            u16::MAX as i32,
        )
    };
    let grid_w = (cols as i32).saturating_mul(cell_w as i32);
    let grid_h = (rows as i32).saturating_mul(cell_h as i32);
    let left = (col * cell_w as f32).round() as i32;
    let cx = left + nat_w / 2;
    let lift_px = if stance.lift == 0.0 {
        0
    } else {
        (stance.lift * cell_h as f32).round() as i32
    };
    let baseline = ((row + 1.0) * cell_h as f32).round() as i32 - lift_px;
    let max_x = if grid_w > dest_w { grid_w - dest_w } else { 0 };
    let max_y = if grid_h > dest_h { grid_h - dest_h } else { 0 };
    let x = match stance.from {
        StretchFrom::Centre => cx - dest_w / 2,
        StretchFrom::Left => left,
        StretchFrom::Right => left + nat_w - dest_w,
    };
    let x0 = clamp_i32(x, 0, max_x);
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

/// A law that keeps the companion off the letters and icons it meets on its
/// paths, which a test's negative control reverts on its own
/// ([`PetBrain::set_old_path`]): each control names the one law its test
/// guards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u128)]
pub enum OldPath {
    /// It watches a program its submit started from beside the line
    /// ([`PetBrain::watching_from_the_line`]); reverted, it drops onto the
    /// program's row.
    WatchFromTheLine = 4,
    /// The first keys typed under its landing take the landing ahead of them;
    /// reverted, it comes down where the caret was when it left.
    FirstKeysAhead = 8,
    /// The head law's look ahead along a run reads as far as the body could run at its top
    /// speed in the ease ([`PetBrain::ease_reach_speed`]); reverted, at the speed it runs at
    /// as the ease begins, and a cat speeding up off the first keys met the next word's letters
    /// on the row above with its box before its sink was done.
    LookAheadAtTopSpeed = 16,
    /// A flight down to a lower row is drawn whole in the air and judged by the head law at
    /// its landing only for the landing's last [`HEAD_EASE_S`] ([`PetBrain::ease_head`]);
    /// reverted, the landing's sink eases in from the take-off, lowering the whole arc, and the
    /// wrap's crossing flies through the line it leaves.
    SinkAtTheLanding = 32,
    /// A cat down on a seat ahead of the caret on its row keeps it while the hand writes toward
    /// it ([`PetBrain::clear_of_letters_above`]); reverted, the first key pulls it back to a
    /// fitted stand within the hand's reach, and the writing sends it out to the seat again.
    SeatAheadPulledBack = 16777216,
    /// A stand the head law draws down its row needs no seat under the line
    /// ([`PetBrain::clear_of_letters_above`]); reverted, a cat at rest under a command's output
    /// hops a row down to the seat under the line and back up to the stand at the first key.
    SunkStandIsASeat = 33554432,
    /// A flight that left before the caret's jump came to rest takes the seat the cat will rest
    /// on once it does (the tick's flight block); reverted, it comes down on the stand by the
    /// caret and flies again to that seat.
    LandWhereItRests = 67108864,
    /// Writing that steps two cells a key keeps the station a cell further ahead
    /// ([`PetBrain::wide_step`]); reverted, the cat runs at the caret's heel and its tail meets
    /// the caret's cell at the last key.
    NarrowStation = 64,
    /// A cat running down a station that wide glyphs carry off runs at its fastest, its
    /// landing giving way to the run as the caret reaches it ([`PetBrain::wide_chase`]);
    /// reverted, a wide glyph's step lays the caret's cell under its tail while it catches up
    /// after a landing.
    WideChase = 128,
    /// The wide station and the fast chase read the run's latest steps
    /// ([`PetBrain::wide_run`]); reverted, they read its largest, and one two-cell step holds
    /// them for the rest of the run.
    WideByLargestStep = 2,
    /// A cat whose head would lie on the ink of the row above its feet is drawn down its own
    /// row, its box's top on the row's, where the row under it is blank under its body
    /// ([`PetBrain::head_target`]); reverted, its head lies on the row above's letters.
    HeadUnderLetters = 1,
    /// A bound along the body's row under a lettered row two above its feet keeps its head
    /// under that row ([`PetBrain::arc_under_letters`], [`HEAD_CLEAR`]); reverted, it reaches a
    /// quarter of a row into it.
    HeadClearInFlight = 256,
    /// A cat whose head would lie on the ink of the row above its feet, with ink in the row
    /// under them too, is fitted to its own row ([`PetBrain::head_target`], [`ROW_FIT`]);
    /// reverted, it is drawn whole on its row and its head lies on the row above's letters.
    RowFit = 512,
    /// A body fitted to its row stays fitted along it while the row above holds ink over it
    /// ([`PetBrain::ease_head`]); reverted, it is drawn down into each blank stretch of the row
    /// under and fitted again where that row holds ink, growing and shrinking as it goes.
    FitHeldAlongTheRow = 1024,
    /// A body the head law keeps in its row, or fitted to one, is drawn no taller than its
    /// fit: a pose's stretch (a leap's take-off, a purr's swell, an ear's twitch up) narrows
    /// it where its feet stand ([`PetBrain::sunk`]); reverted, the stretch takes its ears past
    /// the 0.08 of a row over them into the row above's letters.
    HeldNoTaller = 2048,
    /// The console judges the box the head law draws ([`PetBrain::head_drawn`]); reverted, it
    /// judges the whole body, and a body fitted clear of a dense page's ink stays latched under
    /// it.
    ConsoleReadsTheFit = 4096,
    /// A stance's stretch never carries the drawn box onto the caret's cell
    /// ([`PetBrain::keep_stretch_off_caret`]); reverted, a body stationed flush beside the caret
    /// lays its stretched box a pixel or two into the caret's cell.
    StretchOffCaret = 8192,
    /// A stance's stretch never carries the drawn body onto letters it keeps off
    /// ([`PetBrain::keep_stretch_off_letters`]); reverted, a body standing flush beside a line's
    /// last letters lays its stretched box back over them (the take-off's coil of a dog that
    /// stepped clear of `./k`, a body 6.25 cells across, onto the `k`'s ink).
    StretchOffLetters = 268435456,
    /// A landing's recovery or a commit's stare on the caret's row gives way to the run once
    /// the writing's next steps would reach the body ([`PetBrain::writing_reaches`]), and a
    /// landing holds its pose until it is no longer fresh ([`PetBrain::landing_fresh`]);
    /// reverted, the cat holds its pose, or stops as a stand, and the caret types on under it
    /// (the trail gallery's dog, `cjk` `t0`, looks10 rounds 3 and 4).
    StillUnderWriting = 16384,
    /// A body the head law has drawn down its row or fitted to it stays so along the row across
    /// a gap in the row above ([`PetBrain::ease_head`]); reverted, it rises whole in each gap
    /// wider than itself and is drawn down again at the next word, a rise and return about four
    /// times a second under ragged output (review9fix round 3's review).
    HeadHeldAcrossGaps = 32768,
    /// The head law reads the row above ahead of the body as far as its ease takes, and past
    /// each edge as far as a stretch reaches ([`HEAD_EASE_S`], [`HEAD_REST_REACH`],
    /// [`HEAD_STRETCH_PAD`], [`PetBrain::ease_head`]); reverted, it reads the body's own
    /// columns, and a body that sets off from a gap onto a word is drawn down late, its head on
    /// the word's first letters (review9fix round 3's review).
    HeadLooksAhead = 65536,
    /// A bound along the row is held under a lettered row two above its feet on the page as
    /// each frame in the air finds it ([`PetBrain::arc_under_letters`]); reverted, only as its
    /// take-off found it, and a command's last line printed there after the Return, over a
    /// prompt's blank line, lies under its head mid-bound.
    ArcJudgedInTheAir = 131072,
    /// A gap in the row above that holds a body in its row ([`OldPath::HeadHeldAcrossGaps`])
    /// has ink on both sides of the body: past the row above's last letter (a prompt
    /// segment's end, a command's echo, a powerline's filled tail) the line has ended, and the
    /// body it held is let go to its whole height ([`PetBrain::head_target`]); reverted, any
    /// ink along the row holds it, and a cat fitted under a powerline's first letters stayed a
    /// 40x24 token past them over the filled blank to the line's end (composed r0's `context`,
    /// all 64 frames, and 11 of 64 `enter-bottom.e470`).
    LineEndIsNoGap = 262144,
    /// A seat's head clears the letters of the row above before it by as far as a stretch
    /// drawn from the body's centre reaches ([`head_seat_from`], [`HEAD_STRETCH_PAD`]), as the
    /// head law reads that row ([`PetBrain::ease_head`]); reverted, by a twentieth of a cell
    /// ([`INK_GAP`] less [`INK_PAD`]), and the head law, reading the stretch's reach, fitted
    /// the body it seated there: a cat bounding back along the grid's last row to a seat just
    /// past the output line's end was a 42x23 token from the bound's take-off to its rest
    /// (composed r1's `enter-bottom.e470`, 6 of 24 frames; the rest a phase short of it).
    SeatClearsTheStretch = 524288,
    /// The head law's look ahead along a walk ([`OldPath::HeadLooksAhead`]) stops at the stand
    /// the walk goes to ([`PetBrain::ease_head`]); reverted, it reads the row above as far past
    /// the stand as the walk's speed would carry it, and a cat walking a few cells back to a
    /// whole seat beside a line's last letter read the letters past it, was fitted on the way
    /// and grew again at the seat: a dip and return.
    LookAheadStopsAtTheStand = 1048576,
    /// The head law judges a body again when the page under it changes, as well as when it
    /// moves ([`PetBrain::ease_head`]); reverted, only when it moves, and a command's last line
    /// printed over a cat resting on the grid's last row lay under its whole box until it next
    /// moved (a 70-column line after the Return: 31 frames, half a second).
    JudgedAsThePageChanges = 2097152,
    /// A stand the head law would fit to its row ([`ROW_FIT`]: ink over the body's columns on
    /// the row above and the row under) gives way to the nearest seat within
    /// [`HEAD_SEAT_REACH`] where the body is drawn whole, along its row or on a row under the
    /// caret's ([`PetBrain::clear_of_letters_above`]); reverted, a cat beside the writing keeps
    /// to its row fitted, a token at 0.54 of its height between two lettered rows for a whole
    /// line's typing and its Backspaces where the rows under were blank (composed r1's
    /// `steam`, O2(c), O3).
    WholeSeatOverFit = 4194304,
    /// A seat is no seat where the observed page has ink on its own row under the body
    /// ([`PetBrain::head_seat_along`]), whatever the ink ledger holds for that row; reverted,
    /// only the ledger's ink blocks it, and with the word pass off (no ledger span for a row
    /// the program wrote) the whole seat's search ([`OldPath::WholeSeatOverFit`]) sat the cat on
    /// the selected line under `context`'s prompt, fitted there over its letters.
    SeatReadsItsOwnRow = 8388608,
    /// A coil gathered for a bound after a program's lone jump to another row waits until the
    /// caret has rested ([`PetBrain::row_jump_settling`]), so the bound goes once to the seat
    /// the cat will rest on; reverted, it leaves for the stand beside the new prompt at once,
    /// and the brisk bound after an Enter came down there before the seat past the output's
    /// last letter was known, and flew to it after (r25 K8-3).
    BoundLeavesUnrested = 134217728,
    /// On the alternate screen, with no composer's rule by the line, the cat keeps its line's
    /// row: no seat on the rows under the caret's is sought or kept there
    /// ([`PetBrain::clear_of_letters_above`]); reverted, a cat
    /// by a full-screen prompt between two lettered rows sat three rows under it at rest and
    /// while the hand wrote, and flew up across the lettered row under the prompt at the first
    /// Backspace and back down across it at rest.
    FullScreenKeepsItsRow = 536870912,
    /// A body at rest sets off along its row drawn as the head law draws it for the way it
    /// goes ([`PetBrain::setting_off`], [`PetBrain::owes_the_way`]); reverted, the law reads
    /// that way from the first step or the take-off, and a cat seated whole flush past the row
    /// above's last letter laid its box on that letter's cell for three to five frames as it
    /// set off back into the line.
    WayBeforeSettingOff = 1073741824,
    /// A body the head law draws down its row is drawn no taller than its fit while it is
    /// drawn down, in the air as on the ground ([`PetBrain::drawn_no_taller`]): leaving the
    /// row for another and landing in it from one. Reverted, only a body the law holds along
    /// its row, or fits, is held so, and a cat drawn down under a lettered row on the alternate
    /// screen put its ears four pixels into the row above's letters at the take-off of each
    /// leap straight down (vim's `5j`), and one at the touchdown of each leap back up (`5k`).
    /// A take-off down off a row the body was held on is a crossing
    /// [`OldPath::CrossingRisesIntoTheLine`] draws no taller too, so with that law kept this
    /// one reverted shows only at the touchdowns.
    DrawnDownNoTaller = 2147483648,
    /// A dust puff keeps to the heel it came from ([`PetBrain::dust_by_the_heel`]); reverted, it
    /// lives its whole life wherever the body has run to, a stray grey ball far behind a
    /// sprinting cat.
    DustOutlivesTheHeel = 1 << 32,
    /// The margin takes the body off the line the writing runs into
    /// ([`PetBrain::margin_seat_off_the_line`]); reverted, it stands at the wall on the line, the
    /// last letters run under it and the wrap's crossing flies out through them.
    MarginKeepsTheLine = 1 << 33,
    /// A fitted body by the margin is drawn at its whole box's right edge
    /// ([`PetBrain::fitted_to_the_wall`]); reverted, at the box's middle, across the caret there.
    FitCentredAtTheWall = 1 << 34,
    /// A flight down to a lower row off a row the head law held the body on is drawn whole from
    /// its take-off, its box rising back into the line it was held under
    /// ([`PetBrain::left_top`]).
    CrossingRisesIntoTheLine = 1 << 35,
    /// A pane past the rows or the columns the console's map reads at once
    /// ([`crate::pet_world::MAX_WORLD_ROWS`], [`crate::pet_world::MAX_WORLD_COLS`]) is read about
    /// its caret, and about the body in a second window
    /// ([`crate::pet_world::PetWorld::body_window`]): the rows where it was last drawn and
    /// [`crate::pet_world::KEPT_ROWS`] over and under them, over as many columns as the map
    /// reads about the body's centre, for their glyphs (in a flight to another row whose landing
    /// the caret's window does not read whole, about that landing), so the laws that read the
    /// rows beside a body (the head law's, [`PetBrain::head_target`], and a bound's arc,
    /// [`PetBrain::arc_under_letters`]) read them wherever the caret leaps; a read along the
    /// body's row that neither window holds whole (longer than the map's columns, or across the
    /// edge between the windows) is read as far either way from the body as the window that
    /// reads it reaches ([`PetBrain::read_about`]). Reverted, the map is read about the caret
    /// alone and such a read reads nothing: on a pane 100 rows tall, once the caret leapt 40, 58
    /// or 70 rows down from a body at rest fitted between rows lettered to the margin, the head
    /// law read nothing of the body's rows and let the fit go as it gathered to leap, and its box
    /// met the row above's letters in 38 or 39, 43 and 12 frames of each run from rest, and in 9
    /// to 12 after the `steam` scene's Backspaces; on a pane 300 columns wide, leaping five rows
    /// down to the 290th column, in 37 to 39 after the Backspaces and 153 from rest, 40 rows
    /// down on a pane 100 rows tall in 113, and along the prompt's row to the 290th column in
    /// 128 on the rows over and under it; on a pane 1000 columns wide, along the row from the
    /// 15th column to the 990th, in 359; from the 85th row of a pane 160 rows tall, leaping 40
    /// and 58 rows down, in 38 or 39 and 43; at the foot of a pane 100 rows tall, leaping 88 rows
    /// up, in 10 standing on the prompt's row (every face, the cat and the dog, both ways of
    /// typing, Linux arm64, 2026-11-07 and 2026-11-08); and a bound back along the row from the
    /// 290th column rose into a row lettered two over its own in 1 or 2 frames of each run
    /// (Linux arm64, 2026-11-08).
    WorldReadsTheBodysRows = 1 << 36,
    /// The window about the body reads its cells' ground as the caret's window reads its own
    /// (`PetWorld::read_body`), and the clearance readers read it where the caret's window does
    /// not read their rectangle whole (`PetWorld::body_clearance`): the console's visibility hold
    /// judges the body where it is drawn ([`PetBrain::console_frame_pass`]). Reverted, the window
    /// about the body reads its glyphs alone: on a pane past the map's columns a body at rest
    /// outside the caret's window had its ground read nowhere, the hold went blind and hid it
    /// 0.75 s on (the console's `VETO_BLIND` and `VETO_DWELL`), and a flight it then set off on
    /// stayed off the glass until it landed: off the glass in 84 to 347 frames of a caret's leaps
    /// along the row of a pane 300 to 1,000 columns wide (ATERM-KI-1107-3).
    BodysWindowReadsItsGround = 1 << 40,
    /// The gap in the row above that holds a body in its row ([`PetBrain::gap_above`]) reads each
    /// side from the body's edge as far as the window that reads the cell beside it reaches
    /// ([`PetBrain::read_about`]). Reverted, a side longer than one window reads read nothing and
    /// was no ink: on a pane past the map's columns no gap held a body there, and the head law let
    /// it rise whole in each gap as if [`Self::HeadHeldAcrossGaps`] were reverted
    /// (ATERM-KI-1107-3).
    GapReadAboutTheBody = 1 << 41,
    /// A body standing off the line of the caret's row, clear of its letters and of the caret,
    /// keeps where it stands while the stand by a caret walked back into the line lies on them
    /// (or in a gap between them, [`Self::GapIsTheLines`]) ([`PetBrain::kept_off_the_line`]);
    /// reverted, it follows the caret onto the letters (ATERM-KI-1107-4).
    KeptOffTheLine = 1 << 42,
    /// A flight re-aimed in the air keeps the box's top a held body took off with
    /// ([`PetBrain::left_top`]) only while its way has never gone up ([`PetBrain::rebase`]).
    /// Reverted, the top outlived a flight up turned back down: a body fitted between rows
    /// lettered to the margin at a pane's foot leapt after a caret sent 30 rows up (a pane 40
    /// rows tall) or 52 (64 rows tall) and back 1.2 s later, its flight was turned back down in
    /// the air, and its box, held at its take-off top, moved 26 or 29 rows in one frame and lay
    /// on the letters of the row under the prompt's in 63 or 73 frames of each run (every face,
    /// the cat and the dog, Linux arm64, 2026-11-07).
    TurnKeepsTheTakeOffTop = 1 << 37,
    /// A body the margin holds at the wall on a line that leaves it no seat there, and none
    /// off the line or at rest, fades out where it stands and comes back at the first stand
    /// clear of the letters, or once the caret leaves the line's row ([`PetBrain::wall_seat`],
    /// [`PetBrain::unseated_sense`]); reverted with [`Self::CaretCellIsNoSeat`], it stands at the
    /// wall over the line's last letters, a cat tucked behind the block caret, then crumbs between
    /// those letters. Reverted alone, where the box at the wall lies on the caret's cell, that law
    /// sends the body to a seat off the line once the hand's rhythm closes, and its box lies on the
    /// letters through that wait.
    NoSeatHidesTheBody = 1 << 38,
    /// Where no key reached the console, a caret's own step forward onto a blank cell is the
    /// writing's ([`PetBrain::caret_typed_on`], read by [`PetBrain::writing_on`]); reverted, only
    /// the rhythm's [`RHYTHM_MOVES`] told it, and the first keys typed on past the console after a
    /// pause found the margin untaken: the body hopped over the caret onto the line's last
    /// letters at full alpha (the `last-row` line paused six keys short of its end, Linux arm64,
    /// 2026-11-07).
    CaretTypesOn = 1 << 39,
    /// A stand by the caret the head law draws down its row (its head under the row above's
    /// letters, the row under blank under it) is the cat's seat, as a stand whose head is clear
    /// is ([`PetBrain::clear_of_letters_above`]); reverted, it slides along its row to the first
    /// whole seat past the row above's letters, away from the caret: under a line of words a
    /// held delete sent the cat running back from the caret it followed, rising there and
    /// sinking again as the caret left it, and at rest it bounded 60 cells to a seat past the
    /// line's end (r29 K9-1, K9-2).
    SunkStandSlides = 1 << 43,
    /// A stand by the caret the head law fits to its row takes a whole seat only within
    /// [`HEAD_SEAT_REACH`], at rest as while the hand works ([`PetBrain::clear_of_letters_above`]);
    /// reverted, at rest it is sent along its row to the first whole seat however far, and the
    /// cat waited there up to 64 cells ahead while the person typed toward it (r29 K9-4).
    FittedStandSeatsFar = 1 << 44,
    /// Letters a program writes along the caret's line under a cat on that line, while the
    /// stream watch is up or once its writing through the cat goes on
    /// ([`PetBrain::writing_laid_under`]), send it once across the rule under that line (a
    /// composer's footer under its bottom rule), ground the writing never reaches
    /// ([`PetBrain::seat_off_the_writing`]: the tick's eviction, and a gather for the caret), and
    /// a cat off the line watches the writing from where it is
    /// ([`PetBrain::watch_off_the_writing`]); reverted, it steps off them along its row, into
    /// the writing's way, and the next chunk lays its letters over it again: a hop a chunk as a
    /// program typed a prompt into Claude Code's composer, the cat sitting on the new letters
    /// between them (r29 K9-3).
    WritingEvictsAlongItsRow = 1 << 45,
    /// A stand by the caret the head law fits to its row slides along the row to a whole seat
    /// only on a trip the cat makes anyway, or to the seat it already holds
    /// ([`PetBrain::clear_of_letters_above`]); reverted, the seat ahead that the writing's stand
    /// brings within [`HEAD_SEAT_REACH`] takes the cat running on ahead of the writing to wait
    /// there, and the hand's pause sends a cat fitted by the caret off to one (r29 K9-4).
    FittedStandRunsAhead = 1 << 46,
    /// A burst a program makes has rested once the caret stays put for [`SETTLE_RHYTHM`] of the
    /// burst's own intervals between its jumps, at least [`SETTLE`] ([`PetBrain::settle`]);
    /// reverted, for [`SETTLE`] whatever its pace, and a program writing a prompt in chunks
    /// [`SETTLE`] apart (a harness typing into Claude Code's composer) read as at rest between
    /// them: the stream watch stood down and its burst ended at each late chunk, and the cat
    /// went for each chunk's end as for a caret at rest (r29 K9-3).
    SettleIgnoresTheRhythm = 1 << 47,
    /// A paste holds the caret's burst open for [`PASTE_SETTLE`], within which a person's Return
    /// after a pasted command comes ([`PetBrain::paste_settle`]); reverted, for half a second,
    /// and a Return 0.8 s after the paste came while the cat was in the air for the pasted
    /// line's end: it came down on the new prompt's row 40 cells past the prompt and flew back
    /// to it (r29 K9-5).
    PasteHoldsForItsReturn = 1 << 48,
    /// A body the host carries with the rows it rests on is never hopped off them for the
    /// obstruction their move lays under it ([`PetBrain::console_carries_at`]); reverted, the cat
    /// on top of the harness's line over Claude Code's composer leapt a row up at each line a
    /// program's writing grew the composer by, unless a trip it had planned held it: four hops
    /// on the spot in the long insert's dictation (r29 K9-3).
    CarriedBodyHopsOff = 1 << 49,
    /// A paste opens a burst of its own: its echo joins no burst a program's output left
    /// behind ([`PetBrain::note_console_input`]); reverted, it joined one seconds settled, the
    /// paste's burst ran past [`SETTLE_CAP`] at once, and nothing waited the paste and its Return
    /// out: the cat stepped off the pasted letters for the line's end at once, came down on the
    /// new prompt's row there and flew back to the prompt (windowed zsh after a command's
    /// output, every delay, r29 K9-5).
    PasteOpensItsOwnBurst = 1 << 50,
    /// A whole seat along the row is held over the fitted stand by the caret only by a cat at
    /// that seat while the hand runs the caret no other way, and a cat a pounce's gap off makes
    /// a trip anyway only when it is not running the hand's caret down
    /// ([`PetBrain::clear_of_letters_above`]); reverted, a cat anywhere ahead of the stand held
    /// the seat, and one running a held delete down a pounce's gap behind took it: on the grid's
    /// last row under a line of words, a held delete that began a few cells short of the words'
    /// end sent the cat running up to 5 cells right, away from the caret deleting left, and
    /// waiting at the seat past the words until the caret was out of reach (r29 K9-1).
    SeatHeldAnywhereAhead = 1 << 51,
    /// A play's leap (a batted peek, a pounce at the pointer) comes down on the grid row nearest
    /// its aim (`PetAction::Crouch`'s play launch); reverted, at the aim's own fractional row,
    /// where the head law reads no body ([`PetBrain::head_target`]): the cat that batted a pasted
    /// command's word-cat walked back to the prompt 0.07 of a row up, drawn whole with its box
    /// into the command's line instead of drawn down by the caret (r29 K9-5).
    PlayLandsOffTheRows = 1 << 52,
    /// A frame a host owed and brought late (a starved machine's 0.1 to 0.7 s gaps) is reached
    /// through the span it missed, lived a sixtieth of a second at a time ([`PetBrain::tick`]);
    /// reverted, the span is a single step its motion takes a tenth of a second of, and a flight
    /// took a second and a half, the cat lagged the caret, and the keys it caught up with read
    /// as jumps it hopped for.
    LateSpanInSteps = 1 << 53,
    /// The span a late frame missed is lived with the caret where its path put it at each step,
    /// each move at its output batch's time ([`PetBrain::live_late_span`]), and a hidden caret's
    /// rest is counted from when it came ([`PetBrain::times_the_caret`]); reverted, the caret
    /// moves the whole way at the span's first step, and a starved host's moves of three to
    /// seven cells at once, a typist's echoes or a program's writing, read as jumps the cat
    /// pounced for.
    LateCaretAtItsTimes = 1 << 54,
    /// A body on glass keeps its place when its terminal switches between its main and its
    /// alternate screen, and travels to the new screen's caret ([`PetBrain::keeps_its_body`],
    /// [`crate::companion::CompanionOwner::rescreen_owner`]): the same glass and the same grid.
    /// Reverted, the owner fence crossfaded it as a surface replaced: a ghost faded where it was
    /// and the body came up at the new caret on the fence's own frame, so a cat asleep by the
    /// shell's caret was drawn at Claude Code's composer the frame its first screen came, still
    /// asleep, and a cat by the composer stood at the shell's prompt the frame the program left
    /// (r29 K10-1, K10-2).
    OtherScreenKeepsTheBody = 1 << 55,
    /// A run of strides a gather, a perk or a startle cuts within its floor keeps its stride on
    /// glass that long ([`PetBrain::hold_still_pose`]); reverted, the cut drew the stride for a
    /// single present: a cat that set off and crouched at once for the long insert's dictation
    /// chunk flashed one stride at 120 Hz (r29 K10-4).
    StrideKeepsItsFloor = 1 << 56,
    /// Chrome the host's frame pass does not carry the body off is hopped off at once: the brain
    /// leaves to the host only a body its ride carries ([`PetBrain::console_ride`]), and a pounce
    /// the caret's jump owes does not wait its gather out under the chrome. Reverted, any ride of
    /// the body's rows left it to the host, lawful or not, and a pending pounce waited: a composer
    /// that shrank under a cat on its second line as the program cleared its draft laid its top
    /// rule over the cat's head, the cat perked under it, the host did not carry it, and put it a
    /// row down in one frame (r29 K10-2).
    ChromeOverIsHoppedOff = 1 << 57,
    /// `trail status` names the pose the frame drew ([`PetBrain::pose_name`]); reverted, the
    /// brain's own under the still pose a hold kept on glass, and a sampler timed poses no
    /// present drew (r29 K10-4).
    NamesTheDrawnPose = 1 << 58,
    /// A frame a stall of the host's main thread held back (0.9 to 5.6 s on a starved machine) is
    /// a late frame like any other, its span lived through when the caret's path times it
    /// ([`LATE_SPAN_MAX`]); reverted, a span over a second met the brain in one step: a stall of a second and a half right after an
    /// Enter ended with the cat at the old prompt, where a steady host's had bounded to the new
    /// one, and it bounded at the next key instead (r29 K10-8).
    StallIsLived = 1 << 59,
    /// A body kept on glass across its terminal's switch of screens is judged by the head law
    /// on the new screen afresh, eased as a body that comes to letters ([`PetBrain::rescreen`]);
    /// reverted, the new screen's page read as letters printed over a body at rest, and the
    /// law took it down or fitted it at once: a cat standing by a pager's last line was drawn at
    /// half its size the present the pager gave the shell back, under the prompt's line.
    NewScreenEasesTheHead = 1 << 60,
    /// A body at rest kept on glass across its terminal's switch of screens sets off only once
    /// the caret has rested [`SETTLE`] on the new screen ([`PetBrain::rescreen_settling`]);
    /// reverted, it set off for the first caret the switch showed: a pager's exit restores the
    /// shell's cursor to its line's start a present before the shell's prompt puts it back, and
    /// the cat by `less`'s last line turned its back on the prompt for the seat past the line
    /// above, stood so 1.4 s, and turned again to walk to the prompt.
    NewScreenSettlesFirst = 1 << 61,
    /// A body slid up off a row it is still drawn down into ([`PetBrain::slide_to_row`], the
    /// margin's seat over a line whose row over it cleared) grows to a larger fit only once its
    /// sink has come down to the head law's on its new row ([`PetBrain::head_eased`]): out of the
    /// row it left where that law draws it on its own row, and no deeper into it than
    /// [`SINK_ROWS`] where the law sinks it into a blank row under it; reverted, it grew as it
    /// rose, and the wall holding its right edge widened its box leftward over the line's last
    /// letters (ATERM-KI-1107-4).
    FitWaitsForTheFeet = 1 << 62,
    /// A gap between the letters of the caret's row is the line's, as its letters are: a body
    /// standing off the line keeps where it stands while the stand by a caret walked back into
    /// the line lies in such a gap, letters read on the row both short of the stand and past it
    /// ([`PetBrain::kept_off_the_line`]); reverted, it left its stand across the line's last
    /// letters and the caret for the gap, and followed the caret from there onto the letters
    /// (ATERM-KI-1107-4).
    GapIsTheLines = 1 << 63,
    /// A body the wall's law hides ([`Self::NoSeatHidesTheBody`]) is drawn off the glass within
    /// [`FADE_OFF_THE_LETTERS`] of the tick its box meets the line's letters or the caret
    /// ([`PetBrain::cut_on_the_letters`]), while its fade, which every law reads, runs on;
    /// reverted, it lay over them for the whole fade ([`FADE_OUT`]), 28 frames at 60 Hz on a full
    /// page's last row (ATERM-KI-1107-4).
    LettersCutTheFade = 1 << 64,
    /// The caret's cell is no seat ([`lies_on_the_caret`]): the wall on the caret's row is no
    /// stand where the body drawn there lies on it ([`PetBrain::wall_meets_the_caret`]), and a
    /// body a program's writing holds where it stands is let go to its station once the caret
    /// comes into it ([`PetBrain::held_on_the_caret`]). Where the box the wall draws meets the
    /// line's letters while the hand writes, the wall's hide ([`Self::NoSeatHidesTheBody`])
    /// takes the body off the glass first, and on a page with no seat anywhere it keeps it off
    /// ([`Self::NoSeatAnywhereHides`]). Reverted with the hide, the wall holds and the
    /// writing's last cells run under the body: at 71 columns the fitted cat at the wall of a
    /// full page's last row sat in the bar caret's cell, which read 3.71 against the page; and
    /// reverted alone, the watch holds, so a page written to its last cell left the cat at the
    /// wall behind a block caret, all of it hidden but a sliver (the "crumb").
    CaretCellIsNoSeat = 1 << 65,
    /// A body on its feet rides the page up where the page scrolls letters under it
    /// ([`PetBrain::ride_the_page`]); reverted, it stays on its row of the glass, on the letters
    /// the scroll brought there.
    SeatRidesThePage = 1 << 66,
    /// On a page with no seat off the line anywhere ([`PetBrain::seat_off_the_line`]), a wall
    /// that lies on the caret's cell ([`PetBrain::wall_meets_the_caret`]) and holds the body no
    /// seat of its own ([`PetBrain::wall_unseated`]: the box it draws at rest there meets the
    /// line's letters) keeps the margin at the wall, so the wall's hide
    /// ([`Self::NoSeatHidesTheBody`]) takes the body off the glass until the line ends. Reverted,
    /// the wall side turned and the body stood at the station on the caret's line side, over the
    /// line's letters too: the trail gallery's `flood` at 72 columns, typed through the console,
    /// laid it on "gypsy" in every frame from 470 ms after the last key to the end, where the hide
    /// has it off the glass once the stream's watch lapses (800 ms after the last key).
    NoSeatAnywhereHides = 1 << 67,
    /// While the hand writes, the margin's hold on the wall side lapses where the wall meets the
    /// caret ([`PetBrain::wall_meets_the_caret`]) only once the caret stands in the row's last
    /// cell, where it stays until the next key wraps it: short of it the next keys take the caret
    /// on through the wall's cells. Reverted, the wall side turned in the cell before the last
    /// too: the trail gallery's `flood` at 70 columns, typed through the console, has the caret a
    /// frame there at each row's end, and the turn at the last row's end sent the body along that
    /// row's letters to the station as the caret folded home to the top row, its box on 3 and 4
    /// lettered cells where the wall kept it on 2, to 470 ms after the last key.
    TurnAtTheLastCell = 1 << 68,
}

impl OldPath {
    /// Every law.
    pub const EVERY: [Self; 69] = [
        Self::WatchFromTheLine,
        Self::FirstKeysAhead,
        Self::LookAheadAtTopSpeed,
        Self::SinkAtTheLanding,
        Self::SeatAheadPulledBack,
        Self::SunkStandIsASeat,
        Self::LandWhereItRests,
        Self::NarrowStation,
        Self::WideChase,
        Self::WideByLargestStep,
        Self::HeadUnderLetters,
        Self::HeadClearInFlight,
        Self::RowFit,
        Self::FitHeldAlongTheRow,
        Self::HeldNoTaller,
        Self::ConsoleReadsTheFit,
        Self::StretchOffCaret,
        Self::StretchOffLetters,
        Self::StillUnderWriting,
        Self::HeadHeldAcrossGaps,
        Self::HeadLooksAhead,
        Self::ArcJudgedInTheAir,
        Self::LineEndIsNoGap,
        Self::SeatClearsTheStretch,
        Self::LookAheadStopsAtTheStand,
        Self::JudgedAsThePageChanges,
        Self::WholeSeatOverFit,
        Self::SeatReadsItsOwnRow,
        Self::BoundLeavesUnrested,
        Self::FullScreenKeepsItsRow,
        Self::WayBeforeSettingOff,
        Self::DrawnDownNoTaller,
        Self::DustOutlivesTheHeel,
        Self::MarginKeepsTheLine,
        Self::FitCentredAtTheWall,
        Self::CrossingRisesIntoTheLine,
        Self::WorldReadsTheBodysRows,
        Self::BodysWindowReadsItsGround,
        Self::GapReadAboutTheBody,
        Self::KeptOffTheLine,
        Self::TurnKeepsTheTakeOffTop,
        Self::NoSeatHidesTheBody,
        Self::CaretTypesOn,
        Self::SunkStandSlides,
        Self::FittedStandSeatsFar,
        Self::WritingEvictsAlongItsRow,
        Self::FittedStandRunsAhead,
        Self::SettleIgnoresTheRhythm,
        Self::PasteHoldsForItsReturn,
        Self::CarriedBodyHopsOff,
        Self::PasteOpensItsOwnBurst,
        Self::SeatHeldAnywhereAhead,
        Self::PlayLandsOffTheRows,
        Self::LateSpanInSteps,
        Self::LateCaretAtItsTimes,
        Self::OtherScreenKeepsTheBody,
        Self::StrideKeepsItsFloor,
        Self::ChromeOverIsHoppedOff,
        Self::NamesTheDrawnPose,
        Self::StallIsLived,
        Self::NewScreenEasesTheHead,
        Self::NewScreenSettlesFirst,
        Self::FitWaitsForTheFeet,
        Self::GapIsTheLines,
        Self::LettersCutTheFade,
        Self::CaretCellIsNoSeat,
        Self::SeatRidesThePage,
        Self::NoSeatAnywhereHides,
        Self::TurnAtTheLastCell,
    ];
    /// Every law's bit ([`PetBrain::set_old_paths`], a test seam, so only where it is built).
    #[cfg(any(test, debug_assertions))]
    const ALL: u128 = {
        let mut all = 0;
        let mut k = 0;
        while k < Self::EVERY.len() {
            all |= Self::EVERY[k] as u128;
            k += 1;
        }
        all
    };
}

/// What the head law read where it judged a body ([`PetBrain::head_target`]).
#[derive(Clone, Copy, Debug)]
struct HeadRead {
    /// The sink and fit it draws the body with there ([`head_law`]).
    law: (f32, f32),
    /// The row above was read and holds no ink over the columns, and holds some on both
    /// sides of them: a gap a body the law holds crosses held ([`PetBrain::ease_head`]). A
    /// row above blank across (a page scrolled up a blank line, a screen cleared), or past its
    /// last letter (a line that has ended), is no gap: the body it held is let go
    /// ([`PetBrain::gap_above`]).
    gap_above: bool,
    /// The row under was read and holds no ink under them.
    blank_below: bool,
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
    /// A bound that follows the caret ([`PetBrain::brisk_shape`]): re-aimed and drawn out at
    /// its own pace ([`PetBrain::flight_cap`]).
    brisk: bool,
    /// The brisk bound's own pace (cells a second), as it launched: the animal's
    /// ([`BIG_HOP_SPEED_MAX`]) or the faster one a wide line's crossing takes in
    /// [`BRISK_DUR_MAX`] ([`BRISK_SPEED_MAX`] at most). Unread for any other flight.
    cap: f32,
    /// The furthest stage of the flight's poses drawn yet ([`PetBrain::flight_stage`]): a
    /// flight drawn out in the air ([`PetBrain::retime`]) never goes back from its descent to
    /// its leap.
    stage: u8,
    /// Where the box's top stood (a row coordinate) as a body the head law held down or fitted
    /// left the ground ([`PetBrain::left_top`]); `None` for a body that left whole, and once the
    /// flight's way has gone up ([`PetBrain::rebase`]).
    left_top: Option<f32>,
}

#[derive(Clone, Copy, Debug)]
struct FlightKind {
    big: bool,
    show: bool,
    hop: bool,
    drop: bool,
    brisk: bool,
}

impl FlightKind {
    const PLAIN: Self = Self {
        big: false,
        show: false,
        hop: false,
        drop: false,
        brisk: false,
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum WrapAhead {
    /// At the wall on the caret's row, the writing running into the margin.
    Approach,
    /// On the next line already, where the wrap brings the caret.
    Launched(f32, f32),
    /// The writing stopped with the wall seat on its words: the nearest seat by the caret
    /// that covers none, held until the wrap ([`PetBrain::margin_rest_seat`]).
    Rest(f32, f32),
    /// At the wall off the line the writing runs into, slid under it or (`over`) onto the blank
    /// row over it ([`PetBrain::margin_seat_off_the_line`]), the wrap to come. `kept` is the slack
    /// (cells) the margin holds at ([`PetBrain::at_the_margin`]): no less than it was taken at, so
    /// the caret that took it keeps it ([`PetBrain::margin_slide_lead`]).
    OffTheLine { row: f32, over: bool, kept: f32 },
}

impl WrapAhead {
    /// Waiting off the caret's row for the wrap: on the next line, at a rest seat, or slid off
    /// the line at the wall.
    #[inline]
    const fn off_row(self) -> bool {
        matches!(
            self,
            Self::Launched(..) | Self::Rest(..) | Self::OffTheLine { .. }
        )
    }
}

/// Where the margin holds a body at the wall on the caret's row, read afresh by the wall's law
/// ([`PetBrain::wall_seat`]).
#[derive(Clone, Copy, Debug, PartialEq)]
enum WallSeat {
    /// The wall holds it no seat: the box it draws at rest there meets a lettered cell of the
    /// caret's row, the head law seats it nowhere there, and no seat lies beside the line or
    /// within reach.
    NoSeat,
    /// The stand the margin holds it at, and the hold that keeps it there: a row beside the line
    /// ([`WrapAhead::OffTheLine`]), a seat within reach ([`WrapAhead::Rest`]), or the wall
    /// ([`WrapAhead::Approach`]).
    Seat((f32, f32), WrapAhead),
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
    /// The cat (`cursor_pet = "cat"`, the default).
    #[default]
    Cat,
    /// The dog (`cursor_pet = "dog"`).
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
    #[inline]
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
    #[inline]
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

/// Where the head law last judged a body ([`PetBrain::ease_head`]): its seat `(col, row)`, the
/// columns it read `(a, b)` and the page's content sequence it read them on.
type SinkSeat = ((f32, f32), (f32, f32), u64);

/// What a tick found as it began, for [`PetBrain::keep_place_on_restation`]: what the tick itself
/// sets off or places was measured on its own cells and is not carried.
#[derive(Clone, Copy, Debug)]
struct TickEntry {
    /// Where the body stood `(col, row)`.
    at: (f32, f32),
    /// The landing `(col, row)` of the flight it was in.
    landing: Option<(f32, f32)>,
    /// A split bound's second landing `(col, row)` ([`PetBrain::bound2`]).
    bound2: Option<(f32, f32)>,
}

/// The station a tick measured, for a later tick's [`PetBrain::keep_place_on_restation`].
#[derive(Clone, Copy, Debug, PartialEq)]
struct SeenStation {
    /// The tick's aim `(col, row)` ([`PetBrain::aim`]).
    at: (f32, f32),
    /// The art's width in columns it was measured with ([`art_cols`]).
    width: f32,
    /// The caret's cell `(row, col)` it was measured for.
    caret: (u16, u16),
}

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
    /// The caret's last move was a carriage return and line feed off a line it stood on:
    /// from past its start to the start of the row under it, not by a wrap, nor off a row
    /// where a running command's output ends ([`Self::console_output_ends_on`]: a stream's
    /// own newline). An Enter's echo, whoever pressed it ([`Self::watching_from_the_line`]).
    line_returned: bool,
    regrid_caret: Option<(u16, u16)>,
    last_now: Option<Instant>,

    flight: Option<Flight>,
    deferred_hidden_landing: Option<(f32, f32)>,
    retired_flight_lift: Option<(f32, f32)>,
    /// How far (rows) the body is drawn down its own row ([`Self::head_target`]): drawn last,
    /// and its ease from `sink_from` to `sink_to` since the clock `sink_since`, at
    /// [`SINK_RATE`]: by the time since the target changed, so every host's cadence draws it
    /// alike.
    sink: f32,
    sink_from: f32,
    sink_to: f32,
    sink_since: f64,
    /// The share of its height the body is drawn at ([`Self::head_target`]): drawn last, and
    /// its ease from `fit_from` to `fit_to` since `sink_since`, at [`FIT_RATE`].
    fit: f32,
    fit_from: f32,
    fit_to: f32,
    /// Where the head law's target was judged (the stand, or a flight's landing) and over which
    /// columns (the body's, or a bound's whole way along its row): it is judged again only
    /// when either moves, so a page that scrolls under a body at rest (a hidden caret's home)
    /// does not move it, and a bound along the row is judged over the body's own columns again
    /// where it lands.
    sink_seat: Option<SinkSeat>,
    /// The stand the last tick sent the body to (`(col, row)`, [`Self::station_safe`]): the head
    /// law's look ahead along a walk stops where the walk does ([`Self::ease_head`]).
    walk_to: Option<(f32, f32)>,
    /// The column a body at rest sets off for along its row this tick: a gathering flight's
    /// landing ([`Self::reads_the_way`]), or as far as a walk back against the writing's way
    /// goes in the ease while it is held at rest ([`Self::owes_the_way`]). The head law reads
    /// the row above over that way while the body is still at rest ([`Self::head_span`]).
    /// Cleared as each tick begins.
    setting_off: Option<f32>,
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
    /// How many of the run's latest steps in a row each moved the caret more
    /// than a cell ([`Self::wide_run`]).
    wide_steps: u8,
    key_beat: f32,
    key_pace: f32,
    wall_side: bool,
    pending_wall_transit: bool,
    /// The console's frame pass turned back the last tick's walk ([`Self::walk_turned_back`]):
    /// this tick hops, or pounces, over to where the walk was going as a
    /// [`Self::pending_wall_transit`] does. Spent by the tick after the frame pass that set it.
    chrome_hop: bool,
    wrap_back: bool,
    wrap_ahead: Option<WrapAhead>,
    /// The row of the line whose wall held the body no seat, while the hide that began there
    /// holds: the tick takes the caret as hidden ([`PetBrain::wall_seat`],
    /// [`PetBrain::unseated_sense`]). It ends once the caret is on another row, and on the row
    /// once the fade has run out and a stand clear of the letters has come
    /// ([`PetBrain::unseated_return`]). An Enter on a page's last row scrolls the line away and
    /// leaves the caret on that row: there the hide ends at the next line's first such stand.
    unseated: Option<u16>,
    /// The share of the body the frame draws while the wall's law hides it
    /// ([`PetBrain::cut_on_the_letters`]): all of it (1) but where the hide's box met the line's
    /// letters or the caret, falling to none over [`FADE_OFF_THE_LETTERS`] from there, and
    /// lifting as the body fades back in where the hide ends before the fade has run out.
    wall_cut: f32,
    /// Whether the wall's cut falls: the box met the line's letters or the caret while the hide
    /// held.
    wall_cut_falls: bool,
    /// When (brain clock) the writing last wrapped the caret back to a line's start
    /// ([`PetBrain::writing_wrapped`]): the bound that follows it is the writing's own.
    writing_wrap: Option<f64>,
    launch_running: bool,
    /// The bound being launched follows the caret ([`LeapPolicy::Quiet`]): the brisk one
    /// ([`PetBrain::brisk_shape`]).
    launch_brisk: bool,
    land_running: bool,
    retreat_run: u8,
    retreat_gap: f32,
    retreat_bottled: bool,
    ink_base: u16,
    ink_spans: Vec<(u16, u16)>,
    ink_scrolled: bool,
    /// The rows the ink map last moved up together ([`PetBrain::sense_ink`]), 0 for none: read
    /// once by [`PetBrain::ride_the_page`].
    ink_rose: u16,
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
    /// Test control: dust born at the old mid-back seat ([`DUST_SEAT`]).
    #[cfg(test)]
    dust_floats: bool,
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
    /// [`Self::clock`] the last brake began at: its skid, once on glass, stays there
    /// [`STILL_SECS`] before the arrival it ends in takes the cat.
    brake_at: f64,
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
    /// That still pose's scale as it went on, which a hold keeps drawing.
    still_scale: (f32, f32),
    /// Whether the last frame drew that still pose over the one the brain posed
    /// ([`Self::hold_still_pose`], [`Self::still_hold_edges`]).
    still_held: bool,
    /// Whether the pet asked for frames after its last tick ([`Self::needs_frames`]), so that its
    /// host ticks it again within a frame. A tick after one that did not ends a sleep: it moves
    /// the body by none of the sleep ([`Self::tick`]).
    asked_frames: bool,
    /// A tick that ended a sleep began what it spends none of the sleep on
    /// ([`Self::asked_frames`]): a body to set moving, a heat to rise. The frame after it is owed.
    frame_owed: bool,
    /// Where this tick's walk aims the body, once the tick has read it: the column and row it
    /// sets off for ([`Self::owe_set_off`]). `None` on a tick that reads none (no caret, reduced
    /// motion, the console's resident).
    aim: Option<(f32, f32)>,
    /// The body is left a whole pixel or a row off [`Self::aim`], unmoved: the next frame is owed,
    /// once ([`Self::owe_set_off`]).
    set_off_owed: bool,
    /// This tick's walk holds the body where it waits for the writing to reach it: the seconds
    /// after the tick until the writing it waits for may be over, by its key's age or its run's
    /// ([`SIT_AFTER_DUR`], [`SIT_AFTER`]). The next change offers it ([`Self::next_change_secs`]):
    /// the wait is at rest and asks no frames, and a host at the brain's offers that heard of no
    /// edge there set off whenever anything else came, the seat's edge a few frames on.
    writing_wait: Option<f32>,
    /// The drawn body and aim, in pixels and rows, that [`Self::set_off_owed`] last asked a frame
    /// for: a body that frame kept where it was is not asked for again until one of them moves.
    set_off_asked: Option<[i32; 4]>,
    /// The action began on this tick ([`Self::set_action`]).
    action_began: bool,
    /// What the rest the last tick left the pet in does over the span to the next: its gait
    /// rests ([`Self::enter_settled`], a held stand), and, arrived, its content tires. The span
    /// is that rest whatever the next tick brings, so that tick spends it first
    /// ([`Self::keep_rest`]).
    rest_kept: (bool, bool),
    /// [`Self::clock`] of the last caret move ([`Self::hold_still_pose`]).
    moved_at: Option<f64>,
    /// [`Self::clock`] when the terminal last switched screens under the body
    /// ([`Self::rescreen`], [`Self::rescreen_settling`]).
    rescreen_at: Option<f64>,
    still_console: bool,
    /// The still pose on glass ([`Self::still_since`]) is a beat of the sit
    /// ([`Self::hold_still_pose`]).
    still_sat: bool,
    stand_ends_leg: bool,
    drew_leg: bool,
    /// The run of strides on glass ([`Self::hold_still_pose`]): when its first stride was drawn,
    /// and the stride and scale drawn last.
    stride_on: Option<(f64, PetGlyphId, (f32, f32))>,
    console_posed: bool,
    deferred_arc: Option<(f32, f32, f32, f32, FlightKind)>,

    gallop_since: Option<f64>,
    running: bool,
    pending_drop: bool,
    drop_sit_t: f32,
    drop_stare: bool,
    scrabble: bool,
    commit_t: f32,
    /// The writing reached the cat held still ([`Self::writing_reaches`]): it runs to a cell
    /// clear of the writing's next steps until it is clear of them ([`Self::writing_near`]).
    writing_chase: bool,
    resume: Option<(PetAction, f32)>,
    bored_cool: f32,
    hide_to: Option<f32>,
    hiding: bool,
    hide_t: f32,
    wriggle_t: f32,
    twitch_t: f32,
    twitch_cool: f32,
    twitch_spent: bool,
    twitch_up: bool,
    last_burst: bool,
    droop_seated: bool,
    droop_brief: bool,
    room_lease: bool,
    room_quiet: bool,
    room_hold: bool,
    room_turn: u64,
    room_sibling: bool,
    room_sibling_at: Option<Instant>,
    room_sibling_spent: bool,
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
    /// The settling caret ([`SETTLE`]): the brain clock of the last caret jump the hand
    /// did not make, and of the first jump of the burst it belongs to (`None` until a
    /// second jump joins the first).
    jump_at: Option<f64>,
    burst_from: Option<f64>,
    /// The seconds between the burst's last two jumps, 0 for a jump that joined no burst
    /// ([`PetBrain::settle`]).
    jump_gap: f32,
    /// When a program's last jump along the caret's row wrote through the cat standing on it:
    /// from short of the body's columns to past them ([`PetBrain::writing_laid_under`]).
    written_through: Option<f64>,
    /// The last caret jump the hand did not make went along its row and joined no burst
    /// ([`PetBrain::lone_settling`]).
    lone_along: bool,
    /// The brain clock until which a paste keeps the caret's burst open ([`PASTE_SETTLE`]):
    /// set by a paste, kept by the Enter that follows it, let go by any other key.
    paste_hold: Option<f64>,
    /// The coil on glass has waited for a settling caret ([`PetBrain::coil_waits`]): it
    /// is the caret's, never the live-edge hug's, and it leaves for where the caret
    /// settled.
    coil_waited: bool,
    bound2: Option<(f32, f32, bool)>,
    land_span: f32,
    skid_from: f32,
    skid_dir: f32,
    /// How far the skid slides: up to [`SKID_CELLS`], to the stand as it was at touchdown.
    skid_len: f32,
    motes: [Option<Mote>; PET_MOTES_MAX],
    mote_serial: u8,
    departure_serial: u32,
    settle_serial: u8,
    /// The seat a wake ended in is kept until the caret moves ([`PetBrain::enter_settled`]).
    woke_seat: bool,
    /// The flight came down this tick on ground the host's chrome took while it flew, too late
    /// to re-aim: the cat hops off it on the next tick, and the host leaves it be till then.
    touched_on_chrome: bool,
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
    /// Whether the leg under way crosses letters in front of them ([`PetBrain::deal_front_pass`]);
    /// `None` between legs.
    front_pass: Option<bool>,
    /// The legs dealt a turn in front so far ([`PetBrain::deal_front_pass`]).
    front_pass_serial: u8,
    /// A test's negative control ([`Self::set_letters_unseen`]).
    #[cfg(any(test, debug_assertions))]
    letters_unseen: bool,
    /// A test's negative control ([`Self::set_letter_arrival_dwells`]).
    #[cfg(any(test, debug_assertions))]
    letter_arrival_dwells: bool,
    /// A test's negative control ([`Self::set_letter_step_rearms`]).
    #[cfg(any(test, debug_assertions))]
    letter_step_rearms: bool,
    /// A test's negative controls, a bit a law ([`Self::set_old_path`]).
    #[cfg(any(test, debug_assertions))]
    old_paths: u128,
    /// The host is presenting a frame the program has not finished ([`Self::note_repainting`]).
    repainting: bool,
    last_drawn: [f32; PET_DEPARTURES_MAX],
    last_grid: (u16, u16),
    last_pose: PetGlyphId,
    /// The pose the last frame drew ([`Self::pose_name`]): the brain's own pose with the still
    /// pose a hold kept on glass over it ([`Self::hold_still_pose`]), and its species' skin.
    drawn_pose: PetGlyphId,
    /// The caret's moves since the last frame, at their times ([`Self::note_caret_path`]).
    caret_path: [Option<(Instant, (u16, u16))>; CARET_PATH_LEN],
    /// The next visible change the last frame left due, for a brain that asked no frames: the
    /// deadline a host that draws on demand armed then ([`Self::live_late_span`]).
    span_due: Option<Instant>,
    last_facing: bool,
    last_stance: Stance,
    adopted_look: Option<AdoptedLook>,
    last_cell_h: u16,
    last_cell_w: u16,
    last_reduced: bool,
    col_at_tick: f32,
    home: f32,
    /// The station the last tick that measured one measured ([`Self::keep_place_on_restation`]),
    /// set again where each tick measures its own. A tick that measures none (no caret in the
    /// pet's space, a tick the console resident drew, a reduced-motion tick, a flight set off
    /// before the aim) keeps it.
    station_seen: Option<SeenStation>,
    evicting: bool,
    /// What the one seat rule carries from one asking to the next ([`Self::seat_shadow`]).
    seat: seat::SeatWire,
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
            line_returned: false,
            regrid_caret: None,
            last_now: None,
            flight: None,
            deferred_hidden_landing: None,
            retired_flight_lift: None,
            sink: 0.0,
            sink_from: 0.0,
            sink_to: 0.0,
            sink_since: 0.0,
            fit: 1.0,
            fit_from: 1.0,
            fit_to: 1.0,
            sink_seat: None,
            walk_to: None,
            setting_off: None,
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
            wide_steps: 0,
            rhythm_at: None,
            key_beat: 0.0,
            key_pace: 0.0,
            wall_side: false,
            pending_wall_transit: false,
            chrome_hop: false,
            wrap_back: false,
            wrap_ahead: None,
            unseated: None,
            wall_cut: 1.0,
            wall_cut_falls: false,
            writing_wrap: None,
            launch_running: false,
            launch_brisk: false,
            land_running: false,
            retreat_run: 0,
            retreat_gap: 60.0,
            retreat_bottled: false,
            ink_base: 0,
            ink_spans: Vec::new(),
            ink_scrolled: false,
            ink_rose: 0,
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
            #[cfg(test)]
            dust_floats: false,
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
            brake_at: 0.0,
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
            still_scale: (1.0, 1.0),
            still_held: false,
            asked_frames: false,
            frame_owed: false,
            aim: None,
            set_off_owed: false,
            writing_wait: None,
            set_off_asked: None,
            action_began: false,
            rest_kept: (false, false),
            moved_at: None,
            rescreen_at: None,
            still_console: false,
            still_sat: false,
            stand_ends_leg: false,
            drew_leg: false,
            stride_on: None,
            console_posed: false,
            deferred_arc: None,
            gallop_since: None,
            running: false,
            pending_drop: false,
            drop_sit_t: 0.0,
            drop_stare: false,
            scrabble: false,
            commit_t: 0.0,
            writing_chase: false,
            resume: None,
            bored_cool: 0.0,
            wriggle_t: 0.0,
            hide_to: None,
            hiding: false,
            hide_t: 0.0,
            twitch_t: 0.0,
            twitch_cool: 0.0,
            twitch_spent: false,
            twitch_up: false,
            last_burst: false,
            droop_seated: false,
            droop_brief: false,
            room_lease: false,
            room_quiet: false,
            room_hold: false,
            room_turn: 0,
            room_sibling: false,
            room_sibling_at: None,
            room_sibling_spent: false,
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
            jump_at: None,
            jump_gap: 0.0,
            written_through: None,
            burst_from: None,
            lone_along: false,
            paste_hold: None,
            coil_waited: false,
            bound2: None,
            land_span: 0.0,
            skid_from: 0.0,
            skid_dir: 0.0,
            skid_len: 0.0,
            motes: [None; PET_MOTES_MAX],
            mote_serial: 0,
            departure_serial: 0,
            settle_serial: 0,
            woke_seat: false,
            touched_on_chrome: false,
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
            front_pass: None,
            front_pass_serial: 0,
            #[cfg(any(test, debug_assertions))]
            letters_unseen: false,
            #[cfg(any(test, debug_assertions))]
            letter_arrival_dwells: false,
            #[cfg(any(test, debug_assertions))]
            letter_step_rearms: false,
            #[cfg(any(test, debug_assertions))]
            old_paths: 0,
            repainting: false,
            last_drawn: [0.0; PET_DEPARTURES_MAX],
            last_grid: (0, 0),
            last_pose: PetGlyphId::PetSleep0,
            drawn_pose: PetGlyphId::PetSleep0,
            caret_path: [None; CARET_PATH_LEN],
            span_due: None,
            last_facing: false,
            last_stance: Stance::NEUTRAL,
            adopted_look: None,
            last_cell_h: 0,
            last_cell_w: 0,
            last_reduced: false,
            col_at_tick: 0.0,
            home: 0.0,
            station_seen: None,
            evicting: false,
            seat: seat::SeatWire::new(),
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
    const fn lead(&self) -> f32 {
        self.lead_at(self.run_t)
    }

    #[inline]
    const fn lead_at(&self, run_t: f32) -> f32 {
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
    const fn carry(&self, stand: f32, run_t: f32) -> f32 {
        if !self.carrying_run() {
            return 0.0;
        }
        let (walk, coast) = Self::carry_legs_from(stand);
        let due = walk / self.key_pace;
        if run_t <= due {
            self.key_pace * run_t
        } else if coast > 0.0 {
            walk + coast
                * (1.0 - crate::color_math::exp_f32(-(run_t - due) * self.key_pace / coast))
        } else {
            walk
        }
    }

    #[inline]
    const fn carry_speed(&self) -> f32 {
        if !self.carrying_run() {
            return 0.0;
        }
        let (walk, coast) = self.carry_legs();
        let due = walk / self.key_pace;
        if self.run_t <= due {
            self.key_pace
        } else if coast > 0.0 {
            self.key_pace * crate::color_math::exp_f32(-(self.run_t - due) * self.key_pace / coast)
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

    /// THE CAT HIDES ONLY WHERE IT MAY WALK. The spot a hide walks to: behind the end of the
    /// words on the cat's row, within [`HIDE_RANGE`] of it, where the console lets a body walk
    /// and crouch ([`Self::console_turns_back`]) on a cell of `cell` in a grid of `grid` (`(cols,
    /// rows)`). Behind words the host reserves (aterm's rewrite of Claude Code's mode row holds
    /// the whole extent it wrote, its blanked tail included), the frame pass sent every step of
    /// the walk back to the perch beside them, the hide never arrived, and the cat ran on the spot
    /// under a busy composer for as long as nobody typed (the owner, 2026-10-26: "the cursor kitty
    /// pet got stuck running in place"). `None` where no such spot is near.
    #[inline]
    const fn hide_spot(&self, width: f32, cell: (u16, u16), grid: (u16, u16)) -> Option<f32> {
        let Some((first, end)) = self.ink_span(self.row) else {
            return None;
        };
        if end - first < 2.0 {
            return None;
        }
        let dest = (end - width * 0.7).max(first - width * 0.3);
        if (dest - self.col).abs() > HIDE_RANGE
            || self.console_turns_back((dest, self.row), cell, grid)
        {
            return None;
        }
        Some(dest)
    }

    /// A walk the console turned back: the frame pass put the body back on the perch it was
    /// leaving ([`Self::console_turns_back`]). Walked on, every step was put back the same way,
    /// and the cat ran on the spot. The hide or the chase it walked for is given up and the cat
    /// stops where it was put. Any other walk (the one to the cat's station across the host's
    /// chrome on its row: a chip between the cat and the caret it walks back to) hops over the
    /// chrome to where it was going on the next tick, as over the caret ([`Self::chrome_hop`]):
    /// left to the station, every step was put back for as long as the station stayed across
    /// the chrome.
    #[inline]
    pub(super) const fn walk_turned_back(&mut self) {
        let chasing = self.pursuit_t.is_some();
        if self.hide_to.is_none() && !chasing {
            self.chrome_hop = true;
            return;
        }
        self.hide_to = None;
        if chasing {
            self.pursuit_t = None;
            self.pursuit_cool = self.pursuit_cool.max(PURSUIT_COOL);
        }
        self.speed = 0.0;
    }

    #[inline]
    const fn begin_hide(&mut self) {
        self.hide_to = None;
        self.hiding = true;
        self.hide_t = HIDE_DWELL;
        self.set_action(PetAction::Crouch);
    }

    #[inline]
    const fn hold_lead(&mut self) {
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
    const fn station_speed(&self) -> f32 {
        let (pace, rate) = if self.carrying_run() {
            if self.wall_side {
                return 0.0;
            }
            (self.key_pace, self.carry_speed())
        } else {
            let p = self.pace();
            (p, p)
        };
        rate * ((pace - LAT_SPEED) * (1.0 / (RUN_SPEED - LAT_SPEED))).clamp(0.0, 1.0)
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
        RUN_STRIDE_CELLS * (speed.abs() * (1.0 / MAX_SPEED)).max(1.0)
    }

    #[inline]
    const fn runs_down(&self, gap: f32) -> bool {
        self.rhythm_open() && gap * (self.run_dir as f32) > 0.0
    }

    #[inline]
    const fn station_velocity(&self) -> f32 {
        if self.run_dir < 0 && self.rhythm_open() {
            let p = self.run_pace;
            return -p * ((p - LAT_SPEED) * (1.0 / (RUN_SPEED - LAT_SPEED))).clamp(0.0, 1.0);
        }
        self.station_speed()
    }

    #[inline]
    /// How long the crossing back across a wrap takes the cat ([`Self::wrap_back_aim`]): its
    /// coil, and its brisk bound ([`Self::brisk_shape`]).
    const fn flight_eta(&self, to_col: f32, to_row: f32, cols: u16) -> f32 {
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
            coil + Self::bound_shape(span, true, cols).0
        }
    }

    #[inline]
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
        let mut iter = 0;
        while iter < 3 {
            let eta = self.flight_eta(aim, row, cols);
            aim = ((f32::from(cc) - self.run_pace * eta).max(0.0) + STATION_LEAD).min(limit);
            iter += 1;
        }
        Some(
            match self.console_station((aim, row), (rows, cols), width) {
                Some(s) => s,
                None => (aim, row),
            },
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

    /// The pace (cells a second) the first keys typed under a landing are taken to run on at
    /// ([`Self::tick`]'s first keys): the hand's own over the keys of its run so far, up to a
    /// fast hand's ([`FIRST_KEYS_PACE`]); none for a key that begins a run, whose pace no key
    /// before it has told.
    #[inline]
    const fn first_keys_pace(&self) -> f32 {
        if self.run_dir > 0 && self.run_count >= 2 {
            self.run_pace.clamp(0.0, FIRST_KEYS_PACE)
        } else {
            0.0
        }
    }

    /// The writing's pace (cells a second) over its recent keys, as the last key left it: not
    /// worn down by the time since ([`Self::age_gesture_windows`] decays `run_pace` between
    /// keys, so a wrap a steady hand will reach reads as further off after every key).
    #[inline]
    const fn writing_pace(&self) -> f32 {
        if self.run_secs > 0.0 {
            (self.run_cells / self.run_secs).clamp(0.0, VEL_MAX)
        } else {
            0.0
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

    /// The writing runs on toward the margin: its rhythm is open ([`Self::typing_on`]), or the
    /// hand's own last key, under [`SIT_AFTER`] ago, wrote a letter on a forward run, or, where
    /// no key reached the console in that time (keys typed past it, as `aterm ctl key` types
    /// them), the caret's own last move was a key's ([`Self::caret_typed_on`]).
    #[inline]
    fn writing_on(&self) -> bool {
        self.typing_on()
            || (self.run_dir > 0
                && self.watch_heat < WATCH_GATE
                && match self.key_within_dur(SIT_AFTER_DUR) {
                    Some(kind) => kind == PetInputKind::Text,
                    None => self.caret_typed_on(),
                })
    }

    /// The caret's last move was a key's on a forward run, as at the end of a line being
    /// written: it moved within the rhythm's window ([`RHYTHM_WINDOW`]), a key's step forward
    /// ([`TYPING_STEP_MAX`] cells at most), onto a cell the letter reader sees blank (a letter or
    /// a space typed behind it; a move along a line's letters stands it on one). `false` where the
    /// reader certifies nothing there. Read where no key reached the console
    /// ([`Self::writing_on`]): there the rhythm's [`RHYTHM_MOVES`] were the only sign of a hand,
    /// and the first keys typed on after a pause, short of them, found the margin untaken. The
    /// wall side crossed the stand to the caret's left, and the body hopped over the caret onto
    /// the line's last letters at full alpha (Linux arm64, 2026-11-07: the `last-row` line paused
    /// six keys short of its end and typed on past the console, on 11 x 28 px cells four blank
    /// cells past the line, which seat the body: 51 frames on its letters, 38 at full alpha).
    /// Reverted by [`OldPath::CaretTypesOn`].
    #[inline]
    fn caret_typed_on(&self) -> bool {
        let Some((cr, cc)) = self.last_caret else {
            return false;
        };
        if self.old_path(OldPath::CaretTypesOn) {
            return false;
        }
        let cc = f32::from(cc);
        self.run_count >= 1
            && self.run_step <= TYPING_STEP_MAX
            && matches!(
                self.observed_ink_in_span(f32::from(cr), cc, cc + 1.0),
                Some(false)
            )
    }

    /// The console is at rest: the caret has stood still and no key of any kind was pressed
    /// for [`SIT_AFTER`], the time the cat takes to sit ([`Self::margin_rest_seat`]). A key
    /// that moves the caret away from the margin, or a program still walking it, is no rest.
    #[inline]
    fn hand_stopped(&self) -> bool {
        self.quiet >= SIT_AFTER && self.key_within_dur(SIT_AFTER_DUR).is_none()
    }

    /// Writing the hand began while the cat was in the air has run on past where it comes
    /// down: its stand is ahead on the landing's row by more than the skid. A heavy landing
    /// would plant it for [`BIG_LAND_DUR`] on the letters while the caret runs away, and it
    /// would chase them after, veiled on them (the writing's first keys, before its rhythm
    /// opens at [`RHYTHM_MOVES`]).
    #[inline]
    fn writing_ran_past(&self, f: &Flight, target: f32, target_row: f32) -> bool {
        !self.stream
            && self.play_land.is_none()
            && self.run_dir > 0
            && self.key_within(f.t) == Some(PetInputKind::Text)
            && (target_row - self.row).abs() < 0.5
            && target - self.col > SKID_CELLS + ARRIVED
    }

    /// The hand moved the caret ([`HAND_ECHO`]): a key that moves it (a letter, a delete,
    /// a navigation key, a kill) pressed just now. A submit or a paste hands the caret to
    /// the program, whose repaint may walk it about ([`SETTLE`]).
    #[inline]
    fn hand_moved(&self) -> bool {
        matches!(
            self.key_within_dur(HAND_ECHO_DUR),
            Some(kind) if !matches!(kind, PetInputKind::Submit | PetInputKind::Paste)
        )
    }

    /// A program's burst is still moving the caret ([`SETTLE`]): two jumps the hand did
    /// not make, and the last of them under [`SETTLE`] old; or a paste's jump, while the
    /// paste still holds the burst open ([`Self::paste_holding`]).
    #[inline]
    const fn caret_moving(&self) -> bool {
        self.burst_from.is_some() && (self.jumped_within(self.settle()) || self.paste_holding())
    }

    /// A program's lone jump along the caret's row is under [`LONE_SETTLE`] old: no burst
    /// behind it, no stream watching it.
    #[inline]
    const fn lone_settling(&self) -> bool {
        self.lone_along
            && self.burst_from.is_none()
            && !self.stream
            && self.jumped_within(LONE_SETTLE)
    }

    /// A program's lone jump to another row (the prompt an Enter's command printed, the row a
    /// program moved the caret to) is under [`SETTLE`] old: no burst behind it, and none can be
    /// told from it before then. A coil gathered for a bound after it leaves once the caret has
    /// rested ([`Self::coil_waits`]), when the seat the cat will rest on is known
    /// ([`Self::clear_of_letters_above`], [`Self::caret_rested_after_jump`]): the bound goes
    /// once, to that seat. Launched for the stand beside the prompt, the brisk bound after an
    /// Enter came down there before the seat past the output's last letter was known, and flew
    /// to it a second later; the slow one was turned round in the air to it (r25 K8-3). So does
    /// a hop after the hand's Enter: one for the stand by the prompt under a command's long line
    /// came down there and flew to the seat past the line's end 0.8 to 1.2 s later, and on to a
    /// further one (`pwd` printing 70 columns: three flights for one Enter). Any other hop
    /// leaves at once: the writing a host's keys begin on a new row may already run under it.
    /// So does the writing's wrap ([`Self::writing_wrapped`]): the writing runs on after it.
    #[inline]
    const fn row_jump_settling(&self) -> bool {
        !self.old_path(OldPath::BoundLeavesUnrested)
            && !self.lone_along
            && self.burst_from.is_none()
            && !self.stream
            && !self.writing_wrapped()
            && self.jumped_within(self.settle())
    }

    /// How long (s) a paste holds the caret's burst open ([`PASTE_SETTLE`],
    /// [`OldPath::PasteHoldsForItsReturn`]).
    #[inline]
    pub(super) const fn paste_settle(&self) -> f32 {
        if self.old_path(OldPath::PasteHoldsForItsReturn) {
            PASTE_SETTLE_OLD
        } else {
            PASTE_SETTLE
        }
    }

    /// A paste, or a paste and the Enter after it, pressed under [`PASTE_SETTLE`] ago.
    #[inline]
    const fn paste_holding(&self) -> bool {
        matches!(self.paste_hold, Some(until) if self.clock < until)
    }

    /// The writing wrapped the caret back to a line's start under [`WRITING_WRAP_FRESH`] ago,
    /// and no bound has gone for it yet: the cat it left at the margin expected it (no notice,
    /// [`Self::notices`]), and its bound is the brisk one ([`Self::brisk_shape`]).
    #[inline]
    const fn writing_wrapped(&self) -> bool {
        match self.writing_wrap {
            Some(at) => ((self.clock - at) as f32) < WRITING_WRAP_FRESH,
            None => false,
        }
    }

    /// How long (s) a caret a program moves must stay put to have rested: [`SETTLE`], or in a
    /// burst [`SETTLE_RHYTHM`] of the burst's own interval between its last two jumps, up to
    /// [`SETTLE_RHYTHM_MAX`] ([`OldPath::SettleIgnoresTheRhythm`]). A program writing a prompt in
    /// chunks [`SETTLE`] apart, the next one late by a frame, has not rested between them: read
    /// as rested, the stream watch it armed stood down and its burst ended at each late chunk
    /// (the long insert's dictation, 127 to 147 ms a chunk, and a harness typing into Claude
    /// Code's composer), and the cat went for each chunk's end as for a caret at rest.
    #[inline]
    const fn settle(&self) -> f32 {
        if self.jump_gap <= 0.0 || self.old_path(OldPath::SettleIgnoresTheRhythm) {
            return SETTLE;
        }
        (SETTLE_RHYTHM * self.jump_gap).clamp(SETTLE, SETTLE_RHYTHM_MAX)
    }

    /// The caret came by a jump no hand made and has stayed put since for [`SETTLE`].
    #[inline]
    const fn caret_rested_after_jump(&self) -> bool {
        match (self.jump_at, self.moved_at) {
            (Some(jumped), Some(moved)) => {
                jumped >= moved && ((self.clock - jumped) as f32) >= self.settle()
            }
            _ => false,
        }
    }

    /// A program's writing along the caret's line laid its letters under the cat on that line
    /// ([`OldPath::WritingEvictsAlongItsRow`]): the cat stands on the caret's row on ink, while
    /// the writing goes on (the stream watch is up: [`STREAM_RUN`] jumps of the caret in
    /// [`STREAM_WINDOW`], which no hand's keys make), or once a jump of it that wrote through
    /// the cat (under [`SETTLE_CAP`] ago) has a burst going on after it: a program's first
    /// chunks of a prompt it writes, not yet a stream, held the cat crouched on their letters
    /// until the third. Not at the jump alone: a program may take it straight back
    /// ([`LONE_SETTLE`]). Only letters the caret has passed, the cat's body starting behind it:
    /// a placeholder a program draws ahead of the caret (Claude Code's empty composer, as its
    /// first screens paint) is no writing going on, and sent across the rule for it the cat
    /// flew to the footer and back as a session began.
    #[inline]
    fn writing_laid_under(&self, (cr, cc): (u16, u16), width: f32) -> bool {
        (self.stream
            || (self.burst_from.is_some()
                && matches!(self.written_through, Some(at) if ((self.clock - at) as f32) < SETTLE_CAP)))
            && !self.old_path(OldPath::WritingEvictsAlongItsRow)
            && self.flight.is_none()
            && (self.row - f32::from(cr)).abs() < 0.5
            && self.col < f32::from(cc)
            && self.ink_overlaps(self.col, self.row, width)
    }

    /// THE WRITING'S SEAT IS ACROSS ITS RULE ([`OldPath::WritingEvictsAlongItsRow`]): the nearest
    /// seat to the stand beside the caret ([`Self::head_seat_along`], either way along its row) on
    /// a row past a rule under the caret's line, within [`HEAD_SEAT_ROWS`] of it (a composer's
    /// footer under its bottom rule): ground a program's writing along the caret's line never
    /// reaches, as a composer grows up from its bottom rule and not down past it. `None` where no
    /// rule lies under the line (a shell's prompt: its writing wraps on down) or no seat past it.
    #[inline]
    fn seat_off_the_writing(
        &self,
        caret: (u16, u16),
        (rows, cols): (u16, u16),
        width: f32,
    ) -> Option<(f32, f32)> {
        let caret_row = f32::from(caret.0);
        let want = self.station_now(caret.1, cols, width);
        let limit = (f32::from(cols) - width).max(0.0);
        let rule =
            (1..=HEAD_SEAT_ROWS).find(|&k| self.console_divides_at(caret_row + f32::from(k)))?;
        let mut best: Option<(f32, f32, f32)> = None;
        for below in rule + 1..=HEAD_SEAT_ROWS {
            let r = caret_row + f32::from(below);
            if r >= f32::from(rows) {
                break;
            }
            for leftward in [false, true] {
                let Some(x) = self.head_seat_along(
                    (want, r),
                    caret,
                    (rows, limit),
                    width,
                    (f32::INFINITY, leftward),
                ) else {
                    continue;
                };
                let d = span_eq(x - want, r - caret_row);
                if best.is_none_or(|(bd, ..)| d < bd) {
                    best = Some((d, x, r));
                }
            }
        }
        best.map(|(_, x, r)| (x, r))
    }

    /// Where the cat watches a program's writing that goes on along the caret's line from, a
    /// rule under that line ([`OldPath::WritingEvictsAlongItsRow`]): where it is, when that is
    /// off the caret's row and off ink (on top of a harness's line, on the footer), the host
    /// carrying it with the rows it stands on as the composer grows; else across the rule
    /// ([`Self::seat_off_the_writing`]). `None` without the stream watch, in the air, or with no
    /// rule under the line.
    #[inline]
    fn watch_off_the_writing(
        &self,
        caret: (u16, u16),
        dims: (u16, u16),
        width: f32,
    ) -> Option<(f32, f32)> {
        if !self.stream || self.flight.is_some() || self.old_path(OldPath::WritingEvictsAlongItsRow)
        {
            return None;
        }
        let caret_row = f32::from(caret.0);
        if !(1..=HEAD_SEAT_ROWS).any(|k| self.console_divides_at(caret_row + f32::from(k))) {
            return None;
        }
        if (self.row - caret_row).abs() >= 0.5 && !self.ink_overlaps(self.col, self.row, width) {
            Some((self.col, self.row))
        } else {
            self.seat_off_the_writing(caret, dims, width)
        }
    }

    /// The last caret jump the hand did not make came under `secs` ago.
    #[inline]
    const fn jumped_within(&self, secs: f32) -> bool {
        match self.jump_at {
            Some(at) => ((self.clock - at) as f32) < secs,
            None => false,
        }
    }

    /// The burst the last jump belongs to began under `secs` ago (`unburst` when there
    /// is none).
    #[inline]
    const fn burst_within(&self, secs: f32, unburst: bool) -> bool {
        match self.burst_from {
            Some(from) => ((self.clock - from) as f32) < secs,
            None => unburst,
        }
    }

    /// The burst a program made is over ([`SETTLE`]): its caret has stayed put for
    /// [`SETTLE`], it went on for less than [`SETTLE_CAP`], and the stream watch's last
    /// line event was one of its jumps.
    #[inline]
    const fn repaint_settled(&self) -> bool {
        match (self.burst_from, self.jump_at) {
            (Some(from), Some(at)) => {
                ((self.clock - at) as f32) >= self.settle()
                    && !self.paste_holding()
                    && ((at - from) as f32) < SETTLE_CAP
                    && self.stream_last >= from
            }
            _ => false,
        }
    }

    /// The stream watch stands down ([`STREAM_LAPSE`], or a repaint that settled,
    /// [`Self::repaint_settled`]) and latches a quiet leap: the trip home is a forced
    /// crossing, not the show. The burst that armed it is spent.
    #[inline]
    const fn stand_stream_down(&mut self) {
        self.stream = false;
        self.stream_run = 0;
        self.quiet_leap = true;
        self.burst_from = None;
        self.jump_gap = 0.0;
    }

    /// A program moved the caret by a jump under [`SETTLE`] ago, and not as part of a
    /// burst past [`SETTLE_CAP`] (a stream): where it stands may not be where it stays.
    #[inline]
    const fn caret_unsettled(&self) -> bool {
        (self.jumped_within(self.settle()) || (self.paste_holding() && self.burst_from.is_some()))
            && self.burst_within(SETTLE_CAP, true)
    }

    /// …and a coil gathered for it still waits it out ([`SETTLE_CAP`]).
    #[inline]
    const fn caret_settling(&self) -> bool {
        self.caret_moving() && self.burst_within(SETTLE_CAP, false)
    }

    /// Whether a gathered coil must not launch this tick ([`SETTLE`]). While a program's
    /// burst still moves the caret the coil holds, its clock kept short of the release at
    /// `release`, so the gather stays on glass and not the launch frame, and the arm keeps
    /// re-reading its target. The host's chrome come over the held body (`obstructed`) is
    /// never waited out: the console would step the body aside in one present, so the coil
    /// leaves this tick. A burst that outlasted [`SETTLE_CAP`] under the stream watch is a
    /// stream: the coil stands down to watch it, unless it is the live-edge hug's own trip
    /// (`hugging`). A coil gathered for a `bound`, or for any flight after the hand's Enter,
    /// waits out a program's lone row jump too ([`Self::row_jump_settling`]).
    #[inline]
    const fn coil_waits(
        &mut self,
        release: f32,
        hugging: bool,
        obstructed: bool,
        bound: bool,
    ) -> bool {
        // After the hand's Enter the new row is the program's, and a hop for it waits too.
        let enter = self.last_submit_seq().is_some();
        if self.caret_settling()
            || self.lone_settling()
            || (bound || enter) && self.row_jump_settling()
        {
            if obstructed {
                self.action_t = self.action_t.max(release);
                return false;
            }
            self.coil_waited = true;
            self.action_t = self.action_t.min((release - LAUNCH_LEAD - 1e-3).max(0.0));
            self.speed = 0.0;
            return true;
        }
        if self.caret_moving() && self.stream && !hugging {
            self.hop_crouch = false;
            self.big_gather = false;
            self.set_action(PetAction::Perk);
            return true;
        }
        false
    }

    /// The cat is crouched for the caret: no hide, stakeout, vigil or play behind the coil.
    #[inline]
    const fn caret_coiled(&self) -> bool {
        matches!(self.action, PetAction::Crouch)
            && !self.hiding
            && !self.stakeout
            && !self.vigil
            && !self.pending_vigil_pounce
            && self.play_to.is_none()
    }

    /// A coil that waited for a burst ([`SETTLE`]) whose caret settled where the cat
    /// already stands (or the stream watch holds the stand) leaves nowhere: it stands up
    /// instead of hopping on the spot. So does a row hop's gather whose row came to the cat
    /// (a composer the session's first turn pushed down carried the gathering cat with it,
    /// [`Self::carry_console_body`]): launched, it hopped on the spot.
    #[inline]
    const fn coil_came_home(&mut self, target: f32, target_row: f32) -> bool {
        let home = (self.coil_waited || self.hop_crouch)
            && (target_row - self.row).abs() < 0.5
            && (target - self.col).abs() <= ARRIVED;
        if home {
            self.hop_crouch = false;
            self.set_action_keep(PetAction::Stand);
        }
        home
    }

    /// A gather's target lies on the cat's own row, past [`ARRIVED`] (a gather on the spot is a
    /// trick's or a perch's) and within [`BOUND_SPAN_MIN`]: it came back near while a big jump
    /// was pending or the gather held (the head law's seat at the far margin for a moment, as
    /// the arrow keys began to walk the caret back into the line). Launched, the gather bounded
    /// a cell along the row.
    #[inline]
    const fn bound_came_short(&self, target: f32, target_row: f32) -> bool {
        let gap = (target - self.col).abs();
        (target_row - self.row).abs() < 0.5 && gap > ARRIVED && gap < BOUND_SPAN_MIN
    }

    /// A gather (a pounce's, a row hop's, a big jump's) whose target came back within a short
    /// walk along its own row ([`Self::bound_came_short`]), no ink at the target and the console
    /// barring no stand there, stands up: the walk takes it the rest of the way. Launched at the
    /// target it had re-read, it hopped a cell or two on the spot.
    #[inline]
    fn gather_stands_short(
        &mut self,
        target: f32,
        target_row: f32,
        sense: PetSense,
        width: f32,
    ) -> bool {
        let short = self.bound_came_short(target, target_row)
            && !self.ink_overlaps(target, target_row, width)
            && !self.console_bars((target, target_row), sense.rows, width);
        if short {
            self.hop_crouch = false;
            self.big_gather = false;
            self.pending_big_jump = false;
            self.set_action_keep(PetAction::Stand);
        }
        short
    }

    #[inline]
    fn wrap_ahead_aim(
        &mut self,
        (cr, cc): (u16, u16),
        (rows, cols): (u16, u16),
        width: f32,
    ) -> Option<(f32, f32)> {
        // Down on the next line already, beside the writing (its own row was written past
        // the caret, the stand went a row down), with the wrap due within [`WRAP_WAIT`] at
        // the run's pace: the stand stays on the next line, at the station's column, and
        // the wrap brings the caret to it. Its own row cleared as a mid-line insert pushed
        // the last word down, the cat hopped up onto it a second before the wrap and
        // bounded back down at the wrap: two flights at the wrap.
        if self.wrap_ahead.is_none()
            && self.flight.is_none()
            && self.typing_on()
            && cr + 1 < rows
            && (self.row - f32::from(cr + 1)).abs() < 0.5
            && f32::from(cols.saturating_sub(cc)) <= WRAP_WAIT * self.run_pace
        {
            // At the station's column, or where the cat already stands when it ran on
            // ahead of the writing: walked back to the station, it turned back under the
            // writing as the caret came to the margin.
            let limit = (f32::from(cols) - width).max(0.0);
            let next = (
                self.station_now(cc, cols, width).max(self.col.min(limit)),
                f32::from(cr + 1),
            );
            if !self.ink_overlaps(next.0, next.1, width)
                && self.console_station(next, (rows, cols), width).is_none()
            {
                // Latched as a bound to the next line is ([`WrapAhead::Launched`]): the
                // margin's hysteresis keeps it while the hand's pace wavers.
                self.wrap_ahead = Some(WrapAhead::Launched(next.0, next.1));
                return Some(next);
            }
        }
        // The margin is taken while the hand writes into it ([`Self::writing_on`]: the
        // rhythm open, or its last key a letter under [`SIT_AFTER`] ago), and kept once taken
        // while the caret stays at it. Taken only with the rhythm open, the first keys typed
        // on after a pause, short of the rhythm's [`RHYTHM_MOVES`], found it untaken: the wall
        // side crossed the stand to the caret's left, onto the line's words, and the rhythm's
        // opening brought it back to the wall (a hop up a row, a hop back, then the wrap's
        // crossing: three flights at one line end).
        let slack = match self.wrap_ahead {
            Some(WrapAhead::OffTheLine { kept, .. }) => kept,
            Some(_) => WALL_RETURN_MARGIN,
            None => self.margin_slide_lead((cr, cc), (rows, cols), width),
        };
        if !self.at_the_margin((cr, cc), cols, width, slack)
            || (self.wrap_ahead.is_none() && !self.writing_on())
        {
            // The hand went back from the margin: back up onto the line on the paws, as it
            // came off it, where the line's letters have gone from over the body.
            if matches!(self.wrap_ahead, Some(WrapAhead::OffTheLine { .. }))
                && matches!(
                    self.observed_ink_in_span(f32::from(cr), self.col, self.col + width),
                    Some(false)
                )
            {
                self.slide_to_row(f32::from(cr));
            }
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
                return Some((col, row));
            }
            self.wrap_ahead = None;
            return None;
        }
        let limit = (f32::from(cols) - width).max(0.0);
        if let Some(WrapAhead::Rest(col, row)) = self.wrap_ahead
            && (row - f32::from(cr)).abs() >= 0.5
            && (self.margin_seat_at((col, row), (cr, cc), rows, width)
                || self.seat_off_the_line_at((col, row), (cr, cc), (rows, cols), width))
        {
            return Some((col, row));
        }
        let wall = (limit, f32::from(cr));
        if self.console_station(wall, (rows, cols), width).is_some() {
            self.wrap_ahead = None;
            return None;
        }
        if let Some((row, over)) = self.margin_seat_off_the_line(cr, (rows, cols), limit, width) {
            self.wrap_ahead = Some(WrapAhead::OffTheLine {
                row,
                over,
                kept: slack.max(WALL_RETURN_MARGIN),
            });
            if (self.row - row).abs() >= 0.5 {
                self.slide_to_row(row);
            }
            return Some((limit, row));
        }
        if self.wall_meets_the_caret((cr, cc), (rows, cols), limit, width) && !self.on_the_beat() {
            let seat = match self.margin_rest_seat((cr, cc), (rows, limit), width) {
                Some(seat) => Some(seat),
                None => self.seat_off_the_line((cr, cc), (rows, cols), limit, width),
            };
            match seat {
                // The hand's rhythm closed at the wall: it stopped there.
                Some(seat) if !self.typing_on() => {
                    self.wrap_ahead = Some(WrapAhead::Rest(seat.0, seat.1));
                    return Some(seat);
                }
                // A pause in a run whose next key may wrap the caret off the wall, the wrap's
                // crossing taking the body with it: the wall holds until the rhythm closes, and
                // where its box meets the line's letters the wall's hide takes the body off the
                // glass meanwhile ([`Self::wall_seat`]).
                Some(_) => {}
                // A page with no seat off the line whose wall holds the body no seat either: the
                // box it draws at rest there meets the line's letters. The margin holds it at the
                // wall, where the wall's hide takes it off the glass ([`Self::wall_seat`]) until
                // the line ends: the station on the caret's line side lies on those letters too
                // ([`OldPath::NoSeatAnywhereHides`]).
                None if !self.old_path(OldPath::NoSeatAnywhereHides)
                    && !self.old_path(OldPath::NoSeatHidesTheBody)
                    && self.wall_unseated(
                        (cr, cc),
                        ((self.last_cell_w, self.last_cell_h), (cols, rows)),
                        width,
                    ) == Some(true) => {}
                // A page with no seat off the line: the margin is not taken, the wall side
                // turns ([`Self::update_wall`]), and the stand is the station on the caret's
                // line side, off its cell.
                None => {
                    self.wrap_ahead = None;
                    return None;
                }
            }
        }
        if self.hand_stopped()
            && let Some(seat) = self.margin_rest_seat((cr, cc), (rows, limit), width)
        {
            self.wrap_ahead = Some(WrapAhead::Rest(seat.0, seat.1));
            return Some(seat);
        }
        self.wrap_ahead = Some(WrapAhead::Approach);
        Some(wall)
    }

    /// THE MARGIN KEEPS THE BODY OFF THE LINE. Writing that runs into the margin would run its
    /// last letters under a cat at the wall on their row (the trail gallery's `fold-rest` and
    /// `focus-wrap`, every pet-family frame), and the wrap's crossing would then fly out through
    /// them. So as the margin is taken, the body leaves the line where it stands, on its paws
    /// ([`Self::slide_to_row`], no flight; a wrap still costs one, along its new row), and runs on
    /// to the wall there: to the blank row under the line, where the wrap brings the caret to it
    /// and the crossing runs along that row, with the head law holding the box under the line
    /// ([`Self::head_target`]); else to the blank row over it. A seat with room for the body whole
    /// comes first (a short pane's row under the line is its last, where the body would be
    /// fitted), so over a whole seat under, under a fitted one over. The writing's own row counts
    /// as lettered from the first, so the body is down before the letters come.
    ///
    /// A row is blank over the body's whole way, from where it stands to the wall, and not over
    /// the wall's columns alone: a body a caret's leap or a held key left far back ran along
    /// another line's letters on a row blank only by the wall. Short of them it keeps to the line
    /// and slides once it has passed them.
    ///
    /// The alternate screen keeps this seat ([`Self::note_full_screen`]). The row under the line
    /// is the wrap's own row, where the writing brings the caret next, and it is blank across the
    /// body's whole way. A full-screen program's rows under its line
    /// ([`OldPath::FullScreenKeepsItsRow`]) are a different matter: that law governs where a seat
    /// is sought by the caret, not where a wrap is met. Held to the line on the alternate screen
    /// (the trail gallery's `focus-wrap`, which runs there), the cat on dark lay on the writing
    /// in 41 frames.
    ///
    /// `None` where neither row is blank so, where a rule lies on it, or where the letter reader
    /// certifies nothing: the wall on the line holds until the caret comes into the body there
    /// ([`Self::wall_meets_the_caret`]), and [`Self::margin_rest_seat`] looks further once the
    /// hand has stopped; and for a body that is not on the line, such as one the head law
    /// seated on a composer's footer, which the wrap comes to where it is. Reverted by
    /// [`OldPath::MarginKeepsTheLine`].
    #[inline]
    fn margin_seat_off_the_line(
        &self,
        cr: u16,
        (rows, cols): (u16, u16),
        limit: f32,
        width: f32,
    ) -> Option<(f32, bool)> {
        #[cfg(any(test, debug_assertions))]
        if self.letters_unseen {
            return None;
        }
        let row = f32::from(cr);
        if self.old_path(OldPath::MarginKeepsTheLine)
            || ((self.row - row).abs() >= 0.5
                && !matches!(self.wrap_ahead, Some(WrapAhead::OffTheLine { .. })))
        {
            return None;
        }
        let (from, right) = (self.col.min(limit), f32::from(cols));
        let blank = |r: f32| matches!(self.observed_ink_in_span(r, from, right), Some(false));
        let seat = |r: f32| {
            blank(r)
                && !self.console_rule_at(r)
                && self.console_admits_rest((limit, r), rows, width)
        };
        let under = cr + 1 < rows && seat(row + 1.0);
        let over = cr >= 2 && seat(row - 1.0);
        // The row its head or feet reach past the seat, blank: the body is drawn whole.
        if under && cr + 2 < rows && blank(row + 2.0) {
            return Some((row + 1.0, false));
        }
        if over && blank(row - 2.0) {
            return Some((row - 1.0, true));
        }
        if under {
            return Some((row + 1.0, false));
        }
        if over {
            return Some((row - 1.0, true));
        }
        None
    }

    /// How much sooner (cells) the margin is taken for a body that will slide off the line
    /// ([`Self::margin_seat_off_the_line`]): the slide's whole way, [`ART_ROWS`] at [`SINK_RATE`],
    /// at the writing's pace, and the cell the letter lies in, past the cells the stand already
    /// leads the caret by. Taken at the stand's lead alone, a burst's letter reached the box's
    /// corner before the slide was done. A hold past [`WALL_RETURN_MARGIN`] keeps it at that
    /// slack ([`WrapAhead::OffTheLine`]): let go at the margin's own, the next frame found the caret
    /// short of it and the body slid back up, down again a frame on.
    #[inline]
    fn margin_slide_lead(&self, (cr, cc): (u16, u16), (rows, cols): (u16, u16), width: f32) -> f32 {
        let lead = (self.writing_pace() * (ART_ROWS * (1.0 / SINK_RATE)) + 1.0
            - (STATION_LEAD + self.standoff()))
        .max(0.0);
        if lead > 0.0
            && f32::from(cc) + STATION_LEAD + self.standoff() + width + lead > f32::from(cols)
            && self
                .margin_seat_off_the_line(
                    cr,
                    (rows, cols),
                    (f32::from(cols) - width).max(0.0),
                    width,
                )
                .is_some()
        {
            lead
        } else {
            0.0
        }
    }

    /// The body stands on `row` from this frame, drawn where it is drawn now: the head law's ease
    /// goes on from here ([`Self::head_eased`]), its sink counted from the new row, and takes it
    /// to the law's place on that row, at [`SINK_RATE`]; stepped up to a larger fit, it grows
    /// there only once its sink is down to the law's on that row. It reads as a step down or up
    /// on the paws, not a flight.
    #[inline]
    const fn slide_to_row(&mut self, row: f32) {
        let (sink, fit) = self.head_eased(false);
        self.sink_from = sink - (row - self.row);
        self.fit_from = fit;
        self.sink_since = self.clock;
        self.sink_seat = None;
        self.row = row;
    }

    /// THE HEAD LAW AT THE MARGIN. Where no row beside the line is free for the body
    /// ([`Self::margin_seat_off_the_line`]), writing that runs into the margin runs its last
    /// letters over the cat at the wall ([`Self::wrap_ahead_aim`]) until the caret comes into it
    /// ([`Self::wall_meets_the_caret`]): while the hand writes, the wrap is coming and the
    /// crossing it starts is the line end's one flight. Once the hand has
    /// stopped ([`Self::hand_stopped`], the cat sits), a wall seat whose feet or head lie
    /// among letters ([`Self::head_seat_at`]) is a resting cat on the words: the cat takes the
    /// nearest seat by the caret that keeps the head law ([`Self::head_seat_along`]), on a row
    /// over the caret's or under it, within [`HEAD_SEAT_REACH`] of the wall (rows counted as
    /// [`ROW_AS_COLS`]), never across a rule, and holds it until the wrap
    /// ([`WrapAhead::Rest`]): typing on after the pause costs no hop back to the wall. Rows
    /// over are the seats past a wrapped line's rows on the screen's last row: with one row
    /// over only, the cat at the margin of the line's second row sat with its head among the
    /// first row's letters, and with two, at the margin of a line wrapped three times it sat on
    /// the line's last letters for good (r24 K7-3): the line's first row over the caret's three
    /// is the nearest. `None` where the wall seat keeps the law, where no such seat is near, or
    /// where the letter reader certifies nothing: the wall holds, as the head law's stand does.
    #[inline]
    const fn margin_rest_seat(
        &self,
        (cr, cc): (u16, u16),
        (rows, limit): (u16, f32),
        width: f32,
    ) -> Option<(f32, f32)> {
        let row = cr as f32;
        if !width.is_finite()
            || width <= 0.0
            || self.head_seat_at((limit, row), (cr, cc), rows, width)
        {
            return None;
        }
        let mut best: Option<(f32, f32, f32)> = None;
        // Rows nearest first: one over, one under, then two under, two over, and so on, each
        // row's least distance its rows at [`ROW_AS_COLS`]. Never over a rule: a composer's top
        // rule is no row to leap across to the transcript over it, nor its bottom rule to the
        // footer under it; no rule lies between the caret's row and the seat's, nor across the
        // seat's feet or head.
        let reach_rows = (HEAD_SEAT_REACH / ROW_AS_COLS) as u16;
        let mut step = 0u16;
        while step < 2 * reach_rows {
            let k = (step / 2 + 1) as f32;
            let dr = if step.is_multiple_of(2) == (k < 1.5) {
                -k
            } else {
                k
            };
            step += 1;
            let min_span = k * ROW_AS_COLS;
            if matches!(best, Some((d, ..)) if min_span >= d) || min_span > HEAD_SEAT_REACH {
                break;
            }
            let r = row + dr;
            if r < 1.0 || r >= rows as f32 {
                continue;
            }
            let (lo, hi) = if dr < 0.0 {
                (r - 1.0, row - 1.0)
            } else {
                ((r - 1.0).min(row + 1.0), r)
            };
            let mut ruled = false;
            let mut i = lo;
            while i <= hi + 0.5 {
                ruled |= self.console_rule_at(i);
                i += 1.0;
            }
            if ruled {
                continue;
            }
            let Some(x) = self.head_seat_along(
                (limit, r),
                (cr, cc),
                (rows, limit),
                width,
                (HEAD_SEAT_REACH, true),
            ) else {
                continue;
            };
            if !self.head_seat_at((x, r), (cr, cc), rows, width) {
                continue;
            }
            let d = span_eq(x - limit, dr);
            if d <= HEAD_SEAT_REACH && !matches!(best, Some((bd, ..)) if d >= bd) {
                best = Some((d, x, r));
            }
        }
        match best {
            Some((_, x, r)) => Some((x, r)),
            None => None,
        }
    }

    /// A seat the cat may be sent to off the caret's row by the margin ([`Self::margin_rest_seat`]):
    /// one that keeps the head law ([`Self::head_seat_at`]) with no rule's strokes across its
    /// feet or its head ([`crate::pet_world::PetWorld::rule_line`]): seated on a composer's footer, its bottom rule
    /// would run through the cat's head.
    #[inline]
    const fn margin_seat_at(
        &self,
        (col, row): (f32, f32),
        caret: (u16, u16),
        rows: u16,
        width: f32,
    ) -> bool {
        !self.console_rule_at(row)
            && !self.console_rule_at(row - 1.0)
            && self.head_seat_at((col, row), caret, rows, width)
    }

    /// THE CARET'S CELL IS NO SEAT AT THE WALL. Writing that runs into the margin brings the
    /// caret up to the body at the wall on its row ([`WrapAhead::Approach`]), and where the box
    /// the body is drawn in there, as the head law draws it ([`Self::head_target`], a fitted one
    /// at its whole box's right edge, [`Self::fitted_to_the_wall`]), reaches back over the caret's
    /// cell past the box's first cell, the body lies in that cell: on a full page's last row at 71
    /// columns the fitted cat's fur lay in the bar caret's cell, which read 3.71 against the page
    /// where main's 5.10 (the trail gallery's `last-row`), and at 72 with the caret in the last
    /// column a block caret hid all but a sliver of it (`flood`, the "crumb"). The box's first
    /// cell over the caret's is its art's transparent border: at 72 columns, the caret two cells
    /// from the wall, the box began 0.81 of a cell into the caret's cell and the fitted cat's fur
    /// 17 of the box's 40 px in, clear of it. Where it lies so, and the hand's next key is not
    /// due on its beat ([`Self::on_the_beat`]), the margin sends the body to the nearest seat
    /// off the line ([`Self::margin_rest_seat`] in reach, else [`Self::seat_off_the_line`]
    /// anywhere on the screen) once the hand's rhythm closes ([`Self::typing_on`]: a pause the
    /// next key may still end with a wrap keeps the wall, the wrap's crossing its one flight),
    /// held until the wrap ([`WrapAhead::Rest`]); with none, the wall side turns while the hand
    /// still writes, once the caret is in the row's last cell ([`OldPath::TurnAtTheLastCell`];
    /// [`Self::update_wall`]: held to the margin, the station clamped to the wall,
    /// on the caret, and the fitted cat stood there as a sliver behind the block caret, the
    /// trail gallery's `flood` from its last key through `t470`), and the stand is the station
    /// on the caret's line side; but where the box the wall draws at rest meets the line's
    /// letters ([`Self::wall_unseated`]), that station lies on them too, and the margin holds the
    /// wall for its hide ([`OldPath::NoSeatAnywhereHides`]). Where the box the wall draws at rest
    /// meets the line's letters while the rhythm is open, the wall's hide ([`Self::wall_seat`],
    /// [`OldPath::NoSeatHidesTheBody`]) has the body off the glass before the rhythm closes, and
    /// holds it there to the line's end; this seat is then not taken (see [`Self::wall_seat`]).
    /// `false` where the page is not read there (the letter reader blinded too), or reverted by
    /// [`OldPath::CaretCellIsNoSeat`].
    #[inline]
    const fn wall_meets_the_caret(
        &self,
        (cr, cc): (u16, u16),
        (rows, cols): (u16, u16),
        limit: f32,
        width: f32,
    ) -> bool {
        if self.old_path(OldPath::CaretCellIsNoSeat) || !width.is_finite() || width <= 0.0 {
            return false;
        }
        #[cfg(any(test, debug_assertions))]
        if self.letters_unseen {
            return false;
        }
        let row = cr as f32;
        let Some(HeadRead { law: (_, fit), .. }) =
            self.head_target(row, (rows, cols), (limit, limit + width))
        else {
            return false;
        };
        lies_on_the_caret(self.drawn_cols(limit, fit, cols, width), cc)
    }

    /// Whether the hand's next key is due on its run's beat: the rhythm open ([`Self::typing_on`])
    /// and the last key under [`KEY_BEAT_LATE`] beats ago. A wall the caret comes into then
    /// ([`Self::wall_meets_the_caret`]) is met for a beat or two before the next key wraps the
    /// caret off it, the wrap's crossing taking the body down with it, so nothing of the wall
    /// is read until the beat lapses. Sent off the line at once, a held key typed through a full
    /// page's last row cost its wraps two flights more, up to a seat over the line and back down
    /// after the wrap (`aterm-effects`' `a_held_key_runs_and_each_wrap_typed_through_is_one_flight`,
    /// five flights for three wraps); and a hand that paused between words at the wall cost the
    /// same, so a seat off the line is taken only once the rhythm closes.
    #[inline]
    const fn on_the_beat(&self) -> bool {
        self.typing_on() && self.key_beat > 0.0 && self.run_t <= self.key_beat * KEY_BEAT_LATE
    }

    /// Whether a body a program's writing holds where it stands ([`Self::stream`], the watch's
    /// hold) lies on the caret's cell as it is drawn now ([`lies_on_the_caret`]): the writing
    /// came up to it. The hold lets it go to its station, as the margin's wall does
    /// ([`Self::wall_meets_the_caret`]): held, a page written to its last cell left the cat at the
    /// wall with the caret in its box, a block caret over all of it but a sliver between the
    /// last letter and the caret (the trail gallery's `flood`, keys 1008 at 72 columns and 981 at
    /// 70, the "crumb"), until the watch lapsed 0.8 s on. Reverted by
    /// [`OldPath::CaretCellIsNoSeat`].
    #[inline]
    const fn held_on_the_caret(&self, (cr, cc): (u16, u16), cols: u16, width: f32) -> bool {
        !self.old_path(OldPath::CaretCellIsNoSeat)
            && (self.row - cr as f32).abs() < 0.5
            && width.is_finite()
            && width > 0.0
            && lies_on_the_caret(self.drawn_cols(self.col, self.fit, cols, width), cc)
    }

    /// The columns a body standing at `col` is drawn across at `fit` ([`head_law`]): its box
    /// scaled about its middle, a fitted one moved toward its whole box's right edge by the
    /// margin ([`Self::fitted_to_the_wall`]).
    #[inline]
    const fn drawn_cols(&self, col: f32, fit: f32, cols: u16, width: f32) -> (f32, f32) {
        let fit = fit.clamp(0.0, 1.0);
        let left =
            col + self.fitted_to_the_wall_at(col, fit, cols, width) + width * (1.0 - fit) * 0.5;
        (left, left + width * fit)
    }

    /// The nearest seat off the caret's row anywhere on the screen for a body sent off the wall
    /// ([`Self::wall_meets_the_caret`]): rows nearest first, each from the wall inward, a row's
    /// distance its rows at [`ROW_AS_COLS`] ([`span_eq`]), never across a rule. A seat is one
    /// the head law draws clear of every row's letters ([`Self::seat_off_the_line_at`]): whole, or
    /// drawn down its row, or fitted to it. `None` on a page with no such seat (the trail
    /// gallery's `flood`, a letter in every cell but the spaces between words).
    #[inline]
    const fn seat_off_the_line(
        &self,
        (cr, cc): (u16, u16),
        (rows, cols): (u16, u16),
        limit: f32,
        width: f32,
    ) -> Option<(f32, f32)> {
        if !width.is_finite() || width <= 0.0 || !limit.is_finite() {
            return None;
        }
        let row = cr as f32;
        let mut best: Option<(f32, f32, f32)> = None;
        let mut k = 1u16;
        while k < rows {
            let min_span = k as f32 * ROW_AS_COLS;
            if matches!(best, Some((d, ..)) if min_span >= d) {
                break;
            }
            let mut side = 0u8;
            while side < 2 {
                let dr = if side == 0 { -(k as f32) } else { k as f32 };
                side += 1;
                let r = row + dr;
                if r < 0.0 || r >= rows as f32 {
                    continue;
                }
                // No rule between the caret's row and the seat's, nor on the seat's own.
                let (lo, hi) = if dr < 0.0 {
                    (r, row - 1.0)
                } else {
                    (row + 1.0, r)
                };
                let mut ruled = false;
                let mut i = lo;
                while i <= hi + 0.5 {
                    ruled |= self.console_rule_at(i);
                    i += 1.0;
                }
                if ruled {
                    continue;
                }
                let mut x = limit;
                while x >= 0.0 {
                    let d = span_eq(limit - x, dr);
                    if matches!(best, Some((bd, ..)) if d >= bd) {
                        break;
                    }
                    if self.seat_off_the_line_at((x, r), (cr, cc), (rows, cols), width) {
                        best = Some((d, x, r));
                        break;
                    }
                    x -= 1.0;
                }
            }
            k += 1;
        }
        match best {
            Some((_, x, r)) => Some((x, r)),
            None => None,
        }
    }

    /// Whether a body at `(col, row)`, off the caret's row, is seated clear of every row's letters
    /// as the head law draws it there ([`Self::head_target`]): no ink under its feet (as the letter
    /// reader saw them, and none the ink spans hold), no rule across its feet or its head, a rest
    /// the console admits, and its footprint off the caret's cell on a row beside the caret's;
    /// drawn whole, its head off the row above's letters too ([`Self::head_seat_at`]). Drawn down
    /// its row or fitted to it, the law keeps its box off the rows beside its own ([`head_law`]).
    #[inline]
    const fn seat_off_the_line_at(
        &self,
        (col, row): (f32, f32),
        (cr, cc): (u16, u16),
        (rows, cols): (u16, u16),
        width: f32,
    ) -> bool {
        let caret_row = cr as f32;
        let caret_col = cc as f32;
        if (row - caret_row).abs() < 0.5
            || row < 0.0
            || row >= rows as f32
            || self.console_rule_at(row)
            || self.console_rule_at(row - 1.0)
        {
            return false;
        }
        if (row - caret_row).abs() < 1.5
            && col + INK_PAD < caret_col + 1.0
            && col + width - INK_PAD > caret_col
        {
            return false;
        }
        if !matches!(self.observed_ink_overlaps(col, row, width), Some(false))
            || self.ink_overlaps(col, row, width)
            || !self.console_admits_rest((col, row), rows, width)
        {
            return false;
        }
        match self.head_target(row, (rows, cols), (col, col + width)) {
            Some(HeadRead {
                law: (sink, fit), ..
            }) => (sink > 0.0 || fit < 1.0) || self.head_seat_at((col, row), (cr, cc), rows, width),
            None => false,
        }
    }

    /// Whether a body standing at `(col, row)` keeps the head law there
    /// ([`Self::head_seat_along`]): no ink under its feet (as the letter reader saw them, or
    /// the ink spans where it saw none of them), no letter over its head, a rest the console
    /// admits, and, standing under the caret's row, its head off the caret's cell. On the
    /// caret's own row the letters decide: the wall seat the caret comes up to as it enters the
    /// margin covers no letter yet, and holds. On the screen's top row the head has no row to
    /// meet. False where the letter reader certifies nothing over the head.
    #[inline]
    const fn head_seat_at(
        &self,
        (col, row): (f32, f32),
        (cr, cc): (u16, u16),
        rows: u16,
        width: f32,
    ) -> bool {
        let caret_col = cc as f32;
        if (row - 1.0 - cr as f32).abs() < 0.5
            && col + INK_PAD < caret_col + 1.0
            && col + width - INK_PAD > caret_col
        {
            return false;
        }
        let feet_clear = match self.observed_ink_overlaps(col, row, width) {
            Some(inked) => !inked,
            None => !self.ink_overlaps(col, row, width),
        };
        let head_clear = row < 0.5
            || matches!(
                self.observed_last_letter(
                    row - 1.0,
                    head_seat_from(col, width, !self.old_path(OldPath::SeatClearsTheStretch)),
                    col + width - INK_PAD
                ),
                Some(None)
            );
        feet_clear && head_clear && self.console_admits_rest((col, row), rows, width)
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
    const fn station_now(&self, caret_col: u16, cols: u16, width: f32) -> f32 {
        let limit = cols as f32 - width;
        if limit <= 0.0 {
            return 0.0;
        }
        if self.wall_side {
            return (caret_col as f32 - width - STATION_LEAD - self.standoff()).max(0.0);
        }
        (caret_col as f32 + STATION_LEAD + self.wide_step() + self.lead() + self.standoff())
            .clamp(0.0, limit)
    }

    /// THE CAT KEEPS A WIDE GLYPH'S STEP AHEAD: writing whose keys each write a wide glyph moves
    /// the caret two cells a key, a cell further than the station's lead keeps clear, so the
    /// station stands further ahead ([`WIDE_STEP_LEAD`]) while a forward run of keys
    /// ([`RHYTHM_MOVES`] or more) steps wide ([`Self::wide_run`]). Kept at the caret's
    /// [`STATION_LEAD`], the cat running after `echo 你好世界` fell behind each two-cell step and
    /// its tail lay on the caret's cell at the last key (the trail gallery's `cjk` `t0`, looks9
    /// round 1). Reverted by [`OldPath::NarrowStation`].
    #[inline]
    const fn wide_step(&self) -> f32 {
        if self.wide_run() && !self.old_path(OldPath::NarrowStation) {
            WIDE_STEP_LEAD
        } else {
            0.0
        }
    }

    /// A forward run of keys ([`RHYTHM_MOVES`] or more) whose latest [`WIDE_RUN_STEPS`] steps
    /// each moved the caret more than a cell, none more than [`TYPING_STEP_MAX`]: writing in
    /// wide glyphs. Read off the run's largest step, one two-cell step (a lone wide glyph, or two
    /// keys echoed in one present) held the wide station and the fast chase for the rest of the
    /// run, every later key a cell (looks9's review: the cat stood five cells off the caret in
    /// `echo ab你cdefghijklmnopqrstu` where the same run in ASCII kept three). Reverted by
    /// [`OldPath::WideByLargestStep`].
    #[inline]
    const fn wide_run(&self) -> bool {
        let wide = if self.old_path(OldPath::WideByLargestStep) {
            self.run_step > 1.0
        } else {
            self.wide_steps >= WIDE_RUN_STEPS
        };
        self.run_dir > 0
            && wide
            && self.run_step <= TYPING_STEP_MAX
            && self.run_count >= RHYTHM_MOVES
    }

    /// THE CAT RUNS DOWN WIDE WRITING AT ITS FASTEST: a station that writing in wide glyphs
    /// carries off two cells a key ([`Self::wide_run`]) is run down at the chase's fast gain and
    /// the top speed ([`TOP_SPEED_MAX`]), and a landing's recovery on the caret's row gives way
    /// to the run once the caret's next step would reach the cat. Bounding in to a line typed
    /// in CJK, the cat came down, stood out its landing and ran up to speed while the caret
    /// stepped two cells a key into it: the trail gallery's `cjk` laid the caret's cell under
    /// its tail at `t0` (looks9 round 2). Reverted by [`OldPath::WideChase`], and by it alone:
    /// gated on the wide station's lead ([`Self::wide_step`]), the station's control reverted
    /// this law with its own.
    #[inline]
    const fn wide_chase(&self, gap: f32) -> bool {
        gap > 0.0 && self.wide_run() && !self.old_path(OldPath::WideChase)
    }

    /// THE WRITING NEVER RUNS UNDER A STILL CAT: a cat held still on the caret's row ahead of
    /// writing (a landing's recovery, a commit's stare: [`Self::held_still`]) gives way to the
    /// run, at the chase's fast gain and top speed and on the station's pace, to a cell clear of
    /// the writing's next steps ([`Self::writing_clear`]), once two of them would reach its
    /// body, whatever the step (one cell a key or a wide glyph's two): on the key that steps the
    /// caret toward it, or, the writing's rhythm open ([`Self::typing_on`]), as soon as a step
    /// and a half would, so the key that would land on it finds it already moving. Held, a dog
    /// that came down just ahead of a line typed in CJK stood out its pose while the caret
    /// stepped two cells a key under it: the trail gallery's `cjk` laid the caret's cell and
    /// the right of `界` under its box at `t0` (looks10 round 3). Reverted by
    /// [`OldPath::StillUnderWriting`].
    #[inline]
    fn writing_reaches(&self, caret: (u16, u16), width: f32) -> bool {
        self.held_still() && self.writing_near(caret, width, 2.0)
    }

    /// Whether the writing's next `steps` steps on the cat's row would reach its body: a key
    /// typed of late, the caret coming the writing's way and not yet past the body. A still
    /// cat is reached two steps out, so its run starts a key before the step that would land
    /// on it, and it runs on until the two cannot.
    #[inline]
    fn writing_near(&self, (cr, cc): (u16, u16), width: f32, steps: f32) -> bool {
        if self.wall_side
            || self.stream
            || self.run_dir <= 0
            || (self.row - f32::from(cr)).abs() >= 0.5
            || self.old_path(OldPath::StillUnderWriting)
        {
            return false;
        }
        let writing = match self.key_within_dur(SIT_AFTER_DUR) {
            Some(kind) => kind == PetInputKind::Text,
            None => self.last_dir > 0 && self.run_t < SIT_AFTER,
        };
        let caret = f32::from(cc);
        writing
            && caret + 1.0 + steps * self.run_step.max(1.0) > self.col
            && caret < self.col + width
    }

    /// A landing's pose not yet on glass a still pose's [`STILL_SECS`]: the landing holds it,
    /// whatever would end the recovery ([`Self::wide_chase`], [`Self::writing_reaches`]). Let go
    /// on a present 16 ms after touchdown, the recovery's arm handed the body to the walk, whose
    /// fresh-pose hold stopped it as a stand: the stand's stride ([`STAND_MIN`]) then stood it
    /// on the caret's next wide step (the trail gallery's dog, `cjk` `t0`, looks10 round 4).
    #[inline]
    const fn landing_fresh(&self) -> bool {
        !self.old_path(OldPath::StillUnderWriting) && self.still_fresh()
    }

    /// A still pose that holds the body whatever the station does: a landing's recovery or a
    /// commit's stare, once its pose has been on glass for a still pose's [`STILL_SECS`]
    /// ([`Self::still_fresh`]). Given way sooner, the landing flashed a single present, or the
    /// fresh pose's hold turned it to a stand the run then had to wait out ([`STAND_MIN`]).
    #[inline]
    const fn held_still(&self) -> bool {
        !self.still_fresh()
            && match self.action {
                PetAction::Land => self.land_t > 0.0 || self.commit_t > 0.0,
                PetAction::Stand => self.commit_t > 0.0,
                _ => false,
            }
    }

    /// Where a cat the writing reaches ([`Self::writing_reaches`]) runs to at the least: its
    /// body's left edge [`WRITING_AIR`] past the cell the caret's second step lands on, so the
    /// next key finds it a step clear and moving.
    #[inline]
    const fn writing_clear(&self, cc: u16, cols: u16, width: f32) -> f32 {
        (cc as f32 + 1.0 + 2.0 * self.run_step.max(1.0) + WRITING_AIR)
            .min((cols as f32 - width).max(0.0))
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
    #[inline]
    pub fn sense_ink(&mut self, base_row: u16, spans: &[(u16, u16)], live_row: Option<u16>) {
        #[inline]
        const fn has_nonempty_span(spans: &[(u16, u16)]) -> bool {
            let mut i = 0;
            while i < spans.len() {
                let (f, e) = spans[i];
                if e > f {
                    return true;
                }
                i += 1;
            }
            false
        }
        if self.ink_base == base_row && self.ink_spans.as_slice() == spans {
            self.ink_scrolled = false;
            self.ink_rose = 0;
            self.ink_live = live_row;
            return;
        }
        self.ink_scrolled = self.ink_base == base_row
            && !self.ink_spans.is_empty()
            && self.ink_spans.len() == spans.len()
            && self.ink_spans[1..] == spans[..spans.len() - 1]
            && has_nonempty_span(self.ink_spans.as_slice());
        self.ink_rose = if self.ink_base == base_row {
            page_rise(self.ink_spans.as_slice(), spans)
        } else {
            0
        };
        self.ink_base = base_row;
        self.ink_spans.clear();
        self.ink_spans.extend_from_slice(spans);
        self.ink_live = live_row;
    }

    /// Retire every fact that depends on the current terminal surface.
    #[inline]
    pub fn retire_coordinate_space(&mut self) {
        let species = self.species;
        let content = self.content;
        let quiet = self.quiet;
        let clock = self.clock;
        let worn = match self.pending_worn {
            Some(w) => Some(w),
            None => self.worn,
        };
        let mote_serial = self.mote_serial;
        let departure_serial = self.departure_serial;
        let mote_mark = self.mote_mark;
        let last_frolic = self.last_frolic;
        let bored_cool = self.bored_cool;
        let twitch_cool = self.twitch_cool;
        let trick_content_at = self.trick_content_at;
        let mut stroke = std::mem::take(&mut self.stroke);
        if let Some(now) = self.last_now {
            stroke.sample(now, None, 0.0);
        }
        let mut ink_spans = std::mem::take(&mut self.ink_spans);
        ink_spans.clear();
        let console = self.console.retired();
        // A test's negative controls outlive the brain they were set on, as the laws they
        // revert do.
        #[cfg(any(test, debug_assertions))]
        let old_paths = self.old_paths;
        // A cat that waits on its caret's row ahead of its stand for the
        // writing ([`AWAIT_AHEAD`], the first keys' landing ahead) holds the seat it
        // stands on: held at its stand, it came back to the focus that left
        // it five cells behind the body the user last saw, too far for the
        // arrival to take up its look ([`ADOPT_REACH`]).
        let held = match self.held_for {
            Some((cr, cc))
                if self.flight.is_none()
                    && !self.wall_side
                    && (self.row - cr as f32).abs() < 0.5 =>
            {
                let ahead = self.col - (cc as f32 + STATION_LEAD + self.standoff());
                if ahead > self.held_lead && ahead <= AWAIT_AHEAD {
                    ahead
                } else {
                    self.held_lead
                }
            }
            _ => self.held_lead,
        };
        let left = match self.held_for {
            Some(caret) if held > 0.0 => Some(HeldStand {
                surface: self.surface,
                caret,
                lead: held,
            }),
            _ => None,
        };
        let [s0, s1] = self.held_stash;
        let k0 = match s0 {
            Some(s) if s.surface != self.surface => Some(s),
            _ => None,
        };
        let k1 = match s1 {
            Some(s) if s.surface != self.surface => Some(s),
            _ => None,
        };
        let held_stash = match left {
            Some(stand) => [
                Some(stand),
                match k0 {
                    Some(k) => Some(k),
                    None => k1,
                },
            ],
            None => match (k0, k1) {
                (Some(_), _) => [k0, k1],
                (None, _) => [k1, None],
            },
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
            twitch_cool,
            trick_content_at,
            stroke,
            ink_spans,
            console,
            held_stash,
            #[cfg(any(test, debug_assertions))]
            old_paths,
            ..Self::default()
        };
    }

    /// Whether the caret's moves are met at their own times ([`OldPath::LateCaretAtItsTimes`]),
    /// a hidden one's rest included ([`crate::companion::CompanionOwner::prepare`]); `false`
    /// only where a test reverts the law.
    #[inline]
    #[must_use]
    pub const fn times_the_caret(&self) -> bool {
        !self.old_path(OldPath::LateCaretAtItsTimes)
    }

    /// Whether a body on glass keeps its place across its terminal's switch to its other screen
    /// ([`OldPath::OtherScreenKeepsTheBody`]); `false` only where a test reverts the law.
    #[inline]
    #[must_use]
    pub const fn keeps_its_body(&self) -> bool {
        !self.old_path(OldPath::OtherScreenKeepsTheBody)
    }

    /// The same terminal switched between its main and its alternate screen
    /// ([`OldPath::OtherScreenKeepsTheBody`]): the glass and the grid are the ones the body is
    /// drawn on, so it keeps its place, pose and presence, and the caret's move to the new
    /// screen is a jump it goes to (a sleeper wakes first). Only the facts read off the old
    /// screen go: its letters' ledger and the pointer over them. The console's seats are the
    /// old screen's too, and its next reading lets them go ([`Self::begin_console_tick`]). So
    /// does the page the head law judged the body on ([`Self::ease_head`]): the new screen is
    /// judged afresh and eased to, as a body comes to letters, not met as letters printed over
    /// a body at rest, which the law takes down or fits at once
    /// ([`OldPath::NewScreenEasesTheHead`]).
    #[inline]
    pub fn rescreen(&mut self) {
        self.ink_base = 0;
        self.ink_spans.clear();
        self.ink_live = None;
        self.ink_scrolled = false;
        self.seat.note_new_page();
        self.last_pointer = None;
        self.pending_pointer_pounce = None;
        if !self.old_path(OldPath::NewScreenEasesTheHead) {
            self.sink_seat = None;
        }
        if !self.old_path(OldPath::NewScreenSettlesFirst) {
            self.rescreen_at = Some(self.clock);
        }
    }

    /// The terminal switched screens under the body ([`Self::rescreen`]) and the caret has not
    /// rested [`SETTLE`] since, nor has [`SETTLE_CAP`] passed: the program is still drawing its
    /// new screen, and the caret it shows is no place to set off for yet.
    #[inline]
    const fn rescreen_settling(&self) -> bool {
        let Some(at) = self.rescreen_at else {
            return false;
        };
        let last = match self.moved_at {
            Some(moved) if moved > at => moved,
            _ => at,
        };
        ((self.clock - at) as f32) < SETTLE_CAP && ((self.clock - last) as f32) < SETTLE
    }

    /// The same surface, re-gridded.
    #[inline]
    pub fn rebase_geometry(&mut self) {
        if self.last_caret.is_some() {
            self.regrid_caret = self.last_caret;
        }
        self.last_caret = None;
        self.ink_base = 0;
        self.ink_spans.clear();
        self.ink_live = None;
        self.ink_scrolled = false;
        self.ink_rose = 0;
        self.seat.note_new_page();
        self.last_pointer = None;
        self.pending_pointer_pounce = None;
    }

    /// THE SEAT RIDES THE PAGE. A page that scrolls moves its rows up together
    /// ([`Self::sense_ink`]), and a body on its feet among them stood on blank ground: where
    /// the rows that came up under it hold letters and the ground it sat on, the same rows up, is
    /// still clear of them, it rides up with the page in the frame the page moves, as a host's
    /// regrid carries it ([`Self::carry_with_regrid`]), and sits on what it sat on. Seated off a
    /// full page's last row (the trail gallery's `last-row` at 70 and 71 columns, the seat over
    /// the row of letters over the line, which the body takes there with the wall's hide
    /// reverted, [`OldPath::NoSeatHidesTheBody`]), the line's Enter and its answer scrolled the
    /// page two rows, and the body stayed on its row of the glass, now the line it had left: on
    /// its letters 400 ms, then walking along them 370 ms more. With every law it takes that
    /// seat where the line's last keys are spaces ([`Self::wall_meets_the_caret`]); an Enter
    /// answered by the prompt alone scrolled the page one row, and the body lay on the full
    /// row's letters the scroll brought under it in all 22 frames from 40 ms past the Enter to
    /// its setting off. Inert in flight, beside a console's own rows (it carries a body itself),
    /// where the scroll left the body's ground clear, or where the ride would take it off the
    /// glass. Reverted by [`OldPath::SeatRidesThePage`].
    #[inline]
    fn ride_the_page(&mut self, rows: u16, width: f32) {
        let rose = core::mem::take(&mut self.ink_rose);
        if rose == 0
            || self.old_path(OldPath::SeatRidesThePage)
            || self.flight.is_some()
            || self.console.resident
            || !width.is_finite()
            || width <= 0.0
        {
            return;
        }
        let dy = -f32::from(rose);
        if self.ink_overlaps(self.col, self.row, width)
            && !self.ink_overlaps(self.col, self.row + dy, width)
            && self.carry_with_regrid(dy, rows)
            && let Some((at, ..)) = &mut self.sink_seat
        {
            // Still on its seat to the head law, whose read of the rows round it the page
            // changed: judged as a body moved to another row, the fitted cat was drawn whole
            // for the frames the letter reader took to see the rows that came up, its head
            // in the row of letters over its seat.
            at.1 += dy;
        }
    }

    /// A BODY ON ITS STATION STAYS ON IT WHEN ONLY THE ART'S WIDTH MOVED IT. The art's width in
    /// columns follows the cell's aspect ([`art_cols`]: 5.61 columns on 7x14 px cells, 5.23 on
    /// 15x28), so new cells under a caret that keeps its cell (a window dragged between a 1x and
    /// a 2x display, a font step) move every station that reads the width: the wall side's, which
    /// stands the body's right edge off the caret, by the whole change; a console seat centred on
    /// its perch by half; one clamped at the grid's right edge with that edge. Where the station
    /// this tick measures (`target` on `target_row`, its aim) moved from the one the last tick
    /// that measured one measured (`seen`) by no more than the width's change, for the same caret
    /// cell and on the same row, on the tick that draws the new cells (`seen` was measured on the
    /// cells the tick before drew, `drawn` their width):
    /// - a body within [`ARRIVED`] of that station, at rest (no flight, no speed, no brake, not
    ///   walking, running or landing) and not yet moved this tick (`entry`, what the tick found
    ///   as it began), moves with it, its offset from it kept ([`Self::carry_resting_body`]),
    ///   whatever it does there (asleep, purring, crouched in hiding): the frame that draws the
    ///   new cells draws it on its station;
    /// - a flight in the air as the tick began, coming down within [`ARRIVED`] of that station,
    ///   comes down on the new one, rebased so the drawn body does not move
    ///   ([`Self::rebased_from`]), while it has flown no more than [`REAIM_MAX_U`] of its way
    ///   and the rest fits the animal's pace ([`Self::flight_cap`]);
    /// - a split bound's second landing ([`Self::bound2`]) within [`ARRIVED`] of it, set before
    ///   the tick, moves with it, the cat in the air or on the touch-down between the bounds.
    ///
    /// A flight or a second landing the tick itself sets off before its aim (an eviction's leap)
    /// was aimed on the new cells and is left as it is: re-aimed by the change again, it came
    /// down a width change past the new station.
    ///
    /// Left where they were, a resting body glided onto the new station over about 120 frames
    /// (2026-11-09: a wall-side cat 0.374 columns, a cat on top of Claude Code's composer 0.187),
    /// and a flight came down short of it and glided the rest. The ticks with no caret measure no
    /// station and keep the last one: a cat whose caret was gone for a frame just before the
    /// change is carried by the tick that draws the new cells and sees it again (measured against
    /// the tick before only, it glided the change as before). New cells drawn on a tick with no
    /// caret draw the body where it stood, and the locomotion takes it on from there: carried on
    /// a later tick, it would jump the change on cells that did not change, outside the
    /// no-teleport law's bound for a larger change of aspect (7x15 px cells to 15x29, 0.59
    /// columns). A dectcem hide on a finished frame is not such a tick: its caret is known,
    /// undrawn ([`PetSense::caret_drawn`]), and measures its station. A hidden cursor on a frame
    /// the program has not finished (DEC 2026, drawn past the host's hold) reads the last finished
    /// frame's caret, and none where the grid changed since: a window dragged between displays
    /// under such a frame draws its new cells caretless, as above. A body that was off glass is
    /// placed before the aim by the tick that sees the caret again, on the station it measures (the
    /// first sighting), so it has moved that tick and is not carried as well; a body that comes
    /// back to its station later goes there by the locomotion. A station that moved further (the
    /// wall's law's other side, a clamp at a grid edge that moved), a caret that moved, or another
    /// row leaves the body to the locomotion, as before. The grid's rows and columns may change
    /// with the cells: a window dragged between displays keeps its size in points, so its grid
    /// changes unless its cells scale exactly.
    #[inline]
    const fn keep_place_on_restation(
        &mut self,
        (seen, entry, drawn): (Option<SeenStation>, TickEntry, f32),
        (target, target_row): (f32, f32),
        caret: (u16, u16),
        (width, cols): (f32, u16),
    ) {
        let Some(was) = seen else {
            return;
        };
        let shift = target - was.at.0;
        if was.width == width
            || was.width != drawn
            || was.caret.0 != caret.0
            || was.caret.1 != caret.1
            || (target_row - was.at.1).abs() >= 0.05
            || shift.abs() > (width - was.width).abs() + 1e-4
        {
            return;
        }
        if let Some((c2, r2, _)) = &mut self.bound2
            && matches!(entry.bound2, Some((c, r)) if c == *c2 && r == *r2)
            && (*c2 - was.at.0).abs() <= ARRIVED
            && (*r2 - was.at.1).abs() < 0.5
        {
            *c2 += shift;
        }
        if let Some(f) = &mut self.flight {
            if matches!(entry.landing, Some((c, r)) if c == f.to_col && r == f.to_row)
                && (f.to_col - was.at.0).abs() <= ARRIVED
                && (f.to_row - was.at.1).abs() < 0.5
            {
                let to = f.to_col + shift;
                let u = (f.t / f.dur).clamp(0.0, 1.0);
                let in_time =
                    (to - self.col).abs() / (f.dur - f.t).max(1e-3) <= Self::flight_cap(f, cols);
                if u <= REAIM_MAX_U
                    && in_time
                    && let Some(from) = Self::rebased_from(self.col, to, u)
                {
                    f.from_col = from;
                    f.to_col = to;
                }
            }
            return;
        }
        if self.speed == 0.0
            && self.brake_over.is_none()
            && !matches!(
                self.action,
                PetAction::Walk | PetAction::Run | PetAction::Leap | PetAction::Land
            )
            && self.col == entry.at.0
            && self.row == entry.at.1
            && (self.row - target_row).abs() < 0.05
            && (self.col - was.at.0).abs() <= ARRIVED
        {
            self.carry_resting_body(shift);
        }
    }

    /// Moves a body at rest `dx` columns and what is drawn or kept at its place with it: where
    /// the last tick left it ([`Self::col_at_tick`]), its home, the seat a held look was adopted
    /// on, and the departures drawn still beside it ([`Self::carry_console_body`] moves them
    /// so too).
    #[inline]
    const fn carry_resting_body(&mut self, dx: f32) {
        self.col += dx;
        self.col_at_tick += dx;
        self.home += dx;
        if let Some(look) = &mut self.adopted_look {
            look.seat.0 += dx;
        }
        let mut i = 0;
        while i < self.departures.len() {
            if let Some(record) = &mut self.departures[i]
                && record.still
                && !record.resight
            {
                record.from_col += dx;
                record.edge += dx;
            }
            i += 1;
        }
    }

    #[inline]
    const fn carry_with_regrid(&mut self, dy: f32, rows: u16) -> bool {
        let last = rows.saturating_sub(1) as f32;
        #[inline]
        const fn fits(row: f32, dy: f32, last: f32) -> bool {
            let shifted = row + dy;
            shifted >= 0.0 && shifted <= last
        }
        let flight_fits = match &self.flight {
            Some(f) => fits(f.from_row, dy, last) && fits(f.to_row, dy, last),
            None => true,
        };
        let bound2_fits = match self.bound2 {
            Some((_, r2, _)) => fits(r2, dy, last),
            None => true,
        };
        if !fits(self.row, dy, last) || !flight_fits || !bound2_fits {
            return false;
        }
        if dy == 0.0 {
            return true;
        }
        self.row += dy;
        if let Some(f) = &mut self.flight {
            f.from_row += dy;
            f.to_row += dy;
        }
        if let Some((_, r2, _)) = &mut self.bound2 {
            *r2 += dy;
        }
        if let Some(look) = &mut self.adopted_look {
            look.seat.1 += dy;
        }
        if let Some((_, tr)) = &mut self.play_to {
            *tr = (*tr + dy).clamp(0.0, last);
        }
        if let Some(PlayLand::BatSwipe { row, .. }) = &mut self.play_land {
            *row = (*row + dy).clamp(0.0, last);
        }
        if let Some((_, br)) = &mut self.pending_bat {
            *br = (*br + dy).clamp(0.0, last);
        }
        if let Some((_, sr)) = &mut self.room_cheer_stage {
            *sr = (*sr + dy).clamp(0.0, last);
        }
        if let Some((rr, _)) = &mut self.console.repair_target {
            *rr = (*rr + dy).clamp(0.0, last);
        }
        if let Some((_, lr)) = &mut self.console.last_seat {
            *lr = (*lr + dy).clamp(0.0, last);
        }
        if let Some((_, sr)) = self.console.rest_seat.get_mut() {
            *sr = (*sr + dy).clamp(0.0, last);
        }
        if let Some((_, sr)) = self.console.rest_stand.get_mut() {
            *sr = (*sr + dy).clamp(0.0, last);
        }
        if let Some((_, row)) = &mut self.deferred_hidden_landing {
            *row = (*row + dy).clamp(0.0, last);
        }
        if let Some(target) = &mut self.console.target {
            target.row = (target.row + dy).clamp(0.0, last);
        }
        if let Some(trip) = &mut self.console.trip {
            trip.from.1 = (trip.from.1 + dy).clamp(0.0, last);
            trip.to.1 = (trip.to.1 + dy).clamp(0.0, last);
        }
        let mut i = 0;
        while i < self.departures.len() {
            if let Some(d) = &mut self.departures[i]
                && !d.resight
                && d.own_glass.is_none()
            {
                d.row = (d.row + dy).clamp(0.0, last);
            }
            i += 1;
        }
        let mut j = 0;
        while j < self.motes.len() {
            if let Some(m) = &mut self.motes[j] {
                m.row += dy;
            }
            j += 1;
        }
        true
    }

    /// The re-sighting crossfade.
    #[inline]
    pub fn resight_coordinate_space(&mut self) {
        #[inline]
        const fn has_any_carried(carried: &[Option<Carried>; PET_DEPARTURES_MAX]) -> bool {
            let mut i = 0;
            while i < PET_DEPARTURES_MAX {
                if carried[i].is_some() {
                    return true;
                }
                i += 1;
            }
            false
        }
        let carried = if has_any_carried(&self.carried) {
            self.carried
        } else {
            let mut carried = [None; PET_DEPARTURES_MAX];
            let mut slot_idx = 0;
            let mut i = 0;
            while i < PET_DEPARTURES_MAX {
                let Some(d) = self.departures[i] else {
                    i += 1;
                    continue;
                };
                let presence = self.last_drawn[i];
                let age = (self.lane_clock - d.born) as f32;
                if age < d.life() && presence > 0.0 {
                    let ghost = d.stopped_at(age);
                    let own_clear = match ghost.own_glass {
                        Some(_) => ghost.own_clear,
                        None => match self.console_still_rect_verdict(
                            ghost.from_col,
                            ghost.row,
                            ghost.stance0,
                        ) {
                            Some(clear) => clear,
                            None => ghost.own_clear,
                        },
                    };
                    carried[slot_idx] = Some(Carried {
                        ghost: Departure { own_clear, ..ghost },
                        presence,
                        home: false,
                    });
                }
                slot_idx += 1;
                i += 1;
            }
            carried
        };
        let pending = match self.resight.take() {
            Some(p) => Some(p),
            None => match (self.worn, self.last_now) {
                (Some((coat, iris)), Some(at)) if self.alpha > 0.0 && self.last_presence > 0.0 => {
                    Some(PendingResight {
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
                }
                _ => None,
            },
        };
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
        if let Some(pending) = &mut self.resight {
            pending.ghost.own_glass = Some(glass);
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

    #[inline]
    const fn spend_crossfade(
        &mut self,
        pending: PendingResight,
        carried: [Option<Carried>; PET_DEPARTURES_MAX],
    ) {
        #[inline]
        const fn total_cmp_key_f32(v: f32) -> i32 {
            let bits = v.to_bits() as i32;
            bits ^ ((((bits >> 31) as u32) >> 1) as i32)
        }
        #[inline]
        const fn resolve_glass(c: &Carried, displaced: Option<u32>) -> Option<Option<u32>> {
            match (c.ghost.own_glass, displaced) {
                (Some(_), _) if c.home => Some(None),
                (Some(own), _) => Some(Some(own)),
                (None, None) => Some(None),
                (None, Some(left)) => {
                    if c.ghost.own_clear {
                        Some(Some(left))
                    } else {
                        None
                    }
                }
            }
        }
        let displaced = pending.ghost.own_glass;
        let seat = (self.col, self.row);
        let mut adopted: Option<(usize, Carried)> = None;
        let mut i = 0;
        while i < PET_DEPARTURES_MAX {
            if let Some(c) = carried[i]
                && matches!(resolve_glass(&c, displaced), Some(None))
                && matches!(
                    self.worn,
                    Some((coat, iris)) if coat == c.ghost.coat && iris == c.ghost.iris
                )
                && (c.ghost.from_col - seat.0).abs() <= ADOPT_REACH
                && (c.ghost.row - seat.1).abs() < 0.5
            {
                let replace = match adopted {
                    Some((_, prev)) => {
                        total_cmp_key_f32(c.presence) >= total_cmp_key_f32(prev.presence)
                    }
                    None => true,
                };
                if replace {
                    adopted = Some((i, c));
                }
            }
            i += 1;
        }
        if let Some((_, body)) = adopted {
            self.col = body.ghost.from_col;
            self.row = body.ghost.row;
            self.facing_left = body.ghost.facing_left;
            if let Some(colors) = body.ghost.colors
                && matches!(
                    (body.ghost.surface, self.surface),
                    (Some((id0, alt0)), Some((id1, alt1))) if id0 == id1 && alt0 == alt1
                )
            {
                self.colors = Some(colors);
            }
        }
        self.adopted_look = match adopted {
            Some((_, body)) => Some(AdoptedLook {
                pose: body.ghost.pose0,
                facing_left: body.ghost.facing_left,
                stance: body.ghost.stance0,
                eased: 0.0,
                seat: (self.col, self.row),
                beneath: None,
            }),
            None => None,
        };
        self.alpha = 1.0;
        self.arrive_t = HOMECOMING_IN;
        self.arrive_in = HOMECOMING_IN;
        let mut handed = match adopted {
            Some((_, body)) => body.presence,
            None => 0.0,
        };
        if pending.ghost.own_glass.is_none() || pending.ghost.own_clear {
            self.spawn_departure(Departure {
                born: self.lane_clock,
                ..pending.ghost
            });
        } else {
            handed += pending.ghost.peak;
        }
        let mut rest: [(Carried, Option<u32>); PET_DEPARTURES_MAX] = [(
            Carried {
                ghost: pending.ghost,
                presence: 0.0,
                home: false,
            },
            None,
        ); PET_DEPARTURES_MAX];
        let mut rest_len = 0usize;
        let mut idx = 0;
        while idx < PET_DEPARTURES_MAX {
            let is_adopted = matches!(adopted, Some((adopted_idx, _)) if adopted_idx == idx);
            if !is_adopted && let Some(c) = carried[idx] {
                match resolve_glass(&c, displaced) {
                    Some(own) => {
                        let key = total_cmp_key_f32(c.presence);
                        let mut pos = rest_len;
                        while pos > 0 && key > total_cmp_key_f32(rest[pos - 1].0.presence) {
                            rest[pos] = rest[pos - 1];
                            pos -= 1;
                        }
                        rest[pos] = (c, own);
                        rest_len += 1;
                    }
                    None => handed += c.presence,
                }
            }
            idx += 1;
        }
        let mut free_slots = 0usize;
        let mut di = 0;
        while di < self.departures.len() {
            if self.departures[di].is_none() {
                free_slots += 1;
            }
            di += 1;
        }
        let mut ri = 0;
        while ri < rest_len {
            let (c, own_glass) = rest[ri];
            ri += 1;
            if free_slots == 0 {
                handed += c.presence;
                continue;
            }
            free_slots -= 1;
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

    #[inline]
    pub(super) const fn ink_safe_col_for(
        &self,
        want: f32,
        row: f32,
        width: f32,
        cols: u16,
        caret: Option<(u16, u16)>,
    ) -> f32 {
        #[inline]
        const fn clears_caret(caret_col: Option<f32>, col: f32, width: f32) -> bool {
            match caret_col {
                Some(cc) => col >= cc + STATION_LEAD || col + width <= cc - STATION_LEAD,
                None => true,
            }
        }

        let Some((first, end)) = (match self.observed_ink_overlaps(want, row, width) {
            Some(false) => return want,
            Some(true) => self.ink_span(row),
            None => match self.ink_span(row) {
                Some((first, end)) if want + width - INK_PAD > first && want + INK_PAD < end => {
                    Some((first, end))
                }
                _ => return want,
            },
        }) else {
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
        let beside_blocked = !(right_ok || left_ok)
            && (!clears_caret(caret_col, beside, width) || self.ink_overlaps(beside, row, width));
        if (beside - want).abs() > INK_EVICT_MAX || beside_blocked {
            let right_base = (want + INK_PAD).floor();
            let left_base = (want + width - INK_PAD).ceil() - width;
            let has_row_above = row >= 1.0;
            let row_above = row - 1.0;
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
                        if !has_row_above
                            || matches!(
                                self.observed_ink_overlaps(candidate, row_above, width),
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
            if beside_blocked {
                let mut fallback = None;
                let mut step_u16 = INK_EVICT_MAX as u16 + 1;
                while step_u16 < cols {
                    let step = step_u16 as f32;
                    step_u16 += 1;
                    let right_cand = right_base + step;
                    let left_cand = left_base - step;
                    if right_cand > limit && left_cand < 0.0 {
                        break;
                    }
                    let candidates = [right_cand, left_cand];
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
                            if !has_row_above
                                || matches!(
                                    self.observed_ink_overlaps(candidate, row_above, width),
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

    #[inline]
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
        if beside != want
            && (beside - want).abs() <= INK_EVICT_MAX
            && !self.ink_overlaps(beside, row, width)
        {
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
                if near != want
                    && (near - want).abs() <= INK_EVICT_MAX
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

    #[inline]
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
        if let Some(held) = self.watching_from_the_line(caret, width, cols) {
            return held;
        }
        let want = self.station_now(caret.1, cols, width);
        let row = f32::from(caret.0);
        let stand = self.ink_stand_for(want, row, width, cols, rows, Some(caret));
        let stand = self.clear_of_letters_above(stand, want, caret, (rows, cols), width);
        let stand = self.kept_off_the_line(stand, caret, width);
        if let Some(station) = self.console_station(stand, (rows, cols), width) {
            return station;
        }
        stand
    }

    /// A BODY OFF THE LINE STAYS OFF IT AS THE CARET GOES BACK INTO IT
    /// ([`OldPath::KeptOffTheLine`]): where the caret stands in its row's line, a letter on its
    /// cell or after it, and the stand by it (`stand`, on the caret's row) still lays the body on
    /// that row's letters (nothing took it off them: no clear stand along the row or on a row
    /// beside, [`Self::ink_stand_for`], and no seat within reach,
    /// [`Self::clear_of_letters_above`]) or in a gap between them, letters read on that row both
    /// short of the stand and past it (A GAP IN THE LINE IS THE LINE'S,
    /// [`OldPath::GapIsTheLines`]), a body standing on the caret's row keeps where it
    /// stands while the box it draws at rest there ([`Self::rest_box_cols`]) lies on none of that
    /// row's ink, off the line (past its last letter with the stand short of the body, or short of
    /// its first with the stand past it), and clears the caret by [`STATION_LEAD`] as every stand
    /// [`Self::ink_safe_col_for`] offers does; until the caret's stand comes clear of the letters
    /// (the caret back at the line's end, the line erased short of the stand, an Enter or a wrap
    /// that takes the caret off the row). So it never keeps a body over the caret to keep it off
    /// the letters, nor in a gap between the line's words, where the caret walked back comes to it,
    /// nor from a stand that takes it further off the line. A full page's last row is such a row:
    /// the row over it lettered in every cell and none under it, a line that leaves the body a seat
    /// at the wall leaves none by a caret walked back into it. Reverted, the body followed the
    /// caret onto the line's letters: ten Left arrows from 1.5 s past the last key stood it by the
    /// caret on them to the Enter, its box on them in 213 to 219 of the 219 frames from the first
    /// arrow, 198 to 204 of those at full alpha, reaching 11 to 14 px into them, and its flight off
    /// the seat rose into the row over the line in 20 or 21 (the trail gallery's `last-row`, a line
    /// four blank cells short of the grid's end on 13 x 29, 14 x 26 and 11 x 28 px cells and three
    /// on 14 x 26, the cat and the dog; rendered on 13 x 29, its box on them in 279 of 284 frames,
    /// 264 at full alpha, Linux arm64, 2026-11-08). A body kept wherever its box lay on no ink,
    /// gaps and the caret's cell included, traded letters for the caret: on the same row with four
    /// blank cells at the wall, a line ending in five spaces and 2, 4, 8 or 12 letters, the caret
    /// walked back through the gap and out again, it stood over the caret's cell in 28 to 75 frames
    /// at full alpha on 14 x 26 px cells, where reverted it stood there in 5 to 18
    /// (`a_body_kept_off_the_line_clears_the_caret_on_every_face`). Kept only from a stand on the
    /// letters, a body at the wall on that row left it for the gap once the fitted body fitted
    /// there clear of the caret (14 x 26 px cells, whose fitted body is 2.82 cells across): across
    /// the line's last letters and the caret's cell, then from the gap, on the line, after the
    /// caret's stand onto the letters before it and back onto the last ones as the caret came
    /// out, its box more than 2 px into them in 38 to 237 frames from the first arrow to the
    /// Enter and over the caret's cell in up to 16
    /// (`a_gap_in_the_line_is_the_line_s_on_every_face`, Linux arm64, 2026-11-08). The gap is
    /// read off the letters, not off the row's span ([`Self::ink_span`]), which can still hold
    /// letters a Ctrl+U or an Enter has just cleared: taken off the span, any blank stand inside
    /// it was the line's, and the body bound for the prompt past a Ctrl+U came down short of it
    /// and flew again (`every_flight_after_a_caret_jump_is_one_brisk_bound`), and the cat resting
    /// over a line lettered to the margin lay on a letter under the veil 15 ms past the Enter
    /// (`a_cat_under_a_line_lettered_to_the_margin_is_veiled_once`).
    #[inline]
    fn kept_off_the_line(&self, stand: (f32, f32), (cr, cc): (u16, u16), width: f32) -> (f32, f32) {
        let row = f32::from(cr);
        if self.old_path(OldPath::KeptOffTheLine)
            || self.flight.is_some()
            || self.console_places_body()
            || !width.is_finite()
            || width <= 0.0
            || (stand.1 - row).abs() >= 0.5
            || (self.row - row).abs() >= 0.5
            || (self.col - stand.0).abs() < 1e-3
        {
            return stand;
        }
        // A box edge read off whole px may fall a rounding short of the cell edge it stands on.
        const SLACK: f32 = 1e-3;
        let caret = f32::from(cc);
        let Some((first, end)) = self.ink_span(row) else {
            return stand;
        };
        // On the line: on its letters, or in a gap between them, letters read on the row both
        // short of the stand and past it ([`OldPath::GapIsTheLines`]), within what the map
        // reads about the stand ([`Self::read_about`]: the line whole where the map covers it).
        let on_the_line = match self.observed_ink_overlaps(stand.0, row, width) {
            Some(true) => true,
            Some(false) if !self.old_path(OldPath::GapIsTheLines) => {
                let (a, b) = self.read_about(row, (first, end), (stand.0, stand.0 + width));
                matches!(self.observed_ink_in_span(row, a, stand.0), Some(true))
                    && matches!(
                        self.observed_ink_in_span(row, stand.0 + width, b),
                        Some(true)
                    )
            }
            Some(false) | None => false,
        };
        if end <= caret + 0.5 || !on_the_line {
            return stand;
        }
        let cells = ((self.last_cell_w, self.last_cell_h), self.last_grid);
        let Some((x0, x1)) = self.rest_box_cols((self.col, self.row), cells, width) else {
            return stand;
        };
        // Past the line's end or short of its start, with the stand back toward the line.
        let off_the_line = (x0 + SLACK >= end && stand.0 < self.col)
            || (x1 - SLACK <= first && stand.0 > self.col);
        let clears_the_caret =
            x0 + SLACK >= caret + STATION_LEAD || x1 - SLACK <= caret - STATION_LEAD;
        if off_the_line
            && clears_the_caret
            && matches!(self.observed_ink_in_span(row, x0, x1), Some(false))
        {
            (self.col, self.row)
        } else {
            stand
        }
    }

    /// THE PROGRAM'S ROW IS ITS OWN. The hand's Enter hands the caret to the
    /// program it ran, and the caret stands at the start of the empty row
    /// under that line: whatever the program prints lands there (the trail
    /// gallery's `move-up`: `./k` printed `[vim] NORMAL` a second later on
    /// the row the cat had dropped onto, its body over `NORMAL`). Until the
    /// program writes on that row or moves the caret, a cat standing clear
    /// beside the line the hand ran stays there and watches it run. The
    /// Enter is the console's submit ([`Self::last_submit_seq`]) or, for a key
    /// that reached the terminal without passing the console (`aterm ctl key`,
    /// which the trail gallery types with), its echo: the caret's carriage
    /// return and line feed off the line ([`Self::line_returned`]). A cat
    /// still over the line's last letters when the Enter came (an Enter
    /// pressed before it had walked clear of the caret: the gallery's cat,
    /// over `k` of `./k`, dropped onto `NORMAL`'s row) steps clear of them
    /// along the line ([`Self::ink_safe_col_for`]) instead. A cat in the air
    /// whose landing lies on the line keeps it there: the Enter came while it
    /// flew along the line the hand ran (the gallery's `move-up`, looks8
    /// round 3: the landing took the caret's new row and the body lay on
    /// `NORMAL` in every walking companion's frame at `up`). Inert where neither was
    /// seen, the caret's row holds ink, the line above it holds none, the
    /// cat is dropping or flying to another row, or no clear stand is near
    /// it on the line.
    #[inline]
    const fn watching_from_the_line(
        &self,
        (cr, cc): (u16, u16),
        width: f32,
        cols: u16,
    ) -> Option<(f32, f32)> {
        let (col, row) = match self.flight {
            Some(f) if f.drop => return None,
            Some(f) => (f.to_col, f.to_row),
            None => (self.col, self.row),
        };
        if cc != 0
            || cr == 0
            || self.old_path(OldPath::WatchFromTheLine)
            || (self.last_submit_seq().is_none() && !self.line_returned)
            || self.ink_span(cr as f32).is_some()
            || self.ink_span(cr as f32 - 1.0).is_none()
            || (row - (cr as f32 - 1.0)).abs() >= 0.5
        {
            return None;
        }
        if !self.ink_overlaps(col, row, width) {
            return Some((col, row));
        }
        let clear = self.ink_safe_col_for(col, row, width, cols, Some((cr, cc)));
        if self.ink_overlaps(clear, row, width) {
            return None;
        }
        Some((clear, row))
    }

    /// THE HEAD LAW: a cat standing by the caret never rests its head on a line of letters
    /// ([`Self::observed_last_letter`], [`INK_GAP`], [`Self::veil_over_letters`]), and is never
    /// seated further from the writing than it must be. A seat is a stand whose feet are on no
    /// ink, whose head (in the row above its feet) lies under no letter and off the caret's
    /// cell, and which the console lets it rest in ([`Self::head_seat_along`]); distances are
    /// from the station beside the caret (`want`, by [`span_eq`]).
    ///
    /// A stand right of the caret on its row, or on the row under it where the ink sent it,
    /// whose head would lie among letters moves along its own row past the letters above it
    /// (a stand left of the caret, at the margin's wall, keeps to the margin's own rules,
    /// [`Self::margin_rest_seat`]):
    /// while the hand is at work ([`Self::hand_at_work`]), only to a seat within
    /// [`HEAD_SEAT_REACH`]; a seat at the far margin is not by the writing (a mid-line insert
    /// in a composer of full lines sat the cat 100 cells from it for ten seconds of typing).
    /// Where the ink already sent the stand off the caret's side (a row down, or a pounce's
    /// gap along the writing ahead of a mid-line caret, or nowhere off the ink at all), a seat
    /// on one of the [`HEAD_SEAT_ROWS`] rows under the caret's (a
    /// composer's footer under its bottom rule) that is nearer, or, for a stand among letters
    /// or on ink, no more than [`HEAD_SEAT_SLACK`] further, is taken instead. With none, the
    /// stand holds and the rest veil takes the letters: a cat beside the writing keeps to its
    /// row rather than leave it for a seat under the box, drawn down its row toward the blank
    /// row under it, or fitted to its row, where the row above its feet holds ink
    /// ([`Self::head_target`]). A seat
    /// under the line the cat already holds is kept through a wrap that is coming
    /// ([`Self::seat_kept_through_wrap`]).
    ///
    /// A FULL-SCREEN PROGRAM'S ROWS UNDER ITS LINE ARE ITS OWN
    /// ([`OldPath::FullScreenKeepsItsRow`]): on the alternate screen
    /// ([`Self::note_full_screen`]) the cat keeps its line's row, at a seat along it or at the
    /// stand the head law draws there, and seeks or keeps no seat on the rows under the
    /// caret's, unless a composer's rule lies over the caret's line or under it within
    /// [`HEAD_SEAT_ROWS`]: the composer's own seats govern there, the footer under its lower
    /// rule among them (Claude Code draws its composer on the alternate screen: without this
    /// exception, its long insert kept the cat over 30 cells from the caret, up to 115, in
    /// 1138 frames). Seated three rows under a full-screen prompt between two lettered rows,
    /// at rest and while the hand wrote, a cat flew up across the lettered row under the prompt
    /// at the first Backspace and back down across it once the caret rested: its box on that
    /// row's letters in 26 frames, its body on their ink in 15 (the `steam` scene on the
    /// alternate screen, typed at the window, under Linux's bundled JetBrains Mono).
    #[inline]
    fn clear_of_letters_above(
        &self,
        (col, row): (f32, f32),
        want: f32,
        caret: (u16, u16),
        (rows, cols): (u16, u16),
        width: f32,
    ) -> (f32, f32) {
        let caret_row = f32::from(caret.0);
        if row < 1.0
            || !(-0.5..1.5).contains(&(row - caret_row))
            || !width.is_finite()
            || width <= 0.0
            || self.console_executing()
        {
            return (col, row);
        }
        let limit = (f32::from(cols) - width).max(0.0);
        let by_a_rule = self.console_rule_at(caret_row - 1.0) || {
            let mut k = 0u8;
            let mut found = false;
            while k <= HEAD_SEAT_ROWS && !found {
                found = self.console_rule_at(caret_row + f32::from(k));
                k += 1;
            }
            found
        };
        let under_ok =
            !self.full_screen || by_a_rule || self.old_path(OldPath::FullScreenKeepsItsRow);
        if col < f32::from(caret.1) {
            return self
                .seat_kept_through_wrap(want, caret, (rows, cols, limit), width, under_ok)
                .unwrap_or((col, row));
        }
        let head_on_letters = matches!(
            self.observed_last_letter(
                row - 1.0,
                head_seat_from(col, width, !self.old_path(OldPath::SeatClearsTheStretch)),
                col + width - INK_PAD
            ),
            Some(Some(_))
        );
        let on_ink = self.ink_overlaps(col, row, width);
        // A STAND THE HEAD LAW DRAWS DOWN NEEDS NO SEAT UNDER THE LINE
        // ([`OldPath::SunkStandIsASeat`]): where the row under the body is blank under it, the
        // head law draws the body down its row ([`head_law`]), its head off the row above's
        // letters, its own row blank under it as the page shows it; the seat a row under the
        // line is the same place a fifth of a row lower. Sought there, a cat at rest by a prompt
        // under a command's output hopped down to it, and up again to the stand at the first
        // key: two hops for nothing (r24 K7-1). Nor along the row ([`OldPath::SunkStandSlides`],
        // below): the drawn-down stand is the seat.
        let sunk_clear = head_on_letters
            && !on_ink
            && !self.old_path(OldPath::SunkStandIsASeat)
            && (row - caret_row).abs() < 0.5
            && !matches!(self.observed_ink_overlaps(col, row, width), Some(true))
            && matches!(
                self.head_target(row, (rows, cols), (col, col + width)),
                Some(HeadRead { law: (sink, fit), .. }) if sink > 0.0 && fit >= 1.0
            );
        // A STAND THE HEAD LAW DRAWS DOWN IS A SEAT ([`OldPath::SunkStandSlides`]): drawn down
        // its row, the body by the caret covers no letter of either row, so it is where the cat
        // rests, as a stand whose head is clear is: no slide along the row to a whole seat past
        // the row above's letters. Slid there, the cat left the caret it followed: a held delete
        // under a line of words sent it running back from the caret, rising at the seat and
        // sinking again as the caret left it behind, and once the caret rested it bounded twice
        // to a seat 60 cells off, past the line's end (r29 K9-1, K9-2).
        let sunk_seat = sunk_clear && !self.old_path(OldPath::SunkStandSlides);
        let shifted = span_eq(col - want, row - caret_row);
        // Off the caret's side means a pounce's gap off it, or, for a cat already seated under
        // the caret's line, past [`INK_EVICT_MAX`]: the two thresholds keep a stand a few cells
        // along the writing from flipping the cat between the line and the seat under it as
        // the writing's tail and the station's lead move by fractions of a cell.
        let flight_or_row = match &self.flight {
            Some(f) => f.to_row,
            None => self.row,
        };
        let seated_under = flight_or_row - caret_row >= 1.5;
        let off_side = if seated_under {
            INK_EVICT_MAX
        } else {
            POUNCE_GAP
        };
        // ONE SEAT FOR THE WHOLE LINE. A cat seated under the line the hand is writing (on a
        // composer's footer under its lower rule) keeps that row while the row holds a seat for it, the
        // nearest along it to the station, wherever the line's re-layout puts the caret's row
        // (a mid-line insert that pushes its tail onto a new line grows the composer a row, and
        // opens room beside the caret at the line's end): the line's wrap is then its only
        // trip. Where the footer has no head room under the host's
        // chips, the seat there is the chips' edge, and the cat waits at it. Taken to the line
        // the growth opened under the caret instead, it flew up there, and down to the footer
        // again at the wrap: two flights a long line where one does.
        let kept_row = flight_or_row;
        let kept_below = (kept_row - caret_row).round();
        let sit_key = self.key_within_dur(SIT_AFTER_DUR);
        let writing = sit_key == Some(PetInputKind::Text);
        // THE WHOLE BODY BEFORE THE FIT ([`OldPath::WholeSeatOverFit`]): a stand the head law
        // would fit to its row takes the nearest seat within [`HEAD_SEAT_REACH`] where the body
        // is whole, along its row or on a row under the caret's, and a cat already seated whole
        // under the line keeps that row while it holds such a seat (one seat for the line, as
        // for the writing: a seat a row nearer by a twentieth of a cell sent it flying up across
        // the row under's letters at the line's end). Not on the alternate screen: a full-screen
        // program's rows under its composer are its own chrome, and the cat keeps the
        // composer's row there (`full_screen_tests`). Nor by a composer's rule (over the
        // caret's line or under it within [`HEAD_SEAT_ROWS`]): the composer's own seats govern
        // there, the footer under its lower rule the writing's (sent down to it from a fitted
        // stand beside the line as well, a cat writing a long line in Claude Code's composer
        // flew down and back up at the wrap, turned back on its way and flashed a pose).
        let fitted = !self.old_path(OldPath::WholeSeatOverFit)
            && !self.full_screen
            && !by_a_rule
            && matches!(
                self.head_target(row, (rows, cols), (col, col + width)),
                Some(HeadRead { law: (_, fit), .. }) if fit < 1.0
            );
        if seated_under
            && under_ok
            && (writing || fitted)
            && (kept_row - kept_row.round()).abs() < 0.05
            && (1.0..=f32::from(HEAD_SEAT_ROWS)).contains(&kept_below)
            && kept_row < f32::from(rows)
        {
            // The writing's seat is the row's nearest, however far along; a fitted stand's is
            // one within reach.
            let along = if writing {
                f32::INFINITY
            } else {
                HEAD_SEAT_REACH
            };
            let x_r = self.head_seat_along(
                (want, kept_row),
                caret,
                (rows, limit),
                width,
                (along, false),
            );
            if let Some(a) = x_r
                && (a - want).abs() == 0.0
            {
                return (a, kept_row);
            }
            let l_reach = match x_r {
                Some(a) => (a - want).abs() + 1.0,
                None => along,
            };
            let x_l = self.head_seat_along(
                (want, kept_row),
                caret,
                (rows, limit),
                width,
                (l_reach, true),
            );
            let nearest = match (x_r, x_l) {
                (Some(a), Some(b)) => {
                    if (a - want).abs() <= (b - want).abs() {
                        Some(a)
                    } else {
                        Some(b)
                    }
                }
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            if let Some(x) = nearest
                && (writing || span_eq(x - want, kept_below) <= HEAD_SEAT_REACH)
            {
                return (x, kept_row);
            }
        }
        let beside = (row - caret_row).abs() < 0.5 && shifted <= off_side && !on_ink;
        if beside && (!head_on_letters || sunk_seat) {
            return self
                .seat_kept_through_wrap(want, caret, (rows, cols, limit), width, under_ok)
                .unwrap_or((col, row));
        }
        let hand_work = matches!(sit_key, Some(kind) if kind != PetInputKind::Submit);
        let settled = match self.moved_at {
            Some(at) => (self.clock - at) as f32 >= HEAD_SEAT_SETTLE,
            None => true,
        };
        let lawful = (!head_on_letters || sunk_seat) && !on_ink;
        // A cat further from the stand than a short walk ([`BOUND_SPAN_MIN`]; where it stands,
        // or where its flight comes down) has nothing to watch the caret from: it travels there
        // either way, so it goes once, to where it rests. Sent to the stand among the letters
        // while the caret settled, and on to its seat after [`HEAD_SEAT_SETTLE`], the long
        // insert's cat gathered at the far margin for the stand and launched for the seat it
        // already stood on: a hop on the spot.
        let away = match &self.flight {
            Some(f) => span_eq(f.to_col - want, f.to_row - caret_row),
            None => span_eq(self.col - want, self.row - caret_row),
        } > BOUND_SPAN_MIN;
        // ...once the caret came by a jump no hand made and stayed put after it for [`SETTLE`]:
        // a program's writing that steps it along a key at a time is no caret at rest.
        let resting = settled || (away && self.caret_rested_after_jump());
        // Nor along the row past [`HEAD_SEAT_REACH`] while the caret is on the move: slid past
        // the letters over it to the far margin, a cat beside the line's end ran there as the
        // arrow keys walked the caret back into the line, and crossed back once they stopped.
        // Nor ever for a stand the head law fits to its row ([`OldPath::FittedStandSeatsFar`]):
        // fitted, the body by the caret covers no letter either, and a whole seat is worth
        // taking over it only by the caret. Sent at rest to the first whole seat however far
        // (past a listing's or a command's last line on the grid's last row), the cat waited
        // there up to 64 cells ahead while the person typed toward it (r29 K9-4).
        let fitted_near = fitted && !self.old_path(OldPath::FittedStandSeatsFar);
        let reach = if hand_work || !resting || fitted_near {
            HEAD_SEAT_REACH
        } else {
            f32::INFINITY
        };
        // A STAND THE HEAD LAW FITS IS KEPT BY A CAT AT IT ([`OldPath::FittedStandRunsAhead`]):
        // fitted, the body by the caret covers no letter, so a cat following the caret there, or
        // resting there, keeps it; a whole seat along the row is taken over it only on a trip
        // the cat makes anyway (in the air, on another row, or a pounce's gap off), or where the
        // cat already holds that seat. Slid to the seat ahead that the writing's stand brought
        // within reach, the cat ran on ahead of the writing to wait for it.
        //
        // A cat a pounce's gap off that runs the hand's caret down on its paws (a held key's)
        // makes no trip, and the seat it holds is the one it stands at, while the hand runs the
        // caret no other way ([`OldPath::SeatHeldAnywhereAhead`]): read as a trip, and as the
        // seat held by any cat ahead of the stand, a held delete under a line of words on the
        // grid's last row sent the cat running on to the seat past the words' end, away from the
        // caret deleting left, and waiting there until the caret was out of reach (r29 K9-1).
        let exact = !self.old_path(OldPath::SeatHeldAnywhereAhead);
        let trip_anyway = match &self.flight {
            Some(_) => true,
            None => {
                (self.row - row).abs() >= 0.5
                    || ((self.col - want).abs() > POUNCE_GAP
                        && !(exact && self.runs_down(want - self.col)))
            }
        };
        let seat = if lawful {
            None
        } else {
            match self.head_seat_along((col, row), caret, (rows, limit), width, (reach, false)) {
                Some(x) => {
                    let d = span_eq(x - want, row - caret_row);
                    if d <= reach { Some((d, x, row)) } else { None }
                }
                None => None,
            }
        };
        let holds_ahead = self.flight.is_none()
            && (self.row - row).abs() < 0.5
            && self.col > want + ARRIVED
            && (!exact
                || (matches!(seat, Some((_, x, _)) if (self.col - x).abs() <= ARRIVED)
                    && !(self.run_dir < 0 && self.rhythm_open())));
        let slides = !fitted_near
            || trip_anyway
            || holds_ahead
            || self.old_path(OldPath::FittedStandRunsAhead);
        let mut best: Option<(f32, f32, f32)> = if lawful {
            Some((shifted, col, row))
        } else if !slides {
            None
        } else {
            seat
        };
        // A stand among letters by a caret at rest, the hand away, that the slide along its row
        // took off to a seat out past the reach the hand's work allows: the seats under the box
        // nearer than that seat are taken first. Slid along the row alone, the long insert's
        // cat went to the far margin, 85 cells from the caret, past the composer's footer 2 rows
        // under it. Where the slide finds no seat at all the stand holds, its head veiled, as a
        // cat by a program's writing that paused does.
        let rest_search = match best {
            Some((d, ..)) => resting && !lawful && !hand_work && d > HEAD_SEAT_REACH,
            None => false,
        };
        let budget = match best {
            Some((d, ..)) if rest_search => d,
            _ if fitted => HEAD_SEAT_REACH,
            _ if lawful => shifted,
            _ => shifted + HEAD_SEAT_SLACK,
        };
        // A cat beside the writing keeps to its row with its head veiled; one already seated
        // under the caret's line stays seated rather than come up to a stand among letters
        // (the stand beside a mid-line insert, drawn within [`INK_EVICT_MAX`] of the station
        // by the writing's pushed tail, brought it up onto the words from the footer). Only
        // the writing, or a caret at rest [`HEAD_SEAT_SETTLE`] where the hand or a program
        // left it, sends the cat to a seat under the box: a caret walked back and forth by
        // words is watched from where the cat stands (sent down as alt-left took the caret
        // three words back, the cat came up again as alt-right brought it home).
        if (((writing || settled) && !beside) || seated_under || rest_search || fitted)
            && !sunk_clear
            && under_ok
        {
            let mut below = 1;
            while below <= HEAD_SEAT_ROWS {
                let r = caret_row + f32::from(below);
                let drop = ROW_AS_COLS * f32::from(below);
                if r >= f32::from(rows)
                    || drop >= budget
                    || matches!(best, Some((d, ..)) if drop >= d)
                {
                    break;
                }
                let along = crate::color_math::sqrt_f32((budget * budget - drop * drop).max(0.0));
                let dirs = [false, true];
                let mut di = 0;
                while di < 2 {
                    let leftward = dirs[di];
                    di += 1;
                    let Some(x) = self.head_seat_along(
                        (want, r),
                        caret,
                        (rows, limit),
                        width,
                        (along, leftward),
                    ) else {
                        continue;
                    };
                    let d = span_eq(x - want, r - caret_row);
                    if d < budget && !matches!(best, Some((bd, ..)) if d >= bd) {
                        best = Some((d, x, r));
                    }
                }
                below += 1;
            }
        }
        // THE WRITING COMES TO A SEAT AHEAD ([`OldPath::SeatAheadPulledBack`]): where no seat in
        // the hand's reach keeps the head law and the stand would lay the head among the row
        // above's letters, a cat down on a seat that keeps the law on the caret's row ahead of
        // the caret (the whole body's seat past the row above's last letter the cat took at
        // rest; the seat as the seat search finds it, to its step) keeps it while the hand
        // writes on toward it, until the writing reaches it. Pulled in
        // to the hand's reach at the first key, the cat under a listing's ragged ends flew back
        // 29 cells to a fitted stand by the caret, and out again to the seat a second later as
        // the writing brought it within reach: two 0.64 s flights for a word typed (r24 K7-1).
        if best.is_none()
            && writing
            && self.run_dir >= 0
            && self.flight.is_none()
            && !self.old_path(OldPath::SeatAheadPulledBack)
            && (self.row - caret_row).abs() < 0.5
            && self.col > want + ARRIVED
            && self.col >= f32::from(caret.1) + 1.0
            && !lawful
            && matches!(
                self.head_seat_along(
                    (self.col, self.row),
                    caret,
                    (rows, limit),
                    width,
                    (SEAT_STEP_MIN, false),
                ),
                Some(x) if x - self.col <= SEAT_STEP_MIN
            )
        {
            return (self.col, self.row);
        }
        match best {
            Some((_, x, r)) => (x, r),
            None => (col, row),
        }
    }

    /// A seat under the caret's line the cat already sits on, kept while the writing will wrap
    /// within [`WRAP_WAIT`] at its pace ([`Self::clear_of_letters_above`]): the stand beside
    /// the caret that clears near the margin as a mid-line insert pushes its tail to the next
    /// line is not worth the flight up to it and the flight down again at the wrap. None where
    /// no seat under the line may be kept (`under_ok` false: the alternate screen's).
    #[inline]
    const fn seat_kept_through_wrap(
        &self,
        want: f32,
        caret: (u16, u16),
        (rows, cols, limit): (u16, u16, f32),
        width: f32,
        under_ok: bool,
    ) -> Option<(f32, f32)> {
        let below = (self.row - caret.0 as f32).round();
        if !under_ok
            || self.flight.is_some()
            || below < 2.0
            || below > HEAD_SEAT_ROWS as f32
            || (self.row - self.row.round()).abs() > 0.05
            || !self.typing_on()
            || cols.saturating_sub(caret.1) as f32 > WRAP_WAIT * self.writing_pace()
        {
            return None;
        }
        let row = caret.0 as f32 + below;
        let r_seat = self.head_seat_along(
            (want, row),
            caret,
            (rows, limit),
            width,
            (HEAD_SEAT_SLACK, false),
        );
        if let Some(a) = r_seat
            && (a - want).abs() == 0.0
        {
            return Some((a, row));
        }
        let l_reach = match r_seat {
            Some(a) => ((a - want).abs() + 1.0).min(HEAD_SEAT_SLACK),
            None => HEAD_SEAT_SLACK,
        };
        let l_seat =
            self.head_seat_along((want, row), caret, (rows, limit), width, (l_reach, true));
        match (r_seat, l_seat) {
            (Some(a), Some(b)) => {
                if (a - want).abs() <= (b - want).abs() {
                    Some((a, row))
                } else {
                    Some((b, row))
                }
            }
            (Some(x), None) | (None, Some(x)) => Some((x, row)),
            (None, None) => None,
        }
    }

    /// The first seat from `from` along row `row` (rightward, or leftward for `leftward`), at
    /// most `reach` columns on, that keeps the head law ([`Self::clear_of_letters_above`]): no
    /// ink under its feet, no letter in the row above them, the caret's cell clear of its body,
    /// a stand the console lets it rest in. A seat found past ground that fails is drawn back
    /// to where that ground ends (to a sixteenth of a cell), so it lies where the obstacle
    /// ends and not wherever the search's whole-cell steps from `from` fell.
    #[inline]
    const fn head_seat_along(
        &self,
        (from, row): (f32, f32),
        (caret_row, caret_col): (u16, u16),
        (rows, limit): (u16, f32),
        width: f32,
        (reach, leftward): (f32, bool),
    ) -> Option<f32> {
        #[derive(Clone, Copy)]
        enum SeatStep {
            Unseen,
            Clear,
            Blocked,
            Letter(usize),
        }

        #[inline]
        const fn seat(
            brain: &PetBrain,
            x: f32,
            row: f32,
            rows: u16,
            width: f32,
            over_caret: bool,
            (past_caret, short_of_caret): (f32, f32),
        ) -> SeatStep {
            if over_caret && x < past_caret && x > short_of_caret {
                return SeatStep::Blocked;
            }
            if brain.ink_overlaps(x, row, width)
                || (!brain.old_path(OldPath::SeatReadsItsOwnRow)
                    && matches!(brain.observed_ink_overlaps(x, row, width), Some(true)))
            {
                return SeatStep::Blocked;
            }
            let stretch = !brain.old_path(OldPath::SeatClearsTheStretch);
            let from = head_seat_from(x, width, stretch);
            let to = x + width - INK_PAD;
            let marks = if stretch {
                brain.word_marks_end(row - 1.0, from, to)
            } else {
                None
            };
            match brain.observed_last_letter(row - 1.0, from, to) {
                None => SeatStep::Unseen,
                Some(Some(last)) => SeatStep::Letter(last),
                // A word's trailing marks (a sentence's full stop) are ink the head law reads:
                // the seat is past them, as past the word.
                Some(None) if marks.is_some() => match marks {
                    Some(end) => SeatStep::Letter(end),
                    None => SeatStep::Blocked,
                },
                Some(None) => {
                    if brain.console_admits_rest((x, row), rows, width) {
                        SeatStep::Clear
                    } else {
                        SeatStep::Blocked
                    }
                }
            }
        }

        let over_caret =
            (row - caret_row as f32).abs() < 0.5 || (row - 1.0 - caret_row as f32).abs() < 0.5;
        let bounds = (
            caret_col as f32 + STATION_LEAD,
            caret_col as f32 - STATION_LEAD - width,
        );
        let (past_caret, short_of_caret) = bounds;
        let step = if leftward { -1.0 } else { 1.0 };
        let mut failed: Option<f32> = None;
        let mut x = from;
        while x >= 0.0 && x <= limit && (x - from).abs() <= reach {
            if over_caret && x < past_caret && x > short_of_caret {
                failed = None;
                x = if leftward { short_of_caret } else { past_caret };
                continue;
            }
            let step_kind = seat(self, x, row, rows, width, over_caret, bounds);
            if matches!(step_kind, SeatStep::Unseen) {
                return None;
            }
            if matches!(step_kind, SeatStep::Clear) {
                let Some(mut bad) = failed else {
                    return Some(x);
                };
                let mut good = x;
                let mut iter = 0;
                while iter < 4 {
                    let mid = 0.5 * (bad + good);
                    match seat(self, mid, row, rows, width, over_caret, bounds) {
                        SeatStep::Clear => good = mid,
                        _ => bad = mid,
                    }
                    iter += 1;
                }
                return Some(good);
            }
            failed = Some(x);
            x = match step_kind {
                // Past the last letter over the head; never back onto the column it
                // stood at, which rounding of the head's span could otherwise
                // repeat for ever.
                SeatStep::Letter(last) if !leftward => {
                    failed = None;
                    let lead =
                        x - head_seat_from(x, width, !self.old_path(OldPath::SeatClearsTheStretch));
                    (last as f32 + 1.0 + lead).max(x + SEAT_STEP_MIN)
                }
                _ => x + step,
            };
        }
        None
    }

    /// THE HEAD LAW'S SLIDE ([`Self::over_the_hosts_line`]): the first column from `col`
    /// rightward where a body `width` wide standing on `row` keeps its head off the letters on
    /// the row over it, and its feet off ink. `None` where that row is unseen, where its letters
    /// run on past the last column a body fits, or where the column past them has ink at the feet.
    #[inline]
    pub(super) const fn past_letters_above(
        &self,
        col: f32,
        row: f32,
        cols: u16,
        width: f32,
    ) -> Option<f32> {
        let above = row - 1.0;
        if above < 0.0 || !width.is_finite() || width <= 0.0 {
            return None;
        }
        let limit = (cols as f32 - width).max(0.0);
        let mut x = col;
        let mut step = 0u16;
        let stretch = !self.old_path(OldPath::SeatClearsTheStretch);
        let lead = col - head_seat_from(col, width, stretch);
        while step < cols {
            step += 1;
            let a = x - lead;
            let b = x + width - INK_PAD;
            let marks = if stretch {
                self.word_marks_end(above, a, b)
            } else {
                None
            };
            match self.observed_last_letter(above, a, b) {
                None => return None,
                Some(None) if marks.is_some() => {
                    if let Some(end) = marks {
                        x = end as f32 + 1.0 + lead;
                    }
                    if x > limit {
                        return None;
                    }
                }
                Some(None) => {
                    if x > limit || self.ink_overlaps(x, row, width) {
                        return None;
                    }
                    return Some(x);
                }
                Some(Some(last)) => {
                    x = last as f32 + 1.0 + lead;
                    if x > limit {
                        return None;
                    }
                }
            }
        }
        None
    }

    /// The last cell of a word's trailing marks where they are all the ink the span `[a, b)` of
    /// `row` holds: ink that is no letter (a full stop, a comma, a closing bracket), at most
    /// [`WORD_MARKS_MAX`] cells, run on from a letter just before the span and ended by a blank
    /// cell. A seat the head law reads over them is fitted there, so the seat search steps past
    /// them as past the word ([`OldPath::SeatClearsTheStretch`]). A fill or a rule (a run that
    /// starts at no letter, or runs on) is not a word's: the body is fitted under it as before.
    #[inline]
    const fn word_marks_end(&self, row: f32, a: f32, b: f32) -> Option<usize> {
        if !a.is_finite() || !b.is_finite() || b <= a {
            return None;
        }
        let (first, end) = (a.max(0.0).floor() as usize, b.ceil() as usize);
        let mut c = first;
        let mut run: Option<(usize, usize)> = None;
        while c < end {
            match self.observed_ink_in_span(row, c as f32, c as f32 + 1.0) {
                None => return None,
                Some(true) => {
                    run = match run {
                        None => Some((c, c)),
                        Some((s, l)) if l + 1 == c => Some((s, c)),
                        Some(_) => return None,
                    };
                }
                Some(false) => {}
            }
            c += 1;
        }
        let Some((s, l)) = run else {
            return None;
        };
        if s == 0 || l + 1 - s > WORD_MARKS_MAX {
            return None;
        }
        if !matches!(
            self.observed_last_letter(row, (s - 1) as f32, s as f32),
            Some(Some(_))
        ) {
            return None;
        }
        match self.observed_ink_in_span(row, (l + 1) as f32, (l + 2) as f32) {
            Some(false) => Some(l),
            _ => None,
        }
    }

    /// THE HEAD LAW IN FLIGHT ([`HEAD_CLEAR`]): a bound along the body's row under a lettered
    /// row two above its feet rises no higher than keeps its head under that row: judged at
    /// take-off and on each frame drawn in the air, over the rest of its way, so letters that
    /// land there after take-off lower it (a prompt's blank line on the grid's last row, its
    /// command's last line printed two rows above while the cat bounded back along the row:
    /// its head lay on that line for 18 frames; the frames' judging is reverted by
    /// [`OldPath::ArcJudgedInTheAir`]). On a pane past the map's columns, a way longer than a
    /// window reads is read as far either way from the body as the map reads about it
    /// ([`Self::read_about`]). The head law at rest and along the row ([`Self::head_target`])
    /// keeps it off the row above its feet. Reverted by [`OldPath::HeadClearInFlight`].
    #[inline]
    const fn arc_under_letters(&self, to_col: f32, to_row: f32, width: f32, arc: f32) -> f32 {
        // Reverted ([`OldPath::HeadClearInFlight`]): a quarter row's reach into the lettered
        // row.
        let ceiling = if self.old_path(OldPath::HeadClearInFlight) {
            2.0 - ART_ROWS + 0.25
        } else {
            2.0 - ART_ROWS - HEAD_CLEAR
        };
        if arc <= ceiling || (to_row - self.row).abs() >= 0.5 || !width.is_finite() || width <= 0.0
        {
            return arc;
        }
        let over = self.row.round() - 2.0;
        let (a, b) = self.read_about(
            over,
            (
                self.col.min(to_col) + INK_PAD,
                self.col.max(to_col) + width - INK_PAD,
            ),
            (self.col + INK_PAD, self.col + width - INK_PAD),
        );
        match self.observed_last_letter(over, a, b) {
            Some(Some(_)) => ceiling.max(0.0),
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

    #[inline]
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
                return ((row - edge) * (1.0 / ZEE_INK_FADE_BAND)).clamp(0.0, 1.0);
            }
            k += 1;
        }
        1.0
    }

    #[inline]
    const fn mote_px_cell(&self, col: f32, row: f32) -> (f32, f32) {
        let (cw, ch) = self.mote_cell_px();
        ((col * cw).round() / cw, (row * ch).round() / ch)
    }

    /// The terminal bel rang in the chased pane.
    #[inline]
    pub const fn note_bell(&mut self, now: Instant) {
        self.pending_bell = Some(now);
    }

    /// A shell command completed in the focused pane (OSC 133/633 d, deduped by the host).
    #[inline]
    pub fn note_command_done(&mut self, now: Instant, failed: bool, dur_ms: Option<u64>) {
        let pet_talk = match self.trick_submit_at.take() {
            Some(at) => now.saturating_duration_since(at) <= TRICK_SUBMIT_FORGIVEN_DUR,
            None => false,
        };
        let forgiven_trick = failed
            && pet_talk
            && match dur_ms {
                Some(dur) => dur < CHEER_MIN_MS,
                None => true,
            };
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
        let forgiven = match self.sulk_at {
            Some(at) => (self.clock - at) as f32 <= SULK_FORGIVEN,
            None => false,
        };
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
    #[inline]
    pub const fn note_executing(&mut self, now: Instant, executing: bool) {
        if executing {
            if self.exec_since.is_none() {
                self.exec_since = Some(now);
                self.rearm_idle_ears();
            }
        } else {
            self.exec_since = None;
            self.vigil_armed = false;
        }
    }

    #[inline]
    pub(super) const fn rearm_idle_ears(&mut self) {
        self.twitch_spent = false;
        self.room_sibling_spent = false;
    }

    #[inline]
    pub(super) const fn note_burst_twitch(&mut self, output_burst: bool, eligible: bool) {
        let burst_edge = output_burst && !self.last_burst;
        self.last_burst = output_burst;
        if burst_edge
            && eligible
            && !self.twitch_spent
            && self.twitch_cool <= 0.0
            && self.twitch_t <= 0.0
        {
            self.twitch_t = TWITCH_DUR;
            self.twitch_cool = TWITCH_COOL;
            self.twitch_spent = true;
            self.twitch_up = self.mote_serial.is_multiple_of(2);
            self.mote_serial = self.mote_serial.wrapping_add(1);
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
    #[inline]
    pub fn note_trick(&mut self, now: Instant, trick: Trick, confirmed: bool) {
        let heard = self.trick_heard.take();
        if confirmed
            && self.pending_trick.is_none()
            && self.trick_go.is_none()
            && match heard {
                Some((t, at)) => {
                    t as u8 == trick as u8 && now.saturating_duration_since(at) <= TRICK_CAP_DUR
                }
                None => false,
            }
        {
            return;
        }
        self.pending_trick = Some(match self.pending_trick {
            Some(p) if p.trick as u8 == trick as u8 => PendingTrick {
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
    #[inline]
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
    #[inline]
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
        self.rearm_idle_ears();
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
    const fn cheer_motes_aloft(&self) -> usize {
        let mut n = 0;
        let mut i = 0;
        while i < self.motes.len() {
            if let Some(m) = &self.motes[i]
                && matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart)
            {
                let age = m.age(self.clock);
                if age >= 0.0 && age < 1.0 {
                    n += 1;
                }
            }
            i += 1;
        }
        n
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

    /// How far (rows) the head law draws the body down its row this frame, and the share of its
    /// height it fits it to ([`Self::ease_head`], [`head_law`]): a test's reading of whether the
    /// law holds the body under a row above.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn head_sink_fit(&self) -> (f32, f32) {
        (self.sink, self.fit)
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

    /// A test's negative control: the cat keeps every path it kept before it
    /// learned to keep off the letters and icons it meets on them ([`OldPath`],
    /// each law [`Self::set_old_path`] reverts).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_old_paths(&mut self, old: bool) {
        self.old_paths = if old { OldPath::ALL } else { 0 };
    }

    /// A test's negative control: the cat keeps the path it kept before
    /// `law` kept it off the letters and icons it meets there ([`OldPath`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_old_path(&mut self, law: OldPath, old: bool) {
        if old {
            self.old_paths |= law as u128;
        } else {
            self.old_paths &= !(law as u128);
        }
    }

    /// Whether [`Self::set_old_path`] reverts `law`.
    #[inline]
    const fn old_path(&self, law: OldPath) -> bool {
        #[cfg(any(test, debug_assertions))]
        let old = self.old_paths & law as u128 != 0;
        #[cfg(not(any(test, debug_assertions)))]
        let old = {
            let _ = law;
            false
        };
        old
    }

    /// Whether a cheer is latched and not yet played or wiped.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub const fn cheer_owed(&self) -> bool {
        self.pending_cheer.is_some()
    }

    /// Whether another pane in a split is streaming this frame.
    #[inline]
    pub fn note_room_sibling(&mut self, now: Instant, talking: Option<(f32, f32)>) {
        let level = matches!(talking, Some((c, r)) if c.is_finite() && r.is_finite());
        let active = self.room_sibling
            || matches!(
                self.room_sibling_at,
                Some(at) if now.saturating_duration_since(at) < ROOM_SIBLING_QUIET_DUR
            );
        self.room_sibling = level;
        if level {
            self.room_sibling_at = Some(now);
            if !active
                && !self.room_sibling_spent
                && let Some((col, _)) = talking
            {
                self.room_sibling_spent = true;
                self.pending_room = Some(RoomCue::Look { col });
                self.room_at = Some(now);
            }
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
    #[inline]
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
    ///
    /// A tick after one that asked for no frames ([`Self::needs_frames`]) ends a sleep, through
    /// which the brain promised to draw nothing new: whatever changes on it starts there. It
    /// moves the body, and runs the clocks that draw (a fade, a heat, a flinch, a hold, the
    /// console's trip), by none of the sleep. A glide, a walk or a heat it starts asks for the
    /// next frame ([`Self::frame_owed`]), and so does a body it leaves a pixel off its aim
    /// ([`Self::owe_set_off`]), so a body sets off from the tick that woke it however long the
    /// host slept, as on a host that ticked all along. The clocks the rest holds to (how long a
    /// pose has been worn, a run's windows, a gait rested and its stride planted, the content
    /// an arrival tires) count all of the sleep a tick counts, five seconds at most, and spend
    /// it before what the tick brings ([`Self::keep_rest`]).
    #[inline]
    pub fn tick(&mut self, sense: PetSense) -> PetFrame {
        let (wrapped, carried) = self.live_late_span(sense);
        let frame = self.tick_once(PetSense {
            wrapped: sense.wrapped && !wrapped,
            ..sense
        });
        // The frame's reason names what its span did to the body: the console carried it with
        // the page on a step of the span, and this frame found it carried.
        if carried {
            self.name_the_spans_carry();
        }
        // The one seat rule is asked beside the frame and draws nothing ([`Self::seat_shadow`]):
        // only where debug assertions are on, so no shipped frame pays for an answer nobody
        // sees. What draws from the rule takes this gate out.
        if cfg!(any(test, debug_assertions)) {
            self.ask_the_seat_rule(sense);
        }
        // The deadline a host that draws on demand arms on this frame's reading: the next late
        // frame lives the step it names, not where that frame's reading of the page moves it.
        self.span_due = if self.asked_frames {
            None
        } else {
            self.next_change_deadline(sense.now)
        };
        frame
    }

    /// The caret's moves since the last frame, each at the time its output batch moved it
    /// (`aterm_core`'s `Terminal::caret_path`, oldest first), for the next tick's late span
    /// ([`Self::live_late_span`]); a host that sends none has the span met with its frame's
    /// caret.
    #[inline]
    pub const fn note_caret_path(&mut self, path: [Option<(Instant, (u16, u16))>; CARET_PATH_LEN]) {
        self.caret_path = path;
    }

    /// THE SPAN A LATE FRAME MISSED IS LIVED (r29 K10-8). A host the brain asked frames of that
    /// brings the next one late (a starved machine drew a tenth of a second and more apart, and
    /// its main thread stalled for seconds) would otherwise reach it in one step, whose motion spends no more than a tenth of a
    /// second ([`Self::advance`]'s `dt`): its flights took a second and a half, the cat fell
    /// behind the caret, and the caret's moves it caught up with read as jumps it hopped for, so
    /// a starved window's cat made decisions a steady one's did not. The span up to this frame
    /// is lived first a sixtieth of a second at a time ([`LIVE_STEP`]) on what this frame sees,
    /// with the caret where it stood at each step: where the caret's path put it by then
    /// ([`Self::note_caret_path`]), each move at its batch's own time, when the path ends at
    /// this frame's caret; else where this frame sees it from the span's first step, as one
    /// tick meets it. So the frame draws the moment a steady host's would. A frame no later than
    /// one tick's motion spends ([`STEP_DT_MAX`]) with no more than one move to meet is that
    /// tick. A host away longer than [`LATE_SPAN_MAX`] (a hidden window, a sleeping machine) is
    /// no late frame, nor one away longer than [`LATE_SPAN_UNTIMED_MAX`] whose caret the path
    /// does not time. A brain that asks no frames is lived on the steps a host that draws on
    /// demand would have drawn it on: the step its next visible change was due, and the step a
    /// move of the caret's path came; lived only on the frame, a wake due to end on the way
    /// ended at the late frame instead, and the cat gathered that much later. Returns whether a
    /// step took the frame's wrap, which the frame then does not repeat, and whether a step's
    /// console carried the body with the page ([`Self::place_console_body_at_home`]).
    #[inline]
    fn live_late_span(&mut self, sense: PetSense) -> (bool, bool) {
        let path = core::mem::replace(&mut self.caret_path, [None; CARET_PATH_LEN]);
        if self.old_path(OldPath::LateSpanInSteps) {
            return (false, false);
        }
        let Some(prev) = self.last_now else {
            return (false, false);
        };
        let mut carried = false;
        let timed = !self.old_path(OldPath::LateCaretAtItsTimes)
            && sense.caret.is_some()
            && path.iter().flatten().next_back().map(|&(_, cell)| cell) == sense.caret;
        let longest = if timed && !self.old_path(OldPath::StallIsLived) {
            LATE_SPAN_MAX
        } else {
            LATE_SPAN_UNTIMED_MAX
        };
        if sense.now.saturating_duration_since(prev) > longest {
            return (false, false);
        }
        // A frame no later than one tick's motion spends, with no more than one of the caret's
        // moves to meet in it, is the one tick it always was.
        let moves = if timed {
            path.iter().flatten().filter(|&&(at, _)| at > prev).count()
        } else {
            0
        };
        if sense.now.saturating_duration_since(prev) <= STEP_DT_MAX && moves < 2 {
            return (false, false);
        }
        let mut caret = match self.last_caret {
            Some(c) if timed => Some(c),
            _ => sense.caret,
        };
        let mut wrap_owed = sense.wrapped;
        let mut t = prev + LIVE_STEP;
        // A brain that asks no frames is lived where a host that draws on demand draws it: at
        // the step its next visible change was due ([`Self::next_change_deadline`], as the last
        // frame left it: this frame's reading of the page came at this frame, and a deadline it
        // moves earlier is no step that host drew), and at the step a move of the caret's path
        // came on.
        let mut lived_to = prev;
        let mut due = if self.asked_frames {
            None
        } else {
            self.span_due
        };
        while sense.now.saturating_duration_since(t) >= LIVE_STEP / 2 {
            let woken = timed
                && path
                    .iter()
                    .flatten()
                    .any(|&(at, _)| at > lived_to && at <= t);
            if !self.asked_frames && !woken && !due.is_some_and(|at| at <= t) {
                t += LIVE_STEP;
                continue;
            }
            lived_to = t;
            let was = caret;
            if timed {
                caret = path
                    .iter()
                    .flatten()
                    .rfind(|&&(at, _)| at <= t)
                    .map_or(caret, |&(_, cell)| Some(cell));
            }
            // The wrap goes with the step that takes the caret down a row (the first, untimed).
            let wrapped = wrap_owed
                && (!timed || matches!((was, caret), (Some((wr, _)), Some((cr, _))) if cr > wr));
            wrap_owed &= !wrapped;
            let _ = self.tick_once(PetSense {
                now: t,
                caret,
                wrapped,
                ..sense
            });
            carried |= self.console_carried();
            due = if self.asked_frames {
                None
            } else {
                self.next_change_deadline(t)
            };
            t += LIVE_STEP;
        }
        (sense.wrapped && !wrap_owed, carried)
    }

    #[inline]
    fn tick_once(&mut self, sense: PetSense) -> PetFrame {
        let at = (self.col, self.row);
        self.aim = None;
        self.writing_wait = None;
        let frame = self.advance(sense);
        self.drawn_pose = frame.pose;
        self.set_off_owed = false;
        let driven = self.needs_frames();
        self.owe_set_off(at, driven);
        self.asked_frames = driven || self.set_off_owed;
        frame
    }

    /// Owes the next frame to a body this tick left a whole pixel or a row off its aim
    /// ([`Self::aim`]) and did not move from `at`, where it stood when the tick began, on a tick
    /// that asks no frame otherwise (`driven`). A hold that let go on this tick (a stale front
    /// seat), or a branch that handed the body on and returned before the walk, leaves the walk
    /// to the next tick, which a host that ticked all along runs a frame later and a host at the
    /// brain's offers runs whenever anything else comes. The frame finds out: a body it sets off draws its own frames from there, and one it keeps where it was (on
    /// its front seat, under a cue's act) is not asked for again until it or its aim moves by a
    /// pixel or a row, or frames are asked for something else and stop again, so a body kept off
    /// its aim on purpose costs a frame, never a frame train.
    #[inline]
    const fn owe_set_off(&mut self, at: (f32, f32), driven: bool) {
        let unmoved = at.0 == self.col && at.1 == self.row;
        let key = match self.aim {
            Some((col, row))
                if !driven
                    && unmoved
                    && (Self::glide_px_left(col - self.col, self.last_cell_w)
                        || (row - self.row).abs() >= 0.5) =>
            {
                let cw = if self.last_cell_w > 1 {
                    self.last_cell_w as f32
                } else {
                    1.0
                };
                Some([
                    (self.col * cw).round() as i32,
                    self.row.round() as i32,
                    (col * cw).round() as i32,
                    row.round() as i32,
                ])
            }
            _ => None,
        };
        let owed = match (key, self.set_off_asked) {
            (Some(a), Some(b)) => a[0] != b[0] || a[1] != b[1] || a[2] != b[2] || a[3] != b[3],
            (Some(_), None) => true,
            (None, _) => false,
        };
        self.set_off_owed = owed;
        self.set_off_asked = key;
    }

    #[inline]
    fn advance(&mut self, sense: PetSense) -> PetFrame {
        #[cfg(test)]
        {
            self.hug_armed = false;
        }
        // The station the last tick that measured one measured, what this tick found as it began,
        // and the art's width on the cells the tick before drew
        // ([`Self::keep_place_on_restation`]).
        let seen = self.station_seen;
        let entry = TickEntry {
            at: (self.col, self.row),
            landing: self.flight.map(|f| (f.to_col, f.to_row)),
            bound2: self.bound2.map(|(c, r, _)| (c, r)),
        };
        let drawn = art_cols(self.last_cell_w, self.last_cell_h);
        let elapsed = match self.last_now {
            Some(prev) => sense
                .now
                .saturating_duration_since(prev)
                .as_secs_f32()
                .clamp(0.0, 5.0),
            None => 0.0,
        };
        let dt = elapsed.min(STEP_DT_MAX.as_secs_f32());
        let move_dt = if self.asked_frames { dt } else { 0.0 };
        let chrome_hop = core::mem::replace(&mut self.chrome_hop, false);
        self.frame_owed = false;
        self.action_began = false;
        self.setting_off = None;
        self.keep_rest(elapsed);
        self.last_now = Some(sense.now);
        let airborne_at_entry = self.flight.is_some();
        // The span this tick closes came before anything it brings, on every path the tick
        // takes: aged after a move, a run's first key, met by a host that had slept, began as
        // long before the key as the sleep, and the run's beat, pace and lead ran from there;
        // aged only on some paths (a reduced-motion tick, the hidden caret's), a run's window
        // stood still, and a carrying run asked for frames for good ([`Self::needs_frames`]).
        self.age_gesture_windows(elapsed);
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
        // So is the action's clock, for the action that stood when the span began. Aged after
        // the caret's move had read it ([`Self::at_rest`]), a stand that a host that slept met
        // with a jump was younger than on a host that ticked all along: one host's cat perked at
        // the jump and the other's crouched.
        if !self.action_clock_held() {
            self.action_t += elapsed;
        }
        self.arrive_t = (self.arrive_t - move_dt).max(0.0);

        let width = art_cols(sense.cell_w, sense.cell_h);
        const EDGE_FADE_DUR: Duration =
            Duration::from_nanos((EDGE_FADE as f64 * 1_000_000_000.0).round() as u64);
        let resight = match self.resight.take() {
            Some(pending) if sense.now.saturating_duration_since(pending.at) <= EDGE_FADE_DUR => {
                Some(pending)
            }
            _ => None,
        };
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
        if !regrid_carried {
            self.ride_the_page(sense.rows, width);
        }
        if !regrid_carried
            && !sense.output_burst
            && self.exec_since.is_none()
            && !self.console.resident
            && let (Some(prev), Some(cur)) = (self.last_caret, sense.caret)
            && prev != cur
        {
            self.rearm_idle_ears();
        }
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
        if let Some(frame) = self.tick_console_resident(sense, width, (dt, move_dt)) {
            return frame;
        }
        let caret = sense.caret;
        let (sense, seat_back) = self.unseated_sense(sense, width);
        self.cut_on_the_letters(caret, &sense, width, move_dt);
        let was_invisible = self.alpha <= 0.0;
        let Some((cr, cc)) = sense.caret else {
            self.stroke.sample(sense.now, None, width);
            if let Some(flight) = self.flight.take() {
                let u = (flight.t / flight.dur).clamp(0.0, 1.0);
                let lift = Self::flight_lift(flight.arc, u, flight.big);
                self.retired_flight_lift = if lift > 0.0 {
                    Some((lift, lift * (1.0 / FADE_IN)))
                } else {
                    None
                };
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
                && self.all_motes_none()
                && self.all_departures_none()
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
                    stretch_from: StretchFrom::Centre,
                    purr: 0.0,
                    contented: false,
                    under_ink: self.hiding,
                    motes: [None; PET_MOTES_MAX],
                    departures: [None; PET_DEPARTURES_MAX],
                };
            }
            self.alpha = (self.alpha - move_dt * (1.0 / FADE_OUT)).max(0.0);
            if self.alpha == 0.0 {
                self.colors = None;
                self.quiet_leap = false;
                self.stream = false;
                self.stream_run = 0;
                self.jump_at = None;
                self.burst_from = None;
                self.jump_gap = 0.0;
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
            self.enter_settled();
            return self.emit(sense, width);
        };
        let resumed_hidden_flight = self.deferred_hidden_landing.take().is_some();
        if !resumed_hidden_flight && let Some((lift, fall)) = self.retired_flight_lift.as_mut() {
            *lift = (*lift - *fall * move_dt).max(0.0);
            if *lift == 0.0 {
                self.retired_flight_lift = None;
            }
        }
        if was_invisible {
            self.colors = None;
        }
        if self.room_cheer && self.room_cheer_leaving {
            self.alpha = (self.alpha - move_dt * (1.0 / FADE_OUT)).max(0.0);
            if self.alpha == 0.0 {
                let (col, row) = self.station_safe((cr, cc), sense.cols, sense.rows, width);
                self.col = col;
                self.row = row;
                self.home = col;
                self.col_at_tick = col;
                self.drop_room_cheer();
            }
        } else {
            self.alpha = (self.alpha + move_dt * (1.0 / FADE_IN)).min(1.0);
        }

        let prev = self.last_caret;
        if prev.is_none() {
            // A caret seen again where it was keeps the stand its cat held:
            // after a one-frame hide, or on a surface the focus comes back to
            // (`held_stash`). Anywhere else the stand starts afresh.
            if self.held_for != Some((cr, cc)) {
                let surface = self.surface;
                let mut lead = 0.0;
                let mut si = 0;
                while si < self.held_stash.len() {
                    if let Some(s) = self.held_stash[si]
                        && s.surface == surface
                        && s.caret == (cr, cc)
                    {
                        self.held_stash[si] = None;
                        lead = s.lead;
                        break;
                    }
                    si += 1;
                }
                self.held_lead = lead;
            }
            if was_invisible {
                self.wall_side =
                    f32::from(cc) + STATION_LEAD + self.standoff() + width > f32::from(sense.cols);
                let (c, r) = match seat_back {
                    Some(seat) => seat,
                    None => self.station_safe((cr, cc), sense.cols, sense.rows, width),
                };
                self.col = c;
                self.row = r;
                if !sense.reduced_motion
                    && let Some(pending) = resight
                {
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

        let moved = matches!(prev, Some((pr, pc)) if pr != cr || pc != cc);
        if let Some((pr, pc)) = prev
            && moved
        {
            self.line_returned = cc == 0
                && pc > 0
                && cr == pr.saturating_add(1)
                && !sense.wrapped
                && !self.console_output_ends_on(pr);
        }
        let mut move_dc = 0.0f32;
        let mut move_dr = 0.0f32;
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
            // THE CAT RIDES ITS BOX: a program that moves the caret down with the rows round
            // the cat (a transcript growing over the composer pushes the composer down) carries
            // the cat down with them, in the frame they move, as a host's regrid does
            // ([`Self::carry_with_regrid`]): it sits on what it sat on. Hopped after the
            // prompt instead, the session's first bound was followed by a second one as
            // Claude Code's first turn pushed its composer down two rows under the cat.
            // So does a cat at rest on the seat the console keeps beside a composer whose own
            // rows change as it moves (its footer's words), when the caret's new stand puts
            // that seat the same rows down ([`Self::seat_rides_with`]).
            if dr >= 1.0 && !folded && self.flight.is_none() && !was_invisible && !self.hand_moved()
            {
                let content_rides = self.content_ride(self.console_body(sense)) == Some(dr);
                let seat_ride = if content_rides {
                    None
                } else {
                    match self.seat_ride(dr) {
                        Some(seat) => Some((seat, self.console.rest_stand.get())),
                        None => None,
                    }
                };
                if (content_rides || seat_ride.is_some()) && self.carry_with_regrid(dr, sense.rows)
                {
                    if let Some((seat, stand)) = seat_ride {
                        self.console.rest_seat.set(Some(seat));
                        self.console.rest_stand.set(stand);
                    }
                    if let Some((r, c)) = self.held_for {
                        self.held_for = Some((r.saturating_add(dr as u16), c));
                    }
                    self.note_content_ride();
                }
            }
            // The settling caret's sensor ([`SETTLE`]): a jump the hand did not make joins
            // the burst of the one before it when it came within [`SETTLE`]; the hand's own
            // jump ends any burst.
            if dr != 0.0 || dc.abs() >= POUNCE_JUMP {
                // A repaint that armed the stream watch and has settled is over before the
                // next jump is counted: met in the same tick as the settle, the jump kept
                // the watch up, and its eviction took the cat along the words it was on
                // instead of home.
                if self.stream && self.repaint_settled() {
                    self.stand_stream_down();
                }
                if self.hand_moved() {
                    self.jump_at = None;
                    self.burst_from = None;
                    self.jump_gap = 0.0;
                    self.written_through = None;
                } else {
                    let through = dr == 0.0
                        && dc >= POUNCE_JUMP
                        && !self.paste_holding()
                        && self.flight.is_none()
                        && (self.row - f32::from(cr)).abs() < 0.5
                        && f32::from(pcell.1) < self.col + width
                        && f32::from(cc) > self.col;
                    if through {
                        self.written_through = Some(self.clock);
                    }
                    // A paste's jump opens a burst of its own: the program has the caret
                    // until it settles ([`PASTE_SETTLE`]).
                    let joins = self.jumped_within(self.settle())
                        || (self.paste_holding() && self.burst_from.is_some());
                    self.jump_gap = match self.jump_at {
                        Some(at) if joins => (self.clock - at) as f32,
                        _ => 0.0,
                    };
                    self.burst_from = if joins {
                        match self.burst_from {
                            Some(b) => Some(b),
                            None => self.jump_at,
                        }
                    } else if self.paste_holding() {
                        Some(self.clock)
                    } else {
                        None
                    };
                    self.jump_at = Some(self.clock);
                    self.lone_along = dr == 0.0 && self.burst_from.is_none();
                }
            }
            if dr != 0.0 || folded || dc.abs() >= stream_jump(sense.cols) {
                self.stream_run = if (self.clock - self.stream_last) as f32 <= STREAM_WINDOW {
                    self.stream_run.saturating_add(1)
                } else {
                    1
                };
                self.stream_last = self.clock;
                if self.stream_run >= STREAM_RUN && !self.stream {
                    let caret_coil = self.caret_coiled();
                    self.stream = true;
                    self.pending_pounce = false;
                    self.pending_big_jump = false;
                    self.quiet_leap = false;
                    self.clear_play_intents();
                    let coiled = self.caret_coiled();
                    // The caret's own coil, held for a program's burst
                    // ([`Self::coil_waits`]), stays gathered: the burst is a repaint
                    // until it outlasts [`SETTLE_CAP`], and the coil hands itself to the
                    // watch then. Stood up to watch at once, the cat crouched, perked,
                    // sat and sprang from the sit at Claude Code's turn end: four poses
                    // for its one bound. A play's crouch stands up, as it always has.
                    if !(caret_coil && self.caret_settling()) {
                        self.hop_crouch = false;
                        self.big_gather = false;
                        if self.action.settled() || coiled {
                            self.set_action(PetAction::Perk);
                        }
                    }
                }
            }
            if !self.stream {
                self.quiet_leap |= folded;
            }
            let was_asleep = self.action == PetAction::Sleep;
            let wrapped_on = (folded && dc > 0.0)
                || (cr >= pcell.0
                    && cr <= pcell.0.saturating_add(1)
                    && f32::from(cc) - f32::from(pcell.1) <= -0.5 * f32::from(sense.cols));
            // The writing wrapped the line ([`Self::writing_wrapped`]): the caret it walked
            // along the line to the margin, a key at a time (the hand's, or a program's typing
            // into a composer), went back to a line's start; read before this move ends the
            // writing's run ([`Self::on_move`]). An Enter is a new line, not a wrap; a change
            // of row or a jump back after it is no longer the wrap's. A step on along the new
            // line is the wrap's own: a composer's word wrap brings the word it broke down
            // with the caret, often a present after the caret went to the line's start (8
            // cells on in the long insert); read as another jump, a present that showed it
            // before the cat sprang sent the bound across at a quiet crossing's pace, 0.95 s
            // in the air for 101 cells in 60 of 200 jittered takes. A wrap the hand's own key
            // made is the writing's too, its rhythm open or not: typed on after a pause at the
            // margin, two or three keys short of the rhythm's opening, the wrap read as a
            // surprise, and the cat perked (0.43 s) and crossed at the quiet pace (1.2 s). A
            // cat in the air as the wrap comes still has the crossing to make once it is down:
            // a hop that came down as the wrap came sent it across at the quiet pace.
            if wrapped_on && f32::from(pcell.1) + POUNCE_JUMP >= f32::from(sense.cols) {
                let echo_key = self.key_within_dur(HAND_ECHO_DUR);
                if (self.typing_on() || echo_key == Some(PetInputKind::Text))
                    && echo_key != Some(PetInputKind::Submit)
                {
                    self.writing_wrap = Some(self.clock);
                } else if dr != 0.0 || dc <= -POUNCE_JUMP {
                    self.writing_wrap = None;
                }
            } else if dr != 0.0 || dc <= -POUNCE_JUMP {
                self.writing_wrap = None;
            }
            self.on_move(dr, dc, f32::from(cc), sense.cols, sense.reduced_motion);
            if folded && pcell.0 > cr && dc < 0.0 && self.run_dir < 0 && self.rhythm_open() {
                self.wrap_back = true;
            } else if dc > 0.0 || dr != 0.0 {
                self.wrap_back = false;
            }
            if wrapped_on || dr != 0.0 || dc.abs() >= POUNCE_JUMP {
                self.wrap_ahead = None;
            }
            if folded {
                self.key_pace = 0.0;
            }
            if was_asleep && self.action == PetAction::Waking {
                self.spawn_wake_pop(width);
            }
            self.quiet = 0.0;
            self.woke_seat = false;
            self.moved_at = Some(self.clock);
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
            let (col, row) = match self.console_station(base, (sense.rows, sense.cols), width) {
                Some(s) => s,
                None => base,
            };
            self.col = col;
            self.row = row;
            self.home = col;
            self.col_at_tick = col;
            self.flight = None;
            self.retired_flight_lift = None;
            self.quiet_leap = false;
            self.stream = false;
            self.stream_run = 0;
            self.jump_at = None;
            self.burst_from = None;
            self.jump_gap = 0.0;
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

        self.hold_lead();
        if moved && self.rhythm_open() {
            self.rhythm_at = Some(self.clock);
        }

        if self.action_began && !self.action_clock_held() {
            // An action this tick began wears none of the sleep it ended.
            self.action_t += move_dt;
        }
        self.gait_t += elapsed;
        if matches!(self.action, PetAction::Leap | PetAction::Land) {
            self.gait_pace = 0.0;
            self.gait_rest = GAIT_DWELL;
            self.running = false;
            self.lateral = false;
        }
        self.land_t = (self.land_t - move_dt).max(0.0);
        self.flinch_t = (self.flinch_t - move_dt).max(0.0);
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
            self.coil_waited = false;
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
            && self.pending_cheer.is_none()
        {
            self.pending_cheer = Some(big);
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
        self.pet_hold_t = (self.pet_hold_t - move_dt).max(0.0);
        if self.song_left > 0 {
            self.song_next = (self.song_next - move_dt).max(0.0);
            if self.song_next <= 0.0 {
                self.song_left -= 1;
                self.song_next = SONG_BEAT;
                self.mail_beat(width);
            }
        }
        #[inline]
        fn expired(now: Instant, at: Option<Instant>, ttl: Duration) -> bool {
            match at {
                Some(at) => now.saturating_duration_since(at) > ttl,
                None => false,
            }
        }
        if expired(sense.now, self.pending_bell, BELL_LATCH_TTL_DUR) {
            self.pending_bell = None;
        }
        const PET_LATCH_TTL_DUR: Duration = Duration::from_secs(PET_LATCH_TTL as u64);
        if let Some(at) = self.pet_at
            && sense.now.saturating_duration_since(at) > PET_LATCH_TTL_DUR
        {
            self.pending_pet = 0;
            self.pet_at = None;
        }
        self.sweep_trick_latch(sense.now);
        if expired(sense.now, self.pending_flow, FLOW_LATCH_TTL_DUR) {
            self.pending_flow = None;
        }
        if expired(sense.now, self.pending_perk, V2_PERK_LATE_DUR) {
            self.pending_perk = None;
        }
        if let Some(star) = self.pending_catch
            && expired(sense.now, Some(star.born), V2_CATCH_TTL_DUR)
        {
            self.pending_catch = None;
        }
        if expired(sense.now, self.room_at, ROOM_CUE_TTL_DUR) {
            self.pending_room = None;
            self.room_at = None;
        }
        self.catch_cool = (self.catch_cool - elapsed).max(0.0);
        if let Some(at) = self.bat_at
            && sense.now.saturating_duration_since(at) > BAT_TTL_DUR
        {
            self.pending_bat = None;
            self.bat_at = None;
        }
        if let Some(at) = self.look_at
            && sense.now.saturating_duration_since(at) > LOOK_TTL_DUR
        {
            self.pending_look = None;
            self.look_at = None;
        }
        // A repaint is no stream: a burst that armed the watch and settled inside
        // [`SETTLE_CAP`] ([`Self::repaint_settled`]) stands it down as it settles. Watched to
        // its lapse, Claude Code's repaints (its first screens, a turn's end) kept the cat
        // off the caret for most of a second, and the live-edge hug sent it to the
        // spinner's row and back.
        if self.stream
            && ((self.clock - self.stream_last) as f32 > STREAM_LAPSE || self.repaint_settled())
        {
            self.stand_stream_down();
        }
        self.watch_heat = if sense.output_burst {
            self.frame_owed |= move_dt == 0.0 && self.watch_heat < 1.0;
            (self.watch_heat + move_dt * WATCH_RISE).min(1.0)
        } else if self.room_quiet {
            self.watch_heat
        } else {
            (self.watch_heat - move_dt * WATCH_FALL).max(0.0)
        };
        if self.watch_heat < WATCH_GATE {
            self.watch_t = 0.0;
            self.watch_spent = false;
        }
        self.vigil_t = match self.exec_since {
            Some(at) if self.vigil_armed => sense.now.saturating_duration_since(at).as_secs_f32(),
            _ => 0.0,
        };
        self.twitch_t = (self.twitch_t - move_dt).max(0.0);
        self.stumble_t = (self.stumble_t - move_dt).max(0.0);
        self.bored_cool = (self.bored_cool - elapsed).max(0.0);
        let can_twitch = self.watch_heat < WATCH_GATE
            && self.action.settled()
            && !matches!(self.action, PetAction::Sleep | PetAction::Waking);
        self.note_burst_twitch(sense.output_burst, can_twitch);
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
                    let k = 1.0 - crate::color_math::exp_f32(-dt * (1.0 / DASH_WINDOW));
                    let inv_dt = 1.0 / dt;
                    let vx_inst = (px - lx) * inv_dt;
                    let vy_inst = (py - ly) * inv_dt;
                    self.pointer_vx += (vx_inst - self.pointer_vx) * k;
                    self.pointer_vy += (vy_inst - self.pointer_vy) * k;
                    let inst = if vx_inst == 0.0 && vy_inst == 0.0 {
                        0.0
                    } else {
                        crate::color_math::hypot_f32(vx_inst, vy_inst)
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
                self.pointer_heat = if speed >= GAZE_FOLLOW_SPEED_MIN {
                    self.frame_owed |= move_dt == 0.0 && self.pointer_heat < 1.0;
                    (self.pointer_heat + move_dt * POINTER_RISE).min(1.0)
                } else {
                    (self.pointer_heat - move_dt * POINTER_FALL).max(0.0)
                };
                if self.pointer_heat < POUNCE_REARM {
                    // Distinct branch.
                    self.pointer_armed = true;
                }
                self.dash_t = if speed >= DASH_SPEED {
                    self.dash_t + move_dt
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
                let dist = range.max((py - self.row).abs() * 2.0);
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
                        let t = t + move_dt;
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

        // The margin keeps the wall side while the hand writes into it, but not where the wall
        // the body stands at on the caret's row meets the caret ([`Self::wall_meets_the_caret`])
        // in the row's last cell, where the caret stays until the next key wraps it: with no seat
        // off the line ([`Self::wrap_ahead_aim`]), the stand is the station on the caret's line
        // side. Short of that cell the next keys take the caret on through the wall's cells
        // ([`OldPath::TurnAtTheLastCell`]). A body on another row keeps it: on top of a
        // composer's upper rule, escorting a draft into the margin under it, the turn sent it on
        // a flight to the caret's other side before the wrap.
        if self.flight.is_none()
            && self.wrap_ahead.is_none()
            && !(self.writing_on()
                && self.at_the_margin((cr, cc), sense.cols, width, 0.0)
                && !((self.row - f32::from(cr)).abs() < 0.5
                    && !self.on_the_beat()
                    && (cc.saturating_add(1) >= sense.cols
                        || self.old_path(OldPath::TurnAtTheLastCell))
                    && self.wall_meets_the_caret(
                        (cr, cc),
                        (sense.rows, sense.cols),
                        (f32::from(sense.cols) - width).max(0.0),
                        width,
                    )))
            && !(self.run_dir < 0 && self.rhythm_open())
            && self.update_wall(cc, sense.cols, width)
        {
            self.pending_wall_transit = true;
        }
        let (target, target_row) = self.station_safe((cr, cc), sense.cols, sense.rows, width);
        self.walk_to = Some((target, target_row));
        // Where a program's writing along the caret's line laid its letters under the cat
        // ([`Self::writing_laid_under`]): the ground across the composer's rule it goes to
        // ([`Self::seat_off_the_writing`]).
        let off_writing = if self.writing_laid_under((cr, cc), width) {
            self.seat_off_the_writing((cr, cc), (sense.rows, sense.cols), width)
        } else {
            None
        };
        // Chrome the host reserved over the resting body since it came to rest (the harness's
        // lockup laid over the composer's rule once aterm has told the session is Claude
        // Code's): the cat hops off it to its station, as off ink. Left to the host, the body
        // was moved off the chrome in one frame, and leapt again from where it was put. Not
        // where the rows it rests with moved with the chrome (a composer that grows a row up
        // under the cat on top of its rule): the host carries the cat with them.
        let may_evict_action = matches!(
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
        );
        let may_leave = self.flight.is_none()
            && (may_evict_action || self.action == PetAction::Land)
            && !self.cheering()
            && self.wrap_ahead.is_none();
        // A body the host carries with the rows it rests on (a composer that grows a row up
        // under the cat on top of its rule) is never hopped off them
        // ([`OldPath::CarriedBodyHopsOff`]): the rows' move puts the obstruction under it for
        // the one present before the carry. Hopped off, the cat on top of the harness's line
        // leapt a row up at each line a program's writing grew the composer by: four hops on the
        // spot in the long insert's dictation (r29 K9-3).
        let carried = may_leave && self.console_carries_at((self.col, self.row), sense);
        let chrome_over =
            may_leave && self.console_yields_at((self.col, self.row), sense) && !carried;
        // A pounce the caret's jump owes waits out its gather where the cat is, but not under the
        // host's chrome, which the host would step the body off in one present: the hop off is
        // the trip ([`OldPath::ChromeOverIsHoppedOff`]). Waited out, a cat by Claude Code's
        // composer as the program cleared its draft on exit sat a perk under the rule's chrome,
        // and the host put it a row down in one frame.
        let pounce_waits = self.action.settled()
            && (self.pending_pounce || self.pending_big_jump)
            && (!chrome_over || self.old_path(OldPath::ChromeOverIsHoppedOff));
        if may_leave && !pounce_waits && {
            let obstructed = self.console_obstructed(sense)
                && (!carried || self.old_path(OldPath::CarriedBodyHopsOff));
            (may_evict_action || chrome_over)
                && if chrome_over || obstructed {
                    true
                } else {
                    // Nor off the words while a program's burst still walks the caret about
                    // ([`SETTLE`]): the one trip to where it settles is the eviction. Stepped off
                    // the words its repaint wrote under the cat, the cat was launched at a row the
                    // caret was leaving. The host's chrome over it is never waited out: the console
                    // would step the body aside in one present. Nor are the letters a program
                    // writes along the caret's line under the cat ([`Self::writing_laid_under`]):
                    // its writing goes on there, and the cat goes once to where it will rest, off
                    // the writing's way.
                    (!self.caret_unsettled() || off_writing.is_some())
                        && (self.ink_overlaps(self.col, self.row, width)
                            || (was_evicting
                                && !self.stream
                                && (self.col - target).abs() > f32::EPSILON
                                && (self.row - f32::from(cr)).abs() < 0.5
                                && self.col < f32::from(cc) + STATION_LEAD
                                && self.col + width > f32::from(cc) - STATION_LEAD))
                }
        } {
            if chrome_over {
                self.pending_pounce = false;
                self.pending_big_jump = false;
            }
            let watching = self.action == PetAction::Perk
                && self.watch_heat >= WATCH_GATE
                && !self.watch_spent;
            let (safe, safe_row) = if watching {
                (
                    self.ink_safe_col(self.col, self.row, width, sense.cols),
                    self.row,
                )
            } else if let Some(seat) = off_writing {
                // THE WRITING'S LETTERS SEND THE CAT ONCE ACROSS ITS RULE
                // ([`OldPath::WritingEvictsAlongItsRow`]): a program writing along the caret's
                // line (a harness typing a prompt into Claude Code's composer, chunk by chunk)
                // laid its letters under the cat on that line. It goes once to ground the
                // writing never reaches ([`Self::seat_off_the_writing`]). Stepped off along its
                // row, it stood in the way of the next chunk, and hopped again under each.
                seat
            } else if self.stream && !self.caret_rested_after_jump() {
                let stand = self.ink_stand(self.col, self.row, width, sense.cols, sense.rows);
                self.console_station(stand, (sense.rows, sense.cols), width)
                    .unwrap_or(stand)
            } else {
                // The caret a program jumped has rested ([`SETTLE`]) while the watch is still up:
                // the trip off the words is the one to where it rested. Stepped along the
                // words' row instead, the long insert's cat went off the dictation's letters to
                // the far margin, and on to the seat under the caret after.
                (target, target_row)
            };
            let moved_off = (safe - self.col).abs() > f32::EPSILON;
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
                self.col = Self::evict_toward(self.col, safe, move_dt);
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
            // A coil that waited for a settling caret is the caret's
            // ([`Self::coil_waits`]): taken by the hug, it left for the
            // newest line a repaint was still writing and came down a row
            // off the caret, to leap again when the watch lapsed.
            && !(self.coil_waited && self.action == PetAction::Crouch)
            // Nor while a program's burst still walks the caret about
            // ([`SETTLE`]): a repaint that settles stands the watch down
            // and the cat goes home once; one that runs on past
            // [`SETTLE_CAP`] is a stream, and its live edge is hugged then.
            // Hugged at once, the newest row Claude Code's repaint was still
            // writing (its spinner's, its transcript's last) took the cat
            // there and back.
            && !self.caret_settling()
            && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
            && let Some(live) = self.ink_live_row()
        {
            #[cfg(test)]
            {
                self.hug_armed = true;
            }
            let col = match self.ink_span(live) {
                Some((_, end)) => {
                    (end + STATION_LEAD).clamp(0.0, (f32::from(sense.cols) - width).max(0.0))
                }
                None => self.col,
            };
            if (live - self.row).abs() >= WATCH_HUG_ROWS
                && !self.console_bars((col, live), sense.rows, width)
            {
                hugging = true;
                self.quiet_leap = true;
                (col, live)
            } else if (live - self.row).abs() >= WATCH_HUG_ROWS
                || self.held_on_the_caret((cr, cc), sense.cols, width)
            {
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
        let (target, target_row) = if self.stream
            && self.flight.is_none()
            && !hugging
            && !watch_far
            && !self.held_on_the_caret((cr, cc), sense.cols, width)
        {
            (self.col, self.row)
        } else {
            (target, target_row)
        };
        // THE WRITING'S WAY IS NO PLACE TO WATCH IT FROM ([`OldPath::WritingEvictsAlongItsRow`]):
        // a program writing on along the caret's line, a rule under it, is watched from where the
        // cat is when that is off the line, or from across the rule
        // ([`Self::watch_off_the_writing`]), never from the stand beside the caret or the line's
        // live edge it would lay its next letters over. Sent to the stand by the caret (the
        // stream watch lets a cat go to a stand more than [`WATCH_HUG_ROWS`] off), a cat on top
        // of the harness's line hopped down onto the line a harness was typing into, and off it
        // again when the next letters came.
        let (target, target_row) =
            match self.watch_off_the_writing((cr, cc), (sense.rows, sense.cols), width) {
                Some(seat) => {
                    hugging = false;
                    seat
                }
                None => (target, target_row),
            };

        if self.flight.is_none()
            && !self.still_hold_fresh()
            && let Some((col, row, dur, arc, kind)) = self.deferred_arc
        {
            self.deferred_arc = None;
            self.still_since = None;
            self.begin_arc(col, row, dur, arc, kind);
            return self.emit(sense, width);
        }

        // A cat the head law seated under the caret's line, beside a composer's chrome
        // ([`Self::clear_of_letters_above`]), waits out a wrap there: brought up to the
        // margin on the caret's row for it, it flew up to the line and down again at the wrap,
        // two flights for one. A cat the margin sent to rest two rows under the caret
        // ([`WrapAhead::Rest`], [`Self::margin_rest_seat`]) waits for the wrap already: let go
        // here once it had landed, the margin was lost with the hand stopped, the wall side
        // crossed the stand to the caret's left, and the cat hopped again onto a seat with its
        // head among the line's letters (a pause at the margin of a wrapped line's second row).
        let seated_under = target_row - f32::from(cr) >= 1.5
            && !matches!(self.wrap_ahead, Some(WrapAhead::Rest(..)));
        if seated_under && self.flight.is_none() {
            self.wrap_ahead = None;
        }
        let (target, target_row) = if self.flight.is_none()
            && !self.stream
            && self.play_land.is_none()
            && self.pursuit_t.is_none()
            && !seated_under
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
        self.aim = Some((target, target_row));
        self.keep_place_on_restation(
            (seen, entry, drawn),
            (target, target_row),
            (cr, cc),
            (width, sense.cols),
        );
        self.station_seen = Some(SeenStation {
            at: (target, target_row),
            width,
            caret: (cr, cc),
        });

        if let Some(mut f) = self.flight {
            // THE FIRST KEYS TAKE THE LANDING AHEAD OF THEM: the hand
            // began writing on the row the cat is coming down onto before its
            // rhythm opened ([`RHYTHM_MOVES`]), so no lead aims the landing
            // (the trail gallery's `burst` replayed: the cat flew to the new
            // prompt as `echo` was typed, came down on the stand the caret had
            // when it left and the line's first ten letters were typed under
            // its body before it ran clear). Each key re-aims the landing ahead
            // of where the hand, at its own pace so far ([`Self::first_keys_pace`]),
            // will have taken the caret by the end of its touchdown, and a cell
            // more for a fast hand's run start ([`FIRST_KEYS_ROOM`]). Taken at a
            // fast hand's pace whatever the hand's own, the landing ran away from
            // a slow one: an unhurried key that wrapped the line (its rhythm never
            // opens) sent the wrap's crossing eleven cells past the caret, where
            // the cat stood and waited for the writing (r24 K7-2).
            let first_keys = !self.aims_ahead()
                && move_dr == 0.0
                && (f.to_row - f32::from(cr)).abs() < 0.5
                && !self.old_path(OldPath::FirstKeysAhead)
                && self.letter_written_at_caret((cr, cc), move_dc, sense.cols);
            if move_dc != 0.0
                && move_dc.abs() < POUNCE_JUMP
                && move_dr == 0.0
                && !f.drop
                && !self.stream
                && self.play_land.is_none()
                && (self.aims_ahead() || first_keys)
            {
                let (aim, aim_row) = self.station_safe((cr, cc), sense.cols, sense.rows, width);
                let reach = if self.wall_side {
                    self.station_now(sense.cols.saturating_sub(1), sense.cols, width)
                } else {
                    f32::from(sense.cols) - width
                };
                let u_now = (f.t / f.dur).clamp(0.0, 1.0);
                let cap = Self::flight_cap(&f, sense.cols);
                let hand = self.first_keys_pace();
                let to = if first_keys && self.bound2.is_none() && 0.9 * cap > hand {
                    // The rest of the flight draws out to reach the stand the
                    // first keys run on to, at the animal's pace, and the
                    // landing touches down running ([`Flight::running`]).
                    let room = FIRST_KEYS_ROOM * (hand / FIRST_KEYS_PACE);
                    let left = (f.dur - f.t).max(1e-3);
                    let need = aim + room + hand * TOUCH_LAND_DUR - self.col;
                    let rest = (need / (0.9 * cap - hand)).max(left);
                    if rest > left && u_now <= REAIM_MAX_U {
                        Self::retime(&mut f, rest);
                    }
                    f.running = true;
                    // No farther ahead than a cat waits for the writing
                    // ([`AWAIT_AHEAD`]): a long bound re-aimed at a fast
                    // hand's pace came down sixteen cells ahead of a slow
                    // one's caret and flew back to it (Claude Code's
                    // repaint at 32 s into the generating take).
                    (aim + room + hand * (rest + TOUCH_LAND_DUR))
                        .min(aim + AWAIT_AHEAD - 1.0)
                        .clamp(0.0, reach.max(0.0))
                } else {
                    let t_rem = (f.dur - f.t).max(0.0) + Self::landing_dur(&f);
                    (aim + self.station_drift(t_rem)).clamp(0.0, reach.max(0.0))
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
                    if onward
                        && (aim_row - f.to_row).abs() < 0.5
                        && in_time
                        && u_now <= REAIM_MAX_U
                        && let Some(from) = Self::rebased_from(self.col, to_arc, u_now)
                    {
                        f.from_col = from;
                        f.to_col = to_arc;
                    }
                }
            }
            // THE LANDING GOES WHERE IT RESTS ([`OldPath::LandWhereItRests`]): a flight that left
            // before the caret's jump came to rest ([`Self::caret_rested_after_jump`], [`SETTLE`])
            // aimed at the stand by the caret; once the caret rests, the seat the cat will rest on
            // is known (a stand among letters gives way to the whole body's seat), and a landing
            // still on its first stretch takes it, once, on its row and in the animal's time.
            // Kept on the stand, the cat that flew to a new prompt after an Enter came down
            // fitted beside it and flew again 0.6 s later to the seat past the output's last
            // letter: two flights for one Enter (r24 K7-1).
            if !f.reaimed
                && !f.drop
                && !self.stream
                && self.play_land.is_none()
                && self.bound2.is_none()
                && move_dc == 0.0
                && move_dr == 0.0
                && !self.old_path(OldPath::LandWhereItRests)
                && self.caret_rested_after_jump()
                && matches!(self.jump_at, Some(j) if j + f64::from(self.settle()) > self.clock - f64::from(f.t))
                && (target_row - f.to_row).abs() < 0.5
                && (target - f.to_col).abs() > ARRIVED
            {
                let u_now = (f.t / f.dur).clamp(0.0, 1.0);
                let cap = Self::flight_cap(&f, sense.cols);
                let in_time = (target - self.col).abs() / (f.dur - f.t).max(1e-3) <= cap;
                if u_now <= REAIM_MAX_U
                    && in_time
                    && let Some(from) = Self::rebased_from(self.col, target, u_now)
                {
                    // At the pace it flew: a nearer seat comes sooner, never later.
                    let pace = (f.to_col - f.from_col).abs() / f.dur.max(1e-3);
                    let left = (f.dur - f.t).max(1e-3);
                    let rest = if pace > 0.0 {
                        ((target - self.col).abs() / pace).max(FLIGHT_MIN)
                    } else {
                        left
                    };
                    if rest < left {
                        let k = rest / left;
                        f.t *= k;
                        f.dur *= k;
                    }
                    f.from_col = from;
                    f.to_col = target;
                    f.reaimed = true;
                }
            }
            if airborne_at_entry && (move_dr.abs() >= 0.5 || move_dc.abs() >= POUNCE_JUMP) {
                self.bound2 = None;
            }
            // THE FIRST KEY TAKES THE LANDING: a letter the hand types while the cat is in the
            // air that moves the escort's stand to another row (typing begins under a host's
            // line, whose stand is on top of it) takes the landing there, as the caret's own
            // change of row would. Kept on the rest it was flying to, the cat came down there
            // and flew up again at once: the first key cost a flight.
            let key_station = if airborne_at_entry
                && move_dr == 0.0
                && move_dc != 0.0
                && !f.drop
                && !matches!(self.wrap_ahead, Some(w) if w.off_row())
                && self.key_within_dur(HAND_ECHO_DUR) == Some(PetInputKind::Text)
            {
                let station = self.station_safe((cr, cc), sense.cols, sense.rows, width);
                if (station.1 - f.to_row).abs() >= 0.5 {
                    Some(station)
                } else {
                    None
                }
            } else {
                None
            };
            let key_row = key_station.is_some();
            let row_change = move_dr != 0.0 || key_row;
            if (move_dc.abs() >= POUNCE_JUMP || move_dr != 0.0 || key_row)
                && !f.drop
                && !self.stream
                && self.play_land.is_none()
            {
                // One re-aim a flight, but a caret that changes rows is always followed
                // ([`REAIM_MAX_U`] still bounds it): the session's first bound took a
                // 6-cell shift of the empty prompt as its one re-aim, then came down on
                // the row Claude Code's first turn had moved the prompt off, and leapt
                // again.
                if f.reaimed && !row_change {
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
                    let (aim, aim_row) = key_station.unwrap_or_else(|| {
                        self.station_safe((cr, cc), sense.cols, sense.rows, width)
                    });
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
                    let cap = Self::flight_cap(&f, sense.cols);
                    // A caret that changed rows takes the landing to its new row too, rebased
                    // as the wrap's landing is: a flight that kept the row it left for came
                    // down on a row the caret had left (a program's repaint moving the prompt
                    // down as the cat flew to it), and leapt again from there.
                    // A row change out of reach at the animal's speed in the time the flight
                    // has left draws the rest of the flight out until it is in reach
                    // ([`Self::retime`]): refused, a 150 ms late echo's prompt moved two rows
                    // down under the session's first bound and the cat hopped after it. A
                    // jump along the row out of reach (a stream racing ahead) is not chased.
                    // Never back the way it came: a landing behind the cat in the air turned
                    // the flight round mid-air. It comes down, and goes again.
                    let heading = f.to_col - f.from_col;
                    let turns_back = (to_arc - self.col) * heading.signum() < -ARRIVED
                        && heading.abs() > ARRIVED;
                    let span = span_eq(to_arc - self.col, aim_row - self.row);
                    let mut in_time = span / (f.dur - f.t).max(1e-3) <= cap && !turns_back;
                    let early = !turns_back
                        && if row_change {
                            u_now < ROW_REAIM_MAX_U
                        } else {
                            u_now <= REAIM_MAX_U
                        };
                    // The writing's wrap ([`Self::writing_wrapped`]) out of a hop's reach is
                    // not drawn out across the pane at a hop's pace: the hop comes down on its
                    // own seat and the wrap's brisk bound ([`Self::brisk_shape`]) follows. The
                    // margin's rest hop ([`Self::margin_rest_seat`]) launched as the hand typed
                    // on through the wrap was stretched to the next line's start: 1.3 to 1.55 s
                    // in the air, gliding low over the line it left.
                    let wrap_past_hop = !f.big && row_change && self.writing_wrapped();
                    if !in_time && row_change && early && !wrap_past_hop {
                        Self::retime(&mut f, span / cap);
                        in_time = true;
                    }
                    if in_time && early {
                        self.rebase(&mut f, u_now, (self.col, self.row), (Some(to_arc), aim_row));
                        self.pending_pounce = false;
                        self.pending_big_jump = false;
                        self.pending_wall_transit = false;
                        self.quiet_leap = false;
                    } else if row_change
                        && turns_back
                        && !wrap_past_hop
                        && u_now < ROW_REAIM_MAX_U
                        && !self.ink_overlaps(f.to_col, aim_row, width)
                        && !self.console_yields_at((f.to_col, aim_row), sense)
                    {
                        // The caret's new row is followed even where its station lies behind
                        // the cat (a submit's prompt under the transcript; a stand read off
                        // chrome the host repaints with the composer a present late): the
                        // landing keeps its column and takes the row, and the way along the
                        // row is left for the ground. Kept on the row the caret left, the
                        // bound came down there and hopped after it.
                        let rest = span_eq(f.to_col - self.col, aim_row - self.row) / cap;
                        Self::retime(&mut f, rest);
                        self.rebase(&mut f, u_now, (self.col, self.row), (None, aim_row));
                    }
                    self.bound2 = None;
                }
            }
            // THE LANDING THE CHROME TOOK: chrome the host paints after the cat left (the
            // harness's lockup over the composer's rule, laid once aterm has told the session
            // is Claude Code's) over the ground the flight comes down on takes the landing to
            // the stand clear of it, as a caret move would ([`Self::console_yields_at`]).
            // Landed on it, the host moved the cat off the chrome in one frame and it leapt
            // again: the session's first trip was a bound, a jump on the spot and a hop.
            if airborne_at_entry
                && !f.drop
                && !self.stream
                && self.play_land.is_none()
                && self.bound2.is_none()
                && self.console_yields_at((f.to_col, f.to_row), sense)
            {
                let (aim, aim_row) = self.station_safe((cr, cc), sense.cols, sense.rows, width);
                let t_rem = (f.dur - f.t).max(0.0) + Self::landing_dur(&f);
                let to = (aim + self.station_drift(t_rem))
                    .clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
                let u_now = (f.t / f.dur).clamp(0.0, 1.0);
                let heading = f.to_col - f.from_col;
                let turns_back =
                    (to - self.col) * heading.signum() < -ARRIVED && heading.abs() > ARRIVED;
                if !turns_back
                    && u_now < ROW_REAIM_MAX_U
                    && !self.console_yields_at((to, aim_row), sense)
                {
                    let cap = Self::flight_cap(&f, sense.cols);
                    Self::retime(&mut f, span_eq(to - self.col, aim_row - self.row) / cap);
                    self.rebase(&mut f, u_now, (self.col, self.row), (Some(to), aim_row));
                }
            }
            if airborne_at_entry {
                let step = elapsed.min(FLIGHT_STALL_MAX);
                f.t += step;
            }
            let u = (f.t / f.dur).clamp(0.0, 1.0);
            // On its landing exactly at touchdown: a landing rebased in the air
            // ([`Self::retime`]) is a far-off start, and the blend came down a hair short.
            (self.col, self.row) = if u >= 1.0 {
                (f.to_col, f.to_row)
            } else {
                (
                    f.from_col + (f.to_col - f.from_col) * u,
                    f.from_row + (f.to_row - f.from_row) * u,
                )
            };
            debug_assert!(
                self.col.is_finite() && self.row.is_finite(),
                "a flight drew the animal at ({}, {}): {f:?}",
                self.col,
                self.row
            );
            self.speed = (f.to_col - f.from_col) / f.dur;
            if u >= 1.0 {
                let span = (f.to_col - f.from_col).abs();
                self.content = (self.content + span * CONTENT_PER_CELL).min(1.0);
                self.flight = None;
                self.touched_on_chrome =
                    !f.drop && self.console_yields_at((self.col, self.row), sense);
                self.scrabble = false;
                self.commit_t = 0.0;
                self.land_running = false;
                if f.big && self.bound2.is_some() {
                    self.land_t = TOUCH_LAND_DUR;
                    self.land_span = 0.0;
                } else if f.running
                    || (self.rhythm_open() && !self.stream)
                    || self.writing_ran_past(&f, target, target_row)
                {
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
                    // The slide ends on the stand as it is at touchdown, never past it: a
                    // key typed while the cat was in the air moved its stand back toward
                    // it, and the whole skid slid past and walked back. A flight aimed
                    // elsewhere (a toy, a cheer's stage, the stream watch's trips) slides
                    // as it was aimed.
                    self.skid_len = if self.play_land.is_some() || self.room_cheer || self.stream {
                        SKID_CELLS
                    } else {
                        ((target - self.col) * self.skid_dir).clamp(0.0, SKID_CELLS)
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
                        && (self.stride as u32).wrapping_add(u32::from(self.mote_serial)) & 3 == 0
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
            // A wake's stretch the hop borrowed with less than a still pose's
            // [`STILL_SECS`] left is spent: handed back, it was on glass a single
            // present between the landing and the seat.
            && !(pose == PetAction::Waking && WAKE_DUR - held < STILL_SECS)
        {
            self.set_action(pose);
            self.action_t = held;
            return self.emit(sense, width);
        }

        if self.action == PetAction::Waking && self.pending_drop {
            self.pending_drop = false;
            if (target_row - self.row - 1.0).abs() < 0.5 {
                let to_col = self.ink_safe_col(self.col, target_row, width, sense.cols);
                if (to_col - self.col).abs() < 0.5 && !self.ink_overlaps(to_col, target_row, width)
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
        }

        // The writing at a still cat's heels ends its stare and its landing's recovery
        // ([`Self::writing_reaches`]): it runs clear of the next steps, at the chase's fastest.
        if (move_dc > 0.0 || (self.typing_on() && self.writing_near((cr, cc), width, 1.5)))
            && self.writing_reaches((cr, cc), width)
        {
            self.writing_chase = true;
            self.commit_t = 0.0;
        } else if !self.writing_near((cr, cc), width, 2.0) {
            self.writing_chase = false;
        }
        let reaches = self.writing_chase;
        let target = if reaches {
            target.max(self.writing_clear(cc, sense.cols, width))
        } else {
            target
        };
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
            } else if let Some(big) = self.vigil_cheer.take()
                && self.pending_cheer.is_none()
            {
                self.pending_cheer = Some(big);
            }
        }

        // A drop's sit is spent before the arms are read, so the tick it runs out on reads them
        // as the next one would. Spent in its arm, that tick returned on the seat, and the stand
        // or the walk the sit let go to waited for the next tick, which a host at the brain's
        // offers ran whenever anything else came: the sit's train had ended with it.
        if self.action == PetAction::Sit && self.drop_sit_t > 0.0 && !(self.stream && !hugging) {
            self.drop_sit_t = (self.drop_sit_t - move_dt).max(0.0);
        }
        // LETTERS A PROGRAM WRITES UNDER A GATHER ARE NEVER WAITED OUT
        // ([`OldPath::WritingEvictsAlongItsRow`]): a coil gathered for the caret waits a program's
        // burst out ([`Self::coil_waits`]) so the trip goes once, to where the cat rests; a burst
        // that writes along the caret's line under the gathering cat (a harness typing a prompt
        // into Claude Code's composer) would hold it crouched on the new letters until the burst
        // ran past [`SETTLE_CAP`]. It leaves at once for ground the writing never reaches
        // ([`Self::seat_off_the_writing`]): held, it crouched 0.9 s on the dictation's first
        // letters, and leapt into the writing's way.
        let off_writing = if self.caret_coiled() && self.writing_laid_under((cr, cc), width) {
            self.seat_off_the_writing((cr, cc), (sense.rows, sense.cols), width)
        } else {
            None
        };
        let written_under = off_writing.is_some();
        let (target, target_row) = off_writing.unwrap_or((target, target_row));
        // A cat the caret came into is not held there ([`Self::held_on_the_caret`]).
        let watch_holds =
            self.stream && !hugging && !self.held_on_the_caret((cr, cc), sense.cols, width);
        match self.action {
            PetAction::Sit | PetAction::Stand | PetAction::Perk if watch_holds => {
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
                    self.commit_t = (self.commit_t - move_dt).max(0.0);
                    if self.commit_t <= 0.0 {
                        self.facing_left = target < self.col;
                        self.set_action(PetAction::Perk);
                    }
                    self.speed = 0.0;
                    return self.emit(sense, width);
                }
            }
            PetAction::Sit if self.drop_sit_t > 0.0 => {
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
                self.reads_the_way(target, target_row, dur);
                if self.coil_waits(
                    dur,
                    hugging,
                    self.console_obstructed(sense) || written_under,
                    true,
                ) {
                    return self.emit(sense, width);
                }
                // A gather that waited for a program's jump the caret then took back leaves
                // nowhere: launched, it bounded on the spot. Nor does one whose target came
                // back within a short walk ([`BOUND_SPAN_MIN`]) while it gathered.
                if self.coil_came_home(target, target_row) {
                    self.big_gather = false;
                    return self.emit(sense, width);
                }
                if self.gather_stands_short(target, target_row, sense, width) {
                    return self.emit(sense, width);
                }
                if self.action_t >= dur
                    && !self.gather_owes_veil(dur)
                    && !self.owes_the_way(sense, width)
                {
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
                // Held for a settling caret it is a whole coil, its release
                // drawn on its last ticks as a bound's is.
                let bound =
                    self.predicted_tier(target, target_row, true, sense.cols, width) == Tier::Bound;
                if self.coil_waits(
                    CROUCH_DUR,
                    hugging,
                    self.console_obstructed(sense) || written_under,
                    bound,
                ) {
                    self.coil_d = CROUCH_DUR;
                    return self.emit(sense, width);
                }
                if self.coil_came_home(target, target_row) {
                    return self.emit(sense, width);
                }
                if self.gather_stands_short(target, target_row, sense, width) {
                    return self.emit(sense, width);
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
                self.wriggle_t = (self.wriggle_t - move_dt).max(0.0);
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
                self.stakeout_t += move_dt;
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
                    if let Some((tc, tr)) = self.play_to {
                        self.reads_the_way(tc, tr, CROUCH_DUR);
                    }
                    if self.action_t >= CROUCH_DUR
                        && !self.owes_the_way(sense, width)
                        && let Some((tc, tr)) = self.play_to.take()
                    {
                        // A PLAY COMES DOWN ON A ROW ([`OldPath::PlayLandsOffTheRows`]): its aim
                        // is a point (a word-cat peek's head, the pointer), but the cat stands on
                        // the grid's rows, and the head law that keeps its head off a lettered row
                        // above reads only a body on one ([`Self::head_target`]). Come down where
                        // it aimed, 0.07 of a row above the prompt's row, the cat that batted a
                        // pasted command's word-cat walked back to the prompt there, drawn whole
                        // with its box 6 px into the command's line instead of drawn down by the
                        // caret (windowed zsh, every first paste of a session, r29 K9-5).
                        let tr = if self.old_path(OldPath::PlayLandsOffTheRows) {
                            tr
                        } else {
                            tr.round()
                        };
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
                self.reads_the_way(target, target_row, self.coil_d);
                let vertical = (target_row - self.row).abs() >= 0.5;
                let bound = self.predicted_tier(target, target_row, vertical, sense.cols, width)
                    == Tier::Bound;
                if self.coil_waits(
                    self.coil_d,
                    hugging,
                    self.console_obstructed(sense) || written_under,
                    bound,
                ) || self.coil_came_home(target, target_row)
                    || self.gather_stands_short(target, target_row, sense, width)
                {
                    return self.emit(sense, width);
                }
                if self.action_t >= self.coil_d
                    && !self.gather_owes_veil(self.coil_d)
                    && !self.owes_the_way(sense, width)
                {
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
                if matches!(self.pending_trick, Some(p) if p.trick == trick) {
                    self.pending_trick = None;
                }
                self.speed = 0.0;
                self.perform_trick(
                    trick,
                    cc,
                    width,
                    ((sense.cell_w, sense.cell_h), (sense.cols, sense.rows)),
                );
                if self.trick_settle.is_none() {
                    return self.emit(sense, width);
                }
            }
            PetAction::Land
                if self.land_t > 0.0
                    && !reaches
                    && !(self.wide_chase(target - self.col)
                        && !self.landing_fresh()
                        && (self.row - f32::from(cr)).abs() < 0.5
                        && f32::from(cc) + 1.0 + self.run_step >= self.col) =>
            {
                // Landing recovery. A big landing also skids up to the 0.8 cells
                // the flight aimed short by, ending on the stand as it is at touchdown.
                if self.land_span > 0.0 && self.skid_dir != 0.0 {
                    const INV_SKID_NORM: f32 = 1.0 / (1.0 - crate::color_math::exp_f32(-4.0));
                    let u = (1.0 - self.land_t * (1.0 / BIG_LAND_DUR)).clamp(0.0, 1.0);
                    let s = (1.0 - crate::color_math::exp_f32(-4.0 * u)) * INV_SKID_NORM;
                    self.col = (self.skid_from + self.skid_dir * self.skid_len * s)
                        .clamp(0.0, (f32::from(sense.cols) - width).max(0.0));
                }
                if self.bound2.is_none() {
                    self.plant_stride(move_dt);
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
                    let stage_at = if self.room_cheer_trips < CHEER_STAGE_TRIPS {
                        if let Some(s) = self.cheer_stage(sense, width, self.room_cheer_stage) {
                            self.room_cheer_air = Some(s.air);
                            s.at
                        } else {
                            self.room_cheer_air = None;
                            None
                        }
                    } else {
                        None
                    };
                    if asleep {
                        self.quiet = 0.0;
                        self.spawn_wake_pop(width);
                    }
                    if let Some((col, row)) = stage_at {
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

        // A NEW SCREEN SETTLES BEFORE A CAT AT REST SETS OFF ([`Self::rescreen_settling`]): a
        // program that switched screens is still drawing its first one, and the caret it shows
        // may be a passing one (a pager's exit restores the shell's cursor to its line's start a
        // present before the shell's prompt puts it back). The cat goes once, to where the caret
        // rests.
        if self.action.settled() && self.rescreen_settling() {
            self.speed = 0.0;
            return self.emit(sense, width);
        }
        let gap = target - self.col;
        if self.pending_big_jump && self.bound_came_short(target, target_row) {
            self.pending_big_jump = false;
        }

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
            // A coil a settling caret, or a program's lone jump along the
            // row or after an Enter, will hold ([`Self::coil_waits`],
            // [`Self::lone_settling`], [`Self::row_jump_settling`]) is a whole
            // one from its first tick: dealt a hop's single release frame, the
            // held gather came after it.
            self.coil_d = if self.predicted_tier(target, target_row, true, sense.cols, width)
                == Tier::Hop
                && !self.caret_settling()
                && !self.lone_settling()
                && !(self.last_submit_seq().is_some() && self.row_jump_settling())
            {
                0.0
            } else {
                CROUCH_DUR
            };
            self.set_action(PetAction::Crouch);
            return self.emit(sense, width);
        }

        if self.pending_wall_transit || chrome_hop {
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

        let sched = ((self.vhat.abs() - RUN_SPEED) * (1.0 / RUN_SPEED)).clamp(0.0, 1.0);
        let wide_chase = self.wide_chase(gap) || reaches;
        let gain = if wide_chase {
            CHASE_GAIN_FAST
        } else {
            CHASE_GAIN + (CHASE_GAIN_FAST - CHASE_GAIN) * sched
        };
        let standoff = self.standoff();
        let base_ahead = if self.wall_side {
            0.0
        } else {
            f32::from(cc) + STATION_LEAD + self.wide_step()
        };
        let raw_station = if self.wall_side {
            0.0
        } else {
            base_ahead + self.lead() + standoff
        };
        let limit = f32::from(sense.cols) - width;
        let station_now = if limit <= 0.0 {
            0.0
        } else if self.wall_side {
            (f32::from(cc) - width - STATION_LEAD - standoff).max(0.0)
        } else {
            raw_station.clamp(0.0, limit)
        };
        let on_station = (target - station_now).abs() <= 1e-3;
        let carrying_run = self.carrying_run();
        let walled = !self.wall_side && raw_station >= limit;
        let station_v = self.station_velocity();
        // A cat the writing reaches runs to a cell clear of it ([`Self::writing_clear`]) on the
        // station's own pace as well: its target off the station, the station's velocity fell
        // out of its run, and the caret's next wide step caught the tail it had cleared.
        let feed = if gap * station_v > 0.0 && (on_station || reaches) && !walled {
            station_v
        } else {
            0.0
        };
        let riding = on_station && !walled && carrying_run;
        let rest = if on_station && !self.wall_side && carrying_run {
            (base_ahead + self.lead_at(f32::INFINITY) + standoff).clamp(0.0, limit.max(0.0))
        } else {
            target
        };
        let past_station = riding && gap < 0.0 && rest > self.col;
        let top = if wide_chase {
            TOP_SPEED_MAX
        } else {
            self.top_speed()
        };
        let writing_on = match self.key_within_dur(SIT_AFTER_DUR) {
            Some(kind) => kind == PetInputKind::Text,
            None => self.last_dir > 0 && self.run_t < SIT_AFTER,
        };
        // THE WRITING BRINGS THE STAND: a cat ahead of its stand in the writing's way, the stand
        // beside the caret or one the caret's line holds still behind the cat (past the footer's
        // words), waits for the writing rather than go back the way it comes.
        let writing_comes = !past_station
            && !self.wall_side
            && gap < 0.0
            && gap > -AWAIT_AHEAD
            && (on_station || f32::from(cc) < self.col)
            && (target_row - self.row).abs() < 0.5
            && writing_on
            && self.pursuit_t.is_none()
            && self.play_land.is_none()
            && !self.stream;
        let awaits_writing = writing_comes && gap < -ARRIVED;
        if awaits_writing {
            // The wait ends with the key's [`SIT_AFTER_DUR`], or after it with the run's
            // [`SIT_AFTER`]: the earlier edge is offered, and the tick there finds the next.
            let run_left = if self.last_dir > 0 && self.run_t < SIT_AFTER {
                SIT_AFTER - self.run_t
            } else {
                f32::INFINITY
            };
            let key_left = self.typed_left_dur(SIT_AFTER_DUR);
            let left = key_left.map_or(run_left, |left| left.as_secs_f32().min(run_left));
            self.writing_wait = left.is_finite().then_some(left);
        }
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
        // The front seat holds a body on it, beside a home it is clear of: the seat a seated
        // front-facing cat steps out to from home (below), whose step lands on it exactly
        // ([`Self::evict_toward`]). A glide that passes it, coming home from ahead as a run's
        // lead lets go, goes on home. Held anywhere at or past the seat, that glide stopped
        // wherever the hold first met it, up to [`ARRIVED`] past home; held within a hair of
        // it, on the schedules whose step happened to fall in the hair.
        let seat = (f32::from(cc) + STATION_LEAD + PECK_CLEAR)
            .min((f32::from(sense.cols) - width).max(0.0));
        let seat_kept = at_rest
            && self.sit_front
            && self.facing_left
            && self.pointer_heat <= 0.0
            && gap.abs() <= ARRIVED
            && gap <= 1e-4
            && self.col == seat;
        let stand_held = self.stand_held() && !(at_rest && gap.abs() <= ARRIVED && !riding);
        let from_rest = self.speed == 0.0;
        if stand_held || seat_kept || (self.room_cue_act && at_rest && gap.abs() <= ARRIVED) {
            self.speed = 0.0;
        } else if at_rest && gap.abs() <= ARRIVED && dt > 0.0 && !riding {
            // A whole pixel or more from home the body glides there, easing in, from the frame
            // after a tick that ends a sleep ([`Self::frame_owed`]); under one, the arrival
            // below lands it home ([`Self::glide_px_left`]). Not back the way the writing comes
            // (`writing_comes`): eased back half a cell after a landing aimed ahead of a stand
            // that could not move (pinned past the footer's words), it walked on with the caret
            // a second later, a turn-back no caret move made (the long insert's wrap, 29.9 s).
            self.speed = 0.0;
            if !writing_comes
                && gap != 0.0
                && (sense.cell_w == 0 || Self::glide_px_left(gap, sense.cell_w))
            {
                if move_dt > 0.0 {
                    let rate = gain * SETTLE_STEP_FACTOR;
                    self.speed =
                        gap * (1.0 - crate::color_math::exp_f32(-rate * move_dt)) / move_dt;
                } else {
                    self.frame_owed = true;
                }
            }
        } else if want != self.speed && dt > 0.0 {
            if move_dt > 0.0 {
                let tau = if self.vrun >= RUN_SPEED
                    && want.abs() > self.speed.abs()
                    && want * self.speed >= 0.0
                {
                    LAUNCH_TAU
                } else {
                    SPEED_TAU
                };
                let k = 1.0 - crate::color_math::exp_f32(-move_dt / tau);
                self.speed += (want - self.speed) * k;
            } else {
                self.frame_owed = true;
            }
        }
        let skid_leg = feed == 0.0
            && !carrying_run
            && self.pursuit_t.is_none()
            && self.leg_dist >= BRAKE_DIST
            && self.content >= SKIP_CONTENT;
        if move_dt > 0.0 && !self.braking && !skid_leg {
            let room = rest - self.col;
            if room * self.speed > 0.0 {
                let half = 0.5 * STOP_DECEL * move_dt;
                let moving_on = if !riding && feed * self.speed > 0.0 {
                    feed.abs()
                } else {
                    0.0
                };
                let cap = crate::color_math::sqrt_f32(half * half + 2.0 * STOP_DECEL * room.abs())
                    - half
                    + moving_on;
                if self.speed.abs() > cap {
                    self.speed = cap * self.speed.signum();
                }
            }
        }
        // A body at rest setting off back against the writing goes once the head law draws it
        // for the way its ease takes it ([`Self::reach_back`], [`Self::owes_the_way`]); the
        // writing's way the rest read already covers ([`HEAD_REST_REACH`]). Reverted by
        // [`OldPath::WayBeforeSettingOff`]: read from its first step, a cat seated whole flush
        // past the row above's last letter laid its box on that letter as it set off.
        if from_rest
            && self.speed < 0.0
            && move_dt > 0.0
            && self.flight.is_none()
            && !self.old_path(OldPath::WayBeforeSettingOff)
        {
            let v = self.speed;
            self.speed = 0.0;
            self.setting_off = Some(self.col - self.reach_back(self.col));
            if !self.owes_the_way(sense, width) {
                self.speed = v;
                self.setting_off = None;
            }
        }
        let step = self.speed * move_dt;
        self.col += step;
        let travelled = step.abs();
        if travelled > 0.0 || !(0.0..1024.0).contains(&self.stride) {
            let inv_stride_cells = if self.action == PetAction::Run {
                1.0 / Self::run_stride_cells(self.speed)
            } else if self.lateral {
                1.0 / LAT_STRIDE_CELLS
            } else {
                1.0 / STRIDE_CELLS
            };
            let next_stride = self.stride + travelled * inv_stride_cells;
            self.stride = if (0.0..1024.0).contains(&next_stride) {
                next_stride
            } else {
                next_stride - (next_stride * (1.0 / 1024.0)).floor() * 1024.0
            };
        }

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
            self.brake_at = self.clock;
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

        // A cat that waits ahead for the writing ([`AWAIT_AHEAD`]) keeps
        // facing the writing's way, and one past its station faces the rest
        // ahead it walks on to: turned back to its stand behind it, it looked
        // round and back at each run's start once it stood ahead of its stand
        // (the first keys' landing ahead of them), two flips a run, and for one
        // frame as its walk on began.
        let heading = if past_station { rest - self.col } else { gap };
        if (self.action.settled() || matches!(self.action, PetAction::Waking | PetAction::Land))
            && gap.abs() > ARRIVED
            && !awaits_writing
            && heading.abs() > 1e-4
        {
            self.facing_left = heading < 0.0;
        } else if self.speed.abs() > FLIP_SPEED {
            self.facing_left = self.speed < 0.0;
        }

        self.home = target;
        // Pinned: at the wall, or waiting off the caret's row for a wrap that is coming (on the
        // next line, or at its rest seat by the margin, [`Self::wrap_ahead_aim`]): the
        // writing's run carries it no further, and it is at rest where it waits.
        let pinned = self.wall_side
            || target >= f32::from(sense.cols) - width - 1e-3
            || (matches!(self.wrap_ahead, Some(w) if w.off_row())
                && (target_row - f32::from(cr)).abs() >= 0.5);
        let accelerating = matches!(self.action, PetAction::Walk | PetAction::Run)
            && (target - self.col).abs() > 0.5
            && want.abs() > FLIP_SPEED
            && self.speed.abs() < want.abs();
        // A cat waiting for the writing to reach it is at rest where it waits: kept in its
        // gait it stood in one frame of its stride, legs apart, until the writing came.
        let arrived = !accelerating
            && self.speed.abs() < FLIP_SPEED
            && (awaits_writing
                || ((target - self.col).abs() <= ARRIVED
                    && (!riding || pinned || self.carry_speed() < FLIP_SPEED)));
        // A SKID ON GLASS RUNS A STILL POSE'S FLOOR. The brake cuts a run that crosses its
        // target to a crawl, and the crawl arrives within a frame or two: crossing at 8 to 10
        // cells a second (a run to the seat the ladder deals at rest), the skid was on glass a
        // single 60 Hz frame between the stride and the seat. Once drawn, it holds the cat
        // there, still, until [`STILL_SECS`] have passed since the brake; a brake whose crawl
        // arrives on its own tick is never drawn, and the cat stops as it did.
        if arrived
            && self.brake_over.is_some()
            && self.last_pose as u16 == PetGlyphId::PetSkid as u16
            && ((self.clock - self.brake_at) as f32) < STILL_SECS
        {
            self.speed = 0.0;
            return self.emit(sense, width);
        }
        if arrived {
            self.leg_dist = 0.0;
            self.braking = false;
            self.brake_over = None;
            self.rest_kept.1 = true;
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
                && matches!(
                    self.handoff_parked_clock,
                    Some(at) if self.clock - at >= f64::from(HANDOFF_DEBOUNCE)
                )
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
                            DEPART_SPEED.max(span * (1.0 / (DEPART_MAX - EDGE_FADE))),
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
                let deal = self.mote_serial & 3;
                self.mote_serial = self.mote_serial.wrapping_add(1);
                if deal == 3
                    && let Some(dest) = self.hide_spot(
                        width,
                        (sense.cell_w, sense.cell_h),
                        (sense.cols, sense.rows),
                    )
                {
                    self.hide_to = Some(dest);
                    return self.emit(sense, width);
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
            self.enter_settled();
            // Under a whole pixel from home, an arrived body draws where home does. No frames
            // come for a glide that close ([`Self::needs_frames`]): eased on, it would cross the
            // half pixel its drawn x rounds at on whatever tick came next, at a moment no offer
            // names, and stopped short it would rest a pixel off home's about a quarter of the
            // time. Landed on the tick that parts the two pixels (a glide's or a walk's step,
            // on the frame train, or the tick that moved home or resized the cells), the body
            // draws its last pixel on a tick every schedule runs. A body a rhythm run's lead
            // carries rides its home's drift and lands once the run lets home go; the front
            // seat a pet keeps on purpose is its rest, under a pixel from home or not (a held
            // lead within a pixel of [`PECK_CLEAR`]); and an eviction's own steps do not land.
            let cw = f32::from(sense.cell_w);
            if sense.cell_w > 0
                && !riding
                && !seat_kept
                && !was_evicting
                && !self.evicting
                && !Self::glide_px_left(target - self.col, sense.cell_w)
                && (self.col * cw).round() != (target * cw).round()
            {
                self.col = target;
            }
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
                let clear = seat;
                // Only to a seat at or past home, the one the front seat holds (`seat_kept`):
                // stepped out to a seat a hair short of home, the body stood on it one tick and
                // the landing above took it home on whatever tick came next, which a host at
                // the brain's offers ran whenever anything else came.
                if self.col < clear
                    && (clear - target).abs() < ARRIVED
                    && target - clear <= 1e-4
                    && !self.ink_overlaps(clear, self.row, width)
                    && !self.console_bars((clear, self.row), sense.rows, width)
                {
                    self.col = Self::evict_toward(self.col, clear, move_dt);
                    self.evicting = self.col != clear;
                }
            }
            if !self.room_cue_act {
                self.tend_motes(width);
            }
        } else if stand_held {
            self.rest_kept.0 = true;
            if matches!(
                self.action,
                PetAction::Walk | PetAction::Run | PetAction::Land
            ) {
                self.set_action_keep(PetAction::Stand);
            }
        } else {
            self.content = (self.content + travelled * CONTENT_PER_CELL).min(1.0);
            self.content = (self.content - CONTENT_MOVING_DECAY * move_dt).max(0.0);
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
            } else if move_dt > 0.0 {
                self.gait_pace += (v - self.gait_pace)
                    * (1.0 - crate::color_math::exp_f32(-move_dt * (1.0 / GAIT_TAU)));
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
                && self.stride.fract() < travelled * (1.0 / RUN_STRIDE_CELLS)
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
    #[inline]
    pub fn tick_static_capture(&mut self, sense: PetSense) -> PetFrame {
        let frame = self.tick(sense);
        if !self.console_observation_incoherent()
            && sense.caret.is_some()
            && frame.alpha == 0
            && self.wall_cut >= 1.0
            && self.needs_frames()
        {
            self.alpha = 1.0;
            return self.emit(sense, art_cols(sense.cell_w, sense.cell_h));
        }
        frame
    }

    #[inline]
    const fn on_move(&mut self, dr: f32, dc: f32, caret_col: f32, cols: u16, reduced: bool) {
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
                self.wide_steps = if dc.abs() > 1.0 {
                    self.wide_steps.saturating_add(1)
                } else {
                    0
                };
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
                self.wide_steps = if dc.abs() > 1.0 { 1 } else { 0 };
            }
            self.run_t = 0.0;
            self.lead_room = self.lead_room.max(
                (LEAD_STEP * self.run_count.saturating_sub(RHYTHM_MOVES - 1) as f32).min(LEAD_MAX),
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
            if matches!(self.action, PetAction::Sleep) {
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
        // A jump of a burst a program is making ([`SETTLE`]) is no hand fussing, retreating
        // or leaping for the show: like the stream watch it holds the reaction ladder shut,
        // and its crossing is quiet. The coil its first jump gathered goes once the caret
        // settles. Read as the hand's, Claude Code's composer walked left, right and left
        // again as a turn ended was a frolic, and the frolic's eviction off the transcript's
        // new words launched the cat at a row the caret was leaving.
        if (line_change || dc.abs() >= POUNCE_JUMP) && self.caret_moving() {
            self.quiet_leap = true;
            self.pending_big_jump = false;
            self.big_gather = false;
            self.reversals = 0;
            return;
        }
        self.vhat = (self.vhat + dc * (1.0 / VEL_TAU)).clamp(-VEL_MAX, VEL_MAX);
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

        if self.pending_vigil_pounce && matches!(self.action, PetAction::Crouch) {
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

        let woke = matches!(self.action, PetAction::Sleep);
        if woke {
            self.pending_drop = dr == 1.0;
            if self.pending_drop && dc.abs() >= POUNCE_JUMP {
                self.pending_pounce = true;
            }
            self.set_action(PetAction::Waking);
            return;
        }
        // A jump is no writing: Claude Code's first screens walking the caret
        // along a row cut a stretch the frame after its hind reach went on
        // (one present of it before the seat).
        if dr == 0.0
            && dir > 0
            && dc < POUNCE_JUMP
            && self.run_count >= 2
            && (matches!(self.action, PetAction::Waking)
                || (!self.play_frolic
                    && self.trick_go.is_none()
                    && !self.pending_pounce
                    && !self.pending_big_jump
                    && matches!(
                        self.action,
                        PetAction::Perk | PetAction::Startle | PetAction::Frolic
                    )
                    && matches!(
                        self.last_caret,
                        Some((cr, _)) if (self.row - cr as f32).abs() < 0.5
                    )
                    && self.col < caret_col + STATION_LEAD
                    && {
                        let width = art_cols(
                            if self.last_cell_w > 1 {
                                self.last_cell_w
                            } else {
                                1
                            },
                            if self.last_cell_h > 1 {
                                self.last_cell_h
                            } else {
                                1
                            },
                        );
                        self.col + width > caret_col - STATION_LEAD
                    }))
        {
            self.facing_left = false;
            self.flinch_t = 0.0;
            self.set_action(PetAction::Walk);
        }

        let airborne = matches!(self.action, PetAction::Leap);
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

        if self.reversals >= FROLIC_REVERSALS && !matches!(self.action, PetAction::Frolic) {
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
                && (dc.abs() >= BIG_JUMP_COLS.max(BIG_JUMP_COLS_FRAC * cols as f32)
                    || (self.action.settled() && self.content >= JOY_CONTENT))
            {
                self.pending_big_jump = true;
            }
            // The writing's wrap ([`Self::writing_wrapped`]) is no surprise: the cat it left
            // at the margin goes for it without the double-take.
            if self.at_rest() && !matches!(self.action, PetAction::Perk) && !self.writing_wrapped()
            {
                if (caret_col - self.col).abs() > 1e-4 {
                    self.facing_left = caret_col < self.col;
                }
                self.set_action(PetAction::Perk);
            }
        }
    }

    /// Eases the stride `dt` toward its nearest whole stride, planting it there once it is within
    /// [`STRIDE_PLANT`]: one ease over a span plants where a run of eases over its parts does.
    #[inline]
    const fn plant_stride(&mut self, dt: f32) {
        let target = self.stride.round();
        if dt > 0.0 && (target - self.stride).abs() >= STRIDE_PLANT {
            self.stride += (target - self.stride)
                * (1.0 - crate::color_math::exp_f32(-dt * (1.0 / STRIDE_EASE)));
        }
        if (target - self.stride).abs() < STRIDE_PLANT {
            self.stride = target;
        }
    }

    /// Spends the `secs` since the last tick in the rest it left the pet in
    /// ([`Self::rest_kept`]): the gait rests, its stride planting, and an arrival's content
    /// tires. A stride is drawn only in a gait, so its plant is spent here for the whole span,
    /// however many ticks a host spent it in: the next gait sets off from the same foot.
    #[inline]
    const fn keep_rest(&mut self, secs: f32) {
        let (gait, content) = self.rest_kept;
        self.rest_kept = (false, false);
        if secs <= 0.0 {
            return;
        }
        if content {
            self.content = (self.content - CONTENT_DECAY * secs).max(0.0);
        }
        if gait {
            if self.gait_pace != 0.0 {
                self.gait_pace *= crate::color_math::exp_f32(-secs * (1.0 / GAIT_REST_TAU));
            }
            self.gait_rest += secs;
            self.plant_stride(secs);
        }
    }

    /// Settles the pet. The rest begins: the span to the next tick rests its gait and plants its
    /// stride ([`Self::keep_rest`]).
    #[inline]
    const fn enter_settled(&mut self) {
        // Seated by the console, the cat sits at once, its seat's beats starting with the
        // seat: put past the fold, it sat down on the tail's first beat and the flick cut the
        // seat's first pose after a frame.
        if self.console.rest_seated.get() && self.pending_trick.is_none() && self.quiet < SIT_AFTER
        {
            self.quiet = SIT_AFTER;
        }
        self.speed = 0.0;
        self.rest_kept.0 = true;
        self.quiet_leap = false;
        self.drop_stare = false;

        if !self.action.settled() {
            self.settle_serial = self.settle_serial.wrapping_add(1);
            self.settle_seed = self.settle_serial.wrapping_add(self.mote_serial);
            // A LATE SEAT: a cat whose trip home outlasted the quiet before a seat
            // ([`SIT_AFTER`]) starts its rest from its seat: the seat's beats (tail, blink,
            // glance) and the ladder after it (loaf, groom, sleep) run from here. Counted
            // from the caret's last move, it sat down halfway through a beat and the tail's
            // flick cut the seat's first pose after a frame, or sat for one frame and loafed.
            // A sleeper an edit sent home goes back to its sleep there ([`SLEEP_AFTER`]):
            // started on a new seat, it sat up awake beside the caret for twenty seconds.
            if self.quiet > SIT_AFTER
                && self.quiet < SLEEP_AFTER
                && self.pending_trick.is_none()
                && self.trick_settle.is_none()
            {
                self.quiet = SIT_AFTER;
            }
        }

        if self.trick_settle.is_some()
            && !matches!(self.trick_settle, Some(Trick::Sleep))
            && self.quiet >= self.hold_end_q
        {
            self.clear_trick_settle();
        }
        let next = if matches!(self.trick_settle, Some(Trick::Down)) {
            PetAction::Loaf
        } else if matches!(self.trick_settle, Some(Trick::Sit)) {
            // Commanded seats are checked above the ladder's sleep, so a cat
            // already past [`SLEEP_AFTER`] still wears the seat for the hold.
            PetAction::Sit
        } else if self.quiet >= SLEEP_AFTER
            && (!self.waiting_up() || matches!(self.trick_settle, Some(Trick::Sleep)))
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
        } else if matches!(self.action, PetAction::Waking)
            || (self.woke_seat && matches!(self.action, PetAction::Sit))
        {
            // The wake's stretch ends in the seat it is drawn in, and the cat keeps it until
            // the caret moves: stood up on the next tick, the seat was on glass one frame.
            self.woke_seat = true;
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
        self.settle_seed & 3 == 3
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
        let sum = beat + self.settle_seed as i64;
        if n == 4 { sum & 3 } else { sum.rem_euclid(n) }
    }

    #[inline]
    const fn sit_tail(&self) -> Option<PetGlyphId> {
        let (period, at) = self.tail_clock();
        let Some((beat, p)) = self.sit_beat(period, at) else {
            return None;
        };
        let deal = self.beat_deal(beat, 4);
        if self.sit_front && !self.side_on_holds(p, Self::tail_hand_len(deal)) {
            return None;
        }
        // How long the pose this beat deals now has left on glass.
        let (pose, ends) = match deal {
            0 if p < TAIL_FLICK => (PetGlyphId::PetSitFlick, TAIL_FLICK),
            1 if p < TAIL_TAP => (PetGlyphId::PetSitFlick, TAIL_TAP),
            1 if p >= TAIL_TAP + TAIL_GAP && p < 2.0 * TAIL_TAP + TAIL_GAP => {
                (PetGlyphId::PetSitFlick, 2.0 * TAIL_TAP + TAIL_GAP)
            }
            2 => match (p * (1.0 / TAIL_SWING)).floor() as i64 {
                0 => (PetGlyphId::PetSitFlick, TAIL_SWING),
                1 | 2 => (PetGlyphId::PetSitTailBack, 3.0 * TAIL_SWING),
                3 | 4 => (PetGlyphId::PetSitTailLow, 5.0 * TAIL_SWING),
                5 => (PetGlyphId::PetSitTailBack, 6.0 * TAIL_SWING),
                6 => (PetGlyphId::PetSitFlick, 7.0 * TAIL_SWING),
                _ => return None,
            },
            _ => return None,
        };
        // A beat the seat comes into with less than two frames of its pose left (a landing
        // during a flick: the quiet clock deals beats in flight too) is not begun: its sliver
        // would be a one-frame flash ([`Self::sit_blink_shows`] keeps a blink the same way).
        if self.last_pose as u16 == pose as u16 || ends - p >= STILL_SECS {
            Some(pose)
        } else {
            None
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
        let q_full = COIL_SQUASH * (dur * (1.0 / COIL_MAX)).min(1.0);
        let release0 = (dur - COIL_RELEASE).max(f32::EPSILON);
        if t < release0 {
            let q = q_full * (t / release0).min(1.0);
            let y = 1.0 - q;
            (y, 1.0 / y)
        } else {
            let r = ((t - release0) * (1.0 / COIL_RELEASE)).clamp(0.0, 1.0);
            let y0 = 1.0 - q_full;
            let x0 = 1.0 / y0;
            let (y1, x1) = (1.0 - 0.6 * LAUNCH_STRETCH, 1.0 + LAUNCH_STRETCH);
            (y0 + (y1 - y0) * r, x0 + (x1 - x0) * r)
        }
    }

    #[inline]
    const fn flight_lift(arc: f32, u: f32, big: bool) -> f32 {
        if big {
            if u < 0.5 {
                let s = 1.0 - 2.0 * u;
                arc * (1.0 - s * s * s)
            } else {
                arc * (1.0 - crate::color_math::powf_pos_f32(2.0 * u - 1.0, HANG_P_DOWN))
            }
        } else {
            arc * 4.0 * u * (1.0 - u)
        }
    }

    /// A letter written at the caret this tick: a text key the console saw,
    /// or, for keys that reach the terminal past it (`aterm ctl key`), the
    /// caret's step along its row of a cell or two (a wide letter) onto the
    /// row's end, the cells it left lettered: as the console's letter map
    /// reads the page now ([`Self::observed_last_letter`]), else as the
    /// page's ink does ([`Self::ink_span`], a key behind where the frame's
    /// ink is read after the caret's move).
    #[inline]
    fn letter_written_at_caret(&self, (cr, cc): (u16, u16), move_dc: f32, cols: u16) -> bool {
        if move_dc <= 0.0 {
            return false;
        }
        if matches!(self.key_within_dur(HAND_ECHO_DUR), Some(PetInputKind::Text)) {
            return true;
        }
        if move_dc > 2.5 {
            return false;
        }
        let (row, caret) = (f32::from(cr), f32::from(cc));
        match (
            self.observed_last_letter(row, caret - move_dc, caret),
            self.observed_last_letter(row, caret, f32::from(cols).max(caret + 1.0)),
        ) {
            (Some(left), Some(beyond)) => left.is_some() && beyond.is_none(),
            _ => matches!(
                self.ink_span(row),
                Some((first, end)) if (end - caret).abs() < 0.5 && first < end
            ),
        }
    }

    /// A flight's (column, row) pace, cells a second: its way over its time.
    #[cfg(test)]
    #[inline]
    const fn flight_pace(f: &Flight) -> (f32, f32) {
        let t = f.dur.max(1e-3);
        (
            (f.to_col - f.from_col).abs() / t,
            (f.to_row - f.from_row).abs() / t,
        )
    }

    /// The start that draws the share `s` of a flight's way at `at` on the way to `to`: a
    /// landing rebased in the air, from where the body is drawn, so the drawn path does not
    /// jump. `None` once the way has run (`s` at 1), where no start does and the flight keeps
    /// its landing: solved there, the start was infinite and the drawn column `inf - inf`
    /// (looks8 round 3's review: an Enter during a drop drew the kitty at a NaN column).
    #[inline]
    const fn rebased_from(at: f32, to: f32, s: f32) -> Option<f32> {
        if s < 1.0 - REBASE_SHARE_EPS {
            Some((at - to * s) / (1.0 - s))
        } else {
            None
        }
    }

    /// Rebase a flight in the air onto the landing `to` (a column, `None` to keep its own,
    /// and a row) from `at`, where the body is drawn, at the share `s` of its way
    /// ([`Self::rebased_from`]).
    ///
    /// THE TAKE-OFF TOP GOES WITH A WAY UP. The box's top a body the head law held took off
    /// with ([`Self::left_top`]) keeps a crossing down off that row under it: the frame draws
    /// the box no higher than that top ([`Self::sunk`]). A body flying up comes to stand over
    /// its take-off row, so a way up, before this re-aim or after it, drops the top: kept, a
    /// flight up turned back down in the air drew the box at its take-off top, tens of rows
    /// under the body in one frame and over the row under the one it left. Reverted by
    /// [`OldPath::TurnKeepsTheTakeOffTop`].
    #[inline]
    const fn rebase(&self, f: &mut Flight, s: f32, at: (f32, f32), to: (Option<f32>, f32)) {
        let up_before = f.to_row - f.from_row <= -0.5;
        if let Some(to_col) = to.0
            && let Some(from) = Self::rebased_from(at.0, to_col, s)
        {
            f.from_col = from;
            f.to_col = to_col;
        }
        if let Some(from) = Self::rebased_from(at.1, to.1, s) {
            f.from_row = from;
            f.to_row = to.1;
        }
        if (up_before || f.to_row - f.from_row <= -0.5)
            && !self.old_path(OldPath::TurnKeepsTheTakeOffTop)
        {
            f.left_top = None;
        }
    }

    #[inline]
    const fn pose_schedule(dur: f32) -> (f32, f32) {
        let (rise, desc) = (LEAP_RISE_T, LEAP_DESC_T);
        if rise + desc + MIN_HERO > dur {
            let k = (dur - MIN_HERO) * (1.0 / (LEAP_RISE_T + LEAP_DESC_T));
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

    /// The seat the console holds for the cat at rest ([`PetBrain::console_station`]: clean
    /// ground beside a composer whose upper rule the host holds), where the console, reading the
    /// frame the caret moved `dr` rows down in, already put it `dr` rows under the body at its
    /// own column: the seat went down with the composer, and the cat rides with it
    /// ([`Self::carry_with_regrid`]), the seat kept as the console put it, as far as a box
    /// ride goes ([`console_life::RIDE_ROWS_MAX`]; further is a bound). Hopped after its seat
    /// instead, the cat under Claude Code's footer hopped a row down each time a turn's last
    /// line pushed the composer, whose footer words change as it moves, so its rows' signatures
    /// never matched ([`Self::content_ride`]).
    #[inline]
    const fn seat_ride(&self, dr: f32) -> Option<(f32, f32)> {
        let Some(seat) = self.console.rest_seat.get() else {
            return None;
        };
        if dr <= console_life::RIDE_ROWS_MAX as f32
            && self.console.rest_seated.get()
            && self.action.settled()
            && (seat.0 - self.col).abs() <= ARRIVED
            && (seat.1 - (self.row + dr)).abs() < 0.05
        {
            Some(seat)
        } else {
            None
        }
    }

    /// The stage of a flight's pose: its launch, rise, leap (or hop), apex, descent.
    #[inline]
    const fn flight_stage(pose: PetGlyphId) -> u8 {
        match pose {
            PetGlyphId::PetLaunch => 0,
            PetGlyphId::PetLeapRise => 1,
            PetGlyphId::PetApex => 3,
            PetGlyphId::PetLeapDescend => 4,
            _ => 2,
        }
    }

    /// The pose a flight draws at a stage ([`Self::flight_stage`]).
    #[inline]
    const fn stage_pose(stage: u8, hop: bool) -> PetGlyphId {
        match stage {
            0 => PetGlyphId::PetLaunch,
            1 => PetGlyphId::PetLeapRise,
            3 => PetGlyphId::PetApex,
            4 => PetGlyphId::PetLeapDescend,
            _ if hop => PetGlyphId::PetHop,
            _ => PetGlyphId::PetLeap,
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
    const fn station_drift(&self, t: f32) -> f32 {
        if !self.aims_ahead() {
            return 0.0;
        }
        let k = 1.0 - crate::color_math::exp_f32(-t * (1.0 / VEL_TAU));
        let keep_ahead = self.keep_ahead();
        let stand = keep_ahead.max(self.held_lead);
        let lead = stand + self.carry(stand, self.run_t);
        (self.pace() * VEL_TAU * k + keep_ahead * (1.0 - k) - lead).clamp(0.0, FLIGHT_LEAD_MAX)
    }

    #[inline]
    const fn hop_aim(
        &self,
        to_col: f32,
        to_row: f32,
        vertical: bool,
        cols: u16,
        width: f32,
    ) -> f32 {
        if !self.aims_ahead() {
            return to_col;
        }
        let (dur0, _) = Self::flight_shape(span_eq(to_col - self.col, to_row - self.row), vertical);
        let lead = self.station_drift(dur0 + LAND_DUR);
        (to_col + lead).clamp(0.0, (cols as f32 - width).max(0.0))
    }

    #[inline]
    const fn predicted_tier(
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
    const fn notices(
        &self,
        to_col: f32,
        to_row: f32,
        vertical: bool,
        cols: u16,
        width: f32,
    ) -> bool {
        self.at_rest()
            && !matches!(self.action, PetAction::Perk)
            && !self.drop_stare
            && !self.writing_wrapped()
            && !self.ink_overlaps(self.col, self.row, width)
            && matches!(
                self.predicted_tier(to_col, to_row, vertical, cols, width),
                Tier::Bound
            )
    }

    #[inline]
    const fn leave_ground(
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
        if !matches!(policy, LeapPolicy::Play) {
            self.pending_wall_transit = false;
            self.pending_pounce = false;
        }
        self.launch_running = matches!(policy, LeapPolicy::Quiet) && self.rhythm_open();
        // Every bound after the caret is brisk ([`Self::brisk_shape`]): the writing's wrap, an
        // Enter's new prompt, a line cleared back to its prompt, a program's jump.
        let wrap =
            matches!(policy, LeapPolicy::Quiet) && (self.writing_wrapped() || self.wrap_back);
        self.launch_brisk = matches!(policy, LeapPolicy::Quiet);
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
                self.launch_big(
                    to_col,
                    to_row,
                    cols,
                    width,
                    matches!(policy, LeapPolicy::Show),
                );
            }
        }
        self.quiet_leap = false;
        self.drop_stare = false;
        self.launch_running = false;
        if wrap {
            self.writing_wrap = None;
        }
        self.launch_brisk = false;
        if !matches!(policy, LeapPolicy::Play) {
            self.wrap_back = false;
        }
    }

    #[inline]
    const fn begin_arc(&mut self, to_col: f32, to_row: f32, dur: f32, arc: f32, kind: FlightKind) {
        // NO STILL POSE IS A ONE-FRAME FLASH BEFORE A FLIGHT. A still pose
        // just put on glass (a wake's last beat, a landing's seat, a play
        // bow, a notice) stays on it [`STILL_SECS`] before the cat leaves the
        // ground from it; taken at once, it flashed for a single frame
        // between the pose before it and the leap. The flight leaves on a
        // later tick ([`Self::deferred_arc`]), as aimed, under two 60 Hz
        // frames late, on the words or off them. The stand a stride ends in
        // is held a stride ([`STAND_MIN`], [`Self::still_hold_fresh`]) against
        // every other pose, and against the leap too: a stream's watch that
        // stopped a run under a program's long insert stood the cat, and the
        // wrap that brought the caret down launched it 3 ms short of the
        // stride, a stand flashed between the run and the air.
        if !kind.drop && self.flight.is_none() && self.still_hold_fresh() {
            self.deferred_arc = Some((to_col, to_row, dur, arc, kind));
            return;
        }
        self.deferred_arc = None;
        let FlightKind {
            big,
            show,
            hop,
            drop,
            brisk,
        } = kind;
        let bound = if big && brisk {
            BRISK_SPEED_MAX
        } else if big {
            BIG_HOP_SPEED_MAX
        } else {
            HOP_SPEED_MAX
        };
        let span = span_eq(to_col - self.col, to_row - self.row);
        let rate = span / dur.max(0.05);
        debug_assert!(
            rate <= bound + 1e-3,
            "no flight may outrun the animal that flies it",
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
        let left_top = self.left_top();
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
            coiled: matches!(self.action, PetAction::Crouch)
                && !self.hiding
                && !self.vigil
                && !self.stakeout,
            drop,
            late_move: false,
            running: self.launch_running,
            brisk,
            cap: (span / dur.max(0.05)).clamp(BIG_HOP_SPEED_MAX, BRISK_SPEED_MAX),
            stage: 0,
            left_top,
        });
        self.set_action(PetAction::Leap);
    }

    /// Draws out the rest of a flight to at least `rest` seconds, its progress `t / dur`
    /// kept, so its body, lift and stretch carry on from where they are; its poses never go
    /// back a stage ([`Flight::stage`]).
    #[inline]
    const fn retime(f: &mut Flight, rest: f32) {
        let left = f.dur - f.t;
        if rest > left && left > 0.0 {
            let k = rest / left;
            f.t *= k;
            f.dur *= k;
        }
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

    /// THE WRITING'S WRAP IS A BRISK BOUND. The writing that wraps a line takes the caret from
    /// the margin back to the next line's start, across most of the pane, and writes on as the
    /// cat crosses after it (or a held delete takes the caret back across the wrap,
    /// [`Self::wrap_back_aim`]): the cat goes at the animal's own pace ([`BIG_HOP_SPEED_MAX`],
    /// half its body a 60 Hz frame at most), on the big bound's own arc for the span
    /// ([`Self::big_shape`]), high over the line it crosses, and comes down by the writing, in
    /// [`BRISK_DUR_MAX`] at most: a line wider than 80 columns is crossed as quickly, faster
    /// (up to [`BRISK_SPEED_MAX`], under its body a 60 Hz frame). Held to the animal's pace at
    /// any width, the crossing at 150 columns was a second in the air (r24 K7-5), floating over
    /// the line as the writing ran on, as at 120 columns at 0.8 s. Paced as a quiet crossing
    /// ([`quiet_speed_max`], 0.74 of the pane a second) the bound across Claude Code's composer
    /// at each of its word wraps took 1.06 to 1.17 s, floating over the line as the caret ran 25
    /// cells on. Flown low on a shorter arc, the body crossed the line it left and dimmed its
    /// letters. The cat leaves for the next line when the wrap has come, never on a guess of
    /// when it will ([`Self::wrap_ahead_aim`]).
    ///
    /// EVERY BOUND AFTER THE CARET IS BRISK (r25 K8-3). An Enter's new prompt, a line cleared
    /// back to its prompt (Ctrl+U), the prompt under a listing a command printed, a program's
    /// jump: each is a jump of the caret the cat follows in one bound ([`LeapPolicy::Quiet`]),
    /// and paced as a quiet crossing they hung in the air like the wrap's did, the bound back
    /// to the prompt after a Ctrl+U up to 1.07 s at 150 columns, an Enter's 0.78 to 0.96 s and
    /// the one after `ls -la` 0.91 to 0.96 s (windowed zsh, round 24). Brisk, each is down in
    /// [`BRISK_DUR_MAX`] at any width, in one bound (its pace never splits it,
    /// [`QUIET_ONE_BOUND_SECS`]). The chosen show and play keep the quiet pace: they are the
    /// cat's own, not the caret's.
    const fn brisk_shape(span: f32) -> (f32, f32) {
        let dur = (span / BIG_HOP_SPEED_MAX)
            .clamp(BIG_FLIGHT_BASE, BRISK_DUR_MAX)
            .max(span / BRISK_SPEED_MAX);
        (dur, Self::big_shape(span).1)
    }

    /// The fastest pace (cells a second) a bound flies at: the brisk bound's
    /// ([`Self::brisk_shape`]), or a quiet crossing's ([`quiet_speed_max`]).
    #[inline]
    const fn bound_pace(brisk: bool, cols: u16) -> f32 {
        if brisk {
            BRISK_SPEED_MAX
        } else {
            quiet_speed_max(cols)
        }
    }

    /// The fastest a flight in the air may be drawn on to a landing re-aimed under it
    /// ([`Self::retime`]): its own pace, the brisk bound's as it launched ([`Flight::cap`]).
    #[inline]
    const fn flight_cap(f: &Flight, cols: u16) -> f32 {
        if f.big && f.brisk {
            f.cap
        } else if f.big {
            Self::bound_pace(false, cols)
        } else {
            HOP_SPEED_MAX
        }
    }

    /// A bound's time in the air and its arc: the brisk bound's ([`Self::brisk_shape`]), or the
    /// big bound's shape paced as a quiet crossing.
    #[inline]
    const fn bound_shape(span: f32, brisk: bool, cols: u16) -> (f32, f32) {
        if brisk {
            Self::brisk_shape(span)
        } else {
            let (dur, arc) = Self::big_shape(span);
            (dur.max(span / quiet_speed_max(cols)), arc)
        }
    }

    #[inline]
    const fn begin_big_arc(&mut self, to_col: f32, to_row: f32, cols: u16, show: bool) {
        let span = span_eq(to_col - self.col, to_row - self.row);
        let brisk = self.launch_brisk && !show;
        let (dur, arc) = Self::bound_shape(span, brisk, cols);
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
                brisk,
                ..FlightKind::PLAIN
            },
        );
    }

    #[inline]
    const fn launch_big(&mut self, to_col: f32, to_row: f32, cols: u16, width: f32, show: bool) {
        let span0 = span_eq(to_col - self.col, to_row - self.row);
        let brisk = self.launch_brisk && !show;
        let (dur0, _) = Self::bound_shape(span0, brisk, cols);
        let drift = if self.play_land.is_none() {
            self.station_drift(dur0 + BIG_LAND_DUR)
        } else {
            0.0
        };
        let cols_f = cols as f32;
        let mut aim = to_col + drift;
        aim = aim.clamp(0.0, (cols_f - width).max(0.0));
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
            delta.abs() >= TWO_BOUND_FRAC * cols_f
        } else {
            delta.abs() / Self::bound_pace(brisk, cols) > QUIET_ONE_BOUND_SECS
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

    #[inline]
    const fn spawn_dust(&mut self, width: f32) {
        let heel = self.col + width * 0.5;
        let seat = DUST_SEAT;
        #[cfg(test)]
        let seat = if self.dust_floats { 0.31 } else { seat };
        let mut k = 0u8;
        while k < 3 {
            let seed = self.mote_serial.wrapping_add(k);
            self.spawn_mote(Mote {
                kind: PetMoteKind::Dust,
                born: self.clock,
                life: 0.45 + 0.07 * (k as f32),
                col: heel + 0.23 + 0.17 * ((k % 3) as f32),
                row: self.row + seat,
                dir: -self.skid_dir,
                seed,
                rgb: None,
            });
            k += 1;
        }
        self.mote_serial = self.mote_serial.wrapping_add(3);
    }

    #[inline]
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

    #[inline]
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

    #[inline]
    const fn settle_gaze(
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
        let asleep = matches!(self.action, PetAction::Sleep);
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
        let behind = match side {
            Some(left) => left != self.facing_left && !(caret_gaze && self.sit_front),
            None => false,
        };
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

    #[inline]
    const fn tend_motes(&mut self, width: f32) {
        match self.action {
            PetAction::Sleep => {
                // light sleep only: no new z once its life would outlive the
                // breath window, so deep sleep begins with an empty lane.
                if self.quiet > SLEEP_AFTER + BREATH_WINDOW - ZEE_LIFE {
                    return;
                }
                let beat = beats_reached(self.quiet, SLEEP_AFTER, ZEE_EVERY);
                if beat >= 1 && beat != self.mote_mark {
                    let mut zee_alive = 0usize;
                    let mut i = 0;
                    while i < self.motes.len() {
                        if matches!(self.motes[i], Some(m) if matches!(m.kind, PetMoteKind::Zee)) {
                            zee_alive += 1;
                            if zee_alive >= ZEE_ALIVE_MAX {
                                break;
                            }
                        }
                        i += 1;
                    }
                    if zee_alive < ZEE_ALIVE_MAX {
                        self.mote_mark = beat;
                        self.spawn_dealt(PetMoteKind::Zee, ZEE_LIFE, self.head_anchor(width));
                    }
                }
            }
            PetAction::Purr | PetAction::Sit | PetAction::Loaf if self.purring() => {
                let beat = beats_reached(self.quiet, SIT_AFTER, PURR_MOTE_EVERY);
                if beat >= 1 && beat != self.mote_mark {
                    self.mote_mark = beat;
                    self.spawn_chest_mote(None, width);
                }
            }
            _ => {}
        }
    }

    #[inline]
    const fn spawn_wake_pop(&mut self, width: f32) {
        self.spawn_dealt(PetMoteKind::ZeePop, ZEE_POP_LIFE, self.head_anchor(width));
    }

    #[inline]
    const fn consume_stimuli(&mut self, sense: &PetSense, caret_col: u16, width: f32) -> bool {
        // Fright first: the bell interrupts what the others would start.
        if self.pending_bell.take().is_some() {
            self.quiet = 0.0;
            if matches!(self.action, PetAction::Sleep) {
                self.set_action(PetAction::Waking);
                self.spawn_wake_pop(width);
            } else {
                self.facing_left = (caret_col as f32) < self.col;
                self.flinch_t = FLINCH_DUR;
                self.set_action(PetAction::Perk);
            }
            return true;
        }
        if self.pending_sulk {
            if matches!(self.action, PetAction::Sleep) {
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
            if matches!(self.action, PetAction::Sleep) {
                self.quiet = 0.0;
                self.set_action(PetAction::Waking);
                self.spawn_wake_pop(width);
                return true;
            }
            if !matches!(self.action, PetAction::Perk) {
                self.set_action(PetAction::Perk);
                return true;
            }
            self.pending_cheer = None;
            self.cheer_frolic(big, width, false);
            return true;
        }
        if self.pending_pet > 0 {
            if matches!(self.action, PetAction::Sleep) {
                self.quiet = 0.0;
                self.set_action(PetAction::Waking);
                self.spawn_wake_pop(width);
                return true;
            }
            let n = self.pending_pet;
            self.pending_pet = 0;
            self.pet_at = None;
            self.content = (self.content + (n as f32) * PET_CONTENT).min(1.0);
            self.pet_hold_t = PET_HOLD;
            self.set_action(PetAction::Purr);
            self.spawn_pet_hearts(n, width);
            return true;
        }
        if self.consume_trick(sense, width) {
            return true;
        }
        if self.pending_flow.take().is_some() {
            if matches!(self.action, PetAction::Sleep) {
                return false;
            }
            self.set_action(PetAction::Frolic);
            return true;
        }
        false
    }

    #[inline]
    const fn cheer_frolic(&mut self, big: bool, width: f32, compact: bool) {
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

    #[inline]
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
        if now.saturating_duration_since(p.stamped) > TRICK_TTL_DUR
            || now.saturating_duration_since(p.noted) > TRICK_CAP_DUR
        {
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

    #[inline]
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

    #[inline]
    const fn consume_trick(&mut self, sense: &PetSense, width: f32) -> bool {
        let Some(p) = self.pending_trick else {
            return false;
        };
        if self.quiet < Self::trick_settle_after(p.confirmed) {
            return false;
        }
        if matches!(self.action, PetAction::Sleep) {
            if matches!(p.trick, Trick::Sleep) {
                self.pending_trick = None;
                return false;
            }
            if matches!(p.trick, Trick::Stretch) {
                self.pending_trick = None;
            }
            self.quiet = 0.0;
            self.set_action(PetAction::Waking);
            self.spawn_wake_pop(width);
            return true;
        }
        self.pending_trick = None;
        self.trick_go = Some(p.trick);
        self.trick_heard = if !p.confirmed {
            Some((p.trick, sense.now))
        } else {
            None
        };
        self.perk_in_place();
        true
    }

    #[inline]
    const fn perform_trick(
        &mut self,
        trick: Trick,
        caret_col: u16,
        width: f32,
        (cell, grid): ((u16, u16), (u16, u16)),
    ) {
        let caret = caret_col as f32;
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
                self.enter_settled();
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
                if let Some(dest) = self.hide_spot(width, cell, grid) {
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

    #[inline]
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

    #[inline]
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

    #[inline]
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
    const fn age_gesture_windows(&mut self, secs: f32) {
        if secs <= 0.0 {
            return;
        }
        self.twitch_cool = (self.twitch_cool - secs).max(0.0);
        self.reversal_t += secs;
        if self.reversal_t > FROLIC_WINDOW {
            self.reversals = 0;
            self.reversal_t = 0.0;
        }
        self.retreat_gap = (self.retreat_gap + secs).min(60.0);
        if self.vhat != 0.0 || self.vrun != 0.0 || self.run_pace != 0.0 {
            let d = crate::color_math::exp_f32(-secs * (1.0 / VEL_TAU));
            self.vhat *= d;
            self.vrun *= d;
            self.run_pace *= d;
        }
        let carrying = self.carrying_run();
        // The lead's room decays from the window's close on, however the span is cut: decayed
        // by all of a span that closed the window, a host that slept across the close shrank it
        // by the part before as well, and the next run's lead stood short.
        let past_window = (secs - (RHYTHM_WINDOW - self.run_t).max(0.0)).max(0.0);
        self.run_t = (self.run_t + secs).min(60.0);
        if self.run_t > RHYTHM_WINDOW {
            if carrying {
                self.held_lead = self.lead_at(RHYTHM_WINDOW);
            }
            self.run_count = 0;
            if self.lead_room != 0.0 {
                self.lead_room *= crate::color_math::exp_f32(-past_window * (1.0 / VEL_TAU));
            }
        }
    }

    #[inline]
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
        self.twitch_cool = 0.0;
        self.twitch_spent = false;
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

    #[inline]
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

    #[inline]
    const fn consume_room(&mut self, caret_col: u16, width: f32) -> bool {
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
                self.facing_left = (caret_col as f32) < mid;
                self.perk_in_place();
            }
            RoomCue::Mail => {
                self.facing_left = (caret_col as f32) < mid;
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

    #[inline]
    const fn mail_beat(&mut self, width: f32) {
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

    #[inline]
    const fn consume_bat(&mut self, sense: &PetSense, width: f32) -> bool {
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
        .clamp(0.0, (sense.cols as f32 - width).max(0.0));
        let to_row = by.clamp(0.0, sense.rows.saturating_sub(1) as f32);
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

    #[inline]
    const fn consume_pointer_pounce(&mut self, sense: &PetSense, width: f32) -> bool {
        let Some((ac, ar)) = self.pending_pointer_pounce.take() else {
            return false;
        };
        if self.room_hold || matches!(self.action, PetAction::Sleep | PetAction::Waking) {
            return false;
        }
        let to_col = (ac - width * 0.5).clamp(0.0, (sense.cols as f32 - width).max(0.0));
        let to_row = ar.clamp(0.0, sense.rows.saturating_sub(1) as f32);
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

    #[inline]
    const fn consume_watch(&mut self, elapsed: f32, caret_col: u16) -> bool {
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
        self.facing_left = (caret_col as f32) < self.col;
        self.sit_front = false;
        self.peek = false;
        self.gaze_dwell = 0.0;
        if !matches!(self.action, PetAction::Perk) {
            self.set_action(PetAction::Perk);
        }
        self.speed = 0.0;
        true
    }

    #[inline]
    const fn spawn_cheer_motes(&mut self, big: bool, width: f32, compact: bool) {
        // A cheer's notes are the notes in its air: one still rising there from the purr the
        // cheer broke into is one of them, so its lanes hold three together, never a fourth.
        // A note left behind where the cat sat before it went to its stage is not in that air.
        let max_notes = if big { CHEER_LANES } else { CHEER_LANES - 1 };
        let (ax, ay) = self.chest_anchor(width);
        let col_lo = ax - CHEER_AIR_SLACK;
        let col_hi = ax + CHEER_LANES_REACH.0 + CHEER_AIR_SLACK;
        let row_lo = ay - CHEER_LANES_REACH.1 - CHEER_AIR_SLACK;
        let row_hi = ay + CHEER_AIR_SLACK;
        let mut aloft = 0usize;
        let mut i = 0;
        while i < self.motes.len() {
            if let Some(m) = self.motes[i]
                && matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart)
                && m.col >= col_lo
                && m.col <= col_hi
                && m.row >= row_lo
                && m.row <= row_hi
                && m.age(self.clock) < 1.0
            {
                aloft += 1;
                if aloft >= max_notes {
                    return;
                }
            }
            i += 1;
        }
        let n = max_notes - aloft;
        if compact {
            let mut k = 0;
            while k < n {
                let kf = k as f32;
                let kind = self.next_note_kind();
                self.spawn_dealt(
                    kind,
                    PURR_MOTE_LIFE,
                    (
                        ax + CHEER_COMPACT_STEP.0 * kf,
                        ay - CHEER_COMPACT_STEP.1 * kf,
                    ),
                );
                k += 1;
            }
        } else {
            let mut k = 0;
            while k < n {
                self.spawn_chest_mote(None, width);
                k += 1;
            }
        }
    }

    #[inline]
    const fn spawn_pet_hearts(&mut self, n: u8, width: f32) {
        let mut k = 0u8;
        while k < n {
            self.spawn_chest_mote(Some(PetMoteKind::Heart), width);
            k += 1;
        }
    }

    #[inline]
    const fn chest_anchor(&self, width: f32) -> (f32, f32) {
        let chest_x = if self.facing_left { 0.38 } else { 0.62 };
        (self.col + width * chest_x + 0.23, self.row - 0.27)
    }

    #[inline]
    const fn spawn_chest_mote(&mut self, kind: Option<PetMoteKind>, width: f32) {
        let kind = match kind {
            Some(k) => k,
            None => self.next_note_kind(),
        };
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

    #[inline]
    const fn note_lane_birth(&self, kind: PetMoteKind, (col, row): (f32, f32)) -> (f32, f32) {
        #[inline]
        const fn safe_inside_at(
            (x, _): (f32, f32),
            max_dest: i32,
            max_half: i32,
            max_drift: f32,
            cell_w_f: f32,
            grid_w: i32,
            grid_h: i32,
        ) -> bool {
            max_dest <= grid_w
                && max_dest <= grid_h
                && ((x - max_drift) * cell_w_f).floor() as i32 - max_half >= 0
                && ((x + max_drift) * cell_w_f).ceil() as i32 + max_half <= grid_w
        }

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
        let mut mi = 0;
        while mi < self.motes.len() {
            if let Some(m) = self.motes[mi]
                && matches!(m.kind, PetMoteKind::Note | PetMoteKind::Heart)
            {
                let u = m.age(self.clock);
                if u >= 0.0 && u < 1.0 {
                    max_live_end = max_live_end.max(m.born + (m.life.max(0.01) as f64));
                    live[live_len] = m;
                    live_len += 1;
                }
            }
            mi += 1;
        }
        let live = live.split_at(live_len).0;
        let cw = if self.last_cell_w > 1 {
            self.last_cell_w
        } else {
            1
        };
        let ch = if self.last_cell_h > 1 {
            self.last_cell_h
        } else {
            1
        };
        let cell = (cw, ch);
        let cell_w_f = cell.0 as f32;
        let grid = self.last_grid;
        let grid_w = (grid.0 as i32).saturating_mul(cell.0 as i32);
        let grid_h = (grid.1 as i32).saturating_mul(cell.1 as i32);
        let max_dest_raw = ((mote_tile_side(cell.1) as f32) * NOTE_SCALE_MAX).round() as i32;
        let max_dest = if max_dest_raw < 1 {
            1
        } else if max_dest_raw > u16::MAX as i32 {
            u16::MAX as i32
        } else {
            max_dest_raw
        };
        let max_half = (max_dest + 1) / 2;
        let max_drift = NOTE_BASE + NOTE_RUN + NOTE_SWAY + 0.01;
        if live.is_empty()
            && safe_inside_at(
                (col, row),
                max_dest,
                max_half,
                max_drift,
                cell_w_f,
                grid_w,
                grid_h,
            )
        {
            return (col, row);
        }
        let dir = if self.facing_left { -1.0 } else { 1.0 };
        let step = note_lane_step(cell.0, cell.1);
        const MAX_TICKS: usize = (PURR_MOTE_LIFE * 60.0).ceil() as usize;
        const INV_60: f64 = 1.0 / 60.0;
        let ticks = MAX_TICKS as u32;
        let mut born = Mote {
            kind,
            born: self.clock,
            life: PURR_MOTE_LIFE,
            col: 0.0,
            row: 0.0,
            dir,
            seed: self.mote_serial,
            rgb: None,
        };
        if live.is_empty() {
            let mut li = 0;
            while li < NOTE_LANES.len() {
                let (along, up) = NOTE_LANES[li];
                li += 1;
                let at = (col + dir * along * step, row - up);
                if safe_inside_at(at, max_dest, max_half, max_drift, cell_w_f, grid_w, grid_h) {
                    return at;
                }
                born.col = at.0;
                born.row = at.1;
                let mut inside = true;
                let mut k = 0u32;
                while k < ticks {
                    let clock = self.clock + (k as f64) * INV_60;
                    k += 1;
                    let Some((centre, scale)) = born.note_geom(clock) else {
                        break;
                    };
                    let Some(b) = mote_box_px(centre, scale, cell, grid) else {
                        continue;
                    };
                    let half = ((b.2 as i32) + 1) / 2;
                    let cx = (centre.0 * cell_w_f).round() as i32;
                    if cx - half < 0 || cx + half > grid_w {
                        inside = false;
                        break;
                    }
                }
                if inside {
                    return at;
                }
            }
            return (col, row);
        }
        let mut live_boxes = [([(0i32, 0i32, 0u16); PET_MOTES_MAX], 0usize); MAX_TICKS];
        let mut live_ticks_cached = 0usize;
        let mut clear_of_them = None;
        let mut li = 0;
        while li < NOTE_LANES.len() {
            let (along, up) = NOTE_LANES[li];
            li += 1;
            let at = (col + dir * along * step, row - up);
            let safe_inside =
                safe_inside_at(at, max_dest, max_half, max_drift, cell_w_f, grid_w, grid_h);
            born.col = at.0;
            born.row = at.1;
            let mut meets = false;
            let mut inside = true;
            let mut k = 0u32;
            while k < ticks {
                let clock = self.clock + (k as f64) * INV_60;
                k += 1;
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
                    let half = ((b.2 as i32) + 1) / 2;
                    let cx = (centre.0 * cell_w_f).round() as i32;
                    inside &= cx - half >= 0 && cx + half <= grid_w;
                }
                if clock < max_live_end {
                    let k_idx = (k - 1) as usize;
                    if k_idx >= live_ticks_cached {
                        let (ref mut boxes, ref mut slot_len) = live_boxes[k_idx];
                        let mut n = 0usize;
                        let mut mj = 0;
                        while mj < live.len() {
                            let m = live[mj];
                            mj += 1;
                            if let Some((c, s)) = m.note_geom(clock)
                                && let Some(o) = mote_box_px(c, s, cell, grid)
                            {
                                boxes[n] = o;
                                n += 1;
                            }
                        }
                        *slot_len = n;
                        live_ticks_cached = k_idx + 1;
                    }
                    let (ref boxes, n) = live_boxes[k_idx];
                    let (bx1, by1) = (b.0 + (b.2 as i32), b.1 + (b.2 as i32));
                    let mut oi = 0;
                    while oi < n {
                        let o = boxes[oi];
                        oi += 1;
                        if b.0 < o.0 + (o.2 as i32)
                            && o.0 < bx1
                            && b.1 < o.1 + (o.2 as i32)
                            && o.1 < by1
                        {
                            meets = true;
                            break;
                        }
                    }
                }
                if meets || (!inside && clear_of_them.is_some()) {
                    break;
                }
            }
            if meets {
                continue;
            }
            if inside {
                return at;
            }
            if clear_of_them.is_none() {
                clear_of_them = Some(at);
            }
        }
        match clear_of_them {
            Some(at) => at,
            None => (col, row),
        }
    }

    #[inline]
    const fn cheer_air(
        &self,
        (col, row): (f32, f32),
        (rh, rw, step): (f32, f32, f32),
        width: f32,
        air: CheerAir,
    ) -> (f32, f32, f32, f32) {
        let drift = NOTE_BASE + NOTE_RUN + NOTE_SWAY;
        let bottom = row - 0.27 - 0.3 * NOTE_CLIMB + rh;
        match air {
            CheerAir::Lanes(facing_left) => {
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
        (u * (1.0 / 0.15)).min(1.0) * ((1.0 - u) * (1.0 / 0.35)).clamp(0.0, 1.0)
    }

    #[inline]
    const fn resolve_motes(&mut self) -> [Option<PetMoteSprite>; PET_MOTES_MAX] {
        let mut out = [None; PET_MOTES_MAX];
        let clock = self.clock;
        let mut i = 0;
        while i < PET_MOTES_MAX {
            let idx = i;
            i += 1;
            let Some(m) = self.motes[idx] else { continue };
            out[idx] = self.mote_sprite(&m, m.age(clock));
            if out[idx].is_none() {
                self.motes[idx] = None;
            }
        }
        out
    }

    /// The share of its look a dust puff at `col` keeps by the body that kicked it up: whole
    /// within [`DUST_HEEL_NEAR`] of the body's box, none from [`DUST_HEEL_FAR`] on, where the
    /// brain drops it ([`Self::mote_sprite`]) so it cannot come back when the body returns. A
    /// landing's or a skid's puff stays by the paws and lives its life. Reverted by
    /// [`OldPath::DustOutlivesTheHeel`].
    #[inline]
    const fn dust_by_the_heel(&self, col: f32) -> f32 {
        if self.old_path(OldPath::DustOutlivesTheHeel) {
            return 1.0;
        }
        let width = art_cols(self.last_cell_w, self.last_cell_h);
        let behind = if col < self.col {
            self.col - col
        } else {
            col - (self.col + width)
        };
        ((DUST_HEEL_FAR - behind) * (1.0 / (DUST_HEEL_FAR - DUST_HEEL_NEAR))).clamp(0.0, 1.0)
    }

    /// What mote `m` draws at age `u` (a fraction of its life), `None` once the brain drops it:
    /// its life spent, a z over ink, or a dust puff the body ran clear of.
    #[inline]
    const fn mote_sprite(&self, m: &Mote, u: f32) -> Option<PetMoteSprite> {
        if u < 0.0 || u >= 1.0 {
            return None;
        }
        let spin = if m.seed.is_multiple_of(2) { 1.0 } else { -1.0 };
        let seed_mod3 = (m.seed % 3) as f32;
        Some(match m.kind {
            PetMoteKind::Dust => {
                let col = m.col + m.dir * (0.25 + 0.85 * u) * (0.6 + 0.2 * seed_mod3);
                let kept = self.dust_by_the_heel(col);
                if kept <= 0.0 {
                    return None;
                }
                PetMoteSprite {
                    kind: m.kind,
                    col,
                    row: m.row - (0.08 + 0.22 * u),
                    scale: 0.5 + 1.3 * u,
                    rot: spin * (0.12 + 0.05 * seed_mod3 + 0.6 * u),
                    alpha: (Self::mote_fade_env(u) * kept * 235.0) as u8,
                    rgb: m.rgb,
                }
            }
            PetMoteKind::Zee | PetMoteKind::ZeePop => {
                // Faded at the pixel it draws at, as a z is: the fade turns only at the
                // pop's whole-pixel steps, which a hidden pop's offer walks.
                let (col, row) = m.drift(u);
                let (px_col, px_row) = self.mote_px_cell(col, row);
                let ink = self.zee_ink_fade(px_col, px_row);
                if ink <= 0.0 {
                    return None;
                }
                let (scale, rot, peak, env) = if matches!(m.kind, PetMoteKind::Zee) {
                    let su = look_slice_age(look_slice(u, ZEE_STEPS), ZEE_STEPS);
                    (
                        0.55 + 0.60 * su,
                        spin * (0.10 + 0.04 * seed_mod3 + 0.55 * su),
                        220.0,
                        Self::mote_fade_env(su),
                    )
                } else {
                    (
                        0.85 + 0.55 * u,
                        spin * (0.14 + 0.8 * u),
                        245.0,
                        Self::mote_fade_env(u),
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
                    rot: spin * (0.11 + 0.05 * seed_mod3 + 0.4 * su),
                    alpha: (Self::mote_fade_env(su) * 240.0) as u8,
                    rgb: m.rgb,
                }
            }
        })
    }

    #[inline]
    const fn next_departure_id(&mut self) -> u32 {
        self.departure_serial = self.departure_serial.wrapping_add(1);
        self.departure_serial
    }

    #[inline]
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

    #[inline]
    const fn resolve_departures(&mut self) -> [Option<PetDepartureSprite>; PET_DEPARTURES_MAX] {
        let mut out = [None; PET_DEPARTURES_MAX];
        let mut i = 0;
        while i < PET_DEPARTURES_MAX {
            let idx = i;
            i += 1;
            let Some(d) = self.departures[idx] else {
                continue;
            };
            let t = (self.lane_clock - d.born) as f32;
            let life = d.life();
            if t < 0.0 || t >= life {
                self.departures[idx] = None;
                continue;
            }
            let alpha = d.envelope(t);
            let (col, pose, facing_left, stance) = d.look_at(t);
            let own_glass = d.own_glass.is_some();
            let glass = match d.own_glass {
                Some(g) => g,
                None => 0,
            };
            out[idx] = Some(PetDepartureSprite {
                pose: self.species.skin(pose),
                col,
                row: d.row,
                facing_left,
                scale_x: stance.scale_x,
                scale_y: stance.scale_y,
                lift: stance.lift,
                stretch_from: stance.from,
                alpha: (alpha * 255.0) as u8,
                coat: d.coat,
                iris: d.iris,
                id: d.id,
                under_ink: own_glass,
                own_glass,
                glass,
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
        self.action_began = true;
    }

    #[inline]
    const fn set_action_keep(&mut self, a: PetAction) {
        if a as u8 != self.action as u8 {
            self.action = a;
            self.action_t = 0.0;
            self.mote_mark = 0;
            self.action_began = true;
        }
    }

    #[inline]
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
                    let k = (self.action_t * (1.0 / STRETCH_LEAD)).min(1.0);
                    scale_x *= 1.0
                        + STRETCH_AMP * crate::supernova::sin_cos_f32(core::f32::consts::PI * k).0;
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
                let beat = ((WRIGGLE_DUR - self.wriggle_t) * (1.0 / WRIGGLE_BEAT)) as u32;
                let u = (WRIGGLE_DUR - self.wriggle_t) * 9.0;
                scale_y *= 1.0 + 0.04 * crate::supernova::sin_cos_f32(u).0;
                match beat & 3 {
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
                let u = (1.0 - self.pet_hold_t * (1.0 / PET_HOLD)).clamp(0.0, 1.0);
                let warm = self.content >= PURR_GATE;
                let waves = if warm { 2.0 } else { 1.0 };
                let sin_u = crate::supernova::sin_cos_f32(core::f32::consts::PI * waves * u).0;
                let lean = sin_u * sin_u;
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
                    let u = (into - k * cycle) * (1.0 / GROOM_DUR);
                    if (u < 0.6) != ((k as i64) & 1 != 0) {
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
                if ((self.action_t * (1.0 / (FROLIC_HOLD * 0.5))) as u32).is_multiple_of(2) {
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
                if let Some(mut f) = self.flight {
                    // The head law in flight on the page as it is: output that lands under a
                    // bound already in the air (a prompt's blank line and the command's last
                    // line two rows above it, printed after the Return) lowers its arc, and
                    // nothing raises it again ([`OldPath::ArcJudgedInTheAir`]).
                    let arc = if self.old_path(OldPath::ArcJudgedInTheAir) {
                        f.arc
                    } else {
                        self.arc_under_letters(f.to_col, f.to_row, width, f.arc)
                    };
                    if arc < f.arc {
                        f.arc = arc;
                        if let Some(live) = self.flight.as_mut() {
                            live.arc = arc;
                        }
                    }
                    let u = (f.t / f.dur).clamp(0.0, 1.0);
                    lift = Self::flight_lift(f.arc, u, f.big);
                    pose = Self::flight_pose(&f);
                    if !f.drop {
                        let stage = Self::flight_stage(pose);
                        if stage < f.stage {
                            pose = Self::stage_pose(f.stage, f.hop);
                        } else if let Some(live) = self.flight.as_mut() {
                            live.stage = stage;
                        }
                    }
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
                        scale_x *=
                            1.0 + 0.03 * crate::supernova::sin_cos_f32(TAU * 3.0 * elapsed).0;
                    }
                    (SCRABBLE_DUR, LAND_SQUASH, PetGlyphId::PetLand)
                } else if self.land_span > 0.0 {
                    (
                        BIG_LAND_DUR,
                        LAND_SQUASH
                            * (BIG_SQUASH_NEAR
                                + (1.0 - BIG_SQUASH_NEAR)
                                    * (self.land_span * (1.0 / BIG_SQUASH_SPAN)).min(1.0)),
                        PetGlyphId::PetLand,
                    )
                } else {
                    (LAND_DUR, LAND_SQUASH, PetGlyphId::PetLand)
                };
                let u = 1.0 - (self.land_t / dur).clamp(0.0, 1.0);
                let a = ((dur - self.land_t) * (1.0 / LAND_ATTACK)).clamp(0.0, 1.0);
                let q = q0
                    * a
                    * crate::color_math::exp_f32(-3.0 * u)
                    * crate::supernova::sin_cos_f32(TAU * 1.35 * u).1;
                scale_y *= 1.0 - q;
                scale_x *= 1.0 + q * 0.6;
                pose
            }
            PetAction::Walk | PetAction::Run
                if matches!(
                    self.brake_over,
                    Some(over) if (over - self.col) * self.speed >= 0.0
                ) =>
            {
                let over = self.brake_over.expect("checked by the guard");
                let w = ((over - self.col).abs() * (1.0 / BRAKE_OVERSHOOT)).clamp(0.0, 1.0);
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
            let q = FLINCH_SQUASH * (self.flinch_t * (1.0 / FLINCH_DUR)).clamp(0.0, 1.0);
            scale_y *= 1.0 - q;
            scale_x *= 1.0 + q * 0.6;
        }
        if self.stumble_t > 0.0 {
            let q = 0.18 * (self.stumble_t * (1.0 / STUMBLE_DUR)).clamp(0.0, 1.0);
            scale_y *= 1.0 - q;
            scale_x *= 1.0 + q * 0.7;
        }
        if self.action == PetAction::Walk && self.content >= SKIP_CONTENT {
            let whole = if (0.0..4.0).contains(&self.stride) {
                self.stride
            } else {
                self.stride - (self.stride * 0.25).floor() * 4.0
            };
            if whole < 1.0 {
                lift += 0.07
                    * crate::supernova::sin_cos_f32(whole * core::f32::consts::PI)
                        .0
                        .max(0.0);
            }
        }

        if self.twitch_t > 0.0 && !ear_frame {
            let q = TWITCH_BOB * (self.twitch_t * (1.0 / TWITCH_DUR)).clamp(0.0, 1.0);
            scale_y *= if self.twitch_up { 1.0 + q } else { 1.0 - q };
        }

        let ramp = self.arrive_t > 0.0;
        let ramp_done = if ramp {
            1.0 - (self.arrive_t / self.arrive_in).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let arrival = if ramp {
            let from = self.arrive_from.clamp(0.0, 1.0);
            (from + (1.0 - from) * ramp_done).max(ARRIVE_FLOOR)
        } else {
            1.0
        };
        if let Some((held_lift, _)) = self.retired_flight_lift {
            lift += held_lift;
        }
        let own = (pose, self.facing_left);
        let (col, row, action) = (self.col, self.row, self.action);
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
                            && match look.beneath {
                                Some(b) => b == own,
                                None => true,
                            })) =>
            {
                look.beneath = Some(own);
                let own_stance = Stance {
                    scale_x,
                    scale_y,
                    lift,
                    from: StretchFrom::Centre,
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
        let (lift, fit) = self.sunk(lift, sense, width);
        self.last_pose = pose;
        self.last_facing = facing_left;
        let alpha_clamped = self.alpha.clamp(0.0, 1.0);
        let mut frame = PetFrame {
            alpha: (alpha_clamped * arrival * 255.0) as u8,
            lane_alpha: (alpha_clamped * 255.0) as u8,
            action: self.action,
            pose: self.species.skin(pose),
            col: self.col + self.fitted_to_the_wall(fit, sense.cols, width),
            row: self.row,
            lift,
            facing_left,
            scale_x,
            scale_y,
            stretch_from: StretchFrom::Centre,
            purr,
            contented: self.content >= PURR_GATE,
            under_ink: self.hiding,
            motes: self.resolve_motes(),
            departures: self.resolve_departures(),
        };
        self.deal_front_pass(frame.action);
        self.finish_console_frame(&mut frame, sense);
        self.touched_on_chrome = false;
        self.hold_still_pose(&mut frame);
        // The head law's fit ([`Self::head_target`]), on the pose the frame holds, before the
        // letter veil judges the box the frame draws; a box the law keeps in its row, fits, or
        // draws down no taller ([`Self::drawn_no_taller`]).
        if self.drawn_no_taller() {
            frame.scale_y = frame.scale_y.min(1.0);
        }
        if fit < 1.0 {
            frame.scale_x *= fit;
            frame.scale_y *= fit;
        }
        self.keep_stretch_off_caret(&mut frame, &sense);
        self.keep_stretch_off_letters(&mut frame, &sense, width);
        self.veil_over_letters(&mut frame, sense);
        if self.wall_cut < 1.0 {
            frame.alpha = (f32::from(frame.alpha) * self.wall_cut).round() as u8;
            frame.lane_alpha = (f32::from(frame.lane_alpha) * self.wall_cut).round() as u8;
        }
        // The fade has run out on whatever path the tick took: the cut is spent, and the body
        // comes back whole from its fade-in.
        if self.alpha <= 0.0 {
            self.wall_cut = 1.0;
            self.wall_cut_falls = false;
        }
        const INV_255: f32 = 1.0 / 255.0;
        self.last_presence = f32::from(frame.alpha) * INV_255;
        self.last_stance = Stance::of(&frame);
        let lane_alpha_norm = f32::from(frame.lane_alpha) * INV_255;
        let mut i = 0;
        while i < PET_DEPARTURES_MAX {
            self.last_drawn[i] = match frame.departures[i] {
                Some(d) => f32::from(d.alpha) * INV_255 * lane_alpha_norm,
                None => 0.0,
            };
            i += 1;
        }
        frame
    }

    /// THE BOX KEEPS OFF THE ROWS BESIDE ITS OWN ([`head_law`]). The body stands [`ART_ROWS`]
    /// tall, so a cat on a row reaches [`ART_ROWS`] − 1 of a row into the row above its feet:
    /// under a command's output, a prompt's usual place, its ears and back lay on the output's
    /// descenders for a line's typing (`gjpqy` read 1.6:1 under the dog, looks9's review).
    /// Where that row holds ink over the body's columns, the cat is drawn [`SINK_ROWS`] down its
    /// own row where the row under its feet holds none under them (its head in its own row, its
    /// feet in the blank row under), and fitted to its own row ([`ROW_FIT`]) where the row under
    /// holds ink too, while every law that places it (its stand, its seat, its run, a wrap's one
    /// bound) keeps its row. Read on the body's `row` over the columns `a..b`: the body's where
    /// it stands or lands, and a bound's whole way along its row, so a leap under a line of
    /// output is drawn clear of it all the way. Judged where the body stands or lands, and again
    /// only when that or its columns move ([`Self::sink_seat`]); `None` where the page is not
    /// read there yet. The grid's last row has no row under to take the body down, so it is
    /// read as inked there and a body under a lettered row is fitted to its own row: once a
    /// screen fills, every prompt sits there, and the box lay on the output's letters directly
    /// above it in every frame (review9fix round 3's review: 211 of 211, letters on Solarized
    /// Dark at 2.00:1 against their own 4.51). Not where the console places the body itself
    /// ([`Self::console_places_body`]). The sink is reverted by [`OldPath::HeadUnderLetters`],
    /// the fit by [`OldPath::RowFit`].
    #[inline]
    const fn head_target(
        &self,
        row: f32,
        (rows, cols): (u16, u16),
        (a, b): (f32, f32),
    ) -> Option<HeadRead> {
        const OFF: HeadRead = HeadRead {
            law: (0.0, 1.0),
            gap_above: false,
            blank_below: false,
        };
        if self.console_places_body() || !a.is_finite() || !b.is_finite() || b <= a {
            return Some(OFF);
        }
        let r = row.round();
        if (row - r).abs() > 0.05 || r < 1.0 || r >= rows as f32 {
            return Some(OFF);
        }
        // A body slid off the line at the margin counts the line as lettered from the first
        // ([`Self::margin_seat_off_the_line`]): the writing is on its way along it.
        let (line_above, line_below) = match self.wrap_ahead {
            Some(WrapAhead::OffTheLine { row, over, .. }) if (row - r).abs() < 0.5 => (!over, over),
            _ => (false, false),
        };
        let below = if line_below || r + 1.0 >= rows as f32 {
            Some(true)
        } else {
            self.observed_ink_in_span(r + 1.0, a, b)
        };
        let above = if line_above {
            Some(true)
        } else {
            self.observed_ink_in_span(r - 1.0, a, b)
        };
        match (above, below) {
            (Some(above), Some(below)) => {
                let (sink, fit) = head_law(above, below);
                Some(HeadRead {
                    law: (
                        if self.old_path(OldPath::HeadUnderLetters) {
                            0.0
                        } else {
                            sink
                        },
                        if self.old_path(OldPath::RowFit) {
                            1.0
                        } else {
                            fit
                        },
                    ),
                    gap_above: !above && cols > 0 && self.gap_above(r - 1.0, cols, (a, b)),
                    blank_below: !below,
                })
            }
            _ => None,
        }
    }

    /// Whether the row above (`above`, read blank over the body's columns `a..b`) is a gap the
    /// head law holds a body across ([`HeadRead::gap_above`]): ink on it before the columns and
    /// ink after them, a word on either side. Ink on one side alone is a line that has ended
    /// (the blank, or a background's fill, past a prompt segment's or an echo's last letter),
    /// no gap: a body held fitted there stayed a token to the row's end. Reverted by
    /// [`OldPath::LineEndIsNoGap`]: any ink along the row.
    ///
    /// Each side is read from the body's edge as far as the window that reads the cell beside
    /// it reaches ([`Self::read_about`], [`OldPath::GapReadAboutTheBody`]): on a pane past the
    /// map's columns a side longer than one window reads ([`crate::pet_world::MAX_WORLD_COLS`])
    /// is read that far, not refused, so a word past it is the gap's; ink further off than that
    /// window reads is not read, and counts as none. Reverted, such a side read nothing and was
    /// no ink: no gap, and the law let a body there rise whole in each gap as if it were reverted
    /// itself (ATERM-KI-1107-3).
    #[inline]
    const fn gap_above(&self, above: f32, cols: u16, (a, b): (f32, f32)) -> bool {
        let cols = cols as f32;
        if self.old_path(OldPath::LineEndIsNoGap) {
            return matches!(self.observed_ink_in_span(above, 0.0, cols), Some(true));
        }
        let about = !self.old_path(OldPath::GapReadAboutTheBody);
        let before = a >= 1.0 && {
            let (x, y) = if about {
                self.read_about(above, (0.0, a), (a - 1.0, a))
            } else {
                (0.0, a)
            };
            matches!(self.observed_ink_in_span(above, x, y), Some(true))
        };
        let after = b + 1.0 <= cols && {
            let (x, y) = if about {
                self.read_about(above, (b, cols), (b, b + 1.0))
            } else {
                (b, cols)
            };
            matches!(self.observed_ink_in_span(above, x, y), Some(true))
        };
        before && after
    }

    /// The lift and fit a frame draws with the head law ([`Self::head_target`]) under it: a hop
    /// along a sunk or fitted cat's row, or a skip in its walk, rises no further than keeps its
    /// box's top under the row above's edge, so its box keeps off the row above in the air as
    /// on the ground (the first keys' spring under `gjpqy` lifted the cat's ears into the
    /// descenders). A flight to another row keeps its arc, but one down off a row the law held
    /// the body on keeps its box's top where it stood at the take-off ([`Self::left_top`]) until
    /// the row it comes down to carries it clear. Where the law keeps the box in its
    /// row this frame ([`Self::head_holds`]) the frame draws it no taller than its fit, as it
    /// draws a fitted body ([`Self::drawn_no_taller`], [`OldPath::HeldNoTaller`]): the box's
    /// top lies 0.08 of a row under the row above's edge, and a leap's take-off stretch on a
    /// cat fitted between two lettered rows put its ears 3 px into the letters above (the
    /// `steam` replay typed at the window).
    #[inline]
    fn sunk(&mut self, lift: f32, sense: PetSense, width: f32) -> (f32, f32) {
        let (sink, fit) = self.ease_head(sense, width);
        let lift = match self.flight {
            Some(Flight {
                left_top: Some(top),
                from_row,
                to_row,
                ..
            }) if to_row - from_row >= 0.5 => {
                (lift - sink).min(self.row + 1.0 - ART_ROWS * fit - top)
            }
            _ if self.head_holds() => lift.min((sink + 1.0 - ART_ROWS * fit).max(0.0)) - sink,
            _ => lift - sink,
        };
        (lift, fit)
    }

    /// THE CROSSING KEEPS UNDER THE LINE IT LEFT. A body the head law holds down or fits to its
    /// row (the margin's seat under the line the writing wraps off, [`WrapAhead::OffTheLine`])
    /// may leave it for a whole seat a row lower, along the wrap's new line to a stand over a
    /// status row. Such a flight is drawn whole in the air ([`OldPath::SinkAtTheLanding`]), and
    /// from that row the whole body's top is in the line over it: the crossing went up through
    /// the letters it had just been kept off (the trail gallery's `focus-wrap`, every pet-family
    /// frame from the leap to the landing). So its box's top rises no higher than it stood at
    /// the take-off, until the row the flight has come down to carries it clear: the sink comes
    /// off only as the body comes down, and the arc begins past that. This is where it stood;
    /// `None` for a body that left whole, whose arc is the crossing's own, or that is held by a
    /// reverted law. Reverted by [`OldPath::CrossingRisesIntoTheLine`].
    #[inline]
    const fn left_top(&self) -> Option<f32> {
        if self.sink_to > 0.0 || self.fit_to < 1.0 {
            if self.old_path(OldPath::CrossingRisesIntoTheLine) {
                return None;
            }
            Some(self.row + 1.0 - ART_ROWS * self.fit + self.sink)
        } else {
            None
        }
    }

    /// How far right (cells) a fitted body is drawn inside its whole box by the margin. A body is
    /// fitted between lettered rows ([`head_law`]) and drawn at the middle of its whole box, whose
    /// left edge the wall holds [`art_cols`] from the grid's end: by the margin of a full page's
    /// last row that laid it across the caret and the line's last letter, a crumb behind a block
    /// caret with its haunch on the letter (the trail gallery's `last-row`). Over the last of its
    /// way to the wall, one body's width, it moves to its whole box's right edge, where the grid
    /// ends and the writing comes last, in proportion to the way it has come. It reads neither the
    /// caret nor the keys, so it goes with the body and the fit and never jumps. Reverted by
    /// [`OldPath::FitCentredAtTheWall`].
    #[inline]
    const fn fitted_to_the_wall(&self, fit: f32, cols: u16, width: f32) -> f32 {
        self.fitted_to_the_wall_at(self.col, fit, cols, width)
    }

    /// [`Self::fitted_to_the_wall`] for a body standing at `col`.
    #[inline]
    const fn fitted_to_the_wall_at(&self, col: f32, fit: f32, cols: u16, width: f32) -> f32 {
        if fit >= 1.0 || width <= 0.0 || self.old_path(OldPath::FitCentredAtTheWall) {
            return 0.0;
        }
        let limit = (cols as f32 - width).max(0.0);
        let along = ((col - (limit - width)) / width).clamp(0.0, 1.0);
        width * (1.0 - fit) * 0.5 * along
    }

    /// THE WALL THAT HOLDS NO SEAT HIDES THE BODY. Writing that runs into the margin of a row
    /// with no seat off the line ([`Self::margin_seat_off_the_line`]) and none at rest within
    /// reach ([`Self::margin_rest_seat`]) keeps the body at the wall on the line
    /// ([`WrapAhead::Approach`]). On a full page's last row the row over it is lettered and none
    /// lies under, so the head law fits the body to its row ([`head_law`]) and the wall draws it
    /// at its whole box's right edge ([`Self::fitted_to_the_wall`]). A line that ends nearer the
    /// grid's end than that box is wide runs its last letters under the box, and one that reaches
    /// the end stands the caret on its last letter there. No seat holds the body clear of them,
    /// and a smaller body would not read at 1x. Drawn there, under the letters, it was a cat
    /// tucked behind the block caret and then crumbs between the last letters, for as long as the
    /// line rested (Linux arm64, 2026-11-07: from two blank cells past the line on a 13 x 29 px
    /// cell, its art lay on 1 to 166 px of their ink).
    ///
    /// So this reads, afresh each tick, where the margin holds such a body: a row beside the line
    /// ([`Self::margin_seat_off_the_line`]), a seat within reach ([`Self::margin_rest_seat`],
    /// read whether or not the hand has stopped), or the wall. The wall holds no seat
    /// ([`WallSeat::NoSeat`]) where the box a body draws at rest there ([`Self::rest_box_inked`])
    /// meets a lettered cell of the caret's row and the head law seats it nowhere there either
    /// ([`Self::head_seat_at`]: the whole body's footprint, [`INK_PAD`] in from each edge, on no
    /// ink and its head under no letter). Then the tick takes the caret as hidden
    /// ([`Self::unseated_sense`]). The box is judged at the wall, not where the body has got to
    /// on its way there along the caret's row, so a body that has not reached the wall yet fades
    /// from the key that leaves the wall no seat, not from its own arrival there.
    ///
    /// The margin's hold is read here and not taken from [`Self::wrap_ahead`] alone: under a hide
    /// the tick sees no caret and leaves the margin's last hold as it was, so a seat that opens
    /// beside the line while the body is out is found here, and the body comes back on it (the
    /// trail gallery's row over the line cleared brings it back the same with this reading
    /// dropped, a later one seating it there too: no replayed scene decides it, nor the slack
    /// below, nor the hold a return keeps; each is the margin's own, Linux arm64, 2026-11-07).
    /// `slack` is how far short of the margin the caret may stand ([`Self::at_the_margin`]): none
    /// to begin a hide, [`WALL_RETURN_MARGIN`] while one holds, the slack a taken margin keeps.
    ///
    /// The seat the caret's cell's law finds anywhere on the screen ([`Self::seat_off_the_line`],
    /// [`OldPath::CaretCellIsNoSeat`]) is not read here. A body that law takes there flies to it
    /// from the wall once the hand's rhythm closes, which a hand still writing does not allow; one
    /// the hide has already taken off the glass would come back there rows from where it left,
    /// as the rhythm closes ([`RHYTHM_WINDOW`] past the hand's last key): a dip and a return with
    /// no motion between, which reads as a blink. So the hide holds it out to the line's end, as
    /// it does where no seat lies anywhere.
    ///
    /// `None` where the margin does not hold the body at the wall on the caret's row (in flight,
    /// off that row, the margin untaken or holding it elsewhere), where the letter reader
    /// certifies nothing of the caret's row there or of the row over the body's head, where the
    /// console places the body itself, and with the law reverted
    /// ([`OldPath::NoSeatHidesTheBody`]).
    #[inline]
    fn wall_seat(
        &self,
        (cr, cc): (u16, u16),
        sense: &PetSense,
        width: f32,
        slack: f32,
    ) -> Option<WallSeat> {
        let (rows, cols) = (sense.rows, sense.cols);
        let row = f32::from(cr);
        let limit = (f32::from(cols) - width).max(0.0);
        if self.old_path(OldPath::NoSeatHidesTheBody)
            || self.console_places_body()
            || sense.cell_w == 0
            || sense.cell_h == 0
            || !width.is_finite()
            || width <= 0.0
            || self.flight.is_some()
            || !matches!(self.wrap_ahead, Some(WrapAhead::Approach))
            || (self.row - row).abs() >= 0.5
            || !self.at_the_margin((cr, cc), cols, width, slack)
        {
            return None;
        }
        if let Some((r, over)) = self.margin_seat_off_the_line(cr, (rows, cols), limit, width) {
            let ahead = WrapAhead::OffTheLine {
                row: r,
                over,
                kept: WALL_RETURN_MARGIN,
            };
            return Some(WallSeat::Seat((limit, r), ahead));
        }
        if let Some(seat) = self.margin_rest_seat((cr, cc), (rows, limit), width) {
            return Some(WallSeat::Seat(seat, WrapAhead::Rest(seat.0, seat.1)));
        }
        let grid = (sense.cols, sense.rows);
        Some(
            if self.wall_unseated((cr, cc), ((sense.cell_w, sense.cell_h), grid), width)? {
                WallSeat::NoSeat
            } else {
                WallSeat::Seat((limit, row), WrapAhead::Approach)
            },
        )
    }

    /// Whether the wall on the caret's row holds a body no seat of its own ([`Self::wall_seat`]):
    /// the box it draws at rest there ([`Self::rest_box_inked`], on the `cells` px cells of the
    /// grid they give) meets a lettered cell of the caret's row, and the head law seats it
    /// nowhere there either ([`Self::head_seat_at`]). Seats beside the line or within reach are
    /// not read here. `None` where the letter reader certifies nothing of the caret's row there or
    /// of the row over the body's head.
    #[inline]
    fn wall_unseated(
        &self,
        (cr, cc): (u16, u16),
        cells: ((u16, u16), (u16, u16)),
        width: f32,
    ) -> Option<bool> {
        let (_, (cols, rows)) = cells;
        let row = f32::from(cr);
        let limit = (f32::from(cols) - width).max(0.0);
        let wall = (limit, row);
        let inked = self.rest_box_inked(wall, cells, width)?;
        let head = head_seat_from(limit, width, !self.old_path(OldPath::SeatClearsTheStretch));
        if row >= 0.5
            && self
                .observed_last_letter(row - 1.0, head, limit + width - INK_PAD)
                .is_none()
        {
            return None;
        }
        Some(inked && !self.head_seat_at(wall, (cr, cc), rows, width))
    }

    /// Whether the box a body standing at `(col, row)` draws at rest there
    /// ([`Self::rest_box_cols`], on the `cells` px cells of the grid they give) meets a lettered
    /// cell of `row`. `None` where the letter reader certifies nothing there.
    #[inline]
    fn rest_box_inked(
        &self,
        (col, row): (f32, f32),
        cells: ((u16, u16), (u16, u16)),
        width: f32,
    ) -> Option<bool> {
        let (x0, x1) = self.rest_box_cols((col, row), cells, width)?;
        self.observed_ink_in_span(row, x0, x1)
    }

    /// The columns (cells, `x0..x1`) the box a body standing at `(col, row)` draws at rest there
    /// spans ([`Self::rest_box_inked`]): its fit, at its place in its whole box by the wall
    /// ([`Self::fitted_to_the_wall`]), no pose's stretch, on `cell_w x cell_h` px cells of a
    /// `cols x rows` grid. `None` on a cell of no size.
    ///
    /// Each edge is its px over the cell's width, a division, so an edge on a cell's edge is
    /// that column exactly. Taken as px times the width's reciprocal, an edge on the grid's end
    /// fell a rounding past it at 158 of the widths 20 to 256 on 14 px cells and 94 on 13 px
    /// (1456 x (1/14) is 104.0000076 in `f32`), a read of a box flush with the wall
    /// ([`Self::observed_ink_in_span`]) took in a column past the grid and read nothing, and the
    /// wall's law, the seat kept off the line and a gap's keep read nothing at that wall: on
    /// 14 x 26 px cells, the line 100 and 104 cells long (grids of 99 to 108 columns,
    /// `last_row_after`), the body stood on the line's last letters through its rest in 188 to
    /// 194 frames where the line left no seat, where the law fades it in 28, and walked after the
    /// caret onto them from the first of ten Left arrows in 213 and 215 where the line seated it,
    /// where the seat keeps it in none (13 x 29 and 11 x 28 the same there as at the gallery's
    /// width, Linux arm64, 2026-11-08).
    #[inline]
    fn rest_box_cols(
        &self,
        (col, row): (f32, f32),
        ((cell_w, cell_h), (cols, rows)): ((u16, u16), (u16, u16)),
        width: f32,
    ) -> Option<(f32, f32)> {
        if cell_w == 0 || cell_h == 0 {
            return None;
        }
        let (x0, x1, ..) = drawn_body_px(
            (
                col + self.fitted_to_the_wall_at(col, self.fit, cols, width),
                row,
            ),
            Stance {
                scale_x: self.fit,
                scale_y: self.fit,
                lift: 0.0,
                from: StretchFrom::Centre,
            },
            (cell_w, cell_h),
            (cols, rows),
        );
        let cw = f32::from(cell_w);
        Some((x0 as f32 / cw, x1 as f32 / cw))
    }

    /// The sense the tick goes on with ([`Self::wall_seat`]), and the stand a body the law hid
    /// comes back at this tick. The caret is hidden from the tick where the wall holds the body
    /// no seat ([`WallSeat::NoSeat`]), and the hide is bound to that line's row:
    /// - The caret on another row (an Enter or a wrap that moves it down, a program's move) ends
    ///   it at once: the line it hid the body from is over, and the tick takes the caret as it
    ///   takes any caret seen again, the body fading back in from where its fade has got to, or
    ///   standing by the caret ([`Self::station_safe`]) where it has run out.
    /// - On the row, it holds until its fade has run out ([`FADE_OUT`]) and a stand clear of the
    ///   letters has come ([`Self::unseated_return`]); then the caret is shown again, the tick
    ///   stands the body there (`Some`), and the margin holds it there as it found it. No tick
    ///   whose wall still holds no seat brings it back: the margin keeps the body at that wall, so
    ///   a stand by the caret clear of the letters there is one the next tick hides again (stood
    ///   at it, a line whose last letters follow a run of spaces had the body blink back and out
    ///   every 64 ms from its fade's end to the Enter, Linux arm64, 2026-11-07). An Enter
    ///   or a wrap on a page's last row scrolls the line away and leaves the caret on its row:
    ///   the new line's stand by the caret is that stand.
    #[inline]
    fn unseated_sense(&mut self, sense: PetSense, width: f32) -> (PetSense, Option<(f32, f32)>) {
        let Some(caret) = sense.caret else {
            return (sense, None);
        };
        if self.unseated.is_some_and(|row| row != caret.0) {
            self.unseated = None;
        }
        let slack = if self.unseated.is_some() {
            WALL_RETURN_MARGIN
        } else {
            0.0
        };
        let seat = self.wall_seat(caret, &sense, width, slack);
        if matches!(seat, Some(WallSeat::NoSeat)) {
            self.unseated = Some(caret.0);
        }
        let held = match seat {
            Some(WallSeat::Seat(stand, ahead)) => Some((stand, ahead)),
            _ => None,
        };
        if self.unseated.is_some()
            && self.alpha <= 0.0
            && !matches!(seat, Some(WallSeat::NoSeat))
            && let Some((stand, ahead)) = self.unseated_return(caret, &sense, width, held)
        {
            self.unseated = None;
            if ahead.is_some() {
                self.wrap_ahead = ahead;
            }
            return (sense, Some(stand));
        }
        if self.unseated.is_some() {
            (
                PetSense {
                    caret: None,
                    ..sense
                },
                None,
            )
        } else {
            (sense, None)
        }
    }

    /// Where a body the wall's law hid comes back on its line's row ([`Self::unseated_sense`]),
    /// its fade run out, and the margin's hold that keeps it there: the first stand clear of the
    /// letters, whichever reading finds it, in a tick whose wall holds a seat or does not hold the
    /// body there at all (the caller asks nothing in a tick whose wall holds no seat).
    /// - While the margin holds it at the wall on the caret's row (`held`, read by
    ///   [`Self::wall_seat`] with the slack a taken margin keeps), where the margin holds it now: a
    ///   row beside the line opened while it was out, a seat within reach, or the wall, once the
    ///   box it draws there at rest meets no letter (the line erased short of it). The stand the
    ///   caret gives there ([`Self::station_safe`]) lies on the line's last letters where they run
    ///   on to it (the margin's caret takes the wall side): read by it alone, the body stayed out
    ///   to the Enter though the erase had left it its seat (Linux arm64, 2026-11-07: three
    ///   Backspaces from a line three blank cells or fewer short of the grid's end, on every face,
    ///   back 20 ms past the Enter, not 20 to 24 ms past the Backspace that left the seat).
    /// - Anywhere else on the row (the caret moved back into the line, the line erased past the
    ///   margin, a page's last row scrolled by an Enter or a wrap), the stand the caret gives
    ///   ([`Self::station_safe`]), once no lettered cell of its row lies under the body's whole
    ///   width there.
    ///
    /// `None` while neither is clear: the caret moved back into a line that runs on to the
    /// margin under a lettered row leaves no stand by it off the letters, and the body stays out
    /// until the line is erased or entered (stood by the caret whatever lay under it, it came
    /// back 32 ms after its fade ran out and lay on the line's letters in 249 to 256 frames from
    /// there to the Enter, ten Left arrows walking the caret back into the line on the way, on
    /// every face). Where the letter reader certifies nothing, the stand counts as clear.
    #[inline]
    fn unseated_return(
        &self,
        (cr, cc): (u16, u16),
        sense: &PetSense,
        width: f32,
        held: Option<((f32, f32), WrapAhead)>,
    ) -> Option<((f32, f32), Option<WrapAhead>)> {
        if let Some((stand, ahead)) = held {
            return Some((stand, Some(ahead)));
        }
        let (col, r) = self.station_safe((cr, cc), sense.cols, sense.rows, width);
        if matches!(self.observed_ink_in_span(r, col, col + width), Some(true)) {
            None
        } else {
            Some(((col, r), None))
        }
    }

    /// THE LETTERS CUT THE WALL'S FADE. Where the wall holds no seat ([`Self::wall_seat`]) the
    /// tick hides the caret and the body fades out where it stands ([`FADE_OUT`]), and the line's
    /// letters keep coming under it: on a full page's last row its box lay on them for the whole
    /// fade, 28 frames at 60 Hz at an alpha falling from just under full, its art on up to 161 px
    /// of their ink and in the caret's cell where the line ran to the grid's end (the trail
    /// gallery's `last-row`, the cat and the dog on dark, Linux arm64, 2026-11-09). So from the
    /// tick the box the body draws at rest where it stands ([`Self::rest_box_meets_the_line`])
    /// meets a lettered cell of its row or the caret's cell while the hide holds, the frame draws a
    /// share of the body that falls to none over [`FADE_OFF_THE_LETTERS`] ([`Self::emit`]). A hide
    /// whose box meets neither keeps the whole fade.
    ///
    /// The cut is the frame's, not the fade's: the fade runs on under it, and every law that reads
    /// the fade reads it as before. The body comes back where and when it did (its return waits on
    /// the fade's end, [`Self::unseated_return`]), and a wrap that takes the caret to the next row
    /// while the fade runs still bounds from the wall: Claude Code's composer wraps the writing 37
    /// to 87 ms past such a hide's first frame, the cut run out before some of those wraps, and
    /// each was a bound (Linux arm64, 2026-11-09). There the cut lifts as the body fades back in
    /// ([`FADE_IN`]); once the fade has run out the body is off the glass and the cut is spent. A
    /// program's cursor hide does not hold a body the cut takes off at its seat on the letters
    /// ([`Self::tick_console_resident`]).
    /// Reverted by [`OldPath::LettersCutTheFade`].
    #[inline]
    fn cut_on_the_letters(
        &mut self,
        caret: Option<(u16, u16)>,
        sense: &PetSense,
        width: f32,
        move_dt: f32,
    ) {
        if self.unseated.is_none() || self.alpha <= 0.0 || self.old_path(OldPath::LettersCutTheFade)
        {
            self.lift_the_wall_cut(move_dt);
            return;
        }
        if !self.wall_cut_falls
            && let Some(caret) = caret
            && self.rest_box_meets_the_line(caret, sense, width)
        {
            self.wall_cut_falls = true;
        }
        if self.wall_cut_falls {
            self.wall_cut = (self.wall_cut - move_dt * (1.0 / FADE_OFF_THE_LETTERS)).max(0.0);
        }
    }

    /// The wall's cut in a tick its hide does not hold ([`Self::cut_on_the_letters`]): the hide
    /// ended (or the console's resident drew the tick, [`Self::tick_console_resident`]), it lifts
    /// as the body fades back in ([`FADE_IN`]); the fade run out, or its law reverted, it is spent.
    #[inline]
    fn lift_the_wall_cut(&mut self, move_dt: f32) {
        self.wall_cut_falls = false;
        self.wall_cut = if self.alpha <= 0.0 || self.old_path(OldPath::LettersCutTheFade) {
            1.0
        } else {
            (self.wall_cut + move_dt * (1.0 / FADE_IN)).min(1.0)
        };
    }

    /// Whether the box the body draws at rest where it stands ([`Self::rest_box_cols`]) meets a
    /// lettered cell of its row or the caret's cell: `false` where the letter reader certifies
    /// nothing there and the caret lies outside it.
    #[inline]
    fn rest_box_meets_the_line(&self, (cr, cc): (u16, u16), sense: &PetSense, width: f32) -> bool {
        let cells = ((sense.cell_w, sense.cell_h), (sense.cols, sense.rows));
        let Some((x0, x1)) = self.rest_box_cols((self.col, self.row), cells, width) else {
            return false;
        };
        let caret = f32::from(cc);
        ((f32::from(cr) - self.row).abs() < 0.5 && caret < x1 && caret + 1.0 > x0)
            || matches!(self.observed_ink_in_span(self.row, x0, x1), Some(true))
    }

    /// Whether the head law keeps the box in its row this frame ([`Self::sunk`]): it draws the
    /// body down its row or fits it there, and the body stands on that row or bounds along it,
    /// or it flies down off a row the law held it on ([`Self::left_top`]).
    #[inline]
    const fn head_holds(&self) -> bool {
        let along = match &self.flight {
            Some(f) => {
                if (f.to_row - f.from_row).abs() >= 0.5 {
                    return f.to_row > f.from_row && f.left_top.is_some();
                }
                true
            }
            None => true,
        };
        (self.sink_to > 0.0 || self.fit_to < 1.0) && along
    }

    /// Whether a frame draws the pose no taller than its fit ([`OldPath::HeldNoTaller`]): the
    /// head law holds the body in its row or fits it, or draws it down, a flight to or from
    /// another row's first and last frames included ([`OldPath::DrawnDownNoTaller`]). The
    /// change deadlines read it ([`first_px_step`]): a sleeping body the law holds breathes in
    /// under the clamp, and a deadline at each pixel of the unclamped breath woke its host for
    /// a frame drawn the same.
    #[inline]
    const fn drawn_no_taller(&self) -> bool {
        (self.head_holds()
            || self.fit < 1.0
            || (self.sink > 0.0 && !self.old_path(OldPath::DrawnDownNoTaller)))
            && !self.old_path(OldPath::HeldNoTaller)
    }

    /// The frame as the head law draws it ([`Self::sunk`]): the fit on the pose's scale, no
    /// taller than the fit where the law holds the body in its row or fits it
    /// ([`Self::drawn_no_taller`], [`OldPath::HeldNoTaller`]). The console judges this box
    /// ([`Self::console_frame_pass`]): judged whole, a body fitted clear of a dense page's ink
    /// stayed latched under it, which the `ConsolePetZOrder` model rejects; and judged taller
    /// than drawn, a loaf drawn down its row cleared the ink at a step of its unclamped fold that
    /// no change deadline named, so a host at the brain's offers let the z hold go a tick late.
    /// Reverted by [`OldPath::ConsoleReadsTheFit`].
    #[inline]
    pub(super) const fn head_drawn(&self, frame: &PetFrame) -> PetFrame {
        let mut drawn = *frame;
        if self.old_path(OldPath::ConsoleReadsTheFit) {
            return drawn;
        }
        if self.drawn_no_taller() {
            drawn.scale_y = drawn.scale_y.min(1.0);
        }
        if self.fit < 1.0 {
            drawn.scale_x *= self.fit;
            drawn.scale_y *= self.fit;
        }
        drawn
    }

    /// Whether the writing carries a forward walk's stand on as the body walks to it: a run of
    /// keys writes on along the row ([`Self::writing_on`]), so the stand beside the caret moves
    /// with each key and the body runs past where it stands now. The head law's look ahead
    /// stops at a stand the body comes to rest at ([`OldPath::LookAheadStopsAtTheStand`]), not
    /// at one the writing carries on: capped there, a cat getting going with the first keys (no
    /// flight at a typing run's start, r24 K7-1) read the row above six tenths of a cell ahead
    /// of its box and met the next word's letters before its sink was done. Reverted with the
    /// top speed's read by [`OldPath::LookAheadAtTopSpeed`].
    #[inline]
    fn writing_carries_the_stand(&self) -> bool {
        !self.old_path(OldPath::LookAheadAtTopSpeed) && self.run_dir > 0 && self.writing_on()
    }

    /// How fast (cells a second) a body on its legs may run along its row before the head law's
    /// ease ([`HEAD_EASE_S`]) is done: the faster of its speed and the run's top speed
    /// ([`Self::top_speed`], the wide chase's [`TOP_SPEED_MAX`] for writing in wide glyphs).
    /// Read at the speed it ran at as the ease began, the look ahead fell short of a body
    /// that sped up through it: getting going on foot with the first keys (no flight at a
    /// typing run's start, r24 K7-1), the cat at 18 cells a second was at 28 by the time it was
    /// down, and its box met the next word's letters on the row above for three frames.
    /// Reverted by [`OldPath::LookAheadAtTopSpeed`].
    #[inline]
    const fn ease_reach_speed(&self) -> f32 {
        let now = self.speed.abs();
        if self.old_path(OldPath::LookAheadAtTopSpeed) {
            return now;
        }
        let top = if self.wide_run() {
            TOP_SPEED_MAX
        } else {
            self.top_speed()
        };
        if now > top { now } else { top }
    }

    /// The column a walk along the body's row comes to rest at, for the head law's look ahead:
    /// the body comes to rest at its stand and goes no further
    /// ([`OldPath::LookAheadStopsAtTheStand`]).
    #[inline]
    const fn stand_on_row(&self) -> Option<f32> {
        match self.walk_to {
            Some((col, row))
                if (row - self.row).abs() < 0.5
                    && !self.old_path(OldPath::LookAheadStopsAtTheStand) =>
            {
                Some(col)
            }
            _ => None,
        }
    }

    /// How far (cells) behind its left edge `a` the head law reads the row above over a body
    /// running back against the writing: as far as its ease takes it
    /// ([`Self::ease_reach_speed`] times [`HEAD_EASE_S`]), and no further than its stand
    /// ([`Self::stand_on_row`]).
    #[inline]
    const fn reach_back(&self, a: f32) -> f32 {
        let reach = self.ease_reach_speed() * HEAD_EASE_S;
        match self.stand_on_row() {
            Some(to) => {
                let left = a - to;
                if left > reach {
                    reach
                } else if left > 0.0 {
                    left
                } else {
                    0.0
                }
            }
            None => reach,
        }
    }

    /// A gather along the body's row (its coil `coil` s long) reads the row above over its
    /// flight's way while the body is at rest ([`Self::setting_off`]), over the coil's last
    /// [`HEAD_EASE_S`] and while it waits past its end, and leaves the ground once it is drawn
    /// for that way ([`Self::owes_the_way`]). Until then the body is judged where it stands, as
    /// the page changes under it ([`OldPath::JudgedAsThePageChanges`]). Reverted by
    /// [`OldPath::WayBeforeSettingOff`].
    #[inline]
    const fn reads_the_way(&mut self, to: f32, to_row: f32, coil: f32) {
        if (to_row - self.row).abs() < 0.5
            && self.action_t + HEAD_EASE_S >= coil
            && !self.old_path(OldPath::WayBeforeSettingOff)
        {
            self.setting_off = Some(to);
        }
    }

    /// Whether a body at rest about to set off along its row ([`Self::setting_off`]) is still
    /// drawn taller than the head law draws it for that way ([`Self::head_span`]): its eased
    /// box top stands over the one the law sets, so it waits at rest while the ease brings it
    /// down. Judged by the box's top, so a body the law holds drawn down or fitted
    /// ([`OldPath::FitHeldAlongTheRow`], [`OldPath::HeadHeldAcrossGaps`]) owes nothing for the
    /// other of the two. The span is the one [`Self::ease_head`] eases to, so the wait ends.
    /// Under reduced motion the law draws the body for its way at once, and it owes nothing.
    fn owes_the_way(&self, sense: PetSense, width: f32) -> bool {
        if self.setting_off.is_none()
            || self.flight.is_some()
            || sense.reduced_motion
            || !width.is_finite()
            || width <= 0.0
        {
            return false;
        }
        let (seat, span) = self.head_span(sense.cols, width);
        let Some(read) = self.head_target(seat.1, (sense.rows, sense.cols), span) else {
            return false;
        };
        let (sink, fit) = read.law;
        let (s, f) = self.head_eased(sense.reduced_motion);
        ART_ROWS * f - s > ART_ROWS * fit - sink + 1e-3
    }

    /// Where the head law judges the body ([`Self::ease_head`]) and over which columns: the
    /// stand, or a flight's landing, and the body's columns, or a bound's whole way along its
    /// row, read past each edge as far as a stretch reaches ([`HEAD_STRETCH_PAD`]) and as far
    /// ahead as the ease takes it (a running body's top speed times [`HEAD_EASE_S`] the way it
    /// runs, to its stand; a body at rest [`HEAD_REST_REACH`] the way the writing carries it).
    /// A body at rest about to set off along its row ([`Self::setting_off`]) is read over its
    /// way as well, so the law draws it for that way before it goes
    /// ([`OldPath::WayBeforeSettingOff`]). Along its own row, on a pane past the map's columns, the
    /// span is read as far either way from the body as the map reads about it
    /// ([`Self::read_about`]).
    #[inline]
    fn head_span(&self, cols: u16, width: f32) -> ((f32, f32), (f32, f32)) {
        let (seat, (a, b)) = match &self.flight {
            Some(f) if (f.to_row - f.from_row).abs() < 0.5 => (
                (f.to_col, f.to_row),
                (f.from_col.min(f.to_col), f.from_col.max(f.to_col) + width),
            ),
            Some(f) => ((f.to_col, f.to_row), (f.to_col, f.to_col + width)),
            None => ((self.col, self.row), (self.col, self.col + width)),
        };
        let span = if self.old_path(OldPath::HeadLooksAhead) || cols == 0 {
            (a, b)
        } else {
            let cols = f32::from(cols);
            let pad = HEAD_STRETCH_PAD * width;
            let (behind, ahead) = match &self.flight {
                Some(_) => (0.0, 0.0),
                None if self.speed < 0.0 => (self.reach_back(a), 0.0),
                None if self.speed > 0.0 => {
                    let reach = self.ease_reach_speed() * HEAD_EASE_S;
                    match self.stand_on_row() {
                        Some(to) if !self.writing_carries_the_stand() => {
                            let right = to - a;
                            (
                                0.0,
                                if right > reach {
                                    reach
                                } else if right > 0.0 {
                                    right
                                } else {
                                    0.0
                                },
                            )
                        }
                        _ => (0.0, reach),
                    }
                }
                // A body about to set off along its row reads its way too: the whole way a
                // gather's flight takes, or as far as a walk back against the writing goes in the
                // ease ([`Self::setting_off`]).
                None => match self.setting_off {
                    Some(to) if to < a => (a - to, HEAD_REST_REACH),
                    Some(to) if to - a > HEAD_REST_REACH => (0.0, to - a),
                    _ => (0.0, HEAD_REST_REACH),
                },
            };
            (
                (a - pad - behind).max(0.0).min(a),
                (b + pad + ahead).min(cols).max(b),
            )
        };
        // Along its own row, read as far as the map reads about the body: on a pane past its
        // columns, the way to a far caret runs past either window ([`Self::read_about`]).
        let span = if (seat.1 - self.row).abs() < 0.5 {
            let core = (self.col, self.col + width);
            let span = self.read_about(seat.1 - 1.0, span, core);
            self.read_about(seat.1 + 1.0, span, core)
        } else {
            span
        };
        (seat, span)
    }

    /// The sink and fit drawn this frame: [`Self::head_target`], each eased (at [`SINK_RATE`]
    /// and [`FIT_RATE`]) from where the ease stood when the target last changed (at once under
    /// reduced motion). Where the page is not read yet the body is drawn whole on its row. A
    /// body fitted to its row stays fitted along it while the row above holds ink over it:
    /// drawn down into each blank stretch of the row under and fitted again where that row
    /// holds ink, a cat walking a prompt between two lettered rows of ragged ends grew and
    /// shrank at each (the `steam` capture's dip and return, review9fix round 1's judge).
    /// Reverted by [`OldPath::FitHeldAlongTheRow`]. A body drawn down or fitted stays so along
    /// its row across a gap in the row above (a row above blank across is no gap): drawn down
    /// while the row under stays blank under it, else fitted, and fitted where it was fitted.
    /// Under ragged output (a column listing, words a tab apart) the sink let go in each gap
    /// wider than the body, and the body rose and sank about four times a second (review9fix
    /// round 3's review). Reverted by [`OldPath::HeadHeldAcrossGaps`]. The body is judged
    /// ahead over the columns its ease takes it across: a running body's top speed times
    /// [`HEAD_EASE_S`] the way it runs ([`Self::ease_reach_speed`]), a body at rest
    /// [`HEAD_REST_REACH`] the way the writing
    /// carries it (to the right), and past each edge as far as its stretch reaches
    /// ([`HEAD_STRETCH_PAD`]). Judged over its own columns, a hop off a gap began its sink as it
    /// left, and its box reached the next word's first letters before the ease did. Reverted
    /// by [`OldPath::HeadLooksAhead`]. A body at rest about to set off along its row is judged
    /// over its way as well ([`Self::head_span`], [`OldPath::WayBeforeSettingOff`]).
    #[inline]
    fn ease_head(&mut self, sense: PetSense, width: f32) -> (f32, f32) {
        #[inline]
        const fn apart(x: (f32, f32), y: (f32, f32)) -> bool {
            (x.0 - y.0).abs() > 1e-3 || (x.1 - y.1).abs() > 1e-3
        }
        let (seat, span) = self.head_span(sense.cols, width);
        // The page it was judged on: letters printed under a body at rest (a command's output
        // landing over the cat on the grid's last row) are judged as they land
        // ([`OldPath::JudgedAsThePageChanges`]).
        let page = match self.console.world.as_ref() {
            Some(w) if !self.old_path(OldPath::JudgedAsThePageChanges) => match w.stamp() {
                Some(stamp) => stamp.content_seq,
                None => 0,
            },
            _ => 0,
        };
        let (moved, along, landed) = match self.sink_seat {
            Some((at, over, seen)) => {
                let still = !apart(at, seat) && !apart(over, span);
                (
                    !still || seen != page,
                    (at.1 - seat.1).abs() < 0.5,
                    still && seen != page,
                )
            }
            None => (true, false, false),
        };
        // A flight down to a lower row (the wrap's crossing to the next line) is drawn whole in
        // the air until its landing's last [`HEAD_EASE_S`], when the head law judges the landing
        // and eases the body to it by the touchdown ([`OldPath::SinkAtTheLanding`]): judged at the
        // take-off, the landing's sink came off the arc from the first frame, and every crossing
        // flew 0.18 to 0.43 of a row over the row it left instead of 1.2, its body through the
        // line it left (Solarized Dark's `g` of `dog` at 94.7% of its own reading, r24 K7-6). A
        // flight up to a row under letters comes up into it under them, judged from the
        // take-off as before: whole in the air, it rose into the letters over its landing.
        let in_the_air = match &self.flight {
            Some(f) => {
                f.to_row - f.from_row >= 0.5
                    && f.dur - f.t > HEAD_EASE_S
                    && !self.old_path(OldPath::SinkAtTheLanding)
            }
            None => false,
        };
        let target = if in_the_air {
            self.sink_seat = None;
            (0.0, 1.0)
        } else if moved {
            let judged = if width.is_finite() && width > 0.0 {
                self.head_target(seat.1, (sense.rows, sense.cols), span)
            } else {
                Some(HeadRead {
                    law: (0.0, 1.0),
                    gap_above: false,
                    blank_below: false,
                })
            };
            self.sink_seat = if judged.is_some() {
                Some((seat, span, page))
            } else {
                None
            };
            match judged {
                Some(HeadRead {
                    law: (sink, fit), ..
                }) if sink > 0.0
                    && fit >= 1.0
                    && along
                    && self.fit_to < 1.0
                    && !self.old_path(OldPath::FitHeldAlongTheRow) =>
                {
                    (0.0, self.fit_to)
                }
                Some(HeadRead {
                    gap_above: true,
                    blank_below,
                    ..
                }) if along
                    && (self.sink_to > 0.0 || self.fit_to < 1.0)
                    && !self.old_path(OldPath::HeadHeldAcrossGaps) =>
                {
                    // Drawn down where the row under stays blank under it, else fitted.
                    if self.fit_to < 1.0 || blank_below {
                        (self.sink_to, self.fit_to)
                    } else if self.old_path(OldPath::RowFit) {
                        (0.0, 1.0)
                    } else {
                        (0.0, ROW_FIT)
                    }
                }
                Some(h) => h.law,
                None => (0.0, 1.0),
            }
        } else {
            (self.sink_to, self.fit_to)
        };
        // A page that changes under a body at rest takes it down or fits it where letters land
        // under its box, and never draws it taller: the ease is for a body that walks, and a
        // hidden caret's home whose page scrolls under it does not move.
        let target = if landed && target.0 <= self.sink_to && target.1 >= self.fit_to {
            (self.sink_to, self.fit_to)
        } else {
            target
        };
        if target.0 != self.sink_to || target.1 != self.fit_to {
            // From where the ease stands now, not where the last tick drew it: a host that
            // ticked between draws the same.
            (self.sink_from, self.fit_from) = self.head_eased(sense.reduced_motion);
            // Letters printed over a body that stood still take it down or fit it at once:
            // the ease is for a body that walks into a word, which the look ahead begins in
            // time; the page gives none.
            if landed && (target.0 > self.sink_from || target.1 < self.fit_from) {
                (self.sink_from, self.fit_from) = target;
            }
            (self.sink_to, self.fit_to) = target;
            self.sink_since = self.clock;
        }
        (self.sink, self.fit) = self.head_eased(sense.reduced_motion);
        (self.sink, self.fit)
    }

    /// Where the head law's ease ([`Self::ease_head`]) stands at the clock: each of the sink
    /// and the fit from where it was when its target last changed, at its rate, by the time
    /// since; at its target under reduced motion.
    ///
    /// THE BOX GROWS ONCE IT HAS RISEN TO ITS ROW. A body drawn further down into the row under
    /// its own than the head law draws it there, rising (its sink coming down to `sink_to`)
    /// while it grows to a larger fit, grows only once the sink is down to `sink_to`: out of the
    /// row under where the law draws it on its own row, and no deeper into it than
    /// [`SINK_ROWS`] where the law sinks it, which it does only where that row is blank under
    /// the body ([`head_law`]). A body slid up off the line ([`Self::slide_to_row`]) is drawn
    /// where it stood, its feet in the line's row, and the wall holds a fitted box's right edge
    /// at the grid's end ([`Self::fitted_to_the_wall`]), so grown as it rose, its box widened
    /// leftward over the line's last letters. On a full page's last row whose row over the line
    /// was cleared over a line that seats the body at the wall, the margin's seat on the cleared
    /// row ([`Self::margin_seat_off_the_line`]) had the box on those letters at full alpha in 6
    /// frames on 13 x 29 px cells, 4 and 7 at four and three blank cells on 14 x 26, and 7 on
    /// 11 x 28, reaching 11 to 14 px into them (Linux arm64, 2026-11-08); there the law draws it
    /// on its own row, and with the wait it grows from the frame its box's foot reaches the
    /// line's row's top. Reverted by [`OldPath::FitWaitsForTheFeet`].
    #[inline]
    const fn head_eased(&self, reduced_motion: bool) -> (f32, f32) {
        #[inline]
        const fn eased(from: f32, to: f32, rate: f32, t: f32, reduced_motion: bool) -> f32 {
            let way = to - from;
            if reduced_motion || t * rate >= way.abs() {
                to
            } else {
                from + way.signum() * t * rate
            }
        }
        let t = ((self.clock - self.sink_since) as f32).max(0.0);
        let fit_t = if self.fit_to > self.fit_from
            && self.sink_from > 0.0
            && self.sink_from > self.sink_to
            && !self.old_path(OldPath::FitWaitsForTheFeet)
        {
            let off = self.sink_from - self.sink_to.max(0.0);
            (t - off * (1.0 / SINK_RATE)).max(0.0)
        } else {
            t
        };
        (
            eased(self.sink_from, self.sink_to, SINK_RATE, t, reduced_motion),
            eased(self.fit_from, self.fit_to, FIT_RATE, fit_t, reduced_motion),
        )
    }

    #[inline]
    const fn hold_still_pose(&mut self, frame: &mut PetFrame) {
        self.still_held = false;
        if frame.alpha == 0 {
            self.still_since = None;
            self.stride_on = None;
            return;
        }
        let mut stand = self.is_stand_pose(frame.pose);
        // A set-off cut by a gather, a perk or a startle within its floor keeps its stride on
        // glass that long, as a still pose a cut keeps ([`OldPath::StrideKeepsItsFloor`]): a cat
        // that took one stride and crouched for a program's chunk drew the stride for a single
        // present (the long insert's dictation, at 120 Hz).
        let stride = !stand && matches!(frame.action, PetAction::Walk | PetAction::Run);
        if !stride
            && matches!(
                frame.action,
                PetAction::Crouch | PetAction::Perk | PetAction::Startle
            )
            && !self.old_path(OldPath::StrideKeepsItsFloor)
            && let Some((from, pose, scale)) = self.stride_on
            && ((self.clock - from) as f32) < STILL_SECS
        {
            frame.pose = pose;
            (frame.scale_x, frame.scale_y) = scale;
            self.still_held = true;
            return;
        }
        self.stride_on = if stride {
            let from = match self.stride_on {
                Some((from, _, _)) => from,
                None => self.clock,
            };
            Some((from, frame.pose, (frame.scale_x, frame.scale_y)))
        } else {
            None
        };
        // A landing's touchdown is a pose on the ground: a backspace's flinch on the
        // landing's first present drew it for a single frame.
        let travelling = !stand
            && matches!(
                frame.action,
                PetAction::Walk | PetAction::Run | PetAction::Leap
            );
        // A caret move that deals the seat's ladder again (the gaze looking back, the
        // tail's beat restarting) holds the pose just put on, as a gather does.
        let redealt = frame.action.settled()
            && match (self.still_since, self.moved_at) {
                (Some((on, _)), Some(moved)) => moved >= on,
                _ => false,
            };
        // A sit's beat just put on glass is not given up for the stand within [`STILL_SECS`]
        // either: a key that stood the cat up 24 ms after its sit's tail swung back drew the
        // swing for three 8 ms frames (the composer take, once r25 K8-3's brisk bound moved
        // the sit's beats onto its key).
        if let Some((_, pose)) = self.still_since
            && pose as u8 != frame.pose as u8
            && self.still_hold_fresh()
            && (matches!(
                frame.action,
                PetAction::Crouch | PetAction::Perk | PetAction::Startle
            ) || (!travelling && (self.still_held_kind() || stand && self.still_sat))
                || redealt)
        {
            frame.pose = pose;
            (frame.scale_x, frame.scale_y) = self.still_scale;
            stand = self.is_stand_pose(pose);
            self.still_held = true;
        } else if travelling || matches!(frame.action, PetAction::Crouch) {
            self.still_since = None;
        } else if match self.still_since {
            None => true,
            Some((_, pose)) => pose as u8 != frame.pose as u8,
        } {
            self.still_since = Some((self.clock, frame.pose));
            self.still_scale = (frame.scale_x, frame.scale_y);
            self.still_console = self.console_posed;
            self.still_sat = matches!(frame.action, PetAction::Sit);
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

    /// Deals each leg (a walk, a run or a leap, from its gather to its landing) its side of
    /// the letters ([`FRONT_PASS_EVERY`]): one leg in three that sets out while nothing is
    /// being written ([`Self::being_written`]) crosses words in front of their glyphs, and
    /// every other leg goes under them. Writing mid-leg turns a leg in front back under, so
    /// the cat never covers a line the hand or a program is writing.
    #[inline]
    fn deal_front_pass(&mut self, action: PetAction) {
        if !matches!(
            action,
            PetAction::Walk
                | PetAction::Run
                | PetAction::Crouch
                | PetAction::Leap
                | PetAction::Land
        ) {
            self.front_pass = None;
            return;
        }
        if self.front_pass == Some(false) {
            return;
        }
        if self.being_written() {
            self.front_pass = Some(false);
        } else if self.front_pass.is_none() {
            self.front_pass =
                Some(self.front_pass_serial % FRONT_PASS_EVERY == FRONT_PASS_EVERY - 1);
            self.front_pass_serial = self.front_pass_serial.wrapping_add(1);
        }
    }

    /// Whether a line is being written ([`Self::deal_front_pass`]): a key the app noted
    /// within [`SIT_AFTER`], a typing run the caret's echo reads (keys typed past the
    /// console, as `aterm ctl key` types them: [`RHYTHM_MOVES`] steps one way, the last
    /// within [`SIT_AFTER`]), or a command still running in the console. A caret that jumped
    /// once is not writing.
    #[inline]
    fn being_written(&self) -> bool {
        self.key_within_dur(SIT_AFTER_DUR).is_some()
            || (self.run_count >= RHYTHM_MOVES - 1 && self.run_t < SIT_AFTER)
            || self.console_executing()
    }

    /// Whether the leg under way crosses letters in front of them ([`Self::deal_front_pass`]).
    #[must_use]
    #[inline]
    pub const fn front_pass(&self) -> bool {
        matches!(self.front_pass, Some(true))
    }

    /// A STANCE'S STRETCH NEVER CARRIES THE BODY ONTO THE CARET'S CELL. A pose stretches the
    /// drawn body about its centre ([`drawn_body_px`]), so a body stationed flush beside the
    /// caret ([`STATION_LEAD`]) laid its stretched box a pixel or two over the caret's cell:
    /// the trail gallery's `fold-rest` `f1250` and `f1400` (review9fix round 3), the cat at
    /// rest beside the caret after `yz`, its box 1 px into the caret's cell in every companion
    /// style on every theme. Where the body at its unstretched width keeps off the caret's cell
    /// and the stretch about its centre would not, the frame stretches from the edge beside the
    /// caret instead ([`PetFrame::stretch_from`]): that edge stays where the unstretched body
    /// draws it and the whole swell reaches away from the caret. The body's column (which the
    /// laws on its place and pace read) and its width (whose steps the frame scheduler offers)
    /// are unchanged, so the edge kept moves the box only at a step of its width, and at its
    /// unstretched width every rule draws the same box. A body that meets the caret's cell
    /// unstretched (a bound over it) is left as it is, and so is one the grid's edge would push
    /// back onto the cell. Reverted by [`OldPath::StretchOffCaret`].
    #[inline]
    const fn keep_stretch_off_caret(&self, frame: &mut PetFrame, sense: &PetSense) {
        #[inline]
        const fn meets(
            b: (i32, i32, i32, i32),
            (x0, x1): (i32, i32),
            (y0, y1): (i32, i32),
        ) -> bool {
            b.0 < x1 && b.1 > x0 && b.2 < y1 && b.3 > y0
        }
        if self.old_path(OldPath::StretchOffCaret) || frame.scale_x <= 1.0 {
            return;
        }
        let Some((cr, cc)) = sense.caret else {
            return;
        };
        let (cw, ch) = (sense.cell_w, sense.cell_h);
        let Some(drawn) = frame.body_px(cw, ch, sense.cols, sense.rows) else {
            return;
        };
        let mut natural = *frame;
        natural.scale_x = 1.0;
        let Some(unstretched) = natural.body_px(cw, ch, sense.cols, sense.rows) else {
            return;
        };
        let (cw_px, ch_px) = (cw as i32, ch as i32);
        let (cc_i32, cr_i32) = (cc as i32, cr as i32);
        let (x0, x1) = (cc_i32 * cw_px, (cc_i32 + 1) * cw_px);
        let (y0, y1) = (cr_i32 * ch_px, (cr_i32 + 1) * ch_px);
        if meets(unstretched, (x0, x1), (y0, y1)) || !meets(drawn, (x0, x1), (y0, y1)) {
            return;
        }
        let mut kept = *frame;
        kept.stretch_from = if unstretched.0 >= x1 {
            StretchFrom::Left
        } else {
            StretchFrom::Right
        };
        if match kept.body_px(cw, ch, sense.cols, sense.rows) {
            Some(b) => meets(b, (x0, x1), (y0, y1)),
            None => false,
        } {
            return;
        }
        frame.stretch_from = kept.stretch_from;
    }

    /// A STANCE'S STRETCH NEVER CARRIES THE BODY ONTO LETTERS IT KEEPS OFF. A pose stretches
    /// the drawn body about its centre ([`drawn_body_px`]) by a share of its width, so the
    /// wider the body in cells, the further past its edges: a take-off's coil ([`COIL_SQUASH`],
    /// [`LAUNCH_STRETCH`]) up to 0.42 of a cell past each on the trail gallery's 14 x 26 px
    /// cells (a body 5.2 cells across) and 0.51 under Linux's bundled JetBrains Mono's 13 x 29
    /// (6.25 across), past the stretch a seat's head clears the row above's letters by
    /// ([`HEAD_STRETCH_PAD`]). A body may stand with its footprint ([`INK_PAD`] in from each
    /// edge, as [`Self::ink_overlaps`] reads it) flush beside a line's last letters: the dog
    /// that stepped clear of `./k` after a hasty Enter ([`Self::watching_from_the_line`], the
    /// gallery's `move-up`) coiled to leap as the caret moved up, and the coil laid its box
    /// 6 px back, onto the `k`'s ink. A cat seated whole past the row above's last letter on
    /// the alternate screen coiled there and laid its box a pixel onto that letter's cell.
    /// Where the stretch about the centre would carry the box onto a lettered cell (a cell of a
    /// row's ink span the observed page does not certify blank) that the unstretched box keeps
    /// off, or, for a box already on one, the footprint onto letters in a row the box covers by
    /// a quarter of a row or more (as the letter veil reads them, [`Self::letter_cover`]) that
    /// the unstretched footprint keeps off, the frame stretches from the edge beside the
    /// letters instead ([`PetFrame::stretch_from`]), as it does beside the caret
    /// ([`Self::keep_stretch_off_caret`], which goes first): that edge stays where the
    /// unstretched body draws it and the whole swell reaches away from the letters. The body's
    /// column and width are unchanged. A body whose swell would meet letters from either edge
    /// (letters on both sides), or the caret's cell where the stretch about its centre does
    /// not, or that the grid's edge would push back, is left as it is. Reverted by
    /// [`OldPath::StretchOffLetters`].
    #[inline]
    fn keep_stretch_off_letters(&self, frame: &mut PetFrame, sense: &PetSense, width: f32) {
        /// Whether the footprint from column `left`, `w` wide, lies on letters in a row the box
        /// `b` covers by a quarter of a row or more (cell height `ch` px).
        #[inline]
        const fn foot_on_letters(
            pet: &PetBrain,
            b: (i32, i32, i32, i32),
            ch: i32,
            left: f32,
            w: f32,
        ) -> bool {
            let mut row = b.2.div_euclid(ch);
            let last = (b.3 - 1).div_euclid(ch);
            while row <= last {
                let top = if row * ch > b.2 { row * ch } else { b.2 };
                let bottom = if (row + 1) * ch < b.3 {
                    (row + 1) * ch
                } else {
                    b.3
                };
                if (bottom - top) * 4 >= ch && pet.ink_overlaps(left, row as f32, w) {
                    return true;
                }
                row += 1;
            }
            false
        }
        /// Whether the box `b` meets a lettered cell of a row it covers (cell `cw` x `ch` px).
        #[inline]
        const fn box_on_letters(pet: &PetBrain, b: (i32, i32, i32, i32), cw: i32, ch: i32) -> bool {
            let inv_cw = 1.0 / (cw as f32);
            let (a, e) = (b.0 as f32 * inv_cw, b.1 as f32 * inv_cw);
            let mut row = b.2.div_euclid(ch);
            let last = (b.3 - 1).div_euclid(ch);
            while row <= last {
                if let Some((first, end)) = pet.ink_span(row as f32)
                    && e > first
                    && a < end
                    && !matches!(pet.observed_ink_in_span(row as f32, a, e), Some(false))
                {
                    return true;
                }
                row += 1;
            }
            false
        }
        #[inline]
        const fn meets(b: (i32, i32, i32, i32), c: (i32, i32, i32, i32)) -> bool {
            b.0 < c.1 && b.1 > c.0 && b.2 < c.3 && b.3 > c.2
        }
        if self.old_path(OldPath::StretchOffLetters)
            || frame.scale_x <= 1.0
            || !matches!(frame.stretch_from, StretchFrom::Centre)
            || !width.is_finite()
            || width <= 0.0
        {
            return;
        }
        let (cw, ch) = (sense.cell_w, sense.cell_h);
        let (cols, rows) = (sense.cols, sense.rows);
        let mut natural = *frame;
        natural.scale_x = 1.0;
        let mut left = *frame;
        left.stretch_from = StretchFrom::Left;
        let mut right = *frame;
        right.stretch_from = StretchFrom::Right;
        let (Some(drawn), Some(n), Some(l), Some(r)) = (
            frame.body_px(cw, ch, cols, rows),
            natural.body_px(cw, ch, cols, rows),
            left.body_px(cw, ch, cols, rows),
            right.body_px(cw, ch, cols, rows),
        ) else {
            return;
        };
        let (cw_px, ch_px) = (cw as i32, ch as i32);
        let (col, grow) = (frame.col, (frame.scale_x - 1.0) * width);
        // The cells the unstretched box keeps off, or, on one already, the footprint's letters.
        let by_box = !box_on_letters(self, n, cw_px, ch_px);
        if !by_box && foot_on_letters(self, n, ch_px, col, width) {
            return;
        }
        // The stretched box `b`, its footprint from column `foot`, on letters by that measure.
        let on = |b: (i32, i32, i32, i32), foot: f32| {
            if by_box {
                box_on_letters(self, b, cw_px, ch_px)
            } else {
                foot_on_letters(self, b, ch_px, foot, width + grow)
            }
        };
        if !on(drawn, col - 0.5 * grow) {
            return;
        }
        let from = if l.0 == n.0 && !on(l, col) {
            StretchFrom::Left
        } else if r.1 == n.1 && !on(r, col - grow) {
            StretchFrom::Right
        } else {
            return;
        };
        let kept = if matches!(from, StretchFrom::Left) {
            l
        } else {
            r
        };
        if let Some((cr, cc)) = sense.caret {
            let (cc_px, cr_px) = (cc as i32, cr as i32);
            let cell = (
                cc_px * cw_px,
                (cc_px + 1) * cw_px,
                cr_px * ch_px,
                (cr_px + 1) * ch_px,
            );
            if meets(kept, cell) && !meets(drawn, cell) {
                return;
            }
        }
        frame.stretch_from = from;
    }

    /// The letter veil ([`LETTER_VEIL_DWELL`], [`PetFrame::under_ink`], [`LETTER_VEIL_FLOOR`],
    /// [`Self::letter_cover`], [`Self::clear_of_letters_above`], [`LETTER_VEIL_ON_S`],
    /// [`LETTER_VEIL_OFF_S`], [`Self::needs_frames`], [`Self::sense_ink`], [`Self::path_box`]).
    /// A leg dealt in front ([`Self::front_pass`]) covers none of its letters.
    #[inline]
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
        #[inline]
        const fn cached_body_px(
            cache: &mut Option<Option<(i32, i32, i32, i32)>>,
            frame: &PetFrame,
            sense: &PetSense,
        ) -> Option<(i32, i32, i32, i32)> {
            match *cache {
                Some(rect) => rect,
                None => {
                    let rect = frame.body_px(sense.cell_w, sense.cell_h, sense.cols, sense.rows);
                    *cache = Some(rect);
                    rect
                }
            }
        }
        let mut body_rect: Option<Option<(i32, i32, i32, i32)>> = None;
        let travelling = matches!(
            frame.action,
            PetAction::Walk | PetAction::Run | PetAction::Startle | PetAction::Frolic
        );
        let cover = if travelling
            && !self.front_pass()
            && !self.ink_spans.is_empty()
            && let Some(rect) = cached_body_px(&mut body_rect, frame, &sense)
        {
            let path = Self::path_box(rect, sense.cell_h);
            self.letter_cover(path, sense.cell_w, sense.cell_h)
        } else {
            0.0
        };
        let cover = if cover > 0.0 {
            let since = match self.letter_cover_since {
                Some(s) => s,
                None => {
                    self.letter_cover_since = Some(sense.now);
                    sense.now
                }
            };
            if sense.now.saturating_duration_since(since) >= LETTER_VEIL_DWELL_DUR {
                cover
            } else {
                0.0
            }
        } else {
            self.letter_cover_since = None;
            0.0
        };
        let k = (cover * (1.0 / LETTER_VEIL_FULL)).clamp(0.0, 1.0);
        let walk_target = 1.0 - (1.0 - LETTER_VEIL_FLOOR) * k * k * (3.0 - 2.0 * k);
        let (level, at, last_target) = self.letter_veil;
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
                match cached_body_px(&mut body_rect, frame, &sense) {
                    Some(_) => {
                        let rest = drawn_body_px(
                            (frame.col, frame.row),
                            self.rest_stance(frame),
                            cell,
                            (sense.cols, sense.rows),
                        );
                        self.on_letters(rest, cell, caret_row)
                    }
                    None => Some(false),
                }
            }
            // A veil already lifting off letters the cat rested on (its last target full) lifts
            // on as the cat leaves, in a flight as on a walk or run; only a veil still on is held
            // over the letters it goes on over. Held, a veil caught a frame short of lifting
            // stayed on the cat through the bound and the whole run after it. An escort walking
            // the top of the host's line goes whole ([`Self::escort_over_the_line`]): set out from
            // a pause held veiled over a turn's tip, it went on dimmed the length of the line.
            Some((caret_row, _))
                if travelling
                    && level < 1.0
                    && last_target < 1.0
                    && !self.escort_over_the_line() =>
            {
                let here = match cached_body_px(&mut body_rect, frame, &sense) {
                    Some(rect) => self.on_letters(rect, cell, caret_row),
                    None => Some(false),
                };
                if here == Some(false) {
                    here
                } else {
                    keep = true;
                    None
                }
            }
            Some((caret_row, _)) if in_place => {
                let here = match cached_body_px(&mut body_rect, frame, &sense) {
                    Some(rect) => self.on_letters(rect, cell, caret_row),
                    None => Some(false),
                };
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
                    && matches!(
                        cached_body_px(&mut body_rect, frame, &sense),
                        Some(rect) if self.on_letters(rect, cell, caret.0) == Some(true)
                    )
                    && self.landing_on_letters(sense, caret) == Some(true);
                if gather_on_letters {
                    Some(true)
                } else if veiled && last_target < 1.0 {
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
        let holding = on_letters == Some(false)
            && !rearms
            && self.letter_rest_stepped
            && self.letter_rest_since.is_some()
            && {
                let gone = match self.letter_gone_since {
                    Some(g) => g,
                    None => {
                        self.letter_gone_since = Some(sense.now);
                        sense.now
                    }
                };
                sense.now.saturating_duration_since(gone) < LETTER_VEIL_DWELL_DUR
            };
        let rest_target = match on_letters {
            Some(true) => {
                self.letter_gone_since = None;
                let since = match self.letter_rest_since {
                    Some(s) => s,
                    None => {
                        self.letter_rest_since = Some(sense.now);
                        sense.now
                    }
                };
                let enter = if !dwells
                    && self.key_within_dur(Duration::MAX) == Some(PetInputKind::Submit)
                {
                    self.last_submit_seq()
                } else {
                    None
                };
                if let Some(seq) = enter
                    && (seq != self.letter_step_seq || rearms)
                {
                    self.letter_step_seq = seq;
                    self.letter_rest_stepped = true;
                }
                if self.letter_rest_stepped
                    || sense.now.saturating_duration_since(since) >= LETTER_VEIL_DWELL_DUR
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
        // A tick that ends a sleep ([`Self::tick`]) eases the veil by none of it: an easing veil
        // asks for frames ([`Self::needs_frames`]), so a sleep ends on a veil at rest or held,
        // and an ease the tick begins starts there. Eased by the sleep, a seat's veil rose by a
        // quarter at once on the tick its loaf put it off the letters.
        let dt = match at {
            Some(at) if self.asked_frames => sense.now.saturating_duration_since(at).as_secs_f32(),
            Some(_) => 0.0,
            None => f32::INFINITY,
        };
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
            let inv_on = if resting {
                1.0 / LETTER_VEIL_REST_ON_S
            } else {
                1.0 / LETTER_VEIL_ON_S
            };
            (level - dt.min(LETTER_VEIL_REST_ON_S) * inv_on).max(target)
        } else {
            (level + dt * (1.0 / LETTER_VEIL_OFF_S)).min(target)
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
    #[inline]
    fn hand_writing(&self) -> bool {
        match self.key_within_dur(SIT_AFTER_DUR) {
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
    #[inline]
    fn landing_on_letters(&self, sense: PetSense, caret: (u16, u16)) -> Option<bool> {
        if sense.cell_w == 0 || sense.cell_h == 0 {
            return Some(false);
        }
        let at = match self.flight {
            Some(f) => (f.to_col, f.to_row),
            None => self.station_safe(
                caret,
                sense.cols,
                sense.rows,
                art_cols(sense.cell_w, sense.cell_h),
            ),
        };
        let rect = drawn_body_px(
            at,
            Stance::NEUTRAL,
            (sense.cell_w, sense.cell_h),
            (sense.cols, sense.rows),
        );
        self.on_letters(rect, (sense.cell_w, sense.cell_h), caret.0)
    }

    /// The stance a settled body is judged by over the letters ([`Self::veil_over_letters`]):
    /// the one it draws, unless it rests in a rhythm. A sleeper's breath and a loaf's flick
    /// swell the drawn box by a few hundredths of its height and back; judged by the drawn box,
    /// a body whose top sat near a quarter of a letter's row flipped the verdict with each
    /// breath or flick, and its veil eased up and back down each time, a train of frames each,
    /// all through the breath window or the loaf. A sleeper and a loaf are judged by the box
    /// they rest in, unscaled: the one a sleeper keeps once that window closes, and the one a
    /// loaf folds down to. Not by the fold on the way: a purring loaf draws its purr's swell,
    /// whose steps are offered ([`Self::next_change_deadline`]), and a box that eased with the
    /// fold alone crossed the line at a step nothing offered, so a host at the brain's offers
    /// began the veil's dwell late. Unscaled, it keeps the head law's fit
    /// ([`Self::head_target`]): the box it rests in is the one it is drawn in.
    #[inline]
    const fn rest_stance(&self, frame: &PetFrame) -> Stance {
        let unscaled = match frame.action {
            PetAction::Sleep => true,
            PetAction::Loaf => self.wriggle_t <= 0.0,
            _ => false,
        };
        let (scale_x, scale_y) = if unscaled {
            (self.fit, self.fit)
        } else {
            (frame.scale_x, frame.scale_y)
        };
        Stance {
            scale_x,
            scale_y,
            lift: frame.lift,
            from: frame.stretch_from,
        }
    }

    /// Whether a drawn body's box lays its head on letterforms the console observation
    /// certifies ([`INK_PAD`], [`Self::veil_over_letters`]).
    #[inline]
    const fn on_letters(
        &self,
        (x0, x1, y0, y1): (i32, i32, i32, i32),
        (cell_w, cell_h): (u16, u16),
        caret_row: u16,
    ) -> Option<bool> {
        if cell_w == 0 || cell_h == 0 || y1 <= y0 || x1 <= x0 {
            return Some(false);
        }
        let (inv_cw, ch) = (1.0 / (cell_w as f32), cell_h as i32);
        let (a, b) = (x0 as f32 * inv_cw + INK_PAD, x1 as f32 * inv_cw - INK_PAD);
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

    #[inline]
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
        let inv_cw = 1.0 / (cw as f32);
        let col = x0 as f32 * inv_cw;
        let width = (x1 - x0) as f32 * inv_cw;
        let inv_h = 1.0 / ((y1 - y0) as f32);
        let mut cover = 0.0;
        let mut row = y0.div_euclid(ch);
        let last_row = (y1 - 1).div_euclid(ch);
        while row <= last_row {
            let r_top = row * ch;
            let top = if r_top > y0 { r_top } else { y0 };
            let r_bot = (row + 1) * ch;
            let bottom = if r_bot < y1 { r_bot } else { y1 };
            if (bottom - top) * 4 >= ch && self.ink_overlaps(col, row as f32, width) {
                cover += (bottom - top) as f32 * inv_h;
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

    /// A crouch, perk or startle held still for its caret ([`Self::still_hold_fresh`]): its
    /// action clock stands while the hold does.
    #[inline]
    const fn action_clock_held(&self) -> bool {
        matches!(
            self.action,
            PetAction::Crouch | PetAction::Perk | PetAction::Startle
        ) && self.still_since.is_some()
            && self.still_hold_fresh()
    }

    #[inline]
    const fn still_hold_fresh(&self) -> bool {
        if self.still_fresh() {
            return true;
        }
        self.stand_ends_leg
            && !self.still_console
            && self.still_held_kind()
            && !(self.console_posed && matches!(self.console_attention(), PetAttention::Contact))
            && matches!(self.stand_from, Some(from) if self.clock - from < STAND_MIN as f64)
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
    const fn gait_index(&self, len: usize) -> usize {
        let phase = if self.stride >= 0.0 && self.stride < 1.0 {
            self.stride
        } else {
            let r = self.stride - self.stride.floor();
            if r >= 1.0 { 0.0 } else { r }
        };
        let idx_raw = (phase * len as f32) as usize;
        if idx_raw < len - 1 { idx_raw } else { len - 1 }
    }

    /// The current action (for hosts that gate sound or other effects on it).
    #[inline]
    #[must_use]
    pub const fn action(&self) -> PetAction {
        self.action
    }

    /// True once a sleeping pet is past its breath window (`SLEEP_AFTER + BREATH_WINDOW` quiet
    /// seconds), drawn at full strength and asking for no frame: deep sleep, which the deep-sleep
    /// law of `next_change_secs` holds byte-stable until an input. A pet the lost focus retired
    /// keeps its quiet but reads false through its fade back in (`FADE_IN`), as one a pointer
    /// warmed reads false while its gaze cools ([`Self::needs_frames`] for both). `quiet` gains at
    /// most 5 s however long the window lacks the focus, so a pet the focus reached late is still
    /// breathing.
    #[inline]
    #[must_use]
    pub const fn deep_asleep(&self) -> bool {
        matches!(self.action, PetAction::Sleep)
            && self.quiet >= SLEEP_AFTER + BREATH_WINDOW
            && self.alpha >= 1.0
            && !self.needs_frames()
    }

    /// True while the pet is visible at all (the host's "is there a cat" test).
    #[inline]
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.alpha > 0.0
    }

    #[inline]
    const fn all_motes_none(&self) -> bool {
        let mut i = 0;
        while i < PET_MOTES_MAX {
            if self.motes[i].is_some() {
                return false;
            }
            i += 1;
        }
        true
    }

    #[inline]
    const fn all_departures_none(&self) -> bool {
        let mut i = 0;
        while i < PET_DEPARTURES_MAX {
            if self.departures[i].is_some() {
                return false;
            }
            i += 1;
        }
        true
    }

    /// Drop everything this brain owns because its owner went away.
    #[inline]
    pub fn retire_unowned(&mut self) {
        if self.alpha == 0.0
            && self.last_caret.is_none()
            && self.all_motes_none()
            && self.all_departures_none()
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
        // A test's negative controls outlive the brain they were set on, as the laws they
        // revert do.
        #[cfg(any(test, debug_assertions))]
        let old_paths = self.old_paths;
        Self {
            console,
            ink_spans,
            departure_serial: self.departure_serial,
            #[cfg(any(test, debug_assertions))]
            old_paths,
            ..Self::default()
        }
    }

    /// Host sync for the pet's identity.
    #[inline]
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
    #[inline]
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
    #[inline]
    #[must_use]
    pub const fn needs_frames(&self) -> bool {
        if self.frame_owed {
            return true;
        }
        // A veil at its level is not easing: a still crouch under a word (the
        // hide) holds its floor without the train.
        if self.letter_veil.0 != self.letter_veil.2 && self.alpha > 0.0 && !self.repainting {
            return true;
        }
        // Nor is the head law's sink or fit where it goes ([`Self::head_target`]).
        if (self.sink != self.sink_to || self.fit != self.fit_to) && self.alpha > 0.0 {
            return true;
        }
        // Nor the wall's cut ([`Self::cut_on_the_letters`]): it lifts on past a fade-in that has
        // brought the body back whole.
        if self.wall_cut < 1.0 && self.alpha > 0.0 {
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
            // A rhythm run's lead grows from its key ([`Self::lead`]), moving the body or its
            // pose at moments no offer names.
            || self.carrying_run()
            || self.set_off_owed
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
            // A pop the console hid draws nothing until the offer's walk finds it clear of the
            // ink, or dropped ([`Self::hidden_mote_offer`]).
            if let Some(m) = self.motes[j]
                && !m.kind.rides_offer()
                && !(m.kind.offers_hidden() && self.console_hid_mote(j))
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
        if matches!(self.action, PetAction::Waking) {
            if self.waking_moves() {
                return true;
            }
        } else if !self.action.settled() {
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
            && Self::glide_px_left(self.home - self.col, self.last_cell_w)
        {
            return true;
        }
        false
    }

    /// Whether the wake's stretch moves this frame, or hands the cat on. Only its front reach
    /// moves, drawing out and easing back over [`STRETCH_LEAD`]; after it the stretch holds one
    /// pose until the hind leg's swap at [`STRETCH_SPLIT`] of [`WAKE_DUR`] and then another, two
    /// held stretches the offer covers ([`Self::next_change_secs`]), until its last
    /// [`WAKE_HANDOFF`] rides the train into the ladder. A stretch over ink is a plain seat
    /// throughout. Reduced motion keeps the train it had.
    #[inline]
    const fn waking_moves(&self) -> bool {
        self.last_reduced
            || self.action_t >= WAKE_DUR - WAKE_HANDOFF
            || (self.action_t < STRETCH_LEAD && self.console_stretch_on_blank())
    }

    /// Whether a body `gap` cells short of home, in cells `cell_w` px wide, has a whole pixel or
    /// more of its glide left to draw: the train's rule for a glide, and where the settle ends.
    #[inline]
    const fn glide_px_left(gap: f32, cell_w: u16) -> bool {
        gap.abs() * (cell_w as f32) >= 1.0
    }

    /// The coarse offer. The seat rule's clocks ([`Self::seat_due`]) are not in it while the
    /// rule draws nothing: a host woken for them would draw a frame nobody sees changed, in
    /// states this offer promises no poll (a retired body, a selection over the whole view).
    /// What draws from the rule adds them here, under the gates that say the pet is drawn.
    #[inline]
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        let console = self.console_deadline(now);
        if self.console_frames().is_some() {
            return console;
        }
        let Some(secs) = self.offer_secs() else {
            return console;
        };
        let base = match self.last_now {
            Some(t) => t,
            None => now,
        };
        let at = base + Duration::from_secs_f32((secs + OFFER_PAST).clamp(0.0, 60.0));
        let next = at.max(now + self.arm_floor());
        Some(match console {
            Some(c) => c.min(next),
            None => next,
        })
    }

    /// [`Self::next_change_secs`] and the edge of the motes the console hid.
    #[inline]
    const fn offer_secs(&self) -> Option<f32> {
        if self.alpha < 1.0 || self.last_caret.is_none() {
            return None;
        }
        let secs = self.next_change_secs();
        if matches!(secs, Some(s) if s <= 0.0) {
            return secs;
        }
        match (secs, self.hidden_mote_offer(secs)) {
            (Some(s), Some(h)) => Some(s.min(h)),
            (Some(s), None) | (None, Some(s)) => Some(s),
            (None, None) => None,
        }
    }

    #[inline]
    const fn mote_cell_px(&self) -> (f32, f32) {
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
        (cw, ch)
    }

    /// The edges of the motes the frame draws: each one's death, next look slice and next
    /// whole-pixel step. A mote the console hid draws none of them ([`Self::hidden_mote_offer`]).
    #[inline]
    const fn mote_edges(&self, soon: &mut Soonest) {
        let cell = self.mote_cell_px();
        let mut i = 0;
        while i < self.motes.len() {
            if let Some(m) = self.motes[i]
                && m.kind.rides_offer()
                && !self.console_hid_mote(i)
            {
                self.mote_edges_from_with_cell(&m, 0.0, cell, soon);
            }
            i += 1;
        }
    }

    /// Mote `m`'s edges as a tick `from` seconds after the last would offer them, in seconds
    /// after that tick.
    #[inline]
    const fn mote_edges_from_with_cell(
        &self,
        m: &Mote,
        from: f32,
        (cw, ch): (f32, f32),
        soon: &mut Soonest,
    ) {
        let life = m.life.max(0.01);
        let u0 = m.age(self.clock + from as f64);
        if u0 >= 0.0 && u0 < 1.0 {
            let inv_life = 1.0 / life;
            let remain = (1.0 - u0) * life;
            soon.at(remain);
            // A pop has no look slices: it swells every frame, which a drawn pop's train draws,
            // and a hidden one shows again only at a step of its centre, below
            // ([`Self::hidden_mote_offer`]).
            if !matches!(m.kind, PetMoteKind::ZeePop) {
                let steps = m.look_steps();
                soon.at(((look_slice(u0, steps) + 1.0) * (1.0 / steps) - u0) * life);
            }
            let horizon = match soon.0 {
                Some(s) if s < remain => s,
                _ => remain,
            };
            let (sx, sy) = m.drift_slope();
            let slope = (sx * inv_life * cw, sy * inv_life * ch);
            let step = first_px_step(
                (cw, ch),
                horizon,
                slope,
                PxStepSampler::Mote {
                    m: *m,
                    u0,
                    inv_life,
                },
                (1.0, false),
            );
            if let Some(t) = step {
                soon.at(t);
            }
        }
    }

    /// Where the hold that drew the last frame's still pose over the brain's own may end: its
    /// [`STILL_SECS`] and a stand's [`STAND_MIN`] ([`Self::still_hold_fresh`]). The frame train can
    /// be gone before either, and the tick after the hold draws the brain's pose.
    #[inline]
    const fn still_hold_edges(&self, soon: &mut Soonest) {
        if let Some((at, _)) = self.still_since {
            soon.at(STILL_SECS - (self.clock - at) as f32);
        }
        if let Some(from) = self.stand_from {
            soon.at((STAND_MIN as f64 - (self.clock - from)) as f32);
        }
    }

    /// Mote `m`'s next edge after `from` seconds past the last tick, in seconds past it.
    #[inline]
    const fn mote_own_edge(&self, m: &Mote, from: f32, cell: (f32, f32)) -> Option<f32> {
        let mut soon = Soonest(None);
        self.mote_edges_from_with_cell(m, from, cell, &mut soon);
        match soon.0 {
            Some(t) => Some(from + t),
            None => None,
        }
    }

    #[inline]
    const fn next_change_secs(&self) -> Option<f32> {
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
        if self.still_held {
            self.still_hold_edges(&mut soon);
        }
        if let Some(left) = self.writing_wait {
            soon.at(left);
        }
        if self.vigil
            && self.vigil_armed
            && self.exec_since.is_some()
            && matches!(self.action, PetAction::Crouch)
        {
            return soon.0;
        }
        if self.hiding && matches!(self.action, PetAction::Crouch) {
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
            if !matches!(self.action, PetAction::Sleep) {
                soon.at(SLEEP_AFTER - q);
            }
            return soon.0;
        }
        if matches!(self.action, PetAction::Sleep) && q >= SLEEP_AFTER + BREATH_WINDOW {
            // Deep sleep: byte-stable until an input, by law. A retire's fade back in (a focus
            // regained) is `needs_frames`'s, not a deadline.
            return soon.0;
        }
        let ch1 = if self.last_cell_h > 1 {
            self.last_cell_h
        } else {
            1
        };
        let h = (ART_ROWS * (ch1 as f32)).round();
        let w = (h * ART_ASPECT).round();
        let c0 = self.clock;
        if q < SETTLE_TURN {
            soon.at(SETTLE_TURN - q);
        }
        if self.gaze_dwell > 0.0 {
            let dwell = if matches!(self.action, PetAction::Sleep) {
                SLEEP_FLIP_DWELL
            } else {
                SETTLE_FLIP_DWELL
            };
            soon.at(dwell - self.gaze_dwell);
        }
        if matches!(self.action, PetAction::Sleep) {
            soon.at(SLEEP_AFTER + BREATH_WINDOW - q);
            const HALF_BREATH_PERIOD: f64 = (BREATH_PERIOD as f64) * 0.5;
            soon.at((HALF_BREATH_PERIOD - c0 % HALF_BREATH_PERIOD) as f32);
            let zee_q = beat_at(SLEEP_AFTER, ZEE_EVERY, next_beat(q, SLEEP_AFTER, ZEE_EVERY));
            if zee_q <= SLEEP_AFTER + BREATH_WINDOW - ZEE_LIFE {
                soon.at(zee_q - q);
            }
            let horizon = match soon.0 {
                Some(s) => s,
                None => BREATH_WINDOW,
            };
            let breath = TAU / BREATH_PERIOD * BREATH_DEPTH;
            let step = first_px_step(
                (w, h),
                horizon,
                (0.5 * breath * w, breath * h),
                PxStepSampler::Sleep { c0 },
                (self.fit, self.drawn_no_taller()),
            );
            if let Some(t) = step {
                soon.at(t);
            }
            return soon.0;
        }
        let seat_held =
            self.trick_settle.is_some() && !matches!(self.trick_settle, Some(Trick::Sleep));
        if seat_held {
            soon.at(self.hold_end_q - q);
        }
        if !self.waiting_up() && !seat_held {
            soon.at(SLEEP_AFTER - q);
        }
        let tell = self.purring();
        if tell && !matches!(self.action, PetAction::Purr) {
            soon.at((self.content - PURR_GATE) * (1.0 / CONTENT_DECAY));
        }
        if tell {
            let beat = next_beat(q, SIT_AFTER, PURR_MOTE_EVERY);
            soon.at(beat_at(SIT_AFTER, PURR_MOTE_EVERY, beat) - q);
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
                    let slope = (0.03 * w * (1.0 / SIT_FOLD), 0.05 * h * (1.0 / SIT_FOLD));
                    let step = first_px_step(
                        (w, h),
                        SIT_AFTER - q,
                        slope,
                        PxStepSampler::Stand { q },
                        (self.fit, self.drawn_no_taller()),
                    );
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
                        (0.03 * w * (1.0 / SIT_FOLD), 0.05 * h * (1.0 / SIT_FOLD))
                    } else {
                        (0.0, 0.0)
                    };
                    let horizon = if tell {
                        match soon.0 {
                            Some(s) => s,
                            None => SLEEP_AFTER,
                        }
                    } else {
                        SIT_FOLD - since + PX_MARCH_MIN
                    };
                    let slope = (fx + swell_slope.0, fy + swell_slope.1);
                    let step = first_px_step(
                        (w, h),
                        horizon,
                        slope,
                        PxStepSampler::Seat { q, c0, tell },
                        (self.fit, self.drawn_no_taller()),
                    );
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
            PetAction::Waking => {
                // The held stretch's edges ([`Self::waking_moves`]): the hind leg's swap, and
                // the hand-off the train takes from there.
                if self.console_stretch_on_blank() {
                    soon.at(STRETCH_SPLIT * WAKE_DUR - self.action_t);
                }
                soon.at(WAKE_DUR - WAKE_HANDOFF - self.action_t);
            }
            PetAction::Loaf => {
                let loaf_after = self.loaf_after();
                let since = (q - loaf_after).max(0.0);
                let beat = (since * (1.0 / FLICK_EVERY)).floor();
                let phase = since - beat * FLICK_EVERY;
                soon.at(loaf_after + (beat + 1.0).max(1.0) * FLICK_EVERY - q);
                let mut bi = 0;
                while bi < 2 {
                    let k = beat + (bi as f32);
                    if k >= 1.0 && self.beat_deal(k as i64, 3) == 1 {
                        let open = loaf_after + k * FLICK_EVERY;
                        soon.at(open - q);
                        soon.at(open + LOAF_PEEK_DUR - q);
                    }
                    bi += 1;
                }
                let melt_live = since < SIT_FOLD;
                let thump_live = beat >= 1.0 && phase < FLICK_DUR;
                if melt_live || thump_live || tell {
                    let (mx, my) = if melt_live {
                        (0.02 * w * (1.0 / SIT_FOLD), 0.05 * h * (1.0 / SIT_FOLD))
                    } else {
                        (0.0, 0.0)
                    };
                    let ty = if thump_live {
                        TWITCH_BOB * h * (core::f32::consts::PI / FLICK_DUR)
                    } else {
                        0.0
                    };
                    let horizon = if tell {
                        match soon.0 {
                            Some(s) => s,
                            None => SLEEP_AFTER,
                        }
                    } else {
                        let melt_end = if melt_live { SIT_FOLD - since } else { 0.0 };
                        let thump_end = if thump_live { FLICK_DUR - phase } else { 0.0 };
                        melt_end.max(thump_end) + PX_MARCH_MIN
                    };
                    let slope = (mx + swell_slope.0, my + ty + swell_slope.1);
                    let step = first_px_step(
                        (w, h),
                        horizon,
                        slope,
                        PxStepSampler::Loaf {
                            loaf_after,
                            q,
                            c0,
                            tell,
                        },
                        (self.fit, self.drawn_no_taller()),
                    );
                    if let Some(t) = step {
                        soon.at(t);
                    }
                }
            }
            PetAction::Purr => {
                let owed =
                    CONTENT_DECAY * (q - SIT_AFTER).clamp(0.0, self.loaf_after() - SIT_AFTER);
                soon.at((self.content + owed - PURR_GATE) * (1.0 / CONTENT_DECAY));
                let horizon = match soon.0 {
                    Some(s) => s,
                    None => SLEEP_AFTER,
                };
                let step = first_px_step(
                    (w, h),
                    horizon,
                    swell_slope,
                    PxStepSampler::Seat { q, c0, tell },
                    (self.fit, self.drawn_no_taller()),
                );
                if let Some(t) = step {
                    soon.at(t);
                }
            }
            _ => {}
        }
        soon.0
    }

    #[inline]
    const fn sit_tail_edges(
        &self,
        beat: i64,
        swing_fits: bool,
        front_no_yawn: bool,
    ) -> &'static [f32] {
        if front_no_yawn && self.beat_deal(beat, 3) == 0 {
            return &[];
        }
        match self.beat_deal(beat, 4) {
            0 => &TAIL_EDGES_FLICK,
            1 => &TAIL_EDGES_DOUBLE,
            2 if swing_fits => &TAIL_EDGES_SWING,
            _ => &[],
        }
    }

    #[inline]
    const fn sit_beat_edges(&self, soon: &mut Soonest) {
        let since = self.quiet - SIT_AFTER;
        let (period, at) = self.tail_clock();
        let look_dur = if self.sit_front { self.look_dur() } else { 0.0 };
        let swing_fits = !self.sit_front || look_dur >= 7.0 * TAIL_SWING;
        let front_no_yawn = self.sit_front && !self.sit_yawn();
        let tail_beat = if since < 0.0 {
            0
        } else {
            (since / period).floor() as i64
        };
        let mut bi = 0;
        while bi < 2 {
            let b = tail_beat + bi;
            let open = b as f32 * period + at;
            Self::offer_edge_slice(
                soon,
                since,
                open,
                self.sit_tail_edges(b, swing_fits, front_no_yawn),
            );
            bi += 1;
        }
        let blink_period = BLINK_PERIOD + BLINK_PERIOD_STEP * (((self.settle_seed / 2) % 3) as f32);
        let blink_beat = if since < 0.0 {
            0
        } else {
            (since / blink_period).floor() as i64
        };
        let mut bi = 0;
        while bi < 2 {
            let b = blink_beat + bi;
            let open = b as f32 * blink_period + BLINK_AT;
            let edges: &[f32] = if self.beat_deal(b, 3) == 2 {
                &BLINK_EDGES_TWO
            } else {
                &BLINK_EDGES_ONE
            };
            Self::offer_edge_slice(soon, since, open, edges);
            bi += 1;
        }
        if self.sit_front {
            let look: &[f32] = if look_dur >= LOOK_DUR_LONG {
                &GLANCE_EDGES_LONG
            } else {
                &GLANCE_EDGES_SHORT
            };
            let mut bi = 0;
            while bi < 2 {
                let b = tail_beat + bi;
                if self.beat_deal(b, 3) != 0 {
                    let open = b as f32 * period + GLANCE_AT;
                    Self::offer_edge_slice(soon, since, open, look);
                }
                bi += 1;
            }
        }
    }

    #[inline]
    const fn offer_edge_slice(soon: &mut Soonest, since: f32, open: f32, edges: &[f32]) {
        let mut i = 0;
        while i < edges.len() {
            soon.at(open + edges[i] - since);
            i += 1;
        }
    }

    #[inline]
    const fn breath_sample(clock: f64) -> f32 {
        let t = (clock % (BREATH_PERIOD as f64)) as f32;
        crate::supernova::sin_cos_f32((TAU / BREATH_PERIOD) * t).0
    }

    #[inline]
    const fn swell_sample(clock: f64) -> f32 {
        let t = (clock % ((1.0 / PURR_HZ) as f64)) as f32;
        crate::supernova::sin_cos_f32(TAU * PURR_HZ * t).0
    }

    #[inline]
    const fn stand_scale(quiet: f32) -> (f32, f32) {
        let pre = ((quiet - (SIT_AFTER - SIT_FOLD)) * (1.0 / SIT_FOLD)).clamp(0.0, 1.0);
        (1.0 + 0.03 * pre, 1.0 - 0.05 * pre)
    }

    #[inline]
    const fn seat_scale(quiet: f32, clock: f64, tell: bool) -> (f32, f32) {
        let fold = (1.0 - (quiet - SIT_AFTER) * (1.0 / SIT_FOLD)).clamp(0.0, 1.0);
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
    const fn loaf_scale_at(loaf_after: f32, quiet: f32, clock: f64, tell: bool) -> (f32, f32) {
        Self::loaf_scale_and_beat_at(loaf_after, quiet, clock, tell).0
    }

    #[inline]
    const fn loaf_scale_and_beat(
        &self,
        quiet: f32,
        clock: f64,
        tell: bool,
    ) -> ((f32, f32), f32, f32) {
        Self::loaf_scale_and_beat_at(self.loaf_after(), quiet, clock, tell)
    }

    #[inline]
    const fn loaf_scale_and_beat_at(
        loaf_after: f32,
        quiet: f32,
        clock: f64,
        tell: bool,
    ) -> ((f32, f32), f32, f32) {
        let fold = (1.0 - (quiet - loaf_after) * (1.0 / SIT_FOLD)).clamp(0.0, 1.0);
        let mut scale_y = 1.0 + 0.05 * fold;
        let mut scale_x = 1.0 - 0.02 * fold;
        let since = (quiet - loaf_after).max(0.0);
        let beat = (since * (1.0 / FLICK_EVERY)).floor();
        let phase = since - beat * FLICK_EVERY;
        if beat >= 1.0 && phase < FLICK_DUR {
            let amp = TWITCH_BOB
                * crate::supernova::sin_cos_f32((core::f32::consts::PI / FLICK_DUR) * phase).0;
            let dir = if (beat as i64) & 1 == 0 { 1.0 } else { -1.0 };
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

enum PxStepSampler {
    Mote {
        m: Mote,
        u0: f32,
        inv_life: f32,
    },
    Sleep {
        c0: f64,
    },
    Stand {
        q: f32,
    },
    Seat {
        q: f32,
        c0: f64,
        tell: bool,
    },
    Loaf {
        loaf_after: f32,
        q: f32,
        c0: f64,
        tell: bool,
    },
}

impl PxStepSampler {
    #[inline]
    const fn at(&self, t: f32) -> (f32, f32) {
        match *self {
            Self::Mote { m, u0, inv_life } => m.drift(u0 + t * inv_life),
            Self::Sleep { c0 } => {
                let s = PetBrain::breath_sample(c0 + (t as f64));
                (1.0 - s * BREATH_DEPTH * 0.5, 1.0 + s * BREATH_DEPTH)
            }
            Self::Stand { q } => PetBrain::stand_scale(q + t),
            Self::Seat { q, c0, tell } => PetBrain::seat_scale(q + t, c0 + (t as f64), tell),
            Self::Loaf {
                loaf_after,
                q,
                c0,
                tell,
            } => PetBrain::loaf_scale_at(loaf_after, q + t, c0 + (t as f64), tell),
        }
    }
}

/// The scale a frame draws from a sampled pose scale: no taller than whole where the head law
/// holds the body in its row (`no_taller`, [`PetBrain::drawn_no_taller`]), then times its fit
/// ([`PetBrain::head_target`]), as the frame draws it.
#[inline]
const fn fitted((sx, sy): (f32, f32), (fit, no_taller): (f32, bool)) -> (f32, f32) {
    let sy = if no_taller { sy.min(1.0) } else { sy };
    (sx * fit, sy * fit)
}

#[inline]
const fn first_px_step(
    box_px: (f32, f32),
    horizon: f32,
    slope: (f32, f32),
    sampler: PxStepSampler,
    head: (f32, bool),
) -> Option<f32> {
    if horizon <= 0.0 || (slope.0 <= 0.0 && slope.1 <= 0.0) {
        return None;
    }
    let (w, h) = box_px;
    let fit = head.0;
    let slope = (slope.0 * fit, slope.1 * fit);
    let inv_slope_x = if slope.0 > 0.0 { 1.0 / slope.0 } else { 0.0 };
    let inv_slope_y = if slope.1 > 0.0 { 1.0 / slope.1 } else { 0.0 };
    let (sx0, sy0) = fitted(sampler.at(0.0), head);
    let (mut vx, mut vy) = (sx0 * w, sy0 * h);
    let rest = (vx.round(), vy.round());
    let mut prev = 0.0f32;
    let mut t = 0.0f32;
    let mut i = 0u32;
    while i < PX_MARCH_MAX {
        if i > 0 {
            let (sx, sy) = fitted(sampler.at(t), head);
            (vx, vy) = (sx * w, sy * h);
            if vx.round() != rest.0 || vy.round() != rest.1 {
                let (mut lo, mut hi) = (prev, t);
                let mut b = 0u32;
                while b < PX_BISECT {
                    let mid = 0.5 * (lo + hi);
                    let (msx, msy) = fitted(sampler.at(mid), head);
                    if (msx * w).round() == rest.0 && (msy * h).round() == rest.1 {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                    b += 1;
                }
                return Some(hi);
            }
        }
        if t >= horizon {
            return None;
        }
        let step_x = if inv_slope_x > 0.0 {
            (0.5 - (vx - rest.0).abs()).max(0.0) * inv_slope_x
        } else {
            f32::INFINITY
        };
        let step_y = if inv_slope_y > 0.0 {
            (0.5 - (vy - rest.1).abs()).max(0.0) * inv_slope_y
        } else {
            f32::INFINITY
        };
        let step = step_x.min(step_y);
        prev = t;
        t = (t + step.max(PX_MARCH_MIN)).min(horizon);
        i += 1;
    }
    Some(t)
}

#[cfg(test)]
#[path = "kitty_pet_tests.rs"]
mod tests;
