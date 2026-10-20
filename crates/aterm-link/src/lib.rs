// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// No `unsafe` here: the composing executable admits raw inherited descriptors
// at startup and transfers owned lanes through the safe entry points.
#![forbid(unsafe_code)]
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! **aterm-link** — the fabric bridge (`DESIGN-aterm-fabric.md` §11.2).
//!
//! One per aterm instance, launched BY the instance as a child holding the far
//! ends of two `socketpair(AF_UNIX)`s at fds 3 and 4. It is the *node*: the bus
//! principal under which every session the instance hosts is addressed and
//! attested.
//!
//! ## The two rules the whole crate exists to keep
//!
//! **aterm's engine owns zero I/O.** Everything on this side of the socket —
//! every broker connection, every capability, every byte of the fabric wire —
//! lives in this process. aterm speaks only its own control protocol, and the
//! endpoint it exposes (`aterm-gui`'s `fabric` module) is state, not I/O.
//!
//! **Screen content is untrusted data, and no record becomes terminal input by
//! any path.** Half of that is an absence: this crate contains no function that
//! writes to a PTY. The other half is a check, because the lane to aterm is
//! owner-class and could carry a verb that does: the one writer onto it refuses
//! a line aterm would frame as a second request or split into a field it was
//! not built with ([`ctl::Ctl::request_with_body`]), and each field's own
//! closed grammar keeps the ASCII space out of it. A record of every kind, from every principal, carrying
//! any `re=`, `epoch=` or `gen=`, becomes an inbox row and nothing else. The
//! `term/.../in/` subject stays reserved in [`subject`] so an older node's
//! record is recognised — and then not served.
//!
//! A `re=` is never an input to an authority decision. It is read only as
//! correlation: [`bridge::Bridge::deliver_record`] forwards it onto the
//! `deliver` line and settles the deadline it names (`late=1` after `expired`),
//! [`bridge::Bridge::expire_deadlines`] checks the asker's lane for a reply,
//! [`fabric`] folds `answer`s against overdue asks, and `aterm-gui`'s `fabric`
//! resolves it to a `re-id=` against the recipient's own posts. None of those
//! can grant, apply, admit or deliver anything on its strength. A sender may
//! point `re=` at an unrelated post, so the row also carries the sender's trust
//! label and nothing may treat `re-id=` as proof of who answered. §6.6: "a `re=`
//! is dense and guessable and a `gen=` is observable from presence, so a body
//! that could *trigger* keystrokes on their strength would let any lane-writer
//! drive a worker."
//!
//! `bridge`'s module header states the full argument;
//! `crates/aterm/tests/fabric_world/bridge_e2e.rs`'s `no_bus_record_ever_reaches_a_pty` drives it.
//!
//! Inert off unix: the bridge's lanes are inherited `AF_UNIX` descriptors, its
//! dials are Unix sockets, its secrets are owner-only modes, and astream serves
//! a Unix endpoint only there (`Broker::serve`). crates/aterm links it only on
//! unix (answering `aterm link` elsewhere with a refusal), so a Windows
//! workspace build compiles nothing here.
#![cfg(unix)]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

// `print!`/`println!` in this crate are lines on stdout around a verb's answer (`aterm link`,
// `aterm fabric`, run in-process by the `aterm` executable), written through
// `aterm_uds::stdout`: a reader that closed the pipe (`| head -1`) drops them, where std's
// panicked with exit 101 on the EPIPE. A help or status answer goes through
// `aterm_uds::stdout::answer_exit`, which exits 1 when a full disk lost it; `mint-for`'s cap
// through `aterm_uds::stdout::write_all`, refused on any write error; and the inbox through the
// writer `person::main` hands it, so mail it could not print is never marked read.
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

pub mod body;
pub mod bridge;
mod broker_args;
/// The bridge command line, as a library entry - see [`cli::dispatch`].
pub mod cli;
pub mod ctl;
/// The broker's `--durability strict|ordered|relaxed`, and how a relaxed
/// log's power loss reaches every reader as a generation that continues —
/// see [`durability::open`].
pub mod durability;
/// `aterm fabric on|off|doctor` — see [`enable::on`].
pub mod enable;
/// `aterm fabric` — the fabric's state on one screen, and its traffic live.
pub mod fabric;
pub mod fabric_brief;
pub mod glance;
pub mod mailbox;
pub mod mirror;
pub mod notify;
pub mod pct;
/// `aterm fabric halt|inbox|post` — a person speaking for themself, under a
/// cap of their own; see [`person::main`].
pub mod person;
mod pipeline;
/// The presence row's meaning fields (`role= detail= phase= title=`).
pub mod presence;
/// The live view of who advertises which session, so a bare `@s-<sid>` post
/// routes on one bounded read instead of a walk of the presence index; see
/// [`presence_view::PresenceView`].
pub mod presence_view;
pub mod render;
pub mod state;
pub mod subject;
pub mod transport;
/// How long the bridge's loop waits on the broker, counted where it blocks: a
/// development seam the fabric bench's ping-pong arm reads; see
/// [`wait_tally::Tally`].
pub mod wait_tally;

/// Milliseconds since the Unix epoch — the body's informational `t=`.
///
/// Nothing orders by it (a broker-stamped time is a named seed, §4.1, §14), so
/// a clock jump costs a confusing display and nothing else.
#[must_use]
pub fn now_ms() -> u64 {
    aterm_time::unix_millis().unwrap_or(0)
}

/// A unit test's broker, at astream's Relaxed tier: an ack once the batch is in the
/// page cache. A test's broker runs in a scratch dir on the boot volume in a boot
/// the kernel names, where every event a test can make — a dropped or crashed
/// broker reopened, a clean stop — keeps the log's identity and every acked record
/// under Relaxed exactly as under Strict ([`durability`]'s module doc), so the
/// bridge sees the same log; what it does not pay is a `F_FULLFSYNC` (about 4 ms,
/// serialised across the drive) per batch. A test of what a tier promises opens
/// its broker through [`durability::open`] at the tier it names.
///
/// # Errors
///
/// [`astream_broker::Broker::open_with`]'s.
#[cfg(test)]
pub(crate) fn test_broker(
    log: impl AsRef<std::path::Path>,
) -> std::io::Result<astream_broker::Broker> {
    astream_broker::Broker::open_with(log, astream_broker::Durability::Relaxed)
}
