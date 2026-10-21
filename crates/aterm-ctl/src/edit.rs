// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `amacs` client's two exchanges with the window hosting its tab (amacs
//! design §6.1-§6.4; slice N2): `edit` asks for a fresh Editor view of a file
//! beside the session, and `await edit` waits until that view is gone.
//!
//! [`edit`] is one exchange. The server answers once the view is installed,
//! and refuses on its own deadline (30 s, `ERR timeout ticket=<t>`, after
//! which a late admission is dropped), so the wait here outlasts it. What a
//! failure means for the client is [`EditErr::falls_back_to_terminal`]: an
//! `ERR` or a window nobody could ask sends it to its own terminal; a
//! request the window took and did not answer never does, because the view
//! may still open and a second editor on the file would race it.
//!
//! [`await_edit`] is RE-ARMED with no total limit. Each leg asks for the
//! server's bound (600 s); `OK timeout` re-arms the same key at once, inside
//! the server's orphan grace. A leg cut by its server going away (a seamless
//! update closes every connection at its Commit, a crash closes them all) is
//! re-dialled: the session's socket is resolved afresh
//! ([`crate::resolve_sock_for`], the graph entry a successor republishes) and
//! the wait is re-armed by `uri=`, because a successor knows no ticket the old
//! process minted but waits on the file's Editor views. Nothing taking the
//! wait within [`crate::redial::SUCCESSOR_BOUND`] ends it
//! [`AwaitErr::Replaced`], which the client exits [`crate::EXIT_REPLACED`]
//! with.
//!
//! Both are written against a [`Wire`]: the control socket, or a test's
//! script and clock.

use std::io;
use std::time::{Duration, Instant};

pub use amacs::protocol::edit::{AwaitEdit, AwaitKey, EditOk, EditOutcome, EditRequest, Ticket};

/// The connect phase of either exchange: a running aterm answers at once.
const CONNECT_WAIT: Duration = Duration::from_secs(10);

/// How long `edit` waits for its answer once sent: past the server's own
/// admission deadline (30 s, then `ERR timeout ticket=<t>`), so the server's
/// answer always comes first.
const EDIT_REPLY_WAIT: Duration = Duration::from_secs(45);

/// What one `await edit` leg asks for: the server's bound.
const LEG: Duration = crate::await_task::LEG_MAX;

/// Between two looks for a successor after a leg was cut.
const REDIAL_POLL: Duration = Duration::from_millis(250);

/// Why `edit` opened no view.
#[derive(Debug)]
pub enum EditErr {
    /// The window answered `ERR …` (the whole line): the request was refused
    /// and nothing opened.
    Refused(String),
    /// The window answered `ERR timeout ticket=<t>`: the admission outran the
    /// server's deadline, the ticket is cancelled and a late view is dropped.
    TimedOut(String),
    /// No window could be asked (no instance, a dead socket, a connect phase
    /// nobody served): the request was not sent.
    Unreachable(io::Error),
    /// Request delivery began but its outcome is unknown (a failed send or
    /// a missing reply): the window may still open the view.
    NoAnswer(io::Error),
    /// The window answered a line this client cannot read: it may have opened
    /// the view.
    Unreadable(String),
    /// The request cannot be written as one wire line (a field holding
    /// whitespace, a bad line number); it was not sent.
    Unsendable(String),
}

impl EditErr {
    /// Whether the client edits in its own terminal instead (design §6.1): on
    /// an `ERR` the window answered, or when no window could be asked. A
    /// request the window took without answering (its deadline, or ours)
    /// never does: the view may still open, and two editors would race on
    /// one file.
    #[must_use]
    pub fn falls_back_to_terminal(&self) -> bool {
        match self {
            Self::Refused(_) | Self::Unreachable(_) | Self::Unsendable(_) => true,
            Self::TimedOut(_) | Self::NoAnswer(_) | Self::Unreadable(_) => false,
        }
    }
}

impl std::fmt::Display for EditErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(line) | Self::TimedOut(line) => f.write_str(line),
            Self::Unreachable(e) => write!(f, "no aterm window could be asked ({e})"),
            Self::NoAnswer(e) => write!(f, "the aterm window did not answer ({e})"),
            Self::Unreadable(line) => write!(f, "the aterm window answered {line:?}"),
            Self::Unsendable(why) => write!(f, "the request cannot be sent ({why})"),
        }
    }
}

impl std::error::Error for EditErr {}

