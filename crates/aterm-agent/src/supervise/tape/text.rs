// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A TAPE AS TEXT (the replay plan's §6.6): one record per line, every body line by line, that reads back to the very same items ([`parse`]).

use astream_effects::EffectRecord;

use super::codec::{self, End, Item, Mark, Stamp};
use super::doors;
use super::header::{escape, unescape};
use crate::supervise::CtlReply;

/// The first line's words.
const MAGIC: &str = "atape-text 1";

/// `header` and `items` as text ([module docs](self)).
#[must_use]
pub fn render(header: Option<&str>, items: &[Item]) -> String {
    let mut out = String::new();
    out.push_str(MAGIC);
    out.push('\n');
    if let Some(header) = header {
        out.push_str("header\n");
        pieces(&mut out, '|', header.as_bytes());
    }
    for item in items {
        item_text(&mut out, item);
    }
    out
}

/// One name as a token: escaped, with no space (a field's end) and no `=`
/// (a stamp's), so no name reads as either.
fn token(name: &str) -> String {
    escape(name.as_bytes())
        .replace(' ', "\\x20")
        .replace('=', "\\x3d")
}

/// A token read back.
fn name(token: &str) -> Result<String, String> {
    String::from_utf8(unescape(token)?).map_err(|_| format!("a name that is not UTF-8: {token:?}"))
}

/// `body`'s pieces, each behind `prefix` ([module docs](self)).
fn pieces(out: &mut String, prefix: char, body: &[u8]) {
    pieces_cut(out, prefix, body, &[]);
}

/// The prefix of a line that goes on with the piece before it, with no
/// newline between: a screen's next row ([module docs](self)).
const GLUE: char = '=';

/// `body`'s pieces, each behind `prefix`, and each piece cut again before
/// every offset in `cuts` (ascending, none at a newline), the rest after a
/// cut behind [`GLUE`].
fn pieces_cut(out: &mut String, prefix: char, body: &[u8], cuts: &[usize]) {
    let line = |out: &mut String, lead: char, piece: &[u8]| {
        out.push(lead);
        if !piece.is_empty() {
            out.push(' ');
            out.push_str(&escape(piece));
        }
        out.push('\n');
    };
    let mut cuts = cuts.iter().copied().peekable();
    let mut start = 0;
    for piece in body.split(|&b| b == b'\n') {
        let end = start + piece.len();
        let mut lead = prefix;
        let mut from = start;
        while let Some(cut) = cuts.next_if(|&c| c <= end) {
            if cut > from {
                line(out, lead, &body[from..cut]);
                lead = GLUE;
                from = cut;
            }
        }
        line(out, lead, &body[from..end]);
        start = end + 1;
    }
}

/// Where a screen read's reply text is cut so that each of its rows is a
/// line of its own ([module docs](self)): before each row of its
/// `"rows":[…]` array and before the `]` that closes it.
fn row_cuts(text: &[u8]) -> Vec<usize> {
    const KEY: &[u8] = b"\"rows\":[";
    let Some(open) = text.windows(KEY.len()).position(|w| w == KEY) else {
        return Vec::new();
    };
    let mut at = open + KEY.len();
    let mut cuts = Vec::new();
    loop {
        if text.get(at) != Some(&b'"') {
            return Vec::new();
        }
        cuts.push(at);
        at += 1;
        loop {
            match text.get(at) {
                Some(b'\\') => at += 2,
                Some(b'"') => break,
                Some(b'\n') | None => return Vec::new(),
                Some(_) => at += 1,
            }
        }
        at += 1;
        match text.get(at) {
            Some(b',') => at += 1,
            Some(b']') => {
                cuts.push(at);
                return cuts;
            }
            _ => return Vec::new(),
        }
    }
}

fn stamp_token(stamp: Option<Stamp>) -> String {
    stamp.map_or_else(String::new, |s| {
        format!(" stamp={},{}", s.mono_ns, s.wall_ms)
    })
}

fn item_text(out: &mut String, item: &Item) {
    match item {
        Item::Effect { record, stamp } => effect_text(out, record, *stamp),
        Item::Note { kind, text } => {
            out.push_str(&format!("note {}\n", token(kind)));
            pieces(out, '|', text.as_bytes());
        }
        Item::Mark(Mark::Nested { inner, outer }) => {
            out.push_str(&format!("mark nested {} {}\n", token(inner), token(outer)));
        }
        Item::Mark(Mark::Truncated { door }) => {
            out.push_str(&format!("mark truncated {door}\n"));
        }
        Item::End(End::Returned { how }) => {
            out.push_str("end returned\n");
            pieces(out, '|', how.as_bytes());
        }
        Item::End(End::Suspended) => out.push_str("end suspended\n"),
        Item::End(End::Unwound { message }) => {
            out.push_str("end unwound\n");
            pieces(out, '|', message.as_bytes());
        }
    }
}

/// Whether `door` is a control lane's, written as words and a reply.
fn is_control(door: &str) -> bool {
    door == doors::CTL || door == doors::TELLER
}

