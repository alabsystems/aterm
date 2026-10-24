// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rolling 7-day [`DailyMax`] and [`Peaks`] capacities, and each account's [`PlanWeek`].

use crate::ring::BUCKET_S;

/// Days a peak looks back over, today included.
pub const PEAK_DAYS: u32 = 7;
/// Seconds of readings before the network has learned its peak: 24 hours.
pub const NET_LEARN_S: u32 = 86_400;
/// Seconds of agent readings before tokens have learned their peak.
pub const TOKENS_LEARN_S: u32 = 600;
/// Rows a [`PlanWeek`] keeps: one a day for [`PEAK_DAYS`] days, and room for a reset day's second.
pub const PLAN_ROWS: usize = PEAK_DAYS as usize + 2;
/// Accounts whose weeks [`Peaks::plan`] keeps.
pub const PLAN_ACCOUNTS: usize = 4;
/// Two resets this close are one window: Claude Code's `/usage` cache, the usage tap and the
/// accounts store may place one week's reset seconds apart, and weekly resets are 7 days apart.
pub const SAME_RESET_S: i64 = 3_600;

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

/// One day's newest reading of an account's weekly plan figure, in one weekly window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanDay {
    /// The day it was read on, unix days in the local zone.
    pub day: u32,
    /// The week's share used, tenths of a percent (0..=1000).
    pub used_x10: u16,
    /// When the week resets, unix seconds: which window the reading is of ([`SAME_RESET_S`]).
    pub resets_at: i64,
    /// When it was read, unix seconds.
    pub at: i64,
}

impl PlanDay {
    /// Whether it can be a reading: a share of at most 100%, read before its window reset.
    const fn valid(&self) -> bool {
        self.used_x10 <= 1000 && self.resets_at > self.at
    }

    /// Whether `other` is a reading of the same day and window.
    fn same_slot(&self, other: &Self) -> bool {
        self.day == other.day
            && self.resets_at.abs_diff(other.resets_at) <= SAME_RESET_S.unsigned_abs()
    }
}

/// ONE ACCOUNT'S WEEKLY PLAN FIGURE over the [`PEAK_DAYS`] days ending on its newest row's day:
/// each day's newest reading, oldest first. The day a week resets keeps two rows, the old
/// window's last reading and the new window's newest, so a run draws the reset as a gap.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanWeek {
    /// The account's key (`c-<12 hex>`).
    pub account: String,
    /// The rows, by day, then by window (at most [`PLAN_ROWS`]).
    pub days: Vec<PlanDay>,
}

impl PlanWeek {
    /// Note a reading: it replaces its day and window's row when it was read later than that
    /// row, so the same figure noted again changes nothing. A share over 100% or a reading
    /// at or past its own reset is no reading.
    pub fn note(&mut self, r: PlanDay) {
        if !r.valid() {
            return;
        }
        if let Some(held) = self.days.iter_mut().find(|held| held.same_slot(&r)) {
            if r.at > held.at {
                *held = r;
            }
            return;
        }
        let i = self
            .days
            .partition_point(|d| (d.day, d.resets_at) <= (r.day, r.resets_at));
        self.days.insert(i, r);
        if let Some(newest) = self.days.last().map(|d| d.day) {
            self.days.retain(|d| newest - d.day < PEAK_DAYS);
        }
        let extra = self.days.len().saturating_sub(PLAN_ROWS);
        self.days.drain(..extra);
    }

    /// When its newest reading was read, unix seconds.
    #[must_use]
    pub fn read_at(&self) -> Option<i64> {
        self.days.iter().map(|d| d.at).max()
    }

