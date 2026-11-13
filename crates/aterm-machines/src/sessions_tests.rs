// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::{AcceptWait, AcceptedMode};
use std::os::unix::net::UnixListener;

fn live_row(pid: u32, line: &str) -> crate::protocol::HostedSession {
    parse_aterm_roster_line(pid, line)
        .expect("a readable fixture")
        .expect("a live fixture")
}

struct FixtureStream {
    stream: UnixStream,
    deadline: std::time::Instant,
}

impl FixtureStream {
    fn try_clone(&self) -> std::io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            deadline: self.deadline,
        })
    }

    fn ready(&self, interest: aterm_uds::ready::Interest) -> std::io::Result<()> {
        use std::os::fd::AsFd as _;
        aterm_uds::ready::until(self.stream.as_fd(), interest, self.deadline)
    }
}

impl Read for FixtureStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match self.stream.read(buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    self.ready(aterm_uds::ready::Interest::Read)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }
}

impl Write for FixtureStream {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        loop {
            match self.stream.write(bytes) {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    self.ready(aterm_uds::ready::Interest::Write)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

fn accept_fixture(listener: &UnixListener) -> FixtureStream {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let (stream, _) = aterm_uds::accept(
        listener,
        AcceptWait::Until(deadline),
        AcceptedMode::Nonblocking,
    )
    .unwrap_or_else(|error| panic!("native fixture accept: {error}"));
    FixtureStream { stream, deadline }
}

#[test]
fn the_native_fixture_drains_the_request_and_eof_when_its_client_already_left() {
    let root = aterm_tempfile::tempdir().unwrap();
    let socket = root.path().join("already-left.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut client = UnixStream::connect(&socket).unwrap();
    client.write_all(b"AUTH fixture\nsessions\n").unwrap();
    drop(client);
    let mut stream = BufReader::new(accept_fixture(&listener));
    let mut request = String::new();
    stream.read_line(&mut request).unwrap();
    assert_eq!(request, "AUTH fixture\n");
    request.clear();
    stream.read_line(&mut request).unwrap();
    assert_eq!(request, "sessions\n");
    assert_eq!(stream.read_line(&mut request).unwrap(), 0);
}

#[test]
fn a_slow_session_reply_is_unobserved_instead_of_an_empty_inventory() {
    let root = aterm_tempfile::tempdir().unwrap();
    let sock = root.path().join("aterm-4242.sock");
    std::fs::write(root.path().join("aterm-4242.token"), "feedbeef\n").unwrap();
    let listener = UnixListener::bind(&sock).unwrap();
    let (finish, finished) = std::sync::mpsc::channel();
    let producer = std::thread::spawn(move || {
        let stream = accept_fixture(&listener);
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "AUTH feedbeef\n");
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "sessions\n");
        finished.recv_timeout(Duration::from_secs(60)).unwrap();
        drop(stream);
    });
    let observed = query_instance_sessions_after(4242, &sock, SOCKET_WAIT, Duration::from_secs(1));
    finish.send(()).unwrap();
    producer.join().unwrap();
    assert!(
        !observed.complete,
        "a timed-out response supplies no complete inventory"
    );
    assert!(observed.rows.is_empty());
}

fn snapshot_from_reply(reply: &[u8]) -> crate::protocol::SessionSnapshot {
    let root = aterm_tempfile::tempdir().unwrap();
    let sock = root.path().join("aterm-4242.sock");
    std::fs::write(root.path().join("aterm-4242.token"), "feedbeef\n").unwrap();
    let listener = UnixListener::bind(&sock).unwrap();
    let reply = reply.to_vec();
    let producer = std::thread::spawn(move || {
        let mut stream = accept_fixture(&listener);
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "AUTH feedbeef\n");
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "sessions\n");
        stream.write_all(&reply).unwrap();
    });
    let snapshot = scan_aterm_sessions_in_dir(root.path(), Duration::from_secs(60));
    producer.join().unwrap();
    snapshot
}

