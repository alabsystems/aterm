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
mod tests {
    use super::*;

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1 << 10), "1.0 KiB");
        assert_eq!(human_bytes(1 << 20), "1.0 MiB");
        assert_eq!(human_bytes(3 * (1 << 30)), "3.0 GiB");
        assert_eq!(human_bytes((3 << 30) / 2), "1.5 GiB");
        assert_eq!(human_bytes(1_310_720), "1.2 MiB");
        assert_eq!(human_bytes(u64::MAX), "17179869184.0 GiB");
    }

    #[test]
    fn human_bytes_apart_never_prints_unequal_counts_alike() {
        let mib = 1u64 << 20;
        assert_eq!(
            human_bytes_apart(1400 * mib, 900 * mib),
            (human_bytes(1400 * mib), human_bytes(900 * mib))
        );
        assert_eq!(human_bytes(1174 * mib), human_bytes(1147 * mib));
        assert_eq!(
            human_bytes_apart(1174 * mib, 1147 * mib),
            ("1174.0 MiB".to_string(), "1147.0 MiB".to_string())
        );
        let (a, b) = human_bytes_apart(1174 * mib, 1174 * mib - 100);
        assert_eq!((a.as_str(), b.as_str()), ("1231028224 B", "1231028124 B"));
    }

    #[test]
    fn disk_preflight_keeps_the_free_floor_and_cannot_overflow() {
        let gib = 1u64 << 30;
        assert!(disk_ok(3 * gib, 10 * gib, gib));
        assert!(!disk_ok(3 * gib, 7 * gib / 2, gib));
        assert!(!disk_ok(u64::MAX, 1000, 1));
        assert!(!disk_ok(u64::MAX, u64::MAX, 1));
        assert!(disk_ok(u64::MAX, u64::MAX, 0));
        assert!(disk_ok(3 * gib, 4 * gib, gib));
    }
}
