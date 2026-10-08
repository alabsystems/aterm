// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
//! any path.** That is an absence, not a check: this crate contains no function
//! that writes to a PTY. A record of every kind, from every principal, carrying
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
//! `tests/world/bridge_e2e.rs`'s `no_bus_record_ever_reaches_a_pty` drives it.

pub mod body;
pub mod bridge;
/// The bridge command line, as a library entry - see [`cli::dispatch`].
pub mod cli;
pub mod ctl;
/// `aterm fabric on|off|doctor` — see [`enable::on`].
pub mod enable;
/// `aterm fabric` — the fabric's state on one screen, and its traffic live.
pub mod fabric;
pub mod glance;
/// `aterm fabric mint-for|join` — a SECOND HOST joins the fleet over the
/// sealed transport; see [`join::join`].
pub mod join;
pub mod mailbox;
pub mod notify;
pub mod pct;
/// The presence row's meaning fields (`role= detail= phase= title=`).
pub mod presence;
pub mod render;
pub mod state;
pub mod subject;
pub mod transport;

/// Milliseconds since the Unix epoch — the body's informational `t=`.
///
/// Nothing orders by it (a broker-stamped time is a named seed, §4.1, §14), so
/// a clock jump costs a confusing display and nothing else.
#[must_use]
pub fn now_ms() -> u64 {
    aterm_time::unix_millis().unwrap_or(0)
}
