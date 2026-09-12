// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `glance.json` — the fleet roster on disk, for a reader that has no broker
//! connection and must not grow one (§9.3, A8).
//!
//! ```text
//! aterm-link glance --fleet <F> --broker <ep> [--tcp --key-file P] --cap-file <p>… [--state DIR]
//! ```
//!
//! The file exists so that the macOS menu bar CAN show the same rows
//! `aterm-link ls` prints without growing a broker connection: a menu bar is a
//! UI thread, and it may not attach a capability, complete a sealed handshake,
//! or block on a `Last`. So one process that already holds the fleet's caps does
//! the round trip and leaves the answer in `<state>/fabric/glance.json`, and a
//! reader does a `read` and a parse.
//!
//! **NO SUCH READER IS BUILT.** §9.3 designs one — a `FabricGlance` folded in
//! beside `status_item.rs`'s existing `FleetGlance` — and A8 owns neither
//! `crates/aterm-gui/src/status_item.rs` nor any other UI file: that file today
//! has no `FabricGlance`, no mention of `glance.json`, and no read of any fabric
//! state. What ships in this module is the WRITER and the format; nothing
//! renders it yet. Said here rather than left to be inferred, because aterm has
//! no evidence manifest and this doc IS the claim.
//!
//! ## The one property this file has to keep
//!
//! **A row IS the `Last{/f/<F>/pub/*/*/presence}` answer, not a summary of it.**
//! Every `key=value` token of the presence body is carried through verbatim,
//! under its own name, together with the subject it came from and the offset it
//! was read at. Nothing is renamed, nothing is computed, and a token this build
//! has never heard of survives to the reader — because the alternative is a
//! menu bar that disagrees with `ls` about the same fleet and no way to tell
//! which one is lying. [`Row::fields`] is therefore a map, not a struct.
//!
//! **WITH EXACTLY FOUR EXCEPTIONS, AND THEY ARE THE CAP-FORCED ONES.** The four
//! names in [`RESERVED`] — `subject`, `offset`, `node` and `owner` — are the row
//! keys this file writes from the SUBJECT the broker handed back, and a body
//! token of one of those names is DROPPED rather than carried ([`fields_of`],
//! and again at the splice in [`Glance::to_json`]). A body is whatever a node
//! holding `rw,p=<n>:/f/<F>/pub/<n>/>` chose to write, so carrying it under
//! those names emitted the key TWICE in one object — and every mainstream JSON
//! reader is last-wins, this crate's own [`Glance::from_json`] included, so the
//! publisher's `owner=` beat the broker's. `offset=` was worse than shadowing:
//! the body's value is emitted as a JSON *string*, which [`Glance::from_json`]
//! cannot read as a number, so ONE row denied the whole roster to every reader.
//! Four names of a publisher's is the price of the other three facts being
//! true; the drop is the one place this file renames or removes anything, and
//! it is listed here because this doc is the format's only specification.
//!
//! Two fields are guaranteed present even when the publisher omitted them, and
//! they are the two the human actually reads: `attention=` (why a session wants
//! a human) and `fabric=` (whether that session's node can still be reached). A
//! missing one reads `-`, exactly as `ls` renders a missing token — the reader
//! never has to distinguish "absent" from "unknown".
//!
//! ## Bounds, because this file is durable and a fleet is not bounded
//!
//! * At most [`MAX_ROWS`] rows. A `Last` answer is one row per SUBJECT, so a
//!   fleet that has ever hosted a million sessions has a million-row answer, and
//!   a menu bar that reads it at every wake would be the fleet's biggest cost.
//!   When the answer is cut, `"truncated": true` says so IN THE FILE rather
//!   than leaving a short list to be read as a complete one.
//! * **EVERY key and EVERY value is capped, not just `attention=`.** Each is cut
//!   to [`FIELD_CAP`] bytes and a row keeps at most [`FIELDS_MAX`] of them, so
//!   one row costs a bounded number of bytes and the file as a whole is bounded
//!   by [`MAX_ROWS`] times that. `attention=` is capped at [`ATTENTION_CAP`],
//!   which is the same number and is also the cap aterm applies to its own
//!   (§4.1's presence row) — ONE number, so the two sides cannot disagree.
//!
//!   This section used to justify capping `attention=` alone with "every other
//!   one is a bounded token". Nothing bounded them. [`fields_of`] keeps whatever
//!   the first line of the body holds, [`Row::parse`] validates only the
//!   SUBJECT, and a record is 16 MiB at the broker — so a hostile node's
//!   `detail=` was a multi-megabyte durable write per row, on a path a UI thread
//!   is meant to read at every wake. A bound the code does not keep is worse
//!   than no bound, because it is what the next reader budgets against.
//!
//!   A CUT VALUE IS MARKED with a trailing `…`, and the mark is INSIDE the cap:
//!   [`truncate`] answers at most `cap` bytes including the ellipsis, so a
//!   reader sizing a buffer from [`ATTENTION_CAP`] is not handed `cap + 3`.
//! * A value is written verbatim — still pct-encoded, exactly as it crossed the
//!   bus. `detail=`, `host=` and `title=` are pct-encoded at the publisher
//!   (§4.1), and decoding them here would put a caller-chosen byte sequence
//!   (a newline, an ESC) into a file another program renders. The reader
//!   decodes what it is about to show, at the moment it shows it.
//!
//! ## What A8 built, and what it did not
//!
//! It does not RENDER. The consuming half — the menu-bar `FabricGlance` above —
//! is DESIGNED and not built, and it is the first thing to check for before
//! treating this file's row shape, its `truncated` flag or its `-` defaults as a
//! contract with a shipped reader: there is none.
//!
//! It does not WATCH. §11.2 gives `serve` a `--glance` flag, so that the bridge
//! that already tails presence rewrites the file as they change; that flag needs
//! a line in `bridge.rs`, which A8 does not own. What is here is the whole
//! mechanism — the read, the projection, the atomic write — behind an
//! `aterm-link glance` subcommand that produces byte-identical output. See the
//! rung report.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::transport::Conn;

