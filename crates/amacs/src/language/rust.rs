// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rust: the line lexer, and indentation by bracket depth.

use super::{Cursor, IndentStyle, LexState, Mode, Spans, SyntaxClass, leading, line_space};

/// The strict and reserved keywords that read as keywords in practice
/// (`union` is left out: it is mostly a method name).
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];

pub(super) fn lex(state: LexState, line: &str, out: &mut Spans) -> LexState {
    let mut cur = Cursor::new(line);
    let mut brackets = Brackets::new(state);
    let mut mode = match state.mode {
        Mode::BlockComment(depth) => block_comment(&mut cur, 0, depth, out),
        Mode::Quoted => quoted(&mut cur, 0, out),
        Mode::RawQuoted(hashes) => raw_quoted(&mut cur, 0, hashes, out),
        _ => Mode::Code,
    };
    while mode == Mode::Code {
        let Some(byte) = cur.peek() else {
            break;
        };
        let start = cur.pos();
        match byte {
            b'/' if cur.nth(1) == Some(b'/') => {
                cur.finish();
                out.push(start, cur.pos(), SyntaxClass::Comment);
            }
            b'/' if cur.nth(1) == Some(b'*') => {
                cur.bump_n(2);
                mode = block_comment(&mut cur, start, 1, out);
            }
            b'"' => {
                cur.bump();
                mode = quoted(&mut cur, start, out);
            }
            b'\'' => quote(&mut cur, out),
            b'0'..=b'9' => number(&mut cur, out),
            b'#' if cur.nth(1) == Some(b'[')
                || (cur.nth(1) == Some(b'!') && cur.nth(2) == Some(b'[')) =>
            {
                cur.bump();
                cur.eat(b'!');
                cur.bump();
                brackets.open();
                cur.eat_while(ident_byte);
                out.push(start, cur.pos(), SyntaxClass::Meta);
            }
            b'(' | b'[' | b'{' => {
                cur.bump();
                brackets.open();
                out.push(start, cur.pos(), SyntaxClass::Punct);
            }
            b')' | b']' | b'}' => {
                cur.bump();
                brackets.close();
                out.push(start, cur.pos(), SyntaxClass::Punct);
            }
            _ if ident_byte(byte) => mode = word(&mut cur, out),
            _ if byte.is_ascii_punctuation() => {
                cur.bump();
                out.push(start, cur.pos(), SyntaxClass::Punct);
            }
            _ => cur.bump(),
        }
    }
    let (depth, levels) = brackets.finish();
    LexState {
        mode,
        depth,
        levels,
        ..state
    }
}

/// The rest of a `/* */` comment nested `depth` deep, from `start`.
fn block_comment(cur: &mut Cursor<'_>, start: usize, depth: u8, out: &mut Spans) -> Mode {
    let mut depth = depth;
    while !cur.done() {
        if cur.looking_at(b"*/") {
            cur.bump_n(2);
            depth = depth.saturating_sub(1);
            if depth == 0 {
                out.push(start, cur.pos(), SyntaxClass::Comment);
                return Mode::Code;
            }
        } else if cur.looking_at(b"/*") {
            cur.bump_n(2);
            depth = depth.saturating_add(1);
        } else {
            cur.bump();
        }
    }
    out.push(start, cur.pos(), SyntaxClass::Comment);
    Mode::BlockComment(depth.max(1))
}

/// The rest of an escaped string from `start` (its opening quote read).
fn quoted(cur: &mut Cursor<'_>, start: usize, out: &mut Spans) -> Mode {
    while let Some(byte) = cur.peek() {
        cur.bump();
        match byte {
            b'\\' => cur.bump(),
            b'"' => {
                out.push(start, cur.pos(), SyntaxClass::String);
                return Mode::Code;
            }
            _ => {}
        }
    }
    out.push(start, cur.pos(), SyntaxClass::String);
    Mode::Quoted
}

/// The rest of a raw string closing on `"` and `hashes` `#`, from `start`.
fn raw_quoted(cur: &mut Cursor<'_>, start: usize, hashes: u8, out: &mut Spans) -> Mode {
    while let Some(byte) = cur.peek() {
        cur.bump();
        if byte == b'"' {
            let closing = cur.eat_while(|next| next == b'#');
            if closing >= usize::from(hashes) {
                out.push(start, cur.pos(), SyntaxClass::String);
                return Mode::Code;
            }
        }
    }
    out.push(start, cur.pos(), SyntaxClass::String);
    Mode::RawQuoted(hashes)
}

