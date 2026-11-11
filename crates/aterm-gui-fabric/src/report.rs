// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One `kind=report` a session sends another, wherever that one runs: what aterm-gui's
//! `worker_mail` sends a worker's manager (`docs/DESIGN-workers-subagents-2026-11-04.md` §8,
//! Fabric). It goes by the transport the fabric already has for that place, so the manager reads
//! it as any agent's mail, with `aterm ctl @self inbox`:
//! - **A session of this Mac** ([`To::Here`]): on this Mac's bus, as `post to=@<sid>` queues it,
//!   the sender being the session [`post`] posts as. With no bridge here (the fabric absent) the
//!   bus carries nothing, so nothing is queued.
//! - **A session on another Mac** ([`To::There`]): by direct fleet mail, one fleet drive that posts
//!   it into the session's mailbox there ([`super::fleet_mail`]), the sender being this Mac
//!   (`fleet:<node>`); with the mesh on, on the bus to a Mac whose presence row this one heard
//!   (§11 C6). The bus is one Mac's alone until then, so no bridge carries mail across Macs.
//!
//! A SESSION LEAVING THIS MAC'S ROSTER ([`post_leaving`], a worker whose tab closes) is carried
//! by direct fleet mail alone: the bridge drains only the outboxes of sessions still on the
//! roster, and resolves no sender that left, so a post it queued on the bus would go nowhere.

use super::FabricSession;
use super::fleet_mail::{self, PeerMail, Uncarried};

/// Where a report goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum To<'a> {
    /// The session `sid` of this Mac.
    Here(&'a str),
    /// The session `sid` on the Mac `node` (a proved drive's id, [`fleet_mail::valid_node`]).
    There { sid: &'a str, node: &'a str },
}

/// The longest report: one line, the most direct fleet mail and an inline post carry.
pub const REPORT_MAX: usize = super::POST_INLINE_MAX;

/// Why nothing took a report ([`post`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsent {
    /// Not one line of 1..=[`REPORT_MAX`] bytes.
    NotALine,
    /// A key no `post` takes.
    BadKey(String),
    /// An address that names no session: a sid that is none, or a node no proved drive names.
    NoSession(String),
    /// No fabric bridge runs here, so this Mac's bus carries nothing: the fabric's state word.
    NoBridge(String),
    /// This Mac's bus refused it: `post`'s answer.
    Refused(String),
    /// No fleet drive reached `node`, and why.
    NoDrive { node: String, why: String },
    /// `node` answered the direct fleet mail with no message's id: its answer.
    Answered { node: String, answer: String },
    /// The sender is leaving this Mac's roster, and only the bus reaches the session it is to
    /// ([`post_leaving`]).
    Leaving,
}

impl std::fmt::Display for Unsent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotALine => write!(f, "a report is one line of 1..{REPORT_MAX} bytes"),
            Self::BadKey(key) => write!(f, "the key {key:?} is no post key"),
            Self::NoSession(sid) => write!(f, "{sid:?} names no session"),
            Self::NoBridge(state) => write!(
                f,
                "no fabric bridge runs here (fabric={state}), so the bus carries no mail"
            ),
            Self::Refused(answer) => write!(f, "the bus refused it: {answer}"),
            Self::NoDrive { node, why } => write!(f, "no fleet drive reached {node}: {why}"),
            Self::Answered { node, answer } => {
                write!(f, "{node} refused the direct fleet mail: {answer}")
            }
            Self::Leaving => f.write_str(
                "the sender left this Mac's roster, and only its bus reaches the session, which \
                 carries no departed sender's mail",
            ),
        }
    }
}

/// Send `text`, one line of at most [`REPORT_MAX`] bytes, from `ctx`'s session to `to` as a
/// `kind=report` under the idempotency key `key`, so a second send of the same text under it is
/// answered as the same message (`dup=1`) by the broker's dedup or the mailbox on the other Mac.
/// It never waits for the record to land.
/// # Errors
/// Nothing took it ([`Unsent`]): a malformed address, key or text; no bridge here for the bus
/// (the fabric absent); `post`'s refusal; no drive reached the other Mac, or it refused the mail.
pub fn post(ctx: &dyn FabricSession, to: To<'_>, key: &str, text: &str) -> Result<String, Unsent> {
    send(ctx, to, key, text, false)
}

/// [`post`] from a session leaving this Mac's roster (a worker whose tab closes): by direct fleet
/// mail to a session on another Mac, which the session's departure does not touch.
/// # Errors
/// As [`post`]'s, and [`Unsent::Leaving`] where only the bus reaches the session.
pub fn post_leaving(
    ctx: &dyn FabricSession,
    to: To<'_>,
    key: &str,
    text: &str,
) -> Result<String, Unsent> {
    send(ctx, to, key, text, true)
}

fn send(
    ctx: &dyn FabricSession,
    to: To<'_>,
    key: &str,
    text: &str,
    leaving: bool,
) -> Result<String, Unsent> {
    if !super::valid_key(key) {
        return Err(Unsent::BadKey(key.to_owned()));
    }
    if text.is_empty() || text.len() > REPORT_MAX || text.contains(['\n', '\r']) {
        return Err(Unsent::NotALine);
    }
    match to {
        To::Here(sid) if super::is_principal(sid) => {
            if leaving {
                return Err(Unsent::Leaving);
            }
            bus(ctx, &format!("@{sid}"), key, text)
        }
        To::There { sid, node } if super::is_principal(sid) && fleet_mail::valid_node(node) => {
            if ctx.fabric().mesh_imports() && ctx.peer_mail(node) == PeerMail::Mesh && !leaving {
                return bus(ctx, &format!("@{sid}@{node}"), key, text);
            }
            match fleet_mail::drive_post(ctx, sid, node, "report", key, text) {
                Ok(carried) => Ok(format!(
                    "OK fleet=f{} to=@{sid}@{node} key={key}{}",
                    carried.id,
                    if carried.dup { " dup=1" } else { "" }
                )),
                Err(Uncarried::NoDrive(why)) => Err(Unsent::NoDrive {
                    node: node.to_owned(),
                    why,
                }),
                Err(Uncarried::Answered(answer)) => Err(Unsent::Answered {
                    node: node.to_owned(),
                    answer,
                }),
            }
        }
        To::Here(sid) | To::There { sid, .. } => Err(Unsent::NoSession(sid.to_owned())),
    }
}

/// `post to=<to> kind=report key=<key> -- <text>` on this Mac's bus, where a bridge carries it.
fn bus(ctx: &dyn FabricSession, to: &str, key: &str, text: &str) -> Result<String, Unsent> {
    if !super::bridge_reachable() {
        return Err(Unsent::NoBridge(super::fabric_state().to_string()));
    }
    let reply = super::cmd_post(
        ctx,
        &format!("to={to} kind=report key={key} -- {text}"),
        None,
    );
    let line = reply.trim_end_matches(['\r', '\n']).to_owned();
    if line.starts_with("OK ") {
        Ok(line)
    } else {
        Err(Unsent::Refused(line))
    }
}

#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