/// The most rows one `glance.json` may hold. See the module note on bounds.
pub const MAX_ROWS: usize = 4096;

/// The byte cap on ANY one key or value this file carries, INCLUDING the `…`
/// that marks a cut.
///
/// ONE NUMBER FOR EVERY FIELD, deliberately. The alternative — a cap per
/// interesting field — is three components with three implicit limits for one
/// value, which is the shape this project's worst defects have taken. 256 bytes
/// holds every token the publisher actually writes (`bridge.rs`'s presence body:
/// numbers, principals, a pct-encoded hostname, and `attention=` at exactly this
/// cap) with room to spare, and a longer one is a claim, not a fact.
pub const FIELD_CAP: usize = 256;

/// The byte cap on `attention=`, matching aterm's own (§4.1) — and the same
/// number as [`FIELD_CAP`], so a reader that budgets from either is right.
pub const ATTENTION_CAP: usize = FIELD_CAP;

/// The most `key=value` tokens one row's fields map keeps.
///
/// A row is a presence body's FIRST LINE split on whitespace, so its token count
/// is bounded by nothing the publisher does not choose. `bridge.rs` writes nine;
/// 64 leaves room for every field a later rung adds and still bounds one row at
/// [`FIELDS_MAX`] × 2 × [`FIELD_CAP`] bytes, and the file at [`MAX_ROWS`] times
/// that. Tokens past the bound are DROPPED — the row is still written, because a
/// node that pads its presence row must not be able to erase itself from a human
/// operator's view of the fleet.
pub const FIELDS_MAX: usize = 64;

/// The two tokens every row carries whether or not the publisher wrote them.
const GUARANTEED: [&str; 2] = ["attention", "fabric"];

/// The row keys [`Glance::to_json`] writes from the SUBJECT, which no body token
/// may shadow. See the module note on the four exceptions.
///
/// ONE LIST, READ BY BOTH SIDES: [`fields_of`] refuses these names on the way in
/// and [`Glance::to_json`] refuses them again at the splice, so a `Row` built by
/// any route — parsed, read back, or assembled by a test — still emits each of
/// them exactly once. A fifth subject-derived key added to the writer without a
/// line here is the same defect again, which is what
/// [`tests::no_body_token_can_shadow_a_cap_forced_row_fact`] fails on.
pub const RESERVED: [&str; 4] = ["subject", "offset", "node", "owner"];

/// The value a missing token reads as — the same dash `ls` prints.
pub const ABSENT: &str = "-";

/// How many rows one `Last` page asks for. The broker clamps this to
/// `LAST_PAGE_MAX`; asking for more is not an error, and asking for fewer only
/// costs round trips.
const PAGE: u32 = 512;

/// One presence row, as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The subject the row was published under, verbatim.
    pub subject: String,
    /// The broker offset the row was read at.
    pub offset: u64,
    /// The publishing node, from the subject's fifth segment.
    pub node: String,
    /// The face the row describes: a literal `node`, or a hosted `s-<hex>`.
    pub owner: String,
    /// Every `key=value` token of the body, verbatim and pct-encoded as
    /// published, plus the two [`GUARANTEED`] ones.
    pub fields: BTreeMap<String, String>,
}

