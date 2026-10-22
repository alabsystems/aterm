// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The asciicast v2 codec: the recording model ([`CastHeader`], [`CastEvent`]),
//! its writer ([`write_into`]) and its reader ([`parse`]).
//!
//! Both directions live here, so a header key or a timeline rule is spelled
//! once. The server lifts its recorder into these values once (rebased), writes
//! them for `cast` and audits the same values for `cast drift`; the client path
//! reads a fetched or saved recording back into them. What the writer is given
//! the reader returns, at the format's resolution (six decimals of a second)
//! and for headers a producer writes (the `aterm_dropped` rule on [`write_into`]).
//!
//! The text is what `asciinema play` and `agg` accept: a JSON header object on
//! the first line, then one `[t, "o", "<data>"]` line per output burst and one
//! `[t, "r", "<cols>x<rows>"]` line per resize. Players ignore aterm's own
//! header keys (`aterm_resize_undo`, `aterm_truncated`, `aterm_dropped`,
//! `aterm_handoff`).

use aterm_core::grid::ResizeLaw;

use crate::wire::json_escape_into;

/// One recorded event. `D` is the output burst's storage: the server passes its
/// recorder's shared `Arc<[u8]>`s; the reader owns `Vec<u8>`s.
#[derive(Debug, Clone, PartialEq)]
pub enum CastEvent<D> {
    /// `[t, "o", data]`: program output. `t` is seconds on the cast's own
    /// timeline, exactly as `cast` prints it.
    Output {
        /// Seconds since the (possibly rebased) recording start.
        t: f64,
        /// The burst's bytes.
        data: D,
    },
    /// `[t, "r", "<cols>x<rows>"]`: a geometry change the engine applied.
    Resize {
        /// Seconds since the (possibly rebased) recording start.
        t: f64,
        /// New width.
        cols: u16,
        /// New height.
        rows: u16,
    },
}

impl<D> CastEvent<D> {
    /// The event's timestamp.
    #[must_use]
    #[inline]
    pub const fn t(&self) -> f64 {
        match self {
            CastEvent::Output { t, .. } | CastEvent::Resize { t, .. } => *t,
        }
    }
}

/// Bursts the reader could not hand to the recorder (a full writer queue), as
/// the header's `aterm_dropped` object discloses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CastDrops {
    /// Bursts dropped.
    pub bursts: u64,
    /// Bytes in them.
    pub bytes: u64,
}

/// The header's `aterm_handoff` object: the recording starts at a seamless
/// update, and the adopted session's output and screen before it are not in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandoffHeader {
    /// The build number of the process that handed the session across, when
    /// it was known (`from_build`, `null` when not).
    pub from_build: Option<u64>,
}

/// The asciicast header's facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastHeader {
    /// Width (cols) in force at the first event: the recording's start, or the
    /// size after the last resize a drop-oldest eviction removed.
    pub width: u16,
    /// Height (rows), kept with [`width`](Self::width).
    pub height: u16,
    /// Head events drop-oldest evicted (`aterm_truncated.evicted_events`): the
    /// leading engine state is incomplete when this is non-zero, and the
    /// producer rebased every `t` to the first retained event.
    pub evicted: u64,
    /// `Some` when the producer counts drops (0 included); `None` for a
    /// recording whose producer does not say: a file from an older aterm, or
    /// from another recorder.
    pub dropped: Option<CastDrops>,
    /// `Some(true)` when the recording says its engine kept the resize undo
    /// (`aterm_resize_undo`: every aterm since the undo writes it); `None` for a
    /// recording that does not say, a file from an aterm before it. `cast drift`
    /// replays a recording that carries the key under that law only
    /// ([`Undo::Match`](crate::cast_drift::Undo::Match) never second-guesses it),
    /// so an undo regression in the recording engine still reads `unfaithful`.
    pub resize_undo: Option<bool>,
    /// The resize laws the recording's engine may have laid its rows out
    /// under, the newest first ([`CastHeader::laws_named`] of its
    /// `aterm_resize_undo`): each replay runs under one, the next only when
    /// the ones before it do not reproduce the live screen. 3 names the
    /// current law (a width change lays the rows out where the rows hand-back
    /// puts them); 2 the rows hand-back, as two aterms wrote it (before and
    /// after the pushed rows were owed back); 1 and a recording that does not
    /// say, the law before it (what is owed forgotten before each resize).
    pub resize_laws: &'static [ResizeLaw],
    /// `Some` when the recording starts at a seamless update
    /// (`aterm_handoff`).
    pub handoff: Option<HandoffHeader>,
}