    /// A week from rows a file gave back, kept as [`Self::note`] keeps them.
    #[must_use]
    pub fn from_rows(account: &str, rows: &[PlanDay]) -> Self {
        let mut week = Self {
            account: account.to_owned(),
            days: Vec::new(),
        };
        for r in rows {
            week.note(*r);
        }
        week
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
    /// Each account's weekly plan figure, at most [`PLAN_ACCOUNTS`], none empty.
    pub plan: Vec<PlanWeek>,
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

    /// Note `account`'s reading `r` ([`PlanWeek::note`]); past [`PLAN_ACCOUNTS`], the account
    /// read longest ago goes.
    pub fn note_plan(&mut self, account: &str, r: PlanDay) {
        if !r.valid() {
            return;
        }
        match self.plan.iter_mut().find(|week| week.account == account) {
            Some(week) => week.note(r),
            None => self.plan.push(PlanWeek::from_rows(account, &[r])),
        }
        keep_freshest(&mut self.plan);
    }

    /// The weeks of `(account, rows)` a file gave back ([`PlanWeek::from_rows`]): an account's
    /// first entry, none empty, the [`PLAN_ACCOUNTS`] read most recently.
    #[must_use]
    pub fn plan_from_rows<'a>(
        rows: impl IntoIterator<Item = (&'a str, &'a [PlanDay])>,
    ) -> Vec<PlanWeek> {
        let mut plan: Vec<PlanWeek> = Vec::new();
        for (account, days) in rows {
            if plan.iter().any(|week| week.account == account) {
                continue;
            }
            let week = PlanWeek::from_rows(account, days);
            if !week.days.is_empty() {
                plan.push(week);
            }
        }
        keep_freshest(&mut plan);
        plan
    }
}

