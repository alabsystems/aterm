// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The music box's body: what every note of [`super::music_box_ii`] sounds through, so the notes
//! are one instrument in one place.
//!
//! The notes are summed on a stereo bus and the body adds to them, feed-forward only:
//!
//! - **The comb** is one resonator for each of the instrument's [`TINES`] notes, tuned exactly to
//!   it. A note rings its own tine in sympathy, in phase with itself, and the tine rings on for a
//!   quarter to four tenths of a second after the note is gone: notes run into each other, and a
//!   note's octave answers its overtone.
//! - **The soundboard** is [`MODES`] broad resonances of a small wooden box, which the mallet's
//!   knock rings.
//! - **The room** is four damped combs and two all-pass stages a side, as long as the comb's
//!   ring and dark (its highs die first). Its delays are chosen so that each of the instrument's
//!   notes gets about the same room ([`COMB_FRAMES`]).
//!
//! The faster the typing, the shorter the ring and the less of the comb and the room there is,
//! so a quick passage stays clear and the tail blooms in a pause.
//!
//! **The level bound.** Each resonator has a peak gain of exactly one, at its own frequency and
//! in phase with it, so the board's gain is at most one; the comb's tines together never exceed
//! [`COMB_PEAK`] (a note's own tine, and its neighbours' skirts); each room comb is scaled to a
//! gain of at most one at every frequency. The shares of the output ([`DRY`], [`COMB_GAIN`]
//! times [`COMB_PEAK`], [`BOARD_GAIN`], [`ROOM_SEND`]) sum to one. So a steady tone at any
//! frequency, at any sample rate, comes out no larger than it went in. At the app's 48 kHz and
//! under, where the room's delays are the fitted ones, the instrument's own notes come out
//! within a decibel of each other.
//!
//! **Silence.** Once the bus has been silent and everything the body holds has fallen under
//! [`FLOOR`] for longer than its longest delay, the body empties itself and reports idle: the
//! output is exact zeros, and the render skips it.

use super::music_box_ii::degree_hz;

/// The comb's tines: one for each scale degree from [`TINE_LO`], the bass register's lowest note,
/// to the tune register's highest.
pub(super) const TINES: usize = 20;
pub(super) const TINE_LO: i32 = -9;
/// The soundboard's resonances.
pub(super) const MODES: usize = 6;
/// Each resonance: its frequency in hertz, its ring time in seconds (to −60 dB), and its share of
/// the board's gain. The shares sum to one.
const BOARD: [(f32, f32, f32); MODES] = [
    (196.0, 0.110, 0.22),
    (311.0, 0.090, 0.22),
    (494.0, 0.070, 0.18),
    (784.0, 0.060, 0.16),
    (1_245.0, 0.050, 0.12),
    (1_976.0, 0.040, 0.10),
];
/// The shares of the output: the notes themselves, the most the comb gives, the board, and the
/// most the room is sent.
pub(super) const DRY: f32 = 0.66;
pub(super) const COMB_GAIN: f32 = 0.15;
pub(super) const BOARD_GAIN: f32 = 0.06;
pub(super) const ROOM_SEND: f32 = 0.10;
/// The most the tines' gains sum to at any one frequency, whatever the ring.
pub(super) const COMB_PEAK: f32 = 1.2;
// The level bound: the shares, each at its most, are the whole of the output and no more.
const _: () = assert!(DRY + COMB_GAIN * COMB_PEAK + BOARD_GAIN + ROOM_SEND <= 1.000_001);
/// How much of the comb and of the room's send fast typing takes away.
const COMB_FAST_CUT: f32 = 0.4;
const ROOM_FAST_CUT: f32 = 0.6;
/// The ring of the comb and the room (to −60 dB) for slow typing and for fast.
pub(super) const RING_SLOW_S: f32 = 0.40;
pub(super) const RING_FAST_S: f32 = 0.25;
/// The room's combs a side.
const COMBS: usize = 4;
/// Its all-pass stages a side.
const STAGES: usize = 2;
/// The room's comb lengths at 48 kHz, in frames, left then right: 17 to 33 ms, no two within six
/// percent of each other, and of 1.5 million sets tried the two under which the room's gain
/// varies least from one of the instrument's notes to another (under 6 dB; an arbitrary set of
/// primes varied 18 dB).
pub(super) const COMB_FRAMES: [[u16; COMBS]; 2] =
    [[799, 869, 1_262, 1_422], [887, 1_035, 1_201, 1_570]];
