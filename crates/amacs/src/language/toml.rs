// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TOML: table headers, keys, strings (multi-line ones open across lines),
//! numbers and dates, booleans, comments and punctuation.
//!
//! A key is read where TOML puts one: at the start of a line outside any
//! array, and after `{` or `,` inside an inline table. So the
//! [`LexState`] carries the bracket nesting and, per depth, whether the
//! bracket is an inline table's `{` or an array's `[`: the strings on the
//! lines of a multi-line array are values, not keys.

use super::{Cursor, LexState, Mode, Spans, SyntaxClass, line_space};

pub(super) fn lex(state: LexState, line: &str, out: &mut Spans) -> LexState {
    let mut cur = Cursor::new(line);
    let mut nesting = Nesting {
        depth: state.depth,
        tables: state.levels,
    };
    let mut mode = match state.mode {
        Mode::Quoted => multiline(&mut cur, 0, b'"', out),
        Mode::Literal => multiline(&mut cur, 0, b'\'', out),
        _ => Mode::Code,
    };
    // A line that finishes a string continues a value; any other line starts
    // with a key, or a table header, unless it is inside an array.
    let mut expect_key = state.mode == Mode::Code && nesting.depth == 0;
    let mut line_start = expect_key;
    while mode == Mode::Code {
        cur.eat_while(line_space);
        let Some(byte) = cur.peek() else {
            break;
        };
        let start = cur.pos();
        if byte == b'#' {
            cur.finish();
            out.push(start, cur.pos(), SyntaxClass::Comment);
            break;
        }
        if line_start && byte == b'[' {
            header(&mut cur, out);
            expect_key = false;
            line_start = false;
            continue;
        }
        line_start = false;
        if expect_key {
            match byte {
                b'"' | b'\'' => {
                    cur.bump();
                    single_line(&mut cur, byte);
                    out.push(start, cur.pos(), SyntaxClass::Keyword);
                }
                b'.' => {
                    cur.bump();
                    out.push(start, cur.pos(), SyntaxClass::Punct);
                }
                b'=' => {
                    cur.bump();
                    out.push(start, cur.pos(), SyntaxClass::Punct);
                    expect_key = false;
                }
                _ if bare_key_byte(byte) => {
                    cur.eat_while(bare_key_byte);
                    out.push(start, cur.pos(), SyntaxClass::Keyword);
                }
                b'}' => {
                    cur.bump();
                    nesting.close();
                    out.push(start, cur.pos(), SyntaxClass::Punct);
                    expect_key = false;
                }
                _ => {
                    cur.bump();
                    cur.skip_to_boundary();
                    expect_key = false;
                }
            }
            continue;
        }
        match byte {
            b'"' | b'\'' => {
                let triple = if byte == b'"' {
                    cur.looking_at(b"\"\"\"")
                } else {
                    cur.looking_at(b"'''")
                };
                if triple {
                    cur.bump_n(3);
                    mode = multiline(&mut cur, start, byte, out);
                } else {
                    cur.bump();
                    single_line(&mut cur, byte);
                    out.push(start, cur.pos(), SyntaxClass::String);
                }
            }
            b'[' | b'{' => {
                cur.bump();
                nesting.open(byte == b'{');
                out.push(start, cur.pos(), SyntaxClass::Punct);
                expect_key = byte == b'{';
            }
            b']' | b'}' => {
                cur.bump();
                nesting.close();
                out.push(start, cur.pos(), SyntaxClass::Punct);
            }
            b',' => {
                cur.bump();
                out.push(start, cur.pos(), SyntaxClass::Punct);
                expect_key = nesting.in_table();
            }
            b'=' => {
                cur.bump();
                out.push(start, cur.pos(), SyntaxClass::Punct);
            }
            _ if value_byte(byte) => {
                cur.eat_while(value_byte);
                let word = cur.since(start);
                let class = match word {
                    "true" | "false" => Some(SyntaxClass::Keyword),
                    "inf" | "+inf" | "-inf" | "nan" | "+nan" | "-nan" => Some(SyntaxClass::Number),
                    _ => word
                        .trim_start_matches(['+', '-'])
                        .starts_with(|first: char| first.is_ascii_digit())
                        .then_some(SyntaxClass::Number),
                };
                if let Some(class) = class {
                    out.push(start, cur.pos(), class);
                }
            }
            _ => {
                cur.bump();
                cur.skip_to_boundary();
            }
        }
    }
    LexState {
        mode,
        depth: nesting.depth,
        levels: nesting.tables,
        ..state
    }
}

/// A table header, `[a.b]` or `[[a]]`, through its closing bracket; a quoted
/// key inside it may hold a `]`.
fn header(cur: &mut Cursor<'_>, out: &mut Spans) {
    let start = cur.pos();
    cur.bump();
    let array = cur.eat(b'[');
    while let Some(byte) = cur.peek() {
        cur.bump();
        match byte {
            b'"' | b'\'' => single_line(cur, byte),
            b']' => {
                if array {
                    cur.eat(b']');
                }
                break;
            }
            _ => {}
        }
    }
    out.push(start, cur.pos(), SyntaxClass::Heading);
}

/// The rest of a one-line string after its opening `quote`: `"` takes
/// escapes, `'` does not. An unclosed one ends with the line.
fn single_line(cur: &mut Cursor<'_>, quote: u8) {
    while let Some(byte) = cur.peek() {
        cur.bump();
        if byte == quote {
            return;
        }
        if byte == b'\\' && quote == b'"' {
            cur.bump();
        }
    }
}

