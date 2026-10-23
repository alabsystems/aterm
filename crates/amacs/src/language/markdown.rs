// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Markdown: headings, fenced code (open across lines, in a quote too),
//! quotes, list markers, thematic breaks, inline code, link targets,
//! autolinks and HTML comments (open across lines); and list and quote
//! continuation for line breaks and indentation.

use super::{
    Cursor, FillPrefix, IndentStyle, LexState, Mode, Spans, SyntaxClass, decimal_push, leading,
    leading_blanks, line_space,
};

const MAX_LIST_NUMBER: u32 = 999_999_999;

pub(super) fn lex(state: LexState, line: &str, out: &mut Spans) -> LexState {
    let mut cur = Cursor::new(line);
    let mode = match state.mode {
        Mode::Fence(mark, length, quotes) => {
            match fenced(state, &mut cur, mark, length, quotes, out) {
                Some(next) => return next,
                None => block(&mut cur, out),
            }
        }
        Mode::HtmlComment => html_comment(&mut cur, 0, out),
        _ => block(&mut cur, out),
    };
    let mode = if mode == Mode::Code {
        inline(&mut cur, out)
    } else {
        mode
    };
    LexState { mode, ..state }
}

fn fenced(
    state: LexState,
    cur: &mut Cursor<'_>,
    mark: u8,
    length: u8,
    quotes: u8,
    out: &mut Spans,
) -> Option<LexState> {
    let body = quote_prefix(cur.line, quotes)?;
    let mut seen = 0u8;
    while seen < quotes {
        cur.eat_while(line_space);
        let marker = cur.pos();
        cur.bump();
        out.push(marker, cur.pos(), SyntaxClass::Punct);
        seen = seen.saturating_add(1);
    }
    *cur = rewind(cur, body);
    cur.eat_while(line_space);
    let start = cur.pos();
    let run = cur.eat_while(|byte| byte == mark);
    cur.eat_while(line_space);
    if run >= usize::from(length) && cur.done() {
        out.push(start, cur.pos(), SyntaxClass::Meta);
        return Some(LexState {
            mode: Mode::Code,
            ..state
        });
    }
    out.push(body, cur.len(), SyntaxClass::String);
    Some(state)
}

fn quote_prefix(line: &str, quotes: u8) -> Option<usize> {
    let mut cur = Cursor::new(line);
    let mut seen = 0u8;
    while seen < quotes {
        cur.eat_while(line_space);
        if !cur.eat(b'>') {
            return None;
        }
        seen = seen.saturating_add(1);
    }
    if quotes > 0 {
        cur.eat(b' ');
    }
    Some(cur.pos())
}

fn block(cur: &mut Cursor<'_>, out: &mut Spans) -> Mode {
    let mut quotes = 0u8;
    loop {
        cur.eat_while(line_space);
        let start = cur.pos();
        let Some(byte) = cur.peek() else {
            return Mode::Code;
        };
        match byte {
            b'`' | b'~' => {
                let run = cur.eat_while(|next| next == byte);
                let fence = run >= 3 && !(byte == b'`' && cur.rest().contains('`'));
                if fence {
                    cur.finish();
                    out.push(start, cur.pos(), SyntaxClass::Meta);
                    return Mode::Fence(byte, u8::try_from(run).unwrap_or(u8::MAX), quotes);
                }
                *cur = rewind(cur, start);
                return Mode::Code;
            }
            b'#' => {
                let hashes = cur.eat_while(|next| next == b'#');
                if hashes <= 6 && cur.peek().is_none_or(line_space) {
                    cur.finish();
                    out.push(start, cur.pos(), SyntaxClass::Heading);
                    return Mode::Code;
                }
                *cur = rewind(cur, start);
                return Mode::Code;
            }
            b'>' => {
                cur.bump();
                quotes = quotes.saturating_add(1);
                out.push(start, cur.pos(), SyntaxClass::Punct);
            }
            b'=' if setext_underline(cur.rest(), b'=') => {
                cur.finish();
                out.push(start, cur.pos(), SyntaxClass::Heading);
                return Mode::Code;
            }
            b'-' | b'*' | b'_' if thematic_break(cur.rest()) => {
                cur.finish();
                out.push(start, cur.pos(), SyntaxClass::Punct);
                return Mode::Code;
            }
            _ => {
                if let Some(item) = list_item(cur.rest()) {
                    let marker_end = start.saturating_add(item.marker_end);
                    out.push(start, marker_end, SyntaxClass::Punct);
                    cur.bump_n(item.marker_end);
                    if item.task {
                        cur.eat_while(line_space);
                        let task = cur.pos();
                        cur.bump_n(3);
                        out.push(task, cur.pos(), SyntaxClass::Punct);
                    }
                }
                return Mode::Code;
            }
        }
    }
}

