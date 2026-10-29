// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `ask` exchange's lines: one ASCII line per message, `verb key=value…`, values in hex,
//! padded base64 or percent-escaped text. The member speaks first with its greeting.

use crate::{Error, Result};
use std::io::{Read, Write};

/// The longest line either side reads.
pub(super) const MAX_LINE: usize = 4096;
/// The longest free text field (a name, a model).
const MAX_TEXT: usize = 64;

/// Read one `\n`-ended line, byte by byte so nothing after it is taken.
pub(super) fn read_line<S: Read + ?Sized>(stream: &mut S) -> Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if stream.read(&mut byte)? == 0 {
            return Err(Error::new("the other Mac closed the join exchange"));
        }
        if byte[0] == b'\n' {
            break;
        }
        if line.len() >= MAX_LINE {
            return Err(Error::new("a join exchange line exceeds its bound"));
        }
        line.push(byte[0]);
    }
    String::from_utf8(line)
        .ok()
        .filter(|line| line.bytes().all(|b| b.is_ascii_graphic() || b == b' '))
        .ok_or_else(|| Error::new("a join exchange line is not plain ASCII"))
}

/// Write one line and flush it.
pub(super) fn write_line<S: Write + ?Sized>(stream: &mut S, line: &str) -> Result<()> {
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

/// A parsed line: its verb and its `key=value` fields.
pub(super) struct Line<'a> {
    pub(super) verb: &'a str,
    fields: Vec<(&'a str, &'a str)>,
}

impl<'a> Line<'a> {
    pub(super) fn parse(line: &'a str) -> Self {
        let mut words = line.split(' ');
        let verb = words.next().unwrap_or("");
        let fields = words.filter_map(|word| word.split_once('=')).collect();
        Self { verb, fields }
    }

    pub(super) fn get(&self, key: &str) -> Result<&'a str> {
        self.fields
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .ok_or_else(|| Error::new(format!("a join exchange line has no {key}")))
    }

    pub(super) fn hex<const N: usize>(&self, key: &str) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        let text = self.get(key)?;
        if text.len() != 2 * N || !aterm_codec::hex::decode_into(text.as_bytes(), &mut out) {
            return Err(Error::new(format!(
                "a join exchange line's {key} is not hex"
            )));
        }
        Ok(out)
    }

    pub(super) fn b64(&self, key: &str) -> Result<Vec<u8>> {
        crate::b64::decode_padded(self.get(key)?)
            .ok_or_else(|| Error::new(format!("a join exchange line's {key} is not base64")))
    }

    pub(super) fn text(&self, key: &str) -> Result<String> {
        unescape(self.get(key)?)
    }
}

pub(super) fn hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(bytes)
}

/// `N` bytes from exactly `2 * N` hex digits.
pub(super) fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let mut out = [0u8; N];
    (text.len() == 2 * N && aterm_codec::hex::decode_into(text.as_bytes(), &mut out)).then_some(out)
}

pub(super) fn b64(bytes: &[u8]) -> String {
    crate::b64::encode_padded(bytes)
}

/// Percent-escape free text into one field: letters, digits and `.-_` stay, the rest is
/// `%XX`; it is cut to [`MAX_TEXT`] bytes at a character boundary first.
pub(super) fn escape(text: &str) -> String {
    escape_within(text, MAX_TEXT)
}

/// The text [`escape`] wrote.
pub(super) fn unescape(text: &str) -> Result<String> {
    unescape_within(text, MAX_TEXT)
}

fn escape_within(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::new();
    for byte in text[..end].bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn unescape_within(text: &str, max: usize) -> Result<String> {
    let bad = || Error::new("a join exchange line's text is not escaped");
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let pair = bytes.get(at + 1..at + 3).ok_or_else(bad)?;
            let mut one = [0u8; 1];
            if !aterm_codec::hex::decode_into(pair, &mut one) {
                return Err(bad());
            }
            out.push(one[0]);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    if out.len() > max {
        return Err(bad());
    }
    String::from_utf8(out)
        .ok()
        .filter(|text| !text.chars().any(char::is_control))
        .ok_or_else(bad)
}

/// The longest reason a `NO` line carries.
const MAX_WHY: usize = 400;

/// A `NO` line naming `why`.
pub(super) fn refusal(why: &str) -> String {
    format!("NO why={}", escape_within(why, MAX_WHY))
}

/// The answer line `line`, `OK …` taken apart, or the error its `NO` names.
pub(super) fn answer(line: &str) -> Result<Line<'_>> {
    let parsed = Line::parse(line);
    match parsed.verb {
        "OK" => Ok(parsed),
        "NO" => Err(Error::new(
            parsed
                .get("why")
                .ok()
                .and_then(|why| unescape_within(why, MAX_WHY).ok())
                .unwrap_or_else(|| "the member refused".to_owned()),
        )),
        _ => Err(Error::new("the member answered something else")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_fields_round_trip_and_stay_one_word() {
        for text in ["m2", "Andrew’s MacBook Pro", "Mac14,6", "a b=c%d"] {
            let escaped = escape(text);
            assert!(
                !escaped.contains(' ') && !escaped.contains('='),
                "{escaped}"
            );
            assert_eq!(unescape(&escaped).unwrap(), text);
        }
        assert!(unescape("%0A").is_err(), "a control character is refused");
    }
}
