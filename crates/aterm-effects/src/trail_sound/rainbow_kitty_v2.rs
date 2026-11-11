// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The music box: Rainbow Kitty v2's instrument and its melody engine.

use super::glyph_class::{
    BANG, CLOSE, COLON, COMMA, DASH, DIGIT, LETTER, LINE, MATH, OPEN, QMARK, QUOTE, RISE, SECRET,
    SEMI, SIGIL, STOP,
};
use super::{
    ERASE_MIN_GAP, EventMeta, HELD_ERASE_RUN_WINDOW, OutputGesture, PAN_LAW_SCALE, Partial,
    SONG_FORM, SONG_THEME, SoundEvent, SoundGesture, SoundKind, TrailSynth, Voice, pan_gains,
    penta,
};
use crate::rainbow_kitty::meteor::tri;
use crate::rainbow_kitty::timing;

/// The identity lane: a voice tagged 0 bypasses lane bookkeeping entirely (every voice the nine pinned palettes build).
pub(super) const LANE_NONE: u8 = 0;
/// The verse: steps, re-strikes, the nav tick, the meteor bell, the cadence's pickup and resolution.
pub(super) const LANE_TUNE: u8 = 1;
/// The bloom (§3.3): a lit key's slow-attacking 3f/4f/6f voice, the motif's answering voice, and the air taps after a line end or rest.
pub(super) const LANE_BLOOM: u8 = 2;
/// The word downbeat's dyad, the meteor's thump, the cadence's tonic dyad.
pub(super) const LANE_BASS: u8 = 3;
/// The space's breath.
pub(super) const LANE_BREATH: u8 = 4;
/// Stardust glints.
pub(super) const LANE_GLINT: u8 = 5;
/// The meteor's tick, core and whoosh.
pub(super) const LANE_METEOR: u8 = 6;
/// The meteor's five falling glints.
pub(super) const LANE_RAIN: u8 = 7;
/// The Enter cadence's faraway ice bell.
pub(super) const LANE_CADENCE: u8 = 8;
/// The PTY line-feed brrrring (D18).
pub(super) const LANE_CASCADE: u8 = 9;
/// Grafts: voices that hang off a key without being a step (the shifted key's ring, the `?` rise, the bracket tink, the `=` fifth; §10.4).
pub(super) const LANE_GRAFT: u8 = 10;
/// The ting: the bare Shift's bell ([`TING_LEVEL`]), alone.
pub(super) const LANE_TING: u8 = 11;

/// Fade-steal / damp ramp, seconds. §9.5 law 2: a damp is a ramp, never a cut.
pub(super) const LANE_FADE_STEAL_S: f32 = super::SPACE_DAMP_S;

const TAIL_DUR_PER_TAU: f32 = 2.7;

const SAME_PITCH_HZ: f32 = 0.5;
const SAME_PITCH_CENTS: f32 = 1.0;

/// The age guard, seconds (§14): a voice younger than this has not delivered its attack;
/// [`lane_drops_the_newcomer`] decides who loses.
pub(super) const LANE_AGE_GUARD_S: f32 = 0.040;

/// §14's cap table.
#[inline]
pub(super) const fn lane_cap(lane: u8) -> usize {
    match lane {
        LANE_TUNE | LANE_CASCADE => 4,
        LANE_BLOOM | LANE_GRAFT => 2,
        LANE_GLINT | LANE_METEOR | LANE_RAIN => 3,
        LANE_BASS | LANE_BREATH | LANE_CADENCE | LANE_TING => 1,
        _ => 1,
    }
}

/// Who loses when a full lane's oldest voice is younger than [`LANE_AGE_GUARD_S`].
#[inline]
pub(super) const fn lane_drops_the_newcomer(lane: u8) -> bool {
    matches!(lane, LANE_GLINT | LANE_BLOOM)
}

#[inline]
const fn stride_mag(r: i32, repeated: bool) -> i32 {
    if repeated {
        return 0;
    }
    match r.abs() {
        0 => 1,
        1..=3 => 1,
        4..=5 => 2,
        _ => 3,
    }
}

const ACCEL_SHARE: f32 = 0.75;
const DECEL_SHARE: f32 = 1.35;

const MELODY_CENTRE_DEG: i32 = 3;
const MELODY_GRAVITY_DEG: i32 = 3;

const MELODY_RUN_MAX: u8 = 3;

const WORD_HEAD_SNAP_DEG: i32 = 2;

const MOTIF_LEN: usize = 3;
const MOTIF_EVERY_WORDS: u8 = 3;

const AUTOREPEAT_JITTER_MS: u32 = 2;
const AUTOREPEAT_RUN: u8 = 3;
const AUTOREPEAT_FLOOR_MS: u32 = 25;

/// A typing gap this long is a rest, and a rest resolves the line (§3.1).
pub(super) const PHRASE_PAUSE_MS: u32 = timing::PHRASE_PAUSE_MS;

const IOI_DEFAULT_MS: f32 = timing::IOI_DEFAULT_MS;

const UNDO_N: usize = 8;

/// The PTY cascade's exclusivity window.
pub(super) const CASCADE_EXCLUSIVE_MS: u32 = 180;
const CASCADE_RESTRIKE_MS: u32 = 60;
const CASCADE_RUN_MS: u32 = PHRASE_PAUSE_MS;
const ENTER_ECHO_SWALLOW_MS: u32 = 150;

const ENTER_PICKUP_MIN_KEYS: u8 = 4;

/// C5 — the lattice anchor (§2.6).
pub(super) const TINE_BASE_HZ: f32 = 523.25;

const TINE_ATTACK_S: f32 = 0.004;
const TINE_DUR_TAIL_S: f32 = 0.020;

const P1_LVL: f32 = 0.50;
const P2_RATIO: f32 = 2.0;
const P2_LVL: f32 = 0.16;
const P2_TAU_S: f32 = 0.055;
const P3_RATIO: f32 = 2.760;
const P3_LVL: f32 = 0.12;
const P3_TAU_S: f32 = 0.040;

const MALLET_HZ0: f32 = 1800.0;
const MALLET_HZ1: f32 = 6400.0;
const MALLET_GLIDE_S: f32 = 0.005;
const MALLET_Q: f32 = 0.7;
const MALLET_LVL: f32 = 0.45;
const MALLET_TAU_S: f32 = 0.006;

const TAU_V_BASE_S: f32 = 0.110;
const TAU_V_OFFSET: f32 = 0.068_181_8;
const TAU_V_SLOPE: f32 = 3.727_272_7;
const TAU_V_MIN_S: f32 = 0.028;
const TAU_V_MAX_S: f32 = 0.110;

const ROOF_PLAIN_LO_HZ: f32 = 4200.0;
const ROOF_PLAIN_HI_HZ: f32 = 5200.0;
const ROOF_LIT_ADD_HZ: f32 = 2300.0;
const ROOF_MAX_HZ: f32 = 7500.0;
const ROOF_HEAT_HZ: f32 = 600.0;
const ROOF_HUE_ADD_HZ: f32 = 900.0;
const ROOF_CPS_LO: f32 = 4.0;
const ROOF_CPS_SPAN: f32 = 8.0;
const RESTRIKE_ROOF_DROP_HZ: f32 = 400.0;

const RESTRIKE_P2_LVL: f32 = 0.10;
const RESTRIKE_MALLET_LVL: f32 = 0.30;
const RESTRIKE_TAU_MUL: f32 = 0.7;
const RESTRIKE_L0: f32 = 0.60;
const RESTRIKE_FALL: f32 = 0.85;
const RESTRIKE_FLOOR: f32 = 0.35;
const RESTRIKE_LEVELS: [f32; 5] = [
    RESTRIKE_L0,
    RESTRIKE_L0 * RESTRIKE_FALL,
    RESTRIKE_L0 * (RESTRIKE_FALL * RESTRIKE_FALL),
    RESTRIKE_L0 * (RESTRIKE_FALL * (RESTRIKE_FALL * RESTRIKE_FALL)),
    RESTRIKE_FLOOR,
];

const BLOOM_DEGREES: [i32; 3] = [8, 10, 13];
const BLOOM_LVL: [f32; 3] = [0.10, 0.06, 0.035];
const BLOOM_TAU: [f32; 3] = [0.22, 0.30, 0.38];
const BLOOM_ATTACK_S: f32 = 0.018;
const BLOOM_DELAY_S: f32 = 0.012;
const BLOOM_HEAD_MIN_MUL: f32 = 0.5;
const BLOOM_DECAY_S: f32 = 0.340;
const BLOOM_DECAY_TAU_MUL: f32 = 3.1;
const BLOOM_DUR_S: f32 = 3.0 * BLOOM_DECAY_S + TINE_DUR_TAIL_S;
const BLOOM_TW_RATE: f32 = 5.5;
const BLOOM_TW_DEPTH: f32 = 0.22;
const BLOOM_ROOF_ADD_HZ: f32 = 2400.0;
const BLOOM_LEVEL: f32 = 2.0;
const BLOOM_SPREAD: f32 = 0.28;

const ANSWER_DELAY_S: f32 = 0.19;
const ANSWER_LEVEL: f32 = 0.25;

const AIR_TAP_DELAY_S: [f32; 2] = [0.09, 0.17];
const AIR_TAP_LEVEL: [f32; 2] = [0.199_526_2, 0.1];
const AIR_TAP_PAN: f32 = 0.4;

const CAPITAL_LIFT_DEG: i32 = 3;

const FLOW_ECHO_DELAY_S: f32 = 0.025;
const FLOW_ECHO_PHASE: f32 = 0.75;
const FLOW_ECHO_OCTAVE_DEG: i32 = 5;
const FLOW_ECHO_LEVEL: f32 = 0.25;

const FLOW_KEY_HEADROOM: f32 = 0.954_992_6;

#[inline]
const fn flow_key_headroom(flow: f32) -> f32 {
    lerp(1.0, FLOW_KEY_HEADROOM, crate::color_math::sqrt_f32(flow))
}

const CAP_RING_LEVEL: f32 = 0.501_187_2;
const CAP_RING_LIFT_DEG: i32 = 3;
const CAP_RING_DELAY_S: f32 = 0.060;
const CAP_RING_ATTACK_S: f32 = 0.030;
const FLOW_BASS_DECAY_S: f32 = 0.200;
const FLOW_P2_LVL: f32 = 0.22;

#[inline]
const fn flow_partials(flow: f32) -> (f32, f32) {
    let p2 = lerp(P2_LVL, FLOW_P2_LVL, flow);
    (P1_LVL - (p2 - P2_LVL), p2)
}

const PASSING_LEVEL: f32 = 0.794_328_2;

const G_IOI_REF_S: f32 = 0.25;
const G_IOI_MIN: f32 = 0.45;

const VEL_DB_STEP: f32 = 1.0;
const VEL_DB_RESTRIKE: f32 = 1.5;
const VEL_DB_MOTION: f32 = 0.8;
const PAN_JITTER: f32 = 0.03;

const KEY_TINE_TRIM: f32 = 0.2741;

const BASS_BASE_HZ: f32 = 261.63;

const CHORD_ROOT_RATIO: [f32; 4] = [1.0, 5.0 / 3.0, 4.0 / 3.0, 3.0 / 2.0];

#[derive(Clone, Copy, Debug)]
struct Chord {
    root: usize,
    partner: f32,
    lit: u8,
}

const CHORD_LOOP: [Chord; 8] = [
    Chord {
        root: 0,
        partner: 1.5,
        lit: 0b0000_1101,
    },
    Chord {
        root: 1,
        partner: 0.75,
        lit: 0b0001_0101,
    },
    Chord {
        root: 2,
        partner: 0.75,
        lit: 0b0001_0101,
    },
    Chord {
        root: 3,
        partner: 0.75,
        lit: 0b0001_1010,
    },
    Chord {
        root: 0,
        partner: 1.5,
        lit: 0b0000_1101,
    },
    Chord {
        root: 3,
        partner: 0.75,
        lit: 0b0001_1010,
    },
    Chord {
        root: 1,
        partner: 0.75,
        lit: 0b0001_0101,
    },
    Chord {
        root: 2,
        partner: 0.75,
        lit: 0b0001_0101,
    },
];

const CHORD_AFTER_ENTER: u8 = 7;

/// C4: one octave under the tine ([`TINE_BASE_HZ`]), clear of the sub-200 Hz hum band.
pub(super) const BED_BASE_HZ: f32 = TINE_BASE_HZ * 0.5;
/// The number of chords the sky voices ([`CHORD_LOOP`]'s length), exported for the parent's lattice law.
pub(super) const SKY_BED_CHORDS: usize = CHORD_LOOP.len();
const BED_WEIGHT: [f32; 3] = [1.0, 0.80, 0.65];
const BED_GLIDE_TAU_S: f32 = 0.045;
const BED_LEVEL: f32 = 0.005_5;
const BED_BREATH_HZ: f32 = 1.0 / 12.0;
const BED_BREATH_DEPTH: f32 = 0.35;
const BED_HUE_TAU_S: f32 = 1.2;
const BED_TILT_LO_HZ: f32 = 420.0;
const BED_TILT_HI_HZ: f32 = 980.0;
const BED_DETUNE_CENTS_LO: f32 = 4.0;
const BED_DETUNE_CENTS_HI: f32 = 11.0;
const BED_PARTIALS: [u32; 4] = [1, 2, 3, 4];
const BED_PARTIAL_NORM: f32 = 0.838_1;

#[inline]
const fn compute_sky_bed_degrees(chord: usize) -> [i32; 3] {
    const _: () = assert!(SKY_BED_CHORDS.is_power_of_two());
    let lit = CHORD_LOOP[chord & (SKY_BED_CHORDS - 1)].lit;
    let mut out = [0i32; 3];
    let mut n = 0;
    let mut d = 0;
    while d < 5 {
        if lit & (1 << d) != 0 && n < 3 {
            out[n] = d;
            n += 1;
        }
        d += 1;
    }
    debug_assert!(n == 3, "every chord lights exactly three degrees");
    out
}

const SKY_BED_DEGREES_TABLE: [[i32; 3]; SKY_BED_CHORDS] = {
    let mut table = [[0i32; 3]; SKY_BED_CHORDS];
    let mut c = 0;
    while c < SKY_BED_CHORDS {
        table[c] = compute_sky_bed_degrees(c);
        c += 1;
    }
    table
};

const SKY_BED_TARGETS: [[f32; 3]; SKY_BED_CHORDS] = {
    let mut table = [[0.0f32; 3]; SKY_BED_CHORDS];
    let mut c = 0;
    while c < SKY_BED_CHORDS {
        let degs = SKY_BED_DEGREES_TABLE[c];
        table[c] = [
            BED_BASE_HZ * super::PENTA[degs[0] as usize],
            BED_BASE_HZ * super::PENTA[degs[1] as usize],
            BED_BASE_HZ * super::PENTA[degs[2] as usize],
        ];
        c += 1;
    }
    table
};

/// The sky's voicing for one chord of [`CHORD_LOOP`].
#[inline]
pub(super) const fn sky_bed_degrees(chord: usize) -> [i32; 3] {
    SKY_BED_DEGREES_TABLE[chord & (SKY_BED_CHORDS - 1)]
}

#[inline]
const fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

const BASS_ROOT_LVL: f32 = 0.32;
const BASS_FIFTH_LVL: f32 = 0.22;
const BASS_OCT_LVL: f32 = 0.08;
const BASS_OCT_TAU_S: f32 = 0.060;
const BASS_ATTACK_S: f32 = 0.008;
const BASS_DECAY_S: f32 = 0.111;
const BASS_ROOF_HZ: f32 = 900.0;
const BASS_LEVEL: f32 = 0.630_957_3;

const BREATH_HZ0: f32 = 900.0;
const BREATH_HZ1: f32 = 380.0;
const BREATH_GLIDE_S: f32 = 0.080;
const BREATH_Q: f32 = 0.7;
const BREATH_ATTACK_S: f32 = 0.008;
const BREATH_DECAY_S: f32 = 0.055;
const BREATH_DUR_S: f32 = TAIL_DUR_PER_TAU * BREATH_DECAY_S;
const BREATH_LEVEL: f32 = 0.158_489_3;
const BREATH_ROOF_HZ: f32 = 9000.0;
const SPACE_TWINKLE_LEVEL: f32 = 0.5;
const SPACE_TWINKLE_UNDER: f32 = 0.2;
const SPACE_STEP_LEVEL: f32 = 0.35;
const SPACE_STEP_TAU_S: f32 = 0.045;
const SPACE_STEP_MIN_GAP_MS: u32 = 75;
const SPACE_STEP_HI_HZ: f32 = BASS_BASE_HZ * 4.0;