fn effect_text(out: &mut String, record: &EffectRecord, stamp: Option<Stamp>) {
    let st = stamp_token(stamp);
    match record {
        EffectRecord::Clock(ns) => out.push_str(&format!("clock {ns}{st}\n")),
        EffectRecord::Mono(ns) => out.push_str(&format!("mono {ns}{st}\n")),
        EffectRecord::Rand(word) => out.push_str(&format!("rand {word}{st}\n")),
        EffectRecord::File { path, bytes } => {
            out.push_str(&format!("file {}{st}\n", token(path)));
            pieces(out, '|', bytes);
        }
        EffectRecord::FileErr { path, kind } => {
            out.push_str(&format!("file-err {kind:?} {}{st}\n", token(path)));
        }
        EffectRecord::Exchange {
            door,
            request,
            reply,
        } => {
            let control = is_control(door)
                .then(|| {
                    codec::ctl_args(request)
                        .ok()
                        .zip(codec::ctl_reply_of(reply).ok())
                })
                .flatten()
                .filter(|(words, back)| {
                    let words: Vec<&str> = words.iter().map(String::as_str).collect();
                    codec::ctl_request(&words) == *request && codec::ctl_reply(back) == *reply
                });
            match control {
                Some((words, reply)) => {
                    out.push_str(&format!("exchange {}{st}\n", token(door)));
                    for word in words {
                        out.push('>');
                        if !word.is_empty() {
                            out.push(' ');
                            out.push_str(&escape(word.as_bytes()));
                        }
                        out.push('\n');
                    }
                    match reply {
                        Ok(CtlReply {
                            code,
                            stdout,
                            stderr,
                        }) => {
                            out.push_str(&format!("< ok {code}\n"));
                            pieces(out, '!', stderr.as_bytes());
                            let stdout = stdout.as_bytes();
                            pieces_cut(out, '|', stdout, &row_cuts(stdout));
                        }
                        Err(why) => {
                            out.push_str("< err\n");
                            pieces(out, '|', why.as_bytes());
                        }
                    }
                }
                None => {
                    let raw = if is_control(door) { " raw" } else { "" };
                    out.push_str(&format!("exchange {}{raw}{st}\n", token(door)));
                    pieces(out, '>', request);
                    pieces(out, '<', reply);
                }
            }
        }
        EffectRecord::Emit { door, bytes } => {
            out.push_str(&format!("emit {}{st}\n", token(door)));
            pieces(out, '|', bytes);
        }
        EffectRecord::Raised { door, false_reads } => {
            out.push_str(&format!("raised {} {false_reads}{st}\n", token(door)));
        }
    }
}

/// What [`render`] wrote, read back: the header and the items.
/// # Errors
/// Text [`render`] did not write, named with its line.
pub fn parse(text: &str) -> Result<(Option<String>, Vec<Item>), String> {
    let mut lines = Lines {
        lines: text.split('\n').collect(),
        at: 0,
    };
    if lines.lines.last() == Some(&"") {
        lines.lines.pop();
    }
    if lines.next() != Some(MAGIC) {
        return Err(format!(
            "not a tape's text: its first line is not {MAGIC:?}"
        ));
    }
    let mut header = None;
    let mut items = Vec::new();
    while let Some(head) = lines.next() {
        let at = lines.at;
        let words: Vec<&str> = head.split(' ').collect();
        let item = match words.as_slice() {
            ["header"] if header.is_none() && items.is_empty() => {
                header = Some(lines.text('|')?);
                continue;
            }
            ["note", kind] => Item::Note {
                kind: name(kind)?,
                text: lines.text('|')?,
            },
            ["mark", "nested", inner, outer] => Item::Mark(Mark::Nested {
                inner: name(inner)?,
                outer: name(outer)?,
            }),
            ["mark", "truncated", door] => Item::Mark(Mark::Truncated {
                door: number(door, at)?,
            }),
            ["end", "returned"] => Item::End(End::Returned {
                how: lines.text('|')?,
            }),
            ["end", "suspended"] => Item::End(End::Suspended),
            ["end", "unwound"] => Item::End(End::Unwound {
                message: lines.text('|')?,
            }),
            [kind, rest @ ..] => effect(kind, rest, &mut lines, at)?,
            [] => return Err(format!("line {at}: empty")),
        };
        items.push(item);
    }
    Ok((header, items))
}

/// The text's lines, read one by one.
struct Lines<'a> {
    lines: Vec<&'a str>,
    /// The 1-based number of the last line read.
    at: usize,
}

