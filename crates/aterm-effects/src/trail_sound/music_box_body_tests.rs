// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const SR: f32 = 48_000.0;
const BLOCK: usize = 512;
/// The synth's key rate while nobody types, and one well past the rate that counts as fast.
const SLOW: f32 = 0.0;
const FAST: f32 = 20.0;

/// A body at `sr` whose ring has settled for typing at `rate`.
fn body_at(sr: f32, rate: f32) -> Body {
    let mut b = Body::new(sr);
    for _ in 0..200 {
        b.begin_block(rate);
    }
    b
}

fn body(rate: f32) -> Body {
    body_at(SR, rate)
}

/// Run `frames` of `input` (a function of the frame number, the same on both sides) through `b`
/// in blocks, typing at `rate`, and return the left output.
fn run(b: &mut Body, rate: f32, frames: usize, input: impl Fn(usize) -> f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames);
    let mut n = 0;
    while n < frames {
        b.begin_block(rate);
        let len = BLOCK.min(frames - n);
        for i in n..n + len {
            let x = input(i);
            let (l, r) = b.tick(x, x);
            assert!(l.is_finite() && r.is_finite());
            out.push(l);
        }
        b.end_block(len);
        n += len;
    }
    out
}

fn tone(hz: f32) -> impl Fn(usize) -> f32 {
    move |n| (core::f64::consts::TAU * f64::from(hz) * n as f64 / f64::from(SR)).sin() as f32
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, s| m.max(s.abs()))
}

fn energy(x: &[f32]) -> f64 {
    x.iter().map(|s| f64::from(*s) * f64::from(*s)).sum()
}

fn note(k: usize) -> f32 {
    degree_hz(TINE_LO + k as i32)
}

/// A complex number, for the gain of a filter at one frequency.
#[derive(Clone, Copy, Debug)]
struct Cx(f64, f64);

impl Cx {
    const ONE: Self = Self(1.0, 0.0);
    /// `z` to the power `-n` at `w` radians a frame.
    fn back(w: f64, n: f64) -> Self {
        Self((w * n).cos(), -(w * n).sin())
    }
    fn plus(self, o: Self) -> Self {
        Self(self.0 + o.0, self.1 + o.1)
    }
    fn times(self, o: Self) -> Self {
        Self(self.0 * o.0 - self.1 * o.1, self.0 * o.1 + self.1 * o.0)
    }
    fn by(self, k: f64) -> Self {
        Self(self.0 * k, self.1 * k)
    }
    fn over(self, o: Self) -> Self {
        let d = o.0 * o.0 + o.1 * o.1;
        Self(
            (self.0 * o.0 + self.1 * o.1) / d,
            (self.1 * o.0 - self.0 * o.1) / d,
        )
    }
    fn abs(self) -> f64 {
        self.0.hypot(self.1)
    }
}

/// A resonator's gain at `w`, from its coefficients: `g (1 - z^-2) / (1 - a1 z^-1 + a2 z^-2)`.
fn mode_gain(m: &Mode, w: f64) -> Cx {
    let (z1, z2) = (Cx::back(w, 1.0), Cx::back(w, 2.0));
    Cx::ONE.plus(z2.by(-1.0)).by(f64::from(m.g)).over(
        Cx::ONE
            .plus(z1.by(-f64::from(m.a1)))
            .plus(z2.by(f64::from(m.a2))),
    )
}

/// One side of the room's gain at `w`, from the body's coefficients, derived apart from `parts`:
/// the send's low-pass, the mean of the damped combs, then the all-pass stages.
fn room_gain(b: &Body, side: usize, w: f64) -> Cx {
    let z1 = Cx::back(w, 1.0);
    let low = |k: f32| Cx(f64::from(k), 0.0).over(Cx::ONE.plus(z1.by(-(1.0 - f64::from(k)))));
    let mut combs = Cx(0.0, 0.0);
    for k in 0..COMBS {
        let zd = Cx::back(w, f64::from(b.comb_len[side][k]));
        let fb = f64::from(b.comb_fb[side][k]);
        let comb = zd.over(Cx::ONE.plus(low(b.damp_k).times(zd).by(-fb)));
        combs = combs.plus(comb.by(1.0 - fb));
    }
    let mut gain = low(b.send_k).times(combs.by(1.0 / COMBS as f64));
    for &len in b.stage_len[side].iter().take(STAGES) {
        let zd = Cx::back(w, f64::from(len));
        let g = f64::from(STAGE_GAIN);
        gain = gain.times(zd.plus(Cx(-g, 0.0)).over(Cx::ONE.plus(zd.by(-g))));
    }
    gain
}

