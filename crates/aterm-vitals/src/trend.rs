// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One quantity over time: its level, momentum and variance (design §12.1), and
//! a bounded history of what it read.
//!
//! * **Level** is the fast average (τ 2 s), so one noisy sample does not jump
//!   a tile.
//! * **Momentum** is fast − slow (τ 20 s): positive while the quantity climbs
//!   above where it has been, negative while it eases back.
//! * **Variance** is the median absolute deviation of the last
//!   [`MAD_WINDOW`] raw samples (10 s at 1 Hz): steady or jittery.
//!
//! Every average is time-weighted (`α = 1 − e^(−dt/τ)`), so a late or skipped
//! sample is folded in at its true age.

/// The fast average's time constant: the level the surfaces draw.
pub const FAST_TAU_S: f32 = 2.0;
/// The slow average's time constant: the ghost line momentum is measured from.
pub const SLOW_TAU_S: f32 = 20.0;
/// Raw samples the variance is taken over (10 s at the 1 s cadence).
pub const MAD_WINDOW: usize = 10;
/// Seconds a [`History`] keeps by default: two minutes.
pub const HISTORY_LEN: usize = 120;

/// A quantity's level, momentum and variance ([`Trend::view`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrendView {
    /// The last raw sample.
    pub raw: f32,
    /// The fast average (τ [`FAST_TAU_S`]): what a tile's height and a bar's
    /// length draw.
    pub level: f32,
    /// The slow average (τ [`SLOW_TAU_S`]): the dotted ghost.
    pub slow: f32,
    /// The median absolute deviation of the last [`MAD_WINDOW`] raw samples.
    pub mad: f32,
}

impl TrendView {
    /// A view of a quantity that has read only `v`.
    #[inline]
    #[must_use]
    pub const fn flat(v: f32) -> Self {
        Self {
            raw: v,
            level: v,
            slow: v,
            mad: 0.0,
        }
    }

    /// Momentum: the fast average less the slow one.
    #[inline]
    #[must_use]
    pub const fn momentum(&self) -> f32 {
        self.level - self.slow
    }

    /// Which way the quantity is moving, past a noise gate. The gate is the
    /// larger of `floor` (in the quantity's own units) and half the variance,
    /// so a jittery series has to move further before it reads as rising.
    #[inline]
    #[must_use]
    pub fn motion(&self, floor: f32) -> Motion {
        let gate = floor.max(self.mad * 0.5);
        let m = self.momentum();
        if m > gate {
            Motion::Rising
        } else if m < -gate {
            Motion::Falling
        } else {
            Motion::Steady
        }
    }

    /// The rise glow's step past the same gate as [`Self::motion`]: 0 (none)
    /// while not rising, then 1, 2 or 3 as momentum passes 1×, 2.5× and 5× the
    /// gate. Never for a falling quantity.
    #[inline]
    #[must_use]
    pub fn glow_step(&self, floor: f32) -> u8 {
        let gate = floor.max(self.mad * 0.5);
        if gate <= 0.0 {
            return 0;
        }
        let m = self.momentum();
        if m > gate * 5.0 {
            3
        } else if m > gate * 2.5 {
            2
        } else if m > gate {
            1
        } else {
            0
        }
    }
}

/// Which way a quantity is moving ([`TrendView::motion`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    /// The fast average is above the slow one by more than the gate.
    Rising,
    /// Below it by more than the gate.
    Falling,
    /// Within the gate.
    Steady,
}

impl Motion {
    /// The word the sentences print after a figure: `rising`, `easing`, or
    /// nothing while steady.
    #[inline]
    #[must_use]
    pub const fn word(self) -> Option<&'static str> {
        match self {
            Self::Rising => Some("rising"),
            Self::Falling => Some("easing"),
            Self::Steady => None,
        }
    }

    /// The glyph the compact figures carry: `▴`, `▾`, or nothing.
    #[inline]
    #[must_use]
    pub const fn glyph(self) -> Option<char> {
        match self {
            Self::Rising => Some('\u{25B4}'),
            Self::Falling => Some('\u{25BE}'),
            Self::Steady => None,
        }
    }
}

