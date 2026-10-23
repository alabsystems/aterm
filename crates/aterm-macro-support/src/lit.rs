// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! String literals, and the paths serde attributes spell as strings.

use proc_macro::{Literal, Spacing, Span, TokenStream, TokenTree};

use crate::quote::respan;

/// A string literal's resolved value and optional suffix (`"x"suffix`).
#[derive(Debug, PartialEq, Eq)]
pub struct StrLit {
    /// The value, escapes resolved.
    pub value: String,
    /// The suffix, as written.
    pub suffix: String,
}

/// Reads a literal token as a string literal, or `None` for any other literal kind.
#[inline]
#[must_use]
pub fn str_lit(lit: &Literal) -> Option<StrLit> {
    parse_str_source(&lit.to_string())
}

/// Constructs a string literal token with `value` at `span`.
#[inline]
#[must_use]
pub fn str_literal(value: &str, span: Span) -> Literal {
    let mut lit = Literal::string(value);
    lit.set_span(span);
    lit
}

/// Parses the source text of a cooked (`"..."`) or raw (`r"..."` / `r#"..."#`) string literal.
#[must_use]
pub fn parse_str_source(src: &str) -> Option<StrLit> {
    if let Some(rest) = src.strip_prefix('"') {
        parse_cooked(rest)
    } else if let Some(rest) = src.strip_prefix('r') {
        parse_raw(rest)
    } else {
        None
    }
}

fn parse_cooked(rest: &str) -> Option<StrLit> {
    let mut value = String::new();
    let mut chars = rest.char_indices();
    while let Some((i, ch)) = chars.next() {
        match ch {
            '"' => {
                let suffix = rest.get(i + 1..)?;
                return valid_suffix(suffix).then(|| StrLit {
                    value,
                    suffix: suffix.to_owned(),
                });
            }
            '\\' => {
                let (_, esc) = chars.next()?;
                match esc {
                    'n' => value.push('\n'),
                    'r' => value.push('\r'),
                    't' => value.push('\t'),
                    '\\' => value.push('\\'),
                    '0' => value.push('\0'),
                    '\'' => value.push('\''),
                    '"' => value.push('"'),
                    'x' => {
                        let hi = chars.next()?.1.to_digit(16)?;
                        let lo = chars.next()?.1.to_digit(16)?;
                        let byte = hi * 16 + lo;
                        if byte > 0x7f {
                            return None;
                        }
                        value.push(char::from_u32(byte)?);
                    }
                    'u' => {
                        if chars.next()?.1 != '{' {
                            return None;
                        }
                        let mut code: u32 = 0;
                        let mut digits = 0u32;
                        loop {
                            let (_, d) = chars.next()?;
                            if d == '}' {
                                break;
                            }
                            if d == '_' {
                                continue;
                            }
                            digits += 1;
                            if digits > 6 {
                                return None;
                            }
                            code = code * 16 + d.to_digit(16)?;
                        }
                        if digits == 0 {
                            return None;
                        }
                        value.push(char::from_u32(code)?);
                    }
                    '\n' | '\r' => {
                        while let Some((_, next)) = chars.clone().next() {
                            if matches!(next, ' ' | '\t' | '\n' | '\r') {
                                chars.next();
                            } else {
                                break;
                            }
                        }
                    }
                    _ => return None,
                }
            }
            other => value.push(other),
        }
    }
    None
}

fn parse_raw(rest: &str) -> Option<StrLit> {
    let hashes = rest.bytes().take_while(|&b| b == b'#').count();
    let body = rest.get(hashes..)?.strip_prefix('"')?;
    let close: String = std::iter::once('"')
        .chain(std::iter::repeat_n('#', hashes))
        .collect();
    let at = body.find(&close)?;
    let suffix = body.get(at + close.len()..)?;
    let value = body.get(..at)?;
    valid_suffix(suffix).then(|| StrLit {
        value: value.to_owned(),
        suffix: suffix.to_owned(),
    })
}

/// An integer literal's signed base-10 digits and optional suffix (`0x1F_u8` -> `31` and `u8`).
#[derive(Debug, PartialEq, Eq)]
pub struct IntLit {
    /// Base-10 digits with optional leading `-`, no leading zeros or `_`.
    pub digits: String,
    /// The suffix, as written.
    pub suffix: String,
}

impl IntLit {
    /// Parses the base-10 digits as an `i64`, ignoring the suffix.
    ///
    /// # Errors
    /// The value does not fit; the message is std's ("number too large to
    /// fit in target type").
    #[inline]
    pub fn to_i64(&self) -> Result<i64, String> {
        self.digits.parse::<i64>().map_err(|e| e.to_string())
    }
}

/// Reads a literal token as an integer literal, or `None` for any other literal kind.
#[inline]
#[must_use]
pub fn int_lit(lit: &Literal) -> Option<IntLit> {
    read_int_source(&lit.to_string())
}

/// Parses the source text of an optionally signed decimal, hex, octal, or binary integer literal.
#[inline]
#[must_use]
pub fn parse_int_source(src: &str) -> Option<IntLit> {
    read_int_source(src).filter(|int| valid_suffix(&int.suffix))
}