/// A `'`: a char literal (`'a'`, `'\n'`, `'\u{1F980}'`, `'🦀'`), a lifetime
/// or label (`'a`, `'static`), or a stray quote.
fn quote(cur: &mut Cursor<'_>, out: &mut Spans) {
    let start = cur.pos();
    if cur.nth(1) == Some(b'\\') {
        cur.bump_n(3);
        let mut budget = 10usize;
        while budget > 0 && cur.peek().is_some_and(|byte| byte != b'\'') {
            cur.bump();
            budget -= 1;
        }
        cur.skip_to_boundary();
        cur.eat(b'\'');
        out.push(start, cur.pos(), SyntaxClass::String);
        return;
    }
    let width = usize::from(cur.nth(1).map_or(0, utf8_width));
    if width > 0 && cur.nth(width.saturating_add(1)) == Some(b'\'') {
        cur.bump_n(width.saturating_add(2));
        out.push(start, cur.pos(), SyntaxClass::String);
        return;
    }
    cur.bump();
    if cur.eat_while(ident_byte) > 0 {
        out.push(start, cur.pos(), SyntaxClass::Meta);
    } else {
        out.push(start, cur.pos(), SyntaxClass::Punct);
    }
}

trust_contract_fn! {
    /// How many bytes the UTF-8 character led by `byte` has, read off its
    /// high bits (RFC 3629 §3): 1 for `0xxxxxxx`, 2 for `110xxxxx`, 3 for
    /// `1110xxxx`, 4 for `11110xxx` and the bytes above it that UTF-8 never
    /// writes, and 0 for a continuation byte, `10xxxxxx`, which leads none.
    pub(super) fn utf8_width(byte: u8) -> u8
    clauses [ensures result == utf8_width_table(byte) by utf8_width_is_the_table]
    {
        if byte < 0x80 {
            1
        } else if byte < 0xC0 {
            0
        } else if byte < 0xE0 {
            2
        } else if byte < 0xF0 {
            3
        } else {
            4
        }
    }
}

verify_island! {
    clean {
        def utf8_width_table (b : UInt8) : UInt8 :=
          if b < 128 then 1 else if b < 192 then 0 else if b < 224 then 2 else if b < 240 then 3 else 4

        theorem utf8_width_is_the_table (byte : UInt8) :
            trust_import_amacs__language__rust__utf8_width byte = utf8_width_table byte := by
          simp only [trust_import_amacs__language__rust__utf8_width, utf8_width_table]
    }
}

/// A number: decimal, `0x`/`0o`/`0b`, a fraction and an exponent, and any
/// suffix (`10u8`, `1.5e-3f64`).
fn number(cur: &mut Cursor<'_>, out: &mut Spans) {
    let start = cur.pos();
    let radix = cur.peek() == Some(b'0') && matches!(cur.nth(1), Some(b'x' | b'o' | b'b'));
    if radix {
        cur.bump_n(2);
    } else {
        cur.eat_while(|byte| byte.is_ascii_digit() || byte == b'_');
        if cur.peek() == Some(b'.') && cur.nth(1).is_some_and(|byte| byte.is_ascii_digit()) {
            cur.bump();
            cur.eat_while(|byte| byte.is_ascii_digit() || byte == b'_');
        }
        let exponent = matches!(cur.peek(), Some(b'e' | b'E'))
            && (cur.nth(1).is_some_and(|byte| byte.is_ascii_digit())
                || (matches!(cur.nth(1), Some(b'+' | b'-'))
                    && cur.nth(2).is_some_and(|byte| byte.is_ascii_digit())));
        if exponent {
            cur.bump_n(2);
        }
    }
    cur.eat_while(ident_byte);
    out.push(start, cur.pos(), SyntaxClass::Number);
}

/// A word: an identifier, a keyword or a macro call, or a prefixed literal
/// (`b"…"`, `br#"…"#`, `c"…"`, `r"…"`, `b'x'`) or raw identifier (`r#type`).
fn word(cur: &mut Cursor<'_>, out: &mut Spans) -> Mode {
    let start = cur.pos();
    let first = cur.peek();
    let second = cur.nth(1);
    let (prefix, raw) = match (first, second) {
        (Some(b'b' | b'c'), Some(b'r')) => (2, true),
        (Some(b'r'), _) => (1, true),
        (Some(b'b' | b'c'), _) => (1, false),
        _ => (0, false),
    };
    if prefix > 0 {
        let after = cur.nth(prefix);
        if after == Some(b'"') {
            cur.bump_n(prefix.saturating_add(1));
            return if raw {
                raw_quoted(cur, start, 0, out)
            } else {
                quoted(cur, start, out)
            };
        }
        if raw && after == Some(b'#') {
            cur.bump_n(prefix);
            let hashes = cur.eat_while(|byte| byte == b'#');
            if cur.eat(b'"') {
                let hashes = u8::try_from(hashes).unwrap_or(u8::MAX);
                return raw_quoted(cur, start, hashes, out);
            }
            cur.eat_while(ident_byte);
            return Mode::Code;
        }
        if first == Some(b'b') && second == Some(b'\'') {
            cur.bump();
            quote(cur, out);
            if let Some(last) = out.0.last_mut()
                && last.range.start == start.saturating_add(1)
                && last.class == SyntaxClass::String
            {
                last.range.start = start;
            }
            return Mode::Code;
        }
    }
    cur.eat_while(ident_byte);
    let text = cur.since(start);
    if cur.peek() == Some(b'!') && cur.nth(1) != Some(b'=') {
        cur.bump();
        out.push(start, cur.pos(), SyntaxClass::Meta);
    } else if KEYWORDS.contains(&text) {
        out.push(start, cur.pos(), SyntaxClass::Keyword);
    }
    Mode::Code
}