const STAGE_FRAMES: [[u16; STAGES]; 2] = [[241, 83], [251, 89]];
/// The longest of each the body has room for (a higher sample rate than 48 kHz gets a room this long).
const COMB_MAX: usize = 1_600;
const STAGE_MAX: usize = 256;
const STAGE_GAIN: f32 = 0.5;
/// Where the room's send and the combs' feedback lose their highs, in hertz.
const SEND_LP_HZ: f32 = 3_500.0;
const DAMP_HZ: f32 = 2_600.0;
/// Under this, on the bus and in everything held, the body is silent.
pub(super) const FLOOR: f32 = 3e-6;
/// How fast the body follows the typing speed, a block.
const SPEED_SLEW: f32 = 0.1;
/// The key rate (the synth's decaying count) from which typing is quick, and the span to fully fast.
const RATE_SLOW: f32 = 2.0;
const RATE_SPAN: f32 = 5.0;
const LN_10_X3: f32 = 6.907_755_4;

/// One resonator: `y = g (x - x2) + a1 y1 - a2 y2`, whose gain peaks at exactly one.
#[derive(Clone, Copy, Debug)]
struct Mode {
    g: f32,
    a1: f32,
    a2: f32,
    y1: f32,
    y2: f32,
}

impl Mode {
    const OFF: Self = Self {
        g: 0.0,
        a1: 0.0,
        a2: 0.0,
        y1: 0.0,
        y2: 0.0,
    };

    /// A resonator that peaks at `hz`, at one and in phase, and rings `secs`; what it holds is kept.
    /// A frequency above a third of the sample rate gets none (its gain stays zero).
    const fn tuned(self, hz: f32, secs: f32, sr: f32) -> Self {
        if hz >= sr / 3.0 {
            return Self::OFF;
        }
        let r = ring(secs, 1.0, sr);
        let (_, cos) = crate::supernova::sin_cos_f32(core::f32::consts::TAU * hz / sr);
        Self {
            g: (1.0 - r * r) * 0.5,
            // The pole's angle that puts the peak of the gain, not the pole, on `hz`.
            a1: 4.0 * r * r / (1.0 + r * r) * cos,
            a2: r * r,
            ..self
        }
    }