#[inline]
const fn indent_step_hz(chord: usize, root: f32, n: u32, key: i32) -> f32 {
    let lit = sky_bed_degrees(chord);
    let base = [
        penta(BASS_BASE_HZ, lit[0] + key),
        penta(BASS_BASE_HZ, lit[1] + key),
        penta(BASS_BASE_HZ, lit[2] + key),
    ];
    #[inline]
    const fn hz(base: [f32; 3], k: usize) -> f32 {
        base[k % 3] * f32::from_bits(((127 + (k / 3)) as u32) << 23)
    }
    let root_floor = root * 1.01;
    let mut k0 = 0usize;
    while k0 < 64 {
        if hz(base, k0) > root_floor {
            break;
        }
        k0 += 1;
    }
    let mut cycle = 0usize;
    while cycle < 64 && hz(base, k0 + cycle) < SPACE_STEP_HI_HZ {
        cycle += 1;
    }
    let cycle = if cycle > 1 { cycle } else { 1 };
    let k = k0 + (n.saturating_sub(1) as usize) % cycle;
    let mut f = hz(base, k);
    while f.is_finite() && f >= SPACE_STEP_HI_HZ {
        f *= 0.5;
    }
    f
}

const MARK_NONE: u8 = 0;
const MARK_PERIOD: u8 = 1;
const MARK_BANG: u8 = 2;
const MARK_QMARK: u8 = 3;
const MARK_COMMA: u8 = 4;
const MARK_SEMI: u8 = 5;
const MARK_COLON: u8 = 6;
const MARK_DOUBLED: u8 = 0x80;

#[inline]
const fn phrase_kind(class: u8) -> u8 {
    match class {
        STOP => MARK_PERIOD,
        BANG => MARK_BANG,
        QMARK => MARK_QMARK,
        COMMA => MARK_COMMA,
        SEMI => MARK_SEMI,
        COLON => MARK_COLON,
        _ => MARK_NONE,
    }
}

#[inline]
const fn mark_chords(mark: u8) -> Option<[u8; 2]> {
    match mark {
        MARK_PERIOD | MARK_BANG => Some([0, 4]),
        MARK_QMARK | MARK_COMMA | MARK_COLON => Some([3, 5]),
        MARK_SEMI => Some([1, 6]),
        _ => None,
    }
}

const STEER_MAX_DEG: i32 = 3;

#[inline]
const fn nearest_cand(cands: &[i32], from: i32, fall: bool, skip: i32) -> Option<i32> {
    let mut best: Option<i32> = None;
    let mut i = 0;
    while i < cands.len() {
        let c = cands[i];
        i += 1;
        if c == skip {
            continue;
        }
        best = Some(match best {
            None => c,
            Some(b) => {
                let (dc, db) = ((c - from).abs(), (b - from).abs());
                if dc < db || (dc == db && ((c < b) == fall)) {
                    c
                } else {
                    b
                }
            }
        });
    }
    best
}

#[inline]
const fn cadence_target(mark: u8, from: i32, last_cadence: i32) -> i32 {
    #[inline]
    const fn above(cs: [i32; 2], from: i32) -> Option<i32> {
        let mut i = 0;
        while i < cs.len() {
            let c = cs[i];
            if c > from && c - from <= STEER_MAX_DEG {
                return Some(c);
            }
            i += 1;
        }
        None
    }
    let (cands, fall): (&[i32], bool) = match mark {
        MARK_PERIOD => (&[0, 5], true),
        MARK_BANG => (&[0, 3, 5, 8], false),
        MARK_COMMA => (&[1, 3, 6, 8], true),
        MARK_SEMI => (&[2, 4, 7], true),
        MARK_COLON => (&[3, 8], false),
        MARK_QMARK => {
            if let Some(c) = above([3, 8], from) {
                return c;
            }
            if let Some(c) = above([1, 6], from) {
                return c;
            }
            return from;
        }
        _ => return from,
    };
    let first = match nearest_cand(cands, from, fall, -1) {
        Some(c) => c,
        None => from,
    };
    if !matches!(mark, MARK_PERIOD | MARK_BANG) || first != last_cadence {
        return first;
    }
    match nearest_cand(cands, from, fall, first) {
        Some(alt)
            if (alt - from).abs() <= (first - from).abs() + 1
                && (alt - from).abs() <= STEER_MAX_DEG =>
        {
            alt
        }
        _ => first,
    }
}

const ASIDE_GAIN: f32 = 0.794_328_2;
const ASIDE_ROOF_MUL: f32 = 0.8;
const ASIDE_MAX_KEYS: u8 = 32;
const ASIDE_DEPTH_MAX: u8 = 4;

const QUOTE_NONE: u8 = 0;
const QUOTE_OPENS: u8 = 1;
const QUOTE_CLOSES: u8 = 2;

const WOOD_P2_RATIO: f32 = 3.0;
const WOOD_P2_LVL: f32 = 0.26;
const WOOD_P2_TAU_S: f32 = 0.045;
const WOOD_P3_RATIO: f32 = 5.0;
const WOOD_P3_LVL: f32 = 0.08;
const WOOD_P3_TAU_S: f32 = 0.030;
const WOOD_P1_LVL: f32 = P1_LVL + P2_LVL + P3_LVL - WOOD_P2_LVL - WOOD_P3_LVL;
const WOOD_MALLET_HZ0: f32 = 1400.0;
const WOOD_MALLET_HZ1: f32 = 3600.0;
const WOOD_MALLET_Q: f32 = 1.0;
const WOOD_MALLET_TAU_S: f32 = 0.004;
const DIGIT_TAU_MUL: f32 = 0.7;

const COUNT_GLINT_DEG0: i32 = 13;
const DIGIT_RANK0: i32 = 27;
const COUNT_GLINT_LOW_MUL: f32 = 2.0;

const PLUCK_TAU_MUL: f32 = 0.5;
const PLUCK_TAU_MIN_S: f32 = 0.025;
const PLUCK_P1_LVL: f32 = 0.60;
const PLUCK_P2_RATIO: f32 = 4.0;
const PLUCK_P2_LVL: f32 = 0.18;
const PLUCK_P2_TAU_S: f32 = 0.030;
const PLUCK_NOISE_LVL: f32 = 0.5;
const PLUCK_HZ0: f32 = 1400.0;
const PLUCK_HZ1: f32 = 500.0;
const PLUCK_Q: f32 = 0.9;
const PLUCK_NOISE_TAU_S: f32 = 0.015;
const BANG_GLINT2_DELAY_S: f32 = 0.090;
const CADENCE_STOP_TAU_MUL: f32 = 1.35;
const DASH_TAU_MUL: f32 = 1.5;
const MARK_TAU_MAX_S: f32 = 0.150;
const PAUSE_MARK_GAIN: f32 = 0.794;
const PAUSE_BREATH_MUL: f32 = 0.6;
const QUEST_RISE_DEG: i32 = 3;
const QUEST_RISE_D_DEG: i32 = 2;
const QUEST_RISE_DELAY_S: f32 = 0.060;
const QUEST_RISE_LEVEL: f32 = 0.707_945_8;
const TINK_DEG: i32 = 1;
const TINK_DELAY_S: f32 = 0.045;
const TINK_TAU_S: f32 = 0.035;
const TINK_DUR_S: f32 = TAIL_DUR_PER_TAU * TINK_TAU_S;
const TINK_LEVEL: f32 = 0.398_107_2;
const QUOTE_GLINT_MUL: f32 = 2.0;
const QUOTE_GLINT2_DELAY_S: f32 = 0.045;
const ZIP_HZ_LO: f32 = 900.0;
const ZIP_HZ_HI: f32 = 3200.0;
const ZIP_GLIDE_S: f32 = 0.040;
const ZIP_TAU_S: f32 = 0.030;
const ZIP_MALLET_LVL: f32 = 0.5;
const FIFTH_DEG: i32 = 3;
const FIFTH_DELAY_S: f32 = 0.030;
const FIFTH_LEVEL: f32 = 0.501_187_2;
const STOP_BREATH_DELAY_S: f32 = 0.020;

const NAV_ATTACK_S: f32 = 0.004;
const NAV_DECAY_S: f32 = 0.030;
const NAV_DUR_S: f32 = TAIL_DUR_PER_TAU * NAV_DECAY_S;
const NAV_LEVEL: f32 = 0.063_095_73;

const TING_ATTACK_S: f32 = 0.003;
const TING_DECAY_S: f32 = 0.200;
const TING_DUR_S: f32 = super::RING_OUT_PER_TAU * TING_DECAY_S + super::RELEASE_RAMP_S;
const TING_LP_HZ: f32 = 6500.0;
const TING_P2_LVL: f32 = 0.12;
const TING_P2_TAU_S: f32 = 0.140;
const TING_P3_LVL: f32 = 0.08;
const TING_P3_TAU_S: f32 = 0.030;
const TING_MALLET_LVL: f32 = 0.10;
const TING_LO_HZ: f32 = 1240.0;
const TING_PICK: [usize; 5] = [0, 1, 0, 2, 1];
const TING_TONES: [[i32; 3]; 4] = [[0, 3, 2], [4, 2, 0], [0, 4, 2], [3, 1, 4]];
const TING_LEVEL: f32 = 0.49;

const TING_DUCK: f32 = 0.66;
const TING_DUCK_MARK: f32 = 0.50;
const TING_DUCK_RAMP_S: f32 = LANE_FADE_STEAL_S;

#[inline]
const fn ting_fold(mut f: f32) -> f32 {
    while f >= TING_LO_HZ * 2.0 {
        f *= 0.5;
    }
    while f < TING_LO_HZ {
        f *= 2.0;
    }
    f
}

#[inline]
const fn ting(f: f32) -> Voice {
    Voice {
        dur: TING_DUR_S,
        attack: TING_ATTACK_S,
        decay: TING_DECAY_S,
        p: [
            Partial {
                lvl: P1_LVL,
                f0: f,
                ..Partial::DEFAULT
            },
            Partial {
                lvl: TING_P2_LVL,
                f0: f * P2_RATIO,
                decay: TING_P2_TAU_S,
                ..Partial::DEFAULT
            },
            Partial {
                lvl: TING_P3_LVL,
                f0: f * P3_RATIO,
                decay: TING_P3_TAU_S,
                ..Partial::DEFAULT
            },
        ],
        n_lvl: TING_MALLET_LVL,
        n_f0: MALLET_HZ0,
        n_f1: MALLET_HZ1,
        n_glide: MALLET_GLIDE_S,
        n_q: MALLET_Q,
        n_decay: MALLET_TAU_S,
        lp_cut: TING_LP_HZ,
        lane: LANE_TING,
        ..Voice::DEFAULT
    }
}

const TUNE_DEG_LO: i32 = 0;
const TUNE_DEG_HI: i32 = 8;

const GLINT_DEGREES: [i32; 3] = [7, 9, 11];
const GLINT_LO_HZ: f32 = 2800.0;
const GLINT_HI_HZ: f32 = 5600.0;
const GLINT_ATTACK_S: f32 = 0.002;
const GLINT_DECAY_S: f32 = 0.040;
const GLINT_DUR_S: f32 = TAIL_DUR_PER_TAU * GLINT_DECAY_S;
const GLINT_LEVEL: f32 = 0.125_892_5;
const KEY_GLINT_LEVEL_MUL: f32 = 0.5;
const KEY_GLINT_SHIFTED_MUL: f32 = 4.0;

const FORTE_FM_RATIO: f32 = 4.0;
const FORTE_FM_INDEX: f32 = 0.9;
const FORTE_FM_REF_HZ: f32 = 932.33;
const FORTE_FM_TAU_S: f32 = 0.045;
const FORTE_FM_TURN_HEAD: f32 = 0.0;
const FORTE_FM_TURN: f32 = 0.54;
const FORTE_P2_SHIFT: f32 = 0.06;
const FORTE_P2_MAX: f32 = 0.30;
const FORTE_P2_TAU_ADD_S: f32 = 0.045;
const FORTE_TAU_GAIN: f32 = 0.35;
const FORTE_TAU_MAX_S: f32 = 0.150;
const FORTE_ROOF_ADD_HZ: f32 = 900.0;
const FORTE_GLINT_MUL: f32 = 2.0;
const MARK_SHIFT_GAIN: f32 = 1.122;

const WORD_CAPITAL_GAIN: f32 = 1.412_537_5;

const KEY_GLINT_DELAY_S: f32 = BLOOM_DELAY_S + BLOOM_ATTACK_S;
const GLINT_TWINKLE_DEPTH: f32 = 0.5;
const GLINT_TWINKLE_DEFAULT_HZ: f32 = 10.0;
const GLINT_LP_HZ: f32 = 8000.0;

const MET_TICK_HZ0: f32 = 3200.0;
const MET_TICK_HZ1: f32 = 900.0;
const MET_TICK_GLIDE_S: f32 = 0.004;
const MET_TICK_Q: f32 = 0.7;
const MET_TICK_ATTACK_S: f32 = 0.000_3;
const MET_TICK_DECAY_S: f32 = 0.001_5;
const MET_TICK_DUR_S: f32 = super::RING_OUT_PER_TAU * MET_TICK_DECAY_S + super::RELEASE_RAMP_S;
const MET_TICK_LEVEL: f32 = 0.28;
const MET_TICK_ROOF_HZ: f32 = MET_WHOOSH_LP_HZ;

const MET_CORE_LO_HZ: f32 = TINE_BASE_HZ;
const MET_CORE_HI_HZ: f32 = 2.0 * TINE_BASE_HZ;
const MET_CORE_GLIDE_MUL: f32 = 0.45;
const MET_CORE_FM_RATIO: f32 = 3.01;
const MET_CORE_FM_INDEX: f32 = 0.35;
const MET_CORE_FM_TAU_MUL: f32 = 0.5;
const MET_CORE_ATTACK_S: f32 = 0.006;
const MET_CORE_DECAY_MUL: f32 = 0.7;
const MET_CORE_DUR_MUL: f32 = 1.9;
const MET_CORE_LP_HZ: f32 = 3800.0;
const MET_CORE_LEVEL: f32 = 0.398_107_2;
const MET_ARM_CORE_LEVEL: f32 = 0.1;
const MET_ARM_TTL_S: f32 = 0.150;
const MET_CLAIM_MIN_TAIL_S: f32 = 0.030;

const MET_WHOOSH_HZ0: f32 = 1600.0;
const MET_WHOOSH_HZ1: f32 = 5200.0;
const MET_WHOOSH_GLIDE_MUL: f32 = 0.65;
const MET_WHOOSH_Q: f32 = 1.5;
const MET_WHOOSH_ATTACK_S: f32 = 0.018;
const MET_WHOOSH_DECAY_MUL: f32 = 0.6;
const MET_WHOOSH_DUR_MUL: f32 = 1.9;
const MET_WHOOSH_LP_HZ: f32 = 7000.0;
const MET_WHOOSH_LEVEL: f32 = 0.501_187_2;

const MET_BELL_TAU_S: f32 = 0.080;
const MET_BELL_DUR_S: f32 = 0.260;
const MET_BELL_LEVEL: f32 = 1.258_925_4;

const MET_THUMP_ATTACK_S: f32 = 0.008;
const MET_THUMP_DECAY_S: f32 = 0.060;
const MET_THUMP_DUR_S: f32 = TAIL_DUR_PER_TAU * MET_THUMP_DECAY_S;
const MET_THUMP_LEVEL: f32 = 0.501_187_2;

const MET_RAIN_HZ: [f32; timing::FAN_HERO_N] = [5232.5, 4709.25, 4186.0, 3488.33, 3139.5];
const MET_RAIN_LEAD_S: f32 = 0.010;
const MET_RAIN_ATTACK_S: f32 = 0.002;
const MET_RAIN_DECAY_S: f32 = 0.028;
const MET_RAIN_DUR_S: f32 = TAIL_DUR_PER_TAU * MET_RAIN_DECAY_S;
const MET_RAIN_TWINKLE_HZ: f32 = 12.0;
const MET_RAIN_TWINKLE_DEPTH: f32 = 0.5;
const MET_RAIN_LP_HZ: f32 = 8000.0;
const MET_RAIN_LEVEL: f32 = 0.125_892_5;
const MET_RAIN_PAN: [f32; timing::FAN_HERO_N] = [0.25, -0.20, 0.15, -0.10, 0.05];

const METEOR_INTERRUPT_S: f32 = 0.015;

