// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn row(node: &str, sid: &str) -> String {
    format!("/f/f/pub/{node}/{sid}/presence")
}

const LIVE: &[u8] = b"v=1 t=1 state=live";
const EXITED: &[u8] = b"v=1 t=1 state=exited";

fn nodes(view: &PresenceView, sid: &str) -> Option<Vec<String>> {
    view.snapshot(sid).map(|s| s.nodes.into_iter().collect())
}

fn answer(view: &PresenceView, sid: &str) -> Option<(Vec<String>, u64)> {
    view.snapshot(sid)
        .map(|s| (s.nodes.into_iter().collect(), s.exact_to))
}

#[test]
fn the_view_answers_only_while_live() {
    let view = PresenceView::default();
    assert_eq!(view.snapshot("s-a"), None, "closed");
    let g = view.open();
    assert!(view.apply(g, "f", 7, &row("n-a", "s-a"), LIVE));
    assert_eq!(view.snapshot("s-a"), None, "seeding");
    assert!(view.mark(g, 7, 7), "a mark AT the highest record applied");
    assert_eq!(view.phase(), Phase::Seeding, "is not past it");
    assert!(view.mark(g, 8, 8));
    assert_eq!(view.phase(), Phase::Live);
    let n_a = vec!["n-a".to_string()];
    assert_eq!(answer(&view, "s-a"), Some((n_a.clone(), 8)));
    assert_eq!(answer(&view, "s-b"), Some((vec![], 8)), "a live miss");
    assert!(view.mark(g, 40, 40));
    assert_eq!(answer(&view, "s-a"), Some((n_a.clone(), 40)));
    assert!(view.apply(g, "f", 41, "/f/f/pub/n-z/node/presence", LIVE));
    assert_eq!(answer(&view, "s-a"), Some((n_a, 42)));
    assert!(view.mark(g, 30, 30), "a mark behind it");
    assert_eq!(
        view.snapshot("s-a").map(|s| s.exact_to),
        Some(42),
        "never back"
    );
    view.end(g);
    assert_eq!(view.phase(), Phase::Dead);
    assert_eq!(view.snapshot("s-a"), None, "dead");
}

#[test]
fn a_row_never_loses_to_an_older_one() {
    let view = PresenceView::default();
    let g = view.open();
    for (off, node, body) in [(50, "n-a", EXITED), (30, "n-b", LIVE)] {
        assert!(view.apply(g, "f", off, &row(node, "s-x"), body));
    }
    for (off, node, body) in [(45, "n-a", LIVE), (50, "n-a", EXITED), (25, "n-b", EXITED)] {
        assert!(view.apply(g, "f", off, &row(node, "s-x"), body));
    }
    assert!(view.mark(g, 51, 51));
    assert_eq!(nodes(&view, "s-x"), Some(vec!["n-b".to_string()]));
    assert!(view.apply(g, "f", 60, &row("n-b", "s-x"), EXITED));
    assert!(view.apply(g, "f", 61, &row("n-c", "s-x"), LIVE));
    assert_eq!(nodes(&view, "s-x"), Some(vec!["n-c".to_string()]));
    assert!(view.apply(g, "f", 62, &row("n-d", "s-x"), LIVE));
    assert_eq!(
        nodes(&view, "s-x"),
        Some(vec!["n-c".to_string(), "n-d".to_string()])
    );
}

#[test]
fn only_a_hosting_nodes_live_row_advertises() {
    let view = PresenceView::default();
    let g = view.open();
    let records: [(&str, &[u8]); 7] = [
        (
            "/f/f/pub/n-a/s-x/presence",
            b"v=1 t=1 state=live observer=1",
        ),
        ("/f/f/pub/n-b/s-x/presence", b"v=1 t=1"),
        ("/f/f/pub/n-c/node/presence", LIVE),
        ("/f/f/pub/p/h-human/presence", LIVE),
        ("/f/g/pub/n-d/s-x/presence", LIVE),
        ("/f/f/pub/n-e/s-X!/presence", LIVE),
        ("/f/f/pub/n-f/s-x/presence", LIVE),
    ];
    for (off, (subject, body)) in (1u64..).zip(records) {
        assert!(view.apply(g, "f", off, subject, body));
    }
    assert!(view.mark(g, 100, 100));
    assert_eq!(nodes(&view, "s-x"), Some(vec!["n-f".to_string()]));
}

