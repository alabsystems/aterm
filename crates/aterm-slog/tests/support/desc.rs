// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Description claims' helpers, shared by `descriptions.rs` and `card.rs`: a claim's and a
//! withdrawal's fields, a `desc` mark by an author, a writer aterm ran, a turn and another agent
//! run, and what a reader is shown and listed.

use aterm_slog::{
    Actor, Close, Conf, DescRow, Fields, Key, Kind, Log, Mark, Node, Open, Out, Rank, ShortId, Src,
    TurnOut, shown,
};

use super::{WALL0, at};

/// A claim's fields: `slot` is `value`.
#[must_use]
pub fn say(slot: &str, value: &str) -> Fields {
    Fields::new()
        .tok(Key::Field, slot)
        .and_then(|f| f.text(Key::Value, value))
        .expect("a claim's fields")
}

/// A withdrawal's fields: `slot`, and no value.
#[must_use]
pub fn unsay(slot: &str) -> Fields {
    Fields::new().tok(Key::Field, slot).expect("a slot")
}

/// A `desc` mark by `by` (observed at `src`) on `node`, at `ms`.
pub fn desc(log: &mut Log, by: Actor, src: Src, node: Node, f: Fields, ms: u64) -> u64 {
    log.mark(
        Mark::new(Kind::Desc, src, Conf::Claim)
            .by(by)
            .at(node)
            .fields(f),
        at(ms),
    )
    .expect("a desc mark")
}

/// A writer aterm ran, naming itself `name`, says `slot` of `node` is `value`.
pub fn writer(log: &mut Log, name: &str, node: Node, slot: &str, value: &str, ms: u64) -> u64 {
    let f = say(slot, value).text(Key::As, name).expect("as=");
    desc(log, Actor::Aterm, Src::Mark, node, f, ms)
}

/// A session's short id.
#[must_use]
pub fn sid(s: &str) -> ShortId {
    ShortId::new(s).expect("a sid")
}

/// The text and rank of the claim a reader is shown for `slot`, `ms` into the test.
#[must_use]
pub fn stands<'a>(log: &'a Log, slot: &str, ms: u64) -> Option<(&'a str, Rank)> {
    shown(log.state(), slot, WALL0 + ms).map(|s| (s.claim.value.as_str(), s.rank))
}

/// Every claim, as `described` lists it, `ms` into the test.
#[must_use]
pub fn listing(log: &Log, ms: u64) -> Vec<DescRow<'_>> {
    log.state().desc.listing(WALL0 + ms)
}

/// A turn of `run` that opens at `ms` and closes a millisecond later; the close's seq.
pub fn turn(log: &mut Log, run: Node, ms: u64) -> u64 {
    let t = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(ms),
        )
        .expect("a turn opens");
    log.close(
        t,
        Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
        at(ms + 1),
    )
    .expect("the turn closes")
}

/// Another agent run under the session.
pub fn agent(log: &mut Log, ms: u64) -> Node {
    log.open(
        Open::new(Kind::Agent, Src::Status, Conf::Strong).under(Node::SESSION),
        at(ms),
    )
    .expect("an agent run opens")
}
