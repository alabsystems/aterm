// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The subject taxonomy (§3.3): every subject the bridge publishes is built here,
//! and every subject it receives is parsed here.
//!
//! The parse is the load-bearing half: on the `in` face a stranger's bytes choose
//! part of the address. `>` matches one or more segments, so an 8-segment subject's
//! last two would read as a forged `<src>/<kind>`; an `in` subject is therefore
//! exactly seven segments, parsed by position from the left. A record of another
//! shape, a `<src>` without an `s-|n-|h-|a-` prefix, or a `<sid>` this node does not
//! host is refused, never delivered. The broker's cap check (the `in` grant ends in
//! `*`) refuses the 8-segment publish too (R4); this is the receiver's own wall.

// The segment word grammar (§3.2) lives in `aterm-types`: the endpoint holds lines
// to it too and does not depend on this crate.
pub use aterm_types::fabric_grammar::{is_principal, is_topic, PRINCIPAL_MAX};

/// A fleet name segment: the same bounded lowercase grammar, without a class
/// prefix (it is not a principal).
#[must_use]
pub fn is_fleet(f: &str) -> bool {
    (1..=32).contains(&f.len())
        && f.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Why a record on an `in` lane was refused. Each variant becomes an `ev
/// undeliverable … reason=<token>` record on the bus and nothing else: a refused
/// record is never delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    /// Not seven segments, not under this fleet, or a segment that is not the
    /// shape its position demands.
    Malformed,
    /// A `<sid>` this node does not host. Nothing binds a sid to a node on the
    /// bus, so a sender can address one that moved; the receiver is the only
    /// place that knows.
    NotHosted,
    /// A record on this node's own lane, under this node's `<src>`, at an offset the
    /// bridge never got a `PublishAck` for: a forgery by a co-holder of the node cap
    /// (§6.2). Recorded and escalated, never delivered.
    ForgedSelf,
}

impl Reject {
    /// The `reason=` token this refusal is recorded under.
    #[must_use]
    pub fn token(self) -> &'static str {
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
    /// The owning node — always this bridge's own, or the record would not have
    /// been delivered by the group filter.
    pub node: String,
    /// The session the message is addressed to.
    pub sid: String,
    /// The sender, cap-forced at the broker: the one segment a sender cannot choose,
    /// so provenance is read straight off it (§4.3).
    pub src: String,
    /// The message kind.
    pub kind: String,
}

