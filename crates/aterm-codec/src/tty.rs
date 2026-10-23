// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Text for a terminal.

/// Whether a terminal acts on `c`, or reorders or breaks the line around it.
#[must_use]
#[inline]
pub const fn is_active(c: char) -> bool {
    let u = c as u32;
    u < 0x20
        || (u >= 0x7f && u <= 0x9f)
        || u == 0x061c
        || u == 0x200e
        || u == 0x200f
        || (u >= 0x2028 && u <= 0x202e)
        || (u >= 0x2066 && u <= 0x2069)
}

/// Append `c`'s escape to `out`.
pub fn push_escape(out: &mut String, c: char) {
    match c {
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        c if (c as u32) < 0x20 || c == '\u{7f}' => push_byte_escape(out, c as u8),
        c => {
            let u = c as u32;
            let skip = if u > 0xf_ffff {
                0
            } else if u > 0xffff {
                1
            } else {
                2
            };
            out.push_str("\\u{");
            for shift in [20u32, 16, 12, 8, 4, 0].iter().skip(skip) {
                out.push(crate::hex::digit_lower(((u >> shift) & 0xf) as u8));
            }
            out.push('}');
        }
    }
}

/// Append `b` as `\xNN` (lowercase hex): a C0 control, DEL, or a byte that is
/// not part of any UTF-8 character.
#[inline]
fn push_byte_escape(out: &mut String, b: u8) {
    out.push_str("\\x");
    crate::hex::push_lower(out, &[b]);
}

