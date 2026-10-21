// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the `amacs` client's `edit` and `await edit` exchanges: the
//! re-armed wait and its re-dial against a scripted wire and a scripted
//! clock, the meaning of each `edit` answer, and both exchanges over a real
//! socket to a mock window.

use std::collections::VecDeque;

use super::*;

const SID: &str = "s-ab";
const URI: &str = "file:///tmp/x%20y/COMMIT_EDITMSG";

/// How long the mock window may take to accept and answer the connect phase:
/// a hang detector, never a latency budget (a loaded machine may not run the
/// mock thread for seconds).
#[cfg(unix)]
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// A scripted [`Wire`]: each `resolve` and `send` takes the next scripted
/// answer (a resolve with none left answers `/run/a.sock`, a send with none
/// left is refused as a dead socket is), and the clock moves only by the
/// pauses asked of it.
struct Script {
    resolves: VecDeque<io::Result<String>>,
    answers: VecDeque<io::Result<String>>,
    /// Every send: the socket, the request line and the reply wait.
    sent: Vec<(String, String, Duration)>,
    start: Instant,
    paused: Duration,
}

impl Script {
    fn new(answers: Vec<io::Result<String>>) -> Self {
        Self {
            resolves: VecDeque::new(),
            answers: answers.into(),
            sent: Vec::new(),
            start: Instant::now(),
            paused: Duration::ZERO,
        }
    }

    fn resolving(mut self, socks: Vec<io::Result<String>>) -> Self {
        self.resolves = socks.into();
        self
    }

    /// The requests sent, without their sockets and waits.
    fn requests(&self) -> Vec<&str> {
        self.sent.iter().map(|(_, line, _)| line.as_str()).collect()
    }
}

impl Wire for Script {
    fn resolve(&mut self) -> io::Result<String> {
        self.resolves
            .pop_front()
            .unwrap_or_else(|| Ok("/run/a.sock".to_string()))
    }

    fn send(&mut self, sock: &str, request: &str, reply: Duration) -> io::Result<String> {
        assert!(
            self.sent.len() < 1000,
            "the wait never ends: {:?}",
            self.sent
        );
        self.sent
            .push((sock.to_string(), request.to_string(), reply));
        self.answers
            .pop_front()
            .unwrap_or_else(|| Err(io::ErrorKind::ConnectionRefused.into()))
    }

    fn pause(&mut self, wait: Duration) {
        self.paused += wait;
    }

    fn now(&mut self) -> Instant {
        self.start + self.paused
    }
}

fn ok(line: &str) -> io::Result<String> {
    Ok(line.to_string())
}

fn ticket_leg() -> String {
    format!("@{SID} await edit ticket=7 timeout=600000\n")
}

fn uri_leg() -> String {
    format!("@{SID} await edit uri={URI} timeout=600000\n")
}

/// RE-ARMED UNTIL THE OUTCOME, with no total limit: two legs time out at the
/// server's bound and the third answers. Every leg asks the same socket for
/// the same ticket and the server's whole bound, and waits past it by the
/// leg slack; nothing pauses between legs (the server's orphan grace starts
/// at each leg's end).
#[test]
fn rearm_on_timeout() {
    let mut wire = Script::new(vec![
        ok("OK timeout"),
        ok("OK timeout"),
        ok("OK edit done result=closed saved=1 seq=4"),
    ]);
    let outcome = await_over(&mut wire, SID, AwaitKey::Ticket(Ticket(7)), URI);
    assert_eq!(
        outcome,
        Ok(EditOutcome {
            result: amacs::protocol::edit::EditResult::Closed,
            saved: true,
            seq: 4,
        })
    );
    assert_eq!(wire.requests(), vec![ticket_leg(); 3]);
    for (sock, _, wait) in &wire.sent {
        assert_eq!(sock, "/run/a.sock");
        assert_eq!(*wait, LEG + crate::await_task::LEG_SLACK);
    }
    assert_eq!(wire.paused, Duration::ZERO);
}

