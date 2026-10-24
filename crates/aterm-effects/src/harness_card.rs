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
    contrast_ratio, floor_contrast_keeping_hue, lift_keeping_hue, relative_luminance,
};
use crate::cursor_glow::InkRole;
use crate::effect_util::{
    STAR_ARM_FINE, STAR_ARM_HERO, STAR_ARM_INK, STAR_ARM_STD, STAR_CORE_ADD, STAR_GLINT_COV,
    TWINKLE_FLASH_BOUND_HZ, TWINKLE_FLASH_HZ, mix_rgb, star_arm, star_arm_px, twinkle_env,
    twinkle_peak, twinkle_rgb,
};
use crate::genome::mix;
use crate::rainbow_kitty::meteor::tri;
use crate::rainbow_kitty::ribbon::{PAPER_MARK_CONTRAST, paper_mark_ink};
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
/// [`LINE_DARK_LUMA_FLOOR`] misses on a ground [`dark_ground`] calls dark but that is not
/// near-black (Nord's `#2e3440` at 4.16:1, an imported mid grey at 1.00:1).
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

impl Default for LineInks {
    #[inline]
    fn default() -> Self {
        Self {
            key: None,
            ink: [0; LINE_INK_STEPS + 1],
        }
    }
}

impl LineInks {
    #[inline]
    fn solve(&mut self, palette: CardPalette, ground: u32) {
        if self.key == Some((palette, ground)) {
            return;
        }
        for (i, ink) in self.ink.iter_mut().enumerate() {
            *ink = palette.text_at(i as f32 / LINE_INK_STEPS as f32, ground);
        }
        self.key = Some((palette, ground));
    }

