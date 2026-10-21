// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shell (sh, bash, zsh, ksh, dash): comments, quoted strings (open across
//! lines), here-documents, parameter expansions, reserved words,
//! assignments, numbers and the operators between commands.
//!
//! A here-document (`<<EOF`, `<<-'EOF'`) opens on the line after its
//! command and its body is one string per line, up to the line that is its
//! word. The [`LexState`] carries a 32-bit hash of the word rather than the
//! word, so it stays small; a body line whose hash matches by chance (about
//! one line in four billion) ends the body early. When one line opens two
//! here-documents, only the first is followed, and a `<<` inside an
//! arithmetic `$(( ))` or `(( ))` on its line is a shift.

use super::{Cursor, LexState, Mode, Spans, SyntaxClass, line_space};

/// The hash a here-document's word starts from.
const DELIMITER_SEED: u32 = 0x811C_9DC5;

/// The reserved words, and the builtins that declare or leave.
const KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "case", "esac", "for", "select", "while", "until", "do",
    "done", "in", "function", "time", "coproc", "[[", "]]", "!", "local", "export", "readonly",
    "declare", "typeset", "return", "exit", "break", "continue",
];

pub(super) fn lex(state: LexState, line: &str, out: &mut Spans) -> LexState {
    if let Mode::Heredoc(word, strip_tabs) = state.mode {
        return heredoc_body(state, line, word, strip_tabs, out);
    }
    let mut cur = Cursor::new(line);
    let mut mode = match state.mode {
        Mode::Quoted => double_quoted(&mut cur, 0, out),
        Mode::Literal => single_quoted(&mut cur, 0, out),
        _ => Mode::Code,
    };
    // Whether the next byte starts a word: only there is `#` a comment and
    // a word a reserved one.
    let mut word_start = mode == Mode::Code;
    // The first here-document the line opens, whose body starts on the next
    // line; and whether the line is inside `$(( ))` or `(( ))`, where `<<`
    // shifts.
    let mut heredoc = None;
    let mut arithmetic = false;
    while mode == Mode::Code {
        let Some(byte) = cur.peek() else {
            break;
        };
        let start = cur.pos();
        match byte {
            _ if line_space(byte) => {
                cur.bump();
                word_start = true;
                continue;
            }
            b'#' if word_start => {
                cur.finish();
                out.push(start, cur.pos(), SyntaxClass::Comment);
                break;
            }
            b'\\' => {
                cur.bump_n(2);
                cur.skip_to_boundary();
            }
            b'\'' => {
                cur.bump();
                mode = single_quoted(&mut cur, start, out);
            }
            b'"' => {
                cur.bump();
                mode = double_quoted(&mut cur, start, out);
            }
            b'$' => {
                arithmetic |= cur.looking_at(b"$((");
                dollar(&mut cur, out);
            }
            b'`' => {
                cur.bump();
                out.push(start, cur.pos(), SyntaxClass::Punct);
                word_start = true;
                continue;
            }
            _ if metachar(byte) => {
                cur.eat_while(metachar);
                let operator = cur.since(start);
                arithmetic = (arithmetic || operator.starts_with("((")) && !operator.contains("))");
                if operator == "<<" && !arithmetic {
                    let strip_tabs = cur.eat(b'-');
                    out.push(start, cur.pos(), SyntaxClass::Punct);
                    cur.eat_while(line_space);
                    let word = cur.pos();
                    if let Some(hash) = heredoc_word(&mut cur) {
                        out.push(word, cur.pos(), SyntaxClass::Meta);
                        heredoc = heredoc.or(Some(Mode::Heredoc(hash, strip_tabs)));
                    }
                } else {
                    out.push(start, cur.pos(), SyntaxClass::Punct);
                    word_start = true;
                    continue;
                }
            }
            _ => word(&mut cur, word_start, out),
        }
        word_start = false;
    }
    // A body starts once its command's line is done; one whose line ends
    // inside a string is not followed.
    if mode == Mode::Code
        && let Some(heredoc) = heredoc
    {
        mode = heredoc;
    }
    LexState { mode, ..state }
}