impl CastHeader {
    /// The laws a recording whose header carries `aterm_resize_undo: value`
    /// (`None`: no key) was made under, the newest first: the law before the
    /// rows hand-back for no key and 0, and the newest law this aterm knows for
    /// a value only a newer one writes.
    #[must_use]
    #[inline]
    pub const fn laws_named(value: Option<u64>) -> &'static [ResizeLaw] {
        match value {
            None | Some(0) => &[ResizeLaw::Undo],
            Some(value) => match ResizeLaw::from_header(value) {
                [] => &[ResizeLaw::Rewrap],
                laws => laws,
            },
        }
    }
}

/// A parsed asciicast v2 recording.
#[derive(Debug, Clone, PartialEq)]
pub struct Asciicast {
    /// The header line's facts.
    pub header: CastHeader,
    /// The `o` and `r` events, in file order (other kinds are skipped).
    pub events: Vec<CastEvent<Vec<u8>>>,
}

impl Asciicast {
    /// The recording cut at `until` seconds (later events dropped), to hold a
    /// saved cast against a screen captured at that instant.
    #[must_use]
    pub fn until(mut self, until: f64) -> Self {
        self.events.retain(|e| e.t() <= until);
        self
    }
}

// The writer

/// `aterm_truncated`'s note.
const TRUNCATED_NOTE: &str = "drop-oldest evicted the head; leading ANSI state incomplete; \
                              timestamps rebased to the first retained event";

/// `aterm_handoff`'s note.
const HANDOFF_NOTE: &str = "recording restarted at a seamless update; the output and screen \
                            before it are not in this recording";

/// Append `header` and `events` as asciicast v2 text: the header line, then one
/// newline-terminated line per event with `t` printed to six decimals. `out`
/// grows once, by an upper bound on the text before escaping expands a
/// payload, so a reply built here is one allocation.
///
/// The header is `{"version": 2, "width": W, "height": H`, then, in this order:
/// `"aterm_resize_undo"` when the header says: 0 without the undo, else the
/// value of the newest law [`CastHeader::resize_laws`] names
/// ([`ResizeLaw::header_value`]; 1 when it names none); `aterm_truncated` when
/// `evicted > 0`; `aterm_dropped` when a burst was dropped (a count of none is
/// omitted, and [`parse`] reads that silence back as zero from a header that
/// carries the resize-law key, which every producer that counts drops writes);
/// and `aterm_handoff` when the recording starts at a seamless update.
///
/// A payload is JSON-escaped: each maximal valid UTF-8 run through
/// [`json_escape_into`], and each invalid subpart as one U+FFFD, as
/// `String::from_utf8_lossy` decodes them. A producer that splits a character
/// across bursts carries the incomplete lead to the next burst itself.
pub fn write_into<D: AsRef<[u8]>>(out: &mut String, header: &CastHeader, events: &[CastEvent<D>]) {
    use std::fmt::Write as _;
    out.reserve(len_hint(events));
    let _ = write!(
        out,
        "{{\"version\": 2, \"width\": {}, \"height\": {}",
        header.width, header.height
    );
    if let Some(undo) = header.resize_undo {
        let law = if undo {
            header
                .resize_laws
                .first()
                .copied()
                .map_or(1, ResizeLaw::header_value)
        } else {
            0
        };
        let _ = write!(out, ", \"aterm_resize_undo\": {law}");
    }
    if header.evicted > 0 {
        let _ = write!(
            out,
            ", \"aterm_truncated\": {{\"evicted_events\": {}, \"note\": \"{TRUNCATED_NOTE}\"}}",
            header.evicted
        );
    }
    if let Some(drops) = header.dropped.filter(|d| d.bursts > 0) {
        let _ = write!(
            out,
            ", \"aterm_dropped\": {{\"bursts\": {}, \"bytes\": {}}}",
            drops.bursts, drops.bytes
        );
    }
    if let Some(handoff) = header.handoff {
        out.push_str(", \"aterm_handoff\": {\"carried\": 0, \"from_build\": ");
        match handoff.from_build {
            Some(build) => {
                let _ = write!(out, "{build}");
            }
            None => out.push_str("null"),
        }
        let _ = write!(out, ", \"note\": \"{HANDOFF_NOTE}\"}}");
    }
    out.push_str("}\n");
    for ev in events {
        match ev {
            CastEvent::Output { t, data } => {
                let _ = write!(out, "[{t:.6}, \"o\", \"");
                escape_bytes_into(out, data.as_ref());
                out.push_str("\"]\n");
            }
            CastEvent::Resize { t, cols, rows } => {
                let _ = writeln!(out, "[{t:.6}, \"r\", \"{cols}x{rows}\"]");
            }
        }
    }
}