/// The whole body's gain on `side` for a centred tone of `hz`.
fn gain(b: &Body, side: usize, hz: f32) -> f64 {
    let w = core::f64::consts::TAU * f64::from(hz) / f64::from(b.sr);
    let mut total = Cx(f64::from(DRY), 0.0);
    for m in &b.tines {
        total = total.plus(mode_gain(m, w).by(f64::from(b.comb_gain)));
    }
    for (m, (_, _, share)) in b.board.iter().zip(BOARD) {
        total = total.plus(mode_gain(m, w).by(f64::from(BOARD_GAIN * share)));
    }
    total
        .plus(room_gain(b, side, w).by(f64::from(b.send)))
        .abs()
}

#[test]
fn every_tine_peaks_at_one_in_phase_on_its_own_note() {
    assert!((BOARD.iter().map(|m| m.2).sum::<f32>() - 1.0).abs() < 1e-6);
    for secs in [RING_SLOW_S, RING_FAST_S] {
        for k in 0..TINES {
            let hz = note(k);
            let m = Mode::OFF.tuned(hz, secs, SR);
            let at = |f: f32| mode_gain(&m, core::f64::consts::TAU * f64::from(f) / f64::from(SR));
            let on = at(hz);
            assert!(
                (on.abs() - 1.0).abs() < 2e-3 && on.1.abs() < 5e-2,
                "tine {k} at {hz} Hz: {on:?}"
            );
            assert!(
                at(hz * 1.004).abs() < on.abs() && at(hz / 1.004).abs() < on.abs(),
                "tine {k} does not peak on its note"
            );
        }
    }
}

#[test]
fn each_part_keeps_to_its_share_at_every_frequency_and_sample_rate() {
    const {
        assert!(
            DRY + COMB_GAIN * COMB_PEAK + BOARD_GAIN + ROOM_SEND <= 1.0 + 1e-6,
            "the shares sum to more than one"
        );
    }
    for sr in [SR, 44_100.0, 96_000.0, 22_050.0] {
        for rate in [SLOW, FAST] {
            let b = body_at(sr, rate);
            let (mut comb, mut board, mut room) = (0.0f64, 0.0f64, 0.0f64);
            // Every cent from 20 Hz to 20 kHz, and every note exactly.
            let sweep = (0..12_000).map(|cent| 20.0 * 2f32.powf(cent as f32 / 1_200.0));
            for hz in sweep
                .chain((0..TINES).map(note))
                .filter(|hz| *hz < sr * 0.49)
            {
                let w = core::f64::consts::TAU * f64::from(hz) / f64::from(sr);
                // The comb is one filter: its tines' gains add as complex numbers.
                let tines = b
                    .tines
                    .iter()
                    .fold(Cx(0.0, 0.0), |sum, m| sum.plus(mode_gain(m, w)));
                comb = comb.max(tines.abs());
                board = board.max(
                    b.board
                        .iter()
                        .zip(BOARD)
                        .map(|(m, (_, _, share))| mode_gain(m, w).abs() * f64::from(share))
                        .sum(),
                );
                room = room.max(room_gain(&b, 0, w).abs().max(room_gain(&b, 1, w).abs()));
            }
            assert!(
                comb <= f64::from(COMB_PEAK) && comb > f64::from(COMB_PEAK) * 0.8,
                "the tines sum to {comb} ({sr} Hz audio, rate {rate})"
            );
            assert!(board <= 1.001, "the board reaches {board} ({sr} Hz audio)");
            assert!(
                room <= 1.001,
                "the room reaches {room} ({sr} Hz audio, rate {rate})"
            );
        }
    }
}