impl Row {
    /// One field, or [`ABSENT`].
    #[must_use]
    pub fn field(&self, key: &str) -> &str {
        self.fields.get(key).map_or(ABSENT, String::as_str)
    }

    /// Parse one `Last` record into a row, or refuse it.
    ///
    /// REFUSAL IS THE POINT of returning an `Option`: the filter the reader
    /// sends is `/f/<F>/pub/*/*/presence`, and `*` is exactly one segment, so
    /// the broker cannot hand back anything else — but a file that is written
    /// once and read by a UI forever is the wrong place to trust a filter. The
    /// shape is re-checked here, by position from the left, the way every other
    /// subject in this crate is (`subject::parse_in`).
    #[must_use]
    pub fn parse(fleet: &str, offset: u64, subject: &str, body: &[u8]) -> Option<Row> {
        let segs: Vec<&str> = subject.split('/').collect();
        // `["", "f", "<F>", "pub", "<node>", "<owner>", "presence"]`
        if segs.len() != 7 || !segs[0].is_empty() {
            return None;
        }
        if segs[1] != "f" || segs[2] != fleet || segs[3] != "pub" || segs[6] != "presence" {
            return None;
        }
        if !crate::subject::is_principal(segs[4]) {
            return None;
        }
        let owner = segs[5];
        if owner != "node" && !(owner.starts_with("s-") && crate::subject::is_principal(owner)) {
            return None;
        }
        Some(Row {
            subject: subject.to_string(),
            offset,
            node: segs[4].to_string(),
            owner: owner.to_string(),
            fields: fields_of(body),
        })
    }
}

/// Every `key=value` token of a presence body's first line, verbatim — except a
/// token whose key is [`RESERVED`].
///
/// TOTAL over arbitrary bytes, like [`crate::body::Body::decode`]: the body is
/// read lossily as UTF-8, only the first line is considered (a presence row has
/// no `len=` tail, and a body that grew one must not smuggle a second row), a
/// token without `=` is skipped, and a repeated key keeps the LAST occurrence —
/// which is what a `k=v` line means everywhere else in this crate.
fn fields_of(body: &[u8]) -> BTreeMap<String, String> {
    let line = String::from_utf8_lossy(body);
    let line = line.split('\n').next().unwrap_or("");
    let mut out = BTreeMap::new();
    for token in line.split_whitespace() {
        let Some((k, v)) = token.split_once('=') else {
            continue;
        };
        if k.is_empty() {
            continue;
        }
        // EVERY key and EVERY value, not just `attention=`: see the module note
        // on bounds for the claim this used to rest on and the code that did not
        // keep it.
        let key = truncate(k, FIELD_CAP);
        // A BODY DOES NOT GET TO NAME A FACT THE SUBJECT ALREADY NAMES. These
        // four are written from the cap-forced subject; a body token of the same
        // name became a SECOND key of that name in the row object, which a
        // last-wins reader resolves in the publisher's favour, and `offset=`
        // became a string where the parser needs a number and took the whole
        // file down with it. Dropped here rather than renamed: a `node` field
        // under some other name is a second thing for a reader to get wrong.
        if RESERVED.contains(&key.as_str()) {
            continue;
        }
        // THE COUNT IS A BOUND TOO. A cap on each field bounds a field; the
        // NUMBER of them is the publisher's choice, and this file is durable.
        // The two [`GUARANTEED`] tokens are admitted past the bound, so a node
        // cannot pad its own row until the fields a human reads fall off it, and
        // a key already in the map is never refused — that would drop the LAST
        // occurrence, which is what a `k=v` line means everywhere in this crate.
        if out.len() >= FIELDS_MAX && !GUARANTEED.contains(&key.as_str()) && !out.contains_key(&key)
        {
            continue;
        }
        out.insert(key, truncate(v, FIELD_CAP));
    }
    for k in GUARANTEED {
        out.entry(k.to_string())
            .or_insert_with(|| ABSENT.to_string());
    }
    out
}

/// Truncate `v` to at most `cap` BYTES, on a char boundary, marking a cut with a
/// trailing `…`.
///
/// **THE `…` IS INSIDE THE CAP.** It used to be appended to a `cap`-byte cut, so
/// the answer was up to `cap + 3` bytes — three more than the module header
/// promises and three more than the publisher it calls "the same cap" can hold
/// (`bridge.rs`'s `attention_of` cuts hard at [`ATTENTION_CAP`] with no mark).
/// A caller sizing a buffer from the constant was handed more than it asked for
/// by exactly the amount that made the two caps different numbers.
///
/// A cap that silently produced invalid UTF-8 would be a cap that broke the file
/// it was protecting, so the cut is on a char boundary; and a `cap` too small to
/// hold the mark cuts without one rather than overrunning to carry it.
fn truncate(v: &str, cap: usize) -> String {
    if v.len() <= cap {
        return v.to_string();
    }
    const MARK: char = '…';
    let room = cap.saturating_sub(MARK.len_utf8());
    let mut end = room;
    while end > 0 && !v.is_char_boundary(end) {
        end -= 1;
    }
    if cap < MARK.len_utf8() {
        return v[..end].to_string();
    }
    format!("{}{MARK}", &v[..end])
}