const CAD_PICKUP_DEG: i32 = 3;
const CAD_PICKUP_LEVEL: f32 = 0.794_328_2;
const CAD_RESOLUTION_LOW_DEG: i32 = 0;
const CAD_RESOLUTION_HIGH_DEG: i32 = 5;
const CAD_RESOLUTION_SPLIT: i8 = 2;
const CAD_QUESTION_LIFT_DEG: i32 = 3;
const CAD_DYAD_LEVEL: f32 = 0.707_945_8;
const CAD_BELL_HZ: f32 = 2.0 * TINE_BASE_HZ;
const CAD_BELL_FM_RATIO: f32 = 3.01;
const CAD_BELL_FM_INDEX: f32 = 0.5;
const CAD_BELL_FM_TAU_S: f32 = 0.040;
const CAD_BELL_ATTACK_S: f32 = 0.008;
const CAD_BELL_DECAY_S: f32 = 0.160;
const CAD_BELL_DUR_S: f32 = TAIL_DUR_PER_TAU * CAD_BELL_DECAY_S;
const CAD_BELL_TWINKLE_HZ: f32 = 9.0;
const CAD_BELL_TWINKLE_DEPTH: f32 = 0.3;
const CAD_BELL_LP_HZ: f32 = 3600.0;
const CAD_BELL_LEVEL: f32 = 0.199_526_2;

/// The ice bell at `delay` and its level trim.
#[inline]
pub(super) const fn cad_bell_voice(delay: f32) -> (Voice, f32) {
    let bell = Voice {
        delay,
        dur: CAD_BELL_DUR_S,
        attack: CAD_BELL_ATTACK_S,
        decay: CAD_BELL_DECAY_S,
        p: [
            Partial {
                lvl: P1_LVL,
                f0: CAD_BELL_HZ,
                fm_ratio: CAD_BELL_FM_RATIO,
                fm_i0: CAD_BELL_FM_INDEX,
                fm_tau: CAD_BELL_FM_TAU_S,
                ..Partial::DEFAULT
            },
            Partial::DEFAULT,
            Partial::DEFAULT,
        ],
        tw_rate: CAD_BELL_TWINKLE_HZ,
        tw_depth: CAD_BELL_TWINKLE_DEPTH,
        lp_cut: CAD_BELL_LP_HZ,
        lane: LANE_CADENCE,
        ..Voice::DEFAULT
    };
    (bell, KEY_TINE_TRIM * CAD_BELL_LEVEL)
}

const CASCADE_DEGREES: [i32; 4] = [0, 2, 3, 5];
const CASCADE_DELAYS_S: [f32; 4] = [0.0, 0.045, 0.090, 0.135];
const CASCADE_LEVELS: [f32; 4] = [1.0, 0.298_538_3, 0.269_153_5, 0.239_883_3];
const CASCADE_RESTRIKE_DEG: i32 = 5;
const CASCADE_RESTRIKE_LEVEL: f32 = 0.251_188_6;

const STRUM_DELAYS_S: [f32; 4] = [0.0, 0.022, 0.044, 0.066];
const STRUM_SHAPE: [f32; 4] = [1.0, 0.794_328_2, 0.630_957_3, 0.501_187_2];
const STRUM_TRIM: f32 = 0.653_130_6;

const VERDICT_PLAGAL: [usize; 2] = [2, 0];
const VERDICT_MINOR: usize = 1;
const VERDICT_PLAGAL_S: f32 = 0.26;
const CHORD_AFTER_RED: u8 = 0;
const VERDICT_DYAD_LEVEL: f32 = 0.501_187_2;
const VERDICT_PEAK_TRIM: f32 = 0.891_250_9;
const VERDICT_MINOR_LEVEL: f32 = 0.354_813_4;
const VERDICT_TINE_DEG: i32 = 2;
const VERDICT_TINE_LEVEL: f32 = 0.398_107_2;
const VERDICT_LIT_MS: u32 = 5_000;

/// Which touch a key gets (§9.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Touch {
    /// The line moved.
    Step,
    /// The line derived the pitch it is already on.
    ReStrike,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Undo {
    walk: i8,
    restrike: u8,
    chord: u8,
    word_pos: u8,
    prev_rank: u8,
    run_stride: i8,
    run_len: u8,
    motif: [i8; MOTIF_LEN],
    motif_len: u8,
    motif_play: u8,
    motif_k: u8,
    words_since_motif: u8,
    prev_shifted: bool,
    last_mark: u8,
    token_marked: bool,
    paren_deg: i8,
    last_cadence_deg: i8,
    aside_depth: u8,
    aside_keys: u8,
    quote_deg: i8,
    after_open: bool,
}

impl Undo {
    const HOME: Self = Self {
        walk: 0,
        restrike: 0,
        chord: 0,
        word_pos: 0,
        prev_rank: 0,
        run_stride: 0,
        run_len: 0,
        motif: [0; MOTIF_LEN],
        motif_len: 0,
        motif_play: 0,
        motif_k: 0,
        words_since_motif: 0,
        prev_shifted: false,
        last_mark: MARK_NONE,
        token_marked: false,
        paren_deg: -1,
        last_cadence_deg: -1,
        aside_depth: 0,
        aside_keys: 0,
        quote_deg: -1,
        after_open: false,
    };
}

/// The melody engine (§10): `Copy`, alloc-free, deterministic, driven entirely by `EventMeta::at_ms` from the host's input clock, never by audio-thread arrival gaps.
#[derive(Clone, Copy, Debug)]
pub struct MelodyV2 {
    text: Undo,
    steps: u32,
    last_key_ms: Option<u32>,
    prev_gap: u32,
    repeat_run: u8,
    ioi_ms: f32,
    space_run: bool,
    space_steps: u32,
    last_step_ms: u32,
    stops: TimbreStops,
    flow: f32,
    glint_k: u8,
    cascade_at: Option<u32>,
    cascade_restrike_ms: u32,
    run_theme_pos: u8,
    run_phrase: u8,
    keys_since_enter: u8,
    last_enter_ms: Option<u32>,
    undo: [Undo; UNDO_N],
    undo_len: u8,
    lead: Option<(u8, u32)>,
    bass: Option<(u8, u32)>,
    lift: Option<(u8, u32)>,
    ring: Option<(u8, u32)>,
    graft: Option<(u8, u32)>,
    prev_class: u8,
    verdict_at_ms: Option<u32>,
    bell: Option<(u8, u32)>,
    thump: Option<(u8, u32)>,
}

impl Default for MelodyV2 {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl MelodyV2 {
    /// A fresh melody: home, the loop on I, no history and no subject.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            text: Undo {
                chord: CHORD_AFTER_ENTER,
                ..Undo::HOME
            },
            steps: 0,
            last_key_ms: None,
            prev_gap: 0,
            repeat_run: 0,
            ioi_ms: IOI_DEFAULT_MS,
            space_run: false,
            space_steps: 0,
            last_step_ms: 0,
            stops: TimbreStops::ALL,
            flow: 0.0,
            glint_k: 0,
            cascade_at: None,
            cascade_restrike_ms: 0,
            run_theme_pos: 0,
            run_phrase: 0,
            keys_since_enter: 0,
            last_enter_ms: None,
            undo: [Undo::HOME; UNDO_N],
            undo_len: 0,
            lead: None,
            bass: None,
            lift: None,
            ring: None,
            graft: None,
            prev_class: LETTER,
            verdict_at_ms: None,
            bell: None,
            thump: None,
        }
    }

    /// The sounding verse degree (test / introspection hook).
    #[must_use]
    #[cfg(test)]
    pub const fn walk(&self) -> i8 {
        self.text.walk
    }

    /// Keystrokes that have moved the melody (test / introspection hook).
    #[must_use]
    #[cfg(test)]
    pub const fn steps(&self) -> u32 {
        self.steps
    }

    /// The live chord's index into [`CHORD_LOOP`] (test / introspection hook).
    #[must_use]
    #[cfg(test)]
    pub const fn chord(&self) -> u8 {
        self.text.chord
    }

    /// Letters since the last word boundary (test / introspection hook).
    #[must_use]
    #[cfg(test)]
    pub const fn word_pos(&self) -> u8 {
        self.text.word_pos
    }

    /// Whether the line is answering its own subject.
    #[must_use]
    #[cfg(test)]
    pub const fn motif_answering(&self) -> bool {
        self.text.motif_play > 0
    }

    /// The smoothed inter-onset interval in ms (test / introspection hook).
    #[must_use]
    #[cfg(test)]
    pub const fn ioi_ms(&self) -> f32 {
        self.ioi_ms
    }

    #[inline]
    const fn at_boundary(&self, at: u32) -> bool {
        self.text.word_pos == 0
            || match self.last_key_ms {
                Some(k) => timing::phrase_rest_reached(at.saturating_sub(k), self.ioi_ms),
                None => false,
            }
    }

    #[inline]
    const fn frame(&self) -> Undo {
        self.text
    }

    #[inline]
    const fn push_undo(&mut self) {
        let frame = self.frame();
        if (self.undo_len as usize) == UNDO_N {
            let mut i = 0;
            while i + 1 < UNDO_N {
                self.undo[i] = self.undo[i + 1];
                i += 1;
            }
            self.undo[UNDO_N - 1] = frame;
        } else {
            self.undo[self.undo_len as usize] = frame;
            self.undo_len += 1;
        }
    }

    #[inline]
    const fn pop_undo(&mut self) {
        let Some(n) = self.undo_len.checked_sub(1) else {
            return;
        };
        self.text = self.undo[n as usize];
        self.undo_len = n;
    }

    #[inline]
    const fn deg_is_lit_in(lit_mask: u8, deg: i32) -> bool {
        let pc = deg.rem_euclid(5) as u8;
        lit_mask & (1 << pc) != 0
    }

    #[cfg(test)]
    #[inline]
    const fn deg_is_lit(&self, deg: i32) -> bool {
        Self::deg_is_lit_in(CHORD_LOOP[self.text.chord as usize].lit, deg)
    }

    #[inline]
    const fn nearest_lit_within(&self, deg: i32, from: i32) -> i32 {
        let deg = if deg < TUNE_DEG_LO {
            TUNE_DEG_LO
        } else if deg > TUNE_DEG_HI {
            TUNE_DEG_HI
        } else {
            deg
        };
        let lit_mask = CHORD_LOOP[self.text.chord as usize].lit;
        if Self::deg_is_lit_in(lit_mask, deg) {
            return deg;
        }
        let dir = if deg >= from { 1 } else { -1 };
        let mut k = 1;
        while k <= WORD_HEAD_SNAP_DEG {
            let c0 = deg + dir * k;
            if c0 >= TUNE_DEG_LO && c0 <= TUNE_DEG_HI && Self::deg_is_lit_in(lit_mask, c0) {
                return c0;
            }
            let c1 = deg - dir * k;
            if c1 >= TUNE_DEG_LO && c1 <= TUNE_DEG_HI && Self::deg_is_lit_in(lit_mask, c1) {
                return c1;
            }
            k += 1;
        }
        deg
    }

    #[inline]
    const fn relatch_motif(&mut self) {
        self.text.motif_len = 0;
        self.text.motif_play = 0;
        self.text.motif_k = 0;
        self.text.words_since_motif = 0;
    }

    #[inline]
    const fn end_phrase(&mut self) {
        self.text.last_mark = MARK_NONE;
        self.text.token_marked = false;
        self.text.paren_deg = -1;
        self.close_aside();
        self.text.quote_deg = -1;
        self.text.after_open = false;
    }

    #[inline]
    const fn close_aside(&mut self) {
        self.text.aside_depth = 0;
        self.text.aside_keys = 0;
    }

    /// Whether an aside is open (test / introspection hook).
    #[must_use]
    #[cfg(test)]
    pub const fn in_aside(&self) -> bool {
        self.text.aside_depth > 0
    }

    /// Whether a quotation is open (test / introspection hook; [`QUOTE_OPENS`]).
    #[must_use]
    #[cfg(test)]
    pub const fn in_quote(&self) -> bool {
        self.text.quote_deg >= 0
    }

    /// The mark the next Space will confirm (test / introspection hook): 0 none, 1 `.`, 2 `!`, 3 `?`, 4 `,`, 5 `;`, 6 `:`.
    #[inline]
    #[must_use]
    pub const fn pending_mark(&self) -> u8 {
        if self.text.last_mark & MARK_DOUBLED != 0 {
            MARK_NONE
        } else {
            self.text.last_mark
        }
    }

    /// Whether the live token has spent its steering mark (test / introspection hook).
    #[must_use]
    #[cfg(test)]
    pub const fn token_marked(&self) -> bool {
        self.text.token_marked
    }
}

const WORD_LEAP_MAX_DEG: i32 = 4;

#[derive(Clone, Copy, Debug)]
struct TypedPlan {
    touch: Touch,
    deg: i32,
    lit: bool,
    level: f32,
    mallet_only: bool,
    repeat: bool,
    word_head: bool,
    rest: bool,
    answer_head: bool,
    opens_shift: bool,
    steered: bool,
    sotto: bool,
    quote: u8,
}

/// Which of §3.3's additions this synth voices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimbreStops {
    /// The 3f/4f/6f bloom behind every lit key.
    pub bloom: bool,
    /// `hue_air` on the bloom's level and pan, `ROOF_HUE_ADD_HZ` on the note.
    pub hue: bool,
    /// The two-tap air cloud on Enter and phrase rests, and the answering voice at motif word heads.
    pub room: bool,
}

impl TimbreStops {
    /// Everything: the instrument that ships.
    pub const ALL: Self = Self {
        bloom: true,
        hue: true,
        room: true,
    };
    /// Nothing: the derived line through the shipped tine — the ladder's control.
    pub const PLAIN: Self = Self {
        bloom: false,
        hue: false,
        room: false,
    };
}

impl Default for TimbreStops {
    #[inline]
    fn default() -> Self {
        Self::ALL
    }
}

impl MelodyV2 {
    #[inline]
    const fn note_verdict(&mut self, at: u32, failed: bool, green_long: bool) {
        self.text.chord = if failed {
            CHORD_AFTER_RED
        } else {
            CHORD_AFTER_ENTER
        };
        self.verdict_at_ms = if green_long { Some(at) } else { None };
    }

    #[inline]
    const fn take_verdict_lit(&mut self, at: u32) -> bool {
        let Some(mark) = self.verdict_at_ms.take() else {
            return false;
        };
        at.saturating_sub(mark) <= VERDICT_LIT_MS
    }

