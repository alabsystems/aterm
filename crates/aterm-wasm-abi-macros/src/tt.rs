// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A token tree of plain strings: what the parser reads and the expansion
//! writes. `proc_macro`'s own types only work inside a macro invocation, so
//! the parser works on these and the tests build them with `lex`.

use std::fmt::Write as _;

/// A delimiter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delim {
    /// `( )`.
    Paren,
    /// `[ ]`.
    Bracket,
    /// `{ }`.
    Brace,
    /// An invisible group (a `macro_rules!` fragment).
    None,
}

/// One token tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tt {
    /// An identifier or keyword, `r#` included.
    Ident(String),
    /// A punctuation character; `true` when joined to the next one (`->`,
    /// `::`, the `'` of a lifetime).
    Punct(char, bool),
    /// A literal, as written.
    Lit(String),
    /// A delimited group.
    Group(Delim, Vec<Tt>),
}

impl Tt {
    /// Whether this is the identifier `name`.
    pub fn is_ident(&self, name: &str) -> bool {
        matches!(self, Tt::Ident(i) if i == name)
    }

    /// Whether this is the punctuation `c`.
    pub const fn is_punct(&self, c: char) -> bool {
        matches!(self, Tt::Punct(p, _) if *p == c)
    }
}

/// From the compiler's tokens.
pub fn from_stream(stream: proc_macro::TokenStream) -> Vec<Tt> {
    use proc_macro::{Delimiter, Spacing, TokenTree};
    stream
        .into_iter()
        .map(|t| match t {
            TokenTree::Ident(i) => Tt::Ident(i.to_string()),
            TokenTree::Punct(p) => Tt::Punct(p.as_char(), p.spacing() == Spacing::Joint),
            TokenTree::Literal(l) => Tt::Lit(l.to_string()),
            TokenTree::Group(g) => Tt::Group(
                match g.delimiter() {
                    Delimiter::Parenthesis => Delim::Paren,
                    Delimiter::Bracket => Delim::Bracket,
                    Delimiter::Brace => Delim::Brace,
                    Delimiter::None => Delim::None,
                },
                from_stream(g.stream()),
            ),
        })
        .collect()
}

/// Source text that lexes back to `tts`.
pub fn to_source(tts: &[Tt]) -> String {
    let mut out = String::new();
    write_source(&mut out, tts);
    out
}

fn write_source(out: &mut String, tts: &[Tt]) {
    let mut glued = false;
    for t in tts {
        if !glued && !out.is_empty() && !out.ends_with(['(', '[', '{', ' ']) {
            out.push(' ');
        }
        glued = false;
        match t {
            Tt::Ident(i) => out.push_str(i),
            Tt::Lit(l) => out.push_str(l),
            Tt::Punct(c, joint) => {
                out.push(*c);
                glued = *joint;
            }
            Tt::Group(d, inner) => {
                let (open, close) = match d {
                    Delim::Paren => ("(", ")"),
                    Delim::Bracket => ("[", "]"),
                    Delim::Brace => ("{", "}"),
                    Delim::None => ("", ""),
                };
                out.push_str(open);
                write_source(out, inner);
                out.push_str(close);
            }
        }
    }
}

/// The value of a string literal token (`"…"` with escapes, or `r#"…"#`),
/// or `None` for any other literal: aterm-macro-support's reader, which is
/// plain string work and so runs in these native tests too.
pub fn str_value(lit: &str) -> Option<String> {
    aterm_macro_support::lit::parse_str_source(lit)
        .filter(|l| l.suffix.is_empty())
        .map(|l| l.value)
}

/// A Rust byte-string literal for `bytes`.
pub fn byte_lit(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 3);
    out.push_str("b\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(char::from(b)),
            _ => {
                let _ = write!(out, "\\x{b:02x}");
            }
        }
    }
    out.push('"');
    out
}

/// A Rust string literal for `text`.
pub fn str_lit(text: &str) -> String {
    format!("{text:?}")
}

/// Tokens from source text, for tests: identifiers, literals, punctuation
/// (joined as rustc joins it), groups and `//` comments. Enough Rust for
/// signatures and attributes.
#[cfg(test)]
pub fn lex(src: &str) -> Vec<Tt> {
    let chars: Vec<char> = src.chars().collect();
    let mut pos = 0;
    let out = lex_until(&chars, &mut pos, None);
    assert_eq!(pos, chars.len(), "unbalanced input");
    out
}