/// `bytes` for a terminal.
#[must_use]
pub fn safe(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    if let Ok(text) = std::str::from_utf8(bytes)
        && !text.chars().any(needs_escape)
    {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(bytes.len().saturating_add(16));
    let mut stream = SafeStream::new();
    stream.push(bytes, &mut out);
    stream.finish(&mut out);
    std::borrow::Cow::Owned(out)
}

/// Text that can only ever be ONE line of ordinary characters.
#[must_use]
pub fn one_line(text: &str, cap: usize) -> String {
    let mut out = String::with_capacity(text.len().min(cap.saturating_mul(4)));
    for (n, c) in text.chars().enumerate() {
        if n >= cap {
            out.push('…');
            break;
        }
        match c {
            '\\' => out.push_str("\\\\"),
            c if is_active(c) => push_escape(&mut out, c),
            c => out.push(c),
        }
    }
    if out.contains("trust=") {
        out = out.replace("trust=", "trust\\=");
    }
    out
}

/// The characters a terminal body escapes: [`is_active`] but for the line
/// break and the tab, which a body keeps.
#[inline]
const fn needs_escape(c: char) -> bool {
    is_active(c) && c != '\n' && c != '\t'
}

/// [`safe`] for a body that arrives in pieces (a socket read, a `write` call).
#[derive(Debug, Default, Clone)]
pub struct SafeStream {
    /// The start of a character the last piece cut (at most three bytes).
    carry: [u8; 3],
    /// How many bytes of `carry` are held.
    held: usize,
}

impl SafeStream {
    /// A stream with nothing held.
    #[must_use]
    #[inline]
    pub const fn new() -> Self {
        Self {
            carry: [0; 3],
            held: 0,
        }
    }

    /// Append `piece`, rendered, to `out`, holding a character it cuts.
    pub fn push(&mut self, piece: &[u8], out: &mut String) {
        let mut piece = piece;
        if self.held > 0 {
            let mut joint = [0u8; 6];
            let held = self.held.min(self.carry.len());
            let take = piece.len().min(joint.len() - held);
            joint[..held].copy_from_slice(&self.carry[..held]);
            joint[held..held + take].copy_from_slice(&piece[..take]);
            let joint = &joint[..held + take];
            let used = render(joint, out);
            if used < held {
                self.hold(&joint[used..]);
                return;
            }
            self.held = 0;
            piece = &piece[(used - held).min(piece.len())..];
        }
        let used = render(piece, out);
        self.hold(&piece[used.min(piece.len())..]);
    }

    /// End the body: a character still cut is not one, so its bytes are escaped as `\xNN`.
    #[inline]
    pub fn finish(&mut self, out: &mut String) {
        let held = self.held.min(self.carry.len());
        for &b in &self.carry[..held] {
            push_byte_escape(out, b);
        }
        self.held = 0;
    }

    #[inline]
    fn hold(&mut self, rest: &[u8]) {
        let n = rest.len().min(self.carry.len());
        self.carry[..n].copy_from_slice(&rest[..n]);
        self.held = n;
    }
}

/// Render `bytes` into `out` and return how many were rendered: all of them,
/// except a character cut at the end, which the caller holds for the next piece.
fn render(bytes: &[u8], out: &mut String) -> usize {
    let mut at = 0;
    while at < bytes.len() {
        let rest = &bytes[at..];
        let (valid, bad) = match std::str::from_utf8(rest) {
            Ok(text) => (text, None),
            Err(e) => {
                let (head, _) = rest.split_at(e.valid_up_to());
                (std::str::from_utf8(head).unwrap_or(""), Some(e.error_len()))
            }
        };
        for c in valid.chars() {
            if needs_escape(c) {
                push_escape(out, c);
            } else {
                out.push(c);
            }
        }
        at += valid.len();
        match bad {
            None => break,
            Some(Some(n)) => {
                for &b in bytes.iter().skip(at).take(n) {
                    push_byte_escape(out, b);
                }
                at += n;
            }
            Some(None) => return at,
        }
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The class, character by character: what is active and what is text.
    #[test]
    fn the_active_class_is_controls_bidi_and_the_separators() {
        for c in [
            '\u{0}', '\u{7}', '\t', '\n', '\r', '\u{1b}', '\u{1f}', '\u{7f}', '\u{80}', '\u{85}',
            '\u{9b}', '\u{9d}', '\u{9f}', '\u{61c}', '\u{200e}', '\u{200f}', '\u{2028}',
            '\u{2029}', '\u{202a}', '\u{202e}', '\u{2066}', '\u{2069}',
        ] {
            assert!(is_active(c), "{:04x} is active", c as u32);
        }
        for c in [
            ' ', 'a', '~', '\u{a0}', 'é', '\u{200b}', '\u{200d}', '\u{2027}', '\u{202f}',
            '\u{2065}', '\u{206a}', '界', '🙂',
        ] {
            assert!(!is_active(c), "{:04x} is text", c as u32);
        }
    }

    /// The escape table: each class's spelling, and printable text unchanged.
    #[test]
    fn safe_spells_each_class_and_keeps_text() {
        for (raw, shown) in [
            (
                &b"plain text, \xc3\xa9 \xe7\x95\x8c \xf0\x9f\x99\x82"[..],
                "plain text, é 界 🙂",
            ),
            (b"two\nlines\tand a tab\n", "two\nlines\tand a tab\n"),
            (b"cr\r", "cr\\r"),
            (b"\x1b]2;T\x07", "\\x1b]2;T\\x07"),
            (b"\x00\x1f\x7f", "\\x00\\x1f\\x7f"),
            (
                "\u{9b}31m\u{9d}0;t\u{9c}".as_bytes(),
                "\\u{009b}31m\\u{009d}0;t\\u{009c}",
            ),
            (
                "a\u{202e}b\u{2066}c\u{61c}d\u{200f}".as_bytes(),
                "a\\u{202e}b\\u{2066}c\\u{061c}d\\u{200f}",
            ),
            ("x\u{2028}y\u{2029}".as_bytes(), "x\\u{2028}y\\u{2029}"),
            (b"bad \xff byte", "bad \\xff byte"),
            (b"cut \xe2\x80", "cut \\xe2\\x80"),
            (b"overlong \xc0\xaf", "overlong \\xc0\\xaf"),
            (b"surrogate \xed\xa0\x80", "surrogate \\xed\\xa0\\x80"),
        ] {
            assert_eq!(safe(raw), shown, "{raw:?}");
        }
        assert!(matches!(safe(b"ok\n"), std::borrow::Cow::Borrowed(_)));
    }

    /// In pieces, cut at every byte: the same rendering as the whole, so a
    /// character split across two reads is never shown as two bad bytes.
    #[test]
    fn a_stream_cut_anywhere_renders_as_the_whole() {
        let body = "a\u{9b}é界🙂\u{202e}\x1b\n\u{2028}z".as_bytes();
        let mut bad = body.to_vec();
        bad.extend_from_slice(b"\xff\xf0\x9f");
        for whole in [body, &bad[..]] {
            let want = safe(whole).into_owned();
            for cut in 0..=whole.len() {
                let mut out = String::new();
                let mut s = SafeStream::new();
                s.push(&whole[..cut], &mut out);
                s.push(&whole[cut..], &mut out);
                s.finish(&mut out);
                assert_eq!(out, want, "cut at {cut}");
            }
            let mut out = String::new();
            let mut s = SafeStream::new();
            for b in whole {
                s.push(std::slice::from_ref(b), &mut out);
            }
            s.finish(&mut out);
            assert_eq!(out, want, "byte by byte");
        }
    }

    /// `push_escape` names the three whitespace controls and spells the rest.
    #[test]
    fn push_escape_spells_every_class() {
        let mut out = String::new();
        for c in [
            '\n',
            '\r',
            '\t',
            '\u{1b}',
            '\u{7f}',
            '\u{85}',
            '\u{202e}',
            '\u{1f642}',
        ] {
            push_escape(&mut out, c);
        }
        assert_eq!(out, "\\n\\r\\t\\x1b\\x7f\\u{0085}\\u{202e}\\u{1f642}");
    }
}