fn read_int_source(src: &str) -> Option<IntLit> {
    let (negative, unsigned) = match src.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, src),
    };
    let (base, mut s) = match unsigned.as_bytes() {
        [b'0', b'x', ..] => (16u8, unsigned.get(2..)?),
        [b'0', b'o', ..] => (8, unsigned.get(2..)?),
        [b'0', b'b', ..] => (2, unsigned.get(2..)?),
        [b'0'..=b'9', ..] => (10, unsigned),
        _ => return None,
    };
    let mut value: Vec<u8> = Vec::new();
    let mut has_digit = false;
    while let Some(&b) = s.as_bytes().first() {
        let digit = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' if base > 10 => b - b'a' + 10,
            b'A'..=b'F' if base > 10 => b - b'A' + 10,
            b'_' => {
                s = s.get(1..)?;
                continue;
            }
            b'.' if base == 10 => return None,
            b'e' | b'E' if base == 10 => {
                if exponent_follows(s.get(1..)?) {
                    return None;
                }
                break;
            }
            _ => break,
        };
        if digit >= base {
            return None;
        }
        has_digit = true;
        mul_add(&mut value, base, digit);
        s = s.get(1..)?;
    }
    if !has_digit {
        return None;
    }
    let mut digits: String = value.iter().rev().map(|d| char::from(b'0' + d)).collect();
    if digits.is_empty() {
        digits.push('0');
    }
    if negative {
        digits.insert(0, '-');
    }
    Some(IntLit {
        digits,
        suffix: s.to_owned(),
    })
}

fn exponent_follows(rest: &str) -> bool {
    for b in rest.bytes() {
        match b {
            b'_' => {}
            b'-' | b'+' | b'0'..=b'9' => return true,
            _ => return false,
        }
    }
    false
}

fn mul_add(value: &mut Vec<u8>, base: u8, digit: u8) {
    let mut carry = u16::from(digit);
    for d in value.iter_mut() {
        let next = u16::from(*d) * u16::from(base) + carry;
        *d = (next % 10) as u8;
        carry = next / 10;
    }
    while carry > 0 {
        value.push((carry % 10) as u8);
        carry /= 10;
    }
    while value.last() == Some(&0) {
        value.pop();
    }
}

fn valid_suffix(suffix: &str) -> bool {
    let mut chars = suffix.chars();
    match chars.next() {
        None => true,
        Some(first) => {
            (first == '_' || first.is_alphabetic())
                && chars.all(|c| c == '_' || c.is_alphanumeric())
        }
    }
}

/// Parses `src` as an expression path (`f`, `a::b`, `Vec::<u8>::new`, `<T as Trait>::f`) spanned at `span`.
#[must_use]
pub fn parse_expr_path(src: &str, span: Span) -> Option<TokenStream> {
    let stream: TokenStream = src.parse().ok()?;
    let tokens: Vec<TokenTree> = stream.into_iter().collect();
    if !is_expr_path(&tokens) {
        return None;
    }
    Some(respan(tokens.into_iter().collect(), span))
}

fn is_expr_path(tokens: &[TokenTree]) -> bool {
    let mut i = 0;
    if is_punct_at(tokens, 0, '<') {
        let Some(close) = matching_angle(tokens, 0) else {
            return false;
        };
        if close == 1 || !is_colon2_at(tokens, close + 1) {
            return false;
        }
        i = close + 3;
    } else if is_colon2_at(tokens, 0) {
        i = 2;
    }
    loop {
        if !matches!(tokens.get(i), Some(TokenTree::Ident(_))) {
            return false;
        }
        i += 1;
        if i == tokens.len() {
            return true;
        }
        if !is_colon2_at(tokens, i) {
            return false;
        }
        i += 2;
        if is_punct_at(tokens, i, '<') {
            let Some(close) = matching_angle(tokens, i) else {
                return false;
            };
            i = close + 1;
            if i == tokens.len() {
                return true;
            }
            if !is_colon2_at(tokens, i) {
                return false;
            }
            i += 2;
        }
    }
}

#[inline]
fn is_punct_at(tokens: &[TokenTree], i: usize, ch: char) -> bool {
    matches!(tokens.get(i), Some(TokenTree::Punct(p)) if p.as_char() == ch)
}

#[inline]
fn is_colon2_at(tokens: &[TokenTree], i: usize) -> bool {
    matches!(tokens.get(i), Some(TokenTree::Punct(p)) if p.as_char() == ':' && p.spacing() == Spacing::Joint)
        && is_punct_at(tokens, i + 1, ':')
}

