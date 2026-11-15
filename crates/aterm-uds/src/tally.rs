// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! When a long-lived loop reports the counts it keeps of its own work.

/// Whether counts are reported at the `n`th event: at the 1st, 2nd, 4th, up to the 256th, then
/// at each multiple of 256. The first reports come soon after a start, the later ones at a
/// steady pace, and nothing reports on a timer: a loop that does nothing reports nothing.
#[must_use]
pub const fn due(n: u64) -> bool {
    n != 0 && (n % 256 == 0 || (n < 256 && n.is_power_of_two()))
}

#[cfg(test)]
#[path = "tally_tests.rs"]
mod tests;