#[test]
fn the_gain_worked_out_from_the_coefficients_is_the_gain_the_body_has() {
    let tooth = SR / f32::from(COMB_FRAMES[0][2]) * 7.0;
    for rate in [SLOW, FAST] {
        for hz in [note(9), note(14) * 1.01, tooth, 97.0, 3_000.0] {
            let mut b = body(rate);
            let out = run(&mut b, rate, (SR * 1.2) as usize, tone(hz));
            let measured = f64::from(peak(&out[out.len() - (SR * 0.2) as usize..]));
            let worked = gain(&b, 0, hz);
            assert!(
                (measured / worked - 1.0).abs() < 0.01,
                "{hz} Hz at rate {rate}: measured {measured}, worked out {worked}"
            );
        }
    }
}

#[test]
fn no_note_and_no_steady_tone_comes_out_larger_than_it_went_in() {
    for sr in [SR, 44_100.0, 96_000.0] {
        for rate in [SLOW, FAST] {
            let b = body_at(sr, rate);
            for side in 0..2 {
                let notes: Vec<f64> = (0..TINES).map(|k| gain(&b, side, note(k))).collect();
                let (lo, hi) = notes
                    .iter()
                    .fold((f64::MAX, 0.0f64), |(lo, hi), g| (lo.min(*g), hi.max(*g)));
                assert!(
                    hi <= 1.0,
                    "a note comes out at {hi} ({sr} Hz audio, rate {rate})"
                );
                assert!(lo >= 0.7, "a note is swallowed: {lo}");
                // The room's delays are fitted to the notes up to the app's 48 kHz; above it the
                // room is shorter than fitted, and only the bound holds.
                assert!(
                    sr > SR || 20.0 * (hi / lo).log10() < 1.0,
                    "the notes are uneven: {lo} to {hi} ({sr} Hz audio, rate {rate})"
                );
                // Every cent from 20 Hz to 20 kHz.
                let mut worst = (0.0f64, 0.0f32);
                for cent in 0..12_000 {
                    let hz = 20.0 * 2f32.powf(cent as f32 / 1_200.0);
                    if hz < sr * 0.49 {
                        let g = gain(&b, side, hz);
                        if g > worst.0 {
                            worst = (g, hz);
                        }
                    }
                }
                assert!(
                    worst.0 <= 1.0,
                    "{} Hz comes out at {} ({sr} Hz audio, rate {rate})",
                    worst.1,
                    worst.0
                );
            }
        }
    }
}

#[test]
fn the_room_treats_the_instruments_notes_alike() {
    for rate in [SLOW, FAST] {
        let b = body(rate);
        for side in 0..2 {
            let room: Vec<f64> = (0..TINES)
                .map(|k| {
                    let w = core::f64::consts::TAU * f64::from(note(k)) / f64::from(SR);
                    room_gain(&b, side, w).abs()
                })
                .collect();
            let (lo, hi) = room
                .iter()
                .fold((f64::MAX, 0.0f64), |(lo, hi), g| (lo.min(*g), hi.max(*g)));
            assert!(
                20.0 * (hi / lo).log10() < 7.0,
                "side {side}, rate {rate}: the room gives {lo} to {hi}"
            );
        }
    }
}

#[test]
fn the_body_rings_after_the_note_and_then_is_exactly_silent() {
    let burst = (SR * 0.05) as usize;
    let hz = note(9);
    let struck = move |n: usize| if n < burst { tone(hz)(n) * 0.2 } else { 0.0 };
    let mut b = body(SLOW);
    assert!(b.idle(), "a new body holds something");
    let out = run(&mut b, SLOW, (SR * 2.0) as usize, struck);
    // It rings on: a tenth of a second after the note there is still sound.
    let after = &out[burst + (SR * 0.1) as usize..burst + (SR * 0.15) as usize];
    assert!(peak(after) > 1e-3, "the ring is not heard: {}", peak(after));
    // And it ends: idle inside a second, and from there exact zeros.
    let last = out
        .iter()
        .rposition(|s| *s != 0.0)
        .expect("the note made no sound");
    assert!(
        last < burst + (SR * 0.9) as usize,
        "still sounding {} s after the note",
        (last - burst) as f32 / SR
    );
    assert!(b.idle());
    assert_eq!(b.tick(0.0, 0.0), (0.0, 0.0));
    assert!(b.comb.iter().flatten().flatten().all(|s| *s == 0.0));
    assert!(b.stage.iter().flatten().flatten().all(|s| *s == 0.0));
    assert!(
        b.tines
            .iter()
            .chain(&b.board)
            .all(|m| m.y1 == 0.0 && m.y2 == 0.0)
    );
    // The tail only ever falls: no tenth of a second has more energy than the one before it.
    let tenth = (SR * 0.1) as usize;
    let tail: Vec<f64> = out[burst..].chunks(tenth).map(energy).collect();
    assert!(tail.windows(2).all(|w| w[1] <= w[0]), "{tail:?}");
}

