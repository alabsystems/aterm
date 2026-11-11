// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
        if let Some(k) = ring.push(
            crate::ring::Taken::at(t0 + i as i64 * 10_000, 1_000),
            Some(v),
            false,
        ) && let Some(s) = ring.sustained(k)
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
    let back = Peaks::from_pairs(
        pairs
            .iter()
            .map(|(n, r, days)| (n.as_str(), *r, days.as_slice())),
    );
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
