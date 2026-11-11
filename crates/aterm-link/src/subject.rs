// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The subject taxonomy (§3.3).

use astream_wire::WakeClass;
pub use aterm_types::fabric_grammar::{is_principal, is_topic, PRINCIPAL_MAX};

/// A fleet name segment: the same bounded lowercase grammar, without a class prefix.
#[must_use]
pub fn is_fleet(f: &str) -> bool {
    (1..=32).contains(&f.len())
        && f.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Why a record on an `in` lane was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// Not seven segments, not under this fleet, or a segment that is not the shape its position
    /// demands.
    Malformed,
    /// A `<sid>` this node does not host.
    NotHosted,
    /// Record on this node's own lane at an offset the bridge never got a `PublishAck` for.
    ForgedSelf,
}

impl Reject {
    /// The `reason=` token this refusal is recorded under.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Reject::Malformed => "malformed",
            Reject::NotHosted => "not-hosted",
            Reject::ForgedSelf => "forged-self",
        }
    }
}

/// A parsed `/f/<F>/in/<node>/<sid>/<src>/<kind>` address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InAddr {
    /// The owning node.
    pub node: String,
    /// The session the message is addressed to.
    pub sid: String,
    /// The sender, cap-forced at the broker; for a person's post another Mac carried, once the
    /// bridge verified its seal, `<person>@<node>` (§11 C3c, `Bridge::carried_person`).
    pub src: String,
    /// The message kind, the `<kind>` segment astream's [`WakeClass::parse`] read.
    pub kind: WakeClass,
}

/// Parse an `in` subject by position from the left, against the fleet and node this bridge serves:
/// exactly `f`, `<F>`, `in`, `<node>`, `<sid>`, `<src>`, `<kind>`.
/// # Errors
/// [`Reject::Malformed`] for anything that is not that shape.
pub fn parse_in(fleet: &str, node: &str, subject: &str) -> Result<InAddr, Reject> {
    let segs: Vec<&str> = subject.split('/').collect();
    if segs.len() != 8 || !segs[0].is_empty() {
        return Err(Reject::Malformed);
    }
    if segs[1] != "f" || segs[2] != fleet || segs[3] != "in" || segs[4] != node {
        return Err(Reject::Malformed);
    }
    let (sid, src, kind) = (segs[5], segs[6], segs[7]);
    if !is_principal(sid) || !sid.starts_with("s-") || !is_principal(src) {
        return Err(Reject::Malformed);
    }
    let Some(kind) = WakeClass::parse(kind) else {
        return Err(Reject::Malformed);
    };
    Ok(InAddr {
        node: node.to_string(),
        sid: sid.to_string(),
        src: src.to_string(),
        kind,
    })
}

/// `/f/<F>/pub/<node>/node/<leaf>` — the node's own face.
#[must_use]
pub fn node_face(fleet: &str, node: &str, leaf: &str) -> String {
    format!("/f/{fleet}/pub/{node}/node/{leaf}")
}

/// `/f/<F>/pub/<node>/<sid>/<leaf>` — one hosted session's face.
#[must_use]
pub fn session_face(fleet: &str, node: &str, sid: &str, leaf: &str) -> String {
    format!("/f/{fleet}/pub/{node}/{sid}/{leaf}")
}

/// `/f/<F>/in/<node>/<sid>/<src>/<kind>` — a post onto one hosted session's inbox lane, from `src`.
#[must_use]
pub fn post_lane(fleet: &str, node: &str, sid: &str, src: &str, kind: &str) -> String {
    format!("/f/{fleet}/in/{node}/{sid}/{src}/{kind}")
}

/// `/f/<F>/in/p/<person>/<node>`.
#[must_use]
pub fn person_lane(fleet: &str, person: &str, node: &str) -> String {
    format!("/f/{fleet}/in/p/{person}/{node}")
}

/// Every record on a person's lane, as one filter: what `inbox` reads.
#[must_use]
pub fn person_mail_filter(fleet: &str, person: &str) -> String {
    format!("/f/{fleet}/in/p/{person}/>")
}

/// `/f/<F>/fleet/<person>/halt` — a person's retained fleet halt row.
#[must_use]
pub fn person_halt(fleet: &str, person: &str) -> String {
    format!("/f/{fleet}/fleet/{person}/halt")
}

/// The durable group a person's `inbox` commits under.
#[must_use]
pub fn person_inbox_group(fleet: &str, person: &str) -> String {
    format!("/f/{fleet}/cur/p/{person}/inbox")
}

/// The durable group name the node's inbox drain commits under.
#[must_use]
pub fn inbox_group(fleet: &str, node: &str) -> String {
    format!("/f/{fleet}/cur/{node}/node/inbox")
}

/// Every `in` lane this node owns, as one filter.
#[must_use]
pub fn inbox_filter(fleet: &str, node: &str) -> String {
    format!("/f/{fleet}/in/{node}/>")
}

/// The fleet broadcast face.
#[must_use]
pub fn fleet_filter(fleet: &str) -> String {
    format!("/f/{fleet}/fleet/>")
}

