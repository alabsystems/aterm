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
#[path = "history_tests.rs"]
mod tests;