/// Why `await edit` ended without the edit's outcome.
#[derive(Debug, PartialEq, Eq)]
pub enum AwaitErr {
    /// The window answered `ERR …` (the whole line).
    Refused(String),
    /// The window answered a line this client cannot read.
    Unreadable(String),
    /// The server went away and nothing took the wait again within
    /// [`crate::redial::SUCCESSOR_BOUND`]; the last thing heard is kept.
    Replaced(String),
    /// The session cannot be framed as one stable selector; no resolution
    /// or request was attempted.
    Unsendable(String),
}

impl std::fmt::Display for AwaitErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(line) => f.write_str(line),
            Self::Unreadable(line) => write!(f, "the aterm window answered {line:?}"),
            Self::Unsendable(why) => write!(f, "the request cannot be sent ({why})"),
            Self::Replaced(last) => write!(
                f,
                "the aterm serving the edit went away and no successor took the wait within \
                 {}s ({last})",
                crate::redial::SUCCESSOR_BOUND.as_secs()
            ),
        }
    }
}

impl std::error::Error for AwaitErr {}

/// One exchange's transport and the clock a re-dial is timed on.
trait Wire {
    /// The socket of the instance hosting the session, resolved now.
    fn resolve(&mut self) -> io::Result<String>;
    /// Send `request` (one framed line) to `sock` and read the status line it
    /// answers, waiting `reply` for it once sent.
    fn send(&mut self, sock: &str, request: &str, reply: Duration) -> io::Result<String>;
    /// Wait between two looks for a successor.
    fn pause(&mut self, wait: Duration);
    fn now(&mut self) -> Instant;
}

/// The control socket of the instance hosting session `sid`.
struct SocketWire<'a> {
    sid: &'a str,
}

impl Wire for SocketWire<'_> {
    fn resolve(&mut self) -> io::Result<String> {
        crate::resolve_sock_for(Some(self.sid))
    }

    fn send(&mut self, sock: &str, request: &str, reply: Duration) -> io::Result<String> {
        crate::front_door_send_within(sock, request, CONNECT_WAIT, reply)
    }

    fn pause(&mut self, wait: Duration) {
        std::thread::sleep(wait);
    }

    fn now(&mut self) -> Instant {
        Instant::now()
    }
}

/// `@<sid> edit <request>` to the window hosting session `sid`
/// (`$ATERM_PARENT_SESSION_ID`, `s-<hex>`): the installed view's ticket, or
/// why none opened.
///
/// # Errors
///
/// [`EditErr`]: what the window answered, or why it was not asked.
pub fn edit(sid: &str, request: &EditRequest) -> Result<EditOk, EditErr> {
    edit_over(&mut SocketWire { sid }, sid, request)
}

fn edit_over(wire: &mut dyn Wire, sid: &str, request: &EditRequest) -> Result<EditOk, EditErr> {
    let selector = edit_selector(sid).map_err(EditErr::Unsendable)?;
    let args = request
        .format()
        .map_err(|e| EditErr::Unsendable(e.to_string()))?;
    let sock = wire.resolve().map_err(EditErr::Unreachable)?;
    let line = format!("{selector} edit {args}\n");
    match wire.send(&sock, &line, EDIT_REPLY_WAIT) {
        Ok(reply) => edit_reply(&reply),
        Err(e) if crate::front_door_delivered(&e) => Err(EditErr::NoAnswer(e)),
        Err(e) => Err(EditErr::Unreachable(e)),
    }
}

/// Frame only the stable session selector the resolver and redial follow.
/// Reuse their `s-<hex>` grammar, including short authored fixture IDs; local,
/// self, list and whitespace-bearing selectors cannot name this client tab.
fn edit_selector(sid: &str) -> Result<String, String> {
    let selector = format!("@{sid}");
    if crate::redial::target_sid(&selector).is_some_and(|named| named.as_str() == sid) {
        Ok(selector)
    } else {
        Err("expected an s-<hex> session id".to_string())
    }
}

/// What an `edit` answer means.
fn edit_reply(reply: &str) -> Result<EditOk, EditErr> {
    if reply.starts_with("OK edit ") {
        return EditOk::parse(reply).map_err(|_| EditErr::Unreadable(reply.to_string()));
    }
    if reply.starts_with("ERR timeout ticket=") {
        return Err(EditErr::TimedOut(reply.to_string()));
    }
    if reply == "ERR" || reply.starts_with("ERR ") {
        return Err(EditErr::Refused(reply.to_string()));
    }
    Err(EditErr::Unreadable(reply.to_string()))
}

