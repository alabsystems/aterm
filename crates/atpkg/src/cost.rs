// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cost consent + disk preflight (§9/§11) — the honest-accounting gates before bytes move.
//!
//! atpkg surfaces the **signed** `[cost]` (`download_bytes`/`disk_installed`, §4.2) and,
//! for a large artifact, asks for consent before downloading; and it preflights disk so a
//! multi-GB toolchain bundle never half-stages on a full volume (§9/§10.2). These are the
//! pure decision + formatting helpers; the actual prompt and the free-space query are the
//! CLI/OS edge.

/// Format a byte count as a short human string (`B`/`KiB`/`MiB`/`GiB`) for the cost surface.
///
/// Byte-identical to the previous `format!("{:.1} GiB", n as f64 / GIB as f64)`
/// spelling for every `n` (see [`one_decimal`]) — rewritten `format!`-free because the
/// `format!` expansion embeds `fmt::Arguments` construction (with inlined `unsafe`)
/// that the strict Trust gate cannot lower and fails closed on.
#[must_use]
pub fn human_bytes(n: u64) -> String {
    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    if n >= GIB {
        let mut s = one_decimal(n, 30);
        s.push_str(" GiB");
        s
    } else if n >= MIB {
        let mut s = one_decimal(n, 20);
        s.push_str(" MiB");
        s
    } else if n >= KIB {
        let mut s = one_decimal(n, 10);
        s.push_str(" KiB");
        s
    } else {
        let mut s = crate::dec_u64(n);
        s.push_str(" B");
        s
    }
}

/// Render `n / 2^k` with exactly one fractional digit — byte-identical to
/// `format!("{:.1}", n as f64 / (1u64 << k) as f64)` for every `n`, in straight-line
/// integer arithmetic the strict gate can prove.
///
/// Why this is exact, step by step:
/// 1. `n as f64` rounds `n` to 53 significant bits, ties to even (IEEE 754
///    round-to-nearest-even) — emulated below, kept as `(mant, exp)` with
///    `mant <= 2^53`, `exp <= 11`, so `mant << exp` never has to materialize
///    (for `n` near `u64::MAX` it is exactly `2^64`).
/// 2. Dividing an f64 by `2^k` only changes the exponent — exact, no rounding
///    (the quotient here is far from subnormal). So the f64 being formatted is
///    exactly the dyadic rational `mant / 2^(k - exp)`.
/// 3. `{:.1}` cuts the exact decimal expansion at one fractional digit, rounding
///    to nearest with ties to even on the kept tenths digit (`flt2dec` exact
///    mode; verified against `format!` in `one_decimal_matches_format`).
///
/// All shifts are `wrapping_*` (total, no panic obligations); shift amounts are
/// in-range on every reachable input, as argued inline.
fn one_decimal(n: u64, k: u32) -> String {
    // 1. Emulate `n as f64`. `lz >= 11` means `n < 2^53`: exactly representable.
    let lz = n.leading_zeros();
    let (mant, exp) = if lz >= 11 {
        (n, 0u32)
    } else {
        let excess = 11 - lz; // 1..=11
        let keep = n.wrapping_shr(excess);
        let rem = n & 1u64.wrapping_shl(excess).wrapping_sub(1);
        let half = 1u64.wrapping_shl(excess.wrapping_sub(1));
        let round_up = rem > half || (rem == half && keep & 1 == 1);
        // `keep <= 2^53 - 1`, so the increment cannot wrap; saturating spells
        // that for the prover (identical value on every reachable input).
        let m = if round_up {
            keep.saturating_add(1)
        } else {
            keep
        };
        // A carry to `m == 2^53` is fine: we keep the value as `m * 2^excess`
        // and never materialize the (possibly 65-bit) product.
        (m, excess)
    };
    // 2. The formatted value is exactly `mant / 2^sh`. `exp > 0` only when
    //    `n >= 2^53`, which forces the GiB branch (`k == 30`), so `sh >= 19`;
    //    in the KiB/MiB branches `exp == 0` and `sh == k >= 10`.
    let sh = k.saturating_sub(exp);
    // 3. Tenths digit cut, round to nearest, ties to even:
    //    `t10 / 2^sh` with the remainder deciding the direction.
    //    `mant <= 2^53` by construction; the clamp is a no-op that hands the
    //    prover the dominating bound, so `mant * 10 < 2^57` cannot overflow.
    let mant = if mant <= (1u64 << 53) {
        mant
    } else {
        1u64 << 53
    };
    // `mant <= 2^53`, so this cannot saturate (`10 * 2^53 < 2^57 < 2^64`); the
    // saturating spelling replaces the overflow obligation the gate could not
    // carry through the clamp, with identical value on every reachable input.
    let t10 = mant.saturating_mul(10);
    let d = t10.wrapping_shr(sh);
    let rem = t10 & 1u64.wrapping_shl(sh).wrapping_sub(1);
    let half = 1u64.wrapping_shl(sh.wrapping_sub(1));
    // `d < 2^57 / 2^sh <= 2^47`, so the round-up increment cannot wrap.
    let d = if rem > half || (rem == half && d & 1 == 1) {
        d.saturating_add(1)
    } else {
        d
    };
    // 4. Render "<integer part>.<tenths digit>".
    let mut s = crate::dec_u64(d / 10);
    s.push('.');
    s.push(char::from(b'0'.wrapping_add((d % 10) as u8)));
    s
}