    #[inline]
    const fn on_typed(
        &mut self,
        at: u32,
        rank: u8,
        sing: bool,
        shifted: bool,
        class: u8,
    ) -> TypedPlan {
        let gap = at.saturating_sub(match self.last_key_ms {
            Some(k) => k,
            None => 0,
        });
        let first = self.last_key_ms.is_none();
        self.push_undo();
        let said = self.text.last_mark;
        let rank_before = self.text.prev_rank;

        let held = self.detect_autorepeat(gap, rank, first);

        let rest = !first && timing::phrase_rest_reached(gap, self.ioi_ms);
        if rest {
            let here = self.text.walk as i32;
            self.text.walk = self.nearest_lit_within(here, here) as i8;
            self.text.run_stride = 0;
            self.text.run_len = 0;
            self.relatch_motif();
            self.close_aside();
        }

        let word_head = self.text.word_pos == 0;
        let mut answer_head = false;
        if word_head {
            self.text.words_since_motif = self.text.words_since_motif.saturating_add(1);
            if (self.text.motif_len as usize) == MOTIF_LEN
                && self.text.words_since_motif >= MOTIF_EVERY_WORDS
            {
                self.text.motif_play = MOTIF_LEN as u8;
                self.text.motif_k = 0;
                self.text.words_since_motif = 0;
                answer_head = true;
            }
        }

        let lit_mask = CHORD_LOOP[self.text.chord as usize].lit;
        let from = self.text.walk as i32;
        let mut deg = self.derive(rank, gap, first, word_head);
        if word_head {
            deg = self.nearest_lit_within(deg, from);
            let sounded = deg - from;
            if sounded != 0
                && self.text.run_len >= MELODY_RUN_MAX
                && (self.text.run_stride as i32) == sounded
            {
                let away = -sounded.signum();
                let mut k = 1;
                while k <= WORD_HEAD_SNAP_DEG + 1 {
                    let c = from + away * k;
                    if c >= TUNE_DEG_LO && c <= TUNE_DEG_HI && Self::deg_is_lit_in(lit_mask, c) {
                        deg = c;
                        break;
                    }
                    k += 1;
                }
            }
            if matches!(said, MARK_PERIOD | MARK_BANG) && deg == from {
                let mut k = 1;
                while k <= STEER_MAX_DEG {
                    let c_hi = from + k;
                    if c_hi >= TUNE_DEG_LO
                        && c_hi <= TUNE_DEG_HI
                        && Self::deg_is_lit_in(lit_mask, c_hi)
                    {
                        deg = c_hi;
                        break;
                    }
                    let c_lo = from - k;
                    if c_lo >= TUNE_DEG_LO
                        && c_lo <= TUNE_DEG_HI
                        && Self::deg_is_lit_in(lit_mask, c_lo)
                    {
                        deg = c_lo;
                        break;
                    }
                    k += 1;
                }
            }
        }
        let opens = word_head || !self.text.prev_shifted;
        let opens_shift = shifted && opens;
        if shifted && opens && !first && (deg != from || word_head) {
            let raw = deg + CAPITAL_LIFT_DEG;
            let mut lifted = reflect_deg(raw);
            if lifted == from {
                lifted = reflect_deg(raw + 1);
            }
            let max_leap = from + WORD_LEAP_MAX_DEG;
            let lifted = if word_head || lifted <= max_leap {
                lifted
            } else {
                max_leap
            };
            if lifted != from {
                deg = lifted;
            }
        }
        let mark = phrase_kind(class);
        let doubled = mark != MARK_NONE && said & !MARK_DOUBLED == mark;
        let rb = rank_before as i32;
        let decimal = class == STOP && rb >= DIGIT_RANK0 && rb < DIGIT_RANK0 + 10;
        let mut steered = false;
        let quote_head = word_head || self.text.after_open;
        let mut quote = QUOTE_NONE;
        if doubled {
            // No override.
        } else if class == DASH {
            if !first {
                deg = from;
            }
        } else if class == CLOSE {
            if self.text.paren_deg >= 0 && !first {
                let to = self.text.paren_deg as i32;
                let diff = to - from;
                let clamped = if diff < -WORD_LEAP_MAX_DEG {
                    -WORD_LEAP_MAX_DEG
                } else if diff > WORD_LEAP_MAX_DEG {
                    WORD_LEAP_MAX_DEG
                } else {
                    diff
                };
                deg = from + clamped;
            }
            self.text.paren_deg = -1;
        } else if class == QUOTE && self.text.quote_deg >= 0 {
            let to = self.text.quote_deg as i32;
            let diff = to - from;
            let clamped = if diff < -WORD_LEAP_MAX_DEG {
                -WORD_LEAP_MAX_DEG
            } else if diff > WORD_LEAP_MAX_DEG {
                WORD_LEAP_MAX_DEG
            } else {
                diff
            };
            deg = from + clamped;
            self.text.quote_deg = -1;
            quote = QUOTE_CLOSES;
        } else if class == QUOTE && quote_head {
            let top = if word_head {
                TUNE_DEG_HI
            } else if from + WORD_LEAP_MAX_DEG < TUNE_DEG_HI {
                from + WORD_LEAP_MAX_DEG
            } else {
                TUNE_DEG_HI
            };
            let mut c = deg;
            while c <= top {
                if Self::deg_is_lit_in(lit_mask, c) {
                    deg = c;
                    break;
                }
                c += 1;
            }
            self.text.quote_deg = deg as i8;
            quote = QUOTE_OPENS;
        } else if mark != MARK_NONE && !self.text.token_marked && !decimal {
            deg = cadence_target(mark, from, self.text.last_cadence_deg as i32);
            if matches!(mark, MARK_PERIOD | MARK_BANG) {
                self.text.last_cadence_deg = deg as i8;
            }
            steered = true;
            self.text.token_marked = true;
        }
        if class == OPEN {
            self.text.paren_deg = deg as i8;
        }
        self.text.last_mark = if quote == QUOTE_CLOSES {
            self.text.last_mark
        } else if doubled {
            mark | MARK_DOUBLED
        } else {
            mark
        };
        let aside_before = self.text.aside_depth;
        if class == OPEN {
            let next_depth = self.text.aside_depth + 1;
            self.text.aside_depth = if next_depth < ASIDE_DEPTH_MAX {
                next_depth
            } else {
                ASIDE_DEPTH_MAX
            };
        } else if class == CLOSE {
            self.text.aside_depth = self.text.aside_depth.saturating_sub(1);
        }
        let sotto = aside_before > 0 && self.text.aside_depth > 0;
        self.text.after_open = class == OPEN;
        if self.text.aside_depth == 0 {
            self.text.aside_keys = 0;
        } else if sotto {
            self.text.aside_keys = self.text.aside_keys.saturating_add(1);
            if self.text.aside_keys >= ASIDE_MAX_KEYS {
                self.close_aside();
            }
        }
        self.note_run(deg - from);
        if !first && (self.text.motif_len as usize) < MOTIF_LEN && self.text.motif_play == 0 {
            let diff = deg - from;
            let iv = (if diff < -WORD_LEAP_MAX_DEG {
                -WORD_LEAP_MAX_DEG
            } else if diff > WORD_LEAP_MAX_DEG {
                WORD_LEAP_MAX_DEG
            } else {
                diff
            }) as i8;
            let motif_len = self.text.motif_len as usize;
            let mut ramp = motif_len == MOTIF_LEN - 1;
            let mut m_idx = 0;
            while ramp && m_idx < motif_len {
                ramp = self.text.motif[m_idx] == iv;
                m_idx += 1;
            }
            if iv != 0 && !ramp {
                self.text.motif[motif_len] = iv;
                self.text.motif_len += 1;
            }
        }
        self.text.walk = deg as i8;
        self.text.prev_shifted = shifted;
        self.steps = self.steps.saturating_add(1);

        let repeat = deg == from && !first;
        let touch = if repeat && !word_head && !steered && quote == QUOTE_NONE {
            self.text.restrike = self.text.restrike.saturating_add(1);
            Touch::ReStrike
        } else {
            self.text.restrike = 0;
            Touch::Step
        };

        self.ioi_ms = timing::ioi_next(self.ioi_ms, gap, first, rest);

        let lit = Self::deg_is_lit_in(lit_mask, self.text.walk as i32) || self.take_verdict_lit(at);
        let step_level = if lit || shifted || steered {
            1.0
        } else {
            PASSING_LEVEL
        };
        let level = if matches!(touch, Touch::Step) {
            step_level
        } else {
            let r = self.text.restrike.saturating_sub(1) as usize;
            let idx = if r < RESTRIKE_LEVELS.len() - 1 {
                r
            } else {
                RESTRIKE_LEVELS.len() - 1
            };
            step_level * RESTRIKE_LEVELS[idx]
        };
        let mallet_only = held || (sing && matches!(touch, Touch::ReStrike));
        if rank != 0 {
            self.text.prev_rank = rank;
        }
        self.last_key_ms = Some(at);
        self.space_run = false;
        self.text.word_pos = self.text.word_pos.saturating_add(1);
        self.keys_since_enter = self.keys_since_enter.saturating_add(1);
        TypedPlan {
            touch,
            deg: self.text.walk as i32,
            lit,
            level,
            mallet_only,
            repeat,
            word_head,
            rest,
            answer_head,
            opens_shift,
            steered,
            sotto,
            quote,
        }
    }

    #[inline]
    const fn derive(&mut self, rank: u8, gap: u32, first: bool, word_head: bool) -> i32 {
        let from = self.text.walk as i32;

        if !word_head && self.text.motif_play > 0 {
            let iv = self.text.motif[self.text.motif_k as usize] as i32;
            self.text.motif_k = self.text.motif_k.saturating_add(1);
            self.text.motif_play -= 1;
            let mut deg = reflect_deg(from + iv);
            if iv != 0 && deg == from {
                deg = reflect_deg(from + iv + iv.signum());
            }
            return self.break_run(from, deg);
        }

        if first {
            return from;
        }

        let (raw, repeated) = if rank != 0 && self.text.prev_rank != 0 {
            let d = (rank as i32) - (self.text.prev_rank as i32);
            (d, d == 0)
        } else {
            (gap as i32, false)
        };
        let r = fold_signed13(raw);
        let mag = stride_mag(r, repeated);
        let text_sign = if r != 0 { r.signum() } else { raw.signum() };

        let tempo = self.ioi_ms;
        let sign = if (gap as f32) < ACCEL_SHARE * tempo {
            1
        } else if (gap as f32) > DECEL_SHARE * tempo {
            -1
        } else {
            text_sign
        };
        let mut stride = mag * sign;

        if mag != 0 {
            let pull = if from - MELODY_CENTRE_DEG > MELODY_GRAVITY_DEG {
                -1
            } else if from - MELODY_CENTRE_DEG < -MELODY_GRAVITY_DEG {
                1
            } else {
                0
            };
            stride += pull;
            if stride == 0 {
                stride = pull;
            }
        }
        stride = if stride < -WORD_LEAP_MAX_DEG {
            -WORD_LEAP_MAX_DEG
        } else if stride > WORD_LEAP_MAX_DEG {
            WORD_LEAP_MAX_DEG
        } else {
            stride
        };

        let mut deg = reflect_deg(from + stride);
        if stride != 0 && deg == from {
            deg = reflect_deg(from + stride + stride.signum());
        }

        self.break_run(from, deg)
    }

    #[inline]
    const fn break_run(&self, from: i32, deg: i32) -> i32 {
        let sounded = deg - from;
        if sounded == 0
            || self.text.run_len < MELODY_RUN_MAX
            || (self.text.run_stride as i32) != sounded
        {
            return deg;
        }
        let sgn = sounded.signum();
        let cands = [
            from - sounded,
            from - sounded - sgn,
            from + sounded + sgn,
            from + sounded - sgn,
        ];
        let mut i = 0;
        while i < cands.len() {
            let c = reflect_deg(cands[i]);
            let s = c - from;
            if s != 0 && s != sounded && s.abs() <= WORD_LEAP_MAX_DEG {
                return c;
            }
            i += 1;
        }
        deg
    }

    #[inline]
    const fn note_run(&mut self, stride: i32) {
        if stride != 0 && (self.text.run_stride as i32) == stride {
            self.text.run_len = self.text.run_len.saturating_add(1);
        } else {
            let clamped = if stride < -WORD_LEAP_MAX_DEG {
                -WORD_LEAP_MAX_DEG
            } else if stride > WORD_LEAP_MAX_DEG {
                WORD_LEAP_MAX_DEG
            } else {
                stride
            };
            self.text.run_stride = clamped as i8;
            self.text.run_len = (stride != 0) as u8;
        }
    }

    #[inline]
    const fn detect_autorepeat(&mut self, gap: u32, rank: u8, first: bool) -> bool {
        if first {
            self.repeat_run = 1;
            self.prev_gap = 0;
            return false;
        }
        let regular = rank != 0
            && rank == self.text.prev_rank
            && self.prev_gap != 0
            && gap.abs_diff(self.prev_gap) <= AUTOREPEAT_JITTER_MS;
        self.repeat_run = if regular {
            self.repeat_run.saturating_add(1)
        } else {
            1
        };
        self.prev_gap = gap;
        self.repeat_run >= AUTOREPEAT_RUN || gap <= AUTOREPEAT_FLOOR_MS
    }

    #[inline]
    const fn on_space(&mut self, at: u32) -> bool {
        const _: () = assert!(CHORD_LOOP.len().is_power_of_two());
        const CHORD_MASK: u8 = CHORD_LOOP.len() as u8 - 1;
        let head = !self.space_run;
        if head {
            self.push_undo();
            let pending = self.pending_mark();
            let n = CHORD_LOOP.len() as u8;
            self.text.chord = match mark_chords(pending) {
                Some(want) => {
                    let mut chosen = (self.text.chord + 1) & CHORD_MASK;
                    let mut k = 1u8;
                    while k <= n {
                        let c = (self.text.chord + k) & CHORD_MASK;
                        if c == want[0] || c == want[1] {
                            chosen = c;
                            break;
                        }
                        k += 1;
                    }
                    chosen
                }
                None => (self.text.chord + 1) & CHORD_MASK,
            };
            if matches!(pending, MARK_PERIOD | MARK_BANG | MARK_QMARK) {
                self.relatch_motif();
            }
            self.text.token_marked = false;
            self.text.word_pos = 0;
            self.space_run = true;
            self.space_steps = 0;
        }
        self.last_key_ms = Some(at);
        self.keys_since_enter = self.keys_since_enter.saturating_add(1);
        head
    }

    #[inline]
    const fn on_enter(&mut self, at: u32) -> (bool, bool, bool) {
        let full = self.keys_since_enter >= ENTER_PICKUP_MIN_KEYS;
        let closed = matches!(self.pending_mark(), MARK_PERIOD | MARK_BANG);
        let asked = self.pending_mark() == MARK_QMARK;
        self.end_phrase();
        self.text.chord = CHORD_AFTER_ENTER;
        self.text.walk = self.nearest_lit_within(MELODY_CENTRE_DEG, MELODY_CENTRE_DEG) as i8;
        self.text.run_stride = 0;
        self.text.run_len = 0;
        self.text.prev_rank = 0;
        self.repeat_run = 1;
        self.prev_gap = 0;
        self.text.word_pos = 0;
        self.text.restrike = 0;
        self.last_key_ms = Some(at);
        self.last_enter_ms = Some(at);
        self.undo_len = 0;
        self.space_run = false;
        self.keys_since_enter = 0;
        (full, closed, asked)
    }

    #[inline]
    const fn jump_echoes_enter(&self, at: u32) -> bool {
        match self.last_enter_ms {
            Some(e) => at.saturating_sub(e) < ENTER_ECHO_SWALLOW_MS,
            None => false,
        }
    }

    #[inline]
    const fn on_jump(&mut self, at: u32) -> Option<bool> {
        let in_run = match self.cascade_at {
            Some(c) => at.saturating_sub(c) < CASCADE_RUN_MS,
            None => false,
        };
        if in_run {
            self.text.walk = self.run_theme_step();
        } else {
            let here = self.text.walk as i32;
            self.text.walk = self.nearest_lit_within(here, here) as i8;
            self.run_theme_pos = 0;
            self.run_phrase = 0;
            let _first_edge_stood_in_for = self.run_theme_step();
        }
        self.text.run_stride = 0;
        self.text.run_len = 0;
        self.text.restrike = 0;
        self.end_phrase();
        self.last_key_ms = Some(at);
        self.text.word_pos = 0;
        self.space_run = false;
        let head = match self.cascade_at {
            Some(c) => at.saturating_sub(c) >= CASCADE_EXCLUSIVE_MS,
            None => true,
        };
        self.cascade_at = Some(at);
        if head {
            self.cascade_restrike_ms = at;
            Some(true)
        } else if at.saturating_sub(self.cascade_restrike_ms) >= CASCADE_RESTRIKE_MS {
            self.cascade_restrike_ms = at;
            Some(false)
        } else {
            None
        }
    }

    #[inline]
    const fn run_theme_step(&mut self) -> i8 {
        let start = SONG_FORM[self.run_phrase as usize] as usize;
        let end = SONG_FORM[(self.run_phrase + 1) as usize] as usize;
        let pos = self.run_theme_pos as usize;
        let deg = if pos == start {
            self.run_theme_pos += 1;
            if (self.run_theme_pos as usize) == end {
                self.open_next_run_phrase();
            }
            SONG_THEME[pos]
        } else {
            self.open_next_run_phrase();
            SONG_THEME[end - 1]
        };
        reflect_deg(deg as i32) as i8
    }

    #[inline]
    const fn open_next_run_phrase(&mut self) {
        const _: () = assert!((SONG_FORM.len() - 1).is_power_of_two());
        self.run_phrase = (self.run_phrase + 1) & (SONG_FORM.len() as u8 - 2);
        self.run_theme_pos = SONG_FORM[self.run_phrase as usize];
    }

    #[inline]
    const fn on_backspace(&mut self, at: u32) {
        self.pop_undo();
        self.last_key_ms = Some(at);
        self.space_run = false;
    }

    #[inline]
    const fn on_kill(&mut self, at: u32) {
        self.undo_len = 0;
        self.end_phrase();
        self.text.word_pos = 0;
        self.last_key_ms = Some(at);
    }

    #[inline]
    const fn next_glint_deg(&mut self) -> i32 {
        let deg = self.text.walk as i32 + GLINT_DEGREES[self.glint_k as usize];
        self.glint_k = (self.glint_k + 1) % GLINT_DEGREES.len() as u8;
        deg
    }

    #[inline]
    const fn answer_deg_above(&self, deg: i32) -> i32 {
        let lit_mask = CHORD_LOOP[self.text.chord as usize].lit;
        let mut k = 1;
        while k <= 5 {
            let d = deg + k;
            if Self::deg_is_lit_in(lit_mask, d) {
                return d;
            }
            k += 1;
        }
        deg + CAPITAL_LIFT_DEG
    }
}

#[inline]
const fn fold_signed13(d: i32) -> i32 {
    (d + 6).rem_euclid(13) - 6
}

#[inline]
const fn reflect_deg(deg: i32) -> i32 {
    let mut d = deg;
    let mut i = 0;
    while i < 8 {
        if d > TUNE_DEG_HI {
            d = 2 * TUNE_DEG_HI - d;
        } else if d < TUNE_DEG_LO {
            d = 2 * TUNE_DEG_LO - d;
        } else {
            break;
        }
        i += 1;
    }
    if d < TUNE_DEG_LO {
        TUNE_DEG_LO
    } else if d > TUNE_DEG_HI {
        TUNE_DEG_HI
    } else {
        d
    }
}

