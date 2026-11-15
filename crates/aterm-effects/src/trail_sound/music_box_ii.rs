// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Music box II: the voice that plays phrases.
//!
//! One key is still one immediate note, on the same tine as [`super::rainbow_kitty_v2`]. What
//! changes is what the notes are:
//!
//! - **A word is a figure.** The notes of a word are a function of its letters and of nothing
//!   else ([`Figure`]): each letter steps from the one before it by an interval drawn from the
//!   word so far, so the same word plays the same shape every time and words that start alike
//!   start alike.
//! - **A phrase has one high point.** Each word starts on a tone of the chord under it. Those
//!   starting notes climb for a few words, reach one apex, and come down; no note before or
//!   after the apex word is as high as its highest note.
//! - **Harmony holds.** A chord lasts two to six words and moves by a small table of the
//!   progressions songs use; the finished word chooses among them. A full stop comes home to
//!   the tonic, a question stops on the dominant.
//! - **The space bar is the bass.** Every space is one bass note: the new chord's root when the
//!   chord moves, a walk through its tones while it holds.
//! - **Enter resets nothing.** It ends the line as it would end a phrase; the key, the chord,
//!   the bass and the place in the piece carry on.
//! - **A command's result resolves.** Enter at a shell prompt ([`EventMeta::prompt`]) stops on
//!   the dominant and waits. A result inside [`AWAIT_MS`] answers it: a pass comes home to the
//!   tonic, a failure turns aside to the sixth. A later result is not sounded.
//! - **A pause ends the phrase**, [`REST_MS`] of rendered audio after its last key, and a
//!   phrase left hanging is answered by one quiet note home. The note takes the last key's
//!   gain: it belongs to that key, as a note that key delayed would.
//! - **Each session keeps its own part** ([`EventMeta::session`]), and a key typed at a prompt
//!   that does not echo makes one unpitched tick and changes nothing.
//! - **The notes share one body** ([`super::music_box_body`]): every voice this module starts
//!   sounds through the box's comb, soundboard and room, and the ribbon's hue under the cursor
//!   is where on the tine the key strikes ([`strike_point`]).
//! - **Dynamics are carved only downward**: every factor on a note's level is at most one
//!   (the word's inner letters, fast typing, a pitch struck again while it rings, a long sitting).
//!
//! The decisions use their own random stream ([`MelodyII::rng`]), never the synth's, which the
//! per-sample noise draws from: what is played never depends on how the audio was rendered.

use super::glyph_class::{BANG, COLON, COMMA, QMARK, SECRET, SEMI, STOP};
use super::rainbow_kitty_v2::{LANE_CASCADE, TINE_BASE_HZ, Touch, bass_dyad, tine};
use super::{
    ERASE_MIN_GAP, EventMeta, HELD_ERASE_RUN_WINDOW, OutputGesture, PAN_LAW_SCALE, SoundEvent,
    SoundGesture, SoundKind, TrailSynth, Voice, pow2i, xorshift01,
};
use crate::tone::Tone;

/// Sessions that keep a part at once; the one unused longest gives its place to a new one.
const PARTS: usize = 8;
/// The letters of a word that Backspace can take back one at a time.
const WORD_MAX: usize = 24;
/// The tune's register in scale degrees from C5: G4 to F6.
pub(super) const TUNE_LO: i32 = -3;
pub(super) const TUNE_HI: i32 = 10;
/// How far a word's figure reaches from the note it starts on.
pub(super) const REACH: i32 = 2;
/// Where a word may start, so its whole figure stays in the register.
const HEAD_LO: i32 = TUNE_LO + REACH;
const HEAD_HI: i32 = TUNE_HI - REACH;
/// The highest start of a word before the apex, which leaves the apex room above it.
const RISE_HI: i32 = 3;
/// Where the line settles after the apex.
const POST_FLOOR: i32 = 1;
/// The bass register, one of each scale degree: A3 to G4.
const BASS_LO: i32 = -9;
const BASS_HI: i32 = -3;
/// A pause this long ends the phrase.
pub(super) const REST_MS: u32 = 1_400;
const REST_S: f32 = REST_MS as f32 * 0.001;
/// How long Enter at a prompt waits for the command's result.
pub(super) const AWAIT_MS: u32 = 4_000;
/// The result's answer never follows Enter's own bass note sooner than this.
pub(super) const CADENCE_MIN_MS: u32 = 200;
/// The answer's tune note follows its bass by this much.
const CADENCE_AFTER_S: f32 = 0.07;
/// The notes a phrase needs before its rest is answered.
const ANSWER_MIN_NOTES: u32 = 8;

/// C major, in semitones above the tonic.
const SCALE: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];
/// Equal-tempered semitone ratios.
const SEMITONE: [f32; 12] = [
    1.0,
    1.059_463_1,
    1.122_462,
    1.189_207_1,
    1.259_921,
    1.334_839_9,
    core::f32::consts::SQRT_2,
    1.498_307_1,
    1.587_401,
    1.681_792_9,
    1.781_797_4,
    1.887_748_6,
];

/// The chords a chord may move to, by its root's scale degree; the finished word picks one.
/// The seventh degree's chord is never chosen.
const NEXT_CHORD: [[u8; 4]; 7] = [
    [3, 5, 4, 1],
    [4, 4, 4, 5],
    [5, 3, 5, 3],
    [4, 0, 4, 1],
    [0, 0, 5, 3],
    [3, 1, 3, 4],
    [0, 0, 0, 0],
];
/// A letter's step from the one before it: mostly a second, sometimes a third.
const STEPS: [i32; 8] = [1, -1, 1, -1, 1, -1, 2, -2];
/// How a word's first letter leans its start against the line.
const NUDGE: [i32; 4] = [0, 0, 1, -1];
const FIGURE_SEED: u32 = 0x4B49_5454;
const APEX_SALT: u32 = 0xA9E5_17C3;