/// The whole answer: every presence row of the fleet, in ascending subject
/// order, [`MAX_ROWS`]-bounded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Glance {
    /// The fleet the rows were read from.
    pub fleet: String,
    /// The rows, ascending by subject — the order `Last` answers in.
    pub rows: Vec<Row>,
    /// Whether [`MAX_ROWS`] cut the answer short.
    pub truncated: bool,
}

/// Read every presence row of `fleet` through one connection.
///
/// PAGES ON `resume`, NOT ON ROW COUNT. §5.2: the broker clamps `max` and cuts
/// each index scan after a fixed number of entries VISITED, so a page shorter
/// than `max` — an EMPTY one included — is not the end of the answer. Only an
/// empty `resume` cursor is. A reader that stopped at the first short page would
/// silently lose the tail of a large fleet, and would lose it in exactly the
/// case a large fleet is hardest to notice in.
///
/// # Errors
///
/// Any broker or transport failure.
pub fn read(conn: &mut Conn, fleet: &str) -> io::Result<Glance> {
    let filter = format!("/f/{fleet}/pub/*/*/presence");
    let mut after = String::new();
    let mut rows = Vec::new();
    let mut truncated = false;
    loop {
        let (page, _, resume) = conn.last_page(&filter, &after, PAGE)?;
        for (offset, subject, body) in &page {
            if rows.len() >= MAX_ROWS {
                truncated = true;
                break;
            }
            if let Some(row) = Row::parse(fleet, *offset, subject, body) {
                rows.push(row);
            }
        }
        if truncated || resume.is_empty() {
            break;
        }
        after = resume;
    }
    Ok(Glance {
        fleet: fleet.to_string(),
        rows,
        truncated,
    })
}

/// `<state>/fabric/glance.json` — where the file is written, and the path a
/// menu-bar reader would read (§9.3). No reader exists yet; see the header.
#[must_use]
pub fn path(state_root: &Path) -> PathBuf {
    state_root.join("fabric").join("glance.json")
}

impl Glance {
    /// Render the file.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut s = String::from("{\n  \"v\": 1,\n  \"fleet\": ");
        s.push_str(&json_string(&self.fleet));
        s.push_str(",\n  \"truncated\": ");
        s.push_str(if self.truncated { "true" } else { "false" });
        s.push_str(",\n  \"rows\": [");
        for (i, row) in self.rows.iter().enumerate() {
            s.push_str(if i == 0 { "\n" } else { ",\n" });
            s.push_str("    {\"subject\": ");
            s.push_str(&json_string(&row.subject));
            s.push_str(", \"offset\": ");
            s.push_str(&row.offset.to_string());
            s.push_str(", \"node\": ");
            s.push_str(&json_string(&row.node));
            s.push_str(", \"owner\": ");
            s.push_str(&json_string(&row.owner));
            for (k, v) in &row.fields {
                // The second half of the [`RESERVED`] rule, at the one place the
                // duplicate key would physically be written. `fields_of` cannot
                // put one here, but a `Row` assembled any other way can, and the
                // emitter is where "each key once" is actually a property.
                if RESERVED.contains(&k.as_str()) {
                    continue;
                }
                s.push_str(", ");
                s.push_str(&json_string(k));
                s.push_str(": ");
                s.push_str(&json_string(v));
            }
            s.push('}');
        }
        if !self.rows.is_empty() {
            s.push_str("\n  ");
        }
        s.push_str("]\n}\n");
        s
    }

    /// Write the file atomically under `state_root`.
    ///
    /// ATOMIC BY RENAME, and the temp file is named with this process's pid so
    /// two writers cannot interleave into one temp. A reader therefore sees the
    /// previous complete file or the next complete file, never a prefix — which
    /// matters because the reader is a UI thread with no way to retry a parse
    /// that failed halfway.
    ///
    /// # Errors
    ///
    /// Creating the directory, writing the temp file, or the rename.
    pub fn write_atomic(&self, state_root: &Path) -> io::Result<PathBuf> {
        let final_path = path(state_root);
        let dir = final_path
            .parent()
            .ok_or_else(|| io::Error::other("glance.json has no parent directory"))?;
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".glance.json.{}.tmp", std::process::id()));
        // `sync_all` before the rename: the rename is what publishes the file,
        // and publishing a name that points at unwritten blocks is the classic
        // way a crash leaves a valid-looking empty roster behind.
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(self.to_json().as_bytes())?;
            f.sync_all()?;
        }
        match std::fs::rename(&tmp, &final_path) {
            Ok(()) => Ok(final_path),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    /// Read a `glance.json` back.
    ///
    /// The parser accepts EXACTLY the shape [`Glance::to_json`] writes — a flat
    /// object per row, string and number values only — and answers `None` for
    /// anything else. It exists so a test (and a reader that would rather not
    /// grow a JSON dependency) can compare the file against the `Last` answer it
    /// claims to equal; it is not a general JSON parser and does not pretend to
    /// be one.
    #[must_use]
    pub fn from_json(text: &str) -> Option<Glance> {
        let mut p = Parser {
            b: text.as_bytes(),
            i: 0,
        };
        let mut fleet = String::new();
        let mut truncated = false;
        let mut rows = Vec::new();
        p.ws();
        p.byte(b'{')?;
        loop {
            p.ws();
            if p.peek() == Some(b'}') {
                p.i += 1;
                break;
            }
            let key = p.string()?;
            p.ws();
            p.byte(b':')?;
            p.ws();
            match key.as_str() {
                "fleet" => fleet = p.string()?,
                "truncated" => truncated = p.bool()?,
                "v" => {
                    // A file from a fold version this build does not know is
                    // refused, not guessed at (§10: two readers with different
                    // fold versions disagree loudly, never silently).
                    if p.number()? != 1 {
                        return None;
                    }
                }
                "rows" => rows = p.rows()?,
                _ => return None,
            }
            p.ws();
            if p.peek() == Some(b',') {
                p.i += 1;
            }
        }
        p.ws();
        if p.i != p.b.len() {
            return None;
        }
        Some(Glance {
            fleet,
            rows,
            truncated,
        })
    }
}