/// The running state behind a [`TrendView`].
#[derive(Clone, Debug, Default)]
pub struct Trend {
    fast: f32,
    slow: f32,
    last: f32,
    window: [f32; MAD_WINDOW],
    len: usize,
    head: usize,
    primed: bool,
}

impl Trend {
    /// A trend that has read nothing.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether it has read at least one sample.
    #[inline]
    #[must_use]
    pub const fn primed(&self) -> bool {
        self.primed
    }

    /// Fold in sample `v`, taken `dt_s` seconds after the previous one. The
    /// first sample seeds both averages, so a fresh series starts steady
    /// instead of rising from zero. A non-finite sample is ignored.
    pub fn push(&mut self, v: f32, dt_s: f32) {
        if !v.is_finite() {
            return;
        }
        if self.primed {
            let dt = if dt_s.is_finite() && dt_s > 0.0 {
                dt_s
            } else {
                0.0
            };
            let fast = 1.0 - (-dt / FAST_TAU_S).exp();
            let slow = 1.0 - (-dt / SLOW_TAU_S).exp();
            self.fast += fast * (v - self.fast);
            self.slow += slow * (v - self.slow);
        } else {
            self.fast = v;
            self.slow = v;
            self.primed = true;
        }
        self.last = v;
        self.window[self.head] = v;
        self.head = (self.head + 1) % MAD_WINDOW;
        self.len = (self.len + 1).min(MAD_WINDOW);
    }

    /// The level, momentum and variance now.
    #[must_use]
    pub fn view(&self) -> TrendView {
        TrendView {
            raw: self.last,
            level: self.fast,
            slow: self.slow,
            mad: mad(&self.window[..self.len]),
        }
    }
}

/// The median absolute deviation of `xs` (0 for fewer than two values).
#[must_use]
pub fn mad(xs: &[f32]) -> f32 {
    if xs.len() < 2 {
        return 0.0;
    }
    let mut v: Vec<f32> = xs.iter().copied().filter(|x| x.is_finite()).collect();
    if v.len() < 2 {
        return 0.0;
    }
    let m = median(&mut v);
    for x in &mut v {
        *x = (*x - m).abs();
    }
    median(&mut v)
}

