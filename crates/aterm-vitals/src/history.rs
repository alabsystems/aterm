// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The disk's 24 h free history and its rates (`docs/DESIGN-stats-deck-v2-2026-10-20.md` §7.2),
//! kept across relaunches ([`FreeHistory::saved`], [`FreeHistory::restored`]).

use std::collections::VecDeque;

use crate::ring::{Agg, BUCKET_S, BucketRing, Lands, Taken, lands};

/// Seconds one free-history bucket covers: 10 minutes.
pub const FREE_BUCKET_S: i64 = 600;
/// Buckets the free history keeps: 24 hours.
pub const FREE_BUCKETS: usize = 144;
/// How long `rate60` must have been positive before time to full shows, ms.
pub const FILLING_FOR_MS: i64 = 600_000;
/// Time to full is shown only under this, seconds: 7 days.
pub const TTF_MAX_S: f64 = 7.0 * 86_400.0;
/// The filling clause appears when the disk fills within this, seconds.
pub const FILLING_CLAUSE_S: f64 = 86_400.0;
/// A burst (`rate5`) that fills the disk within this is said, seconds.
pub const URGENT_S: f64 = 3_600.0;
/// How many buckets a rate's first read bucket may sit after its span's start.
pub const SPAN_SLACK: i64 = 3;

/// Which volume a free history reads: where it is mounted and its size.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolumeId {
    /// Its mount point.
    pub mount: String,
    /// Its size, bytes.
    pub total: u64,
}

/// One kept free-history bucket: its start (unix ms) and its readings of free bytes, each
/// weighed by its time ([`Agg`]: the mean, the least and the most free).
pub type FreeRow = (i64, Agg);

/// A free history as the host writes it ([`FreeHistory::saved`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SavedFree {
    /// The volume it read.
    pub volume: VolumeId,
    /// Its read buckets, oldest first.
    pub rows: Vec<FreeRow>,
}

/// The last 24 hours of free space, in 10-minute buckets of time-weighted readings.
#[derive(Clone, Debug, PartialEq)]
pub struct FreeHistory {
    /// The key (unix minute ÷ 10) of `b[0]`.
    first_k: i64,
    b: VecDeque<Agg>,
    /// When the newest reading was taken, unix ms; `None` for a restored history not read on.
    newest_ms: Option<i64>,
    /// Where the time the readings stand for ends, unix ms ([`BucketRing::push`]'s rule).
    covered_to: i64,
    /// The volume of the newest reading ([`Self::note_on`]).
    volume: Option<VolumeId>,
    /// Restored from an earlier run and not read on since ([`Self::restored`]).
    restored: bool,
}

const FREE_BUCKET_MS: i64 = FREE_BUCKET_S * 1000;

impl Default for FreeHistory {
    fn default() -> Self {
        Self {
            first_k: 0,
            b: VecDeque::new(),
            newest_ms: None,
            covered_to: i64::MIN,
            volume: None,
            restored: false,
        }
    }
}

impl FreeHistory {
    /// An empty history.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `free` bytes of the volume mounted at `mount`, `total` bytes in size, read as `t`
    /// says.
    ///
    /// A volume at another mount point starts the history afresh. A history restored from an
    /// earlier run ([`Self::restored`]) is also started afresh by the same mount point at
    /// another size; once read on, a size that moves (as ZFS reports one) keeps it.
    pub fn note_on(&mut self, mount: &str, total: u64, t: Taken, free: u64) {
        let other = self
            .volume
            .as_ref()
            .is_some_and(|v| v.mount != mount || (self.restored && v.total != total));
        if other {
            *self = Self::new();
        }
        self.restored = false;
        match self.volume.as_mut() {
            Some(v) => v.total = total,
            None => {
                self.volume = Some(VolumeId {
                    mount: mount.to_owned(),
                    total,
                });
            }
        }
        self.note(t, free);
    }

