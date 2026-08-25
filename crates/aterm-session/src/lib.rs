// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hierarchical-session routing fabric — the per-edge authority + write seam.
//! (design `docs/design/HIERARCHICAL_SESSIONS.md` §§2, 4, 6.3, 7).
//!
//! This crate is the frontend/transport substrate that lets one terminal drive
//! another: stable [`SessionId`]s + per-launch [`LaunchNonce`]s ([`id`]); the
//! per-edge, op-scoped, fail-closed authority table ([`Op`]/[`Edge`]/[`EdgeToken`]/
//! [`EdgeTable`]/[`decide_edge`]); and the single byte sink that serializes every
//! writer to one PTY master with whole-frame atomicity ([`sink::SinkWriter`]).
//!
//! It performs NO filesystem or socket I/O itself — the GUI owns those (headless
//! invariant). It is the policy + serialization core that the control socket and
//! the file veneer call into.
//!
//! ## Status
//!
//! Design-proposed (Phase 0). The COARSE compile-time class gate lives in
//! `aterm_cap::effects::{ReadScreen, WriteInput, SignalEdge}`; the FINE per-edge
//! object identity (which `src → dst` for which op) is here. Per §7.7, cross-session
//! `WriteInput` by untrusted IN-PROCESS code was blocked on no-mint-reachability
//! (§5.4); that is now GREEN — the `aterm_cap::Authority` mint is sealed behind the
//! `launcher-mint` feature and `ty`-proven (`aterm_spec::derive::mint_reachability_model`),
//! so no in-process code can reach it. The tier remains unbuilt only because aterm
//! runs no untrusted in-process code today (it spawns and confines an OS shell). The
//! same-uid, cross-process path rides the runtime [`EdgeToken`] over the uid-checked
//! control socket and is sound independent of §5.4.

#![forbid(unsafe_code)]
// Under the Trust verifier, register the `trust` tool namespace so the
// `#[cfg_attr(trust_verify, trust::skip)]` opt-out on `fill_random` resolves;
// plain rustc never sets `trust_verify`, so this is inert off-Trust.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

mod edge;
mod id;
pub mod sink;

pub use edge::{ConnectionKind, Edge, EdgeDecision, EdgeTable, EdgeToken, Op, decide_edge};
pub use id::{LaunchNonce, SessionId};

/// Lowercase-hex digit for a nibble value (low 4 bits; the mask is a no-op at
/// both call sites, which pass values already `<= 15`). A branch table keeps
/// this free of arithmetic panic obligations.
fn hex_digit(n: u8) -> char {
    match n & 0x0f {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        10 => 'a',
        11 => 'b',
        12 => 'c',
        13 => 'd',
        14 => 'e',
        _ => 'f',
    }
}

/// Lowercase-hex encode. No dependency — ids/tokens are short and fixed-length.
pub(crate) fn hex(bytes: &[u8]) -> String {
    // Constant capacity hint: covers the largest fixed-length input we encode
    // (32-byte EdgeToken -> 64 hex chars). The capacity is a hint only
    // (`push` grows past it if ever needed), so this is a provably bounded
    // allocation with identical results for every caller.
    let mut s = String::with_capacity(64);
    for &b in bytes {
        // div/rem of a byte value: both nibbles are <= 15.
        s.push(hex_digit(b / 16));
        s.push(hex_digit(b % 16));
    }
    s
}

/// Decode a single ASCII hex digit (either case) to its value (`0..=15`).
/// Same accept set and values as `char::from(c).to_digit(16)`, but in `u8`
/// arithmetic whose range each match arm bounds, so the subtractions are
/// provably underflow-free. Shared workspace-wide (aterm-net, aterm-gui also
/// decode hex); keep this the one copy.
pub const fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Decode lowercase/uppercase hex into `out`, requiring exactly `out.len() * 2`
/// hex chars. Returns `None` on any length or non-hex error.
pub(crate) fn from_hex(s: &str, out: &mut [u8]) -> Option<()> {
    // `out.len() * 2` written overflow-free: a real slice can never be long
    // enough to overflow, so `checked_mul` is behavior-identical.
    if Some(s.len()) != out.len().checked_mul(2) {
        return None;
    }
    for (slot, chunk) in out.iter_mut().zip(s.as_bytes().as_chunks::<2>().0) {
        // `as_chunks::<2>()` yields `&[u8; 2]` arrays — the destructuring is
        // irrefutable, no fallback arm needed.
        let &[hi, lo] = chunk;
        let hi = hex_nibble(hi)?;
        let lo = hex_nibble(lo)?;
        // Nibbles are <= 15; the masks are no-ops that make the combination
        // locally provable. Shift-or (not `* 16 +`): the constant shift lowers
        // to pure LIA and BitOr carries NO overflow obligation, where the
        // verifier fails to chain the Mul's range fact into the Add. The low
        // 4 bits of `hi << 4` are zero, so `|` == `+` — byte-identical.
        *slot = ((hi & 0x0f) << 4) | (lo & 0x0f);
    }
    Some(())
}

/// Constant-time byte equality for secret / anti-spoof comparisons: no early-out on
/// the first differing byte. A length mismatch returns `false` immediately (the
/// values compared here are fixed-length, so length is not itself a secret).
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        // Explicit derefs: `x ^ y` on `&u8` dispatches through the std
        // `<&u8 as BitXor<&u8>>::bitxor` impl — an absent callee to the
        // fail-closed verifier; `*x ^ *y` is the primitive op it lowers
        // natively. Identical bytes, identical constant-time shape.
        diff |= *x ^ *y;
    }
    // NO `std::hint::black_box` here, deliberately — this is the one of aterm's
    // three constant-time comparators without one, and the asymmetry is a Trust
    // constraint, not an oversight. `aterm_digest::ct_eq` and
    // `aterm_core::terminal::shell_integration_auth::constant_time_eq_32` both
    // end `black_box(diff) == 0`; neither crate is gated by
    // `tools/trust-gate-ratchet.tsv`. This one is, at a 92-proved floor, and
    // `black_box` is a Rust-ABI callee with no body in the verification bundle
    // — the FATAL absent-callee class (docs/measurements/
    // 2026-07-09-extern-c-absent-callee-totality.md: only non-unwinding
    // C-family ABIs discharge; "a resolved-but-out-of-bundle Rust body ... →
    // fatal"). Adding it here trades a proof for a barrier. The same fail-closed
    // verifier is why the XOR below is spelled `*x ^ *y`.
    //
    // Measured 2026-08-25 at -O3 on the stage2 aarch64 toolchain: this loop is
    // branch-free as written, exiting only on the index bound. If that ever
    // stops holding, the fix is to take the barrier AND re-measure the crate's
    // proved count, not to add one silently.
    diff == 0
}

/// Fill `buf` with cryptographically-secure random bytes from the OS CSPRNG.
/// Panics only if the OS RNG is unavailable (a fatal startup condition).
// Skip: bottoms out at `rand_core::OsRng::fill_bytes` (absent body wrapping
// the OS entropy syscall) whose only panic is the documented fatal
// no-OS-RNG condition — the same audited OsRng assumption as
// aterm-shell-integration's `generate_nonce`. Verify-only; behavior unchanged.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn fill_random(buf: &mut [u8]) {
    use rand_core::RngCore;
    rand_core::OsRng.fill_bytes(buf);
}
