// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Anchored line formatting and range parsing for `doc read` and `amacs --read`.

use std::fmt;
use std::fmt::Write as _;
use std::str::FromStr;

use super::anchor::{Hash6, Lines, parse_line_number};

/// The cap `doc read` prints under: 256 KiB of lines.
pub const DOC_READ_CAP: usize = 256 * 1024;

/// Line range to read: `A-B` (1-based inclusive), `A-` (to the end), or `A` (single line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineRange {
    /// The first line, 1-based.
    pub first: usize,
    /// The last line, inclusive; `None` reads to the end.
    pub last: Option<usize>,
}

impl LineRange {
    /// The whole text.
    pub const ALL: Self = Self {
        first: 1,
        last: None,
    };
}

impl fmt::Display for LineRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.last {
            Some(last) if last == self.first => write!(f, "{}", self.first),
            Some(last) => write!(f, "{}-{last}", self.first),
            None => write!(f, "{}-", self.first),
        }
    }
}

impl FromStr for LineRange {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let bad = || format!("bad lines={text} (want <first>-<last>, <first>- or <line>, from 1)");
        let range = match text.split_once('-') {
            None => {
                let line = parse_line_number(text).ok_or_else(bad)?;
                Self {
                    first: line,
                    last: Some(line),
                }
            }
            Some((first, "")) => Self {
                first: parse_line_number(first).ok_or_else(bad)?,
                last: None,
            },
            Some((first, last)) => Self {
                first: parse_line_number(first).ok_or_else(bad)?,
                last: Some(parse_line_number(last).ok_or_else(bad)?),
            },
        };
        match range.last {
            Some(last) if last < range.first => Err(bad()),
            _ => Ok(range),
        }
    }
}

/// Format `range` of `text` at sequence `seq` as anchored lines within `cap_bytes`, then a trailer.
#[must_use]
pub fn format_lines(text: &str, seq: u64, range: LineRange, cap_bytes: usize) -> String {
    let lines = Lines::of(text);
    let total = lines.count();
    let last = range.last.map_or(total, |last| last.min(total));
    let mut out = String::new();
    let mut next = None;
    let mut cut = None;
    let mut line = range.first;
    while line <= last {
        let Some(content) = lines.content(line) else {
            break;
        };
        let mut row = format!("{line}:{}|", Hash6::of(content));
        let whole = row.len().saturating_add(content.len()).saturating_add(1);
        if out.len().saturating_add(whole) > cap_bytes {
            if !out.is_empty() {
                next = Some(line);
                break;
            }
            let room = cap_bytes.saturating_sub(row.len()).saturating_sub(1);
            row.push_str(prefix_within(content, room));
            row.push('\n');
            out.push_str(&row);
            cut = Some(line);
            next = (line < last).then(|| line.saturating_add(1));
            break;
        }
        row.push_str(content);
        row.push('\n');
        out.push_str(&row);
        line = line.saturating_add(1);
    }
    let _ = write!(out, "-- lines={total} seq={seq}");
    if let Some(next) = next {
        let _ = write!(out, " next={next}");
    }
    if let Some(cut) = cut {
        let _ = write!(out, " cut={cut}");
    }
    out.push('\n');
    out
}

fn prefix_within(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or("")
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
