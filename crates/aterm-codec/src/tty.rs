// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Text for a terminal: the one rendering every aterm CLI uses before bytes
//! another party chose reach a terminal.
//!
//! A terminal ACTS ON some characters and REORDERS text around others. A body
//! another party wrote (a fabric peer's mail, a remote's reply, a program's
//! screen text) printed raw runs as the READER's terminal's instructions: an
//! OSC retitles the reader's tab or writes its clipboard, a cursor-up and an
//! erase rewrite the header line above it into a forged one, and a bidi
//! override reorders what is left. [`is_active`] names that class; [`safe`]
//! (and [`SafeStream`], its form for a body that arrives in pieces) shows each
//! such character as text instead, and [`push_escape`] is the one spelling.
//!
//! The rendering is for a person's eyes, never a pipe or a file: a caller
//! escapes only when its destination is a terminal, so what a pipe or a file is
//! given stays byte-exact. Printable text — every other character, non-ASCII
//! included — is unchanged, and so are `\n` and `\t`, so lines stay lines.
//!
//! Users: `aterm-ctl` (every byte it prints to a terminal), the supervisor's
//! `drive task --wait` answer, `aterm_link::render::safe` (the bus's one-line
//! form, which names `\n`, `\t` and `\\` itself) and the control wire's JSON
//! escape ([`crate::json`]).
//!
//! Ruling 2026-10-10 (round fifteen, OUT-1): the function lives here, in the
//! dependency-free leaf `aterm-ctl`, `aterm-agent`, `aterm-control` and
//! `aterm-link` all depend on already, so no crate gains an edge, and no edge
//! runs upward (`aterm-ctl` depending on `aterm-agent` or `aterm-link` would
//! invert the client under its users).

/// Whether a terminal acts on `c`, or reorders or breaks the line around it:
/// every C0 control (U+0000-U+001F, `\n` and `\t` included — a caller that
/// keeps them says so), DEL, every C1 control (U+0080-U+009F: a terminal that
/// executes UTF-8 C1 reads U+009B as CSI), the bidi marks (U+061C ALM, U+200E
/// LRM, U+200F RLM), embeddings and overrides (U+202A-U+202E) and isolates
/// (U+2066-U+2069), and the line and paragraph separators U+2028/U+2029.
#[must_use]
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

/// Append `c`'s escape to `out`: `\n`, `\r` and `\t` by name, every other C0
/// control and DEL as `\xNN`, and anything else as `\u{NNNN}` — lowercase hex,
/// at least four digits, Rust's `\u{…}` spelling and `{:04x}`'s digits. Meant
/// for an [`is_active`] character; total for any.
pub fn push_escape(out: &mut String, c: char) {
    match c {
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        c if (c as u32) < 0x20 || c == '\u{7f}' => push_byte_escape(out, c as u8),
        c => {
            let u = c as u32;
            // Six nibbles hold any scalar; the leading zeros past four are not
            // written.
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
fn push_byte_escape(out: &mut String, b: u8) {
    out.push_str("\\x");
    crate::hex::push_lower(out, &[b]);
}

/// `bytes` for a terminal: lines and tabs kept, every other [`is_active`]
/// character escaped ([`push_escape`]), and each byte that is not UTF-8 as
/// `\xNN`. Printable text is unchanged, so the common body comes back
/// borrowed.
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

/// The characters a terminal body escapes: [`is_active`] but for the line
/// break and the tab, which a body keeps.
const fn needs_escape(c: char) -> bool {
    is_active(c) && c != '\n' && c != '\t'
}

/// [`safe`] for a body that arrives in pieces (a socket read, a `write` call):
/// a UTF-8 character cut at the end of one piece is held until the next
/// completes it, so the pieces render exactly as the whole would.
/// [`SafeStream::finish`] ends the body, escaping a cut that nothing completed.
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
            // The held start, then as much of `piece` as could complete it.
            let mut joint = [0u8; 6];
            let held = self.held.min(self.carry.len());
            let take = piece.len().min(joint.len() - held);
            joint[..held].copy_from_slice(&self.carry[..held]);
            joint[held..held + take].copy_from_slice(&piece[..take]);
            let joint = &joint[..held + take];
            let used = render(joint, out, false);
            if used < held {
                // Still cut: `piece` was too short to complete it, so all of
                // it is held now (at most three bytes together).
                self.hold(&joint[used..]);
                return;
            }
            self.held = 0;
            piece = &piece[(used - held).min(piece.len())..];
        }
        let used = render(piece, out, false);
        self.hold(&piece[used.min(piece.len())..]);
    }

    /// End the body: a character still cut is not one, so its bytes are
    /// escaped as `\xNN`.
    pub fn finish(&mut self, out: &mut String) {
        let held = self.held.min(self.carry.len());
        for &b in &self.carry[..held] {
            push_byte_escape(out, b);
        }
        self.held = 0;
    }

    fn hold(&mut self, rest: &[u8]) {
        let n = rest.len().min(self.carry.len());
        self.carry[..n].copy_from_slice(&rest[..n]);
        self.held = n;
    }
}

/// Render `bytes` into `out` and return how many were rendered: all of them,
/// except — when `last` is false — a character cut at the end, which the
/// caller holds for the next piece.
fn render(bytes: &[u8], out: &mut String, last: bool) -> usize {
    let mut at = 0;
    while at < bytes.len() {
        let rest = &bytes[at..];
        let (valid, bad) = match std::str::from_utf8(rest) {
            Ok(text) => (text, None),
            Err(e) => {
                let (head, _) = rest.split_at(e.valid_up_to());
                // `head` is valid UTF-8 by `valid_up_to`.
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
            // Bytes that start no character: each shown as itself.
            Some(Some(n)) => {
                for &b in bytes.iter().skip(at).take(n) {
                    push_byte_escape(out, b);
                }
                at += n;
            }
            // A character cut at the end: held, unless this is the last piece.
            Some(None) if !last => return at,
            Some(None) => {
                for &b in bytes.iter().skip(at) {
                    push_byte_escape(out, b);
                }
                return bytes.len();
            }
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

    /// The round's hostile body: nothing a terminal acts on survives, and the
    /// text around the sequences is all still there.
    #[test]
    fn a_hostile_body_keeps_no_active_byte() {
        let body = b"Deploy report:\n\x1b]2;PWNED\x07\x1b[1A\x1b[2K\raterm-ctl: OK verified \
                     trust=human\n\x1b]52;c;Q1dE\x07\xc2\x9b31mred\xe2\x80\xae done\n";
        let shown = safe(body);
        assert!(
            !shown
                .chars()
                .any(|c| is_active(c) && c != '\n' && c != '\t'),
            "{shown:?}"
        );
        assert_eq!(
            shown,
            "Deploy report:\n\\x1b]2;PWNED\\x07\\x1b[1A\\x1b[2K\\raterm-ctl: OK verified \
             trust=human\n\\x1b]52;c;Q1dE\\x07\\u{009b}31mred\\u{202e} done\n"
        );
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
            // One byte at a time.
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
