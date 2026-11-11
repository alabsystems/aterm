// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Duration, SystemTime, UNIX_EPOCH};

/// Pure epoch conversion agrees across all three units on one reading,
/// rejects pre-epoch readings, saturates milliseconds at `u64::MAX`, and
/// reads post-epoch values from the live wall clock without assuming
/// monotonicity across separate syscalls.
#[test]
fn the_epoch_accessors_read_the_wall_clock() {
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        let before_epoch = UNIX_EPOCH
            .checked_sub(Duration::from_secs(1))
            .expect("native SystemTime represents 1969");
        assert_eq!(super::since_epoch_at(before_epoch), None);
        assert_eq!(super::unix_secs_at(before_epoch), None);
        assert_eq!(super::unix_millis_at(before_epoch), None);
    }

    assert_eq!(super::since_epoch_at(UNIX_EPOCH), Some(Duration::ZERO));
    assert_eq!(super::unix_secs_at(UNIX_EPOCH), Some(0));
    assert_eq!(super::unix_millis_at(UNIX_EPOCH), Some(0));

    let sample = Duration::new(1_790_000_000, 123_456_789);
    let at_sample = UNIX_EPOCH.checked_add(sample).expect("post-epoch sample");
    assert_eq!(super::since_epoch_at(at_sample), Some(sample));
    assert_eq!(super::unix_secs_at(at_sample), Some(1_790_000_000));
    assert_eq!(super::unix_millis_at(at_sample), Some(1_790_000_000_123));

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
    let d = super::since_epoch_at(now).expect("wall clock is at or after the Unix epoch");
    assert_eq!(super::unix_secs_at(now), Some(d.as_secs()));
    assert_eq!(
        super::unix_millis_at(now),
        Some(super::duration_to_millis_saturating(d))
    );
    assert!(super::since_epoch().is_some());
    assert!(super::unix_secs().is_some());
    assert!(super::unix_millis().is_some());
}