/// Levels: every one is a factor of at most one on the event's gain. The two trims carry the
/// 2.1 dB the body takes from a take's energy ([`super::music_box_body`]; measured with
/// `lab music-render` on one paragraph, body in and out), so the box is as loud as it was
/// without one and its peak is lower.
const TUNE_TRIM: f32 = 0.3491;
/// A plain key against a capital, which alone is struck at full level.
const PLAIN_LEVEL: f32 = 0.80;
const INNER_LEVEL: f32 = 0.78;
const COMMA_LEVEL: f32 = 0.80;
const GHOST_LEVEL: f32 = 0.55;
const BASS_TRIM: f32 = 0.2547;
const BASS_WALK_LEVEL: f32 = 0.50;
const BASS_IDLE_LEVEL: f32 = 0.30;
const ANSWER_LEVEL: f32 = 0.35;
const CADENCE_BASS_LEVEL: f32 = 0.85;
const CADENCE_LEVEL: f32 = 0.45;
const SECRET_LEVEL: f32 = 0.50;
const STRUM_LEVEL: f32 = 0.60;
/// The arrow keys walk the scale at a whisper; a jump (a completion, a redraw) is one chord tone.
const NAV_LEVEL: f32 = 0.18;
const NAV_TAU_S: f32 = 0.16;
const JUMP_LEVEL: f32 = 0.35;
/// A jump this soon after Enter is that Enter's own line feed, and is not sounded again.
const ENTER_ECHO_MS: u32 = 150;
/// The loudest a key's humanised touch falls short of full, as a share.
const TOUCH_SPREAD: f32 = 0.11;
/// Fast typing is quieter: the level follows the square root of the gap, down to this floor.
const GAP_REF_S: f32 = 0.25;
const GAP_FLOOR: f32 = 0.45;
/// A pitch struck again while it rings is quieter, by this much for each strike still counted.
const PITCH_LOAD_COST: f32 = 0.20;
const PITCH_LOAD_TAU_MS: f32 = 4_000.0;
/// A long sitting is quieter and darker: the count of keys, forgotten over half an hour.
const TIRED_TAU_MS: f32 = 1_800_000.0;
const TIRED_FULL: f32 = 4_000.0;
const TIRED_LEVEL_COST: f32 = 0.40;
const TIRED_ROOF_COST: f32 = 0.20;

/// A tine's ring, from the gap between keys.
const TAU_BASE_S: f32 = 0.10;
const TAU_PER_GAP: f32 = 0.90;
const TAU_MIN_S: f32 = 0.16;
const TAU_MAX_S: f32 = 0.42;
const HEAD_TAU_MUL: f32 = 1.15;
const COMMA_TAU_MUL: f32 = 1.30;
const CLOSE_TAU_MUL: f32 = 1.80;
const ANSWER_TAU_S: f32 = 0.60;
const BASS_TAU_S: f32 = 0.35;
const BASS_WALK_TAU_S: f32 = 0.22;
const BASS_CLOSE_TAU_S: f32 = 0.50;
/// The lowpass over a tine.
const ROOF_INNER_HZ: f32 = 4_200.0;
const ROOF_HEAD_HZ: f32 = 5_200.0;
const ROOF_HEAT_HZ: f32 = 600.0;
const ROOF_SHIFT_HZ: f32 = 1_500.0;
const ROOF_LIT_HZ: f32 = 2_300.0;
const ROOF_MAX_HZ: f32 = 7_500.0;
/// A failure's answer is darker than a pass's.
const ROOF_FAIL_HZ: f32 = 2_600.0;
const PAN_JITTER: f32 = 0.03;
const ERASE_MUTE_S: f32 = 0.040;
const STRUM_STEP_S: f32 = 0.022;
/// The gap a part starts from, and returns to after a pause.
const IOI_REST_MS: f32 = 400.0;

const fn maxi(a: i32, b: i32) -> i32 {
    if a > b { a } else { b }
}

const fn mini(a: i32, b: i32) -> i32 {
    if a < b { a } else { b }
}

const fn mix(h: u32, x: u32) -> u32 {
    let mut z = (h ^ x).wrapping_mul(0x9E37_79B1);
    z ^= z >> 15;
    z = z.wrapping_mul(0x85EB_CA6B);
    z ^ (z >> 13)
}

/// How far the strike point moves a tine's overtones: at one end of its travel the second
/// partial is cut by this much, at the other the third.
pub(super) const STRIKE_DEPTH: f32 = 0.5;

/// Where on the tine a key strikes, from the ribbon's hue under the cursor: the weights of the
/// tine's second and third partials, each in `1 - STRIKE_DEPTH ..= 1`. The hue is a circle, so
/// the weights are too (hue 0 and hue 1 are the same strike), and one rises as the other falls:
/// as the ribbon's colour turns, the note's overtones trade places and its level never grows.
pub(super) const fn strike_point(hue: f32) -> (f32, f32) {
    let (_, cos) = crate::supernova::sin_cos_f32(core::f32::consts::TAU * hue.fract());
    let along = 0.5 - 0.5 * cos;
    (
        1.0 - STRIKE_DEPTH * along,
        1.0 - STRIKE_DEPTH * (1.0 - along),
    )
}

/// The frequency of a scale degree counted from C5.
pub(super) const fn degree_hz(deg: i32) -> f32 {
    let oct = deg.div_euclid(7);
    let step = deg.rem_euclid(7) as usize;
    TINE_BASE_HZ * SEMITONE[SCALE[step] as usize] * pow2i(oct)
}

/// Whether `deg` is the root, third or fifth of the chord on `root`.
pub(super) const fn is_chord_tone(root: i32, deg: i32) -> bool {
    matches!((deg - root).rem_euclid(7), 0 | 2 | 4)
}

const fn chord_tone_above(root: i32, from: i32) -> i32 {
    let mut d = from;
    while !is_chord_tone(root, d) {
        d += 1;
    }
    d
}