/// Whether an install must ask for cost consent before downloading: its download **or**
/// installed size meets/exceeds `threshold` (the "this is big — proceed?" gate, §11). Below
/// the threshold the install stays silent (batteries-included). A `metered` fleet config
/// would lower the threshold to 0 (always consent); this is the pure predicate.
#[must_use]
pub fn needs_consent(download_bytes: u64, disk_installed: u64, threshold: u64) -> bool {
    download_bytes >= threshold || disk_installed >= threshold
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
    }

    /// The manual `one_decimal` rendering must be byte-identical to the previous
    /// `format!("{:.1}", n as f64 / unit as f64)` spelling — including decimal
    /// ties (round half to even: 1.25 MiB renders "1.2") and the `u64 -> f64`
    /// precision loss above 2^53.
    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn human_bytes_matches_float_format() {
        const KIB: u64 = 1 << 10;
        const MIB: u64 = 1 << 20;
        const GIB: u64 = 1 << 30;
        let cases: &[u64] = &[
            0,
            1,
            512,
            KIB - 1,
            KIB,
            KIB + 1,
            1536,
            MIB - 1,
            MIB,
            MIB + 1,
            1_310_720, // exactly 1.25 MiB — a decimal tie, rounds half-to-even to "1.2"
            3_932_160, // exactly 3.75 MiB — tie the other way, rounds to "3.8"
            GIB - 1,
            GIB,
            GIB + 1,
            (3 << 30) / 2,
            3 * GIB,
            (1 << 53) - 1,
            1 << 53,
            (1 << 53) + 1, // above 2^53: `as f64` rounds; emulation must match
            (1 << 53) + 3,
            u64::MAX - 1,
            u64::MAX,
        ];
        for &n in cases {
            let expected = if n >= GIB {
                format!("{:.1} GiB", n as f64 / GIB as f64)
            } else if n >= MIB {
                format!("{:.1} MiB", n as f64 / MIB as f64)
            } else if n >= KIB {
                format!("{:.1} KiB", n as f64 / KIB as f64)
            } else {
                format!("{n} B")
            };
            assert_eq!(human_bytes(n), expected, "n = {n}");
        }
    }

    #[test]
    fn consent_triggers_above_threshold_on_either_dimension() {
        let mb = 100 * (1 << 20);
        // Below on both ⇒ silent.
        assert!(!needs_consent(1 << 20, 5 << 20, mb));
        // Download alone over ⇒ consent.
        assert!(needs_consent(mb, 0, mb));
        // Installed alone over ⇒ consent (a small download that expands hugely).
        assert!(needs_consent(1 << 20, mb, mb));
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