    #[inline]
    const fn tick(&mut self, edge: f32) -> f32 {
        let y = self.g * edge + self.a1 * self.y1 - self.a2 * self.y2;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// The body's whole state.
#[derive(Clone)]
pub(super) struct Body {
    sr: f32,
    tines: [Mode; TINES],
    board: [Mode; MODES],
    /// The bus's mid signal, one and two frames back, which every resonator reads.
    m1: f32,
    m2: f32,
    comb: [[[f32; COMB_MAX]; COMBS]; 2],
    comb_len: [[u16; COMBS]; 2],
    comb_pos: [[u16; COMBS]; 2],
    comb_lp: [[f32; COMBS]; 2],
    comb_fb: [[f32; COMBS]; 2],
    stage: [[[f32; STAGE_MAX]; STAGES]; 2],
    stage_len: [[u16; STAGES]; 2],
    stage_pos: [[u16; STAGES]; 2],
    send_lp: [f32; 2],
    send_k: f32,
    damp_k: f32,
    comb_gain: f32,
    send: f32,
    /// How quick the typing is, 0..=1, smoothed.
    speed: f32,
    /// The ring the comb and the room were last set for.
    set_for_s: f32,
    /// The largest magnitude seen since the block began, on the bus or held.
    act: f32,
    /// Frames in a row under [`FLOOR`].
    quiet: u32,
    idle: bool,
}

impl core::fmt::Debug for Body {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Body")
            .field("idle", &self.idle)
            .field("speed", &self.speed)
            .field("send", &self.send)
            .finish_non_exhaustive()
    }
}

const fn one_pole(hz: f32, sr: f32) -> f32 {
    1.0 - crate::color_math::exp_f32(-core::f32::consts::TAU * hz / sr)
}

/// The per-frame factor of a ring that falls 60 dB in `secs`.
const fn ring(secs: f32, frames: f32, sr: f32) -> f32 {
    crate::color_math::exp_f32(-LN_10_X3 * frames / (sr * secs))
}

const fn scaled(frames: u16, sr: f32, max: usize) -> u16 {
    let n = (frames as f32 * sr / 48_000.0 + 0.5) as usize;
    let n = if n < 1 { 1 } else { n };
    (if n > max { max } else { n }) as u16
}

impl Body {
    /// An empty body for audio at `sample_rate`.
    pub(super) const fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(8_000.0);
        let mut board = [Mode::OFF; MODES];
        let mut k = 0;
        while k < MODES {
            board[k] = Mode::OFF.tuned(BOARD[k].0, BOARD[k].1, sr);
            k += 1;
        }
        let mut comb_len = [[0u16; COMBS]; 2];
        let mut stage_len = [[0u16; STAGES]; 2];
        let mut c = 0;
        while c < 2 {
            let mut k = 0;
            while k < COMBS {
                comb_len[c][k] = scaled(COMB_FRAMES[c][k], sr, COMB_MAX);
                k += 1;
            }
            let mut k = 0;
            while k < STAGES {
                stage_len[c][k] = scaled(STAGE_FRAMES[c][k], sr, STAGE_MAX);
                k += 1;
            }
            c += 1;
        }
        let mut body = Self {
            sr,
            tines: [Mode::OFF; TINES],
            board,
            m1: 0.0,
            m2: 0.0,
            comb: [[[0.0; COMB_MAX]; COMBS]; 2],
            comb_len,
            comb_pos: [[0; COMBS]; 2],
            comb_lp: [[0.0; COMBS]; 2],
            comb_fb: [[0.0; COMBS]; 2],
            stage: [[[0.0; STAGE_MAX]; STAGES]; 2],
            stage_len,
            stage_pos: [[0; STAGES]; 2],
            send_lp: [0.0; 2],
            send_k: one_pole(SEND_LP_HZ, sr),
            damp_k: one_pole(DAMP_HZ, sr),
            comb_gain: COMB_GAIN,
            send: ROOM_SEND,
            speed: 0.0,
            set_for_s: 0.0,
            act: 0.0,
            quiet: 0,
            idle: true,
        };
        body.set_ring(RING_SLOW_S);
        body
    }

    /// Nothing is held: the body's output for a silent bus is exact zeros.
    #[inline]
    pub(super) const fn idle(&self) -> bool {
        self.idle
    }

    /// Set the comb's tines and the room's feedback to ring `secs`.
    const fn set_ring(&mut self, secs: f32) {
        self.set_for_s = secs;
        let mut k = 0;
        while k < TINES {
            self.tines[k] = self.tines[k].tuned(degree_hz(TINE_LO + k as i32), secs, self.sr);
            k += 1;
        }
        let mut c = 0;
        while c < 2 {
            let mut k = 0;
            while k < COMBS {
                self.comb_fb[c][k] = ring(secs, self.comb_len[c][k] as f32, self.sr);
                k += 1;
            }
            c += 1;
        }
    }

    /// Before a block: follow the typing speed (`rate` is the synth's decaying count of keys).
    pub(super) const fn begin_block(&mut self, rate: f32) {
        let target = ((rate - RATE_SLOW) / RATE_SPAN).clamp(0.0, 1.0);
        self.speed += (target - self.speed) * SPEED_SLEW;
        self.comb_gain = COMB_GAIN * (1.0 - COMB_FAST_CUT * self.speed);
        self.send = ROOM_SEND * (1.0 - ROOM_FAST_CUT * self.speed);
        let secs = RING_SLOW_S + (RING_FAST_S - RING_SLOW_S) * self.speed;
        if (secs - self.set_for_s).abs() > 0.005 {
            self.set_ring(secs);
        }
        self.act = 0.0;
    }

