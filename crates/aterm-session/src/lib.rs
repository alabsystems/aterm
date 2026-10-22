// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The anchor attributes are `macro_rules!` ones where Cargo resolved no proc macro
// under them (crates/aterm-spec-anchor/anchor_rules.rs).
#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! Hierarchical-session routing fabric: the per-edge authority and write seam
//! (`docs/design/HIERARCHICAL_SESSIONS.md` §§2, 4, 6.3, 7).
//!
//! Stable [`SessionId`]s and per-launch [`LaunchNonce`]s; the per-edge, op-scoped,
//! fail-closed authority table ([`EdgeTable`], [`decide_edge`]); and the single byte
//! sink that serializes every writer to one PTY master with whole-frame atomicity
//! ([`sink::SinkWriter`]) and dates the input its program has not read yet
//! ([`input_backlog`]).
//!
//! No filesystem or socket I/O (headless invariant): the GUI owns those and calls
//! into this core.
//!
//! ## Status
//!
//! The per-edge object identity (which `src → dst` for which op) is the whole
//! gate; `Spawn` is the one effect marker a seam requires. The
//! `aterm_cap::Authority` mint is sealed behind the `launcher-mint` feature,
//! so no in-process code can
//! reach it; a tier for untrusted in-process writers is unbuilt because aterm
//! runs none. The same-uid cross-process path rides the runtime [`EdgeToken`]
//! over the uid-checked control socket and is sound independently of the mint.

#![forbid(unsafe_code)]
// Under the Trust verifier, register the `trust` tool namespace so the crate's
// `trust::skip`/`trust::paired` attributes resolve; inert under plain rustc.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

mod edge;
mod id;
pub mod input_backlog;
pub mod sink;

pub use edge::{ConnectionKind, Edge, EdgeDecision, EdgeTable, EdgeToken, Op, decide_edge};
pub use id::{LaunchNonce, SessionId, is_minted_sid};

/// Lowercase-hex digit for a nibble (low 4 bits). A branch table keeps this
/// free of arithmetic panic obligations.
#[inline]
const fn hex_digit(n: u8) -> char {
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

/// Lowercase-hex encode; ids and tokens are short and fixed-length.
pub(crate) fn hex(bytes: &[u8]) -> String {
    // Capacity hint for the largest input (32-byte EdgeToken -> 64 hex chars);
    // a constant keeps the allocation provably bounded.
    let mut s = String::with_capacity(64);
    for &b in bytes {
        // div/rem of a byte value: both nibbles are <= 15.
        s.push(hex_digit(b / 16));
        s.push(hex_digit(b % 16));
    }
    s
}

/// Decode one ASCII hex digit (either case) to `0..=15`, like
/// `char::from(c).to_digit(16)` but in provably underflow-free `u8` arithmetic.
/// This crate keeps its own hex helpers instead of `aterm_codec::hex` because
/// its proved count needs their bodies in the verification bundle.
#[inline]
pub(crate) const fn hex_nibble(c: u8) -> Option<u8> {
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
    // `checked_mul` is behavior-identical for any real slice length.
    if Some(s.len()) != out.len().checked_mul(2) {
        return None;
    }
    for (slot, chunk) in out.iter_mut().zip(s.as_bytes().as_chunks::<2>().0) {
        // `as_chunks::<2>()` yields `&[u8; 2]`, so the destructuring is irrefutable.
        let &[hi, lo] = chunk;
        let hi = hex_nibble(hi)?;
        let lo = hex_nibble(lo)?;
        // Masks make the nibble range locally provable; shift-or (not `* 16 +`)
        // carries no overflow obligation for the verifier and equals `+` here.
        *slot = ((hi & 0x0f) << 4) | (lo & 0x0f);
    }
    Some(())
}

/// Constant-time byte equality for secret and anti-spoof comparisons: no
/// early-out on the first differing byte. A length mismatch returns `false`
/// immediately; the values compared are fixed-length, so length is not secret.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        // Explicit derefs: `&u8 ^ &u8` dispatches through a std impl the
        // verifier treats as an absent callee; `*x ^ *y` is the primitive op.
        diff |= *x ^ *y;
    }
    // No `std::hint::black_box`, unlike aterm's other constant-time comparators: it
    // has no body in the verification bundle and would cost proofs against this
    // crate's floor in aterm-release's `trust/floors.tsv`. The loop compiles
    // branch-free; if that stops holding, add the barrier and re-measure.
    diff == 0
}

/// Fill `buf` with cryptographically secure random bytes from the OS CSPRNG,
/// via [`aterm_uds::rand::fill`], the workspace's one audited entropy surface.
/// Panics only if the OS RNG is unavailable.
// Skip: bottoms out in the `getentropy(2)` FFI call the verifier cannot see; the
// only panic is the deliberate `expect` on a missing OS RNG. Verify-only.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn fill_random(buf: &mut [u8]) {
    // Fail loud rather than mint a guessable session id or launch nonce.
    aterm_uds::rand::fill(buf).expect("OS CSPRNG unavailable: cannot mint session identifiers");
}