/// A LEG CUT BY ITS SERVER is re-dialled: the session's socket is resolved
/// again and the wait re-armed by the file, because a successor knows no
/// ticket the old process minted. A successor that does not host the session
/// yet (`ERR no such session`) is looked at again, and once a leg is taken
/// the wait is back to normal. `ERR unknown ticket` (a server that lost the
/// ticket) re-arms by the file on the same socket. NEGATIVE CONTROL: nothing
/// taking the wait within the successor bound ends it `Replaced`, with what
/// was last heard, never an outcome.
#[test]
fn redial_by_uri_after_replaced() {
    for sid in [
        "",
        " ",
        "s-",
        "s-g",
        "@s-ab",
        ".",
        "1",
        "../s-ab",
        "s-ab,s-cd",
        "s-ab close",
        "s-ab\n",
        "s-ab\r",
        "s-ab\tedit",
        "s-ab close\n@s-ab",
    ] {
        let mut wire =
            Script::new(Vec::new()).resolving(vec![Err(io::ErrorKind::PermissionDenied.into())]);
        let err = await_over(&mut wire, sid, AwaitKey::Ticket(Ticket(7)), URI)
            .expect_err("invalid session");
        assert!(matches!(err, AwaitErr::Unsendable(_)), "{sid:?}: {err:?}");
        assert_eq!(wire.resolves.len(), 1, "{sid:?}: no resolution");
        assert!(wire.sent.is_empty(), "{sid:?}: no request");
    }

    let closed = |seq| EditOutcome {
        result: amacs::protocol::edit::EditResult::Closed,
        saved: false,
        seq,
    };
    for sid in [SID, "s-0123456789abcdef0123"] {
        let mut wire = Script::new(vec![ok("OK edit done result=closed saved=0 seq=1")]);
        assert_eq!(
            await_over(&mut wire, sid, AwaitKey::Ticket(Ticket(7)), URI),
            Ok(closed(1))
        );
        let request = format!("@{sid} await edit ticket=7 timeout=600000\n");
        assert_eq!(wire.requests(), [request.as_str()]);
    }
    let mut wire = Script::new(vec![
        Err(io::ErrorKind::UnexpectedEof.into()),
        Err(io::ErrorKind::ConnectionRefused.into()),
        ok("ERR no such session"),
        ok("OK timeout"),
        ok("OK edit done result=closed saved=0 seq=9"),
    ])
    .resolving(vec![
        ok("/run/a.sock"),
        ok("/run/a.sock"),
        ok("/run/b.sock"),
        ok("/run/b.sock"),
    ]);
    let outcome = await_over(&mut wire, SID, AwaitKey::Ticket(Ticket(7)), URI);
    assert_eq!(outcome, Ok(closed(9)));
    let sent: Vec<(&str, &str)> = wire
        .sent
        .iter()
        .map(|(sock, line, _)| (sock.as_str(), line.as_str()))
        .collect();
    let (ticket, uri) = (ticket_leg(), uri_leg());
    assert_eq!(
        sent,
        [
            ("/run/a.sock", ticket.as_str()),
            ("/run/a.sock", uri.as_str()),
            ("/run/b.sock", uri.as_str()),
            ("/run/b.sock", uri.as_str()),
            ("/run/b.sock", uri.as_str()),
        ]
    );
    assert!(wire.paused < crate::redial::SUCCESSOR_BOUND);

    let mut wire = Script::new(vec![
        ok("ERR unknown ticket"),
        ok("OK edit done result=closed saved=0 seq=2"),
    ]);
    let outcome = await_over(&mut wire, SID, AwaitKey::Ticket(Ticket(7)), URI);
    assert_eq!(outcome, Ok(closed(2)));
    assert_eq!(wire.requests(), [ticket_leg(), uri_leg()]);
    assert_eq!(wire.paused, Duration::ZERO, "the same server, at once");

    // A seamless update's old process may say it is going away before it
    // closes the connection; behind a forward, the relay says its host went.
    for going in [
        "ERR event loop gone",
        "ERR edit waiter was dropped",
        "ERR gone pid=41 sock=/run/a.sock",
        "ERR control server busy; retry",
    ] {
        let mut wire = Script::new(vec![
            ok(going),
            ok("OK edit done result=closed saved=0 seq=3"),
        ]);
        let outcome = await_over(&mut wire, SID, AwaitKey::Ticket(Ticket(7)), URI);
        assert_eq!(outcome, Ok(closed(3)), "{going}");
        assert_eq!(wire.requests(), [ticket_leg(), uri_leg()], "{going}");
    }

    // Nothing answers again: the wait ends at the bound, as a replacement.
    let mut wire = Script::new(vec![Err(io::ErrorKind::UnexpectedEof.into())]);
    let outcome = await_over(&mut wire, SID, AwaitKey::Ticket(Ticket(7)), URI);
    assert!(
        matches!(&outcome, Err(AwaitErr::Replaced(last)) if last.contains("refused")),
        "{outcome:?}"
    );
    assert!(wire.paused >= crate::redial::SUCCESSOR_BOUND - REDIAL_POLL);
    assert!(wire.paused <= crate::redial::SUCCESSOR_BOUND + REDIAL_POLL);
    assert!(wire.requests()[1..].iter().all(|line| *line == uri_leg()));

    // A refusal from the server the wait was armed on is the answer.
    let mut wire = Script::new(vec![ok("ERR scope")]);
    let outcome = await_over(&mut wire, SID, AwaitKey::Ticket(Ticket(7)), URI);
    assert_eq!(outcome, Err(AwaitErr::Refused("ERR scope".to_string())));
    let mut wire = Script::new(vec![ok("OK edit done result=kept saved=1 seq=1")]);
    let outcome = await_over(&mut wire, SID, AwaitKey::Uri(URI.to_string()), URI);
    assert!(
        matches!(outcome, Err(AwaitErr::Unreadable(_))),
        "{outcome:?}"
    );
}