#[cfg(test)]
fn lex_until(chars: &[char], pos: &mut usize, close: Option<char>) -> Vec<Tt> {
    const PUNCT: &str = "+-*/%^!&|=<>@.,;:#$?~'";
    let mut out = Vec::new();
    while *pos < chars.len() {
        let c = chars[*pos];
        if Some(c) == close {
            *pos += 1;
            return out;
        }
        if c.is_whitespace() {
            *pos += 1;
        } else if c == '/' && chars.get(*pos + 1) == Some(&'/') {
            while *pos < chars.len() && chars[*pos] != '\n' {
                *pos += 1;
            }
        } else if let Some((d, end)) = match c {
            '(' => Some((Delim::Paren, ')')),
            '[' => Some((Delim::Bracket, ']')),
            '{' => Some((Delim::Brace, '}')),
            _ => None,
        } {
            *pos += 1;
            out.push(Tt::Group(d, lex_until(chars, pos, Some(end))));
        } else if c == '"'
            || (c == 'r'
                && chars[*pos + 1..]
                    .iter()
                    .find(|&&h| h != '#')
                    .is_some_and(|&q| q == '"'))
        {
            let start = *pos;
            if c == 'r' {
                *pos += 1;
                let mut hashes = 0;
                while chars[*pos] == '#' {
                    hashes += 1;
                    *pos += 1;
                }
                *pos += 1;
                loop {
                    if chars[*pos] == '"'
                        && chars[*pos + 1..].iter().take(hashes).all(|&h| h == '#')
                    {
                        *pos += 1 + hashes;
                        break;
                    }
                    *pos += 1;
                }
            } else {
                *pos += 1;
                while chars[*pos] != '"' {
                    *pos += if chars[*pos] == '\\' { 2 } else { 1 };
                }
                *pos += 1;
            }
            out.push(Tt::Lit(chars[start..*pos].iter().collect()));
        } else if c == '\'' && chars.get(*pos + 2) == Some(&'\'') {
            out.push(Tt::Lit(chars[*pos..*pos + 3].iter().collect()));
            *pos += 3;
        } else if c.is_ascii_digit() {
            let start = *pos;
            while *pos < chars.len() && (chars[*pos].is_ascii_alphanumeric() || chars[*pos] == '_')
            {
                *pos += 1;
            }
            out.push(Tt::Lit(chars[start..*pos].iter().collect()));
        } else if c.is_alphabetic() || c == '_' {
            let start = *pos;
            while *pos < chars.len()
                && (chars[*pos].is_alphanumeric() || chars[*pos] == '_' || chars[*pos] == '#')
            {
                *pos += 1;
            }
            out.push(Tt::Ident(chars[start..*pos].iter().collect()));
        } else if PUNCT.contains(c) {
            *pos += 1;
            let next = chars.get(*pos).copied();
            let joint =
                c == '\'' || next.is_some_and(|n| PUNCT.contains(n) && n != '\'' && n != '#');
            out.push(Tt::Punct(c, joint));
        } else {
            panic!("lex: unexpected {c:?}");
        }
    }
    assert!(close.is_none(), "unclosed group");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_lexes_back_to_the_same_tokens() {
        for src in [
            "Option<Vec<u8>>",
            "&'a str",
            "Result<Self, String>",
            "fn f(&mut self, x: &[u8]) -> u32 { 1 }",
            "::aterm_wasm_abi::__rt::Part",
            "#[doc = \" a \\\"quoted\\\" line\"]",
        ] {
            let tts = lex(src);
            assert_eq!(lex(&to_source(&tts)), tts, "{src}");
        }
    }

    #[test]
    fn string_literals_unescape() {
        assert_eq!(str_value(r#"" plain""#).as_deref(), Some(" plain"));
        assert_eq!(
            str_value(r#""a\"b\\c\n\u{1F600}\x41""#).as_deref(),
            Some("a\"b\\c\n😀A")
        );
        assert_eq!(
            str_value(r##"r#" raw "with" quotes"#"##).as_deref(),
            Some(" raw \"with\" quotes")
        );
        assert_eq!(str_value(r#"r" raw""#).as_deref(), Some(" raw"));
        assert_eq!(str_value("42"), None);
    }

    #[test]
    fn byte_literals_escape_what_rust_would_misread() {
        assert_eq!(byte_lit(b"a\"\\\x00\xff"), r#"b"a\"\\\x00\xff""#);
        assert_eq!(byte_lit("—".as_bytes()), r#"b"\xe2\x80\x94""#);
    }
}