#[test]
fn a_stale_reader_cannot_touch_the_view() {
    let view = PresenceView::default();
    let old = view.open();
    let new = view.open();
    assert!(!view.apply(old, "f", 1, &row("n-a", "s-x"), LIVE));
    assert!(!view.mark(old, 2, 2));
    view.end(old);
    assert_eq!(
        view.phase(),
        Phase::Seeding,
        "the old reader's end is not this open's"
    );
    assert!(view.mark(new, 1, 1));
    assert_eq!(nodes(&view, "s-x"), Some(vec![]));
    view.close();
    assert!(!view.apply(new, "f", 3, &row("n-a", "s-x"), LIVE));
    assert_eq!(view.phase(), Phase::Closed);
}

/// **A CLOSE OR A REOPEN HANGS UP THE SOCKET THE VIEW HOLDS, AND A READER
/// THAT DIALLED FOR AN OPEN THAT IS GONE IS NOT HELD.** The reader dials on
/// its own thread, so its open can be closed or replaced before it hands
/// its socket over: that hand-over is refused, and the reader hangs up
/// itself.
#[cfg(unix)]
#[test]
fn a_close_hangs_up_the_socket_the_view_holds() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-pv")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("a scratch dir");
    let socket = dir.path().join("b.sock");
    let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
    let _server = broker.serve(&socket).expect("serve");
    let dial = || {
        crate::transport::connect(&socket.to_string_lossy(), Duration::from_secs(60)).expect("dial")
    };
    let alive = |conn: &mut Conn| conn.fetch(0, "/f/>", 0).is_ok();
    let view = PresenceView::default();
    let gone = view.open();
    let g = view.open();
    let (mut late, late_closer) = dial();
    assert!(
        !view.hold(gone, Arc::new(late_closer)),
        "dialled for an open that is gone"
    );
    assert!(
        alive(&mut late),
        "the reader hangs that one up, not the view"
    );
    let (mut held, closer) = dial();
    assert!(view.hold(g, Arc::new(closer)));
    assert!(alive(&mut held));
    view.close();
    assert!(!alive(&mut held), "a close hangs up the held socket");
    assert!(alive(&mut late), "and nothing else");

    let g = view.open();
    let (mut held, closer) = dial();
    assert!(view.hold(g, Arc::new(closer)));
    view.open();
    assert!(!alive(&mut held), "a reopen hangs up the previous reader");
}