const fn chord_tone_below(root: i32, from: i32) -> i32 {
    let mut d = from;
    while !is_chord_tone(root, d) {
        d -= 1;
    }
    d
}

/// The chord tone nearest `target` (a chord tone is never more than a step away), looking up
/// first when `up_first`, held to `lo..=hi`; where no chord tone lies between them, `hi` wins.
const fn chord_tone_within(root: i32, target: i32, lo: i32, hi: i32, up_first: bool) -> i32 {
    let target = maxi(lo, mini(target, hi));
    let (a, b) = if up_first {
        (target + 1, target - 1)
    } else {
        (target - 1, target + 1)
    };
    let mut t = if is_chord_tone(root, target) {
        target
    } else if is_chord_tone(root, a) {
        a
    } else {
        b
    };
    if t < lo {
        t = chord_tone_above(root, lo);
    }
    if t > hi {
        t = chord_tone_below(root, hi);
    }
    t
}

/// The one degree of `deg`'s pitch class in the bass register.
const fn bass_of(deg: i32) -> i32 {
    (deg - BASS_LO).rem_euclid(7) + BASS_LO
}

/// The shape of a word so far: a function of its letters alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Figure {
    pub(super) hash: u32,
    /// The current note, in degrees from the word's first.
    pub(super) offset: i32,
    prev: i32,
    pub(super) len: u32,
}

impl Figure {
    pub(super) const NEW: Self = Self {
        hash: FIGURE_SEED,
        offset: 0,
        prev: 0,
        len: 0,
    };

    /// The figure with one more letter.
    pub(super) const fn push(self, rank: u8) -> Self {
        let hash = mix(self.hash, rank as u32 + 1);
        if self.len == 0 {
            return Self {
                hash,
                offset: 0,
                prev: 0,
                len: 1,
            };
        }
        let mut step = STEPS[(hash & 7) as usize];
        if self.prev >= 2 {
            // A leap is answered by a step back.
            step = -1;
        } else if self.prev <= -2 {
            step = 1;
        } else if self.prev != 0 && (hash >> 3) & 3 != 0 {
            // Three times in four a line keeps the way it was going.
            let size = step.abs();
            step = if self.prev > 0 { size } else { -size };
        }
        if self.offset + step > REACH || self.offset + step < -REACH {
            step = -step;
        }
        Self {
            hash,
            offset: self.offset + step,
            prev: step,
            len: self.len + 1,
        }
    }
}

/// How the phrase's last mark asks it to end, once a space or Enter confirms it was the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Close {
    None,
    /// `.` or `!`: home.
    Full,
    /// `?`: on the dominant.
    Open,
}

/// One note the tune decided on.
#[derive(Clone, Copy, Debug)]
struct Note {
    deg: i32,
    level: f32,
    tau_mul: f32,
    lit: bool,
    head: bool,
    pos: u32,
}

/// One bass note.
#[derive(Clone, Copy, Debug)]
struct Bass {
    deg: i32,
    level: f32,
    tau: f32,
    dyad: bool,
}

/// One session's place in the piece.
#[derive(Clone, Copy, Debug)]
pub(super) struct Part {
    id: u32,
    used: u32,
    live: bool,
    last_at: u32,
    pub(super) keys: u32,
    ioi_ms: f32,
    /// The chord's root, a scale degree 0..=5.
    pub(super) chord: i32,
    chord_words: u32,
    chord_hold: u32,
    pub(super) bass: i32,
    /// The last tune note.
    pub(super) last: i32,
    /// The note this word started on.
    pub(super) head: i32,
    pub(super) fig: Figure,
    word: [u8; WORD_MAX],
    word_hash: u32,
    /// Words begun in this phrase before the current one.
    pub(super) phrase_words: u32,
    phrase_notes: u32,
    pub(super) apex_word: u32,
    /// The highest note of the apex word so far, once it has begun.
    apex_pitch: i32,
    /// The highest note before the apex word.
    peak: i32,
    line: i32,
    /// The line and the start before this word began, which taking its first letter back restores.
    undo_line: i32,
    undo_head: i32,
    pending: Close,
    /// The phrase before this one ended on a full close and nothing was played since.
    pub(super) closed: bool,
    rested: bool,
    /// Where the arrow keys have walked to, in degrees from the last note; a typed key ends the walk.
    nav: i32,
    entered: bool,
    enter_at: u32,
    /// Glyphs typed since Enter.
    line_keys: u32,
    /// Enter at a prompt left the music on the dominant, and the command's result has not come.
    pub(super) awaiting: bool,
    lead_slot: u8,
    lead_born: u32,
    last_gain: f32,
    last_pan: f32,
}

const NO_PITCH: i32 = i32::MIN;

impl Part {
    const NEW: Self = Self {
        id: 0,
        used: 0,
        live: false,
        last_at: 0,
        keys: 0,
        ioi_ms: IOI_REST_MS,
        chord: 0,
        chord_words: 0,
        chord_hold: 3,
        bass: -7,
        last: 2,
        head: 2,
        fig: Figure::NEW,
        word: [0; WORD_MAX],
        word_hash: FIGURE_SEED,
        phrase_words: 0,
        phrase_notes: 0,
        apex_word: 3,
        apex_pitch: NO_PITCH,
        peak: NO_PITCH,
        line: 2,
        undo_line: 2,
        undo_head: 2,
        pending: Close::None,
        closed: false,
        rested: true,
        nav: 0,
        entered: false,
        enter_at: 0,
        line_keys: 0,
        awaiting: false,
        lead_slot: 0,
        lead_born: 0,
        last_gain: 0.0,
        last_pan: 0.0,
    };

    /// Start the next phrase where the last one left off, pulled toward the middle.
    const fn begin_phrase(&mut self) {
        self.phrase_words = 0;
        self.phrase_notes = 0;
        self.apex_pitch = NO_PITCH;
        self.peak = NO_PITCH;
        self.line = maxi(0, mini((self.last + 2).div_euclid(2), 2));
        self.pending = Close::None;
    }