/// Render one JSON string. Every byte outside printable ASCII becomes a `\u`
/// escape, so a value that arrived off the bus can never close the string, open
/// a new key, or put a control byte into a file another program renders.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if ('\u{20}'..='\u{7e}').contains(&c) => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

/// The recursive-descent reader for [`Glance::from_json`]'s pinned shape.
struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn byte(&mut self, want: u8) -> Option<()> {
        (self.peek()? == want).then(|| self.i += 1)
    }

    fn bool(&mut self) -> Option<bool> {
        for (lit, val) in [(&b"true"[..], true), (&b"false"[..], false)] {
            if self.b[self.i..].starts_with(lit) {
                self.i += lit.len();
                return Some(val);
            }
        }
        None
    }

    fn number(&mut self) -> Option<u64> {
        let start = self.i;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        (self.i > start).then_some(())?;
        std::str::from_utf8(&self.b[start..self.i])
            .ok()?
            .parse()
            .ok()
    }

    fn string(&mut self) -> Option<String> {
        self.byte(b'"')?;
        let mut out = String::new();
        loop {
            match self.peek()? {
                b'"' => {
                    self.i += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.i += 1;
                    match self.peek()? {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'u' => {
                            let hex = self.b.get(self.i + 1..self.i + 5)?;
                            let unit =
                                u16::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
                            // A surrogate pair is two `\u` escapes; decode them
                            // together or a non-BMP char round-trips as U+FFFD.
                            let ch = if (0xd800..0xdc00).contains(&unit) {
                                if self.b.get(self.i + 5..self.i + 7)? != b"\\u" {
                                    return None;
                                }
                                let hex2 = self.b.get(self.i + 7..self.i + 11)?;
                                let low = u16::from_str_radix(std::str::from_utf8(hex2).ok()?, 16)
                                    .ok()?;
                                self.i += 6;
                                char::decode_utf16([unit, low]).next()?.ok()?
                            } else {
                                char::from_u32(u32::from(unit))?
                            };
                            out.push(ch);
                            self.i += 4;
                        }
                        _ => return None,
                    }
                    self.i += 1;
                }
                c if c < 0x20 => return None,
                _ => {
                    let rest = std::str::from_utf8(&self.b[self.i..]).ok()?;
                    let ch = rest.chars().next()?;
                    out.push(ch);
                    self.i += ch.len_utf8();
                }
            }
        }
    }

    fn rows(&mut self) -> Option<Vec<Row>> {
        self.byte(b'[')?;
        let mut rows = Vec::new();
        loop {
            self.ws();
            if self.peek()? == b']' {
                self.i += 1;
                return Some(rows);
            }
            rows.push(self.row()?);
            self.ws();
            if self.peek()? == b',' {
                self.i += 1;
            }
        }
    }

    fn row(&mut self) -> Option<Row> {
        self.byte(b'{')?;
        let mut subject = None;
        let mut offset = None;
        let mut node = None;
        let mut owner = None;
        let mut fields = BTreeMap::new();
        loop {
            self.ws();
            if self.peek()? == b'}' {
                self.i += 1;
                break;
            }
            let key = self.string()?;
            self.ws();
            self.byte(b':')?;
            self.ws();
            match key.as_str() {
                "subject" => subject = Some(self.string()?),
                "node" => node = Some(self.string()?),
                "owner" => owner = Some(self.string()?),
                "offset" => offset = Some(self.number()?),
                _ => {
                    fields.insert(key, self.string()?);
                }
            }
            self.ws();
            if self.peek()? == b',' {
                self.i += 1;
            }
        }
        Some(Row {
            subject: subject?,
            offset: offset?,
            node: node?,
            owner: owner?,
            fields,
        })
    }
}

