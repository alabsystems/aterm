// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crate's one reader of presence rows: `Last{/f/<F>/pub/*/*/presence}` parsed into
//! [`Row`]s, for `aterm fabric status` ([`read`]) and `notify --on attention` ([`Row::parse`]).
//!
//! A row is the `Last` answer, not a summary: every `key=value` body token is kept verbatim
//! under its own name, unknown ones included, so two readers of one fleet cannot disagree. The
//! [`RESERVED`] names come from the subject the broker handed back, and a body token of one is
//! dropped, or a node could publish itself as somebody else's session. `attention=` and
//! `fabric=` are always present (`-` when missing).
//!
//! Bounds: [`MAX_ROWS`] rows, with a cut answer marked [`Glance::truncated`]; [`FIELD_CAP`]
//! bytes per key and value and [`FIELDS_MAX`] tokens per row, since a record may be 16 MiB at
//! the broker. Values stay pct-encoded as they crossed the bus: decoding here would put a
//! publisher-chosen newline or ESC into every rendered string, so a reader decodes when it
//! shows a value.

use std::collections::BTreeMap;
use std::io;

use crate::transport::Conn;

/// The most rows one roster read may hold: a `Last` answer is one row per subject ever hosted.
pub const MAX_ROWS: usize = 4096;

/// The byte cap on any one key or value a row carries, including the `…` that marks a cut.
/// 256 holds every token the bridge's presence rows carry (`attention=` sits at exactly this cap).
pub const FIELD_CAP: usize = 256;

/// The byte cap on `attention=`, matching aterm's own (§4.1) and [`FIELD_CAP`].
pub const ATTENTION_CAP: usize = FIELD_CAP;

/// The most `key=value` tokens one row keeps, bounding a row at [`FIELDS_MAX`] × 2 ×
/// [`FIELD_CAP`] bytes. Tokens past it are dropped but the row is kept: a node that pads its
/// row must not erase itself from an operator's view.
pub const FIELDS_MAX: usize = 64;

/// The two tokens every row carries whether or not the publisher wrote them.
const GUARANTEED: [&str; 2] = ["attention", "fabric"];

/// The row keys [`Row::parse`] fills from the cap-forced subject; [`fields_of`] drops a body
/// token of any of these names.
pub const RESERVED: [&str; 4] = ["subject", "offset", "node", "owner"];

/// The value a missing token reads as — the same dash `ls` prints.
pub const ABSENT: &str = "-";

/// The `state=` a NODE's will writes on its presence row: the broker fires it
/// when the bridge's connection ends without the next incarnation fencing it —
/// a crash, a `kill -9`, a lost link, an exit. The node is AWAY, not over: its
/// sessions' rows stay as they were (each reads `away` with it), mail to them
/// queues on the bus, and a bridge that comes back as the node publishes
/// `live` above it. A session that is over says so on its own row, `exited`.
pub const AWAY: &str = "away";

/// Whether a node row's `state=` is its WILL: [`AWAY`], or `gone` as a bridge
/// before 2026-10-15 wrote the same will.
#[must_use]
pub fn is_will(state: &str) -> bool {
    state == AWAY || state == "gone"
}

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
    /// The broker's filter should admit nothing else, but a rendered row is the
    /// wrong place to trust a filter: the shape is re-checked by position from
    /// the left, as `subject::parse_in` does.
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
/// Total over arbitrary bytes, like [`crate::body::Body::decode`]: lossy UTF-8,
/// first line only (a body must not smuggle a second row), a token without `=`
/// skipped, and a repeated key keeps the last occurrence.
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
        let key = truncate(k, FIELD_CAP);
        // A body may not name a fact the subject names. Dropped, not renamed:
        // a renamed `node` is a second thing for a reader to get wrong.
        if RESERVED.contains(&key.as_str()) {
            continue;
        }
        // The count is bounded too; [`GUARANTEED`] tokens pass it so padding cannot push them
        // off, and a key already in the map is never refused (last occurrence wins).
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

/// Truncate `v` to at most `cap` bytes on a char boundary, counting the `…` that marks a cut;
/// a `cap` too small for the mark cuts without one.
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

/// Read every presence row of `fleet` through one connection, with astream's
/// [`astream_broker::Client::last_walk`] (§5.2: a short page is not the end of the answer).
/// [`Glance::truncated`] is set only when a further row exists past [`MAX_ROWS`].
///
/// # Errors
///
/// Any broker or transport failure, or a walk that did not end within
/// [`astream_broker::LAST_WALK_PAGES_MAX`] pages.
pub fn read(conn: &mut Conn, fleet: &str) -> io::Result<Glance> {
    let filter = format!("/f/{fleet}/pub/*/*/presence");
    let mut rows = Vec::new();
    let mut truncated = false;
    conn.last_walk(&filter, "", u32::MAX, |(offset, subject, body)| {
        if rows.len() >= MAX_ROWS {
            truncated = true;
            return Ok(astream_broker::Walk::Stop);
        }
        if let Some(row) = Row::parse(fleet, offset, &subject, &body) {
            rows.push(row);
        }
        Ok(astream_broker::Walk::Continue)
    })?;
    Ok(Glance {
        fleet: fleet.to_string(),
        rows,
        truncated,
    })
}

/// Render one JSON string. Every byte outside printable ASCII becomes a `\u`
/// escape, so a value that arrived off the bus can never close the string, open
/// a new key, or put a control byte into a file another program renders.
pub(crate) fn json_string(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(subject: &str, body: &str) -> Row {
        Row::parse("f1", 7, subject, body.as_bytes()).expect("a presence row")
    }

    /// Every token survives, unknown ones included, and the two guaranteed
    /// tokens are present even when the publisher omitted them.
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

    /// The subject shape is re-checked here, by position, whatever the broker's filter
    /// promised. Right-anchored, the eight-segment forgery's tail would read as node `n-a`'s row.
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

    /// `attention=` is capped on a char boundary, mark included.
    #[test]
    fn attention_is_capped_on_a_char_boundary() {
        let long = format!("attention={}", "é".repeat(400));
        let r = row("/f/f1/pub/n-a/node/presence", &long);
        let got = r.field("attention");
        assert!(got.len() <= ATTENTION_CAP, "{}", got.len());
        assert!(got.ends_with('…'));
        // Not capped, not marked: a value that fits is untouched.
        let short = row("/f/f1/pub/n-a/node/presence", "attention=needs%20you");
        assert_eq!(short.field("attention"), "needs%20you");
    }

    /// Every key, value and the token count are bounded.
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

    /// A body token cannot shadow a cap-forced row fact ([`RESERVED`]).
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
        // Tokens that are not reserved are untouched.
        assert_eq!(r.field("state"), "live");
    }
}
