// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lane handlers for the `edit` and `await edit` control verbs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use amacs::protocol::edit::{AwaitEdit, AwaitKey, EditRequest, Ticket};
use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;
use crate::control::Scope;
use crate::control::control_media::{MainPost, Waited, park_for_answer};

pub(crate) mod hold;

/// Maximum wait for an `edit` view to be installed before cancelling its ticket.
pub(crate) const EDIT_ADMISSION_DEADLINE: Duration = Duration::from_secs(30);

/// One `await edit`'s name on the main thread, so its withdraw finds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WaiterId(pub(crate) u64);

/// An `edit` request posted to the main thread with its reply channel.
#[derive(Debug)]
pub(crate) struct EditCall {
    pub(crate) ticket: Ticket,
    /// The session the view opens beside (`@<sid>`, resolved).
    pub(crate) session: u64,
    pub(crate) request: EditRequest,
    /// The request's file in its realpath spelling, when it resolves.
    pub(crate) real_uri: Option<String>,
    pub(crate) reply: Sender<String>,
}

/// An `await edit` request posted to the main thread with its outcome channel.
#[derive(Debug)]
pub(crate) struct AwaitCall {
    pub(crate) waiter: WaiterId,
    pub(crate) key: AwaitKey,
    /// A `uri=` key's file in its realpath spelling, when it resolves.
    pub(crate) real_uri: Option<String>,
    pub(crate) reply: Sender<String>,
}

/// Resolves the canonical `file://` URI for `uri` on the filesystem.
pub(crate) fn real_file_uri(uri: &str) -> Option<String> {
    let path = amacs::protocol::uri::file_uri_to_path(uri).ok()?;
    let real = std::fs::canonicalize(path).ok()?;
    amacs::protocol::uri::path_to_file_uri(&real).ok()
}

/// Mints the next edit ticket scoped to this process ID.
pub(crate) fn mint_ticket() -> Ticket {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let low = NEXT.fetch_add(1, Ordering::Relaxed) & 0xffff_ffff;
    Ticket(u64::from(std::process::id()) << 32 | low)
}

fn mint_waiter() -> WaiterId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    WaiterId(NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Handles `@<sid> edit <file-uri> [line=<L>[:<C>]] [create=1] [for=<word>] [wait=0]`.
pub(crate) fn cmd_edit(
    proxy: &EventLoopProxy<Wake>,
    scope: Scope,
    session: Option<u64>,
    rest: &str,
) -> String {
    if let Some(refusal) = crate::control::control_media::main_thread_refusal() {
        return format!("ERR {refusal}\n");
    }
    serve_edit(
        scope,
        session,
        rest,
        &mut MainPost::new(proxy),
        &mut crate::control::caller_hung_up,
        EDIT_ADMISSION_DEADLINE,
        crate::control::hangup_park,
    )
}

/// Runs [`cmd_edit`]'s lane logic past the initial main-thread stall check.
pub(crate) fn serve_edit(
    scope: Scope,
    session: Option<u64>,
    rest: &str,
    post: &mut MainPost<'_>,
    hung_up: &mut dyn FnMut() -> bool,
    deadline: Duration,
    park: fn(Duration) -> Duration,
) -> String {
    if scope != Scope::Owner {
        return "ERR scope\n".to_string();
    }
    let Some(session) = session else {
        return format!(
            "ERR usage: @<sid> {}: the view opens beside that session's tab\n",
            amacs::protocol::edit::EDIT_USAGE
        );
    };
    let request = match EditRequest::parse(rest) {
        Ok(request) => request,
        Err(error) => return format!("ERR {error}\n"),
    };
    if let Err(error) = amacs::protocol::uri::file_uri_to_path(&request.uri) {
        return format!("ERR refused bad-uri ({error})\n");
    }
    let ticket = mint_ticket();
    let real_uri = real_file_uri(&request.uri);
    let (answer, _hop) = match post.ask(|reply| {
        Wake::EditRequest(Box::new(EditCall {
            ticket,
            session,
            request,
            real_uri,
            reply,
        }))
    }) {
        Ok(posted) => posted,
        Err(error) => return format!("ERR {error}\n"),
    };
    let started = Instant::now();
    let waited = park_for_answer(&answer, started + deadline, hung_up, park);
    match waited {
        Waited::Answer(line) => line,
        Waited::HungUp => {
            post.tell(Wake::EditCancel { ticket });
            crate::control::HUNG_UP_REPLY.to_string()
        }
        Waited::Deadline => {
            post.tell(Wake::EditCancel { ticket });
            format!("ERR timeout ticket={ticket}\n")
        }
        Waited::Dropped => format!("ERR edit ticket={ticket} was dropped\n"),
    }
}

/// Handles `await edit ticket=<t>|uri=<file-uri> [timeout=<ms>]`.
pub(crate) fn await_edit(proxy: &EventLoopProxy<Wake>, scope: Scope, rest: &str) -> String {
    serve_await_edit(
        scope,
        rest,
        &mut MainPost::new(proxy),
        &mut crate::control::caller_hung_up,
        crate::control::hangup_park,
    )
}

/// Runs [`await_edit`]'s lane logic.
pub(crate) fn serve_await_edit(
    scope: Scope,
    rest: &str,
    post: &mut MainPost<'_>,
    hung_up: &mut dyn FnMut() -> bool,
    park: fn(Duration) -> Duration,
) -> String {
    if scope != Scope::Owner {
        return "ERR scope\n".to_string();
    }
    let wait = match AwaitEdit::parse(rest) {
        Ok(wait) => wait,
        Err(error) => return format!("ERR {error}\n"),
    };
    let real_uri = match &wait.key {
        AwaitKey::Uri(uri) => real_file_uri(uri),
        AwaitKey::Ticket(_) => None,
    };
    let timeout = Duration::from_millis(aterm_control::wire::resolve_wait_timeout_ms(
        wait.timeout_ms,
    ));
    let waiter = mint_waiter();
    let (answer, _hop) = match post.ask(|reply| {
        Wake::EditAwait(Box::new(AwaitCall {
            waiter,
            key: wait.key,
            real_uri,
            reply,
        }))
    }) {
        Ok(posted) => posted,
        Err(error) => return format!("ERR {error}\n"),
    };
    let now = Instant::now();
    let deadline = now
        .checked_add(timeout)
        .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 3600));
    let waited = park_for_answer(&answer, deadline, hung_up, park);
    post.tell(Wake::EditAwaitWithdraw { waiter });
    match waited {
        Waited::Answer(line) => line,
        Waited::HungUp => crate::control::HUNG_UP_REPLY.to_string(),
        Waited::Deadline => "OK timeout\n".to_string(),
        Waited::Dropped => "ERR edit waiter was dropped\n".to_string(),
    }
}

#[cfg(test)]
#[path = "control_edit_tests.rs"]
mod tests;
