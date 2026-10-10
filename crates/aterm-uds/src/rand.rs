// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CSPRNG bytes for token minting: the one audited entropy surface for the
//! whole workspace.
//!
//! Every caller that needs random bytes or a hex token goes through [`fill`] /
//! [`hex_token`] instead of reading `/dev/urandom` itself: an unbounded
//! `fs::read` of a device that never reaches EOF grows its buffer until memory
//! runs out. Here the read is bounded by construction (`getentropy(2)`, else
//! `read_exact` into the caller's fixed buffer), the hex encoding is checked
//! exact and injective over every byte value
//! (`tests::hex_encode_is_exact_lowercase_and_injective`), and
//! `tools/grep_guard.sh` check B4 (a stage of `tools/verify.sh`) fails if a
//! quoted `/dev/urandom` literal appears outside this module and
//! aterm-tempfile's twin.
//!
//! Unix mints from `getentropy(2)` with a bounded `/dev/urandom` fallback;
//! Windows mints from `BCryptGenRandom` (the system-preferred RNG).

use std::io;

/// `N` CSPRNG bytes as `2N` lowercase-hex chars: the single-use-token shape
/// (control-socket auth token, seamless-handoff nonce, update re-exec nonce).
///
/// # Errors
/// When no OS entropy source is available (see [`fill`]). The caller chooses
/// to fail closed (no token, no socket bind) or degrade with a documented
/// non-secret fallback; it must never retry with its own device read.
pub fn hex_token<const N: usize>() -> io::Result<String> {
    let mut buf = [0u8; N];
    fill(&mut buf)?;
    Ok(hex_encode(&buf))
}

/// Lowercase-hex encode a fixed-size byte array: exactly `2N` chars out for
/// `N` bytes in, most-significant nibble first.
///
/// A nibble-table loop rather than `format!("{b:02x}")`, so the Trust verifier
/// can model it; `tests::hex_encode_is_exact_lowercase_and_injective` checks it
/// exact and injective over every byte value.
#[cfg_attr(trust_verify, trust::skip)] // idiomatic allocation panic (String::with_capacity/push); hex indexing is provably < 16
pub fn hex_encode<const N: usize>(bytes: &[u8; N]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    // Output is identical to the plain `2 * N` / `HEX[usize::from(b >> 4)]`
    // spelling for every input; this form is what the verifier can discharge.
    // The capacity is only a hint: `saturating_mul(2)` equals `2 * N` for any
    // constructible array, and `.min(4096)` bounds the hint while `push` grows
    // on demand.
    let mut out = String::with_capacity(N.saturating_mul(2).min(4096));
    for &b in bytes {
        // `wrapping_shr(4)` on a `u8` is exactly `>> 4`; the `& 0xf` masks are
        // no-ops that make the `HEX` index provably < 16.
        out.push(HEX[usize::from(b.wrapping_shr(4)) & 0xf] as char);
        out.push(HEX[usize::from(b) & 0xf] as char);
    }
    out
}

/// Fill `buf` from the OS CSPRNG.
///
/// # Errors
/// When no OS entropy source is available. The caller must then fail closed
/// (no token, no socket bind), never fall back to something guessable.
#[cfg(unix)]
// Skip: bottoms out at the `getentropy(2)` FFI call, whose C body the verifier
// cannot see.
#[cfg_attr(trust_verify, trust::skip)]
pub fn fill(buf: &mut [u8]) -> io::Result<()> {
    // getentropy(2) fills up to 256 bytes per call from the system CSPRNG
    // with no fd (macOS and modern Linux).
    unsafe extern "C" {
        fn getentropy(buf: *mut core::ffi::c_void, len: usize) -> i32;
    }
    let ok = buf.chunks_mut(256).all(|chunk| {
        // `as *mut c_void` is exactly what `.cast()` does, spelled inline so
        // the verifier sees the pointer's slice provenance.
        // SAFETY: `chunk` is a live &mut [u8] for the duration of the call, so
        // `as_mut_ptr()` is non-null and valid for `chunk.len()` writes;
        // `chunks_mut(256)` caps `len` at 256, getentropy(2)'s per-call maximum.
        let rc = unsafe { getentropy(chunk.as_mut_ptr() as *mut core::ffi::c_void, chunk.len()) };
        rc == 0
    });
    if ok {
        return Ok(());
    }
    // Fallback: read straight from the kernel CSPRNG device.
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom")?;
    f.read_exact(buf)
}

/// Fill `buf` from the OS CSPRNG.
///
/// # Errors
/// When `BCryptGenRandom` reports failure. The caller must then fail closed
/// (no token, no socket bind), never fall back to something guessable.
#[cfg(windows)]
// Skip: bottoms out at the `BCryptGenRandom` FFI call (unverifiable C body).
#[cfg_attr(trust_verify, trust::skip)]
pub fn fill(buf: &mut [u8]) -> io::Result<()> {
    /// NULL algorithm handle + this flag is the documented system-RNG form.
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
    for chunk in buf.chunks_mut(u32::MAX as usize) {
        // SAFETY: `chunk` is a live &mut [u8] for the duration of the call;
        // 0 == STATUS_SUCCESS.
        let status = unsafe {
            crate::win::ffi::BCryptGenRandom(
                core::ptr::null_mut(),
                chunk.as_mut_ptr(),
                chunk.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(io::Error::other(format!(
                "BCryptGenRandom failed (NTSTATUS {status:#010x})"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::hex_encode;

    /// Every token and nonce is `hex_encode` of CSPRNG bytes, so two distinct
    /// inputs must never print alike. Each output byte pair depends on its
    /// input byte alone, so every byte value is the whole domain: each encodes
    /// to two lowercase-hex chars that an independent decoder
    /// (`u8::from_str_radix`) maps back, and a longer input is the
    /// concatenation of its bytes' pairs, `2N` chars for `N` bytes.
    #[test]
    fn hex_encode_is_exact_lowercase_and_injective() {
        let pair = |b: u8| hex_encode(&[b]);
        let mut seen = std::collections::BTreeSet::new();
        for b in 0..=u8::MAX {
            let hex = pair(b);
            assert_eq!(hex.len(), 2, "{b:#04x}");
            assert!(
                hex.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                "{b:#04x} -> {hex:?} is not lowercase hex"
            );
            assert_eq!(u8::from_str_radix(&hex, 16), Ok(b), "{hex:?} decodes back");
            assert!(seen.insert(hex), "{b:#04x} prints like another byte");
        }
        assert_eq!(hex_encode(&[]), "");
        // Every byte value at every position of the two widths the workspace
        // mints (8: the tempfile suffix, 16: the handoff nonce).
        fn concat<const N: usize>() {
            for pos in 0..N {
                for b in 0..=u8::MAX {
                    let mut bytes = [0x5a; N];
                    bytes[pos] = b;
                    let want: String = bytes.iter().map(|&x| hex_encode(&[x])).collect();
                    assert_eq!(want.len(), 2 * N);
                    assert_eq!(hex_encode(&bytes), want, "N={N} pos={pos} b={b:#04x}");
                }
            }
        }
        concat::<8>();
        concat::<16>();
    }
}