/// `aterm-link glance` — the entry `main.rs` registers.
///
/// One `Last` round trip and one atomic write; the flag parsing it shares with
/// `tui` lives in [`crate::tui`], which is where both subcommands' command line
/// is documented.
#[must_use]
pub fn main(args: &[String]) -> std::process::ExitCode {
    crate::tui::main_entry(args, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **THE HEADER'S CLAIM ABOUT THE MENU BAR, PINNED TO THE MENU BAR.**
    ///
    /// aterm has no evidence manifest, so this module's doc is its claim, and
    /// the claim it used to make — that `status_item.rs` folds these rows into a
    /// `FabricGlance` — named a type that does not exist. The pin binds the two
    /// files: while `status_item.rs` has no `FabricGlance`, the header must keep
    /// saying the reader is not built, and the day one lands this fails and the
    /// paragraph gets rewritten in the present tense it deserves.
    ///
    /// `include_str!` across crates is the house pattern for pinning a claim to
    /// the code that would falsify it (`aterm-gui/src/control.rs` pins
    /// `aterm-control/src/selection.rs` the same way); it is test-only and adds
    /// no dependency.
    #[test]
    fn the_header_does_not_promise_a_menu_bar_reader_that_is_not_there() {
        let ui = include_str!("../../aterm-gui/src/status_item.rs");
        let me = include_str!("glance.rs");
        // Assembled, not written out: a literal would be found in this
        // function's own source and the pin would match itself.
        let marker = ["NO", "SUCH", "READER", "IS", "BUILT."].join(" ");
        let reader = ui.contains("FabricGlance") || ui.contains("glance.json");
        assert!(
            !reader,
            "status_item.rs grew a fabric-glance reader; glance.rs's header \
             still says one is not built"
        );
        assert!(
            me.contains(&marker),
            "no reader of glance.json exists, so this module's header must say so"
        );
    }

    fn row(subject: &str, body: &str) -> Row {
        Row::parse("f1", 7, subject, body.as_bytes()).expect("a presence row")
    }

    /// EVERY TOKEN SURVIVES, including one this build has never heard of, and
    /// the two the human reads are present even when the publisher omitted
    /// them. A row that quietly dropped `attention=` would be a menu bar that
    /// never shows an escalation.
    #[test]
    fn a_row_carries_every_token_and_always_the_two_that_matter() {
        let r = row(
            "/f/f1/pub/n-a/s-0123456789abcdef0123/presence",
            "v=1 t=5 inc=2 state=live hold=0 fabric=connected weather=sunny",
        );
        assert_eq!(r.node, "n-a");
        assert_eq!(r.owner, "s-0123456789abcdef0123");
        assert_eq!(r.field("state"), "live");
        assert_eq!(r.field("fabric"), "connected");
        assert_eq!(r.field("weather"), "sunny", "an unknown token must survive");
        assert_eq!(r.field("attention"), ABSENT, "absent reads as a dash");
        let bare = row("/f/f1/pub/n-a/node/presence", "v=1 t=5 state=gone");
        assert_eq!(bare.owner, "node");
        assert_eq!(bare.field("fabric"), ABSENT);
        assert_eq!(bare.field("attention"), ABSENT);
    }

    /// The subject shape is re-checked HERE, by position, whatever the broker's
    /// filter promised. Note the eight-segment forgery: right-anchored, its tail
    /// reads as a presence row of node `n-a`.
    #[test]
    fn only_a_presence_subject_of_this_fleet_becomes_a_row() {
        for bad in [
            "/f/f2/pub/n-a/node/presence",     // another fleet
            "/f/f1/pub/n-a/node/ev",           // another leaf
            "/f/f1/in/n-a/node/presence",      // another face
            "/f/f1/pub/andrew/node/presence",  // a node with no class
            "/f/f1/pub/n-a/nodes/presence",    // an owner that is neither
            "/f/f1/pub/n-a/h-andrew/presence", // an owner that is not a session
            "/f/f1/pub/x/n-a/node/presence",   // eight segments
            "f/f1/pub/n-a/node/presence",      // no leading slash
        ] {
            assert!(Row::parse("f1", 1, bad, b"v=1").is_none(), "{bad}");
        }
    }

    /// `attention=` is capped — and the cap must not corrupt the file it
    /// protects, nor exceed the number the module header hands to a reader.
    #[test]
    fn attention_is_capped_on_a_char_boundary() {
        let long = format!("attention={}", "é".repeat(400));
        let r = row("/f/f1/pub/n-a/node/presence", &long);
        let got = r.field("attention");
        // AT MOST THE CAP, MARK INCLUDED. The `…` used to be three bytes ON TOP
        // of it, so the "same cap" the header shares with `bridge.rs` was three
        // bytes bigger here than there.
        assert!(got.len() <= ATTENTION_CAP, "{}", got.len());
        assert!(got.ends_with('…'));
        // Not capped, not marked: a value that fits is untouched.
        let short = row("/f/f1/pub/n-a/node/presence", "attention=needs%20you");
        assert_eq!(short.field("attention"), "needs%20you");
    }

    /// **EVERY FIELD IS BOUNDED, NOT ONLY `attention=`.** The header's bounds
    /// section justified capping one token with "every other one is a bounded
    /// token"; nothing bounded them, and this file is durable, written from a
    /// remote publisher's 16 MiB record, and meant to be read at every wake.
    #[test]
    fn no_publisher_chooses_how_big_a_row_is() {
        let huge = "z".repeat(100_000);
        let body =
            format!("state=live detail={huge} host={huge} {huge}=x attention={huge} sneaky={huge}");
        let r = row("/f/f1/pub/n-a/node/presence", &body);
        for (k, v) in &r.fields {
            assert!(k.len() <= FIELD_CAP, "key of {} bytes", k.len());
            assert!(v.len() <= FIELD_CAP, "{k} value of {} bytes", v.len());
        }
        assert_eq!(r.field("state"), "live", "a short value is untouched");
        assert!(r.field("detail").ends_with('…'), "a cut is marked");

        // AND THE NUMBER OF THEM. One line of a million distinct keys is one
        // row's fields map, and the row is durable.
        let many: String = (0..5000).map(|i| format!("k{i}=v{i} ")).collect();
        let r = row("/f/f1/pub/n-a/node/presence", &many);
        assert!(
            r.fields.len() <= FIELDS_MAX + GUARANTEED.len(),
            "{}",
            r.fields.len()
        );
        // The two tokens a human reads survive a row padded to the bound: a node
        // must not be able to hide its own escalation by filling the map.
        let padded = format!("{many} attention=needs%20you");
        let r = row("/f/f1/pub/n-a/node/presence", &padded);
        assert_eq!(r.field("attention"), "needs%20you");
        assert_eq!(r.field("fabric"), ABSENT);
    }

    /// A HOSTILE VALUE CANNOT ESCAPE ITS STRING. The bus carries pct-encoded
    /// tokens, but nothing on the READ side enforces that, and the file is
    /// rendered by a menu bar: a quote, a backslash or a newline that survived
    /// into the JSON would be a value that writes the next key.
    #[test]
    fn a_value_cannot_break_out_of_the_json() {
        let mut g = Glance {
            fleet: "f1".into(),
            rows: vec![row("/f/f1/pub/n-a/node/presence", "state=live")],
            truncated: false,
        };
        g.rows[0].fields.insert(
            "attention".into(),
            "\" , \"owner\": \"n-evil\" \\ \n \u{1b}]0;x\u{7}".into(),
        );
        let text = g.to_json();
        assert_eq!(
            text.matches("\"owner\"").count(),
            1,
            "one owner key:\n{text}"
        );
        assert!(!text.contains('\u{1b}'), "an ESC reached the file");
        let back = Glance::from_json(&text).expect("round trip");
        assert_eq!(back, g, "the escape must be lossless, not lossy");
    }

    /// **A BODY TOKEN CANNOT SHADOW A CAP-FORCED ROW FACT.** The value route is
    /// the test above; this is the KEY route, which needs no escaping at all.
    ///
    /// `subject`, `offset`, `node` and `owner` are written from the subject the
    /// broker handed back — `node` and `owner` cap-forced, the other two the
    /// reader's own. A body is whatever a node holding its own `pub/<n>/` grant
    /// chose to write, and splicing it into the SAME object emitted a second key
    /// of that name AFTER the honest one: every mainstream JSON reader is
    /// last-wins, [`Glance::from_json`] included, so a node published itself as
    /// somebody else's session on somebody else's host. `offset=` did not even
    /// need a lie — the body's value is emitted as a string, `Parser::row` wants
    /// a number, and the whole `glance.json` then parsed as `None`: one hostile
    /// row denying the fleet roster to every reader.
    #[test]
    fn no_body_token_can_shadow_a_cap_forced_row_fact() {
        let r = row(
            "/f/f1/pub/n-evil/node/presence",
            "v=1 state=live owner=s-victim0000 node=n-lab subject=/f/f1/nope offset=99",
        );
        assert_eq!(r.node, "n-evil", "the subject names the node, not the body");
        assert_eq!(r.owner, "node");
        for k in RESERVED {
            assert!(!r.fields.contains_key(k), "{k}= survived into the fields");
        }
        // The tokens that are NOT reserved are untouched: this drops four names,
        // not a publisher's row.
        assert_eq!(r.field("state"), "live");

        let g = Glance {
            fleet: "f1".into(),
            rows: vec![r],
            truncated: false,
        };
        let text = g.to_json();
        for k in RESERVED {
            // The KEY, not the name anywhere in the text: `"owner": "node"` is
            // a legitimate VALUE that spells one of these.
            assert_eq!(
                text.matches(&format!("\"{k}\": ")).count(),
                1,
                "{k} appears twice:\n{text}"
            );
        }
        // ONE ROW MUST NOT DENY THE FILE. The `offset=` case failed the parse of
        // every row, not just its own.
        assert_eq!(Glance::from_json(&text).as_ref(), Some(&g));

        // And the emitter refuses one too, so the property does not depend on
        // where the `Row` came from — a hand-built or future-parsed row cannot
        // write the duplicate key either.
        let mut hand = g.clone();
        hand.rows[0].fields.insert("offset".into(), "x".into());
        hand.rows[0].fields.insert("owner".into(), "n-evil".into());
        let text = hand.to_json();
        assert_eq!(text.matches("\"offset\": ").count(), 1, "{text}");
        assert_eq!(text.matches("\"owner\": ").count(), 1, "{text}");
        assert_eq!(Glance::from_json(&text).as_ref(), Some(&g));
    }

    /// The file round-trips, empty and full, and the parser refuses what it does
    /// not understand rather than half-reading it.
    #[test]
    fn the_file_round_trips_and_refuses_what_it_cannot_read() {
        let empty = Glance {
            fleet: "f1".into(),
            rows: Vec::new(),
            truncated: false,
        };
        assert_eq!(Glance::from_json(&empty.to_json()).as_ref(), Some(&empty));
        let full = Glance {
            fleet: "f1".into(),
            rows: vec![
                row(
                    "/f/f1/pub/n-a/node/presence",
                    "v=1 state=live fabric=connected",
                ),
                row(
                    "/f/f1/pub/n-b/s-0123456789abcdef0123/presence",
                    "v=1 state=live attention=review%20the%20diff fabric=connected",
                ),
            ],
            truncated: true,
        };
        assert_eq!(Glance::from_json(&full.to_json()).as_ref(), Some(&full));
        for bad in [
            "",
            "{",
            "{}x",
            "{\"v\": 2, \"fleet\": \"f1\", \"truncated\": false, \"rows\": []}",
            "{\"v\": 1, \"nope\": 1}",
            "{\"v\": 1, \"rows\": [{\"subject\": \"/f/f1/pub/n-a/node/presence\"}]}",
        ] {
            assert!(Glance::from_json(bad).is_none(), "{bad:?}");
        }
    }

    /// The write is atomic and lands where the menu bar looks.
    #[test]
    fn the_write_lands_at_the_path_the_menu_bar_reads() {
        let dir = std::env::temp_dir().join(format!("atl-glance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let g = Glance {
            fleet: "f1".into(),
            rows: vec![row(
                "/f/f1/pub/n-a/node/presence",
                "state=live fabric=connected",
            )],
            truncated: false,
        };
        let written = g.write_atomic(&dir).expect("write");
        assert_eq!(written, path(&dir));
        assert_eq!(written, dir.join("fabric").join("glance.json"));
        let text = std::fs::read_to_string(&written).expect("read back");
        assert_eq!(Glance::from_json(&text).as_ref(), Some(&g));
        // No temp file is left behind.
        let strays: Vec<_> = std::fs::read_dir(dir.join("fabric"))
            .expect("dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .filter(|n| n != "glance.json")
            .collect();
        assert!(strays.is_empty(), "{strays:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
