// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `cell` wire's attribute names, shared by its producer and style reader.
//! A style is evidence only with the exact fields the producer emits: resolved
//! RGB colours, known attributes, and at most its optional percent-encoded link.

/// The wire's spelling for no active attributes.
pub const NONE: &str = "none";

/// A cell attribute named by the wire, including its width markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Attribute {
    /// Bold text.
    Bold,
    /// Dim text.
    Dim,
    /// Italic text.
    Italic,
    /// Any underline style.
    Underline,
    /// Blinking text.
    Blink,
    /// Inverse colours.
    Inverse,
    /// Struck-through text.
    Strike,
    /// Hidden text.
    Hidden,
    /// A double-width leading cell.
    Wide,
    /// Its continuation cell.
    WideCont,
}

impl Attribute {
    /// The producer's token.
    #[must_use]
    #[inline]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Bold => "bold",
            Self::Dim => "dim",
            Self::Italic => "italic",
            Self::Underline => "underline",
            Self::Blink => "blink",
            Self::Inverse => "inverse",
            Self::Strike => "strike",
            Self::Hidden => "hidden",
            Self::Wide => "wide",
            Self::WideCont => "wide_cont",
        }
    }

    /// A token of this wire, never an inferred unknown attribute.
    #[must_use]
    #[inline]
    pub fn parse(word: &str) -> Option<Self> {
        [
            Self::Bold,
            Self::Dim,
            Self::Italic,
            Self::Underline,
            Self::Blink,
            Self::Inverse,
            Self::Strike,
            Self::Hidden,
            Self::Wide,
            Self::WideCont,
        ]
        .into_iter()
        .find(|attr| attr.word() == word)
    }
}

/// The resolved style fields of one valid `cell` reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style<'a> {
    /// Foreground RGB, exactly six hex digits.
    pub fg: &'a str,
    /// Background RGB, exactly six hex digits.
    pub bg: &'a str,
    /// The attribute list explicitly names dim.
    pub dim: bool,
}

fn dim_of(raw: &str) -> Option<bool> {
    if raw == NONE {
        return Some(false);
    }
    let mut seen = 0_u16;
    for word in raw.split(',') {
        let attr = Attribute::parse(word)?;
        let bit = 1_u16 << attr as u8;
        if seen & bit != 0 {
            return None;
        }
        seen |= bit;
    }
    let wide = (1_u16 << Attribute::Wide as u8) | (1_u16 << Attribute::WideCont as u8);
    if seen & wide == wide {
        return None;
    }
    Some(seen & (1_u16 << Attribute::Dim as u8) != 0)
}

// The producer's percent encoder emits ASCII graphic bytes and complete %XX
// escapes. The blank glyph is an empty token. No decoding/allocation is needed.
fn token(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let mut at = 0;
    while let Some(&b) = bytes.get(at) {
        if b == b'%' {
            if !bytes.get(at + 1).is_some_and(u8::is_ascii_hexdigit)
                || !bytes.get(at + 2).is_some_and(u8::is_ascii_hexdigit)
            {
                return false;
            }
            at += 3;
        } else {
            if !b.is_ascii_graphic() {
                return false;
            }
            at += 1;
        }
    }
    true
}

/// A style only from a complete, well-formed `OK <glyph> <fg> <bg> <attrs>`
/// reply, optionally followed by one `link=<percent-token>`. Empty glyphs are
/// valid; missing colours, future/unknown attributes and extra fields are not.
#[must_use]
pub fn parse_style(line: &str) -> Option<Style<'_>> {
    let body = line
        .strip_suffix("\r\n")
        .or_else(|| line.strip_suffix('\n'))
        .unwrap_or(line);
    if body.contains(['\r', '\n']) {
        return None;
    }
    let mut fields = body.strip_prefix("OK ")?.split(' ');
    let glyph = fields.next()?;
    let fg = fields.next()?;
    let bg = fields.next()?;
    let attrs = fields.next()?;
    if !token(glyph) || !crate::hex::is_hex(fg, 6) || !crate::hex::is_hex(bg, 6) {
        return None;
    }
    let dim = dim_of(attrs)?;
    if let Some(extra) = fields.next()
        && (!token(extra.strip_prefix("link=")?) || fields.next().is_some())
    {
        return None;
    }
    Some(Style { fg, bg, dim })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_cell_styles_keep_dim_and_resolved_colours() {
        assert_eq!(
            parse_style("OK  999999 111318 dim,wide_cont\n"),
            Some(Style {
                fg: "999999",
                bg: "111318",
                dim: true
            })
        );
        let plain = parse_style(
            "OK %E2%9D%AF d0d0d0 111318 bold,underline link=https%3A%2F%2Fexample.test\n",
        )
        .expect("actual complete shape");
        assert!(!plain.dim);
        assert_eq!(plain.fg, "d0d0d0");
        assert!(parse_style("OK x 8A8F99 111318 none\r\n").is_some());
    }

    #[test]
    fn incomplete_or_unknown_cell_styles_prove_neither_draft_nor_placeholder() {
        for raw in [
            "ERR no such verb",
            "OK x d0d0d0 111318",
            "OK x d0d0d0 111318 ",
            "OK x  111318 none",
            "OK x d0d0d0  none",
            "OK x nothex 111318 none",
            "OK x d0d0d0 - none",
            "OK x d0d0d0 111318 unrecognized",
            "OK x d0d0d0 111318 none unexpected",
            "OK x d0d0d0 111318 dim,,bold",
            "OK x d0d0d0 111318 dim,dim",
            "OK x d0d0d0 111318 none,bold",
            "OK x d0d0d0 111318 wide,wide_cont",
            "OK x d0d0d0 111318 none link=a link=b",
            "OK %zz d0d0d0 111318 dim",
            "OK x d0d0d0 111318 none\nOK x d0d0d0 111318 dim\n",
        ] {
            assert_eq!(parse_style(raw), None, "{raw}");
        }
    }
}
