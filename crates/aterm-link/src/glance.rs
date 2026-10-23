// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reader and parser for fleet presence rows (`Last{/f/<F>/pub/*/*/presence}`).

use std::collections::BTreeMap;
use std::io;

use crate::transport::Conn;

/// Maximum rows one roster read may hold.
pub const MAX_ROWS: usize = 4096;

/// Byte cap on any single key or value in a row, including the truncation `…`.
pub const FIELD_CAP: usize = 256;

/// Byte cap on `attention=`, matching [`FIELD_CAP`].
pub const ATTENTION_CAP: usize = FIELD_CAP;

/// Maximum `key=value` tokens kept per row.
pub const FIELDS_MAX: usize = 64;

const GUARANTEED: [&str; 2] = ["attention", "fabric"];

/// Row keys filled from the subject; matching body tokens are dropped.
pub const RESERVED: [&str; 4] = ["subject", "offset", "node", "owner"];

/// Placeholder value for a missing token.
pub const ABSENT: &str = "-";

/// Presence `state=` value written by a node's broker will when its connection ends unfenced.
pub const AWAY: &str = "away";

/// Whether a node row's `state=` represents a broker will ([`AWAY`] or legacy `gone`).
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
    /// Verbatim pct-encoded `key=value` body tokens plus guaranteed keys.
    pub fields: BTreeMap<String, String>,
}

impl Row {
    /// One field, or [`ABSENT`].
    #[must_use]
    pub fn field(&self, key: &str) -> &str {
        self.fields.get(key).map_or(ABSENT, String::as_str)
    }

    /// Parses one `Last` record into a [`Row`], validating the subject shape by position.
    #[must_use]
    pub fn parse(fleet: &str, offset: u64, subject: &str, body: &[u8]) -> Option<Row> {
        let segs: Vec<&str> = subject.split('/').collect();
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
        if RESERVED.contains(&key.as_str()) {
            continue;
        }
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

/// All presence rows of a fleet in ascending subject order, bounded by [`MAX_ROWS`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Glance {
    /// The fleet the rows were read from.
    pub fleet: String,
    /// The rows, ascending by subject.
    pub rows: Vec<Row>,
    /// Whether [`MAX_ROWS`] cut the answer short.
    pub truncated: bool,
}

/// Reads all presence rows of `fleet` using [`astream_broker::Client::last_walk`].
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

/// Renders `s` as a JSON string with non-ASCII and control characters escaped as `\uXXXX`.
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

    #[test]
    fn attention_is_capped_on_a_char_boundary() {
        let long = format!("attention={}", "é".repeat(400));
        let r = row("/f/f1/pub/n-a/node/presence", &long);
        let got = r.field("attention");
        assert!(got.len() <= ATTENTION_CAP, "{}", got.len());
        assert!(got.ends_with('…'));
        let short = row("/f/f1/pub/n-a/node/presence", "attention=needs%20you");
        assert_eq!(short.field("attention"), "needs%20you");
    }

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
        let padded = format!("{many} attention=needs%20you");
        let r = row("/f/f1/pub/n-a/node/presence", &padded);
        assert_eq!(r.field("attention"), "needs%20you");
        assert_eq!(r.field("fabric"), ABSENT);
    }

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
        assert_eq!(r.field("state"), "live");
    }
}
