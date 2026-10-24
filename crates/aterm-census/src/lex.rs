// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The census lexer: a Rust file's code view (comments and literal contents blanked) and text
//! view (comments blanked).

/// What a non-code span is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpanKind {
    /// A `//` or (nested) `/* */` comment, doc comments included.
    Comment,
    /// A string literal: cooked, byte, C or raw.
    Str,
    /// A char or byte-char literal.
    Char,
}

/// Byte range of a comment or literal, plus quote offsets for a string.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Span {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) kind: SpanKind,
    pub(crate) quotes: Option<(usize, usize)>,
}

const fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

const fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Every comment and literal of `src`, in order.
pub(crate) fn spans(src: &str) -> Vec<Span> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let found = if is_ident_start(c) && (i == 0 || !is_ident(b[i - 1])) {
            let mut w = i;
            while w < b.len() && is_ident(b[w]) {
                w += 1;
            }
            let lit = match (&b[i..w], b.get(w)) {
                (b"r" | b"br" | b"cr", _) => raw_string(b, w),
                (b"b" | b"c", Some(b'"')) => cooked_string(b, w),
                (b"b", Some(b'\'')) => char_literal(b, w),
                _ => None,
            };
            if lit.is_none() {
                i = w;
                continue;
            }
            lit
        } else {
            match (c, b.get(i + 1)) {
                (b'/', Some(b'/')) => Some(Lit {
                    end: b[i..]
                        .iter()
                        .position(|&x| x == b'\n')
                        .map_or(b.len(), |p| i + p),
                    kind: SpanKind::Comment,
                    quotes: None,
                }),
                (b'/', Some(b'*')) => Some(block_comment(b, i)),
                (b'"', _) => cooked_string(b, i),
                (b'\'', _) => char_literal(b, i),
                _ => None,
            }
        };
        let Some(lit) = found else {
            i += 1;
            continue;
        };
        let end = lit.end.min(b.len());
        out.push(Span {
            start: i,
            end,
            kind: lit.kind,
            quotes: lit.quotes,
        });
        i = end.max(i + 1);
    }
    out
}

struct Lit {
    end: usize,
    kind: SpanKind,
    quotes: Option<(usize, usize)>,
}

const fn cooked_string(b: &[u8], q: usize) -> Option<Lit> {
    let mut j = q + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => {
                return Some(Lit {
                    end: j + 1,
                    kind: SpanKind::Str,
                    quotes: Some((q, j)),
                });
            }
            _ => j += 1,
        }
    }
    Some(Lit {
        end: b.len(),
        kind: SpanKind::Str,
        quotes: None,
    })
}

fn raw_string(b: &[u8], at: usize) -> Option<Lit> {
    let hashes = b[at..].iter().take_while(|&&c| c == b'#').count();
    let q = at + hashes;
    if b.get(q) != Some(&b'"') {
        return None;
    }
    let close = (q + 1..b.len())
        .find(|&j| b[j] == b'"' && (1..=hashes).all(|k| b.get(j + k) == Some(&b'#')));
    Some(match close {
        Some(close) => Lit {
            end: close + 1 + hashes,
            kind: SpanKind::Str,
            quotes: Some((q, close)),
        },
        None => Lit {
            end: b.len(),
            kind: SpanKind::Str,
            quotes: None,
        },
    })
}