trust_contract_fn! {
    /// Whether `byte` belongs to a Rust word (an identifier, a keyword, a
    /// number's digits and suffix): an ASCII letter or digit, `_`, or any byte
    /// of a non-ASCII character, which the lexer reads whole as an identifier
    /// character so no span ever splits a character.
    #[expect(
        clippy::manual_range_contains,
        reason = "the comparisons are the clause the Clean theorem restates; `RangeInclusive::contains` is a call, which E6 refuses"
    )]
    pub(super) fn ident_byte(byte: u8) -> bool
    clauses [ensures result == ((byte >= 48 && byte <= 57)
        || ((byte >= 65 && byte <= 90)
        || ((byte >= 97 && byte <= 122)
        || (byte == 95 || byte >= 128))))
        by ident_byte_is_exactly_the_word_set]
    {
        (byte >= b'0' && byte <= b'9')
            || ((byte >= b'A' && byte <= b'Z')
                || ((byte >= b'a' && byte <= b'z') || (byte == b'_' || byte >= 0x80)))
    }
}

verify_island! {
    clean {
        theorem ident_byte_is_exactly_the_word_set (byte : UInt8) :
            trust_import_amacs__language__rust__ident_byte byte =
              ((decide (48 <= byte) && decide (byte <= 57))
                || ((decide (65 <= byte) && decide (byte <= 90))
                || ((decide (97 <= byte) && decide (byte <= 122))
                || (decide (byte = 95) || decide (128 <= byte))))) := by
          simp only [trust_import_amacs__language__rust__ident_byte]
    }
}

/// The bracket count and active indent levels of one line.
struct Brackets {
    depth: u16,
    levels: u32,
    shallowest: u16,
}

impl Brackets {
    const fn new(state: LexState) -> Self {
        Self {
            depth: state.depth,
            levels: state.levels,
            shallowest: state.depth,
        }
    }

    const fn open(&mut self) {
        self.depth = self.depth.saturating_add(1);
    }

    fn close(&mut self) {
        if self.depth > 0 {
            self.depth -= 1;
            self.levels &= !level_bit(self.depth);
            self.shallowest = self.shallowest.min(self.depth);
        }
    }

    const fn finish(self) -> (u16, u32) {
        let mut levels = self.levels;
        if self.depth > self.shallowest {
            levels |= level_bit(self.shallowest);
        }
        (self.depth, levels)
    }
}

/// The bit of `levels` for the bracket at `depth`; none past 31.
const fn level_bit(depth: u16) -> u32 {
    if depth < 32 { 1u32 << depth } else { 0 }
}

/// How many indent levels the brackets below `depth` add.
const fn levels_below(levels: u32, depth: u16) -> usize {
    let mask = if depth < 32 {
        (1u32 << depth) - 1
    } else {
        u32::MAX
    };
    (levels & mask).count_ones() as usize
}

pub(super) fn indent_column(state: LexState, kept: usize, line: &str, style: IndentStyle) -> usize {
    match state.mode {
        Mode::Quoted | Mode::RawQuoted(_) => style.columns_of(line),
        Mode::BlockComment(_) => kept,
        _ => {
            let closers = line
                .trim_start_matches([' ', '\t'])
                .bytes()
                .take_while(|byte| matches!(byte, b')' | b']' | b'}'))
                .count();
            let closers = u16::try_from(closers).unwrap_or(u16::MAX);
            let depth = state.depth.saturating_sub(closers);
            levels_below(state.levels, depth).saturating_mul(style.width)
        }
    }
}

/// The new line's prefix for a break at `at_break` (the state after the
/// text before it), outside a string: the bracket depth's indent, one level
/// fewer when `after` starts with a closer, or inside a block comment the
/// indentation of `before`.
pub(super) fn line_prefix(
    at_break: LexState,
    before: &str,
    after: &str,
    style: IndentStyle,
) -> String {
    match at_break.mode {
        Mode::BlockComment(_) => leading(before).to_owned(),
        _ => style.render(indent_column(at_break, 0, after, style)),
    }
}

/// The length of a `//` comment leader at the start of `line`: its blanks,
/// `//`, `///` or `//!`, and the blanks after it.
pub(super) fn comment_leader(line: &str) -> Option<usize> {
    let mut cur = Cursor::new(line);
    cur.eat_while(line_space);
    if !cur.looking_at(b"//") {
        return None;
    }
    cur.bump_n(2);
    if !cur.eat(b'/') {
        cur.eat(b'!');
    }
    cur.eat_while(line_space);
    Some(cur.pos())
}