/// The rest of a `"""` or `'''` string from `start`, which may close on this
/// line or carry on to the next.
fn multiline(cur: &mut Cursor<'_>, start: usize, quote: u8, out: &mut Spans) -> Mode {
    let close: &[u8] = if quote == b'"' { b"\"\"\"" } else { b"'''" };
    while !cur.done() {
        if cur.looking_at(close) {
            cur.bump_n(3);
            // Up to two more quotes belong to the string (`""""x"""""`).
            for _ in 0..2 {
                cur.eat(quote);
            }
            out.push(start, cur.pos(), SyntaxClass::String);
            return Mode::Code;
        }
        let byte = cur.peek();
        cur.bump();
        if byte == Some(b'\\') && quote == b'"' {
            cur.bump();
        }
    }
    out.push(start, cur.pos(), SyntaxClass::String);
    if quote == b'"' {
        Mode::Quoted
    } else {
        Mode::Literal
    }
}

/// The open brackets: how deep, and which of them (one bit per depth below
/// 32) are inline tables.
struct Nesting {
    depth: u16,
    tables: u32,
}

impl Nesting {
    fn open(&mut self, table: bool) {
        if self.depth < 32 {
            let bit = 1u32 << self.depth;
            if table {
                self.tables |= bit;
            } else {
                self.tables &= !bit;
            }
        }
        self.depth = self.depth.saturating_add(1);
    }

    fn close(&mut self) {
        if self.depth > 0 {
            self.depth -= 1;
            if self.depth < 32 {
                self.tables &= !(1u32 << self.depth);
            }
        }
    }

    /// Whether the innermost open bracket is an inline table.
    fn in_table(&self) -> bool {
        match self.depth.checked_sub(1) {
            Some(depth) if depth < 32 => self.tables & (1u32 << depth) != 0,
            _ => false,
        }
    }
}

trust_contract_fn! {
    /// Whether `byte` belongs to a bare value: a number, a date or time, a
    /// boolean (`-1_000`, `0x1F`, `6.02e+23`, `1979-05-27T07:32:00Z`,
    /// `true`): an ASCII letter or digit, `_`, `+`, `-`, `.` or `:`.
    #[expect(
        clippy::manual_range_contains,
        reason = "the comparisons are the clause the Clean theorem restates; `RangeInclusive::contains` is a call, which E6 refuses"
    )]
    pub(super) fn value_byte(byte: u8) -> bool
    clauses [ensures result == ((byte >= 48 && byte <= 57)
        || ((byte >= 65 && byte <= 90)
        || ((byte >= 97 && byte <= 122)
        || (byte == 95 || (byte == 43 || (byte == 45 || (byte == 46 || byte == 58)))))))
        by value_byte_is_exactly_the_value_set]
    {
        (byte >= b'0' && byte <= b'9')
            || ((byte >= b'A' && byte <= b'Z')
                || ((byte >= b'a' && byte <= b'z')
                    || (byte == b'_'
                        || (byte == b'+' || (byte == b'-' || (byte == b'.' || byte == b':'))))))
    }
}

// Spec `toml-value-byte-set` (SPECS.md), kernel-checked against the compiled
// body: the set is the clause, byte for byte.
verify_island! {
    clean {
        theorem value_byte_is_exactly_the_value_set (byte : UInt8) :
            trust_import_amacs__language__toml__value_byte byte =
              ((decide (48 <= byte) && decide (byte <= 57))
                || ((decide (65 <= byte) && decide (byte <= 90))
                || ((decide (97 <= byte) && decide (byte <= 122))
                || (decide (byte = 95) || (decide (byte = 43) || (decide (byte = 45)
                || (decide (byte = 46) || decide (byte = 58)))))))) := by
          simp only [trust_import_amacs__language__toml__value_byte]
    }
}

trust_contract_fn! {
    /// Whether `byte` may appear in a bare TOML key: an ASCII letter or
    /// digit, `_` or `-`. A quoted key may hold anything else.
    #[expect(
        clippy::manual_range_contains,
        reason = "the comparisons are the clause the Clean theorem restates; `RangeInclusive::contains` is a call, which E6 refuses"
    )]
    pub(super) fn bare_key_byte(byte: u8) -> bool
    clauses [ensures result == ((byte >= 48 && byte <= 57)
        || ((byte >= 65 && byte <= 90)
        || ((byte >= 97 && byte <= 122)
        || (byte == 95 || byte == 45))))
        by bare_key_byte_is_exactly_the_key_set]
    {
        (byte >= b'0' && byte <= b'9')
            || ((byte >= b'A' && byte <= b'Z')
                || ((byte >= b'a' && byte <= b'z') || (byte == b'_' || byte == b'-')))
    }
}

// Spec `toml-bare-key-byte-set` (SPECS.md), kernel-checked against the
// compiled body: the set is the clause, byte for byte.
verify_island! {
    clean {
        theorem bare_key_byte_is_exactly_the_key_set (byte : UInt8) :
            trust_import_amacs__language__toml__bare_key_byte byte =
              ((decide (48 <= byte) && decide (byte <= 57))
                || ((decide (65 <= byte) && decide (byte <= 90))
                || ((decide (97 <= byte) && decide (byte <= 122))
                || (decide (byte = 95) || decide (byte = 45))))) := by
          simp only [trust_import_amacs__language__toml__bare_key_byte]
    }
}
