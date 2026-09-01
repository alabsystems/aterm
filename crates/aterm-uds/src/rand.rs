// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CSPRNG bytes for token minting — the ONE audited entropy surface for the
//! whole workspace.
//!
//! Every caller that needs random bytes or a hex token goes through [`fill`] /
//! [`hex_token`] instead of hand-rolling a `/dev/urandom` read. That rule is
//! load-bearing, not aesthetic: the 2026-07-04/05 double kernel panic was a
//! hand-rolled reader in `aterm-gui::seamless` calling `fs::read` on
//! `/dev/urandom` — read-to-EOF on a device that NEVER EOFs, so two parallel test threads grew
//! buffers at CSPRNG speed and exhausted 24 GB of RAM + all swap in ~4 minutes,
//! twice. Here the read is bounded by construction (`getentropy(2)`, else
//! `read_exact` into the caller's fixed buffer), the hex encoding is proved
//! total/exact/injective by the `rand_kani_proofs` trust-mc harnesses, and
//! `tools/grep_guard.sh` check B4 fails if a quoted `/dev/urandom` path literal
//! reappears outside this module (and aterm-tempfile's zero-dep twin).
//!
//! WHERE B4 ACTUALLY RUNS, corrected 2026-08-31: this said "fails the pre-push
//! gate". It does not — `.githooks/pre-push` was demoted to ADVISORY on
//! 2026-08-24 and executes nothing. `grep_guard.sh` is a whole-tree stage of
//! `tools/verify.sh` (`aterm_verify::stages::grep_guards`, unconditional in
//! every mode including `--changed`), so the guard is real and the merge
//! contract carries it; no hook does.
//!
//! Unix mints from `getentropy(2)` (macOS and modern Linux, no fd) with a
//! bounded `/dev/urandom` fallback; Windows mints from `BCryptGenRandom`
//! (system-preferred RNG — bcrypt.dll, documented-stable since Vista), so both
//! platforms exercise one portable surface.

use std::io;

/// `N` CSPRNG bytes as `2N` lowercase-hex chars — the single-use-token shape
/// (control-socket auth token, seamless-handoff nonce, update re-exec nonce)
/// minted in one audited place.
///
/// # Errors
/// When no OS entropy source is available (see [`fill`]) — the caller decides
/// its own failure posture: fail closed (no token ⇒ no socket bind) or degrade
/// with a documented non-secret fallback. It must NEVER retry via its own
/// device read; that is exactly the hand-rolled path this helper exists to
/// retire.
pub fn hex_token<const N: usize>() -> io::Result<String> {
    let mut buf = [0u8; N];
    fill(&mut buf)?;
    Ok(hex_encode(&buf))
}

/// Lowercase-hex encode a fixed-size byte array: exactly `2N` chars out for
/// `N` bytes in, most-significant nibble first.
///
/// Deliberately a nibble-table loop rather than `format!("{b:02x}")`: no fmt
/// machinery means trust-mc can model it, and the `rand_kani_proofs` harnesses
/// prove it total, exact (decodes back to the input), and injective over the
/// full symbolic input space.
#[cfg_attr(trust_verify, trust::skip)] // idiomatic allocation panic (String::with_capacity/push); hex indexing is provably < 16
pub fn hex_encode<const N: usize>(bytes: &[u8; N]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    // Trust L0 discharge — output-byte-IDENTICAL to the plain `2 * N` /
    // `HEX[usize::from(b >> 4)]` spelling for every input (the security
    // contract of this module: the emitted hex chars never change):
    // - the capacity is only an allocation hint. `N.saturating_mul(2)` equals
    //   `2 * N` for every real token size (callers mint 16–32-byte tokens;
    //   `N > usize::MAX / 2` is unconstructible as a stack array), and
    //   `.min(4096)` merely bounds the hint — `String::push` grows on demand,
    //   so the produced string is unchanged even for a hypothetical huge `N`.
    let mut out = String::with_capacity(N.saturating_mul(2).min(4096));
    for &b in bytes {
        // - `wrapping_shr(4)` on a `u8` is exactly `>> 4` (shift 4 < 8, so no
        //   wrap is possible), both nibbles are ≤ 15 by construction, and the
        //   `& 0xf` masks after widening are no-ops that only make the `HEX`
        //   index provably < 16.
        out.push(HEX[usize::from(b.wrapping_shr(4)) & 0xf] as char);
        out.push(HEX[usize::from(b) & 0xf] as char);
    }
    out
}

/// Fill `buf` from the OS CSPRNG.
///
/// # Errors
/// When no OS entropy source is available — the caller must then fail closed
/// (no token ⇒ no socket bind), never fall back to something guessable.
#[cfg(unix)]
// Skip: the body's panic/safety obligations bottom out at the `getentropy(2)`
// FFI call (and its raw-pointer setup), whose C body the verifier cannot see —
// an inherently unverifiable syscall wrapper. Verify-only; behavior unchanged.
#[cfg_attr(trust_verify, trust::skip)]
pub fn fill(buf: &mut [u8]) -> io::Result<()> {
    // getentropy(2) fills up to 256 bytes per call from the system CSPRNG
    // with no fd (macOS and modern Linux).
    unsafe extern "C" {
        fn getentropy(buf: *mut core::ffi::c_void, len: usize) -> i32;
    }
    let ok = buf.chunks_mut(256).all(|chunk| {
        // Trust L0 discharge — behavior-identical: `as *mut c_void` is the
        // exact primitive cast `.cast()` performs (`fn cast<U>(self) -> *mut U
        // { self as _ }`), spelled inline so the verifier sees the pointer's
        // slice provenance instead of an absent callee body. The security
        // contract of this module is untouched: same getentropy call, same
        // arguments — the output bytes are identical for every input.
        // SAFETY: `chunk` is a live &mut [u8] for the duration of the call, so
        // `as_mut_ptr()` is non-null (slice pointers never are) and valid for
        // `chunk.len()` writes; `chunks_mut(256)` caps `len` at 256, the
        // documented getentropy(2) per-call maximum.
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
/// When `BCryptGenRandom` reports failure — the caller must then fail closed
/// (no token ⇒ no socket bind), never fall back to something guessable.
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