    /// Where the word that starts with `rank` begins.
    const fn start_word(&mut self, rank: u8) {
        let h = mix(FIGURE_SEED, rank as u32 + 1);
        let w = self.phrase_words;
        self.undo_line = self.line;
        self.undo_head = self.head;
        if w == 0 && self.phrase_notes == 0 {
            self.apex_word = 2 + mix(APEX_SALT, rank as u32) % 3;
        }
        let nudge = NUDGE[((h >> 8) & 3) as usize];
        let head = if w < self.apex_word {
            if w > 0 {
                self.line = mini(self.line + 1, RISE_HI);
            }
            let head = chord_tone_within(self.chord, self.line + nudge, HEAD_LO, RISE_HI, true);
            let higher = chord_tone_above(self.chord, head + 1);
            if w > 0 && head == self.head && higher <= RISE_HI {
                higher
            } else {
                head
            }
        } else if w == self.apex_word {
            let want = maxi(self.peak.saturating_add(1), self.line + 2);
            let head = chord_tone_above(self.chord, maxi(want, HEAD_LO));
            let head = if head > HEAD_HI {
                chord_tone_below(self.chord, HEAD_HI)
            } else {
                head
            };
            self.line = head;
            head
        } else {
            let hi = if self.apex_pitch == NO_PITCH {
                HEAD_HI
            } else {
                maxi(mini(self.apex_pitch - REACH - 1, HEAD_HI), HEAD_LO)
            };
            self.line = maxi(self.line - 1, POST_FLOOR);
            let head = chord_tone_within(self.chord, self.line + nudge, HEAD_LO, hi, false);
            let lower = chord_tone_below(self.chord, head - 1);
            if head == self.head && lower >= HEAD_LO {
                lower
            } else {
                head
            }
        };
        self.head = head;
    }

    /// The note for one typed glyph.
    const fn typed(&mut self, rank: u8, class: u8) -> Note {
        self.pending = Close::None;
        self.nav = 0;
        let head = self.fig.len == 0;
        if head {
            self.start_word(rank);
        }
        if (self.fig.len as usize) < WORD_MAX {
            self.word[self.fig.len as usize] = rank;
        }
        self.fig = self.fig.push(rank);
        let figure = self.head + self.fig.offset;
        let (deg, level, tau_mul, lit) = match class {
            STOP => {
                self.pending = Close::Full;
                (
                    if self.last >= 7 { 7 } else { 0 },
                    1.0,
                    CLOSE_TAU_MUL,
                    false,
                )
            }
            BANG => {
                self.pending = Close::Full;
                let top = if self.apex_pitch == NO_PITCH {
                    TUNE_HI
                } else {
                    self.apex_pitch
                };
                (if top >= 7 { 7 } else { 4 }, 1.0, CLOSE_TAU_MUL, true)
            }
            QMARK => {
                self.pending = Close::Open;
                let up = self.last + 1;
                let deg = if up <= 1 {
                    1
                } else if up <= 4 {
                    4
                } else {
                    8
                };
                (deg, 1.0, CLOSE_TAU_MUL, true)
            }
            COMMA | SEMI | COLON => {
                let below = chord_tone_below(self.chord, self.last - 1);
                (maxi(below, TUNE_LO), COMMA_LEVEL, COMMA_TAU_MUL, false)
            }
            _ => (
                figure,
                if head { 1.0 } else { INNER_LEVEL },
                if head { HEAD_TAU_MUL } else { 1.0 },
                false,
            ),
        };
        let deg = maxi(TUNE_LO, mini(deg, TUNE_HI));
        if self.phrase_words < self.apex_word {
            self.peak = maxi(self.peak, deg);
        } else if self.phrase_words == self.apex_word {
            self.apex_pitch = maxi(self.apex_pitch, deg);
        }
        self.last = deg;
        self.phrase_notes += 1;
        self.line_keys = self.line_keys.saturating_add(1);
        self.closed = false;
        Note {
            deg,
            level,
            tau_mul,
            lit,
            head,
            pos: self.fig.len - 1,
        }
    }

    /// The word is over: count it, and say whether there was one.
    const fn end_word(&mut self) -> bool {
        if self.fig.len == 0 {
            return false;
        }
        self.word_hash = self.fig.hash;
        self.fig = Figure::NEW;
        self.phrase_words += 1;
        self.chord_words += 1;
        true
    }

    const fn hold_for(&self, hash: u32) -> u32 {
        let quick = if self.ioi_ms < 140.0 {
            2
        } else if self.ioi_ms < 220.0 {
            1
        } else {
            0
        };
        2 + (hash >> 2) % 3 + quick
    }

    const fn move_chord(&mut self, to: i32) {
        self.chord = to;
        self.chord_words = 0;
        self.chord_hold = self.hold_for(self.word_hash);
    }

    /// The next tone of the chord above the bass, wrapping inside its register.
    const fn walk(&self) -> i32 {
        let mut d = self.bass + 1;
        let mut n = 0;
        while n < 7 {
            if d > BASS_HI {
                d = BASS_LO;
            }
            if is_chord_tone(self.chord, d) {
                return d;
            }
            d += 1;
            n += 1;
        }
        self.bass
    }