const fn rewind<'a>(cur: &Cursor<'a>, pos: usize) -> Cursor<'a> {
    Cursor {
        line: cur.line,
        pos,
    }
}

fn setext_underline(rest: &str, mark: u8) -> bool {
    let body = rest.trim_end_matches([' ', '\t', '\r']);
    !body.is_empty() && body.bytes().all(|byte| byte == mark)
}

fn thematic_break(rest: &str) -> bool {
    let mut mark = None;
    let mut count = 0usize;
    for byte in rest.bytes() {
        if line_space(byte) {
            continue;
        }
        if mark.is_some_and(|mark| mark != byte) || !matches!(byte, b'-' | b'*' | b'_') {
            return false;
        }
        mark = Some(byte);
        count = count.saturating_add(1);
    }
    count >= 3
}

fn inline(cur: &mut Cursor<'_>, out: &mut Spans) -> Mode {
    while let Some(byte) = cur.peek() {
        let start = cur.pos();
        match byte {
            b'`' => {
                let run = cur.eat_while(|next| next == b'`');
                while !cur.done() {
                    if cur.peek() == Some(b'`') {
                        if cur.eat_while(|next| next == b'`') == run {
                            break;
                        }
                    } else {
                        cur.bump();
                    }
                }
                out.push(start, cur.pos(), SyntaxClass::String);
            }
            b'\\' => {
                cur.bump();
                if cur.peek().is_some_and(|next| next.is_ascii_punctuation()) {
                    cur.bump();
                }
            }
            b'<' if cur.looking_at(b"<!--") => {
                cur.bump_n(4);
                let mode = html_comment(cur, start, out);
                if mode != Mode::Code {
                    return mode;
                }
            }
            b'<' if cur.nth(1).is_some_and(|next| next.is_ascii_alphabetic()) => {
                cur.bump();
                cur.eat_while(|next| next != b'>' && !line_space(next) && next != b'<');
                if cur.eat(b'>') {
                    out.push(start, cur.pos(), SyntaxClass::Meta);
                }
            }
            b']' if cur.nth(1) == Some(b'(') => {
                cur.bump_n(2);
                out.push(start, cur.pos(), SyntaxClass::Punct);
                let target = cur.pos();
                cur.eat_while(|next| next != b')' && !line_space(next));
                out.push(target, cur.pos(), SyntaxClass::Meta);
                let close = cur.pos();
                if cur.eat(b')') {
                    out.push(close, cur.pos(), SyntaxClass::Punct);
                }
            }
            b'|' => {
                cur.bump();
                out.push(start, cur.pos(), SyntaxClass::Punct);
            }
            _ => cur.bump(),
        }
    }
    Mode::Code
}

fn html_comment(cur: &mut Cursor<'_>, start: usize, out: &mut Spans) -> Mode {
    while !cur.done() {
        if cur.looking_at(b"-->") {
            cur.bump_n(3);
            out.push(start, cur.pos(), SyntaxClass::Comment);
            return Mode::Code;
        }
        cur.bump();
    }
    out.push(start, cur.pos(), SyntaxClass::Comment);
    Mode::HtmlComment
}

/// A list item's marker, as read from the start of a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ListItem {
    /// Bytes of indentation before the marker.
    indent: usize,
    /// Where the marker (`-`, `3.`) ends, from the start of the line.
    marker_end: usize,
    /// Where the item's text starts (after the marker, its blanks and a
    /// task box).
    content: usize,
    marker: Marker,
    /// Whether the item is a task (`- [ ]`, `- [x]`).
    task: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Marker {
    Bullet(u8),
    Ordered { number: u32, delimiter: u8 },
}

