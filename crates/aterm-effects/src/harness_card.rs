// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure emitter for the Autopilot harness card decoration ([`CardPalette`](crate::harness_card::CardPalette)).

use std::sync::Arc;

use aterm_render::{DecoBlend, InkCell, add_sat, premul_rgb};
use aterm_time::Instant;
use std::ops::Range;
use std::time::Duration;

use crate::cli_marks::ICON_CONTRAST;
use crate::color_math::{
    floor_contrast_keeping_hue_with_ground_lum, hue_path_at, hue_path_endpoints, relative_luminance,
};
use crate::cursor_glow::InkRole;
use crate::effect_util::{
    STAR_ARM_FINE, STAR_ARM_HERO, STAR_ARM_INK, STAR_ARM_STD, STAR_CORE_ADD, STAR_GLINT_COV,
    TWINKLE_FLASH_BOUND_HZ, TWINKLE_FLASH_HZ, mix_rgb, star_arm, star_arm_px, twinkle_env,
    twinkle_peak, twinkle_rgb,
};
use crate::genome::mix;
use crate::rainbow_kitty::meteor::tri;
use crate::rainbow_kitty::ribbon::PAPER_MARK_CONTRAST;
#[cfg(test)]
use crate::rainbow_kitty::ribbon::paper_mark_ink;
use crate::ripple::{
    FLOW_STEP_MS, GLINT_CREST_DARK, GLINT_CREST_LIGHT, GLINT_PERIOD_MS, cell_distance, glint,
};
use crate::spectrum::{SPECTRUM_ANCHOR_AT, SPECTRUM_LUT_LEN, spectrum, spectrum_stop};

#[cfg(test)]
mod layer_tests;
mod layers;
#[cfg(test)]
mod oracle;

pub use layers::{CardArts, CardFrame, CardFrames, CardShape, KeptFrame, card_layer_id};

/// Intro episode duration in milliseconds before transitioning to flow ([`HarnessCard::flowing`]).
pub const EPISODE_MS: u64 = 6000;

/// Intro frame cadence in milliseconds (~30 fps).
pub const FRAME_MS: u64 = 33;

/// Maximum duration (ms) past the last drawn frame that [`HarnessCard::deadline`] requests wakes.
pub const SETTLE_GRACE_MS: u64 = 500;

/// Maximum duration (ms) after [`HarnessCard::arm`] while a held-back intro episode may start.
pub const OWED_MAX_MS: u64 = 45_000;

/// Maximum number of peeled star piece layers ([`aterm_render::fx::PieceArt`]) over one pixel.
pub const STAR_DEPTH_MAX: usize = 4;

const TONE_RECTS_MAX: usize = 3 * MAX_TRAIL_SEGMENTS as usize + 2;

/// Upper bound on total fx items emitted across both card layers in a single frame.
pub const CARD_ITEMS_MAX: usize = CARD_UNDER_ITEMS_MAX + CARD_OVER_ITEMS_MAX;

const CARD_UNDER_ITEMS_MAX: usize =
    2 * MAX_EDGE_SEGMENTS as usize + 2 + TRAIL_STOPS.len() * MAX_TRAIL_SEGMENTS as usize;

const CARD_OVER_ITEMS_MAX: usize = 1
    + 4 * RIM_COVER_MAX
    + STAR_DEPTH_MAX * TONE_RECTS_MAX * (STARS_MAX + BURST_STARS + GLINTS_MAX + SHED_SPARKLES);

/// Maximum number of cover rectangles used to lay the kitty rim art.
pub(crate) const RIM_COVER_MAX: usize = 32;

const _: () = assert!(
    CARD_UNDER_ITEMS_MAX == 2 * 48 + 2 + 6 * 16
        && CARD_OVER_ITEMS_MAX == 1 + 4 * 32 + 4 * (3 * 16 + 2) * (9 + 8 + 3 + 5)
        && CARD_ITEMS_MAX == CARD_UNDER_ITEMS_MAX + CARD_OVER_ITEMS_MAX
        && CARD_ITEMS_MAX < u16::MAX as usize,
    "the card's item bound is the design's (§4.8): the under content, then the kitty, the rim's \
     cover cut round the trail's hole and every star's tone rectangles, no art term"
);

/// Initial border hue period (ms) at the start of an intro episode.
pub const TRIP_MS: u64 = 3000;

const PHI_DRIFT: f32 = EPISODE_MS as f32 / (2.0 * TRIP_MS as f32);

const PHI_SETTLED: f32 = 0.0;

/// Continuous border and rim hue period (ms) during flow.
pub const FLOW_TRIP_MS: u64 = 4 * GLINT_PERIOD_MS;

const PHI_INTRO: f32 = PHI_DRIFT - EPISODE_MS as f32 / (2.0 * FLOW_TRIP_MS as f32);

const GLINT_FADE_MS: u64 = 3 * FLOW_STEP_MS;

/// Lead time (ms) placing a flow step's start ahead of an observed wand flare.
pub const FLARE_STEP_LEAD_MS: u64 = FLOW_SLACK_MS / 2;

const LOCK_GAIN: i64 = 4;

const LOCK_SLEW_MS: i64 = FLOW_STEP_MS as i64;

/// Slack window (ms) after a flow step's start before [`HarnessCard::deadline`] schedules a wake.
pub const FLOW_SLACK_MS: u64 = 40;

const FLOW_BLINK_PERIOD_MS: u64 = 2 * GLINT_PERIOD_MS;
const FLOW_BLINK_MS: u64 = 2 * FLOW_STEP_MS;

const GLINT_INTRO_FROM: f32 = 0.88;

const _: () = {
    assert!(
        FLOW_SLACK_MS < FLOW_STEP_MS,
        "the flow's wake lands inside its step"
    );
    assert!(
        FLARE_STEP_LEAD_MS > 0 && FLARE_STEP_LEAD_MS < FLOW_SLACK_MS,
        "Claude's repaints land inside the slack, after the step starts"
    );
    assert!(PHI_INTRO > 0.0, "the intro starts faster than the flow");
    assert!(
        1000.0 / (FLOW_BLINK_PERIOD_MS as f32) <= TWINKLE_FLASH_BOUND_HZ,
        "the flow's blink stays under the photosensitivity bound"
    );
};

const HEAD_LAPS: f32 = 2.0;

const HEAD_SIGMA: f32 = 0.03;

const HEAD_GAIN: f32 = 0.85;

const LIGHT_HEAD_DEEPEN: f32 = 0.3;

const MAX_EDGE_SEGMENTS: i64 = 48;

const BORDER_COV: u8 = 230;

const TRAIL_STOPS: [usize; 6] = [0, 1, 2, 3, 4, 6];

const TRAIL_SEG_CELLS: i64 = 2;

const MAX_TRAIL_SEGMENTS: i64 = 16;

const WAVE_STEP_MS: u64 = 200;

const WAVE_END_MS: u64 = EPISODE_MS - 600;

const SHIMMER_MS: f32 = 1500.0;

const SHIMMER_SIGMA: f32 = 0.12;

const SHIMMER_GAIN: f32 = 0.45;

const TRAIL_COV: u8 = 200;

const PRINT_CRUISE: [u32; 6] = [
    0x00E3_141B,
    0x00FF_7A00,
    0x00FF_C20E,
    0x0022_B848,
    0x0010_60D8,
    0x0082_26C0,
];

const PRINT_PERIDOT: u32 = 0x0080_C800;

const PRINT_TEAL: u32 = 0x0000_A6C4;

const PRINT_CRUISE_RAMP: [(usize, u32); 8] = [
    (stripe_at(0), PRINT_CRUISE[0]),
    (stripe_at(1), PRINT_CRUISE[1]),
    (stripe_at(2), PRINT_CRUISE[2]),
    ((stripe_at(2) + stripe_at(3)) / 2, PRINT_PERIDOT),
    (stripe_at(3), PRINT_CRUISE[3]),
    ((stripe_at(3) + stripe_at(4)) / 2, PRINT_TEAL),
    (stripe_at(4), PRINT_CRUISE[4]),
    (stripe_at(5), PRINT_CRUISE[5]),
];

#[inline]
const fn stripe_at(k: usize) -> usize {
    SPECTRUM_ANCHOR_AT[TRAIL_STOPS[k]]
}

const _: () = {
    let r = &PRINT_CRUISE_RAMP;
    assert!(r[0].0 == 0 && r[r.len() - 1].0 == SPECTRUM_LUT_LEN - 1);
    let mut i = 1;
    while i < r.len() {
        assert!(r[i - 1].0 < r[i].0, "the light ramp's knots run in order");
        i += 1;
    }
};

const PRINT_LAND: [u32; 6] = [
    0x00E3_141B,
    0x00F7_591C,
    0x00FF_8C00,
    0x00FF_B81C,
    0x00FF_6F59,
    0x00E8_336D,
];

const PRINT_OFF: [u32; 6] = [
    0x00C4_CAD4,
    0x00AA_B1BE,
    0x0093_9BA9,
    0x007C_8494,
    0x0065_6D7D,
    0x004F_5666,
];

const PRINT_ALPHA: f32 = 255.0;

const STAR_PRINT_FULL: f32 = STAR_COV * STAR_STEADY_ENV;

const LIGHT_GLINT_EDGE: f32 = 0.5;

const LIGHT_LINE_MAX_LUMA: f32 = 180.0;

/// Kitty sprite width in art pixels at both heights.
pub const KITTY_W: usize = 24;

/// Two-row kitty sprite height in art pixels.
pub const KITTY_H: usize = 14;

/// Three-row tall kitty sprite height in art pixels.
pub const KITTY_TALL_H: usize = 21;

const KITTY_HALF: usize = KITTY_H / 2;

type KittyFrame = [&'static str; KITTY_H];

type KittyTallFrame = [&'static str; KITTY_TALL_H];

const KITTY_PALETTE: &[u8] = b".ocbkenw";

const KITTY_OPEN: KittyFrame = [
    "..............o......o..",
    ".oo..........ono....ono.",
    "oco.........onnoooooonno",
    "oco.........occcccccccco",
    ".ocoooooooooocccccccccco",
    "..occcccccccocekccccekco",
    "..occcccccccocekccccekco",
    "..occcccccccoccbbnnbbcco",
    "..ocbbbbbbccoccbbbbbbcco",
    "..ocbbbbbbcccoooooooooo.",
    "..occccccccccccco.......",
    "..occoccoccoccoo........",
    "..occoccoccocco.........",
    "...oo.oo.oo.oo..........",
];

const KITTY_BLINK: KittyFrame = [
    "..............o......o..",
    ".oo..........ono....ono.",
    "oco.........onnoooooonno",
    "oco.........occcccccccco",
    ".ocoooooooooocccccccccco",
    "..occcccccccocccccccccco",
    "..occcccccccocooccccooco",
    "..occcccccccoccbbnnbbcco",
    "..ocbbbbbbccoccbbbbbbcco",
    "..ocbbbbbbcccoooooooooo.",
    "..occccccccccccco.......",
    "..occoccoccoccoo........",
    "..occoccoccocco.........",
    "...oo.oo.oo.oo..........",
];

const KITTY_FLICK: KittyFrame = [
    "..............o......o..",
    ".............ono....ono.",
    "............onnoooooonno",
    "............occcccccccco",
    "...oooooooooocccccccccco",
    "ooocccccccccocekccccekco",
    "ccccccccccccocekccccekco",
    "ooocccccccccoccbbnnbbcco",
    "..ocbbbbbbccoccbbbbbbcco",
    "..ocbbbbbbcccoooooooooo.",
    "..occccccccccccco.......",
    "..occoccoccoccoo........",
    "..occoccoccocco.........",
    "...oo.oo.oo.oo..........",
];

const KITTY_TALL_OPEN: KittyTallFrame = [
    "..............o......o..",
    ".oo..........ono....ono.",
    "oco.........onno....onno",
    "oco.........onnoooooonno",
    "oco.........occcccccccco",
    "oco.........occcccccccco",
    ".ocoooooooooocccccccccco",
    "..occcccccccocewccccewco",
    "..occcccccccocekccccekco",
    "..occcccccccocekccccekco",
    "..occcccccccoccbbnnbbcco",
    "..occcccccccoccbbbbbbcco",
    "..ocbbbbbbccoccbbbbbbcco",
    "..ocbbbbbbcccoooooooooo.",
    "..ocbbbbbbcccccco.......",
    "..occccccccccccco.......",
    "..occccccccccccco.......",
    "..occoccoccoccoo........",
    "..occoccoccocco.........",
    "..occoccoccocco.........",
    "...oo.oo.oo.oo..........",
];

const KITTY_TALL_BLINK: KittyTallFrame = [
    "..............o......o..",
    ".oo..........ono....ono.",
    "oco.........onno....onno",
    "oco.........onnoooooonno",
    "oco.........occcccccccco",
    "oco.........occcccccccco",
    ".ocoooooooooocccccccccco",
    "..occcccccccocccccccccco",
    "..occcccccccocooccccooco",
    "..occcccccccocccccccccco",
    "..occcccccccoccbbnnbbcco",
    "..occcccccccoccbbbbbbcco",
    "..ocbbbbbbccoccbbbbbbcco",
    "..ocbbbbbbcccoooooooooo.",
    "..ocbbbbbbcccccco.......",
    "..occccccccccccco.......",
    "..occccccccccccco.......",
    "..occoccoccoccoo........",
    "..occoccoccocco.........",
    "..occoccoccocco.........",
    "...oo.oo.oo.oo..........",
];

const KITTY_TALL_FLICK: KittyTallFrame = [
    "..............o......o..",
    ".............ono....ono.",
    "............onno....onno",
    "............onnoooooonno",
    "............occcccccccco",
    "............occcccccccco",
    "...oooooooooocccccccccco",
    "ooocccccccccocewccccewco",
    "ccccccccccccocekccccekco",
    "ooocccccccccocekccccekco",
    "..occcccccccoccbbnnbbcco",
    "..occcccccccoccbbbbbbcco",
    "..ocbbbbbbccoccbbbbbbcco",
    "..ocbbbbbbcccoooooooooo.",
    "..ocbbbbbbcccccco.......",
    "..occccccccccccco.......",
    "..occccccccccccco.......",
    "..occoccoccoccoo........",
    "..occoccoccocco.........",
    "..occoccoccocco.........",
    "...oo.oo.oo.oo..........",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KittyPose {
    Open = 0,
    Blink = 1,
    Flick = 2,
}

#[derive(Clone, Copy, Debug)]
struct KittyArt {
    open: &'static [&'static str],
    blink: &'static [&'static str],
    flick: &'static [&'static str],
    eyes: (usize, usize),
    body: (usize, usize),
}

impl KittyArt {
    #[inline]
    const fn frame(&self, pose: KittyPose) -> &'static [&'static str] {
        match pose {
            KittyPose::Open => self.open,
            KittyPose::Blink => self.blink,
            KittyPose::Flick => self.flick,
        }
    }

    #[inline]
    const fn h(&self) -> usize {
        self.open.len()
    }

    #[inline]
    const fn rows(&self) -> usize {
        self.h() / KITTY_HALF
    }

    #[inline]
    const fn rear(&self) -> usize {
        let frames = [self.open, self.blink, self.flick];
        let mut rear = 0;
        let mut f = 0;
        while f < frames.len() {
            let mut r = self.body.0;
            while r < self.body.0 + self.body.1 {
                let first = first_art(frames[f][r].as_bytes());
                if first > rear {
                    rear = first;
                }
                r += 1;
            }
            f += 1;
        }
        rear
    }
}

const KITTY_SHORT: KittyArt = KittyArt {
    open: &KITTY_OPEN,
    blink: &KITTY_BLINK,
    flick: &KITTY_FLICK,
    eyes: (5, 7),
    body: (4, 7),
};

const KITTY_TALL: KittyArt = KittyArt {
    open: &KITTY_TALL_OPEN,
    blink: &KITTY_TALL_BLINK,
    flick: &KITTY_TALL_FLICK,
    eyes: (7, 10),
    body: (6, 12),
};

const KITTY_TAIL_COLS: usize = 4;