#[inline]
const fn tau_v_s(ioi_s: f32) -> f32 {
    (TAU_V_BASE_S * (TAU_V_OFFSET + TAU_V_SLOPE * ioi_s)).clamp(TAU_V_MIN_S, TAU_V_MAX_S)
}

#[inline]
const fn g_ioi(ioi_s: f32) -> f32 {
    const INV_G_IOI_REF_S: f32 = 1.0 / G_IOI_REF_S;
    crate::color_math::sqrt_f32(ioi_s * INV_G_IOI_REF_S).clamp(G_IOI_MIN, 1.0)
}

#[inline]
const fn roof_hz(cps: f32, lit: bool, heat: f32, hue: f32, touch: Touch) -> f32 {
    const INV_ROOF_CPS_SPAN: f32 = 1.0 / ROOF_CPS_SPAN;
    let u = ((cps - ROOF_CPS_LO) * INV_ROOF_CPS_SPAN).clamp(0.0, 1.0);
    let plain = ROOF_PLAIN_LO_HZ + (ROOF_PLAIN_HI_HZ - ROOF_PLAIN_LO_HZ) * u;
    let base = if lit {
        (plain + ROOF_LIT_ADD_HZ).min(ROOF_MAX_HZ)
    } else {
        plain
    };
    let base = if matches!(touch, Touch::ReStrike) {
        base - RESTRIKE_ROOF_DROP_HZ
    } else {
        base
    };
    (base + ROOF_HEAT_HZ * heat.clamp(0.0, 1.0) + ROOF_HUE_ADD_HZ * hue.clamp(0.0, 1.0))
        .min(ROOF_MAX_HZ)
}

#[inline]
const fn hue_arc(hue: f32) -> f32 {
    tri(2.0 * hue.clamp(0.0, 1.0))
}

const HUE_AIR_FLOOR: f32 = 0.80;
const HUE_AIR_DEFAULT: f32 = hue_air(0.25);

#[inline]
const fn hue_air(hue: f32) -> f32 {
    let (_, cos_h) = crate::supernova::sin_cos_f32(core::f32::consts::TAU * hue);
    HUE_AIR_FLOOR + (1.0 - HUE_AIR_FLOOR) * (0.5 - 0.5 * cos_h)
}

#[inline]
const fn fold_into(f: f32, lo: f32, hi: f32) -> f32 {
    let mut f = f.clamp(1.0, 40_000.0);
    let mut i = 0;
    while i < 24 {
        if f < lo {
            f *= 2.0;
        } else if f >= hi {
            f *= 0.5;
        } else {
            break;
        }
        i += 1;
    }
    f
}

