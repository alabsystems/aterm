// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared helpers: a fake clock (the crate reads none) and a session with an
//! agent run open.

use aterm_slog::{Actor, Conf, Fields, Key, Kind, Log, Node, Open, Src, Stamp};

/// Wall time of the test epoch (2025-10-09), so `t=` reads like a real one.
pub const WALL0: u64 = 1_760_000_000_000;

/// The stamp `ms` into the test: wall and monotonic move together.
#[must_use]
pub fn at(ms: u64) -> Stamp {
    Stamp::new(WALL0 + ms, ms)
}

/// A session log with its session opened and an agent run (`claude`) open
/// under it, at `ms`.
pub fn agent_session(log: &mut Log, ms: u64) -> Node {
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(ms),
    )
    .expect("the session opens");
    let f = Fields::new()
        .tok(Key::Agent, "claude")
        .expect("agent is a token key");
    log.open(
        Open::new(Kind::Agent, Src::Status, Conf::Strong).fields(f),
        at(ms),
    )
    .expect("the agent run opens")
}

/// Every record of `log`'s window, encoded at the content tier.
#[must_use]
pub fn lines(log: &Log) -> Vec<String> {
    log.window()
        .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Content).expect("fits"))
        .collect()
}