fn median(v: &mut [f32]) -> f32 {
    v.sort_by(f32::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Skipped seconds a timed push ([`History::push_at`]) fills with the
/// reading that ends them: a reading every five seconds in the background is
/// that quantity over those five seconds. A longer silence (no window showed
/// the bars) is a gap.
pub const HISTORY_FILL_S: u64 = 6;

/// The last [`HISTORY_LEN`] (or a chosen count of) seconds of one quantity,
/// oldest first. `None` marks a gap: a second nothing was read (the window
/// was hidden, the reader failed), drawn as a gap, never as zero.
///
/// A timed push ([`History::push_at`]) keeps one entry per second of the
/// host clock whatever the reading cadence: two readings in one second (the
/// lens's 2 Hz) fold into that second's mean, and a reading after a short
/// silence (the background's 5 s) fills the seconds it spans. So a history
/// of 60 entries is always the last minute, and the surfaces may say so.
#[derive(Clone, Debug, PartialEq)]
pub struct History {
    values: std::collections::VecDeque<Option<f32>>,
    cap: usize,
    /// The second the newest entry holds, and how many readings it averages
    /// (`None` before the first timed push).
    last: Option<(u64, u32)>,
}

impl Default for History {
    fn default() -> Self {
        Self::with_capacity(HISTORY_LEN)
    }
}

impl History {
    /// An empty history of at most `cap` samples (at least 1).
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        let cap = cap.max(1);
        Self {
            values: std::collections::VecDeque::with_capacity(cap),
            cap,
            last: None,
        }
    }

    /// Append one sample, or a gap with `None`, dropping the oldest past the
    /// cap: one entry per call, whatever the time (see [`Self::push_at`]).
    pub fn push(&mut self, v: Option<f32>) {
        self.last = None;
        self.append(v.filter(|x| x.is_finite()));
    }

    /// Record the reading taken at `at_ns` (the host's monotonic clock) in
    /// the second it fell in: folded into that second's mean when the second
    /// already holds one, else appended after the seconds since the last
    /// reading (filled with this reading for at most [`HISTORY_FILL_S`] of
    /// them, gaps beyond).
    pub fn push_at(&mut self, v: Option<f32>, at_ns: u64) {
        let v = v.filter(|x| x.is_finite());
        let second = at_ns / 1_000_000_000;
        match self.last {
            Some((last, n)) if second <= last => {
                // A gap reading leaves the second's mean as it is.
                if let (Some(back), Some(new)) = (self.values.back_mut(), v) {
                    *back = Some(match *back {
                        Some(old) if n > 0 => (old * n as f32 + new) / (n + 1) as f32,
                        _ => new,
                    });
                    self.last = Some((last, n + 1));
                }
            }
            Some((last, _)) => {
                let skipped = second - last - 1;
                let fill = if skipped <= HISTORY_FILL_S { v } else { None };
                for _ in 0..skipped.min(self.cap as u64) {
                    self.append(fill);
                }
                self.append(v);
                self.last = Some((second, u32::from(v.is_some())));
            }
            None => {
                self.append(v);
                self.last = Some((second, u32::from(v.is_some())));
            }
        }
    }

    fn append(&mut self, v: Option<f32>) {
        if self.values.len() == self.cap {
            self.values.pop_front();
        }
        self.values.push_back(v);
    }

    /// The samples, oldest first.
    #[must_use]
    pub fn to_vec(&self) -> Vec<Option<f32>> {
        self.values.iter().copied().collect()
    }

    /// How many samples (gaps included) it holds.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether it holds nothing.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_sample_seeds_both_averages_so_a_new_series_is_steady() {
        let mut t = Trend::new();
        t.push(0.8, 1.0);
        let v = t.view();
        assert_eq!((v.level, v.slow, v.raw), (0.8, 0.8, 0.8));
        assert_eq!(v.motion(0.02), Motion::Steady);
        assert_eq!(v.glow_step(0.02), 0);
    }

    #[test]
    fn a_step_up_reads_rising_and_glows_then_settles_steady() {
        let mut t = Trend::new();
        for _ in 0..60 {
            t.push(0.1, 1.0);
        }
        t.push(0.9, 1.0);
        t.push(0.9, 1.0);
        let v = t.view();
        assert!(v.level > v.slow, "{v:?}");
        assert_eq!(v.motion(0.04), Motion::Rising);
        assert!(v.glow_step(0.04) >= 2, "{v:?}");
        for _ in 0..200 {
            t.push(0.9, 1.0);
        }
        let v = t.view();
        assert_eq!(v.motion(0.04), Motion::Steady, "{v:?}");
        assert_eq!(v.glow_step(0.04), 0);
    }

    #[test]
    fn a_step_down_reads_falling_and_never_glows() {
        let mut t = Trend::new();
        for _ in 0..60 {
            t.push(0.9, 1.0);
        }
        for _ in 0..3 {
            t.push(0.1, 1.0);
        }
        let v = t.view();
        assert_eq!(v.motion(0.04), Motion::Falling);
        assert_eq!(v.glow_step(0.04), 0);
        assert_eq!(Motion::Falling.word(), Some("easing"));
    }

    #[test]
    fn the_fast_average_follows_its_time_constant() {
        let mut t = Trend::new();
        t.push(0.0, 1.0);
        // One τ later the fast average has covered 1 − 1/e of a step.
        t.push(1.0, FAST_TAU_S);
        let want = 1.0 - (-1.0f32).exp();
        assert!((t.view().level - want).abs() < 1e-5, "{:?}", t.view());
    }

    #[test]
    fn a_skipped_sample_is_folded_in_at_its_true_age() {
        let mut a = Trend::new();
        let mut b = Trend::new();
        a.push(0.0, 1.0);
        b.push(0.0, 1.0);
        a.push(1.0, 4.0);
        b.push(1.0, 1.0);
        assert!(a.view().level > b.view().level);
    }

    #[test]
    fn variance_is_the_median_absolute_deviation_of_the_window() {
        assert_eq!(mad(&[]), 0.0);
        assert_eq!(mad(&[5.0]), 0.0);
        assert_eq!(mad(&[1.0, 1.0, 1.0, 1.0]), 0.0);
        // Median 3; deviations 2,1,0,1,2 → median 1.
        assert_eq!(mad(&[1.0, 2.0, 3.0, 4.0, 5.0]), 1.0);
        let mut t = Trend::new();
        for i in 0..30 {
            t.push(if i % 2 == 0 { 0.2 } else { 0.8 }, 1.0);
        }
        assert!((t.view().mad - 0.3).abs() < 1e-6, "{:?}", t.view());
    }

    #[test]
    fn a_jittery_series_needs_a_larger_move_to_read_rising() {
        let calm = TrendView {
            raw: 0.5,
            level: 0.55,
            slow: 0.5,
            mad: 0.0,
        };
        let jittery = TrendView { mad: 0.3, ..calm };
        assert_eq!(calm.motion(0.04), Motion::Rising);
        assert_eq!(jittery.motion(0.04), Motion::Steady);
    }

    #[test]
    fn non_finite_samples_are_ignored() {
        let mut t = Trend::new();
        t.push(f32::NAN, 1.0);
        assert!(!t.primed());
        t.push(0.5, f32::NAN);
        t.push(f32::INFINITY, 1.0);
        assert_eq!(t.view().level, 0.5);
    }

    const S: u64 = 1_000_000_000;

    #[test]
    fn a_timed_history_keeps_one_entry_a_second_at_any_cadence() {
        // 2 Hz (the lens): two readings a second fold into that second's mean.
        let mut h = History::with_capacity(60);
        for i in 0..120u64 {
            h.push_at(Some(if i % 2 == 0 { 0.2 } else { 0.4 }), 10 * S + i * S / 2);
        }
        assert_eq!(h.len(), 60, "120 readings at 2 Hz are the last minute");
        assert!(
            h.to_vec()
                .iter()
                .all(|v| v.is_some_and(|x| (x - 0.3).abs() < 1e-6))
        );
        // 1 Hz: one entry each.
        let mut h = History::with_capacity(60);
        for i in 0..30u64 {
            h.push_at(Some(0.5), 10 * S + i * S);
        }
        assert_eq!(h.len(), 30);
    }

    #[test]
    fn a_timed_history_fills_a_short_silence_and_gaps_a_long_one() {
        // The background's 5 s: the reading spans the seconds it ends.
        let mut h = History::with_capacity(60);
        h.push_at(Some(0.1), 10 * S);
        h.push_at(Some(0.7), 15 * S);
        assert_eq!(
            h.to_vec(),
            vec![
                Some(0.1),
                Some(0.7),
                Some(0.7),
                Some(0.7),
                Some(0.7),
                Some(0.7)
            ]
        );
        // Minutes with no reading (the bars were hidden) are gaps.
        let mut h = History::with_capacity(60);
        h.push_at(Some(0.1), 10 * S);
        h.push_at(Some(0.7), 100 * S);
        let v = h.to_vec();
        assert_eq!(
            v.len(),
            60,
            "the gap fills the whole minute, then the reading"
        );
        assert_eq!(v.last(), Some(&Some(0.7)));
        assert!(v[..59].iter().all(Option::is_none));
    }

    #[test]
    fn history_keeps_the_newest_samples_and_marks_gaps() {
        let mut h = History::with_capacity(3);
        assert!(h.is_empty());
        h.push(Some(1.0));
        h.push(None);
        h.push(Some(f32::NAN));
        h.push(Some(4.0));
        assert_eq!(h.to_vec(), vec![None, None, Some(4.0)]);
        assert_eq!(h.len(), 3);
    }
}