    /// The bass note for a space, or for Enter; `prompt` says Enter ran a command whose result
    /// the shell will report.
    const fn spaced(&mut self, enter: bool, prompt: bool) -> Bass {
        let word = self.end_word();
        let pending = self.pending;
        // A command line stops on the dominant whatever mark it ends with: its result ends it.
        let asks = enter && prompt;
        let full = !asks && (matches!(pending, Close::Full) || (enter && self.closed));
        let open = asks || matches!(pending, Close::Open);
        let bass = if full || open {
            self.move_chord(if full { 0 } else { 4 });
            Bass {
                deg: bass_of(self.chord),
                level: 1.0,
                tau: if full { BASS_CLOSE_TAU_S } else { BASS_TAU_S },
                dyad: full,
            }
        } else if word && !enter && self.chord_words >= self.chord_hold {
            let to = NEXT_CHORD[self.chord as usize][(self.word_hash & 3) as usize] as i32;
            self.move_chord(to);
            Bass {
                deg: bass_of(self.chord),
                level: 1.0,
                tau: BASS_TAU_S,
                dyad: false,
            }
        } else {
            Bass {
                deg: self.walk(),
                level: if word || enter {
                    BASS_WALK_LEVEL
                } else {
                    BASS_IDLE_LEVEL
                },
                tau: BASS_WALK_TAU_S,
                dyad: false,
            }
        };
        if full || open || enter {
            self.begin_phrase();
            self.closed = full;
        }
        if enter {
            self.awaiting = asks;
            self.line_keys = 0;
        }
        self.bass = bass.deg;
        bass
    }

    /// The command's result, for a part that waits on it: the bass, and the tune's note where
    /// nothing was typed since Enter.
    const fn resolve(&mut self, failed: bool) -> (Bass, Option<i32>) {
        self.awaiting = false;
        self.move_chord(if failed { 5 } else { 0 });
        self.bass = bass_of(self.chord);
        let tune = if self.line_keys == 0 {
            let to = if failed {
                // The sixth itself, in the octave nearer the last note.
                if self.last <= 1 { -2 } else { 5 }
            } else {
                let down = chord_tone_below(0, self.last);
                let up = chord_tone_above(0, self.last);
                if self.last - down <= up - self.last {
                    down
                } else {
                    up
                }
            };
            self.last = maxi(TUNE_LO, mini(to, TUNE_HI));
            Some(self.last)
        } else {
            None
        };
        self.closed = !failed && self.line_keys == 0;
        (
            Bass {
                deg: self.bass,
                level: CADENCE_BASS_LEVEL,
                tau: BASS_CLOSE_TAU_S,
                dyad: !failed,
            },
            tune,
        )
    }

    /// Backspace takes the last letter of the word back, and the figure with it.
    const fn unsing(&mut self) {
        self.pending = Close::None;
        if self.fig.len == 0 || self.fig.len as usize > WORD_MAX {
            return;
        }
        let keep = self.fig.len as usize - 1;
        let mut fig = Figure::NEW;
        let mut i = 0;
        while i < keep {
            fig = fig.push(self.word[i]);
            i += 1;
        }
        self.fig = fig;
        self.phrase_notes = self.phrase_notes.saturating_sub(1);
        self.line_keys = self.line_keys.saturating_sub(1);
        if keep > 0 {
            self.last = maxi(TUNE_LO, mini(self.head + fig.offset, TUNE_HI));
        } else {
            self.line = self.undo_line;
            self.head = self.undo_head;
        }
    }

    /// A kill takes the word in progress.
    const fn kill(&mut self) {
        self.pending = Close::None;
        self.fig = Figure::NEW;
    }

    /// A long pause ends the phrase; the word in progress, if any, keeps its figure.
    const fn rest(&mut self) -> Option<i32> {
        self.rested = true;
        let unresolved = !self.closed
            && self.phrase_notes >= ANSWER_MIN_NOTES
            && !(self.chord == 0 && is_chord_tone(0, self.last));
        let answer = if unresolved {
            let down = chord_tone_below(0, self.last);
            let up = chord_tone_above(0, self.last);
            let to = if self.last - down <= up - self.last {
                down
            } else {
                up
            };
            self.last = maxi(TUNE_LO, mini(to, TUNE_HI));
            self.chord = 0;
            self.chord_words = 0;
            Some(self.last)
        } else {
            None
        };
        self.begin_phrase();
        if self.fig.len > 0 {
            // Mid-word: the word goes on as the first of the next phrase.
            self.phrase_notes = 1;
        }
        self.closed = unresolved || self.closed;
        answer
    }
}

/// The new music box's state: a part for each session, and what the ear has had.
#[derive(Clone, Copy, Debug)]
pub struct MelodyII {
    parts: [Part; PARTS],
    stamp: u32,
    /// The part the last key went to.
    active: usize,
    /// The decisions' own random stream.
    rng: u32,
    /// How lately each scale degree was struck.
    load: [f32; 7],
    /// The keys of this sitting, forgotten over [`TIRED_TAU_MS`].
    tired: f32,
    last_at: u32,
    any: bool,
    /// Audio rendered since the last key, in seconds.
    idle_s: f32,
}

impl Default for MelodyII {
    fn default() -> Self {
        Self::new(1)
    }
}

impl MelodyII {
    /// A fresh instrument whose touch is seeded by `seed`.
    #[must_use]
    pub const fn new(seed: u32) -> Self {
        Self {
            parts: [Part::NEW; PARTS],
            stamp: 0,
            active: 0,
            rng: (seed ^ 0x2B0C_11E5) | 1,
            load: [0.0; 7],
            tired: 0.0,
            last_at: 0,
            any: false,
            idle_s: 0.0,
        }
    }

    /// The part of session `id`, made (in the place unused longest) when it has none.
    const fn part_index(&mut self, id: u32) -> usize {
        self.stamp = self.stamp.wrapping_add(1);
        let (mut i, mut pick, mut oldest) = (0, 0, u32::MAX);
        while i < PARTS {
            let (live, used) = (self.parts[i].live, self.parts[i].used);
            if live && self.parts[i].id == id {
                self.parts[i].used = self.stamp;
                self.active = i;
                return i;
            }
            let age = if live { used } else { 0 };
            if age < oldest {
                oldest = age;
                pick = i;
            }
            i += 1;
        }
        self.parts[pick] = Part {
            id,
            used: self.stamp,
            live: true,
            ..Part::NEW
        };
        self.active = pick;
        pick
    }