#[test]
fn fast_typing_gets_less_of_the_body_and_a_shorter_ring() {
    let burst = (SR * 0.03) as usize;
    let hz = note(11);
    let struck = move |n: usize| if n < burst { tone(hz)(n) * 0.2 } else { 0.0 };
    let tail = |rate: f32| {
        let mut b = body(rate);
        let out = run(&mut b, rate, (SR * 1.5) as usize, struck);
        let from = burst + (SR * 0.02) as usize;
        let early = energy(&out[from..from + (SR * 0.05) as usize]);
        let late = energy(&out[from + (SR * 0.15) as usize..from + (SR * 0.20) as usize]);
        (early, late / early)
    };
    let (slow, slow_fall) = tail(SLOW);
    let (fast, fast_fall) = tail(FAST);
    assert!(
        fast < slow * 0.6,
        "fast typing gets as much: {fast} against {slow}"
    );
    assert!(
        fast_fall < slow_fall * 0.5,
        "the ring is no shorter: {fast_fall} against {slow_fall}"
    );
}

#[test]
fn the_room_is_dark() {
    let b = body(SLOW);
    let at = |hz: f32| {
        room_gain(
            &b,
            0,
            core::f64::consts::TAU * f64::from(hz) / f64::from(SR),
        )
        .abs()
    };
    // The mean over a third of an octave, so no one tooth of a comb decides it.
    let band = |hz: f32| {
        (0..40)
            .map(|i| at(hz * 2f32.powf(i as f32 / 120.0)))
            .sum::<f64>()
            / 40.0
    };
    let (low, high) = (band(500.0), band(7_000.0));
    assert!(
        high < low * 0.45,
        "the highs get {high} of the room, the lows {low}"
    );
    // And what highs there are die sooner: a comb's feedback meets the damping every lap.
    let burst = (SR * 0.05) as usize;
    let fall = |hz: f32| {
        let struck = move |n: usize| if n < burst { tone(hz)(n) * 0.2 } else { 0.0 };
        let mut b = body(SLOW);
        let out = run(&mut b, SLOW, (SR * 0.6) as usize, struck);
        let from = burst + (SR * 0.04) as usize;
        energy(&out[from + (SR * 0.12) as usize..from + (SR * 0.20) as usize])
            / energy(&out[from..from + (SR * 0.08) as usize])
    };
    // Away from every tine and board resonance, so the room alone is heard.
    assert!(
        fall(6_500.0) < fall(1_700.0) * 0.5,
        "{} against {}",
        fall(6_500.0),
        fall(1_700.0)
    );
}

#[test]
fn any_sample_rate_falls_silent() {
    for sr in [44_100.0f32, 96_000.0, 8_000.0] {
        let mut b = Body::new(sr);
        for block in 0..(sr as usize * 3 / BLOCK) {
            b.begin_block(SLOW);
            for n in 0..BLOCK {
                let x = if block < 20 {
                    ((block * BLOCK + n) as f32 * 0.05).sin() * 0.2
                } else {
                    0.0
                };
                let (l, r) = b.tick(x, -x);
                assert!(l.is_finite() && r.is_finite());
            }
            b.end_block(BLOCK);
        }
        assert!(b.idle(), "{sr} Hz audio never fell silent");
    }
}