/// An upper bound on [`write_into`]'s text before escaping expands a payload:
/// the header with every key fits in 512 bytes, and an event's framing (the
/// brackets, kind, quotes and a `t` under 10^9 s) in 40.
fn len_hint<D: AsRef<[u8]>>(events: &[CastEvent<D>]) -> usize {
    events.iter().fold(512, |n, ev| {
        let payload = match ev {
            CastEvent::Output { data, .. } => data.as_ref().len(),
            CastEvent::Resize { .. } => 0,
        };
        n.saturating_add(40).saturating_add(payload)
    })
}

/// JSON-escape raw bytes into `out` as a string body: each maximal valid UTF-8
/// run through [`json_escape_into`], each invalid subpart as one U+FFFD.
fn escape_bytes_into(out: &mut String, bytes: &[u8]) {
    for chunk in bytes.utf8_chunks() {
        json_escape_into(out, chunk.valid());
        if !chunk.invalid().is_empty() {
            out.push(char::REPLACEMENT_CHARACTER);
        }
    }
}

// The reader

/// Parse the text `cast` answers (and `asciinema rec` writes): a JSON header
/// object on the first non-blank line, then one `[t, "<kind>", "<data>"]` array
/// per line. `o` and `r` events are kept; any other kind (`i`, `m`) is skipped.
/// The header's aterm keys are read when present ([`CastHeader`]). Errors name
/// the 1-based line.
pub fn parse(text: &str) -> Result<Asciicast, String> {
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty());
    let (hn, hl) = lines
        .next()
        .ok_or_else(|| "empty recording: no header line".to_string())?;
    let header = parse_header(hl).map_err(|e| format!("line {}: {e}", hn + 1))?;
    let mut events = Vec::new();
    for (n, line) in lines {
        if let Some(ev) = parse_event(line).map_err(|e| format!("line {}: {e}", n + 1))? {
            events.push(ev);
        }
    }
    Ok(Asciicast { header, events })
}

