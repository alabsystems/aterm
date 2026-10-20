// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `edit` and `await edit` control verbs' lane handlers (amacs design
//! §6.2-§6.4; slice N1).
//!
//! `@s-<sid> edit <file-uri> …` asks the window hosting `<sid>` for a fresh
//! Editor view of the file beside `<sid>`'s tab; `await edit ticket=<t>` waits
//! until that view is gone. Both run on a wait lane and park on a one-shot
//! channel the main thread answers (`App::begin_edit`, `App::await_edit` in
//! `app_edit`), in [`crate::control::hangup_park`] slices so a killed client
//! frees the lane within [`crate::control::HANGUP_POLL`], as `askpass` does
//! (`control_askpass`). Every exit of `await edit` withdraws its waiter, and
//! an `edit` that stops waiting cancels its ticket, so a late admission is
//! dropped rather than opening a view nobody asked for any more.
//!
//! The lane also resolves the file's realpath spelling (`/tmp` is
//! `/private/tmp` on macOS), as an admission's grant names it: that reads the
//! file system, which may wait on a network volume, so it happens here and not
//! on the main thread.
//!
//! The lane logic is written against injected `post`/`hung_up` closures: no
//! event-loop proxy can be built in a unit test.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use amacs::protocol::edit::{AwaitEdit, AwaitKey, EditRequest, Ticket};
use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;
use crate::control::Scope;

pub(crate) mod hold;

/// How long `edit` waits for its view to be installed: the main-thread RPC
/// deadline (`control_media::call_main`), after which the ticket is cancelled.
pub(crate) const EDIT_ADMISSION_DEADLINE: Duration = Duration::from_secs(30);

/// One `await edit`'s name on the main thread, so its withdraw finds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WaiterId(pub(crate) u64);

/// An `edit` request, posted to the main thread with the channel its one
/// reply line goes back on.
#[derive(Debug)]
pub(crate) struct EditCall {
    pub(crate) ticket: Ticket,
    /// The session the view opens beside (`@<sid>`, resolved).
    pub(crate) session: u64,
    pub(crate) request: EditRequest,
    /// The request's file in its realpath spelling, when it resolves
    /// ([`real_file_uri`]).
    pub(crate) real_uri: Option<String>,
    pub(crate) reply: Sender<String>,
}

/// An `await edit`, posted to the main thread with the channel its outcome
/// line goes back on.
#[derive(Debug)]
pub(crate) struct AwaitCall {
    pub(crate) waiter: WaiterId,
    pub(crate) key: AwaitKey,
    /// A `uri=` key's file in its realpath spelling, when it resolves
    /// ([`real_file_uri`]).
    pub(crate) real_uri: Option<String>,
    pub(crate) reply: Sender<String>,
}

/// The realpath spelling of `uri`'s file (`file:///tmp/x` is
/// `file:///private/tmp/x` on macOS), as the document store keys it; `None`
/// when `uri` names no local file that resolves. Reads the file system: call it
/// on a lane, never on the main thread.
pub(crate) fn real_file_uri(uri: &str) -> Option<String> {
    let path = amacs::protocol::uri::file_uri_to_path(uri).ok()?;
    let real = std::fs::canonicalize(path).ok()?;
    amacs::protocol::uri::path_to_file_uri(&real).ok()
}

/// The next edit ticket. The high half is this process's id, so a ticket a
/// client kept across a seamless update never names one its successor minted
/// (the successor answers it `ERR unknown ticket`; the client re-arms by
/// `uri=`).
pub(crate) fn mint_ticket() -> Ticket {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let low = NEXT.fetch_add(1, Ordering::Relaxed) & 0xffff_ffff;
    Ticket(u64::from(std::process::id()) << 32 | low)
}

fn mint_waiter() -> WaiterId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    WaiterId(NEXT.fetch_add(1, Ordering::Relaxed))
}

/// `@<sid> edit <file-uri> [line=<L>[:<C>]] [create=1] [for=<word>]`.
/// `session` is the resolved `@<sid>`; `None` on the bare App lane, which has
/// no tab to open beside.
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
        &mut |wake| proxy.send_event(wake).is_ok(),
        &mut crate::control::caller_hung_up,
        EDIT_ADMISSION_DEADLINE,
        crate::control::hangup_park,
    )
}

