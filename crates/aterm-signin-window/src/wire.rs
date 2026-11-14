// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lines the window's host and its opener exchange: one JSON object per line.
//!
//! The opener's first line says what to open ([`Open`]: the window's title, the account's
//! data store and the hosts its page may go to), so nothing of the account is in the host's
//! arguments, which every process on the Mac can read. After it the opener writes a
//! [`Request`] per line to the host's standard input; the host answers each with one reply
//! carrying the request's `id` ([`reply_done`], [`reply_value`], [`reply_error`]), and writes
//! an event line, which carries no `id`, as things happen ([`event`]). A script's answer, and
//! so a reply, may hold what the page holds: the lines travel only on the private pipes
//! between the two aterm processes and are written nowhere else.

use aterm_json::{Value, json};

/// The host says it has opened the window; `store` says whether the page's data persists.
pub const READY: &str = "ready";
/// The window's page finished a navigation (the aside page says nothing of its own).
pub const LOADED: &str = "loaded";
/// A navigation failed; `code` is its `NSError` code.
pub const LOAD_FAILED: &str = "load-failed";
/// A person closed the window.
pub const CLOSED: &str = "closed";
/// This process has no desktop session; no window was made.
pub const NO_DESKTOP: &str = "no-desktop";
/// The window's page, or its aside page, was sent somewhere outside the hosts its [`Open`]
/// names, and did not go.
pub const REFUSED: &str = "navigation-refused";

/// What the opener's first line asks the host to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open {
    /// The window's title.
    pub title: String,
    /// The account's data store, a UUID; none for a page whose data is not kept.
    pub store: Option<String>,
    /// The hosts a page of the window may be sent to, over `https`: a navigation of the whole
    /// page to any other is refused ([`REFUSED`]). A frame inside a page goes where its page
    /// sends it.
    pub hosts: Vec<String>,
    /// The port of this Mac's own loopback address the page may be sent to over `http`: the
    /// waiting sign-in's listener. None where the sign-in has none.
    pub loopback: Option<u16>,
}

/// `open` as the opener's first line, without the newline.
#[must_use]
pub fn open_line(open: &Open) -> String {
    let hosts: Vec<Value> = open.hosts.iter().cloned().map(Value::String).collect();
    aterm_json::to_string(&json!({
        "op": "open",
        "title": open.title,
        "store": open.store,
        "hosts": Value::Array(hosts),
        "loopback": open.loopback,
    }))
    .unwrap_or_default()
}

/// The [`Open`] the opener's first line holds.
///
/// # Errors
/// Why the line is none.
pub fn parse_open(line: &str) -> Result<Open, &'static str> {
    let value: Value = aterm_json::from_str(line).map_err(|_| "the line is not JSON")?;
    if value.get("op").and_then(Value::as_str) != Some("open") {
        return Err("the first line does not say what to open");
    }
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .ok_or("the window has no title")?
        .to_owned();
    let store = match value.get("store") {
        None | Some(Value::Null) => None,
        Some(Value::String(store)) => Some(store.clone()),
        Some(_) => return Err("the store is not a name"),
    };
    let hosts = match value.get("hosts") {
        Some(Value::Array(hosts)) => hosts
            .iter()
            .map(|host| host.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or("a host is not a name")?,
        _ => return Err("the window names no hosts its page may go to"),
    };
    let loopback = match value.get("loopback") {
        None | Some(Value::Null) => None,
        Some(port) => Some(
            port.as_i64()
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port != 0)
                .ok_or("the loopback port is not a port")?,
        ),
    };
    Ok(Open {
        title,
        store,
        hosts,
        loopback,
    })
}

/// What the opener asks of the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Load `url` in the window's page, or in its aside page.
    Load {
        /// The reply's id.
        id: u64,
        /// The address.
        url: String,
        /// Whether the aside page loads it: a second page on the same data store that is
        /// not shown and leaves the window's page as it stands.
        aside: bool,
    },
    /// Evaluate `script` in the page and answer the string it returns.
    Eval {
        /// The reply's id.
        id: u64,
        /// The script, an expression that returns a string or nothing.
        script: String,
        /// Whether the aside page evaluates it.
        aside: bool,
    },
    /// Remove everything the account's data store holds (its cookies, its pages' storage),
    /// so the next page in it starts signed out. Nothing of any other store is touched.
    Forget {
        /// The reply's id.
        id: u64,
    },
    /// Close the window and end the host.
    Close {
        /// The reply's id.
        id: u64,
    },
}

/// A line's `id`, where it holds one.
fn id_of(value: &Value) -> Option<u64> {
    value
        .get("id")
        .and_then(Value::as_i64)
        .and_then(|id| u64::try_from(id).ok())
}

