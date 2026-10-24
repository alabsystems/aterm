// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rolling 7-day [`DailyMax`] and [`Peaks`] capacities.

use crate::ring::BUCKET_S;

/// Days a peak looks back over, today included.
pub const PEAK_DAYS: u32 = 7;
/// Seconds of readings before the network has learned its peak: 24 hours.
pub const NET_LEARN_S: u32 = 86_400;
/// Seconds of agent readings before tokens have learned their peak.
pub const TOKENS_LEARN_S: u32 = 600;

/// The last [`PEAK_DAYS`] days' maxima of one quantity.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DailyMax {
    /// `(day, maximum)` in UTC Unix days, oldest first ([`PEAK_DAYS`]).
    pub days: Vec<(u32, f64)>,
    /// Seconds of sustained readings noted, ever (saturating).
    pub read_s: u32,
}

impl DailyMax {
    /// Note `v`, the sustained-30 s value ending at a bucket that closed on `today`.
    pub fn note_bucket(&mut self, today: u32, v: f64) {
        if !v.is_finite() || v < 0.0 {
            return;
        }
        self.read_s = self.read_s.saturating_add(BUCKET_S as u32);
        match self.days.binary_search_by_key(&today, |&(d, _)| d) {
            Ok(i) => self.days[i].1 = self.days[i].1.max(v),
            Err(i) => self.days.insert(i, (today, v)),
        }
        self.days
            .retain(|&(d, _)| today.checked_sub(d).is_some_and(|age| age < PEAK_DAYS));
        let extra = self.days.len().saturating_sub(PEAK_DAYS as usize);
        self.days.drain(..extra);
    }

    /// The largest maximum of the [`PEAK_DAYS`] days ending `today`.
    #[must_use]
    pub fn peak(&self, today: u32) -> Option<f64> {
        self.days
            .iter()
            .filter(|&&(d, _)| today.checked_sub(d).is_some_and(|age| age < PEAK_DAYS))
            .map(|&(_, v)| v)
            .reduce(f64::max)
    }

    /// Whether at least `min_s` seconds have been read ([`NET_LEARN_S`], [`TOKENS_LEARN_S`]).
    #[inline]
    #[must_use]
    pub const fn learned(&self, min_s: u32) -> bool {
        self.read_s >= min_s
    }

    /// The seconds read and the day rows, for the host to write.
    #[must_use]
    pub fn to_pairs(&self) -> (u32, Vec<(u32, f64)>) {
        (self.read_s, self.days.clone())
    }

    /// A daily max from what [`Self::to_pairs`] gave ([`PEAK_DAYS`]).
    #[must_use]
    pub fn from_pairs(read_s: u32, days: &[(u32, f64)]) -> Self {
        let mut rows: Vec<(u32, f64)> = days
            .iter()
            .copied()
            .filter(|&(_, v)| v.is_finite() && v >= 0.0)
            .collect();
        rows.sort_by_key(|&(d, _)| d);
        rows.dedup_by(|later, kept| {
            let same = later.0 == kept.0;
            if same {
                kept.1 = kept.1.max(later.1);
            }
            same
        });
        let extra = rows.len().saturating_sub(PEAK_DAYS as usize);
        rows.drain(..extra);
        Self { days: rows, read_s }
    }
}

/// One persisted peak: its name ([`Peaks::NAMES`]), the seconds it has read and its day rows.
pub type PeakRow = (&'static str, u32, Vec<(u32, f64)>);

/// Every peak the model keeps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Peaks {
    /// Bytes per second received.
    pub net_rx: DailyMax,
    /// Bytes per second sent.
    pub net_tx: DailyMax,
    /// aterm's output tokens per minute.
    pub tokens: DailyMax,
}