    /// Record `free` bytes read as `t` says. It lands by the rings' one back-step rule
    /// ([`crate::ring::lands`]) and weighs the part of its slot inside its bucket that no earlier
    /// reading stands for.
    pub fn note(&mut self, t: Taken, free: u64) {
        let at = t.at_ms;
        match lands(self.last_k(), self.newest_ms, at, FREE_BUCKET_MS) {
            Lands::First(k) => {
                self.first_k = k;
                self.b.push_back(Agg::default());
            }
            Lands::Newest => {}
            Lands::Ahead(by) => {
                if by >= FREE_BUCKETS as i64 {
                    self.b.clear();
                    self.first_k = at.div_euclid(FREE_BUCKET_MS);
                } else {
                    for _ in 1..by {
                        self.b.push_back(Agg::default());
                    }
                }
                self.b.push_back(Agg::default());
                while self.b.len() > FREE_BUCKETS {
                    self.b.pop_front();
                    self.first_k += 1;
                }
            }
            Lands::Back(by) => {
                let shift = by.saturating_mul(FREE_BUCKET_MS);
                self.first_k -= by;
                self.covered_to = self.covered_to.saturating_sub(shift);
                self.newest_ms = self.newest_ms.map(|n| n.saturating_sub(shift));
                self.b.push_back(Agg::default());
                while self.b.len() > FREE_BUCKETS {
                    self.b.pop_front();
                    self.first_k += 1;
                }
            }
        }
        if self.newest_ms.is_none_or(|n| at >= n) {
            self.newest_ms = Some(at);
        }
        let k = self.last_k().unwrap_or(0);
        let (lo, hi) = t.slot();
        let b_lo = k.saturating_mul(FREE_BUCKET_MS);
        let from = lo.max(self.covered_to).max(b_lo);
        let to = hi.min(b_lo.saturating_add(FREE_BUCKET_MS));
        let w = u32::try_from(to.saturating_sub(from).clamp(0, FREE_BUCKET_MS)).unwrap_or(0);
        if w > 0 {
            self.covered_to = to;
        }
        if let Some(b) = self.b.back_mut() {
            b.add(free as f64, w, false);
        }
    }

    /// The newest bucket's key; `None` before the first reading.
    #[must_use]
    pub fn last_k(&self) -> Option<i64> {
        (!self.b.is_empty()).then(|| self.first_k + self.b.len() as i64 - 1)
    }

    /// How many buckets it holds (unread ones included).
    #[must_use]
    pub fn len(&self) -> usize {
        self.b.len()
    }

