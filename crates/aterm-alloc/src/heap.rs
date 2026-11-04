// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! What a hash table allocates, for charging the heap a value holds ([`table_bytes`]).

/// The widest control group std's `HashMap` reads at once (SSE2's; NEON and the generic group read 8).
const GROUP_WIDTH: usize = 16;

/// The bytes std's `HashMap` allocates for a table whose `capacity()` is `capacity`, holding entries of type `T`.
///
/// It counts the buckets that capacity implies (a SwissTable keeps 1 in 8 of them free past 8),
/// one control byte per bucket, a trailing control group and the padding that aligns the
/// controls, each at its widest, so it is never below the allocation of a table holding no
/// erased entries. An erase can leave a tombstone that takes capacity without returning its
/// bucket, so a table erased from can hold more buckets than its capacity names. A capacity of
/// `0` allocates nothing.
#[must_use]
pub const fn table_bytes<T>(capacity: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    let wanted = if capacity < 8 {
        capacity + 1
    } else {
        capacity.saturating_mul(8) / 7
    };
    let Some(buckets) = wanted.checked_next_power_of_two() else {
        return usize::MAX;
    };
    let align = if core::mem::align_of::<T>() > GROUP_WIDTH {
        core::mem::align_of::<T>()
    } else {
        GROUP_WIDTH
    };
    buckets
        .saturating_mul(core::mem::size_of::<T>())
        .saturating_add(align - 1)
        .saturating_add(buckets)
        .saturating_add(GROUP_WIDTH)
}