    /// One frame of the bus in: what the comb, the board and each side of the room make of it,
    /// before their shares of the output.
    #[inline]
    const fn parts(&mut self, x: [f32; 2]) -> (f32, f32, [f32; 2]) {
        let mut act = self.act;
        let mid = 0.5 * (x[0] + x[1]);
        let edge = mid - self.m2;
        self.m2 = self.m1;
        self.m1 = mid;
        let mut comb = 0.0f32;
        let mut k = 0;
        while k < TINES {
            let y = self.tines[k].tick(edge);
            comb += y;
            act = act.max(y.abs());
            k += 1;
        }
        let mut board = 0.0f32;
        let mut k = 0;
        while k < MODES {
            let y = self.board[k].tick(edge);
            board += BOARD[k].2 * y;
            act = act.max(y.abs());
            k += 1;
        }
        let mut rooms = [0.0f32; 2];
        let mut c = 0;
        while c < 2 {
            let xin = x[c];
            self.send_lp[c] += self.send_k * (xin - self.send_lp[c]);
            let sent = self.send_lp[c];
            let mut wet = 0.0f32;
            let mut k = 0;
            while k < COMBS {
                let pos = self.comb_pos[c][k] as usize;
                let held = self.comb[c][k][pos];
                let fb = self.comb_fb[c][k];
                self.comb_lp[c][k] += self.damp_k * (held - self.comb_lp[c][k]);
                self.comb[c][k][pos] = sent + fb * self.comb_lp[c][k];
                self.comb_pos[c][k] = if pos + 1 >= self.comb_len[c][k] as usize {
                    0
                } else {
                    pos as u16 + 1
                };
                // Scaled by `1 - fb`, a comb's gain is at most one.
                wet += held * (1.0 - fb);
                act = act.max(held.abs());
                k += 1;
            }
            wet *= 1.0 / COMBS as f32;
            let mut k = 0;
            while k < STAGES {
                let pos = self.stage_pos[c][k] as usize;
                let held = self.stage[c][k][pos];
                let y = held - STAGE_GAIN * wet;
                self.stage[c][k][pos] = wet + STAGE_GAIN * y;
                self.stage_pos[c][k] = if pos + 1 >= self.stage_len[c][k] as usize {
                    0
                } else {
                    pos as u16 + 1
                };
                act = act.max(held.abs());
                wet = y;
                k += 1;
            }
            act = act.max(xin.abs()).max(sent.abs());
            rooms[c] = wet;
            c += 1;
        }
        self.act = act;
        (comb, board, rooms)
    }

    /// One frame of the bus in, one frame of the instrument out.
    #[inline]
    pub(super) const fn tick(&mut self, l: f32, r: f32) -> (f32, f32) {
        let (comb, board, room) = self.parts([l, r]);
        let shared = self.comb_gain * comb + BOARD_GAIN * board;
        (
            DRY * l + shared + self.send * room[0],
            DRY * r + shared + self.send * room[1],
        )
    }

    /// After a block of `frames`: once all has been under [`FLOOR`] for longer than the longest
    /// delay, empty the body.
    pub(super) const fn end_block(&mut self, frames: usize) {
        if self.act >= FLOOR {
            self.quiet = 0;
            self.idle = false;
            return;
        }
        if self.idle {
            return;
        }
        self.quiet = self.quiet.saturating_add(frames as u32);
        if self.quiet as usize >= COMB_MAX + 2 * STAGE_MAX {
            let (speed, ring) = (self.speed, self.set_for_s);
            *self = Self::new(self.sr);
            self.speed = speed;
            self.set_ring(ring);
        }
    }
}

#[cfg(test)]
#[path = "music_box_body_tests.rs"]
mod tests;
