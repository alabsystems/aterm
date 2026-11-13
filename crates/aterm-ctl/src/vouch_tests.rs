// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::sync::mpsc;
use std::time::Duration;

use super::hold_paced;

/// The hang detector of every wait here.
const HANG: Duration = Duration::from_secs(60);

const TOKEN: &str = "fixture-owner-token";

#[derive(Debug, PartialEq, Eq)]
enum Seen {
    /// Connection `n`'s request, and whether connection `n - 1` was still open when `n` was
    /// accepted (before its handshake was answered, so before the client could let `n - 1` go).
    Asked {
        n: usize,
        line: String,
        last_open: Option<bool>,
    },
    Closed(usize),
}

/// A stand-in for the caller's own aterm at a fresh socket with its token beside it: each
/// connection's handshake answered, its request line and its close reported.
fn own_aterm() -> (aterm_tempfile::TempDir, String, mpsc::Receiver<Seen>) {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::os::unix::net::UnixListener;
    let dir = crate::test_scratch_dir("vouch");
    let sock = dir.path().join("own.sock");
    let listener = UnixListener::bind(&sock).expect("bind");
    let token = dir
        .path()
        .join(&crate::control_socket::token_names_for_sock("own.sock")[0]);
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(token)
        .and_then(|mut f| f.write_all(TOKEN.as_bytes()))
        .expect("the token beside the socket");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut last: Option<aterm_uds::CtlStream> = None;
        for n in 0.. {
            let Ok((conn, _)) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            ) else {
                return;
            };
            let mut r = BufReader::new(conn.try_clone().expect("clone"));
            let mut line = String::new();
            for _ in 0..2 {
                line.clear();
                r.read_line(&mut line).expect("the handshake");
            }
            let last_open = last
                .as_ref()
                .map(|l| !aterm_uds::hangup::fd_peer_closed(l.as_raw_fd()));
            (&conn).write_all(b"OK version\n").expect("accepted");
            line.clear();
            r.read_line(&mut line).expect("the request");
            let _ = tx.send(Seen::Asked {
                n,
                line: line.trim_end().to_string(),
                last_open,
            });
            last = Some(conn);
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut rest = Vec::new();
                let _ = r.read_to_end(&mut rest);
                let _ = tx.send(Seen::Closed(n));
            });
        }
    });
    (dir, sock.display().to_string(), rx)
}

/// A VOUCH IS HELD ON THE CALLER'S OWN ATERM UNTIL DROPPED, the next opened before the last is let
/// go: it asks `await elsewhere` one leg at a time, each later leg accepted while the one before
/// it is still open (the aterm never reads a moment with neither), and dropping the vouch closes
/// every connection it opened. NEGATIVE CONTROL: a vouch on a socket nothing listens at holds
/// nothing and ends at once.
#[test]
fn a_vouch_holds_its_await_until_dropped_and_renews_before_it_ends() {
    let (_dir, sock, seen) = own_aterm();
    let next = || {
        seen.recv_timeout(HANG)
            .expect("the own aterm hears the vouch")
    };
    let vouch = hold_paced(
        &sock,
        Duration::from_millis(600),
        Duration::from_millis(400),
    )
    .expect("held");
    let mut asked = Vec::new();
    let mut closed = Vec::new();
    while asked.len() < 3 {
        match next() {
            Seen::Asked { n, line, last_open } => {
                assert_eq!(line, "await elsewhere timeout=600");
                assert_eq!(last_open, (n > 0).then_some(true), "leg {n}");
                asked.push(n);
            }
            Seen::Closed(n) => closed.push(n),
        }
    }
    drop(vouch);
    while closed.len() < asked.len() {
        match next() {
            Seen::Asked { n, .. } => asked.push(n),
            Seen::Closed(n) => closed.push(n),
        }
    }
    closed.sort_unstable();
    assert_eq!(closed, asked, "the drop let every vouch go");

    let dir = crate::test_scratch_dir("vouch-none");
    let nothing = dir.path().join("none.sock").display().to_string();
    let idle = hold_paced(
        &nothing,
        Duration::from_millis(600),
        Duration::from_millis(400),
    )
    .expect("a vouch thread starts");
    drop(idle);
}
