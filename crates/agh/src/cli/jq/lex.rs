// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The jq lexer: the tokens of design §8.4, the refused tokens (`..`, `?//` and every
//! assignment) refused here by their `REFUSED_JQ` row, and every `$NAME` a `REFUSED_JQ` row
//! names (`$ENV`) and every `@NAME` that is not a format refused by name.

use aterm_json::Value;

use super::{FORMATS, MAX_DEPTH, refused, refused_name, syntax, too_deep};
use crate::error::Result;

/// A comparison operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CmpOp {
    /// `==`.
    Eq,
    /// `!=`.
    Ne,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
    /// `>`.
    Gt,
    /// `>=`.
    Ge,
}

/// One token.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Tok {
    /// `.` standing alone.
    Dot,
    /// `.name`, written with no space after the dot.
    Field(String),
    /// `[`.
    LBrack,
    /// `]`.
    RBrack,
    /// `{`.
    LBrace,
    /// `}`.
    RBrace,
    /// `(`.
    LParen,
    /// `)`.
    RParen,
    /// `|`.
    Pipe,
    /// `,`.
    Comma,
    /// `:`.
    Colon,
    /// `;`.
    Semi,
    /// `?`.
    Question,
    /// `//`.
    Alt,
    /// A comparison.
    Cmp(CmpOp),
    /// `+`.
    Plus,
    /// `-`.
    Minus,
    /// `$NAME`.
    Var(String),
    /// `@NAME`, one of `FORMATS`.
    Format(String),
    /// A bare name: a function, a keyword, or an object key.
    Name(String),
    /// A number literal.
    Num(Value),
    /// A string literal, with its interpolations.
    Str(Vec<Piece>),
}

/// A piece of a string literal.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Piece {
    /// Literal text.
    Lit(String),
    /// `\(pipeline)`: its tokens.
    Interp(Vec<Spanned>),
}

/// A token and the byte it starts at.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Spanned {
    /// The token.
    pub(super) tok: Tok,
    /// Its first byte's offset in the program.
    pub(super) at: usize,
}

/// Every token of `src`.
pub(super) fn lex(src: &str) -> Result<Vec<Spanned>> {
    let mut lexer = Lexer {
        src: src.as_bytes(),
        text: src,
        nest: 0,
        pos: 0,
    };
    lexer.tokens(false)
}

/// The lexer's state.
struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    /// How many interpolations deep the lexer is.
    nest: usize,
}

/// Whether `b` may start a name.
fn name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// Whether `b` may continue a name.
fn name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether an operand is expected after `prev`, so that `-` followed by a digit is a number.
fn operand_expected(prev: Option<&Tok>) -> bool {
    match prev {
        None => true,
        Some(tok) => {
            matches!(
                tok,
                Tok::LBrack
                    | Tok::LBrace
                    | Tok::LParen
                    | Tok::Pipe
                    | Tok::Comma
                    | Tok::Colon
                    | Tok::Semi
                    | Tok::Alt
                    | Tok::Cmp(_)
                    | Tok::Plus
                    | Tok::Minus
            ) || matches!(tok, Tok::Name(n) if n == "and" || n == "or")
        }
    }
}

