// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The control wire's percent escape, shared by every speaker of the wire.
//!
//! [`encode`] makes any string one space-free token of a response line: every
//! byte that is not ASCII-graphic, and `%`, becomes uppercase `%XX`. [`decode`]
//! is total (malformed escapes pass through, invalid UTF-8 decodes lossily), so
//! it is safe on hostile bytes.
//!
//! Users: the server (`aterm_control::wire`'s `pct_*` re-exports), `aterm-ctl`,
//! `aterm_link::pct`, the supervisor, the wasm32 web hosts (which cannot link
//! `aterm-control`), and the engine's OSC 7 `file://` path decode.

/// Percent-encode `s` into one space-free token: every byte that is not
/// ASCII-graphic, and `%`, becomes `%XX`. Empty -> "".
#[must_use]
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    encode_into(&mut out, s);
    out
}

/// [`encode`] appending into a caller-owned buffer, with no allocation of its
/// own (`blocks` and `history` encode many records under the terminal lock).
pub fn encode_into(out: &mut String, s: &str) {
    // Uppercase nibble table, equal to `{:02X}` without `core::fmt`.
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for b in s.bytes() {
        if b.is_ascii_graphic() && b != b'%' {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[usize::from(b >> 4)] as char);
            out.push(HEX[usize::from(b & 0x0f)] as char);
        }
    }
}

/// Decode [`encode`]'s output: `%XX` (either case) becomes its byte; anything
/// else passes through and invalid UTF-8 decodes lossily. Total.
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
                // Masks are no-ops; shift-or carries no overflow obligation.
                out.push((((hi & 0x0f) << 4) | (lo & 0x0f)) as u8);
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

    /// The escape's `core::fmt` spelling, the reference every former copy was
    /// written against.
    fn fmt_spelling(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            if b.is_ascii_graphic() && b != b'%' {
                out.push(char::from(b));
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    /// Exact wire bytes (`%20`, `%0A`, `%25`, uppercase hex) over pinned
    /// vectors, every one- and two-byte scalar, and a stride of wider ones.
    #[test]
    fn encode_is_the_wires_spelling_and_decode_inverts_it() {
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
        let narrow = (0u32..0x800).filter_map(char::from_u32);
        let wider = (0x800u32..=0x10_ffff)
            .step_by(61)
            .filter_map(char::from_u32);
        for c in narrow.chain(wider) {
            let s = c.to_string();
            assert_eq!(encode(&s), fmt_spelling(&s), "{c:?}");
            assert_eq!(decode(&encode(&s)), s, "{c:?}");
        }
        for s in [
            "构建 agent ✨",
            "Details ›",
            "\t\n\r\u{7f}",
            "~/src/aterm",
            "-",
        ] {
            assert_eq!(decode(&encode(s)), s, "round-trip: {s:?}");
        }
        let mut into = String::from("k=");
        encode_into(&mut into, "a b");
        assert_eq!(into, "k=a%20b");
    }

    /// Anything else survives: malformed and truncated escapes verbatim, an
    /// escape spelling invalid UTF-8 as U+FFFD, never a panic.
    #[test]
    fn decode_is_total() {
        for s in ["%G1", "%zz", "%2", "%4", "trail%", "%4G", "%é", "%+5"] {
            assert_eq!(decode(s), s, "passes through: {s:?}");
        }
        // A stray `%` does not swallow the escape that follows it.
        assert_eq!(decode("%%41"), "%A");
        assert_eq!(decode("%c3%a9"), "é", "lowercase hex decodes too");
        assert_eq!(decode("%FF"), "\u{FFFD}");
    }
}