/// Parse an `in` subject by position from the left, against the fleet and node this
/// bridge serves: exactly `f`, `<F>`, `in`, `<node>`, `<sid>`, `<src>`, `<kind>`. A
/// right-anchored parse is what an 8-segment subject exploits.
///
/// # Errors
///
/// [`Reject::Malformed`] for anything that is not that shape.
pub fn parse_in(fleet: &str, node: &str, subject: &str) -> Result<InAddr, Reject> {
    let segs: Vec<&str> = subject.split('/').collect();
    // A leading '/' yields an empty first segment: `["", "f", "<F>", "in", …]`.
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
    if !crate::body::is_kind(kind) {
        return Err(Reject::Malformed);
    }
    Ok(InAddr {
        node: node.to_string(),
        sid: sid.to_string(),
        src: src.to_string(),
        kind: kind.to_string(),
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

/// The durable group name the node's inbox drain commits under. `cur` is a name
/// face, never published to or delivered, but the cap must grant it (the broker's
/// `SubscribeGroup`/`Commit` arms), so it is built here beside the subjects.
#[must_use]
pub fn inbox_group(fleet: &str, node: &str) -> String {
    format!("/f/{fleet}/cur/{node}/node/inbox")
}

/// Every `in` lane this node owns, as one filter (§6.1: "the owner's bridge
/// reads all lanes with one filter").
#[must_use]
pub fn inbox_filter(fleet: &str, node: &str) -> String {
    format!("/f/{fleet}/in/{node}/>")
}

/// The fleet broadcast face: halts and barriers, drained ahead of the inbox group on
/// every scheduling round (§11.2).
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

/// Parse a fleet halt row, `/f/<F>/fleet/<h-name>/halt`, by the rule every bridge
/// applies a halt under (§5.3), or `None` for a record that halts nothing.
///
/// Five segments by position from the left, `halt` as the leaf, and an `h-`
/// principal: a halt is a human's word, so a row under any other principal (an
/// `a-` service's, a node's) halts nothing. The body is read by the bridge's own
/// decoder ([`crate::body::Body`]): the first line only, space-separated, and a
/// repeated `state=` takes its last value.
///
/// The bridge, `aterm fabric`'s status and `link notify --on halt` all read the
/// face by this, so no reader reports a halt that no bridge applies.
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

/// Every broadcast face in the fleet, as one filter. One subscription, not one per
/// topic: opt-in (`topic add`) is local, and per-topic subscriptions would put every
/// session's topic set on the bus as churn.
#[must_use]
pub fn say_filter(fleet: &str) -> String {
    format!("/f/{fleet}/pub/*/*/say/>")
}

/// One topic's broadcasts fleet-wide, `/f/<F>/pub/*/*/say/<topic>`: a read filter,
/// never a subscription, so it brings none of the churn [`say_filter`] avoids. A
/// backlog `Fetch` over it has the broker skip other topics' records within its
/// bounded scan instead of filling the page with them. `topic` must be an
/// [`is_topic`] name, one literal segment with no `*`, `>` or `/`; the bridge checks
/// every topic that enters its cursor set.
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

/// Parse a broadcast subject left-anchored and by position, seven segments
/// (`f`/`<F>`/`pub`/`<node>`/`<sid>`/`say`/`<topic>`), like [`parse_in`]: the filter's `>`
/// matches one or more segments, so only a counting parse refuses a longer subject.
/// `None` for anything else.
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
mod tests {
    use super::*;

    /// The 8-segment forgery is malformed, though a right-anchored reading
    /// would name `h-andrew` as the sender.
    #[test]
    fn an_eight_segment_subject_is_malformed_however_its_tail_reads() {
        let forged = "/f/f1/in/n-a/s-b/s-1/h-andrew/answer";
        assert_eq!(parse_in("f1", "n-a", forged), Err(Reject::Malformed));
        // The seven-segment form parses, and its sender is the sixth segment.
        let honest = "/f/f1/in/n-a/s-b/s-1/answer";
        assert_eq!(
            parse_in("f1", "n-a", honest),
            Ok(InAddr {
                node: "n-a".into(),
                sid: "s-b".into(),
                src: "s-1".into(),
                kind: "answer".into(),
            })
        );
    }

    /// Every other wrong shape is the same refusal.
    #[test]
    fn the_shape_is_pinned_at_every_position() {
        for bad in [
            "/f/f2/in/n-a/s-b/s-1/answer",    // another fleet
            "/f/f1/in/n-z/s-b/s-1/answer",    // another node
            "/f/f1/pub/n-a/s-b/s-1/answer",   // another face
            "/f/f1/in/n-a/node/s-1/answer",   // a reserved literal as the sid
            "/f/f1/in/n-a/n-b/s-1/answer",    // a NODE as the addressee
            "/f/f1/in/n-a/s-b/andrew/answer", // a src with no class
            "/f/f1/in/n-a/s-b/s-1/whatever",  // an unknown kind
            "/f/f1/in/n-a/s-b/s-1",           // six segments
            "f/f1/in/n-a/s-b/s-1/answer",     // no leading slash
            "/f/f1/in/n-a/s-b/s-1/answer/x",  // eight again
        ] {
            assert_eq!(parse_in("f1", "n-a", bad), Err(Reject::Malformed), "{bad}");
        }
    }

    /// Only a human's row of the halt shape halts: an `a-` service's or a node's
    /// `state=on` row, or one of another shape, halts nothing. The body reads as
    /// the bridge's decoder reads it: a tab is not a separator, a newline ends the
    /// tokens, and a later `state=` wins.
    #[test]
    fn only_a_humans_halt_row_halts() {
        let on = b"v=1 t=1 state=on reason=main%20broken";
        let row = parse_halt("f1", "/f/f1/fleet/h-andrew/halt", on).expect("a human's halt");
        assert_eq!(
            row,
            HaltRow {
                human: "h-andrew",
                on: true,
                reason: Some("main%20broken".to_string()),
            }
        );
        let off = parse_halt("f1", "/f/f1/fleet/h-andrew/halt", b"v=1 t=2 state=off");
        assert_eq!(off.map(|r| (r.on, r.reason)), Some((false, None)));
        for bad in [
            "/f/f1/fleet/a-svc/halt",       // a service is not a human
            "/f/f1/fleet/n-node/halt",      // nor is a node
            "/f/f2/fleet/h-andrew/halt",    // another fleet
            "/f/f1/fleet/h-andrew/barrier", // another leaf
            "/f/f1/fleet/h-andrew/halt/x",  // six segments
            "/f/f1/fleet/x/h-andrew/halt",  // a right-anchored forgery
            "f/f1/fleet/h-andrew/halt",     // no leading slash
            "/f/f1/pub/h-andrew/halt",      // another face
        ] {
            assert_eq!(parse_halt("f1", bad, on), None, "{bad}");
        }
        for (body, state_on) in [
            (&b"v=1 state=off state=on"[..], true),
            (b"v=1 state=on state=off", false),
            (b"v=1\tstate=on", false),
            (b"v=1\nstate=on", false),
            (b"v=1 state=ON", false),
        ] {
            let row = parse_halt("f1", "/f/f1/fleet/h-a/halt", body).expect("a human's row");
            assert_eq!(row.on, state_on, "{:?}", String::from_utf8_lossy(body));
        }
    }
}