impl Lexer<'_> {
    fn peek(&self, ahead: usize) -> Option<u8> {
        self.src.get(self.pos + ahead).copied()
    }

    /// Tokens up to the end, or (when `nested`) up to and including the `)` that closes an
    /// interpolation.
    fn tokens(&mut self, nested: bool) -> Result<Vec<Spanned>> {
        let mut out: Vec<Spanned> = Vec::new();
        let mut depth = 0usize;
        loop {
            while self.peek(0).is_some_and(|b| b.is_ascii_whitespace()) {
                self.pos += 1;
            }
            let at = self.pos;
            let Some(b) = self.peek(0) else {
                if nested {
                    return Err(syntax("an unterminated \\( in a string", at));
                }
                return Ok(out);
            };
            let prev = out.last().map(|s| &s.tok);
            let tok = match b {
                b'.' => self.dot(at)?,
                b'[' => self.one(Tok::LBrack),
                b']' => self.one(Tok::RBrack),
                b'{' => self.one(Tok::LBrace),
                b'}' => self.one(Tok::RBrace),
                b'(' => {
                    depth += 1;
                    self.one(Tok::LParen)
                }
                b')' => {
                    if nested && depth == 0 {
                        self.pos += 1;
                        return Ok(out);
                    }
                    depth = depth.saturating_sub(1);
                    self.one(Tok::RParen)
                }
                b',' => self.one(Tok::Comma),
                b':' => self.one(Tok::Colon),
                b';' => self.one(Tok::Semi),
                b'|' => {
                    if self.peek(1) == Some(b'=') {
                        return Err(refused("|="));
                    }
                    self.one(Tok::Pipe)
                }
                b'?' => {
                    if self.peek(1) == Some(b'/') && self.peek(2) == Some(b'/') {
                        return Err(refused("?//"));
                    }
                    self.one(Tok::Question)
                }
                b'/' => {
                    if self.peek(1) == Some(b'/') {
                        if self.peek(2) == Some(b'=') {
                            return Err(refused("//="));
                        }
                        self.pos += 2;
                        Tok::Alt
                    } else if self.peek(1) == Some(b'=') {
                        return Err(refused("/="));
                    } else {
                        return Err(unexpected(b, at));
                    }
                }
                b'=' => {
                    if self.peek(1) == Some(b'=') {
                        self.pos += 2;
                        Tok::Cmp(CmpOp::Eq)
                    } else {
                        return Err(refused("="));
                    }
                }
                b'!' => {
                    if self.peek(1) == Some(b'=') {
                        self.pos += 2;
                        Tok::Cmp(CmpOp::Ne)
                    } else {
                        return Err(unexpected(b, at));
                    }
                }
                b'<' | b'>' => {
                    let eq = self.peek(1) == Some(b'=');
                    self.pos += if eq { 2 } else { 1 };
                    Tok::Cmp(match (b, eq) {
                        (b'<', false) => CmpOp::Lt,
                        (b'<', true) => CmpOp::Le,
                        (_, false) => CmpOp::Gt,
                        (_, true) => CmpOp::Ge,
                    })
                }
                b'+' | b'*' | b'%' if self.peek(1) == Some(b'=') => {
                    return Err(refused(&format!("{}=", char::from(b))));
                }
                b'+' => self.one(Tok::Plus),
                b'-' => {
                    if self.peek(1) == Some(b'=') {
                        return Err(refused("-="));
                    }
                    if operand_expected(prev) && self.peek(1).is_some_and(|d| d.is_ascii_digit()) {
                        self.pos += 1;
                        self.number(true)
                    } else {
                        self.one(Tok::Minus)
                    }
                }
                b'$' => {
                    self.pos += 1;
                    let name = self.name(at)?;
                    if let Some(e) = refused_name(&format!("${name}")) {
                        return Err(e);
                    }
                    Tok::Var(name)
                }
                b'@' => {
                    self.pos += 1;
                    let name = self.name(at)?;
                    let spelled = format!("@{name}");
                    if !FORMATS.contains(&name.as_str()) {
                        return Err(refused(&spelled));
                    }
                    Tok::Format(name)
                }
                b'"' => self.string(at)?,
                b'0'..=b'9' => self.number(false),
                b if name_start(b) => Tok::Name(self.name(at)?),
                _ => return Err(unexpected_char(self.text, at)),
            };
            out.push(Spanned { tok, at });
        }
    }

    fn one(&mut self, tok: Tok) -> Tok {
        self.pos += 1;
        tok
    }

    /// `.`, `.name` or the refused `..`.
    fn dot(&mut self, at: usize) -> Result<Tok> {
        match self.peek(1) {
            Some(b'.') => Err(refused("..")),
            Some(b) if name_start(b) => {
                self.pos += 1;
                Ok(Tok::Field(self.name(at)?))
            }
            _ => Ok(self.one(Tok::Dot)),
        }
    }

    /// A name at `pos`.
    fn name(&mut self, at: usize) -> Result<String> {
        let start = self.pos;
        if !self.peek(0).is_some_and(name_start) {
            return Err(syntax("a name expected", at));
        }
        while self.peek(0).is_some_and(name_byte) {
            self.pos += 1;
        }
        Ok(self.text[start..self.pos].to_string())
    }

    /// Digits with an optional fraction and an optional exponent (`[eE][+-]?digits`), the `-`
    /// (when `negative`) already read. A number with an exponent is a double, written as a
    /// whole number when it is one within 2^53, as jq's arithmetic writes it.
    fn number(&mut self, negative: bool) -> Tok {
        let start = self.pos;
        while self.peek(0).is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        let mut fraction = false;
        if self.peek(0) == Some(b'.') && self.peek(1).is_some_and(|b| b.is_ascii_digit()) {
            fraction = true;
            self.pos += 1;
            while self.peek(0).is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let sign = usize::from(matches!(self.peek(1), Some(b'+' | b'-')));
        if matches!(self.peek(0), Some(b'e' | b'E'))
            && self.peek(1 + sign).is_some_and(|b| b.is_ascii_digit())
        {
            self.pos += 1 + sign;
            while self.peek(0).is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
            let v: f64 = self.text[start..self.pos].parse().unwrap_or(0.0);
            return Tok::Num(super::eval::from_double(if negative { -v } else { v }));
        }
        let digits = &self.text[start..self.pos];
        let value = if fraction {
            let v: f64 = digits.parse().unwrap_or(0.0);
            Value::from(if negative { -v } else { v })
        } else if let Ok(v) = digits.parse::<u64>() {
            if negative {
                i64::try_from(v).map_or_else(|_| Value::from(-(v as f64)), |v| Value::from(-v))
            } else {
                Value::from(v)
            }
        } else {
            let v: f64 = digits.parse().unwrap_or(0.0);
            Value::from(if negative { -v } else { v })
        };
        Tok::Num(value)
    }

    /// A string literal at `pos` (the opening quote).
    fn string(&mut self, at: usize) -> Result<Tok> {
        self.pos += 1;
        let mut pieces = Vec::new();
        let mut lit = String::new();
        loop {
            let Some(b) = self.peek(0) else {
                return Err(syntax("an unterminated string", at));
            };
            match b {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => {
                    let esc_at = self.pos;
                    let Some(e) = self.peek(1) else {
                        return Err(syntax("an unterminated string", at));
                    };
                    self.pos += 2;
                    match e {
                        b'"' => lit.push('"'),
                        b'\\' => lit.push('\\'),
                        b'/' => lit.push('/'),
                        b'b' => lit.push('\u{8}'),
                        b'f' => lit.push('\u{c}'),
                        b'n' => lit.push('\n'),
                        b'r' => lit.push('\r'),
                        b't' => lit.push('\t'),
                        b'u' => lit.push(self.unicode(esc_at)?),
                        b'(' => {
                            if !lit.is_empty() {
                                pieces.push(Piece::Lit(std::mem::take(&mut lit)));
                            }
                            if self.nest >= MAX_DEPTH {
                                return Err(too_deep(esc_at));
                            }
                            self.nest += 1;
                            let inner = self.tokens(true)?;
                            self.nest -= 1;
                            pieces.push(Piece::Interp(inner));
                        }
                        _ => return Err(syntax("an unknown escape", esc_at)),
                    }
                }
                _ => {
                    // One whole UTF-8 character: `pos` is always on a character boundary.
                    let c = self.text[self.pos..].chars().next().unwrap_or('\u{fffd}');
                    lit.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
        if !lit.is_empty() || pieces.is_empty() {
            pieces.push(Piece::Lit(lit));
        }
        Ok(Tok::Str(pieces))
    }

    /// The four hex digits after `\u` (and a low surrogate's `\uXXXX` after a high one).
    fn unicode(&mut self, at: usize) -> Result<char> {
        let high = self.hex4(at)?;
        if (0xD800..0xDC00).contains(&high) {
            if self.peek(0) == Some(b'\\') && self.peek(1) == Some(b'u') {
                self.pos += 2;
                let low = self.hex4(at)?;
                if (0xDC00..0xE000).contains(&low) {
                    let code = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
                    return char::from_u32(code).ok_or_else(|| syntax("a bad \\u escape", at));
                }
            }
            return Err(syntax("a lone surrogate in a \\u escape", at));
        }
        char::from_u32(high).ok_or_else(|| syntax("a lone surrogate in a \\u escape", at))
    }

    fn hex4(&mut self, at: usize) -> Result<u32> {
        let digits = self
            .text
            .get(self.pos..self.pos + 4)
            .ok_or_else(|| syntax("a short \\u escape", at))?;
        let value = u32::from_str_radix(digits, 16).map_err(|_| syntax("a bad \\u escape", at))?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(syntax("a bad \\u escape", at));
        }
        self.pos += 4;
        Ok(value)
    }
}

/// `jq: unexpected {byte} at byte {offset}`, for an ASCII byte.
fn unexpected(b: u8, at: usize) -> crate::error::Error {
    syntax(&format!("unexpected {}", char::from(b)), at)
}

/// `jq: unexpected {char} at byte {offset}`, for the character at `at`.
fn unexpected_char(text: &str, at: usize) -> crate::error::Error {
    let c = text[at..].chars().next().unwrap_or('\u{fffd}');
    syntax(&format!("unexpected {c}"), at)
}