/// The request one line holds.
///
/// # Errors
/// Why the line is no request.
pub fn parse_request(line: &str) -> Result<Request, &'static str> {
    let value: Value = aterm_json::from_str(line).map_err(|_| "the line is not JSON")?;
    let id = id_of(&value).ok_or("the request has no id")?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or("the request lacks a field of its operation")
    };
    let aside = match value.get("aside") {
        None => Ok(false),
        Some(Value::Bool(aside)) => Ok(*aside),
        Some(_) => Err("the request's aside is not yes or no"),
    };
    match value.get("op").and_then(Value::as_str) {
        Some("load") => Ok(Request::Load {
            id,
            url: text("url")?,
            aside: aside?,
        }),
        Some("eval") => Ok(Request::Eval {
            id,
            script: text("script")?,
            aside: aside?,
        }),
        Some("forget") => Ok(Request::Forget { id }),
        Some("close") => Ok(Request::Close { id }),
        _ => Err("the request names no operation the window has"),
    }
}

/// `request` as its line, without the newline.
#[must_use]
pub fn request_line(request: &Request) -> String {
    let value = match request {
        Request::Load { id, url, aside } => {
            json!({"id": id, "op": "load", "url": url, "aside": aside})
        }
        Request::Eval { id, script, aside } => {
            json!({"id": id, "op": "eval", "script": script, "aside": aside})
        }
        Request::Forget { id } => json!({"id": id, "op": "forget"}),
        Request::Close { id } => json!({"id": id, "op": "close"}),
    };
    aterm_json::to_string(&value).unwrap_or_default()
}

/// The reply to a request that was carried out and answers nothing.
#[must_use]
pub fn reply_done(id: u64) -> String {
    aterm_json::to_string(&json!({"id": id, "ok": true})).unwrap_or_default()
}

/// The reply to an evaluation: the string the script returned (`None` where it returned
/// anything else) and the address the page is at, as the web view itself says it.
#[must_use]
pub fn reply_value(id: u64, value: Option<&str>, url: &str) -> String {
    aterm_json::to_string(&json!({"id": id, "ok": true, "value": value, "url": url}))
        .unwrap_or_default()
}

/// The reply to a request that was not carried out: why.
#[must_use]
pub fn reply_error(id: u64, why: &str) -> String {
    aterm_json::to_string(&json!({"id": id, "ok": false, "error": why})).unwrap_or_default()
}

/// The event `name`'s line.
#[must_use]
pub fn event(name: &str) -> String {
    aterm_json::to_string(&json!({"event": name})).unwrap_or_default()
}

/// The [`READY`] event: whether the page's data persists between windows (`persistent`).
#[must_use]
pub fn ready(persistent: bool) -> String {
    let store = if persistent {
        "persistent"
    } else {
        "ephemeral"
    };
    aterm_json::to_string(&json!({"event": READY, "store": store})).unwrap_or_default()
}

/// The [`LOAD_FAILED`] event with the failure's `NSError` code.
#[must_use]
pub fn load_failed(code: i64) -> String {
    aterm_json::to_string(&json!({"event": LOAD_FAILED, "code": code})).unwrap_or_default()
}

/// One line the host wrote, as the opener reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// A reply to the request with this id.
    Reply {
        /// The request's id.
        id: u64,
        /// What it answered: the value and address of an evaluation (both `None` for any
        /// other request), or why it was not carried out.
        answer: Result<(Option<String>, Option<String>), String>,
    },
    /// An event: its name, and a load failure's code.
    Event {
        /// [`READY`], [`LOADED`], [`LOAD_FAILED`], [`CLOSED`], [`NO_DESKTOP`] or [`REFUSED`].
        name: String,
        /// A load failure's `NSError` code.
        code: Option<i64>,
        /// Whether a [`READY`] window's data persists.
        persistent: bool,
    },
}

/// The reply or event one line of the host's holds.
///
/// # Errors
/// Why the line is neither.
pub fn parse_line(line: &str) -> Result<Line, &'static str> {
    let value: Value = aterm_json::from_str(line).map_err(|_| "the line is not JSON")?;
    if let Some(name) = value.get("event").and_then(Value::as_str) {
        return Ok(Line::Event {
            name: name.to_owned(),
            code: value.get("code").and_then(Value::as_i64),
            persistent: value.get("store").and_then(Value::as_str) == Some("persistent"),
        });
    }
    let id = id_of(&value).ok_or("the line is neither a reply nor an event")?;
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let answer = match value.get("ok") {
        Some(Value::Bool(true)) => Ok((text("value"), text("url"))),
        Some(Value::Bool(false)) => Err(text("error").unwrap_or_default()),
        _ => return Err("the reply does not say whether it was carried out"),
    };
    Ok(Line::Reply { id, answer })
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