const fn frame_is_well_formed(frame: &[&str], h: usize) -> bool {
    if frame.len() != h {
        return false;
    }
    let mut r = 0;
    while r < h {
        let row = frame[r].as_bytes();
        if row.len() != KITTY_W {
            return false;
        }
        let mut c = 0;
        while c < KITTY_W {
            let mut known = false;
            let mut k = 0;
            while k < KITTY_PALETTE.len() {
                known |= row[c] == KITTY_PALETTE[k];
                k += 1;
            }
            if !known {
                return false;
            }
            c += 1;
        }
        r += 1;
    }
    true
}

const fn differ_only_in(
    a: &[&str],
    b: &[&str],
    rows: (usize, usize),
    cols: (usize, usize),
) -> bool {
    let mut r = 0;
    while r < a.len() {
        let (x, y) = (a[r].as_bytes(), b[r].as_bytes());
        let mut c = 0;
        while c < KITTY_W {
            let inside = r >= rows.0 && r < rows.1 && c >= cols.0 && c < cols.1;
            if !inside && x[c] != y[c] {
                return false;
            }
            c += 1;
        }
        r += 1;
    }
    true
}

const fn first_art(row: &[u8]) -> usize {
    let mut c = 0;
    while c < KITTY_W && row[c] == b'.' {
        c += 1;
    }
    c
}

const fn trail_meets_flush(frame: &[&str], body: (usize, usize), rear: usize) -> bool {
    let mut r = body.0;
    while r < body.0 + body.1 {
        let row = frame[r].as_bytes();
        let mut c = first_art(row);
        if c > rear {
            return false;
        }
        while c < rear {
            if row[c] == b'.' {
                return false;
            }
            c += 1;
        }
        r += 1;
    }
    true
}

const fn count_in(frame: &[&str], rows: (usize, usize), ch: u8) -> usize {
    let mut n = 0;
    let mut r = rows.0;
    while r < rows.1 {
        let row = frame[r].as_bytes();
        let mut c = 0;
        while c < KITTY_W {
            n += (row[c] == ch) as usize;
            c += 1;
        }
        r += 1;
    }
    n
}

const fn two_eyes_of_iris_and_pupil(frame: &[&str], rows: (usize, usize)) -> bool {
    let mut eyes = 0;
    let (mut in_eye, mut iris, mut pupil) = (false, false, false);
    let mut c = 0;
    while c <= KITTY_W {
        let (mut e, mut k, mut any) = (false, false, false);
        let mut r = rows.0;
        while c < KITTY_W && r < rows.1 {
            let b = frame[r].as_bytes()[c];
            e = e || b == b'e';
            k = k || b == b'k';
            any = any || b == b'e' || b == b'k' || b == b'w';
            r += 1;
        }
        if any {
            if !in_eye {
                (in_eye, iris, pupil) = (true, false, false);
            }
            iris = iris || e;
            pupil = pupil || k;
        } else if in_eye {
            if !(iris && pupil) {
                return false;
            }
            eyes += 1;
            in_eye = false;
        }
        c += 1;
    }
    eyes == 2
}

const _: () = {
    let arts = [KITTY_SHORT, KITTY_TALL];
    let heights = [KITTY_H, KITTY_TALL_H];
    let mut i = 0;
    while i < arts.len() {
        let art = arts[i];
        let h = heights[i];
        assert!(
            frame_is_well_formed(art.open, h)
                && frame_is_well_formed(art.blink, h)
                && frame_is_well_formed(art.flick, h),
            "every kitty row is KITTY_W palette characters, the art's height of them"
        );
        assert!(
            art.h().is_multiple_of(KITTY_HALF) && art.rows() == 2 + i,
            "the kitty is a whole number of cell rows: two, then three"
        );
        assert!(
            differ_only_in(art.open, art.blink, art.eyes, (0, KITTY_W))
                && count_in(art.blink, art.eyes, b'e') == 0
                && count_in(art.blink, art.eyes, b'k') == 0
                && count_in(art.blink, art.eyes, b'w') == 0,
            "the blink is the open frame with its eyes closed"
        );
        assert!(
            two_eyes_of_iris_and_pupil(art.open, art.eyes),
            "the open cat has two eyes, each an iris and a pupil"
        );
        assert!(
            differ_only_in(art.open, art.flick, (0, h), (0, KITTY_TAIL_COLS)),
            "the flick is the open frame with its tail moved"
        );
        assert!(
            art.body.0 + art.body.1 <= h && art.body.1 >= 6,
            "the body band lies in the art and holds six stripes"
        );
        let rear = art.rear();
        assert!(
            rear < KITTY_TAIL_COLS
                && trail_meets_flush(art.open, art.body, rear)
                && trail_meets_flush(art.blink, art.body, rear)
                && trail_meets_flush(art.flick, art.body, rear),
            "the trail meets the cat's rear flush, no hole in it"
        );
        i += 1;
    }
    assert!(
        2 * KITTY_HALF == KITTY_H && 3 * KITTY_HALF == KITTY_TALL_H,
        "the kitty splits evenly across its rows"
    );
};

const BLINK_AT_MS: u64 = 1250;
const BLINK_PERIOD_MS: u64 = 2500;
const BLINK_MS: u64 = 160;

const FLICK_AT_MS: u64 = 400;
const FLICK_PERIOD_MS: u64 = 2500;
const FLICK_MS: u64 = 240;

const _: () = assert!(
    FLICK_AT_MS + FLICK_MS <= BLINK_AT_MS
        && BLINK_AT_MS + BLINK_MS <= FLICK_AT_MS + FLICK_PERIOD_MS
        && BLINK_PERIOD_MS == FLICK_PERIOD_MS,
    "a blink and a flick never share a frame"
);

const NOSE_PINK: u32 = 0x00FF_8FAB;

const PUPIL: u32 = 0x000A_0A0E;

const RIM_COV: u8 = BORDER_COV;

const STARS_MIN: u64 = 5;
const STARS_SPAN: u64 = 5;
const STARS_MAX: usize = (STARS_MIN + STARS_SPAN - 1) as usize;

const STAR_TRIES: u64 = 24;

const STAR_RATIOS: [f32; 4] = [STAR_ARM_STD, STAR_ARM_FINE, STAR_ARM_HERO, STAR_ARM_FINE];

const STAR_COV: f32 = 235.0;

const STAR_STEADY_ENV: f32 = 0.8;

const STAR_SETTLE_SPAN: f32 = 0.2;

/// Number of stars in the landing burst ring.
pub const BURST_STARS: usize = 8;

const BURST_MS: u64 = 900;

/// Duration (ms) of the kitty's initial ride-in from the trail start to its seat.
pub const RIDE_MS: u64 = 1200;

const SHED_SPARKLES: usize = 5;
const SHED_MS: u64 = 450;
const SHED_COV: f32 = 220.0;

const STAR_REVEAL_MS: f32 = 240.0;

const _: () = assert!(
    RIDE_MS + BURST_MS < EPISODE_MS && RIDE_MS + SHED_MS < EPISODE_MS,
    "the ride, its sparkles and its burst end inside the episode"
);

const BURST_COV: f32 = 255.0;

const GLINTS_MAX: usize = 3;

const GLINT_CELLS: i64 = 6;

const GLINT_COV: f32 = 235.0;

const GLINT_FLARE_DOTS: f32 = 0.5;

const LINE_DRIFT: f32 = 1.0;
const LINE_SWEEPS: f32 = 2.0;
const LINE_SWEEP_SIGMA: f32 = 0.08;
const LINE_SWEEP_GAIN: f32 = 0.55;
const LINE_SWEEP_GAIN_LIGHT: f32 = 0.3;
/// Minimum WCAG relative luminance for dark-theme line ink.
pub(crate) const LINE_DARK_LUMA_FLOOR: f32 = 0.30;
/// Minimum WCAG contrast of a line letter on the ground it is laid on, either polarity: the
/// print law's floor ([`PAPER_MARK_CONTRAST`]), which a letter lifted to
/// [`LINE_DARK_LUMA_FLOOR`] misses on a dark ground that is not near-black.
const LINE_CONTRAST: f32 = PAPER_MARK_CONTRAST;
const LINE_HERO_SPREAD: f32 = 1.0;
const LINE_HERO_DARK_LUMA_FLOOR: f32 = 0.36;
const LINE_HERO_SWEEP_GAIN: f32 = GLINT_CREST_DARK;
const LINE_INK_STEPS: usize = 256;

#[derive(Clone, Copy, Debug)]
struct LineInks {
    key: Option<(CardPalette, u32)>,
    ink: [u32; LINE_INK_STEPS + 1],
}

impl LineInks {
    const EMPTY: Self = Self {
        key: None,
        ink: [0; LINE_INK_STEPS + 1],
    };
}

impl Default for LineInks {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl LineInks {
    const INV_LINE_INK_STEPS: f32 = 1.0 / LINE_INK_STEPS as f32;

    #[inline]
    const fn solve(&mut self, palette: CardPalette, ground: u32) {
        let ground = ground & 0x00FF_FFFF;
        if matches!(self.key, Some((p, g)) if p as u8 == palette as u8 && g == ground) {
            return;
        }
        let table = crate::color_math::relative_luminance_table();
        let y_bg = crate::color_math::relative_luminance_with(table, ground);
        let mut i = 0;
        while i < self.ink.len() {
            self.ink[i] =
                palette.text_at_with_bg_lum(table, i as f32 * Self::INV_LINE_INK_STEPS, y_bg);
            i += 1;
        }
        self.key = Some((palette, ground));
    }

    #[cfg(test)]
    #[inline]
    fn at(&self, palette: CardPalette, ground: u32, at: f32) -> u32 {
        let ground = ground & 0x00FF_FFFF;
        let table = crate::color_math::relative_luminance_table();
        let y_bg = crate::color_math::relative_luminance_with(table, ground);
        self.at_with_ground_lum(table, palette, ground, y_bg, at)
    }