    /// Whether nothing was read yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.b.is_empty()
    }

    /// The volume its readings are of; `None` before [`Self::note_on`].
    #[must_use]
    pub fn volume(&self) -> Option<&VolumeId> {
        self.volume.as_ref()
    }

    /// Its volume and read buckets, for the host to write; `None` with no volume or nothing read.
    #[must_use]
    pub fn saved(&self) -> Option<SavedFree> {
        let volume = self.volume.clone()?;
        let rows = self.saved_rows();
        (!rows.is_empty()).then_some(SavedFree { volume, rows })
    }

    /// Its read buckets, oldest first, each stamped at its start.
    #[must_use]
    pub fn saved_rows(&self) -> Vec<FreeRow> {
        (self.first_k..)
            .zip(&self.b)
            .filter(|(_, b)| b.is_sound())
            .map(|(k, b)| (k.saturating_mul(FREE_BUCKET_MS), *b))
            .collect()
    }

    /// A history from what [`Self::saved`] gave, holding only the rows in the 24 hours ending at
    /// `now_unix_ms`; its first reading keeps it only on the same volume ([`Self::note_on`]).
    #[must_use]
    pub fn restored(saved: &SavedFree, now_unix_ms: i64) -> Self {
        let now_k = now_unix_ms.div_euclid(FREE_BUCKET_MS);
        let oldest = now_k.saturating_sub(FREE_BUCKETS as i64 - 1);
        let mut rows: Vec<(i64, Agg)> = saved
            .rows
            .iter()
            .filter(|(_, b)| b.is_sound())
            .map(|&(at, b)| (at.div_euclid(FREE_BUCKET_MS), b))
            .filter(|(k, _)| (oldest..=now_k).contains(k))
            .collect();
        rows.sort_by_key(|&(k, _)| k);
        let (Some(&(first_k, _)), Some(&(last_k, _))) = (rows.first(), rows.last()) else {
            return Self::new();
        };
        let mut b = VecDeque::from(vec![Agg::default(); (last_k - first_k + 1) as usize]);
        for (k, row) in rows {
            b[(k - first_k) as usize].merge(&row);
        }
        Self {
            first_k,
            b,
            newest_ms: None,
            covered_to: i64::MIN,
            volume: Some(saved.volume.clone()),
            restored: true,
        }
    }

    /// The 24 hours ending at `now_unix_ms`, each bucket's least free space.
    #[must_use]
    pub fn window(&self, now_unix_ms: i64) -> Vec<Option<u64>> {
        self.buckets(now_unix_ms)
            .into_iter()
            .map(|b| b.map(|b| b.min.max(0.0) as u64))
            .collect()
    }

    /// The 24 hours ending at `now_unix_ms`, each bucket's readings: its time-weighted mean, its
    /// least and its most free space.
    #[must_use]
    pub fn buckets(&self, now_unix_ms: i64) -> Vec<Option<Agg>> {
        let now_k = now_unix_ms.div_euclid(FREE_BUCKET_MS);
        let start_k = now_k.saturating_sub(FREE_BUCKETS as i64 - 1);
        (start_k..=now_k)
            .map(|k| {
                k.checked_sub(self.first_k)
                    .and_then(|d| usize::try_from(d).ok())
                    .and_then(|i| self.b.get(i))
                    .filter(|b| b.value().is_some())
                    .copied()
            })
            .collect()
    }
}

/// Growth of bytes used, bytes per second, over the last 5, 10 and 60 minutes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rates {
    /// Over the last 5 minutes: the burst rate.
    pub rate5: Option<f64>,
    /// Over the last 10 minutes: the growth Details prints.
    pub rate10: Option<f64>,
    /// Over the last hour: what time to full divides by.
    pub rate60: Option<f64>,
}

/// The rates of a ring of bytes used, from its newest read bucket back.
#[must_use]
pub fn rates(ring: &BucketRing) -> Rates {
    rates_within(ring, SPAN_SLACK)
}

/// [`rates`], each span's first read bucket allowed `slack` buckets after the span's start: a
/// ring read once a minute (the disk watch's own, `aterm_agent::harness::disk::fill`) needs more
/// than [`SPAN_SLACK`], and a ring of [`crate::ring::RING_BUCKETS`] plus `slack` holds the
/// hour's span whole.
#[must_use]
pub fn rates_within(ring: &BucketRing, slack: i64) -> Rates {
    let Some((ke, ve)) = ring
        .iter()
        .rev()
        .find_map(|(k, b)| b.value().map(|v| (k, v)))
    else {
        return Rates::default();
    };
    let over = |minutes: i64| -> Option<f64> {
        let from = ke - minutes * 60 / BUCKET_S;
        let (ks, vs) = ring
            .iter()
            .filter(|&(k, _)| k >= from)
            .find_map(|(k, b)| b.value().map(|v| (k, v)))?;
        (ks <= from + slack && ks < ke).then(|| (ve - vs) / ((ke - ks) * BUCKET_S) as f64)
    };
    Rates {
        rate5: over(5),
        rate10: over(10),
        rate60: over(60),
    }
}

