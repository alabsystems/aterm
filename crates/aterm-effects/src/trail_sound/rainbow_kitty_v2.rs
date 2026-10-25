// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The music box: Rainbow Kitty v2's instrument and its melody engine.

use super::glyph_class::{
    BANG, CLOSE, COLON, COMMA, DASH, DIGIT, LETTER, LINE, MATH, OPEN, QMARK, QUOTE, RISE, SECRET,
    SEMI, SIGIL, STOP,
};
use super::{
    ERASE_MIN_GAP, EventMeta, HELD_ERASE_RUN_WINDOW, OutputGesture, PAN_LAW_SCALE, Palette,
    Partial, SONG_FORM, SONG_THEME, SoundEvent, SoundGesture, SoundKind, TrailSynth, Voice,
    pan_gains, penta,
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

/// C3: two octaves under the tine and an octave under the bass dyad ([`BASS_BASE_HZ`]).
pub(super) const BED_BASE_HZ: f32 = TINE_BASE_HZ * 0.25;
/// The number of chords the sky voices ([`CHORD_LOOP`]'s length), exported for the parent's lattice law.
pub(super) const SKY_BED_CHORDS: usize = CHORD_LOOP.len();
const BED_WEIGHT: [f32; 3] = [1.0, 0.80, 0.65];
const BED_GLIDE_TAU_S: f32 = 2.6;
const BED_LEVEL: f32 = 0.008_9;
const BED_BREATH_HZ: f32 = 1.0 / 12.0;
const BED_BREATH_DEPTH: f32 = 0.35;
const BED_HUE_TAU_S: f32 = 1.2;
const BED_TILT_LO_HZ: f32 = 350.0;
const BED_TILT_HI_HZ: f32 = 900.0;
const BED_DETUNE_CENTS_LO: f32 = 4.0;
const BED_DETUNE_CENTS_HI: f32 = 11.0;
const BED_PARTIALS: [u32; 4] = [1, 2, 3, 4];
const BED_PARTIAL_NORM: f32 = 0.838_1;

#[inline]
const fn compute_sky_bed_degrees(chord: usize) -> [i32; 3] {
    let lit = CHORD_LOOP[chord % SKY_BED_CHORDS].lit;
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
    SKY_BED_DEGREES_TABLE[chord % SKY_BED_CHORDS]
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
    #[inline]
    const fn hz(lit: [i32; 3], k: usize, key: i32) -> f32 {
        let m = lit.len();
        penta(BASS_BASE_HZ, 5 * (k / m) as i32 + lit[k % m] + key)
    }
    let mut k0 = 0usize;
    while k0 < 64 {
        if hz(lit, k0, key) > root * 1.01 {
            break;
        }
        k0 += 1;
    }
    let mut cycle = 0usize;
    while cycle < 64 && hz(lit, k0 + cycle, key) < SPACE_STEP_HI_HZ {
        cycle += 1;
    }
    let cycle = if cycle > 1 { cycle } else { 1 };
    let k = k0 + (n.saturating_sub(1) as usize) % cycle;
    let mut f = hz(lit, k, key);
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
    const fn deg_is_lit(&self, deg: i32) -> bool {
        let pc = deg.rem_euclid(5) as u8;
        CHORD_LOOP[self.text.chord as usize].lit & (1 << pc) != 0
    }

    #[inline]
    const fn lit(&self) -> bool {
        self.deg_is_lit(self.text.walk as i32)
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
        if self.deg_is_lit(deg) {
            return deg;
        }
        let dir = if deg >= from { 1 } else { -1 };
        let mut k = 1;
        while k <= WORD_HEAD_SNAP_DEG {
            let c0 = deg + dir * k;
            if c0 >= TUNE_DEG_LO && c0 <= TUNE_DEG_HI && self.deg_is_lit(c0) {
                return c0;
            }
            let c1 = deg - dir * k;
            if c1 >= TUNE_DEG_LO && c1 <= TUNE_DEG_HI && self.deg_is_lit(c1) {
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

    fn on_typed(&mut self, at: u32, rank: u8, sing: bool, shifted: bool, class: u8) -> TypedPlan {
        let gap = at.saturating_sub(self.last_key_ms.unwrap_or(0));
        let first = self.last_key_ms.is_none();
        self.push_undo();
        let said = self.text.last_mark;
        let rank_before = self.text.prev_rank;

        let held = self.detect_autorepeat(gap, rank, first);

        let rest = !first && timing::phrase_rest_reached(gap, self.ioi_ms);
        if rest {
            let here = i32::from(self.text.walk);
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
            if usize::from(self.text.motif_len) == MOTIF_LEN
                && self.text.words_since_motif >= MOTIF_EVERY_WORDS
            {
                self.text.motif_play = MOTIF_LEN as u8;
                self.text.motif_k = 0;
                self.text.words_since_motif = 0;
                answer_head = true;
            }
        }

        let from = i32::from(self.text.walk);
        let mut deg = self.derive(rank, gap, first, word_head);
        if word_head {
            deg = self.nearest_lit_within(deg, from);
            let sounded = deg - from;
            if sounded != 0
                && self.text.run_len >= MELODY_RUN_MAX
                && i32::from(self.text.run_stride) == sounded
            {
                let away = -sounded.signum();
                if let Some(alt) = (1..=WORD_HEAD_SNAP_DEG + 1)
                    .map(|k| from + away * k)
                    .find(|c| (TUNE_DEG_LO..=TUNE_DEG_HI).contains(c) && self.deg_is_lit(*c))
                {
                    deg = alt;
                }
            }
            if matches!(said, MARK_PERIOD | MARK_BANG)
                && deg == from
                && let Some(alt) = (1..=STEER_MAX_DEG)
                    .flat_map(|k| [from + k, from - k])
                    .find(|c| (TUNE_DEG_LO..=TUNE_DEG_HI).contains(c) && self.deg_is_lit(*c))
            {
                deg = alt;
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
            let lifted = if word_head {
                lifted
            } else {
                lifted.min(from + WORD_LEAP_MAX_DEG)
            };
            if lifted != from {
                deg = lifted;
            }
        }
        let mark = phrase_kind(class);
        let doubled = mark != MARK_NONE && said & !MARK_DOUBLED == mark;
        let decimal =
            class == STOP && (DIGIT_RANK0..DIGIT_RANK0 + 10).contains(&i32::from(rank_before));
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
                let to = i32::from(self.text.paren_deg);
                deg = from + (to - from).clamp(-WORD_LEAP_MAX_DEG, WORD_LEAP_MAX_DEG);
            }
            self.text.paren_deg = -1;
        } else if class == QUOTE && self.text.quote_deg >= 0 {
            let to = i32::from(self.text.quote_deg);
            deg = from + (to - from).clamp(-WORD_LEAP_MAX_DEG, WORD_LEAP_MAX_DEG);
            self.text.quote_deg = -1;
            quote = QUOTE_CLOSES;
        } else if class == QUOTE && quote_head {
            let top = if word_head {
                TUNE_DEG_HI
            } else {
                (from + WORD_LEAP_MAX_DEG).min(TUNE_DEG_HI)
            };
            if let Some(up) = (deg..=top).find(|c| self.deg_is_lit(*c)) {
                deg = up;
            }
            self.text.quote_deg = deg as i8;
            quote = QUOTE_OPENS;
        } else if mark != MARK_NONE && !self.text.token_marked && !decimal {
            deg = cadence_target(mark, from, i32::from(self.text.last_cadence_deg));
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
            self.text.aside_depth = (self.text.aside_depth + 1).min(ASIDE_DEPTH_MAX);
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
        if !first && usize::from(self.text.motif_len) < MOTIF_LEN && self.text.motif_play == 0 {
            let iv = (deg - from).clamp(-WORD_LEAP_MAX_DEG, WORD_LEAP_MAX_DEG) as i8;
            let ramp = usize::from(self.text.motif_len) == MOTIF_LEN - 1
                && self.text.motif[..usize::from(self.text.motif_len)]
                    .iter()
                    .all(|&m| m == iv);
            if iv != 0 && !ramp {
                self.text.motif[usize::from(self.text.motif_len)] = iv;
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

        let lit = self.lit() || self.take_verdict_lit(at);
        let step_level = if lit || shifted || steered {
            1.0
        } else {
            PASSING_LEVEL
        };
        let level = if touch == Touch::Step {
            step_level
        } else {
            step_level
                * (RESTRIKE_L0
                    * RESTRIKE_FALL.powi(i32::from(self.text.restrike).saturating_sub(1)))
                .max(RESTRIKE_FLOOR)
        };
        let mallet_only = held || (sing && touch == Touch::ReStrike);
        if rank != 0 {
            self.text.prev_rank = rank;
        }
        self.last_key_ms = Some(at);
        self.space_run = false;
        self.text.word_pos = self.text.word_pos.saturating_add(1);
        self.keys_since_enter = self.keys_since_enter.saturating_add(1);
        TypedPlan {
            touch,
            deg: i32::from(self.text.walk),
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

    const fn on_space(&mut self, at: u32) -> bool {
        let head = !self.space_run;
        if head {
            self.push_undo();
            let pending = self.pending_mark();
            let n = CHORD_LOOP.len() as u8;
            self.text.chord = match mark_chords(pending) {
                Some(want) => {
                    let mut chosen = (self.text.chord + 1) % n;
                    let mut k = 1u8;
                    while k <= n {
                        let c = (self.text.chord + k) % n;
                        if c == want[0] || c == want[1] {
                            chosen = c;
                            break;
                        }
                        k += 1;
                    }
                    chosen
                }
                None => (self.text.chord + 1) % n,
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
        self.run_phrase = (self.run_phrase + 1) % (SONG_FORM.len() as u8 - 1);
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
        let mut k = 1;
        while k <= 5 {
            let d = deg + k;
            if self.deg_is_lit(d) {
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
    fn v2_velocity(&mut self, db: f32) -> f32 {
        let u = self.rnd_in(-1.0, 1.0);
        (10.0f32).powf(u * db * 0.05)
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
    fn v2_live_ting_at(&self, f: f32) -> bool {
        let Some((slot, born)) = self.v2.lift else {
            return false;
        };
        let v = &self.voices[usize::from(slot)];
        v.on && v.born == born
            && v.lane == LANE_TING
            && v.damp <= 0.0
            && (1200.0 * (v.p[0].f0 / f).log2()).abs() < SAME_PITCH_CENTS
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
    fn v2_spawn(&mut self, proto: Voice, gain: f32, pan: f32) -> Option<(u8, u32)> {
        let idx = self.spawn(proto, gain, pan)?;
        Some((idx as u8, self.voices[idx].born))
    }

    #[inline]
    fn v2_spawn_ph0(
        &mut self,
        proto: Voice,
        gain: f32,
        pan: f32,
        ph0: Option<f32>,
    ) -> Option<(u8, u32)> {
        let tw_ph = self.rnd();
        let drawn = [self.rnd(), self.rnd(), self.rnd()];
        let ph = [ph0.unwrap_or(drawn[0]), drawn[1], drawn[2]];
        let idx = self.spawn_seeded(proto, gain, pan, tw_ph, ph)?;
        Some((idx as u8, self.voices[idx].born))
    }

    #[inline]
    fn v2_flow_echo_phase(&self, fundamental: f32, delay_s: f32) -> Option<f32> {
        let (slot, born) = self.v2.lead?;
        let lead = &self.voices[usize::from(slot)];
        let p2 = lead.p[1];
        (lead.on
            && lead.born == born
            && lead.lane == LANE_TUNE
            && p2.lvl > 0.0
            && p2.glide <= 0.0
            && p2.f0 == fundamental)
            .then(|| (p2.ph + p2.f0 * delay_s + FLOW_ECHO_PHASE).fract())
    }

    #[inline]
    fn v2_lock_hammer(&mut self, who: Option<(u8, u32)>, turn: f32) {
        let Some((slot, born)) = who else { return };
        let v = &mut self.voices[usize::from(slot)];
        if v.on && v.born == born && v.p[0].fm_ratio > 0.0 {
            v.p[0].fm_ph = (v.p[0].fm_ratio * v.p[0].ph + turn).fract();
        }
    }

    #[inline]
    fn v2_spawn_decoration(&mut self, proto: Voice, gain: f32, pan: f32) -> Option<(u8, u32)> {
        if self.voices.iter().all(|v| v.on) {
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
    fn mute_all_lanes_but(&mut self, lane: u8) {
        for v in &mut self.voices {
            if v.lane != lane {
                v.gl = 0.0;
                v.gr = 0.0;
            }
        }
    }

    /// The v2 admission path (§16 row 9), reached only from [`TrailSynth::push_meta`]'s top-of-function branch, never by a v1 event.
    pub(super) fn push_v2(&mut self, ev: SoundEvent, meta: EventMeta) {
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
                let k = 1.0 - (self.since_event * INV_BED_HUE_TAU_S).exp();
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
        self.rate = self.rate * (self.since_event * INV_RATE_TAU).exp() + 1.0;
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
                for v in &mut self.voices {
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
    fn v2_typed(&mut self, ev: &SoundEvent, meta: EventMeta, at: u32) {
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
        let tau_mul = if plan.touch == Touch::Step {
            1.0
        } else {
            RESTRIKE_TAU_MUL
        };
        let tau = tau_v_s(ioi_s) * tau_mul;
        let lit_roof = ev.shifted
            || (classed && matches!(class, OPEN | BANG))
            || cadence_stop
            || (plan.lit && (plan.touch == Touch::Step || plan.word_head));
        let arc = if stops.hue { hue_arc(ev.hue) } else { 0.0 };
        let roof = roof_hz(cps, lit_roof, ev.heat, arc, plan.touch);
        let roof = if plan.sotto {
            roof * ASIDE_ROOF_MUL
        } else {
            roof
        };
        let aside_gain = if plan.sotto { ASIDE_GAIN } else { 1.0 };
        let deg = plan.deg + i32::from(self.song_key);
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
        let vel_db = if plan.touch == Touch::Step {
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
        let blooms =
            stops.bloom && plan.lit && plan.touch == Touch::Step && !plan.mallet_only && !plucked;
        let clouds = stops.room && plan.rest && !plan.mallet_only;
        let bloom_gain = if blooms || clouds {
            let (air, spread) = if stops.hue {
                (hue_air(ev.hue), BLOOM_SPREAD * (arc - 0.5))
            } else {
                (hue_air(0.25), 0.0)
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

        if stops.bloom && plan.touch == Touch::Step && !plan.mallet_only {
            if classed && class == DIGIT {
                let d = (i32::from(meta.rank) - DIGIT_RANK0).clamp(0, 9);
                let deg = COUNT_GLINT_DEG0 + d % 5 + i32::from(self.song_key);
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
        if stops.bloom && (plan.touch == Touch::Step || closes_its_open) && classed {
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
                    let t_deg = reflect_deg(plan.deg + step) + i32::from(self.song_key);
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

        let ring_gain = if ev.shifted {
            gain * CAP_RING_LEVEL.max(FLOW_ECHO_LEVEL * flow)
        } else {
            gain * FLOW_ECHO_LEVEL * flow
        };
        let struck = plan.touch == Touch::Step && !plan.mallet_only;
        let ring_deg = deg + FLOW_ECHO_OCTAVE_DEG + CAP_RING_LIFT_DEG;
        let rings = hammer.ring && !self.v2_live_ting_at(penta(TINE_BASE_HZ, ring_deg));
        if ev.shifted && struck && !rings && !self.voices.iter().all(|v| v.on) {
            for _ in 0..4 {
                let _ = self.rnd();
            }
        }
        let echoes = flow > 0.0 && plan.lit && !plucked && !ev.shifted;
        if (echoes || rings) && struck {
            let echo_deg = if rings {
                ring_deg
            } else {
                deg + FLOW_ECHO_OCTAVE_DEG
            };
            let mut echo = tine(
                penta(TINE_BASE_HZ, echo_deg),
                Touch::Step,
                tau,
                roof,
                false,
                flow,
            );
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
                let echo_gain = ring_gain;
                let ph0 = self.v2_flow_echo_phase(echo.p[0].f0, FLOW_ECHO_DELAY_S);
                self.v2_spawn_ph0(echo, echo_gain, pan, ph0);
            }
        }

        if stops.room {
            // The answering voice (§3.3): the head opening the line's reply to
            // its subject is answered from above, a beat later, on the nearest
            // lit tone, in the droppable lane.
            if plan.answer_head && !plan.mallet_only {
                let a_deg = self.v2.answer_deg_above(plan.deg) + i32::from(self.song_key);
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

    fn v2_air_cloud(
        &mut self,
        deg: i32,
        roof: f32,
        bloom_gain: f32,
        pan: f32,
        after: f32,
        shifted: bool,
    ) {
        for k in 0..AIR_TAP_DELAY_S.len() {
            let mut tap = bloom(deg, roof, TAU_V_MAX_S);
            tap.lp_cut = shifted_roof(tap.lp_cut, shifted);
            tap.delay = after + AIR_TAP_DELAY_S[k];
            let side = if k % 2 == 0 {
                -AIR_TAP_PAN
            } else {
                AIR_TAP_PAN
            };
            self.v2_spawn(tap, bloom_gain * AIR_TAP_LEVEL[k], pan + side);
        }
    }

    fn v2_space(&mut self, ev: &SoundEvent, at: u32) {
        self.v2.ring = None;
        self.v2.graft = None;
        self.v2.prev_class = LETTER;
        let head = self.v2.on_space(at);
        let g = g_ioi(self.v2.ioi_ms * 0.001);
        let ci = usize::from(self.v2.text.chord);
        let chord = CHORD_LOOP[ci];
        let root = penta(
            BASS_BASE_HZ * CHORD_ROOT_RATIO[chord.root],
            self.song_key.into(),
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
            let f = indent_step_hz(ci, root, self.v2.space_steps, self.song_key.into());
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

    fn v2_shift(&mut self, ev: &SoundEvent) {
        const _: () = assert!(TING_PICK.len() == super::SHIFT_ROTATION.len());
        self.v2_damp(self.v2.lift, LANE_FADE_STEAL_S);
        let k = usize::from(self.shift_step);
        self.shift_step = (self.shift_step + 1) % super::SHIFT_ROTATION.len() as u8;
        let class = TING_TONES[CHORD_LOOP[usize::from(self.v2.text.chord)].root][TING_PICK[k]];
        let f = ting_fold(penta(TINE_BASE_HZ, class + i32::from(self.song_key)));
        let pan = self.v2_pan(ev.pan);
        self.v2.lift = self.v2_spawn(ting(f), ev.gain * KEY_TINE_TRIM * TING_LEVEL, pan);
    }

    fn v2_secret(&mut self, ev: &SoundEvent) {
        let f = penta(TINE_BASE_HZ, SECRET_DEG + i32::from(self.song_key));
        self.v2_damp_same_pitch(LANE_TUNE, f);
        let voice = tine(f, Touch::Step, SECRET_TAU_S, ROOF_PLAIN_LO_HZ, false, 0.0);
        self.v2_spawn(voice, ev.gain * KEY_TINE_TRIM, 0.0);
    }

    fn v2_nav_tick(&mut self, ev: &SoundEvent) {
        let deg = i32::from(self.v2.text.walk) + i32::from(self.song_key);
        let f = penta(TINE_BASE_HZ, deg);
        self.v2_damp_same_pitch(LANE_TUNE, f);
        let voice = Voice {
            dur: NAV_DUR_S,
            attack: NAV_ATTACK_S,
            decay: NAV_DECAY_S,
            p: p1_sine(f),
            lp_cut: ROOF_PLAIN_LO_HZ,
            lane: LANE_TUNE,
            ..Voice::default()
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
        let cps = 1.0 / (self.v2.ioi_ms * 0.001);
        roof_hz(cps, true, ev.heat, arc, Touch::Step)
    }

    fn v2_cascade(&mut self, ev: &SoundEvent, at: u32) {
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
        let base = i32::from(self.v2.text.walk) + i32::from(self.song_key);
        if head {
            let roof = roof_hz(cps, true, ev.heat, arc, Touch::Step);
            for k in 0..CASCADE_DEGREES.len() {
                let f = penta(TINE_BASE_HZ, base + CASCADE_DEGREES[k]);
                let mut voice = tine(f, Touch::Step, tau, roof, false, self.v2.flow);
                voice.delay = CASCADE_DELAYS_S[k];
                voice.lane = LANE_CASCADE;
                let pan = if k % 2 == 0 { ev.pan } else { -ev.pan };
                let gain = ev.gain * KEY_TINE_TRIM * CASCADE_LEVELS[k];
                self.v2_spawn(voice, gain, pan);
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

    fn v2_strum(&mut self, ev: &SoundEvent) {
        self.v2.ring = None;
        self.v2.graft = None;
        self.v2.prev_class = LETTER;
        let lit = sky_bed_degrees(usize::from(self.v2.text.chord));
        let degrees = [lit[0], lit[1], lit[2], lit[0] + 5];
        let tau = tau_v_s(self.v2.ioi_ms * 0.001);
        let roof = self.v2_lit_roof(ev);
        let base = i32::from(self.song_key);
        for k in 0..degrees.len() {
            let f = penta(TINE_BASE_HZ, base + degrees[k]);
            let mut voice = tine(f, Touch::Step, tau, roof, false, self.v2.flow);
            voice.delay = STRUM_DELAYS_S[k];
            voice.lane = LANE_CASCADE;
            let gain = ev.gain * KEY_TINE_TRIM * STRUM_TRIM * STRUM_SHAPE[k];
            self.v2_spawn(voice, gain, ev.pan);
        }
    }

    fn v2_enter(&mut self, ev: &SoundEvent, at: u32, cells: u16) {
        self.v2.ring = None;
        self.v2.graft = None;
        self.v2.prev_class = LETTER;
        self.v2_hand_back_key(at);
        let (full, closed, asked) = self.v2.on_enter(at);
        let walk = self.v2.text.walk;
        let t = timing::flight_ms(f32::from(cells)) * 0.001;
        let ioi_s = self.v2.ioi_ms * 0.001;
        let tau = tau_v_s(ioi_s);
        let stops = self.v2.stops;
        let roof = self.v2_lit_roof(ev);
        let key = i32::from(self.song_key);
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
        let home = if full { resolution } else { i32::from(walk) };
        if stops.room {
            let air = if stops.hue {
                hue_air(ev.hue)
            } else {
                hue_air(0.25)
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
                for _ in 0..4 {
                    let _ = self.rnd();
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

    fn v2_verdict(&mut self, ev: &SoundEvent, at: u32, failed: bool, ice: bool) {
        let tau = tau_v_s(self.v2.ioi_ms * 0.001);
        let roof = self.v2_lit_roof(ev);
        let key = i32::from(self.song_key);

        let dyad = |me: &mut Self, chord_ix: usize, delay: f32, level: f32| {
            let chord = CHORD_LOOP[chord_ix];
            let root = penta(BASS_BASE_HZ * CHORD_ROOT_RATIO[chord.root], key);
            let voice = bass_dyad(root, root * chord.partner, BASS_DECAY_S, delay);
            me.v2.bass = me.v2_spawn(
                voice,
                ev.gain * KEY_TINE_TRIM * VERDICT_PEAK_TRIM * level,
                0.0,
            );
        };

        if failed {
            dyad(self, VERDICT_MINOR, 0.0, VERDICT_MINOR_LEVEL);
        } else {
            dyad(self, VERDICT_PLAGAL[0], 0.0, VERDICT_DYAD_LEVEL);
            dyad(
                self,
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
    fn v2_glint(&mut self, ev: &SoundEvent, twinkle_hz: u8) {
        let vel = self.v2_velocity(VEL_DB_RESTRIKE);
        self.v2_glint_at(ev, twinkle_hz, GLINT_LEVEL, vel, 0.0);
    }

    #[inline]
    fn v2_glint_at(&mut self, ev: &SoundEvent, twinkle_hz: u8, level: f32, vel: f32, delay: f32) {
        let deg = self.v2.next_glint_deg() + i32::from(self.song_key);
        self.v2_glint_deg_at(ev, deg, twinkle_hz, level, vel, delay);
    }

    #[inline]
    fn v2_tink(&mut self, deg: i32, roof: f32, gain: f32, pan: f32) -> Option<(u8, u32)> {
        let voice = Voice {
            delay: TINK_DELAY_S,
            dur: TINK_DUR_S,
            attack: TINE_ATTACK_S,
            decay: TINK_TAU_S,
            p: p1_sine(penta(TINE_BASE_HZ, deg)),
            lp_cut: roof,
            lane: LANE_GRAFT,
            ..Voice::default()
        };
        self.v2_spawn_decoration(voice, gain * TINK_LEVEL, pan)
    }

    fn v2_glint_deg_at(
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
            f32::from(twinkle_hz)
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
            ..Voice::default()
        };
        let pan = self.v2_pan(ev.pan);
        self.v2_damp_same_pitch(LANE_GLINT, f);
        self.v2_spawn(voice, ev.gain * KEY_TINE_TRIM * level * vel, pan);
    }
}

impl TrailSynth {
    fn v2_meteor_arm(&mut self, ev: &SoundEvent, meta: EventMeta, dir: i8) {
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

    fn v2_claim_arm(&mut self, ev: &SoundEvent, dir: i8, t: f32) -> Option<u32> {
        let target = self.v2_core_voice(dir, t);
        let vel = self.v2_velocity(VEL_DB_MOTION);
        let claimed_gain = ev.gain * KEY_TINE_TRIM * MET_CORE_LEVEL * vel;
        let dt = self.inv_sr;
        let mut claimed = None;
        for v in &mut self.voices {
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
            p.k_g = (-dt / p.glide).exp();
            p.f0 = p.f1 + (cur - p.f1) * (now / p.glide).exp();
            let env_now = if v.env_run { v.env_d } else { 1.0 };
            v.decay = target.decay.max(1e-3);
            v.k_d = (-dt / v.decay).exp();
            let env_new = (-now / v.decay).exp();
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

    fn v2_meteor(&mut self, ev: &SoundEvent, meta: EventMeta, dir: i8, cells: u16, armed: bool) {
        // The floor (§12.3): a hop under the meteor distance is the nav tick
        // only, and an arm whose move turns out to be a short hop fizzles here.
        if cells < timing::JUMP_MIN_CELLS {
            self.v2_release_arm();
            self.v2_nav_tick(ev);
            return;
        }
        let t = timing::flight_ms(f32::from(cells)) * 0.001;
        let claimed = if armed {
            self.v2_claim_arm(ev, dir, t)
        } else {
            None
        };

        for v in &mut self.voices {
            if !v.on || v.damp > 0.0 || !matches!(v.lane, LANE_METEOR | LANE_RAIN) {
                continue;
            }
            if claimed == Some(v.born) {
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
            ..Voice::default()
        };
        self.v2_spawn(whoosh, g * MET_WHOOSH_LEVEL, meta.pan_from);

        let deg = i32::from(self.v2.text.walk) + i32::from(self.song_key);
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

        let chord = CHORD_LOOP[usize::from(self.v2.text.chord)];
        let root = penta(
            BASS_BASE_HZ * CHORD_ROOT_RATIO[chord.root],
            i32::from(self.song_key),
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
                    ..Partial::default()
                },
                Partial {
                    lvl: BASS_FIFTH_LVL,
                    f0: root * chord.partner,
                    ..Partial::default()
                },
                Partial::default(),
            ],
            lp_cut: BASS_ROOF_HZ,
            lane: LANE_BASS,
            ..Voice::default()
        };
        self.v2.thump = self.v2_spawn(thump, g * MET_THUMP_LEVEL, 0.0);

        let step_s = timing::rain_spacing_ms(t * 1000.0) * 0.001;
        for k in 0..timing::FAN_HERO_N {
            let glint = Voice {
                delay: t + MET_RAIN_LEAD_S + k as f32 * step_s,
                dur: MET_RAIN_DUR_S,
                attack: MET_RAIN_ATTACK_S,
                decay: MET_RAIN_DECAY_S,
                p: p1_sine(MET_RAIN_HZ[k]),
                tw_rate: MET_RAIN_TWINKLE_HZ,
                tw_depth: MET_RAIN_TWINKLE_DEPTH,
                lp_cut: MET_RAIN_LP_HZ,
                lane: LANE_RAIN,
                ..Voice::default()
            };
            self.v2_spawn(glint, g * MET_RAIN_LEVEL, ev.pan + MET_RAIN_PAN[k]);
        }
    }
}

/// The music box as a [`Palette`] roster entry.
pub struct RainbowKittyV2Palette;

impl Palette for RainbowKittyV2Palette {
    fn design(
        &self,
        _s: &mut TrailSynth,
        _ev: &SoundEvent,
        _kind: SoundKind,
        _g: f32,
        _deg: i32,
        _col_off: i32,
    ) {
    }

    fn bed_sample(&self, s: &mut TrailSynth, dt: f32, lvl: f32) -> (f32, f32) {
        s.bed_rainbow_sky(dt, lvl)
    }

    fn anchor_hz(&self) -> f32 {
        TINE_BASE_HZ
    }
}

impl TrailSynth {
    /// One stereo sample `(mid, side)` of the sky pad, behind the music box's palette bed.
    pub(super) fn bed_rainbow_sky(&mut self, dt: f32, lvl: f32) -> (f32, f32) {
        const _: () = assert!(
            BED_PARTIALS[0] == 1
                && BED_PARTIALS[1] == 2
                && BED_PARTIALS[2] == 3
                && BED_PARTIALS[3] == 4
        );
        let tgt = SKY_BED_TARGETS[usize::from(self.v2.text.chord) % SKY_BED_CHORDS];
        let b = &mut self.bed;
        if b.sky_glide_dt != dt {
            const INV_BED_GLIDE_TAU_S: f32 = -1.0 / BED_GLIDE_TAU_S;
            b.sky_glide_dt = dt;
            b.sky_glide = 1.0 - (dt * INV_BED_GLIDE_TAU_S).exp();
        }
        let glide = b.sky_glide;
        let arc = b.hue_s.clamp(0.0, 1.0);
        if b.sky_detune <= 0.0 || b.sky_detune_arc != arc {
            const INV_1200: f32 = 1.0 / 1200.0;
            b.sky_detune_arc = arc;
            b.sky_detune =
                (2.0f32).powf(lerp(BED_DETUNE_CENTS_LO, BED_DETUNE_CENTS_HI, arc) * INV_1200);
        }
        let detune = b.sky_detune;
        let tone = |ph: f32| -> f32 {
            const INV_3: f32 = 1.0 / 3.0;
            let x = super::sin01(ph)
                + super::sin01(ph * 2.0) * 0.5
                + super::sin01(ph * 3.0) * INV_3
                + super::sin01(ph * 4.0) * 0.25;
            x * BED_PARTIAL_NORM
        };
        let mut m = 0.0;
        for i in 0..3 {
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
mod tests {
    use super::*;
    use crate::cursor_glow::GlowStyle;
    use crate::tone::Tone;
    use crate::trail_sound::{
        CelebrationGesture, LIMIT_ATTACK_S, LIMIT_RELEASE_S, LIMIT_THRESHOLD, SoundVoice,
        TrailSynth, WordGesture, limit,
    };

    const SR: f32 = 48_000.0;
    const SEED: u32 = 0xA5A5_1234;
    const VOL: f32 = 0.4;

    fn synth() -> TrailSynth {
        TrailSynth::new(SR, SEED)
    }

    fn event(kind: SoundKind, pan: f32, shifted: bool) -> SoundEvent {
        SoundEvent {
            style: GlowStyle::RainbowKitty,
            voice: SoundVoice::Style,
            kind: SoundGesture::Trail(kind),
            pan,
            heat: 0.5,
            hue: 0.0,
            gain: VOL,
            tone: Tone::Technical,
            bed: false,
            shifted,
        }
    }

    fn push_ch(s: &mut TrailSynth, kind: SoundKind, at: u32, ch: char) {
        s.push_meta(
            event(kind, 0.0, ch.is_uppercase()),
            EventMeta {
                at_ms: at,
                glyph_class: crate::trail_sound::typed_glyph_class(Some(ch)),
                rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                ..EventMeta::default()
            },
        );
    }

    fn type_line(s: &mut TrailSynth, text: &str, t0: u32, period: u32) -> u32 {
        let mut at = t0;
        for ch in text.chars() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            push_ch(s, kind, at, ch);
            at += period;
        }
        at
    }

    #[test]
    fn the_sky_is_fed_by_the_music_boxs_keys_only_behind_the_knob() {
        let mut off = synth();
        let mut on = synth();
        let mut buf = [0.0f32; 512];
        for _ in 0..12 {
            off.push(event(SoundKind::Typed, 0.0, false));
            on.push(SoundEvent {
                bed: true,
                ..event(SoundKind::Typed, 0.0, false)
            });
            for _ in 0..8 {
                off.render(&mut buf);
                on.render(&mut buf);
            }
        }
        assert_eq!(off.bed.energy, 0.0, "the knob off feeds nothing");
        assert_eq!(off.bed.level, 0.0);
        assert!(
            on.bed.energy > 0.0 && on.bed.level > 0.1,
            "the knob on: the pad is up"
        );
        let mut sounding = synth();
        sounding.push(SoundEvent {
            bed: true,
            ..event(SoundKind::Typed, 0.0, false)
        });
        for _ in 0..30 {
            sounding.render(&mut buf);
        }
        let (l, r) = sounding.bed_sample(1.0 / SR);
        assert!(
            l != 0.0 && (l - r).abs() < 1e-9,
            "the pad is a mono floor under the notes"
        );
        let renders_per_s = 48_000 / (buf.len() / crate::trail_sound::CHANNELS);
        for _ in 0..(6 * renders_per_s) {
            on.render(&mut buf);
        }
        assert_eq!(
            on.bed.level, 0.0,
            "the bed snaps to exact zero after the typing"
        );
        assert_eq!(on.bed.energy, 0.0);
        assert!(on.is_quiet(), "idle parks the device");
    }

    fn type_degrees(text: &str, t0: u32, period: u32) -> Vec<i8> {
        type_degrees_rhythm(text, t0, &[period])
    }

    fn type_degrees_rhythm(text: &str, t0: u32, periods: &[u32]) -> Vec<i8> {
        let mut s = synth();
        let mut at = t0;
        let mut out = Vec::new();
        for (i, ch) in text.chars().enumerate() {
            let kind = match ch {
                ' ' => SoundKind::Space,
                '\n' => SoundKind::Enter { cells: 30 },
                _ => SoundKind::Typed,
            };
            push_ch(&mut s, kind, at, ch);
            if matches!(kind, SoundKind::Typed) {
                out.push(s.v2.walk());
            }
            at += periods[i % periods.len()];
        }
        out
    }

    fn push(s: &mut TrailSynth, kind: SoundKind, at: u32, pan: f32, shifted: bool) {
        s.push_meta(
            event(kind, pan, shifted),
            EventMeta {
                at_ms: at,
                ..EventMeta::default()
            },
        );
    }

    fn since(s: &TrailSynth, mark: u32) -> Vec<Voice> {
        let mut v: Vec<Voice> = s
            .voices
            .iter()
            .filter(|v| v.on && v.born > mark)
            .copied()
            .collect();
        v.sort_by_key(|v| v.born);
        v
    }

    fn tune_voices(v: &[Voice]) -> Vec<Voice> {
        v.iter().filter(|v| v.lane == LANE_TUNE).copied().collect()
    }

    #[test]
    fn the_verdict_is_the_music_boxs_alone_and_never_louder_than_a_key() {
        fn verdict(voice: SoundVoice, style: GlowStyle, failed: bool, ice: bool) -> SoundEvent {
            SoundEvent {
                style,
                voice,
                kind: SoundGesture::Output(OutputGesture::Verdict { failed, ice }),
                pan: 0.0,
                heat: 0.0,
                hue: 0.0,
                gain: VOL,
                tone: Tone::Technical,
                bed: false,
                shifted: false,
            }
        }

        for voice in [
            SoundVoice::Mech,
            SoundVoice::Typewriter,
            SoundVoice::Marimba,
            SoundVoice::Felt,
            SoundVoice::Of(GlowStyle::Water),
            SoundVoice::Of(GlowStyle::Lumen),
        ] {
            let mut s = synth();
            s.push(verdict(voice, GlowStyle::RainbowKitty, false, true));
            assert_eq!(
                render_peak(&mut s, 64),
                0.0,
                "{voice:?} has no verdict — a cat and a music box, or nothing"
            );
        }

        let mut green = synth();
        green.push(verdict(
            SoundVoice::RainbowKittyV2,
            GlowStyle::RainbowKitty,
            false,
            true,
        ));
        let g = render_mono(&mut green, 64);
        assert!(g.iter().any(|x| x.abs() > 1e-4), "a success is told");

        let mut red = synth();
        red.push(verdict(
            SoundVoice::RainbowKittyV2,
            GlowStyle::RainbowKitty,
            true,
            false,
        ));
        let r = render_peak(&mut red, 64);
        assert!(r > 0.0, "a failure is told");

        let mut key = synth();
        push_ch(&mut key, SoundKind::Typed, 0, 'a');
        let k = render_peak(&mut key, 64);
        let g_peak = g.iter().fold(0.0f32, |a, x| a.max(x.abs()));
        let ceiling = k * 0.501_187_2;
        assert!(
            g_peak <= ceiling,
            "the green verdict delivered {:.2} dB re a step — the ceiling is −6",
            20.0 * (g_peak / k).log10()
        );
        assert!(
            r <= ceiling,
            "and the red one delivered {:.2} dB re a step",
            20.0 * (r / k).log10()
        );
    }

    #[test]
    fn the_remainder_past_the_cap_is_one_strum_not_a_verse() {
        let mut s = synth();
        let mut at = 1_000;
        for ch in "hello".chars() {
            push_ch(&mut s, SoundKind::Typed, at, ch);
            at += 90;
        }
        let (walk, steps, word_pos) = (s.v2.walk(), s.v2.steps(), s.v2.word_pos());
        let mark = s.born_seq;
        push(&mut s, SoundKind::Strum, at, 0.25, false);
        let strum = since(&s, mark);
        assert_eq!(strum.len(), 4, "four tines, one gesture: {strum:?}");
        assert!(strum.iter().all(|v| v.lane == LANE_CASCADE));
        let lit = sky_bed_degrees(usize::from(s.v2.chord()));
        let want = [lit[0], lit[1], lit[2], lit[0] + 5];
        for (v, deg) in strum.iter().zip(want) {
            let f = penta(TINE_BASE_HZ, i32::from(s.song_key) + deg);
            assert!(
                (v.p[0].f0 - f).abs() < 1e-3,
                "tine at {} Hz is not lit degree {deg} ({f} Hz)",
                v.p[0].f0
            );
        }
        assert!(
            strum.windows(2).all(|w| w[1].p[0].f0 > w[0].p[0].f0),
            "an UP-strum rises string by string"
        );
        assert_eq!(
            (s.v2.walk(), s.v2.steps(), s.v2.word_pos()),
            (walk, steps, word_pos),
            "the verse does not advance on a paste"
        );
        push_ch(&mut s, SoundKind::Typed, at + 90, 'x');
        assert_eq!(s.v2.steps(), steps + 1, "…and the next key is one step");

        for voice in [
            SoundVoice::Mech,
            SoundVoice::Typewriter,
            SoundVoice::Marimba,
            SoundVoice::Felt,
            SoundVoice::Of(GlowStyle::Water),
            SoundVoice::Of(GlowStyle::Lumen),
        ] {
            let mut other = synth();
            other.push(SoundEvent {
                voice,
                ..event(SoundKind::Strum, 0.0, false)
            });
            assert_eq!(render_peak(&mut other, 64), 0.0, "{voice:?} has no strum");
        }
        let mut key = synth();
        push_ch(&mut key, SoundKind::Typed, 0, 'a');
        let k = render_peak(&mut key, 64);
        let mut strummed = synth();
        push(&mut strummed, SoundKind::Strum, 0, 0.0, false);
        let p = render_peak(&mut strummed, 64);
        assert!(p > 0.0, "the music box strums");
        assert!(
            p <= k,
            "a strum delivered {:.2} dB re a step; a paste never out-shouts a key",
            20.0 * (p / k).log10()
        );
    }

    fn render_peak(s: &mut TrailSynth, blocks: usize) -> f32 {
        let mut buf = [0.0f32; 960];
        let mut peak = 0.0f32;
        for _ in 0..blocks {
            s.render(&mut buf);
            for x in buf {
                peak = peak.max(x.abs());
            }
        }
        peak
    }

    fn render_mono(s: &mut TrailSynth, blocks: usize) -> Vec<f32> {
        let mut buf = [0.0f32; 960];
        let mut out = Vec::with_capacity(blocks * 480);
        for _ in 0..blocks {
            s.render(&mut buf);
            for f in buf.as_chunks::<2>().0 {
                out.push(0.5 * (f[0] + f[1]));
            }
        }
        out
    }

    #[test]
    fn every_key_is_a_step_at_every_rate_and_no_key_is_silent() {
        const KEYS: &str = "thequickbrownfoxjumpsoverthelazydogandthenwritesitdownagain";
        for period in [333u32, 250, 222, 200, 167, 125, 100, 71, 50, 33, 20] {
            let mut s = synth();
            let mut spoke = 0;
            let mut at = 1_000u32;
            for ch in KEYS.chars() {
                let mark = s.born_seq;
                push_ch(&mut s, SoundKind::Typed, at, ch);
                if !tune_voices(&since(&s, mark)).is_empty() {
                    spoke += 1;
                }
                at += period;
            }
            let n = KEYS.chars().count() as u32;
            assert_eq!(
                s.v2.steps(),
                n,
                "at {:.1} cps the melody advanced {} times for {n} keys — a gate has grown back",
                1_000.0 / f64::from(period),
                s.v2.steps()
            );
            assert_eq!(
                spoke,
                n as usize,
                "at {:.1} cps only {spoke} of {n} keys spawned a tune voice",
                1_000.0 / f64::from(period)
            );
        }
    }

    #[test]
    fn a_held_key_is_felt_not_silenced_and_the_line_keeps_moving() {
        let mut s = synth();
        let mut onsets = 0;
        let mut pitched = 0;
        for k in 0..60u32 {
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, 1_000 + k * 33, 'a');
            for v in tune_voices(&since(&s, mark)) {
                onsets += 1;
                if v.p[0].lvl > 0.0 {
                    pitched += 1;
                }
            }
        }
        assert_eq!(s.v2.steps(), 60, "the melody stalled under a held key");
        assert_eq!(onsets, 60, "a held key went SILENT — never legal again");
        assert!(
            pitched <= usize::from(AUTOREPEAT_RUN),
            "{pitched} of 60 auto-repeat onsets were pitched; the detector \
             should have taken all but its own run-in to the mallet alone"
        );

        let mut s = synth();
        let mut pitched = 0;
        let jitter = [30u32, 37, 33, 41, 28, 35, 39, 31];
        let mut at = 1_000u32;
        for (k, ch) in "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefgh"
            .chars()
            .enumerate()
        {
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, at, ch);
            if tune_voices(&since(&s, mark))
                .iter()
                .any(|v| v.p[0].lvl > 0.0)
            {
                pitched += 1;
            }
            at += jitter[k % jitter.len()];
        }
        assert_eq!(
            pitched,
            60,
            "a fast HUMAN hand was mistaken for a machine on {} of 60 keys",
            60 - pitched
        );
    }

    #[test]
    fn the_line_is_a_function_of_the_text_and_the_rhythm_and_of_nothing_else() {
        const TEXT: &str = "the quick brown fox jumps over the lazy dog";
        const OTHER: &str = "the quick brown fox jumps over the busy dog";
        let steady = type_degrees(TEXT, 1_000, 200);

        assert_eq!(
            steady,
            type_degrees(TEXT, 1_000, 200),
            "the same text at the same rhythm played two different lines"
        );
        assert_eq!(
            steady,
            type_degrees(TEXT, 40_000, 200),
            "the line moved when the take started later — an absolute clock leaked in"
        );
        assert!(
            !steady.is_empty() && steady.len() == TEXT.chars().filter(|c| *c != ' ').count(),
            "every typed key must have sounded a degree"
        );

        let hurried = type_degrees_rhythm(TEXT, 1_000, &[120, 340, 150, 300, 130]);
        assert_ne!(
            steady, hurried,
            "the same text typed with a different rhythm played the SAME line — \
             the hand is not reaching the melody"
        );

        let other = type_degrees(OTHER, 1_000, 200);
        assert_ne!(
            steady, other,
            "two different sentences at one rhythm played the same line — \
             the text is not reaching the melody"
        );
        let common = steady
            .iter()
            .zip(&other)
            .take_while(|(a, b)| a == b)
            .count();
        let before_change = TEXT
            .chars()
            .zip(OTHER.chars())
            .take_while(|(a, b)| a == b)
            .filter(|(a, _)| *a != ' ')
            .count();
        assert!(
            common >= before_change,
            "the line diverged at key {common}, BEFORE the text did at key \
             {before_change} — something other than the text moved it"
        );
    }

    #[test]
    fn the_run_guard_holds_on_the_sounded_line_not_the_counted_one() {
        const SCENES: [(&str, u32); 6] = [
            ("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 83),
            ("0123456789 0123456789", 83),
            ("!!!!", 83),
            ("aGVsbG8gd29ybGQgdGhpcyBpcyBub3QgcHJvc2U+Pz8/", 83),
            ("the the the the the the the the ", 83),
            ("!!!! 999 aaaa", 286),
        ];
        let mut takes: Vec<(&str, Vec<i8>)> = Vec::new();
        let mut s = synth();
        let mut at = 500u32;
        let mut degs = Vec::new();
        for (text, period) in SCENES {
            for ch in text.chars() {
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, at, ch);
                if kind == SoundKind::Typed {
                    degs.push(s.v2.walk());
                }
                at += period;
            }
            at += 700;
        }
        takes.push(("pathological, as the bench types it", degs));
        let mut s = synth();
        let mut at = 500u32;
        let mut degs = Vec::new();
        for ch in BENCH_PROSE.chars() {
            let kind = match ch {
                ' ' => SoundKind::Space,
                '\n' => SoundKind::Jump,
                _ => SoundKind::Typed,
            };
            push_ch(&mut s, kind, at, ch);
            if kind == SoundKind::Typed {
                degs.push(s.v2.walk());
            }
            at += 100;
            if ch == '\n' {
                at += 350;
            }
            if ch == '.' {
                at += 550;
            }
        }
        takes.push(("prose, as the bench types it", degs));
        for periods in [
            &[100u32][..],
            &[83, 91, 83, 77, 83],
            &[140, 500, 130, 620, 150],
        ] {
            takes.push(("prose", type_degrees_rhythm(BENCH_PROSE, 1_000, periods)));
        }
        for (name, degs) in takes {
            assert!(
                degs.iter()
                    .all(|d| (TUNE_DEG_LO..=TUNE_DEG_HI).contains(&i32::from(*d))),
                "the line left the TUNE register on {name}"
            );
            let mut run = 1usize;
            let mut worst = (1usize, 0usize);
            for (i, w) in degs.windows(3).enumerate() {
                if w[1] - w[0] == w[2] - w[1] && w[1] != w[0] {
                    run += 1;
                } else {
                    run = 1;
                }
                if run > worst.0 {
                    worst = (run, i + 2);
                }
            }
            assert!(
                worst.0 <= usize::from(MELODY_RUN_MAX),
                "{} identical strides SOUNDED without turning on {name}, ending at key {} \
                 (…{:?}) — the guard counted a stride the ear did not get",
                worst.0,
                worst.1,
                &degs[worst.1.saturating_sub(5)..=worst.1]
            );
        }
    }

    #[test]
    fn every_repeated_pitch_is_charged_to_a_cause_the_design_names() {
        const PATHOLOGICAL: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1234567890 !!!! \
             aGVsbG8gd29ybGQgdGhpcyBpcyBub3QgcHJvc2U ffff the the the the the the the the";
        const RHYTHMS: [&[u32]; 4] = [
            &[200],
            &[83, 91, 83, 77, 83],
            &[140, 500, 130, 620, 150],
            &[71],
        ];
        for (name, text, ordinary) in [
            ("prose", BENCH_PROSE, true),
            ("pathological", PATHOLOGICAL, false),
        ] {
            for periods in RHYTHMS {
                let mut s = synth();
                let mut at = 1_000u32;
                let (mut keys, mut dbl, mut head, mut subj, mut stall) = (0, 0, 0, 0, 0);
                let mut phr = 0;
                let mut prev_rank = 0u8;
                let mut prev_deg: Option<i8> = None;
                for (i, ch) in text.chars().enumerate() {
                    let kind = match ch {
                        ' ' => SoundKind::Space,
                        '\n' => SoundKind::Enter { cells: 30 },
                        _ => SoundKind::Typed,
                    };
                    let was_head = s.v2.word_pos() == 0;
                    let was_answer = !was_head && s.v2.motif_answering();
                    let was_marked = s.v2.token_marked();
                    let rank = crate::trail_sound::typed_glyph_rank(Some(ch));
                    push_ch(&mut s, kind, at, ch);
                    if matches!(kind, SoundKind::Typed) {
                        keys += 1;
                        let deg = s.v2.walk();
                        let class = crate::trail_sound::typed_glyph_class(Some(ch));
                        let phrased =
                            matches!(class, DASH | CLOSE) || (!was_marked && s.v2.token_marked());
                        if prev_deg == Some(deg) {
                            // Charged to the branch that produced the degree:
                            // an answered interval never consulted the alphabet.
                            if phrased {
                                phr += 1;
                            } else if was_answer {
                                subj += 1;
                            } else if rank != 0 && rank == prev_rank {
                                dbl += 1;
                            } else if was_head {
                                head += 1;
                            } else {
                                stall += 1;
                            }
                        }
                        if rank != 0 {
                            prev_rank = rank;
                        }
                        prev_deg = Some(deg);
                    }
                    at += periods[i % periods.len()];
                }
                assert!(
                    keys > 100,
                    "fixture too short to say anything ({keys} keys on {name})"
                );
                assert_eq!(
                    stall, 0,
                    "{stall} of {keys} keys on {name} at rhythm {periods:?} repeated \
                     the sounding pitch with no cause §3.1 names — the line stood \
                     still while the hand moved (dbl {dbl}, head {head}, subj {subj}, \
                     phr {phr})"
                );
                assert!(
                    (head + subj) * 10 < keys,
                    "the ENGINE repeated the sounding pitch on {} of {keys} keys on \
                     {name} at rhythm {periods:?} (head {head}, subj {subj}) — \
                     accounted for, but that is the instrument's repetition, not \
                     the text's",
                    head + subj
                );
                assert!(
                    !ordinary || (dbl + head + subj + phr) * 10 < keys,
                    "{} of {keys} keys on {name} at rhythm {periods:?} repeated the \
                     last pitch (dbl {dbl}, head {head}, subj {subj}, phr {phr}) — \
                     accounted for, but that is no longer a derived line",
                    dbl + head + subj + phr
                );
            }
        }
    }

    const BENCH_PROSE: &str = "\
the renderer keeps one atlas per face and never uploads a glyph twice.\n\
when the shaper hands back a run we look up each cluster, and only the\n\
misses cost anything at all.\n\
\n\
a cache that is wrong is worse than no cache, so the key carries the\n\
face id, the pixel size and the synthesis flags. two faces that differ\n\
only in weight can never collide.\n\
\n\
the slow path is deliberate. it runs once per new glyph and then never\n\
again for the life of the window, which is the whole point of paying\n\
for it up front.\n\
";

    type Cue = (f32, SoundKind, f32, f32, bool, u8);

    fn bench_needs_shift(ch: char) -> bool {
        ch.is_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(ch)
    }

    fn bench_type_text(cues: &mut Vec<Cue>, t0: f32, cps: f32, text: &str, heat: f32) -> f32 {
        let mut t = t0;
        let dt = 1.0 / cps;
        let mut col = 0.0f32;
        for ch in text.chars() {
            let pan = (col / 68.0).clamp(0.0, 1.0) * 1.8 - 0.9;
            let kind = match ch {
                ' ' => SoundKind::Space,
                '\n' => SoundKind::Jump,
                _ => SoundKind::Typed,
            };
            let class = if kind == SoundKind::Typed {
                crate::trail_sound::typed_glyph_class(Some(ch))
            } else {
                0
            };
            cues.push((t, kind, pan, heat, bench_needs_shift(ch), class));
            t += dt;
            if ch == '\n' {
                col = 0.0;
                t += 0.35;
            } else {
                col += 1.0;
            }
            if ch == '.' {
                t += 0.55;
            }
        }
        t
    }

    fn bench_prose_cues() -> Vec<Cue> {
        let mut cues = Vec::new();
        let mut t = 0.5f32;
        while t < 60.0 {
            t = bench_type_text(&mut cues, t, 10.0, BENCH_PROSE, 0.55);
            t += 1.4;
        }
        cues.retain(|c| c.0 < 60.0);
        cues
    }

    fn push_meta_ch(s: &mut TrailSynth, kind: SoundKind, at: u32, ch: char, flow: f32) {
        s.push_meta(
            event(kind, 0.0, ch.is_uppercase()),
            EventMeta {
                at_ms: at,
                glyph_class: crate::trail_sound::typed_glyph_class(Some(ch)),
                rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                flow,
                ..EventMeta::default()
            },
        );
    }

    #[derive(Clone, Copy, Debug)]
    struct Spawn {
        cue: usize,
        lane: u8,
        f0: f32,
        deg: i8,
    }

    fn drive_bench_prose(cues: &[Cue]) -> (TrailSynth, Vec<Spawn>, Vec<u8>) {
        const BLOCK: usize = 512;
        let mut s = TrailSynth::new(SR, 0x504F_4F46);
        let frames = (61.5 * SR) as usize;
        let mut stereo = vec![0.0f32; BLOCK * 2];
        let mut log = Vec::new();
        let mut word_pos = Vec::with_capacity(cues.len());
        let (mut f, mut ci) = (0usize, 0usize);
        while f < frames {
            let n = BLOCK.min(frames - f);
            let t = f as f32 / SR;
            while ci < cues.len() && cues[ci].0 <= t {
                let (ct, kind, pan, heat, shifted, glyph_class) = cues[ci];
                let mut ev = event(kind, pan, shifted);
                ev.heat = heat;
                ev.hue = (ct * 0.18).fract();
                ev.voice = SoundVoice::RainbowKittyV2;
                let mark = s.born_seq;
                s.push_meta(
                    ev,
                    EventMeta {
                        glyph_class,
                        ..EventMeta::default()
                    },
                );
                for v in since(&s, mark) {
                    log.push(Spawn {
                        cue: ci,
                        lane: v.lane,
                        f0: v.p[0].f0,
                        deg: s.v2.walk(),
                    });
                }
                word_pos.push(s.v2.word_pos());
                ci += 1;
            }
            s.render(&mut stereo[..n * 2]);
            f += n;
        }
        (s, log, word_pos)
    }

    #[test]
    fn consecutive_typed_onsets_are_never_a_leap_inside_a_word() {
        let cues = bench_prose_cues();
        let (s, log, word_pos) = drive_bench_prose(&cues);
        assert_eq!(s.steals(), 0, "the prose take ran the voice pool dry");

        let typed: Vec<Spawn> = log
            .iter()
            .filter(|e| e.lane == LANE_TUNE && matches!(cues[e.cue].1, SoundKind::Typed))
            .copied()
            .collect();
        let mut pairs = 0usize;
        for w in typed.windows(2) {
            let (a, b) = (w[0], w[1]);
            if cues[a.cue + 1..b.cue]
                .iter()
                .any(|c| !matches!(c.1, SoundKind::Typed))
            {
                continue;
            }
            pairs += 1;
            let leap = i32::from(b.deg) - i32::from(a.deg);
            assert!(
                leap.abs() <= WORD_LEAP_MAX_DEG,
                "an in-word leap of {leap} degrees ({:.0} Hz -> {:.0} Hz) at cue {} — \
                 this is exactly v1's ghost defect",
                a.f0,
                b.f0,
                b.cue
            );
        }
        assert!(
            pairs >= 250,
            "only {pairs} in-word pairs — the take is not the bench's"
        );

        let lead: Vec<Spawn> = log
            .iter()
            .filter(|e| matches!(e.lane, LANE_TUNE | LANE_CASCADE) && e.f0 > 0.0)
            .copied()
            .collect();
        let mut wide = 0usize;
        for w in lead.windows(2) {
            let (a, b) = (w[0], w[1]);
            let up = (b.f0 / a.f0).max(a.f0 / b.f0);
            if up <= 5.0 / 3.0 + 1e-3 {
                continue;
            }
            wide += 1;
            let wrap = a.deg == TUNE_DEG_HI as i8 && b.deg == TUNE_DEG_LO as i8;
            let line_feed = cues[a.cue..=b.cue]
                .iter()
                .any(|c| matches!(c.1, SoundKind::Jump));
            assert!(
                wrap || line_feed,
                "a lead pair {:.0} Hz -> {:.0} Hz ({up:.2}x) at cue {} is neither the form \
                 wrap nor a line feed",
                a.f0,
                b.f0,
                b.cue
            );
        }
        assert!(
            wide > 0,
            "the take has no wide pair at all — the finding is vacuous"
        );

        for (i, c) in cues.iter().enumerate() {
            if !matches!(c.1, SoundKind::Jump) {
                continue;
            }
            if let Some(next) = cues[i + 1..]
                .iter()
                .position(|c| matches!(c.1, SoundKind::Typed))
            {
                assert_eq!(
                    word_pos[i + 1 + next],
                    1,
                    "the first letter after the line feed at cue {i} was not a word head"
                );
            }
        }
    }

    #[test]
    fn a_mid_word_rest_never_cadences_by_more_than_a_sixth() {
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        push(&mut s, SoundKind::Typed, 1_100, 0.0, false);
        assert_eq!(s.v2.word_pos(), 2, "fixture: two letters into a word");
        let before = s.v2.walk();
        let mark = s.born_seq;
        push(&mut s, SoundKind::Typed, 2_100, 0.0, false);
        let v = tune_voices(&since(&s, mark))[0];
        let ratio = v.p[0].f0 / penta(TINE_BASE_HZ, 0);
        assert!(
            ratio <= 5.0 / 3.0 + 1e-4,
            "the key after a mid-word rest leapt {ratio:.3}x from C5 — E6 is a tenth, \
             the ghost defect on the one key still inside the word"
        );
        assert!(
            (i32::from(s.v2.walk()) - i32::from(before)).abs()
                <= WORD_HEAD_SNAP_DEG + WORD_LEAP_MAX_DEG,
            "the rest resolved by more than the resolution plus one derived stride"
        );
    }

    #[test]
    fn the_column_and_the_mood_never_move_the_tune() {
        let script = |pan: f32, tone: Tone| -> Vec<f32> {
            let mut s = synth();
            let mut out = Vec::new();
            for k in 0..24u32 {
                let mark = s.born_seq;
                let mut ev = event(SoundKind::Typed, pan, false);
                ev.tone = tone;
                s.push_meta(
                    ev,
                    EventMeta {
                        at_ms: 1_000 + k * 250,
                        ..EventMeta::default()
                    },
                );
                for v in tune_voices(&since(&s, mark)) {
                    out.push(v.p[0].f0);
                }
            }
            out
        };
        let reference = script(0.0, Tone::Technical);
        assert!(!reference.is_empty());
        for pan in [-0.9f32, 0.0, 0.9] {
            for tone in Tone::ALL {
                assert_eq!(
                    script(pan, tone),
                    reference,
                    "pan {pan} / {tone:?} moved the tune"
                );
            }
        }
    }

    #[test]
    fn a_run_of_capitals_keeps_its_contour_instead_of_stacking_on_the_ceiling() {
        let line = |text: &str| -> Vec<i32> {
            let mut s = synth();
            let mut at = 1_000u32;
            let mut degs = Vec::new();
            for ch in text.chars() {
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, at, ch);
                if kind == SoundKind::Typed {
                    degs.push(i32::from(s.v2.walk()));
                }
                at += 110;
            }
            degs
        };
        let caps = line("THE QUICK BROWN FOX JUMPS OVER THE LAZY DOG");
        let lower = line("the quick brown fox jumps over the lazy dog");
        assert!(caps.len() > 30, "fixture too short ({} keys)", caps.len());

        let mean = |v: &[i32]| v.iter().sum::<i32>() as f32 / v.len() as f32;
        let ceiling = caps.iter().filter(|&&d| d == TUNE_DEG_HI).count();
        assert!(
            ceiling * 4 < caps.len(),
            "{ceiling} of {} shouted keys sounded the single degree {TUNE_DEG_HI}: \
             a plateau, not a line ({caps:?})",
            caps.len()
        );
        let top = caps.iter().filter(|&&d| d >= TUNE_DEG_HI - 1).count();
        assert!(
            top * 2 < caps.len(),
            "{top} of {} shouted keys sat on the top two degrees ({caps:?})",
            caps.len()
        );
        let distinct = {
            let mut v = caps.clone();
            v.sort_unstable();
            v.dedup();
            v.len()
        };
        assert!(
            distinct >= 5,
            "a shouted line visited only {distinct} degrees ({caps:?})"
        );
        assert!(
            mean(&caps) > mean(&lower),
            "shouted {:.2} vs spoken {:.2}: the lift stopped being audible",
            mean(&caps),
            mean(&lower)
        );
    }

    #[test]
    fn a_bare_shift_is_a_ting_that_never_steps() {
        let fold = |mut f: f32| -> f32 {
            while f >= 2_480.0 {
                f *= 0.5;
            }
            while f < 1_240.0 {
                f *= 2.0;
            }
            f
        };
        for (chord, live) in CHORD_LOOP.iter().enumerate() {
            for key in -3i8..=4 {
                let mut heard: Vec<f32> = Vec::new();
                let mut s = synth();
                s.v2.text.chord = chord as u8;
                s.song_key = key;
                for n in 0..10u32 {
                    let mark = s.born_seq;
                    push(&mut s, SoundKind::Shift, 1_000 + n * 300, 0.0, false);
                    let lift = since(&s, mark);
                    assert_eq!(lift.len(), 1, "chord {chord} Shift {n}: one voice");
                    let v = lift[0];
                    let f = v.p[0].f0;
                    let class = (0..5)
                        .find(|&c| (fold(penta(TINE_BASE_HZ, c + i32::from(key))) - f).abs() < 0.5)
                        .unwrap_or_else(|| {
                            panic!("chord {chord} key {key} Shift {n}: {f} Hz is off the lattice")
                        });
                    assert!(
                        (1_240.0..2_480.0).contains(&f) && live.lit & (1 << class) != 0,
                        "chord {chord} key {key} Shift {n}: the ting sounded {f} Hz, class \
                         {class}, not a lit tone of {:#07b} in the ting's band",
                        live.lit
                    );
                    assert!(
                        v.p.iter()
                            .all(|p| p.fm_ratio == 0.0 && p.fm_i0 == 0.0 && p.glide == 0.0),
                        "Shift {n}: the ting wears no FM and does not bend"
                    );
                    assert_eq!(v.lane, LANE_TING, "the ting lives in its own lane");
                    heard.push(f);
                }
                assert!(
                    heard
                        .windows(2)
                        .all(|w| (w[0] - w[1]).abs() > SAME_PITCH_HZ),
                    "chord {chord} key {key}: a pitch repeated back to back in {heard:?}"
                );
                let mut distinct = heard.clone();
                distinct.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
                distinct.dedup_by(|a, b| (*a - *b).abs() < SAME_PITCH_HZ);
                assert_eq!(distinct.len(), 3, "root, fifth and third: {heard:?}");
                assert_eq!(
                    usize::from(s.v2.text.chord),
                    chord,
                    "a Shift moved the harmony"
                );
            }
        }

        let mut s = synth();
        type_line(&mut s, "hello ", 1_000, 150);
        let before = (
            s.v2.walk(),
            s.v2.steps(),
            s.v2.chord(),
            s.since_voice.to_bits(),
        );
        for k in 0..5u32 {
            push(&mut s, SoundKind::Shift, 1_900 + k * 300, 0.0, false);
        }
        assert_eq!(
            (
                s.v2.walk(),
                s.v2.steps(),
                s.v2.chord(),
                s.since_voice.to_bits()
            ),
            before,
            "five bare Shifts moved the walk, the step count, the chord or v1's beat — a \
             modifier never steps"
        );

        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        push(&mut s, SoundKind::Shift, 1_300, 0.0, false);
        let (slot, born) = s.v2.lift.expect("the Shift left its ting's address");
        push(&mut s, SoundKind::Shift, 1_340, 0.0, false);
        let first = &s.voices[usize::from(slot)];
        assert!(
            first.on && first.born == born,
            "the first ting's slot was recycled under it"
        );
        assert_eq!(
            first.damp, LANE_FADE_STEAL_S,
            "a second Shift must damp the first ting over 12 ms — never two tings"
        );
        assert_ne!(
            s.v2.lift,
            Some((slot, born)),
            "the second Shift must take over the ting's address"
        );
    }

    #[test]
    fn a_strike_or_a_ring_on_the_tings_pitch_never_combs() {
        let cents = |a: f32, b: f32| (1200.0 * (a / b).log2()).abs();
        let (mut strikes, mut forced) = (0usize, 0usize);
        for ctx in ["hello ", "a ", "zoo ", "mmm tt "] {
            for chord in 0..CHORD_LOOP.len() as u8 {
                for k in 0..5u8 {
                    let mut base = synth();
                    type_line(&mut base, ctx, 1_000, 150);
                    let _ = render_mono(&mut base, 40);
                    base.v2.text.chord = chord;
                    base.shift_step = k;
                    push(&mut base, SoundKind::Shift, 2_400, 0.0, false);
                    let (slot, born) = base.v2.lift.expect("the ting's address");
                    let slot = usize::from(slot);
                    let ting_f = base.voices[slot].p[0].f0;
                    for ch in ('a'..='z').chain('A'..='Z') {
                        let mut s = base.clone();
                        let mark = s.born_seq;
                        push_ch(&mut s, SoundKind::Typed, 2_460, ch);
                        let strike = tune_voices(&since(&s, mark))[0];
                        let ting_v = s.voices[slot];
                        assert!(ting_v.on && ting_v.born == born);
                        let on_it = cents(strike.p[0].f0, ting_f) < 1.0;
                        strikes += usize::from(on_it);
                        assert_eq!(
                            ting_v.damp,
                            if on_it { LANE_FADE_STEAL_S } else { 0.0 },
                            "chord {chord} step {k} `{ch}`: a strike at {} Hz against a ting at \
                             {ting_f} Hz",
                            strike.p[0].f0
                        );
                        let Some((ring_slot, _)) = s.v2.ring else {
                            continue;
                        };
                        let ring_f = s.voices[usize::from(ring_slot)].p[0].f0;
                        let mut tuned = base.clone();
                        tuned.voices[slot].p[0].f0 = ring_f;
                        let mark = tuned.born_seq;
                        push_ch(&mut tuned, SoundKind::Typed, 2_460, ch);
                        forced += 1;
                        assert!(
                            tuned.v2.ring.is_none()
                                && !since(&tuned, mark).iter().any(|v| v.lane == LANE_GRAFT),
                            "chord {chord} step {k} `{ch}`: a ring swelled in on a live ting \
                             tuned to its own {ring_f} Hz"
                        );
                    }
                }
            }
        }
        assert!(
            strikes > 0 && forced > 0,
            "fixture: the search must reach a strike collision ({strikes}) and a ringing \
             capital ({forced})"
        );
    }

    #[test]
    fn the_bell_yields_to_the_shifted_key_it_announced_and_to_nothing_else() {
        let take = |ctx: &str, keys: &str, unducked: bool| -> (Voice, Vec<f32>) {
            let mut s = synth();
            s.ting_unducked = unducked;
            let mut at = 1_000u32;
            for c in ctx.chars() {
                at = type_line(&mut s, &c.to_string(), at, 150);
                let _ = render_mono(&mut s, 15);
            }
            push(&mut s, SoundKind::Shift, at, 0.0, false);
            let (slot, born) = s.v2.lift.expect("the ting's address");
            let _ = render_mono(&mut s, 6);
            for (i, c) in keys.chars().enumerate() {
                let kind = if c == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                let shifted = c.is_uppercase() || "(!".contains(c);
                s.push_meta(
                    event(kind, 0.0, shifted),
                    EventMeta {
                        at_ms: at + 60 + i as u32 * 10,
                        glyph_class: crate::trail_sound::typed_glyph_class(Some(c)),
                        rank: crate::trail_sound::typed_glyph_rank(Some(c)),
                        ..EventMeta::default()
                    },
                );
                if i + 1 < keys.len() {
                    let _ = render_mono(&mut s, 1);
                }
            }
            let v = s.voices[usize::from(slot)];
            assert!(
                v.on && v.born == born && v.damp == 0.0 && s.v2.lift == Some((slot, born)),
                "fixture ({ctx:?} + {keys:?}): the ting is live, undamped, at its address"
            );
            s.mute_all_lanes_but(LANE_TING);
            (v, render_mono(&mut s, 4))
        };
        for (ctx, keys, ducks) in [
            ("hello ", "W", Some(TING_DUCK)),
            ("", "W", Some(TING_DUCK)),
            ("he", "L", Some(TING_DUCK)),
            ("hello ", "w", None),
            ("hello ", "7", None),
            ("hello ", "(", Some(TING_DUCK_MARK)),
            ("hello ", "!", Some(TING_DUCK_MARK)),
            ("hello", " ", None),
        ] {
            let (_, x) = take(ctx, keys, false);
            let (_, x0) = take(ctx, keys, true);
            let Some(duck) = ducks else {
                assert_eq!(
                    x, x0,
                    "{ctx:?} + {keys:?}: only the capital it announced ducks"
                );
                continue;
            };
            for (a, b) in x[960..].iter().zip(&x0[960..]) {
                assert!(
                    (a - b * duck).abs() <= 1e-6 + 1e-3 * b.abs(),
                    "{ctx:?} + {keys:?}: past the ramp the ducked bell is {a}, the unducked \
                     {b} × {duck}"
                );
            }
            let step = |x: &[f32]| x.windows(2).fold(0.0f32, |m, w| m.max((w[1] - w[0]).abs()));
            assert!(
                step(&x) <= step(&x0),
                "{ctx:?} + {keys:?}: the duck is a step ({} against the bell's own {})",
                step(&x),
                step(&x0)
            );
        }
        let (one, _) = take("hello ", "W", false);
        let (two, _) = take("hello ", "WO", false);
        assert_eq!(
            (two.pan_t0, two.gl1, two.gr1),
            (one.pan_t0, one.gl1, one.gr1),
            "a run's second capital re-armed the duck"
        );
    }

    #[test]
    fn the_ting_rings_through_the_next_key() {
        for (kind, ch) in [
            (SoundKind::Typed, Some('W')),
            (SoundKind::Typed, Some('?')),
            (SoundKind::Typed, Some('(')),
            (SoundKind::Typed, Some('+')),
            (SoundKind::Backspace, None),
            (SoundKind::Navigation, None),
            (SoundKind::Stardust { twinkle_hz: 9 }, None),
        ] {
            let mut s = synth();
            type_line(&mut s, "hello ", 1_000, 150);
            push(&mut s, SoundKind::Shift, 1_900, 0.0, false);
            let (slot, born) = s.v2.lift.expect("the Shift left its ting's address");
            match ch {
                Some(c) => s.push_meta(
                    event(kind, 0.0, true),
                    EventMeta {
                        at_ms: 1_960,
                        glyph_class: crate::trail_sound::typed_glyph_class(Some(c)),
                        rank: crate::trail_sound::typed_glyph_rank(Some(c)),
                        ..EventMeta::default()
                    },
                ),
                None => push(&mut s, kind, 1_960, 0.0, false),
            }
            let mut buf = [0.0f32; 960];
            for _ in 0..16 {
                s.render(&mut buf);
            }
            let v = &s.voices[usize::from(slot)];
            assert!(
                v.on && v.born == born && v.damp == 0.0 && s.v2.lift == Some((slot, born)),
                "{kind:?} {ch:?}: 160 ms on, the ting must still be on, undamped and at its \
                 own address — the 2026-09-10 resolve is the sound the owner could not hear"
            );
        }
    }

    #[test]
    fn a_shifted_key_does_not_bend() {
        for ch in ['A', '7', '!', '('] {
            let mut s = synth();
            let mark = s.born_seq;
            s.push_meta(
                event(SoundKind::Typed, 0.0, true),
                EventMeta {
                    at_ms: 1_000,
                    rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                    glyph_class: crate::trail_sound::typed_glyph_class(Some(ch)),
                    flow: 1.0,
                    ..EventMeta::default()
                },
            );
            let lead = s.voices[usize::from(s.v2.lead.expect("one strike").0)];
            let line = i32::from(s.v2.walk()) + i32::from(s.song_key);
            assert_eq!(
                lead.p[0].f0,
                penta(TINE_BASE_HZ, line),
                "{ch}: the strike is the LINE's degree"
            );
            let spawned = since(&s, mark);
            for v in &spawned {
                assert_eq!(
                    v.p.map(|p| p.glide),
                    [0.0; 3],
                    "{ch}: nothing it spawns bends"
                );
                assert!(
                    v.lp_cut < 7_639.0,
                    "{ch}: a lane-{} voice is under a {} Hz roof — a bypass, not a filter",
                    v.lane,
                    v.lp_cut
                );
            }
            if ch == 'A' {
                assert!(
                    spawned.iter().any(|v| v.lane == LANE_GLINT),
                    "not vacuous: the capital's sparkle is under the clamp"
                );
                let (slot, born) =
                    s.v2.ring
                        .expect("a capital that opens a word owns its ring");
                let ring = s.voices[usize::from(slot)];
                assert_eq!((ring.born, ring.lane), (born, LANE_GRAFT));
                assert_eq!(
                    ring.p[0].f0,
                    penta(
                        TINE_BASE_HZ,
                        line + FLOW_ECHO_OCTAVE_DEG + CAP_RING_LIFT_DEG
                    ),
                    "the ring is the strike's twelfth"
                );
            } else {
                assert_eq!(s.v2.ring, None, "{ch}: a shifted MARK does not ring");
            }
            assert!(
                !s.voices
                    .iter()
                    .any(|v| v.on && v.lane == LANE_BLOOM && v.delay == FLOW_ECHO_DELAY_S),
                "{ch}: a shifted key takes no flow echo — ring or no ring"
            );
        }
        let mut plain = synth();
        push_ch(&mut plain, SoundKind::Typed, 1_000, 'w');
        assert_eq!(plain.v2.ring, None, "a lowercase letter rang");
        let after_rest = |ch: char, shifted: bool| -> Vec<Voice> {
            let mut s = synth();
            type_line(&mut s, "hello ", 1_000, 150);
            let _ = render_mono(&mut s, 200);
            let mark = s.born_seq;
            s.push_meta(
                event(SoundKind::Typed, 0.0, shifted),
                EventMeta {
                    at_ms: 5_000,
                    rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                    glyph_class: crate::trail_sound::typed_glyph_class(Some(ch)),
                    flow: if shifted { 1.0 } else { 0.0 },
                    ..EventMeta::default()
                },
            );
            since(&s, mark)
        };
        let taps = |v: &[Voice]| -> Vec<f32> {
            v.iter()
                .filter(|v| v.lane == LANE_BLOOM && AIR_TAP_DELAY_S.contains(&v.delay))
                .map(|v| v.lp_cut)
                .collect()
        };
        for ch in ['W', '7', '!', '('] {
            let spawned = after_rest(ch, true);
            if ch == 'W' {
                assert_eq!(
                    taps(&spawned).len(),
                    AIR_TAP_DELAY_S.len(),
                    "the rest's room did not answer"
                );
            }
            for v in &spawned {
                assert!(
                    v.lp_cut < 7_639.0,
                    "{ch} after a rest: a lane-{} voice (delay {}) is under a {} Hz roof — \
                     a bypass, not a filter",
                    v.lane,
                    v.delay,
                    v.lp_cut
                );
            }
        }
        let open = taps(&after_rest('w', false));
        assert_eq!(open.len(), AIR_TAP_DELAY_S.len());
        assert!(
            open.iter().all(|c| *c > ROOF_MAX_HZ),
            "the plain key's room lost its open roof: {open:?}"
        );
    }

    #[test]
    fn all_glyph_sparkles_share_the_strikes_loudness_arc() {
        let mag = |v: &Voice| (v.gl * v.gl + v.gr * v.gr).sqrt();
        let mut attenuated = 0;
        for gap in [250u32, 125, 50] {
            for (ch, mul, count, weight) in [
                ('a', 1.0, 1, 1.0),
                ('7', KEY_GLINT_SHIFTED_MUL, 1, 1.0),
                (
                    '!',
                    KEY_GLINT_SHIFTED_MUL,
                    2,
                    crate::trail_sound::SHIFT_GLYPH_GAIN,
                ),
                ('"', QUOTE_GLINT_MUL, 2, 1.0),
                ('(', KEY_GLINT_SHIFTED_MUL, 1, 1.0),
            ] {
                let mut s = synth();
                push_ch(&mut s, SoundKind::Typed, 1_000, 'z');
                let _ = render_mono(&mut s, 40);
                let at = 1_000 + gap;
                let rank = crate::trail_sound::typed_glyph_rank(Some(ch));
                let class = crate::trail_sound::typed_glyph_class(Some(ch));
                let plan = s.v2.clone().on_typed(at, rank, false, false, class);
                assert_eq!(plan.touch, Touch::Step);
                let mark = s.born_seq;
                push_ch(&mut s, SoundKind::Typed, at, ch);
                let g = g_ioi(s.v2.ioi_ms * 0.001);
                attenuated += usize::from(g < 1.0);
                let voices = since(&s, mark);
                let lead = voices
                    .iter()
                    .find(|v| v.lane == LANE_TUNE)
                    .expect("one strike");
                let glints: Vec<_> = voices.iter().filter(|v| v.lane == LANE_GLINT).collect();
                assert_eq!(
                    glints.len(),
                    count,
                    "{ch}: both punctuation winks are tested"
                );
                for glint in glints {
                    let ratio = mag(glint) * plan.level * weight / mag(lead);
                    let want = GLINT_LEVEL * KEY_GLINT_LEVEL_MUL * mul;
                    assert!(
                        (ratio / want - 1.0).abs() < 1e-3,
                        "{ch} at gap {gap}: {ratio} vs {want}"
                    );
                    if g < 1.0 {
                        let without_arc = ratio / g;
                        assert!(
                            (without_arc / want - 1.0).abs() > 1e-3,
                            "negative control: omitting the arc must be observable"
                        );
                    }
                }
            }
        }
        assert!(
            attenuated >= 10,
            "both fast rates must actually exercise attenuation"
        );
    }

    const MARK_SENTENCE: &str =
        "Hello, World! 123 (a test)? Yes! a-b c/d e=f \"q\" [x] y{z} a<b> @# m_n";

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct PhraseKey {
        ch: char,
        from: i8,
        deg: i8,
        steered: bool,
        chord: u8,
    }

    fn type_phrase(
        s: &mut TrailSynth,
        text: &str,
        at: &mut u32,
        period: u32,
        classless: bool,
    ) -> Vec<PhraseKey> {
        let mut keys = Vec::new();
        for ch in text.chars() {
            let kind = match ch {
                ' ' => SoundKind::Space,
                '\n' => SoundKind::Enter { cells: 30 },
                _ => SoundKind::Typed,
            };
            let from = s.v2.walk();
            let was_marked = s.v2.token_marked();
            if classless && kind == SoundKind::Typed {
                s.push_meta(
                    event(kind, 0.0, ch.is_uppercase()),
                    EventMeta {
                        at_ms: *at,
                        glyph_class: LETTER,
                        rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                        ..EventMeta::default()
                    },
                );
            } else {
                push_ch(s, kind, *at, ch);
            }
            if kind == SoundKind::Typed {
                keys.push(PhraseKey {
                    ch,
                    from,
                    deg: s.v2.walk(),
                    steered: !was_marked && s.v2.token_marked(),
                    chord: s.v2.chord(),
                });
            }
            *at += period;
            let _ = render_mono(s, 4);
        }
        keys
    }

    const PHRASE_CORPUS: &str = "Well, that works. Does it ring; or does it knock? \
         Yes: it rings! Wait - a well-made tie, then (an aside) and home. \
         Two, three; four: go. The end.";

    #[test]
    fn punctuation_phrases_the_line() {
        const SEEDS: [u32; 3] = [SEED, 0x5EED_1234, 0xCAFE_F00D];
        let mut census = [0usize; 8];
        let mut who_steers: Option<Vec<bool>> = None;
        for seed in SEEDS {
            for period in [250u32, 100] {
                let mut s = TrailSynth::new(SR, seed);
                let mut at = 1_000;
                let keys = type_phrase(&mut s, PHRASE_CORPUS, &mut at, period, false);
                assert_eq!(s.v2.steps() as usize, keys.len(), "one key, one step");
                let flags: Vec<bool> = keys.iter().map(|k| k.steered).collect();
                assert_eq!(
                    who_steers.get_or_insert_with(|| flags.clone()),
                    &flags,
                    "the steering marks moved with the rate or the seed ({period} ms)"
                );
                let mut paren: Option<i8> = None;
                for (i, k) in keys.iter().enumerate() {
                    let (deg, from) = (i32::from(k.deg), i32::from(k.from));
                    let pc = deg.rem_euclid(5);
                    let what = format!("`{}` (key {i}, {period} ms, seed {seed:#x})", k.ch);
                    if k.steered {
                        assert!(
                            (deg - from).abs() <= STEER_MAX_DEG,
                            "{what}: steered {from} -> {deg}, more than a fourth"
                        );
                    }
                    match k.ch {
                        '.' | ',' | ';' | ':' | '?' | '!' => {
                            assert!(k.steered, "{what}: the corpus's marks all steer");
                            let (slot, ok) = match k.ch {
                                '.' => (0, pc == 0),
                                ',' => (1, matches!(pc, 1 | 3)),
                                ';' => (2, matches!(pc, 2 | 4)),
                                ':' => (3, pc == 3),
                                '?' => (4, matches!(pc, 1 | 3) && (deg > from || from == 8)),
                                _ => (5, matches!(pc, 0 | 3)),
                            };
                            assert!(ok, "{what}: landed on degree {deg} from {from}");
                            census[slot] += 1;
                        }
                        '-' => {
                            assert_eq!(k.deg, k.from, "{what}: a dash is a tie");
                            census[6] += 1;
                        }
                        '(' => paren = Some(k.deg),
                        ')' => {
                            let to = i32::from(paren.take().expect("fixture: a `(` first"));
                            let want = from + (to - from).clamp(-4, 4);
                            assert_eq!(deg, want, "{what}: `)` returns toward {to} from {from}");
                            census[7] += 1;
                        }
                        _ => {}
                    }
                }
                let mut twin = TrailSynth::new(SR, seed);
                let mut at = 1_000;
                let plain = type_phrase(&mut twin, PHRASE_CORPUS, &mut at, period, true);
                assert!(plain.iter().all(|k| !k.steered), "a classless key steered");
                let line = |ks: &[PhraseKey]| ks.iter().map(|k| k.deg).collect::<Vec<_>>();
                assert_ne!(
                    line(&keys),
                    line(&plain),
                    "negative control: the class changed nothing ({period} ms)"
                );
                assert!(
                    plain
                        .iter()
                        .any(|k| k.ch == '.' && i32::from(k.deg).rem_euclid(5) != 0),
                    "negative control: the classless line cadences on C by itself"
                );
            }
        }
        assert!(
            census.iter().all(|n| *n >= 6),
            "fixture: a mark of the table was never measured ({census:?})"
        );

        let mut s = synth();
        let mut at = 1_000;
        let _ = type_phrase(&mut s, "the end", &mut at, 150, false);
        let mark = s.born_seq;
        let keys = type_phrase(&mut s, ".", &mut at, 150, true);
        let born = since(&s, mark);
        assert!(!keys[0].steered && s.v2.pending_mark() == MARK_NONE);
        assert!(born.iter().all(|v| v.lane != LANE_BREATH));
    }

    #[test]
    fn a_sentence_neither_ends_nor_starts_where_the_last_one_did() {
        const LOWER: &str = "it rings. and it knocks. so it goes! on and on. then a rest. \
             and home. two. three! four. go on. the end. yes.";
        let heads_of = |s: &mut TrailSynth, text: &str, period: u32| {
            let (mut heads, mut moved) = (0usize, 0usize);
            let mut at = 1_000u32;
            let mut stop: Option<i8> = None;
            let mut spaced = false;
            for ch in text.chars() {
                if ch == ' ' || stop.is_none() || !spaced {
                    let keys = type_phrase(s, &ch.to_string(), &mut at, period, false);
                    spaced = ch == ' ';
                    if let Some(k) = keys.first() {
                        stop = (matches!(ch, '.' | '!') && k.steered).then_some(k.deg);
                    }
                    continue;
                }
                let closed = stop.take().expect("a stop");
                assert_eq!(s.v2.walk(), closed, "fixture: a Space moved the line");
                let class = crate::trail_sound::typed_glyph_class(Some(ch));
                let rank = crate::trail_sound::typed_glyph_rank(Some(ch));
                let (mut faithful, mut unruled) = (s.v2, s.v2);
                unruled.text.last_mark = MARK_NONE;
                let want = faithful.on_typed(at, rank, false, false, class).deg;
                let bare = unruled.on_typed(at, rank, false, false, class).deg;
                let _ = type_phrase(s, &ch.to_string(), &mut at, period, false);
                assert_eq!(i32::from(s.v2.walk()), want, "fixture: the twin drifted");
                assert_ne!(
                    s.v2.walk(),
                    closed,
                    "`{ch}` in {text:?} ({period} ms): the sentence starts on the note the \
                     last one ended on"
                );
                heads += 1;
                moved += usize::from(bare == i32::from(closed));
            }
            (heads, moved)
        };
        let mut heads = 0usize;
        for seed in [SEED, 0x5EED_1234, 0xCAFE_F00D] {
            for period in [250u32, 150, 100] {
                heads += heads_of(&mut TrailSynth::new(SR, seed), LOWER, period).0;
            }
        }
        assert_eq!(heads, 99, "fixture: eleven heads × nine takes");
        let (mut heads, mut moved) = (0usize, 0usize);
        for words in 0..8 {
            for tail in ["the end. ... and on", "wow! !! yes"] {
                let text = format!("{}{tail}", "so ".repeat(words));
                let (h, m) = heads_of(&mut synth(), &text, 150);
                (heads, moved) = (heads + h, moved + m);
            }
        }
        assert_eq!(
            (heads, moved),
            (16, 16),
            "fixture: the rule is not what moved these heads — P16 is vacuous here"
        );

        for (first, second) in [('.', '.'), ('.', '!'), ('!', '.')] {
            for a in [2i8, 3] {
                for b in [2i8, 3] {
                    let mut s = synth();
                    let mut at = 1_000u32;
                    let _ = type_phrase(&mut s, "so it goes", &mut at, 150, false);
                    s.v2.text.walk = a;
                    let one = type_phrase(&mut s, &first.to_string(), &mut at, 150, false)[0];
                    let _ = type_phrase(&mut s, " and on", &mut at, 150, false);
                    s.v2.text.walk = b;
                    let two = type_phrase(&mut s, &second.to_string(), &mut at, 150, false)[0];
                    assert!(one.steered && two.steered, "fixture: both stops steer");
                    let pcs = |k: PhraseKey, ch| match ch {
                        '.' => k.deg.rem_euclid(5) == 0,
                        _ => matches!(k.deg.rem_euclid(5), 0 | 3),
                    };
                    assert!(pcs(one, first) && pcs(two, second), "{one:?} {two:?}");
                    assert_ne!(
                        two.deg, one.deg,
                        "`{first}` from {a} then `{second}` from {b}: both close on {}",
                        one.deg
                    );
                    push(&mut s, SoundKind::Backspace, at, 0.0, false);
                    at += 150;
                    let _ = render_mono(&mut s, 4);
                    assert_eq!(
                        s.v2.text.walk, b,
                        "fixture: the Backspace restored the walk"
                    );
                    let again = type_phrase(&mut s, &second.to_string(), &mut at, 150, false)[0];
                    assert_eq!(
                        again.deg, two.deg,
                        "the retyped stop forgot the last cadence"
                    );
                }
            }
        }
    }

    fn strike_of(s: &mut TrailSynth, ch: char, at: u32) -> Voice {
        let mark = s.born_seq;
        push_ch(s, SoundKind::Typed, at, ch);
        tune_voices(&since(s, mark))[0]
    }

    fn aside_vs_twin(prefix: &str, ch: char) -> (f32, f32, bool) {
        let mut s = synth();
        let mut at = 1_000;
        let _ = type_phrase(&mut s, prefix, &mut at, 150, false);
        let mut t = s.clone();
        t.v2.close_aside();
        let a = strike_of(&mut s, ch, at);
        let b = strike_of(&mut t, ch, at);
        (
            (a.gl + a.gr) / (b.gl + b.gr),
            a.lp_cut / b.lp_cut,
            a.p[0].f0 == b.p[0].f0,
        )
    }

    #[test]
    fn an_aside_is_sotto_voce_and_ends_where_it_should() {
        const DB: f32 = -2.0;
        for (prefix, ch) in [
            ("so (ab", 'c'),
            ("so (ab", '7'),
            ("so (ab", ','),
            ("x [y", 'z'),
            ("x {y", 'z'),
            ("f(g", '('),
            ("f(g(x", ')'),
        ] {
            let (lvl, roof, pitch) = aside_vs_twin(prefix, ch);
            let db = 20.0 * lvl.log10();
            assert!(
                (db - DB).abs() < 0.01,
                "{prefix:?} + `{ch}`: inside the aside the key is {db:+.3} dB re its twin"
            );
            assert!(
                (roof - ASIDE_ROOF_MUL).abs() < 1e-5,
                "{prefix:?} + `{ch}`: the aside's roof is ×{roof:.4}"
            );
            assert!(pitch, "{prefix:?} + `{ch}`: the aside moved the pitch");
        }
        for (prefix, ch) in [
            ("so", '('),
            ("so (ab", ')'),
            ("f(g(x)", ')'),
            ("so (ab)", 'c'),
            ("so ab", 'c'),
        ] {
            let (lvl, roof, pitch) = aside_vs_twin(prefix, ch);
            assert!(
                lvl == 1.0 && roof == 1.0 && pitch,
                "{prefix:?} + `{ch}` is outside the aside and was touched: ×{lvl} level, \
                 ×{roof} roof"
            );
        }
        let mut s = synth();
        let mut at = 1_000;
        let _ = type_phrase(&mut s, "(", &mut at, 150, false);
        for ch in "abcdefghijklmnopqrstuvwxyzAB!?".chars() {
            let v = strike_of(&mut s, ch, at);
            at += 100;
            assert!(v.lp_cut < 7639.0, "`{ch}` in an aside: roof {}", v.lp_cut);
            let _ = render_mono(&mut s, 2);
        }

        let open = |text: &str| {
            let mut s = synth();
            let mut at = 1_000;
            let _ = type_phrase(&mut s, text, &mut at, 150, false);
            assert!(s.v2.in_aside(), "fixture: {text:?} opens an aside");
            (s, at)
        };
        let (s, _) = open("f(g(x)");
        assert!(s.v2.in_aside(), "an inner close ended the outer aside");
        for (what, kind) in [
            ("an Enter", SoundKind::Enter { cells: 5 }),
            ("a line feed", SoundKind::Jump),
            ("a kill", SoundKind::Kill),
        ] {
            let (mut s, at) = open("so (ab");
            push(&mut s, kind, at, 0.0, false);
            assert!(!s.v2.in_aside(), "{what} left the aside open");
        }
        let (mut s, at) = open("so (ab");
        let mut t = s.clone();
        t.v2.close_aside();
        let a = strike_of(&mut s, 'c', at + 1_500);
        let b = strike_of(&mut t, 'c', at + 1_500);
        assert!(!s.v2.in_aside(), "a rest left the aside open");
        assert_eq!(
            (a.gl, a.lp_cut),
            (b.gl, b.lp_cut),
            "the key after a rest is still sotto voce"
        );
        let (mut s, mut at) = open("(");
        for k in 1..=u32::from(ASIDE_MAX_KEYS) {
            assert!(s.v2.in_aside(), "the aside ended early, before key {k}");
            let _ = type_phrase(&mut s, "a", &mut at, 150, false);
        }
        assert!(!s.v2.in_aside(), "32 keys in, the aside is still open");
        push(&mut s, SoundKind::Backspace, at, 0.0, false);
        at += 150;
        assert!(
            s.v2.in_aside(),
            "a Backspace over the expiry did not re-open it"
        );
        assert_eq!(s.v2.text.aside_keys, ASIDE_MAX_KEYS - 1);
        let _ = type_phrase(&mut s, "a", &mut at, 150, false);
        assert!(
            !s.v2.in_aside(),
            "retyped, the 32nd key did not end the aside"
        );

        let (mut s, at) = open("so (ab");
        let _ = type_phrase(&mut s, ")", &mut at.clone(), 150, false);
        assert!(!s.v2.in_aside(), "fixture: `)` closes");
        push(&mut s, SoundKind::Backspace, at + 150, 0.0, false);
        assert!(
            s.v2.in_aside(),
            "un-typing the `)` did not re-open the aside"
        );
        assert_eq!(s.v2.text.aside_keys, 2, "…with its two keys");
        for _ in 0..3 {
            push(&mut s, SoundKind::Backspace, at + 300, 0.0, false);
        }
        assert!(!s.v2.in_aside(), "un-typing the `(` left the aside open");
    }

    #[test]
    fn a_quotation_lifts_and_comes_home_and_an_apostrophe_is_neither() {
        const TEXTS: [&str; 4] = [
            "she said \"wait for me\" and left",
            "it's 'fine' he said, don't go",
            "call (\"hi\") then \"so\" ok",
            "say `ls` or 'no' now",
        ];
        let glints = |v: &[Voice]| v.iter().filter(|v| v.lane == LANE_GLINT).count();
        let (mut opens, mut lifted, mut closes, mut apostrophes) = (0, 0, 0, 0);
        for seed in [SEED, 0x5EED_1234, 0xCAFE_F00D] {
            for period in [250u32, 100] {
                for text in TEXTS {
                    let mut s = TrailSynth::new(SR, seed);
                    let mut at = 1_000u32;
                    let mut open_deg: Option<i32> = None;
                    for ch in text.chars() {
                        let class = crate::trail_sound::typed_glyph_class(Some(ch));
                        if class != QUOTE {
                            let _ = type_phrase(&mut s, &ch.to_string(), &mut at, period, false);
                            continue;
                        }
                        let rank = crate::trail_sound::typed_glyph_rank(Some(ch));
                        let from = i32::from(s.v2.walk());
                        let head = s.v2.word_pos() == 0 || s.v2.text.after_open;
                        let derived = {
                            let mut twin = s.v2;
                            twin.on_typed(at, rank, false, false, MATH).deg
                        };
                        let mark = s.born_seq;
                        push_ch(&mut s, SoundKind::Typed, at, ch);
                        let born = since(&s, mark);
                        at += period;
                        let _ = render_mono(&mut s, 4);
                        let deg = i32::from(s.v2.walk());
                        match open_deg {
                            Some(q) => {
                                let want = from + (q - from).clamp(-4, 4);
                                assert_eq!(deg, want, "{text:?}: the close did not come home");
                                assert_eq!(glints(&born), 1, "{text:?}: the close sparkles");
                                assert!(!s.v2.in_quote(), "{text:?}: the close left it open");
                                open_deg = None;
                                closes += 1;
                            }
                            None if head => {
                                assert!(
                                    deg >= derived && s.v2.deg_is_lit(deg),
                                    "{text:?}: the open quote is on {deg}, derived {derived}"
                                );
                                assert_eq!(glints(&born), 2, "{text:?}: the open winks twice");
                                assert!(s.v2.in_quote(), "{text:?}: the open did not open");
                                open_deg = Some(deg);
                                opens += 1;
                                lifted += usize::from(deg > derived);
                            }
                            None => {
                                assert_eq!(deg, derived, "{text:?}: the apostrophe was steered");
                                assert_eq!(glints(&born), 2, "{text:?}: the apostrophe's winks");
                                assert!(!s.v2.in_quote(), "{text:?}: an apostrophe opened");
                                apostrophes += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(
            closes > 0 && apostrophes > 0 && lifted > 0 && opens == closes,
            "fixture: {opens} opened ({lifted} lifted), {closes} closed, {apostrophes} apostrophes"
        );

        let mut s = synth();
        let mut at = 1_000;
        let _ = type_phrase(&mut s, "say \"hi", &mut at, 150, false);
        assert!(s.v2.in_quote(), "fixture: the quotation is open");
        let open = s.v2.frame();
        let _ = type_phrase(&mut s, "\"", &mut at, 150, false);
        assert!(!s.v2.in_quote());
        push(&mut s, SoundKind::Backspace, at, 0.0, false);
        at += 150;
        assert_eq!(s.v2.frame(), open, "un-typing the close did not re-open it");
        for _ in 0..3 {
            push(&mut s, SoundKind::Backspace, at, 0.0, false);
            at += 150;
        }
        assert!(!s.v2.in_quote(), "un-typing the open left it open");
        let _ = type_phrase(&mut s, "\"hi\n", &mut at, 150, false);
        assert!(!s.v2.in_quote(), "an Enter left the quotation open");
    }

    #[test]
    fn a_question_s_return_lands_on_the_g() {
        let enter = |text: &str, words: usize| -> f32 {
            let mut s = synth();
            let mut at = 1_000;
            for _ in 0..words {
                let _ = type_phrase(&mut s, "ab ", &mut at, 150, false);
            }
            let _ = type_phrase(&mut s, text, &mut at, 150, false);
            let mark = s.born_seq;
            push(&mut s, SoundKind::Enter { cells: 30 }, at, 0.0, false);
            let born = since(&s, mark);
            let res: Vec<_> = tune_voices(&born)
                .into_iter()
                .filter(|v| v.delay > 0.0)
                .collect();
            assert_eq!(res.len(), 1, "{text:?}: one resolution");
            res[0].p[0].f0
        };
        for words in 0..CHORD_LOOP.len() {
            let home = enter("so it goes.", words);
            for text in ["is it so?", "is it so? ", "he said \"is it so?\""] {
                let f = enter(text, words);
                let g = [CAD_RESOLUTION_LOW_DEG, CAD_RESOLUTION_HIGH_DEG]
                    .map(|d| penta(TINE_BASE_HZ, d + CAD_QUESTION_LIFT_DEG));
                assert!(
                    g.contains(&f),
                    "{text:?} after {words} words: the Return landed on {f:.1} Hz, not a G"
                );
                assert!(
                    (f / home - 1.5).abs() < 1e-4,
                    "{text:?}: the G is not the fifth over the C the full stop took"
                );
            }
            for text in [
                "so it goes.",
                "wow it goes!",
                "so it goes",
                "so it goes,",
                "is it so??",
            ] {
                let f = enter(text, words);
                assert_eq!(
                    f, home,
                    "{text:?} after {words} words: a Return with no question moved"
                );
                assert!(
                    [CAD_RESOLUTION_LOW_DEG, CAD_RESOLUTION_HIGH_DEG]
                        .map(|d| penta(TINE_BASE_HZ, d))
                        .contains(&f),
                    "{text:?}: the Return is not on a C"
                );
            }
        }
    }

    #[test]
    fn the_question_never_rises_a_wolf_in_any_key() {
        let mut fourths = 0;
        for key in -3i8..=4 {
            for from in TUNE_DEG_LO..=TUNE_DEG_HI {
                let mut s = synth();
                let mut at = 1_000u32;
                let _ = type_phrase(&mut s, "is it so", &mut at, 150, false);
                s.song_key = key;
                s.v2.text.walk = from as i8;
                let mark = s.born_seq;
                let k = type_phrase(&mut s, "?", &mut at, 150, false)[0];
                assert!(k.steered, "fixture: the `?` steers");
                assert_eq!(s.song_key, key, "fixture: the key was handed back");
                let born = since(&s, mark);
                let t = tune_voices(&born)[0];
                let g: Vec<_> = born.iter().filter(|v| v.lane == LANE_GRAFT).collect();
                assert_eq!(g.len(), 1, "one rise");
                let ratio = g[0].p[0].f0 / t.p[0].f0;
                let pure = [4.0 / 3.0, 1.5f32, 1.6]
                    .iter()
                    .any(|p| (ratio / p - 1.0).abs() < 1e-4);
                assert!(
                    pure,
                    "key {key}, `?` on degree {}: the rise is {ratio:.4} — not 4/3, 3/2 or 8/5",
                    k.deg
                );
                fourths += usize::from(ratio < 1.4);
            }
        }
        assert!(fourths > 0, "fixture: no `?` ever sounded a D");
    }

    #[test]
    fn a_mark_confirms_its_cadence_in_the_space_bass() {
        const C4: f32 = 261.63;
        const G4: f32 = 392.445;
        const A4: f32 = 436.05;
        let n = CHORD_LOOP.len() as u8;
        let root_of = |c: u8| BASS_BASE_HZ * CHORD_ROOT_RATIO[CHORD_LOOP[usize::from(c)].root];
        for words in 0..n {
            for (tail, want_hz, want_ix) in [
                ("cat.", Some(C4), Some([0u8, 4])),
                ("cat!", Some(C4), Some([0, 4])),
                ("cat,", Some(G4), Some([3, 5])),
                ("cat:", Some(G4), Some([3, 5])),
                ("cat?", Some(G4), Some([3, 5])),
                ("cat;", Some(A4), Some([1, 6])),
                ("cat", None, None),
                ("cat...", None, None),
                ("cat??", None, None),
            ] {
                let mut s = synth();
                let mut at = 1_000;
                for _ in 0..words {
                    let _ = type_phrase(&mut s, "ab ", &mut at, 150, false);
                }
                let _ = type_phrase(&mut s, tail, &mut at, 150, false);
                let before = s.v2.chord();
                let mark = s.born_seq;
                push_ch(&mut s, SoundKind::Space, at, ' ');
                let after = s.v2.chord();
                let bass: Vec<Voice> = since(&s, mark).into_iter().filter(|v| v.bass).collect();
                assert_eq!(bass.len(), 1, "`{tail} `: one downbeat");
                let got = bass[0].p[0].f0;
                assert!(
                    (got - root_of(after)).abs() < 0.5,
                    "`{tail} `: the bass is {got} Hz, not chord {after}'s root"
                );
                let advanced = (after + n - before - 1) % n + 1;
                assert!((1..=n).contains(&advanced), "P10");
                match (want_hz, want_ix) {
                    (Some(hz), Some(ix)) => {
                        assert!(
                            (got - hz).abs() < 0.5,
                            "`{tail} ` from chord {before}: the bass is {got} Hz, not {hz}"
                        );
                        assert!(ix.contains(&after), "`{tail} `: chord {after}");
                        assert!(
                            (1..advanced).all(|k| !ix.contains(&((before + k) % n))),
                            "`{tail} ` from chord {before} skipped a chord of its own function"
                        );
                    }
                    _ => {
                        assert_eq!(advanced, 1, "`{tail} `: an ordinary advance");
                        assert_eq!(s.v2.pending_mark(), MARK_NONE, "`{tail}`");
                    }
                }
            }
        }

        for (mark, relatched) in [('.', true), ('!', true), ('?', true), (',', false)] {
            let mut s = synth();
            let mut at = 1_000;
            let _ = type_phrase(&mut s, "the quick brown fox", &mut at, 150, false);
            assert_eq!(
                usize::from(s.v2.text.motif_len),
                MOTIF_LEN,
                "fixture: a subject"
            );
            let _ = type_phrase(&mut s, &format!("{mark} "), &mut at, 150, false);
            assert_eq!(
                s.v2.text.motif_len == 0,
                relatched,
                "`{mark} `: the subject's latch"
            );
        }

        for tail in ["home.", "home. ", "home!"] {
            let mut closed = synth();
            let mut at = 1_000;
            let _ = type_phrase(&mut closed, "we are ", &mut at, 150, false);
            let _ = type_phrase(&mut closed, tail, &mut at, 150, false);
            let mut open = closed.clone();
            open.v2.text.last_mark = MARK_NONE;
            let enter = |s: &mut TrailSynth| -> usize {
                let mark = s.born_seq;
                push(s, SoundKind::Enter { cells: 30 }, at, 0.0, false);
                tune_voices(&since(s, mark)).len()
            };
            assert_eq!(
                (enter(&mut open), enter(&mut closed)),
                (2, 1),
                "`{tail}`+Enter: the pickup and the resolution, against the resolution alone"
            );
        }
    }

    #[test]
    fn code_punctuation_never_moves_the_harmony() {
        for (token, steered_want) in [
            ("self.v2.walk.foo.bar.baz", 1usize),
            ("3.14", 0),
            ("v0.88.0", 0),
            ("std::io::Result", 1),
            ("a.b,c;d:e", 1),
        ] {
            let mut s = synth();
            let mut at = 1_000;
            let _ = type_phrase(&mut s, "let x = ", &mut at, 110, false);
            let chord = s.v2.chord();
            let keys = type_phrase(&mut s, token, &mut at, 110, false);
            assert_eq!(
                keys.iter().filter(|k| k.steered).count(),
                steered_want,
                "`{token}`: steering marks"
            );
            assert!(
                keys.iter().all(|k| k.chord == chord),
                "`{token}`: a mark inside a token moved the chord"
            );
            let marks: Vec<i8> = keys
                .iter()
                .filter(|k| !k.ch.is_ascii_alphanumeric())
                .map(|k| k.deg)
                .collect();
            assert!(
                marks.windows(3).all(|w| !(w[0] == w[1] && w[1] == w[2])),
                "`{token}`: three mark notes share a pitch ({marks:?})"
            );
            push_ch(&mut s, SoundKind::Space, at, ' ');
            assert_eq!(
                s.v2.chord(),
                (chord + 1) % CHORD_LOOP.len() as u8,
                "`{token} `: the Space behind a letter or a digit is an ordinary advance"
            );
        }
    }

    #[test]
    fn backspace_unsings_a_phrase() {
        for class in (0..=15u8).chain([17]) {
            for shifted in [false, true] {
                let mut s = synth();
                let mut at = 1_000;
                let _ = type_phrase(&mut s, "so (ab", &mut at, 150, false);
                let before = s.v2.frame();
                s.push_meta(
                    event(SoundKind::Typed, 0.0, shifted),
                    EventMeta {
                        at_ms: at,
                        glyph_class: class,
                        rank: 37,
                        ..EventMeta::default()
                    },
                );
                assert_ne!(s.v2.frame(), before, "class {class}: the control is blind");
                push(&mut s, SoundKind::Backspace, at + 150, 0.0, false);
                assert_eq!(
                    s.v2.frame(),
                    before,
                    "class {class} (shifted {shifted}): typed and deleted, the phrase moved"
                );
            }
        }
        let mut s = synth();
        let mut at = 1_000;
        let _ = type_phrase(&mut s, "ab;", &mut at, 150, false);
        let before = s.v2.frame();
        push_ch(&mut s, SoundKind::Space, at, ' ');
        assert_ne!(s.v2.frame().chord, before.chord);
        push(&mut s, SoundKind::Backspace, at + 150, 0.0, false);
        assert_eq!(
            s.v2.frame(),
            before,
            "a deleted Space left its cadence behind"
        );

        let mut s = synth();
        let mut at = 1_000;
        let _ = type_phrase(&mut s, "a", &mut at, 150, false);
        let home = s.v2.frame();
        let first = type_phrase(&mut s, "b. c", &mut at, 150, false);
        let chord = s.v2.chord();
        for _ in 0..4 {
            push(&mut s, SoundKind::Backspace, at, 0.0, false);
            at += 150;
        }
        assert_eq!(s.v2.frame(), home, "four Backspaces did not reach `a`");
        let again = type_phrase(&mut s, "b. c", &mut at, 150, false);
        let line = |ks: &[PhraseKey]| ks.iter().map(|k| (k.deg, k.steered)).collect::<Vec<_>>();
        assert_eq!(
            line(&again),
            line(&first),
            "the retyped phrase is another tune"
        );
        assert_eq!(
            s.v2.chord(),
            chord,
            "the retyped phrase is on another chord"
        );
        assert!(first[1].steered, "fixture: the full stop steered");
    }

    #[test]
    fn an_unnamed_class_id_is_still_the_letter() {
        let rank = crate::trail_sound::typed_glyph_rank(Some('.'));
        let take = |class: u8, shifted: bool| -> (Vec<u8>, Vec<u32>) {
            let mut s = synth();
            let mut out = Vec::new();
            let mut at = 1_000;
            for ch in "hel".chars() {
                push_ch(&mut s, SoundKind::Typed, at, ch);
                out.extend(render_mono(&mut s, 15));
                at += 150;
            }
            let mut lanes = Vec::new();
            for _ in 0..2 {
                let mark = s.born_seq;
                s.push_meta(
                    event(SoundKind::Typed, 0.0, shifted),
                    EventMeta {
                        at_ms: at,
                        glyph_class: class,
                        rank,
                        ..EventMeta::default()
                    },
                );
                lanes.extend(since(&s, mark).iter().map(|v| v.lane));
                out.extend(render_mono(&mut s, 15));
                at += 150;
            }
            push_ch(&mut s, SoundKind::Typed, at, 'o');
            out.extend(render_mono(&mut s, 60));
            (lanes, out.iter().map(|x| x.to_bits()).collect())
        };
        for shifted in [false, true] {
            let letter = take(LETTER, shifted);
            assert!(
                take(STOP, shifted).1 != letter.1,
                "negative control: a full stop rendered as the letter"
            );
            let first = take(17, shifted);
            for unknown in [17u8, 18, 200, 255] {
                let got = take(unknown, shifted);
                assert!(
                    got.1 == first.1,
                    "class {unknown} (shifted {shifted}): an output of its own"
                );
                assert!(
                    !got.0.contains(&LANE_BREATH),
                    "class {unknown} (shifted {shifted}) breathed"
                );
                if !shifted {
                    assert!(
                        got.1 == letter.1,
                        "class {unknown}: not the letter's output"
                    );
                }
            }
        }
    }

    const SECRET_KEYS: &str = "aZ7.,;:-([)]'\"_~/+=@#?! ";

    fn push_secret(s: &mut TrailSynth, at: u32, ch: char, pan: f32) {
        let kind = if ch == ' ' {
            SoundKind::Space
        } else {
            SoundKind::Typed
        };
        s.push_meta(
            event(
                kind,
                pan,
                ch.is_uppercase() || "!?\"(){}_~+@#:".contains(ch),
            ),
            EventMeta {
                at_ms: at,
                glyph_class: SECRET,
                rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                ..EventMeta::default()
            },
        );
    }

    fn prose_prelude(s: &mut TrailSynth) -> u32 {
        let mut at = 1_000;
        for ch in "Hello, wor".chars() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            push_ch(s, kind, at, ch);
            let _ = render_mono(s, 15);
            at += 150;
        }
        at
    }

    #[test]
    fn a_password_key_is_one_tone_whatever_it_was() {
        let take = |ch: char, secret: bool| -> (Vec<String>, Vec<u32>) {
            let mut s = synth();
            let at = prose_prelude(&mut s);
            let mark = s.born_seq;
            if secret {
                push_secret(&mut s, at, ch, (ch as u32 % 7) as f32 / 7.0 - 0.4);
            } else {
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, at, ch);
            }
            let voices = since(&s, mark)
                .iter()
                .map(|v| format!("{v:?}"))
                .collect::<Vec<_>>();
            let out = render_mono(&mut s, 40);
            (voices, out.iter().map(|x| x.to_bits()).collect())
        };
        let first = take('a', true);
        assert_eq!(first.0.len(), 1, "one key, one tone: {:?}", first.0);
        for ch in SECRET_KEYS.chars() {
            let got = take(ch, true);
            assert_eq!(got.0, first.0, "`{ch}`: a voice of its own at the prompt");
            assert!(
                got.1 == first.1,
                "`{ch}`: an output of its own at the prompt"
            );
        }
        let plain: Vec<_> = ['a', 'Z', '7', '.', ' ']
            .iter()
            .map(|&ch| take(ch, false))
            .collect();
        for (i, a) in plain.iter().enumerate() {
            assert!(
                a.1 != first.1,
                "key {i}: its ordinary click RENDERED as the password tone"
            );
            for b in &plain[i + 1..] {
                assert!(
                    a.1 != b.1,
                    "two ordinary keys rendered alike: the control is blind"
                );
            }
        }
    }

    #[test]
    fn a_password_leaves_the_melody_where_it_was() {
        let mut with = synth();
        let at0 = prose_prelude(&mut with);
        let mut without = with.clone();
        let before = format!("{:?}", with.v2);
        let mut at = at0;
        for ch in SECRET_KEYS.chars() {
            push_secret(&mut with, at, ch, 0.0);
            let _ = render_mono(&mut with, 3);
            at += 120;
        }
        assert_eq!(
            format!("{:?}", with.v2),
            before,
            "a password moved the melody's state"
        );
        let _ = render_mono(&mut with, 60);
        let _ = render_mono(&mut without, 60 + 3 * SECRET_KEYS.chars().count());
        with.rng = without.rng;
        let next = |s: &mut TrailSynth| -> (String, Vec<String>) {
            let mark = s.born_seq;
            push_ch(s, SoundKind::Typed, at, 'l');
            let voices = since(s, mark)
                .into_iter()
                .map(|mut v| {
                    v.born = 0;
                    format!("{v:?}")
                })
                .collect();
            (format!("{:?}", s.v2), voices)
        };
        let (a, b) = (next(&mut with), next(&mut without));
        assert_eq!(
            a.1, b.1,
            "the key after the password is not the key it would have been"
        );
        assert!(!a.1.is_empty());
        let mut typed = synth();
        let mut at = prose_prelude(&mut typed);
        let before = format!("{:?}", typed.v2);
        for ch in SECRET_KEYS.chars() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            push_ch(&mut typed, kind, at, ch);
            at += 120;
        }
        assert_ne!(format!("{:?}", typed.v2), before, "the control is blind");
    }

    #[test]
    fn a_re_struck_mark_does_not_spark_or_graft_again() {
        const PERIOD_MS: u32 = 125;
        let lane = |vs: &[Voice], l: u8| -> Vec<Voice> {
            vs.iter().filter(|v| v.lane == l).copied().collect()
        };
        let mut felt_keys = 0usize;
        for run in ["!!!!", "....", "((((", "????"] {
            let mut s = synth();
            type_line(&mut s, "hello ", 1_000, 150);
            let _ = render_mono(&mut s, 12);
            for (i, ch) in run.chars().enumerate() {
                let mark = s.born_seq;
                push_ch(&mut s, SoundKind::Typed, 1_900 + i as u32 * PERIOD_MS, ch);
                let vs = since(&s, mark);
                let class = crate::trail_sound::typed_glyph_class(Some(ch));
                assert_eq!(
                    lane(&vs, LANE_TUNE).len(),
                    1,
                    "`{run}` key {i}: one key is one step, re-strike or not"
                );
                let grafts = lane(&vs, LANE_GRAFT).len();
                let glints = lane(&vs, LANE_GLINT).len();
                felt_keys += usize::from(lane(&vs, LANE_TUNE)[0].p[0].lvl == 0.0);
                if i == 0 {
                    assert!(glints >= 1, "the first `{ch}` did not sparkle");
                    let wanted = usize::from(matches!(class, QMARK | OPEN));
                    assert_eq!(
                        grafts, wanted,
                        "the first `{ch}` grafted {grafts} voices, not {wanted}"
                    );
                } else {
                    assert_eq!(glints, 0, "a re-struck `{ch}` sparkled again");
                    assert_eq!(grafts, 0, "a re-struck `{ch}` grafted again");
                }
                if stop_breathes(class) {
                    let want = usize::from(i == 0);
                    assert_eq!(
                        lane(&vs, LANE_BREATH).len(),
                        want,
                        "`{run}` key {i}: only the steering stop breathes"
                    );
                    assert_eq!(
                        s.v2.pending_mark(),
                        if i == 0 { MARK_PERIOD } else { MARK_NONE },
                        "`{run}` key {i}: only the steering stop books a cadence"
                    );
                }
                let _ = render_mono(&mut s, 12);
            }
        }
        assert!(
            felt_keys > 0,
            "fixture: a machine-regular run never reached the ladder's felt rung"
        );
    }

    #[test]
    fn an_adjacent_pair_closes_with_its_tink() {
        let lane = |vs: &[Voice], l: u8| -> Vec<Voice> {
            vs.iter().filter(|v| v.lane == l).copied().collect()
        };
        for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
            let mut s = synth();
            type_line(&mut s, "hello ", 1_000, 150);
            let _ = render_mono(&mut s, 12);
            push_ch(&mut s, SoundKind::Typed, 1_900, open);
            let _ = render_mono(&mut s, 12);
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, 2_040, close);
            let vs = since(&s, mark);
            assert!(
                s.v2.text.restrike > 0,
                "fixture: `{open}{close}` — the close did not land on its open's degree"
            );
            let g = lane(&vs, LANE_GRAFT);
            assert_eq!(
                g.len(),
                1,
                "`{open}{close}`: the close grafted {} tinks",
                g.len()
            );
            let key = i32::from(s.song_key);
            let want = penta(
                TINE_BASE_HZ,
                reflect_deg(i32::from(s.v2.walk()) - TINK_DEG) + key,
            );
            assert!(
                (g[0].p[0].f0 - want).abs() < SAME_PITCH_HZ,
                "`{open}{close}`: the tink is at {} Hz, not one degree DOWN at {want}",
                g[0].p[0].f0
            );
            let _ = render_mono(&mut s, 12);
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, 2_180, close);
            let vs = since(&s, mark);
            assert!(
                s.v2.text.restrike > 0,
                "fixture: `{close}{close}` — the second close did not re-strike"
            );
            assert!(
                lane(&vs, LANE_GRAFT).is_empty(),
                "`{close}{close}`: a re-struck close tinked again"
            );
        }
    }

    #[test]
    fn shifted_marks_never_steal_their_own_strike_in_a_batched_burst() {
        let mut s = synth();
        let mut full_pool_keys = 0;
        for (index, ch) in "Aa!b?c(D)e{F}g<H>I+J=K\"L:M~N_O|P"
            .chars()
            .cycle()
            .take(512)
            .enumerate()
        {
            let shifted = ch.is_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(ch);
            s.push_meta(
                event(SoundKind::Typed, 0.0, shifted),
                EventMeta {
                    at_ms: 1_000 + index as u32 * 93,
                    glyph_class: crate::trail_sound::typed_glyph_class(Some(ch)),
                    rank: crate::trail_sound::typed_glyph_rank(Some(ch)),
                    ..EventMeta::default()
                },
            );
            let (slot, born) = s.v2.lead.expect("every key admits its strike");
            let lead = s.voices[usize::from(slot)];
            assert!(
                lead.on && lead.born == born && lead.lane == LANE_TUNE,
                "key {index} ({ch:?}, shifted={shifted}) lost its strike to a decoration"
            );
            full_pool_keys += usize::from(s.voices.iter().all(|voice| voice.on));
        }
        assert!(
            full_pool_keys > 0,
            "the burst must exercise pool exhaustion"
        );
    }

    fn graft_retirement_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            KeyGraftRetirement {
                const Buggy = 0;
                var on = 1;
                var sounded = 0;
                var damped = 0;
                var same = 1;
                var done = 0;
                action Sound when (done == 0 && sounded == 0) { sounded = 1; }
                action Disown when (done == 0 && same == 1) { same = 0; }
                action Retire when (done == 0) {
                    done = 1;
                    on = if same == 1 && sounded == 0 && Buggy == 0 { 0 } else { on };
                    damped = if same == 1 && sounded == 1 && Buggy == 0 { 1 } else { damped };
                }
                invariant OwnedVoiceRetires: done == 0 || same == 0 ||
                    (sounded == 0 && on == 0) || (sounded == 1 && on == 1 && damped == 1);
                invariant UnownedVoiceSurvives: same == 1 || (on == 1 && damped == 0);
                invariant Bounds: on <= 1 && sounded <= 1 && damped <= 1 && same <= 1 && done <= 1;
            }
        }
    }

    #[test]
    fn graft_retirement_model_proves_and_catches_an_omitted_delete() {
        let model = graft_retirement_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    #[test]
    fn typed_grafts_conform_to_owned_retirement_before_and_after_onset() {
        let model = graft_retirement_model();
        let mut negative_controls = 0;
        for deletion in [SoundKind::Backspace, SoundKind::Kill, SoundKind::KillWord] {
            for ch in ['?', '(', ')', '=', 'W'] {
                for sounded in [false, true] {
                    for stale in [false, true] {
                        let mut s = synth();
                        type_line(&mut s, "hello ", 1_000, 150);
                        let _ = render_mono(&mut s, 12);
                        push_ch(&mut s, SoundKind::Typed, 1_900, ch);
                        let (slot, born) = if ch == 'W' { s.v2.ring } else { s.v2.graft }
                            .expect("the typed key produced its pitched decoration");
                        let slot = usize::from(slot);
                        assert!(s.voices[slot].on && s.voices[slot].t < 0.0);
                        let mut expected = model.init_state();
                        let _ = render_mono(&mut s, if sounded { 10 } else { 2 });
                        if sounded {
                            assert!(s.voices[slot].on && s.voices[slot].t >= 0.0);
                            assert!(model.fire("Sound", &mut expected));
                        } else {
                            assert!(s.voices[slot].on && s.voices[slot].t < 0.0);
                        }
                        assert_eq!(s.voices[slot].born, born);
                        assert_eq!(s.voices[slot].damp, 0.0);
                        let before = expected.clone();
                        if stale {
                            assert!(model.fire("Disown", &mut expected));
                            s.v2_retire(Some((slot as u8, born.wrapping_sub(1))), ERASE_MUTE_S);
                        } else {
                            push(&mut s, deletion, 2_000, 0.0, false);
                            assert!(s.v2.ring.is_none() && s.v2.graft.is_none());
                        }
                        assert!(model.fire("Retire", &mut expected));
                        let v = s.voices[slot];
                        let owned_alive = v.on && v.born == born;
                        assert_eq!(i64::from(owned_alive), expected["on"]);
                        assert_eq!(i64::from(owned_alive && v.damp > 0.0), expected["damped"]);
                        assert!(model.check_invariant("OwnedVoiceRetires", &expected));
                        assert!(model.check_invariant("UnownedVoiceSurvives", &expected));
                        if !stale {
                            let mut missing_retire = before;
                            missing_retire.insert("done", 1);
                            assert!(!model.check_invariant("OwnedVoiceRetires", &missing_retire));
                            negative_controls += 1;
                            if !sounded {
                                let _ = render_mono(&mut s, 10);
                                let later = s.voices[slot];
                                assert!(
                                    !later.on || later.born != born,
                                    "{deletion:?} let an unheard {ch:?} graft start later"
                                );
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(negative_controls, 30);
    }

    #[test]
    fn later_text_keys_disown_the_previous_keys_ring_and_graft() {
        let model = graft_retirement_model();
        let mut negative_controls = 0;
        for ch in ['?', 'W'] {
            for sounded in [false, true] {
                for next in [
                    SoundKind::Typed,
                    SoundKind::Space,
                    SoundKind::Enter { cells: 1 },
                    SoundKind::Strum,
                ] {
                    let mut s = synth();
                    type_line(&mut s, "hello ", 1_000, 150);
                    let _ = render_mono(&mut s, 12);
                    push_ch(&mut s, SoundKind::Typed, 1_900, ch);
                    let address = if ch == 'W' { s.v2.ring } else { s.v2.graft }
                        .expect("the earlier key owns a decoration");
                    let (slot, born) = (usize::from(address.0), address.1);
                    let mut expected = model.init_state();
                    if sounded {
                        let _ = render_mono(&mut s, 10);
                        assert!(model.fire("Sound", &mut expected));
                    }
                    assert_eq!(s.voices[slot].t >= 0.0, sounded);
                    assert!(s.voices[slot].on && s.voices[slot].born == born);
                    assert_eq!(s.voices[slot].damp, 0.0);

                    push_ch(&mut s, next, 2_100, 'b');
                    assert!(model.fire("Disown", &mut expected));
                    let owns = s.v2.ring == Some(address) || s.v2.graft == Some(address);
                    assert_eq!(
                        i64::from(owns),
                        expected["same"],
                        "{next:?} kept {ch:?}'s address"
                    );
                    push(&mut s, SoundKind::Backspace, 2_120, 0.0, false);
                    assert!(model.fire("Retire", &mut expected));
                    let v = s.voices[slot];
                    assert_eq!(i64::from(v.on && v.born == born), expected["on"]);
                    assert_eq!(i64::from(v.damp > 0.0), expected["damped"]);

                    let mut wrong_owner = expected;
                    wrong_owner.insert(if sounded { "damped" } else { "on" }, i64::from(sounded));
                    assert!(!model.check_invariant("UnownedVoiceSurvives", &wrong_owner));
                    negative_controls += 1;
                }
            }
        }
        assert_eq!(negative_controls, 16);
    }

    #[test]
    fn every_class_is_deterministic() {
        let script = |text: &str| -> Vec<f32> {
            let mut s = synth();
            let mut out: Vec<f32> = Vec::new();
            let mut at = 1_000u32;
            for ch in text.chars() {
                if ch.is_uppercase() {
                    push(&mut s, SoundKind::Shift, at - 60, 0.0, false);
                }
                let kind = if ch == ' ' {
                    SoundKind::Space
                } else {
                    SoundKind::Typed
                };
                push_ch(&mut s, kind, at, ch);
                out.extend(render_mono(&mut s, 12));
                at += 125;
            }
            out.extend(render_mono(&mut s, 40));
            out
        };
        let a = script(MARK_SENTENCE);
        let b = script(MARK_SENTENCE);
        assert_eq!(a.len(), b.len());
        assert!(
            a.len() >= 3 * SR as usize,
            "the determinism script is only {} s long",
            a.len() as f32 / SR
        );
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            assert!(
                x.to_bits() == y.to_bits(),
                "sample {i} differed between two runs of the class sentence ({x} vs {y})"
            );
        }
        assert!(a.iter().any(|x| x.abs() > 1e-4), "the sentence was silent");
    }

    #[test]
    fn a_doubled_letter_is_a_re_strike_wherever_the_line_stands() {
        const KEYS: &str = "thequickbrownfoxjumpsoverthelazydog";
        let mut checked = 0usize;
        let mut outer = 0usize;
        for n in 1..=KEYS.chars().count() {
            for ch in 'a'..='z' {
                let mut s = synth();
                let mut at = 1_000u32;
                for c in KEYS.chars().take(n) {
                    push_ch(&mut s, SoundKind::Typed, at, c);
                    at += 90;
                }
                push_ch(&mut s, SoundKind::Typed, at, ch);
                let before = s.v2.walk();
                let mark = s.born_seq;
                push_ch(&mut s, SoundKind::Typed, at + 90, ch);
                checked += 1;
                if (i32::from(before) - MELODY_CENTRE_DEG).abs() > MELODY_GRAVITY_DEG {
                    outer += 1;
                }
                assert_eq!(
                    s.v2.walk(),
                    before,
                    "`{ch}{ch}` after {:?} moved the line from {before} to {} — a doubled \
                     letter is a repeat, and gravity may not turn it into a step",
                    &KEYS[..n],
                    s.v2.walk()
                );
                let lead = tune_voices(&since(&s, mark));
                assert_eq!(lead.len(), 1);
                assert_eq!(lead[0].p[2].lvl, 0.0, "a re-strike has no strike partial");
                assert!(s.v2.text.restrike >= 1, "the re-strike ladder did not arm");
            }
        }
        assert!(
            outer > 0,
            "fixture: {checked} doubles checked and none stood where gravity is armed"
        );
    }

    #[test]
    fn a_word_head_common_tone_keeps_the_steps_touch_and_still_damps_the_old_voice() {
        let mut s = synth();
        let mut at = 1_000u32;
        let mut heads_repeated = 0usize;
        let mut steered_repeated = 0usize;
        let mut doubles = 0usize;
        for line in BENCH_PROSE.lines() {
            for word in line.split(' ') {
                if word.is_empty() {
                    continue;
                }
                let mut prev = '\0';
                for (i, ch) in word.chars().enumerate() {
                    let before = s.v2.walk();
                    let old_lead = s.v2.lead;
                    let was_marked = s.v2.token_marked();
                    let mark = s.born_seq;
                    push_ch(&mut s, SoundKind::Typed, at, ch);
                    at += 100;
                    let lead = tune_voices(&since(&s, mark))[0];
                    if s.v2.walk() != before {
                        prev = ch;
                        continue;
                    }
                    let steered = !was_marked && s.v2.token_marked();
                    if let Some((slot, born)) = old_lead {
                        let v = &s.voices[usize::from(slot)];
                        assert!(
                            v.born != born || !v.on || v.damp > 0.0,
                            "a repeated pitch did not damp the voice before it"
                        );
                    }
                    if i == 0 {
                        heads_repeated += 1;
                        assert_eq!(
                            s.v2.text.restrike, 0,
                            "a word head armed the re-strike ladder"
                        );
                        assert!(
                            lead.p[2].lvl > 0.0 && lead.n_lvl == MALLET_LVL,
                            "a word head's common tone lost the step's strike"
                        );
                    } else if steered {
                        steered_repeated += 1;
                        assert_eq!(
                            s.v2.text.restrike, 0,
                            "a cadence armed the re-strike ladder"
                        );
                        assert!(
                            lead.p[0].lvl >= P1_LVL,
                            "a cadence's common tone took the re-strike's levels"
                        );
                    } else {
                        assert_eq!(prev, ch, "an in-word repeat that is not a doubled letter");
                        doubles += 1;
                        assert!(s.v2.text.restrike >= 1);
                        assert_eq!(lead.p[2].lvl, 0.0);
                    }
                    prev = ch;
                }
                push(&mut s, SoundKind::Space, at, 0.0, false);
                at += 100;
            }
            push(&mut s, SoundKind::Jump, at, 0.0, false);
            at += 200;
        }
        assert!(doubles > 0, "fixture: the prose types no doubled letter");
        assert!(
            heads_repeated > 0,
            "fixture: no word head landed on the previous word's last degree"
        );
        assert!(
            steered_repeated > 0,
            "fixture: no mark of the prose cadenced on the degree it stood on"
        );
    }

    #[test]
    fn the_room_answers_line_ends_and_rests_and_the_subject_is_answered_from_above() {
        let taps = |v: &[Voice]| -> Vec<Voice> {
            v.iter()
                .filter(|v| v.lane == LANE_BLOOM && v.delay > BLOOM_DELAY_S + 1e-6)
                .copied()
                .collect()
        };
        let mut s = synth();
        let mut at = 1_000u32;
        for ch in "abcd".chars() {
            let mark = s.born_seq;
            push_ch(&mut s, SoundKind::Typed, at, ch);
            assert!(
                taps(&since(&s, mark)).is_empty(),
                "the room spoke inside a phrase"
            );
            at += 200;
        }
        at += PHRASE_PAUSE_MS;
        let mark = s.born_seq;
        push_ch(&mut s, SoundKind::Typed, at, 'e');
        assert_eq!(
            taps(&since(&s, mark)).len(),
            AIR_TAP_DELAY_S.len(),
            "a rest leaves the room's taps"
        );

        let mut s = synth();
        let mut at = 1_000u32;
        let mut answers = 0usize;
        for (i, ch) in "abcd efg hij klm nop qrs".chars().enumerate() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            let mark = s.born_seq;
            push_ch(&mut s, kind, at, ch);
            at += 120;
            if ch == ' ' {
                continue;
            }
            let spawned = since(&s, mark);
            let lead = tune_voices(&spawned)[0];
            let answer: Vec<&Voice> = spawned
                .iter()
                .filter(|v| v.lane == LANE_BLOOM && (v.delay - ANSWER_DELAY_S).abs() < 1e-6)
                .collect();
            if answer.is_empty() {
                continue;
            }
            answers += 1;
            assert_eq!(answer.len(), 1);
            let a = answer[0];
            assert!(
                s.v2.motif_answering(),
                "an answer with no subject at key {i}"
            );
            assert_eq!(a.n_lvl, 0.0, "the answer has no mallet");
            assert!(
                a.p[0].f0 > lead.p[0].f0 * 1.05,
                "the answer ({} Hz) is not above the head ({} Hz)",
                a.p[0].f0,
                lead.p[0].f0
            );
        }
        assert!(answers >= 1, "the subject was never answered");

        let mut s = synth();
        s.set_v2_timbre_stops(TimbreStops {
            bloom: true,
            hue: true,
            room: false,
        });
        let mut at = 1_000u32;
        let mut extra = 0usize;
        for ch in "abcd efg hij klm nop".chars() {
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            let mark = s.born_seq;
            push_ch(&mut s, kind, at, ch);
            extra += taps(&since(&s, mark)).len();
            at += 1_000;
        }
        assert_eq!(extra, 0, "the room spoke with its stop out");
    }

    #[test]
    fn the_bell_and_the_landing_share_one_clock() {
        for cells in [8u16, 20, 50, 70, 200] {
            let want = timing::flight_ms(f32::from(cells)) * 0.001;
            let mut s = synth();
            push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
            let mark = s.born_seq;
            s.push_meta(
                event(
                    SoundKind::Meteor {
                        dir: 1,
                        cells,
                        armed: false,
                    },
                    0.7,
                    false,
                ),
                EventMeta {
                    at_ms: 1_100,
                    pan_from: -0.7,
                    ..EventMeta::default()
                },
            );
            let bell = since(&s, mark)
                .into_iter()
                .find(|v| v.lane == LANE_TUNE)
                .expect("the meteor must ring a bell");
            assert_eq!(
                bell.delay.to_bits(),
                want.to_bits(),
                "cells {cells}: the bell is at {} s, the pixels land at {want} s",
                bell.delay
            );

            let mut s = synth();
            for k in 0..6u32 {
                push(&mut s, SoundKind::Typed, 1_000 + k * 200, 0.0, false);
            }
            let mark = s.born_seq;
            push(&mut s, SoundKind::Enter { cells }, 2_400, 0.0, false);
            let landed: Vec<Voice> = since(&s, mark)
                .into_iter()
                .filter(|v| v.delay > 0.0 && v.lane != LANE_BLOOM)
                .collect();
            assert_eq!(
                landed.len(),
                3,
                "cells {cells}: the cadence must land three voices on the edge"
            );
            for v in landed {
                assert_eq!(
                    v.delay.to_bits(),
                    want.to_bits(),
                    "cells {cells}: a cadence voice landed at {} s, not {want} s",
                    v.delay
                );
            }
        }
    }

    #[test]
    fn the_core_carries_direction_and_the_whoosh_does_not() {
        let ratios = |dir: i8| -> (f32, f32, f32) {
            let mut s = synth();
            push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
            let mark = s.born_seq;
            s.push_meta(
                event(
                    SoundKind::Meteor {
                        dir,
                        cells: 50,
                        armed: false,
                    },
                    0.8,
                    false,
                ),
                EventMeta {
                    at_ms: 1_100,
                    pan_from: -0.8,
                    ..EventMeta::default()
                },
            );
            let v = since(&s, mark);
            let core = v
                .iter()
                .find(|v| v.lane == LANE_METEOR && v.p[0].lvl > 0.0)
                .expect("core");
            let whoosh = v
                .iter()
                .find(|v| v.lane == LANE_METEOR && v.n_lvl > 0.0 && v.n_f1 > v.n_f0)
                .expect("whoosh");
            (core.p[0].f1 / core.p[0].f0, whoosh.n_f0, whoosh.n_f1)
        };
        let (right, wf0, wf1) = ratios(1);
        let (left, wf0b, wf1b) = ratios(-1);
        assert!(
            right > 1.0 && left < 1.0,
            "core ratios: right {right}, left {left}"
        );
        assert_eq!((wf0, wf1), (wf0b, wf1b), "the whoosh knows the direction");
    }

    #[test]
    fn land_is_silent_under_v2() {
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mark = s.born_seq;
        push(&mut s, SoundKind::Land, 1_100, 0.0, false);
        assert!(
            since(&s, mark).is_empty(),
            "a v2 Land spawned {} voice(s)",
            since(&s, mark).len()
        );
    }

    #[test]
    fn the_cascade_never_stacks() {
        let mut s = synth();
        let mark = s.born_seq;
        push(&mut s, SoundKind::Jump, 1_000, -0.5, false);
        assert_eq!(
            since(&s, mark).len(),
            4,
            "one Jump is the four-note brrrring"
        );

        let mut s = synth();
        let mut cascades = 0;
        let mut onsets = 0;
        let mut restrikes: Vec<f32> = Vec::new();
        for k in 0..12u32 {
            let mark = s.born_seq;
            push(&mut s, SoundKind::Jump, 1_000 + k * 60, -0.5, false);
            let born = since(&s, mark);
            onsets += born.len();
            match born.len() {
                4 => cascades += 1,
                1 => restrikes.push(born[0].p[0].f0),
                _ => {}
            }
            let mut buf = [0.0f32; 960];
            for _ in 0..6 {
                s.render(&mut buf);
            }
        }
        assert_eq!(
            cascades, 1,
            "12 line feeds 60 ms apart minted {cascades} four-note cascades; \
             a run gets one"
        );
        assert!(
            onsets <= 16,
            "12 line feeds produced {onsets} onsets; the cascade stacked"
        );
        assert!(
            restrikes.len() >= 2 && restrikes.windows(2).any(|w| w[0] != w[1]),
            "the re-strikes of a run are one pitch: {restrikes:?}"
        );

        let mut s = synth();
        for k in 0..5u32 {
            push(&mut s, SoundKind::Typed, 1_000 + k * 200, 0.0, false);
        }
        let mark = s.born_seq;
        push(&mut s, SoundKind::Enter { cells: 30 }, 2_000, 0.0, false);
        let after_enter = s.v2.walk();
        push(&mut s, SoundKind::Jump, 2_020, 0.0, false);
        let born = since(&s, mark);
        assert!(
            born.iter().all(|v| v.lane != LANE_CASCADE),
            "the keyed Return's own line-feed echo minted a cascade"
        );
        assert_eq!(
            s.v2.walk(),
            after_enter,
            "the swallowed echo moved the line"
        );
        let mark = s.born_seq;
        push(&mut s, SoundKind::Jump, 2_400, 0.0, false);
        assert_eq!(
            since(&s, mark).len(),
            4,
            "a line feed past the echo window is a PTY cascade again"
        );

        let mut s = synth();
        let mut buf = [0.0f32; 960];
        let mut heads = Vec::new();
        for k in 0..10u32 {
            let mark = s.born_seq;
            push(&mut s, SoundKind::Jump, 1_000 + k * 200, -0.9, false);
            let born = since(&s, mark);
            assert_eq!(born.len(), 4, "line feed {k} of the run: one cascade");
            heads.push(born[0].p[0].f0);
            for _ in 0..20 {
                s.render(&mut buf);
            }
        }
        heads.sort_by(f32::total_cmp);
        heads.dedup();
        assert!(
            heads.len() >= 3,
            "a line-feed run looped one figure: {heads:?}"
        );
    }

    #[test]
    fn the_space_walks_pure_fifths_and_enter_comes_home() {
        let mut s = synth();
        for w in 0..8u32 {
            push(&mut s, SoundKind::Typed, 1_000 + w * 400, 0.0, false);
            let mark = s.born_seq;
            push(&mut s, SoundKind::Space, 1_200 + w * 400, 0.0, false);
            let bass = since(&s, mark)
                .into_iter()
                .find(|v| v.lane == LANE_BASS)
                .expect("a word head is a downbeat");
            let (root, fifth) = (bass.p[0].f0, bass.p[1].f0);
            let ratio = fifth / root;
            assert!(
                (ratio - 1.5).abs() < 1e-4 || (ratio - 0.75).abs() < 1e-4,
                "dyad ratio {ratio} is neither a pure fifth above nor a pure \
                 fourth below"
            );
            assert!(
                (261.0..=437.0).contains(&root) && (261.0..=437.0).contains(&fifth),
                "the dyad ({root} + {fifth}) left the BASS lane"
            );
        }
        let mut s2 = synth();
        push(&mut s2, SoundKind::Typed, 1_000, 0.0, false);
        push(&mut s2, SoundKind::Enter { cells: 40 }, 1_200, 0.0, false);
        let mark = s2.born_seq;
        push(&mut s2, SoundKind::Typed, 1_600, 0.0, false);
        push(&mut s2, SoundKind::Space, 1_800, 0.0, false);
        let bass = since(&s2, mark)
            .into_iter()
            .find(|v| v.lane == LANE_BASS && v.delay == 0.0)
            .expect("the first word of a new line is a downbeat");
        assert!(
            (bass.p[0].f0 - BASS_BASE_HZ).abs() < 1e-3
                && (bass.p[1].f0 / bass.p[0].f0 - 1.5).abs() < 1e-4,
            "after an Enter the first Space must play I (C4 + G4), got {} + {}",
            bass.p[0].f0,
            bass.p[1].f0
        );
    }

    #[test]
    fn a_run_of_spaces_is_one_downbeat_and_a_rising_figure() {
        let mag = |v: &Voice| (v.gl * v.gl + v.gr * v.gr).sqrt();
        let mut s = synth();
        type_line(&mut s, "hello", 1_000, 150);
        let mark = s.born_seq;
        push(&mut s, SoundKind::Space, 2_000, 0.0, false);
        let head = since(&s, mark);
        let bass = head
            .iter()
            .find(|v| v.lane == LANE_BASS)
            .expect("the run's head is the downbeat");
        assert!(
            head.iter().all(|v| v.lane != LANE_GLINT),
            "the head is the downbeat, not a step"
        );
        let (root, head_gain) = (bass.p[0].f0, mag(bass));
        let chord_idx = s.v2.text.chord;
        let walk = s.v2.walk();
        let mut last = root;
        for n in 1..=3u32 {
            let mark = s.born_seq;
            push(&mut s, SoundKind::Space, 2_000 + n * 100, 0.0, false);
            let v = since(&s, mark);
            assert!(
                v.iter().all(|v| v.lane != LANE_BASS),
                "space {n} of the run: a second downbeat"
            );
            assert!(
                v.iter().any(|v| v.lane == LANE_BREATH),
                "space {n} of the run: the breath is still under the step"
            );
            let steps: Vec<&Voice> = v.iter().filter(|v| v.lane == LANE_GLINT).collect();
            assert_eq!(
                steps.len(),
                1,
                "space {n} of the run: exactly one indent step"
            );
            let step = steps[0];
            let f = step.p[0].f0;
            assert!(
                f > last * 1.01,
                "space {n}: the figure must RISE ({last} -> {f} Hz)"
            );
            assert!(
                mag(step) < head_gain,
                "space {n}: a step ({}) may not be louder than the downbeat ({head_gain})",
                mag(step)
            );
            last = f;
        }
        let mark = s.born_seq;
        push(&mut s, SoundKind::Space, 2_340, 0.0, false);
        assert!(
            since(&s, mark).iter().all(|v| v.lane != LANE_GLINT),
            "a space inside SPACE_STEP_MIN_GAP_MS of the last step is breath alone"
        );
        assert_eq!(s.v2.text.chord, chord_idx, "the tail moved the chord");
        assert_eq!(s.v2.walk(), walk, "the tail moved the walk");

        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mut buf = [0.0f32; 3_168];
        let (mut heard, mut first_second) = (0usize, 0usize);
        let glints_before = s.lane_births[usize::from(LANE_GLINT)];
        for k in 0..96u32 {
            let mark = s.born_seq;
            push(&mut s, SoundKind::Space, 1_200 + k * 33, 0.0, false);
            s.render(&mut buf);
            let steps: Vec<Voice> = since(&s, mark)
                .into_iter()
                .filter(|v| v.lane == LANE_GLINT)
                .collect();
            assert!(
                steps.len() <= 1,
                "repeat {k}: {} steps at once",
                steps.len()
            );
            if !steps.is_empty() {
                heard += 1;
                first_second += usize::from(k < 30);
            }
        }
        assert_eq!(
            (s.lane_births[usize::from(LANE_GLINT)] - glints_before) as usize,
            heard,
            "every LANE_GLINT birth of a held spacebar is an indent step"
        );
        assert!(
            (4..=13).contains(&first_second),
            "a held spacebar minted {first_second} steps/s — the gate admits one per 75+ ms"
        );
        assert!(
            heard >= 30,
            "3.2 s of auto-repeat minted only {heard} steps"
        );
    }

    #[test]
    fn the_indent_figure_is_a_cycle_on_every_chord_and_key() {
        for (ci, chord) in CHORD_LOOP.iter().enumerate() {
            for key in -3..=4i32 {
                let root = penta(BASS_BASE_HZ * CHORD_ROOT_RATIO[chord.root], key);
                let f: Vec<f32> = (1..=300u32)
                    .map(|n| indent_step_hz(ci, root, n, key))
                    .collect();
                let cycle = f
                    .iter()
                    .skip(1)
                    .position(|x| (x - f[0]).abs() < SAME_PITCH_HZ)
                    .map(|i| i + 1)
                    .unwrap_or_else(|| panic!("chord {ci} key {key}: no cycle in {f:?}"));
                for (i, x) in f.iter().enumerate() {
                    let n = i + 1;
                    assert!(
                        *x < SPACE_STEP_HI_HZ && *x > root,
                        "chord {ci} key {key} step {n}: {x} Hz is not in ({root}, {SPACE_STEP_HI_HZ})"
                    );
                    if key == 0 {
                        assert!(
                            (BASS_BASE_HZ..SPACE_STEP_HI_HZ).contains(x),
                            "chord {ci} step {n}: {x} Hz left the §9.4 band"
                        );
                    }
                    assert!(
                        (-20i32..40).any(|deg| {
                            chord.lit & (1 << deg.rem_euclid(5)) != 0
                                && (penta(BASS_BASE_HZ, deg + key) - x).abs() < SAME_PITCH_HZ
                        }),
                        "chord {ci} key {key} step {n}: {x} Hz is not a lit degree"
                    );
                    assert!(
                        (x - f[i % cycle]).abs() < SAME_PITCH_HZ,
                        "chord {ci} key {key} step {n}: {x} Hz breaks the {cycle}-note cycle"
                    );
                    if i % cycle > 0 {
                        assert!(
                            *x > f[i - 1] * 1.01,
                            "chord {ci} key {key} step {n}: {} -> {x} does not rise",
                            f[i - 1]
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_v1_event_leaves_every_v2_delta_at_its_identity() {
        for style in [
            GlowStyle::Lumen,
            GlowStyle::Sparkle,
            GlowStyle::Comet,
            GlowStyle::Fire,
        ] {
            for kind in [
                SoundKind::Typed,
                SoundKind::Backspace,
                SoundKind::Jump,
                SoundKind::Kill,
                SoundKind::Space,
                SoundKind::Land,
            ] {
                let mut s = TrailSynth::new(SR, SEED);
                let mut ev = event(kind, 0.3, false);
                ev.style = style;
                s.push(ev);
                for v in s.voices.iter().filter(|v| v.on) {
                    assert_eq!(v.lane, LANE_NONE, "{style:?}/{kind:?}: laned");
                    assert_eq!(v.n_decay, 0.0, "{style:?}/{kind:?}: n_decay");
                    assert_eq!(v.pan_glide_s, 0.0, "{style:?}/{kind:?}: pan glide");
                    assert_eq!(v.arm_ttl, 0.0, "{style:?}/{kind:?}: arm ttl");
                    assert_eq!(v.gl1, v.gl, "{style:?}/{kind:?}: gl1");
                    assert_eq!(v.gr1, v.gr, "{style:?}/{kind:?}: gr1");
                    for p in &v.p {
                        assert_eq!(p.decay, 0.0, "{style:?}/{kind:?}: partial decay");
                    }
                }
            }
        }
    }

    #[test]
    fn the_lanes_hold_their_caps_and_steals_are_zero() {
        let mut s = synth();
        for k in 0..200u32 {
            let at = 1_000 + k * 40;
            match k % 10 {
                9 => push(&mut s, SoundKind::Space, at, 0.0, false),
                7 => push(
                    &mut s,
                    SoundKind::Stardust { twinkle_hz: 9 },
                    at,
                    0.2,
                    false,
                ),
                5 => s.push_meta(
                    event(
                        SoundKind::Meteor {
                            dir: -1,
                            cells: 60,
                            armed: false,
                        },
                        0.5,
                        false,
                    ),
                    EventMeta {
                        at_ms: at,
                        pan_from: -0.5,
                        ..EventMeta::default()
                    },
                ),
                _ => push(&mut s, SoundKind::Typed, at, 0.1, k % 4 == 0),
            }
            let mut buf = [0.0f32; 960];
            for _ in 0..4 {
                s.render(&mut buf);
            }
            assert_caps(&s);
        }
        assert_eq!(s.steals(), 0, "the 28-voice pool ran dry under v2");

        let mut s = synth();
        let mut buf = [0.0f32; 960];
        let mut now = 1_000u32;
        let mut run_to = |s: &mut TrailSynth, at: u32| {
            while now < at {
                s.render(&mut buf);
                assert_caps(s);
                now += 10;
            }
        };
        let mut at = 1_000;
        for k in 0..6u32 {
            run_to(&mut s, at);
            push(&mut s, SoundKind::Typed, at, 0.1, k % 2 == 0);
            at += 100;
        }
        run_to(&mut s, at);
        push(&mut s, SoundKind::Space, at, 0.1, false);
        run_to(&mut s, at + 10);
        push(
            &mut s,
            SoundKind::Stardust { twinkle_hz: 9 },
            at + 10,
            0.2,
            false,
        );
        run_to(&mut s, at + 20);
        meteor(&mut s, at + 20, 60, false);
        run_to(&mut s, at + 60);
        meteor(&mut s, at + 60, 60, false);
        run_to(&mut s, at + 70);
        push(&mut s, SoundKind::Typed, at + 70, 0.3, true);
        run_to(&mut s, at + 80);
        push(
            &mut s,
            SoundKind::Stardust { twinkle_hz: 12 },
            at + 80,
            0.3,
            false,
        );
        run_to(&mut s, at + 90);
        push(&mut s, SoundKind::Enter { cells: 40 }, at + 90, 0.3, false);
        run_to(&mut s, at + 170);
        push(&mut s, SoundKind::Typed, at + 170, 0.0, true);
        run_to(&mut s, at + 270);
        push(&mut s, SoundKind::Typed, at + 270, 0.0, false);
        run_to(&mut s, at + 370);
        push(&mut s, SoundKind::Space, at + 370, 0.0, false);
        run_to(&mut s, at + 800);
        assert_eq!(s.steals(), 0, "§14's worst case ran the 28-voice pool dry");
    }

    fn assert_caps(s: &TrailSynth) {
        for lane in [
            LANE_TUNE,
            LANE_BLOOM,
            LANE_BASS,
            LANE_BREATH,
            LANE_GLINT,
            LANE_METEOR,
            LANE_RAIN,
            LANE_CADENCE,
            LANE_CASCADE,
            LANE_GRAFT,
            LANE_TING,
        ] {
            let live = s
                .voices
                .iter()
                .filter(|v| v.on && v.lane == lane && v.damp <= 0.0 && v.t >= 0.0)
                .count();
            assert!(
                live <= lane_cap(lane),
                "lane {lane} held {live} voices, over its cap of {}",
                lane_cap(lane)
            );
        }
        for v in s.voices.iter().filter(|v| {
            v.on && lane_drops_the_newcomer(v.lane) && v.damp > 0.0 && v.damp0 == LANE_FADE_STEAL_S
        }) {
            let age = v.t - (v.damp0 - v.damp);
            assert!(
                !(0.0..LANE_AGE_GUARD_S - 1e-4).contains(&age),
                "lane {} fade-stole a voice {age} s old — under the 40 ms age guard",
                v.lane
            );
        }
    }

    #[test]
    fn a_hero_star_is_a_pitched_glint_at_its_own_stars_rate() {
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        for (i, rate) in [8u8, 11, 12].iter().enumerate() {
            let mark = s.born_seq;
            push(
                &mut s,
                SoundKind::Stardust { twinkle_hz: *rate },
                1_100 + i as u32 * 300,
                0.4,
                false,
            );
            let g = since(&s, mark);
            assert_eq!(g.len(), 1, "a hero is exactly one glint");
            let g = g[0];
            assert!(g.p[1].lvl == 0.0 && g.p[2].lvl == 0.0, "one partial only");
            assert!(
                (GLINT_LO_HZ..GLINT_HI_HZ).contains(&g.p[0].f0),
                "the glint at {} Hz left the STARDUST lane",
                g.p[0].f0
            );
            assert_eq!(
                g.tw_rate,
                f32::from(*rate),
                "the glint took its own star's rate"
            );
            let mut buf = [0.0f32; 960];
            for _ in 0..14 {
                s.render(&mut buf);
            }
        }
    }

    #[test]
    fn a_held_backspace_unsings_one_note_per_repeat() {
        let mut s = synth();
        let mark = s.born_seq;
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let first_f0: Vec<f32> = tune_voices(&since(&s, mark))
            .iter()
            .map(|v| v.p[0].f0)
            .collect();
        let first = s.v2.walk();
        push(&mut s, SoundKind::Typed, 1_300, 0.0, false);
        push(&mut s, SoundKind::Typed, 1_600, 0.0, false);
        assert_ne!(s.v2.walk(), first, "three steps must have moved the line");
        let mut buf = [0.0f32; 960];
        let mut poofs = 0;
        for k in 0..3u32 {
            let mark = s.born_seq;
            push(&mut s, SoundKind::Backspace, 1_900 + k * 33, 0.0, false);
            if k == 0 {
                assert!(
                    s.voices.iter().any(|v| v.on
                        && v.lane == LANE_TUNE
                        && (v.damp - ERASE_MUTE_S).abs() < 1e-6),
                    "the erased letter's note was not muted by the erase"
                );
            }
            if since(&s, mark).iter().any(|v| v.lane == LANE_NONE) {
                poofs += 1;
            }
            for _ in 0..3 {
                s.render(&mut buf);
            }
        }
        assert_eq!(
            poofs, 1,
            "the erase gate thins the poof to one per 75 ms; it spoke {poofs} times"
        );
        assert_eq!(
            s.v2.walk(),
            first,
            "the line did not rewind one note per repeat"
        );
        let mark = s.born_seq;
        push(&mut s, SoundKind::Typed, 2_300, 0.0, false);
        let retyped: Vec<f32> = tune_voices(&since(&s, mark))
            .iter()
            .map(|v| v.p[0].f0)
            .collect();
        assert_eq!(
            retyped.len(),
            1,
            "the retyped letter spawned {} tune voices, not one",
            retyped.len()
        );
        assert!(
            retyped[0] > 0.0 && !first_f0.is_empty(),
            "the retyped letter made no pitched sound"
        );
    }

    fn render_all(s: &mut TrailSynth, blocks: usize) -> Vec<f32> {
        let mut buf = [0.0f32; 960];
        let mut out = Vec::with_capacity(blocks * 960);
        for _ in 0..blocks {
            s.render(&mut buf);
            out.extend_from_slice(&buf);
        }
        out
    }

    fn peak_of(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }

    #[test]
    fn the_limiter_is_transparent_below_threshold_and_holds_the_ceiling_above() {
        let mut a = synth();
        push(&mut a, SoundKind::Typed, 1_000, 0.0, false);
        assert!(a.v2_latched, "the first v2 trail event latches the bus");
        let mut b = synth();
        push(&mut b, SoundKind::Typed, 1_000, 0.0, false);
        b.v2_latched = false;
        let ya = render_all(&mut a, 40);
        let yb = render_all(&mut b, 40);
        let peak = peak_of(&yb);
        assert!(
            peak > 0.05 && peak < LIMIT_THRESHOLD,
            "the probe must sound under the threshold (peak {peak})"
        );
        assert!(
            ya.iter().zip(&yb).all(|(p, q)| p.to_bits() == q.to_bits()),
            "below the threshold the latched bus moved a bit"
        );
        assert_eq!(
            (a.lim_g_l, a.lim_g_r),
            (1.0, 1.0),
            "the gain left 1.0 under the threshold"
        );

        let k_att = 1.0 - (-1.0 / (SR * LIMIT_ATTACK_S)).exp();
        let k_rel = (-1.0 / (SR * LIMIT_RELEASE_S)).exp();
        let minus_15_dbfs = 10f32.powf(-15.0 / 20.0);
        for y in [minus_15_dbfs, -minus_15_dbfs, LIMIT_THRESHOLD] {
            let (mut env, mut g) = (0.0f32, 1.0f32);
            let out = limit(y, &mut env, &mut g, k_att, k_rel);
            assert_eq!(out.to_bits(), y.to_bits(), "a soft knee touched {y}");
            assert_eq!(g, 1.0, "the gain left 1.0 at {y}");
        }
        let over = LIMIT_THRESHOLD * 1.001;
        let (mut env, mut g) = (0.0f32, 1.0f32);
        let out = limit(over, &mut env, &mut g, k_att, k_rel);
        assert!(
            g < 1.0 && out < over,
            "one part in a thousand over the threshold the gain stayed at {g} — the knee is not hard"
        );

        let loud = |latched: bool| -> (TrailSynth, Vec<f32>) {
            let mut s = synth();
            let mut ev = event(
                SoundKind::Meteor {
                    dir: 1,
                    cells: 50,
                    armed: false,
                },
                0.5,
                false,
            );
            ev.gain = 1.0;
            s.push_meta(
                ev,
                EventMeta {
                    at_ms: 1_000,
                    pan_from: -0.5,
                    ..EventMeta::default()
                },
            );
            s.v2_latched = latched;
            let y = render_all(&mut s, 60);
            (s, y)
        };
        let (_, raw) = loud(false);
        let (mut s, held) = loud(true);
        let raw_peak = peak_of(&raw);
        let held_peak = peak_of(&held);
        assert!(
            raw_peak > LIMIT_THRESHOLD * 1.25,
            "the unlimited meteor must clear the threshold (peak {raw_peak})"
        );
        let over = |y: &[f32]| {
            y.iter()
                .filter(|x| x.abs() > LIMIT_THRESHOLD * 1.02)
                .count()
        };
        let (raw_over, held_over) = (over(&raw), over(&held));
        assert!(
            held_over <= 2 * 384 && raw_over > 4 * held_over,
            "the limiter left {held_over} samples over the ceiling (unlimited: {raw_over})"
        );
        assert!(
            held_peak <= LIMIT_THRESHOLD * 1.3 && held_peak < raw_peak,
            "the limited meteor peaked at {held_peak} (unlimited {raw_peak}) — the attack is not 0.5 ms"
        );
        assert!(
            held_peak > LIMIT_THRESHOLD * 0.9,
            "the limiter pumped the meteor down to {held_peak}, far under the ceiling"
        );

        let tail = render_all(&mut s, 200);
        assert!(s.is_quiet(), "the synth did not settle to quiet");
        assert!(
            tail[tail.len() - 960..].iter().all(|x| *x == 0.0),
            "idle is not exact silence"
        );
        assert_eq!(
            (s.lim_g_l, s.lim_g_r),
            (1.0, 1.0),
            "the limiter did not reset on silence"
        );
    }

    #[test]
    fn the_fallback_clock_wraps_like_a_host_stamp_instead_of_saturating() {
        fn key(s: &mut TrailSynth) {
            s.push(event(SoundKind::Typed, 0.0, false));
        }
        fn render_ms(s: &mut TrailSynth, ms: usize) {
            let mut buf = [0.0f32; 960];
            for _ in 0..ms / 10 {
                s.render(&mut buf);
            }
        }
        const WRAP_S: f64 = 4_294_967.296;

        let mut s = synth();
        s.clock_s = WRAP_S + 0.204;
        key(&mut s);
        render_ms(&mut s, 300);
        key(&mut s);
        assert_eq!(
            s.v2.steps(),
            2,
            "past 49.7 days the second key must still step — the clock saturated ({})",
            s.v2.steps()
        );

        let mut s = synth();
        s.clock_s = WRAP_S - 0.296;
        key(&mut s);
        assert_eq!(s.v2.steps(), 1, "the first key steps");
        render_ms(&mut s, 300);
        key(&mut s);
        assert_eq!(
            s.v2.steps(),
            2,
            "the wrapping key must STILL step — a wrapped clock may cost the \
             contour its gap, and may never cost the key its note"
        );
        render_ms(&mut s, 1_000);
        key(&mut s);
        render_ms(&mut s, 300);
        key(&mut s);
        assert_eq!(s.v2.steps(), 4, "healed, the line must walk on");
        assert!(
            s.v2.ioi_ms() > 0.0 && s.v2.ioi_ms().is_finite(),
            "the wrap left the IOI estimator in a state the arithmetic cannot use"
        );
    }

    fn arm(s: &mut TrailSynth, at: u32) {
        s.push_meta(
            event(SoundKind::MeteorArm { dir: 1 }, 0.0, false),
            EventMeta {
                at_ms: at,
                pan_from: -0.6,
                ..EventMeta::default()
            },
        );
    }

    fn meteor(s: &mut TrailSynth, at: u32, cells: u16, armed: bool) {
        s.push_meta(
            event(
                SoundKind::Meteor {
                    dir: 1,
                    cells,
                    armed,
                },
                0.6,
                false,
            ),
            EventMeta {
                at_ms: at,
                pan_from: -0.6,
                ..EventMeta::default()
            },
        );
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn a_meteor_fizzles_claims_without_a_click_and_never_eats_a_sounded_bell() {
        let mut buf = [0.0f32; 960];

        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mark = s.born_seq;
        arm(&mut s, 1_100);
        let armed = since(&s, mark);
        assert_eq!(
            armed.len(),
            2,
            "an arm is the tick and the core, nothing else"
        );
        assert!(
            armed.iter().all(|v| v.arm_ttl > 0.0),
            "both armed voices carry the ttl"
        );
        for _ in 0..16 {
            s.render(&mut buf);
        }
        let core = s
            .voices
            .iter()
            .find(|v| v.on && v.lane == LANE_METEOR && v.p[0].lvl > 0.0)
            .expect("at 160 ms the armed core is still on its 12 ms fizzle ramp");
        assert!(
            core.arm_ttl == 0.0 && core.damp > 0.0,
            "an unclaimed arm must fizzle on a ramp at its ttl"
        );

        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        arm(&mut s, 1_100);
        for _ in 0..2 {
            s.render(&mut buf);
        }
        let mark = s.born_seq;
        meteor(&mut s, 1_120, 5, true);
        let born = since(&s, mark);
        assert_eq!(born.len(), 1, "a sub-floor hop is the nav tick alone");
        assert!(
            born[0].lane == LANE_TUNE && born[0].n_lvl == 0.0 && born[0].p[0].lvl > 0.0,
            "the hop's voice is the verse note, P1 only"
        );
        assert!(
            s.voices
                .iter()
                .filter(|v| v.on && v.lane == LANE_METEOR)
                .all(|v| v.damp > 0.0 && v.arm_ttl == 0.0),
            "the fizzled arm was not damped"
        );

        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        arm(&mut s, 1_100);
        let before = render_all(&mut s, 3);
        let mark = s.born_seq;
        meteor(&mut s, 1_130, 50, true);
        let after = render_all(&mut s, 1);
        let t = timing::flight_ms(50.0) * 0.001;
        let cores: Vec<&Voice> = s
            .voices
            .iter()
            .filter(|v| v.on && v.lane == LANE_METEOR && v.p[0].lvl > 0.0 && v.damp <= 0.0)
            .collect();
        assert_eq!(cores.len(), 1, "a claim re-targets — never two cores");
        assert!(
            cores[0].arm_ttl == 0.0 && cores[0].pan_glide_s == t && cores[0].born <= mark,
            "the claimed core is the ARMED voice, re-aimed over the true flight"
        );
        assert!(
            since(&s, mark)
                .iter()
                .all(|v| v.p[0].lvl <= 0.0 || v.delay > 0.0),
            "the claim spawned a second sounding tonal voice"
        );
        for ch in 0..2 {
            let pre: Vec<f32> = before.iter().skip(ch).step_by(2).copied().collect();
            let post: Vec<f32> = after.iter().skip(ch).step_by(2).copied().collect();
            let slope = pre[pre.len() - 960..]
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max);
            let jump = (post[0] - pre[pre.len() - 1]).abs();
            assert!(
                jump <= slope * 1.5 + 1e-6,
                "channel {ch}: the claim stepped the bus by {jump} against a pre-claim slope of {slope}"
            );
            let (r0, r1) = (rms(&pre[pre.len() - 48..]), rms(&post[..48]));
            assert!(
                (0.8..=1.25).contains(&(r1 / r0)),
                "channel {ch}: the level jumped across the claim — 1 ms RMS {r0} → {r1}"
            );
        }

        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mark = s.born_seq;
        meteor(&mut s, 1_100, 50, false);
        let first = since(&s, mark);
        let (first_bell_slot, first_bell_born) = s.v2.bell.expect("the first meteor rang a bell");
        for _ in 0..4 {
            s.render(&mut buf);
        }
        let mark = s.born_seq;
        meteor(&mut s, 1_140, 50, false);
        for was in &first {
            let Some(v) = s.voices.iter().find(|v| v.born == was.born) else {
                continue;
            };
            if was.lane == LANE_TUNE || was.lane == LANE_BASS || was.lane == LANE_RAIN {
                assert!(
                    !v.on && v.damp == 0.0,
                    "an UNSOUNDED voice of the first meteor (lane {}) was damped, not cancelled",
                    was.lane
                );
            } else if v.on {
                assert!(
                    v.damp > 0.0,
                    "a sounding layer of the first meteor was not damped"
                );
            }
        }
        let first_bell = &s.voices[usize::from(first_bell_slot)];
        assert!(
            !(first_bell.on && first_bell.born == first_bell_born),
            "the first meteor's unsounded bell survived the second meteor"
        );
        let (slot, born) = s.v2.bell.expect("the second meteor rang a bell");
        assert!(
            since(&s, mark)
                .iter()
                .any(|v| v.born == born && v.lane == LANE_TUNE),
            "the second bell is the second meteor's own"
        );
        for _ in 0..15 {
            s.render(&mut buf);
        }
        let bell = &s.voices[usize::from(slot)];
        assert!(
            bell.on && bell.born == born && bell.t >= 0.0 && bell.damp == 0.0,
            "at T + 50 the second bell must be sounding and undamped"
        );
    }

    #[test]
    fn a_sing_along_never_machine_guns_and_hands_the_key_back_at_a_phrase_boundary() {
        let mut s = synth();
        push(&mut s, SoundKind::Typed, 1_000, 0.0, false);
        let mut riff = event(SoundKind::Typed, 0.0, false);
        riff.kind = SoundGesture::Celebration(CelebrationGesture::RiffBar { bar: 0, sig: 4 });
        s.push(riff);
        let key = s.song_key;
        assert!(
            key != 0 && s.sing > 0.0,
            "the riff must borrow a key and arm the sing duck"
        );

        let mut buf = [0.0f32; 960];
        let (mut pitched, mut mallets, mut heads, mut keys) = (0usize, 0usize, 0usize, 0usize);
        const BURST: &str = "the cat sings so the hand keeps time under it and never over it x";
        let mut head_next = false;
        for (k, ch) in BURST.chars().take(60).enumerate() {
            let mark = s.born_seq;
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            push_ch(&mut s, kind, 1_100 + k as u32 * 33, ch);
            if ch != ' ' {
                keys += 1;
                if head_next {
                    heads += 1;
                }
                head_next = false;
                for v in tune_voices(&since(&s, mark)) {
                    if v.p[0].lvl > 0.0 {
                        pitched += 1;
                    } else {
                        mallets += 1;
                    }
                }
            } else {
                head_next = true;
            }
            for _ in 0..3 {
                s.render(&mut buf);
            }
        }
        assert_eq!(
            pitched + mallets,
            keys,
            "{keys} keys under the riff produced {} tune onsets — a key went \
             silent, which the sing duck may never do",
            pitched + mallets
        );
        assert!(
            s.sing > 0.0,
            "the riff must outlive the burst for the law to be tested"
        );
        assert!(
            heads >= 8,
            "fixture: the burst must contain words ({heads})"
        );
        let doubled = BURST
            .chars()
            .take(60)
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|w| w[0] == w[1] && w[0] != ' ')
            .count();
        assert!(doubled > 0, "fixture: the burst must type a doubled letter");
        assert_eq!(
            mallets, doubled,
            "{mallets} keys went to the mallet under the riff for {doubled} doubled \
             letters — a riff may take the tremolo off a repeat and nothing else"
        );
        assert_eq!(
            pitched,
            keys - doubled,
            "{pitched} pitched TUNE onsets under the riff for {keys} keys — the riff \
             muted the typist's own line"
        );
        assert_eq!(
            s.song_key, key,
            "the key was released while the song was live"
        );

        let mut blocks = 0;
        while s.sing > 0.0 && blocks < 800 {
            s.render(&mut buf);
            blocks += 1;
        }
        assert_eq!(s.sing, 0.0, "the riff must have died");
        assert_eq!(
            s.song_key, key,
            "the key was SNAPPED when the riff died — §10.2 hands it back at the boundary"
        );
        assert!(s.v2_key_pending, "the handback must be pending");

        let mut changes = Vec::new();
        let mut at = 3_200u32;
        for (k, ch) in "in the middle of a word the key is held and never snapped away"
            .chars()
            .enumerate()
        {
            let pos = s.v2.word_pos();
            let before = s.song_key;
            let kind = if ch == ' ' {
                SoundKind::Space
            } else {
                SoundKind::Typed
            };
            push_ch(&mut s, kind, at, ch);
            at += 250 + (k as u32 % 3) * 7;
            if s.song_key != before {
                changes.push((pos, s.song_key));
            }
        }
        assert_eq!(
            changes.len(),
            1,
            "song_key changed {} times after the riff; once, at the boundary",
            changes.len()
        );
        let (pos, to) = changes[0];
        assert_eq!(to, 0, "the handback goes to the neutral lattice");
        assert_eq!(
            pos, 0,
            "the key was handed back MID-WORD, at word_pos {pos}"
        );
        assert!(!s.v2_key_pending, "the handback must clear itself");
    }

    #[test]
    fn a_v1_voice_after_the_music_box_snaps_a_pending_key_instead_of_holding_it_for_ever() {
        let mut s = TrailSynth::new(SR, SEED);
        let mut music_box = event(SoundKind::Typed, 0.0, false);
        music_box.voice = SoundVoice::RainbowKittyV2;
        s.push_meta(
            music_box,
            EventMeta {
                at_ms: 1_000,
                ..EventMeta::default()
            },
        );
        assert!(s.v2_latched, "the music box latches the seam by voice");
        let mut riff = event(SoundKind::Typed, 0.0, false);
        riff.kind = SoundGesture::Celebration(CelebrationGesture::RiffBar { bar: 0, sig: 4 });
        s.push(riff);
        let key = s.song_key;
        assert!(key != 0 && s.sing > 0.0, "the riff must borrow a key");

        let mut buf = [0.0f32; 960];
        let mut blocks = 0;
        while s.sing > 0.0 && blocks < 800 {
            s.render(&mut buf);
            blocks += 1;
        }
        assert_eq!(s.sing, 0.0, "the riff must have died");
        assert!(
            s.v2_key_pending && s.song_key == key,
            "the key is held pending the boundary (fixture precondition)"
        );

        let mut bonk = event(SoundKind::Typed, 0.0, false);
        bonk.kind = SoundGesture::Words(WordGesture::Bonk);
        s.push(bonk);
        assert!(
            s.v2_key_pending && s.song_key == key,
            "a bonk snapped a key the music box was holding for its boundary"
        );

        let mut marimba = event(SoundKind::Typed, 0.0, false);
        marimba.style = GlowStyle::Sparkle;
        marimba.voice = SoundVoice::Marimba;
        s.push(marimba);
        assert_eq!(
            s.song_key, 0,
            "the v1 voice's typed register stayed transposed: the handback never came"
        );
        assert!(
            !s.v2_key_pending,
            "the snap must clear the pending handback"
        );
    }

    fn assert_tails(voices: &[Voice], what: &str) -> Vec<u8> {
        let mut lanes = Vec::new();
        for v in voices {
            if v.lane == LANE_NONE {
                continue;
            }
            let tail = (-v.dur / v.decay).exp();
            assert!(
                tail <= 0.07,
                "{what}: a lane-{} voice (dur {} s, decay {} s) ends at {:.1} % of peak — over A13's 7 %",
                v.lane,
                v.dur,
                v.decay,
                tail * 100.0
            );
            lanes.push(v.lane);
        }
        lanes
    }

    #[test]
    fn tails_end_quietly_and_damps_are_ramps() {
        let mut s = synth();
        let mut buf = [0.0f32; 960];
        let mut seen: Vec<u8> = Vec::new();
        let script: [(SoundKind, bool); 15] = [
            (SoundKind::Typed, false),
            (SoundKind::Typed, true),
            (SoundKind::Space, false),
            (SoundKind::Space, false),
            (SoundKind::Shift, false),
            (SoundKind::Navigation, false),
            (SoundKind::Jump, false),
            (SoundKind::Typed, false),
            (SoundKind::Typed, false),
            (SoundKind::Typed, false),
            (SoundKind::Typed, false),
            (SoundKind::Enter { cells: 40 }, false),
            (
                SoundKind::Meteor {
                    dir: 1,
                    cells: 50,
                    armed: false,
                },
                false,
            ),
            (SoundKind::MeteorArm { dir: -1 }, false),
            (SoundKind::Stardust { twinkle_hz: 10 }, false),
        ];
        let mut at = 1_000;
        for (kind, shifted) in script {
            let mark = s.born_seq;
            s.push_meta(
                event(kind, 0.3, shifted),
                EventMeta {
                    at_ms: at,
                    pan_from: -0.3,
                    ..EventMeta::default()
                },
            );
            seen.extend(assert_tails(&since(&s, mark), &format!("{kind:?}")));
            for _ in 0..60 {
                s.render(&mut buf);
            }
            at += 600;
        }
        push(&mut s, SoundKind::Typed, at, 0.0, false);
        push(&mut s, SoundKind::Backspace, at + 100, 0.0, false);
        push(&mut s, SoundKind::Typed, at + 200, 0.0, false);
        push(&mut s, SoundKind::Typed, at + 260, 0.0, false);
        push(&mut s, SoundKind::Kill, at + 300, 0.0, false);
        meteor(&mut s, at + 400, 50, false);
        meteor(&mut s, at + 440, 50, false);
        let damped = s
            .voices
            .iter()
            .filter(|v| v.on && v.lane != LANE_NONE && v.damp > 0.0)
            .count();
        assert!(
            damped >= 3,
            "the damp sweep saw only {damped} damping voices"
        );
        for v in s.voices.iter().filter(|v| v.on && v.lane != LANE_NONE) {
            if v.damp > 0.0 {
                assert!(
                    v.damp0 >= LANE_FADE_STEAL_S - 1e-6,
                    "a lane-{} voice is damping over {} s — under the 12 ms ramp",
                    v.lane,
                    v.damp0
                );
            }
        }
        seen.sort_unstable();
        seen.dedup();
        for lane in [
            LANE_TUNE,
            LANE_BLOOM,
            LANE_BASS,
            LANE_BREATH,
            LANE_GLINT,
            LANE_METEOR,
            LANE_RAIN,
            LANE_CADENCE,
            LANE_CASCADE,
            LANE_GRAFT,
        ] {
            assert!(seen.contains(&lane), "the tail sweep never saw lane {lane}");
        }
    }

    #[test]
    fn flow_s_voice_is_identity_at_heat_zero() {
        let blooms_of = |flow: f32| -> usize {
            let mut s = synth();
            let mark = s.born_seq;
            push_meta_ch(&mut s, SoundKind::Typed, 1_000, 'a', flow);
            since(&s, mark)
                .iter()
                .filter(|v| v.lane == LANE_BLOOM)
                .count()
        };
        assert_eq!(
            blooms_of(0.0),
            1,
            "the cold step spawns the bloom and nothing else"
        );
        assert_eq!(
            blooms_of(1.0),
            2,
            "negative control: the flowing step spawns the bloom and the echo"
        );
    }

    #[test]
    fn flow_echo_phase_requires_the_current_keys_sounding_octave() {
        for ch in ['e', '7', '?'] {
            let mut s = synth();
            push_meta_ch(&mut s, SoundKind::Typed, 1_000, ch, 1.0);
            let (slot, _) = s.v2.lead.expect("the real typed key owns a strike");
            let lead = s.voices[usize::from(slot)];
            let f = 2.0 * lead.p[0].f0;
            let phase = s.v2_flow_echo_phase(f, FLOW_ECHO_DELAY_S);
            if ch == 'e' {
                let p2 = lead.p[1];
                let want = (p2.ph + p2.f0 * FLOW_ECHO_DELAY_S + FLOW_ECHO_PHASE).fract();
                assert_eq!(phase, Some(want));
                let echo = s
                    .voices
                    .iter()
                    .find(|v| v.on && v.lane == LANE_BLOOM && v.delay == FLOW_ECHO_DELAY_S)
                    .expect("a lit flow key actually spawned its echo");
                assert_eq!(echo.p[0].ph, want, "the shipping spawn uses that phase");
                let mut stale = s.clone();
                stale.voices[usize::from(slot)].born = lead.born.wrapping_add(1);
                assert_eq!(
                    stale.v2_flow_echo_phase(f, FLOW_ECHO_DELAY_S),
                    None,
                    "a recycled slot is not this key"
                );
                let mut bent = s.clone();
                bent.voices[usize::from(slot)].p[1].glide = 0.010;
                assert_eq!(
                    bent.v2_flow_echo_phase(f, FLOW_ECHO_DELAY_S),
                    None,
                    "a moving partial is not stationary"
                );
            } else {
                assert_eq!(
                    phase, None,
                    "{ch}: a wood 3f or muted octave cannot phase-lock 2f"
                );
                assert!(lead.on);
                assert!(lead.p[1].lvl == 0.0 || lead.p[1].f0 != f);
            }
        }
    }

    #[test]
    fn flowing_knocks_do_not_regrow_an_octave_echo() {
        let mut caught_old_gate = 0;
        for flow in [0.0, 0.5, 1.0] {
            let mut s = synth();
            for (index, ch) in "a.b,c;d:e?f!g)h]i}j'k\"l`m-n_o~p\\q|r/s+t=u*v%w^x<y>z@a#b$c&"
                .chars()
                .enumerate()
            {
                let at = 1_000 + index as u32 * 125;
                let class = crate::trail_sound::typed_glyph_class(Some(ch));
                let rank = crate::trail_sound::typed_glyph_rank(Some(ch));
                let plan = s.v2.clone().on_typed(at, rank, false, false, class);
                let mark = s.born_seq;
                push_meta_ch(&mut s, SoundKind::Typed, at, ch, flow);
                if pluck_class(class) && !(class == STOP && plan.steered) {
                    let born = since(&s, mark);
                    assert!(born.iter().any(|v| v.lane == LANE_TUNE));
                    assert!(
                        born.iter()
                            .all(|v| v.lane != LANE_BLOOM || v.delay != FLOW_ECHO_DELAY_S),
                        "{ch:?} at flow {flow} acquired a long hang"
                    );
                    if flow > 0.0 && plan.lit && plan.touch == Touch::Step && !plan.mallet_only {
                        caught_old_gate += 1;
                    }
                }
                let _ = render_mono(&mut s, 12);
            }
        }
        assert!(
            caught_old_gate >= 10,
            "hot lit knocks must exercise the old gate"
        );
    }

    #[test]
    fn a_re_struck_nav_tick_replaces_its_predecessor_instead_of_combing() {
        let mut s = synth();
        s.push(event(SoundKind::Navigation, 0.0, false));
        let mut buf = [0.0f32; 96];
        for _ in 0..20 {
            s.render(&mut buf);
        }
        let f = penta(
            TINE_BASE_HZ,
            i32::from(s.v2.text.walk) + i32::from(s.song_key),
        );
        let live_before = s
            .voices
            .iter()
            .filter(|v| v.on && v.lane == LANE_TUNE && (v.p[0].f0 - f).abs() < SAME_PITCH_HZ)
            .count();
        assert_eq!(live_before, 1, "the first tick is still sounding");
        s.push(event(SoundKind::Navigation, 0.0, false));
        let undamped = s
            .voices
            .iter()
            .filter(|v| {
                v.on && v.lane == LANE_TUNE
                    && (v.p[0].f0 - f).abs() < SAME_PITCH_HZ
                    && v.damp <= 0.0
            })
            .count();
        assert_eq!(
            undamped, 1,
            "§9.5 law 5: exactly the newcomer is undamped at that pitch"
        );
    }

    #[test]
    fn a_word_hop_is_never_thinned_by_the_typing_gap_under_a_named_instrument() {
        for voice in [SoundVoice::Style, SoundVoice::Marimba] {
            let mut s = synth();
            let typed = SoundEvent {
                voice,
                ..event(SoundKind::Typed, 0.0, false)
            };
            let nav = SoundEvent {
                voice,
                ..event(SoundKind::Navigation, 0.0, false)
            };
            s.push(typed);
            let after_key = s.voices.iter().filter(|v| v.on).count();
            let mut buf = [0.0f32; 96];
            for _ in 0..30 {
                s.render(&mut buf);
            }
            s.push(nav);
            let after_nav = s.voices.iter().filter(|v| v.on).count();
            assert!(
                after_nav > after_key,
                "{voice:?}: a word hop 30 ms after a key must still sound"
            );
        }
    }
}