fn list_item(line: &str) -> Option<ListItem> {
    let indent = leading_blanks(line);
    let mut cur = Cursor::new(line);
    cur.bump_n(indent);
    let marker = match cur.peek()? {
        bullet @ (b'-' | b'*' | b'+') => {
            if thematic_break(cur.rest()) {
                return None;
            }
            cur.bump();
            Marker::Bullet(bullet)
        }
        byte if byte.is_ascii_digit() => {
            let mut number = 0u32;
            let mut digits = 0usize;
            while let Some(digit) = cur.peek().filter(u8::is_ascii_digit) {
                cur.bump();
                digits = digits.saturating_add(1);
                if digits > 9 {
                    return None;
                }
                number = decimal_push(number, digit);
            }
            let delimiter = cur.peek().filter(|byte| matches!(byte, b'.' | b')'))?;
            cur.bump();
            Marker::Ordered { number, delimiter }
        }
        _ => return None,
    };
    let marker_end = cur.pos();
    if !cur.peek().is_none_or(line_space) {
        return None;
    }
    cur.eat_while(line_space);
    let task = cur.looking_at(b"[ ]") || cur.looking_at(b"[x]") || cur.looking_at(b"[X]");
    let task = task && cur.nth(3).is_none_or(line_space);
    if task {
        cur.bump_n(3);
        cur.eat_while(line_space);
    }
    Some(ListItem {
        indent,
        marker_end,
        content: cur.pos(),
        marker,
        task,
    })
}

impl ListItem {
    fn is_empty(self, line: &str) -> bool {
        line.get(self.content..)
            .is_none_or(|rest| rest.bytes().all(line_space))
    }

    fn next_marker(self) -> String {
        let mut marker = match self.marker {
            Marker::Bullet(bullet) => char::from(bullet).to_string(),
            Marker::Ordered { number, delimiter } => {
                let next = if number < MAX_LIST_NUMBER {
                    number + 1
                } else {
                    number
                };
                format!("{next}{}", char::from(delimiter))
            }
        };
        marker.push(' ');
        if self.task {
            marker.push_str("[ ] ");
        }
        marker
    }
}

/// A line break after `before` (a line starting in `state`), outside a
/// string: the bytes of `before` that stay (out of `keep`, all of it but its
/// trailing blanks) and the new line's prefix.
pub(super) fn line_break(state: LexState, before: &str, keep: usize) -> (usize, String) {
    if let Mode::Fence(_, _, quotes) = state.mode {
        let markers = quote_prefix(before, quotes).unwrap_or(0);
        let body = before.get(markers..).unwrap_or("");
        let indent = markers.saturating_add(leading_blanks(body));
        return (keep, before.get(..indent).unwrap_or("").to_owned());
    }
    if state.mode == Mode::HtmlComment {
        return (keep, leading(before).to_owned());
    }
    let quoted = quote_leader(before);
    let text = quoted.unwrap_or(0);
    let markers = before.get(..text).unwrap_or("");
    let inner = before.get(text..).unwrap_or("");
    match list_item(inner) {
        Some(item) if item.is_empty(inner) => (
            markers.trim_end_matches([' ', '\t']).len(),
            markers.to_owned(),
        ),
        Some(item) => (
            keep,
            format!("{markers}{}{}", leading(inner), item.next_marker()),
        ),
        None if quoted.is_some() && inner.bytes().all(line_space) => (0, String::new()),
        None => (keep, format!("{markers}{}", leading(inner))),
    }
}

/// The indent for a line under `previous` when `previous` is a list item
/// with text: the column its text starts at. `None` keeps the indentation of
/// the line above.
pub(super) fn indent_column(
    state: LexState,
    previous: Option<&str>,
    line: &str,
    style: IndentStyle,
) -> Option<usize> {
    if matches!(state.mode, Mode::Fence(..) | Mode::HtmlComment) {
        return None;
    }
    if list_item(line).is_some() {
        return Some(style.columns_of(line));
    }
    let above = previous?;
    let item = list_item(above).filter(|item| !item.is_empty(above))?;
    Some(style.columns_through(above, item.content))
}

/// A list item's hanging indent for fill: the first line keeps its marker,
/// the lines after it line up with its text.
pub(super) fn hanging_indent(line: &str, style: IndentStyle) -> Option<FillPrefix> {
    let item = list_item(line)?;
    let columns = style.columns_through(line, item.content);
    let indent = style.columns_through(line, item.indent);
    let mut rest = leading(line).to_owned();
    rest.push_str(&" ".repeat(columns.saturating_sub(indent)));
    Some(FillPrefix {
        first: item.content,
        rest,
    })
}

/// The length of a quote's markers at the start of `line` (`> `, `> > `).
pub(super) fn quote_leader(line: &str) -> Option<usize> {
    let mut cur = Cursor::new(line);
    let mut quotes = 0usize;
    loop {
        cur.eat_while(line_space);
        if !cur.eat(b'>') {
            break;
        }
        quotes = quotes.saturating_add(1);
    }
    cur.eat_while(line_space);
    (quotes > 0).then_some(cur.pos())
}