/// Seconds until `free` bytes run out at `rate60`; `None` without a positive rate, before the
/// disk has been filling for [`FILLING_FOR_MS`], or at [`TTF_MAX_S`] and beyond.
#[must_use]
pub fn time_to_full(free: u64, rate60: Option<f64>, filling_for_ms: i64) -> Option<f64> {
    let secs = time_to_line(free, 0, rate60)?;
    (filling_for_ms >= FILLING_FOR_MS && secs < TTF_MAX_S).then_some(secs)
}

/// Seconds until `free` bytes fall to `line` at `rate` bytes a second of growth (`0` at or
/// below it); `None` without a positive rate. [`time_to_full`] is its `line` 0.
#[must_use]
pub fn time_to_line(free: u64, line: u64, rate: Option<f64>) -> Option<f64> {
    let rate = rate.filter(|r| r.is_finite() && *r > 0.0)?;
    Some(free.saturating_sub(line) as f64 / rate)
}

/// The filling clause's facts ([`filling`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Filling {
    /// Seconds until the disk is full.
    pub secs: f64,
    /// The growth it is at, bytes per second.
    pub rate: f64,
    /// Whether it is the last 5 minutes' burst rather than the hour's rate.
    pub burst: bool,
}

impl Filling {
    /// Under an hour, the filling clause leads Details and the level clause follows it.
    #[must_use]
    pub fn outranks_level(&self) -> bool {
        self.secs < URGENT_S
    }
}