/// [`cmd_edit`]'s lane logic. `post` hands a `Wake` to the main thread and
/// says whether it was taken; `park` bounds one wait slice.
pub(crate) fn serve_edit(
    scope: Scope,
    session: Option<u64>,
    rest: &str,
    post: &mut dyn FnMut(Wake) -> bool,
    hung_up: &mut dyn FnMut() -> bool,
    deadline: Duration,
    park: fn(Duration) -> Duration,
) -> String {
    // Owner exactly: every in-session client holds the Owner token, and the
    // client falls back to editing in its own terminal on any other answer.
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
    let (reply, answer) = std::sync::mpsc::channel();
    let real_uri = real_file_uri(&request.uri);
    let call = EditCall {
        ticket,
        session,
        request,
        real_uri,
        reply,
    };
    if !post(Wake::EditRequest(Box::new(call))) {
        return "ERR event loop gone\n".to_string();
    }
    let started = Instant::now();
    let waited = park_for_answer(&answer, started + deadline, hung_up, park);
    match waited {
        Waited::Answer(line) => line,
        Waited::HungUp => {
            post(Wake::EditCancel { ticket });
            crate::control::HUNG_UP_REPLY.to_string()
        }
        Waited::Deadline => {
            post(Wake::EditCancel { ticket });
            format!("ERR timeout ticket={ticket}\n")
        }
        Waited::Dropped => format!("ERR edit ticket={ticket} was dropped\n"),
    }
}

/// `await edit ticket=<t>|uri=<file-uri> [timeout=<ms>]` (`rest` starts at
/// `edit`).
pub(crate) fn await_edit(proxy: &EventLoopProxy<Wake>, scope: Scope, rest: &str) -> String {
    serve_await_edit(
        scope,
        rest,
        &mut |wake| proxy.send_event(wake).is_ok(),
        &mut crate::control::caller_hung_up,
        crate::control::hangup_park,
    )
}

/// [`await_edit`]'s lane logic; see [`serve_edit`] for the closures.
pub(crate) fn serve_await_edit(
    scope: Scope,
    rest: &str,
    post: &mut dyn FnMut(Wake) -> bool,
    hung_up: &mut dyn FnMut() -> bool,
    park: fn(Duration) -> Duration,
) -> String {
    // Owner exactly, as for `edit`: a waiter reads any ticket's outcome and
    // adopts it (its hold stands again), which is the asking program's power,
    // not an observer's.
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
    let (reply, answer) = std::sync::mpsc::channel();
    let call = AwaitCall {
        waiter,
        key: wait.key,
        real_uri,
        reply,
    };
    if !post(Wake::EditAwait(Box::new(call))) {
        return "ERR event loop gone\n".to_string();
    }
    let waited = park_for_answer(&answer, Instant::now() + timeout, hung_up, park);
    // Every exit withdraws: an answered waiter is already gone, and a timed-out
    // or hung-up one starts its ticket's orphan clock.
    post(Wake::EditAwaitWithdraw { waiter });
    match waited {
        Waited::Answer(line) => line,
        Waited::HungUp => crate::control::HUNG_UP_REPLY.to_string(),
        Waited::Deadline => "OK timeout\n".to_string(),
        Waited::Dropped => "ERR edit waiter was dropped\n".to_string(),
    }
}

/// How a parked lane stopped waiting.
#[derive(Debug, PartialEq, Eq)]
enum Waited {
    Answer(String),
    HungUp,
    Deadline,
    /// The main thread dropped the channel unanswered (the event loop ended).
    Dropped,
}