/// `@<sid> await edit <key>`, re-armed until the edit's outcome ([the
/// module](self)). `uri` is the file the edit opened (`OK edit`'s canonical
/// `uri=`), which a re-dial waits on.
///
/// # Errors
///
/// [`AwaitErr`]: an unframeable session, a refusal, an unreadable answer, or
/// a server gone with no successor.
pub fn await_edit(sid: &str, key: AwaitKey, uri: &str) -> Result<EditOutcome, AwaitErr> {
    await_over(&mut SocketWire { sid }, sid, key, uri)
}

/// How one `await edit` leg ended.
enum Leg {
    /// The edit's outcome.
    Done(EditOutcome),
    /// `OK timeout`: re-arm the same key.
    TimedOut,
    /// `ERR unknown ticket` to a ticket: the server lost it, re-arm by file.
    UnknownTicket,
    /// The server is going away, or went: re-dial.
    Cut(String),
    /// A refusal or an answer that ends the wait.
    End(AwaitErr),
}

fn await_over(
    wire: &mut dyn Wire,
    sid: &str,
    mut key: AwaitKey,
    uri: &str,
) -> Result<EditOutcome, AwaitErr> {
    let selector = edit_selector(sid).map_err(AwaitErr::Unsendable)?;
    let mut sock = wire.resolve();
    // Since when the wait has had no server to take it.
    let mut cut_since: Option<Instant> = None;
    loop {
        let request = AwaitEdit {
            key: key.clone(),
            timeout_ms: Some(u64::try_from(LEG.as_millis()).unwrap_or(u64::MAX)),
        }
        .format()
        .map_err(|e| AwaitErr::Unreadable(e.to_string()))?;
        let request = format!("{selector} await {request}\n");
        let leg = match &sock {
            Ok(sock) => match wire.send(sock, &request, LEG + crate::await_task::LEG_SLACK) {
                Ok(line) => await_reply(&line, &key),
                Err(e) => Leg::Cut(e.to_string()),
            },
            Err(e) => Leg::Cut(e.to_string()),
        };
        let last = match leg {
            Leg::Done(outcome) => return Ok(outcome),
            Leg::TimedOut => {
                cut_since = None;
                continue;
            }
            Leg::UnknownTicket => {
                key = AwaitKey::Uri(uri.to_string());
                continue;
            }
            // While re-dialling, a refusal is an instance that does not host
            // the session yet (`ERR no such session`): look again.
            Leg::End(AwaitErr::Refused(line)) if cut_since.is_some() => line,
            Leg::End(end) => return Err(end),
            Leg::Cut(why) => why,
        };
        let since = *cut_since.get_or_insert_with(|| wire.now());
        key = AwaitKey::Uri(uri.to_string());
        if wire.now().saturating_duration_since(since) >= crate::redial::SUCCESSOR_BOUND {
            return Err(AwaitErr::Replaced(last));
        }
        wire.pause(REDIAL_POLL);
        sock = wire.resolve();
    }
}

/// What one `await edit` answer means for the wait on `key`.
fn await_reply(line: &str, key: &AwaitKey) -> Leg {
    if crate::reply_is_timeout(line) {
        return Leg::TimedOut;
    }
    if line.starts_with("OK edit done ") {
        return match EditOutcome::parse(line) {
            Ok(outcome) => Leg::Done(outcome),
            Err(_) => Leg::End(AwaitErr::Unreadable(line.to_string())),
        };
    }
    if line == "ERR unknown ticket" && matches!(key, AwaitKey::Ticket(_)) {
        return Leg::UnknownTicket;
    }
    // The server's own ways of saying it is going away (its event loop is
    // gone, its main thread dropped the waiter unanswered, the host behind a
    // forward went) or cannot take the leg now (every lane busy, a token a
    // successor's listener does not know yet): the wait was not refused.
    if line == "ERR event loop gone"
        || line == "ERR edit waiter was dropped"
        || aterm_types::control_verbs::host_gone(line).is_some()
        || crate::front_door_refused(line)
    {
        return Leg::Cut(line.to_string());
    }
    if line == "ERR" || line.starts_with("ERR ") {
        return Leg::End(AwaitErr::Refused(line.to_string()));
    }
    Leg::End(AwaitErr::Unreadable(line.to_string()))
}

#[cfg(test)]
#[path = "edit_tests.rs"]
mod tests;