/// What each `edit` answer means for the client (design §6.1): an `ERR` the
/// window answered, or no window to ask, sends it to its own terminal; the
/// server's deadline, or a request taken and never answered, does not (the
/// view may still open), and neither does an answer it cannot read.
#[test]
fn err_reply_selects_terminal_face() {
    for sid in [
        "",
        " ",
        "s-",
        "s-g",
        "@s-ab",
        ".",
        "1",
        "../s-ab",
        "s-ab,s-cd",
        "s-ab close",
        "s-ab\n",
        "s-ab\r",
        "s-ab\tedit",
        "s-ab close\n@s-ab",
    ] {
        let mut wire =
            Script::new(Vec::new()).resolving(vec![Err(io::ErrorKind::PermissionDenied.into())]);
        let request = EditRequest {
            uri: URI.to_string(),
            line: None,
            create: false,
            for_word: None,
            wait: true,
        };
        let err = edit_over(&mut wire, sid, &request).expect_err("invalid session");
        assert!(matches!(err, EditErr::Unsendable(_)), "{sid:?}: {err:?}");
        assert_eq!(wire.resolves.len(), 1, "{sid:?}: no resolution");
        assert!(wire.sent.is_empty(), "{sid:?}: no request");
    }

    let opened = edit_reply(&format!("OK edit ticket=9 view=4 doc=2 uri={URI}")).expect("opened");
    assert_eq!((opened.ticket, opened.view, opened.doc), (Ticket(9), 4, 2));
    assert_eq!(opened.uri, URI);
    for refused in [
        "ERR scope",
        "ERR unsupported headless",
        "ERR unsupported create",
        "ERR no-window",
        "ERR refused not-regular",
        "ERR refused symlink",
        "ERR unknown verb (try: help)",
        "ERR auth; the server refused the token in /run/x.token; re-run",
        "ERR control server busy; retry",
    ] {
        let err = edit_reply(refused).expect_err(refused);
        assert!(matches!(err, EditErr::Refused(_)), "{refused}: {err:?}");
        assert!(err.falls_back_to_terminal(), "{refused}");
        assert_eq!(err.to_string(), refused);
    }
    let timed_out = edit_reply("ERR timeout ticket=12").expect_err("timeout");
    assert!(matches!(timed_out, EditErr::TimedOut(_)), "{timed_out:?}");
    assert!(!timed_out.falls_back_to_terminal());
    for unreadable in ["OK edit ticket=1", "OK", "edit", ""] {
        let err = edit_reply(unreadable).expect_err(unreadable);
        assert!(!err.falls_back_to_terminal(), "{unreadable}: {err:?}");
    }

    let request = EditRequest {
        uri: URI.to_string(),
        line: Some((3, None)),
        create: false,
        for_word: Some("git".to_string()),
        wait: true,
    };
    // No window to ask: the request never left.
    let mut wire = Script::new(Vec::new()).resolving(vec![Err(io::ErrorKind::NotFound.into())]);
    let err = edit_over(&mut wire, SID, &request).expect_err("nothing to ask");
    assert!(matches!(err, EditErr::Unreachable(_)) && err.falls_back_to_terminal());
    assert!(wire.sent.is_empty());
    let mut wire = Script::new(vec![Err(io::ErrorKind::ConnectionRefused.into())]);
    let err = edit_over(&mut wire, SID, &request).expect_err("a dead socket");
    assert!(matches!(err, EditErr::Unreachable(_)) && err.falls_back_to_terminal());
    // A timeout, EOF/reset after the send, or an uncertain write/flush:
    // delivery may have happened, so none can start a second editor.
    for kind in [
        io::ErrorKind::WouldBlock,
        io::ErrorKind::UnexpectedEof,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::BrokenPipe,
    ] {
        let mut wire = Script::new(vec![Err(crate::delivered(
            kind,
            "the edit's delivery is uncertain".to_string(),
        ))]);
        let err = edit_over(&mut wire, SID, &request).expect_err("no answer");
        assert!(matches!(err, EditErr::NoAnswer(_)), "{err:?}");
        assert!(!err.falls_back_to_terminal());
    }
    // A request that cannot be one wire line is never sent.
    let spaced = EditRequest {
        uri: "file:///tmp/a b".to_string(),
        ..request.clone()
    };
    let mut wire = Script::new(Vec::new());
    let err = edit_over(&mut wire, SID, &spaced).expect_err("unsendable");
    assert!(matches!(err, EditErr::Unsendable(_)) && err.falls_back_to_terminal());
    assert!(wire.sent.is_empty());
    // Short captured IDs and real minted IDs retain the same wire spelling.
    for sid in [SID, "s-0123456789abcdef0123"] {
        let mut wire = Script::new(vec![ok(&format!(
            "OK edit ticket=9 view=4 doc=2 uri={URI}"
        ))]);
        let opened = edit_over(&mut wire, sid, &request).expect("opened");
        assert_eq!(opened.ticket, Ticket(9));
        assert_eq!(
            wire.sent,
            [(
                "/run/a.sock".to_string(),
                format!("@{sid} edit {URI} line=3 for=git\n"),
                EDIT_REPLY_WAIT
            )]
        );
    }
}