    #[inline]
    fn at(&self, palette: CardPalette, ground: u32, at: f32) -> u32 {
        let step =
            ((at.clamp(0.0, 1.0) * LINE_INK_STEPS as f32).round() as usize).min(LINE_INK_STEPS);
        if self.key == Some((palette, ground)) {
            self.ink[step]
        } else {
            #[cfg(any(test, debug_assertions))]
            LINE_INK_SOLVES.with(|n| n.set(n.get() + 1));
            palette.text_at(step as f32 / LINE_INK_STEPS as f32, ground)
        }
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
    fn at(self, t: f32) -> u32 {
        match self {
            Self::Cruise => spectrum(t),
            Self::Land => sunset(t),
            Self::Off => moonbow(t),
        }
    }

    #[inline]
    fn art_at(self, t: f32, dark: bool) -> u32 {
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
    pub fn art(self, t: f32, dark: bool) -> u32 {
        self.art_at(t, dark)
    }

    /// Returns the legible text color at normalized position `t` (`0.0..=1.0`) by theme polarity.
    #[must_use]
    #[inline]
    pub fn letter(self, t: f32, dark: bool) -> u32 {
        let base = self.at(t);
        if dark {
            lift_to_luminance(base, LINE_DARK_LUMA_FLOOR)
        } else {
            InkRole::OverText.ink(base)
        }
    }

    /// Returns the resting line letter color at `t` (`0.0..=1.0`) against `ground` (`0x00RRGGBB`),
    /// reading at least 4.5:1 on it: on a dark ground the lifted letter floored along its hue, on
    /// paper the print law's ink.
    #[must_use]
    #[inline]
    pub fn letter_on(self, t: f32, ground: u32) -> u32 {
        let ground = ground & 0x00FF_FFFF;
        if dark_ground(ground) {
            line_floor(self.letter(t, true), ground)
        } else {
            self.text_at(t, ground)
        }
    }

    /// Returns the normalized palette position for trail stripe `k` (`0..6`).
    #[must_use]
    #[inline]
    pub fn stripe_t(self, k: usize) -> f32 {
        let k = k % TRAIL_STOPS.len();
        match self {
            Self::Cruise => stripe_at(k) as f32 / (SPECTRUM_LUT_LEN - 1) as f32,
            Self::Land | Self::Off => k as f32 / (TRAIL_STOPS.len() - 1) as f32,
        }
    }

    #[inline]
    fn stripe(self, k: usize, dark: bool) -> u32 {
        let k = k % TRAIL_STOPS.len();
        let even = k as f32 / (TRAIL_STOPS.len() - 1) as f32;
        match (self, dark) {
            (Self::Cruise, true) => spectrum_stop(TRAIL_STOPS[k]),
            (Self::Land, true) => sunset(even),
            (Self::Off, true) => moonbow(even),
            (Self::Cruise, false) => PRINT_CRUISE[k],
            (Self::Land, false) => PRINT_LAND[k],
            (Self::Off, false) => PRINT_OFF[k],
        }
    }

    #[inline]
    const fn mark(self, k: usize) -> u32 {
        match self {
            Self::Cruise => spectrum_stop(k % 7),
            Self::Land => SUNSET[k % SUNSET.len()],
            Self::Off => MOONBOW[k % MOONBOW.len()],
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
    fn star_ink(self, k: usize) -> u32 {
        print_line(self.stripe(k, false))
    }

    #[inline]
    fn text_at(self, t: f32, ground: u32) -> u32 {
        match self {
            Self::Off => paper_silver_ink(self.art_at(t, false), ground),
            Self::Cruise | Self::Land => paper_mark_ink(self.art_at(t, false), ground),
        }
    }
}

#[inline]
fn paper_silver_ink(rgb: u32, ground: u32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let y_bg = crate::color_math::relative_luminance_with(table, ground & 0x00FF_FFFF);
    let holds = |c: u32| {
        let y = crate::color_math::relative_luminance_with(table, c);
        (y.max(y_bg) + 0.05) / (y.min(y_bg) + 0.05) >= PAPER_MARK_CONTRAST
    };
    if holds(rgb) {
        return rgb;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (0u32, rgb);
    for i in 0..24 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = mix_rgb(rgb, 0, 1.0 - mid);
        if holds(mid_rgb) {
            lo = mid;
            lo_rgb = mid_rgb;
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
        }
        if i >= 1 && lo_rgb == hi_rgb {
            break;
        }
    }
    lo_rgb
}

#[inline]
fn sunset(t: f32) -> u32 {
    even_ramp(&SUNSET, t)
}

#[inline]
fn moonbow(t: f32) -> u32 {
    even_ramp(&MOONBOW, t)
}

#[inline]
fn even_ramp(stops: &[u32], t: f32) -> u32 {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let x = t * (stops.len() - 1) as f32;
    let i = (x as usize).min(stops.len() - 2);
    mix_rgb(stops[i], stops[i + 1], x - i as f32)
}

#[inline]
fn knot_ramp(knots: &[(usize, u32)], t: f32) -> u32 {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let x = t * (SPECTRUM_LUT_LEN - 1) as f32;
    let i = (1..knots.len())
        .find(|&i| x <= knots[i].0 as f32)
        .unwrap_or(knots.len() - 1);
    let ((a, ca), (b, cb)) = (knots[i - 1], knots[i]);
    mix_rgb(ca, cb, (x - a as f32) / (b - a) as f32)
}

/// Darkens `rgb` toward black until its Rec. 709 luma is at most [`LIGHT_LINE_MAX_LUMA`].
#[inline]
pub(crate) fn print_line(rgb: u32) -> u32 {
    let ch = |sh: u32| ((rgb >> sh) & 0xff) as f32;
    let y = 0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0);
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
fn stroke_px(ch: u16) -> u16 {
    (ch / 14).max(1)
}

#[inline]
fn kitty_px(ch: u16) -> u16 {
    ch.saturating_sub(stroke_px(ch)) / KITTY_HALF as u16
}

/// Returns the kitty width in whole cells at cell size `cw × ch`, or `0` if cells are too small.
#[must_use]
#[inline]
pub fn kitty_cols(cw: u16, ch: u16) -> u16 {
    let p = u32::from(kitty_px(ch));
    if p == 0 || cw == 0 {
        return 0;
    }
    let w = KITTY_W as u32 * p;
    u16::try_from(w.div_ceil(u32::from(cw))).unwrap_or(u16::MAX)
}

/// State machine tracking the harness card's intro episode, ride-in, and continuous ripple flow.
#[derive(Clone, Copy, Debug, Default)]
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
        match self.t_ms {
            Some(t) => (t as f32 / EPISODE_MS as f32).min(1.0),
            None => 1.0,
        }
    }

    #[inline]
    const fn intro_rise(self) -> f32 {
        smooth((self.u() - GLINT_INTRO_FROM) / (1.0 - GLINT_INTRO_FROM))
    }

    #[inline]
    const fn phi(self) -> f32 {
        let trip = FLOW_TRIP_MS as f32;
        match (self.t_ms, self.flow) {
            (Some(t), _) => {
                let v = 1.0 - self.u();
                PHI_SETTLED + PHI_INTRO * v * v - t as f32 / trip
            }
            (None, Some(f)) => PHI_SETTLED - (f.hue_ms % (2 * FLOW_TRIP_MS)) as f32 / trip,
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
        if self.t_ms.is_none() {
            return 0.0;
        }
        let u = self.u();
        smooth(u / 0.04) * smooth((1.0 - u) / 0.12)
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
    u64::try_from(to.saturating_duration_since(from).as_millis()).unwrap_or(u64::MAX)
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
    fn meet(self, o: Self) -> Self {
        Self {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        }
    }

    #[inline]
    const fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    #[inline]
    fn contains(self, x: i64, y: i64) -> bool {
        (self.x0..self.x1).contains(&x) && (self.y0..self.y1).contains(&y)
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
    fn new(s: &CardScene) -> Option<Self> {
        if s.cw == 0 || s.ch == 0 || s.rows == 0 || s.cols == 0 {
            return None;
        }
        let (cw, ch) = (i64::from(s.cw), i64::from(s.ch));
        let (ox, oy) = (i64::from(s.origin_x), i64::from(s.origin_y));
        let top = i64::from(s.top);
        let rows = i64::from(s.rows);
        let pad = i64::from(s.pad_left).min(cw / 2);
        let core = Rect {
            x0: ox - pad,
            y0: oy + top * ch,
            x1: ox + (i64::from(s.cols) + 1) * cw,
            y1: oy + (top + rows) * ch,
        };
        let air = |pad: u16| i64::from(pad).min(ch / 2);
        let outer = Rect {
            y0: core.y0 - if top > 0 { air(s.pad_above) } else { 0 },
            y1: core.y1 + air(s.pad_below),
            ..core
        };
        let space = Rect {
            x0: 0,
            y0: 0,
            x1: i64::from(u16::MAX),
            y1: i64::from(u16::MAX),
        };
        let (cx0, cy0, cx1, cy1) = s.clip;
        let pane = Rect {
            x0: i64::from(cx0),
            y0: i64::from(cy0),
            x1: i64::from(cx1),
            y1: i64::from(cy1),
        };
        let clip = outer.meet(space).meet(pane);
        if clip.is_empty() {
            return None;
        }
        let t = i64::from(stroke_px(s.ch));
        let wand = match s.wand {
            Some((c, r)) => (
                (ox + i64::from(c) * cw) as f32 + 0.5 * cw as f32,
                (oy + i64::from(r) * ch) as f32 + 0.5 * ch as f32,
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
            row_base: i64::from(s.row_base),
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
fn clip_ramp(r: Rect, c: Rect, l: Paint, rt: Paint) -> (Paint, Paint) {
    if (c.x0, c.x1) == (r.x0, r.x1) {
        (l, rt)
    } else {
        let span = (r.x1 - r.x0) as f32;
        let at = |x: i64| ramp(l, rt, (x - r.x0) as f32 / span);
        (at(c.x0), at(c.x1))
    }
}

#[inline]
fn ramp(a: Paint, b: Paint, f: f32) -> Paint {
    let f = f.clamp(0.0, 1.0);
    let ch = |x: u32, y: u32| ((x as f32 + (y as f32 - x as f32) * f).round() as u32).min(255);
    let rgb = |x: u32, y: u32| {
        (ch((x >> 16) & 0xff, (y >> 16) & 0xff) << 16)
            | (ch((x >> 8) & 0xff, (y >> 8) & 0xff) << 8)
            | ch(x & 0xff, y & 0xff)
    };
    let alpha = if a.alpha == 0 {
        0
    } else {
        ch(u32::from(a.alpha), u32::from(b.alpha)).max(1) as u8
    };
    Paint {
        color: rgb(a.color, b.color),
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
    fn new(coat: u32, eye: u32, dark: bool) -> Self {
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
fn kitty_frame(clock: Clock) -> KittyPose {
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
    let in_window = |at: u64, period: u64, len: u64| t >= at && (t - at) % period < len;
    if in_window(BLINK_AT_MS, BLINK_PERIOD_MS, BLINK_MS) {
        KittyPose::Blink
    } else if in_window(FLICK_AT_MS, FLICK_PERIOD_MS, FLICK_MS) {
        KittyPose::Flick
    } else {
        KittyPose::Open
    }
}

#[inline]
fn unit(seed: u64, salt: u64) -> f32 {
    (mix(seed ^ mix(salt)) >> 40) as f32 / (1u64 << 24) as f32
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct StarSpot {
    x: i64,
    y: i64,
    ratio: f32,
    phase: f32,
    gold: bool,
}

#[inline]
fn star_region(g: &Geo, s: &CardScene) -> Option<Rect> {
    if g.rows < 2 {
        return None;
    }
    let start = match (s.trail, s.kitty_col) {
        (Some((t0, _)), Some(k)) => t0.min(k),
        (Some((t0, _)), None) => t0,
        (None, Some(k)) => k,
        (None, None) => return None,
    };
    let inner = g.inner();
    let r = Rect {
        x0: g.col(start),
        y0: g.band(g.top),
        x1: if s.dark {
            inner.x1
        } else {
            inner.x1.min(g.col(s.cols))
        },
        y1: g.band(g.top + 2),
    }
    .meet(inner)
    .meet(g.clip);
    (!r.is_empty()).then_some(r)
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
fn rim_px(p: i64, t: i64) -> i64 {
    t.min(p / 2)
}

#[inline]
fn art_at(frame: &[&str], r: i64, c: i64) -> bool {
    usize::try_from(r)
        .ok()
        .and_then(|r| frame.get(r))
        .zip(usize::try_from(c).ok())
        .and_then(|(row, c)| row.as_bytes().get(c))
        .is_some_and(|&b| b != b'.')
}

#[inline]
fn rim_solid(frame: &[&str], r: i64, c: i64) -> bool {
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
    (at.rect.meet(g.inner()).meet(g.clip) == at.rect).then_some(at)
}

#[inline]
fn ride(clock: Clock) -> f32 {
    match clock.ride_ms {
        Some(t) if t < RIDE_MS => {
            let v = 1.0 - t as f32 / RIDE_MS as f32;
            1.0 - v * v * v
        }
        _ => 1.0,
    }
}

#[inline]
fn ride_at(f: f32) -> f32 {
    RIDE_MS as f32 * (1.0 - (1.0 - f.clamp(0.0, 1.0)).cbrt())
}

#[inline]
fn ride_span(g: &Geo, s: &CardScene, seat: &KittyAt) -> Option<(i64, i64)> {
    let (c0, _) = s.trail?;
    let from = g.col(c0).max(g.inner().x0).max(g.clip.x0);
    let dist = seat.rect.x0 - from;
    (dist > 0).then_some((from, dist))
}

#[inline]
fn ridden(g: &Geo, s: &CardScene, seat: KittyAt, clock: Clock) -> KittyAt {
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

fn trail_rect(g: &Geo, s: &CardScene, seat: Option<KittyAt>) -> Option<(Rect, i64)> {
    let (c0, c1) = s.trail?;
    if g.rows < 2 {
        return None;
    }
    let inner = g.inner();
    let (tx0, mut tx1) = (g.col(c0).max(inner.x0), g.col(c1).min(inner.x1));
    if let Some(seat) = seat.filter(|seat| tx1 >= seat.rect.x0) {
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
fn across_trail(b: Rect, (t, amp): (Rect, i64)) -> bool {
    let outer = Rect {
        y0: t.y0 - amp,
        y1: t.y1 + amp,
        ..t
    };
    let inside = b.x0 >= t.x0 && b.x1 <= t.x1 && b.y0 >= t.y0 + amp && b.y1 <= t.y1 - amp;
    !b.meet(outer).is_empty() && !inside
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct GlintSpot {
    x: i64,
    y: i64,
    phase: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct Glints {
    spots: [GlintSpot; GLINTS_MAX],
    n: usize,
    rest: i64,
    flare: i64,
}

impl Glints {
    #[inline]
    fn spots(&self) -> &[GlintSpot] {
        &self.spots[..self.n]
    }
}

#[inline]
fn glint_arms(g: &Geo, dark: bool, h: i64, amp: i64) -> (i64, i64) {
    let tax = if dark { 1.0 } else { STAR_ARM_INK };
    let fits = ((h - 2 * amp - 1) / 2 - 1).max(0);
    let arm = |ratio: f32| i64::from(star_arm_px(star_arm(g.ch as f32, ratio * tax))).min(fits);
    (arm(STAR_ARM_STD), arm(STAR_ARM_HERO))
}

fn glint_spots(g: &Geo, s: &CardScene, seed: u64) -> Glints {
    let mut out = Glints::default();
    let Some(region) = star_region(g, s) else {
        return out;
    };
    let seat = kitty_rect(g, s);
    let Some((t, amp)) = trail_rect(g, s, seat) else {
        return out;
    };
    let (rest, flare) = glint_arms(g, s.dark, t.y1 - t.y0, amp);
    if rest < 1 {
        return out;
    }
    let m = flare + 1;
    let free = seat.map_or(t.x1, |at| at.rect.x0.min(t.x1));
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
    let n = usize::try_from(cells / GLINT_CELLS)
        .unwrap_or(0)
        .clamp(1, GLINTS_MAX);
    let stripes = TRAIL_STOPS.len() as i64;
    let first = (mix(seed ^ 0x474C_494E) % 4) as usize;
    for (i, spot) in out.spots[..n].iter_mut().enumerate() {
        let span = room.x1 - room.x0;
        let at = (i as f32 + 0.2 + 0.6 * unit(seed, 0x474C_0000 | i as u64)) / n as f32;
        let x = room.x0 + (span as f32 * at) as i64;
        let k = 1 + [0i64, 2, 1, 3][(first + i) % 4];
        let centre = t.y0 + (t.y1 - t.y0) * (2 * k + 1) / (2 * stripes);
        let mut y = centre.clamp(room.y0, room.y1 - 1);
        let row_of = |py: i64| (py - g.oy).div_euclid(g.ch);
        if row_of(y - m) != row_of(y + m) {
            let line = g.band(row_of(y + m));
            let (above, below) = (line - m - 1, line + m);
            let side = if centre < line { above } else { below };
            if (room.y0..room.y1).contains(&side) {
                y = side;
            }
        }
        *spot = GlintSpot {
            x,
            y,
            phase: unit(seed, 0x474C_5048 ^ i as u64) * std::f32::consts::PI,
        };
    }
    Glints {
        n,
        rest,
        flare,
        ..out
    }
}

fn field_spots(
    g: &Geo,
    s: &CardScene,
    seed: u64,
    light_theme: bool,
) -> ([StarSpot; STARS_MAX], usize) {
    let mut spots = [StarSpot {
        x: 0,
        y: 0,
        ratio: 0.0,
        phase: 0.0,
        gold: false,
    }; STARS_MAX];
    let Some(region) = star_region(g, s) else {
        return (spots, 0);
    };
    let seat = kitty_rect(g, s);
    let kitty = seat.map(|at| at.rect);
    let trail = if light_theme {
        trail_rect(g, s, seat)
    } else {
        None
    };
    let glints = glint_spots(g, s, seed);
    let want = (STARS_MIN + mix(seed ^ 0x5354_4152) % STARS_SPAN) as usize;
    let gap = g.cw.max(g.ch / 2);
    let mut n = 0;
    for i in 0..want {
        let ratio =
            STAR_RATIOS[i % STAR_RATIOS.len()] * if light_theme { STAR_ARM_INK } else { 1.0 };
        let arm = i64::from(star_arm_px(star_arm(g.ch as f32, ratio)));
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
            x0: room.x0 + span * i as i64 / want as i64,
            x1: room.x0 + span * (i as i64 + 1) / want as i64,
            ..room
        };
        let kitty_pad = kitty.map(|r| Rect {
            x0: r.x0 - m,
            y0: r.y0 - m,
            x1: r.x1 + m,
            y1: r.y1 + m,
        });
        let room_h = (room.y1 - room.y0) as f32;
        for k in 0..STAR_TRIES {
            let salt = ((i as u64) << 8) | k;
            let within = if k < STAR_TRIES / 2 { slice } else { room };
            let x = within.x0 + (unit(seed, salt * 2) * (within.x1 - within.x0) as f32) as i64;
            let y = room.y0 + (unit(seed, salt * 2 + 1) * room_h) as i64;
            let on_kitty = kitty_pad.is_some_and(|r| r.contains(x, y));
            let crowded = spots[..n]
                .iter()
                .map(|o| (o.x, o.y))
                .chain(glints.spots().iter().map(|o| (o.x, o.y)))
                .any(|(ox, oy)| (ox - x).abs() < gap && (oy - y).abs() < gap);
            let reach = Rect {
                x0: x - m,
                y0: y - m,
                x1: x + m + 1,
                y1: y + m + 1,
            };
            let across = trail.is_some_and(|t| across_trail(reach, t));
            if !on_kitty && !crowded && !across {
                spots[n] = StarSpot {
                    x,
                    y,
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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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

impl HarnessCard {
    /// Arms an intro episode at `now` with star `seed`, preserving any active ripple clock.
    #[inline]
    pub fn arm(&mut self, now: Instant, seed: u64) {
        self.episode = Episode::Armed(now);
        self.seed = seed;
        self.still = false;
        self.flows = true;
        self.glint0.get_or_insert(now);
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
        let period = i64::try_from(period).unwrap_or(i64::MAX);
        let ahead = i64::try_from(ms_between(g0, at)).unwrap_or(i64::MAX);
        let behind = i64::try_from(ms_between(at, g0)).unwrap_or(i64::MAX);
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
        self.still = !(animate && drawable) || scene.serious;
        self.drew = Drew::Settled;
        Some(kept.at(origin))
    }

    /// Replays the cached flow [`CardFrame`] from `frames` when `now` falls in the same step.
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
        (ms < EPISODE_MS).then_some(ms)
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
    pub fn draw(
        &mut self,
        now: Instant,
        scene: &CardScene,
        animate: bool,
        frames: &mut CardFrames,
    ) -> CardFrame {
        let (shape, origin) = CardShape::of(scene);
        let local = shape.scene();
        let geo = Geo::new(local).filter(|_| CardArts::dimensions(local.ch).is_some());
        let clock = self.step(now, scene, animate, geo.is_some());
        let (under, over) = match geo.zip(clock) {
            Some((geo, clock)) => frames.fill(&Emit::new(local, geo, clock, self.seed)),
            None => CardFrames::nothing(),
        };
        let step = clock.and_then(|c| c.flow).map(|f| (f.step, self.grid_gen));
        frames.keep((self.drew != Drew::Intro).then(|| KeptFrame {
            shape,
            step,
            under: Arc::clone(&under),
            over: Arc::clone(&over),
        }));
        CardFrame {
            under,
            over,
            origin,
        }
    }

    fn step(
        &mut self,
        now: Instant,
        scene: &CardScene,
        animate: bool,
        drawable: bool,
    ) -> Option<Clock> {
        self.palette = scene.palette;
        if !scene.dark {
            self.inks.solve(scene.palette, scene.ground & 0x00FF_FFFF);
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
                self.glint0.get_or_insert(now);
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
        self.fade0.map_or(1.0, |f| {
            smooth(ms_between(f, at) as f32 / GLINT_FADE_MS as f32)
        })
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
                hue_ms: self.hue0.map_or(ms, |h| ms_between(h, step_at)),
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
    pub fn line_ink_into(
        &self,
        now: Option<Instant>,
        at: &LineAt,
        ground: u32,
        animate: bool,
        out: &mut Vec<InkCell>,
    ) {
        let ground = ground & 0x00FF_FFFF;
        let dark = dark_ground(ground);
        debug_assert!(
            now.is_some() || self.is_still() || !animate,
            "a card that moves needs the clock for its line"
        );
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
        let pos = (clock.u() * LINE_SWEEPS).fract() * 1.4 - 0.2;
        let span = f32::from(at.len.saturating_sub(1).max(1));
        let hero = at.hero.clone();
        let hero_span = f32::from(hero.end.saturating_sub(hero.start).saturating_sub(1).max(1));
        let (wc, wr) = at.wand.unwrap_or((at.col, at.row));
        let glint_at = |c: u16| match clock.glint {
            Some((ms, up)) if up > 0.0 => {
                let d = cell_distance(
                    f32::from(c) - f32::from(wc),
                    f32::from(at.row) - f32::from(wr),
                );
                up * glint(d, ms)
            }
            _ => 0.0,
        };
        let (palette, inks) = (self.palette, &self.inks);
        out.extend(
            (0..at.len)
                .map_while(|i| Some((i, at.col.checked_add(i)?)))
                .map(|(i, c)| {
                    let x = f32::from(i) / span;
                    let place = tri(x + phi);
                    let sweep = if env == 0.0 {
                        0.0
                    } else {
                        let d = (x - pos) / LINE_SWEEP_SIGMA;
                        env * (-d * d).exp()
                    };
                    let crest = sweep.max(glint_at(c));
                    let rgb = if hero.contains(&i) {
                        let k = f32::from(i - hero.start) / hero_span;
                        let place = tri(k * LINE_HERO_SPREAD + phi);
                        if dark {
                            line_floor(
                                mix_rgb(
                                    lift_to_luminance(palette.at(place), LINE_HERO_DARK_LUMA_FLOOR),
                                    WHITE,
                                    crest * LINE_HERO_SWEEP_GAIN,
                                ),
                                ground,
                            )
                        } else {
                            mix_rgb(
                                inks.at(palette, ground, place),
                                0,
                                crest * LINE_SWEEP_GAIN_LIGHT,
                            )
                        }
                    } else if dark {
                        line_floor(
                            mix_rgb(palette.letter(place, true), WHITE, crest * LINE_SWEEP_GAIN),
                            ground,
                        )
                    } else {
                        mix_rgb(
                            inks.at(palette, ground, place),
                            0,
                            crest * LINE_SWEEP_GAIN_LIGHT,
                        )
                    };
                    InkCell {
                        row: at.row,
                        col: c,
                        color: [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8],
                    }
                }),
        );
    }
}

/// A dark-ground line letter `rgb` floored along its hue to [`LINE_CONTRAST`] on `ground`: kept
/// where it already reads, lifted further toward white on a dim ground (Nord's `#2e3440`), and
/// darkened on one past the polarity at which black reads better than white though still under
/// [`dark_ground`]'s cut (an imported mid grey), so a sweep's white never washes it out.
#[inline]
fn line_floor(rgb: u32, ground: u32) -> u32 {
    floor_contrast_keeping_hue(rgb, ground, LINE_CONTRAST)
}

/// Mixes `rgb` toward white to reach WCAG relative luminance `floor` on a `2^-14` step grid.
#[inline]
pub(crate) fn lift_to_luminance(rgb: u32, floor: f32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    if crate::color_math::relative_luminance_with(table, rgb) >= floor {
        return rgb;
    }
    let reaches =
        |t: f32| crate::color_math::relative_luminance_with(table, mix_rgb(rgb, WHITE, t)) >= floor;
    let (mut short, mut step) = (0.0f32, 0.25f32);
    for _ in 0..7 {
        let (a, b, c) = (
            reaches(short + step),
            reaches(short + 2.0 * step),
            reaches(short + 3.0 * step),
        );
        short += step * f32::from(u8::from(!a) + u8::from(!b) + u8::from(!c));
        step *= 0.25;
    }
    mix_rgb(rgb, WHITE, short + 4.0 * step)
}

/// Returns `true` when background `bg` (`0x00RRGGBB`) has WCAG relative luminance below `0.5`.
#[must_use]
#[inline]
pub fn dark_ground(bg: u32) -> bool {
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
/// the ground; where it does not but white reads better on the ground than black — every
/// near-black and dim ground — the light lifted along its hue to the least lightness that reads,
/// added (the burst's blue reads 1.87:1 on Aterm Night, `#4e4eff` 3:1); and otherwise, on a ground
/// past that polarity though still under [`dark_ground`]'s cut, where added light misses the floor
/// or cannot reach it at all (white reads 2.10:1 at most on an imported `#ff8cd9`), or on a dim
/// ground where even white light misses it (`#eb0000`), the light floored along its hue to
/// [`MARK_CONTRAST`] and printed.
#[must_use]
pub fn dark_mark(rgb: u32, ground: u32) -> DarkMark {
    let (rgb, ground) = (rgb & 0x00FF_FFFF, ground & 0x00FF_FFFF);
    let reads = |c: u32| {
        let lit = add_sat(ground, premul_rgb(c, STAR_PRINT_FULL as u8));
        contrast_ratio(lit, ground) >= MARK_CONTRAST
    };
    if reads(rgb) {
        return DarkMark::Light(rgb);
    }
    if contrast_ratio(WHITE, ground) > contrast_ratio(0, ground) {
        let lifted = lift_keeping_hue(rgb, reads);
        if reads(lifted) {
            return DarkMark::Light(lifted);
        }
    }
    DarkMark::Print(floor_contrast_keeping_hue(rgb, ground, MARK_CONTRAST))
}

/// The tint and blend of the `✦` leading a card line whose first letter wears `rgb` on `ground`
/// (both `0x00RRGGBB`): on a dark ground the card's mark ([`dark_mark`]) — the letter's light
/// added where it reads, or lifted along its hue to read where white is the better polarity, its
/// ink laid over the ground where added light cannot read (a mid ground, on which the letter is
/// floored dark: Silver Aerogel's `#929292` read the added `✦` at 1.6:1) — and on paper the
/// letter's ink laid over.
#[must_use]
pub fn line_star(rgb: u32, ground: u32) -> (u32, DecoBlend) {
    if !dark_ground(ground & 0x00FF_FFFF) {
        return (rgb, DecoBlend::Over);
    }
    match dark_mark(rgb, ground) {
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
    [
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
    ]
    .into_iter()
    .all(|piece| lay_cut(piece, k + 1, last, cover, lay))
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
}

impl<'a> Emit<'a> {
    fn new(scene: &'a CardScene, geo: Geo, clock: Clock, seed: u64) -> Self {
        let seat = kitty_rect(&geo, scene);
        Self {
            scene,
            geo,
            clock,
            seed,
            phi: clock.phi(),
            head_env: clock.head_env(),
            head_pos: (clock.u() * HEAD_LAPS).fract(),
            shimmer_pos: clock
                .t_ms
                .map_or(0.0, |t| (t as f32 / SHIMMER_MS).fract() * 1.4 - 0.2),
            kitty: seat.map(|at| (ridden(&geo, scene, at, clock), kitty_frame(clock))),
            seat,
        }
    }

    #[inline]
    fn rides(&self) -> bool {
        self.seat
            .is_some_and(|seat| ride_span(&self.geo, self.scene, &seat).is_some())
    }

    #[inline]
    fn ring_at(&self, x: f32, y: f32) -> f32 {
        let Some((ms, up)) = self.clock.glint.filter(|&(_, up)| up > 0.0) else {
            return 0.0;
        };
        let g = self.geo;
        let d = cell_distance((x - g.wand.0) / g.cw as f32, (y - g.wand.1) / g.ch as f32);
        up * glint(d, ms)
    }

    #[inline]
    fn border_paint(&self, s: f32, (x, y): (f32, f32)) -> Paint {
        let base = self
            .scene
            .palette
            .art_at(tri(2.0 * s + self.phi), self.scene.dark);
        let env = self.head_env;
        let w = if env > 0.0 {
            let d = (s - self.head_pos).rem_euclid(1.0);
            let d = d.min(1.0 - d) / HEAD_SIGMA;
            env * (-d * d).exp()
        } else {
            0.0
        };
        let ring = self.ring_at(x, y);
        if self.scene.dark {
            let w = (w * HEAD_GAIN).max(GLINT_CREST_DARK * ring);
            light(mix_rgb(base, WHITE, w), f32::from(BORDER_COV))
        } else {
            let w = w.max(GLINT_CREST_LIGHT * ring);
            ink(
                mix_rgb(print_line(base), 0, w * LIGHT_HEAD_DEEPEN),
                PRINT_ALPHA,
            )
        }
    }

    fn border(&self, out: &mut impl Ramps) {
        let g = self.geo;
        let o = g.outer;
        let (w, h, t) = (o.x1 - o.x0, o.y1 - o.y0, g.t);
        let per = (2 * (w + h)) as f32;
        let all = g.clip;
        let n = ((w + g.cw - 1) / g.cw).clamp(1, MAX_EDGE_SEGMENTS);
        for bottom in [false, true] {
            let y0 = if bottom { o.y1 - t } else { o.y0 };
            let arc = |x: i64| {
                if bottom {
                    (w + h + (o.x1 - x)) as f32
                } else {
                    (x - o.x0) as f32
                }
            };
            let mid = (y0 + t / 2) as f32;
            let mut pa = self.border_paint(arc(o.x0) / per, (o.x0 as f32, mid));
            for k in 0..n {
                let xa = o.x0 + w * k / n;
                let xb = o.x0 + w * (k + 1) / n;
                let r = Rect {
                    x0: xa,
                    y0,
                    x1: xb,
                    y1: y0 + t,
                };
                let pb = self.border_paint(arc(xb) / per, (xb as f32, mid));
                out.ramp(all, r, pa, pb);
                pa = pb;
            }
        }
        let bands = (o.y0 - g.oy).div_euclid(g.ch)..=(o.y1 - 1 - g.oy).div_euclid(g.ch);
        for left in [false, true] {
            let x0 = if left { o.x0 } else { o.x1 - t };
            let arc = |y: i64| {
                if left {
                    (2 * w + h + (o.y1 - y)) as f32
                } else {
                    (w + (y - o.y0)) as f32
                }
            };
            for r in bands.clone() {
                let ya = g.band(r).max(o.y0 + t);
                let yb = g.band(r + 1).min(o.y1 - t);
                if yb <= ya {
                    continue;
                }
                let p = self.border_paint(
                    arc((ya + yb) / 2) / per,
                    ((x0 + t / 2) as f32, ((ya + yb) / 2) as f32),
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
                if (k + (t / WAVE_STEP_MS) as i64).rem_euclid(2) == 0 {
                    amp
                } else {
                    -amp
                }
            }
            _ => 0,
        }
    }

    #[inline]
    fn shimmer(&self, x: f32, (px, py): (f32, f32)) -> (f32, f32) {
        let sweep = if self.clock.t_ms.is_none() {
            0.0
        } else {
            let d = (x - self.shimmer_pos) / SHIMMER_SIGMA;
            self.head_env * (-d * d).exp() * SHIMMER_GAIN
        };
        (sweep, self.ring_at(px, py))
    }

    #[inline]
    fn stripe_paint(&self, k: usize, (w, ring): (f32, f32)) -> Paint {
        let base = self.scene.palette.stripe(k, self.scene.dark);
        if self.scene.dark {
            let w = w.max(GLINT_CREST_DARK * ring);
            light(mix_rgb(base, WHITE, w), f32::from(TRAIL_COV))
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

    fn trail(&self, out: &mut impl Ramps) {
        let Some(lay) = self.trail_lay() else {
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
        let inner = self.geo.inner();
        let stripes = TRAIL_STOPS.len() as i64;
        let h = yb - ya;
        let mut k = 0;
        let mut xa = tx0;
        let mid = (ya + yb) as f32 * 0.5;
        let mut wa = self.shimmer(0.0, (tx0 as f32, mid));
        let mut ends: [Paint; TRAIL_STOPS.len()] =
            std::array::from_fn(|s| self.stripe_paint(s, wa));
        while xa < drawn {
            let xb = (xa + seg).min(drawn);
            let d = self.wave(&lay, k);
            let wb = self.shimmer((xb - tx0) as f32 / len as f32, (xb as f32, mid));
            let same_w = wb == wa;
            for (s, end) in ends.iter_mut().enumerate() {
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
                let pb = if same_w {
                    *end
                } else {
                    self.stripe_paint(s, wb)
                };
                out.ramp(inner, r, *end, pb);
                *end = pb;
            }
            wa = wb;
            xa = xb;
            k += 1;
        }
    }

    fn trail_lay(&self) -> Option<TrailLay> {
        let g = self.geo;
        let (
            Rect {
                x0: tx0,
                y0: ya,
                x1: tx1,
                y1: yb,
            },
            amp,
        ) = trail_rect(&g, self.scene, self.seat)?;
        let len = tx1 - tx0;
        let kitty = self.kitty.map(|(at, _)| at);
        let drawn = match kitty {
            Some(at) if ride(self.clock) < 1.0 => tx1.min(at.rear()),
            _ => tx1,
        };
        let held = kitty
            .map(|at| at.rect.x0 - rim_px(at.p, g.t))
            .filter(|&held| drawn > held)
            .unwrap_or(i64::MAX);
        let seg = if self.clock.t_ms.is_some() || self.clock.flow.is_some() {
            (TRAIL_SEG_CELLS * g.cw).max((len + MAX_TRAIL_SEGMENTS - 1) / MAX_TRAIL_SEGMENTS)
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
    fn trail_cover(&self, lay: &TrailLay, k: i64) -> Rect {
        let d = self.wave(lay, k);
        let xa = lay.tx0 + k * lay.seg;
        Rect {
            x0: xa,
            y0: lay.ya - d,
            x1: (xa + lay.seg).min(lay.drawn),
            y1: lay.yb - d,
        }
    }

    #[inline]
    fn rim_paint(&self, (x, y): (f32, f32), (cx, cy): (f32, f32)) -> Paint {
        let s = ((x - cx).atan2(cy - y) / std::f32::consts::TAU).rem_euclid(1.0);
        let base = self
            .scene
            .palette
            .art_at(tri(2.0 * s + self.phi), self.scene.dark);
        let ring = self.ring_at(x, y);
        if self.scene.dark {
            light(
                mix_rgb(base, WHITE, GLINT_CREST_DARK * ring),
                f32::from(RIM_COV),
            )
        } else {
            ink(
                mix_rgb(base, 0, GLINT_CREST_LIGHT * ring * LIGHT_HEAD_DEEPEN),
                PRINT_ALPHA,
            )
        }
    }

    #[inline]
    fn rim_hole(&self) -> Option<Rect> {
        let t = self.trail_lay()?;
        Some(Rect {
            x0: t.tx0,
            y0: t.ya,
            x1: t.drawn,
            y1: t.yb,
        })
    }

    fn trail_ink(&self) -> TrailInk {
        let mut ink = TrailInk::default();
        if self.scene.dark {
            return ink;
        }
        let Some(lay) = self.trail_lay() else {
            return ink;
        };
        while ink.n < ink.covers.len() && lay.tx0 + ink.n as i64 * lay.seg < lay.drawn {
            ink.covers[ink.n] = self.trail_cover(&lay, ink.n as i64);
            ink.n += 1;
        }
        ink
    }

    #[inline]
    fn mark_ink(&self, k: usize, dark: u32) -> u32 {
        if self.scene.dark {
            dark
        } else {
            self.scene.palette.star_ink(k)
        }
    }

    fn glints(&self, lay: &mut impl FnMut(StarCall)) {
        let Some(region) = star_region(&self.geo, self.scene) else {
            return;
        };
        let glints = glint_spots(&self.geo, self.scene, self.seed);
        let Some(trail) = self.trail_lay().filter(|_| glints.n > 0) else {
            return;
        };
        let (rest, flare) = (glints.rest, glints.flare);
        let u = self.clock.u();
        let settle = smooth((u - (1.0 - STAR_SETTLE_SPAN)) / STAR_SETTLE_SPAN);
        let riding = self
            .seat
            .and_then(|seat| ride_span(&self.geo, self.scene, &seat));
        let t_sec = self.clock.t_ms.map(|t| t as f32 / 1000.0);
        let rest_rgb = self.scene.palette.glint_rest();
        for (i, s) in glints.spots().iter().enumerate() {
            let up = match (self.clock.ride_ms, riding) {
                (Some(t), Some((from, dist))) => {
                    let passed = ride_at((s.x + flare + 1 - from) as f32 / dist as f32);
                    smooth((t as f32 - passed) / STAR_REVEAL_MS)
                }
                _ => 1.0,
            };
            if up <= 0.0 {
                continue;
            }
            let (sweep, ring) = self.shimmer(
                (s.x - trail.tx0) as f32 / trail.len as f32,
                (s.x as f32, s.y as f32),
            );
            let w = (sweep / SHIMMER_GAIN).max(ring).clamp(0.0, 1.0);
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
                color: self.mark_ink(i, mix_rgb(rest_rgb, WHITE, w)),
                light: self.scene.dark,
            });
        }
    }

    fn field(&self, lay: &mut impl FnMut(StarCall)) {
        let Some(region) = star_region(&self.geo, self.scene) else {
            return;
        };
        let dark = self.scene.dark;
        let (spots, n) = field_spots(&self.geo, self.scene, self.seed, !dark);
        let u = self.clock.u();
        let settle = smooth((u - (1.0 - STAR_SETTLE_SPAN)) / STAR_SETTLE_SPAN);
        let riding = self
            .seat
            .and_then(|seat| Some((seat, ride_span(&self.geo, self.scene, &seat)?)));
        let t_sec = self.clock.t_ms.map(|t| t as f32 / 1000.0);
        let chf = self.geo.ch as f32;
        let (even_rgb, odd_rgb) = match self.scene.palette {
            CardPalette::Cruise => (twinkle_rgb(true), twinkle_rgb(false)),
            CardPalette::Land => (twinkle_rgb(true), mix_rgb(SUNSET[3], WHITE, 0.5)),
            CardPalette::Off => (WHITE, MOONBOW[1]),
        };
        for (i, s) in spots[..n].iter().enumerate() {
            let arm = i64::from(star_arm_px(star_arm(chf, s.ratio)));
            let up = match (self.clock.ride_ms, riding) {
                (Some(t), Some((seat, (from, dist)))) if s.x < seat.rect.x0 => {
                    let passed = ride_at((s.x + arm + 1 - from) as f32 / dist as f32);
                    smooth((t as f32 - passed) / STAR_REVEAL_MS)
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
                    (e, s.gold || (i % 2 == 0 && twinkle_peak(e)) || ring >= 0.5)
                }
                None => (
                    STAR_STEADY_ENV + (1.0 - STAR_STEADY_ENV) * ring,
                    s.gold || ring >= 0.5,
                ),
            };
            let light_rgb = if i % 2 == 0 { even_rgb } else { odd_rgb };
            lay(StarCall {
                clip: region,
                centre: (s.x, s.y),
                arm,
                cov: STAR_COV * env,
                gold,
                color: self.mark_ink(i, light_rgb),
                light: dark,
            });
        }
    }

    fn shed(&self, lay: &mut impl FnMut(StarCall)) {
        let (Some(t), Some(seat)) = (self.clock.ride_ms, self.seat) else {
            return;
        };
        if t >= RIDE_MS {
            return;
        }
        let (Some((from, dist)), Some(region)) = (
            ride_span(&self.geo, self.scene, &seat),
            star_region(&self.geo, self.scene),
        ) else {
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
        for k in 0..SHED_SPARKLES {
            let f = (k + 1) as f32 / (SHED_SPARKLES + 1) as f32;
            let age = tf - ride_at(f);
            if !(0.0..SHED_MS as f32).contains(&age) {
                continue;
            }
            let tau = age / SHED_MS as f32;
            let ratio = STAR_ARM_FINE * (1.0 - 0.4 * tau) * tax;
            let arm = i64::from(star_arm_px(star_arm(chf, ratio)));
            let m = arm + 1;
            let x = from + (dist_f * f).round() as i64 - m;
            let y = y0 + (span_y * (0.15 + 0.7 * unit(self.seed, 0x5348_4544 ^ k as u64))) as i64;
            let (lo, hi) = (
                (region.x0 + m, region.y0 + m),
                (region.x1 - m, region.y1 - m),
            );
            if hi.0 <= lo.0 || hi.1 <= lo.1 {
                return;
            }
            let mut centre = (x.clamp(lo.0, hi.0 - 1), y.clamp(lo.1, hi.1 - 1));
            if let Some(b) = band.filter(|b| b.x1 - b.x0 > 2 * m && b.y1 - b.y0 > 2 * m) {
                centre = (
                    centre.0.clamp(b.x0 + m, b.x1 - m - 1),
                    centre.1.clamp(b.y0 + m, b.y1 - m - 1),
                );
            }
            lay(StarCall {
                clip: region,
                centre,
                arm,
                cov: SHED_COV * (1.0 - tau).powf(1.5),
                gold: false,
                color: self.mark_ink(k, self.scene.palette.mark(k)),
                light: dark,
            });
        }
    }

    fn burst_stars(&self, lay: &mut impl FnMut(StarCall)) {
        let Some(ring) = self.burst_ring() else {
            return;
        };
        for (k, &centre) in ring.centres.iter().enumerate() {
            lay(StarCall {
                clip: ring.region,
                centre,
                arm: ring.arm,
                cov: ring.cov,
                gold: true,
                color: self.mark_ink(k, self.scene.palette.mark(k)),
                light: self.scene.dark,
            });
        }
    }

    /// Every star of the frame, a dark scene's each laid on its ground ([`StarCall::laid_on`]),
    /// the glints by their rest light ([`Self::laid_glints`]).
    fn each_star(&self, lay: &mut impl FnMut(StarCall)) {
        let ground = self.scene.ground;
        self.burst_stars(&mut |s: StarCall| lay(s.laid_on(ground)));
        self.laid_glints(lay);
        let lay = &mut |s: StarCall| lay(s.laid_on(ground));
        self.field(lay);
        self.shed(lay);
    }

    /// The glints, each laid with the polarity its rest light takes on the ground
    /// ([`StarCall::laid_as`]), so a flare toward white never flips one between ink and light.
    fn laid_glints(&self, lay: &mut impl FnMut(StarCall)) {
        let ground = self.scene.ground;
        let rest = dark_mark(self.scene.palette.glint_rest(), ground);
        self.glints(&mut |s: StarCall| lay(s.laid_as(rest, ground)));
    }

    fn burst_ring(&self) -> Option<BurstRing> {
        let age = match self.clock.ride_ms {
            Some(r) if self.rides() => r.checked_sub(RIDE_MS),
            _ => self.clock.t_ms,
        };
        let t = age.filter(|&t| t < BURST_MS)?;
        let region = star_region(&self.geo, self.scene)?;
        let g = self.geo;
        let ((cx, mid), half_w) = match self.seat {
            Some(at) => {
                let r = at.rect;
                (((r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2), (r.x1 - r.x0) / 2)
            }
            None => {
                let (_, end) = self.scene.trail?;
                ((g.col(end).min(region.x1), g.band(g.top + 1)), 0)
            }
        };
        let tau = t as f32 / BURST_MS as f32;
        let fly = 1.0 - (1.0 - tau).powi(3);
        let rx = (half_w as f32 + 2.0 * g.cw as f32).max(3.0 * g.cw as f32);
        let ry = 0.85 * g.ch as f32;
        let reach = 0.35 + 0.65 * fly;
        let ratio = STAR_ARM_HERO + (STAR_ARM_FINE - STAR_ARM_HERO) * tau;
        let ratio = ratio * if self.scene.dark { 1.0 } else { STAR_ARM_INK };
        let turn = unit(self.seed, 0x4255_5253) * std::f32::consts::TAU;
        Some(BurstRing {
            region,
            centres: std::array::from_fn(|k| {
                let theta = turn + std::f32::consts::TAU * k as f32 / BURST_STARS as f32;
                (
                    cx + (theta.cos() * rx * reach).round() as i64,
                    mid + (theta.sin() * ry * reach).round() as i64,
                )
            }),
            arm: i64::from(star_arm_px(star_arm(g.ch as f32, ratio))),
            cov: BURST_COV * (1.0 - tau).powf(1.2),
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
    /// The star laid on `ground`: a dark scene's light as [`dark_mark`] lays it — kept where it
    /// reads, lifted along its hue where white is the polarity that reads best and the lift
    /// reads, else floored and printed — and a printed star as it is.
    #[inline]
    fn laid_on(self, ground: u32) -> Self {
        if !self.light {
            return self;
        }
        match dark_mark(self.color, ground) {
            DarkMark::Light(color) => Self { color, ..self },
            DarkMark::Print(color) => Self {
                color,
                light: false,
                ..self
            },
        }
    }

    /// A glint laid on `ground` by `rest`, its rest light's [`dark_mark`] there: where that
    /// prints, the rest's floored ink printed through the whole flare, as a light scene's glint
    /// keeps its ink and flares in shape alone; else its flared light as [`Self::laid_on`] lays
    /// it, which stays light (white reads wherever the rest light, or its lift, does).
    #[inline]
    fn laid_as(self, rest: DarkMark, ground: u32) -> Self {
        match rest {
            DarkMark::Print(color) if self.light => Self {
                color,
                light: false,
                ..self
            },
            DarkMark::Light(_) | DarkMark::Print(_) => self.laid_on(ground),
        }
    }
}

fn rim_strips(frame: &[&str], p: i64, rim: i64, mut strip: impl FnMut(Rect)) {
    let (h, w) = (frame.len() as i64, KITTY_W as i64);
    let solid = |r: i64, c: i64| rim_solid(frame, r, c);
    for b in 0..=h {
        for (art_row, open_row, y0) in [(b, b - 1, b * p - rim), (b - 1, b, b * p)] {
            let edge = |c: i64| art_at(frame, art_row, c) && !solid(open_row, c);
            let mut c = 0;
            while c < w {
                if !edge(c) {
                    c += 1;
                    continue;
                }
                let c0 = c;
                while c < w && edge(c) {
                    c += 1;
                }
                let outer = |e: i64| !solid(art_row, e) && !solid(open_row, e);
                strip(Rect {
                    x0: c0 * p - if outer(c0 - 1) { rim } else { 0 },
                    y0,
                    x1: c * p + if outer(c) { rim } else { 0 },
                    y1: y0 + rim,
                });
            }
        }
    }
    for v in 0..=w {
        for (art_col, open_col, x0) in [(v, v - 1, v * p - rim), (v - 1, v, v * p)] {
            let edge = |r: i64| solid(r, art_col) && !solid(r, open_col);
            let mut r = 0;
            while r < h {
                if !edge(r) {
                    r += 1;
                    continue;
                }
                let top = r;
                while r < h && edge(r) {
                    r += 1;
                }
                let q = Rect {
                    x0,
                    y0: top * p + if solid(top - 1, open_col) { rim } else { 0 },
                    x1: x0 + rim,
                    y1: r * p - if solid(r, open_col) { rim } else { 0 },
                };
                if q.y1 > q.y0 {
                    strip(q);
                }
            }
        }
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
}

impl TrailInk {
    #[inline]
    fn covers(&self) -> &[Rect] {
        &self.covers[..self.n]
    }
}

#[cfg(test)]
mod tests {
    use super::oracle::{QuadSink, Sink, kitty_quads, lay_star, rim_quads};
    use super::*;
    use crate::cursor_glow::Geom;
    use crate::effect_util::{STAR_GLINT, push_twinkle_star, star_core_px, star_waist_px};
    use aterm_render::GlowQuad;

    pub(super) fn scene() -> CardScene {
        CardScene {
            origin_x: 10,
            origin_y: 46,
            cw: 9,
            ch: 18,
            row_base: 2,
            pad_left: 10,
            pad_above: 0,
            pad_below: 0,
            top: 3,
            rows: 4,
            cols: 60,
            trail: Some((32, 40)),
            kitty_col: Some(40),
            kitty_rows: 2,
            clip: WHOLE_STREAM,
            palette: CardPalette::Cruise,
            dark: true,
            ground: NIGHT,
            serious: false,
            coat: 0x00E8_A060,
            eye: 0x0040_C080,
            wand: Some((10, 3)),
        }
    }

    const NIGHT: u32 = 0x0012_1721;
    const DAY: u32 = 0x00F8_FAFD;
    /// Silver Aerogel's ground, a mid grey [`dark_ground`] calls dark.
    pub(super) const AEROGEL: u32 = 0x0092_9292;
    /// Unikitty's ground, a mid pink [`dark_ground`] calls dark, on which no added light reads.
    pub(super) const UNIKITTY: u32 = 0x00FF_8CD9;
    /// A raspberry mid ground on which the glints' rest gold misses the floor added though white
    /// light reads, so a glint laid frame by frame on its flare would flip from ink to light.
    const RASPBERRY: u32 = 0x00FF_375F;
    /// A dim scarlet on which white reads better than black yet no added light reads the floor.
    const SCARLET: u32 = 0x00EB_0000;

    fn contrast(a: u32, b: u32) -> f32 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    const WHOLE_STREAM: (i32, i32, i32, i32) = (i32::MIN, i32::MIN, i32::MAX, i32::MAX);

    pub(super) fn widest() -> CardScene {
        CardScene {
            cols: 120,
            trail: Some((8, 100)),
            kitty_col: Some(100),
            ..scene()
        }
    }

    pub(super) fn clipped() -> CardScene {
        let s = scene();
        let (cw, ch) = (i32::from(s.cw), i32::from(s.ch));
        CardScene {
            clip: (
                s.origin_x,
                0,
                s.origin_x + 50 * cw,
                s.origin_y + i32::from(s.top + 3) * ch + ch / 2,
            ),
            ..s
        }
    }

    pub(super) fn scenes() -> Vec<CardScene> {
        let two = scenes_two_rows();
        let tall = two.iter().map(|s| CardScene {
            kitty_rows: 3,
            ..*s
        });
        two.iter().copied().chain(tall).collect()
    }

    fn scenes_two_rows() -> Vec<CardScene> {
        let base = scene();
        let mut v = vec![base];
        v.push(CardScene {
            dark: false,
            ..base
        });
        v.push(CardScene {
            palette: CardPalette::Land,
            ..base
        });
        v.push(CardScene {
            palette: CardPalette::Land,
            dark: false,
            ..base
        });
        v.push(CardScene {
            palette: CardPalette::Off,
            ..base
        });
        v.push(CardScene {
            palette: CardPalette::Off,
            dark: false,
            ..base
        });
        v.push(CardScene {
            cw: 8,
            ch: 16,
            pad_left: 0,
            pad_above: 16,
            origin_x: 0,
            origin_y: 0,
            row_base: 0,
            top: 0,
            wand: Some((10, 0)),
            ..base
        });
        v.push(CardScene {
            cw: 16,
            ch: 33,
            kitty_col: Some(44),
            trail: Some((32, 44)),
            ..base
        });
        v.push(CardScene {
            origin_x: -3,
            pad_left: 2,
            rows: 3,
            ..base
        });
        v.push(CardScene {
            cw: 7,
            ch: 13,
            rows: 3,
            dark: false,
            trail: None,
            ..base
        });
        v.push(CardScene {
            cw: 17,
            ch: 35,
            kitty_col: Some(44),
            trail: Some((32, 44)),
            ..base
        });
        v.push(clipped());
        v.push(CardScene {
            dark: false,
            palette: CardPalette::Land,
            ..clipped()
        });
        v.push(aired(base));
        v.push(aired(CardScene {
            dark: false,
            palette: CardPalette::Land,
            ..base
        }));
        v.push(aired(CardScene {
            dark: false,
            palette: CardPalette::Off,
            ..base
        }));
        v.push(CardScene {
            cw: 16,
            ch: 33,
            pad_above: 33,
            rows: 3,
            kitty_col: Some(44),
            trail: Some((32, 44)),
            ..base
        });
        v
    }

    pub(super) fn aired(s: CardScene) -> CardScene {
        CardScene {
            pad_above: s.ch,
            pad_below: s.ch,
            ..s
        }
    }

    fn frame(
        card: &mut HarnessCard,
        now: Instant,
        s: &CardScene,
        animate: bool,
    ) -> (Vec<GlowQuad>, Vec<GlowQuad>, u64) {
        let (mut under, mut over) = (Vec::new(), Vec::new());
        let fp = card.tick(now, s, animate, &mut under, &mut over);
        (under, over, fp)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn kitty_left(over: &[GlowQuad]) -> Option<i64> {
        over.iter()
            .filter(|q| q.alpha == 255)
            .map(|q| i64::from(q.x))
            .min()
    }

    fn ride_from(s: &CardScene) -> i64 {
        let g = Geo::new(s).expect("a scene");
        let seat = kitty_rect(&g, s).expect("a kitty");
        ride_span(&g, s, &seat).expect("a ride").0
    }

    fn shown_at_rest(s: &CardScene, at: Instant) -> HarnessCard {
        let mut card = HarnessCard::default();
        frame(&mut card, at, s, false);
        card
    }

    fn settled(s: &CardScene) -> (Vec<GlowQuad>, Vec<GlowQuad>, u64) {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0 + ms(EPISODE_MS + 1), s, false)
    }

    #[derive(Debug, Default)]
    struct Parts {
        border: Vec<GlowQuad>,
        trail: Vec<GlowQuad>,
        kitty: Vec<GlowQuad>,
        rim: Vec<GlowQuad>,
        stars: Vec<GlowQuad>,
        sparkles: Vec<GlowQuad>,
        burst: Vec<GlowQuad>,
        glints: Vec<GlowQuad>,
    }

    fn parts(s: &CardScene, clock: Clock) -> Parts {
        parts_seeded(s, clock, 7)
    }

    fn lay_calls(cx: &Emit<'_>, out: &mut Vec<GlowQuad>) -> impl FnMut(StarCall) {
        let sink = Sink { geo: cx.geo };
        let trail = cx.trail_ink();
        let ground = cx.scene.ground;
        move |st: StarCall| {
            let st = st.laid_on(ground);
            lay_star(
                out,
                &sink,
                st.clip,
                st.centre,
                st.arm,
                st.cov,
                st.gold,
                st.color,
                trail.covers(),
                st.light,
            );
        }
    }

    fn parts_seeded(s: &CardScene, clock: Clock, seed: u64) -> Parts {
        let geo = Geo::new(s).expect("a scene");
        let cx = Emit::new(s, geo, clock, seed);
        let mut out = Parts::default();
        let sink = Sink { geo };
        cx.border(&mut QuadSink {
            sink,
            out: &mut out.border,
        });
        cx.trail(&mut QuadSink {
            sink,
            out: &mut out.trail,
        });
        kitty_quads(&cx, &sink, &mut out.kitty);
        rim_quads(&cx, &sink, &mut out.rim);
        cx.field(&mut lay_calls(&cx, &mut out.stars));
        cx.shed(&mut lay_calls(&cx, &mut out.sparkles));
        cx.burst_stars(&mut lay_calls(&cx, &mut out.burst));
        cx.laid_glints(&mut lay_calls(&cx, &mut out.glints));
        out
    }

    fn assert_in_band_and_box(q: &GlowQuad, s: &CardScene, what: &str) {
        let (x0, y0) = (i64::from(q.x), i64::from(q.y));
        let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
        assert!(q.w > 0 && q.h > 0, "{what}: an empty quad {q:?}");
        let r = i64::from(q.row) - i64::from(s.row_base);
        let band0 = i64::from(s.origin_y) + r * i64::from(s.ch);
        assert!(
            band0 <= y0 && y1 <= band0 + i64::from(s.ch),
            "{what}: {q:?} leaves its band [{band0}, {})",
            band0 + i64::from(s.ch)
        );
        let cw = i64::from(s.cw);
        let bx0 = (i64::from(s.origin_x) - i64::from(s.pad_left).min(cw / 2)).max(0);
        let bx1 = i64::from(s.origin_x) + (i64::from(s.cols) + 1) * cw;
        let ch = i64::from(s.ch);
        let air = |pad: u16| i64::from(pad).min(ch / 2);
        let top = i64::from(s.origin_y) + i64::from(s.top) * ch;
        let by0 = top - if s.top > 0 { air(s.pad_above) } else { 0 };
        let by1 = top + i64::from(s.rows) * ch + air(s.pad_below);
        assert!(
            bx0 <= x0 && x1 <= bx1 && by0 <= y0 && y1 <= by1,
            "{what}: {q:?} leaves the box x {bx0}..{bx1} y {by0}..{by1}"
        );
        let (cx0, cy0, cx1, cy1) = s.clip;
        assert!(
            i64::from(cx0) <= x0
                && x1 <= i64::from(cx1)
                && i64::from(cy0) <= y0
                && y1 <= i64::from(cy1),
            "{what}: {q:?} leaves the clip {:?}",
            s.clip
        );
    }

    #[test]
    fn kitty_frames_are_rectangular_and_drawn_in_the_palette() {
        for (art, h) in [(KITTY_SHORT, KITTY_H), (KITTY_TALL, KITTY_TALL_H)] {
            for f in [art.open, art.blink, art.flick] {
                assert_eq!(f.len(), h);
                for row in f.iter() {
                    assert_eq!(row.len(), KITTY_W, "a ragged row {row:?}");
                    assert!(
                        row.bytes().all(|b| KITTY_PALETTE.contains(&b)),
                        "an unknown pixel in {row:?}"
                    );
                }
            }
            for (r, (open, blink)) in art.open.iter().zip(art.blink.iter()).enumerate() {
                assert_eq!(
                    open == blink,
                    !(art.eyes.0..art.eyes.1).contains(&r),
                    "the blink is the eyes alone (row {r})"
                );
                if open != blink {
                    assert!(
                        !blink.contains(['e', 'k', 'w']),
                        "the eyes closed (row {r})"
                    );
                }
            }
            for (open, flick) in art.open.iter().zip(art.flick.iter()) {
                assert_eq!(
                    open[KITTY_TAIL_COLS..],
                    flick[KITTY_TAIL_COLS..],
                    "the flick is the tail alone"
                );
            }
            assert!(
                two_eyes_of_iris_and_pupil(art.open, art.eyes),
                "the cat has two eyes, each an iris and a pupil"
            );
            for (what, pupil_row, catch_row) in [
                ("a right eye without its pupil", "ekccccee", "ewccccew"),
                ("one eye", "ekcccccc", "ewcccccc"),
                ("the eyes run together", "ekkkkkek", "ewkkkkew"),
            ] {
                let rows: Vec<String> = art
                    .open
                    .iter()
                    .map(|r| {
                        r.replace("ekccccek", pupil_row)
                            .replace("ewccccew", catch_row)
                    })
                    .collect();
                let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
                assert_ne!(rows, art.open, "CONTROL: {what} is drawn");
                assert!(
                    !two_eyes_of_iris_and_pupil(&rows, art.eyes),
                    "CONTROL: {what} passes the eye check"
                );
            }
            let rear = art.rear();
            for f in [art.open, art.blink, art.flick] {
                for row in &f[art.body.0..art.body.0 + art.body.1] {
                    let first = row.find(|c| c != '.').expect("a body row");
                    assert!(
                        first <= rear && !row[first..rear].contains('.'),
                        "{row:?} meets the trail flush at column {rear}"
                    );
                }
            }
        }
        let cols_of = |f: &[&str], ch: char| -> std::collections::BTreeSet<usize> {
            f.iter()
                .flat_map(|r| {
                    r.char_indices()
                        .filter(move |&(_, c)| c == ch)
                        .map(|(i, _)| i)
                })
                .collect()
        };
        for ch in ['e', 'k', 'n'] {
            assert_eq!(
                cols_of(KITTY_SHORT.open, ch),
                cols_of(KITTY_TALL.open, ch),
                "{ch:?} on the same columns"
            );
        }
        assert_eq!(KITTY_SHORT.open[0], KITTY_TALL.open[0], "the ears' tips");
        let (short, tall) = (KITTY_SHORT.open, KITTY_TALL.open);
        assert_eq!(
            [short[11], short[12], short[12], short[13]],
            [tall[17], tall[18], tall[19], tall[20]],
            "the legs, a row longer, and the paws"
        );
        let mut stray: Vec<&str> = KITTY_TALL.blink.to_vec();
        stray[0] = "..............o......oo.";
        assert!(!differ_only_in(
            KITTY_TALL.open,
            &stray,
            KITTY_TALL.eyes,
            (0, KITTY_W)
        ));
        let mut wag: Vec<&str> = KITTY_TALL.flick.to_vec();
        wag[1] = "....o........ono....ono.";
        assert!(!differ_only_in(
            KITTY_TALL.open,
            &wag,
            (0, KITTY_TALL_H),
            (0, KITTY_TAIL_COLS)
        ));
        let mut ragged: Vec<&str> = KITTY_TALL.open.to_vec();
        ragged[3] = "oco.........onnoooooonn";
        assert!(!frame_is_well_formed(&ragged, KITTY_TALL_H));
        let rear = KITTY_TALL.rear();
        assert!(trail_meets_flush(KITTY_TALL.open, KITTY_TALL.body, rear));
        let mut holed: Vec<&str> = KITTY_TALL.open.to_vec();
        holed[KITTY_TALL.body.0 + 1] = "o.occcccccccocewccccewco";
        assert!(!trail_meets_flush(&holed, KITTY_TALL.body, rear));
        let open_rear = |art: KittyArt| {
            art.open[art.body.0..art.body.0 + art.body.1]
                .iter()
                .map(|r| r.find(|c| c != '.').expect("a body row"))
                .max()
                .expect("body rows")
        };
        for art in [KITTY_SHORT, KITTY_TALL] {
            assert!(trail_meets_flush(art.open, art.body, open_rear(art)));
            assert!(
                !trail_meets_flush(art.flick, art.body, open_rear(art)),
                "CONTROL: the flick's back past the open frame's rear"
            );
        }
        assert!(frame_is_well_formed(KITTY_TALL.open, KITTY_TALL_H));
        assert!(
            !frame_is_well_formed(KITTY_TALL.open, KITTY_H),
            "the wrong height"
        );
    }

    #[test]
    fn kitty_cols_rounds_the_art_up_to_whole_cells() {
        assert_eq!(kitty_cols(8, 16), 6, "2 px pixels: 48 px in 8 px cells");
        assert_eq!(kitty_cols(9, 18), 6, "2 px pixels: 48 px in 9 px cells");
        assert_eq!(kitty_cols(16, 33), 6, "4 px pixels: 96 px in 16 px cells");
        assert_eq!(kitty_cols(7, 21), 7, "2 px pixels: 48 px in 7 px cells");
        assert_eq!(kitty_cols(17, 35), 6, "4 px pixels: 96 px in 17 px cells");
        assert_eq!(kitty_cols(8, 14), 3, "1 px pixels: 24 px in 8 px cells");
        assert_eq!(kitty_cols(8, 8), 3, "7 pixel rows under a 1 px stroke");
        assert_eq!(
            kitty_cols(8, 7),
            0,
            "7 px less the stroke cannot hold 7 rows"
        );
        assert_eq!(kitty_cols(8, 6), 0, "a 6 px cell cannot hold 7 pixel rows");
        assert_eq!(kitty_cols(0, 16), 0);
        assert_eq!(kitty_cols(8, 0), 0);
    }

    #[test]
    fn the_kitty_clears_the_border_at_every_cell_height() {
        let mut drawn = [0; 2];
        for (k, kitty_rows) in [2u8, 3].into_iter().enumerate() {
            for ch in 1..=96u16 {
                let cw = (ch / 2).max(1);
                let cols = kitty_cols(cw, ch);
                let s = CardScene {
                    cw,
                    ch,
                    origin_x: 4,
                    origin_y: 4,
                    top: 0,
                    row_base: 0,
                    rows: 3,
                    trail: Some((32, 40)),
                    kitty_col: Some(40),
                    kitty_rows,
                    cols: 40 + cols + 1,
                    ..scene()
                };
                let g = Geo::new(&s).expect("a scene");
                let Some(at) = kitty_rect(&g, &s) else {
                    assert_eq!(
                        cols, 0,
                        "{ch} px, {kitty_rows} rows: the kitty fits its columns but was dropped"
                    );
                    continue;
                };
                let (r, p) = (at.rect, at.p);
                let inner = g.inner();
                assert_eq!(
                    r.meet(inner),
                    r,
                    "{ch} px: the kitty {r:?} meets the stroke {inner:?}"
                );
                assert_eq!(at.art.rows(), usize::from(kitty_rows));
                assert!(
                    r.y0 >= g.band(0) && r.y1 <= g.band(i64::from(kitty_rows)),
                    "{ch} px: {r:?}"
                );
                if kitty_rows == 2 {
                    assert_eq!(
                        r.y0 + KITTY_HALF as i64 * p,
                        g.band(1),
                        "{ch} px: split at the boundary"
                    );
                }
                assert!(
                    r.x1 - r.x0 <= i64::from(cols) * i64::from(cw),
                    "{ch} px: wider than its {cols} columns"
                );
                assert_eq!(p, i64::from(kitty_px(ch)));
                let settled = parts(&s, Clock::SETTLED);
                for q in settled.kitty.iter().chain(&settled.rim) {
                    let (x0, y0) = (i64::from(q.x), i64::from(q.y));
                    let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
                    assert!(
                        inner.x0 <= x0 && x1 <= inner.x1 && inner.y0 <= y0 && y1 <= inner.y1,
                        "{ch} px: a kitty or rim pixel {q:?} on the stroke"
                    );
                }
                drawn[k] += 1;
            }
        }
        assert!(
            drawn.iter().all(|&n| n >= 80),
            "the sweep must draw the kitty: {drawn:?}"
        );
    }

    fn played(s: &CardScene, t0: Instant) -> HarnessCard {
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, s, true);
        frame(&mut card, t0 + ms(EPISODE_MS - 5), s, true);
        card
    }

    #[test]
    fn a_held_card_is_static_and_wants_no_frames() {
        for s in scenes() {
            let t0 = Instant::now();
            let rest = settled(&s);
            assert_ne!(rest.2, 0, "a settled card still draws");
            let mut card = played(&s, t0);
            frame(&mut card, t0 + ms(EPISODE_MS + 500), &s, true);
            assert!(!card.at_rest(), "CONTROL: it flows");
            for k in 1..6 {
                let now = t0 + ms(EPISODE_MS + 500 + k * 977);
                assert_eq!(frame(&mut card, now, &s, false), rest, "held {k}");
                assert!(card.at_rest(), "held {k}: at rest");
                assert_eq!(card.deadline(now), None, "a held card wants no frames");
            }
            let mut unarmed = HarnessCard::default();
            let first = frame(&mut unarmed, t0, &s, true);
            for k in 1..4 {
                let now = t0 + ms(k * 977);
                assert_eq!(frame(&mut unarmed, now, &s, true), first, "unarmed {k}");
                assert_eq!(unarmed.deadline(now), None, "an unarmed card idles");
            }
        }
        assert_eq!(
            HarnessCard::default().deadline(Instant::now()),
            None,
            "an unarmed card idles"
        );
    }

    #[test]
    fn after_the_intro_the_card_flows_on_the_step_grid() {
        for s in scenes() {
            let t0 = Instant::now();
            let mut card = played(&s, t0);
            let end = t0 + ms(EPISODE_MS);
            assert_eq!(
                card.deadline(end),
                Some(end),
                "the flow's first frame is owed"
            );
            let first = EPISODE_MS / FLOW_STEP_MS + 1;
            let mut prev: Option<(Vec<GlowQuad>, Vec<GlowQuad>, u64)> = None;
            for k in first..first + 2 * GLINT_PERIOD_MS / FLOW_STEP_MS {
                let at = t0 + ms(k * FLOW_STEP_MS + 3);
                let f = frame(&mut card, at, &s, true);
                assert!(card.flowing(at) && !card.intro_live(), "step {k}: flowing");
                assert_eq!(card.flow_step(at), Some(k), "step {k}: the grid");
                let due = t0 + ms((k + 1) * FLOW_STEP_MS + FLOW_SLACK_MS);
                assert_eq!(card.deadline(at), Some(due), "step {k}: the next step");
                assert_eq!(card.deadline(at + ms(90)), Some(due), "step {k}: fixed");
                let again = frame(&mut card, at + ms(FLOW_STEP_MS - 10), &s, true);
                assert_eq!(again, f, "step {k}: a step's frame is the step's alone");
                if let Some(p) = &prev {
                    assert_ne!(p.2, f.2, "step {k}: the fingerprint held");
                    assert!(p.0 != f.0 || p.1 != f.1, "step {k}: the quads held");
                }
                prev = Some(f);
            }
        }
    }

    #[test]
    fn the_flow_settles_when_a_gate_closes_and_resumes_from_the_settled_colours() {
        for s in scenes() {
            let t0 = Instant::now();
            let rest = settled(&s);
            let border = parts(&s, Clock::SETTLED).border.len();
            let mut card = played(&s, t0);
            frame(&mut card, t0 + ms(EPISODE_MS + 200), &s, true);
            let shut = t0 + ms(EPISODE_MS + 300);
            assert_eq!(frame(&mut card, shut, &s, false), rest, "settled at once");
            assert_eq!(card.deadline(shut), None, "held: no wake");
            let back = t0 + ms(EPISODE_MS + 5300);
            let (under, over, fp) = frame(&mut card, back, &s, true);
            assert!(card.flowing(back), "it flows again");
            assert_ne!(fp, rest.2, "a flow frame");
            assert_eq!(
                under[..border],
                rest.0[..border],
                "the border resumes settled"
            );
            assert_eq!(
                over, rest.1,
                "the kitty, its rim and the stars resume settled"
            );
            let up = back + ms(GLINT_FADE_MS + 2 * FLOW_STEP_MS);
            let (later_under, _, _) = frame(&mut card, up, &s, true);
            assert_ne!(
                later_under[..border],
                rest.0[..border],
                "the border flows on"
            );
        }
    }

    #[test]
    fn an_adopted_ripple_flows_on_the_first_cards_steps() {
        let s = scene();
        let t0 = Instant::now();
        let mut first = HarnessCard::default();
        first.arm(t0, 7);
        let origin = first.ripple_origin().expect("armed");
        assert_eq!(origin, t0);
        let t1 = t0 + ms(50);
        let mut second = HarnessCard::default();
        second.adopt_ripple(origin);
        second.arm(t1, 9);
        assert_eq!(second.ripple_origin(), Some(origin), "the arm keeps it");
        second.adopt_ripple(t1);
        assert_eq!(
            second.ripple_origin(),
            Some(origin),
            "a started clock stays"
        );
        let mut alone = HarnessCard::default();
        alone.arm(t1, 9);
        let flow = t1 + ms(EPISODE_MS + 1000);
        for card in [&mut first, &mut second, &mut alone] {
            frame(card, t1, &s, true);
            frame(card, flow, &s, true);
            assert!(card.flowing(flow), "CONTROL: it flows");
        }
        let (a, b, c) = (
            first.deadline(flow).expect("a step"),
            second.deadline(flow).expect("a step"),
            alone.deadline(flow).expect("a step"),
        );
        assert_eq!(a, b, "one step for both");
        assert_eq!(first.flow_step(flow), second.flow_step(flow));
        let off = c.max(a).duration_since(c.min(a)).as_millis() % u128::from(FLOW_STEP_MS);
        assert_eq!(
            off.min(u128::from(FLOW_STEP_MS) - off),
            50,
            "CONTROL: 50 ms off"
        );
    }

    #[test]
    fn a_lock_is_whole_while_unseen_and_slews_a_step_at_most_once_seen() {
        let s = scene();
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, &s, true);
        let seen = t0 + ms(1000);
        assert!(!card.glints_shown(seen), "CONTROL: no glint shows yet");
        card.lock_glint(seen);
        let g0 = t0 + ms(1000 - FLARE_STEP_LEAD_MS);
        assert_eq!(card.glint0, Some(g0), "adopted, unseen");
        frame(&mut card, t0 + ms(EPISODE_MS - 5), &s, true);
        let mut kept = CardFrames::default();
        let at = t0 + ms(EPISODE_MS + 200);
        let _ = card.draw(at, &s, true, &mut kept);
        assert!(card.glints_shown(at), "CONTROL: the flow's glints show");
        assert!(
            card.replay_step(&s, true, &kept, at + ms(1)).is_some(),
            "CONTROL: kept"
        );
        let flare = |birth_after_g0: u64, off: i64| {
            let birth = later(g0, birth_after_g0);
            let start = if off >= 0 {
                later(birth, off.unsigned_abs())
            } else {
                birth - ms(off.unsigned_abs())
            };
            later(start, FLARE_STEP_LEAD_MS)
        };
        let grid = card.grid_gen;
        card.lock_glint(flare(3 * GLINT_PERIOD_MS, 3));
        assert_eq!(
            (card.glint0, card.grid_gen),
            (Some(g0), grid),
            "a 3 ms error is noise"
        );
        assert!(
            card.replay_step(&s, true, &kept, at + ms(1)).is_some(),
            "still kept"
        );
        card.lock_glint(flare(3 * GLINT_PERIOD_MS, 40));
        assert_eq!(card.glint0, Some(g0 + ms(10)), "slewed by +10");
        assert_eq!(card.fade0, None, "the glints stay up");
        assert!(
            !card.replay_step(&s, true, &kept, at + ms(1)).is_some(),
            "a frame of the old grid is drawn anew"
        );
        let g1 = g0 + ms(10);
        card.lock_glint(later(
            later(g1, 10 * GLINT_PERIOD_MS) - ms(30),
            FLARE_STEP_LEAD_MS,
        ));
        assert_eq!(card.glint0, Some(g1 - ms(7)), "slewed by −7");
        let g2 = g1 - ms(7);
        card.lock_glint(later(g2, 11 * GLINT_PERIOD_MS + 3040 + FLARE_STEP_LEAD_MS));
        let g3 = g2 - ms(8);
        assert_eq!(card.glint0, Some(g3), "folded, slewed by −8");
        card.lock_glint(later(g3, 13 * GLINT_PERIOD_MS + 1000 + FLARE_STEP_LEAD_MS));
        let g4 = later(g3, FLOW_STEP_MS);
        assert_eq!(card.glint0, Some(g4), "a step toward it");
        card.lock_glint(later(
            later(g4, 15 * GLINT_PERIOD_MS) - ms(1000),
            FLARE_STEP_LEAD_MS,
        ));
        assert_eq!(card.glint0, Some(g4 - ms(FLOW_STEP_MS)), "a step back");
        assert_eq!(card.fade0, None, "no lock fades the glints");
        let mut fresh = HarnessCard::default();
        let first = t0 + ms(20 * GLINT_PERIOD_MS);
        fresh.lock_glint(first);
        fresh.arm(first + ms(500), 7);
        assert_eq!(
            fresh.glint0,
            Some(first - ms(FLARE_STEP_LEAD_MS)),
            "the first arm keeps the flare's grid"
        );
    }

    #[test]
    fn the_lock_averages_claudes_repaint_jitter_out() {
        let s = scene();
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, &s, true);
        let claude0 = t0 + ms(500);
        let late = |k: u64| {
            if k % 8 == 5 {
                250
            } else {
                (k * 53) % (crate::ripple::CLAUDE_REPAINT_P90_MS + 1)
            }
        };
        let seen = |k: u64| later(claude0, k * GLINT_PERIOD_MS + late(k));
        let p = GLINT_PERIOD_MS as i64;
        let fold = |e: i64| {
            let e = e.rem_euclid(p);
            if e > p / 2 { e - p } else { e }
        };
        let offset = |card: &HarnessCard| {
            let g0 = card.glint0.expect("a grid");
            fold(
                i64::try_from(ms_between(claude0, g0)).unwrap()
                    - i64::try_from(ms_between(g0, claude0)).unwrap(),
            )
        };
        let lead = i64::try_from(FLARE_STEP_LEAD_MS).unwrap();
        for k in 0..2 {
            assert!(
                !card.glints_shown(seen(k)),
                "flare {k}: in the intro, unseen"
            );
            card.lock_glint(seen(k));
            assert_eq!(
                offset(&card),
                i64::try_from(late(k)).unwrap() - lead,
                "flare {k}: taken whole"
            );
        }
        frame(&mut card, t0 + ms(EPISODE_MS - 5), &s, true);
        frame(&mut card, t0 + ms(EPISODE_MS + 200), &s, true);
        let (mut worst, mut max_move) = (0i64, 0i64);
        let mean = (2..302)
            .map(|k| i64::try_from(late(k)).unwrap())
            .sum::<i64>()
            / 300;
        for k in 2..302 {
            let before = offset(&card);
            assert!(card.glints_shown(seen(k)), "flare {k}: the glints show");
            card.lock_glint(seen(k));
            let moved = (offset(&card) - before).abs();
            let bound = if late(k) == 250 { 64 } else { 40 };
            assert!(moved <= bound, "flare {k}: moved {moved} ms");
            max_move = max_move.max(moved);
            assert_eq!(card.fade0, None, "flare {k}: no fade");
            if k >= 12 {
                worst = worst.max((offset(&card) - (mean - lead)).abs());
            }
        }
        let half_cell = (500.0 / crate::ripple::GLINT_CELLS_PER_S) as i64;
        assert!(worst <= half_cell, "locked within {worst} ms");
        assert!(max_move > 0, "CONTROL: the lock does move the grid");
        let (mut off, mut refades, mut chased) = (0i64, 0, 0i64);
        for k in 2..302 {
            let e = fold(i64::try_from(late(k)).unwrap() - off);
            if e.abs() <= 64 {
                chased = chased.max(e.abs());
                off += e;
            } else {
                refades += 1;
                off = i64::try_from(late(k)).unwrap();
            }
        }
        assert!(
            refades > 30 && chased > 40,
            "CONTROL: the old rule re-fades ({refades}) and chases the jitter ({chased} ms)"
        );
    }

    #[test]
    fn a_step_lock_folds_into_half_a_step() {
        let s = scene();
        let mut fresh = HarnessCard::default();
        fresh.lock_step(Instant::now());
        assert_eq!(fresh.glint0, None, "CONTROL: no grid, none made");
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, &s, true);
        let g0 = card.glint0.expect("armed");
        let seen = later(g0, 5 * FLOW_STEP_MS + 50 + FLARE_STEP_LEAD_MS);
        assert!(!card.glints_shown(seen), "CONTROL: no glint shows yet");
        card.lock_step(seen);
        let g1 = later(g0, 50);
        assert_eq!(card.glint0, Some(g1), "taken whole, unseen");
        frame(&mut card, t0 + ms(EPISODE_MS - 5), &s, true);
        let mut kept = CardFrames::default();
        let at = t0 + ms(EPISODE_MS + 200);
        let _ = card.draw(at, &s, true, &mut kept);
        assert!(card.glints_shown(at), "CONTROL: the flow's glints show");
        let seen_at = |g: Instant, step: u64, off: u64| {
            later(g, step * FLOW_STEP_MS + off + FLARE_STEP_LEAD_MS)
        };
        card.lock_step(seen_at(g1, 60, 3));
        assert_eq!(card.glint0, Some(g1), "a 3 ms error is noise");
        assert!(
            card.replay_step(&s, true, &kept, at + ms(1)).is_some(),
            "still kept"
        );
        card.lock_step(seen_at(g1, 61, 40));
        let g2 = later(g1, 10);
        assert_eq!(card.glint0, Some(g2), "slewed by +10");
        card.lock_step(seen_at(g2, 62, 100));
        assert_eq!(card.glint0, Some(g2 - ms(7)), "folded, slewed by −7");
    }

    #[test]
    fn claudes_repaints_carry_the_flowing_cards_steps() {
        let s = scene();
        let run = |period: u64, extra: u64, steps_lock: bool| {
            let t0 = Instant::now();
            let us = |n: u64| t0 + Duration::from_micros(n);
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            let mut kept = CardFrames::default();
            let mut draw = |card: &mut HarnessCard, now: Instant| {
                if card.replay_step(&s, true, &kept, now).is_none() {
                    let _ = card.draw(now, &s, true, &mut kept);
                }
            };
            draw(&mut card, t0);
            let claude0 = 500_000u64;
            let repaint = |n: u64| claude0 + n * period + ((n * 37) % 13) * 1000;
            let flaring = |at: u64| (at.saturating_sub(claude0) % 3_072_000) < 2 * 128_000;
            let (mut n, mut lit) = (0u64, false);
            let (mut fires, mut steps) = (0u64, 0u64);
            let settle = 20_000_000u64;
            let end = settle + 30_000_000;
            let mut last_step = None;
            loop {
                let at = repaint(n);
                if at > end {
                    break;
                }
                while let Some(wake) = card.deadline(us(at)).filter(|&w| w < us(at)) {
                    let w = u64::try_from(wake.duration_since(t0).as_micros()).unwrap();
                    draw(&mut card, wake);
                    if w >= settle {
                        fires += 1;
                    }
                    if card.deadline(wake) == Some(wake) {
                        break;
                    }
                }
                let flare = flaring(at);
                if flare && !lit {
                    card.lock_glint(us(at) - ms(extra));
                } else if steps_lock {
                    card.lock_step(us(at) - ms(extra));
                }
                lit = flare;
                draw(&mut card, us(at));
                if at >= settle {
                    let step = card.flow_step(us(at));
                    if step != last_step {
                        steps += 1;
                        last_step = step;
                    }
                }
                n += 1;
            }
            (fires, steps)
        };
        let (fires, steps) = run(127_700, 0, true);
        assert!(steps > 200, "CONTROL: 30 s of flow ({steps} steps)");
        assert!(
            fires * 20 < steps,
            "rides Claude's repaints: {fires} wakes in {steps} steps"
        );
        for period in [128_000, 128_600, 129_000] {
            let (fires, steps) = run(period, 0, true);
            assert!(
                fires * 5 < steps,
                "{period} µs: still rides them: {fires} wakes in {steps} steps"
            );
        }
        let (fires, steps) = run(127_700, 0, false);
        assert!(
            fires * 2 > steps,
            "NEGATIVE CONTROL: the flare's lock alone falls behind: {fires} of {steps}"
        );
        let (fires, steps) = run(127_700, 83 - FLARE_STEP_LEAD_MS, false);
        assert!(
            fires * 10 >= steps * 9,
            "NEGATIVE CONTROL: the old lock's wakes: {fires} of {steps} steps"
        );
    }

    #[test]
    fn episode_frames_move_and_keep_a_deadline() {
        for s in scenes() {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 11);
            let mut prev: Option<(Vec<GlowQuad>, Vec<GlowQuad>, u64)> = None;
            let mut at = 0;
            while at < EPISODE_MS {
                let now = t0 + ms(at);
                let f = frame(&mut card, now, &s, true);
                assert_eq!(card.deadline(now), Some(now + ms(FRAME_MS)), "{at} ms");
                if let Some(p) = &prev {
                    assert_ne!(p.2, f.2, "the fingerprint held at {at} ms");
                    if at < 5000 {
                        assert!(p.0 != f.0 || p.1 != f.1, "the quads held at {at} ms");
                    }
                }
                prev = Some(f);
                at += FRAME_MS;
            }
            let end = t0 + ms(EPISODE_MS);
            assert_eq!(
                card.deadline(end),
                Some(end),
                "the flow's first frame is owed"
            );
            frame(&mut card, end, &s, true);
            let step = EPISODE_MS / FLOW_STEP_MS;
            assert_eq!(
                card.deadline(end),
                Some(t0 + ms((step + 1) * FLOW_STEP_MS + FLOW_SLACK_MS)),
                "then the step grid"
            );
        }
    }

    #[test]
    fn every_quad_lies_in_one_band_inside_the_box() {
        for s in scenes() {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 3);
            let mut at = 0;
            while at <= EPISODE_MS + 2 * GLINT_PERIOD_MS + 100 {
                let (under, over, _) = frame(&mut card, t0 + ms(at), &s, true);
                for q in &under {
                    assert_in_band_and_box(q, &s, &format!("under at {at} ms"));
                }
                for q in &over {
                    assert_in_band_and_box(q, &s, &format!("over at {at} ms"));
                }
                at += 17;
            }
        }
    }

    #[test]
    fn air_moves_the_border_and_nothing_else() {
        let extent = |quads: &[GlowQuad]| {
            let y0 = quads.iter().map(|q| i64::from(q.y)).min();
            let y1 = quads.iter().map(|q| i64::from(q.y) + i64::from(q.h)).max();
            let x0 = quads.iter().map(|q| i64::from(q.x)).min();
            let x1 = quads.iter().map(|q| i64::from(q.x) + i64::from(q.w)).max();
            (y0.unwrap_or(0), y1.unwrap_or(0), x0, x1)
        };
        let tagged = |quads: &[GlowQuad], row: u16| quads.iter().filter(|q| q.row == row).count();
        for bare in [
            scene(),
            CardScene {
                kitty_rows: 3,
                dark: false,
                palette: CardPalette::Land,
                ..scene()
            },
        ] {
            let g = Geo::new(&bare).expect("a scene");
            let (top, bottom) = (g.band(g.top), g.band(g.top + g.rows));
            let half = g.ch / 2;
            let (over, under) = (
                bare.row_base + bare.top - 1,
                bare.row_base + bare.top + bare.rows,
            );
            for clock in [
                Clock::SETTLED,
                Clock::riding(150),
                Clock::riding(1300),
                Clock::seated(150),
                Clock::flowing(EPISODE_MS + 500),
                Clock::flowing(EPISODE_MS + 1900),
            ] {
                let (a, b) = (parts(&bare, clock), parts(&aired(bare), clock));
                let what = format!("{} rows, {clock:?}", bare.kitty_rows);
                assert!(!a.kitty.is_empty() && !a.trail.is_empty(), "{what}");
                assert_eq!(a.trail, b.trail, "{what}: the trail");
                assert_eq!(a.kitty, b.kitty, "{what}: the kitty");
                assert_eq!(a.rim, b.rim, "{what}: the rim");
                assert_eq!(a.stars, b.stars, "{what}: the field");
                assert_eq!(a.sparkles, b.sparkles, "{what}: the sparkles");
                assert_eq!(a.burst, b.burst, "{what}: the burst");
                let (ya0, ya1, xa0, xa1) = extent(&a.border);
                assert_eq!(
                    (ya0, ya1, tagged(&a.border, over), tagged(&a.border, under)),
                    (top, bottom, 0, 0),
                    "{what}: CONTROL, no air: the border closes on the boxed rows"
                );
                let (yb0, yb1, xb0, xb1) = extent(&b.border);
                assert_eq!((yb0, yb1), (top - half, bottom + half), "{what}: the air");
                assert_eq!((xb0, xb1), (xa0, xa1), "{what}: the sides stay");
                let n = tagged(&a.border, bare.row_base + bare.top);
                assert!(n > 2, "{what}: the top edge");
                assert_eq!(tagged(&b.border, over), n, "{what}: over the box");
                assert_eq!(tagged(&b.border, under), n, "{what}: under the box");
                assert_eq!(b.border.len(), a.border.len() + 4, "{what}: four slices");
                for q in &b.border {
                    assert_in_band_and_box(q, &aired(bare), &what);
                }
            }
        }
        let first = CardScene { top: 0, ..scene() };
        let lifted = CardScene {
            pad_above: first.ch,
            ..first
        };
        let border = parts(&lifted, Clock::SETTLED).border;
        assert_eq!(border, parts(&first, Clock::SETTLED).border);
        assert_eq!(
            extent(&border).0,
            i64::from(first.origin_y),
            "nothing over band 0"
        );
    }

    fn runs_of(frame: &[&str]) -> usize {
        frame
            .iter()
            .map(|r| {
                let b = r.as_bytes();
                (0..b.len())
                    .filter(|&i| b[i] != b'.' && (i == 0 || b[i - 1] != b[i]))
                    .count()
            })
            .sum()
    }

    #[test]
    fn the_kitty_is_opaque_square_pixels_and_dark_marks_are_light() {
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                kitty_rows,
                ..scene()
            };
            let g = Geo::new(&s).expect("a scene");
            let at = kitty_rect(&g, &s).expect("a kitty");
            let inks = KittyInk::new(s.coat, s.eye, s.dark);
            let (under, over, _) = settled(&s);
            assert!(
                under.iter().all(|q| q.alpha == 0 && q.alpha2 == 0),
                "dark border and trail are light"
            );
            assert!(
                over.iter().all(|q| q.alpha == 0 || q.alpha == 255),
                "the rim and the stars are light on dark"
            );
            let kitty: Vec<GlowQuad> = over.iter().filter(|q| q.alpha == 255).copied().collect();
            let art = |f: &[&str]| f.iter().map(|r| (*r).to_string()).collect::<Vec<_>>();
            assert_eq!(
                drawn_kitty(&kitty, &at, inks),
                art(at.art.open),
                "{kitty_rows} rows"
            );
            let area: i64 = kitty.iter().map(|q| i64::from(q.w) * i64::from(q.h)).sum();
            let solid = at
                .art
                .open
                .iter()
                .map(|r| r.bytes().filter(|&b| b != b'.').count())
                .sum::<usize>() as i64;
            assert_eq!(
                area,
                solid * at.p * at.p,
                "{kitty_rows} rows: no overlap, no spill"
            );
            let runs = runs_of(at.art.open);
            assert!(
                kitty.len() * 6 < runs * 5,
                "{kitty_rows} rows: {} quads for {runs} runs",
                kitty.len()
            );
            if kitty_rows == 2 {
                let mid = s.origin_y + i32::from(s.top + 1) * i32::from(s.ch);
                assert!(
                    kitty.iter().all(|q| {
                        let (y0, y1) = (i32::from(q.y), i32::from(q.y) + i32::from(q.h));
                        y1 <= mid || y0 >= mid
                    }) && kitty.iter().any(|q| i32::from(q.y) + i32::from(q.h) == mid),
                    "the halves meet at the boundary"
                );
            }
            let light = CardScene { dark: false, ..s };
            let lit = parts(&light, Clock::SETTLED);
            assert_eq!(lit.kitty.len(), kitty.len(), "{kitty_rows} rows");
            assert!(
                lit.kitty
                    .iter()
                    .chain(&lit.rim)
                    .all(|q| (q.alpha, q.alpha2) == (255, 255)),
                "the kitty and its rim are opaque on light too (print art)"
            );
            let mut nudged = kitty.clone();
            nudged[0].x += u16::try_from(at.p).expect("a pixel");
            assert_ne!(drawn_kitty(&nudged, &at, inks), art(at.art.open));
        }
    }

    fn mark_inks(palette: CardPalette) -> Vec<u32> {
        (0..TRAIL_STOPS.len())
            .map(|k| palette.star_ink(k))
            .chain([WHITE])
            .collect()
    }

    #[test]
    fn a_light_theme_is_source_over_ink_throughout() {
        for s in scenes().into_iter().filter(|s| !s.dark) {
            let inks = mark_inks(s.palette);
            let printed = |c: u32, a: u8| inks.iter().any(|&k| premul_rgb(k, a) == c);
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 5);
            let flow =
                (EPISODE_MS..=EPISODE_MS + 2 * GLINT_PERIOD_MS).step_by(FLOW_STEP_MS as usize);
            for at in (0..EPISODE_MS).step_by(30).chain(flow) {
                let (under, over, _) = frame(&mut card, t0 + ms(at), &s, true);
                for q in under.iter().chain(&over) {
                    assert!(
                        q.alpha > 0 && q.alpha2 > 0,
                        "additive on white at {at} ms: {q:?}"
                    );
                    if q.alpha < 255 || q.alpha2 < 255 {
                        assert!(
                            printed(q.color, q.alpha) && printed(q.color2, q.alpha2),
                            "a translucent mark off the print at {at} ms: {q:?}"
                        );
                    }
                }
            }
            let law = InkRole::OverText.ink(spectrum_stop(0));
            assert!(!printed(premul_rgb(law, 176), 176), "CONTROL");
        }
    }

    fn on_any(quads: &[GlowQuad], x: i64, y: i64) -> bool {
        quads.iter().any(|q| {
            let (qx, qy) = (i64::from(q.x), i64::from(q.y));
            (qx..qx + i64::from(q.w)).contains(&x) && (qy..qy + i64::from(q.h)).contains(&y)
        })
    }

    fn print_breaches(marks: &[GlowQuad], trail: &[GlowQuad], palette: CardPalette) -> Vec<String> {
        let inks: Vec<u32> = (0..TRAIL_STOPS.len())
            .map(|k| palette.star_ink(k))
            .collect();
        let mut out = Vec::new();
        for q in marks {
            let white = q.color == premul_rgb(WHITE, q.alpha);
            if white && inks.iter().any(|&k| premul_rgb(k, q.alpha) == q.color) {
                continue;
            }
            for y in i64::from(q.y)..i64::from(q.y) + i64::from(q.h) {
                for x in i64::from(q.x)..i64::from(q.x) + i64::from(q.w) {
                    if white != on_any(trail, x, y) {
                        out.push(format!("({x}, {y}) white {white}: {q:?}"));
                    }
                }
            }
        }
        out
    }

    #[test]
    fn light_marks_print_on_the_paper_and_glint_on_the_trail() {
        let (mut glints, mut inked) = (0usize, 0usize);
        let longest = CardScene {
            dark: false,
            ..widest()
        };
        for s in scenes()
            .into_iter()
            .filter(|s| !s.dark && s.trail.is_some())
            .chain([longest])
        {
            for t in (0..=EPISODE_MS).step_by(30).map(Some).chain([None]) {
                for ride_ms in [t, None] {
                    let clock = Clock {
                        t_ms: t,
                        ride_ms,
                        glint: None,
                        flow: None,
                    };
                    let p = parts(&s, clock);
                    let marks: Vec<GlowQuad> = p
                        .stars
                        .iter()
                        .chain(&p.sparkles)
                        .chain(&p.burst)
                        .copied()
                        .collect();
                    let bad = print_breaches(&marks, &p.trail, s.palette);
                    assert!(
                        bad.is_empty(),
                        "{t:?}/{ride_ms:?}: {:?}",
                        &bad[..bad.len().min(4)]
                    );
                    for q in &marks {
                        if q.color == premul_rgb(WHITE, q.alpha) {
                            glints += 1;
                        } else {
                            inked += 1;
                        }
                    }
                }
            }
        }
        assert!(glints > 0 && inked > 0, "glints {glints}, ink {inked}");
        let s = CardScene {
            dark: false,
            ..scene()
        };
        let geo = Geo::new(&s).expect("a scene");
        let region = star_region(&geo, &s).expect("a region");
        let p = parts(&s, Clock::SETTLED);
        let (ya, yb) = trail_band(&geo, &s);
        let centre = (geo.col(34), (ya + yb) / 2);
        let mut old = Vec::new();
        let sink = Sink { geo };
        lay_star(
            &mut old,
            &sink,
            region,
            centre,
            4,
            STAR_COV,
            false,
            s.palette.star_ink(0),
            &[],
            false,
        );
        assert!(
            !print_breaches(&old, &p.trail, s.palette).is_empty(),
            "CONTROL"
        );
    }

    #[test]
    fn light_marks_lie_only_on_cells_read_blank() {
        let (mut marks_seen, mut dark_past) = (0usize, 0usize);
        for s in scenes().into_iter().filter(|s| !s.dark) {
            let g = Geo::new(&s).expect("a scene");
            let start = match (s.trail, s.kitty_col) {
                (Some((t0, _)), Some(k)) => t0.min(k),
                (Some((t0, _)), None) => t0,
                (None, Some(k)) => k,
                (None, None) => continue,
            };
            let blank = Rect {
                x0: g.col(start),
                y0: g.band(g.top),
                x1: g.col(s.cols),
                y1: g.band(g.top + 2),
            };
            for t in (0..=EPISODE_MS).step_by(30).map(Some).chain([None]) {
                for ride_ms in [t, None] {
                    let p = parts(
                        &s,
                        Clock {
                            t_ms: t,
                            ride_ms,
                            glint: None,
                            flow: None,
                        },
                    );
                    for q in p.stars.iter().chain(&p.sparkles).chain(&p.burst) {
                        marks_seen += 1;
                        let (x, y) = (i64::from(q.x), i64::from(q.y));
                        let (w, h) = (i64::from(q.w), i64::from(q.h));
                        assert!(
                            x >= blank.x0
                                && y >= blank.y0
                                && x + w <= blank.x1
                                && y + h <= blank.y1,
                            "{t:?}/{ride_ms:?}: {q:?} outside {blank:?}"
                        );
                    }
                }
            }
            let dark = CardScene { dark: true, ..s };
            let region = star_region(&g, &dark).expect("a region");
            dark_past += usize::from(region.x1 > blank.x1);
        }
        assert!(marks_seen > 0);
        assert!(dark_past > 0, "CONTROL");
    }

    #[test]
    fn the_light_field_keeps_off_the_trails_edge() {
        let mut across_dark = 0;
        for s in scenes().into_iter().filter(|s| s.trail.is_some()) {
            let g = Geo::new(&s).expect("a scene");
            let Some(t) = trail_rect(&g, &s, kitty_rect(&g, &s)) else {
                continue;
            };
            for seed in 0..64 {
                let (spots, n) = field_spots(&g, &s, seed, !s.dark);
                for sp in &spots[..n] {
                    let arm = i64::from(star_arm_px(star_arm(g.ch as f32, sp.ratio)));
                    let m = arm + 1;
                    let reach = Rect {
                        x0: sp.x - m,
                        y0: sp.y - m,
                        x1: sp.x + m + 1,
                        y1: sp.y + m + 1,
                    };
                    if s.dark {
                        across_dark += usize::from(across_trail(reach, t));
                    } else {
                        assert!(!across_trail(reach, t), "seed {seed}: {sp:?}");
                    }
                }
            }
        }
        assert!(across_dark > 0, "CONTROL");
    }

    #[test]
    fn the_kept_line_inks_are_the_law() {
        let cream = 0x00FB_F1C7;
        let step = |i: usize| i as f32 / LINE_INK_STEPS as f32;
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
            let mut inks = LineInks::default();
            inks.solve(palette, WHITE);
            for i in 0..=LINE_INK_STEPS {
                for ground in [WHITE, cream] {
                    assert_eq!(
                        inks.at(palette, ground, step(i)),
                        palette.text_at(step(i), ground),
                        "{palette:?} step {i} on {ground:06x}"
                    );
                }
            }
            let half = 0.4 / LINE_INK_STEPS as f32;
            assert_eq!(
                inks.at(palette, WHITE, step(7) + half),
                inks.at(palette, WHITE, step(7))
            );
            assert_eq!(inks.at(palette, WHITE, -1.0), inks.at(palette, WHITE, 0.0));
            assert_eq!(inks.at(palette, WHITE, 2.0), inks.at(palette, WHITE, 1.0));
            assert_eq!(
                inks.at(palette, WHITE, f32::NAN),
                inks.at(palette, WHITE, 0.0)
            );
            assert!(
                (0..=LINE_INK_STEPS)
                    .any(|i| palette.text_at(step(i), WHITE) != palette.text_at(step(i), cream)),
                "{palette:?}: CONTROL, the grounds' inks differ"
            );
        }
        let t0 = Instant::now();
        let mut light = HarnessCard::default();
        light.arm(t0, 7);
        let page = CardScene {
            dark: false,
            ground: cream,
            palette: CardPalette::Land,
            ..scene()
        };
        light.tick(t0, &page, true, &mut Vec::new(), &mut Vec::new());
        assert_eq!(light.inks.key, Some((CardPalette::Land, cream)));
        let white = CardScene {
            ground: WHITE,
            palette: CardPalette::Cruise,
            ..page
        };
        light.tick(
            t0 + ms(FRAME_MS),
            &white,
            true,
            &mut Vec::new(),
            &mut Vec::new(),
        );
        assert_eq!(light.inks.key, Some((CardPalette::Cruise, WHITE)));
        let mut dark = HarnessCard::default();
        dark.arm(t0, 7);
        dark.tick(t0, &scene(), true, &mut Vec::new(), &mut Vec::new());
        assert_eq!(dark.inks.key, None);
    }

    fn star_boxes(quads: &[GlowQuad]) -> Vec<Rect> {
        let rect = |q: &GlowQuad| Rect {
            x0: i64::from(q.x),
            y0: i64::from(q.y),
            x1: i64::from(q.x) + i64::from(q.w),
            y1: i64::from(q.y) + i64::from(q.h),
        };
        let mut boxes: Vec<Rect> = Vec::new();
        for r in quads.iter().map(rect) {
            let grown = Rect {
                x0: r.x0 - 1,
                y0: r.y0 - 1,
                x1: r.x1 + 1,
                y1: r.y1 + 1,
            };
            let mut merged = r;
            boxes.retain(|b| {
                if b.meet(grown).is_empty() {
                    true
                } else {
                    merged = Rect {
                        x0: merged.x0.min(b.x0),
                        y0: merged.y0.min(b.y0),
                        x1: merged.x1.max(b.x1),
                        y1: merged.y1.max(b.y1),
                    };
                    false
                }
            });
            boxes.push(merged);
        }
        boxes
    }

    #[test]
    fn light_sparkles_lie_whole_on_the_trails_ink() {
        let mut seen = 0usize;
        for s in scenes()
            .into_iter()
            .filter(|s| s.trail.is_some())
            .map(|s| CardScene { dark: false, ..s })
        {
            for t in (0..RIDE_MS).step_by(20) {
                let p = parts(
                    &s,
                    Clock {
                        t_ms: Some(t),
                        ride_ms: Some(t),
                        glint: None,
                        flow: None,
                    },
                );
                for b in star_boxes(&p.sparkles) {
                    seen += 1;
                    assert_eq!(
                        b.x1 - b.x0,
                        b.y1 - b.y0,
                        "{}x{} at {t} ms: a cut sparkle {b:?}",
                        s.cw,
                        s.ch
                    );
                }
                assert!(
                    p.sparkles
                        .iter()
                        .all(|q| q.color == premul_rgb(WHITE, q.alpha)),
                    "{}x{} at {t} ms: a sparkle off the trail's ink",
                    s.cw,
                    s.ch
                );
            }
        }
        assert!(seen > 0);
        let s = CardScene {
            dark: false,
            ..scene()
        };
        let geo = Geo::new(&s).expect("a scene");
        let clock = Clock {
            t_ms: Some(300),
            ride_ms: Some(300),
            glint: None,
            flow: None,
        };
        let cx = Emit::new(&s, geo, clock, 7);
        let region = star_region(&geo, &s).expect("a region");
        let lay = cx.trail_lay().expect("a trail");
        let x = (lay.tx0 + lay.drawn) / 2;
        let trail = cx.trail_ink();
        let edge = trail
            .covers()
            .iter()
            .find(|c| (c.x0..c.x1).contains(&x))
            .expect("a segment");
        let centre = (x, edge.y0);
        let mut two = Vec::new();
        let sink = Sink { geo };
        let arm = i64::from(star_arm_px(star_arm(
            geo.ch as f32,
            STAR_ARM_FINE * STAR_ARM_INK,
        )));
        lay_star(
            &mut two,
            &sink,
            region,
            centre,
            arm,
            SHED_COV,
            false,
            s.palette.star_ink(0),
            trail.covers(),
            false,
        );
        assert!(
            two.iter().any(|q| q.color == premul_rgb(WHITE, q.alpha))
                && two.iter().any(|q| q.color != premul_rgb(WHITE, q.alpha)),
            "CONTROL: two tones {two:?}"
        );
    }

    #[test]
    fn a_light_stars_nucleus_and_glints_print_at_its_strength() {
        let s = CardScene {
            dark: false,
            ..scene()
        };
        let geo = Geo::new(&s).expect("a scene");
        let region = star_region(&geo, &s).expect("a region");
        let centre = (region.x0 + 3 * geo.cw, region.y0 + geo.ch);
        let arm = 8;
        let d = ((arm as f32 * STAR_GLINT).round() as i64).max(1);
        let cov = STAR_COV * STAR_STEADY_ENV;
        for k in 0..TRAIL_STOPS.len() {
            let ink_rgb = s.palette.star_ink(k);
            let mut out = Vec::new();
            let sink = Sink { geo };
            lay_star(
                &mut out,
                &sink,
                region,
                centre,
                arm,
                cov,
                true,
                ink_rgb,
                &[],
                false,
            );
            let arm32 = i32::try_from(arm).expect("a small arm");
            let (c, t) = (
                i64::from(star_core_px(arm32)),
                i64::from(star_waist_px(arm32)),
            );
            assert!(c > t, "the nucleus stands out past the waist");
            let cc = (c - 1) / 2;
            let core = Rect {
                x0: centre.0 - cc,
                y0: centre.1 - cc,
                x1: centre.0 - cc + c,
                y1: centre.1 - cc + c,
            };
            let touching: Vec<&GlowQuad> = out
                .iter()
                .filter(|q| {
                    !core
                        .meet(Rect {
                            x0: i64::from(q.x),
                            y0: i64::from(q.y),
                            x1: i64::from(q.x) + i64::from(q.w),
                            y1: i64::from(q.y) + i64::from(q.h),
                        })
                        .is_empty()
                })
                .collect();
            assert!(!touching.is_empty());
            for q in touching {
                assert_eq!(
                    (q.color, q.alpha),
                    (premul_rgb(ink_rgb, 255), 255),
                    "ink {k}: the nucleus {q:?}"
                );
            }
            for (gx, gy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
                let (x, y) = (centre.0 + gx, centre.1 + gy);
                let dot = out
                    .iter()
                    .find(|q| (i64::from(q.x), i64::from(q.y), q.w, q.h) == (x, y, 1, 1))
                    .unwrap_or_else(|| panic!("ink {k}: no glint at ({x}, {y})"));
                assert_eq!(
                    (dot.color, dot.alpha),
                    (premul_rgb(ink_rgb, 255), 255),
                    "ink {k}: {dot:?}"
                );
            }
            let before = ink(
                ink_rgb,
                (cov * STAR_GLINT_COV).floor() * 255.0 / STAR_PRINT_FULL,
            );
            assert!(before.alpha < 128, "CONTROL {before:?}");
            let additive = ink(
                ink_rgb,
                (cov * STAR_CORE_ADD).floor() * 255.0 / STAR_PRINT_FULL,
            );
            assert!(additive.alpha < 128, "CONTROL {additive:?}");
        }
    }

    fn pixels(quads: &[GlowQuad]) -> std::collections::BTreeSet<(i64, i64)> {
        let mut px = std::collections::BTreeSet::new();
        for q in quads {
            for y in i64::from(q.y)..i64::from(q.y) + i64::from(q.h) {
                for x in i64::from(q.x)..i64::from(q.x) + i64::from(q.w) {
                    px.insert((x, y));
                }
            }
        }
        px
    }

    fn one_sided(cx: &Emit, region: Rect, (x, y): (i64, i64)) -> Rect {
        let Some(lay) = cx.trail_lay() else {
            return region;
        };
        let w = cx.wave(&lay, 0).abs();
        let t = Rect {
            x0: lay.tx0,
            y0: lay.ya - w,
            x1: lay.drawn,
            y1: lay.yb + w,
        };
        if t.contains(x, y) {
            return region.meet(Rect {
                y0: lay.ya + w,
                y1: lay.yb - w,
                ..t
            });
        }
        [
            (t.x0 - 1 - x, Rect { x1: t.x0, ..region }),
            (x - t.x1, Rect { x0: t.x1, ..region }),
            (t.y0 - 1 - y, Rect { y1: t.y0, ..region }),
            (y - t.y1, Rect { y0: t.y1, ..region }),
        ]
        .into_iter()
        .filter(|(d, _)| *d >= 0)
        .max_by_key(|(d, _)| *d)
        .map_or(region, |(_, r)| region.meet(r))
    }

    #[test]
    fn light_burst_stars_lie_whole() {
        let (mut frames, mut crossed, mut cut_before) = (0usize, 0usize, 0usize);
        for s in scenes()
            .into_iter()
            .filter(|s| !s.dark && s.trail.is_some())
        {
            let geo = Geo::new(&s).expect("a scene");
            for seed in [3u64, 7, 11, 19, 23] {
                for ride in [true, false] {
                    for t in (0..RIDE_MS + BURST_MS).step_by(20) {
                        let clock = Clock {
                            t_ms: Some(t),
                            ride_ms: ride.then_some(t),
                            glint: None,
                            flow: None,
                        };
                        let cx = Emit::new(&s, geo, clock, seed);
                        let Some(ring) = cx.burst_ring() else {
                            continue;
                        };
                        frames += 1;
                        let sink = Sink { geo };
                        let mut burst = Vec::new();
                        cx.burst_stars(&mut lay_calls(&cx, &mut burst));
                        let (mut whole, mut before) = (Vec::new(), Vec::new());
                        for (k, &c) in ring.centres.iter().enumerate() {
                            let rgb = s.palette.star_ink(k);
                            for (out, clip) in [
                                (&mut whole, ring.region),
                                (&mut before, one_sided(&cx, ring.region, c)),
                            ] {
                                lay_star(
                                    out,
                                    &sink,
                                    clip,
                                    c,
                                    ring.arm,
                                    ring.cov,
                                    true,
                                    rgb,
                                    &[],
                                    false,
                                );
                            }
                        }
                        let want = pixels(&whole);
                        let what = format!("{}x{} seed {seed} ride {ride} {t} ms", s.cw, s.ch);
                        assert_eq!(pixels(&burst), want, "{what}");
                        let tones = burst
                            .iter()
                            .map(|q| q.color == premul_rgb(WHITE, q.alpha))
                            .collect::<std::collections::BTreeSet<bool>>();
                        crossed += usize::from(tones.len() == 2);
                        cut_before += usize::from(pixels(&before) != want);
                    }
                }
            }
        }
        assert!(
            frames > 0 && crossed > 0,
            "{frames} frames, {crossed} in two tones"
        );
        assert!(cut_before * 2 > frames, "CONTROL: {cut_before} of {frames}");
    }

    #[test]
    fn the_light_sweep_only_deepens_a_shade() {
        let grounds = light_builtins();
        let lum = |g: &&(&str, u32)| relative_luminance(g.1);
        let brightest = grounds.iter().max_by(|a, b| lum(a).total_cmp(&lum(b)));
        let dimmest = grounds.iter().min_by(|a, b| lum(a).total_cmp(&lum(b)));
        let value = |rgb: u32| (rgb >> 16).max((rgb >> 8) & 0xff).max(rgb & 0xff) as f32;
        for &(name, g) in brightest.into_iter().chain(dimmest) {
            for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
                let t0 = Instant::now();
                let mut card = HarnessCard::default();
                let page = CardScene {
                    palette,
                    dark: false,
                    ground: g,
                    ..scene()
                };
                card.tick(t0, &page, false, &mut Vec::new(), &mut Vec::new());
                card.arm(t0, 9);
                let len = 80u16;
                let mut least = f32::MAX;
                for at in (0..=EPISODE_MS).step_by(10) {
                    let now = t0 + ms(at);
                    let Some(run) = card.running(now) else {
                        continue;
                    };
                    let clock = Clock {
                        t_ms: Some(run),
                        ride_ms: None,
                        glint: None,
                        flow: None,
                    };
                    let phi = clock.line_drift(LINE_DRIFT);
                    for (i, c) in card.line_ink(now, 0, 0, len, g, true).iter().enumerate() {
                        let rgb = (u32::from(c.color[0]) << 16)
                            | (u32::from(c.color[1]) << 8)
                            | u32::from(c.color[2]);
                        let x = i as f32 / f32::from(len - 1);
                        let plain = card.inks.at(palette, g, tri(x + phi));
                        least = least.min(value(rgb) / value(plain));
                    }
                }
                assert!(
                    (0.69..0.8).contains(&least),
                    "{name} {palette:?}: the darkest swept letter keeps {least:.3}"
                );
            }
        }
        let red = PRINT_CRUISE[0];
        let mud = mix_rgb(red, 0, LINE_SWEEP_GAIN);
        assert!(value(mud) / value(red) < 0.69, "CONTROL");
    }

    #[test]
    fn the_kept_line_inks_step_unseen() {
        let grounds = light_builtins();
        let lum = |g: &&(&str, u32)| relative_luminance(g.1);
        let brightest = grounds.iter().max_by(|a, b| lum(a).total_cmp(&lum(b)));
        let dimmest = grounds.iter().min_by(|a, b| lum(a).total_cmp(&lum(b)));
        let gap = |palette: CardPalette, g: u32, steps: usize| {
            (0..steps)
                .map(|i| {
                    let at = |i: usize| palette.text_at(i as f32 / steps as f32, g);
                    crate::color_math::oklab_distance(at(i), at(i + 1))
                })
                .fold(0.0f32, f32::max)
        };
        for &(name, g) in brightest.into_iter().chain(dimmest) {
            for palette in [CardPalette::Cruise, CardPalette::Land] {
                let mut inks = LineInks::default();
                inks.solve(palette, g);
                let widest = (0..LINE_INK_STEPS)
                    .map(|i| crate::color_math::oklab_distance(inks.ink[i], inks.ink[i + 1]))
                    .fold(0.0f32, f32::max);
                assert!(widest <= 0.02, "{name} {palette:?}: a step of {widest:.4}");
                assert!(
                    gap(palette, g, LINE_INK_STEPS / 16) > 0.02,
                    "{name} {palette:?}: CONTROL"
                );
            }
        }
    }

    fn light_builtins() -> Vec<(&'static str, u32)> {
        use aterm_types::scheme::{Appearance, builtin, builtin_names};
        builtin_names()
            .into_iter()
            .filter_map(|n| Some((n, builtin(n)?)))
            .filter(|(_, s)| s.appearance == Appearance::Light)
            .map(|(n, s)| {
                let c = s.background;
                (
                    n,
                    (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b),
                )
            })
            .collect()
    }

    fn light_letters(palette: CardPalette, ground: u32) -> Vec<u32> {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.tick(
            t0,
            &CardScene {
                palette,
                dark: false,
                ground,
                ..scene()
            },
            false,
            &mut Vec::new(),
            &mut Vec::new(),
        );
        card.line_ink(t0, 0, 0, 80, ground, false)
            .iter()
            .map(|c| {
                (u32::from(c.color[0]) << 16) | (u32::from(c.color[1]) << 8) | u32::from(c.color[2])
            })
            .collect()
    }

    #[test]
    fn the_light_line_is_the_print_at_text_weight() {
        let grounds = light_builtins();
        assert!(grounds.len() >= 7, "{grounds:?}");
        for &(name, g) in &grounds {
            for palette in [CardPalette::Cruise, CardPalette::Land] {
                for rgb in light_letters(palette, g) {
                    let ch = [rgb >> 16, (rgb >> 8) & 0xff, rgb & 0xff];
                    let c = contrast(rgb, g);
                    assert!(
                        ch.contains(&0),
                        "{name} {palette:?}: {rgb:06x} is not a pure hue"
                    );
                    assert!(c >= 4.5, "{name} {palette:?}: {rgb:06x} at {c:.2}:1");
                    assert!(
                        ch.contains(&0xff) || c < 4.6,
                        "{name} {palette:?}: {rgb:06x} is deeper than the text needs, {c:.2}:1"
                    );
                }
            }
        }
        let white = 0x00FF_FFFF;
        for &print in &PRINT_CRUISE[..4] {
            let law = InkRole::OverText.ink(print);
            assert!(contrast(law, white) > 6.0, "CONTROL {law:06x}");
        }
    }

    #[test]
    fn the_light_line_reads_on_every_light_ground() {
        let dim = 0x00C8_C8C8;
        assert!(!dark_ground(dim));
        let mut grounds: Vec<u32> = light_builtins().into_iter().map(|(_, g)| g).collect();
        grounds.push(dim);
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
            for &g in &grounds {
                let t0 = Instant::now();
                let mut card = HarnessCard::default();
                card.tick(
                    t0,
                    &CardScene {
                        palette,
                        dark: false,
                        ground: g,
                        ..scene()
                    },
                    false,
                    &mut Vec::new(),
                    &mut Vec::new(),
                );
                card.arm(t0, 9);
                for at in (0..=EPISODE_MS + 100).step_by(50) {
                    for c in card.line_ink(t0 + ms(at), 0, 0, 80, g, true) {
                        let rgb = (u32::from(c.color[0]) << 16)
                            | (u32::from(c.color[1]) << 8)
                            | u32::from(c.color[2]);
                        assert!(
                            contrast(rgb, g) >= 4.5,
                            "{palette:?} {rgb:06x} on {g:06x} at {at} ms: {:.2}:1",
                            contrast(rgb, g)
                        );
                    }
                }
            }
            let worst = light_letters(palette, 0x00FF_FFFF)
                .into_iter()
                .map(|rgb| contrast(rgb, dim))
                .fold(f32::MAX, f32::min);
            assert!(worst < 4.5, "CONTROL {palette:?}: {worst:.2}");
        }
    }

    /// The line reads 4.5:1 on every ground [`dark_ground`] calls dark, not only near-black ones:
    /// every dark built-in (Nord's `#2e3440` read 4.16:1), a grey every few steps up to the cut,
    /// and the mid grounds imported themes bring (Silver Aerogel `#929292`, Unikitty `#ff8cd9`),
    /// where a letter lifted to [`LINE_DARK_LUMA_FLOOR`] can sit on the ground's own luminance
    /// (1.00:1) — the resting letters ([`CardPalette::letter_on`]) and every frame of the
    /// episode, hero word and sweeps included. NEGATIVE CONTROL: the lifted letter alone on
    /// Silver Aerogel's ground, so the grounds reach the case the floor answers.
    #[test]
    fn the_dark_line_reads_on_every_dark_ground() {
        use aterm_types::scheme::{Appearance, builtin, builtin_names};
        let mut grounds: Vec<u32> = builtin_names()
            .into_iter()
            .filter_map(builtin)
            .filter(|s| s.appearance == Appearance::Dark)
            .map(|s| {
                let c = s.background;
                (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
            })
            .collect();
        assert!(grounds.len() >= 7, "{grounds:?}");
        grounds.extend((0..=255u32).step_by(6).map(|v| v * 0x0001_0101));
        grounds.extend([0x0092_9292, 0x00FF_8CD9]);
        grounds.retain(|&g| dark_ground(g));
        let line = LineAt {
            row: 0,
            col: 0,
            len: 60,
            hero: 20..26,
            wand: None,
        };
        let mut ink = Vec::new();
        let mut lows = Vec::new();
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
            for &g in &grounds {
                // The ground's least: `(contrast, what)`.
                let mut least = (f32::MAX, String::new());
                let mut note = |rgb: u32, what: &dyn Fn() -> String| {
                    let c = contrast(rgb, g);
                    if c < least.0 {
                        least = (c, format!("{rgb:06x} {}", what()));
                    }
                };
                for k in 0..=255u8 {
                    let t = f32::from(k) / 255.0;
                    note(palette.letter_on(t, g), &|| format!("resting at {t:.3}"));
                }
                let t0 = Instant::now();
                let mut card = HarnessCard::default();
                card.tick(
                    t0,
                    &CardScene {
                        palette,
                        ground: g,
                        ..scene()
                    },
                    false,
                    &mut Vec::new(),
                    &mut Vec::new(),
                );
                card.arm(t0, 9);
                for at in (0..=EPISODE_MS + 100).step_by(50) {
                    ink.clear();
                    card.line_ink_into(Some(t0 + ms(at)), &line, g, true, &mut ink);
                    for c in &ink {
                        let rgb = (u32::from(c.color[0]) << 16)
                            | (u32::from(c.color[1]) << 8)
                            | u32::from(c.color[2]);
                        note(rgb, &|| format!("cell {} at {at} ms", c.col));
                    }
                }
                if least.0 < 4.5 {
                    lows.push(format!(
                        "{palette:?} on {g:06x}: {} at {:.2}:1",
                        least.1, least.0
                    ));
                }
            }
        }
        assert!(
            lows.is_empty(),
            "{} grounds under 4.5:1: {lows:#?}",
            lows.len()
        );
        let aerogel = 0x0092_9292;
        let lifted = (0..=255u8)
            .map(|k| {
                contrast(
                    CardPalette::Cruise.letter(f32::from(k) / 255.0, true),
                    aerogel,
                )
            })
            .fold(f32::MAX, f32::min);
        assert!(
            lifted < 1.5,
            "CONTROL: the lifted letter alone, {lifted:.2}:1"
        );
    }

    /// The ground of every built-in and pack theme that [`dark_ground`] calls dark, by name.
    fn dark_theme_grounds() -> Vec<(String, u32)> {
        use aterm_types::scheme::{builtin, builtin_names, parse_theme_str};
        let rgb =
            |c: aterm_types::Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
        builtin_names()
            .into_iter()
            .filter_map(|n| Some((n.to_owned(), rgb(builtin(n)?.background))))
            .chain(aterm_theme_pack::themes().iter().map(|t| {
                let parsed = parse_theme_str(t.text).expect("a pack theme parses");
                (t.id.to_owned(), rgb(parsed.scheme.background))
            }))
            .filter(|&(_, g)| dark_ground(g))
            .collect()
    }

    /// The grounds [`dark_ground`] calls dark on which black reads at least as well as white: a
    /// grey every few steps from that polarity to the cut, a hue every 30° at mid value, the mid
    /// grounds imported themes bring (Silver Aerogel `#929292`, Unikitty `#ff8cd9`), and
    /// [`RASPBERRY`], on which a glint's flare reaches a light that reads.
    fn mid_grounds() -> Vec<u32> {
        let mut grounds: Vec<u32> = (0..=255u32).step_by(3).map(|v| v * 0x0001_0101).collect();
        grounds
            .extend((0..12u8).map(|k| crate::color_math::hsv2rgb(f32::from(k) * 30.0, 0.6, 0.8)));
        grounds.extend([AEROGEL, UNIKITTY, RASPBERRY]);
        grounds.retain(|&g| dark_ground(g) && contrast(0, g) >= contrast(WHITE, g));
        grounds
    }

    /// The instants a card's stars run through: the settled frame, the ride with its sparkles and
    /// burst every 50 ms, the seated intro's twinkle every 100 ms, and the flow's glints every
    /// step for two periods.
    fn star_clocks() -> Vec<Clock> {
        let mut v = vec![Clock::SETTLED];
        v.extend((0..=RIDE_MS + BURST_MS).step_by(50).map(Clock::riding));
        v.extend((0..EPISODE_MS).step_by(100).map(Clock::seated));
        v.extend(
            (0..2 * GLINT_PERIOD_MS)
                .step_by(FLOW_STEP_MS as usize)
                .map(Clock::flowing),
        );
        v
    }

    /// The stars `s` lays at `clock`: as the frame lays them ([`Emit::each_star`]) when `laid`,
    /// else as its four kinds give them, a dark scene's light unchecked against its ground.
    fn star_calls(s: &CardScene, clock: Clock, laid: bool) -> Vec<StarCall> {
        let geo = Geo::new(s).expect("a scene");
        let cx = Emit::new(s, geo, clock, 7);
        let mut calls = Vec::new();
        let mut lay = |c: StarCall| calls.push(c);
        if laid {
            cx.each_star(&mut lay);
        } else {
            cx.burst_stars(&mut lay);
            cx.glints(&mut lay);
            cx.field(&mut lay);
            cx.shed(&mut lay);
        }
        calls
    }

    /// The dark law scenes, all three palettes and both kitties, on `ground`.
    fn dark_scenes_on(ground: u32) -> Vec<CardScene> {
        scenes()
            .into_iter()
            .filter(|s| s.dark)
            .map(|s| CardScene { ground, ..s })
            .collect()
    }

    /// The contrast a dark scene's star reads on `ground` at a settled star's strength: its light
    /// added at [`STAR_PRINT_FULL`], or its printed ink.
    fn star_reads(c: &StarCall, ground: u32) -> f32 {
        if c.light {
            contrast(
                add_sat(ground, premul_rgb(c.color, STAR_PRINT_FULL as u8)),
                ground,
            )
        } else {
            contrast(c.color, ground)
        }
    }

    /// `l` lightened from `r` along its hue: no channel of `l` under `r`'s, and `r`'s channel
    /// order kept (a channel above another in `r` is not below it in `l`).
    fn lightened_from(l: u32, r: u32) -> bool {
        let ch = |c: u32| [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff];
        let (l, r) = (ch(l), ch(r));
        (0..3).all(|i| l[i] >= r[i] && (0..3).all(|j| r[i] <= r[j] || l[i] >= l[j]))
    }

    /// EVERY DARK BUILT-IN AND PACK THEME KEEPS THE STARS THAT READ AND LIFTS THE REST: on the
    /// ground of every built-in and pack theme [`dark_ground`] calls dark, each star the frame
    /// lays — burst, glints, field and ride sparkles, every palette and both kitties, through the
    /// ride, the seated twinkle and the flow's glints — is the star its kind gives, its light
    /// added unchecked, wherever that light reads [`MARK_CONTRAST`] at a settled star's strength;
    /// each other star, only the Cruise palette's deep hues (its blue, indigo and violet on every
    /// such ground, its red on Solarized Dark), is that star in its light lifted along its hue to
    /// read the floor, still added. NEGATIVE CONTROL: some stars start under the floor (the
    /// burst's blue at 1.74-1.92:1), so the comparison reaches the lift; and on Silver Aerogel's
    /// `#929292` the laid stars are not the unchecked ones, so it sees a floor.
    #[test]
    fn every_dark_builtin_and_pack_theme_keeps_its_stars_or_lifts_them() {
        let grounds = dark_theme_grounds();
        assert!(grounds.len() >= 30, "{} dark themes", grounds.len());
        let clocks = star_clocks();
        let shape = |c: &StarCall| (c.clip, c.centre, c.arm, c.cov, c.gold);
        let (mut stars, mut lifted, mut wrong) = (0usize, 0usize, Vec::new());
        for (name, g) in &grounds {
            for s in dark_scenes_on(*g) {
                for &clock in &clocks {
                    let (laid, raw) = (star_calls(&s, clock, true), star_calls(&s, clock, false));
                    assert_eq!(laid.len(), raw.len(), "{name} at {clock:?}");
                    for (l, r) in laid.iter().zip(&raw) {
                        let right = if star_reads(r, *g) >= MARK_CONTRAST {
                            l == r
                        } else {
                            lifted += 1;
                            s.palette == CardPalette::Cruise
                                && l.light
                                && shape(l) == shape(r)
                                && lightened_from(l.color, r.color)
                                && star_reads(l, *g) >= MARK_CONTRAST
                        };
                        if !right {
                            wrong.push(format!(
                                "{name} {g:06x} {:?} at {clock:?}: {:06x} laid {:06x}",
                                s.palette, r.color, l.color
                            ));
                        }
                    }
                    stars += laid.len();
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "{} stars laid wrong: {wrong:#?}",
            wrong.len()
        );
        assert!(stars > 100_000, "CONTROL: {stars} stars compared");
        assert!(
            lifted > 0,
            "NEGATIVE CONTROL: {lifted} stars start under the floor"
        );
        let floored = dark_scenes_on(AEROGEL)
            .iter()
            .flat_map(|s| clocks.iter().map(move |&c| (s, c)))
            .filter(|&(s, c)| star_calls(s, c, true) != star_calls(s, c, false))
            .count();
        assert!(
            floored > 0,
            "NEGATIVE CONTROL: Silver Aerogel's stars are floored"
        );
    }

    /// THE STARS READ ON EVERY MID GROUND: on every ground [`dark_ground`] calls dark past the
    /// polarity at which black reads better than white — greys up to the cut, mid hues, Silver
    /// Aerogel `#929292`, Unikitty `#ff8cd9`, [`RASPBERRY`] — each star the frame lays reads
    /// [`MARK_CONTRAST`] on it at a settled star's strength, every palette and kitty through the
    /// episode and the flow, and some print; a printed field star still twinkles, its coverage
    /// running under and up to the strength at which it prints its whole ink, which the settled
    /// star prints at.
    /// NEGATIVE CONTROL: the stars' light added unchecked on Silver Aerogel's ground misses the
    /// floor (the burst's blue at 1.15:1), so the grounds reach the case the floor answers.
    #[test]
    fn the_stars_read_on_every_mid_ground() {
        let grounds = mid_grounds();
        assert!(grounds.len() >= 20, "{grounds:06x?}");
        let clocks = star_clocks();
        let (mut lows, mut printed) = (Vec::new(), 0usize);
        for &g in &grounds {
            for s in dark_scenes_on(g) {
                for &clock in &clocks {
                    for c in star_calls(&s, clock, true) {
                        let k = star_reads(&c, g);
                        if k < MARK_CONTRAST {
                            lows.push(format!(
                                "{:?} on {g:06x} at {clock:?}: {:06x} at {k:.2}:1",
                                s.palette, c.color
                            ));
                        }
                        printed += usize::from(!c.light);
                    }
                }
            }
        }
        assert!(
            lows.is_empty(),
            "{} stars under the floor: {lows:#?}",
            lows.len()
        );
        assert!(printed > 0, "CONTROL: {printed} stars printed");
        for s in dark_scenes_on(UNIKITTY) {
            let geo = Geo::new(&s).expect("a scene");
            let field = |clock: Clock| {
                let mut calls = Vec::new();
                Emit::new(&s, geo, clock, 7).field(&mut |c: StarCall| {
                    calls.push(c.laid_on(UNIKITTY));
                });
                calls
            };
            let twinkle: Vec<StarCall> = (0..EPISODE_MS)
                .step_by(50)
                .flat_map(|t| field(Clock::seated(t)))
                .collect();
            assert!(
                !twinkle.is_empty() && twinkle.iter().all(|c| !c.light),
                "{:?}: the field prints",
                s.palette
            );
            let (lo, hi) = twinkle.iter().fold((f32::MAX, 0.0f32), |(lo, hi), c| {
                (lo.min(c.cov), hi.max(c.cov))
            });
            assert!(
                lo < STAR_PRINT_FULL && hi >= STAR_PRINT_FULL,
                "{:?}: the printed field twinkles, coverage {lo}..{hi}",
                s.palette
            );
            assert!(
                field(Clock::SETTLED)
                    .iter()
                    .all(|c| c.cov >= STAR_PRINT_FULL),
                "{:?}: the settled field prints its whole ink",
                s.palette
            );
        }
        let unchecked = dark_scenes_on(AEROGEL)
            .iter()
            .flat_map(|s| clocks.iter().flat_map(|&c| star_calls(s, c, false)))
            .map(|c| star_reads(&c, AEROGEL))
            .fold(f32::MAX, f32::min);
        assert!(
            unchecked < MARK_CONTRAST,
            "NEGATIVE CONTROL: the unchecked light on Silver Aerogel, {unchecked:.2}:1"
        );
    }

    /// THE LINE'S `✦` IS THE CARD'S MARK ON EVERY GROUND ([`line_star`]): every resting letter
    /// ink a line can lead with, every palette, laid as the `✦` — on the ground of every built-in
    /// and pack theme [`dark_ground`] calls dark, its light added unchanged, so those themes' `✦`
    /// is the one it was; on every mid ground ([`MARK_CONTRAST`]'s, as the stars'), reading the
    /// floor, added or laid over; on paper laid over unchanged. NEGATIVE CONTROL: added on Silver
    /// Aerogel, as it was, every letter misses the floor.
    #[test]
    fn the_lines_star_is_the_cards_mark_on_every_ground() {
        let letters = |g: u32| {
            [CardPalette::Cruise, CardPalette::Land, CardPalette::Off]
                .into_iter()
                .flat_map(move |p| (0..=255u8).map(move |k| p.letter_on(f32::from(k) / 255.0, g)))
        };
        let reads = |(c, blend): (u32, DecoBlend), g: u32| match blend {
            DecoBlend::Add => contrast(add_sat(g, c), g),
            DecoBlend::Over => contrast(c, g),
        };
        let (mut seen, mut changed, mut lows) = (0usize, Vec::new(), Vec::new());
        for (name, g) in dark_theme_grounds() {
            for c in letters(g) {
                let laid = line_star(c, g);
                if laid != (c, DecoBlend::Add) {
                    changed.push(format!("{name} {g:06x}: {c:06x} laid {laid:?}"));
                }
                seen += 1;
            }
        }
        for g in mid_grounds() {
            for c in letters(g) {
                let k = reads(line_star(c, g), g);
                if k < MARK_CONTRAST {
                    lows.push(format!("{g:06x}: {c:06x} at {k:.2}:1"));
                }
            }
        }
        assert!(
            changed.is_empty(),
            "{} changed: {changed:#?}",
            changed.len()
        );
        assert!(seen >= 30 * 3 * 256, "CONTROL: {seen} dark-theme letters");
        assert!(lows.is_empty(), "{} under the floor: {lows:#?}", lows.len());
        assert!(
            letters(DAY).all(|c| line_star(c, DAY) == (c, DecoBlend::Over)),
            "on paper, laid over unchanged"
        );
        let added = letters(AEROGEL)
            .map(|c| reads((c, DecoBlend::Add), AEROGEL))
            .fold(0.0f32, f32::max);
        assert!(
            added < MARK_CONTRAST,
            "NEGATIVE CONTROL: added on Silver Aerogel, at best {added:.2}:1"
        );
    }

    /// THE STARS PRINT WHERE NO LIGHT READS: on [`SCARLET`], a dim ground on which white reads
    /// better than black, each star the frame lays, every palette and kitty through the episode
    /// and the flow, is printed and reads [`MARK_CONTRAST`]. NEGATIVE CONTROL: white light added
    /// there at a settled star's strength misses the floor, so no lift reaches it and the
    /// polarity alone would have kept every star light and under the floor.
    #[test]
    fn the_stars_print_where_no_light_reads() {
        let white = add_sat(SCARLET, premul_rgb(WHITE, STAR_PRINT_FULL as u8));
        assert!(
            contrast(WHITE, SCARLET) > contrast(0, SCARLET)
                && contrast(white, SCARLET) < MARK_CONTRAST,
            "NEGATIVE CONTROL: white light on scarlet reads {:.2}:1",
            contrast(white, SCARLET)
        );
        let clocks = star_clocks();
        let (mut stars, mut wrong) = (0usize, Vec::new());
        for s in dark_scenes_on(SCARLET) {
            for &clock in &clocks {
                for c in star_calls(&s, clock, true) {
                    let k = star_reads(&c, SCARLET);
                    if c.light || k < MARK_CONTRAST {
                        wrong.push(format!(
                            "{:?} at {clock:?}: {:06x} light {} at {k:.2}:1",
                            s.palette, c.color, c.light
                        ));
                    }
                    stars += 1;
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "{} stars laid wrong: {wrong:#?}",
            wrong.len()
        );
        assert!(stars > 1_000, "CONTROL: {stars} stars");
    }

    /// A GLINT KEEPS ONE POLARITY THROUGH ITS FLARE: on every mid ground, every palette and kitty
    /// through the episode and the flow, the glints the frame lays ([`Emit::laid_glints`]) are
    /// all added light where their rest light is ([`dark_mark`]), and else all print the rest's
    /// floored ink, both seen. NEGATIVE CONTROL: on [`RASPBERRY`] the glints laid frame by frame
    /// on their flared light ([`StarCall::laid_on`]) take both polarities, so the grounds reach
    /// the flip.
    #[test]
    fn a_glint_keeps_one_polarity_through_its_flare() {
        let clocks = star_clocks();
        let glints = |s: &CardScene, clock: Clock, laid: bool| {
            let cx = Emit::new(s, Geo::new(s).expect("a scene"), clock, 7);
            let mut calls = Vec::new();
            if laid {
                cx.laid_glints(&mut |c: StarCall| calls.push(c));
            } else {
                cx.glints(&mut |c: StarCall| calls.push(c.laid_on(s.ground)));
            }
            calls
        };
        let (mut seen, mut printed, mut wrong) = (0usize, 0usize, Vec::new());
        for g in mid_grounds() {
            for s in dark_scenes_on(g) {
                let rest = dark_mark(s.palette.glint_rest(), g);
                for &clock in &clocks {
                    for c in glints(&s, clock, true) {
                        let right = match rest {
                            DarkMark::Light(_) => c.light,
                            DarkMark::Print(ink) => !c.light && c.color == ink,
                        };
                        if !right {
                            wrong.push(format!("{:?} {g:06x} {clock:?}: {c:?}", s.palette));
                        }
                        seen += 1;
                        printed += usize::from(!c.light);
                    }
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "{} glints laid wrong: {wrong:#?}",
            wrong.len()
        );
        assert!(
            printed > 0 && printed < seen,
            "CONTROL: {printed} of {seen} glints printed"
        );
        let mut polarities = [false; 2];
        for s in dark_scenes_on(RASPBERRY) {
            for &clock in &clocks {
                for c in glints(&s, clock, false) {
                    polarities[usize::from(c.light)] = true;
                }
            }
        }
        assert_eq!(
            polarities, [true; 2],
            "NEGATIVE CONTROL: on raspberry, glints laid frame by frame, printed and light"
        );
    }

    #[test]
    fn serious_mode_is_a_static_border() {
        for s in scenes() {
            let s = CardScene { serious: true, ..s };
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 9);
            let first = frame(&mut card, t0 + ms(40), &s, true);
            assert_eq!(
                card.deadline(t0 + ms(40)),
                None,
                "serious mode ends the episode"
            );
            assert!(first.1.is_empty(), "no kitty, no stars");
            let t = i64::from((s.ch / 14).max(1));
            let cw = i64::from(s.cw);
            let bx0 = (i64::from(s.origin_x) - i64::from(s.pad_left).min(cw / 2)).max(0);
            let bx1 = i64::from(s.origin_x) + (i64::from(s.cols) + 1) * cw;
            let by0 = i64::from(s.origin_y) + i64::from(s.top) * i64::from(s.ch);
            let by1 = by0 + i64::from(s.rows) * i64::from(s.ch);
            for q in &first.0 {
                let (x0, y0) = (i64::from(q.x), i64::from(q.y));
                let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
                let on_rim = y1 <= by0 + t || y0 >= by1 - t || x1 <= bx0 + t || x0 >= bx1 - t;
                assert!(on_rim, "a serious quad off the rim: {q:?}");
            }
            assert!(!first.0.is_empty(), "the border stays");
            assert_eq!(frame(&mut card, t0 + ms(900), &s, true), first, "static");
        }
    }

    #[test]
    fn reduced_motion_is_the_settled_frame_from_the_start() {
        for s in scenes() {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            assert!(
                card.deadline(t0).is_some(),
                "an armed card asks for its frame"
            );
            let still = frame(&mut card, t0 + ms(30), &s, false);
            assert_eq!(still, settled(&s), "animate=false is the settled frame");
            assert_eq!(card.deadline(t0 + ms(30)), None);
            for at in [400, 2000, EPISODE_MS + 500] {
                assert_eq!(frame(&mut card, t0 + ms(at), &s, false), still, "{at} ms");
                assert_eq!(card.deadline(t0 + ms(at)), None, "{at} ms");
            }
        }
    }

    #[test]
    fn an_episode_held_back_before_it_played_is_owed() {
        for held in [2000, EPISODE_MS + 3000] {
            let s = scene();
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            let mut at = 0;
            while at <= held {
                let still = frame(&mut card, t0 + ms(at), &s, false);
                assert_eq!(still, settled(&s), "held {held}: {at} ms");
                at += 100;
            }
            let go = t0 + ms(held + 50);
            let (_, first, fp) = frame(&mut card, go, &s, true);
            assert_eq!(card.deadline(go), Some(go + ms(FRAME_MS)), "held {held}");
            let (_, rest, rest_fp) = settled(&s);
            assert_ne!(fp, rest_fp, "held {held}: the episode plays");
            assert_eq!(
                kitty_left(&first),
                kitty_left(&rest),
                "held {held}: the kitty stays in the seat the glass showed"
            );
            assert!(first.len() > rest.len(), "held {held}: the burst flies");
            let mut shown = shown_at_rest(&s, go);
            shown.arm(go, 7);
            assert_eq!(frame(&mut shown, go, &s, true).1, first, "held {held}");
            let late = go + ms(EPISODE_MS - FRAME_MS);
            frame(&mut card, late, &s, true);
            assert!(card.intro_live(), "held {held}: the intro still runs");
            let done = go + ms(EPISODE_MS);
            assert_ne!(frame(&mut card, done, &s, true).2, rest_fp, "held {held}");
            assert!(
                !card.intro_live() && card.flowing(done),
                "held {held}: then the flow"
            );
        }
    }

    #[test]
    fn a_playing_episode_held_back_keeps_its_clock() {
        let s = scene();
        let t0 = Instant::now();
        let (_, rest, rest_fp) = settled(&s);
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, &s, true);
        let held = frame(&mut card, t0 + ms(1000), &s, false);
        assert_eq!(held.2, rest_fp, "held back: the settled frame");
        assert_eq!(card.deadline(t0 + ms(1000)), None);
        let mut twin = shown_at_rest(&s, t0);
        twin.arm(t0, 7);
        assert_eq!(
            frame(&mut card, t0 + ms(2000), &s, true),
            frame(&mut twin, t0 + ms(2000), &s, true),
            "resumed on the arm's clock"
        );
        frame(&mut card, t0 + ms(2100), &s, false);
        let past = t0 + ms(EPISODE_MS + 100);
        let (_, after, fp) = frame(&mut card, past, &s, true);
        assert!(
            !card.intro_live() && card.flowing(past),
            "past its end it flows"
        );
        assert_ne!(fp, rest_fp, "a flow frame");
        assert_eq!(after, rest, "from the settled colours");
        let step = (EPISODE_MS + 100) / FLOW_STEP_MS;
        assert_eq!(
            card.deadline(past),
            Some(t0 + ms((step + 1) * FLOW_STEP_MS + FLOW_SLACK_MS))
        );
    }

    #[test]
    fn the_flows_first_frame_is_asked_for_until_it_is_drawn() {
        let s = scene();
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        let last = t0 + ms(EPISODE_MS - 10);
        frame(&mut card, last, &s, true);
        assert_eq!(card.deadline(last), Some(last + ms(FRAME_MS)));
        let past = t0 + ms(EPISODE_MS + 250);
        assert_eq!(
            card.deadline(past),
            Some(past),
            "the flow's first frame is owed"
        );
        assert_eq!(card.deadline(past), Some(past), "asked until drawn");
        let (_, _, fp) = frame(&mut card, past, &s, true);
        assert_ne!(fp, settled(&s).2, "a flow frame");
        let k = (EPISODE_MS + 250) / FLOW_STEP_MS;
        let next = t0 + ms((k + 1) * FLOW_STEP_MS + FLOW_SLACK_MS);
        assert_eq!(card.deadline(past), Some(next), "drawn: the next step");
        let mut held = HarnessCard::default();
        held.arm(t0, 7);
        frame(&mut held, t0, &s, true);
        frame(&mut held, t0 + ms(100), &s, false);
        assert_eq!(held.deadline(past), None);
    }

    #[test]
    fn an_unticked_card_stops_asking_after_the_grace() {
        let s = scene();
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        let mut at = 0;
        let mut last = 0;
        while at < EPISODE_MS {
            frame(&mut card, t0 + ms(at), &s, true);
            last = at;
            at += FRAME_MS;
        }
        let end = t0 + ms(EPISODE_MS);
        assert_eq!(
            card.deadline(end),
            Some(end),
            "the flow's first frame is owed"
        );
        let asked: Vec<u64> = (0..10_000)
            .filter(|&k| card.deadline(end + ms(k)).is_some())
            .collect();
        assert_eq!(
            asked,
            (0..last + SETTLE_GRACE_MS - EPISODE_MS).collect::<Vec<_>>(),
            "asked only inside the grace"
        );
        assert_eq!(card.deadline(t0 + ms(last + SETTLE_GRACE_MS)), None);
        let late = end + ms(60_000);
        assert_ne!(frame(&mut card, late, &s, true).2, settled(&s).2);
        assert!(card.flowing(late) && card.deadline(late).is_some_and(|d| d > late));

        let mut rearmed = HarnessCard::default();
        rearmed.arm(t0, 7);
        frame(&mut rearmed, t0, &s, true);
        let t1 = t0 + ms(1000);
        rearmed.arm(t1, 7);
        assert_eq!(rearmed.deadline(t1), Some(t1 + ms(FRAME_MS)));
        let gone = t1 + ms(EPISODE_MS);
        assert_eq!(rearmed.deadline(gone), None, "no frame of its own drawn");
    }

    #[test]
    fn an_unticked_flowing_card_stops_asking_after_the_grace() {
        let s = scene();
        let t0 = Instant::now();
        let mut card = played(&s, t0);
        let drawn = t0 + ms(EPISODE_MS + 300);
        frame(&mut card, drawn, &s, true);
        let step = |at: u64| at / FLOW_STEP_MS;
        let next = |k: u64| t0 + ms((k + 1) * FLOW_STEP_MS + FLOW_SLACK_MS);
        let k = step(EPISODE_MS + 300);
        let asked: Vec<u64> = (0..10_000)
            .filter(|&j| card.deadline(drawn + ms(j)).is_some())
            .collect();
        assert_eq!(asked, (0..SETTLE_GRACE_MS).collect::<Vec<_>>(), "the grace");
        assert!(
            (0..SETTLE_GRACE_MS).all(|j| card.deadline(drawn + ms(j)) == Some(next(k))),
            "one fixed instant"
        );
        let late = drawn + ms(60_000);
        frame(&mut card, late, &s, true);
        assert_eq!(card.deadline(late), Some(next(step(EPISODE_MS + 60_300))));
    }

    #[test]
    fn nothing_to_draw_holds_the_episode_back() {
        let s = scene();
        let (_, rest, _) = settled(&s);
        for (what, blank) in [
            (
                "an inverted clip",
                CardScene {
                    clip: (4000, 0, 0, 4000),
                    ..s
                },
            ),
            (
                "an empty clip",
                CardScene {
                    clip: (0, 0, 0, 0),
                    ..s
                },
            ),
            ("no columns", CardScene { cols: 0, ..s }),
            ("no rows", CardScene { rows: 0, ..s }),
            ("no cell size", CardScene { cw: 0, ..s }),
        ] {
            for later in [2000, EPISODE_MS + 1000] {
                let t0 = Instant::now();
                let mut card = HarnessCard::default();
                card.arm(t0, 7);
                for at in [0, 500, 1000] {
                    let now = t0 + ms(at);
                    assert_eq!(
                        frame(&mut card, now, &blank, true),
                        (Vec::new(), Vec::new(), 0),
                        "{what}: {at} ms"
                    );
                    assert_eq!(card.deadline(now), None, "{what}: owed, no wake");
                }
                let go = t0 + ms(later);
                let drawn = frame(&mut card, go, &s, true);
                let mut fresh = HarnessCard::default();
                fresh.arm(go, 7);
                assert_eq!(drawn, frame(&mut fresh, go, &s, true), "{what}: {later}");
                assert_eq!(
                    kitty_left(&drawn.1),
                    Some(ride_from(&s)),
                    "{what}: {later}: the ride starts"
                );
                assert_ne!(kitty_left(&drawn.1), kitty_left(&rest), "{what}: {later}");
                assert_eq!(card.deadline(go), Some(go + ms(FRAME_MS)), "{what}");
            }
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            frame(&mut card, t0, &s, true);
            assert_eq!(frame(&mut card, t0 + ms(1000), &blank, true).2, 0, "{what}");
            assert_eq!(card.deadline(t0 + ms(1000)), None, "{what}");
            let mut twin = HarnessCard::default();
            twin.arm(t0, 7);
            assert_eq!(
                frame(&mut card, t0 + ms(2000), &s, true),
                frame(&mut twin, t0 + ms(2000), &s, true),
                "{what}: resumed on the arm's clock"
            );
            let done = t0 + ms(EPISODE_MS + 100);
            assert_ne!(frame(&mut card, done, &s, true).2, 0, "{what}");
            assert!(card.flowing(done), "{what}: the flow takes over");
        }
    }

    #[test]
    fn an_owed_episode_lapses_after_its_dwell() {
        let s = scene();
        let (_, rest_over, rest_fp) = settled(&s);
        let blank = CardScene { cols: 0, ..s };
        for (held_by, animate, by) in [("reduced", false, &s), ("nothing to draw", true, &blank)] {
            for (at, plays) in [
                (OWED_MAX_MS - 1, true),
                (OWED_MAX_MS, false),
                (OWED_MAX_MS + 3_600_000, false),
            ] {
                let t0 = Instant::now();
                let mut card = HarnessCard::default();
                card.arm(t0, 7);
                frame(&mut card, t0 + ms(10), by, animate);
                let now = t0 + ms(at);
                let (_, over, fp) = frame(&mut card, now, &s, true);
                let what = format!("{held_by}: paid at {at} ms");
                assert_ne!(fp, rest_fp, "{what}: it moves either way");
                assert_eq!(card.intro_live(), plays, "{what}");
                let want = if plays {
                    now + ms(FRAME_MS)
                } else {
                    t0 + ms((at / FLOW_STEP_MS + 1) * FLOW_STEP_MS + FLOW_SLACK_MS)
                };
                assert_eq!(card.deadline(now), Some(want), "{what}");
                if !plays {
                    assert_eq!(over, rest_over, "{what}: no burst, the settled colours");
                    let after = now + ms(1000);
                    frame(&mut card, after, &s, true);
                    assert!(!card.intro_live() && card.flowing(after), "{what}");
                    card.arm(after, 7);
                    frame(&mut card, after, &s, true);
                    assert!(card.intro_live(), "{what}: a fresh episode");
                }
            }
        }
    }

    #[test]
    fn an_episode_owed_through_the_latchs_longest_shed_still_plays() {
        const LATCH_DWELL_MAX_MS: u64 = 30_000;
        let s = scene();
        let (_, rest, rest_fp) = settled(&s);
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        let mut at = 0;
        while at < LATCH_DWELL_MAX_MS {
            assert_eq!(frame(&mut card, t0 + ms(at), &s, false).2, rest_fp);
            at += 1000;
        }
        let restored = t0 + ms(LATCH_DWELL_MAX_MS + 3 * FRAME_MS);
        let (_, over, fp) = frame(&mut card, restored, &s, true);
        assert_ne!(fp, rest_fp, "the show plays");
        assert_eq!(
            kitty_left(&over),
            kitty_left(&rest),
            "in the seat it was shown in"
        );
        assert!(over.len() > rest.len(), "the burst flies");
        assert_eq!(card.deadline(restored), Some(restored + ms(FRAME_MS)));
    }

    fn trail_reach(under: &[GlowQuad], s: &CardScene) -> Option<(i64, i64)> {
        let inner = Geo::new(s).expect("a scene").inner();
        let inside = under.iter().filter(|q| {
            let (x, y) = (i64::from(q.x), i64::from(q.y));
            x >= inner.x0
                && y >= inner.y0
                && x + i64::from(q.w) <= inner.x1
                && y + i64::from(q.h) <= inner.y1
        });
        let x0 = inside.clone().map(|q| i64::from(q.x)).min()?;
        let x1 = inside.map(|q| i64::from(q.x) + i64::from(q.w)).max()?;
        Some((x0, x1))
    }

    fn whole(s: &CardScene, clock: Clock) -> (Vec<GlowQuad>, Vec<GlowQuad>) {
        let p = parts(s, clock);
        let under = [p.border, p.trail].concat();
        let over = [p.kitty, p.rim, p.burst, p.glints, p.stars, p.sparkles].concat();
        (under, over)
    }

    #[test]
    fn a_kitty_shown_seated_rides_no_more() {
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                kitty_rows,
                ..scene()
            };
            let what = format!("{kitty_rows} rows");
            let (rest_under, rest, _) = settled(&s);
            let (seat, reach) = (kitty_left(&rest), trail_reach(&rest_under, &s));
            assert!(seat.is_some() && reach.is_some(), "{what}");
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            let (under, over, _) = frame(&mut card, t0, &s, true);
            assert_eq!(kitty_left(&over), Some(ride_from(&s)), "{what}: control");
            assert_ne!(trail_reach(&under, &s), reach, "{what}: control");
            frame(&mut card, t0 + ms(EPISODE_MS + 10), &s, true);
            let t1 = t0 + ms(EPISODE_MS + 5000);
            card.arm(t1, 7);
            let first = frame(&mut card, t1, &s, true);
            assert_eq!(
                (first.0.clone(), first.1.clone()),
                whole(&s, Clock::seated(0)),
                "{what}: the seated episode's first frame"
            );
            assert!(
                !parts(&s, Clock::seated(0)).burst.is_empty(),
                "{what}: burst"
            );
            for t in [FRAME_MS, RIDE_MS / 2, RIDE_MS, 3000] {
                let (under, over, _) = frame(&mut card, t1 + ms(t), &s, true);
                assert_eq!(kitty_left(&over), seat, "{what}: {t} ms, seated");
                assert_eq!(trail_reach(&under, &s), reach, "{what}: {t} ms, whole");
            }
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            frame(&mut card, t0, &s, true);
            frame(&mut card, t0 + ms(300), &s, false);
            card.arm(t0 + ms(400), 7);
            for t in [400, 500, 900] {
                let (under, over, _) = frame(&mut card, t0 + ms(t), &s, true);
                assert_eq!(kitty_left(&over), seat, "{what}: resumed at {t} ms");
                assert_eq!(trail_reach(&under, &s), reach, "{what}: resumed at {t} ms");
            }
        }
    }

    #[test]
    fn a_ride_armed_again_on_its_way_carries_on() {
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                kitty_rows,
                ..scene()
            };
            let what = format!("{kitty_rows} rows");
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            frame(&mut card, t0, &s, true);
            let re = 500;
            frame(&mut card, t0 + ms(re), &s, true);
            card.arm(t0 + ms(re), 7);
            let mut last = i64::MIN;
            for t in (re..RIDE_MS + BURST_MS).step_by(FRAME_MS as usize) {
                let (under, over) = {
                    let f = frame(&mut card, t0 + ms(t), &s, true);
                    (f.0, f.1)
                };
                let clock = Clock::intro(t - re, Some(t));
                assert_eq!((under, over.clone()), whole(&s, clock), "{what}: {t} ms");
                let x0 = kitty_left(&over).expect("a kitty");
                assert!(x0 >= last, "{what}: {t} ms, back from {last} to {x0}");
                last = x0;
            }
            assert_ne!(
                whole(&s, Clock::riding(0)).1,
                whole(&s, Clock::riding(re)).1,
                "{what}: control"
            );
            assert!(
                !parts(&s, Clock::intro(RIDE_MS + 10 - re, Some(RIDE_MS + 10)))
                    .burst
                    .is_empty(),
                "{what}: the burst where the ride ends"
            );
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            frame(&mut card, t0, &s, true);
            frame(&mut card, t0 + ms(2500), &s, true);
            let t1 = t0 + ms(2500);
            card.arm(t1, 7);
            let (under, over, _) = frame(&mut card, t1, &s, true);
            assert_eq!(
                (under, over),
                whole(&s, Clock::seated(0)),
                "{what}: run out"
            );
        }
    }

    #[test]
    fn the_kitty_rides_in_along_the_trail() {
        for kitty_rows in [2u8, 3] {
            for dark in [true, false] {
                let s = CardScene {
                    kitty_rows,
                    dark,
                    ..scene()
                };
                let what = format!("{kitty_rows} rows, dark {dark}");
                let g = Geo::new(&s).expect("a scene");
                let seat = kitty_rect(&g, &s).expect("a kitty");
                let (from, dist) = ride_span(&g, &s, &seat).expect("a ride");
                assert_eq!(from + dist, seat.rect.x0, "{what}");
                assert_eq!(s.trail.expect("a trail").1, s.kitty_col.expect("a kitty"));
                let trail_end = seat.rear();
                let inks = KittyInk::new(s.coat, s.eye, s.dark);
                let art = |f: &[&str]| f.iter().map(|r| (*r).to_string()).collect::<Vec<_>>();
                let mut last = i64::MIN;
                for t in (0..RIDE_MS + 4 * FRAME_MS).step_by(10) {
                    let clock = Clock::riding(t);
                    let p = parts(&s, clock);
                    let x0 = kitty_left(&p.kitty).expect("a kitty drawn");
                    assert!(x0 >= last, "{what}: {t} ms, back from {last} to {x0}");
                    last = x0;
                    let at = ridden(&g, &s, seat, clock);
                    assert_eq!(at.rect.y0, seat.rect.y0, "{what}: on the seat's rows");
                    assert_eq!(
                        drawn_kitty(&p.kitty, &at, inks),
                        art(at.art.frame(kitty_frame(clock))),
                        "{what}: {t} ms, whole where the ride has it"
                    );
                    for q in p.kitty.iter().chain(&p.rim).chain(&p.trail) {
                        assert_in_band_and_box(q, &s, &what);
                    }
                    let laid = p
                        .trail
                        .iter()
                        .map(|q| i64::from(q.x) + i64::from(q.w))
                        .max()
                        .unwrap_or(from);
                    if t >= RIDE_MS {
                        assert_eq!(x0, seat.rect.x0, "{what}: {t} ms, seated");
                        assert_eq!(laid, trail_end, "{what}: {t} ms, the whole trail");
                    } else {
                        assert_eq!(
                            laid,
                            trail_end.min(at.rear()),
                            "{what}: {t} ms, to its rear"
                        );
                    }
                }
                let at = |t: u64| kitty_left(&parts(&s, Clock::riding(t)).kitty);
                assert_eq!(at(0), Some(from), "{what}: from the trail's start");
                let half = at(RIDE_MS / 2).expect("a kitty") - from;
                assert!(
                    half * 8 >= dist * 7 - 8,
                    "{what}: {half} of {dist} at half time"
                );
                assert!(at(RIDE_MS / 4).expect("a kitty") < seat.rect.x0, "{what}");
                assert_eq!(at(RIDE_MS), Some(seat.rect.x0), "{what}");
                assert_eq!(
                    kitty_left(&parts(&s, Clock::SETTLED).kitty),
                    Some(seat.rect.x0),
                    "{what}: settled, seated"
                );
            }
        }
        let base = scene();
        let seat = kitty_rect(&Geo::new(&base).expect("a scene"), &base)
            .expect("a kitty")
            .rect
            .x0;
        for (name, s) in [
            (
                "no trail",
                CardScene {
                    trail: None,
                    ..base
                },
            ),
            (
                "a trail from the kitty",
                CardScene {
                    trail: Some((40, 44)),
                    ..base
                },
            ),
        ] {
            let g = Geo::new(&s).expect("a scene");
            assert!(
                ride_span(&g, &s, &kitty_rect(&g, &s).expect("a kitty")).is_none(),
                "{name}"
            );
            assert_eq!(
                kitty_left(&parts(&s, Clock::riding(0)).kitty),
                Some(seat),
                "{name}: seated at once"
            );
        }
    }

    #[test]
    fn the_field_and_the_sparkles_come_up_behind_the_kitty() {
        for kitty_rows in [2u8, 3] {
            for dark in [true, false] {
                let s = CardScene {
                    kitty_rows,
                    dark,
                    ..scene()
                };
                let what = format!("{kitty_rows} rows, dark {dark}");
                let g = Geo::new(&s).expect("a scene");
                let seat = kitty_rect(&g, &s).expect("a kitty");
                let region = star_region(&g, &s).expect("a star region");
                let reach = STAR_RATIOS
                    .iter()
                    .map(|r| i64::from(star_arm_px(star_arm(s.ch as f32, *r))))
                    .max()
                    .unwrap_or(0)
                    + 1;
                let mut shed = 0;
                for t in (0..RIDE_MS + SHED_MS + 4 * FRAME_MS).step_by(10) {
                    let clock = Clock::riding(t);
                    let p = parts(&s, clock);
                    let at = ridden(&g, &s, seat, clock).rect;
                    for q in &p.stars {
                        let (x0, y0) = (i64::from(q.x), i64::from(q.y));
                        let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
                        let on = x0 < at.x1 && at.x0 < x1 && y0 < at.y1 && at.y0 < y1;
                        assert!(!on, "{what}: {t} ms, a star on the kitty {q:?}");
                        let ahead = x0 < seat.rect.x0 - reach && x1 > at.x0;
                        assert!(!ahead, "{what}: {t} ms, a star ahead of the kitty {q:?}");
                    }
                    let laid = trail_reach(&p.trail, &s).map_or(i64::MIN, |r| r.1);
                    for q in &p.glints {
                        let x1 = i64::from(q.x) + i64::from(q.w);
                        assert!(
                            x1 <= at.x0,
                            "{what}: {t} ms, a glint ahead of the kitty {q:?}"
                        );
                        assert!(
                            x1 <= laid,
                            "{what}: {t} ms, a glint past the trail laid {q:?}"
                        );
                    }
                    if t >= RIDE_MS {
                        assert!(p.sparkles.is_empty(), "{what}: {t} ms, the sparkles gone");
                    }
                    shed += p.sparkles.len();
                    for q in &p.sparkles {
                        assert_in_band_and_box(q, &s, &what);
                        let (x0, y0) = (i64::from(q.x), i64::from(q.y));
                        let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
                        assert!(x1 <= at.x0, "{what}: {t} ms, behind the kitty {q:?}");
                        assert!(
                            region.x0 <= x0
                                && x1 <= region.x1
                                && region.y0 <= y0
                                && y1 <= region.y1,
                            "{what}: {t} ms, in the star region {q:?}"
                        );
                    }
                }
                assert!(shed > 0, "{what}: sparkles shed");
                let left_of_seat = |p: &Parts| {
                    p.stars
                        .iter()
                        .filter(|q| i64::from(q.x) + i64::from(q.w) < seat.rect.x0 - reach)
                        .count()
                };
                let up = parts(&s, Clock::riding(RIDE_MS + STAR_REVEAL_MS as u64));
                let settled = parts(&s, Clock::SETTLED);
                assert!(
                    left_of_seat(&settled) > 0,
                    "{what}: a field left of the seat"
                );
                assert_eq!(left_of_seat(&parts(&s, Clock::riding(0))), 0, "{what}");
                assert!(left_of_seat(&up) > 0, "{what}: up after the ride");
                assert!(
                    parts(&s, Clock::riding(0)).glints.is_empty(),
                    "{what}: glints"
                );
                assert!(!up.glints.is_empty(), "{what}: glints up after the ride");
                assert!(settled.sparkles.is_empty(), "{what}: settled, none shed");
            }
        }
        let still = CardScene {
            trail: None,
            ..scene()
        };
        for t in (0..RIDE_MS + SHED_MS).step_by(10) {
            assert!(
                parts(&still, Clock::riding(t)).sparkles.is_empty(),
                "{t} ms"
            );
        }
    }

    #[test]
    fn the_ride_ends_on_the_settled_frame() {
        for kitty_rows in [2u8, 3] {
            for dark in [true, false] {
                let s = CardScene {
                    kitty_rows,
                    dark,
                    ..scene()
                };
                let what = format!("{kitty_rows} rows, dark {dark}");
                let rest = parts(&s, Clock::SETTLED);
                let rects = |qs: &[GlowQuad]| {
                    let mut v: Vec<_> = qs.iter().map(|q| (q.x, q.y, q.w, q.h)).collect();
                    v.sort_unstable();
                    v
                };
                let span = |qs: &[GlowQuad]| {
                    (
                        qs.iter().map(|q| q.x).min(),
                        qs.iter().map(|q| i64::from(q.x) + i64::from(q.w)).max(),
                    )
                };
                let g = Geo::new(&s).expect("a scene");
                let seat = kitty_rect(&g, &s).expect("a kitty");
                let ring = rim_ring(
                    &seat,
                    seat.art.open,
                    rim_px(seat.p, g.t),
                    g.inner().meet(g.clip),
                );
                let lit = |qs: &[GlowQuad]| {
                    coverage(qs)
                        .into_keys()
                        .collect::<std::collections::BTreeSet<_>>()
                };
                for t in (RIDE_MS..BLINK_AT_MS).step_by(10) {
                    let p = parts(&s, Clock::riding(t));
                    assert_eq!(p.kitty, rest.kitty, "{what}: {t} ms");
                    let trail = lit(&p.trail);
                    let off: std::collections::BTreeSet<_> =
                        ring.difference(&trail).copied().collect();
                    assert_eq!(lit(&p.rim), off, "{what}: {t} ms");
                    assert_eq!(span(&p.trail), span(&rest.trail), "{what}: {t} ms");
                }
                let still = parts(&s, Clock::riding(EPISODE_MS - 100));
                assert_eq!(lit(&still.rim), lit(&rest.rim), "{what}: the wave still");
                assert_eq!(rects(&still.kitty), rects(&rest.kitty), "{what}");
                let start = parts(&s, Clock::riding(0));
                assert_ne!(start.kitty, rest.kitty, "{what}");
            }
        }
    }

    #[test]
    fn the_burst_opens_where_the_ride_ends_and_ends() {
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                kitty_rows,
                ..scene()
            };
            let g = Geo::new(&s).expect("a scene");
            let seat = kitty_rect(&g, &s).expect("a kitty").rect;
            let burst = |t: u64| parts(&s, Clock::riding(t)).burst;
            for t in (0..RIDE_MS).step_by(FRAME_MS as usize) {
                assert!(
                    burst(t).is_empty(),
                    "{kitty_rows} rows: none on the ride, {t} ms"
                );
            }
            for t in (RIDE_MS..RIDE_MS + BURST_MS).step_by(FRAME_MS as usize) {
                let b = burst(t);
                if t < RIDE_MS + BURST_MS / 2 {
                    assert!(b.len() >= BURST_STARS * 3, "{kitty_rows} rows: {t} ms");
                } else if b.is_empty() {
                    continue;
                }
                let (x0, x1) = (
                    b.iter().map(|q| i64::from(q.x)).min().unwrap_or(0),
                    b.iter()
                        .map(|q| i64::from(q.x) + i64::from(q.w))
                        .max()
                        .unwrap_or(0),
                );
                let cx = (seat.x0 + seat.x1) / 2;
                assert!(
                    x0 < cx && cx < x1,
                    "{kitty_rows} rows: {t} ms round the seat"
                );
            }
            assert!(
                burst(RIDE_MS + BURST_MS).is_empty(),
                "{kitty_rows} rows: gone"
            );
            assert!(parts(&s, Clock::SETTLED).burst.is_empty(), "settled: none");
            let (_, after, _) = {
                let t0 = Instant::now();
                let mut card = HarnessCard::default();
                card.arm(t0, 7);
                frame(&mut card, t0 + ms(RIDE_MS + BURST_MS + SHED_MS), &s, true)
            };
            let (_, rest, _) = settled(&s);
            assert!(
                after.len() <= rest.len() + STARS_MAX * 4,
                "after the burst only the field is left"
            );
        }
        let still = CardScene {
            trail: None,
            ..scene()
        };
        assert!(!parts(&still, Clock::riding(0)).burst.is_empty());
        assert!(parts(&still, Clock::riding(BURST_MS)).burst.is_empty());
    }

    #[test]
    fn the_field_is_five_to_nine_stars_clear_of_text_and_kitty() {
        let mut counts = std::collections::BTreeSet::new();
        for seed in 0..64 {
            for s in [
                scene(),
                CardScene {
                    dark: false,
                    ..scene()
                },
            ] {
                let g = Geo::new(&s).expect("a scene");
                let (spots, n) = field_spots(&g, &s, seed, !s.dark);
                counts.insert(n);
                let region = star_region(&g, &s).expect("a region");
                let kitty = kitty_rect(&g, &s).expect("a kitty").rect;
                let text_end = g.col(s.trail.expect("trail").0);
                for p in &spots[..n] {
                    assert!(
                        region.contains(p.x, p.y),
                        "a star outside its region: {p:?}"
                    );
                    assert!(p.x >= text_end, "a star over the card's text: {p:?}");
                    assert!(!kitty.contains(p.x, p.y), "a star on the kitty: {p:?}");
                }
            }
        }
        assert!(
            counts.iter().all(|n| (5..=9).contains(n)),
            "counts {counts:?}"
        );
        assert!(counts.len() > 1, "the seed varies the count: {counts:?}");
    }

    fn glint_scenes() -> Vec<CardScene> {
        let mut v = scenes();
        for (cw, ch) in [(9u16, 18u16), (16, 33), (17, 35)] {
            for kitty_rows in [2u8, 3] {
                for dark in [true, false] {
                    v.push(CardScene {
                        cw,
                        ch,
                        kitty_rows,
                        dark,
                        trail: Some((32, 52)),
                        kitty_col: Some(52),
                        cols: 63,
                        ..scene()
                    });
                }
            }
        }
        v
    }

    fn rect_of(q: &GlowQuad) -> Rect {
        Rect {
            x0: i64::from(q.x),
            y0: i64::from(q.y),
            x1: i64::from(q.x) + i64::from(q.w),
            y1: i64::from(q.y) + i64::from(q.h),
        }
    }

    #[test]
    fn the_rainbows_glints_lie_on_its_stripes() {
        let mut counts = std::collections::BTreeSet::new();
        let mut laid = 0;
        for s in glint_scenes() {
            let what = format!("{}x{} {} rows dark {}", s.cw, s.ch, s.kitty_rows, s.dark);
            let g = Geo::new(&s).expect("a scene");
            let seat = kitty_rect(&g, &s);
            let glints = glint_spots(&g, &s, 7);
            let Some((t, amp)) = trail_rect(&g, &s, seat) else {
                assert_eq!(glints.n, 0, "{what}: no trail, no glints");
                continue;
            };
            let free = seat.map_or(t.x1, |at| at.rect.x0.min(t.x1));
            if glints.n > 0 {
                let cells = (free - t.x0) / g.cw;
                let want = usize::try_from(cells / GLINT_CELLS).expect("cells");
                assert_eq!(glints.n, want.clamp(1, GLINTS_MAX), "{what}: the count");
                assert!(glints.rest >= 1 && glints.rest <= glints.flare, "{what}");
            }
            counts.insert(glints.n);
            let band = Rect {
                x0: t.x0.max(g.col(s.trail.expect("a trail").0)),
                y0: t.y0 + amp,
                x1: free,
                y1: t.y1 - amp,
            };
            let clocks = std::iter::once(Clock::SETTLED).chain(
                (0..EPISODE_MS)
                    .step_by(FRAME_MS as usize)
                    .flat_map(|t| [Clock::riding(t), Clock::seated(t)]),
            );
            for clock in clocks {
                let p = parts(&s, clock);
                if clock.t_ms.is_none() {
                    assert_eq!(
                        p.glints.is_empty(),
                        glints.n == 0,
                        "{what}: the settled frame's glints"
                    );
                }
                for q in &p.glints {
                    assert_in_band_and_box(q, &s, &what);
                    let r = rect_of(q);
                    assert_eq!(
                        r.meet(band),
                        r,
                        "{what} at {clock:?}: off the rainbow {q:?}"
                    );
                    if !s.dark {
                        let c = q.color;
                        assert!(
                            q.alpha > 0
                                && (c >> 16) & 0xff == c & 0xff
                                && (c >> 8) & 0xff == c & 0xff,
                            "{what} at {clock:?}: not the paper's white {q:?}"
                        );
                    }
                }
                laid += p.glints.len();
            }
        }
        assert!(laid > 0, "glints laid");
        assert!(
            counts.contains(&1) && counts.contains(&GLINTS_MAX),
            "one on a short trail, the most on a long one: {counts:?}"
        );
        let s = scene();
        let g = Geo::new(&s).expect("a scene");
        let (t, amp) = trail_rect(&g, &s, kitty_rect(&g, &s)).expect("a trail");
        let mut out = Vec::new();
        let sink = Sink { geo: g };
        let edge = (t.x0 + 2 * g.cw, t.y0 + amp);
        lay_star(
            &mut out,
            &sink,
            g.clip,
            edge,
            glint_spots(&g, &s, 7).rest,
            GLINT_COV,
            false,
            WHITE,
            &[],
            true,
        );
        let band = Rect {
            y0: t.y0 + amp,
            y1: t.y1 - amp,
            ..t
        };
        assert!(
            out.iter().any(|q| rect_of(q).meet(band) != rect_of(q)),
            "CONTROL: a glint on the band's edge leaves it"
        );
    }

    #[test]
    fn a_glint_flares_as_the_sheen_passes() {
        for s in glint_scenes()
            .into_iter()
            .filter(|s| s.trail == Some((32, 52)))
        {
            let what = format!("{}x{} {} rows dark {}", s.cw, s.ch, s.kitty_rows, s.dark);
            let g = Geo::new(&s).expect("a scene");
            let glints = glint_spots(&g, &s, 7);
            assert_eq!(glints.n, GLINTS_MAX, "{what}");
            assert!(glints.flare > glints.rest, "{what}: room to flare");
            let reach = glints.flare + 1;
            let read = |p: &Parts, spot: &GlintSpot| {
                let mine: Vec<Rect> = p
                    .glints
                    .iter()
                    .map(rect_of)
                    .filter(|r| (r.x0 - spot.x).abs() <= reach && (r.y0 - spot.y).abs() <= reach)
                    .collect();
                let half = mine
                    .iter()
                    .map(|r| (r.x1 - 1 - spot.x).max(spot.x - r.x0))
                    .max();
                let dots = mine.iter().any(|r| {
                    r.x1 - r.x0 == 1 && r.y1 - r.y0 == 1 && r.x0 != spot.x && r.y0 != spot.y
                });
                (half, dots)
            };
            let settled = parts(&s, Clock::SETTLED);
            for spot in glints.spots() {
                assert_eq!(
                    read(&settled, spot),
                    (Some(glints.rest), false),
                    "{what}: at rest"
                );
                let mut flares = 0;
                let mut was_flaring = false;
                let mut rest_between = false;
                for t in (0..EPISODE_MS).step_by(FRAME_MS as usize / 3) {
                    let (half, dots) = read(&parts(&s, Clock::seated(t)), spot);
                    let half = half.expect("a glint");
                    assert!(
                        (glints.rest..=glints.flare).contains(&half),
                        "{what}: {t} ms, arm {half}"
                    );
                    let flaring = half == glints.flare;
                    if flaring && !was_flaring {
                        flares += 1;
                        assert!(dots, "{what}: {t} ms, a flare throws its glints");
                    }
                    if half == glints.rest && flares > 0 {
                        assert!(!dots, "{what}: CONTROL, {t} ms, at rest, no glints");
                        rest_between = true;
                    }
                    was_flaring = flaring;
                }
                let passes = (EPISODE_MS as f32 / SHIMMER_MS) as usize;
                assert!(
                    flares >= passes - 1 && flares <= passes + 1,
                    "{what}: {flares} flares for {passes} passes"
                );
                assert!(rest_between, "{what}: CONTROL, back at rest between passes");
            }
            let last = parts(&s, Clock::seated(EPISODE_MS - 1));
            let first = parts(&s, Clock::flowing(EPISODE_MS));
            for spot in glints.spots() {
                assert_eq!(
                    read(&last, spot),
                    read(&first, spot),
                    "{what}: no jump into the flow"
                );
            }
        }
    }

    #[test]
    fn a_flowing_glint_flares_as_the_ripple_passes() {
        let chroma = |rgb: u32| {
            let [_, r, g, b] = rgb.to_be_bytes();
            r.max(g).max(b) - r.min(g).min(b)
        };
        for s in glint_scenes()
            .into_iter()
            .filter(|s| s.trail == Some((32, 52)))
        {
            let what = format!("{}x{} {} rows dark {}", s.cw, s.ch, s.kitty_rows, s.dark);
            let g = Geo::new(&s).expect("a scene");
            let glints = glint_spots(&g, &s, 7);
            assert_eq!(glints.n, GLINTS_MAX, "{what}");
            let reach = glints.flare + 1;
            let read = |p: &Parts, spot: &GlintSpot| {
                let mine: Vec<Rect> = p
                    .glints
                    .iter()
                    .map(rect_of)
                    .filter(|r| (r.x0 - spot.x).abs() <= reach && (r.y0 - spot.y).abs() <= reach)
                    .collect();
                let half = mine
                    .iter()
                    .map(|r| (r.x1 - 1 - spot.x).max(spot.x - r.x0))
                    .max()
                    .expect("a glint");
                let dots = mine.iter().any(|r| {
                    r.x1 - r.x0 == 1 && r.y1 - r.y0 == 1 && r.x0 != spot.x && r.y0 != spot.y
                });
                (half, dots)
            };
            let held = parts(&s, Clock::SETTLED);
            let steps: Vec<Parts> = (0..GLINT_PERIOD_MS / FLOW_STEP_MS)
                .map(|k| parts(&s, Clock::flowing(EPISODE_MS + k * FLOW_STEP_MS)))
                .collect();
            for spot in glints.spots() {
                assert_eq!(
                    read(&held, spot),
                    (glints.rest, false),
                    "{what}: CONTROL, the held card's glint at rest"
                );
                let seen: Vec<(i64, bool)> = steps.iter().map(|p| read(p, spot)).collect();
                assert!(
                    seen.iter()
                        .all(|&(half, _)| (glints.rest..=glints.flare).contains(&half)),
                    "{what}: {seen:?}"
                );
                assert!(
                    seen.iter().any(|&(half, dots)| half > glints.rest && dots),
                    "{what}: the crest flares the glint at ({}, {}): {seen:?}",
                    spot.x,
                    spot.y
                );
                assert!(
                    seen.contains(&(glints.rest, false)),
                    "{what}: and it is back at rest off the crest: {seen:?}"
                );
            }
            if s.dark {
                let off = CardScene {
                    palette: CardPalette::Off,
                    ..s
                };
                let starlight = parts(&off, Clock::SETTLED).glints;
                assert!(
                    !starlight.is_empty() && starlight.iter().all(|q| chroma(q.color) < 0x20),
                    "{what}: Off's glints are starlight"
                );
                assert!(
                    held.glints.iter().any(|q| chroma(q.color) >= 0x20),
                    "{what}: CONTROL, Cruise's are gold"
                );
            }
        }
    }

    #[test]
    fn the_field_keeps_clear_of_the_glints() {
        let mut near_trail = 0;
        for seed in 0..200u64 {
            for s in glint_scenes()
                .into_iter()
                .filter(|s| s.cw == 9 && s.trail == Some((32, 52)))
            {
                let g = Geo::new(&s).expect("a scene");
                let glints = glint_spots(&g, &s, seed);
                let (field, n) = field_spots(&g, &s, seed, !s.dark);
                let gap = g.cw.max(g.ch / 2);
                for f in &field[..n] {
                    for o in glints.spots() {
                        assert!(
                            (f.x - o.x).abs() >= gap || (f.y - o.y).abs() >= gap,
                            "seed {seed}: a field star {f:?} on a glint {o:?}"
                        );
                    }
                    let (t, _) = trail_rect(&g, &s, kitty_rect(&g, &s)).expect("a trail");
                    near_trail += usize::from(t.contains(f.x, f.y));
                }
            }
        }
        assert!(
            near_trail > 0,
            "CONTROL: field stars on the trail, beside the glints"
        );
    }

    #[test]
    fn the_dark_star_is_the_family_star() {
        let s = scene();
        let g = Geo::new(&s).expect("a scene");
        let geom = Geom {
            cw: 1,
            ch: usize::from(s.ch),
            rows: 64,
            cols: 4000,
            origin_x: 0,
            origin_y: s.origin_y as u16,
            win_w: 4000,
            win_h: 2000,
            head: 0,
        };
        for (arm, cov, gold) in [(2i64, 235.0f32, false), (3, 200.0, true), (6, 255.0, true)] {
            let (sx, sy) = (g.col(45), g.band(g.top + 1) + 1);
            let mut ours = Vec::new();
            let sink = Sink { geo: g };
            lay_star(
                &mut ours,
                &sink,
                g.clip,
                (sx, sy),
                arm,
                cov,
                gold,
                WHITE,
                &[],
                true,
            );
            let mut family = Vec::new();
            assert!(push_twinkle_star(
                &mut family,
                geom,
                sx as i32,
                sy as i32,
                arm as i32,
                cov as u8,
                gold,
                WHITE,
                4096
            ));
            for q in &mut family {
                q.row += s.row_base;
            }
            assert_eq!(ours, family, "arm {arm} cov {cov} gold {gold}");
        }
    }

    #[test]
    fn the_card_is_deterministic() {
        for s in scenes() {
            let t0 = Instant::now();
            let (mut a, mut b) = (HarnessCard::default(), HarnessCard::default());
            a.arm(t0, 42);
            b.arm(t0, 42);
            let flow = [
                EPISODE_MS + 5,
                EPISODE_MS + 900,
                EPISODE_MS + 3100,
                EPISODE_MS + 6200,
            ];
            for at in [0, 33, 700, 2600].into_iter().chain(flow) {
                assert_eq!(
                    frame(&mut a, t0 + ms(at), &s, true),
                    frame(&mut b, t0 + ms(at), &s, true)
                );
            }
        }
        let s = scene();
        let t0 = Instant::now();
        let (mut a, mut b) = (HarnessCard::default(), HarnessCard::default());
        a.arm(t0, 1);
        b.arm(t0, 2);
        assert_ne!(
            frame(&mut a, t0 + ms(EPISODE_MS), &s, true),
            frame(&mut b, t0 + ms(EPISODE_MS), &s, true)
        );
    }

    fn observed(card: &HarnessCard, at: Instant) -> Vec<String> {
        let mut seen = Vec::new();
        for dt in [
            0,
            1,
            FRAME_MS,
            400,
            EPISODE_MS,
            EPISODE_MS + SETTLE_GRACE_MS + 1,
        ] {
            let t = at + ms(dt);
            seen.push(format!("deadline {dt}: {:?}", card.deadline(t)));
            seen.push(format!(
                "ink {dt}: {:?}",
                card.line_ink(t, 4, 2, 9, NIGHT, true)
            ));
        }
        seen.push(format!(
            "at_rest {} still {}",
            card.at_rest(),
            card.is_still()
        ));
        seen
    }

    fn frame_text(f: &CardFrame) -> String {
        format!(
            "under {:?} {:?} over {:?} {:?} at {:?}",
            f.under.items(),
            f.under.palette(),
            f.over.items(),
            f.over.palette(),
            f.origin
        )
    }

    #[test]
    fn a_card_at_rest_is_replayed_bit_for_bit() {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum Way {
            Draw,
            Held,
            Step,
        }
        use Way::{Draw, Held, Step};
        for s in scenes() {
            let land = CardScene {
                palette: match s.palette {
                    CardPalette::Cruise => CardPalette::Land,
                    CardPalette::Land => CardPalette::Off,
                    CardPalette::Off => CardPalette::Cruise,
                },
                ..s
            };
            let serious = CardScene { serious: true, ..s };
            let t0 = Instant::now();
            let lapse = 3 * EPISODE_MS + OWED_MAX_MS;
            let steps: Vec<(u64, CardScene, bool, bool, Way)> = vec![
                (0, s, true, true, Draw),
                (33, s, true, false, Draw),
                (100, s, false, false, Draw),
                (140, s, false, false, Held),
                (180, s, false, false, Held),
                (220, s, true, false, Draw),
                (2000, s, true, false, Draw),
                (EPISODE_MS + 1, s, true, false, Draw),
                (EPISODE_MS + 10, s, true, false, Step),
                (EPISODE_MS + 40, s, true, false, Draw),
                (EPISODE_MS + 90, s, false, false, Draw),
                (EPISODE_MS + 92, s, false, false, Held),
                (EPISODE_MS + 95, land, true, false, Draw),
                (EPISODE_MS + 99, land, true, false, Step),
                (EPISODE_MS + 120, serious, true, false, Draw),
                (EPISODE_MS + 130, serious, true, false, Held),
                (EPISODE_MS + 140, s, true, false, Draw),
                (EPISODE_MS + 200, s, false, true, Draw),
                (EPISODE_MS + 300, s, false, false, Held),
                (EPISODE_MS + 400, s, false, false, Held),
                (EPISODE_MS + 500, s, true, false, Draw),
                (EPISODE_MS + 533, s, true, false, Draw),
                (3 * EPISODE_MS, s, false, true, Draw),
                (lapse + 10, s, false, false, Held),
                (lapse + 20, s, true, false, Draw),
                (lapse + 30, s, true, false, Step),
                (lapse + 200, s, true, false, Draw),
                (lapse + 210, s, true, false, Step),
            ];
            let (mut twin, mut twin_frames) = (HarnessCard::default(), CardFrames::default());
            let (mut host, mut frames) = (HarnessCard::default(), CardFrames::default());
            let mut last: Option<CardFrame> = None;
            for (at, scene, animate, arm, way) in steps {
                let now = t0 + ms(at);
                if arm {
                    twin.arm(now, 9);
                    host.arm(now, 9);
                }
                let want = twin.draw(now, &scene, animate, &mut twin_frames);
                let before = aterm_core::render::fx_counts_here();
                let mut clock_reads = 0;
                let replayed = match host.replay(&scene, animate, &frames) {
                    Some(f) => Some((f, Held)),
                    None => {
                        clock_reads += 1;
                        host.replay_step(&scene, animate, &frames, now)
                            .map(|f| (f, Step))
                    }
                };
                let spent = aterm_core::render::fx_counts_here();
                assert_eq!(
                    (spent.0 - before.0, spent.1 - before.1),
                    (0, 0),
                    "{at} ms: a replay builds nothing and copies nothing"
                );
                let (got, how) = match replayed {
                    Some((f, how)) => {
                        let prev = last.as_ref().expect("a frame before a replay");
                        assert!(
                            Arc::ptr_eq(&f.under, &prev.under) && Arc::ptr_eq(&f.over, &prev.over),
                            "{at} ms: the kept Arcs themselves"
                        );
                        (f, how)
                    }
                    None => {
                        let f = host.draw(now, &scene, animate, &mut frames);
                        let built = aterm_core::render::fx_counts_here().0 - spent.0;
                        assert!(
                            built > 0 || f.is_empty(),
                            "{at} ms: CONTROL, a draw seals its contents"
                        );
                        (f, Draw)
                    }
                };
                assert_eq!(how, way, "{at} ms: how it reached the glass");
                assert_eq!(
                    clock_reads,
                    usize::from(way != Held),
                    "{at} ms: clock reads"
                );
                assert_eq!(frame_text(&got), frame_text(&want), "{at} ms: the frame");
                assert_eq!(observed(&host, now), observed(&twin, now), "{at} ms: host");
                last = Some(got);
            }
        }
    }

    fn lift_by_bisection(rgb: u32, floor: f32) -> u32 {
        if relative_luminance(rgb) >= floor {
            return rgb;
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..14 {
            let mid = 0.5 * (lo + hi);
            if relative_luminance(mix_rgb(rgb, WHITE, mid)) >= floor {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        mix_rgb(rgb, WHITE, hi)
    }

    fn seams(quads: &[GlowQuad]) -> (Vec<String>, usize) {
        let mut ends = std::collections::HashMap::new();
        for q in quads {
            ends.insert((q.y, q.h, q.x + q.w), (q.color2, q.alpha2));
        }
        let (mut bad, mut checked) = (Vec::new(), 0);
        for q in quads {
            if let Some(&end) = ends.get(&(q.y, q.h, q.x)) {
                checked += 1;
                if end != (q.color, q.alpha) {
                    bad.push(format!("{q:?} starts off {end:?}"));
                }
            }
        }
        (bad, checked)
    }

    #[test]
    fn the_border_and_trail_segments_meet() {
        let mut checked = 0;
        for s in scenes() {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            let flow = (EPISODE_MS + 1..EPISODE_MS + 2 * GLINT_PERIOD_MS)
                .step_by(FLOW_STEP_MS as usize * 5);
            for at in [0, 40, 450, 1300, 2600, 4100].into_iter().chain(flow) {
                let (under, _, _) = frame(&mut card, t0 + ms(at), &s, true);
                let (bad, n) = seams(&under);
                assert!(bad.is_empty(), "at {at} ms: {bad:#?}");
                checked += n;
            }
        }
        assert!(checked > 100, "seams checked: {checked}");
        let (mut under, _, _) = settled(&scene());
        let target = (0..under.len())
            .find(|&i| {
                let q = under[i];
                under
                    .iter()
                    .any(|p| (p.y, p.h, p.x + p.w) == (q.y, q.h, q.x))
            })
            .expect("the settled frame has a seam");
        under[target].color ^= 0x0001_0101;
        assert_eq!(seams(&under).0.len(), 1, "a nudged start is caught");
    }

    #[test]
    fn a_lift_finds_the_bisections_mix() {
        let coarse = |rgb: u32, floor: f32| {
            if relative_luminance(rgb) >= floor {
                return rgb;
            }
            let reaches = |t: f32| relative_luminance(mix_rgb(rgb, WHITE, t)) >= floor;
            let (mut short, mut step) = (0.0f32, 0.25f32);
            for _ in 0..6 {
                let n = [1.0, 2.0, 3.0].map(|j| !reaches(short + j * step));
                short += step * f32::from(n.iter().map(|&s| u8::from(s)).sum::<u8>());
                step *= 0.25;
            }
            mix_rgb(rgb, WHITE, short + 4.0 * step)
        };
        let mut seen_short = 0;
        let mut checked = 0;
        for (floor, every) in [
            (LINE_DARK_LUMA_FLOOR, 5),
            (0.0, 15),
            (0.02, 15),
            (0.18, 15),
            (0.5, 15),
            (0.9, 15),
            (1.0, 15),
            (1.2, 15),
        ] {
            for r in (0..=255u32).step_by(every) {
                for g in (0..=255u32).step_by(every) {
                    for b in (0..=255u32).step_by(every) {
                        let rgb = r << 16 | g << 8 | b;
                        let want = lift_by_bisection(rgb, floor);
                        assert_eq!(lift_to_luminance(rgb, floor), want, "{rgb:06x} to {floor}");
                        seen_short += usize::from(coarse(rgb, floor) != want);
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 140_000, "{checked} colours");
        assert!(seen_short > 0, "CONTROL: a search a round short is seen");
    }

    #[test]
    fn at_rest_means_no_deadline_at_any_instant() {
        let s = scene();
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        assert!(card.at_rest(), "unarmed");
        card.arm(t0, 5);
        assert!(!card.at_rest(), "CONTROL: armed");
        for (at, animate) in [
            (0, true),
            (500, true),
            (EPISODE_MS + 1, true),
            (EPISODE_MS + 2, true),
            (EPISODE_MS + 3, false),
            (EPISODE_MS + 900, true),
        ] {
            let now = t0 + ms(at);
            let _ = frame(&mut card, now, &s, animate);
            assert_eq!(card.at_rest(), !animate, "{at} ms");
            if card.at_rest() {
                for dt in [0, 1, FRAME_MS, EPISODE_MS, 10 * EPISODE_MS] {
                    assert_eq!(card.deadline(now + ms(dt)), None, "{at} ms + {dt}");
                }
            } else {
                assert!(card.deadline(now).is_some(), "CONTROL: {at} ms moves");
            }
        }
        frame(&mut card, t0 + ms(EPISODE_MS + 1000), &s, false);
        assert!(card.at_rest(), "held");
    }

    #[test]
    fn an_empty_scene_draws_nothing() {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 1);
        for s in [
            CardScene { rows: 0, ..scene() },
            CardScene { cols: 0, ..scene() },
            CardScene { ch: 0, ..scene() },
        ] {
            assert_eq!(frame(&mut card, t0, &s, true), (Vec::new(), Vec::new(), 0));
        }
    }

    #[test]
    fn the_line_sweeps_then_settles() {
        let t0 = Instant::now();
        let (row, col, len) = (7u16, 11u16, 64u16);
        for (palette, ground) in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off]
            .into_iter()
            .flat_map(|p| [(p, NIGHT), (p, DAY)])
        {
            let dark = ground == NIGHT;
            let mut card = HarnessCard::default();
            card.tick(
                t0,
                &CardScene {
                    palette,
                    dark,
                    ground,
                    ..scene()
                },
                false,
                &mut Vec::new(),
                &mut Vec::new(),
            );
            card.arm(t0, 3);
            let rest = card.line_ink(t0 + ms(EPISODE_MS), row, col, len, ground, false);
            assert_eq!(rest.len(), usize::from(len));
            for (i, c) in rest.iter().enumerate() {
                assert_eq!(
                    (c.row, c.col),
                    (row, col + i as u16),
                    "sorted, unique, contiguous"
                );
            }
            assert_eq!(
                rest,
                card.line_ink(t0 + ms(EPISODE_MS + 5000), row, col, len, ground, false),
                "settled is constant"
            );
            assert_eq!(
                rest,
                card.line_ink(t0 + ms(900), row, col, len, ground, false),
                "reduced motion is settled"
            );
            let mid = card.line_ink(t0 + ms(900), row, col, len, ground, true);
            let next = card.line_ink(t0 + ms(900 + FRAME_MS), row, col, len, ground, true);
            assert_ne!(mid, rest, "the episode moves the line");
            assert_ne!(mid, next, "frame to frame");
            let flow: Vec<Vec<InkCell>> = (0..GLINT_PERIOD_MS / FLOW_STEP_MS)
                .map(|k| {
                    let at = t0 + ms(EPISODE_MS + 200 + k * FLOW_STEP_MS);
                    card.line_ink(at, row, col, len, ground, true)
                })
                .collect();
            assert!(
                flow.iter().filter(|f| **f != rest).count() > flow.len() / 2,
                "{palette:?}: the flow's glints run along the line"
            );
            assert_ne!(
                rest[0].color,
                rest[rest.len() - 1].color,
                "the settled line is a gradient"
            );
            for c in rest.iter().chain(&mid).chain(flow.iter().flatten()) {
                let rgb = (u32::from(c.color[0]) << 16)
                    | (u32::from(c.color[1]) << 8)
                    | u32::from(c.color[2]);
                if dark {
                    assert!(
                        relative_luminance(rgb) >= LINE_DARK_LUMA_FLOOR - 0.01,
                        "{palette:?}: {rgb:06x} too dark"
                    );
                } else {
                    assert!(
                        contrast(rgb, ground) >= 4.5,
                        "{palette:?}: {rgb:06x} under AA on {ground:06x}: {:.2}:1",
                        contrast(rgb, ground)
                    );
                }
            }
        }
        let mut card = HarnessCard::default();
        card.arm(t0, 3);
        assert_eq!(card.line_ink(t0, 0, u16::MAX - 1, 5, NIGHT, true).len(), 2);
    }

    #[test]
    fn the_lines_hero_word_wears_its_own_rainbow() {
        let t0 = Instant::now();
        let plain = LineAt {
            row: 5,
            col: 13,
            len: 55,
            hero: 0..0,
            wand: Some((10, 3)),
        };
        let at = LineAt {
            hero: 12..17,
            ..plain.clone()
        };
        let rgb = |c: &InkCell| {
            (u32::from(c.color[0]) << 16) | (u32::from(c.color[1]) << 8) | u32::from(c.color[2])
        };
        let ink = |card: &HarnessCard, now: Option<Instant>, at: &LineAt, ground, animate| {
            let mut out = Vec::new();
            card.line_ink_into(now, at, ground, animate, &mut out);
            out
        };
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
            for ground in [NIGHT, DAY] {
                let dark = ground == NIGHT;
                let what = format!("{palette:?} dark {dark}");
                let mut card = HarnessCard::default();
                card.tick(
                    t0,
                    &CardScene {
                        palette,
                        dark,
                        ground,
                        ..scene()
                    },
                    false,
                    &mut Vec::new(),
                    &mut Vec::new(),
                );
                let rest = ink(&card, None, &at, ground, false);
                let bare = ink(&card, None, &plain, ground, false);
                assert_eq!(rest.len(), 55, "{what}");
                for (i, c) in rest.iter().enumerate() {
                    assert_eq!((c.row, c.col), (5, 13 + i as u16), "{what}: contiguous");
                }
                for i in (0..55).filter(|i| !(12..17).contains(i)) {
                    assert_eq!(rest[i], bare[i], "{what}: cell {i} is the line's");
                }
                let hero: Vec<u32> = rest[12..17].iter().map(rgb).collect();
                let changed = (12..17).filter(|&i| rest[i] != bare[i]).count();
                assert!(
                    changed >= 3,
                    "{what}: the hero wears its own ink ({changed})"
                );
                let distinct = hero
                    .iter()
                    .enumerate()
                    .all(|(i, a)| hero[..i].iter().all(|b| b != a));
                assert!(distinct, "{what}: five letters, five colours {hero:06x?}");
                let ch = |c: u32, sh: u32| (c >> sh) & 0xff;
                let red = hero
                    .iter()
                    .any(|&c| ch(c, 16) > ch(c, 8) + 40 && ch(c, 16) > ch(c, 0) + 40);
                let blue = hero
                    .iter()
                    .any(|&c| ch(c, 0) > ch(c, 16) + 30 && ch(c, 0) > ch(c, 8));
                match palette {
                    CardPalette::Cruise => assert!(red && blue, "{what}: red to blue {hero:06x?}"),
                    CardPalette::Land => assert!(!blue, "{what}: a sunset {hero:06x?}"),
                    CardPalette::Off => {}
                }
                for &c in &hero {
                    if dark {
                        assert!(
                            relative_luminance(c) >= LINE_HERO_DARK_LUMA_FLOOR - 0.01,
                            "{what}: {c:06x} under the hero's floor"
                        );
                    } else {
                        assert!(
                            contrast(c, ground) >= 4.5,
                            "{what}: {c:06x} under AA on {ground:06x}"
                        );
                    }
                }
                card.arm(t0, 3);
                assert_eq!(
                    ink(&card, Some(t0 + ms(900)), &at, ground, false),
                    rest,
                    "{what}: reduced motion"
                );
                let mut seen: Vec<Vec<InkCell>> = Vec::new();
                for t in (0..EPISODE_MS / 2).step_by(FRAME_MS as usize) {
                    let hero = ink(&card, Some(t0 + ms(t)), &at, ground, true)[12..17].to_vec();
                    if !seen.contains(&hero) {
                        seen.push(hero);
                    }
                }
                assert!(
                    seen.len() > 20,
                    "{what}: the intro moves it ({})",
                    seen.len()
                );
                let flow = (0..GLINT_PERIOD_MS / FLOW_STEP_MS).any(|k| {
                    let now = t0 + ms(EPISODE_MS + 200 + k * FLOW_STEP_MS);
                    ink(&card, Some(now), &at, ground, true)[12..17] != rest[12..17]
                });
                assert!(flow, "{what}: the flow's glints reach the hero");
            }
        }
    }

    #[test]
    fn the_cards_glints_run_outward_from_the_wand() {
        use crate::ripple::GLINT_CELLS_PER_S;
        let s = scene();
        let g = Geo::new(&s).expect("a scene");
        let (cw, ch) = (g.cw as f32, g.ch as f32);
        let (wx, wy) = (g.ox as f32 + 10.5 * cw, g.band(3) as f32 + 0.5 * ch);
        let at = |ms: u64| Clock {
            t_ms: None,
            ride_ms: None,
            glint: Some((ms, 1.0)),
            flow: None,
        };
        for k in 3..15u64 {
            let ms = k * FLOW_STEP_MS;
            let cx = Emit::new(&s, g, at(ms), 7);
            let front = GLINT_CELLS_PER_S * ms as f32 / 1000.0;
            assert_eq!(
                cx.ring_at(wx + front * cw, wy),
                1.0,
                "{ms} ms: the front, right"
            );
            assert_eq!(
                cx.ring_at(wx - front * cw, wy),
                1.0,
                "{ms} ms: the front, left"
            );
            for off in [-3.0, 3.0] {
                assert!(
                    cx.ring_at(wx + (front + off) * cw, wy) < 0.5,
                    "{ms} ms: {off} cells off the front"
                );
            }
        }
        let four = (4000.0 / GLINT_CELLS_PER_S).round() as u64;
        let cx = Emit::new(&s, g, at(four), 7);
        assert_eq!(
            cx.ring_at(wx, wy + 2.0 * ch),
            1.0,
            "two rows down: four cells"
        );
        assert!(
            cx.ring_at(wx, wy + 3.0 * ch) < 0.5,
            "three rows down: not yet"
        );
        let bare = CardScene { wand: None, ..s };
        let gb = Geo::new(&bare).expect("a scene");
        let inner = gb.inner();
        let ms = 10 * FLOW_STEP_MS;
        let cx = Emit::new(&bare, gb, at(ms), 7);
        let front = GLINT_CELLS_PER_S * ms as f32 / 1000.0;
        assert_eq!(
            cx.ring_at(inner.x0 as f32 + front * cw, inner.y0 as f32),
            1.0,
            "CONTROL: no wand, the inner box's corner"
        );
    }

    #[test]
    fn the_flow_stays_under_the_photosensitivity_bound() {
        const SWING: f32 = 0.1;
        const MINUTE_MS: u64 = 60_000;
        let flashes = |series: &[f32]| {
            let (mut n, mut lo, mut hi, mut rising) = (0, f32::MAX, f32::MIN, true);
            for &v in series {
                if rising {
                    lo = lo.min(v);
                    if v - lo >= SWING {
                        n += 1;
                        rising = false;
                        hi = v;
                    }
                } else {
                    hi = hi.max(v);
                    if hi - v >= SWING {
                        rising = true;
                        lo = v;
                    }
                }
            }
            n
        };
        let lum = |premul: u32| relative_luminance(premul);
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
            let s = CardScene { palette, ..scene() };
            let g = Geo::new(&s).expect("a scene");
            let covering = |quads: &[GlowQuad], x: i64, y: i64| {
                quads
                    .iter()
                    .find(|q| {
                        let (qx, qy) = (i64::from(q.x), i64::from(q.y));
                        (qx..qx + i64::from(q.w)).contains(&x)
                            && (qy..qy + i64::from(q.h)).contains(&y)
                    })
                    .map(|q| q.color)
            };
            let border = (g.col(26) + 2, g.outer.y0);
            let (_, yb) = trail_band(&g, &s);
            let trail = (g.col(36) + 2, yb - 2);
            let line = LineAt {
                row: 6,
                col: 13,
                len: 50,
                hero: 12..17,
                wand: Some((10, 3)),
            };
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            frame(&mut card, t0, &s, true);
            frame(&mut card, t0 + ms(EPISODE_MS - 5), &s, true);
            let (mut b, mut t, mut l) = (Vec::new(), Vec::new(), Vec::new());
            let mut ink = Vec::new();
            for at in (EPISODE_MS..EPISODE_MS + MINUTE_MS).step_by(FLOW_STEP_MS as usize) {
                let now = t0 + ms(at);
                let (under, _, _) = frame(&mut card, now, &s, true);
                b.push(lum(
                    covering(&under, border.0, border.1).expect("the border")
                ));
                t.push(lum(covering(&under, trail.0, trail.1).expect("the trail")));
                ink.clear();
                card.line_ink_into(Some(now), &line, NIGHT, true, &mut ink);
                let c = ink[46].color;
                l.push(lum((u32::from(c[0]) << 16)
                    | (u32::from(c[1]) << 8)
                    | u32::from(c[2])));
            }
            for (what, series) in [("border", &b), ("trail", &t), ("line", &l)] {
                let n = flashes(series);
                let hz = n as f32 * 1000.0 / MINUTE_MS as f32;
                assert!(
                    hz <= TWINKLE_FLASH_BOUND_HZ,
                    "{palette:?} {what}: {n} flashes a minute, {hz} Hz"
                );
                assert!(
                    n as u64 >= MINUTE_MS / (2 * GLINT_PERIOD_MS),
                    "{palette:?} {what}: CONTROL, {n} flashes a minute"
                );
            }
        }
    }

    #[test]
    fn the_line_wears_the_cards_palette() {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        let cruise = card.line_ink(t0, 0, 0, 30, NIGHT, false);
        let mut under = Vec::new();
        let mut over = Vec::new();
        let mut wear = |card: &mut HarnessCard, palette| {
            card.tick(
                t0,
                &CardScene { palette, ..scene() },
                false,
                &mut under,
                &mut over,
            );
            card.line_ink(t0, 0, 0, 30, NIGHT, false)
        };
        let land = wear(&mut card, CardPalette::Land);
        assert_ne!(land, cruise);
        let off = wear(&mut card, CardPalette::Off);
        assert_ne!(off, cruise, "Off is not Cruise");
        assert_ne!(off, land, "Off is not Land");
        for ground in [NIGHT, DAY] {
            let line = card.line_ink(t0, 0, 0, 30, ground, false);
            let steps = LINE_INK_STEPS as f32;
            for (i, c) in line.iter().enumerate() {
                let t = tri(i as f32 / 29.0);
                let t = if dark_ground(ground) {
                    t
                } else {
                    (t * steps).round() / steps
                };
                let want = CardPalette::Off.letter_on(t, ground);
                assert_eq!(
                    c.color,
                    [(want >> 16) as u8, (want >> 8) as u8, want as u8],
                    "on {ground:06x}: cell {i} is the palette's letter ink"
                );
            }
        }
    }

    #[test]
    fn the_sunset_ramp_hits_its_stops() {
        assert_eq!(sunset(0.0), SUNSET[0]);
        assert_eq!(sunset(1.0), SUNSET[3]);
        assert_eq!(sunset(1.0 / 3.0), SUNSET[1]);
        assert_eq!(sunset(f32::NAN), SUNSET[0]);
        assert_eq!(sunset(7.0), SUNSET[3]);
    }

    #[test]
    fn the_moonbow_ramp_hits_its_stops() {
        assert_eq!(moonbow(0.0), WHITE);
        assert_eq!(moonbow(0.0), MOONBOW[0]);
        assert_eq!(moonbow(1.0 / 3.0), MOONBOW[1]);
        assert_eq!(moonbow(2.0 / 3.0), MOONBOW[2]);
        assert_eq!(moonbow(1.0), MOONBOW[3]);
        assert_eq!(moonbow(f32::NAN), MOONBOW[0]);
        assert_eq!(moonbow(-3.0), MOONBOW[0]);
        assert_eq!(moonbow(7.0), MOONBOW[3]);
        assert_eq!(CardPalette::Off.art(0.0, true), WHITE);
        assert_eq!(CardPalette::Off.art(1.0, false), PRINT_OFF[5]);
        assert_eq!(CardPalette::Off.stripe(0, true), WHITE);
        assert_eq!(CardPalette::Off.stripe(5, true), MOONBOW[3]);
        assert_eq!(CardPalette::Off.stripe(5, false), PRINT_OFF[5]);
        for c in MOONBOW {
            assert!(relative_luminance(c) >= 0.41, "{c:06x}");
            assert_eq!(lift_to_luminance(c, LINE_DARK_LUMA_FLOOR), c);
        }
    }

    fn on_rim(q: &GlowQuad, s: &CardScene) -> bool {
        let g = Geo::new(s).expect("a scene");
        let inner = g.inner();
        let (x0, y0) = (i64::from(q.x), i64::from(q.y));
        let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
        y1 <= inner.y0 || y0 >= inner.y1 || x1 <= inner.x0 || x0 >= inner.x1
    }

    fn drawn_kitty(over: &[GlowQuad], at: &KittyAt, inks: KittyInk) -> Vec<String> {
        let (r0, p) = (at.rect, at.p);
        (0..at.art.h() as i64)
            .map(|r| {
                (0..KITTY_W as i64)
                    .map(|c| {
                        let (x, y) = (r0.x0 + c * p + p / 2, r0.y0 + r * p + p / 2);
                        over.iter()
                            .filter(|q| q.alpha == 255)
                            .find(|q| {
                                let (qx, qy) = (i64::from(q.x), i64::from(q.y));
                                (qx..qx + i64::from(q.w)).contains(&x)
                                    && (qy..qy + i64::from(q.h)).contains(&y)
                            })
                            .map_or('.', |q| {
                                char::from(
                                    *KITTY_PALETTE
                                        .iter()
                                        .find(|&&b| inks.of(b) == Some(q.color))
                                        .expect("a palette colour"),
                                )
                            })
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_blink_and_the_flick_come_in_their_windows() {
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                kitty_rows,
                ..scene()
            };
            let g = Geo::new(&s).expect("a scene");
            let at = kitty_rect(&g, &s).expect("a kitty");
            let inks = KittyInk::new(s.coat, s.eye, s.dark);
            let art = |f: &[&str]| f.iter().map(|r| (*r).to_string()).collect::<Vec<_>>();
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            let mut seen = [0usize; 3];
            for t in (0..EPISODE_MS).step_by(10) {
                let (_, over, _) = frame(&mut card, t0 + ms(t), &s, true);
                let at = ridden(&g, &s, at, Clock::riding(t));
                let in_window =
                    |first: u64, period: u64, len: u64| t >= first && (t - first) % period < len;
                let (want, k) = if in_window(BLINK_AT_MS, BLINK_PERIOD_MS, BLINK_MS) {
                    (at.art.blink, 1)
                } else if in_window(FLICK_AT_MS, FLICK_PERIOD_MS, FLICK_MS) {
                    (at.art.flick, 2)
                } else {
                    (at.art.open, 0)
                };
                assert_eq!(drawn_kitty(&over, &at, inks), art(want), "{t} ms");
                seen[k] += 1;
            }
            assert!(seen.iter().all(|&n| n > 0), "every frame drawn: {seen:?}");
            assert_eq!(seen[1], 2 * (BLINK_MS as usize / 10), "two blinks");
            assert_eq!(seen[2], 3 * (FLICK_MS as usize / 10), "three flicks");
            let (_, rest, _) = settled(&s);
            assert_eq!(drawn_kitty(&rest, &at, inks), art(at.art.open), "settled");
        }
    }

    #[test]
    fn a_kitty_that_does_not_fit_is_dropped_and_the_rest_stays() {
        let fits = scene();
        let tight = CardScene { cols: 44, ..fits };
        let g = Geo::new(&tight).expect("a scene");
        assert!(kitty_rect(&g, &tight).is_none(), "the kitty is dropped");
        assert!(kitty_rect(&Geo::new(&fits).expect("a scene"), &fits).is_some());
        for dark in [true, false] {
            let s = CardScene { dark, ..tight };
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            for at in [100, EPISODE_MS + 1] {
                let (under, over, _) = frame(&mut card, t0 + ms(at), &s, true);
                let marks = mark_inks(s.palette);
                assert!(
                    !over
                        .iter()
                        .any(|q| q.alpha == 255 && (dark || !marks.contains(&q.color))),
                    "dark {dark}, {at} ms: no kitty pixel"
                );
                assert!(
                    under.iter().any(|q| on_rim(q, &s)),
                    "dark {dark}: the border"
                );
                assert!(
                    under.iter().any(|q| !on_rim(q, &s)),
                    "dark {dark}: the trail"
                );
                assert!(!over.is_empty(), "dark {dark}, {at} ms: the stars");
                for q in under.iter().chain(&over) {
                    assert_in_band_and_box(q, &s, &format!("dark {dark}, {at} ms"));
                }
            }
        }
    }

    fn gap_to_ramp(color: u32, ramp: impl Fn(f32) -> u32) -> u32 {
        (0..=2000)
            .map(|i| {
                let on = ramp(i as f32 / 2000.0);
                [16, 8, 0]
                    .iter()
                    .map(|sh| ((on >> sh) & 0xff).abs_diff((color >> sh) & 0xff))
                    .max()
                    .unwrap_or(0)
            })
            .min()
            .unwrap_or(u32::MAX)
    }

    #[test]
    fn land_wears_the_sunset_ramp() {
        for dark in [true, false] {
            let land = CardScene {
                palette: CardPalette::Land,
                dark,
                ..scene()
            };
            let border = |t: f32| {
                if dark {
                    light(sunset(t), f32::from(BORDER_COV)).color
                } else {
                    ink(print_line(even_ramp(&PRINT_LAND, t)), PRINT_ALPHA).color
                }
            };
            let stripe = |rgb: u32| {
                if dark {
                    light(rgb, f32::from(TRAIL_COV)).color
                } else {
                    ink(rgb, PRINT_ALPHA).color
                }
            };
            let (under, _, _) = settled(&land);
            let (rim, trail): (Vec<&GlowQuad>, Vec<&GlowQuad>) =
                under.iter().partition(|q| on_rim(q, &land));
            assert!(!rim.is_empty() && !trail.is_empty(), "dark {dark}");
            for q in &rim {
                for c in [q.color, q.color2] {
                    let gap = gap_to_ramp(c, border);
                    assert!(
                        gap <= 2,
                        "dark {dark}: border {c:06x} off the sunset by {gap}"
                    );
                }
            }
            let stops: Vec<u32> = (0..TRAIL_STOPS.len())
                .map(|k| {
                    stripe(if dark {
                        sunset(k as f32 / (TRAIL_STOPS.len() - 1) as f32)
                    } else {
                        PRINT_LAND[k]
                    })
                })
                .collect();
            let worn: std::collections::BTreeSet<u32> =
                trail.iter().flat_map(|q| [q.color, q.color2]).collect();
            assert_eq!(
                worn,
                stops.iter().copied().collect(),
                "dark {dark}: the trail is the ramp's six stops"
            );
            let (red, rose) = if dark {
                (SUNSET[0], SUNSET[3])
            } else {
                (PRINT_LAND[0], PRINT_LAND[5])
            };
            assert_eq!(stops[0], stripe(red), "red on top");
            assert_eq!(stops[5], stripe(rose), "rose at the bottom");
            let cruise = CardScene {
                palette: CardPalette::Cruise,
                ..land
            };
            let (under, _, _) = settled(&cruise);
            let off = under
                .iter()
                .filter(|q| on_rim(q, &cruise))
                .map(|q| gap_to_ramp(q.color, border))
                .max()
                .unwrap_or(0);
            assert!(
                off > 40,
                "dark {dark}: Cruise's border is not a sunset ({off})"
            );
        }
    }

    #[test]
    fn off_wears_the_moonbow() {
        for dark in [true, false] {
            for kitty_rows in [2u8, 3] {
                let off = CardScene {
                    palette: CardPalette::Off,
                    dark,
                    kitty_rows,
                    ..scene()
                };
                let border = |t: f32| {
                    if dark {
                        light(moonbow(t), f32::from(BORDER_COV)).color
                    } else {
                        ink(print_line(even_ramp(&PRINT_OFF, t)), PRINT_ALPHA).color
                    }
                };
                let stripe = |rgb: u32| {
                    if dark {
                        light(rgb, f32::from(TRAIL_COV)).color
                    } else {
                        ink(rgb, PRINT_ALPHA).color
                    }
                };
                let (under, _, _) = settled(&off);
                let (rim, trail): (Vec<&GlowQuad>, Vec<&GlowQuad>) =
                    under.iter().partition(|q| on_rim(q, &off));
                assert!(!rim.is_empty() && !trail.is_empty(), "dark {dark}");
                for q in &rim {
                    for c in [q.color, q.color2] {
                        let gap = gap_to_ramp(c, border);
                        assert!(
                            gap <= 2,
                            "dark {dark}, {kitty_rows} rows: border {c:06x} off the moonbow by {gap}"
                        );
                    }
                }
                let stops: Vec<u32> = (0..TRAIL_STOPS.len())
                    .map(|k| {
                        stripe(if dark {
                            moonbow(k as f32 / (TRAIL_STOPS.len() - 1) as f32)
                        } else {
                            PRINT_OFF[k]
                        })
                    })
                    .collect();
                let worn: std::collections::BTreeSet<u32> =
                    trail.iter().flat_map(|q| [q.color, q.color2]).collect();
                assert_eq!(
                    worn,
                    stops.iter().copied().collect(),
                    "dark {dark}, {kitty_rows} rows: the trail is the moonbow's six stops"
                );
                let top = if dark { WHITE } else { PRINT_OFF[0] };
                assert_eq!(stops[0], stripe(top), "white (pearl in print) on top");
                assert_eq!(
                    stops[5],
                    stripe(if dark { MOONBOW[3] } else { PRINT_OFF[5] }),
                    "steel (graphite in print) at the bottom"
                );
                for other in [CardPalette::Cruise, CardPalette::Land] {
                    let s = CardScene {
                        palette: other,
                        ..off
                    };
                    let (under, _, _) = settled(&s);
                    let off_by = under
                        .iter()
                        .filter(|q| on_rim(q, &s))
                        .map(|q| gap_to_ramp(q.color, border))
                        .max()
                        .unwrap_or(0);
                    assert!(
                        off_by > 40,
                        "dark {dark}: {other:?}'s border is not a moonbow ({off_by})"
                    );
                }
            }
        }
    }

    const PRINT_OFF_MAX_CHROMA: f64 = 0.04;

    fn pearl_on_paper(stripes: &[(u32, u8)]) -> Vec<String> {
        use aterm_types::Rgb;
        use aterm_types::scheme_signature::{chroma_hue, oklab, oklab_distance};
        if stripes.len() != TRAIL_STOPS.len() {
            return vec![format!("{} stripes", stripes.len())];
        }
        let mut breaches = Vec::new();
        let paper = |(c, a): (u32, u8)| {
            let ch = |sh: u32| (((c >> sh) & 0xff) + 255 - u32::from(a)).min(255) as u8;
            Rgb::new(ch(16), ch(8), ch(0))
        };
        let on: Vec<Rgb> = stripes.iter().map(|&s| paper(s)).collect();
        for (k, (&(c, a), rgb)) in stripes.iter().zip(&on).enumerate() {
            if a != 255 {
                breaches.push(format!("stripe {k} ({c:06x}) is not opaque: alpha {a}"));
            }
            let px = (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b);
            let (chroma, _) = chroma_hue(*rgb);
            if chroma > PRINT_OFF_MAX_CHROMA {
                breaches.push(format!(
                    "stripe {k} ({px:06x}) is tinted: chroma {chroma:.3}"
                ));
            }
            let contrast = 1.05 / (relative_luminance(px) + 0.05);
            if contrast < PRINT_MIN_PAPER_CONTRAST {
                breaches.push(format!(
                    "stripe {k} ({px:06x}) vanishes on the paper: {contrast:.2}:1"
                ));
            }
        }
        for i in 0..on.len() {
            for j in i + 1..on.len() {
                let d = oklab_distance(on[i], on[j]);
                if d < PRINT_MIN_STRIPE_DE {
                    breaches.push(format!("stripes {i} and {j} are one colour (dE {d:.3})"));
                }
            }
        }
        for k in 1..on.len() {
            let (above, here) = (oklab(on[k - 1]).0, oklab(on[k]).0);
            if here >= above {
                breaches.push(format!(
                    "stripe {k} is out of order: L {here:.3} under {above:.3}"
                ));
            }
        }
        breaches
    }

    #[test]
    fn the_light_off_trail_is_pearl_print() {
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                dark: false,
                palette: CardPalette::Off,
                kitty_rows,
                ..scene()
            };
            let trail = parts(&s, Clock::SETTLED).trail;
            let mut ends: Vec<(u16, u32, u8)> =
                trail.iter().map(|q| (q.y, q.color, q.alpha)).collect();
            ends.sort_unstable();
            ends.dedup_by_key(|e| (e.1, e.2));
            let stripes: Vec<(u32, u8)> = ends.iter().map(|e| (e.1, e.2)).collect();
            assert_eq!(
                pearl_on_paper(&stripes),
                Vec::<String>::new(),
                "{kitty_rows} rows"
            );
        }
        let lay = |rgb: u32| {
            let p = ink(rgb, PRINT_ALPHA);
            (p.color, p.alpha)
        };
        let count =
            |breaches: &[String], what: &str| breaches.iter().filter(|b| b.contains(what)).count();
        let cruise: Vec<(u32, u8)> = PRINT_CRUISE.iter().map(|&c| lay(c)).collect();
        let cruise = pearl_on_paper(&cruise);
        assert_eq!(count(&cruise, "is tinted"), 6, "{cruise:?}");
        let white = pearl_on_paper(&[lay(WHITE); 6]);
        assert!(breached(&white, "vanishes"), "{white:?}");
        assert!(breached(&white, "one colour"), "{white:?}");
        let moon: Vec<(u32, u8)> = (0..TRAIL_STOPS.len())
            .map(|k| lay(CardPalette::Off.stripe(k, true)))
            .collect();
        let moon = pearl_on_paper(&moon);
        assert_eq!(count(&moon, "vanishes"), 3, "{moon:?}");
        for k in 0..3 {
            assert!(
                breached(&moon, &format!("stripe {k} (")),
                "stripe {k}: {moon:?}"
            );
        }
        let flipped: Vec<(u32, u8)> = PRINT_OFF.iter().rev().map(|&c| lay(c)).collect();
        let flipped = pearl_on_paper(&flipped);
        assert_eq!(count(&flipped, "out of order"), 5, "{flipped:?}");
        let vivid: Vec<(u32, u8)> = PRINT_OFF.iter().map(|&c| lay(c)).collect();
        let vivid = vivid_on_paper(CardPalette::Off, &vivid);
        assert!(breached(&vivid, "washed out"), "{vivid:?}");
    }

    #[test]
    fn a_light_ground_prints_the_rainbow() {
        let mid = Clock::seated(1500 + 750);
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                dark: false,
                kitty_rows,
                ..scene()
            };
            let rest = parts(&s, Clock::SETTLED);
            assert_eq!(
                stripes_top_down(&rest.trail),
                PRINT_CRUISE.to_vec(),
                "{kitty_rows} rows: red on top"
            );
            for q in rest.trail.iter().chain(&rest.border).chain(&rest.rim) {
                assert_eq!((q.alpha, q.alpha2), (255, 255), "{kitty_rows} rows: {q:?}");
            }
            assert!(
                !PRINT_CRUISE.contains(&InkRole::Leading.ink(spectrum_stop(0))),
                "the light-ink law's red is not printed"
            );
            let gloss = parts(&s, mid).trail;
            let mut lifted = [0.0f32; 6];
            for q in &gloss {
                assert_eq!((q.alpha, q.alpha2), (255, 255), "{kitty_rows} rows: {q:?}");
                for c in [q.color, q.color2] {
                    let (gap, k, t) = (0..PRINT_CRUISE.len())
                        .flat_map(|k| (0..=100).map(move |i| (k, i as f32 / 100.0)))
                        .map(|(k, t)| (channel_gap(c, mix_rgb(PRINT_CRUISE[k], WHITE, t)), k, t))
                        .min_by(|a, b| a.0.cmp(&b.0).then(a.2.total_cmp(&b.2)))
                        .expect("inks");
                    assert!(gap <= 2, "{kitty_rows} rows: {c:06x} not an ink's gloss");
                    lifted[k] = lifted[k].max(t);
                }
            }
            let middle = lifted[1..5].iter().copied().fold(0.0, f32::max);
            assert!(
                middle > 0.3,
                "{kitty_rows} rows: the gloss runs ({lifted:?})"
            );
            for edge in [lifted[0], lifted[5]] {
                assert!(
                    edge <= SHIMMER_GAIN * LIGHT_GLINT_EDGE + 0.02 && edge > 0.0,
                    "{kitty_rows} rows: an edge stripe glossed {edge} ({lifted:?})"
                );
            }
        }
    }

    fn channel_gap(a: u32, b: u32) -> u32 {
        [16, 8, 0]
            .iter()
            .map(|sh| ((a >> sh) & 0xff).abs_diff((b >> sh) & 0xff))
            .max()
            .unwrap_or(0)
    }

    fn stripes_top_down(trail: &[GlowQuad]) -> Vec<u32> {
        let mut trail = trail.to_vec();
        trail.sort_by_key(|q| (q.y, q.x));
        let mut worn: Vec<u32> = trail.iter().map(|q| q.color).collect();
        worn.dedup();
        worn
    }

    const PRINT_MIN_SATURATION: f64 = 0.6;

    const PRINT_MIN_CHROMA: f64 = 0.16;

    const PRINT_MIN_STRIPE_DE: f64 = 0.05;

    const PRINT_MIN_PAPER_CONTRAST: f32 = 1.4;

    fn vivid_on_paper(palette: CardPalette, stripes: &[(u32, u8)]) -> Vec<String> {
        use aterm_types::Rgb;
        use aterm_types::scheme_signature::{chroma_hue, oklab_distance};
        if stripes.len() != TRAIL_STOPS.len() {
            return vec![format!("{} stripes", stripes.len())];
        }
        let mut breaches = Vec::new();
        let paper = |(c, a): (u32, u8)| {
            let ch = |sh: u32| (((c >> sh) & 0xff) + 255 - u32::from(a)).min(255) as u8;
            Rgb::new(ch(16), ch(8), ch(0))
        };
        let on: Vec<Rgb> = stripes.iter().map(|&s| paper(s)).collect();
        let mut hues = Vec::new();
        for (k, (&(c, a), rgb)) in stripes.iter().zip(&on).enumerate() {
            if a != 255 {
                breaches.push(format!("stripe {k} ({c:06x}) is not opaque: alpha {a}"));
            }
            let px = (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b);
            let (_, sat, _) = crate::spectrum::spectrum_hsv(px);
            let (chroma, hue) = chroma_hue(*rgb);
            if sat < PRINT_MIN_SATURATION || chroma < PRINT_MIN_CHROMA {
                breaches.push(format!(
                    "stripe {k} ({px:06x}) is washed out: saturation {sat:.2}, chroma {chroma:.3}"
                ));
            }
            let contrast = 1.05 / (relative_luminance(px) + 0.05);
            if contrast < PRINT_MIN_PAPER_CONTRAST {
                breaches.push(format!(
                    "stripe {k} ({px:06x}) vanishes on the paper: {contrast:.2}:1"
                ));
            }
            hues.push(hue);
        }
        for i in 0..on.len() {
            for j in i + 1..on.len() {
                let d = oklab_distance(on[i], on[j]);
                if d < PRINT_MIN_STRIPE_DE {
                    breaches.push(format!("stripes {i} and {j} are one colour (dE {d:.3})"));
                }
            }
        }
        let step = |a: f64, b: f64| (b - a + 540.0).rem_euclid(360.0) - 180.0;
        let peak = match palette {
            CardPalette::Cruise => hues.len() - 1,
            CardPalette::Land => 3,
            CardPalette::Off => hues.len() - 1,
        };
        for k in 1..hues.len() {
            if (k <= peak) != (step(hues[k - 1], hues[k]) > 0.0) {
                breaches.push(format!("stripe {k}'s hue is out of order: {hues:.1?}"));
            }
        }
        breaches
    }

    fn breached(breaches: &[String], what: &str) -> bool {
        breaches.iter().any(|b| b.contains(what))
    }

    #[test]
    fn the_light_trail_is_vivid_print() {
        for palette in [CardPalette::Cruise, CardPalette::Land] {
            for kitty_rows in [2u8, 3] {
                let s = CardScene {
                    dark: false,
                    palette,
                    kitty_rows,
                    ..scene()
                };
                let trail = parts(&s, Clock::SETTLED).trail;
                let mut ends: Vec<(u16, u32, u8)> =
                    trail.iter().map(|q| (q.y, q.color, q.alpha)).collect();
                ends.sort_unstable();
                ends.dedup_by_key(|e| (e.1, e.2));
                let stripes: Vec<(u32, u8)> = ends.iter().map(|e| (e.1, e.2)).collect();
                assert_eq!(
                    vivid_on_paper(palette, &stripes),
                    Vec::<String>::new(),
                    "{palette:?}, {kitty_rows} rows"
                );
            }
        }
        const MAIN_TRAIL_LIGHT_ALPHA: f32 = 212.0;
        let lay = |rgb: u32, alpha: f32| {
            let p = ink(rgb, alpha);
            (p.color, p.alpha)
        };
        for palette in [CardPalette::Cruise, CardPalette::Land] {
            let leading = |alpha: f32| -> Vec<(u32, u8)> {
                (0..TRAIL_STOPS.len())
                    .map(|k| lay(InkRole::Leading.ink(palette.stripe(k, true)), alpha))
                    .collect()
            };
            let main = vivid_on_paper(palette, &leading(MAIN_TRAIL_LIGHT_ALPHA));
            assert!(breached(&main, "not opaque"), "{palette:?}: {main:?}");
            assert!(breached(&main, "washed out"), "{palette:?}: {main:?}");
            let opaque = vivid_on_paper(palette, &leading(PRINT_ALPHA));
            assert!(breached(&opaque, "washed out"), "{palette:?}: {opaque:?}");
            assert!(!breached(&opaque, "not opaque"), "{palette:?}: {opaque:?}");
        }
        let thin: Vec<(u32, u8)> = PRINT_CRUISE
            .iter()
            .map(|&c| lay(c, MAIN_TRAIL_LIGHT_ALPHA))
            .collect();
        let thin = vivid_on_paper(CardPalette::Cruise, &thin);
        assert!(breached(&thin, "not opaque"), "{thin:?}");
        let ramp: Vec<(u32, u8)> = (0..TRAIL_STOPS.len())
            .map(|k| lay(sunset(k as f32 / 5.0), PRINT_ALPHA))
            .collect();
        let ramp = vivid_on_paper(CardPalette::Land, &ramp);
        assert!(breached(&ramp, "2 and 4 are one colour"), "{ramp:?}");
        let flipped: Vec<(u32, u8)> = PRINT_CRUISE
            .iter()
            .rev()
            .map(|&c| lay(c, PRINT_ALPHA))
            .collect();
        let flipped = vivid_on_paper(CardPalette::Cruise, &flipped);
        assert_eq!(
            flipped
                .iter()
                .map(|b| b.contains("out of order"))
                .collect::<Vec<_>>(),
            vec![true; 5],
            "{flipped:?}"
        );
        const LEMON: u32 = 0x00FF_FF00;
        let lemon: Vec<(u32, u8)> = (0..PRINT_CRUISE.len())
            .map(|k| lay(if k == 2 { LEMON } else { PRINT_CRUISE[k] }, PRINT_ALPHA))
            .collect();
        let lemon = vivid_on_paper(CardPalette::Cruise, &lemon);
        assert!(
            lemon.len() == 1 && lemon[0].starts_with("stripe 2 (ffff00) vanishes"),
            "{lemon:?}"
        );
    }

    #[test]
    fn the_light_border_keeps_its_chroma_and_reads_on_white() {
        use aterm_types::Rgb;
        use aterm_types::scheme_signature::chroma_hue;
        const RAMP_MIN_CHROMA: f64 = 0.11;
        let chroma = |c: u32| chroma_hue(Rgb::new((c >> 16) as u8, (c >> 8) as u8, c as u8)).0;
        let n = SPECTRUM_LUT_LEN - 1;
        let low = (0..=n)
            .map(|i| chroma(CardPalette::Cruise.art_at(i as f32 / n as f32, false)))
            .fold(f64::MAX, f64::min);
        assert!(
            low >= RAMP_MIN_CHROMA,
            "the light ramp dips to chroma {low:.3}"
        );
        let plain: Vec<(usize, u32)> = (0..PRINT_CRUISE.len())
            .map(|k| (stripe_at(k), PRINT_CRUISE[k]))
            .collect();
        let plain_low = (0..=n)
            .map(|i| chroma(knot_ramp(&plain, i as f32 / n as f32)))
            .fold(f64::MAX, f64::min);
        assert!(
            plain_low < RAMP_MIN_CHROMA,
            "CONTROL: without the knots the ramp holds {plain_low:.3}"
        );
        let luma = |c: u32| {
            0.2126 * ((c >> 16) & 0xff) as f32
                + 0.7152 * ((c >> 8) & 0xff) as f32
                + 0.0722 * (c & 0xff) as f32
        };
        let on_print = |palette: CardPalette| {
            move |t: f32| ink(print_line(palette.art_at(t, false)), PRINT_ALPHA).color
        };
        let washed = (0..=n)
            .map(|i| {
                let main = InkRole::Leading.ink(CardPalette::Cruise.at(i as f32 / n as f32));
                (
                    gap_to_ramp(ink(main, PRINT_ALPHA).color, on_print(CardPalette::Cruise)),
                    luma(main),
                )
            })
            .fold((0, 0.0), |(g, y), (g1, y1)| (g.max(g1), f32::max(y, y1)));
        assert!(
            washed.0 > 40 && washed.1 <= LIGHT_LINE_MAX_LUMA,
            "CONTROL: main's border ink, opaque, is {} off the print ramp at luma {}",
            washed.0,
            washed.1
        );
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
            for kitty_rows in [2u8, 3] {
                let s = CardScene {
                    dark: false,
                    palette,
                    kitty_rows,
                    ..scene()
                };
                let border = parts(&s, Clock::SETTLED).border;
                assert!(!border.is_empty(), "{palette:?} {kitty_rows} rows");
                for q in &border {
                    for c in [q.color, q.color2] {
                        let gap = gap_to_ramp(c, on_print(palette));
                        assert!(
                            gap <= 2,
                            "{palette:?} {kitty_rows} rows: border {c:06x} off its print ramp by {gap}"
                        );
                    }
                }
                for clock in [
                    Clock::SETTLED,
                    Clock::seated(900),
                    Clock::flowing(600),
                    Clock::flowing(1400),
                    Clock::flowing(2300),
                ] {
                    let p = parts(&s, clock);
                    let line = p
                        .border
                        .iter()
                        .flat_map(|q| [q.color, q.color2])
                        .map(luma)
                        .fold(0.0, f32::max);
                    assert!(
                        line <= LIGHT_LINE_MAX_LUMA + 1.0,
                        "{palette:?} {kitty_rows} rows: the border's luma reaches {line}"
                    );
                }
                let rim = parts(&s, Clock::SETTLED)
                    .rim
                    .iter()
                    .map(|q| luma(q.color))
                    .fold(0.0, f32::max);
                if palette == CardPalette::Cruise {
                    assert!(
                        rim > LIGHT_LINE_MAX_LUMA + 1.0,
                        "{kitty_rows} rows: the rim keeps the full gold ({rim})"
                    );
                }
            }
        }
        let gold = PRINT_CRUISE[2];
        let held = print_line(gold);
        assert!(luma(gold) > LIGHT_LINE_MAX_LUMA && luma(held) <= LIGHT_LINE_MAX_LUMA + 1.0);
        assert!(
            luma(held) >= LIGHT_LINE_MAX_LUMA - 1.0,
            "darkened to the bar, no further"
        );
        let hue = |c: u32| chroma_hue(Rgb::new((c >> 16) as u8, (c >> 8) as u8, c as u8)).1;
        assert!(
            (hue(held) - hue(gold)).abs() < 2.0,
            "the gold keeps its hue"
        );
        assert_eq!(
            print_line(PRINT_CRUISE[4]),
            PRINT_CRUISE[4],
            "blue is under the bar"
        );
    }

    #[test]
    fn a_box_larger_than_its_clip_is_cut_to_it() {
        let whole = scene();
        let cut = clipped();
        let (cx0, cy0, cx1, cy1) = cut.clip;
        let t0 = Instant::now();
        for at in [0, 150, 1300, EPISODE_MS + 1] {
            let (mut a, mut b) = (HarnessCard::default(), HarnessCard::default());
            a.arm(t0, 7);
            b.arm(t0, 7);
            let (wu, wo, _) = frame(&mut a, t0 + ms(at), &whole, true);
            let (cu, co, fp) = frame(&mut b, t0 + ms(at), &cut, true);
            assert_ne!(fp, 0, "{at} ms: the pane's part is drawn");
            for q in cu.iter().chain(&co) {
                assert_in_band_and_box(q, &cut, &format!("{at} ms"));
            }
            let kitty = |v: &[GlowQuad]| -> Vec<GlowQuad> {
                v.iter().filter(|q| q.alpha == 255).copied().collect()
            };
            assert!(!kitty(&co).is_empty(), "{at} ms: the kitty fits the clip");
            assert_eq!(kitty(&wo), kitty(&co), "{at} ms");
            let inside = |q: &GlowQuad| {
                i32::from(q.x) >= cx0
                    && i32::from(q.x) + i32::from(q.w) <= cx1
                    && i32::from(q.y) >= cy0
                    && i32::from(q.y) + i32::from(q.h) <= cy1
            };
            for q in wu.iter().filter(|q| inside(q)) {
                assert!(cu.contains(q), "{at} ms: {q:?} lost inside the clip");
            }
            assert!(
                wu.iter().any(|q| !inside(q)),
                "{at} ms: the whole box reaches past the clip"
            );
        }
        let g = Geo::new(&whole).expect("a scene");
        let (cu, _, _) = settled(&cut);
        assert!(
            cu.iter()
                .all(|q| i64::from(q.x) + i64::from(q.w) < g.outer.x1 - g.t)
        );
        let (wu, _, _) = settled(&whole);
        let top = g.outer.y0;
        let edge = |q: &&GlowQuad| i64::from(q.y) == top && i32::from(q.x) < cx1;
        let cut_seg = cu
            .iter()
            .filter(edge)
            .find(|q| i32::from(q.x) + i32::from(q.w) == cx1)
            .expect("a top segment cut at the clip");
        let whole_seg = wu
            .iter()
            .filter(edge)
            .find(|q| q.x == cut_seg.x && i32::from(q.x) + i32::from(q.w) > cx1)
            .expect("the same segment drawn whole");
        assert_eq!(cut_seg.color, whole_seg.color, "the left end stays");
        for sh in [16, 8, 0] {
            let ch = |c: u32| (c >> sh) & 0xff;
            let (a, b) = (ch(whole_seg.color), ch(whole_seg.color2));
            let at = ch(cut_seg.color2);
            assert!(
                a.min(b) <= at && at <= a.max(b),
                "the cut end {:06x} is on the ramp {:06x} → {:06x}",
                cut_seg.color2,
                whole_seg.color,
                whole_seg.color2
            );
        }
        for clip in [(0, 0, 5, 5), (cx1, cy0, cx0, cy1), (0, 0, 0, 0)] {
            let s = CardScene { clip, ..whole };
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            assert_eq!(
                frame(&mut card, t0 + ms(100), &s, true),
                (Vec::new(), Vec::new(), 0),
                "{clip:?}"
            );
        }
    }

    fn coverage(quads: &[GlowQuad]) -> std::collections::BTreeMap<(i64, i64), usize> {
        let mut seen = std::collections::BTreeMap::new();
        for q in quads {
            for y in i64::from(q.y)..i64::from(q.y) + i64::from(q.h) {
                for x in i64::from(q.x)..i64::from(q.x) + i64::from(q.w) {
                    *seen.entry((x, y)).or_insert(0) += 1;
                }
            }
        }
        seen
    }

    fn covers(q: &GlowQuad, (x, y): (i64, i64)) -> bool {
        let (x0, y0) = (i64::from(q.x), i64::from(q.y));
        (x0..x0 + i64::from(q.w)).contains(&x) && (y0..y0 + i64::from(q.h)).contains(&y)
    }

    fn overlap(a: &GlowQuad, b: &GlowQuad) -> bool {
        let meet = |p0: u16, n0: u16, p1: u16, n1: u16| {
            let (lo, hi) = (
                p0.max(p1),
                (u32::from(p0) + u32::from(n0)).min(u32::from(p1) + u32::from(n1)),
            );
            u32::from(lo) < hi
        };
        meet(a.x, a.w, b.x, b.w) && meet(a.y, a.h, b.y, b.h)
    }

    fn rim_ring(
        at: &KittyAt,
        frame: &[&str],
        rim: i64,
        inside: Rect,
    ) -> std::collections::BTreeSet<(i64, i64)> {
        let p = at.p;
        let cell = |x: i64, y: i64| {
            let (dx, dy) = (x - at.rect.x0, y - at.rect.y0);
            (dx >= 0 && dy >= 0).then(|| (dy / p, dx / p))
        };
        let solid = |x: i64, y: i64| cell(x, y).is_some_and(|(r, c)| rim_solid(frame, r, c));
        let notch = |r: i64, c: i64| rim_solid(frame, r, c) && !art_at(frame, r, c);
        let by_a_notch = |x: i64, y: i64| {
            let c = (x - at.rect.x0).div_euclid(p);
            let r = (y - at.rect.y0).div_euclid(p);
            notch(r - 1, c) || notch(r + 1, c)
        };
        let mut ring = std::collections::BTreeSet::new();
        for y in at.rect.y0 - rim..at.rect.y1 + rim {
            for x in at.rect.x0 - rim..at.rect.x1 + rim {
                if solid(x, y) || !inside.contains(x, y) || by_a_notch(x, y) {
                    continue;
                }
                let near = (-rim..=rim).any(|dy| (-rim..=rim).any(|dx| solid(x + dx, y + dy)));
                if near {
                    ring.insert((x, y));
                }
            }
        }
        ring
    }

    fn rings_once(
        quads: &[GlowQuad],
        ring: &std::collections::BTreeSet<(i64, i64)>,
    ) -> Result<(), String> {
        let lit = coverage(quads);
        if let Some((px, n)) = lit.iter().find(|(_, n)| **n > 1) {
            return Err(format!("{px:?} lit {n} times"));
        }
        let lit: std::collections::BTreeSet<_> = lit.into_keys().collect();
        if let Some(px) = lit.difference(ring).next() {
            return Err(format!("{px:?} lit off the ring"));
        }
        if let Some(px) = ring.difference(&lit).next() {
            return Err(format!("{px:?} on the ring left dark"));
        }
        Ok(())
    }

    #[test]
    fn the_rim_rings_the_silhouette_once() {
        let mut checked = 0;
        let mut on_trail = 0;
        for (cw, ch) in [(9u16, 18u16), (8, 16), (16, 33), (17, 35), (20, 44)] {
            for kitty_rows in [2u8, 3] {
                for dark in [true, false] {
                    let s = CardScene {
                        cw,
                        ch,
                        kitty_rows,
                        dark,
                        kitty_col: Some(44),
                        trail: Some((32, 44)),
                        ..scene()
                    };
                    let g = Geo::new(&s).expect("a scene");
                    let at = kitty_rect(&g, &s).expect("a kitty");
                    let rim = rim_px(at.p, g.t);
                    assert!(rim >= 1, "{ch} px: a rim");
                    for (name, t_ms) in [
                        ("open", None),
                        ("blink", Some(BLINK_AT_MS + 10)),
                        ("flick", Some(FLICK_AT_MS + 10)),
                    ] {
                        let clock = t_ms.map_or(Clock::SETTLED, Clock::riding);
                        let frame = at.art.frame(kitty_frame(clock));
                        let at = ridden(&g, &s, at, clock);
                        let drawn = parts(&s, clock);
                        let quads = drawn.rim;
                        let mut ring = rim_ring(&at, frame, rim, g.inner().meet(g.clip));
                        let trail = coverage(&drawn.trail);
                        let before = ring.len();
                        ring.retain(|px| !trail.contains_key(px));
                        on_trail += before - ring.len();
                        let what = format!("{cw}x{ch}, {kitty_rows} rows, dark {dark}, {name}");
                        rings_once(&quads, &ring).unwrap_or_else(|e| panic!("{what}: {e}"));
                        for q in &quads {
                            assert_in_band_and_box(q, &s, &what);
                            if dark {
                                assert_eq!((q.alpha, q.alpha2), (0, 0), "{what}: light");
                            } else {
                                assert_eq!((q.alpha, q.alpha2), (255, 255), "{what}: print {q:?}");
                            }
                        }
                        let hues: std::collections::BTreeSet<u32> =
                            quads.iter().map(|q| q.color).collect();
                        assert!(hues.len() > 3, "{what}: a spectrum, {} hues", hues.len());
                        for c in &hues {
                            let gap = gap_to_ramp(*c, |t| {
                                if dark {
                                    light(spectrum(t), f32::from(RIM_COV)).color
                                } else {
                                    ink(CardPalette::Cruise.art_at(t, false), PRINT_ALPHA).color
                                }
                            });
                            assert!(gap <= 2, "{what}: {c:06x} off the spectrum by {gap}");
                        }
                        let mut short = quads.clone();
                        short.pop();
                        assert!(rings_once(&short, &ring).is_err(), "{what}: CONTROL short");
                        let mut twice = quads.clone();
                        twice.push(quads[0]);
                        assert!(rings_once(&twice, &ring).is_err(), "{what}: CONTROL twice");
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 5 * 2 * 2 * 3);
        assert!(on_trail > 0, "the tail's ring meets the trail somewhere");
        let tiny = CardScene {
            cw: 8,
            ch: 14,
            ..scene()
        };
        assert_eq!(kitty_px(14), 1);
        assert!(
            parts(&tiny, Clock::SETTLED).rim.is_empty(),
            "no rim at one-pixel art"
        );
    }

    #[test]
    fn the_rim_never_lies_on_the_trail() {
        for (cw, ch) in [(9u16, 18u16), (16, 33), (20, 44)] {
            for kitty_rows in [2u8, 3] {
                for dark in [true, false] {
                    let s = CardScene {
                        cw,
                        ch,
                        kitty_rows,
                        dark,
                        kitty_col: Some(44),
                        trail: Some((32, 44)),
                        ..scene()
                    };
                    let what = format!("{cw}x{ch}, {kitty_rows} rows, dark {dark}");
                    let clocks = (0..EPISODE_MS)
                        .step_by(FRAME_MS as usize)
                        .flat_map(|t| [Clock::riding(t), Clock::seated(t)])
                        .chain([Clock::SETTLED]);
                    for clock in clocks {
                        let p = parts(&s, clock);
                        let both = p
                            .rim
                            .iter()
                            .flat_map(|r| p.trail.iter().map(move |t| (r, t)))
                            .find(|(r, t)| overlap(r, t));
                        assert_eq!(both, None, "{what}: {clock:?}");
                    }
                    let g = Geo::new(&s).expect("a scene");
                    let seat = kitty_rect(&g, &s).expect("a kitty");
                    let flick = Clock::seated(FLICK_AT_MS + 10);
                    let ring = rim_ring(
                        &seat,
                        seat.art.frame(kitty_frame(flick)),
                        rim_px(seat.p, g.t),
                        g.inner(),
                    );
                    let trail = parts(&s, flick).trail;
                    assert!(
                        ring.iter().any(|&px| trail.iter().any(|q| covers(q, px))),
                        "{what}: CONTROL"
                    );
                }
            }
        }
    }

    #[test]
    fn the_toes_gaps_open_onto_the_ground() {
        let (mut gaps, mut paws) = (0, 0);
        for (cw, ch) in [(9u16, 18u16), (16, 33), (20, 44)] {
            for kitty_rows in [2u8, 3] {
                for dark in [true, false] {
                    let s = CardScene {
                        cw,
                        ch,
                        kitty_rows,
                        dark,
                        ..scene()
                    };
                    let g = Geo::new(&s).expect("a scene");
                    let seat = kitty_rect(&g, &s).expect("a kitty");
                    let (p, rim) = (seat.p, rim_px(seat.p, g.t));
                    for clock in [
                        Clock::SETTLED,
                        Clock::seated(BLINK_AT_MS + 10),
                        Clock::seated(FLICK_AT_MS + 10),
                    ] {
                        let frame = seat.art.frame(kitty_frame(clock));
                        let lit = coverage(&parts(&s, clock).rim);
                        let what = format!("{cw}x{ch}, {kitty_rows} rows, dark {dark}");
                        let under = |r: i64, c: i64| {
                            let y = seat.rect.y0 + (r + 1) * p;
                            (seat.rect.x0 + c * p..seat.rect.x0 + (c + 1) * p)
                                .flat_map(move |x| (y..y + rim).map(move |y| (x, y)))
                        };
                        for (r, row) in frame.iter().enumerate() {
                            let r = r as i64;
                            for c in 0..row.len() as i64 {
                                if art_at(frame, r, c) || !rim_solid(frame, r, c) {
                                    continue;
                                }
                                gaps += 1;
                                assert!(
                                    under(r, c).all(|px| !lit.contains_key(&px)),
                                    "{what}: the gap at ({r}, {c}) is lined beneath"
                                );
                                for side in [c - 1, c + 1] {
                                    if rim_solid(frame, r + 1, side) {
                                        continue;
                                    }
                                    paws += 1;
                                    assert!(
                                        under(r, side).all(|px| lit.contains_key(&px)),
                                        "{what}: CONTROL, the paw at ({r}, {side})"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(
            gaps > 0 && paws > 0,
            "CONTROL: {gaps} gaps, {paws} paws beside them"
        );
    }

    #[test]
    fn the_trail_spans_the_kittys_body() {
        for kitty_rows in [2u8, 3] {
            for (cw, ch) in [(9u16, 18u16), (16, 33), (17, 35)] {
                let s = CardScene {
                    cw,
                    ch,
                    kitty_rows,
                    kitty_col: Some(44),
                    trail: Some((32, 44)),
                    ..scene()
                };
                let g = Geo::new(&s).expect("a scene");
                let at = kitty_rect(&g, &s).expect("a kitty");
                let (b0, b1) = at.body();
                let what = format!("{cw}x{ch}, {kitty_rows} rows");
                let settled = parts(&s, Clock::SETTLED).trail;
                let y0 = settled
                    .iter()
                    .map(|q| i64::from(q.y))
                    .min()
                    .expect("a trail");
                let y1 = settled
                    .iter()
                    .map(|q| i64::from(q.y) + i64::from(q.h))
                    .max()
                    .expect("a trail");
                assert_eq!((y0, y1), (b0, b1), "{what}: the body band");
                assert!(
                    at.rect.y0 <= y0 && y1 <= at.rect.y1,
                    "{what}: inside the cat's height"
                );
                assert!(
                    y1 - y0 < 2 * i64::from(ch) * 3 / 4,
                    "{what}: narrower than two rows"
                );
                let x0 = g.col(32);
                assert!(
                    settled
                        .iter()
                        .all(|q| i64::from(q.x) == x0
                            && i64::from(q.x) + i64::from(q.w) == at.rear()),
                    "{what}: each stripe one quad from the trail's start to the kitty's rear"
                );
                for r in at.art.body.0..at.art.body.0 + at.art.body.1 {
                    let first = at.art.open[r].find(|c| c != '.').expect("a body row") as i64;
                    let x = at.rect.x0 + first * at.p - 1;
                    let mut rows = at.rect.y0 + r as i64 * at.p..at.rect.y0 + (r as i64 + 1) * at.p;
                    assert!(
                        rows.all(|y| settled.iter().any(|q| covers(q, (x, y)))),
                        "{what}: ground between the trail and the art's row {r}"
                    );
                }
                let crossings = (g.top + 1..g.top + 3)
                    .filter(|&r| (y0 + 1..y1).contains(&g.band(r)))
                    .count();
                assert!(
                    settled.len() <= TRAIL_STOPS.len() + crossings,
                    "{what}: {} quads, six stripes and {crossings} band crossings",
                    settled.len()
                );
                let colours: std::collections::BTreeSet<u32> =
                    settled.iter().map(|q| q.color).collect();
                assert_eq!(colours.len(), TRAIL_STOPS.len(), "{what}: six stripes");
                for t in [100, 1500, 3000, WAVE_END_MS + 50] {
                    let ep = parts(&s, Clock::riding(t)).trail;
                    assert!(
                        ep.len() > settled.len(),
                        "{what}: CONTROL, segments at {t} ms"
                    );
                    let amp = (i64::from(ch) / 16).max(1);
                    let under_cat = ridden(&g, &s, at, Clock::riding(t)).rect.x0;
                    for q in &ep {
                        assert!(
                            i64::from(q.y) >= b0 - amp
                                && i64::from(q.y) + i64::from(q.h) <= b1 + amp,
                            "{what}: {q:?} strays from the band at {t} ms"
                        );
                        if i64::from(q.x) + i64::from(q.w) > under_cat {
                            assert!(
                                i64::from(q.y) >= b0 && i64::from(q.y) + i64::from(q.h) <= b1,
                                "{what}: {q:?} steps under the kitty at {t} ms"
                            );
                        }
                    }
                }
            }
        }
    }
}
