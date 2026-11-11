// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const T0: i64 = 1_792_000_800_000;

#[test]
fn p10_is_the_ten_second_mean_of_the_readings() {
    let mut r = RunQueueRing::default();
    let mut last = Runnable::default();
    for (i, milli) in [4_000, 8_000, 12_000, 30_000].into_iter().enumerate() {
        let at = T0 + i64::try_from(i).expect("few") * 1000;
        last = r.absorb(Taken::at(at, 1000), RunQueueRead::Milli(milli));
    }
    let Runnable::Read(l) = last else {
        panic!("{last:?}")
    };
    assert!((l.mean - 13.5).abs() < 1e-9, "{l:?}");
    assert_eq!(l.max, 30.0);
    // Ten seconds on with nothing read, the window is empty: the failure, not a zero.
    let gone = r.absorb(Taken::at(T0 + 15_000, 1000), RunQueueRead::Failed);
    assert_eq!(gone, Runnable::Unread(Unread::NotSampled));
    assert_eq!(gone.mean(), None);
}

#[test]
fn the_queue_is_p10_and_load1_only_where_p10_is_unread() {
    let read = Runnable::Read(Level {
        mean: 26.8,
        max: 31.0,
    });
    assert_eq!(
        RunQueue::of(read, Some(16.59)),
        Some(RunQueue {
            threads: 26.8,
            basis: QueueBasis::Runnable
        })
    );
    for why in [
        Unread::Unsupported,
        Unread::UnreadFormat,
        Unread::NotSampled,
    ] {
        assert_eq!(
            RunQueue::of(Runnable::Unread(why), Some(16.59)),
            Some(RunQueue {
                threads: 16.59,
                basis: QueueBasis::Load1
            })
        );
    }
    assert_eq!(RunQueue::of(Runnable::default(), None), None);
    assert_eq!(RunQueue::of(Runnable::default(), Some(f64::NAN)), None);
}

#[test]
fn an_unsupported_or_refused_reader_says_why_from_its_first_reading() {
    let mut r = RunQueueRing::default();
    assert_eq!(
        r.absorb(Taken::at(T0, 1000), RunQueueRead::Unsupported),
        Runnable::Unread(Unread::Unsupported)
    );
    let mut r = RunQueueRing::default();
    assert_eq!(
        r.absorb(Taken::at(T0, 1000), RunQueueRead::UnreadFormat),
        Runnable::Unread(Unread::UnreadFormat)
    );
}
