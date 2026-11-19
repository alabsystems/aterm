// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Duration, SystemTime, UNIX_EPOCH};

std::thread_local! {
    static LAST_WALL_READ: core::cell::Cell<Option<SystemTime>> = const { core::cell::Cell::new(None) };
}

pub(super) fn record_wall_read(now: SystemTime) {
    LAST_WALL_READ.set(Some(now));
}

fn observed_elapsed() -> Option<Duration> {
    LAST_WALL_READ
        .take()
        .expect("the accessor read the calling thread's clock")
        .duration_since(UNIX_EPOCH)
        .ok()
}

/// Pure epoch conversion agrees across all three units on one reading,
/// rejects pre-epoch readings, saturates milliseconds at `u64::MAX`, and
/// converts one live wall-clock reading on either side of the epoch.
#[test]
fn the_epoch_accessors_read_the_wall_clock() {
    let check = |reading, elapsed: Option<Duration>| {
        assert_eq!(super::since_epoch_at(reading), elapsed);
        assert_eq!(super::unix_secs_at(reading), elapsed.map(|d| d.as_secs()));
        assert_eq!(
            super::unix_millis_at(reading),
            elapsed.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        );
    };

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        let before_epoch = UNIX_EPOCH
            .checked_sub(Duration::from_secs(1))
            .expect("native SystemTime represents 1969");
        check(before_epoch, None);
    }

    check(UNIX_EPOCH, Some(Duration::ZERO));

    // Whole 100 ns ticks: Windows' SystemTime counts no finer, and drops the rest of an add.
    let sample = Duration::new(1_790_000_000, 123_456_700);
    let at_sample = UNIX_EPOCH.checked_add(sample).expect("post-epoch sample");
    check(at_sample, Some(sample));

    assert_eq!(
        super::duration_to_millis_saturating(Duration::from_secs(u64::MAX)),
        u64::MAX
    );
    let max_whole_ms_secs = u64::MAX / 1_000;
    assert_eq!(
        super::duration_to_millis_saturating(Duration::new(max_whole_ms_secs, 614_999_999)),
        u64::MAX - 1
    );
    assert_eq!(
        super::duration_to_millis_saturating(Duration::new(max_whole_ms_secs, 615_999_999)),
        u64::MAX
    );
    assert_eq!(
        super::duration_to_millis_saturating(Duration::new(max_whole_ms_secs, 616_000_000)),
        u64::MAX
    );

    let now = SystemTime::now();
    check(now, now.duration_since(UNIX_EPOCH).ok());
}

/// Each call of `since_epoch`, `unix_secs` or `unix_millis` adds exactly one
/// to the calling thread's `raw_reads`. The `*_at` conversions of a reading
/// the caller already holds add nothing. A thread starts at zero, and its
/// count moves only with its own reads.
///
/// Negative controls, each run against this test on 2026-11-14 and then
/// restored:
/// - `unix_millis` reading `SystemTime::now()` instead of `wall_now()` failed
///   "unix_millis is one counted read" (left 1, right 2).
/// - The count kept in one process-wide `AtomicU64` instead of the
///   thread-local failed "a fresh thread starts at zero, whatever other
///   threads read" (left 3, right 0).
/// - `unix_secs_at` also adding one failed "unix_secs is one counted read"
///   (left 2, right 1).
#[test]
fn raw_reads_counts_each_wall_read_on_the_calling_thread() {
    let start = super::raw_reads();
    let moved = || super::raw_reads().wrapping_sub(start);

    let actual = super::unix_secs();
    assert_eq!(actual, observed_elapsed().map(|d| d.as_secs()));
    assert_eq!(moved(), 1, "unix_secs is one counted read");
    let actual = super::unix_millis();
    assert_eq!(
        actual,
        observed_elapsed().map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    );
    assert_eq!(moved(), 2, "unix_millis is one counted read");
    let actual = super::since_epoch();
    assert_eq!(actual, observed_elapsed());
    assert_eq!(moved(), 3, "since_epoch is one counted read");

    let held = UNIX_EPOCH
        .checked_add(Duration::from_secs(1_790_000_000))
        .expect("post-epoch reading");
    let _ = super::since_epoch_at(held);
    let _ = super::unix_secs_at(held);
    let _ = super::unix_millis_at(held);
    let _ = super::duration_to_millis_saturating(Duration::from_secs(1));
    assert_eq!(
        moved(),
        3,
        "converting a reading the caller holds reads no clock"
    );

    // std on wasm32-unknown-unknown cannot spawn a thread.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        const READS: u64 = 5;
        let (fresh, own) = std::thread::spawn(|| {
            let fresh = super::raw_reads();
            for _ in 0..READS {
                let _ = super::unix_millis();
            }
            (fresh, super::raw_reads())
        })
        .join()
        .expect("the reading thread returns");
        assert_eq!(
            fresh, 0,
            "a fresh thread starts at zero, whatever other threads read"
        );
        assert_eq!(own, READS, "a thread counts its own reads");
        assert_eq!(
            moved(),
            3,
            "another thread's reads leave this thread's count alone"
        );
    }
}