/// The exchanges over a real socket, through the connect phase every client
/// here runs (`AUTH`, the `version` probe) to a mock window: `edit` sends its
/// one line and reads the view's ticket; `await edit` re-arms on a new
/// connection after `OK timeout`, and reads the outcome.
#[cfg(unix)]
#[test]
fn edit_and_await_edit_over_a_real_socket() {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixListener;

    /// A wire pinned to one socket, sending as [`SocketWire`] does, but never
    /// waiting less than the hang detector for the mock: its connect phase in
    /// place of `CONNECT_WAIT`, and its answer even when the exchange's own
    /// reply wait is shorter.
    struct Pinned(String);
    impl Wire for Pinned {
        fn resolve(&mut self) -> io::Result<String> {
            Ok(self.0.clone())
        }
        fn send(&mut self, sock: &str, request: &str, reply: Duration) -> io::Result<String> {
            crate::front_door_send_within(sock, request, HANG_DETECTOR, reply.max(HANG_DETECTOR))
        }
        fn pause(&mut self, wait: Duration) {
            std::thread::sleep(wait);
        }
        fn now(&mut self) -> Instant {
            Instant::now()
        }
    }

    let dir = std::env::temp_dir().join(format!("aterm-ctl-edit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("private dir");
    std::fs::write(dir.join("mock.sock.token"), "deadbeef\n").expect("token");
    let sock = dir.join("mock.sock");
    let listener = UnixListener::bind(&sock).expect("bind the mock window");
    let answers = [
        format!("OK edit ticket=9 view=4 doc=2 uri={URI}\n"),
        "OK timeout\n".to_string(),
        "OK edit done result=closed saved=1 seq=6\n".to_string(),
    ];
    let window = std::thread::spawn(move || {
        let mut heard = Vec::new();
        for answer in answers {
            let (conn, _) = listener.accept().expect("accept");
            let mut reader = std::io::BufReader::new(conn.try_clone().expect("clone"));
            let mut writer = conn;
            for reply in ["", "OK version=test\n", answer.as_str()] {
                let mut line = String::new();
                reader.read_line(&mut line).expect("a request line");
                heard.push(line);
                writer.write_all(reply.as_bytes()).expect("answer");
            }
        }
        heard
    });
    let mut wire = Pinned(sock.to_str().expect("utf8 path").to_string());
    let request = EditRequest {
        uri: URI.to_string(),
        line: None,
        create: false,
        for_word: None,
        wait: true,
    };
    let opened = edit_over(&mut wire, SID, &request).expect("opened");
    let outcome = await_over(&mut wire, SID, AwaitKey::Ticket(opened.ticket), &opened.uri)
        .expect("the outcome");
    assert!(outcome.saved);
    assert_eq!(outcome.seq, 6);
    let heard = window.join().expect("mock window");
    let probe = ["AUTH deadbeef\n", "version\n"];
    assert_eq!(heard[..2], probe);
    assert_eq!(heard[2], format!("@{SID} edit {URI}\n"));
    assert_eq!(heard[3..5], probe);
    assert_eq!(heard[5], "@s-ab await edit ticket=9 timeout=600000\n");
    assert_eq!(heard[6..8], probe);
    assert_eq!(heard[8], "@s-ab await edit ticket=9 timeout=600000\n");
    let _ = std::fs::remove_dir_all(&dir);
}
