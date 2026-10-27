// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Client `edit` and `await edit` exchanges.

use std::io;
use std::time::{Duration, Instant};

pub use amacs_protocol::edit::{AwaitEdit, AwaitKey, EditOk, EditOutcome, EditRequest, Ticket};

const CONNECT_WAIT: Duration = Duration::from_secs(10);

const EDIT_REPLY_WAIT: Duration = Duration::from_secs(45);

const LEG: Duration = crate::await_task::LEG_MAX;

const REDIAL_POLL: Duration = Duration::from_millis(250);

/// Why `edit` opened no view.
#[derive(Debug)]
pub enum EditErr {
    /// The window answered `ERR …` and opened nothing.
    Refused(String),
    /// The window answered `ERR timeout ticket=<t>`.
    TimedOut(String),
    /// No window could be asked.
    Unreachable(io::Error),
    /// Request delivery began, but its outcome is unknown.
    NoAnswer(io::Error),
    /// The window answered an unreadable line.
    Unreadable(String),
    /// The request cannot be formatted as a single wire line.
    Unsendable(String),
}

impl EditErr {
    /// Whether the client edits in its own terminal instead.
    #[must_use]
    pub const fn falls_back_to_terminal(&self) -> bool {
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
    /// The window answered `ERR …`.
    Refused(String),
    /// The window answered an unreadable line.
    Unreadable(String),
    /// The server disconnected and no successor resumed within [`crate::redial::SUCCESSOR_BOUND`].
    Replaced(String),
    /// The session cannot be framed as a stable selector.
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

trait Wire {
    fn resolve(&mut self) -> io::Result<String>;
    fn send(&mut self, sock: &str, request: &str, reply: Duration) -> io::Result<String>;
    fn pause(&mut self, wait: Duration);
    fn now(&mut self) -> Instant;
}

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

/// Send `@<sid> edit <request>` to the window hosting session `sid` (`$ATERM_PARENT_SESSION_ID`).
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

fn edit_selector(sid: &str) -> Result<String, String> {
    let selector = format!("@{sid}");
    if crate::redial::target_sid(&selector).is_some_and(|named| named.as_str() == sid) {
        Ok(selector)
    } else {
        Err("expected an s-<hex> session id".to_string())
    }
}

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

/// Run `@<sid> await edit <key>`, re-armed until the edit's outcome.
/// # Errors
///
/// [`AwaitErr`]: an unframeable session, a refusal, an unreadable answer, or
/// a server gone with no successor.
pub fn await_edit(sid: &str, key: AwaitKey, uri: &str) -> Result<EditOutcome, AwaitErr> {
    await_over(&mut SocketWire { sid }, sid, key, uri)
}

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
