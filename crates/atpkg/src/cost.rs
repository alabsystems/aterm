// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Byte formatting for the cost surface and the disk preflight.

/// The largest unit `n` fills at least once, as its size and name; `(1, "B")` below a KiB.
const fn unit_of(n: u64) -> (u64, &'static str) {
    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    if n >= GIB {
        (GIB, "GiB")
    } else if n >= MIB {
        (MIB, "MiB")
    } else if n >= KIB {
        (KIB, "KiB")
    } else {
        (1, "B")
    }
}

/// `n` written in `unit`: whole bytes as an integer, a larger unit to one decimal.
fn in_unit(n: u64, (size, name): (u64, &str)) -> String {
    if size == 1 {
        format!("{n} B")
    } else {
        format!("{:.1} {name}", n as f64 / size as f64)
    }
}

/// A byte count as a short human string: whole bytes, else `KiB`/`MiB`/`GiB` to one decimal.
#[must_use]
pub fn human_bytes(n: u64) -> String {
    in_unit(n, unit_of(n))
}

/// Two byte counts set side by side (a need and what is free), each by [`human_bytes`]
/// unless both round alike (1174 MiB and 1147 MiB both read `1.1 GiB`); then both are
/// written in the unit of their difference, so unequal counts never print as one figure.
#[must_use]
pub fn human_bytes_apart(a: u64, b: u64) -> (String, String) {
    let (ha, hb) = (human_bytes(a), human_bytes(b));
    if ha != hb {
        return (ha, hb);
    }
    let unit = unit_of(a.abs_diff(b));
    (in_unit(a, unit), in_unit(b, unit))
}

/// Whether `required` bytes fit in `available` while leaving `free_floor` free afterward.
#[must_use]
pub const fn disk_ok(required: u64, available: u64, free_floor: u64) -> bool {
    (available as u128) >= (required as u128) + (free_floor as u128)
}

/// Bytes to leave free after an install, so a multi-GB toolchain never fills the volume.
pub const FREE_FLOOR: u64 = 1 << 30; // 1 GiB

#[cfg(test)]
#[path = "cost_tests.rs"]
mod tests;