/// The word after `<<`, up to a blank or an operator, as the hash of the
/// line that ends the body: quotes are taken off (`'EOF'`, `"EOF"`, `E"O"F`
/// and `\EOF` all end on `EOF`). `None` when no word follows.
fn heredoc_word(cur: &mut Cursor<'_>) -> Option<u32> {
    let start = cur.pos();
    let mut hash = DELIMITER_SEED;
    while let Some(byte) = cur.peek() {
        if line_space(byte) || metachar(byte) {
            break;
        }
        cur.bump();
        match byte {
            b'\'' | b'"' => {
                while let Some(next) = cur.peek() {
                    cur.bump();
                    if next == byte {
                        break;
                    }
                    hash = delimiter_hash_step(hash, u32::from(next));
                }
            }
            b'\\' => {
                if let Some(next) = cur.peek() {
                    cur.bump();
                    hash = delimiter_hash_step(hash, u32::from(next));
                }
            }
            _ => hash = delimiter_hash_step(hash, u32::from(byte)),
        }
    }
    (cur.pos() > start).then_some(hash)
}

/// A line of a here-document's body: the line that is its word (after its
/// tabs, for `<<-`, and without a CRLF's `\r`) ends it, and any other line
/// is string.
fn heredoc_body(
    state: LexState,
    line: &str,
    word: u32,
    strip_tabs: bool,
    out: &mut Spans,
) -> LexState {
    let mut cur = Cursor::new(line);
    if strip_tabs {
        cur.eat_while(|byte| byte == b'\t');
    }
    let start = cur.pos();
    let end = line.strip_suffix('\r').map_or(line.len(), str::len);
    let mut hash = DELIMITER_SEED;
    while cur.pos() < end {
        let Some(byte) = cur.peek() else {
            break;
        };
        cur.bump();
        hash = delimiter_hash_step(hash, u32::from(byte));
    }
    if hash == word {
        out.push(start, end, SyntaxClass::Meta);
        return LexState {
            mode: Mode::Code,
            ..state
        };
    }
    out.push(0, line.len(), SyntaxClass::String);
    state
}

trust_contract_fn! {
    /// One byte into the hash of a here-document's word: `hash * 16777619 +
    /// byte` in wrapping `u32` arithmetic, a polynomial hash over the FNV
    /// prime. Only wrapping multiplies and adds: E6 kernel-imports those,
    /// and refuses the `^` an FNV-1a step would take.
    fn delimiter_hash_step(hash: u32, byte: u32) -> u32
    clauses [ensures result == delimiter_hash_poly(hash, byte) by delimiter_hash_step_is_the_polynomial]
    {
        hash.wrapping_mul(16_777_619).wrapping_add(byte)
    }
}

// Spec `heredoc-hash-step` (SPECS.md), kernel-checked against the compiled
// body: Clean's `UInt32` `*` and `+` are the wrapping ones.
verify_island! {
    clean {
        def delimiter_hash_poly (hash byte : UInt32) : UInt32 := hash * 16777619 + byte

        theorem delimiter_hash_step_is_the_polynomial (hash byte : UInt32) :
            trust_import_amacs__language__shell__delimiter_hash_step hash byte = delimiter_hash_poly hash byte := by
          simp only [trust_import_amacs__language__shell__delimiter_hash_step, delimiter_hash_poly]
    }
}

/// The rest of a `'…'` string from `start`: no escapes.
fn single_quoted(cur: &mut Cursor<'_>, start: usize, out: &mut Spans) -> Mode {
    while let Some(byte) = cur.peek() {
        cur.bump();
        if byte == b'\'' {
            out.push(start, cur.pos(), SyntaxClass::String);
            return Mode::Code;
        }
    }
    out.push(start, cur.pos(), SyntaxClass::String);
    Mode::Literal
}