fn matching_angle(tokens: &[TokenTree], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut after_joint_minus = false;
    for (i, tt) in tokens.iter().enumerate().skip(open) {
        let mut joint_minus = false;
        if let TokenTree::Punct(p) = tt {
            match p.as_char() {
                '<' => depth += 1,
                '>' if !after_joint_minus => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                '-' => joint_minus = p.spacing() == Spacing::Joint,
                _ => {}
            }
        }
        after_joint_minus = joint_minus;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{IntLit, StrLit, parse_int_source, parse_str_source};

    fn value(src: &str) -> Option<String> {
        parse_str_source(src).map(|lit| lit.value)
    }

    fn int(src: &str) -> Option<(String, String)> {
        parse_int_source(src).map(|IntLit { digits, suffix }| (digits, suffix))
    }

    fn some(digits: &str, suffix: &str) -> Option<(String, String)> {
        Some((digits.to_owned(), suffix.to_owned()))
    }

    #[test]
    fn integers_in_every_base_read_as_base_ten() {
        assert_eq!(int("0"), some("0", ""));
        assert_eq!(int("007"), some("7", ""));
        assert_eq!(int("1_000_000"), some("1000000", ""));
        assert_eq!(int("0x1F"), some("31", ""));
        assert_eq!(int("0xdead_BEEF"), some("3735928559", ""));
        assert_eq!(int("0o777"), some("511", ""));
        assert_eq!(int("0b1010_0101"), some("165", ""));
        assert_eq!(int("-42"), some("-42", ""));
        assert_eq!(int("-0"), some("-0", ""));
        assert_eq!(
            int("0xffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff_ff"),
            some("87112285931760246646623899502532662132735", "")
        );
    }

    #[test]
    fn integer_suffixes_are_kept() {
        assert_eq!(int("7u8"), some("7", "u8"));
        assert_eq!(int("0x10_i64"), some("16", "i64"));
        assert_eq!(int("1f32"), some("1", "f32"));
        assert_eq!(int("1em"), some("1", "em"));
        assert_eq!(int("0x1e3"), some("483", ""));
    }

    #[test]
    fn what_is_not_an_integer_is_refused() {
        assert_eq!(int("1.0"), None);
        assert_eq!(int("1e3"), None);
        assert_eq!(int("1e-3"), None);
        assert_eq!(int("1E+3"), None);
        assert_eq!(int("2e1_0f64"), None);
        assert_eq!(int("0b102"), None);
        assert_eq!(int("0o8"), None);
        assert_eq!(int("0x"), None);
        assert_eq!(int("0x_"), None);
        assert_eq!(int("x1"), None);
        assert_eq!(int("\"1\""), None);
        assert_eq!(int(""), None);
    }

    #[test]
    fn to_i64_says_what_does_not_fit() {
        let read = |src: &str| parse_int_source(src).map(|lit| lit.to_i64());
        assert_eq!(read("9223372036854775807"), Some(Ok(i64::MAX)));
        assert_eq!(read("-9223372036854775808"), Some(Ok(i64::MIN)));
        assert_eq!(read("0xffu8"), Some(Ok(255)));
        assert_eq!(
            read("9223372036854775808"),
            Some(Err("number too large to fit in target type".to_owned()))
        );
        assert_eq!(
            read("-9223372036854775809"),
            Some(Err("number too small to fit in target type".to_owned()))
        );
    }

    #[test]
    fn cooked_escapes() {
        assert_eq!(value(r#""plain""#).as_deref(), Some("plain"));
        assert_eq!(
            value(r#""a\nb\tc\\d\0e\'f\"g""#).as_deref(),
            Some("a\nb\tc\\d\0e'f\"g")
        );
        assert_eq!(value(r#""\x41\x7f""#).as_deref(), Some("A\x7f"));
        assert_eq!(
            value(r#""\u{1F600}\u{1_f6_00}\u{e9}""#).as_deref(),
            Some("😀😀é")
        );
        assert_eq!(value("\"a\\\n    b\"").as_deref(), Some("ab"));
        assert_eq!(value("\"ünï\"").as_deref(), Some("ünï"));
    }

    #[test]
    fn cooked_rejects_what_the_lexer_would() {
        assert_eq!(value(r#""\x80""#), None);
        assert_eq!(value(r#""\u{D800}""#), None);
        assert_eq!(value(r#""\u{}""#), None);
        assert_eq!(value(r#""\u{1234567}""#), None);
        assert_eq!(value(r#""\q""#), None);
        assert_eq!(value(r#""unterminated"#), None);
    }

    #[test]
    fn raw_strings() {
        assert_eq!(value(r##"r"a\nb""##).as_deref(), Some("a\\nb"));
        assert_eq!(value(r###"r#"say "hi""#"###).as_deref(), Some("say \"hi\""));
        assert_eq!(value(r###"r##"a"#b"##"###).as_deref(), Some("a\"#b"));
        assert_eq!(value(r##"r#"open""##), None);
    }

    #[test]
    fn suffixes_are_kept_and_other_literals_refused() {
        assert_eq!(
            parse_str_source(r#""x"suf"#),
            Some(StrLit {
                value: "x".to_owned(),
                suffix: "suf".to_owned()
            })
        );
        assert_eq!(value(r#"b"bytes""#), None);
        assert_eq!(value(r#"c"cstr""#), None);
        assert_eq!(value("'c'"), None);
        assert_eq!(value("42"), None);
    }
}