#[test]
fn partial_and_unread_rosters_retain_rows_without_claiming_completeness() {
    for reply in [
        b"OK 2\n0 s-first - alive zsh meta=0\n".as_slice(),
        b"OK 2\n0 s-first - alive zsh meta=0\nunread\n".as_slice(),
    ] {
        let snapshot = snapshot_from_reply(reply);
        assert!(!snapshot.complete);
        assert_eq!(snapshot.rows.len(), 1);
        assert_eq!(snapshot.rows[0].sid, "s-first");
    }
    for reply in [
        b"OK 0\n".as_slice(),
        b"OK 1\n0 s-ended - exited zsh meta=0\n".as_slice(),
    ] {
        let snapshot = snapshot_from_reply(reply);
        assert!(snapshot.complete);
        assert!(snapshot.rows.is_empty());
    }
    for reply in [b"unread\n".as_slice(), b"OK 1025\n".as_slice()] {
        let snapshot = snapshot_from_reply(reply);
        assert!(!snapshot.complete);
        assert!(snapshot.rows.is_empty());
    }
}

#[test]
fn an_unread_instance_directory_or_graph_is_not_a_complete_empty_inventory() {
    let root = aterm_tempfile::tempdir().unwrap();
    let missing = scan_aterm_sessions_in_dir(&root.path().join("absent"), SOCKET_WAIT);
    assert!(missing.complete);
    assert!(missing.rows.is_empty());

    let file = root.path().join("not-a-directory");
    std::fs::write(&file, "not a directory").unwrap();
    let unread = scan_aterm_sessions_in_dir(&file, SOCKET_WAIT);
    assert!(!unread.complete);
    assert!(unread.rows.is_empty());

    let graph = root.path().join("graph");
    std::fs::create_dir(&graph).unwrap();
    std::fs::write(graph.join("instance"), "unread graph entry").unwrap();
    let unread = scan_aterm_sessions_in_dir(root.path(), SOCKET_WAIT);
    assert!(!unread.complete);
    assert!(unread.rows.is_empty());
}

#[test]
fn roster_line_and_socket_dir_scanner_capture_live_tabs_and_ai_cli_telemetry() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("at-lan-tabs-")
        .tempdir_in("/tmp")
        .unwrap();
    let sock = dir.path().join("aterm-4242.sock");
    std::fs::write(dir.path().join("aterm-4242.token"), "feedbeef\n").unwrap();
    let listener = UnixListener::bind(&sock).unwrap();
    let server = std::thread::spawn(move || {
        let (conn, _) =
            aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking).unwrap();
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "AUTH feedbeef\n");
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "sessions\n");
        let mut w = conn;
        w.write_all(
            b"OK 2\n\
              0 s-ai - alive claude%20code meta=1 nonce=aa window=0 active=1 wfocus=1 \
              program=claude agent=working agent_detail=thinking agent_since_ms=4200 \
              human_ms=15000 role=operator attention=- user_title=Fleet%20Work \
              supervisor=aterm autopilot=cruise task=build%20fleet\n\
              1 s-dead - exited zsh meta=0\n",
        )
        .unwrap();
    });

    let snapshot = scan_aterm_sessions_in_dir(dir.path(), Duration::from_secs(60));
    server.join().unwrap();
    assert!(snapshot.complete);
    let rows = snapshot.rows;
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r.instance_pid, 4242);
    assert_eq!(r.local_id, 0);
    assert_eq!(r.sid, "s-ai");
    assert_eq!(r.title, "Fleet Work");
    assert!(r.active && r.focused);
    assert_eq!(r.program.as_deref(), Some("claude"));
    assert_eq!(r.agent.as_deref(), Some("working"));
    assert_eq!(r.agent_detail.as_deref(), Some("thinking"));
    assert_eq!(r.agent_since_ms, Some(4200));
    assert_eq!(r.human_ms, Some(15000));
    assert_eq!(r.autopilot.as_deref(), Some("cruise"));
    assert_eq!(r.role.as_deref(), Some("operator"));
    assert_eq!(r.task.as_deref(), Some("build fleet"));
    assert_eq!(r.program_state, None, "a line with no report carries none");

    let real_tok = dir.path().join("real.token");
    std::fs::write(&real_tok, "cafef00d\n").unwrap();
    let sym_sock = dir.path().join("aterm-9999.sock");
    std::os::unix::fs::symlink(&real_tok, dir.path().join("aterm-9999.token")).unwrap();
    assert_eq!(read_sock_token(&sym_sock), None);
    let injected_sock = dir.path().join("aterm-8888.sock");
    std::fs::write(dir.path().join("aterm-8888.token"), "bad\nsessions\n").unwrap();
    assert_eq!(read_sock_token(&injected_sock), None);
}