impl Peaks {
    /// The rows' names in [`Self::to_pairs`].
    pub const NAMES: [&'static str; 3] = ["net_rx", "net_tx", "tokens"];

    /// Each peak as `(name, read_s, days)`, in [`Self::NAMES`] order.
    #[must_use]
    pub fn to_pairs(&self) -> [PeakRow; 3] {
        let [a, b, c] = [&self.net_rx, &self.net_tx, &self.tokens].map(DailyMax::to_pairs);
        [
            (Self::NAMES[0], a.0, a.1),
            (Self::NAMES[1], b.0, b.1),
            (Self::NAMES[2], c.0, c.1),
        ]
    }

    /// Peaks from `(name, read_s, days)` rows.
    #[must_use]
    pub fn from_pairs<'a>(
        rows: impl IntoIterator<Item = (&'a str, u32, &'a [(u32, f64)])>,
    ) -> Self {
        let mut p = Self::default();
        for (name, read_s, days) in rows {
            let slot = match name {
                "net_rx" => &mut p.net_rx,
                "net_tx" => &mut p.net_tx,
                "tokens" => &mut p.tokens,
                _ => continue,
            };
            *slot = DailyMax::from_pairs(read_s, days);
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::BucketRing;

    #[test]
    fn peaks() {
        let mut d = DailyMax::default();
        d.note_bucket(10, 9.0);
        d.note_bucket(10, 4.0);
        d.note_bucket(12, 3.0);
        assert_eq!(d.peak(10), Some(9.0));
        assert_eq!(d.peak(16), Some(9.0));
        assert_eq!(d.peak(17), Some(3.0), "day 10 left the window");
        d.note_bucket(17, 1.0);
        assert_eq!(d.days, [(12, 3.0), (17, 1.0)]);
        assert_eq!(d.peak(30), None);

        let mut ring = BucketRing::default();
        let mut day = DailyMax::default();
        let t0 = 1_792_000_800_000i64;
        for (i, v) in [3.0, 3.2, 40.0, 3.1, 2.9, 3.0, 3.3].into_iter().enumerate() {
            if let Some(k) = ring.push(t0 + i as i64 * 10_000, Some(v), false)
                && let Some(s) = ring.sustained(k)
            {
                day.note_bucket(20_740, s);
            }
        }
        assert_eq!(day.peak(20_740), Some(3.1), "{day:?}");

        let mut t = DailyMax::default();
        for _ in 0..59 {
            t.note_bucket(1, 1.0);
        }
        assert!(!t.learned(TOKENS_LEARN_S));
        t.note_bucket(1, 1.0);
        assert!(t.learned(TOKENS_LEARN_S));
        assert!(!t.learned(NET_LEARN_S));
        let n = DailyMax {
            read_s: NET_LEARN_S - 10,
            ..DailyMax::default()
        };
        let mut n2 = n.clone();
        assert!(!n.learned(NET_LEARN_S));
        n2.note_bucket(1, 1.0);
        assert!(n2.learned(NET_LEARN_S));

        let p = Peaks {
            net_rx: d.clone(),
            net_tx: n2,
            tokens: t,
        };
        let pairs = p.to_pairs();
        let back = Peaks::from_pairs(pairs.iter().map(|(n, r, days)| (*n, *r, days.as_slice())));
        assert_eq!(back, p);
        let damaged = DailyMax::from_pairs(
            5,
            &[
                (3, 1.0),
                (1, f64::NAN),
                (2, -4.0),
                (2, 1.0),
                (9, 2.0),
                (9, 7.0),
                (4, 1.0),
                (5, 1.0),
                (6, 1.0),
                (7, 1.0),
                (8, 1.0),
                (10, f64::INFINITY),
            ],
        );
        assert_eq!(
            damaged.days,
            [
                (3, 1.0),
                (4, 1.0),
                (5, 1.0),
                (6, 1.0),
                (7, 1.0),
                (8, 1.0),
                (9, 7.0)
            ]
        );
        assert_eq!(damaged.read_s, 5);

        let mut future = DailyMax::from_pairs(0, &[(10, 2.0), (11, 20.0), (u32::MAX, 99.0)]);
        assert_eq!(future.peak(10), Some(2.0));
        future.note_bucket(10, 1.0);
        assert_eq!(future.days, [(10, 2.0)]);

        let mut edge = DailyMax::default();
        edge.note_bucket(u32::MAX, 4.0);
        assert_eq!(edge.peak(u32::MAX), Some(4.0));
    }
}
