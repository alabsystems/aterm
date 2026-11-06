// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![forbid(unsafe_code)]
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm-link: the fabric bridge (`DESIGN-aterm-fabric.md` §11.2).
#![cfg(unix)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

macro_rules! print {
    ($($arg:tt)*) => {
        ::aterm_uds::stdout::print(::std::format_args!($($arg)*))
    };
}
macro_rules! println {
    () => {
        ::aterm_uds::stdout::print(::std::format_args!("\n"))
    };
    ($($arg:tt)*) => {
        ::aterm_uds::stdout::println(::std::format_args!($($arg)*))
    };
}

#[cfg(test)]
aterm_durable::plain_durable_flushes_in_tests!();

/// The bus log sealed at rest under a key left out of backups.
pub mod at_rest;
pub mod body;
pub mod bridge;
mod broker_args;
/// What a serving broker watches in itself.
pub mod broker_health;
/// The retention rules every broker aterm opens keeps its log by.
pub mod broker_rules;
pub mod cli;
pub mod ctl;
/// The broker's `--durability strict|ordered|relaxed`.
pub mod durability;
pub mod enable;
pub mod fabric;
pub mod fabric_brief;
/// The fabric over the mesh: the fleet `alab`, key-named nodes, and mail one hop.
pub mod federate;
/// The bridge's doors: its clocks, park, broker dials and readers, verb lane,
/// jitter and state-dir writes, in one place a simulation can stand in for.
mod fx;
pub mod glance;
pub mod mailbox;
pub mod mirror;
pub mod notify;
/// `aterm link notify --on need`: each session's need as `aterm fabric brief` folds it.
pub mod notify_need;
pub mod pct;
/// `aterm fabric halt|inbox|post` — a person speaking for themself, under a cap of their own.
pub mod person;
/// A person's key: the seal that carries a person's post to another Mac as the person's.
pub mod person_key;
mod pipeline;
pub mod presence;
/// The live view of who advertises which session.
pub mod presence_view;
/// The broker's rate limit, waited out (critic item E06).
mod rate_hold;
pub mod render;
/// The deterministic fabric simulation (test builds): one real bridge, its
/// doors answered by a seeded world.
#[cfg(test)]
mod sim;
pub mod state;
pub mod subject;
pub mod transport;
/// An unverifiable reservation holds its own sender, not the link (critic item F21).
mod unverified_hold;
/// How long the bridge's loop waits on the broker, counted where it blocks.
pub mod wait_tally;

/// Milliseconds since the Unix epoch — the body's informational `t=`.
#[inline]
#[must_use]
pub fn now_ms() -> u64 {
    aterm_time::unix_millis().unwrap_or(0)
}

#[inline]
pub(crate) fn parse_uint<T: std::str::FromStr>(s: &str) -> Option<T> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// A unit test's broker, at astream's Relaxed tier: an ack once the batch is in the page cache.
/// # Errors
/// [`astream_broker::Broker::open_with`]'s.
#[cfg(test)]
pub(crate) fn test_broker(
    log: impl AsRef<std::path::Path>,
) -> std::io::Result<astream_broker::Broker> {
    astream_broker::Broker::open_with(log, astream_broker::Durability::Relaxed)
}