    /// Account one key at `at`: the part's gap in milliseconds, and the ear's ledgers.
    const fn clock(&mut self, idx: usize, at: u32) -> f32 {
        let since_any = if self.any {
            at.wrapping_sub(self.last_at) as f32
        } else {
            f32::MAX
        };
        self.any = true;
        self.last_at = at;
        self.idle_s = 0.0;
        let fade = 1.0 / (1.0 + since_any / PITCH_LOAD_TAU_MS);
        let mut i = 0;
        while i < self.load.len() {
            self.load[i] *= fade;
            i += 1;
        }
        self.tired = (self.tired / (1.0 + since_any / TIRED_TAU_MS) + 1.0).min(TIRED_FULL * 2.0);
        let p = &mut self.parts[idx];
        let gap = if p.keys == 0 {
            f32::MAX
        } else {
            at.wrapping_sub(p.last_at) as f32
        };
        p.last_at = at;
        p.keys = p.keys.saturating_add(1);
        p.rested = false;
        p.ioi_ms = if gap > REST_MS as f32 {
            IOI_REST_MS
        } else {
            0.6 * p.ioi_ms + 0.4 * gap
        };
        gap
    }

    /// How tired the ear is, 0..=1.
    const fn fatigue(&self) -> f32 {
        (self.tired / TIRED_FULL).clamp(0.0, 1.0)
    }

    /// Strike scale degree `deg`: the level its recent strikes leave it.
    const fn strike(&mut self, deg: i32) -> f32 {
        let pc = deg.rem_euclid(7) as usize;
        let level = 1.0 / (1.0 + PITCH_LOAD_COST * self.load[pc]);
        self.load[pc] += 1.0;
        level
    }

