// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::live_session_row;

#[cfg(unix)]
#[test]
fn queued_survey_lines_survive_the_siblings_close() {
    use super::*;
    let (stream, mut peer) = aterm_uds::CtlStream::pair().unwrap();
    peer.write_all(b"OK 1\nsession sid=s-peer needs=-\n")
        .unwrap();
    drop(peer);
    aterm_spec::wait::until_peer_closed(&stream);
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let mut reader = BufReader::new(PeerRead {
        stream: &stream,
        deadline,
    });
    assert_eq!(
        read_bounded_line(&mut reader, deadline).as_deref(),
        Some("OK 1\n")
    );
    assert_eq!(
        read_bounded_line(&mut reader, deadline).as_deref(),
        Some("session sid=s-peer needs=-\n")
    );
    assert_eq!(read_bounded_line(&mut reader, deadline), None);
}

/// A `survey facts` row counts where it is a live session's: an exited session, a row with no
/// sid and a line that is no session row are not counted, whatever else the row says.
#[test]
fn only_a_live_sessions_row_is_counted() {
    for (line, live) in [
        (
            "session local=3 sid=s-abc state=alive program=claude needs=frozen hold_by=-\n",
            true,
        ),
        (
            "session local=5 sid=s-x state=alive needs=- hold_by=-",
            true,
        ),
        ("session local=7 sid=s-z state=alive attention=1", true),
        ("session local=6 sid=s-y state=exited needs=ask", false),
        ("session local=8 sid=- state=alive needs=-", false),
        ("session local=9 state=alive needs=-", false),
        ("instance pid=1 host=h", false),
    ] {
        assert_eq!(live_session_row(line), live, "{line:?}");
    }
}

/// A scripted sibling over a real Unix socket serving AUTH and `survey
/// facts` — the whole dial in ONE read: its live sessions are counted, whether or not its rows
/// publish a need, and an exited row is not counted.
#[cfg(unix)]
#[test]
fn summarize_peer_reads_a_scripted_sibling() {
    use std::io::{BufRead, BufReader, Write};
    for (needs, published) in [(" needs=ask", true), ("", false)] {
        let scratch = crate::test_scratch::short_dir("the peer's socket");
        let dir = scratch.path();
        let sock = dir.join("peer.sock");
        std::fs::write(dir.join("aterm.token"), "feedbeef\n").unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let accounts: String = (0..8192)
            .map(|i| format!("account cli=codex label=account-{i} condition=usable\n"))
            .collect();
        let reply = format!(
            "OK {}\ninstance pid=9 host=h version=1 sessions=3 level=facts cut=0\n\
             session local=0 sid=s-op state=alive program=zsh role=- attention=1{needs}\n\
             session local=1 sid=s-w state=alive program=zsh role=- attention=-{}\n\
             session local=2 sid=s-x state=exited program=zsh role=- attention=-{needs}\n{accounts}",
            4 + 8192,
            if published { " needs=-" } else { "" },
        );
        let server = std::thread::spawn(move || {
            let (conn, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "AUTH feedbeef\n");
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "survey facts\n");
            let mut w = conn.try_clone().unwrap();
            w.write_all(reply.as_bytes()).unwrap();
            line.clear();
            assert_eq!(reader.read_line(&mut line).unwrap(), 0, "{line:?}");
        });
        let row = super::summarize_peer_within(
            4242,
            sock.to_str().unwrap(),
            aterm_spec::wait::MUST_HAPPEN,
        )
        .expect("summarized");
        server.join().unwrap();
        assert_eq!(row.pid, 4242);
        assert_eq!(row.sessions, 2, "exited row excluded: {needs:?}");
    }
}