/// The rest of a `"…"` string from `start`, where `\` escapes.
fn double_quoted(cur: &mut Cursor<'_>, start: usize, out: &mut Spans) -> Mode {
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

/// A `$`: a parameter (`$name`, `${…}`, `$1`, `$@`), a substitution's
/// opener (`$(`, `$((`), or an ANSI-C string (`$'…'`, ended by the line).
fn dollar(cur: &mut Cursor<'_>, out: &mut Spans) {
    let start = cur.pos();
    cur.bump();
    match cur.peek() {
        Some(b'{') => {
            while let Some(byte) = cur.peek() {
                cur.bump();
                if byte == b'}' {
                    break;
                }
            }
            out.push(start, cur.pos(), SyntaxClass::Meta);
        }
        Some(b'(') => {
            cur.eat_while(|byte| byte == b'(');
            out.push(start, cur.pos(), SyntaxClass::Punct);
        }
        Some(b'\'') => {
            cur.bump();
            while let Some(byte) = cur.peek() {
                cur.bump();
                match byte {
                    b'\\' => cur.bump(),
                    b'\'' => break,
                    _ => {}
                }
            }
            out.push(start, cur.pos(), SyntaxClass::String);
        }
        Some(byte) if byte.is_ascii_alphabetic() || byte == b'_' => {
            cur.eat_while(|next| next.is_ascii_alphanumeric() || next == b'_');
            out.push(start, cur.pos(), SyntaxClass::Meta);
        }
        Some(byte) if byte.is_ascii_digit() || b"@*#?$!-".contains(&byte) => {
            cur.bump();
            out.push(start, cur.pos(), SyntaxClass::Meta);
        }
        _ => out.push(start, cur.pos(), SyntaxClass::Punct),
    }
}

/// A plain word, up to a blank, an operator, a quote, a `$` or a `\`. At a
/// word start it may be a reserved word, a number, or an assignment's name.
fn word(cur: &mut Cursor<'_>, word_start: bool, out: &mut Spans) {
    let start = cur.pos();
    let read =
        cur.eat_while(|byte| !line_space(byte) && !metachar(byte) && !b"'\"$`\\".contains(&byte));
    if read == 0 {
        // The caller routes every byte that ends a word elsewhere; step over
        // one anyway, so the lexer always moves forward.
        cur.bump();
        cur.skip_to_boundary();
        return;
    }
    let text = cur.since(start);
    if !word_start {
        return;
    }
    if KEYWORDS.contains(&text) {
        out.push(start, cur.pos(), SyntaxClass::Keyword);
    } else if text.bytes().all(|byte| byte.is_ascii_digit()) {
        out.push(start, cur.pos(), SyntaxClass::Number);
    } else if let Some((name, _)) = text.split_once('=')
        && !name.is_empty()
        && !name.starts_with(|first: char| first.is_ascii_digit())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        out.push(start, start.saturating_add(name.len()), SyntaxClass::Meta);
    } else if text == "{" || text == "}" {
        out.push(start, cur.pos(), SyntaxClass::Punct);
    }
}

trust_contract_fn! {
    /// Whether `byte` is a shell operator byte, which ends a word even with
    /// no blank around it: `;`, `&`, `|`, `(`, `)`, `<` or `>`. A `#` right
    /// after one starts a comment (`a;# note`); inside a word it does not
    /// (`a#b`).
    pub(super) fn metachar(byte: u8) -> bool
    clauses [ensures result == (byte == 59 || (byte == 38 || (byte == 124
        || (byte == 40 || (byte == 41 || (byte == 60 || byte == 62)))))) by metachar_is_exactly_the_operators]
    {
        byte == b';'
            || (byte == b'&'
                || (byte == b'|' || (byte == b'(' || (byte == b')' || (byte == b'<' || byte == b'>')))))
    }
}

// Spec `shell-metachar-set` (SPECS.md), kernel-checked against the compiled
// body: the set is the clause, byte for byte.
verify_island! {
    clean {
        theorem metachar_is_exactly_the_operators (byte : UInt8) :
            trust_import_amacs__language__shell__metachar byte =
              (decide (byte = 59) || (decide (byte = 38) || (decide (byte = 124)
                || (decide (byte = 40) || (decide (byte = 41) || (decide (byte = 60) || decide (byte = 62))))))) := by
          simp only [trust_import_amacs__language__shell__metachar]
    }
}