    #[inline]
    fn at_with_ground_lum(
        &self,
        table: &crate::color_math::RelativeLuminanceTable,
        palette: CardPalette,
        ground: u32,
        y_bg: f32,
        at: f32,
    ) -> u32 {
        let step = line_ink_step(at);
        if self.key == Some((palette, ground)) {
            self.ink[step]
        } else {
            #[cfg(any(test, debug_assertions))]
            LINE_INK_SOLVES.with(|n| n.set(n.get() + 1));
            palette.text_at_with_bg_lum(table, step as f32 * Self::INV_LINE_INK_STEPS, y_bg)
        }
    }
}

/// The step of [`LineInks`]' table a light-ground line letter at palette position `at` is read
/// at.
#[inline]
const fn line_ink_step(at: f32) -> usize {
    let step = (at.clamp(0.0, 1.0) * LINE_INK_STEPS as f32).round() as usize;
    if step < LINE_INK_STEPS {
        step
    } else {
        LINE_INK_STEPS
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static LINE_INK_SOLVES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test seam returning and resetting the count of light-ground ink solves on this thread.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn take_line_ink_solves() -> usize {
    LINE_INK_SOLVES.with(|n| n.replace(0))
}

const _: () = {
    assert!(
        TWINKLE_FLASH_HZ <= TWINKLE_FLASH_BOUND_HZ,
        "the card's stars twinkle on the family law, under the bound"
    );
    assert!(
        1000.0 / (2.0 * WAVE_STEP_MS as f32) <= TWINKLE_FLASH_BOUND_HZ,
        "the trail's step wave cycles under the photosensitivity bound"
    );
    assert!(
        LINE_HERO_SWEEP_GAIN > LINE_SWEEP_GAIN && LINE_HERO_DARK_LUMA_FLOOR > LINE_DARK_LUMA_FLOOR,
        "the hero word flares harder and stands brighter than the line"
    );
    assert!(
        1000.0 / SHIMMER_MS <= TWINKLE_FLASH_BOUND_HZ,
        "a glint flares under the photosensitivity bound"
    );
};

const SUNSET: [u32; 4] = [0x00FF_3B30, 0x00FF_8A1F, 0x00FF_C83D, 0x00FF_6F91];

const MOONBOW: [u32; 4] = [0x00FF_FFFF, 0x00E4_EAF3, 0x00C3_CCDA, 0x00A2_ADBE];

const WHITE: u32 = 0x00FF_FFFF;

/// Color palette corresponding to the active Autopilot mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CardPalette {
    /// Full rainbow spectrum for Cruise mode.
    #[default]
    Cruise,
    /// Sunset ramp (red to rose) for Land mode.
    Land,
    /// Moonbow ramp (white to steel on dark, pearl to graphite in print) for Off mode.
    Off,
}

impl CardPalette {
    #[inline]
    const fn at(self, t: f32) -> u32 {
        match self {
            Self::Cruise => spectrum(t),
            Self::Land => sunset(t),
            Self::Off => moonbow(t),
        }
    }

    #[inline]
    const fn art_at(self, t: f32, dark: bool) -> u32 {
        match (self, dark) {
            (_, true) => self.at(t),
            (Self::Cruise, false) => knot_ramp(&PRINT_CRUISE_RAMP, t),
            (Self::Land, false) => even_ramp(&PRINT_LAND, t),
            (Self::Off, false) => even_ramp(&PRINT_OFF, t),
        }
    }

    /// Returns the card art color at normalized position `t` (`0.0..=1.0`) for `dark` ground.
    #[must_use]
    #[inline]
    pub const fn art(self, t: f32, dark: bool) -> u32 {
        self.art_at(t, dark)
    }

    /// Returns the legible text color at normalized position `t` (`0.0..=1.0`) by theme polarity.
    #[must_use]
    #[inline]
    pub const fn letter(self, t: f32, dark: bool) -> u32 {
        let base = self.at(t);
        if dark {
            lift_to_luminance(base, LINE_DARK_LUMA_FLOOR)
        } else {
            InkRole::OverText.ink(base)
        }
    }

    #[inline]
    const fn letter_dark_with(
        self,
        table: &crate::color_math::RelativeLuminanceTable,
        t: f32,
    ) -> u32 {
        lift_to_luminance_with(table, self.at(t), LINE_DARK_LUMA_FLOOR)
    }

    /// Returns the resting line letter color at `t` (`0.0..=1.0`) against `ground` (`0x00RRGGBB`),
    /// reading at least 4.5:1 on it: on a dark ground the lifted letter floored along its hue, on
    /// paper the print law's ink.
    #[must_use]
    #[inline]
    pub const fn letter_on(self, t: f32, ground: u32) -> u32 {
        let ground = ground & 0x00FF_FFFF;
        let table = crate::color_math::relative_luminance_table();
        let lg = crate::color_math::relative_luminance_with(table, ground);
        if lg < 0.5 {
            line_floor(table, self.letter_dark_with(table, t), lg)
        } else {
            self.text_at_with_bg_lum(table, t, lg)
        }
    }

    /// Returns the hero word's settled letter at `k` across it (`0.0..=1.0`) against `ground`
    /// (`0x00RRGGBB`): the ink the launch card's status line gives its hero at rest
    /// ([`HarnessCard::line_ink_into`] at the settled clock), `tri(k)` of the palette, on a dark
    /// ground lifted to the hero's luma floor and floored along its hue to 4.5:1, on paper the
    /// print law's ink.
    #[must_use]
    #[inline]
    pub const fn hero_on(self, k: f32, ground: u32) -> u32 {
        let ground = ground & 0x00FF_FFFF;
        let table = crate::color_math::relative_luminance_table();
        let lg = crate::color_math::relative_luminance_with(table, ground);
        let place = tri(k * LINE_HERO_SPREAD + PHI_SETTLED);
        if lg < 0.5 {
            line_floor(
                table,
                lift_to_luminance_with(table, self.at(place), LINE_HERO_DARK_LUMA_FLOOR),
                lg,
            )
        } else {
            self.text_at_with_bg_lum(
                table,
                line_ink_step(place) as f32 * LineInks::INV_LINE_INK_STEPS,
                lg,
            )
        }
    }

    /// Returns the normalized palette position for trail stripe `k` (`0..6`).
    #[must_use]
    #[inline]
    pub const fn stripe_t(self, k: usize) -> f32 {
        let k = k % TRAIL_STOPS.len();
        match self {
            Self::Cruise => stripe_at(k) as f32 / (SPECTRUM_LUT_LEN - 1) as f32,
            Self::Land | Self::Off => k as f32 / (TRAIL_STOPS.len() - 1) as f32,
        }
    }

    #[inline]
    const fn stripe(self, k: usize, dark: bool) -> u32 {
        let k = k % TRAIL_STOPS.len();
        match (self, dark) {
            (Self::Cruise, true) => spectrum_stop(TRAIL_STOPS[k]),
            (Self::Land, true) => sunset(k as f32 / (TRAIL_STOPS.len() - 1) as f32),
            (Self::Off, true) => moonbow(k as f32 / (TRAIL_STOPS.len() - 1) as f32),
            (Self::Cruise, false) => PRINT_CRUISE[k],
            (Self::Land, false) => PRINT_LAND[k],
            (Self::Off, false) => PRINT_OFF[k],
        }
    }

    #[inline]
    const fn mark(self, k: usize) -> u32 {
        match self {
            Self::Cruise => spectrum_stop(k % 7),
            Self::Land => SUNSET[k & (SUNSET.len() - 1)],
            Self::Off => MOONBOW[k & (MOONBOW.len() - 1)],
        }
    }

    /// The light a dark scene's glint rests at, before its flare mixes it toward white.
    #[inline]
    const fn glint_rest(self) -> u32 {
        match self {
            Self::Off => MOONBOW[1],
            Self::Cruise | Self::Land => twinkle_rgb(true),
        }
    }

    #[inline]
    const fn star_ink(self, k: usize) -> u32 {
        print_line(self.stripe(k, false))
    }

    #[cfg(test)]
    #[inline]
    const fn text_at(self, t: f32, ground: u32) -> u32 {
        match self {
            Self::Off => paper_silver_ink(self.art_at(t, false), ground),
            Self::Cruise | Self::Land => paper_mark_ink(self.art_at(t, false), ground),
        }
    }

    #[inline]
    const fn text_at_with_bg_lum(
        self,
        table: &crate::color_math::RelativeLuminanceTable,
        t: f32,
        y_bg: f32,
    ) -> u32 {
        match self {
            Self::Off => paper_silver_ink_with_bg_lum(table, self.art_at(t, false), y_bg),
            Self::Cruise | Self::Land => crate::rainbow_kitty::ribbon::paper_mark_ink_with_bg_lum(
                table,
                self.art_at(t, false),
                y_bg,
            ),
        }
    }
}

#[cfg(test)]
#[inline]
const fn paper_silver_ink(rgb: u32, ground: u32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let y_bg = crate::color_math::relative_luminance_with(table, ground & 0x00FF_FFFF);
    paper_silver_ink_with_bg_lum(table, rgb, y_bg)
}

#[inline]
const fn paper_silver_ink_with_bg_lum(
    table: &crate::color_math::RelativeLuminanceTable,
    rgb: u32,
    y_bg: f32,
) -> u32 {
    let y_bg_plus = y_bg + 0.05;
    #[inline]
    const fn holds(
        table: &crate::color_math::RelativeLuminanceTable,
        y_bg: f32,
        y_bg_plus: f32,
        c: u32,
    ) -> bool {
        let y = crate::color_math::relative_luminance_with(table, c);
        let (num, den) = if y >= y_bg {
            (y + 0.05, y_bg_plus)
        } else {
            (y_bg_plus, y + 0.05)
        };
        num / den >= PAPER_MARK_CONTRAST
    }
    if holds(table, y_bg, y_bg_plus, rgb) {
        return rgb;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (0u32, rgb);
    let mut i = 0;
    while i < 24 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = mix_rgb(rgb, 0, 1.0 - mid);
        let passes = if mid_rgb == lo_rgb {
            true
        } else if mid_rgb == hi_rgb {
            false
        } else {
            holds(table, y_bg, y_bg_plus, mid_rgb)
        };
        if passes {
            lo = mid;
            lo_rgb = mid_rgb;
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
        }
        if i >= 1 && lo_rgb == hi_rgb {
            break;
        }
        i += 1;
    }
    lo_rgb
}

#[inline]
const fn sunset(t: f32) -> u32 {
    even_ramp(&SUNSET, t)
}

#[inline]
const fn moonbow(t: f32) -> u32 {
    even_ramp(&MOONBOW, t)
}

#[inline]
const fn even_ramp(stops: &[u32], t: f32) -> u32 {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let x = t * (stops.len() - 1) as f32;
    let u = x as usize;
    let i = if u < stops.len() - 2 {
        u
    } else {
        stops.len() - 2
    };
    mix_rgb(stops[i], stops[i + 1], x - i as f32)
}

#[inline]
const fn knot_ramp(knots: &[(usize, u32)], t: f32) -> u32 {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let x = t * (SPECTRUM_LUT_LEN - 1) as f32;
    let mut i = 1;
    while i + 1 < knots.len() && x > knots[i].0 as f32 {
        i += 1;
    }
    let ((a, ca), (b, cb)) = (knots[i - 1], knots[i]);
    mix_rgb(ca, cb, (x - a as f32) / (b - a) as f32)
}

/// Darkens `rgb` toward black until its Rec. 709 luma is at most [`LIGHT_LINE_MAX_LUMA`].
#[inline]
pub(crate) const fn print_line(rgb: u32) -> u32 {
    let r = ((rgb >> 16) & 0xff) as f32;
    let g = ((rgb >> 8) & 0xff) as f32;
    let b = (rgb & 0xff) as f32;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if y <= LIGHT_LINE_MAX_LUMA {
        rgb
    } else {
        mix_rgb(rgb, 0, 1.0 - LIGHT_LINE_MAX_LUMA / y)
    }
}

/// Card geometry, theme polarity, and placement in the target stream's pixel space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardScene {
    /// Pixel X of grid cell `(0, 0)`'s left edge.
    pub origin_x: i32,
    /// Pixel Y of grid cell `(0, 0)`'s top edge.
    pub origin_y: i32,
    /// Cell width in pixels.
    pub cw: u16,
    /// Cell height in pixels.
    pub ch: u16,
    /// Stream row index of grid row 0.
    pub row_base: u16,
    /// Available padding pixels left of column 0.
    pub pad_left: u16,
    /// Blank pane pixels available above the banner row (ignored when `top == 0`).
    pub pad_above: u16,
    /// Blank pane pixels available below the last boxed row.
    pub pad_below: u16,
    /// First boxed grid row (the banner row).
    pub top: u16,
    /// Number of grid rows enclosed by the box.
    pub rows: u16,
    /// Inner box width in cells from column 0 (the right border is drawn in column `cols`).
    pub cols: u16,
    /// Half-open pixel clip rect `(x0, y0, x1, y1)` in stream pixel space.
    pub clip: (i32, i32, i32, i32),
    /// Optional `[start_col, end_col)` column range of the rainbow trail.
    pub trail: Option<(u16, u16)>,
    /// Optional left column of the kitty sprite on rows `top..top + kitty_rows`.
    pub kitty_col: Option<u16>,
    /// Height of the kitty in rows (`3` for [`KITTY_TALL_H`], otherwise 2-row [`KITTY_H`]).
    pub kitty_rows: u8,
    /// Active Autopilot color palette.
    pub palette: CardPalette,
    /// Theme polarity (`true` for additive dark ground, a mark whose light cannot read on it
    /// lifted or printed ([`dark_mark`]); `false` for source-over light ground).
    pub dark: bool,
    /// Pane background color (`0x00RRGGBB`).
    pub ground: u32,
    /// Serious mode flag (renders only a static border when `true`).
    pub serious: bool,
    /// Kitty coat color (`0x00RRGGBB`).
    pub coat: u32,
    /// Kitty eye color (`0x00RRGGBB`).
    pub eye: u32,
    /// Optional `(col, row)` origin of Claude's wand ripple; defaults to the box's inner top-left.
    pub wand: Option<(u16, u16)>,
}

#[inline]
const fn stroke_px(ch: u16) -> u16 {
    let s = ch / 14;
    if s > 1 { s } else { 1 }
}

#[inline]
const fn kitty_px(ch: u16) -> u16 {
    ch.saturating_sub(stroke_px(ch)) / KITTY_HALF as u16
}

/// Returns the kitty width in whole cells at cell size `cw × ch`, or `0` if cells are too small.
#[must_use]
#[inline]
pub const fn kitty_cols(cw: u16, ch: u16) -> u16 {
    let p = kitty_px(ch) as u32;
    if p == 0 || cw == 0 {
        return 0;
    }
    let w = (KITTY_W as u32 * p).div_ceil(cw as u32);
    if w > u16::MAX as u32 {
        u16::MAX
    } else {
        w as u16
    }
}

/// State machine tracking the harness card's intro episode, ride-in, and continuous ripple flow.
#[derive(Clone, Copy, Debug)]
pub struct HarnessCard {
    episode: Episode,
    seed: u64,
    palette: CardPalette,
    inks: LineInks,
    still: bool,
    drew: Drew,
    ride: Ride,
    flows: bool,
    glint0: Option<Instant>,
    hue0: Option<Instant>,
    fade0: Option<Instant>,
    drawn: Option<(Instant, Option<(u64, u32)>)>,
    grid_gen: u32,
}

impl Default for HarnessCard {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Drew {
    #[default]
    Settled,
    Intro,
    Flow,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Ride {
    #[default]
    Ahead,
    Since(Instant),
    Done,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Episode {
    #[default]
    Idle,
    Armed(Instant),
    Owed(Instant),
    Playing(Instant),
}

#[derive(Clone, Copy, Debug)]
struct Clock {
    t_ms: Option<u64>,
    ride_ms: Option<u64>,
    glint: Option<(u64, f32)>,
    flow: Option<Flow>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Flow {
    step: u64,
    hue_ms: u64,
}

impl Clock {
    const SETTLED: Self = Self {
        t_ms: None,
        ride_ms: None,
        glint: None,
        flow: None,
    };

    #[inline]
    const fn intro(t: u64, ride_ms: Option<u64>) -> Self {
        let mut clock = Self {
            t_ms: Some(t),
            ride_ms,
            glint: None,
            flow: None,
        };
        clock.glint = Some((on_grid(t), clock.intro_rise()));
        clock
    }

    #[cfg(test)]
    const fn riding(t: u64) -> Self {
        Self::intro(t, Some(t))
    }

    #[cfg(test)]
    const fn seated(t: u64) -> Self {
        Self::intro(t, None)
    }

    #[cfg(test)]
    const fn flowing(ms: u64) -> Self {
        let step = ms / FLOW_STEP_MS;
        Self {
            t_ms: None,
            ride_ms: None,
            glint: Some((step * FLOW_STEP_MS, 1.0)),
            flow: Some(Flow {
                step,
                hue_ms: step * FLOW_STEP_MS,
            }),
        }
    }

    #[inline]
    const fn u(self) -> f32 {
        const INV_EPISODE_MS: f32 = 1.0 / EPISODE_MS as f32;
        match self.t_ms {
            Some(t) => (t as f32 * INV_EPISODE_MS).min(1.0),
            None => 1.0,
        }
    }

    #[inline]
    const fn intro_rise(self) -> f32 {
        const INV_INTRO_SPAN: f32 = 1.0 / (1.0 - GLINT_INTRO_FROM);
        smooth((self.u() - GLINT_INTRO_FROM) * INV_INTRO_SPAN)
    }

    #[inline]
    const fn phi(self) -> f32 {
        const INV_TRIP: f32 = 1.0 / FLOW_TRIP_MS as f32;
        match (self.t_ms, self.flow) {
            (Some(t), _) => {
                let v = 1.0 - self.u();
                PHI_SETTLED + PHI_INTRO * v * v - t as f32 * INV_TRIP
            }
            (None, Some(f)) => PHI_SETTLED - (f.hue_ms % (2 * FLOW_TRIP_MS)) as f32 * INV_TRIP,
            (None, None) => PHI_SETTLED,
        }
    }

    #[inline]
    const fn line_drift(self, drift: f32) -> f32 {
        if self.t_ms.is_none() {
            return 0.0;
        }
        let v = 1.0 - self.u();
        drift * v * v
    }

    #[inline]
    const fn head_env(self) -> f32 {
        const INV_HEAD_RISE: f32 = 1.0 / 0.04;
        const INV_HEAD_FALL: f32 = 1.0 / 0.12;
        if self.t_ms.is_none() {
            return 0.0;
        }
        let u = self.u();
        smooth(u * INV_HEAD_RISE) * smooth((1.0 - u) * INV_HEAD_FALL)
    }
}

#[inline]
const fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

#[inline]
const fn on_grid(ms: u64) -> u64 {
    ms / FLOW_STEP_MS * FLOW_STEP_MS
}

#[inline]
fn ms_between(from: Instant, to: Instant) -> u64 {
    let ms = to.saturating_duration_since(from).as_millis();
    if ms > u64::MAX as u128 {
        u64::MAX
    } else {
        ms as u64
    }
}

#[inline]
fn later(at: Instant, ms: u64) -> Instant {
    at.checked_add(Duration::from_millis(ms)).unwrap_or(at)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

impl Rect {
    #[inline]
    const fn meet(self, o: Self) -> Self {
        Self {
            x0: if self.x0 > o.x0 { self.x0 } else { o.x0 },
            y0: if self.y0 > o.y0 { self.y0 } else { o.y0 },
            x1: if self.x1 < o.x1 { self.x1 } else { o.x1 },
            y1: if self.y1 < o.y1 { self.y1 } else { o.y1 },
        }
    }

    #[inline]
    const fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    #[inline]
    const fn contains(self, x: i64, y: i64) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    #[inline]
    const fn x_span(self, covers: &[Self]) -> (usize, usize) {
        let mut lo = 0;
        while lo < covers.len() && covers[lo].x1 <= self.x0 {
            lo += 1;
        }
        let mut hi = lo;
        while hi < covers.len() && covers[hi].x0 < self.x1 {
            hi += 1;
        }
        (lo, hi)
    }

    #[cfg(test)]
    const fn shift(self, dx: i64, dy: i64) -> Self {
        Self {
            x0: self.x0 + dx,
            y0: self.y0 + dy,
            x1: self.x1 + dx,
            y1: self.y1 + dy,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Geo {
    cw: i64,
    ch: i64,
    ox: i64,
    oy: i64,
    top: i64,
    rows: i64,
    #[cfg(test)]
    row_base: i64,
    core: Rect,
    outer: Rect,
    t: i64,
    clip: Rect,
    wand: (f32, f32),
}

impl Geo {
    #[inline]
    const fn new(s: &CardScene) -> Option<Self> {
        const fn air(pad: u16, ch: i64) -> i64 {
            let p = pad as i64;
            let half = ch / 2;
            if p < half { p } else { half }
        }
        if s.cw == 0 || s.ch == 0 || s.rows == 0 || s.cols == 0 {
            return None;
        }
        let (cw, ch) = (s.cw as i64, s.ch as i64);
        let (ox, oy) = (s.origin_x as i64, s.origin_y as i64);
        let top = s.top as i64;
        let rows = s.rows as i64;
        let pad = air(s.pad_left, cw);
        let core = Rect {
            x0: ox - pad,
            y0: oy + top * ch,
            x1: ox + (s.cols as i64 + 1) * cw,
            y1: oy + (top + rows) * ch,
        };
        let outer = Rect {
            y0: core.y0 - if top > 0 { air(s.pad_above, ch) } else { 0 },
            y1: core.y1 + air(s.pad_below, ch),
            ..core
        };
        let space = Rect {
            x0: 0,
            y0: 0,
            x1: u16::MAX as i64,
            y1: u16::MAX as i64,
        };
        let (cx0, cy0, cx1, cy1) = s.clip;
        let pane = Rect {
            x0: cx0 as i64,
            y0: cy0 as i64,
            x1: cx1 as i64,
            y1: cy1 as i64,
        };
        let clip = outer.meet(space).meet(pane);
        if clip.is_empty() {
            return None;
        }
        let t = stroke_px(s.ch) as i64;
        let wand = match s.wand {
            Some((c, r)) => (
                (ox + c as i64 * cw) as f32 + 0.5 * cw as f32,
                (oy + r as i64 * ch) as f32 + 0.5 * ch as f32,
            ),
            None => ((core.x0 + t) as f32, (core.y0 + t) as f32),
        };
        Some(Self {
            cw,
            ch,
            ox,
            oy,
            top,
            rows,
            #[cfg(test)]
            row_base: s.row_base as i64,
            core,
            outer,
            t,
            clip,
            wand,
        })
    }

    #[inline]
    const fn band(&self, r: i64) -> i64 {
        self.oy + r * self.ch
    }

    #[inline]
    const fn row_of(&self, py: i64) -> i64 {
        (py - self.oy).div_euclid(self.ch)
    }

    #[inline]
    const fn col(&self, c: u16) -> i64 {
        self.ox + c as i64 * self.cw
    }

    #[inline]
    const fn inner(&self) -> Rect {
        Rect {
            x0: self.core.x0 + self.t,
            y0: self.core.y0 + self.t,
            x1: self.core.x1 - self.t,
            y1: self.core.y1 - self.t,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Paint {
    color: u32,
    alpha: u8,
}

trait Ramps {
    fn ramp(&mut self, clip: Rect, r: Rect, l: Paint, rt: Paint);

    fn side(&mut self, clip: Rect, r: Rect, p: Paint);
}

#[inline]
const fn clip_ramp(r: Rect, c: Rect, l: Paint, rt: Paint) -> (Paint, Paint) {
    if c.x0 == r.x0 && c.x1 == r.x1 {
        (l, rt)
    } else {
        let inv_span = 1.0 / (r.x1 - r.x0) as f32;
        (
            ramp(l, rt, (c.x0 - r.x0) as f32 * inv_span),
            ramp(l, rt, (c.x1 - r.x0) as f32 * inv_span),
        )
    }
}

#[inline]
const fn ramp(a: Paint, b: Paint, f: f32) -> Paint {
    const fn ch(x: u32, y: u32, f: f32) -> u32 {
        let v = (x as f32 + (y as f32 - x as f32) * f).round() as u32;
        if v < 255 { v } else { 255 }
    }
    let f = f.clamp(0.0, 1.0);
    let r = ch((a.color >> 16) & 0xff, (b.color >> 16) & 0xff, f);
    let g = ch((a.color >> 8) & 0xff, (b.color >> 8) & 0xff, f);
    let bl = ch(a.color & 0xff, b.color & 0xff, f);
    let alpha = if a.alpha == 0 {
        0
    } else {
        let al = ch(a.alpha as u32, b.alpha as u32, f);
        if al > 1 { al as u8 } else { 1 }
    };
    Paint {
        color: (r << 16) | (g << 8) | bl,
        alpha,
    }
}

#[inline]
const fn light(rgb: u32, cov: f32) -> Paint {
    Paint {
        color: premul_rgb(rgb, cov.clamp(0.0, 255.0) as u8),
        alpha: 0,
    }
}

#[inline]
const fn ink(rgb: u32, alpha: f32) -> Paint {
    let a = (alpha.clamp(1.0, 255.0)) as u8;
    Paint {
        color: premul_rgb(rgb, a),
        alpha: a,
    }
}

#[derive(Clone, Copy, Debug)]
struct KittyInk {
    outline: u32,
    coat: u32,
    belly: u32,
    pupil: u32,
    eye: u32,
    nose: u32,
    glint: u32,
}

impl KittyInk {
    #[inline]
    const fn new(coat: u32, eye: u32, dark: bool) -> Self {
        let coat = coat & 0x00FF_FFFF;
        let outline = if dark && relative_luminance(coat) < 0.06 {
            mix_rgb(coat, WHITE, 0.45)
        } else {
            mix_rgb(coat, 0, 0.65)
        };
        Self {
            outline,
            coat,
            belly: mix_rgb(coat, WHITE, 0.55),
            pupil: PUPIL,
            eye: eye & 0x00FF_FFFF,
            nose: NOSE_PINK,
            glint: WHITE,
        }
    }

    #[cfg(test)]
    #[inline]
    const fn of(self, c: u8) -> Option<u32> {
        match c {
            b'o' => Some(self.outline),
            b'c' => Some(self.coat),
            b'b' => Some(self.belly),
            b'k' => Some(self.pupil),
            b'e' => Some(self.eye),
            b'n' => Some(self.nose),
            b'w' => Some(self.glint),
            _ => None,
        }
    }
}

#[inline]
const fn kitty_frame(clock: Clock) -> KittyPose {
    const fn in_window(t: u64, at: u64, period: u64, len: u64) -> bool {
        t >= at && (t - at) % period < len
    }
    let Some(t) = clock.t_ms else {
        return match clock.flow {
            Some(f)
                if (f.step * FLOW_STEP_MS + GLINT_PERIOD_MS / 2) % FLOW_BLINK_PERIOD_MS
                    < FLOW_BLINK_MS =>
            {
                KittyPose::Blink
            }
            _ => KittyPose::Open,
        };
    };
    if in_window(t, BLINK_AT_MS, BLINK_PERIOD_MS, BLINK_MS) {
        KittyPose::Blink
    } else if in_window(t, FLICK_AT_MS, FLICK_PERIOD_MS, FLICK_MS) {
        KittyPose::Flick
    } else {
        KittyPose::Open
    }
}

#[inline]
const fn unit(seed: u64, salt: u64) -> f32 {
    const INV_2_24: f32 = 1.0 / (1u64 << 24) as f32;
    (mix(seed ^ mix(salt)) >> 40) as f32 * INV_2_24
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct StarSpot {
    x: i64,
    y: i64,
    arm: i64,
    ratio: f32,
    phase: f32,
    gold: bool,
}

#[inline]
const fn star_region(g: &Geo, s: &CardScene) -> Option<Rect> {
    if g.rows < 2 {
        return None;
    }
    let start = match (s.trail, s.kitty_col) {
        (Some((t0, _)), Some(k)) => {
            if t0 < k {
                t0
            } else {
                k
            }
        }
        (Some((t0, _)), None) => t0,
        (None, Some(k)) => k,
        (None, None) => return None,
    };
    let inner = g.inner();
    let cols_x = g.col(s.cols);
    let r = Rect {
        x0: g.col(start),
        y0: g.band(g.top),
        x1: if s.dark || inner.x1 < cols_x {
            inner.x1
        } else {
            cols_x
        },
        y1: g.band(g.top + 2),
    }
    .meet(inner)
    .meet(g.clip);
    if r.is_empty() { None } else { Some(r) }
}

#[derive(Clone, Copy, Debug)]
struct KittyAt {
    rect: Rect,
    p: i64,
    art: KittyArt,
}

impl KittyAt {
    #[inline]
    const fn body(&self) -> (i64, i64) {
        let (first, rows) = self.art.body;
        let y0 = self.rect.y0 + first as i64 * self.p;
        (y0, y0 + rows as i64 * self.p)
    }

    #[inline]
    const fn rear(&self) -> i64 {
        self.rect.x0 + self.art.rear() as i64 * self.p
    }
}

#[inline]
const fn rim_px(p: i64, t: i64) -> i64 {
    let half = p / 2;
    if t < half { t } else { half }
}

#[inline]
const fn art_at(frame: &[&str], r: i64, c: i64) -> bool {
    if r < 0 || c < 0 {
        return false;
    }
    let (r, c) = (r as usize, c as usize);
    if r >= frame.len() {
        return false;
    }
    let row = frame[r].as_bytes();
    c < row.len() && row[c] != b'.'
}

#[inline]
const fn rim_solid(frame: &[&str], r: i64, c: i64) -> bool {
    art_at(frame, r, c) || (art_at(frame, r, c - 1) && art_at(frame, r, c + 1))
}

#[inline]
fn kitty_art(g: &Geo, s: &CardScene) -> KittyArt {
    if s.kitty_rows >= 3 && g.rows >= 3 {
        kitty_tall()
    } else {
        KITTY_SHORT
    }
}

#[inline]
fn kitty_tall() -> KittyArt {
    #[cfg(test)]
    if let Some(art) = TALL_STAND_IN.with(std::cell::Cell::get) {
        return art;
    }
    KITTY_TALL
}

#[cfg(test)]
thread_local! {
    static TALL_STAND_IN: std::cell::Cell<Option<KittyArt>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn stand_in(art: Option<KittyArt>) {
    TALL_STAND_IN.with(|c| c.set(art));
}

#[cfg(test)]
fn stand_in_active() -> bool {
    TALL_STAND_IN.with(|c| c.get().is_some())
}

#[inline]
const fn kitty_place(g: &Geo, art: KittyArt, p: i64, x0: i64) -> KittyAt {
    let y0 = g.band(g.top + 1) - KITTY_HALF as i64 * p;
    KittyAt {
        rect: Rect {
            x0,
            y0,
            x1: x0 + KITTY_W as i64 * p,
            y1: y0 + art.h() as i64 * p,
        },
        p,
        art,
    }
}

#[inline]
fn kitty_rect(g: &Geo, s: &CardScene) -> Option<KittyAt> {
    let col = s.kitty_col?;
    let p = i64::from(kitty_px(s.ch));
    if g.rows < 2 || p == 0 {
        return None;
    }
    let at = kitty_place(g, kitty_art(g, s), p, g.col(col));
    if at.rect.meet(g.inner()).meet(g.clip) == at.rect {
        Some(at)
    } else {
        None
    }
}

#[inline]
const fn ride(clock: Clock) -> f32 {
    match clock.ride_ms {
        Some(t) if t < RIDE_MS => {
            let v = 1.0 - t as f32 / RIDE_MS as f32;
            1.0 - v * v * v
        }
        _ => 1.0,
    }
}

#[inline]
const fn ride_at(f: f32) -> f32 {
    RIDE_MS as f32 * (1.0 - crate::color_math::cbrt_f32(1.0 - f.clamp(0.0, 1.0)))
}

#[inline]
const fn ride_span(g: &Geo, s: &CardScene, seat: &KittyAt) -> Option<(i64, i64)> {
    let Some((c0, _)) = s.trail else {
        return None;
    };
    let mut from = g.col(c0);
    let inner_x0 = g.inner().x0;
    if inner_x0 > from {
        from = inner_x0;
    }
    if g.clip.x0 > from {
        from = g.clip.x0;
    }
    let dist = seat.rect.x0 - from;
    if dist > 0 { Some((from, dist)) } else { None }
}

#[inline]
const fn ridden(g: &Geo, s: &CardScene, seat: KittyAt, clock: Clock) -> KittyAt {
    let f = ride(clock);
    match ride_span(g, s, &seat) {
        Some((from, dist)) if f < 1.0 => {
            kitty_place(g, seat.art, seat.p, from + (dist as f32 * f).round() as i64)
        }
        _ => seat,
    }
}

#[inline]
fn trail_band(g: &Geo, s: &CardScene) -> (i64, i64) {
    let art = kitty_art(g, s);
    let p = i64::from(kitty_px(s.ch)).max(1);
    let (y0, y1) = kitty_place(g, art, p, 0).body();
    let (lo, hi) = (g.band(g.top), g.band(g.top + art.rows() as i64));
    (y0.clamp(lo, hi), y1.clamp(lo, hi))
}

#[inline]
fn trail_rect(g: &Geo, s: &CardScene, seat: Option<KittyAt>) -> Option<(Rect, i64)> {
    let (c0, c1) = s.trail?;
    if g.rows < 2 {
        return None;
    }
    let inner = g.inner();
    let (tx0, mut tx1) = (g.col(c0).max(inner.x0), g.col(c1).min(inner.x1));
    if let Some(seat) = seat
        && tx1 >= seat.rect.x0
    {
        tx1 = tx1.max(seat.rear());
    }
    if tx1 <= tx0 {
        return None;
    }
    let stripes = TRAIL_STOPS.len() as i64;
    let (ya, yb) = trail_band(g, s);
    let h = yb - ya;
    if h < stripes {
        return None;
    }
    let a = (g.ch / 16).max(1);
    let amp = if h / stripes > a { a } else { 0 };
    let r = Rect {
        x0: tx0,
        y0: ya,
        x1: tx1,
        y1: yb,
    };
    Some((r, amp))
}

#[inline]
const fn across_trail(b: Rect, (t, amp): (Rect, i64)) -> bool {
    let outer = Rect {
        y0: t.y0 - amp,
        y1: t.y1 + amp,
        ..t
    };
    let inside = b.x0 >= t.x0 && b.x1 <= t.x1 && b.y0 >= t.y0 + amp && b.y1 <= t.y1 - amp;
    !b.meet(outer).is_empty() && !inside
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct GlintSpot {
    x: i64,
    y: i64,
    phase: f32,
}

impl GlintSpot {
    const EMPTY: Self = Self {
        x: 0,
        y: 0,
        phase: 0.0,
    };
}

impl Default for GlintSpot {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

#[derive(Clone, Copy, Debug)]
struct Glints {
    spots: [GlintSpot; GLINTS_MAX],
    n: usize,
    rest: i64,
    flare: i64,
}

impl Default for Glints {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Glints {
    const EMPTY: Self = Self {
        spots: [GlintSpot::EMPTY; GLINTS_MAX],
        n: 0,
        rest: 0,
        flare: 0,
    };

    #[inline]
    const fn spots(&self) -> &[GlintSpot] {
        self.spots.split_at(self.n).0
    }
}

#[inline]
const fn glint_arms(g: &Geo, dark: bool, h: i64, amp: i64) -> (i64, i64) {
    let tax = if dark { 1.0 } else { STAR_ARM_INK };
    let raw_fits = (h - 2 * amp - 1) / 2 - 1;
    let fits = if raw_fits > 0 { raw_fits } else { 0 };
    let std_arm = star_arm_px(star_arm(g.ch as f32, STAR_ARM_STD * tax)) as i64;
    let hero_arm = star_arm_px(star_arm(g.ch as f32, STAR_ARM_HERO * tax)) as i64;
    (
        if std_arm < fits { std_arm } else { fits },
        if hero_arm < fits { hero_arm } else { fits },
    )
}

#[cfg(test)]
fn glint_spots(g: &Geo, s: &CardScene, seed: u64) -> Glints {
    glint_spots_at(g, s, seed, star_region(g, s), kitty_rect(g, s))
}

#[inline]
fn glint_spots_at(
    g: &Geo,
    s: &CardScene,
    seed: u64,
    region: Option<Rect>,
    seat: Option<KittyAt>,
) -> Glints {
    let mut out = Glints::default();
    let Some(region) = region else {
        return out;
    };
    let Some((t, amp)) = trail_rect(g, s, seat) else {
        return out;
    };
    let (rest, flare) = glint_arms(g, s.dark, t.y1 - t.y0, amp);
    if rest < 1 {
        return out;
    }
    let m = flare + 1;
    let free = match seat {
        Some(at) => at.rect.x0.min(t.x1),
        None => t.x1,
    };
    let room = Rect {
        x0: t.x0.max(region.x0) + m,
        y0: (t.y0 + amp).max(region.y0) + m,
        x1: free.min(region.x1) - m,
        y1: (t.y1 - amp).min(region.y1) - m,
    };
    if room.is_empty() {
        return out;
    }
    let cells = (free - t.x0) / g.cw.max(1);
    let raw_n = cells / GLINT_CELLS;
    let n = if raw_n <= 1 {
        1
    } else if raw_n >= GLINTS_MAX as i64 {
        GLINTS_MAX
    } else {
        raw_n as usize
    };
    let stripes = TRAIL_STOPS.len() as i64;
    let two_stripes = 2 * stripes;
    let first = (mix(seed ^ 0x474C_494E) & 3) as usize;
    let span_f = (room.x1 - room.x0) as f32;
    let n_f = n as f32;
    let th = t.y1 - t.y0;
    let mut i = 0;
    while i < n {
        let at = (i as f32 + 0.2 + 0.6 * unit(seed, 0x474C_0000 | i as u64)) / n_f;
        let x = room.x0 + (span_f * at) as i64;
        let k = 1 + [0i64, 2, 1, 3][(first + i) & 3];
        let centre = t.y0 + th * (2 * k + 1) / two_stripes;
        let mut y = centre.clamp(room.y0, room.y1 - 1);
        if g.row_of(y - m) != g.row_of(y + m) {
            let line = g.band(g.row_of(y + m));
            let (above, below) = (line - m - 1, line + m);
            let side = if centre < line { above } else { below };
            if (room.y0..room.y1).contains(&side) {
                y = side;
            }
        }
        out.spots[i] = GlintSpot {
            x,
            y,
            phase: unit(seed, 0x474C_5048 ^ i as u64) * std::f32::consts::PI,
        };
        i += 1;
    }
    Glints {
        n,
        rest,
        flare,
        ..out
    }
}

#[cfg(test)]
fn field_spots(
    g: &Geo,
    s: &CardScene,
    seed: u64,
    light_theme: bool,
) -> ([StarSpot; STARS_MAX], usize) {
    let Some(region) = star_region(g, s) else {
        return (
            [StarSpot {
                x: 0,
                y: 0,
                arm: 0,
                ratio: 0.0,
                phase: 0.0,
                gold: false,
            }; STARS_MAX],
            0,
        );
    };
    let seat = kitty_rect(g, s);
    let glints = glint_spots_at(g, s, seed, Some(region), seat);
    field_spots_at(g, s, seed, light_theme, region, seat, &glints)
}

#[inline]
fn field_spots_at(
    g: &Geo,
    s: &CardScene,
    seed: u64,
    light_theme: bool,
    region: Rect,
    seat: Option<KittyAt>,
    glints: &Glints,
) -> ([StarSpot; STARS_MAX], usize) {
    let mut spots = [StarSpot {
        x: 0,
        y: 0,
        arm: 0,
        ratio: 0.0,
        phase: 0.0,
        gold: false,
    }; STARS_MAX];
    let kitty = seat.map(|at| at.rect);
    let trail = if light_theme {
        trail_rect(g, s, seat)
    } else {
        None
    };
    let glint_slice = glints.spots();
    let want = (STARS_MIN + mix(seed ^ 0x5354_4152) % STARS_SPAN) as usize;
    let gap = g.cw.max(g.ch / 2);
    let tax = if light_theme { STAR_ARM_INK } else { 1.0 };
    let chf = g.ch as f32;
    let ratios = [
        STAR_RATIOS[0] * tax,
        STAR_RATIOS[1] * tax,
        STAR_RATIOS[2] * tax,
        STAR_RATIOS[3] * tax,
    ];
    let arm_fine = i64::from(star_arm_px(star_arm(chf, ratios[1])));
    let arms = [
        i64::from(star_arm_px(star_arm(chf, ratios[0]))),
        arm_fine,
        i64::from(star_arm_px(star_arm(chf, ratios[2]))),
        arm_fine,
    ];
    let mut n = 0;
    let mut i = 0;
    while i < want {
        let cur_i = i;
        i += 1;
        let idx = cur_i & (STAR_RATIOS.len() - 1);
        let ratio = ratios[idx];
        let arm = arms[idx];
        let m = arm + 1;
        let room = Rect {
            x0: region.x0 + m,
            y0: region.y0 + m,
            x1: region.x1 - m,
            y1: region.y1 - m,
        };
        if room.is_empty() {
            continue;
        }
        let span = room.x1 - room.x0;
        let slice = Rect {
            x0: room.x0 + span * cur_i as i64 / want as i64,
            x1: room.x0 + span * (cur_i as i64 + 1) / want as i64,
            ..room
        };
        let kitty_pad = kitty.map(|r| Rect {
            x0: r.x0 - m,
            y0: r.y0 - m,
            x1: r.x1 + m,
            y1: r.y1 + m,
        });
        let room_w = (room.x1 - room.x0) as f32;
        let slice_w = (slice.x1 - slice.x0) as f32;
        let room_h = (room.y1 - room.y0) as f32;
        let i_salt = (cur_i as u64) << 8;
        let mut k = 0;
        while k < STAR_TRIES {
            let salt = i_salt | k;
            let (within_x0, within_w) = if k < STAR_TRIES / 2 {
                (slice.x0, slice_w)
            } else {
                (room.x0, room_w)
            };
            k += 1;
            let x = within_x0 + (unit(seed, salt * 2) * within_w) as i64;
            let y = room.y0 + (unit(seed, salt * 2 + 1) * room_h) as i64;
            if let Some(r) = kitty_pad
                && r.contains(x, y)
            {
                continue;
            }
            let mut crowded = false;
            let mut si = 0;
            while si < n {
                let o = &spots[si];
                if (o.x - x).abs() < gap && (o.y - y).abs() < gap {
                    crowded = true;
                    break;
                }
                si += 1;
            }
            if !crowded {
                let mut gi = 0;
                while gi < glint_slice.len() {
                    let o = &glint_slice[gi];
                    if (o.x - x).abs() < gap && (o.y - y).abs() < gap {
                        crowded = true;
                        break;
                    }
                    gi += 1;
                }
            }
            if crowded {
                continue;
            }
            let reach = Rect {
                x0: x - m,
                y0: y - m,
                x1: x + m + 1,
                y1: y + m + 1,
            };
            let across = match trail {
                Some(t) => across_trail(reach, t),
                None => false,
            };
            if !across {
                spots[n] = StarSpot {
                    x,
                    y,
                    arm,
                    ratio,
                    phase: unit(seed, 0x5048_0000 | i as u64) * std::f32::consts::PI,
                    gold: i % 3 == 0,
                };
                n += 1;
                break;
            }
        }
    }
    (spots, n)
}

/// Grid placement and hero-word span for the status line beneath the card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineAt {
    /// Grid row of the line.
    pub row: u16,
    /// Starting grid column of the line.
    pub col: u16,
    /// Length of the line in cells.
    pub len: u16,
    /// Cell offset range (`0..len`) of the emphasized hero word within the line (`0..0` if none).
    pub hero: Range<u16>,
    /// Optional `(col, row)` ripple origin in the line's grid ([`CardScene::wand`]).
    pub wand: Option<(u16, u16)>,
}

impl LineAt {
    /// Empty status-line placement before any row or span is bound.
    pub const EMPTY: Self = Self {
        row: 0,
        col: 0,
        len: 0,
        hero: 0..0,
        wand: None,
    };
}

impl Default for LineAt {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl HarnessCard {
    /// Construct an idle harness card state machine.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            episode: Episode::Idle,
            seed: 0,
            palette: CardPalette::Cruise,
            inks: LineInks::EMPTY,
            still: false,
            drew: Drew::Settled,
            ride: Ride::Ahead,
            flows: false,
            glint0: None,
            hue0: None,
            fade0: None,
            drawn: None,
            grid_gen: 0,
        }
    }
    /// Arms an intro episode at `now` with star `seed`, preserving any active ripple clock.
    #[inline]
    pub fn arm(&mut self, now: Instant, seed: u64) {
        self.episode = Episode::Armed(now);
        self.seed = seed;
        self.still = false;
        self.flows = true;
        if self.glint0.is_none() {
            self.glint0 = Some(now);
        }
        self.drawn = None;
        if let Ride::Since(from) = self.ride
            && now.saturating_duration_since(from) >= Duration::from_millis(RIDE_MS)
        {
            self.ride = Ride::Done;
        }
    }

    /// Returns `true` when the card has no pending wake deadline at any instant.
    #[must_use]
    #[inline]
    pub const fn at_rest(&self) -> bool {
        self.still || (!self.intro_live() && !self.flows)
    }

    /// Returns `true` when the card is held at its static settled frame.
    #[must_use]
    #[inline]
    pub const fn is_still(&self) -> bool {
        !self.intro_live() && !(self.flows && !self.still)
    }

    /// Returns `true` while an intro episode is armed or actively playing.
    #[must_use]
    #[inline]
    pub const fn intro_live(&self) -> bool {
        matches!(self.episode, Episode::Armed(_) | Episode::Playing(_))
    }

    /// Returns `true` when the card is in continuous post-intro flow at `now`.
    #[must_use]
    #[inline]
    pub fn flowing(&self, now: Instant) -> bool {
        self.flows
            && !self.still
            && self.glint0.is_some()
            && !matches!(self.episode, Episode::Owed(_))
            && self.running(now).is_none()
    }

    /// Returns the [`FLOW_STEP_MS`] step index containing `now`, or `None` before the first arm.
    #[must_use]
    #[inline]
    pub fn flow_step(&self, now: Instant) -> Option<u64> {
        self.glint0.map(|g0| ms_between(g0, now) / FLOW_STEP_MS)
    }

    /// Returns the origin instant of the ripple clock, or `None` before the first arm or lock.
    #[must_use]
    #[inline]
    pub const fn ripple_origin(&self) -> Option<Instant> {
        self.glint0
    }

    /// Adopts `origin` as the ripple clock start if this card's clock has not yet started.
    #[inline]
    pub const fn adopt_ripple(&mut self, origin: Instant) {
        if self.glint0.is_none() {
            self.glint0 = Some(origin);
            self.grid_gen = self.grid_gen.wrapping_add(1);
        }
    }

    /// Phase-locks the ripple cycle to an observed wand flare at `seen`.
    #[inline]
    pub fn lock_glint(&mut self, seen: Instant) {
        if self.glint0.is_none() {
            let at = seen
                .checked_sub(Duration::from_millis(FLARE_STEP_LEAD_MS))
                .unwrap_or(seen);
            self.glint0 = Some(at);
            self.grid_gen = self.grid_gen.wrapping_add(1);
            return;
        }
        self.slew(seen, GLINT_PERIOD_MS);
    }

    /// Phase-locks the flow step boundaries to an observed mascot shimmer repaint at `seen`.
    #[inline]
    pub fn lock_step(&mut self, seen: Instant) {
        if self.glint0.is_some() {
            self.slew(seen, FLOW_STEP_MS);
        }
    }

    #[inline]
    fn slew(&mut self, seen: Instant, period: u64) {
        let Some(g0) = self.glint0 else {
            return;
        };
        let at = seen
            .checked_sub(Duration::from_millis(FLARE_STEP_LEAD_MS))
            .unwrap_or(seen);
        let period = period.min(i64::MAX as u64) as i64;
        let ahead = ms_between(g0, at).min(i64::MAX as u64) as i64;
        let behind = ms_between(at, g0).min(i64::MAX as u64) as i64;
        let mut e = (ahead - behind).rem_euclid(period);
        if e > period / 2 {
            e -= period;
        }
        let shift = if self.glints_shown(seen) {
            (e / LOCK_GAIN).clamp(-LOCK_SLEW_MS, LOCK_SLEW_MS)
        } else {
            e
        };
        if shift == 0 {
            return;
        }
        let by = Duration::from_millis(shift.unsigned_abs());
        self.glint0 = Some(if shift > 0 {
            g0.checked_add(by).unwrap_or(g0)
        } else {
            g0.checked_sub(by).unwrap_or(g0)
        });
        self.grid_gen = self.grid_gen.wrapping_add(1);
    }

    #[inline]
    fn glints_shown(&self, now: Instant) -> bool {
        !self.still
            && match self.running(now) {
                Some(ms) => Clock::intro(ms, None).intro_rise() > 0.0,
                None => self.flowing(now),
            }
    }

    #[inline]
    const fn holds(&self, serious: bool, drawable: bool, animate: bool) -> bool {
        let may = animate && drawable;
        match self.episode {
            Episode::Idle => !(self.flows && may && !serious),
            Episode::Owed(_) | Episode::Playing(_) => !serious && !may,
            Episode::Armed(_) => false,
        }
    }

    /// Replays the cached settled [`CardFrame`] from `frames` if the card holds still and matches
    /// `scene`'s shape.
    #[inline]
    pub fn replay(
        &mut self,
        scene: &CardScene,
        animate: bool,
        frames: &CardFrames,
    ) -> Option<CardFrame> {
        let kept = frames.kept()?;
        if kept.step.is_some() {
            return None;
        }
        let (shape, origin) = CardShape::of(scene);
        if kept.shape != shape {
            return None;
        }
        let drawable = shape.drawable();
        if !self.holds(scene.serious, drawable, animate) {
            return None;
        }
        self.palette = scene.palette;
        if !scene.dark {
            self.inks.solve(scene.palette, scene.ground);
        }
        self.still = !(animate && drawable) || scene.serious;
        self.drew = Drew::Settled;
        Some(kept.at(origin))
    }

    /// Replays the cached flow [`CardFrame`] from `frames` when `now` falls in the same step.
    #[inline]
    pub fn replay_step(
        &mut self,
        scene: &CardScene,
        animate: bool,
        frames: &CardFrames,
        now: Instant,
    ) -> Option<CardFrame> {
        if !animate
            || scene.serious
            || !self.flows
            || self.drew != Drew::Flow
            || self.episode != Episode::Idle
        {
            return None;
        }
        let kept = frames.kept()?;
        let step = self.flow_step(now)?;
        if kept.step != Some((step, self.grid_gen)) {
            return None;
        }
        let (shape, origin) = CardShape::of(scene);
        if kept.shape != shape {
            return None;
        }
        self.palette = scene.palette;
        if !scene.dark {
            self.inks.solve(scene.palette, scene.ground);
        }
        self.still = false;
        self.drawn = Some((now, Some((step, self.grid_gen))));
        Some(kept.at(origin))
    }

    #[inline]
    fn running(&self, now: Instant) -> Option<u64> {
        let (Episode::Armed(start) | Episode::Playing(start)) = self.episode else {
            return None;
        };
        let ms = ms_between(start, now);
        if ms < EPISODE_MS { Some(ms) } else { None }
    }

    /// Returns the next animation wake instant for the current frame, or `None` if at rest.
    #[inline]
    #[must_use]
    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        if self.at_rest() {
            return None;
        }
        if self.running(now).is_some() {
            return now.checked_add(Duration::from_millis(FRAME_MS));
        }
        if matches!(self.episode, Episode::Owed(_)) {
            return None;
        }
        let (at, step) = self.drawn?;
        if now >= later(at, SETTLE_GRACE_MS) {
            return None;
        }
        match (self.drew, step, self.glint0) {
            (Drew::Flow, Some((k, grid)), Some(g0)) if grid == self.grid_gen => g0.checked_add(
                Duration::from_millis((k + 1) * FLOW_STEP_MS + FLOW_SLACK_MS),
            ),
            (Drew::Intro | Drew::Flow, _, _) if self.flows => Some(now),
            _ => None,
        }
    }

    /// Draws the current frame (`under` and `over` layers) into `frames` for `scene` and `animate`.
    #[inline]
    pub fn draw(
        &mut self,
        now: Instant,
        scene: &CardScene,
        animate: bool,
        frames: &mut CardFrames,
    ) -> CardFrame {
        let (shape, origin) = CardShape::of(scene);
        let local = shape.scene();
        let geo = if CardArts::dimensions(local.ch).is_some() {
            Geo::new(local)
        } else {
            None
        };
        let clock = self.step(now, scene, animate, geo.is_some());
        let (under, over) = match (geo, clock) {
            (Some(geo), Some(clock)) => frames.fill(&Emit::new(local, geo, clock, self.seed)),
            _ => CardFrames::nothing(),
        };
        let step = match clock {
            Some(c) => match c.flow {
                Some(f) => Some((f.step, self.grid_gen)),
                None => None,
            },
            None => None,
        };
        let kept = if self.drew != Drew::Intro {
            Some(KeptFrame {
                shape,
                step,
                under: Arc::clone(&under),
                over: Arc::clone(&over),
            })
        } else {
            None
        };
        frames.keep(kept);
        CardFrame {
            under,
            over,
            origin,
        }
    }

    #[inline]
    fn step(
        &mut self,
        now: Instant,
        scene: &CardScene,
        animate: bool,
        drawable: bool,
    ) -> Option<Clock> {
        self.palette = scene.palette;
        if !scene.dark {
            self.inks.solve(scene.palette, scene.ground);
        }
        let animate = animate && drawable;
        self.still = !animate || scene.serious;
        let clock = self.advance(now, animate, scene.serious);
        self.drew = match (clock.t_ms, clock.flow) {
            (Some(_), _) => Drew::Intro,
            (None, Some(_)) => Drew::Flow,
            (None, None) => Drew::Settled,
        };
        self.drawn = Some((now, clock.flow.map(|f| (f.step, self.grid_gen))));
        if !drawable {
            return None;
        }
        if clock.t_ms.is_none() {
            self.ride = Ride::Done;
        }
        Some(clock)
    }

    #[inline]
    fn advance(&mut self, now: Instant, animate: bool, serious: bool) -> Clock {
        if serious {
            self.episode = Episode::Idle;
            return Clock::SETTLED;
        }
        if let Episode::Owed(armed) = self.episode
            && now.saturating_duration_since(armed) >= Duration::from_millis(OWED_MAX_MS)
        {
            self.episode = Episode::Idle;
        }
        if !animate {
            if let Episode::Armed(armed) = self.episode {
                self.episode = Episode::Owed(armed);
            }
            return Clock::SETTLED;
        }
        if let Episode::Owed(_) = self.episode {
            self.episode = Episode::Armed(now);
        }
        match (self.episode, self.running(now)) {
            (Episode::Armed(start) | Episode::Playing(start), Some(ms)) => {
                self.episode = Episode::Playing(start);
                let ride_ms = match self.ride {
                    Ride::Ahead => {
                        self.ride = Ride::Since(start);
                        Some(ms)
                    }
                    Ride::Since(from) => Some(ms_between(from, now)),
                    Ride::Done => None,
                };
                self.on_ripple(Clock::intro(ms, ride_ms), now)
            }
            (episode, _) => {
                self.episode = Episode::Idle;
                if !self.flows {
                    return Clock::SETTLED;
                }
                let intro = match episode {
                    Episode::Armed(start) | Episode::Playing(start) => Some(start),
                    _ => None,
                };
                match (self.drew, intro) {
                    (Drew::Intro, Some(start)) => self.hue0 = Some(start),
                    (Drew::Flow, _) if self.hue0.is_some() => {}
                    _ => {
                        self.hue0 = Some(now);
                        self.fade0 = Some(now);
                    }
                }
                if self.glint0.is_none() {
                    self.glint0 = Some(now);
                }
                self.flow_at(now)
            }
        }
    }

    #[inline]
    fn on_ripple(&self, mut clock: Clock, now: Instant) -> Clock {
        if let Some(g0) = self.glint0 {
            let ms = on_grid(ms_between(g0, now));
            clock.glint = Some((ms, clock.intro_rise() * self.fade_at(later(g0, ms))));
        }
        clock
    }

    #[inline]
    fn fade_at(&self, at: Instant) -> f32 {
        const INV_GLINT_FADE_MS: f32 = 1.0 / GLINT_FADE_MS as f32;
        match self.fade0 {
            Some(f) => smooth(ms_between(f, at) as f32 * INV_GLINT_FADE_MS),
            None => 1.0,
        }
    }

    #[inline]
    fn flow_at(&self, now: Instant) -> Clock {
        let Some(g0) = self.glint0 else {
            return Clock::SETTLED;
        };
        let step = ms_between(g0, now) / FLOW_STEP_MS;
        let ms = step * FLOW_STEP_MS;
        let step_at = later(g0, ms);
        Clock {
            t_ms: None,
            ride_ms: None,
            glint: Some((ms, self.fade_at(step_at))),
            flow: Some(Flow {
                step,
                hue_ms: match self.hue0 {
                    Some(h) => ms_between(h, step_at),
                    None => ms,
                },
            }),
        }
    }

    /// Computes per-cell [`InkCell`] colors for the status line under the card without a hero word.
    #[inline]
    #[must_use]
    pub fn line_ink(
        &self,
        now: Instant,
        row: u16,
        col: u16,
        len: u16,
        ground: u32,
        animate: bool,
    ) -> Vec<InkCell> {
        let mut out = Vec::with_capacity(usize::from(len));
        let at = LineAt {
            row,
            col,
            len,
            ..LineAt::default()
        };
        self.line_ink_into(Some(now), &at, ground, animate, &mut out);
        out
    }

    /// Appends per-cell [`InkCell`] colors for the status line at `at` into `out`.
    #[inline]
    pub fn line_ink_into(
        &self,
        now: Option<Instant>,
        at: &LineAt,
        ground: u32,
        animate: bool,
        out: &mut Vec<InkCell>,
    ) {
        debug_assert!(
            now.is_some() || self.is_still() || !animate,
            "a card that moves needs the clock for its line"
        );
        if at.len == 0 {
            return;
        }
        let ground = ground & 0x00FF_FFFF;
        let table = crate::color_math::relative_luminance_table();
        let lg = crate::color_math::relative_luminance_with(table, ground);
        let dark = lg < 0.5;
        let clock = match now {
            Some(now) if animate => match self.running(now) {
                Some(ms) => self.on_ripple(Clock::intro(ms, None), now),
                None if self.flowing(now) => self.flow_at(now),
                None => Clock::SETTLED,
            },
            _ => Clock::SETTLED,
        };
        let phi = clock.line_drift(LINE_DRIFT);
        let env = clock.head_env();
        let pos = if env == 0.0 {
            0.0
        } else {
            (clock.u() * LINE_SWEEPS).fract() * 1.4 - 0.2
        };
        let inv_span = 1.0 / f32::from(at.len.saturating_sub(1).max(1));
        let hero = at.hero.clone();
        let inv_hero_span = if hero.is_empty() {
            0.0
        } else {
            1.0 / f32::from(hero.end.saturating_sub(hero.start).saturating_sub(1).max(1))
        };
        let (wc, wr) = at.wand.unwrap_or((at.col, at.row));
        let wc_f = f32::from(wc);
        let dy_rows = f32::from(at.row) - f32::from(wr);
        let active_glint = match clock.glint {
            Some((ms, up)) if up > 0.0 => Some((ms, up)),
            _ => None,
        };
        #[inline]
        const fn line_glint_at(
            active_glint: Option<(u64, f32)>,
            wc_f: f32,
            dy_rows: f32,
            c: u16,
        ) -> f32 {
            match active_glint {
                Some((ms, up)) => {
                    let d = cell_distance(c as f32 - wc_f, dy_rows);
                    up * glint(d, ms)
                }
                None => 0.0,
            }
        }
        const INV_LINE_SWEEP_SIGMA: f32 = 1.0 / LINE_SWEEP_SIGMA;
        let (palette, inks) = (self.palette, &self.inks);
        let mut i = 0;
        while i < at.len {
            let Some(c) = at.col.checked_add(i) else {
                break;
            };
            let cur_i = i;
            i += 1;
            let x = f32::from(cur_i) * inv_span;
            let sweep = if env == 0.0 {
                0.0
            } else {
                let d = (x - pos) * INV_LINE_SWEEP_SIGMA;
                env * crate::color_math::exp_f32(-d * d)
            };
            let crest = sweep.max(line_glint_at(active_glint, wc_f, dy_rows, c));
            let rgb = if hero.contains(&cur_i) {
                let k = f32::from(cur_i - hero.start) * inv_hero_span;
                let place = tri(k * LINE_HERO_SPREAD + phi);
                if dark {
                    line_floor(
                        table,
                        mix_rgb(
                            lift_to_luminance_with(
                                table,
                                palette.at(place),
                                LINE_HERO_DARK_LUMA_FLOOR,
                            ),
                            WHITE,
                            crest * LINE_HERO_SWEEP_GAIN,
                        ),
                        lg,
                    )
                } else {
                    mix_rgb(
                        inks.at_with_ground_lum(table, palette, ground, lg, place),
                        0,
                        crest * LINE_SWEEP_GAIN_LIGHT,
                    )
                }
            } else {
                let place = tri(x + phi);
                if dark {
                    line_floor(
                        table,
                        mix_rgb(
                            palette.letter_dark_with(table, place),
                            WHITE,
                            crest * LINE_SWEEP_GAIN,
                        ),
                        lg,
                    )
                } else {
                    mix_rgb(
                        inks.at_with_ground_lum(table, palette, ground, lg, place),
                        0,
                        crest * LINE_SWEEP_GAIN_LIGHT,
                    )
                }
            };
            out.push(InkCell {
                row: at.row,
                col: c,
                color: [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8],
            });
        }
    }
}

/// A dark-ground line letter `rgb` floored along its hue to [`LINE_CONTRAST`] on `ground`: kept
/// where it already reads, lifted toward white on a dim ground, and darkened on a mid ground on
/// which black reads better than white, so a sweep's white never washes it out.
#[inline]
const fn line_floor(table: &crate::color_math::RelativeLuminanceTable, rgb: u32, lg: f32) -> u32 {
    floor_contrast_keeping_hue_with_ground_lum(table, rgb, lg, LINE_CONTRAST)
}

/// Mixes `rgb` toward white to reach WCAG relative luminance `floor` on a `2^-14` step grid.
#[inline]
pub(crate) const fn lift_to_luminance(rgb: u32, floor: f32) -> u32 {
    lift_to_luminance_with(crate::color_math::relative_luminance_table(), rgb, floor)
}

#[inline]
const fn lift_to_luminance_with(
    table: &crate::color_math::RelativeLuminanceTable,
    rgb: u32,
    floor: f32,
) -> u32 {
    if crate::color_math::relative_luminance_with(table, rgb) >= floor {
        return rgb;
    }
    #[inline]
    const fn reaches(
        table: &crate::color_math::RelativeLuminanceTable,
        rgb: u32,
        floor: f32,
        t: f32,
    ) -> bool {
        crate::color_math::relative_luminance_with(table, mix_rgb(rgb, WHITE, t)) >= floor
    }
    let (mut short, mut step) = (0.0f32, 0.25f32);
    let mut i = 0;
    while i < 7 {
        let (a, b, c) = (
            reaches(table, rgb, floor, short + step),
            reaches(table, rgb, floor, short + 2.0 * step),
            reaches(table, rgb, floor, short + 3.0 * step),
        );
        short += step * ((!a as u8) + (!b as u8) + (!c as u8)) as f32;
        step *= 0.25;
        i += 1;
    }
    mix_rgb(rgb, WHITE, short + 4.0 * step)
}

/// Returns `true` when background `bg` (`0x00RRGGBB`) has WCAG relative luminance below `0.5`.
#[must_use]
#[inline]
pub const fn dark_ground(bg: u32) -> bool {
    relative_luminance(bg) < 0.5
}

/// Minimum WCAG contrast of a dark scene's mark on the ground it is laid on — a field star, a
/// glint, a ride sparkle, a burst star, the line's leading `✦`: the non-text floor
/// ([`ICON_CONTRAST`]), a mark being seen, not read as state.
pub const MARK_CONTRAST: f32 = ICON_CONTRAST;

/// How a dark scene lays a mark on its ground ([`dark_mark`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DarkMark {
    /// Light (`0x00RRGGBB`) added to the ground, which only brightens it.
    Light(u32),
    /// Ink (`0x00RRGGBB`) laid over the ground, source-over, as a light scene prints its marks.
    Print(u32),
}

/// How a dark scene lays a mark of light `rgb` on `ground` (both `0x00RRGGBB`): the light itself,
/// added, where at a settled star's strength ([`STAR_PRINT_FULL`]) it reads [`MARK_CONTRAST`] on
/// the ground; else, where white reads better on the ground than black, the light lifted along
/// its hue to the least lightness that reads, added; and otherwise (black reads better, or even
/// added white misses the floor) the light floored along its hue to [`MARK_CONTRAST`] and
/// printed.
#[inline]
#[must_use]
pub const fn dark_mark(rgb: u32, ground: u32) -> DarkMark {
    let ground = ground & 0x00FF_FFFF;
    let table = crate::color_math::relative_luminance_table();
    let lg = crate::color_math::relative_luminance_with(table, ground);
    dark_mark_with(table, rgb, ground, lg)
}

#[inline]
const fn dark_mark_with(
    table: &crate::color_math::RelativeLuminanceTable,
    rgb: u32,
    ground: u32,
    lg: f32,
) -> DarkMark {
    #[inline]
    const fn reads(
        table: &crate::color_math::RelativeLuminanceTable,
        ground: u32,
        lg: f32,
        c: u32,
    ) -> bool {
        let lit = add_sat(ground, premul_rgb(c, STAR_PRINT_FULL as u8));
        let ll = crate::color_math::relative_luminance_with(table, lit);
        (ll.max(lg) + 0.05) / (ll.min(lg) + 0.05) >= MARK_CONTRAST
    }
    let rgb = rgb & 0x00FF_FFFF;
    if reads(table, ground, lg, rgb) {
        return DarkMark::Light(rgb);
    }
    if ((1.0f32.max(lg) + 0.05) / (1.0f32.min(lg) + 0.05))
        > ((0.0f32.max(lg) + 0.05) / (0.0f32.min(lg) + 0.05))
        && reads(table, ground, lg, WHITE)
    {
        let (r, g, b, up, r_up, g_up, b_up) = hue_path_endpoints(rgb);
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        let (mut lo_rgb, mut hi_rgb) = (rgb, WHITE);
        let mut i = 0;
        while i < 24 {
            let mid = 0.5 * (lo + hi);
            let mid_rgb = hue_path_at(r, g, b, up, r_up, g_up, b_up, false, mid);
            if mid_rgb == lo_rgb {
                lo = mid;
            } else if mid_rgb == hi_rgb || reads(table, ground, lg, mid_rgb) {
                hi = mid;
                hi_rgb = mid_rgb;
            } else {
                lo = mid;
                lo_rgb = mid_rgb;
            }
            if i >= 1 && lo_rgb == hi_rgb {
                break;
            }
            i += 1;
        }
        if reads(table, ground, lg, hi_rgb) {
            return DarkMark::Light(hi_rgb);
        }
    }
    DarkMark::Print(floor_contrast_keeping_hue_with_ground_lum(
        table,
        rgb,
        lg,
        MARK_CONTRAST,
    ))
}

/// The tint and blend of the `✦` leading a card line whose first letter wears `rgb` on `ground`
/// (both `0x00RRGGBB`): on a dark ground the card's mark ([`dark_mark`]), the letter's light
/// added where it reads or once lifted along its hue, else its floored ink laid over the ground;
/// on paper the letter's ink laid over.
#[inline]
#[must_use]
pub const fn line_star(rgb: u32, ground: u32) -> (u32, DecoBlend) {
    let ground = ground & 0x00FF_FFFF;
    let table = crate::color_math::relative_luminance_table();
    let lg = crate::color_math::relative_luminance_with(table, ground);
    if lg >= 0.5 {
        return (rgb, DecoBlend::Over);
    }
    match dark_mark_with(table, rgb, ground, lg) {
        DarkMark::Light(rgb) => (rgb, DecoBlend::Add),
        DarkMark::Print(rgb) => (rgb, DecoBlend::Over),
    }
}

#[derive(Clone, Copy, Debug)]
struct TrailLay {
    tx0: i64,
    drawn: i64,
    len: i64,
    seg: i64,
    ya: i64,
    yb: i64,
    amp: i64,
    held: i64,
}

#[inline]
fn compute_trail_lay(
    g: &Geo,
    scene: &CardScene,
    seat: Option<KittyAt>,
    kitty: Option<(KittyAt, KittyPose)>,
    clock: Clock,
) -> Option<TrailLay> {
    let (
        Rect {
            x0: tx0,
            y0: ya,
            x1: tx1,
            y1: yb,
        },
        amp,
    ) = trail_rect(g, scene, seat)?;
    let len = tx1 - tx0;
    let drawn = match kitty {
        Some((at, _)) if ride(clock) < 1.0 => {
            let rear = at.rear();
            if tx1 < rear { tx1 } else { rear }
        }
        _ => tx1,
    };
    let held = match kitty {
        Some((at, _)) => {
            let h = at.rect.x0 - rim_px(at.p, g.t);
            if drawn > h { h } else { i64::MAX }
        }
        None => i64::MAX,
    };
    let seg = if clock.t_ms.is_some() || clock.flow.is_some() {
        let min_seg = TRAIL_SEG_CELLS * g.cw;
        let cap_seg = (len + MAX_TRAIL_SEGMENTS - 1) / MAX_TRAIL_SEGMENTS;
        if min_seg > cap_seg { min_seg } else { cap_seg }
    } else {
        len
    };
    Some(TrailLay {
        tx0,
        drawn,
        len,
        seg,
        ya,
        yb,
        amp,
        held,
    })
}

#[inline]
fn lay_cut(
    q: Rect,
    k: i64,
    last: i64,
    cover: &impl Fn(i64) -> Rect,
    lay: &mut impl FnMut(Rect) -> bool,
) -> bool {
    if q.is_empty() {
        return true;
    }
    if k > last {
        return lay(q);
    }
    let c = cover(k).meet(q);
    if c.is_empty() {
        return lay_cut(q, k + 1, last, cover, lay);
    }
    let pieces = [
        Rect { y1: c.y0, ..q },
        Rect { y0: c.y1, ..q },
        Rect {
            x1: c.x0,
            y0: c.y0,
            y1: c.y1,
            ..q
        },
        Rect {
            x0: c.x1,
            y0: c.y0,
            y1: c.y1,
            ..q
        },
    ];
    let mut pi = 0;
    while pi < pieces.len() {
        if !lay_cut(pieces[pi], k + 1, last, cover, lay) {
            return false;
        }
        pi += 1;
    }
    true
}

struct Emit<'a> {
    scene: &'a CardScene,
    geo: Geo,
    clock: Clock,
    seed: u64,
    phi: f32,
    head_env: f32,
    head_pos: f32,
    shimmer_pos: f32,
    kitty: Option<(KittyAt, KittyPose)>,
    seat: Option<KittyAt>,
    star_region: Option<Rect>,
    trail_lay: Option<TrailLay>,
    glints: std::cell::OnceCell<Glints>,
}

impl<'a> Emit<'a> {
    #[inline]
    fn new(scene: &'a CardScene, geo: Geo, clock: Clock, seed: u64) -> Self {
        const INV_SHIMMER_MS: f32 = 1.0 / SHIMMER_MS;
        let seat = kitty_rect(&geo, scene);
        let star_region = star_region(&geo, scene);
        let kitty = seat.map(|at| (ridden(&geo, scene, at, clock), kitty_frame(clock)));
        let trail_lay = compute_trail_lay(&geo, scene, seat, kitty, clock);
        Self {
            scene,
            geo,
            clock,
            seed,
            phi: clock.phi(),
            head_env: clock.head_env(),
            head_pos: (clock.u() * HEAD_LAPS).fract(),
            shimmer_pos: match clock.t_ms {
                Some(t) => (t as f32 * INV_SHIMMER_MS).fract() * 1.4 - 0.2,
                None => 0.0,
            },
            kitty,
            seat,
            star_region,
            trail_lay,
            glints: std::cell::OnceCell::new(),
        }
    }

    #[inline]
    fn glint_spots(&self) -> &Glints {
        self.glints.get_or_init(|| {
            glint_spots_at(
                &self.geo,
                self.scene,
                self.seed,
                self.star_region,
                self.seat,
            )
        })
    }

    #[inline]
    const fn rides(&self) -> bool {
        match self.seat {
            Some(seat) => ride_span(&self.geo, self.scene, &seat).is_some(),
            None => false,
        }
    }

    #[inline]
    const fn ring_at(&self, x: f32, y: f32) -> f32 {
        let Some((ms, up)) = self.clock.glint else {
            return 0.0;
        };
        if up <= 0.0 {
            return 0.0;
        }
        let g = self.geo;
        let d = cell_distance((x - g.wand.0) / g.cw as f32, (y - g.wand.1) / g.ch as f32);
        up * glint(d, ms)
    }

    #[inline]
    const fn border_paint(&self, s: f32, (x, y): (f32, f32)) -> Paint {
        const INV_HEAD_SIGMA: f32 = 1.0 / HEAD_SIGMA;
        let base = self
            .scene
            .palette
            .art_at(tri(2.0 * s + self.phi), self.scene.dark);
        let env = self.head_env;
        let w = if env > 0.0 {
            let u = s - self.head_pos;
            let d = u - u.floor();
            let d = d.min(1.0 - d) * INV_HEAD_SIGMA;
            env * crate::color_math::exp_f32(-d * d)
        } else {
            0.0
        };
        let ring = self.ring_at(x, y);
        if self.scene.dark {
            let w = (w * HEAD_GAIN).max(GLINT_CREST_DARK * ring);
            light(mix_rgb(base, WHITE, w), BORDER_COV as f32)
        } else {
            let w = w.max(GLINT_CREST_LIGHT * ring);
            ink(
                mix_rgb(print_line(base), 0, w * LIGHT_HEAD_DEEPEN),
                PRINT_ALPHA,
            )
        }
    }

    #[inline]
    fn border(&self, out: &mut impl Ramps) {
        let g = self.geo;
        let o = g.outer;
        let (w, h, t) = (o.x1 - o.x0, o.y1 - o.y0, g.t);
        let inv_per = 1.0 / (2 * (w + h)) as f32;
        let all = g.clip;
        let n = ((w + g.cw - 1) / g.cw).clamp(1, MAX_EDGE_SEGMENTS);
        #[inline]
        const fn border_arc_x(o: Rect, w: i64, h: i64, bottom: bool, x: i64) -> f32 {
            if bottom {
                (w + h + (o.x1 - x)) as f32
            } else {
                (x - o.x0) as f32
            }
        }
        #[inline]
        const fn border_arc_y(o: Rect, w: i64, h: i64, left: bool, y: i64) -> f32 {
            if left {
                (2 * w + h + (o.y1 - y)) as f32
            } else {
                (w + (y - o.y0)) as f32
            }
        }
        let bottoms = [false, true];
        let mut bi = 0;
        while bi < bottoms.len() {
            let bottom = bottoms[bi];
            bi += 1;
            let y0 = if bottom { o.y1 - t } else { o.y0 };
            let mid = (y0 + t / 2) as f32;
            let mut pa = self.border_paint(
                border_arc_x(o, w, h, bottom, o.x0) * inv_per,
                (o.x0 as f32, mid),
            );
            let mut k = 0;
            while k < n {
                let xa = o.x0 + w * k / n;
                let xb = o.x0 + w * (k + 1) / n;
                k += 1;
                let r = Rect {
                    x0: xa,
                    y0,
                    x1: xb,
                    y1: y0 + t,
                };
                let pb = self.border_paint(
                    border_arc_x(o, w, h, bottom, xb) * inv_per,
                    (xb as f32, mid),
                );
                out.ramp(all, r, pa, pb);
                pa = pb;
            }
        }
        let band_start = (o.y0 - g.oy).div_euclid(g.ch);
        let band_end = (o.y1 - 1 - g.oy).div_euclid(g.ch);
        let (o_y0_t, o_y1_t) = (o.y0 + t, o.y1 - t);
        let lefts = [false, true];
        let mut li = 0;
        while li < lefts.len() {
            let left = lefts[li];
            li += 1;
            let x0 = if left { o.x0 } else { o.x1 - t };
            let mid_x = (x0 + t / 2) as f32;
            let mut r = band_start;
            while r <= band_end {
                let cur_r = r;
                r += 1;
                let ya = g.band(cur_r).max(o_y0_t);
                let yb = g.band(cur_r + 1).min(o_y1_t);
                if yb <= ya {
                    continue;
                }
                let mid_y = (ya + yb) / 2;
                let p = self.border_paint(
                    border_arc_y(o, w, h, left, mid_y) * inv_per,
                    (mid_x, mid_y as f32),
                );
                let slice = Rect {
                    x0,
                    y0: ya,
                    x1: x0 + t,
                    y1: yb,
                };
                out.side(all, slice, p);
            }
        }
    }

    #[inline]
    const fn wave(&self, lay: &TrailLay, k: i64) -> i64 {
        let amp = lay.amp;
        if lay.tx0 + (k + 1) * lay.seg > lay.held {
            return 0;
        }
        match self.clock.t_ms {
            Some(t) if t < WAVE_END_MS && amp > 0 => {
                if (k + (t / WAVE_STEP_MS) as i64) & 1 == 0 {
                    amp
                } else {
                    -amp
                }
            }
            _ => 0,
        }
    }

    #[inline]
    const fn shimmer(&self, x: f32, (px, py): (f32, f32)) -> (f32, f32) {
        const INV_SHIMMER_SIGMA: f32 = 1.0 / SHIMMER_SIGMA;
        let sweep = if self.clock.t_ms.is_none() {
            0.0
        } else {
            let d = (x - self.shimmer_pos) * INV_SHIMMER_SIGMA;
            self.head_env * crate::color_math::exp_f32(-d * d) * SHIMMER_GAIN
        };
        (sweep, self.ring_at(px, py))
    }

    #[inline]
    const fn stripe_paint_with_base(&self, k: usize, base: u32, (w, ring): (f32, f32)) -> Paint {
        if self.scene.dark {
            let w = w.max(GLINT_CREST_DARK * ring);
            light(mix_rgb(base, WHITE, w), TRAIL_COV as f32)
        } else {
            let edge = k == 0 || k + 1 == TRAIL_STOPS.len();
            let share = if edge { LIGHT_GLINT_EDGE } else { 1.0 };
            let deep = GLINT_CREST_LIGHT * ring * LIGHT_HEAD_DEEPEN * share;
            ink(
                mix_rgb(mix_rgb(base, WHITE, w * share), 0, deep),
                PRINT_ALPHA,
            )
        }
    }

    #[inline]
    fn trail(&self, out: &mut impl Ramps) {
        let Some(lay) = self.trail_lay else {
            return;
        };
        let TrailLay {
            tx0,
            drawn,
            len,
            seg,
            ya,
            yb,
            ..
        } = lay;
        if drawn <= tx0 {
            return;
        }
        let inner = self.geo.inner();
        let stripes = TRAIL_STOPS.len() as i64;
        let h = yb - ya;
        let mut k = 0;
        let mut xa = tx0;
        let mid = (ya + yb) as f32 * 0.5;
        let inv_len = 1.0 / len as f32;
        let mut wa = self.shimmer(0.0, (tx0 as f32, mid));
        let (palette, dark) = (self.scene.palette, self.scene.dark);
        let bases: [u32; TRAIL_STOPS.len()] = [
            palette.stripe(0, dark),
            palette.stripe(1, dark),
            palette.stripe(2, dark),
            palette.stripe(3, dark),
            palette.stripe(4, dark),
            palette.stripe(5, dark),
        ];
        let mut ends: [Paint; TRAIL_STOPS.len()] = [
            self.stripe_paint_with_base(0, bases[0], wa),
            self.stripe_paint_with_base(1, bases[1], wa),
            self.stripe_paint_with_base(2, bases[2], wa),
            self.stripe_paint_with_base(3, bases[3], wa),
            self.stripe_paint_with_base(4, bases[4], wa),
            self.stripe_paint_with_base(5, bases[5], wa),
        ];
        while xa < drawn {
            let xb = (xa + seg).min(drawn);
            let d = self.wave(&lay, k);
            let wb = self.shimmer((xb - tx0) as f32 * inv_len, (xb as f32, mid));
            let same_w = wb == wa;
            let mut s = 0;
            while s < ends.len() {
                let si = s as i64;
                let mut y0 = ya + h * si / stripes;
                let mut y1 = ya + h * (si + 1) / stripes;
                if s == 0 {
                    y0 -= d;
                }
                if s == TRAIL_STOPS.len() - 1 {
                    y1 -= d;
                }
                let r = Rect {
                    x0: xa,
                    y0,
                    x1: xb,
                    y1,
                };
                let end = ends[s];
                let pb = if same_w {
                    end
                } else {
                    self.stripe_paint_with_base(s, bases[s], wb)
                };
                out.ramp(inner, r, end, pb);
                ends[s] = pb;
                s += 1;
            }
            wa = wb;
            xa = xb;
            k += 1;
        }
    }

    #[inline]
    const fn trail_lay(&self) -> Option<TrailLay> {
        self.trail_lay
    }

    #[inline]
    const fn trail_cover(&self, lay: &TrailLay, k: i64) -> Rect {
        let d = self.wave(lay, k);
        let xa = lay.tx0 + k * lay.seg;
        let xb = xa + lay.seg;
        Rect {
            x0: xa,
            y0: lay.ya - d,
            x1: if xb < lay.drawn { xb } else { lay.drawn },
            y1: lay.yb - d,
        }
    }

    #[inline]
    const fn rim_paint(&self, (x, y): (f32, f32), (cx, cy): (f32, f32)) -> Paint {
        const INV_TAU: f32 = 1.0 / std::f32::consts::TAU;
        let u = crate::color_math::atan2_f32(x - cx, cy - y) * INV_TAU;
        let s = u - u.floor();
        let base = self
            .scene
            .palette
            .art_at(tri(2.0 * s + self.phi), self.scene.dark);
        let ring = self.ring_at(x, y);
        if self.scene.dark {
            light(
                mix_rgb(base, WHITE, GLINT_CREST_DARK * ring),
                RIM_COV as f32,
            )
        } else {
            ink(
                mix_rgb(base, 0, GLINT_CREST_LIGHT * ring * LIGHT_HEAD_DEEPEN),
                PRINT_ALPHA,
            )
        }
    }

    #[inline]
    const fn rim_hole(&self) -> Option<Rect> {
        let Some(t) = self.trail_lay else {
            return None;
        };
        Some(Rect {
            x0: t.tx0,
            y0: t.ya,
            x1: t.drawn,
            y1: t.yb,
        })
    }

    #[inline]
    const fn trail_ink(&self) -> TrailInk {
        let mut ink = TrailInk::new();
        if self.scene.dark {
            return ink;
        }
        let Some(lay) = self.trail_lay else {
            return ink;
        };
        while ink.n < ink.covers.len() && lay.tx0 + ink.n as i64 * lay.seg < lay.drawn {
            ink.covers[ink.n] = self.trail_cover(&lay, ink.n as i64);
            ink.n += 1;
        }
        ink
    }

    #[inline]
    const fn mark_ink(&self, k: usize, dark: u32) -> u32 {
        if self.scene.dark {
            dark
        } else {
            self.scene.palette.star_ink(k)
        }
    }

    #[inline]
    fn glints(&self, lay: &mut impl FnMut(StarCall)) {
        const INV_STAR_SETTLE_SPAN: f32 = 1.0 / STAR_SETTLE_SPAN;
        const INV_STAR_REVEAL_MS: f32 = 1.0 / STAR_REVEAL_MS;
        const INV_SHIMMER_GAIN: f32 = 1.0 / SHIMMER_GAIN;
        let Some(region) = self.star_region else {
            return;
        };
        let glints = self.glint_spots();
        if glints.n == 0 {
            return;
        }
        let Some(trail) = self.trail_lay else {
            return;
        };
        let (rest, flare) = (glints.rest, glints.flare);
        let u = self.clock.u();
        let settle = smooth((u - (1.0 - STAR_SETTLE_SPAN)) * INV_STAR_SETTLE_SPAN);
        let riding = match self.seat {
            Some(seat) => ride_span(&self.geo, self.scene, &seat)
                .map(|(from, dist)| (from, 1.0 / dist as f32)),
            None => None,
        };
        let t_sec = self.clock.t_ms.map(|t| t as f32 * 0.001);
        let rest_rgb = self.scene.palette.glint_rest();
        let inv_trail_len = 1.0 / trail.len as f32;
        let glint_spots = glints.spots();
        let mut i = 0;
        while i < glint_spots.len() {
            let s = &glint_spots[i];
            let cur_i = i;
            i += 1;
            let up = match (self.clock.ride_ms, riding) {
                (Some(t), Some((from, inv_dist))) => {
                    let passed = ride_at((s.x + flare + 1 - from) as f32 * inv_dist);
                    smooth((t as f32 - passed) * INV_STAR_REVEAL_MS)
                }
                _ => 1.0,
            };
            if up <= 0.0 {
                continue;
            }
            let (sweep, ring) = self.shimmer(
                (s.x - trail.tx0) as f32 * inv_trail_len,
                (s.x as f32, s.y as f32),
            );
            let w = (sweep * INV_SHIMMER_GAIN).max(ring).clamp(0.0, 1.0);
            let env = match t_sec {
                Some(t_sec) => {
                    let e = twinkle_env(t_sec, s.phase);
                    (e + (STAR_STEADY_ENV - e) * settle) * up
                }
                None => STAR_STEADY_ENV,
            };
            let env = (env + (1.0 - STAR_STEADY_ENV) * ring).min(1.0);
            let arm = rest + ((flare - rest) as f32 * w).round() as i64;
            lay(StarCall {
                clip: region,
                centre: (s.x, s.y),
                arm,
                cov: GLINT_COV * env,
                gold: w >= GLINT_FLARE_DOTS,
                color: self.mark_ink(cur_i, mix_rgb(rest_rgb, WHITE, w)),
                light: self.scene.dark,
            });
        }
    }

    #[inline]
    fn field(&self, lay: &mut impl FnMut(StarCall)) {
        const INV_STAR_SETTLE_SPAN: f32 = 1.0 / STAR_SETTLE_SPAN;
        const INV_STAR_REVEAL_MS: f32 = 1.0 / STAR_REVEAL_MS;
        let Some(region) = self.star_region else {
            return;
        };
        let dark = self.scene.dark;
        let (spots, n) = field_spots_at(
            &self.geo,
            self.scene,
            self.seed,
            !dark,
            region,
            self.seat,
            self.glint_spots(),
        );
        if n == 0 {
            return;
        }
        let u = self.clock.u();
        let settle = smooth((u - (1.0 - STAR_SETTLE_SPAN)) * INV_STAR_SETTLE_SPAN);
        let riding = match self.seat {
            Some(seat) => ride_span(&self.geo, self.scene, &seat)
                .map(|(from, dist)| (seat, from, 1.0 / dist as f32)),
            None => None,
        };
        let t_sec = self.clock.t_ms.map(|t| t as f32 * 0.001);
        let (even_rgb, odd_rgb) = match self.scene.palette {
            CardPalette::Cruise => (twinkle_rgb(true), twinkle_rgb(false)),
            CardPalette::Land => (twinkle_rgb(true), mix_rgb(SUNSET[3], WHITE, 0.5)),
            CardPalette::Off => (WHITE, MOONBOW[1]),
        };
        let mut i = 0;
        while i < n {
            let s = &spots[i];
            let cur_i = i;
            i += 1;
            let arm = s.arm;
            let up = match (self.clock.ride_ms, riding) {
                (Some(t), Some((seat, from, inv_dist))) if s.x < seat.rect.x0 => {
                    let passed = ride_at((s.x + arm + 1 - from) as f32 * inv_dist);
                    smooth((t as f32 - passed) * INV_STAR_REVEAL_MS)
                }
                _ => 1.0,
            };
            if up <= 0.0 {
                continue;
            }
            let ring = self.ring_at(s.x as f32, s.y as f32);
            let (env, gold) = match t_sec {
                Some(t_sec) => {
                    let e = twinkle_env(t_sec, s.phase);
                    let e = (e + (STAR_STEADY_ENV - e) * settle) * up;
                    let e = (e + (1.0 - STAR_STEADY_ENV) * ring).min(1.0);
                    (
                        e,
                        s.gold || (cur_i & 1 == 0 && twinkle_peak(e)) || ring >= 0.5,
                    )
                }
                None => (
                    STAR_STEADY_ENV + (1.0 - STAR_STEADY_ENV) * ring,
                    s.gold || ring >= 0.5,
                ),
            };
            let light_rgb = if cur_i & 1 == 0 { even_rgb } else { odd_rgb };
            lay(StarCall {
                clip: region,
                centre: (s.x, s.y),
                arm,
                cov: STAR_COV * env,
                gold,
                color: self.mark_ink(cur_i, light_rgb),
                light: dark,
            });
        }
    }

    #[inline]
    fn shed(&self, lay: &mut impl FnMut(StarCall)) {
        const INV_SHED_MS: f32 = 1.0 / SHED_MS as f32;
        const INV_SHED_STEP: f32 = 1.0 / (SHED_SPARKLES + 1) as f32;
        let (Some(t), Some(seat), Some(region)) = (self.clock.ride_ms, self.seat, self.star_region)
        else {
            return;
        };
        if t >= RIDE_MS {
            return;
        }
        let Some((from, dist)) = ride_span(&self.geo, self.scene, &seat) else {
            return;
        };
        let dark = self.scene.dark;
        let band = if dark {
            None
        } else {
            self.trail_lay().map(|t| {
                Rect {
                    x0: t.tx0,
                    y0: t.ya + t.amp,
                    x1: t.drawn,
                    y1: t.yb - t.amp,
                }
                .meet(region)
            })
        };
        let (y0, y1) = seat.body();
        let span_y = (y1 - y0) as f32;
        let chf = self.geo.ch as f32;
        let tax = if dark { 1.0 } else { STAR_ARM_INK };
        let tf = t as f32;
        let dist_f = dist as f32;
        let mut k = 0;
        while k < SHED_SPARKLES {
            let cur_k = k;
            k += 1;
            let f = (cur_k + 1) as f32 * INV_SHED_STEP;
            let age = tf - ride_at(f);
            if !(0.0..SHED_MS as f32).contains(&age) {
                continue;
            }
            let tau = age * INV_SHED_MS;
            let ratio = STAR_ARM_FINE * (1.0 - 0.4 * tau) * tax;
            let arm = i64::from(star_arm_px(star_arm(chf, ratio)));
            let m = arm + 1;
            let x = from + (dist_f * f).round() as i64 - m;
            let y =
                y0 + (span_y * (0.15 + 0.7 * unit(self.seed, 0x5348_4544 ^ cur_k as u64))) as i64;
            let (lo, hi) = (
                (region.x0 + m, region.y0 + m),
                (region.x1 - m, region.y1 - m),
            );
            if hi.0 <= lo.0 || hi.1 <= lo.1 {
                return;
            }
            let mut centre = (x.clamp(lo.0, hi.0 - 1), y.clamp(lo.1, hi.1 - 1));
            if let Some(b) = band
                && b.x1 - b.x0 > 2 * m
                && b.y1 - b.y0 > 2 * m
            {
                centre = (
                    centre.0.clamp(b.x0 + m, b.x1 - m - 1),
                    centre.1.clamp(b.y0 + m, b.y1 - m - 1),
                );
            }
            lay(StarCall {
                clip: region,
                centre,
                arm,
                cov: SHED_COV * crate::color_math::powf_pos_f32(1.0 - tau, 1.5),
                gold: false,
                color: self.mark_ink(cur_k, self.scene.palette.mark(cur_k)),
                light: dark,
            });
        }
    }

    #[inline]
    fn burst_stars(&self, lay: &mut impl FnMut(StarCall)) {
        let Some(ring) = self.burst_ring() else {
            return;
        };
        let mut k = 0;
        while k < ring.centres.len() {
            let centre = ring.centres[k];
            lay(StarCall {
                clip: ring.region,
                centre,
                arm: ring.arm,
                cov: ring.cov,
                gold: true,
                color: self.mark_ink(k, self.scene.palette.mark(k)),
                light: self.scene.dark,
            });
            k += 1;
        }
    }

    /// Every star of the frame, a dark scene's each laid on its ground ([`StarCall::laid_on`]),
    /// the glints by their rest light ([`Self::laid_glints`]).
    #[inline]
    fn each_star(&self, lay: &mut impl FnMut(StarCall)) {
        if !self.scene.dark {
            self.burst_stars(lay);
            self.glints(lay);
            self.field(lay);
            self.shed(lay);
            return;
        }
        let ground = self.scene.ground & 0x00FF_FFFF;
        let table = crate::color_math::relative_luminance_table();
        let lg = crate::color_math::relative_luminance_with(table, ground);
        let mut cache: [(u32, Option<DarkMark>); 2] = [(0, None), (0, None)];
        let mut cache_slot = 0usize;
        let mut resolve = |s: StarCall| -> StarCall {
            if !s.light {
                return s;
            }
            let dm = if let Some(dm) = cache[0].1
                && cache[0].0 == s.color
            {
                dm
            } else if let Some(dm) = cache[1].1
                && cache[1].0 == s.color
            {
                dm
            } else {
                let dm = dark_mark_with(table, s.color, ground, lg);
                cache[cache_slot] = (s.color, Some(dm));
                cache_slot ^= 1;
                dm
            };
            s.with_dark_mark(dm)
        };
        self.burst_stars(&mut |s: StarCall| lay(resolve(s)));
        self.laid_glints_with(table, ground, lg, lay);
        let lay_star = &mut |s: StarCall| lay(resolve(s));
        self.field(lay_star);
        self.shed(lay_star);
    }

    /// The glints, each laid with the polarity its rest light takes on the ground
    /// ([`StarCall::laid_as`]), so a flare toward white never flips one between ink and light.
    #[cfg(test)]
    #[inline]
    fn laid_glints(&self, lay: &mut impl FnMut(StarCall)) {
        let ground = self.scene.ground & 0x00FF_FFFF;
        let table = crate::color_math::relative_luminance_table();
        let lg = crate::color_math::relative_luminance_with(table, ground);
        self.laid_glints_with(table, ground, lg, lay);
    }

    #[inline]
    fn laid_glints_with(
        &self,
        table: &crate::color_math::RelativeLuminanceTable,
        ground: u32,
        lg: f32,
        lay: &mut impl FnMut(StarCall),
    ) {
        let rest = dark_mark_with(table, self.scene.palette.glint_rest(), ground, lg);
        self.glints(&mut |s: StarCall| lay(s.laid_as(table, rest, ground, lg)));
    }

    #[inline]
    const fn burst_ring(&self) -> Option<BurstRing> {
        const INV_BURST_MS: f32 = 1.0 / BURST_MS as f32;
        const STEP_THETA: f32 = std::f32::consts::TAU / BURST_STARS as f32;
        let age = match self.clock.ride_ms {
            Some(r) if self.rides() => r.checked_sub(RIDE_MS),
            _ => self.clock.t_ms,
        };
        let t = match age {
            Some(t) if t < BURST_MS => t,
            _ => return None,
        };
        let Some(region) = self.star_region else {
            return None;
        };
        let g = self.geo;
        let ((cx, mid), half_w) = match self.seat {
            Some(at) => {
                let r = at.rect;
                (((r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2), (r.x1 - r.x0) / 2)
            }
            None => {
                let Some((_, end)) = self.scene.trail else {
                    return None;
                };
                let col_end = g.col(end);
                let x = if col_end < region.x1 {
                    col_end
                } else {
                    region.x1
                };
                ((x, g.band(g.top + 1)), 0)
            }
        };
        let tau = t as f32 * INV_BURST_MS;
        let u = 1.0 - tau;
        let fly = 1.0 - u * u * u;
        let rx = (half_w as f32 + 2.0 * g.cw as f32).max(3.0 * g.cw as f32);
        let ry = 0.85 * g.ch as f32;
        let reach = 0.35 + 0.65 * fly;
        let ratio = STAR_ARM_HERO + (STAR_ARM_FINE - STAR_ARM_HERO) * tau;
        let ratio = ratio * if self.scene.dark { 1.0 } else { STAR_ARM_INK };
        let turn = unit(self.seed, 0x4255_5253) * std::f32::consts::TAU;
        let mut centres = [(0i64, 0i64); BURST_STARS];
        let mut k = 0;
        while k < BURST_STARS {
            let theta = turn + k as f32 * STEP_THETA;
            let (sin_t, cos_t) = crate::supernova::sin_cos_f32(theta);
            centres[k] = (
                cx + (cos_t * rx * reach).round() as i64,
                mid + (sin_t * ry * reach).round() as i64,
            );
            k += 1;
        }
        Some(BurstRing {
            region,
            centres,
            arm: star_arm_px(star_arm(g.ch as f32, ratio)) as i64,
            cov: BURST_COV * crate::color_math::powf_pos_f32(1.0 - tau, 1.2),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct StarCall {
    clip: Rect,
    centre: (i64, i64),
    arm: i64,
    cov: f32,
    gold: bool,
    color: u32,
    /// Added to the ground as light (a dark scene's), or else printed over it as ink.
    light: bool,
}

impl StarCall {
    #[inline]
    const fn with_dark_mark(self, dm: DarkMark) -> Self {
        match dm {
            DarkMark::Light(color) => Self { color, ..self },
            DarkMark::Print(color) => Self {
                color,
                light: false,
                ..self
            },
        }
    }

    /// The star laid on `ground`: a dark scene's light as [`dark_mark`] lays it, a printed star
    /// as it is.
    #[cfg(test)]
    #[inline]
    const fn laid_on(self, ground: u32) -> Self {
        if !self.light {
            return self;
        }
        self.with_dark_mark(dark_mark(self.color, ground))
    }

    #[inline]
    const fn laid_on_with(
        self,
        table: &crate::color_math::RelativeLuminanceTable,
        ground: u32,
        lg: f32,
    ) -> Self {
        if !self.light {
            return self;
        }
        self.with_dark_mark(dark_mark_with(table, self.color, ground, lg))
    }

    /// A glint laid on `ground` by its rest light's [`dark_mark`] `rest`: where that prints, the
    /// rest's floored ink through the whole flare, so the flare shows in shape alone, as a light
    /// scene's does; else its flared light as [`Self::laid_on`] lays it, which stays light (white
    /// reads wherever the rest light, or its lift, does).
    #[inline]
    const fn laid_as(
        self,
        table: &crate::color_math::RelativeLuminanceTable,
        rest: DarkMark,
        ground: u32,
        lg: f32,
    ) -> Self {
        match rest {
            DarkMark::Print(color) if self.light => Self {
                color,
                light: false,
                ..self
            },
            DarkMark::Light(_) | DarkMark::Print(_) => self.laid_on_with(table, ground, lg),
        }
    }
}

#[inline]
fn rim_strips(frame: &[&str], p: i64, rim: i64, mut strip: impl FnMut(Rect)) {
    #[inline]
    const fn rim_h_edge(frame: &[&str], art_row: i64, open_row: i64, c: i64) -> bool {
        art_at(frame, art_row, c) && !rim_solid(frame, open_row, c)
    }
    #[inline]
    const fn rim_h_outer(frame: &[&str], art_row: i64, open_row: i64, e: i64) -> bool {
        !rim_solid(frame, art_row, e) && !rim_solid(frame, open_row, e)
    }
    #[inline]
    const fn rim_v_edge(frame: &[&str], art_col: i64, open_col: i64, r: i64) -> bool {
        rim_solid(frame, r, art_col) && !rim_solid(frame, r, open_col)
    }
    let (h, w) = (frame.len() as i64, KITTY_W as i64);
    let mut b = 0;
    while b <= h {
        let row_pairs = [(b, b - 1, b * p - rim), (b - 1, b, b * p)];
        let mut rpi = 0;
        while rpi < row_pairs.len() {
            let (art_row, open_row, y0) = row_pairs[rpi];
            rpi += 1;
            let mut c = 0;
            while c < w {
                if !rim_h_edge(frame, art_row, open_row, c) {
                    c += 1;
                    continue;
                }
                let c0 = c;
                while c < w && rim_h_edge(frame, art_row, open_row, c) {
                    c += 1;
                }
                strip(Rect {
                    x0: c0 * p
                        - if rim_h_outer(frame, art_row, open_row, c0 - 1) {
                            rim
                        } else {
                            0
                        },
                    y0,
                    x1: c * p
                        + if rim_h_outer(frame, art_row, open_row, c) {
                            rim
                        } else {
                            0
                        },
                    y1: y0 + rim,
                });
            }
        }
        b += 1;
    }
    let mut v = 0;
    while v <= w {
        let col_pairs = [(v, v - 1, v * p - rim), (v - 1, v, v * p)];
        let mut cpi = 0;
        while cpi < col_pairs.len() {
            let (art_col, open_col, x0) = col_pairs[cpi];
            cpi += 1;
            let mut r = 0;
            while r < h {
                if !rim_v_edge(frame, art_col, open_col, r) {
                    r += 1;
                    continue;
                }
                let top = r;
                while r < h && rim_v_edge(frame, art_col, open_col, r) {
                    r += 1;
                }
                let q = Rect {
                    x0,
                    y0: top * p
                        + if rim_solid(frame, top - 1, open_col) {
                            rim
                        } else {
                            0
                        },
                    x1: x0 + rim,
                    y1: r * p
                        - if rim_solid(frame, r, open_col) {
                            rim
                        } else {
                            0
                        },
                };
                if q.y1 > q.y0 {
                    strip(q);
                }
            }
        }
        v += 1;
    }
}

#[derive(Clone, Copy, Debug)]
struct BurstRing {
    region: Rect,
    centres: [(i64, i64); BURST_STARS],
    arm: i64,
    cov: f32,
}

#[derive(Clone, Copy, Debug)]
struct TrailInk {
    covers: [Rect; MAX_TRAIL_SEGMENTS as usize],
    n: usize,
}

impl Default for TrailInk {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl TrailInk {
    #[inline]
    const fn new() -> Self {
        Self {
            covers: [Rect {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
            }; MAX_TRAIL_SEGMENTS as usize],
            n: 0,
        }
    }

    #[inline]
    const fn covers(&self) -> &[Rect] {
        self.covers.split_at(self.n).0
    }
}

#[cfg(test)]
#[path = "harness_card_tests.rs"]
mod tests;