#[cfg(unix)]
#[test]
fn a_gap_below_the_tail_ends_the_view() {
    use astream_broker::proto::{
        decode_request, encode_response, read_frame, write_frame, Request, Response,
    };
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-pv-gap")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("a scratch dir");
    let socket = dir.path().join("b.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    let path = socket.to_string_lossy().into_owned();
    let view = Arc::new(PresenceView::default());
    let g = view.open();
    spawn_reader(
        move || crate::transport::connect(&path, aterm_spec::wait::MUST_HAPPEN),
        "f",
        Arc::clone(&view),
        g,
    )
    .expect("the reader thread");
    let (mut c, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .expect("accept");
    let mark = |next, head| Response::Mark {
        next,
        head,
        resume: String::new(),
        durable: None,
        oldest: None,
    };
    let send = |c: &mut std::os::unix::net::UnixStream, r: &Response| {
        write_frame(c, &encode_response(r)).expect("write");
    };
    loop {
        let frame = read_frame(&mut c).expect("read").expect("a request");
        match decode_request(&frame) {
            Some(Request::Last { .. } | Request::IdleMarks | Request::GapNotices) => {
                send(&mut c, &mark(5, 5));
            }
            Some(Request::Subscribe { from_offset: 5, .. }) => break,
            other => panic!("the view sends no {other:?}"),
        }
    }
    // One write: the reader hangs up once it reads the gap, so a frame
    // written after that could meet a closed socket.
    let mut after = Vec::new();
    for r in [
        Response::Gap(astream_broker::Gap {
            from: 5,
            to: 8,
            records: Some(2),
        }),
        mark(9, 10),
        Response::Delivery {
            offset: 9,
            subject: row("n-a", "s-a"),
            body: LIVE.to_vec(),
        },
    ] {
        write_frame(&mut after, &encode_response(&r)).expect("encode");
    }
    std::io::Write::write_all(&mut c, &after).expect("write");
    assert!(
        view.wait_for(aterm_spec::wait::MUST_HAPPEN, |phase, reached| {
            phase == Phase::Dead || reached > 9
        })
    );
    assert_eq!(
        view.phase(),
        Phase::Dead,
        "a view that missed records is exact no more"
    );
    assert_eq!(view.snapshot("s-a"), None);
}

#[test]
fn a_confirmation_routes_only_through_to_the_head_and_is_a_mark_either_way() {
    let rec = |off: u64, subject: &str| (off, subject.to_string(), LIVE.to_vec());
    let judge = |found: &[astream_broker::Record], next: u64| {
        confirmation("f", "s-x", 100, found, (next, 200))
    };
    let routes = |exact_to| Confirmation {
        routes: true,
        exact_to,
    };
    let walks = |exact_to| Confirmation {
        routes: false,
        exact_to,
    };
    assert_eq!(judge(&[], 200), routes(200));
    assert_eq!(
        judge(
            &[
                rec(150, &row("n-a", "s-y")),
                rec(160, "/f/f/pub/n-a/node/presence")
            ],
            200
        ),
        routes(150)
    );
    for subject in [row("n-b", "s-x"), "/f/f/pub/N!/s-x/presence".to_string()] {
        assert_eq!(judge(&[rec(170, &subject)], 200), walks(170), "{subject}");
    }
    assert_eq!(
        judge(
            &[rec(120, &row("n-a", "s-y")), rec(170, &row("n-b", "s-x"))],
            200
        ),
        walks(120)
    );
    assert_eq!(judge(&[], 180), walks(180), "cut short by the scan bound");
    assert_eq!(
        judge(&[rec(110, &row("n-a", "s-y"))], 111),
        walks(110),
        "cut short by max"
    );
    let other_fleet = confirmation("g", "s-x", 100, &[rec(150, &row("n-a", "s-x"))], (200, 200));
    assert_eq!(other_fleet, routes(150));

    let view = PresenceView::default();
    let g = view.open();
    view.advance(g, 500, 600);
    assert!(view.mark(g, 100, 120));
    assert_eq!(
        view.snapshot("s-x").map(|s| (s.exact_to, s.head)),
        Some((100, 120)),
        "a seeding view's first mark is the broker's, not a confirmation's"
    );
    view.advance(g, 200, 250);
    assert_eq!(
        view.snapshot("s-x").map(|s| (s.exact_to, s.head)),
        Some((200, 250))
    );
    view.advance(g, 150, 240);
    assert_eq!(
        view.snapshot("s-x").map(|s| (s.exact_to, s.head)),
        Some((200, 250)),
        "never back"
    );
    view.advance(g.wrapping_add(1), 900, 900);
    assert_eq!(view.snapshot("s-x").map(|s| s.exact_to), Some(200));
    let stale = view.snapshot("s-x").expect("live").generation;
    let g2 = view.open();
    assert!(view.mark(g2, 10, 10));
    view.advance(stale, 900, 900);
    assert_eq!(
        view.snapshot("s-x").map(|s| s.exact_to),
        Some(10),
        "a confirmation read from another open moves nothing"
    );
}
