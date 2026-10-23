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
mod tests {
    use super::*;
    use crate::protocol::anchor::Anchor;

    #[test]
    fn each_line_prints_its_anchor_and_text() {
        let text = "fn main() {\r\n    run();\r\n}";
        let read = format_lines(text, 7, LineRange::ALL, DOC_READ_CAP);
        let expected = format!(
            "1:{}|fn main() {{\n2:{}|    run();\n3:{}|}}\n-- lines=3 seq=7\n",
            Hash6::of("fn main() {"),
            Hash6::of("    run();"),
            Hash6::of("}")
        );
        assert_eq!(read, expected);
        for row in read.lines().filter(|row| !row.starts_with("--")) {
            let (anchor, _) = row.split_once('|').unwrap();
            let anchor: Anchor = anchor.parse().unwrap();
            assert_eq!(Anchor::of_line(text, anchor.line), Some(anchor));
        }
        assert_eq!(
            format_lines("", 0, LineRange::ALL, 64),
            "-- lines=0 seq=0\n"
        );
        assert_eq!(
            format_lines("a\nb\nc\n", 1, "2".parse().unwrap(), 64),
            format!("2:{}|b\n-- lines=3 seq=1\n", Hash6::of("b"))
        );
        assert_eq!(
            format_lines("a\n", 1, "5-9".parse().unwrap(), 64),
            "-- lines=1 seq=1\n"
        );
    }

    #[test]
    fn cap_and_next_cursor() {
        let text = (1..=10)
            .map(|n| format!("line {n:02}\n"))
            .collect::<String>();
        let row = format_lines(&text, 3, "1".parse().unwrap(), 1024);
        let row_len = row.lines().next().unwrap().len() + 1;
        assert_eq!(row_len, 17);
        let read = format_lines(&text, 3, LineRange::ALL, row_len * 4 + 5);
        assert_eq!(read.lines().count(), 5, "four lines and the trailer");
        assert!(read.ends_with("-- lines=10 seq=3 next=5\n"), "{read}");
        let rest = format_lines(&text, 3, "5-".parse().unwrap(), 1024);
        assert!(rest.starts_with("5:"), "{rest}");
        assert!(rest.ends_with("-- lines=10 seq=3\n"), "{rest}");
        let bounded = format_lines(&text, 3, "1-2".parse().unwrap(), 1024);
        assert!(bounded.ends_with("-- lines=10 seq=3\n"), "{bounded}");
        let wide = format!("{}\nshort\n", "é".repeat(40));
        let read = format_lines(&wide, 0, LineRange::ALL, 30);
        let first = read.lines().next().unwrap();
        assert!(first.len() < 30, "{first}");
        assert!(first.starts_with(&format!("1:{}|é", Hash6::of(&"é".repeat(40)))));
        assert!(read.ends_with("-- lines=2 seq=0 next=2 cut=1\n"), "{read}");
        let read = format_lines(&wide, 0, "1-1".parse().unwrap(), 30);
        assert!(read.ends_with("-- lines=2 seq=0 cut=1\n"), "{read}");
    }

    #[test]
    fn line_ranges_parse_what_they_print() {
        for (text, range) in [
            (
                "3-9",
                LineRange {
                    first: 3,
                    last: Some(9),
                },
            ),
            (
                "4-",
                LineRange {
                    first: 4,
                    last: None,
                },
            ),
            (
                "2",
                LineRange {
                    first: 2,
                    last: Some(2),
                },
            ),
        ] {
            assert_eq!(text.parse::<LineRange>(), Ok(range));
            assert_eq!(range.to_string(), text);
        }
        for bad in ["", "0", "0-3", "5-3", "-3", "a-b", "1-2-3", "+1"] {
            assert!(bad.parse::<LineRange>().is_err(), "{bad}");
        }
    }
}