/// The filling clause, when it applies.
#[must_use]
pub fn filling(free: u64, rates: &Rates, ttf: Option<f64>) -> Option<Filling> {
    let burst = rates
        .rate5
        .filter(|r| r.is_finite() && *r > 0.0)
        .map(|rate| Filling {
            secs: free as f64 / rate,
            rate,
            burst: true,
        })
        .filter(|f| f.secs < URGENT_S);
    let hour = ttf
        .filter(|s| s.is_finite() && *s >= 0.0)
        .zip(rates.rate60.filter(|r| r.is_finite() && *r > 0.0))
        .map(|(secs, rate)| Filling {
            secs,
            rate,
            burst: false,
        })
        .filter(|f| f.secs < FILLING_CLAUSE_S);
    match (burst, hour) {
        (Some(b), Some(h)) => Some(if b.secs < h.secs { b } else { h }),
        (b, h) => b.or(h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_791_936_000_000;
    const MIN: i64 = 60_000;

    /// A reading at `at_ms` asked at the focused cadence.
    fn tk(at_ms: i64) -> Taken {
        Taken::at(at_ms, 1_000)
    }

    /// A kept row starting at `at` of `n` readings of `free` bytes, a second each.
    fn row(at: i64, free: u64, n: u16) -> FreeRow {
        let v = free as f64;
        (
            at,
            Agg {
                tau_ms: u32::from(n) * 1_000,
                n,
                over_ms: 0,
                mean: v,
                min: v,
                max: v,
            },
        )
    }

    #[test]
    fn the_free_history_keeps_144_ten_minute_minima() {
        let mut edge = FreeHistory::new();
        edge.note(tk(i64::MAX), 9);
        edge.note(tk(i64::MIN), 4);
        assert_eq!(
            edge.last_k(),
            Some(i64::MIN.div_euclid(FREE_BUCKET_S * 1000))
        );
        assert_eq!(edge.window(i64::MIN).last().copied().flatten(), Some(4));
        let mut h = FreeHistory::new();
        for m in 0..25 * 60 {
            h.note(tk(T0 + m * MIN), 1_000_000 - m as u64);
        }
        assert_eq!(h.len(), FREE_BUCKETS, "trimmed to 24 hours");
        let now = T0 + (25 * 60 - 1) * MIN;
        let w = h.window(now);
        assert_eq!(w.len(), FREE_BUCKETS);
        assert_eq!(w.last().copied().flatten(), Some(1_000_000 - (25 * 60 - 1)));
        assert_eq!(w[0], Some(1_000_000 - (60 + 9)));
        h.note(tk(now + 60 * MIN), 5);
        let w = h.window(now + 60 * MIN);
        assert_eq!(&w[FREE_BUCKETS - 6..FREE_BUCKETS - 1], &[None; 5]);
        assert_eq!(w[FREE_BUCKETS - 1], Some(5));
        h.note(tk(now + 61 * MIN), 4);
        h.note(tk(now + 60 * MIN + 30_000), 3);
        assert_eq!(
            &h.window(now + 61 * MIN)[FREE_BUCKETS - 2..],
            &[Some(5), Some(3)]
        );
        // 91 minutes back: every held bucket moves back with it (the one back-step rule), the
        // newest to just before the reading's own.
        h.note(tk(now - 30 * MIN), 2);
        let w = h.window(now - 30 * MIN);
        assert_eq!(&w[FREE_BUCKETS - 3..], &[Some(5), Some(3), Some(2)]);
        assert_eq!(&w[FREE_BUCKETS - 8..FREE_BUCKETS - 3], &[None; 5]);
        assert_eq!(w[FREE_BUCKETS - 9], Some(1_000_000 - (25 * 60 - 1)));
        h.note(tk(now - 20 * MIN), 1);
        assert_eq!(
            &h.window(now - 20 * MIN)[FREE_BUCKETS - 2..],
            &[Some(2), Some(1)]
        );
    }

    const DATA: &str = "/System/Volumes/Data";
    const SIZE: u64 = 1_000_000_000_000;
    const TEN: i64 = FREE_BUCKET_S * 1000;

    fn data() -> VolumeId {
        VolumeId {
            mount: DATA.into(),
            total: SIZE,
        }
    }

    fn read(h: &FreeHistory, now: i64) -> Vec<u64> {
        h.window(now).into_iter().flatten().collect()
    }

    /// F4′: jittered free-space readings count each moment once, as the rings' do: a bucket's
    /// read time is the union of its readings' slots, 3.6 s for four 1 s slots two of which
    /// overlap the one before by 200 ms, and its mean weighs each by the part only it stands for.
    /// Negative control: `note` without its `covered_to` clip (the mutant the F4′ review ran)
    /// adds each whole slot, 4 s.
    #[test]
    fn jittered_free_readings_weigh_the_union_of_their_slots() {
        let mut h = FreeHistory::new();
        let at = [500, 1_300, 2_700, 3_500];
        for (i, &t) in at.iter().enumerate() {
            h.note(tk(T0 + t), 1_000 * (i as u64 + 1));
        }
        let rows = h.saved_rows();
        let agg = rows.last().map(|&(_, a)| a).expect("read");
        assert_eq!((agg.n, agg.tau_ms), (4, 3_600), "the union of the slots");
        let mean =
            (1_000.0 * 1_000.0 + 2_000.0 * 800.0 + 3_000.0 * 1_000.0 + 4_000.0 * 800.0) / 3_600.0;
        assert!((agg.mean - mean).abs() < 1e-9, "{} vs {mean}", agg.mean);
    }

    #[test]
    fn the_free_history_round_trips_through_its_rows() {
        let mut h = FreeHistory::new();
        assert_eq!(h.saved(), None, "nothing read, nothing to write");
        for m in 0..3 * 60 {
            h.note_on(
                DATA,
                SIZE,
                tk(T0 + m * MIN + 7_000),
                900_000_000_000 - m as u64,
            );
        }
        let now = T0 + (3 * 60 - 1) * MIN + 7_000;
        let saved = h.saved().expect("three hours read");
        assert_eq!(saved.volume, data());
        assert_eq!(saved.rows.len(), 18, "one row per ten minutes");
        assert_eq!(
            (saved.rows[0].0, saved.rows[0].1.min, saved.rows[0].1.n),
            (T0, (900_000_000_000_u64 - 9) as f64, 10),
            "stamped at its start"
        );
        assert_eq!(saved.rows[0].1.tau_ms, 10_000, "a second each");
        assert!(saved.rows.windows(2).all(|p| p[1].0 - p[0].0 == TEN));
        let back = FreeHistory::restored(&saved, now + 5 * MIN);
        assert_eq!(back.window(now), h.window(now), "the same day");
        assert_eq!(back.saved(), Some(saved.clone()));
        assert_eq!(back.volume(), Some(&data()));

        let mut on = back.clone();
        on.note_on(DATA, SIZE, tk(now + 11 * MIN), 5);
        let w = on.window(now + 11 * MIN);
        assert_eq!(
            w.last().copied().flatten(),
            Some(5),
            "the relaunch reads on"
        );
        assert_eq!(read(&on, now + 11 * MIN).len(), 19, "after the 18 kept");

        let big = SavedFree {
            volume: data(),
            rows: vec![row(T0, u64::MAX, u16::MAX)],
        };
        let back = FreeHistory::restored(&big, T0);
        assert_eq!(back.saved(), Some(big));
        assert_eq!(read(&back, T0), [u64::MAX]);
        let mut edge = FreeHistory::new();
        edge.note_on(DATA, SIZE, tk(i64::MIN), 4);
        let saved = edge.saved().expect("read");
        assert_eq!(
            read(&FreeHistory::restored(&saved, i64::MIN), i64::MIN),
            [4]
        );
    }

    #[test]
    fn a_restored_history_keeps_only_the_last_24_hours() {
        let saved = SavedFree {
            volume: data(),
            rows: (0..150)
                .map(|i| row(T0 + i * TEN, 1_000 + i as u64, 1))
                .collect(),
        };
        let now = T0 + 149 * TEN + 3 * MIN;
        let h = FreeHistory::restored(&saved, now);
        assert_eq!(
            h.len(),
            FREE_BUCKETS,
            "the oldest six are over 24 hours old"
        );
        assert_eq!(read(&h, now), (1_006..1_150).collect::<Vec<u64>>());
        let later = now + 20 * TEN;
        let h = FreeHistory::restored(&saved, later);
        assert_eq!(read(&h, later), (1_026..1_150).collect::<Vec<u64>>());
        assert_eq!(
            FreeHistory::restored(&saved, now + 150 * TEN),
            FreeHistory::default(),
            "a day and more since the last write: empty"
        );
        let early = T0 + 100 * TEN;
        let h = FreeHistory::restored(&saved, early);
        assert_eq!(
            read(&h, early),
            (1_000..=1_100).collect::<Vec<u64>>(),
            "rows after the clock's now are dropped"
        );
        let damaged = SavedFree {
            volume: data(),
            rows: vec![
                row(T0 + 2 * TEN + 59_000, 7, 3),
                row(T0, 9, 1),
                row(T0 + 2 * TEN, 4, 2),
                row(T0 + TEN, 1, 0),
            ],
        };
        let h = FreeHistory::restored(&damaged, T0 + 2 * TEN);
        assert_eq!(
            h.window(T0 + 2 * TEN)[FREE_BUCKETS - 3..],
            [Some(9), None, Some(4)],
            "sorted, an unread row dropped, one bucket's rows merged"
        );
        let merged = h.saved().expect("kept").rows[1];
        assert_eq!((merged.0, merged.1.min, merged.1.n), (T0 + 2 * TEN, 4.0, 5));
        assert!(
            (merged.1.mean - 5.8).abs() < 1e-12,
            "weighed by time: {merged:?}"
        );
    }

    #[test]
    fn a_restored_history_is_dropped_by_another_volume() {
        let saved = SavedFree {
            volume: data(),
            rows: (0..6)
                .map(|i| row(T0 + i * TEN, 500 - i as u64, 60))
                .collect(),
        };
        let now = T0 + 6 * TEN;
        let restored = FreeHistory::restored(&saved, now);

        let mut h = restored.clone();
        h.note_on("/Volumes/Backup", SIZE, tk(now), 7);
        assert_eq!(read(&h, now), [7], "another mount point");
        assert_eq!(
            h.volume().map(|v| v.mount.as_str()),
            Some("/Volumes/Backup")
        );
        let mut h = restored.clone();
        h.note_on(DATA, 2 * SIZE, tk(now), 7);
        assert_eq!(read(&h, now), [7], "the same mount point at another size");
        let mut h = restored.clone();
        h.note_on(DATA, SIZE, tk(now), 7);
        assert_eq!(read(&h, now), [500, 499, 498, 497, 496, 495, 7], "control");

        h.note_on(DATA, SIZE - 4_096, tk(now + MIN), 6);
        assert_eq!(
            read(&h, now + MIN).len(),
            7,
            "read on, a size that moves keeps it"
        );
        assert_eq!(h.volume().map(|v| v.total), Some(SIZE - 4_096));
        h.note_on("/Volumes/Backup", SIZE, tk(now + 2 * MIN), 3);
        assert_eq!(read(&h, now + 2 * MIN), [3], "another mount point in a run");
        let mut fresh = FreeHistory::new();
        fresh.note(tk(now), 8);
        fresh.note_on(DATA, SIZE, tk(now + MIN), 9);
        assert_eq!(read(&fresh, now + MIN), [8], "no volume yet: kept");
    }

    fn filling_ring(minutes: i64, per_s: f64) -> BucketRing {
        missing_ring(minutes, per_s, 0..0)
    }

    fn missing_ring(minutes: i64, per_s: f64, missed: std::ops::Range<i64>) -> BucketRing {
        let mut r = BucketRing::default();
        for i in (0..minutes * 6).filter(|i| !missed.contains(i)) {
            let _ = r.push(
                tk(T0 + i * 10_000),
                Some(1e12 + per_s * (i * 10) as f64),
                false,
            );
        }
        r
    }

    #[test]
    fn rates_need_the_span_read() {
        let r = filling_ring(7, 1e6);
        let got = rates(&r);
        assert!(
            (got.rate5.expect("5 min read") - 1e6).abs() < 1e-3,
            "{got:?}"
        );
        assert_eq!(got.rate10, None, "7 minutes do not cover 10");
        assert_eq!(got.rate60, None);
        let r = filling_ring(61, 2e6);
        let got = rates(&r);
        for rate in [got.rate5, got.rate10, got.rate60] {
            assert!((rate.expect("covered") - 2e6).abs() < 1e-3, "{got:?}");
        }
        assert_eq!(rates(&BucketRing::default()), Rates::default());
        let r = missing_ring(61, 2e6, 6..8);
        let got = rates(&r);
        assert!((got.rate60.expect("slack") - 2e6).abs() < 1e-3, "{got:?}");
        assert_eq!(rates(&missing_ring(61, 2e6, 6..9)).rate60, None);
    }

    #[test]
    fn a_ring_read_once_a_minute_reads_its_rates_within_a_wider_slack() {
        let mut r = BucketRing::with_capacity(crate::ring::RING_BUCKETS + 12);
        for i in 0..58 {
            let at = i * 65_000 + 7_000;
            let _ = r.push(tk(T0 + at), Some(1e12 + 2e6 * (at / 1_000) as f64), false);
        }
        assert_eq!(
            rates(&r).rate5,
            None,
            "a reading about a minute misses the 30 s slack"
        );
        let got = rates_within(&r, 12);
        for rate in [got.rate5, got.rate10, got.rate60] {
            let rate = rate.expect("covered");
            assert!(
                (rate - 2e6).abs() < 2e6 * 0.05,
                "a bucket's offset only: {got:?}"
            );
        }
    }

    #[test]
    fn time_to_line_is_time_to_full_above_a_line() {
        let free = 36_000_000_000;
        assert_eq!(time_to_line(free, 0, Some(1e7)), Some(3_600.0));
        assert_eq!(time_to_line(free, 18_000_000_000, Some(1e7)), Some(1_800.0));
        assert_eq!(
            time_to_line(free, free + 1, Some(1e7)),
            Some(0.0),
            "below the line"
        );
        assert_eq!(time_to_line(free, 0, Some(0.0)), None);
        assert_eq!(time_to_line(free, 0, Some(-1.0)), None);
        assert_eq!(time_to_line(free, 0, Some(f64::INFINITY)), None);
        assert_eq!(time_to_line(free, 0, Some(f64::NAN)), None);
        assert_eq!(time_to_line(free, 0, None), None);
    }

    #[test]
    fn time_to_full_needs_ten_minutes_of_filling_and_under_a_week() {
        let free = 36_000_000_000;
        assert_eq!(time_to_full(free, Some(1e7), FILLING_FOR_MS - 1), None);
        assert_eq!(time_to_full(free, Some(1e7), FILLING_FOR_MS), Some(3_600.0));
        assert_eq!(time_to_full(free, Some(0.0), FILLING_FOR_MS), None);
        assert_eq!(time_to_full(free, Some(-1e7), FILLING_FOR_MS), None);
        assert_eq!(time_to_full(free, None, FILLING_FOR_MS), None);
        assert_eq!(time_to_full(free, Some(5e4), FILLING_FOR_MS), None);
    }

    #[test]
    fn the_filling_clause_and_the_burst_rule() {
        let free = 36_000_000_000;
        let slow = Rates {
            rate5: Some(2e6),
            rate10: Some(2e6),
            rate60: Some(2e6),
        };
        let f = filling(free, &slow, time_to_full(free, slow.rate60, FILLING_FOR_MS))
            .expect("full in 5 h");
        assert_eq!((f.secs, f.burst), (18_000.0, false));
        assert!(!f.outranks_level());
        let burst = Rates {
            rate5: Some(2e7),
            ..slow
        };
        let f = filling(free, &burst, None).expect("a burst");
        assert_eq!((f.secs, f.rate, f.burst), (1_800.0, 2e7, true));
        assert!(f.outranks_level(), "under an hour it leads Details");
        let mild = Rates {
            rate5: Some(5e6),
            rate10: None,
            rate60: None,
        };
        assert_eq!(filling(free, &mild, None), None);
        assert_eq!(filling(free, &Rates::default(), None), None);
        let crawl = Rates {
            rate60: Some(1e5),
            ..Rates::default()
        };
        assert_eq!(
            filling(
                free,
                &crawl,
                time_to_full(free, crawl.rate60, FILLING_FOR_MS)
            ),
            None
        );
        let non_finite = Rates {
            rate5: Some(f64::INFINITY),
            rate10: None,
            rate60: Some(f64::NAN),
        };
        assert_eq!(filling(free, &non_finite, Some(100.0)), None);
    }

    /// NEGATIVE CONTROL: repeated sub-clamp backward steps eventually cross `CLAMP_BACK_S`
    /// relative to the newest reading, and `window` handles extreme `i64` timestamps without
    /// arithmetic overflow.
    #[test]
    fn repeated_backsteps_shift_free_history_and_extreme_window_does_not_overflow() {
        let mut h = FreeHistory::new();
        let newest = T0 + 2 * FREE_BUCKET_S * 1000;
        h.note(tk(newest), 100);
        let initial_last = h.last_k().unwrap();
        // 40 s into the previous bucket: clamped to the open bucket.
        h.note(tk(newest - 40_000), 90);
        assert_eq!(h.last_k(), Some(initial_last));
        // Another 40 s back (80 s behind `newest`, > CLAMP_BACK_S): shifts `first_k`.
        let stepped = newest - 80_000;
        h.note(tk(stepped), 80);
        assert_eq!(h.last_k(), Some(stepped.div_euclid(FREE_BUCKET_S * 1000)));
        assert_eq!(h.window(i64::MIN).len(), FREE_BUCKETS);
        assert_eq!(h.window(i64::MAX).len(), FREE_BUCKETS);
    }
}