const fn utf8_len(lead: u8) -> usize {
    match lead {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

fn char_literal(b: &[u8], i: usize) -> Option<Lit> {
    let next = *b.get(i + 1)?;
    if next == b'\\' {
        let close = (i + 3..b.len().min(i + 12))
            .find(|&j| b[j] == b'\'' || b[j] == b'\n')
            .filter(|&j| b[j] == b'\'')?;
        return Some(Lit {
            end: close + 1,
            kind: SpanKind::Char,
            quotes: None,
        });
    }
    let after = i + 1 + utf8_len(next);
    (next != b'\n' && b.get(after) == Some(&b'\'')).then_some(Lit {
        end: after + 1,
        kind: SpanKind::Char,
        quotes: None,
    })
}

fn block_comment(b: &[u8], i: usize) -> Lit {
    let mut depth = 0usize;
    let mut j = i;
    while j < b.len() {
        match (b[j], b.get(j + 1)) {
            (b'/', Some(b'*')) => {
                depth += 1;
                j += 2;
            }
            (b'*', Some(b'/')) => {
                depth -= 1;
                j += 2;
                if depth == 0 {
                    break;
                }
            }
            _ => j += 1,
        }
    }
    Lit {
        end: j,
        kind: SpanKind::Comment,
        quotes: None,
    }
}

/// Code and text views of one file.
pub(crate) struct Lexed {
    /// Comments and literal contents blanked.
    pub(crate) code: String,
    /// Comments blanked, literals intact.
    pub(crate) text: String,
}

/// Lex `src` into its code and text views.
pub(crate) fn lex(src: &str) -> Lexed {
    let mut code = src.as_bytes().to_vec();
    let mut text = code.clone();
    for span in spans(src) {
        blank(&mut code[span.start..span.end]);
        if span.kind == SpanKind::Comment {
            blank(&mut text[span.start..span.end]);
        }
        if let Some((open, close)) = span.quotes
            && !src.as_bytes()[span.start..span.end].contains(&b'\n')
        {
            code[open] = b'"';
            code[close] = b'"';
        }
    }
    Lexed {
        code: utf8(code),
        text: utf8(text),
    }
}

/// Code view with trailing whitespace trimmed, run before any unshipped-item
/// mask so a `}` inside a raw string cannot end a test module early.
pub(crate) fn blank_non_code(src: &str) -> String {
    let code = lex(src).code;
    let mut out = String::with_capacity(code.len());
    for line in code.lines() {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn blank(bytes: &mut [u8]) {
    for b in bytes {
        if *b != b'\n' {
            *b = b' ';
        }
    }
}

fn utf8(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_view_blanks_every_literal_and_comment_in_place() {
        let src = "let a = \"x // y\"; // z\nlet b = r#\"multi\nline\"#; /* c\nd */ e\n\
                   'x' '\\'' 'a b\n#[cfg(all(test, feature = \"x\"))] // why\nlet c = br\"}\";";
        let lexed = blank_non_code(src);
        assert_eq!(
            lexed.lines().collect::<Vec<_>>(),
            [
                "let a = \"      \";",
                "let b =",
                "      ;",
                "     e",
                "         'a b",
                "#[cfg(all(test, feature = \" \"))]",
                "let c =   \" \";",
            ]
        );
        assert_eq!(lexed.lines().count(), src.lines().count());
        assert_eq!(blank_non_code("no newline").lines().count(), 1);
        assert_eq!(
            blank_non_code("a\n\n").lines().count(),
            "a\n\n".lines().count()
        );
    }

    #[test]
    fn the_text_view_keeps_literals_and_drops_comments_byte_aligned() {
        let src = "f(\"/dev/x\"); // see \"/dev/y\"\n/* é */ g('é', b'\\n', c\"Ab\")\n";
        let lexed = lex(src);
        assert_eq!(lexed.code.len(), src.len());
        assert_eq!(lexed.text.len(), src.len());
        let text: Vec<&str> = lexed.text.lines().collect();
        assert_eq!(text[0].trim_end(), "f(\"/dev/x\");");
        assert_eq!(text[1].trim_start(), "g('é', b'\\n', c\"Ab\")");
        let code: Vec<&str> = lexed.code.lines().collect();
        assert_eq!(code[0].trim_end(), "f(\"      \");");
        let at = code[1]
            .find('"')
            .expect("a one-line string keeps its quotes");
        assert_eq!(&text[1][at - 1..at + 4], "c\"Ab\"");
    }

    #[test]
    fn lifetimes_labels_and_raw_identifiers_are_code() {
        let src = "fn f<'a>(x: &'a str) { 'outer: loop { r#type(x); } }";
        assert_eq!(lex(src).code, src);
        assert!(spans(src).is_empty());
    }
}
