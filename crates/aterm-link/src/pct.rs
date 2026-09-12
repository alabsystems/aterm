// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE ESCAPE aterm's control protocol and the fabric body share.
//!
//! Byte-for-byte the pair in `aterm_control::wire` (`pct_encode` at `wire.rs:28`,
//! `pct_decode` at `:64`). It is COPIED rather than imported because this crate's
//! dependency surface is pinned by the design (§11.2: `astream-broker`,
//! `aterm-uds`, `aterm-types`, and nothing else), and `aterm-control` is not on
//! that list — pulling it in to reach thirty lines would drag aterm's control
//! stack into the bridge. The copy is held honest by
//! [`tests::the_pair_matches_aterms_own_vectors`], which pins the exact strings
//! aterm produces.

/// Percent-encode every byte that is not an ASCII graphic, and `%` itself.
/// UPPERCASE hex, matching aterm's table (`aterm_uds::rand::hex_encode`'s is
/// lowercase and must not be confused with it).
#[must_use]
pub fn encode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_graphic() && b != b'%' {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[usize::from(b >> 4)] as char);
            out.push(HEX[usize::from(b & 0x0f)] as char);
        }
    }
    out
}

/// Decode [`encode`]'s output. TOTAL: a malformed escape passes through verbatim
/// and invalid UTF-8 decodes lossily, so it is safe on bytes a hostile peer
/// authored.
#[must_use]
pub fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The copy is only safe while it agrees with the original. These are the
    /// exact outputs `aterm_control::wire::pct_encode` produces — a space is
    /// `%20`, a newline `%0A`, a literal percent `%25`, and hex is UPPERCASE —
    /// so a drift in either direction fails here rather than at a peer's parser.
    #[test]
    fn the_pair_matches_aterms_own_vectors() {
        for (plain, encoded) in [
            ("hello", "hello"),
            ("two words", "two%20words"),
            ("a\nb", "a%0Ab"),
            ("100%", "100%25"),
            ("tab\there", "tab%09here"),
            ("é", "%C3%A9"),
            ("", ""),
        ] {
            assert_eq!(encode(plain), encoded, "encode {plain:?}");
            assert_eq!(decode(encoded), plain, "decode {encoded:?}");
        }
        // A malformed escape survives verbatim rather than eating the tail.
        assert_eq!(decode("%zz"), "%zz");
        assert_eq!(decode("%4"), "%4");
        // Round trip over every byte a body may hold.
        let all: String = (0u8..=127).map(char::from).collect();
        assert_eq!(decode(&encode(&all)), all);
    }
}
