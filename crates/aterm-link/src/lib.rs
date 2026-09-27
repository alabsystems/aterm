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
//! any path.** Since round 21 that is an ABSENCE, not a check: this crate
//! contains no function that writes to a PTY. A record of every kind, from
//! every principal, carrying any `re=`, `epoch=` or `gen=` it likes, becomes an
//! inbox row and nothing else.
//!
//! It used to be a fence. Exactly one record shape was converted to PTY bytes —
//! `/f/<F>/term/<node>/<sid>/in/<src>` — under §6.6's four conditions: the
//! `<src>` held the keyboard, the session was not held, the body's `epoch=`
//! equalled the live launch nonce, and `gen=` was absent or equal to the live
//! `content_seq:fp16`. Nothing in the tree wrote such a record; `aterm drive
//! --dial` is the cross-host driving path and it does not go through the bus.
//! The face, its conditions, its holder table and its feed journal are gone,
//! and `bridge`'s module header states what is left in their place. The SUBJECT
//! stays reserved in [`subject`] and the node ring still carries both `term`
//! grants, so an older node's record is still recognised for what it is — and
//! then not served.
//!
//! A `re=` is NEVER AN INPUT TO AN AUTHORITY DECISION. That is the whole of the
//! claim, and it is deliberately narrower than "read in one place": `re=` is
//! read as CORRELATION at the delivery seam, by the renderers, and by the
//! deadline machinery — [`bridge::Bridge::deliver_record`]
//! forwards it onto the `deliver` line (and reads it to SETTLE the deadline it
//! names and to flag a reply that arrives after `expired` as `late=1`),
//! [`bridge::Bridge::expire_deadlines`] reads the asker's own lane to learn
//! whether an `answer`/`report`/`ack` carrying it arrived before it publishes a
//! verdict, this crate's own [`fabric`] report reads it to fold `answer`s
//! against overdue asks for the `aterm fabric` WARNINGS, and `aterm-gui`'s `fabric`
//! resolves it against the recipient's OWN outbound posts into the `re-id=` a
//! reader sees. A correlation label is not
//! a permission — a deadline verdict is a notification, never a keystroke or an
//! admission: none of those readers can grant, apply, admit or deliver anything
//! on the strength of one,
//! and a `re=` a sender chose can therefore point a rendered row at an
//! unrelated post of the recipient's — which is why the row also carries the
//! sender's trust label, and why nothing downstream may treat `re-id=` as
//! proof of who answered. §6.6 states why the authority half has to hold: "a
//! `re=` is dense and guessable and a `gen=` is observable from presence, so a
//! body that could *trigger* keystrokes on their strength would let any
//! lane-writer drive a worker."
//!
//! There is no function in this crate that writes to a PTY. `bridge`'s module
//! header states the full argument, and `tests/bridge_e2e.rs`'s
//! `no_bus_record_ever_reaches_a_pty` drives it: records of every kind, from
//! every principal class, move no byte of the terminal.

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
/// INFORMATIONAL is the whole of its contract: records carry no timestamp of
/// their own, nothing orders by it, and a broker-stamped time is a named seed
/// (§4.1, §14). A clock that jumps costs a confusing display and nothing else.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
