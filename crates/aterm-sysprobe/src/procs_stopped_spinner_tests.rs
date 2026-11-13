// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::wait::MUST_HAPPEN;

/// [`aterm_uds::deadline::spin_stopped`] under [`MUST_HAPPEN`]: spins a child until `cpu_ns(pid)` reads at least `min_ns`, failing by name with the last reading if it does not, stops it, hands its pid to `read` while it is stopped, then kills and reaps it, answering `read`'s value and the CPU time, in nanoseconds, that the reap's `wait4` rusage counts for it.
pub(crate) fn read<T>(
    min_ns: u64,
    cpu_ns: impl FnMut(u32) -> Option<u64>,
    read: impl FnOnce(u32) -> T,
) -> (T, u64) {
    aterm_uds::deadline::spin_stopped(min_ns, cpu_ns, read, MUST_HAPPEN)
}

/// Whether `read` is within 2x of `kernel`, either way: far inside a Mach tick factor (125/3 on Apple silicon).
pub(crate) fn within_2x(read: u64, kernel: u64) -> bool {
    read <= kernel.saturating_mul(2) && read.saturating_mul(2) >= kernel
}