/// A codex's own report (`program_state=`) rides the fleet row, so another Mac's Settings can
/// read it where the codex reader says `unknown`.
#[test]
fn a_roster_line_carries_the_programs_own_report() {
    let row = |state: &str| {
        live_row(
            7,
            &format!(
                "2 s-cx - alive codex meta=1 program=codex agent=unknown program_state={state}"
            ),
        )
        .program_state
    };
    assert_eq!(
        row("blocked:permission").as_deref(),
        Some("blocked:permission")
    );
    assert_eq!(row("-"), None);
}

/// BOUNDED READ FRAMING AND ZERO-PID DEDUP: an oversized `cwd` reply without `\n` stops
/// enrichment rather than shifting its tail into the next session's `dims`, and `pid == 0`
/// graph rows with distinct `sid`s do not collapse on `local_id`.
/// NEGATIVE CONTROL: well-formed newline-terminated replies enrich the preceding session and
/// duplicate `sid`s still deduplicate.
#[test]
fn enrich_stops_on_unterminated_reply_and_dedup_preserves_distinct_zero_pid_sids() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("at-ses-")
        .tempdir_in("/tmp")
        .unwrap();
    let sock = dir.path().join("aterm-77.sock");
    std::fs::write(dir.path().join("aterm-77.token"), "abcd\n").unwrap();
    let listener = UnixListener::bind(&sock).unwrap();
    let server = std::thread::spawn(move || {
        let (mut conn, _) =
            aterm_uds::accept(&listener, AcceptWait::Forever, AcceptedMode::Blocking).unwrap();
        let mut req = Vec::new();
        let _ = conn.read_to_end(&mut req);
        let mut reply = b"OK 24 80\nOK ".to_vec();
        reply.extend(std::iter::repeat_n(b'a', 4100));
        reply.extend_from_slice(b"\nOK 50 160\nOK /Users//ok\n");
        let _ = conn.write_all(&reply);
        conn
    });

    let mut sessions = vec![
        live_row(77, "0 s-first - alive zsh meta=0"),
        live_row(77, "1 s-second - alive zsh meta=0"),
    ];
    // The server above always answers: a hang detector, never the roster's 80 ms.
    enrich_sessions_dims_and_cwd(&sock, "abcd", &mut sessions, Duration::from_secs(60));
    let _held = server.join().unwrap();
    assert_eq!(sessions[0].rows, Some(24));
    assert_eq!(sessions[0].cols, Some(80));
    assert_eq!(
        sessions[0].cwd, None,
        "oversized unterminated cwd must be dropped"
    );
    assert_eq!(
        sessions[1].cols, None,
        "tail of oversized cwd line must not desynchronize into session 1"
    );
    assert_eq!(sessions[1].cwd, None);

    let a = live_row(0, "0 s-one - alive zsh meta=0");
    let b = live_row(0, "0 s-two - alive zsh meta=0");
    let a_dup = live_row(0, "0 s-one - alive zsh meta=0");
    assert!(!same_hosted_session(&a, &b));
    assert!(same_hosted_session(&a, &a_dup));
}

/// A home read with a trailing `/` shortens as the same home does without it. NEGATIVE
/// CONTROL: a sibling that shares its prefix, and a `/` or empty home, shorten nothing.
#[test]
fn a_home_with_a_trailing_slash_shortens_like_the_same_home_without() {
    for home in ["/Users//alice", "/Users//alice/"] {
        assert_eq!(shorten_home_with(" /Users//alice/aterm ", home), "~/aterm");
        assert_eq!(shorten_home_with("/Users//alice", home), "~");
        assert_eq!(
            shorten_home_with("/Users//alice2/aterm", home),
            "/Users//alice2/aterm"
        );
    }
    assert_eq!(shorten_home_with("/usr/bin", "/"), "/usr/bin");
    assert_eq!(shorten_home_with("/", "/"), "/");
    assert_eq!(shorten_home_with("/usr/bin", ""), "/usr/bin");
}
