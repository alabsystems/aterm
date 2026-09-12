//! Small, dependency-free, deterministic hashes.
//!
//! These are *not* cryptographic. They exist so partitioning and frame
//! integrity are stable across processes with no external crate. Each function
//! is named for exactly what it computes.

/// FNV-1a, 64-bit. A stable non-cryptographic hash used for partition
/// assignment. Stable across processes and architectures (no `Hash` RNG).
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET_BASIS;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// The CRC-32/IEEE lookup table (Sarwate's algorithm), built at COMPILE TIME
/// from the reflected polynomial. `const fn`, so the 1 KiB table lands in
/// `.rodata` with zero runtime init and no `once_cell`/`lazy_static` — the
/// per-byte bit-loop is paid once, by the compiler, not on every frame.
const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

const CRC32_TABLE: [u32; 256] = crc32_table();

/// CRC-32, IEEE 802.3 polynomial (reflected `0xEDB88320`). Used for frame
/// payload integrity — computed on every frame encode and decode, so this is
/// the broker's per-record hot path.
///
/// Table-driven (one lookup per byte) rather than bitwise (eight shifts per
/// byte); the index `(crc ^ byte) & 0xFF` is always in `0..=255`, so the
/// `[u32; 256]` lookup is total. Output is byte-identical to the bitwise
/// reference — the canonical `123456789 -> 0xCBF43926` vector and a differential
/// test against that reference both pin it.
#[must_use]
pub fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        let idx = ((crc ^ b as u32) & 0xFF) as usize;
        crc = (crc >> 8) ^ CRC32_TABLE[idx];
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_is_deterministic_and_distinguishes() {
        assert_eq!(fnv1a_64(b"agent-7"), fnv1a_64(b"agent-7"));
        assert_ne!(fnv1a_64(b"agent-7"), fnv1a_64(b"agent-8"));
        assert_ne!(fnv1a_64(b""), fnv1a_64(b"\0"));
    }

    #[test]
    fn crc32_matches_known_vector() {
        // The canonical CRC-32/IEEE check value for the ASCII string "123456789".
        assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32_ieee(b""), 0x0000_0000);
    }

    /// The original bitwise CRC-32/IEEE — the reference the table-driven
    /// [`crc32_ieee`] must reproduce byte-for-byte.
    fn crc32_bitwise_reference(data: &[u8]) -> u32 {
        let mut crc: u32 = 0xFFFF_FFFF;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    #[test]
    fn crc32_table_equals_bitwise_reference() {
        // Every length from empty to past the 256-byte frame payload, filled
        // from a seeded xorshift so the input covers all byte values and the
        // carry across the table lookup is exercised, plus a few fixed vectors.
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0xFF) as u8
        };
        let mut buf = Vec::new();
        for len in 0..=600usize {
            buf.clear();
            for _ in 0..len {
                buf.push(next());
            }
            assert_eq!(
                crc32_ieee(&buf),
                crc32_bitwise_reference(&buf),
                "table CRC diverged from bitwise reference at len {len}"
            );
        }
        for v in [
            &b""[..],
            b"123456789",
            b"\x00",
            b"\xff\xff\xff\xff",
            b"the quick brown fox",
        ] {
            assert_eq!(crc32_ieee(v), crc32_bitwise_reference(v));
        }
    }
}