#[inline]
const fn tine(f: f32, touch: Touch, tau: f32, roof: f32, mallet_only: bool, flow: f32) -> Voice {
    let (p1, p2, p3, mallet) = match touch {
        Touch::Step => {
            let (p1, p2) = flow_partials(flow);
            (p1, p2, P3_LVL, MALLET_LVL)
        }
        Touch::ReStrike => (P1_LVL, RESTRIKE_P2_LVL, 0.0, RESTRIKE_MALLET_LVL),
    };
    let (p1, p2, p3) = if mallet_only {
        (0.0, 0.0, 0.0)
    } else {
        (p1, p2, p3)
    };
    Voice {
        dur: 3.0 * tau + TINE_DUR_TAIL_S,
        attack: TINE_ATTACK_S,
        decay: tau,
        p: [
            Partial {
                lvl: p1,
                f0: f,
                ..Partial::DEFAULT
            },
            Partial {
                lvl: p2,
                f0: f * P2_RATIO,
                decay: P2_TAU_S,
                ..Partial::DEFAULT
            },
            Partial {
                lvl: p3,
                f0: f * P3_RATIO,
                decay: P3_TAU_S,
                ..Partial::DEFAULT
            },
        ],
        n_lvl: mallet,
        n_f0: MALLET_HZ0,
        n_f1: MALLET_HZ1,
        n_glide: MALLET_GLIDE_S,
        n_q: MALLET_Q,
        n_decay: MALLET_TAU_S,
        lp_cut: roof,
        lane: LANE_TUNE,
        ..Voice::DEFAULT
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Forte {
    force: f32,
    gain: f32,
    ring: bool,
    glint_mul: f32,
    fm_turn: f32,
}

impl Forte {
    #[inline]
    const fn of(
        shifted: bool,
        class: u8,
        touch: Touch,
        felt: bool,
        word_head: bool,
        opens_shift: bool,
    ) -> Self {
        let step = matches!(touch, Touch::Step) && !felt;
        let letter = class == LETTER;
        let bang = class == BANG && (!felt || shifted);
        let weight = if (shifted && letter) || bang {
            super::SHIFT_GLYPH_GAIN
        } else if shifted {
            MARK_SHIFT_GAIN
        } else {
            1.0
        };
        let head = if shifted && letter && step && word_head {
            WORD_CAPITAL_GAIN
        } else {
            1.0
        };
        let force = if !step {
            0.0
        } else if bang {
            1.0
        } else if !shifted {
            0.0
        } else if !letter {
            0.35
        } else if word_head {
            1.0
        } else if opens_shift {
            0.8
        } else {
            0.6
        };
        let glint_mul = if shifted && letter {
            FORTE_GLINT_MUL
        } else if matches!(class, OPEN | BANG) {
            KEY_GLINT_SHIFTED_MUL
        } else if class == QUOTE {
            QUOTE_GLINT_MUL
        } else {
            1.0
        };
        Self {
            force,
            gain: weight * head,
            ring: shifted && letter && step && opens_shift,
            glint_mul,
            fm_turn: if head == 1.0 {
                FORTE_FM_TURN
            } else {
                FORTE_FM_TURN_HEAD
            },
        }
    }
}

#[inline]
const fn shifted_roof(lp_cut: f32, shifted: bool) -> f32 {
    if shifted {
        lp_cut.min(ROOF_MAX_HZ)
    } else {
        lp_cut
    }
}

#[inline]
const fn forte_index(f: f32, force: f32) -> f32 {
    let roll = (FORTE_FM_REF_HZ / f).min(1.0);
    FORTE_FM_INDEX * force * roll * roll
}

#[inline]
const fn forte(v: &mut Voice, f: f32, force: f32) {
    v.p[0].fm_ratio = FORTE_FM_RATIO;
    v.p[0].fm_i0 = forte_index(f, force);
    v.p[0].fm_tau = FORTE_FM_TAU_S;
    if v.p[1].f0 == f * P2_RATIO && v.p[1].lvl > 0.0 {
        let d = (FORTE_P2_SHIFT * force)
            .min(FORTE_P2_MAX - v.p[1].lvl)
            .max(0.0);
        v.p[0].lvl -= d;
        v.p[1].lvl += d;
        v.p[1].decay = P2_TAU_S + FORTE_P2_TAU_ADD_S * force;
    }
    v.decay = (v.decay * (1.0 + FORTE_TAU_GAIN * force))
        .min(FORTE_TAU_MAX_S)
        .max(v.decay);
    v.dur = 3.0 * v.decay + TINE_DUR_TAIL_S;
    v.lp_cut = (v.lp_cut + FORTE_ROOF_ADD_HZ * force).min(ROOF_MAX_HZ);
}

#[inline]
const fn wood(v: &mut Voice, f: f32, touch: Touch) {
    v.p[1].f0 = f * WOOD_P2_RATIO;
    v.p[1].decay = WOOD_P2_TAU_S;
    v.p[2].f0 = f * WOOD_P3_RATIO;
    v.p[2].decay = WOOD_P3_TAU_S;
    if matches!(touch, Touch::Step) {
        v.p[0].lvl = WOOD_P1_LVL;
        v.p[1].lvl = WOOD_P2_LVL;
        v.p[2].lvl = WOOD_P3_LVL;
    }
    v.n_f0 = WOOD_MALLET_HZ0;
    v.n_f1 = WOOD_MALLET_HZ1;
    v.n_q = WOOD_MALLET_Q;
    v.n_decay = WOOD_MALLET_TAU_S;
}

#[inline]
const fn pluck_class(class: u8) -> bool {
    matches!(
        class,
        QMARK | STOP | COMMA | SEMI | COLON | CLOSE | QUOTE | LINE | RISE | MATH | SIGIL
    )
}

#[inline]
const fn stop_breathes(class: u8) -> bool {
    matches!(class, STOP | COMMA | SEMI | COLON)
}

#[inline]
const fn pluck(v: &mut Voice, f: f32, class: u8, touch: Touch) {
    let step = matches!(touch, Touch::Step);
    v.p[1].f0 = f * PLUCK_P2_RATIO;
    v.p[1].decay = PLUCK_P2_TAU_S;
    v.p[2].lvl = 0.0;
    if step {
        v.p[0].lvl = PLUCK_P1_LVL;
        v.p[1].lvl = PLUCK_P2_LVL;
    }
    let (f0, f1, decay, lvl) = match class {
        LINE => (ZIP_HZ_HI, ZIP_HZ_LO, ZIP_TAU_S, ZIP_MALLET_LVL),
        RISE => (ZIP_HZ_LO, ZIP_HZ_HI, ZIP_TAU_S, ZIP_MALLET_LVL),
        _ => (PLUCK_HZ0, PLUCK_HZ1, PLUCK_NOISE_TAU_S, PLUCK_NOISE_LVL),
    };
    if matches!(class, LINE | RISE) {
        v.n_glide = ZIP_GLIDE_S;
    }
    (v.n_f0, v.n_f1, v.n_q, v.n_decay) = (f0, f1, PLUCK_Q, decay);
    if step {
        v.n_lvl = lvl;
    }
}

#[inline]
const fn breath(delay: f32) -> Voice {
    Voice {
        delay,
        dur: BREATH_DUR_S,
        attack: BREATH_ATTACK_S,
        decay: BREATH_DECAY_S,
        n_lvl: 1.0,
        n_f0: BREATH_HZ0,
        n_f1: BREATH_HZ1,
        n_glide: BREATH_GLIDE_S,
        n_q: BREATH_Q,
        lp_cut: BREATH_ROOF_HZ,
        lane: LANE_BREATH,
        ..Voice::DEFAULT
    }
}

#[inline]
const fn bass_dyad(root: f32, partner: f32, decay: f32, delay: f32) -> Voice {
    Voice {
        delay,
        dur: TAIL_DUR_PER_TAU * decay,
        attack: BASS_ATTACK_S,
        decay,
        p: [
            Partial {
                lvl: BASS_ROOT_LVL,
                f0: root,
                ..Partial::DEFAULT
            },
            Partial {
                lvl: BASS_FIFTH_LVL,
                f0: partner,
                ..Partial::DEFAULT
            },
            Partial {
                lvl: BASS_OCT_LVL,
                f0: root * 2.0,
                decay: BASS_OCT_TAU_S,
                ..Partial::DEFAULT
            },
        ],
        lp_cut: BASS_ROOF_HZ,
        lane: LANE_BASS,
        bass: true,
        ..Voice::DEFAULT
    }
}

#[inline]
const fn p1_sine(f0: f32) -> [Partial; 3] {
    [
        Partial {
            lvl: P1_LVL,
            f0,
            ..Partial::DEFAULT
        },
        Partial::DEFAULT,
        Partial::DEFAULT,
    ]
}

#[inline]
const fn met_tick() -> Voice {
    Voice {
        dur: MET_TICK_DUR_S,
        attack: MET_TICK_ATTACK_S,
        decay: MET_TICK_DECAY_S,
        n_lvl: 1.0,
        n_f0: MET_TICK_HZ0,
        n_f1: MET_TICK_HZ1,
        n_glide: MET_TICK_GLIDE_S,
        n_q: MET_TICK_Q,
        lp_cut: MET_TICK_ROOF_HZ,
        lane: LANE_METEOR,
        ..Voice::DEFAULT
    }
}

#[inline]
const fn head_breath(root: f32) -> Voice {
    let twinkle = fold_into(root, GLINT_LO_HZ, GLINT_HI_HZ);
    Voice {
        p: [
            Partial {
                lvl: SPACE_TWINKLE_LEVEL,
                f0: twinkle,
                ..Partial::DEFAULT
            },
            Partial {
                lvl: SPACE_TWINKLE_UNDER,
                f0: twinkle * 0.5,
                ..Partial::DEFAULT
            },
            Partial::DEFAULT,
        ],
        ..breath(0.0)
    }
}

#[inline]
const fn bloom(deg: i32, roof: f32, tau_v: f32) -> Voice {
    const INV_TAU_V_MAX_S: f32 = 1.0 / TAU_V_MAX_S;
    let decay = (BLOOM_DECAY_TAU_MUL * tau_v).min(BLOOM_DECAY_S);
    let head = (tau_v * INV_TAU_V_MAX_S).clamp(BLOOM_HEAD_MIN_MUL, 1.0);
    Voice {
        delay: BLOOM_DELAY_S * head,
        dur: (3.0 * decay + TINE_DUR_TAIL_S).min(BLOOM_DUR_S),
        attack: BLOOM_ATTACK_S * head,
        decay,
        p: [
            Partial {
                lvl: BLOOM_LVL[0],
                f0: penta(TINE_BASE_HZ, deg + BLOOM_DEGREES[0]),
                decay: BLOOM_TAU[0],
                ..Partial::DEFAULT
            },
            Partial {
                lvl: BLOOM_LVL[1],
                f0: penta(TINE_BASE_HZ, deg + BLOOM_DEGREES[1]),
                decay: BLOOM_TAU[1],
                ..Partial::DEFAULT
            },
            Partial {
                lvl: BLOOM_LVL[2],
                f0: penta(TINE_BASE_HZ, deg + BLOOM_DEGREES[2]),
                decay: BLOOM_TAU[2],
                ..Partial::DEFAULT
            },
        ],
        tw_rate: BLOOM_TW_RATE,
        tw_depth: BLOOM_TW_DEPTH,
        lp_cut: roof + BLOOM_ROOF_ADD_HZ,
        lane: LANE_BLOOM,
        ..Voice::DEFAULT
    }
}

impl TrailSynth {
    /// The timbre ladder's seam (test hook).
    #[cfg(test)]
    pub const fn set_v2_timbre_stops(&mut self, stops: TimbreStops) {
        self.v2.stops = stops;
    }

    #[inline]
    const fn v2_at_ms(&self, meta: EventMeta) -> u32 {
        if meta.at_ms != 0 {
            meta.at_ms
        } else {
            (self.clock_s * 1000.0) as u64 as u32
        }
    }

    #[inline]
    const fn v2_velocity(&mut self, db: f32) -> f32 {
        const LN10_OVER_20: f32 = core::f32::consts::LN_10 * 0.05;
        let u = self.rnd_in(-1.0, 1.0);
        crate::color_math::exp_f32(u * db * LN10_OVER_20)
    }

    #[inline]
    const fn v2_pan(&mut self, pan: f32) -> f32 {
        const INV_PAN_LAW_SCALE: f32 = 1.0 / PAN_LAW_SCALE;
        pan + self.rnd_in(-PAN_JITTER, PAN_JITTER) * INV_PAN_LAW_SCALE
    }

    #[inline]
    const fn v2_hand_back_key(&mut self, at: u32) {
        if self.v2_key_pending && self.v2.at_boundary(at) {
            self.song_key = 0;
            self.v2_key_pending = false;
        }
    }

    #[inline]
    const fn v2_damp_same_pitch(&mut self, lane: u8, f: f32) {
        let mut i = 0;
        while i < self.voices.len() {
            let v = &mut self.voices[i];
            if v.on && v.lane == lane && v.damp <= 0.0 && (v.p[0].f0 - f).abs() < SAME_PITCH_HZ {
                v.damp = LANE_FADE_STEAL_S;
                v.damp0 = LANE_FADE_STEAL_S;
            }
            i += 1;
        }
    }

    #[inline]
    const fn v2_live_ting_at(&self, f: f32) -> bool {
        const RATIO_HI: f32 =
            crate::color_math::exp_f32(SAME_PITCH_CENTS * (core::f32::consts::LN_2 / 1200.0));
        const RATIO_LO: f32 =
            crate::color_math::exp_f32(-SAME_PITCH_CENTS * (core::f32::consts::LN_2 / 1200.0));
        let Some((slot, born)) = self.v2.lift else {
            return false;
        };
        let v = &self.voices[slot as usize];
        let f0 = v.p[0].f0;
        v.on && v.born == born
            && v.lane == LANE_TING
            && v.damp <= 0.0
            && f0 > f * RATIO_LO
            && f0 < f * RATIO_HI
    }

    #[inline]
    const fn v2_duck_ting(&mut self, to: f32) {
        #[cfg(test)]
        if self.ting_unducked {
            return;
        }
        let Some((slot, born)) = self.v2.lift else {
            return;
        };
        let v = &mut self.voices[slot as usize];
        if v.on
            && v.born == born
            && v.lane == LANE_TING
            && v.damp <= 0.0
            && v.t >= 0.0
            && v.pan_glide_s <= 0.0
        {
            (v.gl1, v.gr1) = (v.gl * to, v.gr * to);
            v.pan_glide_s = TING_DUCK_RAMP_S;
            v.pan_k = 1.0 / TING_DUCK_RAMP_S;
            v.pan_t0 = v.t;
        }
    }

    #[inline]
    const fn v2_spawn(&mut self, proto: Voice, gain: f32, pan: f32) -> Option<(u8, u32)> {
        let Some(idx) = self.spawn(proto, gain, pan) else {
            return None;
        };
        Some((idx as u8, self.voices[idx].born))
    }

    #[inline]
    const fn v2_spawn_ph0(
        &mut self,
        proto: Voice,
        gain: f32,
        pan: f32,
        ph0: Option<f32>,
    ) -> Option<(u8, u32)> {
        let tw_ph = self.rnd();
        let drawn = [self.rnd(), self.rnd(), self.rnd()];
        let ph = [
            match ph0 {
                Some(p) => p,
                None => drawn[0],
            },
            drawn[1],
            drawn[2],
        ];
        let Some(idx) = self.spawn_seeded(proto, gain, pan, tw_ph, ph) else {
            return None;
        };
        Some((idx as u8, self.voices[idx].born))
    }

    #[inline]
    const fn v2_flow_echo_phase(&self, fundamental: f32, delay_s: f32) -> Option<f32> {
        let Some((slot, born)) = self.v2.lead else {
            return None;
        };
        let lead = &self.voices[slot as usize];
        let p2 = lead.p[1];
        if lead.on
            && lead.born == born
            && lead.lane == LANE_TUNE
            && p2.lvl > 0.0
            && p2.glide <= 0.0
            && p2.f0 == fundamental
        {
            Some((p2.ph + p2.f0 * delay_s + FLOW_ECHO_PHASE).fract())
        } else {
            None
        }
    }

    #[inline]
    const fn v2_lock_hammer(&mut self, who: Option<(u8, u32)>, turn: f32) {
        let Some((slot, born)) = who else { return };
        let v = &mut self.voices[slot as usize];
        if v.on && v.born == born && v.p[0].fm_ratio > 0.0 {
            v.p[0].fm_ph = (v.p[0].fm_ratio * v.p[0].ph + turn).fract();
        }
    }

    #[inline]
    const fn v2_spawn_decoration(
        &mut self,
        proto: Voice,
        gain: f32,
        pan: f32,
    ) -> Option<(u8, u32)> {
        if self.all_voices_on() {
            return None;
        }
        self.v2_spawn(proto, gain, pan)
    }

    #[inline]
    const fn v2_damp(&mut self, who: Option<(u8, u32)>, ramp: f32) {
        let Some((slot, born)) = who else { return };
        let v = &mut self.voices[slot as usize];
        if v.on && v.born == born && v.damp <= 0.0 {
            v.damp = ramp;
            v.damp0 = ramp;
        }
    }

    #[inline]
    const fn v2_cancel_unsounded(&mut self, who: Option<(u8, u32)>) {
        let Some((slot, born)) = who else { return };
        let v = &mut self.voices[slot as usize];
        if v.on && v.born == born && v.t < 0.0 {
            v.on = false;
        }
    }

    #[inline]
    const fn v2_retire(&mut self, who: Option<(u8, u32)>, ramp: f32) {
        let Some((slot, born)) = who else { return };
        let v = &mut self.voices[slot as usize];
        if v.on && v.born == born {
            if v.t < 0.0 {
                v.on = false;
            } else if v.damp <= 0.0 {
                v.damp = ramp;
                v.damp0 = ramp;
            }
        }
    }

    #[cfg(test)]
    const fn mute_all_lanes_but(&mut self, lane: u8) {
        let mut i = 0;
        while i < self.voices.len() {
            if self.voices[i].lane != lane {
                self.voices[i].gl = 0.0;
                self.voices[i].gr = 0.0;
            }
            i += 1;
        }
    }

    /// The v2 admission path (§16 row 9), reached only from [`TrailSynth::push_meta`]'s top-of-function branch, never by a v1 event.
    #[inline]
    pub(super) const fn push_v2(&mut self, ev: SoundEvent, meta: EventMeta) {
        // The verdict forks first, above the trail preamble, none of which
        // applies: the bus is already latched by the keyed Enter, the sky's
        // hue does not drift, the bed is not kicked, and it does not count
        // toward the rate estimate (it is not a keystroke).
        if let SoundGesture::Output(OutputGesture::Verdict { failed, ice }) = ev.kind {
            let at = self.v2_at_ms(meta);
            return self.v2_verdict(&ev, at, failed, ice);
        }
        let SoundGesture::Trail(kind) = ev.kind else {
            return;
        };
        let secret =
            matches!(kind, SoundKind::Typed | SoundKind::Space) && meta.glyph_class == SECRET;
        let kind = if secret { SoundKind::Typed } else { kind };
        {
            let arc = hue_arc(ev.hue);
            if self.v2_latched {
                const INV_BED_HUE_TAU_S: f32 = -1.0 / BED_HUE_TAU_S;
                let k = 1.0 - crate::color_math::exp_f32(self.since_event * INV_BED_HUE_TAU_S);
                self.bed.hue_s += (arc - self.bed.hue_s) * k;
            } else {
                self.bed.hue_s = arc;
            }
        }
        self.v2_latched = true;
        self.bed_style = ev.style;
        self.bed_voice = ev.voice;
        self.tone = ev.tone;
        if !secret {
            self.v2.flow = meta.flow;
        }
        if ev.bed {
            self.kick_bed(kind, ev.gain);
        }
        const INV_RATE_TAU: f32 = -1.0 / 0.6;
        self.rate = self.rate * crate::color_math::exp_f32(self.since_event * INV_RATE_TAU) + 1.0;
        self.since_event = 0.0;
        let at = self.v2_at_ms(meta);

        match kind {
            SoundKind::Typed if secret => {
                self.since_voice = 0.0;
                self.damp_pending_shimmer();
                self.v2_secret(&ev);
            }
            SoundKind::Typed => {
                self.since_voice = 0.0;
                self.damp_pending_shimmer();
                self.v2_typed(&ev, meta, at);
            }
            SoundKind::Space => {
                self.since_voice = 0.0;
                self.damp_pending_shimmer();
                self.v2_space(&ev, at);
            }
            SoundKind::Backspace => {
                self.v2_damp(self.v2.lead, ERASE_MUTE_S);
                self.v2.lead = None;
                self.v2_retire(self.v2.ring, ERASE_MUTE_S);
                self.v2.ring = None;
                self.v2_retire(self.v2.graft, ERASE_MUTE_S);
                self.v2.graft = None;
                self.v2.prev_class = LETTER;
                self.v2.on_backspace(at);
                if self.since_erase < ERASE_MIN_GAP {
                    return;
                }
                self.erase_run = if self.since_erase <= HELD_ERASE_RUN_WINDOW {
                    self.erase_run.saturating_add(1)
                } else {
                    1
                };
                self.since_erase = 0.0;
                self.damp_pending_shimmer();
                self.design_trail(ev, kind, 1.0, 0.0);
            }
            SoundKind::Kill | SoundKind::KillWord => {
                self.since_voice = 0.0;
                self.damp_pending_shimmer();
                let mut i = 0;
                while i < self.voices.len() {
                    let v = &mut self.voices[i];
                    i += 1;
                    if !v.on || !matches!(v.lane, LANE_TUNE | LANE_GRAFT) {
                        continue;
                    }
                    if v.lane == LANE_GRAFT && v.t < 0.0 {
                        v.on = false;
                    } else if v.damp <= 0.0 {
                        v.damp = ERASE_MUTE_S;
                        v.damp0 = ERASE_MUTE_S;
                    }
                }
                self.v2.lead = None;
                self.v2.ring = None;
                self.v2.graft = None;
                self.v2.prev_class = LETTER;
                self.v2.on_kill(at);
                self.design_trail(ev, kind, 1.0, 0.0);
            }
            SoundKind::Poof => self.design_trail(ev, kind, 1.0, 0.0),
            SoundKind::Shift => self.v2_shift(&ev),
            SoundKind::Navigation | SoundKind::Glide { .. } | SoundKind::Sweep { .. } => {
                self.v2_nav_tick(&ev);
            }
            SoundKind::Land => {}
            SoundKind::Jump => {
                // A keyed Return's own line-feed echo is not a cascade (A26):
                // a Jump within `ENTER_ECHO_SWALLOW_MS` of that Enter is
                // swallowed whole (no brrrring, no melody step, no beat).
                if self.v2.jump_echoes_enter(at) {
                    return;
                }
                self.since_voice = 0.0;
                self.v2_cascade(&ev, at);
            }
            SoundKind::Enter { cells } => {
                self.since_voice = 0.0;
                self.damp_pending_shimmer();
                self.v2_enter(&ev, at, cells);
            }
            SoundKind::Meteor { dir, cells, armed } => {
                self.since_voice = 0.0;
                self.v2_meteor(&ev, meta, dir, cells, armed);
            }
            SoundKind::MeteorArm { dir } => self.v2_meteor_arm(&ev, meta, dir),
            SoundKind::Stardust { twinkle_hz } => self.v2_glint(&ev, twinkle_hz),
            SoundKind::Strum => {
                self.since_voice = 0.0;
                self.v2_strum(&ev);
            }
        }
    }
}

const SECRET_DEG: i32 = 0;
const SECRET_TAU_S: f32 = 0.069;

const ERASE_MUTE_S: f32 = 0.040;

impl TrailSynth {
    #[inline]
    const fn v2_typed(&mut self, ev: &SoundEvent, meta: EventMeta, at: u32) {
        self.v2_hand_back_key(at);
        let sing = self.sing > 0.0;
        let class = meta.glyph_class;
        let plan = self.v2.on_typed(at, meta.rank, sing, ev.shifted, class);
        let stops = self.v2.stops;
        let classed = !plan.mallet_only && class != LETTER;
        let cadence_stop = classed && class == STOP && plan.steered;
        let plucked = classed && pluck_class(class) && !cadence_stop;
        let pause_mark = classed && plan.steered && matches!(class, COMMA | SEMI | COLON);
        let flow = self.v2.flow;
        let ioi_s = self.v2.ioi_ms * 0.001;
        let cps = 1.0 / ioi_s;
        let step_touch = matches!(plan.touch, Touch::Step);
        let tau_mul = if step_touch { 1.0 } else { RESTRIKE_TAU_MUL };
        let tau = tau_v_s(ioi_s) * tau_mul;
        let lit_roof = ev.shifted
            || (classed && matches!(class, OPEN | BANG))
            || cadence_stop
            || (plan.lit && (step_touch || plan.word_head));
        let arc = if stops.hue { hue_arc(ev.hue) } else { 0.0 };
        let roof = roof_hz(cps, lit_roof, ev.heat, arc, plan.touch);
        let roof = if plan.sotto {
            roof * ASIDE_ROOF_MUL
        } else {
            roof
        };
        let aside_gain = if plan.sotto { ASIDE_GAIN } else { 1.0 };
        let deg = plan.deg + (self.song_key as i32);
        let f = penta(TINE_BASE_HZ, deg);
        let hammer = Forte::of(
            ev.shifted,
            class,
            plan.touch,
            plan.mallet_only,
            plan.word_head,
            plan.opens_shift,
        );
        if plan.repeat {
            self.v2_damp(self.v2.lead, LANE_FADE_STEAL_S);
        }
        if !plan.mallet_only && self.v2_live_ting_at(f) {
            self.v2_damp(self.v2.lift, LANE_FADE_STEAL_S);
        }
        if hammer.ring {
            self.v2_duck_ting(TING_DUCK);
        } else if ev.shifted && classed {
            self.v2_duck_ting(TING_DUCK_MARK);
        }
        let vel_db = if step_touch {
            VEL_DB_STEP
        } else {
            VEL_DB_RESTRIKE
        };
        let vel = self.v2_velocity(vel_db);
        let pan = self.v2_pan(ev.pan);
        let g = g_ioi(ioi_s) * flow_key_headroom(flow);
        let phrase_gain = if pause_mark { PAUSE_MARK_GAIN } else { 1.0 };
        let gain =
            ev.gain * KEY_TINE_TRIM * plan.level * g * vel * hammer.gain * phrase_gain * aside_gain;
        let tau_k = if classed && class == DIGIT {
            tau * DIGIT_TAU_MUL
        } else if plucked {
            (tau * PLUCK_TAU_MUL).max(PLUCK_TAU_MIN_S)
        } else if cadence_stop {
            (tau * CADENCE_STOP_TAU_MUL).min(MARK_TAU_MAX_S).max(tau)
        } else if classed && class == DASH {
            (tau * DASH_TAU_MUL).min(MARK_TAU_MAX_S).max(tau)
        } else {
            tau
        };
        let mut voice = tine(f, plan.touch, tau_k, roof, plan.mallet_only, flow);
        if classed && class == DIGIT {
            wood(&mut voice, f, plan.touch);
        } else if plucked {
            pluck(&mut voice, f, class, plan.touch);
        }
        if hammer.force > 0.0 {
            forte(&mut voice, f, hammer.force);
        }
        self.v2.lead = self.v2_spawn(voice, gain, pan);
        if hammer.force > 0.0 {
            self.v2_lock_hammer(self.v2.lead, hammer.fm_turn);
        }
        self.v2.ring = None;
        self.v2.graft = None;
        let blooms = stops.bloom && plan.lit && step_touch && !plan.mallet_only && !plucked;
        let clouds = stops.room && plan.rest && !plan.mallet_only;
        let bloom_gain = if blooms || clouds {
            let (air, spread) = if stops.hue {
                (hue_air(ev.hue), BLOOM_SPREAD * (arc - 0.5))
            } else {
                (HUE_AIR_DEFAULT, 0.0)
            };
            let bloom_gain =
                ev.gain * KEY_TINE_TRIM * plan.level * g * vel * BLOOM_LEVEL * air * aside_gain;
            if blooms {
                let mut halo = bloom(deg, roof, tau);
                halo.lp_cut = shifted_roof(halo.lp_cut, ev.shifted);
                self.v2_spawn(halo, bloom_gain, pan + spread);
            }
            bloom_gain
        } else {
            0.0
        };

        if stops.bloom && step_touch && !plan.mallet_only {
            if classed && class == DIGIT {
                let raw_d = (meta.rank as i32) - DIGIT_RANK0;
                let d = if raw_d < 0 {
                    0
                } else if raw_d > 9 {
                    9
                } else {
                    raw_d
                };
                let deg = COUNT_GLINT_DEG0 + d % 5 + (self.song_key as i32);
                let mul = if d < 5 {
                    COUNT_GLINT_LOW_MUL
                } else {
                    KEY_GLINT_SHIFTED_MUL
                };
                self.v2_glint_deg_at(
                    ev,
                    deg,
                    0,
                    GLINT_LEVEL * KEY_GLINT_LEVEL_MUL * mul * g * aside_gain,
                    vel,
                    KEY_GLINT_DELAY_S,
                );
            } else {
                let closes_quote = classed && plan.quote == QUOTE_CLOSES;
                let mul = if closes_quote { 1.0 } else { hammer.glint_mul };
                self.v2_glint_at(
                    ev,
                    0,
                    GLINT_LEVEL * KEY_GLINT_LEVEL_MUL * mul * g * aside_gain,
                    vel,
                    KEY_GLINT_DELAY_S,
                );
                let second = match class {
                    BANG if classed => Some(BANG_GLINT2_DELAY_S),
                    QUOTE if classed && !closes_quote => Some(QUOTE_GLINT2_DELAY_S),
                    _ => None,
                };
                if let Some(delay) = second {
                    self.v2_glint_at(
                        ev,
                        0,
                        GLINT_LEVEL * KEY_GLINT_LEVEL_MUL * mul * g * aside_gain,
                        vel,
                        delay,
                    );
                }
            }
        }

        let closes_its_open = class == CLOSE && self.v2.prev_class == OPEN;
        if stops.bloom && (step_touch || closes_its_open) && classed {
            match class {
                QMARK | MATH | COLON if class != COLON || plan.steered => {
                    let (up, attack, delay, level) = if class == QMARK {
                        let up = if deg.rem_euclid(5) == 1 {
                            QUEST_RISE_D_DEG
                        } else {
                            QUEST_RISE_DEG
                        };
                        (up, TINE_ATTACK_S, QUEST_RISE_DELAY_S, QUEST_RISE_LEVEL)
                    } else {
                        (FIFTH_DEG, BLOOM_ATTACK_S, FIFTH_DELAY_S, FIFTH_LEVEL)
                    };
                    let mut graft = tine(
                        penta(TINE_BASE_HZ, deg + up),
                        Touch::Step,
                        tau,
                        roof,
                        false,
                        flow,
                    );
                    graft.n_lvl = 0.0;
                    graft.p[1].lvl = 0.0;
                    graft.p[2].lvl = 0.0;
                    graft.attack = attack;
                    graft.delay = delay;
                    graft.lane = LANE_GRAFT;
                    self.v2.graft = self.v2_spawn_decoration(graft, gain * level, pan);
                }
                OPEN | CLOSE => {
                    let step = if class == OPEN { TINK_DEG } else { -TINK_DEG };
                    let t_deg = reflect_deg(plan.deg + step) + (self.song_key as i32);
                    self.v2.graft = self.v2_tink(t_deg, roof, gain, pan);
                }
                _ => {}
            }
        }
        self.v2.prev_class = if classed { class } else { LETTER };
        if classed && plan.steered && stop_breathes(class) {
            let pan = self.v2_pan(ev.pan);
            let level = if class == STOP { 1.0 } else { PAUSE_BREATH_MUL };
            self.v2_spawn_decoration(
                breath(STOP_BREATH_DELAY_S),
                ev.gain * KEY_TINE_TRIM * BREATH_LEVEL * g * level * aside_gain,
                pan,
            );
        }

        let struck = step_touch && !plan.mallet_only;
        let ring_deg = deg + FLOW_ECHO_OCTAVE_DEG + CAP_RING_LIFT_DEG;
        let ring_hz = if hammer.ring {
            penta(TINE_BASE_HZ, ring_deg)
        } else {
            0.0
        };
        let rings = hammer.ring && !self.v2_live_ting_at(ring_hz);
        if ev.shifted && struck && !rings && !self.all_voices_on() {
            let mut r = 0;
            while r < 4 {
                let _ = self.rnd();
                r += 1;
            }
        }
        let echoes = flow > 0.0 && plan.lit && !plucked && !ev.shifted;
        if (echoes || rings) && struck {
            let ring_gain = if ev.shifted {
                gain * CAP_RING_LEVEL.max(FLOW_ECHO_LEVEL * flow)
            } else {
                gain * FLOW_ECHO_LEVEL * flow
            };
            let echo_hz = if rings {
                ring_hz
            } else {
                penta(TINE_BASE_HZ, deg + FLOW_ECHO_OCTAVE_DEG)
            };
            let mut echo = tine(echo_hz, Touch::Step, tau, roof, false, flow);
            echo.n_lvl = 0.0;
            if rings {
                echo.p[2].lvl = 0.0;
                echo.attack = CAP_RING_ATTACK_S;
                echo.delay = CAP_RING_DELAY_S;
                echo.lane = LANE_GRAFT;
                self.v2.ring = self.v2_spawn_decoration(echo, ring_gain, pan);
            } else {
                echo.attack = BLOOM_ATTACK_S;
                echo.delay = FLOW_ECHO_DELAY_S;
                echo.lane = LANE_BLOOM;
                let ph0 = self.v2_flow_echo_phase(echo.p[0].f0, FLOW_ECHO_DELAY_S);
                self.v2_spawn_ph0(echo, ring_gain, pan, ph0);
            }
        }

        if stops.room {
            // The answering voice (§3.3): the head opening the line's reply to
            // its subject is answered from above, a beat later, on the nearest
            // lit tone, in the droppable lane.
            if plan.answer_head && !plan.mallet_only {
                let a_deg = self.v2.answer_deg_above(plan.deg) + (self.song_key as i32);
                let mut answer = tine(
                    penta(TINE_BASE_HZ, a_deg),
                    Touch::Step,
                    tau,
                    roof,
                    false,
                    flow,
                );
                answer.n_lvl = 0.0;
                answer.delay = ANSWER_DELAY_S;
                answer.lane = LANE_BLOOM;
                let vel = self.v2_velocity(VEL_DB_MOTION);
                let pan = self.v2_pan(ev.pan);
                self.v2_spawn(
                    answer,
                    ev.gain * KEY_TINE_TRIM * ANSWER_LEVEL * g * vel * aside_gain,
                    pan,
                );
            }
            if clouds {
                self.v2_air_cloud(deg, roof, bloom_gain, pan, 0.0, ev.shifted);
            }
        }
    }

    #[inline]
    const fn v2_air_cloud(
        &mut self,
        deg: i32,
        roof: f32,
        bloom_gain: f32,
        pan: f32,
        after: f32,
        shifted: bool,
    ) {
        let mut base_tap = bloom(deg, roof, TAU_V_MAX_S);
        base_tap.lp_cut = shifted_roof(base_tap.lp_cut, shifted);
        let mut k = 0;
        while k < AIR_TAP_DELAY_S.len() {
            let mut tap = base_tap;
            tap.delay = after + AIR_TAP_DELAY_S[k];
            let side = if k & 1 == 0 {
                -AIR_TAP_PAN
            } else {
                AIR_TAP_PAN
            };
            self.v2_spawn(tap, bloom_gain * AIR_TAP_LEVEL[k], pan + side);
            k += 1;
        }
    }

    #[inline]
    const fn v2_space(&mut self, ev: &SoundEvent, at: u32) {
        self.v2.ring = None;
        self.v2.graft = None;
        self.v2.prev_class = LETTER;
        let head = self.v2.on_space(at);
        let g = g_ioi(self.v2.ioi_ms * 0.001);
        let ci = self.v2.text.chord as usize;
        let chord = CHORD_LOOP[ci];
        let root = penta(
            BASS_BASE_HZ * CHORD_ROOT_RATIO[chord.root],
            self.song_key as i32,
        );
        if head {
            self.v2_damp(self.v2.bass, LANE_FADE_STEAL_S);
            let decay = lerp(BASS_DECAY_S, FLOW_BASS_DECAY_S, self.v2.flow);
            let voice = bass_dyad(root, root * chord.partner, decay, 0.0);
            let vel = self.v2_velocity(VEL_DB_MOTION);
            let gain = ev.gain * KEY_TINE_TRIM * BASS_LEVEL * g * vel;
            self.v2.bass = self.v2_spawn(voice, gain, 0.0);
            self.v2.last_step_ms = at;
        }
        let pan = self.v2_pan(ev.pan);
        let exhale = if head { head_breath(root) } else { breath(0.0) };
        self.v2_spawn(exhale, ev.gain * KEY_TINE_TRIM * BREATH_LEVEL * g, pan);
        if !head && at.saturating_sub(self.v2.last_step_ms) >= SPACE_STEP_MIN_GAP_MS {
            self.v2.last_step_ms = at;
            self.v2.space_steps = self.v2.space_steps.saturating_add(1);
            let f = indent_step_hz(ci, root, self.v2.space_steps, self.song_key as i32);
            let mut step = tine(
                f,
                Touch::Step,
                SPACE_STEP_TAU_S,
                ROOF_PLAIN_LO_HZ,
                false,
                0.0,
            );
            step.lane = LANE_GLINT;
            let pan = self.v2_pan(ev.pan);
            self.v2_spawn(step, ev.gain * KEY_TINE_TRIM * SPACE_STEP_LEVEL * g, pan);
        }
    }

    #[inline]
    const fn v2_shift(&mut self, ev: &SoundEvent) {
        const _: () = assert!(TING_PICK.len() == super::SHIFT_ROTATION.len());
        self.v2_damp(self.v2.lift, LANE_FADE_STEAL_S);
        let k = self.shift_step as usize;
        self.shift_step = (self.shift_step + 1) % super::SHIFT_ROTATION.len() as u8;
        let class = TING_TONES[CHORD_LOOP[self.v2.text.chord as usize].root][TING_PICK[k]];
        let f = ting_fold(penta(TINE_BASE_HZ, class + (self.song_key as i32)));
        let pan = self.v2_pan(ev.pan);
        self.v2.lift = self.v2_spawn(ting(f), ev.gain * KEY_TINE_TRIM * TING_LEVEL, pan);
    }

    #[inline]
    const fn v2_secret(&mut self, ev: &SoundEvent) {
        let f = penta(TINE_BASE_HZ, SECRET_DEG + (self.song_key as i32));
        self.v2_damp_same_pitch(LANE_TUNE, f);
        let voice = tine(f, Touch::Step, SECRET_TAU_S, ROOF_PLAIN_LO_HZ, false, 0.0);
        self.v2_spawn(voice, ev.gain * KEY_TINE_TRIM, 0.0);
    }

    #[inline]
    const fn v2_nav_tick(&mut self, ev: &SoundEvent) {
        let deg = (self.v2.text.walk as i32) + (self.song_key as i32);
        let f = penta(TINE_BASE_HZ, deg);
        self.v2_damp_same_pitch(LANE_TUNE, f);
        let voice = Voice {
            dur: NAV_DUR_S,
            attack: NAV_ATTACK_S,
            decay: NAV_DECAY_S,
            p: p1_sine(f),
            lp_cut: ROOF_PLAIN_LO_HZ,
            lane: LANE_TUNE,
            ..Voice::DEFAULT
        };
        let pan = self.v2_pan(ev.pan);
        self.v2_spawn(voice, ev.gain * KEY_TINE_TRIM * NAV_LEVEL, pan);
    }

    #[inline]
    const fn v2_lit_roof(&self, ev: &SoundEvent) -> f32 {
        let arc = if self.v2.stops.hue {
            hue_arc(ev.hue)
        } else {
            0.0
        };
        let cps = 1000.0 / self.v2.ioi_ms;
        roof_hz(cps, true, ev.heat, arc, Touch::Step)
    }

    #[inline]
    const fn v2_cascade(&mut self, ev: &SoundEvent, at: u32) {
        self.v2_hand_back_key(at);
        let Some(head) = self.v2.on_jump(at) else {
            return;
        };
        let ioi_s = self.v2.ioi_ms * 0.001;
        let tau = tau_v_s(ioi_s);
        let cps = 1.0 / ioi_s;
        let arc = if self.v2.stops.hue {
            hue_arc(ev.hue)
        } else {
            0.0
        };
        let base = (self.v2.text.walk as i32) + (self.song_key as i32);
        if head {
            let roof = roof_hz(cps, true, ev.heat, arc, Touch::Step);
            let gain_base = ev.gain * KEY_TINE_TRIM;
            let mut k = 0;
            while k < CASCADE_DEGREES.len() {
                let f = penta(TINE_BASE_HZ, base + CASCADE_DEGREES[k]);
                let mut voice = tine(f, Touch::Step, tau, roof, false, self.v2.flow);
                voice.delay = CASCADE_DELAYS_S[k];
                voice.lane = LANE_CASCADE;
                let pan = if k & 1 == 0 { ev.pan } else { -ev.pan };
                let gain = gain_base * CASCADE_LEVELS[k];
                self.v2_spawn(voice, gain, pan);
                k += 1;
            }
        } else {
            let f = penta(TINE_BASE_HZ, base + CASCADE_RESTRIKE_DEG);
            let mut voice = tine(
                f,
                Touch::ReStrike,
                tau * RESTRIKE_TAU_MUL,
                roof_hz(cps, false, ev.heat, arc, Touch::ReStrike),
                false,
                self.v2.flow,
            );
            voice.lane = LANE_CASCADE;
            let gain = ev.gain * KEY_TINE_TRIM * CASCADE_RESTRIKE_LEVEL;
            self.v2_spawn(voice, gain, ev.pan);
        }
    }

    #[inline]
    const fn v2_strum(&mut self, ev: &SoundEvent) {
        self.v2.ring = None;
        self.v2.graft = None;
        self.v2.prev_class = LETTER;
        let lit = sky_bed_degrees(self.v2.text.chord as usize);
        let degrees = [lit[0], lit[1], lit[2], lit[0] + 5];
        let tau = tau_v_s(self.v2.ioi_ms * 0.001);
        let roof = self.v2_lit_roof(ev);
        let base = self.song_key as i32;
        let gain_base = ev.gain * KEY_TINE_TRIM * STRUM_TRIM;
        let mut k = 0;
        while k < degrees.len() {
            let f = penta(TINE_BASE_HZ, base + degrees[k]);
            let mut voice = tine(f, Touch::Step, tau, roof, false, self.v2.flow);
            voice.delay = STRUM_DELAYS_S[k];
            voice.lane = LANE_CASCADE;
            let gain = gain_base * STRUM_SHAPE[k];
            self.v2_spawn(voice, gain, ev.pan);
            k += 1;
        }
    }

    #[inline]
    const fn v2_enter(&mut self, ev: &SoundEvent, at: u32, cells: u16) {
        self.v2.ring = None;
        self.v2.graft = None;
        self.v2.prev_class = LETTER;
        self.v2_hand_back_key(at);
        let (full, closed, asked) = self.v2.on_enter(at);
        let walk = self.v2.text.walk;
        let t = timing::flight_ms(cells as f32) * 0.001;
        let ioi_s = self.v2.ioi_ms * 0.001;
        let tau = tau_v_s(ioi_s);
        let stops = self.v2.stops;
        let roof = self.v2_lit_roof(ev);
        let key = self.song_key as i32;
        let resolution = if walk <= CAD_RESOLUTION_SPLIT {
            CAD_RESOLUTION_LOW_DEG
        } else {
            CAD_RESOLUTION_HIGH_DEG
        };
        let resolution = if asked {
            resolution + CAD_QUESTION_LIFT_DEG
        } else {
            resolution
        };
        let home = if full { resolution } else { walk as i32 };
        if stops.room {
            let air = if stops.hue {
                hue_air(ev.hue)
            } else {
                HUE_AIR_DEFAULT
            };
            let g = g_ioi(ioi_s);
            let bloom_gain = ev.gain * KEY_TINE_TRIM * g * BLOOM_LEVEL * air;
            self.v2_air_cloud(home + key, roof, bloom_gain, ev.pan, t, ev.shifted);
        }
        if full {
            let mut pickup = tine(
                penta(TINE_BASE_HZ, CAD_PICKUP_DEG + key),
                Touch::Step,
                tau,
                roof,
                false,
                self.v2.flow,
            );
            pickup.lane = LANE_TUNE;
            if closed {
                let mut r = 0;
                while r < 4 {
                    let _ = self.rnd();
                    r += 1;
                }
            } else {
                self.v2_spawn(pickup, ev.gain * KEY_TINE_TRIM * CAD_PICKUP_LEVEL, ev.pan);
            }

            let mut res = tine(
                penta(TINE_BASE_HZ, resolution + key),
                Touch::Step,
                tau,
                roof,
                false,
                self.v2.flow,
            );
            res.delay = t;
            res.lane = LANE_TUNE;
            self.v2_spawn(res, ev.gain * KEY_TINE_TRIM, ev.pan);

            let (bell, _) = cad_bell_voice(t);
            self.v2_spawn(
                bell,
                ev.gain * KEY_TINE_TRIM * CAD_BELL_LEVEL,
                -0.5 * ev.pan,
            );
        }

        let root = penta(BASS_BASE_HZ, key);
        let dyad = bass_dyad(root, root * 1.5, BASS_DECAY_S, t);
        self.v2.bass = self.v2_spawn(dyad, ev.gain * KEY_TINE_TRIM * CAD_DYAD_LEVEL, 0.0);
    }

    #[inline]
    const fn v2_verdict_dyad(
        &mut self,
        ev: &SoundEvent,
        key: i32,
        chord_ix: usize,
        delay: f32,
        level: f32,
    ) {
        let chord = CHORD_LOOP[chord_ix];
        let root = penta(BASS_BASE_HZ * CHORD_ROOT_RATIO[chord.root], key);
        let voice = bass_dyad(root, root * chord.partner, BASS_DECAY_S, delay);
        self.v2.bass = self.v2_spawn(
            voice,
            ev.gain * KEY_TINE_TRIM * VERDICT_PEAK_TRIM * level,
            0.0,
        );
    }

    #[inline]
    const fn v2_verdict(&mut self, ev: &SoundEvent, at: u32, failed: bool, ice: bool) {
        let tau = tau_v_s(self.v2.ioi_ms * 0.001);
        let roof = self.v2_lit_roof(ev);
        let key = self.song_key as i32;

        if failed {
            self.v2_verdict_dyad(ev, key, VERDICT_MINOR, 0.0, VERDICT_MINOR_LEVEL);
        } else {
            self.v2_verdict_dyad(ev, key, VERDICT_PLAGAL[0], 0.0, VERDICT_DYAD_LEVEL);
            self.v2_verdict_dyad(
                ev,
                key,
                VERDICT_PLAGAL[1],
                VERDICT_PLAGAL_S,
                VERDICT_DYAD_LEVEL,
            );
            let mut voice = tine(
                penta(TINE_BASE_HZ, VERDICT_TINE_DEG + key),
                Touch::Step,
                tau,
                roof,
                false,
                self.v2.flow,
            );
            voice.delay = VERDICT_PLAGAL_S;
            voice.lane = LANE_TUNE;
            self.v2_spawn(
                voice,
                ev.gain * KEY_TINE_TRIM * VERDICT_PEAK_TRIM * VERDICT_TINE_LEVEL,
                ev.pan,
            );
            if ice {
                let (bell, _) = cad_bell_voice(VERDICT_PLAGAL_S);
                self.v2_spawn(
                    bell,
                    ev.gain * KEY_TINE_TRIM * VERDICT_PEAK_TRIM * CAD_BELL_LEVEL,
                    -0.5 * ev.pan,
                );
            }
        }
        self.v2.note_verdict(at, failed, !failed && ice);
    }

    #[inline]
    const fn v2_glint(&mut self, ev: &SoundEvent, twinkle_hz: u8) {
        let vel = self.v2_velocity(VEL_DB_RESTRIKE);
        self.v2_glint_at(ev, twinkle_hz, GLINT_LEVEL, vel, 0.0);
    }

    #[inline]
    const fn v2_glint_at(
        &mut self,
        ev: &SoundEvent,
        twinkle_hz: u8,
        level: f32,
        vel: f32,
        delay: f32,
    ) {
        let deg = self.v2.next_glint_deg() + (self.song_key as i32);
        self.v2_glint_deg_at(ev, deg, twinkle_hz, level, vel, delay);
    }

    #[inline]
    const fn v2_tink(&mut self, deg: i32, roof: f32, gain: f32, pan: f32) -> Option<(u8, u32)> {
        let voice = Voice {
            delay: TINK_DELAY_S,
            dur: TINK_DUR_S,
            attack: TINE_ATTACK_S,
            decay: TINK_TAU_S,
            p: p1_sine(penta(TINE_BASE_HZ, deg)),
            lp_cut: roof,
            lane: LANE_GRAFT,
            ..Voice::DEFAULT
        };
        self.v2_spawn_decoration(voice, gain * TINK_LEVEL, pan)
    }

    #[inline]
    const fn v2_glint_deg_at(
        &mut self,
        ev: &SoundEvent,
        deg: i32,
        twinkle_hz: u8,
        level: f32,
        vel: f32,
        delay: f32,
    ) {
        let f = fold_into(penta(TINE_BASE_HZ, deg), GLINT_LO_HZ, GLINT_HI_HZ);
        let rate = if twinkle_hz == 0 {
            GLINT_TWINKLE_DEFAULT_HZ
        } else {
            twinkle_hz as f32
        };
        let voice = Voice {
            delay,
            dur: GLINT_DUR_S,
            attack: GLINT_ATTACK_S,
            decay: GLINT_DECAY_S,
            p: p1_sine(f),
            tw_rate: rate,
            tw_depth: GLINT_TWINKLE_DEPTH,
            lp_cut: shifted_roof(GLINT_LP_HZ, ev.shifted),
            lane: LANE_GLINT,
            ..Voice::DEFAULT
        };
        let pan = self.v2_pan(ev.pan);
        self.v2_damp_same_pitch(LANE_GLINT, f);
        self.v2_spawn(voice, ev.gain * KEY_TINE_TRIM * level * vel, pan);
    }
}

impl TrailSynth {
    #[inline]
    const fn v2_meteor_arm(&mut self, ev: &SoundEvent, meta: EventMeta, dir: i8) {
        let pan = meta.pan_from;
        let mut tick = met_tick();
        tick.arm_ttl = MET_ARM_TTL_S;
        self.v2_spawn(tick, ev.gain * KEY_TINE_TRIM * MET_TICK_LEVEL, pan);
        let t = timing::FLIGHT_MAX_MS * 0.001;
        let mut core = self.v2_core_voice(dir, t);
        core.arm_ttl = MET_ARM_TTL_S;
        core.pan_glide_s = 0.0;
        self.v2_spawn(core, ev.gain * KEY_TINE_TRIM * MET_ARM_CORE_LEVEL, pan);
    }

    #[inline]
    const fn v2_core_voice(&self, dir: i8, t: f32) -> Voice {
        let lo = fold_into(
            penta(
                TINE_BASE_HZ,
                (self.v2.text.walk as i32) + (self.song_key as i32),
            ),
            MET_CORE_LO_HZ,
            MET_CORE_HI_HZ,
        );
        let (f0, f1) = if dir > 0 {
            (lo, lo * 2.0)
        } else {
            (lo * 2.0, lo)
        };
        Voice {
            dur: MET_CORE_DUR_MUL * t,
            attack: MET_CORE_ATTACK_S,
            decay: MET_CORE_DECAY_MUL * t,
            p: [
                Partial {
                    lvl: P1_LVL,
                    f0,
                    f1,
                    glide: MET_CORE_GLIDE_MUL * t,
                    fm_ratio: MET_CORE_FM_RATIO,
                    fm_i0: MET_CORE_FM_INDEX,
                    fm_tau: MET_CORE_FM_TAU_MUL * t,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
                Partial::DEFAULT,
            ],
            lp_cut: MET_CORE_LP_HZ,
            lane: LANE_METEOR,
            pan_glide_s: t,
            ..Voice::DEFAULT
        }
    }

    #[inline]
    const fn v2_release_arm(&mut self) {
        let mut i = 0;
        while i < self.voices.len() {
            let v = &mut self.voices[i];
            if v.on && v.lane == LANE_METEOR && v.arm_ttl > 0.0 && v.damp <= 0.0 {
                v.arm_ttl = 0.0;
                v.damp = LANE_FADE_STEAL_S;
                v.damp0 = LANE_FADE_STEAL_S;
            }
            i += 1;
        }
    }

    #[inline]
    const fn v2_claim_arm(&mut self, ev: &SoundEvent, dir: i8, t: f32) -> Option<u32> {
        let target = self.v2_core_voice(dir, t);
        let vel = self.v2_velocity(VEL_DB_MOTION);
        let claimed_gain = ev.gain * KEY_TINE_TRIM * MET_CORE_LEVEL * vel;
        let dt = self.inv_sr;
        let mut claimed = None;
        let mut i = 0;
        while i < self.voices.len() {
            let v = &mut self.voices[i];
            i += 1;
            if !(v.on && v.lane == LANE_METEOR && v.arm_ttl > 0.0) {
                continue;
            }
            v.arm_ttl = 0.0;
            if v.p[0].lvl <= 0.0 {
                continue;
            }
            let now = v.t.max(0.0);
            let p = &mut v.p[0];
            let cur = if v.env_run {
                p.f1 + (p.f0 - p.f1) * p.g_e
            } else {
                p.f0
            };
            p.f1 = target.p[0].f1;
            p.glide = target.p[0].glide.max(1e-4);
            p.k_g = crate::color_math::exp_f32(-dt / p.glide);
            p.f0 = p.f1 + (cur - p.f1) * crate::color_math::exp_f32(now / p.glide);
            let env_now = if v.env_run { v.env_d } else { 1.0 };
            v.decay = target.decay.max(1e-3);
            v.k_d = crate::color_math::exp_f32(-dt / v.decay);
            let env_new = crate::color_math::exp_f32(-now / v.decay);
            let comp = env_now / env_new.max(1e-6);
            v.gl *= comp;
            v.gr *= comp;
            v.dur = target.dur.max(now + MET_CLAIM_MIN_TAIL_S);
            v.pan1 = ev.pan;
            (v.gl1, v.gr1) = pan_gains(claimed_gain, ev.pan);
            v.pan_glide_s = t;
            v.pan_k = 1.0 / t;
            v.pan_t0 = now;
            v.env_run = false;
            claimed = Some(v.born);
        }
        claimed
    }

    #[inline]
    const fn v2_meteor(
        &mut self,
        ev: &SoundEvent,
        meta: EventMeta,
        dir: i8,
        cells: u16,
        armed: bool,
    ) {
        // The floor (§12.3): a hop under the meteor distance is the nav tick
        // only, and an arm whose move turns out to be a short hop fizzles here.
        if cells < timing::JUMP_MIN_CELLS {
            self.v2_release_arm();
            self.v2_nav_tick(ev);
            return;
        }
        let t = timing::flight_ms(cells as f32) * 0.001;
        let claimed = if armed {
            self.v2_claim_arm(ev, dir, t)
        } else {
            None
        };

        let mut i = 0;
        while i < self.voices.len() {
            let v = &mut self.voices[i];
            i += 1;
            if !v.on || v.damp > 0.0 || !matches!(v.lane, LANE_METEOR | LANE_RAIN) {
                continue;
            }
            if matches!(claimed, Some(b) if b == v.born) {
                continue;
            }
            if v.t < 0.0 {
                v.on = false;
            } else {
                v.damp = METEOR_INTERRUPT_S;
                v.damp0 = METEOR_INTERRUPT_S;
            }
        }
        self.v2_cancel_unsounded(self.v2.bell);
        self.v2_cancel_unsounded(self.v2.thump);
        self.v2.bell = None;
        self.v2.thump = None;

        let g = ev.gain * KEY_TINE_TRIM;
        if claimed.is_none() {
            self.v2_spawn(met_tick(), g * MET_TICK_LEVEL, meta.pan_from);

            let mut core = self.v2_core_voice(dir, t);
            core.pan1 = ev.pan;
            let vel = self.v2_velocity(VEL_DB_MOTION);
            self.v2_spawn(core, g * MET_CORE_LEVEL * vel, meta.pan_from);
        }

        let whoosh = Voice {
            dur: MET_WHOOSH_DUR_MUL * t,
            attack: MET_WHOOSH_ATTACK_S,
            decay: MET_WHOOSH_DECAY_MUL * t,
            n_lvl: 1.0,
            n_f0: MET_WHOOSH_HZ0,
            n_f1: MET_WHOOSH_HZ1,
            n_glide: MET_WHOOSH_GLIDE_MUL * t,
            n_q: MET_WHOOSH_Q,
            lp_cut: MET_WHOOSH_LP_HZ,
            lane: LANE_METEOR,
            pan1: ev.pan,
            pan_glide_s: t,
            ..Voice::DEFAULT
        };
        self.v2_spawn(whoosh, g * MET_WHOOSH_LEVEL, meta.pan_from);

        let deg = (self.v2.text.walk as i32) + (self.song_key as i32);
        let mut bell = tine(
            penta(TINE_BASE_HZ, deg),
            Touch::Step,
            MET_BELL_TAU_S,
            self.v2_lit_roof(ev),
            false,
            self.v2.flow,
        );
        bell.delay = t;
        bell.dur = MET_BELL_DUR_S;
        self.v2.bell = self.v2_spawn(bell, g * MET_BELL_LEVEL, ev.pan);

        let chord = CHORD_LOOP[self.v2.text.chord as usize];
        let root = penta(
            BASS_BASE_HZ * CHORD_ROOT_RATIO[chord.root],
            self.song_key as i32,
        );
        let thump = Voice {
            delay: t,
            dur: MET_THUMP_DUR_S,
            attack: MET_THUMP_ATTACK_S,
            decay: MET_THUMP_DECAY_S,
            p: [
                Partial {
                    lvl: BASS_ROOT_LVL,
                    f0: root,
                    ..Partial::DEFAULT
                },
                Partial {
                    lvl: BASS_FIFTH_LVL,
                    f0: root * chord.partner,
                    ..Partial::DEFAULT
                },
                Partial::DEFAULT,
            ],
            lp_cut: BASS_ROOF_HZ,
            lane: LANE_BASS,
            ..Voice::DEFAULT
        };
        self.v2.thump = self.v2_spawn(thump, g * MET_THUMP_LEVEL, 0.0);

        let step_s = timing::rain_spacing_ms(t * 1000.0) * 0.001;
        let base_delay = t + MET_RAIN_LEAD_S;
        let rain_gain = g * MET_RAIN_LEVEL;
        let mut k = 0;
        while k < timing::FAN_HERO_N {
            let glint = Voice {
                delay: base_delay + k as f32 * step_s,
                dur: MET_RAIN_DUR_S,
                attack: MET_RAIN_ATTACK_S,
                decay: MET_RAIN_DECAY_S,
                p: p1_sine(MET_RAIN_HZ[k]),
                tw_rate: MET_RAIN_TWINKLE_HZ,
                tw_depth: MET_RAIN_TWINKLE_DEPTH,
                lp_cut: MET_RAIN_LP_HZ,
                lane: LANE_RAIN,
                ..Voice::DEFAULT
            };
            self.v2_spawn(glint, rain_gain, ev.pan + MET_RAIN_PAN[k]);
            k += 1;
        }
    }
}

/// The music box roster entry.
pub struct RainbowKittyV2Palette;

impl RainbowKittyV2Palette {
    #[inline]
    pub(super) const fn bed_sample(s: &mut TrailSynth, dt: f32, lvl: f32) -> (f32, f32) {
        s.bed_rainbow_sky(dt, lvl)
    }

    #[inline]
    pub(super) const fn anchor_hz() -> f32 {
        TINE_BASE_HZ
    }
}

impl TrailSynth {
    /// One stereo sample `(mid, side)` of the sky pad, behind the music box's palette bed.
    #[inline]
    pub(super) const fn bed_rainbow_sky(&mut self, dt: f32, lvl: f32) -> (f32, f32) {
        const _: () = assert!(
            BED_PARTIALS[0] == 1
                && BED_PARTIALS[1] == 2
                && BED_PARTIALS[2] == 3
                && BED_PARTIALS[3] == 4
        );
        #[inline]
        const fn tone(ph: f32) -> f32 {
            const INV_3: f32 = 1.0 / 3.0;
            let x = super::sin01(ph)
                + super::sin01(ph * 2.0) * 0.5
                + super::sin01(ph * 3.0) * INV_3
                + super::sin01(ph * 4.0) * 0.25;
            x * BED_PARTIAL_NORM
        }
        let ci = (self.v2.text.chord as usize) & (SKY_BED_CHORDS - 1);
        let tgt = if self.song_key == 0 {
            SKY_BED_TARGETS[ci]
        } else {
            let degs = SKY_BED_DEGREES_TABLE[ci];
            let key = self.song_key as i32;
            [
                penta(BED_BASE_HZ, degs[0] + key),
                penta(BED_BASE_HZ, degs[1] + key),
                penta(BED_BASE_HZ, degs[2] + key),
            ]
        };
        let b = &mut self.bed;
        if b.sky_glide_dt != dt {
            const INV_BED_GLIDE_TAU_S: f32 = -1.0 / BED_GLIDE_TAU_S;
            b.sky_glide_dt = dt;
            b.sky_glide = 1.0 - crate::color_math::exp_f32(dt * INV_BED_GLIDE_TAU_S);
        }
        let glide = b.sky_glide;
        let arc = b.hue_s.clamp(0.0, 1.0);
        if b.sky_detune <= 0.0 || b.sky_detune_arc != arc {
            const LN2_OVER_1200: f32 = core::f32::consts::LN_2 / 1200.0;
            b.sky_detune_arc = arc;
            b.sky_detune = crate::color_math::exp_f32(
                lerp(BED_DETUNE_CENTS_LO, BED_DETUNE_CENTS_HI, arc) * LN2_OVER_1200,
            );
        }
        let detune = b.sky_detune;
        let mut m = 0.0;
        let mut i = 0;
        while i < 3 {
            if b.var_f[i] <= 0.0 {
                b.var_f[i] = tgt[i];
            }
            b.var_f[i] += (tgt[i] - b.var_f[i]) * glide;
            b.var_ph[i] = (b.var_ph[i] + b.var_f[i] * dt).fract();
            let mut x = tone(b.var_ph[i]);
            if i == 2 {
                b.var_ph[3] = (b.var_ph[3] + b.var_f[2] * detune * dt).fract();
                x = 0.5 * (x + tone(b.var_ph[3]));
            }
            m += x * BED_WEIGHT[i];
            i += 1;
        }
        let cut = lerp(BED_TILT_LO_HZ, BED_TILT_HI_HZ, arc);
        let k = (cut * dt * core::f32::consts::TAU).clamp(0.0, 1.0);
        b.lp1 += k * (m - b.lp1);
        b.ph3 = (b.ph3 + BED_BREATH_HZ * dt).fract();
        let breath = 1.0 - BED_BREATH_DEPTH * (0.5 - 0.5 * super::sin01(b.ph3 + 0.25));
        (b.lp1 * breath * lvl * BED_LEVEL, 0.0)
    }
}

#[cfg(test)]
#[path = "rainbow_kitty_v2_tests.rs"]
mod tests;