/// Drop the weeks read longest ago past [`PLAN_ACCOUNTS`].
fn keep_freshest(plan: &mut Vec<PlanWeek>) {
    while plan.len() > PLAN_ACCOUNTS {
        let Some((stalest, _)) = plan
            .iter()
            .enumerate()
            .min_by_key(|(_, week)| week.read_at())
        else {
            break;
        };
        plan.remove(stalest);
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
            ..Peaks::default()
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

    const D: u32 = 20_740;

    fn at(day: u32, hour: i64) -> i64 {
        i64::from(day) * 86_400 + hour * 3_600
    }

    #[test]
    fn a_plan_week_keeps_each_days_newest_reading_per_window() {
        let reset = at(D + 5, 12);
        let next = reset + 7 * 86_400;
        let reading = |day: u32, hour: i64| PlanDay {
            day,
            used_x10: u16::try_from(hour).expect("an hour")
                + 30 * u16::try_from(day - D).expect("a day"),
            resets_at: if at(day, hour) < reset { reset } else { next },
            at: at(day, hour),
        };
        let mut week = PlanWeek::default();
        for day in D..D + 9 {
            for hour in [10, 20] {
                week.note(reading(day, hour));
            }
        }
        let rows: Vec<(u32, i64, i64)> = week
            .days
            .iter()
            .map(|d| (d.day, d.at, d.resets_at))
            .collect();
        assert_eq!(
            rows,
            [
                (D + 2, at(D + 2, 20), reset),
                (D + 3, at(D + 3, 20), reset),
                (D + 4, at(D + 4, 20), reset),
                (D + 5, at(D + 5, 10), reset),
                (D + 5, at(D + 5, 20), next),
                (D + 6, at(D + 6, 20), next),
                (D + 7, at(D + 7, 20), next),
                (D + 8, at(D + 8, 20), next),
            ],
            "7 days, each day's newest, the reset day's two windows"
        );
        assert_eq!(week.read_at(), Some(at(D + 8, 20)));

        let kept = week.clone();
        week.note(reading(D + 8, 20));
        week.note(reading(D + 8, 15));
        week.note(reading(D + 1, 20));
        week.note(PlanDay {
            used_x10: 1001,
            ..reading(D + 8, 23)
        });
        week.note(PlanDay {
            resets_at: at(D + 8, 23),
            ..reading(D + 8, 23)
        });
        assert_eq!(
            week, kept,
            "the same figure, an earlier one, a day out of the week, over 100%, read at its reset"
        );

        week.note(PlanDay {
            used_x10: 999,
            resets_at: next + 300,
            ..reading(D + 8, 23)
        });
        assert_eq!(
            week.days.len(),
            kept.days.len(),
            "a reset 5 minutes off is the same window"
        );
        assert_eq!(
            week.days.last().map(|d| (d.used_x10, d.at)),
            Some((999, at(D + 8, 23)))
        );
    }

    #[test]
    fn a_fifth_account_evicts_the_stalest() {
        let reading = |minute: i64| PlanDay {
            day: D,
            used_x10: 100,
            resets_at: at(D + 3, 0),
            at: at(D, 9) + minute * 60,
        };
        let mut p = Peaks::default();
        for (minute, account) in [(0, "c-a"), (1, "c-b"), (2, "c-c"), (3, "c-d")] {
            p.note_plan(account, reading(minute));
        }
        p.note_plan("c-a", reading(10));
        p.note_plan("c-e", reading(4));
        let accounts = |p: &Peaks| p.plan.iter().map(|w| w.account.clone()).collect::<Vec<_>>();
        assert_eq!(
            accounts(&p),
            ["c-a", "c-c", "c-d", "c-e"],
            "c-b was read longest ago"
        );
        p.note_plan(
            "c-f",
            PlanDay {
                used_x10: 1001,
                ..reading(20)
            },
        );
        assert_eq!(
            accounts(&p),
            ["c-a", "c-c", "c-d", "c-e"],
            "no reading, no account"
        );
        assert_eq!(
            p.plan[0].days.len(),
            1,
            "c-a's later reading replaced its day's row"
        );
        assert_eq!(p.plan[0].read_at(), Some(at(D, 9) + 600));
    }

    #[test]
    fn damaged_plan_rows_are_cleaned() {
        let row = |day: u32, used_x10: u16, resets_at: i64, at: i64| PlanDay {
            day,
            used_x10,
            resets_at,
            at,
        };
        let r = at(D + 9, 0);
        let damaged = [
            row(D + 3, 300, r, at(D + 3, 9)),
            row(D + 1, 100, r, at(D + 1, 9)),
            row(D + 3, 310, r + 60, at(D + 3, 11)),
            row(D + 3, 305, r, at(D + 3, 10)),
            row(D + 2, 2000, r, at(D + 2, 9)),
            row(D + 2, 200, at(D + 2, 9), at(D + 2, 9)),
            row(D + 4, 400, r, at(D + 4, 9)),
        ];
        let week = PlanWeek::from_rows("c-a", &damaged);
        let rows: Vec<(u32, u16)> = week.days.iter().map(|d| (d.day, d.used_x10)).collect();
        assert_eq!(rows, [(D + 1, 100), (D + 3, 310), (D + 4, 400)]);

        let crowded: Vec<PlanDay> = (0..7)
            .flat_map(|i| {
                let day = D + i;
                [
                    row(day, 1, at(day, 1) + 86_400, at(day, 1)),
                    row(day, 2, at(day, 5) + 86_400, at(day, 2)),
                ]
            })
            .rev()
            .collect();
        let capped = PlanWeek::from_rows("c-b", &crowded);
        assert_eq!(capped.days.len(), PLAN_ROWS);
        assert_eq!(
            capped.days.first().map(|d| (d.day, d.used_x10)),
            Some((D + 2, 2))
        );
        assert_eq!(
            capped.days.last().map(|d| (d.day, d.used_x10)),
            Some((D + 6, 2))
        );
        assert!(
            capped
                .days
                .windows(2)
                .all(|w| (w[0].day, w[0].resets_at) < (w[1].day, w[1].resets_at))
        );

        let good = [row(D, 10, r, at(D, 9))];
        let later = |minute: i64| [row(D, 10, r, at(D, 9) + minute * 60)];
        let (b, c, d, e) = (later(1), later(2), later(3), later(4));
        let plan = Peaks::plan_from_rows([
            ("c-a", &good[..]),
            ("c-a", &later(9)[..]),
            ("c-z", &damaged[4..6]),
            ("c-b", &b[..]),
            ("c-c", &c[..]),
            ("c-d", &d[..]),
            ("c-e", &e[..]),
        ]);
        let accounts: Vec<&str> = plan.iter().map(|w| w.account.as_str()).collect();
        assert_eq!(
            accounts,
            ["c-b", "c-c", "c-d", "c-e"],
            "an account's first entry, no empty week, the four read last"
        );
    }
}
