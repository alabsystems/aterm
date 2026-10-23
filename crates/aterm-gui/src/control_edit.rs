// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lane handlers for the `edit` and `await edit` control verbs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use amacs::protocol::edit::{AwaitEdit, AwaitKey, EditRequest, Ticket};
use aterm_window::event_loop::EventLoopProxy;

use crate::Wake;
use crate::control::Scope;
use crate::control::control_media::MainPost;

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
    let _unwaited = (!request.wait).then(|| hold::Unwaited::mark(ticket));
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
    let waited = park_for_answer(&answer, Instant::now() + timeout, hung_up, park);
    post.tell(Wake::EditAwaitWithdraw { waiter });
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
    use crate::control::control_media::{Hops, take_hop};

    /// One park slice of at most 5 ms: these tests never wait out a real bound.
    fn short(left: Duration) -> Duration {
        left.min(Duration::from_millis(5))
    }

    fn never() -> bool {
        false
    }

    /// A count of hops of the calling test's own.
    fn fresh_hops() -> &'static Hops {
        Box::leak(Box::new(Hops::new()))
    }

    /// A main thread made of `serve`, which is handed each wake as
    /// `user_event` is, its hop taken first.
    fn main_thread<'a>(mut serve: impl FnMut(Wake) -> bool + 'a) -> MainPost<'a> {
        MainPost::injected(move |wake| serve(take_hop(wake)), fresh_hops())
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
                &mut main_thread(post),
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
            &mut main_thread(&mut post),
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
            &mut main_thread(&mut post),
            &mut never,
            Duration::from_millis(20),
            short,
        );
        let ticket = held.expect("posted").ticket;
        assert_eq!(line, format!("ERR timeout ticket={ticket}\n"));
        assert_eq!(cancelled, Some(ticket));
    }

    /// `edit` and `await edit` post their requests as counted main-thread
    /// hops ([`MainPost::ask`]): each is counted while the main thread has not
    /// taken it, so the next verb's stall refusal sees a lane parked behind a
    /// main thread that does not take it, and is counted out once taken or
    /// once its lane returns. The cancel and the withdrawal wait for nothing
    /// and are posted uncounted. Negative control: the same request posted
    /// raw leaves the count empty, so what the count sees is the hop.
    #[test]
    fn edit_requests_are_counted_hops_until_taken() {
        let hops = fresh_hops();
        let (reply, _answer) = std::sync::mpsc::channel();
        let raw = Wake::EditRequest(Box::new(EditCall {
            ticket: mint_ticket(),
            session: 1,
            request: EditRequest::parse("file:///tmp/x.txt").expect("a request"),
            real_uri: None,
            reply,
        }));
        assert!(MainPost::injected(|_| true, hops).tell(raw));
        assert_eq!(hops.since_ns(), None, "a raw post is not counted");

        let mut posted = Vec::new();
        let mut counted = Vec::new();
        let line = serve_edit(
            Scope::Owner,
            Some(1),
            "file:///tmp/x.txt",
            &mut MainPost::injected(
                |wake| {
                    posted.push(wake);
                    true
                },
                hops,
            ),
            &mut || {
                counted.push(hops.since_ns().is_some());
                false
            },
            Duration::from_millis(20),
            short,
        );
        assert!(line.starts_with("ERR timeout ticket="), "{line}");
        assert!(
            !counted.is_empty() && counted.iter().all(|&c| c),
            "{counted:?}"
        );
        assert_eq!(hops.since_ns(), None, "the lane returned");
        match posted.as_slice() {
            [Wake::Hop { wake, .. }, Wake::EditCancel { .. }]
                if matches!(**wake, Wake::EditRequest(_)) => {}
            other => panic!("the request counted, its cancel not: {other:?}"),
        }

        let mut posted = Vec::new();
        let mut counted = Vec::new();
        let line = serve_await_edit(
            Scope::Owner,
            "edit ticket=5 timeout=10",
            &mut MainPost::injected(
                |wake| {
                    posted.push(wake);
                    true
                },
                hops,
            ),
            &mut || {
                counted.push(hops.since_ns().is_some());
                false
            },
            short,
        );
        assert_eq!(line, "OK timeout\n");
        assert!(
            !counted.is_empty() && counted.iter().all(|&c| c),
            "{counted:?}"
        );
        assert_eq!(hops.since_ns(), None, "the lane returned");
        match posted.as_slice() {
            [Wake::Hop { wake, .. }, Wake::EditAwaitWithdraw { .. }]
                if matches!(**wake, Wake::EditAwait(_)) => {}
            other => panic!("the wait counted, its withdrawal not: {other:?}"),
        }

        let mut taken = Vec::new();
        let mut counted = Vec::new();
        let line = serve_await_edit(
            Scope::Owner,
            "edit ticket=5 timeout=10",
            &mut MainPost::injected(
                |wake| {
                    taken.push(take_hop(wake));
                    true
                },
                hops,
            ),
            &mut || {
                counted.push(hops.since_ns().is_some());
                false
            },
            short,
        );
        assert_eq!(line, "OK timeout\n");
        assert!(
            !counted.is_empty() && counted.iter().all(|&c| !c),
            "{counted:?}"
        );
        assert!(
            matches!(
                taken.as_slice(),
                [Wake::EditAwait(_), Wake::EditAwaitWithdraw { .. }]
            ),
            "{taken:?}"
        );
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
            &mut main_thread(&mut post),
            &mut hung,
            Duration::from_secs(60),
            short,
        );
        assert_eq!(line, crate::control::HUNG_UP_REPLY);
        assert_eq!(cancelled, keep.map(|call| call.ticket));
    }

    #[test]
    fn await_edit_withdraws_on_every_exit() {
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
        let line = serve_await_edit(
            Scope::Owner,
            "edit ticket=5",
            &mut main_thread(&mut post),
            &mut never,
            short,
        );
        assert_eq!(line, "OK edit done result=closed saved=1 seq=2\n");
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
            &mut main_thread(&mut post_timeout),
            &mut never,
            short,
        );
        assert_eq!(line, "OK timeout\n");
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
            &mut main_thread(&mut post_hung),
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
        let mut nothing = |wake: Wake| panic!("posted {wake:?}");
        assert!(
            serve_await_edit(
                Scope::Owner,
                "edit ticket=x",
                &mut main_thread(&mut nothing),
                &mut never,
                short
            )
            .starts_with("ERR")
        );
        let edge = Scope::Edge(aterm_session::EdgeToken::from_bytes([7; 32]));
        assert_eq!(
            serve_await_edit(
                edge,
                "edit ticket=5",
                &mut main_thread(&mut nothing),
                &mut never,
                short
            ),
            "ERR scope\n"
        );
    }

    /// The lane hands the main thread both spellings of a file it can resolve:
    /// a symlinked directory's and the realpath the store keys documents by.
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
        serve_await_edit(
            Scope::Owner,
            &rest,
            &mut main_thread(&mut post),
            &mut never,
            short,
        );
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
            &mut main_thread(&mut post),
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