impl<'a> Lines<'a> {
    fn next(&mut self) -> Option<&'a str> {
        let line = self.lines.get(self.at).copied()?;
        self.at += 1;
        Some(line)
    }

    /// The next line, if it is a body line behind `prefix`: its piece.
    fn piece(&mut self, prefix: char) -> Result<Option<Vec<u8>>, String> {
        let Some(line) = self.lines.get(self.at).copied() else {
            return Ok(None);
        };
        let Some(rest) = line.strip_prefix(prefix) else {
            return Ok(None);
        };
        self.at += 1;
        if rest.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let piece = rest
            .strip_prefix(' ')
            .ok_or_else(|| format!("line {}: no space after {prefix:?}", self.at))?;
        unescape(piece)
            .map(Some)
            .map_err(|e| format!("line {}: {e}", self.at))
    }

    /// A body behind `prefix`: its pieces joined by newlines, a [`GLUE`]
    /// line's joined to the one before it with none.
    fn bytes(&mut self, prefix: char) -> Result<Vec<u8>, String> {
        let mut body = self
            .piece(prefix)?
            .ok_or_else(|| format!("line {}: a body behind {prefix:?} is missing", self.at + 1))?;
        loop {
            if let Some(piece) = self.piece(prefix)? {
                body.push(b'\n');
                body.extend(piece);
            } else if let Some(piece) = self.piece(GLUE)? {
                body.extend(piece);
            } else {
                return Ok(body);
            }
        }
    }

    fn text(&mut self, prefix: char) -> Result<String, String> {
        let at = self.at + 1;
        String::from_utf8(self.bytes(prefix)?)
            .map_err(|_| format!("line {at}: a text that is not UTF-8"))
    }
}

fn number<T: std::str::FromStr>(word: &str, at: usize) -> Result<T, String> {
    word.parse()
        .map_err(|_| format!("line {at}: {word:?} is no number"))
}

/// The head's fields and its stamp, if its last token is one.
fn split_stamp<'w>(
    fields: &'w [&'w str],
    at: usize,
) -> Result<(&'w [&'w str], Option<Stamp>), String> {
    match fields.split_last() {
        Some((last, rest)) if last.starts_with("stamp=") => {
            let (mono, wall) = last["stamp=".len()..]
                .split_once(',')
                .ok_or_else(|| format!("line {at}: a stamp without its wall clock"))?;
            Ok((
                rest,
                Some(Stamp {
                    mono_ns: number(mono, at)?,
                    wall_ms: number(wall, at)?,
                }),
            ))
        }
        _ => Ok((fields, None)),
    }
}

fn effect(kind: &str, fields: &[&str], lines: &mut Lines<'_>, at: usize) -> Result<Item, String> {
    let (fields, stamp) = split_stamp(fields, at)?;
    let control_door = match (kind, fields) {
        ("exchange", [door]) => Some(name(door)?).filter(|d| is_control(d)),
        _ => None,
    };
    if let Some(door) = control_door {
        return Ok(Item::Effect {
            record: control(door, lines, at)?,
            stamp,
        });
    }
    let record = match (kind, fields) {
        ("clock", [ns]) => EffectRecord::Clock(number(ns, at)?),
        ("mono", [ns]) => EffectRecord::Mono(number(ns, at)?),
        ("rand", [word]) => EffectRecord::Rand(number(word, at)?),
        ("file", [path]) => EffectRecord::File {
            path: name(path)?,
            bytes: lines.bytes('|')?,
        },
        ("file-err", [kind, path]) => EffectRecord::FileErr {
            path: name(path)?,
            kind: codec::KINDS
                .iter()
                .map(|(_, k)| *k)
                .find(|k| format!("{k:?}") == *kind)
                .ok_or_else(|| format!("line {at}: no error kind {kind:?}"))?,
        },
        ("exchange", [door] | [door, "raw"]) => EffectRecord::Exchange {
            door: name(door)?,
            request: lines.bytes('>')?,
            reply: lines.bytes('<')?,
        },
        ("emit", [door]) => EffectRecord::Emit {
            door: name(door)?,
            bytes: lines.bytes('|')?,
        },
        ("raised", [door, false_reads]) => EffectRecord::Raised {
            door: name(door)?,
            false_reads: number(false_reads, at)?,
        },
        _ => {
            return Err(format!(
                "line {at}: no item this build writes: {kind:?} {fields:?}"
            ));
        }
    };
    Ok(Item::Effect { record, stamp })
}

/// A control round trip written as its words and reply.
fn control(door: String, lines: &mut Lines<'_>, at: usize) -> Result<EffectRecord, String> {
    let mut words = Vec::new();
    while let Some(word) = lines.piece('>')? {
        words.push(
            String::from_utf8(word).map_err(|_| format!("line {at}: a word that is not UTF-8"))?,
        );
    }
    let head = lines
        .next()
        .ok_or_else(|| format!("line {at}: a control round trip without its reply"))?;
    let reply = match head.split(' ').collect::<Vec<_>>().as_slice() {
        ["<", "ok", code] => {
            let code = number(code, lines.at)?;
            let stderr = lines.text('!')?;
            let stdout = lines.text('|')?;
            Ok(CtlReply {
                code,
                stdout,
                stderr,
            })
        }
        ["<", "err"] => Err(lines.text('|')?),
        _ => return Err(format!("line {}: not a control reply: {head:?}", lines.at)),
    };
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    Ok(EffectRecord::Exchange {
        door,
        request: codec::ctl_request(&words),
        reply: codec::ctl_reply(&reply),
    })
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
