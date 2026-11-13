// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control::control_media::{Hops, take_hop};

/// A file URI this platform reads as an absolute path: Windows' needs a drive.
const X: &str = if cfg!(windows) {
    "file:///C:/tmp/x.txt"
} else {
    "file:///tmp/x.txt"
};

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

/// Injected `MainPost` that unwraps each `Wake` hop before calling `serve`.
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
    let uri = X;
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
        &format!("{X} line=3"),
        &mut main_thread(&mut post),
        &mut never,
        Duration::from_secs(60),
        short,
    );
    assert!(line.starts_with("OK edit ticket="), "{line}");
}

/// Deadline expiry cancels the ticket and names it in the timeout reply.
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
        X,
        &mut main_thread(&mut post),
        &mut never,
        Duration::from_millis(20),
        short,
    );
    let ticket = held.expect("posted").ticket;
    assert_eq!(line, format!("ERR timeout ticket={ticket}\n"));
    assert_eq!(cancelled, Some(ticket));
}

/// `edit` and `await edit` count their request hops until taken or returned.
#[test]
fn edit_requests_are_counted_hops_until_taken() {
    let hops = fresh_hops();
    let (reply, _answer) = std::sync::mpsc::channel();
    let raw = Wake::EditRequest(Box::new(EditCall {
        ticket: mint_ticket(),
        session: 1,
        request: EditRequest::parse(X).expect("a request"),
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
        X,
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
        X,
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
        &format!("edit uri={X} timeout=10"),
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

/// Resolves and forwards both the requested URI and its canonical realpath URI.
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
    let spelled = amacs::protocol::uri::path_to_file_uri(&dir.path().join("link/x.txt")).unwrap();
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