fn parse_header(line: &str) -> Result<CastHeader, String> {
    let Json::Obj(fields) = parse_json(line)? else {
        return Err("the header is not a JSON object".to_string());
    };
    let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    match get("version").and_then(Json::as_u64) {
        Some(2) => {}
        _ => return Err("not an asciicast v2 header (version != 2)".to_string()),
    }
    let dim = |key: &str| -> Result<u16, String> {
        get(key)
            .and_then(Json::as_u64)
            .map(|v| v.clamp(1, u64::from(u16::MAX)) as u16)
            .ok_or_else(|| format!("the header has no integer `{key}`"))
    };
    let evicted = get("aterm_truncated")
        .and_then(|t| t.get("evicted_events"))
        .and_then(Json::as_u64)
        .unwrap_or(0);
    let law = get("aterm_resize_undo").and_then(Json::as_u64);
    let resize_undo = law.map(|v| v != 0);
    // aterm counted the reader's drops a day before it wrote the resize-law
    // key, and writes `aterm_dropped` only when there was one: under the key,
    // its absence is a count of zero.
    let dropped = match get("aterm_dropped") {
        Some(d) => Some(CastDrops {
            bursts: d.get("bursts").and_then(Json::as_u64).unwrap_or(0),
            bytes: d.get("bytes").and_then(Json::as_u64).unwrap_or(0),
        }),
        None => resize_undo.map(|_| CastDrops::default()),
    };
    let handoff = get("aterm_handoff").map(|h| HandoffHeader {
        from_build: h.get("from_build").and_then(Json::as_u64),
    });
    Ok(CastHeader {
        width: dim("width")?,
        height: dim("height")?,
        evicted,
        dropped,
        resize_undo,
        resize_laws: CastHeader::laws_named(law),
        handoff,
    })
}

fn parse_event(line: &str) -> Result<Option<CastEvent<Vec<u8>>>, String> {
    let Json::Arr(items) = parse_json(line)? else {
        return Err("an event is not a JSON array".to_string());
    };
    let [t, kind, data] = items.as_slice() else {
        return Err("an event is not a [time, kind, data] triple".to_string());
    };
    let t = t
        .as_f64()
        .filter(|t| t.is_finite() && *t >= 0.0)
        .ok_or_else(|| "an event time is not a non-negative number".to_string())?;
    let (Json::Str(kind), Json::Str(data)) = (kind, data) else {
        return Err("an event kind or data is not a string".to_string());
    };
    match kind.as_str() {
        "o" => Ok(Some(CastEvent::Output {
            t,
            data: data.clone().into_bytes(),
        })),
        "r" => {
            let (cols, rows) = data
                .split_once('x')
                .and_then(|(c, r)| Some((c.parse::<u16>().ok()?, r.parse::<u16>().ok()?)))
                .ok_or_else(|| format!("a resize is not <cols>x<rows>: {data:?}"))?;
            Ok(Some(CastEvent::Resize { t, cols, rows }))
        }
        _ => Ok(None),
    }
}

/// The JSON the reader needs: a value tree with numbers kept as their literal
/// (so a timestamp is parsed once, exactly).
#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool,
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(n) => n.parse::<u64>().ok(),
            _ => None,
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => n.parse::<f64>().ok(),
            _ => None,
        }
    }

    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// Nesting deeper than this is refused: the reader recurses per level, and a
/// hostile file must not be able to overflow the stack.
const MAX_JSON_DEPTH: usize = 32;

fn parse_json(text: &str) -> Result<Json, String> {
    let mut p = JsonReader {
        s: text.as_bytes(),
        i: 0,
    };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err("trailing bytes after the JSON value".to_string());
    }
    Ok(v)
}

struct JsonReader<'a> {
    s: &'a [u8],
    i: usize,
}

