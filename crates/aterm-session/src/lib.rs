// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! Hierarchical-session routing fabric: [`SessionId`], [`LaunchNonce`], per-edge fail-closed
//! authority ([`EdgeTable`], [`decide_edge`]), and the atomic PTY write sink ([`sink::SinkWriter`],
//! [`input_backlog`]).

#![forbid(unsafe_code)]
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
    let mut s = String::with_capacity(64);
    for &b in bytes {
        s.push(hex_digit(b / 16));
        s.push(hex_digit(b % 16));
    }
    s
}

/// Decode one ASCII hex digit (either case) to `0..=15`, like
/// `char::from(c).to_digit(16)` but in provably underflow-free `u8` arithmetic.
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
        let &[hi, lo] = chunk;
        let hi = hex_nibble(hi)?;
        let lo = hex_nibble(lo)?;
        *slot = ((hi & 0x0f) << 4) | (lo & 0x0f);
    }
    Some(())
}

/// Constant-time byte equality for fixed-length secret comparisons.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= *x ^ *y;
    }
    diff == 0
}

/// Fill `buf` with cryptographically secure random bytes from the OS CSPRNG,
/// via [`aterm_uds::rand::fill`], the workspace's one audited entropy surface.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn fill_random(buf: &mut [u8]) {
    aterm_uds::rand::fill(buf).expect("OS CSPRNG unavailable: cannot mint session identifiers");
}