/// A fleet halt row as every bridge reads it ([`parse_halt`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaltRow<'a> {
    /// The halting human: the `h-` principal segment, as the subject spells it.
    pub human: &'a str,
    /// The row says `state=on`; any other state, or none, lifts that human's halt.
    pub on: bool,
    /// The `reason=` token as it arrived (pct-encoded, a stranger's bytes), if any.
    pub reason: Option<String>,
}

/// Parse a fleet halt row (`/f/<F>/fleet/<h-name>/halt`).
#[must_use]
pub fn parse_halt<'a>(fleet: &str, subject: &'a str, body: &[u8]) -> Option<HaltRow<'a>> {
    let segs: Vec<&'a str> = subject.split('/').collect();
    if segs.len() != 6 || !segs[0].is_empty() {
        return None;
    }
    if segs[1] != "f" || segs[2] != fleet || segs[3] != "fleet" || segs[5] != "halt" {
        return None;
    }
    let human = segs[4];
    if !human.starts_with("h-") {
        return None;
    }
    let (mut body, _) = crate::body::Body::decode(body);
    Some(HaltRow {
        human,
        on: body.unknown.get("state").is_some_and(|s| s == "on"),
        reason: body.unknown.remove("reason"),
    })
}

/// The leaf of a person's halt requests ([`halt_ask`]), beside the old `halt` row's.
pub const HALT_ASK_LEAF: &str = "halts";

/// `/f/<F>/fleet/<person>/halts` — where a person asks their own Mac's bridge to halt the fleet,
/// or to lift a halt, by its id (`crate::halt::Ask`; §11 C4). Only that person's ring writes it.
#[must_use]
pub fn halt_ask(fleet: &str, person: &str) -> String {
    format!("/f/{fleet}/fleet/{person}/{HALT_ASK_LEAF}")
}

/// Every person's halt requests, as one filter.
#[must_use]
pub fn halt_asks(fleet: &str) -> String {
    format!("/f/{fleet}/fleet/*/{HALT_ASK_LEAF}")
}

/// The person a halt request's subject names: `None` for any other subject, and for a principal
/// that is no person (`h-`), since only a person halts.
#[must_use]
pub fn parse_halt_ask<'a>(fleet: &str, subject: &'a str) -> Option<&'a str> {
    let rest = subject.strip_prefix("/f/")?.strip_prefix(fleet)?;
    let person = rest
        .strip_prefix("/fleet/")?
        .strip_suffix(HALT_ASK_LEAF)?
        .strip_suffix('/')?;
    (is_principal(person) && person.starts_with("h-")).then_some(person)
}

/// `/f/<F>/halt/<node>` — the one subject a Mac's bridge says its halts on, relayed to every Mac
/// (§4.11).
#[must_use]
pub fn halt_subject(fleet: &str, node: &str) -> String {
    format!("/f/{fleet}/halt/{node}")
}

/// Every Mac's halt subject, as one filter.
#[must_use]
pub fn halt_filter(fleet: &str) -> String {
    format!("/f/{fleet}/halt/>")
}

/// The Mac a halt subject names: `None` for any other subject, a deeper one included.
#[must_use]
pub fn parse_halt_origin<'a>(fleet: &str, subject: &'a str) -> Option<&'a str> {
    let node = subject
        .strip_prefix("/f/")?
        .strip_prefix(fleet)?
        .strip_prefix("/halt/")?;
    crate::federate::is_node(node).then_some(node)
}

/// Every broadcast face in the fleet, as one filter.
#[must_use]
pub fn say_filter(fleet: &str) -> String {
    format!("/f/{fleet}/pub/*/*/say/>")
}

/// One topic's broadcasts fleet-wide, `/f/<F>/pub/*/*/say/<topic>`.
#[must_use]
pub fn say_topic_filter(fleet: &str, topic: &str) -> String {
    format!("/f/{fleet}/pub/*/*/say/{topic}")
}

/// A parsed `/f/<F>/pub/<node>/<sid>/say/<topic>` broadcast subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SayAddr {
    /// The publishing node.
    pub node: String,
    /// The publishing session.
    pub sid: String,
    /// The topic the record was published on.
    pub topic: String,
}

/// Parse a seven-segment broadcast subject (`f`/`<F>`/`pub`/`<node>`/`<sid>`/`say`/`<topic>`).
#[must_use]
pub fn parse_say(fleet: &str, subject: &str) -> Option<SayAddr> {
    let segs: Vec<&str> = subject.split('/').collect();
    if segs.len() != 8 || !segs[0].is_empty() {
        return None;
    }
    if segs[1] != "f" || segs[2] != fleet || segs[3] != "pub" || segs[6] != "say" {
        return None;
    }
    let (node, sid, topic) = (segs[4], segs[5], segs[7]);
    if !is_principal(node) || !is_principal(sid) || !sid.starts_with("s-") || !is_topic(topic) {
        return None;
    }
    Some(SayAddr {
        node: node.to_string(),
        sid: sid.to_string(),
        topic: topic.to_string(),
    })
}

#[cfg(test)]
#[path = "subject_tests.rs"]
mod tests;
