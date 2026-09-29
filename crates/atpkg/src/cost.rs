// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cost consent + disk preflight (§9/§11) — the honest-accounting gates before bytes move.
//!
//! atpkg surfaces the **signed** `[cost]` (`download_bytes`/`disk_installed`, §4.2) and,
//! for a large artifact, asks for consent before downloading; and it preflights disk so a
//! multi-GB toolchain bundle never half-stages on a full volume (§9/§10.2). These are the
//! pure decision + formatting helpers; the actual prompt and the free-space query are the
//! CLI/OS edge.

/// Format a byte count as a short human string (`B`/`KiB`/`MiB`/`GiB`, one decimal) for
/// the cost surface.
#[must_use]
pub fn human_bytes(n: u64) -> String {
    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    if n >= GIB {
        format!("{:.1} GiB", n as f64 / GIB as f64)
    } else if n >= MIB {
        format!("{:.1} MiB", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.1} KiB", n as f64 / KIB as f64)
    } else {
        format!("{n} B")
    }
}

/// Disk preflight (§9/§10.2): whether `required` installed bytes fit in `available` while
/// still leaving at least `free_floor` bytes free afterward. Saturating — a colossal
/// `required` can never wrap to "fits". For a coherence group, pass the **sum** of every
/// staged member's signed `disk_installed`.
#[must_use]
pub fn disk_ok(required: u64, available: u64, free_floor: u64) -> bool {
    available >= required.saturating_add(free_floor)
}

/// Bytes to leave free AFTER an install completes — the disk-preflight reserve so a
/// multi-GB toolchain never fills the volume to 0 (§9). Passed as `free_floor` to
/// [`disk_ok`] at every preflight call site so they all agree on the reserve.
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
        // `{:.1}` rounds half to even on the tenths digit: exactly 1.25 MiB is "1.2".
        assert_eq!(human_bytes(1_310_720), "1.2 MiB");
        assert_eq!(human_bytes(u64::MAX), "17179869184.0 GiB");
    }

    #[test]
    fn disk_preflight_keeps_the_free_floor_and_cannot_overflow() {
        let gib = 1u64 << 30;
        // 3 GiB required, 10 GiB available, keep 1 GiB free ⇒ fits (3+1 <= 10).
        assert!(disk_ok(3 * gib, 10 * gib, gib));
        // 3 GiB required, 3.5 GiB available, keep 1 GiB ⇒ does NOT fit (4 > 3.5).
        assert!(!disk_ok(3 * gib, 7 * gib / 2, gib));
        // A colossal requirement never WRAPS (saturating) to fit on a small disk.
        assert!(!disk_ok(u64::MAX, 1000, 1));
        // Exact fit (required + floor == available) is allowed.
        assert!(disk_ok(3 * gib, 4 * gib, gib));
    }
}