    /// Where the part of session `id` is, where it has one.
    const fn find(&self, id: u32) -> Option<usize> {
        let mut i = 0;
        while i < PARTS {
            if self.parts[i].live && self.parts[i].id == id {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    #[cfg(test)]
    pub(super) const fn part(&self, id: u32) -> Option<&Part> {
        match self.find(id) {
            Some(i) => Some(&self.parts[i]),
            None => None,
        }
    }
}

impl TrailSynth {
    /// The decisions' random stream, never the synth's.
    #[inline]
    const fn ii_rnd(&mut self) -> f32 {
        xorshift01(&mut self.ii.rng)
    }

    #[inline]
    const fn ii_at_ms(&self, meta: EventMeta) -> u32 {
        if meta.at_ms != 0 {
            meta.at_ms
        } else {
            (self.clock_s * 1000.0) as u64 as u32
        }
    }

    /// Spawn `proto` with phases from the decisions' stream: the voice's slot and birth, or `(0, 0)`.
    const fn ii_spawn(&mut self, mut proto: Voice, gain: f32, pan: f32) -> (u8, u32) {
        proto.body = true;
        let tw_ph = self.ii_rnd();
        let ph = [self.ii_rnd(), self.ii_rnd(), self.ii_rnd()];
        match self.spawn_seeded(proto, gain, pan, tw_ph, ph) {
            Some(idx) => (idx as u8, self.voices[idx].born),
            None => (0, 0),
        }
    }

    const fn ii_pan(&mut self, pan: f32) -> f32 {
        pan + (2.0 * self.ii_rnd() - 1.0) * PAN_JITTER / PAN_LAW_SCALE
    }

    /// Fade the part's last tune note out.
    const fn ii_damp_lead(&mut self, idx: usize) {
        let (slot, born) = (self.ii.parts[idx].lead_slot, self.ii.parts[idx].lead_born);
        self.ii.parts[idx].lead_born = 0;
        if born == 0 {
            return;
        }
        let v = &mut self.voices[slot as usize];
        if v.on && v.born == born && v.damp <= 0.0 {
            v.damp = ERASE_MUTE_S;
            v.damp0 = ERASE_MUTE_S;
        }
    }

    /// One event for the new music box.
    pub(super) const fn push_ii(&mut self, ev: SoundEvent, meta: EventMeta) {
        // A result is not a key: it moves neither the rate nor the rest.
        if let SoundGesture::Output(OutputGesture::Verdict { failed, .. }) = ev.kind {
            return self.ii_verdict(&ev, meta, failed);
        }
        let SoundGesture::Trail(kind) = ev.kind else {
            return;
        };
        self.bed_style = ev.style;
        self.bed_voice = ev.voice;
        self.tone = Tone::Technical;
        const INV_RATE_TAU: f32 = -1.0 / 0.6;
        self.rate = self.rate * crate::color_math::exp_f32(self.since_event * INV_RATE_TAU) + 1.0;
        self.since_event = 0.0;
        if matches!(kind, SoundKind::Typed | SoundKind::Space) && meta.glyph_class == SECRET {
            self.since_voice = 0.0;
            return self.ii_secret(&ev);
        }
        match kind {
            SoundKind::Typed => {
                self.since_voice = 0.0;
                self.ii_typed(&ev, meta);
            }
            SoundKind::Space => {
                self.since_voice = 0.0;
                self.ii_space(&ev, meta, false);
            }
            SoundKind::Enter { .. } => {
                self.since_voice = 0.0;
                self.ii_space(&ev, meta, true);
            }
            SoundKind::Backspace => {
                let idx = self.ii.part_index(meta.session);
                self.ii_damp_lead(idx);
                self.ii.parts[idx].unsing();
                if self.since_erase < ERASE_MIN_GAP {
                    return;
                }
                self.erase_run = if self.since_erase <= HELD_ERASE_RUN_WINDOW {
                    self.erase_run.saturating_add(1)
                } else {
                    1
                };
                self.since_erase = 0.0;
                self.design_trail(ev, kind, 1.0, 0.0);
            }
            SoundKind::Kill | SoundKind::KillWord => {
                self.since_voice = 0.0;
                let idx = self.ii.part_index(meta.session);
                self.ii_damp_lead(idx);
                self.ii.parts[idx].kill();
                self.design_trail(ev, kind, 1.0, 0.0);
            }
            SoundKind::Poof => self.design_trail(ev, kind, 1.0, 0.0),
            SoundKind::Strum => {
                self.since_voice = 0.0;
                self.ii_strum(&ev, meta);
            }
            SoundKind::Navigation => self.ii_nav(&ev, meta, 0),
            SoundKind::Glide { dir } => self.ii_nav(&ev, meta, dir.signum() as i32),
            SoundKind::Sweep { dir } | SoundKind::Meteor { dir, .. } => {
                self.ii_nav(&ev, meta, 2 * dir.signum() as i32);
            }
            SoundKind::Jump => self.ii_jump(&ev, meta),
            // No landing voice, as in the first music box; Shift, the meteor's pre-cue and the
            // sky's glints are not keys and say nothing here.
            SoundKind::Land
            | SoundKind::Shift
            | SoundKind::MeteorArm { .. }
            | SoundKind::Stardust { .. } => {}
        }
    }

    const fn ii_typed(&mut self, ev: &SoundEvent, meta: EventMeta) {
        let at = self.ii_at_ms(meta);
        let idx = self.ii.part_index(meta.session);
        let rested = self.ii.parts[idx].rested;
        let gap_ms = self.ii.clock(idx, at);
        if gap_ms >= REST_MS as f32 && !rested && self.ii.parts[idx].phrase_notes > 0 {
            // The pause ended the phrase where no audio was rendered through it (the device
            // was paused, or the last key was another session's); its answer's time has passed.
            let _ = self.ii.parts[idx].rest();
            self.ii.parts[idx].rested = false;
        }
        let note = self.ii.parts[idx].typed(meta.rank, meta.glyph_class);
        let ioi_s = self.ii.parts[idx].ioi_ms * 0.001;
        let fatigue = self.ii.fatigue();
        let gap_level = crate::color_math::sqrt_f32(ioi_s / GAP_REF_S).clamp(GAP_FLOOR, 1.0);
        let ghost = if fatigue > 0.5 && !note.head && note.pos % 2 == 1 {
            GHOST_LEVEL
        } else {
            1.0
        };
        let touch = 1.0 - TOUCH_SPREAD * self.ii_rnd();
        let level = note.level
            * if ev.shifted { 1.0 } else { PLAIN_LEVEL }
            * gap_level
            * ghost
            * touch
            * self.ii.strike(note.deg)
            * (1.0 - TIRED_LEVEL_COST * fatigue);
        let tau = (TAU_BASE_S + TAU_PER_GAP * ioi_s).clamp(TAU_MIN_S, TAU_MAX_S) * note.tau_mul;
        let roof = (if note.head {
            ROOF_HEAD_HZ
        } else {
            ROOF_INNER_HZ
        } + ROOF_HEAT_HZ * ev.heat
            + if ev.shifted { ROOF_SHIFT_HZ } else { 0.0 }
            + if note.lit { ROOF_LIT_HZ } else { 0.0 })
        .min(ROOF_MAX_HZ)
            * (1.0 - TIRED_ROOF_COST * fatigue);
        let pan = self.ii_pan(ev.pan);
        let mut voice = tine(degree_hz(note.deg), Touch::Step, tau, roof, false, 0.0);
        let (second, third) = strike_point(ev.hue);
        voice.p[1].lvl *= second;
        voice.p[2].lvl *= third;
        let (slot, born) = self.ii_spawn(voice, ev.gain * TUNE_TRIM * level, pan);
        let p = &mut self.ii.parts[idx];
        p.lead_slot = slot;
        p.lead_born = born;
        p.last_gain = ev.gain;
        p.last_pan = ev.pan;
    }

    const fn ii_space(&mut self, ev: &SoundEvent, meta: EventMeta, enter: bool) {
        let at = self.ii_at_ms(meta);
        let idx = self.ii.part_index(meta.session);
        let _ = self.ii.clock(idx, at);
        let bass = self.ii.parts[idx].spaced(enter, meta.prompt);
        if enter {
            self.ii.parts[idx].entered = true;
            self.ii.parts[idx].enter_at = at;
        }
        let ioi_s = self.ii.parts[idx].ioi_ms * 0.001;
        let gap_level = crate::color_math::sqrt_f32(ioi_s / GAP_REF_S).clamp(GAP_FLOOR, 1.0);
        let level = bass.level * gap_level * (1.0 - TIRED_LEVEL_COST * self.ii.fatigue());
        let f = degree_hz(bass.deg);
        let mut voice = bass_dyad(f, f * 1.5, bass.tau, 0.0);
        if !bass.dyad {
            voice.p[1].lvl = 0.0;
        }
        let pan = self.ii_pan(ev.pan * 0.5);
        let _ = self.ii_spawn(voice, ev.gain * BASS_TRIM * level, pan);
        let p = &mut self.ii.parts[idx];
        p.last_gain = ev.gain;
        p.last_pan = ev.pan;
    }

    /// A key at a prompt that does not echo: one unpitched tick, and nothing learned from it.
    const fn ii_secret(&mut self, ev: &SoundEvent) {
        let voice = tine(
            TINE_BASE_HZ,
            Touch::Step,
            TAU_MIN_S,
            ROOF_INNER_HZ,
            true,
            0.0,
        );
        let _ = self.ii_spawn(voice, ev.gain * TUNE_TRIM * SECRET_LEVEL, ev.pan);
    }

    /// An arrow key: one quiet note `step` degrees along the scale from where the walk stands.
    const fn ii_nav(&mut self, ev: &SoundEvent, meta: EventMeta, step: i32) {
        let idx = self.ii.part_index(meta.session);
        let p = &mut self.ii.parts[idx];
        p.nav = maxi(TUNE_LO - p.last, mini(p.nav + step, TUNE_HI - p.last));
        let deg = p.last + p.nav;
        let level = NAV_LEVEL * (1.0 - TIRED_LEVEL_COST * self.ii.fatigue());
        let voice = tine(
            degree_hz(deg),
            Touch::Step,
            NAV_TAU_S,
            ROOF_INNER_HZ,
            false,
            0.0,
        );
        let _ = self.ii_spawn(voice, ev.gain * TUNE_TRIM * level, ev.pan);
    }

    /// A jump of the cursor that no key of the tune made (a completion, a redraw): the chord tone
    /// above the last note, once. Enter's own line feed is not sounded again.
    const fn ii_jump(&mut self, ev: &SoundEvent, meta: EventMeta) {
        let at = self.ii_at_ms(meta);
        let idx = self.ii.part_index(meta.session);
        let p = self.ii.parts[idx];
        if p.entered && at.wrapping_sub(p.enter_at) < ENTER_ECHO_MS {
            return;
        }
        let up = chord_tone_above(p.chord, p.last + 1);
        let deg = if up > TUNE_HI {
            chord_tone_below(p.chord, TUNE_HI)
        } else {
            up
        };
        let level = JUMP_LEVEL * (1.0 - TIRED_LEVEL_COST * self.ii.fatigue());
        let voice = tine(
            degree_hz(deg),
            Touch::Step,
            TAU_MIN_S,
            ROOF_INNER_HZ,
            false,
            0.0,
        );
        let _ = self.ii_spawn(voice, ev.gain * TUNE_TRIM * level, ev.pan);
    }

    /// A paste: the chord, swept upward once.
    const fn ii_strum(&mut self, ev: &SoundEvent, meta: EventMeta) {
        let idx = self.ii.part_index(meta.session);
        let chord = self.ii.parts[idx].chord;
        let mut deg = chord_tone_above(chord, 0);
        let mut n = 0;
        while n < 4 {
            let mut voice = tine(
                degree_hz(deg),
                Touch::Step,
                TAU_MAX_S,
                ROOF_HEAD_HZ,
                false,
                0.0,
            );
            voice.delay = STRUM_STEP_S * n as f32;
            voice.lane = LANE_CASCADE;
            let _ = self.ii_spawn(voice, ev.gain * TUNE_TRIM * STRUM_LEVEL, ev.pan);
            deg = chord_tone_above(chord, deg + 1);
            n += 1;
        }
    }

    /// The result of the command Enter ran in `meta.session`: the answer to the dominant that
    /// Enter stopped on, at the result's own gain and never above the last key's.
    const fn ii_verdict(&mut self, ev: &SoundEvent, meta: EventMeta, failed: bool) {
        let Some(idx) = self.ii.find(meta.session) else {
            return;
        };
        let p = self.ii.parts[idx];
        if !p.awaiting {
            return;
        }
        let since = self.ii_at_ms(meta).wrapping_sub(p.enter_at);
        if since > AWAIT_MS {
            self.ii.parts[idx].awaiting = false;
            return;
        }
        let (bass, tune) = self.ii.parts[idx].resolve(failed);
        let wait = CADENCE_MIN_MS.saturating_sub(since) as f32 * 0.001;
        let gain = ev.gain.min(p.last_gain) * (1.0 - TIRED_LEVEL_COST * self.ii.fatigue());
        let f = degree_hz(bass.deg);
        let mut voice = bass_dyad(f, f * 1.5, bass.tau, wait);
        if !bass.dyad {
            voice.p[1].lvl = 0.0;
        }
        let pan = self.ii_pan(p.last_pan * 0.5);
        let _ = self.ii_spawn(voice, gain * BASS_TRIM * bass.level, pan);
        let Some(deg) = tune else { return };
        let roof = if failed { ROOF_FAIL_HZ } else { ROOF_HEAD_HZ };
        let mut voice = tine(degree_hz(deg), Touch::Step, ANSWER_TAU_S, roof, false, 0.0);
        voice.delay = wait + CADENCE_AFTER_S;
        let pan = self.ii_pan(p.last_pan);
        let _ = self.ii_spawn(voice, gain * TUNE_TRIM * CADENCE_LEVEL, pan);
    }

    /// `dt` seconds of audio are about to be rendered. Once [`REST_MS`] of it has followed the
    /// last key, the phrase of the session typed in last ends; where it was left hanging, one
    /// quiet note steps home, once, on the sample the rest is reached.
    ///
    /// A part that waits on a command's result with nothing typed since rests only once
    /// [`AWAIT_MS`] have passed.
    pub(super) const fn ii_elapse(&mut self, dt: f32) {
        if !self.ii.any {
            return;
        }
        let before = self.ii.idle_s;
        self.ii.idle_s = before + dt;
        let idx = self.ii.active;
        let p = self.ii.parts[idx];
        if !p.live || p.rested || p.keys == 0 || self.ii.idle_s < REST_S {
            return;
        }
        if p.awaiting && p.line_keys == 0 {
            // Nothing was typed since Enter: the dominant hangs until the result or the limit.
            if self.ii.idle_s < AWAIT_MS as f32 * 0.001 {
                return;
            }
            self.ii.parts[idx].awaiting = false;
        }
        let Some(deg) = self.ii.parts[idx].rest() else {
            return;
        };
        let level = ANSWER_LEVEL * (1.0 - TIRED_LEVEL_COST * self.ii.fatigue());
        let mut voice = tine(
            degree_hz(deg),
            Touch::Step,
            ANSWER_TAU_S,
            ROOF_INNER_HZ,
            false,
            0.0,
        );
        voice.delay = (REST_S - before).max(0.0);
        let _ = self.ii_spawn(voice, p.last_gain * TUNE_TRIM * level, p.last_pan);
    }
}

#[cfg(test)]
#[path = "music_box_ii_tests.rs"]
mod tests;