/// Wait on `answer` until `deadline`, re-checking `hung_up` between slices.
fn park_for_answer(
    answer: &Receiver<String>,
    deadline: Instant,
    hung_up: &mut dyn FnMut() -> bool,
    park: fn(Duration) -> Duration,
) -> Waited {
    loop {
        if hung_up() {
            return Waited::HungUp;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match answer.recv_timeout(park(left)) {
            Ok(line) => return Waited::Answer(line),
            Err(RecvTimeoutError::Disconnected) => return Waited::Dropped,
            Err(RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    return Waited::Deadline;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One park slice of at most 5 ms: these tests never wait out a real bound.
    fn short(left: Duration) -> Duration {
        left.min(Duration::from_millis(5))
    }

    fn never() -> bool {
        false
    }

    #[test]
    fn edit_refuses_before_it_posts_anything() {
        let mut posted = Vec::new();
        let mut post = |wake: Wake| {
            posted.push(format!("{wake:?}"));
            true
        };
        let uri = "file:///tmp/x.txt";
        let ask = |scope, session, rest: &str, post: &mut dyn FnMut(Wake) -> bool| {
            serve_edit(
                scope,
                session,
                rest,
                post,
                &mut never,
                Duration::ZERO,
                short,
            )
        };
        let edge = Scope::Edge(aterm_session::EdgeToken::from_bytes([7; 32]));
        assert_eq!(ask(edge, Some(1), uri, &mut post), "ERR scope\n");
        for (session, rest, refusal) in [
            (None, uri, "ERR usage: @<sid> edit "),
            (Some(1), "", "ERR usage: edit "),
            (Some(1), "file:///x line=0", "ERR bad line=0"),
            (Some(1), "file://host/x", "ERR refused bad-uri"),
        ] {
            let line = ask(Scope::Owner, session, rest, &mut post);
            assert!(line.starts_with(refusal), "{rest:?}: {line}");
        }
        assert!(posted.is_empty(), "{posted:?}");
    }

    #[test]
    fn edit_answers_what_the_main_thread_answers() {
        let mut post = |wake: Wake| {
            if let Wake::EditRequest(call) = wake {
                assert_eq!(call.session, 9);
                assert_eq!(call.request.line, Some((3, None)));
                let line = format!("OK edit ticket={} view=4 doc=1 uri=x\n", call.ticket);
                call.reply.send(line).unwrap();
            }
            true
        };
        let line = serve_edit(
            Scope::Owner,
            Some(9),
            "file:///tmp/x.txt line=3",
            &mut post,
            &mut never,
            Duration::from_secs(60),
            short,
        );
        assert!(line.starts_with("OK edit ticket="), "{line}");
    }

    /// The `call_main` deadline: the ticket is cancelled, and the reply names
    /// it so a log can match the late admission the main thread drops.
    #[test]
    fn timeout_cancels_the_ticket_it_names() {
        let mut held = None;
        let mut cancelled = None;
        let mut post = |wake: Wake| {
            match wake {
                // Keep the channel open and never answer.
                Wake::EditRequest(call) => held = Some(*call),
                Wake::EditCancel { ticket } => cancelled = Some(ticket),
                other => panic!("unexpected {other:?}"),
            }
            true
        };
        let line = serve_edit(
            Scope::Owner,
            Some(1),
            "file:///tmp/x.txt",
            &mut post,
            &mut never,
            Duration::from_millis(20),
            short,
        );
        let ticket = held.expect("posted").ticket;
        assert_eq!(line, format!("ERR timeout ticket={ticket}\n"));
        assert_eq!(cancelled, Some(ticket));
    }

    #[test]
    fn edit_hangup_cancels_and_answers_hangup() {
        let mut cancelled = None;
        let mut keep = None;
        let mut post = |wake: Wake| {
            match wake {
                Wake::EditRequest(call) => keep = Some(*call),
                Wake::EditCancel { ticket } => cancelled = Some(ticket),
                other => panic!("unexpected {other:?}"),
            }
            true
        };
        let mut calls = 0;
        let mut hung = || {
            calls += 1;
            calls > 1
        };
        let line = serve_edit(
            Scope::Owner,
            Some(1),
            "file:///tmp/x.txt",
            &mut post,
            &mut hung,
            Duration::from_secs(60),
            short,
        );
        assert_eq!(line, crate::control::HUNG_UP_REPLY);
        assert_eq!(cancelled, keep.map(|call| call.ticket));
    }

    #[test]
    fn await_edit_withdraws_on_every_exit() {
        // Answered.
        let mut withdrawn = Vec::new();
        let mut post = |wake: Wake| {
            match wake {
                Wake::EditAwait(call) => {
                    assert_eq!(call.key, AwaitKey::Ticket(Ticket(5)));
                    call.reply
                        .send("OK edit done result=closed saved=1 seq=2\n".to_string())
                        .unwrap();
                }
                Wake::EditAwaitWithdraw { waiter } => withdrawn.push(waiter),
                other => panic!("unexpected {other:?}"),
            }
            true
        };
        let line = serve_await_edit(Scope::Owner, "edit ticket=5", &mut post, &mut never, short);
        assert_eq!(line, "OK edit done result=closed saved=1 seq=2\n");
        // Timed out (the channel stays open, unanswered).
        let mut parked = Vec::new();
        let mut post_timeout = |wake: Wake| {
            match wake {
                Wake::EditAwait(call) => parked.push(*call),
                Wake::EditAwaitWithdraw { waiter } => withdrawn.push(waiter),
                other => panic!("unexpected {other:?}"),
            }
            true
        };
        let line = serve_await_edit(
            Scope::Owner,
            "edit uri=file:///tmp/x.txt timeout=10",
            &mut post_timeout,
            &mut never,
            short,
        );
        assert_eq!(line, "OK timeout\n");
        // Hung up.
        let mut post_hung = |wake: Wake| {
            match wake {
                Wake::EditAwait(call) => parked.push(*call),
                Wake::EditAwaitWithdraw { waiter } => withdrawn.push(waiter),
                other => panic!("unexpected {other:?}"),
            }
            true
        };
        let line = serve_await_edit(
            Scope::Owner,
            "edit ticket=5",
            &mut post_hung,
            &mut || true,
            short,
        );
        assert_eq!(line, crate::control::HUNG_UP_REPLY);
        assert_eq!(withdrawn.len(), 3, "{withdrawn:?}");
        assert_eq!(
            parked.iter().map(|call| call.waiter).collect::<Vec<_>>(),
            withdrawn[1..].to_vec(),
            "each waiter withdraws itself"
        );
        // A malformed wait, or one from a scope short of Owner, posts nothing.
        let mut nothing = |wake: Wake| panic!("posted {wake:?}");
        assert!(
            serve_await_edit(
                Scope::Owner,
                "edit ticket=x",
                &mut nothing,
                &mut never,
                short
            )
            .starts_with("ERR")
        );
        let edge = Scope::Edge(aterm_session::EdgeToken::from_bytes([7; 32]));
        assert_eq!(
            serve_await_edit(edge, "edit ticket=5", &mut nothing, &mut never, short),
            "ERR scope\n"
        );
    }

    /// The lane hands the main thread both spellings of a file it can resolve:
    /// a symlinked directory's and the realpath the store keys documents by.
    /// Unix: a directory symlink on Windows needs a privilege a test lacks.
    #[cfg(unix)]
    #[test]
    fn edit_lanes_resolve_the_realpath_spelling() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-edit-real-")
            .tempdir()
            .unwrap();
        let real_dir = dir.path().join("real");
        std::fs::create_dir(&real_dir).unwrap();
        std::fs::write(real_dir.join("x.txt"), "x\n").unwrap();
        std::os::unix::fs::symlink(&real_dir, dir.path().join("link")).unwrap();
        let spelled =
            amacs::protocol::uri::path_to_file_uri(&dir.path().join("link/x.txt")).unwrap();
        let real = amacs::protocol::uri::path_to_file_uri(
            &std::fs::canonicalize(real_dir.join("x.txt")).unwrap(),
        )
        .unwrap();
        assert_ne!(spelled, real);
        assert_eq!(real_file_uri(&spelled).as_deref(), Some(real.as_str()));
        assert_eq!(real_file_uri(&format!("{spelled}.gone")), None);

        let mut parked = None;
        let mut post = |wake: Wake| {
            match wake {
                Wake::EditAwait(call) => parked = Some(*call),
                Wake::EditAwaitWithdraw { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
            true
        };
        let rest = format!("edit uri={spelled} timeout=1");
        serve_await_edit(Scope::Owner, &rest, &mut post, &mut never, short);
        let parked = parked.expect("posted");
        assert_eq!(
            parked.key,
            AwaitKey::Uri(spelled.clone()),
            "the key as asked"
        );
        assert_eq!(parked.real_uri, Some(real.clone()));

        let mut requested = None;
        let mut post = |wake: Wake| {
            match wake {
                Wake::EditRequest(call) => requested = Some(*call),
                Wake::EditCancel { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
            true
        };
        serve_edit(
            Scope::Owner,
            Some(1),
            &spelled,
            &mut post,
            &mut never,
            Duration::ZERO,
            short,
        );
        let requested = requested.expect("posted");
        assert_eq!(requested.request.uri, spelled, "the request as asked");
        assert_eq!(requested.real_uri, Some(real));
    }

    #[test]
    fn tickets_are_unique_and_carry_this_process() {
        let a = mint_ticket();
        let b = mint_ticket();
        assert_ne!(a, b);
        assert_eq!(a.0 >> 32, u64::from(std::process::id()));
    }
}