impl JsonReader<'_> {
    fn ws(&mut self) {
        while self
            .s
            .get(self.i)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        {
            self.i += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&b) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_JSON_DEPTH {
            return Err("JSON nested too deep".to_string());
        }
        self.ws();
        match self.s.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut fields = Vec::new();
                if self.eat(b'}') {
                    return Ok(Json::Obj(fields));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    if !self.eat(b':') {
                        return Err("expected `:` in a JSON object".to_string());
                    }
                    let v = self.value(depth + 1)?;
                    fields.push((key, v));
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b'}') {
                        return Ok(Json::Obj(fields));
                    }
                    return Err("expected `,` or `}` in a JSON object".to_string());
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                if self.eat(b']') {
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b']') {
                        return Ok(Json::Arr(items));
                    }
                    return Err("expected `,` or `]` in a JSON array".to_string());
                }
            }
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.word(b"true", Json::Bool),
            Some(b'f') => self.word(b"false", Json::Bool),
            Some(b'n') => self.word(b"null", Json::Null),
            Some(b'-' | b'0'..=b'9') => {
                let start = self.i;
                while self
                    .s
                    .get(self.i)
                    .is_some_and(|b| matches!(b, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
                {
                    self.i += 1;
                }
                // The span is ASCII by the loop's own filter.
                let lit = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
                if lit.parse::<f64>().is_err() {
                    return Err(format!("bad JSON number {lit:?}"));
                }
                Ok(Json::Num(lit))
            }
            Some(_) => Err("unexpected byte in JSON".to_string()),
            None => Err("unexpected end of JSON".to_string()),
        }
    }

    fn word(&mut self, w: &[u8], v: Json) -> Result<Json, String> {
        if self.s[self.i..].starts_with(w) {
            self.i += w.len();
            Ok(v)
        } else {
            Err("bad JSON literal".to_string())
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .s
            .get(self.i..self.i + 4)
            .ok_or_else(|| "truncated \\u escape".to_string())?;
        let text = std::str::from_utf8(digits).map_err(|_| "bad \\u escape".to_string())?;
        let v = u32::from_str_radix(text, 16).map_err(|_| "bad \\u escape".to_string())?;
        self.i += 4;
        Ok(v)
    }

    /// A JSON string at the cursor (on its `"`). Escapes decode, `\uXXXX`
    /// included; a surrogate pair joins into one scalar and a lone surrogate
    /// becomes U+FFFD.
    fn string(&mut self) -> Result<String, String> {
        if self.s.get(self.i) != Some(&b'"') {
            return Err("expected a JSON string".to_string());
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let start = self.i;
            while self
                .s
                .get(self.i)
                .is_some_and(|b| *b != b'"' && *b != b'\\')
            {
                self.i += 1;
            }
            out.extend_from_slice(&self.s[start..self.i]);
            match self.s.get(self.i) {
                None => return Err("unterminated JSON string".to_string()),
                Some(b'"') => {
                    self.i += 1;
                    // The source is a `&str` and every escape pushes a whole
                    // scalar, so this is valid UTF-8; lossy is the total form.
                    return Ok(String::from_utf8_lossy(&out).into_owned());
                }
                Some(_) => {
                    self.i += 1;
                    let esc = *self
                        .s
                        .get(self.i)
                        .ok_or_else(|| "unterminated JSON escape".to_string())?;
                    self.i += 1;
                    let ch = match esc {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{08}',
                        b'f' => '\u{0C}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode_escape()?,
                        _ => return Err("bad JSON escape".to_string()),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
            }
        }
    }

    /// The scalar a `\uXXXX` (cursor just past the `u`) names, joining a
    /// high surrogate with the `\uXXXX` low surrogate that follows it.
    fn unicode_escape(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        if (0xD800..0xDC00).contains(&hi) {
            if self.s[self.i..].starts_with(b"\\u") {
                let save = self.i;
                self.i += 2;
                let lo = self.hex4()?;
                if (0xDC00..0xE000).contains(&lo) {
                    let scalar = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                    return Ok(char::from_u32(scalar).unwrap_or('\u{FFFD}'));
                }
                // Not a low surrogate: the high one stands alone, and the
                // escape after it is read on its own.
                self.i = save;
            }
            return Ok('\u{FFFD}');
        }
        Ok(char::from_u32(hi).unwrap_or('\u{FFFD}'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A seeded xorshift stream.
    fn stream(seed: u64) -> impl FnMut() -> u64 {
        let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        }
    }

    /// Payload pieces from every class the writer escapes: quotes, backslash and
    /// slash; C0 in its short and `\u` forms; DEL and C1; bidi controls and line
    /// separators; multibyte text.
    const PIECES: [&str; 6] = [
        "plain text ",
        "\"q\\/",
        "\x1b[31m\x07\x00\x08\x0c\r\n\t",
        "\u{7f}\u{85}\u{9b}",
        "\u{200e}\u{2066}x\u{2069}\u{2028}\u{2029}",
        "é漢🙂",
    ];

    /// A header with every field drawn from the headers producers write: one
    /// that carries the resize-law key counts drops, none included; one without
    /// it says nothing of drops, or discloses some; its laws are the ones its
    /// key names.
    fn seeded_header(next: &mut impl FnMut() -> u64) -> CastHeader {
        let law = match next() % 5 {
            0 => None,
            1 => Some(0),
            v => Some(v - 1),
        };
        let resize_undo = law.map(|v| v != 0);
        let bursts = match next() % 2 {
            0 => 0,
            _ => 1 + next() % 10_000,
        };
        let drops = CastDrops {
            bursts,
            bytes: if bursts == 0 {
                0
            } else {
                bursts + next() % (1 << 40)
            },
        };
        let dropped = match resize_undo {
            Some(_) => Some(drops),
            None => (drops.bursts > 0 && next().is_multiple_of(2)).then_some(drops),
        };
        let handoff = match next() % 3 {
            0 => None,
            1 => Some(HandoffHeader { from_build: None }),
            _ => Some(HandoffHeader {
                from_build: Some(next()),
            }),
        };
        CastHeader {
            width: 1 + (next() % u64::from(u16::MAX)) as u16,
            height: 1 + (next() % u64::from(u16::MAX)) as u16,
            evicted: match next() % 2 {
                0 => 0,
                _ => 1 + next() % 1_000_000,
            },
            dropped,
            resize_undo,
            resize_laws: CastHeader::laws_named(law),
            handoff,
        }
    }

    /// Events on microsecond timestamps (the format prints six decimals), from
    /// an offset up to 10^6 s, with payloads built from [`PIECES`].
    fn seeded_events(next: &mut impl FnMut() -> u64, n: usize) -> Vec<CastEvent<Vec<u8>>> {
        let mut micros = next() % 1_000_000_000_000;
        (0..n)
            .map(|_| {
                micros += next() % 2_000_000;
                let t = micros as f64 / 1e6;
                match next() % 5 {
                    0 => CastEvent::Resize {
                        t,
                        cols: next() as u16,
                        rows: next() as u16,
                    },
                    _ => CastEvent::Output {
                        t,
                        data: (0..next() % 6)
                            .map(|_| PIECES[(next() % 6) as usize])
                            .collect::<String>()
                            .into_bytes(),
                    },
                }
            })
            .collect()
    }

    fn written<D: AsRef<[u8]>>(header: &CastHeader, events: &[CastEvent<D>]) -> String {
        let mut out = String::new();
        write_into(&mut out, header, events);
        out
    }

    /// What the writer is given the reader returns: every header field, the
    /// handoff included, and every event, for headers producers write and
    /// timestamps at the format's resolution; the seeds draw every form of
    /// every field. Invalid payload bytes come back as `from_utf8_lossy` reads
    /// them. Negative control: outside that domain the omissions show, a count
    /// of no drops without the resize-law key reads back as unsaid and the key
    /// without a count as none dropped.
    #[test]
    fn write_then_parse_round_trips() {
        let mut forms = std::collections::BTreeSet::new();
        for seed in 0..64 {
            let mut next = stream(seed);
            let header = seeded_header(&mut next);
            let events = seeded_events(&mut next, 64);
            forms.insert(format!("undo {:?}", header.resize_undo));
            forms.insert(format!("laws {:?}", header.resize_laws));
            forms.insert(format!(
                "dropped {:?}",
                header.dropped.map(|d| d.bursts > 0)
            ));
            forms.insert(format!(
                "handoff {:?}",
                header.handoff.map(|h| h.from_build.is_some())
            ));
            forms.insert(format!("evicted {}", header.evicted > 0));
            let text = written(&header, &events);
            assert_eq!(
                parse(&text),
                Ok(Asciicast { header, events }),
                "seed {seed}:\n{text}"
            );
        }
        assert_eq!(
            forms.len(),
            3 + 3 + 3 + 3 + 2,
            "every form drawn: {forms:?}"
        );

        let header = CastHeader {
            width: 80,
            height: 24,
            evicted: 0,
            dropped: Some(CastDrops::default()),
            resize_undo: Some(true),
            resize_laws: CastHeader::laws_named(Some(ResizeLaw::default().header_value())),
            handoff: None,
        };
        let raw = [b'A', 0x80, 0xff, b'B', 0xc3];
        let lossy = parse(&written(
            &header,
            &[CastEvent::Output { t: 0.5, data: raw }],
        ))
        .expect("parses");
        assert_eq!(
            lossy.events,
            [CastEvent::Output {
                t: 0.5,
                data: String::from_utf8_lossy(&raw).into_owned().into_bytes(),
            }]
        );

        let none: [CastEvent<Vec<u8>>; 0] = [];
        let unsaid = CastHeader {
            resize_undo: None,
            ..header.clone()
        };
        assert_eq!(
            parse(&written(&unsaid, &none)).unwrap().header.dropped,
            None
        );
        let uncounted = CastHeader {
            dropped: None,
            ..header
        };
        assert_eq!(
            parse(&written(&uncounted, &none)).unwrap().header.dropped,
            Some(CastDrops::default())
        );
    }

    /// The header key: `3` reads the undo and the current law, `2` the undo and
    /// the two laws aterms wrote it under (the newest first), `1` the undo
    /// only, `0` `Some(false)`, absent `None` (a file from an aterm before the
    /// undo), the last three under the law before the rows hand-back, a value
    /// only a newer aterm writes under the newest law this one knows, and the
    /// rest of the header is read as ever. A header with the key and no
    /// `aterm_dropped` counted no drop.
    #[test]
    fn the_resize_law_key_is_read_from_the_header() {
        let read = |header: &str| {
            parse(&format!("{header}\n[0.1, \"o\", \"hi\"]\n"))
                .expect("parses")
                .header
        };
        let keyed = |value: u64| {
            read(&format!(
                "{{\"version\": 2, \"width\": 80, \"height\": 24, \"aterm_resize_undo\": {value}}}"
            ))
        };
        let current = keyed(3);
        assert_eq!(
            (current.resize_undo, current.resize_laws),
            (Some(true), &[ResizeLaw::Rewrap][..])
        );
        assert_eq!(current.resize_laws, &[ResizeLaw::default()]);
        let handback = keyed(2);
        assert_eq!(
            (handback.resize_undo, handback.resize_laws),
            (
                Some(true),
                &[ResizeLaw::PushedRows, ResizeLaw::HandBack][..]
            )
        );
        let undo = keyed(1);
        assert_eq!(
            (undo.resize_undo, undo.resize_laws),
            (Some(true), &[ResizeLaw::Undo][..])
        );
        assert_eq!((undo.width, undo.height), (80, 24));
        assert_eq!(undo.dropped, Some(CastDrops::default()), "counted, none");
        let off = keyed(0);
        assert_eq!(
            (off.resize_undo, off.resize_laws),
            (Some(false), &[ResizeLaw::Undo][..])
        );
        let plain = read("{\"version\": 2, \"width\": 80, \"height\": 24}");
        assert_eq!(
            (plain.resize_undo, plain.resize_laws),
            (None, &[ResizeLaw::Undo][..])
        );
        assert_eq!(plain.dropped, None, "a producer that does not say");
        let newer = keyed(9);
        assert_eq!(newer.resize_laws, &[ResizeLaw::Rewrap]);
    }

    #[test]
    fn parser_reads_events_with_every_escape() {
        let text = concat!(
            "{\"version\": 2, \"width\": 135, \"height\": 63, \"env\": {\"TERM\": \"xterm\"}}\n",
            "[0.150484, \"o\", \"\\u001b[16t\\\"q\\\\ \\/ \\b\\f\\n\\r\\t\"]\n",
            "\n",
            "[1.655311, \"r\", \"135x64\"]\n",
            "[1.7, \"i\", \"typed\"]\n",
            "[1.8, \"o\", \"\\ud83d\\ude00 \\u00e9 ✻\"]\n",
            "[1.9, \"o\", \"lone \\ud83d x \\ude00 y\"]\n",
            "[2.0, \"o\", \"hi then \\ud83d\\u0041\"]\n",
        );
        let cast = parse(text).expect("parses");
        assert_eq!(
            cast.header,
            CastHeader {
                width: 135,
                height: 63,
                evicted: 0,
                dropped: None,
                resize_undo: None,
                resize_laws: &[ResizeLaw::Undo],
                handoff: None,
            }
        );
        assert_eq!(cast.events.len(), 5, "the `i` event is skipped");
        assert_eq!(
            cast.events[0],
            CastEvent::Output {
                t: 0.150484,
                data: b"\x1b[16t\"q\\ / \x08\x0c\n\r\t".to_vec(),
            }
        );
        assert_eq!(
            cast.events[1],
            CastEvent::Resize {
                t: 1.655311,
                cols: 135,
                rows: 64,
            }
        );
        let text_of = |i: usize| match &cast.events[i] {
            CastEvent::Output { data, .. } => String::from_utf8(data.clone()).unwrap(),
            CastEvent::Resize { .. } => panic!("not output"),
        };
        assert_eq!(text_of(2), "😀 é ✻", "a surrogate pair joins");
        assert_eq!(text_of(3), "lone \u{FFFD} x \u{FFFD} y", "lone surrogates");
        assert_eq!(
            text_of(4),
            "hi then \u{FFFD}A",
            "a high surrogate before a BMP escape"
        );
        assert_eq!(
            format!("{:.6}", cast.events[1].t()),
            "1.655311",
            "the time round-trips"
        );
    }

    /// The truncation, drop and handoff disclosures are read from the header,
    /// whatever else their objects hold.
    #[test]
    fn parser_reads_the_aterm_disclosures() {
        let text = concat!(
            "{\"version\": 2, \"width\": 80, \"height\": 24, \"aterm_truncated\": ",
            "{\"evicted_events\": 17, \"note\": \"drop-oldest evicted the head\"}, ",
            "\"aterm_dropped\": {\"bursts\": 3, \"bytes\": 4096}, ",
            "\"aterm_handoff\": {\"carried\": 0, \"from_build\": 7, \"note\": \"n\"}}\n",
            "[0.000000, \"o\", \"x\"]\n",
        );
        let cast = parse(text).expect("parses");
        assert_eq!(cast.header.evicted, 17);
        assert_eq!(
            cast.header.dropped,
            Some(CastDrops {
                bursts: 3,
                bytes: 4096,
            })
        );
        assert_eq!(
            cast.header.handoff,
            Some(HandoffHeader {
                from_build: Some(7)
            })
        );
        assert_eq!(cast.clone().until(0.0).events.len(), 1);
        assert_eq!(cast.until(-1.0).events.len(), 0);
        let unknown = parse(
            "{\"version\": 2, \"width\": 80, \"height\": 24, \"aterm_handoff\": \
             {\"carried\": 0, \"from_build\": null}}\n",
        )
        .expect("parses");
        assert_eq!(
            unknown.header.handoff,
            Some(HandoffHeader { from_build: None })
        );
    }

    #[test]
    fn parser_names_the_bad_line() {
        for (text, want) in [
            ("", "no header"),
            (
                "{\"version\": 1, \"width\": 80, \"height\": 24}\n",
                "line 1",
            ),
            ("{\"version\": 2, \"height\": 24}\n", "`width`"),
            (
                "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.1, \"o\", \"x\"]\n[0.2, \"r\", \"80by24\"]\n",
                "line 3",
            ),
            (
                "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.1, \"o\", \"\\q\"]\n",
                "line 2",
            ),
            (
                "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.1, \"o\", \"open\n",
                "line 2",
            ),
        ] {
            let err = parse(text).expect_err(text);
            assert!(err.contains(want), "{err} should name {want}");
        }
        let deep = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert!(parse_json(&deep).is_err(), "nesting is bounded");
    }
}
