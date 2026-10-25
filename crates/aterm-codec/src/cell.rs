// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `cell` wire's attribute vocabulary and strict style reader.

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
    /// The attribute list names inverse: the cell's colours swapped, as a caret a program draws
    /// itself over a glyph.
    pub inverse: bool,
    /// The attribute list names wide: the leading cell of a glyph that spans two columns.
    pub wide: bool,
}

/// The attribute list's set, one bit per [`Attribute`]; `None` for an attribute the wire does not
/// name, one named twice, `none` beside another, or both halves of a wide glyph.
fn attributes_of(raw: &str) -> Option<u16> {
    if raw == NONE {
        return Some(0);
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
    Some(seen)
}

const fn has(set: u16, attr: Attribute) -> bool {
    set & (1_u16 << attr as u8) != 0
}

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

/// Parses a complete `OK <glyph> <fg> <bg> <attrs>` reply (with optional `link=<percent-token>`).
/// Refuses missing colours, unknown attributes, extra fields, and unfinished frames (`frame=open`).
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
    let set = attributes_of(attrs)?;
    if let Some(extra) = fields.next()
        && (!token(extra.strip_prefix("link=")?) || fields.next().is_some())
    {
        return None;
    }
    Some(Style {
        fg,
        bg,
        dim: has(set, Attribute::Dim),
        inverse: has(set, Attribute::Inverse),
        wide: has(set, Attribute::Wide),
    })
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
                dim: true,
                inverse: false,
                wide: false,
            })
        );
        let plain = parse_style(
            "OK %E2%9D%AF d0d0d0 111318 bold,underline link=https%3A%2F%2Fexample.test\n",
        )
        .expect("actual complete shape");
        assert!(!plain.dim && !plain.inverse && !plain.wide);
        assert_eq!(plain.fg, "d0d0d0");
        assert!(parse_style("OK x 8A8F99 111318 none\r\n").is_some());
    }

    /// Claude Code 2.1.310's drawn caret (`inverse`) over a narrow and a wide glyph, read from the
    /// one attribute set. NEGATIVE CONTROLS: a wide glyph's continuation is not its leading cell,
    /// and a plain or dim cell is no caret.
    #[test]
    fn a_cell_style_says_inverse_and_wide() {
        let caret = parse_style("OK k 111318 d0d0d0 inverse").expect("a caret");
        assert!(caret.inverse && !caret.wide && !caret.dim);
        let wide = parse_style("OK %E4%BD%A0 111318 d0d0d0 bold,inverse,wide").expect("wide");
        assert!(wide.inverse && wide.wide);
        let cont = parse_style("OK  111318 d0d0d0 inverse,wide_cont").expect("continuation");
        assert!(cont.inverse && !cont.wide);
        for plain in ["OK e 999999 111318 dim", "OK k d0d0d0 111318 none"] {
            let style = parse_style(plain).expect("a plain cell");
            assert!(!style.inverse && !style.wide, "{plain}");
        }
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

    #[test]
    fn a_cell_of_an_unfinished_frame_proves_nothing() {
        for whole in [
            "OK x d0d0d0 111318 none",
            "OK  999999 111318 dim,wide_cont",
            "OK %E2%8F%BA e5e5e5 000000 bold,wide",
            "OK %E2%9D%AF d0d0d0 111318 bold,underline link=https%3A%2F%2Fexample.test",
        ] {
            assert!(parse_style(&format!("{whole}\n")).is_some(), "{whole}");
            let open = format!("{whole} frame=open\n");
            assert_eq!(parse_style(&open), None, "{open}");
        }
    }
}
